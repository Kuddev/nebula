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
const WATCH_QUEUE_CAPACITY: usize = 4096;

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
    epoch: u64,
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
    Query(Option<SearchRequest>),
    Refresh {
        epoch: u64,
    },
    #[cfg(test)]
    ReleaseForTest(mpsc::SyncSender<()>),
}

#[derive(Debug)]
pub(crate) struct FileSearchResult {
    pub(crate) epoch: u64,
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
        let query = query.map(|(generation, query, options)| SearchRequest {
            epoch,
            generation,
            query,
            options,
        });
        if let Ok(mut slot) = self.result.lock() {
            *slot = None;
        }
        self.status
            .store(if root.is_some() { INDEX_BUILDING } else { INDEX_IDLE }, Ordering::Release);
        let _ = self.command_tx.send(IndexCommand::Rebuild { root, epoch, query });
    }

    pub(crate) fn query(
        &self,
        epoch: u64,
        generation: u64,
        query: String,
        options: FileSearchOptions,
    ) {
        let _ = self.command_tx.send(IndexCommand::Query(Some(SearchRequest {
            epoch,
            generation,
            query,
            options,
        })));
    }

    pub(crate) fn clear_query(&self) {
        let _ = self.command_tx.send(IndexCommand::Query(None));
    }

    pub(crate) fn refresh(&self, epoch: u64) {
        let _ = self.command_tx.send(IndexCommand::Refresh { epoch });
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

impl Drop for EmbeddedFileIndex {
    fn drop(&mut self) {
        self.desired_epoch.fetch_add(1, Ordering::AcqRel);
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
    let (watch_tx, watch_rx) = mpsc::sync_channel(WATCH_QUEUE_CAPACITY);
    let rescan_requested = Arc::new(AtomicBool::new(false));
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut root: Option<FileIndexRoot> = None;
    let mut epoch = 0;
    let mut entries = Vec::new();
    let mut current_query: Option<SearchRequest> = None;
    let mut pending_command = None;
    let mut last_watch_check = Instant::now();

    loop {
        let received = pending_command.take().map(Ok).unwrap_or_else(|| {
            command_rx.recv_timeout(WATCH_DEBOUNCE.saturating_sub(last_watch_check.elapsed()))
        });
        match received {
            Ok(IndexCommand::Rebuild { root: next_root, epoch: next_epoch, query }) => {
                if desired_epoch.load(Ordering::Acquire) != next_epoch {
                    continue;
                }
                root = next_root;
                epoch = next_epoch;
                current_query = query;
                entries.clear();
                watcher = None;
                while watch_rx.try_recv().is_ok() {}
                rescan_requested.store(false, Ordering::Release);
                indexed_count.store(0, Ordering::Release);
                truncated.store(false, Ordering::Release);

                let Some(index_root) = root.as_ref() else {
                    status.store(INDEX_IDLE, Ordering::Release);
                    continue;
                };
                status.store(INDEX_BUILDING, Ordering::Release);

                if let FileIndexRoot::Local(local_root) = index_root {
                    let callback_tx = watch_tx.clone();
                    let callback_rescan = Arc::clone(&rescan_requested);
                    let callback_root = local_root.clone();
                    watcher = notify::recommended_watcher(
                        move |mut event: notify::Result<notify::Event>| {
                            if let Ok(event) = &mut event
                                && !event.need_rescan()
                            {
                                if !event_changes_names(&event.kind) {
                                    return;
                                }
                                event
                                    .paths
                                    .retain(|path| indexable_local_path(&callback_root, path));
                                if event.paths.is_empty() {
                                    return;
                                }
                            }
                            if let Err(mpsc::TrySendError::Full(_)) = callback_tx.try_send(event) {
                                callback_rescan.store(true, Ordering::Release);
                            }
                        },
                    )
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
                        command => {
                            pending_command = Some(command);
                            break;
                        },
                    }
                }
                current_query = request.filter(|request| request.epoch == epoch);
                if status.load(Ordering::Acquire) == INDEX_READY
                    && desired_epoch.load(Ordering::Acquire) == epoch
                    && let Some(request) = current_query.as_ref()
                {
                    publish_search_result(&entries, request, &result_slot);
                }
            },
            Ok(IndexCommand::Refresh { epoch: requested_epoch }) => {
                if requested_epoch == epoch
                    && desired_epoch.load(Ordering::Acquire) == epoch
                    && root.is_some()
                    && watcher.is_none()
                {
                    pending_command = Some(IndexCommand::Rebuild {
                        root: root.clone(),
                        epoch,
                        query: current_query.clone(),
                    });
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
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if last_watch_check.elapsed() < WATCH_DEBOUNCE {
            continue;
        }
        last_watch_check = Instant::now();
        let Some(FileIndexRoot::Local(local_root)) = root.as_ref() else { continue };
        if watcher.is_none()
            || status.load(Ordering::Acquire) != INDEX_READY
            || desired_epoch.load(Ordering::Acquire) != epoch
        {
            continue;
        }
        let mut changed_paths = Vec::new();
        let mut rescan = rescan_requested.swap(false, Ordering::AcqRel);
        for event in watch_rx.try_iter().take(WATCH_QUEUE_CAPACITY) {
            match event {
                Ok(event) if event.need_rescan() => rescan = true,
                Ok(event) if event_changes_names(&event.kind) => {
                    changed_paths.extend(event.paths);
                },
                Ok(_) => {},
                Err(error) => {
                    log::debug!("file-search watcher error: {error}");
                    rescan = true;
                },
            }
        }
        if rescan {
            pending_command = Some(IndexCommand::Rebuild {
                root: root.clone(),
                epoch,
                query: current_query.clone(),
            });
            continue;
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
        if desired_epoch.load(Ordering::Acquire) != epoch {
            continue;
        }
        truncated.store(entries.len() >= MAX_INDEX_ENTRIES, Ordering::Release);
        if let Some(request) = current_query.as_ref() {
            publish_search_result(&entries, request, &result_slot);
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
    if !indexable_local_path(root, start) {
        return;
    }
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
            if entries.len() >= MAX_INDEX_ENTRIES || desired_epoch.load(Ordering::Acquire) != epoch
            {
                return;
            }
            let Ok(file_type) = child.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            let name = child.file_name();
            // The normal tree intentionally hides Git's object database; the
            // search index follows that same user-visible boundary.
            if name == ".git" {
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

fn indexable_local_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        !relative.components().any(|component| component.as_os_str() == ".git")
    })
}

fn patch_local_index(
    root: &Path,
    entries: &mut Vec<IndexedPath>,
    mut paths: Vec<PathBuf>,
    epoch: u64,
    desired_epoch: &AtomicU64,
    indexed_count: &AtomicUsize,
) {
    if desired_epoch.load(Ordering::Acquire) != epoch {
        return;
    }
    paths.retain(|path| indexable_local_path(root, path));
    paths.sort();
    paths.dedup();
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in paths {
        if roots.last().is_none_or(|parent| !path.starts_with(parent)) {
            roots.push(path);
        }
    }
    if roots.iter().any(|path| path == root) {
        entries.clear();
        let _ = build_local_index(root, epoch, desired_epoch, indexed_count, entries);
        return;
    }

    if roots.is_empty() {
        return;
    }
    let changed_roots: HashSet<&Path> = roots.iter().map(PathBuf::as_path).collect();
    entries.retain(|entry| !entry.path.ancestors().any(|path| changed_roots.contains(path)));
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
    let name = if match_case { &entry.name } else { &entry.name_folded };
    if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else if name.contains(query) {
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
                epoch: request.epoch,
                generation: request.generation,
                query: request.query.clone(),
                options: request.options,
                rows: Vec::new(),
                total: 0,
                error: Some(error),
            };
        },
    };
    let ranking_query = if request.options.match_case {
        request.query.clone()
    } else {
        request.query.to_lowercase()
    };
    let mut matches: Vec<(u8, &IndexedPath)> = entries
        .iter()
        .filter(|entry| matcher.matches(entry))
        .map(|entry| (result_score(entry, &ranking_query, request.options.match_case), entry))
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
        epoch: request.epoch,
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
            epoch: 1,
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
            epoch: 1,
            generation: 1,
            query: "foo".to_owned(),
            options: FileSearchOptions { whole_word: true, ..Default::default() },
        };
        assert_eq!(search_entries(&entries, &whole_word).total, 1);

        let case_sensitive = SearchRequest {
            epoch: 1,
            generation: 2,
            query: "Foo".to_owned(),
            options: FileSearchOptions { match_case: true, ..Default::default() },
        };
        assert_eq!(search_entries(&entries, &case_sensitive).total, 1);

        let regex = SearchRequest {
            epoch: 1,
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
                epoch: 1,
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

    #[test]
    fn large_change_batches_preserve_unrelated_index_entries() {
        let temp = tempfile::tempdir().unwrap();
        let epoch = AtomicU64::new(2);
        let count = AtomicUsize::new(0);
        let untouched = temp.path().join("untouched.txt");
        let mut entries = vec![indexed_local_path(temp.path(), &untouched, false).unwrap()];
        let paths: Vec<_> = (0..128)
            .map(|number| {
                let path = temp.path().join(format!("changed-{number}.txt"));
                std::fs::write(&path, b"").unwrap();
                path
            })
            .collect();
        patch_local_index(temp.path(), &mut entries, paths, 2, &epoch, &count);

        assert!(entries.iter().any(|entry| entry.path == untouched));
        assert_eq!(entries.len(), 129);
        assert_eq!(count.load(Ordering::Acquire), 129);
    }

    #[test]
    fn local_patch_deduplicates_overlapping_paths_and_hides_git_events() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("src/nested");
        let source = nested.join("needle.rs");
        let sibling = temp.path().join("src-other");
        let git_object = temp.path().join(".git/objects/hidden");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::create_dir_all(git_object.parent().unwrap()).unwrap();
        std::fs::write(&source, b"").unwrap();
        std::fs::write(&git_object, b"").unwrap();
        let epoch = AtomicU64::new(1);
        let count = AtomicUsize::new(0);
        let mut entries = Vec::new();
        patch_local_index(
            temp.path(),
            &mut entries,
            vec![
                source.clone(),
                sibling.clone(),
                nested,
                temp.path().join("src"),
                source.clone(),
                temp.path().join(".git"),
                git_object,
            ],
            1,
            &epoch,
            &count,
        );

        assert_eq!(entries.iter().filter(|entry| entry.path == source).count(), 1);
        assert!(entries.iter().any(|entry| entry.path == sibling));
        assert!(!entries.iter().any(|entry| entry.key.contains(".git")));
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn cancelled_patch_leaves_index_intact() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.txt");
        let mut entries = vec![indexed_local_path(temp.path(), &original, false).unwrap()];
        let count = AtomicUsize::new(1);
        patch_local_index(
            temp.path(),
            &mut entries,
            vec![original.clone()],
            1,
            &AtomicU64::new(2),
            &count,
        );
        assert_eq!(entries[0].path, original);
        assert_eq!(count.load(Ordering::Acquire), 1);
    }

    fn wait_for_result(
        index: &EmbeddedFileIndex,
        matches: impl Fn(&FileSearchResult) -> bool,
    ) -> FileSearchResult {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(result) = index.take_result()
                && matches(&result)
            {
                return result;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("file index did not publish the expected result");
    }

    #[test]
    fn watched_index_tracks_create_rename_delete_without_rebuilding() {
        let temp = tempfile::tempdir().unwrap();
        let index = EmbeddedFileIndex::new();
        index.rebuild(
            Some(FileIndexRoot::Local(temp.path().to_owned())),
            1,
            Some((1, "needle".to_owned(), FileSearchOptions::default())),
        );
        wait_for_result(&index, |result| result.total == 0);

        let created = temp.path().join("needle.txt");
        std::fs::write(&created, b"").unwrap();
        wait_for_result(&index, |result| result.total == 1);
        let renamed = temp.path().join("needle-renamed.txt");
        std::fs::rename(&created, &renamed).unwrap();
        wait_for_result(&index, |result| result.total == 1 && result.rows[0].path == renamed);
        std::fs::remove_file(&renamed).unwrap();
        wait_for_result(&index, |result| result.total == 0);
        assert_eq!(index.desired_epoch.load(Ordering::Acquire), 1);
        index.release_for_test();
    }

    #[test]
    fn switching_roots_discards_queued_old_queries() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("needle-first.txt"), b"").unwrap();
        std::fs::write(second.path().join("needle-second.txt"), b"").unwrap();
        let index = EmbeddedFileIndex::new();
        index.rebuild(Some(FileIndexRoot::Local(first.path().to_owned())), 1, None);
        index.query(1, 1, "needle".to_owned(), FileSearchOptions::default());
        index.rebuild(
            Some(FileIndexRoot::Local(second.path().to_owned())),
            2,
            Some((2, "needle".to_owned(), FileSearchOptions::default())),
        );
        let result = wait_for_result(&index, |result| result.epoch == 2);
        assert_eq!(result.generation, 2);
        assert_eq!(result.total, 1);
        assert_eq!(result.rows[0].name, "needle-second.txt");
        index.release_for_test();
    }
}
