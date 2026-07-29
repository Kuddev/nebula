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

use std::path::PathBuf;

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

/// 命令面板只负责展示目录；候选的匹配与 frecency 排序由共享目录服务完成，
/// 避免 UI 再维护一套会逐渐分叉的目录搜索规则。
#[derive(Debug, Clone)]
struct DirectoryRow {
    path: PathBuf,
    label: String,
    hint: String,
}

impl DirectoryRow {
    fn new(path: PathBuf) -> Self {
        let label = path
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let hint = path.display().to_string();
        Self { path, label, hint }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaletteCandidate {
    Item(usize),
    Profile(usize),
    Directory(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteMode {
    Commands,
    Profiles,
    DefaultShell,
    Directories,
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
    /// Open the frecency-ranked directory picker; this is a UI workflow, not a
    /// shell-specific command or alias.
    OpenDirectoryPicker,
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
    /// Open a local terminal whose PTY starts directly in this directory.
    NewAtDirectory(PathBuf),
    ToggleFilesPanel,
    ToggleGitPanel,
    /// Save every terminal tab as a workspace file.
    ExportWorkspace,
    /// Pick a workspace file and append its tabs to this window.
    ImportWorkspace,
    /// WebDAV 同步（spec 003）：推送本机设置到远端。
    SyncPush,
    /// WebDAV 同步：拉取远端设置并合并到本机。
    SyncPull,
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
        label: "在常用目录中新建终端…",
        hint: "",
        search: "在常用目录中新建终端 new terminal in frequent directory changyong mulu",
        action: PaletteAction::OpenDirectoryPicker,
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
        label: "导出工作区…",
        hint: "",
        search: "导出工作区 export workspace save session daochu gongzuoqu",
        action: PaletteAction::ExportWorkspace,
    },
    PaletteItem {
        label: "打开工作区…",
        hint: "",
        search: "打开工作区 open import workspace load session dakai daoru gongzuoqu",
        action: PaletteAction::ImportWorkspace,
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
        label: "同步：推送设置到云端",
        hint: "",
        search: "同步推送设置到云端 webdav sync push upload settings tongbu tuisong",
        action: PaletteAction::SyncPush,
    },
    PaletteItem {
        label: "同步：从云端拉取设置",
        hint: "",
        search: "同步从云端拉取设置 webdav sync pull download settings tongbu laqu",
        action: PaletteAction::SyncPull,
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
    /// 已排序的可见候选。显式区分类型，避免动态列表长度变化后用“偏移量索引”
    /// 把目录误解释成 Profile 或静态命令。
    filtered: Vec<PaletteCandidate>,
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
    /// Stable id of the default shell, used by the shell/profile picker badge.
    default_shell_id: Option<String>,
    /// Frecency-ranked directory rows supplied by `DirectoryHistory`.
    directories: Vec<DirectoryRow>,
    mode: PaletteMode,
    /// Mouse-hovered row within the visible window (`None` when not hovering).
    hover: Option<usize>,
    /// 打开时武装：指针从首次上报位置真正移动（>2px）前不点亮 hover。
    /// 「+」的下拉紧贴按钮弹出，首行往往恰在指针正下方——立即点亮会被
    /// 读成「PowerShell 被默认选中」（2026-07-28 用户反馈：全部待选）。
    hover_armed: bool,
    /// 武装期间首次上报的指针位置（解除武装的位移基准）。
    pointer_baseline: Option<(f32, f32)>,
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
            default_shell_id: None,
            directories: Vec::new(),
            mode: PaletteMode::Commands,
            hover: None,
            hover_armed: false,
            pointer_baseline: None,
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
        self.refilter();
    }

    /// Whether the palette is in default-shell picking mode.
    pub fn is_picking_default(&self) -> bool {
        self.mode == PaletteMode::DefaultShell
    }

    /// Shell/profile selector mode. This identity is used only for natural
    /// dismissal on window focus loss; the selector remains searchable because
    /// a long shell/profile list otherwise becomes needlessly hard to scan.
    pub fn is_picker(&self) -> bool {
        self.open && self.mode != PaletteMode::Commands
    }

    pub fn is_picking_directory(&self) -> bool {
        self.open && self.mode == PaletteMode::Directories
    }

    /// 图4 版式：shell picker 空查询时，首行（默认 shell）渲染为"推荐"
    /// 大卡片，其余行归入"所有选项"。搜索中或无默认时退回平铺卡片列表。
    pub fn hero_row(&self) -> bool {
        self.mode == PaletteMode::Profiles
            && self.query.is_empty()
            && matches!(
                self.filtered.first(),
                Some(&PaletteCandidate::Profile(index)) if matches!(
                    &self.profiles[index],
                    ProfileRow::Shell { shell, .. }
                        if self.default_shell_id.as_deref() == Some(shell.id.as_str())
                )
            )
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
    pub fn set_shell_menu(
        &mut self,
        shells: &[DetectedShell],
        profiles: &[String],
        default_shell_id: &str,
    ) {
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
        // 图4 版式：默认 shell 是"推荐"大卡片，必须占首行 —— Enter 直接
        // 打开推荐项，检测顺序不再决定谁排第一。
        if let Some(position) = rows.iter().position(|row| {
            matches!(row, ProfileRow::Shell { shell, .. } if shell.id == default_shell_id)
        }) {
            let default_row = rows.remove(position);
            rows.insert(0, default_row);
        }
        self.profiles = rows;
        self.default_shell_id = Some(default_shell_id.to_owned());
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
        self.default_shell_id = None;
    }

    pub fn set_directories(&mut self, paths: Vec<PathBuf>) {
        self.directories = paths.into_iter().map(DirectoryRow::new).collect();
        if self.mode == PaletteMode::Directories {
            self.refilter();
        }
    }

    /// Open (or re-open) the palette with a cleared query and the full list.
    pub fn open(&mut self) {
        self.arm_pointer_hover();
        self.open = true;
        self.mode = PaletteMode::Commands;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    /// Open showing only the quick-launch profiles (the "+" dropdown).
    pub fn open_profiles(&mut self) {
        self.arm_pointer_hover();
        self.open = true;
        self.mode = PaletteMode::Profiles;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    /// Open the default-shell picker (settings row): profile rows only, and
    /// confirm SETS the default rather than launching.
    pub fn open_default_picker(&mut self) {
        self.arm_pointer_hover();
        self.open = true;
        self.mode = PaletteMode::DefaultShell;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    /// Open the generic directory picker. Search results are refreshed by
    /// `Display` after every query edit so ranking remains owned by one service.
    pub fn open_directories(&mut self) {
        self.arm_pointer_hover();
        self.open = true;
        self.mode = PaletteMode::Directories;
        self.query.clear();
        self.query_selection.clear();
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.mode = PaletteMode::Commands;
        self.hover = None;
        self.query_selection.clear();
        self.default_shell_id = None;
    }

    /// 指针驱动的 hover 更新（武装门在此）：打开后指针必须从首次上报
    /// 位置移动超过 2px 才开始点亮；解除一次后恢复普通 hover 跟随。
    pub fn pointer_hover(&mut self, pos: (f32, f32), row: Option<usize>) -> bool {
        if self.hover_armed {
            match self.pointer_baseline {
                None => {
                    self.pointer_baseline = Some(pos);
                    return self.set_hover(None);
                },
                Some(base)
                    if (base.0 - pos.0).abs() < 2.0 && (base.1 - pos.1).abs() < 2.0 =>
                {
                    return self.set_hover(None);
                },
                Some(_) => {
                    self.hover_armed = false;
                    self.pointer_baseline = None;
                },
            }
        }
        self.set_hover(row)
    }

    /// 每次打开（任何模式）都重新武装 hover。
    fn arm_pointer_hover(&mut self) {
        self.hover_armed = true;
        self.pointer_baseline = None;
        self.hover = None;
    }

    /// Update hover based on mouse position. `row` is the index within the
    /// visible window (`0..max_rows`), or `None` when the mouse left. Returns
    /// whether the hover changed, so the caller only redraws on transitions.
    pub fn set_hover(&mut self, row: Option<usize>) -> bool {
        if self.hover == row {
            return false;
        }
        self.hover = row;
        true
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
        // Enter executes the first row before arrow navigation. The full
        // command palette also paints that row selected; lightweight pickers
        // start visually neutral per稿二 but keep this efficient keyboard path.
        let selected = self.selected.unwrap_or(0);
        let candidate = *self.filtered.get(selected)?;
        // 必须在 close() 清空模式之前计算动作；旧实现先关闭再判断
        // picking_default，导致“设置默认 Shell”被错误执行成“启动 Shell”。
        let action = match candidate {
            PaletteCandidate::Item(index) => ITEMS[index].action.clone(),
            PaletteCandidate::Profile(profile) => match &self.profiles[profile] {
                ProfileRow::Config { index, .. } => PaletteAction::LaunchProfile(*index),
                ProfileRow::Shell { shell, .. } if self.mode == PaletteMode::DefaultShell => {
                    PaletteAction::SetDefaultShell(shell.clone())
                },
                ProfileRow::Shell { shell, .. } => PaletteAction::LaunchShell(shell.clone()),
            },
            PaletteCandidate::Directory(directory) => {
                PaletteAction::NewAtDirectory(self.directories[directory].path.clone())
            },
        };
        self.close();
        if let PaletteCandidate::Item(index) = candidate {
            self.record_recent(index);
        }
        Some(action)
    }

    /// Confirm the visible row at `row` (0 = topmost visible line, mirroring
    /// [`Self::visible`]'s scroll window) — the mouse-click path.
    pub fn click(&mut self, row: usize, max_rows: usize) -> Option<PaletteAction> {
        if self.filtered.is_empty() || max_rows == 0 {
            return None;
        }
        let start = self.selected.unwrap_or(0).saturating_sub(max_rows - 1);
        let filtered_index = start + row;
        if filtered_index >= self.filtered.len() {
            return None;
        }
        self.selected = Some(filtered_index);
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
        let candidates: Vec<PaletteCandidate> = match self.mode {
            PaletteMode::Commands => (0..ITEMS.len())
                .map(PaletteCandidate::Item)
                .chain((0..self.profiles.len()).map(PaletteCandidate::Profile))
                .collect(),
            PaletteMode::Profiles | PaletteMode::DefaultShell => {
                (0..self.profiles.len()).map(PaletteCandidate::Profile).collect()
            },
            PaletteMode::Directories => {
                (0..self.directories.len()).map(PaletteCandidate::Directory).collect()
            },
        };
        let combined_search = |candidate: PaletteCandidate| -> &str {
            match candidate {
                PaletteCandidate::Item(index) => ITEMS[index].search,
                PaletteCandidate::Profile(index) => self.profiles[index].search(),
                // 目录模式已经由 DirectoryHistory 完成匹配和排序；这里
                // 不再二次模糊排序，以免破坏 frecency 的确定性。
                PaletteCandidate::Directory(_) => "",
            }
        };
        let query = self.query.trim();
        if self.mode == PaletteMode::Directories {
            self.filtered = candidates;
        } else if query.is_empty() {
            let mut order = candidates;
            order.sort_by_key(|candidate| match candidate {
                PaletteCandidate::Item(index) => {
                    self.recent.iter().position(|recent| recent == index).unwrap_or(usize::MAX)
                },
                PaletteCandidate::Profile(_) | PaletteCandidate::Directory(_) => usize::MAX,
            });
            self.filtered = order;
        } else {
            let mut scored: Vec<(i32, PaletteCandidate)> = candidates
                .into_iter()
                .filter_map(|candidate| {
                    fuzzy_score(query, combined_search(candidate)).map(|score| (score, candidate))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, candidate)| candidate).collect();
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
            let rows = self.filtered.iter().take(max_rows).map(|&row| self.row_for(row)).collect();
            let selected = (self.mode == PaletteMode::Commands).then_some(0);
            return (rows, selected);
        };
        // Keep the selection visible: once it passes the last row, scroll so it
        // sits on the bottom line of the window.
        let start = selected.saturating_sub(max_rows - 1);
        let rows =
            self.filtered.iter().skip(start).take(max_rows).map(|&row| self.row_for(row)).collect();
        (rows, Some(selected - start))
    }

    fn row_for(&self, candidate: PaletteCandidate) -> PaletteRow {
        match candidate {
            PaletteCandidate::Item(index) => PaletteRow {
                icon: String::new(),
                color_id: String::new(),
                label: localized_item_label(&ITEMS[index], self.language).to_owned(),
                hint: ITEMS[index].hint.to_string(),
                is_default: false,
            },
            PaletteCandidate::Profile(index) => PaletteRow {
                icon: self.profiles[index].icon().to_string(),
                color_id: self.profiles[index].color_id().to_string(),
                label: self.profiles[index].label().to_string(),
                hint: self.profiles[index].hint().to_string(),
                is_default: matches!(
                    &self.profiles[index],
                    ProfileRow::Shell { shell, .. }
                        if self.default_shell_id.as_deref() == Some(shell.id.as_str())
                ),
            },
            PaletteCandidate::Directory(index) => PaletteRow {
                icon: "\u{f07b}".to_owned(),
                color_id: String::new(),
                label: self.directories[index].label.clone(),
                hint: self.directories[index].hint.clone(),
                is_default: false,
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
        OpenDirectoryPicker => "New terminal in a frequent directory...",
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
        ExportWorkspace => "Export workspace...",
        ImportWorkspace => "Open workspace...",
        SyncPush => "Sync: push settings",
        SyncPull => "Sync: pull settings",
        LaunchProfile(_) | LaunchShell(_) | SetDefaultShell(_) | NewAtDirectory(_) => item.label,
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
    pub is_default: bool,
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
    /// Height of one standard result row.
    pub row_h: f32,
    /// Top Y of the first result row (or of its section header).
    pub list_y: f32,
    /// Maximum rows drawn before the list scrolls.
    pub max_rows: usize,
    /// Keyboard-hint footer for picker modes; absent in the full command list.
    pub footer: Option<(f32, f32, f32, f32)>,
    /// Per-visible-row rects `(y, h)`; x/w span the panel's inner width. The
    /// picker's card geometry is non-uniform (hero card, section gaps), so
    /// rendering AND hit-testing must both read row rects from here instead
    /// of dividing by `row_h`.
    pub rows: Vec<(f32, f32)>,
    /// Section caption baselines for the picker: (推荐, 所有选项).
    pub headers: (Option<f32>, Option<f32>),
}

impl PaletteLayout {
    /// Visible-row index under a point, honoring the non-uniform card
    /// geometry. Points on section captions or card gaps return `None`.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (px, _, pw, _) = self.panel;
        if x < px || x >= px + pw {
            return None;
        }
        self.rows.iter().position(|&(ry, rh)| y >= ry && y < ry + rh)
    }
}

/// Compute the centered popup layout for a window of `win_w` × `win_h`. The
/// command list keeps a fixed panel height (sized for `max_rows`) so it
/// doesn't jump as the match count changes while typing; pickers shrink to
/// their content. Every palette mode uses the same search-input geometry,
/// keeping rendering, hover and click hit-testing on one contract.
pub fn palette_layout(
    model: &CommandPalette,
    win_w: f32,
    win_h: f32,
    scale: f32,
) -> PaletteLayout {
    let s = |v: f32| v * scale;
    let margin = s(8.0);
    let pad = s(12.0);
    let row_h = s(super::design_tokens::control::COMPACT_ROW);
    // 搜索框与结果行等高：输入仍然可发现，但不会压过真正的数据内容。
    let input_h = row_h;
    let cards = model.is_picker();
    let max_rows = if cards { 10usize } else { 8usize };
    let visible = model.filtered.len().min(max_rows);
    let hero = model.hero_row() && visible > 0;
    let footer_h = if cards { s(36.0) } else { 0.0 };
    let header_h = s(24.0);
    let gap = s(6.0);
    let hero_h = s(58.0);

    // List geometry relative to the list's top edge. Pickers lay cards out
    // with section captions and gaps (图4); the command list stays a dense
    // uniform grid.
    let mut rel_rows: Vec<(f32, f32)> = Vec::with_capacity(visible);
    let mut rel_headers: (Option<f32>, Option<f32>) = (None, None);
    let mut y = 0.0f32;
    if cards {
        let slots = visible.max(1);
        let mut rest = slots;
        if hero {
            rel_headers.0 = Some(y);
            y += header_h;
            rel_rows.push((y, hero_h));
            y += hero_h + s(10.0);
            rest = slots - 1;
            if rest > 0 {
                rel_headers.1 = Some(y);
                y += header_h;
            }
        }
        for extra in 0..rest {
            if rel_rows.len() < visible {
                rel_rows.push((y, row_h));
            }
            y += row_h;
            if extra + 1 < rest {
                y += gap;
            }
        }
        y += s(4.0);
    } else {
        for _ in 0..max_rows {
            if rel_rows.len() < visible {
                rel_rows.push((y, row_h));
            }
            y += row_h;
        }
    }

    let pw = s(if cards { 620.0 } else { 640.0 }).min(win_w - 2.0 * margin);
    let ph = pad + input_h + s(8.0) + y + if cards { footer_h } else { pad };
    let px = ((win_w - pw) * 0.5).max(margin);
    let py = ((win_h - ph) * 0.5).max(s(48.0));

    let input = (px + pad, py + pad, pw - 2.0 * pad, input_h);
    let list_y = py + pad + input_h + s(8.0);
    let rows = rel_rows.into_iter().map(|(ry, rh)| (list_y + ry, rh)).collect();
    let headers =
        (rel_headers.0.map(|v| list_y + v), rel_headers.1.map(|v| list_y + v));

    let footer = cards.then_some((px, py + ph - footer_h, pw, footer_h));

    PaletteLayout { panel: (px, py, pw, ph), input, row_h, list_y, max_rows, footer, rows, headers }
}

// ---- rendering (the parent `display::mod` hands in the model + renderer;
// this module owns the palette's pixels — same split as `side_panel.rs`) ----

use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

/// 卡片列的左右内边距，也是**右侧基准线**：输入框的 Ctrl+K 键帽、推荐卡
/// 的 ↵ chip、路径列、命令行的快捷键 chip、底栏的 Esc 提示——所有右贴边
/// 元素都从 `ix + iw - s(GUTTER)` 起算，右缘才连成一条竖线。
///
/// 2026-07-29：此前这里散着 10/12/14 三个值，底栏还错用了 panel 坐标
/// （`px + pw - s(16)`，实际只内缩 4px），右侧看着毛糙且随字号漂移。
const GUTTER: f32 = 14.0;

/// 标签右缘与右侧信息列之间的最小呼吸缝。挤不下就整列让位，绝不重叠。
const HINT_GAP: f32 = 24.0;

/// 文字与相邻 chip（推荐卡的 ↵）之间的缝——比 [`HINT_GAP`] 窄，因为 chip
/// 自带视觉边界，不需要靠空白来分隔。
const CHIP_GAP: f32 = 12.0;

/// Push the palette's background quads: a dim veil over the window, the glass
/// panel (glow + gradient border + solid fill, matching the settings modal),
/// the query input box, and the selected-row
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
    let layout = palette_layout(model, w, h, scale);
    let (px, py, pw, ph) = layout.panel;
    let (ix, iy, iw, ih) = layout.input;

    quads.push(UiQuad::solid(0.0, 0.0, w, h, 0.0, sk.veil));
    quads.push(UiQuad::glow(
        px - s(24.0),
        py - s(22.0),
        pw + s(48.0),
        ph + s(48.0),
        palette.edge_glow_l,
    ));
    // Hairline ring, not a gradient stroke: the picker reads as one soft gray
    // sheet (图4/图5), and the chrome shell stays neutral.
    quads.push(UiQuad::solid(
        px - s(1.0),
        py - s(1.0),
        pw + s(2.0),
        ph + s(2.0),
        s(15.0),
        sk.hairline,
    ));
    // 面板填充是纯色柔和灰：chrome 壳保持中性，此前 panel→term_bg 的渐变
    // 让弹窗在暗色主题下自成一套明暗，与设置模态和侧栏的平面感割裂
    // （浅色主题还因 alpha 差露出脏渐变）。
    quads.push(UiQuad::solid(px, py, pw, ph, s(14.0), sk.panel));

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

    let cell_w = size.cell_width();
    // Ctrl+K 键帽（图4/图8）：仅 shell picker 空查询时展示，指认打开快捷
    // 键；输入开始后让位给查询文字。
    if model.mode == PaletteMode::Profiles && model.query.is_empty() {
        let combo = super::keycap::layout_combo(
            "Ctrl+K",
            ix + iw - s(GUTTER),
            iy + ih / 2.0,
            cell_w,
            scale,
        );
        super::keycap::push_combo(quads, &sk, &combo, scale);
    }

    if let Some((fx, fy, fw, fh)) = layout.footer {
        quads.push(UiQuad::solid(fx, fy, fw, fh, 0.0, sk.surface));
        quads.push(UiQuad::solid(fx, fy, fw, s(1.0), 0.0, sk.hairline));
    }

    let (visible_rows, selected_row) = model.visible(layout.max_rows);
    let cards = model.is_picker();
    let hero = model.hero_row();
    let accent_ring = Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255);
    if cards {
        // 卡片行（图4）：柔和灰面板上的浅色卡片，hairline 圈边。选中 =
        // accent 描边 + 轻染，hover = 中性 wash；推荐大卡片自带 accent
        // 轻染和右侧回车 chip（glyph 在文字 pass 画）。
        for (row, &(ry, rh)) in layout.rows.iter().enumerate() {
            let is_hero = hero && row == 0;
            let selected = selected_row == Some(row);
            let ring = if selected { accent_ring } else { sk.hairline };
            quads.push(UiQuad::solid(
                ix - s(1.0),
                ry - s(1.0),
                iw + s(2.0),
                rh + s(2.0),
                s(9.0),
                ring,
            ));
            quads.push(UiQuad::solid(ix, ry, iw, rh, s(8.0), sk.card));
            if is_hero || selected {
                quads.push(UiQuad::solid(ix, ry, iw, rh, s(8.0), sk.accent_soft));
            }
            if model.hover == Some(row) {
                quads.push(UiQuad::solid(ix, ry, iw, rh, s(8.0), sk.hover));
            }
            if is_hero {
                let chip = s(28.0);
                let cx = ix + iw - s(GUTTER) - chip;
                let cy = ry + (rh - chip) / 2.0;
                quads.push(UiQuad::solid(
                    cx - s(1.0),
                    cy - s(1.0),
                    chip + s(2.0),
                    chip + s(2.0),
                    s(7.0),
                    sk.hairline,
                ));
                quads.push(UiQuad::solid(cx, cy, chip, chip, s(6.0), sk.card));
            }
        }
    } else {
        // Hover background: subtle highlight when the mouse is over a row.
        if let Some(hover_row) = model.hover {
            if let Some(&(ry, rh)) = layout.rows.get(hover_row) {
                quads.push(UiQuad::solid(
                    ix + s(8.0),
                    ry + s(2.0),
                    iw - s(16.0),
                    rh - s(4.0),
                    s(6.0),
                    sk.hover,
                ));
            }
        }
        // Highlight pill behind the selected row (list scrolls to keep it
        // shown).
        if let Some(row) = selected_row {
            if let Some(&(ry, rh)) = layout.rows.get(row) {
                quads.push(UiQuad::solid(
                    ix + s(8.0),
                    ry,
                    iw - s(16.0),
                    rh - s(4.0),
                    s(6.0),
                    sk.accent_soft,
                ));
                quads.push(UiQuad::solid(
                    ix + s(8.0),
                    ry + s(5.0),
                    s(2.0),
                    rh - s(14.0),
                    s(1.0),
                    accent_ring,
                ));
            }
        }
    }

    // The default badge is deliberately a hairline chip, not another bright
    // accent pill. It identifies the launch target without competing with the
    // selected-row affordance. The hero card 已经用"推荐"分区表达默认身份，
    // 不再叠加徽标。
    let text_x = ix + s(14.0);
    let badge = model.language.pick("默认", "Default");
    let badge_w = badge.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w
        + s(12.0);
    for (row, entry) in visible_rows.into_iter().enumerate() {
        let Some(&(ry, rh)) = layout.rows.get(row) else { continue };
        // 图8 规范：命令列表右侧的快捷键 hint 画成逐键 chip 底（键名在文
        // 字 pass）。挤不下就整组让位，不压进标签。
        if !cards && !entry.hint.is_empty() {
            let combo = super::keycap::layout_combo(
                &entry.hint,
                ix + iw - s(GUTTER),
                ry + rh / 2.0,
                cell_w,
                scale,
            );
            let label_end = text_x
                + entry.label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32
                    * cell_w;
            if combo.bounds.0 > label_end + s(HINT_GAP) {
                super::keycap::push_combo(quads, &sk, &combo, scale);
            }
        }
        if !entry.is_default || (hero && row == 0) {
            continue;
        }
        let label_x = if entry.icon.is_empty() { text_x } else { text_x + s(26.0) };
        let label_w = entry.label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32
            * cell_w;
        let bx = label_x + label_w + s(8.0);
        let by = ry + s(7.0);
        let bh = rh - s(14.0);
        quads.push(UiQuad::solid(
            bx - s(1.0),
            by - s(1.0),
            badge_w + s(2.0),
            bh + s(2.0),
            s(5.0),
            sk.hairline,
        ));
        quads.push(UiQuad::solid(bx, by, badge_w, bh, s(4.0), sk.surface));
    }

    if let Some((_, fy, _, fh)) = layout.footer {
        // 图8 键帽规范：底栏键帽与 Ctrl+K/设置页共用同一 chip 配方。底栏
        // 背景横贯面板，但里面的文字与卡片列对齐（走 ix/iw，不是 panel）。
        let key_h = s(super::keycap::KEY_H);
        let key_y = fy + (fh - key_h) * 0.5;
        let mut chip = |quads: &mut Vec<UiQuad>, x: f32, cols: usize| {
            let width = cols as f32 * cell_w + s(8.0);
            super::keycap::push_chip(quads, &sk, x, key_y, width, key_h, scale);
        };
        chip(quads, ix, 3);
        chip(quads, ix + iw * 0.42, 5);
        let esc_cols = text_width_cols(model.language.pick("Esc", "Esc"));
        chip(
            quads,
            ix + iw
                - s(GUTTER)
                - (esc_cols + text_width_cols(model.language.pick(" 关闭", " Close"))) as f32
                    * cell_w,
            esc_cols,
        );
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
    let layout = palette_layout(model, w, h, scale);
    let (ix, iy, iw, ih) = layout.input;

    // Inks from the theme skin: dark text on light panels, pale on dark.
    let sk = theme.skin();

    // Left edge for result text and the search icon.
    let text_x = ix + s(14.0);

    const ICON_SEARCH: &str = "\u{f0349}"; // mdi-magnify
    r.draw_chrome_text(size, text_x, iy + (ih - cell_h) / 2.0, sk.ink_faint, ICON_SEARCH, gc);

    let query_x = text_x + s(28.0);
    let text_y = iy + (ih - cell_h) / 2.0;
    let query = model.query();
    let cursor = if model.cursor_visible() { "▏" } else { "" };

    if query.is_empty() {
        // 空态同时给出输入光标和用途提示，避免只有一个空白色块让用户猜测。
        r.draw_chrome_text(size, query_x, text_y, sk.ink_strong, cursor, gc);
        let placeholder = match model.mode {
            PaletteMode::Commands => model
                .language
                .pick("搜索命令、Shell 或 Profile…", "Search commands, shells or profiles..."),
            PaletteMode::Profiles | PaletteMode::DefaultShell => {
                model.language.pick("搜索 Shell 或 Profile…", "Search shells or profiles...")
            },
            PaletteMode::Directories => {
                model.language.pick("搜索常用目录…", "Search frequent directories...")
            },
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
            match model.mode {
                PaletteMode::Directories => {
                    model.language.pick("没有匹配的已访问目录", "No matching visited directories")
                },
                PaletteMode::Commands | PaletteMode::Profiles | PaletteMode::DefaultShell => {
                    model.language.pick("无匹配命令", "No matching commands")
                },
            },
            gc,
        );
        if let Some((fx, fy, fw, fh)) = layout.footer {
            let footer_y = fy + (fh - cell_h) * 0.5;
            r.draw_chrome_text(size, fx + s(16.0), footer_y, sk.ink_dim, model.language.pick("↑ ↓ 选择", "↑ ↓ Select"), gc);
            r.draw_chrome_text(size, fx + fw * 0.42, footer_y, sk.ink_dim, model.language.pick("Enter 打开", "Enter Open"), gc);
            r.draw_chrome_text(size, fx + fw - s(16.0) - text_width_cols(model.language.pick("Esc 关闭", "Esc Close")) as f32 * cell_w, footer_y, sk.ink_dim, model.language.pick("Esc 关闭", "Esc Close"), gc);
        }
        return icon_draws;
    }

    // Section captions (图4): 推荐 / 所有选项。
    if let Some(hy) = layout.headers.0 {
        r.draw_chrome_text(
            size,
            text_x,
            hy + s(2.0),
            sk.ink_dim,
            model.language.pick("推荐", "Recommended"),
            gc,
        );
    }
    if let Some(hy) = layout.headers.1 {
        r.draw_chrome_text(
            size,
            text_x,
            hy + s(2.0),
            sk.ink_dim,
            model.language.pick("所有选项", "All options"),
            gc,
        );
    }
    // Ctrl+K 键帽文字（chip 底在 quad pass，几何同源）。
    if model.mode == PaletteMode::Profiles && model.query.is_empty() {
        let combo = super::keycap::layout_combo(
            "Ctrl+K",
            ix + iw - s(GUTTER),
            iy + ih / 2.0,
            cell_w,
            scale,
        );
        draw_combo_text(r, gc, size, &combo, cell_w, cell_h, &sk);
    }

    let cards = model.is_picker();
    let hero = model.hero_row();
    let (rows, selected_row) = model.visible(layout.max_rows);
    for (row, entry) in rows.into_iter().enumerate() {
        let PaletteRow { icon, color_id, label, hint, is_default } = entry;
        let Some(&(row_y, row_hh)) = layout.rows.get(row) else { break };
        let is_hero = hero && row == 0;
        // Hero 卡片双行：名称在上、完整路径在下（图4）；普通行单行居中。
        let ry = if is_hero {
            row_y + s(8.0)
        } else {
            row_y + (row_hh - cell_h) / 2.0 - s(2.0)
        };
        let fg = if Some(row) == selected_row || is_hero { sk.ink_strong } else { sk.ink };
        // Leading icon, then the label indented past it. Detected shells with a
        // brand asset stage a full-color textured quad (drawn later); the rest
        // fall back to the Nerd Font glyph. Built-in action rows carry an empty
        // icon and keep the original left edge.
        let has_color =
            !color_id.is_empty() && crate::shell_detect::color_icon_png(&color_id).is_some();
        let indent = if is_hero { s(34.0) } else { s(26.0) };
        let label_x = if has_color {
            // Square icon sized to the glyph ink, vertically centered on the
            // row (the hero card gets a bigger brand mark).
            let icon_s = if is_hero { (cell_h * 1.35).round() } else { (cell_h * 0.92).round() };
            let icon_y = (row_y + (row_hh - icon_s) / 2.0).round();
            icon_draws.push((color_id, (text_x, icon_y, icon_s, icon_s)));
            text_x + indent
        } else if icon.is_empty() {
            text_x
        } else {
            let icon_y = row_y + (row_hh - cell_h) / 2.0;
            r.draw_chrome_text(size, text_x, icon_y, sk.accent, &icon, gc);
            text_x + indent
        };
        r.draw_chrome_text(size, label_x, ry, fg, &label, gc);
        let badge = model.language.pick("默认", "Default");
        let badge_w = if is_default && !is_hero {
            let width = badge.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32
                * cell_w
                + s(12.0);
            r.draw_chrome_text(
                size,
                label_x + label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32
                    * cell_w
                    + s(10.0),
                ry,
                sk.ink_dim,
                badge,
                gc,
            );
            width + s(10.0)
        } else {
            0.0
        };
        if is_hero {
            // 第二行完整路径（塞不下时尾部省略号），右侧回车 chip glyph。
            let chip = s(28.0);
            let chip_x = ix + iw - s(GUTTER) - chip;
            let path_y = row_y + row_hh - cell_h - s(8.0);
            let budget = ((chip_x - s(CHIP_GAP) - label_x) / cell_w).floor();
            if budget >= 3.0 {
                let shown = fit_head(&hint, budget as usize);
                r.draw_chrome_text(size, label_x, path_y, sk.ink_dim, &shown, gc);
            }
            let chip_text_y = row_y + (row_hh - cell_h) / 2.0;
            r.draw_chrome_text(
                size,
                chip_x + (chip - cell_w) / 2.0,
                chip_text_y,
                sk.accent,
                "↵",
                gc,
            );
        } else if !hint.is_empty() {
            if !cards {
                // 图8 规范：命令列表快捷键 hint 逐键 chip；与 quad pass 用
                // 同一 layout_combo 几何与让位守卫。
                let combo = super::keycap::layout_combo(
                    &hint,
                    ix + iw - s(GUTTER),
                    row_y + row_hh / 2.0,
                    cell_w,
                    scale,
                );
                let label_end = ix + s(GUTTER)
                    + label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32
                        * cell_w;
                if combo.bounds.0 > label_end + s(HINT_GAP) {
                    draw_combo_text(r, gc, size, &combo, cell_w, cell_h, &sk);
                }
            } else if let Some((hint_x, shown)) =
                fit_hint(label_x, &label, badge_w, ix, iw, cell_w, scale, &hint)
            {
                r.draw_chrome_text(size, hint_x, ry, sk.ink_dim, &shown, gc);
            }
        }
    }
    if let Some((_, fy, _, fh)) = layout.footer {
        let footer_y = fy + (fh - cell_h) * 0.5;
        r.draw_chrome_text(
            size,
            ix,
            footer_y,
            sk.ink_dim,
            model.language.pick("↑ ↓ 选择", "↑ ↓ Select"),
            gc,
        );
        r.draw_chrome_text(
            size,
            ix + iw * 0.42,
            footer_y,
            sk.ink_dim,
            model.language.pick("Enter 打开", "Enter Open"),
            gc,
        );
        r.draw_chrome_text(
            size,
            ix + iw - s(GUTTER) - text_width_cols(model.language.pick("Esc 关闭", "Esc Close"))
                as f32
                * cell_w,
            footer_y,
            sk.ink_dim,
            model.language.pick("Esc 关闭", "Esc Close"),
            gc,
        );
    }
    icon_draws
}

/// 图8 键帽文字：逐键 chip 内水平居中键名、弱墨 "+" 分隔；几何来自
/// `keycap::layout_combo`，与 quad pass 同源。
fn draw_combo_text(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    combo: &super::keycap::ComboChips,
    cell_w: f32,
    cell_h: f32,
    sk: &super::theme::Skin,
) {
    let (_, key_y, _, key_h) = combo.bounds;
    let ty = key_y + (key_h - cell_h) / 2.0;
    for (chip_x, chip_w, key) in &combo.chips {
        let key_cols: usize = key.chars().map(|c| c.width().unwrap_or(0)).sum();
        r.draw_chrome_text(
            size,
            chip_x + (chip_w - key_cols as f32 * cell_w) / 2.0,
            ty,
            sk.ink_dim,
            key,
            gc,
        );
    }
    for plus_x in &combo.pluses {
        r.draw_chrome_text(size, *plus_x, ty, sk.ink_faint, "+", gc);
    }
}

fn text_width_cols(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Lay out a row's right-aligned hint next to its left-aligned label: returns
/// the draw X and the (possibly truncated) text, or `None` when fewer than 3
/// columns remain. Pure so the no-collision contract is unit-testable.
///
/// 2026-07-27 用户反馈"字号放大后 powershell 路径叠进标签"：旧预算
/// `iw - label_x - s(28)` 把宽度 `iw` 和绝对坐标 `label_x` 混在一起减——
/// 面板是居中的，窗口一宽 `label_x` 随之变大，差值变负后被 `as usize`
/// 钳成 0，`> 2` 的守卫反而放行了完整路径，57 列的 powershell.exe 右对
/// 齐后一头扎回标签里；字号越大 cell_w 越宽，扎得越深。现在从标签右缘
/// + 呼吸缝起算，CJK 按双列计宽（目录 picker 的 hint 是可含中文的路径，
/// 按 char 数右对齐会溢出面板）。2026-07-29 裁定：溢出时尾部省略号、保
/// 留盘符/根上下文（图2/图4）——叶名辨识交给 label 和推荐卡片的完整路径。
fn fit_hint(
    label_x: f32,
    label: &str,
    after_label: f32,
    ix: f32,
    iw: f32,
    cell_w: f32,
    scale: f32,
    hint: &str,
) -> Option<(f32, String)> {
    let cols = |t: &str| -> usize { t.chars().map(|c| c.width().unwrap_or(0)).sum() };
    let label_end = label_x + cols(label) as f32 * cell_w + after_label;
    let right = ix + iw - GUTTER * scale;
    let budget = ((right - label_end - HINT_GAP * scale) / cell_w).floor();
    if budget < 3.0 {
        return None;
    }
    let shown = fit_head(hint, budget as usize);
    let x = right - cols(&shown) as f32 * cell_w;
    Some((x, shown))
}

/// Paths keep their root context; overflow is cut at the tail with an
/// ellipsis, matching the mockup's right-aligned path column.
fn fit_head(value: &str, budget: usize) -> String {
    let width = |ch: char| ch.width().unwrap_or(0);
    let total: usize = value.chars().map(width).sum();
    if total <= budget {
        return value.to_owned();
    }
    if budget <= 1 {
        return "…".to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let w = width(ch);
        if used + w > budget - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_all_in_declaration_order() {
        let palette = CommandPalette::new();
        assert_eq!(palette.filtered.len(), ITEMS.len());
        assert_eq!(
            palette.filtered[0],
            PaletteCandidate::Item(0),
            "first declared item leads by default"
        );
    }

    #[test]
    fn recent_actions_surface_first_on_empty_query() {
        let mut palette = CommandPalette::new();
        palette.record_recent(3);
        palette.record_recent(7);
        palette.refilter();
        // Most-recent first, then the previous recent, then the declared rest.
        assert_eq!(palette.filtered[0], PaletteCandidate::Item(7));
        assert_eq!(palette.filtered[1], PaletteCandidate::Item(3));
        assert_eq!(palette.filtered[2], PaletteCandidate::Item(0));
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
        let PaletteCandidate::Item(picked) = picked else { panic!("expected static item") };
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
        let PaletteCandidate::Item(index) = palette.filtered[7] else {
            panic!("expected static item")
        };
        assert_eq!(rows[max - 1].label, ITEMS[index].label);
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

        palette.set_directories(vec![PathBuf::from("D:/workspace")]);
        palette.open_directories();
        assert!(palette.is_picker(), "directory selector shares dismissal semantics");
    }

    #[test]
    fn default_shell_confirmation_preserves_picker_mode_until_action_is_built() {
        let shell = DetectedShell {
            name: "PowerShell".into(),
            id: "pwsh".into(),
            program: "pwsh.exe".into(),
            args: vec![],
        };
        let mut palette = CommandPalette::new();
        palette.set_default_shell_menu(std::slice::from_ref(&shell));
        palette.open_default_picker();

        assert_eq!(palette.confirm(), Some(PaletteAction::SetDefaultShell(shell)));
    }

    #[test]
    fn directory_picker_returns_path_without_inventing_a_shell_command() {
        let path = PathBuf::from("D:/项目 空间");
        let mut palette = CommandPalette::new();
        palette.set_directories(vec![path.clone()]);
        palette.open_directories();
        let (rows, selected) = palette.visible(8);

        assert_eq!(selected, None);
        assert_eq!(rows[0].hint, path.display().to_string());
        assert_eq!(palette.confirm(), Some(PaletteAction::NewAtDirectory(path)));
    }

    #[test]
    fn profile_picker_query_filters_rows() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[],
            &["Windows PowerShell".into(), "Git Bash".into()],
            "powershell",
        );
        palette.open_profiles();

        palette.input_text("git");
        let (rows, _) = palette.visible(8);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Git Bash");
    }

    #[test]
    fn search_input_and_result_rows_share_compact_height() {
        let palette = CommandPalette::new();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.5);
        assert_eq!(layout.input.3, layout.row_h);
    }

    fn shell(name: &str, id: &str, program: &str) -> DetectedShell {
        DetectedShell {
            name: name.into(),
            id: id.into(),
            program: program.into(),
            args: vec![],
        }
    }

    #[test]
    fn shell_menu_promotes_default_to_hero_row() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[
                shell("CMD", "cmd", r"C:\Windows\System32\cmd.exe"),
                shell("PowerShell", "powershell", r"C:\Windows\...\powershell.exe"),
            ],
            &[],
            "powershell",
        );
        palette.open_profiles();
        // 检测顺序把 CMD 排前，但推荐位（首行大卡片）必须是默认 shell。
        assert!(palette.hero_row());
        let (rows, _) = palette.visible(10);
        assert_eq!(rows[0].label, "PowerShell");
        assert!(rows[0].is_default);
        // 搜索中不分区：hero 退场，回到平铺卡片。
        palette.input_char('c');
        assert!(!palette.hero_row());
    }

    #[test]
    fn picker_layout_rows_are_non_uniform_and_hit_test_matches() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[
                shell("CMD", "cmd", r"C:\Windows\System32\cmd.exe"),
                shell("PowerShell", "powershell", r"C:\Windows\...\powershell.exe"),
            ],
            &[],
            "powershell",
        );
        palette.open_profiles();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.0);
        assert_eq!(layout.rows.len(), 2);
        let (hero_y, hero_h) = layout.rows[0];
        let (row_y, row_h) = layout.rows[1];
        assert!(hero_h > row_h, "推荐大卡片必须比普通卡片高");
        assert!(layout.headers.0.is_some() && layout.headers.1.is_some());
        // 命中测试与逐行矩形一致；卡片之间的缝隙与分区标题不可点。
        let (px, ..) = layout.panel;
        assert_eq!(layout.row_at(px + 10.0, hero_y + hero_h / 2.0), Some(0));
        assert_eq!(layout.row_at(px + 10.0, row_y + row_h / 2.0), Some(1));
        assert_eq!(layout.row_at(px + 10.0, hero_y - 2.0), None, "分区标题不是行");
        assert_eq!(layout.row_at(px - 20.0, hero_y + 2.0), None, "面板外不命中");
    }

    #[test]
    fn command_layout_rows_stay_uniform() {
        let mut palette = CommandPalette::new();
        palette.open();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.0);
        assert_eq!(layout.rows.len(), layout.max_rows.min(palette.filtered.len()));
        for pair in layout.rows.windows(2) {
            assert_eq!(pair[0].1, pair[1].1, "命令列表保持等高行");
            assert_eq!(pair[1].0 - pair[0].0, pair[0].1, "无缝隙");
        }
    }

    #[test]
    fn hint_never_overlaps_label_on_a_wide_window() {
        // Maximized-wide geometry: the panel is centered, so the label's
        // absolute x exceeds the panel WIDTH. The old budget subtracted that
        // absolute x from the width, went negative, was clamped to 0 by
        // `as usize`, and the `> 2` guard then let the full 57-column path
        // through — right-aligned straight into the label.
        let (ix, iw, cell_w, scale) = (960.0, 770.0, 14.0, 1.25);
        let label = "Windows PowerShell";
        let label_x = ix + 50.0;
        let hint = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let (hint_x, shown) = fit_hint(label_x, label, 0.0, ix, iw, cell_w, scale, hint).unwrap();
        let label_end = label_x + label.chars().count() as f32 * cell_w;
        assert!(hint_x >= label_end, "hint at {hint_x} starts inside the label (ends {label_end})");
        // 2026-07-29 裁定：溢出保头部（盘符/根上下文），尾部省略号。
        assert!(shown.starts_with("C:\\Windows") && shown.ends_with('…'), "got {shown}");
    }

    #[test]
    fn hint_is_dropped_when_the_label_leaves_no_room() {
        let label = "启动：一个足够长到吃掉整行宽度的 profile 名字标签";
        assert_eq!(fit_hint(500.0, label, 0.0, 100.0, 600.0, 12.0, 1.0, r"C:\x"), None);
    }

    #[test]
    fn cjk_hint_right_aligns_by_display_columns_not_chars() {
        // Directory-picker hints are paths that may contain CJK; counting
        // chars instead of columns pushed them past the panel's right edge.
        let (ix, iw, cell_w) = (0.0, 640.0, 10.0);
        let hint = "D:/项目/子目录";
        let (hint_x, shown) = fit_hint(ix + 40.0, "口", 0.0, ix, iw, cell_w, 1.0, hint).unwrap();
        assert_eq!(shown, hint, "plenty of room: no truncation");
        let cols: usize = shown.chars().map(|c| c.width().unwrap_or(0)).sum();
        assert_eq!(hint_x, ix + iw - 14.0 - cols as f32 * cell_w);
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
