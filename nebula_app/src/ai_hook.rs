//! Real-time AI-CLI turn state: typed lifecycle events from Claude Code /
//! Codex into the sidebar dots and the Windows notification center.
//!
//! # Why hooks, not the notification channel
//!
//! Claude Code's terminal notifications (`preferredNotifChannel`) are a dead
//! end on Windows: `auto` only recognizes Apple Terminal / iTerm2 / kitty /
//! ghostty and silently resolves to "no method available" everywhere else
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

/// Claude hook events we subscribe to.
const CLAUDE_EVENTS: [&str; 3] = ["UserPromptSubmit", "Stop", "Notification"];

/// What a lifecycle event means for the pane's turn state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiHookKind {
    /// The user submitted a prompt: a turn is running.
    PromptSubmit,
    /// The turn finished; the CLI waits for the next instruction.
    TurnDone,
    /// The CLI needs the user NOW (permission prompt, idle reminder).
    NeedsAttention,
}

/// A typed AI-CLI lifecycle event, parsed from one pipe connection.
#[derive(Debug, Clone)]
pub struct AiHookEvent {
    /// Pane hosting the CLI (from `NEBULA_PANE_ID`); `None` falls back to the
    /// focused pane (only happens when the env was stripped along the way).
    pub pane: Option<u64>,
    /// "claude" or "codex" — becomes the toast title.
    pub source: String,
    pub kind: AiHookKind,
    /// Human text when the event carries one (claude's notification message,
    /// codex's last assistant message).
    pub message: Option<String>,
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
    let (kind, message) = match source.as_str() {
        "claude" => match payload.get("hook_event_name").and_then(Value::as_str) {
            Some("UserPromptSubmit") => (AiHookKind::PromptSubmit, None),
            Some("Stop") => (AiHookKind::TurnDone, None),
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
        "opencode" => match payload.get("kind").and_then(Value::as_str) {
            Some("prompt") => (AiHookKind::PromptSubmit, None),
            Some("done") => (AiHookKind::TurnDone, None),
            Some("attention") => (
                AiHookKind::NeedsAttention,
                payload.get("message").and_then(Value::as_str).map(|m| truncate(m, 300)),
            ),
            _ => return None,
        },
        _ => return None,
    };
    Some(AiHookEvent { pane, source, kind, message })
}

/// 远端会话只能提交事件语义，Pane 身份始终由本地 SSH 通道覆盖，
/// 防止远端载荷把通知路由到同一窗口中的其他标签页。
pub(crate) fn parse_remote_envelope(bytes: &[u8], pane: Option<u64>) -> Option<AiHookEvent> {
    let mut event = parse_envelope(bytes)?;
    event.pane = pane;
    Some(event)
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
pub use win::{setup_ai_cli, spawn_config_guard, spawn_server};

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
            .spawn(move || serve(&name, proxy))
        {
            log::warn!("ai_hook: failed to spawn pipe server: {err}");
        }
    }

    /// Accept loop. One fresh pipe instance per connection: a client racing
    /// the turnaround sees a failed open for microseconds and retries (the
    /// helper retries for ~100 ms — an eternity at this message rate).
    fn serve(name: &str, proxy: EventLoopProxy<Event>) {
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
                    if proxy.send_event(Event::new(EventType::AiHook(event), None)).is_err() {
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
        if let Err(err) =
            std::thread::Builder::new().name("nebula-ai-setup".into()).spawn(config_guard)
        {
            log::warn!("ai_hook: failed to spawn settings guard: {err}");
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
            {
                break (claude, codex);
            }
            std::thread::sleep(Duration::from_secs(300));
        };

        ensure_claude_hooks();
        ensure_codex_notify();
        ensure_opencode_plugin();

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
                    ensure_claude_hooks();
                    ensure_codex_notify();
                    ensure_opencode_plugin();
                },
                Err(_) => return, // channel closed: shutting down
            }
        }
    }

    /// Degraded guard when file watching is unavailable: heal every 5 min.
    fn poll_guard() -> ! {
        loop {
            std::thread::sleep(Duration::from_secs(300));
            ensure_claude_hooks();
            ensure_codex_notify();
            ensure_opencode_plugin();
        }
    }

    /// One-time-per-process announcement so a switcher rewriting the file in
    /// a loop cannot flood the Action Center with "hooks reinstalled".
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);

    fn announce() {
        if !ANNOUNCED.swap(true, Ordering::Relaxed) {
            crate::notify::toast(
                "Nebula",
                "已接入 AI 回合通知（Claude hooks / Codex notify）。撤销：nebula setup-ai --remove",
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

        let desired: Vec<String> = match current.first() {
            // Already ours: heal the helper path, keep any chain tail as-is.
            Some(first) if first.contains(HELPER_MARK) => {
                let mut argv = current.clone();
                argv[0] = helper;
                argv
            },
            // Occupied: wrap the existing notifier behind --chain.
            Some(_) => {
                let mut argv = vec![helper, "codex".to_owned(), "--chain".to_owned()];
                argv.extend(current.iter().cloned());
                argv
            },
            None => vec![helper, "codex".to_owned()],
        };
        if current == desired {
            return false;
        }

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
  const send = (obj) => {
    // Fire-and-forget; never throw into opencode's event loop.
    try { $`${hook} opencode ${JSON.stringify(obj)}`.quiet().nothrow().catch(() => {}) }
    catch (_) {}
  }
  return {
    event: async ({ event }) => {
      const t = event && event.type
      if (t === "message.updated") {
        const info = event.properties && event.properties.info
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
        send({ kind: "attention", message: (event.properties && event.properties.title) || "" })
      }
    },
  }
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
            match remove_hooks() {
                Ok(true) => println!("claude: 已从 {} 移除 hooks。", path.display()),
                Ok(false) => println!("claude: {} 中没有 Nebula 的 hooks。", path.display()),
                Err(err) => {
                    eprintln!("claude: 移除失败：{err}");
                    return 1;
                },
            }
            match remove_codex_notify() {
                Ok(true) => println!("codex: 已还原 config.toml 的 notify。"),
                Ok(false) => println!("codex: notify 不是 Nebula 接管的，未改动。"),
                Err(err) => {
                    eprintln!("codex: 还原失败：{err}");
                    return 1;
                },
            }
            match remove_opencode_plugin() {
                Ok(true) => println!("opencode: 已删除 plugins/nebula.js。"),
                Ok(false) => println!("opencode: 没有 Nebula 的插件，未改动。"),
                Err(err) => {
                    eprintln!("opencode: 删除失败：{err}");
                    return 1;
                },
            }
            return 0;
        }
        match helper_command() {
            Some(command) => println!("hook 命令：{command}"),
            None => {
                eprintln!("runtime/ 和 nebula.exe 同目录中均未找到 nebula-hook.exe，无法安装。");
                return 1;
            },
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
        println!("对新启动的会话生效；正在运行的会话保持原快照。");
        0
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

    /// Absolute path of the bridge exe.
    fn helper_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let helper = helper_path_from_exe(&exe);
        if helper.is_none() {
            log::warn!(
                "ai_hook: nebula-hook.exe missing from runtime/ and executable directory; AI integrations not installed"
            );
        }
        helper
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
        let tmp = path.with_extension("nebula-tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
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
