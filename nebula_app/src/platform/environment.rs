//! Startup aliases and platform compatibility for local terminal environments.

/// Refresh local pane environments before adding identity and other pane overrides.
pub(crate) fn prepare_local_pty(options: &mut nebula_terminal::tty::Options) {
    #[cfg(windows)]
    {
        if let Err(error) = nebula_terminal::tty::refresh_environment(options) {
            log::warn!("Could not refresh the Windows environment for a new pane: {error}");
        }
        let inherited_override =
            !options.env_is_complete && std::env::var_os(GROK_LEGACY_CONSOLE).is_some();
        apply_windows_terminal_defaults(options, inherited_override);
    }
    #[cfg(not(windows))]
    let _ = options;
}

#[cfg(windows)]
const GROK_LEGACY_CONSOLE: &str = "GROK_FORCE_LEGACY_CONSOLE";

#[cfg(windows)]
fn apply_windows_terminal_defaults(
    options: &mut nebula_terminal::tty::Options,
    inherited_override: bool,
) {
    // Grok 1.0.25 treats unknown Windows terminal names as legacy consoles and
    // omits its Braille logo. Keep our real identity and use its capability override.
    // Remove this default when Grok recognizes Pebrel's terminal capabilities.
    // A complete refreshed environment is authoritative, including deleted keys.
    if (!options.env_is_complete && inherited_override)
        || options.env.keys().any(|name| name.eq_ignore_ascii_case(GROK_LEGACY_CONSOLE))
    {
        return;
    }
    options.env.insert(GROK_LEGACY_CONSOLE.to_owned(), "0".to_owned());
}

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
    #[cfg(windows)]
    #[test]
    fn local_terminal_defaults_enable_grok_unicode_without_changing_identity() {
        let mut options = nebula_terminal::tty::Options {
            env: [("TERM_PROGRAM", "pebrel"), ("WSLENV", "KEEP/p")]
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            env_is_complete: true,
            ..Default::default()
        };
        super::apply_windows_terminal_defaults(&mut options, false);
        assert_eq!(options.env[super::GROK_LEGACY_CONSOLE], "0");
        assert_eq!(options.env["TERM_PROGRAM"], "pebrel");
        assert_eq!(options.env["WSLENV"], "KEEP/p");
        assert!(!options.env.contains_key("WT_SESSION"));
        let once = options.clone();
        super::apply_windows_terminal_defaults(&mut options, false);
        assert_eq!(options, once);
    }

    #[cfg(windows)]
    #[test]
    fn local_terminal_defaults_preserve_explicit_grok_overrides_case_insensitively() {
        for name in [super::GROK_LEGACY_CONSOLE, "grok_force_legacy_console"] {
            for value in ["1", "true", "0", "false", ""] {
                let mut options = nebula_terminal::tty::Options {
                    env: [(name.to_owned(), value.to_owned())].into_iter().collect(),
                    env_is_complete: true,
                    ..Default::default()
                };
                let original = options.clone();
                super::apply_windows_terminal_defaults(&mut options, false);
                assert_eq!(options, original);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn inherited_grok_override_only_applies_to_incomplete_environments() {
        let mut options = nebula_terminal::tty::Options::default();
        super::apply_windows_terminal_defaults(&mut options, true);
        assert!(!options.env.contains_key(super::GROK_LEGACY_CONSOLE));

        // A successful refresh can remove a stale value from the parent's registry
        // snapshot. Do not resurrect it when the new complete environment omits it.
        options.env_is_complete = true;
        super::apply_windows_terminal_defaults(&mut options, true);
        assert_eq!(options.env[super::GROK_LEGACY_CONSOLE], "0");

        let mut fallback = nebula_terminal::tty::Options::default();
        super::apply_windows_terminal_defaults(&mut fallback, false);
        assert_eq!(fallback.env[super::GROK_LEGACY_CONSOLE], "0");
        assert!(!fallback.env_is_complete);
    }

    #[cfg(windows)]
    #[test]
    fn refreshed_local_terminal_preserves_the_pane_grok_override() {
        let mut options = nebula_terminal::tty::Options {
            env: [("grok_force_legacy_console".to_owned(), "1".to_owned())].into_iter().collect(),
            ..Default::default()
        };
        super::prepare_local_pty(&mut options);
        let overrides: Vec<_> = options
            .env
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(super::GROK_LEGACY_CONSOLE))
            .collect();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].1, "1");
    }

    #[cfg(not(windows))]
    #[test]
    fn local_terminal_preparation_preserves_non_windows_environments() {
        let mut options = nebula_terminal::tty::Options {
            env: [("TERM_PROGRAM".to_owned(), "pebrel".to_owned())].into_iter().collect(),
            ..Default::default()
        };
        let original = options.clone();
        super::prepare_local_pty(&mut options);
        assert_eq!(options, original);
    }

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
