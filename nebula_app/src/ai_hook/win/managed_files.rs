use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Install {
    Installed,
    Current,
    Conflict,
}

fn fingerprint(content: &[u8]) -> String {
    let mut fingerprint = String::with_capacity(64);
    for byte in Sha256::digest(content).iter() {
        let _ = write!(&mut fingerprint, "{byte:02x}");
    }
    fingerprint
}

fn marker_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.pebrel-managed",
        path.extension().and_then(|value| value.to_str()).unwrap_or_default()
    ))
}

fn read_if_present(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_owned(path: &Path, content: &[u8], expected: &[u8], legacy_hashes: &[&str]) -> bool {
    let fingerprint = fingerprint(content);
    content == expected
        || legacy_hashes.contains(&fingerprint.as_str())
        || std::fs::read_to_string(marker_path(path))
            .is_ok_and(|marker| marker.trim() == fingerprint)
}

/// Rename the old bridge before updating it so the loader never sees two copies.
/// Older releases had no ownership marker, so only their exact verified payloads
/// may migrate; an edited or unknown file stays untouched.
pub(super) fn install(
    path: &Path,
    legacy: &Path,
    content: &str,
    legacy_hashes: &[&str],
) -> io::Result<Install> {
    let current = read_if_present(path)?;
    let old = read_if_present(legacy)?;
    for (file, existing) in [(path, &current), (legacy, &old)] {
        if existing
            .as_ref()
            .is_some_and(|bytes| !is_owned(file, bytes, content.as_bytes(), legacy_hashes))
        {
            return Ok(Install::Conflict);
        }
    }

    if old.is_some() {
        if current.is_none() {
            std::fs::rename(legacy, path)?;
        } else {
            std::fs::remove_file(legacy)?;
        }
    }
    let expected = fingerprint(content.as_bytes());
    let marker = marker_path(path);
    if old.is_none()
        && current.as_deref() == Some(content.as_bytes())
        && std::fs::read_to_string(&marker).is_ok_and(|value| value.trim() == expected)
    {
        return Ok(Install::Current);
    }
    crate::atomic_file::write(path, content.as_bytes())?;
    crate::atomic_file::write(&marker, format!("{expected}\n").as_bytes())?;
    Ok(Install::Installed)
}

pub(super) fn remove(
    path: &Path,
    legacy: &Path,
    content: &str,
    legacy_hashes: &[&str],
) -> io::Result<bool> {
    let current = read_if_present(path)?;
    let old = read_if_present(legacy)?;
    for (file, existing) in [(path, &current), (legacy, &old)] {
        if existing
            .as_ref()
            .is_some_and(|bytes| !is_owned(file, bytes, content.as_bytes(), legacy_hashes))
        {
            return Err(io::Error::other(format!(
                "Preserving edited or unmanaged integration: {}",
                file.display()
            )));
        }
    }
    let mut removed = false;
    for (file, existing) in [(path, current), (legacy, old)] {
        if existing.is_some() {
            std::fs::remove_file(file)?;
            let marker = marker_path(file);
            if marker.is_file() {
                std::fs::remove_file(marker)?;
            }
            removed = true;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::{Install, fingerprint, install, remove};

    #[test]
    fn legacy_bridge_is_replaced_without_duplicate_loading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pebrel.js");
        let legacy = directory.path().join("nebula.js");
        std::fs::write(&legacy, "legacy bridge").unwrap();
        let legacy_hash = fingerprint(b"legacy bridge");

        assert_eq!(
            install(&path, &legacy, "new bridge", &[&legacy_hash]).unwrap(),
            Install::Installed
        );
        assert!(!legacy.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new bridge");
        assert_eq!(
            install(&path, &legacy, "new bridge", &[&legacy_hash]).unwrap(),
            Install::Current
        );
        assert!(remove(&path, &legacy, "new bridge", &[&legacy_hash]).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn an_edited_legacy_bridge_is_not_overwritten_or_registered_twice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pebrel.ts");
        let legacy = directory.path().join("nebula.ts");
        let edited = "legacy bridge\nuser customization";
        std::fs::write(&legacy, edited).unwrap();
        let legacy_hash = fingerprint(b"legacy bridge");

        assert_eq!(
            install(&path, &legacy, "new bridge", &[&legacy_hash]).unwrap(),
            Install::Conflict
        );
        assert!(!path.exists());
        assert!(remove(&path, &legacy, "new bridge", &[&legacy_hash]).is_err());
        assert_eq!(std::fs::read_to_string(legacy).unwrap(), edited);
    }

    #[test]
    fn an_edited_current_bridge_survives_update_and_remove() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pebrel.js");
        let legacy = directory.path().join("nebula.js");
        install(&path, &legacy, "version one", &[]).unwrap();
        std::fs::write(&path, "version one with user changes").unwrap();
        assert_eq!(install(&path, &legacy, "version two", &[]).unwrap(), Install::Conflict);
        assert!(remove(&path, &legacy, "version two", &[]).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "version one with user changes");
    }

    #[test]
    fn same_name_conflicts_preserve_the_original_legacy_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pebrel.js");
        let legacy = directory.path().join("nebula.js");
        std::fs::write(&legacy, "legacy bridge").unwrap();
        std::fs::write(&path, "user plugin").unwrap();
        let legacy_hash = fingerprint(b"legacy bridge");
        assert_eq!(
            install(&path, &legacy, "new bridge", &[&legacy_hash]).unwrap(),
            Install::Conflict
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user plugin");
        assert_eq!(std::fs::read_to_string(legacy).unwrap(), "legacy bridge");
    }
}
