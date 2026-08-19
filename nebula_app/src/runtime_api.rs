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

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Line, Point};
use nebula_terminal::term::Term;

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
pub(crate) const MAX_KEY_REPEAT: u16 = 64;
pub(crate) const DEFAULT_READ_LINES: usize = 120;
pub(crate) const MAX_READ_LINES: usize = 2_000;
const MAX_READ_BYTES: usize = 1024 * 1024;
const MAX_CLIENTS: usize = 64;
const MAX_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

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
pub enum RuntimeAgentStateSource {
    Hook,
    Screen,
    Process,
}

/// Identity and evidence attached only to panes Nebula recognizes as an AI
/// agent. Lifecycle state remains on [`RuntimePane::task_state`] so the tray,
/// sidebar, waits, and external clients all consume the same reducer output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeAgent {
    pub kind: String,
    pub display_name: String,
    pub session_id: Option<String>,
    pub state_source: RuntimeAgentStateSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_rule: Option<String>,
    pub hook_seen: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<RuntimeAgent>,
    pub task_state: RuntimeTaskState,
    /// Monotonic count of this pane's task-state transitions. A state value on
    /// its own cannot answer "did anything happen since I submitted?", so
    /// callers that sent work compare against a baseline captured at submit
    /// time. Stamped by [`RuntimeHub::publish`]; projection callers leave it 0.
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run: Option<RuntimePaneRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RuntimeRunOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeAgentPane {
    pub window_id: u64,
    pub tab_index: usize,
    pub tab_active: bool,
    pub pane_id: u64,
    pub pane_active: bool,
    pub title: String,
    pub cwd: String,
    pub branch: String,
    pub ssh_destination: Option<String>,
    pub agent: RuntimeAgent,
    pub task_state: RuntimeTaskState,
    pub state_change_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimePaneRead {
    pub window_id: u64,
    pub pane_id: u64,
    pub text: String,
    pub requested_lines: usize,
    pub returned_lines: usize,
    pub history_available: usize,
    pub truncated: bool,
    pub task_state: RuntimeTaskState,
    pub exited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeProcess {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub executable: String,
    pub display_name: String,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePaneProcesses {
    pub window_id: u64,
    pub pane_id: u64,
    pub root_pid: u32,
    pub processes: Vec<RuntimeProcess>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRunPhase {
    Submitted,
    Started,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePaneRun {
    pub run_id: u64,
    pub phase: RuntimeRunPhase,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRunState {
    Finished,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExitCodeCapability {
    Supported,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRunOutcome {
    pub run_id: u64,
    pub state: RuntimeRunState,
    pub exit_code: Option<i32>,
    pub exit_code_capability: ExitCodeCapability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl RuntimeRunOutcome {
    pub(crate) fn command_done(run: RuntimePaneRun, exit_code: Option<i32>) -> Self {
        if run.phase != RuntimeRunPhase::Started {
            return Self::unavailable(run.run_id, "command_start_not_observed");
        }
        match exit_code {
            Some(0) => Self {
                run_id: run.run_id,
                state: RuntimeRunState::Finished,
                exit_code,
                exit_code_capability: ExitCodeCapability::Supported,
                unavailable_reason: None,
            },
            Some(_) => Self {
                run_id: run.run_id,
                state: RuntimeRunState::Failed,
                exit_code,
                exit_code_capability: ExitCodeCapability::Supported,
                unavailable_reason: None,
            },
            None => Self::unavailable(run.run_id, "exit_code_not_reported"),
        }
    }

    pub(crate) fn unavailable(run_id: u64, reason: impl Into<String>) -> Self {
        Self {
            run_id,
            state: RuntimeRunState::Unavailable,
            exit_code: None,
            exit_code_capability: ExitCodeCapability::Unavailable,
            unavailable_reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeRunResult {
    pub window_id: u64,
    pub pane_id: u64,
    #[serde(flatten)]
    pub outcome: RuntimeRunOutcome,
}

pub(crate) fn begin_runtime_run() -> RuntimePaneRun {
    RuntimePaneRun {
        run_id: NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed),
        phase: RuntimeRunPhase::Submitted,
    }
}

/// Named control keys accepted by `pane.send_key`. Printable text is
/// intentionally absent; callers must use `pane.prompt` for text input.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKey {
    Escape,
    Enter,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
}

impl RuntimeKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Escape => "escape",
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Backspace => "backspace",
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Home => "home",
            Self::End => "end",
            Self::Insert => "insert",
            Self::Delete => "delete",
            Self::PageUp => "page_up",
            Self::PageDown => "page_down",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
            Self::G => "g",
            Self::H => "h",
            Self::I => "i",
            Self::J => "j",
            Self::K => "k",
            Self::L => "l",
            Self::M => "m",
            Self::N => "n",
            Self::O => "o",
            Self::P => "p",
            Self::Q => "q",
            Self::R => "r",
            Self::S => "s",
            Self::T => "t",
            Self::U => "u",
            Self::V => "v",
            Self::W => "w",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
        }
    }

    pub(crate) fn letter(self) -> Option<char> {
        let value = self.as_str();
        (value.len() == 1).then(|| value.as_bytes()[0] as char)
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeKeyModifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub control: bool,
}

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    Snapshot,
    NewWindow,
    Focus {
        window_id: Option<u64>,
        pane_id: Option<u64>,
    },
    NewTab {
        window_id: Option<u64>,
        cwd: Option<PathBuf>,
    },
    Split {
        window_id: Option<u64>,
        direction: RuntimeSplitDirection,
    },
    Prompt {
        window_id: Option<u64>,
        pane_id: u64,
        text: String,
        submit: bool,
    },
    ReadPane {
        window_id: Option<u64>,
        pane_id: u64,
        lines: usize,
    },
    Procs {
        window_id: Option<u64>,
        pane_id: u64,
    },
    SendKey {
        window_id: Option<u64>,
        pane_id: u64,
        key: RuntimeKey,
        modifiers: RuntimeKeyModifiers,
        repeat: u16,
    },
    Run {
        window_id: Option<u64>,
        pane_id: u64,
        command: String,
        wait: bool,
        timeout_ms: u64,
    },
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

/// Non-winit event loops (GPUI) receive attach / control without an
/// `EventLoopProxy`. ATTACH is fire-and-forget; control still waits on
/// [`RuntimeDispatch::respond`].
#[derive(Clone)]
pub enum RuntimeCallback {
    Attach,
    Control(Arc<RuntimeDispatch>),
}

#[derive(Clone)]
enum EventSink {
    Winit(EventLoopProxy<Event>),
    Callback(Arc<dyn Fn(RuntimeCallback) + Send + Sync>),
}

impl EventSink {
    fn emit_attach(&self) {
        match self {
            Self::Winit(proxy) => {
                let _ = proxy.send_event(Event::new(EventType::NebulaAttach, None));
            },
            Self::Callback(callback) => callback(RuntimeCallback::Attach),
        }
    }

    fn emit_control(&self, dispatch: Arc<RuntimeDispatch>) -> bool {
        match self {
            Self::Winit(proxy) => {
                proxy.send_event(Event::new(EventType::RuntimeControl(dispatch), None)).is_ok()
            },
            Self::Callback(callback) => {
                callback(RuntimeCallback::Control(dispatch));
                true
            },
        }
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
    next_run_waiter: u64,
    run_waiters:
        std::collections::HashMap<(u64, u64, u64), Vec<(u64, SyncSender<RuntimeRunResult>)>>,
    completed_runs: std::collections::VecDeque<RuntimeRunResult>,
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
        observe_run_lifecycle(&mut state, &snapshot);
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

    fn current(&self) -> Option<RuntimeSnapshot> {
        self.lock().current.clone()
    }

    fn wait_run(
        &self,
        window_id: u64,
        pane_id: u64,
        run_id: u64,
        timeout: Duration,
    ) -> Result<RuntimeRunResult, ApiError> {
        let key = (window_id, pane_id, run_id);
        let (waiter_id, receiver) = {
            let mut state = self.lock();
            if let Some(result) = state.completed_runs.iter().find(|result| {
                result.window_id == window_id
                    && result.pane_id == pane_id
                    && result.outcome.run_id == run_id
            }) {
                return run_result_or_capability_error(result.clone());
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            state.next_run_waiter = state.next_run_waiter.saturating_add(1);
            let waiter_id = state.next_run_waiter;
            state.run_waiters.entry(key).or_default().push((waiter_id, sender));
            (waiter_id, receiver)
        };

        match receiver.recv_timeout(timeout) {
            Ok(result) => run_result_or_capability_error(result),
            Err(RecvTimeoutError::Disconnected) => Err(ApiError::new(
                "runtime_unavailable",
                "runtime run completion channel disconnected",
            )),
            Err(RecvTimeoutError::Timeout) => {
                let mut state = self.lock();
                if let Some(waiters) = state.run_waiters.get_mut(&key) {
                    waiters.retain(|(id, _)| *id != waiter_id);
                    if waiters.is_empty() {
                        state.run_waiters.remove(&key);
                    }
                }
                let phase = state.current.as_ref().and_then(|snapshot| {
                    snapshot
                        .pane(Some(window_id), pane_id)
                        .ok()
                        .and_then(|pane| pane.active_run)
                        .filter(|run| run.run_id == run_id)
                        .map(|run| run.phase)
                });
                let (code, message) = match phase {
                    Some(RuntimeRunPhase::Submitted) => (
                        "run_start_timeout",
                        "the shell did not report CommandStart before timeout",
                    ),
                    Some(RuntimeRunPhase::Started) => {
                        ("timeout", "the command did not report CommandDone before timeout")
                    },
                    None => ("run_aborted", "the pane or run disappeared before completion"),
                };
                Err(ApiError::new(code, message).details(json!({
                    "window_id": window_id,
                    "pane_id": pane_id,
                    "run_id": run_id,
                    "phase": phase
                })))
            },
        }
    }
}

const COMPLETED_RUN_CACHE: usize = 256;

fn observe_run_lifecycle(state: &mut HubState, snapshot: &RuntimeSnapshot) {
    let mut observed = std::collections::HashMap::new();
    for window in &snapshot.windows {
        for tab in &window.tabs {
            for pane in &tab.panes {
                if let Some(active) = pane.active_run {
                    observed.insert((window.id, pane.id, active.run_id), active.phase);
                }
                let Some(outcome) = pane.last_run.clone() else {
                    continue;
                };
                let already_cached = state.completed_runs.iter().any(|result| {
                    result.window_id == window.id
                        && result.pane_id == pane.id
                        && result.outcome.run_id == outcome.run_id
                });
                if !already_cached {
                    complete_run(
                        state,
                        RuntimeRunResult { window_id: window.id, pane_id: pane.id, outcome },
                    );
                }
            }
        }
    }

    let previous_active: Vec<_> = state
        .current
        .iter()
        .flat_map(|previous| previous.windows.iter())
        .flat_map(|window| {
            window.tabs.iter().flat_map(move |tab| {
                tab.panes.iter().filter_map(move |pane| {
                    pane.active_run.map(|run| (window.id, pane.id, run.run_id))
                })
            })
        })
        .collect();
    for key @ (window_id, pane_id, run_id) in previous_active {
        if observed.contains_key(&key)
            || state.completed_runs.iter().any(|result| {
                result.window_id == window_id
                    && result.pane_id == pane_id
                    && result.outcome.run_id == run_id
            })
        {
            continue;
        }
        complete_run(
            state,
            RuntimeRunResult {
                window_id,
                pane_id,
                outcome: RuntimeRunOutcome::unavailable(run_id, "pane_or_run_disappeared"),
            },
        );
    }
}

fn complete_run(state: &mut HubState, result: RuntimeRunResult) {
    let key = (result.window_id, result.pane_id, result.outcome.run_id);
    if let Some(waiters) = state.run_waiters.remove(&key) {
        for (_, sender) in waiters {
            let _ = sender.try_send(result.clone());
        }
    }
    state.completed_runs.push_back(result);
    while state.completed_runs.len() > COMPLETED_RUN_CACHE {
        state.completed_runs.pop_front();
    }
}

fn run_result_or_capability_error(result: RuntimeRunResult) -> Result<RuntimeRunResult, ApiError> {
    if result.outcome.state != RuntimeRunState::Unavailable {
        return Ok(result);
    }
    let reason = result
        .outcome
        .unavailable_reason
        .clone()
        .unwrap_or_else(|| "exit_code_unavailable".to_owned());
    let code = match reason.as_str() {
        "command_start_not_observed" => "shell_integration_unavailable",
        "pane_or_run_disappeared" => "run_aborted",
        _ => "exit_code_unavailable",
    };
    Err(ApiError::new(code, "the command completed without a reliable exit-code result")
        .details(serde_json::to_value(result).unwrap_or_else(|_| json!({ "reason": reason }))))
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
    /// `tab.new` 可选：新标签的工作目录（Explorer 右键并入驻留实例时携带）。
    #[serde(default)]
    cwd: Option<PathBuf>,
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
struct AgentsParams {
    #[serde(default)]
    window_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    #[serde(default = "default_read_lines")]
    lines: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendKeyParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    key: RuntimeKey,
    #[serde(default)]
    modifiers: RuntimeKeyModifiers,
    #[serde(default = "default_key_repeat")]
    repeat: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunParams {
    #[serde(default)]
    window_id: Option<u64>,
    pane_id: u64,
    command: String,
    #[serde(default = "default_true")]
    wait: bool,
    #[serde(default = "default_run_timeout_ms")]
    timeout_ms: u64,
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

fn default_read_lines() -> usize {
    DEFAULT_READ_LINES
}

fn default_key_repeat() -> u16 {
    1
}

fn default_run_timeout_ms() -> u64 {
    COMMAND_TIMEOUT.as_millis() as u64
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
                Ok(Self::NewTab { window_id: params.window_id, cwd: params.cwd })
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
            "pane.read" => {
                let params: ReadParams = parse_params(&request.params)?;
                if params.lines == 0 || params.lines > MAX_READ_LINES {
                    return Err(ApiError::invalid_params(format!(
                        "lines must be between 1 and {MAX_READ_LINES}"
                    )));
                }
                Ok(Self::ReadPane {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    lines: params.lines,
                })
            },
            "pane.procs" => {
                let params: PaneParams = parse_params(&request.params)?;
                Ok(Self::Procs { window_id: params.window_id, pane_id: params.pane_id })
            },
            "pane.send_key" => {
                let params: SendKeyParams = parse_params(&request.params)?;
                if params.repeat == 0 || params.repeat > MAX_KEY_REPEAT {
                    return Err(ApiError::invalid_params(format!(
                        "repeat must be between 1 and {MAX_KEY_REPEAT}"
                    )));
                }
                if params.key.letter().is_some() && !params.modifiers.control {
                    return Err(ApiError::invalid_params(
                        "letter keys require control=true; use pane.prompt for printable text",
                    ));
                }
                Ok(Self::SendKey {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    key: params.key,
                    modifiers: params.modifiers,
                    repeat: params.repeat,
                })
            },
            "pane.run" => {
                let params: RunParams = parse_params(&request.params)?;
                validate_command_line(&params.command)?;
                if params.timeout_ms == 0 || Duration::from_millis(params.timeout_ms) > MAX_WAIT {
                    return Err(ApiError::invalid_params(
                        "timeout_ms must be between 1 and 86400000",
                    ));
                }
                Ok(Self::Run {
                    window_id: params.window_id,
                    pane_id: params.pane_id,
                    command: params.command,
                    wait: params.wait,
                    timeout_ms: params.timeout_ms,
                })
            },
            method => Err(ApiError::new(
                "method_not_found",
                format!("runtime API method {method:?} does not exist"),
            )),
        }
    }
}

/// Read the logical tail of the terminal model. The range is anchored at the
/// buffer bottom, never at `display_offset`, so a user scrolling through
/// history cannot change what an external agent observes.
pub(crate) fn capture_terminal_tail<T: EventListener>(
    term: &Term<T>,
    window_id: u64,
    pane_id: u64,
    requested_lines: usize,
    task_state: RuntimeTaskState,
    exited: bool,
    exit_reason: Option<String>,
) -> RuntimePaneRead {
    let columns = term.columns();
    let screen_lines = term.screen_lines();
    let total_lines = term.total_lines();
    let history_available = total_lines.saturating_sub(screen_lines);
    if columns == 0 || screen_lines == 0 || total_lines == 0 {
        return RuntimePaneRead {
            window_id,
            pane_id,
            text: String::new(),
            requested_lines,
            returned_lines: 0,
            history_available,
            truncated: false,
            task_state,
            exited,
            exit_reason,
        };
    }

    let mut returned_lines = requested_lines.min(total_lines);
    let end = Point::new(Line(screen_lines as i32 - 1), Column(columns - 1));
    let capture = |lines: usize| {
        let start_line = screen_lines as i64 - lines as i64;
        let start = Point::new(Line(start_line.max(-(history_available as i64)) as i32), Column(0));
        term.bounds_to_string(start, end)
    };
    let mut text = capture(returned_lines);

    // Reduce by whole terminal rows first, preserving exact returned_lines.
    // Only a pathological single row can fall through to UTF-8 byte slicing.
    while text.len() > MAX_READ_BYTES && returned_lines > 1 {
        let estimated = ((returned_lines as u128 * MAX_READ_BYTES as u128) / text.len() as u128)
            .clamp(1, (returned_lines - 1) as u128) as usize;
        returned_lines = estimated;
        text = capture(returned_lines);
    }
    let mut byte_truncated = false;
    if text.len() > MAX_READ_BYTES {
        let mut start = text.len() - MAX_READ_BYTES;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text = text[start..].to_owned();
        byte_truncated = true;
    }

    RuntimePaneRead {
        window_id,
        pane_id,
        text,
        requested_lines,
        returned_lines,
        history_available,
        truncated: returned_lines < total_lines || byte_truncated,
        task_state,
        exited,
        exit_reason,
    }
}

pub(crate) fn capture_process_tree(
    window_id: u64,
    pane_id: u64,
    root_pid: u32,
) -> Result<RuntimePaneProcesses, ApiError> {
    let entries = crate::process_tree::descendants(root_pid).map_err(|message| {
        ApiError::new("process_query_failed", "failed to read the pane process tree")
            .details(json!({ "root_pid": root_pid, "reason": message }))
    })?;
    let processes = entries
        .into_iter()
        .map(|entry| {
            let agent_kind = crate::ai_agents::AgentKind::parse(&entry.executable)
                .map(|kind| kind.slug().to_owned());
            RuntimeProcess {
                pid: entry.pid,
                parent_pid: (entry.pid != root_pid).then_some(entry.parent_pid),
                display_name: crate::process_tree::display_name(&entry.executable),
                executable: entry.executable,
                depth: entry.depth,
                agent_kind,
            }
        })
        .collect();
    Ok(RuntimePaneProcesses { window_id, pane_id, root_pid, processes })
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

pub(crate) fn validate_command_line(command: &str) -> Result<(), ApiError> {
    if command.trim().is_empty() {
        return Err(ApiError::invalid_params("command must not be empty"));
    }
    if command.len() > MAX_PROMPT_BYTES {
        return Err(ApiError::invalid_params(format!(
            "command exceeds the {MAX_PROMPT_BYTES}-byte limit"
        )));
    }
    if command.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "command contains control characters; pane.run accepts one plain-text shell line",
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

/// 普通二次启动并入驻留实例：先恢复/聚焦窗口，再新建一个默认 shell 标签页。
pub fn try_open_default_tab_existing() -> bool {
    try_open_tab_existing(None)
}

/// 后台任务把一行文本作为输入敲进某个 pane（不回车）。复用 runtime Prompt
/// 的窗口定位与写入路径（`window_id=None` 按 pane 自动定位）；响应通道即弃
/// ——调用方不关心结果，失败在事件线程侧留日志即可。SFTP 图片粘贴用它回粘
/// 远端路径。
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

/// Explorer 右键「在 Nebula 中打开」/ CLI 带 `--working-directory` 的启动并入
/// 驻留实例：先经 ATTACH 恢复或聚焦既有窗口（与平启动同一套交接，detached
/// 的标签因此先回来），再请求在该窗口打开定目录标签。任一步失败都返回
/// false，调用方回落为独立启动——绝不吞掉用户的手势。
pub fn try_open_directory_existing(dir: &std::path::Path) -> bool {
    try_open_tab_existing(Some(dir))
}

fn try_open_tab_existing(dir: Option<&std::path::Path>) -> bool {
    if legacy_request("ATTACH").is_none() {
        return false;
    }
    // ATTACH 与 tab.new 都落到同一条 winit 事件队列上，先后有序：窗口先
    // 恢复，新标签随后落在恢复出来的窗口里。
    let params = dir.map_or_else(|| json!({}), |dir| json!({ "cwd": dir }));
    request_once("tab.new", params, IO_TIMEOUT).map(|response| response.ok).unwrap_or(false)
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
        Self::spawn_with_sink(EventSink::Winit(proxy), hub)
    }

    /// Same discovery file and ATTACH/PING/JSON protocol as [`Self::spawn`],
    /// but attach/control are delivered through a callback (GPUI has no winit
    /// `EventLoopProxy`).
    pub fn spawn_callback(
        on_event: impl Fn(RuntimeCallback) + Send + Sync + 'static,
        hub: RuntimeHub,
    ) -> Option<Self> {
        Self::spawn_with_sink(EventSink::Callback(Arc::new(on_event)), hub)
    }

    fn spawn_with_sink(sink: EventSink, hub: RuntimeHub) -> Option<Self> {
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
            .spawn(move || serve(listener, server_token, sink, hub))
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

fn serve(listener: TcpListener, token: String, sink: EventSink, hub: RuntimeHub) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let active = active.clone();
        let token = token.clone();
        let sink = sink.clone();
        let hub = hub.clone();
        let _ = std::thread::Builder::new().name("nebula-runtime-client".into()).spawn(move || {
            let _guard = ActiveClient(active);
            if let Err(error) = handle_connection(stream, &token, &sink, &hub) {
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
    sink: &EventSink,
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
        return handle_legacy(&mut stream, &line, token, sink);
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
        "agents.list" => agents_connection(&mut stream, request, hub),
        "pane.wait" => wait_connection(&mut stream, request, hub),
        _ => dispatch_connection(&mut stream, request, sink, hub),
    }
}

fn handle_legacy(
    stream: &mut TcpStream,
    line: &str,
    token: &str,
    sink: &EventSink,
) -> Result<(), IoError> {
    let mut parts = line.split_whitespace();
    let verb = parts.next().unwrap_or("");
    if parts.next() != Some(token) {
        return Ok(());
    }
    match verb {
        "ATTACH" => {
            sink.emit_attach();
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
            "agents.list",
            "window.create",
            "window.focus",
            "tab.new",
            "pane.split",
            "pane.prompt",
            "pane.read",
            "pane.procs",
            "pane.send_key",
            "pane.run",
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

fn agents_connection(
    stream: &mut TcpStream,
    request: ApiRequest,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let params: AgentsParams = match parse_params(&request.params) {
        Ok(params) => params,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    let Some(snapshot) = hub.current() else {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new(
                    "runtime_unavailable",
                    "Nebula has not published its first workspace snapshot yet",
                ),
            ),
        );
    };
    if let Some(window_id) = params.window_id
        && !snapshot.windows.iter().any(|window| window.id == window_id)
    {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new("target_not_found", format!("window {window_id} does not exist")),
            ),
        );
    }

    let mut agents = Vec::new();
    for window in
        snapshot.windows.iter().filter(|window| params.window_id.is_none_or(|id| window.id == id))
    {
        for tab in &window.tabs {
            for pane in &tab.panes {
                let Some(agent) = pane.agent.clone() else { continue };
                agents.push(RuntimeAgentPane {
                    window_id: window.id,
                    tab_index: tab.index,
                    tab_active: tab.active,
                    pane_id: pane.id,
                    pane_active: pane.active,
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    branch: pane.branch.clone(),
                    ssh_destination: pane.ssh_destination.clone(),
                    agent,
                    task_state: pane.task_state,
                    state_change_seq: pane.state_change_seq,
                });
            }
        }
    }
    write_response(
        stream,
        &ApiResponse::success(
            request.id,
            json!({ "revision": snapshot.revision, "agents": agents }),
        ),
    )
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
    sink: &EventSink,
    hub: &RuntimeHub,
) -> Result<(), IoError> {
    let command = match RuntimeCommand::from_request(&request) {
        Ok(command) => command,
        Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
    };
    if let RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } = &command {
        info!(
            "runtime pane.send_key request_id={} window_id={window_id:?} pane_id={pane_id} key={} shift={} alt={} control={} repeat={repeat}",
            request.id,
            key.as_str(),
            modifiers.shift,
            modifiers.alt,
            modifiers.control,
        );
    }
    let run_wait = match &command {
        RuntimeCommand::Run { wait: true, timeout_ms, .. } => {
            Some(Duration::from_millis(*timeout_ms))
        },
        _ => None,
    };
    let (dispatch, receiver) = RuntimeDispatch::new(command);
    if !sink.emit_control(dispatch) {
        return write_response(
            stream,
            &ApiResponse::failure(
                request.id,
                ApiError::new("runtime_unavailable", "Nebula's event loop is not available"),
            ),
        );
    }
    let response = match receiver.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(result)) => {
            if let Some(timeout) = run_wait {
                let action = result.get("action").unwrap_or(&result);
                let target = (
                    action.get("window_id").and_then(Value::as_u64),
                    action.get("pane_id").and_then(Value::as_u64),
                    action.get("run_id").and_then(Value::as_u64),
                );
                match target {
                    (Some(window_id), Some(pane_id), Some(run_id)) => {
                        match hub.wait_run(window_id, pane_id, run_id, timeout) {
                            Ok(run) => ApiResponse::success(
                                request.id,
                                json!({ "run": run, "snapshot": hub.current() }),
                            ),
                            Err(error) => ApiResponse::failure(request.id, error),
                        }
                    },
                    _ => ApiResponse::failure(
                        request.id,
                        ApiError::new(
                            "invalid_runtime_response",
                            "pane.run did not return its window, pane, and run identity",
                        ),
                    ),
                }
            } else {
                ApiResponse::success(request.id, result)
            }
        },
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
        CliCommand::Agents { window } => {
            let response = request_once("agents.list", json!({ "window_id": window }), timeout)?;
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
        CliCommand::Read { window, pane, lines } => {
            let response = request_once(
                "pane.read",
                json!({ "window_id": window, "pane_id": pane, "lines": lines }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Procs { window, pane } => {
            let response = request_once(
                "pane.procs",
                json!({ "window_id": window, "pane_id": pane }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::SendKey { window, pane, key, shift, alt, control, repeat } => {
            let response = request_once(
                "pane.send_key",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "key": key,
                    "modifiers": {
                        "shift": shift,
                        "alt": alt,
                        "control": control
                    },
                    "repeat": repeat
                }),
                timeout,
            )?;
            print_response(&response, options.pretty)
        },
        CliCommand::Run { window, pane, command, no_wait } => {
            let response = request_once(
                "pane.run",
                json!({
                    "window_id": window,
                    "pane_id": pane,
                    "command": command,
                    "wait": !no_wait,
                    "timeout_ms": options.timeout_ms
                }),
                timeout.saturating_add(Duration::from_secs(1)),
            )?;
            print_response(&response, options.pretty)
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
                        agent: None,
                        task_state: state,
                        state_change_seq: 0,
                        active_run: None,
                        last_run: None,
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
    fn send_key_accepts_only_the_restricted_control_contract() {
        let valid = ApiRequest::new(
            "token".into(),
            "pane.send_key",
            json!({
                "pane_id": 3,
                "key": "c",
                "modifiers": { "control": true },
                "repeat": 2
            }),
        );
        assert!(matches!(
            RuntimeCommand::from_request(&valid),
            Ok(RuntimeCommand::SendKey { key: RuntimeKey::C, repeat: 2, .. })
        ));

        let printable =
            ApiRequest::new("token".into(), "pane.send_key", json!({ "pane_id": 3, "key": "c" }));
        assert_eq!(RuntimeCommand::from_request(&printable).unwrap_err().code, "invalid_params");

        let arbitrary_bytes = ApiRequest::new(
            "token".into(),
            "pane.send_key",
            json!({ "pane_id": 3, "key": "escape", "bytes": [27, 91, 50, 74] }),
        );
        assert_eq!(
            RuntimeCommand::from_request(&arbitrary_bytes).unwrap_err().code,
            "invalid_params"
        );
    }

    #[test]
    fn run_requires_one_plain_shell_line() {
        let valid = ApiRequest::new(
            "token".into(),
            "pane.run",
            json!({ "pane_id": 3, "command": "cargo test", "wait": true }),
        );
        assert!(matches!(
            RuntimeCommand::from_request(&valid),
            Ok(RuntimeCommand::Run { wait: true, .. })
        ));

        let multiline = ApiRequest::new(
            "token".into(),
            "pane.run",
            json!({ "pane_id": 3, "command": "echo one\necho two" }),
        );
        assert_eq!(RuntimeCommand::from_request(&multiline).unwrap_err().code, "invalid_params");
    }

    #[test]
    fn run_outcome_requires_a_real_start_and_exit_code() {
        let submitted = RuntimePaneRun { run_id: 41, phase: RuntimeRunPhase::Submitted };
        let no_start = RuntimeRunOutcome::command_done(submitted, Some(0));
        assert_eq!(no_start.state, RuntimeRunState::Unavailable);
        assert_eq!(no_start.unavailable_reason.as_deref(), Some("command_start_not_observed"));

        let started = RuntimePaneRun { run_id: 42, phase: RuntimeRunPhase::Started };
        assert_eq!(
            RuntimeRunOutcome::command_done(started, Some(0)).state,
            RuntimeRunState::Finished
        );
        assert_eq!(
            RuntimeRunOutcome::command_done(started, Some(7)).state,
            RuntimeRunState::Failed
        );
        let missing_code = RuntimeRunOutcome::command_done(started, None);
        assert_eq!(missing_code.state, RuntimeRunState::Unavailable);
        assert_eq!(missing_code.exit_code_capability, ExitCodeCapability::Unavailable);
    }

    #[test]
    fn completed_run_cache_closes_the_waiter_registration_race() {
        let hub = RuntimeHub::new();
        let mut running = snapshot(RuntimeTaskState::Running);
        running.windows[0].tabs[0].panes[0].active_run =
            Some(RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started });
        hub.publish(running);

        let mut done = snapshot(RuntimeTaskState::Finished);
        done.windows[0].tabs[0].panes[0].last_run = Some(RuntimeRunOutcome::command_done(
            RuntimePaneRun { run_id: 51, phase: RuntimeRunPhase::Started },
            Some(0),
        ));
        hub.publish(done);

        // The result was published before this waiter existed. The bounded
        // cache must still return the exact run rather than timing out.
        let result = hub.wait_run(7, 3, 51, Duration::from_millis(10)).unwrap();
        assert_eq!(result.outcome.exit_code, Some(0));
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

    #[test]
    fn terminal_tail_reads_buffer_bottom_with_utf8_intact() {
        let term = nebula_terminal::term::test::mock_term("old\r\n中间\r\nlatest");
        let read = capture_terminal_tail(&term, 7, 3, 2, RuntimeTaskState::Finished, false, None);
        assert_eq!(read.text, "中间\nlatest");
        assert_eq!(read.requested_lines, 2);
        assert_eq!(read.returned_lines, 2);
        assert!(read.truncated);
        assert!(std::str::from_utf8(read.text.as_bytes()).is_ok());
    }

    #[test]
    fn agents_list_projection_keeps_window_and_tab_identity() {
        let mut snapshot = snapshot(RuntimeTaskState::Attention);
        snapshot.windows[0].tabs[0].panes[0].agent = Some(RuntimeAgent {
            kind: "codex".into(),
            display_name: "Codex".into(),
            session_id: Some("thread-7".into()),
            state_source: RuntimeAgentStateSource::Hook,
            state_rule: None,
            hook_seen: true,
        });
        let hub = RuntimeHub::new();
        let published = hub.publish(snapshot);
        assert_eq!(published.windows[0].tabs[0].panes[0].state_change_seq, 1);
        assert_eq!(
            published.windows[0].tabs[0].panes[0].agent.as_ref().unwrap().session_id.as_deref(),
            Some("thread-7")
        );
    }
}
