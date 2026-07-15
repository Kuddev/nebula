use std::env;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_ENV: &str = "NEBULA_CONFIG_FILE";

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
    pub config_home: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
    pub executable_dir: Option<PathBuf>,
    pub system_dir: Option<PathBuf>,
    pub windows_portable: bool,
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
            config_home,
            home_dir,
            executable_dir: env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_owned)),
            #[cfg(windows)]
            system_dir: None,
            #[cfg(not(windows))]
            system_dir: Some(PathBuf::from("/etc/nebula")),
            windows_portable: cfg!(windows),
        }
    }

    fn candidates(&self, extension: &str) -> Vec<PathBuf> {
        let file_name = format!("nebula.{extension}");
        let mut paths = Vec::new();

        if self.windows_portable {
            if let Some(executable_dir) = &self.executable_dir {
                paths.push(executable_dir.join(&file_name));
            }
        }

        if let Some(config_home) = &self.config_home {
            paths.push(config_home.join("nebula").join(&file_name));
            #[cfg(not(windows))]
            paths.push(config_home.join(&file_name));
        }

        if let Some(home) = &self.home_dir {
            #[cfg(not(windows))]
            paths.push(home.join(".config").join("nebula").join(&file_name));
            paths.push(home.join(format!(".{file_name}")));
        }

        if let Some(system_dir) = &self.system_dir {
            paths.push(system_dir.join(&file_name));
        }

        paths.dedup();
        paths
    }
}

pub fn discover(explicit_path: Option<PathBuf>) -> Result<Option<ConfigSource>, String> {
    let environment_path = env::var_os(CONFIG_FILE_ENV).filter(|value| !value.is_empty());
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

    // 先按格式、再按位置查找，确保任何 Lua 配置都优先于旧格式。
    for extension in ["lua", "toml", "yml", "yaml"] {
        for path in roots.candidates(extension) {
            if path.is_file() {
                return source_for_path(path, false).map(Some);
            }
        }
    }

    Ok(None)
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
    let roots = DiscoveryRoots::from_environment();
    roots
        .config_home
        .map(|root| root.join("nebula").join("nebula.lua"))
        .or_else(|| roots.home_dir.map(|home| home.join(".nebula.lua")))
        .ok_or_else(|| "unable to determine the user configuration directory".to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn roots(base: &Path) -> DiscoveryRoots {
        DiscoveryRoots {
            config_home: Some(base.join("config")),
            home_dir: Some(base.join("home")),
            executable_dir: Some(base.join("bin")),
            system_dir: Some(base.join("etc")),
            windows_portable: true,
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
}
