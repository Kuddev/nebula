//! SSH 出站代理：配置解析 + SOCKS5（RFC 1928/1929）与 HTTP CONNECT 握手。
//!
//! 不引入代理 crate：握手完成后把裸 `TcpStream` 交给 russh 的
//! `client::connect_stream`。全局配置存 `nebula_settings.txt`
//! （`ssh_proxy_mode` / `ssh_proxy_url` / `ssh_proxy_no_proxy`），每主机覆盖
//! 存 `ssh_profiles.json` 的 `proxy` 字段（`"direct"` 或代理 URL）。
//!
//! SOCKS5 一律把主机名交给代理端解析（ATYP=0x03，字面 IP 除外）：访问境外
//! 主机时本地 DNS 往往被污染或解析不到，本地解析等于代理白配。

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 与 zap 一致的握手预算：慢代理 10 秒建不起隧道就该报错，而不是让
/// 连接卡片永远转圈。
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 全局代理三态（`ssh_proxy_mode`）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    #[default]
    Off,
    /// 读环境变量 `ALL_PROXY` → `HTTPS_PROXY` → `HTTP_PROXY`（含小写变体），
    /// `NO_PROXY` 并入绕过列表。
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

    /// 汇总「这次连接走不走代理、走哪台」。`override_value` 来自主机 profile
    /// 的 `proxy` 字段；配置写错要立刻报错——静默直连会把「代理没生效」伪装
    /// 成网络问题。
    pub fn resolve(
        &self,
        override_value: Option<&str>,
        target_host: &str,
    ) -> Result<Option<ProxyServer>, String> {
        if let Some(value) = override_value.map(str::trim).filter(|value| !value.is_empty()) {
            if value.eq_ignore_ascii_case("direct") {
                return Ok(None);
            }
            return ProxyServer::parse_url(value).map(Some);
        }
        match self.mode {
            ProxyMode::Off => Ok(None),
            ProxyMode::Custom => {
                if self.url.trim().is_empty() {
                    return Err("代理模式为自定义，但未填写代理地址".to_owned());
                }
                if bypassed(target_host, &self.no_proxy) {
                    return Ok(None);
                }
                ProxyServer::parse_url(&self.url).map(Some)
            },
            ProxyMode::System => {
                let Some(url) = system_proxy_url() else { return Ok(None) };
                let mut no_proxy = self.no_proxy.clone();
                if let Some(env) = env_var("NO_PROXY") {
                    no_proxy.extend(parse_no_proxy(&env));
                }
                if bypassed(target_host, &no_proxy) {
                    return Ok(None);
                }
                ProxyServer::parse_url(&url).map(Some)
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
/// 不命中 `notexample.com`）。
fn bypassed(target_host: &str, no_proxy: &[String]) -> bool {
    let host = target_host.to_ascii_lowercase();
    no_proxy.iter().any(|entry| {
        if entry == "*" {
            return true;
        }
        let suffix = entry.strip_prefix('.').unwrap_or(entry);
        host == *suffix || host.ends_with(&format!(".{suffix}"))
    })
}

/// 环境变量代理，优先级与 zap 的 `resolve_proxy` 对齐。
fn system_proxy_url() -> Option<String> {
    ["ALL_PROXY", "HTTPS_PROXY", "HTTP_PROXY"]
        .iter()
        .find_map(|name| env_var(name))
        .filter(|value| !value.trim().is_empty())
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().or_else(|| std::env::var(name.to_ascii_lowercase()).ok())
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
    }

    #[test]
    fn resolve_precedence_override_then_global() {
        let global = SshProxyConfig {
            mode: ProxyMode::Custom,
            url: "socks5://global.lan:1080".to_owned(),
            no_proxy: parse_no_proxy("10.0.0.1"),
        };
        // 每主机覆盖优先于全局，且不受绕过列表影响（覆盖是明确指令）。
        let forced = global.resolve(Some("http://special.lan:3128"), "10.0.0.1").unwrap().unwrap();
        assert_eq!(forced.scheme, ProxyScheme::HttpConnect);
        assert!(global.resolve(Some("direct"), "vps.example.com").unwrap().is_none());
        // 跟随全局：绕过列表命中 = 直连，其余走代理。
        assert!(global.resolve(None, "10.0.0.1").unwrap().is_none());
        assert_eq!(global.resolve(None, "vps.example.com").unwrap().unwrap().host, "global.lan");
        // 关闭态永远直连；自定义但没填地址必须报错而不是静默直连。
        let off = SshProxyConfig::default();
        assert!(off.resolve(None, "vps.example.com").unwrap().is_none());
        let empty = SshProxyConfig { mode: ProxyMode::Custom, ..Default::default() };
        assert!(empty.resolve(None, "vps.example.com").is_err());
        // 覆盖字段写了非法 URL 也要报错。
        assert!(global.resolve(Some("not-a-url"), "vps.example.com").is_err());
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
