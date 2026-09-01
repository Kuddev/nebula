//! Shared AI-CLI identity, resumability and screen-state detection.
//!
//! Hooks remain the highest-confidence lifecycle source. This module fills the
//! gaps hooks cannot cover: wrapped process identity, agents without a hook
//! API, and interactive permission/question screens that a coarse "turn done"
//! callback cannot distinguish.
//!
//! Detection rules are declarative TOML. Bundled rules cover the popular
//! clients; `%APPDATA%\Nebula\agent-detection\<slug>.toml` (or the equivalent
//! [`nebula_settings::settings_dir`]) overrides one manifest. Overrides are
//! mtime-checked at most once every two seconds, so rules can be tuned while a
//! client is running without putting filesystem work on every frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use serde::Deserialize;

/// AI clients Nebula can identify as a first-class terminal workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Aider,
    Amp,
    OpenCode,
    Copilot,
    Cursor,
    Goose,
    Droid,
    Pi,
    Auggie,
    Hermes,
    Vibe,
    Antigravity,
    Grok,
    Qwen,
    OhMyPi,
    Cline,
    Devin,
    Kimi,
    Kiro,
    Kilo,
    Qoder,
    Maki,
}

impl AgentKind {
    pub const ALL: [Self; 25] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Aider,
        Self::Amp,
        Self::OpenCode,
        Self::Copilot,
        Self::Cursor,
        Self::Goose,
        Self::Droid,
        Self::Pi,
        Self::Auggie,
        Self::Hermes,
        Self::Vibe,
        Self::Antigravity,
        Self::Grok,
        Self::Qwen,
        Self::OhMyPi,
        Self::Cline,
        Self::Devin,
        Self::Kimi,
        Self::Kiro,
        Self::Kilo,
        Self::Qoder,
        Self::Maki,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Aider => "aider",
            Self::Amp => "amp",
            Self::OpenCode => "opencode",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::Goose => "goose",
            Self::Droid => "droid",
            Self::Pi => "pi",
            Self::Auggie => "auggie",
            Self::Hermes => "hermes",
            Self::Vibe => "vibe",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
            Self::Qwen => "qwen",
            Self::OhMyPi => "omp",
            Self::Cline => "cline",
            Self::Devin => "devin",
            Self::Kimi => "kimi",
            Self::Kiro => "kiro",
            Self::Kilo => "kilo",
            Self::Qoder => "qodercli",
            Self::Maki => "maki",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
            Self::Aider => "Aider",
            Self::Amp => "Amp",
            Self::OpenCode => "OpenCode",
            Self::Copilot => "GitHub Copilot",
            Self::Cursor => "Cursor Agent",
            Self::Goose => "Goose",
            Self::Droid => "Droid",
            Self::Pi => "Pi",
            Self::Auggie => "Auggie",
            Self::Hermes => "Hermes",
            Self::Vibe => "Vibe",
            Self::Antigravity => "Antigravity",
            Self::Grok => "Grok",
            Self::Qwen => "Qwen Code",
            Self::OhMyPi => "Oh My Pi",
            Self::Cline => "Cline",
            Self::Devin => "Devin",
            Self::Kimi => "Kimi Code",
            Self::Kiro => "Kiro",
            Self::Kilo => "Kilo Code",
            Self::Qoder => "Qoder",
            Self::Maki => "Maki",
        }
    }

    pub fn label(self) -> &'static str {
        self.slug()
    }

    fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["claude", "claude-code"],
            Self::Codex => &["codex", "codex-cli"],
            Self::Gemini => &["gemini", "gemini-cli"],
            Self::Aider => &["aider", "aider-chat"],
            Self::Amp => &["amp", "amp-local"],
            Self::OpenCode => &["opencode", "open-code"],
            Self::Copilot => &["copilot", "github-copilot", "ghcs"],
            // Cursor's current CLI binary is `agent`; keep the historical
            // labels so existing process snapshots still normalize correctly.
            Self::Cursor => &["agent", "cursor", "cursor-agent"],
            Self::Goose => &["goose"],
            Self::Droid => &["droid"],
            Self::Pi => &["pi"],
            Self::Auggie => &["auggie"],
            Self::Hermes => &["hermes", "hermes-agent"],
            Self::Vibe => &["vibe", "vibe-acp"],
            Self::Antigravity => &["agy", "antigravity", "antigravity-cli"],
            Self::Grok => &["grok", "grok-cli", "grok-build"],
            Self::Qwen => &["qwen", "qwen-code"],
            Self::OhMyPi => &["omp", "oh-my-pi"],
            Self::Cline => &["cline"],
            Self::Devin => &["devin", "devin-cli"],
            Self::Kimi => &["kimi", "kimi-code"],
            Self::Kiro => &["kiro", "kiro-cli"],
            Self::Kilo => &["kilo", "kilo-code"],
            Self::Qoder => &["qodercli", "qoderclicn", "qoder", "qodercn"],
            Self::Maki => &["maki"],
        }
    }

    /// Resolve an executable/program label to a canonical client identity.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw
            .trim()
            .trim_matches(['"', '\''])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(raw)
            .to_ascii_lowercase();
        let raw = [".exe", ".cmd", ".bat", ".ps1", ".com", ".js"]
            .into_iter()
            .find_map(|suffix| raw.strip_suffix(suffix))
            .unwrap_or(&raw);
        Self::ALL.into_iter().find(|agent| agent.aliases().contains(&raw))
    }

    /// Resolve an agent from a submitted shell command, including common
    /// interpreter and package-runner launch forms used inside WSL.
    pub fn parse_command(command: &str) -> Option<Self> {
        let mut tokens = command
            .split_whitespace()
            .map(|token| token.trim_matches(['"', '\'']))
            .filter(|token| !token.is_empty());
        let mut launcher = tokens.next()?;
        if launcher == "&" {
            launcher = tokens.next()?;
        }
        while is_env_assignment(launcher) {
            launcher = tokens.next()?;
        }

        if launcher_stem(launcher) == "env" {
            launcher = tokens
                .by_ref()
                .find(|token| !token.starts_with('-') && !is_env_assignment(token))?;
        }
        if let Some(agent) = Self::parse(launcher) {
            return Some(agent);
        }
        if !is_agent_interpreter(launcher) {
            return None;
        }

        tokens
            .filter(|token| !token.starts_with('-'))
            .find_map(|token| token.split(['/', '\\']).find_map(Self::parse))
    }

    /// Shell-safe resume command. Session ids are untrusted hook/file input,
    /// so an unsupported shape returns `None` instead of being quoted.
    pub fn resume_command(self, session_id: &str) -> Option<String> {
        valid_session_id(session_id)?;
        Some(match self {
            Self::Claude => format!("claude --resume {session_id}"),
            Self::Codex => format!("codex resume {session_id}"),
            Self::Gemini => format!("gemini --resume {session_id}"),
            Self::OpenCode => format!("opencode --session {session_id}"),
            Self::Amp => format!("amp threads continue {session_id}"),
            Self::Cursor => format!("agent --resume={session_id}"),
            Self::Copilot => format!("copilot --resume {session_id}"),
            Self::Grok => format!("grok --resume {session_id}"),
            Self::Pi => format!("pi --session {session_id}"),
            Self::OhMyPi => format!("omp --resume {session_id}"),
            Self::Aider
            | Self::Goose
            | Self::Droid
            | Self::Auggie
            | Self::Hermes
            | Self::Vibe
            | Self::Antigravity
            | Self::Qwen
            | Self::Cline
            | Self::Devin
            | Self::Kiro
            | Self::Kilo
            | Self::Qoder
            | Self::Maki => return None,
            Self::Kimi => format!("kimi --session {session_id}"),
        })
    }

    /// Cold-start commands whose interactive CLI spelling is verified against
    /// the provider's official CLI documentation. Unsupported clients must not
    /// be guessed from their detection slug.
    pub fn start_command(self) -> Option<String> {
        match self {
            Self::Claude => Some("claude".to_owned()),
            Self::Codex => Some("codex".to_owned()),
            Self::OpenCode => Some("opencode".to_owned()),
            Self::Cursor => Some("agent".to_owned()),
            Self::Pi => Some("pi".to_owned()),
            Self::OhMyPi => Some("omp".to_owned()),
            Self::Kimi => Some("kimi".to_owned()),
            _ => None,
        }
    }

    /// Shell-safe fork command for clients with a verified fork syntax.
    pub fn fork_command(self, session_id: &str) -> Option<String> {
        valid_session_id(session_id)?;
        Some(match self {
            Self::Claude => format!("claude --resume {session_id} --fork-session"),
            Self::Codex => format!("codex fork {session_id}"),
            Self::OpenCode => format!("opencode --session {session_id} --fork"),
            Self::Grok => format!("grok --resume {session_id} --fork-session"),
            Self::Pi => format!("pi --fork {session_id}"),
            Self::OhMyPi => format!("omp --fork {session_id}"),
            Self::Kimi => format!("kimi --fork {session_id}"),
            _ => return None,
        })
    }
}

fn valid_session_id(id: &str) -> Option<()> {
    (!id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    .then_some(())
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else { return false };
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn launcher_stem(token: &str) -> String {
    let basename = token.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(token);
    let lower = basename.to_ascii_lowercase();
    [".exe", ".cmd", ".bat", ".ps1", ".com"]
        .into_iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(str::to_owned))
        .unwrap_or(lower)
}

fn is_agent_interpreter(token: &str) -> bool {
    matches!(
        launcher_stem(token).as_str(),
        "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "npx"
            | "npm"
            | "pnpm"
            | "pnpx"
            | "yarn"
            | "bunx"
            | "python"
            | "python3"
            | "ruby"
            | "pipx"
            | "uv"
            | "uvx"
    )
}

/// Semantic state inferred from live application chrome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}

impl AgentStatus {
    /// 这个 pane 是否已经有权威判定。`Unknown` 表示 hook 与屏幕规则都没认领它
    /// （普通 shell，或还没识别出 agent）。
    ///
    /// BEL 的兜底资格看这一位。响铃的含义是「有事发生」，不是「停下来等你输
    /// 入」：CC / codex 在**回合结束**时同样响铃（终端自己的通知文案就写着
    /// 「任务完成，等待输入」，两个概念当初就被焊在一起）。一旦让响铃去表示等
    /// 待输入，agent 每做完一件事都会显示成「在问你」——所以有权威判定时响铃
    /// 不得改写它，只在没有判定时兜底。
    pub const fn is_decided(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Why the current pane status has its value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentStatusSource {
    Hook,
    Screen,
    Process,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub agent: AgentKind,
    pub status: AgentStatus,
    pub rule_id: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    identity: Vec<Gate>,
    rules: Vec<Rule>,
}

/// `_shared.toml`: rules merged into every manifest, with no agent identity.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct SharedManifest {
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    state: RuleState,
    #[serde(default)]
    priority: i32,
    #[serde(default = "whole_recent")]
    region: String,
    #[serde(flatten)]
    gate: Gate,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
struct Gate {
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
    #[serde(default)]
    all: Vec<Gate>,
    #[serde(default)]
    any: Vec<Gate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<Gate>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuleState {
    Idle,
    Working,
    Blocked,
}

impl From<RuleState> for AgentStatus {
    fn from(state: RuleState) -> Self {
        match state {
            RuleState::Idle => Self::Idle,
            RuleState::Working => Self::Working,
            RuleState::Blocked => Self::Blocked,
        }
    }
}

#[derive(Debug)]
struct CompiledManifest {
    manifest: Manifest,
    identity: Vec<CompiledGate>,
    rules: Vec<CompiledGate>,
    override_mtime: Option<SystemTime>,
}

#[derive(Debug)]
struct CompiledGate {
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
}

#[derive(Debug)]
struct Cache {
    manifests: HashMap<AgentKind, CompiledManifest>,
    last_override_scan: Instant,
}

/// Skeleton rules merged into every manifest below (and into user overrides).
const SHARED: &str = include_str!("agent_detection/_shared.toml");

const BUNDLED: &[(AgentKind, &str)] = &[
    (AgentKind::Claude, include_str!("agent_detection/claude.toml")),
    (AgentKind::Codex, include_str!("agent_detection/codex.toml")),
    (AgentKind::Gemini, include_str!("agent_detection/gemini.toml")),
    (AgentKind::Cursor, include_str!("agent_detection/cursor.toml")),
    (AgentKind::OpenCode, include_str!("agent_detection/opencode.toml")),
    (AgentKind::Copilot, include_str!("agent_detection/copilot.toml")),
    (AgentKind::Grok, include_str!("agent_detection/grok.toml")),
    (AgentKind::Pi, include_str!("agent_detection/pi.toml")),
    (AgentKind::Amp, include_str!("agent_detection/amp.toml")),
    (AgentKind::Antigravity, include_str!("agent_detection/antigravity.toml")),
    (AgentKind::Cline, include_str!("agent_detection/cline.toml")),
    (AgentKind::Devin, include_str!("agent_detection/devin.toml")),
    (AgentKind::Droid, include_str!("agent_detection/droid.toml")),
    (AgentKind::Hermes, include_str!("agent_detection/hermes.toml")),
    (AgentKind::Kimi, include_str!("agent_detection/kimi.toml")),
    (AgentKind::Kiro, include_str!("agent_detection/kiro.toml")),
    (AgentKind::Kilo, include_str!("agent_detection/kilo.toml")),
    (AgentKind::Qoder, include_str!("agent_detection/qodercli.toml")),
    (AgentKind::Maki, include_str!("agent_detection/maki.toml")),
];

static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();

fn cache() -> &'static RwLock<Cache> {
    CACHE.get_or_init(|| {
        RwLock::new(Cache { manifests: build_cache(), last_override_scan: Instant::now() })
    })
}

fn build_cache() -> HashMap<AgentKind, CompiledManifest> {
    BUNDLED
        .iter()
        .map(|(agent, source)| {
            let path = override_path(*agent);
            let disk_mtime = modified(&path);
            let bundled = compile_manifest(source, disk_mtime).unwrap_or_else(|error| {
                panic!("bundled {} agent rules are invalid: {error}", agent.slug())
            });
            let loaded = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| {
                    compile_manifest(&text, disk_mtime)
                        .map_err(|error| {
                            log::warn!("agent detection: ignored {}: {error}", path.display());
                        })
                        .ok()
                })
                .filter(|loaded| {
                    let matches = manifest_matches(&loaded.manifest, *agent);
                    if !matches {
                        log::warn!(
                            "agent detection: ignored {} because id {} does not match {}",
                            path.display(),
                            loaded.manifest.id,
                            agent.slug()
                        );
                    }
                    matches
                })
                .unwrap_or(bundled);
            (*agent, loaded)
        })
        .collect()
}

fn override_path(agent: AgentKind) -> PathBuf {
    settings_dir().join("agent-detection").join(format!("{}.toml", agent.slug()))
}

/// Same path contract as nebula-settings, repeated here because that crate is
/// optional in non-GPUI builds while agent semantics serve both shells.
fn settings_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("NEBULA_CONFIG_DIR").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Nebula")
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    path.metadata().and_then(|meta| meta.modified()).ok()
}

fn refresh_overrides_if_needed() {
    let should_scan = cache()
        .read()
        .map(|guard| guard.last_override_scan.elapsed() >= Duration::from_secs(2))
        .unwrap_or(true);
    if !should_scan {
        return;
    }
    let Ok(mut guard) = cache().write() else { return };
    if guard.last_override_scan.elapsed() < Duration::from_secs(2) {
        return;
    }
    guard.last_override_scan = Instant::now();
    let changed = BUNDLED.iter().any(|(agent, _)| {
        let path = override_path(*agent);
        let disk_mtime = modified(&path);
        guard.manifests.get(agent).is_none_or(|loaded| loaded.override_mtime != disk_mtime)
    });
    if changed {
        guard.manifests = build_cache();
    }
}

/// Match one live screen snapshot. No match is `None`: callers retain their
/// higher-confidence hook/process state rather than fabricating idle.
pub fn detect(program: &str, screen: &str) -> Option<Detection> {
    let agent = AgentKind::parse(program)?;
    refresh_overrides_if_needed();
    let guard = cache().read().ok()?;
    let loaded = guard.manifests.get(&agent)?;
    let mut best: Option<(&Rule, &CompiledGate)> = None;
    for (rule, gate) in loaded.manifest.rules.iter().zip(&loaded.rules) {
        let text = region(screen, &rule.region);
        if !gate.matches(text) {
            continue;
        }
        if best.is_none_or(|(previous, _)| rule.priority > previous.priority) {
            best = Some((rule, gate));
        }
    }
    best.map(|(rule, _)| Detection { agent, status: rule.state.into(), rule_id: rule.id.clone() })
}

/// Establish an agent identity from brand-specific terminal chrome.
///
/// This is deliberately separate from [`detect`]: generic status chrome such
/// as a prompt glyph or an interrupt hint can refine a known agent, but must
/// never invent one. The fallback matters most on Windows-hosted WSL panes,
/// where Toolhelp can see `wsl.exe` but not the Linux `codex` process behind it.
pub fn identify(screen: &str) -> Option<AgentKind> {
    refresh_overrides_if_needed();
    let guard = cache().read().ok()?;
    let mut found = None;
    for (agent, loaded) in &guard.manifests {
        if !loaded.identity.iter().any(|gate| gate.matches(screen)) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(*agent);
    }
    found
}

fn compile_manifest(
    source: &str,
    override_mtime: Option<SystemTime>,
) -> Result<CompiledManifest, String> {
    let mut manifest: Manifest = toml::from_str(source).map_err(|error| error.to_string())?;
    if manifest.rules.is_empty() || manifest.rules.len() > 64 {
        return Err("manifest must contain 1..=64 rules".to_owned());
    }
    // 共享骨架并入每一份 manifest——bundled 与用户 override 一视同仁，装了
    // 新 CLI 或改了本地规则都自动带上中断提示判据。理由见 _shared.toml 顶部：
    // per-agent 的 working 规则各写各的，同一个 AND 陷阱在多份文件里重复出现。
    manifest.rules.extend(shared_rules().iter().cloned());
    let rules = manifest
        .rules
        .iter()
        .map(|rule| compile_gate(&rule.gate))
        .collect::<Result<Vec<_>, _>>()?;
    let identity = manifest.identity.iter().map(compile_gate).collect::<Result<Vec<_>, _>>()?;
    Ok(CompiledManifest { manifest, identity, rules, override_mtime })
}

/// Rules every agent manifest inherits. Parsed once; see `_shared.toml`.
fn shared_rules() -> &'static Vec<Rule> {
    static SHARED_RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    SHARED_RULES.get_or_init(|| {
        let parsed: SharedManifest = toml::from_str(SHARED)
            .unwrap_or_else(|error| panic!("bundled shared agent rules are invalid: {error}"));
        parsed.rules
    })
}

fn compile_gate(gate: &Gate) -> Result<CompiledGate, String> {
    let compile = |patterns: &[String]| {
        patterns
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()
    };
    Ok(CompiledGate {
        contains: gate.contains.iter().map(|value| value.to_lowercase()).collect(),
        regex: compile(&gate.regex)?,
        line_regex: compile(&gate.line_regex)?,
        all: gate.all.iter().map(compile_gate).collect::<Result<_, _>>()?,
        any: gate.any.iter().map(compile_gate).collect::<Result<_, _>>()?,
        not_gate: gate.not_gate.iter().map(compile_gate).collect::<Result<_, _>>()?,
    })
}

impl CompiledGate {
    fn matches(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.contains.iter().all(|needle| lower.contains(needle))
            && self.regex.iter().all(|regex| regex.is_match(text))
            && self.line_regex.iter().all(|regex| text.lines().any(|line| regex.is_match(line)))
            && self.all.iter().all(|gate| gate.matches(text))
            && (self.any.is_empty() || self.any.iter().any(|gate| gate.matches(text)))
            && !self.not_gate.iter().any(|gate| gate.matches(text))
    }
}

fn manifest_matches(manifest: &Manifest, agent: AgentKind) -> bool {
    manifest.id == agent.slug()
        || manifest.aliases.iter().any(|alias| agent.aliases().contains(&alias.as_str()))
}

fn whole_recent() -> String {
    "whole_recent".to_owned()
}

fn region<'a>(screen: &'a str, spec: &str) -> &'a str {
    if spec == "whole_recent" {
        return screen;
    }
    if let Some(count) = region_count(spec, "bottom_non_empty_lines") {
        let lines: Vec<&str> = screen.lines().collect();
        let Some(start) = lines
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, line)| !line.trim().is_empty())
            .take(count)
            .last()
            .map(|(index, _)| index)
        else {
            return "";
        };
        return slice_from_line(screen, &lines, start);
    }
    if spec == "after_last_horizontal_rule" {
        let mut offset = 0;
        let mut last = 0;
        for line in screen.lines() {
            offset = (offset + line.len() + 1).min(screen.len());
            let trimmed = line.trim();
            if trimmed.chars().filter(|character| *character == '─').count() >= 3 {
                last = offset;
            }
        }
        return &screen[last..];
    }
    ""
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)?.strip_prefix('(')?.strip_suffix(')')?.parse().ok()
}

fn slice_from_line<'a>(screen: &'a str, lines: &[&str], index: usize) -> &'a str {
    let offset = lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(screen.len());
    &screen[offset..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_canonical_agents() {
        assert_eq!(AgentKind::parse(r"C:\tools\CLAUDE.EXE"), Some(AgentKind::Claude));
        assert_eq!(AgentKind::parse("cursor-agent"), Some(AgentKind::Cursor));
        assert_eq!(AgentKind::parse("grok-cli.cmd"), Some(AgentKind::Grok));
        assert_eq!(AgentKind::parse("cargo"), None);
    }

    #[test]
    fn submitted_commands_resolve_agents_across_wsl_launch_forms() {
        for agent in AgentKind::ALL {
            assert_eq!(AgentKind::parse_command(agent.slug()), Some(agent), "{}", agent.slug());
        }
        assert_eq!(
            AgentKind::parse_command("FOO=1 npx --yes @openai/codex --model o3"),
            Some(AgentKind::Codex)
        );
        assert_eq!(
            AgentKind::parse_command("env DEBUG=1 node /opt/@anthropic-ai/claude-code/cli.js"),
            Some(AgentKind::Claude)
        );
        assert_eq!(
            AgentKind::parse_command("npm exec @google/gemini-cli"),
            Some(AgentKind::Gemini)
        );
        assert_eq!(AgentKind::parse_command("uvx aider-chat"), Some(AgentKind::Aider));
        assert_eq!(AgentKind::parse_command("cat codex.md"), None);
        assert_eq!(AgentKind::parse_command("node server.js"), None);
    }

    #[test]
    fn commands_are_exact_and_injection_safe() {
        assert_eq!(AgentKind::Claude.start_command().as_deref(), Some("claude"));
        assert_eq!(AgentKind::Codex.start_command().as_deref(), Some("codex"));
        assert_eq!(AgentKind::OpenCode.start_command().as_deref(), Some("opencode"));
        assert_eq!(AgentKind::Cursor.start_command().as_deref(), Some("agent"));
        assert_eq!(AgentKind::Pi.start_command().as_deref(), Some("pi"));
        assert_eq!(AgentKind::OhMyPi.start_command().as_deref(), Some("omp"));
        assert_eq!(AgentKind::Kimi.start_command().as_deref(), Some("kimi"));
        assert_eq!(AgentKind::Gemini.start_command(), None);
        assert_eq!(
            AgentKind::Claude.resume_command("abc-123").as_deref(),
            Some("claude --resume abc-123")
        );
        assert_eq!(AgentKind::Codex.fork_command("abc-123").as_deref(), Some("codex fork abc-123"));
        assert_eq!(
            AgentKind::OpenCode.fork_command("abc-123").as_deref(),
            Some("opencode --session abc-123 --fork")
        );
        assert_eq!(
            AgentKind::Cursor.resume_command("abc-123").as_deref(),
            Some("agent --resume=abc-123")
        );
        assert_eq!(AgentKind::Pi.fork_command("abc-123").as_deref(), Some("pi --fork abc-123"));
        assert_eq!(
            AgentKind::OhMyPi.resume_command("abc-123").as_deref(),
            Some("omp --resume abc-123")
        );
        assert_eq!(
            AgentKind::Kimi.resume_command("abc-123").as_deref(),
            Some("kimi --session abc-123")
        );
        assert_eq!(AgentKind::Kimi.fork_command("abc-123").as_deref(), Some("kimi --fork abc-123"));
        assert_eq!(AgentKind::Claude.resume_command("x; calc"), None);
        assert_eq!(AgentKind::Aider.resume_command("abc"), None);
    }

    /// 响铃只能在没人认领这个 pane 时兜底。CC / codex 在回合结束时也响铃，所以
    /// 一旦让它压过 hook / 屏幕规则的判定，「完成」会在 tab 上显示成「在问你」
    /// ——这就是终端徽章那条「结束了却画手掌」的病根。
    #[test]
    fn bell_may_only_speak_for_panes_no_rule_has_claimed() {
        for decided in
            [AgentStatus::Idle, AgentStatus::Working, AgentStatus::Blocked, AgentStatus::Done]
        {
            assert!(decided.is_decided(), "{decided:?} 是权威判定，响铃不得改写");
        }
        assert!(!AgentStatus::Unknown.is_decided(), "没有判定时才轮到响铃兜底");
    }

    #[test]
    fn screen_rules_distinguish_working_blocked_and_idle() {
        let blocked =
            detect("claude", "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel").unwrap();
        assert_eq!(blocked.status, AgentStatus::Blocked);
        let working = detect("codex", "• Working (12s • esc to interrupt)").unwrap();
        assert_eq!(working.status, AgentStatus::Working);
        let idle = detect("claude", "────────────────\n❯ ").unwrap();
        assert_eq!(idle.status, AgentStatus::Idle);
    }

    #[test]
    fn branded_screen_chrome_identifies_codex_without_a_visible_host_process() {
        let screen = "OpenAI Codex (v0.42.0)\n\n› Ask Codex to do anything";
        assert_eq!(identify(screen), Some(AgentKind::Codex));
        assert_eq!(identify("› generic shell prompt"), None);
    }

    /// 下面四段都是 2026-08-22 用 runtime `pane.read` 从真实窗口抓的原文，
    /// 也就是判错的现场。规则改动必须对着它们回归，不能对着记忆里的格式写。
    #[test]
    fn real_claude_working_chrome_reads_working() {
        // 正在干活的 pane。spinner 行用 ✶（U+2736）而不是盲文点阵，且与底部
        // 中断提示相隔 6 行——旧规则要求「盲文行 AND 中断词」且 region 只有
        // 6 行，两个条件同时落空，回合进行中因此被判成 idle，再连续两拍降级
        // 成「完成」蓝点。
        let screen = "  ⎿  Running…\n\
                      ✶ Moseying… (8m 42s · ↓ 18.3k tokens)\n\
                      \x20 ⎿  Tip: You haven't used the ui-ux-pro-max plugin in a while.\n\
                      \x20    with /plugin\n\
                      ────────────────────────────────────────\n\
                      ❯ \n\
                      ────────────────────────────────────────\n\
                      \x20 ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents";
        let detection = detect("claude", screen).unwrap();
        assert_eq!(detection.status, AgentStatus::Working, "rule={}", detection.rule_id);
    }

    #[test]
    fn real_claude_idle_chrome_reads_idle() {
        // 回合已结束：没有中断提示，输入框空着。输入框可见本身不是判据——
        // 上面那段工作中的屏幕里 ❯ 同样在。
        let screen = "✻ Baked for 15m 24s\n\
                      ────────────────────────────────────────\n\
                      ❯ \n\
                      ────────────────────────────────────────\n\
                      \x20 ⏸ manual mode on · ? for shortcuts · ← for agents";
        let detection = detect("claude", screen).unwrap();
        assert_eq!(detection.status, AgentStatus::Idle, "rule={}", detection.rule_id);
    }

    #[test]
    fn real_claude_permission_form_reads_blocked() {
        // 真的停在权限框上。这段同时含「Esc to cancel」，共享 working 规则
        // 必须被它的 not 段拦住，否则「等你点头」会被当成「正在干活」。
        let screen = " List dist artifacts and query GitHub release assets\n\
                      \x20This command requires approval\n\
                      \x20Do you want to proceed?\n\
                      \x20❯ 1. Yes\n\
                      \x20  2. Yes, and don't ask again for: gh release *\n\
                      \x20  3. No\n\
                      \x20Esc to cancel · Tab to amend · ctrl+e to explain";
        let detection = detect("claude", screen).unwrap();
        assert_eq!(detection.status, AgentStatus::Blocked, "rule={}", detection.rule_id);
    }

    #[test]
    fn real_codex_idle_chrome_reads_idle() {
        // codex 早已答完，屏幕上就是空闲输入框。这块屏幕当初根本没人去匹:
        // running_program 是 None，1 Hz 看门狗在入口就早退了，于是转圈长挂。
        let screen = "  代码级修复和带 gpui-shell 的编译、针对性测试已经通过。\n\
                      › Improve documentation in @filename\n\
                      \x20 gpt-5.6-sol xhigh · D:\\temp_build\\nebula";
        let detection = detect("codex", screen).unwrap();
        assert_eq!(detection.status, AgentStatus::Idle, "rule={}", detection.rule_id);
    }

    #[test]
    fn shared_rules_light_up_agents_without_their_own_working_rule() {
        // 归一化的意义：中断提示对每个注册的 CLI 都点亮 working，新接的 CLI
        // 不必从零再写一遍 spinner 正则（cline 至今就没有 working 规则）。
        let screen = "some output\n────────\n> \n  esc to interrupt · ? for shortcuts";
        for agent in ["grok", "pi", "opencode", "gemini", "cline", "kilo"] {
            let found =
                detect(agent, screen).unwrap_or_else(|| panic!("{agent} produced no detection"));
            assert_eq!(found.status, AgentStatus::Working, "{agent} rule={}", found.rule_id);
        }
    }

    #[test]
    fn shared_blocked_rules_outrank_shared_working_on_confirmation_forms() {
        // 确认框里几乎总有中断词；共享层必须自己把这一对张力解开，不能指望
        // 每个 CLI 都记得写 blocked 规则。
        let screen = "Run rm -rf /tmp/x ?\n  Do you want to proceed?\n  1. Yes\n  2. No\n\
                      esc to cancel · enter to confirm";
        for agent in ["grok", "pi", "cline", "kilo"] {
            let found =
                detect(agent, screen).unwrap_or_else(|| panic!("{agent} produced no detection"));
            assert_eq!(found.status, AgentStatus::Blocked, "{agent} rule={}", found.rule_id);
        }
    }
}
