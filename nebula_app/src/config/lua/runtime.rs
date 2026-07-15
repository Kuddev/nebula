use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, Value};

use super::{LuaConfigError, validate_config_value};

pub const ARRAY_MARKER: &str = "__nebula_array";
pub const BUILDER_MARKER: &str = "__nebula_config_builder";
pub const BUILDER_STORE: &str = "__nebula_builder_store";
const BUILDER_STRICT: &str = "__nebula_builder_strict";

#[derive(Clone, Default)]
pub struct ReloadSignal(Arc<AtomicBool>);

impl ReloadSignal {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

pub struct LuaRuntime {
    lua: Lua,
    watched_paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl LuaRuntime {
    pub fn new(config_file: &Path, reload: ReloadSignal) -> Result<Self, LuaConfigError> {
        let lua = Lua::new();
        let watched_paths = Arc::new(Mutex::new(Vec::new()));
        let runtime = Self { lua, watched_paths };
        runtime.register(config_file, reload)?;
        Ok(runtime)
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.watched_paths.lock().unwrap_or_else(|lock| lock.into_inner()).clone()
    }

    pub fn into_lua(self) -> Lua {
        self.lua
    }

    fn register(&self, config_file: &Path, reload: ReloadSignal) -> Result<(), LuaConfigError> {
        let config_dir = config_file.parent().unwrap_or_else(|| Path::new("."));
        let globals = self.lua.globals();
        let package: Table = globals.get("package")?;
        let loaded: Table = package.get("loaded")?;
        let module = self.lua.create_table()?;

        module.set("config_file", path_string(config_file)?)?;
        module.set("config_dir", path_string(config_dir)?)?;
        module.set(
            "home_dir",
            home::home_dir().as_deref().map(path_string).transpose()?.unwrap_or_default(),
        )?;
        module.set(
            "executable_dir",
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_owned))
                .as_deref()
                .map(path_string)
                .transpose()?
                .unwrap_or_default(),
        )?;
        module.set("version", env!("CARGO_PKG_VERSION"))?;
        module.set("target_triple", option_env!("TARGET").unwrap_or("unknown"))?;

        let platform = self.lua.create_table()?;
        platform.set("os", std::env::consts::OS)?;
        platform.set("arch", std::env::consts::ARCH)?;
        platform.set("display_server", display_server())?;
        module.set("platform", platform)?;

        module.set(
            "array",
            self.lua.create_function(|lua, values: Option<Table>| {
                let values = values.unwrap_or(lua.create_table()?);
                let metatable = values.metatable().unwrap_or(lua.create_table()?);
                metatable.set(ARRAY_MARKER, true)?;
                values.set_metatable(Some(metatable))?;
                Ok(values)
            })?,
        )?;

        let reload_signal = reload.clone();
        module.set(
            "reload_configuration",
            self.lua.create_function(move |_, ()| {
                reload_signal.request();
                Ok(())
            })?,
        )?;

        let watched_paths = self.watched_paths.clone();
        let lua_config_dir = path_string(config_dir)?.replace('\\', "/");
        let config_dir = config_dir.to_owned();
        module.set(
            "add_to_config_reload_watch_list",
            self.lua.create_function(move |_, path: String| {
                let path = PathBuf::from(path);
                let path = if path.is_absolute() { path } else { config_dir.join(path) };
                let path = path.canonicalize().unwrap_or(path);
                let mut paths = watched_paths.lock().unwrap_or_else(|lock| lock.into_inner());
                if paths.len() >= 1024 {
                    return Err(mlua::Error::external(
                        "configuration watch list exceeds 1024 paths",
                    ));
                }
                if !paths.contains(&path) {
                    paths.push(path);
                }
                Ok(())
            })?,
        )?;

        for (name, level) in [
            ("log_error", log::Level::Error),
            ("log_warn", log::Level::Warn),
            ("log_info", log::Level::Info),
        ] {
            module.set(
                name,
                self.lua.create_function(move |_, message: String| {
                    log::log!(target: "nebula_lua", level, "{message}");
                    Ok(())
                })?,
            )?;
        }

        module.set("config_builder", self.lua.create_function(create_config_builder)?)?;
        loaded.set("nebula", module)?;

        let current_path: String = package.get("path")?;
        package.set(
            "path",
            format!("{lua_config_dir}/?.lua;{lua_config_dir}/?/init.lua;{current_path}"),
        )?;
        self.lua
            .load(
                r#"
                local original = package.searchers[2]
                package.searchers[2] = function(name)
                    local path = package.searchpath(name, package.path)
                    if path then
                        package.loaded.nebula.add_to_config_reload_watch_list(path)
                    end
                    return original(name)
                end
                "#,
            )
            .set_name("=nebula-module-watcher")
            .exec()?;

        Ok(())
    }
}

fn create_config_builder(lua: &Lua, (): ()) -> mlua::Result<Table> {
    let config = lua.create_table()?;
    let store = lua.create_table()?;
    let metatable = lua.create_table()?;
    metatable.set(BUILDER_MARKER, true)?;
    metatable.set(BUILDER_STORE, store.clone())?;
    metatable.set(BUILDER_STRICT, true)?;

    let index_store = store.clone();
    metatable.set(
        "__index",
        lua.create_function(move |lua, (_table, key): (Table, String)| {
            if key == "set_strict_mode" {
                let function = lua.create_function(|_, (table, strict): (Table, bool)| {
                    if let Some(metatable) = table.metatable() {
                        metatable.set(BUILDER_STRICT, strict)?;
                    }
                    Ok(())
                })?;
                return Ok(Value::Function(function));
            }
            let value: Value = index_store.raw_get(key.clone())?;
            if !matches!(value, Value::Nil) {
                return Ok(value);
            }
            let nested = lua.create_table()?;
            index_store.raw_set(key, nested.clone())?;
            Ok(Value::Table(nested))
        })?,
    )?;

    let assignment_store = store.clone();
    metatable.set(
        "__newindex",
        lua.create_function(move |lua, (_table, key, value): (Table, String, Value)| {
            let patch = lua.create_table()?;
            patch.set(key.clone(), value.clone())?;
            validate_config_value(lua, Value::Table(patch), true).map_err(mlua::Error::external)?;
            assignment_store.raw_set(key, value)
        })?,
    )?;

    let pairs_store = store.clone();
    metatable.set(
        "__pairs",
        lua.create_function(move |lua, _table: Table| {
            let next: mlua::Function = lua.globals().get("next")?;
            Ok((next, pairs_store.clone(), Value::Nil))
        })?,
    )?;
    config.set_metatable(Some(metatable))?;
    Ok(config)
}

pub fn builder_strict(value: &Value) -> bool {
    let Value::Table(table) = value else { return true };
    table
        .metatable()
        .filter(|metatable| metatable.get::<bool>(BUILDER_MARKER).unwrap_or(false))
        .and_then(|metatable| metatable.get::<bool>(BUILDER_STRICT).ok())
        .unwrap_or(true)
}

fn path_string(path: &Path) -> Result<String, LuaConfigError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        LuaConfigError::Message(format!("path is not valid UTF-8: {}", path.display()))
    })
}

fn display_server() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if std::env::var("WINIT_UNIX_BACKEND")
        .is_ok_and(|value| value.eq_ignore_ascii_case("x11"))
    {
        "x11"
    } else if std::env::var("WINIT_UNIX_BACKEND")
        .is_ok_and(|value| value.eq_ignore_ascii_case("wayland"))
    {
        "wayland"
    } else if std::env::var("XDG_SESSION_TYPE")
        .is_ok_and(|value| value.eq_ignore_ascii_case("wayland"))
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        "wayland"
    } else if std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("x11"))
        || std::env::var_os("DISPLAY").is_some()
    {
        "x11"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nebula_module_exposes_stable_foundation_api() {
        let reload = ReloadSignal::default();
        let runtime = LuaRuntime::new(Path::new("nebula.lua"), reload.clone()).unwrap();
        let values: (String, String, bool, bool) = runtime
            .lua()
            .load(
                r#"
                local nebula = require 'nebula'
                local empty = nebula.array()
                nebula.reload_configuration()
                return nebula.platform.os, nebula.version,
                       getmetatable(empty).__nebula_array, nebula.on == nil
                "#,
            )
            .eval()
            .unwrap();
        assert!(!values.0.is_empty());
        assert!(!values.1.is_empty());
        assert!(values.2);
        assert!(values.3);
        assert!(reload.take());
    }
}
