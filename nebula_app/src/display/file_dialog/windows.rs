use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
    OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use winit::raw_window_handle::RawWindowHandle;

use super::{FileFilter, Window, windows_folder};

pub(super) fn pick_file(owner: &Window, title: &str, filters: &[FileFilter]) -> Option<PathBuf> {
    pick_files_inner(owner, title, filters, false).into_iter().next()
}

pub(super) fn pick_files(owner: &Window, title: &str, filters: &[FileFilter]) -> Vec<PathBuf> {
    pick_files_inner(owner, title, filters, true)
}

pub(super) fn pick_folder(owner: &Window, title: &str) -> Option<PathBuf> {
    windows_folder::pick(owner.raw_window_handle(), title)
}

fn pick_files_inner(
    owner: &Window,
    title: &str,
    filters: &[FileFilter],
    multiple: bool,
) -> Vec<PathBuf> {
    let hwnd: HWND = match owner.raw_window_handle() {
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    };
    let filter = build_filter_buffer(filters);
    let title = title.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let mut file_buffer = vec![0u16; 32768];
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = hwnd;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrFile = file_buffer.as_mut_ptr();
    dialog.nMaxFile = file_buffer.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_FILEMUSTEXIST
        | OFN_PATHMUSTEXIST
        | OFN_NOCHANGEDIR
        | OFN_HIDEREADONLY
        | if multiple { OFN_ALLOWMULTISELECT } else { 0 };

    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return Vec::new();
    }
    parse_open_file_buffer(&file_buffer)
}

fn build_filter_buffer(filters: &[FileFilter]) -> Vec<u16> {
    let mut buffer = Vec::new();
    for filter in filters {
        let patterns = filter.patterns.join(";");
        let label = format!("{} ({patterns})", filter.name);
        buffer.extend(label.encode_utf16());
        buffer.push(0);
        buffer.extend(patterns.encode_utf16());
        buffer.push(0);
    }
    buffer.push(0);
    buffer
}

fn parse_open_file_buffer(buffer: &[u16]) -> Vec<PathBuf> {
    let parts = buffer
        .split(|value| *value == 0)
        .take_while(|part| !part.is_empty())
        .map(OsString::from_wide)
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [] => Vec::new(),
        [path] => vec![PathBuf::from(path)],
        [directory, names @ ..] => {
            let directory = PathBuf::from(directory);
            names.iter().map(|name| directory.join(name)).collect()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::parse_open_file_buffer;

    fn wide_parts(parts: &[&str]) -> Vec<u16> {
        let mut buffer = Vec::new();
        for part in parts {
            buffer.extend(part.encode_utf16());
            buffer.push(0);
        }
        buffer.push(0);
        buffer
    }

    #[test]
    fn multi_select_buffer_joins_names_to_the_selected_directory() {
        let buffer = wide_parts(&[r"D:\上传 文件", "alpha.txt", "测试.zip"]);
        assert_eq!(
            parse_open_file_buffer(&buffer),
            vec![PathBuf::from(r"D:\上传 文件\alpha.txt"), PathBuf::from(r"D:\上传 文件\测试.zip"),]
        );
    }

    #[test]
    fn single_select_buffer_keeps_the_absolute_path() {
        let buffer = wide_parts(&[r"D:\release\nebula.zip"]);
        assert_eq!(parse_open_file_buffer(&buffer), vec![PathBuf::from(r"D:\release\nebula.zip")]);
    }
}
