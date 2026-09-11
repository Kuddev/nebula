use gpui::{Context, Window};

use super::{NebulaWorkspace, WorkspaceTab};
use crate::notify::Notification;

fn source_is_visible(window_active: bool, source_active: bool, overlay_open: bool) -> bool {
    window_active && source_active && !overlay_open
}

#[derive(Debug, PartialEq, Eq)]
struct DeliveryChannels {
    in_app: bool,
    system: bool,
}

fn delivery_channels(
    notification: &Notification,
    visible: bool,
    ai_toasts: bool,
) -> DeliveryChannels {
    DeliveryChannels {
        in_app: (!visible || notification.is_attention()) && (ai_toasts || !notification.is_ai()),
        // Keep native notification routing independent of the in-app preference.
        system: !visible,
    }
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
        let delivery = delivery_channels(
            &notification,
            visible,
            crate::gpui_shell::config::ai_toasts_enabled(cx),
        );
        if !delivery.in_app && !delivery.system {
            return;
        }
        let source_view =
            panes.iter().find(|pane| pane.id == pane_id).map(|pane| pane.view.clone());
        if !visible && let Some(meta) = self.tab_meta.get_mut(tab_index) {
            meta.has_bell = true;
        }
        if delivery.in_app {
            let attention = notification.is_attention();
            let confirmation = if attention {
                source_view.and_then(|view| view.update(cx, |view, _| view.capture_confirmation()))
            } else {
                None
            };
            // Log the original message before the banner creates a bounded preview.
            let (title, body) = notification.raw_toast_text();
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
                crate::gpui_shell::toast::banner_for_pane(
                    window,
                    cx,
                    kind,
                    text,
                    pane_id,
                    notification.is_ai(),
                );
            }
        }
        if delivery.system {
            crate::notify::deliver_gpui(&notification, pane_id);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_in_app_ai_toasts_keeps_background_system_notifications() {
        for attention in [false, true] {
            let notification =
                Notification::AiTurn { program: "codex".into(), message: None, attention };
            assert_eq!(
                delivery_channels(&notification, false, false),
                DeliveryChannels { in_app: false, system: true }
            );
            assert_eq!(
                delivery_channels(&notification, false, true),
                DeliveryChannels { in_app: true, system: true }
            );
            assert_eq!(
                delivery_channels(&notification, true, false),
                DeliveryChannels { in_app: false, system: false }
            );
            assert_eq!(
                delivery_channels(&notification, true, true),
                DeliveryChannels { in_app: attention, system: false }
            );
        }
    }

    #[test]
    fn ai_bells_and_osc_messages_obey_only_the_in_app_switch() {
        for notification in [
            Notification::Bell { program: Some("claude".into()) },
            Notification::Text { program: Some("codex".into()), body: "done".into() },
            Notification::CommandDone {
                program: Some("gemini".into()),
                duration: std::time::Duration::from_secs(12),
            },
        ] {
            assert_eq!(
                delivery_channels(&notification, false, false),
                DeliveryChannels { in_app: false, system: true }
            );
        }
    }

    #[test]
    fn ordinary_terminal_notifications_ignore_the_ai_toast_preference() {
        for notification in [
            Notification::Bell { program: None },
            Notification::Text { program: Some("cargo".into()), body: "build finished".into() },
        ] {
            for visible in [false, true] {
                assert_eq!(
                    delivery_channels(&notification, visible, false),
                    delivery_channels(&notification, visible, true)
                );
            }
        }
    }

    #[test]
    fn only_the_visible_source_pane_suppresses_completion_notifications() {
        assert!(source_is_visible(true, true, false));
        assert!(!source_is_visible(false, true, false));
        assert!(!source_is_visible(true, false, false));
        assert!(!source_is_visible(true, true, true));
    }
}
