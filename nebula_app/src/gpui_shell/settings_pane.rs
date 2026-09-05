//! 设置页（GPUI 组件版，覆盖旧壳设置的全部纯键值项）。
//!
//! 版面对齐旧壳：**左侧分区导航 + 右侧单分区内容**（分区名与顺序对照
//! `NebulaSettingsSection`：外观/配置文件/供应商/SSH/网络/交互/按键映射/
//! 高级/备份），并以应用主页承载关于、更新与支持入口。
//!
//! 薄壳纪律：本文件不定义任何设置语义——键名、值域、出厂默认、主题色表、
//! 持久化格式全部在共享 crate `nebula-settings`（从旧壳逐字迁移并有单测
//! 锁定）。这里只负责用组件库把字段摆出来；改动经 `persist_keys` 原地
//! 写回 `nebula_settings.txt`，与旧壳读写同一份文件、同一套语义。
//!
//! 生效时机（对齐旧壳）：主题/配色/背景/字号/模糊/透明/copy_on_select
//! 即时热应用；字体族与默认光标形状对新标签页生效。
//!
//! SSH 主机、AI Provider、按键映射和备份编辑器已经复用共享业务层与持久化合同。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ModifiersChangedEvent, MouseButton,
    MouseMoveEvent, ParentElement as _, Render, RenderImage, Rgba as GpuiRgba, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred,
    div, img, px,
};
use gpui_component::input::InputEvent;
use gpui_component::select::{SelectEvent, SelectItem};
use nebula_settings::{RuntimeSettings, ThemeName, format_hex_rgb, persist_keys};

use design::GROUP_GAP;
use std::sync::Arc;
use std::time::Duration;

use crate::gpui_shell::config::{DEFAULT_CURSOR_BLINK, effective_cursor_blink};
use crate::gpui_shell::prelude::*;
use crate::gpui_shell::widgets::NebulaButton;

mod about;
mod app_icon;
mod appearance;
mod appearance_advanced;
mod appearance_picker;
#[path = "background_color.rs"]
mod background_color;
mod backup;
mod design;
mod font_picker;
mod providers;
mod theme_picker;

mod keymap;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
const THEME_VALUES: [&str; 9] = [
    "Nebula",
    "SilverLight",
    "SteelDark",
    "LimestoneLight",
    "CoalDark",
    "LinenLight",
    "MossDark",
    "Nord",
    "Paper",
];

const REPOSITORY_URL: &str = "https://github.com/Kuddev/nebula";
const BUG_REPORT_TEMPLATE: &str = "bug_report.yml";

/// 左侧分区的稳定路由表。2026-08-28 产品裁定：默认 GPUI 导航收敛为常用项，
/// 暂时隐藏“AI 供应商”和“备份”；页面实现与索引继续保留。后续恢复入口时只改
/// [`HIDDEN_NAV_SECTIONS`]，不得删除或重排这里的条目。
const SECTION_IDS: [&str; 10] = [
    "application",
    "appearance",
    "profiles",
    "providers",
    "ssh",
    "network",
    "interaction",
    "keymap",
    "advanced",
    "backup",
];

/// Bilingual search aliases for the stable section routes. Search is a route
/// finder, so a query such as "font", "opacity", or "更新" lands on the
/// section that owns the control instead of merely filtering the current page.
const SECTION_SEARCH_TERMS: [&str; 10] = [
    "application app 应用 update 更新 version 版本 github support 支持",
    "appearance 外观 theme 主题 font 字体 opacity 透明度 background 背景 cursor 光标 icon 图标",
    "profiles 配置文件 shell terminal 终端 completion 补全 startup 启动",
    "providers provider ai 供应商 模型 api",
    "ssh host 主机 remote 远程 connection 连接",
    "network 网络 proxy 代理 connectivity 连接",
    "interaction 交互 copy 复制 paste 粘贴 tab 标签 panel 面板",
    "keymap key binding shortcut 按键映射 快捷键",
    "advanced 高级 session 会话 tray 托盘 restore 恢复",
    "backup 备份 export 导出 restore 恢复",
];

const HIDDEN_NAV_SECTIONS: &[usize] = &[3, 9];

/// 保留原来的分组展开顺序，组名不再渲染；数组里仍保存稳定的 [`SECTION_IDS`]
/// 下标，不复制设置状态或路由。
const NAV_GROUPS: [(&str, &[usize]); 3] =
    [("workspace", &[0, 1, 2, 6, 7]), ("connections", &[3, 4, 5]), ("system", &[8, 9])];

fn section_label(index: usize, language: crate::display::UiLanguage) -> &'static str {
    match SECTION_IDS.get(index).copied() {
        Some("application") => language.tr("settings.sidebar.application"),
        Some("appearance") => language.tr("settings.sidebar.appearance"),
        Some("profiles") => language.tr("settings.sidebar.profiles"),
        Some("providers") => language.tr("settings.sidebar.providers"),
        Some("ssh") => language.tr("settings.sidebar.ssh"),
        Some("network") => language.tr("settings.sidebar.network"),
        Some("interaction") => language.tr("settings.sidebar.interaction"),
        Some("keymap") => language.tr("settings.sidebar.keymap"),
        Some("advanced") => language.tr("settings.sidebar.advanced"),
        Some("backup") => language.tr("settings.sidebar.backup"),
        _ => "",
    }
}

fn is_nav_section_visible(index: usize) -> bool {
    !HIDDEN_NAV_SECTIONS.contains(&index)
}

fn visible_nav_sections() -> impl Iterator<Item = usize> {
    NAV_GROUPS
        .iter()
        .flat_map(|(_, sections)| sections.iter().copied())
        .filter(|index| is_nav_section_visible(*index))
}

// 这些几何值逐项来自旧壳 `display/settings.rs::settings_geometry`。GPUI
// 设置页沿用同一节奏，避免组件默认间距把标题、分组和表单压成一条均匀列表。
const SETTINGS_NAV_WIDTH: f32 = 232.0;
const SETTINGS_HEADER_HEIGHT: f32 = 52.0;

const SETTINGS_GROUP_GAP: f32 = 32.0;
const SETTINGS_GROUP_TITLE_HEIGHT: f32 = 26.0;
const SETTINGS_GROUP_TITLE_GAP: f32 = 16.0;
const SETTINGS_ROW_HEIGHT: f32 = 44.0;
const SETTINGS_ROW_GAP: f32 = 8.0;
/// 标准设置选择器的实际宽度。字体输入与 Select 共用，避免同列控件漂移。
const SETTINGS_SELECT_WIDTH: f32 = 220.0;

/// 列表/触发条上的展示名：族名偶尔带着导入文件后缀，界面上剥掉。
fn font_display_name(name: &str) -> String {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    for ext in [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return trimmed[..stem.len()].to_owned();
        }
    }
    trimmed.to_owned()
}

/// 导航图标。
///
/// 统一走路径字符串而不是 `IconName`：绝大多数取 lucide 的现成图标，但
/// 「按键映射」在 lucide 里没有对应项（原来错挂成 `ALargeSmall`，那是字号
/// 图标），只能自带一枚——两种来源用同一个类型表达，调用点就不必分叉。
fn section_icon(index: usize) -> SharedString {
    use gpui_component::IconNamed as _;
    match index {
        0 => IconName::LayoutDashboard.path(),
        1 => IconName::Palette.path(),
        2 => IconName::GalleryVerticalEnd.path(),
        3 => IconName::Bot.path(),
        4 => IconName::SquareTerminal.path(),
        5 => IconName::Globe.path(),
        6 => IconName::Inspector.path(),
        7 => crate::gpui_shell::assets::nav::KEYMAP.into(),
        8 => IconName::Settings2.path(),
        _ => IconName::Inbox.path(),
    }
}

fn chrome_theme(theme: ThemeName) -> crate::display::NebulaTheme {
    use crate::display::NebulaTheme;
    match theme {
        ThemeName::Nebula => NebulaTheme::Nebula,
        ThemeName::SilverLight => NebulaTheme::SilverLight,
        ThemeName::SteelDark => NebulaTheme::SteelDark,
        ThemeName::LimestoneLight => NebulaTheme::LimestoneLight,
        ThemeName::CoalDark => NebulaTheme::CoalDark,
        ThemeName::LinenLight => NebulaTheme::LinenLight,
        ThemeName::MossDark => NebulaTheme::MossDark,
        ThemeName::Nord => NebulaTheme::Nord,
        ThemeName::Paper => NebulaTheme::Paper,
    }
}

fn rgb_hsla(r: u8, g: u8, b: u8) -> Hsla {
    GpuiRgba { r: f32::from(r) / 255.0, g: f32::from(g) / 255.0, b: f32::from(b) / 255.0, a: 1.0 }
        .into()
}

fn query_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

/// GitHub issue form 预填合同：模板、版本、平台、安装来源
/// 和诊断摘要都来自当前运行实例，用户只需补充复现步骤与实际表现。
fn issue_url() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let platform = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        _ => "Other",
    };
    let install_source = if cfg!(debug_assertions) {
        "Built from source (cargo build / cargo run)"
    } else {
        "GitHub Release (.msi / .exe / portable archive)"
    };
    let build = if cfg!(debug_assertions) { "debug" } else { "release" };
    let logs = format!(
        "Reported from {} Settings ({} {version}).\n\nPlatform: {} {}\nBuild: {build}",
        crate::brand::NAME,
        crate::brand::NAME,
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let params = [
        ("template", BUG_REPORT_TEMPLATE),
        ("title", "[Bug] "),
        ("version", version),
        ("platform", platform),
        ("install_source", install_source),
        ("logs", logs.as_str()),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", query_component(value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{REPOSITORY_URL}/issues/new?{params}")
}

/// 宿主（workspace）监听：设置已写盘 / 终端目录已变 / 请求打开 SSH 会话。
pub enum SettingsPaneEvent {
    /// Return to the workspace. Settings is a window-level page, not a tab.
    Close,
    Changed,
    /// 导入 Profile 已落盘；Tab 的 Shell 面板若正打开，需要重建候选快照。
    TerminalProfilesChanged,
    /// 设置页"连接"按钮：宿主开 SSH tab（连接语义在业务层）。
    LaunchSsh(String),
}

pub(super) type SharedSelect = Entity<SelectState<Vec<SharedString>>>;
type SharedShellSelect = Entity<SelectState<Vec<ShellSelectItem>>>;

/// 固定下拉只在这里把稳定 value 映射为显示文案。创建和语言切换刷新共用
/// 同一入口，避免 `SelectState` 留着构造时的旧语言。
fn localized_select_labels(
    key: &str,
    values: &[&'static str],
    language: crate::display::UiLanguage,
) -> Vec<SharedString> {
    let labels: Vec<&'static str> = match key {
        "language" => vec![
            language.tr("language.system"),
            language.tr("language.zh_cn"),
            language.tr("language.en_us"),
        ],
        "cursor_shape" => vec![
            language.pick("条形（│）", "Bar (│)"),
            language.pick("下划线（_）", "Underscore (_)"),
            language.pick("实心框（█）", "Filled box (█)"),
            language.pick("空心框（□）", "Empty box (□)"),
        ],
        "tabs_position" => {
            vec![language.pick("左侧边栏", "Left sidebar"), language.pick("顶部", "Top")]
        },
        "tab_reveal" => vec![language.pick("滑动", "Slide"), language.pick("立即", "Instant")],
        "density" => vec![language.pick("标准", "Standard"), language.pick("紧凑", "Compact")],
        "new_tab_position" => vec![
            language.pick("当前标签之后", "After current tab"),
            language.pick("列表末尾", "End of list"),
        ],
        "windowing_behavior" => vec![
            language.pick("创建新窗口", "Create a new window"),
            language.pick("附加到最近使用的窗口", "Attach to the most recent window"),
            language.pick(
                "附加到此桌面最近使用的窗口",
                "Attach to the most recent window on this desktop",
            ),
        ],
        "vcs_display" => vec![
            language.pick("自动检测", "Auto detect"),
            language.pick("仅 Git", "Git only"),
            language.pick("仅 SVN", "SVN only"),
        ],
        "cell_width_mode" => {
            vec![language.pick("紧凑", "Compact"), language.pick("宽松", "Relaxed")]
        },
        "bell" => vec![
            language.pick("关", "Off"),
            language.pick("闪烁", "Visual"),
            language.pick("声音", "Sound"),
            language.pick("闪烁 + 声音", "Visual + sound"),
        ],
        "blur" => vec![
            language.pick("无", "None"),
            language.pick("Mica（低开销）", "Mica (low cost)"),
            language.pick("Mica Alt（低开销）", "Mica Alt (low cost)"),
            language.pick("Aero（玻璃）", "Aero (glass)"),
            language.pick("Acrylic（高开销）", "Acrylic (high cost)"),
        ],
        "accept" => vec![
            language.pick("右方向键", "Right arrow"),
            "Tab",
            language.pick("Tab 或右方向键", "Tab or Right arrow"),
        ],
        "completion_style" => {
            vec![language.pick("行内灰字", "Inline ghost"), language.pick("弹窗列表", "Popup list")]
        },
        "background_image_fit" => vec![
            language.pick("拉伸", "Fill"),
            language.pick("适应", "Uniform"),
            language.pick("填充", "Uniform to fill"),
            language.pick("原始尺寸", "Original size"),
        ],
        "background_image_alignment" => vec![
            language.pick("左上", "Top left"),
            language.pick("顶部", "Top"),
            language.pick("右上", "Top right"),
            language.pick("左侧", "Left"),
            language.pick("居中", "Center"),
            language.pick("右侧", "Right"),
            language.pick("左下", "Bottom left"),
            language.pick("底部", "Bottom"),
            language.pick("右下", "Bottom right"),
        ],
        "ssh_proxy_mode" => vec![
            language.tr("settings.network.mode.off"),
            language.tr("settings.network.mode.system"),
            language.tr("settings.network.mode.custom"),
        ],
        _ => values.to_vec(),
    };
    debug_assert_eq!(labels.len(), values.len(), "localized select label/value mismatch: {key}");
    labels.into_iter().map(SharedString::from).collect()
}

fn provider_input_placeholders(language: crate::display::UiLanguage) -> [&'static str; 5] {
    [
        language.pick("供应商名称", "Provider name"),
        language.pick("备注（可包含空格）", "Note (spaces allowed)"),
        language.pick("官方网站", "Official website"),
        language.pick("API 请求地址", "API endpoint"),
        language.pick("默认模型", "Default model"),
    ]
}

fn localized_input_placeholder(key: &str, language: crate::display::UiLanguage) -> &'static str {
    match key {
        "ssh_label" => language.pick("例如：开发服务器", "e.g. Development server"),
        "ssh_password" => language.pick("留空则连接时询问", "Leave empty to ask when connecting"),
        "ssh_proxy_username" => language.pick("可选", "Optional"),
        "ssh_proxy_password" => {
            language.pick("留空则保留已存密码", "Leave empty to keep the saved password")
        },
        "ssh_jump_host" => language
            .pick("user@bastion:22 或 SSH config 别名", "user@bastion:22 or SSH config alias"),
        "ssh_icon_filter" => language.pick("搜索图标…", "Search icons..."),
        "font_family" => language.pick("输入字体名称", "Enter a font family"),
        "backup_password" => {
            language.pick("备份密码（至少 8 位）", "Backup password (at least 8 characters)")
        },
        "backup_secret" => language.tr("settings.input.backup_secret"),
        "keymap_search" => language.pick("搜索动作或按键…", "Search actions or keys..."),
        _ => "",
    }
}

#[derive(Clone)]
struct ShellSelectItem {
    id: String,
    name: SharedString,
    closed_image: Option<Arc<RenderImage>>,
    row_image: Option<Arc<RenderImage>>,
}

/// 下拉首行那条「导入终端目录」动作行的哨兵 id。
///
/// 它不是任何真实 shell：选中只触发目录扫描，绝不会写进 `shell` 设置。
/// 取这个形状是因为 `shell_detect` 的 id 全是普通标识符（`pwsh`、`cmd`、
/// `wsl:<distro>`、`profile:<家族>|<id>`），双下划线包裹不可能与之相撞。
const SHELL_IMPORT_ACTION_ID: &str = "__nebula_import_terminal_dir__";

#[derive(Clone, Debug, Default)]
enum AboutUpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate(String),
    Available(String),
    Failed(String),
}

#[derive(Clone, Debug)]
enum ProviderStatus {
    Saved,
    Added,
    AtLeastOneRequired,
    Deleted,
    ApiKeySaved,
    Testing,
    TestResult { outcome: crate::provider_test::ProviderTestOutcome, elapsed_ms: u64 },
    CodexConfirmation,
    AppliedToCodex(std::path::PathBuf),
    Error(String),
}

impl ProviderStatus {
    fn is_error(&self) -> bool {
        match self {
            Self::AtLeastOneRequired | Self::Error(_) => true,
            Self::TestResult { outcome, .. } => !outcome.is_success(),
            _ => false,
        }
    }

    fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Saved => language.pick("供应商配置已保存", "Provider settings saved").into(),
            Self::Added => language.pick("已添加自定义供应商", "Custom provider added").into(),
            Self::AtLeastOneRequired => {
                language.pick("至少保留一个供应商", "Keep at least one provider").into()
            },
            Self::Deleted => language
                .pick("供应商及其凭据已删除", "Provider and its credentials deleted")
                .into(),
            Self::ApiKeySaved => language
                .pick(
                    "API Key 已保存到系统凭据管理器",
                    "API key saved to the system credential manager",
                )
                .into(),
            Self::Testing => language.pick("正在测试连接…", "Testing connection...").into(),
            Self::TestResult { outcome, elapsed_ms } => {
                format!("{} · {elapsed_ms} ms", language.provider_test_message(outcome))
            },
            Self::CodexConfirmation => language
                .pick(
                    "再次点击确认：API Key 将明文写入 Codex auth.json（原文件会备份）",
                    "Click again to confirm: the API key will be written in plain text to Codex auth.json (the original file will be backed up)",
                )
                .into(),
            Self::AppliedToCodex(path) => format!(
                "{}: {}",
                language.pick("已应用到 Codex", "Applied to Codex"),
                path.display()
            ),
            Self::Error(error) => format!(
                "{}: {error}",
                language.pick("操作失败", "Operation failed")
            ),
        }
    }
}

#[derive(Clone, Debug)]
enum BackupCompletion {
    Exported(std::path::PathBuf),
    Restored,
    Pushed(String),
    Pulled(String),
}

#[derive(Clone, Debug)]
enum BackupStatus {
    PassphraseTooShort,
    SelectionRequired,
    Processing,
    RemoteConfigSaved,
    CredentialEmpty,
    CredentialUnsupported,
    CredentialSaved,
    Completed(BackupCompletion),
    Error(String),
}

impl BackupStatus {
    fn is_error(&self) -> bool {
        matches!(
            self,
            Self::PassphraseTooShort
                | Self::SelectionRequired
                | Self::CredentialEmpty
                | Self::CredentialUnsupported
                | Self::Error(_)
        )
    }

    fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::PassphraseTooShort => language
                .pick("备份密码至少 8 位", "The backup password must be at least 8 characters")
                .into(),
            Self::SelectionRequired => language
                .pick("请至少勾选一个备份类别", "Select at least one backup category")
                .into(),
            Self::Processing => language.pick("处理中…", "Processing...").into(),
            Self::RemoteConfigSaved => {
                language.pick("远端配置已保存", "Remote configuration saved").into()
            },
            Self::CredentialEmpty => {
                language.pick("凭据不能为空", "Credentials cannot be empty").into()
            },
            Self::CredentialUnsupported => language
                .pick(
                    "当前协议不需要独立凭据",
                    "The current protocol does not use a separate credential",
                )
                .into(),
            Self::CredentialSaved => language
                .pick(
                    "凭据已写入系统凭据管理器",
                    "Credential saved to the system credential manager",
                )
                .into(),
            Self::Completed(BackupCompletion::Exported(path)) => format!(
                "{}: {}",
                language.pick("已导出加密备份", "Encrypted backup exported"),
                path.display()
            ),
            Self::Completed(BackupCompletion::Restored) => language
                .pick(
                    "已从备份恢复（字体/托盘等部分设置重启后生效）",
                    "Backup restored (some settings, including fonts and tray options, apply after restart)",
                )
                .into(),
            Self::Completed(BackupCompletion::Pushed(location)) => {
                format!("{} {location}", language.pick("已推送到", "Pushed to"))
            },
            Self::Completed(BackupCompletion::Pulled(name)) => format!(
                "{} {name} {}",
                language.pick("已从", "Restored from"),
                language.pick("恢复（部分设置重启后生效）", "(some settings apply after restart)")
            ),
            Self::Error(error) => format!(
                "{}: {error}",
                language.pick("备份操作失败", "Backup operation failed")
            ),
        }
    }
}

#[derive(Debug)]
enum TerminalImportError {
    Scan(String),
    NoSupportedTerminal,
    Load(String),
    Import(String),
    Save(String),
}

impl TerminalImportError {
    fn text(self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Scan(error) => format!(
                "{}: {error}",
                language.pick("无法扫描终端目录", "Could not scan the terminal directory")
            ),
            Self::NoSupportedTerminal => language
                .pick(
                    "目录中未找到受支持的终端程序",
                    "No supported terminal program was found in the directory",
                )
                .into(),
            Self::Load(error) => format!(
                "{}: {error}",
                language.pick("无法读取终端配置", "Could not read terminal profiles")
            ),
            Self::Import(error) => format!(
                "{}: {error}",
                language.pick("无法导入终端", "Could not import the terminal")
            ),
            Self::Save(error) => format!(
                "{}: {error}",
                language.pick("无法保存终端配置", "Could not save terminal profiles")
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum SshStatus {
    Saved(String),
    Pinned,
    Imported(usize),
    Opening(String),
    DeleteCommitted { hidden_config: bool },
    CleanupPartial(String),
    Restored(String),
    Validation(SshValidationError),
    PersistFailed(String),
    DeleteFailed(String),
    UndoFailed(String),
    TestStartFailed(String),
    ProfileLoadFailed(String),
    ProfileSaveFailed(String),
    HostListSaveFailed(String),
    CredentialSaveFailed(String),
    SavedWithCleanupError { destination: String, error: String },
    Error(String),
}

impl SshStatus {
    pub(super) fn is_error(&self) -> bool {
        matches!(
            self,
            Self::CleanupPartial(_)
                | Self::Validation(_)
                | Self::PersistFailed(_)
                | Self::DeleteFailed(_)
                | Self::UndoFailed(_)
                | Self::TestStartFailed(_)
                | Self::ProfileLoadFailed(_)
                | Self::ProfileSaveFailed(_)
                | Self::HostListSaveFailed(_)
                | Self::CredentialSaveFailed(_)
                | Self::SavedWithCleanupError { .. }
                | Self::Error(_)
        )
    }

    pub(super) fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Saved(destination) => {
                format!("{} {destination}", language.pick("已保存", "Saved"))
            },
            Self::Pinned => language.pick("置顶状态已更新", "Pin status updated").into(),
            Self::Imported(count) => format!(
                "{} {count} {}",
                language.pick("已导入，config 源共", "Imported"),
                language.pick("个别名", "config aliases")
            ),
            Self::Opening(host) => {
                format!("{} {host}…", language.pick("正在打开", "Opening"))
            },
            Self::DeleteCommitted { hidden_config: true } => language
                .pick(
                    "已隐藏 config 别名，并清理 Pebrel Profile 与凭据",
                    "Config alias hidden; Pebrel profile and credentials removed",
                )
                .into(),
            Self::DeleteCommitted { hidden_config: false } => language
                .pick(
                    "已删除主机、Profile 与凭据",
                    "Host, profile, and credentials deleted",
                )
                .into(),
            Self::CleanupPartial(details) => format!(
                "{}: {details}",
                language.pick(
                    "主机已从列表移除，但部分清理失败",
                    "Host removed from the list, but some cleanup failed",
                )
            ),
            Self::Restored(host) => {
                format!("{} {host}", language.pick("已恢复", "Restored"))
            },
            Self::Validation(error) => error.text(language).into(),
            Self::PersistFailed(error) => format!(
                "{}: {error}",
                language.pick("写入设置失败", "Failed to write settings")
            ),
            Self::DeleteFailed(error) => format!(
                "{}: {error}",
                language.pick("删除主机失败", "Failed to delete host")
            ),
            Self::UndoFailed(error) => {
                format!("{}: {error}", language.pick("撤销失败", "Undo failed"))
            },
            Self::TestStartFailed(error) => format!(
                "{}: {error}",
                language.pick("无法启动连接测试", "Could not start the connection test")
            ),
            Self::ProfileLoadFailed(error) => format!(
                "{}: {error}",
                language.pick("加载 SSH Profile 失败", "Failed to load the SSH profile")
            ),
            Self::ProfileSaveFailed(error) => format!(
                "{}: {error}",
                language.pick("保存 SSH Profile 失败", "Failed to save the SSH profile")
            ),
            Self::HostListSaveFailed(error) => format!(
                "{}: {error}",
                language.pick("保存主机列表失败", "Failed to save the host list")
            ),
            Self::CredentialSaveFailed(error) => format!(
                "{}: {error}",
                language.pick(
                    "Profile 已保存，但密码写入凭据管理器失败",
                    "The profile was saved, but the password could not be written to the credential manager",
                )
            ),
            Self::SavedWithCleanupError { destination, error } => format!(
                "{} {destination}, {}: {error}",
                language.pick("已保存", "Saved"),
                language.pick(
                    "但旧地址凭据清理失败",
                    "but credentials for the previous address could not be removed",
                )
            ),
            Self::Error(error) => {
                format!("{}: {error}", language.pick("SSH 操作失败", "SSH operation failed"))
            },
        }
    }
}

use super::ssh_settings::{SshDeleteUndo, SshEditorState, SshValidationError};

impl ShellSelectItem {
    fn new(id: String, name: String, scale_factor: f32) -> Self {
        // Select 的闭态和菜单行尺寸不同。分别生成与物理像素一一对应的纹理，
        // 避免把 128px 原图交给 GPUI 在每帧缩小而产生模糊边缘。
        let closed_image = crate::gpui_shell::widgets::shell_brand_image(&id, 20.0, scale_factor);
        let row_image = crate::gpui_shell::widgets::shell_brand_image(&id, 24.0, scale_factor);
        Self { id, name: name.into(), closed_image, row_image }
    }

    /// 置顶的导入行。没有品牌贴图，[`Self::view`] 会给它文件夹图标。
    fn import_action(language: crate::display::UiLanguage) -> Self {
        Self {
            id: SHELL_IMPORT_ACTION_ID.to_owned(),
            name: language.pick("导入终端目录…", "Import terminal directory...").into(),
            closed_image: None,
            row_image: None,
        }
    }

    fn is_import_action(&self) -> bool {
        self.id == SHELL_IMPORT_ACTION_ID
    }

    fn view(&self, size: f32, image: Option<&Arc<RenderImage>>) -> gpui::AnyElement {
        let icon: gpui::AnyElement = if let Some(image) = image {
            gpui::StyledImage::object_fit(
                img(image.clone()).size(px(size)).flex_shrink_0(),
                gpui::ObjectFit::Contain,
            )
            .into_any_element()
        } else if self.is_import_action() {
            // 动作行与真实 shell 行必须一眼分得开：文件夹口 = 「去别处拿」。
            Icon::new(IconName::FolderOpen).xsmall().into_any_element()
        } else {
            Icon::new(IconName::SquareTerminal).xsmall().into_any_element()
        };
        h_flex()
            .gap_2()
            .items_center()
            .child(icon)
            .child(div().flex_1().min_w_0().child(self.name.clone()))
            .into_any_element()
    }
}

/// 默认 Shell 下拉的全部候选，以及应当选中的行号。
///
/// 顺序 = 置顶导入行 → 已安装 shell（`detect_shells` 菜单序）→ 用户导入的
/// 终端 profile。导入项以 `profile:<家族>|<id>` 作设置值（[`Profile::settings_id`]
/// 同一形状），品牌图标因此仍能按家族查到。
fn shell_select_items(
    current: &str,
    scale_factor: f32,
    language: crate::display::UiLanguage,
) -> (Vec<ShellSelectItem>, usize) {
    let mut items: Vec<ShellSelectItem> = crate::shell_detect::detect_shells()
        .into_iter()
        .map(|shell| ShellSelectItem::new(shell.id, shell.name, scale_factor))
        .collect();
    if items.is_empty() {
        // 非 Windows 构建不做安装探测，但历史配置仍支持这两个由 PTY
        // 集成层负责启动的稳定 id，设置页不能因此变成空下拉。
        items = vec![
            ShellSelectItem::new("powershell".into(), "PowerShell".into(), scale_factor),
            ShellSelectItem::new("bash".into(), "Git Bash".into(), scale_factor),
        ];
    }
    // 导入的终端目录：`merge_terminal_profiles` 已把它们并进配置的 profile
    // 列表，这里让设置页也能直接选为默认 Shell——否则导入完看不见结果。
    if let Ok(store) = crate::terminal_profiles::TerminalProfiles::load() {
        for profile in store.as_config_profiles() {
            let Some(id) = profile.settings_id() else { continue };
            if items.iter().any(|item| item.id == id) {
                continue;
            }
            items.push(ShellSelectItem::new(id, profile.name, scale_factor));
        }
    }
    if !items.iter().any(|item| item.id == current) {
        // 检测结果可能暂时找不到已保存的 WSL/profile id；先把它保留在首位，
        // 用户仍可看到并重新选择，下一次检测恢复后不会丢失持久化值。
        items.insert(
            0,
            ShellSelectItem::new(
                current.to_owned(),
                crate::shell_detect::display_name_for_id(current).to_owned(),
                scale_factor,
            ),
        );
    }
    let selected = items.iter().position(|item| item.id == current).unwrap_or(0);
    // 导入行最后才插到首位：它不参与选中判定，所以选中行号整体后移一位。
    items.insert(0, ShellSelectItem::import_action(language));
    (items, selected + 1)
}

/// 扫描目录并落盘（阻塞 IO，调用方须放后台执行器）。
/// 逻辑与旧壳 `Display::import_terminal_directory` 逐句对齐，只是把 toast
/// 换成 `Result`，由 UI 线程决定怎么呈现。
fn import_terminal_directory_blocking(
    directory: &std::path::Path,
) -> Result<usize, TerminalImportError> {
    let found = crate::terminal_profiles::scan_directory(directory)
        .map_err(|error| TerminalImportError::Scan(error.to_string()))?;
    if found.is_empty() {
        return Err(TerminalImportError::NoSupportedTerminal);
    }
    let mut profiles = crate::terminal_profiles::TerminalProfiles::load()
        .map_err(|error| TerminalImportError::Load(error.to_string()))?;
    let count = found.len();
    for profile in found {
        profiles.upsert(profile).map_err(|error| TerminalImportError::Import(error.to_string()))?;
    }
    profiles.save().map_err(|error| TerminalImportError::Save(error.to_string()))?;
    Ok(count)
}

/// 原生模态对话框不能在 GPUI update 借用中运行：它自己的消息泵会重入
/// wndproc，造成 AppCell 二次可变借用。与 SSH 私钥选择器一样，先在 UI
/// 线程捕获 HWND，再让专用线程运行旧壳的 IFileOpenDialog。
#[cfg(windows)]
fn pick_folder_with_wsl_places(
    window: &Window,
    title: &'static str,
) -> futures::channel::oneshot::Receiver<Option<std::path::PathBuf>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let owner = HasWindowHandle::window_handle(window)
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
            _ => None,
        })
        .unwrap_or(0);
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let selected = crate::display::file_dialog::pick_folder_with_hwnd(owner as _, title);
        let _ = tx.send(selected);
    });
    rx
}

impl SelectItem for ShellSelectItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        // 闭态留出 chevron 与上下内边距；20px 在 32px Select 内不会挤字。
        Some(self.view(20.0, self.closed_image.as_ref()))
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // 旧壳 ShellPickerRow 的品牌图标是 24×24 逻辑像素。
        self.view(24.0, self.row_image.as_ref())
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(&query.to_lowercase())
            || self.id.to_lowercase().contains(&query.to_lowercase())
    }
}

pub struct SettingsPane {
    pub(super) focus_handle: FocusHandle,
    /// 渲染与写盘的单一事实源；每次 persist 后整体重载。
    pub(super) runtime: RuntimeSettings,
    /// 当前分区（`SECTIONS` 下标）；默认落在应用主页。
    active_section: usize,
    appearance_picker: Option<appearance_picker::AppearancePicker>,
    theme_picker_trigger: FocusHandle,
    icon_picker_trigger: FocusHandle,
    typography_expanded: bool,
    appearance_advanced_expanded: bool,
    about_update: AboutUpdateState,
    about_update_seq: u64,
    about_last_checked: Option<String>,
    settings_search_input: Entity<InputState>,
    settings_search_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    /// 每项还带着自己的 `values` 表：`SelectState` 只认索引，而从代码侧
    /// 改设置（还原默认值、命令面板切换）时手里只有配置文件记号，没有
    /// 这张表就没法把闭框的选中项拉回去。
    selects: Vec<(&'static str, SharedSelect, &'static [&'static str])>,
    shell_select: SharedShellSelect,
    /// 外观页背景色：闭态 combobox + 开态 SV/色相/色板/hex 浮层（旧壳
    /// `SettingsDropdown::BackgroundColor`，不是组件库 ColorPicker）。
    bg_picker_open: bool,
    bg_picker_hsv: (f32, f32, f32),
    bg_picker_drag: Option<crate::display::BgPickerPart>,
    bg_hex_input: Entity<InputState>,
    bg_hex_focused: bool,
    bg_hex_syncing: bool,
    bg_picker_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    bg_sv_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    bg_hue_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    opacity_slider: Entity<SliderState>,
    wallpaper_opacity_slider: Entity<SliderState>,
    pub(super) proxy_url_input: Entity<InputState>,
    pub(super) proxy_protocol_select: SharedSelect,
    pub(super) proxy_test_seq: u64,
    pub(super) proxy_test_status: crate::display::ProxyTestStatus,
    provider_store: crate::ai_providers::ProviderStore,
    /// Name / note / website / endpoint / model. API keys deliberately do not
    /// use a GPUI text widget; the native credential dialog is write-only.
    provider_inputs: Vec<Entity<InputState>>,
    provider_status: Option<ProviderStatus>,
    provider_test_seq: u64,
    provider_test_running: bool,
    provider_codex_confirm: Option<String>,
    /// SSH 主机列表（共享三键 + merge 权威）；操作后整体重载防漂移。
    /// SSH 区的行为实现拆在 `ssh_settings.rs`（同类型第二个 impl 块）。
    pub(super) ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists,
    /// SSH 编辑器的文本字段常驻，以便所有文本改动都能使进行中的测试失效。
    pub(super) ssh_username_input: Entity<InputState>,
    pub(super) ssh_destination_input: Entity<InputState>,
    pub(super) ssh_port_input: Entity<InputState>,
    pub(super) ssh_label_input: Entity<InputState>,
    pub(super) ssh_password_input: Entity<InputState>,
    pub(super) ssh_proxy_host_input: Entity<InputState>,
    pub(super) ssh_proxy_port_input: Entity<InputState>,
    pub(super) ssh_proxy_username_input: Entity<InputState>,
    pub(super) ssh_proxy_password_input: Entity<InputState>,
    pub(super) ssh_jump_host_input: Entity<InputState>,
    /// 用户名输入框本身可自由编辑；候选层只提供最近使用值，不把输入约束成
    /// 固定枚举。锚点跟随输入框，滚动/DPI 变化后仍贴在其下方。
    pub(super) ssh_username_picker_open: bool,
    pub(super) ssh_username_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    /// 身份条头像的图标选择器（旧壳 `SshEditorHit::Avatar` + `icon_popup`）：
    /// 点头像展开，顶部一个正经搜索框，下面是分组过的目录。不做成右上角
    /// 的下拉框——图标属于头像那件事，摆成独立字段就成了可填可不填的杂项。
    pub(super) ssh_icon_picker_open: bool,
    pub(super) ssh_icon_filter_input: Entity<InputState>,
    /// 头像上一帧的窗口坐标，供弹层锚定（同字体目录的做法）。
    pub(super) ssh_icon_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    pub(super) ssh_editor: Option<SshEditorState>,
    pub(super) ssh_editor_focus_handle: FocusHandle,
    pub(super) ssh_editor_seq: u64,
    pub(super) ssh_test_seq: u64,
    pub(super) ssh_status: Option<SshStatus>,
    pub(super) ssh_show_hidden: bool,
    /// 删除确认（二次点击生效，旧壳确认对话框的轻量对应）。
    pub(super) ssh_delete_confirm: Option<String>,
    /// 未决删除的撤销窗口（8 秒；见 `ssh_settings::SshDeleteUndo`）。
    pub(super) ssh_delete_undo: Option<SshDeleteUndo>,
    pub(super) ssh_undo_seq: u64,
    /// 可直接编辑的字体链及其建议弹层；逗号分隔主字体与 fallback 字体。
    pub(super) font_picker_open: bool,
    font_loading: bool,
    /// None = 尚未枚举；首次展开时在后台线程装配（几百字体的机器上
    /// `IsMonospacedFont` 逐族探询是实打实的开销，不挡 UI 帧）。
    font_system: Option<Vec<crate::font_install::SystemFontFamily>>,
    /// GPUI text system 已注册的导入族名（启动扫描 + 本次导入累计）。
    font_imported: Vec<String>,
    font_family_input: Entity<InputState>,
    /// 字体输入框上一帧的窗口坐标。字体目录是宽弹层，不能把整条设置行当
    /// 锚点；否则输入框在右侧、菜单却会从正文左缘展开。
    font_picker_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    /// 备份类别选择（本地 UI 态；出厂默认 = 共享 `BackupSelection::default`）。
    backup_selection: crate::encrypted_backup::BackupSelection,
    /// 备份密码（masked；只在导出/恢复动作瞬时读取，不落任何配置）。
    backup_pass_input: Entity<InputState>,
    backup_status: Option<BackupStatus>,
    /// 导出/恢复/推送进行中（按钮禁用 + 忽略过期完成回调）。
    backup_busy: bool,
    backup_seq: u64,
    /// 远端同步配置（`nebula_backup.txt` 共享权威）。
    backup_remote: crate::backup_remote::BackupRemoteConfig,
    /// 非密文槽位输入（容量按最多槽位的协议 S3 = 4）。
    backup_remote_inputs: Vec<Entity<InputState>>,
    /// 密文槽（WebDAV 密码 / S3 Secret）：masked，写入凭据管理器后即清。
    backup_secret_input: Entity<InputState>,
    /// 按键映射编辑器（旧壳 spec 002 的 GPUI 形态）：搜索输入 + 捕获态 +
    /// `keybind=` 行的工作镜像。模型层（combo 解析/展示/冲突/默认表）复用
    /// `display::keymap`，两壳同一套存储与语义。
    keymap_search_input: Entity<InputState>,
    keymap_capture: Option<usize>,
    keymap_capture_preview: String,
    keymap_binds: Vec<(String, String)>,
    /// 不透明度滑块的落盘 debounce。滑块只有 `Change` 事件（没有"松手"），
    /// 而 [`Self::persist`] 是重操作：写盘 + 三次设置重载 + 每个终端
    /// `apply_settings`（含四次字体构造）+ 主题全量重建 + 壁纸重烘焙。拖拽期间
    /// 每帧走一遍就会卡死，所以拖拽只更新内存视效，落盘挪到停手之后。
    /// 覆盖这个字段即取消上一个待落盘任务（旧 `Task` drop = 取消）。
    slider_persist: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = RuntimeSettings::load();
        let language = crate::gpui_shell::config::ui_language(cx);
        let mut selects: Vec<(&'static str, SharedSelect, &'static [&'static str])> = Vec::new();
        let mut subscriptions = Vec::new();

        let mut add_select = |key: &'static str,
                              values: &'static [&'static str],
                              current: &str,
                              window: &mut Window,
                              cx: &mut Context<Self>| {
            let ix = values.iter().position(|v| *v == current).unwrap_or(0);
            let select = cx.new(|cx| {
                SelectState::new(
                    localized_select_labels(key, values, language),
                    Some(IndexPath::default().row(ix)),
                    window,
                    cx,
                )
            });
            subscriptions.push(cx.subscribe_in(
                &select,
                window,
                move |this: &mut Self,
                      entity: &SharedSelect,
                      event: &SelectEvent<Vec<SharedString>>,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
                    if let SelectEvent::Confirm(Some(_)) = event {
                        let row = entity.read(cx).selected_index(cx).map(|path| path.row);
                        if let Some(value) = row.and_then(|row| values.get(row)) {
                            this.persist(&[(key, (*value).to_string())], cx);
                            if key == "language" {
                                this.refresh_localized_controls(window, cx);
                                cx.refresh_windows();
                            }
                        }
                    }
                },
            ));
            selects.push((key, select, values));
        };

        let cursor_current =
            runtime.cursor_shape.map(|shape| shape.settings_value()).unwrap_or("beam");
        let shell_current = runtime.shell.clone().unwrap_or_else(|| "powershell".into());

        add_select(
            "language",
            &["system", "zh-CN", "en-US"],
            runtime.language.settings_value(),
            window,
            cx,
        );
        add_select("theme", &THEME_VALUES, runtime.theme.prompt_name(), window, cx);
        // 选项顺序与文案照抄旧壳 `CURSOR_SHAPE_OPTIONS` / `cursor_shape_label`。
        add_select(
            "cursor_shape",
            &["beam", "underline", "block", "hollow"],
            cursor_current,
            window,
            cx,
        );
        add_select(
            "tabs_position",
            &["sidebar", "top"],
            runtime.tabs_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "tab_reveal",
            &["slide", "instant"],
            runtime.tab_reveal.settings_value(),
            window,
            cx,
        );
        add_select(
            "density",
            &["standard", "compact"],
            runtime.density.settings_value(),
            window,
            cx,
        );
        add_select(
            "new_tab_position",
            &["after_current", "end"],
            runtime.new_tab_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "windowing_behavior",
            &["use_new", "use_any_existing", "use_existing"],
            runtime.windowing_behavior.settings_value(),
            window,
            cx,
        );
        add_select(
            "vcs_display",
            &["auto", "git", "svn"],
            runtime.vcs_display.settings_value(),
            window,
            cx,
        );
        add_select(
            "cell_width_mode",
            &["compact", "relaxed"],
            runtime.cell_width_mode.settings_value(),
            window,
            cx,
        );
        // 文案照抄旧壳 `accept_label` / `completion_style_label`。
        add_select(
            "bell",
            &["none", "visual", "audible", "both"],
            runtime.bell.settings_value(),
            window,
            cx,
        );
        // 五档按 DWM 每帧成本排列，不是质量递进；用户按性能预算选择。
        // Aero 是实时玻璃，Acrylic 是实时材质模糊；两者都会采样窗口后方真实内容。
        // Mica / Mica Alt 使用系统壁纸 backdrop，不由 Nebula 读取或重采样。
        add_select(
            "blur",
            &["none", "mica", "mica-alt", "aero", "acrylic"],
            runtime.blur.settings_value(),
            window,
            cx,
        );
        add_select(
            "accept",
            &["right", "tab", "both"],
            runtime.accept.settings_value(),
            window,
            cx,
        );
        add_select(
            "completion_style",
            &["inline", "popup"],
            runtime.completion_style.settings_value(),
            window,
            cx,
        );
        // 壁纸 fit/对齐：存原文，经旧壳 renderer::image 的 parse 归一化
        // （兼容 cover/contain 等别名），展示用规范记号。
        let bgimg_fit = crate::renderer::image::BackgroundImageFit::parse(
            runtime.background_image_fit.as_deref().unwrap_or(""),
        )
        .unwrap_or_default()
        .settings_value();
        // 顺序与文案照抄旧壳 `BACKGROUND_FIT_OPTIONS` / `background_image_fit_label`。
        add_select(
            "background_image_fit",
            &["fill", "uniform", "uniform_to_fill", "none"],
            bgimg_fit,
            window,
            cx,
        );
        let bgimg_align = crate::renderer::image::BackgroundImageAlignment::parse(
            runtime.background_image_alignment.as_deref().unwrap_or(""),
        )
        .unwrap_or_default()
        .settings_value();
        // 九宫格顺序照抄旧壳 `BACKGROUND_ALIGNMENT_OPTIONS`（左上 → 右下）。
        add_select(
            "background_image_alignment",
            &[
                "top_left",
                "top",
                "top_right",
                "left",
                "center",
                "right",
                "bottom_left",
                "bottom",
                "bottom_right",
            ],
            bgimg_align,
            window,
            cx,
        );
        add_select(
            "ssh_proxy_mode",
            &["off", "system", "custom"],
            runtime.ssh_proxy_mode.settings_value(),
            window,
            cx,
        );

        // 与旧壳的默认 Shell 菜单共用检测层：不能在设置页另维护一份两项
        // 白名单，否则 CMD/Nushell/WSL 会出现在新建终端菜单，却无法设为默认。
        // 选项 = 彩色品牌 PNG（extra/shell-icons，与旧壳设置页/命令面板同
        // 一批资产）+ 名称，闭态与下拉同源（SelectItem::display_title/render）。
        let shell_icon_scale = window.scale_factor().max(0.5);
        let (shell_items, shell_index) =
            shell_select_items(&shell_current, shell_icon_scale, language);
        let shell_select = cx.new(|cx| {
            SelectState::new(shell_items, Some(IndexPath::default().row(shell_index)), window, cx)
        });
        subscriptions.push(cx.subscribe_in(
            &shell_select,
            window,
            move |this: &mut Self,
                  _entity: &SharedShellSelect,
                  event: &SelectEvent<Vec<ShellSelectItem>>,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    // 置顶那行是动作不是选项：走导入流程，且不落盘。
                    if id == SHELL_IMPORT_ACTION_ID {
                        this.import_terminal_directory(window, cx);
                    } else {
                        this.persist(&[("shell", id.clone())], cx);
                    }
                }
            },
        ));

        let bg_hex_input = {
            let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
            let rgb = runtime.background.unwrap_or([term.r, term.g, term.b]);
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("#rrggbb")
                    .default_value(format_hex_rgb(rgb))
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 window: &mut Window,
                 cx: &mut Context<Self>| {
                    this.on_bg_hex_event(event, window, cx);
                },
            ));
            input
        };
        let opacity_slider = cx.new(|_| {
            SliderState::new().min(0.00).max(1.00).step(0.05).default_value(runtime.opacity)
        });
        subscriptions.push(cx.subscribe(&opacity_slider, |this, _, event: &SliderEvent, cx| {
            if let SliderEvent::Change(value) = event {
                this.set_opacity(value.start(), cx);
            }
        }));
        let wallpaper_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.05)
                .max(1.00)
                .step(0.05)
                .default_value(runtime.background_image_opacity)
        });
        subscriptions.push(cx.subscribe(
            &wallpaper_opacity_slider,
            |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.set_wallpaper_opacity(value.start(), cx);
                }
            },
        ));
        let (proxy_protocol, proxy_address) =
            crate::display::manual_proxy_parts(&runtime.ssh_proxy_url);
        let proxy_protocol_ix = crate::display::MANUAL_PROXY_PROTOCOL_OPTIONS
            .iter()
            .position(|item| *item == proxy_protocol)
            .unwrap_or(0);
        let proxy_protocol_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from("SOCKS5"), SharedString::from("HTTP")],
                Some(IndexPath::default().row(proxy_protocol_ix)),
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe_in(
            &proxy_protocol_select,
            window,
            |this: &mut Self,
             _,
             event: &SelectEvent<Vec<SharedString>>,
             _,
             cx: &mut Context<Self>| {
                if matches!(event, SelectEvent::Confirm(Some(_))) {
                    this.commit_proxy_address(cx);
                }
            },
        ));
        let proxy_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("127.0.0.1:7890")
                .pattern(regex::Regex::new(r"^[^\s]{0,256}$").expect("static regex"))
                .default_value(proxy_address.to_owned())
        });
        subscriptions.push(cx.subscribe_in(
            &proxy_url_input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx: &mut Context<Self>| {
                this.on_proxy_address_event(event, cx);
            },
        ));
        let provider_store = crate::ai_providers::load();
        let active_provider = provider_store
            .providers
            .iter()
            .find(|provider| provider.id == provider_store.active_id)
            .or_else(|| provider_store.providers.first())
            .cloned()
            .unwrap_or_else(|| {
                crate::ai_providers::AiProvider::preset(
                    crate::ai_providers::ProviderKind::Custom,
                    "custom-1",
                )
            });
        let provider_values = [
            active_provider.name,
            active_provider.note,
            active_provider.website_url,
            active_provider.base_url,
            active_provider.model,
        ];
        let provider_placeholders = provider_input_placeholders(language);
        let provider_inputs = provider_values
            .into_iter()
            .zip(provider_placeholders)
            .map(|(value, placeholder)| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder(placeholder).default_value(value)
                })
            })
            .collect();

        // 远端备份的非密文槽位输入（容量按最多槽位的协议 S3 = 4）；值在
        // 构造体内按当前协议回填。
        let backup_remote_inputs: Vec<Entity<InputState>> =
            (0..4).map(|_| cx.new(|cx| InputState::new(window, cx))).collect();

        let ssh_username_input = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        // 地址框现在只承担 host/IP；仍兼容粘贴整段 user@host，由共享 helper
        // 在保存/测试时拆出内嵌用户名。
        let ssh_destination_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("example.com / 192.0.2.1"));
        // 端口键入即过滤：至多 5 位数字（旧壳同规则；范围校验在保存/测试时做）。
        let ssh_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("22")
                .pattern(regex::Regex::new(r"^\d{0,5}$").expect("static regex"))
        });
        let ssh_label_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_label", language))
        });
        let ssh_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(localized_input_placeholder("ssh_password", language))
        });
        let ssh_icon_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_icon_filter", language))
        });
        let ssh_proxy_host_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("127.0.0.1"));
        let ssh_proxy_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("1080")
                .pattern(regex::Regex::new(r"^\d{0,5}$").expect("static regex"))
        });
        let ssh_proxy_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_proxy_username", language))
        });
        let ssh_proxy_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(localized_input_placeholder("ssh_proxy_password", language))
        });
        let ssh_jump_host_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_jump_host", language))
        });
        for input in [
            ssh_username_input.clone(),
            ssh_destination_input.clone(),
            ssh_port_input.clone(),
            ssh_label_input.clone(),
            ssh_password_input.clone(),
            ssh_proxy_host_input.clone(),
            ssh_proxy_port_input.clone(),
            ssh_proxy_username_input.clone(),
            ssh_proxy_password_input.clone(),
            ssh_jump_host_input.clone(),
        ] {
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx: &mut Context<Self>| {
                    if matches!(event, InputEvent::Change) {
                        this.touch_ssh_editor(cx);
                    }
                },
            ));
        }
        let font_family_input =
            Self::new_font_family_input(runtime.font_family.clone(), window, cx);
        subscriptions.push(cx.subscribe_in(
            &font_family_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_font_family_input_event(event, window, cx);
            },
        ));

        let bg_picker_hsv = {
            let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
            let rgb = runtime.background.unwrap_or([term.r, term.g, term.b]);
            crate::display::rgb_to_hsv(crate::display::color::Rgb::new(rgb[0], rgb[1], rgb[2]))
        };

        // GPUI 会先匹配 KeyBinding action，再派发元素的 capture/bubble KeyDown。
        // 因此录制 PageDown 等已有快捷键必须在应用级 interceptor 提前截住；
        // 焦点检查保证后台设置标签或普通设置输入不受影响。
        let keymap_interceptor = cx.listener(|this, event: &gpui::KeystrokeEvent, window, cx| {
            if this.keymap_capture.is_some() && this.focus_handle.contains_focused(window, cx) {
                cx.stop_propagation();
                this.handle_keymap_capture(&event.keystroke, cx);
            }
        });
        subscriptions.push(cx.intercept_keystrokes(keymap_interceptor));
        let appearance_interceptor = cx.listener(Self::intercept_appearance_picker);
        subscriptions.push(cx.intercept_keystrokes(appearance_interceptor));

        let settings_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(language.pick(
                "搜索全部设置，例如「字号」「透明度」「更新」",
                "Search all settings, e.g. font, opacity, update",
            ))
        });
        subscriptions.push(cx.subscribe_in(
            &settings_search_input,
            window,
            |_this: &mut Self,
             _: &Entity<InputState>,
             event: &InputEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                if matches!(event, InputEvent::Change | InputEvent::Focus | InputEvent::Blur) {
                    cx.notify();
                }
            },
        ));

        Self {
            focus_handle: cx.focus_handle(),
            runtime,
            active_section: 0,
            appearance_picker: None,
            theme_picker_trigger: cx.focus_handle(),
            icon_picker_trigger: cx.focus_handle(),
            typography_expanded: false,
            appearance_advanced_expanded: false,
            about_update: AboutUpdateState::Idle,
            about_update_seq: 0,
            about_last_checked: None,
            settings_search_input,
            settings_search_trigger_bounds: None,
            selects,
            shell_select,
            bg_picker_open: false,
            bg_picker_hsv,
            bg_picker_drag: None,
            bg_hex_input,
            bg_hex_focused: false,
            bg_hex_syncing: false,
            bg_picker_trigger_bounds: None,
            bg_sv_bounds: None,
            bg_hue_bounds: None,
            opacity_slider,
            wallpaper_opacity_slider,
            proxy_url_input,
            proxy_protocol_select,
            proxy_test_seq: 0,
            proxy_test_status: crate::display::ProxyTestStatus::Idle,
            provider_store,
            provider_inputs,
            provider_status: None,
            provider_test_seq: 0,
            provider_test_running: false,
            provider_codex_confirm: None,
            ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists::load(),
            ssh_username_input,
            ssh_destination_input,
            ssh_port_input,
            ssh_label_input,
            ssh_password_input,
            ssh_proxy_host_input,
            ssh_proxy_port_input,
            ssh_proxy_username_input,
            ssh_proxy_password_input,
            ssh_jump_host_input,
            ssh_username_picker_open: false,
            ssh_username_trigger_bounds: None,
            ssh_icon_picker_open: false,
            ssh_icon_filter_input,
            ssh_icon_trigger_bounds: None,
            ssh_editor: None,
            ssh_editor_focus_handle: cx.focus_handle(),
            ssh_editor_seq: 0,
            ssh_test_seq: 0,
            ssh_status: None,
            ssh_show_hidden: false,
            ssh_delete_confirm: None,
            ssh_delete_undo: None,
            ssh_undo_seq: 0,
            font_picker_open: false,
            font_loading: false,
            font_system: None,
            font_imported: Vec::new(),
            font_family_input,
            font_picker_trigger_bounds: None,
            backup_selection: crate::encrypted_backup::BackupSelection::default(),
            backup_pass_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(localized_input_placeholder("backup_password", language))
            }),
            backup_status: None,
            backup_busy: false,
            backup_seq: 0,
            backup_remote: {
                let cfg = crate::backup_remote::BackupRemoteConfig::load();
                for (ix, input) in backup_remote_inputs.iter().enumerate() {
                    let value = cfg.slot(ix).unwrap_or_default().to_owned();
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                }
                cfg
            },
            backup_remote_inputs,
            backup_secret_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(localized_input_placeholder("backup_secret", language))
            }),
            keymap_search_input: {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(localized_input_placeholder("keymap_search", language))
                });
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    |_this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     _: &mut Window,
                     cx: &mut Context<Self>| {
                        // 搜索词变化只影响可见行集合；捕获态不因打字被打断
                        // （捕获期间焦点在分区根上，输入框收不到键）。
                        if matches!(event, InputEvent::Change) {
                            cx.notify();
                        }
                    },
                ));
                input
            },
            keymap_capture: None,
            keymap_capture_preview: String::new(),
            keymap_binds: nebula_settings::keybind_pairs(),
            slider_persist: None,
            _subscriptions: subscriptions,
        }
    }

    /// 写盘 → 重载单一事实源与全局 `Settings` → 通知宿主热应用。
    pub(super) fn persist(&mut self, updates: &[(&str, String)], cx: &mut Context<Self>) {
        if let Err(err) = self.try_persist(updates, cx) {
            super::try_write_stderr(format_args!(
                "[nebula:gpui] failed to persist settings: {err}"
            ));
        }
    }

    fn try_persist(
        &mut self,
        updates: &[(&str, String)],
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        persist_keys(updates)?;
        self.runtime = RuntimeSettings::load();
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        gpui_component::set_locale(settings.ui_language.gpui_component_locale());
        cx.set_global(settings);
        if updates.iter().any(|(key, _)| matches!(*key, "ssh_proxy_mode" | "ssh_proxy_url")) {
            self.invalidate_proxy_test();
        }
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
        Ok(())
    }

    /// 语言切换不重建输入/下拉实体：重建会丢焦点、编辑值、undo 和订阅。
    /// 固定候选按稳定 value 保留索引，随后仅替换显示项与 placeholder。
    fn refresh_localized_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let language = crate::gpui_shell::config::ui_language(cx);
        for (key, select, values) in &self.selects {
            let selected = select.read(cx).selected_index(cx);
            let items = localized_select_labels(key, values, language);
            select.update(cx, |state, cx| {
                state.set_items(items, window, cx);
                state.set_selected_index(selected, window, cx);
            });
        }
        self.refresh_shell_items(window, cx);

        for (input, placeholder) in
            self.provider_inputs.iter().zip(provider_input_placeholders(language))
        {
            input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        }
        for (input, key) in [
            (&self.ssh_label_input, "ssh_label"),
            (&self.ssh_password_input, "ssh_password"),
            (&self.ssh_proxy_username_input, "ssh_proxy_username"),
            (&self.ssh_proxy_password_input, "ssh_proxy_password"),
            (&self.ssh_jump_host_input, "ssh_jump_host"),
            (&self.ssh_icon_filter_input, "ssh_icon_filter"),
            (&self.font_family_input, "font_family"),
            (&self.backup_pass_input, "backup_password"),
            (&self.backup_secret_input, "backup_secret"),
            (&self.keymap_search_input, "keymap_search"),
        ] {
            let placeholder = localized_input_placeholder(key, language);
            input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        }
        self.settings_search_input.update(cx, |state, cx| {
            state.set_placeholder(
                language.pick(
                    "搜索全部设置，例如「字号」「透明度」「更新」",
                    "Search all settings, e.g. font, opacity, update",
                ),
                window,
                cx,
            )
        });
        cx.notify();
    }

    fn toggle(
        &mut self,
        key: &'static str,
        value: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key == "follow_system_theme" {
            self.set_follow_system_theme(value, window, cx);
            return;
        }
        if key == "background_image_cover_chrome" {
            self.request_cover_chrome(value, window, cx);
            return;
        }
        self.persist(&[(key, (value as u8).to_string())], cx);
    }

    /// 旧壳 `request_toggle_background_image_cover_chrome`：关→开要确认
    /// （壳变半透明、控件对比度下降）；开→关直接生效。
    fn request_cover_chrome(&mut self, enable: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !enable {
            self.persist(&[("background_image_cover_chrome", "0".to_owned())], cx);
            return;
        }
        if self.runtime.background_image_cover_chrome {
            return;
        }
        let language = crate::gpui_shell::config::ui_language(cx);
        let pane = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let pane = pane.clone();
            confirm_dialog(
                dialog,
                window,
                language.tr("settings.appearance.background_cover_title"),
                SharedString::from(language.tr("settings.appearance.background_cover_description")),
                language.pick("开启", "Enable"),
                language.pick("取消", "Cancel"),
                ButtonVariant::Primary,
            )
            .on_ok(move |_, _, cx| {
                let _ = pane.update(cx, |this, cx| {
                    this.persist(&[("background_image_cover_chrome", "1".to_owned())], cx);
                });
                true
            })
        });
        cx.notify();
    }

    /// 对齐旧壳 `toggle_system_theme_following`：开关会 `apply_nebula_theme`，
    /// 把终端底色写成**此刻生效**主题的 `term_bg`。开启时按系统外观折算家族
    /// 成员；关闭时回到用户点选的 preference，不再继续跟 OS。
    fn set_follow_system_theme(
        &mut self,
        follow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let applied = crate::gpui_shell::theme::resolve_theme_name(
            self.runtime.theme,
            follow,
            crate::gpui_shell::theme::system_is_light(cx),
        );
        self.persist(
            &[
                ("follow_system_theme", (follow as u8).to_string()),
                ("background", format_hex_rgb(applied.term_theme().background)),
            ],
            cx,
        );
        self.sync_background_color_picker(window, cx);
    }

    /// 旧壳“恢复默认设置”只重置外观，不触碰 Shell、SSH、快捷键等业务配置。
    fn reset_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_font_picker(window, false, cx);
        self.persist(
            &[
                ("theme", "Nebula".to_owned()),
                ("app_icon", nebula_settings::AppIconName::default().settings_value().to_owned()),
                ("font_family", String::new()),
                ("font_size", String::new()),
                ("follow_system_theme", "0".to_owned()),
                ("opacity", "1".to_owned()),
                ("background", String::new()),
                ("background_image", String::new()),
                ("background_image_opacity", "0.38".to_owned()),
                ("background_image_fit", String::new()),
                ("background_image_alignment", String::new()),
                ("background_image_cover_chrome", "0".to_owned()),
            ],
            cx,
        );
        self.typography_expanded = false;
        let family = self.current_font_chain(cx);
        self.font_family_input.update(cx, |input, cx| input.set_value(family, window, cx));
        self.sync_background_color_picker(window, cx);
    }

    /// Workspace tab、设置导航和设置内容共用的主文字字号事实源。
    ///
    /// 旧壳合同（display/mod.rs `ui_font_px`）：chrome 排版锚定**配置字号**
    /// （nebula.toml `font.size`，默认 11.25pt = 15px），终端的持久化缩放
    /// （设置 spinner / Ctrl+滚轮写入的 `font_size=`）只影响终端网格，
    /// 不得放大侧栏与设置文字。
    fn font_size_px(&self, cx: &App) -> f32 {
        cx.global::<crate::gpui_shell::config::Settings>().base_font_size_px
    }

    /// 终端字号（预览与「终端字号」步进行显示的值）。
    fn terminal_font_size_px(&self, cx: &App) -> f32 {
        cx.global::<crate::gpui_shell::config::Settings>().font_size_px
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = size.clamp(4.0, 96.0);
        self.persist(&[("font_size", format!("{size:.2}"))], cx);
    }

    /// 拖拽期间只做"改 alpha + 重绘"这两件必须的事，落盘与整套热应用
    /// 交给 [`Self::schedule_slider_persist`]。旧壳拖 opacity 就是改内存 + 重绘，
    /// 这条快路径是为了对回那个手感。
    fn set_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let opacity = opacity.clamp(0.0, 1.0);
        if (self.runtime.opacity - opacity).abs() < 1e-4 {
            return;
        }
        self.runtime.opacity = opacity;
        crate::gpui_shell::wallpaper::set_opacity_live(opacity, cx);
        crate::gpui_shell::theme::reapply_shell_opacity(
            self.runtime.theme,
            self.runtime.follow_system_theme,
            cx,
        );
        // 壳色是全局 token：只 `notify` 设置页不会让终端窗口的背景重绘。
        // `refresh_windows` 只推一个 effect，同一 update cycle 内多次调用会合并
        // 成一次重绘，所以每帧调是安全的。
        cx.refresh_windows();
        cx.notify();
        // 用户拖动后 opacity 已是显式选择；同时把旧 blur=1 规范成枚举值，
        // 否则共享迁移层会再次把 blur=1 + opacity=1.00 解读成默认 0.82。
        self.schedule_slider_persist(
            vec![
                ("opacity", format!("{opacity:.2}")),
                ("blur", self.runtime.blur.settings_value().to_owned()),
            ],
            cx,
        );
    }

    /// 背景图透明度是**烘焙进纹理**的（decode + 长边压到 2560 + 逐像素乘
    /// alpha），没有便宜的实时路径：每帧重烘焙一次会直接卡死。所以拖拽期间
    /// 只让滑块跟手，图面在停手后随一次落盘统一重建。
    fn set_wallpaper_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let opacity = opacity.clamp(0.05, 1.0);
        if (self.runtime.background_image_opacity - opacity).abs() < 1e-4 {
            return;
        }
        self.runtime.background_image_opacity = opacity;
        cx.notify();
        self.schedule_slider_persist(
            vec![("background_image_opacity", format!("{opacity:.2}"))],
            cx,
        );
    }

    /// `Change` 同时覆盖鼠标拖动和键盘调整，因此统一用 debounce 延后落盘：
    /// 每个新事件覆盖上一个待落盘任务，旧 `Task` drop 即取消。
    ///
    /// 落盘那一次会走完整的 [`Self::persist`]（写盘 + 设置重载 + 每终端
    /// `apply_settings` + 主题重建 + 壁纸重烘焙），跑一次没问题；每帧跑就是
    /// 2026-08-21 报的"拖拽卡顿的要死"。
    fn schedule_slider_persist(
        &mut self,
        updates: Vec<(&'static str, String)>,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        self.slider_persist = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(180)).await;
            let _ = this.update(cx, |this, cx| {
                this.persist(&updates, cx);
            });
        }));
    }

    /// 「导入终端目录…」：选目录 → 后台扫描落盘 → 刷新下拉。
    ///
    /// 对齐旧壳 `Display::import_terminal_directory`。扫描要遍历目录并读每个
    /// 候选 exe 的 PE 头判架构，放 UI 线程会卡住整窗，因此下沉到后台执行器。
    fn import_terminal_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 用户点的是动作行，不是换 shell：先把闭态标题拨回真正生效的那项，
        // 否则扫描期间下拉会显示「导入终端目录…」，像是默认 shell 被改掉了。
        self.restore_shell_selection(window, cx);
        let language = crate::gpui_shell::config::ui_language(cx);

        #[cfg(windows)]
        let picked = pick_folder_with_wsl_places(
            window,
            language.pick("选择终端安装目录", "Select a terminal installation directory"),
        );
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                language
                    .pick("选择终端安装目录", "Select a terminal installation directory")
                    .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            #[cfg(windows)]
            let Ok(Some(directory)) = picked.await else { return };
            #[cfg(not(windows))]
            let directory = {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                let Some(directory) = paths.into_iter().next() else { return };
                directory
            };
            let outcome = cx
                .background_spawn(async move { import_terminal_directory_blocking(&directory) })
                .await;
            let _ = this.update_in(cx, |pane, window, cx| {
                pane.finish_terminal_import(outcome, window, cx);
            });
        })
        .detach();
    }

    /// 导入收尾：提示结果，成功则重建候选并广播配置变更。
    fn finish_terminal_import(
        &mut self,
        outcome: Result<usize, TerminalImportError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(count) => {
                let language = crate::gpui_shell::config::ui_language(cx);
                let message = format!(
                    "{} {count} {}",
                    language.pick("已导入", "Imported"),
                    language.pick("个终端，立即可用", "terminals; they are ready to use")
                );
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Success,
                    message,
                );
                self.refresh_shell_items(window, cx);
                // 新 profile 要立即进入 Tab 的 Shell 面板；它不是普通运行时
                // 设置，避免因此重建所有终端字体与主题。
                cx.emit(SettingsPaneEvent::TerminalProfilesChanged);
                cx.notify();
            },
            Err(error) => {
                let message = error.text(crate::gpui_shell::config::ui_language(cx));
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Warning,
                    message,
                );
            },
        }
    }

    /// 按当前设置值重建下拉候选（导入后新增行即时可见）。
    fn refresh_shell_items(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.runtime.shell.clone().unwrap_or_else(|| "powershell".into());
        let (items, selected) = shell_select_items(
            &current,
            window.scale_factor().max(0.5),
            crate::gpui_shell::config::ui_language(cx),
        );
        self.shell_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_index(Some(IndexPath::default().row(selected)), window, cx);
        });
    }

    /// 把下拉选中项拨回设置里真正生效的 shell。
    fn restore_shell_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.runtime.shell.clone().unwrap_or_else(|| "powershell".into());
        self.shell_select.update(cx, |state, cx| {
            state.set_selected_value(&current, window, cx);
        });
    }

    pub(super) fn select_of(&self, key: &str) -> Option<SharedSelect> {
        self.selects.iter().find(|(k, _, _)| *k == key).map(|(_, entity, _)| entity.clone())
    }

    /// 从代码侧改掉某项设置后，把闭框的选中项拉回新值。
    ///
    /// `SelectState` 自己持有选中索引，而 [`Self::persist`] 只写设置文件和
    /// `runtime`——两者之间没有任何联系。少了这一步，"还原为默认值"会把值
    /// 真的写回去、撤销图标也如期消失，但闭框仍显示旧标签，看上去就是
    /// 撤销按钮没反应。
    ///
    /// 值不在 `values` 里（光标形状的"未设置"写空串）时落到第 0 项，与
    /// `add_select` 建初始索引时的同一条回落规则一致。
    fn sync_select(&mut self, key: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, select, values)) = self.selects.iter().find(|(k, _, _)| *k == key) else {
            return;
        };
        let row = values.iter().position(|candidate| *candidate == value).unwrap_or(0);
        // `set_selected_index` 只改索引和 `final_selected_index`，不发
        // `Confirm`，所以不会绕回订阅里再 persist 一次。
        select.clone().update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::default().row(row)), window, cx);
        });
    }

    fn select_row(
        &self,
        key: &'static str,
        label: &'static str,
        desc: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let select = self.select_of(key);
        // 闭态选中值 = accent（旧壳 combobox_value 15 处调用 14 处传
        // sk.accent）。闭框/背景都不带文字色，包一层就能继承下去；右侧
        // chevron 在组件内自带 muted，不会被染色。
        let control = div()
            .w(px(SETTINGS_SELECT_WIDTH))
            .text_color(cx.theme().link)
            .children(select.map(|state| Select::new(&state)));
        self.maybe_marked(key, label, desc, control, cx)
    }

    fn shell_select_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        self.row(
            language.pick("默认 Shell", "Default shell"),
            language.pick(
                "新标签用哪个程序开。已经开着的标签不受影响——它们跟的是各自创建时的选择。",
                "Chooses the program used for new tabs. Existing tabs are unaffected because each retains the shell chosen when it was created.",
            ),
            div()
                .w(px(SETTINGS_SELECT_WIDTH))
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().link)
                .child(Select::new(&self.shell_select)),
            cx,
        )
    }

    /// 布尔项 = 滑动开关（旧壳设置的液态胶囊 toggle），不是勾选框。
    /// 四通道动画对照 `SettingsToggleAnim`（LiquidToggle 过冲 / 按住拉伸 / recoil）。

    /// 出厂默认值。
    ///
    /// 不手抄一张常量表：空配置文本解析出来的就是默认值本身，和真实加载走
    /// 同一段代码。以后谁调整某个键的默认值，这里自动跟上，不会失配。
    fn factory_defaults() -> &'static RuntimeSettings {
        static DEFAULTS: std::sync::OnceLock<RuntimeSettings> = std::sync::OnceLock::new();
        DEFAULTS
            .get_or_init(|| RuntimeSettings::from_raw(&nebula_settings::RawSettings::from_text("")))
    }

    /// 这一项在这台机器上是否被改过，以及它的出厂值（供 ↶ 还原写回）。
    ///
    /// `None` = 该键不参与脏值标记。自由文本（代理地址、快捷键、字体名）不
    /// 进来：它们没有"默认长什么样"的直觉，标一条线只会让人以为出了错。
    fn setting_override(&self, key: &str) -> Option<(bool, String)> {
        let (cur, def) = (&self.runtime, Self::factory_defaults());
        // 布尔键写回 "1"/"0"，与 `persist_keys` 的读侧一致。
        macro_rules! flag {
            ($field:ident) => {
                Some((cur.$field != def.$field, String::from(if def.$field { "1" } else { "0" })))
            };
        }
        // 枚举键写回各自的 `settings_value()`，即配置文件里的记号。
        macro_rules! pick {
            ($field:ident) => {
                Some((cur.$field != def.$field, def.$field.settings_value().to_owned()))
            };
        }
        match key {
            "follow_system_theme" => flag!(follow_system_theme),
            "copy_on_select" => flag!(copy_on_select),
            "multiline_paste_confirm" => flag!(multiline_paste_confirm),
            "tab_close_visible" => flag!(tab_close_visible),
            "powerline" => flag!(powerline),
            "ghost" => flag!(ghost),
            "cjk_bold_regular" => flag!(cjk_bold_regular),
            "fetch" => flag!(fetch),
            "keep_session" => flag!(keep_session),
            "restore_session" => flag!(restore_session),
            "resume_ai" => flag!(resume_ai),
            "tray" => flag!(tray),
            "panel_resize" => flag!(panel_resize),
            "background_image_cover_chrome" => flag!(background_image_cover_chrome),
            "language" => pick!(language),
            "accept" => pick!(accept),
            "completion_style" => pick!(completion_style),
            "tabs_position" => pick!(tabs_position),
            "tab_reveal" => pick!(tab_reveal),
            "density" => pick!(density),
            "new_tab_position" => pick!(new_tab_position),
            "windowing_behavior" => pick!(windowing_behavior),
            "cell_width_mode" => pick!(cell_width_mode),
            "vcs_display" => pick!(vcs_display),
            "bell" => pick!(bell),
            "blur" => pick!(blur),
            "ssh_proxy_mode" => pick!(ssh_proxy_mode),
            // 光标两项是 Option：形状未写时沿用 Term 默认；闪烁未写时按产品默认 true。
            // 还原写空串即回到各自的缺省语义，而不是写一个"看起来一样"的显式值。
            "cursor_shape" => Some((cur.cursor_shape != def.cursor_shape, String::new())),
            "cursor_blink" => Some((
                effective_cursor_blink(cur.cursor_blink)
                    != effective_cursor_blink(def.cursor_blink),
                String::new(),
            )),
            _ => None,
        }
    }

    fn switch_row(
        &self,
        key: &'static str,
        label: &'static str,
        desc: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let control = crate::gpui_shell::widgets::NebulaSwitch::new(key).checked(checked).on_click(
            cx.listener(move |this, checked: &bool, window, cx| {
                this.toggle(key, *checked, window, cx);
            }),
        );
        self.maybe_marked(key, label, desc, control, cx)
    }

    /// 接了默认值对照的键走带标记的行，其余走普通行。
    ///
    /// 判断放在这一层而不是各个 section：脏值是**行**的属性，不是某个分区的
    /// 特性；散到 132 个调用点上去传参，第一次漏传就再也对不齐了。
    fn maybe_marked(
        &self,
        key: &'static str,
        label: &'static str,
        desc: &'static str,
        control: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.setting_override(key) {
            Some((dirty, factory)) => self
                .row_with_reset(
                    label,
                    desc,
                    dirty,
                    move |this, window, cx| {
                        this.persist(&[(key, factory.clone())], cx);
                        // 开关行读 `runtime`，notify 就够；下拉框自己存索引，
                        // 必须显式拉回，否则撤销只改了值不改显示。
                        this.sync_select(key, &factory, window, cx);
                    },
                    control,
                    cx,
                )
                .into_any_element(),
            None => self.row(label, desc, control, cx).into_any_element(),
        }
    }

    /// 旧壳启动目录：点路径打开选文件夹；空着显示「继承当前目录」；
    /// 有值时右侧「清除」。不是手填文本框。
    fn startup_directory_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let current = self
            .runtime
            .startup_directory
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty());
        let has_dir = current.is_some();
        let label: SharedString = current
            .map(str::to_owned)
            .unwrap_or_else(|| {
                language.pick("继承当前目录", "Inherit current directory").to_owned()
            })
            .into();
        let color = if has_dir { cx.theme().link } else { cx.theme().muted_foreground };
        self.row(
            language.pick("启动目录", "Startup directory"),
            language.pick(
                "新标签落在哪个目录。不设则继承 Pebrel 自己的工作目录，从资源管理器右键进来时就是那个文件夹。",
                "Chooses the directory for new tabs. When unset, they inherit Pebrel's working directory, such as the folder used to launch it from File Explorer.",
            ),
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .id("startup-directory")
                        .min_w_0()
                        .max_w(px(280.0))
                        .truncate()
                        .cursor_pointer()
                        .text_color(color)
                        .child(label)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pick_startup_directory(window, cx);
                        })),
                )
                .when(has_dir, |row| {
                    row.child(NebulaButton::new("startup-directory-clear").label(language.pick("清除", "Clear")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.clear_startup_directory(cx);
                        }),
                    ))
                }),
            cx,
        )
    }

    fn pick_startup_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let language = crate::gpui_shell::config::ui_language(cx);
        #[cfg(windows)]
        let picked = pick_folder_with_wsl_places(
            window,
            language.pick("选择终端启动目录", "Select the terminal startup directory"),
        );
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                language.pick("选择终端启动目录", "Select the terminal startup directory").into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            #[cfg(windows)]
            let Ok(Some(path)) = picked.await else { return };
            #[cfg(not(windows))]
            let path = {
                let Ok(Ok(Some(paths))) = picked.await else { return };
                let Some(path) = paths.into_iter().next() else { return };
                path
            };
            if !path.is_dir() {
                return;
            }
            let value = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |pane, cx| {
                pane.persist(&[("startup_directory", value)], cx);
            });
        })
        .detach();
    }

    fn clear_startup_directory(&mut self, cx: &mut Context<Self>) {
        self.persist(&[("startup_directory", String::new())], cx);
    }

    /// 文本输入 + 保存按钮（保存时读值写盘）。
    fn input_row(
        &self,
        label: &'static str,
        desc: &'static str,
        key: &'static str,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = input.clone();
        self.row(
            label,
            desc,
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(280.0)).child(Input::new(input)))
                .child(
                    NebulaButton::new(SharedString::from(format!("save-{key}")))
                        .label(crate::gpui_shell::config::ui_language(cx).pick("保存", "Save"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let value = state.read(cx).value().to_string();
                            this.persist(&[(key, value)], cx);
                        })),
                ),
            cx,
        )
    }

    /// 旧壳连续量使用轨道而不是加减按钮；数值和轨道共享同一个
    /// `SliderState`，拖动时由订阅统一写盘并热应用。
    fn slider_row(
        &self,
        label: &'static str,
        desc: &'static str,
        state: &Entity<SliderState>,
        display: SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        self.row(
            label,
            desc,
            h_flex()
                .w(px(220.0))
                .items_center()
                .gap_3()
                .child(div().flex_1().min_w_0().child(Slider::new(state)))
                .child(div()
                        .w(px(48.0))
                        .flex_shrink_0()
                        // 固定数值列宽，百分比位数变化时轨道不会左右跳动。
                        .child(display)),
            cx,
        )
    }

    fn choose_background_image(&mut self, cx: &mut Context<Self>) {
        let language = crate::gpui_shell::config::ui_language(cx);
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                language.pick("选择终端背景图片", "Select a terminal background image").into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let value = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |pane, cx| {
                pane.persist(&[("background_image", value)], cx);
            });
        })
        .detach();
    }

    fn background_image_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let current = self.runtime.background_image.clone();
        let has_image = current.as_ref().is_some_and(|path| !path.trim().is_empty());
        let path_label: Option<SharedString> =
            current.as_deref().filter(|path| !path.trim().is_empty()).map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_owned()
                    .into()
            });
        self.row_with_reset(
            language.pick("背景图片", "Background image"),
            language.pick(
                "铺在终端文字后面。图片本身不参与配色，字色仍由主题决定；看不清就调下面的不透明度。",
                "Draws an image behind terminal text. The image does not affect colors; text colors still come from the theme. Reduce the opacity below if text is hard to read.",
            ),
            has_image,
            |this, _, cx| {
                this.persist(&[("background_image", String::new())], cx);
            },
            h_flex()
                .items_center()
                .gap_2()
                .child(NebulaButton::new("background-image-choose").label(language.pick("选择图片", "Choose image")).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.choose_background_image(cx);
                    }),
                ))
                .when_some(path_label, |row, name| {
                    row.child(
                        div()
                            .max_w(px(180.0))
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(name),
                    )
                }),
            cx,
        )
    }

    fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.about_update, AboutUpdateState::Checking) {
            return;
        }
        self.about_update_seq = self.about_update_seq.wrapping_add(1);
        let sequence = self.about_update_seq;
        self.about_update = AboutUpdateState::Checking;
        let window_handle = window.window_handle();
        let task = cx.background_executor().spawn(async { crate::update_check::check_now() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let prompt = result.as_ref().ok().filter(|result| result.update_available).cloned();
            let _ = this.update(cx, |pane, cx| {
                if pane.about_update_seq != sequence {
                    return;
                }
                pane.about_last_checked =
                    Some(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
                pane.about_update = match result {
                    Ok(result) if result.update_available => {
                        AboutUpdateState::Available(result.latest)
                    },
                    Ok(result) => AboutUpdateState::UpToDate(result.latest),
                    Err(error) => AboutUpdateState::Failed(error),
                };
                cx.notify();
            });
            if let Some(result) = prompt {
                let _ = window_handle.update(cx, move |_, window, cx| {
                    crate::gpui_shell::workspace::open_update_dialog(result, window, cx);
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn section_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let font_picker = self.font_picker_dropdown(window, cx);
        let terminal = self
            .group(language.pick("终端", "Terminal"), cx)
            .child(self.shell_select_row(cx))
            .child(self.startup_directory_row(cx))
            .child(self.select_row(
                "bell",
                language.pick("终端铃声", "Terminal bell"),
                language.pick("程序发出 BEL 时的反应。开放工位上选闪烁，免得一次 Tab 补全失败响得整层楼都听见。", "Controls how Pebrel responds to BEL. Visual feedback is useful in shared spaces where an audible bell would be disruptive."),
                cx,
            ))
            .child(font_picker);
        let completion = self
            .group(language.pick("补全", "Completion"), cx)
            .child(self.switch_row(
                "ghost",
                language.pick("启用命令补全", "Enable command completion"),
                language.pick("按历史在光标后给出灰色建议，按下接受键才真正写进命令行。关掉后不再出现任何建议。", "Shows history-based suggestions after the cursor and inserts one only when the accept key is pressed. Disable this to hide all suggestions."),
                self.runtime.ghost,
                cx,
            ))
            .child(self.select_row(
                "accept",
                language.pick("补全接受键", "Completion accept key"),
                language.pick("用哪个键把灰色建议收下。Tab 在不少 shell 里已经绑了原生补全，撞车时改成右方向键。", "Chooses the key that accepts a suggestion. Many shells already use Tab for native completion; use Right arrow if they conflict."),
                cx,
            ))
            .child(self.select_row(
                "completion_style",
                language.pick("补全样式", "Completion style"),
                language.pick("行内灰字续在光标后，不挡住下方输出；弹窗列表能一次看到多个候选，代价是会盖住一片终端内容。", "Inline ghost text follows the cursor without covering output. A popup shows several candidates at once but obscures part of the terminal."),
                cx,
            ));
        v_flex().w_full().gap(px(GROUP_GAP)).child(terminal).child(completion)
    }

    fn section_interaction(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        // 分区名已经写在页头上，组标题再叫一遍"交互"就是同一个词说两遍。
        // 拆成两组反而各自有了名字：一组管鼠标怎么用，一组管标签往哪放。
        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(
                self.group(language.pick("鼠标与选区", "Mouse and selection"), cx)
                    .child(self.switch_row(
                        "copy_on_select",
                        language.pick("选中即复制（关 = 右键复制/粘贴）", "Copy on select (off = right-click copy/paste)"),
                        language.pick("开着时松开鼠标就进剪贴板，右键直接粘贴。关掉后选中不复制，右键弹出复制/粘贴菜单。", "When enabled, releasing the mouse copies the selection and right-click pastes. When disabled, selection does not copy and right-click opens a copy/paste menu."),
                        self.runtime.copy_on_select,
                        cx,
                    ))
                    .child(self.switch_row(
                        "multiline_paste_confirm",
                        language.pick(
                            "粘贴多行或高风险内容前询问",
                            "Ask before multiline or risky pastes",
                        ),
                        language.pick(
                            "在裸 shell 中粘贴换行、提权命令或控制字符时先确认。Bracketed paste 与全屏程序不受影响；关闭后直接粘贴。",
                            "Confirms line breaks, privileged commands, or control characters pasted into a plain shell. Bracketed paste and full-screen apps are unaffected; disabling this pastes directly.",
                        ),
                        self.runtime.multiline_paste_confirm,
                        cx,
                    ))
                    .child(self.switch_row(
                        "panel_resize",
                        language.pick("拖拽调节侧栏宽度", "Drag to resize the sidebar"),
                        language.pick("关掉后侧栏与面板的分界线钉死，拖不动；宽度仍可在别处改，只是不会被误拖。", "When disabled, the divider between the sidebar and panel cannot be dragged. The width can still be changed elsewhere without accidental resizing."),
                        self.runtime.panel_resize,
                        cx,
                    ))
                    .child(self.switch_row(
                        "cjk_bold_regular",
                        language.pick("CJK 粗体使用常规字形（提亮不加粗）", "Use regular glyphs for CJK bold (brighten without thickening)"),
                        language.pick("中日韩字形笔画密，字体引擎合成的伪粗体会糊成一团。开着时这些字改用提亮表达加粗，拉丁字母照旧走真粗体。", "Synthetic bold can blur dense CJK glyphs. When enabled, CJK bold is represented by brighter regular glyphs while Latin text still uses true bold."),
                        self.runtime.cjk_bold_regular,
                        cx,
                    )),
            )
            .child(
                self.group(language.pick("标签与窗口", "Tabs and windows"), cx)
                    .child(self.select_row(
                        "tabs_position",
                        language.pick("标签栏位置", "Tab bar position"),
                        language.pick("左侧边栏放得下完整路径和分屏结构，顶部则把纵向空间全留给终端。", "The left sidebar can show full paths and split structure; the top position reserves all vertical space for the terminal."),
                        cx,
                    ))
                    .child(self.select_row(
                        "tab_reveal",
                        language.pick("标签展开动效", "Tab reveal animation"),
                        language.pick("滑动=新标签带位移动画；即时=直接出现。远程桌面或低配机上选即时能少掉一次重绘。", "Slide animates new tabs into place; Instant shows them immediately. Instant avoids an extra redraw on remote desktops or slower computers."),
                        cx,
                    ))
                    .child(self.select_row(
                        "new_tab_position",
                        language.pick("新标签位置", "New tab position"),
                        language.pick("只管新建的标签插在哪。会话恢复与工作区导入按各自记录的顺序排，不看这一项。", "Controls only where newly created tabs are inserted. Restored sessions and imported workspaces retain their recorded order."),
                        cx,
                    ))
                    .child(self.select_row(
                        "windowing_behavior",
                        language.pick("新建实例行为", "New instance behavior"),
                        language.pick("从桌面或命令行再启动一次 Pebrel 时：另开一个窗口，还是把它作为新标签并进已有窗口。", "When Pebrel is launched again from the desktop or command line, choose whether to open a new window or attach the request as a tab in an existing window."),
                        cx,
                    ))
                    .child(self.select_row(
                        "vcs_display",
                        language.pick("侧栏版本控制（Git/SVN）", "Sidebar version control (Git/SVN)"),
                        language.pick("侧栏那块状态读哪种仓库。自动检测按目录里的 `.git` / `.svn` 判断，只有两者并存时才需要手动指定。", "Chooses which repository the sidebar status reads. Auto detect checks `.git` and `.svn`; manual selection is only needed when both are present."),
                        cx,
                    )),
            )
    }

    fn section_advanced(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        // 平台没实现的开关整行不画：开着却没效果比没有更糟（见
        // `platform::capabilities` 的说明）。
        let caps = crate::platform::CAPABILITIES;
        self.group(language.pick("会话生命周期", "Session lifecycle"), cx)
            .when(caps.hide_window_on_close, |group| group.child(self.switch_row(
                "keep_session",
                language.pick("关窗后保留后台会话", "Keep sessions running after the window closes"),
                language.pick("开着时点 × 只是把窗口收走，里面的 shell 继续在常驻进程里跑、可以再附着回来；关掉则连 shell 一起杀，未保存的东西会丢。", "When enabled, closing the window leaves its shells running in the resident process so they can be reattached. When disabled, closing the window terminates them and unsaved work is lost."),
                self.runtime.keep_session,
                cx,
            )))
            .child(self.switch_row(
                "restore_session",
                language.pick("启动时恢复上次标签", "Restore previous tabs at startup"),
                language.pick("重开时按上次的标签与分屏布局重建，工作目录一起回来。进程不会续命——恢复出来的是新 shell。", "Rebuilds the previous tab and split layout, including working directories. Processes are not resumed; restored tabs start new shells."),
                self.runtime.restore_session,
                cx,
            ))
            .child(self.switch_row(
                "resume_ai",
                language.pick("恢复会话时自动接续 AI 对话", "Resume AI conversations when restoring sessions"),
                language.pick("恢复出来的 AI 标签接着上次那段对话，而不是开一段新的。", "Restored AI tabs continue their previous conversation instead of starting a new one."),
                self.runtime.resume_ai,
                cx,
            ))
            .when(caps.system_tray, |group| group.child(self.switch_row(
                "tray",
                language.pick("常驻系统托盘图标", "Keep an icon in the system tray"),
                language.pick("在通知区域留一个图标，正在跑的 AI 会话从那里能直接看到状态。", "Keeps an icon in the notification area where the status of running AI sessions is visible."),
                self.runtime.tray,
                cx,
            )))
    }

    fn section_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        match self.active_section {
            0 => self.section_home(window, cx),
            1 => self.section_appearance(window, cx),
            2 => self.section_profiles(window, cx),
            3 => self.section_providers(cx),
            4 => self.section_ssh(cx),
            5 => self.section_network(cx),
            6 => self.section_interaction(cx),
            7 => self.section_keymap(cx),
            8 => self.section_advanced(cx),
            _ => self.section_backup(cx),
        }
        .into_any_element()
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // 选中底直接取 workspace 侧栏那一枚 token。两处都是"当前在哪"的
        // 指示，中间只隔着一条 hairline，用两种蓝会读成两套系统。
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let active_icon = theme.primary;
        let foreground = theme.foreground;
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let hairline = crate::gpui_shell::theme::settings_hairline(cx);
        let back_row_h = px(34.0);
        // 设置导航、内容和 workspace 左侧 tab 共享这一个主文字字号。
        let main_text_px = self.font_size_px(cx);

        // 一级标题靠左，二级导航略向右收进；只靠对齐关系建立层级，
        // 不额外添加卡片或分组标题。
        // 顶上不再画「设置」二字：右栏页头已经写着当前分区名，两者叠在同一
        // 视线高度上就是同一件事说两遍；这个页面是不是设置页，窗口和 tab 早
        // 就说明了。省下的标题块空间直接归还给导航项。
        //
        // 分栏靠一条 hairline 而不是留白：留白只说明"这两块不挨着"，线才说明
        // "这是两个区"——导航是全局的，右栏是当前分区的，二者不是同一层。
        let mut nav = v_flex()
            .w(px(SETTINGS_NAV_WIDTH))
            .h_full()
            .flex_shrink_0()
            .px_2()
            .pt(px(12.0))
            .pb(px(8.0))
            .gap(px(2.0))
            .border_r_1()
            .border_color(hairline)
            .child(
                div()
                    .id("settings-back")
                    .mx_1()
                    .mb(px(24.0))
                    .h(back_row_h)
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |item| item.bg(hover_bg).text_color(foreground))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsPaneEvent::Close)))
                    .child(Icon::new(IconName::ArrowLeft).small())
                    .child(language.pick("返回工作区", "Back to workspace")),
            );
        for ix in visible_nav_sections() {
            let active = ix == self.active_section;
            nav = nav.child(
                div()
                    .id(("settings-nav", ix))
                    .px_3()
                    .ml_1()
                    .mr_1()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(main_text_px))
                    // 选中态同时改变底色、墨色和字重，余光扫过也能确认当前位置。
                    .font_weight(if active {
                        gpui::FontWeight::SEMIBOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .when(active, |item| item.bg(active_bg).text_color(active_fg))
                    .when(!active, |item| item.text_color(muted).hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_section = ix;
                        cx.notify();
                    }))
                    .child(Icon::default().path(section_icon(ix)).small().text_color(
                        if active { active_icon } else { muted },
                    ))
                    .child(section_label(ix, language)),
            );
        }
        nav.into_any_element()
    }
}

impl EventEmitter<SettingsPaneEvent> for SettingsPane {}

impl Focusable for SettingsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = crate::gpui_shell::config::ui_language(cx);
        let nav = self.render_nav(cx);
        let content = self.section_content(window, cx);
        // 这里**不再**挂 font_family。旧壳设置页整页走终端 mono，是因为自绘
        // 只有一套字形缓存；GPUI 壳没有这个限制，继承那个观感只会让中文说明
        // 字距发虚、行更长。根字体由 `theme.font_family`（Windows 上是雅黑
        // UI）提供，等宽退回它真正的职责：标记机器可读的字面量。
        let base_px = self.font_size_px(cx);
        let font_picker_open = self.font_picker_open;
        let bg_picker_open = self.bg_picker_open && self.active_section == 1;
        let bg_dragging = self.bg_picker_drag.is_some();
        let ssh_editor_modal = self.ssh_editor_modal(cx);
        let appearance_picker_modal = self.appearance_picker_modal(window, cx);
        // 现有 reset 合同只覆盖外观键；其它页面显示同一个按钮会产生假承诺。
        let show_reset = self.active_section == 1;
        let appearance_header = show_reset.then(|| self.appearance_page_header(cx));
        let appearance_colors = appearance_picker::AppearanceColors::current(cx);
        let hairline = crate::gpui_shell::theme::settings_hairline(cx);
        let search_hover = cx.theme().list_hover;
        let reset_hover = cx.theme().list_hover;
        let search_query = self.settings_search_input.read(cx).value().trim().to_lowercase();
        let search_results: Vec<usize> = if search_query.is_empty() {
            Vec::new()
        } else {
            visible_nav_sections()
                .filter(|index| {
                    SECTION_SEARCH_TERMS[*index].contains(&search_query)
                        || section_label(*index, language).to_lowercase().contains(&search_query)
                })
                .take(6)
                .collect()
        };
        let search_focused =
            self.settings_search_input.read(cx).focus_handle(cx).is_focused(window);
        let search_panel = (search_focused && !search_results.is_empty()).then(|| {
            v_flex()
                .w(self
                    .settings_search_trigger_bounds
                    .map(|bounds| bounds.size.width)
                    .unwrap_or(px(680.0)))
                .p_1()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(hairline)
                .bg(crate::gpui_shell::theme::settings_panel_bg(cx))
                .shadow_lg()
                .occlude()
                .children(search_results.into_iter().map(|index| {
                    h_flex()
                        .id(("settings-search-result", index))
                        .h(px(36.0))
                        .px_2()
                        .gap_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |row| row.bg(search_hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.active_section = index;
                            this.settings_search_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                            cx.notify();
                        }))
                        .child(Icon::default().path(section_icon(index)).small())
                        .child(section_label(index, language))
                }))
        });

        let search_trigger = cx.entity().downgrade();
        let search_trigger_bounds = self.settings_search_trigger_bounds;

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            // 旧壳 `input/chrome.rs` 的合同是页面任何左键先撤销捕获。
            // 行自身会 stop_propagation 并完成「同行取消 / 他行转移」；其它
            // 控件的 mouse_down 仍先取得自己的焦点，这里只清状态，不抢焦点，
            // 因而点进搜索框后键盘完整归 InputState。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.keymap_capture.take().is_some() {
                        this.keymap_capture_preview.clear();
                        cx.notify();
                    }
                }),
            )
            // KeyDown 已由构造期注册的 interceptor 在 action 前独占；这里仅
            // 处理不走 KeystrokeEvent 的修饰键变化，用于实时回显 "Ctrl+…"。
            .when_some(self.keymap_capture, |root, _| {
                root.on_modifiers_changed(cx.listener(
                    |this, event: &ModifiersChangedEvent, _window, cx| {
                        if this.keymap_capture.is_none() {
                            return;
                        }
                        let prefix =
                            crate::display::keymap::gpui_mods_prefix(&event.modifiers);
                        if this.keymap_capture_preview != prefix {
                            this.keymap_capture_preview = prefix;
                            cx.notify();
                        }
                    },
                ))
            })
            // 旧壳设置页是独立的不透明 panel；终端壁纸只属于终端内容，
            // 不应穿透设置文字和控件。
            //
            // 这层不透明底必须自带卡圆角：外层终端卡虽然 `overflow_hidden`，
            // 但 GPUI 的裁剪是矩形 content_mask、不跟圆角，方角底会直接盖掉
            // 卡的四个圆角——这就是设置页看着「四角是直角」的原因。
            .rounded(crate::gpui_shell::theme::card_radius(cx))
            .bg(crate::gpui_shell::theme::settings_panel_bg(cx))
            .text_color(cx.theme().foreground)
            .when(show_reset, |root| root.bg(appearance_colors.surface))
            // 主文字的字号/字重从设置根向两栏继承；局部仅允许说明文字、
            // 徽标等语义上的次级信息覆盖为更小字号。
            .text_size(px(base_px))
            .font_weight(gpui::FontWeight::NORMAL)
            .flex()
            .flex_row()
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    // 页头固定高度；正文单独滚动，滚动设置时仍能知道
                    // 自己在哪个分区，也不会让标题与首组的距离随内容变化。
                    .when_some(appearance_header, |main, header| main.child(header))
                    .when(!show_reset, |main| main.child(
                        div()
                            .h(px(SETTINGS_HEADER_HEIGHT))
                            .flex_shrink_0()
                            .px_5()
                            .flex()
                            .items_center()
                            // 页头与正文之间画线，正文首组的上留白因此可以收
                            // 掉：线本身已经完成了"标题到此为止"的交代，再留一
                            // 段大空白就成了两遍。
                            .border_b_1()
                            .border_color(hairline)
                            .child(
                                h_flex()
                                    .flex_1()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(180.0))
                                            .flex_shrink_0()
                                            // 全页唯一放大的一处文字。层级本该
                                            // 靠字重和位置做，但页头是页面唯一
                                            // 的锚点，允许它比正文大一档。
                                            .text_size(px(base_px * 1.2))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(section_label(self.active_section, language)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .relative()
                                                    .w_full()
                                                    .max_w(px(680.0))
                                                    .child(
                                                        Input::new(&self.settings_search_input)
                                                            .cleanable(true)
                                                            .prefix(
                                                                Icon::new(IconName::Search)
                                                                    .xsmall()
                                                                    .text_color(
                                                                        cx.theme()
                                                                            .muted_foreground,
                                                                    ),
                                                            )
                                                            .aria_label(language.pick(
                                                                "在全部设置中搜索",
                                                                "Search all settings",
                                                            )),
                                                    )
                                                    .child(
                                                        gpui::canvas(
                                                            move |bounds, _, cx| {
                                                                let _ = search_trigger.update(
                                                                    cx,
                                                                    |pane, cx| {
                                                                        if pane
                                                                            .settings_search_trigger_bounds
                                                                            == Some(bounds)
                                                                        {
                                                                            return;
                                                                        }
                                                                        pane.settings_search_trigger_bounds =
                                                                            Some(bounds);
                                                                        cx.notify();
                                                                    },
                                                                );
                                                            },
                                                            |_, _, _, _| {},
                                                        )
                                                        .absolute()
                                                        .size_full(),
                                                    )
                                                    .when_some(
                                                        search_panel.zip(search_trigger_bounds),
                                                        |search, (panel, bounds)| {
                                                            search.child(
                                                                deferred(
                                                                    anchored()
                                                                        .anchor(
                                                                            gpui::Anchor::TopLeft,
                                                                        )
                                                                        .position(
                                                                            bounds.bottom_left(),
                                                                        )
                                                                        .offset(gpui::point(
                                                                            px(0.0),
                                                                            px(6.0),
                                                                        ))
                                                                        .snap_to_window_with_margin(
                                                                            px(8.0),
                                                                        )
                                                                        .child(panel),
                                                                )
                                                                .with_priority(3),
                                                            )
                                                        },
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(px(180.0))
                                            .flex_shrink_0()
                                            .flex()
                                            .justify_end()
                                            .when(show_reset, |slot| {
                                                slot.child(
                                                    div()
                                                        .id("settings-reset")
                                                        .size(px(24.0))
                                                        .rounded_md()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .cursor_pointer()
                                                        .hover(move |el| el.bg(reset_hover))
                                                        .tooltip(|window, cx| {
                                                            let language = crate::gpui_shell::config::ui_language(cx);
                                                            gpui_component::tooltip::Tooltip::new(
                                                                language.pick(
                                                                    "还原外观为默认值",
                                                                    "Restore appearance defaults",
                                                                ),
                                                            )
                                                            .build(window, cx)
                                                        })
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.reset_appearance(window, cx);
                                                            },
                                                        ))
                                                        .child(Icon::new(IconName::Undo2)),
                                                )
                                            }),
                                    ),
                            ),
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .px_5()
                            // 内容与页头线之间的一口气。分组之间的 24 由各
                            // section 容器的 `gap` 提供，不放在这里，也不放在
                            // 组自己身上——组自带 `pt` 时，首个元素不是分组的
                            // 页（按键映射开头是搜索框）就会直接贴到线上。
                            .pt(px(20.0))
                            .pb(px(22.0))
                            .when(show_reset, |content| content.px(px(40.0)).pt(px(28.0)).pb(px(30.0)))
                            // 注意这层包装 `v_flex` 的 `w_full` 不能删（2026-08-23
                            // 又栽了一次）：`overflow_y_scrollbar` 把内容层清成
                            // `Display::Block`，而 flex 容器在 block 父里
                            // `width:auto` 是 shrink-to-fit——不写 `w_full` 就退回
                            // 按内容取宽，每行各算一个宽度。判据是"窗口拖宽拖窄
                            // 都齐、卡在中间某个宽度不齐"：内容自然宽超过可用宽
                            // 时被压住反而对齐，没超过就各取各的。
                            //
                            // 不设行宽上限：内容跟着窗口延伸。标签与值离得
                            // 太远的问题由控件列定宽 + 右对齐解决（见
                            // design::CTRL_COL_W），不靠把整页钉窄。
                            // 正文继承设置根的统一主字号；只有说明、徽标、
                            // 快捷键提示等次级信息在各自元素上显式缩小。
                            .overflow_y_scrollbar()
                            // 这层 v_flex 不能省。`overflow_y_scrollbar` 不是
                            // 就地加滚动条：它把本容器的样式 clone 到自己新建
                            // 的外层 div，再把这层样式清空、只补 flex_1（见
                            // gpui-component scroll/scrollable.rs）。于是真正
                            // 承载正文的那层退回 gpui 默认的 Display::Block，
                            // 各分组的 `w_full` 不再撑满，而是各自按内容取宽：
                            // 2026-08-17 A/B 实测，去掉这层后预览组右缘停在
                            // 1356 而主题组到 1762，一组一个宽度，控件既不贴左
                            // 也不贴右。补一层竖向 flex 后分组走交叉轴 stretch
                            // 取宽，症状消失。
                            //
                            // 行宽上限：控件贴的是这个上限的右边，不是窗口的右
                            // 边。没有它时标签在最左、值在最右，1080px 宽的窗口
                            // 里眼睛要横跨一整屏才能把"这一项"和"它现在是什么"
                            // 配上，扫到第三行就串行。窗口再宽只是两侧留白变多。
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .justify_center()
                                    .child(v_flex().w_full().when(!show_reset, |content| content.max_w(px(960.0))).child(content)),
                            ),
                    ),
            )
            .when_some(ssh_editor_modal, |root, modal| root.child(modal))
            .when_some(appearance_picker_modal, |root, modal| root.child(modal))
            .when(font_picker_open, |root| {
                root
                    // 搜索框是当前焦点时 Escape 仍沿元素树冒泡到设置根；
                    // 这里只接管弹层生命周期，不干扰其它设置输入。
                    .on_key_down(cx.listener(
                        |this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key.eq_ignore_ascii_case("escape") {
                                cx.stop_propagation();
                                this.close_font_picker(window, true, cx);
                            }
                        },
                    ))
                    // 旧壳的“第一击只关闭浮层”合同：透明层覆盖设置页其余
                    // 控件并吃掉 mouse-down；字体面板自身通过 deferred
                    // priority=2 绘制在该层之上，内部搜索/滚动/点击照常工作。
                    .child(
                        div()
                            .id("font-picker-dismiss-layer")
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .cursor_default()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_font_picker(window, true, cx);
                                }),
                            ),
                    )
            })
            .when(bg_picker_open, |root| {
                root.on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key.eq_ignore_ascii_case("escape") {
                        cx.stop_propagation();
                        this.close_background_picker(cx);
                    }
                }))
                .when(bg_dragging, |root| {
                    root.on_mouse_move(cx.listener(
                        |this, event: &MouseMoveEvent, window, cx| {
                            this.on_bg_picker_move(event, window, cx);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.finish_bg_picker_drag(cx);
                        }),
                    )
                })
                .child(
                    div()
                        .id("bg-picker-dismiss-layer")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .cursor_default()
                        .when(bg_dragging, |layer| {
                            layer.on_mouse_move(cx.listener(
                                |this, event: &MouseMoveEvent, window, cx| {
                                    this.on_bg_picker_move(event, window, cx);
                                },
                            ))
                        })
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.finish_bg_picker_drag(cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_background_picker(cx);
                            }),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod appearance_picker_tests;
