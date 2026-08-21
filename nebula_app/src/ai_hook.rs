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

#![cfg_attr(not(windows), allow(dead_code))]

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

/// Claude hook events we subscribe to. Session boundaries carry the id needed
/// for resume/fork; PostToolUse lets a stale permission state return to working
/// before the whole turn completes.
const CLAUDE_EVENTS: [&str; 6] =
    ["SessionStart", "UserPromptSubmit", "Notification", "PostToolUse", "Stop", "SessionEnd"];

/// What a lifecycle event means for the pane's turn state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A typed AI-CLI lifecycle event, parsed from one pipe connection.
#[derive(Debug, Clone)]
pub struct AiHookEvent {
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
}

/// Parse one pipe message: a `nebula-hook/1 source=<s> pane=<n>` header line,
/// then the hook's raw JSON payload verbatim (the helper never re-encodes;
/// all JSON work happens here, off the turn's hot path).
fn parse_envelope(bytes: &[u8]) -> Option<AiHookEvent> {
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
    // 会话身份随事件顺带捕获：claude 每个 hook 载荷都带 `session_id`
    // （snake_case）；codex notify 带 `thread-id`（kebab-case，即 rollout
    // uuid）。opencode/pi 的桥接载荷不带 id，读不到就是 None。
    let session_id = payload
        .get("session_id")
        .or_else(|| payload.get("thread-id"))
        .or_else(|| payload.get("sessionID"))
        .or_else(|| payload.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let (kind, message) = match source.as_str() {
        "claude" => match payload.get("hook_event_name").and_then(Value::as_str) {
            Some("SessionStart") => (AiHookKind::SessionStart, None),
            Some("UserPromptSubmit") => (AiHookKind::PromptSubmit, None),
            Some("PostToolUse") => (AiHookKind::ToolComplete, None),
            Some("Stop") => (AiHookKind::TurnDone, None),
            Some("SessionEnd") => (AiHookKind::SessionEnd, None),
            Some("Notification") => (
                AiHookKind::NeedsAttention,
                payload.get("message").and_then(Value::as_str).map(str::to_owned),
            ),
            // SubagentStop and friends would only produce noise.
            _ => return None,
        },
        "codex" => match payload.get("type").and_then(Value::as_str) {
            Some("agent-turn-complete") => (
                AiHookKind::TurnDone,
                payload
                    .get("last-assistant-message")
                    .and_then(Value::as_str)
                    .map(|m| truncate(m, 300)),
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
            Some("attention") => (
                AiHookKind::NeedsAttention,
                payload.get("message").and_then(Value::as_str).map(|m| truncate(m, 300)),
            ),
            _ => return None,
        },
        _ => return None,
    };
    Some(AiHookEvent { pane, source, kind, message, session_id })
}

/// 远端会话只能提交事件语义，Pane 身份始终由本地 SSH 通道覆盖，
/// 防止远端载荷把通知路由到同一窗口中的其他标签页。
pub(crate) fn parse_remote_envelope(bytes: &[u8], pane: Option<u64>) -> Option<AiHookEvent> {
    let mut event = parse_envelope(bytes)?;
    event.pane = pane;
    Some(event)
}

/// 屏幕尾部的文本像不像「AI 正在等用户回答」。
///
/// codex 的 notify 只有一种事件——弹出交互式提问（选择框、批准确认）时它
/// 发的也是 `agent-turn-complete`。光看事件流分不出"说完了"和"在等你"，
/// 只能看屏幕：回合结束的瞬间尾部还挂着提交/确认类提示，就是在等人，
/// 蓝点该升级成手掌。
///
/// 特征串取各家问题框的**操作提示行**——那一行只在等待输入时才存在。刻意
/// 不收 "esc to interrupt"：它在整个回合运行期间都挂在状态栏上，收了它，
/// 每次正常完成都会被误报成"在问你"。
pub(crate) fn tail_looks_like_question(tail: &str) -> bool {
    let lower = tail.to_ascii_lowercase();
    [
        // codex 的问题框底栏。
        "enter to submit",
        "tab to add notes",
        // claude code 的选择框与权限确认。
        "enter to confirm",
        "do you want to proceed",
        // 通用 CLI 确认式。
        "(y/n)",
        "[y/n]",
    ]
    .iter()
    .any(|mark| lower.contains(mark))
}

#[cfg(test)]
mod question_tail_tests {
    use super::tail_looks_like_question;

    #[test]
    fn codex_question_footer_reads_as_asking() {
        let tail = "Question 1/2 (2 unanswered)\n> 1. 深色主题 (Recommended)\n\
                    tab to add notes | enter to submit answer | esc to interrupt";
        assert!(tail_looks_like_question(tail));
    }

    #[test]
    fn yes_no_prompts_read_as_asking() {
        assert!(tail_looks_like_question("Overwrite existing file? (y/N)"));
        assert!(tail_looks_like_question("Do you want to proceed?\n> 1. Yes"));
    }

    #[test]
    fn a_running_status_bar_does_not_read_as_asking() {
        // "esc to interrupt" 单独出现是**运行中**的状态栏；把它当特征收进
        // 去，每次正常完成都会误报成提问。
        assert!(!tail_looks_like_question("Working on it… esc to interrupt"));
        assert!(!tail_looks_like_question("$ cargo build\n   Compiling nebula v0.8.0"));
    }
}

#[cfg(test)]
mod remote_tests {
    use super::{AiHookKind, parse_remote_envelope};

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
            b"nebula-hook/1 source=pi pane=3\n{\"kind\":\"session-start\",\"sessionId\":\"pi-42\"}",
            Some(3),
        )
        .unwrap();
        assert_eq!(start.kind, AiHookKind::SessionStart);
        assert_eq!(start.session_id.as_deref(), Some("pi-42"));

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
}

/// Char-boundary-safe cut with an ellipsis (toast bodies are small).
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max_chars).collect();
    format!("{cut}…")
}

#[cfg(windows)]
pub use win::{setup_ai_cli, spawn_config_guard, spawn_gpui_server, spawn_server};

#[cfg(not(windows))]
pub fn spawn_gpui_server() -> std::sync::mpsc::Receiver<AiHookEvent> {
    let (_tx, rx) = std::sync::mpsc::channel();
    rx
}

#[cfg(windows)]
mod win {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use winit::event_loop::EventLoopProxy;

    use super::{CLAUDE_EVENTS, HELPER_MARK, HOOK_EXE_ENV, PIPE_ENV, parse_envelope};
    use crate::event::{Event, EventType};

    // ─── pipe server ────────────────────────────────────────────────────────

    /// Create the per-instance pipe, export its name to future children, and
    /// start the accept loop. Must run before the first PTY spawns.
    pub fn spawn_server(proxy: EventLoopProxy<Event>) {
        spawn_pipe_server(move |event| {
            proxy.send_event(Event::new(EventType::AiHook(event), None)).is_ok()
        });
    }

    /// GPUI owns a different event loop, but hook parsing and pipe ownership
    /// stay identical. The workspace drains this channel on its foreground
    /// executor and routes events by the same stable pane id contract.
    pub fn spawn_gpui_server() -> std::sync::mpsc::Receiver<super::AiHookEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_pipe_server(move |event| tx.send(event).is_ok());
        rx
    }

    fn spawn_pipe_server(sink: impl Fn(super::AiHookEvent) -> bool + Send + 'static) {
        let name = format!(r"\\.\pipe\nebula-notify-{}", std::process::id());
        // SAFETY: single-threaded startup; no other thread reads the env yet.
        unsafe { std::env::set_var(PIPE_ENV, &name) };
        // Export nebula-hook.exe's path for the opencode plugin (best-effort:
        // if the helper isn't found, the plugin simply no-ops like anywhere
        // outside Nebula). Forward slashes: the path is interpolated into
        // Bun's `$` shell inside the plugin, matching `helper_command`.
        if let Some(helper) = helper_path() {
            let p = helper.display().to_string().replace('\\', "/");
            unsafe { std::env::set_var(HOOK_EXE_ENV, p) };
        }
        if let Err(err) = std::thread::Builder::new()
            .name("nebula-ai-pipe".into())
            .spawn(move || serve(&name, sink))
        {
            log::warn!("ai_hook: failed to spawn pipe server: {err}");
        }
    }

    /// Accept loop. One fresh pipe instance per connection: a client racing
    /// the turnaround sees a failed open for microseconds and retries (the
    /// helper retries for ~100 ms — an eternity at this message rate).
    fn serve(name: &str, sink: impl Fn(super::AiHookEvent) -> bool) {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
        };
        // PIPE_ACCESS_INBOUND is a FILE_FLAGS_AND_ATTRIBUTES constant, hence
        // its home in the FileSystem module rather than Pipes.
        use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, ReadFile};
        use windows_sys::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        };

        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        loop {
            // SAFETY: `wide` is NUL-terminated and outlives the call. Null
            // security attributes = default DACL, same-user access only.
            let pipe = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    64 * 1024,
                    0,
                    std::ptr::null(),
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                log::warn!("ai_hook: CreateNamedPipeW failed; AI turn events disabled");
                return;
            }

            // SAFETY: `pipe` is a valid handle owned by this frame.
            // ERROR_PIPE_CONNECTED = the client connected first; still good.
            let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if connected {
                let mut buf = Vec::with_capacity(4096);
                let mut chunk = [0u8; 4096];
                loop {
                    let mut read = 0u32;
                    // SAFETY: `chunk` outlives the call; `read` written first.
                    let ok = unsafe {
                        ReadFile(
                            pipe,
                            chunk.as_mut_ptr(),
                            chunk.len() as u32,
                            &mut read,
                            std::ptr::null_mut(),
                        )
                    };
                    // ok == 0 is the normal EOF (BROKEN_PIPE on client close).
                    if ok == 0 || read == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..read as usize]);
                    if buf.len() > (1 << 20) {
                        break; // hard cap: nothing legitimate is this big
                    }
                }
                if let Some(event) = parse_envelope(&buf) {
                    log::debug!("ai_hook: {event:?}");
                    if !sink(event) {
                        // Event loop gone: shutting down.
                        // SAFETY: `pipe` is still the valid handle from above.
                        unsafe {
                            DisconnectNamedPipe(pipe);
                            CloseHandle(pipe);
                        }
                        return;
                    }
                }
            }
            // SAFETY: `pipe` is valid; failures past this point only cost
            // this one instance, the loop creates a fresh one.
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
        }
    }

    // ─── settings self-heal ─────────────────────────────────────────────────

    /// Boot entrypoint: install now, then keep installed (see module docs).
    pub fn spawn_config_guard() {
        // `setup-ai --remove` 落下的持久开关：用户明确断开过就不再自动
        // 装回（#38 的自愈复发面 / #8 卸载后仍在 hook）。重新启用走
        // `nebula setup-ai`。
        if hooks_disabled() {
            log::info!("ai_hook: ai_hooks=0 (setup-ai --remove); auto-install disabled");
            return;
        }
        if let Err(err) =
            std::thread::Builder::new().name("nebula-ai-setup".into()).spawn(config_guard)
        {
            log::warn!("ai_hook: failed to spawn settings guard: {err}");
        }
    }

    /// `nebula_settings.txt` 里 `ai_hooks=0`（由 `setup-ai --remove` 写入）。
    fn hooks_disabled() -> bool {
        nebula_settings::RawSettings::load().bool_on("ai_hooks") == Some(false)
    }

    /// 一轮完整自愈。每轮都重读开关：`setup-ai --remove` 可能发生在本进程
    /// 存活期间，它触发的 config 变更事件会立刻打回这里——不重读就会在
    /// 400ms 内把刚移除的接线原样装回（#38 实测的自愈复发路径）。
    fn heal_all() {
        if hooks_disabled() {
            return;
        }
        ensure_claude_hooks();
        ensure_codex_notify();
        ensure_opencode_plugin();
        ensure_pi_extension();
        for (agent, path, result) in ensure_runtime_skills() {
            match result {
                Ok(ManagedSkillInstall::Installed) => {
                    log::info!("ai_hook: installed {agent} runtime skill at {}", path.display())
                },
                Ok(ManagedSkillInstall::Current) => {},
                Ok(ManagedSkillInstall::Conflict) => log::warn!(
                    "ai_hook: preserving unmanaged or edited {agent} skill at {}",
                    path.display()
                ),
                Err(error) => log::warn!(
                    "ai_hook: failed to install {agent} runtime skill at {}: {error}",
                    path.display()
                ),
            }
        }
    }

    fn config_guard() {
        use notify::{RecursiveMode, Watcher};

        // Neither CLI installed (yet): re-check occasionally instead of
        // watching directories that do not exist.
        let (claude_dir, codex_dir) = loop {
            let claude = claude_config_dir().filter(|d| d.exists());
            let codex = codex_config_dir().filter(|d| d.exists());
            if claude.is_some()
                || codex.is_some()
                || opencode_config_dir().is_some_and(|d| d.exists())
                || pi_agent_dir().is_some_and(|d| d.exists())
            {
                break (claude, codex);
            }
            std::thread::sleep(Duration::from_secs(300));
        };

        heal_all();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                log::warn!("ai_hook: settings watcher unavailable ({err}); polling instead");
                poll_guard()
            },
        };
        for dir in [&claude_dir, &codex_dir].into_iter().flatten() {
            if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                log::warn!("ai_hook: cannot watch {}: {err}; polling instead", dir.display());
                poll_guard();
            }
        }

        loop {
            match rx.recv() {
                Ok(event) => {
                    // Only the two config files matter — ~/.codex especially
                    // is a busy directory (sessions, sqlite WALs) that would
                    // otherwise trigger constant re-checks.
                    let relevant = match &event {
                        Ok(ev) => {
                            ev.paths.is_empty()
                                || ev.paths.iter().any(|p| {
                                    p.file_name()
                                        .is_some_and(|n| n == "settings.json" || n == "config.toml")
                                })
                        },
                        Err(_) => true,
                    };
                    if !relevant {
                        continue;
                    }
                    // Debounce the writer's burst, then heal. Our own atomic
                    // rename lands here once and heals to a no-op.
                    while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
                    heal_all();
                },
                Err(_) => return, // channel closed: shutting down
            }
        }
    }

    /// Degraded guard when file watching is unavailable: heal every 5 min.
    fn poll_guard() -> ! {
        loop {
            std::thread::sleep(Duration::from_secs(300));
            heal_all();
        }
    }

    /// One-time-per-process announcement so a switcher rewriting the file in
    /// a loop cannot flood the Action Center with "hooks reinstalled".
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);

    fn announce() {
        if !ANNOUNCED.swap(true, Ordering::Relaxed) {
            crate::notify::toast(
                "Nebula",
                "已接入 AI 回合通知（Claude / Codex / Pi / opencode）。撤销：nebula setup-ai --remove",
            );
        }
    }

    // ─── codex notify (config.toml) ─────────────────────────────────────────

    /// Codex home: `$CODEX_HOME`, else `~/.codex`.
    fn codex_config_dir() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Some(PathBuf::from(home));
        }
        Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".codex"))
    }

    /// notify 序列化后的字节预算。正常接线只有几百字节；超过它的唯一已知
    /// 途径是与其他 notify 包装器互相包装的指数膨胀（#38，最终 130 MB 撑爆
    /// config.toml 让 codex 起不来）。宁可这一轮不接线，也不把病态值落盘。
    const NOTIFY_BYTE_BUDGET: usize = 8 * 1024;

    /// 由现有 notify argv 算出应写入的新 argv；`None` = 不动文件。
    ///
    /// 与 codex-computer-use 的共存规则（#38）：它重新注册时若认不出
    /// notify\[0\] 是自己，会把整个旧数组 JSON 序列化进自己的
    /// `--previous-notify` 参数。此时 nebula-hook 不在最外层，但仍在链中
    /// ——事件会沿链回流。若这时再包一层，两个包装器互相包装、转义反斜杠
    /// 每轮翻倍。所以：helper 标记出现在**任何位置**（含 JSON 字符串内部）
    /// 都算已接线，只有最外层是自己时才做路径自愈。
    fn desired_codex_notify(current: &[String], helper: &str) -> Option<Vec<String>> {
        let desired: Vec<String> = match current.first() {
            // Already ours: heal the helper path, keep any chain tail as-is.
            Some(first) if first.contains(HELPER_MARK) => {
                let mut argv = current.to_vec();
                argv[0] = helper.to_owned();
                argv
            },
            // 已在链中但不在最外层：保持现状，绝不再包（见上）。
            Some(_) if current.iter().any(|arg| arg.contains(HELPER_MARK)) => return None,
            // Occupied: wrap the existing notifier behind --chain.
            Some(_) => {
                let mut argv = vec![helper.to_owned(), "codex".to_owned(), "--chain".to_owned()];
                argv.extend(current.iter().cloned());
                argv
            },
            None => vec![helper.to_owned(), "codex".to_owned()],
        };
        if current == desired {
            return None;
        }
        // 长度兜底：对任何形态的膨胀（不止 #38 这一种循环）一律拒写。
        // +4 ≈ 每个元素的引号、逗号与空格开销。
        let bytes: usize = desired.iter().map(|arg| arg.len() + 4).sum();
        if bytes > NOTIFY_BYTE_BUDGET {
            log::warn!(
                "ai_hook: codex notify would serialize to {bytes} bytes (> {NOTIFY_BYTE_BUDGET}); \
                 refusing to write (wrapper loop guard, #38)"
            );
            return None;
        }
        Some(desired)
    }

    /// Wire codex's `notify` to nebula-hook. Codex has a SINGLE notify slot
    /// which may already be taken (e.g. OpenAI's own computer-use notifier),
    /// so an occupied slot is wrapped, not evicted: nebula-hook forwards to
    /// the pipe and then invokes the original program via `--chain` with the
    /// same payload. toml_edit keeps the file's formatting and comments.
    /// Idempotent; heals a moved helper path. Returns whether it wrote.
    pub fn ensure_codex_notify() -> bool {
        let Some(path) = codex_config_dir().map(|d| d.join("config.toml")) else { return false };
        let Ok(raw) = std::fs::read_to_string(&path) else { return false }; // no codex → skip
        let Some(helper) = helper_path() else { return false };
        let helper = helper.display().to_string().replace('\\', "/");

        let Ok(mut doc) = raw.parse::<toml_edit::DocumentMut>() else {
            log::warn!("ai_hook: {} is not valid TOML; left alone", path.display());
            return false;
        };

        let current: Vec<String> = doc
            .get("notify")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();

        let Some(desired) = desired_codex_notify(&current, &helper) else {
            return false;
        };

        let mut array = toml_edit::Array::new();
        for arg in &desired {
            array.push(arg.as_str());
        }
        doc["notify"] = toml_edit::value(array);

        let bak = path.with_extension("toml.nebula-bak");
        if !bak.exists() {
            if let Err(err) = std::fs::copy(&path, &bak) {
                log::warn!("ai_hook: backup failed ({err}); not touching {}", path.display());
                return false;
            }
        }
        match write_atomic(&path, &doc.to_string()) {
            Ok(()) => {
                log::info!("ai_hook: codex notify wired in {}", path.display());
                announce();
                true
            },
            Err(err) => {
                log::warn!("ai_hook: failed to write {}: {err}", path.display());
                false
            },
        }
    }

    /// Undo [`ensure_codex_notify`]: restore a wrapped notifier from the
    /// `--chain` tail, or drop the key entirely when we created it.
    fn remove_codex_notify() -> std::io::Result<bool> {
        let Some(path) = codex_config_dir().map(|d| d.join("config.toml")) else {
            return Ok(false);
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Ok(false),
        };
        let mut doc = raw
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let current: Vec<String> = doc
            .get("notify")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        if !current.first().is_some_and(|f| f.contains(HELPER_MARK)) {
            return Ok(false); // not ours
        }
        match current.iter().position(|a| a == "--chain") {
            // Restore the original argv that lived behind --chain.
            Some(chain) => {
                let mut array = toml_edit::Array::new();
                for arg in &current[chain + 1..] {
                    array.push(arg.as_str());
                }
                doc["notify"] = toml_edit::value(array);
            },
            // We created the key; remove it outright.
            None => {
                doc.as_table_mut().remove("notify");
            },
        }
        write_atomic(&path, &doc.to_string())?;
        Ok(true)
    }

    // ─── opencode plugin (~/.config/opencode/plugins/nebula.js) ─────────────

    /// The Nebula↔opencode bridge, auto-dropped into opencode's global plugin
    /// dir. opencode is a Bun app that auto-loads `{plugin,plugins}/*.js`; this
    /// plugin subscribes to its event bus and shells out to nebula-hook.exe
    /// (path in `NEBULA_HOOK_EXE`, pipe in the inherited `NEBULA_NOTIFY_PIPE`),
    /// normalizing events into the tiny `{kind,message}` payload `parse_envelope`
    /// reads. Dedup lives here (opencode-specific), keeping the Rust side
    /// decoupled from opencode's evolving SDK event schema.
    const OPENCODE_PLUGIN_JS: &str = r#"// Nebula ↔ opencode bridge — AUTO-GENERATED by Nebula, do not edit.
// Forwards turn lifecycle to Nebula's sidebar (icon + spinner + toasts).
// Inert outside Nebula (no NEBULA_HOOK_EXE in the environment).
export const NebulaNotify = async ({ $ }) => {
  const hook = process.env.NEBULA_HOOK_EXE
  if (!hook) return {}
  let active = false
  let lastUser = ""
  let sessionId = ""
  const send = (obj) => {
    // Fire-and-forget; never throw into opencode's event loop.
    try {
      if (sessionId) obj.session_id = sessionId
      $`${hook} opencode ${JSON.stringify(obj)}`.quiet().nothrow().catch(() => {})
    }
    catch (_) {}
  }
  return {
    event: async ({ event }) => {
      const t = event && event.type
      const props = (event && event.properties) || {}
      const info = props.info || {}
      // Root session only: subagents carry parentID and must not steal the
      // pane's resume identity.
      const candidate = props.sessionID || info.sessionID || (!info.parentID && info.id)
      if (candidate) {
        const first = !sessionId
        sessionId = candidate
        if (first) send({ kind: "session-start" })
      }
      if (t === "message.updated") {
        if (info && info.role === "user" && info.id !== lastUser) {
          lastUser = info.id
          active = true
          send({ kind: "prompt" })
        }
      } else if (t === "session.idle") {
        // Dedupe opencode's spurious idles (startup/cancel): only a turn
        // that actually started reports done.
        if (active) { active = false; send({ kind: "done" }) }
      } else if (t === "permission.updated") {
        active = false
        send({ kind: "attention", message: props.title || "" })
      } else if (t === "tool.execute.after") {
        send({ kind: "tool-complete" })
      } else if (t === "session.deleted") {
        send({ kind: "session-end" })
      }
    },
  }
}
"#;

    // Pi 官方扩展 API 在 agent_start/agent_end 提供稳定的回合边界。扩展只做
    // fire-and-forget 转发，且 NEBULA_HOOK_EXE 不存在时完全静默，因此全局安装
    // 不会影响从其他终端启动的 Pi。
    const PI_EXTENSION_TS: &str = r#"// Nebula ↔ Pi bridge — AUTO-GENERATED by Nebula, do not edit.
import { spawn } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  let active = false;
  const send = (kind: "session-start" | "prompt" | "tool-complete" | "done" | "session-end", ctx?: any) => {
    const hook = process.env.NEBULA_HOOK_EXE;
    if (!hook) return;
    try {
      let session_id = "";
      try { session_id = ctx?.sessionManager?.getSessionId?.() || ""; } catch (_) {}
      spawn(hook, ["pi", JSON.stringify({ kind, session_id })], {
        detached: true,
        stdio: "ignore",
        windowsHide: true,
      }).unref();
    } catch (_) {}
  };

  pi.on("agent_start", async (_event, ctx) => {
    active = true;
    send("prompt", ctx);
  });
  pi.on("agent_end", async (_event, ctx) => {
    if (!active) return;
    active = false;
    send("done", ctx);
  });
  try { pi.on("session_start", async (_event, ctx) => send("session-start", ctx)); } catch (_) {}
  try { pi.on("tool_result", async (_event, ctx) => send("tool-complete", ctx)); } catch (_) {}
  try { pi.on("session_shutdown", async (_event, ctx) => send("session-end", ctx)); } catch (_) {}
}
"#;

    /// Idempotently install/heal our hook entries in claude's settings.json.
    /// Returns whether the file was modified.
    pub fn ensure_claude_hooks() -> bool {
        let Some(dir) = claude_config_dir() else { return false };
        if !dir.exists() {
            return false; // no claude footprint → nothing to install into
        }
        let Some(command) = helper_command() else { return false };

        let path = dir.join("settings.json");
        let mut root: Value = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(json) => json,
                Err(err) => {
                    // Mid-rewrite by a concurrent writer, or genuinely broken:
                    // never "repair" by clobbering. The watcher retries on the
                    // next change, the boot pass on the next start.
                    log::warn!("ai_hook: {} is not valid JSON ({err}); left alone", path.display());
                    return false;
                },
            },
            Err(_) => json!({}),
        };

        let Some(changed) = install_into(&mut root, &command) else {
            log::warn!("ai_hook: {} has an unexpected shape; left alone", path.display());
            return false;
        };
        if !changed {
            return false;
        }

        // First modification keeps a pristine copy next to the original.
        if path.exists() {
            let bak = path.with_extension("json.nebula-bak");
            if !bak.exists() {
                if let Err(err) = std::fs::copy(&path, &bak) {
                    log::warn!("ai_hook: backup failed ({err}); not touching {}", path.display());
                    return false;
                }
            }
        }
        let Ok(raw) = serde_json::to_string_pretty(&root) else { return false };
        match write_atomic(&path, &raw) {
            Ok(()) => {
                log::info!("ai_hook: claude hooks installed into {}", path.display());
                announce();
                true
            },
            Err(err) => {
                log::warn!("ai_hook: failed to write {}: {err}", path.display());
                false
            },
        }
    }

    /// Pure JSON surgery: ensure each subscribed event carries exactly one
    /// nebula-hook command, healing a stale absolute path in place. `None`
    /// means the document's shape is not what claude documents — refuse.
    fn install_into(root: &mut Value, command: &str) -> Option<bool> {
        let obj = root.as_object_mut()?;
        let hooks = obj.entry("hooks").or_insert_with(|| json!({})).as_object_mut()?;
        let mut changed = false;
        for event in CLAUDE_EVENTS {
            let matchers = hooks.entry(event).or_insert_with(|| json!([])).as_array_mut()?;
            let mut found = false;
            for matcher in matchers.iter_mut() {
                let Some(cmds) = matcher.get_mut("hooks").and_then(Value::as_array_mut) else {
                    continue;
                };
                for cmd in cmds {
                    let ours = cmd
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains(HELPER_MARK));
                    if !ours {
                        continue;
                    }
                    found = true;
                    if cmd.get("command").and_then(Value::as_str) != Some(command) {
                        if let Some(entry) = cmd.as_object_mut() {
                            entry.insert("command".into(), json!(command));
                            changed = true;
                        }
                    }
                }
            }
            if !found {
                matchers.push(json!({
                    "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
                }));
                changed = true;
            }
        }
        Some(changed)
    }

    /// Strip every nebula-hook entry (and matchers left empty by that).
    fn remove_hooks() -> std::io::Result<bool> {
        let Some(dir) = claude_config_dir() else { return Ok(false) };
        let path = dir.join("settings.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return Ok(false),
        };
        let mut root: Value =
            serde_json::from_str(&raw).map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut changed = false;
        if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            for event in CLAUDE_EVENTS {
                let Some(matchers) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
                    continue;
                };
                for matcher in matchers.iter_mut() {
                    if let Some(cmds) = matcher.get_mut("hooks").and_then(Value::as_array_mut) {
                        let before = cmds.len();
                        cmds.retain(|c| {
                            !c.get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|c| c.contains(HELPER_MARK))
                        });
                        changed |= cmds.len() != before;
                    }
                }
                let before = matchers.len();
                matchers.retain(|m| {
                    m.get("hooks").and_then(Value::as_array).is_none_or(|c| !c.is_empty())
                });
                changed |= matchers.len() != before;
            }
        }
        if changed {
            write_atomic(&path, &serde_json::to_string_pretty(&root)?)?;
        }
        Ok(changed)
    }

    /// `nebula setup-ai [--remove]` entrypoint (console attached in `main`).
    pub fn setup_ai_cli(remove: bool) -> i32 {
        let Some(dir) = claude_config_dir() else {
            eprintln!("找不到用户目录（USERPROFILE / CLAUDE_CONFIG_DIR）。");
            return 1;
        };
        let path = dir.join("settings.json");
        if remove {
            let mut failed = false;
            match remove_hooks() {
                Ok(true) => println!("claude: 已从 {} 移除 hooks。", path.display()),
                Ok(false) => println!("claude: {} 中没有 Nebula 的 hooks。", path.display()),
                Err(err) => {
                    eprintln!("claude: 移除失败：{err}");
                    failed = true;
                },
            }
            match remove_codex_notify() {
                Ok(true) => println!("codex: 已还原 config.toml 的 notify。"),
                Ok(false) => println!("codex: notify 不是 Nebula 接管的，未改动。"),
                Err(err) => {
                    eprintln!("codex: 还原失败：{err}");
                    failed = true;
                },
            }
            match remove_opencode_plugin() {
                Ok(true) => println!("opencode: 已删除 plugins/nebula.js。"),
                Ok(false) => println!("opencode: 没有 Nebula 的插件，未改动。"),
                Err(err) => {
                    eprintln!("opencode: 删除失败：{err}");
                    failed = true;
                },
            }
            match remove_pi_extension() {
                Ok(true) => println!("pi: 已删除 extensions/nebula.ts。"),
                Ok(false) => println!("pi: 没有 Nebula 的扩展，未改动。"),
                Err(err) => {
                    eprintln!("pi: 删除失败：{err}");
                    failed = true;
                },
            }
            for (agent, path) in runtime_skill_candidates() {
                match remove_runtime_skill(&path) {
                    Ok(ManagedSkillRemoval::Removed) => {
                        println!("{agent}: 已移除 Nebula Runtime Skill（{}）。", path.display())
                    },
                    Ok(ManagedSkillRemoval::Absent) => {
                        println!("{agent}: 没有 Nebula 管理的 Runtime Skill，未改动。")
                    },
                    Ok(ManagedSkillRemoval::Conflict) => {
                        eprintln!(
                            "{agent}: {} 已被用户修改，保留该 Skill；如需删除请手动确认内容。",
                            path.display()
                        );
                        failed = true;
                    },
                    Err(error) => {
                        eprintln!("{agent}: 移除 Runtime Skill 失败：{error}");
                        failed = true;
                    },
                }
            }
            // 持久开关：不写它，下次 Nebula 启动（含开机自启）会把上面
            // 刚清掉的四处原样装回——移除必须比自愈活得久（#8、#38）。
            match nebula_settings::persist_keys(&[("ai_hooks", "0".to_owned())]) {
                Ok(()) => println!(
                    "已写入 ai_hooks=0：Nebula 启动时不再自动接线（重新启用：nebula setup-ai）。"
                ),
                Err(err) => {
                    eprintln!("警告：无法写入 ai_hooks=0（{err}），下次启动仍会自动装回。");
                    failed = true;
                },
            }
            // 卸载器必须尽最大努力清理所有集成，不能因一个损坏的用户配置
            // 提前返回而让其他 Hook 永久指向即将被删除的程序目录。
            return i32::from(failed);
        }
        match helper_command() {
            Some(command) => println!("hook 命令：{command}"),
            None => {
                eprintln!("runtime/ 和 nebula.exe 同目录中均未找到 nebula-hook.exe，无法安装。");
                return 1;
            },
        }
        let mut setup_failed = false;
        // 显式安装即重新授权：清掉 --remove 落下的持久开关，守护线程下次
        // 启动恢复自愈。
        if let Err(err) = nebula_settings::persist_keys(&[("ai_hooks", "1".to_owned())]) {
            eprintln!(
                "警告：无法写入 ai_hooks=1（{err}）；若之前执行过 --remove，自动接线仍是关闭状态。"
            );
        }
        if dir.exists() {
            if ensure_claude_hooks() {
                println!("claude: 已写入 {}（首次改动备份 *.nebula-bak）。", path.display());
            } else {
                println!("claude: {} 已是最新。", path.display());
            }
        } else {
            println!("claude: 未检测到（{} 不存在），跳过。", dir.display());
        }
        match codex_config_dir().map(|d| d.join("config.toml")) {
            Some(cfg) if cfg.exists() => {
                if ensure_codex_notify() {
                    println!("codex: 已接管 notify（原 notifier 经 --chain 保留）。");
                } else {
                    println!("codex: {} 已是最新。", cfg.display());
                }
            },
            _ => println!("codex: 未检测到 config.toml，跳过。"),
        }
        match opencode_config_dir() {
            Some(cfg) if cfg.exists() => {
                if ensure_opencode_plugin() {
                    println!(
                        "opencode: 已安装 {}。",
                        cfg.join("plugins").join("nebula.js").display()
                    );
                } else {
                    println!("opencode: 插件已是最新。");
                }
            },
            _ => println!("opencode: 未检测到（~/.config/opencode 不存在），跳过。"),
        }
        match pi_agent_dir() {
            Some(agent) if agent.exists() => {
                if ensure_pi_extension() {
                    println!(
                        "pi: 已安装 {}。",
                        agent.join("extensions").join("nebula.ts").display()
                    );
                } else {
                    println!("pi: 扩展已是最新。");
                }
            },
            _ => println!("pi: 未检测到（~/.pi/agent 不存在），跳过。"),
        }
        for (agent, path, result) in ensure_runtime_skills() {
            match result {
                Ok(ManagedSkillInstall::Installed) => {
                    println!("{agent}: 已安装 Nebula Runtime Skill 到 {}。", path.display())
                },
                Ok(ManagedSkillInstall::Current) => {
                    println!("{agent}: Nebula Runtime Skill 已是最新。")
                },
                Ok(ManagedSkillInstall::Conflict) => {
                    eprintln!(
                        "{agent}: {} 已存在非 Nebula 管理或被编辑的同名 Skill，未覆盖。",
                        path.display()
                    );
                    setup_failed = true;
                },
                Err(error) => {
                    eprintln!("{agent}: 安装 Runtime Skill 失败：{error}");
                    setup_failed = true;
                },
            }
        }
        println!("对新启动的会话生效；正在运行的会话保持原快照。");
        i32::from(setup_failed)
    }

    // ─── Runtime skill (Codex + Claude Code) ───────────────────────────────

    const RUNTIME_SKILL_MD: &str = include_str!("../../docs/skills/nebula-runtime/SKILL.md");
    const RUNTIME_SKILL_OPENAI_YAML: &str =
        include_str!("../../docs/skills/nebula-runtime/agents/openai.yaml");
    const RUNTIME_SKILL_MARKER: &str = ".nebula-managed";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ManagedSkillInstall {
        Installed,
        Current,
        Conflict,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ManagedSkillRemoval {
        Removed,
        Absent,
        Conflict,
    }

    fn runtime_skill_candidates() -> Vec<(&'static str, PathBuf)> {
        let mut targets = Vec::new();
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            targets.push((
                "codex",
                PathBuf::from(profile).join(".agents").join("skills").join("nebula-runtime"),
            ));
        }
        if let Some(claude) = claude_config_dir() {
            targets.push(("claude", claude.join("skills").join("nebula-runtime")));
        }
        targets
    }

    fn ensure_runtime_skills() -> Vec<(&'static str, PathBuf, std::io::Result<ManagedSkillInstall>)>
    {
        runtime_skill_candidates()
            .into_iter()
            .filter(|(agent, _)| match *agent {
                "codex" => codex_config_dir().is_some_and(|dir| dir.exists()),
                "claude" => claude_config_dir().is_some_and(|dir| dir.exists()),
                _ => false,
            })
            .map(|(agent, path)| {
                let result = ensure_runtime_skill(&path);
                (agent, path, result)
            })
            .collect()
    }

    fn skill_fingerprint(skill: &[u8], metadata: &[u8]) -> String {
        use sha2::{Digest as _, Sha256};

        let mut digest = Sha256::new();
        digest.update(skill);
        digest.update([0]);
        digest.update(metadata);
        digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn read_skill_fingerprint(dir: &Path) -> Option<String> {
        let skill = std::fs::read(dir.join("SKILL.md")).ok()?;
        let metadata = std::fs::read(dir.join("agents").join("openai.yaml")).ok()?;
        Some(skill_fingerprint(&skill, &metadata))
    }

    fn ensure_runtime_skill(dir: &Path) -> std::io::Result<ManagedSkillInstall> {
        let skill_path = dir.join("SKILL.md");
        let metadata_path = dir.join("agents").join("openai.yaml");
        let marker_path = dir.join(RUNTIME_SKILL_MARKER);
        let expected =
            skill_fingerprint(RUNTIME_SKILL_MD.as_bytes(), RUNTIME_SKILL_OPENAI_YAML.as_bytes());
        let current = read_skill_fingerprint(dir);
        let marker =
            std::fs::read_to_string(&marker_path).ok().map(|value| value.trim().to_owned());
        let exact_skill = std::fs::read(&skill_path)
            .is_ok_and(|contents| contents == RUNTIME_SKILL_MD.as_bytes());
        let metadata_compatible = !metadata_path.exists()
            || std::fs::read(&metadata_path)
                .is_ok_and(|contents| contents == RUNTIME_SKILL_OPENAI_YAML.as_bytes());
        let empty = !skill_path.exists() && !metadata_path.exists();
        let owned = current.as_ref().zip(marker.as_ref()).is_some_and(|(a, b)| a == b);

        if current.as_deref() == Some(expected.as_str())
            && marker.as_deref() == Some(expected.as_str())
        {
            return Ok(ManagedSkillInstall::Current);
        }
        if !(empty || owned || (exact_skill && metadata_compatible)) {
            return Ok(ManagedSkillInstall::Conflict);
        }

        // 标记只在两份内容都原子写完后落下；崩溃不会把半套文件误认成
        // Nebula 所有，后续也绝不凭目录名覆盖用户同名 Skill。
        crate::atomic_file::write(&skill_path, RUNTIME_SKILL_MD.as_bytes())?;
        crate::atomic_file::write(&metadata_path, RUNTIME_SKILL_OPENAI_YAML.as_bytes())?;
        crate::atomic_file::write(&marker_path, format!("{expected}\n").as_bytes())?;
        Ok(ManagedSkillInstall::Installed)
    }

    fn remove_runtime_skill(dir: &Path) -> std::io::Result<ManagedSkillRemoval> {
        let marker_path = dir.join(RUNTIME_SKILL_MARKER);
        let Some(marker) =
            std::fs::read_to_string(&marker_path).ok().map(|value| value.trim().to_owned())
        else {
            return Ok(ManagedSkillRemoval::Absent);
        };
        if read_skill_fingerprint(dir).as_deref() != Some(marker.as_str()) {
            return Ok(ManagedSkillRemoval::Conflict);
        }

        for path in [dir.join("SKILL.md"), dir.join("agents").join("openai.yaml"), marker_path] {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        let metadata_dir = dir.join("agents");
        if metadata_dir.is_dir() && std::fs::read_dir(&metadata_dir)?.next().is_none() {
            std::fs::remove_dir(metadata_dir)?;
        }
        if dir.is_dir() && std::fs::read_dir(dir)?.next().is_none() {
            std::fs::remove_dir(dir)?;
        }
        Ok(ManagedSkillRemoval::Removed)
    }

    #[cfg(test)]
    mod runtime_skill_tests {
        use super::{
            ManagedSkillInstall, ManagedSkillRemoval, RUNTIME_SKILL_MD, ensure_runtime_skill,
            remove_runtime_skill,
        };

        #[test]
        fn managed_skill_installs_idempotently_and_removes_its_own_files() {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path().join("nebula-runtime");

            assert_eq!(ensure_runtime_skill(&dir).unwrap(), ManagedSkillInstall::Installed);
            assert_eq!(ensure_runtime_skill(&dir).unwrap(), ManagedSkillInstall::Current);
            assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), RUNTIME_SKILL_MD);
            assert_eq!(remove_runtime_skill(&dir).unwrap(), ManagedSkillRemoval::Removed);
            assert!(!dir.exists());
        }

        #[test]
        fn managed_skill_never_overwrites_an_unmanaged_same_name() {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path().join("nebula-runtime");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "user-owned\n").unwrap();

            assert_eq!(ensure_runtime_skill(&dir).unwrap(), ManagedSkillInstall::Conflict);
            assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), "user-owned\n");
            assert_eq!(remove_runtime_skill(&dir).unwrap(), ManagedSkillRemoval::Absent);
        }

        #[test]
        fn managed_skill_preserves_user_edits_during_update_and_remove() {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path().join("nebula-runtime");
            assert_eq!(ensure_runtime_skill(&dir).unwrap(), ManagedSkillInstall::Installed);
            std::fs::write(dir.join("SKILL.md"), "edited after install\n").unwrap();

            assert_eq!(ensure_runtime_skill(&dir).unwrap(), ManagedSkillInstall::Conflict);
            assert_eq!(remove_runtime_skill(&dir).unwrap(), ManagedSkillRemoval::Conflict);
            assert_eq!(
                std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
                "edited after install\n"
            );
        }
    }

    /// Claude's config directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    fn claude_config_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude"))
    }

    // ─── opencode plugin (~/.config/opencode/plugins/nebula.js) ─────────────

    /// opencode's global config dir. It uses `xdg-basedir`, which on Windows
    /// resolves `$XDG_CONFIG_HOME` else `~/.config` (NOT %APPDATA%), so mirror
    /// that exactly or the plugin lands where opencode never looks.
    fn opencode_config_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(dir).join("opencode"));
        }
        Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".config").join("opencode"))
    }

    /// Drop our event-forwarding plugin into opencode's global plugin dir.
    /// Unlike claude/codex, opencode never rewrites files under its own plugin
    /// dir, so no self-heal watcher is needed — a write-if-changed on boot
    /// suffices (and heals a stale copy after a Nebula upgrade). Only writes
    /// when opencode is actually installed. Returns whether it wrote.
    pub fn ensure_opencode_plugin() -> bool {
        // Only act when opencode exists — don't scaffold its config tree.
        let Some(cfg) = opencode_config_dir().filter(|d| d.exists()) else { return false };
        let dir = cfg.join("plugins");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!("ai_hook: cannot create {}: {err}", dir.display());
            return false;
        }
        let path = dir.join("nebula.js");
        // Skip the rewrite (and opencode's file-watcher reload) when identical.
        if std::fs::read_to_string(&path).is_ok_and(|cur| cur == OPENCODE_PLUGIN_JS) {
            return false;
        }
        match write_atomic(&path, OPENCODE_PLUGIN_JS) {
            Ok(()) => {
                log::info!("ai_hook: opencode plugin installed at {}", path.display());
                announce();
                true
            },
            Err(err) => {
                log::warn!("ai_hook: failed to write {}: {err}", path.display());
                false
            },
        }
    }

    /// Undo [`ensure_opencode_plugin`]: delete the plugin file if it is ours.
    fn remove_opencode_plugin() -> std::io::Result<bool> {
        let Some(cfg) = opencode_config_dir() else { return Ok(false) };
        let path = cfg.join("plugins").join("nebula.js");
        // Only delete a file we recognise as ours (carries the pipe env name).
        let ours = std::fs::read_to_string(&path).is_ok_and(|c| c.contains("NEBULA_HOOK_EXE"));
        if ours {
            std::fs::remove_file(&path)?;
            return Ok(true);
        }
        Ok(false)
    }

    // ─── Pi extension (~/.pi/agent/extensions/nebula.ts) ───────────────────

    fn pi_agent_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".pi").join("agent"))
    }

    /// Install the bridge only when Pi already has a global agent directory;
    /// Nebula must not create a fake Pi footprint for users who do not use it.
    pub fn ensure_pi_extension() -> bool {
        let Some(agent) = pi_agent_dir().filter(|dir| dir.exists()) else { return false };
        let dir = agent.join("extensions");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            log::warn!("ai_hook: cannot create {}: {err}", dir.display());
            return false;
        }
        let path = dir.join("nebula.ts");
        if std::fs::read_to_string(&path).is_ok_and(|current| current == PI_EXTENSION_TS) {
            return false;
        }
        match write_atomic(&path, PI_EXTENSION_TS) {
            Ok(()) => {
                log::info!("ai_hook: Pi extension installed at {}", path.display());
                announce();
                true
            },
            Err(err) => {
                log::warn!("ai_hook: failed to write {}: {err}", path.display());
                false
            },
        }
    }

    fn remove_pi_extension() -> std::io::Result<bool> {
        let Some(agent) = pi_agent_dir() else { return Ok(false) };
        let path = agent.join("extensions").join("nebula.ts");
        let ours = std::fs::read_to_string(&path).is_ok_and(|content| {
            content.contains("NEBULA_HOOK_EXE") && content.contains("Pi bridge")
        });
        if !ours {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }

    /// Absolute path of the bridge exe.
    ///
    /// The helper is an optional runtime asset, so a development checkout or
    /// an incomplete standalone directory is a valid state. `helper_path()`
    /// is called from several self-healing paths and from the pipe bootstrap;
    /// logging on every probe turns that state into an apparent infinite
    /// warning loop when a config watcher is busy. Keep the state transition
    /// noisy once, but make repeated probes silent until the helper is found
    /// again (or removed later).
    static HELPER_MISSING_ANNOUNCED: AtomicBool = AtomicBool::new(false);

    fn helper_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let helper = helper_path_from_exe(&exe);
        match helper {
            Some(path) => {
                // Permit a later installation/removal to be reported once on
                // the next state transition rather than caching a stale path.
                HELPER_MISSING_ANNOUNCED.store(false, Ordering::Relaxed);
                Some(path)
            },
            None => {
                if !HELPER_MISSING_ANNOUNCED.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "ai_hook: nebula-hook.exe missing from runtime/ and executable directory; AI integrations not installed"
                    );
                }
                None
            },
        }
    }

    fn helper_path_from_exe(exe: &Path) -> Option<PathBuf> {
        let exe_dir = exe.parent()?;
        // 新包优先使用分类目录，旧同目录位置仅用于开发构建和兼容历史包。
        [exe_dir.join("runtime").join("nebula-hook.exe"), exe_dir.join("nebula-hook.exe")]
            .into_iter()
            .find(|path| path.is_file())
    }

    /// The quoted claude hook command. Forward slashes on purpose: they
    /// survive every shell claude may run hooks through (cmd, PowerShell,
    /// git-bash).
    fn helper_command() -> Option<String> {
        let helper = helper_path()?;
        Some(format!("\"{}\" claude", helper.display().to_string().replace('\\', "/")))
    }

    /// Write via tmp + rename (MoveFileEx REPLACE_EXISTING under the hood):
    /// readers never observe a torn file, a crash leaves the original intact.
    fn write_atomic(path: &Path, data: &str) -> std::io::Result<()> {
        // 临时文件名带上进程号。多个 Nebula 实例各自守着同一份
        // `settings.json` 自愈，共用一个固定的 tmp 名就会互相踩：A 写 tmp、
        // B 覆盖同一个 tmp、A `rename` 把它搬走，B 的 `rename` 于是报
        // `ERROR_FILE_NOT_FOUND(2)`——一句"系统找不到指定的文件"，指的却是
        // 那个临时文件，读起来像 settings.json 不见了。
        //
        // 内容本身是幂等的（装的是同一套 hook 条目），所以最后谁赢都行，
        // 要防的只是这个假报错。
        let tmp = path.with_extension(format!("nebula-tmp-{}", std::process::id()));
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }

    #[cfg(test)]
    mod codex_notify_tests {
        use super::desired_codex_notify;

        const HELPER: &str = "C:/Program Files/Nebula/runtime/nebula-hook.exe";

        fn argv(args: &[&str]) -> Vec<String> {
            args.iter().map(|s| (*s).to_owned()).collect()
        }

        #[test]
        fn an_empty_slot_is_claimed_outright() {
            assert_eq!(desired_codex_notify(&[], HELPER), Some(argv(&[HELPER, "codex"])));
        }

        #[test]
        fn a_foreign_notifier_is_wrapped_behind_chain() {
            let current = argv(&["C:/cua/codex-computer-use.exe", "turn-ended"]);
            assert_eq!(
                desired_codex_notify(&current, HELPER),
                Some(argv(&[
                    HELPER,
                    "codex",
                    "--chain",
                    "C:/cua/codex-computer-use.exe",
                    "turn-ended",
                ]))
            );
        }

        #[test]
        fn our_stale_helper_path_heals_and_keeps_the_chain_tail() {
            let current = argv(&["D:/old/nebula-hook.exe", "codex", "--chain", "C:/cua/cua.exe"]);
            assert_eq!(
                desired_codex_notify(&current, HELPER),
                Some(argv(&[HELPER, "codex", "--chain", "C:/cua/cua.exe"]))
            );
        }

        #[test]
        fn an_up_to_date_wiring_is_left_alone() {
            let current = argv(&[HELPER, "codex"]);
            assert_eq!(desired_codex_notify(&current, HELPER), None);
        }

        // #38 的核心形态：codex-computer-use 重新注册时把我们的 chain JSON
        // 编码进 --previous-notify。我们不在最外层，但已在链中——再包一层
        // 就进入互相包装、反斜杠每轮翻倍的指数爆炸。
        #[test]
        fn a_notifier_that_swallowed_us_into_previous_notify_is_not_wrapped_again() {
            let current = argv(&[
                "C:/cua/codex-computer-use.exe",
                "--previous-notify",
                r#"["C:\\Program Files\\Nebula\\runtime\\nebula-hook.exe", "codex", "--chain", "C:\\cua\\cua.exe", "turn-ended"]"#,
                "turn-ended",
            ]);
            assert_eq!(desired_codex_notify(&current, HELPER), None);
        }

        // 兜底：即使标记检测失手（比如未来某个包装器改了我们的文件名），
        // 病态膨胀也会被字节预算拦住，config.toml 不会被写到 codex 起不来。
        #[test]
        fn an_oversized_result_is_refused() {
            let ballooned = "\\".repeat(64 * 1024);
            let current = argv(&["C:/cua/codex-computer-use.exe", &ballooned]);
            assert_eq!(desired_codex_notify(&current, HELPER), None);
        }
    }

    #[cfg(test)]
    mod runtime_asset_tests {
        use super::helper_path_from_exe;

        #[test]
        fn hook_helper_prefers_runtime_directory() {
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join("nebula.exe");
            let runtime = dir.path().join("runtime");
            std::fs::create_dir(&runtime).unwrap();
            std::fs::write(dir.path().join("nebula-hook.exe"), b"legacy").unwrap();
            std::fs::write(runtime.join("nebula-hook.exe"), b"structured").unwrap();

            assert_eq!(helper_path_from_exe(&exe), Some(runtime.join("nebula-hook.exe")));
        }

        #[test]
        fn hook_helper_falls_back_to_legacy_sibling() {
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join("nebula.exe");
            let legacy = dir.path().join("nebula-hook.exe");
            std::fs::write(&legacy, b"legacy").unwrap();

            assert_eq!(helper_path_from_exe(&exe), Some(legacy));
        }
    }
}
