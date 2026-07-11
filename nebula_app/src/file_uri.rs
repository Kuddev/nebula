//! Translate `file://` URIs (from OSC 8 hyperlinks) into native Windows paths.
//!
//! The shells Nebula integrates with all emit clickable `ls` entries as OSC 8
//! `file://` links, but they disagree on the path dialect inside the URI:
//!
//! - **PowerShell** (`Nebula-List`): `file:///C:/Users/me/a%20b.txt` — empty
//!   host, real Windows drive path, percent-encoded.
//! - **Git-bash / MSYS** (`ls --hyperlink`): `file://HOST/d/temp/x` — MSYS
//!   drive form (`/d/…`), so the leading segment is the drive letter.
//! - **WSL** (`ls --hyperlink`): `file://HOST/mnt/c/x` — the WSL mount form
//!   (`/mnt/<drive>/…`), or `/cygdrive/<drive>/…` under some coreutils builds.
//!
//! Feeding those posix-looking paths to `cmd /c start ""` fails: `explorer`
//! can't resolve `/d/temp` and `cmd`'s own tokenizer mangles URIs with spaces
//! or non-ASCII bytes. Normalizing to a real Windows path up front lets us hand
//! `explorer.exe` a path it can always open, and keeps unicode filenames intact
//! (percent-decoding is UTF-8 aware).
//!
//! Only compiled on Windows — every other platform keeps the default
//! `xdg-open`/`open` hint action untouched.
#![cfg(windows)]

use std::path::PathBuf;

/// Decode a `file://` URI into a native Windows path.
///
/// Returns `None` when the input is not a `file:` URI (so the caller can fall
/// back to the default hint action), or when it names a remote host we won't
/// touch. `drive_exists` gates the ambiguous MSYS `/d/…` form: a single-letter
/// leading segment is only treated as a drive when that drive actually exists,
/// otherwise it's a real directory named `d`.
pub fn file_uri_to_local_path(uri: &str) -> Option<PathBuf> {
    file_uri_to_local_path_with(uri, |drive| drive_exists(drive))
}

/// Test seam for [`file_uri_to_local_path`]; `drive_exists` is injected so unit
/// tests don't depend on which drive letters the host machine happens to have.
fn file_uri_to_local_path_with(
    uri: &str,
    drive_exists: impl Fn(char) -> bool,
) -> Option<PathBuf> {
    // Case-insensitive `file:` scheme check without allocating.
    let rest = strip_scheme(uri)?;

    // After `file:` the authority is introduced by `//`. Anything else (e.g.
    // `file:relative`) is non-standard; bail to the fallback path.
    let rest = rest.strip_prefix("//")?;

    // Split `HOST/PATH`. The path always begins at the first '/', and an empty
    // host (`file:///C:/…`) leaves that slash at index 0.
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    let path = percent_decode_utf8(&rest[slash..]);

    if host_is_local(host) {
        translate_local_path(&path, &drive_exists)
    } else {
        // Remote host → UNC share (`\\HOST\path`). The plain `cmd start` link
        // action already accepted these; we just build a path `explorer`
        // understands instead of a URI.
        let host = percent_decode_utf8(host);
        let tail = path.trim_start_matches('/').replace('/', "\\");
        (!host.is_empty()).then(|| PathBuf::from(format!("\\\\{host}\\{tail}")))
    }
}

/// Strip a case-insensitive `file:` scheme prefix.
fn strip_scheme(uri: &str) -> Option<&str> {
    let bytes = uri.as_bytes();
    (bytes.len() >= 5 && bytes[..5].eq_ignore_ascii_case(b"file:")).then(|| &uri[5..])
}

/// Whether a URI host names this machine (so the path is local, not a share).
fn host_is_local(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host == "127.0.0.1" || host == "::1" || host == "[::1]" {
        return true;
    }
    // `ls --hyperlink` uses the machine's hostname; treat our own name as local.
    std::env::var("COMPUTERNAME")
        .ok()
        .is_some_and(|name| name.eq_ignore_ascii_case(host))
}

/// Convert a decoded, local, posix-looking URI path into a Windows path.
///
/// `path` still has its leading '/' and forward slashes. Handles, in order:
/// Windows drive paths (`/C:/…`), WSL/Cygwin mounts (`/mnt/c/…`,
/// `/cygdrive/c/…`), and the MSYS drive form (`/c/…`, drive-existence gated).
fn translate_local_path(path: &str, drive_exists: &impl Fn(char) -> bool) -> Option<PathBuf> {
    // `/C:/Users/…` — a real Windows drive path with a stray leading slash.
    if is_drive_prefixed(&path[1..]) {
        return Some(to_windows(&path[1..]));
    }

    let trimmed = path.trim_start_matches('/');

    // `/mnt/c/…` (WSL) or `/cygdrive/c/…` (Cygwin/some MSYS builds).
    for mount in ["mnt/", "cygdrive/"] {
        if let Some(after) = trimmed.strip_prefix(mount) {
            if let Some(win) = mount_drive_path(after) {
                return Some(win);
            }
        }
    }

    // `/c/temp/…` — MSYS drive form. Only when the drive really exists; a lone
    // leading segment named like an existing drive letter is otherwise a
    // genuine directory (`/opt`, `/home`, …).
    let mut segments = trimmed.splitn(2, '/');
    if let Some(first) = segments.next() {
        if let Some(drive) = single_drive_letter(first) {
            if drive_exists(drive) {
                let tail = segments.next().unwrap_or("");
                return Some(to_windows(&format!("{drive}:/{tail}")));
            }
        }
    }

    // A truly posix path with no drive mapping (e.g. a WSL-internal
    // `/home/user` path) isn't reachable from Windows `explorer`; give up so
    // the caller can fall back.
    None
}

/// Build a Windows path from a `<drive>/<rest>` mount tail (`c/x` → `C:\x`).
fn mount_drive_path(after: &str) -> Option<PathBuf> {
    let mut parts = after.splitn(2, '/');
    let drive = single_drive_letter(parts.next()?)?;
    let tail = parts.next().unwrap_or("");
    Some(to_windows(&format!("{drive}:/{tail}")))
}

/// `true` for a `X:` / `X:/…` drive-letter prefix.
fn is_drive_prefixed(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// The uppercase drive letter if `s` is exactly one ascii letter, else `None`.
fn single_drive_letter(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_uppercase()),
        _ => None,
    }
}

/// Normalize forward slashes to backslashes and uppercase the drive letter.
fn to_windows(path: &str) -> PathBuf {
    let mut out = path.replace('/', "\\");
    if is_drive_prefixed(&out) {
        // SAFETY: `is_drive_prefixed` guarantees a leading ascii byte.
        out[..1].make_ascii_uppercase();
    }
    PathBuf::from(out)
}

/// Whether drive `letter` (e.g. `'C'`) is currently mounted.
fn drive_exists(letter: char) -> bool {
    std::path::Path::new(&format!("{letter}:\\")).exists()
}

/// UTF-8 aware percent-decoding. `%HH` pairs are collected into a byte buffer
/// and decoded as UTF-8 at the end, so multi-byte characters (e.g. CJK
/// filenames encoded by the shell) round-trip correctly. Malformed escapes are
/// left verbatim.
fn percent_decode_utf8(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve against a fixed set of "mounted" drives so tests are stable.
    fn t(uri: &str) -> Option<String> {
        file_uri_to_local_path_with(uri, |d| matches!(d, 'C' | 'D'))
            .map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn powershell_drive_uri() {
        assert_eq!(t("file:///C:/Users/me/file.txt"), Some(r"C:\Users\me\file.txt".into()));
    }

    #[test]
    fn drive_letter_uppercased() {
        assert_eq!(t("file:///d:/temp/x"), Some(r"D:\temp\x".into()));
    }

    #[test]
    fn percent_encoded_space() {
        assert_eq!(t("file:///C:/a%20b/c.txt"), Some(r"C:\a b\c.txt".into()));
    }

    #[test]
    fn utf8_filename_roundtrips() {
        // "文档" percent-encoded as UTF-8.
        assert_eq!(
            t("file:///D:/%E6%96%87%E6%A1%A3/x.md"),
            Some("D:\\文档\\x.md".into())
        );
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(t("FILE:///C:/x"), Some(r"C:\x".into()));
    }

    #[test]
    fn wsl_mount_path() {
        assert_eq!(t("file://localhost/mnt/c/work/a.rs"), Some(r"C:\work\a.rs".into()));
    }

    #[test]
    fn cygdrive_mount_path() {
        assert_eq!(t("file://localhost/cygdrive/d/proj/b.rs"), Some(r"D:\proj\b.rs".into()));
    }

    #[test]
    fn msys_drive_form_when_drive_exists() {
        assert_eq!(t("file://localhost/d/temp_build/x"), Some(r"D:\temp_build\x".into()));
    }

    #[test]
    fn msys_drive_form_root() {
        assert_eq!(t("file://localhost/c/"), Some(r"C:\".into()));
    }

    #[test]
    fn leading_segment_that_is_not_a_drive_is_not_mangled() {
        // 'z' isn't a mounted drive → not a drive form; and there's no Windows
        // mapping for a bare posix path, so we fall back (None).
        assert_eq!(t("file://localhost/z/opt/thing"), None);
    }

    #[test]
    fn pure_posix_path_falls_back() {
        assert_eq!(t("file://localhost/home/user/.bashrc"), None);
    }

    #[test]
    fn remote_host_becomes_unc() {
        assert_eq!(t("file://fileserver/share/doc.txt"), Some(r"\\fileserver\share\doc.txt".into()));
    }

    #[test]
    fn non_file_scheme_falls_back() {
        assert_eq!(t("https://example.com/a"), None);
        assert_eq!(t("mailto:x@y.z"), None);
    }
}
