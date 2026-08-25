//! GPUI 更新安装包的下载、校验与启动。
//!
//! 更新检查只负责提供 release 元数据；本模块再次收紧资产合同，并把大文件
//! 流式写入同目录 `.part` 文件。只有长度、PE 文件头与 SHA-256 全部通过后，
//! 才原子替换为可启动的安装包，避免中断下载或错误响应变成可执行文件。

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use sha2::{Digest as _, Sha256};

use crate::update_check::UpdateAsset;

const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/Kuddev/nebula/releases/download/";
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;

static DOWNLOAD_SESSION: Mutex<Option<DownloadSession>> = Mutex::new(None);

#[derive(Clone, Debug)]
pub(crate) enum DownloadStatus {
    Idle,
    Downloading { downloaded: u64, total: Option<u64> },
    Ready { path: PathBuf, bytes: u64 },
    Failed(String),
}

impl DownloadStatus {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready { .. } | Self::Failed(_))
    }
}

#[derive(Clone, Debug)]
struct DownloadSession {
    asset: UpdateAsset,
    status: DownloadStatus,
}

fn session() -> MutexGuard<'static, Option<DownloadSession>> {
    DOWNLOAD_SESSION.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(crate) fn status(asset: &UpdateAsset) -> DownloadStatus {
    session()
        .as_ref()
        .filter(|current| current.asset == *asset)
        .map(|current| current.status.clone())
        .unwrap_or(DownloadStatus::Idle)
}

/// 将当前资产切换到下载态。`false` 表示同一资产已经在下载或已经校验完成。
pub(crate) fn begin(asset: &UpdateAsset) -> Result<bool, String> {
    validate_asset(asset)?;
    let mut current = session();
    if let Some(existing) = current.as_ref().filter(|existing| existing.asset == *asset)
        && matches!(
            existing.status,
            DownloadStatus::Downloading { .. } | DownloadStatus::Ready { .. }
        )
    {
        return Ok(false);
    }
    *current = Some(DownloadSession {
        asset: asset.clone(),
        status: DownloadStatus::Downloading { downloaded: 0, total: asset.size },
    });
    Ok(true)
}

/// 在后台执行器线程调用；进度直接写入进程内会话，UI 以低频轮询刷新。
pub(crate) fn run(asset: UpdateAsset) {
    let outcome = download_and_verify(&asset);
    let mut current = session();
    let Some(current) = current.as_mut().filter(|current| current.asset == asset) else {
        return;
    };
    current.status = match outcome {
        Ok((path, bytes)) => DownloadStatus::Ready { path, bytes },
        Err(error) => DownloadStatus::Failed(error),
    };
}

pub(crate) fn launch_ready(asset: &UpdateAsset) -> Result<(), String> {
    let path = match status(asset) {
        DownloadStatus::Ready { path, .. } => path,
        _ => return Err("安装包尚未下载并通过校验".to_owned()),
    };
    let (_, expected_path) = download_paths(asset)?;
    if path != expected_path || !path.is_file() {
        return Err("已校验的安装包不存在或路径已改变".to_owned());
    }
    // Ready 只表示下载完成时通过过校验；安装前再读一遍，避免缓存文件在
    // 弹窗等待用户确认期间被替换后仍直接执行。
    verify_file(&path, asset).map_err(|error| format!("安装前重新校验失败：{error}"))?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        Command::new(&path)
            // 安装向导必须可见；这里只切断旧进程的标准流并让安装器独立存活。
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|error| format!("无法启动更新安装包：{error}"))?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Err("当前平台暂不支持应用内安装更新".to_owned())
    }
}

fn download_and_verify(asset: &UpdateAsset) -> Result<(PathBuf, u64), String> {
    validate_asset(asset)?;
    let (partial_path, final_path) = download_paths(asset)?;
    let _download_lock = crate::atomic_file::try_lifetime_lock(&final_path)
        .map_err(|error| format!("无法锁定更新下载目录：{error}"))?
        .ok_or_else(|| "另一个 Nebula 进程正在下载这项更新".to_owned())?;

    if final_path.is_file()
        && let Ok(bytes) = verify_file(&final_path, asset)
    {
        return Ok((final_path, bytes));
    }

    let result = download_to_partial(asset, &partial_path).and_then(|bytes| {
        crate::atomic_file::replace(&partial_path, &final_path)
            .map_err(|error| format!("无法保存已校验的更新安装包：{error}"))?;
        Ok((final_path.clone(), bytes))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial_path);
    }
    result
}

fn download_to_partial(asset: &UpdateAsset, partial_path: &Path) -> Result<u64, String> {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .build()
        .new_agent();
    let mut response = agent
        .get(&asset.download_url)
        .header("User-Agent", "nebula-terminal-updater")
        .header("Accept", "application/octet-stream")
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|error| format!("下载安装包失败：{error}"))?;

    let response_size = response.body().content_length();
    if let (Some(expected), Some(actual)) = (asset.size, response_size)
        && expected != actual
    {
        return Err(format!("安装包长度与 release 元数据不一致（{actual} / {expected} 字节）"));
    }
    let total = asset.size.or(response_size);
    if total.is_some_and(|bytes| bytes > MAX_INSTALLER_BYTES) {
        return Err("安装包超过 512 MiB 安全上限".to_owned());
    }

    let mut output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(partial_path)
        .map_err(|error| format!("无法创建更新临时文件：{error}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut pe_header = Vec::with_capacity(2);
    let mut buffer = vec![0_u8; DOWNLOAD_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| format!("读取安装包失败：{error}"))?;
        if read == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(read as u64);
        if downloaded > MAX_INSTALLER_BYTES {
            return Err("安装包超过 512 MiB 安全上限".to_owned());
        }
        if pe_header.len() < 2 {
            let take = (2 - pe_header.len()).min(read);
            pe_header.extend_from_slice(&buffer[..take]);
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|error| format!("写入更新临时文件失败：{error}"))?;
        set_progress(asset, downloaded, total);
    }
    output.sync_all().map_err(|error| format!("同步更新临时文件失败：{error}"))?;

    verify_download(downloaded, &pe_header, hasher.finalize(), asset)?;
    Ok(downloaded)
}

fn verify_file(path: &Path, asset: &UpdateAsset) -> Result<u64, String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取更新缓存：{error}"))?;
    let metadata = file.metadata().map_err(|error| format!("无法读取更新缓存大小：{error}"))?;
    let bytes = metadata.len();
    if bytes > MAX_INSTALLER_BYTES {
        return Err("更新缓存超过 512 MiB 安全上限".to_owned());
    }
    let mut hasher = Sha256::new();
    let mut pe_header = Vec::with_capacity(2);
    let mut buffer = vec![0_u8; DOWNLOAD_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|error| format!("读取更新缓存失败：{error}"))?;
        if read == 0 {
            break;
        }
        if pe_header.len() < 2 {
            let take = (2 - pe_header.len()).min(read);
            pe_header.extend_from_slice(&buffer[..take]);
        }
        hasher.update(&buffer[..read]);
    }
    verify_download(bytes, &pe_header, hasher.finalize(), asset)?;
    Ok(bytes)
}

fn verify_download(
    bytes: u64,
    pe_header: &[u8],
    digest: impl AsRef<[u8]>,
    asset: &UpdateAsset,
) -> Result<(), String> {
    if bytes == 0 || asset.size.is_some_and(|expected| expected != bytes) {
        return Err(format!("安装包长度校验失败（实际 {bytes} 字节）"));
    }
    if pe_header != b"MZ" {
        return Err("下载内容不是 Windows PE 安装包".to_owned());
    }
    let expected = asset.sha256.as_deref().ok_or_else(|| "release 未提供 SHA-256".to_owned())?;
    let mut actual = String::with_capacity(64);
    for byte in digest.as_ref() {
        let _ = write!(&mut actual, "{byte:02x}");
    }
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!("安装包 SHA-256 校验失败（实际 {actual}）"));
    }
    Ok(())
}

fn set_progress(asset: &UpdateAsset, downloaded: u64, total: Option<u64>) {
    let mut current = session();
    if let Some(current) = current.as_mut().filter(|current| current.asset == *asset) {
        current.status = DownloadStatus::Downloading { downloaded, total };
    }
}

fn download_paths(asset: &UpdateAsset) -> Result<(PathBuf, PathBuf), String> {
    let directory = nebula_settings::settings_dir().join("updates");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建更新下载目录：{error}"))?;
    let final_path = directory.join(&asset.name);
    let partial_path = directory.join(format!("{}.part", asset.name));
    Ok((partial_path, final_path))
}

fn validate_asset(asset: &UpdateAsset) -> Result<(), String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("当前平台没有可用的自动更新安装包".to_owned());
    }
    validate_windows_asset_contract(asset)
}

fn validate_windows_asset_contract(asset: &UpdateAsset) -> Result<(), String> {
    if asset.version.is_empty()
        || !asset
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err("release 版本号不符合安装包命名规则".to_owned());
    }
    let expected_name = format!("NebulaTerminal-{}-windows-x64-setup.exe", asset.version);
    if asset.name != expected_name {
        return Err("release 资产不是当前平台的精确安装包".to_owned());
    }
    if !asset.download_url.starts_with(RELEASE_DOWNLOAD_PREFIX)
        || !asset.download_url.ends_with(&format!("/{expected_name}"))
    {
        return Err("release 安装包 URL 不属于 Nebula 官方仓库".to_owned());
    }
    if asset.size.is_some_and(|bytes| bytes == 0 || bytes > MAX_INSTALLER_BYTES) {
        return Err("release 安装包大小无效".to_owned());
    }
    let hash = asset.sha256.as_deref().ok_or_else(|| {
        "release 未提供可验证的 SHA-256；为避免执行未知安装包，已停止自动下载".to_owned()
    })?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("release 提供的 SHA-256 格式无效".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{UpdateAsset, validate_windows_asset_contract};

    fn asset(url: &str, sha256: Option<&str>) -> UpdateAsset {
        UpdateAsset {
            version: "1.4.0".to_owned(),
            name: "NebulaTerminal-1.4.0-windows-x64-setup.exe".to_owned(),
            download_url: url.to_owned(),
            size: Some(42),
            sha256: sha256.map(str::to_owned),
        }
    }

    #[test]
    fn accepts_exact_official_asset_contract() {
        let url = "https://github.com/Kuddev/nebula/releases/download/v1.4.0/NebulaTerminal-1.4.0-windows-x64-setup.exe";
        let hash = "a".repeat(64);
        assert!(validate_windows_asset_contract(&asset(url, Some(hash.as_str()))).is_ok());
    }

    #[test]
    fn rejects_untrusted_url_or_missing_digest() {
        let official = "https://github.com/Kuddev/nebula/releases/download/v1.4.0/NebulaTerminal-1.4.0-windows-x64-setup.exe";
        let untrusted = "https://example.invalid/NebulaTerminal-1.4.0-windows-x64-setup.exe";
        let hash = "a".repeat(64);

        assert!(validate_windows_asset_contract(&asset(untrusted, Some(hash.as_str()))).is_err());
        assert!(validate_windows_asset_contract(&asset(official, None)).is_err());
    }
}
