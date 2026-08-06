//! 发现本机的 AI CLI 历史会话（claude / codex），给命令面板的
//! 「恢复 AI 会话」列表供数据。
//!
//! # 为什么只读文件头部
//!
//! 会话 jsonl 动辄几十 MB，整读会把面板打开卡成秒级。而标题几乎总在最前：
//! claude 压缩过的会话第一行就是 `{"type":"summary",...}`，新会话则是首条
//! user 消息；codex 的 rollout 第一行是 session_meta（带 cwd）。所以这里
//! 每个文件最多读 [`HEAD_BYTES`]，而且只为**真正要显示**的前 N 条读。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 每个会话文件最多读多少字节找标题。
const HEAD_BYTES: usize = 64 * 1024;
/// 标题最长几个字符（超出截断加省略号）。
const TITLE_CHARS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiSessionSource {
    Claude,
    Codex,
}

impl AiSessionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiSession {
    pub source: AiSessionSource,
    /// resume 用的会话 id（claude：jsonl 文件名主干；codex：rollout 文件名
    /// 末尾的 uuid）。
    pub id: String,
    /// 列表主行。扫描阶段为空，[`scan`] 只为最终显示的那批读文件补齐。
    pub title: String,
    /// 副信息：claude 是项目目录，codex 是会话的 cwd。
    pub project: String,
    pub modified: SystemTime,
    path: PathBuf,
}

impl AiSession {
    /// 在终端里敲下去就能恢复这个会话的命令行。
    pub fn resume_command(&self) -> String {
        match self.source {
            AiSessionSource::Claude => format!("claude --resume {}", self.id),
            AiSessionSource::Codex => format!("codex resume {}", self.id),
        }
    }

    /// hint 里的位置词。codex 有真实 cwd，取末段目录名；claude 只有编码过的
    /// 项目目录名（`D--temp-build-nebula`，分隔符和 `_` 都压成了 `-`，不可逆），
    /// 取最后一个 `-` 后的尾段——不完整但足够认出"是哪个项目"。
    pub fn place_label(&self) -> String {
        match self.source {
            AiSessionSource::Codex => Path::new(&self.project)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            AiSessionSource::Claude => {
                self.project.rsplit('-').next().unwrap_or("").to_owned()
            },
        }
    }

    fn fill_details(&mut self) {
        let Ok(head) = read_head(&self.path, HEAD_BYTES) else { return };
        match self.source {
            AiSessionSource::Claude => {
                if let Some(title) = claude_title(&head) {
                    self.title = title;
                }
            },
            AiSessionSource::Codex => {
                let (title, cwd) = codex_details(&head);
                if let Some(title) = title {
                    self.title = title;
                }
                if let Some(cwd) = cwd {
                    self.project = cwd;
                }
            },
        }
        if self.title.is_empty() {
            // 兜底：读不出标题也得给人一行能认的字，id 前八位是 claude
            // `--resume` 补全里露出的同一串，两边能对上。
            let short = self.id.chars().take(8).collect::<String>();
            self.title = format!("{} 会话 {short}", self.source.label());
        }
    }
}

/// 扫描两家的会话目录，按最近活动排序，取前 `limit` 条并补齐标题。
pub fn scan(limit: usize) -> Vec<AiSession> {
    let mut all = Vec::new();
    all.extend(scan_claude());
    all.extend(scan_codex());
    all.sort_by(|a, b| b.modified.cmp(&a.modified));
    all.truncate(limit);
    for session in &mut all {
        session.fill_details();
    }
    all
}

/// 「3 分钟前 / 2 小时前 / 5 天前」式的相对时间，面板右侧的 hint 用。
pub fn relative_label(modified: SystemTime) -> String {
    let elapsed = SystemTime::now().duration_since(modified).unwrap_or(Duration::ZERO);
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "刚刚".to_owned(),
        1..=59 => format!("{minutes} 分钟前"),
        60..=1439 => format!("{} 小时前", minutes / 60),
        _ => format!("{} 天前", minutes / 1440),
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn modified_of(path: &Path) -> SystemTime {
    path.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `~/.claude/projects/<编码目录>/<uuid>.jsonl`，一个文件一个会话。
fn scan_claude() -> Vec<AiSession> {
    let Some(root) = home_dir().map(|h| h.join(".claude").join("projects")) else {
        return Vec::new();
    };
    let Ok(projects) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for project in projects.flatten() {
        let project_label = project.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(project.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            out.push(AiSession {
                source: AiSessionSource::Claude,
                id,
                title: String::new(),
                project: project_label.clone(),
                modified: modified_of(&path),
                path,
            });
        }
    }
    out
}

/// `~/.codex/sessions/YYYY/MM/DD/rollout-<时间戳>-<uuid>.jsonl`。
fn scan_codex() -> Vec<AiSession> {
    let Some(root) = home_dir().map(|h| h.join(".codex").join("sessions")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // 目录按年/月/日三层嵌套；有限深度的手写递归比引一个 walkdir 轻。
    let mut stack = vec![(root, 0u8)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 4 {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Some(id) = codex_session_id(&path) else { continue };
            out.push(AiSession {
                source: AiSessionSource::Codex,
                id,
                title: String::new(),
                project: String::new(),
                modified: modified_of(&path),
                path,
            });
        }
    }
    out
}

/// rollout 文件名末尾是会话 uuid：`rollout-2026-08-01T12-30-00-<uuid>.jsonl`。
/// 按「最后 36 个字符 + 连字符位置」验证，不然时间戳里的数字段会被当成 id。
fn codex_session_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() < 36 {
        return None;
    }
    let (_, tail) = stem.split_at(stem.len() - 36);
    let dashes_ok = tail.match_indices('-').map(|(i, _)| i).eq([8, 13, 18, 23]);
    let charset_ok = tail.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    (dashes_ok && charset_ok).then(|| tail.to_owned())
}

fn read_head(path: &Path, limit: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; limit];
    let mut filled = 0;
    // read 不保证填满：读到 EOF 或缓冲满为止。
    loop {
        let n = file.read(&mut buffer[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
        if filled == buffer.len() {
            break;
        }
    }
    buffer.truncate(filled);
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn truncate_title(text: &str) -> String {
    let text = text.trim().replace(['\r', '\n'], " ");
    let mut out: String = text.chars().take(TITLE_CHARS).collect();
    if text.chars().count() > TITLE_CHARS {
        out.push('…');
    }
    out
}

/// CLI 自动注入的指令/上下文块——JSONL 里挂在 user 名下，看着像"第一条用户
/// 消息"，实际没有一个字是用户敲的。拿它们当标题，列表里就是一排
/// 「# AGENTS.md instructions <INSTRUCTIONS>…」（用户 08-02 截图的现场）。
fn looks_injected(text: &str) -> bool {
    text.starts_with('<')
        || text.starts_with("Caveat:")
        || text.starts_with("# AGENTS.md")
        || text.starts_with("# CLAUDE.md")
        || text.starts_with("[Request interrupted")
}

/// claude 会话的标题：优先 `summary` 行（压缩过的会话第一行就是），否则
/// 首条 user 消息的文本。头部截断出的半行 JSON 解析失败即跳过，天然安全。
fn claude_title(head: &str) -> Option<String> {
    for line in head.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("summary") => {
                if let Some(summary) = value.get("summary").and_then(|s| s.as_str()) {
                    if !summary.trim().is_empty() {
                        return Some(truncate_title(summary));
                    }
                }
            },
            Some("user") => {
                let content = value.pointer("/message/content");
                let text = match content {
                    Some(serde_json::Value::String(text)) => Some(text.clone()),
                    Some(serde_json::Value::Array(parts)) => parts.iter().find_map(|part| {
                        (part.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .then(|| part.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                            .flatten()
                    }),
                    _ => None,
                };
                if let Some(text) = text {
                    let text = text.trim();
                    // isMeta 是 CLI 打在注入行上的标记（/clear 回显、上下文
                    // 快照等）——内容再像人话也不是用户说的；没打标记的再过
                    // 一遍 `looks_injected` 的形态筛。
                    let meta = value.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false);
                    if !meta && !text.is_empty() && !looks_injected(text) {
                        return Some(truncate_title(text));
                    }
                }
            },
            _ => {},
        }
    }
    None
}

/// codex rollout 的 (标题, cwd)：session_meta 行给 cwd，首条 user 输入给标题。
fn codex_details(head: &str) -> (Option<String>, Option<String>) {
    let mut title = None;
    let mut cwd = None;
    for line in head.lines() {
        if title.is_some() && cwd.is_some() {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(payload) = value.get("payload") else { continue };
        if cwd.is_none() {
            if let Some(dir) = payload.get("cwd").and_then(|c| c.as_str()) {
                cwd = Some(dir.to_owned());
            }
        }
        if title.is_none()
            && payload.get("role").and_then(|r| r.as_str()) == Some("user")
            && let Some(parts) = payload.get("content").and_then(|c| c.as_array())
        {
            let text = parts.iter().find_map(|part| {
                (part.get("type").and_then(|t| t.as_str()) == Some("input_text"))
                    .then(|| part.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                    .flatten()
            });
            if let Some(text) = text {
                let text = text.trim();
                if !text.is_empty() && !looks_injected(text) {
                    title = Some(truncate_title(text));
                }
            }
        }
    }
    (title, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compacted_claude_session_titles_from_its_summary_line() {
        let head = r#"{"type":"summary","summary":"修复 SSH 编辑器身份条","leafUuid":"x"}
{"type":"user","message":{"content":"继续"}}"#;
        assert_eq!(claude_title(head), Some("修复 SSH 编辑器身份条".to_owned()));
    }

    #[test]
    fn a_fresh_claude_session_titles_from_the_first_user_message() {
        let head = r#"{"type":"file-history-snapshot","messageId":"m1"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"帮我查一下这个 panic 的根因"}]}}"#;
        assert_eq!(claude_title(head), Some("帮我查一下这个 panic 的根因".to_owned()));
    }

    #[test]
    fn injected_system_blocks_never_become_titles() {
        // 首条 user 是 <system-reminder> 注入时要跳过它取下一条真人消息。
        let head = r#"{"type":"user","message":{"content":"<system-reminder>ctx</system-reminder>"}}
{"type":"user","message":{"content":"真正的问题"}}"#;
        assert_eq!(claude_title(head), Some("真正的问题".to_owned()));
    }

    #[test]
    fn injected_instructions_and_meta_lines_never_become_titles() {
        // 用户 08-02 截图：列表里一排「# AGENTS.md instructions…」。这类
        // 注入块（还有打了 isMeta 的 /clear 回显）都挂在 user 名下，必须
        // 跳过去取后面第一条真人消息。
        let head = r##"{"type":"user","message":{"content":[{"type":"text","text":"# AGENTS.md instructions <INSTRUCTIONS> 【基础环境与规范】"}]}}
{"type":"user","isMeta":true,"message":{"content":"看着像人话的注入行"}}
{"type":"user","message":{"content":"Caveat: The messages below were generated..."}}
{"type":"user","message":{"content":"真正的第一句"}}"##;
        assert_eq!(claude_title(head), Some("真正的第一句".to_owned()));
    }

    #[test]
    fn codex_agents_md_injection_is_skipped() {
        let head = r##"{"timestamp":"t","type":"session_meta","payload":{"id":"abc","cwd":"D:\\proj"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\n【基础环境与规范】"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"帮我看个 bug"}]}}"##;
        let (title, cwd) = codex_details(head);
        assert_eq!(title, Some("帮我看个 bug".to_owned()));
        assert_eq!(cwd, Some(r"D:\proj".to_owned()));
    }

    #[test]
    fn place_labels_shorten_to_something_recognizable() {
        let codex = AiSession {
            source: AiSessionSource::Codex,
            id: "x".into(),
            title: String::new(),
            project: r"D:\temp_build\nebula".into(),
            modified: SystemTime::UNIX_EPOCH,
            path: PathBuf::new(),
        };
        assert_eq!(codex.place_label(), "nebula");
        let claude = AiSession {
            source: AiSessionSource::Claude,
            project: "D--temp-build-nebula".into(),
            ..codex
        };
        assert_eq!(claude.place_label(), "nebula");
    }

    #[test]
    fn a_truncated_trailing_line_is_ignored_not_fatal() {
        let head = "{\"type\":\"summary\",\"summary\":\"标题\"}\n{\"type\":\"user\",\"mess";
        assert_eq!(claude_title(head), Some("标题".to_owned()));
    }

    #[test]
    fn codex_rollout_yields_cwd_and_first_user_text() {
        let head = r#"{"timestamp":"t","type":"session_meta","payload":{"id":"abc","cwd":"D:\\proj"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"打包发布"}]}}"#;
        let (title, cwd) = codex_details(head);
        assert_eq!(title, Some("打包发布".to_owned()));
        assert_eq!(cwd, Some(r"D:\proj".to_owned()));
    }

    #[test]
    fn codex_ids_come_from_the_uuid_tail_only() {
        let path =
            Path::new("rollout-2026-08-01T12-30-00-0199a213-c2a4-7cf5-8f6b-d746fbb6e86c.jsonl");
        assert_eq!(codex_session_id(path), Some("0199a213-c2a4-7cf5-8f6b-d746fbb6e86c".to_owned()));
        // 名字不带 uuid 的不是会话文件。
        assert_eq!(codex_session_id(Path::new("rollout-notes.jsonl")), None);
    }

    #[test]
    fn resume_commands_match_each_clis_syntax() {
        let claude = AiSession {
            source: AiSessionSource::Claude,
            id: "abc-123".into(),
            title: String::new(),
            project: String::new(),
            modified: SystemTime::UNIX_EPOCH,
            path: PathBuf::new(),
        };
        assert_eq!(claude.resume_command(), "claude --resume abc-123");
        let codex = AiSession { source: AiSessionSource::Codex, ..claude };
        assert_eq!(codex.resume_command(), "codex resume abc-123");
    }

    #[test]
    fn titles_collapse_newlines_and_cap_length() {
        let long = "第一行\n第二行".to_owned() + &"字".repeat(80);
        let title = truncate_title(&long);
        assert!(title.starts_with("第一行 第二行"));
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= TITLE_CHARS + 1);
    }
}
