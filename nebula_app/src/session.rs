//! Session restore: reopen with the same tabs (and their directories) you had
//! when the window closed — Otty-style, no "restore?" dialog.
//!
//! A snapshot is written continuously (1 Hz, skipped when nothing changed), so
//! a crash or force-kill still restores to within a second of where you were.
//! `boot_attempts` guards against a restore-crash loop: it's bumped before the
//! restore is attempted and cleared by the first successful autosave, so after
//! three failed launches Nebula starts clean to break the cycle.
//!
//! v2 additionally preserves each tab's custom name and optional color. v3
//! persists the normal logical window size and maximized state. Split trees
//! inside a tab still collapse to their focused pane's cwd for now.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::display::color::Rgb;

/// Highest snapshot format this build understands.
const VERSION: u32 = 3;

/// Give up restoring after this many launches that never reached a successful
/// autosave (i.e. crashed within the first second).
const MAX_BOOT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSession {
    /// Working directory of the tab's focused pane.
    pub cwd: String,
    /// User override from inline rename. `None` keeps cwd/title-derived labels.
    #[serde(default)]
    pub custom_name: Option<String>,
    /// User-selected tab light-strip color. `None` follows the current theme.
    #[serde(default)]
    pub color: Option<Rgb>,
}

/// Last normal (non-maximized, non-fullscreen) inner size in logical pixels.
/// Logical units keep the perceived size stable when the next launch lands on
/// a monitor with a different DPI scale factor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowState {
    pub fn valid_size(self) -> bool {
        self.width >= 100 && self.height >= 100
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub version: u32,
    /// Launches since the last successful autosave (crash-loop breaker).
    #[serde(default)]
    pub boot_attempts: u32,
    pub active_tab: usize,
    pub tabs: Vec<TabSession>,
    #[serde(default)]
    pub window: Option<WindowState>,
}

impl Session {
    pub fn new(active_tab: usize, tabs: Vec<TabSession>) -> Self {
        Self { version: VERSION, boot_attempts: 0, active_tab, tabs, window: None }
    }
}

/// `%APPDATA%\Nebula\session.json` (or the `.config` fallback), next to the
/// settings and history files.
fn session_path() -> PathBuf {
    crate::display::nebula_data_dir().join("session.json")
}

/// Load the previous session, if any and version-compatible.
pub fn load() -> Option<Session> {
    let data = std::fs::read_to_string(session_path()).ok()?;
    let mut session: Session = serde_json::from_str(&data).ok()?;
    // Defaults fill fields introduced after v1. Upgrade in memory so the first
    // successful autosave rewrites the current format.
    if matches!(session.version, 1 | 2) {
        session.version = VERSION;
    }
    (session.version == VERSION).then_some(session)
}

/// Persist `session`. Best-effort: failures must never take the terminal down.
pub fn save(session: &Session) {
    if let Ok(json) = serde_json::to_string(session) {
        let _ = std::fs::write(session_path(), json);
    }
}

/// Whether a loaded session should actually be restored: respects the
/// crash-loop breaker and skips empty sessions (a clean quit — every tab
/// closed one by one — persists an empty tab list on purpose).
pub fn should_restore(session: &Session) -> bool {
    session.boot_attempts < MAX_BOOT_ATTEMPTS && !session.tabs.is_empty()
}

/// A saved cwd as a `PathBuf`, if it still exists on disk. A vanished
/// directory must not sink the pane spawn — ConPTY fails outright on an
/// invalid startup directory — so callers fall back to the default cwd.
pub fn valid_dir(cwd: &str) -> Option<PathBuf> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    let path = PathBuf::from(cwd);
    path.is_dir().then_some(path)
}

/// Bump the attempt counter on disk before a restore is tried, so a crash
/// during/after restore is counted against the loop breaker.
pub fn mark_boot_attempt(session: &mut Session) {
    session.boot_attempts += 1;
    save(session);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_tabs_deserialize_with_default_metadata() {
        let json = r#"{"version":1,"boot_attempts":0,"active_tab":0,"tabs":[{"cwd":"D:/work"}]}"#;
        let session: Session = serde_json::from_str(json).unwrap();
        assert_eq!(session.version, 1);
        assert_eq!(session.tabs[0].custom_name, None);
        assert_eq!(session.tabs[0].color, None);
    }

    #[test]
    fn v2_round_trip_preserves_tab_name_and_color() {
        let session = Session::new(
            0,
            vec![TabSession {
                cwd: "D:/work".into(),
                custom_name: Some("Backend".into()),
                color: Some(Rgb::new(97, 175, 239)),
            }],
        );
        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, session);
    }

    #[test]
    fn older_session_without_window_state_stays_compatible() {
        let json = r#"{"version":2,"boot_attempts":0,"active_tab":0,"tabs":[{"cwd":"D:/work"}]}"#;
        let session: Session = serde_json::from_str(json).unwrap();
        assert_eq!(session.window, None);
    }

    #[test]
    fn window_state_round_trip_preserves_logical_size_and_maximize() {
        let mut session = Session::new(0, Vec::new());
        session.window = Some(WindowState { width: 1280, height: 720, maximized: true });
        let json = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.window, session.window);
    }
}
