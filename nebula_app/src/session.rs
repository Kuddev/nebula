//! Session restore: reopen with the same tabs (and their directories) you had
//! when the window closed, with no "restore?" dialog.
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
    Shell {
        name: String,
        program: String,
        args: Vec<String>,
    },
    /// A quick-launch profile, embedded rather than referenced by name.
    Profile {
        name: String,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        #[serde(default)]
        shell_id: Option<String>,
    },
    /// A saved SSH destination; restoring reconnects automatically.
    Ssh {
        host: String,
    },
}

/// Split axis, mirrored from `display::SplitDirection` so the display layer
/// stays serde-free.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    LeftRight,
    TopBottom,
}

/// 快照瞬间仍在一个 pane 前台运行的 AI CLI 对话。冷恢复用它接续会话：
/// 身份来自 hook 直报（claude `session_id` / codex `thread-id`），缺 id 的
/// claude 退化成 `--continue`（按 cwd 找最近对话，恰好匹配恢复语义）。
///
/// 只存「安全启动描述」——来源名 + id，不存正文、不存启动参数原文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSession {
    /// CLI identity as the hook reports it (`claude` / `codex`).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl AgentSession {
    /// 恢复时敲进 shell 的命令行；无法安全构造（未知来源、异形 id、codex
    /// 缺 id）时返回 `None`，宁可只恢复布局也不上屏可疑字节。命令形状与
    /// `ai_sessions::AiSession::resume_command`（手动恢复面板）保持一致。
    pub fn resume_command(&self) -> Option<String> {
        let id = match self.session_id.as_deref() {
            Some(id) if valid_session_id(id) => Some(id),
            // 有 id 但形状可疑：绝不把它敲进终端。
            Some(_) => return None,
            None => None,
        };
        match (self.source.as_str(), id) {
            ("claude", Some(id)) => Some(format!("claude --resume {id}")),
            // claude 无 id（hook 未装、OSC 认出的启动）：`--continue` 恢复
            // 当前目录最近一次对话——pane 的 cwd 已经先被恢复，语义正好。
            ("claude", None) => Some("claude --continue".to_owned()),
            ("codex", Some(id)) => Some(format!("codex resume {id}")),
            // codex 没有按目录 continue 的形式，缺 id 只能放弃接续。
            _ => None,
        }
    }
}

/// 会话 id 只可能是 uuid 一族的字符集；其余一律拒绝，这行字符串会被敲进
/// 用户的 shell，必须按不可信输入对待。
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A tab's pane tree. Leaves carry each pane's working directory; splits carry
/// the axis and the first child's share in permille — an integer, so autosave
/// change detection and file diffs never trip on f32 serialization noise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutSession {
    Pane {
        cwd: String,
        /// v4 的追加可选字段（老文件缺省、老版本忽略，无需升版）：快照时
        /// 该 pane 前台的 AI CLI 对话，冷恢复据此自动接续。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<AgentSession>,
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
            Self::Pane { cwd, .. } => cwd,
            Self::Split { first, .. } => first.first_cwd(),
        }
    }

    /// Depth-first leaves — the SAME order `rebuild_layout` spawns panes in,
    /// so index i here pairs with the i-th live leaf after a restore.
    pub fn leaves(&self) -> Vec<&LayoutSession> {
        fn walk<'tree>(node: &'tree LayoutSession, out: &mut Vec<&'tree LayoutSession>) {
            match node {
                LayoutSession::Pane { .. } => out.push(node),
                LayoutSession::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                },
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
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
    /// 上一次退出有没有走完收尾。1 Hz 自动保存一律写 `false`，只有真正的
    /// 收尾路径（关窗、退出、detach 驻留）在最后一笔写 `true`——所以启动时
    /// 读到 `false` 就说明上次是崩溃、强杀或断电。
    ///
    /// 老版本文件没有这个字段，`serde(default)` 会给 `false`：一次性地把
    /// 升级前的最后一次会话判成异常退出。恢复行为完全不变（两种情况都恢复），
    /// 代价只是多一条提示，所以不值得为它单开一个版本号。
    #[serde(default)]
    pub clean_exit: bool,
}

impl Session {
    pub fn new(active_tab: usize, tabs: Vec<TabSession>) -> Self {
        Self {
            version: VERSION,
            boot_attempts: 0,
            active_tab,
            tabs,
            window: None,
            clean_exit: false,
        }
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

/// 收尾时的最后一笔快照：先打上 [`Session::clean_exit`] 再落盘。所有正常
/// 退出路径都必须走这里，否则下次启动会把这次退出误判成崩溃。
pub fn save_final(session: &mut Session) {
    session.clean_exit = true;
    save(session);
}

/// 上次是异常退出（崩溃 / 强杀 / 断电），而不是正常收尾。空会话不算——
/// 那是一路关标签关干净的正常退出。
pub fn was_crash(session: &Session) -> bool {
    !session.clean_exit && !session.tabs.is_empty()
}

/// 断路器跳闸（连续 [`MAX_BOOT_ATTEMPTS`] 次启动都没活到第一次自动保存）时，
/// 把这份会话挪到 `session.crashed.json` 再让本次启动走干净路径。
///
/// 必须**挪走**而不是留在原地：启动一成功，一秒后的自动保存就会把
/// `session.json` 盖掉，那份「一恢复就崩」的现场是唯一的诊断材料。顺带
/// 也让 `boot_attempts` 自然归零，用户不必手工删文件才能恢复正常。
pub fn quarantine() -> Option<PathBuf> {
    let from = session_path();
    let to = crate::display::nebula_data_dir().join("session.crashed.json");
    std::fs::copy(&from, &to).ok()?;
    let _ = std::fs::remove_file(&from);
    Some(to)
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

    /// 崩溃判定的三种现场：自动保存写的半路快照、正常收尾、以及一路关标签
    /// 关到空——只有第一种算异常退出。
    #[test]
    fn only_an_unfinished_teardown_counts_as_a_crash() {
        let mut running = Session::new(0, vec![TabSession::single("D:/work".into(), None, None)]);
        assert!(was_crash(&running), "1 Hz 自动保存写的快照 = 还没走到收尾");
        running.clean_exit = true;
        assert!(!was_crash(&running));
        let quit_clean = Session::new(0, vec![]);
        assert!(!was_crash(&quit_clean), "空会话是正常退出，不是崩溃");
    }

    /// 升级前的会话文件没有 `clean_exit`，读出来必须是 false（当作异常退出）
    /// 而不是解析失败——恢复行为不变，只多一条提示。
    #[test]
    fn a_file_written_before_clean_exit_existed_still_parses() {
        let json = r#"{"version":4,"boot_attempts":0,"active_tab":0,"tabs":[{"cwd":"D:/work"}]}"#;
        let session: Session = serde_json::from_str(json).unwrap();
        assert!(!session.clean_exit);
        assert!(should_restore(&session));
    }

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
            first: Box::new(LayoutSession::Pane {
                cwd: "D:/work".into(),
                agent: Some(AgentSession {
                    source: "claude".into(),
                    session_id: Some("0199a213-c2a4-7cf5-8f6b-d746fbb6e86c".into()),
                }),
            }),
            second: Box::new(LayoutSession::Split {
                axis: SplitAxis::TopBottom,
                ratio_permille: 500,
                first: Box::new(LayoutSession::Pane { cwd: "D:/logs".into(), agent: None }),
                second: Box::new(LayoutSession::Pane { cwd: String::new(), agent: None }),
            }),
        });
        tab.active_pane = 2;
        let session = Session::new(0, vec![tab]);

        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored = parse(&json).expect("v4 must parse");
        assert_eq!(restored, session);
        assert_eq!(restored.tabs[0].layout.as_ref().unwrap().pane_count(), 3);
    }

    /// agent 是 v4 的追加可选字段：带 agent 的树往返不丢；没有这个字段的
    /// 旧 v4 文件照常解析（缺省 None），两个方向都不需要升版。
    #[test]
    fn pane_agent_field_is_additive_on_v4() {
        let json = r#"{"version":4,"boot_attempts":0,"active_tab":0,"tabs":[{"cwd":"D:/w","layout":{"kind":"pane","cwd":"D:/w"}}]}"#;
        let session = parse(json).expect("v4 without agent must parse");
        assert_eq!(
            session.tabs[0].layout,
            Some(LayoutSession::Pane { cwd: "D:/w".into(), agent: None })
        );

        let with_agent = LayoutSession::Pane {
            cwd: "D:/w".into(),
            agent: Some(AgentSession { source: "codex".into(), session_id: Some("abc-1".into()) }),
        };
        let json = serde_json::to_string(&with_agent).unwrap();
        assert_eq!(serde_json::from_str::<LayoutSession>(&json).unwrap(), with_agent);
    }

    /// resume 命令是要敲进用户 shell 的字节：形状必须与手动恢复面板一致，
    /// 异形 id 一律拒绝，未知来源与缺 id 的 codex 放弃接续。
    #[test]
    fn agent_resume_commands_are_exact_and_injection_safe() {
        let agent = |source: &str, id: Option<&str>| AgentSession {
            source: source.into(),
            session_id: id.map(str::to_owned),
        };
        assert_eq!(
            agent("claude", Some("abc-123")).resume_command().as_deref(),
            Some("claude --resume abc-123")
        );
        assert_eq!(
            agent("claude", None).resume_command().as_deref(),
            Some("claude --continue")
        );
        assert_eq!(
            agent("codex", Some("b5f6c1c2-1111-2222-3333-444455556666")).resume_command().as_deref(),
            Some("codex resume b5f6c1c2-1111-2222-3333-444455556666")
        );
        assert_eq!(agent("codex", None).resume_command(), None);
        assert_eq!(agent("gemini", Some("abc")).resume_command(), None, "无 resume 语法的来源");
        // shell 元字符、空串、超长——全都不许上屏。
        assert_eq!(agent("claude", Some("abc; rm -rf /")).resume_command(), None);
        assert_eq!(agent("claude", Some("")).resume_command(), None);
        assert_eq!(agent("claude", Some(&"a".repeat(65))).resume_command(), None);
    }

    /// leaves 的 DFS 序必须与 pane_count/重建顺序一致——恢复注入靠索引配对。
    #[test]
    fn leaves_walk_depth_first_matching_rebuild_order() {
        let tree = LayoutSession::Split {
            axis: SplitAxis::LeftRight,
            ratio_permille: 500,
            first: Box::new(LayoutSession::Pane { cwd: "a".into(), agent: None }),
            second: Box::new(LayoutSession::Split {
                axis: SplitAxis::TopBottom,
                ratio_permille: 500,
                first: Box::new(LayoutSession::Pane { cwd: "b".into(), agent: None }),
                second: Box::new(LayoutSession::Pane { cwd: "c".into(), agent: None }),
            }),
        };
        let leaves = tree.leaves();
        assert_eq!(leaves.len(), tree.pane_count());
        let cwds: Vec<_> = leaves
            .iter()
            .map(|leaf| match leaf {
                LayoutSession::Pane { cwd, .. } => cwd.as_str(),
                LayoutSession::Split { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(cwds, ["a", "b", "c"]);
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
