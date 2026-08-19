//! Right-side drawer: directory tree / git status for the focused pane's cwd
//! This module owns only the *model* — tree flattening, git
//! parsing, layout maths, and hit-testing. Rendering lives in `display::mod`
//! (mirroring the command palette split), and input dispatch in `input::mod`.
//!
//! The panel is an overlay drawer: it floats above the terminal's right edge
//! instead of reflowing the PTY, so toggling it never resizes the shell.
//!
//! Refresh model: cheap and synchronous, but *only* on toggle, on a cwd/root
//! change, or when the throttle window (a few seconds) has elapsed — never on
//! every frame. `git --no-optional-locks` keeps the status call from touching
//! the index lock, so it can't corrupt or stall a concurrent git operation.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// Which view the drawer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelView {
    /// Directory tree of the focused pane's cwd.
    Files,
    /// Git branch + working-tree changes of the enclosing repository.
    Git,
}

/// One flattened row of the directory tree.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// 用于切换到上级根目录的合成 `..` 行。必须显式区分，避免普通目录的
    /// 展开、拖拽和双击逻辑把导航项误认为真实文件系统条目。
    pub is_parent: bool,
    /// Git-ignored entries remain in the normal tree order, but render with
    /// subdued ink so generated/build output recedes from source files.
    pub ignored: bool,
}

/// Which VCS produced a [`GitInfo`] snapshot. SVN reuses the same carrier so
/// both shells' thin views render one shape; semantic gaps (no staging area,
/// no push) are encoded where the operations dispatch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VcsKind {
    #[default]
    Git,
    /// 可执行 add/commit/update 等工作副本命令。
    Svn,
    /// `svnadmin create` 生成的服务端仓库；只能浏览或检出。
    SvnRepository,
}

/// Parsed `git status` snapshot.
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    /// Which VCS this snapshot came from（SVN 时 `branch` 放 `rNNN` 修订号，
    /// `staged` 恒空、`ahead` 恒 0——SVN 集中式，提交即发布）。
    pub vcs: VcsKind,
    /// Current branch (or short detached-HEAD description).
    pub branch: String,
    /// Working-tree line insertions/deletions (unstaged + staged).
    pub plus: u64,
    pub minus: u64,
    /// Commits ahead of upstream — what a push would publish. 0 = nothing to
    /// push (the push button keys off this: only committed work is pushable).
    pub ahead: u32,
    /// Worktree changes not yet staged (`??` counts here as `?`).
    pub unstaged: Vec<(char, String)>,
    /// Index changes ready to commit.
    pub staged: Vec<(char, String)>,
    /// 合并冲突（porcelain 的 U*/AA/DD；SVN 的 `C`）。冲突路径**同时**保留
    /// 在 `staged`/`unstaged` 里：旧壳视图零改动照常显示，GPUI 壳按本列表
    /// 单独分组并从另两组过滤（VS Code 的 Merge Changes 合同）。
    pub conflicts: Vec<(char, String)>,
    /// 仅 [`VcsKind::SvnRepository`] 使用；工作副本和 Git 的操作目录取
    /// [`SidePanel::vcs_root`]，服务端仓库必须保留祖先扫描得到的真实根。
    pub repository_root: Option<PathBuf>,
}

impl GitInfo {
    pub fn svn_add_ready(&self) -> bool {
        self.vcs == VcsKind::Svn && self.unstaged.iter().any(|(status, _)| *status == '?')
    }

    pub fn svn_commit_ready(&self) -> bool {
        self.vcs == VcsKind::Svn
            && self.unstaged.iter().any(|(status, _)| matches!(status, 'A' | 'D' | 'M' | 'R'))
    }
}

/// Result of hit-testing a pixel against the open drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelHit {
    None,
    /// The "文件" view tab in the header.
    ViewFiles,
    /// The "Git" view tab in the header.
    ViewGit,
    /// The Files view's filter input box.
    Search,
    /// Choose a window-local tree root with the native folder picker.
    OpenDirectory,
    /// Reveal the current tree root in the platform file manager.
    RevealDirectory,
    /// Open a fresh terminal tab whose PTY starts at the current tree root.
    NewTerminalHere,
    /// Clear the window-local root and resume following the focused pane cwd.
    FollowCurrentDirectory,
    /// A list row (index into the *visible* rows of the current view).
    Row(usize),
    /// Inside the panel but on nothing interactive.
    Inside,
}

/// An in-progress drag of a tree entry toward the terminal (drop = paste the
/// full path into the shell, like dropping an entry from Explorer).
#[derive(Debug, Clone)]
pub struct FileDrag {
    pub path: PathBuf,
    /// Display name for the drag ghost that follows the pointer.
    pub name: String,
    /// Pointer position at press; the drag activates past a small threshold
    /// so plain clicks (and double-clicks) don't count as drags.
    pub origin: (f32, f32),
    /// Latest pointer position (physical px) — anchors the drag ghost.
    pub pos: (f32, f32),
    /// Directories defer their normal expand/collapse click until release, so
    /// crossing the drag threshold never mutates the tree as a side effect.
    pub is_dir: bool,
    /// Visible row at press time. Release validates its path before toggling,
    /// preventing a throttled tree refresh from acting on a different row.
    pub source_row: usize,
    pub active: bool,
}

impl FileDrag {
    pub fn new(
        path: PathBuf,
        name: String,
        is_dir: bool,
        source_row: usize,
        origin: (f32, f32),
    ) -> Self {
        Self { path, name, origin, pos: origin, is_dir, source_row, active: false }
    }

    /// Update the ghost position and cross the small click/drag threshold.
    /// Once active, a drag never falls back to an expand/collapse click.
    pub fn update_position(&mut self, pos: (f32, f32)) {
        self.pos = pos;
        if !self.active {
            let (ox, oy) = self.origin;
            if (pos.0 - ox).abs() >= 8.0 || (pos.1 - oy).abs() >= 8.0 {
                self.active = true;
            }
        }
    }

    /// Bytes pasted on a valid terminal drop. This intentionally preserves
    /// the existing cross-shell compatibility boundary: whitespace paths use
    /// double quotes, but no single literal syntax can safely encode every
    /// special character for PowerShell, CMD, Bash and NuShell at once.
    pub fn terminal_drop_text(&self, over_terminal: bool) -> Option<Vec<u8>> {
        if !self.active || !over_terminal {
            return None;
        }
        let mut text = self.path.display().to_string();
        // Unix permits control characters (including CR/LF) in file names.
        // Sending those bytes to a PTY could execute input despite the drop
        // contract explicitly requiring paste-only behaviour.
        if text.chars().any(char::is_control) {
            return None;
        }
        if text.contains(char::is_whitespace) {
            text = format!("\"{text}\"");
        }
        text.push(' ');
        Some(text.into_bytes())
    }
}

/// Re-run the (throttled) refresh at most this often while the panel is open.
const REFRESH_EVERY: Duration = Duration::from_secs(4);
/// Hard cap on flattened tree rows, bounding both fs walking and rendering.
const MAX_ROWS: usize = 1000;
/// Hard cap on entries listed per directory. Applied *after* sorting, so the
/// cap keeps the alphabetical head of the directory rather than whatever
/// `read_dir` happened to yield first.
const MAX_PER_DIR: usize = 600;
/// Total directory entries the filter index may VISIT while being built.
/// This bounds the walk itself (a `target/` or `node_modules/` tree has
/// hundreds of thousands of entries — walking it per keystroke froze the UI),
/// not just the matches kept.
const SEARCH_VISIT_BUDGET: usize = 20_000;
/// Entries kept in the filter index.
const SEARCH_INDEX_CAP: usize = 10_000;
/// Directories that are all bulk and no signal — never indexed for filtering.
const SEARCH_SKIP_DIRS: &[&str] =
    &["target", "node_modules", ".git", ".cache", ".gradle", "build", "trellis"];

pub struct SidePanel {
    pub open: bool,
    pub view: PanelView,
    /// Root the tree/git snapshot was built from (the focused pane's cwd).
    root: Option<PathBuf>,
    /// Latest focused pane cwd, retained while a custom root is active so the
    /// panel can resume following immediately without persisting any setting.
    followed_cwd: Option<PathBuf>,
    /// Window-local override selected from the Files view.
    custom_root: Option<PathBuf>,
    /// Visible feedback for an invalid/disappeared custom root.
    root_notice: Option<String>,
    /// Flattened visible tree rows for the Files view.
    rows: Vec<FileRow>,
    /// Directories the user expanded (persists across refreshes).
    expanded: HashSet<PathBuf>,
    /// Git snapshot, `None` when the root isn't inside a work tree.
    git: Option<GitInfo>,
    /// Scroll offset in rows.
    pub scroll: usize,
    /// Files-view filter query; non-empty switches the tree to a flat list of
    /// deep matches, matching the tree filter's flat-result behavior.
    pub search: String,
    /// Whether the filter box owns the keyboard.
    pub search_focus: bool,
    search_selection: super::text_input::SelectAllState,
    /// Flat, budget-bounded index of the tree used by the filter. Built ONCE
    /// on the first filtering keystroke and reused for the rest of the query
    /// (each keystroke then only string-matches in memory); dropped whenever
    /// the root changes or a refresh rebuilds the snapshot.
    search_index: Option<Vec<FileRow>>,
    /// Commit-message input (Git view): buffer + focus, same modal keyboard
    /// contract as the Files filter box.
    pub commit_msg: String,
    pub commit_focus: bool,
    commit_selection: super::text_input::SelectAllState,
    /// Last clicked file row (path + when), for double-click-to-open.
    pub last_file_click: Option<(PathBuf, Instant)>,
    /// In-progress drag of a file or directory row toward the terminal.
    pub drag_file: Option<FileDrag>,
    /// Persistently selected file (row highlight). Cleared by clicking off
    /// the panel, closing the drawer, or the root changing.
    pub selected: Option<PathBuf>,
    /// What the pointer currently hovers (rows/buttons/header tabs light up).
    pub hover: PanelHit,
    /// Pointer position of the last hover update — disambiguates WHICH git
    /// action button is under the pointer inside the shared strip.
    pub hover_pos: (f32, f32),
    /// A git mutation (add/commit/push) is running on a worker thread; the
    /// action buttons gray out and re-arm when it lands.
    op_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by the worker when it finishes — `sync` folds it into a refresh.
    op_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Last operation's error (empty = success), shown on the summary line.
    op_error: std::sync::Arc<std::sync::Mutex<String>>,
    /// A snapshot worker (fs walk + git subprocesses) is in flight. Guards
    /// against stacking workers; a refresh requested meanwhile re-arms
    /// `needs_refresh` and runs after this one lands.
    snapshot_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The worker's finished snapshot, harvested by `sync` on the next frame.
    /// 切换视图/根不再同步跑 git——旧内容原样留在屏上，新快照落地后整体
    /// 替换（VSCode 的树刷新模式）。
    snapshot_slot: std::sync::Arc<std::sync::Mutex<Option<PanelSnapshot>>>,
    last_refresh: Option<Instant>,
    needs_refresh: bool,
}

/// What the background snapshot worker produces: everything `refresh` used to
/// compute synchronously on the render thread.
struct PanelSnapshot {
    /// Root the snapshot was built from — stale snapshots (root changed while
    /// the worker ran) are dropped on harvest.
    root: PathBuf,
    rows: Vec<FileRow>,
    git: Option<GitInfo>,
}

fn git_pull_args() -> Vec<String> {
    vec!["pull".into(), "--ff-only".into()]
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SvnMutation {
    Add(Vec<PathBuf>),
    Commit(String),
    Update,
    Revert(PathBuf),
    Resolve(PathBuf),
    Cleanup,
}

impl SvnMutation {
    fn cli_args(&self) -> Vec<OsString> {
        let args: Vec<OsString> = match self {
            Self::Add(paths) => {
                let mut args = vec!["add".into(), "--parents".into(), "--".into()];
                args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
                args
            },
            Self::Commit(message) => vec![
                "commit".into(),
                "--non-interactive".into(),
                "-m".into(),
                message.as_str().into(),
            ],
            Self::Update => vec!["update".into(), "--non-interactive".into()],
            Self::Revert(path) => vec![
                "revert".into(),
                "--depth".into(),
                "infinity".into(),
                "--".into(),
                path.as_os_str().to_owned(),
            ],
            // `working` 保留用户解决后的文件内容，只把冲突状态标记为已处理。
            Self::Resolve(path) => vec![
                "resolve".into(),
                "--accept".into(),
                "working".into(),
                "--".into(),
                path.as_os_str().to_owned(),
            ],
            Self::Cleanup => vec!["cleanup".into()],
        };
        args
    }

    fn tortoise_args(&self, working_dir: &Path) -> Vec<OsString> {
        let command = |name: &str| OsString::from(format!("/command:{name}"));
        let path_arg = |path: &Path| OsString::from(format!("/path:{}", path.display()));
        match self {
            Self::Add(paths) => vec![
                command("add"),
                OsString::from(format!(
                    "/path:{}",
                    paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join("*")
                )),
            ],
            Self::Commit(message) => vec![
                command("commit"),
                path_arg(working_dir),
                OsString::from(format!("/logmsg:{message}")),
            ],
            Self::Update => vec![command("update"), path_arg(working_dir)],
            Self::Revert(path) => vec![command("revert"), path_arg(path)],
            Self::Resolve(path) => vec![command("resolve"), path_arg(path)],
            // TortoiseProc 的 cleanup 对话框需要额外 `/cleanup` 才勾选基础清理。
            Self::Cleanup => vec![command("cleanup"), path_arg(working_dir), "/cleanup".into()],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SvnVisual {
    Log(PathBuf),
    Diff(PathBuf),
    BrowseRepository(PathBuf),
    CheckoutRepository(PathBuf),
}

impl SvnVisual {
    fn tortoise_args(&self) -> Vec<OsString> {
        let path_arg = |path: &Path| OsString::from(format!("/path:{}", path.display()));
        match self {
            Self::Log(path) => vec!["/command:log".into(), path_arg(path)],
            Self::Diff(path) => vec!["/command:diff".into(), path_arg(path)],
            Self::BrowseRepository(path) => vec![
                "/command:repobrowser".into(),
                OsString::from(format!("/path:{}", local_repository_url(path))),
            ],
            // 不传 `/path`，让 TortoiseSVN 的检出窗口由用户选择工作副本目录。
            Self::CheckoutRepository(path) => vec![
                "/command:checkout".into(),
                OsString::from(format!("/url:{}", local_repository_url(path))),
            ],
        }
    }
}

fn local_repository_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    if encoded.starts_with("//") {
        format!("file:{encoded}")
    } else if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn find_path_command(program: &str) -> Option<PathBuf> {
    let requested = PathBuf::from(program);
    if requested.components().count() > 1 {
        return requested.is_file().then_some(requested);
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let direct = directory.join(program);
        if direct.is_file() {
            return Some(direct);
        }
        #[cfg(windows)]
        {
            let executable = directory.join(format!("{program}.exe"));
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

#[cfg(windows)]
fn find_tortoise_proc() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    // 自定义安装盘不会出现在 ProgramFiles；注册表的 ProcPath 才是权威位置。
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for key_name in [r"SOFTWARE\TortoiseSVN", r"SOFTWARE\WOW6432Node\TortoiseSVN"] {
            let candidate = RegKey::predef(hive)
                .open_subkey(key_name)
                .and_then(|key| key.get_value::<String, _>("ProcPath"))
                .ok()
                .map(PathBuf::from);
            if candidate.as_ref().is_some_and(|path| path.is_file()) {
                return candidate;
            }
        }
    }
    if let Some(path) = find_path_command("TortoiseProc") {
        return Some(path);
    }
    for root in
        ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(|name| std::env::var_os(name))
    {
        for relative in [r"TortoiseSVN\bin\TortoiseProc.exe", r"SVN\bin\TortoiseProc.exe"] {
            let candidate = PathBuf::from(&root).join(relative);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn find_tortoise_proc() -> Option<PathBuf> {
    None
}

fn svn_relative_target(root: &Path, relative: &str) -> Option<PathBuf> {
    use std::path::Component;

    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(component, Component::Prefix(_) | Component::RootDir | Component::ParentDir)
        })
    {
        return None;
    }
    Some(root.join(relative))
}

impl SidePanel {
    pub fn new() -> Self {
        Self {
            open: false,
            view: PanelView::Files,
            root: None,
            followed_cwd: None,
            custom_root: None,
            root_notice: None,
            rows: Vec::new(),
            expanded: HashSet::new(),
            git: None,
            scroll: 0,
            search: String::new(),
            search_focus: false,
            search_selection: Default::default(),
            search_index: None,
            commit_msg: String::new(),
            commit_focus: false,
            commit_selection: Default::default(),
            last_file_click: None,
            drag_file: None,
            selected: None,
            hover: PanelHit::None,
            hover_pos: (0.0, 0.0),
            op_running: Default::default(),
            op_done: Default::default(),
            op_error: Default::default(),
            snapshot_running: Default::default(),
            snapshot_slot: Default::default(),
            last_refresh: None,
            needs_refresh: false,
        }
    }

    /// Toggle the drawer. Re-invoking with the *other* view while open only
    /// switches views instead of closing the drawer.
    pub fn toggle(&mut self, view: PanelView) {
        if self.open && self.view == view {
            self.open = false;
            self.selected = None;
            self.drag_file = None;
            return;
        }
        self.open = true;
        self.view = view;
        self.scroll = 0;
        self.needs_refresh = true;
    }

    /// Adopt the focused pane's cwd, refreshing when the root changed, a
    /// refresh was requested (toggle), or the throttle window has elapsed.
    /// Called once per drawn frame from the window context; cheap when nothing
    /// changed. Returns whether the snapshot was rebuilt (i.e. needs redraw).
    pub fn sync(&mut self, cwd: Option<PathBuf>) -> bool {
        if !self.open {
            return false;
        }
        // 先收割落地的后台快照——旧内容在工人跑动期间一直显示，这里一次
        // 性换成新内容（先显示旧的、再更新，VSCode 的树刷新模式）。
        let mut changed = self.harvest_snapshot();
        // 聚焦 pane 报不出本地有效目录时（SSH 远端路径、未映射的 WSL 路
        // 径、shell 还没发 OSC）保持现状：跟随的语义是"最后一个已知有效
        // 目录"，清空只会让切到远程 tab 的瞬间目录树闪成空白。
        if cwd.is_some() {
            self.followed_cwd = cwd;
        }
        let custom_invalidated = self.custom_root.as_ref().is_some_and(|root| !root.is_dir());
        if custom_invalidated {
            self.custom_root = None;
            self.root_notice = Some("所选目录不可用，已跟随当前目录".to_owned());
        }
        let next_root = self.custom_root.clone().or_else(|| self.followed_cwd.clone());
        let root_changed = next_root != self.root;
        // While a filter query is live, skip the periodic re-snapshot: it
        // would drop and rebuild the search index under the user's fingers.
        let stale = self.search.trim().is_empty()
            && self.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        // A finished git mutation forces a refresh so the new state (staged
        // list, ahead count) shows on the next frame.
        if self.op_done.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.needs_refresh = true;
        }
        if !(root_changed || custom_invalidated || stale || self.needs_refresh) {
            return changed;
        }
        if root_changed {
            self.root = next_root;
            self.expanded.clear();
            self.scroll = 0;
            self.selected = None;
        }
        self.refresh();
        changed || root_changed || custom_invalidated
    }

    /// 测试辅助：等后台快照工人落地并收割。生产路径永不阻塞。
    #[cfg(test)]
    fn wait_snapshot(&mut self) {
        for _ in 0..1000 {
            if !self.snapshot_running.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.harvest_snapshot();
    }

    /// Fold a finished background snapshot into the visible state. Returns
    /// whether anything on screen changed.
    fn harvest_snapshot(&mut self) -> bool {
        // 过滤查询激活时不收割：树快照会覆盖过滤视图。快照留在槽里，查询
        // 清空后的下一帧照常落地。
        if !self.search.trim().is_empty() {
            return false;
        }
        let snapshot = match self.snapshot_slot.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        let Some(snapshot) = snapshot else { return false };
        // 工人跑动期间根又变了：这份快照已经过期，丢弃。新刷新在路上。
        if self.root.as_ref() != Some(&snapshot.root) {
            return false;
        }
        self.rows = snapshot.rows;
        self.git = snapshot.git;
        // New snapshot → the filter index is stale; rebuild lazily on demand.
        self.search_index = None;
        true
    }

    /// Override the focused pane cwd for this panel instance only. `SidePanel`
    /// belongs to one window and this field is deliberately never serialized.
    pub fn set_custom_root(&mut self, root: PathBuf) -> bool {
        if !root.is_dir() {
            self.root_notice = Some("所选目录不可用".to_owned());
            return false;
        }
        let changed = self.custom_root.as_ref() != Some(&root) || self.root.as_ref() != Some(&root);
        self.custom_root = Some(root.clone());
        self.root_notice = None;
        if self.root.as_ref() != Some(&root) {
            self.root = Some(root);
            self.expanded.clear();
            self.scroll = 0;
            self.selected = None;
            self.refresh();
        }
        changed
    }

    /// Resume following the most recently observed focused pane cwd.
    pub fn clear_custom_root(&mut self) -> bool {
        if self.custom_root.take().is_none() {
            return false;
        }
        self.root_notice = None;
        let next_root = self.followed_cwd.clone();
        if self.root != next_root {
            self.root = next_root;
            self.expanded.clear();
            self.scroll = 0;
            self.selected = None;
            self.refresh();
        }
        true
    }

    pub fn custom_root_active(&self) -> bool {
        self.custom_root.is_some()
    }

    /// 目录：Git/SVN 状态所属的那一个。始终是终端当前目录，与目录树的浏览
    /// 位置（`custom_root`）无关——`refresh` 就是按这个路径抓 git 的，路径摘要
    /// 必须显示同一个值，否则用户会看到「树在 A、状态是 B」的错位。
    pub fn vcs_root(&self) -> Option<&Path> {
        self.followed_cwd.as_deref().or(self.root.as_deref())
    }

    pub fn root_notice(&self) -> Option<&str> {
        self.root_notice.as_deref()
    }

    /// Only real Git file rows are interactive. Section headers and the blank
    /// area below the snapshot must never produce a full-width hover pill.
    pub fn git_row_is_file(&self, visible_row: usize) -> bool {
        if self.view != PanelView::Git {
            return false;
        }
        let Some(git) = self.git.as_ref() else { return false };
        let absolute = self.scroll + visible_row;
        if git.unstaged.is_empty() && git.staged.is_empty() {
            return false;
        }
        let unstaged = 1..1 + git.unstaged.len();
        let staged_start = git.unstaged.len() + 2;
        let staged = staged_start..staged_start + git.staged.len();
        unstaged.contains(&absolute) || staged.contains(&absolute)
    }

    /// 外部（右键菜单删除等文件系统变更）请求下一帧重建快照。
    pub fn request_refresh(&mut self) {
        self.needs_refresh = true;
    }

    /// 面板顶部的一句话提示（复用根目录不可用的同一条 UI）。
    pub fn set_notice(&mut self, message: String) {
        self.root_notice = Some(message);
    }

    /// Rebuild the tree and git snapshot from `root`.
    fn refresh(&mut self) {
        self.needs_refresh = false;
        self.last_refresh = Some(Instant::now());
        let Some(root) = self.root.clone() else {
            // 没有根：清空是即时且无成本的，不需要工人。
            self.rows.clear();
            self.git = None;
            self.search_index = None;
            return;
        };
        // 快照工人一次只跑一个：fs 遍历 + 三个 git 子进程在渲染线程上曾是
        // 切换视图卡顿的直接根因。工人在飞时再次请求就重挂 `needs_refresh`，
        // 落地后 `sync` 的下一帧自然补跑，不排队叠罗汉。
        if self.snapshot_running.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.needs_refresh = true;
            return;
        }
        let expanded = self.expanded.clone();
        // VCS 状态始终读**终端当前目录**，不跟着目录树的浏览位置走。在树里点
        // `..` 往上翻是纯浏览动作（`custom_root`，窗口内覆盖），把仓库状态一起
        // 带走的后果是：翻到仓库外面之后 Git 视图只剩「当前目录不在 Git/SVN
        // 仓库中」，而回头的入口偏偏只画在 Files 视图里。行与状态在这里解耦：
        // 行看 `root`，git 看 `followed_cwd`。
        let git_root = self.followed_cwd.clone().unwrap_or_else(|| root.clone());
        let running = std::sync::Arc::clone(&self.snapshot_running);
        let slot = std::sync::Arc::clone(&self.snapshot_slot);
        std::thread::spawn(move || {
            let rows = SidePanel::tree_rows(&root, &expanded);
            // 设置可强制只认 Git / SVN（混合仓库场景）；Auto 保持既有探测：
            // a checkout nested inside a Git tree must remain visible as SVN.
            // Prefer SVN only when its metadata is in the current path's
            // ancestor chain; ordinary Git directories keep the cheaper Git
            // first path and only probe SVN as a fallback.
            let git = match nebula_settings::RuntimeSettings::load().vcs_display {
                nebula_settings::VcsDisplayName::Git => read_git(&git_root),
                nebula_settings::VcsDisplayName::Svn => read_svn(&git_root),
                nebula_settings::VcsDisplayName::Auto => {
                    if svn_dir_hint(&git_root) {
                        read_svn(&git_root).or_else(|| read_git(&git_root))
                    } else {
                        read_git(&git_root).or_else(|| read_svn(&git_root))
                    }
                },
            };
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(PanelSnapshot { root, rows, git });
            }
            running.store(false, std::sync::atomic::Ordering::Release);
        });
    }

    /// Rebuild only the flattened rows (tree shape / filter changes; the git
    /// snapshot stays).
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(root) = self.root.clone() else { return };
        let needle = self.search.trim().to_lowercase();
        if needle.is_empty() {
            self.rows = Self::tree_rows(&root, &self.expanded);
            return;
        }
        // Filter mode: string-match against the cached flat index. The index
        // is built at most once per snapshot (budget-bounded walk); each
        // keystroke after that is pure in-memory filtering — walking the tree
        // per keystroke froze the UI on big checkouts.
        if self.search_index.is_none() {
            let mut index = Vec::new();
            let mut budget = SEARCH_VISIT_BUDGET;
            build_search_index(&root, 0, &mut index, &mut budget);
            Self::mark_ignored(&root, &mut index);
            self.search_index = Some(index);
        }
        let index = self.search_index.as_ref().unwrap();
        self.rows.extend(
            index
                .iter()
                .filter(|row| row.name.to_lowercase().contains(&needle))
                .take(MAX_ROWS)
                .cloned(),
        );
    }

    /// Append typed text to the filter query and re-derive the rows.
    pub fn search_input(&mut self, text: &str) {
        self.search_selection.insert(&mut self.search, text);
        self.scroll = 0;
        self.rebuild_rows();
    }

    pub fn search_backspace(&mut self) {
        self.search_selection.backspace(&mut self.search);
        self.scroll = 0;
        self.rebuild_rows();
    }

    pub fn search_select_all(&mut self) {
        self.search_selection.select(&self.search);
    }

    pub fn search_selected_text(&self) -> Option<String> {
        self.search_selection.selected_text(&self.search)
    }

    pub fn search_all_selected(&self) -> bool {
        self.search_selection.is_selected()
    }

    /// Leave the filter box; `clear` also resets the query (Esc).
    pub fn search_unfocus(&mut self, clear: bool) {
        self.search_focus = false;
        self.search_selection.clear();
        if clear && !self.search.is_empty() {
            self.search.clear();
            self.scroll = 0;
            self.rebuild_rows();
        }
    }

    // ---- git mutations (add / commit / pull / push) ----

    /// Whether a git mutation is in flight (buttons gray out).
    pub fn op_running(&self) -> bool {
        self.op_running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Last mutation's error, if any (cleared by the next successful op).
    pub fn op_error(&self) -> Option<String> {
        let e = self.op_error.lock().ok()?;
        (!e.is_empty()).then(|| e.clone())
    }

    fn set_op_error(&mut self, message: impl Into<String>) {
        if let Ok(mut error) = self.op_error.lock() {
            *error = message.into();
        }
    }

    /// Run `<program> <args>` on a worker thread; UI stays live (a push can
    /// take seconds over the network). Completion flips `op_done`, which the
    /// next drawn frame folds into a refresh.
    fn spawn_vcs_at(&mut self, program: PathBuf, args: Vec<OsString>, root: PathBuf) {
        use std::sync::atomic::Ordering;
        if self.op_running.swap(true, Ordering::Relaxed) {
            return; // one at a time
        }
        let running = self.op_running.clone();
        let done = self.op_done.clone();
        let error = self.op_error.clone();
        if let Ok(mut message) = error.lock() {
            message.clear();
        }
        let display_name = program.display().to_string();
        let spawn_result =
            std::thread::Builder::new().name("nebula-vcs-op".into()).spawn(move || {
                let mut cmd = std::process::Command::new(&program);
                cmd.args(&args).current_dir(&root);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                let msg = match cmd.output() {
                    Ok(out) if out.status.success() => String::new(),
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        // First meaningful line is enough for a status strip.
                        err.lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or(&format!("{display_name} 失败"))
                            .to_string()
                    },
                    Err(e) => format!("{display_name}: {e}"),
                };
                if let Ok(mut slot) = error.lock() {
                    *slot = msg;
                }
                running.store(false, Ordering::Relaxed);
                done.store(true, Ordering::Relaxed);
            });
        if let Err(spawn_error) = spawn_result {
            self.op_running.store(false, Ordering::Relaxed);
            self.set_op_error(format!("无法启动版本控制任务: {spawn_error}"));
        }
    }

    fn spawn_vcs(&mut self, program: impl Into<PathBuf>, args: Vec<OsString>) {
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        self.spawn_vcs_at(program.into(), args, root);
    }

    fn spawn_git(&mut self, args: Vec<String>) {
        self.spawn_vcs("git", args.into_iter().map(OsString::from).collect());
    }

    fn spawn_svn_mutation(&mut self, operation: SvnMutation) {
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        if let Some(svn) = find_path_command("svn") {
            self.spawn_vcs_at(svn, operation.cli_args(), root);
        } else if let Some(tortoise) = find_tortoise_proc() {
            self.spawn_vcs_at(tortoise, operation.tortoise_args(&root), root);
        } else {
            self.set_op_error("未找到 svn.exe 或 TortoiseSVN，无法执行 SVN 操作");
        }
    }

    fn launch_svn_visual(&mut self, visual: SvnVisual) -> bool {
        let Some(program) = find_tortoise_proc() else {
            self.set_op_error("此可视化操作需要 TortoiseSVN（TortoiseProc.exe）");
            return false;
        };
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        let mut command = std::process::Command::new(&program);
        command.args(visual.tortoise_args()).current_dir(root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW 不会隐藏 Tortoise GUI。
        }
        match command.spawn() {
            Ok(_) => {
                self.set_op_error(String::new());
                true
            },
            Err(error) => {
                self.set_op_error(format!("无法启动 {}: {error}", program.display()));
                false
            },
        }
    }

    /// 当前快照的 VCS 种类；None = 不在任何仓库里。
    pub fn vcs(&self) -> Option<VcsKind> {
        self.git.as_ref().map(|info| info.vcs)
    }

    /// `git add -A`: stage everything (the ⊕ button). SVN 没有暂存区，
    /// no-op（按钮层按 [`Self::vcs`] 直接不画）。
    pub fn git_stage_all(&mut self) {
        if self.vcs() != Some(VcsKind::Git) {
            return;
        }
        if self.git.as_ref().is_some_and(|g| !g.unstaged.is_empty()) && !self.op_running() {
            self.spawn_git(vec!["add".into(), "-A".into()]);
        }
    }

    /// `git add -- <path>`：单文件暂存（VS Code 行内 ＋ 的合同）。
    pub fn git_stage_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["add".into(), "--".into(), path.to_owned()]);
        }
    }

    /// `git restore --staged -- <path>`：单文件取消暂存（VS Code 行内 −）。
    pub fn git_unstage_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["restore".into(), "--staged".into(), "--".into(), path.to_owned()]);
        }
    }

    /// `git restore -- <path>`：丢弃工作区改动（调用方负责确认交互；
    /// untracked 文件不适用——restore 不删新文件，按钮层不对 `?` 提供）。
    pub fn git_discard_path(&mut self, path: &str) {
        if self.vcs() == Some(VcsKind::Git) && !self.op_running() {
            self.spawn_git(vec!["restore".into(), "--".into(), path.to_owned()]);
        }
    }

    /// Commit button: with staged changes, open the message input (Enter then
    /// commits via [`Self::git_commit_submit`]).
    pub fn git_begin_commit(&mut self) {
        if self.git.as_ref().is_some_and(|g| !g.staged.is_empty()) && !self.op_running() {
            self.commit_focus = true;
            self.commit_selection.clear();
        }
    }

    pub fn commit_input(&mut self, text: &str) {
        self.commit_selection.insert(&mut self.commit_msg, text);
    }

    pub fn commit_backspace(&mut self) {
        self.commit_selection.backspace(&mut self.commit_msg);
    }

    pub fn commit_select_all(&mut self) {
        self.commit_selection.select(&self.commit_msg);
    }

    pub fn commit_selected_text(&self) -> Option<String> {
        self.commit_selection.selected_text(&self.commit_msg)
    }

    pub fn commit_all_selected(&self) -> bool {
        self.commit_selection.is_selected()
    }

    pub fn commit_cancel(&mut self) {
        self.commit_focus = false;
        self.commit_msg.clear();
        self.commit_selection.clear();
    }

    pub fn commit_unfocus(&mut self) {
        self.commit_focus = false;
        self.commit_selection.clear();
    }

    /// Enter in the message box: run `git commit -m <msg>`.
    pub fn git_commit_submit(&mut self) {
        let msg = self.commit_msg.trim().to_string();
        if msg.is_empty() || self.op_running() {
            return;
        }
        self.commit_focus = false;
        self.commit_msg.clear();
        self.commit_selection.clear();
        self.vcs_commit_message(&msg);
    }

    /// 直接以给定消息提交（GPUI 壳的输入组件走这里，不经旧壳的内部输入
    /// 状态机）。按 VCS 分派：git 提交暂存区；svn 没有暂存区，提交整个
    /// 工作副本的修改。
    pub fn vcs_commit_message(&mut self, message: &str) {
        let message = message.trim();
        if message.is_empty() || self.op_running() {
            return;
        }
        match self.vcs() {
            Some(VcsKind::Git) => {
                self.spawn_git(vec!["commit".into(), "-m".into(), message.to_owned()]);
            },
            Some(VcsKind::Svn) => {
                self.spawn_svn_mutation(SvnMutation::Commit(message.to_owned()));
            },
            Some(VcsKind::SvnRepository) | None => {},
        }
    }

    /// Push button — only enabled with committed-but-unpushed work (`ahead`).
    /// SVN 的 `ahead` 恒 0（提交即发布），按钮自然不亮。
    pub fn git_push(&mut self) {
        if self.git.as_ref().is_some_and(|g| g.ahead > 0) && !self.op_running() {
            self.spawn_git(vec!["push".into()]);
        }
    }

    /// Pull only fast-forward updates, never creating an implicit merge commit.
    /// SVN 对应 `svn update`。
    pub fn git_pull(&mut self) {
        if self.op_running() {
            return;
        }
        match self.vcs() {
            Some(VcsKind::Git) => self.spawn_git(git_pull_args()),
            Some(VcsKind::Svn) => self.spawn_svn_mutation(SvnMutation::Update),
            Some(VcsKind::SvnRepository) | None => {},
        }
    }

    /// SVN 的“添加”只接纳 `?` 未版本化项，不引入 Git 暂存区语义。
    pub fn svn_add_all(&mut self) {
        if self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let paths: Vec<PathBuf> = self
            .git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::Svn)
            .into_iter()
            .flat_map(|info| info.unstaged.iter())
            .filter(|(status, _)| *status == '?')
            .filter_map(|(_, path)| svn_relative_target(&root, path))
            .collect();
        if !paths.is_empty() {
            self.spawn_svn_mutation(SvnMutation::Add(paths));
        }
    }

    pub fn svn_add_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Add(vec![path]));
    }

    pub fn svn_revert_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Revert(path));
    }

    pub fn svn_resolve_path(&mut self, path: &str) {
        if self.vcs() != Some(VcsKind::Svn) || self.op_running() {
            return;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return };
        let Some(path) = svn_relative_target(&root, path) else { return };
        self.spawn_svn_mutation(SvnMutation::Resolve(path));
    }

    pub fn svn_cleanup(&mut self) {
        if self.vcs() == Some(VcsKind::Svn) && !self.op_running() {
            self.spawn_svn_mutation(SvnMutation::Cleanup);
        }
    }

    pub fn svn_log(&mut self) {
        if self.vcs() == Some(VcsKind::Svn) {
            if let Some(root) = self.vcs_root().map(Path::to_path_buf) {
                self.launch_svn_visual(SvnVisual::Log(root));
            }
        }
    }

    pub fn svn_diff_path(&mut self, path: &str) -> bool {
        if self.vcs() != Some(VcsKind::Svn) {
            return false;
        }
        let Some(root) = self.vcs_root().map(Path::to_path_buf) else { return false };
        let Some(path) = svn_relative_target(&root, path) else { return false };
        self.launch_svn_visual(SvnVisual::Diff(path))
    }

    pub fn svn_browse_repository(&mut self) {
        let root = self
            .git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::SvnRepository)
            .and_then(|info| info.repository_root.clone());
        if let Some(root) = root {
            self.launch_svn_visual(SvnVisual::BrowseRepository(root));
        }
    }

    pub fn svn_checkout_repository(&mut self) {
        let root = self
            .git
            .as_ref()
            .filter(|info| info.vcs == VcsKind::SvnRepository)
            .and_then(|info| info.repository_root.clone());
        if let Some(root) = root {
            self.launch_svn_visual(SvnVisual::CheckoutRepository(root));
        }
    }

    /// Depth-first flatten of `dir` into `rows`, following `expanded`.
    /// Build the Files view's flattened tree snapshot. Free of `&self` so the
    /// background snapshot worker can run it off the render thread.
    fn tree_rows(root: &Path, expanded: &HashSet<PathBuf>) -> Vec<FileRow> {
        let mut rows = Vec::new();
        if let Some(parent) = root.parent() {
            rows.push(FileRow {
                path: parent.to_path_buf(),
                name: "..".to_owned(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: true,
                ignored: false,
            });
        }
        Self::flatten_dir_into(&mut rows, expanded, root, 0);
        Self::mark_ignored(root, &mut rows);
        rows
    }

    fn flatten_dir_into(
        rows: &mut Vec<FileRow>,
        expanded: &HashSet<PathBuf>,
        dir: &Path,
        depth: usize,
    ) {
        if rows.len() >= MAX_ROWS {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let entries: Vec<(bool, String, PathBuf)> = read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                // `.git` is noise in a file tree; everything else shows.
                if name == ".git" {
                    return None;
                }
                let is_dir = e.file_type().ok()?.is_dir();
                Some((is_dir, name, e.path()))
            })
            .collect();
        for (is_dir, name, path) in Self::ordered_entries(entries, MAX_PER_DIR) {
            if rows.len() >= MAX_ROWS {
                return;
            }
            let is_expanded = is_dir && expanded.contains(&path);
            rows.push(FileRow {
                path: path.clone(),
                name,
                depth,
                is_dir,
                expanded: is_expanded,
                is_parent: false,
                ignored: false,
            });
            if is_expanded {
                Self::flatten_dir_into(rows, expanded, &path, depth + 1);
            }
        }
    }

    /// Order one directory's entries and apply the per-directory cap.
    ///
    /// Order: directories first, then case-insensitive alphabetical. The cap is
    /// applied *after* sorting — capping the `read_dir` iterator instead samples
    /// whatever order the filesystem yields, and in a dot-heavy root (274 of
    /// this repo's 318 entries start with `.`) that pushes `nebula_app`, `docs`
    /// and the rest of the real tree past the cap, leaving a screen of `.tmp-*`.
    fn ordered_entries(
        mut entries: Vec<(bool, String, PathBuf)>,
        cap: usize,
    ) -> Vec<(bool, String, PathBuf)> {
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        entries.truncate(cap);
        entries
    }

    /// Annotate the already-sorted snapshot in one `git check-ignore` call. This
    /// runs after flattening, so ignore state can never change row order.
    fn mark_ignored(root: &Path, rows: &mut [FileRow]) {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let candidates: Vec<_> = rows
            .iter()
            .filter(|row| !row.is_parent)
            .filter_map(|row| row.path.strip_prefix(root).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        if candidates.is_empty() {
            return;
        }

        let mut command = Command::new("git");
        command
            .args(["--no-optional-locks", "check-ignore", "-z", "--stdin"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let Ok(mut child) = command.spawn() else { return };
        let Some(mut stdin) = child.stdin.take() else { return };
        for path in candidates {
            if stdin.write_all(path.as_bytes()).is_err() || stdin.write_all(&[0]).is_err() {
                return;
            }
        }
        drop(stdin);
        let Ok(output) = child.wait_with_output() else { return };
        if !output.status.success() && output.status.code() != Some(1) {
            return;
        }
        let ignored: HashSet<String> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
            .collect();
        for row in rows {
            row.ignored = row.path.strip_prefix(root).ok().is_some_and(|relative| {
                ignored.contains(&relative.to_string_lossy().replace('\\', "/"))
            });
        }
    }

    /// Click on visible row `index` (post-scroll). Directories toggle their
    /// expansion; files are inert (v1). Returns whether anything changed.
    pub fn click_row(&mut self, index: usize) -> bool {
        if self.view != PanelView::Files || !self.search.trim().is_empty() {
            return false;
        }
        let Some(row) = self.rows.get(self.scroll + index) else { return false };
        if !row.is_dir {
            return false;
        }
        let path = row.path.clone();
        if row.is_parent {
            // 返回上级只改变窗口内的目录树根节点，不能连带修改终端 cwd；
            // 用户仍可通过现有“跟随”操作回到当前终端目录。
            return self.set_custom_root(path);
        }
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        // Re-flatten only (no git re-run): tree shape changed, content didn't.
        self.rebuild_rows();
        true
    }

    /// Complete the plain-click half of a pending directory drag. The source
    /// path must still occupy the pressed row; otherwise a refresh/scroll
    /// change could expand an unrelated directory after mouse release.
    pub fn click_drag_source(&mut self, drag: &FileDrag) -> bool {
        if drag.active || !drag.is_dir {
            return false;
        }
        let matches_source = self
            .visible_row(drag.source_row)
            .is_some_and(|row| row.is_dir && !row.is_parent && row.path == drag.path);
        matches_source && self.click_row(drag.source_row)
    }

    /// Scroll by `delta` rows (positive = down), clamped to the list length.
    pub fn scroll_by(&mut self, delta: i32, visible_rows: usize) {
        let len = match self.view {
            PanelView::Files => self.rows.len(),
            // Two section headers + both file lists.
            PanelView::Git => {
                self.git.as_ref().map_or(0, |g| g.unstaged.len() + g.staged.len() + 2)
            },
        };
        let max = len.saturating_sub(visible_rows);
        self.scroll = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
    }

    pub fn file_rows(&self) -> &[FileRow] {
        &self.rows
    }

    /// The tree row currently shown at visible index `idx` (post-scroll).
    pub fn visible_row(&self, idx: usize) -> Option<&FileRow> {
        if self.view != PanelView::Files {
            return None;
        }
        self.rows.get(self.scroll + idx)
    }

    pub fn git(&self) -> Option<&GitInfo> {
        self.git.as_ref()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

/// Resting drawer width in logical pixels (clamped to 42% of the window in
/// `panel_layout`). Shared with the grid-padding reserve and the terminal
/// card's right edge, so the drawer genuinely occupies layout space (the grid
/// reflows around it) instead of floating over the terminal.
pub const PANEL_W_LOGICAL: f32 = 300.0;

/// Panel geometry, physical pixels: `(x, y, w, h)` of the drawer, plus the
/// header strip height and one list row height.
pub struct PanelLayout {
    pub panel: (f32, f32, f32, f32),
    pub header_h: f32,
    pub row_h: f32,
    /// Files-view filter input box (between the summary line and the list).
    pub search: (f32, f32, f32, f32),
    /// Y of the first list row (below header, summary line and search box).
    pub list_y: f32,
    pub max_rows: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct PanelToolsLayout {
    pub directory: (f32, f32, f32, f32),
    pub follow: (f32, f32, f32, f32),
    pub terminal: (f32, f32, f32, f32),
    pub reveal: (f32, f32, f32, f32),
}

/// Drawer layout: a floating panel pinned to the SAME vertical band as the
/// left tab sidebar (`chrome_tab_layout`) — top at `margin + bar_h + 12`,
/// bottom at `win_h - margin - 12` — and inset from the right window edge by
/// `margin`, so both chrome panels share one height, one baseline, and float
/// with all four corners in open space (a flush edge squares off the corners).
/// The `_top`/`_bottom` chrome reserves the caller passes are no longer used
/// for the band: the constants here are locked to the sidebar's so the two can
/// never drift. `slide` is the open-animation progress (0 = fully off-screen
/// right, 1 = resting position); the whole drawer rides it.
pub fn panel_layout(
    win_w: f32,
    win_h: f32,
    _top: f32,
    _bottom: f32,
    scale: f32,
    slide: f32,
    panel_w: f32,
) -> PanelLayout {
    let s = |v: f32| v * scale;
    // Same margin / bar height / breathing gap as `chrome_tab_layout`.
    let margin = s(8.0);
    let bar_h = s(40.0);
    let gap = s(12.0);
    // `panel_w` 是（可能被拖拽调过的）逻辑宽；[`PANEL_W_LOGICAL`] 只是默认值。
    let w = s(panel_w).min(win_w * 0.42);
    // Motion Runtime already provides the physical response. Applying another
    // curve here would double-ease the drawer and make its ending feel sticky.
    let eased = slide.clamp(0.0, 1.0);
    // Resting x is inset by `margin` (mirroring the left panel's left inset);
    // closed, it rides fully off the right edge. Travel = the panel width plus
    // its margin so nothing peeks while closed.
    let rest_x = win_w - margin - w;
    let x = rest_x + (1.0 - eased) * (w + margin);
    let y = margin + bar_h + gap;
    let h = (win_h - margin - gap - y).max(0.0);
    // 12px top margin + 32px segmented control + 10px breathing room.
    let header_h = s(50.0);
    let row_h = s(34.0);
    let tools = panel_tools_layout_raw(x, y, w, header_h, scale);
    let search =
        (x + s(12.0), tools.directory.1 + tools.directory.3 + s(10.0), w - s(24.0), s(30.0));
    let list_y = search.1 + search.3 + s(8.0);
    let max_rows = (((y + h) - list_y) / row_h).max(0.0) as usize;
    PanelLayout { panel: (x, y, w, h), header_h, row_h, search, list_y, max_rows }
}

/// Hit-test a pixel against the open drawer (`layout` from [`panel_layout`]).
pub fn panel_hit(layout: &PanelLayout, x: f32, y: f32) -> PanelHit {
    let (px, py, pw, ph) = layout.panel;
    if x < px || x >= px + pw || y < py || y >= py + ph {
        return PanelHit::None;
    }
    if y < py + layout.header_h {
        // Header: one segmented control with two equal slots. Its 12px outer
        // margin is intentionally inert instead of creating oversized hits.
        let scale = layout.header_h / 50.0;
        let inset = 12.0 * scale;
        let top = py + inset;
        let height = 32.0 * scale;
        if x >= px + inset && x < px + pw - inset && y >= top && y < top + height {
            return if x < px + pw * 0.5 { PanelHit::ViewFiles } else { PanelHit::ViewGit };
        }
    }
    let (sx, sy, sw, sh) = layout.search;
    if x >= sx && x < sx + sw && y >= sy && y < sy + sh {
        return PanelHit::Search;
    }
    if y >= layout.list_y {
        let row = ((y - layout.list_y) / layout.row_h) as usize;
        if row < layout.max_rows {
            return PanelHit::Row(row);
        }
    }
    PanelHit::Inside
}

pub fn panel_action_rects(
    layout: &PanelLayout,
    _custom_root: bool,
    _has_root: bool,
) -> impl Iterator<Item = (PanelHit, (f32, f32, f32, f32))> {
    let tools = panel_tools_layout(layout);
    [
        (PanelHit::FollowCurrentDirectory, tools.follow),
        (PanelHit::NewTerminalHere, tools.terminal),
        (PanelHit::RevealDirectory, tools.reveal),
    ]
    .into_iter()
}

pub fn panel_tools_layout(layout: &PanelLayout) -> PanelToolsLayout {
    let scale = layout.header_h / 50.0;
    let (px, py, pw, _) = layout.panel;
    panel_tools_layout_raw(px, py, pw, layout.header_h, scale)
}

fn panel_tools_layout_raw(
    px: f32,
    py: f32,
    pw: f32,
    header_h: f32,
    scale: f32,
) -> PanelToolsLayout {
    let s = |value: f32| value * scale;
    let y = py + header_h + s(4.0);
    let button = s(26.0);
    let gap = s(6.0);
    let right = px + pw - s(12.0);
    let reveal = (right - button, y, button, button);
    let terminal = (reveal.0 - gap - button, y, button, button);
    let follow = (terminal.0 - gap - button, y, button, button);
    let directory_x = px + s(12.0);
    let directory = (directory_x, y, (follow.0 - gap - directory_x).max(s(48.0)), button);
    PanelToolsLayout { directory, follow, terminal, reveal }
}

pub fn panel_interactive_hit(
    layout: &PanelLayout,
    view: PanelView,
    custom_root: bool,
    has_root: bool,
    x: f32,
    y: f32,
) -> PanelHit {
    if view == PanelView::Files {
        let directory = panel_tools_layout(layout).directory;
        if x >= directory.0
            && x < directory.0 + directory.2
            && y >= directory.1
            && y < directory.1 + directory.3
        {
            return PanelHit::OpenDirectory;
        }
        for (hit, (rx, ry, rw, rh)) in panel_action_rects(layout, custom_root, has_root) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                return if has_root || hit == PanelHit::FollowCurrentDirectory {
                    hit
                } else {
                    PanelHit::Inside
                };
            }
        }
    }
    panel_hit(layout, x, y)
}

/// Budget-bounded deep walk building the flat filter index. `budget` counts
/// every entry VISITED (not kept), so a huge build tree can't stall the UI;
/// bulk directories (`target/`, `node_modules/`, …) are skipped outright, and
/// symlinks/junctions are never followed (cycle safety).
fn build_search_index(dir: &Path, depth: usize, index: &mut Vec<FileRow>, budget: &mut usize) {
    if *budget == 0 || depth > 8 || index.len() >= SEARCH_INDEX_CAP {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        if *budget == 0 || index.len() >= SEARCH_INDEX_CAP {
            return;
        }
        *budget -= 1;
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = ft.is_dir();
        if is_dir && (name.starts_with('.') || SEARCH_SKIP_DIRS.contains(&name.as_str())) {
            continue;
        }
        let path = entry.path();
        index.push(FileRow {
            path: path.clone(),
            name,
            depth: 0,
            is_dir,
            expanded: false,
            is_parent: false,
            ignored: false,
        });
        if is_dir {
            build_search_index(&path, depth + 1, index, budget);
        }
    }
}

/// Snapshot git state for `root`: branch, ±line counts, changed files./// `None` when git is missing or `root` isn't inside a work tree. Runs
/// synchronously — callers throttle (see [`SidePanel::sync`]).
fn read_git(root: &Path) -> Option<GitInfo> {
    use std::process::Command;
    // `safe.directory` scoped to this one invocation: repos owned by another
    // user — most commonly a `\\wsl$\…` UNC root, where every file belongs to
    // the WSL distro — make git bail with "dubious ownership" and the Git view
    // silently blanked while `git status` in the user's own shell worked fine
    // (个别情况 status 可用但面板不显示). Read-only status/diff on a directory
    // the user is already browsing carries none of the write risks the global
    // opt-in guards against.
    let safe_directory = format!("safe.directory={}", root.display());
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = Command::new("git");
        cmd.args(["-c", &safe_directory, "--no-optional-locks"]).args(args).current_dir(root);
        // Suppress the console window that `Command` flashes on Windows GUI apps.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            // Leave a trace instead of a silent blank panel: the first stderr
            // line names the actual refusal (ownership, not-a-repo, …).
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr.lines().next().unwrap_or("unknown error");
            log::debug!("git {:?} failed in {}: {reason}", args.first(), root.display());
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // `-b --porcelain` yields `## branch...upstream [ahead N]` + one `XY path`
    // per change, X = index (staged) status, Y = worktree status.
    let status = run(&["status", "--porcelain", "-b"])?;
    let mut info = GitInfo::default();
    for line in status.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            // `main...origin/main [ahead 1]` → `main`; detached prints as-is.
            info.branch = head.split("...").next().unwrap_or(head).to_string();
            if let Some(idx) = head.find("ahead ") {
                info.ahead = head[idx + 6..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
            }
        } else if line.len() > 3 {
            let x = line.as_bytes()[0] as char;
            let y = line.as_bytes()[1] as char;
            let path = line[3..].trim().to_string();
            if x == '?' || y == '?' {
                info.unstaged.push(('?', path));
                continue;
            }
            // Merge conflicts (VS Code's "Merge Changes" group). The path
            // also stays in staged/unstaged below so the legacy view keeps
            // rendering it untouched.
            if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
                info.conflicts.push(('U', path.clone()));
            }
            // One file can be in BOTH lists (partially staged).
            if x != ' ' {
                info.staged.push((x, path.clone()));
            }
            if y != ' ' {
                info.unstaged.push((y, path));
            }
        }
    }

    // `x files changed, 140 insertions(+), 69 deletions(-)` → (140, 69).
    if let Some(stat) = run(&["diff", "--shortstat", "HEAD"]) {
        for part in stat.split(',') {
            let num: u64 = part.trim().split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            if part.contains("insertion") {
                info.plus = num;
            } else if part.contains("deletion") {
                info.minus = num;
            }
        }
    }
    Some(info)
}

/// Cheap preference hint for nested checkouts. `read_svn` still runs the
/// authoritative `svn info` command, so this hint cannot make detection
/// incorrect when metadata is represented differently by an SVN client.
fn svn_dir_hint(root: &Path) -> bool {
    !matches!(crate::svn_status::classify_dir(root), crate::svn_status::SvnDirKind::Plain)
}

/// `svn status` 每行：第 1 列 item 状态（M/A/D/C/?/!…），第 8 列起路径。
/// SVN 没有暂存区——所有变化都归 `unstaged`，与 Git 视图同列渲染。
fn parse_svn_status(status: &str) -> Vec<(char, String)> {
    status
        .lines()
        .filter_map(|line| {
            let mut chars = line.chars();
            let state = chars.next()?;
            if state == ' ' || line.len() < 9 {
                return None;
            }
            let path = line[8..].trim();
            (!path.is_empty()).then(|| (state, path.replace('\\', "/")))
        })
        .collect()
}

/// Snapshot SVN state for `root`. The CLI（`svn info`/`svn status`）is tried
/// first for maximum fidelity; machines without a command-line client
/// (TortoiseSVN installs GUI only) fall back to reading `.svn/wc.db`
/// directly（`svn_status` 模块，TSVNCache 同款路线）。`None` means the path
/// is not a working copy at all.
fn read_svn(root: &Path) -> Option<GitInfo> {
    match crate::svn_status::classify_dir(root) {
        crate::svn_status::SvnDirKind::Repository(repository_root) => Some(GitInfo {
            vcs: VcsKind::SvnRepository,
            branch: "SVN 版本库".to_owned(),
            repository_root: Some(repository_root),
            ..GitInfo::default()
        }),
        crate::svn_status::SvnDirKind::WorkingCopy(_) => {
            read_svn_cli(root).map(fill_svn_conflicts).or_else(|| read_svn_wc_db(root))
        },
        // 少数客户端的元数据布局可能不同，仍给权威 CLI 一次识别机会。
        crate::svn_status::SvnDirKind::Plain => read_svn_cli(root).map(fill_svn_conflicts),
    }
}

/// CLI 输出没有单独的冲突列表：从 `unstaged` 里把 `C` 行登记到
/// [`GitInfo::conflicts`]（路径保留原位，合同见字段注释）。
fn fill_svn_conflicts(mut info: GitInfo) -> GitInfo {
    info.conflicts = info
        .unstaged
        .iter()
        .filter(|(state, _)| *state == 'C')
        .map(|(_, path)| ('C', path.clone()))
        .collect();
    info
}

/// 零外部依赖的 SVN 快照：`svn_status` 读 `.svn/wc.db` 推导状态字母表，
/// 修订号取 NODES 根行。路径统一转成相对 `root` 的正斜杠形式，与 CLI
/// `svn status` 的展示合同一致（只显示 `root` 之下的条目）。
fn read_svn_wc_db(root: &Path) -> Option<GitInfo> {
    let crate::svn_status::SvnDirKind::WorkingCopy(wc_root) = crate::svn_status::classify_dir(root)
    else {
        return None;
    };
    let changes = crate::svn_status::working_copy_status(&wc_root).ok()?;
    let revision = crate::svn_status::working_copy_revision(&wc_root);
    let mut info = GitInfo {
        vcs: VcsKind::Svn,
        branch: revision.map(|value| format!("r{value}")).unwrap_or_else(|| "svn".to_owned()),
        ..GitInfo::default()
    };
    let prefix = root.strip_prefix(&wc_root).ok().map(|relative| {
        let mut text = relative.to_string_lossy().replace('\\', "/");
        if !text.is_empty() && !text.ends_with('/') {
            text.push('/');
        }
        text
    });
    for change in changes {
        let shown = match prefix.as_deref() {
            None | Some("") => change.rel_path.clone(),
            Some(prefix) => match change.rel_path.strip_prefix(prefix) {
                Some(inside) => inside.to_owned(),
                // `root` 子目录之外的变更不在本视图的展示合同内。
                None => continue,
            },
        };
        let state = change.state.letter().chars().next().unwrap_or('M');
        if change.state == crate::svn_status::SvnState::Conflicted {
            info.conflicts.push(('C', shown.clone()));
        }
        // SVN 无暂存区：全部归 unstaged（GPUI 会把冲突条目过滤出去单独分组）。
        info.unstaged.push((if state == 'U' { '?' } else { state }, shown));
    }
    Some(info)
}

/// CLI 路线（现代客户端的权威路径）。
fn read_svn_cli(root: &Path) -> Option<GitInfo> {
    use std::process::Command;
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = Command::new("svn");
        // 交互式认证提示会把无头子进程挂死；快照必须是非交互的。
        cmd.arg("--non-interactive").args(args).current_dir(root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr.lines().next().unwrap_or("unknown error");
            log::debug!("svn {:?} failed in {}: {reason}", args.first(), root.display());
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // `--show-item` is available in modern SVN. Fall back to regular
    // `svn info` so older clients still produce a revision number.
    let revision = run(&["info", "--show-item", "revision"])
        .and_then(|value| (!value.trim().is_empty()).then_some(value))
        .or_else(|| run(&["info"]).and_then(|value| parse_svn_revision(&value)))?;
    let status = run(&["status"])?;
    Some(GitInfo {
        vcs: VcsKind::Svn,
        branch: format!("r{}", revision.trim()),
        unstaged: parse_svn_status(&status),
        ..GitInfo::default()
    })
}

fn parse_svn_revision(info: &str) -> Option<String> {
    info.lines().find_map(|line| {
        let value = line.strip_prefix("Revision:")?.trim();
        (!value.is_empty()).then_some(value.to_owned())
    })
}

// ---- rendering (mirrors the `settings.rs` split: the parent `display::mod`
// hands in a snapshot + renderer; this module owns the drawer's pixels) ----

use crate::display::color::Rgb;
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use super::ui::widgets;
use super::{NebulaTheme, SizeInfo, UI_CORNER_RADIUS_LOGICAL};

// Nerd Font glyphs verified in the bundled Maple Mono font. The folder pair
// intentionally uses the lighter outline family from the approved prototype.
pub(crate) const ICON_FOLDER: &str = "\u{f114}";
pub(crate) const ICON_FOLDER_OPEN: &str = "\u{f115}";
const ICON_TERMINAL: &str = "\u{ea85}";
const ICON_FILE: &str = "\u{ea7b}";
pub(crate) const ICON_CHEVRON_RIGHT: &str = "\u{eab6}";
const ICON_CHEVRON_DOWN: &str = "\u{eab4}";

/// GPUI 文件树与旧壳共用同一组 Nerd Font 字形，避免两套图标在展开状态
/// 和字宽上出现细微漂移。
pub(crate) fn folder_icon(expanded: bool) -> &'static str {
    if expanded { ICON_FOLDER_OPEN } else { ICON_FOLDER }
}

pub(crate) fn chevron_icon(expanded: bool) -> &'static str {
    if expanded { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_RIGHT }
}
const ICON_BRANCH: &str = "\u{ea68}";
const ICON_SEARCH: &str = "\u{f002}";
const ICON_HOME: &str = "\u{f015}";
const ICON_FOLLOW: &str = "\u{f140}";

/// File-type icon for a tree row, keyed by extension (dotfile names like
/// `.gitignore` count as their own family). The glyph carries the type; the
/// ink stays the tree's neutral scheme — no per-type colors in the chrome.
/// Every codepoint here is verified present in the bundled Maple Mono NF CN
/// (codicon/seti/devicon/octicon blocks), so nothing can render as tofu.
pub(crate) fn file_type_icon(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".git") {
        return "\u{e65d}"; // seti-git: .gitignore/.gitattributes/.gitmodules
    }
    let ext = lower.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match ext {
        "md" | "markdown" => "\u{eb1d}",           // cod-markdown
        "json" | "jsonl" | "ndjson" => "\u{eb0f}", // cod-json
        "toml" => "\u{e6b2}",
        "yml" | "yaml" => "\u{e6a8}",
        "xml" => "\u{e619}",
        "rs" => "\u{e68b}",
        "py" => "\u{e606}",
        "js" | "mjs" | "cjs" | "jsx" => "\u{e60c}",
        "ts" | "tsx" => "\u{e628}",
        "html" | "htm" => "\u{e60e}",
        "css" | "scss" | "less" => "\u{e614}",
        "c" | "h" => "\u{e61e}",
        "cpp" | "cc" | "cxx" | "hpp" => "\u{e61d}",
        "cs" => "\u{e648}",
        "java" => "\u{e66d}",
        "go" => "\u{e627}",
        "sh" | "bash" | "zsh" => "\u{e691}",
        "ps1" | "psm1" | "psd1" => "\u{e683}",
        "bat" | "cmd" => "\u{ea85}", // cod-terminal
        "sql" | "db" | "sqlite" => "\u{e64d}",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" => "\u{e60d}",
        "zip" | "7z" | "rar" | "gz" | "tar" | "xz" | "zst" => "\u{f1c6}",
        "pdf" => "\u{f1c1}",
        "lock" => "\u{e672}",
        "log" => "\u{f4ed}",
        "txt" => "\u{f0f6}",
        _ => ICON_FILE,
    }
}

/// Git status colors (GitHub Primer hues), picked per theme brightness so
/// they hold contrast on both surface families.
fn status_color(status: char, is_light: bool) -> Option<Rgb> {
    Some(match (status, is_light) {
        ('M' | 'R' | 'C', false) => Rgb::new(210, 153, 34),
        ('M' | 'R' | 'C', true) => Rgb::new(154, 103, 0),
        ('A', false) => Rgb::new(63, 185, 80),
        ('A', true) => Rgb::new(26, 127, 55),
        ('D', false) => Rgb::new(248, 81, 73),
        ('D', true) => Rgb::new(207, 34, 46),
        _ => None?, // '?' and friends fall back to dim ink.
    })
}

/// The terminal palette colors the tree rows share with `ls` (Nebula-List
/// paints dirs with ANSI Blue and executables with ANSI Green — the drawer
/// must agree with what the user sees in the grid, including theme switches).
#[derive(Clone, Copy)]
pub struct LsColors {
    pub dir: Rgb,
    pub exec: Rgb,
}

/// Executable extensions, matching Nebula-List's green set.
fn is_executable(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["exe", "dll", "bat", "cmd", "ps1", "com", "msi", "sh"]
        .iter()
        .any(|ext| lower.rsplit('.').next() == Some(*ext) && lower.contains('.'))
}

/// Push the drawer's background quads: the flat panel surface (same 底色 as
/// the left tab sidebar), the active header view-tab pill, and the filter input
/// box — all curved with the shared chrome radius.
/// Display columns of a drag-chip label (CJK counts 2) — shared by the quad
/// pass (chip width; it has no cell metrics) and the text pass, so the label
/// and its chip agree on the same width.
fn drag_chip_cols(name: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    name.chars().map(|c| c.width().unwrap_or(0).max(1)).sum()
}

fn panel_action_tooltip(
    panel: &SidePanel,
    layout: &PanelLayout,
    scale: f32,
    cell_w: f32,
) -> Option<((f32, f32, f32, f32), &'static str)> {
    if panel.view != PanelView::Files {
        return None;
    }
    let (action, label) = match panel.hover {
        PanelHit::FollowCurrentDirectory => {
            (PanelHit::FollowCurrentDirectory, "跟随当前终端  Alt+R")
        },
        PanelHit::NewTerminalHere => (PanelHit::NewTerminalHere, "在此新建终端  Alt+T"),
        PanelHit::RevealDirectory => (PanelHit::RevealDirectory, "在资源管理器中打开  Alt+O"),
        _ => return None,
    };
    let action_rect =
        panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
            .find_map(|(hit, rect)| (hit == action).then_some(rect))?;
    let s = |value: f32| value * scale;
    let (px, _, pw, _) = layout.panel;
    let width = (drag_chip_cols(label) as f32 * cell_w + s(16.0)).min(pw - s(16.0));
    let x = (action_rect.0 + action_rect.2 * 0.5 - width * 0.5)
        .clamp(px + s(8.0), px + pw - s(8.0) - width);
    Some(((x, action_rect.1 + action_rect.3 + s(6.0), width, s(26.0)), label))
}

pub(super) fn push_quads(
    panel: &SidePanel,
    layout: &PanelLayout,
    theme: &NebulaTheme,
    quads: &mut Vec<UiQuad>,
    scale: f32,
    cell_w: f32,
) {
    let s = |v: f32| v * scale;
    let palette = theme.palette();
    let sk = theme.skin();
    let (px, py, pw, ph) = layout.panel;
    // Shared chrome radius + the tab sidebar's accent (edge_r) — so the drawer
    // curves and lights up exactly like the left vertical tabs.
    let radius = s(UI_CORNER_RADIUS_LOGICAL);
    let accent = palette.edge_r;

    // Panel surface: the SAME flat 底色 as the left tab sidebar (`palette.panel`,
    // not a gradient — the gradient budget belongs to the brand art, chrome
    // stays flat).
    quads.push(UiQuad::solid(px, py, pw, ph, radius, palette.panel));

    // One segmented shell owns both Files and Git. This is deliberately a
    // single dark control, not two unrelated pills floating in the header.
    let segment = (px + s(12.0), py + s(12.0), pw - s(24.0), s(32.0));
    let tab_w = (segment.2 - s(4.0)) * 0.5;
    let tab_h = segment.3 - s(4.0);
    let (fx, gx) = (segment.0 + s(2.0), segment.0 + s(2.0) + tab_w);
    let active_x = match panel.view {
        PanelView::Files => fx,
        PanelView::Git => gx,
    };
    let ty = segment.1 + s(2.0);
    quads.push(UiQuad::solid(segment.0, segment.1, segment.2, segment.3, radius, sk.input));
    quads.push(UiQuad::solid(active_x, ty, tab_w, tab_h, radius, sk.card));
    quads.push(UiQuad::solid(active_x, ty, tab_w, tab_h, radius, sk.surface));
    // Hover never changes segmented-control geometry or fill. The text pass
    // alone raises the inactive label's ink, so switching stays visually still.
    if let Some(git) = panel.git() {
        let count = git.unstaged.len() + git.staged.len();
        if count > 0 {
            let digits = count.to_string();
            let badge_w = digits.len() as f32 * cell_w + s(12.0);
            let content_w = cell_w + s(6.0) + cell_w * 3.0 + s(6.0) + badge_w;
            let start = gx + (tab_w - content_w) * 0.5;
            let badge_x = start + cell_w + s(6.0) + cell_w * 3.0 + s(6.0);
            quads.push(UiQuad::solid(
                badge_x,
                ty + (tab_h - s(16.0)) * 0.5,
                badge_w,
                s(16.0),
                s(16.0) * 0.5,
                sk.accent_soft,
            ));
        }
    }

    if panel.view == PanelView::Files {
        let tools = panel_tools_layout(layout);
        super::ui::surface::push_stroke(quads, tools.directory, radius, scale, sk.hairline);
        quads.push(UiQuad::solid(
            tools.directory.0,
            tools.directory.1,
            tools.directory.2,
            tools.directory.3,
            radius,
            sk.input,
        ));
        for (hit, (x, y, w, h)) in
            panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
        {
            let fill = if panel.hover == hit {
                Some(sk.hover_strong)
            } else if hit == PanelHit::FollowCurrentDirectory && !panel.custom_root_active() {
                Some(sk.accent_soft)
            } else {
                None
            };
            if let Some(fill) = fill {
                quads.push(UiQuad::solid(x, y, w, h, radius, fill));
            }
        }
        if !panel.custom_root_active() {
            let (_, y, w, _) = tools.follow;
            quads.push(UiQuad::solid(
                tools.follow.0 + w - s(7.0),
                y + s(4.0),
                s(5.0),
                s(5.0),
                s(5.0) * 0.5,
                sk.ok,
            ));
        }
    }

    // Hovered list row: a quiet wash under the pointer (never on top of the
    // selected pill — selection outranks hover).
    if let PanelHit::Row(i) = panel.hover {
        if i < layout.max_rows {
            let hover_ok = match panel.view {
                PanelView::Files => panel
                    .file_rows()
                    .get(panel.scroll + i)
                    .is_some_and(|row| panel.selected.as_ref() != Some(&row.path)),
                PanelView::Git => panel.git_row_is_file(i),
            };
            if hover_ok {
                let ry = layout.list_y + i as f32 * layout.row_h;
                quads.push(UiQuad::solid(
                    px + s(10.0),
                    ry - s(1.0),
                    pw - s(20.0),
                    layout.row_h - s(4.0),
                    radius,
                    sk.hover,
                ));
            }
        }
    }

    // Files-view filter box (input surface; accent ring while focused).
    if panel.view == PanelView::Files {
        let (sx, sy, sw, sh) = layout.search;
        if panel.search_focus {
            let a = sk.accent;
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw + s(2.0),
                sh + s(2.0),
                radius + s(1.0),
                Rgba::new(a.r, a.g, a.b, 200),
            ));
        }
        quads.push(UiQuad::solid(sx, sy, sw, sh, radius, sk.input));
        if panel.search_all_selected() && !panel.search.is_empty() {
            let columns: usize = panel.search.chars().map(|c| c.width().unwrap_or(0)).sum();
            let selection_x = sx + s(8.0) + cell_w * 1.8;
            let selection_w = (columns as f32 * cell_w).min(sw - (selection_x - sx) - s(8.0));
            quads.push(UiQuad::solid(
                selection_x - s(2.0),
                sy + s(6.0),
                selection_w + s(4.0),
                sh - s(12.0),
                s(4.0),
                sk.accent_soft,
            ));
        }

        // The selected file row wears the tab's floating-pill language: an
        // accent halo + the tab 底色 + a soft accent wash — the same treatment
        // the left sidebar's active tab and the header view-tab use, so a
        // picked row reads as "selected" identically across the whole chrome.
        // The dragged row shares it, so the drag has a visible subject from press.
        let marked = panel.drag_file.as_ref().map(|d| &d.path).or(panel.selected.as_ref());
        if let Some(mark) = marked {
            if let Some(i) = panel
                .file_rows()
                .iter()
                .skip(panel.scroll)
                .take(layout.max_rows)
                .position(|row| &row.path == mark)
            {
                let ry = layout.list_y + i as f32 * layout.row_h - s(1.0);
                let (px, _, pw, _) = layout.panel;
                let rx = px + s(10.0);
                let rw = pw - s(20.0);
                let rh = layout.row_h - s(2.0);
                quads.push(UiQuad::solid(
                    rx - s(1.0),
                    ry - s(1.0),
                    rw + s(2.0),
                    rh + s(2.0),
                    radius + s(1.0),
                    Rgba::new(accent.r, accent.g, accent.b, 40),
                ));
                quads.push(UiQuad::solid(rx, ry, rw, rh, radius, palette.tab_bg_l));
                quads.push(UiQuad::solid(
                    rx,
                    ry,
                    rw,
                    rh,
                    radius,
                    Rgba::new(accent.r, accent.g, accent.b, 26),
                ));
            }
        }

        // Drag ghost: a floating chip beside the pointer while a file is in
        // flight — the pointer alone was invisible feedback.
        if let Some(drag) = panel.drag_file.as_ref().filter(|d| d.active) {
            let (mx, my) = drag.pos;
            let chip_w = (drag_chip_cols(&drag.name) as f32 * s(8.0) + s(32.0)).min(s(220.0));
            quads.push(UiQuad::solid(
                mx + s(12.0),
                my + s(14.0),
                chip_w,
                s(26.0),
                s(8.0),
                sk.accent_soft,
            ));
            quads.push(UiQuad::solid(
                mx + s(12.0),
                my + s(14.0),
                s(2.0),
                s(26.0),
                s(1.0),
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 190),
            ));
        }
    } else if panel.git().is_some() {
        // Git view: the strip is either the commit-message input (accent
        // ring) or the three action buttons (暂存 / 提交 / 推送). Outside a
        // repository there is nothing to act on — no strip at all.
        let (sx, sy, sw, sh) = layout.search;
        if panel.commit_focus {
            let a = sk.accent;
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw + s(2.0),
                sh + s(2.0),
                radius + s(1.0),
                Rgba::new(a.r, a.g, a.b, 200),
            ));
            quads.push(UiQuad::solid(sx, sy, sw, sh, radius, sk.input));
            if panel.commit_all_selected() && !panel.commit_msg.is_empty() {
                let columns: usize = panel.commit_msg.chars().map(|c| c.width().unwrap_or(0)).sum();
                let selection_w = (columns as f32 * cell_w).min(sw - s(16.0));
                quads.push(UiQuad::solid(
                    sx + s(6.0),
                    sy + s(6.0),
                    selection_w + s(4.0),
                    sh - s(12.0),
                    s(4.0),
                    sk.accent_soft,
                ));
            }
        } else {
            for (bx, bw) in git_button_rects(sx, sw, s(6.0)) {
                quads.push(UiQuad::solid(bx, sy, bw, sh, radius, sk.input));
            }
            // Hovered action button brightens (hover wash over the pill).
            if panel.hover == PanelHit::Search {
                let (hx, _) = panel.hover_pos;
                for (bx, bw) in git_button_rects(sx, sw, s(6.0)) {
                    if hx >= bx && hx < bx + bw {
                        quads.push(UiQuad::solid(bx, sy, bw, sh, radius, sk.hover));
                    }
                }
            }
        }
    }

    // Tooltip is appended last so it floats above the search field below the
    // tools row. The text pass uses the same helper and therefore cannot drift.
    if let Some((tooltip, _)) = panel_action_tooltip(panel, layout, scale, cell_w) {
        let tooltip_radius = super::ui::tokens::radius::CONTROL * scale;
        super::ui::surface::push_stroke(quads, tooltip, tooltip_radius, scale, sk.hairline);
        quads.push(UiQuad::solid(
            tooltip.0,
            tooltip.1,
            tooltip.2,
            tooltip.3,
            tooltip_radius,
            sk.card,
        ));
    }
}

/// The four git action buttons' `(x, w)` spans inside `sx..sx+sw`.
pub fn git_button_rects(sx: f32, sw: f32, gap: f32) -> [(f32, f32); 4] {
    let bw = (sw - 3.0 * gap) / 4.0;
    [(sx, bw), (sx + bw + gap, bw), (sx + 2.0 * (bw + gap), bw), (sx + 3.0 * (bw + gap), bw)]
}

/// Draw the drawer's text: header tabs, the summary line (cwd tail or the
/// branch ± counts), the filter box content, then the visible rows.
pub(super) fn draw_text(
    panel: &SidePanel,
    layout: &PanelLayout,
    theme: &NebulaTheme,
    _ls: LsColors,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let sk = theme.skin();
    let is_light = theme.palette().is_light;
    let (px, py, pw, _) = layout.panel;
    let text_pad = s(12.0);
    // Truncation budgets are in display COLUMNS (CJK counts 2), matching
    // draw_chrome_text's advance — a char-count budget lets a CJK name run
    // twice as wide as intended, straight across the hover wash.
    // Paths left-truncate (`…tail` — the discriminating end stays visible);
    // file names right-truncate (`name…`, see `truncate_tab_label`).
    let clip_tail = |t: &str, budget_cols: usize| -> String {
        use unicode_width::UnicodeWidthChar;
        let budget = budget_cols.max(4);
        let total: usize = t.chars().map(|c| c.width().unwrap_or(0).max(1)).sum();
        if total <= budget {
            return t.to_string();
        }
        // Walk from the end, keeping the widest tail that fits after the `…`.
        let mut used = 1usize; // the ellipsis column
        let mut tail = std::collections::VecDeque::new();
        for ch in t.chars().rev() {
            let w = ch.width().unwrap_or(0).max(1);
            if used + w > budget {
                break;
            }
            used += w;
            tail.push_front(ch);
        }
        format!("…{}", tail.iter().collect::<String>())
    };
    // Right edge every row's text must stop before: the hover wash ends at
    // `px + pw - s(10)`, keep a small inset inside it.
    let row_text_right = px + pw - s(18.0);

    // Center each icon/label group inside its segmented-control slot.
    let segment_x = px + s(12.0);
    let segment_w = pw - s(24.0);
    let slot_w = (segment_w - s(4.0)) * 0.5;
    let header_ty = widgets::centered_y(py + s(12.0), s(32.0), cell_h);
    let files_hover = panel.hover == PanelHit::ViewFiles;
    let git_hover = panel.hover == PanelHit::ViewGit;
    let files_ink = if panel.view == PanelView::Files {
        sk.ink_strong
    } else if files_hover {
        sk.ink
    } else {
        sk.ink_dim
    };
    let git_ink = if panel.view == PanelView::Git {
        sk.ink_strong
    } else if git_hover {
        sk.ink
    } else {
        sk.ink_dim
    };
    let files_content_w = cell_w + s(6.0) + cell_w * 4.0;
    let fx = segment_x + s(2.0) + (slot_w - files_content_w) * 0.5;
    r.draw_chrome_text(size, fx, header_ty, files_ink, ICON_FOLDER, gc);
    r.draw_chrome_text(size, fx + cell_w + s(6.0), header_ty, files_ink, "文件", gc);
    let git_count = panel.git().map(|git| git.unstaged.len() + git.staged.len()).unwrap_or(0);
    let badge = (git_count > 0).then(|| git_count.to_string());
    let badge_w = badge.as_ref().map_or(0.0, |text| text.len() as f32 * cell_w + s(12.0));
    let git_content_w =
        cell_w + s(6.0) + cell_w * 3.0 + if badge.is_some() { s(6.0) + badge_w } else { 0.0 };
    let gx = segment_x + s(2.0) + slot_w + (slot_w - git_content_w) * 0.5;
    r.draw_chrome_text(size, gx, header_ty, git_ink, ICON_BRANCH, gc);
    let git_label_x = gx + cell_w + s(6.0);
    r.draw_chrome_text(size, git_label_x, header_ty, git_ink, "Git", gc);
    if let Some(badge) = badge {
        r.draw_chrome_text(
            size,
            git_label_x + cell_w * 3.0 + s(12.0),
            header_ty,
            sk.accent,
            &badge,
            gc,
        );
    }

    let directory = panel_tools_layout(layout).directory;
    let summary_y = widgets::centered_y(directory.1, directory.3, cell_h);
    let scroll = panel.scroll;
    let row_ty = |i: usize| {
        widgets::centered_y(layout.list_y + i as f32 * layout.row_h, layout.row_h, cell_h)
    };

    match panel.view {
        PanelView::Files => {
            let tools = panel_tools_layout(layout);
            let path_x = tools.directory.0 + s(9.0) + cell_w + s(10.0);
            let summary_cols =
                (((tools.directory.0 + tools.directory.2 - s(9.0) - path_x) / cell_w).floor()
                    as usize)
                    .max(4);
            let (summary, summary_ink) = if let Some(notice) = panel.root_notice() {
                (clip_tail(notice, summary_cols), Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b))
            } else {
                (
                    panel
                        .root()
                        .map(|root| clip_tail(&root.display().to_string(), summary_cols))
                        .unwrap_or_else(|| "（无目录）".into()),
                    sk.ink_dim,
                )
            };
            let path_ink =
                if panel.hover == PanelHit::OpenDirectory { sk.ink_strong } else { summary_ink };
            r.draw_chrome_text(
                size,
                tools.directory.0 + s(9.0),
                summary_y,
                sk.ink_faint,
                ICON_HOME,
                gc,
            );
            r.draw_chrome_text(size, path_x, summary_y, path_ink, &summary, gc);

            for (hit, (x, y, w, h)) in
                panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
            {
                let enabled = panel.root().is_some() || hit == PanelHit::FollowCurrentDirectory;
                let ink = if panel.hover == hit {
                    sk.ink_strong
                } else if !enabled {
                    sk.ink_faint
                } else if hit == PanelHit::FollowCurrentDirectory && !panel.custom_root_active() {
                    sk.accent
                } else {
                    sk.ink_dim
                };
                let label = match hit {
                    PanelHit::RevealDirectory => ICON_FOLDER_OPEN,
                    PanelHit::NewTerminalHere => ICON_TERMINAL,
                    PanelHit::FollowCurrentDirectory => ICON_FOLLOW,
                    _ => continue,
                };
                let tx = x + ((w - cell_w) / 2.0).max(0.0);
                let ty = widgets::centered_y(y, h, cell_h);
                r.draw_chrome_text(size, tx, ty, ink, label, gc);
            }

            // Filter box: magnifier + query (caret while focused) or hint.
            let (sx, sy, _, sh) = layout.search;
            let search_ty = widgets::centered_y(sy, sh, cell_h);
            r.draw_chrome_text(size, sx + s(8.0), search_ty, sk.ink_faint, ICON_SEARCH, gc);
            let qx = sx + s(8.0) + cell_w * 1.8;
            if panel.search.is_empty() && !panel.search_focus {
                r.draw_chrome_text(size, qx, search_ty, sk.ink_faint, "筛选文件…", gc);
            } else {
                let shown = if panel.search_focus
                    && !panel.search_all_selected()
                    && super::caret_blink_on()
                {
                    format!("{}▏", panel.search)
                } else {
                    panel.search.clone()
                };
                r.draw_chrome_text(size, qx, search_ty, sk.ink_strong, &shown, gc);
            }

            // Tree rows: chevron (dirs, tree mode only) + folder/file icon + name.
            let filtering = !panel.search.trim().is_empty();
            for (i, row) in panel.file_rows().iter().skip(scroll).take(layout.max_rows).enumerate()
            {
                let ry = row_ty(i);
                let mut x = px + text_pad + row.depth as f32 * cell_w * 2.4;
                if !filtering {
                    if row.is_dir && !row.is_parent {
                        let chev =
                            if row.expanded { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_RIGHT };
                        r.draw_chrome_text(size, x, ry, sk.ink_faint, chev, gc);
                    }
                    x += cell_w * 1.9;
                }
                let (icon, icon_ink, name_ink) = if row.ignored {
                    (
                        if row.is_dir && row.expanded {
                            ICON_FOLDER_OPEN
                        } else if row.is_dir {
                            ICON_FOLDER
                        } else {
                            file_type_icon(&row.name)
                        },
                        sk.ink_ignored,
                        sk.ink_ignored,
                    )
                } else if row.is_dir {
                    (
                        if row.expanded { ICON_FOLDER_OPEN } else { ICON_FOLDER },
                        sk.icon,
                        sk.ink_strong,
                    )
                } else {
                    (file_type_icon(&row.name), sk.ink_dim, sk.ink)
                };
                r.draw_chrome_text(size, x, ry, icon_ink, icon, gc);
                // Name budget from its REAL pixel start (indent + chevron +
                // icon) to the hover wash's right edge — a long name ends in
                // `…` exactly inside the wash instead of bleeding past it.
                let name_x = x + cell_w * 2.2;
                let name_cols = (((row_text_right - name_x) / cell_w).floor() as usize).max(2);
                let name = super::truncate_tab_label(&row.name, name_cols);
                r.draw_chrome_text(size, name_x, ry, name_ink, &name, gc);
            }
            let has_real_rows = panel.file_rows().iter().any(|row| !row.is_parent);
            if !has_real_rows {
                let empty = if filtering {
                    crate::ux::EmptyState::new(
                        "没有匹配文件",
                        "当前筛选词未匹配工作区内容。",
                        "修改筛选词，或按 Esc 清空筛选。",
                    )
                } else if panel.root.is_none() {
                    crate::ux::EmptyState::new(
                        "没有可浏览的目录",
                        "当前终端尚未报告工作目录。",
                        "在终端中进入一个目录后点击刷新。",
                    )
                } else {
                    crate::ux::EmptyState::new(
                        "此目录为空",
                        "当前工作目录中没有可显示的文件。",
                        "在终端创建文件，或选择其他目录。",
                    )
                };
                let parent_row_offset = panel
                    .file_rows()
                    .first()
                    .is_some_and(|row| row.is_parent)
                    .then_some(layout.row_h)
                    .unwrap_or(0.0);
                let y = layout.list_y + parent_row_offset + s(8.0);
                r.draw_chrome_text(size, px + text_pad, y, sk.ink_strong, &empty.title, gc);
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    y + s(20.0),
                    sk.ink_dim,
                    &super::truncate_tab_label(&empty.reason, 32),
                    gc,
                );
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    y + s(40.0),
                    sk.accent,
                    &super::truncate_tab_label(&empty.action, 32),
                    gc,
                );
            }

            // Drag ghost label, riding the chip pushed by `push_quads`.
            // Same chip-width formula as there (that pass has no cell_w), then
            // truncated against the REAL glyph advance so the label always
            // ends inside the chip.
            if let Some(drag) = panel.drag_file.as_ref().filter(|d| d.active) {
                let (mx, my) = drag.pos;
                let ty = widgets::centered_y(my + s(14.0), s(26.0), cell_h);
                let chip_w = (drag_chip_cols(&drag.name) as f32 * s(8.0) + s(32.0)).min(s(220.0));
                let max_cols = (((chip_w - s(26.0)) / cell_w).floor() as usize).max(2);
                r.draw_chrome_text(
                    size,
                    mx + s(10.0) + s(12.0),
                    ty,
                    sk.ink_strong,
                    &super::truncate_tab_label(&drag.name, max_cols),
                    gc,
                );
            }
        },
        PanelView::Git => match panel.git() {
            Some(git) => {
                // Branch line: icon + name strong; ↑ahead + line counts on the
                // right (an op error takes the line over instead).
                let bx = px + text_pad;
                r.draw_chrome_text(size, bx, summary_y, sk.ink_dim, ICON_BRANCH, gc);
                let branch =
                    clip_tail(if git.branch.is_empty() { "(no branch)" } else { &git.branch }, 18);
                r.draw_chrome_text(size, bx + cell_w * 1.8, summary_y, sk.ink_strong, &branch, gc);
                if let Some(err) = panel.op_error() {
                    let msg = clip_tail(&err, branch.chars().count() + 4);
                    let ex = px + pw - text_pad - msg.chars().count() as f32 * cell_w;
                    let c_del = status_color('D', is_light).unwrap();
                    r.draw_chrome_text(size, ex, summary_y, c_del, &msg, gc);
                } else {
                    let c_add = status_color('A', is_light).unwrap();
                    let c_del = status_color('D', is_light).unwrap();
                    let minus = format!("\u{2212}{}", git.minus);
                    let plus = format!("+{}", git.plus);
                    let ahead =
                        if git.ahead > 0 { format!("↑{} ", git.ahead) } else { String::new() };
                    let minus_x = px + pw - text_pad - minus.chars().count() as f32 * cell_w;
                    let plus_x = minus_x - (plus.chars().count() + 1) as f32 * cell_w;
                    let ahead_x = plus_x - (ahead.chars().count() + 1) as f32 * cell_w;
                    if !ahead.is_empty() {
                        r.draw_chrome_text(size, ahead_x, summary_y, sk.accent, &ahead, gc);
                    }
                    r.draw_chrome_text(size, plus_x, summary_y, c_add, &plus, gc);
                    r.draw_chrome_text(size, minus_x, summary_y, c_del, &minus, gc);
                }

                // Action strip: commit-message input while composing, else the
                // 暂存 / 提交 / 推送 buttons (disabled = dim ink).
                let (sx, sy, sw, sh) = layout.search;
                let strip_ty = widgets::centered_y(sy, sh, cell_h);
                if panel.commit_focus {
                    let caret = if !panel.commit_all_selected() && super::caret_blink_on() {
                        "▏"
                    } else {
                        ""
                    };
                    let shown = format!("{}{caret}", panel.commit_msg);
                    let hint = if panel.commit_msg.is_empty() {
                        "提交信息…  Enter 提交 · Esc 取消"
                    } else {
                        ""
                    };
                    if hint.is_empty() {
                        r.draw_chrome_text(size, sx + s(8.0), strip_ty, sk.ink_strong, &shown, gc);
                    } else {
                        r.draw_chrome_text(size, sx + s(8.0), strip_ty, sk.ink_faint, hint, gc);
                    }
                } else {
                    let busy = panel.op_running();
                    let stage_on = !busy && !git.unstaged.is_empty();
                    let commit_on = !busy && !git.staged.is_empty();
                    let pull_on = !busy;
                    let push_on = !busy && git.ahead > 0;
                    let push_label = if git.ahead > 0 {
                        format!("推送 ↑{}", git.ahead)
                    } else {
                        "推送".to_string()
                    };
                    let labels: [(&str, bool); 4] = [
                        (if busy { "…" } else { "暂存" }, stage_on),
                        ("提交", commit_on),
                        ("拉取", pull_on),
                        (&push_label, push_on),
                    ];
                    for ((bx, bw), (label, enabled)) in
                        git_button_rects(sx, sw, s(6.0)).into_iter().zip(labels)
                    {
                        let hovered = panel.hover == PanelHit::Search
                            && panel.hover_pos.0 >= bx
                            && panel.hover_pos.0 < bx + bw;
                        let cols: usize =
                            label.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
                        let lx = bx + (bw - cols as f32 * cell_w).max(0.0) / 2.0;
                        let ink = if enabled { sk.ink_strong } else { sk.ink_faint };
                        r.draw_chrome_text(
                            size,
                            lx,
                            strip_ty + if hovered { -s(1.0) } else { 0.0 },
                            ink,
                            label,
                            gc,
                        );
                    }
                }

                // Sectioned rows: 未暂存 header, its files, 已暂存 header, its
                // files — one flat scroll space.
                enum GLine<'a> {
                    Header(String),
                    File(char, &'a String),
                }
                let mut lines: Vec<GLine<'_>> = Vec::new();
                if git.unstaged.is_empty() && git.staged.is_empty() {
                    lines.push(GLine::Header("工作区干净".into()));
                } else {
                    lines.push(GLine::Header(format!("未暂存 ({})", git.unstaged.len())));
                    for (c, p) in &git.unstaged {
                        lines.push(GLine::File(*c, p));
                    }
                    lines.push(GLine::Header(format!("已暂存 ({})", git.staged.len())));
                    for (c, p) in &git.staged {
                        lines.push(GLine::File(*c, p));
                    }
                }
                for (i, line) in lines.iter().skip(scroll).take(layout.max_rows).enumerate() {
                    let ry = row_ty(i);
                    match line {
                        GLine::Header(t) => {
                            r.draw_chrome_text(size, px + text_pad, ry, sk.ink_dim, t, gc)
                        },
                        GLine::File(status, path) => {
                            let sc = status_color(*status, is_light).unwrap_or(sk.ink_dim);
                            r.draw_chrome_text(
                                size,
                                px + text_pad,
                                ry,
                                sc,
                                &status.to_string(),
                                gc,
                            );
                            let path_x = px + text_pad + cell_w * 2.0;
                            let path_cols =
                                (((row_text_right - path_x) / cell_w).floor() as usize).max(4);
                            let text = clip_tail(path, path_cols);
                            r.draw_chrome_text(size, path_x, ry, sk.ink, &text, gc);
                        },
                    }
                }
            },
            None => {
                r.draw_chrome_text(
                    size,
                    px + text_pad,
                    summary_y,
                    sk.ink_dim,
                    "不在 git 仓库中",
                    gc,
                );
            },
        },
    }

    if let Some((tooltip, label)) = panel_action_tooltip(panel, layout, scale, cell_w) {
        r.draw_chrome_text(
            size,
            tooltip.0 + s(8.0),
            widgets::centered_y(tooltip.1, tooltip.3, cell_h),
            sk.ink_strong,
            label,
            gc,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_switches_views_without_closing() {
        let mut p = SidePanel::new();
        p.toggle(PanelView::Files);
        assert!(p.open);
        p.toggle(PanelView::Git);
        assert!(p.open, "switching views keeps the drawer open");
        assert_eq!(p.view, PanelView::Git);
        p.toggle(PanelView::Git);
        assert!(!p.open, "re-toggling the current view closes");
    }

    #[test]
    fn sync_noops_while_closed() {
        let mut p = SidePanel::new();
        assert!(!p.sync(Some(std::env::temp_dir())));
    }

    #[test]
    fn custom_root_is_window_local_and_ignores_cwd_sync_until_cleared() {
        let base =
            std::env::temp_dir().join(format!("nebula-panel-root-test-{}", std::process::id()));
        let cwd = base.join("cwd");
        let custom = base.join("custom");
        let next_cwd = base.join("next-cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::create_dir_all(&next_cwd).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(cwd)));
        assert!(panel.set_custom_root(custom.clone()));
        assert!(panel.custom_root_active());

        panel.sync(Some(next_cwd.clone()));
        assert_eq!(panel.root(), Some(custom.as_path()));

        assert!(panel.clear_custom_root());
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(next_cwd.as_path()));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_custom_root_returns_to_latest_cwd_with_visible_feedback() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-missing-root-test-{}", std::process::id()));
        let cwd = base.join("cwd");
        let custom = base.join("custom");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&custom).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        panel.sync(Some(cwd.clone()));
        assert!(panel.set_custom_root(custom.clone()));
        std::fs::remove_dir_all(&custom).unwrap();

        assert!(panel.sync(Some(cwd.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(cwd.as_path()));
        assert_eq!(panel.root_notice(), Some("所选目录不可用，已跟随当前目录"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn invalid_custom_root_refreshes_notice_when_followed_cwd_is_the_same_path() {
        let root = std::env::temp_dir()
            .join(format!("nebula-panel-same-missing-root-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(root.clone())));
        assert!(panel.set_custom_root(root.clone()));
        std::fs::remove_dir_all(&root).unwrap();

        assert!(panel.sync(Some(root.clone())));
        assert!(!panel.custom_root_active());
        assert_eq!(panel.root(), Some(root.as_path()));
        assert_eq!(panel.root_notice(), Some("所选目录不可用，已跟随当前目录"));
    }

    /// The per-directory cap must be applied *after* ordering. Capping the
    /// `read_dir` iterator instead samples filesystem order, which in a
    /// dot-heavy repo root pushes the real source directories past the cap and
    /// leaves the tree showing nothing but `.tmp-*` scratch dirs.
    #[test]
    fn per_directory_cap_keeps_the_ordered_head() {
        let e = |dir: bool, name: &str| (dir, name.to_owned(), PathBuf::from(name));
        let raw = vec![
            e(true, "zz-last-dir"),
            e(true, ".tmp-scratch"),
            e(false, "a.txt"),
            e(true, "nebula_app"),
        ];
        let kept = SidePanel::ordered_entries(raw, 2);
        let names: Vec<_> = kept.iter().map(|(_, name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [".tmp-scratch", "nebula_app"],
            "cap keeps the ordered head (dirs first, alphabetical), not read_dir order"
        );
        // Files still sort behind every directory, and the cap never reorders.
        let all = SidePanel::ordered_entries(vec![e(false, "a.txt"), e(true, "zz-last-dir")], 10);
        assert_eq!(all[0].1, "zz-last-dir", "directories precede files");
    }

    #[test]
    fn tree_lists_dirs_first_and_expands_on_click() {
        let base = std::env::temp_dir().join(format!("nebula-panel-test-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("a.txt"), "x").unwrap();
        std::fs::write(sub.join("inner.txt"), "y").unwrap();

        let mut p = SidePanel::new();
        p.toggle(PanelView::Files);
        assert!(p.sync(Some(base.clone())));
        p.wait_snapshot();
        let rows = p.file_rows();
        assert_eq!(rows[0].name, "..", "parent navigation stays at the top");
        assert!(rows[0].is_parent);
        assert_eq!(rows[1].name, "sub", "directory sorts before file");
        assert!(rows[1].is_dir);
        assert_eq!(rows.len(), 3, "collapsed dir hides children");

        assert!(p.click_row(1), "clicking a dir toggles expansion");
        let rows = p.file_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2].name, "inner.txt");
        assert_eq!(rows[2].depth, 1);

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn ignored_state_does_not_change_existing_tree_order() {
        let mut rows = vec![
            FileRow {
                path: PathBuf::from("src"),
                name: "src".into(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: false,
                ignored: false,
            },
            FileRow {
                path: PathBuf::from("target"),
                name: "target".into(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: false,
                ignored: false,
            },
        ];
        let before: Vec<_> = rows.iter().map(|row| row.name.clone()).collect();
        rows[1].ignored = true;
        let after: Vec<_> = rows.iter().map(|row| row.name.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn parent_row_navigates_the_tree_root_without_becoming_draggable() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-parent-row-test-{}", std::process::id()));
        let child = base.join("child");
        std::fs::create_dir_all(&child).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(child.clone())));
        panel.wait_snapshot();
        let parent = panel.file_rows().first().expect("parent row").clone();
        assert_eq!(parent.name, "..");
        assert!(parent.is_parent);
        assert!(parent.is_dir);
        assert_eq!(parent.path, base);

        let drag = FileDrag::new(parent.path.clone(), parent.name, true, 0, (10.0, 10.0));
        assert!(!panel.click_drag_source(&drag), "the parent row never enters drag dispatch");
        assert!(panel.click_row(0));
        assert_eq!(panel.root(), Some(base.as_path()));
        assert!(panel.custom_root_active(), "upward navigation is window-local");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn directory_drag_defers_and_validates_the_plain_click() {
        let base = std::env::temp_dir()
            .join(format!("nebula-panel-directory-drag-test-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let mut panel = SidePanel::new();
        panel.toggle(PanelView::Files);
        assert!(panel.sync(Some(base.clone())));
        panel.wait_snapshot();
        let drag = FileDrag::new(sub, "sub".into(), true, 1, (10.0, 10.0));

        assert!(panel.click_drag_source(&drag), "a non-drag release keeps directory click");
        assert!(panel.file_rows()[1].expanded);

        let mut active = drag.clone();
        active.update_position((18.0, 10.0));
        assert!(active.active, "eight physical pixels arm the drag");
        assert!(!panel.click_drag_source(&active), "an active drag must not toggle the tree");
        assert!(panel.file_rows()[1].expanded);

        let mut stale = drag;
        stale.source_row = 2;
        assert!(!panel.click_drag_source(&stale), "a changed source row must be ignored");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn terminal_drop_text_requires_an_active_terminal_drop_and_quotes_unicode_whitespace() {
        let path = PathBuf::from("D:/项目 空间");
        let mut drag = FileDrag::new(path, "项目 空间".into(), true, 0, (0.0, 0.0));

        assert_eq!(drag.terminal_drop_text(true), None, "plain clicks never paste");
        drag.update_position((7.0, 7.0));
        assert!(!drag.active, "diagonal motion below each axis threshold remains a click");
        drag.update_position((8.0, 7.0));
        assert!(drag.active);
        assert_eq!(drag.terminal_drop_text(false), None, "dropping back on the drawer is inert");
        assert_eq!(
            String::from_utf8(drag.terminal_drop_text(true).unwrap()).unwrap(),
            "\"D:/项目 空间\" "
        );

        let mut control =
            FileDrag::new(PathBuf::from("unsafe\npath"), "unsafe".into(), true, 0, (0.0, 0.0));
        control.update_position((8.0, 0.0));
        assert_eq!(control.terminal_drop_text(true), None, "a drop must never inject Enter");
    }

    #[test]
    fn hit_test_maps_header_and_rows() {
        let l = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0, PANEL_W_LOGICAL);
        let (px, py, pw, _) = l.panel;
        assert_eq!(panel_hit(&l, px - 1.0, py + 5.0), PanelHit::None);
        assert_eq!(panel_hit(&l, px + 5.0, py + 5.0), PanelHit::Inside);
        assert_eq!(panel_hit(&l, px + 20.0, py + 20.0), PanelHit::ViewFiles);
        assert_eq!(panel_hit(&l, px + pw - 20.0, py + 20.0), PanelHit::ViewGit);
        assert_eq!(panel_hit(&l, px + 5.0, l.list_y + l.row_h * 1.5), PanelHit::Row(1));
    }

    #[test]
    fn git_hover_only_accepts_real_file_rows() {
        let mut panel = SidePanel::new();
        panel.view = PanelView::Git;
        panel.git = Some(GitInfo {
            vcs: VcsKind::Git,
            branch: "main".into(),
            plus: 0,
            minus: 0,
            ahead: 0,
            unstaged: vec![('?', "one.txt".into()), ('M', "two.txt".into())],
            staged: vec![('A', "three.txt".into())],
            conflicts: Vec::new(),
            repository_root: None,
        });

        assert!(!panel.git_row_is_file(0), "未暂存标题");
        assert!(panel.git_row_is_file(1));
        assert!(panel.git_row_is_file(2));
        assert!(!panel.git_row_is_file(3), "已暂存标题");
        assert!(panel.git_row_is_file(4));
        assert!(!panel.git_row_is_file(5), "列表末尾空白行");

        panel.scroll = 2;
        assert!(panel.git_row_is_file(0), "滚动后的真实文件行");
        assert!(!panel.git_row_is_file(1), "滚动后的已暂存标题");
    }

    #[test]
    fn files_summary_actions_have_distinct_exact_hit_targets() {
        let layout = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0, PANEL_W_LOGICAL);
        let actions: Vec<_> = panel_action_rects(&layout, true, true).collect();
        let reveal = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::RevealDirectory)
            .expect("reveal-directory action");
        let follow = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::FollowCurrentDirectory)
            .expect("follow-current-directory action");
        let terminal = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::NewTerminalHere)
            .expect("new-terminal-here action");
        let center = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        let (reveal_x, reveal_y) = center(reveal.1);
        let (follow_x, follow_y) = center(follow.1);
        let (terminal_x, terminal_y) = center(terminal.1);
        let (directory_x, directory_y) = center(panel_tools_layout(&layout).directory);

        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, directory_x, directory_y),
            PanelHit::OpenDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, reveal_x, reveal_y),
            PanelHit::RevealDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, terminal_x, terminal_y),
            PanelHit::NewTerminalHere
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, follow_x, follow_y),
            PanelHit::FollowCurrentDirectory
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, false, true, follow_x, follow_y),
            PanelHit::FollowCurrentDirectory,
            "the active follow control keeps its stable hit target"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, false, false, terminal_x, terminal_y),
            PanelHit::Inside,
            "the terminal action must not exist without a tree root"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Git, true, true, reveal_x, reveal_y),
            PanelHit::Inside,
            "Files-only actions must not create invisible Git hit targets"
        );

        for (index, (_, a)) in actions.iter().enumerate() {
            for (_, b) in actions.iter().skip(index + 1) {
                let overlaps =
                    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1;
                assert!(!overlaps, "summary action hit targets must not overlap");
            }
        }
    }

    #[test]
    fn self_drawn_fields_replace_select_all_on_paste() {
        let mut panel = SidePanel::new();
        panel.search_input("old");
        panel.search_select_all();
        assert_eq!(panel.search_selected_text().as_deref(), Some("old"));
        panel.search_input("new\nvalue");
        assert_eq!(panel.search, "newvalue");

        panel.commit_input("old commit");
        panel.commit_select_all();
        assert_eq!(panel.commit_selected_text().as_deref(), Some("old commit"));
        panel.commit_input("new commit");
        assert_eq!(panel.commit_msg, "new commit");
    }

    #[test]
    fn git_action_strip_has_four_equal_buttons() {
        let rects = git_button_rects(10.0, 430.0, 10.0);
        assert_eq!(rects.len(), 4);
        assert!(rects.windows(2).all(|pair| (pair[0].1 - pair[1].1).abs() < f32::EPSILON));
        let last = rects.last().expect("at least one git action");
        assert!((last.0 + last.1 - 440.0).abs() < f32::EPSILON);
    }

    #[test]
    fn git_pull_is_fast_forward_only() {
        assert_eq!(git_pull_args(), vec!["pull", "--ff-only"]);
    }

    #[test]
    fn svn_status_parses_item_state_and_normalizes_separators() {
        let status = "M       src\\main.rs\nA       docs/new.md\n?       target\n!       gone.rs\n        props-only-ignored\n";
        let changes = parse_svn_status(status);
        assert_eq!(
            changes,
            vec![
                ('M', "src/main.rs".to_owned()),
                ('A', "docs/new.md".to_owned()),
                ('?', "target".to_owned()),
                ('!', "gone.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn svn_info_revision_parser_accepts_standard_output() {
        let info = "Path: .\r\nWorking Copy Root Path: D:\\checkout\r\nRevision: 42\r\nNode Kind: directory\r\n";
        assert_eq!(parse_svn_revision(info).as_deref(), Some("42"));
        assert_eq!(parse_svn_revision("Path: .\nNode Kind: directory\n"), None);
    }

    #[test]
    fn svn_snapshot_disables_stage_and_push_semantics() {
        // SVN 没有暂存区、没有 push：快照层用 staged 恒空 + ahead 恒 0 编码，
        // 两壳按钮的既有 gating（staged/ahead）不改一行就得到正确禁用。
        let info = GitInfo {
            vcs: VcsKind::Svn,
            branch: "r42".into(),
            unstaged: vec![('M', "a.rs".into())],
            ..GitInfo::default()
        };
        assert!(info.staged.is_empty());
        assert_eq!(info.ahead, 0);
    }

    #[test]
    fn svn_snapshot_separates_addable_and_committable_changes() {
        let only_unversioned = GitInfo {
            vcs: VcsKind::Svn,
            unstaged: vec![('?', "new.txt".into())],
            ..GitInfo::default()
        };
        assert!(only_unversioned.svn_add_ready());
        assert!(!only_unversioned.svn_commit_ready());

        let versioned = GitInfo {
            vcs: VcsKind::Svn,
            unstaged: vec![('M', "tracked.txt".into()), ('C', "conflict.txt".into())],
            ..GitInfo::default()
        };
        assert!(!versioned.svn_add_ready());
        assert!(versioned.svn_commit_ready());
    }

    #[test]
    fn svn_repository_snapshot_keeps_the_ancestor_root() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        for directory in ["conf", "db/revs", "hooks"] {
            std::fs::create_dir_all(repository.join(directory)).unwrap();
        }
        std::fs::write(repository.join("format"), "8\n").unwrap();

        let info = read_svn(&repository.join("db/revs")).expect("repository snapshot");
        assert_eq!(info.vcs, VcsKind::SvnRepository);
        assert_eq!(info.repository_root.as_deref(), Some(repository.as_path()));
        assert!(info.unstaged.is_empty());
    }

    #[test]
    fn svn_commands_keep_paths_and_messages_as_separate_arguments() {
        let path = PathBuf::from(r"D:\工作副本\src\main.rs");
        let cli = SvnMutation::Resolve(path.clone()).cli_args();
        assert_eq!(
            cli,
            vec![
                OsString::from("resolve"),
                OsString::from("--accept"),
                OsString::from("working"),
                OsString::from("--"),
                path.as_os_str().to_owned(),
            ]
        );

        let commit = SvnMutation::Commit("修复空格 ; $(echo nope)".into())
            .tortoise_args(Path::new(r"D:\工作副本"));
        assert_eq!(commit[0], OsString::from("/command:commit"));
        assert_eq!(commit[1], OsString::from(r"/path:D:\工作副本"));
        assert_eq!(commit[2], OsString::from("/logmsg:修复空格 ; $(echo nope)"));
    }

    #[test]
    fn tortoise_checkout_uses_an_encoded_local_repository_url() {
        let repository = PathBuf::from("D:/新建 文件夹");
        assert_eq!(
            local_repository_url(&repository),
            "file:///D:/%E6%96%B0%E5%BB%BA%20%E6%96%87%E4%BB%B6%E5%A4%B9"
        );
        assert_eq!(
            SvnVisual::CheckoutRepository(repository).tortoise_args(),
            vec![
                OsString::from("/command:checkout"),
                OsString::from("/url:file:///D:/%E6%96%B0%E5%BB%BA%20%E6%96%87%E4%BB%B6%E5%A4%B9"),
            ]
        );
    }

    #[test]
    fn svn_relative_targets_cannot_escape_the_visible_root() {
        let root = Path::new(r"D:\checkout");
        assert_eq!(svn_relative_target(root, "src/main.rs"), Some(root.join("src/main.rs")));
        assert!(svn_relative_target(root, "../outside.txt").is_none());
        assert!(svn_relative_target(root, r"D:\outside.txt").is_none());
    }
}
