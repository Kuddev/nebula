use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use crate::event::{Event, EventType};

type SftpError = Box<dyn std::error::Error + Send + Sync>;
type SftpResult<T> = Result<T, SftpError>;

const TRANSFER_CHUNK: usize = 256 * 1024;
const MAX_RECURSIVE_ENTRIES: usize = 100_000;
static TRANSFER_NONCE: AtomicU64 = AtomicU64::new(1);

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
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    proxy: EventLoopProxy<Event>,
    window_id: WindowId,
}

#[derive(Clone)]
struct TaskContext {
    state: Arc<Mutex<SftpSnapshot>>,
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    task_generation: u64,
    proxy: EventLoopProxy<Event>,
    window_id: WindowId,
    last_wake: Arc<Mutex<Instant>>,
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
        }
    }
}

impl SftpController {
    pub fn new(
        destination: impl Into<String>,
        proxy: EventLoopProxy<Event>,
        window_id: WindowId,
    ) -> io::Result<Self> {
        crate::ssh_session::runtime()?;
        let controller = Self {
            state: Arc::new(Mutex::new(SftpSnapshot {
                destination: destination.into(),
                path: ".".to_owned(),
                entries: Vec::new(),
                phase: SftpPhase::Connecting,
                error: None,
                progress: None,
            })),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            proxy,
            window_id,
        };
        controller.refresh(".");
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
                upload_local_paths(&sftp, local_paths, &remote_dir, &context).await?;
                let entries = read_remote_dir(&sftp, &remote_dir).await?;
                Ok((remote_dir, entries))
            },
        );
    }

    pub fn download(&self, entry: SftpEntry, local_directory: PathBuf) {
        let snapshot = self.snapshot();
        let destination = snapshot.destination;
        let path = snapshot.path;
        let progress = TransferProgress::new(entry.name.clone(), entry.size);
        self.start_job(SftpPhase::Working, Some(progress), move |context| async move {
            let sftp = crate::ssh_session::open_sftp(&destination).await?;
            download_remote_entry(&sftp, entry, local_directory, &context).await?;
            let entries = read_remote_dir(&sftp, &path).await?;
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
        self.cancel.store(true, Ordering::Release);
        let mut state = lock(&self.state);
        if state.phase == SftpPhase::Working {
            state.error = Some("正在取消传输…".to_owned());
        }
        drop(state);
        wake(&self.proxy, self.window_id);
    }

    fn start_job<J, F>(&self, phase: SftpPhase, progress: Option<TransferProgress>, job: J)
    where
        J: FnOnce(TaskContext) -> F + Send + 'static,
        F: Future<Output = SftpResult<(String, Vec<SftpEntry>)>> + Send + 'static,
    {
        self.cancel.store(false, Ordering::Release);
        let task_generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut state = lock(&self.state);
            state.phase = phase;
            state.error = None;
            state.progress = progress;
        }
        wake(&self.proxy, self.window_id);

        let context = TaskContext {
            state: self.state.clone(),
            cancel: self.cancel.clone(),
            generation: self.generation.clone(),
            task_generation,
            proxy: self.proxy.clone(),
            window_id: self.window_id,
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

impl TaskContext {
    fn check_cancelled(&self) -> SftpResult<()> {
        if self.cancel.load(Ordering::Acquire) || !self.is_current() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "操作已取消").into())
        } else {
            Ok(())
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

    fn advance(&self, bytes: u64) {
        if !self.is_current() {
            return;
        }
        if let Some(progress) = lock(&self.state).progress.as_mut() {
            progress.advance(bytes);
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
            wake(&self.proxy, self.window_id);
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
        wake(&self.proxy, self.window_id);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wake(proxy: &EventLoopProxy<Event>, window_id: WindowId) {
    let _ = proxy.send_event(Event::new(EventType::SftpUpdated, window_id));
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
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

#[derive(Debug)]
struct UploadPlan {
    directories: Vec<String>,
    files: Vec<(PathBuf, String, u64)>,
    total: u64,
}

fn build_upload_plan(local_paths: Vec<PathBuf>, remote_dir: String) -> SftpResult<UploadPlan> {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut stack = Vec::new();

    for local in local_paths.into_iter().rev() {
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "本地路径缺少有效名称"))?
            .to_owned();
        validate_name(&name)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        stack.push((local, normalize_remote_path(&remote_dir, &name)));
    }

    while let Some((local, remote)) = stack.pop() {
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
                stack.push((child.path(), normalize_remote_path(&remote, &name)));
            }
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            files.push((local, remote, metadata.len()));
        }
    }

    Ok(UploadPlan { directories, files, total })
}

async fn upload_local_paths(
    sftp: &SftpSession,
    local_paths: Vec<PathBuf>,
    remote_dir: &str,
    context: &TaskContext,
) -> SftpResult<()> {
    let remote_dir = remote_dir.to_owned();
    let plan = tokio::task::spawn_blocking(move || build_upload_plan(local_paths, remote_dir))
        .await
        .map_err(|err| format!("扫描上传目录失败: {err}"))??;
    context.set_total(plan.total);

    for directory in plan.directories {
        context.check_cancelled()?;
        // 与 Tabby 一致：递归上传允许目标目录已存在。
        let _ = sftp.create_dir(directory).await;
    }
    for (local, remote, _) in plan.files {
        context.check_cancelled()?;
        upload_file_atomic(sftp, &local, &remote, context).await?;
    }
    Ok(())
}

async fn upload_file_atomic(
    sftp: &SftpSession,
    local: &Path,
    destination: &str,
    context: &TaskContext,
) -> SftpResult<()> {
    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = temporary_upload_path(destination, nonce);
    let result = async {
        let mut source = tokio::fs::File::open(local).await?;
        let mut target = sftp
            .open_with_flags(
                temporary.clone(),
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await?;
        let mut buffer = vec![0u8; TRANSFER_CHUNK];
        loop {
            context.check_cancelled()?;
            let count = source.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count]).await?;
            context.advance(count as u64);
        }
        target.shutdown().await?;

        // SFTP v3 rename通常不覆盖；与 Tabby 一样先删除旧目标，再替换临时文件。
        let _ = sftp.remove_file(destination.to_owned()).await;
        sftp.rename(temporary.clone(), destination.to_owned()).await?;
        Ok::<_, SftpError>(())
    }
    .await;

    if result.is_err() {
        let _ = sftp.remove_file(temporary).await;
    }
    result
}

#[derive(Debug)]
struct DownloadPlan {
    directories: Vec<PathBuf>,
    files: Vec<(String, PathBuf, u64)>,
    total: u64,
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
    let mut stack = vec![(entry, local_directory)];
    let mut visited_directories = HashSet::new();

    while let Some((entry, parent)) = stack.pop() {
        context.check_cancelled()?;
        if files.len() + directories.len() >= MAX_RECURSIVE_ENTRIES {
            return Err(io::Error::other("下载目录超过 100000 项，已停止").into());
        }
        validate_name(&entry.name)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
        let local = parent.join(&entry.name);
        let (remote, kind, size) = if entry.kind == SftpEntryKind::Symlink {
            let target = sftp.read_link(entry.path.clone()).await?;
            let parent = entry.path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("/");
            let target = normalize_remote_path(parent, &target);
            let metadata = sftp.metadata(target.clone()).await?;
            let kind =
                if metadata.is_dir() { SftpEntryKind::Directory } else { SftpEntryKind::File };
            (target, kind, metadata.len())
        } else {
            (entry.path, entry.kind, entry.size)
        };

        if kind == SftpEntryKind::Directory {
            if !visited_directories.insert(remote.clone()) {
                return Err(
                    io::Error::other(format!("检测到远端符号链接目录循环: {remote}")).into()
                );
            }
            directories.push(local.clone());
            let children = read_remote_dir(sftp, &remote).await?;
            for child in children.into_iter().rev() {
                stack.push((child, local.clone()));
            }
        } else {
            total = total.saturating_add(size);
            files.push((remote, local, size));
        }
    }

    Ok(DownloadPlan { directories, files, total })
}

async fn download_remote_entry(
    sftp: &SftpSession,
    entry: SftpEntry,
    local_directory: PathBuf,
    context: &TaskContext,
) -> SftpResult<()> {
    let plan = build_download_plan(sftp, entry, local_directory, context).await?;
    context.set_total(plan.total);
    for directory in plan.directories {
        context.check_cancelled()?;
        tokio::fs::create_dir_all(directory).await?;
    }
    for (remote, local, _) in plan.files {
        context.check_cancelled()?;
        download_file_atomic(sftp, &remote, &local, context).await?;
    }
    Ok(())
}

async fn download_file_atomic(
    sftp: &SftpSession,
    remote: &str,
    destination: &Path,
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
    let result = async {
        let mut source = sftp.open(remote.to_owned()).await?;
        let mut target = tokio::fs::File::create(&temporary).await?;
        let mut buffer = vec![0u8; TRANSFER_CHUNK];
        loop {
            context.check_cancelled()?;
            let count = source.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count]).await?;
            context.advance(count as u64);
        }
        target.flush().await?;
        target.shutdown().await?;
        let _ = tokio::fs::remove_file(destination).await;
        tokio::fs::rename(&temporary, destination).await?;
        Ok::<_, SftpError>(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(temporary).await;
    }
    result
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
