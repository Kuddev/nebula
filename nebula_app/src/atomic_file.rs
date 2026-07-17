//! Small cross-platform primitives for durable application-state files.
//!
//! State writers use a sibling temporary file followed by an atomic replace,
//! so a crash cannot leave a half-written JSON document. A best-effort lock
//! prevents two Nebula processes from compacting the same store at once.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(1);

/// Atomically replace `path` with `contents`, syncing the temporary file first.
pub(crate) fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::other("state path has no parent"))?;
    std::fs::create_dir_all(parent)?;

    let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("state");
    let temporary =
        parent.join(format!(".{file_name}.nebula-tmp-{}-{sequence}", std::process::id()));

    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Try to own a sibling lock file without waiting on the UI thread.
pub(crate) fn try_lock(path: &Path) -> io::Result<Option<FileLock>> {
    let lock_path = lock_path(path);
    match create_lock(&lock_path) {
        Ok(lock) => Ok(Some(lock)),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // A crashed process can strand the tiny lock file. State writes
            // finish in milliseconds, so a 30-second-old lock is stale enough
            // to reclaim without racing a healthy writer.
            let stale = std::fs::metadata(&lock_path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= Duration::from_secs(30));
            if !stale {
                return Ok(None);
            }
            let _ = std::fs::remove_file(&lock_path);
            match create_lock(&lock_path) {
                Ok(lock) => Ok(Some(lock)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(None),
                Err(err) => Err(err),
            }
        },
        Err(err) => Err(err),
    }
}

fn create_lock(path: &Path) -> io::Result<FileLock> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(err) = write!(file, "{}", std::process::id()) {
        // 锁文件一旦创建成功就代表“已占用”；写 PID 失败时必须立刻回收，
        // 否则一次磁盘错误会伪装成持续 30 秒的跨进程竞争。
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(err);
    }
    drop(file);
    Ok(FileLock { path: path.to_owned() })
}

fn lock_path(path: &Path) -> PathBuf {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    path.with_extension(if extension.is_empty() {
        "nebula-lock".to_owned()
    } else {
        format!("{extension}.nebula-lock")
    })
}

pub(crate) struct FileLock {
    path: PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(not(windows))]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)?;
    // rename 只保证目录项切换是原子的；同步父目录后，掉电恢复时才不会
    // 出现“文件内容已落盘、文件名更新却丢失”的窗口。
    if let Some(parent) = destination.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{try_lock, write};

    #[test]
    fn atomic_write_replaces_and_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");

        let first = try_lock(&path).unwrap().expect("first lock");
        assert!(try_lock(&path).unwrap().is_none());
        drop(first);
        assert!(try_lock(&path).unwrap().is_some());
    }
}
