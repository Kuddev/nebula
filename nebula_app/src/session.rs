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
//! persists the normal logical window size and maximized state. v4 records the
//! full split tree of every tab (axis, ratio, per-pane cwd) plus the tab's
//! launch identity (shell / profile / SSH destination), and the same schema
//! doubles as the workspace-export file format: `session.json` is simply the
//! automatic, unnamed workspace.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::display::color::Rgb;

/// Highest snapshot format this build understands.
const VERSION: u32 = 4;

/// Give up restoring after this many launches that never reached a successful
/// autosave (i.e. crashed within the first second).
const MAX_BOOT_ATTEMPTS: u32 = 3;

/// How a tab's first pane starts. The persistable subset of `TabLaunch`:
/// document and settings tabs never enter a session. Shell and profile
/// launches embed their full command so an exported workspace stays portable
/// even when the target machine's config lists different profiles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchSession {
    Default,
    /// A detected shell from the new-tab dropdown (e.g. a WSL distro).
    Shell { name: String, program: String, args: Vec<String> },
    /// A quick-launch profile, embedded rather than referenced by name.
    Profile { name: String, command: String, args: Vec<String>, cwd: Option<String> },
    /// A saved SSH destination; restoring reconnects automatically.
    Ssh { host: String },
}

/// Split axis, mirrored from `display::SplitDirection` so the display layer
/// stays serde-free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    LeftRight,
    TopBottom,
}

/// A tab's pane tree. Leaves carry each pane's working directory; splits carry
/// the axis and the first child's share in permille — an integer, so autosave
/// change detection and file diffs never trip on f32 serialization noise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutSession {
    Pane {
        cwd: String,
    },
    Split {
        axis: SplitAxis,
        ratio_permille: u16,
        first: Box<LayoutSession>,
        second: Box<LayoutSession>,
    },
}

impl LayoutSession {
    /// Number of panes (leaves) in this tree.
    pub fn pane_count(&self) -> usize {
        match self {
            Self::Pane { .. } => 1,
            Self::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }

    /// Working directory of the depth-first first leaf — the leaf a restored
    /// tab's first pane adopts.
    pub fn first_cwd(&self) -> &str {
        match self {
            Self::Pane { cwd } => cwd,
            Self::Split { first, .. } => first.first_cwd(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabSession {
    /// Working directory of the tab's focused pane. Kept alongside `layout`
    /// because the boot path seeds its first pane from it before the tree is
    /// rebuilt, and v1–v3 files carry nothing else.
    pub cwd: String,
    /// User override from inline rename. `None` keeps cwd/title-derived labels.
    #[serde(default)]
    pub custom_name: Option<String>,
    /// User-selected tab light-strip color. `None` follows the current theme.
    #[serde(default)]
    pub color: Option<Rgb>,
    /// v4: how the first pane starts. `None` (older file) means `Default`.
    #[serde(default)]
    pub launch: Option<LaunchSession>,
    /// v4: the full split tree. `None` (older file) is a single pane at `cwd`.
    #[serde(default)]
    pub layout: Option<LayoutSession>,
    /// v4: focused leaf as a depth-first index into `layout`.
    #[serde(default)]
    pub active_pane: usize,
}

impl TabSession {
    /// A v3-shaped tab: one pane at `cwd`, default shell.
    pub fn single(cwd: String, custom_name: Option<String>, color: Option<Rgb>) -> Self {
        Self { cwd, custom_name, color, launch: None, layout: None, active_pane: 0 }
    }
}

/// Last normal (non-maximized, non-fullscreen) inner size in logical pixels.
/// Logical units keep the perceived size stable when the next launch lands on
/// a monitor with a different DPI scale factor. Write-only since v4: startup
/// always sizes the window from the configured column/line count, so this
/// record is diagnostic and forward-compat data, never replayed at boot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
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

/// Parse a session/workspace document, upgrading older versions in place.
fn parse(data: &str) -> Option<Session> {
    let mut session: Session = serde_json::from_str(data).ok()?;
    // Defaults fill fields introduced after v1. Upgrade in memory so the first
    // successful autosave (or re-export) rewrites the current format.
    if matches!(session.version, 1..=3) {
        session.version = VERSION;
    }
    (session.version == VERSION).then_some(session)
}

/// Load the previous session, if any and version-compatible.
pub fn load() -> Option<Session> {
    load_from(&session_path())
}

/// Load a session/workspace file from an explicit path (workspace import).
pub fn load_from(path: &Path) -> Option<Session> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Persist `session`. Best-effort: failures must never take the terminal down.
/// The atomic replace matters here — this file is written every second and a
/// crash mid-write must not cost the very session it exists to restore.
pub fn save(session: &Session) {
    if let Ok(json) = serde_json::to_string(session) {
        let _ = crate::atomic_file::write(&session_path(), json.as_bytes());
    }
}

/// Write a session as a named workspace file. Pretty-printed — workspace
/// files are user-visible artifacts meant to be read, diffed and versioned.
pub fn save_to(path: &Path, session: &Session) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(session).map_err(std::io::Error::other)?;
    crate::atomic_file::write(path, json.as_bytes())
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
        assert_eq!(session.tabs[0].layout, None);
        assert_eq!(session.tabs[0].launch, None);
    }

    #[test]
    fn v2_round_trip_preserves_tab_name_and_color() {
        let session = Session::new(
            0,
            vec![TabSession::single(
                "D:/work".into(),
                Some("Backend".into()),
                Some(Rgb::new(97, 175, 239)),
            )],
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

    #[test]
    fn v3_file_upgrades_in_place_to_v4() {
        let json = r#"{"version":3,"boot_attempts":1,"active_tab":0,"tabs":[{"cwd":"D:/work"}]}"#;
        let session = parse(json).expect("v3 must parse");
        assert_eq!(session.version, VERSION);
        assert_eq!(session.tabs[0].layout, None);
        assert_eq!(session.tabs[0].active_pane, 0);
    }

    #[test]
    fn v4_split_tree_and_launch_round_trip() {
        let mut tab = TabSession::single("D:/work".into(), None, None);
        tab.launch = Some(LaunchSession::Ssh { host: "root@203.0.113.7".into() });
        tab.layout = Some(LayoutSession::Split {
            axis: SplitAxis::LeftRight,
            ratio_permille: 618,
            first: Box::new(LayoutSession::Pane { cwd: "D:/work".into() }),
            second: Box::new(LayoutSession::Split {
                axis: SplitAxis::TopBottom,
                ratio_permille: 500,
                first: Box::new(LayoutSession::Pane { cwd: "D:/logs".into() }),
                second: Box::new(LayoutSession::Pane { cwd: String::new() }),
            }),
        });
        tab.active_pane = 2;
        let session = Session::new(0, vec![tab]);

        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored = parse(&json).expect("v4 must parse");
        assert_eq!(restored, session);
        assert_eq!(restored.tabs[0].layout.as_ref().unwrap().pane_count(), 3);
    }

    #[test]
    fn workspace_files_round_trip_through_save_to_and_load_from() {
        let dir = std::env::temp_dir().join("nebula-session-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.nebula-workspace.json");

        let mut tab = TabSession::single("D:/work".into(), Some("Main".into()), None);
        tab.launch = Some(LaunchSession::Shell {
            name: "Debian (WSL)".into(),
            program: "wsl.exe".into(),
            args: vec!["-d".into(), "Debian".into()],
        });
        let session = Session::new(0, vec![tab]);

        save_to(&path, &session).expect("save_to must succeed");
        let restored = load_from(&path).expect("load_from must parse");
        assert_eq!(restored, session);
        let _ = std::fs::remove_file(&path);
    }
}
