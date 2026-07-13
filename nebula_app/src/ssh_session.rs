//! 由 SSH 通道直接驱动的远端终端会话。
//!
//! 远端 Pane 不创建本地伪终端，但继续使用统一的输入、缩放和关闭消息协议，
//! 从而让渲染与键盘处理保持传输层无关。

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use log::{error, warn};
use nebula_terminal::event::{Event as TerminalEvent, WindowSize};
use nebula_terminal::event_loop::{EventLoopSender, Msg, StreamProcessor};
use nebula_terminal::sync::FairMutex;
use nebula_terminal::term::Term;
use russh::client;
use russh::keys::ssh_key;
use russh::{ChannelMsg, Disconnect};

use crate::event::EventProxy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDestination {
    pub original: String,
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshDestination {
    pub fn parse(value: &str) -> io::Result<Self> {
        let original = value.trim().to_owned();
        let address = original.strip_prefix("ssh://").unwrap_or(&original).to_owned();
        let (user, host_port) = address.rsplit_once('@').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "SSH destination requires user@host")
        })?;
        if user.is_empty() || host_port.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH destination is incomplete",
            ));
        }

        let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
            let (host, suffix) = rest.split_once(']').ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid bracketed SSH address")
            })?;
            let port = suffix
                .strip_prefix(':')
                .map(str::parse)
                .transpose()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid SSH port"))?
                .unwrap_or(22);
            (host.to_owned(), port)
        } else if let Some((host, port)) = host_port.rsplit_once(':') {
            if host.contains(':') {
                (host_port.to_owned(), 22)
            } else {
                let port = port
                    .parse()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid SSH port"))?;
                (host.to_owned(), port)
            }
        } else {
            (host_port.to_owned(), 22)
        };
        if host.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "SSH host is empty"));
        }

        Ok(Self { original, user: user.to_owned(), host, port })
    }
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
                        warn!("Failed to store SSH host key: {err}");
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            },
            Err(err) => {
                warn!("SSH host key verification failed: {err}");
                show_host_key_changed(&self.host, self.port, &err.to_string());
                Ok(false)
            },
        }
    }
}

/// 启动密码认证的 SSH Pane。返回 `Unsupported` 时保留兼容路径，
/// 避免尚未覆盖的认证方式在迁移期间失效。
pub fn spawn_password_session(
    destination: SshDestination,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
) -> io::Result<EventLoopSender> {
    let Some(password) = crate::ssh_credentials::load_stored_password(&destination.original)?
    else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no stored password for direct SSH authentication",
        ));
    };
    let (sender, receiver) = EventLoopSender::standalone()?;
    std::thread::Builder::new().name(format!("SSH {}", destination.host)).spawn(move || {
        run_session(destination, password, initial_size, terminal, event_proxy, receiver)
    })?;
    Ok(sender)
}

fn run_session(
    destination: SshDestination,
    mut password: Vec<u8>,
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
    receiver: Receiver<Msg>,
) {
    let runtime =
        match tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build() {
            Ok(runtime) => runtime,
            Err(err) => {
                render_error(&terminal, &event_proxy, &format!("SSH runtime failed: {err}"));
                return;
            },
        };
    runtime.block_on(async move {
        if let Err(err) = run_session_async(
            &destination,
            &password,
            initial_size,
            terminal.clone(),
            event_proxy.clone(),
            receiver,
        )
        .await
        {
            error!("Direct SSH session failed for {}: {err}", destination.original);
            render_error(&terminal, &event_proxy, &format!("SSH connection failed: {err}"));
        }
        password.fill(0);
        terminal.lock().exit();
        event_proxy.send_event(TerminalEvent::Wakeup.into());
    });
}

async fn run_session_async(
    destination: &SshDestination,
    password: &[u8],
    initial_size: WindowSize,
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    event_proxy: EventProxy,
    receiver: Receiver<Msg>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        keepalive_interval: Some(Duration::from_secs(5)),
        keepalive_max: 6,
        ..Default::default()
    });
    let handler = ClientHandler { host: destination.host.clone(), port: destination.port };
    let mut session =
        client::connect(config, (destination.host.as_str(), destination.port), handler).await?;

    let password = String::from_utf8(password.to_vec())?;
    let auth = session.authenticate_password(destination.user.clone(), password).await?;
    if !auth.success() {
        let _ = crate::ssh_credentials::forget_password(&destination.original);
        return Err("password authentication was rejected".into());
    }

    let mut channel = session.channel_open_session().await?;
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
    loop {
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
                Some(ChannelMsg::ExitStatus { .. }) | None => break,
                _ => {},
            },
        }
    }
    let _ = session.disconnect(Disconnect::ByApplication, "", "English").await;
    Ok(())
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

#[cfg(not(windows))]
fn confirm_new_host(_host: &str, _port: u16, _key: &ssh_key::PublicKey) -> bool {
    false
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

#[cfg(not(windows))]
fn show_host_key_changed(_host: &str, _port: u16, _detail: &str) {}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::SshDestination;

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
}
