use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const RESET_KEYS: &[&str] = &[
    "language",
    "theme",
    "app_icon",
    "follow_system_theme",
    "font_family",
    "font_size",
    "cursor_shape",
    "cursor_blink",
    "copy_on_select",
    "multiline_paste_confirm",
    "tab_close_visible",
    "terminal_proxy",
    "powerline",
    "shell",
    "executor",
    "startup_directory",
    "ghost",
    "accept",
    "completion_style",
    "cjk_bold_regular",
    "tabs_position",
    "tab_reveal",
    "density",
    "new_tab_position",
    "windowing_behavior",
    "cell_width_mode",
    "vcs_display",
    "bell",
    "fetch",
    "auto_check_updates",
    "keep_session",
    "restore_session",
    "resume_ai",
    "tray",
    "blur",
    "opacity",
    "background",
    "background_image",
    "background_image_opacity",
    "background_image_fit",
    "background_image_alignment",
    "background_image_cover_chrome",
    "panel_resize",
    "sidebar_w",
    "ssh_proxy_mode",
    "ssh_proxy_url",
    "ssh_proxy_no_proxy",
    "quick_terminal_hotkey",
    "pane_card_radius",
    "pane_card_gutter",
    "pane_card_shadow",
    "pane_card_divider",
    "keybind",
];

static RESET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn restore_default_settings() -> io::Result<Option<PathBuf>> {
    restore_defaults_at(&super::settings_path())
}

fn default_settings_text(text: &str) -> String {
    text.split_inclusive('\n')
        .filter(|line| {
            !line.split_once('=').is_some_and(|(key, _)| {
                RESET_KEYS.iter().any(|known| key.trim().eq_ignore_ascii_case(known))
            })
        })
        .collect()
}

fn restore_defaults_at(path: &Path) -> io::Result<Option<PathBuf>> {
    let original = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let restored = default_settings_text(original.as_deref().unwrap_or_default());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let sequence = RESET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{timestamp}-{}-{sequence}", std::process::id());
    let backup =
        original.as_ref().map(|_| path.with_extension(format!("before-reset-{suffix}.bak")));
    if let (Some(text), Some(backup)) = (&original, &backup) {
        let mut file = OpenOptions::new().write(true).create_new(true).open(backup)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    let temporary = path.with_extension(format!("reset-{suffix}.tmp"));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let written = file.write_all(restored.as_bytes()).and_then(|_| file.sync_all());
    drop(file);
    let result = written.and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map(|_| backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawSettings, RuntimeSettings};

    #[test]
    fn reset_removes_all_overrides_and_keeps_user_data() {
        let text = "# preferences\r\n THEME = Nord\r\ncopy_on_select=1\nkeybind=ctrl+x:Copy\nFONT_SIZE=30\nexecutor=custom\nssh_hosts=saved-host\nai_provider=custom\nfuture_setting=keep\n";
        let result = default_settings_text(text);
        assert_eq!(
            result,
            "# preferences\r\nssh_hosts=saved-host\nai_provider=custom\nfuture_setting=keep\n"
        );
        let runtime = RuntimeSettings::from_raw(&RawSettings::from_text(&result));
        let defaults = RuntimeSettings::from_raw(&RawSettings::default());
        assert_eq!(runtime.theme, defaults.theme);
        assert_eq!(runtime.copy_on_select, defaults.copy_on_select);
        assert!(runtime.font_size_px.is_none());
        assert!(runtime.shell.is_none());
        assert!(crate::keybind_pairs_from_text(&result).is_empty());
    }

    #[test]
    fn duplicate_keys_and_legacy_aliases_cannot_override_the_reset() {
        let text = "theme=Nord\nTHEME=Paper\nshell=pwsh\nexecutor=cmd\nkeybind=ctrl+a:Copy\nkeybind=ctrl+b:Paste\n";
        assert_eq!(default_settings_text(text), "");
    }

    #[test]
    fn reset_covers_every_runtime_settings_key() {
        let source = include_str!("lib.rs");
        let reader = source.split("pub fn from_raw(raw: &RawSettings) -> Self").nth(1).unwrap();
        let reader: String = reader
            .split("/// 解析")
            .next()
            .unwrap()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        for accessor in ["raw.value(\"", "raw.f32(\"", "raw.bool_on(\""] {
            for expression in reader.split(accessor).skip(1) {
                let key = expression.split('"').next().unwrap();
                assert!(RESET_KEYS.contains(&key), "missing reset key: {key}");
            }
        }
    }

    #[test]
    fn reset_backs_up_the_original_without_touching_adjacent_files() {
        let directory = std::env::temp_dir().join(format!(
            "nebula-reset-test-{}-{}",
            std::process::id(),
            RESET_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("nebula_settings.txt");
        let hosts = directory.join("ssh_hosts.json");
        fs::write(&path, "theme=Nord\nssh_hosts=keep\n").unwrap();
        fs::write(&hosts, "user data").unwrap();
        let backup = restore_defaults_at(&path).unwrap().unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "theme=Nord\nssh_hosts=keep\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "ssh_hosts=keep\n");
        assert_eq!(fs::read_to_string(&hosts).unwrap(), "user data");
        let next_backup = restore_defaults_at(&path).unwrap().unwrap();
        assert_ne!(backup, next_backup);
        assert_eq!(fs::read_to_string(&backup).unwrap(), "theme=Nord\nssh_hosts=keep\n");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_settings_file_is_not_overwritten() {
        let directory = std::env::temp_dir().join(format!(
            "nebula-reset-invalid-{}-{}",
            std::process::id(),
            RESET_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("nebula_settings.txt");
        fs::write(&path, [0xff, 0xfe, 0x80]).unwrap();
        assert!(restore_defaults_at(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe, 0x80]);
        fs::remove_dir_all(directory).unwrap();
    }
}
