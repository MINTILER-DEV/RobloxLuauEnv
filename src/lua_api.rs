use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use chrono::Local;
use mlua::{
    AnyUserData, Error, Function, Lua, MetaMethod, MultiValue, RegistryKey, Result, Table,
    UserData, UserDataMethods, Value,
};
use reqwest::header::CONTENT_TYPE;
use serde_json::Value as JsonValue;

use crate::image;
use crate::instance::{Instance, InstanceRef, PropertyValue};
use crate::math::{Color3, Vector3};
use crate::project::{LoadedProject, ProjectLayout, ProjectMount, ProjectNode, is_rleimg_path};
use crate::runtime::{Runtime, RuntimeMode};
use crate::signal::{ConnectionHandle, Signal, SignalRef, signal_arg_to_lua};

pub struct RobloxEnvironment {
    lua: Lua,
    runtime: Runtime,
    module_cache: Rc<RefCell<HashMap<u64, RegistryKey>>>,
}

impl RobloxEnvironment {
    pub fn new(mode: RuntimeMode) -> Result<Self> {
        let lua = Lua::new();
        let runtime = Runtime::new(mode);
        let environment = Self {
            lua,
            runtime,
            module_cache: Rc::new(RefCell::new(HashMap::new())),
        };
        environment.install_globals()?;
        Ok(environment)
    }

    pub fn run_script(&self, name: &str, source: &str) -> Result<()> {
        self.lua
            .load(source)
            .set_name(name)
            .exec()
            .map_err(|error| Error::RuntimeError(format!("Failed while running '{name}': {error}")))
    }

    pub fn run_file(&self, path: &Path) -> Result<()> {
        let source = std::fs::read_to_string(path).map_err(|error| {
            Error::RuntimeError(format!("Could not read script {}: {error}", path.display()))
        })?;
        self.run_script(&path.display().to_string(), &source)
    }

    pub fn run_project_path(&self, path: &Path) -> Result<()> {
        let project = if is_rleimg_path(path) {
            image::read_project_image(path)?
        } else {
            LoadedProject::from_path(path)?
        };
        self.run_project(project)
    }

    pub fn run_project(&self, project: LoadedProject) -> Result<()> {
        let layout = project.layout()?;
        self.instantiate_layout(&layout)?;
        self.bootstrap_client_player_scripts()?;

        for script in self.collect_auto_run_scripts() {
            self.execute_script_instance(&script)?;
        }

        Ok(())
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    fn install_globals(&self) -> Result<()> {
        let globals = self.lua.globals();
        globals.set(
            "game",
            self.lua.create_userdata(LuaInstance::new(
                self.runtime.clone(),
                self.runtime.data_model(),
            ))?,
        )?;
        globals.set(
            "workspace",
            self.lua.create_userdata(LuaInstance::new(
                self.runtime.clone(),
                self.runtime.get_service("Workspace")?,
            ))?,
        )?;

        let instance_table = self.lua.create_table()?;
        let runtime = self.runtime.clone();
        instance_table.set(
            "new",
            self.lua.create_function(
                move |lua, (class_name, parent): (String, Option<AnyUserData>)| {
                    let instance = runtime.create_instance(&class_name);
                    let userdata =
                        lua.create_userdata(LuaInstance::new(runtime.clone(), instance.clone()))?;
                    if let Some(parent_ud) = parent {
                        let parent = parent_ud.borrow::<LuaInstance>()?.instance.clone();
                        runtime.set_parent(lua, &instance, Some(parent))?;
                    }
                    Ok(userdata)
                },
            )?,
        )?;
        globals.set("Instance", instance_table)?;

        let vector3_table = self.lua.create_table()?;
        vector3_table.set(
            "new",
            self.lua
                .create_function(|lua, (x, y, z): (f64, f64, f64)| {
                    lua.create_userdata(Vector3::new(x, y, z))
                })?,
        )?;
        globals.set("Vector3", vector3_table)?;

        let color3_table = self.lua.create_table()?;
        color3_table.set(
            "new",
            self.lua
                .create_function(|lua, (r, g, b): (f64, f64, f64)| {
                    lua.create_userdata(Color3::new(r, g, b))
                })?,
        )?;
        globals.set("Color3", color3_table)?;

        globals.set(
            "print",
            self.lua.create_function(|lua, values: MultiValue| {
                println!(
                    "{}",
                    format_console_output_line(lua, &values_to_console_string(&values), false)
                );
                Ok(())
            })?,
        )?;
        globals.set(
            "warn",
            self.lua.create_function(|lua, values: MultiValue| {
                eprintln!(
                    "{}",
                    format_console_output_line(lua, &values_to_console_string(&values), true)
                );
                Ok(())
            })?,
        )?;

        globals.set("task", self.create_task_table()?)?;
        globals.set("require", self.create_require_function()?)?;

        Ok(())
    }

    fn create_task_table(&self) -> Result<Table> {
        let table = self.lua.create_table()?;

        table.set(
            "wait",
            self.lua.create_function(|_, seconds: Option<f64>| {
                let seconds = seconds.unwrap_or(0.03).max(0.0);
                thread::sleep(Duration::from_secs_f64(seconds));
                Ok(seconds)
            })?,
        )?;

        table.set(
            "spawn",
            self.lua
                .create_function(|_, (callback, args): (Function, MultiValue)| {
                    let _ = callback.call::<()>(args);
                    Ok(())
                })?,
        )?;

        table.set(
            "defer",
            self.lua
                .create_function(|_, (callback, args): (Function, MultiValue)| {
                    let _ = callback.call::<()>(args);
                    Ok(())
                })?,
        )?;

        table.set(
            "delay",
            self.lua.create_function(
                |_, (seconds, callback, args): (f64, Function, MultiValue)| {
                    thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
                    let _ = callback.call::<()>(args);
                    Ok(())
                },
            )?,
        )?;

        Ok(table)
    }

    fn create_require_function(&self) -> Result<Function> {
        let runtime = self.runtime.clone();
        let module_cache = self.module_cache.clone();
        self.lua.create_function(move |lua, target: Value| {
            let instance = match target {
                Value::UserData(userdata) if userdata.is::<LuaInstance>() => {
                    userdata.borrow::<LuaInstance>()?.instance.clone()
                }
                _ => {
                    return Err(Error::RuntimeError(
                        "require currently expects a ModuleScript instance".to_string(),
                    ));
                }
            };

            if instance.borrow().class_name != "ModuleScript" {
                return Err(Error::RuntimeError(
                    "require currently expects a ModuleScript instance".to_string(),
                ));
            }

            let instance_id = instance.borrow().id;
            if let Some(cached) = module_cache.borrow().get(&instance_id) {
                return lua.registry_value(cached);
            }

            let source = match Instance::get_property(&instance, "Source") {
                Some(PropertyValue::String(value)) => value,
                _ => String::new(),
            };
            let env = create_script_environment(lua, &runtime, &instance)?;
            let value = lua
                .load(&source)
                .set_name(script_chunk_name(&instance))
                .set_environment(env)
                .eval::<Value>()?;
            let key = lua.create_registry_value(value.clone())?;
            module_cache.borrow_mut().insert(instance_id, key);
            Ok(value)
        })
    }

    fn instantiate_layout(&self, layout: &ProjectLayout) -> Result<()> {
        for mount in &layout.top_level {
            match mount {
                ProjectMount::DataModelChild(node) => {
                    let parent = self.runtime.data_model();
                    self.instantiate_project_node(node, &parent)?;
                }
                ProjectMount::ServiceContents {
                    service_name,
                    children,
                } => {
                    if !self.runtime.is_service_visible(service_name) {
                        continue;
                    }
                    let parent = self.runtime.get_service(service_name)?;
                    for child in children {
                        self.instantiate_project_node(child, &parent)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn instantiate_project_node(
        &self,
        node: &ProjectNode,
        parent: &InstanceRef,
    ) -> Result<InstanceRef> {
        let instance =
            if let Some(existing) = find_matching_child(parent, &node.name, &node.class_name) {
                existing
            } else {
                let instance = self.runtime.create_instance(&node.class_name);
                self.runtime.set_property(
                    &self.lua,
                    &instance,
                    "Name",
                    PropertyValue::String(node.name.clone()),
                )?;
                self.runtime
                    .set_parent(&self.lua, &instance, Some(parent.clone()))?;
                instance
            };

        if let Some(source) = &node.source {
            self.runtime.set_property(
                &self.lua,
                &instance,
                "Source",
                PropertyValue::String(source.clone()),
            )?;
        }
        if let Some(run_context) = &node.run_context {
            self.runtime.set_property(
                &self.lua,
                &instance,
                "RunContext",
                PropertyValue::String(run_context.clone()),
            )?;
        }
        if let Some(value) = &node.value {
            self.runtime.set_property(
                &self.lua,
                &instance,
                "Value",
                PropertyValue::BinaryString(value.clone()),
            )?;
        }
        {
            let mut instance_mut = instance.borrow_mut();
            instance_mut.script_path = node
                .script_path
                .as_ref()
                .map(|path| normalize_script_path(path));
            instance_mut.auto_run_enabled = node.auto_run;
        }
        self.runtime.mark_replicated_instance(&instance);
        self.runtime.ensure_builtin_children(&instance);

        for child in &node.children {
            self.instantiate_project_node(child, &instance)?;
        }

        Ok(instance)
    }

    fn bootstrap_client_player_scripts(&self) -> Result<()> {
        if self.runtime.mode() != RuntimeMode::Client {
            return Ok(());
        }

        let starter_player = self.runtime.get_service("StarterPlayer")?;
        let starter_player_scripts = self.runtime.ensure_named_child(
            &starter_player,
            "StarterPlayerScripts",
            "StarterPlayerScripts",
        );
        let Some(local_player) = self.runtime.local_player() else {
            return Ok(());
        };
        let player_scripts =
            self.runtime
                .ensure_named_child(&local_player, "PlayerScripts", "PlayerScripts");

        let existing_children = player_scripts.borrow().children.clone();
        for child in existing_children {
            self.runtime.destroy_instance(&self.lua, &child)?;
        }

        let starter_children = starter_player_scripts.borrow().children.clone();
        for child in starter_children {
            let cloned = self.runtime.clone_instance_tree(&child);
            self.runtime
                .set_parent(&self.lua, &cloned, Some(player_scripts.clone()))?;
            self.runtime.mark_replicated_instance(&cloned);
        }

        Ok(())
    }

    fn collect_auto_run_scripts(&self) -> Vec<InstanceRef> {
        let mut scripts = Vec::new();
        collect_auto_run_scripts_from(&self.runtime.data_model(), &mut scripts);
        scripts
            .into_iter()
            .filter(|instance| self.should_auto_run_instance(instance))
            .collect()
    }

    fn should_auto_run_instance(&self, instance: &InstanceRef) -> bool {
        if !instance.borrow().auto_run_enabled {
            return false;
        }

        let class_name = instance.borrow().class_name.clone();
        match (self.runtime.mode(), class_name.as_str()) {
            (RuntimeMode::Server, "Script") => match script_run_context(instance).as_deref() {
                Some("Server") => true,
                Some("Legacy") => {
                    is_descendant_of_service(instance, "ServerScriptService")
                        || is_descendant_of_service(instance, "Workspace")
                }
                _ => false,
            },
            (RuntimeMode::Client, "Script") => {
                if script_run_context(instance).as_deref() != Some("Client") {
                    return false;
                }
                is_descendant_of_service(instance, "Workspace")
                    || is_descendant_of_service(instance, "ReplicatedStorage")
                    || is_descendant_of_service(instance, "ReplicatedFirst")
                    || is_descendant_of_named_container(instance, "PlayerScripts")
            }
            (RuntimeMode::Client, "LocalScript") => {
                is_descendant_of_service(instance, "ReplicatedFirst")
                    || is_descendant_of_named_container(instance, "PlayerScripts")
            }
            _ => false,
        }
    }

    fn execute_script_instance(&self, instance: &InstanceRef) -> Result<()> {
        let source = match Instance::get_property(instance, "Source") {
            Some(PropertyValue::String(value)) => value,
            _ => String::new(),
        };
        let env = create_script_environment(&self.lua, &self.runtime, instance)?;
        self.lua
            .load(&source)
            .set_name(script_chunk_name(instance))
            .set_environment(env)
            .exec()
    }
}

fn find_matching_child(parent: &InstanceRef, name: &str, class_name: &str) -> Option<InstanceRef> {
    parent
        .borrow()
        .children
        .iter()
        .find(|child| {
            let child_ref = child.borrow();
            child_ref.name == name && child_ref.class_name == class_name
        })
        .cloned()
}

fn collect_auto_run_scripts_from(root: &InstanceRef, scripts: &mut Vec<InstanceRef>) {
    let children = root.borrow().children.clone();
    for child in children {
        let class_name = child.borrow().class_name.clone();
        if matches!(class_name.as_str(), "Script" | "LocalScript") {
            scripts.push(child.clone());
        }
        collect_auto_run_scripts_from(&child, scripts);
    }
}

fn script_run_context(instance: &InstanceRef) -> Option<String> {
    match Instance::get_property(instance, "RunContext") {
        Some(PropertyValue::String(value)) => Some(value),
        _ => None,
    }
}

fn is_descendant_of_service(instance: &InstanceRef, service_name: &str) -> bool {
    top_level_service_name(instance).as_deref() == Some(service_name)
}

fn is_descendant_of_named_container(instance: &InstanceRef, ancestor_name: &str) -> bool {
    let mut cursor = Some(instance.clone());
    while let Some(current) = cursor {
        let current_ref = current.borrow();
        if current_ref.name == ancestor_name || current_ref.class_name == ancestor_name {
            return true;
        }
        cursor = current_ref
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade());
    }
    false
}

fn top_level_service_name(instance: &InstanceRef) -> Option<String> {
    let mut cursor = Some(instance.clone());
    let mut candidate = None;
    while let Some(current) = cursor {
        let current_ref = current.borrow();
        if current_ref.parent.is_none() {
            return candidate;
        }
        candidate = Some(current_ref.name.clone());
        cursor = current_ref
            .parent
            .as_ref()
            .and_then(|parent| parent.upgrade());
    }
    None
}

fn create_script_environment(lua: &Lua, runtime: &Runtime, script: &InstanceRef) -> Result<Table> {
    let globals = lua.globals();
    let env = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", globals)?;
    env.set_metatable(Some(meta))?;
    env.set(
        "script",
        lua.create_userdata(LuaInstance::new(runtime.clone(), script.clone()))?,
    )?;
    Ok(env)
}

#[derive(Clone)]
pub struct LuaInstance {
    runtime: Runtime,
    pub instance: InstanceRef,
}

impl LuaInstance {
    pub fn new(runtime: Runtime, instance: InstanceRef) -> Self {
        Self { runtime, instance }
    }
}

impl UserData for LuaInstance {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: String| {
            Instance::assert_alive(&this.instance)?;

            if let Some(value) = lookup_instance_member(lua, this, &key)? {
                return Ok(value);
            }

            if let Some(child) = this
                .instance
                .borrow()
                .children
                .iter()
                .find(|child| child.borrow().name == key)
                .cloned()
            {
                return Ok(Value::UserData(
                    lua.create_userdata(LuaInstance::new(this.runtime.clone(), child))?,
                ));
            }

            Ok(Value::Nil)
        });

        methods.add_meta_method(
            MetaMethod::NewIndex,
            |lua, this, (key, value): (String, Value)| {
                Instance::assert_alive(&this.instance)?;

                match key.as_str() {
                    "Parent" => {
                        let parent = value_to_instance(value)?;
                        this.runtime.set_parent(lua, &this.instance, parent)?;
                    }
                    "OnServerInvoke" | "OnClientInvoke"
                        if this.instance.borrow().class_name == "RemoteFunction" =>
                    {
                        set_remote_function_callback(lua, this.instance.borrow().id, &key, value)?;
                    }
                    _ => {
                        let property =
                            lua_value_to_property_for_instance(&this.instance, &key, &value)?;
                        this.runtime
                            .set_property(lua, &this.instance, &key, property)?;
                    }
                }

                Ok(())
            },
        );

        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "{} \"{}\"",
                this.instance.borrow().class_name,
                this.instance.borrow().name
            ))
        });

        methods.add_meta_method(MetaMethod::Eq, |_, this, other: AnyUserData| {
            if !other.is::<LuaInstance>() {
                return Ok(false);
            }
            let other_instance = other.borrow::<LuaInstance>()?.instance.clone();
            Ok(Rc::ptr_eq(&this.instance, &other_instance))
        });
    }
}

fn lookup_instance_member(lua: &Lua, this: &LuaInstance, key: &str) -> Result<Option<Value>> {
    match key {
        "Name" => Ok(Some(Value::String(
            lua.create_string(&this.instance.borrow().name)?,
        ))),
        "ClassName" => Ok(Some(Value::String(
            lua.create_string(&this.instance.borrow().class_name)?,
        ))),
        "Parent" => Ok(match Instance::get_parent(&this.instance) {
            Some(parent) => Some(Value::UserData(
                lua.create_userdata(LuaInstance::new(this.runtime.clone(), parent))?,
            )),
            None => Some(Value::Nil),
        }),
        "LocalPlayer" if this.instance.borrow().class_name == "Players" => {
            let value = if let Some(player) = this.runtime.local_player() {
                Value::UserData(
                    lua.create_userdata(LuaInstance::new(this.runtime.clone(), player))?,
                )
            } else {
                Value::Nil
            };
            Ok(Some(value))
        }
        "ChildAdded" | "ChildRemoved" | "Changed" | "Destroying" | "AncestryChanged"
        | "DescendantAdded" | "DescendantRemoving" | "Touched" | "TouchEnded" | "PlayerAdded"
        | "PlayerRemoving" => {
            if let Some(signal) = Instance::find_event(&this.instance, key) {
                return Ok(Some(Value::UserData(
                    lua.create_userdata(LuaSignal {
                        signal,
                        runtime: this.runtime.clone(),
                    })?,
                )));
            }
            Ok(Some(Value::Nil))
        }
        "GetChildren" => Ok(Some(Value::Function(make_get_children(lua, this.clone())?))),
        "GetDescendants" => Ok(Some(Value::Function(make_get_descendants(
            lua,
            this.clone(),
        )?))),
        "FindFirstChild" => Ok(Some(Value::Function(make_find_first_child(
            lua,
            this.clone(),
        )?))),
        "WaitForChild" => Ok(Some(Value::Function(make_wait_for_child(
            lua,
            this.clone(),
        )?))),
        "IsA" => Ok(Some(Value::Function(make_is_a(lua, this.clone())?))),
        "GetFullName" => Ok(Some(Value::Function(make_get_full_name(
            lua,
            this.clone(),
        )?))),
        "GetService" => Ok(Some(Value::Function(make_get_service(lua, this.clone())?))),
        "GetPropertyChangedSignal" => Ok(Some(Value::Function(make_get_property_changed_signal(
            lua,
            this.clone(),
        )?))),
        "Destroy" => Ok(Some(Value::Function(make_destroy(lua, this.clone())?))),
        "Clone" => Ok(Some(Value::Function(make_clone(lua, this.clone())?))),
        "ClearAllChildren" => Ok(Some(Value::Function(make_clear_all_children(
            lua,
            this.clone(),
        )?))),
        "GetPlayers" if this.instance.borrow().class_name == "Players" => {
            Ok(Some(Value::Function(make_get_players(lua, this.clone())?)))
        }
        "LoadCharacter" if this.instance.borrow().class_name == "Player" => Ok(Some(
            Value::Function(make_load_character(lua, this.clone())?),
        )),
        "SetNetworkOwner" if this.instance.borrow().class_name == "Part" => Ok(Some(
            Value::Function(make_set_network_owner(lua, this.clone())?),
        )),
        "GetNetworkOwner" if this.instance.borrow().class_name == "Part" => Ok(Some(
            Value::Function(make_get_network_owner(lua, this.clone())?),
        )),
        "IsClient" if this.instance.borrow().class_name == "RunService" => {
            Ok(Some(Value::Function(make_is_client(lua, this.clone())?)))
        }
        "IsServer" if this.instance.borrow().class_name == "RunService" => {
            Ok(Some(Value::Function(make_is_server(lua, this.clone())?)))
        }
        "GetAsync" if this.instance.borrow().class_name == "HttpService" => Ok(Some(
            Value::Function(make_http_get_async(lua, this.clone())?),
        )),
        "PostAsync" if this.instance.borrow().class_name == "HttpService" => Ok(Some(
            Value::Function(make_http_post_async(lua, this.clone())?),
        )),
        "JSONEncode" if this.instance.borrow().class_name == "HttpService" => {
            Ok(Some(Value::Function(make_http_json_encode(lua)?)))
        }
        "JSONDecode" if this.instance.borrow().class_name == "HttpService" => {
            Ok(Some(Value::Function(make_http_json_decode(lua)?)))
        }
        "InvokeServer" if this.instance.borrow().class_name == "RemoteFunction" => Ok(Some(
            Value::Function(make_remote_function_invoke_server(lua, this.clone())?),
        )),
        "InvokeClient" if this.instance.borrow().class_name == "RemoteFunction" => Ok(Some(
            Value::Function(make_remote_function_invoke_client(lua, this.clone())?),
        )),
        "OnServerInvoke" if this.instance.borrow().class_name == "RemoteFunction" => Ok(Some(
            get_remote_function_callback(lua, this.instance.borrow().id, "OnServerInvoke")?
                .unwrap_or(Value::Nil),
        )),
        "OnClientInvoke" if this.instance.borrow().class_name == "RemoteFunction" => Ok(Some(
            get_remote_function_callback(lua, this.instance.borrow().id, "OnClientInvoke")?
                .unwrap_or(Value::Nil),
        )),
        _ => match Instance::get_property(&this.instance, key) {
            Some(value) => Ok(Some(property_to_lua(lua, &value)?)),
            None => Ok(None),
        },
    }
}

fn make_get_children(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let table = lua.create_table()?;
        for (index, child) in instance
            .instance
            .borrow()
            .children
            .iter()
            .cloned()
            .enumerate()
        {
            table.set(
                index + 1,
                lua.create_userdata(LuaInstance::new(instance.runtime.clone(), child))?,
            )?;
        }
        Ok(table)
    })
}

fn make_get_descendants(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let table = lua.create_table()?;
        for (index, child) in Instance::all_descendants(&instance.instance)
            .into_iter()
            .enumerate()
        {
            table.set(
                index + 1,
                lua.create_userdata(LuaInstance::new(instance.runtime.clone(), child))?,
            )?;
        }
        Ok(table)
    })
}

fn make_find_first_child(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let mut args = args.into_iter();
        let _self = args.next();
        let Some(Value::String(name)) = args.next() else {
            return Err(Error::RuntimeError(
                "FindFirstChild expects a name".to_string(),
            ));
        };
        let recursive = match args.next() {
            Some(Value::Boolean(value)) => value,
            _ => false,
        };
        let name = name.to_str()?.to_string();

        let found = if recursive {
            Instance::all_descendants(&instance.instance)
                .into_iter()
                .find(|child| child.borrow().name == name)
        } else {
            instance
                .instance
                .borrow()
                .children
                .iter()
                .find(|child| child.borrow().name == name)
                .cloned()
        };

        Ok(match found {
            Some(child) => Value::UserData(
                lua.create_userdata(LuaInstance::new(instance.runtime.clone(), child))?,
            ),
            None => Value::Nil,
        })
    })
}

fn make_wait_for_child(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let mut args = args.into_iter();
        let _self = args.next();
        let Some(Value::String(name)) = args.next() else {
            return Err(Error::RuntimeError(
                "WaitForChild expects a child name".to_string(),
            ));
        };
        let timeout = match args.next() {
            Some(Value::Integer(value)) => Some(value as f64),
            Some(Value::Number(value)) => Some(value),
            _ => None,
        };
        let name = name.to_str()?.to_string();

        let start = std::time::Instant::now();
        loop {
            if let Some(child) = instance
                .instance
                .borrow()
                .children
                .iter()
                .find(|child| child.borrow().name == name)
                .cloned()
            {
                return Ok(Value::UserData(
                    lua.create_userdata(LuaInstance::new(instance.runtime.clone(), child))?,
                ));
            }

            if let Some(limit) = timeout {
                if start.elapsed().as_secs_f64() >= limit.max(0.0) {
                    return Ok(Value::Nil);
                }
            }

            thread::sleep(Duration::from_millis(10));
        }
    })
}

fn make_is_a(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, (_self, class_name): (Value, String)| {
        Ok(Instance::is_a(&instance.instance, &class_name))
    })
}

fn make_get_full_name(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        Ok(Value::String(
            lua.create_string(Instance::full_name(&instance.instance))?,
        ))
    })
}

fn make_get_service(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, (_self, service_name): (Value, String)| {
        if instance.instance.borrow().class_name != "DataModel" {
            return Err(Error::RuntimeError(
                "GetService is only available on the DataModel in this environment".to_string(),
            ));
        }

        let service = instance.runtime.get_service(&service_name)?;
        Ok(Value::UserData(lua.create_userdata(LuaInstance::new(
            instance.runtime.clone(),
            service,
        ))?))
    })
}

fn make_get_property_changed_signal(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, (_self, property_name): (Value, String)| {
        let signal = Instance::ensure_property_signal(&instance.instance, &property_name);
        Ok(Value::UserData(lua.create_userdata(LuaSignal {
            signal,
            runtime: instance.runtime.clone(),
        })?))
    })
}

fn make_destroy(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        instance.runtime.destroy_instance(lua, &instance.instance)?;
        Ok(())
    })
}

fn make_clone(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let cloned = instance.runtime.clone_instance_tree(&instance.instance);
        Ok(Value::UserData(lua.create_userdata(LuaInstance::new(
            instance.runtime.clone(),
            cloned,
        ))?))
    })
}

fn make_clear_all_children(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let children = instance.instance.borrow().children.clone();
        for child in children {
            instance.runtime.destroy_instance(lua, &child)?;
        }
        Ok(())
    })
}

fn make_get_players(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let table = lua.create_table()?;
        let players = instance
            .instance
            .borrow()
            .children
            .iter()
            .filter(|child| child.borrow().class_name == "Player")
            .cloned()
            .collect::<Vec<_>>();
        for (index, player) in players.into_iter().enumerate() {
            table.set(
                index + 1,
                lua.create_userdata(LuaInstance::new(instance.runtime.clone(), player))?,
            )?;
        }
        Ok(table)
    })
}

fn make_load_character(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, _self: Value| {
        if instance.runtime.mode() == RuntimeMode::Client {
            return Err::<(), Error>(Error::RuntimeError(
                "LoadCharacter is disabled in emulate-client mode".to_string(),
            ));
        }
        Err::<(), Error>(Error::RuntimeError(
            "LoadCharacter is not implemented yet".to_string(),
        ))
    })
}

fn make_set_network_owner(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, (_self, owner): (Value, Value)| {
        if instance.runtime.mode() != RuntimeMode::Server {
            return Err(Error::RuntimeError(
                "SetNetworkOwner can only be called by the server".to_string(),
            ));
        }

        let owner_name = match owner {
            Value::Nil => None,
            Value::UserData(userdata) if userdata.is::<LuaInstance>() => {
                let player = userdata.borrow::<LuaInstance>()?.instance.clone();
                if player.borrow().class_name != "Player" {
                    return Err(Error::RuntimeError(
                        "SetNetworkOwner expects a Player or nil".to_string(),
                    ));
                }
                Some(player.borrow().name.clone())
            }
            _ => {
                return Err(Error::RuntimeError(
                    "SetNetworkOwner expects a Player or nil".to_string(),
                ));
            }
        };

        instance
            .runtime
            .set_network_owner(&instance.instance, owner_name)?;
        Ok(())
    })
}

fn make_get_network_owner(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let Some(owner_name) = instance.runtime.get_network_owner_name(&instance.instance) else {
            return Ok(Value::Nil);
        };

        if let Some(player) = instance.runtime.find_player_by_name(&owner_name) {
            return Ok(Value::UserData(lua.create_userdata(LuaInstance::new(
                instance.runtime.clone(),
                player,
            ))?));
        }

        Ok(Value::Nil)
    })
}

fn make_is_client(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, _self: Value| Ok(instance.runtime.mode() == RuntimeMode::Client))
}

fn make_is_server(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, _self: Value| Ok(instance.runtime.mode() == RuntimeMode::Server))
}

fn make_http_get_async(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |_, (_self, url): (Value, String)| {
        assert_http_enabled(&instance.instance)?;
        let response = reqwest::blocking::get(url.as_str())
            .and_then(|response| response.error_for_status())
            .map_err(|error| {
                Error::RuntimeError(format!("HttpService:GetAsync failed: {error}"))
            })?;
        response
            .text()
            .map_err(|error| Error::RuntimeError(format!("HttpService:GetAsync failed: {error}")))
    })
}

fn make_http_post_async(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(
        move |_, (_self, url, body, content_type): (Value, String, String, Option<String>)| {
            assert_http_enabled(&instance.instance)?;
            let client = reqwest::blocking::Client::new();
            let mut request = client.post(url).body(body);
            if let Some(content_type) = content_type {
                request = request.header(CONTENT_TYPE, content_type);
            }
            let response = request
                .send()
                .and_then(|response| response.error_for_status())
                .map_err(|error| {
                    Error::RuntimeError(format!("HttpService:PostAsync failed: {error}"))
                })?;
            response.text().map_err(|error| {
                Error::RuntimeError(format!("HttpService:PostAsync failed: {error}"))
            })
        },
    )
}

fn make_http_json_encode(lua: &Lua) -> Result<Function> {
    lua.create_function(|_, (_self, value): (Value, Value)| {
        let json = lua_value_to_json(&value)?;
        serde_json::to_string(&json)
            .map_err(|error| Error::RuntimeError(format!("HttpService:JSONEncode failed: {error}")))
    })
}

fn make_http_json_decode(lua: &Lua) -> Result<Function> {
    lua.create_function(|lua, (_self, value): (Value, String)| {
        let json: JsonValue = serde_json::from_str(&value).map_err(|error| {
            Error::RuntimeError(format!("HttpService:JSONDecode failed: {error}"))
        })?;
        json_to_lua(lua, &json)
    })
}

fn remote_function_registry_name(property_name: &str) -> &'static str {
    match property_name {
        "OnServerInvoke" => "__roblox_env_remote_function_on_server_invoke",
        "OnClientInvoke" => "__roblox_env_remote_function_on_client_invoke",
        _ => "__roblox_env_remote_function_unknown",
    }
}

fn ensure_remote_function_registry(lua: &Lua, property_name: &str) -> Result<Table> {
    let registry_name = remote_function_registry_name(property_name);
    if let Ok(table) = lua.named_registry_value::<Table>(registry_name) {
        return Ok(table);
    }

    let table = lua.create_table()?;
    lua.set_named_registry_value(registry_name, table.clone())?;
    Ok(table)
}

fn set_remote_function_callback(
    lua: &Lua,
    instance_id: u64,
    property_name: &str,
    value: Value,
) -> Result<()> {
    let registry = ensure_remote_function_registry(lua, property_name)?;
    match value {
        Value::Nil => registry.set(instance_id, Value::Nil)?,
        Value::Function(function) => registry.set(instance_id, function)?,
        _ => {
            return Err(Error::RuntimeError(format!(
                "{property_name} must be a function or nil"
            )));
        }
    }
    Ok(())
}

fn get_remote_function_callback(
    lua: &Lua,
    instance_id: u64,
    property_name: &str,
) -> Result<Option<Value>> {
    let registry = ensure_remote_function_registry(lua, property_name)?;
    let value: Value = registry.get(instance_id)?;
    Ok(match value {
        Value::Nil => None,
        other => Some(other),
    })
}

fn make_remote_function_invoke_server(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let mut args = args.into_iter();
        let _self = args.next();
        let callback = get_remote_function_callback(
            lua,
            instance.instance.borrow().id,
            "OnServerInvoke",
        )?;

        let Some(Value::Function(function)) = callback else {
            return Err(Error::RuntimeError(
                "RemoteFunction.OnServerInvoke is not set".to_string(),
            ));
        };

        let mut callback_args = Vec::new();
        callback_args.push(Value::Nil);
        callback_args.extend(args);
        function.call::<MultiValue>(MultiValue::from_vec(callback_args))
    })
}

fn make_remote_function_invoke_client(lua: &Lua, instance: LuaInstance) -> Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let mut args = args.into_iter();
        let _self = args.next();
        let _player = args.next();
        let callback = get_remote_function_callback(
            lua,
            instance.instance.borrow().id,
            "OnClientInvoke",
        )?;

        let Some(Value::Function(function)) = callback else {
            return Err(Error::RuntimeError(
                "RemoteFunction.OnClientInvoke is not set".to_string(),
            ));
        };

        function.call::<MultiValue>(MultiValue::from_vec(args.collect()))
    })
}

fn assert_http_enabled(instance: &InstanceRef) -> Result<()> {
    match Instance::get_property(instance, "HttpEnabled") {
        Some(PropertyValue::Bool(true)) => Ok(()),
        Some(PropertyValue::Bool(false)) => {
            Err(Error::RuntimeError("HttpService is disabled".to_string()))
        }
        _ => Ok(()),
    }
}

#[derive(Clone)]
struct LuaSignal {
    signal: SignalRef,
    runtime: Runtime,
}

impl UserData for LuaSignal {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: String| {
            match key.as_str() {
                "Connect" => Ok(Value::Function(make_signal_connect(
                    lua,
                    this.signal.clone(),
                    false,
                )?)),
                "Once" => Ok(Value::Function(make_signal_connect(
                    lua,
                    this.signal.clone(),
                    true,
                )?)),
                "Wait" => Ok(Value::Function(make_signal_wait(
                    lua,
                    this.signal.clone(),
                    this.runtime.clone(),
                )?)),
                _ => Ok(Value::Nil),
            }
        });
    }
}

fn make_signal_connect(lua: &Lua, signal: SignalRef, once: bool) -> Result<Function> {
    lua.create_function(move |lua, (_self, callback): (Value, Function)| {
        let connection = Signal::connect(&signal, lua, callback, once)?;
        Ok(lua.create_userdata(LuaConnection { connection })?)
    })
}

fn make_signal_wait(lua: &Lua, signal: SignalRef, runtime: Runtime) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        let generation = Signal::generation(&signal);
        let args = Signal::wait_next(&signal, generation);
        let lua_args = args
            .iter()
            .map(|arg| signal_arg_to_lua(lua, &runtime, arg))
            .collect::<mlua::Result<Vec<_>>>()?;
        Ok(MultiValue::from_vec(lua_args))
    })
}

#[derive(Clone)]
struct LuaConnection {
    connection: ConnectionHandle,
}

impl UserData for LuaConnection {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: String| {
            match key.as_str() {
                "Disconnect" => Ok(Value::Function(make_disconnect(
                    lua,
                    this.connection.clone(),
                )?)),
                "Connected" => Ok(Value::Boolean(Signal::is_connected(
                    &this.connection.signal,
                    this.connection.id,
                ))),
                _ => Ok(Value::Nil),
            }
        });
    }
}

fn make_disconnect(lua: &Lua, connection: ConnectionHandle) -> Result<Function> {
    lua.create_function(move |lua, _self: Value| {
        Signal::disconnect(&connection.signal, lua, connection.id)?;
        Ok(())
    })
}

fn property_to_lua(lua: &Lua, value: &PropertyValue) -> Result<Value> {
    Ok(match value {
        PropertyValue::Bool(value) => Value::Boolean(*value),
        PropertyValue::Number(value) => Value::Number(*value),
        PropertyValue::String(value) => Value::String(lua.create_string(value)?),
        PropertyValue::BinaryString(value) => Value::String(lua.create_string(value)?),
        PropertyValue::Vector3(value) => Value::UserData(lua.create_userdata(value.clone())?),
        PropertyValue::Color3(value) => Value::UserData(lua.create_userdata(value.clone())?),
    })
}

fn lua_value_to_property_for_instance(
    instance: &InstanceRef,
    property_name: &str,
    value: &Value,
) -> Result<PropertyValue> {
    match value {
        Value::Boolean(value) => Ok(PropertyValue::Bool(*value)),
        Value::Integer(value) => Ok(PropertyValue::Number(*value as f64)),
        Value::Number(value) => Ok(PropertyValue::Number(*value)),
        Value::String(value)
            if instance.borrow().class_name == "StringValue" && property_name == "Value" =>
        {
            Ok(PropertyValue::BinaryString(value.as_bytes().to_vec()))
        }
        Value::String(value) => Ok(PropertyValue::String(value.to_str()?.to_string())),
        Value::UserData(userdata) if userdata.is::<Vector3>() => Ok(PropertyValue::Vector3(
            userdata.borrow::<Vector3>()?.clone(),
        )),
        Value::UserData(userdata) if userdata.is::<Color3>() => {
            Ok(PropertyValue::Color3(userdata.borrow::<Color3>()?.clone()))
        }
        _ => Err(Error::RuntimeError(
            "Unsupported property value; use booleans, numbers, strings, Vector3, or Color3"
                .to_string(),
        )),
    }
}

fn value_to_instance(value: Value) -> Result<Option<InstanceRef>> {
    match value {
        Value::Nil => Ok(None),
        Value::UserData(userdata) if userdata.is::<LuaInstance>() => {
            Ok(Some(userdata.borrow::<LuaInstance>()?.instance.clone()))
        }
        _ => Err(Error::RuntimeError("Expected an Instance".to_string())),
    }
}

fn script_chunk_name(instance: &InstanceRef) -> String {
    instance
        .borrow()
        .script_path
        .clone()
        .unwrap_or_else(|| Instance::full_name(instance))
}

fn normalize_script_path(path: &std::path::Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn format_console_output_line(lua: &Lua, message: &str, warning: bool) -> String {
    let timestamp = Local::now().format("%H:%M:%S%.3f").to_string();
    let location = current_console_location(lua).unwrap_or_else(|| "<unknown>:0:1".to_string());
    if warning {
        format!("[{timestamp}] [warn] {message}: {location}")
    } else {
        format!("[{timestamp}] {message}: {location}")
    }
}

fn current_console_location(lua: &Lua) -> Option<String> {
    lua.inspect_stack(1, |debug| {
        let source = debug.source();
        let short_src = source
            .short_src
            .as_deref()
            .or(source.source.as_deref())
            .map(|value| sanitize_debug_source(value))
            .unwrap_or_else(|| "<unknown>".to_string());
        let line = debug.current_line().unwrap_or(0);
        format!("{short_src}:{line}:1")
    })
}

fn sanitize_debug_source(source: &str) -> String {
    source
        .trim_start_matches('@')
        .trim_start_matches("[string \"")
        .trim_end_matches("\"]")
        .to_string()
}

fn values_to_console_string(values: &MultiValue) -> String {
    values
        .iter()
        .map(value_to_console_string)
        .collect::<Vec<_>>()
        .join("\t")
}

fn value_to_console_string(value: &Value) -> String {
    let mut visited = HashSet::new();
    value_to_console_string_inner(value, 0, &mut visited)
}

fn value_to_console_string_inner(
    value: &Value,
    depth: usize,
    visited: &mut HashSet<usize>,
) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.to_string_lossy(),
        Value::UserData(_) => "<userdata>".to_string(),
        Value::Table(table) => format_table_for_console(table, depth, visited),
        Value::Function(_) => "<function>".to_string(),
        Value::Thread(_) => "<thread>".to_string(),
        Value::LightUserData(_) => "<lightuserdata>".to_string(),
        Value::Buffer(_) => "<buffer>".to_string(),
        Value::Error(error) => error.to_string(),
        Value::Vector(value) => format!("Vector({}, {}, {})", value.x(), value.y(), value.z()),
        Value::Other(_) => "<other>".to_string(),
    }
}

fn format_table_for_console(table: &Table, depth: usize, visited: &mut HashSet<usize>) -> String {
    let pointer = table.to_pointer() as usize;
    if !visited.insert(pointer) {
        return "{<cycle>}".to_string();
    }

    let indent = "  ".repeat(depth);
    let child_indent = "  ".repeat(depth + 1);
    let mut lines = Vec::new();

    for pair in table.clone().pairs::<Value, Value>() {
        match pair {
            Ok((key, value)) => {
                let key_string = format_console_key(&key, depth + 1, visited);
                let value_string = value_to_console_string_inner(&value, depth + 1, visited);
                lines.push(format!("{child_indent}{key_string} = {value_string}"));
            }
            Err(error) => {
                lines.push(format!("{child_indent}<error> = {}", error));
            }
        }
    }

    visited.remove(&pointer);

    if lines.is_empty() {
        "{}".to_string()
    } else {
        format!("{{\n{}\n{indent}}}", lines.join(",\n"))
    }
}

fn format_console_key(value: &Value, depth: usize, visited: &mut HashSet<usize>) -> String {
    match value {
        Value::String(value) => format!("[\"{}\"]", value.to_string_lossy()),
        Value::Integer(value) => format!("[{value}]"),
        Value::Number(value) => format!("[{value}]"),
        _ => format!("[{}]", value_to_console_string_inner(value, depth, visited)),
    }
}

fn lua_value_to_json(value: &Value) -> Result<JsonValue> {
    match value {
        Value::Nil => Ok(JsonValue::Null),
        Value::Boolean(value) => Ok(JsonValue::Bool(*value)),
        Value::Integer(value) => Ok(JsonValue::Number((*value).into())),
        Value::Number(value) => serde_json::Number::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or_else(|| Error::RuntimeError("Cannot JSON encode NaN or infinity".to_string())),
        Value::String(value) => Ok(JsonValue::String(value.to_str()?.to_string())),
        Value::Table(table) => table_to_json(table),
        _ => Err(Error::RuntimeError(
            "HttpService:JSONEncode only supports nil, booleans, numbers, strings, and tables"
                .to_string(),
        )),
    }
}

fn table_to_json(table: &Table) -> Result<JsonValue> {
    let mut numeric_items = Vec::<(usize, JsonValue)>::new();
    let mut object_items = serde_json::Map::new();
    let mut has_string_keys = false;
    let mut has_non_array_keys = false;

    for pair in table.clone().pairs::<Value, Value>() {
        let (key, value) = pair?;
        match key {
            Value::Integer(index) if index > 0 => {
                numeric_items.push((index as usize, lua_value_to_json(&value)?));
            }
            Value::Number(index) if index.fract() == 0.0 && index > 0.0 => {
                numeric_items.push((index as usize, lua_value_to_json(&value)?));
            }
            Value::String(key) => {
                has_string_keys = true;
                object_items.insert(key.to_str()?.to_string(), lua_value_to_json(&value)?);
            }
            _ => {
                has_non_array_keys = true;
            }
        }
    }

    if has_non_array_keys {
        return Err(Error::RuntimeError(
            "HttpService:JSONEncode only supports string and positive integer table keys"
                .to_string(),
        ));
    }

    if !has_string_keys {
        numeric_items.sort_by_key(|(index, _)| *index);
        let contiguous = numeric_items
            .iter()
            .enumerate()
            .all(|(offset, (index, _))| *index == offset + 1);
        if contiguous {
            return Ok(JsonValue::Array(
                numeric_items.into_iter().map(|(_, value)| value).collect(),
            ));
        }
    }

    for (index, value) in numeric_items {
        object_items.insert(index.to_string(), value);
    }

    Ok(JsonValue::Object(object_items))
}

fn json_to_lua(lua: &Lua, value: &JsonValue) -> Result<Value> {
    Ok(match value {
        JsonValue::Null => Value::Nil,
        JsonValue::Bool(value) => Value::Boolean(*value),
        JsonValue::Number(value) => Value::Number(
            value
                .as_f64()
                .ok_or_else(|| Error::RuntimeError("Could not decode JSON number".to_string()))?,
        ),
        JsonValue::String(value) => Value::String(lua.create_string(value)?),
        JsonValue::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in items.iter().enumerate() {
                table.set(index + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        JsonValue::Object(items) => {
            let table = lua.create_table()?;
            for (key, item) in items {
                table.set(key.as_str(), json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::RobloxEnvironment;
    use crate::project::{LoadedProject, ProjectFile};
    use crate::runtime::RuntimeMode;
    use mlua::{Lua, Value};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn collect_project_files(root: &Path, prefix: &Path) -> Vec<ProjectFile> {
        fn walk(root: &Path, dir: &Path, prefix: &Path, out: &mut Vec<ProjectFile>) {
            let mut entries = fs::read_dir(dir)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.as_ref().map(|e| e.path()).ok());

            for entry in entries {
                let entry = entry.expect("directory entry");
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, prefix, out);
                    continue;
                }

                let relative = path.strip_prefix(root).unwrap_or_else(|error| {
                    panic!("strip prefix failed for {}: {error}", path.display())
                });
                out.push(ProjectFile {
                    relative_path: prefix.join(relative),
                    bytes: fs::read(&path).unwrap_or_else(|error| {
                        panic!("failed to read {}: {error}", path.display())
                    }),
                });
            }
        }

        let mut files = Vec::new();
        walk(root, root, prefix, &mut files);
        files
    }

    #[test]
    fn parts_stay_anchored_when_lua_tries_to_disable_them() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        env.run_script(
            "anchor_test",
            r#"
                local part = Instance.new("Part")
                part.Anchored = false
                assert(part.Anchored == true)
            "#,
        )
        .expect("script should succeed");
    }

    #[test]
    fn game_get_service_returns_workspace() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        env.run_script(
            "service_test",
            r#"
                local ws = game:GetService("Workspace")
                assert(ws ~= nil)
                assert(ws.ClassName == "Workspace")
            "#,
        )
        .expect("script should succeed");
    }

    #[test]
    fn client_mode_disables_character_auto_loads() {
        let env = RobloxEnvironment::new(RuntimeMode::Client).expect("environment");
        env.run_script(
            "client_test",
            r#"
                local players = game:GetService("Players")
                assert(players.CharacterAutoLoads == false)
                assert(players.LocalPlayer ~= nil)
            "#,
        )
        .expect("script should succeed");
    }

    #[test]
    fn client_cannot_access_server_only_services() {
        let env = RobloxEnvironment::new(RuntimeMode::Client).expect("environment");
        env.run_script(
            "service_visibility",
            r#"
                local ok_storage = pcall(function()
                    return game:GetService("ServerStorage")
                end)
                local ok_sss = pcall(function()
                    return game:GetService("ServerScriptService")
                end)
                assert(ok_storage == false)
                assert(ok_sss == false)
                assert(game.ServerStorage == nil)
                assert(game.ServerScriptService == nil)
            "#,
        )
        .expect("client should not see server-only services");
    }

    #[test]
    fn client_run_context_and_local_scripts_use_new_runtime_rules() {
        let env = RobloxEnvironment::new(RuntimeMode::Client).expect("environment");
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("Workspace/ClientBoot.client.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "WorkspaceClientRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/ClientReplica.client.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "ReplicatedStorageClientRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedFirst/LocalBoot.local.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "ReplicatedFirstLocalRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("Workspace/ShouldNotRun.local.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "WorkspaceLocalRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
            ],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_client_runtime_rules",
            r#"
                assert(workspace:FindFirstChild("WorkspaceClientRan") ~= nil)
                assert(workspace:FindFirstChild("ReplicatedStorageClientRan") ~= nil)
                assert(workspace:FindFirstChild("ReplicatedFirstLocalRan") ~= nil)
                assert(workspace:FindFirstChild("WorkspaceLocalRan") == nil)

                assert(workspace.ClientBoot.ClassName == "Script")
                assert(workspace.ClientBoot.RunContext == "Client")

                local rs = game:GetService("ReplicatedStorage")
                assert(rs.ClientReplica.ClassName == "Script")
                assert(rs.ClientReplica.RunContext == "Client")

                local rf = game:GetService("ReplicatedFirst")
                assert(rf.LocalBoot.ClassName == "LocalScript")
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn project_loader_runs_server_scripts_and_leaves_modules_requirable() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("Workspace/Boot.server.luau"),
                    bytes: br#"
                        local answer = require(script.Parent.Answer)
                        local part = Instance.new("Part")
                        part.Name = answer.Name
                        part.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("Workspace/Answer.luau"),
                    bytes: br#"return { Name = "LoadedFromModule" }"#.to_vec(),
                },
            ],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify",
            r#"
                local part = workspace:FindFirstChild("LoadedFromModule")
                assert(part ~= nil)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn server_and_legacy_run_contexts_follow_placement_rules() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/ServerBoot.server.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "ServerBootRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("Workspace/LegacyWorkspace.legacy.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "LegacyWorkspaceRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ServerScriptService/LegacyServer.legacy.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "LegacyServerRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/LegacyReplica.legacy.luau"),
                    bytes: br#"
                        local marker = Instance.new("StringValue")
                        marker.Name = "LegacyReplicaRan"
                        marker.Parent = workspace
                    "#
                    .to_vec(),
                },
            ],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_server_runtime_rules",
            r#"
                assert(workspace:FindFirstChild("ServerBootRan") ~= nil)
                assert(workspace:FindFirstChild("LegacyWorkspaceRan") ~= nil)
                assert(workspace:FindFirstChild("LegacyServerRan") ~= nil)
                assert(workspace:FindFirstChild("LegacyReplicaRan") == nil)

                local rs = game:GetService("ReplicatedStorage")
                assert(rs.ServerBoot.ClassName == "Script")
                assert(rs.ServerBoot.RunContext == "Server")
                assert(rs.LegacyReplica.RunContext == "Legacy")
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn script_disable_directive_keeps_script_instance_without_running_it() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![ProjectFile {
                relative_path: PathBuf::from("ServerScriptService/Boot.server.luau"),
                bytes: br#"
                    --!rle script-disable
                    local marker = Instance.new("StringValue")
                    marker.Name = "ShouldNotExist"
                    marker.Parent = workspace
                "#
                .to_vec(),
            }],
        };

        env.run_project(project).expect("project should load");
        env.run_script(
            "verify_disabled_script",
            r#"
                local sss = game:GetService("ServerScriptService")
                local boot = sss:FindFirstChild("Boot")
                assert(boot ~= nil)
                assert(boot.ClassName == "Script")
                assert(workspace:FindFirstChild("ShouldNotExist") == nil)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn init_module_owns_its_folder_children() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/init.luau"),
                    bytes: br#"return { Value = 7 }"#.to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("ReplicatedStorage/Foo/Child.luau"),
                    bytes: br#"return 12"#.to_vec(),
                },
            ],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_init",
            r#"
                local foo = game:GetService("ReplicatedStorage").Foo
                assert(foo ~= nil)
                assert(foo.ClassName == "ModuleScript")
                assert(foo.Child ~= nil)
                assert(foo.Child.ClassName == "ModuleScript")
                local result = require(foo)
                assert(result.Value == 7)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn external_text_files_load_into_string_value_value() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![ProjectFile {
                relative_path: PathBuf::from("ExternalData/config.json"),
                bytes: br#"{"enabled":true}"#.to_vec(),
            }],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_external_text",
            r#"
                local config = game:FindFirstChild("config.json")
                assert(config ~= nil)
                assert(config.ClassName == "StringValue")
                assert(config.Value == '{"enabled":true}')
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn external_binary_files_preserve_raw_bytes() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let project = LoadedProject {
            files: vec![ProjectFile {
                relative_path: PathBuf::from("ExternalData/hello.elf"),
                bytes: vec![0x7f, 0x45, 0x4c, 0x46, 0x00, 0x02, 0xff],
            }],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_external_binary",
            r#"
                local elf = game:FindFirstChild("hello.elf")
                assert(elf ~= nil)
                assert(elf.ClassName == "StringValue")
                local value = elf.Value
                assert(#value == 7)
                local b1, b2, b3, b4, b5, b6, b7 = string.byte(value, 1, 7)
                assert(b1 == 0x7f)
                assert(b2 == 0x45)
                assert(b3 == 0x4c)
                assert(b4 == 0x46)
                assert(b5 == 0x00)
                assert(b6 == 0x02)
                assert(b7 == 0xff)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn elfluau_runs_compiled_strlen_probe() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let module_root = Path::new("projects/ElfLuau/ReplicatedStorage/ElfLuau");
        let probe_path = Path::new("projects/ElfLuau/ExternalData/elfluau_probe.elf");

        let mut files = collect_project_files(module_root, Path::new("ReplicatedStorage/ElfLuau"));
        files.push(ProjectFile {
            relative_path: PathBuf::from("ExternalData/elfluau_probe.elf"),
            bytes: fs::read(probe_path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", probe_path.display())),
        });

        env.run_project(LoadedProject { files })
            .expect("project should load");
        env.run_script(
            "verify_elfluau_probe",
            r#"
                local rs = game:GetService("ReplicatedStorage")
                local ElfLuau = require(rs:WaitForChild("ElfLuau"))
                local code = ElfLuau.translateExternal("elfluau_probe.elf")
                assert(code:find("local fn_L_401026", 1, true) ~= nil)
                assert(code:find("while true do", 1, true) ~= nil)
                local result = ElfLuau.runExternal("elfluau_probe.elf", {
                    argv = { "/probe" },
                })
                assert(result.exitCode == 0)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn elfluau_runs_musl_strlen_probe() {
        let env = RobloxEnvironment::new(RuntimeMode::Server).expect("environment");
        let module_root = Path::new("projects/ElfLuau/ReplicatedStorage/ElfLuau");
        let probe_path = Path::new("projects/ElfLuau/ExternalData/musl_strlen.elf");

        let mut files = collect_project_files(module_root, Path::new("ReplicatedStorage/ElfLuau"));
        files.push(ProjectFile {
            relative_path: PathBuf::from("ExternalData/musl_strlen.elf"),
            bytes: fs::read(probe_path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", probe_path.display())),
        });

        env.run_project(LoadedProject { files })
            .expect("project should load");
        env.run_script(
            "verify_musl_strlen_probe",
            r#"
                local rs = game:GetService("ReplicatedStorage")
                local ElfLuau = require(rs:WaitForChild("ElfLuau"))
                local code = ElfLuau.translateExternal("musl_strlen.elf")
                assert(code:find("local fn_L_401026", 1, true) ~= nil)
                assert(code:find("while true do", 1, true) ~= nil)
                local result = ElfLuau.runExternal("musl_strlen.elf", {
                    argv = { "/probe" },
                })
                assert(result.exitCode == 0)
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn only_server_can_set_network_owner() {
        let server = RobloxEnvironment::new(RuntimeMode::Server).expect("server");
        server
            .run_script(
                "server_owner",
                r#"
                    local players = game:GetService("Players")
                    local player = Instance.new("Player")
                    player.Name = "Owner"
                    player.Parent = players

                    local part = Instance.new("Part")
                    part:SetNetworkOwner(player)
                    assert(part:GetNetworkOwner() == player)
                "#,
            )
            .expect("server should set owner");

        let client = RobloxEnvironment::new(RuntimeMode::Client).expect("client");
        client
            .run_script(
                "client_owner",
                r#"
                    local part = Instance.new("Part")
                    local ok = pcall(function()
                        part:SetNetworkOwner(game:GetService("Players").LocalPlayer)
                    end)
                    assert(ok == false)
                "#,
            )
            .expect("client should not set owner");
    }

    #[test]
    fn starter_player_scripts_copy_into_player_scripts_in_client_mode() {
        let env = RobloxEnvironment::new(RuntimeMode::Client).expect("environment");
        let project = LoadedProject {
            files: vec![
                ProjectFile {
                    relative_path: PathBuf::from("StarterPlayerScripts/Boot.local.luau"),
                    bytes: br#"
                        local counter = workspace:FindFirstChild("LocalBootRuns")
                        if counter == nil then
                            counter = Instance.new("StringValue")
                            counter.Name = "LocalBootRuns"
                            counter.Value = "1"
                            counter.Parent = workspace
                        else
                            counter.Value = tostring(tonumber(counter.Value) + 1)
                        end
                    "#
                    .to_vec(),
                },
                ProjectFile {
                    relative_path: PathBuf::from("StarterPlayerScripts/ClientBoot.client.luau"),
                    bytes: br#"
                        local counter = workspace:FindFirstChild("ClientBootRuns")
                        if counter == nil then
                            counter = Instance.new("StringValue")
                            counter.Name = "ClientBootRuns"
                            counter.Value = "1"
                            counter.Parent = workspace
                        else
                            counter.Value = tostring(tonumber(counter.Value) + 1)
                        end
                    "#
                    .to_vec(),
                },
            ],
        };

        env.run_project(project).expect("project should run");
        env.run_script(
            "verify_starter_player_copy",
            r#"
                local players = game:GetService("Players")
                local player = players.LocalPlayer
                assert(player ~= nil)
                assert(player.Name == "Player1")
                assert(players.Player1 == player)
                assert(#players:GetPlayers() == 1)

                local playerScripts = player:FindFirstChild("PlayerScripts")
                assert(playerScripts ~= nil)
                assert(playerScripts:FindFirstChild("Boot") ~= nil)
                assert(playerScripts:FindFirstChild("Boot").ClassName == "LocalScript")
                assert(playerScripts:FindFirstChild("ClientBoot") ~= nil)
                assert(playerScripts:FindFirstChild("ClientBoot").ClassName == "Script")
                assert(playerScripts:FindFirstChild("ClientBoot").RunContext == "Client")

                local starterPlayer = game:GetService("StarterPlayer")
                assert(starterPlayer.StarterPlayerScripts ~= nil)
                assert(starterPlayer.StarterPlayerScripts:FindFirstChild("Boot") ~= nil)
                assert(starterPlayer.StarterPlayerScripts:FindFirstChild("ClientBoot") ~= nil)

                assert(workspace.LocalBootRuns.Value == "1")
                assert(workspace.ClientBootRuns.Value == "1")
            "#,
        )
        .expect("verification should succeed");
    }

    #[test]
    fn console_formatter_pretty_prints_tables() {
        let lua = Lua::new();
        let table = lua.create_table().expect("table");
        let nested = lua.create_table().expect("nested");
        nested.set("value", 5).expect("nested value");
        table.set("name", "demo").expect("name");
        table.set("nested", nested).expect("nested");

        let formatted = super::value_to_console_string(&Value::Table(table));
        assert!(formatted.contains('\n'));
        assert!(formatted.contains("  [\"name\"] = demo"));
        assert!(formatted.contains("  [\"nested\"] = {\n"));
        assert!(formatted.contains("    [\"value\"] = 5"));
    }
}
