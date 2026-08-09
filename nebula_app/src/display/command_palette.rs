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
use crate::config::ui_config::Profile;
use crate::shell_detect::DetectedShell;
use unicode_width::UnicodeWidthChar;

/// A dynamic quick-launch row: a config profile (launched by index) or a
/// detected shell (spec carried inline). Built fresh on every menu open.
#[derive(Debug, Clone)]
enum ProfileRow {
    /// Config profile at this index — routed through `TabRequest::NewProfile`.
    Config { label: String, hint: String, search: String, profile: Profile },
    /// Detected shell — routed through `TabRequest::NewShell`. `hint` is the
    /// program path, shown dimmed — the familiar profile-menu layout.
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
            Self::Config { hint, .. } => hint,
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
            Self::Config { profile, .. } => profile
                .shell_id
                .as_deref()
                .map(crate::shell_detect::icon_for_id)
                .unwrap_or("\u{ea60}"),
        }
    }

    /// Stable shell id for the full-color brand icon lookup, or `""` for
    /// config profiles (which have no brand asset and keep their glyph).
    fn color_id(&self) -> &str {
        match self {
            Self::Shell { shell, .. } => &shell.id,
            Self::Config { profile, .. } => profile.shell_id.as_deref().unwrap_or(""),
        }
    }

    fn is_shell(&self) -> bool {
        matches!(self, Self::Shell { .. } | Self::Config { .. })
    }
}

/// An SSH destination shown by the launcher. The display label may be a saved
/// alias while `host` remains the exact launch payload.
#[derive(Debug, Clone)]
struct SshRow {
    label: String,
    hint: String,
    search: String,
    host: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherFilter {
    All,
    Ssh,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LauncherGroup {
    Recommended,
    Shell,
    Ssh,
}

impl LauncherGroup {
    fn label(self, language: super::UiLanguage) -> &'static str {
        match self {
            Self::Recommended => language.pick("推荐", "Recommended"),
            Self::Shell => language.pick("所有 Shell", "All shells"),
            Self::Ssh => language.pick("SSH 主机", "SSH hosts"),
        }
    }
}

impl LauncherFilter {
    fn label(self, language: super::UiLanguage) -> &'static str {
        match self {
            Self::All => language.pick("全部", "All"),
            Self::Ssh => "SSH",
            Self::Shell => "Shell",
        }
    }

    fn next(self, delta: i32) -> Self {
        const FILTERS: [LauncherFilter; 3] =
            [LauncherFilter::All, LauncherFilter::Ssh, LauncherFilter::Shell];
        let index = FILTERS.iter().position(|filter| *filter == self).unwrap_or(0) as i32;
        FILTERS[(index + delta).rem_euclid(FILTERS.len() as i32) as usize]
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

/// 「恢复 AI 会话」模式的一行：标题 + 右侧「位置 · 相对时间」+ 确认时敲进
/// 终端的 resume 命令行。由 `Display` 用 `ai_sessions::scan` 的结果构造。
pub struct AiSessionRow {
    pub label: String,
    pub hint: String,
    pub search: String,
    pub command: String,
    /// claude / codex——决定行首的品牌 logo 与右缘的来源 chip。
    pub source: crate::ai_sessions::AiSessionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaletteCandidate {
    Item(usize),
    Profile(usize),
    Ssh(usize),
    Directory(usize),
    AiSession(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteMode {
    Commands,
    Profiles,
    DefaultShell,
    Directories,
    AiSessions,
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
    /// 复制聚焦 pane 的工作目录到剪贴板。
    CopyCwd,
    /// 在系统文件管理器里打开工作目录。
    RevealCwd,
    /// 折叠 / 展开左侧标签侧栏。
    ToggleSidebar,
    /// 设置·交互「拖拽调节侧栏宽度」开关（关→开要过确认框）。
    TogglePanelResize,
    /// Open the frecency-ranked directory picker; this is a UI workflow, not a
    /// shell-specific command or alias.
    OpenDirectoryPicker,
    /// 打开「恢复 AI 会话」列表（claude / codex 的本地历史会话）。
    OpenAiSessionPicker,
    /// 把这条 resume 命令行敲进当前聚焦的终端执行。
    ResumeAiSession(String),
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
    LaunchProfile(Profile),
    /// Launch a detected shell (the new-tab dropdown) in a new tab.
    LaunchShell(DetectedShell),
    /// Launch a saved SSH destination in a new tab.
    LaunchSsh(String),
    /// Set a detected shell as the default (the settings "默认 Shell" picker).
    SetDefaultShell(DetectedShell),
    SetDefaultProfile(Profile),
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
        label: "复制路径",
        hint: "",
        search: "复制路径 copy path cwd working directory fuzhi lujing gongzuo mulu",
        action: PaletteAction::CopyCwd,
    },
    PaletteItem {
        label: "在资源管理器中显示",
        hint: "",
        search: "在资源管理器中显示 reveal open explorer file manager folder ziyuan guanliqi \
                 wenjianjia",
        action: PaletteAction::RevealCwd,
    },
    PaletteItem {
        label: "在常用目录中新建终端…",
        hint: "",
        search: "在常用目录中新建终端 new terminal in frequent directory changyong mulu",
        action: PaletteAction::OpenDirectoryPicker,
    },
    PaletteItem {
        // 母命令：一个入口通向所有「名词」（AI 会话 / 目录 / SSH / 标签页），
        // 而不是每加一个品类就往命令表里塞一条子命令——那样这张表迟早会被
        // 名词淹掉。目前只列 AI 会话，其余品类随面板打磨一起接进来。
        label: "快速跳转…",
        hint: "Ctrl+Shift+O",
        search: "快速跳转 恢复 AI 会话 open quickly jump resume ai session claude codex kuaisu tiaozhuan huifu",
        action: PaletteAction::OpenAiSessionPicker,
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
        hint: "Ctrl+Shift+F",
        search: "目录树面板 files tree explorer panel mulushu wenjian",
        action: PaletteAction::ToggleFilesPanel,
    },
    PaletteItem {
        label: "显示标签侧栏",
        hint: "",
        search: "显示标签侧栏 toggle show hide tab sidebar xianshi biaoqian celan",
        action: PaletteAction::ToggleSidebar,
    },
    PaletteItem {
        label: "拖拽调节侧栏宽度",
        hint: "",
        search: "拖拽调节侧栏宽度 drag resize sidebar drawer panel width tuozhuai tiaojie kuandu",
        action: PaletteAction::TogglePanelResize,
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

/// 「工作目录」表头右缘那段路径的列宽上限。超出从头部省略——超过这个宽度
/// 后路径就开始跟表头横线抢地方，横线一短，分组的视觉边界就散了。
const CWD_CONTEXT_COLS: usize = 40;

/// 命令列表左侧勾选态列的宽度。与带图标行的缩进（`s(26.0)`）取同一个值：
/// shell 行画品牌标、命令行画 ✓ 或留空，三种行的标签因此落在同一个 x 上。
const CHECK_COL_W: f32 = 26.0;

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
    /// 可见窗口的首项，必须与 `selected` 分开：滚轮/滑块只移动视口，不能让
    /// Enter 悄悄执行已经滚出屏幕的旧选中项。
    scroll_offset: usize,
    /// 拖动滑块时，指针相对滑块顶部的偏移；保留它才能避免按下瞬间跳位。
    scrollbar_drag: Option<f32>,
    /// Recently-run `ITEMS` indices, most-recent first (deduped, capped at
    /// `RECENT_MAX`). Lifts frequent actions to the top of an empty query.
    /// Static items only: profile indices shift whenever the config changes.
    recent: Vec<usize>,
    /// Dynamic quick-launch rows, refreshed on every open so live config
    /// reloads and shell (re)detection are picked up. In profiles-only (the
    /// new-tab dropdown) these are detected shells + config profiles; in the
    /// full palette they're the config profiles appended after the actions.
    profiles: Vec<ProfileRow>,
    /// Saved/configured SSH destinations used only by the launcher picker.
    ssh_hosts: Vec<SshRow>,
    /// Stable id of the default shell, used by the shell/profile picker badge.
    default_shell_id: Option<String>,
    launcher_filter: LauncherFilter,
    /// Frecency-ranked directory rows supplied by `DirectoryHistory`.
    directories: Vec<DirectoryRow>,
    /// 「恢复 AI 会话」的行，打开时由 `ai_sessions::scan` 现扫——历史会话
    /// 随时在长，缓存一份很快就是旧账。
    ai_sessions: Vec<AiSessionRow>,
    mode: PaletteMode,
    /// Mouse-hovered row within the visible window (`None` when not hovering).
    hover: Option<usize>,
    launcher_chip_hover: Option<LauncherFilter>,
    /// 打开时武装：指针从首次上报位置真正移动（>2px）前不点亮 hover。
    /// 「+」的下拉紧贴按钮弹出，首行往往恰在指针正下方——立即点亮会被
    /// 读成「PowerShell 被默认选中」（2026-07-28 用户反馈：全部待选）。
    hover_armed: bool,
    /// 武装期间首次上报的指针位置（解除武装的位移基准）。
    pointer_baseline: Option<(f32, f32)>,
    /// 打开这一刻对窗口状态的取样（见 [`PaletteContext`]）。
    context: PaletteContext,
}

/// 命令面板打开时对窗口状态的一次取样。
///
/// 面板是**模态**的：打开期间用户碰不到侧栏、也换不了目录，所以这些值取样
/// 一次就够。让面板每帧回读 `Display` 会把面板的渲染路径和窗口状态绑在
/// 一起——命令行为随后台事件（cwd 上报）在面板打开期间跳变，是比省一次
/// 拷贝更贵的问题。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteContext {
    /// 聚焦 pane 的工作目录。`None` = shell 没上报：此时整个「工作目录」
    /// 组不出现，而不是留一组作用在空路径上的死命令。
    pub cwd: Option<PathBuf>,
    /// 左侧标签侧栏是否展开（勾选态 ✓）。
    pub sidebar: bool,
    /// 「拖拽调节侧栏宽度」是否已开（勾选态 ✓）。
    pub panel_resize: bool,
    /// 新建标签页会不会真的落在 `cwd` 里。配了启动目录时不会——`spawn_tab`
    /// 那边启动目录优先，此时这条命令必须留在「标签页」组，否则组标题就在
    /// 说谎（用户读到「工作目录 · D:\…」，敲下去却开在别处）。
    pub new_tab_inherits_cwd: bool,
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
            scroll_offset: 0,
            scrollbar_drag: None,
            recent: Vec::new(),
            profiles: Vec::new(),
            ssh_hosts: Vec::new(),
            default_shell_id: None,
            launcher_filter: LauncherFilter::All,
            directories: Vec::new(),
            ai_sessions: Vec::new(),
            mode: PaletteMode::Commands,
            hover: None,
            launcher_chip_hover: None,
            hover_armed: false,
            pointer_baseline: None,
            context: PaletteContext::default(),
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

    /// 灌入打开这一刻的窗口状态。cwd 决定「工作目录」组在不在，所以变了
    /// 就得重新过滤——不然 shell 刚上报 cwd 时列表还是旧的那几行。
    pub fn set_context(&mut self, context: PaletteContext) {
        if self.context == context {
            return;
        }
        self.context = context;
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

    /// 命令列表左侧那条**勾选态列**在不在。只有命令面板有开关类命令；
    /// picker 的左列是类型图标 / 品牌标，两者不能共用同一列。
    pub fn has_check_column(&self) -> bool {
        self.open && self.mode == PaletteMode::Commands
    }

    pub fn is_picking_directory(&self) -> bool {
        self.open && self.mode == PaletteMode::Directories
    }

    /// Launcher rows are intentionally uniform. The default target may sort
    /// first and carry a badge, but it must never grow into a taller hero row.
    pub fn hero_row(&self) -> bool {
        false
    }

    /// 打开「恢复 AI 会话」列表。行已按最近活动排好序，这里不再重排。
    pub fn open_ai_sessions(&mut self, rows: Vec<AiSessionRow>) {
        self.ai_sessions = rows;
        self.open = true;
        self.mode = PaletteMode::AiSessions;
        self.query.clear();
        self.query_selection = Default::default();
        self.hover = None;
        self.launcher_chip_hover = None;
        self.hover_armed = false;
        self.pointer_baseline = None;
        self.refilter();
    }

    /// Refresh the dynamic quick-launch rows from the config's profile names.
    /// Called by the full-palette open path so a reloaded config is reflected.
    pub fn set_profiles(&mut self, profiles: &[Profile]) {
        self.profiles = profiles
            .iter()
            .map(|profile| ProfileRow::Config {
                label: profile.name.clone(),
                hint: profile.command.clone(),
                search: format!(
                    "{} {} profile launch connect qidong",
                    profile.name, profile.command
                ),
                profile: profile.clone(),
            })
            .collect();
    }

    /// Populate the new-tab dropdown: detected shells first (installed-shell
    /// order), then config profiles. The label carries no verb prefix here —
    /// this menu IS the shell picker, so bare names read cleaner.
    pub fn set_shell_menu(
        &mut self,
        shells: &[DetectedShell],
        profiles: &[Profile],
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
        rows.extend(profiles.iter().map(|profile| ProfileRow::Config {
            label: profile.name.clone(),
            hint: profile.command.clone(),
            search: format!("{} {} profile launch connect qidong", profile.name, profile.command),
            profile: profile.clone(),
        }));
        // 图4 版式：默认 shell 是"推荐"大卡片，必须占首行 —— Enter 直接
        // 打开推荐项，检测顺序不再决定谁排第一。
        if let Some(position) = rows.iter().position(|row| match row {
            ProfileRow::Shell { shell, .. } => shell.id == default_shell_id,
            ProfileRow::Config { profile, .. } => {
                profile.settings_id().as_deref() == Some(default_shell_id)
            },
        }) {
            let default_row = rows.remove(position);
            rows.insert(0, default_row);
        }
        self.profiles = rows;
        self.default_shell_id = Some(default_shell_id.to_owned());
    }

    /// Refresh SSH rows independently from shell/profile rows. This keeps
    /// saved destinations out of the settings default-shell picker while
    /// still making imported or auto-saved hosts available immediately.
    pub fn set_ssh_hosts(&mut self, hosts: &[(String, String)]) {
        self.ssh_hosts = hosts
            .iter()
            .map(|(label, host)| SshRow {
                label: label.clone(),
                hint: host.clone(),
                search: format!("{label} {host} ssh host remote").to_lowercase(),
                host: host.clone(),
            })
            .collect();
        if self.mode == PaletteMode::Profiles {
            self.refilter();
        }
    }

    /// Populate the settings "默认 Shell" picker: detected shells only (no
    /// config profiles — you can't default to an ssh jump), and confirming
    /// sets the default instead of launching.
    pub fn set_default_shell_menu(
        &mut self,
        shells: &[DetectedShell],
        profiles: &[Profile],
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
        rows.extend(profiles.iter().filter(|profile| profile.settings_id().is_some()).map(
            |profile| ProfileRow::Config {
                label: profile.name.clone(),
                hint: profile.command.clone(),
                search: format!("{} {} shell profile", profile.name, profile.command),
                profile: profile.clone(),
            },
        ));
        self.profiles = rows;
        self.default_shell_id = Some(default_shell_id.to_owned());
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
        self.launcher_filter = LauncherFilter::All;
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
        self.scrollbar_drag = None;
        self.query_selection.clear();
        self.default_shell_id = None;
        self.launcher_filter = LauncherFilter::All;
    }

    pub fn is_launcher(&self) -> bool {
        self.open && self.mode == PaletteMode::Profiles
    }

    pub fn launcher_filter(&self) -> LauncherFilter {
        self.launcher_filter
    }

    pub fn launcher_chip_counts(&self) -> [(LauncherFilter, usize); 3] {
        let shell_count = self.profiles.iter().filter(|row| row.is_shell()).count();
        [
            (LauncherFilter::All, shell_count + self.ssh_hosts.len()),
            (LauncherFilter::Ssh, self.ssh_hosts.len()),
            (LauncherFilter::Shell, shell_count),
        ]
    }

    pub fn set_launcher_filter(&mut self, filter: LauncherFilter) -> bool {
        if !self.is_launcher() || self.launcher_filter == filter {
            return false;
        }
        self.launcher_filter = filter;
        self.refilter();
        true
    }

    pub fn cycle_launcher_filter(&mut self, delta: i32) -> bool {
        self.set_launcher_filter(self.launcher_filter.next(delta))
    }

    /// 指针驱动的 hover 更新（武装门在此）：打开后指针必须从首次上报
    /// 位置移动超过 2px 才开始点亮；解除一次后恢复普通 hover 跟随。
    pub fn pointer_hover(
        &mut self,
        pos: (f32, f32),
        row: Option<usize>,
        chip: Option<LauncherFilter>,
    ) -> bool {
        if self.hover_armed {
            match self.pointer_baseline {
                None => {
                    self.pointer_baseline = Some(pos);
                    return self.set_hover(None, None);
                },
                Some(base) if (base.0 - pos.0).abs() < 2.0 && (base.1 - pos.1).abs() < 2.0 => {
                    return self.set_hover(None, None);
                },
                Some(_) => {
                    self.hover_armed = false;
                    self.pointer_baseline = None;
                },
            }
        }
        self.set_hover(row, chip)
    }

    /// 每次打开（任何模式）都重新武装 hover。
    fn arm_pointer_hover(&mut self) {
        self.hover_armed = true;
        self.pointer_baseline = None;
        self.hover = None;
        self.launcher_chip_hover = None;
    }

    /// Update hover based on mouse position. `row` is the index within the
    /// visible window (`0..max_rows`), or `None` when the mouse left. Returns
    /// whether the hover changed, so the caller only redraws on transitions.
    pub fn set_hover(&mut self, row: Option<usize>, chip: Option<LauncherFilter>) -> bool {
        if self.hover == row && self.launcher_chip_hover == chip {
            return false;
        }
        self.hover = row;
        self.launcher_chip_hover = chip;
        true
    }

    /// 光标该不该画。相位来自 [`ui::caret`](super::ui::caret) 的共享节律，
    /// 与标签重命名框、SSH 编辑器、侧栏搜索框完全一致——打字时常亮、
    /// 停手后按系统的 `GetCaretBlinkTime` 呼吸。
    ///
    /// 这里曾经自带一个 1060ms、占空比 0.5 的 `Pulse`。它的问题不在参数，
    /// 而在于**每个输入框各造一套节律**：同一个窗口里两个光标以不同相位
    /// 闪烁，读起来像界面有两个焦点。
    pub fn cursor_visible(&self) -> bool {
        super::ui::caret::is_on()
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

    /// Move the selection by `delta` rows, wrapping at both ends and keeping it
    /// inside the independently scrollable visible window.
    pub fn move_selection(&mut self, delta: i32, max_rows: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        let current = self.selected.unwrap_or(self.scroll_offset.min(self.filtered.len() - 1));
        let selected = ((current as i32 + delta).rem_euclid(len)) as usize;
        self.selected = Some(selected);
        self.hover = None;

        if max_rows == 0 {
            return;
        }
        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if selected >= self.scroll_offset + max_rows {
            self.scroll_offset = selected + 1 - max_rows;
        }
    }

    /// Confirm the current selection: records it as recent, closes the palette,
    /// and returns the action to run, or `None` when nothing matches.
    pub fn confirm(&mut self) -> Option<PaletteAction> {
        // Enter executes the first row before arrow navigation. The full
        // command palette also paints that row selected; lightweight pickers
        // start visually neutral per稿二 but keep this efficient keyboard path.
        let selected = self.selected.unwrap_or(self.scroll_offset.min(self.filtered.len() - 1));
        let candidate = *self.filtered.get(selected)?;
        // 必须在 close() 清空模式之前计算动作；旧实现先关闭再判断
        // picking_default，导致“设置默认 Shell”被错误执行成“启动 Shell”。
        let action = match candidate {
            PaletteCandidate::Item(index) => ITEMS[index].action.clone(),
            PaletteCandidate::Profile(profile) => match &self.profiles[profile] {
                ProfileRow::Config { profile, .. } if self.mode == PaletteMode::DefaultShell => {
                    PaletteAction::SetDefaultProfile(profile.clone())
                },
                ProfileRow::Config { profile, .. } => PaletteAction::LaunchProfile(profile.clone()),
                ProfileRow::Shell { shell, .. } if self.mode == PaletteMode::DefaultShell => {
                    PaletteAction::SetDefaultShell(shell.clone())
                },
                ProfileRow::Shell { shell, .. } => PaletteAction::LaunchShell(shell.clone()),
            },
            PaletteCandidate::Ssh(index) => {
                PaletteAction::LaunchSsh(self.ssh_hosts[index].host.clone())
            },
            PaletteCandidate::Directory(directory) => {
                PaletteAction::NewAtDirectory(self.directories[directory].path.clone())
            },
            PaletteCandidate::AiSession(session) => {
                PaletteAction::ResumeAiSession(self.ai_sessions[session].command.clone())
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
    /// 滚动窗口的首行下标。选中行越过最后一行时窗口整体下移，让它停在底行。
    ///
    /// 行、表头、点击换算**必须**共用这一个算式。此前它被抄了两份（`visible`
    /// 与 `click` 各一份）而表头那份压根没算，于是列表一滚动，行走了、表头
    /// 还停在开头那套分组上——分组标题指着一批已经滚走的行。
    fn scroll_start(&self, max_rows: usize) -> usize {
        self.scroll_offset.min(self.filtered.len().saturating_sub(max_rows))
    }

    fn max_scroll(&self, max_rows: usize) -> usize {
        self.filtered.len().saturating_sub(max_rows)
    }

    /// 独立滚动视口。已有选中项一旦离开视口，就钳到最近边缘，确保 Enter
    /// 永远只会执行用户当前看得见的行。
    pub fn scroll_by(&mut self, delta: i32, max_rows: usize) -> bool {
        if max_rows == 0 {
            return false;
        }
        let target = (self.scroll_offset as i64 + delta as i64)
            .clamp(0, self.max_scroll(max_rows) as i64) as usize;
        self.set_scroll_offset(target, max_rows)
    }

    fn set_scroll_offset(&mut self, target: usize, max_rows: usize) -> bool {
        let target = target.min(self.max_scroll(max_rows));
        if target == self.scroll_offset {
            return false;
        }
        self.scroll_offset = target;
        self.hover = None;
        if let Some(selected) = self.selected {
            let last = (target + max_rows.saturating_sub(1)).min(self.filtered.len() - 1);
            self.selected = Some(selected.clamp(target, last));
        }
        true
    }

    pub fn scrollbar_press(
        &mut self,
        x: f32,
        y: f32,
        max_rows: usize,
        scrollbar: PaletteScrollbar,
    ) -> bool {
        if !scrollbar.hit_test(x, y) {
            return false;
        }
        let (_, thumb_y, _, thumb_h) = scrollbar.thumb;
        let grab = if y >= thumb_y && y < thumb_y + thumb_h { y - thumb_y } else { thumb_h * 0.5 };
        self.scrollbar_drag = Some(grab);
        self.scrollbar_drag_to(y, max_rows, scrollbar);
        true
    }

    pub fn scrollbar_drag_to(
        &mut self,
        y: f32,
        max_rows: usize,
        scrollbar: PaletteScrollbar,
    ) -> bool {
        let Some(grab) = self.scrollbar_drag else { return false };
        let (_, track_y, _, track_h) = scrollbar.track;
        let (_, _, _, thumb_h) = scrollbar.thumb;
        let travel = (track_h - thumb_h).max(0.0);
        let max_scroll = self.max_scroll(max_rows);
        if travel <= f32::EPSILON || max_scroll == 0 {
            return false;
        }
        let fraction = ((y - grab - track_y) / travel).clamp(0.0, 1.0);
        let target = (fraction * max_scroll as f32).round() as usize;
        self.set_scroll_offset(target, max_rows)
    }

    pub fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    pub fn end_scrollbar_drag(&mut self) -> bool {
        self.scrollbar_drag.take().is_some()
    }

    pub fn click(&mut self, row: usize, max_rows: usize) -> Option<PaletteAction> {
        if self.filtered.is_empty() || max_rows == 0 {
            return None;
        }
        let filtered_index = self.scroll_start(max_rows) + row;
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
    /// 每个可见行前面要不要起一条分组表头：`Some((标签, 右缘上下文))` = 这
    /// 一行是新一组的第一行。上下文为空串表示这一组不挂东西；「工作目录」
    /// 组挂当前 cwd——那一组的动作全作用在它身上，路径写在标题上一次，好过
    /// 每行重复一遍。返回 `None` 表示这个模式不分组（shell 选择器…）。
    ///
    /// 参数是 `max_rows` 而不是可见行数：表头必须落在**滚动之后**的那个窗口
    /// 上（见 [`Self::scroll_start`]），拿行数算不出窗口从哪儿开始。窗口首行
    /// 永远起一条表头（`previous` 从 `None` 开始）——组标题滚走之后，列表顶
    /// 上仍然写着「你在哪一组」。
    ///
    /// 分组的前提是同组的行**连续**，那件事由 [`Self::refilter`] 末尾的稳定
    /// 排序保证。
    fn group_labels(&self, max_rows: usize) -> Option<Vec<Option<(String, String)>>> {
        let start = self.scroll_start(max_rows);
        let window: Vec<PaletteCandidate> =
            self.filtered.iter().skip(start).take(max_rows).copied().collect();
        if window.is_empty() {
            return None;
        }
        match self.mode {
            PaletteMode::AiSessions => {
                let mut labels = Vec::with_capacity(window.len());
                let mut previous = None;
                for candidate in &window {
                    let PaletteCandidate::AiSession(index) = candidate else {
                        labels.push(None);
                        continue;
                    };
                    let source = self.ai_sessions[*index].source;
                    labels.push(
                        (previous != Some(source))
                            .then(|| (source_group_label(source), String::new())),
                    );
                    previous = Some(source);
                }
                Some(labels)
            },
            PaletteMode::Commands => {
                let mut labels = Vec::with_capacity(window.len());
                let mut previous = None;
                for candidate in &window {
                    let group = self.command_group(*candidate);
                    labels.push((previous != Some(group)).then(|| {
                        let context = if group == CommandGroup::Cwd {
                            self.cwd_context()
                        } else {
                            String::new()
                        };
                        (group.label(self.language), context)
                    }));
                    previous = Some(group);
                }
                Some(labels)
            },
            PaletteMode::Profiles => {
                let mut labels = Vec::with_capacity(window.len());
                let mut previous = None;
                for candidate in &window {
                    let Some(group) = self.launcher_group(*candidate) else {
                        labels.push(None);
                        continue;
                    };
                    labels.push(
                        (previous != Some(group))
                            .then(|| (group.label(self.language).to_owned(), String::new())),
                    );
                    previous = Some(group);
                }
                Some(labels)
            },
            _ => None,
        }
    }

    fn profile_is_default(&self, index: usize) -> bool {
        match &self.profiles[index] {
            ProfileRow::Shell { shell, .. } => {
                self.default_shell_id.as_deref() == Some(shell.id.as_str())
            },
            ProfileRow::Config { profile, .. } => profile
                .settings_id()
                .is_some_and(|id| self.default_shell_id.as_deref() == Some(id.as_str())),
        }
    }

    /// Launcher 的推荐身份只来自真实默认 Shell；SSH 没有可靠的使用频率数据，
    /// 不能为了复刻原型而伪造推荐项。开始搜索后隐藏推荐分组，结果只按类别分段。
    fn launcher_group(&self, candidate: PaletteCandidate) -> Option<LauncherGroup> {
        match candidate {
            PaletteCandidate::Profile(index)
                if self.query.trim().is_empty() && self.profile_is_default(index) =>
            {
                Some(LauncherGroup::Recommended)
            },
            PaletteCandidate::Profile(_) => Some(LauncherGroup::Shell),
            PaletteCandidate::Ssh(_) => Some(LauncherGroup::Ssh),
            _ => None,
        }
    }

    /// 「工作目录」表头右缘的路径。原样显示（技术值不美化），过长时砍掉
    /// **头部**——路径的辨识信息在尾部（项目名 / 子目录），砍尾等于把整行
    /// 变成一串没用的盘符。
    fn cwd_context(&self) -> String {
        let Some(cwd) = self.context.cwd.as_ref() else { return String::new() };
        fit_tail(&cwd.display().to_string(), CWD_CONTEXT_COLS)
    }

    /// 一条候选属于哪个分组。动态的 shell / profile 行自成一组。
    fn command_group(&self, candidate: PaletteCandidate) -> CommandGroup {
        let PaletteCandidate::Item(index) = candidate else { return CommandGroup::Shells };
        let group = CommandGroup::of(&ITEMS[index].action);
        // 工作目录组的成立条件逐条命令判定：cwd 未知时整组不存在；「新建
        // 标签页」还要额外满足**它确实会开在那里**（见 `new_tab_inherits_cwd`）。
        // 不满足就退回标签页组——留一条孤儿表头，或者让组标题指着一个命令
        // 并不会去的目录，都比少一行糟。
        if group == CommandGroup::Cwd {
            let inherits =
                ITEMS[index].action != PaletteAction::NewTab || self.context.new_tab_inherits_cwd;
            if self.context.cwd.is_none() || !inherits {
                return CommandGroup::Tabs;
            }
        }
        group
    }

    /// 让同一来源的会话连续排列。排在前面的是**拥有最新一条会话的那个来源**
    /// ——既满足了分组表头「同组成块」的前提，又不至于把「我刚才在做的那件
    /// 事」压到第二组去。组内顺序原样保留（空查询=最近优先，有查询=模糊得分
    /// 优先），所以这里只做一次稳定划分，不重新排序。
    fn group_ai_sessions_by_source(&mut self) {
        let lead = match self.filtered.first() {
            Some(PaletteCandidate::AiSession(index)) => self.ai_sessions[*index].source,
            _ => return,
        };
        let rows = std::mem::take(&mut self.filtered);
        let (mut head, tail): (Vec<_>, Vec<_>) =
            rows.into_iter().partition(|candidate| match candidate {
                PaletteCandidate::AiSession(index) => self.ai_sessions[*index].source == lead,
                _ => true,
            });
        head.extend(tail);
        self.filtered = head;
    }

    /// 一条命令这一轮出不出现在列表里。
    ///
    /// 两类：形态没打磨完的（`OpenAiSessionPicker` —— 筛选芯片、类型标、按
    /// 对象推导的页脚动词都还没做，先不让用户撞见半成品），和依赖的状态压根
    /// 不存在的（cwd 未知时的工作目录组）。
    ///
    /// 放行只需删掉对应分支；不要改 `ITEMS`，那样会连带丢掉搜索词和本地化
    /// 文案。
    fn parked(&self, action: &PaletteAction) -> bool {
        match action {
            PaletteAction::OpenAiSessionPicker => true,
            PaletteAction::CopyCwd | PaletteAction::RevealCwd => self.context.cwd.is_none(),
            _ => false,
        }
    }

    fn refilter(&mut self) {
        let candidates: Vec<PaletteCandidate> = match self.mode {
            PaletteMode::Commands => (0..ITEMS.len())
                .filter(|index| !self.parked(&ITEMS[*index].action))
                .map(PaletteCandidate::Item)
                .chain((0..self.profiles.len()).map(PaletteCandidate::Profile))
                .collect(),
            PaletteMode::Profiles => {
                let profiles = (0..self.profiles.len())
                    .filter(|_| self.launcher_filter != LauncherFilter::Ssh)
                    .map(PaletteCandidate::Profile);
                let ssh = (0..self.ssh_hosts.len())
                    .filter(|_| self.launcher_filter != LauncherFilter::Shell)
                    .map(PaletteCandidate::Ssh);
                profiles.chain(ssh).collect()
            },
            PaletteMode::DefaultShell => {
                (0..self.profiles.len()).map(PaletteCandidate::Profile).collect()
            },
            PaletteMode::Directories => {
                (0..self.directories.len()).map(PaletteCandidate::Directory).collect()
            },
            PaletteMode::AiSessions => {
                (0..self.ai_sessions.len()).map(PaletteCandidate::AiSession).collect()
            },
        };
        let combined_search = |candidate: PaletteCandidate| -> &str {
            match candidate {
                PaletteCandidate::Item(index) => ITEMS[index].search,
                PaletteCandidate::Profile(index) => self.profiles[index].search(),
                PaletteCandidate::Ssh(index) => &self.ssh_hosts[index].search,
                // 目录模式已经由 DirectoryHistory 完成匹配和排序；这里
                // 不再二次模糊排序，以免破坏 frecency 的确定性。
                PaletteCandidate::Directory(_) => "",
                PaletteCandidate::AiSession(index) => &self.ai_sessions[index].search,
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
                PaletteCandidate::Profile(_)
                | PaletteCandidate::Ssh(_)
                | PaletteCandidate::Directory(_)
                | PaletteCandidate::AiSession(_) => usize::MAX,
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
        // 分组表头要求同组的行连续；上面两种排序（最近优先 / 模糊得分）都会把
        // 分类打散，所以这一步必须在**所有**排序之后。`sort_by_key` 是稳定
        // 排序，组内既有的先后原样保留。
        match self.mode {
            PaletteMode::AiSessions => self.group_ai_sessions_by_source(),
            PaletteMode::Commands => {
                let groups: Vec<_> = self.filtered.iter().map(|c| self.command_group(*c)).collect();
                let mut paired: Vec<_> = groups.into_iter().zip(self.filtered.drain(..)).collect();
                paired.sort_by_key(|(group, _)| *group);
                self.filtered = paired.into_iter().map(|(_, candidate)| candidate).collect();
            },
            _ => {},
        }
        self.selected = None; // Reset selection on refilter
        self.scroll_offset = 0;
        self.scrollbar_drag = None;
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
        let start = self.scroll_start(max_rows);
        // No stored selection yet: the first visible row is the default target.
        // `selected` stays None until navigation so Up from a fresh palette
        // still wraps to the last row, matching the existing keyboard model.
        let Some(selected) = self.selected else {
            let rows = self
                .filtered
                .iter()
                .skip(start)
                .take(max_rows)
                .map(|&row| self.row_for(row))
                .collect();
            let selected = (self.mode == PaletteMode::Commands).then_some(0);
            return (rows, selected);
        };
        let rows: Vec<_> =
            self.filtered.iter().skip(start).take(max_rows).map(|&row| self.row_for(row)).collect();
        let selected_row =
            (selected >= start && selected < start + rows.len()).then_some(selected - start);
        (rows, selected_row)
    }

    fn row_for(&self, candidate: PaletteCandidate) -> PaletteRow {
        match candidate {
            PaletteCandidate::Item(index) => PaletteRow {
                icon: String::new(),
                color_id: String::new(),
                label: localized_item_label(&ITEMS[index], self.language).to_owned(),
                hint: ITEMS[index].hint.to_string(),
                is_default: false,
                chip: String::new(),
                // 开关类命令带当前状态：勾上的是「已经开着」，不是「点了会
                // 开」。列表里同时有开关和一次性动作时，这是唯一能把两者分开
                // 的信号。
                checked: match ITEMS[index].action {
                    PaletteAction::ToggleSidebar => Some(self.context.sidebar),
                    PaletteAction::TogglePanelResize => Some(self.context.panel_resize),
                    _ => None,
                },
            },
            PaletteCandidate::Profile(index) => PaletteRow {
                icon: self.profiles[index].icon().to_string(),
                color_id: self.profiles[index].color_id().to_string(),
                label: self.profiles[index].label().to_string(),
                hint: self.profiles[index].hint().to_string(),
                is_default: self.profile_is_default(index),
                chip: String::new(),
                checked: None,
            },
            PaletteCandidate::Ssh(index) => PaletteRow {
                // Same outline terminal mark for every SSH destination; the
                // host identity belongs in the label/address, not in a random
                // per-row glyph color.
                icon: "\u{f489}".to_owned(),
                color_id: String::new(),
                label: self.ssh_hosts[index].label.clone(),
                hint: self.ssh_hosts[index].hint.clone(),
                is_default: false,
                chip: String::new(),
                checked: None,
            },
            PaletteCandidate::Directory(index) => PaletteRow {
                icon: "\u{f07b}".to_owned(),
                color_id: String::new(),
                label: self.directories[index].label.clone(),
                hint: self.directories[index].hint.clone(),
                is_default: false,
                chip: String::new(),
                checked: None,
            },
            PaletteCandidate::AiSession(index) => {
                PaletteRow {
                    // 双星（mdi-creation）：AI 会话的**通用**标，不按 claude /
                    // codex 分。来源交给分组承担，行首再放品牌标是同一条信息
                    // 说两遍；而且品牌标每行长得都不一样，混合列表里就没有一个
                    // 稳定的「这是会话」的记号可扫。
                    //
                    // 必须是双星不能是单颗四角星——单星是 Gemini 的标识，拿它
                    // 当通用 AI 标会被读成某一家的品牌入口（用户 08-02 裁定）。
                    //
                    // 走字形而不是纹理，顺带解决主题自适应：字形用 skin 的墨色
                    // 画，深浅主题自动跟随；纹理要为每个主题各备一版并重采样。
                    icon: AI_SESSION_GLYPH.to_owned(),
                    color_id: String::new(),
                    label: self.ai_sessions[index].label.clone(),
                    hint: self.ai_sessions[index].hint.clone(),
                    is_default: false,
                    // 右缘是**类型**标，不是来源标。
                    chip: self.language.pick("会话", "Session").to_owned(),
                    checked: None,
                }
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
        CopyCwd => "Copy path",
        RevealCwd => "Reveal in file manager",
        ToggleSidebar => "Show tab sidebar",
        TogglePanelResize => "Drag to resize panels",
        OpenDirectoryPicker => "New terminal in a frequent directory...",
        OpenAiSessionPicker => "Open quickly...",
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
        LaunchProfile(_) | LaunchShell(_) | LaunchSsh(_) | SetDefaultShell(_)
        | SetDefaultProfile(_) | NewAtDirectory(_) | ResumeAiSession(_) => item.label,
    }
}

/// 命令的分类。分组表头按它分，组的先后也按它排（`ordinal`）。
///
/// 分组的前提是同组的行**连续**，而空查询要把最近用过的命令顶上来、有查询
/// 要按模糊得分排——两种排序都会把分类打散。所以 refilter 末尾再按 ordinal
/// 做一次**稳定**排序：组的顺序固定下来，组内仍然是「最近优先 / 得分优先」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CommandGroup {
    /// 作用在聚焦 pane 工作目录上的动作。排在最前：面板打开时用户十有八九
    /// 是要对「手头这个目录」做点什么。
    Cwd,
    Tabs,
    Jump,
    View,
    Workspace,
    Appearance,
    Settings,
    /// 动态的 shell / profile 行，永远排在静态命令之后。
    Shells,
}

impl CommandGroup {
    fn of(action: &PaletteAction) -> Self {
        use PaletteAction::*;
        match action {
            NewTab | CopyCwd | RevealCwd => Self::Cwd,
            OpenDirectoryPicker | CloseTab | NextTab | PrevTab | NewWindow | SplitRight
            | SplitDown => Self::Tabs,
            OpenAiSessionPicker => Self::Jump,
            ToggleFilesPanel | ToggleGitPanel | ToggleSidebar | TogglePanelResize => Self::View,
            ExportWorkspace | ImportWorkspace | SyncPush | SyncPull => Self::Workspace,
            PickBackgroundImage | CycleBackground | ResetAppearance | SelectTheme(_) => {
                Self::Appearance
            },
            _ => Self::Settings,
        }
    }

    fn label(self, language: super::UiLanguage) -> String {
        match self {
            Self::Cwd => language.pick("工作目录", "WORKING DIRECTORY"),
            Self::Tabs => language.pick("标签页", "TABS"),
            Self::Jump => language.pick("跳转", "JUMP"),
            Self::View => language.pick("视图", "VIEW"),
            Self::Workspace => language.pick("工作区", "WORKSPACE"),
            Self::Appearance => language.pick("外观", "APPEARANCE"),
            Self::Settings => language.pick("设置", "SETTINGS"),
            Self::Shells => language.pick("SHELL 与 PROFILE", "SHELLS & PROFILES"),
        }
        .to_owned()
    }
}

/// AI 会话分组表头的文案。用产品名而不是 `AiSessionSource::label()` 的小写
/// 短标（`claude` / `codex`）：表头是给人读的标题，不是数据里的枚举值。
fn source_group_label(source: crate::ai_sessions::AiSessionSource) -> String {
    match source {
        crate::ai_sessions::AiSessionSource::Claude => "CLAUDE CODE 会话".to_owned(),
        crate::ai_sessions::AiSessionSource::Codex => "CODEX 会话".to_owned(),
    }
}

/// AI 会话行的行首字形：mdi-creation，一大一小两颗四角星。见 `row_for`
/// 的 `AiSession` 分支说明为什么是「双星 + 通用 + 字形」这三条。
const AI_SESSION_GLYPH: &str = "\u{f0674}";

/// One rendered palette row. `icon` is the Nerd Font fallback glyph (empty for
/// built-in action rows); `color_id` names a full-color brand PNG when the row
/// is a detected shell, or an `ai:*` id for AI-session brand logos (empty
/// otherwise, so the glyph shows instead).
pub struct PaletteRow {
    pub icon: String,
    pub color_id: String,
    pub label: String,
    pub hint: String,
    pub is_default: bool,
    /// 右缘小药丸的文字（AI 会话行 = 来源 `claude` / `codex`）；空 = 无。
    pub chip: String,
    /// 开关类命令的当前状态：`Some(true)` 画 ✓。`None` = 这行不是开关（一次
    /// 性动作），左列留空但**仍然占位**——同一组里一行缩进一行不缩进，标签
    /// 就成了锯齿。
    pub checked: Option<bool>,
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

/// Scrollbar geometry shared by rendering and pointer input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaletteScrollbar {
    /// Visual track `(x, y, w, h)`.
    pub track: (f32, f32, f32, f32),
    /// Visual thumb `(x, y, w, h)`.
    pub thumb: (f32, f32, f32, f32),
    /// Widened pointer target around the thin visual track.
    hit: (f32, f32, f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LauncherChip {
    pub filter: LauncherFilter,
    pub label: &'static str,
    pub count: usize,
    pub rect: (f32, f32, f32, f32),
    pub selected: bool,
}

impl LauncherChip {
    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        let (cx, cy, cw, ch) = self.rect;
        x >= cx && x < cx + cw && y >= cy && y < cy + ch
    }
}

/// 与 `launcher-unified.html` 的 `.chip` 保持同一套水平节奏：左右 11px，
/// 标签与计数之间 6px。宽度必须基于 UI 字体的真实 cell，而不是经验常数；
/// 否则 CJK 双列标签或两位数计数会先于圆角底溢出。
const LAUNCHER_CHIP_PAD_X: f32 = 11.0;
const LAUNCHER_CHIP_TEXT_GAP: f32 = 6.0;

fn launcher_chip_width(label: &str, count: usize, cell_w: f32, scale: f32) -> f32 {
    let count = count.to_string();
    let text_w = (text_width_cols(label) + text_width_cols(&count)) as f32 * cell_w;
    text_w + (LAUNCHER_CHIP_PAD_X * 2.0 + LAUNCHER_CHIP_TEXT_GAP) * scale
}

fn launcher_chip_text_x(chip: &LauncherChip, count: &str, cell_w: f32, scale: f32) -> (f32, f32) {
    let label_x = chip.rect.0 + LAUNCHER_CHIP_PAD_X * scale;
    let count_w = text_width_cols(count) as f32 * cell_w;
    let count_x = chip.rect.0 + chip.rect.2 - LAUNCHER_CHIP_PAD_X * scale - count_w;
    (label_x, count_x)
}

impl PaletteScrollbar {
    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        let (hx, hy, hw, hh) = self.hit;
        x >= hx && x < hx + hw && y >= hy && y < hy + hh
    }
}

/// Popup layout rectangles, all in physical pixels for the given `scale`.
pub struct PaletteLayout {
    /// Outer panel `(x, y, w, h)`.
    pub panel: (f32, f32, f32, f32),
    /// Query input box `(x, y, w, h)`.
    pub input: (f32, f32, f32, f32),
    /// Launcher-only group controls and their containing strip.
    pub chips: Vec<LauncherChip>,
    pub chip_band: Option<(f32, f32, f32, f32)>,
    /// Height of one standard result row.
    pub row_h: f32,
    /// Top Y of the first result row (or of its section header).
    pub list_y: f32,
    /// Scrollable list bounds `(x, y, w, h)`. Launcher rows use the HTML
    /// prototype's outer 6px list inset; other modes retain their current
    /// alignment with the query field.
    pub list: (f32, f32, f32, f32),
    /// Maximum rows drawn before the list scrolls.
    pub max_rows: usize,
    /// Keyboard-hint footer for picker modes; absent in the full command list.
    pub footer: Option<(f32, f32, f32, f32)>,
    /// Per-visible-row rects `(y, h)`; x/w span the panel's inner width. The
    /// picker's card geometry is non-uniform (hero card, section gaps), so
    /// rendering AND hit-testing must both read row rects from here instead
    /// of dividing by `row_h`.
    pub rows: Vec<(f32, f32)>,
    /// 分组表头：`(y, 标签, 右缘上下文)`。标签左对齐、右边拉一条淡色发丝线
    /// 到面板右缘；上下文非空时贴在线的右端（如「工作目录」那组挂当前 cwd
    /// ——这组动作作用在谁身上，写在标题上而不是每行重复）。
    ///
    /// 通用化之前这里是一对 `Option<f32>`，只够 hero 版式的「推荐 / 所有
    /// 选项」两条用；现在 hero 也走这个 vec，不再有第二套机制。
    pub groups: Vec<(f32, String, String)>,
    /// Present only when the filtered result set exceeds `max_rows`.
    pub scrollbar: Option<PaletteScrollbar>,
}

impl PaletteLayout {
    /// Visible-row index under a point, honoring the non-uniform card
    /// geometry. Points on section captions or card gaps return `None`.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (lx, _, lw, _) = self.list;
        if x < lx || x >= lx + lw {
            return None;
        }
        self.rows.iter().position(|&(ry, rh)| y >= ry && y < ry + rh)
    }

    pub fn chip_at(&self, x: f32, y: f32) -> Option<LauncherFilter> {
        self.chips.iter().find(|chip| chip.hit_test(x, y)).map(|chip| chip.filter)
    }
}

/// Compute the centered popup layout for a window of `win_w` × `win_h`. The
/// command list and AI-session picker keep a fixed panel height (sized for
/// `max_rows`) so they don't jump as the match count changes while typing;
/// smaller one-shot pickers shrink to their content. Every palette mode uses
/// the same search-input geometry, keeping rendering, hover and click
/// hit-testing on one contract.
pub fn palette_layout(
    model: &CommandPalette,
    win_w: f32,
    win_h: f32,
    scale: f32,
    cell_w: f32,
) -> PaletteLayout {
    palette_layout_with_workspace_bounds(model, win_w, win_h, scale, cell_w, None)
}

pub(crate) fn palette_layout_with_workspace_bounds(
    model: &CommandPalette,
    win_w: f32,
    win_h: f32,
    scale: f32,
    cell_w: f32,
    workspace_bounds: Option<(f32, f32)>,
) -> PaletteLayout {
    let s = |v: f32| v * scale;
    let cell_w = cell_w.max(s(1.0));
    let margin = s(8.0);
    let pad = s(12.0);
    let cards = model.is_picker();
    let launcher = model.is_launcher();
    let row_h = s(if launcher { 42.0 } else { super::ui::tokens::control::COMPACT_ROW });
    // HTML launcher 的搜索行比结果行矮，空间来自 pal-in 的上下留白；其他模式
    // 继续保持输入框与结果行等高，避免改变既有命令面板。
    let input_h = if launcher { s(34.0) } else { row_h };
    let footer_h = if cards { s(36.0) } else { 0.0 };
    let header_h = if launcher { s(LAUNCHER_GROUP_HEADER_H) } else { s(24.0) };
    let gap = s(6.0);
    let hero_h = s(58.0);
    let chip_h = s(24.0);
    let chip_band_h = if launcher { chip_h + s(10.0) } else { 0.0 };
    let launcher_top_pad = s(14.0);
    let launcher_input_gap = s(6.0);
    // HTML launcher geometry: the list has 8px horizontal inset, 6px top
    // breathing room, and 8px bottom breathing room. Keeping these axes
    // separate prevents the icon tile from reading as glued to the panel.
    let launcher_list_pad_x = s(8.0);
    let launcher_list_pad_top = s(6.0);
    let launcher_list_pad_bottom = s(8.0);
    // 固定预留推荐 / Shell / SSH 三条标题，筛选或搜索不会改变面板高度。
    let launcher_group_reserve = header_h * 3.0;
    let max_rows = if launcher {
        let fixed = launcher_top_pad
            + input_h
            + launcher_input_gap
            + chip_band_h
            + launcher_list_pad_top
            + launcher_list_pad_bottom
            + launcher_group_reserve
            + footer_h;
        (((win_h - margin * 2.0 - fixed).max(row_h) / row_h).floor() as usize).clamp(1, 5)
    } else if model.mode == PaletteMode::AiSessions {
        // Session recovery is a quick chooser, not a history browser. Six
        // rows keep the pane scan-friendly while the existing scrollbar still
        // exposes older sessions.
        6usize
    } else if cards {
        10usize
    } else {
        8usize
    };
    let visible = model.filtered.len().min(max_rows);
    let hero = model.hero_row() && visible > 0;

    // List geometry relative to the list's top edge. Pickers lay cards out
    // with section captions and gaps (图4); the command list stays a dense
    // uniform grid.
    let mut rel_rows: Vec<(f32, f32)> = Vec::with_capacity(visible);
    let mut rel_groups: Vec<(f32, String, String)> = Vec::new();
    let mut y = 0.0f32;
    if launcher {
        // Launcher 与 HTML 一样同时保留 chips 和类别标题。标题本身承担分隔，
        // 行之间不再额外插缝，因此推荐项与普通项始终等高。
        let labels = model.group_labels(max_rows).unwrap_or_default();
        for group in &labels {
            if let Some((label, context)) = group {
                rel_groups.push((y, label.clone(), context.clone()));
                y += header_h;
            }
            rel_rows.push((y, row_h));
            y += row_h;
        }
        y = y.max(launcher_group_reserve + max_rows as f32 * row_h);
    } else if cards {
        let slots = visible.max(1);
        let mut rest = slots;
        if hero {
            rel_groups.push((
                y,
                model.language.pick("推荐", "Recommended").to_owned(),
                String::new(),
            ));
            y += header_h;
            rel_rows.push((y, hero_h));
            y += hero_h + s(10.0);
            rest = slots - 1;
            if rest > 0 {
                rel_groups.push((
                    y,
                    model.language.pick("所有选项", "All options").to_owned(),
                    String::new(),
                ));
                y += header_h;
            }
        } else if let Some(labels) = model.group_labels(max_rows) {
            // 分组版式：每遇到新的一组就插一条表头，然后铺该组的行。
            let count = labels.len();
            for (index, group) in labels.iter().enumerate() {
                if let Some((label, context)) = group {
                    rel_groups.push((y, label.clone(), context.clone()));
                    y += header_h;
                }
                rel_rows.push((y, row_h));
                y += row_h;
                if index + 1 < count {
                    y += gap;
                }
            }
            y += s(4.0);
            rest = 0;
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
        if rest > 0 {
            y += s(4.0);
        }
    } else if let Some(labels) = model.group_labels(max_rows) {
        // 命令列表：均匀行距，但每组前面插一条表头。组与组之间再留半行呼吸，
        // 首组紧贴列表顶——顶上那条线不该和输入框贴出一道双边。
        for (index, group) in labels.iter().enumerate() {
            if let Some((label, context)) = group {
                if index > 0 {
                    y += s(6.0);
                }
                rel_groups.push((y, label.clone(), context.clone()));
                y += header_h;
            }
            rel_rows.push((y, row_h));
            y += row_h;
        }
        // 行少时也把面板撑到固定高度：否则每敲一个字面板都在长高缩矮。
        y = y.max(max_rows as f32 * row_h);
    } else {
        for _ in 0..max_rows {
            if rel_rows.len() < visible {
                rel_rows.push((y, row_h));
            }
            y += row_h;
        }
    }

    if model.mode == PaletteMode::AiSessions {
        // AI 会话只有 Claude/Codex 两组。为两条组标题和完整十行预留稳定高度，
        // 搜索过滤时只减少行，不移动面板边界和底栏，避免输入过程中整块 UI 跳动。
        let group_reserve = header_h * 2.0;
        let row_gaps = max_rows.saturating_sub(1) as f32 * gap;
        y = y.max(group_reserve + max_rows as f32 * row_h + row_gaps + s(4.0));
    }

    // 三个快捷面板属于同一个 UI 族：Shell launcher、命令面板和 AI 会话
    // 跳转都以终端工作区为水平基准。此前只有 launcher 使用工作区边界，另外
    // 两个仍固定请求 960 logical px，在 150% DPI 下会比 launcher 宽 216px。
    let desired_pw = workspace_bounds
        .map_or(s(960.0), |(left, right)| (right - left).max(0.0))
        .min(win_w - 2.0 * margin);
    let ph = if launcher {
        launcher_top_pad
            + input_h
            + launcher_input_gap
            + chip_band_h
            + launcher_list_pad_top
            + y
            + launcher_list_pad_bottom
            + footer_h
    } else {
        pad + input_h + s(8.0) + chip_band_h + y + if cards { footer_h } else { pad }
    };
    let top = launcher.then_some(96.0);
    let pane = workspace_bounds.map_or_else(
        || widgets::pane_geometry(win_w, win_h, scale, desired_pw, ph, 8.0, 12.0, top),
        |bounds| {
            widgets::pane_geometry_in_horizontal_bounds(
                win_w, win_h, scale, desired_pw, ph, 8.0, 12.0, top, bounds,
            )
        },
    );
    let (px, py, pw, ph) = pane.panel;

    let input = if launcher {
        (px + s(16.0), py + launcher_top_pad, pw - s(32.0), input_h)
    } else {
        (px + pad, py + pad, pw - 2.0 * pad, input_h)
    };
    let chip_band = launcher.then_some((
        px + s(16.0),
        input.1 + input_h + launcher_input_gap,
        pw - s(32.0),
        chip_band_h,
    ));
    let mut chips = Vec::new();
    if let Some((band_x, band_y, _, band_h)) = chip_band {
        // 胶囊属于整条分组背景带，必须在带内居中；直接拿 band_y 会把全部
        // 10px 留白压到底部，文字虽在胶囊内居中，整组仍会显得向上漂。
        let chip_y = widgets::centered_y(band_y, band_h, chip_h);
        let mut chip_x = band_x;
        for (filter, count) in model.launcher_chip_counts() {
            let label = filter.label(model.language);
            let chip_w = launcher_chip_width(label, count, cell_w, scale);
            chips.push(LauncherChip {
                filter,
                label,
                count,
                rect: (chip_x, chip_y, chip_w, chip_h),
                selected: model.launcher_filter() == filter,
            });
            chip_x += chip_w + s(6.0);
        }
    }
    let list_y = if launcher {
        chip_band.map_or(input.1 + input_h, |(_, by, _, bh)| by + bh) + launcher_list_pad_top
    } else {
        py + pad + input_h + s(8.0) + chip_band_h
    };
    let (list_x, list_w) = if launcher {
        (px + launcher_list_pad_x, pw - launcher_list_pad_x * 2.0)
    } else {
        (input.0, input.2)
    };
    let rows = rel_rows.into_iter().map(|(ry, rh)| (list_y + ry, rh)).collect();
    let groups = rel_groups.into_iter().map(|(gy, label, ctx)| (list_y + gy, label, ctx)).collect();

    let footer = cards.then_some((px, py + ph - footer_h, pw, footer_h));
    let scrollbar = (model.filtered.len() > max_rows && max_rows > 0).then(|| {
        let track_w = s(3.0);
        let track_x = px + pw - s(8.0);
        let track_h = y.max(row_h);
        let visible_fraction = max_rows as f32 / model.filtered.len() as f32;
        let thumb_h = (track_h * visible_fraction).max(s(28.0)).min(track_h);
        let max_scroll = model.max_scroll(max_rows);
        let fraction = if max_scroll == 0 {
            0.0
        } else {
            model.scroll_start(max_rows) as f32 / max_scroll as f32
        };
        let thumb_y = list_y + (track_h - thumb_h) * fraction;
        PaletteScrollbar {
            track: (track_x, list_y, track_w, track_h),
            thumb: (track_x, thumb_y, track_w, thumb_h),
            // 细滚动条保持克制，但命中区必须足够宽，避免高 DPI 下难以抓取。
            hit: (track_x - s(6.0), list_y, track_w + s(12.0), track_h),
        }
    });

    PaletteLayout {
        panel: (px, py, pw, ph),
        input,
        chips,
        chip_band,
        row_h,
        list_y,
        list: (list_x, list_y, list_w, y),
        max_rows,
        footer,
        rows,
        groups,
        scrollbar,
    }
}

// ---- rendering (the parent `display::mod` hands in the model + renderer;
// this module owns the palette's pixels — same split as `side_panel.rs`) ----

use super::ui::overlay_list::{self, RowState};
use super::ui::surface;
use super::ui::widgets::{self, ChipState};
use crate::renderer::ui::UiQuad;
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

/// 输入框与列表行共用的左内边距。列表文字必须与查询文字**左缘对齐**，
/// 否则输入框读起来像悬在列表外面的另一个控件。
const INPUT_PAD_X: f32 = 14.0;

const PICKER_ICON_BOX: f32 = 28.0;
const PICKER_ICON_GAP: f32 = 11.0;
const PICKER_ICON_INDENT: f32 = PICKER_ICON_BOX + PICKER_ICON_GAP;

/// Group captions are structure, not content. Keeping them below body size
/// preserves the HTML reference's quiet scan path through the rows.
const LAUNCHER_GROUP_TEXT_SCALE: f32 = 0.8;
const LAUNCHER_GROUP_HEADER_H: f32 = 28.0;

/// 搜索图标占的列数（图标一列 + 一列缝）。按 cell 列而不是固定 px 计量：
/// 固定 px 的缝隙在字号变化时会与字形脱节——大字号显挤、小字号显空。
const SEARCH_SLOT_COLS: f32 = 2.0;

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
    workspace_bounds: Option<(f32, f32)>,
) {
    if !model.is_open() {
        return;
    }
    let w = size.width();
    let h = size.height();
    let s = |v: f32| v * scale;
    let sk = theme.skin();
    let layout = palette_layout_with_workspace_bounds(
        model,
        w,
        h,
        scale,
        size.cell_width(),
        workspace_bounds,
    );
    let (ix, iy, iw, ih) = layout.input;
    let (list_x, _, list_w, _) = layout.list;
    let launcher = model.is_launcher();

    // The three-dot launcher owns the entire pointer/keyboard scope while it
    // is open. Its veil is therefore painted in this final overlay pass so
    // settings/chrome text cannot leak above it. Other command-palette modes
    // retain their lighter popover behavior.
    if launcher {
        quads.push(UiQuad::solid(0.0, 0.0, w, h, 0.0, sk.veil));
    }

    let panel_radius = if launcher {
        super::ui::tokens::radius::LAUNCHER
    } else {
        super::ui::tokens::radius::OVERLAY
    };
    surface::push_surface_with_radius(
        quads,
        layout.panel,
        (0.0, 0.0, w, h),
        0.0,
        scale,
        &sk,
        surface::Elevation::Popover,
        1.0,
        panel_radius,
    );

    // 原型的搜索行直接落在面板底上；其他 palette 模式仍保留内凹输入框。
    if !launcher {
        quads.push(UiQuad::solid(ix, iy, iw, ih, s(super::ui::tokens::radius::CONTROL), sk.input));
    }

    if let Some((_bx, by, _bw, bh)) = layout.chip_band {
        let (panel_x, _, panel_w, _) = layout.panel;
        // One quiet rule under the tag band, spanning the complete pane.
        quads.push(
            UiQuad::solid(panel_x, by + bh - 1.0, panel_w, 1.0, 0.0, sk.hairline).pixel_snapped(),
        );
        for chip in &layout.chips {
            let state = if chip.selected {
                ChipState::Selected
            } else if model.launcher_chip_hover == Some(chip.filter) {
                ChipState::Hover
            } else {
                ChipState::Quiet
            };
            widgets::push_chip(quads, chip.rect, scale, &sk, state);
        }
    }
    if model.query_all_selected() && !model.query.is_empty() {
        let cell_w = size.cell_width();
        let columns: usize = model.query.chars().map(|c| c.width().unwrap_or(0)).sum();
        let search_left = if launcher { ix } else { ix + s(INPUT_PAD_X) };
        overlay_list::push_selection_band(
            quads,
            search_left + cell_w * SEARCH_SLOT_COLS,
            layout.input,
            columns as f32,
            cell_w,
            scale,
            &sk,
        );
    }

    let cell_w = size.cell_width();
    let row_content_x = if launcher { list_x + s(10.0) } else { ix + s(INPUT_PAD_X) };
    let row_content_right = if launcher { list_x + list_w - s(10.0) } else { ix + iw - s(GUTTER) };
    // Group captions provide hierarchy through type and spacing. Do not add
    // separator rules below them; the single full-width tag rule above is the
    // only divider in the pane body.
    // 搜索图标槽与查询文字的起点。槽宽按 **cell 列**算而不是固定 px：
    // 字号一变，固定 px 的缝隙就会与字形脱节（大字号显挤、小字号显空）。
    let query_x = if launcher { ix } else { ix + s(INPUT_PAD_X) } + cell_w * SEARCH_SLOT_COLS;

    // 文本光标是细梁 quad 而不是 `▏` 字形（不占列宽，placeholder 与真实
    // 输入同 x）——理由与几何都在 `overlay_list::push_query_caret`。
    if !model.query_all_selected() {
        let columns: usize = model.query.chars().map(|c| c.width().unwrap_or(0)).sum();
        overlay_list::push_query_caret(
            quads,
            query_x + columns as f32 * cell_w,
            layout.input,
            size.cell_height(),
            scale,
            &sk,
        );
    }

    // Ctrl+K 键帽：仅 shell picker 空查询时展示，指认打开快捷键；
    // 输入开始后让位给查询文字。
    if model.mode == PaletteMode::Profiles && model.query.is_empty() {
        let combo = super::ui::keycap::layout_combo(
            "Ctrl+K",
            if launcher { ix + iw } else { ix + iw - s(GUTTER) },
            iy + ih / 2.0,
            cell_w,
            scale,
        );
        super::ui::keycap::push_combo(quads, &sk, &combo, scale);
    }

    if let Some(footer) = layout.footer {
        overlay_list::push_footer_band(quads, footer, &sk);
    }

    if let Some(scrollbar) = layout.scrollbar {
        let (tx, ty, tw, th) = scrollbar.thumb;
        let alpha = if model.scrollbar_dragging() { 0.72 } else { 0.48 };
        quads.push(UiQuad::solid(tx, ty, tw, th, tw * 0.5, sk.scrollbar_thumb.with_alpha(alpha)));
    }

    let (visible_rows, selected_row) = model.visible(layout.max_rows);
    let cards = model.is_picker();
    let hero = model.hero_row();
    if cards {
        // 列表行**不画卡片**（2026-07-29 用户裁定）：默认完全透明，露出面板
        // 底；只有 hover / 选中才上一层底色。
        //
        // 此前每行都是「hairline 圈 + 白底卡片」，五行排下来是五个带框的白
        // 矩形——那读作表单，不读作列表。边框在这里不传递任何信息（明度差
        // 已经把行分开了），删掉之后噪音立刻降一个量级。
        //
        // 选中态用中性的 hover_strong 而不是 accent_soft：强调色预算一屏
        // 只花一次，而这个面板里真正需要它的是"当前选中"这一处。
        // 推荐卡也不再染色——它已经被"推荐"分区标题、更大的尺寸、双行布局
        // 和右侧 ↵ chip 标记了四次身份，第五次是浪费。
        let corner = s(super::ui::tokens::radius::OVERLAY);
        for (row, &(ry, rh)) in layout.rows.iter().enumerate() {
            let is_hero = hero && row == 0;
            let row_rect = (list_x, ry, list_w, rh);
            let state = if selected_row == Some(row) {
                RowState::Selected
            } else if model.hover == Some(row) {
                RowState::Hover
            } else {
                RowState::Idle
            };
            overlay_list::push_row_state(quads, row_rect, corner, &sk, state);
            // Every launcher/picker row gets the same icon container. Brand
            // artwork and fallback glyphs may differ, but their visual weight
            // no longer depends on whether an asset happens to have its own
            // colored background.
            let icon_rect =
                overlay_list::icon_tile_rect(row_content_x, row_rect, s(PICKER_ICON_BOX));
            overlay_list::push_icon_tile(quads, icon_rect, scale, &sk);
            if is_hero {
                // 推荐卡右缘的 ↵ chip 与图标瓦片同一容器语言（迁移前圆角
                // 手写成 6，比瓦片的 ICON_TILE=7 少 1px——无信息的偏差，
                // 组件化时归一）。
                let chip = s(28.0);
                let chip_rect =
                    overlay_list::icon_tile_rect(row_content_right - chip, row_rect, chip);
                overlay_list::push_icon_tile(quads, chip_rect, scale, &sk);
            }
        }
    } else {
        // Hover / selected pills: the option-row recipe (2px breathing gaps,
        // CONTROL corner, accent beam on the keyboard selection) lives in
        // `overlay_list::push_option_row`. A row that is hovered AND selected
        // keeps painting both layers, same as before the extraction.
        if let Some(hover_row) = model.hover {
            if let Some(&(ry, rh)) = layout.rows.get(hover_row) {
                let rect = (ix + s(8.0), ry, iw - s(16.0), rh);
                overlay_list::push_option_row(quads, rect, scale, &sk, RowState::Hover, false);
            }
        }
        if let Some(row) = selected_row {
            if let Some(&(ry, rh)) = layout.rows.get(row) {
                let rect = (ix + s(8.0), ry, iw - s(16.0), rh);
                overlay_list::push_option_row(quads, rect, scale, &sk, RowState::Selected, true);
            }
        }
    }

    // The default badge is deliberately a hairline chip, not another bright
    // accent pill. It identifies the launch target without competing with the
    // selected-row affordance. The hero card 已经用"推荐"分区表达默认身份，
    // 不再叠加徽标。
    let text_x = row_content_x;
    let check_col = model.has_check_column();
    let badge = model.language.pick("默认", "Default");
    let badge_w =
        badge.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w + s(12.0);
    for (row, entry) in visible_rows.into_iter().enumerate() {
        let Some(&(ry, rh)) = layout.rows.get(row) else { continue };
        // 图8 规范：命令列表右侧的快捷键 hint 画成逐键 chip 底（键名在文
        // 字 pass）。挤不下就整组让位，不压进标签。
        if !cards && !entry.hint.is_empty() {
            let combo = super::ui::keycap::layout_combo(
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
                super::ui::keycap::push_combo(quads, &sk, &combo, scale);
            }
        }
        // AI 会话行右缘的来源 chip（"claude"/"codex"）与「默认」徽标同宗
        // ——身份标注，不与选中态抢强调色，皮肤统一在
        // `overlay_list::push_identity_chip`。文字在 text pass 按同一几何画。
        if !entry.chip.is_empty() {
            let rect = overlay_list::identity_chip_rect(
                ix + iw - s(GUTTER),
                (ix, ry, iw, rh),
                text_width_cols(&entry.chip) as f32,
                cell_w,
                scale,
            );
            overlay_list::push_identity_chip(quads, rect, scale, &sk);
        }
        // 开关类命令的勾选态。画成线段 quad 而不是字体里的 `✓` 字形：那个
        // 码位在不同 fallback 字体里宽窄不一，行与行之间会左右晃半格，而这
        // 一列的全部价值就在于**竖直对齐**。青色（`ok`）是「已经是这样」的
        // 语义色，与强调色分开——列表里同时有开关和一次性动作时，这是唯一
        // 能把两者分开的信号。
        if entry.checked == Some(true) {
            super::ui::icons::push_check(
                quads,
                text_x + s(CHECK_COL_W) / 2.0,
                ry + rh / 2.0,
                scale,
                sk.ok,
            );
        }
        if !entry.is_default || (hero && row == 0) {
            continue;
        }
        let label_x = if !entry.icon.is_empty() {
            text_x + s(PICKER_ICON_INDENT)
        } else if check_col {
            text_x + s(CHECK_COL_W)
        } else {
            text_x
        };
        let label_w =
            entry.label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w;
        overlay_list::push_identity_chip(
            quads,
            (label_x + label_w + s(8.0), ry + s(7.0), badge_w, rh - s(14.0)),
            scale,
            &sk,
        );
    }

    // 底栏**不画 chip 底**（2026-07-29 用户裁定）。
    //
    // chip 底在这套界面里的语义是「可点击」——命令行右侧的快捷键 chip、
    // Ctrl+K 徽标都对应一个能按下去的东西。底栏的 ↑↓/Enter/Esc 是纯键位
    // 说明，给它们戴上 chip 会把那个信号稀释成装饰，用户就不再能靠"有没有
    // chip 底"判断某处能不能点。
    //
    // 键位与说明的区分改由**墨色权重**承担（键名 ink_dim、说明 ink_faint，
    // 见文字 pass）。这也符合层级手段的成本排序：明度差比边框/填充贵，
    // 但效果更干净——底栏因此从三个灰块变回一行安静的文字。
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
    workspace_bounds: Option<(f32, f32)>,
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
    let layout = palette_layout_with_workspace_bounds(
        model,
        w,
        h,
        scale,
        size.cell_width(),
        workspace_bounds,
    );
    let (ix, iy, iw, ih) = layout.input;
    let (list_x, _, list_w, _) = layout.list;
    let launcher = model.is_launcher();

    // Inks from the theme skin: dark text on light panels, pale on dark.
    let sk = theme.skin();

    // 搜索区和列表各自遵循 HTML 的 16px 内容线；非 launcher 模式继续沿用
    // 输入框内 14px 的既有基准。
    let search_x = if launcher { ix } else { ix + s(INPUT_PAD_X) };
    let text_x = if launcher { list_x + s(10.0) } else { search_x };
    let text_right = if launcher { list_x + list_w - s(10.0) } else { ix + iw - s(GUTTER) };

    const ICON_SEARCH: &str = "\u{f0349}"; // mdi-magnify
    let text_y = widgets::centered_y(iy, ih, cell_h);
    r.draw_chrome_text(size, search_x, text_y, sk.ink_faint, ICON_SEARCH, gc);

    // placeholder 与真实查询共用这一个起点。光标是 quad pass 画的细梁，
    // 不占列宽，所以打下第一个字符时文字不会跳位。
    let query_x = search_x + cell_w * SEARCH_SLOT_COLS;
    let query = model.query();

    if query.is_empty() {
        let placeholder = match model.mode {
            PaletteMode::Commands => model
                .language
                .pick("搜索命令、Shell 或 Profile…", "Search commands, shells or profiles..."),
            PaletteMode::Profiles | PaletteMode::DefaultShell => {
                if model.mode == PaletteMode::Profiles {
                    model.language.pick("搜索 Shell 或 SSH 主机…", "Search shells or SSH hosts...")
                } else {
                    model.language.pick("搜索 Shell 或 Profile…", "Search shells or profiles...")
                }
            },
            PaletteMode::Directories => {
                model.language.pick("搜索常用目录…", "Search frequent directories...")
            },
            PaletteMode::AiSessions => {
                model.language.pick("搜索 AI 会话…", "Search AI sessions...")
            },
        };
        r.draw_chrome_text(size, query_x, text_y, sk.ink_faint, placeholder, gc);
    } else {
        r.draw_chrome_text(size, query_x, text_y, sk.ink_strong, query, gc);
    }

    for chip in &layout.chips {
        let (_, cy, _, ch) = chip.rect;
        let ty = cy + (ch - cell_h) * 0.5;
        let hovered = model.launcher_chip_hover == Some(chip.filter);
        let count = chip.count.to_string();
        let (label_x, count_x) = launcher_chip_text_x(chip, &count, cell_w, scale);
        let label_ink = if chip.selected {
            sk.accent
        } else if hovered {
            sk.ink_strong
        } else {
            sk.ink_dim
        };
        r.draw_chrome_text(size, label_x, ty, label_ink, chip.label, gc);
        r.draw_chrome_text(
            size,
            count_x,
            ty,
            if chip.selected { sk.accent } else { sk.ink_faint },
            &count,
            gc,
        );
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
                PaletteMode::AiSessions => model
                    .language
                    .pick("没有找到 AI 会话（claude / codex）", "No AI sessions found"),
                PaletteMode::Profiles => {
                    model.language.pick("当前分组没有匹配项", "No matches in this group")
                },
                PaletteMode::Commands | PaletteMode::DefaultShell => {
                    model.language.pick("无匹配命令", "No matching commands")
                },
            },
            gc,
        );
        if let Some((_, fy, _, fh)) = layout.footer {
            draw_footer_hints(r, gc, size, model, ix, iw, fy, fh, cell_w, cell_h, scale, &sk);
        }
        return icon_draws;
    }

    // 分组表头（图4 的推荐/所有选项，以及 AI 会话按来源分的组）。
    for (gy, label, ctx) in &layout.groups {
        if launcher {
            r.draw_ui_text(
                size,
                text_x,
                gy + s(3.0),
                LAUNCHER_GROUP_TEXT_SCALE,
                sk.ink_faint,
                nebula_terminal::term::cell::Flags::empty(),
                label,
                gc,
            );
        } else {
            r.draw_chrome_text(size, text_x, gy + s(2.0), sk.ink_dim, label, gc);
        }
        // 右缘上下文（如「工作目录」组挂当前 cwd）：右对齐贴到面板右缘，
        // 与横线让出的位置同一套基准（见 quad pass 的 `ctx_w`）。
        if !ctx.is_empty() {
            let ctx_w = text_width_cols(ctx) as f32 * cell_w;
            r.draw_chrome_text(size, text_right - ctx_w, gy + s(2.0), sk.ink_faint, ctx, gc);
        }
    }
    // Ctrl+K 键帽文字（chip 底在 quad pass，几何同源）。
    if model.mode == PaletteMode::Profiles && model.query.is_empty() {
        let combo = super::ui::keycap::layout_combo(
            "Ctrl+K",
            if launcher { ix + iw } else { ix + iw - s(GUTTER) },
            iy + ih / 2.0,
            cell_w,
            scale,
        );
        draw_combo_text(r, gc, size, &combo, cell_w, cell_h, &sk);
    }

    let cards = model.is_picker();
    let hero = model.hero_row();
    let check_col = model.has_check_column();
    let (rows, selected_row) = model.visible(layout.max_rows);
    let badge = model.language.pick("默认", "Default");
    let badge_w = |present: bool| -> f32 {
        if present { text_width_cols(badge) as f32 * cell_w + s(12.0) + s(10.0) } else { 0.0 }
    };

    for (row, entry) in rows.into_iter().enumerate() {
        let PaletteRow { icon, color_id, label, hint, is_default, chip, checked: _ } = entry;
        let Some(&(row_y, row_hh)) = layout.rows.get(row) else { break };
        let is_hero = hero && row == 0;
        // Hero 卡片双行：名称在上、完整路径在下（图4）；普通行单行居中。
        let ry = if is_hero { row_y + s(8.0) } else { widgets::centered_y(row_y, row_hh, cell_h) };
        let fg = if Some(row) == selected_row || is_hero {
            sk.ink_strong
        } else if launcher {
            sk.ink_dim
        } else {
            sk.ink
        };
        // Leading icon, then the label indented past it. Detected shells with a
        // brand asset stage a full-color textured quad (drawn later); the rest
        // fall back to the Nerd Font glyph. Built-in action rows carry an empty
        // icon and keep the original left edge.
        let has_color =
            !color_id.is_empty() && crate::shell_detect::color_icon_png(&color_id).is_some();
        let indent = if cards { s(PICKER_ICON_INDENT) } else { s(26.0) };
        let label_x = if has_color {
            let icon_s = if cards { s(PICKER_ICON_BOX) } else { (cell_h * 0.92).round() };
            let icon_y = widgets::centered_y(row_y, row_hh, icon_s).round();
            icon_draws.push((color_id, (text_x, icon_y, icon_s, icon_s)));
            text_x + indent
        } else if check_col {
            // 命令面板的左列**永远占位**（勾选态列，✓ 在 quad pass 画）：
            // 开关命令勾上，一次性动作留空。留空而不是收窄——同一组里一行
            // 缩进一行不缩进，标签就排成锯齿，而分组表头刚把这些行归到了
            // 一起。
            text_x + s(CHECK_COL_W)
        } else if icon.is_empty() {
            text_x
        } else {
            let icon_y = widgets::centered_y(row_y, row_hh, cell_h);
            let icon_x = if cards { text_x + (s(PICKER_ICON_BOX) - cell_w) * 0.5 } else { text_x };
            let icon_ink = if launcher { sk.ink_dim } else { sk.icon };
            r.draw_chrome_text(size, icon_x, icon_y, icon_ink, &icon, gc);
            text_x + indent
        };
        // 右缘边界：来源 chip 占掉的宽度从这一行所有右侧内容里扣除；chip
        // 文字与 quad pass 的药丸底同一几何。
        let right_limit = if chip.is_empty() {
            text_right
        } else {
            let chip_w = text_width_cols(&chip) as f32 * cell_w + s(12.0);
            let cx = text_right - chip_w;
            r.draw_chrome_text(size, cx + s(6.0), ry, sk.ink_dim, &chip, gc);
            cx - s(CHIP_GAP)
        };
        // 标签按可用宽截断（尾部省略号）。60 字符的会话标题此前不截断，
        // 直接横穿路径列画出面板（用户 08-02 截图）；任何一行的标签都不许
        // 画进右侧信息区。
        // HTML reference contract: details consume the remaining flex space
        // and align to the shared right edge. Reserve a quiet right-hand zone
        // before fitting the label so the two columns can never overlap.
        let path_floor = list_x + list_w * 0.54;
        let badge_reserve = badge_w(is_default && !is_hero);
        let label_limit = if cards && !hint.is_empty() && !is_hero {
            right_limit.min(path_floor - s(HINT_GAP) - badge_reserve)
        } else {
            right_limit
        };
        let label_budget = ((label_limit - label_x) / cell_w).floor().max(0.0) as usize;
        let label = fit_head(&label, label_budget);
        r.draw_chrome_text(size, label_x, ry, fg, &label, gc);
        let badge_span = if is_default && !is_hero {
            r.draw_chrome_text(
                size,
                label_x + text_width_cols(&label) as f32 * cell_w + s(10.0),
                ry,
                sk.ink_dim,
                badge,
                gc,
            );
            badge_w(true)
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
            let chip_text_y = widgets::centered_y(row_y, row_hh, cell_h);
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
                let combo = super::ui::keycap::layout_combo(
                    &hint,
                    ix + iw - s(GUTTER),
                    row_y + row_hh / 2.0,
                    cell_w,
                    scale,
                );
                let label_end = ix
                    + s(GUTTER)
                    + label.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>() as f32 * cell_w;
                if combo.bounds.0 > label_end + s(HINT_GAP) {
                    draw_combo_text(r, gc, size, &combo, cell_w, cell_h, &sk);
                }
            } else if cards {
                // Details end on one shared right edge. Overflow keeps the
                // root/program prefix and ellipsizes at the tail, matching the
                // launcher HTML rather than drifting into the label column.
                let label_end = label_x + text_width_cols(&label) as f32 * cell_w + badge_span;
                let detail_left = path_floor.max(label_end + s(HINT_GAP));
                let budget = ((right_limit - detail_left) / cell_w).floor();
                if budget >= 3.0 {
                    let (shown, hint_x) =
                        fit_right_detail(&hint, budget as usize, right_limit, cell_w);
                    let hint_ink =
                        if Some(row) == selected_row { sk.ink_dim } else { sk.ink_faint };
                    r.draw_chrome_text(size, hint_x, ry, hint_ink, &shown, gc);
                }
            }
        }
    }
    if let Some((_, fy, _, fh)) = layout.footer {
        draw_footer_hints(r, gc, size, model, ix, iw, fy, fh, cell_w, cell_h, scale, &sk);
    }
    icon_draws
}

/// 底栏的三条键位提示。
///
/// 键名与释义分成两级墨（`ink_dim` / `ink_faint`）而不是给键名套 chip 底：
/// chip 底在这套界面里的语义是「可点击」，底栏这三条按不下去。用墨色权重
/// 区分同样能让键名跳出来，而且不占额外面积、不给画面增加三个灰块。
///
/// 三条的锚点分别是左缘、居中、右缘——与列表列同一套 `ix`/`iw`/`GUTTER`，
/// 所以右缘和上面的快捷键 chip 连成一条竖线。
#[allow(clippy::too_many_arguments)]
fn draw_footer_hints(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    model: &CommandPalette,
    ix: f32,
    iw: f32,
    fy: f32,
    fh: f32,
    cell_w: f32,
    cell_h: f32,
    scale: f32,
    sk: &super::ui::theme::Skin,
) {
    let y = fy + (fh - cell_h) * 0.5;
    if model.is_launcher() {
        let hints = [
            ("↑ ↓", model.language.pick("选择", "Select")),
            ("Enter", model.language.pick("打开", "Open")),
            ("Tab", model.language.pick("切换分组", "Switch group")),
            ("Esc", model.language.pick("关闭", "Close")),
        ];
        // Launcher 的 `ix/iw` 已经是面板 16px 内容线，不再重复内缩。
        let mut x = ix;
        for (key, label) in hints {
            r.draw_chrome_text(size, x, y, sk.ink_dim, key, gc);
            let label_x = x + (text_width_cols(key) + 1) as f32 * cell_w;
            r.draw_chrome_text(size, label_x, y, sk.ink_faint, label, gc);
            x = label_x + text_width_cols(label) as f32 * cell_w + scale * 16.0;
        }
        let count = model.language.pick("项", "items");
        let count = format!("{} {count}", model.filtered.len());
        let count_x = ix + iw - text_width_cols(&count) as f32 * cell_w;
        if count_x > x {
            r.draw_chrome_text(size, count_x, y, sk.ink_faint, &count, gc);
        }
        return;
    }
    // (键, 释义)。键名保持 ASCII/箭头，释义随语言切换。
    let hints = [
        ("↑ ↓", model.language.pick("选择", "Select")),
        ("Enter", model.language.pick("打开", "Open")),
        ("Esc", model.language.pick("关闭", "Close")),
    ];
    // 释义与键名之间空一整列：等宽网格上的半格会让两段文字看着没对齐。
    let span = |key: &str, label: &str| {
        (text_width_cols(key) + 1 + text_width_cols(label)) as f32 * cell_w
    };
    let xs = [
        ix,
        ix + (iw - span(hints[1].0, hints[1].1)) * 0.5,
        ix + iw - scale * GUTTER - span(hints[2].0, hints[2].1),
    ];
    for (&x, (key, label)) in xs.iter().zip(hints) {
        r.draw_chrome_text(size, x, y, sk.ink_dim, key, gc);
        let label_x = x + (text_width_cols(key) + 1) as f32 * cell_w;
        r.draw_chrome_text(size, label_x, y, sk.ink_faint, label, gc);
    }
}

/// 键帽文字：chip 内水平居中整串键位；几何来自 `keycap::layout_combo`，
/// 与 quad pass 同源。
fn draw_combo_text(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    combo: &super::ui::keycap::ComboChips,
    cell_w: f32,
    cell_h: f32,
    sk: &super::ui::theme::Skin,
) {
    let (_, key_y, _, key_h) = combo.bounds;
    let ty = widgets::centered_y(key_y, key_h, cell_h);
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
}

fn text_width_cols(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
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

fn fit_right_detail(value: &str, budget: usize, right: f32, cell_w: f32) -> (String, f32) {
    let shown = fit_head(value, budget);
    let x = right - text_width_cols(&shown) as f32 * cell_w;
    (shown, x)
}

/// 路径的**头部**省略：`D:\a\b\…\nebula`。与 [`fit_head`] 相反，因为这两处
/// 要保住的信息在两头——picker 的路径列扫的是共同前缀（哪个盘、哪个用户），
/// 分组表头挂的那个 cwd 扫的是尾巴（在哪个项目里）。
fn fit_tail(value: &str, budget: usize) -> String {
    let width = |ch: char| ch.width().unwrap_or(0);
    let total: usize = value.chars().map(width).sum();
    if total <= budget {
        return value.to_owned();
    }
    if budget <= 1 {
        return "…".to_owned();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0usize;
    for ch in value.chars().rev() {
        let w = width(ch);
        if used + w > budget - 1 {
            break;
        }
        tail.push(ch);
        used += w;
    }
    let mut out = String::from("…");
    out.extend(tail.into_iter().rev());
    out
}

/// `PaletteRow::color_id` 的 `ai:` 命名空间曾在这里解析成 claude / codex 的
/// 品牌纹理。2026-08-02 撤掉：AI 会话统一走 [`AI_SESSION_GLYPH`] 双星字形，
/// 来源由分组承担。Shell / Profile 行的彩色真标不受影响——那是识别不同的
/// **程序**，与「同一类事物的不同来源」不是一回事。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_all_in_declaration_order() {
        let palette = CommandPalette::new();
        // 暂缓露出的命令（`CommandPalette::parked`）不进列表，但仍留在
        // `ITEMS` 里——它的动作、搜索词和本地化文案都还在，放行时只需删掉
        // 那条判定。工作目录组的两条也在此列：新面板没有 cwd。
        let parked = ITEMS.iter().filter(|item| palette.parked(&item.action)).count();
        assert_eq!(palette.filtered.len(), ITEMS.len() - parked);
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
        palette.move_selection(-1, 5);
        assert_eq!(
            palette.selected,
            Some(palette.filtered.len() - 1),
            "up from top wraps to bottom"
        );
        palette.move_selection(1, 5);
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
            palette.move_selection(1, max);
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
        palette.set_default_shell_menu(std::slice::from_ref(&shell), &[], "pwsh");
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
            &[
                Profile {
                    name: "Windows PowerShell".into(),
                    command: "powershell.exe".into(),
                    args: vec![],
                    cwd: None,
                    shell_id: None,
                    terminal_profile_id: None,
                },
                Profile {
                    name: "Git Bash".into(),
                    command: "bash.exe".into(),
                    args: vec![],
                    cwd: None,
                    shell_id: None,
                    terminal_profile_id: None,
                },
            ],
            "powershell",
        );
        palette.open_profiles();

        palette.input_text("git");
        let (rows, _) = palette.visible(8);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Git Bash");
    }

    #[test]
    fn launcher_filters_cycle_all_ssh_shell_and_reset_selection() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[shell("PowerShell", "powershell", "powershell.exe")],
            &[],
            "powershell",
        );
        palette.set_ssh_hosts(&[("生产机".into(), "root@example.com".into())]);
        palette.open_profiles();

        assert_eq!(palette.launcher_filter(), LauncherFilter::All);
        assert_eq!(palette.visible(10).0.len(), 2);
        palette.move_selection(1, 10);
        assert!(palette.cycle_launcher_filter(1));
        assert_eq!(palette.launcher_filter(), LauncherFilter::Ssh);
        assert_eq!(palette.visible(10).0[0].label, "生产机");
        assert_eq!(palette.selected, None, "切组后不能保留旧分组的行索引");
        assert!(palette.cycle_launcher_filter(1));
        assert_eq!(palette.launcher_filter(), LauncherFilter::Shell);
        assert_eq!(palette.visible(10).0[0].label, "PowerShell");
        assert!(palette.cycle_launcher_filter(1));
        assert_eq!(palette.launcher_filter(), LauncherFilter::All);
        assert!(palette.cycle_launcher_filter(-1));
        assert_eq!(palette.launcher_filter(), LauncherFilter::Shell);
    }

    #[test]
    fn launcher_ssh_confirmation_keeps_the_exact_destination() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&[], &[], "powershell");
        palette.set_ssh_hosts(&[("生产机".into(), "ssh://root@example.com:2222".into())]);
        palette.open_profiles();
        assert!(palette.set_launcher_filter(LauncherFilter::Ssh));

        let (rows, _) = palette.visible(10);
        assert_eq!(rows[0].label, "生产机");
        assert_eq!(rows[0].hint, "ssh://root@example.com:2222");
        assert_eq!(
            palette.confirm(),
            Some(PaletteAction::LaunchSsh("ssh://root@example.com:2222".into()))
        );
    }

    #[test]
    fn launcher_panel_height_stays_fixed_across_filtering() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[shell("CMD", "cmd", "cmd.exe"), shell("PowerShell", "powershell", "powershell.exe")],
            &[],
            "powershell",
        );
        palette.set_ssh_hosts(&[("生产机".into(), "root@example.com".into())]);
        palette.open_profiles();
        let full = palette_layout(&palette, 1600.0, 900.0, 1.0, 8.0).panel.3;
        palette.input_text("cmd");
        let filtered = palette_layout(&palette, 1600.0, 900.0, 1.0, 8.0).panel.3;
        assert_eq!(full, filtered, "搜索结果变化不能让 launcher 上下跳动");
        palette.set_launcher_filter(LauncherFilter::Ssh);
        let ssh = palette_layout(&palette, 1600.0, 900.0, 1.0, 8.0).panel.3;
        assert_eq!(full, ssh, "切换分组不能改变 launcher 高度");
    }

    #[test]
    fn launcher_content_stays_inside_the_panel_at_high_dpi() {
        let shells: Vec<_> = (0..7)
            .map(|index| shell(&format!("Shell {index}"), &format!("shell-{index}"), "shell.exe"))
            .collect();
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&shells, &[], "shell-0");
        palette.set_ssh_hosts(&[("production".into(), "root@example.com".into())]);
        palette.open_profiles();

        let layout = palette_layout(&palette, 1913.0, 1110.0, 1.5, 12.0);
        let panel_bottom = layout.panel.1 + layout.panel.3;
        let content_bottom = layout.footer.map_or(panel_bottom, |(_, footer_y, _, _)| footer_y);
        assert_eq!(layout.max_rows, 5, "启动器保持紧凑的五行视口");
        assert!(layout.rows.iter().all(|(y, h)| y + h <= content_bottom));
        assert!(layout.groups.iter().all(|(y, _, _)| *y < content_bottom));
        assert!(layout.footer.is_none_or(|(_, y, _, h)| y + h <= panel_bottom));
    }

    #[test]
    fn imported_profile_keeps_shell_icon_and_click_snapshot() {
        let profile = Profile {
            name: "PowerShell 7".into(),
            command: r"D:\\tools\\pwsh.exe".into(),
            args: vec!["-NoLogo".into()],
            cwd: None,
            shell_id: Some("pwsh".into()),
            terminal_profile_id: Some("pwsh-test".into()),
        };
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&[], std::slice::from_ref(&profile), "powershell");
        palette.open_profiles();

        let (rows, _) = palette.visible(8);
        assert_eq!(rows[0].label, "PowerShell 7");
        assert_eq!(rows[0].icon, crate::shell_detect::icon_for_id("pwsh"));
        assert_eq!(palette.click(0, 8), Some(PaletteAction::LaunchProfile(profile)));
    }

    #[test]
    fn imported_profile_can_be_selected_as_default_shell() {
        let shell = shell("CMD", "cmd", r"C:\\Windows\\System32\\cmd.exe");
        let profile = Profile {
            name: "PowerShell 7".into(),
            command: r"D:\\tools\\pwsh.exe".into(),
            args: vec![],
            cwd: None,
            shell_id: Some("pwsh".into()),
            terminal_profile_id: Some("pwsh-test".into()),
        };
        let default_id = profile.settings_id().unwrap();
        let mut palette = CommandPalette::new();
        palette.set_default_shell_menu(&[shell], std::slice::from_ref(&profile), &default_id);
        palette.open_default_picker();
        palette.move_selection(1, 8);

        assert_eq!(palette.confirm(), Some(PaletteAction::SetDefaultProfile(profile)));
    }

    #[test]
    fn search_input_and_result_rows_share_compact_height() {
        let palette = CommandPalette::new();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.5, 12.0);
        assert_eq!(layout.input.3, layout.row_h);
    }

    fn shell(name: &str, id: &str, program: &str) -> DetectedShell {
        DetectedShell { name: name.into(), id: id.into(), program: program.into(), args: vec![] }
    }

    #[test]
    fn shell_menu_keeps_default_first_without_a_hero_row() {
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
        // 检测顺序把 CMD 排前，但默认 shell 仍排在第一；身份只靠 badge，
        // 不再用额外高度破坏列表节奏。
        assert!(!palette.hero_row());
        let (rows, _) = palette.visible(10);
        assert_eq!(rows[0].label, "PowerShell");
        assert!(rows[0].is_default);
        palette.input_char('c');
        assert!(!palette.hero_row());
    }

    /// 命令列表按分类分组：组的顺序固定（工作目录→标签页→跳转→视图→
    /// 工作区→外观→设置→shell），组内保持「最近优先」。表头只在每组第一
    /// 行起一次。
    #[test]
    fn commands_group_by_category_and_headers_start_each_group() {
        let palette = CommandPalette::new();
        let visible = palette.filtered.len().min(24);
        let groups: Vec<_> =
            palette.filtered.iter().take(visible).map(|c| palette.command_group(*c)).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted, "同一分类的命令必须连续，否则表头会切进组中间");

        let labels = palette.group_labels(visible).unwrap();
        assert_eq!(labels[0].as_ref().map(|(l, _)| l.as_str()), Some("标签页"));
        assert_eq!(labels[1], None, "组内第二行不再起表头");
        // 起表头的次数 == 分类数
        let distinct = {
            let mut seen = groups.clone();
            seen.dedup();
            seen.len()
        };
        assert_eq!(labels.iter().flatten().count(), distinct);
    }

    /// cwd 未知时整个「工作目录」组不出现——留一组作用在空路径上的死命令
    /// 比少一组糟得多。cwd 一到，这组排在最前，路径挂在表头右缘（写一次，
    /// 而不是每行重复），「新建标签页」也从标签页组挪进来：它确实继承那个
    /// 目录，组名已经把「在此」说清楚了。
    #[test]
    fn cwd_group_appears_only_with_a_working_directory() {
        let mut palette = CommandPalette::new();
        let without: Vec<_> = palette
            .group_labels(palette.filtered.len())
            .unwrap()
            .into_iter()
            .flatten()
            .map(|(label, _)| label)
            .collect();
        assert!(!without.iter().any(|l| l == "工作目录"), "没有 cwd 就没有这一组");
        assert!(!palette.filtered.iter().any(|&c| matches!(
            c,
            PaletteCandidate::Item(i) if ITEMS[i].action == PaletteAction::CopyCwd
        )));

        palette.set_context(PaletteContext {
            cwd: Some(PathBuf::from("D:\\temp_build\\nebula")),
            sidebar: true,
            panel_resize: false,
            new_tab_inherits_cwd: true,
        });
        let labels = palette.group_labels(palette.filtered.len()).unwrap();
        let (label, context) = labels[0].as_ref().expect("第一行起一条表头");
        assert_eq!(label, "工作目录", "有 cwd 时它排最前");
        assert_eq!(context, "D:\\temp_build\\nebula", "路径挂在表头右缘");
        assert_eq!(
            palette.command_group(palette.filtered[0]),
            CommandGroup::Cwd,
            "「新建标签页」跟着进工作目录组"
        );

        // 配了启动目录：新标签页开在别处，于是它退回标签页组，而复制 / 定位
        // 两条仍然作用在 cwd 上——组还在，只是少一行。
        palette.set_context(PaletteContext {
            cwd: Some(PathBuf::from("D:\\temp_build\\nebula")),
            sidebar: true,
            panel_resize: false,
            new_tab_inherits_cwd: false,
        });
        let new_tab = ITEMS.iter().position(|item| item.action == PaletteAction::NewTab).unwrap();
        assert_eq!(
            palette.command_group(PaletteCandidate::Item(new_tab)),
            CommandGroup::Tabs,
            "开不在那里就不能挂在工作目录标题下"
        );
        assert!(
            palette
                .group_labels(palette.filtered.len())
                .unwrap()
                .into_iter()
                .flatten()
                .any(|(label, _)| label == "工作目录")
        );
    }

    /// 开关类命令带勾选态，一次性动作不带。列表里同时有两类时，✓ 是唯一
    /// 能把它们分开的信号；`None` 与 `Some(false)` 的区别不能丢——后者是
    /// 「开关，当前关着」，前者是「这行根本不是开关」。
    #[test]
    fn toggle_rows_carry_their_current_state() {
        let mut palette = CommandPalette::new();
        palette.set_context(PaletteContext {
            cwd: None,
            sidebar: true,
            panel_resize: false,
            new_tab_inherits_cwd: true,
        });
        let row_of = |palette: &CommandPalette, action: PaletteAction| {
            let index = ITEMS.iter().position(|item| item.action == action).unwrap();
            palette.row_for(PaletteCandidate::Item(index)).checked
        };
        assert_eq!(row_of(&palette, PaletteAction::ToggleSidebar), Some(true));
        assert_eq!(row_of(&palette, PaletteAction::TogglePanelResize), Some(false));
        assert_eq!(row_of(&palette, PaletteAction::NewTab), None, "一次性动作不是开关");
    }

    /// 表头跟着滚动窗口走。此前 `group_labels` 取的是 `filtered` 的**前 N
    /// 条**、行取的是滚动后的窗口，两边基准不同：往下翻一页，行换了、表头
    /// 还写着列表开头那几组，标题于是指着一批已经滚走的行。
    #[test]
    fn headers_follow_the_scrolled_window() {
        const MAX_ROWS: usize = 4;
        let mut palette = CommandPalette::new();
        palette.open();
        palette.set_context(PaletteContext {
            cwd: Some(PathBuf::from("D:\\temp_build\\nebula")),
            sidebar: false,
            panel_resize: false,
            new_tab_inherits_cwd: true,
        });
        assert!(palette.filtered.len() > MAX_ROWS * 2, "要够长才能翻页");

        let group_at =
            |palette: &CommandPalette, index: usize| palette.command_group(palette.filtered[index]);
        let top_label = |palette: &CommandPalette| {
            palette.group_labels(MAX_ROWS).unwrap()[0].as_ref().map(|(l, _)| l.clone())
        };
        assert_eq!(top_label(&palette), Some(group_at(&palette, 0).label(palette.language)));

        // 一路下翻到第 9 行：窗口变成 6..10，表头必须跟着换成第 6 行那一组。
        palette.move_selection(9, MAX_ROWS);
        let start = palette.scroll_start(MAX_ROWS);
        assert_eq!(start, 6, "选中行停在底行");
        let labels = palette.group_labels(MAX_ROWS).unwrap();
        let (rows, selected) = palette.visible(MAX_ROWS);
        assert_eq!(labels.len(), rows.len(), "表头与行必须逐条对齐");
        assert_eq!(selected, Some(MAX_ROWS - 1));
        assert_eq!(
            top_label(&palette),
            Some(group_at(&palette, start).label(palette.language)),
            "窗口首行永远起一条表头，写的是**它自己**那一组"
        );
        // 窗口内其余各行只在换组时起表头。
        for (offset, label) in labels.iter().enumerate().skip(1) {
            let changed =
                group_at(&palette, start + offset) != group_at(&palette, start + offset - 1);
            assert_eq!(label.is_some(), changed, "第 {offset} 行的表头判定与实际换组不符");
        }
    }

    #[test]
    fn wheel_scroll_uses_an_independent_clamped_window() {
        const MAX_ROWS: usize = 5;
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(palette.filtered.len() > MAX_ROWS);

        assert!(palette.scroll_by(3, MAX_ROWS));
        assert_eq!(palette.scroll_start(MAX_ROWS), 3);
        let expected = palette.row_for(palette.filtered[3]).label;
        let (rows, selected) = palette.visible(MAX_ROWS);
        assert_eq!(rows[0].label, expected);
        assert_eq!(selected, Some(0), "命令面板默认执行当前窗口首行");

        assert!(palette.scroll_by(i32::MAX, MAX_ROWS));
        assert_eq!(palette.scroll_start(MAX_ROWS), palette.filtered.len() - MAX_ROWS);
        assert!(!palette.scroll_by(1, MAX_ROWS), "列表底部继续下滚应保持不变");
    }

    #[test]
    fn overflowing_palette_exposes_a_draggable_scrollbar() {
        const MAX_ROWS: usize = 8;
        let mut palette = CommandPalette::new();
        palette.open();
        let layout = palette_layout(&palette, 1200.0, 900.0, 1.0, 8.0);
        assert_eq!(layout.max_rows, MAX_ROWS);
        let scrollbar = layout.scrollbar.expect("长命令列表必须显示滚动条");

        let (tx, ty, tw, th) = scrollbar.track;
        assert!(scrollbar.hit_test(tx + tw * 0.5, ty + th - 1.0));
        assert!(palette.scrollbar_press(tx + tw * 0.5, ty + th - 1.0, layout.max_rows, scrollbar,));
        assert_eq!(palette.scroll_start(MAX_ROWS), palette.filtered.len() - MAX_ROWS);
        assert!(palette.scrollbar_dragging());
        assert!(palette.end_scrollbar_drag());
    }

    /// 分组表头的前提是同来源的行**连续**。模糊搜索按得分排序会把两家的
    /// 会话打散，所以 refilter 末尾必须再做一次稳定划分；打头的那一组是
    /// 拥有最新一条会话的来源，「我刚才在做的那件事」不会被压到第二组。
    #[test]
    fn ai_sessions_stay_grouped_by_source_even_while_searching() {
        use crate::ai_sessions::AiSessionSource::{Claude, Codex};
        let row = |source, label: &str| AiSessionRow {
            label: label.to_owned(),
            hint: String::new(),
            search: label.to_owned(),
            command: String::new(),
            source,
        };
        let mut palette = CommandPalette::new();
        // 交错到达，最新的一条（首行）是 claude。
        palette.open_ai_sessions(vec![
            row(Claude, "aa 修复"),
            row(Codex, "ab 修复"),
            row(Claude, "ac 修复"),
            row(Codex, "ad 修复"),
        ]);
        let sources = |p: &CommandPalette| -> Vec<_> {
            p.filtered
                .iter()
                .map(|c| match c {
                    PaletteCandidate::AiSession(i) => p.ai_sessions[*i].source,
                    _ => unreachable!(),
                })
                .collect()
        };
        assert_eq!(sources(&palette), vec![Claude, Claude, Codex, Codex], "空查询就该分好组");
        let labels = palette.group_labels(4).unwrap();
        assert_eq!(labels[0].as_ref().map(|(l, _)| l.as_str()), Some("CLAUDE CODE 会话"));
        assert_eq!(labels[1], None, "组内第二行不再起表头");
        assert_eq!(labels[2].as_ref().map(|(l, _)| l.as_str()), Some("CODEX 会话"));
        assert_eq!(labels[3], None);
    }

    #[test]
    fn ai_session_panel_height_stays_fixed_while_filtering() {
        use crate::ai_sessions::AiSessionSource::{Claude, Codex};
        let rows = (0..12)
            .map(|index| AiSessionRow {
                label: format!("session {index}"),
                hint: String::new(),
                search: if index == 0 {
                    "unique needle".to_owned()
                } else {
                    format!("session {index}")
                },
                command: String::new(),
                source: if index % 2 == 0 { Claude } else { Codex },
            })
            .collect();
        let mut palette = CommandPalette::new();
        palette.open_ai_sessions(rows);
        let full = palette_layout(&palette, 1600.0, 1000.0, 1.0, 8.0).panel.3;

        palette.input_text("unique needle");
        assert_eq!(palette.filtered.len(), 1);
        let filtered = palette_layout(&palette, 1600.0, 1000.0, 1.0, 8.0).panel.3;

        assert_eq!(filtered, full, "过滤结果不能改变 AI 会话面板高度");
    }

    #[test]
    fn launcher_rows_are_equal_and_chip_hit_test_matches() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(
            &[
                shell("CMD", "cmd", r"C:\Windows\System32\cmd.exe"),
                shell("PowerShell", "powershell", r"C:\Windows\...\powershell.exe"),
            ],
            &[],
            "powershell",
        );
        palette.set_ssh_hosts(&[("生产机".into(), "root@192.0.2.10".into())]);
        palette.open_profiles();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.0, 8.0);
        assert_eq!(layout.rows.len(), 3);
        assert_eq!(layout.chips.len(), 3);
        assert_eq!(
            layout.chips.iter().map(|chip| chip.label).collect::<Vec<_>>(),
            ["全部", "SSH", "Shell"]
        );
        assert_eq!(layout.chips.iter().map(|chip| chip.count).collect::<Vec<_>>(), [3, 1, 2]);
        let (first_y, first_h) = layout.rows[0];
        let (row_y, row_h) = layout.rows[1];
        assert_eq!(first_h, row_h, "默认项与普通项必须等高");
        assert!(row_y - first_y > first_h, "类别切换必须留出标题的分隔空间");
        assert_eq!(
            layout.groups.iter().map(|(_, label, _)| label.as_str()).collect::<Vec<_>>(),
            ["推荐", "所有 Shell", "SSH 主机"]
        );
        let (px, ..) = layout.panel;
        assert_eq!(layout.row_at(px + 10.0, first_y + first_h / 2.0), Some(0));
        assert_eq!(layout.row_at(px + 10.0, row_y + row_h / 2.0), Some(1));
        assert_eq!(layout.row_at(px + 10.0, first_y - 2.0), None, "chip 分组区不是行");
        assert_eq!(layout.row_at(px - 20.0, first_y + 2.0), None, "面板外不命中");
        let ssh = &layout.chips[1];
        assert_eq!(layout.chip_at(ssh.rect.0 + 2.0, ssh.rect.1 + 2.0), Some(LauncherFilter::Ssh));
        let (_, band_y, _, band_h) = layout.chip_band.expect("launcher 必须有 chip 背景带");
        for chip in &layout.chips {
            let chip_center = chip.rect.1 + chip.rect.3 * 0.5;
            let band_center = band_y + band_h * 0.5;
            assert!((chip_center - band_center).abs() < 0.01, "chip 必须与背景带垂直居中");
        }
    }

    #[test]
    fn launcher_pane_respects_reserved_workspace_bounds() {
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&[shell("PowerShell", "powershell", "pwsh.exe")], &[], "powershell");
        palette.open_profiles();

        let left = 230.0;
        let right = 1300.0;
        let layout = palette_layout_with_workspace_bounds(
            &palette,
            1600.0,
            900.0,
            1.0,
            8.0,
            Some((left, right)),
        );
        let (panel_x, _, panel_w, _) = layout.panel;
        assert!(panel_x >= left);
        assert!(panel_x + panel_w <= right);
        assert!(layout.input.0 >= left);
        assert!(layout.input.0 + layout.input.2 <= right);
    }

    #[test]
    fn command_and_jump_panels_match_launcher_workspace_width() {
        use crate::ai_sessions::AiSessionSource;

        let bounds = (230.0, 1300.0);
        let layout = |palette: &CommandPalette| {
            palette_layout_with_workspace_bounds(palette, 1600.0, 1000.0, 1.0, 8.0, Some(bounds))
        };

        let mut launcher = CommandPalette::new();
        launcher.set_shell_menu(
            &[shell("PowerShell", "powershell", "powershell.exe")],
            &[],
            "powershell",
        );
        launcher.open_profiles();

        let mut commands = CommandPalette::new();
        commands.open();

        let mut jump = CommandPalette::new();
        jump.open_ai_sessions(vec![AiSessionRow {
            label: "继续修复".into(),
            hint: "nebula · 刚刚".into(),
            search: "继续修复 nebula".into(),
            command: "codex resume test".into(),
            source: AiSessionSource::Codex,
        }]);

        let launcher_panel = layout(&launcher).panel;
        for panel in [layout(&commands).panel, layout(&jump).panel] {
            assert_eq!(panel.0, launcher_panel.0, "快捷面板必须共享工作区左缘");
            assert_eq!(panel.2, launcher_panel.2, "快捷面板必须与 Shell 选择器同宽");
        }
    }

    #[test]
    fn launcher_chip_count_stays_inside_pill_at_double_digits() {
        let shells: Vec<_> = (0..10)
            .map(|index| DetectedShell {
                name: format!("Shell {index}"),
                id: format!("shell-{index}"),
                program: format!(r"C:\Shells\{index}\shell.exe"),
                args: Vec::new(),
            })
            .collect();
        let mut palette = CommandPalette::new();
        palette.set_shell_menu(&shells, &[], "shell-0");
        palette.set_ssh_hosts(&[("生产机".into(), "root@192.0.2.10".into())]);
        palette.open_profiles();

        let scale = 1.25;
        let cell_w = 10.5;
        let layout = palette_layout(&palette, 1600.0, 900.0, scale, cell_w);
        assert_eq!(layout.chips.iter().map(|chip| chip.count).collect::<Vec<_>>(), [11, 1, 10]);

        for chip in &layout.chips {
            let count = chip.count.to_string();
            let (label_x, count_x) = launcher_chip_text_x(chip, &count, cell_w, scale);
            let label_right = label_x + text_width_cols(chip.label) as f32 * cell_w;
            let count_right = count_x + text_width_cols(&count) as f32 * cell_w;
            assert!((count_x - label_right - LAUNCHER_CHIP_TEXT_GAP * scale).abs() < 0.01);
            assert!(
                count_right <= chip.rect.0 + chip.rect.2 - LAUNCHER_CHIP_PAD_X * scale + 0.01,
                "计数必须完整落在 chip 右内边距之前"
            );
        }
    }

    #[test]
    fn command_layout_rows_stay_uniform() {
        let mut palette = CommandPalette::new();
        palette.open();
        let layout = palette_layout(&palette, 1600.0, 900.0, 1.0, 8.0);
        assert_eq!(layout.rows.len(), layout.max_rows.min(palette.filtered.len()));
        for pair in layout.rows.windows(2) {
            assert_eq!(pair[0].1, pair[1].1, "命令列表保持等高行");
            assert_eq!(pair[1].0 - pair[0].0, pair[0].1, "无缝隙");
        }
    }

    #[test]
    fn detail_paths_share_a_right_edge_after_ellipsis() {
        let right = 900.0;
        let cell_w = 10.0;
        for value in ["cmd.exe", r"C:\Program Files\PowerShell\7\pwsh.exe"] {
            let (shown, x) = fit_right_detail(value, 18, right, cell_w);
            assert_eq!(x + text_width_cols(&shown) as f32 * cell_w, right);
            assert!(text_width_cols(&shown) <= 18);
        }
    }

    #[test]
    fn long_paths_keep_their_root_and_ellipsize_at_the_tail() {
        // 2026-07-29 裁定：溢出保头部（盘符/根上下文），尾部省略号——
        // 叶名辨识交给标签和推荐卡的完整路径。
        let shown = fit_head(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", 20);
        assert!(shown.starts_with("C:\\Windows") && shown.ends_with('…'), "got {shown}");
    }

    #[test]
    fn tail_ellipsis_counts_display_columns_not_chars() {
        // 目录 picker 的路径可含 CJK；按 char 数裁会溢出面板。
        let shown = fit_head("D:/项目/一个很长的子目录名字", 10);
        let cols: usize = shown.chars().map(|c| c.width().unwrap_or(0)).sum();
        assert!(cols <= 10, "按显示列宽裁剪，实测 {cols} 列");
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
