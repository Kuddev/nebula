//! Command palette (`Ctrl+Shift+P`): a fuzzy-searchable launcher for every
//! Nebula action — the discoverable entry point for features whose shortcuts
//! are hard to remember.
//!
//! This module owns only the *model*: the action list, the query/selection
//! state, fuzzy filtering, and the popup layout maths. Rendering lives in
//! `display::mod` (it mirrors the settings modal), and execution is dispatched
//! by the input layer, which is the only place that can reach both the display
//! and the window context. Keeping the model here makes it self-contained and
//! keeps the giant `mod.rs` free of the item table.

use super::{NebulaTheme, SizeInfo};
use crate::shell_detect::DetectedShell;
use unicode_width::UnicodeWidthChar;

/// A dynamic quick-launch row: a config profile (launched by index) or a
/// detected shell (spec carried inline). Built fresh on every menu open.
#[derive(Debug, Clone)]
enum ProfileRow {
    /// Config profile at this index — routed through `TabRequest::NewProfile`.
    Config { label: String, search: String, index: usize },
    /// Detected shell — routed through `TabRequest::NewShell`. `hint` is the
    /// program path, shown dimmed (Windows Terminal's profile menu layout).
    Shell { label: String, hint: String, search: String, shell: DetectedShell },
}

impl ProfileRow {
    fn label(&self) -> &str {
        match self {
            Self::Config { label, .. } | Self::Shell { label, .. } => label,
        }
    }

    fn hint(&self) -> &str {
        match self {
            Self::Config { .. } => "",
            Self::Shell { hint, .. } => hint,
        }
    }

    fn search(&self) -> &str {
        match self {
            Self::Config { search, .. } | Self::Shell { search, .. } => search,
        }
    }

    /// Leading Nerd Font glyph. Detected shells carry their own; config
    /// profiles get a generic launch mark.
    fn icon(&self) -> &'static str {
        match self {
            Self::Shell { shell, .. } => shell.icon(),
            Self::Config { .. } => "\u{ea60}",
        }
    }

    /// Stable shell id for the full-color brand icon lookup, or `""` for
    /// config profiles (which have no brand asset and keep their glyph).
    fn color_id(&self) -> &str {
        match self {
            Self::Shell { shell, .. } => &shell.id,
            Self::Config { .. } => "",
        }
    }
}

/// A single executable action reachable from the command palette.
///
/// Deliberately flat so the input layer can match on it after the palette
/// closes, without holding any borrow. Each variant maps onto either a
/// `TabRequest` (tab / split / window operations) or a `Display` method
/// (theme / settings / appearance) — see `keyboard.rs::run_palette_action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    NewWindow,
    SplitRight,
    SplitDown,
    OpenSettings,
    OpenSettingsFile,
    ToggleGhost,
    CycleAccept,
    PickBackgroundImage,
    CycleBackground,
    ResetAppearance,
    SelectTheme(NebulaTheme),
    /// Launch the quick-launch profile at this config index in a new tab.
    LaunchProfile(usize),
    /// Launch a detected shell (the new-tab dropdown) in a new tab.
    LaunchShell(DetectedShell),
    /// Set a detected shell as the default (the settings "默认 Shell" picker).
    SetDefaultShell(DetectedShell),
    ToggleFilesPanel,
    ToggleGitPanel,
}

/// One palette row.
///
/// * `label`  — localized text shown on the left.
/// * `hint`   — optional shortcut / aux text, dimmed and right-aligned (ASCII,
///   so its on-screen width equals its `char` count).
/// * `search` — the haystack matched against the query. Includes the label plus
///   latin aliases (pinyin / English) so the palette is reachable even when the
///   IME can't feed CJK into it.
/// * `action` — what to run on confirm.
struct PaletteItem {
    label: &'static str,
    hint: &'static str,
    search: &'static str,
    action: PaletteAction,
}

/// The full action table, in declaration order (also the tie-break order when
/// fuzzy scores are equal, and the order shown for an empty query).
const ITEMS: &[PaletteItem] = &[
    PaletteItem {
        label: "新建标签页",
        hint: "Ctrl+Shift+T",
        search: "新建标签页 new tab xinjian biaoqianye",
        action: PaletteAction::NewTab,
    },
    PaletteItem {
        label: "关闭标签页",
        hint: "Ctrl+Shift+W",
        search: "关闭标签页 close tab guanbi",
        action: PaletteAction::CloseTab,
    },
    PaletteItem {
        label: "下一个标签页",
        hint: "Ctrl+Tab",
        search: "下一个标签页 next tab xiayige",
        action: PaletteAction::NextTab,
    },
    PaletteItem {
        label: "上一个标签页",
        hint: "Ctrl+Shift+Tab",
        search: "上一个标签页 previous prev tab shangyige",
        action: PaletteAction::PrevTab,
    },
    PaletteItem {
        label: "新建窗口",
        hint: "Ctrl+Shift+E",
        search: "新建窗口 new window xinjian chuangkou",
        action: PaletteAction::NewWindow,
    },
    PaletteItem {
        label: "左右分屏",
        hint: "Ctrl+Shift+D",
        search: "左右分屏 split right vertical zuoyou fenping",
        action: PaletteAction::SplitRight,
    },
    PaletteItem {
        label: "上下分屏",
        hint: "Ctrl+Shift+S",
        search: "上下分屏 split down horizontal shangxia fenping",
        action: PaletteAction::SplitDown,
    },
    PaletteItem {
        label: "目录树面板",
        hint: "Ctrl+Shift+O",
        search: "目录树面板 files tree explorer panel mulushu wenjian",
        action: PaletteAction::ToggleFilesPanel,
    },
    PaletteItem {
        label: "Git 面板",
        hint: "Ctrl+Shift+G",
        search: "git 面板 status branch panel mianban",
        action: PaletteAction::ToggleGitPanel,
    },
    PaletteItem {
        label: "打开设置",
        hint: "",
        search: "打开设置 open settings preferences dakai shezhi",
        action: PaletteAction::OpenSettings,
    },
    PaletteItem {
        label: "打开配置文件",
        hint: "",
        search: "打开配置文件 open config file dakai peizhi wenjian",
        action: PaletteAction::OpenSettingsFile,
    },
    PaletteItem {
        label: "切换行内补全 (Ghost)",
        hint: "",
        search: "切换行内补全 toggle ghost completion qiehuan buquan",
        action: PaletteAction::ToggleGhost,
    },
    PaletteItem {
        label: "切换补全接受键",
        hint: "",
        search: "切换补全接受键 cycle accept key completion jieshou",
        action: PaletteAction::CycleAccept,
    },
    PaletteItem {
        label: "选择背景图片…",
        hint: "",
        search: "选择背景图片 background image picture xuanze beijing tupian",
        action: PaletteAction::PickBackgroundImage,
    },
    PaletteItem {
        label: "切换背景色",
        hint: "",
        search: "切换背景色 cycle background color qiehuan beijingse",
        action: PaletteAction::CycleBackground,
    },
    PaletteItem {
        label: "恢复外观默认",
        hint: "",
        search: "恢复外观默认 reset appearance default huifu waiguan moren",
        action: PaletteAction::ResetAppearance,
    },
    PaletteItem {
        label: "主题：Nebula",
        hint: "",
        search: "主题 nebula theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::Nebula),
    },
    PaletteItem {
        label: "主题：Silver Light",
        hint: "",
        search: "主题 silver light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::SilverLight),
    },
    PaletteItem {
        label: "主题：Steel Dark",
        hint: "",
        search: "主题 steel dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::SteelDark),
    },
    PaletteItem {
        label: "主题：Limestone",
        hint: "",
        search: "主题 limestone light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::LimestoneLight),
    },
    PaletteItem {
        label: "主题：Coal Dark",
        hint: "",
        search: "主题 coal dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::CoalDark),
    },
    PaletteItem {
        label: "主题：Linen Light",
        hint: "",
        search: "主题 linen light theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::LinenLight),
    },
    PaletteItem {
        label: "主题：Moss Dark",
        hint: "",
        search: "主题 moss dark theme zhuti",
        action: PaletteAction::SelectTheme(NebulaTheme::MossDark),
    },
];

/// How many recently-run actions are remembered for the empty-query ordering.
const RECENT_MAX: usize = 6;

/// Command palette UI + filtering state, embedded in `Display`.
pub struct CommandPalette {
    language: super::UiLanguage,
    open: bool,
    query: String,
    query_selection: super::text_input::SelectAllState,
    /// Indices into the combined item space — `0..ITEMS.len()` are the static
    /// actions, `ITEMS.len()..` map onto `profiles`. Best match first.
    filtered: Vec<usize>,
    /// Selected row *within `filtered`*. `None` when nothing is selected yet
    /// (initial state — keyboard nav or hover will activate selection).
    selected: Option<usize>,
    /// Recently-run `ITEMS` indices, most-recent first (deduped, capped at
    /// `RECENT_MAX`). Lifts frequent actions to the top of an empty query.
    /// Static items only: profile indices shift whenever the config changes.
    recent: Vec<usize>,
    /// Dynamic quick-launch rows, refreshed on every open so live config
    /// reloads and shell (re)detection are picked up. In profiles-only (the
    /// new-tab dropdown) these are detected shells + config profiles; in the
    /// full palette they're the config profiles appended after the actions.
    profiles: Vec<ProfileRow>,
    /// Show ONLY profile rows — the new-tab dropdown mode.
    profiles_only: bool,
    /// In profiles-only mode, confirming a detected-shell row SETS it as the
    /// default shell (the settings "默认 Shell" picker) instead of launching a
    /// tab. Config-profile rows are hidden in this mode.
    picking_default: bool,
    /// Mouse-hovered row within the visible window (`None` when not hovering).
    hover: Option<usize>,
    /// Cursor blink animation state for the search input.
    cursor_pulse: crate::motion::Pulse,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut palette = Self {
            language: super::UiLanguage::ZhCn,
            open: false,
            query: String::new(),
            query_selection: Default::default(),
            filtered: Vec::new(),
            selected: None, // No selection until user navigates
            recent: Vec::new(),
            profiles: Vec::new(),
            profiles_only: false,
            picking_default: false,
            hover: None,
            cursor_pulse: crate::motion::Pulse::new(std::time::Duration::from_millis(1060)),
        };
        palette.refilter();
        palette
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_language(&mut self, language: super::UiLanguage) {
        self.language = language;
        if !self.profiles_only {
            self.refilter();
        }
    }

    /// Whether the palette is in default-shell picking mode.
    pub fn is_picking_default(&self) -> bool {
        self.picking_default
    }

    /// Shell/profile selector mode. This identity is used only for natural
    /// dismissal on window focus loss; the selector remains searchable because
    /// a long shell/profile list otherwise becomes needlessly hard to scan.
    pub fn is_picker(&self) -> bool {
        self.open && self.profiles_only
    }

    /// Refresh the dynamic quick-launch rows from the config's profile names.
    /// Called by the full-palette open path so a reloaded config is reflected.
    pub fn set_profiles(&mut self, names: &[String]) {
        self.profiles = names
            .iter()
            .enumerate()
            .map(|(index, name)| ProfileRow::Config {
                // The label carries a glyph-free prefix so profile rows read
                // distinctly from built-in actions; the haystack adds latin
                // aliases (matching the static items' convention).
                label: format!("{}：{name}", self.language.pick("启动", "Launch")),
                search: format!("启动 {name} profile launch connect qidong"),
                index,
            })
            .collect();
    }

    /// Populate the new-tab dropdown: detected shells first (installed-shell
    /// order), then config profiles. The label carries no verb prefix here —
    /// this menu IS the shell picker, so bare names read cleaner.
    pub fn set_shell_menu(&mut self, shells: &[DetectedShell], profiles: &[String]) {
        let mut rows: Vec<ProfileRow> = shells
            .iter()
            .map(|shell| ProfileRow::Shell {
                label: shell.name.clone(),
                hint: shell.program.clone(),
                search: format!("{} {} shell profile", shell.name, shell.id),
                shell: shell.clone(),
            })
            .collect();
        rows.extend(profiles.iter().enumerate().map(|(index, name)| ProfileRow::Config {
            label: name.clone(),
            search: format!("{name} profile launch connect qidong"),
            index,
        }));
        self.profiles = rows;
    }

    /// Populate the settings "默认 Shell" picker: detected shells only (no
    /// config profiles — you can't default to an ssh jump), and confirming
    /// sets the default instead of launching.
    pub fn set_default_shell_menu(&mut self, shells: &[DetectedShell]) {
        self.profiles = shells
            .iter()
            .map(|shell| ProfileRow::Shell {
                label: shell.name.clone(),
                hint: shell.program.clone(),
                search: format!("{} {} shell profile", shell.name, shell.id),
                shell: shell.clone(),
            })
            .collect();
    }

    /// Open (or re-open) the palette with a cleared query and the full list.
    pub fn open(&mut self) {
        self.open = true;
        self.profiles_only = false;
        self.picking_default = false;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    /// Open showing only the quick-launch profiles (the "+" dropdown).
    pub fn open_profiles(&mut self) {
        self.open = true;
        self.profiles_only = true;
        self.picking_default = false;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    /// Open the default-shell picker (settings row): profile rows only, and
    /// confirm SETS the default rather than launching.
    pub fn open_default_picker(&mut self) {
        self.open = true;
        self.profiles_only = true;
        self.picking_default = true;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.profiles_only = false;
        self.picking_default = false;
        self.hover = None;
        self.query_selection.clear();
    }

    /// Update hover based on mouse position. `row` is the index within the
    /// visible window (`0..max_rows`), or `None` when the mouse left.
    pub fn set_hover(&mut self, row: Option<usize>) {
        self.hover = row;
    }

    /// The number of filtered results currently shown.
    pub fn visible_count(&self) -> usize {
        self.filtered.len()
    }

    /// Update cursor blink state. Call this each frame while the palette is open.
    pub fn tick_cursor(&mut self, frame: crate::motion::Frame) {
        self.cursor_pulse.step(frame);
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_pulse.visible(0.5)
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// Append a typed character (control chars ignored) and re-filter.
    pub fn input_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let mut encoded = [0u8; 4];
        self.query_selection.insert(&mut self.query, c.encode_utf8(&mut encoded));
        self.refilter();
    }

    pub fn input_text(&mut self, text: &str) {
        self.query_selection.insert(&mut self.query, text);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query_selection.backspace(&mut self.query);
        self.refilter();
    }

    pub fn select_all(&mut self) {
        self.query_selection.select(&self.query);
    }

    pub fn selected_text(&self) -> Option<String> {
        self.query_selection.selected_text(&self.query)
    }

    pub fn query_all_selected(&self) -> bool {
        self.query_selection.is_selected()
    }

    /// Move the selection by `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        // Initialize selection on first navigation
        let current = self.selected.unwrap_or(0) as i32;
        self.selected = Some(((current + delta).rem_euclid(len)) as usize);
    }

    /// Confirm the current selection: records it as recent, closes the palette,
    /// and returns the action to run, or `None` when nothing matches.
    pub fn confirm(&mut self) -> Option<PaletteAction> {
        // The first row is visibly selected on a freshly opened palette. Make
        // Enter execute that same row even before arrow navigation, otherwise
        // the UI advertises an action that the keyboard cannot confirm.
        let selected = self.selected.unwrap_or(0);
        let idx = *self.filtered.get(selected)?;
        self.close();
        if let Some(profile) = idx.checked_sub(ITEMS.len()) {
            return Some(match &self.profiles[profile] {
                ProfileRow::Config { index, .. } => PaletteAction::LaunchProfile(*index),
                ProfileRow::Shell { shell, .. } if self.picking_default => {
                    PaletteAction::SetDefaultShell(shell.clone())
                },
                ProfileRow::Shell { shell, .. } => PaletteAction::LaunchShell(shell.clone()),
            });
        }
        self.record_recent(idx);
        Some(ITEMS[idx].action.clone())
    }

    /// Confirm the visible row at `row` (0 = topmost visible line, mirroring
    /// [`Self::visible`]'s scroll window) — the mouse-click path.
    pub fn click(&mut self, row: usize, max_rows: usize) -> Option<PaletteAction> {
        if self.filtered.is_empty() || max_rows == 0 {
            return None;
        }
        let start = self.selected.unwrap_or(0).saturating_sub(max_rows - 1);
        let idx = start + row;
        if idx >= self.filtered.len() {
            return None;
        }
        self.selected = Some(idx);
        self.confirm()
    }

    /// Remember `idx` as the most-recently run command (deduped, capped), so a
    /// freshly-opened (empty-query) palette lists frequent actions first.
    fn record_recent(&mut self, idx: usize) {
        self.recent.retain(|&i| i != idx);
        self.recent.insert(0, idx);
        self.recent.truncate(RECENT_MAX);
    }

    /// Re-score every item against the query and rebuild `filtered`. With a
    /// query: fuzzy score, best first, ties in declaration order. Empty query:
    /// recently-run first, then declaration order (a stable sort keeps the
    /// declared order for the un-recent tail), then profiles. Resets the
    /// selection to the top.
    fn refilter(&mut self) {
        // Candidate indices in combined space: static actions first (skipped
        // entirely in profiles-only mode), then the dynamic profile rows.
        let candidates: Vec<usize> = if self.profiles_only {
            (ITEMS.len()..ITEMS.len() + self.profiles.len()).collect()
        } else {
            (0..ITEMS.len() + self.profiles.len()).collect()
        };
        let combined_search = |idx: usize| -> &str {
            if idx < ITEMS.len() {
                ITEMS[idx].search
            } else {
                self.profiles[idx - ITEMS.len()].search()
            }
        };
        let query = self.query.trim();
        if query.is_empty() {
            let mut order = candidates;
            order.sort_by_key(|&i| self.recent.iter().position(|&r| r == i).unwrap_or(usize::MAX));
            self.filtered = order;
        } else {
            let mut scored: Vec<(i32, usize)> = candidates
                .into_iter()
                .filter_map(|i| fuzzy_score(query, combined_search(i)).map(|score| (score, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = None; // Reset selection on refilter
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// The at-most `max_rows` visible rows, scrolled so the selection stays in
    /// view, plus the selected row's index *within that window* (`None` when the
    /// list is empty OR nothing is selected yet). Collected so the result borrows nothing.
    pub fn visible(&self, max_rows: usize) -> (Vec<PaletteRow>, Option<usize>) {
        if self.filtered.is_empty() || max_rows == 0 {
            return (Vec::new(), None);
        }
        // No stored selection yet: the first row is the visual/default target.
        // `selected` stays None until navigation so Up from a fresh palette
        // still wraps to the last row, matching the existing keyboard model.
        let Some(selected) = self.selected else {
            let rows =
                self.filtered.iter().take(max_rows).map(|&idx| self.row_for_idx(idx)).collect();
            return (rows, Some(0));
        };
        // Keep the selection visible: once it passes the last row, scroll so it
        // sits on the bottom line of the window.
        let start = selected.saturating_sub(max_rows - 1);
        let rows = self
            .filtered
            .iter()
            .skip(start)
            .take(max_rows)
            .map(|&idx| self.row_for_idx(idx))
            .collect();
        (rows, Some(selected - start))
    }

    fn row_for_idx(&self, idx: usize) -> PaletteRow {
        match idx.checked_sub(ITEMS.len()) {
            Some(p) => PaletteRow {
                icon: self.profiles[p].icon().to_string(),
                color_id: self.profiles[p].color_id().to_string(),
                label: self.profiles[p].label().to_string(),
                hint: self.profiles[p].hint().to_string(),
            },
            None => PaletteRow {
                icon: String::new(),
                color_id: String::new(),
                label: localized_item_label(&ITEMS[idx], self.language).to_owned(),
                hint: ITEMS[idx].hint.to_string(),
            },
        }
    }
}

fn localized_item_label(item: &PaletteItem, language: super::UiLanguage) -> &'static str {
    use PaletteAction::*;
    if language == super::UiLanguage::ZhCn {
        return item.label;
    }
    match item.action {
        NewTab => "New tab",
        CloseTab => "Close tab",
        NextTab => "Next tab",
        PrevTab => "Previous tab",
        NewWindow => "New window",
        SplitRight => "Split right",
        SplitDown => "Split down",
        ToggleFilesPanel => "Files panel",
        ToggleGitPanel => "Git panel",
        OpenSettings => "Open settings",
        OpenSettingsFile => "Open configuration file",
        ToggleGhost => "Toggle ghost completion",
        CycleAccept => "Cycle completion accept key",
        PickBackgroundImage => "Choose background image...",
        CycleBackground => "Cycle background color",
        ResetAppearance => "Restore appearance defaults",
        SelectTheme(NebulaTheme::Nebula) => "Theme: Nebula",
        SelectTheme(NebulaTheme::SilverLight) => "Theme: Silver Light",
        SelectTheme(NebulaTheme::SteelDark) => "Theme: Steel Dark",
        SelectTheme(NebulaTheme::LimestoneLight) => "Theme: Limestone",
        SelectTheme(NebulaTheme::CoalDark) => "Theme: Coal Dark",
        SelectTheme(NebulaTheme::LinenLight) => "Theme: Linen Light",
        SelectTheme(NebulaTheme::MossDark) => "Theme: Moss Dark",
        LaunchProfile(_) | LaunchShell(_) | SetDefaultShell(_) => item.label,
    }
}

/// One rendered palette row. `icon` is the Nerd Font fallback glyph (empty for
/// built-in action rows); `color_id` names a full-color brand PNG when the row
/// is a detected shell (empty otherwise, so the glyph shows instead).
pub struct PaletteRow {
    pub icon: String,
    pub color_id: String,
    pub label: String,
    pub hint: String,
}

/// Subsequence fuzzy score, or `None` if the needle isn't a subsequence of the
/// haystack. Consecutive runs and word-start matches are rewarded so intuitive
/// queries rank first (e.g. "nt" prefers "new tab" over "next"). An empty
/// needle matches everything with score 0, preserving declaration order.
fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let mut next = 0usize;
    let mut score = 0i32;
    let mut run = 0i32;
    let mut prev = ' ';
    for hc in haystack.chars().flat_map(char::to_lowercase) {
        if next < needle.len() && hc == needle[next] {
            score += 1 + run * 5; // consecutive-match run bonus (dominant)
            if !prev.is_alphanumeric() {
                score += 4; // word / segment start
            }
            run += 1;
            next += 1;
        } else {
            run = 0;
        }
        prev = hc;
    }
    (next == needle.len()).then_some(score)
}

/// Popup layout rectangles, all in physical pixels for the given `scale`.
pub struct PaletteLayout {
    /// Outer panel `(x, y, w, h)`.
    pub panel: (f32, f32, f32, f32),
    /// Query input box `(x, y, w, h)`.
    pub input: (f32, f32, f32, f32),
    /// Height of one result row.
    pub row_h: f32,
    /// Top Y of the first result row.
    pub list_y: f32,
    /// Maximum rows drawn before the list scrolls.
    pub max_rows: usize,
}

/// Compute the centered popup layout for a window of `win_w` × `win_h`. The
/// panel height is fixed (sized for `max_rows`) so it doesn't jump as the match
/// count changes while typing. Every palette mode uses the same search-input
/// geometry, keeping rendering, hover and click hit-testing on one contract.
pub fn palette_layout(win_w: f32, win_h: f32, scale: f32) -> PaletteLayout {
    let s = |v: f32| v * scale;
    let margin = s(8.0);
    let pad = s(12.0);
    let row_h = s(super::design_tokens::control::COMPACT_ROW);
    // 搜索框与结果行等高：输入仍然可发现，但不会压过真正的数据内容。
    let input_h = row_h;
    let max_rows = 8usize;

    let pw = s(640.0).min(win_w - 2.0 * margin);
    let ph = pad + input_h + s(8.0) + max_rows as f32 * row_h + pad;
    let px = ((win_w - pw) * 0.5).max(margin);
    let py = ((win_h - ph) * 0.5).max(s(48.0));

    let input = (px + pad, py + pad, pw - 2.0 * pad, input_h);
    let list_y = py + pad + input_h + s(8.0);

    PaletteLayout { panel: (px, py, pw, ph), input, row_h, list_y, max_rows }
}

// ---- rendering (the parent `display::mod` hands in the model + renderer;
// this module owns the palette's pixels — same split as `side_panel.rs`) ----

use crate::renderer::ui::{Gradient, Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

/// Push the palette's background quads: a dim veil over the window, the glass
/// panel (glow + gradient border + fill, matching the settings modal), the
/// query input box, and the selected-row
/// highlight. No-op while closed.
pub(super) fn push_quads(
    model: &CommandPalette,
    theme: &NebulaTheme,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    scale: f32,
) {
    if !model.is_open() {
        return;
    }
    let w = size.width();
    let h = size.height();
    let s = |v: f32| v * scale;
    let palette = theme.palette();
    let sk = theme.skin();
    let layout = palette_layout(w, h, scale);
    let (px, py, pw, ph) = layout.panel;
    let (ix, iy, iw, ih) = layout.input;

    quads.push(UiQuad::solid(0.0, 0.0, w, h, 0.0, Rgba::new(0, 0, 0, 150)));
    quads.push(UiQuad::glow(
        px - s(24.0),
        py - s(22.0),
        pw + s(48.0),
        ph + s(48.0),
        palette.edge_glow_l,
    ));
    quads.push(UiQuad::gradient(
        px - s(1.0),
        py - s(1.0),
        pw + s(2.0),
        ph + s(2.0),
        s(15.0),
        palette.tab_stroke_l,
        palette.edge_r,
        Gradient::Axis([0.9, 0.35]),
    ));
    quads.push(UiQuad::gradient(
        px,
        py,
        pw,
        ph,
        s(14.0),
        palette.panel,
        sk.panel_grad_to,
        Gradient::Axis([0.25, 0.95]),
    ));

    quads.push(UiQuad::solid(ix, iy, iw, ih, s(super::design_tokens::control::RADIUS), sk.input));
    if model.query_all_selected() && !model.query.is_empty() {
        let cell_w = size.cell_width();
        let columns: usize = model.query.chars().map(|c| c.width().unwrap_or(0)).sum();
        let selection_x = ix + s(14.0 + 28.0);
        let selection_w = (columns as f32 * cell_w).min(iw - s(56.0));
        quads.push(UiQuad::solid(
            selection_x - s(2.0),
            iy + s(7.0),
            selection_w + s(4.0),
            ih - s(14.0),
            s(4.0),
            sk.accent_soft,
        ));
    }

    // Hover background: subtle highlight when the mouse is over a row.
    if let Some(hover_row) = model.hover {
        if hover_row < layout.max_rows {
            let row_y = layout.list_y + hover_row as f32 * layout.row_h;
            quads.push(UiQuad::solid(
                ix + s(8.0),
                row_y + s(2.0),
                iw - s(16.0),
                layout.row_h - s(4.0),
                s(6.0),
                sk.hover,
            ));
        }
    }

    // Highlight pill behind the selected row (list scrolls to keep it shown).
    let (_, selected_row) = model.visible(layout.max_rows);
    if let Some(row) = selected_row {
        let ry = layout.list_y + row as f32 * layout.row_h;
        quads.push(UiQuad::gradient(
            ix,
            ry,
            iw,
            layout.row_h - s(4.0),
            s(8.0),
            palette.tab_bg_l,
            palette.tab_bg_r,
            Gradient::Horizontal,
        ));
    }
}

/// Draw the palette's text: the query line (with a caret) or a placeholder,
/// then the result rows with right-aligned shortcut hints. No-op while closed.
///
/// Returns the full-color brand-icon draw requests (`color_id`, pixel rect)
/// for detected-shell rows: the caller resolves each to a texture and stages
/// it for the post-text image pass (a textured quad can't be interleaved with
/// glyph batches). Rows whose id has no brand asset draw the Nerd Font glyph
/// here and contribute nothing to the returned list.
pub(super) fn draw_text(
    model: &CommandPalette,
    theme: &NebulaTheme,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) -> Vec<(String, (f32, f32, f32, f32))> {
    let mut icon_draws = Vec::new();
    if !model.is_open() {
        return icon_draws;
    }
    let s = |v: f32| v * scale;
    let w = size.width();
    let h = size.height();
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let layout = palette_layout(w, h, scale);
    let (ix, iy, iw, ih) = layout.input;

    // Inks from the theme skin: dark text on light panels, pale on dark.
    let sk = theme.skin();

    // Left edge for result text and the search icon.
    let text_x = ix + s(14.0);

    const ICON_SEARCH: &str = "\u{f0349}"; // mdi-magnify
    r.draw_chrome_text(size, text_x, iy + (ih - cell_h) / 2.0, sk.accent, ICON_SEARCH, gc);

    let query_x = text_x + s(28.0);
    let text_y = iy + (ih - cell_h) / 2.0;
    let query = model.query();
    let cursor = if model.cursor_visible() { "▏" } else { "" };

    if query.is_empty() {
        // 空态同时给出输入光标和用途提示，避免只有一个空白色块让用户猜测。
        r.draw_chrome_text(size, query_x, text_y, sk.ink_strong, cursor, gc);
        let placeholder = if model.profiles_only {
            model.language.pick("搜索 Shell 或 Profile…", "Search shells or profiles...")
        } else {
            model
                .language
                .pick("搜索命令、Shell 或 Profile…", "Search commands, shells or profiles...")
        };
        r.draw_chrome_text(size, query_x + s(10.0), text_y, sk.ink_dim, placeholder, gc);
    } else {
        let shown =
            if model.query_all_selected() { query.to_owned() } else { format!("{query}{cursor}") };
        r.draw_chrome_text(size, query_x, text_y, sk.ink_strong, &shown, gc);
    }

    if model.is_empty() {
        r.draw_chrome_text(
            size,
            text_x,
            layout.list_y + s(8.0),
            sk.ink_dim,
            model.language.pick("无匹配命令", "No matching commands"),
            gc,
        );
        return icon_draws;
    }

    let (rows, selected_row) = model.visible(layout.max_rows);
    for (row, entry) in rows.into_iter().enumerate() {
        let PaletteRow { icon, color_id, label, hint } = entry;
        let ry = layout.list_y + row as f32 * layout.row_h + (layout.row_h - cell_h) / 2.0 - s(2.0);
        let fg = if Some(row) == selected_row { sk.ink_strong } else { sk.ink };
        // Leading icon, then the label indented past it. Detected shells with a
        // brand asset stage a full-color textured quad (drawn later); the rest
        // fall back to the Nerd Font glyph. Built-in action rows carry an empty
        // icon and keep the original left edge.
        let has_color =
            !color_id.is_empty() && crate::shell_detect::color_icon_png(&color_id).is_some();
        let label_x = if has_color {
            // Square icon sized to the glyph ink, vertically centered on the row.
            let icon_s = (cell_h * 0.92).round();
            let icon_y = (ry + (cell_h - icon_s) / 2.0).round();
            icon_draws.push((color_id, (text_x, icon_y, icon_s, icon_s)));
            text_x + s(26.0)
        } else if icon.is_empty() {
            text_x
        } else {
            r.draw_chrome_text(size, text_x, ry, sk.accent, &icon, gc);
            text_x + s(26.0)
        };
        r.draw_chrome_text(size, label_x, ry, fg, &label, gc);
        if !hint.is_empty() {
            // Truncate long paths: reserve space for the hint, and if it would
            // collide with the label, clip the hint with "…" suffix.
            let max_hint_chars = ((iw - label_x - s(28.0)) / cell_w).floor() as usize;
            let hint_display = if hint.chars().count() > max_hint_chars && max_hint_chars > 2 {
                let truncated: String = hint.chars().take(max_hint_chars - 1).collect();
                format!("{}…", truncated)
            } else {
                hint.clone()
            };
            let hint_w = hint_display.chars().count() as f32 * cell_w;
            r.draw_chrome_text(size, ix + iw - s(14.0) - hint_w, ry, sk.ink_dim, &hint_display, gc);
        }
    }
    icon_draws
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_all_in_declaration_order() {
        let palette = CommandPalette::new();
        assert_eq!(palette.filtered.len(), ITEMS.len());
        assert_eq!(palette.filtered[0], 0, "first declared item leads by default");
    }

    #[test]
    fn recent_actions_surface_first_on_empty_query() {
        let mut palette = CommandPalette::new();
        palette.record_recent(3);
        palette.record_recent(7);
        palette.refilter();
        // Most-recent first, then the previous recent, then the declared rest.
        assert_eq!(palette.filtered[0], 7);
        assert_eq!(palette.filtered[1], 3);
        assert_eq!(palette.filtered[2], 0);
    }

    #[test]
    fn visible_returns_row_struct() {
        let mut palette = CommandPalette::new();
        palette.profiles = vec![ProfileRow::Shell {
            label: "PowerShell".into(),
            hint: "pwsh.exe".into(),
            search: "pwsh".into(),
            shell: DetectedShell {
                name: "PowerShell".into(),
                id: "pwsh".into(),
                program: "pwsh.exe".into(),
                args: vec![],
            },
        }];
        // Dynamic shell rows belong to the shell/profile picker. The full
        // command palette deliberately keeps static actions first.
        palette.open_profiles();
        let (rows, _) = palette.visible(10);
        assert!(!rows.is_empty());
        assert_eq!(rows.last().unwrap().label, "PowerShell");
    }

    #[test]
    fn record_recent_dedups_and_caps() {
        let mut palette = CommandPalette::new();
        for i in 0..(RECENT_MAX + 3) {
            palette.record_recent(i);
        }
        assert_eq!(palette.recent.len(), RECENT_MAX);
        // Re-running an existing action moves it to the front without growing.
        palette.record_recent(2);
        assert_eq!(palette.recent.first(), Some(&2));
        assert_eq!(palette.recent.len(), RECENT_MAX);
    }

    #[test]
    fn fuzzy_matches_subsequence_and_rejects_the_rest() {
        assert!(fuzzy_score("nt", "new tab").is_some());
        assert!(fuzzy_score("newtab", "new tab").is_some());
        assert!(fuzzy_score("xyz", "new tab").is_none());
        assert!(fuzzy_score("", "anything").is_some(), "empty query matches everything");
    }

    #[test]
    fn fuzzy_rewards_consecutive_and_word_start() {
        // A consecutive run beats the same letters scattered across separators.
        let consecutive = fuzzy_score("tab", "xtab").unwrap();
        let scattered = fuzzy_score("tab", "t-a-b").unwrap();
        assert!(consecutive > scattered, "consecutive {consecutive} vs scattered {scattered}");
        // A word-start match beats a mid-word match of the same length.
        let word_start = fuzzy_score("t", "x t").unwrap();
        let mid_word = fuzzy_score("t", "xt").unwrap();
        assert!(word_start > mid_word, "word-start {word_start} vs mid-word {mid_word}");
    }

    #[test]
    fn confirm_records_recent_and_closes() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.selected = Some(2);
        let picked = palette.filtered[2];
        let action = palette.confirm();
        assert!(action.is_some());
        assert!(!palette.is_open());
        assert_eq!(palette.recent.first(), Some(&picked));
    }

    #[test]
    fn typing_filters_then_backspace_restores() {
        let mut palette = CommandPalette::new();
        palette.open();
        let full = palette.filtered.len();
        for ch in "zqxjk".chars() {
            palette.input_char(ch);
        }
        assert!(palette.filtered.len() < full, "gibberish should filter most out");
        for _ in 0.."zqxjk".len() {
            palette.backspace();
        }
        assert_eq!(palette.filtered.len(), full, "clearing the query restores the full list");
    }

    #[test]
    fn move_selection_wraps_both_ends() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.selected, None);
        palette.move_selection(-1);
        assert_eq!(
            palette.selected,
            Some(palette.filtered.len() - 1),
            "up from top wraps to bottom"
        );
        palette.move_selection(1);
        assert_eq!(palette.selected, Some(0), "down from bottom wraps to top");
    }

    #[test]
    fn visible_window_scrolls_to_keep_selection_in_view() {
        let mut palette = CommandPalette::new();
        palette.open();
        let max = 5;
        // Selection at the top: window starts at 0, selection on row 0.
        let (rows, sel) = palette.visible(max);
        assert_eq!(rows.len(), max);
        assert_eq!(sel, Some(0));
        // Move past the window; the selection pins to the bottom visible row.
        for _ in 0..7 {
            palette.move_selection(1);
        }
        assert_eq!(palette.selected, Some(7));
        let (rows, sel) = palette.visible(max);
        assert_eq!(rows.len(), max);
        assert_eq!(sel, Some(max - 1), "selection pinned to bottom row when scrolled");
        // The bottom visible row is the actually-selected item (field .label).
        assert_eq!(rows[max - 1].label, ITEMS[palette.filtered[7]].label);
    }

    #[test]
    fn profile_modes_keep_their_dismissible_picker_identity() {
        let mut palette = CommandPalette::new();
        assert!(!palette.is_picker());

        palette.open();
        assert!(!palette.is_picker(), "full palette keeps its search focus");

        palette.open_profiles();
        assert!(palette.is_picker(), "new-tab shell selector dismisses on focus loss");

        palette.close();
        assert!(!palette.is_picker());

        palette.open_default_picker();
        assert!(palette.is_picker(), "default-shell selector shares dismissal semantics");
    }

    #[test]
    fn profile_picker_query_filters_rows() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&[], &["Windows PowerShell".into(), "Git Bash".into()]);
        palette.open_profiles();

        palette.input_text("git");
        let (rows, _) = palette.visible(8);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Git Bash");
    }

    #[test]
    fn search_input_and_result_rows_share_compact_height() {
        let layout = palette_layout(1600.0, 900.0, 1.5);
        assert_eq!(layout.input.3, layout.row_h);
    }

    #[test]
    fn select_all_copy_and_paste_replace_the_query() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.input_text("old query");
        palette.select_all();
        assert_eq!(palette.selected_text().as_deref(), Some("old query"));
        palette.input_text("new\r\nquery");
        assert_eq!(palette.query(), "newquery");
        assert!(!palette.query_all_selected());
    }
}
