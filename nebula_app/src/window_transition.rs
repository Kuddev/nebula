//! Native window transition stages used to coordinate monitor/DPI changes.

/// Low-frequency stages emitted by the native window system while a window is
/// being moved or resized interactively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWindowStage {
    EnterSizeMove,
    ExitSizeMove,
}

/// A stage event associated with one native window handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWindowStageEvent {
    pub hwnd: usize,
    pub stage: NativeWindowStage,
}

#[cfg(windows)]
mod platform {
    use super::{NativeWindowStage, NativeWindowStageEvent};
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    };

    struct Shared {
        events: Mutex<VecDeque<NativeWindowStageEvent>>,
        pending: AtomicBool,
    }

    // The WinEvent callback is a plain `extern "system"` function without a
    // context pointer, so the queue it feeds has to be reachable globally.
    // Exactly one tracker exists per process (created in `main`).
    static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

    // `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE` are *sent* to the window procedure
    // and never appear in the posted-message queue, so winit's `with_msg_hook`
    // (which only sees `PeekMessageW` results) cannot observe them. The
    // corresponding WinEvents *are* delivered to this callback while the
    // native modal move/size loop pumps messages, which is exactly the window
    // of time we need to cover.
    unsafe extern "system" fn move_size_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        id_child: i32,
        _id_event_thread: u32,
        _time_ms: u32,
    ) {
        // The same event codes are reused for UI-element notifications; only
        // top-level window transitions carry OBJID_WINDOW/CHILDID_SELF.
        if id_object != OBJID_WINDOW || id_child != 0 {
            return;
        }

        let stage = match event {
            EVENT_SYSTEM_MOVESIZESTART => NativeWindowStage::EnterSizeMove,
            EVENT_SYSTEM_MOVESIZEEND => NativeWindowStage::ExitSizeMove,
            _ => return,
        };

        let hwnd = hwnd as usize;
        if hwnd == 0 {
            return;
        }

        if let Some(shared) = SHARED.get() {
            shared.events.lock().push_back(NativeWindowStageEvent { hwnd, stage });
            shared.pending.store(true, Ordering::Release);
        }
    }

    /// Observes only the two native move/size transition markers through a
    /// process-scoped WinEvent hook. Ordinary Win32 messages are untouched;
    /// the drain fast-path is a single atomic load.
    #[derive(Clone)]
    pub struct NativeWindowStageTracker {
        shared: Arc<Shared>,
    }

    impl NativeWindowStageTracker {
        /// Must be called on the winit event-loop thread: out-of-context
        /// WinEvent callbacks are delivered on the installing thread's message
        /// pump. The hook lives for the process lifetime; Windows releases it
        /// on exit.
        pub fn new() -> Self {
            let shared = SHARED
                .get_or_init(|| {
                    Arc::new(Shared {
                        events: Mutex::new(VecDeque::with_capacity(4)),
                        pending: AtomicBool::new(false),
                    })
                })
                .clone();

            static HOOK: OnceLock<usize> = OnceLock::new();
            HOOK.get_or_init(|| {
                let hook = unsafe {
                    SetWinEventHook(
                        EVENT_SYSTEM_MOVESIZESTART,
                        EVENT_SYSTEM_MOVESIZEEND,
                        std::ptr::null_mut(),
                        Some(move_size_event_proc),
                        GetCurrentProcessId(),
                        GetCurrentThreadId(),
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                if hook.is_null() {
                    // Non-fatal: DPI/size changes then apply immediately
                    // (pre-coalescing behavior) instead of at drag end.
                    log::warn!(
                        "SetWinEventHook(MOVESIZESTART/END) failed; \
                         cross-monitor drag updates will not be coalesced"
                    );
                }
                hook as usize
            });

            Self { shared }
        }

        #[inline]
        pub fn drain(&self, mut apply: impl FnMut(NativeWindowStageEvent)) {
            if !self.shared.pending.swap(false, Ordering::Acquire) {
                return;
            }

            // Pop under a short lock and apply outside it: `apply` reaches
            // display/window state and must not run while holding the queue
            // lock the WinEvent callback pushes into.
            loop {
                let event = self.shared.events.lock().pop_front();
                match event {
                    Some(event) => apply(event),
                    None => break,
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::NativeWindowStageEvent;

    /// Cross-platform no-op keeps the application event path free of Windows
    /// types while preserving one shared Processor interface.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct NativeWindowStageTracker;

    impl NativeWindowStageTracker {
        pub fn new() -> Self {
            Self
        }

        pub fn drain(&self, _apply: impl FnMut(NativeWindowStageEvent)) {}
    }
}

pub use platform::NativeWindowStageTracker;
