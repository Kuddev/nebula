//! Runtime endpoint discovery and resident server ownership.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Endpoint {
    pub(super) port: u16,
    pub(super) token: String,
}

fn port_file() -> PathBuf {
    crate::display::nebula_data_dir().join("runtime.port")
}

fn legacy_port_file() -> PathBuf {
    crate::display::nebula_data_dir().join("mux.port")
}

pub(super) fn read_endpoint() -> Option<Endpoint> {
    read_endpoint_from(port_file())
}

fn read_endpoint_from(path: PathBuf) -> Option<Endpoint> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut parts = data.split_whitespace();
    Some(Endpoint { port: parts.next()?.parse().ok()?, token: parts.next()?.to_owned() })
}

pub(super) fn endpoint_addr(endpoint: &Endpoint) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port))
}

fn fresh_token() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut a = RandomState::new().build_hasher();
    let mut b = RandomState::new().build_hasher();
    a.write_u32(std::process::id());
    b.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0),
    );
    format!("{:016x}{:016x}", a.finish(), b.finish())
}

/// 普通二次启动并入驻留实例：先恢复/聚焦窗口，再新建一个默认 shell 标签页。
pub fn try_open_default_tab_existing() -> bool {
    try_open_tab_existing(None)
}

/// 后台任务把一行文本作为输入敲进某个 pane（不回车）。
#[cfg(feature = "legacy-shell")]
pub fn dispatch_prompt(proxy: &EventLoopProxy<Event>, pane_id: u64, text: String) {
    let (dispatch, _receiver) = RuntimeDispatch::new(RuntimeCommand::Prompt {
        window_id: None,
        pane_id,
        text,
        submit: false,
    });
    if proxy.send_event(Event::new(EventType::RuntimeControl(dispatch), None)).is_err() {
        warn!("prompt dispatch failed: event loop is gone");
    }
}

/// Explorer 右键或带 `--working-directory` 的启动并入驻留实例。
pub fn try_open_directory_existing(dir: &std::path::Path) -> bool {
    try_open_tab_existing(Some(dir))
}

/// 按“创建新窗口”策略把一次普通启动交给驻留进程。
///
/// 仍先发送 ATTACH，保证隐藏驻留进程被唤醒；真正的窗口由同一 GPUI App
/// 创建，避免第二个进程争抢 runtime.port 和托盘所有权。
pub fn try_open_window_existing(dir: Option<&std::path::Path>) -> bool {
    if legacy_request("ATTACH").is_none() {
        return false;
    }
    let params = dir.map_or_else(|| json!({}), |dir| json!({ "cwd": dir }));
    cli::request_once("window.create", params, IO_TIMEOUT)
        .map(|response| response.ok)
        .unwrap_or(false)
}

fn try_open_tab_existing(dir: Option<&std::path::Path>) -> bool {
    if legacy_request("ATTACH").is_none() {
        return false;
    }
    // ATTACH 与 tab.new 落到同一事件队列，窗口先恢复，新标签随后创建。
    let params = dir.map_or_else(|| json!({}), |dir| json!({ "cwd": dir }));
    cli::request_once("tab.new", params, IO_TIMEOUT).map(|response| response.ok).unwrap_or(false)
}

fn legacy_request(verb: &str) -> Option<()> {
    read_endpoint()
        .and_then(|endpoint| legacy_request_to(verb, &endpoint))
        // 已运行的 pre-v1 版本只发布 mux.port；升级期间仍允许普通启动交接。
        .or_else(|| {
            read_endpoint_from(legacy_port_file())
                .and_then(|endpoint| legacy_request_to(verb, &endpoint))
        })
}

fn legacy_request_to(verb: &str, endpoint: &Endpoint) -> Option<()> {
    let mut stream = TcpStream::connect_timeout(&endpoint_addr(endpoint), CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.write_all(format!("{verb} {}\n", endpoint.token).as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    (line.trim() == "OK").then_some(())
}

/// Resident versioned runtime API server.
pub struct RuntimeServer {
    endpoint: Endpoint,
    port_file: PathBuf,
    _owner_lock: crate::atomic_file::LifetimeFileLock,
}

impl RuntimeServer {
    #[cfg(feature = "legacy-shell")]
    pub fn spawn(proxy: EventLoopProxy<Event>, hub: RuntimeHub) -> Option<Self> {
        Self::spawn_with_sink(EventSink::Winit(proxy), hub)
    }

    /// GPUI variant of [`Self::spawn`], delivered through a callback.
    pub fn spawn_callback(
        on_event: impl Fn(RuntimeCallback) + Send + Sync + 'static,
        hub: RuntimeHub,
    ) -> Option<Self> {
        Self::spawn_with_sink(EventSink::Callback(Arc::new(on_event)), hub)
    }

    fn spawn_with_sink(sink: EventSink, hub: RuntimeHub) -> Option<Self> {
        let path = port_file();
        let owner_lock = match crate::atomic_file::try_lifetime_lock(&path) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                info!("Runtime API owner is starting or already running; staying client-only");
                return None;
            },
            Err(error) => {
                warn!("Runtime API: cannot lock {path:?}: {error}; control plane disabled");
                return None;
            },
        };
        if read_endpoint().and_then(|endpoint| legacy_request_to("PING", &endpoint)).is_some() {
            info!("Runtime API server already running; this instance stays client-only");
            return None;
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        let endpoint = Endpoint { port: listener.local_addr().ok()?.port(), token: fresh_token() };
        let contents = format!("{} {} {}\n", endpoint.port, endpoint.token, PROTOCOL_VERSION);
        if crate::atomic_file::write(&path, contents.as_bytes()).is_err() {
            warn!("Runtime API: cannot write {path:?}; control plane disabled");
            return None;
        }

        let server_token = endpoint.token.clone();
        let spawned = std::thread::Builder::new()
            .name("nebula-runtime-api".into())
            .spawn(move || serve(listener, server_token, sink, hub))
            .is_ok();
        spawned.then(|| Self { endpoint, port_file: path, _owner_lock: owner_lock })
    }
}

impl Drop for RuntimeServer {
    fn drop(&mut self) {
        // Do not delete another process's newer discovery record.
        if read_endpoint().as_ref() == Some(&self.endpoint) {
            let _ = std::fs::remove_file(&self.port_file);
        }
    }
}
