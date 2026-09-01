//! Persistent, scoped command history backing Nebula's fish-style suggestions.
//!
//! Local Windows shells share one pool. WSL commands are isolated by distro,
//! and SSH commands by destination, so a valid command on one machine never
//! becomes a misleading suggestion on another.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;

const HISTORY_MAX: usize = 5_000;
pub(crate) const LOCAL_HISTORY_FILE: &str = "nebula_history.jsonl";
pub(crate) const WSL_HISTORY_FILE: &str = "nebula_history_wsl.jsonl";
pub(crate) const SSH_HISTORY_FILE: &str = "nebula_history_ssh.jsonl";

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum HistoryScope {
    #[default]
    Local,
    Wsl(String),
    Ssh(String),
}

impl HistoryScope {
    fn normalized(self) -> Self {
        match self {
            Self::Local => Self::Local,
            Self::Wsl(distro) => Self::Wsl(normalize_identity(&distro)),
            Self::Ssh(destination) => Self::Ssh(normalize_ssh_identity(&destination)),
        }
    }

    fn file_name(&self) -> &'static str {
        match self {
            Self::Local => LOCAL_HISTORY_FILE,
            Self::Wsl(_) => WSL_HISTORY_FILE,
            Self::Ssh(_) => SSH_HISTORY_FILE,
        }
    }

    fn identity(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Wsl(identity) | Self::Ssh(identity) => Some(identity),
        }
    }
}

fn normalize_identity(identity: &str) -> String {
    identity.trim().to_ascii_lowercase()
}

fn normalize_ssh_identity(destination: &str) -> String {
    let destination = destination.trim();
    match destination.rsplit_once('@') {
        Some((user, host)) => format!("{user}@{}", host.to_ascii_lowercase()),
        None => destination.to_ascii_lowercase(),
    }
}

#[derive(Debug, Default)]
struct HistoryIndex {
    entries: Vec<String>,
    index: BTreeMap<String, usize>,
}

impl HistoryIndex {
    fn hint(&self, prefix: &str) -> Option<&str> {
        if prefix.is_empty() {
            return None;
        }
        let mut best: Option<(usize, &str)> = None;
        for (cmd, &pos) in self.index.range(prefix.to_owned()..) {
            if !cmd.starts_with(prefix) {
                break;
            }
            if cmd.len() == prefix.len() {
                continue;
            }
            if best.is_none_or(|(best_pos, _)| pos > best_pos) {
                best = Some((pos, &cmd[prefix.len()..]));
            }
        }
        best.map(|(_, remainder)| remainder)
    }

    fn hints(&self, prefix: &str, limit: usize) -> Vec<(&str, &str)> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut matches: Vec<(usize, &str)> = Vec::new();
        for (cmd, &pos) in self.index.range(prefix.to_owned()..) {
            if !cmd.starts_with(prefix) {
                break;
            }
            if cmd.len() != prefix.len() {
                matches.push((pos, cmd.as_str()));
            }
        }
        matches.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        matches.into_iter().take(limit).map(|(_, cmd)| (cmd, &cmd[prefix.len()..])).collect()
    }

    fn insert(&mut self, cmd: String) {
        if let Some(&old) = self.index.get(&cmd) {
            self.entries.remove(old);
            self.entries.push(cmd);
            self.reindex();
            return;
        }
        self.entries.push(cmd.clone());
        self.index.insert(cmd, self.entries.len() - 1);
        if self.entries.len() > HISTORY_MAX {
            let drop = self.entries.len() - HISTORY_MAX;
            self.entries.drain(0..drop);
            self.reindex();
        }
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (position, command) in self.entries.iter().enumerate() {
            self.index.insert(command.clone(), position);
        }
    }
}

#[derive(Debug, Default)]
pub struct NebulaHistory {
    pools: HashMap<HistoryScope, HistoryIndex>,
}

impl NebulaHistory {
    pub fn load() -> Self {
        let mut history = Self::default();
        history.load_file(&HistoryScope::Local);
        history.load_file(&HistoryScope::Wsl(String::new()));
        history.load_file(&HistoryScope::Ssh(String::new()));
        history_debug_log(format!(
            "history_load scopes={} entries={}",
            history.pools.len(),
            history.pools.values().map(|pool| pool.entries.len()).sum::<usize>()
        ));
        history
    }

    pub fn record(&mut self, scope: &HistoryScope, cmd: &str, cwd: &str) {
        let scope = scope.clone().normalized();
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        let pool = self.pools.entry(scope.clone()).or_default();
        if pool.entries.last().map(String::as_str) == Some(cmd) {
            history_debug_log(format!(
                "history_record_skip_repeat scope={scope:?} cmd={cmd:?} cwd={cwd:?}"
            ));
            return;
        }
        append(&scope, cmd, cwd);
        pool.insert(cmd.to_owned());
        history_debug_log(format!(
            "history_record scope={scope:?} cmd={cmd:?} cwd={cwd:?} entries={}",
            pool.entries.len()
        ));
    }

    pub fn hint(&self, scope: &HistoryScope, prefix: &str) -> Option<&str> {
        self.pools.get(&scope.clone().normalized())?.hint(prefix)
    }

    pub fn hints(&self, scope: &HistoryScope, prefix: &str, limit: usize) -> Vec<(&str, &str)> {
        self.pools
            .get(&scope.clone().normalized())
            .map_or_else(Vec::new, |pool| pool.hints(prefix, limit))
    }

    fn load_file(&mut self, category: &HistoryScope) {
        let path = history_path(category.file_name());
        let Ok(data) = std::fs::read_to_string(path) else { return };
        for line in data.lines() {
            let Some((scope, cmd)) = parse_record(category, line) else { continue };
            self.pools.entry(scope).or_default().insert(cmd);
        }
    }
}

pub(crate) fn history_file_names() -> [&'static str; 3] {
    [LOCAL_HISTORY_FILE, WSL_HISTORY_FILE, SSH_HISTORY_FILE]
}

fn history_path(file_name: &str) -> PathBuf {
    crate::platform::dirs::data_dir().join(file_name)
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
        .map(|duration| format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    let path = history_path(LOCAL_HISTORY_FILE).with_file_name("nebula_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{ts}] {}", message.as_ref());
    }
}

fn append(scope: &HistoryScope, cmd: &str, cwd: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut record = serde_json::json!({ "ts": ts, "cwd": cwd, "cmd": cmd });
    if let Some(identity) = scope.identity() {
        record["scope"] = serde_json::Value::String(
            match scope {
                HistoryScope::Wsl(_) => "wsl",
                HistoryScope::Ssh(_) => "ssh",
                HistoryScope::Local => unreachable!(),
            }
            .to_owned(),
        );
        record["target"] = serde_json::Value::String(identity.to_owned());
    }
    let path = history_path(scope.file_name());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{record}");
    }
}

fn parse_record(category: &HistoryScope, line: &str) -> Option<(HistoryScope, String)> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let cmd = value.get("cmd")?.as_str()?.trim();
    if cmd.is_empty() {
        return None;
    }
    let record_scope = value.get("scope").and_then(serde_json::Value::as_str);
    let target = value.get("target").and_then(serde_json::Value::as_str).unwrap_or_default();
    let scope = match category {
        HistoryScope::Local if record_scope.is_none() => HistoryScope::Local,
        HistoryScope::Wsl(_) if record_scope == Some("wsl") && !target.trim().is_empty() => {
            HistoryScope::Wsl(target.to_owned())
        },
        HistoryScope::Ssh(_) if record_scope == Some("ssh") && !target.trim().is_empty() => {
            HistoryScope::Ssh(target.to_owned())
        },
        _ => return None,
    }
    .normalized();
    Some((scope, cmd.to_owned()))
}

pub(crate) fn record_category_file(line: &str) -> Option<&'static str> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if value.get("cmd")?.as_str()?.trim().is_empty() {
        return None;
    }
    match value.get("scope") {
        None => Some(LOCAL_HISTORY_FILE),
        Some(scope) => {
            let target = value.get("target")?.as_str()?.trim();
            if target.is_empty() {
                return None;
            }
            match scope.as_str()? {
                "wsl" => Some(WSL_HISTORY_FILE),
                "ssh" => Some(SSH_HISTORY_FILE),
                _ => None,
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(scope: HistoryScope, cmds: &[&str]) -> NebulaHistory {
        let mut history = NebulaHistory::default();
        let pool = history.pools.entry(scope.normalized()).or_default();
        for command in cmds {
            pool.insert((*command).to_owned());
        }
        history
    }

    #[test]
    fn hint_returns_remainder_of_newest_match() {
        let scope = HistoryScope::Local;
        let history = hist(scope.clone(), &["cargo build", "cargo test", "git status"]);
        assert_eq!(history.hint(&scope, "cargo "), Some("test"));
        assert_eq!(history.hint(&scope, "git "), Some("status"));
    }

    #[test]
    fn exact_missing_and_empty_prefixes_have_no_hint() {
        let scope = HistoryScope::Local;
        let history = hist(scope.clone(), &["cargo build"]);
        assert_eq!(history.hint(&scope, "cargo build"), None);
        assert_eq!(history.hint(&scope, "npm "), None);
        assert_eq!(history.hint(&scope, ""), None);
    }

    #[test]
    fn rerunning_a_command_moves_it_to_the_newest_position() {
        let scope = HistoryScope::Local;
        let history = hist(scope.clone(), &["ls", "ll", "ls"]);
        assert_eq!(history.hint(&scope, "l"), Some("s"));
    }

    #[test]
    fn prefix_ranges_do_not_bleed_into_adjacent_commands() {
        let scope = HistoryScope::Local;
        let history = hist(scope.clone(), &["git push", "gitk"]);
        assert_eq!(history.hint(&scope, "git "), Some("push"));
    }

    #[test]
    fn scopes_do_not_leak_commands() {
        let local = HistoryScope::Local;
        let wsl = HistoryScope::Wsl("Ubuntu".to_owned());
        let ssh = HistoryScope::Ssh("root@example.com".to_owned());
        let mut history = NebulaHistory::default();
        history
            .pools
            .entry(local.clone().normalized())
            .or_default()
            .insert("dir windows".to_owned());
        history
            .pools
            .entry(wsl.clone().normalized())
            .or_default()
            .insert("sudo apt update".to_owned());
        history
            .pools
            .entry(ssh.clone().normalized())
            .or_default()
            .insert("systemctl restart api".to_owned());

        assert_eq!(history.hint(&local, "dir "), Some("windows"));
        assert_eq!(history.hint(&wsl, "sudo "), Some("apt update"));
        assert_eq!(history.hint(&ssh, "systemctl "), Some("restart api"));
        assert_eq!(history.hint(&local, "sudo "), None);
        assert_eq!(history.hint(&wsl, "systemctl "), None);
    }

    #[test]
    fn remote_identity_is_case_insensitive() {
        let stored = HistoryScope::Wsl("ubuntu".to_owned());
        let queried = HistoryScope::Wsl("Ubuntu".to_owned());
        let history = hist(stored, &["apt update"]);
        assert_eq!(history.hint(&queried, "apt "), Some("update"));
    }

    #[test]
    fn ssh_normalization_preserves_case_sensitive_user_names() {
        assert_eq!(
            HistoryScope::Ssh("BuildUser@EXAMPLE.COM:22".to_owned()).normalized(),
            HistoryScope::Ssh("BuildUser@example.com:22".to_owned())
        );
        assert_ne!(
            HistoryScope::Ssh("BuildUser@example.com".to_owned()).normalized(),
            HistoryScope::Ssh("builduser@example.com".to_owned()).normalized()
        );
    }

    #[test]
    fn hints_list_matches_newest_first_and_skip_exact() {
        let scope = HistoryScope::Local;
        let history = hist(scope.clone(), &["git pull", "git push", "git status", "git p"]);
        assert_eq!(
            history.hints(&scope, "git p", 2),
            vec![("git push", "ush"), ("git pull", "ull")]
        );
    }

    #[test]
    fn sync_records_route_back_to_their_category_files() {
        assert_eq!(record_category_file(r#"{"cmd":"dir"}"#), Some(LOCAL_HISTORY_FILE));
        assert_eq!(
            record_category_file(r#"{"scope":"wsl","target":"ubuntu","cmd":"ls"}"#),
            Some(WSL_HISTORY_FILE)
        );
        assert_eq!(
            record_category_file(r#"{"scope":"ssh","target":"me@box","cmd":"pwd"}"#),
            Some(SSH_HISTORY_FILE)
        );
        assert_eq!(record_category_file(r#"{"scope":"ssh","cmd":"pwd"}"#), None);
        assert_eq!(record_category_file(r#"{"scope":"future","cmd":"pwd"}"#), None);
        assert_eq!(record_category_file(r#"{"cmd":"  "}"#), None);
        assert_eq!(record_category_file("not json"), None);
    }

    #[test]
    fn category_files_reject_mismatched_record_scopes() {
        let wsl_file = HistoryScope::Wsl(String::new());
        let ssh_record = r#"{"scope":"ssh","target":"me@box","cmd":"pwd"}"#;
        assert_eq!(parse_record(&wsl_file, ssh_record), None);

        let local_record = r#"{"cmd":"dir"}"#;
        assert_eq!(
            parse_record(&HistoryScope::Local, local_record),
            Some((HistoryScope::Local, "dir".to_owned()))
        );
    }
}
