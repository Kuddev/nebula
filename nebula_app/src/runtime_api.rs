//! Versioned local control plane shared by Nebula's CLI, agents, and future plugins.
//!
//! The transport is loopback TCP plus a per-instance discovery token. Requests
//! are JSON Lines so clients in any language can use the API without linking
//! Rust types. GUI and PTY mutations are dispatched onto winit's event thread;
//! transport workers only validate, wait, and serialize.

mod agent_api;
mod cli;
mod command;
mod server;
#[cfg(test)]
mod tests;

pub use cli::run_cli;
use command::{
    RuntimeWaitState, SubscribeParams, WaitParams, default_read_lines, default_true, parse_params,
    validate_agent_name, validate_agent_selector, wait_matches,
};
pub(crate) use command::{
    capture_process_tree, capture_terminal_tail, validate_command_line, validate_prompt,
};
use server::{Endpoint, endpoint_addr, read_endpoint};
pub use server::{
    RuntimeServer, dispatch_prompt, try_open_default_tab_existing, try_open_directory_existing,
};

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
static NEXT_AGENT_ID: AtomicU64 = AtomicU64::new(1);

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

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::git_worktree::WorktreeProvenance>,
    pub kind: String,
    pub display_name: String,
    pub session_id: Option<String>,
    pub state_source: RuntimeAgentStateSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_rule: Option<String>,
    pub hook_seen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeManagedAgent {
    pub agent_id: String,
    pub generation: u64,
    pub name: String,
    pub kind: String,
    pub window_id: u64,
    pub pane_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::git_worktree::WorktreeProvenance>,
    pub active: bool,
    pub observed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePaneLifecycleKind {
    Closed,
    Exited,
}

impl RuntimePaneLifecycleKind {
    fn error_code(self) -> &'static str {
        match self {
            Self::Closed => "pane_closed",
            Self::Exited => "pane_exited",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePaneLifecycle {
    pub sequence: u64,
    pub window_id: u64,
    pub pane_id: u64,
    pub event: RuntimePaneLifecycleKind,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane_lifecycles: Vec<RuntimePaneLifecycle>,
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
            pane_lifecycles: Vec::new(),
        }
    }

    fn pane(&self, window_id: Option<u64>, pane_id: u64) -> Result<&RuntimePane, ApiError> {
        self.pane_target(window_id, pane_id).map(|(_, pane)| pane)
    }

    fn pane_target(
        &self,
        window_id: Option<u64>,
        pane_id: u64,
    ) -> Result<(u64, &RuntimePane), ApiError> {
        let matches: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window_id.is_none_or(|id| window.id == id))
            .flat_map(|window| {
                window
                    .tabs
                    .iter()
                    .flat_map(move |tab| tab.panes.iter().map(move |pane| (window.id, pane)))
            })
            .filter(|(_, pane)| pane.id == pane_id)
            .collect();
        match matches.as_slice() {
            [target] => Ok(*target),
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
    AgentStart {
        window_id: Option<u64>,
        name: String,
        kind: crate::ai_agents::AgentKind,
        cwd: Option<PathBuf>,
        session_id: Option<String>,
        command: String,
        worktree: Option<crate::git_worktree::WorktreeProvenance>,
    },
    AgentFork {
        window_id: Option<u64>,
        source_pane_id: Option<u64>,
        source_cwd: Option<PathBuf>,
        name: String,
        kind: crate::ai_agents::AgentKind,
        session_id: Option<String>,
        command: String,
        branch: Option<String>,
        base: Option<String>,
        path: Option<PathBuf>,
        allow_dirty_source: bool,
    },
    AgentPrompt {
        agent: String,
        generation: Option<u64>,
        text: String,
        submit: bool,
    },
    AgentRead {
        agent: String,
        generation: Option<u64>,
        lines: usize,
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
    next_pane_lifecycle: u64,
    pane_lifecycles: std::collections::VecDeque<RuntimePaneLifecycle>,
    next_run_waiter: u64,
    run_waiters:
        std::collections::HashMap<(u64, u64, u64), Vec<(u64, SyncSender<RuntimeRunResult>)>>,
    completed_runs: std::collections::VecDeque<RuntimeRunResult>,
    agent_generations: std::collections::HashMap<String, u64>,
    managed_agents: std::collections::HashMap<String, RuntimeManagedAgent>,
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
        observe_removed_panes(&mut state, &snapshot);
        snapshot.pane_lifecycles = state.pane_lifecycles.iter().cloned().collect();
        let agent_lifecycle_changed = project_managed_agents(&mut state, &mut snapshot);
        // Stamp per-pane transition counters before the dedup compare. Panes
        // whose state is unchanged keep their previous counter, so an
        // otherwise-identical projection still compares equal here.
        stamp_state_change_seq(state.current.as_ref(), &mut snapshot);
        observe_run_lifecycle(&mut state, &snapshot);
        if let Some(current) = state.current.clone() {
            snapshot.revision = current.revision;
            if current == snapshot {
                if agent_lifecycle_changed {
                    notify_snapshot_subscribers(&mut state, &current);
                }
                return current;
            }
            snapshot.revision = current.revision.saturating_add(1);
        } else {
            snapshot.revision = 1;
        }

        state.current = Some(snapshot.clone());
        notify_snapshot_subscribers(&mut state, &snapshot);
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

    pub(crate) fn record_pane_closed(&self, window_id: u64, pane_id: u64) {
        self.record_pane_lifecycle(window_id, pane_id, RuntimePaneLifecycleKind::Closed);
    }

    pub(crate) fn record_pane_exited(&self, window_id: u64, pane_id: u64) {
        self.record_pane_lifecycle(window_id, pane_id, RuntimePaneLifecycleKind::Exited);
    }

    fn record_pane_lifecycle(&self, window_id: u64, pane_id: u64, event: RuntimePaneLifecycleKind) {
        let republish = {
            let mut state = self.lock();
            record_pane_lifecycle_locked(&mut state, window_id, pane_id, event)
                .then(|| state.current.clone())
                .flatten()
        };
        if let Some(snapshot) = republish {
            self.publish(snapshot);
        }
    }

    fn pane_lifecycle_error(&self, window_id: Option<u64>, pane_id: u64) -> Option<ApiError> {
        let state = self.lock();
        let matches: Vec<_> = state
            .pane_lifecycles
            .iter()
            .filter(|lifecycle| {
                lifecycle.pane_id == pane_id && window_id.is_none_or(|id| lifecycle.window_id == id)
            })
            .collect();
        match matches.as_slice() {
            [lifecycle] => Some(
                ApiError::new(
                    lifecycle.event.error_code(),
                    format!(
                        "pane {} in window {} has {}",
                        lifecycle.pane_id,
                        lifecycle.window_id,
                        match lifecycle.event {
                            RuntimePaneLifecycleKind::Closed => "closed",
                            RuntimePaneLifecycleKind::Exited => "exited",
                        }
                    ),
                )
                .details(serde_json::to_value(lifecycle).unwrap_or(Value::Null)),
            ),
            [] => None,
            _ => Some(ApiError::new(
                "ambiguous_target",
                format!(
                    "pane id {pane_id} has lifecycle events in multiple windows; provide window_id"
                ),
            )),
        }
    }

    pub(crate) fn register_agent(
        &self,
        name: String,
        kind: crate::ai_agents::AgentKind,
        window_id: u64,
        pane_id: u64,
        session_id: Option<String>,
        worktree: Option<crate::git_worktree::WorktreeProvenance>,
    ) -> Result<RuntimeManagedAgent, ApiError> {
        let mut state = self.lock();
        if state.managed_agents.values().any(|agent| agent.active && agent.name == name) {
            return Err(ApiError::new(
                "agent_name_conflict",
                format!("an active agent named {name:?} already exists"),
            ));
        }
        let generation = state.agent_generations.get(&name).copied().unwrap_or(0).saturating_add(1);
        state.agent_generations.insert(name.clone(), generation);
        let sequence = NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed);
        let agent_id = format!("agent-{}-{sequence}", std::process::id());
        let agent = RuntimeManagedAgent {
            agent_id: agent_id.clone(),
            generation,
            name,
            kind: kind.slug().to_owned(),
            window_id,
            pane_id,
            session_id,
            worktree,
            active: true,
            observed: false,
            closed_reason: None,
        };
        state.managed_agents.insert(agent_id, agent.clone());
        Ok(agent)
    }

    pub(crate) fn ensure_agent_name_available(&self, name: &str) -> Result<(), ApiError> {
        if self.lock().managed_agents.values().any(|agent| agent.active && agent.name == name) {
            return Err(ApiError::new(
                "agent_name_conflict",
                format!("an active agent named {name:?} already exists"),
            ));
        }
        Ok(())
    }

    pub(crate) fn close_agent(&self, agent_id: &str, reason: &str) {
        let mut state = self.lock();
        let changed = if let Some(agent) = state.managed_agents.get_mut(agent_id) {
            if agent.active {
                agent.active = false;
                agent.closed_reason = Some(reason.to_owned());
                true
            } else {
                false
            }
        } else {
            false
        };
        if changed && let Some(snapshot) = state.current.clone() {
            // Agent waits share the bounded snapshot channel with pane waits.
            // Re-send the canonical snapshot so a registry-only close wakes
            // immediately instead of degrading into an unrelated timeout.
            notify_snapshot_subscribers(&mut state, &snapshot);
        }
    }

    fn managed_agent(
        &self,
        selector: &str,
        generation: Option<u64>,
        require_active: bool,
    ) -> Result<RuntimeManagedAgent, ApiError> {
        let state = self.lock();
        let by_name = || {
            let exact_generation = generation.and_then(|generation| {
                state
                    .managed_agents
                    .values()
                    .filter(|agent| agent.name == selector && agent.generation == generation)
                    .max_by_key(|agent| agent.generation)
            });
            exact_generation.or_else(|| {
                state
                    .managed_agents
                    .values()
                    .filter(|agent| agent.name == selector && (!require_active || agent.active))
                    .max_by_key(|agent| agent.generation)
            })
        };
        let agent =
            state.managed_agents.get(selector).or_else(by_name).cloned().ok_or_else(|| {
                ApiError::new("agent_not_found", format!("agent {selector:?} does not exist"))
            })?;
        if generation.is_some_and(|expected| expected != agent.generation) {
            return Err(ApiError::new(
                "agent_identity_mismatch",
                format!(
                    "agent {:?} is generation {}, not {}",
                    agent.name,
                    agent.generation,
                    generation.expect("checked Some")
                ),
            )
            .details(json!({
                "agent_id": agent.agent_id,
                "expected_generation": generation,
                "actual_generation": agent.generation
            })));
        }
        if require_active && !agent.active {
            let code = match agent.closed_reason.as_deref() {
                Some(
                    reason @ ("agent_exited" | "agent_replaced" | "pane_closed" | "pane_exited"),
                ) => reason,
                _ => "agent_closed",
            };
            return Err(ApiError::new(code, format!("agent {:?} is no longer active", agent.name))
                .details(serde_json::to_value(agent).unwrap_or(Value::Null)));
        }
        Ok(agent)
    }

    pub(crate) fn active_agent(
        &self,
        selector: &str,
        generation: Option<u64>,
    ) -> Result<RuntimeManagedAgent, ApiError> {
        self.managed_agent(selector, generation, true)
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

const PANE_LIFECYCLE_CACHE: usize = 256;

fn observe_removed_panes(state: &mut HubState, snapshot: &RuntimeSnapshot) {
    let Some(current) = state.current.as_ref() else { return };
    let live: std::collections::HashSet<_> = snapshot
        .windows
        .iter()
        .flat_map(|window| {
            window
                .tabs
                .iter()
                .flat_map(move |tab| tab.panes.iter().map(move |pane| (window.id, pane.id)))
        })
        .collect();
    let removed: Vec<_> = current
        .windows
        .iter()
        .flat_map(|window| {
            window
                .tabs
                .iter()
                .flat_map(move |tab| tab.panes.iter().map(move |pane| (window.id, pane.id)))
        })
        .filter(|target| !live.contains(target))
        .collect();
    for (window_id, pane_id) in removed {
        record_pane_lifecycle_locked(state, window_id, pane_id, RuntimePaneLifecycleKind::Closed);
    }
}

fn record_pane_lifecycle_locked(
    state: &mut HubState,
    window_id: u64,
    pane_id: u64,
    event: RuntimePaneLifecycleKind,
) -> bool {
    // The first terminal event is the cause. A real PTY exit must not be
    // overwritten by the shutdown/close that the UI performs immediately
    // afterwards.
    if state
        .pane_lifecycles
        .iter()
        .any(|lifecycle| lifecycle.window_id == window_id && lifecycle.pane_id == pane_id)
    {
        return false;
    }
    state.next_pane_lifecycle = state.next_pane_lifecycle.saturating_add(1);
    state.pane_lifecycles.push_back(RuntimePaneLifecycle {
        sequence: state.next_pane_lifecycle,
        window_id,
        pane_id,
        event,
    });
    while state.pane_lifecycles.len() > PANE_LIFECYCLE_CACHE {
        state.pane_lifecycles.pop_front();
    }
    let closed_reason = event.error_code();
    for agent in state
        .managed_agents
        .values_mut()
        .filter(|agent| agent.active && agent.window_id == window_id && agent.pane_id == pane_id)
    {
        agent.active = false;
        agent.closed_reason = Some(closed_reason.to_owned());
    }
    true
}

fn notify_snapshot_subscribers(state: &mut HubState, snapshot: &RuntimeSnapshot) {
    state.subscribers.retain(|(_, sender)| match sender.try_send(snapshot.clone()) {
        Ok(()) => true,
        // A subscriber that cannot keep up is disconnected instead of
        // back-pressuring the GUI event thread.
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    });
}

fn project_managed_agents(state: &mut HubState, snapshot: &mut RuntimeSnapshot) -> bool {
    let mut lifecycle_changed = false;
    for managed in state.managed_agents.values_mut().filter(|agent| agent.active) {
        let pane =
            snapshot.windows.iter_mut().find(|window| window.id == managed.window_id).and_then(
                |window| {
                    window
                        .tabs
                        .iter_mut()
                        .flat_map(|tab| tab.panes.iter_mut())
                        .find(|pane| pane.id == managed.pane_id)
                },
            );
        let Some(pane) = pane else {
            managed.active = false;
            managed.closed_reason = Some("pane_closed".to_owned());
            lifecycle_changed = true;
            continue;
        };
        match &mut pane.agent {
            Some(agent) if agent.kind == managed.kind => {
                let session_mismatch = managed
                    .session_id
                    .as_deref()
                    .zip(agent.session_id.as_deref())
                    .is_some_and(|(expected, actual)| expected != actual);
                let identity_mismatch =
                    agent.agent_id.as_deref().is_some_and(|agent_id| agent_id != managed.agent_id);
                if session_mismatch || identity_mismatch {
                    managed.active = false;
                    managed.closed_reason = Some("agent_replaced".to_owned());
                    lifecycle_changed = true;
                } else {
                    if managed.session_id.is_none() && agent.session_id.is_some() {
                        managed.session_id.clone_from(&agent.session_id);
                        lifecycle_changed = true;
                    }
                    if !managed.observed {
                        managed.observed = true;
                        lifecycle_changed = true;
                    }
                    agent.agent_id = Some(managed.agent_id.clone());
                    agent.generation = Some(managed.generation);
                    agent.name = Some(managed.name.clone());
                    agent.worktree.clone_from(&managed.worktree);
                }
            },
            Some(_) if managed.observed => {
                managed.active = false;
                managed.closed_reason = Some("agent_replaced".to_owned());
                lifecycle_changed = true;
            },
            None if managed.observed => {
                managed.active = false;
                managed.closed_reason = Some("agent_exited".to_owned());
                lifecycle_changed = true;
            },
            Some(_) | None => {},
        }
    }
    lifecycle_changed
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
        "agents.list" => agent_api::agents_connection(&mut stream, request, hub),
        "agent.get" => agent_api::agent_get_connection(&mut stream, request, hub),
        "agent.wait" => agent_api::agent_wait_connection(&mut stream, request, hub),
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
            "agent.start",
            "agent.fork",
            "agent.get",
            "agent.prompt",
            "agent.read",
            "agent.wait",
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
            "pane.wait.after_seq",
            "pane.wait.lifecycle",
            "agent.wait.identity",
            "agent.fork.transactional_worktree",
            "agent.worktree.provenance",
            "events.pane_lifecycle"
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
    let mut target_window = params.window_id;
    if let Some(snapshot) = current {
        match snapshot.pane_target(target_window, params.pane_id) {
            Ok((window_id, pane)) => {
                target_window = Some(window_id);
                if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
                    return write_response(stream, &ApiResponse::failure(request.id, error));
                }
                if wait_matches(pane, params.state, params.after_seq) {
                    return write_response(
                        stream,
                        &ApiResponse::success(
                            request.id,
                            serde_json::to_value(snapshot).map_err(IoError::other)?,
                        ),
                    );
                }
                observed = Some((pane.task_state, pane.state_change_seq));
            },
            Err(error) if error.code == "target_not_found" => {
                if let Some(lifecycle) = hub.pane_lifecycle_error(target_window, params.pane_id) {
                    return write_response(stream, &ApiResponse::failure(request.id, lifecycle));
                }
            },
            Err(error) => {
                return write_response(stream, &ApiResponse::failure(request.id, error));
            },
        }
    } else if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
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
        if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
            return write_response(stream, &ApiResponse::failure(request.id, error));
        }
        match snapshot.pane_target(target_window, params.pane_id) {
            Ok((window_id, pane)) => {
                target_window = Some(window_id);
                if wait_matches(pane, params.state, params.after_seq) {
                    return write_response(
                        stream,
                        &ApiResponse::success(
                            request.id,
                            serde_json::to_value(snapshot).map_err(IoError::other)?,
                        ),
                    );
                }
                observed = Some((pane.task_state, pane.state_change_seq));
            },
            Err(error) => {
                return write_response(stream, &ApiResponse::failure(request.id, error));
            },
        }
    }

    if let Some(error) = hub.pane_lifecycle_error(target_window, params.pane_id) {
        return write_response(stream, &ApiResponse::failure(request.id, error));
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
                "window_id": target_window,
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
    let (command, mut worktree_transaction) =
        match agent_api::prepare_dispatch_command(command, hub) {
            Ok(prepared) => prepared,
            Err(error) => return write_response(stream, &ApiResponse::failure(request.id, error)),
        };
    match &command {
        RuntimeCommand::SendKey { window_id, pane_id, key, modifiers, repeat } => {
            info!(
                "runtime pane.send_key request_id={} window_id={window_id:?} pane_id={pane_id} key={} shift={} alt={} control={} repeat={repeat}",
                request.id,
                key.as_str(),
                modifiers.shift,
                modifiers.alt,
                modifiers.control,
            );
        },
        RuntimeCommand::AgentStart { window_id, name, kind, session_id, worktree, .. } => {
            info!(
                "runtime agent.start request_id={} window_id={window_id:?} name={} kind={} resume={} worktree={}",
                request.id,
                name,
                kind.slug(),
                session_id.is_some(),
                worktree.is_some()
            );
        },
        RuntimeCommand::AgentFork { .. } => {
            unreachable!("agent.fork must be prepared before UI dispatch")
        },
        RuntimeCommand::AgentPrompt { agent, generation, text, submit } => {
            info!(
                "runtime agent.prompt request_id={} agent={} generation={generation:?} submit={submit} prompt_bytes={}",
                request.id,
                agent,
                text.len()
            );
        },
        RuntimeCommand::AgentRead { agent, generation, lines } => {
            info!(
                "runtime agent.read request_id={} agent={} generation={generation:?} lines={lines}",
                request.id, agent
            );
        },
        _ => {},
    }
    let run_wait = match &command {
        RuntimeCommand::Run { wait: true, timeout_ms, .. } => {
            Some(Duration::from_millis(*timeout_ms))
        },
        _ => None,
    };
    let (dispatch, receiver) = RuntimeDispatch::new(command);
    if !sink.emit_control(dispatch) {
        let error = agent_api::rollback_prepared_worktree(
            ApiError::new("runtime_unavailable", "Nebula's event loop is not available"),
            worktree_transaction.take(),
        );
        return write_response(stream, &ApiResponse::failure(request.id, error));
    }
    let response = match receiver.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(mut result)) => {
            if let Some(transaction) = worktree_transaction.take() {
                let provenance = transaction.commit();
                agent_api::attach_worktree_result(&mut result, &provenance);
            }
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
        Ok(Err(error)) => ApiResponse::failure(
            request.id,
            agent_api::rollback_prepared_worktree(error, worktree_transaction.take()),
        ),
        Err(_) => {
            let mut error =
                ApiError::new("runtime_timeout", "runtime event thread did not answer in time");
            if let Some(transaction) = worktree_transaction.take() {
                // UI dispatch may still arrive after the response channel times out. The
                // checkout must stay alive because a late-created PTY can already use it.
                let provenance = transaction.commit();
                error.details = Some(json!({
                    "worktree": provenance,
                    "cleanup_deferred": true,
                    "reason": "the UI outcome is unknown; Nebula did not remove the worktree"
                }));
            }
            ApiResponse::failure(request.id, error)
        },
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
