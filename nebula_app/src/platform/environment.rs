//! Startup compatibility for platform environment variable names.

/// Must run before any threads start: environment mutation is process-global.
pub unsafe fn import_environment_aliases() {
    for (name, value) in environment_aliases(std::env::vars_os().collect()) {
        unsafe { std::env::set_var(name, value) };
    }
}

pub(crate) fn environment_aliases(
    variables: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, std::ffi::OsString)> {
    let mut aliases = Vec::new();
    for (name, value) in &variables {
        let Some(name) = name.to_str() else { continue };
        let canonical = if cfg!(windows) { name.to_ascii_uppercase() } else { name.to_owned() };
        let name = canonical.as_str();
        if let Some(suffix) = name.strip_prefix("PEBREL_") {
            if configuration_override(suffix) && value.is_empty() {
                continue;
            }
            aliases.push((format!("NEBULA_{suffix}"), value.clone()));
        } else if let Some(suffix) = name.strip_prefix("NEBULA_") {
            if configuration_override(suffix) && value.is_empty() {
                continue;
            }
            let current = format!("PEBREL_{suffix}");
            if !variables.iter().any(|(name, value)| {
                name.to_str().is_some_and(|name| {
                    let matches = if cfg!(windows) {
                        name.eq_ignore_ascii_case(&current)
                    } else {
                        name == current
                    };
                    matches && !(configuration_override(suffix) && value.is_empty())
                })
            }) {
                aliases.push((current, value.clone()));
            }
        }
    }
    aliases
}

fn configuration_override(suffix: &str) -> bool {
    matches!(suffix, "CONFIG_DIR" | "CONFIG_FILE" | "GPUI_CONFIG")
}

#[cfg(test)]
mod tests {
    #[test]
    fn environment_aliases_preserve_legacy_inputs_and_prefer_explicit_pebrel_values() {
        let aliases = super::environment_aliases(
            [
                ("NEBULA_CONFIG_DIR", "legacy"),
                ("PEBREL_CONFIG_DIR", "current"),
                ("NEBULA_PANE_REMOTE", "1"),
                ("OTHER_TOOL", "unchanged"),
            ]
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        );
        assert_eq!(
            aliases,
            vec![
                ("NEBULA_CONFIG_DIR".to_owned(), "current".into()),
                ("PEBREL_PANE_REMOTE".to_owned(), "1".into()),
            ]
        );
    }

    #[test]
    fn empty_configuration_overrides_fall_back_without_reopening_an_empty_hook_scope() {
        let aliases = super::environment_aliases(
            [
                ("PEBREL_CONFIG_DIR", ""),
                ("NEBULA_CONFIG_DIR", "legacy-config"),
                ("PEBREL_CONFIG_FILE", ""),
                ("NEBULA_CONFIG_FILE", "legacy.toml"),
                ("PEBREL_GPUI_CONFIG", ""),
                ("NEBULA_GPUI_CONFIG", "legacy-gpui.toml"),
                ("PEBREL_NOTIFY_PIPE", ""),
                ("NEBULA_NOTIFY_PIPE", "stale-pipe"),
            ]
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        );
        assert_eq!(
            aliases,
            vec![
                ("PEBREL_CONFIG_DIR".to_owned(), "legacy-config".into()),
                ("PEBREL_CONFIG_FILE".to_owned(), "legacy.toml".into()),
                ("PEBREL_GPUI_CONFIG".to_owned(), "legacy-gpui.toml".into()),
                ("NEBULA_NOTIFY_PIPE".to_owned(), "".into()),
            ]
        );
    }
}
