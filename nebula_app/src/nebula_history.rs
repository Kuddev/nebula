//! Persistent, indexed command history backing Nebula's fish-style ghost-text
//! hint.
//!
//! Commands are appended to `nebula_history.jsonl` (one `{ts,cwd,cmd}` record
//! per line) and held in memory newest-last. A prefix index keeps the hint
//! lookup at `O(log n + k)` instead of scanning the whole list on every
//! keystroke — important once the history grows into the thousands.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

/// Max commands kept in memory and persisted.
const HISTORY_MAX: usize = 5_000;

/// An indexed, deduplicated command history.
#[derive(Debug, Default)]
pub struct NebulaHistory {
    /// Commands in recency order, oldest first, newest last. Deduplicated:
    /// re-running a command moves it to the end rather than adding a copy.
    entries: Vec<String>,
    /// Prefix index: `command -> position in `entries``. A `BTreeMap` so a
    /// prefix query becomes a range scan over the small matching window rather
    /// than a full linear pass. Values are kept in sync with `entries`.
    index: BTreeMap<String, usize>,
    /// Directory targets extracted from location-changing commands. This powers
    /// a Nu/Reedline-like hint path where `cd D:\te` can complete from an older
    /// `cd D:/temp_build/wuwei` even though the slash style differs.
    dir_entries: Vec<DirHistoryEntry>,
}

#[derive(Debug, Clone)]
struct DirHistoryEntry {
    /// User-visible path as it appeared in history, or as resolved from a
    /// Nebula JSONL `cwd`. Kept for case and slash style when no style was typed.
    display: String,
    /// Comparison key: Windows-style separators and ASCII-folded case.
    normalized: String,
}

impl NebulaHistory {
    /// Load history from disk, newest-last, capped at [`HISTORY_MAX`].
    pub fn load() -> Self {
        let mut history = Self::default();
        history.load_nebula_history();
        let recent_dirs: Vec<_> =
            history.dir_entries.iter().rev().take(8).map(|entry| entry.display.clone()).collect();
        history_debug_log(format!(
            "history_load entries={} dir_entries={} recent_dirs={recent_dirs:?}",
            history.entries.len(),
            history.dir_entries.len()
        ));
        history
    }

    /// Record a freshly run command: persist it and update the in-memory index.
    /// No-ops for blank input or an immediate repeat of the last command.
    pub fn record(&mut self, cmd: &str, cwd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if self.entries.last().map(String::as_str) == Some(cmd) {
            history_debug_log(format!("history_record_skip_repeat cmd={cmd:?} cwd={cwd:?}"));
            return;
        }
        append(cmd, cwd);
        self.insert_with_cwd(cmd.to_owned(), Some(cwd));
        history_debug_log(format!(
            "history_record cmd={cmd:?} cwd={cwd:?} entries={} dir_entries={}",
            self.entries.len(),
            self.dir_entries.len()
        ));
    }

    /// The newest command that begins with `prefix` and strictly extends it,
    /// returning only the remainder (the part past `prefix`). `None` when
    /// nothing matches. Uses the prefix index, so cost scales with the number
    /// of matches, not the history size.
    pub fn hint(&self, prefix: &str) -> Option<&str> {
        if prefix.is_empty() {
            return None;
        }
        // All keys with `prefix` as a prefix form a contiguous BTreeMap range
        // `[prefix, prefix++)`, where `prefix++` is `prefix` with its last code
        // unit bumped. Scan that window and keep the most-recent (highest pos).
        let mut best: Option<(usize, &str)> = None;
        for (cmd, &pos) in self.index.range(prefix.to_owned()..) {
            if !cmd.starts_with(prefix) {
                break;
            }
            if cmd.len() == prefix.len() {
                continue; // exact match — nothing to hint
            }
            if best.is_none_or(|(bp, _)| pos > bp) {
                best = Some((pos, &cmd[prefix.len()..]));
            }
        }
        best.map(|(_, rem)| rem)
    }

    /// Directory-target hint for cd-like commands.
    ///
    /// Unlike [`hint`](Self::hint), this compares normalized path prefixes, so
    /// Windows users can type backslashes while history contains forward slashes
    /// (or different drive-letter casing) and still get the historical target.
    pub fn dir_hint(&self, line: &str) -> Option<String> {
        let request = parse_cd_request(line)?;
        let request_norm = normalize_dir_key(&request.target, false)?;
        if request_norm.is_empty() {
            return None;
        }

        let mut matched_display = None;
        let result = self.dir_entries.iter().rev().find_map(|entry| {
            if entry.normalized.len() <= request_norm.len()
                || !entry.normalized.starts_with(&request_norm)
            {
                return None;
            }

            let start = request_norm.len();
            let source = if entry.display.is_char_boundary(start) {
                &entry.display[start..]
            } else if entry.normalized.is_char_boundary(start) {
                &entry.normalized[start..]
            } else {
                return None;
            };
            matched_display = Some(entry.display.clone());
            Some(apply_separator_style(source, request.separator))
        });
        history_debug_log(format!(
            "dir_hint line={line:?} target={:?} norm={request_norm:?} dir_entries={} matched={matched_display:?} rem={result:?}",
            request.target,
            self.dir_entries.len()
        ));
        result
    }

    /// Recency rank of directory `path` among recently visited directories.
    ///
    /// Returns `Some(0)` when `path` is the most-recently visited directory (or
    /// an ancestor of one), larger values for older visits, and `None` when it
    /// was never visited. Reuses the same normalized directory history as
    /// [`dir_hint`](Self::dir_hint), so path completions can prefer directories
    /// the user actually goes to over the alphabetically-first sibling on disk.
    ///
    /// Matching respects path-segment boundaries: `D:\temp` does not count as
    /// visited just because `D:\temp_build\...` was — only an exact directory or
    /// a true ancestor (followed by a separator) qualifies.
    pub fn dir_rank(&self, path: &str) -> Option<usize> {
        let key = normalize_dir_key(path, true)?;
        if key.is_empty() {
            return None;
        }
        self.dir_entries.iter().rev().position(|entry| {
            let normalized = &entry.normalized;
            *normalized == key
                || normalized.strip_prefix(&key).is_some_and(|rest| rest.starts_with('\\'))
        })
    }

    /// Insert a command, deduplicating and rebuilding the index when an old
    /// copy is displaced, and trimming to [`HISTORY_MAX`].
    fn insert(&mut self, cmd: String) {
        self.insert_with_cwd(cmd, None);
    }

    fn insert_with_cwd(&mut self, cmd: String, cwd: Option<&str>) {
        self.insert_dir_from_command(&cmd, cwd);

        if let Some(&old) = self.index.get(&cmd) {
            // Move an existing command to the front of recency: drop the old
            // slot and re-push. Positions after it shift, so reindex.
            self.entries.remove(old);
            self.entries.push(cmd);
            self.reindex();
            return;
        }
        self.entries.push(cmd.clone());
        let pos = self.entries.len() - 1;
        self.index.insert(cmd, pos);

        if self.entries.len() > HISTORY_MAX {
            let drop = self.entries.len() - HISTORY_MAX;
            self.entries.drain(0..drop);
            self.reindex();
        }
    }

    fn insert_dir_from_command(&mut self, cmd: &str, cwd: Option<&str>) {
        let Some(target) = parse_cd_target(cmd) else {
            return;
        };
        let Some(display) = history_dir_display(&target, cwd) else {
            return;
        };
        let Some(normalized) = normalize_dir_key(&display, true) else {
            return;
        };
        if normalized.is_empty() {
            return;
        }

        if let Some(pos) = self.dir_entries.iter().position(|entry| entry.normalized == normalized)
        {
            self.dir_entries.remove(pos);
        }
        self.dir_entries.push(DirHistoryEntry { display, normalized });

        if self.dir_entries.len() > HISTORY_MAX {
            let drop = self.dir_entries.len() - HISTORY_MAX;
            self.dir_entries.drain(0..drop);
        }
    }

    /// Rebuild the prefix index from `entries`. Called only on the rare
    /// dedup/trim paths, not on the common append.
    fn reindex(&mut self) {
        self.index.clear();
        for (i, cmd) in self.entries.iter().enumerate() {
            self.index.insert(cmd.clone(), i);
        }
    }

    fn load_nebula_history(&mut self) {
        if let Ok(data) = std::fs::read_to_string(history_path()) {
            for line in data.lines() {
                if let Some((cmd, cwd)) = parse_record(line) {
                    self.insert_with_cwd(cmd, cwd.as_deref());
                }
            }
        }
    }
}

/// Path to `nebula_history.jsonl` under the user data dir, creating the
/// directory if needed.
fn history_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Nebula");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("nebula_history.jsonl")
}

#[cfg(test)]
fn history_debug_log(_message: impl AsRef<str>) {}

#[cfg(not(test))]
fn history_debug_log(message: impl AsRef<str>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        std::env::var("NEBULA_DEBUG_LOG").is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    }) {
        return;
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    if let Some(dir) = history_path().parent().map(PathBuf::from) {
        let path = dir.join("nebula_debug.log");
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "[{ts}] {}", message.as_ref());
        }
    }
}

#[derive(Debug, Clone)]
struct CdRequest {
    target: String,
    separator: Option<char>,
}

fn parse_cd_request(line: &str) -> Option<CdRequest> {
    let target = parse_cd_target(line)?;
    let separator = target.chars().rev().find(|ch| matches!(ch, '/' | '\\'));
    Some(CdRequest { target, separator })
}

fn parse_cd_target(line: &str) -> Option<String> {
    let line = line.trim_start();
    let (command, rest) = split_first_token(line)?;
    if !is_location_command(command) {
        return None;
    }

    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }

    // Keep the first version deliberately conservative: options such as
    // `Set-Location -Path foo` can be added once the basic history path proves
    // stable. Treating `-Path` as a directory would make noisy hints worse.
    if rest.starts_with('-') {
        return None;
    }

    split_first_token(rest).map(|(target, _)| target.to_owned())
}

fn split_first_token(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    if input.is_empty() {
        return None;
    }

    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '"' | '\'' | '`') {
        let start = first.len_utf8();
        for (idx, ch) in chars {
            if ch == first {
                return Some((&input[start..idx], &input[idx + ch.len_utf8()..]));
            }
        }
        return Some((&input[start..], ""));
    }

    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            return Some((&input[..idx], &input[idx..]));
        }
    }
    Some((input, ""))
}

fn is_location_command(command: &str) -> bool {
    matches!(
        command.to_ascii_lowercase().as_str(),
        "cd" | "chdir" | "pushd" | "sl" | "set-location"
    )
}

fn history_dir_display(target: &str, cwd: Option<&str>) -> Option<String> {
    let target = target.trim();
    if target.is_empty()
        || target == "-"
        || target.starts_with(['$', '%'])
        || target.contains(['*', '?'])
    {
        return None;
    }

    if let Some(expanded) = expand_home(target) {
        return Some(expanded);
    }

    if is_absolute_like(target) {
        return Some(trim_trailing_separators(target.to_owned()));
    }

    let cwd = cwd?.trim();
    if cwd.is_empty() {
        return None;
    }
    Some(trim_trailing_separators(PathBuf::from(cwd).join(target).display().to_string()))
}

fn expand_home(target: &str) -> Option<String> {
    let rest = target.strip_prefix('~')?;
    if !rest.is_empty() && !rest.starts_with(['/', '\\']) {
        return None;
    }

    let home =
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)?;
    Some(trim_trailing_separators(
        home.join(rest.trim_start_matches(['/', '\\'])).display().to_string(),
    ))
}

fn is_absolute_like(path: &str) -> bool {
    path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':')
}

fn normalize_dir_key(path: &str, trim_trailing: bool) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        let ch = if matches!(ch, '/' | '\\') { '\\' } else { ch };
        out.push(ch.to_ascii_lowercase());
    }

    if trim_trailing {
        out = trim_trailing_separators(out);
    }
    Some(out)
}

fn trim_trailing_separators(mut path: String) -> String {
    while path.len() > root_len(&path) && path.ends_with(['/', '\\']) {
        path.pop();
    }
    path
}

fn root_len(path: &str) -> usize {
    if path.as_bytes().get(1) == Some(&b':') {
        3.min(path.len())
    } else if path.starts_with(['/', '\\']) {
        1
    } else {
        0
    }
}

fn apply_separator_style(rem: &str, separator: Option<char>) -> String {
    let Some(separator) = separator else {
        return rem.to_owned();
    };
    rem.chars().map(|ch| if matches!(ch, '/' | '\\') { separator } else { ch }).collect()
}

/// Append one `{ts,cwd,cmd}` JSONL record. Best-effort; failures are ignored so
/// persistence never blocks the prompt.
fn append(cmd: &str, cwd: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record = serde_json::json!({ "ts": ts, "cwd": cwd, "cmd": cmd });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(history_path()) {
        let _ = writeln!(f, "{record}");
    }
}

/// Extract the `cmd` and optional `cwd` fields from one JSONL line, skipping
/// malformed lines.
fn parse_record(line: &str) -> Option<(String, Option<String>)> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let cmd = value.get("cmd")?.as_str()?.to_owned();
    let cwd = value.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
    Some((cmd, cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(cmds: &[&str]) -> NebulaHistory {
        let mut h = NebulaHistory::default();
        for c in cmds {
            h.insert((*c).to_owned());
        }
        h
    }

    #[test]
    fn hint_returns_remainder_of_newest_match() {
        let h = hist(&["cargo build", "cargo test", "git status"]);
        assert_eq!(h.hint("cargo "), Some("test"));
        assert_eq!(h.hint("git "), Some("status"));
    }

    #[test]
    fn no_hint_for_exact_or_missing() {
        let h = hist(&["cargo build"]);
        assert_eq!(h.hint("cargo build"), None); // exact, nothing to add
        assert_eq!(h.hint("npm "), None); // no match
        assert_eq!(h.hint(""), None); // empty prefix
    }

    #[test]
    fn dedup_moves_to_newest() {
        // Re-running "ls" should make it win over the later "ll" for prefix "l".
        let h = hist(&["ls", "ll", "ls"]);
        assert_eq!(h.hint("l"), Some("s"));
    }

    #[test]
    fn prefix_range_does_not_bleed() {
        let h = hist(&["git push", "gitk"]);
        // "git " must not match "gitk" (no space).
        assert_eq!(h.hint("git "), Some("push"));
    }

    #[test]
    fn dir_hint_matches_slash_history_from_backslash_input() {
        let h = hist(&["cd D:/temp_build/wuwei"]);

        assert_eq!(h.dir_hint("cd D:\\te").as_deref(), Some("mp_build\\wuwei"));
        assert_eq!(h.dir_hint("cd D:/te").as_deref(), Some("mp_build/wuwei"));
        assert_eq!(h.dir_hint("cd D").as_deref(), Some(":/temp_build/wuwei"));
    }

    #[test]
    fn dir_hint_uses_newest_directory_match() {
        let h = hist(&["cd D:/temp", "cd D:/temp_build/wuwei"]);

        assert_eq!(h.dir_hint("cd D:\\te").as_deref(), Some("mp_build\\wuwei"));
    }

    #[test]
    fn dir_hint_resolves_relative_nebula_history_against_cwd() {
        let mut h = NebulaHistory::default();
        h.insert_with_cwd("cd wuwei".to_owned(), Some("D:\\temp_build"));

        assert_eq!(h.dir_hint("cd D:\\temp_build\\w").as_deref(), Some("uwei"));
    }

    #[test]
    fn dir_hint_ignores_non_location_commands() {
        let h = hist(&["echo D:/temp_build/wuwei"]);

        assert_eq!(h.dir_hint("echo D:/te"), None);
    }

    #[test]
    fn dir_rank_prefers_recently_visited_directory() {
        // Telegram visited first, then temp_build (as the parent of wuwei).
        let h = hist(&["cd D:/Telegram", "cd D:/temp_build/wuwei"]);
        let temp_build = h.dir_rank("D:\\temp_build\\").expect("temp_build visited");
        let telegram = h.dir_rank("D:\\Telegram").expect("Telegram visited");
        assert!(
            temp_build < telegram,
            "temp_build (rank {temp_build}) should outrank Telegram (rank {telegram})"
        );
    }

    #[test]
    fn dir_rank_none_for_unvisited_and_respects_segment_boundaries() {
        let h = hist(&["cd D:/temp_build/wuwei"]);
        assert_eq!(h.dir_rank("D:\\never_here"), None);
        // "D:\temp" must not count as visited just because "temp_build" was.
        assert_eq!(h.dir_rank("D:\\temp"), None);
        assert!(h.dir_rank("D:\\temp_build").is_some());
        assert!(h.dir_rank("D:\\temp_build\\wuwei").is_some());
    }
}
