use gpui::{Context, Window};

use super::{NebulaWorkspace, WorkspaceTab};
use crate::notify::Notification;

fn source_is_visible(window_active: bool, source_active: bool, overlay_open: bool) -> bool {
    window_active && source_active && !overlay_open
}

impl NebulaWorkspace {
    pub(super) fn deliver_pane_notification(
        &mut self,
        pane_id: u64,
        notification: Notification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_index) = self.tab_of_pane(pane_id) else { return };
        let Some(WorkspaceTab::Terminal { panes, focused, .. }) = self.tabs.get(tab_index) else {
            return;
        };
        let reader_open = panes
            .iter()
            .find(|pane| pane.id == pane_id)
            .is_some_and(|pane| pane.view.read(cx).is_reading_answer());
        let visible = source_is_visible(
            window.is_window_active() && !self.window_hidden,
            tab_index == self.active && *focused == pane_id,
            self.settings_open || reader_open,
        );
        let attention = notification.is_attention();
        let source_view =
            panes.iter().find(|pane| pane.id == pane_id).map(|pane| pane.view.clone());
        let confirmation = if attention {
            source_view.and_then(|view| view.update(cx, |view, _| view.capture_confirmation()))
        } else {
            None
        };
        if visible && !attention {
            return;
        }
        if !visible && let Some(meta) = self.tab_meta.get_mut(tab_index) {
            meta.has_bell = true;
        }
        let (title, body) = notification.toast_text();
        let kind = if attention {
            crate::display::ToastKind::Warning
        } else {
            crate::display::ToastKind::Info
        };
        let text = format!("{title} \u{b7} {body}");
        if let Some(confirmation) = confirmation {
            crate::gpui_shell::toast::confirmation_for_pane(
                window,
                cx,
                text,
                pane_id,
                confirmation,
            );
        } else {
            crate::gpui_shell::toast::banner_for_pane(window, cx, kind, text, pane_id);
        }
        if !visible {
            crate::notify::deliver_gpui(&notification, pane_id);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_visible_source_pane_suppresses_completion_notifications() {
        assert!(source_is_visible(true, true, false));
        assert!(!source_is_visible(false, true, false));
        assert!(!source_is_visible(true, false, false));
        assert!(!source_is_visible(true, true, true));
    }
}
