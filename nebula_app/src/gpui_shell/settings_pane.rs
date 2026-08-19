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
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored,
    deferred, div, img, px,
};
use gpui_component::input::InputEvent;
use gpui_component::select::{SelectEvent, SelectItem};
use nebula_settings::{RuntimeSettings, ThemeName, format_hex_rgb, persist_keys};
use std::sync::Arc;

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::widgets::NebulaButton;

#[path = "background_color.rs"]
mod background_color;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
const THEME_VALUES: [&str; 7] =
    ["Nebula", "SilverLight", "SteelDark", "LimestoneLight", "CoalDark", "LinenLight", "MossDark"];

const REPOSITORY_URL: &str = "https://github.com/Kuddev/nebula";
const BUG_REPORT_TEMPLATE: &str = "bug_report.yml";

/// 左侧分区导航。主页承载应用身份、版本与支持入口；供应商和备份仍保留
/// 业务实现，但不作为设置主导航入口，避免把旧壳没有的管理页塞进侧栏。
const SECTIONS: [&str; 10] =
    ["主页", "外观", "配置文件", "供应商", "SSH", "网络", "交互", "按键映射", "高级", "备份"];

/// 左侧导航保留稳定的 [`SECTIONS`] 下标，只控制旧壳式显示顺序；AI
/// 供应商（3）和备份（9）仍可由业务层复用，但不再出现在设置侧栏。
const NAV_ORDER: [usize; 8] = [0, 1, 2, 6, 7, 4, 5, 8];

// 这些几何值逐项来自旧壳 `display/settings.rs::settings_geometry`。GPUI
// 设置页沿用同一节奏，避免组件默认间距把标题、分组和表单压成一条均匀列表。
const SETTINGS_NAV_WIDTH: f32 = 196.0;
const SETTINGS_HEADER_HEIGHT: f32 = 72.0;
const SETTINGS_GROUP_GAP: f32 = 32.0;
const SETTINGS_GROUP_TITLE_HEIGHT: f32 = 26.0;
const SETTINGS_GROUP_TITLE_GAP: f32 = 16.0;
const SETTINGS_ROW_HEIGHT: f32 = 44.0;
// 触发条仍 220，对齐旧壳 combobox。弹层加宽，长族名 + 徽标才读得完。
const FONT_PICKER_WIDTH: f32 = 220.0;
const FONT_PICKER_PANEL_WIDTH: f32 = 400.0;

const THEME_NAMES: [ThemeName; 7] = [
    ThemeName::Nebula,
    ThemeName::SilverLight,
    ThemeName::SteelDark,
    ThemeName::LimestoneLight,
    ThemeName::CoalDark,
    ThemeName::LinenLight,
    ThemeName::MossDark,
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

fn section_icon(index: usize) -> IconName {
    match index {
        0 => IconName::LayoutDashboard,
        1 => IconName::Palette,
        2 => IconName::GalleryVerticalEnd,
        3 => IconName::Bot,
        4 => IconName::SquareTerminal,
        5 => IconName::Globe,
        6 => IconName::Inspector,
        7 => IconName::ALargeSmall,
        8 => IconName::Settings2,
        _ => IconName::Inbox,
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

/// 宿主（workspace）监听：设置已写盘 / 请求打开 SSH 会话。
pub enum SettingsPaneEvent {
    Changed,
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
    selects: Vec<(&'static str, SharedSelect)>,
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
    /// 字体选择器（旧壳 `SettingsDropdown::Font` 的 GPUI 形态）：展开态 +
    /// 「显示全部」临时过滤（不落盘）+ 惰性枚举的系统/导入字体目录。
    pub(super) font_picker_open: bool,
    font_show_all: bool,
    font_loading: bool,
    /// None = 尚未枚举；首次展开时在后台线程装配（几百字体的机器上
    /// `IsMonospacedFont` 逐族探询是实打实的开销，不挡 UI 帧）。
    font_system: Option<Vec<crate::font_install::SystemFontFamily>>,
    /// GPUI text system 已注册的导入族名（启动扫描 + 本次导入累计）。
    font_imported: Vec<String>,
    font_query_input: Entity<InputState>,
    /// 触发按钮上一帧的窗口坐标。字体目录是宽弹层，不能把整条设置行当
    /// 锚点；否则按钮在右侧、菜单却会从正文左缘展开。
    font_picker_trigger_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    /// 导入/选择的错误驻留条（旧壳 `nebula_font_notice` 同义）。
    font_notice: Option<String>,
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
    _subscriptions: Vec<Subscription>,
}

impl SettingsPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = RuntimeSettings::load();
        let mut selects: Vec<(&'static str, SharedSelect)> = Vec::new();
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
            selects.push((key, select));
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
            SliderState::new().min(0.20).max(1.00).step(0.05).default_value(runtime.opacity)
        });
        subscriptions.push(cx.subscribe(&opacity_slider, |this, _, event: &SliderEvent, cx| {
            match event {
                SliderEvent::Change(value) => this.set_opacity(value.start(), cx),
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
            |this, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => this.set_wallpaper_opacity(value.start(), cx),
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

        let font_query_input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索字体…"));
        subscriptions.push(cx.subscribe_in(
            &font_query_input,
            window,
            |_: &mut Self, _, event: &InputEvent, _, cx| {
                // 列表跟着打字走（旧壳「所见即所搜」）；目录装配在渲染时按
                // 当前查询串现算，这里只需要触发一帧。
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));

        let bg_picker_hsv = {
            let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
            let rgb = runtime.background.unwrap_or([term.r, term.g, term.b]);
            crate::display::rgb_to_hsv(crate::display::color::Rgb::new(rgb[0], rgb[1], rgb[2]))
        };

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
            font_show_all: false,
            font_loading: false,
            font_system: None,
            font_imported: Vec::new(),
            font_query_input,
            font_picker_trigger_bounds: None,
            font_notice: None,
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
            _subscriptions: subscriptions,
        }
    }

    /// 写盘 → 重载单一事实源与全局 `Settings` → 通知宿主热应用。
    pub(super) fn persist(&mut self, updates: &[(&str, String)], cx: &mut Context<Self>) {
        if let Err(err) = persist_keys(updates) {
            eprintln!("[nebula:gpui] failed to persist settings: {err}");
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
    fn request_cover_chrome(&mut self, enable: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !enable {
            self.persist(&[("background_image_cover_chrome", "0".to_owned())], cx);
            return;
        }
        if self.runtime.background_image_cover_chrome {
            return;
        }
        let pane = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let pane = pane.clone();
            center_confirm_dialog(dialog, window)
                .title("让背景图覆盖窗口控件区域？")
                .confirm()
                .button_props(
                    DialogButtonProps::default().ok_text("开启").cancel_text("取消"),
                )
                .child(SharedString::from(
                    "背景图会延伸到标题栏、窗口按钮、Tab 与 SSH 侧栏下方，低对比度图片可能影响操作可见性；界面仍会保留最低不透明度保护。",
                ))
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

    fn set_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let opacity = opacity.clamp(0.2, 1.0);
        self.persist(&[("opacity", format!("{opacity:.2}"))], cx);
    }

    fn set_wallpaper_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let opacity = opacity.clamp(0.05, 1.0);
        self.persist(&[("background_image_opacity", format!("{opacity:.2}"))], cx);
    }

    // ---- 字体选择器（旧壳 toggle_font_picker / font_catalog 的 GPUI 形态）----

    /// 当前生效字体链（settings.txt 覆盖 toml 后的值，可能含逗号 fallback）。
    pub(super) fn current_font_chain(&self, cx: &App) -> String {
        cx.try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from(crate::font_install::REQUIRED_FONT_FAMILY))
    }

    fn toggle_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.font_picker_open {
            self.close_font_picker(window, true, cx);
            return;
        }

        // 旧壳每次重新展开都从完整目录开始，搜索串和上次错误不跨弹层
        // 生命周期残留；昂贵目录只在第一次打开时异步枚举。
        self.font_notice = None;
        self.font_query_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.font_picker_open = true;
        self.ensure_font_catalog(cx);
        self.font_query_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// 关闭字体弹层并清理这次展开的临时查询。`restore_focus` 只在 Esc、
    /// 选中和点击外部时使用；系统文件选择器接管焦点期间不强抢窗口焦点。
    fn close_font_picker(
        &mut self,
        window: &mut Window,
        restore_focus: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.font_picker_open {
            return;
        }
        self.font_picker_open = false;
        self.font_query_input.update(cx, |input, cx| input.set_value("", window, cx));
        if restore_focus {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    /// 惰性装配字体目录：系统族枚举 + 已导入文件逐个探测族名，全部丢
    /// 后台线程（旧壳在 UI 线程同步做；GPUI 帧预算更紧，不值得卡一帧）。
    fn ensure_font_catalog(&mut self, cx: &mut Context<Self>) {
        if self.font_system.is_some() || self.font_loading {
            return;
        }
        #[cfg(windows)]
        {
            self.font_loading = true;
            let task = cx.background_executor().spawn(async move {
                let system = crate::font_install::enumerate_system_font_families();
                let imported: Vec<String> = crate::font_install::imported_font_files()
                    .iter()
                    .filter_map(|path| crate::font_install::probe_font_file_families(path).ok())
                    .flatten()
                    .collect();
                (system, imported)
            });
            cx.spawn(async move |this, cx| {
                let (system, imported) = task.await;
                let _ = this.update(cx, |pane, cx| {
                    pane.font_system = Some(system);
                    for family in imported {
                        if !pane
                            .font_imported
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(&family))
                        {
                            pane.font_imported.push(family);
                        }
                    }
                    pane.font_loading = false;
                    cx.notify();
                });
            })
            .detach();
        }
        #[cfg(not(windows))]
        {
            self.font_system = Some(Vec::new());
        }
    }

    /// 选中即生效：只换主族、保留用户手写的 fallback 链（共享
    /// `replace_primary_font_family`），写盘后经 `persist` 的 Changed 事件
    /// 让宿主逐终端热应用——字体度量变化会走 viewport observe 自动重排
    /// 网格并 resize PTY。
    fn pick_font_family(&mut self, family: String, window: &mut Window, cx: &mut Context<Self>) {
        let chain = self.current_font_chain(cx);
        let next = crate::font_install::replace_primary_font_family(&chain, &family);
        self.font_notice = None;
        self.close_font_picker(window, true, cx);
        self.persist(&[("font_family", next)], cx);
    }

    /// 「导入终端目录…」：选目录 → 后台扫描落盘 → 刷新下拉。
    ///
    /// 对齐旧壳 `Display::import_terminal_directory`。扫描要遍历目录并读每个
    /// 候选 exe 的 PE 头判架构，放 UI 线程会卡住整窗，因此下沉到后台执行器。
    fn import_terminal_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 用户点的是动作行，不是换 shell：先把闭态标题拨回真正生效的那项，
        // 否则扫描期间下拉会显示「导入终端目录…」，像是默认 shell 被改掉了。
        self.restore_shell_selection(window, cx);

        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择终端安装目录".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(directory) = paths.into_iter().next() else { return };
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
                // 新 profile 要进新建终端菜单/命令面板，让宿主重读配置。
                cx.emit(SettingsPaneEvent::Changed);
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

    #[cfg(windows)]
    fn import_font_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("导入终端字体".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(source) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |pane, window, cx| {
                pane.finish_font_import(&source, window, cx);
            });
        })
        .detach();
    }

    /// 导入落库（共享 `store_imported_font`：哈希去重存入数据目录）→
    /// DirectWrite 探测族名 → 注册进 GPUI text system → 立即应用首族
    /// （旧壳 `set_terminal_font_by_index` 的导入分支同义）。
    #[cfg(windows)]
    fn finish_font_import(
        &mut self,
        source: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stored = match crate::font_install::store_imported_font(source) {
            Ok(stored) => stored,
            Err(error) => {
                self.font_notice = Some(error);
                cx.notify();
                return;
            },
        };
        let cleanup = |stored: &crate::font_install::StoredFont| {
            if stored.created {
                let _ = std::fs::remove_file(&stored.path);
            }
        };
        let families = match crate::font_install::probe_font_file_families(&stored.path) {
            Ok(families) => families,
            Err(error) => {
                cleanup(&stored);
                self.font_notice = Some(format!("字体无法加载：{error}"));
                cx.notify();
                return;
            },
        };
        let bytes = match std::fs::read(&stored.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                cleanup(&stored);
                self.font_notice = Some(format!("无法读取导入字体：{error}"));
                cx.notify();
                return;
            },
        };
        if let Err(error) = cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(bytes)]) {
            cleanup(&stored);
            self.font_notice = Some(format!("字体注册失败：{error}"));
            cx.notify();
            return;
        }
        for family in &families {
            if !self.font_imported.iter().any(|known| known.eq_ignore_ascii_case(family)) {
                self.font_imported.push(family.clone());
            }
        }
        if let Some(first) = families.into_iter().next() {
            self.pick_font_family(first, window, cx);
        }
    }

    /// 字体行（收起态）：用真实 GPUI Button 承载下拉锚点。当前主族继承
    /// 设置页正在使用的字体链，因而按钮文字本身就是 WYSIWYG 预览。
    fn font_picker_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let chain = self.current_font_chain(cx);
        let primary: SharedString = crate::renderer::primary_font_family(&chain).to_owned().into();
        let picker = cx.entity().downgrade();
        let control = div()
            .relative()
            .w(px(FONT_PICKER_WIDTH))
            .child(
                Button::new("font-picker-toggle")
                    .w_full()
                    .selected(self.font_picker_open)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_left()
                                    .font_family(primary.clone())
                                    .child(font_display_name(&primary)),
                            )
                            .child(
                                Icon::new(if self.font_picker_open {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            ),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_font_picker(window, cx);
                    })),
            )
            // 与组件库 Popover 相同：用零绘制 canvas 捕获触发控件真实
            // Bounds。坐标来自布局结果，滚动、DPI 和窗口缩放后仍然准确。
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        let _ = picker.update(cx, |picker, _| {
                            picker.font_picker_trigger_bounds = Some(bounds);
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        self.row("字体", control).into_any_element()
    }

    /// 展开面板：搜索 + 「显示全部」临时过滤 + 目录列表（族名用本字体
    /// 绘制、等宽/导入/内置徽标、当前项高亮）+ 导入按钮。目录装配走共享
    /// `font_catalog`（同名合并、当前项保底），内置字体按旧壳规则恒排首位。
    fn font_picker_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::font_install::{FontCatalogEntry, FontSource, REQUIRED_FONT_FAMILY};

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover_bg = theme.list_hover;
        let selected_bg = theme.list_active;
        let warning = theme.warning;
        let chain = self.current_font_chain(cx);
        let primary = crate::renderer::primary_font_family(&chain).to_owned();
        let query = self.font_query_input.read(cx).value().to_string();
        let system = self.font_system.clone().unwrap_or_default();
        let mut catalog = crate::font_install::font_catalog(
            &system,
            &self.font_imported,
            self.font_show_all,
            &query,
            &primary,
        );
        // 内置字体永远排在最前（旧壳 rebuild_font_catalog 同规则），
        // 搜索不命中也不藏——它是出厂兜底，消失会让人以为坏了。
        catalog.retain(|entry| !entry.name.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY));
        catalog.insert(
            0,
            FontCatalogEntry {
                name: REQUIRED_FONT_FAMILY.to_owned(),
                monospaced: true,
                source: FontSource::Imported,
            },
        );

        let badge = |text: &'static str, color: Hsla| {
            div()
                .flex_shrink_0()
                .px(px(6.0))
                .py(px(1.0))
                .rounded_sm()
                .text_xs()
                .text_color(color)
                .border_1()
                .border_color(color.opacity(0.4))
                .child(text)
        };

        let rows: Vec<_> = catalog
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let name = entry.name.clone();
                let family: SharedString = entry.name.clone().into();
                let selected = entry.name.eq_ignore_ascii_case(&primary);
                let required = entry.name.eq_ignore_ascii_case(REQUIRED_FONT_FAMILY);
                h_flex()
                    .id(SharedString::from(format!("font-row-{ix}")))
                    .h(px(36.0))
                    .w_full()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(selected_bg))
                    .hover(|row| row.bg(hover_bg))
                    // 旧壳 begin_preview_face：候选行用该字体自己的字形画名字。
                    // 展示名去掉文件后缀（导入路径偶尔会把 .ttf 带进族名）。
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family)
                            .child(font_display_name(&entry.name)),
                    )
                    .when(required, |row| row.child(badge("内置", muted)))
                    .when(!required && entry.source == FontSource::Imported, |row| {
                        row.child(badge("导入", muted))
                    })
                    .when(!entry.monospaced, |row| row.child(badge("比例", warning)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_font_family(name.clone(), window, cx);
                    }))
            })
            .collect();
        let empty = rows.is_empty();

        v_flex()
            // 面板由字体行的 deferred/anchored 槽托管，不参与设置文档流高度；
            // 弹层宽于触发条，长族名和「内置/导入/比例」徽标并排才读得完。
            .w(px(FONT_PICKER_PANEL_WIDTH))
            .max_w_full()
            .p_3()
            .gap_2()
            // 与 Select/Popover 共用组件库表面，避免字体菜单成为另一套
            // 边框、圆角和阴影语言。
            .popover_style(cx)
            .occlude()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    // 搜索占剩余宽度；「显示全部」和开关 shrink_0 保证不被
                    // 挤出面板。
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.font_query_input)),
                    )
                    .child(div().flex_shrink_0().text_sm().text_color(muted).child("显示全部"))
                    .child(Switch::new("font-show-all").checked(self.font_show_all).on_click(
                        cx.listener(|this, checked: &bool, _, cx| {
                            // 临时过滤，不写入设置（旧壳 toggle_font_show_all）。
                            this.font_show_all = *checked;
                            cx.notify();
                        }),
                    )),
            )
            .when(self.font_loading, |panel| {
                panel.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().xsmall())
                        .child(div().text_sm().text_color(muted).child("正在枚举系统字体…")),
                )
            })
            .when(!self.font_loading, |panel| {
                panel.child(
                    // gap/宽度都必须写在滚动区内层：`overflow_y_scrollbar` 会把
                    // 外层样式搬走（见本文件设置正文处的详注），写在它之前的
                    // gap_1 落在只有一个子项的外层、对行间距无效，行上的
                    // w_full 也会因父级宽度不确定而回落 max-content，使每行
                    // 宽度随字体名长短参差。
                    v_flex().max_h(px(320.0)).overflow_y_scrollbar().child(
                        v_flex().w_full().gap_1().children(rows).when(empty, |list| {
                            list.child(
                                div().py_2().text_sm().text_color(muted).child("没有匹配的字体"),
                            )
                        }),
                    ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child({
                        let button = NebulaButton::new("font-import").label("导入字体");
                        #[cfg(windows)]
                        let button = button.on_click(cx.listener(|this, _, window, cx| {
                            this.import_font_file(window, cx);
                        }));
                        button
                    })
                    .when_some(self.font_notice.clone(), |row, notice| {
                        row.child(div().text_xs().text_color(theme.danger).child(notice))
                    }),
            )
    }

    /// 字体行和浮层的锚点。使用 GPUI 与 `Popover` 相同的真实触发框
    /// Bounds + `deferred(anchored())` 管线：浮层不参与文档流，右边缘与
    /// 按钮右边缘对齐，窗口空间不足时再由 anchored 贴边收拢。
    fn font_picker_dropdown(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let row = self.font_picker_row(cx);
        let panel = self.font_picker_open.then(|| self.font_picker_panel(cx));
        let trigger_bounds = self.font_picker_trigger_bounds;

        div().relative().w_full().h(px(SETTINGS_ROW_HEIGHT)).flex_shrink_0().child(row).when_some(
            panel.zip(trigger_bounds),
            |anchor, (panel, trigger_bounds)| {
                anchor.child(
                    deferred(
                        anchored()
                            .anchor(gpui::Corner::TopRight)
                            .position(trigger_bounds.bottom_right())
                            .offset(gpui::point(px(0.0), px(6.0)))
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel),
                    )
                    // 页面级透明拦截层优先级为普通元素；弹层必须始终压在
                    // 它上面，否则鼠标到不了搜索框和候选行。
                    .with_priority(2),
                )
            },
        )
    }

    pub(super) fn select_of(&self, key: &str) -> Option<SharedSelect> {
        self.selects.iter().find(|(k, _)| *k == key).map(|(_, entity)| entity.clone())
    }

    /// 旧壳设置页靠留白与标题分组，不用外层线框把每段内容圈成盒子。
    pub(super) fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        // 分组标题与侧栏 tab、设置导航共用正文基准字号和 Regular 字重；
        // 层级只靠颜色与留白表达，避免标题额外放大后整页出现三套字号。
        let title_px = self.font_size_px(cx);
        v_flex()
            .w_full()
            .child(
                div()
                    .h(px(SETTINGS_GROUP_TITLE_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(title_px))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(cx.theme().sidebar_accent_foreground)
                    .child(title),
            )
            // 旧壳组标题占 26px，首行位于标题顶端下方 42px；中间这 16px
            // 是形成分组呼吸感的关键，不能交给组件默认 gap 随意变化。
            .child(div().h(px(SETTINGS_GROUP_TITLE_GAP)).flex_shrink_0())
    }

    /// 一级设置组之间的分隔。容器高度保持原有 32px 组间距，细线位于
    /// 中央，因此增加层级提示但不改变页面纵向密度。
    fn group_divider(cx: &Context<Self>) -> gpui::Div {
        div()
            .w_full()
            .h(px(SETTINGS_GROUP_GAP))
            .flex_shrink_0()
            .flex()
            .items_center()
            .child(div().w_full().h(px(1.0)).bg(cx.theme().border))
    }

    pub(super) fn row(&self, label: &'static str, control: impl IntoElement) -> impl IntoElement {
        self.row_inner(label, None, control)
    }

    /// 标题旁的撤销箭头只在该项被覆盖时出现，点下去清回默认
    /// （旧壳背景图的「↶」同一合同）。
    fn row_with_reset(
        &self,
        label: &'static str,
        dirty: bool,
        on_reset: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        control: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reset = dirty.then(|| {
            let id = SharedString::from(format!("setting-reset-{label}"));
            div()
                .id(id)
                .size(px(22.0))
                .rounded_md()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|el| el.bg(cx.theme().list_hover))
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new("还原为默认值").build(window, cx)
                })
                .on_click(cx.listener(move |this, _, window, cx| on_reset(this, window, cx)))
                .child(Icon::new(IconName::Undo2).xsmall())
                .into_any_element()
        });
        self.row_inner(label, reset, control)
    }

    fn row_inner(
        &self,
        label: &'static str,
        reset: Option<gpui::AnyElement>,
        control: impl IntoElement,
    ) -> impl IntoElement {
        // 行高走旧壳密度阶梯（`tokens::control::settings_row`）：标准 44、
        // 紧凑 38——「界面外观」设置由此对设置页真实生效。
        let row_h = if self.runtime.density == nebula_settings::DensityName::Compact {
            38.0
        } else {
            SETTINGS_ROW_HEIGHT
        };
        h_flex()
            .w_full()
            .h(px(row_h))
            .flex_shrink_0()
            .items_center()
            // 旧壳 `settings_geometry`：标签起 row_x+16，控件右缘距行右 16
            // （ROW_INSET）。GPUI 行内两端原来都贴边，长表单左缘和右缘会
            // 呈一条光柱——旧壳的两档缩进是刻意留的呼吸边。
            .pr_4()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl_4()
                    .items_center()
                    .gap_1()
                    .child(div().min_w_0().child(label))
                    .children(reset),
            )
            .child(control)
    }

    fn select_row(
        &self,
        key: &'static str,
        label: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let select = self.select_of(key);
        // 闭态选中值 = accent（旧壳 combobox_value 15 处调用 14 处传
        // sk.accent）。闭框/背景都不带文字色，包一层就能继承下去；右侧
        // chevron 在组件内自带 muted，不会被染色。
        self.row(
            label,
            div()
                .w(px(220.0))
                .text_color(cx.theme().link)
                .children(select.map(|state| Select::new(&state))),
        )
    }

    fn shell_select_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
        self.row(
            "默认 Shell",
            div()
                .w(px(220.0))
                .font_family(family)
                .text_color(cx.theme().link)
                .child(Select::new(&self.shell_select)),
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
        let family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
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
            .font_family(family)
            .text_size(px(size))
            .text_color(foreground)
            .child("user@nebula ~ $ nebula --version")
            .child(format!(
                "Nebula Terminal · {} · {:.0}px",
                self.runtime.font_family.as_deref().unwrap_or("Maple Mono Normal NF CN"),
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
    fn switch_row(
        &self,
        key: &'static str,
        label: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.row(
            label,
            crate::gpui_shell::widgets::NebulaSwitch::new(key).checked(checked).on_click(
                cx.listener(move |this, checked: &bool, window, cx| {
                    this.toggle(key, *checked, window, cx);
                }),
            ),
        )
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
        let label: SharedString = current
            .map(str::to_owned)
            .unwrap_or_else(|| "继承当前目录".to_owned())
            .into();
        let color = if has_dir { cx.theme().link } else { cx.theme().muted_foreground };
        self.row(
            "启动目录",
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
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pick_startup_directory(cx);
                        })),
                )
                .when(has_dir, |row| {
                    row.child(
                        NebulaButton::new("startup-directory-clear").label("清除").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.clear_startup_directory(cx);
                            }),
                        ),
                    )
                }),
        )
    }

    fn pick_startup_directory(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择终端启动目录".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
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
        key: &'static str,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = input.clone();
        self.row(
            label,
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
        )
    }

    fn stepper_row(
        &self,
        label: &'static str,
        id: &'static str,
        display: SharedString,
        on_minus: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        on_plus: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.row(
            label,
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
        )
    }

    /// 旧壳连续量使用轨道而不是加减按钮；数值和轨道共享同一个
    /// `SliderState`，拖动时由订阅统一写盘并热应用。
    fn slider_row(
        &self,
        label: &'static str,
        state: &Entity<SliderState>,
        display: SharedString,
    ) -> impl IntoElement {
        self.row(
            label,
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

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if matches!(self.about_update, AboutUpdateState::Checking) {
            return;
        }
        self.about_update_seq = self.about_update_seq.wrapping_add(1);
        let sequence = self.about_update_seq;
        self.about_update = AboutUpdateState::Checking;
        let task = cx.background_executor().spawn(async { crate::update_check::check_now() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
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
            .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(cx)));
        let identity = v_flex()
            .w(px(300.0))
            .min_w(px(260.0))
            .flex_shrink_0()
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(img(self.about_logo.clone()).size(px(64.0)).rounded(px(12.0)))
                    .child(
                        v_flex()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Nebula"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(format!("版本 {}", env!("CARGO_PKG_VERSION"))),
                            )
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{} · {}",
                                std::env::consts::OS,
                                std::env::consts::ARCH
                            ))),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .text_sm()
                    .text_color(muted)
                    .child("面向本地 Shell、SSH、分屏与 AI 会话的终端工作区。"),
            )
            .child(h_flex().mt_4().gap_2().items_center().child(update_button))
            .child(div().mt_2().text_xs().text_color(status_color).child(status));
        let actions = v_flex()
            .flex_1()
            .min_w(px(320.0))
            .gap(px(2.0))
            .child(Self::about_action_row(
                "about-report-issue",
                IconName::TriangleAlert,
                "提交 issue",
                "生成包含版本、平台与构建方式的预填 GitHub issue",
                issue_url(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-github",
                IconName::GitHub,
                "GitHub",
                "查看源码、开发进度与贡献指南",
                REPOSITORY_URL.to_owned(),
                cx,
            ))
            .child(Self::about_action_row(
                "about-releases",
                IconName::BookOpen,
                "发布记录",
                "查看版本说明并下载最新构建",
                crate::update_check::RELEASES_PAGE.to_owned(),
                cx,
            ));

        self.group("关于 Nebula", cx).child(
            h_flex()
                .w_full()
                .items_start()
                .flex_wrap()
                .gap(px(48.0))
                .child(identity)
                .child(actions),
        )
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
            self.runtime.follow_system_theme,
            cx,
        ));
        let custom_background = self
            .group("自定义背景", cx)
            .child(self.background_color_row(cx))
            .child(self.background_image_row(cx))
            .child(self.select_row("background_image_fit", "背景图像拉伸模式", cx))
            .child(self.select_row("background_image_alignment", "背景图像对齐", cx))
            .child(self.slider_row(
                "背景图像不透明度",
                &self.wallpaper_opacity_slider,
                wallpaper_opacity,
            ))
            .child(self.switch_row(
                "background_image_cover_chrome",
                "将背景图扩展到标题栏和侧边栏",
                self.runtime.background_image_cover_chrome,
                cx,
            ));
        let cursor = self
            .group("光标", cx)
            .child(self.select_row("cursor_shape", "光标形状", cx))
            .child(self.switch_row(
                "cursor_blink",
                "光标闪烁",
                self.runtime.cursor_blink.unwrap_or(true),
                cx,
            ));
        let interface = self
            .group("界面", cx)
            .child(self.select_row("language", "语言", cx))
            .child(self.select_row("density", "界面外观", cx))
            .child(self.slider_row("终端正文不透明度", &self.opacity_slider, opacity))
            .child(self.switch_row("blur", "背景模糊", self.runtime.blur, cx));
        let terminal = self
            .group("终端外观", cx)
            .child(self.stepper_row(
                "终端字号（Ctrl+滚轮缩放）",
                "font-size",
                font_size,
                // 整数步进：分数字号（滚轮缩放遗留，如 15.30）先吸附回
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
            .child(self.select_row("cell_width_mode", "字体间距", cx))
            .child(self.switch_row("fetch", "启动欢迎信息", self.runtime.fetch, cx))
            .child(self.switch_row("powerline", "Powerline 提示符", self.runtime.powerline, cx));

        v_flex()
            .w_full()
            .child(preview)
            .child(Self::group_divider(cx))
            .child(themes)
            .child(Self::group_divider(cx))
            .child(theme_mode)
            .child(Self::group_divider(cx))
            .child(custom_background)
            .child(Self::group_divider(cx))
            .child(cursor)
            .child(Self::group_divider(cx))
            .child(interface)
            .child(Self::group_divider(cx))
            .child(terminal)
    }

    fn section_profiles(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let font_picker = self.font_picker_dropdown(cx);
        let terminal = self
            .group("终端", cx)
            .child(self.shell_select_row(cx))
            .child(self.startup_directory_row(cx))
            .child(self.select_row("bell", "终端铃声", cx))
            .child(font_picker);
        let completion = self
            .group("补全", cx)
            .child(self.switch_row("ghost", "启用命令补全", self.runtime.ghost, cx))
            .child(self.select_row("accept", "补全接受键", cx))
            .child(self.select_row("completion_style", "补全样式", cx));
        v_flex().w_full().child(terminal).child(Self::group_divider(cx)).child(completion)
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
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-enabled")
                            .checked(enabled)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("enabled", *value, cx);
                            })),
                    ),
                )
                .child(
                    self.row(
                        "名称",
                        div().w(px(330.0)).child(Input::new(&self.provider_inputs[0])),
                    ),
                )
                .child(
                    self.row(
                        "备注",
                        div().w(px(330.0)).child(Input::new(&self.provider_inputs[1])),
                    ),
                )
                .child(self.row(
                    "官方网站",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[2])),
                ))
                .child(self.row(
                    "API 请求地址",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[3])),
                ))
                .child(self.row(
                    "默认模型",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[4])),
                ))
                .child(
                    self.row(
                        "API Key",
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
                    ),
                )
                .child(
                    self.row(
                        "Codex Goals",
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-goals")
                            .checked(goals)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_goals", *value, cx);
                            })),
                    ),
                )
                .child(
                    self.row(
                        "Codex 远程压缩",
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-remote")
                            .checked(remote)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_remote_compaction", *value, cx);
                            })),
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
        let category_rows = categories.map(|(id, label, checked, apply)| {
            self.row(
                label,
                crate::gpui_shell::widgets::NebulaSwitch::new(id).checked(checked).on_click(
                    cx.listener(move |this, value: &bool, _, cx| {
                        apply(this, *value);
                        cx.notify();
                    }),
                ),
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
            self.row(label, div().w(px(300.0)).children(input.map(|input| Input::new(&input))))
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
            .child(
                self.row("备份密码", div().w(px(300.0)).child(Input::new(&self.backup_pass_input))),
            )
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

        v_flex()
            .w_full()
            .child(local_group)
            .child(Self::group_divider(cx))
            .child(remote_group)
            .when_some(self.backup_status.clone(), |page, (message, error)| {
                page.child(
                    div()
                        .pt_4()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }

    fn section_interaction(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("交互", cx)
            .child(self.switch_row(
                "copy_on_select",
                "选中即复制（关 = 右键复制/粘贴）",
                self.runtime.copy_on_select,
                cx,
            ))
            .child(self.select_row("tab_reveal", "标签展开动效", cx))
            .child(self.select_row("new_tab_position", "新标签位置", cx))
            .child(self.select_row("vcs_display", "侧栏版本控制（Git/SVN）", cx))
            .child(self.switch_row(
                "panel_resize",
                "拖拽调节侧栏宽度",
                self.runtime.panel_resize,
                cx,
            ))
            .child(self.switch_row(
                "cjk_bold_regular",
                "CJK 粗体使用常规字形（提亮不加粗）",
                self.runtime.cjk_bold_regular,
                cx,
            ))
    }

    // ---- 按键映射编辑器（模型层 `display::keymap`，两壳同读同写）----

    /// 搜索口径与旧壳一致：动作名（中/英）+ 当前生效键文本。
    fn keymap_row_haystack(&self, flat: usize) -> String {
        use crate::display::keymap;
        let custom = keymap::build_bindings(&self.keymap_binds);
        let combo = if flat == keymap::QUICK_TERMINAL_ROW {
            keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey)
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                .map(|(combo, _)| combo)
                .unwrap_or_default()
        };
        let (zh, en) = if flat == keymap::QUICK_TERMINAL_ROW {
            ("快速终端", "Quick terminal")
        } else {
            keymap::EDITABLE_ACTIONS.get(flat - 1).map(|(_, zh, en)| (*zh, *en)).unwrap_or(("", ""))
        };
        format!("{zh} {en} {combo}").to_lowercase()
    }

    /// 过滤后的可见行（flat 下标，升序）。空查询 = 全部。
    fn keymap_visible(&self, cx: &App) -> Vec<usize> {
        use crate::display::keymap;
        let query = self.keymap_search_input.read(cx).value().trim().to_lowercase();
        (0..keymap::editable_row_count())
            .filter(|flat| query.is_empty() || self.keymap_row_haystack(*flat).contains(&query))
            .collect()
    }

    /// 冲突检测（旧壳 `keymap_clash_info` 同义）：同一 combo 多动作 → 逐行
    /// 标记 + 只报第一组的提示条。
    fn keymap_clashes(&self) -> (Vec<bool>, Option<String>) {
        use crate::display::keymap;
        let total = keymap::editable_row_count();
        let custom = keymap::build_bindings(&self.keymap_binds);
        let mut combos: Vec<Option<String>> = Vec::with_capacity(total);
        for flat in 0..total {
            let combo = if flat == keymap::QUICK_TERMINAL_ROW {
                Some(keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey))
            } else {
                keymap::EDITABLE_ACTIONS
                    .get(flat - 1)
                    .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                    .map(|(combo, _)| combo)
            };
            combos.push(combo.filter(|combo| !combo.is_empty()));
        }
        let name = |flat: usize| -> String {
            if flat == keymap::QUICK_TERMINAL_ROW {
                "快速终端".to_owned()
            } else {
                keymap::EDITABLE_ACTIONS
                    .get(flat - 1)
                    .map(|(_, zh, _)| (*zh).to_owned())
                    .unwrap_or_default()
            }
        };
        let mut rows = vec![false; total];
        let mut note = None;
        for a in 0..total {
            let Some(combo_a) = combos[a].clone() else { continue };
            for b in (a + 1)..total {
                let Some(combo_b) = &combos[b] else { continue };
                if !combo_a.eq_ignore_ascii_case(combo_b) {
                    continue;
                }
                rows[a] = true;
                rows[b] = true;
                if note.is_none() {
                    let (a_name, b_name) = (name(a), name(b));
                    note = Some(format!(
                        "{combo_a} 同时绑定了「{a_name}」与「{b_name}」——只有排前面的「{a_name}」会触发"
                    ));
                }
            }
        }
        (rows, note)
    }

    /// 捕获完成：一个动作只保留一条自定义绑定，但同一 combo 可以同时归属
    /// 多个动作。冲突不再靠静默注销旧动作来“解决”，而是由
    /// `keymap_clashes` 标记双方并显示警告条，让用户自己决定改哪一行。
    fn keymap_assign(&mut self, row: usize, combo: String, cx: &mut Context<Self>) {
        use crate::display::keymap;
        self.keymap_capture = None;
        self.keymap_capture_preview.clear();
        if row == keymap::QUICK_TERMINAL_ROW {
            self.persist(&[("quick_terminal_hotkey", combo)], cx);
            return;
        }
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row - 1) else { return };
        let name = keymap::action_storage_name(action);
        self.keymap_binds.retain(|(_, a)| !a.eq_ignore_ascii_case(&name));
        self.keymap_binds.push((combo, name));
        self.persist_keybinds(cx);
    }

    /// 捕获态裸 Backspace：删除自定义绑定，回落内置默认。
    fn keymap_clear_custom(&mut self, row: usize, cx: &mut Context<Self>) {
        use crate::display::keymap;
        self.keymap_capture = None;
        self.keymap_capture_preview.clear();
        if row == keymap::QUICK_TERMINAL_ROW {
            self.persist(
                &[("quick_terminal_hotkey", keymap::DEFAULT_QUICK_TERMINAL_HOTKEY.to_owned())],
                cx,
            );
            return;
        }
        let Some((action, ..)) = keymap::EDITABLE_ACTIONS.get(row - 1) else { return };
        let name = keymap::action_storage_name(action);
        self.keymap_binds.retain(|(_, a)| !a.eq_ignore_ascii_case(&name));
        self.persist_keybinds(cx);
    }

    /// keybind= 整表落盘 → 重载镜像 → 通知宿主热应用（工作区会重注入
    /// gpui 键位表）。persist 走通用路径拿重载与事件，这里补镜像。
    fn persist_keybinds(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = nebula_settings::persist_keybinds(&self.keymap_binds) {
            eprintln!("[nebula:gpui] failed to persist keybinds: {err}");
        }
        self.keymap_binds = nebula_settings::keybind_pairs();
        self.runtime = RuntimeSettings::load();
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    /// 键帽 chip：捕获行回显预览；自定义 accent 描边；冲突 danger 底；
    /// 默认 ink_dim；未绑定 ink_faint（旧壳墨色分级裁定）。
    fn keymap_keycap(
        &self,
        text: &str,
        custom: bool,
        clash: bool,
        capturing: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let color = if clash {
            theme.danger
        } else if capturing {
            theme.link
        } else if custom {
            theme.link
        } else if text.is_empty() {
            crate::gpui_shell::theme::faint_ink(cx)
        } else {
            theme.muted_foreground
        };
        div()
            .min_w(px(72.0))
            .px_2()
            .h(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL * 0.75))
            .text_size(px(self.font_size_px(cx) * 0.86))
            .when(clash, |chip| chip.bg(theme.danger).text_color(theme.danger_foreground))
            .when(!clash && (custom || capturing), |chip| {
                chip.border_1().border_color(theme.link).text_color(color)
            })
            .when(!clash && !custom && !capturing, |chip| {
                chip.border_1().border_color(theme.border).text_color(color)
            })
            .child(if text.is_empty() && !capturing {
                SharedString::from("未绑定")
            } else {
                SharedString::from(text.to_owned())
            })
    }

    /// 一行「动作 + 键帽」。点击行进入捕获态（旧壳点行即捕获）。
    fn keymap_row(&self, flat: usize, clash: bool, cx: &Context<Self>) -> impl IntoElement {
        use crate::display::keymap;
        let custom = keymap::build_bindings(&self.keymap_binds);
        let label: SharedString = if flat == keymap::QUICK_TERMINAL_ROW {
            "快速终端".into()
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .map(|(_, zh, _)| (*zh).to_owned())
                .unwrap_or_default()
                .into()
        };
        let (text, is_custom) = if flat == keymap::QUICK_TERMINAL_ROW {
            (
                keymap::display_stored_combo(&self.runtime.quick_terminal_hotkey),
                self.runtime.quick_terminal_hotkey != keymap::DEFAULT_QUICK_TERMINAL_HOTKEY,
            )
        } else {
            keymap::EDITABLE_ACTIONS
                .get(flat - 1)
                .and_then(|(action, ..)| keymap::effective_combo(action, &custom))
                .map(|(combo, custom)| (combo, custom))
                .unwrap_or_else(|| (String::new(), false))
        };
        let capturing = self.keymap_capture == Some(flat);
        let cap_text = if capturing {
            if self.keymap_capture_preview.is_empty() {
                "按新按键…".to_owned()
            } else {
                format!("{}…", self.keymap_capture_preview)
            }
        } else {
            text.clone()
        };
        h_flex()
            .id(("keymap-row", flat))
            .w_full()
            .h(px(SETTINGS_ROW_HEIGHT))
            .flex_shrink_0()
            .items_center()
            .pr_4()
            .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
            .cursor_pointer()
            .hover(|style| style.bg(crate::gpui_shell::theme::settings_hover_bg(cx, false)))
            // mouse_down 而非 click：容器（section 根）同捕一个 mouse_down
            // 做「点击任何位置先撤销」，这里 stop_propagation 抢先处理
            // 「点别的行 = 捕获转移、点同一行 = 取消」（旧壳 input/chrome.rs
            // 的 SettingsHit::KeymapRow 分支同合同）。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.keymap_capture == Some(flat) {
                        this.keymap_capture = None;
                        this.keymap_capture_preview.clear();
                    } else {
                        this.keymap_capture = Some(flat);
                        this.keymap_capture_preview.clear();
                        // 焦点收到分区根：键盘事件沿根路径冒泡给捕获处理器，
                        // 搜索框不再分走按键。
                        window.focus(&this.focus_handle);
                    }
                    cx.notify();
                }),
            )
            .child(div().flex_1().min_w_0().pl_4().child(label))
            .child(self.keymap_keycap(&cap_text, is_custom, clash, capturing, cx))
    }

    fn section_keymap(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::display::keymap;

        let visible = self.keymap_visible(cx);
        let (clash_rows, clash_note) = self.keymap_clashes();

        // 分组渲染（旧壳无框分组裁定）：组内可见行为空的组整组隐藏；组
        // 标题 0.86× 小字压在行块上方。
        let mut groups_block = v_flex().w_full().gap_1();
        let mut start = 0usize;
        for (zh, _en, count) in keymap::GROUPS {
            let end = start + count;
            let rows: Vec<_> = visible
                .iter()
                .filter(|flat| (start..end).contains(*flat))
                .map(|flat| self.keymap_row(*flat, clash_rows[*flat], cx))
                .collect();
            if !rows.is_empty() {
                groups_block = groups_block.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_size(px(self.font_size_px(cx) * 0.86))
                        .text_color(cx.theme().muted_foreground)
                        .child(*zh),
                );
                for row in rows {
                    groups_block = groups_block.child(row);
                }
            }
            start = end;
        }

        // 只读行：数字系/AI 贴入键（表驱动，不可在图形页编辑）。随搜索过滤。
        let query = self.keymap_search_input.read(cx).value().trim().to_lowercase();
        let readonly: Vec<&(&str, &str, &str)> = keymap::READONLY_ROWS
            .iter()
            .filter(|(zh, en, combo)| {
                query.is_empty() || format!("{zh} {en} {combo}").to_lowercase().contains(&query)
            })
            .collect();
        if !readonly.is_empty() {
            groups_block = groups_block.child(
                div()
                    .pt_3()
                    .pb_1()
                    .text_size(px(self.font_size_px(cx) * 0.86))
                    .text_color(cx.theme().muted_foreground)
                    .child("只读"),
            );
            for (zh, _en, combo) in readonly {
                groups_block = groups_block.child(
                    h_flex()
                        .w_full()
                        .h(px(SETTINGS_ROW_HEIGHT))
                        .flex_shrink_0()
                        .items_center()
                        .pr_4()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pl_4()
                                .text_color(crate::gpui_shell::theme::faint_ink(cx))
                                .child(*zh),
                        )
                        .child(
                            div()
                                .min_w(px(72.0))
                                .px_2()
                                .h(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL * 0.75))
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_size(px(self.font_size_px(cx) * 0.86))
                                .text_color(crate::gpui_shell::theme::faint_ink(cx))
                                .child(*combo),
                        ),
                );
            }
        }

        // 旧壳按键映射页没有悬挂分组标题：搜索框独占整行（row_w × 34px），
        // 占位「搜索动作或按键…」，下面再空 12px 才到冲突条 / 分组。
        v_flex()
            .w_full()
            // 捕获态的「点击任何位置先撤销」（旧壳 input/chrome.rs 的统一撤
            // 销合同）：行的 mouse_down 会 stop_propagation 自行处理转移/
            // 取消，搜索框这里显式撤销（旧壳点搜索框 = blur 捕获），其余
            // 任何落点冒泡到这里 = 纯取消。
            .when(self.keymap_capture.is_some(), |section| {
                section.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if this.keymap_capture.take().is_some() {
                            this.keymap_capture_preview.clear();
                            cx.notify();
                        }
                    }),
                )
            })
            .child(
                div()
                    .w_full()
                    .h(px(34.0))
                    .flex_shrink_0()
                    .child(Input::new(&self.keymap_search_input).w_full()),
            )
            .child(div().h(px(12.0)).w_full().flex_shrink_0())
            // 冲突是允许存在的可见状态，用组件库的 Warning Alert 呈现；
            // 不再用自绘 danger 色块，也不静默删掉另一个动作。
            .when_some(clash_note, |section, note| {
                section.child(Alert::warning("keymap-clash-warning", note).small())
            })
            .child(groups_block)
            .child(
                div()
                    .pt_4()
                    .text_size(px(self.font_size_px(cx) * 0.86))
                    .text_color(cx.theme().muted_foreground)
                    .child("点击行改键 · Backspace 恢复默认 · Esc 取消"),
            )
    }

    fn section_advanced(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("高级", cx)
            .child(self.switch_row(
                "keep_session",
                "关窗后保留后台会话",
                self.runtime.keep_session,
                cx,
            ))
            .child(self.switch_row(
                "restore_session",
                "启动时恢复上次标签",
                self.runtime.restore_session,
                cx,
            ))
            .child(self.switch_row(
                "resume_ai",
                "恢复会话时自动接续 AI 对话",
                self.runtime.resume_ai,
                cx,
            ))
            .child(self.switch_row("tray", "常驻系统托盘图标", self.runtime.tray, cx))
    }

    fn section_content(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        match self.active_section {
            0 => self.section_home(cx),
            1 => self.section_appearance(cx),
            2 => self.section_profiles(cx),
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
        let active_bg = crate::gpui_shell::theme::settings_hover_bg(cx, true);
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let row_h = px(32.0);
        // 设置导航、内容和 workspace 左侧 tab 共享这一个主文字字号。
        let main_text_px = self.font_size_px(cx);

        // 旧壳顶部是品牌标题而不是搜索框；导航入口保持连续排列，避免
        // 额外分组标题把旧版的行距和可视顺序撑开。
        let mut nav =
            v_flex().w(px(SETTINGS_NAV_WIDTH)).h_full().flex_shrink_0().px_2().gap(px(2.0)).child(
                div()
                    .h(px(72.0))
                    .w_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(px(main_text_px * 1.45))
                    .text_color(theme.foreground)
                    .child("Nebula 设置"),
            );
        for ix in NAV_ORDER {
            let active = ix == self.active_section;
            nav = nav.child(
                div()
                    .id(("settings-nav", ix))
                    .px_2()
                    .h(row_h)
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(main_text_px))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .when(active, |item| item.bg(active_bg).text_color(active_fg))
                    .when(!active, |item| item.text_color(muted).hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_section = ix;
                        cx.notify();
                    }))
                    .child(Icon::new(section_icon(ix)).small().text_color(if active {
                        active_fg
                    } else {
                        muted
                    }))
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.render_nav(cx);
        let content = self.section_content(cx);
        // 旧壳设置页全部文字走终端 mono 字体（draw_chrome_text 的字形缓存），
        // sans 只属于组件库默认；根上挂一次，nav/表单/下拉弹层全部继承。
        let family: SharedString = self.current_font_chain(cx).into();
        let base_px = self.font_size_px(cx);
        let font_picker_open = self.font_picker_open;
        let bg_picker_open = self.bg_picker_open && self.active_section == 1;
        let bg_dragging = self.bg_picker_drag.is_some();
        let ssh_editor_modal = self.ssh_editor_modal(cx);
        let show_reset = !matches!(self.active_section, 0 | 4);

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
            // 按键捕获态的键盘独占：焦点在本根 div（点行时收进来），键盘
            // 事件沿焦点路径冒泡到这里；捕获未激活时不挂处理器，输入框、
            // 下拉等组件照常吃键。修饰键实时回显同址（ModifiersChanged
            // 不是 KeyDown，走独立通道）。
            .when_some(self.keymap_capture, |root, _| {
                root.on_key_down(cx.listener(
                    |this, event: &KeyDownEvent, _window, cx| {
                        let Some(row) = this.keymap_capture else { return };
                        cx.stop_propagation();
                        match crate::display::keymap::capture_gpui(&event.keystroke) {
                            crate::display::keymap::CaptureOutcome::Cancel => {
                                this.keymap_capture = None;
                                this.keymap_capture_preview.clear();
                                cx.notify();
                            },
                            crate::display::keymap::CaptureOutcome::ClearCustom => {
                                this.keymap_clear_custom(row, cx);
                            },
                            crate::display::keymap::CaptureOutcome::Bind(combo) => {
                                this.keymap_assign(row, combo, cx);
                            },
                            crate::display::keymap::CaptureOutcome::Pending => {},
                        }
                    },
                ))
                .on_modifiers_changed(cx.listener(
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
            .rounded(crate::gpui_shell::theme::card_radius())
            .bg(crate::gpui_shell::theme::settings_panel_bg(cx))
            .text_color(cx.theme().foreground)
            .font_family(family)
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
                            .px_6()
                            .flex()
                            .items_center()
                            .text_size(px(base_px))
                            .font_weight(gpui::FontWeight::NORMAL)
                            .child(
                                h_flex()
                                    .flex_1()
                                    .items_center()
                                    .gap_1()
                                    .child(SECTIONS[self.active_section])
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
                                                        "还原此页为默认值",
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
                            .px_6()
                            .pt_8()
                            .pb_8()
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
