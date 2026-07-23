use std::path::PathBuf;

use super::window::Window;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "file_dialog/desktop.rs"]
mod platform;
#[cfg(windows)]
#[path = "file_dialog/windows.rs"]
mod platform;
#[cfg(windows)]
#[path = "file_dialog/folder.rs"]
mod windows_folder;

#[derive(Debug, Clone, Copy)]
struct FileFilter {
    name: &'static str,
    #[cfg_attr(windows, allow(dead_code))]
    extensions: &'static [&'static str],
    #[cfg_attr(not(windows), allow(dead_code))]
    patterns: &'static [&'static str],
}

const ALL_FILES_FILTER: FileFilter =
    FileFilter { name: "All files", extensions: &["*"], patterns: &["*.*"] };
const IMAGE_FILTERS: &[FileFilter] = &[
    FileFilter {
        name: "Images",
        extensions: &["png", "jpg", "jpeg", "webp", "bmp"],
        patterns: &["*.png", "*.jpg", "*.jpeg", "*.webp", "*.bmp"],
    },
    ALL_FILES_FILTER,
];
const FONT_FILTERS: &[FileFilter] = &[
    FileFilter {
        name: "Fonts",
        extensions: &["ttf", "otf", "ttc", "otc"],
        patterns: &["*.ttf", "*.otf", "*.ttc", "*.otc"],
    },
    ALL_FILES_FILTER,
];
const PRIVATE_KEY_FILTERS: &[FileFilter] = &[
    FileFilter {
        name: "SSH private keys",
        extensions: &["pem", "key", "ppk"],
        patterns: &["id_*", "*.pem", "*.key", "*.ppk"],
    },
    ALL_FILES_FILTER,
];

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

pub(super) fn pick_image_file(owner: &Window) -> Option<String> {
    platform::pick_file(owner, "Choose background image", IMAGE_FILTERS)
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn pick_font_file(owner: &Window) -> Option<PathBuf> {
    platform::pick_file(owner, "导入终端字体", FONT_FILTERS)
}

pub(super) fn pick_private_key_file(owner: &Window) -> Option<Result<PathBuf, String>> {
    let path = platform::pick_file(owner, "Choose SSH private key", PRIVATE_KEY_FILTERS)?;
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

pub(super) fn pick_upload_files(owner: &Window) -> Vec<PathBuf> {
    platform::pick_files(owner, "选择要上传的文件", &[ALL_FILES_FILTER])
}

pub(super) fn pick_upload_directory(owner: &Window) -> Option<PathBuf> {
    platform::pick_folder(owner, "选择要上传的文件夹")
}

pub(super) fn pick_download_directory(owner: &Window) -> Option<PathBuf> {
    platform::pick_folder(owner, "选择下载位置")
}

pub(super) fn pick_side_panel_directory(owner: &Window) -> Option<PathBuf> {
    platform::pick_folder(owner, "选择目录树根目录")
}

pub(super) fn pick_startup_directory(owner: &Window) -> Option<PathBuf> {
    platform::pick_folder(owner, "选择终端启动目录")
}

#[cfg(test)]
mod tests {
    use super::{PrivateKeyFileKind, classify_private_key_contents};

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
}
