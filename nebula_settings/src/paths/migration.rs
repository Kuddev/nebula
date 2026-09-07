use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_FILE: &str = ".pebrel-migration.lock";
const COMPLETE_FILE: &str = ".pebrel-migration-v1";
const CONFIG_FILES: &[&str] = &["pebrel.lua", "pebrel.toml", "pebrel.yml", "pebrel.yaml"];
static COPY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DATA_FILES: &[(&str, &str)] = &[
    ("nebula_settings.txt", "pebrel_settings.txt"),
    ("nebula_assistant.txt", "pebrel_assistant.txt"),
    ("nebula_providers.json", "pebrel_providers.json"),
    ("nebula_backup.txt", "pebrel_backup.txt"),
    ("nebula_sync.txt", "pebrel_sync.txt"),
    ("nebula_theme.txt", "pebrel_theme.txt"),
    ("nebula_history.jsonl", "pebrel_history.jsonl"),
    ("nebula_history_wsl.jsonl", "pebrel_history_wsl.jsonl"),
    ("nebula_history_ssh.jsonl", "pebrel_history_ssh.jsonl"),
    ("nebula_debug.log", "pebrel_debug.log"),
    ("nebula-panic.log", "pebrel-panic.log"),
    ("nebula.lua", "pebrel.lua"),
    ("nebula.toml", "pebrel.toml"),
    ("nebula.yml", "pebrel.yml"),
    ("nebula.yaml", "pebrel.yaml"),
];

/// Map persisted legacy names at migration and archive-import boundaries.
pub fn canonical_data_file_name(name: &str) -> &str {
    DATA_FILES.iter().find_map(|(old, new)| (*old == name).then_some(*new)).unwrap_or(name)
}

pub fn legacy_data_file_name(name: &str) -> Option<&'static str> {
    DATA_FILES.iter().find_map(|(old, new)| (*new == name).then_some(*old))
}

pub fn is_migration_artifact(name: &str) -> bool {
    name == LOCK_FILE
        || name == COMPLETE_FILE
        || name
            .rsplit_once(".pebrel-migrate-")
            .and_then(|(_, suffix)| suffix.strip_suffix(".tmp"))
            .is_some_and(|suffix| {
                let parts: Vec<_> = suffix.split('-').collect();
                parts.len() == 3
                    && parts.iter().all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
}

pub(super) fn migrate_data_at(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    let destination = destination.canonicalize()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(destination.join(LOCK_FILE))?;
    // The OS releases this lock on process exit, including an interrupted migration.
    lock.lock()?;
    if destination.join(COMPLETE_FILE).try_exists()? {
        return Ok(());
    }

    let keep_config = CONFIG_FILES.iter().try_fold(false, |exists, name| {
        destination_exists(&destination.join(name)).map(|found| exists || found)
    })?;
    if source.try_exists()? {
        let source = source.canonicalize()?;
        if source != destination {
            if destination.starts_with(&source) || source.starts_with(&destination) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy and Pebrel data directories must not contain one another",
                ));
            }
            // Preserve the original paths for absolute imports and manual recovery.
            copy_tree(&source, &destination, keep_config)?;
        }
    }
    for &(old, new) in DATA_FILES {
        if keep_config && CONFIG_FILES.contains(&new) {
            continue;
        }
        let old = destination.join(old);
        if old.try_exists()? {
            copy_file(&old, &destination.join(new))?;
        }
    }

    write_if_absent(&destination.join(COMPLETE_FILE), |file| {
        file.write_all(b"Legacy data copied; original paths retained.\n")
    })
}

fn copy_tree(source: &Path, destination: &Path, keep_config: bool) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(is_migration_artifact) {
            continue;
        }
        if keep_config && name.to_str().is_some_and(|name| CONFIG_FILES.contains(&name)) {
            continue;
        }
        let target = destination.join(&name);
        let source = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            match fs::symlink_metadata(&target) {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    continue;
                },
                Ok(_) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&target)?;
                },
                Err(error) => return Err(error),
            }
            copy_tree(&source, &target, false)?;
        } else if kind.is_file() {
            copy_file(&source, &target)?;
        } else if kind.is_symlink() {
            copy_symlink(&source, &target)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported legacy data entry: {}", source.display()),
            ));
        }
    }
    Ok(())
}

fn destination_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    write_if_absent(destination, |output| {
        let mut input = File::open(source)?;
        io::copy(&mut input, output)?;
        #[cfg(unix)]
        output.set_permissions(input.metadata()?.permissions())?;
        Ok(())
    })
}

fn write_if_absent(
    destination: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    if destination_exists(destination)? {
        return Ok(());
    }
    let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let temporary = destination.with_extension(format!(
        "pebrel-migrate-{}-{timestamp}-{sequence}.tmp",
        std::process::id()
    ));
    let mut output = OpenOptions::new().create_new(true).write(true).open(&temporary)?;
    let written = write(&mut output).and_then(|_| output.sync_all());
    drop(output);
    let result = written.and_then(|_| {
        // Publish a complete file without overwriting concurrent user writes.
        match publish_file(&temporary, destination) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            result => result,
        }
    });
    let cleanup = match fs::remove_file(&temporary) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    };
    result.and(cleanup)
}

#[cfg(not(windows))]
fn publish_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(temporary, destination)
}

#[cfg(windows)]
fn publish_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let new: Vec<_> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // Flags 0 refuses an existing destination and also works on FAT/exFAT volumes.
    if unsafe { MoveFileExW(existing.as_ptr(), new.as_ptr(), 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn copy_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    if destination_exists(destination)? {
        return Ok(());
    }
    let target = fs::read_link(source)?;
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, destination);
    #[cfg(windows)]
    let result = {
        use std::os::windows::fs::{FileTypeExt, symlink_dir, symlink_file};
        if fs::symlink_metadata(source)?.file_type().is_symlink_dir() {
            symlink_dir(target, destination)
        } else {
            symlink_file(target, destination)
        }
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
