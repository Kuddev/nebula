//! Platform file operations shared by the legacy file tree and GPUI workspace.

#[cfg(windows)]
pub(crate) fn send_to_recycle_bin(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };

    if !path.exists() {
        return Err("\u{8def}\u{5f84}\u{5df2}\u{4e0d}\u{5b58}\u{5728}".to_owned());
    }
    let mut source: Vec<u16> = path.as_os_str().encode_wide().collect();
    source.push(0);
    source.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE as u32,
        pFrom: source.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code == 0 && operation.fAnyOperationsAborted == 0 {
        Ok(())
    } else {
        Err(format!("\u{7cfb}\u{7edf}\u{62d2}\u{7edd}\u{ff08}\u{4ee3}\u{7801} {code}\u{ff09}"))
    }
}

#[cfg(not(windows))]
pub(crate) fn send_to_recycle_bin(path: &std::path::Path) -> Result<(), String> {
    let result =
        if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    result.map_err(|error| error.to_string())
}
