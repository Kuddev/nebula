//! Child-process inspection for close confirmation (a close-confirmation safety net).
//!
//! Before a pane/tab/window closes, its shell's descendant process tree is
//! checked against a whitelist of "stateless" programs (shells and plumbing).
//! Any other descendant — a running build, vim, ssh — means the close should be
//! confirmed by the user rather than silently killing work. This deliberately
//! needs no shell integration (unlike the OSC 133 approach), so it works with
//! any shell out of the box.

/// Programs whose presence never blocks a close: shells themselves plus the
/// console plumbing every ConPTY session drags along. `git.exe` is here
/// because Nebula's own prompt integration spawns it on every prompt render
/// (branch for the powerline) — the close snapshot routinely catches one
/// mid-flight, and a user-run git operation is crash-safe by design anyway.
const STATELESS: &[&str] = &[
    "cmd.exe",
    "conhost.exe",
    "openconsole.exe",
    "powershell.exe",
    "pwsh.exe",
    "bash.exe",
    "sh.exe",
    "dash.exe",
    "zsh.exe",
    "fish.exe",
    "nu.exe",
    "wsl.exe",
    "wslhost.exe",
    "wslrelay.exe",
    "winpty-agent.exe",
    "git.exe",
];

/// 进去之后就是一个新提示符的程序：它们「在跑」不代表有活儿在跑。
///
/// 与 [`STATELESS`] 有意分成两张表。`STATELESS` 回答的是「关掉这个 pane 会不会
/// 丢东西」，所以里面有 `git.exe`（我们自己的 prompt 集成每个提示符都跑它）和
/// conhost 一类 console plumbing；这张表回答的是「前台这个东西会不会自己结束」。
/// 混用会让 `git log` 刚开始就被判成不在跑。
///
/// 只收会 fork 子进程的 shell，不收 `python` / `node` 这类 REPL。区别在能不能
/// 被纠正：在 `cmd` 里跑 `ping -t` 会多出一个 `ping.exe`，进程树看得见，状态能
/// 被拉回运行中；而 REPL 里的 `time.sleep(60)` 跑在解释器进程内部，进程树永远
/// 只看到 `python.exe`，一旦判成不在跑就再也纠正不回来。
const INTERACTIVE_SHELLS: &[&str] =
    &["cmd", "powershell", "pwsh", "bash", "sh", "dash", "zsh", "fish", "nu", "wsl"];

/// 这条命令是不是「进入一个交互式 shell」，而不是一件会自己结束的活儿。
///
/// Windows 的 ConPTY 没有前台进程组（POSIX 的 `tcgetpgrp`），拿不到「现在谁占着
/// 终端」这个实时事实，只能枚举进程树——那必须节流，最快也要几秒才出结果。命令
/// 启动的这一刻是唯一能零延迟判断的时机，所以这里用名字判一次。
///
/// 必须整条命令就只有这个 shell 名（可带路径和 `.exe`）。带参数一律当普通命令：
/// `cmd /c dir`、`bash script.sh`、`wsl ls` 都会自己结束，状态该正常走完。
pub fn is_interactive_shell_command(command: &str) -> bool {
    let command = command.trim();
    // 整条命令是一个带引号的路径时先剥引号：安装路径里有空格就得这么写，
    // 剥完不能再按空白切词，否则 `"C:\Program Files\pwsh.exe"` 会被当成两段。
    if let Some(inner) = command.strip_prefix('"').and_then(|rest| rest.strip_suffix('"'))
        && !inner.contains('"')
    {
        return is_shell_executable(inner);
    }
    let mut tokens = command.split_whitespace();
    let Some(first) = tokens.next() else { return false };
    if tokens.next().is_some() {
        return false;
    }
    is_shell_executable(first)
}

/// 一个可执行文件路径的基名（去掉目录与 `.exe`）是否在交互式 shell 表里。
fn is_shell_executable(path: &str) -> bool {
    let name = path.trim_matches(['"', '\'']).rsplit(['/', '\\']).next().unwrap_or(path);
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(name.as_str());
    INTERACTIVE_SHELLS.contains(&stem)
}

/// Turn a raw exe name into something a confirm dialog can show: drop the
/// `.exe` suffix (`node.exe` → `node`).
///
/// 2026-07-27 用户反馈：关闭窗口时确认框写的是 `node.exe 仍在运行`——那其实
/// 是 Claude Code，只不过快照只看得见宿主解释器。真正的修复在调用方
/// （`busy_process_in` 优先用 pane 已知的 `running_program`）；这里只负责
/// 没有那份身份信息时把名字擦干净。
pub fn display_name(exe: &str) -> String {
    exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe).to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub executable: String,
    pub depth: u32,
}

/// One Toolhelp pass over every process on the machine: `pid -> (parent, exe)`.
///
/// Shared by the descendant walk and the ancestry check so a caller never pays
/// for two snapshots, and so both see the same instant.
#[cfg(windows)]
fn snapshot() -> Result<std::collections::HashMap<u32, (u32, String)>, String> {
    use std::collections::HashMap;
    use std::mem;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!("CreateToolhelp32Snapshot failed: {}", std::io::Error::last_os_error()));
    }

    let mut processes: HashMap<u32, (u32, String)> = HashMap::new();
    let read_result = unsafe {
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(handle, &mut entry) == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                processes.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                if Process32NextW(handle, &mut entry) == 0 {
                    break;
                }
            }
            Ok(())
        }
    };
    unsafe { CloseHandle(handle) };
    read_result.map_err(|error| format!("Process32FirstW failed: {error}"))?;
    Ok(processes)
}

/// Snapshot the real local descendants owned by one terminal shell. The root
/// is included at depth zero so clients can distinguish the PTY owner from the
/// commands below it. Toolhelp is intentionally sampled on demand; running it
/// on the 1 Hz UI state pump would scan the whole machine continuously.
#[cfg(windows)]
pub fn descendants(root_pid: u32) -> Result<Vec<ProcessEntry>, String> {
    use std::collections::{HashMap, HashSet, VecDeque};

    if root_pid == 0 {
        return Err("the pane does not own a local shell process".to_owned());
    }

    let processes = snapshot()?;

    if !processes.contains_key(&root_pid) {
        return Err(format!("shell process {root_pid} is no longer present"));
    }

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in &processes {
        if parent != pid {
            children.entry(parent).or_default().push(pid);
        }
    }
    for child_ids in children.values_mut() {
        child_ids.sort_unstable();
    }

    let mut result = Vec::new();
    let mut queue = VecDeque::from([(root_pid, 0_u32)]);
    let mut seen = HashSet::new();
    while let Some((pid, depth)) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some((parent_pid, executable)) = processes.get(&pid) {
            result.push(ProcessEntry {
                pid,
                parent_pid: *parent_pid,
                executable: executable.clone(),
                depth,
            });
        }
        if let Some(child_ids) = children.get(&pid) {
            queue.extend(child_ids.iter().map(|child| (*child, depth.saturating_add(1))));
        }
    }
    Ok(result)
}

#[cfg(not(windows))]
pub fn descendants(_root_pid: u32) -> Result<Vec<ProcessEntry>, String> {
    Err("pane.procs is not implemented on this platform".to_owned())
}

/// First non-stateless process under `root_pid` (the pane's shell), or `None`
/// when the whole tree is safe to kill. The name is used in the confirm modal.
#[cfg(windows)]
pub fn busy_child(root_pid: u32) -> Option<String> {
    descendants(root_pid).ok()?.into_iter().skip(1).find_map(|process| {
        let lower = process.executable.to_ascii_lowercase();
        (!STATELESS.contains(&lower.as_str())).then_some(process.executable)
    })
}

#[cfg(not(windows))]
pub fn busy_child(_root_pid: u32) -> Option<String> {
    None
}

/// First descendant that `AgentKind` recognizes as an AI CLI, as its slug.
///
/// `busy_child` walks BFS level order, so the first non-stateless hit is always
/// the host interpreter rather than the agent: codex really runs as
/// `powershell → node.exe → codex.exe → node_repl.exe`, and taking the depth-1
/// `node.exe` as the identity yields "node", which `AgentKind::parse` rejects —
/// the pane then has no logo and no semantic state at all.
///
/// 2026-08-22 实测（pane 4，issue #42 同批）：codex 已经答完，屏幕上就是空闲
/// 输入框，但 `running_program` 是 None，于是 1 Hz 屏幕看门狗在入口处早退，
/// 没有任何机制去看那块屏幕，转圈一直挂着。命令行首 token 是脆弱推断（会话
/// 恢复、别名、`npx codex` 都会落空），进程树才是客观事实。
#[cfg(windows)]
pub fn agent_child(root_pid: u32) -> Option<String> {
    descendants(root_pid).ok()?.into_iter().skip(1).find_map(|process| {
        crate::ai_agents::AgentKind::parse(&process.executable).map(|kind| kind.slug().to_owned())
    })
}

#[cfg(not(windows))]
pub fn agent_child(_root_pid: u32) -> Option<String> {
    None
}

/// 上溯上限。真实的 pane 进程链是 shell → 解释器 → agent → 子进程，个位数
/// 深度；给足余量同时保证快照里的父指针成环时一定会停。
#[cfg_attr(not(windows), allow(dead_code))]
const MAX_ANCESTRY_DEPTH: u32 = 64;

/// `pid` 是否落在 `root_pid` 的进程树内（含 `root_pid` 自身）。
///
/// 纯函数形态，便于用构造出来的进程表覆盖分叉、成环和缺链三种情况。
/// `None` 的含义是「拿不到证据」，绝不能当成校验失败——见
/// [`is_within_tree`] 的说明。
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_within_tree(
    processes: &std::collections::HashMap<u32, (u32, String)>,
    pid: u32,
    root_pid: u32,
) -> Option<bool> {
    if pid == 0 || root_pid == 0 {
        return None;
    }
    if pid == root_pid {
        return Some(true);
    }
    // pid 已经不在表里：它退出得比这次快照更早，无法判定归属。
    processes.get(&pid)?;
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        // 链断在半路（父进程先退出，Windows 上 pid 会悬空）= 这条链走不到
        // root，就是不在它的树内。这与「起始 pid 查不到」不同：那种情况才是
        // 真的没有证据。
        let Some(&(parent, _)) = processes.get(&current) else { return Some(false) };
        if parent == root_pid {
            return Some(true);
        }
        // 走到进程树顶。
        if parent == 0 || parent == current {
            return Some(false);
        }
        current = parent;
    }
    // 超过上限只可能是成环，说明快照本身不可信。
    None
}

/// 带 TTL 的全机进程快照，供路由校验和祖先链查询共用。
///
/// 用闭包而不是把表交出去：调用方只需要读，克隆整张进程表纯属浪费。
#[cfg(windows)]
fn with_cached_snapshot<T>(
    read: impl FnOnce(&std::collections::HashMap<u32, (u32, String)>) -> T,
) -> Option<T> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;

    type Cached = Option<(Instant, HashMap<u32, (u32, String)>)>;
    static CACHE: LazyLock<Mutex<Cached>> = LazyLock::new(|| Mutex::new(None));

    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let fresh = cache.as_ref().is_some_and(|(taken, _)| taken.elapsed() < SNAPSHOT_TTL);
    if !fresh {
        *cache = Some((Instant::now(), snapshot().ok()?));
    }
    let (_, processes) = cache.as_ref()?;
    Some(read(processes))
}

/// 事件路由的第二因子：`pid` 是否真的运行在 `root_pid` 这个 pane 里。
///
/// 环境变量携带的 pane id 是任何进程都能伪造的；进程祖先链不能。两者一致才
/// 采信声明的 pane，不一致就拒绝该事件（而不是回退到当前焦点 pane——那正是
/// 伪造者想要的结果）。
///
/// 返回 `None` 表示这一因子当前给不出证据（快照失败，或进程在我们查询之前
/// 就退出了）。调用方此时必须放行：宁可少一层校验，也不能因为一次竞态丢掉
/// 真实的完成通知。
///
/// 快照在 [`SNAPSHOT_TTL`] 内复用：一个回合里 hook 事件是成批到达的（每个
/// tool call 一条），逐条扫描全机进程表会把校验本身变成开销。过期的表只会让
/// 刚起的 helper 查不到而返回 `None`（放行），不会误判成伪造。
#[cfg(windows)]
pub fn is_within_tree(pid: u32, root_pid: u32) -> Option<bool> {
    with_cached_snapshot(|processes| resolve_within_tree(processes, pid, root_pid)).flatten()
}

/// 快照复用窗口。取值只需覆盖「同一回合内成批到达的 hook」，不需要更长：
/// 表越旧，查不到新 helper 的概率越高，校验就越容易退化成放行。
#[cfg(windows)]
const SNAPSHOT_TTL: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(not(windows))]
pub fn is_within_tree(_pid: u32, _root_pid: u32) -> Option<bool> {
    None
}

/// 沿 `pid` 的祖先链找最近的 AI CLI 进程，返回它的 pid 与 slug。
///
/// hook helper 不一定是 agent 直接 spawn 的：Windows 上 claude 通过 shell 执行
/// hook 命令，helper 的父进程是 `cmd.exe`，agent 在更上一层。所以不能只看父
/// 进程，必须往上找到第一个认得的 CLI。
///
/// 这个 pid 是 agent 的**进程身份**，比 session id 稳：嵌套 `claude -p` 子代理
/// 有自己的 pid，但它的 session id 是短命的，一旦被当成 pane 的会话身份，就会
/// 把真正活着的那个顶掉（`claude --resume` 之后指向一个已经结束的会话）。
#[cfg(windows)]
pub fn nearest_agent_ancestor(pid: u32) -> Option<(u32, String)> {
    with_cached_snapshot(|processes| resolve_agent_ancestor(processes, pid)).flatten()
}

#[cfg(not(windows))]
pub fn nearest_agent_ancestor(_pid: u32) -> Option<(u32, String)> {
    None
}

/// [`nearest_agent_ancestor`] 的纯函数内核。
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_agent_ancestor(
    processes: &std::collections::HashMap<u32, (u32, String)>,
    pid: u32,
) -> Option<(u32, String)> {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let (parent, executable) = processes.get(&current)?;
        if let Some(kind) = crate::ai_agents::AgentKind::parse(executable) {
            return Some((current, kind.slug().to_owned()));
        }
        if *parent == 0 || *parent == current {
            return None;
        }
        current = *parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        display_name, is_interactive_shell_command, resolve_agent_ancestor, resolve_within_tree,
    };

    fn table(rows: &[(u32, u32)]) -> HashMap<u32, (u32, String)> {
        rows.iter().map(|&(pid, parent)| (pid, (parent, format!("p{pid}.exe")))).collect()
    }

    /// 带自定义可执行名的进程表，用来构造「helper 上面隔着一层 shell」的链。
    fn named_table(rows: &[(u32, u32, &str)]) -> HashMap<u32, (u32, String)> {
        rows.iter()
            .map(|&(pid, parent, exe)| (pid, (parent, exe.to_owned())))
            .collect()
    }

    /// hook helper 的父进程往往不是 agent 本身：Windows 上 claude 通过
    /// `cmd.exe` 执行 hook 命令。只看父进程会把 agent 身份漏掉，必须往上找。
    #[test]
    fn agent_identity_comes_from_the_nearest_ancestor_not_the_parent() {
        // pwsh(100) → claude(200) → cmd(300) → nebula-hook(400)
        let processes = named_table(&[
            (100, 4, "pwsh.exe"),
            (200, 100, "claude.exe"),
            (300, 200, "cmd.exe"),
            (400, 300, "nebula-hook.exe"),
        ]);
        let (pid, slug) = resolve_agent_ancestor(&processes, 400).expect("claude 应被找到");
        assert_eq!(pid, 200);
        assert_eq!(slug, "claude");

        // 嵌套子代理：主 claude(200) 之下又起了一个 claude(500)。从子代理这条链
        // 上溯，拿到的是子代理自己的 pid——两条流因此能被区分开，短命的子会话
        // 不会顶掉活着的那个。
        let nested = named_table(&[
            (100, 4, "pwsh.exe"),
            (200, 100, "claude.exe"),
            (500, 200, "claude.exe"),
            (600, 500, "nebula-hook.exe"),
        ]);
        assert_eq!(resolve_agent_ancestor(&nested, 600).map(|(pid, _)| pid), Some(500));
        assert_eq!(resolve_agent_ancestor(&nested, 200).map(|(pid, _)| pid), Some(200));

        // 链上没有任何认得的 CLI：不许瞎猜。
        let plain = named_table(&[(100, 4, "pwsh.exe"), (200, 100, "nebula-hook.exe")]);
        assert_eq!(resolve_agent_ancestor(&plain, 200), None);
    }

    #[test]
    fn display_name_drops_exe_suffix() {
        assert_eq!(display_name("node.exe"), "node");
        assert_eq!(display_name("CARGO.EXE"), "CARGO");
        // Unix names and dotted names that are not suffixes stay intact.
        assert_eq!(display_name("cargo"), "cargo");
        assert_eq!(display_name("python3.11"), "python3.11");
    }

    /// 裸 shell 名 = 进了一个新提示符，不是一件会结束的活儿；一旦带参数就是
    /// 普通命令，状态必须照常走完，否则 `git log` 这种会一开始就被判完成。
    #[test]
    fn only_a_bare_shell_name_counts_as_entering_a_shell() {
        for bare in ["cmd", "cmd.exe", "CMD.EXE", "wsl", "pwsh", "bash", "  zsh  "] {
            assert!(is_interactive_shell_command(bare), "{bare} 应判为进入交互式 shell");
        }
        // 带路径也认。
        assert!(is_interactive_shell_command(r"C:\Windows\System32\cmd.exe"));
        assert!(is_interactive_shell_command("\"C:/Program Files/pwsh.exe\""));

        for command in ["cmd /c dir", "bash script.sh", "wsl ls", "git log", "npm run dev", ""] {
            assert!(!is_interactive_shell_command(command), "{command} 应判为普通命令");
        }
        // REPL 故意不在表里：它们里面跑的活儿在解释器进程内部，进程树看不见，
        // 判错了就再也纠正不回来。
        assert!(!is_interactive_shell_command("python"));
        assert!(!is_interactive_shell_command("node"));
    }

    #[test]
    fn ancestry_accepts_a_pid_inside_the_pane_tree() {
        // shell(100) → node(200) → claude(300) → hook(400)
        let processes = table(&[(100, 4), (200, 100), (300, 200), (400, 300)]);
        assert_eq!(resolve_within_tree(&processes, 400, 100), Some(true));
        assert_eq!(resolve_within_tree(&processes, 300, 100), Some(true));
        // root 自身算在树内：agent 直接就是 pane 根进程时也要通过。
        assert_eq!(resolve_within_tree(&processes, 100, 100), Some(true));
    }

    #[test]
    fn ancestry_rejects_a_pid_from_another_pane() {
        // 两个 pane 各自一条链；500 伪造声明自己属于 pane 100。
        let processes = table(&[(100, 4), (200, 100), (500, 4), (600, 500)]);
        assert_eq!(resolve_within_tree(&processes, 600, 100), Some(false));
        assert_eq!(resolve_within_tree(&processes, 500, 100), Some(false));
    }

    #[test]
    fn ancestry_reports_no_evidence_instead_of_failure() {
        let processes = table(&[(100, 4), (200, 100)]);
        // 进程已经退出：拿不到证据，必须是 None 而不是 Some(false)，否则一次
        // 竞态就会把真实事件判成伪造。
        assert_eq!(resolve_within_tree(&processes, 999, 100), None);
        // pid 0 与未知 root 同理。
        assert_eq!(resolve_within_tree(&processes, 0, 100), None);
        assert_eq!(resolve_within_tree(&processes, 200, 0), None);
        // 成环的快照不可信。
        let looped = table(&[(10, 11), (11, 10)]);
        assert_eq!(resolve_within_tree(&looped, 10, 100), None);
    }
}
