use std::path::{Path, PathBuf};

#[cfg(windows)]
use sha2::{Digest, Sha256};

pub const REQUIRED_FONT_FAMILY: &str = "Maple Mono Normal NF CN";
pub const REQUIRED_FONT_FILE: &str = "MapleMonoNormal-NF-CN-Regular.ttf";

#[cfg(windows)]
const MAX_IMPORTED_FONT_BYTES: usize = 64 * 1024 * 1024;

#[cfg(windows)]
pub struct StoredFont {
    pub path: PathBuf,
    pub created: bool,
}

#[cfg(windows)]
pub fn imported_font_directory() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Nebula").join("fonts")
}

#[cfg(windows)]
pub fn imported_font_files() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(imported_font_directory()) else { return Vec::new() };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && supported_font_extension(path))
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[cfg(windows)]
pub fn store_imported_font(source: &Path) -> Result<StoredFont, String> {
    if !supported_font_extension(source) {
        return Err("只支持 .ttf、.otf、.ttc 和 .otc 字体文件".to_owned());
    }
    let bytes = std::fs::read(source)
        .map_err(|error| format!("无法读取字体 {}: {error}", source.display()))?;
    if bytes.is_empty() || bytes.len() > MAX_IMPORTED_FONT_BYTES {
        return Err("字体文件为空或超过 64 MB 限制".to_owned());
    }

    let digest = Sha256::digest(&bytes);
    let id = digest[..12].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let extension = source.extension().and_then(|value| value.to_str()).unwrap_or("ttf");
    let directory = imported_font_directory();
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建字体目录 {}: {error}", directory.display()))?;
    let path = directory.join(format!("{id}.{}", extension.to_ascii_lowercase()));
    let created = !path.exists();
    if created {
        std::fs::write(&path, bytes)
            .map_err(|error| format!("无法保存导入字体 {}: {error}", path.display()))?;
    }
    Ok(StoredFont { path, created })
}

fn supported_font_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
}

fn packaged_font_directory(executable: &Path) -> PathBuf {
    executable.parent().unwrap_or_else(|| Path::new(".")).join("fonts")
}

/// Locate the packaged font directory without installing or copying anything.
pub fn bundled_font_directory() -> PathBuf {
    let packaged = std::env::current_exe().ok().map(|exe| packaged_font_directory(&exe));
    if let Some(directory) = packaged.as_ref() {
        if directory.join(REQUIRED_FONT_FILE).is_file() {
            return directory.clone();
        }
    }

    // Local release builds run from target/release rather than an extracted
    // package, so keep the repository asset as a development-only fallback.
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .join("assets")
        .join("fonts");
    if source.join(REQUIRED_FONT_FILE).is_file() {
        return source;
    }

    packaged.unwrap_or_else(|| PathBuf::from("fonts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_font_directory_is_next_to_the_executable() {
        let executable = Path::new(r"C:\Nebula\nebula.exe");
        assert_eq!(packaged_font_directory(executable), PathBuf::from(r"C:\Nebula\fonts"));
    }

    #[test]
    fn imported_font_extensions_are_limited_to_directwrite_font_containers() {
        assert!(supported_font_extension(Path::new("terminal.TTF")));
        assert!(supported_font_extension(Path::new("terminal.otf")));
        assert!(!supported_font_extension(Path::new("terminal.woff2")));
        assert!(!supported_font_extension(Path::new("terminal.exe")));
    }
}
