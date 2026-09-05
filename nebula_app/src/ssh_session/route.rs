use crate::ssh_profiles::{SshHostJumpMode, SshHostProxyMode, SshProfileAuth};
use crate::ssh_proxy::{ProxyLink, ProxyMode, ProxyScheme, ProxyServer, SshProxyConfig};

use super::SshDestination;

pub(super) struct ResolvedRoute {
    pub destination: SshDestination,
    pub profile: SshProfileAuth,
    pub transport: RouteTransport,
    #[cfg(test)]
    pub known_hosts_path: Option<std::path::PathBuf>,
}

pub(super) enum RouteTransport {
    Direct,
    Server(ProxyServer),
    Command(String),
    Jump(Box<ResolvedRoute>),
}

#[derive(Clone)]
enum NetworkOverride {
    Direct,
    Server(ProxyServer),
}

impl ResolvedRoute {
    pub fn pool_key(&self) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mut digest = Sha256::new();
        let mut current = self;
        loop {
            let metadata = serde_json::to_vec(&(
                &current.destination.original,
                current.destination.pool_key(),
                current.profile.auth,
                &current.profile.private_keys,
                &current.destination.identity_files,
            ))
            .expect("SSH route metadata is serializable");
            digest.update((metadata.len() as u64).to_be_bytes());
            digest.update(metadata);
            match &current.transport {
                RouteTransport::Jump(jump) => {
                    digest.update(b"jump");
                    current = jump;
                },
                RouteTransport::Direct => {
                    digest.update(b"direct");
                    break;
                },
                RouteTransport::Server(server) => {
                    digest.update(b"proxy");
                    digest.update(server.identity());
                    break;
                },
                RouteTransport::Command(command) => {
                    digest.update(b"command");
                    digest.update(command);
                    break;
                },
            }
        }
        let mut fingerprint = String::with_capacity(64);
        for byte in digest.finalize() {
            let _ = write!(fingerprint, "{byte:02x}");
        }
        format!("{}|route:{fingerprint}", self.destination.pool_key())
    }
}

pub(super) fn resolve_route_with(
    destination: SshDestination,
    profile: SshProfileAuth,
    global: &SshProxyConfig,
    depth: u8,
    proxy_password: Option<&str>,
    resolve_host: &mut impl FnMut(&str) -> Result<(SshDestination, SshProfileAuth), String>,
    load_secret: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<ResolvedRoute, String> {
    build_route(
        destination,
        profile,
        global,
        depth,
        proxy_password,
        None,
        &mut Vec::new(),
        resolve_host,
        load_secret,
    )
}

fn build_route(
    destination: SshDestination,
    profile: SshProfileAuth,
    global: &SshProxyConfig,
    depth: u8,
    proxy_password: Option<&str>,
    inherited_proxy: Option<NetworkOverride>,
    ancestors: &mut Vec<(String, u16)>,
    resolve_host: &mut impl FnMut(&str) -> Result<(SshDestination, SshProfileAuth), String>,
    load_secret: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<ResolvedRoute, String> {
    profile.connection.validate(&destination.original)?;
    let endpoint = (destination.host.to_ascii_lowercase(), destination.port);
    if ancestors.contains(&endpoint) {
        return Err("跳板链存在循环或将目标主机自身用作跳板".to_owned());
    }
    ancestors.push(endpoint);
    let options = &profile.connection;
    let network = match inherited_proxy {
        Some(network) => Some(network),
        None => match options.proxy_mode {
            SshHostProxyMode::Inherit => None,
            SshHostProxyMode::Direct => Some(NetworkOverride::Direct),
            SshHostProxyMode::Socks5 | SshHostProxyMode::Http => {
                let username = options.proxy_username.trim();
                let password = if username.is_empty() {
                    None
                } else if let Some(password) = proxy_password {
                    Some(password.to_owned())
                } else if let Some(target) = options.proxy_credential_target(&destination.original)
                {
                    let secret = load_secret(&target)?;
                    match secret {
                        Some(mut secret) => {
                            let password = std::str::from_utf8(&secret)
                                .map(str::to_owned)
                                .map_err(|_| "代理密码无法读取，请重新填写并保存".to_owned());
                            secret.fill(0);
                            Some(password?)
                        },
                        None => None,
                    }
                } else {
                    None
                };
                if options.proxy_mode == SshHostProxyMode::Socks5
                    && password.as_ref().is_some_and(|password| password.len() > 255)
                {
                    return Err("SOCKS5 代理密码不能超过 255 字节".to_owned());
                }
                Some(NetworkOverride::Server(ProxyServer {
                    scheme: if options.proxy_mode == SshHostProxyMode::Socks5 {
                        ProxyScheme::Socks5
                    } else {
                        ProxyScheme::HttpConnect
                    },
                    host: options.normalized_proxy_host(),
                    port: options.effective_proxy_port(),
                    username: (!username.is_empty()).then(|| username.to_owned()),
                    password,
                }))
            },
        },
    };
    let jump = match options.jump_mode {
        SshHostJumpMode::Host => Some(options.jump_host.trim().to_owned()),
        SshHostJumpMode::None => None,
        SshHostJumpMode::Inherit => destination
            .proxy_jump
            .as_deref()
            .map(str::trim)
            .filter(|jump| !jump.is_empty() && !jump.eq_ignore_ascii_case("none"))
            .map(str::to_owned)
            .or_else(|| {
                (depth == 0 && global.mode == ProxyMode::Custom)
                    .then(|| crate::ssh_proxy::jump_target(&global.url).map(str::to_owned))
                    .flatten()
            }),
    };
    let transport = if let Some(spec) = jump {
        crate::ssh_profiles::validate_ssh_destination(&spec)
            .map_err(|_| "跳板地址无效，仅支持单个 SSH 别名或 user@host:port".to_owned())?;
        if depth >= 2 {
            return Err("跳板链过深，最多支持 2 级跳板".to_owned());
        }
        let (jump_destination, jump_profile) = resolve_host(&spec)?;
        RouteTransport::Jump(Box::new(build_route(
            jump_destination,
            jump_profile,
            global,
            depth + 1,
            None,
            network,
            ancestors,
            resolve_host,
            load_secret,
        )?))
    } else {
        match network {
            Some(NetworkOverride::Direct) => RouteTransport::Direct,
            Some(NetworkOverride::Server(server)) => RouteTransport::Server(server),
            None => match global.resolve(None, &destination.host)? {
                Some(ProxyLink::Server(server)) => RouteTransport::Server(server),
                Some(ProxyLink::Command(command)) => RouteTransport::Command(command),
                Some(ProxyLink::Jump(_)) | None => RouteTransport::Direct,
            },
        }
    };
    ancestors.pop();
    Ok(ResolvedRoute {
        destination,
        profile,
        transport,
        #[cfg(test)]
        known_hosts_path: None,
    })
}

#[cfg(test)]
mod tests;
