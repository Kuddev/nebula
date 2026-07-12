//! `nebula ssh` — a thin wrapper around the system `ssh` that bootstraps
//! Nebula's shell integration on the *remote* host.
//!
//! Why this exists: Nebula recognises the running program / cwd / busy state of
//! a pane through three purely local channels (process-tree walk, local prompt
//! screen-scrape, and OSC 133 emitted by the locally-injected rcfile). Over a
//! plain `ssh`, all three go blind — the box only sees `ssh.exe`, and the real
//! `claude` / `vim` / `cargo` runs on the server. But SSH is a transparent byte
//! pipe: any OSC escape the *remote* shell emits travels back and is parsed by
//! Nebula's existing `osc_cwd` sniffer, exactly as if it were local.
//!
//! So this wrapper base64-inlines a small POSIX integration script, decodes it
//! into a temp rcfile on the remote, and execs the remote shell with it. The
//! remote shell then emits OSC 133;A/C/D (spinner) and a
//! `NEBULA|cwd|branch|program` title (tab icon + cwd) — no consumer-side change
//! beyond teaching the title parser to read the 4th `program` field.
//!
//! v1 scope: Linux remote + bash/zsh. Anything else degrades to a plain login
//! shell (no integration, but the connection still works).

/// Remote bash integration, decoded into `$TMP/bashrc` and passed via
/// `bash --rcfile`. Mirrors the local bash rc but drops all Windows path
/// translation (the remote cwd is already a real POSIX path) and adds the
/// `program` field to the title so the tab icon resolves over SSH.
#[cfg(windows)]
const REMOTE_BASH: &str = r#"
[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
__nebula_branch=""
__nebula_at_prompt=0
__nebula_precmd() {
    printf '\033]133;D\007'
    if command -v git >/dev/null 2>&1; then
        __nebula_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
    else
        __nebula_branch=""
    fi
    printf '\033]133;A\007'
    printf '\033]2;NEBULA|%s|%s|\007' "$PWD" "$__nebula_branch"
    __nebula_at_prompt=1
}
# preexec via DEBUG trap: fires before each command. Guarded so it emits
# only for the first command after a prompt (not for PROMPT_COMMAND's own
# work, completion, or subsequent pipeline stages).
__nebula_preexec() {
    case "$__nebula_at_prompt" in 1) ;; *) return ;; esac
    case "${COMP_LINE:-}" in ?*) return ;; esac
    __nebula_at_prompt=0
    # First whitespace-delimited word, then strip any path prefix
    # (`/usr/bin/vim` -> `vim`). Matches the local `extract_program`.
    __nebula_prog="${BASH_COMMAND%% *}"
    __nebula_prog="${__nebula_prog##*/}"
    printf '\033]133;C\007'
    printf '\033]2;NEBULA|%s|%s|%s\007' "$PWD" "$__nebula_branch" "$__nebula_prog"
}
trap '__nebula_preexec' DEBUG
case ";${PROMPT_COMMAND:-};" in
    *";__nebula_precmd;"*) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}__nebula_precmd" ;;
esac
"#;

/// Remote zsh integration, decoded into `$ZDOTDIR/.zshrc`. Sources the user's
/// real config first (their `$HOME` files, since we hijacked `$ZDOTDIR`), then
/// installs precmd/preexec hooks via zsh's native `add-zsh-hook`.
#[cfg(windows)]
const REMOTE_ZSH: &str = r#"
[ -f "$HOME/.zshenv" ] && source "$HOME/.zshenv"
[ -f "$HOME/.zshrc" ] && source "$HOME/.zshrc"
autoload -Uz add-zsh-hook 2>/dev/null
__nebula_branch=""
__nebula_precmd() {
    printf '\033]133;D\007'
    if command -v git >/dev/null 2>&1; then
        __nebula_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
    else
        __nebula_branch=""
    fi
    printf '\033]133;A\007'
    printf '\033]2;NEBULA|%s|%s|\007' "$PWD" "$__nebula_branch"
}
__nebula_preexec() {
    # $1 is the full command line zsh is about to run.
    local prog="${1%% *}"
    prog="${prog##*/}"
    printf '\033]133;C\007'
    printf '\033]2;NEBULA|%s|%s|%s\007' "$PWD" "$__nebula_branch" "$prog"
}
if command -v add-zsh-hook >/dev/null 2>&1; then
    add-zsh-hook precmd __nebula_precmd
    add-zsh-hook preexec __nebula_preexec
fi
"#;

/// ssh short options that consume a value (attached or as the next arg).
const VALUE_OPTS: &[u8] = b"bcDEeFIiJLlmOopQRSWw";
/// Standalone flags meaning "no interactive login session" (`-N` no remote
/// command, `-f` background, `-G`/`-V` queries, `-T` no tty, `-W` stdio
/// forward): never inject into, never worth auto-saving as a host.
const NO_SESSION_FLAGS: &[u8] = b"NnfGTVW";

/// How long a typed `ssh` session must live before OSC 133;D confirms it as
/// a real connection worth auto-saving in the sidebar (fast failures — DNS,
/// connection refused, an aborted auth prompt — die well under this).
pub const SAVE_MIN_SESSION: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a typed `ssh` session must live before mere PTY activity counts
/// as "connected" for the sidebar auto-save. The session-end path above kept
/// the sidebar empty exactly while the user was USING the connection; remote
/// output still arriving this far past launch means the login got beyond the
/// fast-failure window (DNS/refused/instant auth reject), so save right away.
/// A remote retitle or the `nebula ssh` NEBULA| title confirms sooner still.
pub const SAVE_CONNECTED_AFTER: std::time::Duration = std::time::Duration::from_secs(6);

/// Parse the destination out of a committed command line, when that line is
/// an *interactive* ssh login (`ssh host`, `ssh -p 2222 user@host`,
/// `nebula ssh host`). This is what the sidebar auto-save remembers: the
/// returned string is later handed back to `nebula ssh`, so `-l user` folds
/// into `user@host` and a non-default `-p` port becomes an `ssh://` URI
/// (the only port syntax the ssh CLI accepts as a destination).
///
/// `None` for everything else — non-ssh commands, forms without a login
/// session (`-N`/`-f`/`-G`/`-T`/`-V`/`-W`), or an explicit remote command
/// (`ssh host ls`): none of those confirm as "connected to this host".
pub fn ssh_destination(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    // Program identity, path/extension-normalized (`/usr/bin/ssh`,
    // `ssh.exe`); `nebula ssh …` counts too — same wrapper, same semantics.
    let mut program = crate::display::extract_program(tokens.next()?)?;
    if program == "nebula" {
        if tokens.next() != Some("ssh") {
            return None;
        }
        program = "ssh".to_owned();
    }
    if program != "ssh" {
        return None;
    }

    let (mut login_user, mut port) = (None, None);
    while let Some(tok) = tokens.next() {
        if tok == "--" {
            let dest = tokens.next()?;
            // A token after the destination is a remote command.
            return if tokens.next().is_some() {
                None
            } else {
                Some(format_destination(dest, login_user, port))
            };
        }
        let bytes = tok.as_bytes();
        if bytes.first() == Some(&b'-') && bytes.len() >= 2 {
            for (idx, &b) in bytes.iter().enumerate().skip(1) {
                if NO_SESSION_FLAGS.contains(&b) {
                    return None;
                }
                if VALUE_OPTS.contains(&b) {
                    // Attached value (`-p2222`) or the next token (`-p 2222`).
                    let attached = &tok[idx + 1..];
                    let value =
                        if attached.is_empty() { tokens.next() } else { Some(attached) };
                    match b {
                        b'l' => login_user = value,
                        b'p' => port = value,
                        _ => {},
                    }
                    break;
                }
            }
            continue;
        }
        // First bare token = destination; anything after = remote command.
        return if tokens.next().is_some() {
            None
        } else {
            Some(format_destination(tok, login_user, port))
        };
    }
    None
}

/// Fold `-l user` / `-p port` into a destination string that reconnects when
/// handed back to `nebula ssh` as a single argument.
fn format_destination(dest: &str, login_user: Option<&str>, port: Option<&str>) -> String {
    // An ssh:// URI typed by the user already carries everything.
    if dest.starts_with("ssh://") {
        return dest.to_owned();
    }
    let mut host = dest.to_owned();
    if let Some(user) = login_user {
        if !host.contains('@') {
            host = format!("{user}@{host}");
        }
    }
    match port {
        // A non-default port only round-trips as an ssh:// URI. Bare IPv6
        // destinations (they contain ':') can't take the suffix unambiguously
        // — keep the plain host and lose the port instead of corrupting it.
        Some(p) if p != "22" && !dest.contains(':') => format!("ssh://{host}:{p}"),
        _ => host,
    }
}

/// Locate the system `ssh`. Windows 10+ ships OpenSSH; prefer the known path,
/// fall back to whatever `ssh` is on `PATH`.
#[cfg(windows)]
fn find_ssh() -> String {
    if let Ok(sysroot) = std::env::var("SystemRoot") {
        let p = std::path::Path::new(&sysroot).join("System32").join("OpenSSH").join("ssh.exe");
        if p.exists() {
            return p.display().to_string();
        }
    }
    "ssh".to_owned()
}

/// How to invoke ssh. Some forms are *broken* by injecting a PTY + bootstrap
/// remote command, so they must run exactly as the user typed them.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum SshPlan {
    /// exec ssh with the user's args untouched — no `-t`, no bootstrap.
    Passthrough,
    /// Add `-t` (idempotent) and append the base64 launcher as the remote cmd.
    Inject,
}

/// First-pass verdict from the command line alone.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum CliVerdict {
    /// Definitely don't inject (broken form, query, or explicit remote cmd).
    Passthrough,
    /// CLI looks injectable, but settings resolved from `~/.ssh/config`
    /// (RequestTTY / SessionType / RemoteCommand) may still force passthrough.
    NeedsConfigCheck,
}

/// Classify the ssh command line. Passthrough when a form would be corrupted by
/// injecting an interactive PTY + bootstrap:
///   - `-N -n -f`  no remote command / background: no login shell to integrate
///   - `-W`        raw stdio forward (ProxyCommand channel) — a PTY destroys it
///   - `-G -T -V`  query / no-tty / version: no session
///   - an explicit remote command (`ssh host ls`)
///
/// `-J` / `-o ProxyJump` / `-o ProxyCommand` are deliberately NOT passthrough:
/// they only describe *how to reach* the destination — a normal login shell
/// still lands there, so bootstrap injection is valid (the bastion case).
#[cfg(windows)]
fn cli_verdict(args: &[String]) -> CliVerdict {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // `-- host [cmd]`: passthrough only if a command follows the host.
            return if args.len() > i + 2 {
                CliVerdict::Passthrough
            } else {
                CliVerdict::NeedsConfigCheck
            };
        }
        let bytes = a.as_bytes();
        if bytes.first() == Some(&b'-') && bytes.len() >= 2 {
            let mut consumes_next = false;
            for (idx, &b) in bytes.iter().enumerate().skip(1) {
                // Passthrough triggers are checked BEFORE the value-opt break so
                // `-W` (both a value-opt and a trigger) passes through, while a
                // stray 'W' inside an attached `-oProxyCommand=…W…` value is not
                // misread as a trigger (the leading `o` breaks the scan first).
                if NO_SESSION_FLAGS.contains(&b) {
                    return CliVerdict::Passthrough;
                }
                if VALUE_OPTS.contains(&b) {
                    // Rest of this token is the option's attached value; it eats
                    // the next arg only when the opt is the token's last char.
                    consumes_next = idx == bytes.len() - 1;
                    break;
                }
            }
            i += if consumes_next { 2 } else { 1 };
            continue;
        }
        // First bare token = destination; a token after it = remote command.
        return if i + 1 < args.len() {
            CliVerdict::Passthrough
        } else {
            CliVerdict::NeedsConfigCheck
        };
    }
    // No destination at all (`nebula ssh`, `nebula ssh -v`): nothing to inject
    // into — let ssh print its own usage / error.
    CliVerdict::Passthrough
}

/// Run `ssh -G <args>` (offline config resolution, never connects) and decide
/// whether the *resolved* settings force passthrough. Fails open to `false`
/// (inject) if ssh can't run or errors, so a config quirk never blocks a login.
#[cfg(windows)]
fn config_forces_passthrough(ssh: &str, args: &[String]) -> bool {
    let out = std::process::Command::new(ssh).arg("-G").args(args).output();
    match out {
        Ok(o) if o.status.success() => {
            parse_g_says_passthrough(&String::from_utf8_lossy(&o.stdout))
        },
        _ => false,
    }
}

/// Pure parser over `ssh -G` output (`keyword value`, keyword case-insensitive,
/// first value wins). Split out to be unit-testable without a real ssh.
#[cfg(windows)]
fn parse_g_says_passthrough(g_output: &str) -> bool {
    let (mut request_tty, mut session_type, mut remote_command) = (None, None, None);
    for line in g_output.lines() {
        let mut it = line.split_whitespace();
        let key = match it.next() {
            Some(k) => k.to_ascii_lowercase(),
            None => continue,
        };
        let rest = it.collect::<Vec<_>>().join(" ");
        match key.as_str() {
            "requesttty" if request_tty.is_none() => request_tty = Some(rest),
            "sessiontype" if session_type.is_none() => session_type = Some(rest),
            "remotecommand" if remote_command.is_none() => remote_command = Some(rest),
            _ => {},
        }
    }
    // -T equivalent: no PTY wanted.
    if request_tty.as_deref() == Some("no") {
        return true;
    }
    // -N (none) or a subsystem session: no interactive login shell.
    if matches!(session_type.as_deref(), Some("none") | Some("subsystem")) {
        return true;
    }
    // A remote command baked into config owns the session (anything but none).
    matches!(remote_command.as_deref(), Some(rc) if !rc.is_empty() && rc != "none")
}

/// POSIX-sh bootstrap, run as the remote command under the login shell. Decodes
/// whichever integration matches an available remote shell and execs it. Every
/// branch falls through to a plain login shell on failure, so a connection is
/// never lost to a bootstrap problem.
///
/// NOTE: this whole script is base64-wrapped by [`run`] and fed to the remote
/// via `echo <b64> | base64 -d | sh`, so `sh` reads it from a *pipe*, not a tty.
/// Every `exec` therefore reattaches stdin to `/dev/tty`, or the interactive
/// shell would inherit the exhausted pipe and exit immediately.
#[cfg(windows)]
fn build_bootstrap(bash_b64: &str, zsh_b64: &str) -> String {
    format!(
        "export NEBULA_SSH=1; \
         D=$(mktemp -d 2>/dev/null || echo /tmp/nebula-$$); mkdir -p \"$D\"; \
         if command -v base64 >/dev/null 2>&1; then \
           if command -v zsh >/dev/null 2>&1; then \
             printf %s '{zsh_b64}' | base64 -d > \"$D/.zshrc\" 2>/dev/null && \
             export ZDOTDIR=\"$D\" && exec zsh -i </dev/tty; \
           fi; \
           if command -v bash >/dev/null 2>&1; then \
             printf %s '{bash_b64}' | base64 -d > \"$D/bashrc\" 2>/dev/null && \
             exec bash --rcfile \"$D/bashrc\" -i </dev/tty; \
           fi; \
         fi; \
         exec \"${{SHELL:-/bin/sh}}\" -i </dev/tty"
    )
}

/// Decide the final plan: CLI verdict first (cheap, offline), then — only when
/// the CLI looks injectable — a `ssh -G` config probe to catch settings hidden
/// in `~/.ssh/config` (RequestTTY/SessionType/RemoteCommand).
#[cfg(windows)]
fn plan_for(ssh: &str, args: &[String]) -> SshPlan {
    match cli_verdict(args) {
        CliVerdict::Passthrough => SshPlan::Passthrough,
        CliVerdict::NeedsConfigCheck => {
            if config_forces_passthrough(ssh, args) {
                SshPlan::Passthrough
            } else {
                SshPlan::Inject
            }
        },
    }
}

/// Whether the args already request a remote PTY (`-t`/`-tt`, or `t` in a
/// cluster of non-value flags), so injection doesn't add a redundant `-t`.
#[cfg(windows)]
fn already_has_tty(args: &[String]) -> bool {
    args.iter().any(|a| a == "-t" || a == "-tt")
}

/// Concrete host aliases from the user's `~/.ssh/config`, in file order —
/// the source of the sidebar's "SSH HOSTS" section. Pattern entries (`*`,
/// `?`) and negations are skipped: they are match rules, not destinations
/// you can click to connect to. Missing/unreadable config → empty list
/// (the section simply doesn't render).
pub fn ssh_config_hosts() -> Vec<String> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let Ok(data) = std::fs::read_to_string(home.join(".ssh").join("config")) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for line in data.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("Host ")
            .or_else(|| line.strip_prefix("host "))
            .or_else(|| line.strip_prefix("Host\t"))
        else {
            continue;
        };
        for name in rest.split_whitespace() {
            if name.contains(['*', '?']) || name.starts_with('!') {
                continue;
            }
            if !hosts.iter().any(|h| h == name) {
                hosts.push(name.to_owned());
            }
        }
    }
    hosts
}

fn askpass_destination_from_args(args: &[String]) -> Option<String> {
    let (mut login_user, mut port) = (None, None);
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return args.get(i + 1).map(|dest| {
                format_destination(dest, login_user.as_deref(), port.as_deref())
            });
        }
        let bytes = arg.as_bytes();
        if bytes.first() == Some(&b'-') && bytes.len() >= 2 {
            let mut consumed_next = false;
            for (index, &option) in bytes.iter().enumerate().skip(1) {
                if VALUE_OPTS.contains(&option) {
                    let attached = &arg[index + 1..];
                    let value = if attached.is_empty() {
                        consumed_next = true;
                        args.get(i + 1).map(String::as_str)
                    } else {
                        Some(attached)
                    };
                    match option {
                        b'l' => login_user = value.map(ToOwned::to_owned),
                        b'p' => port = value.map(ToOwned::to_owned),
                        _ => {},
                    }
                    break;
                }
            }
            i += if consumed_next { 2 } else { 1 };
            continue;
        }
        return Some(format_destination(arg, login_user.as_deref(), port.as_deref()));
    }
    None
}

/// Per-pane data for launching SSH from the configured default shell. The
/// password stays behind OpenSSH's AskPass contract and never enters this
/// command or the terminal scrollback.
pub struct SshPaneLaunch {
    pub command: Vec<u8>,
}

pub fn build_pane_launch(
    shell_id: &str,
    exe: &std::path::Path,
    destination: &str,
) -> std::io::Result<SshPaneLaunch> {
    if !valid_destination(destination) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SSH destination contains unsafe shell characters",
        ));
    }
    let exe_text = exe.to_string_lossy();
    let shell = shell_id.trim().to_ascii_lowercase();
    let command = if matches!(shell.as_str(), "pwsh" | "powershell" | "ps") {
        format!(
            "& '{}' ssh -- '{}'\r",
            exe_text.replace('\'', "''"),
            destination.replace('\'', "''")
        )
    } else if shell == "cmd" {
        format!("\"{exe_text}\" ssh -- {destination}\r")
    } else if matches!(shell.as_str(), "bash" | "git-bash" | "gitbash") {
        format!(
            "'{}' ssh -- '{}'\r",
            git_bash_path(exe),
            destination.replace('\'', "'\\''")
        )
    } else if shell == "nu" {
        format!(
            "^'{}' ssh -- '{}'\r",
            exe_text.replace('\'', "''"),
            destination.replace('\'', "''")
        )
    } else if shell.starts_with("wsl:") || shell == "wsl" {
        format!(
            "'{}' ssh -- '{}'\r",
            wsl_path(exe),
            destination.replace('\'', "'\\''")
        )
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unsupported default shell id: {shell_id}"),
        ));
    };

    Ok(SshPaneLaunch { command: command.into_bytes() })
}

pub struct SshAskpassEnv {
    pub values: std::collections::HashMap<String, String>,
    pub attempt_path: std::path::PathBuf,
}

pub fn build_askpass_env(
    exe: &std::path::Path,
    destination: &str,
    session_id: u64,
) -> SshAskpassEnv {
    let attempt_path = std::env::temp_dir().join(format!(
        "nebula-ssh-askpass-{}-{session_id}.flag",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&attempt_path);
    let mut values = std::collections::HashMap::new();
    values.insert("SSH_ASKPASS".into(), exe.to_string_lossy().into_owned());
    values.insert("SSH_ASKPASS_REQUIRE".into(), "force".into());
    values.insert("DISPLAY".into(), "nebula".into());
    values.insert("NEBULA_SSH_ASKPASS".into(), "1".into());
    values.insert("NEBULA_SSH_DESTINATION".into(), destination.into());
    values.insert(
        "NEBULA_SSH_ASKPASS_ATTEMPT".into(),
        attempt_path.display().to_string(),
    );
    SshAskpassEnv { values, attempt_path }
}

fn valid_destination(destination: &str) -> bool {
    !destination.is_empty()
        && destination.chars().all(|ch| {
            !ch.is_control()
                && !ch.is_whitespace()
                && !matches!(ch, '\'' | '"' | '`' | '$' | '&' | '|' | ';' | '<' | '>' | '(' | ')')
        })
}

fn git_bash_path(path: &std::path::Path) -> String {
    slash_drive_path(path, "")
}

fn wsl_path(path: &std::path::Path) -> String {
    slash_drive_path(path, "/mnt")
}

fn slash_drive_path(path: &std::path::Path, prefix: &str) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let bytes = raw.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        format!("{prefix}/{drive}{}", &raw[2..])
    } else {
        raw
    }
}

/// `nebula ssh` entrypoint. Returns the process exit code.
#[cfg(windows)]
pub fn run(args: Vec<String>) -> i32 {
    use base64::Engine as _;
    use std::process::Command;

    let ssh = find_ssh();
    let mut cmd = Command::new(&ssh);
    let askpass = askpass_destination_from_args(&args).and_then(|destination| {
        std::env::current_exe().ok().map(|exe| {
            build_askpass_env(&exe, &destination, std::process::id() as u64)
        })
    });
    if let Some(context) = &askpass {
        cmd.envs(&context.values);
    }

    match plan_for(&ssh, &args) {
        SshPlan::Passthrough => {
            cmd.args(&args);
        },
        SshPlan::Inject => {
            let b64 = base64::engine::general_purpose::STANDARD;
            let bootstrap = build_bootstrap(&b64.encode(REMOTE_BASH), &b64.encode(REMOTE_ZSH));
            // The bootstrap contains quotes and `$` — passing it straight
            // through Windows `Command` arg-escaping → ssh.exe → the remote
            // login shell is a three-layer quoting minefield. Sidestep it:
            // base64 the whole script (alphabet is `A-Za-z0-9+/=`, zero shell
            // metacharacters) and send a trivially-portable one-liner every
            // remote shell (bash/zsh/sh/fish/csh) parses identically — just
            // echo, a pipe, and base64. The kitty/wezterm `ssh kitten` approach.
            let launcher = format!("echo {} | base64 -d | sh", b64.encode(&bootstrap));
            // -t forces a remote PTY so the shell is interactive (idempotent:
            // skipped if the user already asked for one). Order: `-t`, then the
            // user's args (with the destination), then the launcher last.
            if !already_has_tty(&args) {
                cmd.arg("-t");
            }
            cmd.args(&args);
            cmd.arg(launcher);
        },
    }

    let result = match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("nebula ssh: failed to launch ssh: {e}");
            1
        },
    };
    if let Some(context) = askpass {
        let _ = std::fs::remove_file(context.attempt_path);
    }
    result
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> CliVerdict {
        cli_verdict(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn cli_needs_check_for_plain_logins() {
        use CliVerdict::NeedsConfigCheck as N;
        assert_eq!(v(&["host"]), N);
        assert_eq!(v(&["user@host"]), N);
        assert_eq!(v(&["-p", "2222", "host"]), N);
        assert_eq!(v(&["-i", "key", "user@host"]), N);
        assert_eq!(v(&["-o", "ProxyJump=jump", "host"]), N); // bastion → inject
        assert_eq!(v(&["-J", "jump", "host"]), N);
        assert_eq!(v(&["-o", "RequestTTY=no", "host"]), N); // resolved by -G layer
        assert_eq!(v(&["--", "host"]), N);
    }

    #[test]
    fn cli_passthrough_for_broken_forms() {
        use CliVerdict::Passthrough as P;
        assert_eq!(v(&["host", "bash"]), P); // explicit remote command
        assert_eq!(v(&["--", "host", "ls"]), P);
        assert_eq!(v(&["-N", "-L", "8080:localhost:80", "host"]), P);
        assert_eq!(v(&["-fN", "-L", "8080:localhost:80", "host"]), P);
        assert_eq!(v(&["-W", "target:22", "jump"]), P); // stdio forward
        assert_eq!(v(&["-G", "host"]), P);
        assert_eq!(v(&["-T", "git@github.com"]), P);
        assert_eq!(v(&["-V"]), P);
        assert_eq!(v(&["-tN", "host"]), P); // -N wins over -t
        assert_eq!(v(&[]), P); // no destination
        assert_eq!(v(&["-v"]), P); // no destination
    }

    #[test]
    fn g_output_forces_passthrough() {
        // RequestTTY=no → passthrough.
        assert!(parse_g_says_passthrough("requesttty no\nsessiontype default\n"));
        // SessionType none / subsystem → passthrough.
        assert!(parse_g_says_passthrough("sessiontype none\n"));
        assert!(parse_g_says_passthrough("sessiontype subsystem\n"));
        // RemoteCommand baked into config → passthrough.
        assert!(parse_g_says_passthrough("remotecommand tmux attach\n"));
    }

    #[test]
    fn g_output_allows_inject() {
        // A normal interactive login: none of the triggers fire.
        let normal = "requesttty auto\nsessiontype default\nremotecommand none\n";
        assert!(!parse_g_says_passthrough(normal));
        assert!(!parse_g_says_passthrough("")); // empty = inject
        // Case-insensitive keyword, force TTY is fine to inject into.
        assert!(!parse_g_says_passthrough("RequestTTY force\n"));
    }

    #[test]
    fn destination_parsed_from_typed_logins() {
        let d = ssh_destination;
        assert_eq!(d("ssh host"), Some("host".into()));
        assert_eq!(d("ssh user@host"), Some("user@host".into()));
        assert_eq!(d("  ssh.exe   user@host  "), Some("user@host".into()));
        assert_eq!(d("/usr/bin/ssh host"), Some("host".into()));
        assert_eq!(d("nebula ssh host"), Some("host".into()));
        assert_eq!(d("ssh -i key -t user@host"), Some("user@host".into()));
        assert_eq!(d("ssh -J jump user@host"), Some("user@host".into()));
        assert_eq!(d("ssh -- host"), Some("host".into()));
        assert_eq!(d("ssh ssh://user@host:2200"), Some("ssh://user@host:2200".into()));
        // -l / -p fold into a reconnectable single-token destination.
        assert_eq!(d("ssh -l root host"), Some("root@host".into()));
        assert_eq!(d("ssh -lroot host"), Some("root@host".into()));
        assert_eq!(d("ssh -p 2222 host"), Some("ssh://host:2222".into()));
        assert_eq!(d("ssh -p2222 user@host"), Some("ssh://user@host:2222".into()));
        assert_eq!(d("ssh -p 22 host"), Some("host".into())); // default port
        // Bare IPv6 can't take an unambiguous port suffix: host survives.
        assert_eq!(d("ssh -p 2222 ::1"), Some("::1".into()));
        // -l never overrides an explicit user@.
        assert_eq!(d("ssh -l root admin@host"), Some("admin@host".into()));
    }

    #[test]
    fn destination_rejected_for_non_logins() {
        let d = ssh_destination;
        assert_eq!(d(""), None);
        assert_eq!(d("vim notes.md"), None);
        assert_eq!(d("nebula run x"), None);
        assert_eq!(d("ssh"), None); // no destination
        assert_eq!(d("ssh -p 2222"), None);
        assert_eq!(d("ssh host ls -la"), None); // explicit remote command
        assert_eq!(d("ssh -- host ls"), None);
        assert_eq!(d("ssh -G host"), None); // query / no-session forms
        assert_eq!(d("ssh -T git@github.com"), None);
        assert_eq!(d("ssh -N -L 8080:localhost:80 host"), None);
        assert_eq!(d("ssh -fN host"), None);
        assert_eq!(d("ssh -W target:22 jump"), None);
        assert_eq!(d("ssh -V"), None);
    }

    #[test]
    fn ssh_pane_launch_uses_shell_specific_quoting() {
        let exe = std::path::Path::new(r"C:\Program Files\Nebula\nebula.exe");
        let ps = build_pane_launch("pwsh", exe, "root@example.com").unwrap();
        assert_eq!(String::from_utf8(ps.command).unwrap(),
            "& 'C:\\Program Files\\Nebula\\nebula.exe' ssh -- 'root@example.com'\r");

        let cmd = build_pane_launch("cmd", exe, "root@example.com").unwrap();
        assert_eq!(String::from_utf8(cmd.command).unwrap(),
            "\"C:\\Program Files\\Nebula\\nebula.exe\" ssh -- root@example.com\r");

        let bash = build_pane_launch("git-bash", exe, "root@example.com").unwrap();
        assert_eq!(String::from_utf8(bash.command).unwrap(),
            "'/c/Program Files/Nebula/nebula.exe' ssh -- 'root@example.com'\r");

        let nu = build_pane_launch("nu", exe, "root@example.com").unwrap();
        assert_eq!(String::from_utf8(nu.command).unwrap(),
            "^'C:\\Program Files\\Nebula\\nebula.exe' ssh -- 'root@example.com'\r");

        let wsl = build_pane_launch("wsl:Ubuntu", exe, "root@example.com").unwrap();
        assert_eq!(String::from_utf8(wsl.command).unwrap(),
            "'/mnt/c/Program Files/Nebula/nebula.exe' ssh -- 'root@example.com'\r");
    }

    #[test]
    fn ssh_pane_launch_sets_askpass_environment() {
        let launch = build_askpass_env(
            std::path::Path::new(r"C:\Nebula\nebula.exe"),
            "alice@example.com",
            42,
        );
        assert_eq!(launch.values.get("SSH_ASKPASS").map(String::as_str), Some(r"C:\Nebula\nebula.exe"));
        assert_eq!(launch.values.get("SSH_ASKPASS_REQUIRE").map(String::as_str), Some("force"));
        assert_eq!(launch.values.get("NEBULA_SSH_ASKPASS").map(String::as_str), Some("1"));
        assert_eq!(launch.values.get("NEBULA_SSH_DESTINATION").map(String::as_str), Some("alice@example.com"));
        assert!(launch.values["NEBULA_SSH_ASKPASS_ATTEMPT"].contains("42"));
    }

    #[test]
    fn ssh_pane_launch_rejects_shell_injection() {
        let exe = std::path::Path::new(r"C:\Nebula\nebula.exe");
        for bad in ["host name", "host\nwhoami", "host;whoami", "host&whoami", "'host'"] {
            assert!(build_pane_launch("pwsh", exe, bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn askpass_destination_is_parsed_from_forwarded_argv() {
        let parse = |args: &[&str]| {
            askpass_destination_from_args(&args.iter().map(|x| x.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(parse(&["--", "host"]), Some("host".into()));
        assert_eq!(parse(&["-l", "root", "host"]), Some("root@host".into()));
        assert_eq!(parse(&["-p", "2222", "user@host"]), Some("ssh://user@host:2222".into()));
        assert_eq!(parse(&["-i", "key file", "host"]), Some("host".into()));
        assert_eq!(parse(&["-V"]), None);
    }
}
