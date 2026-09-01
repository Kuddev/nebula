//! Embedded, cross-platform filename index for the Files drawer.
//!
//! The index follows the drawer root instead of indexing whole disks. It is
//! built on a worker thread, queried in memory, and patched from `notify`
//! events. This gives the drawer Everything-style query latency without a
//! Windows-only executable or a second on-disk database.

use super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

const MAX_INDEX_ENTRIES: usize = 500_000;
const MAX_SEARCH_RESULTS: usize = 1_000;
const WATCH_DEBOUNCE: Duration = Duration::from_millis(80);

const INDEX_IDLE: u8 = 0;
const INDEX_BUILDING: u8 = 1;
const INDEX_READY: u8 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileSearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIndexStatus {
    Idle,
    Building,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileIndexRoot {
    Local(PathBuf),
    Wsl(crate::shell_detect::WslCwd),
}

#[derive(Clone)]
struct SearchRequest {
    generation: u64,
    query: String,
    options: FileSearchOptions,
}

enum IndexCommand {
    Rebuild {
        root: Option<FileIndexRoot>,
        epoch: u64,
        query: Option<SearchRequest>,
    },
    Query(SearchRequest),
    #[cfg(test)]
    ReleaseForTest(mpsc::SyncSender<()>),
}

#[derive(Debug)]
pub(crate) struct FileSearchResult {
    pub(crate) generation: u64,
    pub(crate) query: String,
    pub(crate) options: FileSearchOptions,
    pub(crate) rows: Vec<FileRow>,
    pub(crate) total: usize,
    pub(crate) error: Option<String>,
}

#[derive(Debug)]
struct IndexedPath {
    path: PathBuf,
    guest_path: Option<String>,
    name: String,
    name_folded: String,
    key: String,
    key_folded: String,
    is_dir: bool,
}

/// Handle owned by one drawer model. The worker exits when the handle and its
/// command sender are dropped with the window.
pub(crate) struct EmbeddedFileIndex {
    command_tx: mpsc::Sender<IndexCommand>,
    result: Arc<Mutex<Option<FileSearchResult>>>,
    desired_epoch: Arc<AtomicU64>,
    status: Arc<AtomicU8>,
    indexed_count: Arc<AtomicUsize>,
    truncated: Arc<AtomicBool>,
}

impl EmbeddedFileIndex {
    pub(crate) fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let desired_epoch = Arc::new(AtomicU64::new(0));
        let status = Arc::new(AtomicU8::new(INDEX_IDLE));
        let indexed_count = Arc::new(AtomicUsize::new(0));
        let truncated = Arc::new(AtomicBool::new(false));

        let worker_result = Arc::clone(&result);
        let worker_epoch = Arc::clone(&desired_epoch);
        let worker_status = Arc::clone(&status);
        let worker_count = Arc::clone(&indexed_count);
        let worker_truncated = Arc::clone(&truncated);
        let _ = std::thread::Builder::new().name("nebula file index".to_owned()).spawn(move || {
            run_index_worker(
                command_rx,
                worker_result,
                worker_epoch,
                worker_status,
                worker_count,
                worker_truncated,
            );
        });

        Self { command_tx, result, desired_epoch, status, indexed_count, truncated }
    }

    pub(crate) fn rebuild(
        &self,
        root: Option<FileIndexRoot>,
        epoch: u64,
        query: Option<(u64, String, FileSearchOptions)>,
    ) {
        self.desired_epoch.store(epoch, Ordering::Release);
        let query =
            query.map(|(generation, query, options)| SearchRequest { generation, query, options });
        let _ = self.command_tx.send(IndexCommand::Rebuild { root, epoch, query });
    }

    pub(crate) fn query(&self, generation: u64, query: String, options: FileSearchOptions) {
        let _ =
            self.command_tx.send(IndexCommand::Query(SearchRequest { generation, query, options }));
    }

    pub(crate) fn take_result(&self) -> Option<FileSearchResult> {
        self.result.lock().ok()?.take()
    }

    pub(crate) fn status(&self) -> FileIndexStatus {
        match self.status.load(Ordering::Acquire) {
            INDEX_BUILDING => FileIndexStatus::Building,
            INDEX_READY => FileIndexStatus::Ready,
            _ => FileIndexStatus::Idle,
        }
    }

    pub(crate) fn indexed_count(&self) -> usize {
        self.indexed_count.load(Ordering::Acquire)
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn release_for_test(&self) {
        self.desired_epoch.fetch_add(1, Ordering::AcqRel);
        let (released_tx, released_rx) = mpsc::sync_channel(0);
        self.command_tx
            .send(IndexCommand::ReleaseForTest(released_tx))
            .expect("file-index worker must accept the release command");
        released_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("file-index worker must release its filesystem watcher");
    }
}

fn run_index_worker(
    command_rx: mpsc::Receiver<IndexCommand>,
    result_slot: Arc<Mutex<Option<FileSearchResult>>>,
    desired_epoch: Arc<AtomicU64>,
    status: Arc<AtomicU8>,
    indexed_count: Arc<AtomicUsize>,
    truncated: Arc<AtomicBool>,
) {
    let (watch_tx, watch_rx) = mpsc::channel();
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut root: Option<FileIndexRoot> = None;
    let mut epoch = 0;
    let mut entries = Vec::new();
    let mut current_query: Option<SearchRequest> = None;
    let mut pending_command = None;

    loop {
        let received = pending_command
            .take()
            .map(Ok)
            .unwrap_or_else(|| command_rx.recv_timeout(WATCH_DEBOUNCE));
        match received {
            Ok(IndexCommand::Rebuild { root: next_root, epoch: next_epoch, query }) => {
                root = next_root;
                epoch = next_epoch;
                current_query = query;
                entries.clear();
                watcher = None;
                while watch_rx.try_recv().is_ok() {}
                indexed_count.store(0, Ordering::Release);
                truncated.store(false, Ordering::Release);

                let Some(index_root) = root.as_ref() else {
                    status.store(INDEX_IDLE, Ordering::Release);
                    continue;
                };
                status.store(INDEX_BUILDING, Ordering::Release);

                if let FileIndexRoot::Local(local_root) = index_root {
                    let callback_tx = watch_tx.clone();
                    watcher = notify::recommended_watcher(move |event| {
                        let _ = callback_tx.send(event);
                    })
                    .ok();
                    if let Some(active_watcher) = watcher.as_mut()
                        && let Err(error) =
                            active_watcher.watch(local_root, RecursiveMode::Recursive)
                    {
                        log::debug!(
                            "unable to watch file-search root {}: {error}",
                            local_root.display()
                        );
                        watcher = None;
                    }
                }

                let completed = match index_root {
                    FileIndexRoot::Local(local_root) => build_local_index(
                        local_root,
                        next_epoch,
                        &desired_epoch,
                        &indexed_count,
                        &mut entries,
                    ),
                    FileIndexRoot::Wsl(located) => {
                        build_wsl_index(located, &indexed_count, &mut entries)
                    },
                };
                if !completed || desired_epoch.load(Ordering::Acquire) != next_epoch {
                    continue;
                }
                truncated.store(entries.len() >= MAX_INDEX_ENTRIES, Ordering::Release);
                status.store(INDEX_READY, Ordering::Release);
                if let Some(request) = current_query.as_ref() {
                    publish_search_result(&entries, request, &result_slot);
                }
            },
            Ok(IndexCommand::Query(mut request)) => {
                // A user can type several characters while a large index is
                // being scanned. Keep only the newest queued query, but never
                // consume a root rebuild: save that command for the next loop.
                while let Ok(next) = command_rx.try_recv() {
                    match next {
                        IndexCommand::Query(next_request) => request = next_request,
                        rebuild @ IndexCommand::Rebuild { .. } => {
                            pending_command = Some(rebuild);
                            break;
                        },
                        #[cfg(test)]
                        release @ IndexCommand::ReleaseForTest(_) => {
                            pending_command = Some(release);
                            break;
                        },
                    }
                }
                current_query = Some(request);
                if status.load(Ordering::Acquire) == INDEX_READY
                    && desired_epoch.load(Ordering::Acquire) == epoch
                {
                    publish_search_result(
                        &entries,
                        current_query.as_ref().expect("query was just set"),
                        &result_slot,
                    );
                }
            },
            #[cfg(test)]
            Ok(IndexCommand::ReleaseForTest(released)) => {
                watcher = None;
                root = None;
                entries.clear();
                current_query = None;
                indexed_count.store(0, Ordering::Release);
                truncated.store(false, Ordering::Release);
                status.store(INDEX_IDLE, Ordering::Release);
                let _ = released.send(());
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(FileIndexRoot::Local(local_root)) = root.as_ref() else { continue };
                // Reading the handle here is intentional: keeping the watcher
                // alive is what keeps platform notifications subscribed.
                if watcher.is_none() {
                    continue;
                }
                if status.load(Ordering::Acquire) != INDEX_READY {
                    continue;
                }
                let mut changed_paths = Vec::new();
                while let Ok(event) = watch_rx.try_recv() {
                    match event {
                        Ok(event) if event_changes_names(&event.kind) => {
                            changed_paths.extend(event.paths);
                        },
                        Ok(_) => {},
                        Err(error) => log::debug!("file-search watcher error: {error}"),
                    }
                }
                if changed_paths.is_empty() {
                    continue;
                }
                patch_local_index(
                    local_root,
                    &mut entries,
                    changed_paths,
                    epoch,
                    &desired_epoch,
                    &indexed_count,
                );
                truncated.store(entries.len() >= MAX_INDEX_ENTRIES, Ordering::Release);
                if let Some(request) = current_query.as_ref() {
                    publish_search_result(&entries, request, &result_slot);
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn build_local_index(
    root: &Path,
    epoch: u64,
    desired_epoch: &AtomicU64,
    indexed_count: &AtomicUsize,
    entries: &mut Vec<IndexedPath>,
) -> bool {
    collect_local_path(root, root, false, epoch, desired_epoch, entries);
    indexed_count.store(entries.len(), Ordering::Release);
    desired_epoch.load(Ordering::Acquire) == epoch
}

fn collect_local_path(
    root: &Path,
    start: &Path,
    include_start: bool,
    epoch: u64,
    desired_epoch: &AtomicU64,
    entries: &mut Vec<IndexedPath>,
) {
    let mut directories = VecDeque::new();
    if include_start {
        let Ok(file_type) = start.symlink_metadata().map(|metadata| metadata.file_type()) else {
            return;
        };
        if file_type.is_symlink() {
            return;
        }
        if let Some(entry) = indexed_local_path(root, start, file_type.is_dir()) {
            entries.push(entry);
        }
        if file_type.is_dir() {
            directories.push_back(start.to_path_buf());
        }
    } else {
        directories.push_back(start.to_path_buf());
    }

    while let Some(directory) = directories.pop_front() {
        if entries.len() >= MAX_INDEX_ENTRIES || desired_epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let Ok(read) = std::fs::read_dir(&directory) else { continue };
        for child in read.flatten() {
            if entries.len() >= MAX_INDEX_ENTRIES {
                return;
            }
            let Ok(file_type) = child.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            let name = child.file_name();
            // The normal tree intentionally hides Git's object database; the
            // search index follows that same user-visible boundary.
            if file_type.is_dir() && name == ".git" {
                continue;
            }
            let path = child.path();
            if let Some(entry) = indexed_local_path(root, &path, file_type.is_dir()) {
                entries.push(entry);
            }
            if file_type.is_dir() {
                directories.push_back(path);
            }
        }
    }
}

fn indexed_local_path(root: &Path, path: &Path, is_dir: bool) -> Option<IndexedPath> {
    let relative = path.strip_prefix(root).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let key = relative.to_string_lossy().replace('\\', "/");
    Some(IndexedPath {
        path: path.to_path_buf(),
        guest_path: None,
        name_folded: name.to_lowercase(),
        key_folded: key.to_lowercase(),
        name,
        key,
        is_dir,
    })
}

fn build_wsl_index(
    located: &crate::shell_detect::WslCwd,
    indexed_count: &AtomicUsize,
    entries: &mut Vec<IndexedPath>,
) -> bool {
    let mut rows = Vec::new();
    let mut budget = SEARCH_VISIT_BUDGET;
    build_wsl_search_index(located, &mut rows, &mut budget);
    let root = normalize_wsl_guest_path(&located.guest);
    entries.extend(rows.into_iter().map(|row| {
        let guest = row.guest_path.clone().unwrap_or_else(|| row.path.display().to_string());
        let key = guest.strip_prefix(&root).unwrap_or(&guest).trim_start_matches('/').to_owned();
        IndexedPath {
            path: row.path,
            guest_path: row.guest_path,
            name_folded: row.name.to_lowercase(),
            key_folded: key.to_lowercase(),
            name: row.name,
            key,
            is_dir: row.is_dir,
        }
    }));
    indexed_count.store(entries.len(), Ordering::Release);
    true
}

fn event_changes_names(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn patch_local_index(
    root: &Path,
    entries: &mut Vec<IndexedPath>,
    mut paths: Vec<PathBuf>,
    epoch: u64,
    desired_epoch: &AtomicU64,
    indexed_count: &AtomicUsize,
) {
    paths.retain(|path| path.starts_with(root));
    paths.sort_by_key(|path| path.components().count());
    paths.dedup();
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !roots.iter().any(|parent| path.starts_with(parent)) {
            roots.push(path);
        }
    }
    if roots.len() > 64 || roots.iter().any(|path| path == root) {
        entries.clear();
        let _ = build_local_index(root, epoch, desired_epoch, indexed_count, entries);
        return;
    }

    entries.retain(|entry| {
        !roots.iter().any(|changed| entry.path == *changed || entry.path.starts_with(changed))
    });
    for changed in roots {
        if entries.len() >= MAX_INDEX_ENTRIES {
            break;
        }
        collect_local_path(root, &changed, true, epoch, desired_epoch, entries);
    }
    indexed_count.store(entries.len(), Ordering::Release);
}

fn publish_search_result(
    entries: &[IndexedPath],
    request: &SearchRequest,
    result_slot: &Mutex<Option<FileSearchResult>>,
) {
    let result = search_entries(entries, request);
    if let Ok(mut slot) = result_slot.lock() {
        *slot = Some(result);
    }
}

enum QueryMatcher {
    Plain { terms: Vec<String>, whole_word: bool, match_case: bool },
    Regex(regex::Regex),
}

impl QueryMatcher {
    fn compile(query: &str, options: FileSearchOptions) -> Result<Self, String> {
        if options.regex {
            let source = if options.whole_word {
                format!(r"(?:^|[^\p{{L}}\p{{N}}_])(?:{query})(?:$|[^\p{{L}}\p{{N}}_])")
            } else {
                query.to_owned()
            };
            return regex::RegexBuilder::new(&source)
                .case_insensitive(!options.match_case)
                .build()
                .map(Self::Regex)
                .map_err(|error| error.to_string());
        }
        let terms = query
            .split_whitespace()
            .map(|term| if options.match_case { term.to_owned() } else { term.to_lowercase() })
            .collect();
        Ok(Self::Plain { terms, whole_word: options.whole_word, match_case: options.match_case })
    }

    fn matches(&self, entry: &IndexedPath) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(&entry.key),
            Self::Plain { terms, whole_word, match_case } => {
                let haystack = if *match_case { &entry.key } else { &entry.key_folded };
                terms.iter().all(|term| {
                    if *whole_word {
                        contains_whole_word(haystack, term)
                    } else {
                        haystack.contains(term)
                    }
                })
            },
        }
    }
}

fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.match_indices(needle).any(|(start, matched)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + matched.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn result_score(entry: &IndexedPath, query: &str, match_case: bool) -> u8 {
    let (name, query) = if match_case {
        (entry.name.as_str(), query.to_owned())
    } else {
        (entry.name_folded.as_str(), query.to_lowercase())
    };
    if name == query {
        0
    } else if name.starts_with(&query) {
        1
    } else if name.contains(&query) {
        2
    } else {
        3
    }
}

fn search_entries(entries: &[IndexedPath], request: &SearchRequest) -> FileSearchResult {
    let matcher = match QueryMatcher::compile(&request.query, request.options) {
        Ok(matcher) => matcher,
        Err(error) => {
            return FileSearchResult {
                generation: request.generation,
                query: request.query.clone(),
                options: request.options,
                rows: Vec::new(),
                total: 0,
                error: Some(error),
            };
        },
    };
    let mut matches: Vec<(u8, &IndexedPath)> = entries
        .iter()
        .filter(|entry| matcher.matches(entry))
        .map(|entry| (result_score(entry, &request.query, request.options.match_case), entry))
        .collect();
    let total = matches.len();
    let compare = |(left_score, left): &(u8, &IndexedPath),
                   (right_score, right): &(u8, &IndexedPath)| {
        left_score
            .cmp(right_score)
            .then(right.is_dir.cmp(&left.is_dir))
            .then(left.name_folded.cmp(&right.name_folded))
            .then(left.key_folded.cmp(&right.key_folded))
    };
    if matches.len() > MAX_SEARCH_RESULTS {
        matches.select_nth_unstable_by(MAX_SEARCH_RESULTS, compare);
        matches.truncate(MAX_SEARCH_RESULTS);
    }
    matches.sort_unstable_by(compare);
    let rows = matches
        .into_iter()
        .take(MAX_SEARCH_RESULTS)
        .map(|(_, entry)| FileRow {
            path: entry.path.clone(),
            guest_path: entry.guest_path.clone(),
            name: entry.name.clone(),
            depth: 0,
            is_dir: entry.is_dir,
            expanded: false,
            is_parent: false,
            ignored: false,
        })
        .collect();
    FileSearchResult {
        generation: request.generation,
        query: request.query.clone(),
        options: request.options,
        rows,
        total,
        error: None,
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn entry(key: &str) -> IndexedPath {
        let name = key.rsplit('/').next().unwrap_or(key).to_owned();
        IndexedPath {
            path: PathBuf::from(key),
            guest_path: None,
            name_folded: name.to_lowercase(),
            key_folded: key.to_lowercase(),
            name,
            key: key.to_owned(),
            is_dir: false,
        }
    }

    #[test]
    fn plain_search_is_case_insensitive_and_ands_terms() {
        let entries = [entry("src/FileTree/SearchIndex.rs"), entry("docs/search.md")];
        let request = SearchRequest {
            generation: 1,
            query: "TREE index".to_owned(),
            options: FileSearchOptions::default(),
        };
        let result = search_entries(&entries, &request);
        assert_eq!(result.total, 1);
        assert_eq!(result.rows[0].name, "SearchIndex.rs");
    }

    #[test]
    fn case_whole_word_and_regex_options_share_one_matcher() {
        let entries = [entry("src/Foo.rs"), entry("src/foo_bar.rs"), entry("src/food.rs")];
        let whole_word = SearchRequest {
            generation: 1,
            query: "foo".to_owned(),
            options: FileSearchOptions { whole_word: true, ..Default::default() },
        };
        assert_eq!(search_entries(&entries, &whole_word).total, 1);

        let case_sensitive = SearchRequest {
            generation: 2,
            query: "Foo".to_owned(),
            options: FileSearchOptions { match_case: true, ..Default::default() },
        };
        assert_eq!(search_entries(&entries, &case_sensitive).total, 1);

        let regex = SearchRequest {
            generation: 3,
            query: r"foo(?:d|_bar)".to_owned(),
            options: FileSearchOptions { regex: true, ..Default::default() },
        };
        assert_eq!(search_entries(&entries, &regex).total, 2);
    }

    #[test]
    fn invalid_regex_is_reported_without_stale_rows() {
        let result = search_entries(
            &[entry("src/main.rs")],
            &SearchRequest {
                generation: 7,
                query: "[".to_owned(),
                options: FileSearchOptions { regex: true, ..Default::default() },
            },
        );
        assert_eq!(result.generation, 7);
        assert!(result.rows.is_empty());
        assert!(result.error.is_some());
    }

    #[test]
    fn local_index_crawls_nested_paths_but_hides_git_objects() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(temp.path().join("src/nested")).expect("source directories");
        std::fs::create_dir_all(temp.path().join(".git/objects")).expect("git directories");
        std::fs::write(temp.path().join("src/nested/needle.rs"), b"").expect("source file");
        std::fs::write(temp.path().join(".git/objects/hidden"), b"").expect("git object");

        let epoch = AtomicU64::new(4);
        let count = AtomicUsize::new(0);
        let mut entries = Vec::new();
        assert!(build_local_index(temp.path(), 4, &epoch, &count, &mut entries));
        assert!(entries.iter().any(|entry| entry.key == "src/nested/needle.rs"));
        assert!(!entries.iter().any(|entry| entry.key.contains(".git")));
        assert_eq!(count.load(Ordering::Acquire), entries.len());
    }

    #[test]
    fn local_index_patch_replaces_removed_and_created_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let old = temp.path().join("old.txt");
        std::fs::write(&old, b"").expect("old file");
        let epoch = AtomicU64::new(8);
        let count = AtomicUsize::new(0);
        let mut entries = Vec::new();
        assert!(build_local_index(temp.path(), 8, &epoch, &count, &mut entries));

        std::fs::remove_file(&old).expect("remove old file");
        let new_dir = temp.path().join("new");
        std::fs::create_dir(&new_dir).expect("new directory");
        std::fs::write(new_dir.join("fresh.txt"), b"").expect("new file");
        patch_local_index(temp.path(), &mut entries, vec![old, new_dir], 8, &epoch, &count);

        assert!(!entries.iter().any(|entry| entry.name == "old.txt"));
        assert!(entries.iter().any(|entry| entry.key == "new/fresh.txt"));
        assert_eq!(count.load(Ordering::Acquire), entries.len());
    }
}
