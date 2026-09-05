//! nebula-hook — the bridge between AI-CLI lifecycle hooks and Nebula.
//!
//! Claude Code (`Stop` / `Notification` / `UserPromptSubmit` hooks), Codex
//! (`notify` program), Pi and opencode (bundled extensions/plugins, shelling out on
//! `session.idle` / `permission.updated` / user-prompt) invoke this for every
//! turn event. It forwards the raw payload to the hosting Nebula instance over
//! a named pipe and exits.
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
//! nebula-hook opencode <json>                     # payload as last arg
//! nebula-hook pi <json>                           # payload as last arg
//! ```
//! `--chain` exists because codex has a single `notify` slot which may
//! already be taken (e.g. OpenAI's own computer-use notifier): we forward to
//! Nebula and then invoke the original program with the same payload.
//!
//! `opencode` is fed by a Nebula-authored plugin (dropped into the user's
//! opencode plugin dir) that subscribes to opencode's event bus and shells out
//! here with a normalized `{"kind":...}` payload — same wire shape as codex.

use std::io::{Read, Write};

const MAX_PAYLOAD_BYTES: usize = 1 << 20;

/// 其他 agent 的 hook runner 独有的环境变量。
///
/// 这些 CLI 会主动读取并信任 `~/.claude/settings.json`，于是我们装在那里的
/// claude hook 也会在**它们的**事件上被触发。把那些事件当 claude 上报有两个
/// 后果：pane 贴错 provider 身份，以及把别家的 session id 交给
/// `claude --resume` —— 一个根本不存在的会话。每个 runner 都会向 hook 进程
/// 导出自己独有的变量，这是唯一可靠的判据。
///
/// 这道门是承重的，不是保险。它也是会**静默失效**的那一类：变量一旦被上游
/// 改名或停止导出，门就永远不再命中，而且没有任何报错。Nebula 因此不把它当
/// 唯一防线——载荷形状校验在 `ai_hook::parse_envelope` 里独立拦一次，
/// `foreign_runner_payload_is_rejected_even_if_the_env_gate_fails` 就是钉住这
/// 个前提的测试。新增 provider 时在这里加一行。
const FOREIGN_HOOK_RUNNERS: &[&str] = &[
    // Grok Build 的 hook runner 注入（形如 "user:stop[0].hooks[0]"）。它替代
    // 了早期的 GROK_SESSION_ID：后者已不再导出，只在被插值进 hook 命令时才
    // 解析得到——如果当初的门写在那个变量上，今天就是一扇不会响的门。
    "GROK_HOOK_NAME",
];

/// 当前进程是否由别家 agent 的 hook runner 启动。
fn foreign_hook_runner() -> Option<&'static str> {
    FOREIGN_HOOK_RUNNERS
        .iter()
        .copied()
        .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// 一次调用的去向。写进侧信道日志，用来回答「通知为什么没出现」。
enum Outcome {
    PayloadTooLarge,
    /// 写进了本地命名管道。
    Sent,
    /// 写进了远端 OSC 通道（SSH pane）。
    RemoteOsc,
    /// 不在 Nebula 里运行：既没有管道也没有远端令牌。这是最常见的一种，
    /// 也是设计如此（约束 2）。
    NotHosted,
    /// 管道存在但连不上（服务端正在换实例，或宿主已退出）。
    PipeUnavailable,
    /// 调用方是别家 agent 的 hook runner，串台门闭合。
    ForeignRunner,
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "payload-too-large",
            Self::Sent => "sent",
            Self::RemoteOsc => "remote-osc",
            Self::NotHosted => "not-hosted",
            Self::PipeUnavailable => "pipe-unavailable",
            Self::ForeignRunner => "foreign-runner",
        }
    }
}

/// JSON 字符串转义。`pane` 来自环境变量，任何进程都能把它设成带引号或控制字符
/// 的内容；不转义就会写出一行坏掉的 NDJSON。
fn escape_json(raw: &str, out: &mut String) {
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn outcome_line(source: &str, pane: &str, bytes: usize, outcome: &Outcome, at_ms: u128) -> String {
    let mut line = String::with_capacity(160);
    line.push_str("{\"at_ms\":");
    line.push_str(&at_ms.to_string());
    line.push_str(",\"source\":\"");
    escape_json(source, &mut line);
    line.push_str("\",\"pane\":\"");
    escape_json(pane, &mut line);
    line.push_str("\",\"payload_bytes\":");
    line.push_str(&bytes.to_string());
    line.push_str(",\"outcome\":\"");
    line.push_str(outcome.as_str());
    line.push_str("\"}\n");
    line
}

/// 结构化侧信道。默认完全关闭：只有 `NEBULA_HOOK_LOG` 指向一个可追加的文件时
/// 才写，每次一行 NDJSON。
///
/// 为什么需要它：这个进程的所有失败都被有意吞掉（约束 1——退出码对调用方的
/// CLI 有意义），于是「完成通知没出现」这类问题事后完全无从取证。只记路由事实
/// 和去向，**绝不记载荷内容**：payload 里有 cwd、工具参数，甚至选区正文。
fn log_outcome(source: &str, pane: &str, bytes: usize, outcome: &Outcome) {
    let Some(path) = std::env::var_os("NEBULA_HOOK_LOG").filter(|value| !value.is_empty()) else {
        return;
    };
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let line = outcome_line(source, pane, bytes, outcome, at_ms);
    // 追加打开 + 一次写入。失败一律忽略：日志绝不能影响 agent。
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn main() {
    // Constraint 1: never leak a failure to the calling CLI.
    let _ = std::panic::catch_unwind(run);
}

fn read_payload(mut reader: impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::with_capacity(4096);
    reader.by_ref().take((MAX_PAYLOAD_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        std::io::copy(&mut reader, &mut std::io::sink())?;
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(source) =
        args.first().filter(|s| matches!(s.as_str(), "claude" | "codex" | "opencode" | "pi"))
    else {
        return;
    };

    // Payload: claude streams JSON on stdin; codex and opencode append it as
    // the last arg.
    let payload = match source.as_str() {
        "claude" => match read_payload(std::io::stdin().lock()) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                log_outcome(source, "", MAX_PAYLOAD_BYTES + 1, &Outcome::PayloadTooLarge);
                return;
            },
            Err(_) => return,
        },
        _ => args.last().cloned().unwrap_or_default().into_bytes(),
    };

    // 串台门。放在读完 stdin 之后：约束 1 要求 claude 模式无论如何都把 stdin
    // 抽干（未读的管道会在 CLI 侧变成 hook write error），所以先读再退。
    if source == "claude" && foreign_hook_runner().is_some() {
        log_outcome(source, "", payload.len(), &Outcome::ForeignRunner);
        return;
    }

    let pane = std::env::var("NEBULA_PANE_ID").unwrap_or_default();
    let mut message = format!("nebula-hook/1 source={source} pane={pane}\n").into_bytes();
    message.extend_from_slice(&payload);

    // 本地 Pane 使用命名管道；远端 Pane 没有本地管道时，把同一信封写入控制终端的私有 OSC。
    let mut outcome = Outcome::NotHosted;
    if let Some(pipe) = std::env::var_os("NEBULA_NOTIFY_PIPE") {
        // The server accepts one connection at a time and re-creates the pipe
        // instance in between, so a raced connect fails for microseconds.
        // Retry briefly, then give up silently: notifications are best-effort.
        outcome = Outcome::PipeUnavailable;
        for _ in 0..20 {
            match std::fs::OpenOptions::new().write(true).open(&pipe) {
                Ok(mut file) => {
                    let _ = file.write_all(&message);
                    outcome = Outcome::Sent;
                    break;
                },
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    } else if let Ok(token) = std::env::var("NEBULA_REMOTE_HOOK_TOKEN") {
        if token.len() == 32
            && token.bytes().all(|byte| byte.is_ascii_hexdigit())
            && message.len() <= 64 * 1024
        {
            let osc = format!("\x1b]777;nebula-hook;{};{}\x07", token, base64_encode(&message));
            if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
                let _ = tty.write_all(osc.as_bytes());
                let _ = tty.flush();
                outcome = Outcome::RemoteOsc;
            }
        }
    }
    log_outcome(source, &pane, payload.len(), &outcome);

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

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 { TABLE[(c & 0x3f) as usize] as char } else { '=' });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{FOREIGN_HOOK_RUNNERS, Outcome, base64_encode, foreign_hook_runner, outcome_line};

    #[test]
    fn oversized_stdin_is_drained_but_never_forwarded_as_truncated_json() {
        let bytes = vec![b'x'; super::MAX_PAYLOAD_BYTES * 2];
        let mut input = std::io::Cursor::new(&bytes);
        assert!(super::read_payload(&mut input).unwrap().is_none());
        assert_eq!(input.position(), bytes.len() as u64);
        assert_eq!(
            super::read_payload(&b"{\"answer\":\"ok\"}"[..]).unwrap().unwrap(),
            b"{\"answer\":\"ok\"}"
        );
    }

    #[test]
    fn encodes_remote_hook_payload() {
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"a"), "YQ==");
    }

    /// 这道门是承重的：命中任一别家 runner 的变量就必须闭合。空值不算命中——
    /// 有些 runner 会把变量导出成空串。
    ///
    /// 环境变量是进程全局状态，所以这里串行地设置和清理，不与其他测试共用变量。
    #[test]
    fn foreign_runner_gate_closes_on_a_non_empty_marker() {
        let marker = FOREIGN_HOOK_RUNNERS[0];
        assert!(!FOREIGN_HOOK_RUNNERS.is_empty(), "至少要保留一个已知 runner 判据");
        // SAFETY: 单线程测试内自设自清，没有并发读者。
        unsafe {
            std::env::remove_var(marker);
        }
        assert_eq!(foreign_hook_runner(), None);
        unsafe {
            std::env::set_var(marker, "");
        }
        assert_eq!(foreign_hook_runner(), None, "空值不足以证明调用方是别家 runner");
        unsafe {
            std::env::set_var(marker, "user:stop[0].hooks[0]");
        }
        assert_eq!(foreign_hook_runner(), Some(marker));
        unsafe {
            std::env::remove_var(marker);
        }
    }

    /// 侧信道每行必须是合法 NDJSON，而且不能夹带载荷内容。`pane` 来自环境
    /// 变量，是这行里唯一可被外部塞进引号和控制字符的字段。
    #[test]
    fn outcome_line_is_valid_ndjson_and_carries_no_payload() {
        let line = outcome_line("claude", "7", 1234, &Outcome::Sent, 1_700_000_000_123);
        assert_eq!(
            line,
            "{\"at_ms\":1700000000123,\"source\":\"claude\",\"pane\":\"7\",\"payload_bytes\":1234,\"outcome\":\"sent\"}\n"
        );

        let hostile = outcome_line("claude", "7\",\"outcome\":\"sent\n", 0, &Outcome::NotHosted, 1);
        assert!(hostile.ends_with("\"outcome\":\"not-hosted\"}\n"), "结论字段必须是最后一个");
        // 注入的引号和换行都被转义，整行仍然只有一行。
        assert_eq!(hostile.matches('\n').count(), 1);
        assert!(hostile.contains("\\\"") && hostile.contains("\\n"));
    }
}
