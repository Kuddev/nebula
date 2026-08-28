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
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, Image,
    ImageFormat, InteractiveElement as _, IntoElement, KeyDownEvent, ModifiersChangedEvent,
    MouseButton, MouseMoveEvent, ParentElement as _, Render, RenderImage, Rgba as GpuiRgba,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    anchored, deferred, div, img, px,
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

#[path = "background_color.rs"]
mod background_color;
mod design;
mod font_picker;

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
const SECTIONS: [&str; 10] =
    ["应用", "外观", "配置文件", "AI 供应商", "SSH", "网络", "交互", "按键映射", "高级", "备份"];

const HIDDEN_NAV_SECTIONS: &[usize] = &[3, 9];

/// 保留原来的分组展开顺序，组名不再渲染；数组里仍保存稳定的 [`SECTIONS`]
/// 下标，不复制设置状态或路由。
const NAV_GROUPS: [(&str, &[usize]); 3] =
    [("工作区", &[0, 1, 2, 6, 7]), ("连接与智能", &[3, 4, 5]), ("系统", &[8, 9])];

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
const SETTINGS_NAV_WIDTH: f32 = 196.0;
const SETTINGS_HEADER_HEIGHT: f32 = 48.0;

const SETTINGS_GROUP_GAP: f32 = 32.0;
const SETTINGS_GROUP_TITLE_HEIGHT: f32 = 26.0;
const SETTINGS_GROUP_TITLE_GAP: f32 = 16.0;
const SETTINGS_ROW_HEIGHT: f32 = 44.0;
const SETTINGS_ROW_GAP: f32 = 8.0;
/// 标准设置选择器的实际宽度。字体输入与 Select 共用，避免同列控件漂移。
const SETTINGS_SELECT_WIDTH: f32 = 220.0;

const THEME_NAMES: [ThemeName; 9] = [
    ThemeName::Nebula,
    ThemeName::SilverLight,
    ThemeName::SteelDark,
    ThemeName::LimestoneLight,
    ThemeName::CoalDark,
    ThemeName::LinenLight,
    ThemeName::MossDark,
    ThemeName::Nord,
    ThemeName::Paper,
];

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
        "Reported from Nebula Settings (Nebula {version}).\n\nPlatform: {} {}\nBuild: {build}",
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
    Changed,
    /// 导入 Profile 已落盘；Tab 的 Shell 面板若正打开，需要重建候选快照。
    TerminalProfilesChanged,
    /// 设置页"连接"按钮：宿主开 SSH tab（连接语义在业务层）。
    LaunchSsh(String),
}

pub(super) type SharedSelect = Entity<SelectState<Vec<SharedString>>>;
type SharedShellSelect = Entity<SelectState<Vec<ShellSelectItem>>>;

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

use super::ssh_settings::{SshDeleteUndo, SshEditorState};

impl ShellSelectItem {
    fn new(id: String, name: String, scale_factor: f32) -> Self {
        // Select 的闭态和菜单行尺寸不同。分别生成与物理像素一一对应的纹理，
        // 避免把 128px 原图交给 GPUI 在每帧缩小而产生模糊边缘。
        let closed_image = crate::gpui_shell::widgets::shell_brand_image(&id, 20.0, scale_factor);
        let row_image = crate::gpui_shell::widgets::shell_brand_image(&id, 24.0, scale_factor);
        Self { id, name: name.into(), closed_image, row_image }
    }

    /// 置顶的导入行。没有品牌贴图，[`Self::view`] 会给它文件夹图标。
    fn import_action() -> Self {
        Self {
            id: SHELL_IMPORT_ACTION_ID.to_owned(),
            name: "导入终端目录…".into(),
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
fn shell_select_items(current: &str, scale_factor: f32) -> (Vec<ShellSelectItem>, usize) {
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
    items.insert(0, ShellSelectItem::import_action());
    (items, selected + 1)
}

/// 扫描目录并落盘（阻塞 IO，调用方须放后台执行器）。
/// 逻辑与旧壳 `Display::import_terminal_directory` 逐句对齐，只是把 toast
/// 换成 `Result`，由 UI 线程决定怎么呈现。
fn import_terminal_directory_blocking(directory: &std::path::Path) -> Result<usize, String> {
    let found = crate::terminal_profiles::scan_directory(directory)
        .map_err(|error| format!("无法扫描终端目录：{error}"))?;
    if found.is_empty() {
        return Err("目录中未找到受支持的终端程序".to_owned());
    }
    let mut profiles = crate::terminal_profiles::TerminalProfiles::load()
        .map_err(|error| format!("无法读取终端配置：{error}"))?;
    let count = found.len();
    for profile in found {
        profiles.upsert(profile).map_err(|error| format!("无法导入终端：{error}"))?;
    }
    profiles.save().map_err(|error| format!("无法保存终端配置：{error}"))?;
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
    about_logo: Arc<Image>,
    about_update: AboutUpdateState,
    about_update_seq: u64,
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
    provider_status: Option<(String, bool)>,
    provider_test_seq: u64,
    provider_test_running: bool,
    provider_codex_confirm: Option<String>,
    /// SSH 主机列表（共享三键 + merge 权威）；操作后整体重载防漂移。
    /// SSH 区的行为实现拆在 `ssh_settings.rs`（同类型第二个 impl 块）。
    pub(super) ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists,
    /// SSH 编辑器的文本字段常驻，以便所有文本改动都能使进行中的测试失效。
    pub(super) ssh_destination_input: Entity<InputState>,
    pub(super) ssh_port_input: Entity<InputState>,
    pub(super) ssh_label_input: Entity<InputState>,
    pub(super) ssh_password_input: Entity<InputState>,
    /// 身份条头像的图标选择器（旧壳 `SshEditorHit::Avatar` + `icon_popup`）：
    /// 点头像展开，顶部一个正经搜索框，下面是分组过的目录。不做成右上角
    /// 的下拉框——图标属于头像那件事，摆成独立字段就成了可填可不填的杂项。
    pub(super) ssh_icon_picker_open: bool,
    pub(super) ssh_icon_filter_input: Entity<InputState>,
    /// 头像上一帧的窗口坐标，供弹层锚定（同字体目录的做法）。
    pub(super) ssh_icon_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    pub(super) ssh_editor: Option<SshEditorState>,
    pub(super) ssh_editor_seq: u64,
    pub(super) ssh_test_seq: u64,
    pub(super) ssh_status: Option<(String, bool)>,
    pub(super) ssh_show_hidden: bool,
    /// 删除确认（二次点击生效，旧壳确认对话框的轻量对应）。
    pub(super) ssh_delete_confirm: Option<String>,
    /// 未决删除的撤销窗口（8 秒；见 `ssh_settings::SshDeleteUndo`）。
    pub(super) ssh_delete_undo: Option<SshDeleteUndo>,
    pub(super) ssh_undo_seq: u64,
    /// 可直接编辑的字体链及其建议弹层；逗号分隔语义与 Windows Terminal 一致。
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
    backup_status: Option<(String, bool)>,
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
        let mut selects: Vec<(&'static str, SharedSelect, &'static [&'static str])> = Vec::new();
        let mut subscriptions = Vec::new();

        let mut add_select = |key: &'static str,
                              labels: &'static [&'static str],
                              values: &'static [&'static str],
                              current: &str,
                              window: &mut Window,
                              cx: &mut Context<Self>| {
            let ix = values.iter().position(|v| *v == current).unwrap_or(0);
            let select = cx.new(|cx| {
                SelectState::new(
                    labels.iter().map(|l| SharedString::from(*l)).collect::<Vec<_>>(),
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
                      _window: &mut Window,
                      cx: &mut Context<Self>| {
                    if let SelectEvent::Confirm(Some(_)) = event {
                        let row = entity.read(cx).selected_index(cx).map(|path| path.row);
                        if let Some(value) = row.and_then(|row| values.get(row)) {
                            this.persist(&[(key, (*value).to_string())], cx);
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
            &["跟随系统", "简体中文", "English"],
            &["system", "zh-CN", "en-US"],
            runtime.language.settings_value(),
            window,
            cx,
        );
        add_select("theme", &THEME_VALUES, &THEME_VALUES, runtime.theme.prompt_name(), window, cx);
        // 选项顺序与文案照抄旧壳 `CURSOR_SHAPE_OPTIONS` / `cursor_shape_label`。
        add_select(
            "cursor_shape",
            &["条形（│）", "下划线（_）", "实心框（█）", "空心框（□）"],
            &["beam", "underline", "block", "hollow"],
            cursor_current,
            window,
            cx,
        );
        add_select(
            "tabs_position",
            &["左侧边栏", "顶部"],
            &["sidebar", "top"],
            runtime.tabs_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "tab_reveal",
            &["滑动", "立即"],
            &["slide", "instant"],
            runtime.tab_reveal.settings_value(),
            window,
            cx,
        );
        add_select(
            "density",
            &["标准", "紧凑"],
            &["standard", "compact"],
            runtime.density.settings_value(),
            window,
            cx,
        );
        add_select(
            "new_tab_position",
            &["当前标签之后", "列表末尾"],
            &["after_current", "end"],
            runtime.new_tab_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "windowing_behavior",
            &["创建新窗口", "附加到最近使用的窗口", "附加到此桌面最近使用的窗口"],
            &["use_new", "use_any_existing", "use_existing"],
            runtime.windowing_behavior.settings_value(),
            window,
            cx,
        );
        add_select(
            "vcs_display",
            &["自动检测", "仅 Git", "仅 SVN"],
            &["auto", "git", "svn"],
            runtime.vcs_display.settings_value(),
            window,
            cx,
        );
        add_select(
            "cell_width_mode",
            &["紧凑", "宽松"],
            &["compact", "relaxed"],
            runtime.cell_width_mode.settings_value(),
            window,
            cx,
        );
        // 文案照抄旧壳 `accept_label` / `completion_style_label`。
        add_select(
            "bell",
            &["关", "闪烁", "声音", "闪烁 + 声音"],
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
            &["无", "Mica（低开销）", "Mica Alt（低开销）", "Aero（玻璃）", "Acrylic（高开销）"],
            &["none", "mica", "mica-alt", "aero", "acrylic"],
            runtime.blur.settings_value(),
            window,
            cx,
        );
        add_select(
            "accept",
            &["右方向键", "Tab", "Tab 或右方向键"],
            &["right", "tab", "both"],
            runtime.accept.settings_value(),
            window,
            cx,
        );
        add_select(
            "completion_style",
            &["行内灰字", "弹窗列表"],
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
            &["拉伸", "适应", "填充", "原始尺寸"],
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
            &["左上", "顶部", "右上", "左侧", "居中", "右侧", "左下", "底部", "右下"],
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
            &["不使用代理", "跟随系统", "自定义代理"],
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
        let (shell_items, shell_index) = shell_select_items(&shell_current, shell_icon_scale);
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
        let provider_placeholders =
            ["供应商名称", "备注（可包含空格）", "官方网站", "API 请求地址", "默认模型"];
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

        // 占位文案与旧壳 `ssh_editor_render` 一字不差（对齐合同）。
        let ssh_destination_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("user@example.com"));
        // 端口键入即过滤：至多 5 位数字（旧壳同规则；范围校验在保存/测试时做）。
        let ssh_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("22")
                .pattern(regex::Regex::new(r"^\d{0,5}$").expect("static regex"))
        });
        let ssh_label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("给这台机器起个名字"));
        let ssh_password_input =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("留空则连接时询问"));
        let ssh_icon_filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索图标…"));
        for input in [
            ssh_destination_input.clone(),
            ssh_port_input.clone(),
            ssh_label_input.clone(),
            ssh_password_input.clone(),
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

        Self {
            focus_handle: cx.focus_handle(),
            runtime,
            active_section: 0,
            about_logo: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../../extra/logo/nebula.png").to_vec(),
            )),
            about_update: AboutUpdateState::Idle,
            about_update_seq: 0,
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
            ssh_destination_input,
            ssh_port_input,
            ssh_label_input,
            ssh_password_input,
            ssh_icon_picker_open: false,
            ssh_icon_filter_input,
            ssh_icon_trigger_bounds: None,
            ssh_editor: None,
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
                InputState::new(window, cx).masked(true).placeholder("备份密码（至少 8 位）")
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
                InputState::new(window, cx).masked(true).placeholder("凭据只写入系统凭据管理器")
            }),
            keymap_search_input: {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索动作或按键…"));
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
        if let Err(err) = persist_keys(updates) {
            super::try_write_stderr(format_args!(
                "[nebula:gpui] failed to persist settings: {err}"
            ));
        }
        self.runtime = RuntimeSettings::load();
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        cx.set_global(settings);
        if updates.iter().any(|(key, _)| matches!(*key, "ssh_proxy_mode" | "ssh_proxy_url")) {
            self.invalidate_proxy_test();
        }
        cx.emit(SettingsPaneEvent::Changed);
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
    fn request_cover_chrome(&mut self, enable: bool, window: &mut Window, cx: &mut Context<Self>) {        if !enable {
            self.persist(&[("background_image_cover_chrome", "0".to_owned())], cx);
            return;
        }
        if self.runtime.background_image_cover_chrome {
            return;
        }
        let pane = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let pane = pane.clone();
            confirm_dialog(
                dialog,
                window,
                "让背景图覆盖窗口控件区域？",
                SharedString::from(
                    "背景图会延伸到标题栏、窗口按钮、Tab 与 SSH 侧栏下方，低对比度图片可能影响操作可见性；界面仍会保留最低不透明度保护。",
                ),
                "开启",
                "取消",
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
        self.persist(
            &[
                ("theme", "Nebula".to_owned()),
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

        #[cfg(windows)]
        let picked = pick_folder_with_wsl_places(window, "选择终端安装目录");
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择终端安装目录".into()),
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
        outcome: Result<usize, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(count) => {
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::display::ToastKind::Success,
                    format!("已导入 {count} 个终端，立即可用"),
                );
                self.refresh_shell_items(window, cx);
                // 新 profile 要立即进入 Tab 的 Shell 面板；它不是普通运行时
                // 设置，避免因此重建所有终端字体与主题。
                cx.emit(SettingsPaneEvent::TerminalProfilesChanged);
                cx.notify();
            },
            Err(message) => {
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
        let (items, selected) = shell_select_items(&current, window.scale_factor().max(0.5));
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
    fn sync_select(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.row(
            "默认 Shell",
            "新标签用哪个程序开。已经开着的标签不受影响——它们跟的是各自创建时的选择。",
            div()
                .w(px(SETTINGS_SELECT_WIDTH))
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().link)
                .child(Select::new(&self.shell_select)),
            cx,
        )
    }

    fn appearance_preview(&self, cx: &Context<Self>) -> gpui::Div {
        let theme = chrome_theme(crate::gpui_shell::theme::effective_theme_name(cx));
        let palette = theme.palette();
        let ink = theme.card_ink().fg;
        let background = if self.runtime.follow_system_theme {
            [palette.term_bg.r, palette.term_bg.g, palette.term_bg.b]
        } else {
            self.runtime.background.unwrap_or([
                palette.term_bg.r,
                palette.term_bg.g,
                palette.term_bg.b,
            ])
        };
        let family = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| crate::font_install::REQUIRED_FONT_FAMILY.to_owned());
        let size = self.terminal_font_size_px(cx).clamp(11.0, 20.0);
        let foreground = rgb_hsla(ink.r, ink.g, ink.b);
        let accent = theme.accent();

        v_flex()
            .w_full()
            .h(px(150.0))
            .p_4()
            .gap_1()
            .rounded_lg()
            .bg(rgb_hsla(background[0], background[1], background[2]))
            .font(crate::font_install::gpui_font_with_fallbacks(&family))
            .text_size(px(size))
            .text_color(foreground)
            .child("user@nebula ~ $ nebula --version")
            .child(format!(
                "Nebula Terminal · {} · {:.0}px",
                self.runtime
                    .font_family
                    .as_deref()
                    .unwrap_or(crate::font_install::REQUIRED_FONT_FAMILY),
                size
            ))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().text_color(rgb_hsla(accent.r, accent.g, accent.b)).child("❯"))
                    .child(div().w(px(8.0)).h(px(size)).bg(foreground)),
            )
    }

    fn theme_previews(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex().w_full().flex_wrap().gap(px(20.0)).children(THEME_NAMES.into_iter().map(|name| {
            let theme = chrome_theme(name);
            let palette = theme.palette();
            let accent = theme.accent();
            let selected = crate::gpui_shell::theme::effective_theme_name(cx) == name;
            v_flex()
                .id(SharedString::from(format!("theme-preview-{}", name.prompt_name())))
                .w(px(170.0))
                .h(px(92.0))
                .gap_2()
                .cursor_pointer()
                .child(
                    v_flex()
                        .h(px(64.0))
                        .flex_shrink_0()
                        .p_2()
                        .gap_1()
                        .rounded_lg()
                        .bg(rgb_hsla(palette.shell_bg.r, palette.shell_bg.g, palette.shell_bg.b))
                        .when(selected, |card| {
                            card.border_1().border_color(rgb_hsla(accent.r, accent.g, accent.b))
                        })
                        .when(!selected, |card| card.hover(|style| style.shadow_md()))
                        .child(
                            v_flex()
                                .flex_1()
                                .p_2()
                                .gap_1()
                                .rounded_md()
                                .bg(rgb_hsla(
                                    palette.term_bg.r,
                                    palette.term_bg.g,
                                    palette.term_bg.b,
                                ))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .w(px(10.0))
                                                .h(px(4.0))
                                                .rounded_full()
                                                .bg(rgb_hsla(accent.r, accent.g, accent.b)),
                                        )
                                        .child(div().w(px(52.0)).h(px(4.0)).rounded_full().bg(
                                            rgb_hsla(
                                                theme.card_ink().fg.r,
                                                theme.card_ink().fg.g,
                                                theme.card_ink().fg.b,
                                            ),
                                        )),
                                )
                                .child(div().w(px(82.0)).h(px(4.0)).rounded_full().bg(rgb_hsla(
                                    palette.edge_l.r,
                                    palette.edge_l.g,
                                    palette.edge_l.b,
                                )))
                                .child(div().w(px(58.0)).h(px(4.0)).rounded_full().bg(rgb_hsla(
                                    palette.edge_r.r,
                                    palette.edge_r.g,
                                    palette.edge_r.b,
                                ))),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_center()
                        .text_color(if selected {
                            rgb_hsla(accent.r, accent.g, accent.b)
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(theme.short_label()),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.persist(&crate::gpui_shell::theme::theme_card_persist_updates(name), cx);
                    this.sync_background_color_picker(window, cx);
                }))
        }))
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
        let current = self
            .runtime
            .startup_directory
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty());
        let has_dir = current.is_some();
        let label: SharedString =
            current.map(str::to_owned).unwrap_or_else(|| "继承当前目录".to_owned()).into();
        let color = if has_dir { cx.theme().link } else { cx.theme().muted_foreground };
        self.row(
            "启动目录",
            "新标签落在哪个目录。不设则继承 Nebula 自己的工作目录，从资源管理器右键进来时就是那个文件夹。",
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
                    row.child(NebulaButton::new("startup-directory-clear").label("清除").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.clear_startup_directory(cx);
                        }),
                    ))
                }),
            cx,
        )
    }

    fn pick_startup_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        let picked = pick_folder_with_wsl_places(window, "选择终端启动目录");
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择终端启动目录".into()),
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
                        .label("保存")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let value = state.read(cx).value().to_string();
                            this.persist(&[(key, value)], cx);
                        })),
                ),
            cx,
        )
    }

    fn stepper_row(
        &self,
        label: &'static str,
        desc: &'static str,
        id: &'static str,
        display: SharedString,
        on_minus: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        on_plus: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.row(
            label,
            desc,
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    NebulaButton::new(SharedString::from(format!("minus-{id}")))
                        .label("−")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            on_minus(this, cx);
                        })),
                )
                .child(div().min_w(px(64.0)).child(display))
                .child(
                    NebulaButton::new(SharedString::from(format!("plus-{id}")))
                        .label("+")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            on_plus(this, cx);
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
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择终端背景图片".into()),
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
            "背景图片",
            "铺在终端文字后面。图片本身不参与配色，字色仍由主题决定；看不清就调下面的不透明度。",
            has_image,
            |this, _, cx| {
                this.persist(&[("background_image", String::new())], cx);
            },
            h_flex()
                .items_center()
                .gap_2()
                .child(NebulaButton::new("background-image-choose").label("选择图片").on_click(
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

    fn active_provider_index(&self) -> Option<usize> {
        self.provider_store
            .providers
            .iter()
            .position(|provider| provider.id == self.provider_store.active_id)
            .or_else(|| (!self.provider_store.providers.is_empty()).then_some(0))
    }

    fn sync_provider_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        let draft =
            crate::ai_providers::ProviderMetadataDraft::from(&self.provider_store.providers[index]);
        let values = [draft.name, draft.note, draft.website_url, draft.base_url, draft.model];
        for (input, value) in self.provider_inputs.iter().zip(values) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    fn select_provider(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.provider_store.providers.iter().any(|provider| provider.id == id) {
            self.provider_store.active_id = id;
            let _ = crate::ai_providers::save(&self.provider_store);
            self.provider_codex_confirm = None;
            self.provider_status = None;
            self.sync_provider_inputs(window, cx);
            cx.notify();
        }
    }

    fn save_provider_metadata(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.active_provider_index() else { return false };
        let values: Vec<String> =
            self.provider_inputs.iter().map(|input| input.read(cx).value().to_string()).collect();
        let draft = crate::ai_providers::ProviderMetadataDraft {
            name: values[0].clone(),
            note: values[1].clone(),
            website_url: values[2].clone(),
            base_url: values[3].clone(),
            model: values[4].clone(),
        };
        crate::ai_providers::apply_metadata_draft(&mut self.provider_store.providers[index], draft);
        match crate::ai_providers::save(&self.provider_store) {
            Ok(()) => {
                self.provider_status = Some(("供应商配置已保存".to_owned(), false));
                true
            },
            Err(error) => {
                self.provider_status = Some((error.to_string(), true));
                false
            },
        }
    }

    fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_provider_metadata(cx);
        let id = crate::ai_providers::next_custom_id(&self.provider_store);
        self.provider_store.providers.push(crate::ai_providers::AiProvider::preset(
            crate::ai_providers::ProviderKind::Custom,
            &id,
        ));
        self.provider_store.active_id = id;
        self.provider_codex_confirm = None;
        match crate::ai_providers::save(&self.provider_store) {
            Ok(()) => self.provider_status = Some(("已添加自定义供应商".to_owned(), false)),
            Err(error) => self.provider_status = Some((error.to_string(), true)),
        }
        self.sync_provider_inputs(window, cx);
        cx.notify();
    }

    fn delete_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        if self.provider_store.providers.len() <= 1 {
            self.provider_status = Some(("至少保留一个供应商".to_owned(), true));
            cx.notify();
            return;
        }
        let id = self.provider_store.providers[index].id.clone();
        match crate::ai_providers::remove_provider(&mut self.provider_store, &id) {
            Ok(()) => {
                self.provider_status = Some(("供应商及其凭据已删除".to_owned(), false));
                self.provider_codex_confirm = None;
                self.sync_provider_inputs(window, cx);
            },
            Err(error) => self.provider_status = Some((error.to_string(), true)),
        }
        cx.notify();
    }

    fn toggle_provider_flag(&mut self, flag: &'static str, value: bool, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        let provider = &mut self.provider_store.providers[index];
        match flag {
            "enabled" => provider.enabled = value,
            "codex_goals" => provider.codex_goals = value,
            "codex_remote_compaction" => provider.codex_remote_compaction = value,
            _ => return,
        }
        self.provider_codex_confirm = None;
        if let Err(error) = crate::ai_providers::save(&self.provider_store) {
            self.provider_status = Some((error.to_string(), true));
        }
        cx.notify();
    }

    fn prompt_provider_key(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        let provider = &mut self.provider_store.providers[index];
        match crate::ai_providers::prompt_and_store_api_key(provider) {
            Ok(true) => {
                self.provider_status = Some(("API Key 已保存到系统凭据管理器".to_owned(), false));
                if let Err(error) = crate::ai_providers::save(&self.provider_store) {
                    self.provider_status = Some((error.to_string(), true));
                }
            },
            Ok(false) => {},
            Err(error) => self.provider_status = Some((error.to_string(), true)),
        }
        cx.notify();
    }

    fn test_provider(&mut self, cx: &mut Context<Self>) {
        if self.provider_test_running || !self.save_provider_metadata(cx) {
            return;
        }
        let Some(index) = self.active_provider_index() else { return };
        let provider = self.provider_store.providers[index].clone();
        self.provider_test_seq = self.provider_test_seq.wrapping_add(1);
        let sequence = self.provider_test_seq;
        self.provider_test_running = true;
        self.provider_status = Some(("正在测试连接…".to_owned(), false));

        let task = cx
            .background_executor()
            .spawn(async move { crate::ai_providers::test_provider(&provider) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                if sequence != pane.provider_test_seq
                    || result.provider_id != pane.provider_store.active_id
                {
                    return;
                }
                pane.provider_test_running = false;
                pane.provider_status =
                    Some((format!("{} · {} ms", result.message, result.elapsed_ms), !result.ok));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_provider_to_codex(&mut self, cx: &mut Context<Self>) {
        if !self.save_provider_metadata(cx) {
            return;
        }
        let Some(index) = self.active_provider_index() else { return };
        let provider = self.provider_store.providers[index].clone();
        if self.provider_codex_confirm.as_deref() != Some(provider.id.as_str()) {
            self.provider_codex_confirm = Some(provider.id);
            self.provider_status = Some((
                "再次点击确认：API Key 将明文写入 Codex auth.json（原文件会备份）".to_owned(),
                false,
            ));
            cx.notify();
            return;
        }
        self.provider_codex_confirm = None;
        self.provider_status = Some(match crate::codex_config::apply_provider(&provider) {
            Ok(path) => (format!("已应用到 Codex：{}", path.display()), false),
            Err(error) => (error, true),
        });
        cx.notify();
    }

    // ---- 分区内容（归属对照旧壳各 section）----

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

    fn about_action_row(
        id: &'static str,
        icon: IconName,
        title: &'static str,
        subtitle: &'static str,
        url: String,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().list_hover;
        h_flex()
            .id(id)
            .w_full()
            .min_h(px(52.0))
            .px_3()
            .py_2()
            .gap_3()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(Icon::new(icon).small().text_color(muted))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().child(title))
                    .child(div().text_xs().text_color(muted).truncate().child(subtitle)),
            )
            .child(Icon::new(IconName::ExternalLink).xsmall().text_color(muted))
            .into_any_element()
    }

    fn section_home(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let checking = matches!(self.about_update, AboutUpdateState::Checking);
        let (status, status_color): (SharedString, Hsla) = match &self.about_update {
            AboutUpdateState::Idle => ("通过 GitHub Releases 检查新版本。".into(), muted),
            AboutUpdateState::Checking => ("正在检查更新…".into(), muted),
            AboutUpdateState::UpToDate(latest) => {
                (format!("已是最新版本（GitHub v{latest}）").into(), theme.success)
            },
            AboutUpdateState::Available(latest) => {
                (format!("发现新版本 v{latest}").into(), theme.warning)
            },
            AboutUpdateState::Failed(error) => (format!("检查失败：{error}").into(), theme.danger),
        };
        let update_button = NebulaButton::new("about-check-updates")
            .label(if checking { "正在检查…" } else { "检查更新" })
            .outline()
            .disabled(checking)
            .on_click(cx.listener(|this, _, window, cx| this.check_for_updates(window, cx)));
        // 关于页做成一段终端输出，不是因为好看：这几行正是报 issue 时要贴的
        // 东西（版本、平台、构建方式、配置在哪）。做成可读又可整段复制的一
        // 块，比藏在"生成预填 Issue"按钮背后让人无从核对要诚实。
        //
        // 这也是全页 mono 的另一处正当用途——它标记的仍然是机器可读的字面量。
        let mono: SharedString = cx.theme().mono_font_family.clone();
        let base_px = self.font_size_px(cx);
        let ink = theme.foreground;
        let accent = theme.primary;
        let build = if cfg!(debug_assertions) { "debug" } else { "release" };
        let config_path = nebula_settings::settings_path().display().to_string();
        let facts: [(&'static str, String); 3] = [
            ("build", build.to_owned()),
            ("platform", format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)),
            ("config", config_path),
        ];
        // 复制走的文本与屏幕上逐字一致，粘进 issue 不用再改。
        let transcript = format!(
            "Nebula {}
{}",
            env!("CARGO_PKG_VERSION"),
            facts.iter().map(|(key, value)| format!("  {key:<9}{value}")).collect::<Vec<_>>().join(
                "
"
            ),
        );
        let fact_rows = facts.into_iter().map(|(key, value)| {
            h_flex()
                .gap_2()
                .items_start()
                .child(
                    // 固定键列宽，值才对齐成一列——这是等宽字体在这里的全部
                    // 意义，不然用 sans 就够了。
                    div().w(px(76.0)).flex_shrink_0().text_color(muted).child(key),
                )
                .child(div().min_w_0().text_color(ink).child(value))
                .into_any_element()
        });
        let banner = v_flex()
            .id("about-transcript")
            .group("about-transcript")
            .relative()
            .flex_1()
            .min_w(px(300.0))
            .gap(px(3.0))
            .p_4()
            .rounded(px(10.0))
            .border_1()
            .border_color(crate::gpui_shell::theme::settings_hairline(cx))
            .bg(theme.background)
            .font_family(mono)
            .text_size(px(base_px * 0.92))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(accent).child("❯"))
                    .child(div().text_color(muted).child("nebula --version")),
            )
            .child(div().mt_1().text_color(ink).child(format!(
                "Nebula {}",
                env!("CARGO_PKG_VERSION")
            )))
            .children(fact_rows)
            // 复制是动作，按需出现（与设置行的 ↶ 同一条规矩）。
            .child(
                div()
                    .id("about-copy")
                    .absolute()
                    .top(px(8.0))
                    .right(px(8.0))
                    .size(px(24.0))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .invisible()
                    .group_hover("about-transcript", |el| el.visible())
                    .hover(|el| el.bg(theme.list_hover))
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("复制这段信息").build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            transcript.clone(),
                        ));
                    })
                    .child(Icon::new(IconName::Copy).xsmall().text_color(muted)),
            );
        let identity = h_flex()
            .w_full()
            .gap(px(18.0))
            .items_start()
            .child(img(self.about_logo.clone()).size(px(68.0)).rounded(px(12.0)).flex_shrink_0())
            .child(banner);
        let actions = v_flex()
            .w_full()
            .gap(px(2.0))
            .child(Self::about_action_row(
                "about-report-issue",
                IconName::TriangleAlert,
                "反馈问题",
                "生成包含版本、平台与构建方式的预填 GitHub Issue",
                issue_url(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-github",
                IconName::Github,
                "GitHub",
                "源代码",
                REPOSITORY_URL.to_owned(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-releases",
                IconName::BookOpen,
                "更新内容",
                "查看发布说明并下载最新版本",
                crate::update_check::RELEASES_PAGE.to_owned(),
                cx,
            ));

        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(identity)
            // 检查更新紧跟在版本信息下面：它回答的正是那一行提出的问题。
            .child(
                h_flex()
                    .mt_5()
                    .gap_3()
                    .items_center()
                    .child(update_button)
                    .child(div().text_xs().text_color(status_color).child(status)),
            )
            .child(actions)
    }

    fn section_appearance(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        // 旧壳 spinner 只显示整数（`{:.0}`）；步进也按整数走（见下）。
        let font_size: SharedString = format!("{:.0} px", self.terminal_font_size_px(cx)).into();
        let opacity: SharedString = format!("{:.0}%", self.runtime.opacity * 100.0).into();
        let wallpaper_opacity: SharedString =
            format!("{:.0}%", self.runtime.background_image_opacity * 100.0).into();
        let preview = self.group("预览", cx).child(self.appearance_preview(cx));
        let themes = self.group("主题", cx).child(self.theme_previews(cx));
        let theme_mode = self.group("主题模式", cx).child(self.switch_row(
            "follow_system_theme",
            "跟随系统明暗模式",
            "开着时 Windows 切浅色/深色，Nebula 跟着换；上面手选的主题只在系统当前这个模式下生效。",
            self.runtime.follow_system_theme,
            cx,
        ));
        let custom_background = self
            .group("自定义背景", cx)
            .child(self.background_color_row(cx))
            .child(self.background_image_row(cx))
            .child(self.select_row("background_image_fit", "背景图像拉伸模式", "拉伸会把图压变形；适应保持比例、四周留边；填充保持比例但裁掉溢出的部分；原始尺寸按图片自己的像素铺。", cx))
            .child(self.select_row("background_image_alignment", "背景图像对齐", "图片没铺满或被裁掉时，保留哪一侧。壁纸类的图通常选顶部或居中，主体才不会被切走。", cx))
            .child(self.slider_row(
                "背景图像不透明度",
                "压得越低文字越清楚。图片不参与配色，字色始终由主题决定。",
                &self.wallpaper_opacity_slider,
                wallpaper_opacity,
                cx,
            ))
            .child(self.switch_row(
                "background_image_cover_chrome",
                "将背景图扩展到标题栏和侧边栏",
                "开着时整窗一张图，界面和终端连成一片；关掉则图片只铺终端区域，侧栏与标题栏保持纯色、文字更稳。",
                self.runtime.background_image_cover_chrome,
                cx,
            ));
        let cursor = self
            .group("光标", cx)
            .child(self.select_row(
                "cursor_shape",
                "光标形状",
                "条形贴近编辑器的手感，实心框在满屏输出里最容易一眼找到。",
                cx,
            ))
            .child(self.switch_row(
                "cursor_blink",
                "光标闪烁",
                "关掉后光标常亮。长时间盯屏时不闪更省心，代价是光标在密集输出里没那么显眼。",
                self.runtime.cursor_blink.unwrap_or(DEFAULT_CURSOR_BLINK),
                cx,
            ));
        let interface = self
            .group("界面", cx)
            .child(self.select_row(
                "language",
                "语言",
                "只改 Nebula 自己的界面。终端里程序输出什么语言由它们自己的环境变量决定，不受这里影响。",
                cx,
            ))
            .child(self.select_row("density", "界面外观", "紧凑会收窄标签行高与设置页行距这些界面留白。终端内容的行距不归它管，那是字体的事。", cx))
            .child(self.slider_row(
                "终端正文不透明度",
                "1 = 完全不透明。调低会透出后方窗口，配合下面的窗口模糊才不至于让文字压在杂乱内容上。",
                &self.opacity_slider,
                opacity,
                cx,
            ))
            .child(self.select_row("blur", "背景模糊", "五者是五套成本模型，不是越靠后越好：Mica 只取系统壁纸的色调、最省；Aero 与 Acrylic 每帧实时模糊窗口后方的真实内容，Acrylic 还多一层着色与噪点。", cx));
        let terminal = self
            .group("终端外观", cx)
            .child(self.stepper_row(
                "终端字号（Ctrl+滚轮缩放）",
                "只改终端网格。侧栏与设置页的文字锚在配置字号上，不会跟着一起放大。",
                "font-size",
                font_size, // 整数步进：分数字号（滚轮缩放遗留，如 15.30）先吸附回
                // 最近的整数档，再继续 ±1——不会出现 15.3→14.3 这类漂移。
                |this, cx| {
                    let size = (this.terminal_font_size_px(cx).ceil() - 1.0).round();
                    this.set_font_size(size, cx);
                },
                |this, cx| {
                    let size = (this.terminal_font_size_px(cx).floor() + 1.0).round();
                    this.set_font_size(size, cx);
                },
                cx,
            ))
            .child(self.select_row("cell_width_mode", "字体间距", "列宽的取整方式。紧凑向下取整、字更密；宽松向上补一像素，专治 `Maple Mono` 这类平均字宽带小数的字体把字形挤扁。只作用于终端网格。", cx))
            .child(self.switch_row(
                "fetch",
                "启动欢迎信息",
                "新会话开头跑一次 `fastfetch` 打印系统信息。默认关，因为开新标签会因此慢一拍。",
                self.runtime.fetch,
                cx,
            ))
            .child(self.switch_row(
                "powerline",
                "Powerline 提示符",
                "给 Nebula 注入的提示符加箭头分段。需要终端字体带 Powerline 字形，否则那些箭头会显示成方框。",
                self.runtime.powerline,
                cx,
            ));

        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(preview)
            .child(themes)
            .child(theme_mode)
            .child(custom_background)
            .child(cursor)
            .child(interface)
            .child(terminal)
    }

    fn section_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let font_picker = self.font_picker_dropdown(window, cx);
        let terminal = self
            .group("终端", cx)
            .child(self.shell_select_row(cx))
            .child(self.startup_directory_row(cx))
            .child(self.select_row(
                "bell",
                "终端铃声",
                "程序发出 BEL 时的反应。开放工位上选闪烁，免得一次 Tab 补全失败响得整层楼都听见。",
                cx,
            ))
            .child(font_picker);
        let completion = self
            .group("补全", cx)
            .child(self.switch_row(
                "ghost",
                "启用命令补全",
                "按历史在光标后给出灰色建议，按下接受键才真正写进命令行。关掉后不再出现任何建议。",
                self.runtime.ghost,
                cx,
            ))
            .child(self.select_row(
                "accept",
                "补全接受键",
                "用哪个键把灰色建议收下。Tab 在不少 shell 里已经绑了原生补全，撞车时改成右方向键。",
                cx,
            ))
            .child(self.select_row(
                "completion_style",
                "补全样式",
                "行内灰字续在光标后，不挡住下方输出；弹窗列表能一次看到多个候选，代价是会盖住一片终端内容。",
                cx,
            ));
        v_flex().w_full().gap(px(GROUP_GAP)).child(terminal).child(completion)
    }

    fn section_providers(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let active_index = self.active_provider_index().unwrap_or(0);
        let active = self.provider_store.providers.get(active_index).cloned();
        let active_id = active.as_ref().map(|provider| provider.id.clone()).unwrap_or_default();
        let provider_rows = self.provider_store.providers.iter().map(|provider| {
            let id = provider.id.clone();
            let selected = provider.id == active_id;
            let name = provider.name.clone();
            let kind = provider.kind.label();
            h_flex()
                .id(SharedString::from(format!("provider-row-{}", provider.id)))
                .h(px(34.0))
                .w_full()
                .px_2()
                .gap_2()
                .items_center()
                .rounded_md()
                .when(selected, |row| row.bg(theme.list_active))
                .hover(move |row| row.bg(hover_bg))
                .child(Icon::new(IconName::Bot).xsmall().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(
                    div()
                        .max_w(px(78.0))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(kind),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_provider(id.clone(), window, cx);
                }))
        });

        let mut editor = v_flex().flex_1().min_w_0().gap_3();
        if let Some(provider) = active {
            let key_status: SharedString = if provider.api_key_set {
                if provider.api_key_hint.is_empty() {
                    "已保存在系统凭据管理器".into()
                } else {
                    provider.api_key_hint.clone().into()
                }
            } else if provider.kind.requires_api_key() {
                "未设置".into()
            } else {
                "此供应商不需要 API Key".into()
            };
            let enabled = provider.enabled;
            let goals = provider.codex_goals;
            let remote = provider.codex_remote_compaction;
            editor = editor
                .child(
                    self.row(
                        "启用",
                        "关掉后这个供应商不再出现在 AI 启动器里，配置和密钥都留着，随时可以开回来。",
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-enabled")
                            .checked(enabled)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("enabled", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(self.row(
                    "名称",
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[0])),
                    cx,
                ))
                .child(self.row(
                    "备注",
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[1])),
                    cx,
                ))
                .child(self.row(
                    "官方网站",
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[2])),
                    cx,
                ))
                .child(self.row(
                    "API 请求地址",
                    "供应商的 API 根地址，多数以 `/v1` 结尾。这里填错不会在保存时报错，而是等到第一次对话请求才失败。",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[3])),
                    cx,
                ))
                .child(self.row(
                    "默认模型",
                    "新会话默认用的模型名，按供应商文档里的写法逐字填——名字对不上时同样是发起请求那一刻才报错。",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[4])),
                    cx,
                ))
                .child(
                    self.row(
                        "API Key",
                        "密钥不写进设置文件，这里只显示它是否已经设过。换一把新的直接点替换，旧的会被覆盖。",
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(key_status),
                            )
                            .child(
                                NebulaButton::new("provider-set-key")
                                    .label(if provider.api_key_set {
                                        "替换…"
                                    } else {
                                        "设置…"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.prompt_provider_key(cx);
                                    })),
                            ),
                        cx,
                    ),
                )
                .child(
                    self.row(
                        "Codex Goals",
                        "写进 `~/.codex/config.toml` 的 `features.goals`。这是 Codex 自己的特性开关，Nebula 只负责把它落到配置里，其它供应商不受影响。",
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-goals")
                            .checked(goals)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_goals", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(
                    self.row(
                        "Codex 远程压缩",
                        "同上，对应 `features.remote_compaction_v2`。同样只在用 Codex 时生效。",
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-remote")
                            .checked(remote)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_remote_compaction", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(NebulaButton::new("provider-save").label("保存").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.save_provider_metadata(cx);
                                cx.notify();
                            }),
                        ))
                        .child(
                            NebulaButton::new("provider-test")
                                .label(if self.provider_test_running {
                                    "测试中…"
                                } else {
                                    "测试连接"
                                })
                                .disabled(self.provider_test_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.test_provider(cx);
                                })),
                        )
                        .child(NebulaButton::new("provider-codex").label("应用到 Codex").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_provider_to_codex(cx);
                            }),
                        ))
                        .child(
                            NebulaButton::new("provider-delete").label("删除").danger().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.delete_provider(window, cx);
                                }),
                            ),
                        ),
                );
        } else {
            editor = editor.child(div().text_color(theme.muted_foreground).child("没有供应商配置"));
        }

        self.group("供应商", cx)
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .w(px(210.0))
                            .flex_shrink_0()
                            .h(px(420.0))
                            .gap_1()
                            .overflow_y_scrollbar()
                            .children(provider_rows)
                            .child(
                                NebulaButton::new("provider-add").label("+ 自定义供应商").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.add_provider(window, cx);
                                    }),
                                ),
                            ),
                    )
                    .child(editor),
            )
            .when_some(self.provider_status.clone(), |group, (message, error)| {
                group.child(
                    div()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }

    // ---- 备份（本地加密导出/恢复 + 远端同步）----

    /// 读备份密码并预检（真正的强度校验在 `encrypted_backup` 内部再做一
    /// 次；这里只提前给出友好提示）。
    fn backup_passphrase(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let pass = self.backup_pass_input.read(cx).value().to_string();
        if pass.chars().count() < 8 {
            self.backup_status = Some(("备份密码至少 8 位".to_owned(), true));
            cx.notify();
            return None;
        }
        Some(pass)
    }

    /// 后台执行一段备份计算并把结果写回状态行。`Ok` 文案由任务给出。
    fn backup_run_async(
        &mut self,
        task: impl std::future::Future<Output = Result<String, String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.backup_seq = self.backup_seq.wrapping_add(1);
        let seq = self.backup_seq;
        self.backup_busy = true;
        self.backup_status = Some(("处理中…".to_owned(), false));
        let task = cx.background_executor().spawn(task);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                if seq != pane.backup_seq {
                    return;
                }
                pane.backup_busy = false;
                pane.backup_status = Some(match result {
                    Ok(message) => (message, false),
                    Err(error) => (error, true),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 导出：先弹保存对话框（取消则零成本），再后台 collect + seal + 写盘。
    fn export_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        if self.backup_selection.is_empty() {
            self.backup_status = Some(("请至少勾选一个备份类别".to_owned(), true));
            cx.notify();
            return;
        }
        let selection = self.backup_selection;
        let start_dir = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let picked =
            cx.prompt_for_new_path(&start_dir, Some(&format!("nebula-{stamp}.nebula-backup")));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let _ = this.update(cx, |pane, cx| {
                pane.backup_run_async(
                    async move {
                        let archive = crate::encrypted_backup::collect(selection)?;
                        let packet = crate::encrypted_backup::seal(&archive, &pass)?;
                        std::fs::write(&path, packet)
                            .map_err(|err| format!("写入备份文件失败：{err}"))?;
                        Ok(format!("已导出加密备份：{}", path.display()))
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// 恢复：选文件 → 后台解密落盘 → 热应用（设置/主机列表随之重载）。
    fn restore_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 Nebula 加密备份".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |pane, cx| {
                pane.backup_run_async(
                    async move {
                        let packet = std::fs::read(&path)
                            .map_err(|err| format!("读取备份文件失败：{err}"))?;
                        crate::encrypted_backup::restore(&packet, &pass)?;
                        Ok("已从备份恢复（字体/托盘等部分设置重启后生效）".to_owned())
                    },
                    cx,
                );
                // 恢复覆盖了 settings/主机列表等文件：立即重载并通知宿主
                // 热应用。后台任务完成前 UI 短暂显示旧值，可接受。
                pane.reload_after_restore(cx);
            });
        })
        .detach();
    }

    /// 恢复后的单一重载入口（设置/SSH 主机/远端配置）。
    fn reload_after_restore(&mut self, cx: &mut Context<Self>) {
        self.runtime = RuntimeSettings::load();
        self.ssh_hosts = crate::gpui_shell::ssh_hosts::SshHostLists::load();
        self.backup_remote = crate::backup_remote::BackupRemoteConfig::load();
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        cx.set_global(settings);
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    /// 远端配置写盘（读当前协议的非密文槽位输入）。
    fn save_remote_config(&mut self, cx: &mut Context<Self>) {
        let protocol = self.backup_remote.protocol;
        let secret_slot = crate::backup_remote::secret_field(protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            let Some(input) = self.backup_remote_inputs.get(input_ix) else { break };
            let value = input.read(cx).value().trim().to_string();
            self.backup_remote.set_slot(slot, value);
            input_ix += 1;
        }
        self.backup_status = Some(match self.backup_remote.save() {
            Ok(()) => ("远端配置已保存".to_owned(), false),
            Err(err) => (err, true),
        });
        cx.notify();
    }

    /// 密文槽写入系统凭据管理器（WebDAV 密码 / S3 Secret），随后清空输入。
    fn store_remote_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_remote_config(cx);
        let secret = self.backup_secret_input.read(cx).value().to_string();
        if secret.is_empty() {
            self.backup_status = Some(("凭据不能为空".to_owned(), true));
            cx.notify();
            return;
        }
        let result = match self.backup_remote.protocol {
            crate::backup_remote::BackupProtocol::WebDav => {
                crate::backup_remote::store_webdav_password(
                    &self.backup_remote.webdav_username,
                    &secret,
                )
            },
            crate::backup_remote::BackupProtocol::S3 => {
                crate::backup_remote::store_s3_secret(&self.backup_remote.s3_access_key, &secret)
            },
            _ => Err("当前协议不需要独立凭据".to_owned()),
        };
        self.backup_status = Some(match result {
            Ok(()) => ("凭据已写入系统凭据管理器".to_owned(), false),
            Err(err) => (err, true),
        });
        self.backup_secret_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// 立即推送：collect + seal + 按配置协议上传（全部后台）。
    fn push_remote(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        self.save_remote_config(cx);
        if self.backup_selection.is_empty() {
            self.backup_status = Some(("请至少勾选一个备份类别".to_owned(), true));
            cx.notify();
            return;
        }
        let selection = self.backup_selection;
        self.backup_run_async(
            async move {
                crate::backup_remote::validate()?;
                let archive = crate::encrypted_backup::collect(selection)?;
                let packet = crate::encrypted_backup::seal(&archive, &pass)?;
                let location = crate::backup_remote::push(&packet)?;
                Ok(format!("已推送到 {location}"))
            },
            cx,
        );
    }

    /// 恢复最新：按配置协议拉取最新备份并解密落盘（全部后台）。
    fn pull_remote(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        self.save_remote_config(cx);
        self.backup_run_async(
            async move {
                crate::backup_remote::validate()?;
                let (name, packet) = crate::backup_remote::pull_latest()?;
                crate::encrypted_backup::restore(&packet, &pass)?;
                Ok(format!("已从 {name} 恢复（部分设置重启后生效）"))
            },
            cx,
        );
    }

    /// 协议切换：字段独立保存（来回切换不丢配置），槽位输入随协议回填。
    fn select_backup_protocol(
        &mut self,
        protocol: crate::backup_remote::BackupProtocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 先把当前协议的输入落到内存配置，再切换（不写盘，写盘归保存钮）。
        let secret_slot = crate::backup_remote::secret_field(self.backup_remote.protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(self.backup_remote.protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            if let Some(input) = self.backup_remote_inputs.get(input_ix) {
                let value = input.read(cx).value().trim().to_string();
                self.backup_remote.set_slot(slot, value);
            }
            input_ix += 1;
        }
        self.backup_remote.protocol = protocol;
        let secret_slot = crate::backup_remote::secret_field(protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            let value = self.backup_remote.slot(slot).unwrap_or_default().to_owned();
            if let Some(input) = self.backup_remote_inputs.get(input_ix) {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
            input_ix += 1;
        }
        cx.notify();
    }

    fn section_backup(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::backup_remote::BackupProtocol;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let busy = self.backup_busy;
        let selection = self.backup_selection;

        // 类别开关（与共享 `BackupSelection` 一一对应）。
        let categories: [(&'static str, &'static str, bool, fn(&mut Self, bool)); 9] = [
            ("bk-appearance", "外观与主题", selection.appearance, |s, v| {
                s.backup_selection.appearance = v;
            }),
            ("bk-config", "终端配置", selection.config, |s, v| s.backup_selection.config = v),
            ("bk-ssh", "SSH 主机（脱敏）", selection.ssh, |s, v| s.backup_selection.ssh = v),
            ("bk-sync", "同步配置", selection.sync, |s, v| s.backup_selection.sync = v),
            ("bk-assistant", "AI 助手配置", selection.assistant, |s, v| {
                s.backup_selection.assistant = v;
            }),
            ("bk-session", "会话与工作区", selection.session, |s, v| {
                s.backup_selection.session = v;
            }),
            ("bk-dirhist", "目录历史", selection.directory_history, |s, v| {
                s.backup_selection.directory_history = v;
            }),
            ("bk-cmdhist", "命令历史", selection.command_history, |s, v| {
                s.backup_selection.command_history = v;
            }),
            ("bk-fonts", "自装字体", selection.fonts, |s, v| s.backup_selection.fonts = v),
        ];
        // 这九行是一份勾选清单，不逐项写说明：清单的价值在于一眼扫完，
        // 每行挂两行小字会把它撑成九屏。范围与边界由组顶部那句统一交代
        // （端到端加密、私钥永不进包）。
        let category_rows = categories.map(|(id, label, checked, apply)| {
            self.row(
                label,
                "",
                crate::gpui_shell::widgets::NebulaSwitch::new(id).checked(checked).on_click(
                    cx.listener(move |this, value: &bool, _, cx| {
                        apply(this, *value);
                        cx.notify();
                    }),
                ),
                cx,
            )
        });

        let protocol = self.backup_remote.protocol;
        let protocol_buttons = [
            (BackupProtocol::Off, "关闭"),
            (BackupProtocol::Folder, "目录"),
            (BackupProtocol::WebDav, "WebDAV"),
            (BackupProtocol::S3, "S3"),
            (BackupProtocol::Sftp, "SFTP"),
        ]
        .map(|(value, label)| {
            let selected = value == protocol;
            Button::new(SharedString::from(format!("bk-protocol-{}", value.settings_value())))
                .label(label)
                .small()
                .when(selected, |b| b.primary())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_backup_protocol(value, window, cx);
                }))
        });

        // 非密文槽位标签（顺序 = `BackupRemoteConfig::slot` 的下标序）。
        let slot_labels: &[&'static str] = match protocol {
            BackupProtocol::Off => &[],
            BackupProtocol::Folder => &["目标目录"],
            BackupProtocol::WebDav => &["WebDAV 地址", "用户名"],
            BackupProtocol::S3 => &["Endpoint", "Region", "桶/前缀", "Access Key"],
            BackupProtocol::Sftp => &["SSH 目的地 (user@host)", "远端目录"],
        };
        let slot_rows = slot_labels.iter().enumerate().map(|(ix, label)| {
            let input = self.backup_remote_inputs.get(ix).cloned();
            self.row(
                label,
                "",
                div().w(px(300.0)).children(input.map(|input| Input::new(&input))),
                cx,
            )
        });

        let secret_ready = crate::backup_remote::protocol_secret_set(protocol);
        let secret_label = match protocol {
            BackupProtocol::WebDav => Some("WebDAV 密码"),
            BackupProtocol::S3 => Some("S3 Secret Key"),
            _ => None,
        };

        let mut remote_group = self
            .group("远端同步", cx)
            .child(div().text_xs().text_color(muted).child(
                "推送 = 当前勾选类别加密打包后上传；恢复最新 = 拉取远端最新包解密落盘。\
                 SFTP 复用上方 SSH 主机的认证。",
            ))
            .child(h_flex().gap_2().children(protocol_buttons))
            .children(slot_rows);
        if let Some(label) = secret_label {
            remote_group = remote_group.child(
                self.row(
                    label,
                    "",
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(muted).child(if secret_ready {
                            "已设置"
                        } else {
                            "未设置"
                        }))
                        .child(div().w(px(220.0)).child(Input::new(&self.backup_secret_input)))
                        .child(NebulaButton::new("bk-store-secret").label("保存凭据").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.store_remote_secret(window, cx);
                            }),
                        )),
                    cx,
                ),
            );
        }
        if protocol != BackupProtocol::Off {
            remote_group = remote_group.child(
                h_flex()
                    .gap_2()
                    .child(NebulaButton::new("bk-save-remote").label("保存配置").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.save_remote_config(cx);
                        }),
                    ))
                    .child(
                        NebulaButton::new("bk-push")
                            .label(if busy { "处理中…" } else { "立即推送" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.push_remote(cx);
                            })),
                    )
                    .child(
                        NebulaButton::new("bk-pull").label("恢复最新备份").disabled(busy).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.pull_remote(cx);
                            }),
                        ),
                    ),
            );
        }

        let local_group = self
            .group("加密备份", cx)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("端到端加密（密码不落盘）；SSH 私钥永不进包，主机列表脱敏导出。"),
            )
            .children(category_rows)
            .child(self.row(
                "备份密码",
                "导出时用它加密整个包，恢复时要一模一样的一串。密码不落盘、也无从找回——忘了这份备份就打不开了。",
                div().w(px(300.0)).child(Input::new(&self.backup_pass_input)),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        NebulaButton::new("bk-export")
                            .label(if busy { "处理中…" } else { "导出到文件…" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_backup(cx);
                            })),
                    )
                    .child(
                        NebulaButton::new("bk-restore")
                            .label("从文件恢复…")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.restore_backup(cx);
                            })),
                    ),
            );

        v_flex().w_full().gap(px(GROUP_GAP)).child(local_group).child(remote_group).when_some(
            self.backup_status.clone(),
            |page, (message, error)| {
                page.child(
                    div()
                        .pt_4()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            },
        )
    }

    fn section_interaction(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        // 分区名已经写在页头上，组标题再叫一遍"交互"就是同一个词说两遍。
        // 拆成两组反而各自有了名字：一组管鼠标怎么用，一组管标签往哪放。
        v_flex()
            .w_full()
            .gap(px(GROUP_GAP))
            .child(
                self.group("鼠标与选区", cx)
                    .child(self.switch_row(
                        "copy_on_select",
                        "选中即复制（关 = 右键复制/粘贴）",
                        "开着时松开鼠标就进剪贴板，右键直接粘贴。关掉后选中不复制，右键弹菜单——Windows 终端的老习惯。",
                        self.runtime.copy_on_select,
                        cx,
                    ))
                    .child(self.switch_row(
                        "panel_resize",
                        "拖拽调节侧栏宽度",
                        "关掉后侧栏与面板的分界线钉死，拖不动；宽度仍可在别处改，只是不会被误拖。",
                        self.runtime.panel_resize,
                        cx,
                    ))
                    .child(self.switch_row(
                        "cjk_bold_regular",
                        "CJK 粗体使用常规字形（提亮不加粗）",
                        "中日韩字形笔画密，字体引擎合成的伪粗体会糊成一团。开着时这些字改用提亮表达加粗，拉丁字母照旧走真粗体。",
                        self.runtime.cjk_bold_regular,
                        cx,
                    )),
            )
            .child(
                self.group("标签与窗口", cx)
                    .child(self.select_row(
                        "tabs_position",
                        "标签栏位置",
                        "左侧边栏放得下完整路径和分屏结构，顶部则把纵向空间全留给终端。",
                        cx,
                    ))
                    .child(self.select_row(
                        "tab_reveal",
                        "标签展开动效",
                        "滑动=新标签带位移动画；即时=直接出现。远程桌面或低配机上选即时能少掉一次重绘。",
                        cx,
                    ))
                    .child(self.select_row(
                        "new_tab_position",
                        "新标签位置",
                        "只管新建的标签插在哪。会话恢复与工作区导入按各自记录的顺序排，不看这一项。",
                        cx,
                    ))
                    .child(self.select_row(
                        "windowing_behavior",
                        "新建实例行为",
                        "从桌面或命令行再启动一次 Nebula 时：另开一个窗口，还是把它作为新标签并进已有窗口。",
                        cx,
                    ))
                    .child(self.select_row(
                        "vcs_display",
                        "侧栏版本控制（Git/SVN）",
                        "侧栏那块状态读哪种仓库。自动检测按目录里的 `.git` / `.svn` 判断，只有两者并存时才需要手动指定。",
                        cx,
                    )),
            )
    }

    fn section_advanced(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("会话生命周期", cx)
            .child(self.switch_row(
                "keep_session",
                "关窗后保留后台会话",
                "开着时点 × 只是把窗口收走，里面的 shell 继续在常驻进程里跑、可以再附着回来；关掉则连 shell 一起杀，未保存的东西会丢。",
                self.runtime.keep_session,
                cx,
            ))
            .child(self.switch_row(
                "restore_session",
                "启动时恢复上次标签",
                "重开时按上次的标签与分屏布局重建，工作目录一起回来。进程不会续命——恢复出来的是新 shell。",
                self.runtime.restore_session,
                cx,
            ))
            .child(self.switch_row(
                "resume_ai",
                "恢复会话时自动接续 AI 对话",
                "恢复出来的 AI 标签接着上次那段对话，而不是开一段新的。",
                self.runtime.resume_ai,
                cx,
            ))
            .child(self.switch_row(
                "tray",
                "常驻系统托盘图标",
                "在通知区域留一个图标，正在跑的 AI 会话从那里能直接看到状态。",
                self.runtime.tray,
                cx,
            ))
    }

    fn section_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        match self.active_section {
            0 => self.section_home(cx),
            1 => self.section_appearance(cx),
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
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // 选中底直接取 workspace 侧栏那一枚 token。两处都是"当前在哪"的
        // 指示，中间只隔着一条 hairline，用两种蓝会读成两套系统。
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let hairline = crate::gpui_shell::theme::settings_hairline(cx);
        let row_h = px(32.0);
        // 设置导航、内容和 workspace 左侧 tab 共享这一个主文字字号。
        let main_text_px = self.font_size_px(cx);

        // 一级标题靠左，二级导航略向右收进；只靠对齐关系建立层级，
        // 不额外添加卡片或分组标题。
        // 顶上不再画「设置」二字：右栏页头已经写着当前分区名，两者叠在同一
        // 视线高度上就是同一件事说两遍；这个页面是不是设置页，窗口和 tab 早
        // 就说明了。省下的 72px 直接归还给导航项。
        //
        // 分栏靠一条 hairline 而不是留白：留白只说明"这两块不挨着"，线才说明
        // "这是两个区"——导航是全局的，右栏是当前分区的，二者不是同一层。
        let mut nav = v_flex()
            .w(px(SETTINGS_NAV_WIDTH))
            .h_full()
            .flex_shrink_0()
            .px_2()
            .py(px(8.0))
            .gap(px(2.0))
            .border_r_1()
            .border_color(hairline);
        for ix in visible_nav_sections() {
            let active = ix == self.active_section;
            nav = nav.child(
                div()
                    .id(("settings-nav", ix))
                    .px_2()
                    .ml_2()
                    .mr_1()
                    .h(row_h)
                    .flex()
                    .items_center()
                    .gap_2()
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
                        if active { active_fg } else { muted },
                    ))
                    .child(SECTIONS[ix]),
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
        // 现有 reset 合同只覆盖外观键；其它页面显示同一个按钮会产生假承诺。
        let show_reset = self.active_section == 1;
        let hairline = crate::gpui_shell::theme::settings_hairline(cx);

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
                    // 旧壳标题栏固定 72px；正文单独滚动，滚动设置时仍能知道
                    // 自己在哪个分区，也不会让标题与首组的距离随内容变化。
                    .child(
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
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            // 全页唯一放大的一处文字。层级本该
                                            // 靠字重和位置做，但页头是页面唯一
                                            // 的锚点，允许它比正文大一档。
                                            .text_size(px(base_px * 1.2))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(SECTIONS[self.active_section]),
                                    )
                                    .when(show_reset, |header| {
                                        header.child(
                                            div()
                                                .id("settings-reset")
                                                .size(px(24.0))
                                                .rounded_md()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|el| el.bg(cx.theme().list_hover))
                                                .tooltip(|window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        "还原外观为默认值",
                                                    )
                                                    .build(window, cx)
                                                })
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.reset_appearance(window, cx);
                                                }))
                                                .child(Icon::new(IconName::Undo2)),
                                        )
                                    }),
                            ),
                    )
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
                            .child(v_flex().w_full().child(content)),
                    ),
            )
            .when_some(ssh_editor_modal, |root, modal| root.child(modal))
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
mod tests {
    use super::*;

    #[test]
    fn settings_nav_visibility_keeps_stable_routes_but_hides_two_entries() {
        let visibility: Vec<_> = (0..SECTIONS.len()).map(is_nav_section_visible).collect();
        assert_eq!(visibility, vec![true, true, true, false, true, true, true, true, true, false]);
    }

    #[test]
    fn settings_nav_preserves_the_group_expansion_order_without_headings() {
        let visible: Vec<_> = visible_nav_sections().collect();
        assert_eq!(visible, vec![0, 1, 2, 6, 7, 4, 5, 8]);
        let labels: Vec<_> = visible.into_iter().map(|index| SECTIONS[index]).collect();
        assert_eq!(
            labels,
            vec![
                "应用",
                "外观",
                "配置文件",
                "交互",
                "按键映射",
                "SSH",
                "网络",
                "高级"
            ]
        );
    }
}
