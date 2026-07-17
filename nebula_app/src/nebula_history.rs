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

/// Max commands kept in memory.
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
}

impl NebulaHistory {
    /// Load history from disk, newest-last, capped at [`HISTORY_MAX`].
    pub fn load() -> Self {
        let mut history = Self::default();
        history.load_nebula_history();
        history_debug_log(format!("history_load entries={}", history.entries.len()));
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
        self.insert(cmd.to_owned());
        history_debug_log(format!(
            "history_record cmd={cmd:?} cwd={cwd:?} entries={}",
            self.entries.len()
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

    /// Insert a command, deduplicating and rebuilding the index when an old
    /// copy is displaced, and trimming to [`HISTORY_MAX`].
    fn insert(&mut self, cmd: String) {
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
                if let Some((cmd, _cwd)) = parse_record(line) {
                    self.insert(cmd);
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
}
