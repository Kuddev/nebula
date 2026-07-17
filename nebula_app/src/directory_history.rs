//! Shared directory intelligence derived from successful cwd transitions.
//!
//! Unlike command-text heuristics, this service records only directories the
//! shell actually entered (reported through OSC cwd/title integration). The
//! resulting frecency score is reused by ghost text, path completion and UI
//! directory pickers; no shell-specific command or alias is introduced.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const DATABASE_VERSION: u32 = 1;
const MAX_TOTAL_RANK: f64 = 10_000.0;
const MAX_ENTRIES: usize = 2_048;
const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

#[derive(Clone)]
pub(crate) struct DirectoryHistory {
    state: Arc<Mutex<ServiceState>>,
}

struct ServiceState {
    database: DirectoryDatabase,
    pending: Vec<Observation>,
    /// 首次迁移尚未成功落盘时保留完整种子。后续拿到跨进程锁后，只有在
    /// 磁盘仍无数据库时才采用它，避免和另一个进程已迁移的数据重复计数。
    unsaved_seed: Option<DirectoryDatabase>,
    path: PathBuf,
    persistence_enabled: bool,
}

#[derive(Debug, Clone)]
struct Observation {
    path: String,
    normalized: String,
    at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirectoryDatabase {
    version: u32,
    #[serde(default)]
    entries: Vec<DirectoryEntry>,
}

impl Default for DirectoryDatabase {
    fn default() -> Self {
        Self { version: DATABASE_VERSION, entries: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirectoryEntry {
    path: String,
    rank: f64,
    last_accessed: u64,
}

impl DirectoryEntry {
    fn score(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.last_accessed);
        let recency = if age < HOUR {
            4.0
        } else if age < DAY {
            2.0
        } else if age < WEEK {
            0.5
        } else {
            0.25
        };
        self.rank * recency
    }
}

/// Process-wide service shared by every window and pane.
pub(crate) fn global() -> DirectoryHistory {
    static INSTANCE: OnceLock<DirectoryHistory> = OnceLock::new();
    INSTANCE.get_or_init(DirectoryHistory::load).clone()
}

impl DirectoryHistory {
    fn load() -> Self {
        let path = crate::display::nebula_data_dir().join("directory_history.json");
        let (mut database, mut persistence_enabled, missing) = match read_database(&path) {
            Ok(Some(database)) => (database, true, false),
            Ok(None) => (DirectoryDatabase::default(), true, true),
            Err(err) => {
                log::warn!(
                    "Directory history {} is invalid or unsupported ({err}); using memory only",
                    path.display()
                );
                (DirectoryDatabase::default(), false, false)
            },
        };

        let mut unsaved_seed = None;
        if missing {
            import_legacy_history(
                &mut database,
                &crate::display::nebula_data_dir().join("nebula_history.jsonl"),
            );
            database.age_and_limit(current_time());
            if !database.entries.is_empty() {
                match crate::atomic_file::try_lock(&path) {
                    Ok(Some(_lock)) => match read_database(&path) {
                        // 另一个进程可能刚完成初始化；采用磁盘数据，不能把
                        // 同一份旧 JSONL 再叠加一次。
                        Ok(Some(on_disk)) => database = on_disk,
                        Ok(None) => {
                            if let Err(err) = save_database(&path, &database) {
                                log::warn!("Failed to persist migrated directory history: {err}");
                                unsaved_seed = Some(database.clone());
                            }
                        },
                        Err(err) => {
                            log::warn!("Refusing to replace concurrently-created history: {err}");
                            persistence_enabled = false;
                        },
                    },
                    Ok(None) => unsaved_seed = Some(database.clone()),
                    Err(err) => {
                        log::warn!("Failed to lock migrated directory history: {err}");
                        unsaved_seed = Some(database.clone());
                    },
                }
            }
        }

        Self {
            state: Arc::new(Mutex::new(ServiceState {
                database,
                pending: Vec::new(),
                unsaved_seed,
                path,
                persistence_enabled,
            })),
        }
    }

    /// Record a directory only after the shell reports it as the active cwd.
    pub(crate) fn record(&self, path: &str) -> bool {
        let Some(observation) = observation(path, current_time()) else { return false };
        let mut state = self.state.lock();
        state.database.apply(&observation);
        state.pending.push(observation);
        state.database.age_and_limit(current_time());
        flush(&mut state);
        true
    }

    /// Remainder for a cd-like ghost suggestion, ranked by frecency.
    pub(crate) fn hint(&self, line: &str, cwd: &str) -> Option<String> {
        let request = parse_directory_request(line, cwd)?;
        let request_key = normalize(&request.absolute)?;
        let now = current_time();
        let state = self.state.lock();
        let mut candidates: Vec<&DirectoryEntry> = state
            .database
            .entries
            .iter()
            .filter(|entry| {
                normalize(&entry.path).is_some_and(|key| {
                    key.len() > request_key.len() && key.starts_with(&request_key)
                })
            })
            .collect();
        candidates.sort_by(|left, right| {
            right.score(now).total_cmp(&left.score(now)).then_with(|| left.path.cmp(&right.path))
        });

        candidates.into_iter().take(12).find_map(|entry| {
            if !Path::new(&entry.path).is_dir() {
                return None;
            }
            let suffix = suffix_after_prefix(&entry.path, &request.absolute)?;
            Some(apply_separator_style(suffix, request.separator))
        })
    }

    /// Frecency score for an exact directory or a directory containing a
    /// visited descendant. This lets a frequently used project lift its parent
    /// candidate while completing one path segment at a time.
    pub(crate) fn score(&self, path: &Path) -> Option<f64> {
        let key = normalize_path(path)?;
        let now = current_time();
        let state = self.state.lock();
        state
            .database
            .entries
            .iter()
            .filter_map(|entry| {
                let entry_key = normalize(&entry.path)?;
                (entry_key == key || is_descendant_key(&entry_key, &key)).then(|| entry.score(now))
            })
            .max_by(f64::total_cmp)
    }

    /// General UI candidates, using ordered keyword matching rather than a
    /// product-specific shell command.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<PathBuf> {
        let keywords: Vec<String> = query.split_whitespace().filter_map(normalize).collect();
        let now = current_time();
        let state = self.state.lock();
        let mut entries: Vec<&DirectoryEntry> = state
            .database
            .entries
            .iter()
            .filter(|entry| ordered_keyword_match(&entry.path, &keywords))
            .collect();
        entries.sort_by(|left, right| {
            right.score(now).total_cmp(&left.score(now)).then_with(|| left.path.cmp(&right.path))
        });
        entries
            .into_iter()
            .filter(|entry| Path::new(&entry.path).is_dir())
            .take(limit)
            .map(|entry| PathBuf::from(&entry.path))
            .collect()
    }

    /// Reorder existing filesystem completions without changing their match
    /// set. Unvisited candidates retain the completion engine's stable order.
    pub(crate) fn rank_file_suggestions(
        &self,
        suggestions: Vec<nebula_completions::file::FileSuggestion>,
        cwd: &str,
    ) -> Vec<nebula_completions::file::FileSuggestion> {
        let now = current_time();
        let state = self.state.lock();
        let mut scored: Vec<_> = suggestions
            .into_iter()
            .enumerate()
            .map(|(order, suggestion)| {
                let display = suggestion.display_override.as_deref().unwrap_or(&suggestion.path);
                let score = resolve_candidate(display, cwd)
                    .and_then(|path| state.database.score_path(&path, now));
                (score, order, suggestion)
            })
            .collect();
        scored.sort_by(|left, right| match (left.0, right.0) {
            (Some(left_score), Some(right_score)) => {
                right_score.total_cmp(&left_score).then_with(|| left.1.cmp(&right.1))
            },
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.1.cmp(&right.1),
        });
        scored.into_iter().map(|(_, _, suggestion)| suggestion).collect()
    }
}

impl DirectoryDatabase {
    fn apply(&mut self, observation: &Observation) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            normalize(&entry.path).as_deref() == Some(observation.normalized.as_str())
        }) {
            entry.path.clone_from(&observation.path);
            entry.rank = (entry.rank + 1.0).max(0.0);
            entry.last_accessed = entry.last_accessed.max(observation.at);
        } else {
            self.entries.push(DirectoryEntry {
                path: observation.path.clone(),
                rank: 1.0,
                last_accessed: observation.at,
            });
        }
    }

    fn age_and_limit(&mut self, now: u64) {
        let total = self.entries.iter().map(|entry| entry.rank).sum::<f64>();
        if total > MAX_TOTAL_RANK {
            let factor = 0.9 * MAX_TOTAL_RANK / total;
            for entry in &mut self.entries {
                entry.rank *= factor;
            }
            self.entries.retain(|entry| entry.rank >= 1.0);
        }
        if self.entries.len() > MAX_ENTRIES {
            self.entries.sort_by(|left, right| {
                right
                    .score(now)
                    .total_cmp(&left.score(now))
                    .then_with(|| left.path.cmp(&right.path))
            });
            self.entries.truncate(MAX_ENTRIES);
        }
    }

    fn validate(&self) -> Result<(), String> {
        for entry in &self.entries {
            if normalize(&entry.path).is_none() {
                return Err("directory entry contains an empty or control-character path".into());
            }
            if !entry.rank.is_finite() || entry.rank <= 0.0 {
                return Err(format!("directory entry has invalid rank {}", entry.rank));
            }
        }
        Ok(())
    }

    fn score_path(&self, path: &Path, now: u64) -> Option<f64> {
        let key = normalize_path(path)?;
        self.entries
            .iter()
            .filter_map(|entry| {
                let entry_key = normalize(&entry.path)?;
                (entry_key == key || is_descendant_key(&entry_key, &key)).then(|| entry.score(now))
            })
            .max_by(f64::total_cmp)
    }
}

fn flush(state: &mut ServiceState) {
    if !state.persistence_enabled || (state.pending.is_empty() && state.unsaved_seed.is_none()) {
        return;
    }
    let Ok(Some(_lock)) = crate::atomic_file::try_lock(&state.path) else { return };

    let mut merged = match read_database(&state.path) {
        Ok(Some(database)) => database,
        Ok(None) => state.unsaved_seed.clone().unwrap_or_default(),
        Err(err) => {
            log::warn!("Refusing to overwrite invalid directory history: {err}");
            state.persistence_enabled = false;
            return;
        },
    };
    for observation in &state.pending {
        merged.apply(observation);
    }
    merged.age_and_limit(current_time());
    match save_database(&state.path, &merged) {
        Ok(()) => {
            state.database = merged;
            state.pending.clear();
            state.unsaved_seed = None;
        },
        Err(err) => log::warn!("Failed to save directory history: {err}"),
    }
}

fn read_database(path: &Path) -> Result<Option<DirectoryDatabase>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let database: DirectoryDatabase =
        serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
    if database.version != DATABASE_VERSION {
        return Err(format!(
            "unsupported version {} (expected {DATABASE_VERSION})",
            database.version
        ));
    }
    database.validate()?;
    Ok(Some(database))
}

fn save_database(path: &Path, database: &DirectoryDatabase) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(database).map_err(std::io::Error::other)?;
    crate::atomic_file::write(path, &bytes)
}

fn import_legacy_history(database: &mut DirectoryDatabase, path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(path) = value.get("cwd").and_then(serde_json::Value::as_str) else { continue };
        let at = value.get("ts").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let Some(normalized) = normalize(path) else { continue };
        database.apply(&Observation { path: path.to_owned(), normalized, at });
    }
}

fn observation(path: &str, at: u64) -> Option<Observation> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return None;
    }
    // 数据库明确使用 UTF-8；拒绝无法无损编码的本地路径，不能用
    // to_string_lossy 生成一个之后再也无法访问的伪路径。
    let path = trim_trailing_separators(path.to_str()?.to_owned());
    let normalized = normalize(&path)?;
    Some(Observation { path, normalized, at })
}

fn resolve_candidate(candidate: &str, cwd: &str) -> Option<PathBuf> {
    // display_override 已经是未转义的真实候选；再次 trim 引号或空格会
    // 破坏合法文件名。这里只去掉补全器为目录追加的末尾分隔符。
    let candidate = trim_trailing_separators(candidate.to_owned());
    if candidate.is_empty() {
        return None;
    }
    if let Some(rest) = candidate.strip_prefix('~') {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)?;
        return Some(home.join(rest.trim_start_matches(['/', '\\'])));
    }
    let path = PathBuf::from(&candidate);
    if path.is_absolute() || candidate.as_bytes().get(1) == Some(&b':') {
        Some(path)
    } else if cwd.is_empty() {
        None
    } else {
        Some(PathBuf::from(cwd).join(path))
    }
}

fn current_time() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

fn normalize_path(path: &Path) -> Option<String> {
    normalize(&trim_trailing_separators(path.display().to_string()))
}

fn normalize(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    #[cfg(windows)]
    let value = value.replace('/', "\\").to_ascii_lowercase();
    #[cfg(not(windows))]
    let value = value.to_owned();
    Some(trim_trailing_separators(value))
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

fn is_descendant_key(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .and_then(|rest| rest.chars().next())
        .is_some_and(std::path::is_separator)
}

fn suffix_after_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let byte =
        path.char_indices().nth(prefix.chars().count()).map_or(path.len(), |(index, _)| index);
    path.get(byte..).filter(|suffix| !suffix.is_empty())
}

fn ordered_keyword_match(path: &str, keywords: &[String]) -> bool {
    let Some((last, previous)) = keywords.split_last() else { return true };
    let Some(path) = normalize(path) else { return false };
    let Some(last_index) = path.rfind(last) else { return false };
    if path[last_index + last.len()..].contains(std::path::is_separator) {
        return false;
    }
    let mut head = &path[..last_index];
    for keyword in previous.iter().rev() {
        let Some(index) = head.rfind(keyword) else { return false };
        head = &head[..index];
    }
    true
}

#[derive(Debug)]
struct DirectoryRequest {
    absolute: String,
    separator: Option<char>,
}

fn parse_directory_request(line: &str, cwd: &str) -> Option<DirectoryRequest> {
    let line = line.trim_start();
    let (command, rest) = split_first_token(line)?;
    if !matches!(
        command.to_ascii_lowercase().as_str(),
        "cd" | "chdir" | "pushd" | "sl" | "set-location"
    ) {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('-') {
        return None;
    }
    let (target, _) = split_first_token(rest)?;
    let separator = target.chars().rev().find(|ch| matches!(ch, '/' | '\\'));
    let absolute = resolve_request_path(target, cwd)?;
    Some(DirectoryRequest { absolute, separator })
}

fn resolve_request_path(target: &str, cwd: &str) -> Option<String> {
    if target.is_empty()
        || target == "-"
        || target.starts_with(['$', '%'])
        || target.contains(['*', '?'])
    {
        return None;
    }
    if let Some(rest) = target.strip_prefix('~') {
        if !rest.is_empty() && !rest.starts_with(['/', '\\']) {
            return None;
        }
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)?;
        return Some(trim_trailing_separators(
            home.join(rest.trim_start_matches(['/', '\\'])).display().to_string(),
        ));
    }
    if Path::new(target).is_absolute() || target.as_bytes().get(1) == Some(&b':') {
        return Some(trim_trailing_separators(target.to_owned()));
    }
    if cwd.is_empty() {
        return None;
    }
    Some(trim_trailing_separators(PathBuf::from(cwd).join(target).display().to_string()))
}

fn split_first_token(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '"' | '\'' | '`') {
        let start = first.len_utf8();
        for (index, ch) in chars {
            if ch == first {
                return Some((&input[start..index], &input[index + ch.len_utf8()..]));
            }
        }
        return Some((&input[start..], ""));
    }
    for (index, ch) in input.char_indices() {
        if ch.is_whitespace() {
            return Some((&input[..index], &input[index..]));
        }
    }
    Some((input, ""))
}

fn apply_separator_style(value: &str, separator: Option<char>) -> String {
    let Some(separator) = separator else { return value.to_owned() };
    value.chars().map(|ch| if matches!(ch, '/' | '\\') { separator } else { ch }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[(&str, f64, u64)]) -> DirectoryHistory {
        DirectoryHistory {
            state: Arc::new(Mutex::new(ServiceState {
                database: DirectoryDatabase {
                    version: DATABASE_VERSION,
                    entries: entries
                        .iter()
                        .map(|(path, rank, last_accessed)| DirectoryEntry {
                            path: (*path).to_owned(),
                            rank: *rank,
                            last_accessed: *last_accessed,
                        })
                        .collect(),
                },
                pending: Vec::new(),
                unsaved_seed: None,
                path: PathBuf::new(),
                persistence_enabled: false,
            })),
        }
    }

    #[test]
    fn frequency_and_recency_both_affect_score() {
        let now = 10 * WEEK;
        let frequent = DirectoryEntry { path: "frequent".into(), rank: 20.0, last_accessed: 0 };
        let recent =
            DirectoryEntry { path: "recent".into(), rank: 4.0, last_accessed: now - HOUR / 2 };
        assert_eq!(frequent.score(now), 5.0);
        assert_eq!(recent.score(now), 16.0);
    }

    #[test]
    fn aging_bounds_total_rank_and_removes_noise() {
        let mut database = DirectoryDatabase {
            version: DATABASE_VERSION,
            entries: vec![
                DirectoryEntry { path: "a".into(), rank: 10_000.0, last_accessed: 0 },
                DirectoryEntry { path: "b".into(), rank: 1.0, last_accessed: 0 },
            ],
        };
        database.age_and_limit(0);
        assert!(database.entries.iter().map(|entry| entry.rank).sum::<f64>() < MAX_TOTAL_RANK);
        assert_eq!(database.entries.len(), 1, "low-signal entry ages out");
    }

    #[test]
    fn ordered_keywords_constrain_the_last_path_component() {
        assert!(ordered_keyword_match("D:/temp_build/nebula_app", &["temp".into(), "app".into()]));
        assert!(!ordered_keyword_match("D:/app/nebula/src", &["app".into()]));
    }

    #[test]
    fn parent_score_inherits_a_visited_descendant() {
        let now = current_time();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("workspace").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let history = history(&[(project.to_str().unwrap(), 5.0, now)]);
        assert!(history.score(&temp.path().join("workspace")).is_some());
        assert!(history.score(&temp.path().join("work")).is_none());
    }

    #[test]
    fn hint_uses_frecency_instead_of_insertion_order() {
        let now = current_time();
        let temp = tempfile::tempdir().unwrap();
        let older = temp.path().join("team-old");
        let common = temp.path().join("team-common");
        std::fs::create_dir(&older).unwrap();
        std::fs::create_dir(&common).unwrap();
        let history = history(&[
            (older.to_str().unwrap(), 1.0, now),
            (common.to_str().unwrap(), 10.0, now - DAY),
        ]);
        let prefix = temp.path().join("team-").display().to_string();
        let line = format!("cd {prefix}");
        assert_eq!(history.hint(&line, "").as_deref(), Some("common"));
    }

    #[test]
    fn record_accepts_utf8_spaces_and_rejects_unsafe_or_non_directory_paths() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("项目 空间");
        let file = temp.path().join("plain-file");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(&file, b"file").unwrap();
        let history = history(&[]);

        assert!(history.record(directory.to_str().unwrap()));
        assert!(history.score(&directory).is_some());
        assert_eq!(history.search("项目", 8), vec![directory]);
        assert!(!history.record(file.to_str().unwrap()));
        assert!(!history.record("bad\npath"));
    }

    #[test]
    fn relative_directory_hint_resolves_against_reported_cwd() {
        let now = current_time();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("项目 空间");
        std::fs::create_dir(&project).unwrap();
        let history = history(&[(project.to_str().unwrap(), 5.0, now)]);

        assert_eq!(
            history.hint("cd 项", temp.path().to_str().unwrap()).as_deref(),
            Some("目 空间")
        );
    }

    fn suggestion(path: &str) -> nebula_completions::file::FileSuggestion {
        nebula_completions::file::FileSuggestion {
            span: nebula_completions::Span::new(0, path.len()),
            path: path.to_owned(),
            style: None,
            is_dir: true,
            display_override: None,
            match_indices: Vec::new(),
        }
    }

    #[test]
    fn completion_ranking_promotes_visited_paths_and_keeps_unvisited_order() {
        let now = current_time();
        let temp = tempfile::tempdir().unwrap();
        for name in ["alpha", "visited", "zeta"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let visited = temp.path().join("visited");
        let history = history(&[(visited.to_str().unwrap(), 5.0, now)]);
        let separator = std::path::MAIN_SEPARATOR;
        let ranked = history.rank_file_suggestions(
            vec![
                suggestion(&format!("alpha{separator}")),
                suggestion(&format!("visited{separator}")),
                suggestion(&format!("zeta{separator}")),
            ],
            temp.path().to_str().unwrap(),
        );
        let paths: Vec<_> = ranked.into_iter().map(|item| item.path).collect();

        assert_eq!(
            paths,
            vec![
                format!("visited{separator}"),
                format!("alpha{separator}"),
                format!("zeta{separator}"),
            ]
        );
    }

    #[test]
    fn future_or_unversioned_database_is_rejected_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("directory_history.json");
        let future = br#"{"version":99,"entries":[]}"#;
        std::fs::write(&path, future).unwrap();
        assert!(read_database(&path).unwrap_err().contains("unsupported version"));

        let directory = temp.path().join("valid");
        std::fs::create_dir(&directory).unwrap();
        let pending = observation(directory.to_str().unwrap(), current_time()).unwrap();
        let mut state = ServiceState {
            database: DirectoryDatabase::default(),
            pending: vec![pending],
            unsaved_seed: None,
            path: path.clone(),
            persistence_enabled: true,
        };
        flush(&mut state);
        assert_eq!(std::fs::read(&path).unwrap(), future);
        assert!(!state.persistence_enabled);

        std::fs::write(&path, br#"{"entries":[]}"#).unwrap();
        assert!(read_database(&path).is_err(), "version field must be explicit");
    }

    #[test]
    fn lock_contention_keeps_pending_observations_for_the_next_flush() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("directory_history.json");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let history = DirectoryHistory {
            state: Arc::new(Mutex::new(ServiceState {
                database: DirectoryDatabase::default(),
                pending: Vec::new(),
                unsaved_seed: None,
                path: path.clone(),
                persistence_enabled: true,
            })),
        };

        let lock = crate::atomic_file::try_lock(&path).unwrap().unwrap();
        assert!(history.record(first.to_str().unwrap()));
        assert_eq!(history.state.lock().pending.len(), 1);
        drop(lock);
        assert!(history.record(second.to_str().unwrap()));

        let database = read_database(&path).unwrap().unwrap();
        assert_eq!(database.entries.len(), 2);
        assert!(history.state.lock().pending.is_empty());
    }

    #[test]
    fn unsaved_migration_seed_is_used_only_when_disk_is_still_empty() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("directory_history.json");
        let migrated = temp.path().join("migrated");
        let current = temp.path().join("current");
        std::fs::create_dir(&migrated).unwrap();
        std::fs::create_dir(&current).unwrap();
        let seed = DirectoryDatabase {
            version: DATABASE_VERSION,
            entries: vec![DirectoryEntry {
                path: migrated.to_str().unwrap().to_owned(),
                rank: 3.0,
                last_accessed: 1,
            }],
        };
        let pending = observation(current.to_str().unwrap(), current_time()).unwrap();
        let mut state = ServiceState {
            database: seed.clone(),
            pending: vec![pending],
            unsaved_seed: Some(seed),
            path: path.clone(),
            persistence_enabled: true,
        };

        flush(&mut state);
        let database = read_database(&path).unwrap().unwrap();
        assert_eq!(database.entries.len(), 2);
        assert!(state.unsaved_seed.is_none());
    }

    #[test]
    fn legacy_jsonl_migration_preserves_frequency_and_latest_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nebula_history.jsonl");
        let directory = temp.path().join("legacy");
        let first = serde_json::json!({"cwd": directory, "ts": 10, "cmd": "one"});
        let second = serde_json::json!({"cwd": directory, "ts": 20, "cmd": "two"});
        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        let mut database = DirectoryDatabase::default();

        import_legacy_history(&mut database, &path);
        assert_eq!(database.entries.len(), 1);
        assert_eq!(database.entries[0].rank, 2.0);
        assert_eq!(database.entries[0].last_accessed, 20);
    }

    #[cfg(windows)]
    #[test]
    fn windows_separator_variants_merge_into_one_directory() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().to_str().unwrap();
        let alternate = canonical.replace('\\', "/");
        let mut database = DirectoryDatabase::default();

        database.apply(&observation(canonical, 10).unwrap());
        database.apply(&observation(&alternate, 20).unwrap());
        assert_eq!(database.entries.len(), 1);
        assert_eq!(database.entries[0].rank, 2.0);
        assert_eq!(database.entries[0].last_accessed, 20);
    }
}
