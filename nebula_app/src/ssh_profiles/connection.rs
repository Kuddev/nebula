use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshHostProxyMode {
    #[default]
    Inherit,
    Direct,
    Socks5,
    Http,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshHostJumpMode {
    #[default]
    Inherit,
    None,
    Host,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshConnectionOptions {
    pub proxy_mode: SshHostProxyMode,
    pub proxy_host: String,
    pub proxy_port: Option<u16>,
    pub proxy_username: String,
    pub jump_mode: SshHostJumpMode,
    pub jump_host: String,
}

impl SshConnectionOptions {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn effective_proxy_port(&self) -> u16 {
        self.proxy_port.unwrap_or(match self.proxy_mode {
            SshHostProxyMode::Http => 8080,
            _ => 1080,
        })
    }

    pub fn has_custom_proxy(&self) -> bool {
        matches!(self.proxy_mode, SshHostProxyMode::Socks5 | SshHostProxyMode::Http)
    }

    pub fn normalized_proxy_host(&self) -> String {
        let host = self.proxy_host.trim();
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host)
            .to_ascii_lowercase()
    }

    pub fn validate(&self, destination: &str) -> Result<(), String> {
        validate_ssh_destination(destination)?;
        if self.has_custom_proxy() {
            validate_host(&self.normalized_proxy_host()).map_err(|_| {
                "代理地址无效，请只填写主机名或 IP，不要包含协议、端口或密码".to_owned()
            })?;
            if self.effective_proxy_port() == 0 {
                return Err("代理端口必须在 1–65535 之间".to_owned());
            }
            let username = self.proxy_username.trim();
            if username.chars().any(char::is_control) {
                return Err("代理用户名不能包含控制字符".to_owned());
            }
            if self.proxy_mode == SshHostProxyMode::Socks5 && username.len() > 255 {
                return Err("SOCKS5 用户名不能超过 255 字节".to_owned());
            }
            if self.proxy_mode == SshHostProxyMode::Http && username.contains(':') {
                return Err("HTTP 代理用户名不能包含冒号".to_owned());
            }
        }
        if self.jump_mode == SshHostJumpMode::Host {
            validate_ssh_destination(&self.jump_host)
                .map_err(|_| "跳板地址无效，请填写单个 SSH 别名或 user@host:port".to_owned())?;
            if normalized_destination(&self.jump_host) == normalized_destination(destination) {
                return Err("不能将目标主机自身设为跳板".to_owned());
            }
        }
        Ok(())
    }

    pub fn proxy_credential_target(&self, destination: &str) -> Option<String> {
        if !self.has_custom_proxy() || self.proxy_username.trim().is_empty() {
            return None;
        }
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mode = match self.proxy_mode {
            SshHostProxyMode::Socks5 => "socks5",
            SshHostProxyMode::Http => "http",
            _ => return None,
        };
        let mut digest = Sha256::new();
        for field in [
            destination.trim().to_owned(),
            mode.to_owned(),
            self.normalized_proxy_host(),
            self.effective_proxy_port().to_string(),
            self.proxy_username.trim().to_owned(),
        ] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        let mut fingerprint = String::with_capacity(64);
        for byte in digest.finalize() {
            let _ = write!(fingerprint, "{byte:02x}");
        }
        Some(format!("Nebula/SSH/Proxy/{fingerprint}"))
    }
}

pub(crate) fn validate_ssh_destination(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || ",;&|<>\"'`\\?#".contains(character)
        })
    {
        return Err("SSH 地址为空或包含不允许的字符".to_owned());
    }
    let address = value.strip_prefix("ssh://").unwrap_or(value);
    if address.starts_with('-') {
        return Err("SSH 地址不能以选项前缀开头".to_owned());
    }
    let host_port = if let Some((username, host)) = address.rsplit_once('@') {
        if username.is_empty() || username.contains(['@', ':', '/', '[', ']']) {
            return Err("SSH 用户名无效".to_owned());
        }
        host
    } else {
        address
    };
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']').ok_or("IPv6 地址缺少右方括号")?;
        if host.parse::<Ipv6Addr>().is_err() {
            return Err("IPv6 地址无效".to_owned());
        }
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or("SSH 端口格式无效")?)
        };
        (host, port)
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        if host.contains(':') { (host_port, None) } else { (host, Some(port)) }
    } else {
        (host_port, None)
    };
    validate_host(host)?;
    if port.is_some_and(|port| port.parse::<u16>().map_or(true, |port| port == 0)) {
        return Err("SSH 端口必须在 1–65535 之间".to_owned());
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<(), String> {
    if host.is_empty() || host.starts_with('-') || host.len() > 253 {
        return Err("主机名无效".to_owned());
    }
    if host.contains(':') {
        host.parse::<Ipv6Addr>().map_err(|_| "IPv6 地址无效".to_owned())?;
    } else if host
        .chars()
        .any(|character| !character.is_alphanumeric() && !matches!(character, '.' | '-' | '_'))
    {
        return Err("主机名包含不允许的字符".to_owned());
    }
    Ok(())
}

fn normalized_destination(destination: &str) -> String {
    let destination = destination.trim().strip_prefix("ssh://").unwrap_or(destination.trim());
    let (username, host) = destination.rsplit_once('@').unwrap_or(("", destination));
    let host = host.strip_suffix(":22").unwrap_or(host).to_ascii_lowercase();
    format!("{username}@{host}")
}

#[cfg(test)]
mod tests;
