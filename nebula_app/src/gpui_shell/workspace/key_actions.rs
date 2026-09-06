//! Workspace 快捷键动作实现（从 `workspace.rs` 拆出以守行数预算）。

use gpui::{Context, Window};

use super::{NebulaWorkspace, WorkspaceTab};

/// 标签左移/右移的落点。**不环绕**：首个标签再左移、末个再右移都是 no-op。
/// 环绕在这里是有害的——用户按住键连点想把标签推到最左，环绕会让它突然
/// 跳到最右端，而标签栏此时正好滚在另一头，人就找不着自己的标签了。
pub(super) fn move_target(active: usize, len: usize, right: bool) -> Option<usize> {
    if len < 2 || active >= len {
        return None;
    }
    if right { (active + 1 < len).then(|| active + 1) } else { active.checked_sub(1) }
}

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
        let ix = if self.settings_open {
            if next { 0 } else { len - 1 }
        } else if next {
            (self.active + 1) % len
        } else {
            (self.active + len - 1) % len
        };
        self.activate_tab(ix, window, cx);
    }

    /// Ctrl+Shift+PageUp/PageDown（WT 的 `moveTab forward/backward`）：把活动
    /// 标签在序列里挪一格。复用拖拽提交用的 [`Self::move_tab`]，两条路径共享
    /// 同一份下标搬运与 active 修正，不会各自解释一套。
    pub(super) fn move_active_tab(
        &mut self,
        right: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(to) = move_target(self.active, self.tabs.len(), right) else { return };
        let from = self.active;
        self.move_tab(from, to, window, cx);
        // 顶栏模式下标签可能刚被挪出可视窗口；侧栏模式同样按整行窗口对齐。
        self.reveal_active_tab();
        cx.notify();
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
            crate::gpui_shell::try_write_stderr(format_args!(
                "[nebula:gpui] failed to persist font size: {err}"
            ));
            return;
        }
        self.apply_runtime_settings(cx);
    }

    pub(super) fn copy_focused_terminal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.tabs
            .get(self.active)
            .and_then(WorkspaceTab::focused_view)
            .is_some_and(|view| view.update(cx, |view, cx| view.copy_selection(true, window, cx)))
    }

    pub(super) fn paste_focused_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.tabs.get(self.active).and_then(WorkspaceTab::focused_view) {
            view.update(cx, |view, cx| view.paste(window, cx));
        }
    }
}
