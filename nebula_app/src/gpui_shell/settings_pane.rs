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
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Rgba as GpuiRgba,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::input::InputEvent;
use gpui_component::select::SelectEvent;
use nebula_settings::{RuntimeSettings, ThemeName, format_hex_rgb, persist_keys};

use crate::gpui_shell::prelude::*;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
const THEME_VALUES: [&str; 7] =
    ["Nebula", "SilverLight", "SteelDark", "LimestoneLight", "CoalDark", "LinenLight", "MossDark"];

/// 左侧分区导航（名称与顺序对照旧壳 `NebulaSettingsSection`）。
const SECTIONS: [&str; 9] =
    ["外观", "配置文件", "供应商", "SSH", "网络", "交互", "按键映射", "高级", "备份"];

/// 导航的显示顺序与分组，对照旧壳 `settings.rs` 的 `nav_groups`：前四项裸列
/// 不带标题，「连接」收拢供应商/SSH/网络，「系统」收拢高级/备份。
///
/// 第二项是组内成员在 [`SECTIONS`] 里的下标——**下标就是 section 身份**
/// （`section_content`/`section_icon` 都按它分派），所以调显示顺序或重新分组
/// 只动这张表，不必碰分派代码。
const NAV_GROUPS: [(Option<&str>, &[usize]); 3] =
    [(None, &[0, 1, 5, 6]), (Some("连接"), &[2, 3, 4]), (Some("系统"), &[7, 8])];

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
    GpuiRgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    }
    .into()
}

/// 宿主（workspace）监听：设置已写盘 / 请求打开 SSH 会话。
pub enum SettingsPaneEvent {
    Changed,
    /// 设置页"连接"按钮：宿主开 SSH tab（连接语义在业务层）。
    LaunchSsh(String),
}

type SharedSelect = Entity<SelectState<Vec<SharedString>>>;

pub struct SettingsPane {
    focus_handle: FocusHandle,
    /// 渲染与写盘的单一事实源；每次 persist 后整体重载。
    runtime: RuntimeSettings,
    /// 当前分区（`SECTIONS` 下标）；旧壳默认落在外观。
    active_section: usize,
    selects: Vec<(&'static str, SharedSelect)>,
    dir_input: Entity<InputState>,
    bg_input: Entity<InputState>,
    wallpaper_input: Entity<InputState>,
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
        add_select(
            "shell",
            &["PowerShell", "Bash (Git Bash)"],
            &["powershell", "bash"],
            &shell_current,
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

        let dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空 = 继承当前窗格目录")
                .default_value(runtime.startup_directory.clone().unwrap_or_default())
        });
        let bg_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("#rrggbb，留空跟随主题")
                .default_value(runtime.background.map(format_hex_rgb).unwrap_or_default())
        });
        let wallpaper_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(r"本地图片路径（png/jpg/webp/bmp），留空关闭")
                .default_value(runtime.background_image.clone().unwrap_or_default())
        });
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

        let font_query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索字体…"));
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
            dir_input,
            bg_input,
            wallpaper_input,
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

    fn toggle_font_picker(&mut self, cx: &mut Context<Self>) {
        self.font_picker_open = !self.font_picker_open;
        if self.font_picker_open {
            // 首次展开才枚举——整个功能里唯一昂贵的一步，放在用户已经
            // 预期有一次加载的时刻（旧壳同判）。
            self.ensure_font_catalog(cx);
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
                    .filter_map(|path| {
                        crate::font_install::probe_font_file_families(path).ok()
                    })
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
    fn pick_font_family(&mut self, family: String, cx: &mut Context<Self>) {
        let chain = self.current_font_chain(cx);
        let next = crate::font_install::replace_primary_font_family(&chain, &family);
        self.font_picker_open = false;
        self.font_notice = None;
        self.persist(&[("font_family", next)], cx);
    }

    #[cfg(windows)]
    fn import_font_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("导入终端字体".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else { return };
            let Some(source) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |pane, cx| pane.finish_font_import(&source, cx));
        })
        .detach();
    }

    /// 导入落库（共享 `store_imported_font`：哈希去重存入数据目录）→
    /// DirectWrite 探测族名 → 注册进 GPUI text system → 立即应用首族
    /// （旧壳 `set_terminal_font_by_index` 的导入分支同义）。
    #[cfg(windows)]
    fn finish_font_import(&mut self, source: &std::path::Path, cx: &mut Context<Self>) {
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
        if let Err(error) =
            cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(bytes)])
        {
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
            self.pick_font_family(first, cx);
        }
    }

    /// 字体行（收起态）：当前主族用它自己渲染（最诚实的预览）+ 更换按钮。
    fn font_picker_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chain = self.current_font_chain(cx);
        let primary: SharedString = crate::renderer::primary_font_family(&chain).to_owned().into();
        Self::row(
            "字体",
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_family(primary.clone())
                        .child(primary),
                )
                .child(
                    Button::new("font-picker-toggle")
                        .label(if self.font_picker_open { "收起" } else { "更换" })
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_font_picker(cx);
                        })),
                ),
        )
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick_font_family(name.clone(), cx);
                    }))
            })
            .collect();
        let empty = rows.is_empty();

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.font_query_input)))
                    .child(div().text_sm().text_color(muted).child("显示全部"))
                    .child(
                        Switch::new("font-show-all")
                            .checked(self.font_show_all)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                // 临时过滤，不写入设置（旧壳 toggle_font_show_all）。
                                this.font_show_all = *checked;
                                cx.notify();
                            })),
                    ),
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
                    v_flex()
                        .max_h(px(320.0))
                        .gap_1()
                        .overflow_y_scrollbar()
                        .children(rows)
                        .when(empty, |list| {
                            list.child(
                                div()
                                    .py_2()
                                    .text_sm()
                                    .text_color(muted)
                                    .child("没有匹配的字体"),
                            )
                        }),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child({
                        let button = Button::new("font-import").label("导入字体文件…").small();
                        #[cfg(windows)]
                        let button = button.on_click(cx.listener(|this, _, _, cx| {
                            this.import_font_file(cx);
                        }));
                        button
                    })
                    .child(div().text_xs().text_color(muted).child("支持 .ttf / .otf / .ttc / .otc"))
                    .when_some(self.font_notice.clone(), |row, notice| {
                        row.child(div().text_xs().text_color(theme.danger).child(notice))
                    }),
            )
    }

    fn select_of(&self, key: &str) -> Option<SharedSelect> {
        self.selects.iter().find(|(k, _)| *k == key).map(|(_, entity)| entity.clone())
    }

    /// 旧壳设置页靠留白与标题分组，不用外层线框把每段内容圈成盒子。
    fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        v_flex()
            .w_full()
            .max_w(px(760.0))
            .gap_4()
            .py_2()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(title))
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        h_flex().w_full().items_center().child(div().flex_1().text_sm().child(label)).child(control)
    }

    fn select_row(&self, key: &'static str, label: &'static str) -> impl IntoElement {
        let select = self.select_of(key);
        Self::row(label, div().w(px(220.0)).children(select.map(|state| Select::new(&state))))
    }

    fn shell_select_row(&self, cx: &Context<Self>) -> impl IntoElement {
        let select = self.select_of("shell");
        let shell_id = self.runtime.shell.as_deref().unwrap_or("powershell");
        let glyph = crate::shell_detect::icon_for_id(shell_id);
        let family: SharedString = cx
            .try_global::<crate::gpui_shell::config::Settings>()
            .map(|settings| settings.font_family.clone())
            .unwrap_or_else(|| String::from("Maple Mono Normal NF CN"))
            .into();
        Self::row(
            "默认 Shell",
            h_flex()
                .w(px(220.0))
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(18.0))
                        .flex_shrink_0()
                        .font_family(family)
                        .text_sm()
                        .child(glyph),
                )
                .child(div().flex_1().min_w_0().children(select.map(|state| Select::new(&state)))),
        )
    }

    fn appearance_preview(&self, cx: &Context<Self>) -> gpui::Div {
        let theme = chrome_theme(self.runtime.theme);
        let palette = theme.palette();
        let ink = theme.card_ink().fg;
        let background = self
            .runtime
            .background
            .unwrap_or([palette.term_bg.r, palette.term_bg.g, palette.term_bg.b]);
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
            .max_w(px(680.0))
            .h(px(118.0))
            .p_4()
            .gap_1()
            .rounded_lg()
            .bg(rgb_hsla(background[0], background[1], background[2]))
            .font_family(family)
            .text_size(px(size))
            .text_color(foreground)
            .child("user@nebula ~ $ nebula --version")
            .child(format!("Nebula Terminal · {} · {:.0}px", self.runtime.font_family.as_deref().unwrap_or("Maple Mono Normal NF CN"), size))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().text_color(rgb_hsla(accent.r, accent.g, accent.b)).child("❯"))
                    .child(div().w(px(8.0)).h(px(size)).bg(foreground)),
            )
    }

    fn theme_previews(&self, cx: &mut Context<Self>) -> gpui::Div {
        h_flex().w_full().flex_wrap().gap_3().children(THEME_NAMES.into_iter().map(|name| {
            let theme = chrome_theme(name);
            let palette = theme.palette();
            let accent = theme.accent();
            let selected = self.runtime.theme == name;
            div()
                .id(SharedString::from(format!("theme-preview-{}", name.prompt_name())))
                .w(px(142.0))
                .h(px(92.0))
                .p_2()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .cursor_pointer()
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
                        .bg(rgb_hsla(palette.term_bg.r, palette.term_bg.g, palette.term_bg.b))
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
                                .child(
                                    div()
                                        .w(px(52.0))
                                        .h(px(4.0))
                                        .rounded_full()
                                        .bg(rgb_hsla(
                                            theme.card_ink().fg.r,
                                            theme.card_ink().fg.g,
                                            theme.card_ink().fg.b,
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .w(px(82.0))
                                .h(px(4.0))
                                .rounded_full()
                                .bg(rgb_hsla(palette.edge_l.r, palette.edge_l.g, palette.edge_l.b)),
                        )
                        .child(
                            div()
                                .w(px(58.0))
                                .h(px(4.0))
                                .rounded_full()
                                .bg(rgb_hsla(palette.edge_r.r, palette.edge_r.g, palette.edge_r.b)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
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
        self.group("外观", cx)
            .child(self.appearance_preview(cx))
            .child(self.theme_previews(cx))
            .child(self.select_row("theme", "主题"))
            .child(self.switch_row(
                "follow_system_theme",
                "跟随系统外观自动切换深浅",
                self.runtime.follow_system_theme,
                cx,
            ))
            .child(self.input_row(
                "终端背景色（覆盖主题）",
                "background",
                &self.bg_input.clone(),
                cx,
            ))
            .child(self.stepper_row(
                "窗口透明度",
                "opacity",
                opacity,
                |this, cx| {
                    let opacity = this.runtime.opacity - 0.05;
                    this.set_opacity(opacity, cx);
                },
                |this, cx| {
                    let opacity = this.runtime.opacity + 0.05;
                    this.set_opacity(opacity, cx);
                },
                cx,
            ))
            .child(self.switch_row("blur", "背景模糊", self.runtime.blur, cx))
            .child(self.input_row("背景图", "background_image", &self.wallpaper_input.clone(), cx))
            .child(self.stepper_row(
                "背景图不透明度",
                "bgimg-opacity",
                wallpaper_opacity,
                |this, cx| {
                    let opacity = this.runtime.background_image_opacity - 0.05;
                    this.set_wallpaper_opacity(opacity, cx);
                },
                |this, cx| {
                    let opacity = this.runtime.background_image_opacity + 0.05;
                    this.set_wallpaper_opacity(opacity, cx);
                },
                cx,
            ))
            .child(self.select_row("background_image_fit", "背景图适配"))
            .child(self.select_row("background_image_alignment", "背景图对齐"))
            .child(self.switch_row(
                "background_image_cover_chrome",
                "背景图铺满整窗（含侧栏/标题栏）",
                self.runtime.background_image_cover_chrome,
                cx,
            ))
            .child(self.select_row("density", "界面密度"))
            .child(self.stepper_row(
                "字号",
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
            .child(self.select_row("cursor_shape", "光标形状（新标签页生效）"))
            .child(self.switch_row(
                "cursor_blink",
                "光标闪烁",
                self.runtime.cursor_blink.unwrap_or(true),
                cx,
            ))
    }

    fn section_profiles(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("配置文件", cx)
            .child(self.select_row("language", "界面语言"))
            .child(self.shell_select_row(cx))
            .child(self.input_row(
                "新标签启动目录",
                "startup_directory",
                &self.dir_input.clone(),
                cx,
            ))
            .child(self.font_picker_row(cx))
            .when(self.font_picker_open, |group| {
                let panel = self.font_picker_panel(cx);
                group.child(panel)
            })
            .child(self.switch_row("ghost", "启用命令补全", self.runtime.ghost, cx))
            .child(self.select_row("accept", "补全接受键"))
            .child(self.select_row("completion_style", "补全样式"))
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
        let labels = crate::ssh_profiles::SshProfiles::load(
            &crate::display::nebula_data_dir().join("ssh_profiles.json"),
        )
        .map(|profiles| profiles.labels())
        .unwrap_or_default();
        let hidden: Vec<String> = self.ssh_hosts.hidden_hosts().to_vec();
        let delete_confirm = self.ssh_delete_confirm.clone();

        let host_rows = hosts.into_iter().enumerate().map(|(ix, host)| {
            let pinned = self.ssh_hosts.is_pinned(&host);
            let from_config = self.ssh_hosts.is_from_config(&host);
            let confirm = delete_confirm.as_deref() == Some(host.as_str());
            let label = labels.get(&host).cloned().unwrap_or_else(|| host.clone());
            let subtitle = if from_config {
                format!("~/.ssh/config · {host}")
            } else {
                format!("已保存 · {host}")
            };
            let connect_host = host.clone();
            let pin_host = host.clone();
            let delete_host = host.clone();
            h_flex()
                .id(SharedString::from(format!("ssh-host-row-{ix}")))
                .h(px(48.0))
                .w_full()
                .px_2()
                .items_center()
                .gap_2()
                .rounded_md()
                .hover(move |row| row.bg(hover_bg))
                .child(Icon::new(IconName::SquareTerminal).small().text_color(muted))
                .when(pinned, |row| {
                    row.child(div().text_xs().text_color(theme.primary).child("置顶"))
                })
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
        let hidden_count = self.ssh_hosts.hidden_hosts().len();

        self.group("SSH 主机", cx)
            .child(div().text_xs().text_color(muted).child(
                "保存的目的地 + ~/.ssh/config 别名合并展示（与旧壳同一份数据）。\
                 认证方式/私钥编辑器待迁移：默认自动依次尝试私钥、已存密码与交互认证。",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.ssh_new_input)))
                    .child(Button::new("ssh-add").label("添加").small().on_click(cx.listener(
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
            .child(self.select_row("ssh_proxy_mode", "代理模式"))
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
            .child(self.select_row("tab_reveal", "标签展开动效"))
            .child(self.select_row("new_tab_position", "新标签位置"))
            .child(self.select_row("cell_width_mode", "单元格宽度"))
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

    fn section_keymap(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let hotkey: SharedString = self.runtime.quick_terminal_hotkey.clone().into();
        self.group("按键映射", cx)
            .child(Self::row(
                "快速终端热键",
                div().text_sm().text_color(cx.theme().muted_foreground).child(hotkey),
            ))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
                "快捷键编辑器待迁移：绑定表当前请在旧壳设置页修改，\
                 存储于 nebula_settings.txt 的 keybind= 行，两壳同读。",
            ))
    }

    fn section_advanced(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        self.group("高级", cx)
            .child(self.switch_row("fetch", "新会话欢迎屏 fastfetch", self.runtime.fetch, cx))
            .child(self.switch_row(
                "powerline",
                "Powerline 提示符（新会话生效）",
                self.runtime.powerline,
                cx,
            ))
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
        // 行高走 token 阶梯（旧壳导航行同档）：34px 挤得读不出层级，也把
        // 点击热区压到了 token 规定的最小命中之下。
        let row_h = px(crate::display::ui::tokens::control::ROW);

        let mut nav = v_flex().w(px(174.0)).h_full().flex_shrink_0().p_3().gap_1();
        for (caption, members) in NAV_GROUPS {
            if let Some(caption) = caption {
                // 分组标题（旧壳 nav_groups 的「连接」/「系统」）：只做视觉
                // 分隔，不参与命中；上留白比下大，标题贴住它统领的那一组。
                nav = nav.child(
                    div().px_3().pt_4().pb_1().text_xs().text_color(muted).child(caption),
                );
            }
            for &ix in members {
                let active = ix == self.active_section;
                nav = nav.child(
                    div()
                        .id(("settings-nav", ix))
                        .px_3()
                        .h(row_h)
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .cursor_pointer()
                        .text_sm()
                        .when(active, |item| item.bg(active_bg).text_color(active_fg))
                        .when(!active, |item| {
                            item.text_color(muted).hover(|s| s.bg(hover_bg))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_section = ix;
                            cx.notify();
                        }))
                        .child(
                            Icon::new(section_icon(ix))
                                .small()
                                .text_color(if active { active_fg } else { muted }),
                        )
                        .child(SECTIONS[ix]),
                );
            }
        }
        nav.into_any_element()
    }
}
            .flex_shrink_0()
            .p_3()
            .gap_2()
            .children(SECTIONS.iter().enumerate().map(|(ix, label)| {
                let active = ix == self.active_section;
                div()
                    .id(("settings-nav", ix))
                    .px_3()
                    .h(px(34.0))
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
                    .child(
                        Icon::new(section_icon(ix))
                            .small()
                            .text_color(if active { active_fg } else { muted }),
                    )
                    .child(*label)
            }))
            .into_any_element()
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

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            // 旧壳设置页是独立的不透明 panel；终端壁纸只属于终端内容，
            // 不应穿透设置文字和控件。
            //
            // 这层不透明底必须自带卡圆角：外层终端卡虽然 `overflow_hidden`，
            // 但 GPUI 的裁剪是矩形 content_mask、不跟圆角，方角底会直接盖掉
            // 卡的四个圆角——这就是设置页看着「四角是直角」的原因。
            .rounded(crate::gpui_shell::theme::card_radius())
            .bg(crate::gpui_shell::theme::settings_panel_bg(cx))
            .text_color(cx.theme().foreground)
            .flex()
            .flex_row()
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_8()
                    .gap_6()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SharedString::from(format!(
                                "设置 · {}",
                                SECTIONS[self.active_section]
                            ))),
                    )
                    .child(content)
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "写入 nebula_settings.txt，与旧壳共享同一份设置；两边可交替修改。",
                    )),
            )
    }
}
