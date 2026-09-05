//! Real-time AI-CLI turn state: typed lifecycle events from Claude Code,
//! Codex, Pi and opencode into the sidebar dots and notification center.
//!
//! # Why hooks, not the notification channel
//!
//! Claude Code's terminal notifications (`preferredNotifChannel`) are a dead
//! end on Windows: `auto` only recognizes a fixed set of terminal identifiers
//! and silently resolves to "no method available" everywhere else
//! (verified by decompiling claude 2.1.158; there is no env-var override
//! either). Rewriting `~/.claude.json` from outside is worse: claude rewrites
//! that file wholesale with no lock (anthropics/claude-code#28922), so
//! external edits get clobbered. Hooks are the reliable seam: they fire
//! INDEPENDENTLY of the notification channel, they carry typed semantics plus
//! the message text, and they live in `~/.claude/settings.json` — user-owned,
//! never rewritten by claude itself.
//!
//! * `UserPromptSubmit` → a turn started (sidebar spinner resumes),
//! * `Stop`             → the turn finished (dot + toast),
//! * `Notification`     → claude needs the user (permission/idle) + message.
//!
//! # The chain
//!
//! ```text
//! claude hook / codex notify
//!   └─▶ nebula-hook.exe             std-only bridge, <15 ms
//!         │  reads NEBULA_NOTIFY_PIPE + NEBULA_PANE_ID from its env
//!         │  (absent outside Nebula → exits silently: the config is
//!         │   global, the effect is Nebula-scoped)
//!         ▼
//!   \\.\pipe\nebula-notify-<pid>    per-instance named pipe (this module)
//!         ▼
//!   EventType::AiHook               winit user event, routed by pane id
//!         ▼
//!   WindowContext::handle_ai_hook   turn state + tab dot + toast
//! ```
//!
//! # Self-healing config (the ccswitch problem)
//!
//! Anything may rewrite `settings.json` wholesale (config switchers like
//! cc-switch do exactly that), silently dropping our hook entries. Two layers
//! put them back:
//!
//! 1. every boot runs [`win::ensure_claude_hooks`] (idempotent, atomic
//!    tmp+rename write, one-time `.nebula-bak` backup, refuses to touch a
//!    file it cannot parse);
//! 2. a watcher on the claude config directory re-runs it whenever
//!    `settings.json` changes, healing a wipe in under a second. Claude
//!    snapshots hooks per session, so running sessions keep firing and new
//!    sessions read the healed file — the coverage hole is ~zero.
//!
//! Our own atomic write triggers the watcher once; the re-check finds the
//! hooks present, writes nothing, and the cycle terminates.
//!
//! `nebula setup-ai [--remove]` does the same install/uninstall explicitly.
//!
//! # 诊断
//!
//! 事件走不通时有两个落点，都不需要重新编译：
//!
//! * 设 `NEBULA_HOOK_LOG=<path>` 后重启 Nebula，`nebula-hook` 会为每次调用追加
//!   一行 NDJSON，说明它把信封送去了哪里（`sent` / `pipe-unavailable` /
//!   `not-hosted` / `remote-osc` / `foreign-runner`）。只记路由事实，不记载荷。
//! * 进程内被拦下的事件由 [`GateVerdict`] 给出具体原因，写在 debug 日志里。
//!
//! 两者合起来能区分「helper 没送到」和「送到了但被规则拦下」——这是这条链路上
//! 最容易互相误判的两类故障。

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Environment variable carrying this instance's pipe name into child shells
/// (ConPTY merges the current process environment, so setting it process-wide
/// before the first PTY spawn covers every pane).
pub const PIPE_ENV: &str = "NEBULA_NOTIFY_PIPE";
/// Per-pane identity, injected into each pane's PTY environment.
pub const PANE_ENV: &str = "NEBULA_PANE_ID";
/// Absolute path of `nebula-hook.exe`, exported so the opencode Bun plugin
/// (which cannot resolve nebula.exe's install dir on its own) can shell out to
/// the bridge. Same process-wide scope as [`PIPE_ENV`].
pub const HOOK_EXE_ENV: &str = "NEBULA_HOOK_EXE";

/// Marker locating our entries inside `settings.json` — matches on the
/// helper's name so entries survive Nebula moving to a new absolute path.
const HELPER_MARK: &str = "nebula-hook";

/// The hook entry's argv tail. `claude` is the source discriminator
/// `nebula-hook` reads from `args[0]`, and it must travel as a real argument:
/// appended to the command string instead, some shell has to re-parse the whole
/// line, which is exactly what broke in #80.
const HELPER_ARGS: [&str; 1] = ["claude"];

/// Claude hook events we subscribe to. Session boundaries carry the id needed
/// for resume/fork; PostToolUse lets a stale permission state return to working
/// before the whole turn completes.
const CLAUDE_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "Notification",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

const MESSAGE_MAX_CHARS: usize = 300;
/// Codex's structured final answer feeds Agent delegation callbacks. Keep a
/// larger bounded copy than the notification preview so the origin Agent can
/// summarize a useful result without reading the target terminal again.
const TURN_RESULT_MAX_CHARS: usize = 4_000;
const ID_MAX_CHARS: usize = 512;
const CONTEXT_STRING_MAX_CHARS: usize = 1_024;
const RAW_CONTEXT_MAX_BYTES: usize = 16 * 1_024;
const MAX_TRACKED_STREAMS: usize = 512;
const MAX_EVENT_IDS_PER_STREAM: usize = 64;
const DUPLICATE_WINDOW_MS: u64 = 1_500;

static RECEIVE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static EVENT_GATE: LazyLock<Mutex<AiHookEventGate>> =
    LazyLock::new(|| Mutex::new(AiHookEventGate::default()));

/// What a lifecycle event means for the pane's turn state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiHookKind {
    /// Agent process/session became live; usually the earliest session-id edge.
    SessionStart,
    /// The user submitted a prompt: a turn is running.
    PromptSubmit,
    /// A tool completed; clears a stale permission/question wait.
    ToolComplete,
    /// The turn finished; the CLI waits for the next instruction.
    TurnDone,
    /// The CLI needs the user NOW (permission prompt, idle reminder).
    NeedsAttention,
    /// Agent session shut down and no longer owns the pane.
    SessionEnd,
}

/// Provider 的 Hook 能力并不对称。这里描述 Nebula 当前实际安装的桥接能力，
/// 避免上层把“有生命周期 Hook”误当成“也有权限上下文或事件顺序保证”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiHookCapabilities {
    pub attention_context: bool,
    pub background_tasks: bool,
    /// Nebula 自己的 bridge 是否为事件盖了单调序号。**没有任何 provider 提供
    /// 原生顺序字段**：opencode/pi 的序号由我们注入的 plugin/extension 生成
    /// （启动纪元 × 1e6 + 自增），claude/codex 的 hook 完全没有顺序信息，只能
    /// 依赖本地到达顺序。名字里是 bridge 而不是 provider，正是这个原因。
    pub bridge_sequence: bool,
    pub serialized_delivery: bool,
}

pub fn capabilities_for(source: &str) -> AiHookCapabilities {
    match source {
        "claude" => AiHookCapabilities {
            attention_context: true,
            background_tasks: true,
            bridge_sequence: false,
            serialized_delivery: false,
        },
        "opencode" => AiHookCapabilities {
            attention_context: true,
            background_tasks: false,
            bridge_sequence: true,
            serialized_delivery: true,
        },
        "pi" => AiHookCapabilities {
            attention_context: false,
            background_tasks: false,
            bridge_sequence: true,
            serialized_delivery: false,
        },
        // Codex notify 当前只给 turn-complete；没有 permission payload，也没有
        // 可验证的 provider sequence。接收顺序只能代表本机实际到达顺序。
        "codex" => AiHookCapabilities {
            attention_context: false,
            background_tasks: false,
            bridge_sequence: false,
            serialized_delivery: false,
        },
        _ => AiHookCapabilities {
            attention_context: false,
            background_tasks: false,
            bridge_sequence: false,
            serialized_delivery: false,
        },
    }
}

/// Agent 当前的权限档位。**这不是「正在等你批准」**，两者必须分开：
///
/// * `BypassPermissions` 是一个持续状态——用户用 `--dangerously-skip-permissions`
///   起的会话根本不会来问，把它当成 awaiting 会让徽标永远误亮；
/// * `NeedsAttention` 是一次瞬时事件，只有真的卡住等人时才发。
///
/// 合起来才能正确回答「这个 pane 现在是不是在无人监督地改我的仓库」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiPermissionMode {
    /// 每次动作都会问。
    Default,
    /// 自动同意文件编辑，其余仍会问。
    AcceptEdits,
    /// 全部跳过：不会有任何权限请求抵达。
    BypassPermissions,
    /// 只读规划，不落盘。
    Plan,
}

impl AiPermissionMode {
    /// Claude 的 hook 载荷直接带 `permission_mode`，这比读 agent 进程的 argv
    /// 可靠得多——会话中途切档、包装脚本启动、`npx claude` 都不会反映在命令行
    /// 里。Windows 上读别的进程命令行还要 WMI 或 PEB 遍历，代价与收益完全不成
    /// 比例，所以这里只认 provider 自己声明的值，读不到就是 `None`。
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "default" => Some(Self::Default),
            "acceptEdits" | "accept_edits" => Some(Self::AcceptEdits),
            "bypassPermissions" | "bypass_permissions" => Some(Self::BypassPermissions),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }

    /// 这一档会不会产生权限请求。用来判断一个没有明确类型的通知该不该被解释
    /// 成「等你批准」。
    pub fn can_ask_for_permission(self) -> bool {
        !matches!(self, Self::BypassPermissions)
    }
}

/// Claude `Stop` 会携带本回合的后台 Task 列表。主回合停止不等于后台
/// subagent 已经停止；至少一个 task 仍 running 时，Pane 必须保持 Working。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AiBackgroundTasks {
    pub active: u32,
    pub total: u32,
}

/// 权限/等待输入事件的可行动上下文。`raw_context` 是经过字段脱敏、深度和
/// 体积限制的副本；它绝不能等同于 provider 的原始 stdin，也不能进 Debug 日志。
#[derive(Clone)]
pub struct AttentionContext {
    pub source: String,
    pub pane_id: Option<u64>,
    pub session_id: Option<String>,
    pub event_kind: AiHookKind,
    pub event_id: Option<String>,
    pub bridge_sequence: Option<u64>,
    /// Provider 声明的 Unix 时间戳（毫秒）；没有可靠字段时保持 None。
    pub occurred_at_ms: Option<u64>,
    /// Nebula 完成 envelope 解析的 Unix 时间戳（毫秒）。
    pub received_at_ms: u64,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub git_branch: Option<String>,
    pub permission_or_tool: Option<String>,
    /// 事件抵达时 agent 声明的权限档位。`BypassPermissions` 时这条 attention
    /// 一定不是「等你批准」（那种会话不会来问），只可能是等你输入——UI 的文案
    /// 必须据此区分，否则用户会以为有个批准按钮在等他。
    pub permission_mode: Option<AiPermissionMode>,
    pub message: Option<String>,
    pub selection: Option<String>,
    pub raw_context: Option<String>,
}

impl std::fmt::Debug for AttentionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttentionContext")
            .field("source", &self.source)
            .field("pane_id", &self.pane_id)
            .field("session_id", &self.session_id)
            .field("event_kind", &self.event_kind)
            .field("event_id", &self.event_id)
            .field("bridge_sequence", &self.bridge_sequence)
            .field("occurred_at_ms", &self.occurred_at_ms)
            .field("received_at_ms", &self.received_at_ms)
            .field("cwd", &self.cwd)
            .field("project", &self.project)
            .field("git_branch", &self.git_branch)
            .field("permission_or_tool", &self.permission_or_tool)
            .field("permission_mode", &self.permission_mode)
            .field("message", &self.message)
            .field("selection_chars", &self.selection.as_ref().map(|s| s.chars().count()))
            .field("raw_context_bytes", &self.raw_context.as_ref().map(String::len))
            .finish()
    }
}

impl AttentionContext {
    /// 通知正文只放定位和请求摘要；选区正文、raw context 从不进入系统通知。
    pub fn summary_for_pane(&self, pane_id: u64) -> String {
        let mut parts = Vec::with_capacity(4);
        if let Some(project) = self.project.as_deref().or(self.cwd.as_deref()) {
            parts.push(truncate(project, 120));
        }
        parts.push(format!("Pane {pane_id}"));
        if let Some(request) = self.permission_or_tool.as_deref() {
            parts.push(truncate(request, 100));
        }
        if let Some(message) = self.message.as_deref() {
            let message = truncate(message, MESSAGE_MAX_CHARS);
            if !parts.iter().any(|part| part == &message) {
                parts.push(message);
            }
        }
        if self.selection.is_some() {
            parts.push("selection context".to_owned());
        }
        parts.join(" · ")
    }
}

/// A typed AI-CLI lifecycle event, parsed from one pipe connection.
#[derive(Debug, Clone)]
pub struct AiHookEvent {
    pub answer: Option<crate::assistant_answer::AssistantAnswer>,
    pub answer_cwd: Option<std::path::PathBuf>,
    /// Pane hosting the CLI (from `NEBULA_PANE_ID`); `None` falls back to the
    /// focused pane (only happens when the env was stripped along the way).
    pub pane: Option<u64>,
    /// AI CLI identity, used as the toast title.
    pub source: String,
    pub kind: AiHookKind,
    /// Human text when the event carries one (claude's notification message,
    /// codex's last assistant message).
    pub message: Option<String>,
    /// CLI 自己的会话身份：claude hook 载荷的 `session_id`、codex notify 的
    /// `thread-id`（即 rollout 文件名尾部的 uuid，`codex resume` 认它）。
    /// 冷恢复接续对话的唯一事实源——文件系统扫描只能靠 mtime 猜。
    pub session_id: Option<String>,
    /// Provider 给出的幂等身份；只在同一 source/session/pane 内去重。
    pub event_id: Option<String>,
    /// Nebula bridge 盖的单调序号（不是 provider 原生顺序）。没有时只承诺
    /// 本地接收顺序。
    pub bridge_sequence: Option<u64>,
    pub occurred_at_ms: Option<u64>,
    pub received_at_ms: u64,
    /// 进程内严格单调的接收序号，用于稳定批处理；不冒充 provider 顺序。
    pub received_sequence: u64,
    /// 写入命名管道的那个进程的真实 pid，由内核回答
    /// （`GetNamedPipeClientProcessId`），不是载荷里自报的。路由的第二因子：
    /// 校验它是否真的跑在声明的 pane 进程树内。远端 SSH 走 OSC 通道，没有本地
    /// 客户端进程，因此恒为 `None`。
    pub client_pid: Option<u32>,
    /// 沿 [`Self::client_pid`] 祖先链找到的最近 AI CLI 进程的 pid。这是 agent 的
    /// **进程身份**，用来区分嵌套子代理：`claude -p` 起的子代理有自己的 pid，
    /// 它的 session id 却是短命的——一旦被当成 pane 的会话身份，就会把真正活着
    /// 的那个顶掉。远端 SSH 没有本地进程，恒为 `None`。
    pub agent_pid: Option<u32>,
    /// Agent 自己声明的权限档位。与 [`AiHookKind::NeedsAttention`] 是两件事：
    /// 前者是持续状态，后者是瞬时事件。
    pub permission_mode: Option<AiPermissionMode>,
    pub background_tasks: Option<AiBackgroundTasks>,
    pub attention: Option<AttentionContext>,
}

impl AiHookEvent {
    pub fn capabilities(&self) -> AiHookCapabilities {
        capabilities_for(&self.source)
    }

    pub fn active_background_tasks(&self) -> u32 {
        self.background_tasks.map_or(0, |tasks| tasks.active)
    }

    fn stream_key(&self, pane: Option<u64>) -> AiHookStreamKey {
        AiHookStreamKey {
            source: self.source.clone(),
            session_id: self.session_id.clone(),
            pane,
            agent_pid: self.agent_pid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AiHookStreamKey {
    source: String,
    session_id: Option<String>,
    pane: Option<u64>,
    /// agent 的进程身份。同一个 pane 里主 agent 与它 spawn 的子代理各占一条
    /// 流：两者的 session id 也不同，但那个值可能缺失，pid 不会。
    agent_pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamLifecycle {
    Active,
    Blocked,
    Done,
    Ended,
}

#[derive(Debug)]
struct AiHookStreamState {
    last_bridge_sequence: Option<u64>,
    last_occurred_at_ms: Option<u64>,
    last_received_sequence: u64,
    lifecycle: StreamLifecycle,
    seen_event_ids: VecDeque<String>,
    last_fingerprint: Option<(u64, u64)>,
}

impl AiHookStreamState {
    fn new(event: &AiHookEvent) -> Self {
        Self {
            last_bridge_sequence: None,
            last_occurred_at_ms: None,
            last_received_sequence: event.received_sequence,
            lifecycle: StreamLifecycle::Active,
            seen_event_ids: VecDeque::new(),
            last_fingerprint: None,
        }
    }
}

#[derive(Debug, Default)]
struct AiHookEventGate {
    streams: HashMap<AiHookStreamKey, AiHookStreamState>,
}

/// 事件门的判定结果。带原因，而不只是一个 bool——「通知没出现」这类问题事后
/// 唯一的线索就是这个原因，日志里必须说得出是哪一条规则拦的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    Accepted,
    /// 同一个 `event_id` 已经处理过。
    DuplicateEventId,
    /// bridge 序号不比上一次大（重放或迟到）。
    StaleSequence,
    /// 没有序号，但 provider 时间戳比上一次早。
    StaleTime,
    /// 完全没有身份元数据的终态事件，在短窗口内重复抵达。
    DuplicateFingerprint,
    /// 该 session 已经 SessionEnd，只有 SessionStart 能复活。
    AfterSessionEnd,
    /// Done 之后抵达的 ToolComplete，且没有任何证据证明它更新。
    UnorderedAfterDone,
}

impl GateVerdict {
    pub fn accepted(self) -> bool {
        self == Self::Accepted
    }
}

impl AiHookEventGate {
    #[cfg(test)]
    fn accept(&mut self, event: &AiHookEvent, pane_id: u64) -> bool {
        self.verdict(event, pane_id).accepted()
    }

    fn verdict(&mut self, event: &AiHookEvent, pane_id: u64) -> GateVerdict {
        let key = event.stream_key(Some(pane_id));
        if !self.streams.contains_key(&key) && self.streams.len() >= MAX_TRACKED_STREAMS {
            if let Some(oldest) = self
                .streams
                .iter()
                .min_by_key(|(_, state)| state.last_received_sequence)
                .map(|(key, _)| key.clone())
            {
                self.streams.remove(&oldest);
            }
        }
        let state = self.streams.entry(key).or_insert_with(|| AiHookStreamState::new(event));

        if let Some(event_id) = event.event_id.as_deref()
            && state.seen_event_ids.iter().any(|seen| seen == event_id)
        {
            return GateVerdict::DuplicateEventId;
        }

        let provider_order = event
            .bridge_sequence
            .zip(state.last_bridge_sequence)
            .map(|(current, previous)| current.cmp(&previous));
        let time_order = event
            .occurred_at_ms
            .zip(state.last_occurred_at_ms)
            .map(|(current, previous)| current.cmp(&previous));
        if provider_order.is_some_and(|order| order != std::cmp::Ordering::Greater) {
            return GateVerdict::StaleSequence;
        }
        if provider_order.is_none() && time_order == Some(std::cmp::Ordering::Less) {
            return GateVerdict::StaleTime;
        }
        let strictly_newer = provider_order == Some(std::cmp::Ordering::Greater)
            || (provider_order.is_none() && time_order == Some(std::cmp::Ordering::Greater));

        let fingerprint = event_fingerprint(event);
        let use_fingerprint = event.event_id.is_none()
            && event.bridge_sequence.is_none()
            && event.occurred_at_ms.is_none()
            && matches!(
                event.kind,
                AiHookKind::TurnDone | AiHookKind::NeedsAttention | AiHookKind::SessionEnd
            );
        if use_fingerprint
            && let Some((previous, at)) = state.last_fingerprint
            && previous == fingerprint
            && event.received_at_ms.saturating_sub(at) <= DUPLICATE_WINDOW_MS
        {
            return GateVerdict::DuplicateFingerprint;
        }

        match state.lifecycle {
            StreamLifecycle::Ended if event.kind != AiHookKind::SessionStart => {
                return GateVerdict::AfterSessionEnd;
            },
            // Permission granted 后 PostToolUse 合法地把 Blocked 拉回 Working。
            StreamLifecycle::Blocked => {},
            // Done 后的无序 ToolComplete 最常见于迟到 Hook。只有 bridge
            // sequence/time 明确证明更新，或发送端保证串行时才允许恢复。
            StreamLifecycle::Done
                if event.kind == AiHookKind::ToolComplete
                    && !strictly_newer
                    && !(event.capabilities().serialized_delivery
                        && event.bridge_sequence.is_some()) =>
            {
                return GateVerdict::UnorderedAfterDone;
            },
            _ => {},
        }

        if event.kind == AiHookKind::SessionStart {
            state.seen_event_ids.clear();
        }
        if let Some(event_id) = event.event_id.as_deref() {
            if state.seen_event_ids.len() == MAX_EVENT_IDS_PER_STREAM {
                state.seen_event_ids.pop_front();
            }
            state.seen_event_ids.push_back(event_id.to_owned());
        }
        if let Some(sequence) = event.bridge_sequence {
            state.last_bridge_sequence = Some(sequence);
        }
        if let Some(occurred_at_ms) = event.occurred_at_ms {
            state.last_occurred_at_ms = Some(occurred_at_ms);
        }
        state.last_received_sequence = state.last_received_sequence.max(event.received_sequence);
        state.last_fingerprint = use_fingerprint.then_some((fingerprint, event.received_at_ms));
        state.lifecycle = match event.kind {
            AiHookKind::SessionStart | AiHookKind::PromptSubmit | AiHookKind::ToolComplete => {
                StreamLifecycle::Active
            },
            AiHookKind::TurnDone if event.active_background_tasks() > 0 => StreamLifecycle::Active,
            AiHookKind::TurnDone => StreamLifecycle::Done,
            AiHookKind::NeedsAttention => StreamLifecycle::Blocked,
            AiHookKind::SessionEnd => StreamLifecycle::Ended,
        };
        GateVerdict::Accepted
    }
}

fn event_fingerprint(event: &AiHookEvent) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    event.kind.hash(&mut hasher);
    event.message.hash(&mut hasher);
    event.answer.hash(&mut hasher);
    event.background_tasks.hash(&mut hasher);
    if let Some(attention) = event.attention.as_ref() {
        attention.cwd.hash(&mut hasher);
        attention.project.hash(&mut hasher);
        attention.permission_or_tool.hash(&mut hasher);
        attention.raw_context.hash(&mut hasher);
    }
    hasher.finish()
}

/// 在最终 Pane 已解析后调用。全进程共用一扇门，关闭/跨窗口移动期间不会为
/// 每个 view 留下互相矛盾的事件缓存；pane id 在本进程生命周期内不复用。
///
/// 返回带原因的判定：调用方必须把原因记进日志。事件被静默丢掉是这套链路里最
/// 难查的一类故障——用户看到的只是「通知没出现」。
pub(crate) fn accept_for_pane(event: &AiHookEvent, pane_id: u64) -> GateVerdict {
    EVENT_GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).verdict(event, pane_id)
}

/// 同一 pump 批次内，只有一组事件全部带 bridge sequence 时才按该序号
/// 重排；不同会话仍占据原来的交错槽位。跨批次的旧序号由事件门拒绝。
pub(crate) fn reorder_batch(events: Vec<AiHookEvent>) -> Vec<AiHookEvent> {
    let keys = events.iter().map(|event| event.stream_key(event.pane)).collect::<Vec<_>>();
    let mut groups: HashMap<AiHookStreamKey, VecDeque<AiHookEvent>> = HashMap::new();
    for (key, event) in keys.iter().cloned().zip(events) {
        groups.entry(key).or_default().push_back(event);
    }
    for group in groups.values_mut() {
        if group.len() > 1 && group.iter().all(|event| event.bridge_sequence.is_some()) {
            let mut ordered = group.drain(..).collect::<Vec<_>>();
            ordered.sort_by_key(|event| event.bridge_sequence);
            group.extend(ordered);
        }
    }
    keys.into_iter().filter_map(|key| groups.get_mut(&key).and_then(VecDeque::pop_front)).collect()
}

/// Parse one pipe message: a `nebula-hook/1 source=<s> pane=<n>` header line,
/// then the hook's raw JSON payload verbatim (the helper never re-encodes;
/// all JSON work happens here, off the turn's hot path).
fn parse_envelope(bytes: &[u8]) -> Option<AiHookEvent> {
    let received_at_ms = unix_time_ms();
    let received_sequence = RECEIVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nl = bytes.iter().position(|&b| b == b'\n')?;
    let header = std::str::from_utf8(&bytes[..nl]).ok()?.trim();
    let raw = &bytes[nl + 1..];

    let mut fields = header.split_whitespace();
    if fields.next() != Some("nebula-hook/1") {
        return None;
    }
    let (mut source, mut pane) = (None, None);
    for field in fields {
        match field.split_once('=') {
            Some(("source", v)) => source = Some(v.to_owned()),
            Some(("pane", v)) => pane = v.parse().ok(),
            _ => (),
        }
    }
    let source = source?;

    let payload: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    // 会话身份的候选字段名按 source 收紧。claude 每个 hook 载荷都带
    // `session_id`（snake_case）；codex notify 带 `thread-id`（kebab-case，即
    // rollout uuid）。**claude 分支绝不读 camelCase**：那是别家 hook runner 的
    // 写法，把它的 session id 当成 claude 的，就会把一个不存在的会话交给
    // `claude --resume`（见 nebula_hook 的 FOREIGN_HOOK_RUNNERS 注释）。
    let session_id_keys: &[&str] = match source.as_str() {
        "claude" => &["session_id"],
        "codex" => &["thread-id", "session_id"],
        // opencode/pi 由我们自己的 bridge 规范化成 snake_case；camelCase 是
        // provider SDK 原样透传时的兼容路径。
        _ => &["session_id", "sessionID", "sessionId"],
    };
    let session_id = session_id_keys
        .iter()
        .find_map(|key| payload.get(*key))
        .and_then(Value::as_str)
        .map(|id| truncate(id, ID_MAX_CHARS));
    let event_id =
        context_string(&payload, &["event_id", "eventId"]).map(|id| truncate(&id, ID_MAX_CHARS));
    // 旧字段名保留：用户机器上可能还装着上一版 bridge，它写的是
    // `provider_sequence`。字段来源始终是 bridge，不是 provider 原生顺序。
    let bridge_sequence = context_u64(
        &payload,
        &["bridge_sequence", "bridgeSequence", "provider_sequence", "providerSequence"],
    );
    let occurred_at_ms =
        context_u64(&payload, &["occurred_at_ms", "occurredAtMs", "occurred_at", "timestamp"]);
    let background_tasks = background_task_summary(&payload);
    let permission_mode = context_string(&payload, &["permission_mode", "permissionMode"])
        .as_deref()
        .and_then(AiPermissionMode::parse);
    let (kind, message) = match source.as_str() {
        // 第二道串台门。第一道在 nebula_hook 里靠环境变量判断调用方是不是别家
        // 的 hook runner；那种门会被上游改名静默失效，所以这里独立再拦一次：
        // 别家 runner 的载荷用 camelCase 字段名，claude 从不这样发。
        "claude" if payload.get("hookEventName").is_some() => return None,
        "claude" => match payload.get("hook_event_name").and_then(Value::as_str) {
            Some("SessionStart") => (AiHookKind::SessionStart, None),
            Some("UserPromptSubmit") => (AiHookKind::PromptSubmit, None),
            Some("PostToolUse") => (AiHookKind::ToolComplete, None),
            Some("Stop") => (AiHookKind::TurnDone, None),
            Some("SessionEnd") => (AiHookKind::SessionEnd, None),
            // `Notification` 覆盖「权限询问」和「idle 提醒」两类，将来也可能
            // 用来传别的东西。类型不可操作时丢掉，读不到类型时照常上报。
            Some("Notification") if !attention_is_actionable(&payload) => return None,
            Some("Notification") | Some("PermissionRequest") => {
                (AiHookKind::NeedsAttention, attention_message(&payload))
            },
            // SubagentStop and friends would only produce noise.
            _ => return None,
        },
        "codex" => match payload.get("type").and_then(Value::as_str) {
            Some("agent-turn-complete") => (
                AiHookKind::TurnDone,
                payload
                    .get("last-assistant-message")
                    .and_then(Value::as_str)
                    .map(|m| truncate(m, TURN_RESULT_MAX_CHARS)),
            ),
            _ => return None,
        },
        // opencode's Bun plugin normalizes its event bus into a tiny
        // `{"kind":"prompt|done|attention","message":?}` payload (see the
        // embedded plugin in `ensure_opencode_plugin`), so this side stays
        // decoupled from opencode's evolving SDK event schema.
        "opencode" | "pi" => match payload.get("kind").and_then(Value::as_str) {
            Some("session-start") => (AiHookKind::SessionStart, None),
            Some("prompt") => (AiHookKind::PromptSubmit, None),
            Some("tool-complete") => (AiHookKind::ToolComplete, None),
            Some("done") => (AiHookKind::TurnDone, None),
            Some("session-end") => (AiHookKind::SessionEnd, None),
            Some("attention") => (AiHookKind::NeedsAttention, attention_message(&payload)),
            _ => return None,
        },
        _ => return None,
    };
    let attention = (kind == AiHookKind::NeedsAttention).then(|| AttentionContext {
        source: source.clone(),
        pane_id: pane,
        session_id: session_id.clone(),
        event_kind: kind,
        event_id: event_id.clone(),
        bridge_sequence,
        occurred_at_ms,
        received_at_ms,
        cwd: context_string(&payload, &["cwd", "directory"]),
        project: context_string(
            &payload,
            &["project", "project_path", "projectPath", "workspace_root", "workspaceRoot"],
        )
        .or_else(|| first_string_in_array(&payload, "workspace_roots")),
        git_branch: context_string(&payload, &["git_branch", "gitBranch", "branch"]),
        permission_or_tool: context_string(
            &payload,
            &[
                "permission_or_tool",
                "permissionOrTool",
                "permission_type",
                "permissionType",
                "tool_name",
                "toolName",
                "tool",
            ],
        )
        .or_else(|| {
            payload
                .get("hook_event_name")
                .and_then(Value::as_str)
                .filter(|name| *name == "PermissionRequest")
                .map(str::to_owned)
        }),
        permission_mode,
        message: message.clone(),
        selection: context_string(&payload, &["selection", "selected_text", "selectedText"]),
        raw_context: sanitized_raw_context(&payload),
    });
    let answer = crate::assistant_answer::AssistantAnswer::from_hook(&source, &payload);
    let answer_cwd = payload.get("cwd").and_then(Value::as_str)
        .filter(|path| path.len() <= 4096 && !path.chars().any(char::is_control))
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute());
    Some(AiHookEvent {
        answer,
        answer_cwd,
        pane,
        source,
        kind,
        message,
        session_id,
        event_id,
        bridge_sequence,
        occurred_at_ms,
        received_at_ms,
        received_sequence,
        // 只有命名管道的服务端能从内核问出来；解析阶段一律留空。
        client_pid: None,
        agent_pid: None,
        permission_mode,
        background_tasks,
        attention,
    })
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
}

fn context_container<'a>(payload: &'a Value, key: &str) -> Option<&'a Value> {
    payload.get(key).filter(|value| value.is_object())
}

fn context_value<'a>(payload: &'a Value, names: &[&str]) -> Option<&'a Value> {
    for container in [
        Some(payload),
        context_container(payload, "context"),
        context_container(payload, "payload"),
        context_container(payload, "properties"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(value) = names.iter().find_map(|name| container.get(*name)) {
            return Some(value);
        }
    }
    None
}

fn context_string(payload: &Value, names: &[&str]) -> Option<String> {
    let value = context_value(payload, names)?;
    let value = value
        .as_str()
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| value.get("type").and_then(Value::as_str))?;
    (!value.trim().is_empty()).then(|| truncate(value.trim(), CONTEXT_STRING_MAX_CHARS))
}

fn context_u64(payload: &Value, names: &[&str]) -> Option<u64> {
    let value = context_value(payload, names)?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn first_string_in_array(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .and_then(|values| values.iter().find_map(Value::as_str))
        .map(|value| truncate(value, CONTEXT_STRING_MAX_CHARS))
}

fn attention_message(payload: &Value) -> Option<String> {
    context_string(payload, &["message", "title", "reason", "description"])
        .map(|message| truncate(&message, MESSAGE_MAX_CHARS))
}

/// 通知类型里代表「真的在等用户动作」的词根。用子串而不是精确枚举：provider
/// 改一次字面量（`permission` → `permission_prompt` → `tool_permission`）就要
/// 改一次白名单，而语义一直没变。
const ACTIONABLE_ATTENTION_MARKERS: &[&str] =
    &["permission", "approval", "approve", "confirm", "input", "idle", "wait"];

/// 这个通知是否值得让 pane 进入「等你」状态。
///
/// 判据是「不认识才丢，缺失要放行」，两个方向都是踩过的坑：
///
/// * **缺失要放行**——事件自身的文档语义就是「需要用户注意」。若写成「读不到
///   类型就丢」，provider 一旦把字段改名或改成嵌套结构，attention 徽标就被永久
///   静默杀死，而且没有任何报错。
/// * **能读到却不认识要丢**——那有可能是 agent 在说话而不是在阻塞等待，会把
///   pane 永远停在一个清不掉的等待图标上。
///
/// 只认 `notificationType` / `notification_type` 这一族字段名。故意不读 `type`
/// 和 `kind`：codex 用 `type` 表示 `agent-turn-complete`，我们自己的 opencode/pi
/// bridge 用 `kind` 表示生命周期阶段，把它们当通知类型会误伤真实事件。
fn attention_is_actionable(payload: &Value) -> bool {
    let Some(kind) = context_string(payload, &["notificationType", "notification_type"]) else {
        return true;
    };
    let kind = kind.to_ascii_lowercase();
    ACTIONABLE_ATTENTION_MARKERS.iter().any(|marker| kind.contains(marker))
}

fn background_task_summary(payload: &Value) -> Option<AiBackgroundTasks> {
    let tasks = payload.get("background_tasks")?;
    let mut active = 0u32;
    let mut total = 0u32;
    count_background_tasks(tasks, &mut active, &mut total);
    Some(AiBackgroundTasks { active, total })
}

fn count_background_tasks(value: &Value, active: &mut u32, total: &mut u32) {
    match value {
        Value::Array(values) => {
            for value in values {
                count_background_tasks(value, active, total);
            }
        },
        Value::Object(task) => {
            if let Some(kind) = task.get("type").and_then(Value::as_str) {
                if !kind.eq_ignore_ascii_case("subagent") {
                    return;
                }
                *total = total.saturating_add(1);
                if task.get("status").and_then(Value::as_str).is_some_and(|status| {
                    matches!(
                        status.to_ascii_lowercase().as_str(),
                        "running" | "processing" | "in_progress" | "active"
                    )
                }) {
                    *active = active.saturating_add(1);
                }
                return;
            }
            for value in task.values() {
                count_background_tasks(value, active, total);
            }
        },
        _ => {},
    }
}

fn sanitized_raw_context(payload: &Value) -> Option<String> {
    let sanitized = sanitize_json(payload, 0);
    let raw = serde_json::to_string(&sanitized).ok()?;
    if raw == "null" || raw == "{}" {
        return None;
    }
    if raw.len() <= RAW_CONTEXT_MAX_BYTES {
        return Some(raw);
    }
    let mut limited = truncate_utf8_bytes(&raw, RAW_CONTEXT_MAX_BYTES.saturating_sub(14));
    limited.push_str("...[truncated]");
    Some(limited)
}

fn sanitize_json(value: &Value, depth: usize) -> Value {
    if depth >= 6 {
        return Value::String("[depth limited]".to_owned());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(value) => Value::String(truncate(value, CONTEXT_STRING_MAX_CHARS)),
        Value::Array(values) => Value::Array(
            values.iter().take(24).map(|value| sanitize_json(value, depth + 1)).collect(),
        ),
        Value::Object(values) => {
            let mut sanitized = serde_json::Map::new();
            for (key, value) in values.iter().take(48) {
                let value = if sensitive_context_key(key) {
                    Value::String("[redacted]".to_owned())
                } else {
                    sanitize_json(value, depth + 1)
                };
                sanitized.insert(key.clone(), value);
            }
            Value::Object(sanitized)
        },
    }
}

fn sensitive_context_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "authorization",
        "cookie",
        "credential",
        "apikey",
        "accesskey",
        "privatekey",
        "environment",
        "env",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

/// 远端会话只能提交事件语义，Pane 身份始终由本地 SSH 通道覆盖，
/// 防止远端载荷把通知路由到同一窗口中的其他标签页。
pub(crate) fn parse_remote_envelope(bytes: &[u8], pane: Option<u64>) -> Option<AiHookEvent> {
    let mut event = parse_envelope(bytes)?;
    event.pane = pane;
    if let Some(attention) = event.attention.as_mut() {
        attention.pane_id = pane;
    }
    Some(event)
}

/// 「回合结束时 AI 是不是其实在等人回答」这个判断，2026-08-22 起统一走
/// `ai_agents::detect` 的 blocked 规则（`agent_detection/*.toml` 与
/// `_shared.toml`）。此处原有一份 `tail_looks_like_question` 裸关键词表，
/// 扫底部 15 行全文，只要 `(y/n)`、`do you want to proceed` 等字样落在正文
/// 任何位置就算数——agent 打印的代码片段、上一轮答完还没滚走的旧框都会让
/// 正常结束的回合挂上警告三角，而 Blocked 一旦点亮就再难落下。manifest 的
/// 规则带 region 锚定（`after_last_horizontal_rule` / 底部 N 行），只认当前
/// 活动框，两个壳现在共用它。

#[cfg(test)]
mod remote_tests {
    use super::{
        AiHookEventGate, AiHookKind, GateVerdict, RAW_CONTEXT_MAX_BYTES, capabilities_for,
        parse_remote_envelope, reorder_batch,
    };

    #[test]
    fn remote_envelope_uses_local_pane_identity() {
        let raw = b"nebula-hook/1 source=codex pane=999\n{\"type\":\"agent-turn-complete\",\"last-assistant-message\":\"done\"}";
        let event = parse_remote_envelope(raw, Some(7)).unwrap();
        assert_eq!(event.pane, Some(7));
        assert_eq!(event.kind, AiHookKind::TurnDone);
        assert_eq!(event.message.as_deref(), Some("done"));
    }

    #[test]
    fn claude_events_carry_their_session_id_for_cold_resume() {
        let raw = b"nebula-hook/1 source=claude pane=3\n{\"session_id\":\"0199a213-c2a4-7cf5-8f6b-d746fbb6e86c\",\"hook_event_name\":\"Stop\"}";
        let event = parse_remote_envelope(raw, Some(3)).unwrap();
        assert_eq!(event.session_id.as_deref(), Some("0199a213-c2a4-7cf5-8f6b-d746fbb6e86c"));
    }

    /// 串台门的第二层。第一层（`nebula_hook::foreign_hook_runner`）是环境变量
    /// 判据，会随上游改名静默失效；这条测试钉住「即使那扇门不响，载荷形状仍然
    /// 拦得住」。别家 hook runner 读我们装在 ~/.claude/settings.json 里的条目、
    /// 用 camelCase 发事件，若被当成 claude 上报，pane 会贴错 provider 身份，
    /// 而且它的 session id 会被交给 `claude --resume` —— 一个不存在的会话。
    #[test]
    fn foreign_runner_payload_is_rejected_even_if_the_env_gate_fails() {
        let raw = b"nebula-hook/1 source=claude pane=3\n{\"hookEventName\":\"Stop\",\"sessionId\":\"grok-session-1\",\"cwd\":\"D:/x\"}";
        assert!(parse_remote_envelope(raw, Some(3)).is_none());
    }

    /// claude 只发 snake_case。即使一个载荷同时带着合法的 `hook_event_name`
    /// 和别家风格的 `sessionId`，也绝不能把后者当成 claude 的会话身份。
    #[test]
    fn claude_session_id_never_comes_from_camel_case() {
        let raw = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Stop\",\"sessionId\":\"grok-session-1\"}";
        let event = parse_remote_envelope(raw, Some(3)).unwrap();
        assert_eq!(event.kind, AiHookKind::TurnDone);
        assert_eq!(event.session_id, None);
    }

    /// 防御式解析：不认识才丢，缺失要放行。两个方向都会咬人——读不到类型就丢
    /// 会让 provider 改一次字段名就永久静默 attention 徽标；能读到却不认识还
    /// 放行，会把 pane 停在一个清不掉的等待图标上。
    #[test]
    fn attention_type_gate_drops_only_types_it_can_read_and_does_not_know() {
        let missing = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Notification\",\"message\":\"Claude needs your permission\"}";
        assert_eq!(
            parse_remote_envelope(missing, Some(3)).map(|event| event.kind),
            Some(AiHookKind::NeedsAttention),
            "没有类型字段时必须照常上报"
        );

        let known = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Notification\",\"notificationType\":\"permission_prompt\"}";
        assert_eq!(
            parse_remote_envelope(known, Some(3)).map(|event| event.kind),
            Some(AiHookKind::NeedsAttention)
        );

        // 未来若 provider 换成 tool_permission / awaiting_input 之类的新写法，
        // 子串判据仍然命中，不需要跟着改白名单。
        let renamed = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Notification\",\"notification_type\":\"tool_permission_request\"}";
        assert_eq!(
            parse_remote_envelope(renamed, Some(3)).map(|event| event.kind),
            Some(AiHookKind::NeedsAttention)
        );

        // 这类是 agent 在说话，不是在阻塞等待：放行会让等待图标清不掉。
        let chatty = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Notification\",\"notificationType\":\"assistant_message\"}";
        assert!(parse_remote_envelope(chatty, Some(3)).is_none());

        // PermissionRequest 本身就是权限请求，不受通知类型字段影响。
        let explicit = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"PermissionRequest\",\"notificationType\":\"assistant_message\"}";
        assert_eq!(
            parse_remote_envelope(explicit, Some(3)).map(|event| event.kind),
            Some(AiHookKind::NeedsAttention)
        );
    }

    /// bypass 是持续状态，awaiting 是瞬时事件——两者必须分开读，否则「用户
    /// 全局跳过权限」会被显示成「正在等你批准」，徽标永远误亮。
    #[test]
    fn permission_mode_is_reported_separately_from_attention() {
        let bypass = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Notification\",\"permission_mode\":\"bypassPermissions\",\"message\":\"Claude is waiting for your input\"}";
        let event = parse_remote_envelope(bypass, Some(3)).unwrap();
        // 事件本身仍然是「需要你」——bypass 会话也会等你输入下一条指令。
        assert_eq!(event.kind, AiHookKind::NeedsAttention);
        // 但它不可能是在等批准，UI 的文案要据此区分。
        assert_eq!(event.permission_mode, Some(super::AiPermissionMode::BypassPermissions));
        assert!(!event.permission_mode.unwrap().can_ask_for_permission());
        assert_eq!(
            event.attention.as_ref().and_then(|context| context.permission_mode),
            Some(super::AiPermissionMode::BypassPermissions)
        );

        let asking = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"PermissionRequest\",\"permission_mode\":\"default\",\"tool_name\":\"Bash\"}";
        let event = parse_remote_envelope(asking, Some(3)).unwrap();
        assert!(event.permission_mode.unwrap().can_ask_for_permission());
        assert_eq!(
            event.attention.as_ref().and_then(|c| c.permission_or_tool.as_deref()),
            Some("Bash")
        );

        // 读不到就是读不到：不拿 argv 猜，也不默认成 default。
        let silent = b"nebula-hook/1 source=claude pane=3\n{\"hook_event_name\":\"Stop\"}";
        assert_eq!(parse_remote_envelope(silent, Some(3)).unwrap().permission_mode, None);
    }

    #[test]
    fn codex_thread_id_is_the_session_identity() {
        // codex notify 的 `thread-id` 就是 rollout uuid，`codex resume` 认它。
        let raw = b"nebula-hook/1 source=codex pane=3\n{\"type\":\"agent-turn-complete\",\"thread-id\":\"b5f6c1c2-1111-2222-3333-444455556666\"}";
        let event = parse_remote_envelope(raw, Some(3)).unwrap();
        assert_eq!(event.session_id.as_deref(), Some("b5f6c1c2-1111-2222-3333-444455556666"));
        // 桥接载荷没有 id 的来源读出 None，不许瞎编。
        let raw = b"nebula-hook/1 source=opencode pane=3\n{\"kind\":\"done\"}";
        assert_eq!(parse_remote_envelope(raw, Some(3)).unwrap().session_id, None);
    }

    #[test]
    fn pi_extension_events_use_the_normalized_lifecycle_shape() {
        let start = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"session-start\",\"sessionId\":\"pi-42\",\"bridge_sequence\":\"1700000000000001\"}",
            Some(3),
        )
        .unwrap();
        assert_eq!(start.kind, AiHookKind::SessionStart);
        assert_eq!(start.session_id.as_deref(), Some("pi-42"));
        assert_eq!(start.bridge_sequence, Some(1_700_000_000_000_001));

        let prompt = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"prompt\"}",
            Some(3),
        )
        .unwrap();
        assert_eq!(prompt.kind, AiHookKind::PromptSubmit);

        let done =
            parse_remote_envelope(b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"done\"}", Some(3))
                .unwrap();
        assert_eq!(done.kind, AiHookKind::TurnDone);

        for (kind, expected) in
            [("tool-complete", AiHookKind::ToolComplete), ("session-end", AiHookKind::SessionEnd)]
        {
            let raw = format!("nebula-hook/1 source=pi pane=3\n{{\"kind\":\"{kind}\"}}");
            assert_eq!(parse_remote_envelope(raw.as_bytes(), Some(3)).unwrap().kind, expected);
        }
    }

    #[test]
    fn permission_context_is_structured_bounded_and_redacted() {
        let oversized = "x".repeat(RAW_CONTEXT_MAX_BYTES * 2);
        let payload = serde_json::json!({
            "session_id": "claude-session",
            "hook_event_name": "PermissionRequest",
            "cwd": "D:/work/nebula",
            "tool_name": "Write",
            "message": "Allow writing ai_hook.rs?",
            "selection": "selected source",
            "api_key": "must-not-leak",
            "tool_input": { "content": oversized }
        });
        let raw = format!("nebula-hook/1 source=claude pane=9\n{payload}");
        let event = parse_remote_envelope(raw.as_bytes(), Some(9)).unwrap();
        let context = event.attention.as_ref().unwrap();

        assert_eq!(event.kind, AiHookKind::NeedsAttention);
        assert_eq!(context.pane_id, Some(9));
        assert_eq!(context.cwd.as_deref(), Some("D:/work/nebula"));
        assert_eq!(context.permission_or_tool.as_deref(), Some("Write"));
        assert_eq!(context.selection.as_deref(), Some("selected source"));
        let sanitized = context.raw_context.as_deref().unwrap();
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("must-not-leak"));
        assert!(sanitized.len() <= RAW_CONTEXT_MAX_BYTES);
        let summary = context.summary_for_pane(9);
        assert!(summary.contains("Pane 9"));
        assert!(summary.contains("Write"));
        assert!(!summary.contains("selected source"));
    }

    #[test]
    fn claude_stop_reports_running_background_subagents() {
        let raw = br#"nebula-hook/1 source=claude pane=3
{"session_id":"s","hook_event_name":"Stop","background_tasks":[{"type":"subagent","status":"running"},{"type":"subagent","status":"completed"},{"type":"other","status":"running"}]}"#;
        let event = parse_remote_envelope(raw, Some(3)).unwrap();
        assert_eq!(event.kind, AiHookKind::TurnDone);
        assert_eq!(event.active_background_tasks(), 1);
        assert_eq!(event.background_tasks.unwrap().total, 2);
        assert!(capabilities_for("claude").background_tasks);
        assert!(!capabilities_for("pi").attention_context);
    }

    #[test]
    fn distinct_original_answers_are_not_swallowed_by_preview_deduplication() {
        for provider in ["claude", "codex"] {
            let mut gate = AiHookEventGate::default();
            for ending in ["first", "second"] {
                let answer = format!("{}{ending}", "shared preview ".repeat(500));
                let payload = if provider == "claude" {
                    serde_json::json!({"hook_event_name": "Stop", "session_id": "session", "last_assistant_message": answer})
                } else {
                    serde_json::json!({"type": "agent-turn-complete", "thread-id": "session", "last-assistant-message": answer})
                };
                let raw = format!("nebula-hook/1 source={provider} pane=3\n{payload}");
                let event = parse_remote_envelope(raw.as_bytes(), Some(3)).unwrap();
                assert!(gate.accept(&event, 3));
            }
        }
    }

    #[test]
    fn event_gate_rejects_duplicates_and_out_of_order_provider_events() {
        let mut gate = AiHookEventGate::default();
        let done = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"done\",\"session_id\":\"s\",\"bridge_sequence\":3,\"event_id\":\"s:3\"}",
            Some(3),
        )
        .unwrap();
        assert!(gate.accept(&done, 3));
        assert!(!gate.accept(&done, 3), "same event id must be idempotent");

        let next_done = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"done\",\"session_id\":\"s\",\"bridge_sequence\":4,\"event_id\":\"s:4\"}",
            Some(3),
        )
        .unwrap();
        assert!(
            gate.accept(&next_done, 3),
            "a newer sequence must not be swallowed by the short duplicate window"
        );

        let late_prompt = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"prompt\",\"session_id\":\"s\",\"bridge_sequence\":2,\"event_id\":\"s:2\"}",
            Some(3),
        )
        .unwrap();
        assert!(!gate.accept(&late_prompt, 3));
    }

    #[test]
    fn event_gate_keeps_sessions_isolated_and_session_end_terminal() {
        let mut gate = AiHookEventGate::default();
        let ended = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=7\n{\"kind\":\"session-end\",\"session_id\":\"old\",\"bridge_sequence\":5}",
            Some(7),
        )
        .unwrap();
        assert!(gate.accept(&ended, 7));

        let revive = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=7\n{\"kind\":\"prompt\",\"session_id\":\"old\",\"bridge_sequence\":6}",
            Some(7),
        )
        .unwrap();
        assert!(!gate.accept(&revive, 7));

        let new_session = parse_remote_envelope(
            b"nebula-hook/1 source=pi pane=7\n{\"kind\":\"prompt\",\"session_id\":\"new\",\"bridge_sequence\":1}",
            Some(7),
        )
        .unwrap();
        assert!(gate.accept(&new_session, 7));
    }

    #[test]
    fn blocked_can_resume_but_unordered_done_cannot_regress() {
        let mut gate = AiHookEventGate::default();
        let attention = parse_remote_envelope(
            b"nebula-hook/1 source=claude pane=4\n{\"hook_event_name\":\"PermissionRequest\",\"session_id\":\"s\",\"tool_name\":\"Bash\"}",
            Some(4),
        )
        .unwrap();
        let tool = parse_remote_envelope(
            b"nebula-hook/1 source=claude pane=4\n{\"hook_event_name\":\"PostToolUse\",\"session_id\":\"s\"}",
            Some(4),
        )
        .unwrap();
        let done = parse_remote_envelope(
            b"nebula-hook/1 source=claude pane=4\n{\"hook_event_name\":\"Stop\",\"session_id\":\"s\"}",
            Some(4),
        )
        .unwrap();

        assert!(gate.accept(&attention, 4));
        assert!(gate.accept(&tool, 4), "permission continuation must resume working");
        assert!(gate.accept(&done, 4));
        assert!(!gate.accept(&tool, 4), "late unordered tool event must not overwrite Done");
    }

    /// 每一种拦截都要说得出是哪条规则——「通知没出现」事后唯一的线索就是这个
    /// 原因，一个光秃秃的 bool 等于没有线索。
    #[test]
    fn gate_reports_which_rule_dropped_the_event() {
        let event = |json: &str| {
            let raw = format!("nebula-hook/1 source=pi pane=3\n{json}");
            parse_remote_envelope(raw.as_bytes(), Some(3)).unwrap()
        };
        let mut gate = AiHookEventGate::default();

        let done = event(
            "{\"kind\":\"done\",\"session_id\":\"s\",\"bridge_sequence\":3,\"event_id\":\"s:3\"}",
        );
        assert_eq!(gate.verdict(&done, 3), GateVerdict::Accepted);
        assert_eq!(gate.verdict(&done, 3), GateVerdict::DuplicateEventId);

        let older = event(
            "{\"kind\":\"prompt\",\"session_id\":\"s\",\"bridge_sequence\":2,\"event_id\":\"s:2\"}",
        );
        assert_eq!(gate.verdict(&older, 3), GateVerdict::StaleSequence);

        // 无序号但带 provider 时间戳：走时间判据。
        let mut timed = AiHookEventGate::default();
        let newer = event("{\"kind\":\"done\",\"session_id\":\"t\",\"occurred_at_ms\":2000}");
        let earlier = event("{\"kind\":\"prompt\",\"session_id\":\"t\",\"occurred_at_ms\":1000}");
        assert_eq!(timed.verdict(&newer, 3), GateVerdict::Accepted);
        assert_eq!(timed.verdict(&earlier, 3), GateVerdict::StaleTime);

        // SessionEnd 之后只有 SessionStart 能复活。
        let mut ended = AiHookEventGate::default();
        assert_eq!(
            ended.verdict(&event("{\"kind\":\"session-end\",\"session_id\":\"u\"}"), 3),
            GateVerdict::Accepted
        );
        assert_eq!(
            ended.verdict(&event("{\"kind\":\"prompt\",\"session_id\":\"u\"}"), 3),
            GateVerdict::AfterSessionEnd
        );

        // Done 之后无证据的 ToolComplete 不能把完成态倒退回运行中。
        let mut finished = AiHookEventGate::default();
        assert_eq!(
            finished.verdict(&event("{\"kind\":\"done\",\"session_id\":\"v\"}"), 3),
            GateVerdict::Accepted
        );
        assert_eq!(
            finished.verdict(&event("{\"kind\":\"tool-complete\",\"session_id\":\"v\"}"), 3),
            GateVerdict::UnorderedAfterDone
        );
    }

    #[test]
    fn sequenced_batch_reorders_each_stream_without_mixing_streams() {
        fn event(session: &str, sequence: u64) -> super::AiHookEvent {
            let raw = format!(
                "nebula-hook/1 source=pi pane=3\n{{\"kind\":\"prompt\",\"session_id\":\"{session}\",\"bridge_sequence\":{sequence},\"event_id\":\"{session}:{sequence}\"}}"
            );
            parse_remote_envelope(raw.as_bytes(), Some(3)).unwrap()
        }

        let ordered =
            reorder_batch(vec![event("a", 3), event("b", 2), event("a", 1), event("b", 1)]);
        let sequence = ordered
            .iter()
            .map(|event| (event.session_id.as_deref().unwrap(), event.bridge_sequence.unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(sequence, vec![("a", 1), ("b", 1), ("a", 3), ("b", 2)]);
    }
}

/// Char-boundary-safe cut with an ellipsis (toast bodies are small).
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max_chars).collect();
    format!("{cut}…")
}

#[cfg(all(windows, feature = "legacy-shell"))]
pub use win::spawn_server;
#[cfg(windows)]
pub use win::{setup_ai_cli, spawn_config_guard, spawn_gpui_server};

#[cfg(not(windows))]
pub fn spawn_gpui_server() -> std::sync::mpsc::Receiver<AiHookEvent> {
    let (_tx, rx) = std::sync::mpsc::channel();
    rx
}

#[cfg(windows)]
mod win;
