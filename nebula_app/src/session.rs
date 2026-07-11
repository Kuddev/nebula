//! Session restore: reopen with the same tabs (and their directories) you had
//! when the window closed — Otty-style, no "restore?" dialog.
//!
//! A snapshot is written continuously (1 Hz, skipped when nothing changed), so
//! a crash or force-kill still restores to within a second of where you were.
//! `boot_attempts` guards against a restore-crash loop: it's bumped before the
//! restore is attempted and cleared by the first successful autosave, so after
//! three failed launches Nebula starts clean to break the cycle.
//!
//! v1 restores the tab list + per-tab working directory + active tab. Split
//! trees inside a tab collapse to their focused pane's cwd for now.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Highest snapshot format this build understands.
const VERSION: u32 = 1;

/// Give up restoring after this many launches that never reached a successful
/// autosave (i.e. crashed within the first second).
const MAX_BOOT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSession {
    /// Working directory of the tab's focused pane.
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub version: u32,
    /// Launches since the last successful autosave (crash-loop breaker).
    #[serde(default)]
    pub boot_attempts: u32,
    pub active_tab: usize,
    pub tabs: Vec<TabSession>,
}

impl Session {
    pub fn new(active_tab: usize, tabs: Vec<TabSession>) -> Self {
        Self { version: VERSION, boot_attempts: 0, active_tab, tabs }
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
    let session: Session = serde_json::from_str(&data).ok()?;
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
