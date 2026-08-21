//! 多协议远程备份：把 [`crate::encrypted_backup`] 的加密归档（NEBUBAK1，
//! Argon2id + AES-256-GCM，服务器只见密文）推到远端，或取回最新一份恢复。
//!
//! 协议后端：
//! - 本地/网络目录（含 NAS 的 UNC 路径）
//! - WebDAV（PUT/GET/PROPFIND/DELETE，Basic 认证，同 `sync.rs` 的栈）
//! - S3 兼容（SigV4 签名，路径式 URL——AWS/MinIO/R2/B2/OSS 通吃）
//! - SFTP（复用既有 SSH 会话栈与已保存主机的认证）
//!
//! GitHub/Google Drive/OneDrive 三个 OAuth 后端需要浏览器授权回环，桌面终端里
//! 先不做。归档按 UTC 时间戳命名（字典序即时间序），
//! 每个远端保留最近 [`KEEP_ARCHIVES`] 份，多出的在推送成功后尽力清理。
//! 网络/密钥派生都在调用方的后台线程阻塞完成（与 `sync.rs` 同一模型）。

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::warn;

/// 远程备份配置文件（位于 `nebula_data_dir()`），独立于 `nebula_sync.txt`：
/// 备份管道自身的配置不该被备份恢复覆盖到不可用。
const CONFIG_FILE: &str = "nebula_backup.txt";

/// 归档文件名：`nebula-backup-YYYYMMDD-HHMMSS.nbk`（UTC）。前后缀是列表
/// 过滤的安全边界——清理绝不会碰远端目录里不带这两段的文件。
const ARCHIVE_PREFIX: &str = "nebula-backup-";
const ARCHIVE_SUFFIX: &str = ".nbk";

/// 每个远端保留的归档份数。
const KEEP_ARCHIVES: usize = 10;

/// Windows 凭据管理器条目（与 `sync.rs` 的通用 DPAPI 存取同一后端）。
#[cfg(windows)]
const WEBDAV_PASSWORD_TARGET: &str = "Nebula Backup WebDAV Password";
#[cfg(windows)]
const S3_SECRET_TARGET: &str = "Nebula Backup S3 Secret Key";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupProtocol {
    #[default]
    Off,
    Folder,
    WebDav,
    S3,
    Sftp,
}

impl BackupProtocol {
    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "folder" => Some(Self::Folder),
            "webdav" => Some(Self::WebDav),
            "s3" => Some(Self::S3),
            "sftp" => Some(Self::Sftp),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Folder => "folder",
            Self::WebDav => "webdav",
            Self::S3 => "s3",
            Self::Sftp => "sftp",
        }
    }
}

/// `nebula_backup.txt` 的解析结果。每个协议的字段独立保存：来回切换协议
/// 不丢已填的配置。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackupRemoteConfig {
    pub protocol: BackupProtocol,
    pub folder_path: String,
    pub webdav_url: String,
    pub webdav_username: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    /// 原样保存的「桶/前缀」串（如 `my-bucket/nebula`），首个 `/` 前是桶。
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub sftp_destination: String,
    pub sftp_path: String,
    /// 手写豁免开关（UI 故意不给入口）：自建内网 WebDAV/S3 允许 http。
    pub allow_http: bool,
}

pub fn config_path() -> PathBuf {
    crate::display::nebula_data_dir().join(CONFIG_FILE)
}

/// 设置页输入槽位数（协议决定语义）。密文槽（WebDAV 密码 / S3 Secret）
/// 不进配置文件，由凭据管理器承接。
pub fn field_count(protocol: BackupProtocol) -> usize {
    match protocol {
        BackupProtocol::Off => 0,
        BackupProtocol::Folder => 1,
        BackupProtocol::WebDav => 3,
        BackupProtocol::S3 => 5,
        BackupProtocol::Sftp => 2,
    }
}

/// 密文槽位下标（有的协议没有：目录不需要凭据，SFTP 复用 SSH 主机认证）。
pub fn secret_field(protocol: BackupProtocol) -> Option<usize> {
    match protocol {
        BackupProtocol::WebDav => Some(2),
        BackupProtocol::S3 => Some(4),
        _ => None,
    }
}

impl BackupRemoteConfig {
    pub fn load() -> Self {
        Self::parse(&std::fs::read_to_string(config_path()).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "protocol" => {
                    if let Some(protocol) = BackupProtocol::from_settings(value) {
                        cfg.protocol = protocol;
                    }
                },
                "folder_path" => cfg.folder_path = value.to_owned(),
                "webdav_url" => cfg.webdav_url = value.to_owned(),
                "webdav_username" => cfg.webdav_username = value.to_owned(),
                "s3_endpoint" => cfg.s3_endpoint = value.to_owned(),
                "s3_region" => cfg.s3_region = value.to_owned(),
                "s3_bucket" => cfg.s3_bucket = value.to_owned(),
                "s3_access_key" => cfg.s3_access_key = value.to_owned(),
                "sftp_destination" => cfg.sftp_destination = value.to_owned(),
                "sftp_path" => cfg.sftp_path = value.to_owned(),
                "allow_http" => cfg.allow_http = matches!(value, "1" | "true" | "on"),
                _ => {},
            }
        }
        cfg
    }

    /// 按当前协议读第 `index` 个非密文槽位的值（密文槽返回 None——它从
    /// 不驻留配置，占位文案由凭据存在性决定）。
    pub fn slot(&self, index: usize) -> Option<&str> {
        match (self.protocol, index) {
            (BackupProtocol::Folder, 0) => Some(&self.folder_path),
            (BackupProtocol::WebDav, 0) => Some(&self.webdav_url),
            (BackupProtocol::WebDav, 1) => Some(&self.webdav_username),
            (BackupProtocol::S3, 0) => Some(&self.s3_endpoint),
            (BackupProtocol::S3, 1) => Some(&self.s3_region),
            (BackupProtocol::S3, 2) => Some(&self.s3_bucket),
            (BackupProtocol::S3, 3) => Some(&self.s3_access_key),
            (BackupProtocol::Sftp, 0) => Some(&self.sftp_destination),
            (BackupProtocol::Sftp, 1) => Some(&self.sftp_path),
            _ => None,
        }
    }

    /// [`Self::slot`] 的写侧；写不进（密文槽/越界）返回 false。
    pub fn set_slot(&mut self, index: usize, value: String) -> bool {
        let target = match (self.protocol, index) {
            (BackupProtocol::Folder, 0) => &mut self.folder_path,
            (BackupProtocol::WebDav, 0) => &mut self.webdav_url,
            (BackupProtocol::WebDav, 1) => &mut self.webdav_username,
            (BackupProtocol::S3, 0) => &mut self.s3_endpoint,
            (BackupProtocol::S3, 1) => &mut self.s3_region,
            (BackupProtocol::S3, 2) => &mut self.s3_bucket,
            (BackupProtocol::S3, 3) => &mut self.s3_access_key,
            (BackupProtocol::Sftp, 0) => &mut self.sftp_destination,
            (BackupProtocol::Sftp, 1) => &mut self.sftp_path,
            _ => return false,
        };
        *target = value;
        true
    }

    pub fn save(&self) -> Result<(), String> {
        let text = format!(
            "protocol={}\nfolder_path={}\nwebdav_url={}\nwebdav_username={}\ns3_endpoint={}\ns3_region={}\ns3_bucket={}\ns3_access_key={}\nsftp_destination={}\nsftp_path={}\nallow_http={}\n",
            self.protocol.settings_value(),
            self.folder_path.trim(),
            self.webdav_url.trim(),
            self.webdav_username.trim(),
            self.s3_endpoint.trim(),
            self.s3_region.trim(),
            self.s3_bucket.trim(),
            self.s3_access_key.trim(),
            self.sftp_destination.trim(),
            self.sftp_path.trim(),
            self.allow_http as u8,
        );
        std::fs::write(config_path(), text).map_err(|err| format!("写入备份配置失败：{err}"))
    }
}

// ---- 凭据（Windows 凭据管理器；其余平台用环境变量） ----

pub fn has_webdav_password() -> bool {
    webdav_password().is_some()
}

pub fn has_s3_secret() -> bool {
    s3_secret().is_some()
}

#[cfg(windows)]
pub fn store_webdav_password(username: &str, secret: &str) -> Result<(), String> {
    crate::ssh_credentials::windows_store::save_secret(
        WEBDAV_PASSWORD_TARGET,
        username,
        secret.trim().as_bytes(),
    )
    .map_err(|err| format!("保存到凭据管理器失败：{err}"))
}

#[cfg(windows)]
pub fn store_s3_secret(access_key: &str, secret: &str) -> Result<(), String> {
    crate::ssh_credentials::windows_store::save_secret(
        S3_SECRET_TARGET,
        access_key,
        secret.trim().as_bytes(),
    )
    .map_err(|err| format!("保存到凭据管理器失败：{err}"))
}

#[cfg(not(windows))]
pub fn store_webdav_password(_username: &str, _secret: &str) -> Result<(), String> {
    Err("此平台请设环境变量 NEBULA_BACKUP_WEBDAV_PASSWORD".to_owned())
}

#[cfg(not(windows))]
pub fn store_s3_secret(_access_key: &str, _secret: &str) -> Result<(), String> {
    Err("此平台请设环境变量 NEBULA_BACKUP_S3_SECRET".to_owned())
}

fn webdav_password() -> Option<String> {
    if let Ok(password) = std::env::var("NEBULA_BACKUP_WEBDAV_PASSWORD") {
        if !password.trim().is_empty() {
            return Some(password.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if let Ok(Some(secret)) =
        crate::ssh_credentials::windows_store::load_secret(WEBDAV_PASSWORD_TARGET)
    {
        return String::from_utf8(secret).ok();
    }
    None
}

fn s3_secret() -> Option<String> {
    if let Ok(secret) = std::env::var("NEBULA_BACKUP_S3_SECRET") {
        if !secret.trim().is_empty() {
            return Some(secret.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if let Ok(Some(secret)) = crate::ssh_credentials::windows_store::load_secret(S3_SECRET_TARGET) {
        return String::from_utf8(secret).ok();
    }
    None
}

/// 当前协议的密文字段是否已经有可用凭据（设置页占位文案用）。
pub fn protocol_secret_set(protocol: BackupProtocol) -> bool {
    match protocol {
        BackupProtocol::WebDav => has_webdav_password(),
        BackupProtocol::S3 => has_s3_secret(),
        _ => true,
    }
}

// ---- 归档命名 ----

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Unix 秒 → UTC (年, 月, 日, 时, 分, 秒)。Howard Hinnant 的 civil_from_days，
/// 不引日期库——备份名只要可排序、可读即可。
fn utc_parts(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        month as u32,
        day as u32,
        (rem / 3_600) as u32,
        (rem % 3_600 / 60) as u32,
        (rem % 60) as u32,
    )
}

fn archive_name(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    format!(
        "{ARCHIVE_PREFIX}{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}{ARCHIVE_SUFFIX}"
    )
}

/// 只认我们自己的归档名（前后缀精确匹配）。列表、恢复与清理共用这一道
/// 过滤，远端目录里的其他文件对备份管道不可见。
fn is_archive_name(name: &str) -> bool {
    name.starts_with(ARCHIVE_PREFIX)
        && name.ends_with(ARCHIVE_SUFFIX)
        && name.len() > ARCHIVE_PREFIX.len() + ARCHIVE_SUFFIX.len()
}

// ---- 协议后端抽象 ----

/// 一个远端目的地的最小动词集。列表只返回归档名（时间戳命名让字典序 =
/// 时间序，新旧判断不依赖各协议五花八门的 mtime 语义）。
trait Backend {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    fn get(&self, name: &str) -> Result<Vec<u8>, String>;
    fn list(&self) -> Result<Vec<String>, String>;
    fn delete(&self, name: &str) -> Result<(), String>;
    /// 状态行里的目的地描述（不含凭据）。
    fn describe(&self) -> String;
}

/// 校验配置完整性并组装后端。缺什么直接说清楚缺什么，绝不半配置上路。
fn backend(cfg: &BackupRemoteConfig) -> Result<Box<dyn Backend>, String> {
    match cfg.protocol {
        BackupProtocol::Off => Err("未启用远程备份：先在设置 → 备份里选择协议".to_owned()),
        BackupProtocol::Folder => {
            let path = cfg.folder_path.trim();
            if path.is_empty() {
                return Err("未配置备份目录路径".to_owned());
            }
            Ok(Box::new(FolderBackend { root: PathBuf::from(path) }))
        },
        BackupProtocol::WebDav => {
            let url = cfg.webdav_url.trim().trim_end_matches('/').to_owned();
            if url.is_empty() {
                return Err("未配置 WebDAV 目录 URL".to_owned());
            }
            if !url.starts_with("https://") && !cfg.allow_http {
                return Err(
                    "已拒绝：WebDAV URL 不是 HTTPS（自建内网服务可在 nebula_backup.txt 写 allow_http=1 豁免）"
                        .to_owned(),
                );
            }
            let username = cfg.webdav_username.trim().to_owned();
            if username.is_empty() {
                return Err("未配置 WebDAV 用户名".to_owned());
            }
            let password = webdav_password()
                .ok_or("缺少 WebDAV 密码：在设置里输入或设 NEBULA_BACKUP_WEBDAV_PASSWORD")?;
            Ok(Box::new(WebDavBackend { url, username, password }))
        },
        BackupProtocol::S3 => {
            let endpoint = cfg.s3_endpoint.trim().trim_end_matches('/').to_owned();
            if endpoint.is_empty() {
                return Err("未配置 S3 Endpoint".to_owned());
            }
            if !endpoint.starts_with("https://") && !cfg.allow_http {
                return Err(
                    "已拒绝：S3 Endpoint 不是 HTTPS（自建内网服务可在 nebula_backup.txt 写 allow_http=1 豁免）"
                        .to_owned(),
                );
            }
            let region = cfg.s3_region.trim().to_owned();
            if region.is_empty() {
                return Err("未配置 S3 区域（MinIO/R2 可写 us-east-1 / auto）".to_owned());
            }
            let raw = cfg.s3_bucket.trim().trim_matches('/');
            let (bucket, prefix) = match raw.split_once('/') {
                Some((bucket, prefix)) => (bucket.to_owned(), prefix.trim_matches('/').to_owned()),
                None => (raw.to_owned(), String::new()),
            };
            if bucket.is_empty() {
                return Err("未配置 S3 存储桶".to_owned());
            }
            let access_key = cfg.s3_access_key.trim().to_owned();
            if access_key.is_empty() {
                return Err("未配置 S3 Access Key".to_owned());
            }
            let secret_key = s3_secret()
                .ok_or("缺少 S3 Secret Key：在设置里输入或设 NEBULA_BACKUP_S3_SECRET")?;
            Ok(Box::new(S3Backend { endpoint, region, bucket, prefix, access_key, secret_key }))
        },
        BackupProtocol::Sftp => {
            let destination = cfg.sftp_destination.trim().to_owned();
            if destination.is_empty() {
                return Err(
                    "未配置 SFTP 目标（user@host[:port]，认证复用 SSH 主机配置）".to_owned()
                );
            }
            let mut path = cfg.sftp_path.trim().trim_end_matches('/').to_owned();
            if path.is_empty() {
                path = ".".to_owned();
            }
            Ok(Box::new(SftpBackend { destination, path }))
        },
    }
}

/// 点击「备份到远程 / 从远程恢复」时的先行校验：配置齐全性与凭据存在性，
/// 不碰网络。错误文案与真正执行时一致，用户在输口令前就能看到缺什么。
pub fn validate() -> Result<(), String> {
    backend(&BackupRemoteConfig::load()).map(|_| ())
}

// ---- 入口：推送 / 取回 ----

/// 上传一份新归档并尽力清理超出保留数的旧份。阻塞，跑在后台线程。
pub fn push(packet: &[u8]) -> Result<String, String> {
    let cfg = BackupRemoteConfig::load();
    let backend = backend(&cfg)?;
    let name = archive_name(now_unix());
    backend.put(&name, packet)?;
    let mut message =
        format!("已备份到{}（{name}，{} KB）", backend.describe(), packet.len().div_ceil(1024));
    // 清理是尽力而为：上传已经成功，旧份删不掉只降级为日志加一句提示。
    match prune(backend.as_ref()) {
        Ok(kept) if kept > 1 => message.push_str(&format!("，远端保留 {kept} 份")),
        Ok(_) => {},
        Err(err) => {
            warn!("backup prune: {err}");
            message.push_str("；旧份清理失败（详见日志）");
        },
    }
    Ok(message)
}

/// 取回远端最新一份归档：`(归档名, 密文字节)`。阻塞，跑在后台线程。
pub fn pull_latest() -> Result<(String, Vec<u8>), String> {
    let cfg = BackupRemoteConfig::load();
    let backend = backend(&cfg)?;
    let mut names: Vec<String> =
        backend.list()?.into_iter().filter(|name| is_archive_name(name)).collect();
    names.sort();
    let Some(latest) = names.pop() else {
        return Err(format!("{}上没有 Nebula 备份归档", backend.describe()));
    };
    let bytes = backend.get(&latest)?;
    Ok((latest, bytes))
}

/// 删除超出 [`KEEP_ARCHIVES`] 的最旧归档，返回清理后的份数。
fn prune(backend: &dyn Backend) -> Result<usize, String> {
    let mut names: Vec<String> =
        backend.list()?.into_iter().filter(|name| is_archive_name(name)).collect();
    names.sort();
    let excess = names.len().saturating_sub(KEEP_ARCHIVES);
    for name in &names[..excess] {
        backend.delete(name)?;
    }
    Ok(names.len() - excess)
}

// ---- 后端：本地/网络目录 ----

struct FolderBackend {
    root: PathBuf,
}

impl Backend for FolderBackend {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|err| format!("创建备份目录失败：{err}"))?;
        crate::atomic_file::write(&self.root.join(name), bytes)
            .map_err(|err| format!("写入备份失败：{err}"))
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.root.join(name)).map_err(|err| format!("读取备份失败：{err}"))
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(format!("读取备份目录失败：{err}")),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("读取备份目录失败：{err}"))?;
            if entry.file_type().map(|kind| kind.is_file()).unwrap_or(false) {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(names)
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        std::fs::remove_file(self.root.join(name)).map_err(|err| format!("删除旧备份失败：{err}"))
    }

    fn describe(&self) -> String {
        format!("目录 {}", self.root.display())
    }
}

// ---- 后端：WebDAV ----

struct WebDavBackend {
    /// 目录 URL，已去尾部斜杠。
    url: String,
    username: String,
    password: String,
}

fn http_agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

impl WebDavBackend {
    fn auth(&self) -> String {
        use base64::Engine as _;
        let raw = format!("{}:{}", self.username, self.password);
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw))
    }

    fn file_url(&self, name: &str) -> String {
        format!("{}/{name}", self.url)
    }

    fn put_once(&self, name: &str, bytes: &[u8]) -> Result<u16, String> {
        let response = http_agent()
            .put(&self.file_url(name))
            .header("Authorization", &self.auth())
            .send(bytes)
            .map_err(|err| format!("上传失败：{err}"))?;
        Ok(response.status().as_u16())
    }

    /// 目录不存在时的一次性补救。MKCOL 只建最后一级——多级路径请先在
    /// 服务端建好。
    fn mkcol(&self) -> Result<(), String> {
        let request = ureq::http::Request::builder()
            .method("MKCOL")
            .uri(&self.url)
            .header("Authorization", self.auth())
            .body(Vec::new())
            .map_err(|err| format!("构造 MKCOL 请求失败：{err}"))?;
        let response =
            http_agent().run(request).map_err(|err| format!("创建远端目录失败：{err}"))?;
        match response.status().as_u16() {
            // 405 = 目录已存在（Method Not Allowed on existing collection）。
            200 | 201 | 405 => Ok(()),
            401 | 403 => Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
            status => Err(format!("创建远端目录失败：HTTP {status}")),
        }
    }
}

impl Backend for WebDavBackend {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let status = self.put_once(name, bytes)?;
        let status = match status {
            // 目标目录不存在时多数服务器回 404/409；建目录后重试一次。
            404 | 409 => {
                self.mkcol()?;
                self.put_once(name, bytes)?
            },
            other => other,
        };
        match status {
            200 | 201 | 204 => Ok(()),
            401 | 403 => Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
            other => Err(format!("上传失败：HTTP {other}")),
        }
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        let mut response = http_agent()
            .get(&self.file_url(name))
            .header("Authorization", &self.auth())
            .call()
            .map_err(|err| format!("下载失败：{err}"))?;
        match response.status().as_u16() {
            200 => response.body_mut().read_to_vec().map_err(|err| format!("读取远端失败：{err}")),
            401 | 403 => Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
            status => Err(format!("下载失败：HTTP {status}")),
        }
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let request = ureq::http::Request::builder()
            .method("PROPFIND")
            .uri(format!("{}/", self.url))
            .header("Authorization", self.auth())
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0"?><propfind xmlns="DAV:"><prop><displayname/></prop></propfind>"#.to_owned())
            .map_err(|err| format!("构造列表请求失败：{err}"))?;
        let mut response =
            http_agent().run(request).map_err(|err| format!("列出远端失败：{err}"))?;
        match response.status().as_u16() {
            207 | 200 => {},
            // 目录还不存在 = 还没有任何备份。
            404 => return Ok(Vec::new()),
            401 | 403 => return Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
            status => return Err(format!("列出远端失败：HTTP {status}")),
        }
        let body =
            response.body_mut().read_to_vec().map_err(|err| format!("读取列表失败：{err}"))?;
        Ok(propfind_archive_names(&String::from_utf8_lossy(&body)))
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let request = ureq::http::Request::builder()
            .method("DELETE")
            .uri(&self.file_url(name))
            .header("Authorization", self.auth())
            .body(Vec::new())
            .map_err(|err| format!("构造删除请求失败：{err}"))?;
        let response = http_agent().run(request).map_err(|err| format!("删除旧备份失败：{err}"))?;
        match response.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            status => Err(format!("删除旧备份失败：HTTP {status}")),
        }
    }

    fn describe(&self) -> String {
        format!("WebDAV {}", self.url)
    }
}

/// 从 PROPFIND 的多状态 XML 里挑出我们的归档名。不引 XML 解析器：只认
/// `…href>…</…` 文本段里以归档前后缀命名的最后一段路径。归档名是纯 ASCII
/// `[0-9a-z.-]`，URL 编码对它是恒等变换，直接比对安全。
fn propfind_archive_names(xml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("href>") {
        rest = &rest[open + "href>".len()..];
        let Some(close) = rest.find('<') else { break };
        let href = rest[..close].trim().trim_end_matches('/');
        let name = href.rsplit('/').next().unwrap_or_default();
        if is_archive_name(name) && !names.iter().any(|seen| seen == name) {
            names.push(name.to_owned());
        }
        rest = &rest[close..];
    }
    names
}

// ---- 后端：S3 兼容（SigV4） ----

struct S3Backend {
    /// `https://host[:port]`，已去尾部斜杠。
    endpoint: String,
    region: String,
    bucket: String,
    /// 可为空；非空时不带首尾斜杠。
    prefix: String,
    access_key: String,
    secret_key: String,
}

impl S3Backend {
    fn host(&self) -> String {
        let after_scheme =
            self.endpoint.split_once("://").map(|(_, rest)| rest).unwrap_or(&self.endpoint);
        let host = after_scheme.split('/').next().unwrap_or(after_scheme);
        // 默认端口按惯例省略；显式写默认端口会让签名的 host 与 ureq 实际
        // 发送的 Host 头不一致，SignatureDoesNotMatch 且极难排查。
        let default_port = if self.endpoint.starts_with("http://") { ":80" } else { ":443" };
        host.strip_suffix(default_port).unwrap_or(host).to_owned()
    }

    fn key(&self, name: &str) -> String {
        if self.prefix.is_empty() { name.to_owned() } else { format!("{}/{name}", self.prefix) }
    }

    /// 路径式 object 路径：`/bucket/key`（对 MinIO/R2/自建最不挑剔）。
    fn object_path(&self, name: &str) -> String {
        format!("/{}/{}", self.bucket, self.key(name))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), String> {
        let amz_date = sigv4_timestamp(now_unix());
        let payload_hash = sha256_hex(body);
        let authorization = sigv4_authorization(
            method,
            &self.host(),
            path,
            query,
            &payload_hash,
            &amz_date,
            &self.region,
            &self.access_key,
            &self.secret_key,
        );
        let canonical_query = sigv4_canonical_query(query);
        let url = if canonical_query.is_empty() {
            format!("{}{}", self.endpoint, sigv4_encode_path(path))
        } else {
            format!("{}{}?{canonical_query}", self.endpoint, sigv4_encode_path(path))
        };
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("Authorization", authorization)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .body(body.to_vec())
            .map_err(|err| format!("构造 S3 请求失败：{err}"))?;
        let mut response = http_agent().run(request).map_err(|err| format!("连接失败：{err}"))?;
        let status = response.status().as_u16();
        let bytes =
            response.body_mut().read_to_vec().map_err(|err| format!("读取响应失败：{err}"))?;
        Ok((status, bytes))
    }

    fn explain(status: u16, body: &[u8]) -> String {
        // S3 的错误正文是小段 XML；只挑 <Code> 给人看，避免整包倾倒。
        let text = String::from_utf8_lossy(body);
        let code = text
            .split_once("<Code>")
            .and_then(|(_, rest)| rest.split_once("</Code>"))
            .map(|(code, _)| code.trim().to_owned());
        match code {
            Some(code) if !code.is_empty() => format!("HTTP {status}（{code}）"),
            _ => format!("HTTP {status}"),
        }
    }
}

impl Backend for S3Backend {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let (status, body) = self.request("PUT", &self.object_path(name), &[], bytes)?;
        match status {
            200 => Ok(()),
            403 => Err("认证失败：检查 S3 Access Key / Secret Key / 区域".to_owned()),
            _ => Err(format!("上传失败：{}", Self::explain(status, &body))),
        }
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        let (status, body) = self.request("GET", &self.object_path(name), &[], &[])?;
        match status {
            200 => Ok(body),
            403 => Err("认证失败：检查 S3 Access Key / Secret Key / 区域".to_owned()),
            _ => Err(format!("下载失败：{}", Self::explain(status, &body))),
        }
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let prefix = self.key(ARCHIVE_PREFIX);
        let path = format!("/{}/", self.bucket);
        let (status, body) =
            self.request("GET", &path, &[("list-type", "2"), ("prefix", &prefix)], &[])?;
        match status {
            200 => {},
            403 => return Err("认证失败：检查 S3 Access Key / Secret Key / 区域".to_owned()),
            _ => return Err(format!("列出远端失败：{}", Self::explain(status, &body))),
        }
        let text = String::from_utf8_lossy(&body);
        let mut names = Vec::new();
        let mut rest: &str = &text;
        while let Some(open) = rest.find("<Key>") {
            rest = &rest[open + "<Key>".len()..];
            let Some(close) = rest.find("</Key>") else { break };
            let key = &rest[..close];
            let name = key.rsplit('/').next().unwrap_or_default();
            if is_archive_name(name) {
                names.push(name.to_owned());
            }
            rest = &rest[close..];
        }
        Ok(names)
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let (status, body) = self.request("DELETE", &self.object_path(name), &[], &[])?;
        match status {
            200 | 204 => Ok(()),
            _ => Err(format!("删除旧备份失败：{}", Self::explain(status, &body))),
        }
    }

    fn describe(&self) -> String {
        format!("S3 {}/{}", self.host(), self.bucket)
    }
}

// ---- SigV4 签名（S3 专用最小实现） ----

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut out, byte| {
        out.push_str(&format!("{byte:02x}"));
        out
    })
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, KeyInit as _, Mac as _};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sigv4_timestamp(secs: u64) -> String {
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// AWS 的 uri-encode：未保留字符原样，其余 %XX 大写；`encode_slash=false`
/// 时保留路径分隔符（object key 用）。
fn sigv4_uri_encode(text: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            },
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn sigv4_encode_path(path: &str) -> String {
    sigv4_uri_encode(path, false)
}

fn sigv4_canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| (sigv4_uri_encode(key, true), sigv4_uri_encode(value, true)))
        .collect();
    pairs.sort();
    pairs.into_iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("&")
}

/// 组装 Authorization 头。签名头固定为 host + x-amz-content-sha256 +
/// x-amz-date 三件套——正好是我们每个请求发出的全部自定义头。
#[allow(clippy::too_many_arguments)]
fn sigv4_authorization(
    method: &str,
    host: &str,
    path: &str,
    query: &[(&str, &str)],
    payload_hash: &str,
    amz_date: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
) -> String {
    let date = &amz_date[..8];
    let canonical_request = format!(
        "{method}\n{}\n{}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}",
        sigv4_encode_path(path),
        sigv4_canonical_query(query),
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), date.as_bytes());
    let key = hmac_sha256(&key, region.as_bytes());
    let key = hmac_sha256(&key, b"s3");
    let key = hmac_sha256(&key, b"aws4_request");
    let signature = hmac_sha256(&key, string_to_sign.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut out, byte| {
            out.push_str(&format!("{byte:02x}"));
            out
        },
    );
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}"
    )
}

// ---- 后端：SFTP（复用 SSH 会话栈） ----

struct SftpBackend {
    destination: String,
    path: String,
}

impl SftpBackend {
    fn remote_file(&self, name: &str) -> String {
        format!("{}/{name}", self.path)
    }

    /// 在共享 SSH runtime 上阻塞执行一段 SFTP 作业。调用方已在后台 OS
    /// 线程，block_on 不会碰 UI 线程。
    fn run<T, F, Fut>(&self, job: F) -> Result<T, String>
    where
        F: FnOnce(russh_sftp::client::SftpSession) -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let runtime =
            crate::ssh_session::runtime().map_err(|err| format!("SSH 运行时不可用：{err}"))?;
        let destination = self.destination.clone();
        runtime.block_on(async move {
            let sftp = crate::ssh_session::open_sftp(&destination)
                .await
                .map_err(|err| format!("SFTP 连接失败：{err}"))?;
            job(sftp).await
        })
    }
}

impl Backend for SftpBackend {
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncWriteExt as _;
        let path = self.path.clone();
        let remote = self.remote_file(name);
        self.run(|sftp| async move {
            // 目录已存在时 create_dir 失败是常态，忽略；真正的问题会在
            // 打开文件时以更准确的错误暴露。
            let _ = sftp.create_dir(path).await;
            let mut file = sftp
                .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)
                .await
                .map_err(|err| format!("打开远端文件失败：{err}"))?;
            file.write_all(&bytes).await.map_err(|err| format!("写入失败：{err}"))?;
            file.shutdown().await.map_err(|err| format!("写入收尾失败：{err}"))?;
            Ok(())
        })
    }

    fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        use tokio::io::AsyncReadExt as _;
        let remote = self.remote_file(name);
        self.run(|sftp| async move {
            let mut file =
                sftp.open(remote).await.map_err(|err| format!("打开远端文件失败：{err}"))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await.map_err(|err| format!("读取失败：{err}"))?;
            Ok(bytes)
        })
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let path = self.path.clone();
        self.run(|sftp| async move {
            let entries = match sftp.read_dir(path).await {
                Ok(entries) => entries,
                // 目录还不存在 = 还没有任何备份。
                Err(_) => return Ok(Vec::new()),
            };
            Ok(entries.into_iter().map(|entry| entry.file_name()).collect())
        })
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let remote = self.remote_file(name);
        self.run(|sftp| async move {
            sftp.remove_file(remote).await.map_err(|err| format!("删除旧备份失败：{err}"))
        })
    }

    fn describe(&self) -> String {
        format!("SFTP {}:{}", self.destination, self.path)
    }
}

pub fn warn_result<T>(result: &Result<T, String>) {
    if let Err(err) = result {
        warn!("backup remote: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names_are_utc_stamped_and_sort_chronologically() {
        // 2026-08-13 05:19:00 UTC。
        assert_eq!(archive_name(1_786_598_340), "nebula-backup-20260813-051900.nbk");
        // Unix 纪元与闰日。
        assert_eq!(archive_name(0), "nebula-backup-19700101-000000.nbk");
        assert_eq!(archive_name(1_582_934_400), "nebula-backup-20200229-000000.nbk");
        let older = archive_name(1_700_000_000);
        let newer = archive_name(1_800_000_000);
        assert!(newer > older, "时间戳命名必须让字典序等于时间序");
    }

    #[test]
    fn archive_filter_rejects_foreign_files() {
        assert!(is_archive_name("nebula-backup-20260813-051900.nbk"));
        assert!(!is_archive_name("nebula-backup-.nbk"));
        assert!(!is_archive_name("photo.png"));
        assert!(!is_archive_name("nebula-backup-20260813-051900.nbk.tmp"));
        assert!(!is_archive_name("other-backup-20260813.nbk"));
    }

    #[test]
    fn config_round_trips_every_protocol_field() {
        let cfg = BackupRemoteConfig {
            protocol: BackupProtocol::S3,
            folder_path: r"\\nas\share\nebula".into(),
            webdav_url: "https://dav.example.com/nebula".into(),
            webdav_username: "user".into(),
            s3_endpoint: "https://s3.us-east-1.amazonaws.com".into(),
            s3_region: "us-east-1".into(),
            s3_bucket: "bucket/prefix".into(),
            s3_access_key: "AKIDEXAMPLE".into(),
            sftp_destination: "dev@10.0.0.8:2222".into(),
            sftp_path: "/home/dev/backups".into(),
            allow_http: true,
        };
        let text = format!(
            "protocol={}\nfolder_path={}\nwebdav_url={}\nwebdav_username={}\ns3_endpoint={}\ns3_region={}\ns3_bucket={}\ns3_access_key={}\nsftp_destination={}\nsftp_path={}\nallow_http=1\n",
            cfg.protocol.settings_value(),
            cfg.folder_path,
            cfg.webdav_url,
            cfg.webdav_username,
            cfg.s3_endpoint,
            cfg.s3_region,
            cfg.s3_bucket,
            cfg.s3_access_key,
            cfg.sftp_destination,
            cfg.sftp_path,
        );
        assert_eq!(BackupRemoteConfig::parse(&text), cfg);
        // 未知键与坏行安静跳过。
        assert_eq!(
            BackupRemoteConfig::parse("garbage\nunknown=1\n"),
            BackupRemoteConfig::default()
        );
    }

    #[test]
    fn sigv4_matches_aws_documented_lifecycle_example() {
        // AWS 官方示例（s3 REST API 签名文档 GET Bucket Lifecycle）：
        // 空载荷、仅 host/x-amz-content-sha256/x-amz-date 三个签名头。
        let authorization = sigv4_authorization(
            "GET",
            "examplebucket.s3.amazonaws.com",
            "/",
            &[("lifecycle", "")],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "20130524T000000Z",
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }

    #[test]
    fn sigv4_encoding_follows_aws_rules() {
        assert_eq!(sigv4_uri_encode("a b/c~d", false), "a%20b/c~d");
        assert_eq!(sigv4_uri_encode("a b/c~d", true), "a%20b%2Fc~d");
        // 查询串键排序 + 空值保留等号。
        assert_eq!(
            sigv4_canonical_query(&[("prefix", "nebula-backup-"), ("list-type", "2")]),
            "list-type=2&prefix=nebula-backup-"
        );
        assert_eq!(sigv4_canonical_query(&[("lifecycle", "")]), "lifecycle=");
        assert_eq!(sigv4_timestamp(1_369_353_600), "20130524T000000Z");
    }

    #[test]
    fn propfind_scan_extracts_only_our_archives() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response><d:href>/dav/nebula/</d:href></d:response>
  <d:response><d:href>/dav/nebula/nebula-backup-20260813-051900.nbk</d:href></d:response>
  <d:response><d:href>/dav/nebula/nebula-backup-20260812-231000.nbk</d:href></d:response>
  <d:response><d:href>/dav/nebula/notes.txt</d:href></d:response>
  <D:response xmlns:D="DAV:"><D:href>/dav/nebula/nebula-backup-20260812-231000.nbk</D:href></D:response>
</d:multistatus>"#;
        assert_eq!(
            propfind_archive_names(xml),
            vec![
                "nebula-backup-20260813-051900.nbk".to_owned(),
                "nebula-backup-20260812-231000.nbk".to_owned(),
            ]
        );
    }

    #[test]
    fn s3_host_strips_default_port_and_splits_bucket_prefix() {
        let backend = S3Backend {
            endpoint: "https://minio.lan:9000".into(),
            region: "us-east-1".into(),
            bucket: "nebula".into(),
            prefix: "backups".into(),
            access_key: "ak".into(),
            secret_key: "sk".into(),
        };
        assert_eq!(backend.host(), "minio.lan:9000");
        assert_eq!(backend.object_path("a.nbk"), "/nebula/backups/a.nbk");
        let default_port = S3Backend { endpoint: "https://s3.example.com:443".into(), ..backend };
        assert_eq!(default_port.host(), "s3.example.com");
    }

    /// 内存后端专测 prune 的排序与保留语义。
    struct MemoryBackend(std::cell::RefCell<Vec<String>>);
    impl Backend for MemoryBackend {
        fn put(&self, name: &str, _bytes: &[u8]) -> Result<(), String> {
            self.0.borrow_mut().push(name.to_owned());
            Ok(())
        }
        fn get(&self, _name: &str) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
        fn list(&self) -> Result<Vec<String>, String> {
            Ok(self.0.borrow().clone())
        }
        fn delete(&self, name: &str) -> Result<(), String> {
            self.0.borrow_mut().retain(|kept| kept != name);
            Ok(())
        }
        fn describe(&self) -> String {
            "memory".to_owned()
        }
    }

    #[test]
    fn prune_keeps_newest_archives_and_ignores_foreign_files() {
        let names: Vec<String> = (0..13)
            .map(|hour| format!("nebula-backup-202608{:02}-000000.nbk", hour + 1))
            .chain(["keep-me.txt".to_owned()])
            .collect();
        let backend = MemoryBackend(std::cell::RefCell::new(names));
        assert_eq!(prune(&backend).unwrap(), KEEP_ARCHIVES);
        let survivors = backend.0.borrow();
        // 13 份归档删到 10 份（删最旧的 01/02/03 号），外来文件不动。
        assert_eq!(survivors.len(), KEEP_ARCHIVES + 1);
        assert!(survivors.contains(&"keep-me.txt".to_owned()));
        assert!(!survivors.iter().any(|name| name.contains("20260801")));
        assert!(!survivors.iter().any(|name| name.contains("20260803")));
        assert!(survivors.iter().any(|name| name.contains("20260804")));
        assert!(survivors.iter().any(|name| name.contains("20260813")));
    }
}
