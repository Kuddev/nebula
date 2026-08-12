//! Nebula 主工作区：自绘 TitleBar + 多 Tab 终端。
//!
//! 布局约束：中心是单个根级 TabPanel（`stack_panel` 为空 → Tab 不可拖出分屏），
//! 因此 `TerminalPanel::on_removed` 只会由真实关闭触发。引入分屏布局时，
//! 会话清理需要改为由 workspace 的显式关闭路径驱动。

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, Styled as _, Subscription, Window, div,
};

use crate::terminal::panel::{TerminalPanel, TerminalPanelEvent};
use crate::ui::prelude::*;

gpui::actions!(nebula_workspace, [NewTerminal, CloseActiveTerminal]);

/// 注册工作区快捷键；在 `gpui_component::init` 之后调用一次。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-w", CloseActiveTerminal, None),
    ]);
}

pub struct NebulaWorkspace {
    dock_area: Entity<DockArea>,
    tab_panel: Entity<TabPanel>,
    _subscriptions: Vec<Subscription>,
}

impl NebulaWorkspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("nebula-workspace", Some(1), window, cx));
        let weak_dock = dock_area.downgrade();

        let panel = cx.new(|cx| TerminalPanel::new(window, cx));
        let tabs = DockItem::tabs(vec![Arc::new(panel.clone())], &weak_dock, window, cx);
        let tab_panel = match &tabs {
            DockItem::Tabs { view, .. } => view.clone(),
            _ => unreachable!("DockItem::tabs 恒定返回 Tabs 变体"),
        };
        dock_area.update(cx, |dock, cx| dock.set_center(tabs, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&panel, window, Self::on_terminal_event),
            cx.subscribe_in(&tab_panel, window, Self::on_tab_layout_event),
        ];

        // 首个 Tab 的 active_ix 保持 0 不变，TabPanel 不会触发激活回调，
        // 手动把初始输入焦点交给终端。
        let focus = panel.read(cx).focus_handle(cx);
        window.defer(cx, move |window, _| window.focus(&focus));

        Self { dock_area, tab_panel, _subscriptions: subscriptions }
    }

    fn add_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| TerminalPanel::new(window, cx));
        self._subscriptions.push(cx.subscribe_in(&panel, window, Self::on_terminal_event));
        // add_panel 会激活新 Tab 并把焦点交给面板（即终端）。
        self.tab_panel.update(cx, |tabs, cx| tabs.add_panel(Arc::new(panel), window, cx));
    }

    fn close_active_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.tab_panel.read(cx).active_panel(cx) else { return };
        self.tab_panel.update(cx, |tabs, cx| tabs.remove_panel(panel, window, cx));
        self.quit_if_empty(cx);
    }

    /// 终端应用惯例：最后一个 Tab 关闭即退出应用。
    fn quit_if_empty(&self, cx: &mut Context<Self>) {
        if self.tab_panel.read(cx).active_panel(cx).is_none() {
            cx.quit();
        }
    }

    fn on_terminal_event(
        &mut self,
        panel: &Entity<TerminalPanel>,
        event: &TerminalPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalPanelEvent::TitleChanged => {
                // Tab 标题渲染在 TabPanel 的渲染树里，面板自身的 notify
                // 不会让 TabPanel 重绘，需要在这里转发。
                self.tab_panel.update(cx, |_, cx| cx.notify());
            },
            TerminalPanelEvent::Exited | TerminalPanelEvent::CloseRequested => {
                let panel = panel.clone();
                self.tab_panel.update(cx, |tabs, cx| {
                    tabs.remove_panel(Arc::new(panel), window, cx);
                });
                self.quit_if_empty(cx);
            },
        }
    }

    fn on_tab_layout_event(
        &mut self,
        _: &Entity<TabPanel>,
        event: &PanelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 覆盖用户直接点 Tab 关闭按钮的路径（不经过 workspace）。
        if matches!(event, PanelEvent::LayoutChanged) {
            self.quit_if_empty(cx);
        }
    }
}

impl Render for NebulaWorkspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                this.close_active_terminal(window, cx);
            }))
            .child(
                TitleBar::new()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Nebula"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("GPUI"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .pr_2()
                            // 阻止标题栏拖拽区吃掉按钮点击。
                            .occlude()
                            .child(
                                Button::new("new-terminal")
                                    .icon(IconName::Plus)
                                    .ghost()
                                    .small()
                                    .tooltip("新建终端 (Ctrl+Shift+T)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_terminal(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("open-gallery")
                                    .icon(IconName::LayoutDashboard)
                                    .ghost()
                                    .small()
                                    .tooltip("组件验收页")
                                    .on_click(|_, _, cx| {
                                        crate::app::open_gallery_window(cx);
                                    }),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.dock_area.clone()))
    }
}
