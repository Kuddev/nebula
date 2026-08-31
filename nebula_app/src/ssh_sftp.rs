pub(crate) mod limits;
pub(crate) mod remote_cwd;
mod transaction;
mod transfer;

use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use tokio::io::AsyncWriteExt;

use transaction::FileStamp;
use transfer::TransferObserver;

type SftpError = Box<dyn std::error::Error + Send + Sync>;
type SftpResult<T> = Result<T, SftpError>;

const MAX_RECURSIVE_ENTRIES: usize = 100_000;
const CANCEL_REQUESTED: usize = 1;
const PUBLISHER_UNIT: usize = 2;
static TRANSFER_NONCE: AtomicU64 = AtomicU64::new(1);

type TaskControl = Arc<AtomicUsize>;

/// UI 层被通知"状态变了，该重画了"的方式。
///
/// 传输跑在网络 runtime 上，而画面归 UI 层。两者之间只需要一个"响一声"的
/// 信号——控制器绝不该知道 UI 是哪一套窗口系统、消息循环长什么样。做成
/// 闭包而不是具体的事件代理类型，是因为不同的宿主唤醒自己的方式完全不同，
/// 而这个模块对它们应当一视同仁。
pub(crate) type WakeFn = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SftpConflictPolicy {
    Overwrite,
    Skip,
    KeepBoth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SftpTransferOptions {
    pub conflict: SftpConflictPolicy,
    pub skip_unchanged: bool,
}

impl Default for SftpTransferOptions {
    fn default() -> Self {
        Self { conflict: SftpConflictPolicy::Overwrite, skip_unchanged: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SftpEntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SftpEntry {
    pub name: String,
    pub path: String,
    pub kind: SftpEntryKind,
    pub size: u64,
    pub modified: u64,
    pub permissions: String,
    /// UI-only parent navigation row. Never sent to remote mutation APIs.
    pub is_parent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferProgress {
    pub label: String,
    pub transferred: u64,
    pub total: u64,
}

impl TransferProgress {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        Self { label: label.into(), transferred: 0, total }
    }

    pub fn advance(&mut self, bytes: u64) {
        self.transferred = self.transferred.saturating_add(bytes).min(self.total);
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 1.0 } else { self.transferred as f32 / self.total as f32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SftpPhase {
    Connecting,
    Loading,
    Ready,
    Working,
    Error,
}

#[derive(Clone, Debug)]
pub struct SftpSnapshot {
    pub destination: String,
    pub path: String,
    pub entries: Vec<SftpEntry>,
    pub phase: SftpPhase,
    pub error: Option<String>,
    pub progress: Option<TransferProgress>,
}

#[derive(Clone)]
pub struct SftpController {
    state: Arc<Mutex<SftpSnapshot>>,
    task_control: Arc<Mutex<TaskControl>>,
    generation: Arc<AtomicU64>,
    wake: WakeFn,
    pending_uploads: Arc<Mutex<Vec<PathBuf>>>,
    upload_debounce_active: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TaskContext {
    state: Arc<Mutex<SftpSnapshot>>,
    task_control: TaskControl,
    generation: Arc<AtomicU64>,
    task_generation: u64,
    wake: WakeFn,
    last_wake: Arc<Mutex<Instant>>,
}

struct PublishGuard {
    task_control: TaskControl,
}

impl Drop for PublishGuard {
    fn drop(&mut self) {
        let previous = self.task_control.fetch_sub(PUBLISHER_UNIT, Ordering::AcqRel);
        debug_assert!(previous & !CANCEL_REQUESTED >= PUBLISHER_UNIT);
    }
}

impl SftpEntry {
    #[cfg(test)]
    fn test(name: &str, kind: SftpEntryKind) -> Self {
        Self {
            name: name.to_owned(),
            path: format!("/{name}"),
            kind,
            size: 0,
            modified: 0,
            permissions: String::new(),
            is_parent: false,
        }
    }
}

impl SftpController {
    pub fn new(destination: impl Into<String>, wake: WakeFn) -> io::Result<Self> {
        Self::new_at(destination, ".", wake)
    }

    pub(crate) fn new_at(
        destination: impl Into<String>,
        initial_path: impl Into<String>,
        wake: WakeFn,
    ) -> io::Result<Self> {
        crate::ssh_session::runtime()?;
        let initial_path = initial_path.into();
        let controller = Self {
            state: Arc::new(Mutex::new(SftpSnapshot {
                destination: destination.into(),
                path: initial_path.clone(),
                entries: Vec::new(),
                phase: SftpPhase::Connecting,
                error: None,
                progress: None,
            })),
            task_control: Arc::new(Mutex::new(Arc::new(AtomicUsize::new(0)))),
            generation: Arc::new(AtomicU64::new(0)),
            wake,
            pending_uploads: Arc::new(Mutex::new(Vec::new())),
            upload_debounce_active: Arc::new(AtomicBool::new(false)),
        };
        controller.refresh(initial_path);
        Ok(controller)
    }

    pub fn snapshot(&self) -> SftpSnapshot {
        lock(&self.state).clone()
    }

    pub fn refresh(&self, requested_path: impl Into<String>) {
        let requested_path = requested_path.into();
        let destination = self.snapshot().destination;
        self.start_job(SftpPhase::Loading, None, move |_context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            let path = sftp.canonicalize(requested_path).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
            Ok((path, entries))
        });
    }

    pub fn upload_paths(&self, local_paths: Vec<PathBuf>) {
        if local_paths.is_empty() {
            return;
        }
        lock(&self.pending_uploads).extend(local_paths);
        if self.upload_debounce_active.swap(true, Ordering::AcqRel) {
            return;
        }
        let controller = self.clone();
        crate::ssh_session::runtime().expect("SFTP runtime checked at construction").spawn(
            async move {
                // Windows sends one DroppedFile event per selected path. A short
                // debounce folds that burst into one transfer job instead of each
                // path cancelling the previous generation.
                tokio::time::sleep(Duration::from_millis(75)).await;
                let paths = std::mem::take(&mut *lock(&controller.pending_uploads));
                controller.upload_debounce_active.store(false, Ordering::Release);
                controller.start_upload_paths(paths);
            },
        );
    }

    fn start_upload_paths(&self, local_paths: Vec<PathBuf>) {
        self.start_upload_paths_with_options(local_paths, SftpTransferOptions::default());
    }

    pub(crate) fn upload_paths_with_options(
        &self,
        local_paths: Vec<PathBuf>,
        options: SftpTransferOptions,
    ) {
        self.start_upload_paths_with_options(local_paths, options);
    }

    fn start_upload_paths_with_options(
        &self,
        local_paths: Vec<PathBuf>,
        options: SftpTransferOptions,
    ) {
        if local_paths.is_empty() {
            return;
        }
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let remote_dir = snapshot.path;
        let label = if local_paths.len() == 1 {
            local_paths[0]
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "上传".to_owned())
        } else {
            format!("上传 {} 项", local_paths.len())
        };
        self.start_job(
            SftpPhase::Working,
            Some(TransferProgress::new(label, 0)),
            move |context| async move {
                let sftp = crate::ssh_session::open_sftp(&destination).await?;
                upload_local_paths(&sftp, local_paths, &remote_dir, options, &context).await?;
                let entries = read_remote_dir(&sftp, &remote_dir).await?;
                Ok((remote_dir, entries))
            },
        );
    }

    pub fn download(&self, entry: SftpEntry, local_directory: PathBuf) {
        self.download_with_options(entry, local_directory, SftpTransferOptions::default());
    }

    pub(crate) fn download_with_options(
        &self,
        entry: SftpEntry,
        local_directory: PathBuf,
        options: SftpTransferOptions,
    ) {
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        let progress = TransferProgress::new(entry.name.clone(), entry.size);
        self.start_job(SftpPhase::Working, Some(progress), move |context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            download_remote_entry(&sftp, entry, local_directory, options, &context).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
            Ok((path, entries))
        });
    }

    pub(crate) fn copy_from(
        &self,
        source_destination: String,
        entry: SftpEntry,
        options: SftpTransferOptions,
    ) {
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        let progress =
            TransferProgress::new(format!("复制 {}", entry.name), entry.size.saturating_mul(2));
        self.start_job(SftpPhase::Working, Some(progress), move |context| async move {
            let source = crate::ssh_session::open_sftp(&source_destination).await?;
            let target = crate::ssh_session::open_sftp(&destination).await?;
            copy_remote_entry(&source, &target, entry, &path, options, &context).await?;
            let entries = read_remote_dir(&target, &path).await?;
            Ok((path, entries))
        });
    }

    pub fn create_directory(&self, name: &str) -> Result<(), String> {
        let name = validate_name(name).map_err(str::to_owned)?.to_owned();
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        let new_path = normalize_remote_path(&path, &name);
        self.start_job(SftpPhase::Working, None, move |_context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            sftp.create_dir(new_path).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
            Ok((path, entries))
        });
        Ok(())
    }

    pub fn rename(&self, entry: SftpEntry, name: &str) -> Result<(), String> {
        let name = validate_name(name).map_err(str::to_owned)?.to_owned();
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        let new_path = normalize_remote_path(&path, &name);
        self.start_job(SftpPhase::Working, None, move |_context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            sftp.rename(entry.path, new_path).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
            Ok((path, entries))
        });
        Ok(())
    }

    pub fn delete(&self, entry: SftpEntry) {
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        self.start_job(SftpPhase::Working, None, move |context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            delete_remote_entry(&sftp, entry, &context).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
            Ok((path, entries))
        });
    }

    pub fn cancel(&self) {
        let task_control = lock(&self.task_control).clone();
        let previous = task_control.fetch_or(CANCEL_REQUESTED, Ordering::AcqRel);
        let publishing = previous & !CANCEL_REQUESTED != 0;
        let mut state = lock(&self.state);
        if state.phase == SftpPhase::Working {
            state.error = Some(if publishing {
                "正在完成已进入发布阶段的文件，随后取消…".to_owned()
            } else {
                "正在取消传输…".to_owned()
            });
        }
        drop(state);
        (self.wake)();
    }

    fn start_job<J, F>(&self, phase: SftpPhase, progress: Option<TransferProgress>, job: J)
    where
        J: FnOnce(TaskContext) -> F + Send + 'static,
        F: Future<Output = SftpResult<(String, Vec<SftpEntry>)>> + Send + 'static,
    {
        let task_control = Arc::new(AtomicUsize::new(0));
        let task_generation = {
            // 新任务必须换一枚独立令牌；旧任务的发布 guard 可能晚些才释放，若
            // 复用同一原子值，它会错误扣减新任务的发布计数。
            let mut current = lock(&self.task_control);
            let task_generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            *current = task_control.clone();
            task_generation
        };
        {
            let mut state = lock(&self.state);
            state.phase = phase;
            state.error = None;
            state.progress = progress;
        }
        (self.wake)();

        let context = TaskContext {
            state: self.state.clone(),
            task_control,
            generation: self.generation.clone(),
            task_generation,
            wake: self.wake.clone(),
            last_wake: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
        };
        let completion = context.clone();
        crate::ssh_session::runtime().expect("SFTP runtime checked at construction").spawn(
            async move {
                let result = job(context).await;
                completion.finish(result);
            },
        );
    }
}

impl TransferObserver for TaskContext {
    fn advance(&self, bytes: u64) {
        if !self.is_current() {
            return;
        }
        if let Some(progress) = lock(&self.state).progress.as_mut() {
            progress.advance(bytes);
        }
        self.wake_throttled(false);
    }

    fn cancelled(&self) -> bool {
        self.task_control.load(Ordering::Acquire) & CANCEL_REQUESTED != 0 || !self.is_current()
    }
}

impl TaskContext {
    fn check_cancelled(&self) -> SftpResult<()> {
        if self.cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "操作已取消").into())
        } else {
            Ok(())
        }
    }

    /// 把最终发布声明成不可拆分区间。
    ///
    /// 目录传输会并发发布多个文件，因此高位保存发布者计数；取消位与计数通过
    /// 同一个 CAS 竞争，保证取消先到时不会再开始 rename，发布先到时则允许该
    /// 笔备份/替换/恢复事务完整收尾。
    fn begin_publish(&self) -> SftpResult<PublishGuard> {
        self.check_cancelled()?;
        let mut current = self.task_control.load(Ordering::Acquire);
        loop {
            if current & CANCEL_REQUESTED != 0 || !self.is_current() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "操作已取消").into());
            }
            let next = current
                .checked_add(PUBLISHER_UNIT)
                .ok_or_else(|| io::Error::other("同时发布的文件数量超出系统限制"))?;
            match self.task_control.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if !self.is_current() {
                        self.task_control.fetch_sub(PUBLISHER_UNIT, Ordering::AcqRel);
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "操作已取消").into());
                    }
                    return Ok(PublishGuard { task_control: self.task_control.clone() });
                },
                Err(observed) => current = observed,
            }
        }
    }

    fn set_total(&self, total: u64) {
        if !self.is_current() {
            return;
        }
        if let Some(progress) = lock(&self.state).progress.as_mut() {
            progress.total = total;
            progress.transferred = progress.transferred.min(total);
        }
        self.wake_throttled(true);
    }

    fn set_label(&self, label: impl Into<String>) {
        if !self.is_current() {
            return;
        }
        if let Some(progress) = lock(&self.state).progress.as_mut() {
            progress.label = label.into();
        }
        self.wake_throttled(false);
    }

    fn is_current(&self) -> bool {
        self.generation.load(Ordering::Acquire) == self.task_generation
    }

    fn wake_throttled(&self, force: bool) {
        let mut last = lock(&self.last_wake);
        if force || last.elapsed() >= Duration::from_millis(50) {
            *last = Instant::now();
            (self.wake)();
        }
    }

    fn finish(&self, result: SftpResult<(String, Vec<SftpEntry>)>) {
        if !self.is_current() {
            return;
        }
        let mut state = lock(&self.state);
        match result {
            Ok((path, entries)) => {
                state.path = path;
                state.entries = entries;
                state.phase = SftpPhase::Ready;
                state.error = None;
                state.progress = None;
            },
            Err(err) => {
                state.phase = SftpPhase::Error;
                state.error = Some(err.to_string());
                state.progress = None;
            },
        }
        drop(state);
        (self.wake)();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn normalize_remote_path(base: &str, path: &str) -> String {
    let normalized_path = path.replace('\\', "/");
    let mut components = Vec::new();

    if !normalized_path.starts_with('/') {
        components.extend(base.replace('\\', "/").split('/').map(str::to_owned));
    }
    components.extend(normalized_path.split('/').map(str::to_owned));

    let mut resolved = Vec::new();
    for component in components {
        match component.as_str() {
            "" | "." => {},
            ".." => {
                resolved.pop();
            },
            _ => resolved.push(component),
        }
    }

    if resolved.is_empty() { "/".to_owned() } else { format!("/{}", resolved.join("/")) }
}

pub fn validate_name(name: &str) -> Result<&str, &'static str> {
    if name.is_empty() || matches!(name, "." | "..") {
        return Err("名称不能为空，也不能使用 . 或 ..");
    }
    if name.contains(['/', '\\', '\0']) {
        return Err("名称不能包含路径分隔符或空字符");
    }
    Ok(name)
}

pub fn temporary_upload_path(destination: &str, nonce: u64) -> String {
    let normalized = normalize_remote_path("/", destination);
    let (parent, name) = normalized.rsplit_once('/').unwrap_or(("", normalized.as_str()));
    let parent = if parent.is_empty() { "/" } else { parent };
    let separator = if parent == "/" { "" } else { "/" };
    format!("{parent}{separator}.{name}.nebula-upload-{nonce:016x}")
}

pub fn sort_entries(entries: &mut [SftpEntry]) {
    entries.sort_by(|left, right| {
        let left_rank = !matches!(left.kind, SftpEntryKind::Directory);
        let right_rank = !matches!(right.kind, SftpEntryKind::Directory);
        left_rank
            .cmp(&right_rank)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

async fn read_remote_dir(sftp: &SftpSession, path: &str) -> SftpResult<Vec<SftpEntry>> {
    let mut entries = Vec::new();
    for entry in sftp.read_dir(path.to_owned()).await? {
        let metadata = entry.metadata();
        let kind = match metadata.file_type() {
            FileType::Dir => SftpEntryKind::Directory,
            FileType::Symlink => SftpEntryKind::Symlink,
            FileType::File | FileType::Other => SftpEntryKind::File,
        };
        let type_prefix = match kind {
            SftpEntryKind::Directory => 'd',
            SftpEntryKind::Symlink => 'l',
            SftpEntryKind::File => '-',
        };
        entries.push(SftpEntry {
            name: entry.file_name(),
            path: entry.path(),
            kind,
            size: metadata.len(),
            modified: u64::from(metadata.mtime.unwrap_or(0)),
            permissions: format!("{type_prefix}{}", metadata.permissions()),
            is_parent: false,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

#[derive(Debug)]
struct UploadPlan {
    directories: Vec<String>,
    files: Vec<UploadFile>,
    total: u64,
}

#[derive(Debug)]
struct UploadFile {
    local: PathBuf,
    remote: String,
    stamp: FileStamp,
}

#[derive(Debug)]
struct UploadRoot {
    local: PathBuf,
    remote: String,
    is_directory: bool,
    stamp: Option<FileStamp>,
}

fn inspect_upload_roots(
    local_paths: Vec<PathBuf>,
    remote_dir: String,
) -> SftpResult<Vec<UploadRoot>> {
    let mut roots = Vec::with_capacity(local_paths.len());
    for local in local_paths {
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "本地路径缺少有效名称"))?;
        validate_name(name)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let metadata = std::fs::symlink_metadata(&local)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("暂不上传本地符号链接: {}", local.display()),
            )
            .into());
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("本地源不是普通文件或目录: {}", local.display()),
            )
            .into());
        }
        roots.push(UploadRoot {
            remote: normalize_remote_path(&remote_dir, name),
            stamp: metadata.is_file().then(|| FileStamp::read(&local)).transpose()?,
            local,
            is_directory: metadata.is_dir(),
        });
    }
    Ok(roots)
}

async fn resolve_upload_roots(
    sftp: &SftpSession,
    local_paths: Vec<PathBuf>,
    remote_dir: &str,
    options: SftpTransferOptions,
) -> SftpResult<Vec<UploadRoot>> {
    let remote_dir = remote_dir.to_owned();
    let roots = tokio::task::spawn_blocking(move || inspect_upload_roots(local_paths, remote_dir))
        .await
        .map_err(|error| format!("扫描上传入口失败: {error}"))??;
    let mut resolved = Vec::with_capacity(roots.len());
    let mut reserved = HashSet::with_capacity(roots.len());
    for mut root in roots {
        let existing = transaction::remote_metadata_optional(sftp, &root.remote).await?;
        let collides_with_batch = reserved.contains(&root.remote);
        if existing.is_none() && !collides_with_batch {
            reserved.insert(root.remote.clone());
            resolved.push(root);
            continue;
        }

        if options.skip_unchanged
            && !collides_with_batch
            && !root.is_directory
            && root.stamp.is_some_and(|stamp| {
                existing
                    .as_ref()
                    .is_some_and(|metadata| transaction::remote_unchanged(stamp, metadata))
            })
        {
            continue;
        }
        match options.conflict {
            SftpConflictPolicy::Skip => continue,
            SftpConflictPolicy::KeepBoth => {
                root.remote =
                    duplicate_remote_root_path(sftp, &root.remote, root.is_directory, &reserved)
                        .await?;
            },
            SftpConflictPolicy::Overwrite => {
                if collides_with_batch {
                    return Err(io::Error::other(format!(
                        "批量上传包含多个同名根项目，不能同时覆盖: {}",
                        root.remote
                    ))
                    .into());
                }
                let metadata = existing.as_ref().expect("remote collision checked above");
                let compatible = (root.is_directory && metadata.is_dir())
                    || (!root.is_directory && (metadata.is_regular() || metadata.is_symlink()));
                if !compatible {
                    return Err(io::Error::other(format!(
                        "远端同名目标类型不同，不能直接覆盖: {}",
                        root.remote
                    ))
                    .into());
                }
            },
        }
        reserved.insert(root.remote.clone());
        resolved.push(root);
    }
    Ok(resolved)
}

/// 保留两者时既要避开服务器现有名称，也要避开本批次已经预留、但还没真正
/// 创建的名称；否则两个本地根项目会在并发阶段写到同一个远端目标。
async fn duplicate_remote_root_path(
    sftp: &SftpSession,
    destination: &str,
    is_directory: bool,
    reserved: &HashSet<String>,
) -> SftpResult<String> {
    let normalized = normalize_remote_path("/", destination);
    let (parent, name) = normalized.rsplit_once('/').unwrap_or(("", normalized.as_str()));
    let parent = if parent.is_empty() { "/" } else { parent };
    for index in 1..1000 {
        let candidate =
            normalize_remote_path(parent, &transaction::duplicate_name(name, is_directory, index));
        if !reserved.contains(&candidate)
            && transaction::remote_metadata_optional(sftp, &candidate).await?.is_none()
        {
            return Ok(candidate);
        }
    }
    Err(io::Error::other(format!("无法为同名项目生成可用名称: {destination}")).into())
}

fn build_upload_plan(roots: Vec<UploadRoot>) -> SftpResult<UploadPlan> {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut stack = Vec::new();

    for root in roots.into_iter().rev() {
        stack.push((root.local, root.remote, root.stamp));
    }

    while let Some((local, remote, known_stamp)) = stack.pop() {
        if files.len() + directories.len() >= MAX_RECURSIVE_ENTRIES {
            return Err(io::Error::other("上传目录超过 100000 项，已停止").into());
        }
        let metadata = std::fs::symlink_metadata(&local)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("暂不上传本地符号链接: {}", local.display()),
            )
            .into());
        }
        if metadata.is_dir() {
            directories.push(remote.clone());
            let mut children = std::fs::read_dir(&local)?.collect::<Result<Vec<_>, _>>()?;
            children.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
            for child in children.into_iter().rev() {
                let name = child.file_name().to_string_lossy().into_owned();
                validate_name(&name)
                    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
                stack.push((child.path(), normalize_remote_path(&remote, &name), None));
            }
        } else if metadata.is_file() {
            let stamp = known_stamp.map(Ok).unwrap_or_else(|| FileStamp::read(&local))?;
            total = total.saturating_add(stamp.len);
            files.push(UploadFile { local, remote, stamp });
        }
    }

    Ok(UploadPlan { directories, files, total })
}

async fn upload_local_paths(
    sftp: &SftpSession,
    local_paths: Vec<PathBuf>,
    remote_dir: &str,
    options: SftpTransferOptions,
    context: &TaskContext,
) -> SftpResult<()> {
    let roots = resolve_upload_roots(sftp, local_paths, remote_dir, options).await?;
    let plan = tokio::task::spawn_blocking(move || build_upload_plan(roots))
        .await
        .map_err(|err| format!("扫描上传目录失败: {err}"))??;
    context.set_total(plan.total);

    execute_upload_plan(sftp, plan, options, context).await
}

async fn execute_upload_plan(
    sftp: &SftpSession,
    plan: UploadPlan,
    options: SftpTransferOptions,
    context: &TaskContext,
) -> SftpResult<()> {
    // 目录必须串行建，而且必须按计划里的顺序：计划是先序遍历产出的，父目录
    // 排在子目录之前。并发建目录会让子目录的 MKDIR 抢在父目录之前发出去，
    // 撞上"父路径不存在"而失败。这一步是元数据操作，串行的代价也小。
    for directory in plan.directories {
        context.check_cancelled()?;
        if let Err(create_error) = sftp.create_dir(directory.clone()).await {
            let existing = transaction::remote_metadata_optional(sftp, &directory).await?;
            if !existing.is_some_and(|metadata| metadata.is_dir()) {
                return Err(create_error.into());
            }
        }
    }
    // 文件之间没有顺序依赖，可以并发。这是目录上传最大的一笔性能：一个几百
    // 个小文件的目录，串行传等于把几百条往返链首尾相接，而每条链的时间几乎
    // 全是等待。
    transfer::run_bounded(&plan.files, limits::DIRECTORY_FILE_CONCURRENCY, |file| async move {
        context.check_cancelled()?;
        context.set_label(file.remote.rsplit('/').next().unwrap_or(file.remote.as_str()));
        transaction::upload_file(
            sftp,
            &file.local,
            &file.remote,
            file.stamp,
            options.skip_unchanged,
            context,
        )
        .await
    })
    .await?;
    Ok(())
}

/// 列一个远端目录，供不带状态机的浏览器直接使用。
///
/// 与 [`SftpController::refresh`] 的区别在职责：那个驱动一整套面板状态机
/// （phase / error / 当前路径都要更新，还要唤醒宿主重画），这里只回答"这个
/// 目录里有什么"，让调用方自己决定怎么呈现。相对路径先经 `canonicalize`
/// 落成绝对路径，否则"上一级"这类操作没有可靠的起点。
///
/// 错误以文案返回而不是 `Box<dyn Error>`：调用方是界面，它要的是能直接显示
/// 给用户的一句话，而不是一条需要再加工的错误链。
pub(crate) async fn list_dir(destination: &str, path: &str) -> Result<Vec<SftpEntry>, String> {
    let sftp = crate::ssh_session::open_sftp(destination)
        .await
        .map_err(|err| format!("无法连接 {destination}：{err}"))?;
    let resolved = sftp
        .canonicalize(path.to_owned())
        .await
        .map_err(|err| format!("无法解析路径 {path}：{err}"))?;
    read_remote_dir(&sftp, &resolved).await.map_err(|err| format!("无法读取目录 {resolved}：{err}"))
}

/// 为补齐列一个远端目录，`(是否目录, 名字)`。
///
/// 与 [`SftpController::refresh`] 的区别在职责：那个驱动 SFTP 面板的状态机
/// （phase / error / 当前路径都要更新），这里只要一份条目，而且失败是**正常**
/// 结果——补齐补不出来就是没有候选，不该弹错误、不该改任何 UI 状态。
///
/// 符号链接一律当目录：远端的 `l` 十有八九指向目录（`/bin`、`/lib` 这些），
/// 而补齐把它当文件的代价是不给尾随 `/`，用户得自己补一刀。
pub async fn list_dir_for_completion(destination: &str, dir: &str) -> Option<Vec<(bool, String)>> {
    let sftp = match crate::ssh_session::open_sftp(destination).await {
        Ok(sftp) => sftp,
        Err(err) => {
            log::debug!("补齐列远端目录失败（{destination}:{dir}）: {err}");
            return None;
        },
    };
    match read_remote_dir(&sftp, dir).await {
        Ok(entries) => Some(
            entries
                .into_iter()
                .map(|entry| (entry.kind != SftpEntryKind::File, entry.name))
                .collect(),
        ),
        Err(err) => {
            log::debug!("补齐读远端目录失败（{destination}:{dir}）: {err}");
            None
        },
    }
}

/// 把剪贴板截图上传到远端临时目录，成功后把远端路径交给 `on_uploaded`。
///
/// 上传在 SSH runtime 上异步进行，UI 线程零阻塞；失败只留日志，绝不把错误
/// 文本粘进终端。回调而不是事件代理：这个模块不该知道"路径最终怎么送到
/// 那个 pane 的输入里"，宿主自己清楚。
pub fn upload_clipboard_image(
    destination: String,
    png: Vec<u8>,
    on_uploaded: impl FnOnce(String) + Send + 'static,
) {
    let Ok(runtime) = crate::ssh_session::runtime() else {
        return;
    };
    runtime.spawn(async move {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        // `/tmp` 而不是远端 cwd：绝对路径对远端的命令行工具一样可读，且不往
        // 用户的项目目录里排泄粘贴产物；`~` 需要展开，这里没有 shell。
        let remote = format!("/tmp/nebula-paste-{stamp}.png");
        let result = async {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            let mut file = sftp
                .open_with_flags(
                    remote.clone(),
                    OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                )
                .await?;
            file.write_all(&png).await?;
            file.shutdown().await?;
            Ok::<_, SftpError>(())
        }
        .await;
        match result {
            Ok(()) => on_uploaded(remote),
            Err(err) => log::warn!("剪贴板图片上传失败（{destination}）: {err}"),
        }
    });
}

#[derive(Debug)]
struct DownloadPlan {
    directories: Vec<PathBuf>,
    files: Vec<DownloadFile>,
    total: u64,
}

#[derive(Debug)]
struct DownloadFile {
    remote: String,
    local: PathBuf,
    size: u64,
    modified: u64,
}

async fn build_download_plan(
    sftp: &SftpSession,
    entry: SftpEntry,
    local_directory: PathBuf,
    context: &TaskContext,
) -> SftpResult<DownloadPlan> {
    validate_name(&entry.name)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut pending = vec![(entry, local_directory)];
    let mut visited_directories = HashSet::new();

    // 按层推进而不是深度优先逐个问：SFTP 没有递归 LIST，一棵树的规模全靠一层
    // 层问出来。串行问的话，"算出总共多少字节"这件事本身就要花掉和传输相当的
    // 时间——用户看到的是进度条迟迟不动，以为卡住了。同一层的目录之间没有
    // 依赖，可以并发列。
    while !pending.is_empty() {
        context.check_cancelled()?;
        if files.len() + directories.len() >= MAX_RECURSIVE_ENTRIES {
            return Err(io::Error::other("下载目录超过 100000 项，已停止").into());
        }

        // 先把这一层解析成"要列的目录"和"要传的文件"。符号链接的解析和环路
        // 检测都在这里串行做完：环路判定依赖已访问集合，并发写它会让"这个
        // 目录是不是第一次见"的答案取决于调度顺序。
        let mut to_list = Vec::new();
        for (entry, parent) in std::mem::take(&mut pending) {
            validate_name(&entry.name)
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
            let local = parent.join(&entry.name);
            let (remote, kind, size, modified) = if entry.kind == SftpEntryKind::Symlink {
                let target = sftp.read_link(entry.path.clone()).await?;
                let parent = entry.path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("/");
                let target = normalize_remote_path(parent, &target);
                let metadata = sftp.metadata(target.clone()).await?;
                let kind =
                    if metadata.is_dir() { SftpEntryKind::Directory } else { SftpEntryKind::File };
                (target, kind, metadata.len(), u64::from(metadata.mtime.unwrap_or(0)))
            } else {
                (entry.path, entry.kind, entry.size, entry.modified)
            };

            if kind == SftpEntryKind::Directory {
                if !visited_directories.insert(remote.clone()) {
                    return Err(
                        io::Error::other(format!("检测到远端符号链接目录循环: {remote}")).into()
                    );
                }
                directories.push(local.clone());
                to_list.push((remote, local));
            } else {
                total = total.saturating_add(size);
                files.push(DownloadFile { remote, local, size, modified });
            }
        }

        // 这一层的目录并发列。结果按输入顺序回来，所以子目录的排列和串行
        // 遍历时一致——上传/下载的顺序不该因为并发而变得不可预期。
        let listings = transfer::map_bounded(
            &to_list,
            limits::DIRECTORY_LISTING_CONCURRENCY,
            |(remote, _)| async move { read_remote_dir(sftp, remote).await },
        )
        .await?;
        for ((_, local), children) in to_list.iter().zip(listings) {
            for child in children {
                pending.push((child, local.clone()));
            }
        }
    }

    Ok(DownloadPlan { directories, files, total })
}

async fn download_remote_entry(
    sftp: &SftpSession,
    mut entry: SftpEntry,
    local_directory: PathBuf,
    options: SftpTransferOptions,
    context: &TaskContext,
) -> SftpResult<()> {
    let mut root_is_directory = entry.kind == SftpEntryKind::Directory;
    let mut root_size = entry.size;
    let mut root_modified = entry.modified;
    if entry.kind == SftpEntryKind::Symlink {
        let target = sftp.read_link(entry.path.clone()).await?;
        let parent = entry.path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("/");
        let metadata = sftp.metadata(normalize_remote_path(parent, &target)).await?;
        root_is_directory = metadata.is_dir();
        root_size = metadata.len();
        root_modified = u64::from(metadata.mtime.unwrap_or(0));
    }
    let desired = local_directory.join(&entry.name);
    let existing = match tokio::fs::symlink_metadata(&desired).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if existing.is_some() {
        if options.skip_unchanged
            && !root_is_directory
            && transaction::local_unchanged(&desired, root_size, root_modified).await?
        {
            context.set_total(root_size);
            context.advance(root_size);
            return Ok(());
        }
        match options.conflict {
            SftpConflictPolicy::Skip => {
                context.set_total(root_size);
                context.advance(root_size);
                return Ok(());
            },
            SftpConflictPolicy::KeepBoth => {
                let duplicate =
                    transaction::duplicate_local_path(&desired, root_is_directory).await?;
                entry.name = duplicate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| io::Error::other("重命名后的本地目标缺少有效文件名"))?
                    .to_owned();
            },
            SftpConflictPolicy::Overwrite => {
                let target_is_directory =
                    existing.as_ref().is_some_and(|metadata| metadata.is_dir());
                let target_is_file = existing.as_ref().is_some_and(|metadata| {
                    metadata.is_file() || metadata.file_type().is_symlink()
                });
                if (root_is_directory && !target_is_directory)
                    || (!root_is_directory && !target_is_file)
                {
                    return Err(io::Error::other(format!(
                        "本地同名目标类型不同，不能直接覆盖: {}",
                        desired.display()
                    ))
                    .into());
                }
            },
        }
    }

    let plan = build_download_plan(sftp, entry, local_directory, context).await?;
    context.set_total(plan.total);

    execute_download_plan(sftp, plan, options.skip_unchanged, context).await
}

async fn execute_download_plan(
    sftp: &SftpSession,
    plan: DownloadPlan,
    skip_unchanged: bool,
    context: &TaskContext,
) -> SftpResult<()> {
    // 与上传对称：目录串行建（`create_dir_all` 自己会补父级，但保持顺序仍然
    // 让失败信息指向真正出问题的那一层），文件并发传。
    for directory in plan.directories {
        context.check_cancelled()?;
        tokio::fs::create_dir_all(directory).await?;
    }
    // 并发度取"计划里真有几个文件"和上限的较小值，窗口再按它摊。单个大文件
    // （最常见、也最吃窗口的场景）因此拿到完整窗口，而不是被目录传输的上限
    // 提前摊薄成四分之一。
    let files = plan.files.len().min(limits::DIRECTORY_FILE_CONCURRENCY).max(1);
    let window = limits::window_per_file(files);
    transfer::run_bounded(&plan.files, files, |file| async move {
        context.check_cancelled()?;
        context.set_label(
            file.local
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.remote.clone()),
        );
        download_file_atomic(
            sftp,
            &file.remote,
            file.size,
            file.modified,
            &file.local,
            window,
            skip_unchanged,
            context,
        )
        .await
    })
    .await?;
    Ok(())
}

fn transfer_staging_root() -> SftpResult<PathBuf> {
    let base = std::env::temp_dir();
    if !base.is_absolute() {
        return Err(io::Error::other(format!(
            "系统临时目录不是绝对路径，已拒绝跨主机复制: {}",
            base.display()
        ))
        .into());
    }
    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    Ok(base.join(format!("nebula-sftp-copy-{nonce:016x}")))
}

async fn copy_remote_entry(
    source: &SftpSession,
    target: &SftpSession,
    entry: SftpEntry,
    target_directory: &str,
    options: SftpTransferOptions,
    context: &TaskContext,
) -> SftpResult<()> {
    if options.skip_unchanged {
        let source_metadata = if entry.kind == SftpEntryKind::Symlink {
            source.metadata(entry.path.clone()).await?
        } else {
            source.symlink_metadata(entry.path.clone()).await?
        };
        if source_metadata.is_regular() {
            let target_path = normalize_remote_path(target_directory, &entry.name);
            if transaction::remote_metadata_optional(target, &target_path).await?.is_some_and(
                |target_metadata| {
                    target_metadata.is_regular()
                        && target_metadata.len() == source_metadata.len()
                        && source_metadata.mtime.is_some()
                        && target_metadata.mtime == source_metadata.mtime
                },
            ) {
                context.set_total(source_metadata.len());
                context.advance(source_metadata.len());
                return Ok(());
            }
        }
    }

    let staging_root = transfer_staging_root()?;
    tokio::fs::create_dir_all(&staging_root).await?;
    let staged_entry = staging_root.join(&entry.name);
    let result = async {
        let download = build_download_plan(source, entry, staging_root.clone(), context).await?;
        let source_bytes = download.total;
        context.set_total(source_bytes.saturating_mul(2));
        execute_download_plan(source, download, false, context).await?;

        let roots =
            resolve_upload_roots(target, vec![staged_entry], target_directory, options).await?;
        let upload = tokio::task::spawn_blocking(move || build_upload_plan(roots))
            .await
            .map_err(|error| format!("扫描跨主机复制 staging 失败: {error}"))??;
        context.set_total(source_bytes.saturating_add(upload.total));
        execute_upload_plan(target, upload, options, context).await
    }
    .await;

    if let Err(error) = tokio::fs::remove_dir_all(&staging_root).await
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("跨主机复制 staging 清理失败（{}）: {error}", staging_root.display());
    }
    result
}

/// 下载一个文件并原子替换本地目标。
///
/// 和上传对称：先写同目录下的隐藏临时文件，完整落地后再改名。中断留下的是
/// 一个可识别的临时文件而不是半个正常文件——用户不会拿着截断的下载结果
/// 当完整文件用。
///
/// `total` 是远端报告的长度，交给引擎切分段并发；`window` 是这个文件可用的
/// 在途请求数（目录传输里要和别的文件分摊）。
async fn download_file_atomic(
    sftp: &SftpSession,
    remote: &str,
    total: u64,
    modified: u64,
    destination: &Path,
    window: usize,
    skip_unchanged: bool,
    context: &TaskContext,
) -> SftpResult<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "下载路径缺少有效文件名"))?;
    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_file_name(format!(".{name}.nebula-download-{nonce:016x}"));
    if skip_unchanged && transaction::local_unchanged(destination, total, modified).await? {
        context.advance(total);
        return Ok(());
    }
    if let Err(error) =
        transfer::download_segmented(sftp, remote, total, &temporary, window, context).await
    {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    if let Err(error) = context.check_cancelled() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    transaction::publish_local_file(temporary, destination, modified, context).await
}

async fn delete_remote_entry(
    sftp: &SftpSession,
    entry: SftpEntry,
    context: &TaskContext,
) -> SftpResult<()> {
    enum Step {
        Visit(SftpEntry),
        RemoveDirectory(String),
    }

    let mut stack = vec![Step::Visit(entry)];
    let mut visited = 0usize;
    while let Some(step) = stack.pop() {
        context.check_cancelled()?;
        visited += 1;
        if visited > MAX_RECURSIVE_ENTRIES {
            return Err(io::Error::other("删除目录超过 100000 项，已停止").into());
        }
        match step {
            Step::Visit(entry) if entry.kind == SftpEntryKind::Directory => {
                let children = read_remote_dir(sftp, &entry.path).await?;
                stack.push(Step::RemoveDirectory(entry.path));
                for child in children.into_iter().rev() {
                    stack.push(Step::Visit(child));
                }
            },
            Step::Visit(entry) => sftp.remove_file(entry.path).await?,
            Step::RemoveDirectory(path) => sftp.remove_dir(path).await?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        SftpEntry, SftpEntryKind, TransferProgress, normalize_remote_path, sort_entries,
        temporary_upload_path, validate_name,
    };

    #[test]
    fn remote_paths_are_posix_and_cannot_escape_root() {
        assert_eq!(normalize_remote_path("/home/dev", "../logs"), "/home/logs");
        assert_eq!(normalize_remote_path("/", "../../etc"), "/etc");
        assert_eq!(normalize_remote_path("/home/dev", r"child\file"), "/home/dev/child/file");
    }

    #[test]
    fn create_and_rename_reject_path_separators_and_special_names() {
        for invalid in ["", ".", "..", "folder/name", r"folder\name", "bad\0name"] {
            assert!(validate_name(invalid).is_err(), "{invalid:?} should be rejected");
        }
        assert_eq!(validate_name("release-assets").unwrap(), "release-assets");
    }

    #[test]
    fn directories_sort_before_files_then_by_name() {
        let mut entries = vec![
            SftpEntry::test("z.txt", SftpEntryKind::File),
            SftpEntry::test("Beta", SftpEntryKind::Directory),
            SftpEntry::test("alpha", SftpEntryKind::Directory),
            SftpEntry::test("A.txt", SftpEntryKind::File),
        ];
        sort_entries(&mut entries);

        assert_eq!(
            entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Beta", "A.txt", "z.txt"]
        );
    }

    #[test]
    fn temporary_upload_stays_beside_destination_and_is_hidden() {
        assert_eq!(
            temporary_upload_path("/home/dev/release.zip", 0x2a),
            "/home/dev/.release.zip.nebula-upload-000000000000002a"
        );
        assert_eq!(
            temporary_upload_path("/release.zip", 0),
            "/.release.zip.nebula-upload-0000000000000000"
        );
    }

    #[test]
    fn transfer_progress_never_exceeds_total() {
        let mut progress = TransferProgress::new("release.zip", 10);
        progress.advance(6);
        progress.advance(8);

        assert_eq!(progress.transferred, 10);
        assert_eq!(progress.fraction(), 1.0);
    }

    #[test]
    fn symlinks_sort_with_files_but_keep_their_kind() {
        let mut entries = vec![
            SftpEntry::test("z-link", SftpEntryKind::Symlink),
            SftpEntry::test("folder", SftpEntryKind::Directory),
            SftpEntry::test("a.txt", SftpEntryKind::File),
        ];
        sort_entries(&mut entries);

        assert_eq!(entries[0].kind, SftpEntryKind::Directory);
        assert_eq!(entries[1].name, "a.txt");
        assert_eq!(entries[2].kind, SftpEntryKind::Symlink);
    }
}
