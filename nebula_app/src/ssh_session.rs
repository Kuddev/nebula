//! 由 SSH 通道直接驱动的远端终端会话。
//!
//! 远端 Pane 不创建本地伪终端，但继续使用统一的输入、缩放和关闭消息协议，
//! 从而让渲染与键盘处理保持传输层无关。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::event::EventProxy;

type SessionError = Box<dyn std::error::Error + Send + Sync>;
type ClientSession = client::Handle<ClientHandler>;
type SharedSession = Arc<tokio::sync::Mutex<ClientSession>>;

/// 直连 SSH 会话的宿主回调抽象：终端事件泵（[`EventListener`]）+ 连接
/// 阶段上报。旧壳 `EventProxy`（winit 事件循环）与 GPUI 会话代理都实现
/// 它，[`spawn_session`] 因此对 UI 壳无感——同一条 russh 业务路径服务
/// 两个壳，不产生第二套连接语义。
///
/// [`EventListener`]: nebula_terminal::event::EventListener
pub trait SshEventHost:
    nebula_terminal::event::EventListener + Clone + Send + Sync + 'static
{
    /// 连接阶段变化（连接卡片/横幅的数据源）。默认丢弃。
    fn ssh_stage(&self, stage: SshStage) {
        let _ = stage;
    }
}

impl SshEventHost for EventProxy {
    fn ssh_stage(&self, stage: SshStage) {
        // [`EventProxy`] 自带 `tab_id`，后台 runtime 不需要知道 pane id
        // 或 window id（固有 `send_event(EventType)`，非 trait 方法）。
        self.send_event(crate::event::EventType::SshConnect(stage));
    }
}

/// 直连 SSH 会话的连接阶段。
///
/// 这些取值不是估算的进度百分比——因为我们用 russh 自己实现客户端，而不是
/// spawn `ssh.exe` 再解析 `-v` 输出，每个阶段都对应一个真实的调用点：
/// [`SshDestination::resolve`] → [`client::connect`] → [`authenticate`] →
/// `channel_open_session` + `request_pty` + `request_shell`。
///
/// 连接池命中时 [`authenticated_session`] 直接返回既有连接，`Connect` 与
/// `Authenticate` 都不会上报：复用是瞬时的，连接卡片也就不会浮出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshStage {
    /// 正在解析地址（含 `ssh -G` 读 `~/.ssh/config`）。
    Resolve,
    /// 正在建立 TCP 连接并完成协议握手/密钥交换。
    Connect,
    /// 正在认证。
    Authenticate,
    /// 正在打开 channel、申请 pty 与 shell。
    OpenShell,
    /// 会话已就绪：卡片让位给真实终端。
    Ready,
    /// 连接失败，附带面向用户的原因。
    Failed(String),
}

/// 向拥有该 pane 的窗口上报连接阶段（[`SshEventHost::ssh_stage`]）。
fn report_stage<H: SshEventHost>(progress: Option<&H>, stage: SshStage) {
    if let Some(proxy) = progress {
        proxy.ssh_stage(stage);
    }
}

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
pub fn spawn_session<H: SshEventHost>(
    destination: String,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<H>>>,
    event_proxy: H,
) -> io::Result<EventLoopSender> {
    let (sender, receiver) = EventLoopSender::standalone()?;
    let profiles_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
    runtime()?.spawn(async move {
        // 地址解析是第一个真实阶段：`ssh -G` 要读 ~/.ssh/config，慢链路上
        // 这一步本身就可能耗时。
        report_stage(Some(&event_proxy), SshStage::Resolve);
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
        // 会话是否曾经就绪。它把两种 `Err` 分开：连接建立**之前**失败要把
        // 卡片留在屏幕上让用户读原因；建立**之后**断开则是普通的会话结束，
        // 必须照常退出 pane，否则会留下一个连不上也关不掉的空壳。
        let ready = Arc::new(AtomicBool::new(false));
        let result = match resolved {
            Ok(Ok((destination, profile))) => {
                run_session_async(
                    destination,
                    profile,
                    initial_size,
                    terminal.clone(),
                    event_proxy.clone(),
                    receiver,
                    ready.clone(),
                )
                .await
            },
            Ok(Err(err)) => Err(err.into()),
            Err(err) => Err(format!("SSH 地址解析任务失败: {err}").into()),
        };
        match result {
            Err(err) if !ready.load(Ordering::Relaxed) => {
                // 这里刻意**不用** `error!`：它会额外推一条红色 message bar，
                // 而连接卡片已经把同一条原因连同阶段和日志一起呈现了。同一个
                // 错误报两次，其中一次还被卡片的遮罩切成半残的红条。
                info!("直连 SSH 会话失败 {destination}: {err}");
                report_stage(Some(&event_proxy), SshStage::Failed(err.to_string()));
                // 错误也写进 grid：卡片关掉之后它仍然留在回滚里。
                render_error(&terminal, &event_proxy, &format!("SSH 连接失败: {err}"));
                // 这里**不** exit：pane 一退出 tab 就关了，卡片和它携带的
                // 失败原因会一起消失。收尾交给卡片上的「关闭」。
            },
            Err(err) => {
                // 会话中途断开时没有卡片在场，message bar 是唯一的提示渠道。
                error!("直连 SSH 会话中断 {destination}: {err}");
                render_error(&terminal, &event_proxy, &format!("SSH 连接失败: {err}"));
                terminal.lock().exit();
            },
            Ok(()) => terminal.lock().exit(),
        }
        event_proxy.send_event(TerminalEvent::Wakeup);
    });
    Ok(sender)
}

async fn run_session_async<H: SshEventHost>(
    destination: SshDestination,
    profile: crate::ssh_profiles::SshProfileAuth,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<H>>>,
    event_proxy: H,
    receiver: Receiver<Msg>,
    ready: Arc<AtomicBool>,
) -> Result<(), SessionError> {
    let session = authenticated_session(&destination, &profile, Some(&event_proxy)).await?;
    report_stage(Some(&event_proxy), SshStage::OpenShell);
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
    // Shell 已就绪：连接卡片到此让位给真实终端，持续重绘随之停止。此后再
    // 出错就是会话中途断开，不该复活卡片。
    ready.store(true, Ordering::Relaxed);
    report_stage(Some(&event_proxy), SshStage::Ready);

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
                    event_proxy.send_event(TerminalEvent::Wakeup);
                },
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Eof) | None => break,
                _ => {},
            },
            _ = wait_for_sync(sync_deadline), if sync_deadline.is_some() => {
                stream.stop_sync(&mut *terminal.lock());
                event_proxy.send_event(TerminalEvent::Wakeup);
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

async fn authenticated_session<H: SshEventHost>(
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
    progress: Option<&H>,
) -> Result<SharedSession, SessionError> {
    authenticated_session_at(destination, profile, progress, 0).await
}

/// [`authenticated_session`] 的带深度版本。`depth` 是当前跳板层级：目标连接
/// 为 0，它的跳板为 1……跳板递归（[`open_transport`] 的 `Jump` 分支）经
/// [`authenticated_session_boxed`] 回到这里，深度上限在那边把关。
async fn authenticated_session_at<H: SshEventHost>(
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
    progress: Option<&H>,
    depth: u8,
) -> Result<SharedSession, SessionError> {
    // 代理决策先于连接池查找：pool key 必须带上代理身份，否则改完代理设置
    // 还在复用旧代理建立的连接，表现为「改了设置没生效」。配置写错在这里
    // 直接报错——静默直连会把配置问题伪装成网络问题。
    let global = tokio::task::spawn_blocking(crate::ssh_proxy::SshProxyConfig::load_global)
        .await
        .map_err(|err| format!("读取代理配置任务失败: {err}"))?;
    let proxy = resolve_network_proxy(&global, destination)
        .map_err(|err| format!("SSH 代理配置无效: {err}"))?;
    let key = match &proxy {
        Some(link) => format!("{}|{}", destination.pool_key(), link.identity()),
        None => destination.pool_key(),
    };
    if let Some(existing) = connection_pool().lock().await.get(&key).cloned() {
        if !existing.lock().await.is_closed() {
            info!("复用已认证 SSH 连接: {key}");
            // 复用不上报 Connect/Authenticate：这条路径是瞬时的，交给
            // 350ms 门槛把卡片整个吃掉，用户看到的就是直接出 prompt。
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
    report_stage(progress, SshStage::Connect);
    let mut session = open_transport(proxy.as_ref(), config, destination, handler, depth).await?;
    report_stage(progress, SshStage::Authenticate);
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

/// SSH/SFTP 的唯一 Nebula 代理决策入口。主机编辑器不再维护第二套代理；
/// OpenSSH `ProxyJump` 是目标解析的一部分，仍需先于全局网络设置生效。
fn resolve_network_proxy(
    global: &crate::ssh_proxy::SshProxyConfig,
    destination: &SshDestination,
) -> Result<Option<crate::ssh_proxy::ProxyLink>, String> {
    global.resolve(destination.proxy_jump.as_deref(), &destination.host)
}

/// 跳板递归需要的类型擦除：async fn 相互递归时 future 类型无限展开，
/// `Box<dyn Future>` 在这里切断它。
fn authenticated_session_boxed<'a, H: SshEventHost>(
    destination: &'a SshDestination,
    profile: &'a crate::ssh_profiles::SshProfileAuth,
    progress: Option<&'a H>,
    depth: u8,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<SharedSession, SessionError>> + Send + 'a>,
> {
    Box::pin(authenticated_session_at(destination, profile, progress, depth))
}

/// 建立 SSH 传输层。直连交给 russh 自己开 TCP；代理服务器先完成隧道握手，
/// 再把裸流交给 `connect_stream`；跳板则先对跳板主机走一遍完整的
/// 连接 + 认证（沿用它自己的 profile / 密钥 / 代理配置，会话进连接池可
/// 复用），再在其上开 direct-tcpip 通道当传输层——三条路返回同一种句柄。
async fn open_transport(
    proxy: Option<&crate::ssh_proxy::ProxyLink>,
    config: Arc<client::Config>,
    destination: &SshDestination,
    handler: ClientHandler,
    depth: u8,
) -> Result<ClientSession, SessionError> {
    use crate::ssh_proxy::ProxyLink;
    match proxy {
        Some(ProxyLink::Server(server)) => {
            info!("经代理 {} 连接 {}:{}", server.display(), destination.host, destination.port);
            let stream = crate::ssh_proxy::connect(server, &destination.host, destination.port)
                .await
                .map_err(|err| format!("经代理 {} 连接失败: {err}", server.display()))?;
            Ok(client::connect_stream(config, stream, handler).await?)
        },
        Some(ProxyLink::Jump(spec)) => {
            // 上限 2 级：target → 跳板 → 跳板的跳板。成环配置（a jump b、
            // b jump a）也在这里截断，报错而不是栈溢出。
            if depth >= 2 {
                return Err(format!("跳板链过深或存在循环（经 {spec}，最多 2 级跳板）").into());
            }
            info!("经跳板 {spec} 连接 {}:{}", destination.host, destination.port);
            let (jump_destination, jump_profile) = tokio::task::spawn_blocking({
                let spec = spec.clone();
                move || {
                    let jump_destination = SshDestination::resolve(&spec)?;
                    let path = crate::display::nebula_data_dir().join("ssh_profiles.json");
                    let profile = match crate::ssh_profiles::SshProfiles::load(&path) {
                        Ok(profiles) => profiles.for_destination(&spec),
                        Err(err) => {
                            warn!("加载跳板 SSH Profile 失败，使用自动认证: {err}");
                            crate::ssh_profiles::SshProfiles::default().for_destination(&spec)
                        },
                    };
                    Ok::<_, io::Error>((jump_destination, profile))
                }
            })
            .await
            .map_err(|err| format!("解析跳板地址任务失败: {err}"))?
            .map_err(|err| format!("解析跳板 {spec} 失败: {err}"))?;
            // 跳板阶段不上报进度：连接卡片的阶段属于目标主机，来回横跳
            // 只会让用户以为连接在抽风。
            let jump_session = authenticated_session_boxed(
                &jump_destination,
                &jump_profile,
                None::<&EventProxy>,
                depth + 1,
            )
            .await
            .map_err(|err| format!("连接跳板 {spec} 失败: {err}"))?;
            let channel = {
                let session = jump_session.lock().await;
                session
                    .channel_open_direct_tcpip(
                        destination.host.clone(),
                        u32::from(destination.port),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|err| {
                        format!(
                            "经跳板 {spec} 转发到 {}:{} 失败: {err}",
                            destination.host, destination.port
                        )
                    })?
            };
            Ok(client::connect_stream(config, channel.into_stream(), handler).await?)
        },
        Some(ProxyLink::Command(command)) => {
            info!("经自定义命令连接 {}:{}", destination.host, destination.port);
            let stream =
                crate::ssh_proxy::connect_command(command, &destination.host, destination.port)
                    .await
                    .map_err(|err| format!("自定义代理命令启动失败: {err}"))?;
            Ok(client::connect_stream(config, stream, handler).await?)
        },
        None => {
            Ok(client::connect(config, (destination.host.as_str(), destination.port), handler)
                .await?)
        },
    }
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
    let mut local_key_errors = Vec::new();
    for method in plan {
        match method {
            AuthMethod::PrivateKey(path) => {
                if try_private_key(session, destination, &path, true, &mut local_key_errors).await?
                {
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
    Err(auth_failure(profile.auth, key_count, &local_key_errors).into())
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
        info!("SFTP 将经跳板 {proxy_jump} 建立");
    }

    // SFTP 面板自己有加载态，不参与终端 pane 的连接卡片。
    let session = authenticated_session(&destination, &profile, None::<&EventProxy>).await?;
    let channel = {
        let session = session.lock().await;
        session.channel_open_session().await?
    };
    channel.request_subsystem(true, "sftp").await?;
    Ok(russh_sftp::client::SftpSession::new(channel.into_stream()).await?)
}

// ---- 「测试连接」（SSH 编辑器页脚，spec ui-redesign 稿一） ----

/// 编辑器草稿的连通性测试请求。带草稿密码/密钥而不是磁盘 profile——
/// 测试要回答「保存后能不能连上」，不是「上次保存的配置行不行」。
#[derive(Debug, Clone)]
pub struct SshTestRequest {
    /// Window-local monotonic id. The editor may be changed and tested again
    /// while an older network task is still finishing, so address equality is
    /// not sufficient to associate a result with its request.
    pub request_id: u64,
    pub destination: String,
    pub auth: crate::ssh_profiles::SshAuthMode,
    pub private_keys: Vec<PathBuf>,
    /// 密码框里的未保存草稿；`None`/空 = 只用密钥与已存凭据。
    pub password: Option<String>,
}

const TEST_TIMEOUT: Duration = Duration::from_secs(12);

/// 一次草稿测试的完成数据。旧 winit 壳与 GPUI 设置页共用它：业务层只负责
/// 解析、代理和认证，具体 UI 决定怎样把结果投递回自己的事件循环。
#[derive(Debug, Clone)]
pub struct SshTestResult {
    pub request_id: u64,
    pub destination: String,
    pub ok: bool,
    pub message: String,
    pub elapsed_ms: u64,
}

/// 执行一次无人值守的草稿测试。绝不弹 AskPass：交互式方法在测试中一律跳过
/// （见 [`test_authenticate`]）。
async fn run_test(request: SshTestRequest) -> SshTestResult {
    let started = std::time::Instant::now();
    let request_id = request.request_id;
    let raw = request.destination.clone();
    let outcome = tokio::time::timeout(TEST_TIMEOUT, async {
        let (resolved, global) = tokio::task::spawn_blocking({
            let raw = raw.clone();
            move || {
                let resolved = SshDestination::resolve(&raw)?;
                let global = crate::ssh_proxy::SshProxyConfig::load_global();
                Ok::<_, io::Error>((resolved, global))
            }
        })
        .await
        .map_err(|err| -> SessionError { format!("SSH 地址解析任务失败: {err}").into() })??;
        // SSH 编辑器不再维护重复的每主机代理；测试与真连都只读取网络页
        // 的当前配置。`ProxyJump` 属于 OpenSSH 目标解析，仍按其原语义保留。
        let proxy = resolve_network_proxy(&global, &resolved)
            .map_err(|err| -> SessionError { format!("SSH 代理配置无效: {err}").into() })?;
        test_connect(&resolved, &request, proxy.as_ref()).await
    })
    .await;
    let (ok, message) = match outcome {
        Ok(Ok(())) => (true, String::new()),
        Ok(Err(err)) => (false, err.to_string()),
        Err(_) => (false, format!("连接超时（{} 秒无响应）", TEST_TIMEOUT.as_secs())),
    };
    SshTestResult {
        request_id,
        destination: raw,
        ok,
        message,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

/// 启动测试并交给调用方异步等待结果。GPUI 没有旧壳的 winit `EventLoopProxy`，
/// 所以它通过这个 receiver 把结果安全地回写到自己的 Entity；网络与认证仍然
/// 完全复用同一条 Tokio SSH runtime。
pub fn start_test(
    request: SshTestRequest,
) -> io::Result<tokio::sync::oneshot::Receiver<SshTestResult>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    runtime()?.spawn(async move {
        let _ = sender.send(run_test(request).await);
    });
    Ok(receiver)
}

/// 旧 winit 壳的事件投递适配器。测试实现本身在 [`run_test`]，避免两个壳
/// 各自维护解析、代理和认证的近似副本。
pub fn spawn_test(
    request: SshTestRequest,
    proxy: winit::event_loop::EventLoopProxy<crate::event::Event>,
    window_id: winit::window::WindowId,
) -> io::Result<()> {
    runtime()?.spawn(async move {
        let result = run_test(request).await;
        let _ = proxy.send_event(crate::event::Event::new(
            crate::event::EventType::SshTestDone {
                request_id: result.request_id,
                destination: result.destination,
                ok: result.ok,
                message: result.message,
                elapsed_ms: result.elapsed_ms,
            },
            window_id,
        ));
    });
    Ok(())
}

// ---- 设置→网络：真实出网测试 ----

const NETWORK_TEST_HOST: &str = "example.com";
const NETWORK_TEST_PORT: u16 = 80;

trait NetworkTestStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> NetworkTestStream for T {}

/// 使用与 SSH 新连接相同的全局配置解析和代理握手建立字节流，再请求一个
/// 真实 HTTP 页面。只探测代理端口并不能证明代理有出网能力，所以这里必须
/// 收到目标站点的 HTTP 状态行才算成功。
pub fn spawn_proxy_test(
    request_id: u64,
    proxy: winit::event_loop::EventLoopProxy<crate::event::Event>,
    window_id: winit::window::WindowId,
) -> io::Result<()> {
    runtime()?.spawn(async move {
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(TEST_TIMEOUT, proxy_test_once()).await;
        let (ok, message) = match outcome {
            Ok(Ok(route)) => (true, route),
            Ok(Err(err)) => (false, err.to_string()),
            Err(_) => (false, format!("网络测试超时（{} 秒无响应）", TEST_TIMEOUT.as_secs())),
        };
        let _ = proxy.send_event(crate::event::Event::new(
            crate::event::EventType::ProxyTestDone {
                request_id,
                ok,
                message,
                elapsed_ms: started.elapsed().as_millis() as u64,
            },
            window_id,
        ));
    });
    Ok(())
}

async fn proxy_test_once() -> Result<String, SessionError> {
    let global = tokio::task::spawn_blocking(crate::ssh_proxy::SshProxyConfig::load_global)
        .await
        .map_err(|err| format!("读取代理设置失败: {err}"))?;
    let link =
        global.resolve(None, NETWORK_TEST_HOST).map_err(|err| format!("代理设置无效: {err}"))?;
    let route = match &link {
        Some(crate::ssh_proxy::ProxyLink::Server(server)) => server.display(),
        Some(crate::ssh_proxy::ProxyLink::Jump(target)) => format!("SSH {target}"),
        Some(crate::ssh_proxy::ProxyLink::Command(_)) => "自定义命令".to_owned(),
        None if global.mode == crate::ssh_proxy::ProxyMode::Custom => "直连地址".to_owned(),
        None => "直接连接".to_owned(),
    };
    let mut stream = proxy_test_stream(link.as_ref()).await?;
    stream
        .write_all(
            b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\nUser-Agent: Nebula-Network-Test\r\n\r\n",
        )
        .await
        .map_err(|err| format!("发送网页测试请求失败: {err}"))?;
    stream.flush().await.map_err(|err| format!("发送网页测试请求失败: {err}"))?;

    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while response.len() < 2048 && !response.windows(2).any(|part| part == b"\r\n") {
        let read =
            stream.read(&mut chunk).await.map_err(|err| format!("读取网页测试响应失败: {err}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&response);
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("目标站点没有返回有效 HTTP 状态行: {status_line}"))?;
    if !(200..500).contains(&status) {
        return Err(format!("目标站点返回 HTTP {status}").into());
    }
    Ok(route)
}

async fn proxy_test_stream(
    link: Option<&crate::ssh_proxy::ProxyLink>,
) -> Result<Box<dyn NetworkTestStream>, SessionError> {
    use crate::ssh_proxy::ProxyLink;
    match link {
        Some(ProxyLink::Server(server)) => {
            crate::ssh_proxy::connect(server, NETWORK_TEST_HOST, NETWORK_TEST_PORT)
                .await
                .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
                .map_err(|err| format!("经代理 {} 出网失败: {err}", server.display()).into())
        },
        Some(ProxyLink::Command(command)) => {
            crate::ssh_proxy::connect_command(command, NETWORK_TEST_HOST, NETWORK_TEST_PORT)
                .await
                .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
                .map_err(|err| format!("自定义代理命令出网失败: {err}").into())
        },
        Some(ProxyLink::Jump(spec)) => {
            let (jump_destination, jump_profile) = tokio::task::spawn_blocking({
                let spec = spec.clone();
                move || {
                    let destination = SshDestination::resolve(&spec)?;
                    let path = crate::display::nebula_data_dir().join("ssh_profiles.json");
                    let profile = crate::ssh_profiles::SshProfiles::load(&path)
                        .unwrap_or_default()
                        .for_destination(&spec);
                    Ok::<_, io::Error>((destination, profile))
                }
            })
            .await
            .map_err(|err| format!("解析跳板任务失败: {err}"))??;
            let jump = authenticated_session_at::<EventProxy>(
                &jump_destination,
                &jump_profile,
                None,
                1,
            )
            .await?;
            let channel = {
                let session = jump.lock().await;
                session
                    .channel_open_direct_tcpip(
                        NETWORK_TEST_HOST,
                        u32::from(NETWORK_TEST_PORT),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|err| format!("经跳板 {spec} 打开出网通道失败: {err}"))?
            };
            Ok(Box::new(channel.into_stream()))
        },
        None => tokio::net::TcpStream::connect((NETWORK_TEST_HOST, NETWORK_TEST_PORT))
            .await
            .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
            .map_err(|err| format!("直接连接测试站点失败: {err}").into()),
    }
}

/// 一次性连接（不进连接池——池里的旧连接不能代表新草稿），认证完即 drop。
/// 跳板路径例外：跳板本身的会话仍走池（它不承载草稿，复用是安全的）。
async fn test_connect(
    destination: &SshDestination,
    request: &SshTestRequest,
    proxy: Option<&crate::ssh_proxy::ProxyLink>,
) -> Result<(), SessionError> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: None,
        keepalive_max: 3,
        ..Default::default()
    });
    let handler = ClientHandler { host: destination.host.clone(), port: destination.port };
    let mut session = open_transport(proxy, config, destination, handler, 0).await?;
    test_authenticate(&mut session, destination, request).await
}

/// 无人值守版认证：none → 草稿密码 → 密钥/已存密码计划。keyboard-interactive
/// 与「连接时询问」在这里跳过——测试不能弹框，也不能把交互失败误报成配置错。
async fn test_authenticate(
    session: &mut ClientSession,
    destination: &SshDestination,
    request: &SshTestRequest,
) -> Result<(), SessionError> {
    if session.authenticate_none(&destination.user).await?.success() {
        return Ok(());
    }
    let has_draft_password =
        request.password.as_deref().is_some_and(|password| !password.is_empty());
    if let Some(password) = request.password.as_deref().filter(|p| !p.is_empty()) {
        if authenticate_password(session, &destination.user, password.as_bytes()).await? {
            return Ok(());
        }
    }
    let plan =
        authentication_plan(request.auth, &request.private_keys, &destination.identity_files);
    let mut interactive_skipped = false;
    let mut stored_password = None;
    let mut loaded_stored_password = false;
    let mut local_key_errors = Vec::new();
    for method in plan {
        match method {
            AuthMethod::PrivateKey(path) => {
                // allow_prompt=false：spawn_test 承诺绝不弹框，密钥口令也
                // 不例外——受口令保护且无已存口令的密钥记为本地问题。
                if try_private_key(session, destination, &path, false, &mut local_key_errors)
                    .await?
                {
                    clear_secret(&mut stored_password);
                    return Ok(());
                }
            },
            AuthMethod::StoredPassword => {
                // A non-empty draft is the user's explicit answer for this
                // test. Do not let an older credential-manager value turn a
                // wrong draft into a misleading success.
                if has_draft_password {
                    continue;
                }
                if !loaded_stored_password {
                    stored_password =
                        crate::ssh_credentials::load_stored_password(&destination.original)?;
                    loaded_stored_password = true;
                }
                if let Some(password) = stored_password.as_deref() {
                    if authenticate_password(session, &destination.user, password).await? {
                        clear_secret(&mut stored_password);
                        return Ok(());
                    }
                }
            },
            AuthMethod::KeyboardInteractive | AuthMethod::PromptPassword => {
                interactive_skipped = true;
            },
        }
    }
    clear_secret(&mut stored_password);
    if !local_key_errors.is_empty() {
        return Err(format!("私钥无法使用：{}", local_key_errors.join("；")).into());
    }
    if interactive_skipped {
        return Err("服务器可达，但此配置需要连接时交互输入（密码/MFA），测试无法替你完成".into());
    }
    Err("认证未通过：请检查密码、私钥或服务器端授权".into())
}

fn auth_failure(
    mode: crate::ssh_profiles::SshAuthMode,
    key_count: usize,
    local_key_errors: &[String],
) -> String {
    use crate::ssh_profiles::SshAuthMode;
    // 纯密钥模式下所有私钥都倒在本地时，服务器根本没见过密钥——报
    // 「服务器拒绝」是误导，直接报本地原因。
    if mode == SshAuthMode::PublicKey
        && !local_key_errors.is_empty()
        && local_key_errors.len() >= key_count
    {
        return format!("私钥无法使用：{}", local_key_errors.join("；"));
    }
    let message = match mode {
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
    };
    if local_key_errors.is_empty() {
        message
    } else {
        format!("{message}（本地密钥问题：{}）", local_key_errors.join("；"))
    }
}

/// 判定私钥解析失败是否因为「密钥受口令保护」。russh 对 OpenSSH 加密容器和
/// PKCS#1 传统加密（DEK-Info）报 `KeyIsEncrypted`；PKCS#8 加密无口令时报的
/// 是 ASN.1 解码错误，只能靠 PEM 头识别。其余失败（格式不支持/文件损坏）
/// 绝不能当成「缺口令」——那会弹一个永远解不开的口令框。
fn key_needs_passphrase(err: &russh::keys::Error, pem: &[u8]) -> bool {
    const PKCS8_ENCRYPTED: &[u8] = b"-----BEGIN ENCRYPTED PRIVATE KEY-----";
    matches!(err, russh::keys::Error::KeyIsEncrypted)
        || pem.windows(PKCS8_ENCRYPTED.len()).any(|window| window == PKCS8_ENCRYPTED)
}

/// 用 `path` 的私钥认证一轮。密钥本地不可用（读取/解析/口令问题）返回
/// `Ok(false)` 让认证计划继续，同时把原因记入 `local_errors`——最终报错
/// 必须能区分「服务器拒绝了密钥」和「密钥根本没送出去」。
/// `allow_prompt=false`（测试连接）时绝不弹口令框。
async fn try_private_key(
    session: &mut ClientSession,
    destination: &SshDestination,
    path: &Path,
    allow_prompt: bool,
    local_errors: &mut Vec<String>,
) -> Result<bool, SessionError> {
    let private_key = match std::fs::read(path) {
        Ok(private_key) => private_key,
        Err(err) => {
            warn!("无法读取 SSH 私钥 {}: {err}", path.display());
            local_errors.push(format!("{}: 无法读取（{err}）", path.display()));
            return Ok(false);
        },
    };
    // 无口令解析优先：绝大多数密钥（含云厂商 .pem）在这里直接成功，全程
    // 零交互。只有确认密钥受口令保护才进入口令流程。
    let mut key = match russh::keys::load_secret_key(path, None) {
        Ok(key) => Some(key),
        Err(err) if key_needs_passphrase(&err, &private_key) => None,
        Err(err) => {
            warn!("SSH 私钥 {} 无法解析: {err}", path.display());
            local_errors.push(format!("{}: 无法解析（{err}）", path.display()));
            return Ok(false);
        },
    };

    if key.is_none() {
        let mut stored = crate::ssh_credentials::load_private_key_passphrase(&private_key)?;
        key = stored
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|passphrase| russh::keys::load_secret_key(path, Some(passphrase)).ok());
        if key.is_none() {
            crate::ssh_credentials::forget_private_key_passphrase(&private_key)?;
        }
        clear_secret(&mut stored);
    }

    if key.is_none() {
        if !allow_prompt {
            local_errors.push(format!("{}: 私钥受口令保护，测试无法替你输入口令", path.display()));
            return Ok(false);
        }
        let prompt = format!("密钥口令: {}", path.display());
        if let Some((mut passphrase, save)) = prompt_secret(prompt, None, true).await? {
            let text = String::from_utf8_lossy(&passphrase).into_owned();
            key = russh::keys::load_secret_key(path, Some(&text)).ok();
            if key.is_some() && save {
                crate::ssh_credentials::store_private_key_passphrase(&private_key, &passphrase)?;
            }
            passphrase.fill(0);
        }
        if key.is_none() {
            local_errors.push(format!("{}: 密钥口令不正确或已取消", path.display()));
        }
    }
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

fn render_error<H: SshEventHost>(
    terminal: &Arc<FairMutex<Term<H>>>,
    event_proxy: &H,
    message: &str,
) {
    let mut stream = StreamProcessor::default();
    let text = format!("\r\n\x1b[31m{message}\x1b[0m\r\n");
    stream.feed(&mut *terminal.lock(), event_proxy, text.as_bytes());
    event_proxy.send_event(TerminalEvent::Wakeup);
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
    use super::{
        AuthMethod, SshDestination, authentication_plan, parse_resolved_config,
        resolve_network_proxy,
    };
    use crate::ssh_profiles::SshAuthMode;
    use crate::ssh_proxy::{ProxyLink, ProxyMode, SshProxyConfig};
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
    fn ssh_runtime_uses_global_network_proxy_and_keeps_openssh_proxy_jump() {
        let global = SshProxyConfig {
            mode: ProxyMode::Custom,
            url: "socks5://127.0.0.1:7890".to_owned(),
            no_proxy: Vec::new(),
        };
        let direct_target = SshDestination::parse("root@example.com").unwrap();
        assert!(matches!(
            resolve_network_proxy(&global, &direct_target).unwrap(),
            Some(ProxyLink::Server(_))
        ));

        let mut jump_target = direct_target;
        jump_target.proxy_jump = Some("bastion.internal".to_owned());
        assert_eq!(
            resolve_network_proxy(&global, &jump_target).unwrap(),
            Some(ProxyLink::Jump("bastion.internal".to_owned()))
        );
    }

    #[test]
    fn auto_auth_plan_keeps_key_order_and_deduplicates_keys() {
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

    /// 测试专用密钥（ssh-keygen 现场生成，从未用于任何真实主机）。
    /// PKCS#1 格式（`BEGIN RSA PRIVATE KEY`）就是云厂商控制台下载的经典
    /// .pem——它必须在无口令、零交互下直接解析成功。
    const TEST_RSA_PKCS1_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEoQIBAAKCAQEAt9Kvur74e4WQha50+U5XjU/eksBFrf3K6Q0r6sQ0nxUz3nJW
EVKGnxpcBu0tjMBCH/4+PmoKefrs9XweUlbQeLCJD/RXWaR7ETafuFif6n2ZdeHh
sqbFoPKOAJNOt3+p7x+XEp8pKjJ+YTJNQ7qGSjqywUsEU/kXVn2ntQDaaaMnF7kt
AYTFAQDtzntbcmy/N1LloOYio/Wi70ZCsB/MBuKe+mCYr5dq9VImrOuUEGDIER9y
48ZE+PpF9TkU+5NNYaVzObuw3M1GN0Tu5FoaKaIjLcAqEfyCRf5hfA3Z+i0S9HIc
XmgN7RLumqJb5sHREIAoJfgHtc8N3BVvzhoiWwIDAQABAoIBAETiLBzYPEgZXHdj
0QytSUy4fcjTSSkyngtn9qmSXb+xU88LXGpAWRcc6xhjX3rLftv7S3rbBNMB7zLs
kHY9dwCK8smqP+NlKgLgy8hqWX6nE08j1o46RXuS+RiJGunTaqwjU9rUDrpz0nz8
uwxixLjjNyIMyPHouVCdZK+EwtPrf3aEshezbqs7qoN/7ULwYvAKxa44fn3sEQPY
MY9QhdpkaybT7pib3tYWmRjEJNvIbnT01IOcUfrcWcY1tOekLW/Y3TPd3bTJXp6O
GLQCXIgcnPNK4xOSPx8N0kNPjgpAeB3TewVqfllzoeCdEw7ycdKDFHqaAwbIpsqb
EpsJ3Y0CgYEA3ex1L6i+sHdU/0Qn4kBjTNa3Uw+Mk2SDZJO2Uykgj2Kbh6O8mJaP
+ZGTm4IRv/AcfpkZ6hxrk5Ndv/nk9LwwYpHKb0TgkFzD2UD9C7vQPLXesf3whr3i
KQvGxLtGR8qzOnwdl7YjY2hCVgzpulF81pq059kcPBxpPVuYGklEjx8CgYEA1AyJ
9yPrC5+bCXggUIGk5bXSi/7WYsTlFMrjrGFm1GmA/HEUmNNtgs8Hm/5Hun50rvwo
20vjS2Pd/Y5MTS6mUDjxXpDCB7sdU4HXa00TBQML141jzAT3D51Uf9coItpc+OQe
kzszaVUT8Uk9nAgzQS13qRYkgy4TG+Q2ireRkUUCgYArZsAwVu8cMepUle66597D
u0ZVHzhd5w1vURgaQXPVtvI138bVjLSRmW/lvNVd1UatV6Hi0DYVwX9XOTcWyeso
i9ysUCse8JV42qXicpOyG9t2sfQlVeNyJZR1Cy8egTz2FinvbraTDWPT0mivgJpK
mi0BHsvP0bqfPleL5IJc/wJ/M1rWDwSj6Cy/X4u4R8ceKIPgegc95K3KzT5V5Wmx
fcAPfRPl6R1LaGK7dQwgUwpNOBPZ0UKPybJmEQJleEvT+5nO2xgz5atrbs4DXflM
oeoa9BlKEh8htqZj0JJLJiW8Xorg3Md5rAjuy4DxatiRkTdxw4GZVivSdO7QRsgu
eQKBgQCxrxYM7PmWMc8Kd1bs5oi3DoJSphWe5BVGaa83BKgNDossq3cJWi+IAz4B
3YQjwa20xeLJ9E/Gg8dmzUANSqu9h1npnq2oGL/q7HNAkzzol4n7wLltbALuQk8f
dl+RK/+C/B4FvPhU0VmBustY8wIK7Ag0/hZzGsXvuefRU26d/Q==
-----END RSA PRIVATE KEY-----
";

    /// 测试专用：口令为 `test-passphrase` 的 OpenSSH 加密 ed25519。
    const TEST_ENCRYPTED_OPENSSH_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABBtaqDtAS
iEydl7ufOxfloWAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAINGK341X7zN9RTlt
hvn9NiZD8t69+cieB7ZTqiWV+p+VAAAAoCwhYq9GyCpiJ+ZtgOdAMi0w7EM9CS7p3ClSWs
yWhRmWiblcWRIDB7EUi4Pl1Rkjf8LAr1PcVqFB5RR9SLtgByaZmWeOx29h3mcqDtjkD27M
X1l3VqjJ/4jpeFutSPjJNztG+wNGlALsGkFxLBZ8hk4u86lVdoBjLEOivCvo1Qmd1XT74t
AAhYTvsZWAkp6JTIprUWd7YdQJ7HfVl+fRqto=
-----END OPENSSH PRIVATE KEY-----
";

    #[test]
    fn rsa_pkcs1_pem_parses_without_passphrase() {
        let key = russh::keys::decode_secret_key(TEST_RSA_PKCS1_PEM, None)
            .expect("PKCS#1 RSA .pem 必须无口令直接解析（russh 需要 rsa feature）");
        assert!(key.algorithm().is_rsa());
    }

    #[test]
    fn rsa_pkcs1_pem_with_passphrase_also_parses() {
        // 用户把无口令密钥误存了口令时不该解析失败：无加密的 PEM 忽略口令。
        let key = russh::keys::decode_secret_key(TEST_RSA_PKCS1_PEM, Some("whatever"))
            .expect("无加密 PEM 携带多余口令也应解析");
        assert!(key.algorithm().is_rsa());
    }

    #[test]
    fn encrypted_openssh_key_is_classified_as_needing_passphrase() {
        let err = russh::keys::decode_secret_key(TEST_ENCRYPTED_OPENSSH_PEM, None)
            .expect_err("加密密钥无口令解析必须失败");
        assert!(super::key_needs_passphrase(&err, TEST_ENCRYPTED_OPENSSH_PEM.as_bytes()));
        // 口令正确则解开——证明失败确实只是缺口令。
        russh::keys::decode_secret_key(TEST_ENCRYPTED_OPENSSH_PEM, Some("test-passphrase"))
            .expect("口令正确必须解开");
    }

    #[test]
    fn pkcs8_encrypted_banner_is_classified_as_needing_passphrase() {
        let pem =
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n";
        let err = russh::keys::decode_secret_key(pem, None).expect_err("占位密文必须解析失败");
        assert!(super::key_needs_passphrase(&err, pem.as_bytes()));
    }

    #[test]
    fn unparseable_key_is_not_classified_as_needing_passphrase() {
        // 格式不支持/损坏绝不能进口令流程——那正是「无口令 .pem 弹出系统
        // 口令框」事故的形态。
        let garbage = "this is not a private key";
        let err = russh::keys::decode_secret_key(garbage, None).expect_err("垃圾输入必须解析失败");
        assert!(!super::key_needs_passphrase(&err, garbage.as_bytes()));
    }

    #[test]
    fn all_keys_failing_locally_is_not_reported_as_server_rejection() {
        let errors = vec!["C:\\keys\\a.pem: 无法解析（unsupported）".to_owned()];
        let message = super::auth_failure(SshAuthMode::PublicKey, 1, &errors);
        assert!(message.starts_with("私钥无法使用"), "实际文案: {message}");
        assert!(!message.contains("服务器拒绝"), "实际文案: {message}");

        // 有密钥真的送到了服务器（本地失败数 < 密钥数）时保留原判词。
        let partial = super::auth_failure(SshAuthMode::PublicKey, 2, &errors);
        assert!(partial.contains("服务器拒绝"), "实际文案: {partial}");
        assert!(partial.contains("本地密钥问题"), "实际文案: {partial}");
    }
}
