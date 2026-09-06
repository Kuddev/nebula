//! Nebula's notification center.
//!
//! One funnel for everything that may deserve the user's attention —
//! terminal bells (Claude Code / Codex ring one when a turn finishes),
//! OSC 9 text notifications, long commands finishing (OSC 133;C/D) — with
//! one policy gate and pluggable delivery. Deliberately small and additive:
//! new sources should become a [`Notification`] variant, new outputs a line
//! in [`deliver`], so AI-CLI-specific hooks can land without rewiring.
//!
//! Delivery on Windows is a real WinRT toast (system tray / notification
//! center), on top of the taskbar flash. Unlike launchers that ask the user to
//! hand-edit hook scripts into each CLI's config — Nebula needs ZERO user
//! setup: AI CLIs already ring BEL when a turn ends, so the toast fires off
//! that signal out of the box. Toast identity comes from a "Nebula" AUMID
//! registered under `HKCU\Software\Classes\AppUserModelId` (the documented
//! registry route for unpackaged apps — no COM, no Start-menu shortcut, no
//! installer), so banners read "Nebula" instead of "Windows PowerShell".
//!
//! Delivery discipline: the toast RPC runs on a throwaway thread so a slow or
//! faulty notification stack can never stall the winit event loop — and a
//! panic there kills that thread, not the terminal. Notifications are
//! best-effort by contract: every failure degrades to a log line, never to a
//! crash. A small global throttle keeps a bell-happy background job from
//! flooding the Action Center.

#[cfg(any(feature = "gpui-shell", test))]
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(feature = "legacy-shell")]
use winit::event_loop::EventLoopProxy;

#[cfg(feature = "legacy-shell")]
use crate::display::window::Window;
#[cfg(feature = "legacy-shell")]
use crate::event::{Event, EventType};

/// Event-loop proxy for toast click handlers. A click lands on a WinRT
/// threadpool thread, which can only talk to the app through user events.
/// Set once at boot, before the first toast can exist.
#[cfg(feature = "legacy-shell")]
static PROXY: OnceLock<EventLoopProxy<Event>> = OnceLock::new();

/// Install the proxy used by toast activation (click-to-focus).
#[cfg(feature = "legacy-shell")]
pub fn init_proxy(proxy: EventLoopProxy<Event>) {
    let _ = PROXY.set(proxy);
}

pub use crate::platform::notifications::notify_test;
use crate::platform::notifications::{ToastActivation, toast_clickable};

#[cfg(feature = "gpui-shell")]
static GPUI_ACTIVATION: OnceLock<std::sync::mpsc::Sender<crate::gpui_shell::GpuiShellEvent>> =
    OnceLock::new();

#[cfg(feature = "gpui-shell")]
pub(crate) fn init_gpui_activation(
    sender: std::sync::mpsc::Sender<crate::gpui_shell::GpuiShellEvent>,
) {
    let _ = GPUI_ACTIVATION.set(sender);
}

#[cfg(feature = "gpui-shell")]
fn gpui_activation(
    sender: std::sync::mpsc::Sender<crate::gpui_shell::GpuiShellEvent>,
    pane_id: Option<u64>,
) -> ToastActivation {
    Arc::new(move || {
        let _ = sender.send(crate::gpui_shell::GpuiShellEvent::NotificationFocus(pane_id));
    })
}

fn application_activation(pane_id: Option<u64>) -> Option<ToastActivation> {
    #[cfg(feature = "gpui-shell")]
    {
        GPUI_ACTIVATION.get().map(|sender| gpui_activation(sender.clone(), pane_id))
    }
    #[cfg(not(feature = "gpui-shell"))]
    {
        let _ = pane_id;
        None
    }
}

/// Something that happened in a pane which may deserve attention.
#[derive(Debug, Clone)]
pub enum Notification {
    /// BEL from the shell/TUI. AI CLIs ring this when a turn completes, so
    /// it is the primary "claude/codex finished" signal. Carries the tracked
    /// program name (e.g. "claude", "codex") when one is running, so the toast
    /// can say who finished.
    Bell { program: Option<String> },
    /// A tracked command finished (OSC 133;C started it, 133;D ended it).
    CommandDone { duration: Duration, program: Option<String> },
    /// Free-text notification from a program (OSC 9, iTerm style). Claude
    /// Code emits these (with the turn's actual message) when its notif
    /// channel is `iterm2`/`iterm2_with_bell`. Carries the tracked program
    /// name so the toast is titled "claude" instead of "Nebula".
    Text { body: String, program: Option<String> },
    /// Typed AI-CLI turn event delivered through the `nebula-hook` pipe
    /// (claude hooks / codex notify — see `ai_hook`). `attention` means the
    /// CLI needs the user NOW (permission prompt / idle reminder) rather
    /// than "turn finished".
    AiTurn { program: String, message: Option<String>, attention: bool },
}

impl Notification {
    /// Toast title + body. Title names the source ("Nebula" or the program);
    /// body carries the human detail.
    pub(crate) fn toast_text(&self) -> (String, String) {
        match self {
            Self::Bell { program } => match program {
                Some(p) => (p.clone(), "任务完成，等待输入".to_owned()),
                None => (crate::brand::NAME.to_owned(), "终端响铃".to_owned()),
            },
            Self::CommandDone { duration, program } => {
                let secs = duration.as_secs();
                let human = if secs >= 60 {
                    format!("{}m {}s", secs / 60, secs % 60)
                } else {
                    format!("{secs}s")
                };
                match program {
                    Some(p) => (p.clone(), format!("命令完成，用时 {human}")),
                    None => (crate::brand::NAME.to_owned(), format!("命令完成，用时 {human}")),
                }
            },
            Self::Text { body, program } => match program {
                Some(p) => (p.clone(), body.clone()),
                None => (crate::brand::NAME.to_owned(), body.clone()),
            },
            Self::AiTurn { program, message, attention } => {
                let body = message.clone().unwrap_or_else(|| {
                    if *attention {
                        "需要你的确认或输入".to_owned()
                    } else {
                        "回合完成，等待下一条指令".to_owned()
                    }
                });
                (program.clone(), body)
            },
        }
    }

    pub(crate) fn is_attention(&self) -> bool {
        matches!(self, Self::AiTurn { attention: true, .. })
    }
}

/// Commands shorter than this never notify: quick `ls`-style commands would
/// otherwise flash the taskbar all day.
pub const COMMAND_NOTIFY_MIN: Duration = Duration::from_secs(10);

/// Minimum spacing between system toasts. Anything inside the window still
/// flashes the taskbar (cheap, silent, coalesced by the shell) but skips the
/// toast, so a build script ringing BEL in a loop cannot flood Action Center.
const TOAST_THROTTLE: Duration = Duration::from_secs(3);

#[cfg(any(feature = "gpui-shell", test))]
#[derive(Default)]
struct PaneNotificationThrottle {
    recent: HashMap<(u64, bool), Instant>,
}

#[cfg(any(feature = "gpui-shell", test))]
impl PaneNotificationThrottle {
    fn accepts(&mut self, pane_id: u64, attention: bool, now: Instant) -> bool {
        self.recent.retain(|_, last| now.saturating_duration_since(*last) < TOAST_THROTTLE);
        if self.recent.contains_key(&(pane_id, attention)) {
            return false;
        }
        self.recent.insert((pane_id, attention), now);
        true
    }
}

#[cfg(feature = "gpui-shell")]
pub(crate) fn deliver_gpui(notification: &Notification, pane_id: u64) {
    static THROTTLE: OnceLock<Mutex<PaneNotificationThrottle>> = OnceLock::new();
    let accepted = THROTTLE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .accepts(pane_id, notification.is_attention(), Instant::now());
    if !accepted {
        log::debug!("notify: toast suppressed for pane={pane_id}: {notification:?}");
        return;
    }
    let (title, body) = notification.toast_text();
    spawn_toast(title, body, application_activation(Some(pane_id)));
}

/// Deliver `notification` for a window that is currently unfocused.
///
/// Policy lives at the call sites: they only call this when the window is
/// NOT focused (a focused user already sees the pane; the visual bell covers
/// that case). Delivery is taskbar attention + a real system toast (which
/// carries its own sound), the native Windows notification-center channel.
///
/// `pane` names the pane the event came from, when known: clicking the toast
/// then focuses the window AND surfaces that pane's tab (mac-style).
#[cfg(feature = "legacy-shell")]
pub fn deliver(window: &Window, notification: &Notification, pane: Option<u64>) {
    // Taskbar flash / attention request (winit wraps FlashWindowEx). Always
    // fires: it is idempotent, silent, and the shell coalesces repeats.
    window.set_urgent(true);

    if throttled() {
        log::debug!("notify: toast suppressed by throttle: {notification:?}");
        return;
    }

    let (title, body) = notification.toast_text();
    let window_id = window.id();
    let activation = PROXY.get().map(|proxy| {
        let proxy = proxy.clone();
        Arc::new(move || {
            let _ = proxy.send_event(Event::new(EventType::FocusWindow { pane }, window_id));
        }) as ToastActivation
    });
    log::debug!("notify: toast '{title}': '{body}'");
    // Fire-and-forget worker: the WinRT show() is a cross-process RPC (can
    // take tens of ms — an eternity for the event loop), and notifications
    // must never be able to take the terminal down with them.
    spawn_toast(title, body, activation);
}

/// Global toast rate limit. Returns true when this one should be dropped.
#[cfg(feature = "legacy-shell")]
fn throttled() -> bool {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    // A poisoned lock only means some thread panicked mid-check; the state is
    // a plain Option, safe to keep using.
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match *last {
        Some(at) if at.elapsed() < TOAST_THROTTLE => true,
        _ => {
            *last = Some(Instant::now());
            false
        },
    }
}

/// Raise a native system toast. Best-effort: any failure is logged and
/// swallowed (the taskbar flash already fired, so the user is not left with
/// nothing). Native delivery runs on a worker thread, never on the event loop.
pub(crate) fn toast(title: &str, body: &str) {
    spawn_toast(title.to_owned(), body.to_owned(), application_activation(None));
}

fn spawn_toast(title: String, body: String, activation: Option<ToastActivation>) {
    if let Err(error) = std::thread::Builder::new()
        .name("nebula-toast".into())
        .spawn(move || toast_clickable(&title, &body, activation))
    {
        log::warn!("notify: failed to spawn toast thread: {error}");
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn pane_throttle_does_not_suppress_another_pane_or_attention() {
        let now = Instant::now();
        let mut throttle = PaneNotificationThrottle::default();
        assert!(throttle.accepts(1, false, now));
        assert!(throttle.accepts(2, false, now));
        assert!(!throttle.accepts(1, false, now));
        assert!(throttle.accepts(1, true, now));
        assert!(!throttle.accepts(1, true, now));
        assert!(throttle.accepts(2, true, now));
    }

    #[test]
    fn pane_throttle_expires_without_extending_on_repeats() {
        let now = Instant::now();
        let mut throttle = PaneNotificationThrottle::default();
        assert!(throttle.accepts(1, false, now));
        assert!(!throttle.accepts(1, false, now + Duration::from_secs(2)));
        assert!(throttle.accepts(1, false, now + TOAST_THROTTLE));
        assert_eq!(throttle.recent.len(), 1);
    }

    #[test]
    fn attention_importance_is_independent_of_completion_text() {
        let done =
            Notification::AiTurn { program: "codex".to_owned(), message: None, attention: false };
        let waiting = Notification::AiTurn {
            program: "claude".to_owned(),
            message: Some("Confirm command".to_owned()),
            attention: true,
        };
        assert!(!done.is_attention());
        assert!(waiting.is_attention());
        assert_eq!(waiting.toast_text(), ("claude".to_owned(), "Confirm command".to_owned()));
        assert!(!Notification::Bell { program: None }.is_attention());
        assert!(
            !Notification::Text { body: "permission".to_owned(), program: None }.is_attention()
        );
    }

    #[cfg(feature = "gpui-shell")]
    #[test]
    fn activation_preserves_exact_pane_and_generic_application_targets() {
        use crate::gpui_shell::GpuiShellEvent;

        let (sender, receiver) = std::sync::mpsc::channel();
        let pane_activation = gpui_activation(sender.clone(), Some(42));
        let generic_activation = gpui_activation(sender, None);
        pane_activation();
        generic_activation();
        assert!(matches!(receiver.try_recv(), Ok(GpuiShellEvent::NotificationFocus(Some(42)))));
        assert!(matches!(receiver.try_recv(), Ok(GpuiShellEvent::NotificationFocus(None))));
        drop(receiver);
        pane_activation();
    }
}
