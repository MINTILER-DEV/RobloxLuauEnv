use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Function, Lua, MultiValue, RegistryKey, Result, Value};

use crate::instance::InstanceRef;
use crate::lua_api::LuaInstance;
use crate::math::{Color3, Vector3};
use crate::runtime::Runtime;

pub type SignalRef = Rc<RefCell<Signal>>;

#[derive(Debug)]
pub struct Signal {
    name: String,
    next_connection_id: u64,
    listeners: Vec<Listener>,
}

#[derive(Debug)]
struct Listener {
    id: u64,
    callback: RegistryKey,
    once: bool,
}

#[derive(Clone, Debug)]
pub struct ConnectionHandle {
    pub signal: SignalRef,
    pub id: u64,
}

#[derive(Clone, Debug)]
pub enum SignalArg {
    Nil,
    String(String),
    Instance(InstanceRef),
    Vector3(Vector3),
    Color3(Color3),
}

impl Signal {
    pub fn named(name: impl Into<String>) -> SignalRef {
        Rc::new(RefCell::new(Self {
            name: name.into(),
            next_connection_id: 1,
            listeners: Vec::new(),
        }))
    }

    pub fn connect(
        signal: &SignalRef,
        lua: &Lua,
        callback: Function,
        once: bool,
    ) -> Result<ConnectionHandle> {
        let key = lua.create_registry_value(callback)?;
        let mut signal_mut = signal.borrow_mut();
        let id = signal_mut.next_connection_id;
        signal_mut.next_connection_id += 1;
        signal_mut.listeners.push(Listener {
            id,
            callback: key,
            once,
        });
        Ok(ConnectionHandle {
            signal: signal.clone(),
            id,
        })
    }

    pub fn disconnect(signal: &SignalRef, lua: &Lua, id: u64) -> Result<bool> {
        let mut signal_mut = signal.borrow_mut();
        if let Some(index) = signal_mut
            .listeners
            .iter()
            .position(|listener| listener.id == id)
        {
            let listener = signal_mut.listeners.remove(index);
            lua.remove_registry_value(listener.callback)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn is_connected(signal: &SignalRef, id: u64) -> bool {
        signal
            .borrow()
            .listeners
            .iter()
            .any(|listener| listener.id == id)
    }

    pub fn fire(
        signal: &SignalRef,
        lua: &Lua,
        runtime: &Runtime,
        args: &[SignalArg],
    ) -> Result<()> {
        let calls = {
            let signal_ref = signal.borrow();
            signal_ref
                .listeners
                .iter()
                .map(|listener| {
                    let function: Function = lua.registry_value(&listener.callback)?;
                    Ok((listener.id, listener.once, function))
                })
                .collect::<mlua::Result<Vec<_>>>()?
        };

        let lua_args = args
            .iter()
            .map(|arg| signal_arg_to_lua(lua, runtime, arg))
            .collect::<mlua::Result<Vec<_>>>()?;

        let signal_name = signal.borrow().name.clone();
        let mut disconnect_ids = Vec::new();
        for (id, once, callback) in calls {
            if let Err(error) = callback.call::<()>(MultiValue::from_vec(lua_args.clone())) {
                eprintln!("[roblox-env warning] signal '{signal_name}' callback failed: {error}");
            }
            if once {
                disconnect_ids.push(id);
            }
        }

        for id in disconnect_ids {
            let _ = Self::disconnect(signal, lua, id);
        }

        Ok(())
    }
}

fn signal_arg_to_lua(lua: &Lua, runtime: &Runtime, arg: &SignalArg) -> mlua::Result<Value> {
    Ok(match arg {
        SignalArg::Nil => Value::Nil,
        SignalArg::String(value) => Value::String(lua.create_string(value)?),
        SignalArg::Instance(instance) => Value::UserData(
            lua.create_userdata(LuaInstance::new(runtime.clone(), instance.clone()))?,
        ),
        SignalArg::Vector3(value) => Value::UserData(lua.create_userdata(value.clone())?),
        SignalArg::Color3(value) => Value::UserData(lua.create_userdata(value.clone())?),
    })
}
