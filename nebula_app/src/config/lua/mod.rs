use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

use mlua::Value as LuaValue;
use nebula_config::{ConfigDiagnostic, UnknownFieldPolicy, capture_diagnostics};
use serde::Deserialize;

use crate::config::UiConfig;

pub mod convert;
pub mod runtime;

use convert::{LuaValueError, lua_value_to_toml};
use runtime::{LuaRuntime, ReloadSignal, builder_strict};

pub struct LuaGeneration {
    pub lua: mlua::Lua,
    pub watched_paths: Vec<PathBuf>,
}

pub struct LoadedLuaConfig {
    pub config: UiConfig,
    pub generation: LuaGeneration,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug)]
pub enum LuaConfigError {
    Io(std::io::Error),
    Lua(mlua::Error),
    Value(LuaValueError),
    Diagnostics(Vec<ConfigDiagnostic>),
    Message(String),
}

impl Display for LuaConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Lua(error) => write!(formatter, "{error}"),
            Self::Value(error) => write!(formatter, "{error}"),
            Self::Diagnostics(diagnostics) => {
                for (index, diagnostic) in diagnostics.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("\n")?;
                    }
                    formatter.write_str(&diagnostic.message)?;
                }
                Ok(())
            },
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for LuaConfigError {}

impl From<std::io::Error> for LuaConfigError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<mlua::Error> for LuaConfigError {
    fn from(error: mlua::Error) -> Self {
        Self::Lua(error)
    }
}

impl From<LuaValueError> for LuaConfigError {
    fn from(error: LuaValueError) -> Self {
        Self::Value(error)
    }
}

pub fn load_lua_file(path: &Path, reload: ReloadSignal) -> Result<LoadedLuaConfig, LuaConfigError> {
    let source = fs::read_to_string(path)?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let runtime = LuaRuntime::new(path, reload)?;
    let value: LuaValue = runtime.lua().load(source).set_name(path.to_string_lossy()).eval()?;
    let strict = builder_strict(&value);
    let (mut config, diagnostics) = validate_config_value(runtime.lua(), value, strict)?;
    let mut watched_paths = vec![path.canonicalize().unwrap_or_else(|_| path.to_owned())];
    for watched in runtime.watched_paths() {
        if !watched_paths.contains(&watched) {
            watched_paths.push(watched);
        }
    }
    config.config_paths = watched_paths.clone();
    let lua = runtime.into_lua();

    Ok(LoadedLuaConfig { config, generation: LuaGeneration { lua, watched_paths }, diagnostics })
}

pub(super) fn validate_config_value(
    lua: &mlua::Lua,
    value: LuaValue,
    strict: bool,
) -> Result<(UiConfig, Vec<ConfigDiagnostic>), LuaConfigError> {
    if !matches!(value, LuaValue::Table(_)) {
        return Err(LuaConfigError::Message(format!(
            "configuration must return a table or config builder, got {}",
            value.type_name()
        )));
    }
    let value = lua_value_to_toml(lua, value)?;
    let policy = if strict { UnknownFieldPolicy::Deny } else { UnknownFieldPolicy::Warn };
    let (config, diagnostics) = capture_diagnostics(policy, || UiConfig::deserialize(value));
    let config = config.map_err(|error| LuaConfigError::Message(error.to_string()))?;
    let errors: Vec<_> =
        diagnostics.iter().filter(|diagnostic| diagnostic.is_error()).cloned().collect();
    if !errors.is_empty() {
        return Err(LuaConfigError::Diagnostics(errors));
    }
    Ok((config, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_utf8_bom_config_and_tracks_required_modules() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("theme.lua"), "return { history = 9876 }").unwrap();
        fs::write(
            temp.path().join("nebula.lua"),
            "\u{feff}local n=require 'nebula'\nlocal s=require 'theme'\nreturn { scrolling=s }",
        )
        .unwrap();

        let loaded =
            load_lua_file(&temp.path().join("nebula.lua"), ReloadSignal::default()).unwrap();
        assert_eq!(loaded.config.scrolling.history(), 9876);
        assert!(loaded.generation.watched_paths.iter().any(|path| path.ends_with("theme.lua")));
    }

    #[test]
    fn rejects_unknown_fields_and_invalid_values() {
        let temp = tempfile::tempdir().unwrap();
        for (name, source) in [
            ("unknown.lua", "return { windwo = {} }"),
            ("invalid.lua", "return { scrolling = { history = 'many' } }"),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, source).unwrap();
            assert!(load_lua_file(&path, ReloadSignal::default()).is_err(), "{source}");
        }
    }

    #[test]
    fn config_builder_validates_top_level_assignments() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nebula.lua");
        fs::write(
            &path,
            r#"
            local nebula = require 'nebula'
            local config = nebula.config_builder()
            config.scrolling = { history = 4242 }
            return config
            "#,
        )
        .unwrap();
        let loaded = load_lua_file(&path, ReloadSignal::default()).unwrap();
        assert_eq!(loaded.config.scrolling.history(), 4242);
    }
}
