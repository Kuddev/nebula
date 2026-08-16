//! 设置页（GPUI 组件版，覆盖旧壳设置的全部纯键值项）。
//!
//! 版面对齐旧壳：**左侧分区导航 + 右侧单分区内容**（分区名与顺序对照
//! `NebulaSettingsSection`：外观/配置文件/供应商/SSH/网络/交互/按键映射/
//! 高级/备份），默认落在外观。
//!
//! 薄壳纪律：本文件不定义任何设置语义——键名、值域、出厂默认、主题色表、
//! 持久化格式全部在共享 crate `nebula-settings`（从旧壳逐字迁移并有单测
//! 锁定）。这里只负责用组件库把字段摆出来；改动经 `persist_keys` 原地
//! 写回 `nebula_settings.txt`，与旧壳读写同一份文件、同一套语义。
//!
//! 生效时机（对齐旧壳）：主题/配色/背景色/字号/copy_on_select 即时热应用；
//! 字体族与默认光标形状对新标签页生效；窗口透明/模糊/托盘/快速终端热键
//! 等依赖尚未迁移的窗口子系统，写盘后由旧壳消费（本页如实标注）。
//!
//! 未迁移的编辑器（依赖各自功能子系统）：按键映射、SSH 主机管理、
//! AI Provider、备份/同步。对应分区放明确的占位说明。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ModifiersChangedEvent, MouseButton,
    ParentElement as _, Render, RenderImage, Rgba as GpuiRgba, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored, deferred, div,
    img, px,
};
use image::Frame;
use std::sync::Arc;
use gpui_component::input::InputEvent;
use gpui_component::select::{SelectEvent, SelectItem};
use nebula_settings::{RuntimeSettings, ThemeName, format_hex_rgb, persist_keys};

use crate::gpui_shell::prelude::*;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
const THEME_VALUES: [&str; 7] =
    ["Nebula", "SilverLight", "SteelDark", "LimestoneLight", "CoalDark", "LinenLight", "MossDark"];

/// 左侧分区导航（名称与顺序对照旧壳 `NebulaSettingsSection`）。
const SECTIONS: [&str; 9] =
    ["外观", "配置文件", "供应商", "SSH", "网络", "交互", "按键映射", "高级", "备份"];

/// 导航的显示顺序与分组，对照旧壳 `settings.rs` 的 `nav_groups`：供应商是
/// 常用业务设置，和外观/配置/交互/按键同列；「连接」只收拢 SSH 与网络。
///
/// 第二项是组内成员在 [`SECTIONS`] 里的下标——**下标就是 section 身份**
/// （`section_content`/`section_icon` 都按它分派），所以调显示顺序或重新分组
/// 只动这张表，不必碰分派代码。
const NAV_GROUPS: [(Option<&str>, &[usize]); 3] =
    [(None, &[0, 1, 2, 5, 6]), (Some("连接"), &[3, 4]), (Some("系统"), &[7, 8])];

// 这些几何值逐项来自旧壳 `display/settings.rs::settings_geometry`。GPUI
// 设置页沿用同一节奏，避免组件默认间距把标题、分组和表单压成一条均匀列表。
const SETTINGS_NAV_WIDTH: f32 = 196.0;
const SETTINGS_HEADER_HEIGHT: f32 = 72.0;
const SETTINGS_GROUP_GAP: f32 = 32.0;
const SETTINGS_GROUP_TITLE_HEIGHT: f32 = 26.0;
const SETTINGS_GROUP_TITLE_GAP: f32 = 16.0;
const SETTINGS_ROW_HEIGHT: f32 = 44.0;

const THEME_NAMES: [ThemeName; 7] = [
    ThemeName::Nebula,
    ThemeName::SilverLight,
    ThemeName::SteelDark,
    ThemeName::LimestoneLight,
    ThemeName::CoalDark,
    ThemeName::LinenLight,
    ThemeName::MossDark,
];

fn section_icon(index: usize) -> IconName {
    match index {
        0 => IconName::Palette,
        1 => IconName::GalleryVerticalEnd,
        2 => IconName::Bot,
        3 => IconName::SquareTerminal,
        4 => IconName::Globe,
        5 => IconName::Inspector,
        6 => IconName::ALargeSmall,
        7 => IconName::Settings2,
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

/// 宿主（workspace）监听：设置已写盘 / 请求打开 SSH 会话。
pub enum SettingsPaneEvent {
    Changed,
    /// 设置页"连接"按钮：宿主开 SSH tab（连接语义在业务层）。
    LaunchSsh(String),
}

type SharedSelect = Entity<SelectState<Vec<SharedString>>>;
type SharedShellSelect = Entity<SelectState<Vec<ShellSelectItem>>>;

#[derive(Clone)]
struct ShellSelectItem {
    id: String,
    name: SharedString,
    image: Option<Arc<RenderImage>>,
}

impl ShellSelectItem {
    fn new(id: String, name: String) -> Self {
        let image = crate::shell_detect::color_icon_png(&id).and_then(|bytes| {
            let mut rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
            // GPUI RenderImage consumes BGRA frames, matching the workspace
            // sidebar loader; keeping the original colors is the point of this
            // picker, unlike the old glyph-only label.
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Some(Arc::new(RenderImage::new([Frame::new(rgba)])))
        });
        Self { id, name: name.into(), image }
    }

    fn view(&self, size: f32) -> gpui::AnyElement {
        let icon: gpui::AnyElement = if let Some(image) = &self.image {
            gpui::StyledImage::object_fit(
                img(image.clone()).size(px(size)).flex_shrink_0(),
                gpui::ObjectFit::Contain,
            )
            .into_any_element()
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

impl SelectItem for ShellSelectItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        // 闭态留出 chevron 与上下内边距；20px 在 32px Select 内不会挤字。
        Some(self.view(20.0))
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // 旧壳 ShellPickerRow 的品牌图标是 24×24 逻辑像素。
        self.view(24.0)
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
    focus_handle: FocusHandle,
    /// 渲染与写盘的单一事实源；每次 persist 后整体重载。
    runtime: RuntimeSettings,
    /// 当前分区（`SECTIONS` 下标）；旧壳默认落在外观。
    active_section: usize,
    selects: Vec<(&'static str, SharedSelect)>,
    shell_select: SharedShellSelect,
    dir_input: Entity<InputState>,
    /// 外观页使用组件库的真实交互控件；状态实体是渲染与拖拽的唯一来源。
    background_color: Entity<ColorPickerState>,
    opacity_slider: Entity<SliderState>,
    wallpaper_opacity_slider: Entity<SliderState>,
    proxy_url_input: Entity<InputState>,
    proxy_bypass_input: Entity<InputState>,
    provider_store: crate::ai_providers::ProviderStore,
    /// Name / note / website / endpoint / model. API keys deliberately do not
    /// use a GPUI text widget; the native credential dialog is write-only.
    provider_inputs: Vec<Entity<InputState>>,
    provider_status: Option<(String, bool)>,
    provider_test_seq: u64,
    provider_test_running: bool,
    provider_codex_confirm: Option<String>,
    /// SSH 主机列表（共享三键 + merge 权威）；操作后整体重载防漂移。
    ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists,
    ssh_new_input: Entity<InputState>,
    ssh_status: Option<(String, bool)>,
    ssh_show_hidden: bool,
    /// 删除确认（二次点击生效，旧壳确认对话框的轻量对应）。
    ssh_delete_confirm: Option<String>,
    /// 字体选择器（旧壳 `SettingsDropdown::Font` 的 GPUI 形态）：展开态 +
    /// 「显示全部」临时过滤（不落盘）+ 惰性枚举的系统/导入字体目录。
    font_picker_open: bool,
    font_show_all: bool,
    font_loading: bool,
    /// None = 尚未枚举；首次展开时在后台线程装配（几百字体的机器上
    /// `IsMonospacedFont` 逐族探询是实打实的开销，不挡 UI 帧）。
    font_system: Option<Vec<crate::font_install::SystemFontFamily>>,
    /// GPUI text system 已注册的导入族名（启动扫描 + 本次导入累计）。
    font_imported: Vec<String>,
    font_query_input: Entity<InputState>,
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
        add_select(
            "cursor_shape",
            &["块状", "竖线", "下划线", "空心块"],
            &["block", "beam", "underline", "hollow"],
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
            "cell_width_mode",
            &["紧凑", "宽松"],
            &["compact", "relaxed"],
            runtime.cell_width_mode.settings_value(),
            window,
            cx,
        );
        add_select(
            "accept",
            &["→ 方向键", "Tab", "两者皆可"],
            &["right", "tab", "both"],
            runtime.accept.settings_value(),
            window,
            cx,
        );
        add_select(
            "completion_style",
            &["内联 ghost", "弹出列表"],
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
        add_select(
            "background_image_fit",
            &["裁剪铺满", "完整显示", "拉伸", "原始尺寸"],
            &["uniform_to_fill", "uniform", "fill", "none"],
            bgimg_fit,
            window,
            cx,
        );
        let bgimg_align = crate::renderer::image::BackgroundImageAlignment::parse(
            runtime.background_image_alignment.as_deref().unwrap_or(""),
        )
        .unwrap_or_default()
        .settings_value();
        add_select(
            "background_image_alignment",
            &["居中", "左上", "上", "右上", "左", "右", "左下", "下", "右下"],
            &[
                "center",
                "top_left",
                "top",
                "top_right",
                "left",
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
            &["关闭", "系统代理", "自定义"],
            &["off", "system", "custom"],
            runtime.ssh_proxy_mode.settings_value(),
            window,
            cx,
        );

        // 与旧壳的默认 Shell 菜单共用检测层：不能在设置页另维护一份两项
        // 白名单，否则 CMD/Nushell/WSL 会出现在新建终端菜单，却无法设为默认。
        // 选项 = 彩色品牌 PNG（extra/shell-icons，与旧壳设置页/命令面板同
        // 一批资产）+ 名称，闭态与下拉同源（SelectItem::display_title/render）。
        let detected = crate::shell_detect::detect_shells();
        let mut shell_items: Vec<ShellSelectItem> = detected
            .into_iter()
            .map(|shell| ShellSelectItem::new(shell.id, shell.name))
            .collect();
        if shell_items.is_empty() {
            // 非 Windows 构建不做安装探测，但历史配置仍支持这两个由 PTY
            // 集成层负责启动的稳定 id，设置页不能因此变成空下拉。
            shell_items = vec![
                ShellSelectItem::new(
                    "powershell".into(),
                    "PowerShell".into(),
                ),
                ShellSelectItem::new("bash".into(), "Git Bash".into()),
            ];
        }
        if !shell_items.iter().any(|item| item.id == shell_current) {
            // 检测结果可能暂时找不到已保存的 WSL/profile id；先把它保留在首位，
            // 用户仍可看到并重新选择，下一次检测恢复后不会丢失持久化值。
            shell_items.insert(
                0,
                ShellSelectItem::new(
                    shell_current.clone(),
                    crate::shell_detect::display_name_for_id(&shell_current).to_owned(),
                ),
            );
        }
        let shell_index = shell_items
            .iter()
            .position(|item| item.id == shell_current)
            .unwrap_or(0);
        let shell_select = cx.new(|cx| {
            SelectState::new(shell_items, Some(IndexPath::default().row(shell_index)), window, cx)
        });
        subscriptions.push(cx.subscribe_in(
            &shell_select,
            window,
            move |this: &mut Self,
                  _entity: &SharedShellSelect,
                  event: &SelectEvent<Vec<ShellSelectItem>>,
                  _window: &mut Window,
                  cx: &mut Context<Self>| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    this.persist(&[("shell", id.clone())], cx);
                }
            },
        ));

        let dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空 = 继承当前窗格目录")
                .default_value(runtime.startup_directory.clone().unwrap_or_default())
        });
        let background_color_value = runtime
            .background
            .map(|[r, g, b]| rgb_hsla(r, g, b))
            .unwrap_or_else(|| cx.theme().background);
        let background_color = cx.new(|cx| {
            ColorPickerState::new(window, cx).default_value(background_color_value)
        });
        subscriptions.push(cx.subscribe(
            &background_color,
            |this, _, event: &ColorPickerEvent, cx| {
                if let ColorPickerEvent::Change(Some(color)) = event {
                    this.persist(&[("background", color.to_hex())], cx);
                }
            },
        ));
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.20)
                .max(1.00)
                .step(0.05)
                .default_value(runtime.opacity)
        });
        subscriptions.push(cx.subscribe(
            &opacity_slider,
            |this, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(value) => this.set_opacity(value.start(), cx),
            },
        ));
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
        let proxy_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("socks5://127.0.0.1:7890")
                .default_value(runtime.ssh_proxy_url.clone())
        });
        let proxy_bypass_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("逗号分隔，如 10.0.0.0/8,*.internal")
                .default_value(runtime.ssh_proxy_no_proxy.clone())
        });
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

        Self {
            focus_handle: cx.focus_handle(),
            runtime,
            active_section: 0,
            selects,
            shell_select,
            dir_input,
            background_color,
            opacity_slider,
            wallpaper_opacity_slider,
            proxy_url_input,
            proxy_bypass_input,
            provider_store,
            provider_inputs,
            provider_status: None,
            provider_test_seq: 0,
            provider_test_running: false,
            provider_codex_confirm: None,
            ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists::load(),
            ssh_new_input: cx.new(|cx| {
                InputState::new(window, cx).placeholder("user@host[:port] 或 ~/.ssh/config 别名")
            }),
            ssh_status: None,
            ssh_show_hidden: false,
            ssh_delete_confirm: None,
            font_picker_open: false,
            font_show_all: false,
            font_loading: false,
            font_system: None,
            font_imported: Vec::new(),
            font_query_input,
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
                    |_this: &mut Self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>| {
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
    fn persist(&mut self, updates: &[(&str, String)], cx: &mut Context<Self>) {
        if let Err(err) = persist_keys(updates) {
            eprintln!("[nebula:gpui] failed to persist settings: {err}");
        }
        self.runtime = RuntimeSettings::load();
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        cx.set_global(settings);
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    fn toggle(&mut self, key: &'static str, value: bool, cx: &mut Context<Self>) {
        self.persist(&[(key, (value as u8).to_string())], cx);
    }

    fn font_size_px(&self, cx: &App) -> f32 {
        self.runtime
            .font_size_px
            .unwrap_or_else(|| cx.global::<crate::gpui_shell::config::Settings>().font_size_px)
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
    fn current_font_chain(&self, cx: &App) -> String {
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
    fn pick_font_family(
        &mut self,
        family: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chain = self.current_font_chain(cx);
        let next = crate::font_install::replace_primary_font_family(&chain, &family);
        self.font_notice = None;
        self.close_font_picker(window, true, cx);
        self.persist(&[("font_family", next)], cx);
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
        Self::row(
            "字体",
            Button::new("font-picker-toggle")
                .w(px(220.0))
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
                                .child(primary),
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
        .into_any_element()
    }

    /// 展开面板：搜索 + 「显示全部」临时过滤 + 目录列表（真实字体预览、
    /// 等宽/导入/内置徽标、当前项高亮）+ 导入按钮。目录装配走共享
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_family(family.clone())
                            .child(entry.name.clone()),
                    )
                    .when(required, |row| row.child(badge("内置", muted)))
                    .when(!required && entry.source == FontSource::Imported, |row| {
                        row.child(badge("导入", muted))
                    })
                    .when(!entry.monospaced, |row| row.child(badge("比例", warning)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(muted)
                            .font_family(family)
                            .child("AaBb 123 终端"),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_font_family(name.clone(), window, cx);
                    }))
            })
            .collect();
        let empty = rows.is_empty();

        v_flex()
            // 面板由字体行的 deferred/anchored 槽托管，不参与设置文档流高度；
            // 固定宽度让族名、徽标和 WYSIWYG 样张同时可读，窄窗由 anchored
            // 自动贴边，行为与组件库 Select 的弹层一致。
            .w(px(520.0))
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
                    .child(div().flex_1().child(Input::new(&self.font_query_input)))
                    .child(div().text_sm().text_color(muted).child("显示全部"))
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
                    v_flex().max_h(px(320.0)).gap_1().overflow_y_scrollbar().children(rows).when(
                        empty,
                        |list| {
                            list.child(
                                div().py_2().text_sm().text_color(muted).child("没有匹配的字体"),
                            )
                        },
                    ),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child({
                        let button = Button::new("font-import").label("导入字体文件…").small();
                        #[cfg(windows)]
                        let button = button.on_click(cx.listener(|this, _, window, cx| {
                            this.import_font_file(window, cx);
                        }));
                        button
                    })
                    .child(
                        div().text_xs().text_color(muted).child("支持 .ttf / .otf / .ttc / .otc"),
                    )
                    .when_some(self.font_notice.clone(), |row, notice| {
                        row.child(div().text_xs().text_color(theme.danger).child(notice))
                    }),
            )
    }

    /// 字体行和浮层的锚点。使用 GPUI 与 `Select` 相同的
    /// `deferred(anchored())` 管线，浮层不会把下面的补全分组向下推，也不会
    /// 被设置正文的滚动容器按普通子项布局。
    fn font_picker_dropdown(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let row = self.font_picker_row(cx);
        let panel = self.font_picker_open.then(|| self.font_picker_panel(cx));

        div()
            .relative()
            .w_full()
            .h(px(SETTINGS_ROW_HEIGHT))
            .flex_shrink_0()
            .child(row)
            .when_some(panel, |anchor, panel| {
                anchor.child(
                    deferred(
                        anchored()
                            .snap_to_window_with_margin(px(8.0))
                            .child(panel.mt_1p5()),
                    )
                    // 页面级透明拦截层优先级为普通元素；弹层必须始终压在
                    // 它上面，否则鼠标到不了搜索框和候选行。
                    .with_priority(2),
                )
            })
    }

    fn select_of(&self, key: &str) -> Option<SharedSelect> {
        self.selects.iter().find(|(k, _)| *k == key).map(|(_, entity)| entity.clone())
    }

    /// 旧壳设置页靠留白与标题分组，不用外层线框把每段内容圈成盒子。
    fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        // 分组标题：1.2× 字号 + ink_strong（旧壳 settings.rs 分组标题裁定）。
        // text_sm + muted 那一档是正文注释的层级，分组标题要压得住下面的行。
        let title_px = self.font_size_px(cx) * 1.2;
        v_flex()
            .w_full()
            .child(
                div()
                    .h(px(SETTINGS_GROUP_TITLE_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(title_px))
                    .text_color(cx.theme().sidebar_accent_foreground)
                    .child(title),
            )
            // 旧壳组标题占 26px，首行位于标题顶端下方 42px；中间这 16px
            // 是形成分组呼吸感的关键，不能交给组件默认 gap 随意变化。
            .child(div().h(px(SETTINGS_GROUP_TITLE_GAP)).flex_shrink_0())
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(SETTINGS_ROW_HEIGHT))
            .flex_shrink_0()
            .items_center()
            // 旧壳 `settings_geometry`：标签起 row_x+16，控件右缘距行右 16
            // （ROW_INSET）。GPUI 行内两端原来都贴边，长表单左缘和右缘会
            // 呈一条光柱——旧壳的两档缩进是刻意留的呼吸边。
            .pr_4()
            .child(div().flex_1().min_w_0().pl_4().child(label))
            .child(control)
    }

    fn select_row(&self, key: &'static str, label: &'static str, cx: &Context<Self>) -> impl IntoElement {
        let select = self.select_of(key);
        // 闭态选中值 = accent（旧壳 combobox_value 15 处调用 14 处传
        // sk.accent）。闭框/背景都不带文字色，包一层就能继承下去；右侧
        // chevron 在组件内自带 muted，不会被染色。
        Self::row(
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
        Self::row(
            "默认 Shell",
            div()
                .w(px(220.0))
                .font_family(family)
                .text_color(cx.theme().link)
                .child(Select::new(&self.shell_select)),
        )
    }

    fn appearance_preview(&self, cx: &Context<Self>) -> gpui::Div {
        let theme = chrome_theme(self.runtime.theme);
        let palette = theme.palette();
        let ink = theme.card_ink().fg;
        let background = self.runtime.background.unwrap_or([
            palette.term_bg.r,
            palette.term_bg.g,
            palette.term_bg.b,
        ]);
        let family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
        let size = self.font_size_px(cx).clamp(11.0, 20.0);
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
            let selected = self.runtime.theme == name;
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.persist(&[("theme", name.prompt_name().to_owned())], cx);
                }))
        }))
    }

    /// 布尔项 = 滑动开关（旧壳设置的 toggle pill 形态），不是勾选框。
    fn switch_row(
        &self,
        key: &'static str,
        label: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::row(
            label,
            Switch::new(key).checked(checked).on_click(cx.listener(
                move |this, checked: &bool, _, cx| {
                    this.toggle(key, *checked, cx);
                },
            )),
        )
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
        Self::row(
            label,
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(280.0)).child(Input::new(input)))
                .child(
                    Button::new(SharedString::from(format!("save-{key}")))
                        .label("保存")
                        .small()
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
        Self::row(
            label,
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new(SharedString::from(format!("minus-{id}")))
                        .label("−")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            on_minus(this, cx);
                        })),
                )
                .child(div().text_sm().min_w(px(64.0)).child(display))
                .child(
                    Button::new(SharedString::from(format!("plus-{id}")))
                        .label("+")
                        .small()
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
        Self::row(
            label,
            h_flex()
                .w(px(220.0))
                .items_center()
                .gap_3()
                .child(div().flex_1().min_w_0().child(Slider::new(state)))
                .child(
                    div()
                        .w(px(48.0))
                        .flex_shrink_0()
                        .text_sm()
                        // 固定数值列宽，百分比位数变化时轨道不会左右跳动。
                        .child(display),
                ),
        )
    }

    fn background_color_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let featured = crate::display::BACKGROUND_SWATCHES
            .iter()
            .map(|color| {
                let (r, g, b) = color.as_tuple();
                rgb_hsla(r, g, b)
            })
            .collect::<Vec<_>>();
        let current: SharedString = self
            .runtime
            .background
            .map(format_hex_rgb)
            .unwrap_or_else(|| "跟随主题".to_owned())
            .into();
        let custom = self.runtime.background.is_some();
        Self::row(
            "背景色",
            h_flex()
                .w(px(280.0))
                .items_center()
                .gap_2()
                .child(
                    ColorPicker::new(&self.background_color)
                        .featured_colors(featured)
                        .small(),
                )
                .child(div().flex_1().min_w_0().text_sm().truncate().child(current))
                .child(
                    Button::new("background-color-reset")
                        .label("跟随主题")
                        .small()
                        .disabled(!custom)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let theme_background = cx.theme().background;
                            this.background_color.update(cx, |state, cx| {
                                state.set_value(theme_background, window, cx);
                            });
                            this.persist(&[("background", String::new())], cx);
                        })),
                ),
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
        let display: SharedString = current
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .unwrap_or("未选择")
            .to_owned()
            .into();
        Self::row(
            "背景图片",
            h_flex()
                .w(px(420.0))
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(display),
                )
                .child(
                    Button::new("background-image-choose")
                        .label("选择图片…")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choose_background_image(cx);
                        })),
                )
                .child(
                    Button::new("background-image-clear")
                        .label("清除")
                        .small()
                        .disabled(!has_image)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.persist(&[("background_image", String::new())], cx);
                        })),
                ),
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

    fn section_appearance(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let font_size: SharedString = format!("{:.1} px", self.font_size_px(cx)).into();
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
            .child(self.slider_row(
                "终端正文不透明度",
                &self.opacity_slider,
                opacity,
            ))
            .child(self.switch_row("blur", "背景模糊", self.runtime.blur, cx));
        let terminal = self
            .group("终端外观", cx)
            .child(self.stepper_row(
                "终端字号（Ctrl+滚轮缩放）",
                "font-size",
                font_size,
                |this, cx| {
                    let size = this.font_size_px(cx) - 0.5;
                    this.set_font_size(size, cx);
                },
                |this, cx| {
                    let size = this.font_size_px(cx) + 0.5;
                    this.set_font_size(size, cx);
                },
                cx,
            ))
            .child(self.select_row("cell_width_mode", "字体间距", cx))
            .child(self.switch_row("fetch", "启动欢迎信息", self.runtime.fetch, cx))
            .child(self.switch_row("powerline", "Powerline 提示符", self.runtime.powerline, cx));

        v_flex()
            .w_full()
            .gap(px(SETTINGS_GROUP_GAP))
            .child(preview)
            .child(themes)
            .child(theme_mode)
            .child(custom_background)
            .child(cursor)
            .child(interface)
            .child(terminal)
    }

    fn section_profiles(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let font_picker = self.font_picker_dropdown(cx);
        let terminal = self
            .group("终端", cx)
            .child(self.shell_select_row(cx))
            .child(self.input_row("启动目录", "startup_directory", &self.dir_input.clone(), cx))
            .child(font_picker);
        let completion = self
            .group("补全", cx)
            .child(self.switch_row("ghost", "启用命令补全", self.runtime.ghost, cx))
            .child(self.select_row("accept", "补全接受键", cx))
            .child(self.select_row("completion_style", "补全样式", cx));
        v_flex().w_full().gap(px(SETTINGS_GROUP_GAP)).child(terminal).child(completion)
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
                .child(div().flex_1().min_w_0().text_sm().truncate().child(name))
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
                .child(Self::row(
                    "启用",
                    Switch::new("provider-enabled").checked(enabled).on_click(cx.listener(
                        |this, value: &bool, _, cx| {
                            this.toggle_provider_flag("enabled", *value, cx);
                        },
                    )),
                ))
                .child(Self::row(
                    "名称",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[0])),
                ))
                .child(Self::row(
                    "备注",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[1])),
                ))
                .child(Self::row(
                    "官方网站",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[2])),
                ))
                .child(Self::row(
                    "API 请求地址",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[3])),
                ))
                .child(Self::row(
                    "默认模型",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[4])),
                ))
                .child(Self::row(
                    "API Key",
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(theme.muted_foreground).child(key_status))
                        .child(
                            Button::new("provider-set-key")
                                .label(if provider.api_key_set { "替换…" } else { "设置…" })
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.prompt_provider_key(cx);
                                })),
                        ),
                ))
                .child(Self::row(
                    "Codex Goals",
                    Switch::new("provider-codex-goals").checked(goals).on_click(cx.listener(
                        |this, value: &bool, _, cx| {
                            this.toggle_provider_flag("codex_goals", *value, cx);
                        },
                    )),
                ))
                .child(Self::row(
                    "Codex 远程压缩",
                    Switch::new("provider-codex-remote").checked(remote).on_click(cx.listener(
                        |this, value: &bool, _, cx| {
                            this.toggle_provider_flag("codex_remote_compaction", *value, cx);
                        },
                    )),
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("provider-save").label("保存").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.save_provider_metadata(cx);
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("provider-test")
                                .label(if self.provider_test_running {
                                    "测试中…"
                                } else {
                                    "测试连接"
                                })
                                .small()
                                .disabled(self.provider_test_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.test_provider(cx);
                                })),
                        )
                        .child(
                            Button::new("provider-codex").label("应用到 Codex").small().on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.apply_provider_to_codex(cx);
                                }),
                            ),
                        )
                        .child(Button::new("provider-delete").label("删除").small().on_click(
                            cx.listener(|this, _, window, cx| {
                                this.delete_provider(window, cx);
                            }),
                        )),
                );
        } else {
            editor = editor
                .child(div().text_sm().text_color(theme.muted_foreground).child("没有供应商配置"));
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
                                Button::new("provider-add")
                                    .label("+ 自定义供应商")
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_provider(window, cx);
                                    })),
                            ),
                    )
                    .child(editor),
            )
            .when_some(self.provider_status.clone(), |group, (message, error)| {
                group.child(
                    div()
                        .text_sm()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }

    /// SSH 列表操作的统一收尾：写盘、报状态、清确认态。列表操作永不触碰
    /// Credential Manager——删除主机保留凭据（连接语义归业务层）。
    fn ssh_apply(
        &mut self,
        mutate: impl FnOnce(&mut crate::gpui_shell::ssh_hosts::SshHostLists),
        status: &str,
        cx: &mut Context<Self>,
    ) {
        mutate(&mut self.ssh_hosts);
        match self.ssh_hosts.persist() {
            Ok(()) => self.ssh_status = Some((status.to_owned(), false)),
            Err(err) => self.ssh_status = Some((format!("写入设置失败: {err}"), true)),
        }
        self.ssh_delete_confirm = None;
        cx.notify();
    }

    fn section_ssh(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let muted = theme.muted_foreground;
        let hosts = self.ssh_hosts.merged();
        let profiles = crate::ssh_profiles::SshProfiles::load(
            &crate::display::nebula_data_dir().join("ssh_profiles.json"),
        )
        .ok();
        let labels = profiles.as_ref().map(|profiles| profiles.labels()).unwrap_or_default();
        let icons = profiles.as_ref().map(|profiles| profiles.icons()).unwrap_or_default();
        let family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
        let hidden: Vec<String> = self.ssh_hosts.hidden_hosts().to_vec();
        let delete_confirm = self.ssh_delete_confirm.clone();

        let host_rows = hosts.into_iter().enumerate().map(|(ix, host)| {
            let pinned = self.ssh_hosts.is_pinned(&host);
            let from_config = self.ssh_hosts.is_from_config(&host);
            let confirm = delete_confirm.as_deref() == Some(host.as_str());
            let label = labels.get(&host).cloned().unwrap_or_else(|| host.clone());
            // 行首 OS 图标（旧壳裁定 2026-08-09）：id 取自 ssh_profiles 存储，
            // 未认出回落通用终端形状；mono 字体渲染 Nerd Font 字位。
            let os_icon = crate::display::ui::os_icons::resolve(icons.get(&host).map(String::as_str));
            let subtitle = if from_config {
                format!("~/.ssh/config · {host}")
            } else {
                format!("已保存 · {host}")
            };
            let connect_host = host.clone();
            let pin_host = host.clone();
            let delete_host = host.clone();
            // 旧壳裁定（display/settings.rs 的 `ssh_host_action_rect` 注释）：
            // 三个动作槽只在 hover 时显形，静态那一行只剩身份信息——一行常显
            // 三枚等价按钮会让人逐个悬停去猜哪个是「进去」。
            let row_group = SharedString::from(format!("ssh-host-actions-{ix}"));
            h_flex()
                .id(SharedString::from(format!("ssh-host-row-{ix}")))
                .group(row_group.clone())
                // 旧壳 `SSH_HOST_ROW_H` 固定 58px；两行文字与 OS 图标在
                // 这个高度里共用中线，不能压成普通 48px 设置行。
                .h(px(58.0))
                .w_full()
                .px_2()
                .items_center()
                .gap_2()
                .rounded_md()
                .hover(move |row| row.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .h_full()
                        .flex_shrink_0()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(family.clone())
                        .text_size(px(18.0))
                        .text_color(muted)
                        .text_center()
                        .child(os_icon.glyph.to_string())
                        // 旧壳把置顶记号压在图标槽右缘，不额外占一列；否则
                        // 置顶行的主机标题会比其它行整体右移。
                        .when(pinned, |slot| {
                            slot.child(
                                div()
                                    .absolute()
                                    .right(px(-2.0))
                                    .bottom(px(7.0))
                                    .text_size(px(8.0))
                                    .text_color(theme.primary)
                                    .child("\u{eab4}"),
                            )
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().truncate().child(label))
                        .child(div().text_xs().text_color(muted).truncate().child(subtitle)),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-connect-{ix}")))
                        .label("连接")
                        .small()
                        .invisible()
                        .group_hover(row_group.clone(), |button| button.visible())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.emit(SettingsPaneEvent::LaunchSsh(connect_host.clone()));
                            this.ssh_status = Some((format!("正在打开 {connect_host}…"), false));
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-pin-{ix}")))
                        .label(if pinned { "取消置顶" } else { "置顶" })
                        .small()
                        .invisible()
                        .group_hover(row_group.clone(), |button| button.visible())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ssh_apply(
                                |lists| lists.toggle_pin(&pin_host),
                                "置顶状态已更新",
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("ssh-delete-{ix}")))
                        .label(if confirm {
                            "确认删除"
                        } else if from_config {
                            "隐藏"
                        } else {
                            "删除"
                        })
                        .small()
                        // 进了确认态就常显：指针移开还让它隐形，等于把「再点
                        // 一次才真删」这个状态藏起来。
                        .when(!confirm, |button| {
                            button
                                .invisible()
                                .group_hover(row_group.clone(), |button| button.visible())
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.ssh_delete_confirm.as_deref() == Some(delete_host.as_str()) {
                                let host = delete_host.clone();
                                let status = if this.ssh_hosts.is_from_config(&host) {
                                    "已隐藏（~/.ssh/config 本身未改动，凭据保留）"
                                } else {
                                    "已从列表移除（凭据保留）"
                                };
                                this.ssh_apply(|lists| lists.remove(&host), status, cx);
                            } else {
                                this.ssh_delete_confirm = Some(delete_host.clone());
                                cx.notify();
                            }
                        })),
                )
        });

        let hidden_rows = self.ssh_show_hidden.then(|| {
            hidden
                .iter()
                .enumerate()
                .map(|(ix, host)| {
                    let restore_host = host.clone();
                    h_flex()
                        .h(px(30.0))
                        .w_full()
                        .px_2()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(muted)
                                .truncate()
                                .child(host.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("ssh-restore-{ix}")))
                                .label("恢复")
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ssh_apply(
                                        |lists| lists.restore_hidden(&restore_host),
                                        "已恢复到主机列表",
                                        cx,
                                    );
                                })),
                        )
                })
                .collect::<Vec<_>>()
        });

        let new_input = self.ssh_new_input.clone();
        let focus_input = self.ssh_new_input.clone();
        let hidden_count = self.ssh_hosts.hidden_hosts().len();

        self.group("SSH 主机", cx)
            .child(
                h_flex()
                    .h(px(32.0))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child("已保存主机"),
                    )
                    .child(
                        Button::new("ssh-add-host")
                            .label("+ 添加主机")
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                focus_input.update(cx, |input, cx| input.focus(window, cx));
                                this.ssh_status = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(
                "保存的目的地 + ~/.ssh/config 别名合并展示（与旧壳同一份数据）。\
                 认证方式/私钥编辑器待迁移：默认自动依次尝试私钥、已存密码与交互认证。",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.ssh_new_input)))
                    .child(Button::new("ssh-save").label("保存").small().on_click(cx.listener(
                        move |this, _, window, cx| {
                            let value = new_input.read(cx).value().trim().to_string();
                            if value.is_empty() {
                                this.ssh_status = Some(("目的地不能为空".to_owned(), true));
                                cx.notify();
                                return;
                            }
                            this.ssh_apply(|lists| lists.remember(&value), "已保存到主机列表", cx);
                            this.ssh_new_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                        },
                    ))),
            )
            .child(v_flex().gap_1().children(host_rows))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("ssh-import").label("重新读取 ~/.ssh/config").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.ssh_hosts = crate::gpui_shell::ssh_hosts::SshHostLists::load();
                                let count = crate::ssh::ssh_config_hosts().len();
                                this.ssh_status =
                                    Some((format!("已导入，config 源共 {count} 个别名"), false));
                                cx.notify();
                            }),
                        ),
                    )
                    .when(hidden_count > 0, |row| {
                        let show = self.ssh_show_hidden;
                        row.child(
                            Button::new("ssh-toggle-hidden")
                                .label(if show {
                                    SharedString::from("收起已隐藏")
                                } else {
                                    SharedString::from(format!("已隐藏 {hidden_count} 项"))
                                })
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ssh_show_hidden = !this.ssh_show_hidden;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when_some(hidden_rows, |group, rows| group.child(v_flex().gap_1().children(rows)))
            .when_some(self.ssh_status.clone(), |group, (message, error)| {
                group.child(
                    div()
                        .text_sm()
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
            Self::row(
                label,
                Switch::new(id).checked(checked).on_click(cx.listener(
                    move |this, value: &bool, _, cx| {
                        apply(this, *value);
                        cx.notify();
                    },
                )),
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
            Self::row(label, div().w(px(300.0)).children(input.map(|input| Input::new(&input))))
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
            remote_group = remote_group.child(Self::row(
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
                    .child(Button::new("bk-store-secret").label("保存凭据").small().on_click(
                        cx.listener(|this, _, window, cx| {
                            this.store_remote_secret(window, cx);
                        }),
                    )),
            ));
        }
        if protocol != BackupProtocol::Off {
            remote_group = remote_group.child(
                h_flex()
                    .gap_2()
                    .child(Button::new("bk-save-remote").label("保存配置").small().on_click(
                        cx.listener(|this, _, _, cx| {
                            this.save_remote_config(cx);
                        }),
                    ))
                    .child(
                        Button::new("bk-push")
                            .label(if busy { "处理中…" } else { "立即推送" })
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.push_remote(cx);
                            })),
                    )
                    .child(
                        Button::new("bk-pull")
                            .label("恢复最新备份")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pull_remote(cx);
                            })),
                    ),
            );
        }

        self.group("加密备份", cx)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("端到端加密（密码不落盘）；SSH 私钥永不进包，主机列表脱敏导出。"),
            )
            .children(category_rows)
            .child(Self::row(
                "备份密码",
                div().w(px(300.0)).child(Input::new(&self.backup_pass_input)),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("bk-export")
                            .label(if busy { "处理中…" } else { "导出到文件…" })
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_backup(cx);
                            })),
                    )
                    .child(
                        Button::new("bk-restore")
                            .label("从文件恢复…")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.restore_backup(cx);
                            })),
                    ),
            )
            .child(remote_group)
            .when_some(self.backup_status.clone(), |group, (message, error)| {
                group.child(
                    div()
                        .text_sm()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }

    fn section_network(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("网络（SSH 出站代理）", cx)
            .child(self.select_row("ssh_proxy_mode", "代理模式", cx))
            .child(self.input_row("代理 URL", "ssh_proxy_url", &self.proxy_url_input.clone(), cx))
            .child(self.input_row(
                "绕过列表",
                "ssh_proxy_no_proxy",
                &self.proxy_bypass_input.clone(),
                cx,
            ))
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
    fn keymap_row(
        &self,
        flat: usize,
        clash: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
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

        self.group("按键映射", cx)
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
            // 搜索框：宽度同其它控件右列（220），旧壳搜索行几何的对应物。
            .child(
                h_flex()
                    .w_full()
                    .h(px(SETTINGS_ROW_HEIGHT))
                    .flex_shrink_0()
                    .items_center()
                    .pr_4()
                    .child(div().flex_1().min_w_0().pl_4().child("搜索"))
                    .child(
                        div().w(px(220.0)).child(Input::new(&self.keymap_search_input).small()),
                    ),
            )
            // 冲突是允许存在的可见状态，用组件库的 Warning Alert 呈现；
            // 不再用自绘 danger 色块，也不静默删掉另一个动作。
            .when_some(clash_note, |section, note| {
                section.child(Alert::warning("keymap-clash-warning", note).small().mt_2())
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
            .child(self.switch_row("tray", "常驻系统托盘图标（旧壳生效）", self.runtime.tray, cx))
    }

    fn section_content(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        match self.active_section {
            0 => self.section_appearance(cx),
            1 => self.section_profiles(cx),
            2 => self.section_providers(cx),
            3 => self.section_ssh(cx),
            4 => self.section_network(cx),
            5 => self.section_interaction(cx),
            6 => self.section_keymap(cx),
            7 => self.section_advanced(cx),
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

        // 86 + 根容器的 2px gap = 旧壳 nav_y0(88)。标题和导航从此共享
        // 同一坐标节奏，连接/系统 caption 也精确占用一个 24px 槽位。
        let mut nav =
            v_flex().w(px(SETTINGS_NAV_WIDTH)).h_full().flex_shrink_0().px_2().gap(px(2.0)).child(
                div()
                    .h(px(86.0))
                    .px_3()
                    .pt_5()
                    .text_xl()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Nebula 设置"),
            );
        for (caption, members) in NAV_GROUPS {
            if let Some(caption) = caption {
                // 根容器在 caption 后还会补 2px gap，所以正文高度取 22px，
                // 合计正好复刻旧壳 `nav_groups` 的 24px 占位。
                nav = nav.child(
                    div()
                        .h(px(22.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .text_xs()
                        .text_color(muted)
                        .child(caption),
                );
            }
            for &ix in members {
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
                        .text_sm()
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
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SECTIONS[self.active_section]),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .px_6()
                            .pt_8()
                            .pb_8()
                            // 正文基准字号跟随终端字号（旧壳 draw_chrome_text
                            // 用 cell 尺寸；用户调字号设置页一起变）。行内不
                            // 再自带 text_sm，缺省继承这里的档位。
                            .text_size(px(base_px))
                            .overflow_y_scrollbar()
                            .child(content)
                            .child(
                                div()
                                    .pt_8()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "写入 nebula_settings.txt，与旧壳共享同一份设置；两边可交替修改。",
                                    ),
                            ),
                    ),
            )
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
    }
}
