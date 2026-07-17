//! Right-side drawer: directory tree / git status for the focused pane's cwd
//! (otty-style). This module owns only the *model* — tree flattening, git
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
}

/// Parsed `git status` snapshot.
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
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
const MAX_ROWS: usize = 400;
/// Hard cap on entries listed per directory.
const MAX_PER_DIR: usize = 200;
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
    /// deep matches (VS Code's explorer filter).
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
    last_refresh: Option<Instant>,
    needs_refresh: bool,
}

fn git_pull_args() -> Vec<String> {
    vec!["pull".into(), "--ff-only".into()]
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
            last_refresh: None,
            needs_refresh: false,
        }
    }

    /// Toggle the drawer. Re-invoking with the *other* view while open only
    /// switches views (VS Code sidebar behaviour) instead of closing.
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
        self.followed_cwd = cwd;
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
            return false;
        }
        if root_changed {
            self.root = next_root;
            self.expanded.clear();
            self.scroll = 0;
            self.selected = None;
        }
        self.refresh();
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

    /// Rebuild the tree and git snapshot from `root`.
    fn refresh(&mut self) {
        self.needs_refresh = false;
        self.last_refresh = Some(Instant::now());
        // New snapshot → the filter index is stale; rebuild lazily on demand.
        self.search_index = None;
        self.rebuild_rows();
        self.git = None;
        if let Some(root) = self.root.clone() {
            self.git = read_git(&root);
        }
    }

    /// Rebuild only the flattened rows (tree shape / filter changes; the git
    /// snapshot stays).
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(root) = self.root.clone() else { return };
        let needle = self.search.trim().to_lowercase();
        if needle.is_empty() {
            self.flatten_dir(&root, 0);
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

    /// Run `git <args>` on a worker thread; UI stays live (a push can take
    /// seconds over the network). Completion flips `op_done`, which the next
    /// drawn frame folds into a refresh.
    fn spawn_git(&mut self, args: Vec<String>) {
        use std::sync::atomic::Ordering;
        let Some(root) = self.root.clone() else { return };
        if self.op_running.swap(true, Ordering::Relaxed) {
            return; // one at a time
        }
        let running = self.op_running.clone();
        let done = self.op_done.clone();
        let error = self.op_error.clone();
        std::thread::Builder::new()
            .name("nebula-git-op".into())
            .spawn(move || {
                let mut cmd = std::process::Command::new("git");
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
                        err.lines().find(|l| !l.trim().is_empty()).unwrap_or("git 失败").to_string()
                    },
                    Err(e) => format!("git: {e}"),
                };
                if let Ok(mut slot) = error.lock() {
                    *slot = msg;
                }
                running.store(false, Ordering::Relaxed);
                done.store(true, Ordering::Relaxed);
            })
            .ok();
    }

    /// `git add -A`: stage everything (the ⊕ button).
    pub fn git_stage_all(&mut self) {
        if self.git.as_ref().is_some_and(|g| !g.unstaged.is_empty()) && !self.op_running() {
            self.spawn_git(vec!["add".into(), "-A".into()]);
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
        self.spawn_git(vec!["commit".into(), "-m".into(), msg]);
    }

    /// Push button — only enabled with committed-but-unpushed work (`ahead`).
    pub fn git_push(&mut self) {
        if self.git.as_ref().is_some_and(|g| g.ahead > 0) && !self.op_running() {
            self.spawn_git(vec!["push".into()]);
        }
    }

    /// Pull only fast-forward updates, never creating an implicit merge commit.
    pub fn git_pull(&mut self) {
        if self.git.is_some() && !self.op_running() {
            self.spawn_git(git_pull_args());
        }
    }

    /// Depth-first flatten of `dir` into `rows`, following `expanded`.
    fn flatten_dir(&mut self, dir: &Path, depth: usize) {
        if self.rows.len() >= MAX_ROWS {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<(bool, String, PathBuf)> = read
            .flatten()
            .take(MAX_PER_DIR)
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
        // Directories first, then case-insensitive alphabetical (Explorer/
        // VS Code convention).
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        for (is_dir, name, path) in entries {
            if self.rows.len() >= MAX_ROWS {
                return;
            }
            let expanded = is_dir && self.expanded.contains(&path);
            self.rows.push(FileRow { path: path.clone(), name, depth, is_dir, expanded });
            if expanded {
                self.flatten_dir(&path, depth + 1);
            }
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
            .is_some_and(|row| row.is_dir && row.path == drag.path);
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
) -> PanelLayout {
    let s = |v: f32| v * scale;
    // Same margin / bar height / breathing gap as `chrome_tab_layout`.
    let margin = s(8.0);
    let bar_h = s(40.0);
    let gap = s(12.0);
    let w = s(PANEL_W_LOGICAL).min(win_w * 0.42);
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
    let header_h = s(40.0);
    let row_h = s(34.0);
    let search = (x + s(14.0), y + header_h + s(34.0), w - s(28.0), s(34.0));
    let list_y = search.1 + search.3 + s(16.0); // header + summary + filter box
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
        // Header: two half-width view tabs.
        return if x < px + pw * 0.5 { PanelHit::ViewFiles } else { PanelHit::ViewGit };
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
    custom_root: bool,
    has_root: bool,
) -> impl Iterator<Item = (PanelHit, (f32, f32, f32, f32))> {
    let scale = layout.header_h / 40.0;
    let s = |value: f32| value * scale;
    let (px, py, pw, _) = layout.panel;
    let height = s(26.0);
    let y = py + layout.header_h + s(4.0);
    let right = px + pw - s(10.0);
    let icon_width = s(30.0);
    let gap = s(4.0);
    let open_x = right - icon_width;
    let terminal_x = open_x - gap - icon_width;
    let leftmost_icon_x = if has_root { terminal_x } else { open_x };
    let follow_width = s(62.0);
    let follow_x = leftmost_icon_x - gap - follow_width;

    // Fixed-size options keep per-frame draw and pointer hit-testing free of
    // tiny heap allocations while still allowing root-dependent actions.
    [
        Some((PanelHit::OpenDirectory, (open_x, y, icon_width, height))),
        has_root.then_some((PanelHit::NewTerminalHere, (terminal_x, y, icon_width, height))),
        (custom_root && has_root)
            .then_some((PanelHit::FollowCurrentDirectory, (follow_x, y, follow_width, height))),
    ]
    .into_iter()
    .flatten()
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
        for (hit, (rx, ry, rw, rh)) in panel_action_rects(layout, custom_root, has_root) {
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                return hit;
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
        index.push(FileRow { path: path.clone(), name, depth: 0, is_dir, expanded: false });
        if is_dir {
            build_search_index(&path, depth + 1, index, budget);
        }
    }
}

/// Snapshot git state for `root`: branch, ±line counts, changed files./// `None` when git is missing or `root` isn't inside a work tree. Runs
/// synchronously — callers throttle (see [`SidePanel::sync`]).
fn read_git(root: &Path) -> Option<GitInfo> {
    use std::process::Command;
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = Command::new("git");
        cmd.arg("--no-optional-locks").args(args).current_dir(root);
        // Suppress the console window that `Command` flashes on Windows GUI apps.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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

// ---- rendering (mirrors the `settings.rs` split: the parent `display::mod`
// hands in a snapshot + renderer; this module owns the drawer's pixels) ----

use crate::display::color::Rgb;
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use super::{NebulaTheme, SizeInfo, UI_CORNER_RADIUS_LOGICAL};

// Codicon glyphs (same family as the chrome's sidebar/settings icons).
pub(super) const ICON_FOLDER: &str = "\u{ea83}";
pub(super) const ICON_FOLDER_OPEN: &str = "\u{eaf7}";
const ICON_TERMINAL: &str = "\u{ea85}";
const ICON_FILE: &str = "\u{ea7b}";
pub(super) const ICON_CHEVRON_RIGHT: &str = "\u{eab6}";
const ICON_CHEVRON_DOWN: &str = "\u{eab4}";
const ICON_BRANCH: &str = "\u{ea68}";
const ICON_SEARCH: &str = "\u{ea6d}";

/// File-type icon for a tree row, keyed by extension (dotfile names like
/// `.gitignore` count as their own family). The glyph carries the type; the
/// ink stays the tree's neutral scheme — no per-type colors in the chrome.
/// Every codepoint here is verified present in the bundled Maple Mono NF CN
/// (codicon/seti/devicon/octicon blocks), so nothing can render as tofu.
pub(super) fn file_type_icon(name: &str) -> &'static str {
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

    // Header: two half-width view tabs. The active one wears the left
    // sidebar's floating-pill language — an accent halo, the tab 底色, and a
    // soft accent wash — no accent bar (state is brightness, per the sheet).
    // A hovered inactive tab gets the quiet hover wash.
    let tab_w = pw * 0.5 - s(8.0);
    let tab_h = layout.header_h - s(8.0);
    let (fx, gx) = (px + s(6.0), px + pw * 0.5 + s(2.0));
    let active_x = match panel.view {
        PanelView::Files => fx,
        PanelView::Git => gx,
    };
    let ty = py + s(4.0);
    quads.push(UiQuad::solid(
        active_x - s(1.0),
        ty - s(1.0),
        tab_w + s(2.0),
        tab_h + s(2.0),
        radius + s(1.0),
        Rgba::new(accent.r, accent.g, accent.b, 40),
    ));
    quads.push(UiQuad::solid(active_x, ty, tab_w, tab_h, radius, palette.tab_bg_l));
    quads.push(UiQuad::solid(
        active_x,
        ty,
        tab_w,
        tab_h,
        radius,
        Rgba::new(accent.r, accent.g, accent.b, 26),
    ));
    let hovered_tab_x = match panel.hover {
        PanelHit::ViewFiles if panel.view != PanelView::Files => Some(fx),
        PanelHit::ViewGit if panel.view != PanelView::Git => Some(gx),
        _ => None,
    };
    if let Some(hx) = hovered_tab_x {
        quads.push(UiQuad::solid(hx, ty, tab_w, tab_h, radius, sk.hover));
    }

    if panel.view == PanelView::Files {
        for (hit, (x, y, w, h)) in
            panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
        {
            let fill = if panel.hover == hit { sk.hover_strong } else { sk.input };
            quads.push(UiQuad::solid(x, y, w, h, radius, fill));
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
    ls: LsColors,
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

    // Header tabs: icon + label.
    let header_ty = py + (layout.header_h - cell_h) / 2.0;
    let files_hover = panel.hover == PanelHit::ViewFiles;
    let git_hover = panel.hover == PanelHit::ViewGit;
    let files_lift = if files_hover && panel.view != PanelView::Files { -s(1.0) } else { 0.0 };
    let git_lift = if git_hover && panel.view != PanelView::Git { -s(1.0) } else { 0.0 };
    let (files_ink, git_ink) = match panel.view {
        PanelView::Files => (sk.ink_strong, sk.ink_dim),
        PanelView::Git => (sk.ink_dim, sk.ink_strong),
    };
    let fx = px + s(6.0) + s(12.0);
    r.draw_chrome_text(size, fx, header_ty + files_lift, files_ink, ICON_FOLDER, gc);
    r.draw_chrome_text(size, fx + cell_w * 1.8, header_ty + files_lift, files_ink, "文件", gc);
    let gx = px + pw * 0.5 + s(2.0) + s(12.0);
    r.draw_chrome_text(size, gx, header_ty + git_lift, git_ink, ICON_BRANCH, gc);
    r.draw_chrome_text(size, gx + cell_w * 1.8, header_ty + git_lift, git_ink, "Git", gc);

    let summary_y = py + layout.header_h + (s(30.0) - cell_h) / 2.0;
    let scroll = panel.scroll;
    let row_ty = |i: usize| layout.list_y + i as f32 * layout.row_h + (layout.row_h - cell_h) / 2.0;

    match panel.view {
        PanelView::Files => {
            let action_x =
                panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
                    .map(|(_, rect)| rect.0)
                    .fold(px + pw - text_pad, f32::min);
            let summary_cols =
                (((action_x - px - 2.0 * text_pad) / cell_w).floor() as usize).max(4);
            let (summary, summary_ink) = if let Some(notice) = panel.root_notice() {
                (clip_tail(notice, summary_cols), Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b))
            } else if let Some(hint) = match panel.hover {
                PanelHit::NewTerminalHere => Some("在此新建终端"),
                PanelHit::OpenDirectory => Some("选择文件树目录"),
                PanelHit::FollowCurrentDirectory => Some("跟随当前终端"),
                _ => None,
            } {
                (clip_tail(hint, summary_cols), sk.ink_strong)
            } else {
                (
                    panel
                        .root()
                        .map(|root| clip_tail(&root.display().to_string(), summary_cols))
                        .unwrap_or_else(|| "（无目录）".into()),
                    sk.ink_dim,
                )
            };
            r.draw_chrome_text(size, px + text_pad, summary_y, summary_ink, &summary, gc);

            for (hit, (x, y, w, h)) in
                panel_action_rects(layout, panel.custom_root_active(), panel.root().is_some())
            {
                let ink = if panel.hover == hit { sk.ink_strong } else { sk.ink_dim };
                let (label, columns) = match hit {
                    PanelHit::OpenDirectory => (ICON_FOLDER_OPEN, 1.0),
                    PanelHit::NewTerminalHere => (ICON_TERMINAL, 1.0),
                    PanelHit::FollowCurrentDirectory => ("跟随", 4.0),
                    _ => continue,
                };
                let tx = x + ((w - cell_w * columns) / 2.0).max(0.0);
                let ty = y + (h - cell_h) / 2.0;
                r.draw_chrome_text(size, tx, ty, ink, label, gc);
            }

            // Filter box: magnifier + query (caret while focused) or hint.
            let (sx, sy, _, sh) = layout.search;
            let search_ty = sy + (sh - cell_h) / 2.0;
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
                let hovered = matches!(panel.hover, PanelHit::Row(h) if h == i);
                let selected = panel.selected.as_ref() == Some(&row.path)
                    || panel.drag_file.as_ref().is_some_and(|d| d.path == row.path);
                let lift_x = if hovered || selected { s(1.0) } else { 0.0 };
                let lift_y = if hovered { -s(1.0) } else { 0.0 };
                let ry = row_ty(i) + lift_y;
                let mut x = px + text_pad + row.depth as f32 * cell_w * 2.4 + lift_x;
                if !filtering {
                    if row.is_dir {
                        let chev =
                            if row.expanded { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_RIGHT };
                        r.draw_chrome_text(size, x, ry, sk.ink_faint, chev, gc);
                    }
                    x += cell_w * 1.9;
                }
                let (icon, icon_ink, name_ink) = if row.is_dir {
                    // `ls` parity: directories in the terminal's ANSI blue.
                    (if row.expanded { ICON_FOLDER_OPEN } else { ICON_FOLDER }, ls.dir, ls.dir)
                } else if is_executable(&row.name) {
                    // Executables in ANSI green, same as Nebula-List.
                    (file_type_icon(&row.name), ls.exec, ls.exec)
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
            if panel.file_rows().is_empty() {
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
                let y = layout.list_y + s(8.0);
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
                let ty = my + s(12.0) + (s(26.0) - cell_h) / 2.0;
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
                let strip_ty = sy + (sh - cell_h) / 2.0;
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
        let rows = p.file_rows();
        assert_eq!(rows[0].name, "sub", "directory sorts before file");
        assert!(rows[0].is_dir);
        assert_eq!(rows.len(), 2, "collapsed dir hides children");

        assert!(p.click_row(0), "clicking a dir toggles expansion");
        let rows = p.file_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].name, "inner.txt");
        assert_eq!(rows[1].depth, 1);

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
        let drag = FileDrag::new(sub, "sub".into(), true, 0, (10.0, 10.0));

        assert!(panel.click_drag_source(&drag), "a non-drag release keeps directory click");
        assert!(panel.file_rows()[0].expanded);

        let mut active = drag.clone();
        active.update_position((18.0, 10.0));
        assert!(active.active, "eight physical pixels arm the drag");
        assert!(!panel.click_drag_source(&active), "an active drag must not toggle the tree");
        assert!(panel.file_rows()[0].expanded);

        let mut stale = drag;
        stale.source_row = 1;
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
        let l = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0);
        let (px, py, pw, _) = l.panel;
        assert_eq!(panel_hit(&l, px - 1.0, py + 5.0), PanelHit::None);
        assert_eq!(panel_hit(&l, px + 5.0, py + 5.0), PanelHit::ViewFiles);
        assert_eq!(panel_hit(&l, px + pw - 5.0, py + 5.0), PanelHit::ViewGit);
        assert_eq!(panel_hit(&l, px + 5.0, l.list_y + l.row_h * 1.5), PanelHit::Row(1));
    }

    #[test]
    fn git_hover_only_accepts_real_file_rows() {
        let mut panel = SidePanel::new();
        panel.view = PanelView::Git;
        panel.git = Some(GitInfo {
            branch: "main".into(),
            plus: 0,
            minus: 0,
            ahead: 0,
            unstaged: vec![('?', "one.txt".into()), ('M', "two.txt".into())],
            staged: vec![('A', "three.txt".into())],
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
        let layout = panel_layout(1000.0, 800.0, 40.0, 30.0, 1.0, 1.0);
        let actions: Vec<_> = panel_action_rects(&layout, true, true).collect();
        let open = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::OpenDirectory)
            .expect("open-directory action");
        let follow = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::FollowCurrentDirectory)
            .expect("follow-current-directory action");
        let terminal = actions
            .iter()
            .find(|(hit, _)| *hit == PanelHit::NewTerminalHere)
            .expect("new-terminal-here action");
        let center = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
        let (open_x, open_y) = center(open.1);
        let (follow_x, follow_y) = center(follow.1);
        let (terminal_x, terminal_y) = center(terminal.1);

        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, true, true, open_x, open_y),
            PanelHit::OpenDirectory
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
            PanelHit::Inside,
            "the reset action must not exist while following the terminal cwd"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Files, false, false, terminal_x, terminal_y),
            PanelHit::Inside,
            "the terminal action must not exist without a tree root"
        );
        assert_eq!(
            panel_interactive_hit(&layout, PanelView::Git, true, true, open_x, open_y),
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
}
