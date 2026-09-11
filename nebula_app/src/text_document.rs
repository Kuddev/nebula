//! Renderer-independent text snapshots shared by local and SFTP documents.

use std::io;
use std::sync::Arc;

pub(crate) const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct TextSnapshot {
    pub text: String,
    pub bytes: Arc<[u8]>,
    pub bom: bool,
    pub crlf: bool,
    pub truncated: bool,
    pub invalid_encoding: bool,
    pub read_only: bool,
}

#[derive(Debug)]
pub(crate) enum SaveError {
    Changed,
    ReadOnly,
    Io(io::Error),
}

impl From<io::Error> for SaveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => formatter.write_str("The file changed since it was opened"),
            Self::ReadOnly => formatter.write_str("The document is read-only"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SaveError {}

impl TextSnapshot {
    /// Preserve the read-only contract when the bounded reader already removed
    /// the overflow byte instead of handing decode an oversized allocation.
    pub(crate) fn decode_prefix(bytes: Vec<u8>, read_only: bool, truncated: bool) -> Self {
        let mut snapshot = Self::decode(bytes, read_only);
        snapshot.truncated |= truncated;
        snapshot.read_only |= truncated;
        snapshot
    }

    pub(crate) fn decode(mut bytes: Vec<u8>, read_only: bool) -> Self {
        let truncated = bytes.len() > MAX_BYTES;
        bytes.truncate(MAX_BYTES);
        let bom = bytes.starts_with(b"\xef\xbb\xbf");
        let body = if bom { &bytes[3..] } else { &bytes };
        let invalid_encoding = std::str::from_utf8(body).is_err() || body.contains(&0);
        let text = String::from_utf8_lossy(body);
        let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
        let text = text.replace("\r\n", "\n");
        Self {
            text,
            bytes: bytes.into(),
            bom,
            crlf,
            truncated,
            invalid_encoding,
            read_only: read_only || truncated || invalid_encoding,
        }
    }

    pub(crate) fn encode(&self, text: &str) -> Result<Vec<u8>, SaveError> {
        if self.read_only {
            return Err(SaveError::ReadOnly);
        }
        let mut bytes = if self.bom { b"\xef\xbb\xbf".to_vec() } else { Vec::new() };
        let encoded = if self.crlf { text.replace('\n', "\r\n") } else { text.to_owned() };
        bytes.extend_from_slice(encoded.as_bytes());
        if bytes.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Editable text is limited to 8 MiB",
            )
            .into());
        }
        Ok(bytes)
    }

    pub(crate) fn verify(&self, current: &[u8]) -> Result<(), SaveError> {
        if current == self.bytes.as_ref() { Ok(()) } else { Err(SaveError::Changed) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_already_bounded_prefix_remains_read_only() {
        let snapshot = TextSnapshot::decode_prefix(b"valid prefix".to_vec(), false, true);
        assert!(snapshot.truncated && snapshot.read_only);
        assert!(matches!(snapshot.encode("replacement"), Err(SaveError::ReadOnly)));
    }

    #[test]
    fn both_backends_preserve_bom_crlf_and_unicode() {
        let snapshot = TextSnapshot::decode("\u{feff}标题\r\n内容\r\n".as_bytes().to_vec(), false);
        assert_eq!(snapshot.text, "标题\n内容\n");
        assert_eq!(snapshot.encode("新标题\n").unwrap(), "\u{feff}新标题\r\n".as_bytes());
        assert_eq!(snapshot.encode("").unwrap(), b"\xef\xbb\xbf");
    }

    #[test]
    fn same_length_external_edits_are_conflicts() {
        let snapshot = TextSnapshot::decode(b"first".to_vec(), false);
        assert!(snapshot.verify(b"first").is_ok());
        assert!(matches!(snapshot.verify(b"other"), Err(SaveError::Changed)));
    }

    #[test]
    fn binary_and_partial_snapshots_never_become_writable() {
        for bytes in [vec![b'a', 0, b'b'], vec![0xff], vec![b'x'; MAX_BYTES + 1]] {
            let snapshot = TextSnapshot::decode(bytes, false);
            assert!(matches!(snapshot.encode("replacement"), Err(SaveError::ReadOnly)));
        }
    }
}
