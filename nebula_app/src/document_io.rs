//! Prefix reads bound memory before decoding or parsing a document.

use std::io::{self, Read};

pub(crate) fn read_prefix(
    reader: &mut impl Read,
    declared_size: u64,
    limit: usize,
) -> io::Result<(Vec<u8>, bool)> {
    let capacity = declared_size.min(limit as u64) as usize;
    let mut bytes = Vec::with_capacity(capacity);
    reader.by_ref().take(limit as u64).read_to_end(&mut bytes)?;
    // Probe overflow separately, avoiding an 8 MiB -> 16 MiB Vec growth merely
    // to read the extra byte that identifies a truncated preview.
    let mut extra = [0u8; 1];
    let truncated = reader.read(&mut extra)? != 0;
    if truncated {
        if let Err(error) = std::str::from_utf8(&bytes) {
            if error.error_len().is_none() {
                bytes.truncate(error.valid_up_to());
            }
        }
    }
    Ok((bytes, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct VirtualLargeFile {
        remaining: usize,
        read: usize,
    }
    impl Read for VirtualLargeFile {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.remaining);
            buffer[..count].fill(b'x');
            self.remaining -= count;
            self.read += count;
            Ok(count)
        }
    }

    #[test]
    fn virtual_hundred_megabyte_file_never_materializes_its_full_contents() {
        let length = 100 * 1024 * 1024;
        let limit = 64 * 1024;
        let mut file = VirtualLargeFile { remaining: length, read: 0 };
        let (bytes, truncated) = read_prefix(&mut file, length as u64, limit).unwrap();
        assert!(truncated);
        assert_eq!(file.read, limit + 1);
        assert_eq!(bytes.len(), limit);
        assert_eq!(bytes.capacity(), limit);
    }

    #[test]
    fn short_files_growth_and_unicode_boundaries_remain_correct() {
        let (bytes, truncated) = read_prefix(&mut "甲乙丙".as_bytes(), 9, 7).unwrap();
        assert!(truncated);
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "甲乙");
        let (bytes, truncated) = read_prefix(&mut &b"abcdef"[..], 1, 4).unwrap();
        assert!(truncated);
        assert_eq!(bytes, b"abcd");
        let (bytes, truncated) = read_prefix(&mut &b"abcd"[..], 4, 4).unwrap();
        assert!(!truncated);
        assert_eq!(bytes, b"abcd");
    }
}
