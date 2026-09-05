use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use russh::Channel;
use russh::keys::ssh_key::{Algorithm, LineEnding, PrivateKey, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{ResolvedRoute, RouteTransport, custom_proxy, host, jump, resolve_route_with};
use crate::ssh_profiles::{SshAuthMode, SshHostProxyMode};
use crate::ssh_proxy::{ProxyScheme, SshProxyConfig};
use crate::ssh_session::{SshTestRequest, test_connect};

const PROXY_USER: &str = "fixture-proxy-user";
const PROXY_PASSWORD: &str = "fixture-proxy-draft-password";
const BASTION_USER: &str = "fixture-bastion-user";
const TARGET_USER: &str = "fixture-target-user";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    ProxyPassword,
    BastionIdentity,
    TargetIdentity,
    BastionHostKey,
    TargetHostKey,
}

#[derive(Default)]
struct Observations {
    proxy_authenticated: Option<bool>,
    proxy_destination: Option<SocketAddr>,
    ssh_authentication: Vec<(&'static str, String, bool)>,
    forwards: Vec<(String, u32)>,
}

struct LoopbackSshServer {
    username: &'static str,
    public_key: PublicKey,
    forward_destination: Option<SocketAddr>,
    observations: Arc<Mutex<Observations>>,
}

impl server::Handler for LoopbackSshServer {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        username: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let accepted = username == self.username && public_key == &self.public_key;
        self.observations.lock().unwrap().ssh_authentication.push((
            self.username,
            username.to_owned(),
            accepted,
        ));
        Ok(if accepted { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_password(
        &mut self,
        username: &str,
        _password: &str,
    ) -> Result<Auth, Self::Error> {
        self.observations.lock().unwrap().ssh_authentication.push((
            "unexpected-password-auth",
            username.to_owned(),
            false,
        ));
        Ok(Auth::reject())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.observations
            .lock()
            .unwrap()
            .forwards
            .push((host_to_connect.to_owned(), port_to_connect));
        let Some(destination) = self.forward_destination else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        if host_to_connect != destination.ip().to_string()
            || port_to_connect != u32::from(destination.port())
        {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        let mut stream = TcpStream::connect(destination).await?;
        stream.set_nodelay(true)?;
        reply.accept().await;
        tokio::spawn(async move {
            let mut channel = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut channel, &mut stream).await;
        });
        Ok(())
    }
}

fn generate_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

fn write_key(path: &Path, key: &PrivateKey) {
    std::fs::write(path, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
}

fn configure_known_hosts(route: &mut ResolvedRoute, path: &Path) {
    route.known_hosts_path = Some(path.to_owned());
    if let RouteTransport::Jump(jump) = &mut route.transport {
        configure_known_hosts(jump, path);
    }
}

fn spawn_ssh_server(
    listener: TcpListener,
    host_key: PrivateKey,
    handler: LoopbackSshServer,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            inactivity_timeout: Some(Duration::from_secs(10)),
            ..Default::default()
        });
        if let Ok(session) = server::run_stream(config, stream, handler).await {
            let _ = session.await;
        }
    })
}

async fn send_fragmented(stream: &mut TcpStream, response: &[u8]) -> io::Result<()> {
    for byte in response {
        stream.write_all(&[*byte]).await?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn read_sized_bytes(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let length = stream.read_u8().await?;
    let mut data = vec![0; usize::from(length)];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

async fn socks_proxy_handshake(
    stream: &mut TcpStream,
    destination: SocketAddr,
    observations: &Arc<Mutex<Observations>>,
) -> io::Result<bool> {
    let mut greeting = [0; 4];
    stream.read_exact(&mut greeting).await?;
    assert_eq!(greeting, [5, 2, 0, 2]);
    send_fragmented(stream, &[5, 2]).await?;
    assert_eq!(stream.read_u8().await?, 1);
    let username = read_sized_bytes(stream).await?;
    let password = read_sized_bytes(stream).await?;
    let accepted = username == PROXY_USER.as_bytes() && password == PROXY_PASSWORD.as_bytes();
    observations.lock().unwrap().proxy_authenticated = Some(accepted);
    send_fragmented(stream, &[1, u8::from(!accepted)]).await?;
    if !accepted {
        return Ok(false);
    }
    let mut request = [0; 4];
    stream.read_exact(&mut request).await?;
    assert_eq!(request, [5, 1, 0, 1]);
    let mut address = [0; 4];
    stream.read_exact(&mut address).await?;
    let port = stream.read_u16().await?;
    let requested = SocketAddr::from((address, port));
    assert_eq!(requested, destination, "the network proxy must connect to the bastion");
    observations.lock().unwrap().proxy_destination = Some(requested);
    Ok(true)
}

async fn http_proxy_handshake(
    stream: &mut TcpStream,
    destination: SocketAddr,
    observations: &Arc<Mutex<Observations>>,
) -> io::Result<bool> {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        assert!(request.len() < 16 * 1024);
        request.push(stream.read_u8().await?);
    }
    let request = String::from_utf8(request).unwrap();
    let authority = destination.to_string();
    assert!(request.starts_with(&format!("CONNECT {authority} HTTP/1.1\r\n")));
    assert!(request.contains(&format!("\r\nHost: {authority}\r\n")));
    let authentication =
        base64::engine::general_purpose::STANDARD.encode(format!("{PROXY_USER}:{PROXY_PASSWORD}"));
    let accepted =
        request.contains(&format!("\r\nProxy-Authorization: Basic {authentication}\r\n"));
    observations.lock().unwrap().proxy_authenticated = Some(accepted);
    if !accepted {
        send_fragmented(stream, b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await?;
        return Ok(false);
    }
    observations.lock().unwrap().proxy_destination = Some(destination);
    Ok(true)
}

fn spawn_proxy(
    listener: TcpListener,
    scheme: ProxyScheme,
    destination: SocketAddr,
    observations: Arc<Mutex<Observations>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        client.set_nodelay(true).unwrap();
        let accepted = match scheme {
            ProxyScheme::Socks5 => {
                socks_proxy_handshake(&mut client, destination, &observations).await
            },
            ProxyScheme::HttpConnect => {
                http_proxy_handshake(&mut client, destination, &observations).await
            },
        }
        .unwrap();
        if !accepted {
            return;
        }
        let mut server = TcpStream::connect(destination).await.unwrap();
        server.set_nodelay(true).unwrap();
        match scheme {
            ProxyScheme::Socks5 => {
                send_fragmented(&mut client, &[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await.unwrap();
            },
            ProxyScheme::HttpConnect => {
                send_fragmented(&mut client, b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
            },
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
    })
}

async fn verify_loopback_route(scheme: ProxyScheme, failure: Failure) {
    let temporary = tempfile::tempdir().unwrap();
    let known_hosts = temporary.path().join("known_hosts");
    let bastion_key_path = temporary.path().join("bastion-identity");
    let target_key_path = temporary.path().join("target-identity");
    let bastion_identity = generate_key();
    let target_identity = generate_key();
    let bastion_host_key = generate_key();
    let target_host_key = generate_key();
    assert_ne!(bastion_identity.public_key(), target_identity.public_key());
    assert_ne!(bastion_host_key.public_key(), target_host_key.public_key());
    write_key(&bastion_key_path, &bastion_identity);
    write_key(&target_key_path, &target_identity);

    let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_address = target_listener.local_addr().unwrap();
    let bastion_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bastion_address = bastion_listener.local_addr().unwrap();
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let wrong_host_key = generate_key();
    for (address, key) in [
        (
            bastion_address,
            if failure == Failure::BastionHostKey { &wrong_host_key } else { &bastion_host_key },
        ),
        (
            target_address,
            if failure == Failure::TargetHostKey { &wrong_host_key } else { &target_host_key },
        ),
    ] {
        russh::keys::known_hosts::learn_known_hosts_path(
            &address.ip().to_string(),
            address.port(),
            key.public_key(),
            &known_hosts,
        )
        .unwrap();
    }
    let initial_known_hosts = std::fs::read(&known_hosts).unwrap();
    let observations = Arc::new(Mutex::new(Observations::default()));
    let target_task = spawn_ssh_server(
        target_listener,
        target_host_key,
        LoopbackSshServer {
            username: TARGET_USER,
            public_key: target_identity.public_key().clone(),
            forward_destination: None,
            observations: observations.clone(),
        },
    );
    let bastion_task = spawn_ssh_server(
        bastion_listener,
        bastion_host_key,
        LoopbackSshServer {
            username: BASTION_USER,
            public_key: bastion_identity.public_key().clone(),
            forward_destination: Some(target_address),
            observations: observations.clone(),
        },
    );
    let proxy_task = spawn_proxy(proxy_listener, scheme, bastion_address, observations.clone());

    let (destination, mut profile) = host(&format!("{TARGET_USER}@{target_address}"));
    profile.auth = SshAuthMode::PublicKey;
    profile.private_keys = vec![if failure == Failure::TargetIdentity {
        bastion_key_path.clone()
    } else {
        target_key_path.clone()
    }];
    custom_proxy(
        &mut profile,
        if scheme == ProxyScheme::Socks5 {
            SshHostProxyMode::Socks5
        } else {
            SshHostProxyMode::Http
        },
        "127.0.0.1",
    );
    profile.connection.proxy_port = Some(proxy_address.port());
    profile.connection.proxy_username = PROXY_USER.to_owned();
    jump(&mut profile, "loopback-bastion");
    let (bastion_destination, mut bastion_profile) =
        host(&format!("{BASTION_USER}@{bastion_address}"));
    bastion_profile.auth = SshAuthMode::PublicKey;
    bastion_profile.private_keys =
        vec![if failure == Failure::BastionIdentity { target_key_path } else { bastion_key_path }];
    let proxy_draft = if failure == Failure::ProxyPassword {
        "incorrect-fixture-proxy-draft"
    } else {
        PROXY_PASSWORD
    };
    let request = SshTestRequest {
        request_id: 1,
        destination: destination.original.clone(),
        auth: profile.auth,
        private_keys: profile.private_keys.clone(),
        password: None,
        connection: profile.connection.clone(),
        proxy_password: Some(proxy_draft.to_owned()),
    };
    let mut route = resolve_route_with(
        destination,
        profile,
        &SshProxyConfig::default(),
        0,
        request.proxy_password.as_deref(),
        &mut |spec| {
            assert_eq!(spec, "loopback-bastion");
            Ok((bastion_destination.clone(), bastion_profile.clone()))
        },
        &mut |_| panic!("a loopback fixture must never read stored credentials"),
    )
    .unwrap();
    configure_known_hosts(&mut route, &known_hosts);
    assert!(!route.pool_key().contains(proxy_draft));
    let outcome = test_connect(&route, &request).await;
    if failure == Failure::None {
        outcome.expect("real proxy -> SSH bastion -> SSH target should authenticate");
    } else {
        assert!(outcome.is_err(), "the invalid credential or host key must be rejected");
    }
    assert_eq!(std::fs::read(&known_hosts).unwrap(), initial_known_hosts);
    {
        let observed = observations.lock().unwrap();
        assert_eq!(observed.proxy_authenticated, Some(failure != Failure::ProxyPassword));
        if failure == Failure::ProxyPassword {
            assert!(observed.ssh_authentication.is_empty());
            assert!(observed.forwards.is_empty());
        } else {
            assert_eq!(observed.proxy_destination, Some(bastion_address));
            if failure == Failure::BastionHostKey {
                assert!(observed.ssh_authentication.is_empty());
                assert!(observed.forwards.is_empty());
            } else {
                assert!(observed.ssh_authentication.contains(&(
                    BASTION_USER,
                    BASTION_USER.to_owned(),
                    failure != Failure::BastionIdentity,
                )));
                if failure == Failure::BastionIdentity {
                    assert!(observed.forwards.is_empty());
                    assert!(
                        !observed
                            .ssh_authentication
                            .iter()
                            .any(|(role, _, _)| *role == TARGET_USER)
                    );
                } else {
                    assert_eq!(
                        observed.forwards,
                        vec![("127.0.0.1".to_owned(), u32::from(target_address.port()))]
                    );
                    if failure == Failure::TargetHostKey {
                        assert!(
                            !observed
                                .ssh_authentication
                                .iter()
                                .any(|(role, _, _)| *role == TARGET_USER)
                        );
                    } else {
                        assert!(observed.ssh_authentication.contains(&(
                            TARGET_USER,
                            TARGET_USER.to_owned(),
                            failure != Failure::TargetIdentity,
                        )));
                    }
                }
            }
        }
        assert!(
            !observed
                .ssh_authentication
                .iter()
                .any(|(role, _, _)| *role == "unexpected-password-auth")
        );
    }
    for task in [proxy_task, bastion_task, target_task] {
        if task.is_finished() {
            task.await.expect("loopback protocol task should not panic");
        } else {
            task.abort();
            let _ = task.await;
        }
    }
}

fn run_loopback(scheme: ProxyScheme, failure: Failure) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(15), verify_loopback_route(scheme, failure))
            .await
            .expect("the isolated SSH route fixture must finish within its deadline");
    });
}

#[test]
fn socks5_bastion_target_uses_real_ssh_and_independent_identities() {
    run_loopback(ProxyScheme::Socks5, Failure::None);
}

#[test]
fn http_connect_bastion_target_uses_real_ssh_and_independent_identities() {
    run_loopback(ProxyScheme::HttpConnect, Failure::None);
}

#[test]
fn wrong_proxy_draft_does_not_fall_back_to_saved_credentials_or_direct() {
    run_loopback(ProxyScheme::HttpConnect, Failure::ProxyPassword);
}

#[test]
fn target_private_key_is_not_accepted_as_bastion_identity() {
    run_loopback(ProxyScheme::Socks5, Failure::BastionIdentity);
}

#[test]
fn bastion_private_key_is_not_accepted_as_target_identity() {
    run_loopback(ProxyScheme::HttpConnect, Failure::TargetIdentity);
}

#[test]
fn changed_bastion_host_key_stops_the_route_before_authentication() {
    run_loopback(ProxyScheme::Socks5, Failure::BastionHostKey);
}

#[test]
fn changed_target_host_key_stops_forwarded_ssh_before_authentication() {
    run_loopback(ProxyScheme::HttpConnect, Failure::TargetHostKey);
}
