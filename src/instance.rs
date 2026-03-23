use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use mlua::{Error, Result};

use crate::math::{Color3, Vector3};
use crate::signal::{Signal, SignalRef};

pub type InstanceRef = Rc<RefCell<Instance>>;

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Number(f64),
    String(String),
    BinaryString(Vec<u8>),
    Vector3(Vector3),
    Color3(Color3),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PropertyKind {
    Bool,
    Number,
    String,
    Vector3,
    Color3,
}

#[derive(Debug)]
pub struct Instance {
    pub id: u64,
    pub class_name: String,
    pub name: String,
    pub parent: Option<Weak<RefCell<Instance>>>,
    pub children: Vec<InstanceRef>,
    pub properties: HashMap<String, PropertyValue>,
    pub events: HashMap<String, SignalRef>,
    pub property_signals: HashMap<String, SignalRef>,
    pub script_path: Option<String>,
    pub network_owner_name: Option<String>,
    pub client_authoritative: bool,
    pub is_service: bool,
    pub destroyed: bool,
}

impl Instance {
    pub fn new(id: u64, class_name: &str, name: &str, is_service: bool) -> Self {
        let mut events = HashMap::new();
        for event_name in default_events(class_name) {
            events.insert(event_name.to_string(), Signal::named(event_name));
        }

        Self {
            id,
            class_name: class_name.to_string(),
            name: name.to_string(),
            parent: None,
            children: Vec::new(),
            properties: default_properties(class_name),
            events,
            property_signals: HashMap::new(),
            script_path: None,
            network_owner_name: None,
            client_authoritative: false,
            is_service,
            destroyed: false,
        }
    }

    pub fn get_parent(instance: &InstanceRef) -> Option<InstanceRef> {
        instance.borrow().parent.as_ref().and_then(Weak::upgrade)
    }

    pub fn find_event(instance: &InstanceRef, name: &str) -> Option<SignalRef> {
        instance.borrow().events.get(name).cloned()
    }

    pub fn ensure_property_signal(instance: &InstanceRef, property_name: &str) -> SignalRef {
        if let Some(signal) = instance
            .borrow()
            .property_signals
            .get(property_name)
            .cloned()
        {
            return signal;
        }

        let mut instance_mut = instance.borrow_mut();
        instance_mut
            .property_signals
            .entry(property_name.to_string())
            .or_insert_with(|| Signal::named(format!("{}.Changed", property_name)))
            .clone()
    }

    pub fn get_property(instance: &InstanceRef, name: &str) -> Option<PropertyValue> {
        match name {
            "Name" => Some(PropertyValue::String(instance.borrow().name.clone())),
            "ClassName" => Some(PropertyValue::String(instance.borrow().class_name.clone())),
            _ => instance.borrow().properties.get(name).cloned(),
        }
    }

    pub fn set_property(instance: &InstanceRef, name: &str, value: PropertyValue) -> Result<bool> {
        match name {
            "Name" => {
                let PropertyValue::String(new_name) = value else {
                    return Err(Error::RuntimeError("Name must be a string".to_string()));
                };
                let mut instance_mut = instance.borrow_mut();
                let changed = instance_mut.name != new_name;
                if changed {
                    instance_mut.name = new_name;
                }
                Ok(changed)
            }
            "ClassName" => Err(Error::RuntimeError("ClassName is read-only".to_string())),
            _ => {
                let class_name = instance.borrow().class_name.clone();
                let Some(kind) = property_kind(&class_name, name) else {
                    return Err(Error::RuntimeError(format!(
                        "Unknown property '{name}' on {class_name}"
                    )));
                };
                validate_property_kind(kind, &value)?;

                let mut instance_mut = instance.borrow_mut();
                let changed = instance_mut.properties.get(name) != Some(&value);
                if changed {
                    instance_mut.properties.insert(name.to_string(), value);
                }
                Ok(changed)
            }
        }
    }

    pub fn is_a(instance: &InstanceRef, class_name: &str) -> bool {
        is_a_class(&instance.borrow().class_name, class_name)
    }

    pub fn assert_alive(instance: &InstanceRef) -> Result<()> {
        if instance.borrow().destroyed {
            return Err(Error::RuntimeError(format!(
                "{} has been destroyed",
                instance.borrow().name
            )));
        }
        Ok(())
    }

    pub fn clone_shallow(instance: &InstanceRef, new_id: u64) -> InstanceRef {
        let source = instance.borrow();
        let cloned = Instance {
            id: new_id,
            class_name: source.class_name.clone(),
            name: source.name.clone(),
            parent: None,
            children: Vec::new(),
            properties: source.properties.clone(),
            events: source
                .events
                .keys()
                .map(|name| (name.clone(), Signal::named(name)))
                .collect(),
            property_signals: HashMap::new(),
            script_path: source.script_path.clone(),
            network_owner_name: source.network_owner_name.clone(),
            client_authoritative: source.client_authoritative,
            is_service: false,
            destroyed: false,
        };
        Rc::new(RefCell::new(cloned))
    }

    pub fn all_descendants(instance: &InstanceRef) -> Vec<InstanceRef> {
        let children = instance.borrow().children.clone();
        let mut descendants = Vec::new();
        for child in children {
            descendants.push(child.clone());
            descendants.extend(Self::all_descendants(&child));
        }
        descendants
    }

    pub fn full_name(instance: &InstanceRef) -> String {
        let mut segments = Vec::new();
        let mut cursor = Some(instance.clone());
        while let Some(current) = cursor {
            let current_ref = current.borrow();
            segments.push(current_ref.name.clone());
            cursor = current_ref.parent.as_ref().and_then(Weak::upgrade);
        }
        segments.reverse();
        segments.join(".")
    }
}

pub fn validate_property_kind(expected: PropertyKind, value: &PropertyValue) -> Result<()> {
    let actual = match value {
        PropertyValue::Bool(_) => PropertyKind::Bool,
        PropertyValue::Number(_) => PropertyKind::Number,
        PropertyValue::String(_) | PropertyValue::BinaryString(_) => PropertyKind::String,
        PropertyValue::Vector3(_) => PropertyKind::Vector3,
        PropertyValue::Color3(_) => PropertyKind::Color3,
    };

    if expected == actual {
        Ok(())
    } else {
        Err(Error::RuntimeError(format!(
            "Expected property type {:?}, got {:?}",
            expected, actual
        )))
    }
}

pub fn property_kind(class_name: &str, property_name: &str) -> Option<PropertyKind> {
    match property_name {
        "Archivable" => Some(PropertyKind::Bool),
        "CharacterAutoLoads" if class_name == "Players" => Some(PropertyKind::Bool),
        "UserId" if class_name == "Player" => Some(PropertyKind::Number),
        "DisplayName" if class_name == "Player" => Some(PropertyKind::String),
        "HttpEnabled" if class_name == "HttpService" => Some(PropertyKind::Bool),
        "Source" if matches!(class_name, "Script" | "LocalScript" | "ModuleScript") => {
            Some(PropertyKind::String)
        }
        "Value" if class_name == "StringValue" => Some(PropertyKind::String),
        "StreamingEnabled" if class_name == "Workspace" => Some(PropertyKind::Bool),
        "Anchored" | "CanCollide" if class_name == "Part" => Some(PropertyKind::Bool),
        "Transparency" if class_name == "Part" => Some(PropertyKind::Number),
        "Material" if class_name == "Part" => Some(PropertyKind::String),
        "Position" | "Size" if class_name == "Part" => Some(PropertyKind::Vector3),
        "Color" if class_name == "Part" => Some(PropertyKind::Color3),
        _ => None,
    }
}

pub fn default_properties(class_name: &str) -> HashMap<String, PropertyValue> {
    let mut properties = HashMap::from([("Archivable".to_string(), PropertyValue::Bool(true))]);

    match class_name {
        "Workspace" => {
            properties.insert("StreamingEnabled".to_string(), PropertyValue::Bool(false));
        }
        "Players" => {
            properties.insert("CharacterAutoLoads".to_string(), PropertyValue::Bool(true));
        }
        "Player" => {
            properties.insert("UserId".to_string(), PropertyValue::Number(1.0));
            properties.insert(
                "DisplayName".to_string(),
                PropertyValue::String("Player".to_string()),
            );
        }
        "HttpService" => {
            properties.insert("HttpEnabled".to_string(), PropertyValue::Bool(true));
        }
        "Part" => {
            properties.insert("Anchored".to_string(), PropertyValue::Bool(true));
            properties.insert("CanCollide".to_string(), PropertyValue::Bool(false));
            properties.insert(
                "Position".to_string(),
                PropertyValue::Vector3(Vector3::zero()),
            );
            properties.insert(
                "Size".to_string(),
                PropertyValue::Vector3(Vector3::new(4.0, 1.0, 2.0)),
            );
            properties.insert("Color".to_string(), PropertyValue::Color3(Color3::gray()));
            properties.insert("Transparency".to_string(), PropertyValue::Number(0.0));
            properties.insert(
                "Material".to_string(),
                PropertyValue::String("Plastic".to_string()),
            );
        }
        "Script" | "LocalScript" | "ModuleScript" => {
            properties.insert("Source".to_string(), PropertyValue::String(String::new()));
        }
        "StringValue" => {
            properties.insert(
                "Value".to_string(),
                PropertyValue::BinaryString(Vec::new()),
            );
        }
        _ => {}
    }

    properties
}

pub fn default_events(class_name: &str) -> Vec<&'static str> {
    let mut events = vec![
        "AncestryChanged",
        "Changed",
        "ChildAdded",
        "ChildRemoved",
        "DescendantAdded",
        "DescendantRemoving",
        "Destroying",
    ];

    if class_name == "Part" {
        events.push("Touched");
        events.push("TouchEnded");
    }
    if class_name == "Players" {
        events.push("PlayerAdded");
        events.push("PlayerRemoving");
    }

    events
}

pub fn is_a_class(class_name: &str, query: &str) -> bool {
    if class_name == query {
        return true;
    }

    let ancestors: &[&str] = match class_name {
        "DataModel" => &["ServiceProvider", "Instance"],
        "Workspace" => &["WorldRoot", "Model", "PVInstance", "Instance"],
        "Model" => &["PVInstance", "Instance"],
        "Part" => &["BasePart", "PVInstance", "Instance"],
        "Folder" => &["Instance"],
        "StringValue" => &["ValueBase", "Instance"],
        "ReplicatedStorage"
        | "ServerStorage"
        | "ServerScriptService"
        | "Lighting"
        | "Players"
        | "RunService"
        | "HttpService"
        | "TweenService" => &["Service", "Instance"],
        "Player" => &["Instance"],
        "Script" | "LocalScript" | "ModuleScript" => &["LuaSourceContainer", "Instance"],
        _ => &["Instance"],
    };

    ancestors.contains(&query)
}
