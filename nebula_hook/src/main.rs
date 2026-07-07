//! nebula-hook — the bridge between AI-CLI lifecycle hooks and Nebula.
//!
//! Claude Code (`Stop` / `Notification` / `UserPromptSubmit` hooks) and Codex
//! (`notify` program) invoke this for every turn event. It forwards the raw
//! payload to the hosting Nebula instance over a named pipe and exits.
//! Design constraints, in order:
//!
//! 1. INVISIBLE: a Stop hook's exit code is meaningful to claude (non-zero
//!    surfaces an error banner, 2 even blocks the turn). Every path —
//!    including panic — must exit 0, fast. Claude also writes the payload to
//!    our stdin, so claude mode always drains stdin even when the message
//!    goes nowhere: an unread pipe could surface as a hook write error.
//! 2. SCOPED: the hook config is global (settings.json), but the effect must
//!    be Nebula-only. The scope guard is the environment: NEBULA_NOTIFY_PIPE
//!    only exists for processes spawned inside Nebula. Anywhere else this is
//!    an invisible ~10 ms no-op.
//! 3. FAST: pure std, no JSON handling (Nebula parses), one pipe write.
//!    Keeps the whole claude→toast chain under ~50 ms.
//!
//! Usage (installed by `nebula setup-ai` / Nebula's boot self-heal):
//! ```text
//! nebula-hook claude                              # payload on stdin
//! nebula-hook codex <json>                        # payload as last arg
//! nebula-hook codex --chain <exe> <fixed…> <json> # + exec previous notifier
//! ```
//! `--chain` exists because codex has a single `notify` slot which may
//! already be taken (e.g. OpenAI's own computer-use notifier): we forward to
//! Nebula and then invoke the original program with the same payload.

use std::io::{Read, Write};

fn main() {
    // Constraint 1: never leak a failure to the calling CLI.
    let _ = std::panic::catch_unwind(run);
}

fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(source) = args.first().filter(|s| matches!(s.as_str(), "claude" | "codex")) else {
        return;
    };

    // Payload: claude streams JSON on stdin; codex appends it as the last arg.
    let payload = match source.as_str() {
        "claude" => {
            let mut buf = Vec::with_capacity(4096);
            // Cap far above any real payload; claude closes stdin right away.
            let _ = std::io::stdin().lock().take(1 << 20).read_to_end(&mut buf);
            buf
        },
        _ => args.last().cloned().unwrap_or_default().into_bytes(),
    };

    // Constraint 2: outside Nebula there is no pipe variable — do nothing.
    if let Some(pipe) = std::env::var_os("NEBULA_NOTIFY_PIPE") {
        let pane = std::env::var("NEBULA_PANE_ID").unwrap_or_default();
        let mut message = format!("nebula-hook/1 source={source} pane={pane}\n").into_bytes();
        message.extend_from_slice(&payload);

        // The server accepts one connection at a time and re-creates the pipe
        // instance in between, so a raced connect fails for microseconds.
        // Retry briefly, then give up silently: notifications are best-effort.
        for _ in 0..20 {
            match std::fs::OpenOptions::new().write(true).open(&pipe) {
                Ok(mut file) => {
                    let _ = file.write_all(&message);
                    break;
                },
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    }

    // Chain mode: keep a pre-existing codex notifier working. Runs even
    // outside Nebula — the original program must keep firing everywhere.
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let ["codex", "--chain", prog, rest @ ..] = &strs[..] {
        if !rest.is_empty() {
            let (fixed, json) = rest.split_at(rest.len() - 1);
            let _ = std::process::Command::new(prog).args(fixed).args(json).spawn();
        }
    }
}
