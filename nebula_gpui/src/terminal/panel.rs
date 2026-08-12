//! 终端的 Dock 面板合同：把 `TerminalView` 包装成 `gpui_component::dock::Panel`。
//!
//! 面板只负责 Tab 语义（标题、关闭、焦点转发、生命周期），终端行为全部留在
//! `TerminalView`；workspace 通过 `TerminalPanelEvent` 得知标题变化与会话结束。

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{IconName, Sizable as _};

use super::view::{TerminalView, TerminalViewEvent};

/// 面板对 workspace 暴露的事件。
pub enum TerminalPanelEvent {
    /// Tab 标题需要刷新（宿主 TabPanel 不订阅面板重绘，需要 workspace 转发）。
    TitleChanged,
    /// 会话已结束，宿主应关闭本 Tab。
    Exited,
    /// 用户点击了面板的关闭按钮，宿主应关闭本 Tab。
    CloseRequested,
}

pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal = cx.new(TerminalView::new);
        let subscription = cx.subscribe_in(
            &terminal,
            window,
            |_, _, event: &TerminalViewEvent, _, cx| match event {
                TerminalViewEvent::TitleChanged => {
                    cx.emit(TerminalPanelEvent::TitleChanged);
                    cx.notify();
                },
                TerminalViewEvent::Exited => cx.emit(TerminalPanelEvent::Exited),
            },
        );
        Self { terminal, _subscriptions: vec![subscription] }
    }

    fn tab_title(&self, cx: &App) -> SharedString {
        // PowerShell 的 OSC 标题常是完整路径，按字符截断避免撑爆 Tab 栏。
        const MAX_CHARS: usize = 24;
        let title = &self.terminal.read(cx).title;
        let mut chars = title.chars();
        let head: String = chars.by_ref().take(MAX_CHARS).collect();
        if chars.next().is_some() { format!("{head}…").into() } else { head.into() }
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}
impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        // 转发终端的焦点句柄，TabPanel 切换 Tab 时输入焦点直达终端。
        self.terminal.read(cx).focus_handle.clone()
    }
}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "NebulaTerminalPanel"
    }

    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(self.tab_title(cx))
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_title(cx)
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        // 根级 TabPanel 被组件库判定为固定布局（closable 恒 false，不渲染
        // Tab 关闭按钮），所以由面板自己提供关闭入口，渲染在 Tab 栏右侧。
        Some(
            Button::new("close-terminal")
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip("关闭当前终端 (Ctrl+Shift+W)")
                .on_click(
                    cx.listener(|_, _, _, cx| cx.emit(TerminalPanelEvent::CloseRequested)),
                ),
        )
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        // 终端网格贴满面板，不要 Tab 内容区的默认内边距。
        false
    }

    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Tab 被移除时立即回收会话；实体引用清零后 TerminalView::drop 会再兜底。
        self.terminal.read(cx).shutdown();
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}
