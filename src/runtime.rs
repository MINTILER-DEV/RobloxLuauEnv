use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Error, Lua, Result};

use crate::instance::{Instance, InstanceRef, PropertyValue};
use crate::signal::{Signal, SignalArg};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    Server,
    Client,
}

#[derive(Clone)]
pub struct Runtime {
    state: Rc<RefCell<RuntimeState>>,
}

struct RuntimeState {
    next_id: u64,
    mode: RuntimeMode,
    data_model: InstanceRef,
    services: HashMap<String, InstanceRef>,
    local_player: Option<InstanceRef>,
}

const DEFAULT_SERVICES: &[(&str, &str)] = &[
    ("Workspace", "Workspace"),
    ("ReplicatedStorage", "ReplicatedStorage"),
    ("ServerStorage", "ServerStorage"),
    ("ServerScriptService", "ServerScriptService"),
    ("Lighting", "Lighting"),
    ("Players", "Players"),
    ("RunService", "RunService"),
    ("HttpService", "HttpService"),
    ("TweenService", "TweenService"),
];

impl Runtime {
    pub fn new(mode: RuntimeMode) -> Self {
        let data_model = Rc::new(RefCell::new(Instance::new(1, "DataModel", "game", false)));
        let runtime = Self {
            state: Rc::new(RefCell::new(RuntimeState {
                next_id: 2,
                mode,
                data_model: data_model.clone(),
                services: HashMap::new(),
                local_player: None,
            })),
        };

        for (service_name, class_name) in DEFAULT_SERVICES {
            if !runtime.is_service_visible(service_name) {
                continue;
            }
            let service = runtime.create_named_instance_internal(class_name, service_name, true);
            runtime.attach_initial(&service, &data_model);
            runtime
                .state
                .borrow_mut()
                .services
                .insert(service_name.to_ascii_lowercase(), service);
        }

        runtime.configure_mode_defaults();
        runtime
    }

    pub fn mode(&self) -> RuntimeMode {
        self.state.borrow().mode
    }

    pub fn data_model(&self) -> InstanceRef {
        self.state.borrow().data_model.clone()
    }

    pub fn local_player(&self) -> Option<InstanceRef> {
        self.state.borrow().local_player.clone()
    }

    pub fn get_service(&self, name: &str) -> Result<InstanceRef> {
        let key = name.to_ascii_lowercase();
        self.state
            .borrow()
            .services
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::RuntimeError(format!("Unknown service '{name}'")))
    }

    pub fn is_service_visible(&self, name: &str) -> bool {
        !(self.mode() == RuntimeMode::Client
            && matches!(name, "ServerStorage" | "ServerScriptService"))
    }

    pub fn create_instance(&self, class_name: &str) -> InstanceRef {
        let instance = self.create_named_instance_internal(class_name, class_name, false);
        if self.mode() == RuntimeMode::Client {
            instance.borrow_mut().client_authoritative = true;
        }
        instance
    }

    pub fn clone_instance_tree(&self, source: &InstanceRef) -> InstanceRef {
        let cloned_root = Instance::clone_shallow(source, self.allocate_id());
        if self.mode() == RuntimeMode::Client {
            self.set_client_authoritative_recursive(&cloned_root, true);
        }
        let children = source.borrow().children.clone();
        for child in children {
            let cloned_child = self.clone_instance_tree(&child);
            self.attach_initial(&cloned_child, &cloned_root);
        }
        cloned_root
    }

    pub fn set_network_owner(
        &self,
        instance: &InstanceRef,
        owner_name: Option<String>,
    ) -> Result<()> {
        if instance.borrow().class_name != "Part" {
            return Err(Error::RuntimeError(
                "SetNetworkOwner is only available on Part".to_string(),
            ));
        }
        instance.borrow_mut().network_owner_name = owner_name;
        Ok(())
    }

    pub fn get_network_owner_name(&self, instance: &InstanceRef) -> Option<String> {
        instance.borrow().network_owner_name.clone()
    }

    pub fn find_player_by_name(&self, name: &str) -> Option<InstanceRef> {
        let players = self.get_service("Players").ok()?;
        players
            .borrow()
            .children
            .iter()
            .find(|child| {
                let child_ref = child.borrow();
                child_ref.class_name == "Player" && child_ref.name == name
            })
            .cloned()
    }

    pub fn destroy_instance(&self, lua: &Lua, instance: &InstanceRef) -> Result<()> {
        if instance.borrow().destroyed {
            return Ok(());
        }

        if let Some(signal) = Instance::find_event(instance, "Destroying") {
            Signal::fire(&signal, lua, self, &[])?;
        }

        for descendant in Instance::all_descendants(instance) {
            if let Some(signal) = Instance::find_event(&descendant, "Destroying") {
                Signal::fire(&signal, lua, self, &[])?;
            }
        }

        self.set_parent(lua, instance, None)?;

        let all = std::iter::once(instance.clone())
            .chain(Instance::all_descendants(instance))
            .collect::<Vec<_>>();
        for current in all {
            let mut current_mut = current.borrow_mut();
            current_mut.children.clear();
            current_mut.parent = None;
            current_mut.destroyed = true;
        }

        Ok(())
    }

    pub fn set_parent(
        &self,
        lua: &Lua,
        child: &InstanceRef,
        new_parent: Option<InstanceRef>,
    ) -> Result<()> {
        Instance::assert_alive(child)?;
        if let Some(ref parent) = new_parent {
            Instance::assert_alive(parent)?;
        }

        if let Some(parent) = &new_parent {
            if Rc::ptr_eq(parent, child) {
                return Err(Error::RuntimeError(
                    "Cannot parent an instance to itself".to_string(),
                ));
            }
            for descendant in Instance::all_descendants(child) {
                if Rc::ptr_eq(parent, &descendant) {
                    return Err(Error::RuntimeError(
                        "Cannot parent an instance to one of its descendants".to_string(),
                    ));
                }
            }
        }

        let old_parent = Instance::get_parent(child);
        if matches!(
            (&old_parent, &new_parent),
            (Some(old_parent), Some(new_parent)) if Rc::ptr_eq(old_parent, new_parent)
        ) {
            return Ok(());
        }

        let subtree = std::iter::once(child.clone())
            .chain(Instance::all_descendants(child))
            .collect::<Vec<_>>();

        if let Some(parent) = &old_parent {
            parent
                .borrow_mut()
                .children
                .retain(|candidate| !Rc::ptr_eq(candidate, child));

            if let Some(signal) = Instance::find_event(parent, "ChildRemoved") {
                Signal::fire(&signal, lua, self, &[SignalArg::Instance(child.clone())])?;
            }
            self.fire_descendant_signal(lua, parent, "DescendantRemoving", child)?;
        }

        child.borrow_mut().parent = new_parent.as_ref().map(Rc::downgrade);

        if let Some(parent) = &new_parent {
            parent.borrow_mut().children.push(child.clone());

            if let Some(signal) = Instance::find_event(parent, "ChildAdded") {
                Signal::fire(&signal, lua, self, &[SignalArg::Instance(child.clone())])?;
            }
            self.fire_descendant_signal(lua, parent, "DescendantAdded", child)?;
        }

        self.fire_property_changed(lua, child, "Parent")?;

        for current in subtree {
            if let Some(signal) = Instance::find_event(&current, "AncestryChanged") {
                let parent_arg = Instance::get_parent(&current)
                    .map(SignalArg::Instance)
                    .unwrap_or(SignalArg::Nil);
                Signal::fire(
                    &signal,
                    lua,
                    self,
                    &[SignalArg::Instance(current.clone()), parent_arg],
                )?;
            }
        }

        Ok(())
    }

    pub fn set_property(
        &self,
        lua: &Lua,
        instance: &InstanceRef,
        property_name: &str,
        value: PropertyValue,
    ) -> Result<()> {
        Instance::assert_alive(instance)?;
        let value = self.apply_property_policy(instance, property_name, value);
        let changed = Instance::set_property(instance, property_name, value)?;
        if changed {
            self.fire_property_changed(lua, instance, property_name)?;
        }
        Ok(())
    }

    pub fn fire_property_changed(
        &self,
        lua: &Lua,
        instance: &InstanceRef,
        property_name: &str,
    ) -> Result<()> {
        if let Some(changed_signal) = Instance::find_event(instance, "Changed") {
            Signal::fire(
                &changed_signal,
                lua,
                self,
                &[SignalArg::String(property_name.to_string())],
            )?;
        }
        if instance
            .borrow()
            .property_signals
            .contains_key(property_name)
        {
            let signal = Instance::ensure_property_signal(instance, property_name);
            Signal::fire(&signal, lua, self, &[])?;
        }
        Ok(())
    }

    fn attach_initial(&self, child: &InstanceRef, parent: &InstanceRef) {
        child.borrow_mut().parent = Some(Rc::downgrade(parent));
        parent.borrow_mut().children.push(child.clone());
    }

    pub fn mark_replicated_instance(&self, instance: &InstanceRef) {
        self.set_client_authoritative_recursive(instance, false);
    }

    fn configure_mode_defaults(&self) {
        if self.mode() != RuntimeMode::Client {
            return;
        }

        let players = match self.get_service("Players") {
            Ok(service) => service,
            Err(_) => return,
        };

        let _ = Instance::set_property(&players, "CharacterAutoLoads", PropertyValue::Bool(false));

        let local_player = self.create_named_instance_internal("Player", "LocalPlayer", false);
        let _ = Instance::set_property(
            &local_player,
            "DisplayName",
            PropertyValue::String("LocalPlayer".to_string()),
        );
        self.attach_initial(&local_player, &players);
        self.state.borrow_mut().local_player = Some(local_player);
    }

    fn create_named_instance_internal(
        &self,
        class_name: &str,
        default_name: &str,
        is_service: bool,
    ) -> InstanceRef {
        Rc::new(RefCell::new(Instance::new(
            self.allocate_id(),
            class_name,
            default_name,
            is_service,
        )))
    }

    fn allocate_id(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        let id = state.next_id;
        state.next_id += 1;
        id
    }

    fn set_client_authoritative_recursive(&self, instance: &InstanceRef, value: bool) {
        let children = {
            let mut instance_mut = instance.borrow_mut();
            instance_mut.client_authoritative = value;
            instance_mut.children.clone()
        };
        for child in children {
            self.set_client_authoritative_recursive(&child, value);
        }
    }

    fn fire_descendant_signal(
        &self,
        lua: &Lua,
        origin: &InstanceRef,
        signal_name: &str,
        child: &InstanceRef,
    ) -> Result<()> {
        let descendants = std::iter::once(child.clone())
            .chain(Instance::all_descendants(child))
            .collect::<Vec<_>>();
        let mut cursor = Some(origin.clone());

        while let Some(current) = cursor {
            if let Some(signal) = Instance::find_event(&current, signal_name) {
                for descendant in &descendants {
                    Signal::fire(
                        &signal,
                        lua,
                        self,
                        &[SignalArg::Instance(descendant.clone())],
                    )?;
                }
            }
            cursor = Instance::get_parent(&current);
        }

        Ok(())
    }

    fn apply_property_policy(
        &self,
        instance: &InstanceRef,
        property_name: &str,
        value: PropertyValue,
    ) -> PropertyValue {
        let class_name = instance.borrow().class_name.clone();
        if class_name == "Players"
            && property_name == "CharacterAutoLoads"
            && self.mode() == RuntimeMode::Client
        {
            if value != PropertyValue::Bool(false) {
                eprintln!(
                    "[roblox-env warning] Players.CharacterAutoLoads is forced to false in emulate-client mode."
                );
            }
            return PropertyValue::Bool(false);
        }

        if class_name != "Part" {
            return value;
        }

        if self.mode() == RuntimeMode::Client && !instance.borrow().client_authoritative {
            let local_player_name = self
                .local_player()
                .map(|player| player.borrow().name.clone());
            let owned_by_client = matches!(
                (&instance.borrow().network_owner_name, local_player_name),
                (Some(owner), Some(local_name)) if *owner == local_name
            );
            if !owned_by_client {
                eprintln!(
                    "[roblox-env warning] Client changed {}.{property_name} locally, but the change would not replicate because LocalPlayer does not own that part.",
                    Instance::full_name(instance)
                );
            }
        }

        match (property_name, &value) {
            ("Anchored", PropertyValue::Bool(false)) => {
                eprintln!(
                    "[roblox-env warning] {} requested Anchored = false, but physics is disabled so it remains true.",
                    Instance::full_name(instance)
                );
                PropertyValue::Bool(true)
            }
            ("CanCollide", PropertyValue::Bool(true)) => {
                eprintln!(
                    "[roblox-env warning] {} enabled CanCollide = true, but collision simulation is not implemented.",
                    Instance::full_name(instance)
                );
                value
            }
            _ => value,
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(RuntimeMode::Server)
    }
}
