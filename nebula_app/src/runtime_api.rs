//! Versioned local control plane shared by Nebula's CLI, agents, and future plugins.
//!
//! The transport is loopback TCP plus a per-instance discovery token. Requests
//! are JSON Lines so clients in any language can use the API without linking
//! Rust types. GUI and PTY mutations are dispatched onto winit's event thread;
//! transport workers only validate, wait, and serialize.

use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader, Error as IoError, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::{info, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use winit::event_loop::EventLoopProxy;

use crate::cli::{
    ControlCommand as CliCommand, ControlOptions, ControlSplitDirection, ControlWaitState,
};
use crate::event::{Event, EventType};

pub const PROTOCOL_NAME: &str = "nebula.runtime";
pub const PROTOCOL_VERSION: u16 = 1;
pub const SUPPORTED_VERSIONS: &[u16] = &[PROTOCOL_VERSION];

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_PROMPT_BYTES: usize = 32 * 1024;
const MAX_CLIENTS: usize = 64;
const MAX_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    pub protocol: String,
    pub version: u16,
    pub id: String,
    pub token: String,
    pub method: String,
    #[serde(default = "empty_object")]
    pub params: Value,
}

impl ApiRequest {
    fn new(token: String, method: impl Into<String>, params: Value) -> Self {
        let sequence = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            id: format!("{}-{sequence}", std::process::id()),
            token,
            method: method.into(),
            params,
        }
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiResponse {
    pub protocol: String,
    pub version: u16,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl ApiResponse {
    fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(id: impl Into<String>, error: ApiError) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiEvent {
    pub protocol: String,
    pub version: u16,
    pub event: String,
    pub revision: u64,
    pub data: RuntimeSnapshot,
}

impl ApiEvent {
    fn snapshot(snapshot: RuntimeSnapshot) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            event: "runtime.snapshot".to_owned(),
            revision: snapshot.revision,
            data: snapshot,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), details: None }
    }

    pub(crate) fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new("invalid_params", message)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskState {
    Idle,
    Running,
    WaitingInput,
    Attention,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSplitDirection {
    LeftRight,
    TopBottom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeSnapshot {
    pub revision: u64,
    pub process_id: u32,
    pub app_version: String,
    pub protocol_version: u16,
    pub detached_windows: usize,
    pub windows: Vec<RuntimeWindow>,
}

impl RuntimeSnapshot {
    pub(crate) fn new(detached_windows: usize, windows: Vec<RuntimeWindow>) -> Self {
        Self {
            revision: 0,
            process_id: std::process::id(),
            app_version: env!("VERSION").to_owned(),
            protocol_version: PROTOCOL_VERSION,
            detached_windows,
            windows,
        }
    }

    fn pane(&self, window_id: Option<u64>, pane_id: u64) -> Result<&RuntimePane, ApiError> {
        let matches: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window_id.is_none_or(|id| window.id == id))
            .flat_map(|window| window.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .filter(|pane| pane.id == pane_id)
            .collect();
        match matches.as_slice() {
            [pane] => Ok(*pane),
            [] => Err(ApiError::new(
                "target_not_found",
                format!("pane {pane_id} was not found in the requested window"),
            )),
            _ => Err(ApiError::new(
                "ambiguous_target",
                format!("pane id {pane_id} exists in multiple windows; provide window_id"),
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeWindow {
    pub id: u64,
    pub focused: bool,
    pub session_exempt: bool,
    pub active_tab: usize,
    pub focused_pane_id: Option<u64>,
    pub tabs: Vec<RuntimeTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTab {
    pub index: usize,
    pub active: bool,
    pub label: String,
    pub kind: String,
    pub bell: bool,
    pub focused_pane_id: Option<u64>,
    pub layout: Option<RuntimeLayout>,
    pub panes: Vec<RuntimePane>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeLayout {
    Pane {
        pane_id: u64,
    },
    Split {
        direction: RuntimeSplitDirection,
        ratio: f32,
        first: Box<RuntimeLayout>,
        second: Box<RuntimeLayout>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePane {
    pub id: u64,
    pub active: bool,
    pub title: String,
    pub cwd: String,
    pub branch: String,
    pub ssh_destination: Option<String>,
    pub running_program: Option<String>,
    pub task_state: RuntimeTaskState,
    /// Monotonic count of this pane's task-state transitions. A state value on
    /// its own cannot answer "did anything happen since I submitted?", so
    /// callers that sent work compare against a baseline captured at submit
    /// time. Stamped by [`RuntimeHub::publish`]; projection callers leave it 0.
    #[serde(default)]
    pub state_change_seq: u64,
}

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Snapshot,
    NewWindow,
    Focus { window_id: Option<u64>, pane_id: Option<u64> },
    NewTab { window_id: Option<u64> },
    Split { window_id: Option<u64>, direction: RuntimeSplitDirection },
    Prompt { window_id: Option<u64>, pane_id: u64, text: String, submit: bool },
}

#[derive(Debug)]
pub struct RuntimeDispatch {
    pub command: RuntimeCommand,
    reply: SyncSender<Result<Value, ApiError>>,
}

impl RuntimeDispatch {
    fn new(command: RuntimeCommand) -> (Arc<Self>, Receiver<Result<Value, ApiError>>) {
        let (reply, receiver) = mpsc::sync_channel(1);
        (Arc::new(Self { command, reply }), receiver)
    }

    pub(crate) fn respond(&self, response: Result<Value, ApiError>) {
        let _ = self.reply.send(response);
    }
}

#[derive(Clone, Default)]
pub struct RuntimeHub {
    inner: Arc<Mutex<HubState>>,
}

/// Carry each pane's transition counter forward, incrementing only where the
/// task state actually changed. Pane ids are window-local, so the identity key
/// is (window, pane) — matching on the pane id alone would let a same-numbered
/// pane in another window inherit an unrelated counter.
fn stamp_state_change_seq(previous: Option<&RuntimeSnapshot>, next: &mut RuntimeSnapshot) {
    let mut before = std::collections::HashMap::new();
    if let Some(previous) = previous {
        for window in &previous.windows {
            for tab in &window.tabs {
                for pane in &tab.panes {
                    before.insert((window.id, pane.id), (pane.task_state, pane.state_change_seq));
                }
            }
        }
    }

    for window in &mut next.windows {
        let window_id = window.id;
        for tab in &mut window.tabs {
            for pane in &mut tab.panes {
                pane.state_change_seq = match before.get(&(window_id, pane.id)) {
                    Some((state, seq)) if *state == pane.task_state => *seq,
                    Some((_, seq)) => seq.saturating_add(1),
                    // A pane observed for the first time starts at 1, so a
                    // baseline of 0 always reads as "not yet seen".
                    None => 1,
                };
            }
        }
    }
}

#[derive(Default)]
struct HubState {
    current: Option<RuntimeSnapshot>,
    next_subscription: u64,
    subscribers: Vec<(u64, SyncSender<RuntimeSnapshot>)>,
}

impl RuntimeHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Publish only semantic changes. Repeated event-loop wakeups do not burn
    /// revisions or flood subscribers with identical snapshots.
    pub(crate) fn publish(&self, mut snapshot: RuntimeSnapshot) -> RuntimeSnapshot {
        let mut state = self.lock();
        // Stamp per-pane transition counters before the dedup compare. Panes
        // whose state is unchanged keep their previous counter, so an
        // otherwise-identical projection still compares equal here.
        stamp_state_change_seq(state.current.as_ref(), &mut snapshot);
        if let Some(current) = &state.current {
            snapshot.revision = current.revision;
            if current == &snapshot {
                return current.clone();
            }
            snapshot.revision = current.revision.saturating_add(1);
        } else {
            snapshot.revision = 1;
        }

        state.current = Some(snapshot.clone());
        state.subscribers.retain(|(_, sender)| match sender.try_send(snapshot.clone()) {
            Ok(()) => true,
            // A subscriber that cannot keep up is disconnected instead of
            // back-pressuring the GUI event thread.
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
        snapshot
    }

    fn subscribe(&self) -> (u64, Option<RuntimeSnapshot>, Receiver<RuntimeSnapshot>) {
        let (sender, receiver) = mpsc::sync_channel(16);
        let mut state = self.lock();
        state.next_subscription = state.next_subscription.saturating_add(1);
        let id = state.next_subscription;
        let current = state.current.clone();
        state.subscribers.push((id, sender));
        (id, current, receiver)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetParams {
    #[serde(default)]
    window_id: Option<u64>,
    #[serde(default)]
    pane_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowParams {
    #[serde(default)]
    window_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitParams {
    #[serde(default)]
    window_id: Option<u64>,
    direction: RuntimeSplitDirection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    text: String,
    #[serde(default = "default_true")]
    submit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscribeParams {
    #[serde(default)]
    since_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    state: RuntimeWaitState,
    timeout_ms: u64,
    /// Baseline transition counter captured when the client submitted work.
    /// When present, the wait additionally requires the pane's counter to
    /// advance past it — so a pane that was already in the target state does
    /// not satisfy "wait until it settles again".
    #[serde(default)]
    after_seq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeWaitState {
    Idle,
    Running,
    WaitingInput,
    Attention,
    Finished,
    Failed,
    Settled,
}

fn default_true() -> bool {
    true
}

impl RuntimeCommand {
    fn from_request(request: &ApiRequest) -> Result<Self, ApiError> {
        match request.method.as_str() {
            "runtime.snapshot" => Ok(Self::Snapshot),
            "window.create" => Ok(Self::NewWindow),
            "window.focus" => {
                let params: TargetParams = parse_params(&request.params)?;
                Ok(Self::Focus { window_id: params.window_id, pane_id: params.pane_id })
            },
            "tab.new" => {
                let params: WindowParams = parse_params(&request.params)?;
                Ok(Self::NewTab { window_id: params.window_id })
            },
            "pane.split" => {
                let params: SplitParams = parse_params(&request.params)?;
                Ok(Self::Split { window_id: params.window_id, direction: params.direction })
            },
            "pane.prompt" => {
                let params: PromptParams = parse_params(&request.params)?;
                validate_prompt(&params.text)?;
                Ok(Self::Prompt {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    text: params.text,
                    submit: params.submit,
                })
            },
            method => Err(ApiError::new(
                "method_not_found",
                format!("runtime API method {method:?} does not exist"),
            )),
        }
    }
}

fn parse_params<T: DeserializeOwned>(value: &Value) -> Result<T, ApiError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ApiError::invalid_params(format!("invalid method parameters: {error}")))
}

pub(crate) fn validate_prompt(text: &str) -> Result<(), ApiError> {
    if text.is_empty() {
        return Err(ApiError::invalid_params("prompt text must not be empty"));
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(ApiError::invalid_params(format!(
            "prompt text exceeds the {MAX_PROMPT_BYTES}-byte limit"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "prompt text contains control characters; pane.prompt accepts one plain-text line",
        ));
    }
    Ok(())
}

/// A wait is satisfied only when the pane both reads as the requested state and
/// has moved past the caller's baseline. Without the counter check, waiting on
/// an already-idle pane returns immediately and the caller concludes its work
/// finished before the shell even saw it.
fn wait_matches(pane: &RuntimePane, expected: RuntimeWaitState, after_seq: Option<u64>) -> bool {
    after_seq.is_none_or(|baseline| pane.state_change_seq > baseline)
        && wait_state_matches(pane.task_state, expected)
}

fn wait_state_matches(actual: RuntimeTaskState, expected: RuntimeWaitState) -> bool {
    match expected {
        RuntimeWaitState::Idle => actual == RuntimeTaskState::Idle,
        RuntimeWaitState::Running => actual == RuntimeTaskState::Running,
        RuntimeWaitState::WaitingInput => actual == RuntimeTaskState::WaitingInput,
        RuntimeWaitState::Attention => actual == RuntimeTaskState::Attention,
        RuntimeWaitState::Finished => actual == RuntimeTaskState::Finished,
        RuntimeWaitState::Failed => actual == RuntimeTaskState::Failed,
        RuntimeWaitState::Settled => actual != RuntimeTaskState::Running,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    port: u16,
    token: String,
}

fn port_file() -> PathBuf {
    crate::display::nebula_data_dir().join("runtime.port")
}

fn legacy_port_file() -> PathBuf {
    crate::display::nebula_data_dir().join("mux.port")
}

fn read_endpoint() -> Option<Endpoint> {
    read_endpoint_from(port_file())
}

fn read_endpoint_from(path: PathBuf) -> Option<Endpoint> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut parts = data.split_whitespace();
    Some(Endpoint { port: parts.next()?.parse().ok()?, token: parts.next()?.to_owned() })
}

fn endpoint_addr(endpoint: &Endpoint) -> SocketAddr {
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

/// Hand a plain launch to the resident instance. This legacy verb remains
/// supported while both paths share the same authenticated endpoint.
pub fn try_attach_existing() -> bool {
    legacy_request("ATTACH").is_some()
}

fn legacy_request(verb: &str) -> Option<()> {
    read_endpoint()
        .and_then(|endpoint| legacy_request_to(verb, &endpoint))
        // Upgrade compatibility: an already-running pre-v1 Nebula still
        // publishes mux.port. A plain launch can attach to it and exit; a new
        // resident process writes runtime.port after the old process ends.
        .or_else(|| {
            read_endpoint_from(legacy_port_file())
                .and_then(|endpoint| legacy_request_to(verb, &endpoint))
        })
}

fn legacy_request_to(verb: &str, endpoint: &Endpoint) -> Option<()> {
    let mut stream = TcpStream::connect_timeout(&endpoint_addr(&endpoint), CONNECT_TIMEOUT).ok()?;
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
}

impl RuntimeServer {
    pub fn spawn(proxy: EventLoopProxy<Event>, hub: RuntimeHub) -> Option<Self> {
        if read_endpoint().and_then(|endpoint| legacy_request_to("PING", &endpoint)).is_some() {
            info!("Runtime API server already running; this instance stays client-only");
            return None;
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        let endpoint = Endpoint { port: listener.local_addr().ok()?.port(), token: fresh_token() };
        let path = port_file();
        let contents = format!("{} {} {}\n", endpoint.port, endpoint.token, PROTOCOL_VERSION);
        if crate::atomic_file::write(&path, contents.as_bytes()).is_err() {
            warn!("Runtime API: cannot write {path:?}; control plane disabled");
            return None;
        }

        let server_token = endpoint.token.clone();
        let spawned = std::thread::Builder::new()
            .name("nebula-runtime-api".into())
            .spawn(move || serve(listener, server_token, proxy, hub))
            .is_ok();
        spawned.then(|| Self { endpoint, port_file: path })
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

fn serve(listener: TcpListener, token: String, proxy: EventLoopProxy<Event>, hub: RuntimeHub) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let active = active.clone();
        let token = token.clone();
        let proxy = proxy.clone();
        let hub = hub.clone();
        let _ = std::thread::Builder::new().name("nebula-runtime-client".into()).spawn(move || {
            let _guard = ActiveClient(active);
            if let Err(error) = handle_connection(stream, &token, &proxy, &hub) {
                log::debug!("Runtime API client disconnected: {error}");
            }
        });
    }
}

struct ActiveClient(Arc<AtomicUsize>);

impl Drop for ActiveClient {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection(
    mut stream: TcpStream,
    token: &str,
    proxy: &EventLoopProxy<Event>,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let reader = BufReader::new(stream.try_clone()?);
    let mut limited = reader.take((MAX_REQUEST_BYTES + 1) as u64);
    let mut line = String::new();
    limited.read_line(&mut line)?;
    if line.len() > MAX_REQUEST_BYTES {
        return write_response(
            &mut stream,
            &ApiResponse::failure(
                "unknown",
                ApiError::new("request_too_large", "request too large"),
            ),
        );
    }

    if !line.trim_start().starts_with('{') {
        return handle_legacy(&mut stream, &line, token, proxy);
    }

    let raw: Value = match serde_json::from_str(&line) {
        Ok(raw) => raw,
        Err(error) => {
            return write_response(
                &mut stream,
                &ApiResponse::failure(
                    "unknown",
                    ApiError::new("invalid_request", format!("invalid JSON: {error}")),
                ),
            );
        },
    };
    // Authentication precedes detailed validation so an unauthenticated local
    // process cannot use parse errors as a protocol oracle.
    if raw.get("token").and_then(Value::as_str) != Some(token) {
        return Ok(());
    }
    let id = raw.get("id").and_then(Value::as_str).unwrap_or("unknown").to_owned();
    let request: ApiRequest = match serde_json::from_value(raw) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                &mut stream,
                &ApiResponse::failure(
                    id,
                    ApiError::new("invalid_request", format!("invalid request envelope: {error}")),
                ),
            );
        },
    };

    if request.protocol != PROTOCOL_NAME || request.version != PROTOCOL_VERSION {
        let error = ApiError::new(
            "protocol_version_mismatch",
            format!(
                "requested protocol {:?} v{}; this runtime supports {} v{}",
                request.protocol, request.version, PROTOCOL_NAME, PROTOCOL_VERSION
            ),
        )
        .details(json!({ "supported_versions": SUPPORTED_VERSIONS }));
        return write_response(&mut stream, &ApiResponse::failure(request.id, error));
    }

    match request.method.as_str() {
        "runtime.describe" => {
            write_response(&mut stream, &ApiResponse::success(request.id, runtime_description()))
        },
        "events.subscribe" => subscribe_connection(&mut stream, request, hub),
        "pane.wait" => wait_connection(&mut stream, request, hub),
        _ => dispatch_connection(&mut stream, request, proxy),
    }
}

fn handle_legacy(
    stream: &mut TcpStream,
    line: &str,
    token: &str,
    proxy: &EventLoopProxy<Event>,
) -> Result<(), IoError> {
    let mut parts = line.split_whitespace();
    let verb = parts.next().unwrap_or("");
    if parts.next() != Some(token) {
        return Ok(());
    }
    match verb {
        "ATTACH" => {
            let _ = proxy.send_event(Event::new(EventType::NebulaAttach, None));
            stream.write_all(b"OK\n")
        },
        "PING" => stream.write_all(b"OK\n"),
        _ => Ok(()),
    }
}

fn runtime_description() -> Value {
    json!({
        "app_version": env!("VERSION"),
        "protocol": PROTOCOL_NAME,
        "protocol_version": PROTOCOL_VERSION,
        "supported_versions": SUPPORTED_VERSIONS,
        "schema": "docs/runtime-api-v1.schema.json",
        "capabilities": [
            "runtime.describe",
            "runtime.snapshot",
            "events.subscribe",
            "window.create",
            "window.focus",
            "tab.new",
            "pane.split",
            "pane.prompt",
            "pane.wait"
        ],
        // Additive params cannot be detected from `capabilities`: an older
        // build ignores an unknown `after_seq` and still races. Clients that
        // need the guarantee should require the feature string.
        "features": [
            "pane.wait.after_seq"
        ]
    })
}

fn subscribe_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: SubscribeParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let (subscription_id, current, receiver) = hub.subscribe();
    write_response(
        stream,
        &ApiResponse::success(
            request.id,
            json!({
                "subscription_id": subscription_id,
                "current_revision": current.as_ref().map_or(0, |snapshot| snapshot.revision)
            }),
        ),
    )?;
    stream.set_write_timeout(None)?;
    if let Some(snapshot) = current
        && params.since_revision.is_none_or(|revision| snapshot.revision > revision)
    {
        write_json_line(stream, &ApiEvent::snapshot(snapshot))?;
    }
    while let Ok(snapshot) = receiver.recv() {
        write_json_line(stream, &ApiEvent::snapshot(snapshot))?;
    }
    Ok(())
}

fn wait_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: WaitParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let timeout = Duration::from_millis(params.timeout_ms);
    if timeout.is_zero() || timeout > MAX_WAIT {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::invalid_params("timeout_ms must be between 1 and 86400000"),
            ),
        );
    }

    let (_, current, receiver) = hub.subscribe();
    // Remember what the pane last looked like so a timeout can report why it
    // never matched rather than only that it did not.
    let mut observed = None;
    if let Some(snapshot) = current {
        match snapshot.pane(params.window_id, params.pane_id) {
            Ok(pane) if wait_matches(pane, params.state, params.after_seq) => {
                return write_response(
                    stream,
                    &ApiResponse::success(
                        request.id,
                        serde_json::to_value(snapshot).map_err(IoError::other)?,
                    ),
                );
            },
            Ok(pane) => observed = Some((pane.task_state, pane.state_change_seq)),
            Err(_) => (),
        }
    }

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let snapshot = match receiver.recv_timeout(remaining) {
            Ok(snapshot) => snapshot,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        };
        match snapshot.pane(params.window_id, params.pane_id) {
            Ok(pane) if wait_matches(pane, params.state, params.after_seq) => {
                return write_response(
                    stream,
                    &ApiResponse::success(
                        request.id,
                        serde_json::to_value(snapshot).map_err(IoError::other)?,
                    ),
                );
            },
            Ok(pane) => observed = Some((pane.task_state, pane.state_change_seq)),
            Err(_) => (),
        }
    }

    let detail = match observed {
        Some((state, seq)) => format!("last observed state {state:?} at state_change_seq {seq}"),
        None => "the pane was never present in a published snapshot".to_owned(),
    };
    write_response(
        stream,
        &ApiResponse::failure(
            request.id,
            ApiError::new(
                "timeout",
                format!(
                    "pane {} did not reach the requested state before timeout; {detail}",
                    params.pane_id
                ),
            )
            .details(json!({
                "pane_id": params.pane_id,
                "after_seq": params.after_seq,
                "observed_state_change_seq": observed.map(|(_, seq)| seq)
            })),
        ),
    )
}

fn dispatch_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    proxy: &EventLoopProxy<Event>,
) -> Result<(), IoError> {
    let command = match RuntimeCommand::from_request(&request) {
        Ok(command) => command,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let (dispatch, receiver) = RuntimeDispatch::new(command);
    if proxy.send_event(Event::new(EventType::RuntimeControl(dispatch), None)).is_err() {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new("runtime_unavailable", "Nebula's event loop is not available"),
            ),
        );
    }
    let response = match receiver.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(result)) => ApiResponse::success(request.id, result),
        Ok(Err(error)) => ApiResponse::failure(request.id, error),
        Err(_) => ApiResponse::failure(
            request.id,
            ApiError::new("runtime_timeout", "runtime event thread did not answer in time"),
        ),
    };
    write_response(stream, &response)
}

fn write_response(stream: &mut TcpStream, response: &ApiResponse) -> Result<(), IoError> {
    write_json_line(stream, response)
}

fn write_json_line<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<(), IoError> {
    serde_json::to_writer(&mut *stream, value).map_err(IoError::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn client_stream(
    endpoint: &Endpoint,
    request: &ApiRequest,
    timeout: Option<Duration>,
) -> Result<TcpStream, IoError> {
    let mut stream = TcpStream::connect_timeout(&endpoint_addr(&endpoint), CONNECT_TIMEOUT)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    serde_json::to_writer(&mut stream, request).map_err(IoError::other)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    Ok(stream)
}

fn request_once(
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<ApiResponse, Box<dyn Error>> {
    let endpoint =
        read_endpoint().ok_or_else(|| CliError("no resident Nebula runtime found".into()))?;
    let request = ApiRequest::new(endpoint.token.clone(), method, params);
    let stream = client_stream(&endpoint, &request, Some(timeout))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    if line.is_empty() {
        return Err(CliError("runtime closed the connection without a response".into()).into());
    }
    Ok(serde_json::from_str(&line)?)
}

fn print_response(response: &ApiResponse, pretty: bool) -> Result<(), Box<dyn Error>> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(response)?);
    } else {
        println!("{}", serde_json::to_string(response)?);
    }
    if response.ok {
        Ok(())
    } else {
        let error =
            response.error.as_ref().map_or("runtime request failed", |error| &error.message);
        Err(CliError(error.to_owned()).into())
    }
}

/// CLI adapter. It deliberately speaks the same serialized protocol as any
/// external client instead of calling Processor helpers in-process.
pub fn run_cli(options: ControlOptions) -> Result<(), Box<dyn Error>> {
    if options.timeout_ms == 0 || Duration::from_millis(options.timeout_ms) > MAX_WAIT {
        return Err(CliError("--timeout-ms must be between 1 and 86400000".into()).into());
    }
    let timeout = Duration::from_millis(options.timeout_ms);
    match options.command {
        CliCommand::Describe => {
            let response = request_once("runtime.describe", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Snapshot => {
            let response = request_once("runtime.snapshot", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Subscribe { since } => subscribe_cli(since, timeout),
        CliCommand::NewWindow => {
            let response = request_once("window.create", json!({}), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Focus { window, pane } => {
            let response = request_once(
                "window.focus",
                json!({ "window_id": window, "pane_id": pane }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::NewTab { window } => {
            let response = request_once("tab.new", json!({ "window_id": window }), timeout)?;
            print_response(&response, options.pretty)
        },
        CliCommand::Split { window, direction } => {
            let direction = match direction {
                ControlSplitDirection::Right => RuntimeSplitDirection::LeftRight,
                ControlSplitDirection::Down => RuntimeSplitDirection::TopBottom,
            };
            let response = request_once(
                "pane.split",
                json!({ "window_id": window, "direction": direction }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Prompt { window, pane, text, no_submit, wait } => {
            let response = request_once(
                "pane.prompt",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "text": text,
                    "submit": !no_submit
                }),
                timeout,
            )?;
            if !response.ok || wait.is_none() {
                return print_response(&response, options.pretty);
            }
            // The prompt response carries the snapshot taken immediately after
            // submission. Using its counter as the baseline is what makes the
            // follow-up wait mean "settled again", not "already settled".
            let baseline = pane_state_change_seq(&response, window, pane);
            wait_cli(window, pane, wait.expect("checked above"), baseline, timeout, options.pretty)
        },
        CliCommand::Wait { window, pane, state, after_seq } => {
            wait_cli(window, pane, state, after_seq, timeout, options.pretty)
        },
    }
}

/// Dig a pane's transition counter out of a command response's embedded
/// snapshot. Returns `None` when the shape is unexpected, which degrades the
/// follow-up wait to plain state matching rather than failing the command.
fn pane_state_change_seq(
    response: &ApiResponse,
    window_id: Option<u64>,
    pane_id: u64,
) -> Option<u64> {
    let snapshot = response.result.as_ref()?.get("snapshot")?;
    let snapshot: RuntimeSnapshot = serde_json::from_value(snapshot.clone()).ok()?;
    snapshot.pane(window_id, pane_id).ok().map(|pane| pane.state_change_seq)
}

fn wait_cli(
    window: Option<u64>,
    pane: u64,
    state: ControlWaitState,
    after_seq: Option<u64>,
    timeout: Duration,
    pretty: bool,
) -> Result<(), Box<dyn Error>> {
    let state = match state {
        ControlWaitState::Idle => "idle",
        ControlWaitState::Running => "running",
        ControlWaitState::WaitingInput => "waiting_input",
        ControlWaitState::Attention => "attention",
        ControlWaitState::Finished => "finished",
        ControlWaitState::Failed => "failed",
        ControlWaitState::Settled => "settled",
    };
    let response = request_once(
        "pane.wait",
        json!({
            "window_id": window,
            "pane_id": pane,
            "state": state,
            "timeout_ms": timeout.as_millis() as u64,
            "after_seq": after_seq
        }),
        timeout.saturating_add(Duration::from_secs(1)),
    )?;
    print_response(&response, pretty)
}

fn subscribe_cli(since: Option<u64>, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let endpoint =
        read_endpoint().ok_or_else(|| CliError("no resident Nebula runtime found".into()))?;
    let request = ApiRequest::new(
        endpoint.token.clone(),
        "events.subscribe",
        json!({ "since_revision": since }),
    );
    let stream = client_stream(&endpoint, &request, Some(timeout))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(
            CliError("runtime closed the subscription without an acknowledgement".into()).into()
        );
    }
    print!("{line}");
    std::io::stdout().flush()?;
    reader.get_mut().set_read_timeout(None)?;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        print!("{line}");
        std::io::stdout().flush()?;
    }
}

#[derive(Debug)]
struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CliError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(state: RuntimeTaskState) -> RuntimeSnapshot {
        RuntimeSnapshot::new(
            0,
            vec![RuntimeWindow {
                id: 7,
                focused: true,
                session_exempt: false,
                active_tab: 0,
                focused_pane_id: Some(3),
                tabs: vec![RuntimeTab {
                    index: 0,
                    active: true,
                    label: "test".into(),
                    kind: "shell".into(),
                    bell: false,
                    focused_pane_id: Some(3),
                    layout: Some(RuntimeLayout::Pane { pane_id: 3 }),
                    panes: vec![RuntimePane {
                        id: 3,
                        active: true,
                        title: "shell".into(),
                        cwd: "D:/work".into(),
                        branch: "main".into(),
                        ssh_destination: None,
                        running_program: None,
                        task_state: state,
                        state_change_seq: 0,
                    }],
                }],
            }],
        )
    }

    #[test]
    fn hub_revisions_change_only_when_semantic_state_changes() {
        let hub = RuntimeHub::new();
        let first = hub.publish(snapshot(RuntimeTaskState::Idle));
        let duplicate = hub.publish(snapshot(RuntimeTaskState::Idle));
        let changed = hub.publish(snapshot(RuntimeTaskState::Running));
        assert_eq!(first.revision, 1);
        assert_eq!(duplicate.revision, 1);
        assert_eq!(changed.revision, 2);
    }

    #[test]
    fn subscribers_receive_the_canonical_revision() {
        let hub = RuntimeHub::new();
        hub.publish(snapshot(RuntimeTaskState::Idle));
        let (_, current, receiver) = hub.subscribe();
        assert_eq!(current.unwrap().revision, 1);
        hub.publish(snapshot(RuntimeTaskState::Running));
        assert_eq!(receiver.recv_timeout(Duration::from_millis(50)).unwrap().revision, 2);
    }

    #[test]
    fn prompt_rejects_terminal_control_sequences() {
        assert!(validate_prompt("please inspect the build").is_ok());
        assert!(validate_prompt("unsafe\u{1b}[2J").is_err());
        assert!(validate_prompt("two\nlines").is_err());
    }

    #[test]
    fn settled_wait_excludes_only_running() {
        assert!(!wait_state_matches(RuntimeTaskState::Running, RuntimeWaitState::Settled));
        assert!(wait_state_matches(RuntimeTaskState::WaitingInput, RuntimeWaitState::Settled));
        assert!(wait_state_matches(RuntimeTaskState::Failed, RuntimeWaitState::Settled));
    }

    #[test]
    fn state_change_seq_advances_only_on_transitions() {
        let hub = RuntimeHub::new();
        let seq = |snapshot: &RuntimeSnapshot| snapshot.pane(None, 3).unwrap().state_change_seq;

        let first = hub.publish(snapshot(RuntimeTaskState::Idle));
        assert_eq!(seq(&first), 1, "a newly seen pane starts at 1, never 0");
        // A duplicate publish is deduped, which only holds because the stamp
        // carried the counter forward instead of bumping it.
        assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Idle))), 1);
        assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Running))), 2);
        assert_eq!(seq(&hub.publish(snapshot(RuntimeTaskState::Idle))), 3);
    }

    #[test]
    fn wait_ignores_a_pane_that_never_left_the_target_state() {
        let hub = RuntimeHub::new();
        let idle = hub.publish(snapshot(RuntimeTaskState::Idle));
        let pane = idle.pane(None, 3).unwrap();

        // Without a baseline, an already-idle pane satisfies "wait for idle".
        assert!(wait_matches(pane, RuntimeWaitState::Idle, None));
        // With the baseline captured at submit time, it must not: this is the
        // race where a wait returned before the shell had started the command.
        assert!(!wait_matches(pane, RuntimeWaitState::Idle, Some(pane.state_change_seq)));

        let running = hub.publish(snapshot(RuntimeTaskState::Running));
        let settled = hub.publish(snapshot(RuntimeTaskState::Idle));
        assert!(!wait_matches(
            running.pane(None, 3).unwrap(),
            RuntimeWaitState::Idle,
            Some(pane.state_change_seq)
        ));
        assert!(wait_matches(
            settled.pane(None, 3).unwrap(),
            RuntimeWaitState::Idle,
            Some(pane.state_change_seq)
        ));
    }

    #[test]
    fn state_change_seq_does_not_leak_across_windows() {
        // Pane ids are window-local, so pane 3 in window 8 must not inherit
        // window 7's counter and appear to have already transitioned.
        let hub = RuntimeHub::new();
        hub.publish(snapshot(RuntimeTaskState::Running));
        let mut relabelled = snapshot(RuntimeTaskState::Idle);
        relabelled.windows[0].id = 8;
        let published = hub.publish(relabelled);
        assert_eq!(published.pane(Some(8), 3).unwrap().state_change_seq, 1);
    }

    #[test]
    fn protocol_schema_is_valid_json_and_tracks_v1() {
        let schema: Value =
            serde_json::from_str(include_str!("../../docs/runtime-api-v1.schema.json")).unwrap();
        assert_eq!(schema["properties"]["version"]["const"], PROTOCOL_VERSION);
    }
}
