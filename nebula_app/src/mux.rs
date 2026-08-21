//! Single-instance mux service: the resident Nebula process owns the live
//! sessions (multiplexer-style). A plain second launch hands over to the
//! resident instance over a loopback socket; the resident restores its window
//! and opens a new tab without disturbing detached tabs whose PTYs never
//! stopped. This is what lets a `claude` conversation survive closing the
//! window while a deliberate second launch still has a visible effect.
//!
//! Discovery is a port file (`%APPDATA%\Nebula\mux.port`) holding
//! `<port> <token>`. The token gates the loopback port against OTHER local
//! users (same-user processes can already do anything to us, so this is not
//! trying to be cryptography). The file always describes the newest server;
//! a stale file (crashed process) fails the connect within the timeout and
//! the next launch simply takes the file over — attach degrades, never hangs.
//!
//! Production GUI serving for both shells is [`crate::runtime_api::RuntimeServer`]
//! (`runtime.port`, which also speaks this legacy ATTACH/PING line protocol).
//! This module is the JSON-less callback path used by tests and as a fallback
//! writer of `mux.port`.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use log::{info, warn};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const IO_TIMEOUT: Duration = Duration::from_millis(700);

fn port_file() -> PathBuf {
    crate::display::nebula_data_dir().join("mux.port")
}

/// Printable ~128-bit token from two OS-seeded `RandomState`s. Good enough to
/// stop other local users from poking the loopback port; nothing more.
fn fresh_token() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut a = RandomState::new().build_hasher();
    let mut b = RandomState::new().build_hasher();
    a.write_u32(std::process::id());
    b.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("{:016x}{:016x}", a.finish(), b.finish())
}

fn read_port_file_at(path: &Path) -> Option<(u16, String)> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut parts = data.split_whitespace();
    let port = parts.next()?.parse().ok()?;
    let token = parts.next()?.to_owned();
    Some((port, token))
}

/// One request/response round-trip against the server described by `path`.
pub(crate) fn request_at(path: &Path, verb: &str) -> Option<()> {
    let (port, token) = read_port_file_at(path)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.write_all(format!("{verb} {token}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    (line.trim() == "OK").then_some(())
}

/// The serving side, owned by the resident instance. Dropping it removes the
/// port file (clean exit); a killed process leaves a stale file, handled by
/// the connect timeout on the next launch.
pub struct MuxServer {
    port_file: PathBuf,
}

impl MuxServer {
    /// Start serving attach requests, unless a live server already owns the
    /// port file (then this instance stays client-only and returns `None`).
    #[allow(dead_code)]
    pub fn spawn(proxy: EventLoopProxy<Event>) -> Option<Self> {
        Self::spawn_callback(move || {
            let _ = proxy.send_event(Event::new(EventType::NebulaAttach, None));
        })
    }

    /// ATTACH/PING server that notifies via callback instead of a winit proxy.
    /// Writes `mux.port` under [`crate::display::nebula_data_dir`].
    #[allow(dead_code)]
    pub fn spawn_callback(on_attach: impl Fn() + Send + 'static) -> Option<Self> {
        Self::spawn_callback_at(port_file(), on_attach)
    }

    /// Same protocol as [`Self::spawn_callback`], with an injectable port path
    /// so tests do not depend on `NEBULA_CONFIG_DIR` / `data_dir` OnceLock.
    pub fn spawn_callback_at(path: PathBuf, on_attach: impl Fn() + Send + 'static) -> Option<Self> {
        if request_at(&path, "PING").is_some() {
            info!("Mux server already running; this instance won't serve attach requests");
            return None;
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = fresh_token();
        if std::fs::write(&path, format!("{port} {token}")).is_err() {
            warn!("Mux: cannot write {path:?}; window re-attach disabled");
            return None;
        }
        let spawned = std::thread::Builder::new()
            .name("nebula-mux".into())
            .spawn(move || serve_callback(listener, token, on_attach))
            .is_ok();
        spawned.then(|| Self { port_file: path })
    }
}

impl Drop for MuxServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.port_file);
    }
}

fn serve_callback(listener: TcpListener, token: String, on_attach: impl Fn()) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let verb = parts.next().unwrap_or("");
        if parts.next() != Some(token.as_str()) {
            continue; // Bad/missing token: silent drop.
        }
        match verb {
            "ATTACH" => {
                on_attach();
                let _ = stream.write_all(b"OK\n");
            },
            "PING" => {
                let _ = stream.write_all(b"OK\n");
            },
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn callback_server_writes_port_file_and_answers_ping_and_attach() {
        let dir = std::env::temp_dir().join(format!(
            "nebula-mux-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp mux dir");
        let path = dir.join("mux.port");
        let attached = Arc::new(AtomicUsize::new(0));
        let flag = attached.clone();
        let server = MuxServer::spawn_callback_at(path.clone(), move || {
            flag.fetch_add(1, Ordering::SeqCst);
        })
        .expect("mux callback server");

        assert!(path.is_file(), "port file must exist while the server lives");
        assert!(request_at(&path, "PING").is_some(), "PING must return OK");
        assert!(request_at(&path, "ATTACH").is_some(), "ATTACH must return OK");
        assert_eq!(attached.load(Ordering::SeqCst), 1);

        drop(server);
        assert!(!path.exists(), "Drop must remove the port file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
