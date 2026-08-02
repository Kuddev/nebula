//! Nebula 助手：错误自动恢复（docs/specs/001-nebula-assistant.md 阶段一）。
//!
//! 命令以非零码退出（OSC 133;D;<code>，Nebula 自己的 shell 集成上报）时，
//! 把失败命令、退出码、cwd、git 分支和 **grid 里的真实输出尾部**发给一个
//! OpenAI 兼容端点，拿回一条修复建议，画进 pane 底部的建议条；Ctrl+. 只贴
//! 入不执行。触发判定全部在本模块的纯函数里（可单测），网络在后台 OS 线程
//! 阻塞完成（Kaku 验证过的模型），结果经 `EventType::AiFixReady` 回主循环。
//!
//! 隐私边界（spec「安全与隐私」）：默认关闭；开启后发送的内容 = 命令文本、
//! 退出码、目录尾部、分支名、≤2000 字符输出尾部（先经 [`redact_secrets`]
//! 打码）。API key 不落明文配置：环境变量或 Windows 凭据管理器。

use std::path::PathBuf;
use std::time::Duration;

use log::{info, warn};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};

/// 配置文件名（位于 `nebula_data_dir()`）。独立于 `nebula_settings.txt`：
/// 设置页整存整取那份文件，手工加进去的陌生键会在下次保存时被抹掉；
/// 助手配置独立成文件，阶段三设置页接管时再迁移。
const CONFIG_FILE: &str = "nebula_assistant.txt";

/// A fix produced by the model for one failed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiFix {
    pub command: String,
    pub explain: String,
    pub danger: bool,
}

/// Per-pane lifecycle of one suggestion. `seq` guards against a stale
/// response landing after a newer failure took over the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiFixState {
    /// Request in flight; the bar shows "analyzing".
    Pending { seq: u64 },
    /// Suggestion on screen, waiting for Ctrl+. / Esc / typing.
    Ready { seq: u64, fix: AiFix },
}

impl AiFixState {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Pending { seq } | Self::Ready { seq, .. } => *seq,
        }
    }
}

/// `nebula_assistant.txt` 的解析结果；文件缺失 = 全默认 = 关闭。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub ignored_exit_codes: Vec<i32>,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-5.4-mini".into(),
            ignored_exit_codes: Vec::new(),
        }
    }
}

impl AssistantConfig {
    /// Read the config file. Called per failed command, not per frame — a
    /// sub-millisecond read beats cache-invalidation plumbing here.
    pub fn load() -> Self {
        Self::parse(&std::fs::read_to_string(config_path()).unwrap_or_default())
    }

    fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "enabled" => cfg.enabled = matches!(value, "on" | "true" | "1"),
                "base_url" if !value.is_empty() => cfg.base_url = value.to_owned(),
                "model" if !value.is_empty() => cfg.model = value.to_owned(),
                "ignored_exit_codes" => {
                    cfg.ignored_exit_codes =
                        value.split(',').filter_map(|c| c.trim().parse().ok()).collect();
                },
                _ => {},
            }
        }
        cfg
    }
}

pub fn config_path() -> PathBuf {
    crate::display::nebula_data_dir().join(CONFIG_FILE)
}

/// API key 的来源：`NEBULA_AI_KEY` → `OPENAI_API_KEY`。凭据管理器存储随
/// 阶段三的配置中心一起来——没有录入 UI 的加密存储是空中楼阁。
fn api_key() -> Option<String> {
    for var in ["NEBULA_AI_KEY", "OPENAI_API_KEY"] {
        if let Ok(key) = std::env::var(var) {
            let key = key.trim().to_owned();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}

/// Exit codes that mean "the user stopped it", not "it failed":
/// 130/143/137 = SIGINT/SIGTERM/SIGKILL + 128; the negative constant is
/// Windows STATUS_CONTROL_C_EXIT as an i32.
const USER_ABORT_CODES: &[i32] = &[130, 137, 143, -1073741510];

/// Single-word invocations that fail by design (usage screens): suggesting a
/// "fix" for a bare `git` would only annoy.
const BARE_TOOLS: &[&str] =
    &["npm", "pnpm", "yarn", "pip", "pip3", "cargo", "git", "dotnet", "go", "docker", "kubectl"];

/// Interactive programs whose non-zero exit is routine (`:cq`, `q` under
/// load, a dropped ssh) — not something to "fix".
const INTERACTIVE: &[&str] = &[
    "vim", "nvim", "vi", "nano", "less", "more", "top", "htop", "btop", "ssh", "claude", "codex",
    "gemini", "aider", "lazygit", "yazi", "ranger", "fzf", "gh",
];

/// 防误触判定（spec 的规则表）。`program` 是侧栏图标那份身份
/// （`extract_program(last_committed)`）。
pub fn should_suggest(
    exit_code: i32,
    command: &str,
    program: Option<&str>,
    ignored: &[i32],
) -> bool {
    if exit_code == 0 || USER_ABORT_CODES.contains(&exit_code) || ignored.contains(&exit_code) {
        return false;
    }
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let mut words = command.split_whitespace();
    let first = words.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<&str> = words.collect();
    // Help screens exit non-zero on several tools; the user asked for them.
    if rest.iter().any(|w| matches!(*w, "--help" | "-h" | "-?" | "/?" | "help")) {
        return false;
    }
    if rest.is_empty() && BARE_TOOLS.contains(&first.as_str()) {
        return false;
    }
    if let Some(program) = program {
        if INTERACTIVE.contains(&program.to_ascii_lowercase().as_str()) {
            return false;
        }
    }
    true
}

/// 同 pane 两次触发之间的最短间隔（连续失败的循环里别刷屏）。
pub const COOLDOWN: Duration = Duration::from_secs(5);

/// Local danger check, OR-ed with the model's own `danger` flag — the model
/// judges intent, this judges the literal text; either alone can be fooled.
pub fn is_dangerous(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "git reset --hard",
        "git push --force",
        "git push -f",
        "git clean -fd",
        "remove-item -recurse",
        "rd /s",
        "del /s",
        "del /f",
        "format ",
        "mkfs",
        "dd if=",
        "shutdown",
        "taskkill /f",
        "kill -9",
        "chmod -r 777",
        "> /dev/sda",
        "drop table",
        "truncate table",
    ];
    PATTERNS.iter().any(|p| lower.contains(p))
}

/// 打码疑似密钥：40+ 连续 base64/hex 字符（token、私钥体、connection
/// string 的主体形态）替换为 `[redacted]` 后再出境。
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= 40 {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric()
            || ch == '+'
            || ch == '/'
            || ch == '='
            || ch == '-'
            || ch == '_'
        {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Everything one fix request carries to the model.
#[derive(Debug, Clone)]
pub struct FixRequest {
    pub pane: u64,
    pub seq: u64,
    pub command: String,
    pub exit_code: i32,
    pub cwd: String,
    pub branch: String,
    /// Last lines of the command's real output, ANSI-free, already redacted.
    pub output_tail: String,
}

const SYSTEM_PROMPT: &str = "You fix failed shell commands inside a terminal. Reply with ONLY a \
JSON object, no markdown fence: {\"command\":\"<corrected command>\",\"explain\":\"<why, max 120 \
chars>\",\"danger\":<bool>}. Write \"explain\" in the same human language as the terminal output \
(Chinese output → Chinese explain). Set \"danger\" true when the command deletes or overwrites \
data, force-pushes, rewrites history, kills processes, or changes system state. If no sensible \
fix exists, reply {\"command\":\"\",\"explain\":\"<one-line reason>\",\"danger\":false}.";

/// Fire one fix request on a background thread; the outcome (a fix, or None
/// for "stay silent") lands back on the main loop as [`EventType::AiFixReady`].
pub fn spawn_fix_request(proxy: EventLoopProxy<Event>, cfg: AssistantConfig, req: FixRequest) {
    let spawned = std::thread::Builder::new().name("nebula-ai-fix".into()).spawn(move || {
        let fix = request_fix(&cfg, &req);
        let _ = proxy.send_event(Event::new(
            EventType::AiFixReady { pane: req.pane, seq: req.seq, fix },
            None,
        ));
    });
    if let Err(err) = spawned {
        warn!("assistant: spawn failed: {err}");
    }
}

/// 失败即沉默（Kaku 的哲学：拿不到建议和「刻意不建议」同款处理），但要
/// 把原因写进日志——静默功能坏起来只有日志能救。
fn request_fix(cfg: &AssistantConfig, req: &FixRequest) -> Option<AiFix> {
    let Some(key) = api_key() else {
        info!("assistant: no API key (NEBULA_AI_KEY / OPENAI_API_KEY / credential store)");
        return None;
    };
    let user = format!(
        "OS: {}\nCommand: {}\nExit code: {}\nDirectory: {}\nGit branch: {}\nOutput (tail):\n{}",
        std::env::consts::OS,
        req.command,
        req.exit_code,
        req.cwd,
        if req.branch.is_empty() { "-" } else { &req.branch },
        req.output_tail,
    );
    let body = serde_json::json!({
        "model": cfg.model,
        "temperature": 0.2,
        "max_tokens": 300,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user},
        ],
    });
    let config =
        ureq::config::Config::builder().timeout_global(Some(Duration::from_secs(30))).build();
    let agent: ureq::Agent = config.new_agent();
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut response =
        match agent.post(&url).header("Authorization", &format!("Bearer {key}")).send_json(&body) {
            Ok(response) => response,
            Err(err) => {
                warn!("assistant: request failed: {err}");
                return None;
            },
        };
    let value: serde_json::Value = match response.body_mut().read_json() {
        Ok(value) => value,
        Err(err) => {
            warn!("assistant: bad response body: {err}");
            return None;
        },
    };
    let content = value["choices"][0]["message"]["content"].as_str()?;
    parse_fix(content)
}

/// Extract the `{...}` object from the model's reply (models love to wrap
/// JSON in prose or fences no matter what the system prompt says).
fn parse_fix(content: &str) -> Option<AiFix> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(&content[start..=end]).ok()?;
    let command = value["command"].as_str()?.trim().to_owned();
    if command.is_empty() || command.contains('\n') {
        return None;
    }
    let explain = value["explain"].as_str().unwrap_or_default().trim().to_owned();
    let danger = value["danger"].as_bool().unwrap_or(false) || is_dangerous(&command);
    Some(AiFix { command, explain, danger })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_only_for_real_failures() {
        assert!(should_suggest(127, "cargoo build", None, &[]));
        assert!(!should_suggest(0, "cargo build", None, &[]));
        // User aborts, on both Unix and Windows conventions.
        assert!(!should_suggest(130, "npm run dev", None, &[]));
        assert!(!should_suggest(-1073741510, "ping localhost", None, &[]));
        // Help screens and bare package managers fail by design.
        assert!(!should_suggest(1, "git --help", None, &[]));
        assert!(!should_suggest(1, "cargo", None, &[]));
        assert!(should_suggest(101, "cargo buld", None, &[]));
        // Interactive programs exiting non-zero are routine.
        assert!(!should_suggest(1, "vim src/main.rs", Some("vim"), &[]));
        // User-configured ignore list.
        assert!(!should_suggest(2, "make", None, &[2]));
    }

    #[test]
    fn danger_catches_both_shells() {
        assert!(is_dangerous("rm -rf /tmp/x"));
        assert!(is_dangerous("git push --force origin main"));
        assert!(is_dangerous("Remove-Item -Recurse -Force C:\\x"));
        assert!(!is_dangerous("cargo build --release"));
    }

    #[test]
    fn redaction_hides_long_tokens_only() {
        let text = "token=ghp_0123456789abcdef0123456789abcdef01234567 path=C:/x";
        let out = redact_secrets(text);
        assert!(out.contains("[redacted]"), "got {out}");
        assert!(out.contains("path=C:/x"));
        assert_eq!(redact_secrets("plain words"), "plain words");
    }

    #[test]
    fn parses_model_reply_with_fence_noise() {
        let fix = parse_fix("Sure!\n```json\n{\"command\":\"cargo build\",\"explain\":\"拼写\",\"danger\":false}\n```").unwrap();
        assert_eq!(fix.command, "cargo build");
        assert_eq!(fix.explain, "拼写");
        assert!(!fix.danger);
        // Local danger regex overrides a lying model.
        let fix =
            parse_fix("{\"command\":\"rm -rf /\",\"explain\":\"x\",\"danger\":false}").unwrap();
        assert!(fix.danger);
        // Empty command = deliberate silence.
        assert!(parse_fix("{\"command\":\"\",\"explain\":\"no fix\",\"danger\":false}").is_none());
    }

    #[test]
    fn config_parses_and_defaults_off() {
        let cfg = AssistantConfig::parse("enabled=on\nmodel=m1\nignored_exit_codes=2, 3\n");
        assert!(cfg.enabled);
        assert_eq!(cfg.model, "m1");
        assert_eq!(cfg.ignored_exit_codes, vec![2, 3]);
        assert!(!AssistantConfig::parse("").enabled);
    }
}
