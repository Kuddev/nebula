//! Workspace 快捷键动作实现（从 `workspace.rs` 拆出以守行数预算）。

use gpui::{Context, Window};

use super::{NebulaWorkspace, WorkspaceTab};

impl NebulaWorkspace {
    pub(super) fn select_adjacent_tab(
        &mut self,
        next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.tabs.len();
        if len == 0 {
            return;
        }
        let ix = if next { (self.active + 1) % len } else { (self.active + len - 1) % len };
        self.activate_tab(ix, window, cx);
    }

    /// 设置步进同源：逻辑 px ±1，钳 4–64；`delta == 0` 回到 toml 基准字号。
    pub(super) fn bump_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let current = settings.map(|s| s.font_size_px).unwrap_or(15.0);
        let next = if delta == 0.0 {
            settings.map(|s| s.base_font_size_px).unwrap_or(15.0)
        } else {
            (current.round() + delta).clamp(4.0, 64.0)
        };
        if (next - current).abs() < f32::EPSILON {
            return;
        }
        if let Err(err) = nebula_settings::persist_keys(&[("font_size", format!("{next:.2}"))]) {
            eprintln!("[nebula:gpui] failed to persist font size: {err}");
            return;
        }
        self.apply_runtime_settings(cx);
    }

    pub(super) fn copy_focused_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view) {
            view.update(cx, |view, cx| view.copy_selection(true, window, cx));
        }
    }

    pub(super) fn paste_focused_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view) {
            view.update(cx, |view, cx| view.paste(window, cx));
        }
    }
}
