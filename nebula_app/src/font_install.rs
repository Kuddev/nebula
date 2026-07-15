use std::path::{Path, PathBuf};

pub const REQUIRED_FONT_FAMILY: &str = "Maple Mono Normal NF CN";
pub const REQUIRED_FONT_FILE: &str = "MapleMonoNormal-NF-CN-Regular.ttf";

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
}
