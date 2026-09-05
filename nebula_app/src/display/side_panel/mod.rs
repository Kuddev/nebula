//! Right-side drawer: directory tree / git status for the focused pane's cwd
//! This module owns only the *model* — tree flattening, git
//! parsing, layout maths, and hit-testing. Rendering lives in `display::mod`
//! (mirroring the command palette split), and input dispatch in `input::mod`.
//!
//! The panel is an overlay drawer: it floats above the terminal's right edge
//! instead of reflowing the PTY, so toggling it never resizes the shell.
//!
//! Tree/Git snapshots refresh in the background on toggle, root changes, or
//! explicit refresh. The filename index survives those snapshots and follows
//! filesystem notifications. `git --no-optional-locks` avoids touching
//! the index lock, so it can't corrupt or stall a concurrent git operation.
//!
//! 2026-08-31 拆分（原单文件 4031 行，远超 2000 行红线）：本模块留面板状态机，
//! VCS 数据层去 [`vcs`]、目录枚举去 [`enumerate`]、落笔去 [`render`]。子模块
//! 一律 `use super::*` 取本模块的类型与常量，反向则靠下面的 glob 重导出，
//! 于是 `display::side_panel::X` 这一层外部路径在拆分前后完全不变。

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

mod enumerate;
mod icons;
#[cfg(feature = "legacy-shell")]
mod render;
mod search;
#[cfg(test)]
mod tests;
mod vcs;

// 子模块的项一律 `pub(crate)`：本 crate 是 bin，没有下游用户，所以拆分不必
// 把内部实现抬到 `pub`。glob 转发让 `display::side_panel::X` 这层路径不变。
pub(crate) use enumerate::*;
pub(crate) use icons::*;
#[cfg(feature = "legacy-shell")]
pub(crate) use render::*;
pub(crate) use search::*;
pub(crate) use vcs::*;

/// Which view the drawer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelView {
    /// Directory tree of the focused pane's cwd.
    Files,
    /// Git branch + working-tree changes of the enclosing repository.
    Git,
}

/// Git 抽屉内部的三个等宽入口。它与 [`PanelView`] 分层：后者只负责文件/VCS
/// 工具切换，这一层才负责提交、线路和冲突三种版本控制工作流。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GitPanelView {
    #[default]
    Changes,
    History,
    Conflicts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitCommit {
    /// 完整对象 ID 用来建立真实父子关系；短哈希只用于界面显示。
    pub full_hash: String,
    pub short_hash: String,
    pub decorations: String,
    pub subject: String,
    pub author: String,
    pub timestamp: i64,
    /// 按 Git 记录顺序保存父提交：第一个是主线，后续父提交是合并进来的线路。
    pub parent_hashes: Vec<String>,
}

/// 三栏合并器运行 Git 的位置。WSL 仓库必须留在来宾中执行，不能把 `/home`
/// 一类路径误交给宿主文件 API。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitLocation {
    Local { root: PathBuf },
    Wsl { distro: String, root: String },
}

/// One flattened row of the directory tree.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub path: PathBuf,
    /// WSL 来宾中的真实绝对路径。`path` 仍作为现有选择/展开状态的稳定键，
    /// 但绝不能把它交给宿主文件 API；来宾操作必须显式使用这一字段。
    pub guest_path: Option<String>,
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
    /// `git log --all --date-order` 的最近提交。显示层依据对象 ID 和父提交
    /// 生成连续轨道，不把 `git --graph` 的字符画当成视觉数据。
    pub history: Vec<GitCommit>,
    /// 仅 [`VcsKind::SvnRepository`] 使用；工作副本和 Git 的操作目录取
    /// [`SidePanel::vcs_root`]，服务端仓库必须保留祖先扫描得到的真实根。
    pub repository_root: Option<PathBuf>,
    /// 仅 [`VcsKind::SvnRepository`]：版本库摘要（HEAD 修订号、UUID、格式号、
    /// 体积、顶层布局、最后一条提交）。这些事实全躺在 `db/` 下的纯文本文件
    /// 里，读它们不需要 svn 客户端，也不需要 spawn 任何进程。
    pub repository: Option<crate::svn_status::RepositorySummary>,
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
        drop_text_for_path(&self.path.display().to_string())
    }
}

/// 一条路径落进 PTY 时该写出的字节。`None` = 这条路径不能安全地写进去。
///
/// 两个壳共用这一层。旧壳自己追踪按压阈值与命中（[`FileDrag`]），GPUI 壳用
/// 引擎原生的拖放，但"什么样的路径可以写、写成什么形状"必须只有一处定义：
/// 少了控制字符那道闸，一个换行就能让"粘贴路径"变成"执行命令"。
///
/// 参数是字符串而不是 `Path`：WSL 行要写的是**来宾**路径（`/home/x`），它在
/// 宿主上不是一个有效的 `Path`，转一圈只会被 Windows 的路径语义改写。
pub fn drop_text_for_path(path: &str) -> Option<Vec<u8>> {
    let mut text = path.to_owned();
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

/// WSL 子命令预算。
///
/// 原值是 6s，注释里的假设是"抽屉要快照时 WSL 终端已经在跑，等更久说明工人卡死"
/// ——2026-08-21 实测推翻了这个假设。`wsl.exe -d Debian -- find /` 的耗时**高度
/// 可变**：同一台机器上量到 346ms（VM 热）、7.5s（VM 热但要建新会话）、以及
/// >20s（WSL2 的 VM 因空闲被整个关闭后冷启）。也就是说任何"短"预算都会在某些
/// 时刻必然超时。
///
/// 超时的代价原本被完全隐藏：工人被 kill、[`wsl_read_dirs`] 返回空列表、UI 把空
/// 结果显示成"此目录为空"——这就是 WSL 文件树空白的真正根因。
///
/// 现在预算给足，并且**失败不再伪装成空目录**（见 [`SidePanel::enumeration_failed`]）。
/// 工人跑在后台线程，放宽预算不卡 UI；`snapshot_running` 保证同一时刻只有一个
/// 工人；期间面板显示"正在读取目录…"，这是诚实的反馈。
const WSL_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
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
    /// Git 抽屉内部当前页；切换文件/VCS 抽屉不会丢掉用户上次所在的页。
    pub git_view: GitPanelView,
    /// Root the tree/git snapshot was built from (the focused pane's cwd).
    root: Option<PathBuf>,
    /// Latest focused pane cwd, retained while a custom root is active so the
    /// panel can resume following immediately without persisting any setting.
    followed_cwd: Option<PathBuf>,
    /// 聚焦终端是 WSL 且停在来宾目录时的位置。宿主未必看得见 WSL 文件系统
    /// （9P 重定向不一定可用），所以这不是"另一种 cwd"——它是让 Git 视图
    /// 改用「在来宾里跑 git」的开关，与 [`Self::followed_cwd`] 各管一段。
    followed_wsl: Option<crate::shell_detect::WslCwd>,
    /// Window-local override selected from the Files view.
    custom_root: Option<PathBuf>,
    /// WSL guest tree 的窗口内浏览根（例如点 `..` 后得到的 `/home`）。与
    /// `custom_root` 互斥，且不改变终端实际 cwd / Git 跟随位置。
    custom_wsl_root: Option<crate::shell_detect::WslCwd>,
    /// Visible feedback for an invalid/disappeared custom root.
    root_notice: Option<String>,
    /// Flattened visible tree rows for the Files view.
    rows: Vec<FileRow>,
    /// Last unfiltered tree snapshot. Search results replace `rows`, but
    /// clearing the query restores this cache immediately instead of walking
    /// the filesystem (or starting another WSL command) on the UI thread.
    tree_rows: Vec<FileRow>,
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
    /// Everything-style, root-scoped filename index. It owns a worker thread;
    /// rendering only submits queries and harvests generation-checked rows.
    file_index: EmbeddedFileIndex,
    search_options: FileSearchOptions,
    search_generation: u64,
    search_applied_generation: u64,
    search_index_epoch: u64,
    search_index_root: Option<FileIndexRoot>,
    search_index_refresh_requested: bool,
    search_total: usize,
    search_error: Option<String>,
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
    /// 上一份落地快照的枚举是否失败（WSL 超时 / find 非零退出）。UI 靠它区分
    /// "读不到"和"目录真的是空的"。
    enumeration_failed: bool,
    needs_refresh: bool,
}

/// What the background snapshot worker produces: everything `refresh` used to
/// compute synchronously on the render thread.
struct PanelSnapshot {
    /// Root the snapshot was built from — stale snapshots (root changed while
    /// the worker ran) are dropped on harvest.
    root: PathBuf,
    /// Files 快照所属的来宾位置；避免两个发行版恰好同为 `/home` 时串结果。
    files_wsl: Option<crate::shell_detect::WslCwd>,
    rows: Vec<FileRow>,
    /// 这次枚举是否全部成功。WSL 冷启动可能耗尽预算被 kill，那时 `rows` 是空的
    /// 但**不代表目录是空的**——UI 必须能区分，否则会把"读不到"说成"此目录为空"。
    enumeration_ok: bool,
    /// `None` 表示目录行先行发布、VCS 仍在读取；`Some(None)` 才表示当前
    /// 目录不在仓库中。这样慢 WSL git 不会让 Files 一直显示空白。
    git: Option<Option<GitInfo>>,
}

/// 工人 panic/提前返回也必须解锁；否则一次异常会让此窗口今后的所有刷新失效。
struct SnapshotRunningGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for SnapshotRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl SidePanel {
    pub fn new() -> Self {
        Self {
            open: false,
            view: PanelView::Files,
            git_view: GitPanelView::Changes,
            root: None,
            followed_cwd: None,
            followed_wsl: None,
            custom_root: None,
            custom_wsl_root: None,
            root_notice: None,
            rows: Vec::new(),
            tree_rows: Vec::new(),
            expanded: HashSet::new(),
            git: None,
            scroll: 0,
            search: String::new(),
            search_focus: false,
            search_selection: Default::default(),
            file_index: EmbeddedFileIndex::new(),
            search_options: FileSearchOptions::default(),
            search_generation: 0,
            search_applied_generation: 0,
            search_index_epoch: 0,
            search_index_root: None,
            search_index_refresh_requested: false,
            search_total: 0,
            search_error: None,
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
            enumeration_failed: false,
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

    /// Adopt the focused pane's cwd, refreshing when the root changed or a
    /// refresh was requested (toggle).
    /// Called once per drawn frame from the window context; cheap when nothing
    /// changed. Returns whether the snapshot was rebuilt (i.e. needs redraw).
    ///
    /// 抽屉内部的刷新（文件操作后、根切换后）走这一层就够；跟随聚焦终端的
    /// 调用方请用 [`Self::sync_at`] 一并交出 WSL 位置。
    pub fn sync(&mut self, cwd: Option<PathBuf>) -> bool {
        self.sync_at(cwd, None)
    }

    /// [`Self::sync`] 加上 WSL 位置。`wsl` = 聚焦终端所在的 WSL 发行版与来宾
    /// 目录（[`crate::shell_detect::wsl_cwd`] 解析），`None` = 聚焦的是宿主
    /// 终端或压根认不出发行版。
    pub fn sync_at(
        &mut self,
        cwd: Option<PathBuf>,
        wsl: Option<crate::shell_detect::WslCwd>,
    ) -> bool {
        if !self.open {
            return false;
        }
        // 先收割落地的后台快照——旧内容在工人跑动期间一直显示，这里一次
        // 性换成新内容（先显示旧的、再更新，VSCode 的树刷新模式）。
        let mut changed = self.harvest_snapshot();
        changed |= self.harvest_file_search();
        // 聚焦 pane 报不出位置时（SSH、shell 尚未发 OSC）保留最后一个有效根；
        // 有明确新位置时则恢复实时跟随，树内点 `..` 产生的浏览覆盖不能继续压住
        // 新 pane/cwd。相同 cwd 上的手动浏览仍保留，直到位置真的变化或用户刷新。
        let followed_changed = match (cwd.as_ref(), wsl.as_ref()) {
            (Some(next), _) => {
                self.followed_cwd.as_ref() != Some(next) || self.followed_wsl.is_some()
            },
            (None, Some(next)) => {
                self.followed_wsl.as_ref() != Some(next) || self.followed_cwd.is_some()
            },
            (None, None) => false,
        };
        let follow_override_cleared = if followed_changed {
            let cleared_host = self.custom_root.take().is_some();
            let cleared_wsl = self.custom_wsl_root.take().is_some();
            cleared_host || cleared_wsl
        } else {
            false
        };
        if follow_override_cleared {
            self.root_notice = None;
        }

        let wsl_before = self.followed_wsl.clone();
        if let Some(cwd) = cwd {
            self.followed_cwd = Some(cwd);
            self.followed_wsl = None;
        } else if let Some(wsl) = wsl {
            // `/mnt/d` 能映射为 D:\，而 `/home` 等必须由来宾读取；清掉旧宿主根
            // 才不会让 WSL `/` 再次被上一份 Windows 快照覆盖。
            self.followed_cwd = None;
            self.followed_wsl = Some(wsl);
        }
        // 换了 WSL 仓库、或从 WSL 切回宿主，都要立刻重算 Git 快照：这类切换
        // 不改 `root`（宿主看不见来宾目录，树停在原处），光靠 `root_changed`
        // 抓不到，只等节流窗口的话面板会挂着上一个仓库的分支名不动。
        let wsl_changed = wsl_before != self.followed_wsl;
        let custom_invalidated = self.custom_root.as_ref().is_some_and(|root| !root.is_dir());
        if custom_invalidated {
            self.custom_root = None;
            self.root_notice = Some("所选目录不可用，已跟随当前目录".to_owned());
        }
        let next_root = self
            .custom_root
            .clone()
            .or_else(|| self.custom_wsl_root.as_ref().map(wsl_root_key))
            .or_else(|| self.followed_tree_root());
        let root_changed = next_root != self.root;
        // A finished git mutation forces a refresh so the new state (staged
        // list, ahead count) shows on the next frame.
        if self.op_done.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.request_refresh();
        }
        // 目录内容**不做定时重扫**。这里原先有一条 `stale`（每 4 秒无条件重跑
        // 工人），代价是每 4 秒重新拉一遍 WSL 子进程 + 三个 git 子进程；配上 WSL
        // 冷路径要 7.5s，面板就在"正在读取目录…"和结果之间反复闪——2026-08-21
        // 用户裁定：目录识别不要轮询。
        //
        // 剩下的触发点全是明确事件：cwd/根变化、手动刷新按钮（`needs_refresh`）、
        // git 操作完成（`op_done`）、浏览覆盖失效。终端里新建/删除文件不再自动
        // 反映，由刷新按钮兜底——这是这条裁定的显式代价。
        if !(root_changed
            || custom_invalidated
            || follow_override_cleared
            || wsl_changed
            || self.needs_refresh)
        {
            return changed;
        }
        if root_changed {
            self.root = next_root;
            self.expanded.clear();
            self.scroll = 0;
            self.selected = None;
        }
        self.refresh();
        changed || root_changed || custom_invalidated || follow_override_cleared || wsl_changed
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
        let snapshot = match self.snapshot_slot.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        let Some(snapshot) = snapshot else { return false };
        // 工人跑动期间根又变了：这份快照已经过期，丢弃。新刷新在路上。
        if self.root.as_ref() != Some(&snapshot.root)
            || self.file_wsl_root().cloned() != snapshot.files_wsl
        {
            // 当前调用紧接着会看到 needs_refresh 并为新根启动工人；不能静默
            // 丢弃后等四秒节流，否则 pane 切换看起来像没有跟随。
            self.needs_refresh = true;
            return false;
        }
        self.tree_rows = snapshot.rows;
        if self.search.trim().is_empty() {
            self.rows.clone_from(&self.tree_rows);
        }
        self.enumeration_failed = !snapshot.enumeration_ok;
        if let Some(git) = snapshot.git {
            self.git = git;
        }
        true
    }

    fn harvest_file_search(&mut self) -> bool {
        let Some(result) = self.file_index.take_result() else { return false };
        if result.epoch != self.search_index_epoch
            || self.search_index_root != self.current_index_root()
            || result.generation != self.search_generation
            || result.query != self.search
            || result.options != self.search_options
        {
            return false;
        }
        self.rows = result.rows;
        self.search_total = result.total;
        self.search_error = result.error;
        self.search_applied_generation = result.generation;
        self.scroll = 0;
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
        self.custom_wsl_root = None;
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
        let cleared_host = self.custom_root.take().is_some();
        let cleared_wsl = self.custom_wsl_root.take().is_some();
        if !cleared_host && !cleared_wsl {
            return false;
        }
        self.root_notice = None;
        let next_root = self.followed_tree_root();
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
        self.custom_root.is_some() || self.custom_wsl_root.is_some()
    }

    /// Files 视图当前是否由 WSL guest 提供。宿主可达的 `/mnt/<盘>` 与 UNC
    /// 不会进入这里，仍保留完整的宿主打开、回收站和 Explorer 操作。
    pub fn file_wsl_root(&self) -> Option<&crate::shell_detect::WslCwd> {
        self.custom_wsl_root.as_ref().or_else(|| {
            (self.custom_root.is_none() && self.followed_cwd.is_none())
                .then_some(self.followed_wsl.as_ref())
                .flatten()
        })
    }

    fn followed_tree_root(&self) -> Option<PathBuf> {
        self.followed_cwd.clone().or_else(|| self.followed_wsl.as_ref().map(wsl_root_key))
    }

    fn set_custom_wsl_root(&mut self, located: crate::shell_detect::WslCwd) -> bool {
        let root = wsl_root_key(&located);
        let changed =
            self.custom_wsl_root.as_ref() != Some(&located) || self.root.as_ref() != Some(&root);
        self.custom_root = None;
        self.custom_wsl_root = Some(located);
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

    /// 目录：Git/SVN 状态所属的那一个。始终是终端当前目录，与目录树的浏览
    /// 位置（`custom_root`）无关——`refresh` 就是按这个路径抓 git 的，路径摘要
    /// 必须显示同一个值，否则用户会看到「树在 A、状态是 B」的错位。
    /// VCS 视图与写操作的作用目录。
    ///
    /// 优先级：显式浏览定位（`custom_root`）→ 终端 cwd → 树根。
    ///
    /// 2026-08-31 把 `custom_root` 提到最前。原先它排在 `followed_cwd` 之后，
    /// 于是"在侧栏打开一个 SVN 工作副本/版本库"这件事对 VCS 视图完全无效——
    /// 面板照旧显示终端 cwd 所在的 Git 仓库，用户看到的就是「小乌龟建的目录
    /// 识别不上」。SVN 用户尤其撞得多：TortoiseSVN 是资源管理器工作流，本来
    /// 就没有"先 cd 过去"的习惯。翻出仓库外时视图会变空，这由 VCS 视图自己
    /// 画的「跟随当前目录」入口兜回来（[`Self::custom_root_active`]）。
    pub fn vcs_root(&self) -> Option<&Path> {
        self.custom_root.as_deref().or(self.followed_cwd.as_deref()).or(self.root.as_deref())
    }

    /// 当前 Git 仓库的执行位置，供工作区里的三栏合并 Tab 使用。
    pub fn git_location(&self) -> Option<GitLocation> {
        if self.vcs() != Some(VcsKind::Git) {
            return None;
        }
        if let Some(located) = self.followed_wsl.as_ref() {
            return Some(GitLocation::Wsl {
                distro: located.distro.clone(),
                root: located.guest.clone(),
            });
        }
        self.vcs_root().map(|root| GitLocation::Local { root: root.to_path_buf() })
    }

    pub fn select_git_view(&mut self, view: GitPanelView) {
        self.git_view = view;
        self.selected = None;
        self.scroll = 0;
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
        self.search_index_refresh_requested = true;
    }

    /// 面板顶部的一句话提示（复用根目录不可用的同一条 UI）。
    pub fn set_notice(&mut self, message: String) {
        self.root_notice = Some(message);
    }

    fn current_index_root(&self) -> Option<FileIndexRoot> {
        self.root.as_ref().map(|root| {
            self.file_wsl_root()
                .cloned()
                .map(FileIndexRoot::Wsl)
                .unwrap_or_else(|| FileIndexRoot::Local(root.clone()))
        })
    }

    fn sync_file_index(&mut self) {
        let root = self.current_index_root();
        let refresh_requested = std::mem::take(&mut self.search_index_refresh_requested);
        if self.search_index_root == root {
            if refresh_requested {
                self.file_index.refresh(self.search_index_epoch);
            }
            return;
        }
        self.search_index_root = root.clone();
        self.search_index_epoch = self.search_index_epoch.wrapping_add(1);
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_error = None;
        self.search_total = 0;
        let active_query = if self.search.trim().is_empty() {
            None
        } else {
            self.rows.clear();
            Some((self.search_generation, self.search.clone(), self.search_options))
        };
        self.file_index.rebuild(root, self.search_index_epoch, active_query);
    }

    /// Rebuild the tree and git snapshot from `root`.
    fn refresh(&mut self) {
        self.needs_refresh = false;
        let Some(root) = self.root.clone() else {
            // 没有根：清空是即时且无成本的，不需要工人。
            self.rows.clear();
            self.tree_rows.clear();
            self.git = None;
            self.sync_file_index();
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
        let files_wsl = self.file_wsl_root().cloned();
        self.sync_file_index();
        // VCS 状态跟着 [`Self::vcs_root`]：显式浏览定位优先，其次终端 cwd。
        // 在树里点 `..` 往上翻也算显式定位，所以翻到仓库外面时视图会变空——
        // 那条回头路由 VCS 视图自己画的「跟随当前目录」入口负责（此前它只
        // 画在 Files 视图里，才不得不让 VCS 状态死盯终端 cwd）。
        let git_root = self
            .custom_root
            .clone()
            .or_else(|| self.followed_cwd.clone())
            .unwrap_or_else(|| root.clone());
        let wsl = self.followed_wsl.clone();
        let running = std::sync::Arc::clone(&self.snapshot_running);
        let slot = std::sync::Arc::clone(&self.snapshot_slot);
        std::thread::spawn(move || {
            let _running_guard = SnapshotRunningGuard(running);
            let (rows, enumeration_ok) = match &files_wsl {
                Some(located) => SidePanel::tree_rows_wsl(located, &expanded),
                None => (SidePanel::tree_rows(&root, &expanded), true),
            };
            // 文件列表是 Files 的主结果，不能被随后可能耗满超时预算的 WSL
            // git 探测扣住。先发布目录行；VCS 完成后再用同一行集补全快照。
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(PanelSnapshot {
                    root: root.clone(),
                    files_wsl: files_wsl.clone(),
                    rows: rows.clone(),
                    enumeration_ok,
                    git: None,
                });
            }
            // 设置可强制只认 Git / SVN（混合仓库场景）；Auto 保持既有探测：
            // a checkout nested inside a Git tree must remain visible as SVN.
            // Prefer SVN only when its metadata is in the current path's
            // ancestor chain; ordinary Git directories keep the cheaper Git
            // first path and only probe SVN as a fallback.
            let git = match &wsl {
                // 聚焦的是 WSL 终端：宿主的 `git_root` 与来宾目录毫无关系
                // （UNC 不可达时它还停在上一个宿主目录），必须在来宾里跑
                // git。SVN 不走这条——WSL 里用 svn 没有实据，不加没验证过
                // 的分支；`vcs_display` 强制 Svn 时同理保持"无仓库"。
                Some(located)
                    if !matches!(
                        nebula_settings::RuntimeSettings::load().vcs_display,
                        nebula_settings::VcsDisplayName::Svn
                    ) =>
                {
                    read_git_wsl(located)
                },
                Some(_) => None,
                None => match nebula_settings::RuntimeSettings::load().vcs_display {
                    nebula_settings::VcsDisplayName::Git => read_git(&git_root),
                    nebula_settings::VcsDisplayName::Svn => read_svn(&git_root),
                    nebula_settings::VcsDisplayName::Auto => {
                        if svn_dir_hint(&git_root) {
                            read_svn(&git_root).or_else(|| read_git(&git_root))
                        } else {
                            read_git(&git_root).or_else(|| read_svn(&git_root))
                        }
                    },
                },
            };
            if let Ok(mut slot) = slot.lock() {
                *slot =
                    Some(PanelSnapshot { root, files_wsl, rows, enumeration_ok, git: Some(git) });
            }
        });
    }

    /// Rebuild only the flattened rows (tree shape / filter changes; the git
    /// snapshot stays).
    fn rebuild_rows(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if !self.search.trim().is_empty() {
            self.queue_file_search();
            return;
        }
        match self.file_wsl_root() {
            Some(located) => {
                let (rows, ok) = Self::tree_rows_wsl(located, &self.expanded);
                self.tree_rows = rows;
                self.enumeration_failed = !ok;
            },
            None => {
                self.tree_rows = Self::tree_rows(&root, &self.expanded);
                self.enumeration_failed = false;
            },
        }
        self.rows.clone_from(&self.tree_rows);
    }

    fn queue_file_search(&mut self) {
        self.scroll = 0;
        self.search_error = None;
        self.search_total = 0;
        self.search_generation = self.search_generation.wrapping_add(1);
        if self.search.trim().is_empty() {
            self.file_index.clear_query();
            self.rows.clone_from(&self.tree_rows);
            self.search_applied_generation = self.search_generation;
            return;
        }
        self.file_index.query(
            self.search_index_epoch,
            self.search_generation,
            self.search.clone(),
            self.search_options,
        );
    }

    /// Replace the GPUI Files query. The input component owns caret/IME state;
    /// the shared model owns only sanitized text and indexed results.
    pub fn set_file_search_query(&mut self, mut query: String) {
        query.retain(|character| !matches!(character, '\r' | '\n'));
        if self.search == query {
            return;
        }
        self.search = query;
        self.search_selection.clear();
        self.queue_file_search();
    }

    pub fn file_search_options(&self) -> FileSearchOptions {
        self.search_options
    }

    pub fn toggle_file_search_match_case(&mut self) {
        self.search_options.match_case = !self.search_options.match_case;
        self.queue_file_search();
    }

    pub fn toggle_file_search_whole_word(&mut self) {
        self.search_options.whole_word = !self.search_options.whole_word;
        self.queue_file_search();
    }

    pub fn toggle_file_search_regex(&mut self) {
        self.search_options.regex = !self.search_options.regex;
        self.queue_file_search();
    }

    pub fn file_search_pending(&self) -> bool {
        !self.search.trim().is_empty()
            && (self.file_index.status() == FileIndexStatus::Building
                || self.search_applied_generation != self.search_generation)
    }

    pub fn file_index_status(&self) -> FileIndexStatus {
        self.file_index.status()
    }

    pub fn file_indexed_count(&self) -> usize {
        self.file_index.indexed_count()
    }

    pub fn file_index_truncated(&self) -> bool {
        self.file_index.truncated()
    }

    pub fn file_search_total(&self) -> usize {
        self.search_total
    }

    pub fn file_search_error(&self) -> Option<&str> {
        self.search_error.as_deref()
    }

    /// Append typed text to the filter query and re-derive the rows.
    pub fn search_input(&mut self, text: &str) {
        self.search_selection.insert(&mut self.search, text);
        self.queue_file_search();
    }

    pub fn search_backspace(&mut self) {
        self.search_selection.backspace(&mut self.search);
        self.queue_file_search();
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
            self.queue_file_search();
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
                guest_path: None,
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

    /// UNC 不可达时直接从 WSL guest 枚举 Files 树。命令参数逐项传给
    /// `CreateProcessW`，不经过 shell，因此发行版名、路径和文件名都不会参与
    /// 命令拼接。`find -printf` 用 NUL 分隔，换行文件名也不会破坏记录边界。
    /// 返回 `(行, 枚举是否全部成功)`。失败必须往上传，否则 UI 只能看到一个空
    /// 列表、把"读不到"说成"此目录为空"。
    ///
    /// 整棵树只发**一条** `find`，见 [`wsl_read_dirs`]。
    fn tree_rows_wsl(
        located: &crate::shell_detect::WslCwd,
        expanded: &HashSet<PathBuf>,
    ) -> (Vec<FileRow>, bool) {
        let root = normalize_wsl_guest_path(&located.guest);
        let mut rows = Vec::new();
        if let Some(parent) = wsl_guest_parent(&root) {
            rows.push(FileRow {
                path: PathBuf::from(&parent),
                guest_path: Some(parent),
                name: "..".to_owned(),
                depth: 0,
                is_dir: true,
                expanded: false,
                is_parent: true,
                ignored: false,
            });
        }
        let dirs = wsl_dirs_to_list(&root, expanded);
        let Some(listing) = wsl_read_dirs(&located.distro, &dirs) else {
            return (rows, false);
        };
        Self::flatten_wsl_dir_into(&mut rows, expanded, &listing, &root, 0);
        // 非零退出通常只是某个已展开的子目录在两帧之间被删了——那一条起点报错，
        // 其余起点的输出照样在 stdout 里。只有**根**也没读出条目时才是真失败：
        // 根为空的目录不可能有已展开的后代，所以这时起点只有根一个，非零退出
        // 就等于根读不到。
        let ok = listing.exit_ok || listing.by_dir.contains_key(&root);
        (rows, ok)
    }

    /// 把一趟 `find` 的结果摊平成树行。已经没有子进程了：所有层的条目都在
    /// `listing` 里，递归只是按 `expanded` 挑出要展开的桶。
    fn flatten_wsl_dir_into(
        rows: &mut Vec<FileRow>,
        expanded: &HashSet<PathBuf>,
        listing: &WslDirListing,
        guest_dir: &str,
        depth: usize,
    ) {
        if rows.len() >= MAX_ROWS {
            return;
        }
        let Some(entries) = listing.by_dir.get(guest_dir) else { return };
        for (is_dir, name, guest_path) in entries {
            if rows.len() >= MAX_ROWS {
                return;
            }
            // 该 PathBuf 只作为展开/选中的稳定键；真实来宾路径始终取
            // `guest_path`，绝不把这个键交给宿主文件系统。
            let path_key = PathBuf::from(guest_path);
            let is_expanded = *is_dir && expanded.contains(&path_key);
            rows.push(FileRow {
                path: path_key,
                guest_path: Some(guest_path.clone()),
                name: name.clone(),
                depth,
                is_dir: *is_dir,
                expanded: is_expanded,
                is_parent: false,
                ignored: false,
            });
            if is_expanded {
                Self::flatten_wsl_dir_into(rows, expanded, listing, guest_path, depth + 1);
            }
        }
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
                guest_path: None,
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
        let guest_path = row.guest_path.clone();
        if row.is_parent {
            // 返回上级只改变窗口内的目录树根节点，不能连带修改终端 cwd；
            // 用户仍可通过现有“跟随”操作回到当前终端目录。
            if let (Some(current), Some(guest)) = (self.file_wsl_root(), guest_path) {
                return self.set_custom_wsl_root(crate::shell_detect::WslCwd {
                    distro: current.distro.clone(),
                    guest,
                });
            }
            return self.set_custom_root(path);
        }
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        // Re-flatten only (no git re-run): tree shape changed, content didn't.
        self.rebuild_rows();
        true
    }

    /// Open a directory returned by the flat search list as this drawer's
    /// window-local browse root. The caller clears its visible input after a
    /// successful navigation, which restores the normal tree snapshot.
    pub fn browse_search_directory(&mut self, path: PathBuf, guest_path: Option<String>) -> bool {
        if self.search.trim().is_empty() {
            return false;
        }
        if let Some(guest) = guest_path {
            let Some(distro) = self.file_wsl_root().map(|root| root.distro.clone()) else {
                return false;
            };
            self.set_custom_wsl_root(crate::shell_detect::WslCwd { distro, guest })
        } else {
            self.set_custom_root(path)
        }
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

    /// 首份目录快照是否仍在后台读取。GPUI 用它区分“正在读取”和真空目录。
    pub fn snapshot_pending(&self) -> bool {
        self.snapshot_running.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 上一次落地的枚举是否失败（WSL 冷启动耗尽预算、find 非零退出等）。
    /// 行为空且这里为真时，UI 必须提示可重试，而不是宣称“此目录为空”。
    pub fn enumeration_failed(&self) -> bool {
        self.enumeration_failed
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
