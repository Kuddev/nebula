//! Installed-shell detection for the new-tab dropdown (Windows Terminal's
//! profile menu). Ported from Tabby's detector set (tabby-electron/src/shells)
//! so the menu lists what's actually installed, in a stable, familiar order:
//! PowerShell 7 → Windows PowerShell → CMD → Git Bash → Nushell → WSL distros.
//!
//! Detection touches the filesystem and the registry, so callers run it ONCE
//! per process (at first menu open) and cache the result — see
//! `Display::nebula_detected_shells`.
//!
//! Every entry also carries a stable `id` for the "default shell" setting
//! (`shell=<id>` in nebula_settings.txt): ids re-resolve to fresh paths on
//! each boot, so an updated Git or a moved WSL distro never strands the
//! setting. `powershell` and `bash` keep their historic meaning (the PTY layer
//! attaches its prompt/OSC bootstrap to those two), which is why detection
//! reuses their ids instead of minting path-based ones.

use std::path::PathBuf;

/// One launchable shell for the dropdown / default-shell setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedShell {
    /// Menu label, e.g. "PowerShell 7", "WSL · Ubuntu".
    pub name: String,
    /// Stable settings id, e.g. "pwsh", "cmd", "wsl:Ubuntu".
    pub id: String,
    /// Program to spawn (absolute where detection knows it).
    pub program: String,
    /// Arguments passed to the program.
    pub args: Vec<String>,
}

impl DetectedShell {
    pub fn shell(&self) -> nebula_terminal::tty::Shell {
        nebula_terminal::tty::Shell::new(self.program.clone(), self.args.clone())
    }

    /// A Nerd Font glyph for the menu/tab row, keyed off the stable id. All
    /// code points live in the Maple Mono NF bundle (same set the prompt and
    /// chrome already draw), so none render as tofu.
    pub fn icon(&self) -> &'static str {
        icon_for_id(&self.id)
    }
}

/// Nerd Font glyph for a shell id — shared by detected shells and the settings
/// row so a saved `shell=<id>` always draws the same mark. WSL distro ids carry
/// a `wsl:` prefix; everything else matches whole.
pub fn icon_for_id(id: &str) -> &'static str {
    let id = id.to_ascii_lowercase();
    if id.starts_with("wsl") {
        return "\u{f17c}"; // Linux/Tux (WSL distros)
    }
    match id.as_str() {
        "pwsh" | "powershell" => "\u{ebc7}", // codicon terminal-powershell
        "cmd" => "\u{ebc4}",                 // codicon terminal-cmd
        "bash" | "git-bash" | "gitbash" => "\u{e795}", // devicon bash/terminal
        "nu" => "\u{f489}",                  // generic terminal glyph
        _ => "\u{ea85}",                     // codicon terminal (fallback)
    }
}

/// Full-color brand icon (embedded PNG, rasterized from Tabby's SVGs with a
/// 12% safe margin) for a shell id — the terminal picker draws this textured
/// quad instead of the flat Nerd Font glyph. WSL distro ids map by distro
/// name (`wsl:Ubuntu` → the Ubuntu roundel), falling back to the generic
/// Tux for unknown distros. `None` = no brand asset; caller keeps the glyph.
pub fn color_icon_png(id: &str) -> Option<&'static [u8]> {
    let lower = id.to_ascii_lowercase();
    if let Some(distro) = lower.strip_prefix("wsl:") {
        // Match the distro family in its name (registry names vary:
        // "Ubuntu-22.04", "kali-linux", "openSUSE-Tumbleweed").
        let asset = if distro.contains("ubuntu") {
            ICON_UBUNTU
        } else if distro.contains("debian") {
            ICON_DEBIAN
        } else if distro.contains("kali") {
            ICON_KALI
        } else if distro.contains("alpine") {
            ICON_ALPINE
        } else if distro.contains("suse") {
            ICON_SUSE
        } else if distro.contains("alma") {
            ICON_ALMA
        } else if distro.contains("oracle") {
            ICON_ORACLE
        } else if distro.contains("euler") {
            ICON_EULER
        } else {
            ICON_LINUX
        };
        return Some(asset);
    }
    Some(match lower.as_str() {
        "pwsh" => ICON_PWSH,
        "powershell" => ICON_POWERSHELL,
        "cmd" => ICON_CMD,
        "bash" | "git-bash" | "gitbash" => ICON_GIT_BASH,
        "nu" => ICON_NUSHELL,
        "wsl" => ICON_LINUX, // legacy id (no distro name)
        _ => return None,
    })
}

const ICON_PWSH: &[u8] = include_bytes!("../../extra/shell-icons/powershell-core.png");
const ICON_POWERSHELL: &[u8] = include_bytes!("../../extra/shell-icons/powershell.png");
const ICON_CMD: &[u8] = include_bytes!("../../extra/shell-icons/cmd.png");
const ICON_GIT_BASH: &[u8] = include_bytes!("../../extra/shell-icons/git-bash.png");
const ICON_NUSHELL: &[u8] = include_bytes!("../../extra/shell-icons/nushell.png");
const ICON_LINUX: &[u8] = include_bytes!("../../extra/shell-icons/linux.png");
const ICON_UBUNTU: &[u8] = include_bytes!("../../extra/shell-icons/ubuntu.png");
const ICON_DEBIAN: &[u8] = include_bytes!("../../extra/shell-icons/debian.png");
const ICON_KALI: &[u8] = include_bytes!("../../extra/shell-icons/kali.png");
const ICON_ALPINE: &[u8] = include_bytes!("../../extra/shell-icons/alpine.png");
const ICON_SUSE: &[u8] = include_bytes!("../../extra/shell-icons/suse.png");
const ICON_ALMA: &[u8] = include_bytes!("../../extra/shell-icons/alma.png");
const ICON_ORACLE: &[u8] = include_bytes!("../../extra/shell-icons/oracle-linux.png");
const ICON_EULER: &[u8] = include_bytes!("../../extra/shell-icons/open-euler.png");

/// Human label for a saved shell id, without touching the filesystem — the
/// settings row redraws every frame, so it can't afford `detect_shells`.
/// Mirrors the names detection produces; unknown ids show verbatim.
pub fn display_name_for_id(id: &str) -> String {
    let trimmed = id.trim();
    if let Some(distro) = trimmed.strip_prefix("wsl:") {
        return format!("WSL · {distro}");
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" | "ps" => "PowerShell".into(),
        "cmd" => "CMD".into(),
        "bash" | "git-bash" | "gitbash" => "Git Bash".into(),
        "nu" => "Nushell".into(),
        _ => trimmed.to_owned(),
    }
}

fn existing(path: PathBuf) -> Option<String> {
    path.is_file().then(|| path.display().to_string())
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

/// Detect every installed shell, menu order. Non-Windows builds return an
/// empty list (the dropdown then shows only config profiles).
pub fn detect_shells() -> Vec<DetectedShell> {
    #[cfg(windows)]
    {
        detect_windows()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Resolve a settings id (`shell=<id>`) back to a launchable shell. Ids that
/// name the two PTY-integrated executors (`powershell`/`bash` families) return
/// `None` — the PTY layer owns those spawns and their prompt bootstrap.
pub fn resolve_id(id: &str) -> Option<DetectedShell> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let lower = id.to_ascii_lowercase();
    if is_pty_integrated_id(&lower) {
        return None;
    }
    detect_shells().into_iter().find(|shell| shell.id.eq_ignore_ascii_case(id))
}

pub fn is_pty_integrated_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "powershell" | "ps" | "bash" | "git-bash" | "gitbash"
    )
}

#[cfg(windows)]
fn detect_windows() -> Vec<DetectedShell> {
    let mut shells = Vec::new();

    // PowerShell 7+ (pwsh). App Paths registration first (Tabby's source of
    // truth), then the well-known installs. `-NoLogo` mirrors Tabby/WT.
    if let Some(program) = find_pwsh() {
        shells.push(DetectedShell {
            name: "PowerShell 7".into(),
            id: "pwsh".into(),
            program,
            args: vec!["-NoLogo".into()],
        });
    }

    // Windows PowerShell 5.1 — always present on Windows. Kept under the
    // historic `powershell` id so the PTY layer's prompt bootstrap applies.
    if let Some(program) = env_path("SystemRoot")
        .and_then(|root| existing(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")))
    {
        shells.push(DetectedShell {
            name: "Windows PowerShell".into(),
            id: "powershell".into(),
            program,
            args: vec!["-NoLogo".into()],
        });
    }

    // CMD. Absolute path (not bare "cmd.exe") so the row shows where it lives.
    if let Some(program) =
        env_path("SystemRoot").and_then(|root| existing(root.join(r"System32\cmd.exe")))
    {
        shells.push(DetectedShell {
            name: "命令提示符 CMD".into(),
            id: "cmd".into(),
            program,
            args: Vec::new(),
        });
    }

    // Git Bash — registry install path first (Tabby), then well-known dirs.
    // Kept under the historic `bash` id: the PTY layer injects the Nebula
    // rcfile (OSC 7 cwd / prompt contract) on this id.
    if let Some(program) = find_git_bash() {
        shells.push(DetectedShell {
            name: "Git Bash".into(),
            id: "bash".into(),
            program,
            args: vec!["--login".into(), "-i".into()],
        });
    }

    // Nushell — the user's WT shows it; WT itself only lists it via fragments.
    if let Some(program) = find_nushell() {
        shells.push(DetectedShell {
            name: "Nushell".into(),
            id: "nu".into(),
            program,
            args: Vec::new(),
        });
    }

    // WSL distributions, one entry each (Tabby enumerates Lxss). Hidden
    // plumbing distros (docker-desktop*) are skipped like Windows Terminal.
    shells.extend(find_wsl_distros());

    shells
}

#[cfg(windows)]
fn find_pwsh() -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let app_paths = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\pwsh.exe")
        .and_then(|key| key.get_value::<String, _>(""))
        .ok()
        .map(PathBuf::from)
        .and_then(existing);
    if app_paths.is_some() {
        return app_paths;
    }

    if let Some(path) =
        env_path("ProgramFiles").and_then(|root| existing(root.join(r"PowerShell\7\pwsh.exe")))
    {
        return Some(path);
    }
    // Store install exposes an execution alias under WindowsApps.
    env_path("LOCALAPPDATA")
        .and_then(|root| existing(root.join(r"Microsoft\WindowsApps\pwsh.exe")))
}

#[cfg(windows)]
fn find_git_bash() -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    // HKLM then HKCU InstallPath, exactly Tabby's lookup order.
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        if let Some(path) = RegKey::predef(hive)
            .open_subkey(r"Software\GitForWindows")
            .and_then(|key| key.get_value::<String, _>("InstallPath"))
            .ok()
            .map(|install| PathBuf::from(install).join(r"bin\bash.exe"))
            .and_then(existing)
        {
            return Some(path);
        }
    }

    // Well-known directories (mirrors the PTY layer's own bash lookup).
    for candidate in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if let Some(path) = existing(PathBuf::from(candidate)) {
            return Some(path);
        }
    }
    for root in ["LOCALAPPDATA", "USERPROFILE"].into_iter().filter_map(env_path) {
        for candidate in [
            root.join(r"Programs\Git\bin\bash.exe"),
            root.join(r"scoop\apps\git\current\bin\bash.exe"),
        ] {
            if let Some(path) = existing(candidate) {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(windows)]
fn find_nushell() -> Option<String> {
    for root in ["ProgramFiles", "LOCALAPPDATA", "USERPROFILE"].into_iter().filter_map(env_path) {
        for candidate in [
            root.join(r"nu\bin\nu.exe"),
            root.join(r"Programs\nu\bin\nu.exe"),
            root.join(r"scoop\apps\nu\current\nu.exe"),
        ] {
            if let Some(path) = existing(candidate) {
                return Some(path);
            }
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join("nu.exe")).find_map(existing)
    })
}

#[cfg(windows)]
fn find_wsl_distros() -> Vec<DetectedShell> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let wsl_exe = match env_path("SystemRoot")
        .and_then(|root| existing(root.join(r"System32\wsl.exe")))
    {
        Some(path) => path,
        None => return Vec::new(),
    };

    let lxss = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Lxss")
    {
        Ok(key) => key,
        // WSL installed but no registered distro: offer the default entry
        // only when the legacy bash.exe shim exists (Tabby's fallback).
        Err(_) => {
            return env_path("SystemRoot")
                .and_then(|root| existing(root.join(r"System32\bash.exe")))
                .map(|_| {
                    vec![DetectedShell {
                        name: "WSL".into(),
                        id: "wsl".into(),
                        program: wsl_exe,
                        args: Vec::new(),
                    }]
                })
                .unwrap_or_default();
        },
    };

    let mut distros = Vec::new();
    for guid in lxss.enum_keys().flatten() {
        let Ok(sub) = lxss.open_subkey(&guid) else { continue };
        let Ok(name) = sub.get_value::<String, _>("DistributionName") else { continue };
        // Plumbing distros are not user shells (same skip list as WT).
        if name.starts_with("docker-desktop") {
            continue;
        }
        distros.push(DetectedShell {
            name: format!("WSL · {name}"),
            id: format!("wsl:{name}"),
            program: wsl_exe.clone(),
            args: vec!["-d".into(), name],
        });
    }
    distros.sort_by(|a, b| a.name.cmp(&b.name));
    distros
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_pty_integrated_ids() {
        // These two ids belong to the PTY layer's executor bootstrap; the
        // resolver must never shadow them with a raw spawn.
        assert_eq!(resolve_id("powershell"), None);
        assert_eq!(resolve_id("bash"), None);
        assert_eq!(resolve_id(""), None);
    }

    #[test]
    fn powershell_seven_is_not_the_windows_powershell_integration() {
        assert!(is_pty_integrated_id("powershell"));
        assert!(is_pty_integrated_id("bash"));
        assert!(!is_pty_integrated_id("pwsh"));
        assert!(!is_pty_integrated_id("cmd"));
        assert!(!is_pty_integrated_id("nu"));
        assert!(!is_pty_integrated_id("wsl:Ubuntu"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_detection_finds_the_builtins() {
        let shells = detect_shells();
        // Every Windows box has Windows PowerShell and CMD.
        assert!(shells.iter().any(|s| s.id == "powershell"));
        assert!(shells.iter().any(|s| s.id == "cmd"));
        // Ids are unique — the settings roundtrip depends on it.
        let mut ids: Vec<_> = shells.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), shells.len());
    }
}
