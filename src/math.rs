use std::fmt;

use mlua::{MetaMethod, UserData, UserDataMethods};

#[derive(Clone, Debug, PartialEq)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }
}

impl Default for Vector3 {
    fn default() -> Self {
        Self::zero()
    }
}

impl fmt::Display for Vector3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, {}, {}", self.x, self.y, self.z)
    }
}

impl UserData for Vector3 {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |_, this, key: String| {
            Ok(match key.as_str() {
                "X" => mlua::Value::Number(this.x),
                "Y" => mlua::Value::Number(this.y),
                "Z" => mlua::Value::Number(this.z),
                _ => mlua::Value::Nil,
            })
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Vector3({this})"))
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Color3 {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Color3 {
    pub const fn new(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b }
    }

    pub const fn gray() -> Self {
        Self::new(0.639_215_686_3, 0.639_215_686_3, 0.639_215_686_3)
    }
}

impl Default for Color3 {
    fn default() -> Self {
        Self::gray()
    }
}

impl fmt::Display for Color3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, {}, {}", self.r, self.g, self.b)
    }
}

impl UserData for Color3 {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |_, this, key: String| {
            Ok(match key.as_str() {
                "R" => mlua::Value::Number(this.r),
                "G" => mlua::Value::Number(this.g),
                "B" => mlua::Value::Number(this.b),
                _ => mlua::Value::Nil,
            })
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Color3({this})"))
        });
    }
}
