//! Native outbound file drags. The source owns materialization and resource lifetime.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
mod windows;

#[derive(Clone, Debug)]
pub(crate) struct DragFile {
    pub relative_path: PathBuf,
    pub directory: bool,
    pub size: u64,
}

pub(crate) struct MaterializedFile {
    pub path: PathBuf,
    /// Returned streams retain this lease, including cloned streams.
    pub lease: Arc<dyn Send + Sync>,
    pub cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub(crate) type Manifest = Arc<dyn Fn() -> io::Result<Vec<DragFile>> + Send + Sync>;

pub(crate) type Materialize = Arc<dyn Fn(usize) -> io::Result<MaterializedFile> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragOutcome {
    Copied,
    Cancelled,
}

#[derive(Clone, Copy)]
pub(crate) struct DragOrigin {
    thread: u32,
}

impl DragOrigin {
    /// Capture on the source window thread before handing work to the STA worker.
    pub(crate) fn capture() -> Self {
        #[cfg(windows)]
        return Self {
            thread: unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
        };
        #[cfg(not(windows))]
        Self { thread: 0 }
    }
}

pub(crate) fn supported() -> bool {
    cfg!(windows)
}

/// Called on the window thread when GPUI hands a drag to the native adapter.
pub(crate) fn release_pointer() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
    }
}

/// Runs on a dedicated worker, never the GPUI or network executor thread.
pub(crate) fn run(
    origin: DragOrigin,
    files: Manifest,
    materialize: Materialize,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> io::Result<DragOutcome> {
    #[cfg(windows)]
    return windows::run(origin, files, materialize, cancelled);
    #[cfg(not(windows))]
    {
        let _ = (origin, files, materialize, cancelled);
        Err(io::Error::new(io::ErrorKind::Unsupported, "Native file drag is unavailable"))
    }
}

fn validate_manifest(files: &[DragFile]) -> io::Result<()> {
    if files.is_empty() || files.len() > 100_000 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid drag file count"));
    }
    let mut names = std::collections::HashSet::new();
    for file in files {
        validate_relative_path(&file.relative_path)?;
        let name = file.relative_path.to_string_lossy().replace('/', "\\").to_lowercase();
        if !names.insert(name) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Duplicate Windows file name"));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid Windows drag file name");
    let value = path.to_str().ok_or_else(invalid)?;
    if value.is_empty() || value.encode_utf16().count() >= 260 || path.is_absolute() {
        return Err(invalid());
    }
    if path.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Err(invalid());
    }
    for name in value.split(['/', '\\']) {
        if name.is_empty()
            || matches!(name, "." | "..")
            || name.ends_with([' ', '.'])
            || name.chars().any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return Err(invalid());
        }
        let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_or_change_identity_on_windows() {
        for name in [
            "../secret",
            "a/../b",
            "a\\..\\b",
            "/absolute",
            "C:\\absolute",
            "a:b",
            "CON.txt",
            "lpt1",
            "x.",
            "x ",
            "a\0b",
        ] {
            assert!(validate_relative_path(Path::new(name)).is_err(), "{name:?}");
        }
        assert!(validate_relative_path(Path::new("部署资料/配置 文件.txt")).is_ok());
    }

    #[test]
    fn rejects_case_collisions_before_a_copy_can_overwrite_another_file() {
        let files: Vec<_> = ["a.txt", "A.TXT"]
            .into_iter()
            .map(|name| DragFile { relative_path: name.into(), directory: false, size: 1 })
            .collect();
        assert!(validate_manifest(&files).is_err());
    }
}
