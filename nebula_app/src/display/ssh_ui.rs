//! SSH editor and reversible-deletion state types.
//!
//! Keeping these out of `display::mod` makes the security-sensitive lifetime
//! rule explicit: a pending deletion owns credential cleanup and only Undo may
//! disarm it.

use super::text_input::SelectAllState;

/// How long a destructive SSH action stays reversible in the in-app bar.
pub const SSH_DELETE_UNDO_DURATION: std::time::Duration = std::time::Duration::from_secs(8);

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
    pub field: SshEditorField,
    pub error: Option<String>,
    pub(super) destination_selection: SelectAllState,
    pub(super) password_selection: SelectAllState,
}

#[derive(Debug, Clone, Copy)]
pub struct SshEditorRects {
    pub destination: (f32, f32, f32, f32),
    pub password: (f32, f32, f32, f32),
    pub password_toggle: (f32, f32, f32, f32),
    pub save_checkbox: (f32, f32, f32, f32),
    pub save_toggle: (f32, f32, f32, f32),
    pub primary: (f32, f32, f32, f32),
    pub cancel: (f32, f32, f32, f32),
}
