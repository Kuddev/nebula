use std::sync::Mutex;

use nebula_terminal::event::EventListener;
use russh::keys::ssh_key::{Algorithm, PrivateKey};
use russh::server::{self, Auth, ChannelOpenHandle, Session};
use russh::{ChannelId, Pty};
use tokio::net::TcpListener;

use super::*;
use crate::ssh_session::route::{ResolvedRoute, RouteTransport};
use crate::ssh_session::{AcquiredSession, NoopSshEventHost, authenticated_route};

#[derive(Clone, Default)]
struct Events {
    exits: Arc<std::sync::atomic::AtomicUsize>,
    stages: Arc<Mutex<Vec<SshStage>>>,
    replies: Option<mpsc::UnboundedSender<Msg>>,
}

impl EventListener for Events {
    fn send_event(&self, event: TerminalEvent) {
        if let TerminalEvent::PtyWrite(reply) = &event
            && let Some(sender) = &self.replies
        {
            sender.send(Msg::Input(reply.as_bytes().to_vec().into())).unwrap();
        }
        if matches!(event, TerminalEvent::Exit) {
            self.exits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl SshEventHost for Events {
    fn ssh_stage(&self, stage: SshStage) {
        self.stages.lock().unwrap().push(stage);
    }
}

fn size() -> WindowSize {
    WindowSize { num_cols: 80, num_lines: 24, cell_width: 8, cell_height: 16 }
}

fn terminal(events: &Events) -> Arc<FairMutex<Term<Events>>> {
    Arc::new(FairMutex::new(Term::new(Default::default(), &size(), events.clone())))
}

fn check(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), future)
            .await
            .expect("SSH regression timed out");
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Open,
    TerminalQuery,
    RejectPty,
    RejectShell,
    DropWithoutStatus,
    ExitAfterEof,
    HangFirstConnection,
}

struct Loopback {
    mode: Mode,
    hang: bool,
    data: mpsc::UnboundedSender<Vec<u8>>,
    channels: Vec<Channel<server::Msg>>,
}

impl server::Handler for Loopback {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        reply.accept().await;
        self.channels.push(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _columns: u32,
        _rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.mode == Mode::RejectPty {
            session.channel_failure(channel)
        } else {
            session.channel_success(channel)
        }
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.mode == Mode::RejectShell {
            return session.channel_failure(channel);
        }
        session.data(channel, &b"welcome before shell confirmation\r\n"[..])?;
        session.channel_success(channel)?;
        match self.mode {
            Mode::TerminalQuery => {
                // The primary DA reply terminates crossterm's capability discovery.
                session.data(channel, &b"\x1b[2J\x1b[?u\x1b[c"[..])?;
            },
            Mode::DropWithoutStatus => {
                session.eof(channel)?;
                session.close(channel)?;
            },
            Mode::ExitAfterEof => {
                session.eof(channel)?;
                session.exit_status_request(channel, 0)?;
                session.close(channel)?;
            },
            _ => {},
        }
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = self.data.send(data.to_vec());
        Ok(())
    }
}

#[test]
fn ssh_first_capability_query_receives_reply_and_input_keeps_flowing() {
    check(async {
        let mut fixture = Fixture::new(Mode::TerminalQuery).await;
        let acquired = fixture.connect().await;
        let (sender, mut input) = mpsc::unbounded_channel();
        let events = Events { replies: Some(sender.clone()), ..Events::default() };
        let options = super::super::terminal_config(nebula_terminal::term::Config {
            suppress_bringup_da1: true,
            conpty_resize: true,
            kitty_keyboard: true,
            ..Default::default()
        });
        assert!(!options.conpty_resize);
        let terminal = Arc::new(FairMutex::new(Term::new(options, &size(), events.clone())));
        let (mut channel, token) = open_shell(&acquired, size(), None, &events).await.unwrap();
        let exchange = async {
            let mut replies = Vec::new();
            while !replies.ends_with(b"\x1b[?6c") {
                replies.extend(fixture.data.recv().await.unwrap());
            }
            assert_eq!(replies, b"\x1b[?0u\x1b[?6c");
            let keys = b"\x1b[A\x1b[B\x03\x1a";
            sender.send(Msg::Input(keys.to_vec().into())).unwrap();
            let mut received = Vec::new();
            while received.len() < keys.len() {
                received.extend(fixture.data.recv().await.unwrap());
            }
            assert_eq!(received, keys);
            sender.send(Msg::Shutdown).unwrap();
        };
        let (result, ()) = tokio::join!(
            pump(&mut channel, token, size(), &terminal, &events, &mut input),
            exchange,
        );
        result.unwrap();
        fixture.forget(&acquired.session).await;
    });
}

#[test]
fn ssh_terminal_options_preserve_preferences_but_disable_conpty_only_behavior() {
    use nebula_terminal::term::{Config, Osc52};
    use nebula_terminal::vte::ansi::{CursorShape, CursorStyle};

    let local = Config {
        suppress_bringup_da1: true,
        conpty_resize: true,
        kitty_keyboard: true,
        scrolling_history: 42,
        semantic_escape_chars: "test".to_owned(),
        osc52: Osc52::Disabled,
        default_cursor_style: CursorStyle { shape: CursorShape::Underline, blinking: true },
        ..Default::default()
    };
    let remote = super::super::terminal_config(local.clone());
    assert!(!remote.suppress_bringup_da1);
    assert!(!remote.conpty_resize);
    assert_eq!(remote.kitty_keyboard, local.kitty_keyboard);
    assert_eq!(remote.scrolling_history, local.scrolling_history);
    assert_eq!(remote.semantic_escape_chars, local.semantic_escape_chars);
    assert_eq!(remote.osc52, local.osc52);
    assert_eq!(remote.default_cursor_style, local.default_cursor_style);

    let (sender, mut input) = mpsc::unbounded_channel();
    let events = Events { replies: Some(sender), ..Default::default() };
    let mut terminal = Term::new(local, &size(), events);
    let mut parser: nebula_terminal::vte::ansi::Processor =
        nebula_terminal::vte::ansi::Processor::new();
    parser.advance(&mut terminal, b"\x1b[c");
    assert!(input.try_recv().is_err(), "the pre-primed local handshake is still suppressed");
    parser.advance(&mut terminal, b"\x1b[c");
    assert!(matches!(input.try_recv(), Ok(Msg::Input(reply)) if reply.as_ref() == b"\x1b[?6c"));
}

#[test]
fn duplicate_ssh_directory_preserves_literal_paths_and_rejects_control_characters() {
    use super::super::initial_remote_cd_command;

    assert_eq!(
        initial_remote_cd_command(Some("/srv/Team's \"App\" ")),
        Some(b"cd '/srv/Team'\\''s \"App\" '\r".to_vec())
    );
    assert_eq!(
        initial_remote_cd_command(Some("/srv/$(whoami);pwd")),
        Some(b"cd '/srv/$(whoami);pwd'\r".to_vec())
    );
    for path in ["/srv/app\n", "/srv/app\r", "\t/srv/app", "/srv/\x1bapp", "", "relative"] {
        assert_eq!(initial_remote_cd_command(Some(path)), None, "{path:?}");
    }
    assert_eq!(initial_remote_cd_command(Some(&format!("/{}", "x".repeat(16 * 1024)))), None);
}

struct Fixture {
    route: ResolvedRoute,
    data: mpsc::UnboundedReceiver<Vec<u8>>,
    task: tokio::task::JoinHandle<()>,
    _directory: tempfile::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(mode: Mode) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        russh::keys::known_hosts::learn_known_hosts_path(
            "127.0.0.1",
            address.port(),
            key.public_key(),
            &known_hosts,
        )
        .unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let (data_tx, data) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            let mut first = true;
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                stream.set_nodelay(true).unwrap();
                let handler = Loopback {
                    mode,
                    hang: first && mode == Mode::HangFirstConnection,
                    data: data_tx.clone(),
                    channels: Vec::new(),
                };
                first = false;
                let config = config.clone();
                connections.spawn(async move {
                    if let Ok(session) = server::run_stream(config, stream, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        let destination = format!("fixture@127.0.0.1:{}", address.port());
        let route = ResolvedRoute {
            destination: SshDestination::parse(&destination).unwrap(),
            profile: crate::ssh_profiles::SshProfiles::default().for_destination(&destination),
            transport: RouteTransport::Direct,
            known_hosts_path: Some(known_hosts),
        };
        Self { route, data, task, _directory: directory }
    }

    async fn connect(&self) -> AcquiredSession {
        authenticated_route(&self.route, None::<&NoopSshEventHost>, false).await.unwrap()
    }

    async fn forget(&self, session: &SharedSession) {
        super::super::evict_pooled_session(&self.route.pool_key(), session).await;
    }
}

#[test]
fn cancellation_stops_startup_before_a_shell_exists() {
    check(async {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (_input, mut cancellation) = input_bridge(receiver);
        let events = Events::default();
        let terminal = terminal(&events);
        sender.send(Msg::Shutdown).unwrap();
        drive(std::future::pending(), &mut cancellation, &terminal, &events).await;
        assert_eq!(events.exits.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(events.stages.lock().unwrap().is_empty());
    });
}

#[test]
fn handshake_budget_excludes_interactive_host_confirmation() {
    check(async {
        let handshake = Handshake::default();
        let operation = async {
            assert!(
                handshake
                    .confirm(async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        true
                    })
                    .await
            );
            Ok(())
        };
        handshake.connect_with_budget(operation, Duration::from_millis(30)).await.unwrap();
    });
}

#[test]
fn cancelled_handshake_cannot_leave_a_pending_host_prompt() {
    check(async {
        let handshake = Handshake::default();
        {
            let connection = handshake.connect(std::future::pending::<Result<(), russh::Error>>());
            tokio::pin!(connection);
            tokio::select! {
                _ = &mut connection => panic!("handshake must still be waiting"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
        assert!(!handshake.confirm(std::future::pending()).await);
    });
}

#[test]
fn rejected_pty_and_shell_do_not_become_ready() {
    check(async {
        for mode in [Mode::RejectPty, Mode::RejectShell] {
            let fixture = Fixture::new(mode).await;
            let acquired = fixture.connect().await;
            let events = Events::default();
            let result = open_shell(&acquired, size(), None, &events).await;
            assert!(result.is_err());
            assert!(
                !events.stages.lock().unwrap().iter().any(|stage| matches!(stage, SshStage::Ready))
            );
            fixture.forget(&acquired.session).await;
        }
    });
}

#[test]
fn shell_confirmation_preserves_early_output_and_remote_directory() {
    check(async {
        let mut fixture = Fixture::new(Mode::Open).await;
        let acquired = fixture.connect().await;
        let (channel, _) =
            open_shell(&acquired, size(), Some("/srv/Team's App "), &Events::default())
                .await
                .unwrap();
        assert!(channel.pending.iter().any(
            |message| matches!(message, ChannelMsg::Data { data } if data.starts_with(b"welcome"))
        ));
        let command = fixture.data.recv().await.unwrap();
        assert_eq!(command, b"cd '/srv/Team'\\''s App '\r");
        drop(channel);
        fixture.forget(&acquired.session).await;
    });
}

#[test]
fn unexpected_disconnect_preserves_pane_but_explicit_exit_closes_it() {
    check(async {
        for mode in [Mode::DropWithoutStatus, Mode::ExitAfterEof] {
            let fixture = Fixture::new(mode).await;
            let acquired = fixture.connect().await;
            let events = Events::default();
            let terminal = terminal(&events);
            let (mut channel, token) = open_shell(&acquired, size(), None, &events).await.unwrap();
            let (_input_tx, mut input) = mpsc::unbounded_channel();
            let result = pump(&mut channel, token, size(), &terminal, &events, &mut input).await;
            assert_eq!(result.is_ok(), mode == Mode::ExitAfterEof);
            finish(result, &terminal, &events);
            assert_eq!(
                events.exits.load(std::sync::atomic::Ordering::Relaxed),
                usize::from(mode == Mode::ExitAfterEof)
            );
            assert_eq!(
                events
                    .stages
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|stage| matches!(stage, SshStage::Failed(_))),
                mode == Mode::DropWithoutStatus
            );
            fixture.forget(&acquired.session).await;
        }
    });
}

#[test]
fn hung_channel_does_not_lock_reuse_and_fresh_connection_can_retry() {
    check(async {
        let fixture = Fixture::new(Mode::HangFirstConnection).await;
        let acquired = fixture.connect().await;
        let events = Events::default();
        let opening =
            open_shell_with_budget(&acquired, size(), None, &events, Duration::from_millis(100));
        let reuse = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let reused = fixture.connect().await;
            assert!(Arc::ptr_eq(&reused.session, &acquired.session));
        };
        let (result, ()) = tokio::join!(opening, reuse);
        assert!(result.is_err());
        fixture.forget(&acquired.session).await;
        let replacement = fixture.connect().await;
        assert!(!Arc::ptr_eq(&replacement.session, &acquired.session));
        let (channel, _) = open_shell(&replacement, size(), None, &events).await.unwrap();
        assert!(!super::super::evict_pooled_session(&replacement.key, &acquired.session).await);
        let reused = fixture.connect().await;
        assert!(Arc::ptr_eq(&reused.session, &replacement.session));
        drop(channel);
        fixture.forget(&replacement.session).await;
    });
}
