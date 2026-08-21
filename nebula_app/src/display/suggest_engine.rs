//! 补全/建议引擎：ghost 余量与弹窗候选的计算核心。
//!
//! 从 `Display` 的方法下沉为自由函数：winit 壳把 `Display` 字段借成
//! [`SuggestSources`]，GPUI 壳借进程级单例（`gpui_shell::terminal::suggest`）。
//! 数据源与排序规则两个壳共用，避免第二套平行实现（与 `ssh_session` 的
//! `SshEventHost` 泛型下沉同一手法）。

use nebula_completions::file::complete_item;
use nebula_completions::{CompletionOptions, Span};

use super::state::{CompletionStyle, NebulaCompletionItem, NebulaCompletionKind, NebulaPaneState};
use super::{
    NEBULA_GHOST_MAX, nebula_command_hint, nebula_command_hints, nebula_debug_log,
    nebula_is_command_position, nebula_path_wants_directory,
};

/// 借用的共享数据源与运行时开关。生命周期只覆盖一次重算调用。
pub(crate) struct SuggestSources<'a> {
    pub history: &'a crate::nebula_history::NebulaHistory,
    pub directories: &'a crate::directory_history::DirectoryHistory,
    pub commands: &'a std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pub enabled: bool,
    pub style: CompletionStyle,
}

/// WSL / SSH pane 在命令位置能补的东西。
///
/// 本进程的 PATH 描述的是 Windows，里面一个 `notepad.exe` 在来宾或远端都不存在，
/// 拿它去补 tab 只会给出跑不起来的命令。真实的远端 PATH 要一次往返才问得到
/// （见 [`SuggestEnv`] 的文档），在那之前这张表是诚实的兜底：全是 POSIX shell
/// 内置与几乎每台 Linux 都装了的工具。
///
/// 排序无所谓——[`nebula_command_hints`] 会自己按长度和字典序收敛。
const POSIX_COMMANDS: &[&str] = &[
    // shell 内置
    "alias",
    "bg",
    "cd",
    "declare",
    "echo",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fg",
    "history",
    "jobs",
    "kill",
    "local",
    "printf",
    "pwd",
    "read",
    "return",
    "set",
    "shift",
    "source",
    "test",
    "trap",
    "true",
    "type",
    "umask",
    "unalias",
    "unset",
    "wait",
    "which",
    // coreutils 与文件操作
    "basename",
    "cat",
    "chgrp",
    "chmod",
    "chown",
    "cp",
    "cut",
    "df",
    "dirname",
    "du",
    "find",
    "grep",
    "head",
    "less",
    "ln",
    "ls",
    "mkdir",
    "more",
    "mv",
    "readlink",
    "realpath",
    "rm",
    "rmdir",
    "sed",
    "sort",
    "stat",
    "tail",
    "tee",
    "touch",
    "tr",
    "uniq",
    "wc",
    "xargs",
    // 进程 / 系统
    "chsh",
    "env",
    "free",
    "htop",
    "id",
    "journalctl",
    "man",
    "mount",
    "nohup",
    "ps",
    "service",
    "su",
    "sudo",
    "systemctl",
    "top",
    "uname",
    "uptime",
    "whoami",
    // 网络与传输
    "curl",
    "dig",
    "ip",
    "netstat",
    "ping",
    "rsync",
    "scp",
    "ss",
    "ssh",
    "wget",
    // 归档
    "gunzip",
    "gzip",
    "tar",
    "unzip",
    "xz",
    "zip",
    // 包管理
    "apt",
    "apt-get",
    "dnf",
    "dpkg",
    "pacman",
    "snap",
    "yum",
    // 开发
    "awk",
    "cargo",
    "cmake",
    "docker",
    "g++",
    "gcc",
    "gdb",
    "git",
    "go",
    "make",
    "nano",
    "node",
    "npm",
    "pip",
    "pip3",
    "pnpm",
    "python",
    "python3",
    "rustc",
    "rustup",
    "vim",
    "yarn",
];

/// [`POSIX_COMMANDS`] 的 `Vec<String>` 视图。
///
/// `nebula_command_hint`/`_hints` 吃的是 `&[String]`（本机那份来自 PATH 探针）。
/// 转换只做一次；等远端 PATH 真的探到了，替换的就是这个容器本身。
fn posix_commands() -> &'static [String] {
    static CACHE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| POSIX_COMMANDS.iter().map(|name| (*name).to_owned()).collect())
}

/// 补齐面对的是**哪台机器**的文件系统与命令集。
///
/// 这是补齐正确性的根，不只是个功能开关。同一条 `ls /te<Tab>`：本地 tab 该查
/// `D:\te*`，WSL tab 该查来宾的 `/te*`，SSH tab 该问远端。走错机器不是"补不
/// 出来"这么轻——它会**补出另一台机器上的路径**，而那条路径在当前 shell 里
/// 根本不存在。
///
/// 这不是假想的失败模式。宿主把 `/temp_build` 解析成"当前盘符下的
/// `\temp_build`"，而这台机器的 D 盘恰好就有 `D:\temp_build`，于是
/// [`crate::directory_history::DirectoryHistory::hint`] 的 `is_dir` 把关会当成
/// 命中，把宿主的目录补进 WSL 的命令行里。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SuggestEnv {
    /// 普通本地 tab：`std::fs` 与本进程 PATH 指向同一台机器。
    #[default]
    Local,
    /// WSL tab：cwd 是来宾的 Linux 绝对路径。宿主的 `std::fs` 读不到它——本机
    /// 的 9P 重定向不可用，`\\wsl.localhost\` 三条路实测全部失败，见
    /// [`crate::shell_detect::wsl_unc_cwd`] 的记录。要列来宾目录只能
    /// `wsl.exe -d <发行版> -- find`，那是子进程，冷启动可达数秒，绝不能挂在
    /// 按键路径上。
    Wsl { distro: String },
    /// SSH tab：文件系统只能经 SFTP 往返，命令集是远端的。`destination` 进
    /// 缓存键——同一条 `/home/kud` 在两台远端上是两个不同的目录。
    Ssh { destination: String },
}

impl SuggestEnv {
    /// 本进程的 `std::fs` 与 PATH 是否描述这个 pane 所在的机器。
    pub fn is_this_machine(&self) -> bool {
        matches!(self, Self::Local)
    }
}

/// Refresh the inline ghost remainder or the popup candidate list for the
/// current prompt line. Cached on `cwd + line`; dismissal keeps the key so a
/// closed popup stays closed until the line itself changes.
pub(crate) fn suggest_update(
    sources: &SuggestSources<'_>,
    state: &mut NebulaPaneState,
    line_override: Option<String>,
) {
    let line = line_override.unwrap_or_else(|| state.line_buf.clone());
    if !sources.enabled || line.is_empty() {
        state.clear_completion_hints();
        nebula_debug_log(format!(
            "suggest_skip enabled={} cwd={:?} line={:?} line_buf={:?}",
            sources.enabled, state.cwd, line, state.line_buf
        ));
        return;
    }

    // 命令目录由后台 PowerShell 探针填充；目录从 0 变为完整集合时，即使
    // 用户没有继续输入，也必须让上一帧的“无候选”缓存失效。
    let command_generation = sources.commands.lock().map(|commands| commands.len()).unwrap_or(0);
    // 远端目录是异步拉回来的，那一刻 cwd 与行都没变——少了这个代际，拉到的
    // 条目要等用户再多打一个字符才会显形。
    let remote_generation = crate::remote_dirs::generation();
    let key =
        format!("{}\u{0}{line}\u{0}{command_generation}\u{0}{remote_generation}", state.cwd);
    if state.completion_suppressed_line.as_deref() == Some(line.as_str()) {
        state.suggestion_key = key;
        state.suggestion.clear();
        state.completion_items.clear();
        state.completion_selected = None;
        return;
    }
    state.completion_suppressed_line = None;
    if key == state.suggestion_key {
        // Cache hit also protects an Esc-dismissed popup: dismissal clears
        // the items but keeps the key, so nothing reopens until the line
        // actually changes.
        return;
    }
    state.suggestion_key = key;
    state.suggestion.clear();
    state.completion_items.clear();
    state.completion_selected = None;

    nebula_debug_log(format!(
        "suggest_begin cwd={:?} line={:?} line_buf={:?}",
        state.cwd, line, state.line_buf
    ));

    // Popup style computes a multi-candidate list instead of the single
    // ghost remainder; the two are mutually exclusive per pane.
    if sources.style == CompletionStyle::Popup {
        suggest_collect(sources, state, &line);
        return;
    }

    // History first: newest command that extends the whole line (indexed
    // prefix lookup — scales with matches, not history size).
    if let Some(rem) = sources.history.hint(&line) {
        state.suggestion = clamp_ghost(rem);
        nebula_debug_log(format!("suggest_result kind=history rem={:?}", state.suggestion));
        return;
    }

    // Directory-history hint for cd-like commands. This normalizes Windows
    // slash style/case, so `cd D:\te` can pick a previous
    // `cd D:/temp_build/wuwei` before generic filesystem completion falls
    // back to alphabetic candidates like `D:\Telegram\`.
    //
    // 只在本机：frecency 池里混着三台机器访问过的路径，条目自身没有环境标签，
    // 而 `hint` 是拿**宿主的** `is_dir` 把关的——在 WSL tab 里 `/temp_build`
    // 会被解析成 `D:\temp_build` 并当成命中（见 [`SuggestEnv`]）。
    if state.suggest_env.is_this_machine() {
        if let Some(rem) = sources.directories.hint(&line, &state.cwd) {
            state.suggestion = clamp_ghost(&rem);
            nebula_debug_log(format!("suggest_result kind=dir rem={:?}", state.suggestion));
            return;
        }
    }

    // First token with no path separators is a command position. Reuse the
    // process PATH inherited by the shell so typing `ca` can ghost `rgo`
    // even before that command has appeared in Nebula/Nushell history.
    // WSL/SSH 的 shell 没继承这个 PATH，那边走 [`POSIX_COMMANDS`]。
    if nebula_is_command_position(&line) {
        let hinted = if state.suggest_env.is_this_machine() {
            sources.commands.lock().ok().and_then(|commands| {
                nebula_command_hint(commands.as_slice(), &line).map(str::to_owned)
            })
        } else {
            nebula_command_hint(posix_commands(), &line).map(str::to_owned)
        };
        if let Some(rem) = hinted {
            state.suggestion = clamp_ghost(&rem);
            nebula_debug_log(format!("suggest_result kind=command rem={:?}", state.suggestion));
            return;
        }
    }

    // Otherwise complete the final path token. Absolute tokens (drive,
    // root or `~`) resolve without a cwd; relative ones need the tracked
    // cwd, so bail if it is unknown.
    let token = line.rsplit([' ', '\t']).next().unwrap_or("");
    if token.is_empty() {
        return;
    }
    // 非本机的路径补齐走各自的通道（来宾 `find` / SFTP），都带往返，所以只
    // 读缓存：miss 时把目录登记给壳去异步拉（见 [`remote_path_matches`]）。
    // 这里**必须**分流而不是"顺手用 `std::fs` 试试"：宿主会把 `/te` 解析成
    // 当前盘的 `\te`，补出来的是 `D:\temp_build` 这种在当前 shell 里根本不
    // 存在的路径——比补不出来更糟。
    if !state.suggest_env.is_this_machine() {
        let remainder = remote_path_matches(state, &line, token).into_iter().next();
        if let Some((rem, _)) = remainder {
            state.suggestion = clamp_ghost(&rem);
            nebula_debug_log(format!("suggest_result kind=remote_path rem={:?}", state.suggestion));
        } else {
            nebula_debug_log(format!(
                "suggest_result kind=none env={:?} token={:?} pending={:?}",
                state.suggest_env, token, state.pending_remote_dir
            ));
        }
        return;
    }
    let absolute = token.starts_with(['/', '\\', '~']) || token.as_bytes().get(1) == Some(&b':'); // Windows drive, e.g. `D:`
    if !absolute && state.cwd.is_empty() {
        return;
    }

    // Case-insensitive so `mor` completes `MoRealm` on Windows; prefer
    // directories for the common directory-changing commands.
    let options = CompletionOptions { case_sensitive: false, ..CompletionOptions::default() };
    let want_dir = nebula_path_wants_directory(&line);
    let span = Span::new(0, token.len());
    let cwd = state.cwd.clone();
    let cwd_slot = [cwd.as_str()];
    let cwds: &[&str] = if cwd.is_empty() { &[] } else { &cwd_slot };
    let matches = complete_item(want_dir, span, token, cwds, &options, false, None);
    let matches = if want_dir {
        sources.directories.rank_file_suggestions(matches, &state.cwd)
    } else {
        matches
    };
    let candidates: Vec<_> = matches
        .iter()
        .take(6)
        .map(|s| s.display_override.as_deref().unwrap_or(&s.path).to_owned())
        .collect();
    let remainder = matches.into_iter().find_map(|s| {
        let path = s.display_override.as_deref().unwrap_or(&s.path);
        // The match was case-insensitive, so the suggestion is the slice of
        // `path` past what the user typed. Compare the head ignoring ASCII
        // case (so `mor` matches `MoRealm`) and guard the byte split against
        // multibyte boundaries.
        if path.len() <= token.len() || !path.is_char_boundary(token.len()) {
            return None;
        }
        let (head, rem) = path.split_at(token.len());
        if !head.eq_ignore_ascii_case(token) {
            return None;
        }
        // Stop at the first separator so a single deep match doesn't drill
        // the whole tree into the ghost; suggest one segment.
        Some(match rem.find(['/', '\\']) {
            Some(i) => rem[..=i].to_owned(),
            None => rem.to_owned(),
        })
    });
    if let Some(rem) = remainder {
        state.suggestion = clamp_ghost(&rem);
        nebula_debug_log(format!(
            "suggest_result kind=path token={:?} candidates={:?} rem={:?}",
            token, candidates, state.suggestion
        ));
    } else {
        nebula_debug_log(format!(
            "suggest_result kind=none token={:?} candidates={:?}",
            token, candidates
        ));
    }
}

/// Cap ghost length so a long path/command can't spill into the chrome.
fn clamp_ghost(rem: &str) -> String {
    rem.chars().take(NEBULA_GHOST_MAX).collect()
}

/// 来宾 / 远端目录里匹配当前 token 的候选，`(插入余量, 是否目录)`。
///
/// 只读 [`crate::remote_dirs`] 的缓存——列一个来宾目录要一次子进程往返（冷
/// 启动实测可达 7.5 秒），挂在按键路径上整个 UI 都会卡住。缓存没有时把目录
/// 登记到 `state.pending_remote_dir`，壳看到就去异步拉，回填后代际一变，下
/// 一次重算就有候选了。
///
/// 命令位置（`gre<Tab>`）不问目录：那是在补命令名，为它跑一趟 SSH 往返纯属
/// 浪费。带了 `/` 就不同了——`./scr` 或 `/usr/bin/l` 明确是在打路径。
fn remote_path_matches(
    state: &mut NebulaPaneState,
    line: &str,
    token: &str,
) -> Vec<(String, bool)> {
    if nebula_is_command_position(line) && !token.contains('/') {
        return Vec::new();
    }
    let Some(request) = crate::remote_dirs::path_request(token, &state.cwd) else {
        return Vec::new();
    };
    match crate::remote_dirs::lookup(&state.suggest_env, &request.dir) {
        Some(entries) => crate::remote_dirs::candidates(&request, &entries),
        None => {
            state.pending_remote_dir = Some(request.dir);
            Vec::new()
        },
    }
}

/// Elide long popup labels from the LEFT (paths keep their informative
/// tail; the head the user already typed is the expendable part).
pub(crate) fn elide_left(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count + 1 - max_chars).collect();
    format!("…{tail}")
}

/// Fill `state.completion_items` for the popup style: the same sources as
/// the ghost hint (history → directory history → PATH commands → file
/// system), but keeping several candidates each instead of the first hit.
fn suggest_collect(sources: &SuggestSources<'_>, state: &mut NebulaPaneState, line: &str) {
    // 8 是视口行数，不是数据上限。旧实现把两者混成一个常量，候选在收集
    // 阶段就被截断，因而既画不出滚动条，Tab 也永远走不到第九项以后。
    const POPUP_LIMIT: usize = 256;
    const LABEL_MAX: usize = 44;

    let mut items: Vec<NebulaCompletionItem> = Vec::new();
    let push = |items: &mut Vec<NebulaCompletionItem>, item: NebulaCompletionItem| {
        if item.insert.is_empty() {
            return;
        }
        if items.iter().any(|seen| seen.insert == item.insert && seen.kind == item.kind) {
            return;
        }
        if items.len() < POPUP_LIMIT {
            items.push(item);
        }
    };

    // Whole-line history matches, newest first.
    for (full, rem) in sources.history.hints(line, 3) {
        push(
            &mut items,
            NebulaCompletionItem {
                label: elide_left(full, LABEL_MAX),
                insert: rem.to_owned(),
                kind: NebulaCompletionKind::History,
            },
        );
    }

    let token = line.rsplit([' ', '\t']).next().unwrap_or("");

    // Frecency-ranked directory completion for cd-like commands. 只在本机，
    // 理由同 ghost 分支：frecency 池没有环境标签，而 `hint` 拿宿主的 `is_dir`
    // 把关，会把 `D:\temp_build` 当成 WSL 的 `/temp_build`。
    if state.suggest_env.is_this_machine() {
        if let Some(rem) = sources.directories.hint(line, &state.cwd) {
            push(
                &mut items,
                NebulaCompletionItem {
                    label: elide_left(&format!("{token}{rem}"), LABEL_MAX),
                    insert: rem,
                    kind: NebulaCompletionKind::Dir,
                },
            );
        }
    }

    // PATH executables while the first token is being typed. 本机用 shell 继承
    // 的进程 PATH；WSL/SSH 的 shell 没继承它，走 [`POSIX_COMMANDS`]。
    if nebula_is_command_position(line) {
        let local = state.suggest_env.is_this_machine();
        let guard = local.then(|| sources.commands.lock().ok()).flatten();
        let commands: &[String] = match guard.as_deref() {
            Some(commands) => commands,
            None if local => &[],
            None => posix_commands(),
        };
        for command in nebula_command_hints(commands, line, POPUP_LIMIT) {
            // 精确命令也必须成为可接受项；补一个空格既给用户明确反馈，
            // 又让 Enter 只完成选择而不立刻执行命令。
            let insert = if command.len() == line.len() {
                " ".to_owned()
            } else {
                command[line.len()..].to_owned()
            };
            push(
                &mut items,
                NebulaCompletionItem {
                    label: elide_left(command, LABEL_MAX),
                    insert,
                    kind: NebulaCompletionKind::Command,
                },
            );
        }
    }

    // Filesystem candidates for the final token (same gating as the ghost
    // path: absolute tokens work without a cwd, relative ones need one).
    // 非本机走 [`remote_path_matches`]：同一份缓存、同一套分流，只是这里保留
    // 多个候选而不是第一个。
    if !token.is_empty() && !state.suggest_env.is_this_machine() {
        for (rem, is_dir) in remote_path_matches(state, line, token).into_iter().take(POPUP_LIMIT) {
            push(
                &mut items,
                NebulaCompletionItem {
                    label: elide_left(&format!("{token}{rem}"), LABEL_MAX),
                    insert: rem,
                    kind: if is_dir {
                        NebulaCompletionKind::Dir
                    } else {
                        NebulaCompletionKind::File
                    },
                },
            );
        }
    } else if !token.is_empty() && state.suggest_env.is_this_machine() {
        let absolute =
            token.starts_with(['/', '\\', '~']) || token.as_bytes().get(1) == Some(&b':');
        if absolute || !state.cwd.is_empty() {
            let options =
                CompletionOptions { case_sensitive: false, ..CompletionOptions::default() };
            let want_dir = nebula_path_wants_directory(line);
            let span = Span::new(0, token.len());
            let cwd = state.cwd.clone();
            let cwd_slot = [cwd.as_str()];
            let cwds: &[&str] = if cwd.is_empty() { &[] } else { &cwd_slot };
            let matches = complete_item(want_dir, span, token, cwds, &options, false, None);
            let matches = if want_dir {
                sources.directories.rank_file_suggestions(matches, &state.cwd)
            } else {
                matches
            };
            for candidate in matches.iter().take(POPUP_LIMIT) {
                let path = candidate.display_override.as_deref().unwrap_or(&candidate.path);
                if path.len() <= token.len() || !path.is_char_boundary(token.len()) {
                    continue;
                }
                let (head, rem) = path.split_at(token.len());
                if !head.eq_ignore_ascii_case(token) {
                    continue;
                }
                // One segment at a time, like the ghost: a deep match
                // drills the tree one directory per acceptance.
                let (insert, cut_at_dir) = match rem.find(['/', '\\']) {
                    Some(i) => (&rem[..=i], true),
                    None => (rem, false),
                };
                let kind = if cut_at_dir || candidate.is_dir {
                    NebulaCompletionKind::Dir
                } else {
                    NebulaCompletionKind::File
                };
                push(
                    &mut items,
                    NebulaCompletionItem {
                        label: elide_left(&format!("{token}{insert}"), LABEL_MAX),
                        insert: insert.to_owned(),
                        kind,
                    },
                );
            }
        }
    }

    nebula_debug_log(format!("suggest_result kind=popup line={:?} items={}", line, items.len()));
    state.completion_items = items;
    state.completion_selected = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory_history::DirectoryHistory;
    use crate::nebula_history::NebulaHistory;

    struct Fixture {
        history: NebulaHistory,
        directories: DirectoryHistory,
        commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Fixture {
        /// 历史与 frecency 都留空，断言才只反映被测的那条分支。`commands` 装的
        /// 是**这台机器**的 PATH 探针结果，所以放一个只有 Windows 才有的名字。
        fn new() -> Self {
            Self {
                history: NebulaHistory::default(),
                directories: DirectoryHistory::empty(),
                commands: std::sync::Arc::new(std::sync::Mutex::new(vec![
                    "notepad.exe".to_owned(),
                    "grepwin.exe".to_owned(),
                ])),
            }
        }

        fn sources(&self, style: CompletionStyle) -> SuggestSources<'_> {
            SuggestSources {
                history: &self.history,
                directories: &self.directories,
                commands: &self.commands,
                enabled: true,
                style,
            }
        }

        /// 在给定环境下算一次 ghost，返回补出的余量。
        fn ghost(&self, env: SuggestEnv, cwd: &str, line: &str) -> String {
            let mut state =
                NebulaPaneState { cwd: cwd.to_owned(), suggest_env: env, ..Default::default() };
            suggest_update(
                &self.sources(CompletionStyle::Inline),
                &mut state,
                Some(line.to_owned()),
            );
            state.suggestion
        }

        /// 同上但走弹窗，返回候选的插入余量。
        fn popup(&self, env: SuggestEnv, cwd: &str, line: &str) -> Vec<String> {
            let mut state =
                NebulaPaneState { cwd: cwd.to_owned(), suggest_env: env, ..Default::default() };
            suggest_update(
                &self.sources(CompletionStyle::Popup),
                &mut state,
                Some(line.to_owned()),
            );
            state.completion_items.into_iter().map(|item| item.insert).collect()
        }
    }

    /// 走错机器不是"补不出来"这么轻——它会把**宿主**的目录补进 WSL 的命令行。
    ///
    /// 同一条命令、同一个 cwd，只有环境不同：本机该补出来，WSL 必须一无所获。
    /// 断言用的是同一份真实存在的宿主目录，所以"WSL 补不出"只可能来自环境
    /// 分流，不可能是路径碰巧不存在。
    #[test]
    fn a_foreign_pane_never_completes_a_path_off_this_machine() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("host-only-dir")).unwrap();
        let cwd = temp.path().to_string_lossy().into_owned();
        let fixture = Fixture::new();

        assert_eq!(
            fixture.ghost(SuggestEnv::Local, &cwd, "ls host-only-"),
            "dir\\",
            "本机 tab 的 std::fs 就是这个 pane 的文件系统（补到目录带尾分隔符）"
        );
        for env in [
            SuggestEnv::Wsl { distro: "Debian".to_owned() },
            SuggestEnv::Ssh { destination: "kud@box".to_owned() },
        ] {
            assert_eq!(
                fixture.ghost(env.clone(), &cwd, "ls host-only-"),
                "",
                "{env:?} 的文件系统这个进程碰不到，补出宿主路径比补不出来更糟"
            );
            assert!(
                fixture.popup(env.clone(), &cwd, "ls host-only-").is_empty(),
                "弹窗与 ghost 必须同一套分流：{env:?}"
            );
        }
    }

    /// 进程 PATH 描述的是 Windows。把它当成来宾/远端的命令集，补出的
    /// `notepad.exe` 在那边根本不存在。
    #[test]
    fn a_foreign_pane_completes_posix_commands_instead_of_this_machines_binaries() {
        let fixture = Fixture::new();
        assert_eq!(fixture.ghost(SuggestEnv::Local, "", "notepad"), ".exe");

        let wsl = SuggestEnv::Wsl { distro: "Debian".to_owned() };
        assert_eq!(fixture.ghost(wsl.clone(), "", "notepad"), "", "来宾没有 notepad.exe");
        assert_eq!(fixture.ghost(wsl.clone(), "", "grep"), "", "精确命令不再往更长的邻居补");
        assert_eq!(fixture.ghost(wsl.clone(), "", "systemc"), "tl");
        assert!(fixture.popup(wsl, "", "gre").contains(&"p".to_owned()), "弹窗同样走 POSIX 表");
    }

    /// 这张表是"远端 PATH 还没探到"时的兜底，所以里面只能是那边真有的东西。
    #[test]
    fn the_posix_table_names_nothing_windows_only() {
        assert!(
            POSIX_COMMANDS.iter().all(|name| !name.ends_with(".exe")),
            "Windows 可执行后缀不该出现在 POSIX 表里"
        );
        for expected in ["ls", "grep", "sudo", "systemctl", "apt", "cargo"] {
            assert!(POSIX_COMMANDS.contains(&expected), "缺少常用命令 {expected}");
        }
        assert_eq!(posix_commands().len(), POSIX_COMMANDS.len(), "两个视图必须同源");
    }
}
