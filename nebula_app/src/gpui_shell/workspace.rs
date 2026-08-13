//! Nebula 主工作区：左侧垂直 Tab 侧边栏 + 主内容区（终端 / 设置页）。
//!
//! 布局对齐 nebula_app 的产品形态（TABS 侧边栏、每项图标 + 标题 + 关闭、
//! 激活高亮、主区圆角卡片）。设置页与旧壳同形态：一个单例特殊 tab。
//! 终端实例由本视图直接持有；会话清理走显式 `shutdown` +
//! `TerminalView::drop` 兜底。

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::gpui_shell::prelude::*;
use crate::gpui_shell::settings_pane::{SettingsPane, SettingsPaneEvent};
use crate::gpui_shell::terminal::view::{TerminalView, TerminalViewEvent};

gpui::actions!(
    nebula_workspace,
    [NewTerminal, CloseActiveTerminal, ToggleSidebar, OpenSettings]
);

/// 注册工作区快捷键；在 `gpui_component::init` 之后调用一次。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-w", CloseActiveTerminal, None),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
}

enum WorkspaceTab {
    Terminal { view: Entity<TerminalView>, _subscription: Subscription },
    Settings { view: Entity<SettingsPane>, _subscription: Subscription },
}

impl WorkspaceTab {
    fn is_settings(&self) -> bool {
        matches!(self, Self::Settings { .. })
    }
}

pub struct NebulaWorkspace {
    tabs: Vec<WorkspaceTab>,
    active: usize,
    sidebar_collapsed: bool,
}

impl NebulaWorkspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self { tabs: Vec::new(), active: 0, sidebar_collapsed: false };
        this.add_terminal(window, cx);
        this
    }

    fn add_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(TerminalView::new);
        let subscription = cx.subscribe_in(&view, window, Self::on_terminal_event);
        self.tabs.push(WorkspaceTab::Terminal { view, _subscription: subscription });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 设置页是单例 tab（旧壳同形态）：已开则激活，未开则新建。
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(WorkspaceTab::is_settings) {
            self.activate_tab(ix, window, cx);
            return;
        }
        let view = cx.new(|cx| SettingsPane::new(window, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_settings_event);
        self.tabs.push(WorkspaceTab::Settings { view, _subscription: subscription });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // 侧边栏标题渲染在本视图里，直接重绘。
            TerminalViewEvent::TitleChanged => cx.notify(),
            TerminalViewEvent::Exited => {
                let id = view.entity_id();
                let ix = self.tabs.iter().position(|tab| {
                    matches!(tab, WorkspaceTab::Terminal { view, .. } if view.entity_id() == id)
                });
                if let Some(ix) = ix {
                    self.close_tab(ix, window, cx);
                }
            },
        }
    }

    /// 设置改动：全局 `Settings` 已由设置页重载，这里热应用到所有终端
    /// 并联动窗口 chrome 主题深浅。
    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsPane>,
        event: &SettingsPaneEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsPaneEvent::Changed => {
                for tab in &self.tabs {
                    if let WorkspaceTab::Terminal { view, .. } = tab {
                        view.update(cx, |view, cx| view.apply_settings(cx));
                    }
                }
                crate::gpui_shell::theme::apply_chrome_theme(cx);
                cx.notify();
            },
        }
    }

    /// 终端应用惯例：最后一个 Tab 关闭即退出应用。
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        // 立即回收会话；实体引用清零后 TerminalView::drop 再兜底。
        if let WorkspaceTab::Terminal { view, .. } = &tab {
            view.read(cx).shutdown();
        }

        if self.tabs.is_empty() {
            cx.quit();
            return;
        }
        if ix < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active, window, cx);
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = match self.tabs.get(self.active) {
            Some(WorkspaceTab::Terminal { view, .. }) => view.read(cx).focus_handle.clone(),
            Some(WorkspaceTab::Settings { view, .. }) => view.read(cx).focus_handle(cx),
            None => return,
        };
        window.defer(cx, move |window, _| window.focus(&focus));
    }

    fn tab_title(&self, ix: usize, cx: &App) -> SharedString {
        const MAX_CHARS: usize = 20;
        match &self.tabs[ix] {
            WorkspaceTab::Settings { .. } => "设置".into(),
            WorkspaceTab::Terminal { view, .. } => {
                let title = &view.read(cx).title;
                let mut chars = title.chars();
                let head: String = chars.by_ref().take(MAX_CHARS).collect();
                if chars.next().is_some() { format!("{head}…").into() } else { head.into() }
            },
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let sidebar_bg = theme.sidebar;
        let sidebar_border = theme.sidebar_border;
        let muted = theme.muted_foreground;
        let active_bg = theme.sidebar_accent;
        let active_fg = theme.sidebar_accent_foreground;
        let hover_bg = theme.list_hover;

        let items = (0..self.tabs.len()).map(|ix| {
            let active = ix == self.active;
            let title = self.tab_title(ix, cx);
            let icon = if self.tabs[ix].is_settings() {
                IconName::Settings
            } else {
                IconName::SquareTerminal
            };
            h_flex()
                .id(("sidebar-tab", ix))
                .gap_2()
                .px_2()
                .h(px(30.0))
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .when(active, |item| item.bg(active_bg).text_color(active_fg))
                .when(!active, |item| {
                    item.text_color(muted).hover(|style| style.bg(hover_bg))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_tab(ix, window, cx);
                }))
                .child(Icon::new(icon).small().text_color(if active { active_fg } else { muted }))
                .child(div().flex_1().min_w_0().text_sm().truncate().child(title))
                .child(
                    Button::new(("close-tab", ix))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab(ix, window, cx);
                        })),
                )
        });

        v_flex()
            .w(px(210.0))
            .h_full()
            .flex_shrink_0()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(sidebar_border)
            .p_2()
            .gap_1()
            .child(
                h_flex()
                    .px_2()
                    .pb_1()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("TABS {}", self.tabs.len())),
                    )
                    .child(
                        Button::new("sidebar-new-tab")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .tooltip("新建终端 (Ctrl+Shift+T)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_terminal(window, cx);
                            })),
                    ),
            )
            .child(v_flex().flex_1().gap_1().children(items))
    }
}

impl Render for NebulaWorkspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme_border = cx.theme().border;
        let collapsed = self.sidebar_collapsed;
        let content: Option<gpui::AnyElement> = match self.tabs.get(self.active) {
            Some(WorkspaceTab::Terminal { view, .. }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            Some(WorkspaceTab::Settings { view, .. }) => {
                Some(gpui::IntoElement::into_any_element(view.clone()))
            },
            None => None,
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.add_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseActiveTerminal, window, cx| {
                this.close_active(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .child(
                TitleBar::new()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .occlude()
                            .child(
                                Button::new("toggle-sidebar")
                                    .icon(if collapsed {
                                        IconName::PanelLeftOpen
                                    } else {
                                        IconName::PanelLeftClose
                                    })
                                    .ghost()
                                    .xsmall()
                                    .tooltip("折叠/展开侧边栏 (Ctrl+Shift+B)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = !this.sidebar_collapsed;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Nebula"),
                            ),
                    )
                    .child(
                        h_flex().items_center().occlude().child(
                            Button::new("open-settings")
                                .icon(IconName::Settings)
                                .ghost()
                                .xsmall()
                                .tooltip("设置 (Ctrl+,)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_settings(window, cx);
                                })),
                        ),
                    ),
            )
            .child(
                // 不用 h_flex：它默认 items_center，会把子项高度压成内容高度。
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .when(!collapsed, |root| root.child(self.render_sidebar(cx)))
                    .child(
                        div().flex_1().min_w_0().p_2().child(
                            div()
                                .size_full()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme_border)
                                .overflow_hidden()
                                .children(content),
                        ),
                    ),
            )
    }
}
