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
use setting_help::{SettingHelp, help};
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
mod reset;
mod search_header;
mod setting_help;
mod theme_picker;

mod initialization;
mod keymap;
mod localization;
mod navigation;
mod shell_picker;
mod status;

use localization::*;
use navigation::*;
use shell_picker::*;
pub(super) use status::SshStatus;
use status::{
    AboutUpdateState, BackupCompletion, BackupStatus, ProviderStatus, TerminalImportError,
};

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

use super::ssh_settings::{SshDeleteUndo, SshEditorState, SshValidationError};

pub struct SettingsPane {
    pub(super) focus_handle: FocusHandle,
    /// 渲染与写盘的单一事实源；每次 persist 后整体重载。
    pub(super) runtime: RuntimeSettings,
    /// 当前分区（`SECTIONS` 下标）；默认落在应用主页。
    active_section: usize,
    appearance_picker: Option<appearance_picker::AppearancePicker>,
    theme_picker_trigger: FocusHandle,
    icon_picker_trigger: FocusHandle,
    expanded_setting_help: std::collections::HashSet<&'static str>,
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

    /// Workspace tab 与设置正文共用的主文字字号事实源；导航使用组件小字号。
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
        desc: impl Into<SettingHelp>,
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
            help("shell", language),
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
            "terminal_proxy" => flag!(terminal_proxy),
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

    /// 斗隔 union 里 network_settings.rs 也用它。
    pub(super) fn switch_row(
        &self,
        key: &'static str,
        label: &'static str,
        desc: impl Into<SettingHelp>,
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
        desc: impl Into<SettingHelp>,
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
            help("startup_directory", language),
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
                    row.child(
                        NebulaButton::new("startup-directory-clear")
                            .label(language.pick("清除", "Clear"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_startup_directory(cx);
                            })),
                    )
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
        desc: impl Into<SettingHelp>,
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
            help("background_image", language),
            has_image,
            |this, _, cx| {
                this.persist(&[("background_image", String::new())], cx);
            },
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    NebulaButton::new("background-image-choose")
                        .label(language.pick("选择图片", "Choose image"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choose_background_image(cx);
                        })),
                )
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

    fn section_profiles(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let terminal = self
            .group(language.pick("启动", "Startup"), cx)
            .child(self.shell_select_row(cx))
            .child(self.startup_directory_row(cx));
        let alerts = self.group(language.pick("提醒", "Alerts"), cx).child(self.select_row(
            "bell",
            language.pick("终端铃声", "Terminal bell"),
            help("bell", language),
            cx,
        ));
        let completion = self
            .group(language.pick("补全", "Completion"), cx)
            .child(self.switch_row(
                "ghost",
                language.pick("启用命令补全", "Enable command completion"),
                help("ghost", language),
                self.runtime.ghost,
                cx,
            ))
            .child(self.select_row(
                "accept",
                language.pick("补全接受键", "Completion accept key"),
                help("accept", language),
                cx,
            ))
            .child(self.select_row(
                "completion_style",
                language.pick("补全样式", "Completion style"),
                help("completion_style", language),
                cx,
            ));
        v_flex().w_full().gap(px(GROUP_GAP)).child(terminal).child(completion).child(alerts)
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
                        language.pick("选中即复制", "Copy on select"),
                        help("copy_on_select", language),
                        self.runtime.copy_on_select,
                        cx,
                    ))
                    .child(self.switch_row(
                        "multiline_paste_confirm",
                        language.pick("粘贴前确认", "Confirm pastes"),
                        help("multiline_paste_confirm", language),
                        self.runtime.multiline_paste_confirm,
                        cx,
                    ))
                    .child(self.switch_row(
                        "panel_resize",
                        language.pick("拖拽调节侧栏宽度", "Drag to resize the sidebar"),
                        help("panel_resize", language),
                        self.runtime.panel_resize,
                        cx,
                    ))
                    .child(self.switch_row(
                        "cjk_bold_regular",
                        language.pick("中日韩粗体提亮", "Brighten CJK bold"),
                        help("cjk_bold_regular", language),
                        self.runtime.cjk_bold_regular,
                        cx,
                    )),
            )
            .child(
                self.group(language.pick("标签与窗口", "Tabs and windows"), cx)
                    .child(self.select_row(
                        "tabs_position",
                        language.pick("标签栏位置", "Tab bar position"),
                        help("tabs_position", language),
                        cx,
                    ))
                    .child(self.select_row(
                        "tab_reveal",
                        language.pick("标签展开动效", "Tab reveal animation"),
                        help("tab_reveal", language),
                        cx,
                    ))
                    .child(self.select_row(
                        "new_tab_position",
                        language.pick("新标签位置", "New tab position"),
                        help("new_tab_position", language),
                        cx,
                    ))
                    .child(self.select_row(
                        "windowing_behavior",
                        language.pick("新建实例行为", "New instance behavior"),
                        help("windowing_behavior", language),
                        cx,
                    ))
                    .child(self.select_row(
                        "vcs_display",
                        language.pick("侧栏版本控制", "Sidebar version control"),
                        help("vcs_display", language),
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
            .when(caps.hide_window_on_close, |group| {
                group.child(self.switch_row(
                    "keep_session",
                    language.pick(
                        "关窗后保留后台会话",
                        "Keep sessions running after the window closes",
                    ),
                    help("keep_session", language),
                    self.runtime.keep_session,
                    cx,
                ))
            })
            .child(self.switch_row(
                "restore_session",
                language.pick("启动时恢复上次标签", "Restore previous tabs at startup"),
                help("restore_session", language),
                self.runtime.restore_session,
                cx,
            ))
            .child(self.switch_row(
                "resume_ai",
                language.pick("恢复时接续 AI 对话", "Resume AI conversations"),
                help("resume_ai", language),
                self.runtime.resume_ai,
                cx,
            ))
            .when(caps.system_tray, |group| {
                group.child(self.switch_row(
                    "tray",
                    language.pick("常驻系统托盘图标", "Keep an icon in the system tray"),
                    help("tray", language),
                    self.runtime.tray,
                    cx,
                ))
            })
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
        // 返回入口和分类共用紧凑菜单尺寸；字号与搜索框一致，不继承终端配置字号。
        let mut nav = v_flex()
            .w(px(SETTINGS_NAV_WIDTH))
            .h_full()
            .flex_shrink_0()
            .px_2()
            .pt(px(12.0))
            .pb(px(8.0))
            .gap(px(4.0))
            .text_sm()
            .line_height(px(20.0))
            .border_r_1()
            .border_color(hairline)
            .child(
                div()
                    .id("settings-back")
                    .mx_1()
                    .mb(px(20.0))
                    .h(px(SETTINGS_NAV_ROW_HEIGHT))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |item| item.bg(hover_bg).text_color(foreground))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsPaneEvent::Close)))
                    .child(Icon::new(IconName::ArrowLeft).size(px(SETTINGS_NAV_ICON_SIZE)))
                    .child(language.pick("返回工作区", "Back to workspace")),
            );
        for ix in visible_nav_sections() {
            let active = ix == self.active_section;
            nav = nav.child(
                div()
                    .id(("settings-nav", ix))
                    .px(px(10.0))
                    .ml_1()
                    .mr_1()
                    .h(px(SETTINGS_NAV_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded_md()
                    .cursor_pointer()
                    // 选中态同时改变底色、墨色和字重，余光扫过也能确认当前位置。
                    .font_weight(if active {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .when(active, |item| item.bg(active_bg).text_color(active_fg))
                    .when(!active, |item| item.text_color(muted).hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_section = ix;
                        cx.notify();
                    }))
                    .child(
                        Icon::default()
                            .path(section_icon(ix))
                            .size(px(SETTINGS_NAV_ICON_SIZE))
                            .flex_shrink_0()
                            .text_color(if active { active_icon } else { muted }),
                    )
                    .child(section_label(ix, language)),
            );
        }
        nav.child(div().flex_1().min_h(px(24.0)))
            .child(
                Button::new("settings-restore-defaults")
                    .icon(IconName::Undo2)
                    .label(language.pick("恢复默认设置", "Restore defaults"))
                    .ghost()
                    .small()
                    .w_full()
                    .h(px(SETTINGS_NAV_ROW_HEIGHT))
                    .justify_start()
                    .px_3()
                    .text_color(muted)
                    .tooltip(language.pick("恢复设置与快捷键；保留 SSH 主机、凭据和历史，并备份原设置。", "Restores settings and shortcuts. Keeps SSH hosts, credentials and history, and backs up current settings."))
                    .on_click(cx.listener(|this, _, window, cx| this.reset_all_settings(window, cx))),
            )
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
        let ssh_editor_modal = self.ssh_editor_modal(window, cx);
        let appearance_picker_modal = self.appearance_picker_modal(window, cx);
        let application_page = self.active_section == 0;
        let header = self.render_search_header(window, cx);

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
            // 正文继承主字号；导航和搜索使用组件小字号，说明与徽标单独缩小。
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
                    .child(header)
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
                            .when(!application_page, |content| content.pt(px(28.0)).pb(px(30.0)))
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
                                    .child(v_flex().w_full().when(application_page, |content| content.max_w(px(960.0))).child(content)),
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
