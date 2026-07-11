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

/// First non-stateless process under `root_pid` (the pane's shell), or `None`
/// when the whole tree is safe to kill. The name is used in the confirm modal.
#[cfg(windows)]
pub fn busy_child(root_pid: u32) -> Option<String> {
    use std::collections::HashMap;
    use std::mem;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    if root_pid == 0 {
        return None;
    }

    // One snapshot of every process: (pid -> (parent, exe name)).
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut procs: HashMap<u32, (u32, String)> = HashMap::new();
    unsafe {
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                procs.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }

    // Parent -> children edges, then BFS down from the shell.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(parent, _)) in &procs {
        // PIDs are recycled; a stale parent id equal to itself would loop.
        if parent != pid {
            children.entry(parent).or_default().push(pid);
        }
    }

    let mut queue = vec![root_pid];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = queue.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if pid != root_pid {
            if let Some((_, name)) = procs.get(&pid) {
                let lower = name.to_ascii_lowercase();
                if !STATELESS.contains(&lower.as_str()) {
                    return Some(name.clone());
                }
            }
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend_from_slice(kids);
        }
    }
    None
}

#[cfg(not(windows))]
pub fn busy_child(_root_pid: u32) -> Option<String> {
    None
}
