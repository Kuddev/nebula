//! Find the current Codex conversation when no session ID was reported by its hook.
//!
//! Codex keeps the active rollout open, and its process environment carries
//! the pane ID supplied by the PTY launcher. This links a pane to its rollout
//! without relying on working-directory matches or file modification
//! time. Only the first metadata record is read, without conversation content.
//! The lookup is bounded and is only called by a background
//! task; UI code must never scan `/proc` or start `wsl.exe` synchronously.

use std::io::{self, Read, Seek as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_PROBE_LINE: usize = 64 * 1024;
const MAX_PROBE_RECORDS: usize = 128;

const PROBE_SCRIPT: &str = r###"
pane="$1"
instance="$2"
for proc in /proc/[0-9]*; do
    pid=${proc##*/}
    IFS= read -r comm < "$proc/comm" 2>/dev/null || continue
    case "$comm" in
        codex|codex.exe|codex-*) ;;
        *) continue ;;
    esac
    [ -r "$proc/environ" ] || continue
    environment=$(tr "\0" "\n" < "$proc/environ" 2>/dev/null || true)
    pane_id=$(printf '%s\n' "$environment" | sed -n "s/^PEBREL_PANE_ID=//p" | head -n 1)
    if [ "$pane_id" != "$pane" ]; then
        pane_id=$(printf '%s\n' "$environment" | sed -n "s/^NEBULA_PANE_ID=//p" | head -n 1)
    fi
    process_id=$(printf '%s\n' "$environment" | sed -n "s/^PEBREL_PROCESS_ID=//p" | head -n 1)
    [ "$pane_id" = "$pane" ] && [ "$process_id" = "$instance" ] || continue
    codex_root=$(printf '%s\n' "$environment" | sed -n "s/^CODEX_HOME=//p" | head -n 1)
    if [ -z "$codex_root" ]; then
        guest_home=$(printf '%s\n' "$environment" | sed -n "s/^HOME=//p" | head -n 1)
        [ -n "$guest_home" ] || continue
        codex_root="$guest_home/.codex"
    fi
    for fd in "$proc"/fd/*; do
        link=$(readlink "$fd" 2>/dev/null || true)
        case "$link" in
            "$codex_root"/sessions/*/rollout-*.jsonl)
                first=$(head -c 65536 "$fd" 2>/dev/null | head -n 1)
                [ -n "$first" ] || continue
                printf '%s\t%s\t%s\t%s\n' "$pid" "${fd##*/}" "$link" "$first"
                ;;
        esac
    done
done
"###;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeRecord {
    _pid: u32,
    _fd: u32,
    link: String,
    first_line: String,
}

/// Probe the active Codex rollout for one pane.
///
/// WSL panes are inspected inside the guest because Windows Toolhelp cannot
/// see Linux processes or their open session files. Linux hosts use the same
/// process metadata directly. Native Windows and macOS sessions continue to
/// use hook-reported identities; this fallback requires Linux procfs.
pub(crate) fn probe_codex_session(
    pane_id: u64,
    exec_context: Option<&crate::runtime_exec::PaneExecContext>,
) -> Option<String> {
    let pane_id = pane_id.to_string();
    let context = exec_context?;
    let instance = context.process_instance()?;
    if let Some(distro) = context.wsl_distribution() {
        return probe_wsl(distro, context.wsl_user(), &pane_id, instance);
    }

    #[cfg(unix)]
    {
        return probe_local_proc(&pane_id, instance);
    }
    #[cfg(not(unix))]
    {
        let _ = (pane_id, instance);
        None
    }
}

fn parse_probe_records(output: &str) -> Option<String> {
    let mut candidate = None;
    for record in output.lines().filter_map(parse_probe_record) {
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&record.first_line) else {
            continue;
        };
        if meta.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = meta.get("payload").unwrap_or(&meta);
        let Some(id) = payload.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !valid_uuid(id) {
            continue;
        }
        // Codex's main interactive thread is marked as a CLI/user thread.
        // Keep accepting older session_meta lines that lack these fields, but
        // reject an explicit guardian/sub-agent marker.
        if !matches_optional_string(payload, &meta, "source", "cli")
            || !matches_optional_string(payload, &meta, "thread_source", "user")
        {
            continue;
        }
        let Some(filename_id) = rollout_id(Path::new(&record.link)) else { continue };
        if filename_id != id {
            continue;
        }
        if candidate.as_ref().is_some_and(|existing| existing != id) {
            // More than one distinct main/user rollout is ambiguous.  A
            // process can have guardian/sub-agent files open, but it must not
            // have two possible user conversations silently choose one.
            return None;
        }
        candidate = Some(id.to_owned());
    }
    candidate
}

fn parse_probe_record(line: &str) -> Option<ProbeRecord> {
    let mut fields = line.splitn(4, '\t');
    Some(ProbeRecord {
        _pid: fields.next()?.parse().ok()?,
        _fd: fields.next()?.parse().ok()?,
        link: fields.next()?.to_owned(),
        first_line: fields.next()?.to_owned(),
    })
}

fn matches_optional_string(
    payload: &serde_json::Value,
    root: &serde_json::Value,
    key: &str,
    expected: &str,
) -> bool {
    payload
        .get(key)
        .or_else(|| root.get(key))
        .map_or(true, |value| value.as_str() == Some(expected))
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.as_bytes().iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) { *byte == b'-' } else { byte.is_ascii_hexdigit() }
        })
}

fn rollout_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let start = stem.len().checked_sub(36)?;
    let id = stem.get(start..)?;
    valid_uuid(id).then(|| id.to_owned())
}

fn probe_wsl(
    distro: Option<&str>,
    user: Option<&str>,
    pane_id: &str,
    instance: &str,
) -> Option<String> {
    let mut command = Command::new("wsl.exe");
    command.args(wsl_probe_args(distro, user, pane_id, instance));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    run_probe_command(command)
}

fn wsl_probe_args(
    distro: Option<&str>,
    user: Option<&str>,
    pane_id: &str,
    instance: &str,
) -> Vec<String> {
    let mut args = Vec::with_capacity(9);
    if let Some(distro) = distro {
        args.extend(["--distribution".to_owned(), distro.to_owned()]);
    }
    if let Some(user) = user {
        args.extend(["--user".to_owned(), user.to_owned()]);
    }
    args.extend([
        "--exec".to_owned(),
        "sh".to_owned(),
        "-c".to_owned(),
        PROBE_SCRIPT.to_owned(),
        "pebrel-ai-session-probe".to_owned(),
        pane_id.to_owned(),
        instance.to_owned(),
    ]);
    args
}

/// Bound the guest lookup by time and output size. A temporary file avoids a
/// reader thread surviving when a descendant keeps the output handle open.
fn run_probe_command(mut command: Command) -> Option<String> {
    let mut output = tempfile::tempfile().ok()?;
    command.stdin(Stdio::null()).stdout(output.try_clone().ok()?).stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None)
                if Instant::now() < deadline
                    && output
                        .metadata()
                        .is_ok_and(|meta| meta.len() <= MAX_PROBE_OUTPUT as u64) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            },
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            },
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            },
        }
    };
    output.rewind().ok()?;
    let output = read_bounded(output, MAX_PROBE_OUTPUT).ok()?;
    status.success().then(|| parse_probe_records(&String::from_utf8_lossy(&output)))?
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = limit.saturating_sub(output.len());
        if read > remaining {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "probe output exceeded limit"));
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

#[cfg(unix)]
fn probe_local_proc(pane_id: &str, instance: &str) -> Option<String> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut output = String::new();
    let mut records = 0;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        if Instant::now() >= deadline {
            return None;
        }
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let proc_dir = entry.path();
        let Ok(comm) = std::fs::read_to_string(proc_dir.join("comm")) else { continue };
        if !is_codex_process_name(comm.trim()) {
            continue;
        }
        let Ok(environment) = std::fs::read(proc_dir.join("environ")) else { continue };
        if !environment_has_value(&environment, b"PEBREL_PANE_ID", pane_id.as_bytes())
            && !environment_has_value(&environment, b"NEBULA_PANE_ID", pane_id.as_bytes())
        {
            continue;
        }
        if !environment_has_value(
            &environment,
            crate::agent_env::PROCESS_ENV.as_bytes(),
            instance.as_bytes(),
        ) {
            continue;
        }
        let Some(codex_home) = codex_home_from_environment(&environment) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(proc_dir.join("fd")) else { continue };
        for fd in fds.flatten() {
            let fd_path = fd.path();
            let Ok(link) = std::fs::read_link(&fd_path) else { continue };
            if !is_rollout_link(&link, &codex_home) {
                continue;
            }
            let Ok(first_line) = read_first_line(&fd_path) else { continue };
            let fd_number = fd.file_name().to_string_lossy().parse::<u32>().ok();
            let Some(fd_number) = fd_number else { continue };
            use std::fmt::Write as _;
            let _ = writeln!(output, "{pid}\t{fd_number}\t{}\t{first_line}", link.display());
            records += 1;
            if records >= MAX_PROBE_RECORDS || output.len() >= MAX_PROBE_OUTPUT {
                return None;
            }
        }
    }
    parse_probe_records(&output)
}

#[cfg(unix)]
fn is_codex_process_name(name: &str) -> bool {
    matches!(name, "codex" | "codex.exe") || name.starts_with("codex-")
}

#[cfg(unix)]
fn environment_has_value(environment: &[u8], key: &[u8], expected: &[u8]) -> bool {
    let mut prefix = Vec::with_capacity(key.len() + 1);
    prefix.extend_from_slice(key);
    prefix.push(b'=');
    environment
        .split(|byte| *byte == 0)
        .any(|item| item.strip_prefix(prefix.as_slice()).is_some_and(|value| value == expected))
}

#[cfg(unix)]
fn environment_value<'a>(environment: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut prefix = Vec::with_capacity(key.len() + 1);
    prefix.extend_from_slice(key);
    prefix.push(b'=');
    environment
        .split(|byte| *byte == 0)
        .find_map(|item| item.strip_prefix(prefix.as_slice()))
        .filter(|value| !value.is_empty())
}

#[cfg(unix)]
fn codex_home_from_environment(environment: &[u8]) -> Option<std::path::PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let home = environment_value(environment, b"CODEX_HOME")
        .or_else(|| environment_value(environment, b"HOME"))?;
    let home = std::path::PathBuf::from(OsStr::from_bytes(home));
    if environment_value(environment, b"CODEX_HOME").is_some() {
        Some(home)
    } else {
        Some(home.join(".codex"))
    }
}

#[cfg(unix)]
fn is_rollout_link(link: &Path, codex_home: &Path) -> bool {
    let sessions = codex_home.join("sessions");
    link.strip_prefix(sessions).is_ok_and(|relative| {
        relative.components().count() >= 2
            && link
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
    })
}

#[cfg(unix)]
fn read_first_line(path: &Path) -> std::io::Result<String> {
    use std::io::{BufRead as _, BufReader};
    let file = std::fs::File::open(path)?;
    let mut line = String::new();
    BufReader::new(file).take(MAX_PROBE_LINE as u64).read_line(&mut line)?;
    if !line.ends_with(['\r', '\n']) && line.len() >= MAX_PROBE_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "session metadata line too long"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_ID: &str = "01a079fa-4a9b-7d93-8a4a-4a7a9edaf247";
    const GUARDIAN_ID: &str = "01a079f7-7e36-7232-882b-f06d3bde9df8";

    #[cfg(unix)]
    #[test]
    fn guest_lookup_returns_within_its_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 10"]);
        let started = Instant::now();
        assert_eq!(run_probe_command(command), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn oversized_output_is_rejected_instead_of_selecting_a_partial_candidate() {
        assert_eq!(read_bounded(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_bounded(&b"12345"[..], 4).is_err());
    }

    #[test]
    fn selects_cli_user_rollout_and_rejects_guardian_thread() {
        let output = format!(
            "1996\t37\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{ROOT_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ROOT_ID}\",\"source\":\"cli\",\"thread_source\":\"user\"}}}}\n1996\t80\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{GUARDIAN_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{GUARDIAN_ID}\",\"source\":\"cli\",\"thread_source\":\"guardian\"}}}}\n"
        );
        assert_eq!(parse_probe_records(&output).as_deref(), Some(ROOT_ID));
    }

    #[test]
    fn rejects_a_rollout_bound_to_a_different_filename() {
        let output = format!(
            "1996\t37\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{ROOT_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{GUARDIAN_ID}\"}}}}\n"
        );
        assert_eq!(parse_probe_records(&output), None);
    }

    #[test]
    fn accepts_legacy_session_meta_without_thread_markers() {
        let output = format!(
            "1996\t37\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{ROOT_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ROOT_ID}\"}}}}\n"
        );
        assert_eq!(parse_probe_records(&output).as_deref(), Some(ROOT_ID));
    }

    #[test]
    fn wsl_probe_keeps_named_distribution_and_pane_as_arguments() {
        let full = wsl_probe_args(Some("Debian"), Some("hello"), "7", "42");
        let named: Vec<_> = full.iter().map(String::as_str).take(5).collect();
        assert_eq!(named, vec!["--distribution", "Debian", "--user", "hello", "--exec"]);
        let args = wsl_probe_args(None, None, "7", "42");
        assert_eq!(args.first().map(String::as_str), Some("--exec"));
        assert_eq!(args[args.len() - 2], "7");
        assert_eq!(args.last().map(String::as_str), Some("42"));
        assert_eq!(args[args.len() - 3], "pebrel-ai-session-probe");
    }

    #[test]
    fn explicit_subagent_markers_are_not_accepted_as_the_main_thread() {
        let output = format!(
            "1996\t37\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{ROOT_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ROOT_ID}\",\"source\":\"subagent\",\"thread_source\":\"user\"}}}}\n"
        );
        assert_eq!(parse_probe_records(&output), None);
    }

    #[test]
    fn multiple_distinct_main_rollouts_are_rejected_as_ambiguous() {
        let output = format!(
            "1996\t37\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{ROOT_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ROOT_ID}\",\"source\":\"cli\"}}}}\n1996\t38\t/home/hello/.codex/sessions/2026/09/07/rollout-x-{GUARDIAN_ID}.jsonl\t{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{GUARDIAN_ID}\",\"source\":\"cli\"}}}}\n"
        );
        assert_eq!(parse_probe_records(&output), None);
    }

    #[cfg(unix)]
    #[test]
    fn custom_codex_home_is_used_instead_of_the_default_directory() {
        let environment = b"HOME=/home/hello\0CODEX_HOME=/srv/codex-data\0";
        assert_eq!(
            codex_home_from_environment(environment).as_deref(),
            Some(Path::new("/srv/codex-data"))
        );
        let environment = b"HOME=/home/hello\0";
        assert_eq!(
            codex_home_from_environment(environment).as_deref(),
            Some(Path::new("/home/hello/.codex"))
        );
    }
}
