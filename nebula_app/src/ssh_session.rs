//! 由 SSH 通道直接驱动的远端终端会话。
//!
//! 远端 Pane 不创建本地伪终端，但继续使用统一的输入、缩放和关闭消息协议，
//! 从而让渲染与键盘处理保持传输层无关。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use log::{error, info, warn};
use nebula_terminal::event::{Event as TerminalEvent, WindowSize};
use nebula_terminal::event_loop::{EventLoopSender, Msg, StreamProcessor};
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::Term;
use russh::ChannelMsg;
use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::ssh_key;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};

use crate::event::EventProxy;

type SessionError = Box<dyn std::error::Error + Send + Sync>;
type ClientSession = client::Handle<ClientHandler>;
type SharedSession = Arc<tokio::sync::Mutex<ClientSession>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthMethod {
    PrivateKey(PathBuf),
    StoredPassword,
    KeyboardInteractive,
    PromptPassword,
}

fn authentication_plan(
    mode: crate::ssh_profiles::SshAuthMode,
    explicit_keys: &[PathBuf],
    resolved_keys: &[PathBuf],
) -> Vec<AuthMethod> {
    use crate::ssh_profiles::SshAuthMode;

    let key_methods = || {
        let mut seen = Vec::<String>::new();
        explicit_keys
            .iter()
            .chain(resolved_keys)
            .filter(|path| {
                let normalized = path.to_string_lossy().to_lowercase();
                if seen.contains(&normalized) {
                    false
                } else {
                    seen.push(normalized);
                    true
                }
            })
            .cloned()
            .map(AuthMethod::PrivateKey)
            .collect::<Vec<_>>()
    };

    match mode {
        SshAuthMode::Auto => {
            let mut methods = key_methods();
            methods.extend([
                AuthMethod::StoredPassword,
                AuthMethod::KeyboardInteractive,
                AuthMethod::PromptPassword,
            ]);
            methods
        },
        SshAuthMode::Password => {
            vec![AuthMethod::StoredPassword, AuthMethod::PromptPassword]
        },
        SshAuthMode::PublicKey => key_methods(),
        SshAuthMode::KeyboardInteractive => vec![AuthMethod::KeyboardInteractive],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDestination {
    pub original: String,
    pub user: String,
    pub host: String,
    pub port: u16,
    identity_files: Vec<PathBuf>,
    proxy_jump: Option<String>,
}

impl SshDestination {
    pub fn parse(value: &str) -> io::Result<Self> {
        let original = value.trim().to_owned();
        let address = original.strip_prefix("ssh://").unwrap_or(&original).to_owned();
        let (user, host_port) = address.rsplit_once('@').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "SSH 地址需要包含 user@host")
        })?;
        if user.is_empty() || host_port.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH 地址不完整"));
        }

        let (host, port) = parse_host_port(host_port)?;
        Ok(Self {
            original,
            user: user.to_owned(),
            host,
            port,
            identity_files: Vec::new(),
            proxy_jump: None,
        })
    }

    /// 使用系统 SSH 的离线配置展开能力解析别名、用户名、端口和 IdentityFile。
    /// 这能保持用户现有 `~/.ssh/config` 行为，同时网络连接仍完全由 Rust 传输层承担。
    fn resolve(value: &str) -> io::Result<Self> {
        let original = value.trim().to_owned();
        if original.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH 地址为空"));
        }

        let output = Command::new(find_ssh()).arg("-G").arg("--").arg(&original).output();
        if let Ok(output) = output {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(destination) = parse_resolved_config(&original, &text) {
                    return Ok(destination);
                }
            }
        }

        if let Ok(destination) = Self::parse(&original) {
            return Ok(destination);
        }

        let address = original.strip_prefix("ssh://").unwrap_or(&original);
        let (host, port) = parse_host_port(address)?;
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "无法确定 SSH 用户名"))?;
        Ok(Self {
            original,
            user,
            host,
            port,
            identity_files: default_identity_files(),
            proxy_jump: None,
        })
    }

    fn pool_key(&self) -> String {
        format!("{}@{}:{}", self.user, self.host.to_ascii_lowercase(), self.port)
    }
}

fn parse_host_port(host_port: &str) -> io::Result<(String, u16)> {
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "无效的 IPv6 SSH 地址"))?;
        let port = suffix
            .strip_prefix(':')
            .map(str::parse)
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "无效的 SSH 端口"))?
            .unwrap_or(22);
        (host.to_owned(), port)
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        if host.contains(':') {
            (host_port.to_owned(), 22)
        } else {
            let port = port
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "无效的 SSH 端口"))?;
            (host.to_owned(), port)
        }
    } else {
        (host_port.to_owned(), 22)
    };
    if host.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH 主机为空"));
    }
    Ok((host, port))
}

fn parse_resolved_config(original: &str, text: &str) -> Option<SshDestination> {
    let mut user = None;
    let mut host = None;
    let mut port = None;
    let mut identity_files = Vec::new();
    let mut proxy_jump = None;
    for line in text.lines() {
        let (key, value) = line.split_once(char::is_whitespace)?;
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "user" if user.is_none() => user = Some(value.to_owned()),
            "hostname" if host.is_none() => host = Some(value.to_owned()),
            "port" if port.is_none() => port = value.parse().ok(),
            "identityfile" => identity_files.push(expand_home(value)),
            "proxyjump" if !value.eq_ignore_ascii_case("none") => {
                proxy_jump = Some(value.to_owned());
            },
            _ => {},
        }
    }
    Some(SshDestination {
        original: original.to_owned(),
        user: user?,
        host: host?,
        port: port.unwrap_or(22),
        identity_files,
        proxy_jump,
    })
}

fn find_ssh() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let path = PathBuf::from(root).join("System32").join("OpenSSH").join("ssh.exe");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("ssh")
}

fn expand_home(value: &str) -> PathBuf {
    let value = value.trim_matches('"');
    if let Some(rest) = value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(value)
}

fn default_identity_files() -> Vec<PathBuf> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .into_iter()
        .filter_map(|name| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| PathBuf::from(home).join(".ssh").join(name))
        })
        .collect()
}

struct ClientHandler {
    host: String,
    port: u16,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match russh::keys::known_hosts::check_known_hosts(&self.host, self.port, server_public_key)
        {
            Ok(true) => Ok(true),
            Ok(false) => {
                if confirm_new_host(&self.host, self.port, server_public_key) {
                    if let Err(err) = russh::keys::known_hosts::learn_known_hosts(
                        &self.host,
                        self.port,
                        server_public_key,
                    ) {
                        warn!("保存 SSH 主机密钥失败: {err}");
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            },
            Err(err) => {
                warn!("SSH 主机密钥验证失败: {err}");
                show_host_key_changed(&self.host, self.port, &err.to_string());
                Ok(false)
            },
        }
    }
}

pub(crate) fn runtime() -> io::Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| {
        let workers =
            std::thread::available_parallelism().map(|count| count.get().clamp(2, 4)).unwrap_or(2);
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(workers)
            .thread_name("nebula-ssh")
            .build()
            .map_err(|err| err.to_string())
    }) {
        Ok(runtime) => Ok(runtime),
        Err(err) => Err(io::Error::other(format!("SSH Runtime 初始化失败: {err}"))),
    }
}

fn connection_pool() -> &'static tokio::sync::Mutex<HashMap<String, SharedSession>> {
    static POOL: OnceLock<tokio::sync::Mutex<HashMap<String, SharedSession>>> = OnceLock::new();
    POOL.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// 启动 SSH Pane。地址解析和认证在共享 Runtime 中执行，避免阻塞窗口线程。
pub fn spawn_session(
    destination: String,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
) -> io::Result<EventLoopSender> {
    let (sender, receiver) = EventLoopSender::standalone()?;
    let profiles_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
    runtime()?.spawn(async move {
        let raw = destination.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            let resolved = SshDestination::resolve(&raw)?;
            let profiles = match crate::ssh_profiles::SshProfiles::load(&profiles_path) {
                Ok(profiles) => profiles,
                Err(err) => {
                    warn!("加载 SSH Profile 失败，使用自动认证: {err}");
                    crate::ssh_profiles::SshProfiles::default()
                },
            };
            Ok::<_, io::Error>((resolved, profiles.for_destination(&raw)))
        })
        .await;
        let result = match resolved {
            Ok(Ok((destination, profile))) => {
                run_session_async(
                    destination,
                    profile,
                    initial_size,
                    terminal.clone(),
                    event_proxy.clone(),
                    receiver,
                )
                .await
            },
            Ok(Err(err)) => Err(err.into()),
            Err(err) => Err(format!("SSH 地址解析任务失败: {err}").into()),
        };
        if let Err(err) = result {
            error!("直连 SSH 会话失败 {destination}: {err}");
            render_error(&terminal, &event_proxy, &format!("SSH 连接失败: {err}"));
        }
        terminal.lock().exit();
        event_proxy.send_event(TerminalEvent::Wakeup.into());
    });
    Ok(sender)
}

async fn run_session_async(
    destination: SshDestination,
    profile: crate::ssh_profiles::SshProfileAuth,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
    receiver: Receiver<Msg>,
) -> Result<(), SessionError> {
    if let Some(proxy_jump) = destination.proxy_jump.as_deref() {
        return Err(format!("当前直连模式尚未接入跳板机 {proxy_jump}").into());
    }

    let session = authenticated_session(&destination, &profile).await?;
    let mut channel = {
        let session = session.lock().await;
        session.channel_open_session().await?
    };
    channel
        .request_pty(
            true,
            "xterm-256color",
            u32::from(initial_size.num_cols),
            u32::from(initial_size.num_lines),
            u32::from(initial_size.cell_width) * u32::from(initial_size.num_cols),
            u32::from(initial_size.cell_height) * u32::from(initial_size.num_lines),
            &[],
        )
        .await?;
    let hook_token = remote_hook_token()?;
    channel.set_env(false, "NEBULA_REMOTE_HOOK_TOKEN", hook_token.clone()).await?;
    channel.request_shell(true).await?;

    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        while let Ok(message) = receiver.recv() {
            if input_tx.send(message).is_err() {
                break;
            }
        }
    });

    let mut stream = StreamProcessor::default();
    stream.resize(initial_size);
    stream.set_remote_hook_token(hook_token);
    loop {
        let sync_deadline = stream.next_sync_timeout();
        tokio::select! {
            message = input_rx.recv() => match message {
                Some(Msg::Input(bytes)) => channel.data(bytes.as_ref()).await?,
                Some(Msg::Resize(size)) => {
                    stream.resize(size);
                    channel.window_change(
                        u32::from(size.num_cols),
                        u32::from(size.num_lines),
                        u32::from(size.cell_width) * u32::from(size.num_cols),
                        u32::from(size.cell_height) * u32::from(size.num_lines),
                    ).await?;
                },
                Some(Msg::Shutdown) | None => {
                    let _ = channel.eof().await;
                    break;
                },
            },
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    stream.feed(&mut *terminal.lock(), &event_proxy, data.as_ref());
                    event_proxy.send_event(TerminalEvent::Wakeup.into());
                },
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Eof) | None => break,
                _ => {},
            },
            _ = wait_for_sync(sync_deadline), if sync_deadline.is_some() => {
                stream.stop_sync(&mut *terminal.lock());
                event_proxy.send_event(TerminalEvent::Wakeup.into());
            },
        }
    }
    Ok(())
}

async fn wait_for_sync(deadline: Option<std::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    }
}

async fn authenticated_session(
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
) -> Result<SharedSession, SessionError> {
    let key = destination.pool_key();
    if let Some(existing) = connection_pool().lock().await.get(&key).cloned() {
        if !existing.lock().await.is_closed() {
            info!("复用已认证 SSH 连接: {key}");
            return Ok(existing);
        }
        connection_pool().lock().await.remove(&key);
    }

    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });
    let handler = ClientHandler { host: destination.host.clone(), port: destination.port };
    let mut session =
        client::connect(config, (destination.host.as_str(), destination.port), handler).await?;
    authenticate(&mut session, destination, profile).await?;

    let session = Arc::new(tokio::sync::Mutex::new(session));
    let mut pool = connection_pool().lock().await;
    if let Some(existing) = pool.get(&key).cloned() {
        if !existing.lock().await.is_closed() {
            return Ok(existing);
        }
    }
    pool.insert(key, session.clone());
    Ok(session)
}

async fn authenticate(
    session: &mut ClientSession,
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
) -> Result<(), SessionError> {
    if session.authenticate_none(&destination.user).await?.success() {
        return Ok(());
    }

    let plan =
        authentication_plan(profile.auth, &profile.private_keys, &destination.identity_files);
    let key_count =
        plan.iter().filter(|method| matches!(method, AuthMethod::PrivateKey(_))).count();
    if profile.auth == crate::ssh_profiles::SshAuthMode::Auto && key_count >= 3 {
        warn!(
            "自动认证将尝试 {key_count} 把私钥；若服务器触发 MaxAuthTries，请在 SSH Profile 中明确指定密钥"
        );
    }

    let mut reusable_password = None;
    let mut loaded_stored_password = false;
    let mut stored_password_was_present = false;
    for method in plan {
        match method {
            AuthMethod::PrivateKey(path) => {
                if try_private_key(session, destination, &path).await? {
                    clear_secret(&mut reusable_password);
                    return Ok(());
                }
            },
            AuthMethod::StoredPassword => {
                if !loaded_stored_password {
                    reusable_password =
                        crate::ssh_credentials::load_stored_password(&destination.original)?;
                    loaded_stored_password = true;
                    stored_password_was_present = reusable_password.is_some();
                }
                if let Some(password) = reusable_password.as_deref() {
                    if authenticate_password(session, &destination.user, password).await? {
                        clear_secret(&mut reusable_password);
                        return Ok(());
                    }
                }
            },
            AuthMethod::KeyboardInteractive => {
                if try_keyboard_interactive(session, destination, reusable_password.as_deref())
                    .await?
                {
                    clear_secret(&mut reusable_password);
                    return Ok(());
                }
            },
            AuthMethod::PromptPassword => {
                if let Some((mut password, save)) =
                    prompt_secret(destination.original.clone(), None, true).await?
                {
                    let accepted =
                        authenticate_password(session, &destination.user, &password).await?;
                    if accepted {
                        if save {
                            crate::ssh_credentials::store_password(
                                &destination.original,
                                &password,
                            )?;
                        }
                        password.fill(0);
                        clear_secret(&mut reusable_password);
                        return Ok(());
                    }
                    password.fill(0);
                }
            },
        }
    }

    if stored_password_was_present {
        crate::ssh_credentials::forget_password(&destination.original)?;
    }
    clear_secret(&mut reusable_password);
    Err(auth_failure(profile.auth, key_count).into())
}

/// 在现有认证连接上打开独立 SFTP 子系统；连接池和认证策略仍只有一份。
pub(crate) async fn open_sftp(
    raw_destination: &str,
) -> Result<russh_sftp::client::SftpSession, SessionError> {
    let profiles_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
    let raw = raw_destination.to_owned();
    let (destination, profile) = tokio::task::spawn_blocking(move || {
        let destination = SshDestination::resolve(&raw)?;
        let profiles = match crate::ssh_profiles::SshProfiles::load(&profiles_path) {
            Ok(profiles) => profiles,
            Err(err) => {
                warn!("加载 SSH Profile 失败，SFTP 使用自动认证: {err}");
                crate::ssh_profiles::SshProfiles::default()
            },
        };
        Ok::<_, io::Error>((destination, profiles.for_destination(&raw)))
    })
    .await
    .map_err(|err| format!("SSH 地址解析任务失败: {err}"))??;

    if let Some(proxy_jump) = destination.proxy_jump.as_deref() {
        return Err(format!("当前 SFTP 模式尚未接入跳板机 {proxy_jump}").into());
    }

    let session = authenticated_session(&destination, &profile).await?;
    let channel = {
        let session = session.lock().await;
        session.channel_open_session().await?
    };
    channel.request_subsystem(true, "sftp").await?;
    Ok(russh_sftp::client::SftpSession::new(channel.into_stream()).await?)
}

fn auth_failure(mode: crate::ssh_profiles::SshAuthMode, key_count: usize) -> String {
    use crate::ssh_profiles::SshAuthMode;
    match mode {
        SshAuthMode::Auto if key_count >= 3 => format!(
            "服务器拒绝了自动认证；已尝试 {key_count} 把私钥，可能触发 MaxAuthTries，请明确选择一把密钥"
        ),
        SshAuthMode::Auto => "服务器拒绝了所有可用的 SSH 认证方式".to_owned(),
        SshAuthMode::Password => "服务器拒绝了密码认证，未回退到其他认证方式".to_owned(),
        SshAuthMode::PublicKey if key_count == 0 => {
            "密钥认证没有可用的私钥，请选择私钥文件或配置 IdentityFile".to_owned()
        },
        SshAuthMode::PublicKey => "服务器拒绝了指定的私钥，未回退到密码认证".to_owned(),
        SshAuthMode::KeyboardInteractive => {
            "服务器拒绝了 keyboard-interactive 认证，未回退到密码认证".to_owned()
        },
    }
}

async fn try_private_key(
    session: &mut ClientSession,
    destination: &SshDestination,
    path: &Path,
) -> Result<bool, SessionError> {
    let private_key = match std::fs::read(path) {
        Ok(private_key) => private_key,
        Err(err) => {
            warn!("无法读取 SSH 私钥 {}: {err}", path.display());
            return Ok(false);
        },
    };
    let prompt = format!("密钥口令: {}", path.display());
    let mut stored = crate::ssh_credentials::load_private_key_passphrase(&private_key)?;
    let mut key = stored
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|passphrase| russh::keys::load_secret_key(path, Some(passphrase)).ok())
        .or_else(|| russh::keys::load_secret_key(path, None).ok());

    if key.is_none() {
        clear_secret(&mut stored);
        crate::ssh_credentials::forget_private_key_passphrase(&private_key)?;
        if let Some((mut passphrase, save)) = prompt_secret(prompt, None, true).await? {
            let text = String::from_utf8_lossy(&passphrase).into_owned();
            key = russh::keys::load_secret_key(path, Some(&text)).ok();
            if key.is_some() && save {
                crate::ssh_credentials::store_private_key_passphrase(&private_key, &passphrase)?;
            }
            passphrase.fill(0);
        }
    }
    clear_secret(&mut stored);
    let Some(key) = key else { return Ok(false) };

    let key = Arc::new(key);
    let cert_path = PathBuf::from(format!("{}-cert.pub", path.display()));
    if cert_path.is_file() {
        if let Ok(certificate) = russh::keys::load_openssh_certificate(&cert_path) {
            if session
                .authenticate_openssh_cert(&destination.user, key.clone(), certificate)
                .await?
                .success()
            {
                return Ok(true);
            }
        }
    }

    let hash = rsa_hash_for(session, key.algorithm().is_rsa()).await;
    let key = PrivateKeyWithHashAlg::new(key, hash);
    Ok(session.authenticate_publickey(&destination.user, key).await?.success())
}

async fn rsa_hash_for(session: &ClientSession, rsa: bool) -> Option<HashAlg> {
    if !rsa {
        return None;
    }
    match session.best_supported_rsa_hash().await {
        Ok(Some(hash)) => hash,
        _ => Some(HashAlg::Sha512),
    }
}

async fn authenticate_password(
    session: &mut ClientSession,
    user: &str,
    password: &[u8],
) -> Result<bool, SessionError> {
    let password = String::from_utf8(password.to_vec())?;
    Ok(session.authenticate_password(user, password).await?.success())
}

async fn try_keyboard_interactive(
    session: &mut ClientSession,
    destination: &SshDestination,
    password: Option<&[u8]>,
) -> Result<bool, SessionError> {
    let mut state =
        session.authenticate_keyboard_interactive_start(&destination.user, None::<String>).await?;
    for _ in 0..8 {
        match state {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                let mut responses = Vec::with_capacity(prompts.len());
                for prompt in prompts {
                    if !prompt.echo
                        && prompt.prompt.to_ascii_lowercase().contains("password")
                        && password.is_some()
                    {
                        responses.push(String::from_utf8_lossy(password.unwrap()).into_owned());
                        continue;
                    }
                    let label = format!(
                        "{} - {} {} {}",
                        destination.original, name, instructions, prompt.prompt
                    );
                    let Some((mut response, _)) = prompt_secret(label, None, false).await? else {
                        return Ok(false);
                    };
                    responses.push(String::from_utf8_lossy(&response).into_owned());
                    response.fill(0);
                }
                state = session.authenticate_keyboard_interactive_respond(responses).await?;
            },
        }
    }
    Ok(false)
}

async fn prompt_secret(
    destination: String,
    initial: Option<Vec<u8>>,
    allow_save: bool,
) -> io::Result<Option<(Vec<u8>, bool)>> {
    tokio::task::spawn_blocking(move || {
        crate::ssh_credentials::prompt_password(&destination, initial.as_deref(), allow_save)
    })
    .await
    .map_err(|err| io::Error::other(format!("凭据输入任务失败: {err}")))?
}

fn clear_secret(secret: &mut Option<Vec<u8>>) {
    if let Some(secret) = secret.as_mut() {
        secret.fill(0);
    }
    *secret = None;
}

fn remote_hook_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| io::Error::other(format!("生成 SSH Hook 令牌失败: {err}")))?;
    let mut token = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

fn render_error(
    terminal: &Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: &EventProxy,
    message: &str,
) {
    let mut stream = StreamProcessor::default();
    let text = format!("\r\n\x1b[31m{message}\x1b[0m\r\n");
    stream.feed(&mut *terminal.lock(), event_proxy, text.as_bytes());
    event_proxy.send_event(TerminalEvent::Wakeup.into());
}

#[cfg(windows)]
fn confirm_new_host(host: &str, port: u16, key: &ssh_key::PublicKey) -> bool {
    use std::ptr::null_mut;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
    };

    let fingerprint = key.fingerprint(ssh_key::HashAlg::Sha256);
    let text = wide(&format!(
        "首次连接到 {host}:{port}。\n\n主机密钥：{fingerprint}\n\n是否信任并保存此主机密钥？"
    ));
    let title = wide("Nebula SSH");
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
        ) == IDYES
    }
}

#[cfg(windows)]
fn show_host_key_changed(host: &str, port: u16, detail: &str) {
    use std::ptr::null_mut;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    let text = wide(&format!(
        "{host}:{port} 的主机密钥与已保存记录不一致。\n\n连接已终止，以避免连接到错误的主机。\n\n{detail}"
    ));
    let title = wide("Nebula SSH");
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{AuthMethod, SshDestination, authentication_plan, parse_resolved_config};
    use crate::ssh_profiles::SshAuthMode;
    use std::path::PathBuf;

    #[test]
    fn parses_saved_destinations() {
        let plain = SshDestination::parse("root@example.com").unwrap();
        assert_eq!(
            (plain.user.as_str(), plain.host.as_str(), plain.port),
            ("root", "example.com", 22)
        );

        let uri = SshDestination::parse("ssh://alice@example.com:2200").unwrap();
        assert_eq!(
            (uri.user.as_str(), uri.host.as_str(), uri.port),
            ("alice", "example.com", 2200)
        );

        let ipv6 = SshDestination::parse("ssh://root@[2001:db8::1]:2222").unwrap();
        assert_eq!((ipv6.host.as_str(), ipv6.port), ("2001:db8::1", 2222));
    }

    #[test]
    fn parses_resolved_ssh_config() {
        let config =
            "user deploy\nhostname server.internal\nport 2200\nidentityfile ~/.ssh/id_ed25519\n";
        let destination = parse_resolved_config("prod", config).unwrap();
        assert_eq!(destination.user, "deploy");
        assert_eq!(destination.host, "server.internal");
        assert_eq!(destination.port, 2200);
        assert_eq!(destination.identity_files.len(), 1);
    }

    #[test]
    fn auto_auth_plan_matches_tabby_order_and_deduplicates_keys() {
        let explicit = vec![PathBuf::from(r"C:\Keys\chosen"), PathBuf::from(r"c:\keys\CHOSEN")];
        let resolved = vec![PathBuf::from(r"C:\Keys\config")];

        assert_eq!(
            authentication_plan(SshAuthMode::Auto, &explicit, &resolved),
            vec![
                AuthMethod::PrivateKey(PathBuf::from(r"C:\Keys\chosen")),
                AuthMethod::PrivateKey(PathBuf::from(r"C:\Keys\config")),
                AuthMethod::StoredPassword,
                AuthMethod::KeyboardInteractive,
                AuthMethod::PromptPassword,
            ]
        );
    }

    #[test]
    fn password_mode_never_falls_back_to_other_methods() {
        assert_eq!(
            authentication_plan(
                SshAuthMode::Password,
                &[PathBuf::from(r"C:\Keys\ignored")],
                &[PathBuf::from(r"C:\Keys\ignored-config")],
            ),
            vec![AuthMethod::StoredPassword, AuthMethod::PromptPassword]
        );
    }

    #[test]
    fn public_key_mode_uses_only_key_sources() {
        assert_eq!(
            authentication_plan(
                SshAuthMode::PublicKey,
                &[PathBuf::from(r"C:\Keys\chosen")],
                &[PathBuf::from(r"C:\Keys\config")],
            ),
            vec![
                AuthMethod::PrivateKey(PathBuf::from(r"C:\Keys\chosen")),
                AuthMethod::PrivateKey(PathBuf::from(r"C:\Keys\config")),
            ]
        );
    }

    #[test]
    fn interactive_mode_is_strict() {
        assert_eq!(
            authentication_plan(SshAuthMode::KeyboardInteractive, &[], &[]),
            vec![AuthMethod::KeyboardInteractive]
        );
    }
}
