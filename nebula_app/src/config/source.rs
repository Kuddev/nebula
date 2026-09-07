use std::env;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_ENV: &str = "PEBREL_CONFIG_FILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Lua,
    Toml,
    Yaml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    pub primary_path: PathBuf,
    pub format: ConfigFormat,
    pub explicit: bool,
}

#[derive(Debug, Clone)]
pub struct DiscoveryRoots {
    pub data_dir: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
    pub executable_dir: Option<PathBuf>,
    pub system_dir: Option<PathBuf>,
    pub windows_portable: bool,
    pub isolated: bool,
}

impl DiscoveryRoots {
    pub fn from_environment() -> Self {
        let home_dir = home::home_dir();

        #[cfg(windows)]
        let config_home = dirs::config_dir();
        #[cfg(not(windows))]
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home_dir.as_ref().map(|home| home.join(".config")));

        Self {
            data_dir: Some(nebula_settings::settings_dir()),
            config_home,
            home_dir,
            executable_dir: env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_owned)),
            #[cfg(windows)]
            system_dir: None,
            #[cfg(not(windows))]
            system_dir: Some(PathBuf::from("/etc")),
            windows_portable: cfg!(windows),
            isolated: ["PEBREL_CONFIG_DIR", "NEBULA_CONFIG_DIR"]
                .into_iter()
                .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty())),
        }
    }

    fn branded_candidates(&self, brand: &str, extension: &str) -> Vec<PathBuf> {
        let file_name = format!("{brand}.{extension}");
        let mut paths = Vec::new();

        if self.isolated {
            if let Some(data_dir) = &self.data_dir {
                paths.push(data_dir.join(file_name));
            }
            return paths;
        }

        if self.windows_portable {
            if let Some(executable_dir) = &self.executable_dir {
                paths.push(executable_dir.join(&file_name));
            }
        }

        if let Some(data_dir) = &self.data_dir {
            paths.push(data_dir.join(&file_name));
        }

        if let Some(config_home) = &self.config_home {
            paths.push(config_home.join(brand).join(&file_name));
            if brand == "nebula" {
                paths.push(config_home.join("pebrel").join(&file_name));
            }
            #[cfg(not(windows))]
            paths.push(config_home.join(&file_name));
        }

        if let Some(home) = &self.home_dir {
            #[cfg(not(windows))]
            paths.push(home.join(".config").join(brand).join(&file_name));
            paths.push(home.join(format!(".{file_name}")));
        }

        if let Some(system_dir) = &self.system_dir {
            paths.push(system_dir.join(brand).join(&file_name));
        }

        paths.dedup();
        paths
    }
}

pub fn discover(explicit_path: Option<PathBuf>) -> Result<Option<ConfigSource>, String> {
    let environment_path = [CONFIG_FILE_ENV, "NEBULA_CONFIG_FILE"]
        .into_iter()
        .find_map(|name| env::var_os(name).filter(|value| !value.is_empty()));
    discover_with(
        &DiscoveryRoots::from_environment(),
        explicit_path,
        environment_path.map(PathBuf::from),
    )
}

pub fn discover_with(
    roots: &DiscoveryRoots,
    explicit_path: Option<PathBuf>,
    environment_path: Option<PathBuf>,
) -> Result<Option<ConfigSource>, String> {
    if let Some(path) = explicit_path.or(environment_path) {
        return source_for_path(path, true).map(Some);
    }

    // A deliberately created Pebrel config takes priority over every legacy format.
    for brand in ["pebrel", "nebula"] {
        for extension in ["lua", "toml", "yml", "yaml"] {
            for path in roots.branded_candidates(brand, extension) {
                if path.is_file() {
                    return source_for_path(path, false).map(Some);
                }
            }
        }
    }

    Ok(None)
}

pub(crate) fn discover_toml() -> Option<PathBuf> {
    let roots = DiscoveryRoots::from_environment();
    ["pebrel", "nebula"]
        .into_iter()
        .flat_map(|brand| roots.branded_candidates(brand, "toml"))
        .find(|path| path.is_file())
}

pub fn source_for_path(path: PathBuf, explicit: bool) -> Result<ConfigSource, String> {
    let extension =
        path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase);
    let format = match extension.as_deref() {
        Some("lua") => ConfigFormat::Lua,
        Some("toml") => ConfigFormat::Toml,
        Some("yml" | "yaml") => ConfigFormat::Yaml,
        _ => {
            return Err(format!(
                "unsupported configuration extension for {} (expected .lua, .toml, .yml, or .yaml)",
                path.display()
            ));
        },
    };
    Ok(ConfigSource { primary_path: path, format, explicit })
}

pub fn default_lua_path() -> Result<PathBuf, String> {
    Ok(nebula_settings::settings_dir().join("pebrel.lua"))
}

/// Resolve the Lua file edited by the settings command.
///
/// Only the first loaded config is authoritative. Later entries can be imports;
/// opening one of those merely because it has a `.lua` suffix would edit a
/// dependency instead of the user's entry point.
pub fn user_lua_path(active_paths: &[PathBuf]) -> Result<PathBuf, String> {
    active_user_lua_path(active_paths).map(Ok).unwrap_or_else(default_lua_path)
}

fn active_user_lua_path(active_paths: &[PathBuf]) -> Option<PathBuf> {
    active_paths
        .first()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lua"))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn roots(base: &Path) -> DiscoveryRoots {
        DiscoveryRoots {
            data_dir: Some(base.join("config/pebrel")),
            config_home: Some(base.join("config")),
            home_dir: Some(base.join("home")),
            executable_dir: Some(base.join("bin")),
            system_dir: Some(base.join("etc")),
            windows_portable: true,
            isolated: false,
        }
    }

    #[test]
    fn explicit_and_environment_paths_have_priority() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let explicit = temp.path().join("explicit.lua");
        let environment = temp.path().join("environment.toml");
        let source = discover_with(&roots, Some(explicit.clone()), Some(environment)).unwrap();
        assert_eq!(source.unwrap().primary_path, explicit);
    }

    #[test]
    fn lua_precedes_legacy_config_even_when_lua_is_broken() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        fs::create_dir_all(temp.path().join("bin")).unwrap();
        fs::create_dir_all(temp.path().join("config/nebula")).unwrap();
        fs::write(temp.path().join("bin/nebula.toml"), "[scrolling]\nhistory=123").unwrap();
        fs::write(temp.path().join("config/nebula/nebula.lua"), "invalid lua").unwrap();

        let source = discover_with(&roots, None, None).unwrap().unwrap();
        assert_eq!(source.format, ConfigFormat::Lua);
        assert!(source.primary_path.ends_with("config/nebula/nebula.lua"));
    }

    #[test]
    fn rejects_unknown_explicit_extension() {
        let error = source_for_path(PathBuf::from("nebula.json"), true).unwrap_err();
        assert!(error.contains("unsupported configuration extension"));
    }

    #[test]
    fn pebrel_config_precedes_legacy_lua_and_data_overrides_stay_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = roots(temp.path());
        fs::create_dir_all(temp.path().join("config/pebrel")).unwrap();
        fs::create_dir_all(temp.path().join("config/nebula")).unwrap();
        fs::write(temp.path().join("config/pebrel/pebrel.toml"), "[font]\nsize=12").unwrap();
        fs::write(temp.path().join("config/nebula/nebula.lua"), "return {}").unwrap();
        let source = discover_with(&roots, None, None).unwrap().unwrap();
        assert_eq!(source.format, ConfigFormat::Toml);
        assert!(source.primary_path.ends_with("pebrel/pebrel.toml"));

        roots.isolated = true;
        roots.data_dir = Some(temp.path().join("portable"));
        assert_eq!(discover_with(&roots, None, None).unwrap(), None);
        fs::create_dir_all(temp.path().join("portable")).unwrap();
        fs::write(temp.path().join("portable/nebula.lua"), "return {}").unwrap();
        assert!(
            discover_with(&roots, None, None)
                .unwrap()
                .unwrap()
                .primary_path
                .ends_with("portable/nebula.lua")
        );
    }

    #[test]
    fn user_lua_path_prefers_the_active_lua_entry_point() {
        let active = PathBuf::from("D:/config/NEBULA.LUA");
        let imported = PathBuf::from("D:/config/import.lua");
        assert_eq!(user_lua_path(&[active.clone(), imported]).unwrap(), active);
    }

    #[test]
    fn imported_lua_is_not_treated_as_the_entry_point() {
        let active = PathBuf::from("D:/config/nebula.toml");
        let imported = PathBuf::from("D:/config/import.lua");
        assert_eq!(active_user_lua_path(&[active, imported]), None);
    }
}
