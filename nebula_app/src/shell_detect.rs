//! Installed-shell detection for the new-tab dropdown's profile menu.
//!
//! Third-party provenance for this module and its icon assets is recorded
//! in THIRD-PARTY-NOTICES at the repository root. The menu lists what's
//! actually installed, in a stable, familiar order:
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

/// 检测到的 shell 如何提供 tab 图标、转圈和命令完成状态所依赖的语义生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellIntegration {
    PowerShell,
    Bash,
    /// Nushell 原生发 OSC 133；不能替换 config，否则会连带覆盖内置/外部
    /// completer、menus 与 keybindings。
    NativeOsc133,
    /// WSL 来宾：宿主端只见 wsl.exe，本地三条识别通道全盲。来宾侧是标准
    /// POSIX 环境，与 SSH 远端同构——把 `nebula ssh` 的远端 bash 集成脚本
    /// （OSC 133 生命周期 + `NEBULA|cwd|branch|program` 标题，git 分支来自
    /// 脚本内的 `git rev-parse`）经 automount 路径喂给来宾 bash 即可。
    WslBash,
    Unsupported,
}

impl DetectedShell {
    pub fn shell(&self) -> nebula_terminal::tty::Shell {
        #[cfg(windows)]
        match self.integration() {
            ShellIntegration::PowerShell => {
                return nebula_terminal::tty::powershell_with_nebula_integration(
                    self.program.clone(),
                    self.args.clone(),
                );
            },
            ShellIntegration::Bash => {
                return nebula_terminal::tty::bash_with_nebula_integration(
                    self.program.clone(),
                    self.args.clone(),
                );
            },
            ShellIntegration::WslBash => {
                if let Some(shell) = wsl_bash_with_nebula_integration(&self.program, &self.args) {
                    return shell;
                }
                // rcfile 落不了盘（temp 目录异常等）：裸拉起，行为同注入前。
            },
            ShellIntegration::NativeOsc133 | ShellIntegration::Unsupported => {},
        }

        nebula_terminal::tty::Shell::new(self.program.clone(), self.args.clone())
    }

    fn integration(&self) -> ShellIntegration {
        let id = self.id.trim().to_ascii_lowercase();
        if id == "wsl" || id.starts_with("wsl:") {
            return ShellIntegration::WslBash;
        }
        match id.as_str() {
            "powershell" | "pwsh" => ShellIntegration::PowerShell,
            "bash" | "git-bash" | "gitbash" => ShellIntegration::Bash,
            "nu" => ShellIntegration::NativeOsc133,
            // CMD 没有可靠的原生 pre-exec hook。
            _ => ShellIntegration::Unsupported,
        }
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
    let id = profile_shell_id(id).unwrap_or(id).to_ascii_lowercase();
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

/// Full-color brand icon (embedded PNG, rasterized from vector artwork with
/// a 12% safe margin; see THIRD-PARTY-NOTICES) for a shell id — the terminal picker draws this textured
/// quad instead of the flat Nerd Font glyph. WSL distro ids map by distro
/// name (`wsl:Ubuntu` → the Ubuntu roundel), falling back to the generic
/// Tux for unknown distros. `None` = no brand asset; caller keeps the glyph.
pub fn color_icon_png(id: &str) -> Option<&'static [u8]> {
    let lower = profile_shell_id(id).unwrap_or(id).to_ascii_lowercase();
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

/// Imported profiles persist their shell family and store id together as
/// `profile:<shell-id>|<profile-id>`. Rendering only needs the family; keeping
/// this parser here makes every icon consumer agree on that representation.
fn profile_shell_id(id: &str) -> Option<&str> {
    id.strip_prefix("profile:")
        .or_else(|| id.strip_prefix("PROFILE:"))
        .and_then(|value| value.split_once('|').map(|(shell, _)| shell))
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

    // PowerShell 7+ (pwsh). App Paths registration first (the authoritative
    // source), then the well-known installs. `-NoLogo` is the usual default.
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

    // Git Bash — registry install path first, then well-known dirs.
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

    // Nushell — installed per-user; only ever listed via fragment files.
    if let Some(program) = find_nushell() {
        shells.push(DetectedShell {
            name: "Nushell".into(),
            id: "nu".into(),
            program,
            args: Vec::new(),
        });
    }

    // WSL distributions, one entry each (enumerated from Lxss). Hidden
    // plumbing distros (docker-desktop*) are skipped — they aren't shells.
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
    env_path("LOCALAPPDATA").and_then(|root| existing(root.join(r"Microsoft\WindowsApps\pwsh.exe")))
}

#[cfg(windows)]
fn find_git_bash() -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    // HKLM then HKCU InstallPath, in that order.
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
    for candidate in
        [r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"]
    {
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

/// 静默 tab 行右侧的 shell 短标：`wsl:Ubuntu` /「WSL · Ubuntu」→ `ubuntu`、
/// 「PowerShell 7」/ `pwsh` → `pwsh`……口径按人叫得出的短名，不按 exe 名。
/// 吃 shell 的 settings id 或菜单显示名都行——两条管道喂进来的是哪种取决
/// 于 tab 的启动方式，短标必须两头一致。
pub fn shell_short_tag(name_or_id: &str) -> String {
    let raw = name_or_id.trim();
    if let Some(distro) = raw.strip_prefix("wsl:") {
        return distro.to_ascii_lowercase();
    }
    // 菜单显示名「WSL · Ubuntu」：发行版名就是短标。
    if let Some((_, distro)) = raw.split_once('·') {
        return distro.trim().to_ascii_lowercase();
    }
    let lower = raw.to_ascii_lowercase();
    // 两个 PowerShell 各有稳定 id，先精确匹配 id 再看显示名：同一台 shell
    // 经 `TabLaunch::Default`（喂 id）和 `TabLaunch::Shell`（喂菜单名）两条
    // 管道进来，必须标成同一个词，否则「新建的 tab」和「恢复出来的 tab」
    // 明明是同一个 shell 却挂着两个短标。
    match lower.as_str() {
        "pwsh" => return "pwsh".into(),
        "powershell" | "ps" => return "ps".into(),
        _ => {},
    }
    if lower.contains("powershell") {
        // 用户口头就用 pwsh 指 7、ps 指系统自带的 5.1；两者都是缩写，且
        // 同时装着两版时在侧栏一眼分得开。
        return if lower.contains("windows") { "ps".into() } else { "pwsh".into() };
    }
    if lower.contains("bash") {
        return "bash".into();
    }
    if lower.contains("nushell") {
        return "nu".into();
    }
    if lower.contains("cmd") || lower.contains("命令提示符") {
        return "cmd".into();
    }
    // 未知的取首词小写、封顶 10 字符——这是注脚，不是第二个标签名。
    lower.split_whitespace().next().unwrap_or("").chars().take(10).collect()
}

/// 已注册 WSL 发行版的名字（注册表 `Lxss` 各子键的 `DistributionName`），
/// 字母序。目录选择器用它把 `\\wsl.localhost\<名>` 钉进侧栏——系统的
/// 「Linux」导航节点不是每台 Win11 都有（issue #12 的现场就没有），钉进
/// 去的入口才保证在。跳过 docker-desktop 等管道发行版，口径同 shell 菜单。
#[cfg(windows)]
pub fn wsl_distro_names() -> Vec<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let Ok(lxss) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Lxss")
    else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for guid in lxss.enum_keys().flatten() {
        let Ok(sub) = lxss.open_subkey(&guid) else { continue };
        let Ok(name) = sub.get_value::<String, _>("DistributionName") else { continue };
        if name.starts_with("docker-desktop") {
            continue;
        }
        names.push(name);
    }
    names.sort();
    names
}

#[cfg(windows)]
fn find_wsl_distros() -> Vec<DetectedShell> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let wsl_exe =
        match env_path("SystemRoot").and_then(|root| existing(root.join(r"System32\wsl.exe"))) {
            Some(path) => path,
            None => return Vec::new(),
        };

    let lxss = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Lxss")
    {
        Ok(key) => key,
        // WSL installed but no registered distro: offer the default entry
        // only when the legacy bash.exe shim exists.
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
        // Plumbing distros are not user shells.
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

/// WSL 来宾 shell 集成：`wsl [-d X] --exec bash --rcfile /mnt/<盘>/…`。
///
/// - `--exec` 绕过来宾默认 shell 直接 exec，参数按 argv 原样传递（不经来宾
///   shell 再解释，宿主路径带空格也安全）。
/// - v1 范围与 `nebula ssh` 相同：bash。脚本先 source `~/.bashrc`，bash
///   用户无感；默认 shell 是 zsh/fish 的来宾会换到 bash——代价与远端注入
///   一致，换来 cwd/git 分支/命令转圈全通。
/// - 依赖 automount 默认根 `/mnt`（`wsl.conf` 改根的场景 rcfile 打不开，
///   bash 落到无 rc 的交互会话，仍可用）。
#[cfg(windows)]
fn wsl_bash_with_nebula_integration(
    program: &str,
    args: &[String],
) -> Option<nebula_terminal::tty::Shell> {
    let rcfile = wsl_integration_rcfile()?;
    let mut args = args.to_vec();
    args.extend([
        "--exec".to_owned(),
        "bash".to_owned(),
        "--rcfile".to_owned(),
        crate::ssh::wsl_path(&rcfile),
    ]);
    Some(nebula_terminal::tty::Shell::new(program.to_owned(), args))
}

/// 集成 rcfile 落盘（进程内写一次，所有 WSL tab 复用同一份）；内容与
/// `nebula ssh` 注入远端的脚本完全同源（`ssh::REMOTE_BASH`）。
#[cfg(windows)]
fn wsl_integration_rcfile() -> Option<PathBuf> {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = std::env::temp_dir().join("nebula-wsl-integration.bashrc");
        // 脚本常量本身就是 LF 行尾；bash 读 CRLF rcfile 会把 \r 当内容。
        std::fs::write(&path, crate::ssh::REMOTE_BASH.as_bytes()).ok()?;
        Some(path)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    fn detected(id: &str, program: &str, args: &[&str]) -> super::DetectedShell {
        super::DetectedShell {
            name: id.to_owned(),
            id: id.to_owned(),
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    #[test]
    fn shell_short_tags_read_like_what_people_call_them() {
        use super::shell_short_tag;
        // 两条管道各自的形态都要认：settings id 与菜单显示名。
        assert_eq!(shell_short_tag("wsl:Ubuntu-24.04"), "ubuntu-24.04");
        assert_eq!(shell_short_tag("WSL · Debian"), "debian");
        assert_eq!(shell_short_tag("PowerShell 7"), "pwsh");
        assert_eq!(shell_short_tag("pwsh"), "pwsh");
        assert_eq!(shell_short_tag("Windows PowerShell"), "ps");
        assert_eq!(shell_short_tag("Git Bash"), "bash");
        assert_eq!(shell_short_tag("Nushell"), "nu");
        assert_eq!(shell_short_tag("CMD"), "cmd");
    }

    /// 同一台 shell 的 settings id 与菜单显示名必须给出同一个短标。
    /// 回归锁：`powershell`(id) 曾标成 `pwsh` 而「Windows PowerShell」(名)
    /// 标成 `powershell`——同一台机器因为 tab 的启动方式不同挂了两个词，
    /// 而其中一个还不是缩写。
    #[test]
    fn shell_short_tags_agree_across_id_and_display_name() {
        use super::{display_name_for_id, shell_short_tag};
        for id in ["pwsh", "powershell", "cmd", "bash", "nu", "wsl:Ubuntu"] {
            assert_eq!(
                shell_short_tag(id),
                shell_short_tag(&display_name_for_id(id)),
                "id `{id}` 与它的显示名短标不一致"
            );
        }
        // 两个 PowerShell 仍然分得开。
        assert_ne!(shell_short_tag("powershell"), shell_short_tag("pwsh"));
    }

    use super::*;

    #[test]
    fn shell_integration_is_selected_by_stable_shell_id() {
        assert_eq!(
            detected("powershell", "powershell.exe", &[]).integration(),
            ShellIntegration::PowerShell
        );
        assert_eq!(detected("pwsh", "pwsh.exe", &[]).integration(), ShellIntegration::PowerShell);
        assert_eq!(detected("bash", "bash.exe", &[]).integration(), ShellIntegration::Bash);
        assert_eq!(detected("nu", "nu.exe", &[]).integration(), ShellIntegration::NativeOsc133);
        assert_eq!(detected("cmd", "cmd.exe", &[]).integration(), ShellIntegration::Unsupported);
        // WSL 来宾走 bash rcfile 注入（`nebula ssh` 远端同款脚本）。
        assert_eq!(
            detected("wsl:Ubuntu", "wsl.exe", &[]).integration(),
            ShellIntegration::WslBash
        );
        assert_eq!(detected("wsl", "wsl.exe", &[]).integration(), ShellIntegration::WslBash);
    }

    /// WSL 注入的参数形态：保留原 `-d <发行版>`，追加
    /// `--exec bash --rcfile /mnt/...`（`--exec` 保证 argv 不被来宾 shell
    /// 二次解释）。rcfile 写进 %TEMP%，路径必然是 automount 形态。
    #[cfg(windows)]
    #[test]
    fn wsl_shells_launch_bash_with_the_shared_integration_rcfile() {
        let launched = detected("wsl:Ubuntu", "wsl.exe", &["-d", "Ubuntu"]).shell();
        assert_eq!(launched.program(), "wsl.exe");
        let args = launched.args();
        assert_eq!(&args[..2], &["-d".to_owned(), "Ubuntu".to_owned()]);
        assert_eq!(&args[2..4], &["--exec".to_owned(), "bash".to_owned()]);
        assert_eq!(args[4], "--rcfile");
        assert!(args[5].starts_with("/mnt/"), "automount path, got {}", args[5]);
        assert!(args[5].ends_with("nebula-wsl-integration.bashrc"));
    }

    #[test]
    fn native_and_unsupported_shells_keep_their_program_and_completion_args() {
        for source in [
            detected("nu", "nu.exe", &["--config", "custom.nu"]),
            detected("cmd", "cmd.exe", &["/Q"]),
            detected("other", "other.exe", &["--interactive"]),
        ] {
            let launched = source.shell();
            assert_eq!(launched.program(), source.program);
            assert_eq!(launched.args(), source.args);
        }
    }

    #[cfg(windows)]
    #[test]
    fn menu_powershell_loads_nebula_without_disabling_the_user_profile() {
        let launched = detected("pwsh", "pwsh.exe", &["-NoLogo"]).shell();

        assert_eq!(launched.program(), "pwsh.exe");
        assert_eq!(launched.args().first().map(String::as_str), Some("-NoLogo"));
        assert!(launched.args().iter().any(|arg| arg == "-Command"));
        assert!(!launched.args().iter().any(|arg| arg.eq_ignore_ascii_case("-NoProfile")));
    }

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

    #[test]
    fn imported_profile_shell_keys_reuse_brand_assets() {
        let profile_key = "profile:pwsh|pwsh-1234";
        assert_eq!(icon_for_id(profile_key), icon_for_id("pwsh"));
        assert!(color_icon_png(profile_key).is_some());
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
