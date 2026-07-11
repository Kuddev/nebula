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

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use crate::display::window::Window;
use crate::event::{Event, EventType};

/// Event-loop proxy for toast click handlers. A click lands on a WinRT
/// threadpool thread, which can only talk to the app through user events.
/// Set once at boot, before the first toast can exist.
static PROXY: OnceLock<EventLoopProxy<Event>> = OnceLock::new();

/// Install the proxy used by toast activation (click-to-focus).
pub fn init_proxy(proxy: EventLoopProxy<Event>) {
    let _ = PROXY.set(proxy);
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
    fn toast_text(&self) -> (String, String) {
        match self {
            Self::Bell { program } => match program {
                Some(p) => (p.clone(), "任务完成，等待输入".to_owned()),
                None => ("Nebula".to_owned(), "终端响铃".to_owned()),
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
                    None => ("Nebula".to_owned(), format!("命令完成，用时 {human}")),
                }
            },
            Self::Text { body, program } => match program {
                Some(p) => (p.clone(), body.clone()),
                None => ("Nebula".to_owned(), body.clone()),
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
}

/// Commands shorter than this never notify: quick `ls`-style commands would
/// otherwise flash the taskbar all day.
pub const COMMAND_NOTIFY_MIN: Duration = Duration::from_secs(10);

/// Minimum spacing between system toasts. Anything inside the window still
/// flashes the taskbar (cheap, silent, coalesced by the shell) but skips the
/// toast, so a build script ringing BEL in a loop cannot flood Action Center.
const TOAST_THROTTLE: Duration = Duration::from_secs(3);

/// Deliver `notification` for a window that is currently unfocused.
///
/// Policy lives at the call sites: they only call this when the window is
/// NOT focused (a focused user already sees the pane; the visual bell covers
/// that case). Delivery is taskbar attention + a real system toast (which
/// carries its own sound), the native Windows notification-center channel.
///
/// `pane` names the pane the event came from, when known: clicking the toast
/// then focuses the window AND surfaces that pane's tab (mac-style).
pub fn deliver(window: &Window, notification: &Notification, pane: Option<u64>) {
    // Taskbar flash / attention request (winit wraps FlashWindowEx). Always
    // fires: it is idempotent, silent, and the shell coalesces repeats.
    window.set_urgent(true);

    if throttled() {
        log::debug!("notify: toast suppressed by throttle: {notification:?}");
        return;
    }

    let (title, body) = notification.toast_text();
    let focus = (window.id(), pane);
    log::debug!("notify: toast '{title}': '{body}'");
    // Fire-and-forget worker: the WinRT show() is a cross-process RPC (can
    // take tens of ms — an eternity for the event loop), and notifications
    // must never be able to take the terminal down with them.
    if let Err(err) = std::thread::Builder::new()
        .name("nebula-toast".into())
        .spawn(move || toast_clickable(&title, &body, Some(focus)))
    {
        // Taskbar flash already fired, so the user is not left with nothing.
        log::warn!("notify: failed to spawn toast thread: {err}");
    }
}

/// Global toast rate limit. Returns true when this one should be dropped.
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
/// nothing). Runs on the toast worker thread, never on the event loop.
#[cfg(windows)]
pub(crate) fn toast(title: &str, body: &str) {
    toast_clickable(title, body, None);
}

/// [`toast`], optionally wired for click-to-focus: activating the banner (or
/// its Action Center entry) surfaces `window` and, when a pane is named, its
/// tab. Uses the in-process WinRT Activated handler — no COM server, no
/// protocol registration. The one trade-off: clicks after Nebula exited do
/// nothing, which is exactly right (there is nothing left to focus).
#[cfg(windows)]
fn toast_clickable(title: &str, body: &str, focus: Option<(WindowId, Option<u64>)>) {
    use tauri_winrt_notification::{IconCrop, Toast};

    // Attribute the toast to the Nebula AUMID so it reads "Nebula" instead of
    // "Windows PowerShell". One registry write, cached per process.
    win::ensure_aumid();

    let mut toast = Toast::new(win::AUMID)
        .title(title)
        .text1(body)
        .duration(tauri_winrt_notification::Duration::Short);
    // Belt and braces: besides the AUMID IconUri (which some Windows builds
    // cache stale), embed the logo per-toast as appLogoOverride so the banner
    // always carries the Nebula mark next to the message.
    if let Some(icon) = win::icon_path() {
        toast = toast.icon(&icon, IconCrop::Square, "Nebula");
    }
    if let Some((window, pane)) = focus {
        if let Some(proxy) = PROXY.get() {
            let proxy = proxy.clone();
            toast = toast.on_activated(move |_action| {
                let _ = proxy.send_event(Event::new(EventType::FocusWindow { pane }, window));
                Ok(())
            });
        }
    }

    match toast.show() {
        Ok(()) => log::debug!("notify: toast shown"),
        Err(err) => log::warn!("notify: toast failed: {err}"),
    }
}

#[cfg(not(windows))]
pub(crate) fn toast(_title: &str, _body: &str) {}

#[cfg(not(windows))]
fn toast_clickable(_title: &str, _body: &str, _focus: Option<(WindowId, Option<u64>)>) {}

/// `nebula notify-test` entrypoint: run the full toast pipeline synchronously
/// (registration + show), printing per-step diagnostics to the console. Skips
/// the focus policy and throttle on purpose — the tester is looking at the
/// screen. Returns a process exit code.
#[cfg(windows)]
pub fn notify_test() -> i32 {
    println!("[1/2] Registering AUMID '{}' ...", win::AUMID);
    match win::register_aumid() {
        Ok(()) => {
            println!(r"      OK  (HKCU\Software\Classes\AppUserModelId\{})", win::AUMID);
            match win::icon_path() {
                Some(path) => println!("      icon: {}", path.display()),
                None => println!("      icon: unavailable (banner will have no logo)"),
            }
        },
        Err(err) => {
            eprintln!("      FAILED: {err}");
            return 1;
        },
    }

    println!("[2/2] Showing toast ...");
    let mut toast = tauri_winrt_notification::Toast::new(win::AUMID)
        .title("Nebula")
        .text1("通知链路正常：nebula notify-test")
        .duration(tauri_winrt_notification::Duration::Short);
    if let Some(icon) = win::icon_path() {
        toast = toast.icon(&icon, tauri_winrt_notification::IconCrop::Square, "Nebula");
    }
    match toast.show() {
        Ok(()) => {
            println!("      OK  — a toast should be on screen now.");
            println!();
            println!("If nothing appeared, check Windows Settings > System > Notifications:");
            println!("the global toggle, Do Not Disturb / Focus Assist, and the Nebula entry.");
            0
        },
        Err(err) => {
            eprintln!("      FAILED: {err}");
            1
        },
    }
}

#[cfg(not(windows))]
pub fn notify_test() -> i32 {
    println!("notify-test: system toasts are only implemented on Windows.");
    0
}

/// Windows-only: the Nebula AppUserModelID and its registration.
///
/// A WinRT toast must be attributed to an AUMID that Windows can resolve to
/// an app identity, or it silently refuses to show. For an unpackaged app the
/// documented lightweight route is a registry key —
/// `HKCU\Software\Classes\AppUserModelId\<AUMID>` with a `DisplayName` value
/// (what Microsoft's own ToastNotificationManagerCompat writes). Per-user, no
/// admin rights, no COM, no Start-menu shortcut, and idempotent: rewriting
/// the same value is a cheap no-op, so a broken key self-heals on next run.
#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};

    /// AppUserModelID for Nebula. Toast notifications fire under this identity
    /// so the system shows "Nebula" instead of "PowerShell" / "cmd.exe".
    pub const AUMID: &str = "com.nebula.terminal";

    /// Embedded toast icon. `IconUri` must point at a real file on disk, so
    /// this is materialized to the Nebula data dir on first registration —
    /// works for a portable exe with no installer. (Some terminals solve this
    /// problem with an installer-created Start-menu shortcut; we don't have
    /// an installer to lean on.)
    const ICON_PNG: &[u8] = include_bytes!("../../extra/logo/nebula.png");

    /// Ensure the AUMID is registered. Best-effort, cached per process: the
    /// write itself is a few syscalls, there is just no point repeating them
    /// for every toast.
    pub fn ensure_aumid() {
        static DONE: OnceLock<()> = OnceLock::new();
        DONE.get_or_init(|| {
            if let Err(err) = register_aumid() {
                log::warn!("notify: AUMID registration failed (toast may not appear): {err}");
            }
        });
    }

    /// Write `DisplayName` + `IconUri` under the AUMID key. `RegSetKeyValueW`
    /// creates the missing subkey chain itself. The icon is best-effort: a
    /// failed write only costs the logo, never the toast.
    pub fn register_aumid() -> Result<(), String> {
        let subkey = format!(r"Software\Classes\AppUserModelId\{AUMID}");
        set_reg_sz(&subkey, "DisplayName", "Nebula")?;
        match ensure_icon_file() {
            Some(icon) => set_reg_sz(&subkey, "IconUri", &icon.display().to_string())?,
            None => log::debug!("notify: toast icon not materialized; banner shows no logo"),
        }
        Ok(())
    }

    /// The materialized icon path, for diagnostics (`nebula notify-test`).
    pub fn icon_path() -> Option<PathBuf> {
        ensure_icon_file()
    }

    /// Write the embedded logo to `%APPDATA%\Nebula\toast_icon.png` (idempotent;
    /// refreshed when the embedded bytes change size, e.g. after a logo swap).
    fn ensure_icon_file() -> Option<PathBuf> {
        let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("Nebula");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("toast_icon.png");
        let stale = std::fs::metadata(&path)
            .map(|meta| meta.len() != ICON_PNG.len() as u64)
            .unwrap_or(true);
        if stale {
            std::fs::write(&path, ICON_PNG).ok()?;
        }
        Some(path)
    }

    /// Set one REG_SZ value under HKCU\`subkey`, creating the key as needed.
    fn set_reg_sz(subkey: &str, name: &str, data: &str) -> Result<(), String> {
        let subkey_w = to_wide(subkey);
        let name_w = to_wide(name);
        let data_w = to_wide(data);
        let data_bytes = (data_w.len() * std::mem::size_of::<u16>()) as u32;

        // SAFETY: every pointer references a live, NUL-terminated UTF-16
        // buffer owned by this frame; RegSetKeyValueW copies the data before
        // returning and creates intermediate keys as needed.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                subkey_w.as_ptr(),
                name_w.as_ptr(),
                REG_SZ,
                data_w.as_ptr().cast(),
                data_bytes,
            )
        };

        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("RegSetKeyValueW({name}) failed with status {status}"))
        }
    }

    /// NUL-terminated UTF-16 for Win32 wide-string APIs.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
}
