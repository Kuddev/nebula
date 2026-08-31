//! SFTP 传输的最终发布事务。
//!
//! 网络传输只负责把完整字节写进 staging；本模块负责决定 staging 如何成为
//! 用户可见目标。把这两层分开，上传、下载和跨主机复制才能共享同一套备份、
//! 回滚与符号链接语义。

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as ClientError;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::transfer::TransferObserver as _;
use super::{SftpError, SftpResult, TRANSFER_NONCE, TaskContext, normalize_remote_path, transfer};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    pub len: u64,
    pub modified: Option<u32>,
    pub permissions: Option<u32>,
}

impl FileStamp {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("本地源不是普通文件: {}", path.display()),
            ));
        }
        let modified = metadata.modified().ok().and_then(system_time_seconds);
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt as _;
            Some(metadata.permissions().mode())
        };
        #[cfg(not(unix))]
        let permissions = None;
        Ok(Self { len: metadata.len(), modified, permissions })
    }
}

fn system_time_seconds(time: SystemTime) -> Option<u32> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(u32::try_from(seconds).unwrap_or(u32::MAX))
}

pub(super) async fn remote_metadata_optional(
    sftp: &SftpSession,
    path: &str,
) -> SftpResult<Option<FileAttributes>> {
    match sftp.symlink_metadata(path.to_owned()).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(ClientError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
            Ok(None)
        },
        Err(error) => Err(error.into()),
    }
}

pub(super) fn remote_unchanged(stamp: FileStamp, metadata: &FileAttributes) -> bool {
    metadata.is_regular()
        && metadata.len() == stamp.len
        && stamp.modified.is_some()
        && metadata.mtime == stamp.modified
}

pub(super) async fn local_unchanged(
    path: &Path,
    remote_size: u64,
    remote_mtime: u64,
) -> io::Result<bool> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() != remote_size || remote_mtime == 0 {
        return Ok(false);
    }
    Ok(metadata.modified().ok().and_then(system_time_seconds).map(u64::from) == Some(remote_mtime))
}

fn remote_sibling_path(destination: &str, marker: &str, nonce: u64) -> String {
    let normalized = normalize_remote_path("/", destination);
    let (parent, name) = normalized.rsplit_once('/').unwrap_or(("", normalized.as_str()));
    let parent = if parent.is_empty() { "/" } else { parent };
    let separator = if parent == "/" { "" } else { "/" };
    format!("{parent}{separator}.{name}.{marker}-{nonce:016x}")
}

fn local_sibling_path(destination: &Path, marker: &str, nonce: u64) -> io::Result<PathBuf> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径缺少有效文件名"))?;
    Ok(destination.with_file_name(format!(".{name}.{marker}-{nonce:016x}")))
}

pub(super) fn duplicate_name(name: &str, is_directory: bool, index: usize) -> String {
    let (base, extension) = if is_directory {
        (name, "")
    } else {
        match name.rfind('.').filter(|position| *position > 0) {
            Some(position) => (&name[..position], &name[position..]),
            None => (name, ""),
        }
    };
    let suffix = if index == 1 { " (copy)".to_owned() } else { format!(" (copy {index})") };
    format!("{base}{suffix}{extension}")
}

pub(super) async fn duplicate_local_path(
    destination: &Path,
    is_directory: bool,
) -> io::Result<PathBuf> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径缺少有效文件名"))?;
    for index in 1..1000 {
        let candidate = destination.with_file_name(duplicate_name(name, is_directory, index));
        match tokio::fs::symlink_metadata(&candidate).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {},
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("无法为“保留两者”找到可用名称: {}", destination.display()),
    ))
}

async fn set_remote_staging_metadata(
    sftp: &SftpSession,
    staging: &str,
    source: FileStamp,
    existing: Option<&FileAttributes>,
) -> SftpResult<()> {
    if let Some(permissions) =
        existing.and_then(|metadata| metadata.permissions).or(source.permissions)
    {
        let mut attributes = FileAttributes::empty();
        attributes.permissions = Some(permissions);
        sftp.set_metadata(staging.to_owned(), attributes).await?;
    }
    if let Some(modified) = source.modified {
        let mut attributes = FileAttributes::empty();
        attributes.atime = Some(modified);
        attributes.mtime = Some(modified);
        if let Err(error) = sftp.set_metadata(staging.to_owned(), attributes).await {
            // 时间戳失败只会让 skip-unchanged 少命中，不能在完整 staging 已写好后
            // 把一次新文件上传判成失败；已有目标的权限恢复则在上面严格执行。
            log::warn!("无法恢复远端文件时间戳（{staging}）: {error}");
        }
    }
    Ok(())
}

pub(super) async fn upload_file(
    sftp: &SftpSession,
    local: &Path,
    destination: &str,
    source: FileStamp,
    skip_unchanged: bool,
    context: &TaskContext,
) -> SftpResult<()> {
    let existing = remote_metadata_optional(sftp, destination).await?;
    if skip_unchanged {
        let comparable = match existing.as_ref() {
            Some(metadata) if metadata.is_symlink() => {
                Some(sftp.metadata(destination.to_owned()).await?)
            },
            Some(metadata) => Some(metadata.clone()),
            None => None,
        };
        if comparable.as_ref().is_some_and(|metadata| remote_unchanged(source, metadata)) {
            context.advance(source.len);
            return Ok(());
        }
    }

    if existing.as_ref().is_some_and(FileAttributes::is_dir) {
        return Err(
            io::Error::other(format!("远端目标是目录，不能用文件覆盖: {destination}")).into()
        );
    }
    if existing.as_ref().is_some_and(FileAttributes::is_symlink) {
        // rename 会替换链接节点本身。原地写让服务器沿链接打开真实目标，保留
        // 链接身份；代价是这条路径无法提供 rename 级原子性。
        transfer::upload_stream(sftp, local, destination, context).await?;
        return Ok(());
    }

    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    let staging = remote_sibling_path(destination, "nebula-upload", nonce);
    let backup = remote_sibling_path(destination, "nebula-backup", nonce);
    let mut preserve_staging = false;
    let result = async {
        transfer::upload_stream(sftp, local, &staging, context).await?;
        set_remote_staging_metadata(sftp, &staging, source, existing.as_ref()).await?;
        let _publish = context.begin_publish()?;

        if existing.is_none() {
            sftp.rename(staging.clone(), destination.to_owned()).await?;
            return Ok(());
        }

        sftp.rename(destination.to_owned(), backup.clone()).await?;
        if let Err(publish_error) = sftp.rename(staging.clone(), destination.to_owned()).await {
            if let Err(restore_error) = sftp.rename(backup.clone(), destination.to_owned()).await {
                preserve_staging = true;
                return Err(io::Error::other(format!(
                    "替换远端文件失败，且旧文件自动恢复失败；旧文件位于 {backup}，新文件位于 {staging}。发布错误: {publish_error}；恢复错误: {restore_error}"
                ))
                .into());
            }
            return Err(publish_error.into());
        }
        if let Err(error) = sftp.remove_file(backup.clone()).await {
            log::warn!("远端替换已完成，但旧文件备份清理失败（{backup}）: {error}");
        }
        Ok(())
    }
    .await;

    if result.is_err() && !preserve_staging {
        let _ = sftp.remove_file(staging).await;
    }
    result
}

async fn set_local_modified(path: PathBuf, modified: u64) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        let time = UNIX_EPOCH + std::time::Duration::from_secs(modified);
        file.set_times(std::fs::FileTimes::new().set_modified(time))
    })
    .await
    .map_err(|error| io::Error::other(format!("设置本地文件时间戳任务失败: {error}")))?
}

pub(super) async fn publish_local_file(
    staging: PathBuf,
    destination: &Path,
    modified: u64,
    context: &TaskContext,
) -> SftpResult<()> {
    let existing = match tokio::fs::symlink_metadata(destination).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    if existing.as_ref().is_some_and(|metadata| metadata.file_type().is_symlink()) {
        let write_result = async {
            context.check_cancelled()?;
            let mut source = tokio::fs::File::open(&staging).await?;
            let mut target =
                tokio::fs::OpenOptions::new().write(true).truncate(true).open(destination).await?;
            let mut buffer = vec![0; 32 * 1024];
            loop {
                context.check_cancelled()?;
                let count = source.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                target.write_all(&buffer[..count]).await?;
            }
            target.flush().await?;
            target.shutdown().await?;
            Ok::<_, SftpError>(())
        }
        .await;
        if let Err(error) = write_result {
            return Err(io::Error::other(format!(
                "写入本地符号链接目标失败；完整下载保留在 {}。错误: {error}",
                staging.display()
            ))
            .into());
        }
        let _ = tokio::fs::remove_file(&staging).await;
        if modified != 0
            && let Err(error) = set_local_modified(destination.to_owned(), modified).await
        {
            log::warn!("无法恢复本地文件时间戳（{}）: {error}", destination.display());
        }
        return Ok(());
    }
    if existing.as_ref().is_some_and(|metadata| !metadata.is_file()) {
        return Err(io::Error::other(format!(
            "本地目标不是普通文件，不能直接覆盖: {}",
            destination.display()
        ))
        .into());
    }
    if let Some(metadata) = existing.as_ref() {
        tokio::fs::set_permissions(&staging, metadata.permissions()).await?;
    }

    let nonce = TRANSFER_NONCE.fetch_add(1, Ordering::Relaxed);
    let backup = local_sibling_path(destination, "nebula-backup", nonce)?;
    let mut preserve_staging = false;
    let result = async {
        let _publish = context.begin_publish()?;
        if existing.is_none() {
            tokio::fs::rename(&staging, destination).await?;
            return Ok::<_, SftpError>(());
        }

        tokio::fs::rename(destination, &backup).await?;
        if let Err(publish_error) = tokio::fs::rename(&staging, destination).await {
            if let Err(restore_error) = tokio::fs::rename(&backup, destination).await {
                preserve_staging = true;
                return Err(io::Error::other(format!(
                    "替换本地文件失败，且旧文件自动恢复失败；旧文件位于 {}，新文件位于 {}。发布错误: {publish_error}；恢复错误: {restore_error}",
                    backup.display(),
                    staging.display()
                ))
                .into());
            }
            return Err(publish_error.into());
        }
        if let Err(error) = tokio::fs::remove_file(&backup).await {
            log::warn!("本地替换已完成，但旧文件备份清理失败（{}）: {error}", backup.display());
        }
        Ok(())
    }
    .await;

    if result.is_err() && !preserve_staging {
        let _ = tokio::fs::remove_file(&staging).await;
    }
    result?;
    if modified != 0
        && let Err(error) = set_local_modified(destination.to_owned(), modified).await
    {
        log::warn!("无法恢复本地文件时间戳（{}）: {error}", destination.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{duplicate_name, local_sibling_path, remote_sibling_path};

    #[test]
    fn duplicate_names_keep_file_extensions() {
        assert_eq!(duplicate_name("report.txt", false, 1), "report (copy).txt");
        assert_eq!(duplicate_name("archive.tar.gz", false, 2), "archive.tar (copy 2).gz");
        assert_eq!(duplicate_name(".env", false, 1), ".env (copy)");
        assert_eq!(duplicate_name("folder", true, 1), "folder (copy)");
    }

    #[test]
    fn transaction_paths_stay_next_to_destination() {
        assert_eq!(
            remote_sibling_path("/srv/report.txt", "nebula-backup", 42),
            "/srv/.report.txt.nebula-backup-000000000000002a"
        );
        assert_eq!(
            remote_sibling_path("/report.txt", "nebula-staging", 1),
            "/.report.txt.nebula-staging-0000000000000001"
        );
        assert_eq!(
            local_sibling_path(Path::new("C:/data/report.txt"), "nebula-backup", 42).unwrap(),
            Path::new("C:/data/.report.txt.nebula-backup-000000000000002a")
        );
    }
}
