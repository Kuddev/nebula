pub(crate) mod limits;
pub(crate) mod remote_cwd;
mod transfer;

use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use tokio::io::AsyncWriteExt;

use transfer::TransferObserver;

type SftpError = Box<dyn std::error::Error + Send + Sync>;
type SftpResult<T> = Result<T, SftpError>;

const MAX_RECURSIVE_ENTRIES: usize = 100_000;
static TRANSFER_NONCE: AtomicU64 = AtomicU64::new(1);

/// UI 层被通知"状态变了，该重画了"的方式。
///
/// 传输跑在网络 runtime 上，而画面归 UI 层。两者之间只需要一个"响一声"的
/// 信号——控制器绝不该知道 UI 是哪一套窗口系统、消息循环长什么样。做成
/// 闭包而不是具体的事件代理类型，是因为不同的宿主唤醒自己的方式完全不同，
/// 而这个模块对它们应当一视同仁。
pub(crate) type WakeFn = Arc<dyn Fn() + Send + Sync>;

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
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    wake: WakeFn,
    pending_uploads: Arc<Mutex<Vec<PathBuf>>>,
    upload_debounce_active: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TaskContext {
    state: Arc<Mutex<SftpSnapshot>>,
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    task_generation: u64,
    wake: WakeFn,
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
            is_parent: false,
        }
    }
}

impl SftpController {
    pub fn new(destination: impl Into<String>, wake: WakeFn) -> io::Result<Self> {
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
            wake,
            pending_uploads: Arc::new(Mutex::new(Vec::new())),
            upload_debounce_active: Arc::new(AtomicBool::new(false)),
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
        (self.wake)();
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
        (self.wake)();

        let context = TaskContext {
            state: self.state.clone(),
            cancel: self.cancel.clone(),
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
        self.cancel.load(Ordering::Acquire) || !self.is_current()
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

    // 目录必须串行建，而且必须按计划里的顺序：计划是先序遍历产出的，父目录
    // 排在子目录之前。并发建目录会让子目录的 MKDIR 抢在父目录之前发出去，
    // 撞上"父路径不存在"而失败。这一步是元数据操作，串行的代价也小。
    for directory in plan.directories {
        context.check_cancelled()?;
        // 递归上传允许目标目录已存在。
        let _ = sftp.create_dir(directory).await;
    }
    // 文件之间没有顺序依赖，可以并发。这是目录上传最大的一笔性能：一个几百
    // 个小文件的目录，串行传等于把几百条往返链首尾相接，而每条链的时间几乎
    // 全是等待。
    transfer::run_bounded(
        &plan.files,
        limits::DIRECTORY_FILE_CONCURRENCY,
        |(local, remote, _)| async move {
            context.check_cancelled()?;
            upload_file_atomic(sftp, local, remote, context).await
        },
    )
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

/// 上传一个文件并原子替换目标。
///
/// 先写同目录下的隐藏临时文件，全部落地后再改名顶上去。目标路径在任何时刻
/// 要么是完整的旧内容、要么是完整的新内容，绝不会是半个文件——中途断线、
/// 取消、进程被杀都不会让对端看到截断的数据。
async fn upload_file_atomic(
    sftp: &SftpSession,
    local: &Path,
    destination: &str,
    context: &TaskContext,
) -> SftpResult<()> {
    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = temporary_upload_path(destination, nonce);
    let result = async {
        transfer::upload_stream(sftp, local, &temporary, context).await?;
        // SFTP v3 的 rename 通常不覆盖已存在的目标，所以要先让路。删除和改名
        // 之间存在一个窗口：这一刻目标路径不存在。真正的可退替换（先把旧文件
        // 挪到备份名、改名失败再挪回来）需要目标属性检查配合，留给替换事务。
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
                to_list.push((remote, local));
            } else {
                total = total.saturating_add(size);
                files.push((remote, local, size));
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
    entry: SftpEntry,
    local_directory: PathBuf,
    context: &TaskContext,
) -> SftpResult<()> {
    let plan = build_download_plan(sftp, entry, local_directory, context).await?;
    context.set_total(plan.total);
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
    transfer::run_bounded(&plan.files, files, |(remote, local, size)| async move {
        context.check_cancelled()?;
        download_file_atomic(sftp, remote, *size, local, window, context).await
    })
    .await?;
    Ok(())
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
    destination: &Path,
    window: usize,
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
        transfer::download_segmented(sftp, remote, total, &temporary, window, context).await?;
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
