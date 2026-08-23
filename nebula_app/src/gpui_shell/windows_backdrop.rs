//! Windows system-backdrop prerequisites for GPUI top-level windows.
//!
//! GPUI renders through DirectComposition and creates every platform window with
//! `WS_EX_NOREDIRECTIONBITMAP`. That is correct for transparent tool windows, but a
//! top-level Mica window needs a DWM redirection surface. The style is consumed by
//! DWM during `CreateWindowExW`, so changing it after the HWND exists is too late.

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CBT_CREATEWNDW, CallNextHookEx, HCBT_CREATEWND, HHOOK, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_CBT, WS_EX_APPWINDOW, WS_EX_NOREDIRECTIONBITMAP,
};

/// Installs a same-thread CBT hook before GPUI creates its first HWND.
///
/// Only regular application windows are changed. GPUI menus, prompts and other
/// tool windows keep `WS_EX_NOREDIRECTIONBITMAP`, preserving their transparent
/// DirectComposition behavior.
pub(super) fn install_main_window_creation_hook() -> Option<MainWindowCreationHook> {
    // SAFETY: the callback is a process-local function and the hook is scoped to
    // the current UI thread, so no DLL module handle is required by Win32.
    let hook = unsafe {
        SetWindowsHookExW(
            WH_CBT,
            Some(main_window_creation_hook),
            std::ptr::null_mut(),
            GetCurrentThreadId(),
        )
    };
    if hook.is_null() {
        log::warn!(
            "failed to install the GPUI main-window creation hook; system Mica may be unavailable"
        );
        None
    } else {
        Some(MainWindowCreationHook(hook))
    }
}

pub(super) struct MainWindowCreationHook(HHOOK);

impl Drop for MainWindowCreationHook {
    fn drop(&mut self) {
        // SAFETY: the handle came from SetWindowsHookExW and is owned by this guard.
        unsafe {
            UnhookWindowsHookEx(self.0);
        }
    }
}

unsafe extern "system" fn main_window_creation_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && code as u32 == HCBT_CREATEWND && lparam != 0 {
        // SAFETY: for HCBT_CREATEWND, Win32 guarantees lParam points to a writable
        // CBT_CREATEWNDW for the window whose creation is still in progress.
        let create = unsafe { &mut *(lparam as *mut CBT_CREATEWNDW) };
        if !create.lpcs.is_null() {
            let create_struct = unsafe { &mut *create.lpcs };
            let is_main_window = create_struct.dwExStyle & WS_EX_APPWINDOW != 0;
            if is_main_window {
                create_struct.dwExStyle &= !WS_EX_NOREDIRECTIONBITMAP;
            }
        }
    }

    // SAFETY: every hook must forward events it does not consume. A null hook
    // handle is explicitly supported for CallNextHookEx.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}
