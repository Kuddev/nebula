//! WebDAV 配置同步（docs/specs/003-webdav-sync.md）。
//!
//! 单文件 GET/PUT + ETag 乐观锁；内容先经端到端加密（Argon2id 派生 +
//! AES-256-GCM）再出境，服务器只见密文。同步集是白名单制：外观/交互
//! 标量 + `keybind=` 行 + SSH 地址簿 + 命令历史尾部；任何凭据永不入集
//! （SSH 密码在凭据管理器里，从不进设置文件）。网络与 KDF 都在 event
//! 层的后台 OS 线程阻塞完成（与 ai_assistant 同款模型）——不配置同步
//! 的用户一次也不会执行这里的任何加密代码，终端热路径零涉及。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use log::warn;

/// 同步配置文件（位于 `nebula_data_dir()`）。独立于被同步的
/// `nebula_settings.txt`——同步管道自身的配置不应被同步覆盖。
const CONFIG_FILE: &str = "nebula_sync.txt";

/// 封包魔数 + 版本。改格式必须换魔数，旧客户端读到新包要能干脆报错。
const MAGIC: &[u8; 8] = b"NEBSYNC1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// 同步的历史行数上限：约 200 KB 明文，符合「不大的东西才同步」。
const HISTORY_SYNC_CAP: usize = 2000;

/// 入集的 `nebula_settings.txt` 键（spec 003「同步集」）。SSH 三张表
/// 应用户要求入集（2026-07-28）——那是地址簿，不是凭据。用户裁定的
/// 排除项：自定义字体（本地文件，目标机器多半没有）、shell 与
/// startup_directory（机器相关）、background_image（本地路径）。
const SYNC_KEYS: &[&str] = &[
    "language",
    "theme",
    "follow_system_theme",
    "ghost",
    "accept",
    "font_size",
    "cursor_shape",
    "cursor_blink",
    "copy_on_select",
    "cjk_bold_regular",
    "fetch",
    "powerline",
    "keep_session",
    "restore_session",
    "resume_ai",
    "tray",
    "panel_resize",
    "sidebar_w",
    "drawer_w",
    "hosts_band",
    "opacity",
    "background",
    "background_image_opacity",
    "background_image_fit",
    "background_image_alignment",
    "background_image_cover_chrome",
    "pinned_hosts",
    "saved_hosts",
    "hidden_hosts",
];

/// 逗号列表语义的键：合并时求并集而不是整包覆盖——两台机器各自保存
/// 的 SSH 主机都要活下来（与键位的 combo 并集同一条设计裁定）。
const LIST_KEYS: &[&str] = &["pinned_hosts", "saved_hosts", "hidden_hosts"];

/// 客户端弱口令黑名单（硬拒绝，用户裁定 2026-07-28）。E2E 的强度短板
/// 在口令；123456 这类口令让加密形同虚设，宁可拒绝同步也不给虚假安全感。
const WEAK_PASSPHRASES: &[&str] = &[
    "123456",
    "1234567",
    "12345678",
    "123456789",
    "1234567890",
    "password",
    "password1",
    "qwerty",
    "qwertyuiop",
    "abc123",
    "111111",
    "000000",
    "654321",
    "666666",
    "888888",
    "letmein",
    "iloveyou",
    "admin",
    "admin123",
    "root",
    "passw0rd",
    "p@ssw0rd",
    "dragon",
    "monkey",
    "sunshine",
    "princess",
    "welcome",
    "shadow",
    "master",
    "qazwsx",
    "asdfgh",
    "zxcvbn",
    "asdfghjkl",
    "1q2w3e4r",
    "1qaz2wsx",
    "qwe123",
    "a123456",
    "123123",
    "121212",
    "112233",
    "159357",
    "147258",
    "789456",
    "woaini",
    "5201314",
    "nebula",
];

/// Windows 凭据管理器里的条目名（`ssh_credentials` 的通用 DPAPI 存取）。
#[cfg(windows)]
const PASSWORD_TARGET: &str = "Nebula Sync WebDAV Password";
#[cfg(windows)]
const PASSPHRASE_TARGET: &str = "Nebula Sync Passphrase";

pub fn config_path() -> PathBuf {
    crate::display::nebula_data_dir().join(CONFIG_FILE)
}

/// `nebula_sync.txt` 的解析结果；文件缺失 = 未配置 = 两个动作都提示引导。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncConfig {
    pub url: String,
    pub username: String,
    pub allow_http: bool,
    pub auto_pull: bool,
}

impl SyncConfig {
    pub fn load() -> Self {
        Self::parse(&std::fs::read_to_string(config_path()).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "url" => cfg.url = value.to_owned(),
                "username" => cfg.username = value.to_owned(),
                "allow_http" => cfg.allow_http = matches!(value, "1" | "true" | "on"),
                "auto_pull" => cfg.auto_pull = matches!(value, "1" | "true" | "on"),
                _ => {},
            }
        }
        cfg
    }

    pub fn configured(&self) -> bool {
        !self.url.is_empty()
    }

    /// 写回 `nebula_sync.txt`（设置页保存路径）。逐键重写但保留
    /// `allow_http`——那是手写豁免开关，UI 故意不给入口。
    pub fn save(&self) -> Result<(), String> {
        let text = format!(
            "url={}\nusername={}\nallow_http={}\nauto_pull={}\n",
            self.url.trim(),
            self.username.trim(),
            self.allow_http as u8,
            self.auto_pull as u8,
        );
        std::fs::write(config_path(), text).map_err(|err| format!("写入同步配置失败：{err}"))
    }
}

// ---- 设置页的凭据存取（Windows 凭据管理器；其余平台用环境变量） ----

/// WebDAV 密码是否已可用（凭据管理器或环境变量任一即可）。
pub fn has_password() -> bool {
    webdav_password().is_some()
}

pub fn has_passphrase() -> bool {
    sync_passphrase().is_some()
}

#[cfg(windows)]
pub fn store_password(username: &str, secret: &str) -> Result<(), String> {
    crate::ssh_credentials::windows_store::save_secret(
        PASSWORD_TARGET,
        username,
        secret.trim().as_bytes(),
    )
    .map_err(|err| format!("保存到凭据管理器失败：{err}"))
}

/// E2E 口令入库前先过弱口令闸——拒绝的口令连凭据管理器都不该进，
/// 否则「保存成功」和「同步被拒」会互相矛盾。
#[cfg(windows)]
pub fn store_passphrase(username: &str, secret: &str) -> Result<(), String> {
    let webdav = webdav_password().unwrap_or_default();
    if let Some(reason) = passphrase_weakness(secret.trim(), username, &webdav) {
        return Err(reason.to_owned());
    }
    crate::ssh_credentials::windows_store::save_secret(
        PASSPHRASE_TARGET,
        username,
        secret.trim().as_bytes(),
    )
    .map_err(|err| format!("保存到凭据管理器失败：{err}"))
}

#[cfg(not(windows))]
pub fn store_password(_username: &str, _secret: &str) -> Result<(), String> {
    Err("此平台请设环境变量 NEBULA_WEBDAV_PASSWORD".to_owned())
}

#[cfg(not(windows))]
pub fn store_passphrase(_username: &str, _secret: &str) -> Result<(), String> {
    Err("此平台请设环境变量 NEBULA_SYNC_PASSPHRASE".to_owned())
}

/// WebDAV 密码：环境变量 → Windows 凭据管理器。明文永不落盘。
fn webdav_password() -> Option<String> {
    if let Ok(password) = std::env::var("NEBULA_WEBDAV_PASSWORD") {
        if !password.trim().is_empty() {
            return Some(password.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if let Ok(Some(secret)) = crate::ssh_credentials::windows_store::load_secret(PASSWORD_TARGET) {
        return String::from_utf8(secret).ok();
    }
    None
}

/// E2E 口令：环境变量 → Windows 凭据管理器。没有口令绝不上传。
fn sync_passphrase() -> Option<String> {
    if let Ok(passphrase) = std::env::var("NEBULA_SYNC_PASSPHRASE") {
        if !passphrase.trim().is_empty() {
            return Some(passphrase.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if let Ok(Some(secret)) = crate::ssh_credentials::windows_store::load_secret(PASSPHRASE_TARGET)
    {
        return String::from_utf8(secret).ok();
    }
    None
}

/// 弱口令闸（硬拒绝）。返回 Some(原因) = 拒绝同步。
/// 与 WebDAV 密码相同也拒绝：服务商能验证那个密码，等于能派生密钥，
/// 端到端就破功了。
fn passphrase_weakness(
    passphrase: &str,
    username: &str,
    webdav_password: &str,
) -> Option<&'static str> {
    if passphrase.chars().count() < 8 {
        return Some("同步口令太短（至少 8 个字符）");
    }
    let lower = passphrase.to_lowercase();
    if WEAK_PASSPHRASES.contains(&lower.as_str()) {
        return Some("同步口令在常见弱口令表里，请换一个");
    }
    if !username.is_empty() && lower == username.to_lowercase() {
        return Some("同步口令不能与 WebDAV 用户名相同");
    }
    if passphrase == webdav_password {
        return Some("同步口令不能与 WebDAV 密码相同（服务商可解密，端到端失效）");
    }
    None
}

// ---- 封包与加密 ----

/// Argon2id 密钥派生，锁定最低推荐档 m=19 MiB、t=2、p=1（用户裁定
/// 2026-07-28：SSH 地址簿与命令历史属高危隐私，必须认真加密；性能上
/// 只在同步动作的后台线程跑 ~0.1s/19 MiB 瞬时，可以上，但克制——
/// 参数取下限、不做档位配置、不做算法协商，加密全部关在本模块）。
fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(19_456, 2, 1, Some(32))
        .map_err(|err| format!("argon2 params: {err}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|err| format!("argon2: {err}"))?;
    Ok(key)
}

/// 明文 → `NEBSYNC1 | salt | nonce | ciphertext`。每次封包新 salt/nonce。
fn seal(plaintext: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|err| format!("rng: {err}"))?;
    getrandom::fill(&mut nonce).map_err(|err| format!("rng: {err}"))?;
    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new((&key).into());
    let ciphertext = cipher
        .encrypt(&Nonce::try_from(&nonce[..]).expect("nonce 长度固定"), plaintext)
        .map_err(|_| "加密失败".to_owned())?;
    let mut packet = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
    packet.extend_from_slice(MAGIC);
    packet.extend_from_slice(&salt);
    packet.extend_from_slice(&nonce);
    packet.extend_from_slice(&ciphertext);
    Ok(packet)
}

/// 封包 → 明文。口令错误与包损坏都表现为 GCM 认证失败，同一句人话。
fn open_packet(packet: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    if packet.len() < MAGIC.len() + SALT_LEN + NONCE_LEN || &packet[..MAGIC.len()] != MAGIC {
        return Err("远端文件不是 Nebula 同步包（或版本不兼容）".to_owned());
    }
    let salt = &packet[MAGIC.len()..MAGIC.len() + SALT_LEN];
    let nonce = &packet[MAGIC.len() + SALT_LEN..MAGIC.len() + SALT_LEN + NONCE_LEN];
    let ciphertext = &packet[MAGIC.len() + SALT_LEN + NONCE_LEN..];
    let key = derive_key(passphrase, salt)?;
    let cipher = Aes256Gcm::new((&key).into());
    cipher
        .decrypt(&Nonce::try_from(nonce).expect("nonce 长度已由封包头校验"), ciphertext)
        .map_err(|_| "解密失败：同步口令不一致或文件损坏".to_owned())
}

// ---- payload ----

/// 一台设备的同步内容快照。合并语义：标量整包新者胜；键位按 combo、
/// 主机表按逗号项、历史按整行——求并集，冲突取新方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPayload {
    pub modified: u64,
    pub device: String,
    pub settings: Vec<(String, String)>,
    pub keybinds: Vec<(String, String)>,
    pub history: Vec<String>,
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 从 `nebula_settings.txt` 原文筛出同步集（历史另行读取）。
pub fn payload_from_settings_text(text: &str, history: Vec<String>) -> SyncPayload {
    let mut settings = Vec::new();
    let mut keybinds = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key == "keybind" {
            if let Some((combo, action)) = value.split_once(':') {
                keybinds.push((combo.trim().to_lowercase(), action.trim().to_owned()));
            }
        } else if SYNC_KEYS.contains(&key) {
            settings.push((key.to_owned(), value.trim().to_owned()));
        }
    }
    SyncPayload { modified: now_unix(), device: device_name(), settings, keybinds, history }
}

fn payload_to_json(payload: &SyncPayload) -> Vec<u8> {
    let settings: serde_json::Map<String, serde_json::Value> = payload
        .settings
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::json!({
        "version": 1,
        "modified": payload.modified,
        "device": payload.device,
        "settings": settings,
        "keybinds": payload.keybinds,
        "history": payload.history,
    })
    .to_string()
    .into_bytes()
}

fn payload_from_json(bytes: &[u8]) -> Result<SyncPayload, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|err| format!("同步包内容损坏：{err}"))?;
    let modified = value["modified"].as_u64().unwrap_or(0);
    let device = value["device"].as_str().unwrap_or("unknown").to_owned();
    let mut settings = Vec::new();
    if let Some(map) = value["settings"].as_object() {
        for (key, val) in map {
            // 白名单在读取侧再过滤一次：远端旧版本或恶意包都不能借同步
            // 写进机器相关键（shell=evil.exe 这类）。
            if SYNC_KEYS.contains(&key.as_str()) {
                if let Some(text) = val.as_str() {
                    settings.push((key.clone(), text.to_owned()));
                }
            }
        }
    }
    let mut keybinds = Vec::new();
    if let Some(list) = value["keybinds"].as_array() {
        for entry in list {
            if let (Some(combo), Some(action)) = (entry[0].as_str(), entry[1].as_str()) {
                keybinds.push((combo.to_lowercase(), action.to_owned()));
            }
        }
    }
    let history = value["history"]
        .as_array()
        .map(|list| list.iter().filter_map(|v| v.as_str()).map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(SyncPayload { modified, device, settings, keybinds, history })
}

/// 逗号列表并集：新方顺序优先，旧方补差，去重去空。
fn merge_list(newer: &str, older: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for item in newer.split(',').chain(older.split(',')) {
        let item = item.trim();
        if !item.is_empty() && !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen.join(",")
}

/// 历史整行去重并集，按 ts 升序（保 load 的 newest-last 语义），cap 取最新。
fn merge_history(a: &[String], b: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged: Vec<(u64, String)> = Vec::new();
    for line in a.iter().chain(b.iter()) {
        if seen.insert(line.clone()) {
            let ts = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v["ts"].as_u64())
                .unwrap_or(0);
            merged.push((ts, line.clone()));
        }
    }
    merged.sort_by_key(|(ts, _)| *ts);
    let start = merged.len().saturating_sub(HISTORY_SYNC_CAP);
    merged[start..].iter().map(|(_, line)| line.clone()).collect()
}

/// 类型感知合并（spec 003）。
pub fn merge(local: &SyncPayload, remote: &SyncPayload) -> SyncPayload {
    let (newer, older) =
        if remote.modified >= local.modified { (remote, local) } else { (local, remote) };
    let mut keybind_map: HashMap<String, String> = HashMap::new();
    for (combo, action) in older.keybinds.iter().chain(newer.keybinds.iter()) {
        keybind_map.insert(combo.clone(), action.clone());
    }
    let mut keybinds: Vec<(String, String)> = keybind_map.into_iter().collect();
    keybinds.sort();
    let mut settings = newer.settings.clone();
    for (key, older_val) in &older.settings {
        match settings.iter_mut().find(|(k, _)| k == key) {
            Some((k, v)) if LIST_KEYS.contains(&k.as_str()) => *v = merge_list(v, older_val),
            Some(_) => {},
            None => settings.push((key.clone(), older_val.clone())),
        }
    }
    SyncPayload {
        modified: newer.modified.max(older.modified),
        device: newer.device.clone(),
        settings,
        keybinds,
        history: merge_history(&older.history, &newer.history),
    }
}

/// 把合并结果写回 `nebula_settings.txt` 原文：白名单键行替换、`keybind=`
/// 行整体重写，其余（shell、目录、字体……）原样保留。
pub fn apply_to_settings_text(payload: &SyncPayload, current: &str) -> String {
    let values: HashMap<&str, &str> =
        payload.settings.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut out = String::new();
    for line in current.lines() {
        match line.split_once('=') {
            Some(("keybind", _)) => continue,
            Some((key, _)) if values.contains_key(key.trim()) => {
                out.push_str(&format!("{}={}\n", key.trim(), values[key.trim()]));
            },
            _ if line.trim().is_empty() => continue,
            _ => {
                out.push_str(line);
                out.push('\n');
            },
        }
    }
    // 本地文件缺的白名单键（远端新增设置）追加到尾部。
    let existing: Vec<&str> =
        current.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k.trim())).collect();
    for (key, value) in &payload.settings {
        if !existing.contains(&key.as_str()) {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    for (combo, action) in &payload.keybinds {
        out.push_str(&format!("keybind={combo}:{action}\n"));
    }
    out
}

// ---- 本地文件 ----

fn settings_file_path() -> PathBuf {
    crate::display::nebula_data_dir().join("nebula_settings.txt")
}

fn history_file_path(file_name: &str) -> PathBuf {
    crate::display::nebula_data_dir().join(file_name)
}

/// 本地历史尾部（最近 [`HISTORY_SYNC_CAP`] 行）。
fn history_tail() -> Vec<String> {
    let mut lines = Vec::new();
    for file_name in crate::nebula_history::history_file_names() {
        let text = std::fs::read_to_string(history_file_path(file_name)).unwrap_or_default();
        lines.extend(
            text.lines()
                .filter(|line| crate::nebula_history::record_category_file(line).is_some())
                .map(str::to_owned),
        );
    }
    lines.sort_by_key(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value["ts"].as_u64())
            .unwrap_or(0)
    });
    let start = lines.len().saturating_sub(HISTORY_SYNC_CAP);
    lines.drain(0..start);
    lines
}

fn write_history(lines: &[String]) -> Result<(), String> {
    let mut categories: HashMap<&'static str, Vec<&str>> = HashMap::new();
    for line in lines {
        let Some(file_name) = crate::nebula_history::record_category_file(line) else { continue };
        categories.entry(file_name).or_default().push(line);
    }
    for file_name in crate::nebula_history::history_file_names() {
        let mut text = categories.remove(file_name).unwrap_or_default().join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        std::fs::write(history_file_path(file_name), text)
            .map_err(|err| format!("写入历史失败：{err}"))?;
    }
    Ok(())
}

// ---- 网络 ----

struct Remote {
    payload: Option<SyncPayload>,
    etag: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn basic_auth(cfg: &SyncConfig, password: &str) -> String {
    use base64::Engine as _;
    let raw = format!("{}:{password}", cfg.username);
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw))
}

fn fetch_remote(cfg: &SyncConfig, password: &str, passphrase: &str) -> Result<Remote, String> {
    let mut response = agent()
        .get(&cfg.url)
        .header("Authorization", &basic_auth(cfg, password))
        .call()
        .map_err(|err| format!("连接失败：{err}"))?;
    match response.status().as_u16() {
        200 => {
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body =
                response.body_mut().read_to_vec().map_err(|err| format!("读取远端失败：{err}"))?;
            let payload = payload_from_json(&open_packet(&body, passphrase)?)?;
            Ok(Remote { payload: Some(payload), etag })
        },
        404 => Ok(Remote { payload: None, etag: None }),
        401 | 403 => Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
        status => Err(format!("远端返回 HTTP {status}")),
    }
}

/// PUT 封包；`etag` 是乐观锁（None = 要求远端不存在）。412 表示并发写。
fn put_remote(
    cfg: &SyncConfig,
    password: &str,
    packet: &[u8],
    etag: Option<&str>,
) -> Result<bool, String> {
    let mut request = agent().put(&cfg.url).header("Authorization", &basic_auth(cfg, password));
    request = match etag {
        Some(etag) => request.header("If-Match", etag),
        None => request.header("If-None-Match", "*"),
    };
    let response = request.send(packet).map_err(|err| format!("上传失败：{err}"))?;
    match response.status().as_u16() {
        200 | 201 | 204 => Ok(true),
        412 => Ok(false),
        401 | 403 => Err("认证失败：检查 WebDAV 用户名/密码".to_owned()),
        status => Err(format!("远端返回 HTTP {status}")),
    }
}

fn preflight() -> Result<(SyncConfig, String, String), String> {
    let cfg = SyncConfig::load();
    if !cfg.configured() {
        return Err(format!("未配置：在 {} 写入 url= 与 username=", config_path().display()));
    }
    if !cfg.url.starts_with("https://") && !cfg.allow_http {
        return Err("已拒绝：url 不是 HTTPS（自建内网服务可写 allow_http=1 豁免）".to_owned());
    }
    let password =
        webdav_password().ok_or("缺少 WebDAV 密码：设 NEBULA_WEBDAV_PASSWORD 环境变量")?;
    let passphrase =
        sync_passphrase().ok_or("缺少端到端加密口令：设 NEBULA_SYNC_PASSPHRASE 环境变量")?;
    if let Some(reason) = passphrase_weakness(&passphrase, &cfg.username, &password) {
        return Err(format!("已拒绝同步：{reason}"));
    }
    Ok((cfg, password, passphrase))
}

/// 一次同步动作的结果；`history_changed` 提示 event 层热加载历史。
pub struct SyncOutcome {
    pub message: String,
    pub history_changed: bool,
}

fn local_payload() -> (SyncPayload, String) {
    let text = std::fs::read_to_string(settings_file_path()).unwrap_or_default();
    let payload = payload_from_settings_text(&text, history_tail());
    (payload, text)
}

/// 合并结果落盘（settings + 历史），返回历史是否变化。
fn apply_local(merged: &SyncPayload, current_text: &str) -> Result<bool, String> {
    let new_text = apply_to_settings_text(merged, current_text);
    if new_text != current_text {
        std::fs::write(settings_file_path(), new_text)
            .map_err(|err| format!("写入设置失败：{err}"))?;
    }
    let local_history = history_tail();
    if merged.history != local_history {
        write_history(&merged.history)?;
        return Ok(true);
    }
    Ok(false)
}

/// 推送：本地 → 远端（412 时先合并再重试一次）。阻塞，跑在后台线程。
pub fn push() -> Result<SyncOutcome, String> {
    let (cfg, password, passphrase) = preflight()?;
    let (mut local, text) = local_payload();
    let remote = fetch_remote(&cfg, &password, &passphrase)?;
    let mut history_changed = false;
    if let Some(remote_payload) = &remote.payload {
        // 远端有别台机器的更新：先并进来，推上去的是合并结果，绝不覆盖。
        local = merge(&local, remote_payload);
        history_changed = apply_local(&local, &text)?;
    }
    local.modified = now_unix();
    local.device = device_name();
    let packet = seal(&payload_to_json(&local), &passphrase)?;
    if put_remote(&cfg, &password, &packet, remote.etag.as_deref())? {
        return Ok(SyncOutcome {
            message: format!(
                "同步已推送（{} 项设置、{} 条键位、{} 条历史）",
                local.settings.len(),
                local.keybinds.len(),
                local.history.len()
            ),
            history_changed,
        });
    }
    // 412：拉最新、再并一次、重试一次。仍冲突就把决定权还给用户。
    let fresh = fetch_remote(&cfg, &password, &passphrase)?;
    if let Some(remote_payload) = &fresh.payload {
        local = merge(&local, remote_payload);
        let text = std::fs::read_to_string(settings_file_path()).unwrap_or_default();
        history_changed = apply_local(&local, &text)? || history_changed;
    }
    let packet = seal(&payload_to_json(&local), &passphrase)?;
    if put_remote(&cfg, &password, &packet, fresh.etag.as_deref())? {
        Ok(SyncOutcome {
            message: "同步已推送（已并入远端并发修改）".to_owned(), history_changed
        })
    } else {
        Err("远端持续变化，稍后再试".to_owned())
    }
}

/// 拉取：远端 → 本地。settings 写回后由 mtime 监视自动生效；历史变化
/// 由调用方（event 层）热加载。
pub fn pull() -> Result<SyncOutcome, String> {
    let (cfg, password, passphrase) = preflight()?;
    let remote = fetch_remote(&cfg, &password, &passphrase)?;
    let Some(remote_payload) = remote.payload else {
        return Ok(SyncOutcome {
            message: "远端为空：先在这台机器推送一次".to_owned(),
            history_changed: false,
        });
    };
    let (local, text) = local_payload();
    let merged = merge(&local, &remote_payload);
    let settings_before = apply_to_settings_text(&merged, &text) != text;
    let history_changed = apply_local(&merged, &text)?;
    if !settings_before && !history_changed {
        return Ok(SyncOutcome {
            message: "已是最新（与远端一致）".to_owned(), history_changed
        });
    }
    Ok(SyncOutcome {
        message: format!(
            "已拉取 {} 的设置（{} 条键位、{} 条历史）",
            remote_payload.device,
            merged.keybinds.len(),
            merged.history.len()
        ),
        history_changed,
    })
}

/// 启动自动拉取的守门：配置齐全且 `auto_pull=1` 才值得起线程。不配置
/// 同步的用户在这里一次文件读后就此打住——加密代码永不执行。
pub fn auto_pull_enabled() -> bool {
    let cfg = SyncConfig::load();
    cfg.configured() && cfg.auto_pull && webdav_password().is_some() && sync_passphrase().is_some()
}

pub fn warn_result<T>(result: &Result<T, String>) {
    if let Err(err) = result {
        warn!("sync: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_wrong_passphrase() {
        let packet = seal(b"hello nebula", "correct horse").unwrap();
        assert_eq!(open_packet(&packet, "correct horse").unwrap(), b"hello nebula");
        assert!(open_packet(&packet, "wrong").is_err());
        assert!(open_packet(b"garbage", "correct horse").is_err());
    }

    #[test]
    fn payload_respects_whitelist() {
        let text = "theme=nebula-dark\nshell=pwsh\nstartup_directory=C:\\x\nfont_family=Maple\nsaved_hosts=root@10.0.0.1\nkeybind=ctrl+shift+t:CreateNewTab\nopacity=0.95\n";
        let payload = payload_from_settings_text(text, Vec::new());
        let keys: Vec<&str> = payload.settings.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"theme"));
        assert!(keys.contains(&"opacity"));
        // 用户裁定入集：SSH 地址簿。
        assert!(keys.contains(&"saved_hosts"));
        // 用户裁定排除：自定义字体、机器相关键。
        assert!(!keys.contains(&"font_family"));
        assert!(!keys.contains(&"shell"));
        assert!(!keys.contains(&"startup_directory"));
        assert_eq!(payload.keybinds, vec![("ctrl+shift+t".to_owned(), "CreateNewTab".to_owned())]);
    }

    #[test]
    fn json_roundtrip() {
        let payload = SyncPayload {
            modified: 1_800_000_000,
            device: "PC-A".into(),
            settings: vec![("theme".into(), "nebula-dark".into())],
            keybinds: vec![("ctrl+shift+t".into(), "CreateNewTab".into())],
            history: vec![r#"{"ts":1,"cwd":"C:\\","cmd":"cargo build"}"#.into()],
        };
        let bytes = payload_to_json(&payload);
        assert_eq!(payload_from_json(&bytes).unwrap(), payload);
    }

    #[test]
    fn remote_payload_cannot_smuggle_machine_keys() {
        // 恶意/旧版远端包里带 shell：读取侧白名单必须丢弃。
        let json = br#"{"version":1,"modified":9,"device":"evil","settings":{"theme":"x","shell":"evil.exe","font_family":"Evil"},"keybinds":[],"history":[]}"#;
        let payload = payload_from_json(json).unwrap();
        let keys: Vec<&str> = payload.settings.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["theme"]);
    }

    #[test]
    fn merge_scalars_lists_keybinds_history() {
        let local = SyncPayload {
            modified: 100,
            device: "A".into(),
            settings: vec![
                ("theme".into(), "old".into()),
                ("saved_hosts".into(), "a@1,b@2".into()),
            ],
            keybinds: vec![
                ("ctrl+shift+t".into(), "CreateNewTab".into()),
                ("f5".into(), "SplitRight".into()),
            ],
            history: vec![
                r#"{"ts":10,"cmd":"ls"}"#.into(),
                r#"{"ts":30,"cmd":"cargo test"}"#.into(),
            ],
        };
        let remote = SyncPayload {
            modified: 200,
            device: "B".into(),
            settings: vec![
                ("theme".into(), "new".into()),
                ("saved_hosts".into(), "b@2,c@3".into()),
            ],
            keybinds: vec![("ctrl+shift+t".into(), "SplitDown".into())],
            history: vec![r#"{"ts":20,"cmd":"git st"}"#.into()],
        };
        let merged = merge(&local, &remote);
        // 标量新者胜；列表并集（新方顺序优先）。
        assert!(merged.settings.contains(&("theme".to_owned(), "new".to_owned())));
        assert!(merged.settings.contains(&("saved_hosts".to_owned(), "b@2,c@3,a@1".to_owned())));
        // 键位并集 + 同 combo 新者胜。
        assert!(merged.keybinds.contains(&("ctrl+shift+t".to_owned(), "SplitDown".to_owned())));
        assert!(merged.keybinds.contains(&("f5".to_owned(), "SplitRight".to_owned())));
        // 历史整行并集按 ts 排序。
        assert_eq!(
            merged.history,
            vec![
                r#"{"ts":10,"cmd":"ls"}"#.to_owned(),
                r#"{"ts":20,"cmd":"git st"}"#.to_owned(),
                r#"{"ts":30,"cmd":"cargo test"}"#.to_owned(),
            ]
        );
    }

    #[test]
    fn apply_preserves_local_only_lines() {
        let payload = SyncPayload {
            modified: 1,
            device: "A".into(),
            settings: vec![("theme".into(), "synced".into()), ("ghost".into(), "0".into())],
            keybinds: vec![("f5".into(), "SplitRight".into())],
            history: Vec::new(),
        };
        let current = "theme=local\nshell=pwsh\nfont_family=Maple\nkeybind=ctrl+q:none\n";
        let applied = apply_to_settings_text(&payload, current);
        assert!(applied.contains("theme=synced\n"));
        assert!(applied.contains("shell=pwsh\n"));
        assert!(applied.contains("font_family=Maple\n"));
        assert!(applied.contains("ghost=0\n"));
        assert!(applied.contains("keybind=f5:SplitRight\n"));
        assert!(!applied.contains("ctrl+q"));
    }

    #[test]
    fn weak_passphrases_are_rejected() {
        assert!(passphrase_weakness("123456", "u", "pw").is_some());
        assert!(passphrase_weakness("P@ssw0rd", "u", "pw").is_some());
        assert!(passphrase_weakness("short7", "u", "pw").is_some());
        assert!(passphrase_weakness("same-as-dav", "u", "same-as-dav").is_some());
        assert!(passphrase_weakness("user@example.com", "user@example.com", "pw").is_some());
        assert!(passphrase_weakness("correct horse battery", "u", "pw").is_none());
    }
}
