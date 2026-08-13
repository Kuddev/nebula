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

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui::prelude::FluentBuilder as _;
use gpui_component::select::SelectEvent;
use nebula_settings::{RuntimeSettings, format_hex_rgb, persist_keys};

use crate::gpui_shell::prelude::*;

/// 主题下拉（展示名 = 持久化名，与旧壳一致）。
const THEME_VALUES: [&str; 7] = [
    "Nebula",
    "SilverLight",
    "SteelDark",
    "LimestoneLight",
    "CoalDark",
    "LinenLight",
    "MossDark",
];

/// 左侧分区导航（名称与顺序对照旧壳 `NebulaSettingsSection`）。
const SECTIONS: [&str; 9] =
    ["外观", "配置文件", "供应商", "SSH", "网络", "交互", "按键映射", "高级", "备份"];

/// 宿主（workspace）监听：设置已写盘，全局 `Settings` 已重载。
pub enum SettingsPaneEvent {
    Changed,
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
    proxy_url_input: Entity<InputState>,
    proxy_bypass_input: Entity<InputState>,
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

        Self {
            focus_handle: cx.focus_handle(),
            runtime,
            active_section: 0,
            selects,
            dir_input,
            bg_input,
            proxy_url_input,
            proxy_bypass_input,
            _subscriptions: subscriptions,
        }
    }

    /// 写盘 → 重载单一事实源与全局 `Settings` → 通知宿主热应用。
    fn persist(&mut self, updates: &[(&str, String)], cx: &mut Context<Self>) {
        if let Err(err) = persist_keys(updates) {
            eprintln!("[nebula:gpui] failed to persist settings: {err}");
        }
        self.runtime = RuntimeSettings::load();
        let settings = crate::gpui_shell::config::Settings::load();
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

    fn select_of(&self, key: &str) -> Option<SharedSelect> {
        self.selects.iter().find(|(k, _)| *k == key).map(|(_, entity)| entity.clone())
    }

    /// 分组卡片（右侧内容区的分组式版面，对齐旧壳）。
    fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        v_flex()
            .w_full()
            .max_w(px(680.0))
            .gap_3()
            .p_5()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .bg(cx.theme().group_box)
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(title))
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .child(div().flex_1().text_sm().child(label))
            .child(control)
    }

    fn select_row(&self, key: &'static str, label: &'static str) -> impl IntoElement {
        let select = self.select_of(key);
        Self::row(label, div().w(px(220.0)).children(select.map(|state| Select::new(&state))))
    }

    fn checkbox_row(
        &self,
        key: &'static str,
        label: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Self::row(
            label,
            Checkbox::new(key).checked(checked).on_click(cx.listener(
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

    /// 未迁移分区的占位说明。
    fn pending_group(
        &self,
        title: &'static str,
        note: &'static str,
        cx: &Context<Self>,
    ) -> gpui::Div {
        self.group(title, cx)
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(note))
    }

    // ---- 分区内容（归属对照旧壳各 section）----

    fn section_appearance(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let font_size: SharedString = format!("{:.1} px", self.font_size_px(cx)).into();
        let opacity: SharedString = format!("{:.0}%", self.runtime.opacity * 100.0).into();
        self.group("外观", cx)
            .child(self.select_row("theme", "主题"))
            .child(self.checkbox_row(
                "follow_system_theme",
                "跟随系统外观自动切换深浅",
                self.runtime.follow_system_theme,
                cx,
            ))
            .child(self.input_row("终端背景色（覆盖主题）", "background", &self.bg_input.clone(), cx))
            .child(self.stepper_row(
                "窗口透明度（旧壳窗口生效）",
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
            .child(self.checkbox_row("blur", "背景模糊（旧壳窗口生效）", self.runtime.blur, cx))
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
            .child(self.checkbox_row(
                "cursor_blink",
                "光标闪烁",
                self.runtime.cursor_blink.unwrap_or(true),
                cx,
            ))
    }

    fn section_profiles(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let font_label: SharedString = self
            .runtime
            .font_family
            .clone()
            .unwrap_or_else(|| "（跟随 nebula.toml / 内置默认）".into())
            .into();
        self.group("配置文件", cx)
            .child(self.select_row("language", "界面语言"))
            .child(self.select_row("shell", "默认 Shell"))
            .child(self.input_row("新标签启动目录", "startup_directory", &self.dir_input.clone(), cx))
            .child(Self::row(
                "字体（选择器待迁移）",
                div().text_sm().text_color(cx.theme().muted_foreground).child(font_label),
            ))
            .child(self.checkbox_row("ghost", "启用命令补全", self.runtime.ghost, cx))
            .child(self.select_row("accept", "补全接受键"))
            .child(self.select_row("completion_style", "补全样式"))
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
            .child(self.checkbox_row(
                "copy_on_select",
                "选中即复制（关 = 右键复制/粘贴）",
                self.runtime.copy_on_select,
                cx,
            ))
            .child(self.select_row("tab_reveal", "标签展开动效"))
            .child(self.select_row("new_tab_position", "新标签位置"))
            .child(self.select_row("cell_width_mode", "单元格宽度"))
            .child(self.checkbox_row(
                "panel_resize",
                "拖拽调节侧栏宽度",
                self.runtime.panel_resize,
                cx,
            ))
            .child(self.checkbox_row(
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
            .child(self.checkbox_row("fetch", "新会话欢迎屏 fastfetch", self.runtime.fetch, cx))
            .child(self.checkbox_row(
                "powerline",
                "Powerline 提示符（新会话生效）",
                self.runtime.powerline,
                cx,
            ))
            .child(self.checkbox_row(
                "keep_session",
                "关窗后保留后台会话",
                self.runtime.keep_session,
                cx,
            ))
            .child(self.checkbox_row(
                "restore_session",
                "启动时恢复上次标签",
                self.runtime.restore_session,
                cx,
            ))
            .child(self.checkbox_row(
                "resume_ai",
                "恢复会话时自动接续 AI 对话",
                self.runtime.resume_ai,
                cx,
            ))
            .child(self.checkbox_row(
                "tray",
                "常驻系统托盘图标（旧壳生效）",
                self.runtime.tray,
                cx,
            ))
    }

    fn section_content(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        match self.active_section {
            0 => self.section_appearance(cx),
            1 => self.section_profiles(cx),
            2 => self.pending_group(
                "供应商",
                "AI Provider 编辑器待迁移（API 地址/密钥托管在系统凭据库）。\
                 当前请在旧壳设置页修改。",
                cx,
            ),
            3 => self.pending_group(
                "SSH",
                "SSH 主机管理（保存/置顶/隐藏恢复）待迁移。当前请在旧壳侧栏\
                 与设置页管理，数据两壳同读。",
                cx,
            ),
            4 => self.section_network(cx),
            5 => self.section_interaction(cx),
            6 => self.section_keymap(cx),
            7 => self.section_advanced(cx),
            _ => self.pending_group(
                "备份",
                "加密备份/远程同步（文件夹/WebDAV/S3/SFTP）编辑器待迁移。\
                 当前请在旧壳设置页操作。",
                cx,
            ),
        }
        .into_any_element()
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::IntoElement as _;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = theme.list_hover;

        v_flex()
            .w(px(150.0))
            .h_full()
            .flex_shrink_0()
            .p_2()
            .gap_1()
            .border_r_1()
            .border_color(theme.border)
            .children(SECTIONS.iter().enumerate().map(|(ix, label)| {
                let active = ix == self.active_section;
                div()
                    .id(("settings-nav", ix))
                    .px_3()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |item| item.bg(active_bg).text_color(active_fg))
                    .when(!active, |item| item.text_color(muted).hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_section = ix;
                        cx.notify();
                    }))
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
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .flex()
            .flex_row()
            .child(nav)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_6()
                    .gap_4()
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
