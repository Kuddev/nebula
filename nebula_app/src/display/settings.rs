//! Settings special tab for Nebula's runtime appearance and completion settings.
//!
//! Mirrors the `command_palette` split, but goes one step further: besides the
//! *model* (sections, hit-testing, geometry, and the `nebula_settings.txt`
//! runtime store) this module also owns the panel's *rendering* — both the
//! background [`push_quads`] and the [`draw_text`] labels — so the giant
//! `display::mod` no longer carries the settings UI. The input layer stays the
//! only place that mutates state, reaching the `Display` methods that wrap this
//! model; rendering reads a snapshot [`SettingsView`] handed in each frame.
//!
//! Being a descendant module of `display`, this file can freely use the parent's
//! private helpers (`contains_rect`, `truncate_tab_label`, `nebula_data_dir`,
//! `NebulaTheme::palette`, `AcceptKey`, …) via `super::` — no visibility
//! churn needed in `mod.rs`.

use unicode_width::UnicodeWidthChar;

use nebula_terminal::vte::ansi::CursorShape;

use crate::config::UiConfig;
use crate::display::color::Rgb;
use crate::encrypted_backup::BackupSelection;
use crate::renderer::image::{BackgroundImageAlignment, BackgroundImageFit};
use crate::renderer::ui::{Rgba, UiQuad};
use crate::renderer::{GlyphCache, Renderer};

use super::keymap;
use super::ui::theme::Skin;
use super::ui::{icons, os_icons, surface, text_field, tokens, widgets};
use super::{
    AcceptKey, LanguagePreference, NebulaShell, NebulaTheme, SizeInfo, UiLanguage,
    chrome_settings_button_rect, contains_rect, nebula_data_dir, truncate_tab_label,
};

// Visual language: one flat panel color, one hairline, three text grays, ONE
// accent — hierarchy comes from typography and spacing. Every color is a
// [`Skin`] token from `display::theme` (single source of truth), so the page
// flips correctly between the light and dark theme families.

/// WebDAV 同步还处于内部迭代阶段。保留状态、持久化与后端实现，但在交互闭环
/// 完成前不向用户暴露入口；集中守门可避免绘制、命中和滚动高度各自遗漏。
const SHOW_WEBDAV_SYNC_SETTINGS: bool = false;

/// 备份页的后端已经可用，但页面先不放进侧栏，等恢复流程的可视化预览和
/// 冲突策略交互接入后再重新开放。枚举、命中和导出逻辑继续保留，避免隐藏
/// 入口演变成删除功能。
const SHOW_BACKUP_SETTINGS: bool = false;

/// Sidebar sections of the settings panel. Deliberately small: only sections
/// with real functionality behind them are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NebulaSettingsSection {
    /// Themes, custom colors, wallpaper, cursor and window opacity.
    #[default]
    Appearance,
    /// Completion behaviour plus the raw `nebula_settings.txt` config file.
    Profiles,
    /// OpenAI-compatible AI providers and their OS-backed API keys.
    Providers,
    /// Saved SSH destinations and hidden-host recovery.
    Ssh,
    /// Outbound proxy policy for SSH connections.
    Proxy,
    /// Selection/clipboard behaviour (the 「交互」 page).
    Interaction,
    /// Read-only shortcut sheet + pointer to `[[keyboard.bindings]]` remapping.
    Keymap,
    /// Power-user switches (session residency on close, …).
    Advanced,
    /// Password-protected export and restore of Nebula-owned data.
    Backup,
}

impl NebulaSettingsSection {
    fn label(self, language: UiLanguage) -> &'static str {
        match self {
            Self::Appearance => language.pick("外观", "Appearance"),
            Self::Profiles => language.pick("配置文件", "Profiles"),
            Self::Providers => language.pick("供应商", "Providers"),
            Self::Ssh => "SSH",
            Self::Proxy => language.pick("网络", "Network"),
            Self::Interaction => language.pick("交互", "Interaction"),
            Self::Keymap => language.pick("按键映射", "Key bindings"),
            Self::Advanced => language.pick("高级", "Advanced"),
            Self::Backup => language.pick("备份", "Backup"),
        }
    }
}

fn nav_icon(section: NebulaSettingsSection) -> icons::SettingsNavIcon {
    match section {
        NebulaSettingsSection::Appearance => icons::SettingsNavIcon::Appearance,
        NebulaSettingsSection::Profiles => icons::SettingsNavIcon::Profiles,
        NebulaSettingsSection::Providers => icons::SettingsNavIcon::Providers,
        NebulaSettingsSection::Ssh => icons::SettingsNavIcon::Ssh,
        NebulaSettingsSection::Proxy => icons::SettingsNavIcon::Proxy,
        NebulaSettingsSection::Interaction => icons::SettingsNavIcon::Interaction,
        NebulaSettingsSection::Keymap => icons::SettingsNavIcon::Keymap,
        NebulaSettingsSection::Advanced => icons::SettingsNavIcon::Advanced,
        NebulaSettingsSection::Backup => icons::SettingsNavIcon::Backup,
    }
}

/// Shortcut sheet shown in 设置→按键映射. Editable rows live in
/// [`keymap::EDITABLE_ACTIONS`]; the read-only extras in
/// [`keymap::READONLY_ROWS`] (spec 002).

/// Which independently draggable opacity control is being adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsOpacityTarget {
    Terminal,
    BackgroundImage,
}

/// Which inline dropdown (combobox) is currently expanded. At most one at a
/// time; the option list floats over later rows instead of pushing them down.
/// 用户范式（2026-07-23）：凡是多选项的设置一律做成内嵌下拉框，
/// 不再用"点击循环切换"——所有选项必须先可见再选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsDropdown {
    Shell,
    Font,
    BackgroundFit,
    BackgroundAlignment,
    Language,
    Accept,
    CursorShape,
    TabReveal,
    /// 代理：SSH 连接代理模式（关闭/系统/自定义）。
    SshProxyMode,
    /// 网络→指定代理→手动填写：地址协议（SOCKS5/HTTP）。
    SshProxyProtocol,
    /// 网络→指定代理→SSH 跳板：已保存主机的选择下拉。
    SshJumpHost,
    /// 外观密度（标准/紧凑）。
    Density,
    NewTabPosition,
    CellWidthMode,
    /// 背景色：色板网格 + 16 进制输入的专用浮层（不是通用行列表）。
    BackgroundColor,
}

/// 背景色色盘的预设色板（2 行 × 6 列）。前排偏星云/终端惯用暗底，尾部
/// 提供近黑与亮底；任意颜色可用下方的 16 进制输入框手动指定。
pub(crate) const BACKGROUND_SWATCHES: [Rgb; 12] = [
    Rgb::new(8, 10, 24),
    Rgb::new(12, 16, 28),
    Rgb::new(18, 14, 32),
    Rgb::new(24, 24, 37),
    Rgb::new(0, 43, 54),
    Rgb::new(6, 26, 28),
    Rgb::new(40, 42, 54),
    Rgb::new(30, 30, 30),
    Rgb::new(12, 12, 12),
    Rgb::new(0, 0, 0),
    Rgb::new(253, 246, 227),
    Rgb::new(255, 255, 255),
];

pub(super) const BACKGROUND_FIT_OPTIONS: [BackgroundImageFit; 4] = [
    BackgroundImageFit::Fill,
    BackgroundImageFit::Uniform,
    BackgroundImageFit::UniformToFill,
    BackgroundImageFit::None,
];

pub(super) const BACKGROUND_ALIGNMENT_OPTIONS: [BackgroundImageAlignment; 9] = [
    BackgroundImageAlignment::TopLeft,
    BackgroundImageAlignment::Top,
    BackgroundImageAlignment::TopRight,
    BackgroundImageAlignment::Left,
    BackgroundImageAlignment::Center,
    BackgroundImageAlignment::Right,
    BackgroundImageAlignment::BottomLeft,
    BackgroundImageAlignment::Bottom,
    BackgroundImageAlignment::BottomRight,
];

pub(super) const LANGUAGE_OPTIONS: [LanguagePreference; 3] =
    [LanguagePreference::System, LanguagePreference::ZhCn, LanguagePreference::EnUs];

pub(super) const ACCEPT_OPTIONS: [AcceptKey; 3] =
    [AcceptKey::Both, AcceptKey::Tab, AcceptKey::Right];

pub(super) const TAB_REVEAL_OPTIONS: [TabRevealMotion; 2] =
    [TabRevealMotion::Slide, TabRevealMotion::Instant];

/// 下拉行序即为这里的顺序；连接时的真正决策在 `crate::ssh_proxy`。
pub(super) const SSH_PROXY_MODE_OPTIONS: [crate::ssh_proxy::ProxyMode; 3] = [
    crate::ssh_proxy::ProxyMode::Off,
    crate::ssh_proxy::ProxyMode::System,
    crate::ssh_proxy::ProxyMode::Custom,
];

fn ssh_proxy_mode_label(mode: crate::ssh_proxy::ProxyMode, language: UiLanguage) -> &'static str {
    match mode {
        crate::ssh_proxy::ProxyMode::Off => language.pick("不使用代理", "No proxy"),
        crate::ssh_proxy::ProxyMode::System => language.pick("跟随系统", "Follow system"),
        crate::ssh_proxy::ProxyMode::Custom => language.pick("自定义代理", "Custom proxy"),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManualProxyProtocol {
    #[default]
    Socks5,
    Http,
}

pub(super) const MANUAL_PROXY_PROTOCOL_OPTIONS: [ManualProxyProtocol; 2] =
    [ManualProxyProtocol::Socks5, ManualProxyProtocol::Http];

/// 网络页“测试网络”的窗口态。结果只代表发起测试时已经落盘的那份设置；
/// 用户随后改动模式或地址时会回到 `Idle`，避免旧成功状态冒充新配置有效。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) enum ProxyTestStatus {
    #[default]
    Idle,
    Running,
    Success {
        elapsed_ms: u64,
        route: String,
    },
    Failed {
        message: String,
    },
}

fn manual_proxy_protocol_label(
    protocol: ManualProxyProtocol,
    _language: UiLanguage,
) -> &'static str {
    match protocol {
        ManualProxyProtocol::Socks5 => "SOCKS5",
        ManualProxyProtocol::Http => "HTTP",
    }
}

/// 把持久化 URL 拆成协议选择与可编辑地址。协议前缀由下拉框负责，输入框
/// 只显示地址正文，避免用户在两个控件里重复维护同一份信息。
pub(super) fn manual_proxy_parts(value: &str) -> (ManualProxyProtocol, &str) {
    let value = value.trim();
    for (prefix, protocol) in [
        ("socks5://", ManualProxyProtocol::Socks5),
        ("socks5h://", ManualProxyProtocol::Socks5),
        ("socks://", ManualProxyProtocol::Socks5),
        ("http://", ManualProxyProtocol::Http),
    ] {
        if value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return (protocol, &value[prefix.len()..]);
        }
    }
    // 兼容旧配置中的裸 `host:port`；不再显示含糊的“自动识别”，明确按
    // 历史规则作为 SOCKS5 处理。
    (ManualProxyProtocol::Socks5, value)
}

pub(super) fn manual_proxy_value(protocol: ManualProxyProtocol, address: &str) -> String {
    let address = address.trim();
    if address.is_empty() {
        return String::new();
    }
    match protocol {
        ManualProxyProtocol::Socks5 => format!("socks5://{address}"),
        ManualProxyProtocol::Http => format!("http://{address}"),
    }
}

/// 设置页的悬停层使用当前主题的 accent，而不是固定的灰色或另一套绿色。
/// 透明度在浅色/深色主题分别取值，保证两种底色上都只是轻微提示。
fn settings_skin(theme: NebulaTheme) -> Skin {
    let mut skin = theme.skin();
    let (hover_alpha, strong_alpha) = if skin.is_light { (22, 34) } else { (30, 46) };
    skin.hover = Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, hover_alpha);
    skin.hover_strong = Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, strong_alpha);
    skin
}

/// 网络页几何的动态输入：模式与子模式决定下方内容的种类与高度，覆盖行数
/// 决定「每主机覆盖」列表的高度。命中 / 绘制 / 滚动上限三方共用同一份，
/// 保证控件与点击区不漂移（组件化范式：几何同源）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ProxyChoice {
    Detected(usize),
    #[default]
    Manual,
    Jump,
    Command,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProxyPaneState {
    pub mode: crate::ssh_proxy::ProxyMode,
    /// 指定代理列表当前选中项。发现项的下标对应 `local_proxies` 快照。
    pub choice: ProxyChoice,
    pub found_count: usize,
    pub scanning: bool,
    /// 设置了每主机链路覆盖的主机数（profiles.json 的 `proxy` 字段）。
    pub override_count: usize,
}

/// 按键映射页几何的动态输入：搜索过滤后的每组可见行数 + 冲突提示占位。
/// 数组与 [`keymap::GROUPS`] 对齐（长度由 keymap 侧测试锁定为 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeymapPaneState {
    pub visible: [u8; 5],
    pub readonly_visible: u8,
    /// 是否显示冲突提示条（占一段版面，无冲突不留空洞）。
    pub clash: bool,
}

/// 每主机覆盖行的摘要：`direct` / `jump:` / 代理 URL → 人话。解析失败时
/// 原样展示——错值也该被看见，而不是被摘要藏起来。
pub(super) fn ssh_proxy_override_summary(value: &str, language: UiLanguage) -> String {
    let value = value.trim();
    if value.eq_ignore_ascii_case("direct") {
        return language.pick("不走代理", "Direct (no proxy)").to_owned();
    }
    match crate::ssh_proxy::ProxyLink::parse(value) {
        Ok(crate::ssh_proxy::ProxyLink::Jump(target)) => {
            format!(
                "{}{}{}",
                language.pick("SSH 跳板 · 经 ", "Jump host · via "),
                target,
                language.pick(" 转发", "")
            )
        },
        Ok(crate::ssh_proxy::ProxyLink::Server(server)) => {
            format!("{} · {}", language.pick("指定代理", "Custom proxy"), server.display())
        },
        Ok(crate::ssh_proxy::ProxyLink::Command(_)) => language
            .pick("自定义命令 · stdin/stdout 转发", "Custom command · stdin/stdout")
            .to_owned(),
        Err(_) => value.to_owned(),
    }
}

pub(super) const DENSITY_OPTIONS: [super::ui::tokens::Density; 2] =
    [super::ui::tokens::Density::Standard, super::ui::tokens::Density::Compact];
pub(super) const NEW_TAB_POSITION_OPTIONS: [NewTabPosition; 2] =
    [NewTabPosition::AfterCurrent, NewTabPosition::End];
pub(super) const CELL_WIDTH_MODE_OPTIONS: [CellWidthMode; 2] =
    [CellWidthMode::Compact, CellWidthMode::Relaxed];

/// Order mirrors the appearance page the user referenced.
pub(super) const CURSOR_SHAPE_OPTIONS: [CursorShape; 4] =
    [CursorShape::Beam, CursorShape::Underline, CursorShape::Block, CursorShape::HollowBlock];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TabRevealMotion {
    #[default]
    Slide,
    Instant,
}

impl TabRevealMotion {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "slide" => Some(Self::Slide),
            "instant" => Some(Self::Instant),
            _ => None,
        }
    }

    fn settings_value(self) -> &'static str {
        match self {
            Self::Slide => "slide",
            Self::Instant => "instant",
        }
    }
}

fn tab_reveal_label(motion: TabRevealMotion, language: UiLanguage) -> &'static str {
    match motion {
        TabRevealMotion::Slide => language.pick("滑动", "Slide"),
        TabRevealMotion::Instant => language.pick("立即", "Instant"),
    }
}

pub(super) fn density_label(
    density: super::ui::tokens::Density,
    language: UiLanguage,
) -> &'static str {
    match density {
        super::ui::tokens::Density::Standard => language.pick("标准", "Standard"),
        super::ui::tokens::Density::Compact => language.pick("紧凑", "Compact"),
    }
}

pub(super) fn density_parse(value: &str) -> Option<super::ui::tokens::Density> {
    match value.trim().to_ascii_lowercase().as_str() {
        "standard" => Some(super::ui::tokens::Density::Standard),
        "compact" => Some(super::ui::tokens::Density::Compact),
        _ => None,
    }
}

pub(super) fn density_settings_value(density: super::ui::tokens::Density) -> &'static str {
    match density {
        super::ui::tokens::Density::Standard => "standard",
        super::ui::tokens::Density::Compact => "compact",
    }
}

/// 新标签插入策略：真正创建标签时，新标签在标签顺序中的落点。
/// 上游兼容默认是紧邻当前标签之后。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NewTabPosition {
    #[default]
    AfterCurrent,
    End,
}

impl NewTabPosition {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "after_current" => Some(Self::AfterCurrent),
            "end" => Some(Self::End),
            _ => None,
        }
    }

    fn settings_value(self) -> &'static str {
        match self {
            Self::AfterCurrent => "after_current",
            Self::End => "end",
        }
    }
}

fn new_tab_position_label(position: NewTabPosition, language: UiLanguage) -> &'static str {
    match position {
        NewTabPosition::AfterCurrent => language.pick("当前标签之后", "After current"),
        NewTabPosition::End => language.pick("列表末尾", "End"),
    }
}

/// 单元格宽度模式：终端把字体的非整数设计宽度转换为整像素列宽的方式。
/// 「紧凑」保持上游的向下取整并作为兼容默认；「宽松」采用最接近整数取整，
/// 补足与 Windows Terminal 相差的那一像素。它只影响列宽，不改变单元格高度、
/// 字形比例或原生界面排版。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CellWidthMode {
    #[default]
    Compact,
    Relaxed,
}

impl CellWidthMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "relaxed" => Some(Self::Relaxed),
            _ => None,
        }
    }

    fn settings_value(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Relaxed => "relaxed",
        }
    }
}

fn cell_width_mode_label(mode: CellWidthMode, language: UiLanguage) -> &'static str {
    match mode {
        CellWidthMode::Compact => language.pick("紧凑", "Compact"),
        CellWidthMode::Relaxed => language.pick("宽松", "Relaxed"),
    }
}

pub(super) fn cursor_shape_label(shape: CursorShape, language: UiLanguage) -> &'static str {
    match shape {
        CursorShape::Beam => language.pick("条形（│）", "Bar (│)"),
        CursorShape::Underline => language.pick("下划线（_）", "Underscore (_)"),
        CursorShape::Block => language.pick("实心框（█）", "Filled box (█)"),
        CursorShape::HollowBlock => language.pick("空心框（□）", "Empty box (□)"),
        CursorShape::Hidden => language.pick("隐藏", "Hidden"),
    }
}

fn accept_label(accept: AcceptKey, language: UiLanguage) -> &'static str {
    match accept {
        AcceptKey::Right => language.pick("右方向键", "Right arrow"),
        AcceptKey::Tab => "Tab",
        AcceptKey::Both => language.pick("Tab 或右方向键", "Tab or Right arrow"),
    }
}

fn language_label(preference: LanguagePreference, language: UiLanguage) -> &'static str {
    match preference {
        LanguagePreference::System => language.pick("跟随系统", "Follow system"),
        LanguagePreference::ZhCn => "简体中文",
        LanguagePreference::EnUs => "English",
    }
}

/// Hit result for the top-left Nebula settings affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    None,
    Toggle,
    Panel,
    Nav(NebulaSettingsSection),
    Theme(NebulaTheme),
    Language(LanguagePreference),
    SystemThemeToggle,
    GhostToggle,
    AcceptCycle,
    ShellCycle,
    StartupDirectory,
    StartupDirectoryClear,
    /// One of the expanded shell picker rows (index into detected_shells).
    ShellPickerRow(usize),
    FontCycle,
    /// Imported-font picker rows; the final row is always "导入字体…".
    FontPickerRow(usize),
    /// 字体弹层顶部的搜索框。点它是定位光标，不是关掉弹层。
    FontSearchField,
    /// Font-size spinner steppers on the "字号" row.
    FontSizeUp,
    FontSizeDown,
    /// Cursor group: shape dropdown + its option rows, and the blink toggle.
    CursorShapeDropdown,
    CursorShapeOption(usize),
    CursorBlinkToggle,
    /// 交互: copy-on-select toggle row.
    CopyOnSelectToggle,
    /// 交互: 拖拽调节左侧栏宽 / 右抽屉宽。开启走确认框（reflow 开销）。
    /// SSH HOSTS 分界高度不归它管，始终可拖。
    PanelResizeToggle,
    /// 交互: 全宽字形 bold run 用 Regular 字形（粗体提亮不加粗）。
    CjkBoldToggle,
    TabRevealDropdown,
    TabRevealOption(usize),
    DensityDropdown,
    DensityOption(usize),
    NewTabPositionDropdown,
    NewTabPositionOption(usize),
    CellWidthModeDropdown,
    CellWidthModeOption(usize),
    /// Language combobox trigger (options resolve to [`SettingsHit::Language`]).
    LanguageDropdown,
    /// Expanded dropdown option rows for the cycle-style settings.
    AcceptOption(usize),
    FitOption(usize),
    AlignOption(usize),
    /// Restore one address from the persistent hidden-host list.
    RestoreHiddenSsh(usize),
    /// SSH 主机行本体：不是可点动作，只承载 hover 底色并让右缘三枚图标显形。
    /// 2026-08-11 用户裁定：静态那一行只留身份信息，动作藏进 hover。
    SshHostRow(usize),
    /// SSH settings page: connect to a saved destination.
    SshHostConnect(usize),
    /// SSH settings page: edit a saved destination.
    SshHostEdit(usize),
    /// SSH settings page: hide a destination without editing ~/.ssh/config.
    SshHostHide(usize),
    /// SSH settings page: re-read ~/.ssh/config immediately.
    SshImportConfig,
    /// SSH settings page: open the existing SSH editor for a new host.
    SshAddHost,
    /// AI provider management: preset/add, select, edit and persist actions.
    ProviderAdd,
    ProviderRow(usize),
    ProviderField(usize),
    ProviderSave,
    ProviderTest,
    ProviderDelete,
    ProviderEnableToggle(usize),
    ProviderCodexGoalsToggle,
    ProviderCodexRemoteToggle,
    ProviderApplyCodex,
    FetchToggle,
    PowerlineToggle,
    BlurToggle,
    OpacitySlider,
    BackgroundColor,
    /// 背景色浮层：色板网格里的一格。
    BackgroundSwatch(usize),
    /// 背景色浮层：16 进制输入框。
    BackgroundHexInput,
    /// 背景色浮层内部的空白（吞掉点击且不关闭浮层）。
    BackgroundPopupPanel,
    BackgroundImage,
    BackgroundImageClear,
    BackgroundImageFit,
    BackgroundImageAlignment,
    BackgroundImageCoverChrome,
    BackgroundImageOpacitySlider,
    OpenConfigFile,
    ImportTerminal,
    Reset,
    /// 高级: keep the resident server (detach) on window close.
    KeepSessionToggle,
    /// 高级: 启动时恢复上次会话（也是崩溃恢复的总开关）。
    RestoreSessionToggle,
    /// 高级→同步: 输入框（0=url 1=用户名 2=WebDAV 密码 3=E2E 口令）。
    SyncInput(usize),
    SyncAutoPullToggle,
    SyncPushButton,
    SyncPullButton,
    /// 代理→SSH 连接代理: 模式下拉触发行。
    SshProxyModeDropdown,
    SshProxyModeOption(usize),
    /// 网络页：按当前已提交设置执行一次真实出网测试。
    SshProxyTest,
    /// 网络→指定代理→手动填写：协议下拉。
    SshProxyProtocolDropdown,
    SshProxyProtocolOption(usize),
    /// 网络: 输入框（0=代理地址（手动填写展开行） 1=绕过列表）。
    SshProxyInput(usize),
    /// 网络→指定代理: 列表单选行（先是本机发现，随后是三种其他方式）。
    SshProxyLinkPick(usize),
    /// 网络→指定代理: 重新执行本机协议握手扫描。
    SshProxyRescan,
    /// 网络→指定代理→SSH 跳板: 主机下拉触发行。
    SshJumpHostDropdown,
    SshJumpHostOption(usize),
    /// 网络: 每主机覆盖行（值=覆盖列表下标），点击开该主机的编辑器。
    SshProxyOverrideEdit(usize),
    /// 按键映射: 页顶搜索框（过滤动作名与按键）。
    KeymapSearchField,
    /// 按键映射: one editable action row (click → capture a new combo).
    /// 值 = **可见槽位**（过滤后的顺序），chrome 经 display 映射回 flat 行。
    KeymapRow(usize),
    /// 按键映射: one read-only extras row. 不可点击，但 hover 得有着落——
    /// 2026-08-09 裁定：列表每一项都要轻量色变反馈，无位移。
    KeymapReadonlyRow(usize),
    BackupSelection(usize),
    BackupExport,
    BackupRestore,
}

/// Stable animation slots for the settings switches. Rendering and the shared
/// motion clock both use this mapping, so one switch can never borrow another
/// switch's thumb position while the pointer moves between rows.
pub(super) const SETTINGS_TOGGLE_COUNT: usize = 15;

pub(super) fn settings_toggle_slot(hit: SettingsHit) -> Option<usize> {
    Some(match hit {
        SettingsHit::SystemThemeToggle => 0,
        SettingsHit::GhostToggle => 1,
        SettingsHit::CursorBlinkToggle => 2,
        SettingsHit::CopyOnSelectToggle => 3,
        SettingsHit::PanelResizeToggle => 4,
        SettingsHit::CjkBoldToggle => 5,
        SettingsHit::FetchToggle => 6,
        SettingsHit::PowerlineToggle => 7,
        SettingsHit::BlurToggle => 8,
        SettingsHit::KeepSessionToggle => 9,
        SettingsHit::RestoreSessionToggle => 10,
        SettingsHit::SyncAutoPullToggle => 11,
        SettingsHit::BackgroundImageCoverChrome => 12,
        SettingsHit::ProviderCodexGoalsToggle => 13,
        SettingsHit::ProviderCodexRemoteToggle => 14,
        _ => return None,
    })
}

// ---- runtime settings store (`Nebula/nebula_settings.txt`) ----

pub(super) struct NebulaRuntimeSettings {
    pub(super) language: LanguagePreference,
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    pub(super) shell: NebulaShell,
    /// Raw default-shell id (`shell=<id>`), when the user picked a detected
    /// shell the 2-value `shell` enum can't represent (cmd, pwsh, nushell, a
    /// WSL distro). `None` = the enum value is authoritative. Written verbatim
    /// so `shell_detect::resolve_id` and the PTY layer both see the real id.
    pub(super) shell_id: Option<String>,
    /// Default working directory for fresh terminal tabs. `None` inherits the
    /// focused pane (or the process cwd for the first window).
    pub(super) startup_directory: Option<std::path::PathBuf>,
    pub(super) font_family: String,
    /// Terminal font size in LOGICAL px (`None` = follow the config file).
    /// Persisted so the settings spinner and Ctrl+wheel zoom survive restarts.
    pub(super) font_size: Option<f32>,
    /// Default cursor shape; escape sequences (vim, claude) still override.
    pub(super) cursor_shape: CursorShape,
    /// Default-on: a static cursor reads as a hang ("没有活动感").
    pub(super) cursor_blink: bool,
    /// 交互: 选中即复制（copyOnSelect）。关 = 右键复制。
    pub(super) copy_on_select: bool,
    /// 全宽字形（CJK 等）在 bold run 里用 Regular 字形（粗体提亮不加粗）。
    /// 默认开：小字号下雅黑 Bold fallback 与 Regular 混排发闷（任务 #4）。
    pub(super) cjk_bold_regular: bool,
    pub(super) tab_reveal: TabRevealMotion,
    /// 界面外观预设：标准 / 紧凑。
    pub(super) density: super::ui::tokens::Density,
    pub(super) new_tab_position: NewTabPosition,
    pub(super) cell_width_mode: CellWidthMode,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    /// Window close keeps the PTYs alive in the resident process (detach /
    /// re-attach session restore). Off = closing a window kills its shells.
    pub(super) keep_session: bool,
    /// 高级·会话：启动时恢复上次的标签。
    pub(super) restore_session: bool,
    /// 窗口背景模糊。Windows 11 上是 Mica（见
    /// `display::window::apply_windows_backdrop`），macOS / Wayland 上走
    /// winit 自己的实现。默认开：纯 alpha 会让背景的高频细节直接透上来压在
    /// 字上，低透明度下文字就读不出来了，而这正是透明度最常被调低的场景。
    pub(super) blur: bool,
    pub(super) opacity: f32,
    pub(super) background: Option<Rgb>,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    pub(super) background_image_fit: BackgroundImageFit,
    pub(super) background_image_alignment: BackgroundImageAlignment,
    pub(super) background_image_cover_chrome: bool,
    /// Chrome theme. Persisted so a restart keeps the chosen look AND the
    /// powerline bridge file gets rewritten with the right name on boot
    /// (it used to be reset to the default theme every launch).
    pub(super) theme: NebulaTheme,
    /// Automatically choose the light/dark member of the selected theme
    /// family when the operating system appearance changes.
    pub(super) follow_system_theme: bool,
    /// SSH host aliases pinned to the top of the sidebar's "SSH HOSTS"
    /// section (right-click a host row), in pinned order.
    pub(super) pinned_hosts: Vec<String>,
    /// SSH destinations auto-saved after a successful typed `ssh` connection,
    /// most recent first (see `Display::nebula_save_ssh_host`).
    pub(super) saved_hosts: Vec<String>,
    /// SSH aliases explicitly removed from the sidebar. This is separate from
    /// `saved_hosts` because entries discovered in `~/.ssh/config` would
    /// otherwise reappear on the very next merge.
    pub(super) hidden_hosts: Vec<String>,
    /// 交互：允许拖拽调节左侧栏宽 / SSH HOSTS 分界高 / 右抽屉宽。默认关，
    /// 开启走一次确认框——宽度拖动会实时重排终端，性能敏感。
    pub(super) panel_resize: bool,
    /// 左侧栏逻辑宽；[`super::SIDEBAR_W_LOGICAL`] 是默认值。
    pub(super) sidebar_w: f32,
    /// 右抽屉逻辑宽；布局时仍钳在窗口 42%。
    pub(super) drawer_w: f32,
    /// SSH HOSTS 停靠区高度覆盖（逻辑 px）；0 = 自动弹性规则。
    pub(super) hosts_band: f32,
    /// User keybinding overrides, raw `(combo, action)` pairs in file order
    /// (spec 002). Kept verbatim so unknown-but-valid future actions survive a
    /// load/save cycle; `display::keymap::build_bindings` parses them.
    pub(super) keybinds: Vec<(String, String)>,
    /// 快速终端全局切换键，和普通动作绑定分开持久化。
    pub(super) quick_terminal_hotkey: String,
    /// SSH 出站代理（全局三态）。解析与连接决策都在 `crate::ssh_proxy`，
    /// 这里只负责三个键在 `nebula_settings.txt` 里的持久化往返——写文件是
    /// 整体重写，键不进这个结构体就会在下一次保存时被抹掉。
    pub(super) ssh_proxy_mode: crate::ssh_proxy::ProxyMode,
    pub(super) ssh_proxy_url: String,
    /// 绕过列表原文（逗号分隔），按用户输入原样保存；拆分归 `ssh_proxy`。
    pub(super) ssh_proxy_no_proxy: String,
}

/// Load runtime UI settings from `Nebula/nebula_settings.txt`; defaults when
/// absent. Format is one `key=value` per line so power users can edit it while
/// the graphical settings page catches up.
pub(super) fn nebula_settings_load(config: &UiConfig) -> NebulaRuntimeSettings {
    let path = nebula_data_dir().join("nebula_settings.txt");
    let mut settings = NebulaRuntimeSettings {
        language: LanguagePreference::System,
        ghost: true,
        accept: AcceptKey::Both,
        shell: NebulaShell::PowerShell,
        shell_id: None,
        startup_directory: None,
        font_family: config.font.normal().family.clone(),
        font_size: None,
        cursor_shape: CursorShape::Beam,
        cursor_blink: true,
        copy_on_select: true,
        cjk_bold_regular: true,
        tab_reveal: TabRevealMotion::Slide,
        density: super::ui::tokens::Density::Standard,
        new_tab_position: NewTabPosition::AfterCurrent,
        cell_width_mode: CellWidthMode::Compact,
        // Off by default: the welcome screen pipes a whole script through the
        // fresh shell and repaints on resize — real startup-latency cost on
        // the critical path (user ruling: startup speed outranks the art).
        fetch: false,
        powerline: true,
        // Off by default (user ruling 2026-07-12): a plain terminal should die
        // clean on close. Residency leaves shells running in the background,
        // which reads as "the app didn't really exit" — opt IN, not out.
        keep_session: false,
        restore_session: true,
        // 2026-07-31 用户裁定：默认开。纯 alpha 下背景的高频细节压着文字，
        // 那正是"透明度调低就看不清"的物理来源；模糊把它拍成低频色块。想
        // 真透出后面窗口内容的人可以关掉。
        blur: true,
        opacity: config.window_opacity(),
        background: None,
        background_image: None,
        background_image_opacity: 0.38,
        background_image_fit: BackgroundImageFit::default(),
        background_image_alignment: BackgroundImageAlignment::default(),
        background_image_cover_chrome: false,
        theme: NebulaTheme::default(),
        // Preserve existing installations: automatic switching is opt-in so
        // an update never replaces an explicitly selected theme unexpectedly.
        follow_system_theme: false,
        pinned_hosts: Vec::new(),
        saved_hosts: Vec::new(),
        hidden_hosts: Vec::new(),
        panel_resize: false,
        sidebar_w: super::SIDEBAR_W_LOGICAL,
        drawer_w: super::side_panel::PANEL_W_LOGICAL,
        hosts_band: 0.0,
        keybinds: Vec::new(),
        quick_terminal_hotkey: keymap::DEFAULT_QUICK_TERMINAL_HOTKEY.to_owned(),
        ssh_proxy_mode: crate::ssh_proxy::ProxyMode::Off,
        ssh_proxy_url: String::new(),
        ssh_proxy_no_proxy: String::new(),
    };
    if let Ok(data) = std::fs::read_to_string(path) {
        for line in data.lines() {
            match line.split_once('=') {
                Some(("language", v)) => {
                    if let Some(language) = LanguagePreference::parse(v) {
                        settings.language = language;
                    }
                },
                Some(("ghost", v)) => settings.ghost = v.trim() != "0",
                Some(("theme", v)) => {
                    if let Some(theme) = NebulaTheme::from_prompt_name(v.trim()) {
                        settings.theme = theme;
                    }
                },
                Some(("accept", "right")) => settings.accept = AcceptKey::Right,
                Some(("accept", "tab")) => settings.accept = AcceptKey::Tab,
                Some(("accept", "both")) => settings.accept = AcceptKey::Both,
                Some(("shell" | "executor", v)) => {
                    let v = v.trim();
                    if let Some(shell) = NebulaShell::from_settings(v) {
                        settings.shell = shell;
                    }
                    // Preserve the raw id for detected shells the enum can't
                    // represent (cmd, pwsh, nushell, wsl:<distro>); the enum
                    // still tracks the PTY-integrated executor family so the
                    // prompt bootstrap picks the right base.
                    if !v.is_empty() {
                        settings.shell_id = Some(v.to_owned());
                    }
                },
                Some(("font_family", v)) => {
                    let family = v.trim();
                    if !family.is_empty() {
                        settings.font_family = family.to_owned();
                    }
                },
                Some(("font_size", v)) => {
                    if let Ok(size) = v.trim().parse::<f32>() {
                        settings.font_size = Some(size.clamp(6.0, 72.0));
                    }
                },
                Some(("cursor_shape", v)) => {
                    if let Some(shape) = parse_cursor_shape(v) {
                        settings.cursor_shape = shape;
                    }
                },
                Some(("cursor_blink", v)) => settings.cursor_blink = parse_bool(v, true),
                Some(("copy_on_select", v)) => settings.copy_on_select = parse_bool(v, true),
                Some(("cjk_bold_regular", v)) => settings.cjk_bold_regular = parse_bool(v, true),
                Some(("tab_reveal", v)) => {
                    settings.tab_reveal = TabRevealMotion::parse(v).unwrap_or_default();
                },
                Some(("density", v)) => {
                    settings.density = density_parse(v).unwrap_or_default();
                },
                Some(("new_tab_position", v)) => {
                    settings.new_tab_position = NewTabPosition::parse(v).unwrap_or_default();
                },
                Some(("cell_width_mode", v)) => {
                    settings.cell_width_mode = CellWidthMode::parse(v).unwrap_or_default();
                },
                Some(("startup_directory", v)) => {
                    let path = std::path::PathBuf::from(v.trim());
                    if path.is_dir() {
                        settings.startup_directory = Some(path);
                    }
                },
                Some(("fetch", v)) => settings.fetch = parse_bool(v, true),
                Some(("powerline", v)) => settings.powerline = parse_bool(v, true),
                Some(("keep_session", v)) => settings.keep_session = parse_bool(v, false),
                Some(("restore_session", v)) => settings.restore_session = parse_bool(v, true),
                Some(("panel_resize", v)) => settings.panel_resize = parse_bool(v, false),
                Some(("sidebar_w", v)) => {
                    if let Ok(w) = v.trim().parse::<f32>() {
                        settings.sidebar_w = w.clamp(super::SIDEBAR_W_MIN, super::SIDEBAR_W_MAX);
                    }
                },
                Some(("drawer_w", v)) => {
                    if let Ok(w) = v.trim().parse::<f32>() {
                        settings.drawer_w = w.clamp(super::DRAWER_W_MIN, super::DRAWER_W_MAX);
                    }
                },
                Some(("hosts_band", v)) => {
                    if let Ok(h) = v.trim().parse::<f32>() {
                        settings.hosts_band =
                            if h > 0.0 { h.max(super::HOSTS_BAND_MIN) } else { 0.0 };
                    }
                },
                Some(("blur", v)) => settings.blur = parse_bool(v, true),
                Some(("opacity", v)) => {
                    if let Ok(opacity) = v.trim().parse::<f32>() {
                        settings.opacity = opacity.clamp(0.0, 1.0);
                    }
                },
                Some(("background", v)) => settings.background = parse_hex_rgb(v.trim()),
                Some(("background_image", v)) => {
                    let v = v.trim();
                    settings.background_image = (!v.is_empty()).then(|| v.to_owned());
                },
                Some(("background_image_opacity", v)) => {
                    if let Ok(opacity) = v.trim().parse::<f32>() {
                        settings.background_image_opacity = opacity.clamp(0.0, 1.0);
                    }
                },
                Some(("background_image_fit", v)) => {
                    if let Some(fit) = BackgroundImageFit::parse(v) {
                        settings.background_image_fit = fit;
                    }
                },
                Some(("background_image_alignment", v)) => {
                    if let Some(alignment) = BackgroundImageAlignment::parse(v) {
                        settings.background_image_alignment = alignment;
                    }
                },
                Some(("background_image_cover_chrome", v)) => {
                    settings.background_image_cover_chrome = parse_bool(v, false);
                },
                Some(("pinned_hosts", v)) => {
                    settings.pinned_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                Some(("saved_hosts", v)) => {
                    settings.saved_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                Some(("follow_system_theme", v)) => {
                    settings.follow_system_theme = parse_bool(v, false)
                },
                Some(("hidden_hosts", v)) => {
                    settings.hidden_hosts = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                },
                Some(("keybind", v)) => {
                    // `keybind=<combo>:<action>`；解析验证归 keymap 模块，这里
                    // 只收原文——非法行在构建绑定表时静默丢弃。
                    if let Some((combo, action)) = v.split_once(':') {
                        let (combo, action) = (combo.trim(), action.trim());
                        if !combo.is_empty() && !action.is_empty() {
                            settings.keybinds.push((combo.to_lowercase(), action.to_owned()));
                        }
                    }
                },
                Some(("quick_terminal_hotkey", v)) => {
                    let value = v.trim();
                    if value.parse::<global_hotkey::hotkey::HotKey>().is_ok() {
                        settings.quick_terminal_hotkey = value.to_owned();
                    }
                },
                Some(("ssh_proxy_mode", v)) => {
                    settings.ssh_proxy_mode = crate::ssh_proxy::ProxyMode::parse(v);
                },
                Some(("ssh_proxy_url", v)) => settings.ssh_proxy_url = v.trim().to_owned(),
                Some(("ssh_proxy_no_proxy", v)) => {
                    settings.ssh_proxy_no_proxy = v.trim().to_owned();
                },
                _ => {},
            }
        }
    }
    settings
}

fn parse_bool(value: &str, default: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn parse_cursor_shape(value: &str) -> Option<CursorShape> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Some(CursorShape::Block),
        "beam" | "bar" => Some(CursorShape::Beam),
        "underline" => Some(CursorShape::Underline),
        "hollow" => Some(CursorShape::HollowBlock),
        _ => None,
    }
}

pub(super) fn cursor_shape_settings_value(shape: CursorShape) -> &'static str {
    match shape {
        CursorShape::Beam => "beam",
        CursorShape::Underline => "underline",
        CursorShape::HollowBlock => "hollow",
        CursorShape::Block | CursorShape::Hidden => "block",
    }
}

pub(crate) fn parse_hex_rgb(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb::new(r, g, b))
}

fn format_hex_rgb(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)
}

fn background_image_fit_label(fit: BackgroundImageFit, language: UiLanguage) -> &'static str {
    match fit {
        BackgroundImageFit::Fill => language.pick("拉伸", "Fill"),
        BackgroundImageFit::Uniform => language.pick("适应", "Uniform"),
        BackgroundImageFit::UniformToFill => language.pick("填充", "Uniform to fill"),
        BackgroundImageFit::None => language.pick("原始尺寸", "None"),
    }
}

fn background_image_alignment_label(
    alignment: BackgroundImageAlignment,
    language: UiLanguage,
) -> &'static str {
    match alignment {
        BackgroundImageAlignment::TopLeft => language.pick("左上", "Top left"),
        BackgroundImageAlignment::Top => language.pick("顶部", "Top"),
        BackgroundImageAlignment::TopRight => language.pick("右上", "Top right"),
        BackgroundImageAlignment::Left => language.pick("左侧", "Left"),
        BackgroundImageAlignment::Center => language.pick("居中", "Center"),
        BackgroundImageAlignment::Right => language.pick("右侧", "Right"),
        BackgroundImageAlignment::BottomLeft => language.pick("左下", "Bottom left"),
        BackgroundImageAlignment::Bottom => language.pick("底部", "Bottom"),
        BackgroundImageAlignment::BottomRight => language.pick("右下", "Bottom right"),
    }
}

pub(super) fn nebula_settings_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(nebula_data_dir().join("nebula_settings.txt"))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Persist runtime settings next to the history file.
pub(super) fn nebula_settings_write(settings: &NebulaRuntimeSettings) {
    let accept = match settings.accept {
        AcceptKey::Right => "right",
        AcceptKey::Tab => "tab",
        AcceptKey::Both => "both",
    };
    let background = settings.background.map(format_hex_rgb).unwrap_or_default();
    let background_image = settings.background_image.as_deref().unwrap_or("");
    // A picked detected-shell id (cmd/pwsh/nu/wsl:X) is written verbatim; the
    // 2-value enum is the fallback for the built-in powershell/bash choice.
    let shell =
        settings.shell_id.clone().unwrap_or_else(|| settings.shell.settings_value().to_owned());
    let startup_directory = settings
        .startup_directory
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let theme = settings.theme.prompt_name();
    let path = nebula_data_dir().join("nebula_settings.txt");
    let pinned_hosts = settings.pinned_hosts.join(",");
    let saved_hosts = settings.saved_hosts.join(",");
    let hidden_hosts = settings.hidden_hosts.join(",");
    let font_size = settings.font_size.map(|size| format!("{size:.1}")).unwrap_or_default();
    let mut keybinds = String::new();
    for (combo, action) in &settings.keybinds {
        keybinds.push_str(&format!("keybind={combo}:{action}\n"));
    }
    let quick_terminal_hotkey = settings.quick_terminal_hotkey.trim();
    let ssh_proxy_mode = settings.ssh_proxy_mode.as_str();
    let ssh_proxy_url = settings.ssh_proxy_url.trim();
    let ssh_proxy_no_proxy = settings.ssh_proxy_no_proxy.trim();
    let _ = std::fs::write(
        path,
        format!(
            "language={}\ntheme={theme}\nfollow_system_theme={}\nghost={}\naccept={accept}\nshell={shell}\nstartup_directory={startup_directory}\nfont_family={}\nfont_size={font_size}\ncursor_shape={}\ncursor_blink={}\ncopy_on_select={}\ncjk_bold_regular={}
tab_reveal={}\ndensity={}\nnew_tab_position={}\ncell_width_mode={}\nfetch={}\npowerline={}\nkeep_session={}\nrestore_session={}\nblur={}\nopacity={:.2}\nbackground={background}\nbackground_image={background_image}\nbackground_image_opacity={:.2}\nbackground_image_fit={}\nbackground_image_alignment={}\nbackground_image_cover_chrome={}\npanel_resize={}\nsidebar_w={:.0}\ndrawer_w={:.0}\nhosts_band={:.0}\npinned_hosts={pinned_hosts}\nsaved_hosts={saved_hosts}\nhidden_hosts={hidden_hosts}\nssh_proxy_mode={ssh_proxy_mode}\nssh_proxy_url={ssh_proxy_url}\nssh_proxy_no_proxy={ssh_proxy_no_proxy}\nquick_terminal_hotkey={quick_terminal_hotkey}\n{keybinds}",
            settings.language.as_str(),
            settings.follow_system_theme as u8,
            settings.ghost as u8,
            settings.font_family,
            cursor_shape_settings_value(settings.cursor_shape),
            settings.cursor_blink as u8,
            settings.copy_on_select as u8,
            settings.cjk_bold_regular as u8,
            settings.tab_reveal.settings_value(),
            density_settings_value(settings.density),
            settings.new_tab_position.settings_value(),
            settings.cell_width_mode.settings_value(),
            settings.fetch as u8,
            settings.powerline as u8,
            settings.keep_session as u8,
            settings.restore_session as u8,
            settings.blur as u8,
            settings.opacity,
            settings.background_image_opacity,
            settings.background_image_fit.settings_value(),
            settings.background_image_alignment.settings_value(),
            settings.background_image_cover_chrome as u8,
            settings.panel_resize as u8,
            settings.sidebar_w,
            settings.drawer_w,
            settings.hosts_band,
        ),
    );
}

// ---- geometry + hit-testing ----

#[derive(Debug, Clone, Copy)]
struct SettingsGeometry {
    gear: (f32, f32, f32, f32),
    popup: (f32, f32, f32, f32),
    sidebar: (f32, f32, f32, f32),
    content: (f32, f32, f32, f32),
    /// 中等宽度只保留导航图标，把空间还给正文；窄宽度进一步把设置行改为两层。
    compact_nav: bool,
    stacked_rows: bool,
    nav: [(NebulaSettingsSection, f32, f32, f32, f32); 9],
    /// Navigation group labels occupy the intentional gaps before connection
    /// and system settings, so the rail never contains unexplained whitespace.
    nav_groups: [(f32, f32, f32, f32); 2],
    options: [(NebulaTheme, f32, f32, f32, f32); 7],
    /// Live terminal preview card at the top of Appearance: configure →
    /// immediately see (font, size, colors, wallpaper opacity, cursor).
    preview: (f32, f32, f32, f32),
    system_theme: (f32, f32, f32, f32),
    shell: (f32, f32, f32, f32),
    startup_directory: (f32, f32, f32, f32),
    startup_directory_clear: (f32, f32, f32, f32),
    font: (f32, f32, f32, f32),
    /// "字号" spinner row; the value box + steppers derive via `widgets`.
    font_size_row: (f32, f32, f32, f32),
    cell_width_mode: (f32, f32, f32, f32),
    fetch: (f32, f32, f32, f32),
    powerline: (f32, f32, f32, f32),
    ghost: (f32, f32, f32, f32),
    accept: (f32, f32, f32, f32),
    open_config_file: (f32, f32, f32, f32),
    terminal_import: (f32, f32, f32, f32),
    ssh_host_row0: (f32, f32, f32, f32),
    ssh_host_count: usize,
    ssh_host_row_h: f32,
    ssh_host_gap: f32,
    ssh_add_host: (f32, f32, f32, f32),
    ssh_import_config: (f32, f32, f32, f32),
    hidden_host_row0: (f32, f32, f32, f32),
    hidden_host_count: usize,
    /// Full-width "窗口透明度" row and its draggable track.
    language_row: (f32, f32, f32, f32),
    density_row: (f32, f32, f32, f32),
    opacity_row: (f32, f32, f32, f32),
    opacity_slider: (f32, f32, f32, f32),
    /// 「窗口背景模糊」开关，紧贴透明度滑块——它只在透明时才看得出效果。
    blur: (f32, f32, f32, f32),
    /// Cursor group: shape combobox row + blink toggle row.
    cursor_shape_row: (f32, f32, f32, f32),
    cursor_blink_row: (f32, f32, f32, f32),
    background: (f32, f32, f32, f32),
    background_image: (f32, f32, f32, f32),
    background_image_clear: (f32, f32, f32, f32),
    background_image_fit: (f32, f32, f32, f32),
    background_image_alignment: (f32, f32, f32, f32),
    background_image_cover_chrome: (f32, f32, f32, f32),
    background_image_opacity_row: (f32, f32, f32, f32),
    background_image_opacity_slider: (f32, f32, f32, f32),
    /// 交互: copy-on-select toggle row.
    copy_on_select: (f32, f32, f32, f32),
    /// 交互·拖拽调节侧栏的开关行。
    panel_resize: (f32, f32, f32, f32),
    /// 交互: CJK 粗体策略 toggle row.
    cjk_bold: (f32, f32, f32, f32),
    tab_reveal: (f32, f32, f32, f32),
    new_tab_position: (f32, f32, f32, f32),
    reset: (f32, f32, f32, f32),
    /// Top edge of the scrollable content viewport (just below the fixed
    /// header band); everything above it never scrolls.
    content_top: f32,
    /// Total designed content height per section (scaled px, measured from
    /// `content_top`). `max_scroll = (height - viewport).max(0)`.
    appearance_h: f32,
    profiles_h: f32,
    providers_h: f32,
    interaction_h: f32,
    keymap_h: f32,
    /// 按键映射页动态几何：搜索框 / 冲突提示条 / 分组行。`keymap_slot_ys`
    /// 是过滤后各可见行的 y（前「可见总数」个有效）；`keymap_title_ys` 是
    /// 各组标题的 y（NaN = 该组被滤空；下标 5 = 固定快捷键组）。
    keymap_search: (f32, f32, f32, f32),
    keymap_note: (f32, f32, f32, f32),
    keymap_pane: KeymapPaneState,
    keymap_slot_ys: [f32; 32],
    keymap_title_ys: [f32; 6],
    keymap_hint_y: f32,
    /// 行矩形模板：x/w/h 通用，可见槽 `i` 的 y 在 `keymap_slot_ys[i]`。
    keymap_row0: (f32, f32, f32, f32),
    /// First row of the read-only shortcut group below the editable block.
    keymap_readonly_row0: (f32, f32, f32, f32),
    keymap_row_h: f32,
    advanced_h: f32,
    ssh_h: f32,
    provider_add: (f32, f32, f32, f32),
    provider_row0: (f32, f32, f32, f32),
    provider_row_h: f32,
    provider_row_count: usize,
    provider_fields: [(f32, f32, f32, f32); 6],
    provider_codex_goals: (f32, f32, f32, f32),
    provider_codex_remote: (f32, f32, f32, f32),
    provider_codex_apply: (f32, f32, f32, f32),
    provider_save: (f32, f32, f32, f32),
    provider_test: (f32, f32, f32, f32),
    provider_delete: (f32, f32, f32, f32),
    proxy_h: f32,
    keep_session: (f32, f32, f32, f32),
    /// 高级·会话：启动时恢复上次的标签（崩溃/强杀后同样走这条路）。
    restore_session: (f32, f32, f32, f32),
    /// 网络页：主模式仍是下拉框；指定代理分支按 HTML 原型排成扫描标题、
    /// 一张连续列表卡、选中项展开、绕过列表与每主机覆盖。
    ssh_proxy_mode: (f32, f32, f32, f32),
    ssh_proxy_scan_head: (f32, f32, f32, f32),
    ssh_proxy_scan_button: (f32, f32, f32, f32),
    ssh_proxy_list: (f32, f32, f32, f32),
    ssh_proxy_found_row0: (f32, f32, f32, f32),
    ssh_proxy_other_rows: [(f32, f32, f32, f32); 3],
    ssh_proxy_expand: (f32, f32, f32, f32),
    ssh_proxy_bypass: (f32, f32, f32, f32),
    ssh_proxy_test: (f32, f32, f32, f32),
    ssh_proxy_override_row0: (f32, f32, f32, f32),
    ssh_proxy_inherit: (f32, f32, f32, f32),
    proxy_pane: ProxyPaneState,
    sync_rows: [(f32, f32, f32, f32); 4],
    sync_auto_pull: (f32, f32, f32, f32),
    sync_actions: (f32, f32, f32, f32),
    /// Backups follow the HTML prototype: an always-visible auto-backup card,
    /// a compact export/restore segmented control, then grouped rows.
    backup_auto: (f32, f32, f32, f32),
    backup_segment: (f32, f32, f32, f32),
    backup_groups: [(f32, f32, f32, f32); 4],
    backup_rows: [(f32, f32, f32, f32); 9],
    backup_actions: (f32, f32, f32, f32),
    backup_h: f32,
}

/// Scrollable-content viewport height for the Settings tab.
fn settings_viewport_h(popup_h: f32, scale_factor: f32) -> f32 {
    popup_h - 72.0 * scale_factor
}

fn advanced_content_end(advanced_y0: f32, sync_y0: f32, row_h: f32) -> f32 {
    if SHOW_WEBDAV_SYNC_SETTINGS { sync_y0 + 7.0 * row_h } else { advanced_y0 + 2.0 * row_h }
}

/// Max scroll offset for `section` at the current window size. The input
/// layer clamps its accumulated wheel delta with this. Dropdown popups float
/// over rows, so they never change a section's content height.
pub(super) fn settings_max_scroll(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    section: NebulaSettingsSection,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    proxy: ProxyPaneState,
    keymap_pane: KeymapPaneState,
    provider_count: usize,
) -> f32 {
    let mut geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        0.0,
        hidden_host_count,
        ssh_host_count,
        density,
        proxy,
        keymap_pane,
    );
    fit_provider_rows(&mut geometry, provider_count);
    let (_, _, _, ph) = geometry.popup;
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
        NebulaSettingsSection::Providers => geometry.providers_h,
        NebulaSettingsSection::Ssh => geometry.ssh_h,
        NebulaSettingsSection::Proxy => geometry.proxy_h,
        NebulaSettingsSection::Interaction => geometry.interaction_h,
        NebulaSettingsSection::Keymap => geometry.keymap_h,
        NebulaSettingsSection::Advanced => geometry.advanced_h,
        NebulaSettingsSection::Backup => geometry.backup_h,
    };
    (content_h - settings_viewport_h(ph, scale_factor)).max(0.0)
}

fn settings_geometry(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    proxy: ProxyPaneState,
    keymap_pane: KeymapPaneState,
) -> SettingsGeometry {
    let s = |v: f32| v * scale_factor;
    let gear = chrome_settings_button_rect(size_info, scale_factor);

    // The settings surface is the active tab's content card. Keeping the
    // geometry rooted in that card makes sidebar/drawer animations and DPI
    // changes follow the exact same bounds as terminal and document tabs.
    let (popup_x, popup_y, popup_w, popup_h) = area;
    // 断点依据设置卡片的逻辑宽度，不依据物理像素或字体大小，避免 DPI 缩放
    // 意外改变交互布局。
    let logical_popup_w = popup_w / scale_factor.max(f32::EPSILON);
    let compact_nav = logical_popup_w < 900.0;
    let stacked_rows = logical_popup_w < 650.0;
    // Match the reference settings shell when wide; an icon-only rail gives
    // the content a usable minimum width at medium and narrow sizes.
    let sidebar_w =
        if compact_nav { s(64.0).min(popup_w * 0.30) } else { s(196.0).min(popup_w * 0.30) };
    let sidebar = (popup_x, popup_y, sidebar_w, popup_h);
    let content_gap = if compact_nav { s(8.0) } else { s(16.0) };
    let content_x = popup_x + sidebar_w + content_gap;
    let content_w = (popup_w - sidebar_w - content_gap).max(s(1.0));
    let content = (content_x, popup_y, content_w, popup_h);

    // The header band (big section title) is fixed; everything below it
    // scrolls by `scroll` px. `at` maps a design-space Y to screen space.
    let content_top = popup_y + s(72.0);
    let at = |design_y: f32| popup_y + s(design_y) - scroll;

    // ---- vertical rhythm (design px from the popup top) ----
    // Mirrors the HTML design sheet's breathing room: a group title hangs
    // 42px above its first row (title + 16px gap); rows inside a group are
    // CONTIGUOUS — one hairline frame around the block, hairline separators
    // between rows — and a finished group leaves 32px before the next title,
    // so `74 = 32 (section gap) + 42 (hanging title)`.
    // 行高与组间距按密度降档：紧凑用阶梯上既有的 COMPACT_ROW，组间距
    // 随行高等量收窄，不引入新数值。
    let row_advance = super::ui::tokens::control::settings_row(density);
    let row_advance = if stacked_rows {
        // 两层设置行必须同时容纳标签和通栏控件；密度 token 仍作为下限，
        // 让紧凑外观与既有主题保持一致。
        row_advance.max(72.0)
    } else {
        row_advance
    };
    #[allow(non_snake_case)]
    let ROW_H: f32 = row_advance;
    #[allow(non_snake_case)]
    let GROUP_ADVANCE: f32 = 74.0 - (44.0 - row_advance);
    // 预览卡设计高度：两行示例文本 + 光标演示的呼吸空间。
    const PREVIEW_H: f32 = 150.0;
    let content_inset = s(if compact_nav { 16.0 } else { 24.0 });

    // Live preview leads the page: configure → see it immediately.
    let preview_y0 = 146.0;
    let preview = (
        content_x + content_inset,
        at(preview_y0),
        (content_w - 2.0 * content_inset).max(s(1.0)),
        s(PREVIEW_H),
    );

    // 主题卡宽屏四列、窄屏两列；实际行数继续参与后续区块的 Y 坐标计算，
    // 确保绘制、滚动与命中测试同步移动。
    let card_gap = s(20.0);
    let card_columns = if stacked_rows { 2.0 } else { 4.0 };
    let card_inner_w = (content_w - 2.0 * content_inset).max(s(1.0));
    let card_w =
        ((card_inner_w - (card_columns - 1.0) * card_gap) / card_columns).max(s(1.0)).min(s(170.0));
    let card_h = s(64.0);
    let card_y0 = preview_y0 + PREVIEW_H + GROUP_ADVANCE;
    let card_x = content_x + content_inset;
    let card_row_pitch = card_h + s(48.0);
    let card = |i: f32| card_x + (i % card_columns) * (card_w + card_gap);
    let card_slot_y = |i: f32| at(card_y0) + (i / card_columns).floor() * card_row_pitch;

    let row_x = content_x + content_inset;
    let row_w = (content_w - 2.0 * content_inset).max(s(1.0));
    let row_h = s(ROW_H);

    // Appearance: preview, cards, colors, cursor and interface groups.
    let card_rows = (7.0f32 / card_columns).ceil();
    let system_theme_y0 = card_y0 + card_rows * (64.0 + 48.0) + GROUP_ADVANCE;
    let color_y0 = system_theme_y0 + ROW_H + GROUP_ADVANCE;
    // Background-image controls: path, stretch,
    // alignment and an independent image-opacity slider.
    let background_image_y0 = color_y0 + ROW_H;
    let background_image_fit_y0 = background_image_y0 + ROW_H;
    let background_image_alignment_y0 = background_image_fit_y0 + ROW_H;
    let background_image_opacity_y0 = background_image_alignment_y0 + ROW_H;
    let background_image_cover_chrome_y0 = background_image_opacity_y0 + ROW_H;
    // 光标组：形状下拉 + 闪烁开关。
    let cursor_y0 = color_y0 + 6.0 * ROW_H + GROUP_ADVANCE;
    let iface_y0 = cursor_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let density_y0 = iface_y0 + ROW_H;
    let opacity_y0 = density_y0 + ROW_H;
    // 模糊开关跟在透明度后面：它修饰的正是透明度透出来的那层东西，隔开就
    // 读不出这层因果了。
    let blur_y0 = opacity_y0 + ROW_H;
    // Terminal presentation belongs to Appearance: font sizing, the startup
    // welcome and the prompt decoration all change what a new terminal looks
    // like, while Profiles remains focused on what executable is launched.
    let terminal_appearance_y0 = blur_y0 + ROW_H + GROUP_ADVANCE;
    let appearance_h = s(terminal_appearance_y0 + 4.0 * ROW_H + 32.0 - 72.0);
    // 宽命中区包住细轨道，拖拽时无需精确点中 4px 线条。
    let opacity_row = (row_x, at(opacity_y0), row_w, row_h);
    let slider_x = if stacked_rows { row_x + s(16.0) } else { row_x + row_w - s(212.0) };
    let slider_w = if stacked_rows {
        (row_w - s(32.0)).max(s(24.0))
    } else {
        s(188.0).min(row_w * 0.42).max(s(96.0).min(row_w.max(s(1.0))))
    };
    let slider_y = if stacked_rows { s(34.0) } else { s(4.0) };
    let opacity_slider = (slider_x, at(opacity_y0) + slider_y, slider_w, s(36.0));
    let background_image_opacity_row = (row_x, at(background_image_opacity_y0), row_w, row_h);
    let background_image_opacity_slider =
        (slider_x, at(background_image_opacity_y0) + slider_y, slider_w, s(36.0));

    // Sidebar navigation rows. The rects line up with the active-row
    // highlight drawn while rendering. The reference uses 32px rows with a
    // 2px gap; the icon and label carry the internal breathing instead.
    let nav_x = popup_x + s(10.0);
    let nav_w = sidebar_w - s(20.0);
    let nav_h = s(32.0);
    let nav_gap = s(2.0);
    let nav_y0 = popup_y + s(88.0);
    let nav_slot = |i: f32| nav_y0 + i * (nav_h + nav_gap);
    let connections_group_y = nav_slot(5.0);
    let ssh_nav_y = if compact_nav { nav_slot(5.0) } else { connections_group_y + s(24.0) };
    let proxy_nav_y = ssh_nav_y + nav_h + nav_gap;
    let system_group_y = proxy_nav_y + nav_h + nav_gap;
    let advanced_nav_y = if compact_nav { nav_slot(7.0) } else { system_group_y + s(24.0) };
    let backup_nav_y = advanced_nav_y + nav_h + nav_gap;
    let nav = [
        (NebulaSettingsSection::Appearance, nav_x, nav_slot(0.0), nav_w, nav_h),
        (NebulaSettingsSection::Profiles, nav_x, nav_slot(1.0), nav_w, nav_h),
        (NebulaSettingsSection::Providers, nav_x, nav_slot(2.0), nav_w, nav_h),
        (NebulaSettingsSection::Interaction, nav_x, nav_slot(3.0), nav_w, nav_h),
        (NebulaSettingsSection::Keymap, nav_x, nav_slot(4.0), nav_w, nav_h),
        (NebulaSettingsSection::Ssh, nav_x, ssh_nav_y, nav_w, nav_h),
        (NebulaSettingsSection::Proxy, nav_x, proxy_nav_y, nav_w, nav_h),
        (NebulaSettingsSection::Advanced, nav_x, advanced_nav_y, nav_w, nav_h),
        (NebulaSettingsSection::Backup, nav_x, backup_nav_y, nav_w, nav_h),
    ];
    let nav_groups =
        [(nav_x, connections_group_y, nav_w, s(24.0)), (nav_x, system_group_y, nav_w, s(24.0))];

    // Profiles: dropdown popups FLOAT over later rows (Windows 11 combobox),
    // so every row keeps a fixed offset — no picker shove, no scroll jumps.
    let shell_y0 = 146.0;
    // Import belongs to the Terminal group, immediately below the default
    // Shell row, instead of being buried in the configuration section.
    let terminal_import_y0 = shell_y0 + ROW_H;
    let startup_directory_y0 = terminal_import_y0 + ROW_H;
    let font_y0 = startup_directory_y0 + ROW_H;
    let ghost_y0 = font_y0 + ROW_H + GROUP_ADVANCE;
    let open_y0 = ghost_y0 + 2.0 * ROW_H + GROUP_ADVANCE;
    let profiles_h = s(open_y0 + ROW_H + 32.0 - 72.0);

    // AI providers intentionally use a denser list + editor flow inspired by
    // CC Switch: the left-side list keeps switching cheap, while the active
    // provider exposes base URL/model/key fields without a second window.
    const PROVIDER_ROW_H: f32 = 58.0;
    let providers_y0 = 146.0;
    let provider_title_y = at(providers_y0 - 48.0);
    let provider_add_w = s(112.0).min(row_w * 0.42).max(s(34.0));
    let provider_add = (
        row_x + row_w - provider_add_w,
        widgets::centered_y(provider_title_y, s(32.0), s(30.0)),
        provider_add_w,
        s(30.0),
    );
    let provider_row0_y = providers_y0;
    let provider_row0 = (row_x, at(provider_row0_y), row_w, s(PROVIDER_ROW_H));
    let provider_form_y0 = provider_row0_y + PROVIDER_ROW_H * 6.0 + GROUP_ADVANCE;
    let provider_field = |i: f32| (row_x, at(provider_form_y0 + i * ROW_H), row_w, row_h);
    let provider_fields = [
        provider_field(0.0),
        provider_field(1.0),
        provider_field(2.0),
        provider_field(3.0),
        provider_field(4.0),
        provider_field(5.0),
    ];
    let provider_codex_goals = provider_field(6.0);
    let provider_codex_remote = provider_field(7.0);
    let provider_codex_apply = provider_field(8.0);
    let provider_actions_y = provider_form_y0 + 9.0 * ROW_H + GROUP_ADVANCE;
    let provider_action_w = s(112.0).min(row_w * 0.30).max(s(34.0));
    let provider_save =
        (row_x + row_w - provider_action_w, at(provider_actions_y), provider_action_w, row_h);
    let provider_test = (
        provider_save.0 - provider_action_w - s(10.0),
        at(provider_actions_y),
        provider_action_w,
        row_h,
    );
    let provider_delete = (row_x, at(provider_actions_y), provider_action_w, row_h);
    let providers_h = s(provider_actions_y + ROW_H + 32.0 - 72.0);

    // 交互: 剪贴板行为、标签行为（展开、新标签位置）与拖拽调节一组，
    // 文本渲染（CJK 粗体策略）另一组。
    let interaction_y0 = 146.0;
    let cjk_bold_y0 = interaction_y0 + ROW_H * 4.0 + GROUP_ADVANCE;
    let interaction_h = s(cjk_bold_y0 + ROW_H + 32.0 - 72.0);
    let copy_on_select = (row_x, at(interaction_y0), row_w, row_h);
    let tab_reveal = (row_x, at(interaction_y0 + ROW_H), row_w, row_h);
    let new_tab_position = (row_x, at(interaction_y0 + ROW_H * 2.0), row_w, row_h);
    let panel_resize = (row_x, at(interaction_y0 + ROW_H * 3.0), row_w, row_h);
    let cjk_bold = (row_x, at(cjk_bold_y0), row_w, row_h);

    // 按键映射没有普通设置页的首个“悬挂分组标题”，搜索框直接占用那块
    // 空间；继续沿用 146px 起点会无端多出 42px 空白。
    let keymap_y0 = 104.0;
    let keymap_search = (row_x, at(keymap_y0), row_w, s(34.0));
    let mut keymap_cursor = keymap_y0 + 34.0 + 12.0;
    let keymap_note = (row_x, at(keymap_cursor), row_w, s(50.0));
    if keymap_pane.clash {
        keymap_cursor += 50.0 + 12.0;
    }
    let mut keymap_slot_ys = [0.0f32; 32];
    let mut keymap_title_ys = [f32::NAN; 6];
    let mut keymap_slot = 0usize;
    let mut keymap_first_group = true;
    let keymap_rows_top = keymap_cursor + 42.0;
    for group in 0..keymap::GROUPS.len() {
        let rows = keymap_pane.visible[group] as usize;
        if rows == 0 {
            continue;
        }
        keymap_cursor += if keymap_first_group { 42.0 } else { GROUP_ADVANCE };
        keymap_first_group = false;
        keymap_title_ys[group] = at(keymap_cursor - 42.0);
        for _ in 0..rows {
            if keymap_slot < keymap_slot_ys.len() {
                keymap_slot_ys[keymap_slot] = at(keymap_cursor);
            }
            keymap_slot += 1;
            keymap_cursor += ROW_H;
        }
    }
    let keymap_row0 = (
        row_x,
        if keymap_slot > 0 { keymap_slot_ys[0] } else { at(keymap_rows_top) },
        row_w,
        row_h,
    );
    if keymap_pane.readonly_visible > 0 {
        keymap_cursor += if keymap_first_group { 42.0 } else { GROUP_ADVANCE };
        keymap_title_ys[5] = at(keymap_cursor - 42.0);
    }
    let keymap_readonly_row0 = (row_x, at(keymap_cursor), row_w, row_h);
    keymap_cursor += keymap_pane.readonly_visible as f32 * ROW_H;
    let keymap_hint_y = at(keymap_cursor + 10.0);
    let keymap_h = s(keymap_cursor + 10.0 + 26.0 + 32.0 - 72.0);

    // Advanced: session residency, then the gated WebDAV sync group (spec 003).
    // 隐藏期间同步几何仍保留，方便后续继续完善，但页面高度只计算可见内容。
    let advanced_y0 = 146.0;
    let keep_session = (row_x, at(advanced_y0), row_w, row_h);
    let restore_session = (row_x, at(advanced_y0 + ROW_H), row_w, row_h);
    let sync_y0 = advanced_y0 + ROW_H * 2.0 + GROUP_ADVANCE;
    let sync_row = |i: f32| (row_x, at(sync_y0 + i * ROW_H), row_w, row_h);
    let sync_rows = [sync_row(0.0), sync_row(1.0), sync_row(2.0), sync_row(3.0)];
    let sync_auto_pull = sync_row(4.0);
    let sync_actions = sync_row(5.0);
    let advanced_h = s(advanced_content_end(advanced_y0, sync_y0, ROW_H) + 32.0 - 72.0);

    // SSH 独立页：标题栏直接提供添加动作，正文只保留紧凑主机卡片、导入与
    // 隐藏主机恢复。代理拥有独立页面，不能再参与 SSH 页的高度或滚动计算。
    const SSH_HOST_ROW_H: f32 = 58.0;
    const SSH_HOST_GAP: f32 = 8.0;
    let ssh_host_y0 = 146.0;
    let ssh_host_row = |i: f32| {
        (row_x, at(ssh_host_y0 + i * (SSH_HOST_ROW_H + SSH_HOST_GAP)), row_w, s(SSH_HOST_ROW_H))
    };
    let ssh_host_row0 = ssh_host_row(0.0);
    let ssh_import_y0 = ssh_host_y0 + ssh_host_count as f32 * (SSH_HOST_ROW_H + SSH_HOST_GAP)
        - if ssh_host_count > 0 { SSH_HOST_GAP } else { 0.0 }
        + 16.0;
    let title_bar_y = at(ssh_host_y0 - 48.0);
    let title_bar_h = s(32.0);
    let add_button_w = s(112.0).min(row_w * 0.42).max(s(34.0));
    let add_button_h = s(30.0);
    let ssh_add_host = (
        row_x + row_w - add_button_w,
        widgets::centered_y(title_bar_y, title_bar_h, add_button_h),
        add_button_w,
        add_button_h,
    );
    let ssh_import_config = (row_x, at(ssh_import_y0), row_w, row_h);
    let hidden_y0 = ssh_import_y0 + ROW_H + GROUP_ADVANCE;
    let ssh_end = if hidden_host_count == 0 {
        ssh_import_y0 + ROW_H
    } else {
        hidden_y0 + hidden_host_count as f32 * ROW_H
    };
    let ssh_h = s(ssh_end + 32.0 - 72.0);

    // 出网测试是网络页的第一项，用户打开页面即可验证当前设置；三种模式
    // 的选择与地址控件排在测试横幅之后，避免把功能入口埋在页面最底部。
    let proxy_test_y = 146.0;
    let ssh_proxy_test = (row_x, at(proxy_test_y), row_w, s(54.0));
    // 网络代理只保留一张紧凑设置卡：所有模式都有方式行；自定义代理再
    // 展开地址与直连地址两行。旧跳板/命令只保留兼容解析，不再暴露第二套入口。
    let proxy_y0 = proxy_test_y + 54.0 + 18.0;
    let ssh_proxy_mode = (row_x, at(proxy_y0), row_w, row_h);
    let pane_y0 = proxy_y0 + ROW_H;
    let ssh_proxy_expand = (row_x, at(pane_y0), row_w, row_h);
    let bypass_y = pane_y0 + ROW_H;
    let ssh_proxy_bypass = (row_x, at(bypass_y), row_w, row_h);
    let pane_end =
        if proxy.mode == crate::ssh_proxy::ProxyMode::Custom { pane_y0 + ROW_H } else { pane_y0 };
    // 旧扫描/跳板/覆盖能力只保留在兼容后端；这些零尺寸几何用于渐进收口
    // 内部结构，任何绘制与命中都不再消费它们。
    let hidden_proxy_rect = (row_x, at(pane_y0), 0.0, 0.0);
    let ssh_proxy_scan_head = hidden_proxy_rect;
    let ssh_proxy_scan_button = hidden_proxy_rect;
    let ssh_proxy_list = hidden_proxy_rect;
    let ssh_proxy_found_row0 = hidden_proxy_rect;
    let ssh_proxy_other_rows = [hidden_proxy_rect; 3];
    let ssh_proxy_override_row0 = hidden_proxy_rect;
    let ssh_proxy_inherit = hidden_proxy_rect;
    let proxy_h = s(pane_end + 32.0 - 72.0);

    // Backup prototype: automatic-backup summary, export/restore segmented
    // action, then one grouped manifest card. Backup rows are taller because
    // every category carries a description and size, unlike the generic
    // single-line setting rows above.
    const BACKUP_ROW_H: f32 = 52.0;
    let backup_auto = (row_x, at(126.0), row_w, s(68.0));
    let backup_segment = (row_x, at(212.0), s(300.0).min(row_w), s(38.0));
    let backup_group = |y: f32| (row_x, at(y), row_w, s(24.0));
    let backup_groups =
        [backup_group(300.0), backup_group(480.0), backup_group(556.0), backup_group(632.0)];
    let backup_row = |y: f32| (row_x, at(y), row_w, s(BACKUP_ROW_H));
    let backup_rows = [
        backup_row(324.0), // appearance
        backup_row(376.0), // config
        backup_row(504.0), // SSH
        backup_row(428.0), // sync
        backup_row(580.0), // assistant
        backup_row(656.0), // session
        backup_row(708.0), // directory history
        backup_row(760.0), // command history
        backup_row(812.0), // fonts
    ];
    let backup_actions = backup_segment;
    let backup_h = s(864.0 + 40.0 - 72.0);

    SettingsGeometry {
        gear,
        popup: (popup_x, popup_y, popup_w, popup_h),
        sidebar,
        content,
        compact_nav,
        stacked_rows,
        nav,
        nav_groups,
        options: [
            (NebulaTheme::Nebula, card(0.0), card_slot_y(0.0), card_w, card_h),
            (NebulaTheme::SilverLight, card(1.0), card_slot_y(1.0), card_w, card_h),
            (NebulaTheme::SteelDark, card(2.0), card_slot_y(2.0), card_w, card_h),
            (NebulaTheme::LimestoneLight, card(3.0), card_slot_y(3.0), card_w, card_h),
            (NebulaTheme::CoalDark, card(4.0), card_slot_y(4.0), card_w, card_h),
            (NebulaTheme::LinenLight, card(5.0), card_slot_y(5.0), card_w, card_h),
            (NebulaTheme::MossDark, card(6.0), card_slot_y(6.0), card_w, card_h),
        ],
        preview,
        system_theme: (row_x, at(system_theme_y0), row_w, row_h),
        background: (row_x, at(color_y0), row_w, row_h),
        background_image: (row_x, at(background_image_y0), row_w, row_h),
        background_image_clear: (
            row_x + row_w - s(48.0),
            at(background_image_y0) + if stacked_rows { s(33.0) } else { s(5.0) },
            s(36.0),
            s(34.0),
        ),
        background_image_fit: (row_x, at(background_image_fit_y0), row_w, row_h),
        background_image_alignment: (row_x, at(background_image_alignment_y0), row_w, row_h),
        background_image_cover_chrome: (row_x, at(background_image_cover_chrome_y0), row_w, row_h),
        background_image_opacity_row,
        background_image_opacity_slider,
        cursor_shape_row: (row_x, at(cursor_y0), row_w, row_h),
        cursor_blink_row: (row_x, at(cursor_y0 + ROW_H), row_w, row_h),
        language_row: (row_x, at(iface_y0), row_w, row_h),
        density_row: (row_x, at(density_y0), row_w, row_h),
        opacity_row,
        opacity_slider,
        blur: (row_x, at(blur_y0), row_w, row_h),
        shell: (row_x, at(shell_y0), row_w, row_h),
        startup_directory: (row_x, at(startup_directory_y0), row_w, row_h),
        startup_directory_clear: (
            row_x + row_w - s(82.0),
            at(startup_directory_y0) + if stacked_rows { s(33.0) } else { s(5.0) },
            s(72.0),
            s(34.0),
        ),
        font: (row_x, at(font_y0), row_w, row_h),
        font_size_row: (row_x, at(terminal_appearance_y0), row_w, row_h),
        cell_width_mode: (row_x, at(terminal_appearance_y0 + ROW_H), row_w, row_h),
        fetch: (row_x, at(terminal_appearance_y0 + 2.0 * ROW_H), row_w, row_h),
        powerline: (row_x, at(terminal_appearance_y0 + 3.0 * ROW_H), row_w, row_h),
        ghost: (row_x, at(ghost_y0), row_w, row_h),
        accept: (row_x, at(ghost_y0 + ROW_H), row_w, row_h),
        open_config_file: (row_x, at(open_y0), row_w, row_h),
        terminal_import: (row_x, at(terminal_import_y0), row_w, row_h),
        ssh_host_row0,
        ssh_host_count,
        ssh_host_row_h: s(SSH_HOST_ROW_H),
        ssh_host_gap: s(SSH_HOST_GAP),
        ssh_add_host,
        ssh_import_config,
        hidden_host_row0: (row_x, at(hidden_y0), row_w, row_h),
        hidden_host_count,
        copy_on_select,
        panel_resize,
        cjk_bold,
        tab_reveal,
        new_tab_position,
        reset: if stacked_rows {
            (popup_x + popup_w - s(58.0), popup_y + s(24.0), s(38.0), s(38.0))
        } else {
            (popup_x + popup_w - s(170.0), popup_y + s(24.0), s(150.0), s(42.0))
        },
        content_top,
        appearance_h,
        profiles_h,
        providers_h,
        ssh_h,
        provider_add,
        provider_row0,
        provider_row_h: s(PROVIDER_ROW_H),
        provider_row_count: 6,
        provider_fields,
        provider_codex_goals,
        provider_codex_remote,
        provider_codex_apply,
        provider_save,
        provider_test,
        provider_delete,
        proxy_h,
        interaction_h,
        keymap_h,
        keymap_search,
        keymap_note,
        keymap_pane,
        keymap_slot_ys,
        keymap_title_ys,
        keymap_hint_y,
        keymap_row0,
        keymap_readonly_row0,
        keymap_row_h: row_h,
        advanced_h,
        keep_session,
        restore_session,
        ssh_proxy_mode,
        ssh_proxy_scan_head,
        ssh_proxy_scan_button,
        ssh_proxy_list,
        ssh_proxy_found_row0,
        ssh_proxy_other_rows,
        ssh_proxy_expand,
        ssh_proxy_bypass,
        ssh_proxy_test,
        ssh_proxy_override_row0,
        ssh_proxy_inherit,
        proxy_pane: proxy,
        sync_rows,
        sync_auto_pull,
        sync_actions,
        backup_auto,
        backup_segment,
        backup_groups,
        backup_rows,
        backup_actions,
        backup_h,
    }
}

/// Provider rows are backed by the persisted collection, while the rest of
/// Settings geometry is static. Moving the editor block here keeps hit tests,
/// rendering and scroll bounds on the same calculation without threading a
/// provider count through every unrelated geometry helper.
fn fit_provider_rows(geometry: &mut SettingsGeometry, provider_count: usize) {
    let old_count = geometry.provider_row_count;
    let delta = provider_count as f32 - old_count as f32;
    let offset = delta * geometry.provider_row_h;
    geometry.provider_row_count = provider_count;
    for field in &mut geometry.provider_fields {
        field.1 += offset;
    }
    geometry.provider_codex_goals.1 += offset;
    geometry.provider_codex_remote.1 += offset;
    geometry.provider_codex_apply.1 += offset;
    geometry.provider_save.1 += offset;
    geometry.provider_test.1 += offset;
    geometry.provider_delete.1 += offset;
    geometry.providers_h = (geometry.providers_h + offset).max(0.0);
}

pub(crate) fn opacity_slider_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    target: SettingsOpacityTarget,
    density: super::ui::tokens::Density,
) -> (f32, f32, f32, f32) {
    let geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        0,
        0,
        density,
        ProxyPaneState::default(),
        KeymapPaneState::default(),
    );
    match target {
        SettingsOpacityTarget::Terminal => geometry.opacity_slider,
        SettingsOpacityTarget::BackgroundImage => geometry.background_image_opacity_slider,
    }
}

pub(crate) fn opacity_from_pointer(pointer_x: f32, slider: (f32, f32, f32, f32)) -> f32 {
    ((pointer_x - slider.0) / slider.2.max(1.0)).clamp(0.0, 1.0)
}

/// 主机行右缘的三枚方形图标槽（连接 / 编辑 / 隐藏）。绘制与命中共用这一处
/// 推导，是"看得见的按钮点不中"的唯一防线。
///
/// 2026-08-11：从 54px 宽的描边文字按钮改成方形图标槽。文字按钮常态占掉右侧
/// 约 180px，把主机名挤到一半宽；图标槽只在 hover 时显形，静态那一行只剩身份
/// 信息。命中区保持正方且比墨迹宽，指针容差不被视觉尺寸绑死。
fn ssh_host_action_rect(
    row: (f32, f32, f32, f32),
    scale: f32,
    action: usize,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let (x, y, w, h) = row;
    let gap = s(2.0);
    let right = x + w - s(12.0);
    let slot = s(28.0).min((h - s(10.0)).max(s(20.0)));
    let bx = right
        - slot * (3usize.saturating_sub(action) as f32)
        - gap * (2usize.saturating_sub(action) as f32);
    (bx, widgets::centered_y(y, h, slot), slot, slot)
}

/// Compact trailing command button inside an otherwise non-clickable settings row.
const STANDARD_ROW_ACTION_W: f32 = 112.0;

fn row_action_rect(row: (f32, f32, f32, f32), scale: f32, logical_w: f32) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let (x, y, w, h) = row;
    let button_w = s(logical_w).min(w * 0.42);
    let button_h = s(30.0).min(h);
    let button_y = if h >= s(56.0) { y + h - s(38.0) } else { widgets::centered_y(y, h, button_h) };
    (x + w - s(16.0) - button_w, button_y, button_w, button_h)
}

/// Appearance 预览卡的壁纸绘制矩形：`(fit 目标, 实际允许触碰的裁剪带)`。
/// 裁剪带是预览与设置卡内容区的竖向交集——预览滚到 header 之下时壁纸不
/// 能跟着涂出去。完全滚出可视区时返回 `None`。
pub(super) fn appearance_preview_wallpaper_rects(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_hosts: usize,
    density: super::ui::tokens::Density,
) -> Option<((f32, f32, f32, f32), (f32, f32, f32, f32))> {
    let geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_hosts,
        0,
        density,
        ProxyPaneState::default(),
        KeymapPaneState::default(),
    );
    let (vx, vy, vw, vh) = geometry.preview;
    let (_, content_y, _, _) = geometry.content;
    let (_, py, _, ph) = geometry.popup;
    let top = vy.max(content_y);
    let bottom = (vy + vh).min(py + ph);
    if bottom <= top || vw <= 0.0 {
        return None;
    }
    Some(((vx, vy, vw, vh), (vx, top, vw, bottom - top)))
}

/// The combobox anchor rect + option count for `dropdown`, IF it belongs to
/// the active section. Hit-testing, popup quads and popup text all resolve
/// the floating list through this one helper so the three can never disagree.
/// 背景色浮层的几何：面板矩形、12 个色板格、16 进制输入框。绘制与命中
/// 测试共用这一个来源（组件化范式：几何同源，控件与点击区不漂移）。
pub(super) struct BackgroundColorPopup {
    pub(super) rect: (f32, f32, f32, f32),
    pub(super) swatch: [(f32, f32, f32, f32); 12],
    pub(super) hex: (f32, f32, f32, f32),
}

pub(super) fn background_color_popup(
    geometry: &SettingsGeometry,
    scale: f32,
) -> BackgroundColorPopup {
    let s = |v: f32| v * scale;
    let (ax, ay, aw, ah) = widgets::combobox_rect(geometry.background, scale);
    const COLS: usize = 6;
    let cell = s(30.0);
    let gap = s(8.0);
    let pad = s(12.0);
    let grid_w = COLS as f32 * cell + (COLS - 1) as f32 * gap;
    let grid_h = 2.0 * cell + gap;
    let hex_h = s(34.0);
    let w = (grid_w + 2.0 * pad).max(aw);
    let h = pad + grid_h + gap + hex_h + pad;
    // 与 combobox 浮层同规则：锚行右缘对齐，紧贴行下方展开。
    let x = ax + aw - w;
    let y = ay + ah + s(6.0);
    let mut swatch = [(0.0, 0.0, 0.0, 0.0); 12];
    for (i, rect) in swatch.iter_mut().enumerate() {
        let row = i / COLS;
        let col = i % COLS;
        *rect =
            (x + pad + col as f32 * (cell + gap), y + pad + row as f32 * (cell + gap), cell, cell);
    }
    let hex = (x + pad, y + pad + grid_h + gap, w - 2.0 * pad, hex_h);
    BackgroundColorPopup { rect: (x, y, w, h), swatch, hex }
}

/// 字体弹层的总行数：候选行 + 顶部那个搜索框。
///
/// 一个字体都筛不出来时仍然是 1 行——那时搜索框是弹层里唯一的东西，也正是
/// 用户要用来改查询串的那一个。
pub(super) fn font_popup_row_count(font_rows: usize) -> usize {
    font_rows + 1
}

const FONT_POPUP_MAX_VISIBLE_ROWS: usize = 8;

fn font_popup_window(total_rows: usize, requested_scroll: usize) -> (usize, usize) {
    let candidates = total_rows.saturating_sub(1);
    let candidate_visible = candidates.min(FONT_POPUP_MAX_VISIBLE_ROWS.saturating_sub(1));
    let max_scroll = candidates.saturating_sub(candidate_visible);
    (requested_scroll.min(max_scroll), 1 + candidate_visible.min(candidates))
}

fn popup_visible_index(
    dropdown: SettingsDropdown,
    absolute: Option<usize>,
    offset: usize,
    visible: usize,
) -> Option<usize> {
    let absolute = absolute?;
    if dropdown != SettingsDropdown::Font {
        return Some(absolute);
    }
    if absolute == 0 {
        Some(0)
    } else if absolute >= 1 + offset && absolute < 1 + offset + visible.saturating_sub(1) {
        Some(absolute - offset)
    } else {
        None
    }
}

/// 弹层第 `row` 行对应第几个候选。`None` = 那是搜索框。
pub(super) fn font_popup_slot(row: usize) -> Option<usize> {
    row.checked_sub(1)
}

fn dropdown_anchor(
    geometry: &SettingsGeometry,
    section: NebulaSettingsSection,
    dropdown: SettingsDropdown,
    shell_count: usize,
    font_count: usize,
    scale: f32,
) -> Option<((f32, f32, f32, f32), usize)> {
    use NebulaSettingsSection as Section;
    let anchor = |row| widgets::combobox_rect(row, scale);
    match (section, dropdown) {
        (Section::Profiles, SettingsDropdown::Shell) => Some((anchor(geometry.shell), shell_count)),
        (Section::Profiles, SettingsDropdown::Font) => {
            Some((anchor(geometry.font), font_popup_row_count(font_count)))
        },
        (Section::Profiles, SettingsDropdown::Accept) => {
            Some((anchor(geometry.accept), ACCEPT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::CellWidthMode) => {
            Some((anchor(geometry.cell_width_mode), CELL_WIDTH_MODE_OPTIONS.len()))
        },
        (Section::Interaction, SettingsDropdown::TabReveal) => {
            Some((anchor(geometry.tab_reveal), TAB_REVEAL_OPTIONS.len()))
        },
        (Section::Proxy, SettingsDropdown::SshProxyMode) => Some((
            ssh_proxy_mode_control(geometry.ssh_proxy_mode, scale),
            SSH_PROXY_MODE_OPTIONS.len(),
        )),
        (Section::Proxy, SettingsDropdown::SshProxyProtocol) => Some((
            ssh_proxy_manual_controls(geometry.ssh_proxy_expand, scale).0,
            MANUAL_PROXY_PROTOCOL_OPTIONS.len(),
        )),
        // 跳板主机下拉挂在展开行上；空列表也给一行（占位提示，点了无动作）。
        (Section::Proxy, SettingsDropdown::SshJumpHost) => Some((
            ssh_proxy_expand_control(geometry.ssh_proxy_expand, scale),
            geometry.ssh_host_count.max(1),
        )),
        (Section::Interaction, SettingsDropdown::NewTabPosition) => {
            Some((anchor(geometry.new_tab_position), NEW_TAB_POSITION_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::BackgroundFit) => {
            Some((anchor(geometry.background_image_fit), BACKGROUND_FIT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::BackgroundAlignment) => {
            Some((anchor(geometry.background_image_alignment), BACKGROUND_ALIGNMENT_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::Language) => {
            Some((anchor(geometry.language_row), LANGUAGE_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::Density) => {
            Some((anchor(geometry.density_row), DENSITY_OPTIONS.len()))
        },
        (Section::Appearance, SettingsDropdown::CursorShape) => {
            Some((anchor(geometry.cursor_shape_row), CURSOR_SHAPE_OPTIONS.len()))
        },
        _ => None,
    }
}

/// Hit-test the top-left settings button and its popup. `scroll` must be the
/// same offset the renderer used, so hits land on what the user actually sees;
/// rows scrolled out of the content viewport don't respond.
#[allow(clippy::too_many_arguments)]
/// 字体弹层里搜索框的矩形。命中与渲染各算一遍会漂，所以两边都问这里。
///
/// 返回 `None` 表示当前没有展开字体弹层。
pub fn font_search_field_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    section: NebulaSettingsSection,
    scroll: f32,
    dropdown: Option<SettingsDropdown>,
    font_count: usize,
    popup_scroll: usize,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
) -> Option<(f32, f32, f32, f32)> {
    if dropdown != Some(SettingsDropdown::Font) {
        return None;
    }
    let geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        ProxyPaneState::default(),
        KeymapPaneState::default(),
    );
    let s = |v: f32| v * scale_factor;
    let (_, py, _, ph) = geometry.popup;
    let (anchor, total) =
        dropdown_anchor(&geometry, section, SettingsDropdown::Font, 0, font_count, scale_factor)?;
    let (_, count) = font_popup_window(total, popup_scroll);
    let popup = widgets::combobox_popup_rect(
        anchor,
        count,
        scale_factor,
        geometry.content_top,
        py + ph - s(6.0),
    );
    Some(widgets::popup_row_rect(popup, 0, scale_factor))
}

/// 字体弹层的共享滚动条几何与最大候选偏移；与
/// [`push_popup_quads`] 的绘制参数同源，track/thumb 命中和拖拽都用它。
#[allow(clippy::too_many_arguments)]
pub(crate) fn font_popup_scrollbar(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    section: NebulaSettingsSection,
    scroll: f32,
    dropdown: Option<SettingsDropdown>,
    font_count: usize,
    popup_scroll: usize,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
) -> Option<(widgets::OverlayScrollbar, usize)> {
    if dropdown != Some(SettingsDropdown::Font) {
        return None;
    }
    let geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        ProxyPaneState::default(),
        KeymapPaneState::default(),
    );
    let s = |v: f32| v * scale_factor;
    let (_, py, _, ph) = geometry.popup;
    let (anchor, total) =
        dropdown_anchor(&geometry, section, SettingsDropdown::Font, 0, font_count, scale_factor)?;
    let (offset, count) = font_popup_window(total, popup_scroll);
    let popup = widgets::combobox_popup_rect(
        anchor,
        count,
        scale_factor,
        geometry.content_top,
        py + ph - s(6.0),
    );
    let total_h = total as f32 * widgets::POPUP_ROW_H * scale_factor;
    let viewport_h = count as f32 * widgets::POPUP_ROW_H * scale_factor;
    let bar = widgets::overlay_scrollbar(
        popup,
        viewport_h,
        total_h,
        offset as f32 * widgets::POPUP_ROW_H * scale_factor,
        scale_factor,
    )?;
    Some((bar, total - count))
}

/// 代理输入框（0=地址 1=绕过）的输入矩形；与渲染共用
/// [`sync_input_rect`]，供鼠标点击换算 caret 落点。
pub fn ssh_proxy_input_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    proxy: ProxyPaneState,
    index: usize,
) -> (f32, f32, f32, f32) {
    let geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        proxy,
        KeymapPaneState::default(),
    );
    match index {
        0 => ssh_proxy_manual_controls(geometry.ssh_proxy_expand, scale_factor).1,
        1 => sync_input_rect(geometry.ssh_proxy_bypass, scale_factor),
        _ => ssh_proxy_expand_control(geometry.ssh_proxy_expand, scale_factor),
    }
}

/// 按键映射页搜索框矩形；与渲染同一份 [`settings_geometry`]，供鼠标
/// 点击换算 caret 落点。
pub fn keymap_search_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    keymap_pane: KeymapPaneState,
) -> (f32, f32, f32, f32) {
    settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        ProxyPaneState::default(),
        keymap_pane,
    )
    .keymap_search
}

/// Active provider field rectangle, shared by pointer placement and IME
/// anchoring with the render pass.
pub fn provider_input_rect(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    scroll: f32,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    provider_count: usize,
    index: usize,
) -> Option<(f32, f32, f32, f32)> {
    let mut geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        ProxyPaneState::default(),
        KeymapPaneState::default(),
    );
    fit_provider_rows(&mut geometry, provider_count);
    geometry.provider_fields.get(index).copied().map(|row| sync_input_rect(row, scale_factor))
}

pub fn settings_hit(
    size_info: &SizeInfo,
    scale_factor: f32,
    area: (f32, f32, f32, f32),
    x: f32,
    y: f32,
    popup_open: bool,
    section: NebulaSettingsSection,
    scroll: f32,
    dropdown: Option<SettingsDropdown>,
    shell_count: usize,
    font_count: usize,
    font_popup_scroll: usize,
    hidden_host_count: usize,
    ssh_host_count: usize,
    density: super::ui::tokens::Density,
    proxy: ProxyPaneState,
    keymap_pane: KeymapPaneState,
    provider_count: usize,
) -> SettingsHit {
    let mut geometry = settings_geometry(
        size_info,
        scale_factor,
        area,
        scroll,
        hidden_host_count,
        ssh_host_count,
        density,
        proxy,
        keymap_pane,
    );
    fit_provider_rows(&mut geometry, provider_count);
    let s = |v: f32| v * scale_factor;

    if contains_rect(geometry.gear, x, y) {
        return SettingsHit::Toggle;
    }

    if !popup_open {
        return SettingsHit::None;
    }

    // Scrolled content only responds inside its viewport (below the fixed
    // header, above the popup's bottom edge).
    let (_, py, _, ph) = geometry.popup;
    let in_viewport = y >= geometry.content_top && y <= py + ph;

    // An expanded dropdown owns the pointer first: its floating option list
    // covers later rows, and those must not react through it.
    if let Some(dropdown) = dropdown {
        // 背景色是专用浮层（色板网格 + hex 输入），不走通用行列表。
        if dropdown == SettingsDropdown::BackgroundColor {
            if section == NebulaSettingsSection::Appearance {
                let popup = background_color_popup(&geometry, scale_factor);
                for (index, rect) in popup.swatch.iter().enumerate() {
                    if contains_rect(*rect, x, y) {
                        return SettingsHit::BackgroundSwatch(index);
                    }
                }
                if contains_rect(popup.hex, x, y) {
                    return SettingsHit::BackgroundHexInput;
                }
                if contains_rect(popup.rect, x, y) {
                    return SettingsHit::BackgroundPopupPanel;
                }
            }
        } else if let Some((anchor, total)) =
            dropdown_anchor(&geometry, section, dropdown, shell_count, font_count, scale_factor)
        {
            let (offset, count) = if dropdown == SettingsDropdown::Font {
                font_popup_window(total, font_popup_scroll)
            } else {
                (0, total)
            };
            let popup = widgets::combobox_popup_rect(
                anchor,
                count,
                scale_factor,
                geometry.content_top,
                py + ph - s(6.0),
            );
            if let Some(index) = widgets::popup_row_at(popup, count, scale_factor, x, y) {
                let index = if dropdown == SettingsDropdown::Font && index > 0 {
                    index + offset
                } else {
                    index
                };
                return match dropdown {
                    SettingsDropdown::Shell => SettingsHit::ShellPickerRow(index),
                    SettingsDropdown::Font => match font_popup_slot(index) {
                        Some(slot) => SettingsHit::FontPickerRow(slot),
                        None => SettingsHit::FontSearchField,
                    },
                    SettingsDropdown::BackgroundFit => SettingsHit::FitOption(index),
                    SettingsDropdown::BackgroundAlignment => SettingsHit::AlignOption(index),
                    SettingsDropdown::Language => SettingsHit::Language(LANGUAGE_OPTIONS[index]),
                    SettingsDropdown::Accept => SettingsHit::AcceptOption(index),
                    SettingsDropdown::TabReveal => SettingsHit::TabRevealOption(index),
                    SettingsDropdown::Density => SettingsHit::DensityOption(index),
                    SettingsDropdown::NewTabPosition => SettingsHit::NewTabPositionOption(index),
                    SettingsDropdown::CellWidthMode => SettingsHit::CellWidthModeOption(index),
                    SettingsDropdown::CursorShape => SettingsHit::CursorShapeOption(index),
                    SettingsDropdown::SshProxyMode => SettingsHit::SshProxyModeOption(index),
                    SettingsDropdown::SshProxyProtocol => {
                        SettingsHit::SshProxyProtocolOption(index)
                    },
                    SettingsDropdown::SshJumpHost => SettingsHit::SshJumpHostOption(index),
                    // 背景色浮层在上方特判处理，走不到通用行列表。
                    SettingsDropdown::BackgroundColor => SettingsHit::Panel,
                };
            }
            if contains_rect(popup, x, y) {
                // Padding strip inside the floating list: swallow the click
                // so rows underneath cannot react through the popup.
                return SettingsHit::Panel;
            }
        }
    }

    // Sidebar navigation and the header reset button are available from every
    // section.
    for (nav_section, nx, ny, nw, nh) in geometry.nav {
        if nav_section == NebulaSettingsSection::Backup && !SHOW_BACKUP_SETTINGS {
            continue;
        }
        if contains_rect((nx, ny, nw, nh), x, y) {
            return SettingsHit::Nav(nav_section);
        }
    }
    if !matches!(section, NebulaSettingsSection::Ssh | NebulaSettingsSection::Providers)
        && contains_rect(geometry.reset, x, y)
    {
        return SettingsHit::Reset;
    }

    if in_viewport {
        match section {
            NebulaSettingsSection::Appearance => {
                for (theme, ox, oy, ow, oh) in geometry.options {
                    if contains_rect((ox, oy, ow, oh), x, y) {
                        return SettingsHit::Theme(theme);
                    }
                }
                if contains_rect(widgets::toggle_rect(geometry.system_theme, scale_factor), x, y) {
                    return SettingsHit::SystemThemeToggle;
                }
                if contains_rect(widgets::combobox_rect(geometry.background, scale_factor), x, y) {
                    return SettingsHit::BackgroundColor;
                }
                if contains_rect(geometry.background_image_clear, x, y) {
                    return SettingsHit::BackgroundImageClear;
                }
                if contains_rect(geometry.background_image, x, y) {
                    return SettingsHit::BackgroundImage;
                }
                if contains_rect(
                    widgets::combobox_rect(geometry.background_image_fit, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::BackgroundImageFit;
                }
                if contains_rect(
                    widgets::combobox_rect(geometry.background_image_alignment, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::BackgroundImageAlignment;
                }
                if contains_rect(geometry.background_image_opacity_slider, x, y) {
                    return SettingsHit::BackgroundImageOpacitySlider;
                }
                if contains_rect(
                    widgets::toggle_rect(geometry.background_image_cover_chrome, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::BackgroundImageCoverChrome;
                }
                if contains_rect(
                    widgets::combobox_rect(geometry.cursor_shape_row, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::CursorShapeDropdown;
                }
                if contains_rect(
                    widgets::toggle_rect(geometry.cursor_blink_row, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::CursorBlinkToggle;
                }
                if contains_rect(widgets::combobox_rect(geometry.language_row, scale_factor), x, y)
                {
                    return SettingsHit::LanguageDropdown;
                }
                if contains_rect(geometry.density_row, x, y) {
                    return SettingsHit::DensityDropdown;
                }
                if contains_rect(widgets::toggle_rect(geometry.blur, scale_factor), x, y) {
                    return SettingsHit::BlurToggle;
                }
                if contains_rect(geometry.opacity_slider, x, y) {
                    return SettingsHit::OpacitySlider;
                }
                {
                    let (_, up, down) =
                        widgets::spinner_rects(geometry.font_size_row, scale_factor);
                    if contains_rect(up, x, y) {
                        return SettingsHit::FontSizeUp;
                    }
                    if contains_rect(down, x, y) {
                        return SettingsHit::FontSizeDown;
                    }
                }
                if contains_rect(
                    widgets::combobox_rect(geometry.cell_width_mode, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::CellWidthModeDropdown;
                }
                if contains_rect(widgets::toggle_rect(geometry.fetch, scale_factor), x, y) {
                    return SettingsHit::FetchToggle;
                }
                if contains_rect(widgets::toggle_rect(geometry.powerline, scale_factor), x, y) {
                    return SettingsHit::PowerlineToggle;
                }
            },
            NebulaSettingsSection::Profiles => {
                // The import row touches the Shell row at one inclusive
                // boundary in the shared hit helper; give it priority there
                // so a click on its top edge cannot open the dropdown.
                if contains_rect(
                    row_action_rect(geometry.terminal_import, scale_factor, STANDARD_ROW_ACTION_W),
                    x,
                    y,
                ) {
                    return SettingsHit::ImportTerminal;
                }
                if contains_rect(widgets::combobox_rect(geometry.shell, scale_factor), x, y) {
                    return SettingsHit::ShellCycle;
                }
                if contains_rect(geometry.startup_directory_clear, x, y) {
                    return SettingsHit::StartupDirectoryClear;
                }
                if contains_rect(geometry.startup_directory, x, y) {
                    return SettingsHit::StartupDirectory;
                }
                if contains_rect(widgets::combobox_rect(geometry.font, scale_factor), x, y) {
                    return SettingsHit::FontCycle;
                }
                if contains_rect(widgets::toggle_rect(geometry.ghost, scale_factor), x, y) {
                    return SettingsHit::GhostToggle;
                }
                if contains_rect(widgets::combobox_rect(geometry.accept, scale_factor), x, y) {
                    return SettingsHit::AcceptCycle;
                }
                if contains_rect(
                    row_action_rect(geometry.open_config_file, scale_factor, STANDARD_ROW_ACTION_W),
                    x,
                    y,
                ) {
                    return SettingsHit::OpenConfigFile;
                }
            },
            NebulaSettingsSection::Providers => {
                if contains_rect(geometry.provider_add, x, y) {
                    return SettingsHit::ProviderAdd;
                }
                for index in 0..geometry.provider_row_count {
                    let row = (
                        geometry.provider_row0.0,
                        geometry.provider_row0.1 + index as f32 * geometry.provider_row_h,
                        geometry.provider_row0.2,
                        geometry.provider_row_h,
                    );
                    if contains_rect(row, x, y) {
                        if x >= row.0 + row.2 - scale_factor * 76.0 {
                            return SettingsHit::ProviderEnableToggle(index);
                        }
                        return SettingsHit::ProviderRow(index);
                    }
                }
                for (index, field) in geometry.provider_fields.iter().enumerate() {
                    if contains_rect(*field, x, y) {
                        return SettingsHit::ProviderField(index);
                    }
                }
                if contains_rect(
                    widgets::toggle_rect(geometry.provider_codex_goals, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::ProviderCodexGoalsToggle;
                }
                if contains_rect(
                    widgets::toggle_rect(geometry.provider_codex_remote, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::ProviderCodexRemoteToggle;
                }
                if contains_rect(
                    row_action_rect(geometry.provider_codex_apply, scale_factor, 148.0),
                    x,
                    y,
                ) {
                    return SettingsHit::ProviderApplyCodex;
                }
                if contains_rect(geometry.provider_save, x, y) {
                    return SettingsHit::ProviderSave;
                }
                if contains_rect(geometry.provider_test, x, y) {
                    return SettingsHit::ProviderTest;
                }
                if contains_rect(geometry.provider_delete, x, y) {
                    return SettingsHit::ProviderDelete;
                }
            },
            NebulaSettingsSection::Ssh => {
                for index in 0..geometry.ssh_host_count {
                    let row = (
                        geometry.ssh_host_row0.0,
                        geometry.ssh_host_row0.1
                            + index as f32 * (geometry.ssh_host_row_h + geometry.ssh_host_gap),
                        geometry.ssh_host_row0.2,
                        geometry.ssh_host_row_h,
                    );
                    if contains_rect(ssh_host_action_rect(row, scale_factor, 0), x, y) {
                        return SettingsHit::SshHostConnect(index);
                    }
                    if contains_rect(ssh_host_action_rect(row, scale_factor, 1), x, y) {
                        return SettingsHit::SshHostEdit(index);
                    }
                    if contains_rect(ssh_host_action_rect(row, scale_factor, 2), x, y) {
                        return SettingsHit::SshHostHide(index);
                    }
                    // 三枚图标之后才轮到行本体，顺序反了图标就永远拿不到命中。
                    if contains_rect(row, x, y) {
                        return SettingsHit::SshHostRow(index);
                    }
                }
                if contains_rect(geometry.ssh_add_host, x, y) {
                    return SettingsHit::SshAddHost;
                }
                if contains_rect(
                    row_action_rect(
                        geometry.ssh_import_config,
                        scale_factor,
                        STANDARD_ROW_ACTION_W,
                    ),
                    x,
                    y,
                ) {
                    return SettingsHit::SshImportConfig;
                }
                let (row_x, row_y, row_w, row_h) = geometry.hidden_host_row0;
                for index in 0..geometry.hidden_host_count {
                    let rect = (row_x, row_y + index as f32 * row_h, row_w, row_h);
                    if contains_rect(row_action_rect(rect, scale_factor, 80.0), x, y) {
                        return SettingsHit::RestoreHiddenSsh(index);
                    }
                }
            },
            NebulaSettingsSection::Proxy => {
                if contains_rect(
                    ssh_proxy_mode_control(geometry.ssh_proxy_mode, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::SshProxyModeDropdown;
                }
                if geometry.proxy_pane.mode == crate::ssh_proxy::ProxyMode::Custom {
                    let (protocol, address) =
                        ssh_proxy_manual_controls(geometry.ssh_proxy_expand, scale_factor);
                    if contains_rect(protocol, x, y) {
                        return SettingsHit::SshProxyProtocolDropdown;
                    }
                    if contains_rect(address, x, y) {
                        return SettingsHit::SshProxyInput(0);
                    }
                }
                if contains_rect(ssh_proxy_test_button(geometry.ssh_proxy_test, scale_factor), x, y)
                {
                    return SettingsHit::SshProxyTest;
                }
            },
            NebulaSettingsSection::Interaction => {
                if contains_rect(widgets::toggle_rect(geometry.copy_on_select, scale_factor), x, y)
                {
                    return SettingsHit::CopyOnSelectToggle;
                }
                if contains_rect(widgets::combobox_rect(geometry.tab_reveal, scale_factor), x, y) {
                    return SettingsHit::TabRevealDropdown;
                }
                if contains_rect(
                    widgets::combobox_rect(geometry.new_tab_position, scale_factor),
                    x,
                    y,
                ) {
                    return SettingsHit::NewTabPositionDropdown;
                }
                if contains_rect(widgets::toggle_rect(geometry.panel_resize, scale_factor), x, y) {
                    return SettingsHit::PanelResizeToggle;
                }
                if contains_rect(widgets::toggle_rect(geometry.cjk_bold, scale_factor), x, y) {
                    return SettingsHit::CjkBoldToggle;
                }
            },
            NebulaSettingsSection::Keymap => {
                if contains_rect(geometry.keymap_search, x, y) {
                    return SettingsHit::KeymapSearchField;
                }
                let (row_x, _, row_w, row_h) = geometry.keymap_row0;
                let total: usize =
                    geometry.keymap_pane.visible.iter().map(|count| *count as usize).sum();
                for slot in 0..total.min(geometry.keymap_slot_ys.len()) {
                    let rect = (row_x, geometry.keymap_slot_ys[slot], row_w, row_h);
                    if contains_rect(rect, x, y) {
                        return SettingsHit::KeymapRow(slot);
                    }
                }
                let (row_x, row_y, row_w, row_h) = geometry.keymap_readonly_row0;
                for index in 0..geometry.keymap_pane.readonly_visible as usize {
                    let rect = (row_x, row_y + index as f32 * row_h, row_w, row_h);
                    if contains_rect(rect, x, y) {
                        return SettingsHit::KeymapReadonlyRow(index);
                    }
                }
            },
            NebulaSettingsSection::Advanced => {
                if contains_rect(widgets::toggle_rect(geometry.keep_session, scale_factor), x, y) {
                    return SettingsHit::KeepSessionToggle;
                }
                if contains_rect(widgets::toggle_rect(geometry.restore_session, scale_factor), x, y)
                {
                    return SettingsHit::RestoreSessionToggle;
                }
                if SHOW_WEBDAV_SYNC_SETTINGS {
                    for (index, rect) in geometry.sync_rows.iter().enumerate() {
                        // 命中整行都算输入框：行左侧是它的 label，点标签聚焦
                        // 输入是 Windows 设置页的惯例。
                        if contains_rect(*rect, x, y) {
                            return SettingsHit::SyncInput(index);
                        }
                    }
                    if contains_rect(
                        widgets::toggle_rect(geometry.sync_auto_pull, scale_factor),
                        x,
                        y,
                    ) {
                        return SettingsHit::SyncAutoPullToggle;
                    }
                    let [push, pull] = sync_button_rects(geometry.sync_actions, scale_factor);
                    if contains_rect(push, x, y) {
                        return SettingsHit::SyncPushButton;
                    }
                    if contains_rect(pull, x, y) {
                        return SettingsHit::SyncPullButton;
                    }
                }
            },
            NebulaSettingsSection::Backup => {
                for (index, rect) in geometry.backup_rows.iter().enumerate() {
                    if contains_rect(*rect, x, y) {
                        return SettingsHit::BackupSelection(index);
                    }
                }
                let [export, restore] = backup_segment_rects(geometry.backup_segment, scale_factor);
                if contains_rect(export, x, y) {
                    return SettingsHit::BackupExport;
                }
                if contains_rect(restore, x, y) {
                    return SettingsHit::BackupRestore;
                }
            },
        }
    }

    if contains_rect(geometry.popup, x, y) { SettingsHit::Panel } else { SettingsHit::None }
}

// ---- rendering ----

/// Renderer-owned snapshot for one SSH destination. Keeping this small model
/// separate from `Display` means the settings page never reaches into runtime
/// collections while drawing, and the same host ordering can be reused by
/// the sidebar and command palette without UI-specific branching.
pub(super) struct SshSettingsHost {
    pub(super) destination: String,
    pub(super) label: String,
    pub(super) icon: String,
    pub(super) pinned: bool,
}

/// A per-frame snapshot of the display state the settings render reads. Owns its
/// data (notably the wallpaper path) so the caller can hand it in by reference
/// while still borrowing `&mut renderer` for [`draw_text`].
pub(super) struct SettingsView {
    /// The active tab's content card in physical pixels. Settings fills this
    /// area like any other tab instead of inventing a second floating window.
    pub(super) area: (f32, f32, f32, f32),
    pub(super) language_preference: LanguagePreference,
    pub(super) language: UiLanguage,
    pub(super) section: NebulaSettingsSection,
    pub(super) hover: SettingsHit,
    /// Settings control currently held by the primary mouse button. This is
    /// separate from hover so toggles can reproduce the HTML reference's
    /// pressed stretch without making every row a click target.
    pub(super) pressed: SettingsHit,
    /// Independent travel/stretch/color/hover channels for every switch.
    pub(super) toggle_motion: [widgets::ToggleMotion; SETTINGS_TOGGLE_COUNT],
    pub(super) theme: NebulaTheme,
    pub(super) follow_system_theme: bool,
    pub(super) ghost: bool,
    pub(super) accept: AcceptKey,
    /// Pre-rendered "默认 Shell" value (icon + name) — resolved by `Display`
    /// from the rich `shell_id` when set, else the 2-value enum label.
    pub(super) shell_label: String,
    /// Which combobox is expanded, if any (floating option list).
    pub(super) dropdown: Option<SettingsDropdown>,
    /// Detected shells for the picker (cached once per process).
    pub(super) shells: Vec<(String, String, String)>, // (id, name, program)
    pub(super) shell_id: Option<String>,
    pub(super) startup_directory: Option<String>,
    pub(super) providers: Vec<crate::ai_providers::AiProvider>,
    pub(super) active_provider_id: String,
    pub(super) provider_inputs: [String; 6],
    pub(super) provider_cursors: [text_field::TextCursor; 6],
    pub(super) provider_focus: Option<usize>,
    pub(super) provider_status: Option<(String, bool)>,
    pub(super) font_family: String,
    /// Current terminal font size in LOGICAL px, for the spinner value box.
    pub(super) font_size_px: f32,
    /// Private families plus Maple; the import action is rendered separately.
    pub(super) fonts: Vec<String>,
    pub(super) font_notice: Option<String>,
    /// 字体目录的「显示全部」临时过滤是否开启。
    pub(super) font_show_all: bool,
    /// 字体目录搜索串；长在弹层顶部那个搜索框里。
    pub(super) font_query: String,
    /// 搜索框的光标与选区。下沉到 [`super::ui::text_field`] 的同一套模型，
    /// 新加的输入框直接继承，不必再实现一遍。
    pub(super) font_query_cursor: text_field::TextCursor,
    /// 字体弹层的候选滚动偏移；搜索框占第 0 行，滚动只移动其余候选。
    pub(super) font_popup_scroll: usize,
    /// 字体弹层滚动条是否正被拖拽（thumb 高亮用）。
    pub(super) font_popup_dragging: bool,
    /// 非等宽族的小写名集合；下拉行据此追加比例字体警告。
    pub(super) font_proportional: std::collections::HashSet<String>,
    /// Persistent soft-deleted destinations. Rows provide a discoverable
    /// recovery path after the short Undo bar has expired.
    pub(super) hidden_hosts: Vec<String>,
    /// SSH destinations copied from the sidebar's merged, ordered snapshot.
    pub(super) ssh_hosts: Vec<SshSettingsHost>,
    pub(super) fetch: bool,
    pub(super) powerline: bool,
    pub(super) keep_session: bool,
    /// 高级·会话：启动时恢复上次的标签。
    pub(super) restore_session: bool,
    pub(super) blur: bool,
    pub(super) opacity: f32,
    /// Which opacity slider is mid-drag, for thumb-dot grow feedback.
    pub(super) dragging_opacity: Option<SettingsOpacityTarget>,
    pub(super) cursor_shape: CursorShape,
    pub(super) cursor_blink: bool,
    pub(super) copy_on_select: bool,
    /// 交互·「拖拽调节侧栏」总开关。
    pub(super) panel_resize: bool,
    pub(super) cjk_bold_regular: bool,
    pub(super) tab_reveal: TabRevealMotion,
    pub(super) density: super::ui::tokens::Density,
    pub(super) new_tab_position: NewTabPosition,
    pub(super) cell_width_mode: CellWidthMode,
    /// Live-preview colors: the ACTUAL terminal background/foreground the
    /// grid would use right now (custom background wins over the theme).
    pub(super) preview_bg: Rgb,
    pub(super) preview_fg: Rgb,
    pub(super) background: Option<Rgb>,
    /// 背景色浮层的 16 进制草稿（形如 `#0A0C18`）与输入聚焦态。
    pub(super) bg_hex_input: String,
    pub(super) bg_hex_active: bool,
    pub(super) background_image: Option<String>,
    pub(super) background_image_opacity: f32,
    pub(super) background_image_fit: BackgroundImageFit,
    pub(super) background_image_alignment: BackgroundImageAlignment,
    pub(super) background_image_cover_chrome: bool,
    /// Content scroll offset in scaled px (0 = top). Owned by `Display`,
    /// clamped there against [`settings_max_scroll`].
    pub(super) scroll: f32,
    /// 按键映射: per editable row `(display combo, customized)`; `None` =
    /// unbound. Precomputed by `Display` from the override + default tables.
    pub(super) keymap: Vec<Option<(String, bool)>>,
    /// 快速终端行使用独立的全局快捷键字符串。
    pub(super) quick_terminal_hotkey: String,
    pub(super) quick_hotkey_error: Option<String>,
    /// Row currently capturing a new combo, if any.
    pub(super) keymap_capture: Option<usize>,
    /// 捕获态按住的修饰键前缀（"Ctrl+"），实时回显。
    pub(super) keymap_capture_preview: String,
    /// 按键映射页搜索：查询串 + 聚焦态；过滤后可见行的 flat 下标
    /// （编辑组 / 只读组分开），由 Display 用同一过滤谓词预计算。
    pub(super) keymap_query: String,
    pub(super) keymap_query_cursor: text_field::TextCursor,
    pub(super) keymap_search_focus: bool,
    pub(super) keymap_visible: Vec<usize>,
    pub(super) keymap_readonly_visible: Vec<usize>,
    /// 与 flat 行对齐的冲突标记 + 预排好的冲突提示句（None = 无冲突）。
    pub(super) keymap_clash_rows: Vec<bool>,
    pub(super) keymap_clash_note: Option<String>,
    /// 高级→同步：四个输入草稿（url、用户名、WebDAV 密码、E2E 口令）。
    pub(super) sync_inputs: [String; 4],
    /// 聚焦的同步输入框下标（0..4）。
    pub(super) sync_focus: Option<usize>,
    pub(super) sync_auto_pull: bool,
    /// 凭据管理器里已有 [密码, 口令]，决定密码框占位文案。
    pub(super) sync_secret_set: [bool; 2],
    /// 最近一次同步动作的结果 `(message, is_error)`。
    pub(super) sync_status: Option<(String, bool)>,
    pub(super) sync_busy: bool,
    /// 网络页：[手动地址, 绕过列表, 自定义命令] 输入原文 + 聚焦下标。
    pub(super) ssh_proxy_mode: crate::ssh_proxy::ProxyMode,
    pub(super) ssh_proxy_inputs: [String; 3],
    pub(super) ssh_proxy_cursors: [text_field::TextCursor; 3],
    pub(super) ssh_proxy_focus: Option<usize>,
    pub(super) ssh_proxy_protocol: ManualProxyProtocol,
    pub(super) ssh_proxy_choice: ProxyChoice,
    pub(super) local_proxies: Vec<crate::ssh_proxy::LocalProxyEndpoint>,
    pub(super) proxy_scanning: bool,
    /// 「跟随系统」当前读到的代理：`(URL, 来自注册表)`。None = 系统未启用。
    /// Display 在进网络页 / 切模式时刷新缓存；渲染只读，不做系统调用。
    pub(super) system_proxy_probe: Option<(String, bool)>,
    /// 当前网络设置的最近一次真实出网测试结果。
    pub(super) proxy_test_status: ProxyTestStatus,
    /// 每主机覆盖行：`(显示名, 摘要, ssh_hosts 下标)`，随视图快照重建。
    pub(super) ssh_proxy_overrides: Vec<(String, String, usize)>,
    pub(super) backup_selection: BackupSelection,
    pub(super) backup_status: Option<(String, bool)>,
}

/// 同步行右侧的输入框矩形（quad/text/hit 三处共用）。行左侧留给标签。
fn sync_input_rect((rx, ry, rw, rh): (f32, f32, f32, f32), scale: f32) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    if rh >= s(56.0) {
        return (rx + s(16.0), ry + rh - s(38.0), (rw - s(32.0)).max(s(1.0)), s(32.0));
    }
    let w = rw * 0.56;
    let h = rh - s(12.0);
    (rx + rw - s(16.0) - w, ry + (rh - h) / 2.0, w, h)
}

/// 原型的展开组件不带左侧行标签：跳板和命令占满可用宽度；手动填写把
/// 同一行拆成固定协议选择器与自适应地址输入框。
fn ssh_proxy_expand_control(
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    scale: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let h = rh - s(12.0);
    (rx, ry + (rh - h) / 2.0, rw, h)
}

/// 网络代理三态文字很短，使用紧凑下拉，避免通用 220px 控件在这一行显得
/// 空旷。绘制、命中、弹层锚点和文字都复用这个矩形。
fn ssh_proxy_mode_control(
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    scale: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    if rh >= s(56.0) {
        return (rx + s(16.0), ry + rh - s(38.0), (rw - s(32.0)).max(s(1.0)), s(32.0));
    }
    let w = s(156.0).min(rw * 0.38).max(s(132.0));
    let h = s(32.0);
    (rx + rw - s(16.0) - w, ry + (rh - h) * 0.5, w, h)
}

/// 测试横幅右侧动作。横幅整块承载状态，只有这个按钮触发联网，避免用户
/// 点击错误文案时无意重复发起请求。
fn ssh_proxy_test_button(
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    scale: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let w = s(108.0).min(rw * 0.34).max(s(88.0));
    let h = s(30.0);
    (rx + rw - s(12.0) - w, ry + (rh - h) * 0.5, w, h)
}

fn ssh_proxy_manual_controls(
    row: (f32, f32, f32, f32),
    scale: f32,
) -> ((f32, f32, f32, f32), (f32, f32, f32, f32)) {
    let s = |v: f32| v * scale;
    let (x, y, w, h) = sync_input_rect(row, scale);
    let gap = s(8.0);
    let protocol_w = s(112.0).min((w - gap) * 0.38);
    let protocol = (x, y, protocol_w, h);
    let address = (x + protocol_w + gap, y, (w - protocol_w - gap).max(s(80.0)), h);
    (protocol, address)
}

/// 同步输入框的展示内容：`(文本, 是否占位, 列数)`。密码/口令显示为
/// 掩码点；超宽时截尾部显示（编辑总发生在末尾）。列数供 caret 定位。
fn sync_input_display(view: &SettingsView, index: usize, max_cols: usize) -> (String, bool, usize) {
    let language = view.language;
    let raw = &view.sync_inputs[index];
    if raw.is_empty() {
        let text = match index {
            0 => language
                .pick("https://dav.example.com/nebula.sync", "https://dav.example.com/nebula.sync"),
            1 => language.pick("WebDAV 用户名", "WebDAV username"),
            2 if view.sync_secret_set[0] => {
                language.pick("已保存（输入以更换）", "Saved (type to replace)")
            },
            3 if view.sync_secret_set[1] => {
                language.pick("已保存（输入以更换）", "Saved (type to replace)")
            },
            _ => language.pick("未设置", "Not set"),
        };
        return (text.to_owned(), true, 0);
    }
    if index >= 2 {
        let dots = raw.chars().count().min(24);
        return ("●".repeat(dots), false, dots);
    }
    // 从尾部收集不超过 max_cols 列的字符（中文占 2 列）。
    let (text, cols) = text_tail(raw, max_cols);
    (text, false, cols)
}

/// 从尾部收集不超过 `max_cols` 列的字符（中文占 2 列）；编辑总发生在
/// 末尾，超宽时截头部。返回 `(展示文本, 实际列数)`，列数供 caret 定位。
fn text_tail(raw: &str, max_cols: usize) -> (String, usize) {
    let mut cols = 0usize;
    let mut chars: Vec<char> = Vec::new();
    for ch in raw.chars().rev() {
        let w = ch.width().unwrap_or(1).max(1);
        if cols + w > max_cols {
            break;
        }
        cols += w;
        chars.push(ch);
    }
    chars.reverse();
    (chars.into_iter().collect(), cols)
}

/// SSH 代理输入框的展示内容：`(文本, 是否占位, 列数)`。与
/// [`sync_input_display`] 同一契约，行矩形也共用 [`sync_input_rect`]。
/// 聚焦时窗口跟随光标：光标退进被截掉的头部时改从光标处向后开窗，
/// 保证 caret 永远落在可见列里。
fn ssh_proxy_input_display(
    view: &SettingsView,
    index: usize,
    max_cols: usize,
) -> (String, bool, usize, usize) {
    let raw = &view.ssh_proxy_inputs[index];
    if raw.is_empty() {
        let text = match index {
            // 无前缀地址就能用（自动按 socks5），placeholder 直接示范最短
            // 形态；System 模式下地址行整个不渲染，无需在此分支。
            0 => "127.0.0.1:7890",
            1 => view.language.pick("例：10.0.0.0, .internal", "e.g. 10.0.0.0, .internal"),
            _ => "corkscrew proxy.corp 8080 %h %p",
        };
        return (text.to_owned(), true, 0, 0);
    }
    if view.ssh_proxy_focus == Some(index) {
        let caret = view.ssh_proxy_cursors[index].caret(raw);
        let total = raw.chars().count();
        let (tail, cols) = text_tail(raw, max_cols);
        let hidden = total - tail.chars().count();
        if caret >= hidden {
            return (tail, false, cols, hidden);
        }
        // 光标在尾窗口之外：从光标处向后开窗（光标贴左缘）。
        let mut cols = 0usize;
        let mut text = String::new();
        for ch in raw.chars().skip(caret) {
            let w = ch.width().unwrap_or(1).max(1);
            if cols + w > max_cols {
                break;
            }
            cols += w;
            text.push(ch);
        }
        return (text, false, cols, caret);
    }
    let (text, cols) = text_tail(raw, max_cols);
    let hidden = raw.chars().count() - text.chars().count();
    (text, false, cols, hidden)
}

fn provider_input_display(
    view: &SettingsView,
    index: usize,
    max_cols: usize,
) -> (String, bool, usize) {
    let raw = &view.provider_inputs[index];
    if raw.is_empty() {
        let placeholder = if index == 5 {
            match view.providers.iter().find(|provider| provider.id == view.active_provider_id) {
                Some(provider) if !provider.kind.requires_api_key() => {
                    view.language.pick("本地服务无需 API Key", "No API key required").to_owned()
                },
                Some(provider) if provider.api_key_set => format!(
                    "{}  {}",
                    provider.api_key_hint,
                    view.language.pick("（输入以更换）", "(type to replace)")
                ),
                _ => view.language.pick("输入 API Key", "Enter API key").to_owned(),
            }
        } else {
            view.language.pick("未设置", "Not set").to_owned()
        };
        return (placeholder, true, 0);
    }

    let source = if index == 5 { "●".repeat(raw.chars().count()) } else { raw.clone() };
    let caret = view.provider_cursors[index].caret(raw);
    let total = source.chars().count();
    let (tail, _) = text_tail(&source, max_cols);
    let hidden = total - tail.chars().count();
    if view.provider_focus != Some(index) || caret >= hidden {
        return (tail, false, hidden);
    }
    let mut cols = 0usize;
    let mut display = String::new();
    for ch in source.chars().skip(caret) {
        let width = ch.width().unwrap_or(1).max(1);
        if cols + width > max_cols {
            break;
        }
        cols += width;
        display.push(ch);
    }
    (display, false, caret)
}

/// 视图 → 网络页几何输入。所有 settings_geometry 调用点共用同一份推导，
/// 防止命中与绘制对模式的理解不一致。
fn proxy_pane_state(view: &SettingsView) -> ProxyPaneState {
    ProxyPaneState {
        mode: view.ssh_proxy_mode,
        choice: view.ssh_proxy_choice,
        found_count: view.local_proxies.len(),
        scanning: view.proxy_scanning,
        override_count: view.ssh_proxy_overrides.len(),
    }
}

/// 视图 → 按键映射页几何输入（每组可见行数从 flat 下标反推）。
fn keymap_pane_state_view(view: &SettingsView) -> KeymapPaneState {
    let mut pane = KeymapPaneState {
        readonly_visible: view.keymap_readonly_visible.len() as u8,
        clash: view.keymap_clash_note.is_some(),
        ..Default::default()
    };
    let mut start = 0usize;
    for (group, (.., count)) in keymap::GROUPS.iter().enumerate() {
        let end = start + count;
        pane.visible[group] =
            view.keymap_visible.iter().filter(|flat| (start..end).contains(*flat)).count() as u8;
        start = end;
    }
    pane
}

/// 动作行的 [立即推送, 立即拉取] 按钮矩形。
fn sync_button_rects(
    (rx, ry, _, rh): (f32, f32, f32, f32),
    scale: f32,
) -> [(f32, f32, f32, f32); 2] {
    let s = |v: f32| v * scale;
    let w = s(150.0);
    let h = rh - s(12.0);
    let y = ry + (rh - h) / 2.0;
    [(rx, y, w, h), (rx + w + s(12.0), y, w, h)]
}

/// Export/restore segmented control from the backup prototype. The hit boxes
/// are the same inner slots that are painted, including the 3px outer inset.
fn backup_segment_rects(
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    scale: f32,
) -> [(f32, f32, f32, f32); 2] {
    let inset = 3.0 * scale;
    let inner_w = (rw - inset * 2.0).max(0.0);
    let slot_w = inner_w * 0.5;
    [
        (rx + inset, ry + inset, slot_w, rh - inset * 2.0),
        (rx + inset + slot_w, ry + inset, slot_w, rh - inset * 2.0),
    ]
}

fn backup_item_selected(selection: BackupSelection, index: usize) -> bool {
    match index {
        0 => selection.appearance,
        1 => selection.config,
        2 => selection.ssh,
        3 => selection.sync,
        4 => selection.assistant,
        5 => selection.session,
        6 => selection.directory_history,
        7 => selection.command_history,
        _ => selection.fonts,
    }
}

/// 键位行的 keycap 矩形（quad 与 text 两个 pass 共用同一几何）。
fn keymap_keycap_rect(
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    label: &str,
    cell_w: f32,
    scale: f32,
) -> (f32, f32, f32, f32) {
    let s = |v: f32| v * scale;
    let cols: usize = label.chars().map(|c| c.width().unwrap_or(0)).sum();
    let cap_w = cols as f32 * cell_w + s(24.0);
    let cap_h = rh - s(14.0);
    (rx + rw - s(16.0) - cap_w, ry + (rh - cap_h) / 2.0, cap_w, cap_h)
}

/// 键位行右侧的展示文本：(文本, 是否自定义, 是否有绑定)。捕获态的
/// 「按下新按键…」由调用侧替换。
fn keymap_row_value(view: &SettingsView, index: usize) -> (String, bool, bool) {
    if view.keymap_capture == Some(index) {
        // 按住修饰键时实时回显（"Ctrl+…"），否则给占位 + 取消提示。
        let text = if view.keymap_capture_preview.is_empty() {
            view.language
                .pick("按下新按键…（Esc 取消）", "Press new keys… (Esc cancels)")
                .to_owned()
        } else {
            format!("{}…", view.keymap_capture_preview)
        };
        return (text, false, false);
    }
    if index == keymap::QUICK_TERMINAL_ROW {
        return (
            keymap::display_stored_combo(&view.quick_terminal_hotkey),
            view.quick_terminal_hotkey != keymap::DEFAULT_QUICK_TERMINAL_HOTKEY,
            true,
        );
    }
    let action_index = index - 1;
    match view.keymap.get(action_index).and_then(|slot| slot.as_ref()) {
        Some((combo, customized)) => (combo.clone(), *customized, true),
        None => (view.language.pick("未绑定", "Unbound").to_owned(), false, false),
    }
}

/// Preview sample layout shared by the quad pass (cursor demo) and the text
/// pass (sample lines): 16px inner pad, 1.4× line pitch.
fn preview_line_y(top: f32, cell_h: f32, line: f32, scale: f32) -> f32 {
    top + 16.0 * scale + line * (cell_h * 1.4)
}
/// Columns of "❯ " before the demo cursor on the preview's prompt line.
const PREVIEW_PROMPT_COLS: usize = 2;

fn dropdown_selected_index(view: &SettingsView, dropdown: SettingsDropdown) -> Option<usize> {
    match dropdown {
        SettingsDropdown::Shell => {
            view.shells.iter().position(|(id, _, _)| view.shell_id.as_deref() == Some(id.as_str()))
        },
        // 加一：弹层第 0 行是搜索框，候选整体下移一行。
        SettingsDropdown::Font => {
            // 多级 fallback 列表按主族高亮（issue #33）。
            let primary =
                crate::renderer::text::glyph_cache::primary_font_family(&view.font_family);
            view.fonts.iter().position(|family| family == primary).map(|slot| slot + 1)
        },
        SettingsDropdown::BackgroundFit => {
            BACKGROUND_FIT_OPTIONS.iter().position(|fit| *fit == view.background_image_fit)
        },
        SettingsDropdown::BackgroundAlignment => BACKGROUND_ALIGNMENT_OPTIONS
            .iter()
            .position(|alignment| *alignment == view.background_image_alignment),
        SettingsDropdown::Language => {
            LANGUAGE_OPTIONS.iter().position(|preference| *preference == view.language_preference)
        },
        SettingsDropdown::Accept => ACCEPT_OPTIONS.iter().position(|key| *key == view.accept),
        SettingsDropdown::TabReveal => {
            TAB_REVEAL_OPTIONS.iter().position(|motion| *motion == view.tab_reveal)
        },
        SettingsDropdown::Density => DENSITY_OPTIONS.iter().position(|d| *d == view.density),
        SettingsDropdown::NewTabPosition => {
            NEW_TAB_POSITION_OPTIONS.iter().position(|position| *position == view.new_tab_position)
        },
        SettingsDropdown::CellWidthMode => {
            CELL_WIDTH_MODE_OPTIONS.iter().position(|mode| *mode == view.cell_width_mode)
        },
        SettingsDropdown::CursorShape => {
            CURSOR_SHAPE_OPTIONS.iter().position(|shape| *shape == view.cursor_shape)
        },
        SettingsDropdown::SshProxyMode => {
            SSH_PROXY_MODE_OPTIONS.iter().position(|mode| *mode == view.ssh_proxy_mode)
        },
        SettingsDropdown::SshProxyProtocol => MANUAL_PROXY_PROTOCOL_OPTIONS
            .iter()
            .position(|protocol| *protocol == view.ssh_proxy_protocol),
        SettingsDropdown::SshJumpHost => crate::ssh_proxy::jump_target(&view.ssh_proxy_inputs[0])
            .and_then(|target| view.ssh_hosts.iter().position(|host| host.destination == target)),
        SettingsDropdown::BackgroundColor => view
            .background
            .and_then(|current| BACKGROUND_SWATCHES.iter().position(|color| *color == current)),
    }
}

fn dropdown_hover_index(hover: SettingsHit, dropdown: SettingsDropdown) -> Option<usize> {
    match (dropdown, hover) {
        (SettingsDropdown::Shell, SettingsHit::ShellPickerRow(index)) => Some(index),
        (SettingsDropdown::Font, SettingsHit::FontPickerRow(index)) => Some(index + 1),
        (SettingsDropdown::BackgroundFit, SettingsHit::FitOption(index)) => Some(index),
        (SettingsDropdown::BackgroundAlignment, SettingsHit::AlignOption(index)) => Some(index),
        (SettingsDropdown::Language, SettingsHit::Language(preference)) => {
            LANGUAGE_OPTIONS.iter().position(|option| *option == preference)
        },
        (SettingsDropdown::Accept, SettingsHit::AcceptOption(index)) => Some(index),
        (SettingsDropdown::TabReveal, SettingsHit::TabRevealOption(index)) => Some(index),
        (SettingsDropdown::Density, SettingsHit::DensityOption(index)) => Some(index),
        (SettingsDropdown::NewTabPosition, SettingsHit::NewTabPositionOption(index)) => Some(index),
        (SettingsDropdown::CellWidthMode, SettingsHit::CellWidthModeOption(index)) => Some(index),
        (SettingsDropdown::CursorShape, SettingsHit::CursorShapeOption(index)) => Some(index),
        (SettingsDropdown::SshProxyMode, SettingsHit::SshProxyModeOption(index)) => Some(index),
        (SettingsDropdown::SshProxyProtocol, SettingsHit::SshProxyProtocolOption(index)) => {
            Some(index)
        },
        (SettingsDropdown::SshJumpHost, SettingsHit::SshJumpHostOption(index)) => Some(index),
        _ => None,
    }
}

/// Push the Settings tab's background quads, navigation, rows and controls.
pub(super) fn push_quads(
    view: &SettingsView,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |v: f32| v * scale;
    let sk = settings_skin(view.theme);

    let mut geometry = settings_geometry(
        size,
        scale,
        view.area,
        view.scroll,
        view.hidden_hosts.len(),
        view.ssh_hosts.len(),
        view.density,
        proxy_pane_state(view),
        keymap_pane_state_view(view),
    );
    fit_provider_rows(&mut geometry, view.providers.len());
    let (px, py, pw, ph) = geometry.popup;
    // Scrolled content is clipped EXACTLY at the viewport edges: quads that
    // cross the fixed header separator or the popup's bottom edge are cut at
    // the line via [`UiQuad::clip_y`] (uv-remapped, so rounded corners and
    // glows are truncated mid-shape instead of bleeding past the hairline).
    let clip_top = geometry.content_top;
    let clip_bot = py + ph - s(6.0);
    let clip = |quads: &mut Vec<UiQuad>, quad: UiQuad| {
        if let Some(quad) = quad.clip_y(clip_top, clip_bot) {
            quads.push(quad);
        }
    };
    // 通用组件（widgets）不感知视口裁剪：输出先落到 staged，再统一过 clip。
    let mut staged: Vec<UiQuad> = Vec::new();

    // The page is flush with the active tab card. No veil, drop shadow or
    // second window outline: depth belongs to the app shell, not this page.
    quads.push(UiQuad::solid(px, py, pw, ph, s(12.0), sk.panel));
    // Sidebar and content use spacing alone; no structural divider lines.
    let section = view.section;
    for (nav_section, nx, ny, nw, nh) in geometry.nav {
        if nav_section == NebulaSettingsSection::Backup && !SHOW_BACKUP_SETTINGS {
            continue;
        }
        if nav_section == section {
            // 2026-08-09 对齐原型 .nav-item.on：选中 = accent_soft，与侧栏
            // tab、网络页单选行同 token。回滚: sk.surface（中性档）。
            quads.push(UiQuad::solid(nx, ny, nw, nh, s(8.0), sk.accent_soft));
        } else if view.hover == SettingsHit::Nav(nav_section) {
            quads.push(UiQuad::solid(nx, ny, nw, nh, s(8.0), sk.hover));
        }
        let icon_ink = if nav_section == section {
            Rgba::new(sk.ink_strong.r, sk.ink_strong.g, sk.ink_strong.b, 235)
        } else if view.hover == SettingsHit::Nav(nav_section) {
            Rgba::new(sk.icon_hover.r, sk.icon_hover.g, sk.icon_hover.b, 230)
        } else {
            Rgba::new(sk.icon.r, sk.icon.g, sk.icon.b, 190)
        };
        // 空心图标（代理的中继环）挖空用的必须是行的**有效底色**：面板色
        // 与选中/悬浮药丸（半透明）合成后的结果，猜错环心就是一块色斑。
        let icon_cutout = if nav_section == section {
            // 选中底换 accent_soft 后挖空必须跟着换，否则环心是旧色斑。
            // 回滚: icons::blend_over(sk.panel, sk.surface)
            icons::blend_over(sk.panel, sk.accent_soft)
        } else if view.hover == SettingsHit::Nav(nav_section) {
            icons::blend_over(sk.panel, sk.hover)
        } else {
            sk.panel
        };
        let icon_x = if geometry.compact_nav { nx + (nw - s(18.0)) * 0.5 } else { nx + s(10.0) };
        icons::push_settings_nav_icon(
            quads,
            nav_icon(nav_section),
            (icon_x, ny + s(7.0), s(18.0), s(18.0)),
            scale,
            icon_ink,
            icon_cutout,
        );
    }

    // Reset: a quiet ghost button in the header. SSH intentionally has no
    // page-level "Upgrade" action; host management is the complete surface.
    if !matches!(section, NebulaSettingsSection::Ssh | NebulaSettingsSection::Providers) {
        let (rx, ry, rw, rh) = geometry.reset;
        quads.push(UiQuad::solid(rx, ry, rw, rh, s(8.0), sk.surface));
        let hovered = view.hover == SettingsHit::Reset;
        if hovered {
            quads.push(UiQuad::solid(rx, ry, rw, rh, s(8.0), sk.hover));
        }
    }

    // Settings groups are unframed. Natural row spacing carries hierarchy;
    // controls provide their own local hover feedback and click targets.
    let group_frame = |_quads: &mut Vec<UiQuad>, _first_row, _rows: usize| {};
    let row_hover = |_quads: &mut Vec<UiQuad>, _rect, _hovered: bool| {};
    let action_button = |quads: &mut Vec<UiQuad>, row, logical_w: f32, hovered: bool| {
        let rect = row_action_rect(row, scale, logical_w);
        clip(
            quads,
            UiQuad::solid(
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                rect.3 * 0.5,
                if hovered { sk.hover } else { sk.surface },
            ),
        );
    };
    // Widget wrappers: stage → viewport-clip → push. Every multi-option row
    // shares ONE combobox component (user ruling 2026-07-23), hover/press
    // feedback included, so no page ever hand-rolls its own control again.
    let combobox = |quads: &mut Vec<UiQuad>,
                    staged: &mut Vec<UiQuad>,
                    row,
                    hot: bool,
                    open: bool| {
        widgets::push_combobox(staged, widgets::combobox_rect(row, scale), scale, &sk, hot, open);
        for quad in staged.drain(..) {
            clip(quads, quad);
        }
    };
    let slider = |quads: &mut Vec<UiQuad>, staged: &mut Vec<UiQuad>, hit, value: f32, hot: bool| {
        widgets::push_slider(staged, hit, value, scale, &sk, hot);
        for quad in staged.drain(..) {
            clip(quads, quad);
        }
    };
    let toggle = |quads: &mut Vec<UiQuad>,
                  staged: &mut Vec<UiQuad>,
                  row,
                  hit: SettingsHit,
                  on: bool,
                  _hot: bool,
                  _pressed: bool| {
        let motion = settings_toggle_slot(hit)
            .map_or_else(|| widgets::ToggleMotion::settled(on), |index| view.toggle_motion[index]);
        widgets::push_toggle(staged, row, scale, &sk, motion);
        for quad in staged.drain(..) {
            clip(quads, quad);
        }
    };

    match section {
        NebulaSettingsSection::Appearance => {
            // ---- Live preview card: configure → immediately see ----
            // Terminal colors, font family/size (text pass), and the demo
            // cursor all read the same state the real grid uses.
            {
                let (vx, vy, vw, vh) = geometry.preview;
                clip(
                    quads,
                    UiQuad::solid(
                        vx - s(1.0),
                        vy - s(1.0),
                        vw + s(2.0),
                        vh + s(2.0),
                        s(11.0),
                        sk.hairline,
                    ),
                );
                clip(
                    quads,
                    UiQuad::solid(
                        vx,
                        vy,
                        vw,
                        vh,
                        s(10.0),
                        Rgba::new(view.preview_bg.r, view.preview_bg.g, view.preview_bg.b, 255),
                    ),
                );
                // Demo cursor on the prompt line, driven by the REAL shape +
                // blink settings (shares the UI caret's 500ms phase).
                if !view.cursor_blink || super::caret_blink_on() {
                    let cell_w = size.cell_width();
                    let cell_h = size.cell_height();
                    let cursor_x = vx + s(16.0) + PREVIEW_PROMPT_COLS as f32 * cell_w;
                    let cursor_y = preview_line_y(vy, cell_h, 2.0, scale);
                    let ink =
                        Rgba::new(view.preview_fg.r, view.preview_fg.g, view.preview_fg.b, 235);
                    let bg =
                        Rgba::new(view.preview_bg.r, view.preview_bg.g, view.preview_bg.b, 255);
                    let stroke = (1.5 * scale).max(1.0);
                    let beam_w = (2.0 * scale).max(1.0);
                    match view.cursor_shape {
                        CursorShape::Beam => {
                            clip(
                                quads,
                                UiQuad::solid(cursor_x, cursor_y, beam_w, cell_h, 0.0, ink),
                            );
                        },
                        CursorShape::Underline => {
                            clip(
                                quads,
                                UiQuad::solid(
                                    cursor_x,
                                    cursor_y + cell_h - beam_w,
                                    cell_w,
                                    beam_w,
                                    0.0,
                                    ink,
                                ),
                            );
                        },
                        CursorShape::HollowBlock => {
                            clip(
                                quads,
                                UiQuad::solid(cursor_x, cursor_y, cell_w, cell_h, 0.0, ink),
                            );
                            clip(
                                quads,
                                UiQuad::solid(
                                    cursor_x + stroke,
                                    cursor_y + stroke,
                                    cell_w - 2.0 * stroke,
                                    cell_h - 2.0 * stroke,
                                    0.0,
                                    bg,
                                ),
                            );
                        },
                        CursorShape::Hidden => {},
                        CursorShape::Block => {
                            clip(
                                quads,
                                UiQuad::solid(cursor_x, cursor_y, cell_w, cell_h, 0.0, ink),
                            );
                        },
                    }
                }
            }

            // Theme cards are MINIATURE TERMINAL WINDOWS, each painted in its
            // own theme's colors: shell_bg window shell, a rounded term_bg
            // "terminal card" floating inside it (the real window's model,
            // shrunk), and fake prompt/output lines in the theme's own inks.
            // A flat panel swatch only answered "what color is the chrome";
            // the mini window answers what the picker is really asked: how do
            // background, text and highlights look TOGETHER. (2026-07-28 用户
            // 裁定；窗控红绿灯明确不画——各平台窗控样式不同，预览不预设
            // 任何一家。) Selection = accent ring + halo; hover = 2px lift —
            // no wash, so the preview colors stay true.
            for (theme, ox, oy, ow, oh) in geometry.options {
                let selected = theme == view.theme;
                let hovered = view.hover == SettingsHit::Theme(theme);
                let lift = if hovered && !selected { s(2.0) } else { 0.0 };
                let oy = oy - lift;
                let stroke = if selected {
                    Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255)
                } else {
                    sk.hairline
                };
                let stroke_w = if selected { s(2.0) } else { s(1.0) };
                if selected {
                    // Selected card glows softly: the accent ring plus a
                    // diffuse halo, per the design sheet's lit-control look.
                    clip(
                        quads,
                        UiQuad::glow(
                            ox - s(14.0),
                            oy - s(14.0),
                            ow + s(28.0),
                            oh + s(28.0),
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 66),
                        ),
                    );
                } else if hovered {
                    // Hover halo: same shape, fainter — enough 辉光 to read
                    // as "lit up" without competing with the selected card.
                    clip(
                        quads,
                        UiQuad::glow(
                            ox - s(12.0),
                            oy - s(10.0),
                            ow + s(24.0),
                            oh + s(26.0),
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 38),
                        ),
                    );
                }
                clip(
                    quads,
                    UiQuad::solid(
                        ox - stroke_w,
                        oy - stroke_w,
                        ow + 2.0 * stroke_w,
                        oh + 2.0 * stroke_w,
                        s(9.0),
                        stroke,
                    ),
                );
                let p = theme.palette();
                let ink = theme.card_ink();
                let shell = Rgba::new(p.shell_bg.r, p.shell_bg.g, p.shell_bg.b, 255);
                clip(quads, UiQuad::solid(ox, oy, ow, oh, s(8.0), shell));
                // Inner terminal card. The taller top margin reads as a title
                // bar without drawing one.
                let (tx, ty) = (ox + s(10.0), oy + s(14.0));
                let (tw, th) = (ow - s(20.0), oh - s(22.0));
                let term = Rgba::new(p.term_bg.r, p.term_bg.g, p.term_bg.b, 255);
                clip(quads, UiQuad::solid(tx, ty, tw, th, s(5.0), term));
                // Three fake lines as pill bars: a prompt command in fg (the
                // `❯` itself is a real glyph, drawn in the text pass), two
                // highlight tokens, a dim trailing line. Widths are fractions
                // of the card so narrow cards keep the proportions.
                let bar_h = s(3.0);
                let line_x = tx + s(8.0);
                let line_pitch = s(11.0);
                let y0 = ty + s(8.0);
                let inner_w = tw - s(16.0);
                let bar = |x: f32, y: f32, w: f32, c: Rgb, a: u8| {
                    UiQuad::solid(x, y, w, bar_h, s(1.5), Rgba::new(c.r, c.g, c.b, a))
                };
                let prompt_w = s(9.0); // room the text-pass ❯ occupies
                let cmd_w = inner_w * 0.42;
                clip(quads, bar(line_x + prompt_w, y0, cmd_w, ink.fg, 230));
                // A block caret hugging the command's end, in the theme's
                // accent — the one "alive" spark on the card.
                let acc = theme.accent();
                clip(
                    quads,
                    UiQuad::solid(
                        line_x + prompt_w + cmd_w + s(3.0),
                        y0 - s(2.0),
                        s(3.0),
                        bar_h + s(4.0),
                        s(1.0),
                        Rgba::new(acc.r, acc.g, acc.b, 255),
                    ),
                );
                // 第二行是各主题的品牌双色（edge_l→edge_r，即侧栏品牌
                // 渐变对）：固定 ANSI green/blue 让 4 张暗卡 3 张亮卡两两
                // 同色（2026-07-28 用户反馈「预览颜色都一样」），身份色带
                // 才是卡片间唯一稳定的区分资产。
                clip(
                    quads,
                    bar(
                        line_x,
                        y0 + line_pitch,
                        inner_w * 0.28,
                        Rgb::new(p.edge_l.r, p.edge_l.g, p.edge_l.b),
                        235,
                    ),
                );
                clip(
                    quads,
                    bar(
                        line_x + inner_w * 0.28 + s(5.0),
                        y0 + line_pitch,
                        inner_w * 0.20,
                        Rgb::new(p.edge_r.r, p.edge_r.g, p.edge_r.b),
                        235,
                    ),
                );
                clip(quads, bar(line_x, y0 + 2.0 * line_pitch, inner_w * 0.55, ink.fg, 96));
            }

            group_frame(quads, geometry.system_theme, 1);
            row_hover(quads, geometry.system_theme, view.hover == SettingsHit::SystemThemeToggle);
            toggle(
                quads,
                &mut staged,
                geometry.system_theme,
                SettingsHit::SystemThemeToggle,
                view.follow_system_theme,
                view.hover == SettingsHit::SystemThemeToggle,
                view.pressed == SettingsHit::SystemThemeToggle,
            );

            // 自定义背景和界面都使用连续分组，避免设置 Tab 内再次出现
            // 漂浮卡片语言。
            group_frame(quads, geometry.background, 6);
            row_hover(quads, geometry.background, view.hover == SettingsHit::BackgroundColor);
            // 背景色也是多选项设置：同一 combobox 组件，浮层换成色板+hex。
            combobox(
                quads,
                &mut staged,
                geometry.background,
                view.hover == SettingsHit::BackgroundColor,
                view.dropdown == Some(SettingsDropdown::BackgroundColor),
            );
            row_hover(quads, geometry.background_image, view.hover == SettingsHit::BackgroundImage);
            if view.background_image.is_some() {
                row_hover(
                    quads,
                    geometry.background_image_clear,
                    view.hover == SettingsHit::BackgroundImageClear,
                );
            }
            combobox(
                quads,
                &mut staged,
                geometry.background_image_fit,
                view.hover == SettingsHit::BackgroundImageFit,
                view.dropdown == Some(SettingsDropdown::BackgroundFit),
            );
            combobox(
                quads,
                &mut staged,
                geometry.background_image_alignment,
                view.hover == SettingsHit::BackgroundImageAlignment,
                view.dropdown == Some(SettingsDropdown::BackgroundAlignment),
            );
            slider(
                quads,
                &mut staged,
                geometry.background_image_opacity_slider,
                view.background_image_opacity,
                view.hover == SettingsHit::BackgroundImageOpacitySlider
                    || view.dragging_opacity == Some(SettingsOpacityTarget::BackgroundImage),
            );
            row_hover(
                quads,
                geometry.background_image_cover_chrome,
                view.hover == SettingsHit::BackgroundImageCoverChrome,
            );
            toggle(
                quads,
                &mut staged,
                geometry.background_image_cover_chrome,
                SettingsHit::BackgroundImageCoverChrome,
                view.background_image_cover_chrome,
                view.hover == SettingsHit::BackgroundImageCoverChrome,
                view.pressed == SettingsHit::BackgroundImageCoverChrome,
            );

            // 光标组：形状下拉 + 闪烁开关。
            group_frame(quads, geometry.cursor_shape_row, 2);
            row_hover(
                quads,
                geometry.cursor_shape_row,
                view.hover == SettingsHit::CursorShapeDropdown,
            );
            combobox(
                quads,
                &mut staged,
                geometry.cursor_shape_row,
                view.hover == SettingsHit::CursorShapeDropdown,
                view.dropdown == Some(SettingsDropdown::CursorShape),
            );
            row_hover(
                quads,
                geometry.cursor_blink_row,
                view.hover == SettingsHit::CursorBlinkToggle,
            );
            toggle(
                quads,
                &mut staged,
                geometry.cursor_blink_row,
                SettingsHit::CursorBlinkToggle,
                view.cursor_blink,
                view.hover == SettingsHit::CursorBlinkToggle,
                view.pressed == SettingsHit::CursorBlinkToggle,
            );

            // 界面组：语言（同一通用下拉组件）+ 界面外观预设 + 终端不透明度
            // + 背景模糊。
            group_frame(quads, geometry.language_row, 3);
            row_hover(quads, geometry.language_row, view.hover == SettingsHit::LanguageDropdown);
            combobox(
                quads,
                &mut staged,
                geometry.language_row,
                view.hover == SettingsHit::LanguageDropdown,
                view.dropdown == Some(SettingsDropdown::Language),
            );
            row_hover(quads, geometry.density_row, view.hover == SettingsHit::DensityDropdown);
            combobox(
                quads,
                &mut staged,
                geometry.density_row,
                view.hover == SettingsHit::DensityDropdown,
                view.dropdown == Some(SettingsDropdown::Density),
            );
            slider(
                quads,
                &mut staged,
                geometry.opacity_slider,
                view.opacity,
                view.hover == SettingsHit::OpacitySlider
                    || view.dragging_opacity == Some(SettingsOpacityTarget::Terminal),
            );
            row_hover(quads, geometry.blur, view.hover == SettingsHit::BlurToggle);
            toggle(
                quads,
                &mut staged,
                geometry.blur,
                SettingsHit::BlurToggle,
                view.blur,
                view.hover == SettingsHit::BlurToggle,
                view.pressed == SettingsHit::BlurToggle,
            );

            group_frame(quads, geometry.font_size_row, 4);
            widgets::push_spinner(
                &mut staged,
                geometry.font_size_row,
                scale,
                &sk,
                view.hover == SettingsHit::FontSizeUp,
                view.hover == SettingsHit::FontSizeDown,
            );
            row_hover(
                quads,
                geometry.cell_width_mode,
                view.hover == SettingsHit::CellWidthModeDropdown,
            );
            combobox(
                quads,
                &mut staged,
                geometry.cell_width_mode,
                view.hover == SettingsHit::CellWidthModeDropdown,
                view.dropdown == Some(SettingsDropdown::CellWidthMode),
            );
            for quad in staged.drain(..) {
                clip(quads, quad);
            }
            row_hover(quads, geometry.fetch, view.hover == SettingsHit::FetchToggle);
            row_hover(quads, geometry.powerline, view.hover == SettingsHit::PowerlineToggle);
            toggle(
                quads,
                &mut staged,
                geometry.fetch,
                SettingsHit::FetchToggle,
                view.fetch,
                view.hover == SettingsHit::FetchToggle,
                view.pressed == SettingsHit::FetchToggle,
            );
            toggle(
                quads,
                &mut staged,
                geometry.powerline,
                SettingsHit::PowerlineToggle,
                view.powerline,
                view.hover == SettingsHit::PowerlineToggle,
                view.pressed == SettingsHit::PowerlineToggle,
            );
        },
        NebulaSettingsSection::Profiles => {
            // 终端组：Shell / 启动目录 / 字体。下拉列表是浮层，行的
            // hairline 分组保持固定，不再被展开的列表推开。
            group_frame(quads, geometry.shell, 2);
            group_frame(quads, geometry.startup_directory, 1);
            group_frame(quads, geometry.font, 1);
            group_frame(quads, geometry.ghost, 2);
            group_frame(quads, geometry.open_config_file, 1);
            for (hit, rect) in [
                (SettingsHit::ShellCycle, geometry.shell),
                (SettingsHit::ImportTerminal, geometry.terminal_import),
                (SettingsHit::StartupDirectory, geometry.startup_directory),
                (SettingsHit::FontCycle, geometry.font),
                (SettingsHit::GhostToggle, geometry.ghost),
                (SettingsHit::AcceptCycle, geometry.accept),
                (SettingsHit::OpenConfigFile, geometry.open_config_file),
            ] {
                row_hover(quads, rect, view.hover == hit);
            }
            if view.startup_directory.is_some() {
                row_hover(
                    quads,
                    geometry.startup_directory_clear,
                    view.hover == SettingsHit::StartupDirectoryClear,
                );
            }
            action_button(
                quads,
                geometry.terminal_import,
                STANDARD_ROW_ACTION_W,
                view.hover == SettingsHit::ImportTerminal,
            );
            action_button(
                quads,
                geometry.open_config_file,
                STANDARD_ROW_ACTION_W,
                view.hover == SettingsHit::OpenConfigFile,
            );
            combobox(
                quads,
                &mut staged,
                geometry.shell,
                view.hover == SettingsHit::ShellCycle,
                view.dropdown == Some(SettingsDropdown::Shell),
            );
            combobox(
                quads,
                &mut staged,
                geometry.font,
                view.hover == SettingsHit::FontCycle,
                view.dropdown == Some(SettingsDropdown::Font),
            );
            combobox(
                quads,
                &mut staged,
                geometry.accept,
                view.hover == SettingsHit::AcceptCycle,
                view.dropdown == Some(SettingsDropdown::Accept),
            );
            // Boolean rows render a real switch instead of an "On/Off" string.
            for (rect, on) in [(geometry.ghost, view.ghost)] {
                toggle(
                    quads,
                    &mut staged,
                    rect,
                    SettingsHit::GhostToggle,
                    on,
                    view.hover == SettingsHit::GhostToggle,
                    view.pressed == SettingsHit::GhostToggle,
                );
            }
        },
        NebulaSettingsSection::Providers => {
            for index in 0..geometry.provider_row_count {
                let row = (
                    geometry.provider_row0.0,
                    geometry.provider_row0.1 + index as f32 * geometry.provider_row_h,
                    geometry.provider_row0.2,
                    geometry.provider_row_h,
                );
                let active = view
                    .providers
                    .get(index)
                    .is_some_and(|provider| provider.id == view.active_provider_id);
                let hovered = view.hover == SettingsHit::ProviderRow(index)
                    || view.hover == SettingsHit::ProviderEnableToggle(index);
                clip(
                    quads,
                    UiQuad::solid(
                        row.0,
                        row.1,
                        row.2,
                        row.3,
                        tokens::radius::CONTROL * scale,
                        if active {
                            sk.accent_soft
                        } else if hovered {
                            sk.hover
                        } else {
                            sk.surface
                        },
                    ),
                );
            }
            for (index, field) in geometry.provider_fields.iter().enumerate() {
                let input_rect = sync_input_rect(*field, scale);
                let mut input = Vec::new();
                surface::push_input(
                    &mut input,
                    input_rect,
                    scale,
                    &sk,
                    view.density,
                    view.provider_focus == Some(index),
                );
                if view.provider_focus == Some(index) {
                    let max_cols = (((input_rect.2 - s(24.0)) / size.cell_width()) as usize).max(1);
                    let (display, placeholder, hidden) =
                        provider_input_display(view, index, max_cols);
                    text_field::push_cursor(
                        &mut input,
                        input_rect.1,
                        input_rect.3,
                        input_rect.0 + s(12.0),
                        if placeholder { "" } else { &display },
                        &view.provider_cursors[index].shifted(hidden),
                        size.cell_width(),
                        scale,
                        &sk,
                    );
                }
                for quad in input {
                    clip(quads, quad);
                }
            }
            let current =
                view.providers.iter().find(|provider| provider.id == view.active_provider_id);
            for (row, hit, on) in [
                (
                    geometry.provider_codex_goals,
                    SettingsHit::ProviderCodexGoalsToggle,
                    current.is_some_and(|provider| provider.codex_goals),
                ),
                (
                    geometry.provider_codex_remote,
                    SettingsHit::ProviderCodexRemoteToggle,
                    current.is_some_and(|provider| provider.codex_remote_compaction),
                ),
            ] {
                toggle(quads, &mut staged, row, hit, on, view.hover == hit, view.pressed == hit);
            }
            action_button(
                quads,
                geometry.provider_codex_apply,
                148.0,
                view.hover == SettingsHit::ProviderApplyCodex,
            );
            for (hit, rect) in [
                (SettingsHit::ProviderAdd, geometry.provider_add),
                (SettingsHit::ProviderSave, geometry.provider_save),
                (SettingsHit::ProviderTest, geometry.provider_test),
                (SettingsHit::ProviderDelete, geometry.provider_delete),
            ] {
                let mut button = Vec::new();
                widgets::push_outline_button(&mut button, rect, scale, &sk, view.hover == hit);
                for quad in button {
                    clip(quads, quad);
                }
            }
        },
        NebulaSettingsSection::Ssh => {
            // Saved hosts are the primary SSH surface. The compact fixed pitch
            // keeps every two-line card separate without turning it into a tall tile.
            for index in 0..geometry.ssh_host_count {
                let row = (
                    geometry.ssh_host_row0.0,
                    geometry.ssh_host_row0.1
                        + index as f32 * (geometry.ssh_host_row_h + geometry.ssh_host_gap),
                    geometry.ssh_host_row0.2,
                    geometry.ssh_host_row_h,
                );
                let (rx, ry, rw, rh) = row;
                // 2026-08-11 用户裁定：常态不画卡片底。原来每行都铺 accent_soft，
                // 五行灰块叠起来就是「灰蒙蒙」的来源。底色只在 hover 时出现，
                // 静态列表回到纯净的白底 + 文字层级。
                let row_hovered = matches!(
                    view.hover,
                    SettingsHit::SshHostRow(i)
                        | SettingsHit::SshHostConnect(i)
                        | SettingsHit::SshHostEdit(i)
                        | SettingsHit::SshHostHide(i)
                        if i == index
                );
                if row_hovered {
                    clip(
                        quads,
                        UiQuad::solid(rx, ry, rw, rh, s(tokens::radius::OVERLAY), sk.hover),
                    );
                }
                // 行左缘画主机的 OS 图标（文字 pass，os_icons 字形），不再是
                // 状态环——2026-08-09 用户裁定：已保存主机要把图标显示出来。
                // 主机模型依旧没有可靠的在线态，图标标注的是「这是哪台机器」，
                // 不发明绿点。
                //
                // 三枚动作图标同样只在 hover 时显形，且不再套描边方框——方框
                // 加文字常态吃掉右侧 180px，把主机名挤到一半宽。命中区始终存在
                // （见 ssh_host_action_rect），只是墨迹不画。
                if row_hovered {
                    // 眼睛图标要挖空，挖空色必须是这一枚图标脚下的实际底色，
                    // 否则半透明的 hover washes 叠起来会在瞳孔周围留一圈脏边。
                    let row_bg = surface::over(sk.hover, sk.panel);
                    for (action, icon) in [
                        (0usize, icons::RowActionIcon::Connect),
                        (1usize, icons::RowActionIcon::Edit),
                        (2usize, icons::RowActionIcon::Hide),
                    ] {
                        let rect = ssh_host_action_rect(row, scale, action);
                        let icon_hovered = match (action, view.hover) {
                            (0, SettingsHit::SshHostConnect(i)) => i == index,
                            (1, SettingsHit::SshHostEdit(i)) => i == index,
                            (2, SettingsHit::SshHostHide(i)) => i == index,
                            _ => false,
                        };
                        let (ink, cutout) = if icon_hovered {
                            clip(
                                quads,
                                UiQuad::solid(
                                    rect.0,
                                    rect.1,
                                    rect.2,
                                    rect.3,
                                    s(tokens::radius::CONTROL),
                                    sk.hover_strong,
                                ),
                            );
                            (sk.ink.into(), surface::over(sk.hover_strong, row_bg))
                        } else {
                            (sk.ink_dim.into(), row_bg)
                        };
                        let mut ink_quads = Vec::new();
                        icons::push_row_action_icon(
                            &mut ink_quads,
                            icon,
                            rect,
                            scale,
                            ink,
                            cutout,
                        );
                        for quad in ink_quads {
                            clip(quads, quad);
                        }
                    }
                }
            }
            // "Add host" belongs to the Saved hosts heading, not a second
            // full-width settings row below the list.
            let add = geometry.ssh_add_host;
            let add_hovered = view.hover == SettingsHit::SshAddHost;
            let mut add_button = Vec::new();
            widgets::push_outline_button(&mut add_button, add, scale, &sk, add_hovered);
            for quad in add_button {
                clip(quads, quad);
            }
            let add_icon_rect =
                if add.2 < s(96.0) { add } else { (add.0 + s(4.0), add.1, add.3, add.3) };
            let mut add_icon = Vec::new();
            let add_icon_rgb = if add_hovered { sk.icon_hover } else { sk.icon };
            icons::push_add(
                &mut add_icon,
                add_icon_rect,
                scale,
                Rgba::new(add_icon_rgb.r, add_icon_rgb.g, add_icon_rgb.b, 230),
            );
            for quad in add_icon {
                clip(quads, quad);
            }
            group_frame(quads, geometry.ssh_import_config, 1);
            action_button(
                quads,
                geometry.ssh_import_config,
                STANDARD_ROW_ACTION_W,
                view.hover == SettingsHit::SshImportConfig,
            );
            if geometry.hidden_host_count > 0 {
                group_frame(quads, geometry.hidden_host_row0, geometry.hidden_host_count);
                for index in 0..geometry.hidden_host_count {
                    let mut rect = geometry.hidden_host_row0;
                    rect.1 += index as f32 * rect.3;
                    action_button(
                        quads,
                        rect,
                        80.0,
                        view.hover == SettingsHit::RestoreHiddenSsh(index),
                    );
                }
            }
        },
        NebulaSettingsSection::Proxy => {
            widgets::push_combobox(
                &mut staged,
                ssh_proxy_mode_control(geometry.ssh_proxy_mode, scale),
                scale,
                &sk,
                view.hover == SettingsHit::SshProxyModeDropdown,
                view.dropdown == Some(SettingsDropdown::SshProxyMode),
            );
            for quad in staged.drain(..) {
                clip(quads, quad);
            }
            let cell_w = size.cell_width();
            // 输入控件与 caret 共用同一矩形；协议与地址的几何也同时供命中和
            // 文字使用，避免精简页面后又出现“看得到但点不到”的漂移。
            let input_control = |quads: &mut Vec<UiQuad>, (ix, iy, iw, ih), index: usize| {
                let focused = view.ssh_proxy_focus == Some(index);
                let mut input = Vec::new();
                surface::push_input(
                    &mut input,
                    (ix, iy, iw, ih),
                    scale,
                    &sk,
                    view.density,
                    focused,
                );
                if focused {
                    let max_cols = (((iw - s(24.0)) / cell_w) as usize).max(1);
                    let (display, placeholder, _, hidden) =
                        ssh_proxy_input_display(view, index, max_cols);
                    if !placeholder {
                        // 展示串是跟随光标的窗口：光标按窗口起点平移，
                        // 与文字落在同一套列换算上。
                        text_field::push_cursor(
                            &mut input,
                            iy,
                            ih,
                            ix + s(12.0),
                            &display,
                            &view.ssh_proxy_cursors[index].shifted(hidden),
                            cell_w,
                            scale,
                            &sk,
                        );
                    } else {
                        text_field::push_cursor(
                            &mut input,
                            iy,
                            ih,
                            ix + s(12.0),
                            "",
                            &view.ssh_proxy_cursors[index],
                            cell_w,
                            scale,
                            &sk,
                        );
                    }
                }
                for quad in input {
                    clip(quads, quad);
                }
            };
            if view.ssh_proxy_mode == crate::ssh_proxy::ProxyMode::Custom {
                let (protocol, address) =
                    ssh_proxy_manual_controls(geometry.ssh_proxy_expand, scale);
                widgets::push_combobox(
                    &mut staged,
                    protocol,
                    scale,
                    &sk,
                    view.hover == SettingsHit::SshProxyProtocolDropdown,
                    view.dropdown == Some(SettingsDropdown::SshProxyProtocol),
                );
                for quad in staged.drain(..) {
                    clip(quads, quad);
                }
                input_control(quads, address, 0);
            }

            // 功能横幅保持中性底；成功/失败只由状态文字表达，不用整块绿/红
            // 背景污染页面。按钮走通用 outline widget。
            let banner = geometry.ssh_proxy_test;
            let corner = s(tokens::radius::OVERLAY);
            let mut banner_quads = Vec::new();
            surface::push_stroke(&mut banner_quads, banner, corner, scale, sk.hairline);
            banner_quads
                .push(UiQuad::solid(banner.0, banner.1, banner.2, banner.3, corner, sk.surface));
            let button = ssh_proxy_test_button(banner, scale);
            widgets::push_outline_button(
                &mut banner_quads,
                button,
                scale,
                &sk,
                view.hover == SettingsHit::SshProxyTest
                    && !matches!(view.proxy_test_status, ProxyTestStatus::Running),
            );
            for quad in banner_quads {
                clip(quads, quad);
            }
        },
        NebulaSettingsSection::Interaction => {
            group_frame(quads, geometry.copy_on_select, 4);
            row_hover(
                quads,
                geometry.copy_on_select,
                view.hover == SettingsHit::CopyOnSelectToggle,
            );
            toggle(
                quads,
                &mut staged,
                geometry.copy_on_select,
                SettingsHit::CopyOnSelectToggle,
                view.copy_on_select,
                view.hover == SettingsHit::CopyOnSelectToggle,
                view.pressed == SettingsHit::CopyOnSelectToggle,
            );
            row_hover(quads, geometry.tab_reveal, view.hover == SettingsHit::TabRevealDropdown);
            combobox(
                quads,
                &mut staged,
                geometry.tab_reveal,
                view.hover == SettingsHit::TabRevealDropdown,
                view.dropdown == Some(SettingsDropdown::TabReveal),
            );
            row_hover(
                quads,
                geometry.new_tab_position,
                view.hover == SettingsHit::NewTabPositionDropdown,
            );
            combobox(
                quads,
                &mut staged,
                geometry.new_tab_position,
                view.hover == SettingsHit::NewTabPositionDropdown,
                view.dropdown == Some(SettingsDropdown::NewTabPosition),
            );
            row_hover(quads, geometry.panel_resize, view.hover == SettingsHit::PanelResizeToggle);
            toggle(
                quads,
                &mut staged,
                geometry.panel_resize,
                SettingsHit::PanelResizeToggle,
                view.panel_resize,
                view.hover == SettingsHit::PanelResizeToggle,
                view.pressed == SettingsHit::PanelResizeToggle,
            );
            group_frame(quads, geometry.cjk_bold, 1);
            row_hover(quads, geometry.cjk_bold, view.hover == SettingsHit::CjkBoldToggle);
            toggle(
                quads,
                &mut staged,
                geometry.cjk_bold,
                SettingsHit::CjkBoldToggle,
                view.cjk_bold_regular,
                view.hover == SettingsHit::CjkBoldToggle,
                view.pressed == SettingsHit::CjkBoldToggle,
            );
        },
        NebulaSettingsSection::Keymap => {
            let cell_w = size.cell_width();
            // 搜索框（原型 .search：input 底、聚焦 accent 边——push_input
            // 配方已含这两态）。捕获进行时搜索不聚焦，caret 不闪。
            {
                let (sx, sy, sw, sh) = geometry.keymap_search;
                let focused = view.keymap_search_focus && view.keymap_capture.is_none();
                let mut input = Vec::new();
                surface::push_input(
                    &mut input,
                    (sx, sy, sw, sh),
                    scale,
                    &sk,
                    view.density,
                    focused,
                );
                text_field::push_cursor(
                    &mut input,
                    sy,
                    sh,
                    sx + s(12.0),
                    &view.keymap_query,
                    &view.keymap_query_cursor,
                    cell_w,
                    scale,
                    &sk,
                );
                for quad in input {
                    clip(quads, quad);
                }
            }
            // 冲突提示条：warn 变体（有待办动作才配警示色，纪律见原型 451）。
            if geometry.keymap_pane.clash {
                let (nx, ny, _nw, nh) = geometry.keymap_note;
                // 警告是语义状态，不再退回被废弃的整块卡片：一条高对比
                // amber beam 保留提示作用，同时让内容继续和页面背景融为一体。
                let beam_w = s(3.0);
                let mark_d = s(16.0);
                clip(
                    quads,
                    UiQuad::solid(
                        nx,
                        ny + s(5.0),
                        beam_w,
                        (nh - s(10.0)).max(0.0),
                        beam_w * 0.5,
                        Rgba::new(sk.warn.r, sk.warn.g, sk.warn.b, 225),
                    ),
                );
                clip(
                    quads,
                    UiQuad::solid(
                        nx + s(8.0),
                        ny + s(10.0),
                        mark_d,
                        mark_d,
                        mark_d * 0.5,
                        Rgba::new(sk.warn.r, sk.warn.g, sk.warn.b, 38),
                    ),
                );
            }
            let (row_x, _, row_w, row_h) = geometry.keymap_row0;
            // 每个动作分组各自一圈 hairline；中心回填页面本色而不是 card 色，
            // 所以只得到独立线框，没有用户不需要的分组背景块。
            let outline_group = |quads: &mut Vec<UiQuad>, first_y: f32, rows: usize| {
                if rows == 0 {
                    return;
                }
                let rect = (row_x, first_y, row_w, row_h * rows as f32);
                let corner = s(tokens::radius::OVERLAY);
                let mut outline = Vec::new();
                surface::push_stroke(&mut outline, rect, corner, scale, sk.hairline);
                outline.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, corner, sk.panel));
                for row in 1..rows {
                    outline.push(UiQuad::solid(
                        rect.0,
                        rect.1 + row as f32 * row_h,
                        rect.2,
                        s(1.0).max(1.0),
                        0.0,
                        sk.hairline,
                    ));
                }
                for quad in outline {
                    clip(quads, quad);
                }
            };
            let mut first_slot = 0usize;
            for rows in geometry.keymap_pane.visible {
                let rows = rows as usize;
                if rows > 0 && first_slot < geometry.keymap_slot_ys.len() {
                    outline_group(quads, geometry.keymap_slot_ys[first_slot], rows);
                    first_slot += rows;
                }
            }
            if !view.keymap_readonly_visible.is_empty() {
                outline_group(
                    quads,
                    geometry.keymap_readonly_row0.1,
                    view.keymap_readonly_visible.len(),
                );
            }
            for (slot, flat) in view.keymap_visible.iter().copied().enumerate() {
                if slot >= geometry.keymap_slot_ys.len() {
                    break;
                }
                let rect = (row_x, geometry.keymap_slot_ys[slot], row_w, row_h);
                let hovered = view.hover == SettingsHit::KeymapRow(slot);
                row_hover(quads, rect, hovered);
                // Keycap 底座：捕获中的行换 accent 描边 + 软填充提示「正在
                // 等待按键」；未绑定行不画底座；冲突行 danger 底（.kbd.clash）。
                let capturing = view.keymap_capture == Some(flat);
                let (label, _, bound) = keymap_row_value(view, flat);
                let cap = keymap_keycap_rect(rect, &label, cell_w, scale);
                let (cx, cy, cw, ch) = cap;
                if capturing {
                    clip(
                        quads,
                        UiQuad::solid(
                            cx - s(1.0),
                            cy - s(1.0),
                            cw + s(2.0),
                            ch + s(2.0),
                            s(7.0),
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
                        ),
                    );
                    clip(quads, UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.panel));
                    clip(quads, UiQuad::solid(cx, cy, cw, ch, s(6.0), sk.accent_soft));
                } else if bound {
                    let combo = super::ui::keycap::layout_combo(
                        &label,
                        rect.0 + rect.2 - s(16.0),
                        rect.1 + rect.3 / 2.0,
                        cell_w,
                        scale,
                    );
                    let danger = view.keymap_clash_rows.get(flat).copied().unwrap_or(false);
                    let (_, chip_y, _, chip_h) = combo.bounds;
                    let mut chip_quads = Vec::new();
                    for &(chip_x, chip_w, _) in &combo.chips {
                        super::ui::keycap::push_chip_toned(
                            &mut chip_quads,
                            &sk,
                            chip_x,
                            chip_y,
                            chip_w,
                            chip_h,
                            scale,
                            hovered,
                            danger,
                        );
                    }
                    for quad in chip_quads {
                        clip(quads, quad);
                    }
                }
            }
            // 只读行同样给轻量 hover（无位移的色变）：一列可交互行里没有
            // 反馈的行读作「死区」，像是渲染坏了。
            let (rx, ry, rw, rh) = geometry.keymap_readonly_row0;
            for index in 0..view.keymap_readonly_visible.len() {
                let rect = (rx, ry + index as f32 * rh, rw, rh);
                row_hover(quads, rect, view.hover == SettingsHit::KeymapReadonlyRow(index));
            }
        },
        NebulaSettingsSection::Advanced => {
            group_frame(quads, geometry.keep_session, 2);
            row_hover(quads, geometry.keep_session, view.hover == SettingsHit::KeepSessionToggle);
            toggle(
                quads,
                &mut staged,
                geometry.keep_session,
                SettingsHit::KeepSessionToggle,
                view.keep_session,
                view.hover == SettingsHit::KeepSessionToggle,
                view.pressed == SettingsHit::KeepSessionToggle,
            );
            row_hover(
                quads,
                geometry.restore_session,
                view.hover == SettingsHit::RestoreSessionToggle,
            );
            toggle(
                quads,
                &mut staged,
                geometry.restore_session,
                SettingsHit::RestoreSessionToggle,
                view.restore_session,
                view.hover == SettingsHit::RestoreSessionToggle,
                view.pressed == SettingsHit::RestoreSessionToggle,
            );

            if SHOW_WEBDAV_SYNC_SETTINGS {
                // ---- 同步（WebDAV，spec 003）：4 输入行 + 自动拉取开关 ----
                group_frame(quads, geometry.sync_rows[0], 5);
                let cell_w = size.cell_width();
                for (index, row) in geometry.sync_rows.iter().enumerate() {
                    row_hover(quads, *row, view.hover == SettingsHit::SyncInput(index));
                    let (ix, iy, iw, ih) = sync_input_rect(*row, scale);
                    let focused = view.sync_focus == Some(index);
                    let border = if focused { sk.accent } else { sk.ink_dim };
                    let border_alpha = if focused { 255 } else { 90 };
                    clip(
                        quads,
                        UiQuad::solid(
                            ix - s(1.0),
                            iy - s(1.0),
                            iw + s(2.0),
                            ih + s(2.0),
                            s(8.0),
                            Rgba::new(border.r, border.g, border.b, border_alpha),
                        ),
                    );
                    clip(quads, UiQuad::solid(ix, iy, iw, ih, s(7.0), sk.surface));
                    if focused && super::caret_blink_on() {
                        let max_cols = (((iw - s(24.0)) / cell_w) as usize).max(1);
                        let (_, placeholder, cols) = sync_input_display(view, index, max_cols);
                        let cols = if placeholder { 0 } else { cols };
                        let caret_h = ih - s(10.0);
                        clip(
                            quads,
                            UiQuad::solid(
                                (ix + s(12.0) + cols as f32 * cell_w).min(ix + iw - s(6.0)),
                                iy + (ih - caret_h) / 2.0,
                                (1.5 * scale).max(1.0),
                                caret_h,
                                0.0,
                                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
                            ),
                        );
                    }
                }
                row_hover(
                    quads,
                    geometry.sync_auto_pull,
                    view.hover == SettingsHit::SyncAutoPullToggle,
                );
                toggle(
                    quads,
                    &mut staged,
                    geometry.sync_auto_pull,
                    SettingsHit::SyncAutoPullToggle,
                    view.sync_auto_pull,
                    view.hover == SettingsHit::SyncAutoPullToggle,
                    view.pressed == SettingsHit::SyncAutoPullToggle,
                );

                // 动作行：两个独立按钮（同步中变灰、不吃 hover）。
                let [push_rect, pull_rect] = sync_button_rects(geometry.sync_actions, scale);
                for (rect, hit) in [
                    (push_rect, SettingsHit::SyncPushButton),
                    (pull_rect, SettingsHit::SyncPullButton),
                ] {
                    let (bx, by, bw, bh) = rect;
                    let hot = view.hover == hit && !view.sync_busy;
                    clip(
                        quads,
                        UiQuad::solid(
                            bx - s(1.0),
                            by - s(1.0),
                            bw + s(2.0),
                            bh + s(2.0),
                            s(9.0),
                            sk.hairline,
                        ),
                    );
                    clip(
                        quads,
                        UiQuad::solid(
                            bx,
                            by,
                            bw,
                            bh,
                            s(8.0),
                            if hot { sk.hover } else { sk.panel },
                        ),
                    );
                }
            }
        },
        NebulaSettingsSection::Backup => {
            let overlay_radius = super::ui::tokens::radius::OVERLAY * scale;
            let control_radius = super::ui::tokens::radius::CONTROL * scale;
            let chip_radius = super::ui::tokens::radius::CHIP * scale;
            // Automatic-backup summary card. Its switch is deliberately shown
            // disabled while the page is gated: the current backend supports
            // explicit encrypted exports, but has no scheduled-retention
            // service yet, so presenting an active control would be dishonest.
            {
                let (ax, ay, aw, ah) = geometry.backup_auto;
                let mut stroke = Vec::new();
                surface::push_stroke(
                    &mut stroke,
                    geometry.backup_auto,
                    overlay_radius,
                    scale,
                    sk.hairline,
                );
                for quad in stroke {
                    clip(quads, quad);
                }
                clip(quads, UiQuad::solid(ax, ay, aw, ah, overlay_radius, sk.panel));
                let icon_rect = (ax + s(16.0), ay + (ah - s(34.0)) * 0.5, s(34.0), s(34.0));
                let (ix, iy, iw, ih) = icon_rect;
                clip(quads, UiQuad::solid(ix, iy, iw, ih, control_radius, sk.surface));
                let mut icon = Vec::new();
                icons::push_settings_nav_icon(
                    &mut icon,
                    icons::SettingsNavIcon::Backup,
                    icon_rect,
                    scale,
                    Rgba::new(sk.icon.r, sk.icon.g, sk.icon.b, 220),
                    icons::blend_over(sk.panel, sk.surface),
                );
                for quad in icon {
                    clip(quads, quad);
                }
                let track = (ax + aw - s(50.0), ay + (ah - s(20.0)) * 0.5, s(34.0), s(20.0));
                clip(
                    quads,
                    UiQuad::solid(track.0, track.1, track.2, track.3, track.3 * 0.5, sk.track_off),
                );
                clip(
                    quads,
                    UiQuad::solid(
                        track.0 + s(2.0),
                        track.1 + s(2.0),
                        s(16.0),
                        s(16.0),
                        s(16.0) * 0.5,
                        sk.knob_off,
                    ),
                );
            }

            // Export / restore actions share the prototype's segmented plate.
            {
                let (sx, sy, sw, sh) = geometry.backup_segment;
                clip(quads, UiQuad::solid(sx, sy, sw, sh, overlay_radius, sk.card));
                let [export, restore] = backup_segment_rects(geometry.backup_segment, scale);
                for (rect, hit, active) in [
                    (export, SettingsHit::BackupExport, true),
                    (restore, SettingsHit::BackupRestore, false),
                ] {
                    let (bx, by, bw, bh) = rect;
                    let fill = if active {
                        sk.panel
                    } else if view.hover == hit {
                        sk.hover
                    } else {
                        Rgba::new(0, 0, 0, 0)
                    };
                    clip(quads, UiQuad::solid(bx, by, bw, bh, control_radius, fill));
                }
            }

            // One manifest card with quiet group headers. Row geometry is not
            // contiguous in category-index order, so paint by the explicit
            // visual order used by the HTML prototype.
            {
                let (gx, gy, gw, _) = geometry.backup_groups[0];
                let last = geometry.backup_rows[8];
                let gh = last.1 + last.3 - gy;
                clip(quads, UiQuad::solid(gx, gy, gw, gh, overlay_radius, sk.panel));
            }
            for (index, row) in geometry.backup_rows.iter().enumerate() {
                row_hover(quads, *row, view.hover == SettingsHit::BackupSelection(index));
                let on = backup_item_selected(view.backup_selection, index);
                let cb = (row.0 + s(16.0), row.1 + (row.3 - s(16.0)) * 0.5, s(16.0), s(16.0));
                if on {
                    clip(
                        quads,
                        UiQuad::solid(
                            cb.0,
                            cb.1,
                            cb.2,
                            cb.3,
                            chip_radius,
                            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
                        ),
                    );
                    let mut check = Vec::new();
                    icons::push_check(
                        &mut check,
                        cb.0 + cb.2 * 0.5,
                        cb.1 + cb.3 * 0.5,
                        scale * 0.82,
                        Rgba::new(sk.ink_on_accent.r, sk.ink_on_accent.g, sk.ink_on_accent.b, 255),
                    );
                    for quad in check {
                        clip(quads, quad);
                    }
                } else {
                    let mut stroke = Vec::new();
                    surface::push_stroke(&mut stroke, cb, chip_radius, scale, sk.hairline);
                    for quad in stroke {
                        clip(quads, quad);
                    }
                    clip(quads, UiQuad::solid(cb.0, cb.1, cb.2, cb.3, chip_radius, sk.panel));
                }
            }
        },
    }

    // Overlay scrollbar on the content viewport's right edge, only when the
    // section actually overflows (same style as the pane scrollbar: thin
    // rounded thumb, no track).
    let content_h = match section {
        NebulaSettingsSection::Appearance => geometry.appearance_h,
        NebulaSettingsSection::Profiles => geometry.profiles_h,
        NebulaSettingsSection::Providers => geometry.providers_h,
        NebulaSettingsSection::Ssh => geometry.ssh_h,
        NebulaSettingsSection::Proxy => geometry.proxy_h,
        NebulaSettingsSection::Interaction => geometry.interaction_h,
        NebulaSettingsSection::Keymap => geometry.keymap_h,
        NebulaSettingsSection::Advanced => geometry.advanced_h,
        NebulaSettingsSection::Backup => geometry.backup_h,
    };
    let viewport_h = settings_viewport_h(ph, scale);
    if content_h > viewport_h {
        let max_scroll = content_h - viewport_h;
        let frac = (view.scroll / max_scroll).clamp(0.0, 1.0);
        let track_h = viewport_h - s(12.0);
        let thumb_h = (track_h * viewport_h / content_h).max(s(28.0));
        let ty = clip_top + s(6.0) + (track_h - thumb_h) * frac;
        let tx = px + pw - s(7.0);
        quads.push(UiQuad::solid(
            tx,
            ty,
            s(4.0),
            thumb_h,
            s(2.0),
            sk.scrollbar_thumb.with_alpha(0.45),
        ));
    }
}

/// The floating dropdown option list. `draw_chrome` paints these AFTER the
/// base text pass (a separate `draw_ui` call), so page labels can never bleed
/// through the popup plate — the same modal layering rule the command palette
/// needed.
pub(super) fn push_popup_quads(
    view: &SettingsView,
    quads: &mut Vec<UiQuad>,
    size: &SizeInfo,
    scale: f32,
) {
    let Some(dropdown) = view.dropdown else { return };
    let s = |v: f32| v * scale;
    let sk = settings_skin(view.theme);
    let mut geometry = settings_geometry(
        size,
        scale,
        view.area,
        view.scroll,
        view.hidden_hosts.len(),
        view.ssh_hosts.len(),
        view.density,
        proxy_pane_state(view),
        keymap_pane_state_view(view),
    );
    fit_provider_rows(&mut geometry, view.providers.len());
    // 背景色专用浮层：色板网格 + hex 输入框（几何与 hit 同源）。
    if dropdown == SettingsDropdown::BackgroundColor {
        if view.section != NebulaSettingsSection::Appearance {
            return;
        }
        let popup = background_color_popup(&geometry, scale);
        let (px2, py2, pw2, ph2) = popup.rect;
        // 与通用 combobox 浮层同一套皮肤：柔和投影 + hairline + 不透明面板。
        quads.push(UiQuad::glow(
            px2 - s(14.0),
            py2 - s(10.0),
            pw2 + s(28.0),
            ph2 + s(26.0),
            Rgba::new(0, 0, 0, 70),
        ));
        quads.push(UiQuad::solid(
            px2 - s(1.0),
            py2 - s(1.0),
            pw2 + s(2.0),
            ph2 + s(2.0),
            s(11.0),
            sk.hairline,
        ));
        let mut plate = sk.panel;
        plate.a = 255;
        quads.push(UiQuad::solid(px2, py2, pw2, ph2, s(10.0), plate));
        quads.push(UiQuad::solid(px2, py2, pw2, ph2, s(10.0), sk.surface));

        let selected = dropdown_selected_index(view, dropdown);
        for (index, rect) in popup.swatch.iter().enumerate() {
            let (sx, sy, sw2, sh2) = *rect;
            let hovered = view.hover == SettingsHit::BackgroundSwatch(index);
            if selected == Some(index) || hovered {
                let ring = if selected == Some(index) { sk.accent } else { sk.ink_dim };
                quads.push(UiQuad::solid(
                    sx - s(2.0),
                    sy - s(2.0),
                    sw2 + s(4.0),
                    sh2 + s(4.0),
                    s(8.0),
                    Rgba::new(ring.r, ring.g, ring.b, 255),
                ));
            }
            // 每格带 1px hairline 描边：亮色格在浅色面板上也有边界。
            quads.push(UiQuad::solid(
                sx - s(1.0),
                sy - s(1.0),
                sw2 + s(2.0),
                sh2 + s(2.0),
                s(7.0),
                sk.hairline,
            ));
            let color = BACKGROUND_SWATCHES[index];
            quads.push(UiQuad::solid(
                sx,
                sy,
                sw2,
                sh2,
                s(6.0),
                Rgba::new(color.r, color.g, color.b, 255),
            ));
        }

        // hex 输入框：聚焦态用主题色描边；caret 与 UI 光标共用 500ms 相位。
        let (hx, hy, hw, hh) = popup.hex;
        let focused = view.bg_hex_active;
        let border = if focused { sk.accent } else { sk.ink_dim };
        let border_alpha = if focused { 255 } else { 120 };
        quads.push(UiQuad::solid(
            hx - s(1.0),
            hy - s(1.0),
            hw + s(2.0),
            hh + s(2.0),
            s(8.0),
            Rgba::new(border.r, border.g, border.b, border_alpha),
        ));
        quads.push(UiQuad::solid(hx, hy, hw, hh, s(7.0), sk.surface));
        if focused && super::caret_blink_on() {
            let cell_w = size.cell_width();
            let caret_x = hx + s(12.0) + view.bg_hex_input.chars().count() as f32 * cell_w;
            let caret_h = hh - s(12.0);
            quads.push(UiQuad::solid(
                caret_x.min(hx + hw - s(6.0)),
                hy + (hh - caret_h) / 2.0,
                (1.5 * scale).max(1.0),
                caret_h,
                0.0,
                Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
            ));
        }
        return;
    }
    let (_, py, _, ph) = geometry.popup;
    let Some((anchor, total)) = dropdown_anchor(
        &geometry,
        view.section,
        dropdown,
        view.shells.len(),
        view.fonts.len() + 1,
        scale,
    ) else {
        return;
    };
    let (offset, count) = if dropdown == SettingsDropdown::Font {
        font_popup_window(total, view.font_popup_scroll)
    } else {
        (0, total)
    };
    let popup =
        widgets::combobox_popup_rect(anchor, count, scale, geometry.content_top, py + ph - s(6.0));
    let selected =
        popup_visible_index(dropdown, dropdown_selected_index(view, dropdown), offset, count);
    let hover =
        popup_visible_index(dropdown, dropdown_hover_index(view.hover, dropdown), offset, count);
    widgets::push_combobox_popup(quads, popup, count, selected, hover, scale, &sk, view.density);
    // 字体弹层第 0 行是一个正经输入框：下沉底 + 光标/选区。它不是选项，所以
    // 不吃 hover 高亮，走 `push_input` 而不是 popup 行的配方。
    if matches!(dropdown, SettingsDropdown::Font) {
        let field = widgets::popup_row_rect(popup, 0, scale);
        surface::push_input(quads, field, scale, &sk, view.density, true);
        text_field::push_cursor(
            quads,
            field.1,
            field.3,
            field.0 + s(12.0),
            &view.font_query,
            &view.font_query_cursor,
            size.cell_width(),
            scale,
            &sk,
        );
    }
    if let Some(index) = selected {
        let (rx, ry, rw, rh) = widgets::popup_row_rect(popup, index, scale);
        icons::push_check(
            quads,
            rx + rw - s(16.0),
            ry + rh * 0.5,
            scale,
            Rgba::new(sk.accent.r, sk.accent.g, sk.accent.b, 255),
        );
    }
    if dropdown == SettingsDropdown::Font {
        let total_h = total as f32 * widgets::POPUP_ROW_H * scale;
        let viewport_h = count as f32 * widgets::POPUP_ROW_H * scale;
        if let Some(scrollbar) = widgets::overlay_scrollbar(
            popup,
            viewport_h,
            total_h,
            offset as f32 * widgets::POPUP_ROW_H * scale,
            scale,
        ) {
            widgets::push_overlay_scrollbar(
                quads,
                scrollbar,
                scale,
                &sk,
                view.font_popup_dragging,
                view.font_popup_dragging,
            );
        }
    }
}

/// Option labels for the floating dropdown; returns shell brand-icon draw
/// requests like [`draw_text`]. Must run AFTER `push_popup_quads`'s quads are
/// painted so the labels sit on top of the popup plate.
pub(super) fn draw_popup_text(
    view: &SettingsView,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) -> Vec<(String, (f32, f32, f32, f32))> {
    let mut icon_draws = Vec::new();
    let Some(dropdown) = view.dropdown else { return icon_draws };
    let s = |v: f32| v * scale;
    let sk = settings_skin(view.theme);
    let language = view.language;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let geometry = settings_geometry(
        size,
        scale,
        view.area,
        view.scroll,
        view.hidden_hosts.len(),
        view.ssh_hosts.len(),
        view.density,
        proxy_pane_state(view),
        keymap_pane_state_view(view),
    );
    // 背景色浮层：hex 草稿（或占位提示）画进输入框，色板格无文字。
    if dropdown == SettingsDropdown::BackgroundColor {
        if view.section != NebulaSettingsSection::Appearance {
            return icon_draws;
        }
        let popup = background_color_popup(&geometry, scale);
        let (hx, hy, hw, hh) = popup.hex;
        let ty = hy + (hh - cell_h) / 2.0;
        if view.bg_hex_input.is_empty() {
            r.draw_chrome_text(size, hx + s(12.0), ty, sk.ink_dim, "#RRGGBB", gc);
        } else {
            r.draw_chrome_text(size, hx + s(12.0), ty, sk.ink, &view.bg_hex_input, gc);
        }
        // 输入框右侧给一个动作提示（回车应用）。
        let hint = language.pick("回车应用", "Enter applies");
        let hint_cols: usize = hint.chars().map(|c| c.width().unwrap_or(0)).sum();
        let hint_x = hx + hw - s(12.0) - hint_cols as f32 * cell_w;
        if hint_x > hx + s(12.0) + 9.0 * cell_w {
            r.draw_chrome_text(size, hint_x, ty, sk.ink_dim, hint, gc);
        }
        return icon_draws;
    }
    let (_, py, _, ph) = geometry.popup;
    let Some((anchor, total)) = dropdown_anchor(
        &geometry,
        view.section,
        dropdown,
        view.shells.len(),
        view.fonts.len() + 1,
        scale,
    ) else {
        return icon_draws;
    };
    let (offset, count) = if dropdown == SettingsDropdown::Font {
        font_popup_window(total, view.font_popup_scroll)
    } else {
        (0, total)
    };
    let popup =
        widgets::combobox_popup_rect(anchor, count, scale, geometry.content_top, py + ph - s(6.0));
    let selected =
        popup_visible_index(dropdown, dropdown_selected_index(view, dropdown), offset, count);
    for index in 0..count {
        let absolute_index =
            if dropdown == SettingsDropdown::Font && index > 0 { index + offset } else { index };
        let (rx, ry, rw, rh) = widgets::popup_row_rect(popup, index, scale);
        let ty = ry + (rh - cell_h) / 2.0;
        // Shell rows lead with the brand icon; every other list is text-only.
        let mut text_x = rx + s(12.0);
        let label: String = match dropdown {
            SettingsDropdown::Shell => {
                let Some((id, name, program)) = view.shells.get(absolute_index) else { continue };
                icon_draws
                    .push((id.clone(), (rx + s(8.0), ry + (rh - s(24.0)) / 2.0, s(24.0), s(24.0))));
                text_x = rx + s(40.0);
                if program.is_empty() { name.clone() } else { format!("{name}  ·  {program}") }
            },
            SettingsDropdown::Font => {
                // 第 0 行是搜索框：它的底与光标在 quads pass 里画，这里只
                // 落查询串本身（空着时落提示语）。
                let Some(slot) = font_popup_slot(absolute_index) else {
                    let showing = !view.font_query.is_empty();
                    let text = if showing {
                        view.font_query.clone()
                    } else {
                        language
                            .pick("搜索字体…（直接打字）", "Search fonts… (just type)")
                            .to_owned()
                    };
                    let ink = if showing { sk.ink } else { sk.ink_faint };
                    r.draw_chrome_text(size, text_x, ty, ink, &text, gc);
                    continue;
                };
                match view.fonts.get(slot) {
                    // 候选行用**这个字体自己的字形**画自己的名字：选之前就看见
                    // 选之后的样子（WYSIWYG）。chrome 文本按单元格步进排版，
                    // 所以比例字体在这里的挤压与它进终端网格后完全一致——预览
                    // 不美化，正因如此才有判断价值。
                    //
                    // 元信息（「· 非等宽」）留在界面字体里：那是我们的批注，
                    // 不是字体样本，跟着候选字体变形只会让人误读。
                    Some(family) => {
                        let max_chars = (((rx + rw - s(28.0)) - text_x).max(cell_w) / cell_w)
                            .floor()
                            .max(1.0) as usize;
                        let color = if selected == Some(index) { sk.accent } else { sk.ink };
                        let name = truncate_tab_label(family, max_chars);
                        let previewing = gc.begin_preview_face(family);
                        r.draw_chrome_text(size, text_x, ty, color, &name, gc);
                        if previewing {
                            gc.end_preview_face();
                        }
                        // 非等宽批注接在名字之后，按已画列数让位。
                        if view.font_proportional.contains(&family.to_lowercase()) {
                            let cols: usize =
                                name.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() + 3;
                            let note_x = text_x + cols as f32 * cell_w;
                            let note = language.pick("· 非等宽", "· not monospaced");
                            let note_cols: usize =
                                note.chars().map(|c| c.width().unwrap_or(0)).sum();
                            if note_x + note_cols as f32 * cell_w < rx + rw - s(28.0) {
                                r.draw_chrome_text(size, note_x, ty, sk.ink_dim, note, gc);
                            }
                        }
                        continue;
                    },
                    // 倒数第二行是过滤切换，最后一行是导入。
                    None if slot == view.fonts.len() => {
                        if view.font_show_all {
                            language.pick("◉  显示全部字体", "(*) Showing all fonts").to_owned()
                        } else {
                            language.pick("○  仅等宽字体", "( ) Monospaced only").to_owned()
                        }
                    },
                    None => language.pick("＋  导入字体…", "+  Import font...").to_owned(),
                }
            },
            SettingsDropdown::BackgroundFit => {
                background_image_fit_label(BACKGROUND_FIT_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::BackgroundAlignment => {
                background_image_alignment_label(BACKGROUND_ALIGNMENT_OPTIONS[index], language)
                    .to_owned()
            },
            SettingsDropdown::Language => {
                language_label(LANGUAGE_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::Accept => accept_label(ACCEPT_OPTIONS[index], language).to_owned(),
            SettingsDropdown::TabReveal => {
                tab_reveal_label(TAB_REVEAL_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::Density => density_label(DENSITY_OPTIONS[index], language).to_owned(),
            SettingsDropdown::NewTabPosition => {
                new_tab_position_label(NEW_TAB_POSITION_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::CellWidthMode => {
                cell_width_mode_label(CELL_WIDTH_MODE_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::CursorShape => {
                cursor_shape_label(CURSOR_SHAPE_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::SshProxyMode => {
                ssh_proxy_mode_label(SSH_PROXY_MODE_OPTIONS[index], language).to_owned()
            },
            SettingsDropdown::SshProxyProtocol => {
                manual_proxy_protocol_label(MANUAL_PROXY_PROTOCOL_OPTIONS[index], language)
                    .to_owned()
            },
            SettingsDropdown::SshJumpHost => match view.ssh_hosts.get(absolute_index) {
                Some(host) if host.label != host.destination => {
                    format!("{}  ·  {}", host.label, host.destination)
                },
                Some(host) => host.destination.clone(),
                // 空列表的占位行：告诉用户去哪里补数据，点击无动作。
                None => language
                    .pick("没有已保存的主机——先在 SSH 页添加", "No saved hosts — add one in SSH")
                    .to_owned(),
            },
            // 上方特判提前返回；此臂只为 match 完备。
            SettingsDropdown::BackgroundColor => continue,
        };
        let import_row = matches!(dropdown, SettingsDropdown::Font)
            && font_popup_slot(absolute_index).is_some_and(|slot| view.fonts.get(slot).is_none());
        let color = if selected == Some(index) || import_row { sk.accent } else { sk.ink };
        let max_chars =
            (((rx + rw - s(28.0)) - text_x).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let label = truncate_tab_label(&label, max_chars);
        r.draw_chrome_text(size, text_x, ty, color, &label, gc);
    }
    icon_draws
}

/// Draw a chrome title at `mult`× the terminal font size. Rasterized at the
/// REAL target size (`draw_doc_text`), not GPU-stretched from the base atlas —
/// stretching is what made every modal title fuzzy with ragged edges. The
/// title still grows down and to the right from the (x, y) top-left anchor.
fn draw_big_text(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    _scale: f32,
    x: f32,
    y: f32,
    mult: f32,
    ink: Rgb,
    text: &str,
) {
    r.draw_ui_text(size, x, y, mult, ink, nebula_terminal::term::cell::Flags::empty(), text, gc);
}

/// A group heading inside the content pane: clearly larger than row labels
/// (strict size hierarchy: page title 1.6× > group 1.2× > rows 1.0×) and in
/// the strong ink. One helper so every group shares one size/rhythm.
fn section_title(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    x: f32,
    y: f32,
    text: &str,
) {
    draw_big_text(r, gc, size, scale, x, y, 1.2, sk.ink_strong, text);
}

/// 网络页的分组标题属于测试横幅这一组；标题必须挂在横幅上方，不能再
/// 以代理方式行作为锚点，否则横幅提前后标题会被绘制到横幅内部。
fn proxy_section_title_y(test_row_y: f32, scale: f32) -> f32 {
    test_row_y - 42.0 * scale
}

/// Keymap groups follow the prototype's quiet hierarchy: small, tracked-ish
/// captions and generous whitespace do the grouping work; no frame or filled
/// block is needed around a category.
fn keymap_group_title(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    x: f32,
    y: f32,
    text: &str,
    ink: Rgb,
) {
    r.draw_ui_text(size, x, y, 0.86, ink, nebula_terminal::term::cell::Flags::empty(), text, gc);
}

fn warning_lines(note: &str, max_cols: usize) -> [String; 2] {
    let mut lines = [String::new(), String::new()];
    let mut line = 0usize;
    let mut used = 0usize;
    let mut remaining = false;
    for ch in note.chars() {
        let width = ch.width().unwrap_or(1).max(1);
        if used + width > max_cols {
            if line == 0 {
                line = 1;
                used = 0;
            } else {
                remaining = true;
                break;
            }
        }
        lines[line].push(ch);
        used += width;
    }
    if remaining && !lines[1].is_empty() {
        let _ = lines[1].pop();
        lines[1].push('…');
    }
    lines
}

/// Draw a settings row: a left-aligned label and a right-aligned, truncated
/// value, both vertically centered. Labels are single-line by design — any
/// explanation must fit the label itself (rows with obvious semantics carry
/// no description at all). Inks come from the active theme's [`Skin`].
#[allow(clippy::too_many_arguments)]
fn row_label(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    k: &str,
    v: &str,
    value_ink: Rgb,
) {
    row_label_with_right_inset(r, gc, size, scale, sk, (rx, ry, rw, rh), k, v, value_ink, 0.0);
}

fn draw_button_label(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    rect: (f32, f32, f32, f32),
    label: &str,
    ink: Rgb,
) {
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let cols = label.chars().map(|ch| ch.width().unwrap_or(1)).sum::<usize>();
    r.draw_chrome_text(
        size,
        rect.0 + (rect.2 - cols as f32 * cell_w) * 0.5,
        widgets::centered_y(rect.1, rect.3, cell_h),
        ink,
        label,
        gc,
    );
}

#[allow(clippy::too_many_arguments)]
fn row_label_with_right_inset(
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
    sk: &Skin,
    (rx, ry, rw, rh): (f32, f32, f32, f32),
    k: &str,
    v: &str,
    value_ink: Rgb,
    right_inset: f32,
) {
    let s = |val: f32| val * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    if rh >= s(56.0) {
        // 窄屏行明确分为两层：标签占稳定的上基线，值或控件独占下一层。
        let label_y = ry + s(9.0);
        r.draw_chrome_text(size, rx + s(16.0), label_y, sk.ink, k, gc);
        let value_left = rx + s(16.0);
        let value_right = rx + rw - s(16.0) - right_inset;
        let max_chars = ((value_right - value_left).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let value = truncate_tab_label(v, max_chars);
        if !value.is_empty() {
            r.draw_chrome_text(size, value_left, ry + s(9.0) + cell_h, value_ink, &value, gc);
        }
        return;
    }
    let ty = ry + (rh - cell_h) / 2.0;
    r.draw_chrome_text(size, rx + s(16.0), ty, sk.ink, k, gc);
    let value_left = rx + rw * 0.42;
    let value_right = rx + rw - s(16.0) - right_inset;
    let max_chars = ((value_right - value_left).max(cell_w) / cell_w).floor().max(1.0) as usize;
    let value = truncate_tab_label(v, max_chars);
    let value_cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
    let vx = value_right - value_cols as f32 * cell_w;
    r.draw_chrome_text(size, vx.max(value_left), ty, value_ink, &value, gc);
}

/// Draw the Settings tab's text labels on top of its quads.
pub(super) fn draw_text(
    view: &SettingsView,
    r: &mut Renderer,
    gc: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) -> Vec<(String, (f32, f32, f32, f32))> {
    let s = |v: f32| v * scale;
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let sk = settings_skin(view.theme);
    let language = view.language;

    let mut geometry = settings_geometry(
        size,
        scale,
        view.area,
        view.scroll,
        view.hidden_hosts.len(),
        view.ssh_hosts.len(),
        view.density,
        proxy_pane_state(view),
        keymap_pane_state_view(view),
    );
    fit_provider_rows(&mut geometry, view.providers.len());
    // Kept for parity with [`draw_popup_text`]'s shell icons; the base page
    // currently stages no icon draws of its own.
    let icon_draws = Vec::new();
    let (px, py, _pw, ph) = geometry.popup;
    let (content_x, content_y, _content_w, _) = geometry.content;
    // Text has no scissor, so unlike the quad pass (which cuts quads at the
    // viewport edges) a text block is drawn only when it fits ENTIRELY inside
    // the viewport — a glyph must never cross the header hairline.
    let clip_top = geometry.content_top;
    let clip_bot = py + ph - s(6.0);
    let visible = |ry: f32, rh: f32| ry >= clip_top && ry + rh <= clip_bot;
    let row_text_y = |ry: f32, rh: f32| {
        if geometry.stacked_rows { ry + s(9.0) } else { ry + (rh - cell_h) / 2.0 }
    };
    // 通用 combobox 的当前值：控件框内左对齐，截断在 chevron 井之前。
    let combobox_value = |r: &mut Renderer,
                          gc: &mut GlyphCache,
                          row: (f32, f32, f32, f32),
                          value: &str,
                          ink: Rgb| {
        let rect = widgets::combobox_rect(row, scale);
        let tx = widgets::combobox_text_x(rect, scale);
        let right = widgets::combobox_text_right(rect, scale);
        let max_chars = ((right - tx).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let value = truncate_tab_label(value, max_chars);
        r.draw_chrome_text(size, tx, rect.1 + (rect.3 - cell_h) / 2.0, ink, &value, gc);
    };
    let combobox_value_rect = |r: &mut Renderer,
                               gc: &mut GlyphCache,
                               rect: (f32, f32, f32, f32),
                               value: &str,
                               ink: Rgb| {
        let tx = widgets::combobox_text_x(rect, scale);
        let right = widgets::combobox_text_right(rect, scale);
        let max_chars = ((right - tx).max(cell_w) / cell_w).floor().max(1.0) as usize;
        let value = truncate_tab_label(value, max_chars);
        r.draw_chrome_text(size, tx, rect.1 + (rect.3 - cell_h) / 2.0, ink, &value, gc);
    };
    // Group titles hang 42px above their first row (title + 16px gap) and
    // scroll with it.
    let group_y = |row_y: f32| row_y - s(42.0);
    let title_h = s(26.0);

    let section = view.section;
    // Brand title in the sidebar header. The compact rail is intentionally
    // icon-only, so a long brand label must not compete with the content.
    if !geometry.compact_nav {
        draw_big_text(
            r,
            gc,
            size,
            scale,
            px + s(24.0),
            py + s(22.0),
            1.5,
            sk.ink_strong,
            language.pick("Nebula 设置", "Nebula Settings"),
        );
    }
    if !matches!(section, NebulaSettingsSection::Ssh | NebulaSettingsSection::Providers) {
        // Center the reset label inside its ghost button.
        let (rx, ry, rw, rh) = geometry.reset;
        let label = if geometry.stacked_rows {
            "↶"
        } else {
            language.pick("恢复默认设置", "Restore defaults")
        };
        let cols: usize = label.chars().map(|c| c.width().unwrap_or(0)).sum();
        let tx = rx + (rw - cols as f32 * cell_w) / 2.0;
        r.draw_chrome_text(size, tx, ry + (rh - cell_h) / 2.0, sk.ink_dim, label, gc);
    }
    // Sidebar navigation labels share the icon geometry and visibility gate
    // from the quad/hit passes, so hidden entries cannot leave ghost text.
    for (nav_section, nx, ny, _nw, nh) in geometry.nav {
        if nav_section == NebulaSettingsSection::Backup && !SHOW_BACKUP_SETTINGS {
            continue;
        }
        if geometry.compact_nav {
            continue;
        }
        let active = nav_section == section;
        let hovered = view.hover == SettingsHit::Nav(nav_section);
        r.draw_chrome_text(
            size,
            nx + s(38.0),
            ny + (nh - cell_h) / 2.0,
            if active {
                sk.ink_strong
            } else if hovered {
                sk.ink
            } else {
                sk.ink_dim
            },
            nav_section.label(view.language),
            gc,
        );
    }
    let group_text_h = cell_h * 0.78;
    if !geometry.compact_nav {
        for (rect, label) in geometry
            .nav_groups
            .into_iter()
            .zip([language.pick("连接", "Connections"), language.pick("系统", "System")])
        {
            r.draw_ui_text(
                size,
                rect.0 + s(10.0),
                widgets::centered_y(rect.1, rect.3, group_text_h),
                0.78,
                sk.ink_dim,
                nebula_terminal::term::cell::Flags::empty(),
                label,
                gc,
            );
        }
    }
    // Content header: the big section title alone. (No subtitle — the nav
    // label + title already say everything; the old dim sentence only added
    // noise under the heading.)
    draw_big_text(
        r,
        gc,
        size,
        scale,
        content_x + s(24.0),
        content_y + s(20.0),
        1.6,
        sk.ink_strong,
        section.label(view.language),
    );

    match section {
        NebulaSettingsSection::Appearance => {
            // Live preview: sample lines in the CURRENT font/size on the
            // CURRENT terminal colors; the demo cursor quad shares this
            // layout via `preview_line_y`.
            {
                let (vx, vy, _, vh) = geometry.preview;
                if visible(group_y(vy), title_h) {
                    section_title(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        content_x + s(24.0),
                        group_y(vy),
                        language.pick("预览", "Preview"),
                    );
                }
                if visible(vy, vh) {
                    let fg = view.preview_fg;
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 0.0, scale),
                        fg,
                        "user@nebula ~ $ nebula --version",
                        gc,
                    );
                    let sample = format!(
                        "Nebula Terminal · {} · {:.0}px",
                        view.font_family, view.font_size_px
                    );
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 1.0, scale),
                        fg,
                        &sample,
                        gc,
                    );
                    r.draw_chrome_text(
                        size,
                        vx + s(16.0),
                        preview_line_y(vy, cell_h, 2.0, scale),
                        fg,
                        "❯",
                        gc,
                    );
                }
            }
            let cards_y = geometry.options[0].2;
            if visible(group_y(cards_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    content_x + s(24.0),
                    group_y(cards_y),
                    language.pick("主题", "Themes"),
                );
            }
            for (theme, ox, oy, ow, oh) in geometry.options {
                let selected = theme == view.theme;
                let hovered = view.hover == SettingsHit::Theme(theme);
                // The label rides the card's 2px hover lift (quads do the
                // same), and hides only when IT would cross the viewport edge
                // — a half-clipped card keeps its fully-visible label.
                let lift = if hovered && !selected { s(2.0) } else { 0.0 };
                // The mini window's prompt glyph, in the card's own accent —
                // the quads pass carries the fake-output bars, this is the one
                // real glyph. draw_ui_text rasterizes at the true tiny size
                // (GPU-stretched atlas bitmaps would go fuzzy), and its cell
                // top is placed so the glyph's midline meets the command bar's.
                let prompt_y = oy + s(18.0) - lift;
                if visible(prompt_y, cell_h) {
                    r.draw_ui_text(
                        size,
                        ox + s(18.0),
                        prompt_y,
                        0.55,
                        theme.accent(),
                        nebula_terminal::term::cell::Flags::BOLD,
                        "❯",
                        gc,
                    );
                }
                let text_y = oy + oh + s(12.0) - lift;
                if !visible(text_y, cell_h) {
                    continue;
                }
                let card_label = theme.short_label();
                r.draw_chrome_text(
                    size,
                    ox + (ow - card_label.chars().count() as f32 * cell_w) / 2.0,
                    text_y,
                    if selected {
                        sk.accent
                    } else if hovered {
                        sk.ink
                    } else {
                        sk.ink_dim
                    },
                    card_label,
                    gc,
                );
            }
            let (st_x, st_y, _, st_h) = geometry.system_theme;
            if visible(group_y(st_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    st_x,
                    group_y(st_y),
                    language.pick("主题模式", "Theme mode"),
                );
            }
            if visible(st_y, st_h) {
                r.draw_chrome_text(
                    size,
                    st_x + s(16.0),
                    row_text_y(st_y, st_h),
                    sk.ink,
                    language.pick("跟随系统明暗模式", "Follow system appearance"),
                    gc,
                );
            }
            let (bg_x, bg_y, _, bg_h) = geometry.background;
            if visible(group_y(bg_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    bg_x,
                    group_y(bg_y),
                    language.pick("自定义背景", "Custom background"),
                );
            }
            if visible(bg_y, bg_h) {
                let background_v = view
                    .background
                    .map(format_hex_rgb)
                    .unwrap_or_else(|| language.pick("主题默认", "Theme default").to_owned());
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background,
                    language.pick("背景色", "Background color"),
                    "",
                    sk.accent,
                );
                // 值画进 combobox 控件框内（chevron 井之前），右对齐到行缘
                // 会压住下拉箭头（浅色模式下重叠尤其明显）。
                combobox_value(r, gc, geometry.background, &background_v, sk.accent);
            }
            let (img_x, img_y, _, img_h) = geometry.background_image;
            let _ = img_x;
            if visible(img_y, img_h) {
                let image_v = view
                    .background_image
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_else(|| language.pick("未设置", "Not set").to_owned());
                row_label_with_right_inset(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image,
                    language.pick("背景图片", "Background image"),
                    &image_v,
                    sk.accent,
                    if view.background_image.is_some() { s(48.0) } else { 0.0 },
                );
                if view.background_image.is_some() {
                    let (cx, cy, cw, ch) = geometry.background_image_clear;
                    r.draw_chrome_text(
                        size,
                        cx + (cw - cell_w) / 2.0,
                        cy + (ch - cell_h) / 2.0,
                        if view.hover == SettingsHit::BackgroundImageClear {
                            sk.ink
                        } else {
                            sk.ink_dim
                        },
                        "↶",
                        gc,
                    );
                }
            }
            let (_, fit_y, _, fit_h) = geometry.background_image_fit;
            if visible(fit_y, fit_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_fit,
                    language.pick("背景图像拉伸模式", "Background image stretch mode"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.background_image_fit,
                    background_image_fit_label(view.background_image_fit, language),
                    sk.accent,
                );
            }
            let (_, align_y, _, align_h) = geometry.background_image_alignment;
            if visible(align_y, align_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_alignment,
                    language.pick("背景图像对齐", "Background image alignment"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.background_image_alignment,
                    background_image_alignment_label(view.background_image_alignment, language),
                    sk.accent,
                );
            }
            let (_, image_opacity_y, _, image_opacity_h) = geometry.background_image_opacity_row;
            if visible(image_opacity_y, image_opacity_h) {
                let stacked = image_opacity_h >= s(56.0);
                let text_y = if stacked {
                    image_opacity_y + s(9.0)
                } else {
                    image_opacity_y + (image_opacity_h - cell_h) / 2.0
                };
                r.draw_chrome_text(
                    size,
                    geometry.background_image_opacity_row.0 + s(16.0),
                    text_y,
                    sk.ink,
                    language.pick("背景图像不透明度", "Background image opacity"),
                    gc,
                );
                let image_opacity_v = format!("{:.0}%", view.background_image_opacity * 100.0);
                let image_opacity_cols: usize =
                    image_opacity_v.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    if stacked {
                        geometry.background_image_opacity_row.0
                            + geometry.background_image_opacity_row.2
                            - s(16.0)
                            - image_opacity_cols as f32 * cell_w
                    } else {
                        geometry.background_image_opacity_slider.0
                            - s(10.0)
                            - image_opacity_cols as f32 * cell_w
                    },
                    text_y,
                    sk.accent,
                    &image_opacity_v,
                    gc,
                );
            }
            let (_, cover_y, _, cover_h) = geometry.background_image_cover_chrome;
            if visible(cover_y, cover_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.background_image_cover_chrome,
                    language.pick(
                        "将背景图扩展到标题栏和侧边栏",
                        "Extend background image into title bar and sidebar",
                    ),
                    "",
                    sk.ink,
                );
            }
            // ---- 光标组 ----
            let (cs_x, cs_y, _, cs_h) = geometry.cursor_shape_row;
            if visible(group_y(cs_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    cs_x,
                    group_y(cs_y),
                    language.pick("光标", "Cursor"),
                );
            }
            if visible(cs_y, cs_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cursor_shape_row,
                    language.pick("光标形状", "Cursor shape"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.cursor_shape_row,
                    cursor_shape_label(view.cursor_shape, language),
                    sk.accent,
                );
            }
            let (_, blink_y, _, blink_h) = geometry.cursor_blink_row;
            if visible(blink_y, blink_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cursor_blink_row,
                    language.pick("光标闪烁", "Cursor blinking"),
                    "",
                    sk.ink,
                );
            }
            let (or_x, or_y, _, or_h) = geometry.opacity_row;
            let (lr_x, lr_y, _, lr_h) = geometry.language_row;
            if visible(group_y(lr_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    content_x + s(24.0),
                    group_y(lr_y),
                    language.pick("界面", "Interface"),
                );
            }
            if visible(lr_y, lr_h) {
                r.draw_chrome_text(
                    size,
                    lr_x + s(16.0),
                    row_text_y(lr_y, lr_h),
                    sk.ink,
                    language.pick("语言", "Language"),
                    gc,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.language_row,
                    language_label(view.language_preference, language),
                    sk.accent,
                );
            }
            if visible(geometry.density_row.1, geometry.density_row.3) {
                let (dr_x, dr_y, _, dr_h) = geometry.density_row;
                r.draw_chrome_text(
                    size,
                    dr_x + s(16.0),
                    row_text_y(dr_y, dr_h),
                    sk.ink,
                    language.pick("界面外观", "Appearance"),
                    gc,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.density_row,
                    density_label(view.density, language),
                    sk.accent,
                );
            }
            if visible(or_y, or_h) {
                let stacked = or_h >= s(56.0);
                let text_y = if stacked { or_y + s(9.0) } else { or_y + (or_h - cell_h) / 2.0 };
                r.draw_chrome_text(
                    size,
                    or_x + s(16.0),
                    text_y,
                    sk.ink,
                    language.pick("终端正文不透明度", "Terminal content opacity"),
                    gc,
                );
                let opacity_v = format!("{:.0}%", view.opacity * 100.0);
                let opacity_cols: usize = opacity_v.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    if stacked {
                        or_x + geometry.opacity_row.2 - s(16.0) - opacity_cols as f32 * cell_w
                    } else {
                        geometry.opacity_slider.0 - s(10.0) - opacity_cols as f32 * cell_w
                    },
                    text_y,
                    sk.accent,
                    &opacity_v,
                    gc,
                );
            }
            let (br_x, br_y, _, br_h) = geometry.blur;
            if visible(br_y, br_h) {
                // 文案说的是效果不是实现：用户认的是"背景糊不糊"，不是
                // Mica 这个 Windows 专有名词——而且这个开关在 macOS 上走的
                // 是另一套实现。
                r.draw_chrome_text(
                    size,
                    br_x + s(16.0),
                    row_text_y(br_y, br_h),
                    sk.ink,
                    language.pick("背景模糊", "Blur behind window"),
                    gc,
                );
            }
            let (fa_x, fa_y, _, fa_h) = geometry.font_size_row;
            if visible(group_y(fa_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    fa_x,
                    group_y(fa_y),
                    language.pick("终端外观", "Terminal appearance"),
                );
            }
            if visible(fa_y, fa_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.font_size_row,
                    language.pick("终端字号（Ctrl+滚轮缩放）", "Font size (Ctrl+wheel zooms)"),
                    "",
                    sk.ink,
                );
                let (value_box, _, _) = widgets::spinner_rects(geometry.font_size_row, scale);
                let value = format!("{:.0}", view.font_size_px);
                let cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
                r.draw_chrome_text(
                    size,
                    value_box.0 + (value_box.2 - cols as f32 * cell_w) / 2.0,
                    value_box.1 + (value_box.3 - cell_h) / 2.0,
                    sk.ink,
                    &value,
                    gc,
                );
            }
            if visible(geometry.cell_width_mode.1, geometry.cell_width_mode.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cell_width_mode,
                    language.pick("字体间距", "Font spacing"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.cell_width_mode,
                    cell_width_mode_label(view.cell_width_mode, language),
                    sk.accent,
                );
            }
            if visible(geometry.fetch.1, geometry.fetch.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.fetch,
                    language.pick("启动欢迎信息", "Startup welcome"),
                    "",
                    sk.ink,
                );
            }
            if visible(geometry.powerline.1, geometry.powerline.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.powerline,
                    language.pick("Powerline 提示符", "Powerline prompt"),
                    "",
                    sk.ink,
                );
            }
        },
        NebulaSettingsSection::Profiles => {
            // Rows carry single, self-explanatory Chinese labels — the old
            // second-line descriptions overflowed the 44px rows and collided
            // with the next group's title.
            let (sh_x, sh_y, _, sh_h) = geometry.shell;
            if visible(group_y(sh_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    sh_x,
                    group_y(sh_y),
                    language.pick("终端", "Terminal"),
                );
            }
            if visible(sh_y, sh_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.shell,
                    language.pick("默认 Shell", "Default shell"),
                    "",
                    sk.ink,
                );
                combobox_value(r, gc, geometry.shell, &view.shell_label, sk.accent);
            }
            let (_ix, iy, _, ih) = geometry.terminal_import;
            if visible(iy, ih) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.terminal_import,
                    language.pick("导入终端目录", "Import terminal directory"),
                    "",
                    sk.ink,
                );
                draw_button_label(
                    r,
                    gc,
                    size,
                    row_action_rect(geometry.terminal_import, scale, STANDARD_ROW_ACTION_W),
                    language.pick("导入", "Import"),
                    if view.hover == SettingsHit::ImportTerminal { sk.accent } else { sk.ink },
                );
            }
            if visible(geometry.startup_directory.1, geometry.startup_directory.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.startup_directory,
                    language.pick("启动目录", "Startup directory"),
                    "",
                    sk.ink,
                );

                let (dx, dy, dw, dh) = geometry.startup_directory;
                // 与"默认 Shell / 终端字体"同一右对齐基线；有清除按钮时向左避让。
                let stacked = dh >= s(56.0);
                let value_left = if stacked { dx + s(16.0) } else { dx + dw * 0.42 };
                let value_right = if view.startup_directory.is_some() {
                    geometry.startup_directory_clear.0 - s(12.0)
                } else {
                    dx + dw - s(16.0)
                };
                let max_chars = ((value_right - value_left).max(cell_w) / cell_w).floor() as usize;
                let value = view
                    .startup_directory
                    .as_deref()
                    .unwrap_or_else(|| language.pick("继承当前目录", "Inherit current directory"));
                let value = truncate_tab_label(value, max_chars.max(1));
                let value_cols: usize = value.chars().map(|c| c.width().unwrap_or(0)).sum();
                let value_x = (value_right - value_cols as f32 * cell_w).max(value_left);

                let value_y = if stacked { dy + s(9.0) + cell_h } else { dy + (dh - cell_h) / 2.0 };
                r.draw_chrome_text(
                    size,
                    if stacked { value_left } else { value_x },
                    value_y,
                    if view.startup_directory.is_some() { sk.accent } else { sk.ink_dim },
                    &value,
                    gc,
                );

                if view.startup_directory.is_some() {
                    let (cx, cy, cw, ch) = geometry.startup_directory_clear;
                    let clear = language.pick("清除", "Clear");
                    let clear_cols: usize =
                        clear.chars().map(|character| character.width().unwrap_or(0)).sum();
                    r.draw_chrome_text(
                        size,
                        cx + (cw - clear_cols as f32 * cell_w) / 2.0,
                        cy + (ch - cell_h) / 2.0,
                        sk.accent,
                        clear,
                        gc,
                    );
                }
            }
            if visible(geometry.font.1, geometry.font.3) {
                // 查询串现在长在弹层顶部的搜索框里，触发器只管报当前字体。
                // 加载失败的告警优先。
                let font_value = match view.font_notice.as_deref() {
                    Some(notice) => notice,
                    None => &view.font_family,
                };
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.font,
                    language.pick("终端字体", "Terminal font"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.font,
                    font_value,
                    if view.font_notice.is_some() { sk.ink_dim } else { sk.accent },
                );
            }
            let (gh_x, gh_y, _, gh_h) = geometry.ghost;
            if visible(group_y(gh_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    gh_x,
                    group_y(gh_y),
                    language.pick("补全", "Completion"),
                );
            }
            if visible(gh_y, gh_h) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.ghost,
                    language.pick("历史补全灰字", "History ghost text"),
                    "",
                    sk.ink,
                );
            }
            if visible(geometry.accept.1, geometry.accept.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.accept,
                    language.pick("补全接受键", "Completion accept key"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.accept,
                    accept_label(view.accept, language),
                    sk.accent,
                );
            }

            let (ocx, ocy, _ocw, och) = geometry.open_config_file;
            if visible(group_y(ocy), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ocx,
                    group_y(ocy),
                    language.pick("配置文件", "Configuration"),
                );
            }
            if visible(ocy, och) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.open_config_file,
                    language.pick("打开配置文件", "Open configuration file"),
                    "",
                    sk.ink,
                );
                draw_button_label(
                    r,
                    gc,
                    size,
                    row_action_rect(geometry.open_config_file, scale, STANDARD_ROW_ACTION_W),
                    language.pick("打开", "Open"),
                    if view.hover == SettingsHit::OpenConfigFile { sk.accent } else { sk.ink },
                );
            }
        },
        NebulaSettingsSection::Providers => {
            let (list_x, list_y, _, _) = geometry.provider_row0;
            if visible(group_y(list_y), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    list_x,
                    group_y(list_y),
                    language.pick("AI 供应商", "AI providers"),
                );
            }
            if visible(geometry.provider_add.1, geometry.provider_add.3) {
                draw_button_label(
                    r,
                    gc,
                    size,
                    geometry.provider_add,
                    language.pick("＋ 添加", "+ Add"),
                    if view.hover == SettingsHit::ProviderAdd { sk.accent } else { sk.ink },
                );
            }
            for (index, provider) in view.providers.iter().enumerate() {
                let row = (
                    geometry.provider_row0.0,
                    geometry.provider_row0.1 + index as f32 * geometry.provider_row_h,
                    geometry.provider_row0.2,
                    geometry.provider_row_h,
                );
                if !visible(row.1, row.3) {
                    continue;
                }
                let active = provider.id == view.active_provider_id;
                let value = if provider.model.is_empty() {
                    provider.kind.label()
                } else {
                    provider.model.as_str()
                };
                row_label_with_right_inset(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    row,
                    &provider.name,
                    value,
                    if active { sk.accent } else { sk.ink_dim },
                    s(76.0),
                );
                let enabled = provider.enabled;
                let state = if enabled {
                    language.pick("启用", "On")
                } else {
                    language.pick("关闭", "Off")
                };
                let cols = state.chars().map(|ch| ch.width().unwrap_or(1)).sum::<usize>();
                r.draw_chrome_text(
                    size,
                    row.0 + row.2 - s(16.0) - cols as f32 * cell_w,
                    widgets::centered_y(row.1, row.3, cell_h),
                    if enabled { sk.accent } else { sk.ink_dim },
                    state,
                    gc,
                );
            }

            for (index, row) in geometry.provider_fields.iter().enumerate() {
                if !visible(row.1, row.3) {
                    continue;
                }
                let label = match index {
                    0 => language.pick("供应商名称", "Provider name"),
                    1 => language.pick("备注", "Note"),
                    2 => language.pick("官网链接", "Website"),
                    3 => language.pick("API 请求地址", "API endpoint"),
                    4 => language.pick("默认模型", "Default model"),
                    _ => "API Key",
                };
                row_label(r, gc, size, scale, &sk, *row, label, "", sk.ink);
                let input = sync_input_rect(*row, scale);
                let max_cols = ((input.2 - s(24.0)).max(cell_w) / cell_w).floor() as usize;
                let (value, placeholder, _) = provider_input_display(view, index, max_cols.max(1));
                r.draw_chrome_text(
                    size,
                    input.0 + s(12.0),
                    widgets::centered_y(input.1, input.3, cell_h),
                    if placeholder { sk.ink_dim } else { sk.ink },
                    &value,
                    gc,
                );
            }
            for (row, label, detail) in [
                (
                    geometry.provider_codex_goals,
                    "Codex Goal mode",
                    language.pick("写入 features.goals", "Writes features.goals"),
                ),
                (
                    geometry.provider_codex_remote,
                    language.pick("Codex 远程压缩", "Codex remote compaction"),
                    language.pick(
                        "写入 features.remote_compaction_v2",
                        "Writes features.remote_compaction_v2",
                    ),
                ),
            ] {
                if visible(row.1, row.3) {
                    row_label(r, gc, size, scale, &sk, row, label, detail, sk.ink_dim);
                }
            }
            if visible(geometry.provider_codex_apply.1, geometry.provider_codex_apply.3) {
                row_label_with_right_inset(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.provider_codex_apply,
                    language.pick("Codex 配置", "Codex configuration"),
                    language.pick("写入 auth.json / config.toml", "Write auth.json / config.toml"),
                    sk.ink_dim,
                    s(164.0),
                );
                draw_button_label(
                    r,
                    gc,
                    size,
                    row_action_rect(geometry.provider_codex_apply, scale, 148.0),
                    language.pick("应用到 Codex", "Apply to Codex"),
                    if view.hover == SettingsHit::ProviderApplyCodex { sk.accent } else { sk.ink },
                );
            }
            for (hit, rect, label) in [
                (
                    SettingsHit::ProviderDelete,
                    geometry.provider_delete,
                    language.pick("删除", "Delete"),
                ),
                (
                    SettingsHit::ProviderTest,
                    geometry.provider_test,
                    language.pick("测试连接", "Test"),
                ),
                (SettingsHit::ProviderSave, geometry.provider_save, language.pick("保存", "Save")),
            ] {
                if visible(rect.1, rect.3) {
                    draw_button_label(
                        r,
                        gc,
                        size,
                        rect,
                        label,
                        if view.hover == hit { sk.accent } else { sk.ink },
                    );
                }
            }
            if let Some((message, is_error)) = &view.provider_status {
                let y = geometry.provider_save.1 + geometry.provider_save.3 + s(8.0);
                if visible(y, cell_h) {
                    let max_cols = (geometry.provider_row0.2 / cell_w).floor().max(1.0) as usize;
                    let message = truncate_tab_label(message, max_cols);
                    r.draw_chrome_text(
                        size,
                        geometry.provider_delete.0,
                        y,
                        if *is_error {
                            Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b)
                        } else {
                            sk.accent
                        },
                        &message,
                        gc,
                    );
                }
            }
        },
        NebulaSettingsSection::Ssh => {
            let (host_x, host_y, host_w, _host_h) = geometry.ssh_host_row0;
            if visible(group_y(host_y), title_h) {
                let available = (geometry.ssh_add_host.0 - host_x - s(8.0)).max(cell_w);
                let max_chars = (available / (cell_w * 1.2)).floor().max(1.0) as usize;
                let saved_hosts =
                    truncate_tab_label(language.pick("已保存主机", "Saved hosts"), max_chars);
                section_title(r, gc, size, scale, &sk, host_x, group_y(host_y), &saved_hosts);
            }
            let add = geometry.ssh_add_host;
            if add.2 >= s(96.0) && visible(add.1, add.3) {
                r.draw_chrome_text(
                    size,
                    add.0 + s(36.0),
                    widgets::centered_y(add.1, add.3, cell_h),
                    if view.hover == SettingsHit::SshAddHost { sk.accent } else { sk.ink },
                    language.pick("添加主机", "Add host"),
                    gc,
                );
            }
            let draw_centered_action = |r: &mut Renderer,
                                        gc: &mut GlyphCache,
                                        rect: (f32, f32, f32, f32),
                                        label: &str,
                                        ink: Rgb| {
                let cols = label.chars().map(|ch| ch.width().unwrap_or(1)).sum::<usize>();
                let tx = rect.0 + (rect.2 - cols as f32 * cell_w) * 0.5;
                r.draw_chrome_text(size, tx, rect.1 + (rect.3 - cell_h) / 2.0, ink, label, gc);
            };
            for (index, host) in view.ssh_hosts.iter().enumerate() {
                let row = (
                    host_x,
                    host_y + index as f32 * (geometry.ssh_host_row_h + geometry.ssh_host_gap),
                    host_w,
                    geometry.ssh_host_row_h,
                );
                if !visible(row.1, row.3) {
                    continue;
                }
                let detail_h = cell_h * 0.78;
                let title_y = widgets::centered_y(row.1, row.3, cell_h + detail_h);
                // 左缘 OS 图标：与侧栏主机行同一套 os_icons 配方——真墨迹按
                // `ink_em` 反解缩放到统一目标宽（Nerd Font 图标墨迹 0.76~1.20 em
                // 各不相同，不缩就有的顶文字有的悬空），跨双行垂直居中。id 来自
                // 主机存储，auto / 未认出回落通用终端形状。
                let icon = os_icons::resolve(Some(host.icon.as_str()));
                let icon_slot = (cell_h * 0.72).round();
                let icon_px = icon_slot * 0.82;
                let icon_mult = os_icons::scale_for(icon, size.cell_width(), icon_px);
                r.draw_chrome_text_scaled(
                    size,
                    row.0 + s(20.0) - icon_slot * 0.5 + (icon_slot - icon_px) * 0.5,
                    widgets::centered_y(row.1, row.3, cell_h * icon_mult),
                    icon_mult,
                    sk.icon,
                    icon.glyph.encode_utf8(&mut [0u8; 4]),
                    gc,
                );
                r.draw_chrome_text(size, row.0 + s(38.0), title_y, sk.ink, &host.label, gc);
                r.draw_ui_text(
                    size,
                    row.0 + s(38.0),
                    title_y + cell_h * 0.95,
                    0.78,
                    sk.ink_dim,
                    nebula_terminal::term::cell::Flags::empty(),
                    &host.destination,
                    gc,
                );
                if host.pinned {
                    r.draw_chrome_text(size, row.0 + s(27.0), title_y, sk.accent, "\u{eab4}", gc);
                }
                // 三枚动作图标改由 quad pass 绘制（icons::push_row_action_icon），
                // 文字 pass 这里不再画「连接 / 编辑 / 隐藏」三个词。
            }
            if visible(geometry.ssh_import_config.1, geometry.ssh_import_config.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.ssh_import_config,
                    language.pick("导入 ~/.ssh/config", "Import ~/.ssh/config"),
                    "",
                    sk.ink,
                );
                let action =
                    row_action_rect(geometry.ssh_import_config, scale, STANDARD_ROW_ACTION_W);
                draw_button_label(
                    r,
                    gc,
                    size,
                    action,
                    language.pick("立即刷新", "Refresh now"),
                    if view.hover == SettingsHit::SshImportConfig { sk.accent } else { sk.ink_dim },
                );
            }
            if geometry.hidden_host_count > 0 {
                let (hx, hy, hw, hh) = geometry.hidden_host_row0;
                if visible(group_y(hy), title_h) {
                    section_title(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        hx,
                        group_y(hy),
                        language.pick("已隐藏主机", "Hidden hosts"),
                    );
                }
                for (index, host) in view.hidden_hosts.iter().enumerate() {
                    let rect = (hx, hy + index as f32 * hh, hw, hh);
                    if visible(rect.1, rect.3) {
                        row_label(
                            r,
                            gc,
                            size,
                            scale,
                            &sk,
                            rect,
                            host,
                            language.pick("恢复", "Restore"),
                            sk.accent,
                        );
                    }
                }
            }
        },
        NebulaSettingsSection::Proxy => {
            let (gx, test_y, ..) = geometry.ssh_proxy_test;
            let title_y = proxy_section_title_y(test_y, scale);
            if visible(title_y, title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    gx,
                    title_y,
                    language.pick("网络代理", "Network proxy"),
                );
            }
            if visible(geometry.ssh_proxy_mode.1, geometry.ssh_proxy_mode.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.ssh_proxy_mode,
                    language.pick("代理方式", "Proxy setting"),
                    "",
                    sk.ink,
                );
                combobox_value_rect(
                    r,
                    gc,
                    ssh_proxy_mode_control(geometry.ssh_proxy_mode, scale),
                    ssh_proxy_mode_label(view.ssh_proxy_mode, language),
                    sk.accent,
                );
            }
            let cell_w = size.cell_width();
            let cell_h = size.cell_height();

            if view.ssh_proxy_mode == crate::ssh_proxy::ProxyMode::Custom {
                let row = geometry.ssh_proxy_expand;
                if visible(row.1, row.3) {
                    row_label(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        row,
                        language.pick("代理地址", "Proxy address"),
                        "",
                        sk.ink,
                    );
                    let (protocol, address) = ssh_proxy_manual_controls(row, scale);
                    combobox_value_rect(
                        r,
                        gc,
                        protocol,
                        manual_proxy_protocol_label(view.ssh_proxy_protocol, language),
                        sk.accent,
                    );
                    let (ix, iy, iw, ih) = address;
                    let max_cols = (((iw - s(24.0)) / cell_w) as usize).max(1);
                    let (text, placeholder, _, _) = ssh_proxy_input_display(view, 0, max_cols);
                    r.draw_chrome_text(
                        size,
                        ix + s(12.0),
                        iy + (ih - cell_h) / 2.0,
                        if placeholder { sk.ink_dim } else { sk.ink },
                        &text,
                        gc,
                    );
                }
            }

            let banner = geometry.ssh_proxy_test;
            if visible(banner.1, banner.3) {
                let button = ssh_proxy_test_button(banner, scale);
                let (status, status_ink) = match &view.proxy_test_status {
                    ProxyTestStatus::Idle => (
                        language
                            .pick(
                                "测试当前设置是否可以访问网络",
                                "Test whether the current setting can access the network",
                            )
                            .to_owned(),
                        sk.ink_dim,
                    ),
                    ProxyTestStatus::Running => (
                        language
                            .pick(
                                "正在通过当前设置测试网络…",
                                "Testing through the current setting…",
                            )
                            .to_owned(),
                        sk.accent,
                    ),
                    ProxyTestStatus::Success { elapsed_ms, route } => (
                        format!(
                            "{} · {route} · {elapsed_ms} ms",
                            language.pick("网络连接正常", "Network connection succeeded"),
                        ),
                        Rgb::new(sk.ok.r, sk.ok.g, sk.ok.b),
                    ),
                    ProxyTestStatus::Failed { message } => (
                        format!("{}：{message}", language.pick("测试失败", "Test failed")),
                        Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b),
                    ),
                };
                let available = (button.0 - banner.0 - s(28.0)).max(cell_w);
                let max_chars = (available / cell_w).floor().max(1.0) as usize;
                let status = truncate_tab_label(&status, max_chars);
                r.draw_chrome_text(
                    size,
                    banner.0 + s(14.0),
                    banner.1 + (banner.3 - cell_h) / 2.0,
                    status_ink,
                    &status,
                    gc,
                );
                let caption = if matches!(view.proxy_test_status, ProxyTestStatus::Running) {
                    language.pick("测试中…", "Testing…")
                } else {
                    language.pick("测试网络", "Test network")
                };
                let caption_cols: usize =
                    caption.chars().map(|ch| ch.width().unwrap_or(1).max(1)).sum();
                r.draw_chrome_text(
                    size,
                    button.0 + (button.2 - caption_cols as f32 * cell_w) / 2.0,
                    button.1 + (button.3 - cell_h) / 2.0,
                    if matches!(view.proxy_test_status, ProxyTestStatus::Running) {
                        sk.ink_faint
                    } else {
                        sk.ink_dim
                    },
                    caption,
                    gc,
                );
            }
        },
        NebulaSettingsSection::Interaction => {
            let (ix, iy, _, ih) = geometry.copy_on_select;
            if visible(group_y(iy), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ix,
                    group_y(iy),
                    language.pick("剪贴板", "Clipboard"),
                );
            }
            if visible(iy, ih) {
                // The switch (drawn in `push_quads`) carries the state; the
                // label spells out the OFF fallback so both modes are clear.
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.copy_on_select,
                    language.pick(
                        "自动将所选内容复制到剪贴板（关闭时右键复制 / 粘贴）",
                        "Copy selection to clipboard (off: right-click copies / pastes)",
                    ),
                    "",
                    sk.ink,
                );
            }
            if visible(geometry.tab_reveal.1, geometry.tab_reveal.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.tab_reveal,
                    language.pick("标签展开", "Tab reveal"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.tab_reveal,
                    tab_reveal_label(view.tab_reveal, language),
                    sk.accent,
                );
            }
            if visible(geometry.new_tab_position.1, geometry.new_tab_position.3) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.new_tab_position,
                    language.pick("新标签位置", "New tab position"),
                    "",
                    sk.ink,
                );
                combobox_value(
                    r,
                    gc,
                    geometry.new_tab_position,
                    new_tab_position_label(view.new_tab_position, language),
                    sk.accent,
                );
            }
            if visible(geometry.panel_resize.1, geometry.panel_resize.3) {
                // 开关本体在 quads pass；开启前的性能告知走确认框，这里的
                // label 只说清楚它管哪三条分界线。
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.panel_resize,
                    language.pick(
                        "拖拽调节侧栏宽度（左侧栏 / 右抽屉；SSH 分界高度无需开关）",
                        "Drag to resize panel widths (sidebar / drawer)",
                    ),
                    "",
                    sk.ink,
                );
            }
            let (bx, by, _, bh) = geometry.cjk_bold;
            if visible(group_y(by), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    bx,
                    group_y(by),
                    language.pick("文本渲染", "Text rendering"),
                );
            }
            if visible(by, bh) {
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.cjk_bold,
                    language.pick(
                        "中文粗体只提亮不加粗（避免小字号下笔画发闷）",
                        "Render CJK bold with regular glyphs (avoids muddy strokes)",
                    ),
                    "",
                    sk.ink,
                );
            }
        },
        NebulaSettingsSection::Keymap => {
            // 搜索框文字：查询串或占位；聚焦态的 caret 在 quad pass。
            {
                let (sx, sy, _, sh) = geometry.keymap_search;
                if visible(sy, sh) {
                    let showing = !view.keymap_query.is_empty();
                    let text = if showing {
                        view.keymap_query.clone()
                    } else {
                        language.pick("搜索动作或按键…", "Search actions or keys…").to_owned()
                    };
                    let ink = if showing { sk.ink } else { sk.ink_faint };
                    r.draw_chrome_text(
                        size,
                        sx + s(12.0),
                        sy + (sh - cell_h) / 2.0,
                        ink,
                        &text,
                        gc,
                    );
                }
            }
            // 冲突提示句必须写清哪个绑定不生效，不能让配置静默失效。
            if let Some(note) = &view.keymap_clash_note {
                let (nx, ny, nw, nh) = geometry.keymap_note;
                if geometry.keymap_pane.clash && visible(ny, nh) {
                    let icon_y = ny + (nh - cell_h) / 2.0;
                    let warn_ink = Rgb::new(sk.warn.r, sk.warn.g, sk.warn.b);
                    r.draw_chrome_text(size, nx + s(9.0), icon_y, warn_ink, "!", gc);
                    let max_cols =
                        (((nw - s(38.0)).max(cell_w)) / cell_w).floor().max(1.0) as usize;
                    let lines = warning_lines(note, max_cols);
                    let text_x = nx + s(30.0);
                    r.draw_chrome_text(size, text_x, ny + s(6.0), warn_ink, &lines[0], gc);
                    if !lines[1].is_empty() {
                        r.draw_chrome_text(
                            size,
                            text_x,
                            ny + s(6.0) + cell_h,
                            sk.ink_dim,
                            &lines[1],
                            gc,
                        );
                    }
                }
            }
            // 分组标题（无框分组：标题 + 间距承担层级）；下标 5 = 固定组。
            for (group, title_y) in geometry.keymap_title_ys.iter().enumerate() {
                if !title_y.is_finite() || !visible(*title_y, title_h) {
                    continue;
                }
                let (zh, en) = match keymap::GROUPS.get(group) {
                    Some((zh, en, _)) => (*zh, *en),
                    None => ("固定快捷键", "Fixed shortcuts"),
                };
                keymap_group_title(
                    r,
                    gc,
                    size,
                    geometry.keymap_row0.0 + s(4.0),
                    *title_y,
                    language.pick(zh, en),
                    sk.ink_dim,
                );
            }
            let (kx, _, kw, kh) = geometry.keymap_row0;
            // 行矩形按文字行剔除：quad 走 scissor 能画半行，文字只要居中
            // 的字盒仍完整落在视口内就照画——否则底部半行只剩空 keycap。
            let line_visible = |ry: f32, rh: f32| {
                let ty = ry + (rh - cell_h) / 2.0;
                ty >= clip_top && ty + cell_h <= clip_bot
            };
            if view.keymap_visible.is_empty() {
                let (_, ey, ..) = geometry.keymap_row0;
                if visible(ey, kh) {
                    r.draw_chrome_text(
                        size,
                        kx + s(4.0),
                        ey + s(6.0),
                        sk.ink_faint,
                        language.pick("没有匹配的动作或按键。", "No actions or keys match."),
                        gc,
                    );
                }
            }
            for (slot, flat) in view.keymap_visible.iter().copied().enumerate() {
                if slot >= geometry.keymap_slot_ys.len() {
                    break;
                }
                let rect = (kx, geometry.keymap_slot_ys[slot], kw, kh);
                if !line_visible(rect.1, rect.3) {
                    continue;
                }
                let i = flat;
                let ty = rect.1 + (kh - cell_h) / 2.0;
                let (zh_label, en_label) = if i == keymap::QUICK_TERMINAL_ROW {
                    if view.quick_hotkey_error.is_some() {
                        ("快速终端（注册失败）", "Quick terminal (failed)")
                    } else {
                        ("快速终端", "Quick terminal")
                    }
                } else {
                    let (_, zh, en) = keymap::EDITABLE_ACTIONS[i - 1];
                    (zh, en)
                };
                r.draw_chrome_text(
                    size,
                    rect.0 + s(16.0),
                    ty,
                    if i == keymap::QUICK_TERMINAL_ROW && view.quick_hotkey_error.is_some() {
                        if sk.is_light { Rgb::new(207, 34, 46) } else { Rgb::new(248, 81, 73) }
                    } else {
                        sk.ink
                    },
                    language.pick(zh_label, en_label),
                    gc,
                );
                let hovered = view.hover == SettingsHit::KeymapRow(slot);
                let capturing = view.keymap_capture == Some(i);
                let (value, customized, bound) = keymap_row_value(view, i);
                let clash = view.keymap_clash_rows.get(i).copied().unwrap_or(false);
                // 墨色分级（2026-08-09 对齐原型 .kbd）：冲突 danger、捕获/
                // 自定义 accent、hover 提一档、默认键 ink_dim（回滚: sk.ink）、
                // 未绑定 ink_faint（回滚: sk.ink_dim）。
                let ink = if capturing {
                    sk.accent
                } else if clash {
                    Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b)
                } else if hovered && bound {
                    sk.ink_strong
                } else if customized {
                    sk.accent
                } else if bound {
                    sk.ink_dim
                } else {
                    sk.ink_faint
                };
                if capturing || !bound {
                    // 捕获提示 / 未绑定占位仍是整段文本（不是键位展示）。
                    let (cap_x, ..) = keymap_keycap_rect(rect, &value, cell_w, scale);
                    r.draw_chrome_text(size, cap_x + s(12.0), ty, ink, &value, gc);
                } else {
                    // 键帽规范：一颗 chip 承载整串键位（Windows 心智）。
                    let combo = super::ui::keycap::layout_combo(
                        &value,
                        rect.0 + rect.2 - s(16.0),
                        rect.1 + rect.3 / 2.0,
                        cell_w,
                        scale,
                    );
                    for (chip_x, chip_w, key) in &combo.chips {
                        let key_cols: usize = key.chars().map(|c| c.width().unwrap_or(0)).sum();
                        r.draw_chrome_text(
                            size,
                            chip_x + (chip_w - key_cols as f32 * cell_w) / 2.0,
                            ty,
                            ink,
                            key,
                            gc,
                        );
                    }
                    // hover 才浮现「改键」提示（原型 .rebind 语义）：整行
                    // 本就可点，这里只补可见性，不加新命中。
                    if hovered && !capturing {
                        let rebind = language.pick("改键", "Rebind");
                        let rebind_cols: usize =
                            rebind.chars().map(|c| c.width().unwrap_or(1)).sum();
                        r.draw_chrome_text(
                            size,
                            combo.bounds.0 - s(12.0) - rebind_cols as f32 * cell_w,
                            ty,
                            sk.accent,
                            rebind,
                            gc,
                        );
                    }
                }
            }
            let (rx, ry, rw, rh) = geometry.keymap_readonly_row0;
            for (row, flat) in view.keymap_readonly_visible.iter().copied().enumerate() {
                let Some((zh_label, en_label, combo)) = keymap::READONLY_ROWS.get(flat) else {
                    continue;
                };
                let rect = (rx, ry + row as f32 * rh, rw, rh);
                if line_visible(rect.1, rect.3) {
                    row_label(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        rect,
                        view.language.pick(zh_label, en_label),
                        combo,
                        sk.ink_dim,
                    );
                }
            }
            // 页尾提示：改键入口与恢复默认的操作说明（原页首长标题的归宿）。
            if visible(geometry.keymap_hint_y, cell_h) {
                r.draw_chrome_text(
                    size,
                    kx + s(4.0),
                    geometry.keymap_hint_y,
                    sk.ink_faint,
                    language.pick(
                        "点击行改键 · 捕获时按 Backspace 恢复默认绑定",
                        "Click a row to rebind · Backspace during capture restores the default",
                    ),
                    gc,
                );
            }
        },
        NebulaSettingsSection::Advanced => {
            let (ax, ay, _, ah) = geometry.keep_session;
            if visible(group_y(ay), title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    ax,
                    group_y(ay),
                    language.pick("会话", "Sessions"),
                );
            }
            if visible(ay, ah) {
                // The switch (drawn in `push_quads`) carries the state; the
                // label says what closing a window keeps alive while it is ON.
                row_label(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.keep_session,
                    language.pick(
                        "关闭窗口后保留会话（后台驻留，可恢复对话）",
                        "Keep sessions after closing the window (resident and restorable)",
                    ),
                    "",
                    sk.ink,
                );
            }
            {
                let (_, ry, _, rh) = geometry.restore_session;
                if visible(ry, rh) {
                    // 关掉它就永远干净启动：session.json 照写不误（导出工作区
                    // 与崩溃诊断都靠它），只是开机不再回放。
                    row_label(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        geometry.restore_session,
                        language.pick(
                            "启动时恢复上次的标签（异常退出后同样恢复）",
                            "Restore last tabs on launch (also after a crash)",
                        ),
                        "",
                        sk.ink,
                    );
                }
            }

            if SHOW_WEBDAV_SYNC_SETTINGS {
                // ---- 同步（WebDAV）----
                let (sx, sy, ..) = geometry.sync_rows[0];
                if visible(group_y(sy), title_h) {
                    section_title(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        sx,
                        group_y(sy),
                        language.pick("同步（WebDAV）", "Sync (WebDAV)"),
                    );
                }
                let labels = [
                    language.pick("服务器文件 URL", "Server file URL"),
                    language.pick("用户名", "Username"),
                    language.pick("WebDAV 密码", "WebDAV password"),
                    language.pick("端到端口令", "End-to-end passphrase"),
                ];
                let cell_w = size.cell_width();
                let cell_h = size.cell_height();
                for (index, row) in geometry.sync_rows.iter().enumerate() {
                    if !visible(row.1, row.3) {
                        continue;
                    }
                    row_label(r, gc, size, scale, &sk, *row, labels[index], "", sk.ink);
                    let (ix, iy, iw, ih) = sync_input_rect(*row, scale);
                    let max_cols = (((iw - s(24.0)) / cell_w) as usize).max(1);
                    let (text, placeholder, _) = sync_input_display(view, index, max_cols);
                    let ink = if placeholder { sk.ink_dim } else { sk.ink };
                    r.draw_chrome_text(
                        size,
                        ix + s(12.0),
                        iy + (ih - cell_h) / 2.0,
                        ink,
                        &text,
                        gc,
                    );
                }
                if visible(geometry.sync_auto_pull.1, geometry.sync_auto_pull.3) {
                    row_label(
                        r,
                        gc,
                        size,
                        scale,
                        &sk,
                        geometry.sync_auto_pull,
                        language.pick("启动时自动拉取", "Pull automatically on startup"),
                        "",
                        sk.ink,
                    );
                }
                let (_, by, _, bh) = geometry.sync_actions;
                if visible(by, bh + s(30.0)) {
                    let [push_rect, pull_rect] = sync_button_rects(geometry.sync_actions, scale);
                    let captions = [
                        (push_rect, language.pick("立即推送", "Push now")),
                        (pull_rect, language.pick("立即拉取", "Pull now")),
                    ];
                    for ((bx, byy, bw, bhh), caption) in captions {
                        let cols: usize =
                            caption.chars().map(|c| c.width().unwrap_or(1).max(1)).sum();
                        let ink = if view.sync_busy { sk.ink_dim } else { sk.ink };
                        r.draw_chrome_text(
                            size,
                            bx + (bw - cols as f32 * cell_w) / 2.0,
                            byy + (bhh - cell_h) / 2.0,
                            ink,
                            caption,
                            gc,
                        );
                    }
                    // 状态行：最近一次动作结果（错误红、成功淡墨）。
                    if let Some((message, error)) = &view.sync_status {
                        let ink = if *error {
                            Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b)
                        } else {
                            sk.ink_dim
                        };
                        r.draw_chrome_text(size, sx, by + bh + s(8.0), ink, message, gc);
                    }
                }
            }
        },
        NebulaSettingsSection::Backup => {
            r.draw_chrome_text(
                size,
                content_x + s(24.0),
                content_y + s(49.0),
                sk.ink_faint,
                language.pick(
                    "导出、恢复与自动备份 · 加密文件可跨设备迁移",
                    "Export, restore, and automatic backups · encrypted and portable",
                ),
                gc,
            );

            let (ax, ay, _, ah) = geometry.backup_auto;
            if visible(ay, ah) {
                r.draw_chrome_text(
                    size,
                    ax + s(64.0),
                    ay + s(11.0),
                    sk.ink_strong,
                    language.pick("自动备份", "Automatic backup"),
                    gc,
                );
                r.draw_chrome_text(
                    size,
                    ax + s(64.0),
                    ay + s(11.0) + cell_h,
                    sk.ink_faint,
                    language.pick(
                        "恢复预览与回滚流程完成后开放，当前请使用手动导出",
                        "Available after restore preview and rollback are complete; use manual export for now",
                    ),
                    gc,
                );
            }

            let [export, restore] = backup_segment_rects(geometry.backup_segment, scale);
            for ((bx, by, bw, bh), caption, active) in [
                (export, language.pick("导出备份", "Export backup"), true),
                (restore, language.pick("恢复备份", "Restore backup"), false),
            ] {
                if visible(by, bh) {
                    let cols =
                        caption.chars().map(|c| c.width().unwrap_or(1).max(1)).sum::<usize>();
                    r.draw_chrome_text(
                        size,
                        bx + (bw - cols as f32 * cell_w) / 2.0,
                        by + (bh - cell_h) / 2.0,
                        if active { sk.ink_strong } else { sk.ink_dim },
                        caption,
                        gc,
                    );
                }
            }

            let title_y = geometry.backup_groups[0].1 - s(30.0);
            if visible(title_y, title_h) {
                section_title(
                    r,
                    gc,
                    size,
                    scale,
                    &sk,
                    geometry.backup_groups[0].0,
                    title_y,
                    language.pick("导出内容", "Backup contents"),
                );
            }

            let group_labels = [
                language.pick("设置  ·  3 项", "SETTINGS  ·  3 ITEMS"),
                language.pick("SSH  ·  1 项", "SSH  ·  1 ITEM"),
                language.pick("AI  ·  1 项", "AI  ·  1 ITEM"),
                language.pick("数据与历史  ·  4 项", "DATA & HISTORY  ·  4 ITEMS"),
            ];
            for (group, label) in geometry.backup_groups.iter().zip(group_labels) {
                if visible(group.1, group.3) {
                    r.draw_chrome_text(
                        size,
                        group.0 + s(16.0),
                        group.1 + (group.3 - cell_h) / 2.0,
                        sk.ink_faint,
                        label,
                        gc,
                    );
                }
            }

            let items = [
                (
                    ("外观", "Appearance"),
                    ("主题、字号、透明度与背景", "Theme, font size, opacity, and background"),
                    "12 KB",
                ),
                (
                    ("配置文件", "Profiles"),
                    ("Shell 配置与启动参数", "Shell profiles and launch arguments"),
                    "4 KB",
                ),
                (
                    ("SSH 地址簿", "SSH address book"),
                    (
                        "主机、端口、代理与认证方式，不含凭据",
                        "Hosts, ports, proxies, and auth methods; no credentials",
                    ),
                    "2 KB",
                ),
                (
                    ("同步配置", "Sync configuration"),
                    ("WebDAV 地址与同步策略，不含密码", "WebDAV endpoint and policy; no passwords"),
                    "1 KB",
                ),
                (
                    ("AI 助手", "AI assistant"),
                    (
                        "模型、MCP 与 Skills 开关，不含 API Key",
                        "Models, MCP, and Skills switches; no API keys",
                    ),
                    "6 KB",
                ),
                (
                    ("会话布局", "Session layout"),
                    ("标签页、分屏与工作区", "Tabs, splits, and workspaces"),
                    "3 KB",
                ),
                (
                    ("目录历史", "Directory history"),
                    ("最近目录，用于启动器推荐", "Recent folders used by launcher suggestions"),
                    "18 KB",
                ),
                (
                    ("命令历史", "Command history"),
                    ("各会话保存的本地命令记录", "Locally stored command history by session"),
                    "64 KB",
                ),
                (
                    ("导入字体", "Imported fonts"),
                    ("Nebula 管理的私有字体文件", "Private font files managed by Nebula"),
                    "2.4 MB",
                ),
            ];
            for (index, row) in geometry.backup_rows.iter().enumerate() {
                if visible(row.1, row.3) {
                    let (label, description, meta) = items[index];
                    let text_x = row.0 + s(44.0);
                    let meta_cols = meta.chars().count();
                    let meta_x = row.0 + row.2 - s(16.0) - meta_cols as f32 * cell_w;
                    let max_desc_cols =
                        (((meta_x - s(12.0) - text_x) / cell_w).floor() as usize).max(1);
                    let description = truncate_tab_label(
                        language.pick(description.0, description.1),
                        max_desc_cols,
                    );
                    r.draw_chrome_text(
                        size,
                        text_x,
                        row.1 + s(7.0),
                        if backup_item_selected(view.backup_selection, index) {
                            sk.ink
                        } else {
                            sk.ink_dim
                        },
                        language.pick(label.0, label.1),
                        gc,
                    );
                    r.draw_chrome_text(
                        size,
                        text_x,
                        row.1 + s(7.0) + cell_h,
                        sk.ink_faint,
                        &description,
                        gc,
                    );
                    r.draw_chrome_text(
                        size,
                        meta_x,
                        row.1 + (row.3 - cell_h) / 2.0,
                        sk.ink_faint,
                        meta,
                        gc,
                    );
                }
            }
            if let Some((status, error)) = &view.backup_status {
                let last = geometry.backup_rows[8];
                let status_y = last.1 + last.3 + s(8.0);
                if visible(status_y, cell_h) {
                    r.draw_chrome_text(
                        size,
                        geometry.backup_actions.0,
                        status_y,
                        if *error {
                            Rgb::new(sk.danger.r, sk.danger.g, sk.danger.b)
                        } else {
                            sk.ink_dim
                        },
                        status,
                        gc,
                    );
                }
            }
        },
    }
    icon_draws
}

#[cfg(test)]
mod tests {
    use super::{
        CELL_WIDTH_MODE_OPTIONS, CellWidthMode, KeymapPaneState, ManualProxyProtocol,
        NEW_TAB_POSITION_OPTIONS, NebulaSettingsSection, NewTabPosition, ProxyChoice,
        ProxyPaneState, SHOW_BACKUP_SETTINGS, SHOW_WEBDAV_SYNC_SETTINGS, STANDARD_ROW_ACTION_W,
        SettingsHit, TabRevealMotion, UiLanguage, advanced_content_end, cell_width_mode_label,
        font_popup_row_count, font_popup_slot, manual_proxy_parts, manual_proxy_value,
        new_tab_position_label, opacity_from_pointer, proxy_section_title_y, row_action_rect,
        settings_geometry, settings_hit, ssh_proxy_manual_controls,
    };
    use crate::display::SizeInfo;
    use crate::display::ui::tokens::Density;
    use crate::display::ui::widgets;

    #[test]
    fn the_font_popup_reserves_its_first_row_for_the_search_field() {
        // 搜索框占掉第 0 行，选项整体下移一行。用「多算一行」而不是另开一套
        // 几何：弹层的定位、上下翻转与裁剪都还归 combobox_popup_rect 管。
        assert_eq!(font_popup_row_count(0), 1, "一个字体都没有时也要有搜索框");
        assert_eq!(font_popup_row_count(7), 8);

        assert_eq!(font_popup_slot(0), None, "第 0 行是搜索框，不是选项");
        assert_eq!(font_popup_slot(1), Some(0));
        assert_eq!(font_popup_slot(4), Some(3));
    }

    #[test]
    fn slider_pointer_maps_to_clamped_fraction() {
        let slider = (100.0, 20.0, 200.0, 36.0);
        assert_eq!(opacity_from_pointer(50.0, slider), 0.0);
        assert_eq!(opacity_from_pointer(100.0, slider), 0.0);
        assert_eq!(opacity_from_pointer(200.0, slider), 0.5);
        assert_eq!(opacity_from_pointer(300.0, slider), 1.0);
        assert_eq!(opacity_from_pointer(350.0, slider), 1.0);
    }

    #[test]
    fn hidden_webdav_group_does_not_extend_advanced_content() {
        assert!(!SHOW_WEBDAV_SYNC_SETTINGS);
        // SSH 已迁到独立页面；隐藏同步组不能继续把 Advanced 撑高。
        assert_eq!(advanced_content_end(146.0, 308.0, 44.0), 234.0);
    }

    #[test]
    fn backup_settings_entry_stays_gated_until_restore_workflow_is_complete() {
        assert!(!SHOW_BACKUP_SETTINGS);
    }

    #[test]
    fn tab_reveal_motion_defaults_compatibly_and_round_trips() {
        assert_eq!(TabRevealMotion::default(), TabRevealMotion::Slide);
        assert_eq!(TabRevealMotion::parse("slide"), Some(TabRevealMotion::Slide));
        assert_eq!(TabRevealMotion::parse("INSTANT"), Some(TabRevealMotion::Instant));
        assert_eq!(TabRevealMotion::parse("unknown").unwrap_or_default(), TabRevealMotion::Slide);
        for value in [TabRevealMotion::Slide, TabRevealMotion::Instant] {
            assert_eq!(TabRevealMotion::parse(value.settings_value()), Some(value));
        }
    }

    #[test]
    fn settings_actions_keep_their_hit_area_on_the_button() {
        let row = (100.0, 200.0, 500.0, 44.0);
        let button = row_action_rect(row, 1.0, STANDARD_ROW_ACTION_W);
        assert!(button.0 > row.0);
        assert!(button.0 + button.2 <= row.0 + row.2);
        assert!(!super::contains_rect(button, row.0 + 8.0, row.1 + row.3 * 0.5));
        assert!(
            super::contains_rect(button, button.0 + button.2 * 0.5, button.1 + button.3 * 0.5,)
        );

        let toggle = widgets::toggle_rect(row, 1.0);
        assert!(!super::contains_rect(toggle, row.0 + 8.0, row.1 + row.3 * 0.5));
        assert!(
            super::contains_rect(toggle, toggle.0 + toggle.2 * 0.5, toggle.1 + toggle.3 * 0.5,)
        );
    }

    #[test]
    fn profile_import_and_open_actions_share_one_width() {
        let import = row_action_rect((100.0, 200.0, 500.0, 44.0), 1.5, STANDARD_ROW_ACTION_W);
        let open = row_action_rect((100.0, 400.0, 500.0, 44.0), 1.5, STANDARD_ROW_ACTION_W);
        assert_eq!(import.2, open.2);
        assert_eq!(import.2, STANDARD_ROW_ACTION_W * 1.5);
    }

    #[test]
    fn settings_geometry_releases_navigation_width_before_stacking_rows() {
        let size = proxy_test_size();
        let geometry = |width| {
            settings_geometry(
                &size,
                1.0,
                (0.0, 0.0, width, 900.0),
                0.0,
                0,
                0,
                Density::Standard,
                ProxyPaneState::default(),
                KeymapPaneState::default(),
            )
        };

        let wide = geometry(1200.0);
        assert!(!wide.compact_nav);
        assert!(!wide.stacked_rows);
        assert_eq!(wide.sidebar.2, 196.0);

        let medium = geometry(800.0);
        assert!(medium.compact_nav);
        assert!(!medium.stacked_rows);
        assert_eq!(medium.sidebar.2, 64.0);
        assert!(medium.content.0 + medium.content.2 <= medium.popup.0 + medium.popup.2);

        let narrow = geometry(600.0);
        assert!(narrow.compact_nav);
        assert!(narrow.stacked_rows);
        assert!(narrow.shell.3 >= 72.0);
        let shell_control = widgets::combobox_rect(narrow.shell, 1.0);
        assert!(shell_control.0 >= narrow.shell.0);
        assert!(shell_control.0 + shell_control.2 <= narrow.shell.0 + narrow.shell.2);
        assert!(shell_control.1 > narrow.shell.1 + 24.0);
    }

    fn proxy_test_size() -> SizeInfo {
        SizeInfo::new(1400.0, 1000.0, 10.0, 20.0, 0.0, 0.0, false)
    }

    #[test]
    fn provider_geometry_tracks_every_persisted_custom_entry() {
        let size = proxy_test_size();
        let area = (0.0, 0.0, 1200.0, 1800.0);
        let provider_count = 14;
        let mut geometry = settings_geometry(
            &size,
            1.0,
            area,
            0.0,
            0,
            0,
            Density::Standard,
            ProxyPaneState::default(),
            KeymapPaneState::default(),
        );
        let old_field_y = geometry.provider_fields[0].1;
        super::fit_provider_rows(&mut geometry, provider_count);
        assert_eq!(geometry.provider_row_count, provider_count);
        assert_eq!(geometry.provider_fields[0].1, old_field_y + geometry.provider_row_h * 8.0);

        let last_row = (
            geometry.provider_row0.0,
            geometry.provider_row0.1 + 13.0 * geometry.provider_row_h,
            geometry.provider_row0.2,
            geometry.provider_row_h,
        );
        let hit = settings_hit(
            &size,
            1.0,
            area,
            last_row.0 + 20.0,
            last_row.1 + last_row.3 * 0.5,
            true,
            NebulaSettingsSection::Providers,
            0.0,
            None,
            0,
            0,
            0,
            0,
            0,
            Density::Standard,
            ProxyPaneState::default(),
            KeymapPaneState::default(),
            provider_count,
        );
        assert_eq!(hit, SettingsHit::ProviderRow(13));
    }

    #[test]
    fn custom_proxy_geometry_expands_selected_component_and_scroll_height() {
        let size = proxy_test_size();
        let area = (0.0, 0.0, 1200.0, 900.0);
        let manual = ProxyPaneState {
            mode: crate::ssh_proxy::ProxyMode::Custom,
            choice: ProxyChoice::Manual,
            found_count: 3,
            override_count: 2,
            ..Default::default()
        };
        let detected = ProxyPaneState { choice: ProxyChoice::Detected(0), ..manual };
        let manual_geometry = settings_geometry(
            &size,
            1.0,
            area,
            0.0,
            0,
            4,
            Density::Standard,
            manual,
            KeymapPaneState::default(),
        );
        let detected_geometry = settings_geometry(
            &size,
            1.0,
            area,
            0.0,
            0,
            4,
            Density::Standard,
            detected,
            KeymapPaneState::default(),
        );
        assert!(manual_geometry.ssh_proxy_list.3 > detected_geometry.ssh_proxy_list.3);
        assert!(manual_geometry.proxy_h > detected_geometry.proxy_h);
        assert!(manual_geometry.ssh_proxy_expand.1 > manual_geometry.ssh_proxy_other_rows[0].1);
        assert!(manual_geometry.ssh_proxy_expand.1 < manual_geometry.ssh_proxy_other_rows[1].1);
    }

    #[test]
    fn proxy_section_title_stays_above_the_test_banner() {
        let size = proxy_test_size();
        let geometry = settings_geometry(
            &size,
            1.0,
            (0.0, 0.0, 1200.0, 900.0),
            0.0,
            0,
            0,
            Density::Standard,
            ProxyPaneState::default(),
            KeymapPaneState::default(),
        );
        let title_y = proxy_section_title_y(geometry.ssh_proxy_test.1, 1.0);
        assert!(title_y >= geometry.content_top);
        assert!(title_y + 26.0 <= geometry.ssh_proxy_test.1);
    }

    #[test]
    fn custom_proxy_hit_test_tracks_found_rows_rescan_and_selected_input() {
        let size = proxy_test_size();
        let area = (0.0, 0.0, 1200.0, 900.0);
        let proxy = ProxyPaneState {
            mode: crate::ssh_proxy::ProxyMode::Custom,
            choice: ProxyChoice::Command,
            found_count: 2,
            ..Default::default()
        };
        let geometry = settings_geometry(
            &size,
            1.0,
            area,
            0.0,
            0,
            4,
            Density::Standard,
            proxy,
            KeymapPaneState::default(),
        );
        let hit = |rect: (f32, f32, f32, f32)| {
            settings_hit(
                &size,
                1.0,
                area,
                rect.0 + rect.2 * 0.5,
                rect.1 + rect.3 * 0.5,
                true,
                NebulaSettingsSection::Proxy,
                0.0,
                None,
                0,
                0,
                0,
                0,
                4,
                Density::Standard,
                proxy,
                KeymapPaneState::default(),
                6,
            )
        };
        assert_eq!(hit(geometry.ssh_proxy_scan_button), SettingsHit::SshProxyRescan);
        assert_eq!(hit(geometry.ssh_proxy_found_row0), SettingsHit::SshProxyLinkPick(0));
        assert_eq!(hit(geometry.ssh_proxy_other_rows[2]), SettingsHit::SshProxyLinkPick(4));
        assert_eq!(hit(geometry.ssh_proxy_expand), SettingsHit::SshProxyInput(2));
    }

    #[test]
    fn manual_proxy_protocol_and_address_round_trip_without_duplicate_prefixes() {
        assert_eq!(
            manual_proxy_parts("SOCKS5://127.0.0.1:1080"),
            (ManualProxyProtocol::Socks5, "127.0.0.1:1080")
        );
        assert_eq!(
            manual_proxy_parts("http://proxy.lan:8080"),
            (ManualProxyProtocol::Http, "proxy.lan:8080")
        );
        assert_eq!(
            manual_proxy_parts("127.0.0.1:7890"),
            (ManualProxyProtocol::Socks5, "127.0.0.1:7890")
        );
        assert_eq!(
            manual_proxy_value(ManualProxyProtocol::Socks5, "127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(manual_proxy_value(ManualProxyProtocol::Http, ""), "");
    }

    #[test]
    fn manual_proxy_expand_has_separate_protocol_and_address_hit_targets() {
        let size = proxy_test_size();
        let area = (0.0, 0.0, 1200.0, 900.0);
        let proxy = ProxyPaneState {
            mode: crate::ssh_proxy::ProxyMode::Custom,
            choice: ProxyChoice::Manual,
            found_count: 1,
            ..Default::default()
        };
        let geometry = settings_geometry(
            &size,
            1.0,
            area,
            0.0,
            0,
            4,
            Density::Standard,
            proxy,
            KeymapPaneState::default(),
        );
        let hit = |rect: (f32, f32, f32, f32)| {
            settings_hit(
                &size,
                1.0,
                area,
                rect.0 + rect.2 * 0.5,
                rect.1 + rect.3 * 0.5,
                true,
                NebulaSettingsSection::Proxy,
                0.0,
                None,
                0,
                0,
                0,
                0,
                4,
                Density::Standard,
                proxy,
                KeymapPaneState::default(),
                6,
            )
        };
        let (protocol, address) = ssh_proxy_manual_controls(geometry.ssh_proxy_expand, 1.0);
        assert_eq!(hit(protocol), SettingsHit::SshProxyProtocolDropdown);
        assert_eq!(hit(address), SettingsHit::SshProxyInput(0));
        assert!(protocol.0 + protocol.2 < address.0, "两个控件之间必须保留间距");
    }

    #[test]
    fn new_tab_position_defaults_compatibly_and_round_trips() {
        assert_eq!(NewTabPosition::default(), NewTabPosition::AfterCurrent);
        assert_eq!(NewTabPosition::parse("after_current"), Some(NewTabPosition::AfterCurrent));
        assert_eq!(NewTabPosition::parse("END"), Some(NewTabPosition::End));
        assert_eq!(
            NewTabPosition::parse("unknown").unwrap_or_default(),
            NewTabPosition::AfterCurrent
        );
        for value in [NewTabPosition::AfterCurrent, NewTabPosition::End] {
            assert_eq!(NewTabPosition::parse(value.settings_value()), Some(value));
        }
    }

    #[test]
    fn cell_width_mode_defaults_compatibly_and_round_trips() {
        assert_eq!(CellWidthMode::default(), CellWidthMode::Compact);
        assert_eq!(CellWidthMode::parse("compact"), Some(CellWidthMode::Compact));
        assert_eq!(CellWidthMode::parse("RELAXED"), Some(CellWidthMode::Relaxed));
        assert_eq!(CellWidthMode::parse("unknown").unwrap_or_default(), CellWidthMode::Compact);
        for value in [CellWidthMode::Compact, CellWidthMode::Relaxed] {
            assert_eq!(CellWidthMode::parse(value.settings_value()), Some(value));
        }
    }

    #[test]
    fn new_tab_position_dropdown_offers_both_choices_with_the_compatible_one_first() {
        // 列表顺序即下拉顺序，也是持久化索引的来源。把兼容默认放在首位是
        // 合同的一部分——重排这个数组会让升级用户的选择悄悄改变。
        assert_eq!(NEW_TAB_POSITION_OPTIONS.len(), 2);
        assert_eq!(NEW_TAB_POSITION_OPTIONS[0], NewTabPosition::default());
        assert_eq!(NEW_TAB_POSITION_OPTIONS[1], NewTabPosition::End);
        // 下拉与标签共用同一张表：每个选项都要能渲染出各自的文案。
        for option in NEW_TAB_POSITION_OPTIONS {
            assert!(!new_tab_position_label(option, UiLanguage::ZhCn).is_empty());
            assert!(!new_tab_position_label(option, UiLanguage::EnUs).is_empty());
        }
    }

    #[test]
    fn cell_width_mode_dropdown_offers_both_choices_with_the_compatible_one_first() {
        // 列表顺序即下拉顺序，也是持久化索引的来源；兼容默认必须排第一。
        assert_eq!(CELL_WIDTH_MODE_OPTIONS.len(), 2);
        assert_eq!(CELL_WIDTH_MODE_OPTIONS[0], CellWidthMode::default());
        assert_eq!(CELL_WIDTH_MODE_OPTIONS[1], CellWidthMode::Relaxed);
        for option in CELL_WIDTH_MODE_OPTIONS {
            assert!(!cell_width_mode_label(option, UiLanguage::ZhCn).is_empty());
            assert!(!cell_width_mode_label(option, UiLanguage::EnUs).is_empty());
        }
    }
}
