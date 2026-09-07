use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

#[path = "paths/migration.rs"]
mod migration;

pub use migration::{canonical_data_file_name, is_migration_artifact, legacy_data_file_name};

fn first_override(mut variable: impl FnMut(&str) -> Option<OsString>) -> Option<PathBuf> {
    ["PEBREL_CONFIG_DIR", "NEBULA_CONFIG_DIR"]
        .into_iter()
        .find_map(|name| variable(name).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

fn override_dir() -> Option<PathBuf> {
    first_override(|name| std::env::var_os(name))
}

fn default_dir(name: &str) -> PathBuf {
    #[cfg(windows)]
    let root = std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        });
    let name = if cfg!(all(unix, not(target_os = "macos"))) {
        name.to_lowercase()
    } else {
        name.to_owned()
    };
    root.unwrap_or_else(std::env::temp_dir).join(name)
}

/// Pebrel's data directory. The legacy override remains an input alias.
pub fn settings_dir() -> PathBuf {
    override_dir().unwrap_or_else(|| default_dir("Pebrel"))
}

pub fn settings_path() -> PathBuf {
    settings_dir().join("pebrel_settings.txt")
}

/// Run at process startup before settings, databases or background tasks open.
pub fn migrate_legacy_data() -> io::Result<()> {
    let destination = settings_dir();
    let source = override_dir().unwrap_or_else(|| default_dir("Nebula"));
    migration::migrate_data_at(&source, &destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Could not migrate Nebula data from {} to {}: {error}",
                source.display(),
                destination.display(),
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_override_precedes_legacy_alias_and_empty_values_are_ignored() {
        assert_eq!(
            first_override(|name| Some(OsString::from(name))),
            Some(PathBuf::from("PEBREL_CONFIG_DIR"))
        );
        assert_eq!(
            first_override(|name| Some(OsString::from(if name == "PEBREL_CONFIG_DIR" {
                ""
            } else {
                "legacy"
            }))),
            Some(PathBuf::from("legacy"))
        );
        assert_eq!(first_override(|_| None), None);
    }
}
