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

/// Cap for OSC 1337 inline-image payloads (metadata plus base64). Anything
/// larger is dropped while it is still arriving rather than buffered forever.
const MAX_IMAGE_PAYLOAD: usize = 12 * 1024 * 1024;
/// A compressed image can expand by orders of magnitude. Keep a 4K screenshot
/// comfortably inside the budget, but reject image bombs before a decoder sees
/// them. The frontend repeats this check as a defense-in-depth boundary.
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
/// 远端 Hook JSON 使用 Base64 传输；上限阻止伪造 OSC 长时间占用内存。
const MAX_HOOK_PAYLOAD: usize = 96 * 1024;

/// An OSC event recognized by the sniffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    /// OSC 7 / 9;9 — the shell reported its working directory (native path).
    Cwd(String),
    /// OSC 133;A — the shell is about to draw a prompt (semantic zone start).
    PromptMark,
    /// OSC 133;C — a command started executing.
    CommandStart,
    /// OSC 133;D — the command finished. `exit_code` is the first parameter
    /// (`133;D;<code>[;aid=…]`), reported by Nebula's own shell integration;
    /// third-party integrations that send a bare `133;D` yield `None`.
    CommandDone { exit_code: Option<i32> },
    /// OSC 1337 `SetUserVar=<name>=<b64>` — a shell-integration variable
    /// (the OSC 1337 shell-integration convention). Carries Nebula queries
    /// (`nebula_ai_query`) from the `#`-line interception, among others.
    UserVar { name: String, value: String },
    /// OSC 9 — free-text program notification (iTerm style).
    Notify(String),
    /// OSC 9;4 — ConEmu 任务进度。`state` 是原始状态码，`value` 是 0..=100 的
    /// 百分比（只有 state 1 和 4 带值）。
    ///
    /// ConEmu 只定义 0 清除 / 1 正常 / 2 错误 / 3 不确定 / 4 暂停。这里不在解析
    /// 层做语义收窄：部分 shell 集成实测会用规范外的 `9;4;5;0` 表示「成功完成」，
    /// 把未知码当成非法而丢掉，等于让进度条卡在最后一个状态上
    /// 永远不消失。映射交给消费端。
    Progress { state: u8, value: Option<u8> },
    /// Nebula 远端 Hook 私有 OSC：随机通道令牌 + 原始 Hook 信封。
    RemoteHook { token: String, envelope: Vec<u8> },
    /// OSC 1337 `File=...inline=1:<base64>` — an iTerm2 inline image.
    /// Only static PNG/JPEG/GIF input is accepted; animated GIFs are rendered
    /// as their first frame by the frontend.
    /// `width`/`height` come from the encoded image header, in pixels.
    InlineImage { data: Vec<u8>, width: u32, height: u32 },
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
                        self.reset_to_ground();
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
                self.reset_to_ground();
            },
            0x1b => self.phase = Phase::OscEsc,
            _ => self.push(b),
        }
    }

    fn reset_to_ground(&mut self) {
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
        let cap = if self.payload.starts_with(b"1337;") {
            MAX_IMAGE_PAYLOAD
        } else if self.payload.starts_with(b"777;nebula-hook;") {
            MAX_HOOK_PAYLOAD
        } else {
            MAX_PAYLOAD
        };
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
            if let Some(var) = rest.strip_prefix(b"SetUserVar=") {
                return parse_user_var(var);
            }
            return parse_osc1337_image(rest);
        }
        if let Some(rest) = self.payload.strip_prefix(b"777;nebula-hook;") {
            return parse_remote_hook(rest);
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
                // `D;<code>[;aid=…]` — take the first parameter when it is a
                // plain integer; a bare `D` or junk parameter reports None.
                let exit_code = rest
                    .get(2..)
                    .map(|params| params.split(|&b| b == b';').next().unwrap_or(params))
                    .and_then(|first| std::str::from_utf8(first).ok())
                    .and_then(|first| first.trim().parse::<i32>().ok());
                return Some(OscEvent::CommandDone { exit_code });
            }
            return None;
        }
        if let Some(rest) = self.payload.strip_prefix(b"9;") {
            // OSC 9 family. `9;9;` (cwd) matched above; `9;4;` is ConEmu
            // progress; anything else is an iTerm-style text notification.
            if let Some(progress) = rest.strip_prefix(b"4;") {
                let mut fields = progress.split(|&b| b == b';');
                let state = fields
                    .next()
                    .and_then(|field| std::str::from_utf8(field).ok())
                    .and_then(|field| field.trim().parse::<u8>().ok())?;
                let value = fields
                    .next()
                    .and_then(|field| std::str::from_utf8(field).ok())
                    .and_then(|field| field.trim().parse::<u8>().ok())
                    .map(|percent| percent.min(100));
                return Some(OscEvent::Progress { state, value });
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
    const E: &[u8] = b"777;";
    let matches = |target: &[u8]| target.starts_with(payload) || payload.starts_with(target);
    matches(A) || matches(B) || matches(C) || matches(D) || matches(E)
}

/// Parse a `SetUserVar=<name>=<base64>` body. Names are restricted to the
/// word-character set every emitter in the wild uses; the value must decode
/// to UTF-8 (these are shell-integration strings, not blobs). A var larger
/// than 8 KiB is not a query — reject rather than ferry it around.
fn parse_user_var(rest: &[u8]) -> Option<OscEvent> {
    use base64::Engine as _;

    let eq = rest.iter().position(|&b| b == b'=')?;
    let name = std::str::from_utf8(&rest[..eq]).ok()?;
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(&rest[eq + 1..]).ok()?;
    if decoded.len() > 8 * 1024 {
        return None;
    }
    let value = String::from_utf8(decoded).ok()?;
    Some(OscEvent::UserVar { name: name.to_owned(), value })
}

fn parse_remote_hook(rest: &[u8]) -> Option<OscEvent> {
    use base64::Engine as _;

    let separator = rest.iter().position(|byte| *byte == b';')?;
    let token = std::str::from_utf8(&rest[..separator]).ok()?.trim();
    if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let envelope = base64::engine::general_purpose::STANDARD.decode(&rest[separator + 1..]).ok()?;
    if envelope.is_empty() || envelope.len() > 64 * 1024 {
        return None;
    }
    Some(OscEvent::RemoteHook { token: token.to_owned(), envelope })
}

/// Parse an OSC 1337 body (`File=key=value;...:<base64>`) into an inline
/// image event. Only `inline=1` PNG/JPEG/GIF payloads are accepted;
/// `width`/`height` come from the encoded file rather than terminal params, so
/// broken or hostile metadata cannot lie about the allocation size.
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

    let data = base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| {
            // Some emitters wrap base64 in whitespace/newlines; strip and retry.
            let cleaned: Vec<u8> =
                data.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            base64::engine::general_purpose::STANDARD.decode(&cleaned)
        })
        .ok()?;

    let (width, height) = image_dimensions(&data)?;
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    if pixels > MAX_IMAGE_PIXELS {
        return None;
    }
    Some(OscEvent::InlineImage { data, width, height })
}

/// Read dimensions from the supported formats without allocating their pixel
/// buffers. This is deliberately a small sniffer, not a decoder: the frontend
/// still validates and decodes the bytes in a bounded background job.
fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    png_dimensions(data).or_else(|| gif_dimensions(data)).or_else(|| jpeg_dimensions(data))
}

fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || !png.starts_with(MAGIC) || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    valid_dimensions(width, height)
}

fn gif_dimensions(gif: &[u8]) -> Option<(u32, u32)> {
    if gif.len() < 10 || !(gif.starts_with(b"GIF87a") || gif.starts_with(b"GIF89a")) {
        return None;
    }
    let width = u16::from_le_bytes(gif[6..8].try_into().ok()?) as u32;
    let height = u16::from_le_bytes(gif[8..10].try_into().ok()?) as u32;
    valid_dimensions(width, height)
}

fn jpeg_dimensions(jpeg: &[u8]) -> Option<(u32, u32)> {
    if jpeg.len() < 4 || !jpeg.starts_with(&[0xff, 0xd8]) {
        return None;
    }

    let mut offset = 2;
    while offset < jpeg.len() {
        while jpeg.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *jpeg.get(offset)?;
        offset += 1;

        // Standalone markers have no length field.
        if marker == 0xd8 || marker == 0xd9 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }

        let segment_len =
            u16::from_be_bytes(jpeg.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if segment_len < 2 || offset.checked_add(segment_len)? > jpeg.len() {
            return None;
        }

        let is_start_of_frame = matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        );
        if is_start_of_frame {
            if segment_len < 7 {
                return None;
            }
            let height =
                u16::from_be_bytes(jpeg.get(offset + 3..offset + 5)?.try_into().ok()?) as u32;
            let width =
                u16::from_be_bytes(jpeg.get(offset + 5..offset + 7)?.try_into().ok()?) as u32;
            return valid_dimensions(width, height);
        }

        // Start-of-scan is followed by entropy-coded data; a valid dimensions
        // marker must have appeared before it.
        if marker == 0xda {
            return None;
        }
        offset += segment_len;
    }
    None
}

fn valid_dimensions(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS)
        .then_some((width, height))
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
        assert_eq!(one(b"\x1b]7;file:///C:/Users/foo\x07").as_deref(), Some("C:/Users/foo"));
    }

    #[test]
    fn osc7_unix_with_host_and_st() {
        assert_eq!(one(b"\x1b]7;file://host/home/user\x1b\\").as_deref(), Some("/home/user"));
    }

    #[test]
    fn osc7_percent_encoded_space() {
        assert_eq!(one(b"\x1b]7;file:///C:/My%20Docs\x07").as_deref(), Some("C:/My Docs"));
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
        assert_eq!(
            events(b"\x1b]133;D;0\x07"),
            vec![(10, OscEvent::CommandDone { exit_code: Some(0) })]
        );
    }

    #[test]
    fn osc133_done_exit_code_variants() {
        // Bare D (third-party integrations): finished, code unknown.
        assert_eq!(events(b"\x1b]133;D\x07"), vec![(8, OscEvent::CommandDone { exit_code: None })]);
        // Trailing params after the code (some terminals send `;aid=<pid>`).
        assert_eq!(
            events(b"\x1b]133;D;127;aid=4242\x07"),
            vec![(21, OscEvent::CommandDone { exit_code: Some(127) })]
        );
        // Windows STATUS_CONTROL_C_EXIT is negative in i32 — must round-trip.
        assert_eq!(
            events(b"\x1b]133;D;-1073741510\x07"),
            vec![(20, OscEvent::CommandDone { exit_code: Some(-1073741510) })]
        );
        // Junk parameter degrades to None, not a dropped event.
        assert_eq!(
            events(b"\x1b]133;D;abc\x07"),
            vec![(12, OscEvent::CommandDone { exit_code: None })]
        );
    }

    #[test]
    fn osc1337_set_user_var() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode("[mode:auto] kill port 3000");
        let seq = format!("\x1b]1337;SetUserVar=nebula_ai_query={b64}\x07");
        assert_eq!(
            events(seq.as_bytes()),
            vec![(
                seq.len(),
                OscEvent::UserVar {
                    name: "nebula_ai_query".into(),
                    value: "[mode:auto] kill port 3000".into(),
                }
            )]
        );
        // Bad base64 and hostile names are dropped, not passed through.
        assert!(events(b"\x1b]1337;SetUserVar=x=!!!\x07").is_empty());
        assert!(events(b"\x1b]1337;SetUserVar=bad name=QUJD\x07").is_empty());
    }

    #[test]
    fn osc9_text_notification() {
        assert_eq!(
            events(b"\x1b]9;build done\x07"),
            vec![(15, OscEvent::Notify("build done".into()))]
        );
        // `9;9;` (cwd) must keep precedence over the free-text notification.
        assert_eq!(one(b"\x1b]9;9;C:\\w\x07").as_deref(), Some("C:\\w"));
    }

    /// OSC 9;4 是 ConEmu 的任务进度，不是文本通知。状态码原样带出去，规范外的
    /// 码也要带（部分 shell 集成会用 `9;4;5;0` 表示成功完成）：在解析层判非法
    /// 会让进度条永远停在最后一个状态上，语义收窄留给消费端。
    #[test]
    fn osc9_4_reports_conemu_progress() {
        assert_eq!(
            events(b"\x1b]9;4;1;50\x07"),
            vec![(11, OscEvent::Progress { state: 1, value: Some(50) })]
        );
        assert_eq!(
            events(b"\x1b]9;4;3\x07"),
            vec![(8, OscEvent::Progress { state: 3, value: None })]
        );
        assert_eq!(
            events(b"\x1b]9;4;5;0\x07"),
            vec![(10, OscEvent::Progress { state: 5, value: Some(0) })]
        );
        // 状态码读不出来就不是一条进度事件。
        assert!(events(b"\x1b]9;4;x\x07").is_empty());
    }

    #[test]
    fn remote_hook_requires_valid_token_and_base64() {
        use base64::Engine as _;
        let envelope = b"nebula-hook/1 source=codex pane=999\n{\"type\":\"agent-turn-complete\"}";
        let encoded = base64::engine::general_purpose::STANDARD.encode(envelope);
        let seq = format!("\x1b]777;nebula-hook;0123456789abcdef0123456789abcdef;{encoded}\x07");
        assert_eq!(
            events(seq.as_bytes()),
            vec![(
                seq.len(),
                OscEvent::RemoteHook {
                    token: "0123456789abcdef0123456789abcdef".into(),
                    envelope: envelope.to_vec(),
                }
            )]
        );
        assert!(events(b"\x1b]777;nebula-hook;short;AAAA\x07").is_empty());
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
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn tiny_png_osc() -> Vec<u8> {
        inline_image_osc(TINY_PNG)
    }

    fn inline_image_osc(data: &[u8]) -> Vec<u8> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
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
            OscEvent::InlineImage { data, width, height } => {
                assert_eq!((data.as_slice(), *width, *height), (TINY_PNG, 1, 1));
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

    #[test]
    fn osc1337_accepts_jpeg_and_gif_headers() {
        // Minimal SOF0 segment declaring a 3x2 image. The protocol layer only
        // sniffs dimensions; the frontend decoder still rejects truncated data.
        let jpeg = [
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x02, 0x00, 0x03, 0x03, 0x01, 0x11,
            0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00,
        ];
        let jpeg_events = events(&inline_image_osc(&jpeg));
        assert!(matches!(
            jpeg_events.as_slice(),
            [(_, OscEvent::InlineImage { width: 3, height: 2, .. })]
        ));

        let gif = b"GIF89a\x04\x00\x05\x00";
        let gif_events = events(&inline_image_osc(gif));
        assert!(matches!(
            gif_events.as_slice(),
            [(_, OscEvent::InlineImage { width: 4, height: 5, .. })]
        ));
    }

    #[test]
    fn osc1337_rejects_video_unknown_formats_and_pixel_bombs() {
        assert!(events(&inline_image_osc(b"\0\0\0\x18ftypmp42not-an-image")).is_empty());

        let mut oversized = TINY_PNG.to_vec();
        oversized[16..20].copy_from_slice(&5000u32.to_be_bytes());
        oversized[20..24].copy_from_slice(&5000u32.to_be_bytes());
        assert!(events(&inline_image_osc(&oversized)).is_empty());
    }
}
