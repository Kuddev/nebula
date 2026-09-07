use std::fs::OpenOptions;
use std::io::Write;
use std::{io, panic, ptr, thread};

use windows_sys::Win32::System::Console::GetConsoleCP;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MessageBoxW,
};

use nebula_terminal::tty::windows::win32_string;

/// Startup failures precede logger/panic initialization and must not open the data directory.
pub fn report_startup_error(error: &dyn std::fmt::Display, gui_launch: bool) {
    let has_console = unsafe { GetConsoleCP() } != 0;
    if !startup_error_needs_dialog(gui_launch, has_console) || cfg!(test) {
        return;
    }
    let message = win32_string(&error.to_string());
    let title = win32_string(crate::brand::NAME);
    // Startup is still single-threaded and has not entered a GUI event loop.
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_OK | MB_SETFOREGROUND | MB_TASKMODAL,
        );
    }
}

fn startup_error_needs_dialog(gui_launch: bool, has_console: bool) -> bool {
    gui_launch && !has_console
}

// Install a panic handler that renders the panic in a classical Windows error
// dialog box as well as writes the panic to STDERR and a persistent log.
pub fn attach_handler() {
    let panic_log = crate::platform::dirs::data_dir().join("pebrel-panic.log");
    panic::set_hook(Box::new(move |panic_info| {
        let message = panic_info.to_string();
        let _ = writeln!(io::stderr(), "{message}");
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&panic_log) {
            let _ = writeln!(file, "{message}\n");
        }

        let dialog_message = win32_string(&format!("{message}\n\nPress Ctrl-C to Copy"));
        let dialog_title = win32_string("Pebrel: Runtime Error");

        // MessageBox 的模态消息泵不能运行在发生 panic 的 GPUI 线程上，否则
        // 会重入 AppCell 并把原始 panic 升级成无诊断信息的 double panic。
        // 等待专用线程结束可保证主线程 unwind 退出前用户仍能看到并复制错误。
        if let Ok(dialog_thread) =
            thread::Builder::new().name("panic-dialog".into()).spawn(move || unsafe {
                MessageBoxW(
                    ptr::null_mut(),
                    dialog_message.as_ptr(),
                    dialog_title.as_ptr(),
                    MB_ICONERROR | MB_OK | MB_SETFOREGROUND | MB_TASKMODAL,
                );
            })
        {
            let _ = dialog_thread.join();
        }
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_gui_startup_without_a_console_uses_an_error_dialog() {
        assert!(super::startup_error_needs_dialog(true, false));
        assert!(!super::startup_error_needs_dialog(true, true));
        assert!(!super::startup_error_needs_dialog(false, false));
        assert!(!super::startup_error_needs_dialog(false, true));
    }
}
