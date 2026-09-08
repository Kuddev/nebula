//! File snapshots and conditional, atomic saves. No GPUI state or disk I/O in rendering.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Document {
    pub(super) text: String,
    pub(super) read_only: bool,
    pub(super) truncated: bool,
    pub(super) invalid_encoding: bool,
    target: PathBuf,
    pub(super) bytes: Arc<[u8]>,
    pub(super) bom: bool,
    pub(super) crlf: bool,
    pub(super) modified: Option<std::time::SystemTime>,
}

#[derive(Debug)]
pub(super) enum SaveError {
    Changed,
    ReadOnly,
    Io(std::io::Error),
}

impl From<std::io::Error> for SaveError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl Document {
    pub(super) fn load(path: &Path) -> std::io::Result<Self> {
        let target = path.canonicalize()?;
        let file = fs::File::open(&target)?;
        let metadata = file.metadata()?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        let truncated = bytes.len() > MAX_BYTES;
        bytes.truncate(MAX_BYTES);
        let bom = bytes.starts_with(b"\xef\xbb\xbf");
        let body = if bom { &bytes[3..] } else { &bytes };
        let invalid_encoding = std::str::from_utf8(body).is_err() || body.contains(&0);
        let text = String::from_utf8_lossy(body);
        let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
        let text = text.replace("\r\n", "\n");
        Ok(Self {
            text,
            read_only: metadata.permissions().readonly() || truncated || invalid_encoding,
            truncated,
            invalid_encoding,
            target,
            bytes: bytes.into(),
            bom,
            crlf,
            modified: metadata.modified().ok(),
        })
    }

    fn check_current(&self, path: &Path) -> Result<(), SaveError> {
        if path.canonicalize()? != self.target {
            return Err(SaveError::Changed);
        }
        let file = fs::File::open(&self.target)?;
        let metadata = file.metadata()?;
        if metadata.permissions().readonly() {
            return Err(SaveError::ReadOnly);
        }
        if metadata.len() != self.bytes.len() as u64 {
            return Err(SaveError::Changed);
        }
        let mut current = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut current)?;
        if current.as_slice() != self.bytes.as_ref() {
            return Err(SaveError::Changed);
        }
        Ok(())
    }

    pub(super) fn save(&self, path: &Path, text: String) -> Result<Self, SaveError> {
        if self.read_only {
            return Err(SaveError::ReadOnly);
        }
        self.check_current(path)?;
        if text == self.text {
            return Ok(self.clone());
        }
        let mut bytes = if self.bom { b"\xef\xbb\xbf".to_vec() } else { Vec::new() };
        let encoded = if self.crlf { text.replace('\n', "\r\n") } else { text.clone() };
        bytes.extend_from_slice(encoded.as_bytes());
        let parent = self.target.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent")
        })?;
        // The temporary file lives beside the target, so replacement cannot cross volumes.
        // Resolve symlinks once and revalidate them before replacing the target, not the link.
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.as_file().set_permissions(fs::metadata(&self.target)?.permissions())?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        self.check_current(path)?;
        staged.persist(&self.target).map_err(|error| SaveError::Io(error.error))?;
        Ok(Self {
            text,
            bytes: bytes.into(),
            modified: fs::metadata(&self.target).and_then(|meta| meta.modified()).ok(),
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_preserves_bom_crlf_and_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("中文.md");
        fs::write(&path, b"\xef\xbb\xbf# title\r\nold\r\n").unwrap();
        let doc = Document::load(&path).unwrap();
        assert_eq!(doc.text, "# title\nold\n");
        let saved = doc.save(&path, "# 标题\n新内容\n".into()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), "\u{feff}# 标题\r\n新内容\r\n".as_bytes());
        assert_eq!(saved.text, "# 标题\n新内容\n");
        saved.save(&path, "".into()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xef\xbb\xbf");
    }

    #[test]
    fn concurrent_edit_is_not_overwritten_even_if_length_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        fs::write(&path, "first").unwrap();
        let doc = Document::load(&path).unwrap();
        fs::write(&path, "other").unwrap();
        assert!(matches!(doc.save(&path, "draft".into()), Err(SaveError::Changed)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "other");
        fs::remove_file(&path).unwrap();
        assert!(doc.save(&path, "draft".into()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn partial_or_lossy_previews_can_never_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        fs::write(&path, vec![b'x'; MAX_BYTES + 1]).unwrap();
        let doc = Document::load(&path).unwrap();
        assert!(doc.truncated && doc.read_only);
        assert!(matches!(doc.save(&path, "short".into()), Err(SaveError::ReadOnly)));
        fs::write(&path, [0xff, 0xfe, 0]).unwrap();
        let doc = Document::load(&path).unwrap();
        assert!(doc.invalid_encoding && doc.read_only);
        assert!(matches!(doc.save(&path, "text".into()), Err(SaveError::ReadOnly)));
    }

    #[cfg(unix)]
    #[test]
    fn save_through_symlink_preserves_link_and_target_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("script.sh");
        let link = dir.path().join("link.sh");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&target, &link).unwrap();
        Document::load(&link).unwrap().save(&link, "new".into()).unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
    }
}
