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

/// Snapshot the real local descendants owned by one terminal shell. The root
/// is included at depth zero so clients can distinguish the PTY owner from the
/// commands below it. Toolhelp is intentionally sampled on demand; running it
/// on the 1 Hz UI state pump would scan the whole machine continuously.
#[cfg(windows)]
pub fn descendants(root_pid: u32) -> Result<Vec<ProcessEntry>, String> {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::mem;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    if root_pid == 0 {
        return Err("the pane does not own a local shell process".to_owned());
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "CreateToolhelp32Snapshot failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut processes: HashMap<u32, (u32, String)> = HashMap::new();
    let read_result = unsafe {
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                processes.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
            Ok(())
        }
    };
    unsafe { CloseHandle(snapshot) };
    read_result.map_err(|error| format!("Process32FirstW failed: {error}"))?;

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

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn display_name_drops_exe_suffix() {
        assert_eq!(display_name("node.exe"), "node");
        assert_eq!(display_name("CARGO.EXE"), "CARGO");
        // Unix names and dotted names that are not suffixes stay intact.
        assert_eq!(display_name("cargo"), "cargo");
        assert_eq!(display_name("python3.11"), "python3.11");
    }
}
