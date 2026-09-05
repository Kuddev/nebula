//! 由 SSH 通道直接驱动的远端终端会话。
//!
//! 远端 Pane 不创建本地伪终端，但继续使用统一的输入、缩放和关闭消息协议，
//! 从而让渲染与键盘处理保持传输层无关。

use std::borrow::Cow;
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

#[cfg(feature = "legacy-shell")]
use crate::event::EventProxy;
use crate::proxy_test::{ProxyTestFailure, ProxyTestOutcome, ProxyTestResult, ProxyTestRoute};

mod route;
use route::{ResolvedRoute, RouteTransport};

type SessionError = Box<dyn std::error::Error + Send + Sync>;
type ClientSession = client::Handle<ClientHandler>;
type SharedSession = Arc<tokio::sync::Mutex<ClientSession>>;

struct AcquiredSession {
    key: String,
    session: SharedSession,
    reused: bool,
    jump_sessions: Vec<SharedSession>,
}

struct OpenedTransport {
    session: ClientSession,
    jump_sessions: Vec<SharedSession>,
}

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

#[cfg(feature = "legacy-shell")]
impl SshEventHost for EventProxy {
    fn ssh_stage(&self, stage: SshStage) {
        // [`EventProxy`] 自带 `tab_id`，后台 runtime 不需要知道 pane id
        // 或 window id（固有 `send_event(EventType)`，非 trait 方法）。
        self.send_event(crate::event::EventType::SshConnect(stage));
    }
}

#[derive(Clone)]
struct NoopSshEventHost;

impl nebula_terminal::event::EventListener for NoopSshEventHost {}
impl SshEventHost for NoopSshEventHost {}

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

#[derive(Debug, Default)]
struct KeyboardInteractiveAttempt {
    success: bool,
    prompted: bool,
    prompt_required: bool,
    used_password: bool,
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
            // 一些 sshd/PAM 只公布 keyboard-interactive，即使提示本质上仍是
            // 登录密码。已输入的密码必须先复用到这个单提示流程，不能直接再
            // 弹一次系统凭据窗口。
            vec![
                AuthMethod::StoredPassword,
                AuthMethod::KeyboardInteractive,
                AuthMethod::PromptPassword,
            ]
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
        crate::ssh_profiles::validate_ssh_destination(&original)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        if original.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH 地址为空"));
        }

        // OpenSSH 不把裸 `user@host:port` 识别为端口语法，而会原样输出成
        // `hostname host:port`。后续把它交给 socket 就会在 Windows 上得到
        // WSAHOST_NOT_FOUND。只规范化配置探测参数，保留 original 作为列表、
        // Profile 与凭据的稳定身份。
        let config_target = ssh_config_probe_target(&original);
        let output = ssh_config_output(config_target.as_ref());
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

/// 给 `ssh -G` 的离线配置探测目标。Nebula 历史存盘格式允许
/// `user@host:port`，而 OpenSSH 只会从 `ssh://user@host:port` URI 中拆出
/// 端口；无显式端口、已有 URI 与裸 IPv6 都保持原样，避免改变 Host 匹配。
fn ssh_config_probe_target(value: &str) -> Cow<'_, str> {
    if value.starts_with("ssh://") {
        return Cow::Borrowed(value);
    }
    let host_port = value.rsplit_once('@').map_or(value, |(_, host_port)| host_port);
    let has_explicit_port = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split_once(']')
            .and_then(|(_, suffix)| suffix.strip_prefix(':'))
            .is_some_and(|port| port.parse::<u16>().is_ok())
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        !host.is_empty() && !host.contains(':') && port.parse::<u16>().is_ok()
    } else {
        false
    };

    if has_explicit_port { Cow::Owned(format!("ssh://{value}")) } else { Cow::Borrowed(value) }
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

/// Run OpenSSH's config-only probe without attaching a console window.
///
/// Nebula is a Windows GUI process (`windows_subsystem = "windows"`). Starting
/// `ssh.exe -G` without `CREATE_NO_WINDOW` briefly creates a conhost window;
/// SFTP used to run this probe on every directory change, which made the black
/// flash particularly visible. Other platforms keep the ordinary process
/// flags.
fn ssh_config_output(target: &str) -> io::Result<std::process::Output> {
    let mut command = Command::new(find_ssh());
    command.arg("-G").arg("--").arg(target);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command.output()
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
    allow_prompt: bool,
    #[cfg(test)]
    known_hosts_path: Option<PathBuf>,
}

impl ClientHandler {
    fn verify_host_key(&self, key: &ssh_key::PublicKey) -> Result<bool, russh::keys::Error> {
        #[cfg(test)]
        if let Some(path) = self.known_hosts_path.as_deref() {
            return russh::keys::known_hosts::check_known_hosts_path(
                &self.host, self.port, key, path,
            );
        }
        russh::keys::known_hosts::check_known_hosts(&self.host, self.port, key)
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match self.verify_host_key(server_public_key) {
            Ok(true) => Ok(true),
            Ok(false) => {
                if self.allow_prompt
                    && confirm_new_host(&self.host, self.port, server_public_key).await
                {
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
                if self.allow_prompt {
                    show_host_key_changed(&self.host, self.port, &err.to_string());
                }
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
    spawn_session_at(destination, None, initial_size, terminal, event_proxy)
}

/// Start an SSH pane and optionally move its fresh interactive shell to a
/// known remote working directory. The directory is sent only after the shell
/// channel is ready; it never participates in address parsing or authentication.
pub fn spawn_session_at<H: SshEventHost>(
    destination: String,
    initial_remote_cwd: Option<String>,
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
            let profiles = crate::ssh_profiles::SshProfiles::load(&profiles_path)?;
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
                    initial_remote_cwd,
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
    initial_remote_cwd: Option<String>,
) -> Result<(), SessionError> {
    let mut acquired =
        authenticated_session_at(&destination, &profile, Some(&event_proxy), 0).await?;
    report_stage(Some(&event_proxy), SshStage::OpenShell);
    let shell =
        open_shell_channel(&acquired.session, initial_size, initial_remote_cwd.as_deref()).await;
    let (mut channel, hook_token) = match shell {
        Ok(shell) => shell,
        Err(first_error) if acquired.reused => {
            // `Handle::is_closed` 只反映已完成的关闭；TCP reset 与 channel-open
            // 失败之间存在窗口。复用对象打不开首个 channel 时精确淘汰并只重试
            // 一次，新连接失败则把真实错误交给连接卡片。
            evict_pooled_session(&acquired.key, &acquired.session).await;
            info!("复用 SSH transport 打开 channel 失败，重建连接: {first_error}");
            acquired =
                authenticated_session_at(&destination, &profile, Some(&event_proxy), 0).await?;
            report_stage(Some(&event_proxy), SshStage::OpenShell);
            match open_shell_channel(&acquired.session, initial_size, initial_remote_cwd.as_deref())
                .await
            {
                Ok(shell) => shell,
                Err(retry_error) => {
                    evict_pooled_session(&acquired.key, &acquired.session).await;
                    return Err(format!(
                        "SSH channel 打开失败（复用连接错误: {first_error}；重建后错误: {retry_error}）"
                    )
                    .into());
                },
            }
        },
        Err(error) => {
            evict_pooled_session(&acquired.key, &acquired.session).await;
            return Err(error);
        },
    };
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
    let result: Result<(), SessionError> = async {
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
                    // 只改本地几何：远端保持旧尺寸，等去抖后的 `Resize` 才发
                    // `window_change`。交互式拖拽每帧都会来一条。
                    Some(Msg::ResizeGrid(size)) => stream.resize(size),
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
    .await;
    let transport_closed = acquired.session.lock().await.is_closed();
    if result.is_err() || transport_closed {
        evict_pooled_session(&acquired.key, &acquired.session).await;
    }
    result
}

async fn open_shell_channel(
    session: &SharedSession,
    initial_size: WindowSize,
    initial_remote_cwd: Option<&str>,
) -> Result<(russh::Channel<client::Msg>, String), SessionError> {
    let channel = {
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
    // 远端 pane 的自声明是安全护栏而非连接前提；AcceptEnv 未放行时忽略。
    let _ = channel.set_env(false, "NEBULA_PANE_REMOTE", "1").await;
    channel.request_shell(true).await?;
    if let Some(command) = initial_remote_cd_command(initial_remote_cwd) {
        channel.data_bytes(command).await?;
    }
    Ok((channel, hook_token))
}

/// Build one shell-safe `cd` command for a path learned from OSC 7 or SFTP.
/// Relative and control-character-bearing values are rejected: a duplicated
/// tab must never turn terminal metadata into an arbitrary command stream.
fn initial_remote_cd_command(path: Option<&str>) -> Option<Vec<u8>> {
    let path = path?.trim();
    if !path.starts_with('/') || path.len() > 16 * 1024 || path.chars().any(char::is_control) {
        return None;
    }
    let quoted = path.replace('\'', "'\\''");
    Some(format!("cd '{quoted}'\r").into_bytes())
}

async fn evict_pooled_session(key: &str, session: &SharedSession) -> bool {
    let mut pool = connection_pool().lock().await;
    let matches = pool.get(key).is_some_and(|pooled| Arc::ptr_eq(pooled, session));
    if matches {
        pool.remove(key);
    }
    matches
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
    Ok(authenticated_session_at(destination, profile, progress, 0).await?.session)
}

async fn authenticated_session_at<H: SshEventHost>(
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
    progress: Option<&H>,
    depth: u8,
) -> Result<AcquiredSession, SessionError> {
    let route = resolve_connection_route(destination, profile, depth, None).await?;
    authenticated_route(&route, progress, false).await
}

async fn resolve_connection_route(
    destination: &SshDestination,
    profile: &crate::ssh_profiles::SshProfileAuth,
    depth: u8,
    proxy_password: Option<String>,
) -> Result<ResolvedRoute, SessionError> {
    let destination = destination.clone();
    let profile = profile.clone();
    tokio::task::spawn_blocking(move || {
        let global = crate::ssh_proxy::SshProxyConfig::load_global();
        let path = crate::display::nebula_data_dir().join("ssh_profiles.json");
        let profiles = crate::ssh_profiles::SshProfiles::load(&path)
            .map_err(|err| format!("读取 SSH 主机配置失败: {err}"))?;
        route::resolve_route_with(
            destination,
            profile,
            &global,
            depth,
            proxy_password.as_deref(),
            &mut |spec| {
                let destination = SshDestination::resolve(spec)
                    .map_err(|err| format!("解析跳板地址失败: {err}"))?;
                Ok((destination, profiles.for_destination(spec)))
            },
            &mut |target| {
                crate::ssh_credentials::load_generic_secret(target)
                    .map_err(|err| format!("读取代理凭据失败，请重新保存代理密码: {err}"))
            },
        )
    })
    .await
    .map_err(|err| format!("解析 SSH 连接路径任务失败: {err}"))?
    .map_err(Into::into)
}

async fn authenticated_route<H: SshEventHost>(
    route: &ResolvedRoute,
    progress: Option<&H>,
    unattended: bool,
) -> Result<AcquiredSession, SessionError> {
    let key = route.pool_key();
    let existing = if unattended {
        None
    } else {
        connection_pool().lock().await.get(&key).cloned()
    };
    if let Some(existing) = existing {
        if !existing.lock().await.is_closed() {
            info!("复用已认证 SSH 连接: {key}");
            return Ok(AcquiredSession {
                key,
                session: existing,
                reused: true,
                jump_sessions: Vec::new(),
            });
        }
        connection_pool().lock().await.remove(&key);
    }

    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });
    report_stage(progress, SshStage::Connect);
    let mut transport = open_transport(route, config, unattended).await?;
    report_stage(progress, SshStage::Authenticate);
    if unattended {
        let request = SshTestRequest {
            request_id: 0,
            destination: route.destination.original.clone(),
            auth: route.profile.auth,
            private_keys: route.profile.private_keys.clone(),
            password: None,
            connection: route.profile.connection.clone(),
            proxy_password: None,
        };
        test_authenticate(&mut transport.session, &route.destination, &request).await?;
    } else {
        authenticate(&mut transport.session, &route.destination, &route.profile).await?;
    }

    let session = Arc::new(tokio::sync::Mutex::new(transport.session));
    if unattended {
        return Ok(AcquiredSession {
            key,
            session,
            reused: false,
            jump_sessions: transport.jump_sessions,
        });
    }
    let mut pool = connection_pool().lock().await;
    if let Some(existing) = pool.get(&key).cloned() {
        if !existing.lock().await.is_closed() {
            return Ok(AcquiredSession {
                key,
                session: existing,
                reused: true,
                jump_sessions: Vec::new(),
            });
        }
    }
    pool.insert(key.clone(), session.clone());
    Ok(AcquiredSession { key, session, reused: false, jump_sessions: transport.jump_sessions })
}

#[cfg(test)]
fn resolve_network_proxy(
    global: &crate::ssh_proxy::SshProxyConfig,
    destination: &SshDestination,
) -> Result<Option<crate::ssh_proxy::ProxyLink>, String> {
    global.resolve(destination.proxy_jump.as_deref(), &destination.host)
}

async fn open_transport(
    route: &ResolvedRoute,
    config: Arc<client::Config>,
    unattended: bool,
) -> Result<OpenedTransport, SessionError> {
    let destination = &route.destination;
    let handler = ClientHandler {
        host: destination.host.clone(),
        port: destination.port,
        allow_prompt: !unattended,
        #[cfg(test)]
        known_hosts_path: route.known_hosts_path.clone(),
    };
    let mut jump_sessions = Vec::new();
    let session = match &route.transport {
        RouteTransport::Server(server) => {
            info!("经代理 {} 连接 {}:{}", server.display(), destination.host, destination.port);
            let stream = crate::ssh_proxy::connect(server, &destination.host, destination.port)
                .await
                .map_err(|err| format!("经代理 {} 连接失败: {err}", server.display()))?;
            client::connect_stream(config, stream, handler).await?
        },
        RouteTransport::Jump(jump) => {
            let spec = &jump.destination.original;
            info!("经跳板 {spec} 连接 {}:{}", destination.host, destination.port);
            let acquired = Box::pin(authenticated_route(
                jump,
                None::<&NoopSshEventHost>,
                unattended,
            ))
            .await
            .map_err(|err| format!("连接跳板 {spec} 失败: {err}"))?;
            let channel = {
                let session = acquired.session.lock().await;
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
            jump_sessions = acquired.jump_sessions;
            jump_sessions.push(acquired.session);
            client::connect_stream(config, channel.into_stream(), handler).await?
        },
        RouteTransport::Command(command) => {
            info!("经自定义命令连接 {}:{}", destination.host, destination.port);
            let stream =
                crate::ssh_proxy::connect_command(command, &destination.host, destination.port)
                    .await
                    .map_err(|err| format!("自定义代理命令启动失败: {err}"))?;
            client::connect_stream(config, stream, handler).await?
        },
        RouteTransport::Direct => {
            client::connect(config, (destination.host.as_str(), destination.port), handler)
                .await?
        },
    };
    Ok(OpenedTransport { session, jump_sessions })
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
    let mut interactive_prompt_was_shown = false;
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
                        crate::ssh_credentials::load_stored_password(&destination.original)
                            .unwrap_or_else(|error| {
                                warn!("Could not read saved SSH password; prompting instead: {error}");
                                None
                            });
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
                let attempt = try_keyboard_interactive(
                    session,
                    destination,
                    reusable_password.as_deref(),
                    true,
                )
                .await?;
                interactive_prompt_was_shown |= attempt.prompted;
                if attempt.success {
                    clear_secret(&mut reusable_password);
                    return Ok(());
                }
            },
            AuthMethod::PromptPassword => {
                // 同一轮里已经用过保存密码，或 keyboard-interactive 已经向用户
                // 问过一次，就直接报告失败。再次弹相同密码框只会制造重复认证
                // 尝试，严重时还会触发服务端 MaxAuthTries/限流。
                if stored_password_was_present || interactive_prompt_was_shown {
                    continue;
                }
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
        if let Err(error) = crate::ssh_credentials::forget_password(&destination.original) {
            warn!("Could not remove rejected SSH password: {error}");
        }
    }
    clear_secret(&mut reusable_password);
    Err(auth_failure(profile.auth, key_count, &local_key_errors).into())
}

/// 在目标主机上跑一条命令并收集它的标准输出，脚本经标准输入送入。
///
/// 走连接池里已认证的传输，所以不会触发第二次认证或 MFA；开的是独立 exec
/// 通道，交互终端里**不会**出现任何回显——用户看不到我们在后台问了什么。
///
/// `script` 作为被执行程序的标准输入。这样命令行里就不必嵌任何引号，脚本
/// 含 `'`、`$` 都无需转义（拼命令行做两层转义，错一个字符就是难查的静默
/// 失败）。
///
/// `budget` 到点即放弃：远端可能因为负载或磁盘卡住不回话，而调用方是 UI，
/// 不能无限等。超时返回错误而不是空字符串——"问不到"和"答案是空"必须分开。
pub(crate) async fn exec_capture(
    raw_destination: &str,
    command: &str,
    script: &[u8],
    budget: Duration,
) -> Result<String, SessionError> {
    let profiles_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
    let raw = raw_destination.to_owned();
    let (destination, profile) = tokio::task::spawn_blocking(move || {
        let destination = SshDestination::resolve(&raw)?;
        let profiles = crate::ssh_profiles::SshProfiles::load(&profiles_path)
            .unwrap_or_else(|_| crate::ssh_profiles::SshProfiles::default());
        Ok::<_, io::Error>((destination, profiles.for_destination(&raw)))
    })
    .await
    .map_err(|err| format!("SSH 地址解析任务失败: {err}"))??;

    let session = authenticated_session(&destination, &profile, None::<&NoopSshEventHost>).await?;
    let mut channel = {
        let session = session.lock().await;
        session.channel_open_session().await?
    };
    channel.exec(true, command).await?;
    if !script.is_empty() {
        channel.data_bytes(script.to_vec()).await?;
        // 不发 EOF 的话远端 `sh` 会一直等更多输入，命令永远不结束。
        channel.eof().await?;
    }

    let collect = async {
        let mut stdout = Vec::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                // 标准错误只当诊断线索，不混进结果——远端的 `ps: not found`
                // 之类抱怨不该被当成路径。
                ChannelMsg::ExtendedData { data, .. } => {
                    if let Ok(text) = std::str::from_utf8(&data) {
                        let text = text.trim();
                        if !text.is_empty() {
                            log::debug!("远端命令 stderr（{raw_destination}）: {text}");
                        }
                    }
                },
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {},
            }
        }
        stdout
    };

    match tokio::time::timeout(budget, collect).await {
        // 远端文件名和路径未必是合法 UTF-8。有损转换让"大部分能读"胜过
        // "整次探测失败"；真正需要字节精度的路径操作走 SFTP，不走这里。
        Ok(stdout) => Ok(String::from_utf8_lossy(&stdout).into_owned()),
        Err(_) => Err(format!("远端命令超过 {} 秒未返回", budget.as_secs()).into()),
    }
}

/// 在现有认证连接上打开独立 SFTP 子系统；连接池和认证策略仍只有一份。
pub(crate) async fn open_sftp(
    raw_destination: &str,
) -> Result<russh_sftp::client::SftpSession, SessionError> {
    let profiles_path = crate::display::nebula_data_dir().join("ssh_profiles.json");
    let raw = raw_destination.to_owned();
    let (destination, profile) = tokio::task::spawn_blocking(move || {
        let destination = SshDestination::resolve(&raw)?;
        let profiles = crate::ssh_profiles::SshProfiles::load(&profiles_path)?;
        Ok::<_, io::Error>((destination, profiles.for_destination(&raw)))
    })
    .await
    .map_err(|err| format!("SSH 地址解析任务失败: {err}"))??;

    if let Some(proxy_jump) = destination.proxy_jump.as_deref() {
        info!("SFTP 将经跳板 {proxy_jump} 建立");
    }

    // SFTP 面板自己有加载态，不参与终端 pane 的连接卡片。
    let session = authenticated_session(&destination, &profile, None::<&NoopSshEventHost>).await?;
    let channel = {
        let session = session.lock().await;
        session.channel_open_session().await?
    };
    channel.request_subsystem(true, "sftp").await?;
    // 显式给参数而不是用上游默认值：默认单包 256 KiB 会让每个 READ/WRITE 都
    // 顶满一个巨型请求（部分服务端在这个尺寸上会静默出错），而默认在途写
    // 请求只有 8 个，高时延链路上填不满管道。判据见 `ssh_sftp::limits`。
    Ok(russh_sftp::client::SftpSession::new_with_config(
        channel.into_stream(),
        crate::ssh_sftp::limits::session_config(),
    )
    .await?)
}

// ---- 「测试连接」（SSH 编辑器页脚，spec ui-redesign 稿一） ----

/// 编辑器草稿的连通性测试请求。带草稿密码/密钥而不是磁盘 profile——
/// 测试要回答「保存后能不能连上」，不是「上次保存的配置行不行」。
#[derive(Clone)]
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
    pub connection: crate::ssh_profiles::SshConnectionOptions,
    pub proxy_password: Option<String>,
}

impl std::fmt::Debug for SshTestRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshTestRequest")
            .field("request_id", &self.request_id)
            .field("destination", &self.destination)
            .field("auth", &self.auth)
            .field("private_keys", &self.private_keys)
            .field("connection", &self.connection)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .field("proxy_password", &self.proxy_password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
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

/// 执行一次无人值守的草稿测试。绝不弹 AskPass：草稿/已存密码可以回答明确的
/// keyboard-interactive 密码问题，OTP 等真正需要用户参与的问题则留给正式连接
/// （见 [`test_authenticate`]）。
async fn run_test(request: SshTestRequest) -> SshTestResult {
    let started = std::time::Instant::now();
    let request_id = request.request_id;
    let raw = request.destination.clone();
    let outcome = tokio::time::timeout(TEST_TIMEOUT, async {
        let resolved = tokio::task::spawn_blocking({
            let raw = raw.clone();
            move || {
                SshDestination::resolve(&raw)
            }
        })
        .await
        .map_err(|err| -> SessionError {
            format!("SSH 地址解析任务失败: {err}").into()
        })??;
        let profile = crate::ssh_profiles::SshProfileAuth {
            destination: request.destination.clone(),
            auth: request.auth,
            private_keys: request.private_keys.clone(),
            label: None,
            icon: None,
            connection: request.connection.clone(),
        };
        let route = resolve_connection_route(
            &resolved,
            &profile,
            0,
            request.proxy_password.clone(),
        )
        .await?;
        test_connect(&route, &request).await
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
#[cfg(feature = "legacy-shell")]
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

struct RetainedNetworkStream {
    stream: Box<dyn NetworkTestStream>,
    _jump_sessions: Vec<SharedSession>,
}

impl AsyncRead for RetainedNetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut()).poll_read(context, buffer)
    }
}

impl AsyncWrite for RetainedNetworkStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(self.stream.as_mut()).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut()).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(self.stream.as_mut()).poll_shutdown(context)
    }
}

/// 一次出网测试的完成数据。旧 winit 壳经事件投递；GPUI 设置页走 oneshot，
/// 握手与 HTTP 探测仍是 [`proxy_test_once`] 这一条路径。
fn proxy_test_pair(
    outcome: Result<Result<ProxyTestRoute, ProxyTestFailure>, tokio::time::error::Elapsed>,
) -> ProxyTestOutcome {
    match outcome {
        Ok(Ok(route)) => ProxyTestOutcome::Success(route),
        Ok(Err(error)) => ProxyTestOutcome::Failed(error),
        Err(_) => {
            ProxyTestOutcome::Failed(ProxyTestFailure::Timeout { seconds: TEST_TIMEOUT.as_secs() })
        },
    }
}

async fn run_proxy_test(request_id: u64) -> ProxyTestResult {
    let started = std::time::Instant::now();
    let outcome = proxy_test_pair(tokio::time::timeout(TEST_TIMEOUT, proxy_test_once()).await);
    ProxyTestResult { request_id, outcome, elapsed_ms: started.elapsed().as_millis() as u64 }
}

/// 启动出网测试并交给调用方异步等待。GPUI 没有 winit `EventLoopProxy`，
/// 所以通过这个 receiver 把结果回写到设置页 Entity；握手逻辑仍在
/// [`proxy_test_once`]。
pub fn start_proxy_test(
    request_id: u64,
) -> io::Result<tokio::sync::oneshot::Receiver<ProxyTestResult>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    runtime()?.spawn(async move {
        let _ = sender.send(run_proxy_test(request_id).await);
    });
    Ok(receiver)
}

/// 使用与 SSH 新连接相同的全局配置解析和代理握手建立字节流，再请求一个
/// 真实 HTTP 页面。只探测代理端口并不能证明代理有出网能力，所以这里必须
/// 收到目标站点的 HTTP 状态行才算成功。
#[cfg(feature = "legacy-shell")]
pub fn spawn_proxy_test(
    request_id: u64,
    proxy: winit::event_loop::EventLoopProxy<crate::event::Event>,
    window_id: winit::window::WindowId,
) -> io::Result<()> {
    runtime()?.spawn(async move {
        let result = run_proxy_test(request_id).await;
        let _ = proxy.send_event(crate::event::Event::new(
            crate::event::EventType::ProxyTestDone {
                request_id: result.request_id,
                outcome: result.outcome,
                elapsed_ms: result.elapsed_ms,
            },
            window_id,
        ));
    });
    Ok(())
}

async fn proxy_test_once() -> Result<ProxyTestRoute, ProxyTestFailure> {
    let global = tokio::task::spawn_blocking(crate::ssh_proxy::SshProxyConfig::load_global)
        .await
        .map_err(|error| ProxyTestFailure::LoadSettings(error.to_string()))?;
    let link = global
        .resolve(None, NETWORK_TEST_HOST)
        .map_err(|error| ProxyTestFailure::InvalidSettings(error.to_string()))?;
    let route = match &link {
        Some(crate::ssh_proxy::ProxyLink::Server(server)) => {
            ProxyTestRoute::ProxyServer(server.display())
        },
        Some(crate::ssh_proxy::ProxyLink::Jump(target)) => ProxyTestRoute::SshJump(target.clone()),
        Some(crate::ssh_proxy::ProxyLink::Command(_)) => ProxyTestRoute::CustomCommand,
        None if global.mode == crate::ssh_proxy::ProxyMode::Custom => ProxyTestRoute::DirectAddress,
        None => ProxyTestRoute::Direct,
    };
    let mut stream = proxy_test_stream(link.as_ref()).await?;
    stream
        .write_all(
            b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\nUser-Agent: Nebula-Network-Test\r\n\r\n",
        )
        .await
        .map_err(|error| ProxyTestFailure::SendRequest(error.to_string()))?;
    stream.flush().await.map_err(|error| ProxyTestFailure::SendRequest(error.to_string()))?;

    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while response.len() < 2048 && !response.windows(2).any(|part| part == b"\r\n") {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| ProxyTestFailure::ReadResponse(error.to_string()))?;
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
        .ok_or_else(|| ProxyTestFailure::InvalidHttpStatusLine(status_line.to_owned()))?;
    if !(200..500).contains(&status) {
        return Err(ProxyTestFailure::HttpStatus { status });
    }
    Ok(route)
}

async fn proxy_test_stream(
    link: Option<&crate::ssh_proxy::ProxyLink>,
) -> Result<Box<dyn NetworkTestStream>, ProxyTestFailure> {
    use crate::ssh_proxy::ProxyLink;
    match link {
        Some(ProxyLink::Server(server)) => {
            crate::ssh_proxy::connect(server, NETWORK_TEST_HOST, NETWORK_TEST_PORT)
                .await
                .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
                .map_err(|error| ProxyTestFailure::ProxyServer {
                    server: server.display(),
                    error: error.to_string(),
                })
        },
        Some(ProxyLink::Command(command)) => {
            crate::ssh_proxy::connect_command(command, NETWORK_TEST_HOST, NETWORK_TEST_PORT)
                .await
                .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
                .map_err(|error| ProxyTestFailure::CustomCommand(error.to_string()))
        },
        Some(ProxyLink::Jump(spec)) => {
            let (jump_destination, jump_profile) = tokio::task::spawn_blocking({
                let spec = spec.clone();
                move || {
                    let destination = SshDestination::resolve(&spec).map_err(|error| {
                        ProxyTestFailure::JumpResolve {
                            target: spec.clone(),
                            error: error.to_string(),
                        }
                    })?;
                    let path = crate::display::nebula_data_dir().join("ssh_profiles.json");
                    let profile = crate::ssh_profiles::SshProfiles::load(&path)
                        .map_err(|error| ProxyTestFailure::JumpResolve {
                            target: spec.clone(),
                            error: format!("读取 SSH 主机配置失败: {error}"),
                        })?
                        .for_destination(&spec);
                    Ok::<_, ProxyTestFailure>((destination, profile))
                }
            })
            .await
            .map_err(|error| ProxyTestFailure::JumpTask(error.to_string()))??;
            let route = resolve_connection_route(&jump_destination, &jump_profile, 1, None)
                .await
                .map_err(|error| ProxyTestFailure::JumpConnect {
                    target: spec.clone(),
                    error: error.to_string(),
                })?;
            let jump = authenticated_route(&route, None::<&NoopSshEventHost>, true)
                .await
                .map_err(|error| ProxyTestFailure::JumpConnect {
                    target: spec.clone(),
                    error: error.to_string(),
                })?;
            let channel = {
                let session = jump.session.lock().await;
                session
                    .channel_open_direct_tcpip(
                        NETWORK_TEST_HOST,
                        u32::from(NETWORK_TEST_PORT),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|error| ProxyTestFailure::JumpChannel {
                        target: spec.clone(),
                        error: error.to_string(),
                    })?
            };
            let mut sessions = jump.jump_sessions;
            sessions.push(jump.session);
            Ok(Box::new(RetainedNetworkStream {
                stream: Box::new(channel.into_stream()),
                _jump_sessions: sessions,
            }))
        },
        None => tokio::net::TcpStream::connect((NETWORK_TEST_HOST, NETWORK_TEST_PORT))
            .await
            .map(|stream| Box::new(stream) as Box<dyn NetworkTestStream>)
            .map_err(|error| ProxyTestFailure::Direct(error.to_string())),
    }
}

async fn test_connect(
    route: &ResolvedRoute,
    request: &SshTestRequest,
) -> Result<(), SessionError> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: None,
        keepalive_max: 3,
        ..Default::default()
    });
    let mut transport = open_transport(route, config, true).await?;
    test_authenticate(&mut transport.session, &route.destination, request).await
}

/// 无人值守版认证：none → 草稿密码 → 密钥/已存密码计划。明确的
/// keyboard-interactive 密码问题可以复用已有密码；OTP 与「连接时询问」在这里
/// 跳过——测试不能弹框，也不能把真实的二次验证误报成配置错误。
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
    let mut credential_was_attempted = has_draft_password;
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
                    credential_was_attempted = true;
                    if authenticate_password(session, &destination.user, password).await? {
                        clear_secret(&mut stored_password);
                        return Ok(());
                    }
                }
            },
            AuthMethod::KeyboardInteractive => {
                let password = request
                    .password
                    .as_deref()
                    .filter(|password| !password.is_empty())
                    .map(str::as_bytes)
                    .or_else(|| stored_password.as_deref());
                let attempt =
                    try_keyboard_interactive(session, destination, password, false).await?;
                credential_was_attempted |= attempt.used_password;
                interactive_skipped |= attempt.prompt_required;
                if attempt.success {
                    clear_secret(&mut stored_password);
                    return Ok(());
                }
            },
            AuthMethod::PromptPassword => {
                // 已经拿草稿/存储密码试过时，继续弹框不会让这次“无人值守测试”
                // 更可信；直接报告认证失败。完全没有凭据时才说明正式连接需要询问。
                interactive_skipped |= !credential_was_attempted;
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
    const PKCS8_ENCRYPTED_PREFIX: &[u8] = b"-----BEGIN ENCRYPTED ";
    const PKCS8_ENCRYPTED_SUFFIX: &[u8] = b"PRIVATE KEY-----";
    matches!(err, russh::keys::Error::KeyIsEncrypted)
        || pem.split(|byte| *byte == b'\n').any(|line| {
            line.strip_suffix(b"\r").unwrap_or(line).strip_prefix(PKCS8_ENCRYPTED_PREFIX)
                == Some(PKCS8_ENCRYPTED_SUFFIX)
        })
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
        let mut stored = crate::ssh_credentials::load_private_key_passphrase(&private_key)
            .unwrap_or_else(|error| {
                warn!("Could not read saved SSH key passphrase; prompting instead: {error}");
                None
            });
        key = stored
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|passphrase| russh::keys::load_secret_key(path, Some(passphrase)).ok());
        if key.is_none() && stored.is_some() {
            if let Err(error) = crate::ssh_credentials::forget_private_key_passphrase(&private_key) {
                warn!("Could not remove rejected SSH key passphrase: {error}");
            }
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
            let text = zeroize::Zeroizing::new(String::from_utf8_lossy(&passphrase).into_owned());
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
    allow_prompt: bool,
) -> Result<KeyboardInteractiveAttempt, SessionError> {
    let mut attempt = KeyboardInteractiveAttempt::default();
    let mut state =
        session.authenticate_keyboard_interactive_start(&destination.user, None::<String>).await?;
    for _ in 0..8 {
        match state {
            KeyboardInteractiveAuthResponse::Success => {
                attempt.success = true;
                return Ok(attempt);
            },
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(attempt),
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                let mut responses = Vec::with_capacity(prompts.len());
                for prompt in prompts {
                    if !prompt.echo && is_password_prompt(&prompt.prompt) && password.is_some() {
                        attempt.used_password = true;
                        responses.push(String::from_utf8_lossy(password.unwrap()).into_owned());
                        continue;
                    }
                    if !allow_prompt {
                        attempt.prompt_required = true;
                        return Ok(attempt);
                    }
                    let label = format!(
                        "{} - {} {} {}",
                        destination.original, name, instructions, prompt.prompt
                    );
                    attempt.prompted = true;
                    let Some((mut response, _)) = prompt_secret(label, None, false).await? else {
                        return Ok(attempt);
                    };
                    responses.push(String::from_utf8_lossy(&response).into_owned());
                    response.fill(0);
                }
                state = session.authenticate_keyboard_interactive_respond(responses).await?;
            },
        }
    }
    Ok(attempt)
}

fn is_password_prompt(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    ["password", "passphrase", "密码", "口令"].iter().any(|marker| lower.contains(marker))
}

async fn prompt_secret(
    destination: String,
    initial: Option<Vec<u8>>,
    allow_save: bool,
) -> io::Result<Option<(zeroize::Zeroizing<Vec<u8>>, bool)>> {
    if crate::ssh_prompt::available() {
        let _initial = zeroize::Zeroizing::new(initial.unwrap_or_default());
        return crate::ssh_prompt::secret(
            destination, allow_save && crate::platform::credentials::can_store(),
        ).await;
    }
    tokio::task::spawn_blocking(move || {
        let initial = zeroize::Zeroizing::new(initial.unwrap_or_default());
        crate::ssh_credentials::prompt_password(&destination, Some(&initial), allow_save)
            .map(|response| response.map(|(value, save)| (zeroize::Zeroizing::new(value), save)))
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

async fn confirm_new_host(host: &str, port: u16, key: &ssh_key::PublicKey) -> bool {
    if crate::ssh_prompt::available() {
        return crate::ssh_prompt::confirm_host(
            host, port, key.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
        ).await.unwrap_or_else(|error| {
            warn!("SSH host confirmation failed: {error}");
            false
        });
    }
    confirm_new_host_legacy(host, port, key)
}

#[cfg(not(windows))]
fn confirm_new_host_legacy(host: &str, port: u16, _key: &ssh_key::PublicKey) -> bool {
    warn!("SSH host {host}:{port} is untrusted and no confirmation window is available");
    false
}

#[cfg(not(windows))]
fn show_host_key_changed(host: &str, port: u16, detail: &str) {
    log::error!("{host}:{port} 的主机密钥与已保存记录不一致，连接已终止: {detail}");
}

#[cfg(windows)]
fn confirm_new_host_legacy(host: &str, port: u16, key: &ssh_key::PublicKey) -> bool {
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
        AuthMethod, SshDestination, authentication_plan, initial_remote_cd_command,
        is_password_prompt, parse_resolved_config, resolve_network_proxy, ssh_config_probe_target,
    };
    use crate::ssh_profiles::SshAuthMode;
    use crate::ssh_proxy::{ProxyLink, ProxyMode, SshProxyConfig};
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding as RsaLineEnding};
    use std::path::PathBuf;
    use std::sync::LazyLock;
    use zeroize::Zeroizing;

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
    fn openssh_probe_normalizes_legacy_destinations_with_explicit_ports() {
        assert_eq!(
            ssh_config_probe_target("root@154.64.232.109:20222"),
            "ssh://root@154.64.232.109:20222"
        );
        assert_eq!(
            ssh_config_probe_target("root@[2001:db8::1]:20222"),
            "ssh://root@[2001:db8::1]:20222"
        );
        assert_eq!(
            ssh_config_probe_target("ssh://root@example.com:20222"),
            "ssh://root@example.com:20222"
        );
        assert_eq!(ssh_config_probe_target("root@example.com"), "root@example.com");
        assert_eq!(ssh_config_probe_target("root@2001:db8::1"), "root@2001:db8::1");
    }

    #[test]
    fn duplicated_ssh_tabs_quote_their_remote_working_directory() {
        assert_eq!(
            initial_remote_cd_command(Some("/srv/Team's App")),
            Some(b"cd '/srv/Team'\\''s App'\r".to_vec())
        );
        assert_eq!(initial_remote_cd_command(Some("relative/path")), None);
        assert_eq!(initial_remote_cd_command(Some("/srv/app\nwhoami")), None);
    }

    #[test]
    fn proxy_test_timeout_copy_matches_shared_budget() {
        // 353bc17 把这条文案搬进 i18n 目录，秒数改由 `ProxyTestFailure` 携带
        // （旧的 `proxy_test_timeout_message` 随之删除，这个测试的引用被漏下
        // 了）。判据不变：用户看到的文案必须和共享的超时预算是同一个数。
        let timeout = crate::proxy_test::ProxyTestOutcome::Failed(
            crate::proxy_test::ProxyTestFailure::Timeout { seconds: super::TEST_TIMEOUT.as_secs() },
        );
        assert_eq!(
            crate::display::UiLanguage::ZhCn.proxy_test_message(&timeout, 0),
            "网络测试超时（12 秒无响应）"
        );
        assert_eq!(super::TEST_TIMEOUT.as_secs(), 12);
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
    fn password_mode_supports_pam_without_falling_back_to_keys() {
        assert_eq!(
            authentication_plan(
                SshAuthMode::Password,
                &[PathBuf::from(r"C:\Keys\ignored")],
                &[PathBuf::from(r"C:\Keys\ignored-config")],
            ),
            vec![
                AuthMethod::StoredPassword,
                AuthMethod::KeyboardInteractive,
                AuthMethod::PromptPassword,
            ]
        );
    }

    #[test]
    fn keyboard_interactive_reuses_password_only_for_password_prompts() {
        for prompt in ["Password:", "Enter passphrase:", "密码：", "请输入口令："] {
            assert!(is_password_prompt(prompt), "应识别密码问题: {prompt}");
        }
        for prompt in ["Verification code:", "OTP:", "Token:", "Duo choice:"] {
            assert!(!is_password_prompt(prompt), "二次验证不得自动填登录密码: {prompt}");
        }
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

    // 私钥夹具只在测试进程内生成，避免仓库保存可被误用的静态密钥材料；
    // LazyLock 让两个解析场景复用同一次较慢的 RSA 素数生成。
    static TEST_RSA_PKCS1_PEM: LazyLock<Zeroizing<String>> = LazyLock::new(|| {
        let key =
            rsa::RsaPrivateKey::new(&mut rand::rng(), 1024).expect("测试用 RSA 密钥必须能生成");
        key.to_pkcs1_pem(RsaLineEnding::LF).expect("测试用 RSA 密钥必须能编码为 PKCS#1 PEM")
    });

    fn encrypted_openssh_pem(password: &str) -> Zeroizing<String> {
        let mut rng = rand::rng();
        let key = russh::keys::ssh_key::PrivateKey::random(
            &mut rng,
            russh::keys::ssh_key::Algorithm::Ed25519,
        )
        .expect("测试用 Ed25519 密钥必须能生成");
        key.encrypt(&mut rng, password)
            .expect("测试用 OpenSSH 密钥必须能加密")
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("测试用 OpenSSH 密钥必须能编码为 PEM")
    }

    #[test]
    fn rsa_pkcs1_pem_parses_without_passphrase() {
        let key = russh::keys::decode_secret_key(TEST_RSA_PKCS1_PEM.as_str(), None)
            .expect("PKCS#1 RSA .pem 必须无口令直接解析（russh 需要 rsa feature）");
        assert!(key.algorithm().is_rsa());
    }

    #[test]
    fn rsa_pkcs1_pem_with_passphrase_also_parses() {
        // 用户把无口令密钥误存了口令时不该解析失败：无加密的 PEM 忽略口令。
        let key = russh::keys::decode_secret_key(TEST_RSA_PKCS1_PEM.as_str(), Some("whatever"))
            .expect("无加密 PEM 携带多余口令也应解析");
        assert!(key.algorithm().is_rsa());
    }

    #[test]
    fn encrypted_openssh_key_is_classified_as_needing_passphrase() {
        let pem = encrypted_openssh_pem("test-passphrase");
        let err = russh::keys::decode_secret_key(pem.as_str(), None)
            .expect_err("加密密钥无口令解析必须失败");
        assert!(super::key_needs_passphrase(&err, pem.as_bytes()));
        // 口令正确则解开——证明失败确实只是缺口令。
        russh::keys::decode_secret_key(pem.as_str(), Some("test-passphrase"))
            .expect("口令正确必须解开");
    }

    #[test]
    fn pkcs8_encrypted_banner_is_classified_as_needing_passphrase() {
        let pem = [b"-----BEGIN ENCRYPTED ".as_slice(), b"PRIVATE KEY-----\nAAAA\n"].concat();
        let pem = String::from_utf8(pem).expect("测试 PEM 必须是 UTF-8");
        let err = russh::keys::decode_secret_key(&pem, None).expect_err("占位密文必须解析失败");
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
