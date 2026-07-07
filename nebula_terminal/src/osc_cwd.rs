//! Sniffer for OSC sequences the vte parser drops: OSC 7 / OSC 9;9 (working
//! directory) and OSC 133;A (FinalTerm/semantic prompt mark).
//!
//! The vte parser Nebula uses (crates.io `vte` 0.15) does not decode OSC 7
//! (`file://` URI), OSC 9;9 (ConEmu path) or OSC 133 (semantic prompt zones) —
//! it logs them as "unhandled" and drops them. Rather than fork the parser, we
//! tee the raw PTY byte stream through this tiny state machine.
//!
//! Each recognized event is returned tagged with the byte offset **just past
//! its terminator** within the fed chunk. Prompt marks need the grid cursor
//! exactly where the shell emitted the sequence, so the PTY reader splits its
//! `parser.advance` call at these offsets and applies each mark in between —
//! zero vte changes, perfect cursor accuracy.
//!
//! On Windows the cwd channels differ by convention: Nushell/Windows-Terminal
//! shells default to OSC 9;9 (Nushell's OSC 7 is off by default on Windows),
//! while PowerShell/pwsh and most Unix shells use OSC 7. We accept both.
//! OSC 133;A comes from Nebula's own shell integration (PS1/prompt hooks) or
//! natively from shells like Nushell.
//!
//! The state machine survives an OSC split across read chunks, and stops
//! accumulating as soon as a payload can't be one of ours — so an unrelated
//! but huge OSC (e.g. an OSC 52 clipboard blob) never grows our buffer.

/// Cap on a single OSC payload we're willing to buffer. A real cwd path is far
/// shorter; anything longer is not a directory report and gets dropped.
const MAX_PAYLOAD: usize = 4096;

/// Cap for OSC 1337 inline-image payloads (base64 PNG). Anything larger is
/// dropped rather than buffered forever.
const MAX_IMAGE_PAYLOAD: usize = 12 * 1024 * 1024;

/// An OSC event recognized by the sniffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    /// OSC 7 / 9;9 — the shell reported its working directory (native path).
    Cwd(String),
    /// OSC 133;A — the shell is about to draw a prompt (semantic zone start).
    PromptMark,
    /// OSC 133;C — a command started executing.
    CommandStart,
    /// OSC 133;D — the command finished (exit code, when reported).
    CommandDone,
    /// OSC 9 — free-text program notification (iTerm style).
    Notify(String),
    /// OSC 1337 `File=...inline=1:<base64>` — an iTerm2 inline image (PNG).
    /// `width`/`height` come from the PNG header, in pixels.
    InlineImage { png: Vec<u8>, width: u32, height: u32 },
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Outside any escape sequence.
    #[default]
    Ground,
    /// Saw `ESC` (0x1b).
    Esc,
    /// Inside `ESC ]` … collecting the OSC payload.
    Osc,
    /// Inside an OSC and saw `ESC` — maybe the `ESC \` string terminator.
    OscEsc,
}

/// Streaming OSC 7 / 9;9 / 133;A sniffer. Feed it every PTY byte; it returns
/// the recognized events, each tagged with the offset just past its
/// terminator (the `parser.advance` split point).
#[derive(Default)]
pub struct CwdSniffer {
    phase: Phase,
    payload: Vec<u8>,
    /// Cleared once the payload's prefix rules out all sequences we care
    /// about, so the rest of that (irrelevant) OSC is skipped unbuffered.
    interested: bool,
}

impl CwdSniffer {
    /// Feed a chunk of raw PTY output. Returns all complete events within the
    /// chunk in order, tagged with the byte offset just past each terminator.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<(usize, OscEvent)> {
        let mut events = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match self.phase {
                Phase::Ground => {
                    if b == 0x1b {
                        self.phase = Phase::Esc;
                    }
                },
                Phase::Esc => match b {
                    b']' => {
                        self.phase = Phase::Osc;
                        self.payload.clear();
                        self.interested = true;
                    },
                    0x1b => {}, // another ESC: stay armed
                    _ => self.phase = Phase::Ground,
                },
                Phase::Osc => self.step_osc(b, i, &mut events),
                Phase::OscEsc => {
                    if b == b'\\' {
                        // ST terminator (ESC \).
                        if let Some(event) = self.parse() {
                            events.push((i + 1, event));
                        }
                        self.to_ground();
                    } else {
                        // The ESC belonged to the payload after all; keep it and
                        // reprocess this byte in the normal OSC state.
                        self.push(0x1b);
                        self.phase = Phase::Osc;
                        self.step_osc(b, i, &mut events);
                    }
                },
            }
        }
        events
    }

    /// Handle one byte while inside an OSC payload. `i` is the byte's offset
    /// in the fed chunk, used to tag completed events.
    fn step_osc(&mut self, b: u8, i: usize, events: &mut Vec<(usize, OscEvent)>) {
        match b {
            0x07 => {
                // BEL terminator.
                if let Some(event) = self.parse() {
                    events.push((i + 1, event));
                }
                self.to_ground();
            },
            0x1b => self.phase = Phase::OscEsc,
            _ => self.push(b),
        }
    }

    fn to_ground(&mut self) {
        self.phase = Phase::Ground;
        self.payload.clear();
        self.interested = false;
    }

    /// Append a payload byte, giving up early once the prefix can't match.
    fn push(&mut self, b: u8) {
        if !self.interested {
            return;
        }
        // Inline images are the one legitimately huge OSC we buffer.
        let cap = if self.payload.starts_with(b"1337;") { MAX_IMAGE_PAYLOAD } else { MAX_PAYLOAD };
        if self.payload.len() >= cap {
            self.interested = false;
            return;
        }
        self.payload.push(b);
        // Decide as soon as we have enough bytes to compare against the
        // prefixes we care about ("7;", "9;9;", "133;" and "1337;").
        if self.payload.len() <= 4 && !prefix_could_match(&self.payload) {
            self.interested = false;
        }
    }

    /// Parse a completed payload into an event.
    fn parse(&self) -> Option<OscEvent> {
        if let Some(rest) = self.payload.strip_prefix(b"7;") {
            return parse_osc7_uri(rest).map(OscEvent::Cwd);
        }
        if let Some(rest) = self.payload.strip_prefix(b"9;9;") {
            let s = String::from_utf8_lossy(rest);
            let s = s.trim().trim_end_matches(['/', '\\']);
            return (!s.is_empty()).then(|| OscEvent::Cwd(s.to_string()));
        }
        if let Some(rest) = self.payload.strip_prefix(b"1337;") {
            return parse_osc1337_image(rest);
        }
        if let Some(rest) = self.payload.strip_prefix(b"133;") {
            // Semantic prompt zones (FinalTerm). `A` may carry kitty-style
            // `;key=value` params — accept those too. B (command start being
            // typed) has no consumer yet.
            let phased = |ch: u8| rest.first() == Some(&ch) && (rest.len() == 1 || rest[1] == b';');
            if phased(b'A') {
                return Some(OscEvent::PromptMark);
            }
            if phased(b'C') {
                return Some(OscEvent::CommandStart);
            }
            if phased(b'D') {
                return Some(OscEvent::CommandDone);
            }
            return None;
        }
        if let Some(rest) = self.payload.strip_prefix(b"9;") {
            // OSC 9 family. `9;9;` (cwd) matched above; `9;4;` is ConEmu
            // progress (no consumer yet); anything else is an iTerm-style
            // text notification.
            if rest.starts_with(b"4;") {
                return None;
            }
            let text = String::from_utf8_lossy(rest).trim().to_owned();
            return (!text.is_empty()).then_some(OscEvent::Notify(text));
        }
        None
    }
}

/// Whether `payload` is a prefix of, or prefixed by, one of our OSC numbers.
fn prefix_could_match(payload: &[u8]) -> bool {
    const A: &[u8] = b"7;";
    const B: &[u8] = b"9;";
    const C: &[u8] = b"133;";
    const D: &[u8] = b"1337;";
    let matches = |target: &[u8]| target.starts_with(payload) || payload.starts_with(target);
    matches(A) || matches(B) || matches(C) || matches(D)
}

/// Parse an OSC 1337 body (`File=key=value;...:<base64>`) into an inline
/// image event. Only `inline=1` PNG payloads are accepted; `width`/`height`
/// come straight from the PNG IHDR, so broken/foreign params can't lie.
fn parse_osc1337_image(rest: &[u8]) -> Option<OscEvent> {
    use base64::Engine as _;

    let rest = rest.strip_prefix(b"File=")?;
    let colon = rest.iter().position(|&b| b == b':')?;
    let (args, data) = (&rest[..colon], &rest[colon + 1..]);

    // `inline=1` is required — without it iTerm2 semantics are "download".
    let inline = args.split(|&b| b == b';').any(|arg| arg == b"inline=1");
    if !inline {
        return None;
    }

    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| {
            // Some emitters wrap base64 in whitespace/newlines; strip and retry.
            let cleaned: Vec<u8> =
                data.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            base64::engine::general_purpose::STANDARD.decode(&cleaned)
        })
        .ok()?;

    let (width, height) = png_dimensions(&png)?;
    Some(OscEvent::InlineImage { png, width, height })
}

/// Read a PNG's pixel dimensions from its IHDR without decoding the image.
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || !png.starts_with(MAGIC) || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (width > 0 && height > 0 && width <= 16384 && height <= 16384).then_some((width, height))
}

/// Decode an OSC 7 body (`file://HOST/PATH`) into a native path.
fn parse_osc7_uri(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?.trim();
    let after = s.strip_prefix("file://").unwrap_or(s);

    // `after` is `HOST/PATH`; the path starts at the first '/'. An empty host
    // (`file:///C:/…`) leaves the slash at index 0.
    let slash = after.find('/')?;
    let decoded = percent_decode(&after[slash..]);

    // Windows drive paths arrive as "/C:/Users/…"; strip the leading slash.
    let cleaned = if is_windows_drive_path(&decoded) { decoded[1..].to_string() } else { decoded };

    (!cleaned.is_empty()).then_some(cleaned)
}

/// True for "/C:/…" style paths that need their leading slash removed.
fn is_windows_drive_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

/// Minimal percent-decoding (`%20` → space, etc.); leaves malformed escapes as-is.
fn percent_decode(s: &str) -> String {
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

    fn events(bytes: &[u8]) -> Vec<(usize, OscEvent)> {
        CwdSniffer::default().feed(bytes)
    }

    /// The newest cwd within `events`, mirroring the PTY reader's use.
    fn one_of(events: Vec<(usize, OscEvent)>) -> Option<String> {
        events.into_iter().rev().find_map(|(_, e)| match e {
            OscEvent::Cwd(cwd) => Some(cwd),
            _ => None,
        })
    }

    fn one(bytes: &[u8]) -> Option<String> {
        one_of(events(bytes))
    }

    #[test]
    fn osc7_windows_drive() {
        assert_eq!(
            one(b"\x1b]7;file:///C:/Users/foo\x07").as_deref(),
            Some("C:/Users/foo")
        );
    }

    #[test]
    fn osc7_unix_with_host_and_st() {
        assert_eq!(
            one(b"\x1b]7;file://host/home/user\x1b\\").as_deref(),
            Some("/home/user")
        );
    }

    #[test]
    fn osc7_percent_encoded_space() {
        assert_eq!(
            one(b"\x1b]7;file:///C:/My%20Docs\x07").as_deref(),
            Some("C:/My Docs")
        );
    }

    #[test]
    fn osc9_9_conemu_path() {
        assert_eq!(one(b"\x1b]9;9;C:\\Users\\foo\\\x07").as_deref(), Some("C:\\Users\\foo"));
    }

    #[test]
    fn split_across_chunks() {
        let mut s = CwdSniffer::default();
        assert!(s.feed(b"\x1b]7;file:///C:/Wor").is_empty());
        assert_eq!(one_of(s.feed(b"k/dir\x07")).as_deref(), Some("C:/Work/dir"));
    }

    #[test]
    fn keeps_order_of_multiple() {
        let ev = events(b"\x1b]7;file:///a\x07\x1b]7;file:///b\x07");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1, OscEvent::Cwd("/a".into()));
        assert_eq!(ev[1].1, OscEvent::Cwd("/b".into()));
    }

    #[test]
    fn ignores_other_osc() {
        // OSC 0 title and an OSC 52 clipboard blob must not be mistaken for cwd.
        assert!(events(b"\x1b]0;my title\x07").is_empty());
        assert!(events(b"\x1b]52;c;QUJD\x07").is_empty());
    }

    #[test]
    fn ignores_plain_text() {
        assert!(events(b"just some normal output\n").is_empty());
    }

    #[test]
    fn osc133_prompt_mark_bel() {
        // ESC ] 1 3 3 ; A BEL — 8 bytes; offset points just past the BEL.
        assert_eq!(events(b"\x1b]133;A\x07"), vec![(8, OscEvent::PromptMark)]);
    }

    #[test]
    fn osc133_prompt_mark_st() {
        // ST terminator: ESC ] 1 3 3 ; A ESC \ — offset just past the '\'.
        assert_eq!(events(b"\x1b]133;A\x1b\\"), vec![(9, OscEvent::PromptMark)]);
    }

    #[test]
    fn osc133_mark_offset_mid_stream() {
        // The mark's offset is the advance split point after surrounding text.
        let ev = events(b"out\x1b]133;A\x07$ ");
        assert_eq!(ev, vec![(11, OscEvent::PromptMark)]);
    }

    #[test]
    fn osc133_with_params() {
        // kitty-style extra params on A are still a prompt mark.
        assert_eq!(events(b"\x1b]133;A;cl=m\x07"), vec![(13, OscEvent::PromptMark)]);
    }

    #[test]
    fn osc133_other_phases_ignored() {
        // B (command line start) has no consumer; C/D became events.
        assert!(events(b"\x1b]133;B\x07").is_empty());
        assert_eq!(events(b"\x1b]133;C\x07"), vec![(8, OscEvent::CommandStart)]);
        assert_eq!(events(b"\x1b]133;D;0\x07"), vec![(10, OscEvent::CommandDone)]);
    }

    #[test]
    fn osc9_text_notification() {
        assert_eq!(
            events(b"\x1b]9;build done\x07"),
            vec![(15, OscEvent::Notify("build done".into()))]
        );
        // ConEmu progress (9;4) is reserved; cwd (9;9) must keep precedence.
        assert!(events(b"\x1b]9;4;1;50\x07").is_empty());
        assert_eq!(
            one(b"\x1b]9;9;C:\\w\x07").as_deref(),
            Some("C:\\w")
        );
    }

    #[test]
    fn cwd_and_mark_interleaved() {
        let ev = events(b"\x1b]7;file:///C:/w\x07\x1b]133;A\x07");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1, OscEvent::Cwd("C:/w".into()));
        // First OSC is 17 bytes, the mark another 8: offset just past its BEL.
        assert_eq!(ev[1], (25, OscEvent::PromptMark));
    }

    #[test]
    fn mark_split_across_chunks() {
        let mut s = CwdSniffer::default();
        assert!(s.feed(b"\x1b]133;").is_empty());
        // Terminator lands in the second chunk; offset is chunk-relative.
        assert_eq!(s.feed(b"A\x07rest"), vec![(2, OscEvent::PromptMark)]);
    }

    /// A minimal valid 1x1 transparent PNG.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn tiny_png_osc() -> Vec<u8> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(TINY_PNG);
        let mut seq = b"\x1b]1337;File=name=eC5wbmc=;size=70;inline=1:".to_vec();
        seq.extend_from_slice(b64.as_bytes());
        seq.push(0x07);
        seq
    }

    #[test]
    fn osc1337_inline_png() {
        let seq = tiny_png_osc();
        let ev = events(&seq);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, seq.len());
        match &ev[0].1 {
            OscEvent::InlineImage { png, width, height } => {
                assert_eq!((png.as_slice(), *width, *height), (TINY_PNG, 1, 1));
            },
            other => panic!("expected InlineImage, got {other:?}"),
        }
    }

    #[test]
    fn osc1337_without_inline_ignored() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(TINY_PNG);
        let seq = format!("\x1b]1337;File=name=eC5wbmc=;size=70:{b64}\x07");
        assert!(events(seq.as_bytes()).is_empty());
    }

    #[test]
    fn osc1337_survives_chunk_splits() {
        let seq = tiny_png_osc();
        let mut s = CwdSniffer::default();
        let (a, b) = seq.split_at(20);
        assert!(s.feed(a).is_empty());
        let ev = s.feed(b);
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0].1, OscEvent::InlineImage { width: 1, height: 1, .. }));
    }
}
