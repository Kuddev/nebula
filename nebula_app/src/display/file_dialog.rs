use std::path::PathBuf;

use winit::raw_window_handle::RawWindowHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrivateKeyFileKind {
    PrivateKey,
    PublicKey,
    Unsupported,
}

pub(super) fn classify_private_key_contents(contents: &[u8]) -> PrivateKeyFileKind {
    let Ok(text) = std::str::from_utf8(contents) else {
        return PrivateKeyFileKind::Unsupported;
    };
    let trimmed = text.trim_start();
    if trimmed.starts_with("ssh-")
        || trimmed.starts_with("ecdsa-sha2-")
        || trimmed.starts_with("sk-ssh-")
        || trimmed.starts_with("sk-ecdsa-")
        || russh::keys::ssh_key::PublicKey::from_openssh(trimmed).is_ok()
    {
        return PrivateKeyFileKind::PublicKey;
    }
    if [
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "-----BEGIN ENCRYPTED PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----",
        "PuTTY-User-Key-File-",
    ]
    .iter()
    .any(|header| trimmed.starts_with(header))
    {
        PrivateKeyFileKind::PrivateKey
    } else {
        PrivateKeyFileKind::Unsupported
    }
}

pub(super) fn pick_image_file(owner: RawWindowHandle) -> Option<String> {
    pick_file(
        owner,
        "Images (*.png;*.jpg;*.jpeg;*.webp;*.bmp)\0*.png;*.jpg;*.jpeg;*.webp;*.bmp\0All files (*.*)\0*.*\0\0",
        "Choose background image",
    )
    .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn pick_private_key_file(owner: RawWindowHandle) -> Option<Result<PathBuf, String>> {
    let path = pick_file(
        owner,
        "SSH private keys (id_*;*.pem;*.key;*.ppk)\0id_*;*.pem;*.key;*.ppk\0All files (*.*)\0*.*\0\0",
        "Choose SSH private key",
    )?;
    let contents = match std::fs::read(&path) {
        Ok(contents) => contents,
        Err(err) => return Some(Err(format!("无法读取私钥 {}: {err}", path.display()))),
    };
    Some(match classify_private_key_contents(&contents) {
        PrivateKeyFileKind::PrivateKey => Ok(path),
        PrivateKeyFileKind::PublicKey => Err("请选择私钥文件，不要选择 .pub 公钥".to_owned()),
        PrivateKeyFileKind::Unsupported => {
            Err("文件不是受支持的 OpenSSH、PEM 或 PPK 私钥".to_owned())
        },
    })
}

pub(super) fn pick_upload_files(owner: RawWindowHandle) -> Vec<PathBuf> {
    pick_files(owner, "All files (*.*)\0*.*\0\0", "选择要上传的文件", true)
}

pub(super) fn pick_upload_directory(owner: RawWindowHandle) -> Option<PathBuf> {
    pick_folder(owner, "选择要上传的文件夹")
}

pub(super) fn pick_download_directory(owner: RawWindowHandle) -> Option<PathBuf> {
    pick_folder(owner, "选择下载位置")
}

pub(super) fn pick_side_panel_directory(owner: RawWindowHandle) -> Option<PathBuf> {
    pick_folder(owner, "选择目录树根目录")
}

fn pick_file(owner: RawWindowHandle, filter: &str, title: &str) -> Option<PathBuf> {
    pick_files(owner, filter, title, false).into_iter().next()
}

fn pick_files(owner: RawWindowHandle, filter: &str, title: &str, multiple: bool) -> Vec<PathBuf> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
        OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let hwnd: HWND = match owner {
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    };
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
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

fn parse_open_file_buffer(buffer: &[u16]) -> Vec<PathBuf> {
    let parts = buffer
        .split(|value| *value == 0)
        .take_while(|part| !part.is_empty())
        .map(|part| String::from_utf16_lossy(part))
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

fn pick_folder(owner: RawWindowHandle, title: &str) -> Option<PathBuf> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW,
        SHGetPathFromIDListW,
    };

    let hwnd: HWND = match owner {
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    };
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut display_name = vec![0u16; 260];
    let dialog = BROWSEINFOW {
        hwndOwner: hwnd,
        pidlRoot: std::ptr::null_mut(),
        pszDisplayName: display_name.as_mut_ptr(),
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        lpfn: None,
        lParam: 0,
        iImage: 0,
    };
    let item = unsafe { SHBrowseForFolderW(&dialog) };
    if item.is_null() {
        return None;
    }

    let mut path = vec![0u16; 260];
    let ok = unsafe { SHGetPathFromIDListW(item, path.as_mut_ptr()) } != 0;
    unsafe { CoTaskMemFree(item.cast()) };
    if !ok {
        return None;
    }
    let length = path.iter().position(|value| *value == 0).unwrap_or(path.len());
    (length > 0).then(|| PathBuf::from(String::from_utf16_lossy(&path[..length])))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{PrivateKeyFileKind, classify_private_key_contents, parse_open_file_buffer};

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
    fn public_key_text_is_rejected_as_private_key() {
        assert_eq!(
            classify_private_key_contents(b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest user@host"),
            PrivateKeyFileKind::PublicKey
        );
    }

    #[test]
    fn encrypted_and_putty_private_key_headers_are_accepted_for_later_unlock() {
        assert_eq!(
            classify_private_key_contents(
                b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot-decoded-until-passphrase\n"
            ),
            PrivateKeyFileKind::PrivateKey
        );
        assert_eq!(
            classify_private_key_contents(
                b"PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: aes256-cbc"
            ),
            PrivateKeyFileKind::PrivateKey
        );
    }

    #[test]
    fn unrelated_file_is_not_accepted_as_private_key() {
        assert_eq!(
            classify_private_key_contents(b"this is not an SSH key"),
            PrivateKeyFileKind::Unsupported
        );
    }

    #[test]
    fn native_multi_select_buffer_joins_names_to_the_selected_directory() {
        let buffer = wide_parts(&[r"D:\上传 文件", "alpha.txt", "测试.zip"]);
        assert_eq!(
            parse_open_file_buffer(&buffer),
            vec![PathBuf::from(r"D:\上传 文件\alpha.txt"), PathBuf::from(r"D:\上传 文件\测试.zip"),]
        );
    }

    #[test]
    fn native_single_select_buffer_keeps_the_absolute_path() {
        let buffer = wide_parts(&[r"D:\release\nebula.zip"]);
        assert_eq!(parse_open_file_buffer(&buffer), vec![PathBuf::from(r"D:\release\nebula.zip")]);
    }
}
