//! Shared command discovery and first-token completion primitives.
//!
//! This module owns no window or renderer state. Both the legacy shell and the
//! GPUI product use the same process-wide command snapshot and matching rules.

use std::collections::HashSet;
use std::path::Path;

/// Cap on ghost-text length so a deeply nested path cannot spill across the
/// whole row and cover surrounding UI.
pub(crate) const NEBULA_GHOST_MAX: usize = 96;

/// First word of a committed command line, normalized to a program identity:
/// lowercase, without path prefix or common Windows launcher extensions.
pub(crate) fn extract_program(line: &str) -> Option<String> {
    let first = line.trim().split_whitespace().next()?;
    let base = first.trim_matches('"').rsplit(['/', '\\']).next().unwrap_or(first);
    let mut name = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1", ".com"] {
        if let Some(stripped) = name.strip_suffix(ext) {
            name = stripped.to_owned();
            break;
        }
    }
    (!name.is_empty()).then_some(name)
}

#[cfg(windows)]
fn nebula_pathexts() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.PS1".to_owned())
        .split(';')
        .filter_map(|ext| {
            let ext = ext.trim();
            if ext.is_empty() {
                None
            } else if ext.starts_with('.') {
                Some(ext.to_ascii_lowercase())
            } else {
                Some(format!(".{}", ext.to_ascii_lowercase()))
            }
        })
        .collect()
}

#[cfg(windows)]
fn nebula_command_name(path: &Path, pathexts: &[String]) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let ext = format!(".{ext}").to_ascii_lowercase();
    if !pathexts.iter().any(|known| known == &ext) {
        return None;
    }
    path.file_stem()?.to_str().filter(|name| !name.is_empty()).map(ToOwned::to_owned)
}

#[cfg(not(windows))]
fn nebula_command_name(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata().ok()?.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    path.file_name()?.to_str().filter(|name| !name.is_empty()).map(ToOwned::to_owned)
}

fn nebula_path_commands() -> Vec<String> {
    let Some(path_env) = std::env::var_os("PATH") else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    let mut seen = HashSet::new();
    #[cfg(windows)]
    let pathexts = nebula_pathexts();

    for dir in std::env::split_paths(&path_env).filter(|dir| !dir.as_os_str().is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }

            #[cfg(windows)]
            let command = nebula_command_name(&entry.path(), &pathexts);
            #[cfg(not(windows))]
            let command = nebula_command_name(&entry.path());

            if let Some(command) = command {
                #[cfg(windows)]
                let key = command.to_ascii_lowercase();
                #[cfg(not(windows))]
                let key = command.clone();

                if seen.insert(key) {
                    commands.push(command);
                }
            }
        }
    }

    commands.sort_by(|a, b| {
        #[cfg(windows)]
        {
            a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then(a.cmp(b))
        }
        #[cfg(not(windows))]
        {
            a.cmp(b)
        }
    });
    commands
}

fn nebula_collect_commands() -> Vec<String> {
    let mut commands = nebula_path_commands();

    #[cfg(windows)]
    {
        let mut seen: HashSet<String> = commands.iter().map(|c| c.to_ascii_lowercase()).collect();
        for command in nebula_powershell_commands() {
            if seen.insert(command.to_ascii_lowercase()) {
                commands.push(command);
            }
        }
        commands.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then(a.cmp(b)));
    }

    commands
}

/// PowerShell cmdlets, functions and aliases do not appear on PATH. Collect
/// them once without loading the user's profile; failures simply leave PATH
/// commands as the available completion source.
#[cfg(windows)]
fn nebula_powershell_commands() -> Vec<String> {
    use std::os::windows::process::CommandExt;

    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Command -CommandType Cmdlet,Function,Alias -ErrorAction SilentlyContinue \
             | Select-Object -ExpandProperty Name",
        ])
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Process-wide command list populated on a background thread, so discovery
/// never blocks window startup.
pub(crate) fn nebula_commands_handle() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    static COMMANDS: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<String>>>> =
        std::sync::OnceLock::new();
    COMMANDS
        .get_or_init(|| {
            let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let bg = shared.clone();
            std::thread::spawn(move || {
                let commands = nebula_collect_commands();
                if let Ok(mut guard) = bg.lock() {
                    *guard = commands;
                }
            });
            shared
        })
        .clone()
}

pub(crate) fn nebula_command_hint<'a>(commands: &'a [String], prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return None;
    }

    #[cfg(windows)]
    let exact = commands.iter().any(|command| command.eq_ignore_ascii_case(prefix));
    #[cfg(not(windows))]
    let exact = commands.iter().any(|command| command == prefix);
    if exact {
        return None;
    }

    commands.iter().find_map(|command| {
        if command.len() <= prefix.len() || !command.is_char_boundary(prefix.len()) {
            return None;
        }
        let (head, rem) = command.split_at(prefix.len());
        #[cfg(windows)]
        let matches = head.eq_ignore_ascii_case(prefix);
        #[cfg(not(windows))]
        let matches = head == prefix;

        matches.then_some(rem)
    })
}

/// Return up to `limit` full command names extending `prefix`. An exact match
/// is placed first but does not suppress longer neighbors in the popup list.
pub(crate) fn nebula_command_hints<'a>(
    commands: &'a [String],
    prefix: &str,
    limit: usize,
) -> Vec<&'a str> {
    if prefix.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<&'a str> = Vec::new();

    for exact_only in [true, false] {
        for command in commands {
            if command.len() < prefix.len() || !command.is_char_boundary(prefix.len()) {
                continue;
            }
            let head = &command[..prefix.len()];
            let exact = command.len() == prefix.len();
            if exact != exact_only {
                continue;
            }
            #[cfg(windows)]
            let matches = head.eq_ignore_ascii_case(prefix);
            #[cfg(not(windows))]
            let matches = head == prefix;

            #[cfg(windows)]
            let duplicate = out.iter().any(|seen| seen.eq_ignore_ascii_case(command));
            #[cfg(not(windows))]
            let duplicate = out.iter().any(|seen| *seen == command.as_str());

            if matches && !duplicate {
                out.push(command);
                if out.len() == limit {
                    return out;
                }
            }
        }
    }
    out
}

pub(crate) fn nebula_is_command_position(line: &str) -> bool {
    !line.contains([' ', '\t'])
        && !line.contains(['/', '\\'])
        && line.as_bytes().get(1) != Some(&b':')
}

pub(crate) fn nebula_path_wants_directory(line: &str) -> bool {
    let command = line.split([' ', '\t']).next().unwrap_or("");
    matches!(
        command.to_ascii_lowercase().as_str(),
        "cd" | "chdir" | "pushd" | "sl" | "set-location"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn program_identity_strips_paths_and_launcher_extensions() {
        assert_eq!(extract_program(r#"D:\tools\Claude.EXE --resume"#).as_deref(), Some("claude"));
        assert_eq!(extract_program("/usr/local/bin/cargo test").as_deref(), Some("cargo"));
        assert_eq!(extract_program(""), None);
    }

    #[test]
    fn exact_command_suppresses_only_the_inline_remainder() {
        let commands = strings(&["claude", "claude-agent-acp"]);
        assert_eq!(nebula_command_hint(&commands, "clau"), Some("de"));
        assert_eq!(nebula_command_hint(&commands, "claude"), None);

        #[cfg(windows)]
        assert_eq!(nebula_command_hint(&commands, "CLAUDE"), None);
    }

    #[test]
    fn popup_keeps_exact_and_longer_commands_without_duplicates() {
        let commands = strings(&["claude", "claude-agent-acp", "claude", "cargo"]);
        assert_eq!(
            nebula_command_hints(&commands, "claude", 8),
            vec!["claude", "claude-agent-acp"]
        );
        assert_eq!(nebula_command_hints(&commands, "c", 2), vec!["claude", "claude-agent-acp"]);
        assert_eq!(nebula_command_hints(&strings(&["LsaIso", "lsass", "ls"]), "ls", 8)[0], "ls");
        assert!(nebula_command_hints(&commands, "", 8).is_empty());
    }

    #[test]
    fn command_and_directory_positions_are_explicit() {
        assert!(nebula_is_command_position("cargo"));
        assert!(!nebula_is_command_position("cargo test"));
        assert!(!nebula_is_command_position("./cargo"));
        assert!(nebula_path_wants_directory("cd src"));
        assert!(nebula_path_wants_directory("Set-Location src"));
        assert!(!nebula_path_wants_directory("cat src"));
    }
}
