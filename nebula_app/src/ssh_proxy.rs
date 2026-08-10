//! SSH 出站代理：配置解析 + SOCKS5（RFC 1928/1929）与 HTTP CONNECT 握手。
//!
//! 不引入代理 crate：握手完成后把裸 `TcpStream` 交给 russh 的
//! `client::connect_stream`。全局配置存 `nebula_settings.txt`
//! （`ssh_proxy_mode` / `ssh_proxy_url` / `ssh_proxy_no_proxy`）。跳板链路的建
//! 立在 `ssh_session::open_transport`——它需要完整的认证栈，本模块只负责把
//! 配置解析成 [`ProxyLink`]。
//!
//! 「跟随系统」在 Windows 上读取 Internet Settings 注册表（WinINET，Clash /
//! v2rayN 等代理软件写的就是它），不读取终端环境变量，避免不同启动方式
//! 让同一份软件设置产生不同的连接路线。
//!
//! SOCKS5 一律把主机名交给代理端解析（ATYP=0x03，字面 IP 除外）：访问境外
//! 主机时本地 DNS 往往被污染或解析不到，本地解析等于代理白配。

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream as StdTcpStream};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

/// 与 zap 一致的握手预算：慢代理 10 秒建不起隧道就该报错，而不是让
/// 连接卡片永远转圈。
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 全局代理三态（`ssh_proxy_mode`）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    #[default]
    Off,
    /// 读取操作系统的代理设置；不会读取终端环境变量。
    System,
    Custom,
}

impl ProxyMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" => Self::System,
            "custom" => Self::Custom,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::System => "system",
            Self::Custom => "custom",
        }
    }
}

/// 一条已解析的出站链路：普通代理服务器，或经另一台 SSH 主机转发（跳板）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyLink {
    Server(ProxyServer),
    /// SSH 跳板。值是 `user@host[:port]`、`~/.ssh/config` 别名或已存主机，
    /// 解析与认证沿用跳板自己的配置（profile / 密钥 / 它自己的代理设置）。
    Jump(String),
    /// 用子进程 stdin/stdout 承载目标字节流，语义与 OpenSSH ProxyCommand
    /// 一致。命令来自用户设置；`%h` / `%p` 在启动前替换为目标主机和端口。
    Command(String),
}

impl ProxyLink {
    /// 配置值统一入口：`jump:<主机>`、带协议前缀的 URL、或裸 `host[:port]`。
    /// 裸地址按 SOCKS5 处理，兼容已有设置；新界面要求用户在 SOCKS5 与
    /// HTTP 之间明确选择，不再提供语义含糊的自动识别。
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if let Some(rest) = strip_prefix_ignore_case(value, "jump:") {
            let target = rest.trim();
            if target.is_empty() {
                return Err("jump: 后面需要写跳板主机，例如 jump:user@bastion".to_owned());
            }
            if target.contains(',') {
                return Err("暂不支持多级跳板链（jump: 只能写一台主机）".to_owned());
            }
            return Ok(Self::Jump(target.to_owned()));
        }
        if let Some(rest) = strip_prefix_ignore_case(value, "command:") {
            let command = rest.trim();
            if command.is_empty() {
                return Err("command: 后面需要填写代理命令".to_owned());
            }
            if !command.contains("%h") || !command.contains("%p") {
                return Err("自定义代理命令必须同时包含 %h（目标主机）和 %p（目标端口）".to_owned());
            }
            return Ok(Self::Command(command.to_owned()));
        }
        if value.contains("://") {
            return ProxyServer::parse_url(value).map(Self::Server);
        }
        ProxyServer::parse_url(&format!("socks5://{value}")).map(Self::Server).map_err(|_| {
            format!(
                "无法识别的代理地址: {value}（支持 socks5:// / http:// / host:port / jump:主机）"
            )
        })
    }

    /// 连接池 key 里的链路身份（不含凭据）。
    pub fn identity(&self) -> String {
        match self {
            Self::Server(server) => server.identity(),
            Self::Jump(target) => format!("jump:{target}"),
            Self::Command(command) => format!("command:{command}"),
        }
    }
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .len()
        .checked_sub(prefix.len())
        .and_then(|_| value.get(..prefix.len()))
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

/// `jump:<目标>` 的目标部分（trim 后），大小写不敏感。设置页用它判断
/// 「指定代理」的子模式并回显当前跳板；真值解析仍走 [`ProxyLink::parse`]。
pub fn jump_target(value: &str) -> Option<&str> {
    strip_prefix_ignore_case(value.trim(), "jump:").map(str::trim)
}

/// 返回 `command:` 后的命令正文；设置页用它派生子模式并隐藏持久化前缀。
pub fn command_target(value: &str) -> Option<&str> {
    strip_prefix_ignore_case(value.trim(), "command:").map(str::trim)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalProxyProtocol {
    Socks5,
    Http,
    Mixed,
}

impl LocalProxyProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Socks5 => "SOCKS5",
            Self::Http => "HTTP",
            Self::Mixed => "HTTP + SOCKS5",
        }
    }

    fn url_scheme(self) -> &'static str {
        match self {
            // 混合端口优先 SOCKS5，让目标域名继续由代理端解析。
            Self::Socks5 | Self::Mixed => "socks5",
            Self::Http => "http",
        }
    }
}

/// 一次真实本机握手探测得到的代理端点。名称只描述协议，不根据常用端口
/// 猜测进程名，避免把任意监听 7890 的程序误标成 Clash。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProxyEndpoint {
    pub host: String,
    pub port: u16,
    pub protocol: LocalProxyProtocol,
}

impl LocalProxyEndpoint {
    pub fn name(&self) -> &'static str {
        match self.protocol {
            LocalProxyProtocol::Socks5 => "本机 SOCKS5 代理",
            LocalProxyProtocol::Http => "本机 HTTP 代理",
            LocalProxyProtocol::Mixed => "本机混合代理",
        }
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn url(&self) -> String {
        format!("{}://{}:{}", self.protocol.url_scheme(), self.host, self.port)
    }
}

const LOCAL_PROXY_PORTS: [u16; 9] = [7890, 7891, 7897, 1080, 10808, 10809, 20170, 20171, 2080];
const LOCAL_PROXY_PROBE_TIMEOUT: Duration = Duration::from_millis(140);

/// 扫描常用本机端口并分别执行 SOCKS5 与 HTTP CONNECT 握手。这个函数会
/// 阻塞，调用方必须放到后台线程；只把握手成功的端点交给 UI。
pub fn scan_local_proxies(extra_ports: &[u16]) -> Vec<LocalProxyEndpoint> {
    let mut ports = LOCAL_PROXY_PORTS.to_vec();
    for port in extra_ports.iter().copied().filter(|port| *port != 0) {
        if !ports.contains(&port) {
            ports.push(port);
        }
    }

    ports
        .into_iter()
        .filter_map(|port| {
            let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let socks5 = probe_local_socks5(address);
            let http = probe_local_http(address);
            let protocol = match (socks5, http) {
                (true, true) => LocalProxyProtocol::Mixed,
                (true, false) => LocalProxyProtocol::Socks5,
                (false, true) => LocalProxyProtocol::Http,
                (false, false) => return None,
            };
            Some(LocalProxyEndpoint { host: "127.0.0.1".to_owned(), port, protocol })
        })
        .collect()
}

fn local_probe_stream(address: SocketAddr) -> io::Result<StdTcpStream> {
    let stream = StdTcpStream::connect_timeout(&address, LOCAL_PROXY_PROBE_TIMEOUT)?;
    stream.set_read_timeout(Some(LOCAL_PROXY_PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(LOCAL_PROXY_PROBE_TIMEOUT))?;
    Ok(stream)
}

fn probe_local_socks5(address: SocketAddr) -> bool {
    let Ok(mut stream) = local_probe_stream(address) else { return false };
    if stream.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return false;
    }
    let mut response = [0u8; 2];
    stream.read_exact(&mut response).is_ok() && response == [0x05, 0x00]
}

fn probe_local_http(address: SocketAddr) -> bool {
    let Ok(mut stream) = local_probe_stream(address) else { return false };
    let request = b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    let mut response = [0u8; 160];
    let Ok(read) = stream.read(&mut response) else { return false };
    let head = String::from_utf8_lossy(&response[..read]);
    let Some(status) = head.lines().next().and_then(|line| line.split_whitespace().nth(1)) else {
        return false;
    };
    // 普通 Web 服务常以 400/404/405 回应 CONNECT；这里只接收代理对隧道
    // 请求的典型结果，避免把本机网站误报成 HTTP 代理。
    matches!(status.parse::<u16>(), Ok(200 | 407 | 500 | 502 | 503 | 504))
}

/// 自定义代理命令的双向字节流。持有 Child 以保证 russh 使用期间进程存活；
/// 流被丢弃时主动终止，避免失败连接遗留后台进程。
pub struct CommandStream {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl AsyncRead for CommandStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for CommandStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(cx)
    }
}

impl Drop for CommandStream {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn render_proxy_command(template: &str, target_host: &str, target_port: u16) -> io::Result<String> {
    // 用户命令本身是受信设置，但 `%h` 来自 SSH 配置。限制替换值字符集，
    // 防止恶意 Host 借 shell 元字符改变用户原本配置的命令。
    if target_host.is_empty()
        || !target_host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':'))
    {
        return Err(proxy_err("自定义代理命令的目标主机包含不安全字符"));
    }
    if !template.contains("%h") || !template.contains("%p") {
        return Err(proxy_err("自定义代理命令必须同时包含 %h 和 %p"));
    }
    Ok(template.replace("%h", target_host).replace("%p", &target_port.to_string()))
}

pub async fn connect_command(
    template: &str,
    target_host: &str,
    target_port: u16,
) -> io::Result<CommandStream> {
    let rendered = render_proxy_command(template, target_host, target_port)?;
    #[cfg(windows)]
    let mut command = {
        let mut command = tokio::process::Command::new("cmd.exe");
        command.args(["/D", "/S", "/C", &rendered]);
        command.creation_flags(0x08000000);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &rendered]);
        command
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| proxy_err(&format!("无法启动自定义代理命令: {err}")))?;
    let stdin = child.stdin.take().ok_or_else(|| proxy_err("自定义代理命令没有 stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| proxy_err("自定义代理命令没有 stdout"))?;
    Ok(CommandStream { child, stdin, stdout })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Socks5,
    HttpConnect,
}

/// 一台已解析好的代理服务器。密码只活在内存里，不进连接池 key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyServer {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyServer {
    /// 解析 `socks5://[user:pass@]host[:port]` / `http://[user:pass@]host[:port]`。
    /// `socks5h` 与 `socks5` 等价——我们本来就把域名交给代理解析。
    pub fn parse_url(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| format!("代理地址缺少协议前缀（socks5:// 或 http://）: {url}"))?;
        let (scheme, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "socks5" | "socks5h" | "socks" => (ProxyScheme::Socks5, 1080),
            "http" => (ProxyScheme::HttpConnect, 8080),
            other => return Err(format!("不支持的代理协议 {other}（支持 socks5 / http）")),
        };
        let rest = rest.trim_end_matches('/');
        let (userinfo, host_port) = match rest.rsplit_once('@') {
            Some((userinfo, host_port)) => (Some(userinfo), host_port),
            None => (None, rest),
        };
        let (username, password) = match userinfo {
            Some(userinfo) => {
                let (user, pass) = match userinfo.split_once(':') {
                    Some((user, pass)) => (user, Some(pass)),
                    None => (userinfo, None),
                };
                (Some(percent_decode(user)), pass.map(percent_decode))
            },
            None => (None, None),
        };
        let (host, port) = split_host_port(host_port, default_port)?;
        if host.is_empty() {
            return Err(format!("代理地址缺少主机名: {url}"));
        }
        Ok(Self { scheme, host, port, username, password })
    }

    /// 连接池 key 里的代理身份。刻意不含密码：换密码不改变隧道的对端，
    /// 旧连接仍然有效，不该被踢出池子。
    pub fn identity(&self) -> String {
        let scheme = match self.scheme {
            ProxyScheme::Socks5 => "socks5",
            ProxyScheme::HttpConnect => "http",
        };
        match self.username.as_deref() {
            Some(user) => format!("{scheme}://{user}@{}:{}", self.host, self.port),
            None => format!("{scheme}://{}:{}", self.host, self.port),
        }
    }

    /// 面向用户的错误信息里用：不含凭据。
    pub fn display(&self) -> String {
        let scheme = match self.scheme {
            ProxyScheme::Socks5 => "socks5",
            ProxyScheme::HttpConnect => "http",
        };
        format!("{scheme}://{}:{}", self.host, self.port)
    }
}

fn split_host_port(host_port: &str, default_port: u16) -> Result<(String, u16), String> {
    if let Some(rest) = host_port.strip_prefix('[') {
        let (host, suffix) =
            rest.split_once(']').ok_or_else(|| format!("无效的 IPv6 代理地址: {host_port}"))?;
        let port = match suffix.strip_prefix(':') {
            Some(port) => port.parse().map_err(|_| format!("无效的代理端口: {port}"))?,
            None => default_port,
        };
        return Ok((host.to_owned(), port));
    }
    match host_port.rsplit_once(':') {
        // 不带方括号但含多个冒号 = 裸 IPv6，整段当主机。
        Some((host, _)) if host.contains(':') => Ok((host_port.to_owned(), default_port)),
        Some((host, port)) => {
            let port = port.parse().map_err(|_| format!("无效的代理端口: {port}"))?;
            Ok((host.to_owned(), port))
        },
        None => Ok((host_port.to_owned(), default_port)),
    }
}

/// URL userinfo 里的百分号转义（密码带 `@`/`:` 时必须转义才能进 URL）。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 全局代理配置（设置页「网络」区块的持久化形态）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SshProxyConfig {
    pub mode: ProxyMode,
    pub url: String,
    /// 绕过列表：逗号分隔的主机名/后缀（`.internal` / `10.0.0.1` / `*`）。
    pub no_proxy: Vec<String>,
}

impl SshProxyConfig {
    /// 直接读 `nebula_settings.txt` 的三个键。SSH runtime 线程不持有窗口的
    /// 设置结构，走文件是两边共享配置的既有方式（同 `sync.rs`）。
    pub fn load_global() -> Self {
        let path = crate::display::nebula_data_dir().join("nebula_settings.txt");
        let mut config = Self::default();
        let Ok(data) = std::fs::read_to_string(path) else { return config };
        for line in data.lines() {
            match line.split_once('=') {
                Some(("ssh_proxy_mode", v)) => config.mode = ProxyMode::parse(v),
                Some(("ssh_proxy_url", v)) => config.url = v.trim().to_owned(),
                Some(("ssh_proxy_no_proxy", v)) => config.no_proxy = parse_no_proxy(v),
                _ => {},
            }
        }
        config
    }

    /// 汇总「这次连接走不走代理、走哪条链路」。`~/.ssh/config` 的
    /// `ProxyJump` 是目标自身的连接要求，优先于全局网络设置；主机编辑器
    /// 不再维护第二套代理覆盖字段。
    pub fn resolve(
        &self,
        config_proxy_jump: Option<&str>,
        target_host: &str,
    ) -> Result<Option<ProxyLink>, String> {
        if let Some(jump) = config_proxy_jump.map(str::trim).filter(|value| !value.is_empty()) {
            if jump.contains(',') {
                return Err(format!("暂不支持多级跳板链（ProxyJump {jump}）"));
            }
            return Ok(Some(ProxyLink::Jump(jump.to_owned())));
        }
        match self.mode {
            ProxyMode::Off => Ok(None),
            ProxyMode::Custom => {
                if self.url.trim().is_empty() {
                    return Err("代理模式为自定义，但未填写代理地址".to_owned());
                }
                ProxyLink::parse(&self.url).map(Some)
            },
            ProxyMode::System => {
                let Some((url, no_proxy)) = system_proxy() else { return Ok(None) };
                if bypassed(target_host, &no_proxy) {
                    return Ok(None);
                }
                ProxyServer::parse_url(&url).map(|server| Some(ProxyLink::Server(server)))
            },
        }
    }
}

pub fn parse_no_proxy(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.to_ascii_lowercase())
        .collect()
}

/// 目标主机是否命中绕过列表。规则与 curl 的 `NO_PROXY` 对齐：`*` 全绕过；
/// 条目做完整匹配或点边界后缀匹配（`example.com` 命中 `a.example.com`，
/// 不命中 `notexample.com`）。另收 WinINET ProxyOverride 的两种惯用形态：
/// `<local>`（无点主机名）与尾通配（`192.168.*` / `*.example.com`）——
/// Windows 代理软件写进注册表的就是这些，不认等于系统绕过列表白读。
fn bypassed(target_host: &str, no_proxy: &[String]) -> bool {
    let host = target_host.to_ascii_lowercase();
    no_proxy.iter().any(|entry| {
        if entry == "*" {
            return true;
        }
        if entry == "<local>" {
            return !host.contains('.') && !host.contains(':');
        }
        if let Some(prefix) = entry.strip_suffix('*') {
            if !prefix.is_empty() {
                return host.starts_with(prefix);
            }
        }
        let suffix = entry.strip_prefix("*.").or_else(|| entry.strip_prefix('.')).unwrap_or(entry);
        host == *suffix || host.ends_with(&format!(".{suffix}"))
    })
}

/// 「跟随系统」只读取操作系统的代理设置。Windows 上使用 WinINET 注册表；
/// 不读取 `ALL_PROXY`、`HTTP_PROXY` 等环境变量，避免终端启动环境悄悄改变
/// 软件内的连接路线。
fn system_proxy() -> Option<(String, Vec<String>)> {
    #[cfg(windows)]
    {
        return registry_proxy();
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 设置页「跟随系统」面板展示的探测结果来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemProxySource {
    /// `HKCU\..\Internet Settings`（`ProxyEnable` + `ProxyServer`）。
    Registry,
}

/// 当前系统代理读到了什么：`(URL, 来源)`。只做展示，不含绕过列表——连接
/// 决策走 [`SshProxyConfig::resolve`]。注册表是跨进程调用，调用方必须缓存
/// （进设置网络页 / 切模式时刷新），禁止进逐帧路径。
pub fn probe_system_proxy() -> Option<(String, SystemProxySource)> {
    #[cfg(windows)]
    {
        return registry_proxy().map(|(url, _)| (url, SystemProxySource::Registry));
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// HKCU\...\Internet Settings：`ProxyEnable` 非零时读 `ProxyServer` 与
/// `ProxyOverride`。值的解析拆成纯函数，跨平台可测。
#[cfg(windows)]
fn registry_proxy() -> Option<(String, Vec<String>)> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    if key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let url = wininet_proxy_url(&server)?;
    let bypass: String = key.get_value("ProxyOverride").unwrap_or_default();
    Some((url, parse_wininet_override(&bypass)))
}

/// WinINET `ProxyServer` 的两种形态：单值 `host:port`（所有协议共用一个
/// HTTP 代理），或 `http=...;https=...;socks=...` 的分协议表。分协议时优先
/// 取 `socks=`（域名交给代理解析，见模块注释），其余按 HTTP CONNECT。
fn wininet_proxy_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains('=') {
        return Some(format!("http://{value}"));
    }
    let pick = |wanted: &str| {
        value.split(';').find_map(|part| {
            let (key, addr) = part.split_once('=')?;
            let addr = addr.trim();
            (key.trim().eq_ignore_ascii_case(wanted) && !addr.is_empty()).then(|| addr.to_owned())
        })
    };
    if let Some(addr) = pick("socks") {
        return Some(format!("socks5://{addr}"));
    }
    pick("https").or_else(|| pick("http")).map(|addr| format!("http://{addr}"))
}

/// WinINET `ProxyOverride`：分号分隔，`<local>` 原样保留给 [`bypassed`] 特判。
fn parse_wininet_override(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.to_ascii_lowercase())
        .collect()
}

/// 经代理建立到 `target_host:target_port` 的隧道，返回可直接交给
/// `russh::client::connect_stream` 的流。
pub async fn connect(
    proxy: &ProxyServer,
    target_host: &str,
    target_port: u16,
) -> io::Result<TcpStream> {
    tokio::time::timeout(PROXY_CONNECT_TIMEOUT, async {
        let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port)).await?;
        // russh 的 connect 会给自己的 socket 设 nodelay；走 connect_stream
        // 时这份职责归我们，漏掉就是整条 SSH 会话按键延迟。
        stream.set_nodelay(true)?;
        match proxy.scheme {
            ProxyScheme::Socks5 => {
                socks5_handshake(&mut stream, proxy, target_host, target_port).await?
            },
            ProxyScheme::HttpConnect => {
                http_connect_handshake(&mut stream, proxy, target_host, target_port).await?
            },
        }
        Ok(stream)
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("代理握手超时（{} 秒无响应）", PROXY_CONNECT_TIMEOUT.as_secs()),
        )
    })?
}

/// RFC 1928 + RFC 1929。域名走 ATYP=0x03 交给代理解析；字面 IP 不涉及解析，
/// 用对应的二进制形态。
async fn socks5_handshake(
    stream: &mut TcpStream,
    proxy: &ProxyServer,
    target_host: &str,
    target_port: u16,
) -> io::Result<()> {
    let has_auth = proxy.username.is_some();
    let greeting: &[u8] = if has_auth { &[0x05, 0x02, 0x00, 0x02] } else { &[0x05, 0x01, 0x00] };
    stream.write_all(greeting).await?;

    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 0x05 {
        return Err(proxy_err("对端不是 SOCKS5 代理（版本应答不符）"));
    }
    match reply[1] {
        0x00 => {},
        0x02 => {
            let username = proxy.username.as_deref().unwrap_or_default().as_bytes();
            let password = proxy.password.as_deref().unwrap_or_default().as_bytes();
            if username.len() > 255 || password.len() > 255 {
                return Err(proxy_err("SOCKS5 用户名/密码超过 255 字节"));
            }
            let mut request = Vec::with_capacity(3 + username.len() + password.len());
            request.push(0x01);
            request.push(username.len() as u8);
            request.extend_from_slice(username);
            request.push(password.len() as u8);
            request.extend_from_slice(password);
            stream.write_all(&request).await?;
            let mut auth_reply = [0u8; 2];
            stream.read_exact(&mut auth_reply).await?;
            if auth_reply[1] != 0x00 {
                return Err(proxy_err("SOCKS5 代理拒绝了用户名/密码"));
            }
        },
        0xFF => return Err(proxy_err("SOCKS5 代理要求认证，但未配置用户名/密码")),
        method => return Err(proxy_err(&format!("SOCKS5 代理要求不支持的认证方式 {method:#04x}"))),
    }

    let mut request = vec![0x05, 0x01, 0x00];
    match target_host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        },
        Ok(std::net::IpAddr::V6(ip)) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        },
        Err(_) => {
            let host = target_host.as_bytes();
            if host.len() > 255 {
                return Err(proxy_err("目标主机名超过 255 字节"));
            }
            request.push(0x03);
            request.push(host.len() as u8);
            request.extend_from_slice(host);
        },
    }
    request.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&request).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(proxy_err(socks5_reply_message(head[1])));
    }
    // 吃掉应答里的绑定地址，让后续字节流从 SSH 协议开始。
    let addr_len = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            usize::from(len[0])
        },
        atyp => return Err(proxy_err(&format!("SOCKS5 应答携带未知地址类型 {atyp:#04x}"))),
    };
    let mut remainder = vec![0u8; addr_len + 2];
    stream.read_exact(&mut remainder).await?;
    Ok(())
}

fn socks5_reply_message(code: u8) -> &'static str {
    match code {
        0x01 => "SOCKS5 代理内部错误",
        0x02 => "SOCKS5 代理规则拒绝了此连接",
        0x03 => "SOCKS5 代理无法到达目标网络",
        0x04 => "SOCKS5 代理无法到达目标主机",
        0x05 => "目标主机拒绝连接（经由 SOCKS5 代理）",
        0x06 => "SOCKS5 连接超时（TTL 过期）",
        0x07 => "SOCKS5 代理不支持 CONNECT 命令",
        0x08 => "SOCKS5 代理不支持该地址类型",
        _ => "SOCKS5 代理返回未知错误",
    }
}

/// HTTP/1.1 CONNECT（含 Basic 认证）。读到空行为止，只认状态码。
async fn http_connect_handshake(
    stream: &mut TcpStream,
    proxy: &ProxyServer,
    target_host: &str,
    target_port: u16,
) -> io::Result<()> {
    // IPv6 字面量在 authority 里必须带方括号。
    let target = if target_host.contains(':') {
        format!("[{target_host}]:{target_port}")
    } else {
        format!("{target_host}:{target_port}")
    };
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(username) = proxy.username.as_deref() {
        use base64::Engine as _;
        let credentials = format!("{username}:{}", proxy.password.as_deref().unwrap_or_default());
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    // 逐字节读到 CRLFCRLF：多读一个字节都会吞掉 SSH 的版本行。
    let mut response = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        if response.len() > 16 * 1024 {
            return Err(proxy_err("HTTP 代理应答头超长"));
        }
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&response);
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line.split_whitespace().nth(1).and_then(|code| code.parse::<u16>().ok());
    match status {
        Some(200..=299) => Ok(()),
        Some(407) => Err(proxy_err("HTTP 代理要求认证（407），请检查用户名/密码")),
        Some(code) => Err(proxy_err(&format!("HTTP 代理拒绝建立隧道（{code}）"))),
        None => Err(proxy_err(&format!("HTTP 代理应答无法解析: {status_line}"))),
    }
}

fn proxy_err(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    #[test]
    fn parses_proxy_urls() {
        let plain = ProxyServer::parse_url("socks5://127.0.0.1:7890").unwrap();
        assert_eq!(
            (plain.scheme, plain.host.as_str(), plain.port, plain.username),
            (ProxyScheme::Socks5, "127.0.0.1", 7890, None)
        );

        let auth = ProxyServer::parse_url("http://user:p%40ss@proxy.lan").unwrap();
        assert_eq!(auth.scheme, ProxyScheme::HttpConnect);
        assert_eq!(auth.port, 8080, "http 缺省端口");
        assert_eq!(auth.username.as_deref(), Some("user"));
        assert_eq!(auth.password.as_deref(), Some("p@ss"), "百分号转义必须解开");

        assert_eq!(ProxyServer::parse_url("socks5://host").unwrap().port, 1080, "socks5 缺省端口");
        assert!(ProxyServer::parse_url("proxy.lan:1080").is_err(), "缺协议前缀必须报错");
        assert!(ProxyServer::parse_url("ftp://proxy.lan").is_err());
    }

    #[test]
    fn identity_excludes_password_but_keeps_user() {
        let server = ProxyServer::parse_url("socks5://user:secret@proxy.lan:1080").unwrap();
        let identity = server.identity();
        assert!(!identity.contains("secret"), "密码不得进连接池 key");
        assert_eq!(identity, "socks5://user@proxy.lan:1080");
    }

    #[test]
    fn no_proxy_matching_uses_dot_boundaries() {
        let list = parse_no_proxy("localhost, .Internal, 10.0.0.1, example.com");
        assert!(bypassed("localhost", &list));
        assert!(bypassed("db.internal", &list), "前导点 = 后缀匹配");
        assert!(bypassed("EXAMPLE.COM", &list), "大小写不敏感");
        assert!(bypassed("a.example.com", &list), "裸域名也做点边界后缀匹配");
        assert!(!bypassed("notexample.com", &list), "无点边界不得误伤");
        assert!(!bypassed("10.0.0.10", &list), "IP 是完整匹配不是前缀匹配");
        assert!(bypassed("anything", &[String::from("*")]));

        // WinINET ProxyOverride 的惯用形态。
        let wininet = parse_wininet_override("localhost;127.*;*.corp.example;<local>");
        assert!(bypassed("127.0.0.1", &wininet), "尾通配 = 前缀匹配");
        assert!(bypassed("db.corp.example", &wininet), "*.suffix 同点边界后缀");
        assert!(bypassed("bastion", &wininet), "<local> 命中无点主机名");
        assert!(!bypassed("bastion.lan", &wininet), "<local> 不得命中带点主机");
    }

    #[test]
    fn wininet_proxy_server_forms() {
        assert_eq!(wininet_proxy_url("127.0.0.1:7890").as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(
            wininet_proxy_url("http=127.0.0.1:7890;https=127.0.0.1:7891").as_deref(),
            Some("http://127.0.0.1:7891"),
            "分协议表优先 https"
        );
        assert_eq!(
            wininet_proxy_url("http=1.2.3.4:80;socks=127.0.0.1:1080").as_deref(),
            Some("socks5://127.0.0.1:1080"),
            "有 socks= 用 socks（域名交代理解析）"
        );
        assert_eq!(wininet_proxy_url("  "), None);
    }

    #[test]
    fn proxy_link_parse_forms() {
        assert!(matches!(
            ProxyLink::parse("socks5://127.0.0.1:7890"),
            Ok(ProxyLink::Server(server)) if server.scheme == ProxyScheme::Socks5
        ));
        // 裸 host:port（从 Clash 复制的形态）默认按 socks5。
        assert!(matches!(
            ProxyLink::parse("127.0.0.1:7890"),
            Ok(ProxyLink::Server(server)) if server.scheme == ProxyScheme::Socks5 && server.port == 7890
        ));
        assert_eq!(
            ProxyLink::parse("JUMP:ops@bastion.corp:2222"),
            Ok(ProxyLink::Jump("ops@bastion.corp:2222".to_owned())),
            "jump: 前缀大小写不敏感，值原样保留"
        );
        assert_eq!(ProxyLink::parse("jump:a,b").is_ok(), false, "多跳链明确报错");
        assert!(ProxyLink::parse("jump: ").is_err());
        assert!(ProxyLink::parse("ftp://x").is_err());
        assert_eq!(ProxyLink::Jump("bastion".into()).identity(), "jump:bastion");
        assert_eq!(
            ProxyLink::parse("command:corkscrew proxy 8080 %h %p"),
            Ok(ProxyLink::Command("corkscrew proxy 8080 %h %p".to_owned()))
        );
        assert!(ProxyLink::parse("command:nc proxy 8080").is_err(), "缺占位符必须报错");
    }

    #[test]
    fn proxy_command_replaces_only_valid_target_placeholders() {
        assert_eq!(
            render_proxy_command("tool --host %h --port %p", "vps.example.com", 2222).unwrap(),
            "tool --host vps.example.com --port 2222"
        );
        assert!(render_proxy_command("tool %h %p", "bad host&whoami", 22).is_err());
    }

    #[test]
    fn resolve_proxy_jump_then_global() {
        let global = SshProxyConfig {
            mode: ProxyMode::Custom,
            url: "socks5://global.lan:1080".to_owned(),
            no_proxy: parse_no_proxy("10.0.0.1"),
        };
        // ssh_config 的 ProxyJump 优先于全局绕过列表。
        assert_eq!(
            global.resolve(Some("ops@bastion"), "10.0.0.1").unwrap(),
            Some(ProxyLink::Jump("ops@bastion".to_owned()))
        );
        assert!(global.resolve(Some("j1,j2"), "vps.example.com").is_err(), "多跳报错");
        // 自定义模式不再提供“直连目标主机”条目；旧配置里的绕过列表
        // 不会让新的全局代理设置悄悄失效。
        assert!(matches!(
            global.resolve(None, "10.0.0.1").unwrap().unwrap(),
            ProxyLink::Server(server) if server.host == "global.lan"
        ));
        assert!(matches!(
            global.resolve(None, "vps.example.com").unwrap().unwrap(),
            ProxyLink::Server(server) if server.host == "global.lan"
        ));
        // 全局 url 也可以写 jump: 或裸地址。
        let jump_global = SshProxyConfig {
            mode: ProxyMode::Custom,
            url: "jump:bastion".to_owned(),
            no_proxy: Vec::new(),
        };
        assert_eq!(
            jump_global.resolve(None, "vps.example.com").unwrap(),
            Some(ProxyLink::Jump("bastion".to_owned()))
        );
        // 关闭态永远直连；自定义但没填地址必须报错而不是静默直连。
        let off = SshProxyConfig::default();
        assert!(off.resolve(None, "vps.example.com").unwrap().is_none());
        let empty = SshProxyConfig { mode: ProxyMode::Custom, ..Default::default() };
        assert!(empty.resolve(None, "vps.example.com").is_err());
    }

    /// 进程内 SOCKS5 服务器：校验客户端字节流的每一段，再回放数据验证
    /// 隧道透明。域名必须以 ATYP=0x03 原样到达——这是「代理端解析 DNS」
    /// 判据本身。
    #[test]
    fn socks5_handshake_sends_domain_and_credentials() {
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0u8; 4];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [0x05, 0x02, 0x00, 0x02]);
                stream.write_all(&[0x05, 0x02]).await.unwrap();

                let mut head = [0u8; 2];
                stream.read_exact(&mut head).await.unwrap();
                assert_eq!(head[0], 0x01);
                let mut user = vec![0u8; usize::from(head[1])];
                stream.read_exact(&mut user).await.unwrap();
                assert_eq!(user, b"user");
                let mut len = [0u8; 1];
                stream.read_exact(&mut len).await.unwrap();
                let mut pass = vec![0u8; usize::from(len[0])];
                stream.read_exact(&mut pass).await.unwrap();
                assert_eq!(pass, b"pass");
                stream.write_all(&[0x01, 0x00]).await.unwrap();

                let mut request = [0u8; 5];
                stream.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x03], "域名必须走 ATYP=0x03");
                let mut host = vec![0u8; usize::from(request[4])];
                stream.read_exact(&mut host).await.unwrap();
                assert_eq!(host, b"vps.example.com");
                let mut port = [0u8; 2];
                stream.read_exact(&mut port).await.unwrap();
                assert_eq!(u16::from_be_bytes(port), 2222);
                stream
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1F, 0x90])
                    .await
                    .unwrap();

                // 隧道透明性：SSH 版本行原样穿过。
                let mut banner = [0u8; 8];
                stream.read_exact(&mut banner).await.unwrap();
                assert_eq!(&banner, b"SSH-2.0-");
            });

            let proxy = ProxyServer {
                scheme: ProxyScheme::Socks5,
                host: addr.ip().to_string(),
                port: addr.port(),
                username: Some("user".to_owned()),
                password: Some("pass".to_owned()),
            };
            let mut stream = connect(&proxy, "vps.example.com", 2222).await.unwrap();
            stream.write_all(b"SSH-2.0-").await.unwrap();
            server.await.unwrap();
        });
    }

    #[test]
    fn socks5_failure_reply_maps_to_readable_error() {
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0u8; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                stream.write_all(&[0x05, 0x00]).await.unwrap();
                let mut request = vec![0u8; 22];
                stream.read_exact(&mut request).await.unwrap();
                stream.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
            });
            let proxy = ProxyServer {
                scheme: ProxyScheme::Socks5,
                host: addr.ip().to_string(),
                port: addr.port(),
                username: None,
                password: None,
            };
            let err = connect(&proxy, "vps.example.com", 22).await.unwrap_err();
            assert!(err.to_string().contains("拒绝连接"), "{err}");
        });
    }

    #[test]
    fn http_connect_sends_basic_auth_and_stops_at_header_end() {
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                let text = String::from_utf8(request).unwrap();
                assert!(text.starts_with("CONNECT vps.example.com:22 HTTP/1.1\r\n"), "{text}");
                // user:pass 的标准 base64。
                assert!(text.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"), "{text}");
                stream
                    .write_all(
                        b"HTTP/1.1 200 Connection established\r\nX-Filler: 1\r\n\r\nSSH-2.0-server",
                    )
                    .await
                    .unwrap();
            });
            let proxy = ProxyServer {
                scheme: ProxyScheme::HttpConnect,
                host: addr.ip().to_string(),
                port: addr.port(),
                username: Some("user".to_owned()),
                password: Some("pass".to_owned()),
            };
            let mut stream = connect(&proxy, "vps.example.com", 22).await.unwrap();
            // 头之后的字节属于隧道：一个都不能被握手吃掉。
            let mut banner = [0u8; 14];
            stream.read_exact(&mut banner).await.unwrap();
            assert_eq!(&banner, b"SSH-2.0-server");
            server.await.unwrap();
        });
    }

    #[test]
    fn http_connect_407_reports_auth_problem() {
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                stream
                    .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .await
                    .unwrap();
            });
            let proxy = ProxyServer {
                scheme: ProxyScheme::HttpConnect,
                host: addr.ip().to_string(),
                port: addr.port(),
                username: None,
                password: None,
            };
            let err = connect(&proxy, "vps.example.com", 22).await.unwrap_err();
            assert!(err.to_string().contains("407"), "{err}");
        });
    }
}
