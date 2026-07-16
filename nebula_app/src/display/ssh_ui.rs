//! SSH editor and reversible-deletion state types.
//!
//! Keeping these out of `display::mod` makes the security-sensitive lifetime
//! rule explicit: a pending deletion owns credential cleanup and only Undo may
//! disarm it.

use super::text_input::SelectAllState;
use crate::ssh_profiles::SshAuthMode;
use std::path::PathBuf;

/// How long a destructive SSH action stays reversible in the in-app bar.
pub const SSH_DELETE_UNDO_DURATION: std::time::Duration = std::time::Duration::from_secs(8);

pub fn auth_sections(mode: SshAuthMode) -> (bool, bool) {
    match mode {
        SshAuthMode::Auto => (true, true),
        SshAuthMode::Password => (true, false),
        SshAuthMode::PublicKey => (false, true),
        SshAuthMode::Agent | SshAuthMode::KeyboardInteractive => (false, false),
    }
}

pub fn push_private_key(keys: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    let normalized = path.to_string_lossy();
    if keys.iter().any(|existing| existing.to_string_lossy().eq_ignore_ascii_case(&normalized)) {
        false
    } else {
        keys.push(path);
        true
    }
}

#[derive(Debug)]
pub(super) struct SshDeleteUndo {
    pub(super) host: String,
    pub(super) saved_index: Option<usize>,
    pub(super) pinned_index: Option<usize>,
    pub(super) was_hidden: bool,
    pub(super) from_config: bool,
    pub(super) started_at: std::time::Instant,
    pub(super) delete_credential_on_drop: bool,
}

impl Drop for SshDeleteUndo {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.delete_credential_on_drop {
            let _ = crate::ssh_credentials::forget_password(&self.host);
            let path = super::nebula_data_dir().join("ssh_profiles.json");
            if let Ok(mut profiles) = crate::ssh_profiles::SshProfiles::load(&path) {
                profiles.remove(&self.host);
                let _ = profiles.save(&path);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshEditorField {
    Destination,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshEditorHit {
    None,
    Destination,
    Password,
    PasswordToggle,
    Auth(SshAuthMode),
    AddPrivateKey,
    RemovePrivateKey(usize),
    SaveToggleBox,
    SaveToggleLabel,
    Primary,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct SshHostEditor {
    /// Destination before editing, when this modal was opened from a row.
    pub original_destination: Option<String>,
    pub destination: String,
    pub password: String,
    pub save_password: bool,
    pub show_password: bool,
    pub auth: SshAuthMode,
    pub private_keys: Vec<PathBuf>,
    pub field: SshEditorField,
    pub focus: crate::ux::FocusIndex,
    pub error: Option<String>,
    pub(super) destination_selection: SelectAllState,
    pub(super) password_selection: SelectAllState,
}

#[derive(Debug, Clone)]
pub struct SshEditorRects {
    pub destination: (f32, f32, f32, f32),
    pub password: (f32, f32, f32, f32),
    pub password_toggle: (f32, f32, f32, f32),
    pub auth: [(SshAuthMode, (f32, f32, f32, f32)); 5],
    pub add_private_key: (f32, f32, f32, f32),
    pub private_key_rows: Vec<(usize, (f32, f32, f32, f32))>,
    pub save_checkbox: (f32, f32, f32, f32),
    pub save_toggle: (f32, f32, f32, f32),
    pub primary: (f32, f32, f32, f32),
    pub cancel: (f32, f32, f32, f32),
}

#[cfg(test)]
mod tests {
    use super::{auth_sections, push_private_key};
    use crate::ssh_profiles::SshAuthMode;
    use std::path::PathBuf;

    #[test]
    fn auth_modes_show_the_same_sections_as_tabby() {
        assert_eq!(auth_sections(SshAuthMode::Auto), (true, true));
        assert_eq!(auth_sections(SshAuthMode::Password), (true, false));
        assert_eq!(auth_sections(SshAuthMode::PublicKey), (false, true));
        assert_eq!(auth_sections(SshAuthMode::Agent), (false, false));
        assert_eq!(auth_sections(SshAuthMode::KeyboardInteractive), (false, false));
    }

    #[test]
    fn private_key_list_keeps_order_and_deduplicates_windows_paths() {
        let mut keys = Vec::new();
        assert!(push_private_key(&mut keys, PathBuf::from(r"C:\Keys\first")));
        assert!(!push_private_key(&mut keys, PathBuf::from(r"c:\keys\FIRST")));
        assert!(push_private_key(&mut keys, PathBuf::from(r"C:\Keys\second")));
        assert_eq!(keys, vec![PathBuf::from(r"C:\Keys\first"), PathBuf::from(r"C:\Keys\second")]);
    }
}
