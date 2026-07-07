use std::ffi::OsStr;
use std::io::{self, Result};
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

use crate::event::{OnResize, WindowSize};
use crate::tty::windows::child::ChildExitWatcher;
use crate::tty::{ChildEvent, EventedPty, EventedReadWrite, Options, Shell};

mod blocking;
mod child;
mod conpty;

use blocking::{UnblockedReader, UnblockedWriter};
use conpty::Conpty as Backend;
use miow::pipe::{AnonRead, AnonWrite};
use polling::{Event, Poller};

pub const PTY_CHILD_EVENT_TOKEN: usize = 1;
pub const PTY_READ_WRITE_TOKEN: usize = 2;

type ReadPipe = UnblockedReader<AnonRead>;
type WritePipe = UnblockedWriter<AnonWrite>;

pub struct Pty {
    // XXX: Backend is required to be the first field, to ensure correct drop order. Dropping
    // `conout` before `backend` will cause a deadlock (with Conpty).
    backend: Backend,
    conout: ReadPipe,
    conin: WritePipe,
    child_watcher: ChildExitWatcher,
}

pub fn new(config: &Options, window_size: WindowSize, _window_id: u64) -> Result<Pty> {
    conpty::new(config, window_size)
}

impl Pty {
    fn new(
        backend: impl Into<Backend>,
        conout: impl Into<ReadPipe>,
        conin: impl Into<WritePipe>,
        child_watcher: ChildExitWatcher,
    ) -> Self {
        Self { backend: backend.into(), conout: conout.into(), conin: conin.into(), child_watcher }
    }

    pub fn child_watcher(&self) -> &ChildExitWatcher {
        &self.child_watcher
    }
}

fn with_key(mut event: Event, key: usize) -> Event {
    event.key = key;
    event
}

impl EventedReadWrite for Pty {
    type Reader = ReadPipe;
    type Writer = WritePipe;

    #[inline]
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        poll_opts: polling::PollMode,
    ) -> io::Result<()> {
        self.conin.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.conout.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.child_watcher.register(poll, with_key(interest, PTY_CHILD_EVENT_TOKEN));

        Ok(())
    }

    #[inline]
    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        poll_opts: polling::PollMode,
    ) -> io::Result<()> {
        self.conin.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.conout.register(poll, with_key(interest, PTY_READ_WRITE_TOKEN), poll_opts);
        self.child_watcher.register(poll, with_key(interest, PTY_CHILD_EVENT_TOKEN));

        Ok(())
    }

    #[inline]
    fn deregister(&mut self, _poll: &Arc<Poller>) -> io::Result<()> {
        self.conin.deregister();
        self.conout.deregister();
        self.child_watcher.deregister();

        Ok(())
    }

    #[inline]
    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.conout
    }

    #[inline]
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.conin
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child_watcher.event_rx().try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ChildEvent::Exited(None)),
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.backend.on_resize(window_size)
    }
}

// Modified per stdlib implementation.
// https://github.com/rust-lang/rust/blob/6707bf0f59485cf054ac1095725df43220e4be20/library/std/src/sys/args/windows.rs#L174
fn push_escaped_arg(cmd: &mut String, arg: &str) {
    let arg_bytes = arg.as_bytes();
    let quote = arg_bytes.iter().any(|c| *c == b' ' || *c == b'\t') || arg_bytes.is_empty();
    if quote {
        cmd.push('"');
    }

    let mut backslashes: usize = 0;
    for x in arg.chars() {
        if x == '\\' {
            backslashes += 1;
        } else {
            if x == '"' {
                // Add n+1 backslashes to total 2n+1 before internal '"'.
                cmd.extend((0..=backslashes).map(|_| '\\'));
            }
            backslashes = 0;
        }
        cmd.push(x);
    }

    if quote {
        // Add n backslashes to total 2n before ending '"'.
        cmd.extend((0..backslashes).map(|_| '\\'));
        cmd.push('"');
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NebulaShellExecutor {
    PowerShell,
    Bash,
}

#[derive(Clone, Copy, Debug)]
struct NebulaRuntimeSettings {
    shell: NebulaShellExecutor,
}

fn nebula_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("Nebula")
}

fn nebula_settings_value(key: &str) -> Option<String> {
    let data = std::fs::read_to_string(nebula_data_dir().join("nebula_settings.txt")).ok()?;
    data.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_owned())
    })
}

fn nebula_runtime_settings() -> NebulaRuntimeSettings {
    let shell_value = nebula_settings_value("shell")
        .or_else(|| nebula_settings_value("executor"))
        .map(|value| value.to_ascii_lowercase());
    let shell = match shell_value.as_deref() {
        Some("bash" | "git-bash" | "gitbash" | "wsl") => NebulaShellExecutor::Bash,
        _ => NebulaShellExecutor::PowerShell,
    };

    NebulaRuntimeSettings { shell }
}

/// Whether the side-loaded OpenConsole ConPTY path is enabled
/// (`openconsole=off` in nebula_settings.txt opts out; default on). Shared
/// by `ConptyApi::new` and the app layer, which uses it to suppress the
/// Term's duplicate answer to the host's pre-primed bring-up DA1 query.
pub fn conpty_sideload_enabled() -> bool {
    nebula_settings_value("openconsole")
        .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "off" | "false" | "no"))
        .unwrap_or(true)
}

fn nebula_existing_file(path: PathBuf) -> Option<String> {
    path.is_file().then(|| path.display().to_string())
}

fn nebula_find_bash() -> Option<String> {
    if let Some(path) = std::env::var_os("NEBULA_BASH").map(PathBuf::from) {
        if let Some(path) = nebula_existing_file(path) {
            return Some(path);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join("bash.exe"),
                dir.join("bin").join("bash.exe"),
                dir.join("usr").join("bin").join("bash.exe"),
            ] {
                if let Some(path) = nebula_existing_file(candidate) {
                    return Some(path);
                }
            }
        }
    }

    for candidate in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
        r"C:\msys64\usr\bin\bash.exe",
        r"C:\msys64\mingw64\bin\bash.exe",
    ] {
        if let Some(path) = nebula_existing_file(PathBuf::from(candidate)) {
            return Some(path);
        }
    }

    for root in
        ["LOCALAPPDATA", "USERPROFILE"].into_iter().filter_map(|name| std::env::var_os(name))
    {
        let root = PathBuf::from(root);
        for candidate in [
            root.join("Programs").join("Git").join("bin").join("bash.exe"),
            root.join("scoop")
                .join("apps")
                .join("git")
                .join("current")
                .join("bin")
                .join("bash.exe"),
        ] {
            if let Some(path) = nebula_existing_file(candidate) {
                return Some(path);
            }
        }
    }

    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join("bash.exe")).find_map(nebula_existing_file)
    })
}

/// Nebula default PowerShell prompt: a powerline-style, colored prompt that
/// makes the integrated experience look like Nebula out of the box instead of
/// a bare PowerShell. ANSI sequences are emitted to stdout and rendered by the
/// terminal itself, so colors work regardless of the PowerShell version.
const NEBULA_PROMPT_PS1: &str = r#"
$global:NebE = [char]27
$global:NebArrow = [char]0xE0B0
$global:NebLeftRound = [char]0xE0B6
$global:NebRightRound = [char]0xE0B4
$global:NebPromptArrow = [char]0x276F
$global:NebFolderIcon = [char]0xE70F
$global:NebGitBranchIcon = [char]0xF418
$global:NebClockIcon = [char]0xF017
$global:NebulaPromptCount = 0
$global:NebulaThemeFile = Join-Path ([System.IO.Path]::GetTempPath()) 'nebula_theme.txt'
$global:NebulaSettingsFile = if ($env:APPDATA) {
    Join-Path $env:APPDATA 'Nebula\nebula_settings.txt'
} elseif ($env:HOME) {
    Join-Path (Join-Path $env:HOME '.config') 'Nebula\nebula_settings.txt'
} else {
    Join-Path ([System.IO.Path]::GetTempPath()) 'Nebula\nebula_settings.txt'
}

function global:Get-NebulaSetting {
    param([string]$Key, [string]$Default)

    try {
        if (Test-Path -LiteralPath $NebulaSettingsFile) {
            foreach ($line in Get-Content -LiteralPath $NebulaSettingsFile -ErrorAction SilentlyContinue) {
                $pair = $line -split '=', 2
                if ($pair.Count -eq 2 -and $pair[0].Trim() -eq $Key) {
                    return $pair[1].Trim()
                }
            }
        }
    } catch {}

    $Default
}

function global:Get-NebulaBoolSetting {
    param([string]$Key, [bool]$Default)

    $fallback = if ($Default) { '1' } else { '0' }
    $value = (Get-NebulaSetting $Key $fallback).ToLowerInvariant()
    switch ($value) {
        '1'     { return $true }
        'true'  { return $true }
        'yes'   { return $true }
        'on'    { return $true }
        '0'     { return $false }
        'false' { return $false }
        'no'    { return $false }
        'off'   { return $false }
        default { return $Default }
    }
}

function global:Get-NebulaThemeName {
    try {
        if (Test-Path -LiteralPath $NebulaThemeFile) {
            $name = (Get-Content -LiteralPath $NebulaThemeFile -Raw -ErrorAction SilentlyContinue).Trim()
            switch ($name) {
                'SilverLight'    { return 'SilverLight' }
                'SteelDark'      { return 'SteelDark' }
                'LimestoneLight' { return 'LimestoneLight' }
                'CoalDark'       { return 'CoalDark' }
                'LinenLight'     { return 'LinenLight' }
                'MossDark'       { return 'MossDark' }
                'Nebula'         { return 'Nebula' }
            }
        }
    } catch {}
    'Nebula'
}

function global:prompt {
    # Same principle as Oh My Posh: prompt rendering may execute external
    # commands, so preserve the previous command status. Errors inside the
    # prompt must stay silent — but ONLY inside: assigning here is scoped to
    # this function. (A top-level assignment once silenced the whole session,
    # eating every user-facing error, e.g. a failed `cd`.)
    $ErrorActionPreference = 'SilentlyContinue'
    $originalLastExitCode = $global:LASTEXITCODE
    $e = $NebE
    # OSC 133;D — the previous command just finished (this prompt proves it).
    # Nebula pairs it with the 133;C from the PSConsoleHostReadLine wrapper to
    # time commands and raise a background notification when a long one
    # completes.
    [Console]::Write("$e]133;D$([char]7)")
    $reset = "$e[0m"
    $loc = (Get-Location).Path
    $hp = $env:USERPROFILE
    if ($hp -and $loc.StartsWith($hp)) { $loc = '~' + $loc.Substring($hp.Length) }
    $branch = ''
    $b = git rev-parse --abbrev-ref HEAD 2>$null
    if ($LASTEXITCODE -eq 0 -and $b) { $branch = $b }
    $time = Get-Date -Format 'HH:mm:ss'

    $global:NebulaPromptCount = [int]$global:NebulaPromptCount + 1
    $leadingNewline = ''
    try {
        # PowerShell cursor Y is zero-based. Like Oh My Posh's cancelNewline,
        # do not add a leading spacer for the first prompt or when at top.
        if ($global:NebulaPromptCount -gt 1 -and $Host.UI.RawUI.CursorPosition.Y -gt 0) {
            $leadingNewline = "`n"
        }
    } catch {
        if ($global:NebulaPromptCount -gt 1) { $leadingNewline = "`n" }
    }

    # Always initialize the default palette before reading the shared theme
    # bridge. The theme file is written by the UI process; if it is missing,
    # being replaced, or contains an unknown value, the prompt must still emit
    # valid ANSI instead of falling back to PowerShell's plain `PS>`.
    # ANSI has no alpha channel; soften the prompt by mixing every
    # segment background toward the terminal background.
    $icon0=@(57,75,112);   $icon1=@(46,60,90);   $iconFg=@(192,202,245)
    $path0=@(41,52,82);    $path1=@(33,42,66);   $pathFg=@(169,177,214)
    $branch0=@(47,79,79);  $branch1=@(38,63,63); $branchFg=@(139,213,202)
    $time0=@(29,33,46);    $time1=@(19,23,34);   $timeFg=@(100,116,139)
    $termbg=@(15,17,26)

    $theme = Get-NebulaThemeName
    switch ($theme) {
        'SilverLight' {
            # Premium-light sheet: neutral badge chips on a white terminal,
            # blue-tinted branch badge (#e0f2fe/#0369a1), violet prompt (#8250df).
            $icon0=@(229,231,235); $icon1=@(209,213,219); $iconFg=@(55,65,81)
            $path0=@(243,244,246); $path1=@(229,231,235); $pathFg=@(55,65,81)
            $branch0=@(224,242,254); $branch1=@(186,230,253); $branchFg=@(3,105,161)
            $time0=@(249,250,251); $time1=@(243,244,246); $timeFg=@(107,114,128)
            $termbg=@(255,255,255)
        }
        'SteelDark' {
            $icon0=@(71,85,105);  $icon1=@(57,68,84);   $iconFg=@(241,245,249)
            $path0=@(51,65,85);   $path1=@(41,52,68);   $pathFg=@(203,213,225)
            $branch0=@(59,82,73); $branch1=@(47,66,58); $branchFg=@(163,184,153)
            $time0=@(40,44,56);   $time1=@(30,34,44);   $timeFg=@(148,163,184)
            $termbg=@(26,28,36)
        }
        'LimestoneLight' {
            $icon0=@(214,211,209); $icon1=@(193,190,188); $iconFg=@(250,250,249)
            $path0=@(231,229,228); $path1=@(212,210,208); $pathFg=@(68,64,60)
            $branch0=@(200,198,167); $branch1=@(178,176,146); $branchFg=@(41,37,36)
            $time0=@(235,233,230); $time1=@(220,218,214); $timeFg=@(163,160,151)
            $termbg=@(255,255,255)
        }
        'CoalDark' {
            $icon0=@(82,82,82);   $icon1=@(66,66,66);   $iconFg=@(245,245,245)
            $path0=@(64,64,64);   $path1=@(51,51,51);   $pathFg=@(212,212,212)
            $branch0=@(74,79,65); $branch1=@(59,63,52); $branchFg=@(181,181,166)
            $time0=@(48,48,48);   $time1=@(38,38,38);   $timeFg=@(115,115,115)
            $termbg=@(23,23,23)
        }
        'LinenLight' {
            $icon0=@(212,212,208); $icon1=@(191,191,187); $iconFg=@(255,255,255)
            $path0=@(229,229,223); $path1=@(210,210,204); $pathFg=@(63,63,63)
            $branch0=@(181,196,177); $branch1=@(159,176,155); $branchFg=@(45,45,45)
            $time0=@(236,236,230); $time1=@(221,221,215); $timeFg=@(176,179,176)
            $termbg=@(255,255,255)
        }
        'MossDark' {
            $icon0=@(75,85,72);   $icon1=@(60,68,58);   $iconFg=@(240,253,244)
            $path0=@(59,66,56);   $path1=@(47,53,45);   $pathFg=@(220,252,231)
            $branch0=@(60,79,60); $branch1=@(48,63,48); $branchFg=@(187,247,208)
            $time0=@(42,47,42);   $time1=@(34,38,34);   $timeFg=@(107,114,107)
            $termbg=@(30,33,30)
        }
    }

    if (-not (Get-NebulaBoolSetting 'powerline' $true)) {
        $branchText = if ($branch) { " ($branch)" } else { "" }
        $output = "$leadingNewline$e]133;A$([char]7)$e]2;NEBULA|$loc|$branch$([char]7)$e[38;2;$($pathFg[0]);$($pathFg[1]);$($pathFg[2])m$loc$branchText $e[35m$NebPromptArrow $reset"
        try { Set-PSReadLineOption -ExtraPromptLineCount (($output | Measure-Object -Line).Lines - 1) } catch {}
        $global:LASTEXITCODE = $originalLastExitCode
        return $output
    }

    $segs = New-Object System.Collections.ArrayList
    [void]$segs.Add(@{ c0=$icon0; c1=$icon1; fg=$iconFg; t=" $NebFolderIcon " })
    [void]$segs.Add(@{ c0=$path0; c1=$path1; fg=$pathFg; t="  $loc  " })
    if ($branch) { [void]$segs.Add(@{ c0=$branch0; c1=$branch1; fg=$branchFg; t=" $NebGitBranchIcon $branch  " }) }
    [void]$segs.Add(@{ c0=$time0; c1=$time1; fg=$timeFg; t=" $NebClockIcon $time  " })

    $out = ''
    if ($segs.Count -gt 0) {
        $first = @($segs[0].c0)
        if ($first.Count -lt 3) { $first = $termbg }
        # 49 = default background: the cap cell's square corners always match the
        # real terminal bg (any theme / wallpaper), no hardcoded colour seams.
        $out += "$reset$e[38;2;$($first[0]);$($first[1]);$($first[2])m$e[49m$NebLeftRound$reset"
    }
    for ($i = 0; $i -lt $segs.Count; $i++) {
        $s = $segs[$i]; $t = $s.t; $n = $t.Length
        $c0 = @($s.c0); $c1 = @($s.c1); $fg = @($s.fg)
        if ($c0.Count -lt 3) { $c0 = $termbg }
        if ($c1.Count -lt 3) { $c1 = $c0 }
        if ($fg.Count -lt 3) { $fg = @(210,218,238) }
        $out += "$e[38;2;$($fg[0]);$($fg[1]);$($fg[2])m"
        for ($j = 0; $j -lt $n; $j++) {
            if ($n -le 1) {
                $f = 0.0
            } else {
                $t01 = [double]$j / [double]($n - 1)
                # Non-linear smootherstep easing (6t^5 - 15t^4 + 10t^3):
                # slow at both ends, faster in the middle, closer to a soft glass transition.
                $f = $t01 * $t01 * $t01 * ($t01 * ($t01 * 6.0 - 15.0) + 10.0)
            }
            $r  = [int][Math]::Round($c0[0] + ($c1[0] - $c0[0]) * $f)
            $g  = [int][Math]::Round($c0[1] + ($c1[1] - $c0[1]) * $f)
            $bl = [int][Math]::Round($c0[2] + ($c1[2] - $c0[2]) * $f)
            $out += "$e[48;2;$r;$g;${bl}m" + $t[$j]
        }
        if ($i -lt $segs.Count - 1) {
            $nb = @($segs[$i + 1].c0)
            if ($nb.Count -lt 3) { $nb = $termbg }
            $out += "$reset$e[38;2;$($c1[0]);$($c1[1]);$($c1[2])m$e[48;2;$($nb[0]);$($nb[1]);$($nb[2])m$NebArrow$reset"
        } else {
            # 49 = default background, same reason as the left cap above.
            $out += "$reset$e[38;2;$($c1[0]);$($c1[1]);$($c1[2])m$e[49m$NebRightRound$reset"
        }
    }
    $output = "$leadingNewline$e]133;A$([char]7)$e]2;NEBULA|$loc|$branch$([char]7)$out`n`n$e[35m$NebPromptArrow $reset"
    try { Set-PSReadLineOption -ExtraPromptLineCount (($output | Measure-Object -Line).Lines - 1) } catch {}
    $global:LASTEXITCODE = $originalLastExitCode
    $output
}

# Build a spec-correct file:// URI from a Windows path for OSC 8 hyperlinks.
# RFC 3986: escape every segment (UTF-8 + surrogate pairs via EscapeDataString),
# keep '/' as the separator and a leading "D:" drive as-is. UNC \\server\share
# becomes file://server/share/...; local paths become file:///D:/...
function global:ConvertTo-NebulaFileUri {
    param([string]$Path)

    # UNC (\\server\share\x): the first two segments are the authority; strip
    # the leading backslashes so empty split segments don't inflate the slashes.
    $isUnc = $Path.StartsWith('\\')
    $body = if ($isUnc) { $Path.Substring(2) } else { $Path }
    $escaped = (($body -replace '\\','/') -split '/' | ForEach-Object {
        if ($_ -match '^[A-Za-z]:$') { $_ } else { [System.Uri]::EscapeDataString($_) }
    }) -join '/'

    if ($isUnc) { 'file://' + $escaped } else { 'file:///' + $escaped }
}

# Unix-style colored directory listing, replacing PowerShell's default table.
function global:Nebula-List {
    $e = [char]27
    # 颜色统一走 ANSI-16 索引：终端主题表（Rust theme.rs → apply_term_colors）
    # 是唯一色源，浅/深主题切换时这里自动跟随，不再散落硬编码 RGB。
    # 37=元信息  90=次要(大小/日期)  34=目录  32=可执行  39=普通文件(默认前景)
    $meta = "$e[37m"
    $muted = "$e[90m"
    $argList = @($args | Where-Object { "$_" -notlike '-*' })
    $target = if ($argList.Count -gt 0) { $argList[0] } else { '.' }
    $items = Get-ChildItem -Force $target -ErrorAction SilentlyContinue |
        Sort-Object @{ Expression = { -not $_.PSIsContainer } }, Name
    foreach ($i in $items) {
        $isDir = $i.PSIsContainer
        if ($isDir) {
            $mode = 'drwxr-xr-x'
            $size = '     -'
            $col  = "$e[34m"
        } else {
            $mode = '-rw-r--r--'
            $len = $i.Length
            if ($len -ge 1048576) { $size = '{0,5:N1}M' -f ($len / 1048576) }
            elseif ($len -ge 1024) { $size = '{0,5:N1}K' -f ($len / 1024) }
            else { $size = '{0,6}' -f $len }
            # 设计稿：普通文件用默认前景（深灰近黑），可执行类才上绿色。
            $col = if ($i.Extension -match '^\.(exe|dll|bat|cmd|ps1|com|msi|sh)$') { "$e[32m" } else { "$e[39m" }
        }
        $date = '{0,12}' -f $i.LastWriteTime.ToString('MMM d HH:mm', [System.Globalization.CultureInfo]::InvariantCulture)
        # OSC 8 hyperlink around the name (nushell's osc8 behaviour): the
        # terminal turns it into a click target that opens the file/folder.
        # Full RFC 3986 encoding (UTF-8, CJK, spaces) via ConvertTo-NebulaFileUri.
        $uri = ConvertTo-NebulaFileUri $i.FullName
        $b = [char]7
        "$meta$mode$e[0m  $muted$size$e[0m  $muted$date$e[0m  $e]8;;$uri$b$col$($i.Name)$e[0m$e]8;;$b"
    }
}
Remove-Item Alias:ls  -Force -ErrorAction SilentlyContinue
Remove-Item Alias:dir -Force -ErrorAction SilentlyContinue
Remove-Item Alias:ll  -Force -ErrorAction SilentlyContinue
Set-Alias -Name ls  -Value Nebula-List -Scope Global -Option AllScope -Force
Set-Alias -Name ll  -Value Nebula-List -Scope Global -Option AllScope -Force
Set-Alias -Name dir -Value Nebula-List -Scope Global -Option AllScope -Force

function global:Convert-NebulaBareEnvAssignment {
    param([string]$Line)

    # PowerShell 的赋值右侧是表达式，$env:KEY=sk-ant-xxx 这类裸 token 会被当命令/表达式解析。
    # 这里仅兼容单行、纯字面量 token；复杂表达式仍交给 PowerShell 原生解析，避免误改用户命令。
    if ([string]::IsNullOrWhiteSpace($Line) -or $Line.Contains("`n") -or $Line.Contains("`r")) {
        return $null
    }

    $pattern = '^(?<indent>\s*)\$env:(?<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?<value>[^''"`$@\(\[\{;|&<>#\s][^;|&<>`]*)\s*$'
    if ($Line -notmatch $pattern) {
        return $null
    }

    $value = $Matches['value'].Trim()
    if ([string]::IsNullOrEmpty($value)) {
        return $null
    }

    $escaped = $value.Replace("'", "''")
    return ($Matches['indent'] + '$env:' + $Matches['name'] + "='" + $escaped + "'")
}

function global:Convert-NebulaBareCd {
    param([string]$Line)

    # `cd D:/Program Files/` — an unquoted path with spaces splats into two
    # positional args and Set-Location errors out. People paste paths like
    # this constantly, so quote the whole remainder when it's a plain literal
    # (no quotes/variables/operators that PowerShell should parse itself).
    if ([string]::IsNullOrWhiteSpace($Line) -or $Line.Contains("`n") -or $Line.Contains("`r")) {
        return $null
    }

    $pattern = '^(?<indent>\s*)(?<cmd>cd|chdir|pushd|sl|Set-Location)\s+(?<path>[^''"`$;|&<>()\[\]{}-][^''"`$;|&<>]*\s[^''"`$;|&<>]*?)\s*$'
    if ($Line -notmatch $pattern) {
        return $null
    }

    $path = $Matches['path'].Trim()
    if ([string]::IsNullOrEmpty($path)) {
        return $null
    }

    $escaped = $path.Replace("'", "''")
    return ($Matches['indent'] + $Matches['cmd'] + " '" + $escaped + "'")
}

# oh-my-zsh-style experience: Nebula syntax colors. Prediction is OFF on
# purpose: Nebula draws its own fish-style ghost hint, and running PSReadLine's
# InlinePrediction alongside it double-renders a second gray hint AND races the
# ghost-accept keys — the two sources desync and commit garbage like
# "lsls sclaude" into history (which the hint then resurfaces, spooking users).
if (Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue) {
    try { Set-PSReadLineOption -PredictionSource None -ErrorAction SilentlyContinue } catch {}
    try {
        # 不让 PowerShell 的 continuation prompt 回退成突兀的 `>>`，视觉上保持 Nebula 的单箭头。
        # 35=Magenta：主题表里的提示符色（浅色=优雅紫 #8250df），与主提示符一致。
        Set-PSReadLineOption -ContinuationPrompt "$([char]27)[35m$NebPromptArrow $([char]27)[0m" -ErrorAction SilentlyContinue
    } catch {}
    try {
        # 语法高亮同样只用 ANSI-16 索引——色值由终端主题表决定，浅/深自动适配。
        Set-PSReadLineOption -Colors @{
            Command          = "$([char]27)[96m"
            Parameter        = "$([char]27)[95m"
            String           = "$([char]27)[32m"
            Number           = "$([char]27)[33m"
            Operator         = "$([char]27)[37m"
            Variable         = "$([char]27)[94m"
            Comment          = "$([char]27)[90m"
        } -ErrorAction SilentlyContinue
    } catch {}
    try {
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            param($key, $arg)

            $line = ''
            $cursor = 0
            try {
                [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
                $converted = Convert-NebulaBareEnvAssignment $line
                if (-not $converted) { $converted = Convert-NebulaBareCd $line }
                if ($converted) {
                    try {
                        [Microsoft.PowerShell.PSConsoleReadLine]::Replace(0, $line.Length, $converted)
                    } catch {
                        try {
                            [Microsoft.PowerShell.PSConsoleReadLine]::Replace(0, $line.Length, $converted, $null, $null)
                        } catch {}
                    }
                }
            } catch {}

            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine($key, $arg)
        } -ErrorAction SilentlyContinue

        # Nu/Reedline-style editing muscle memory: Ctrl+U removes everything
        # from the cursor back to the command start. At the line end this clears
        # the whole command in one chord, matching the expected shell UX.
        Set-PSReadLineKeyHandler -Key Ctrl+u -Function BackwardDeleteLine -ErrorAction SilentlyContinue
        Set-PSReadLineKeyHandler -Key Ctrl+k -Function ForwardDeleteLine -ErrorAction SilentlyContinue
    } catch {}

    # OSC 133;C — wrap PSConsoleHostReadLine (VS Code shell integration's
    # approach, same signal nushell emits natively before executing): the host
    # only returns from ReadLine once it has a *complete* command, so C fires
    # exactly once, right before execution. The previous Enter-key-handler
    # emission misfired on multiline continuations (`{` + Enter) and blank
    # Enters, spinning Nebula's sidebar spinner for commands that never ran.
    # Defined after the Set-PSReadLineOption calls above so PSReadLine is
    # already imported and this global override sticks.
    function global:PSConsoleHostReadLine {
        $line = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine($Host.Runspace, $ExecutionContext)
        # A blank Enter re-renders the prompt without running anything: no C,
        # so the spinner doesn't flash for a no-op.
        if (-not [string]::IsNullOrWhiteSpace($line)) {
            [Console]::Write("$([char]27)]133;C$([char]7)")
        }
        $line
    }
}
Clear-Host
"#;

/// Write `contents` to `path` only when it differs from what's already there.
/// These integration scripts sit on every pane-spawn's critical path, and the
/// content only changes across Nebula builds — skipping the rewrite avoids a
/// synchronous disk write (and antivirus re-scan) per tab.
fn write_if_changed(path: &std::path::Path, contents: &[u8]) -> bool {
    match std::fs::read(path) {
        Ok(existing) if existing == contents => true,
        _ => std::fs::write(path, contents).is_ok(),
    }
}

/// Write the Nebula prompt script to a temp file, returning its path.
fn nebula_prompt_script_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("nebula_prompt.ps1");
    // NOTE: do NOT touch the theme bridge file here. The UI process owns it
    // (written with the restored/selected theme); stamping a default from the
    // spawn path used to reset the powerline palette on every new tab.

    // Windows PowerShell 5.1 treats UTF-8 without BOM as the local ANSI codepage.
    // The embedded prompt contains non-ASCII comments, so write a UTF-8 BOM to
    // keep script parsing deterministic across Windows versions.
    let mut script = Vec::with_capacity(3 + NEBULA_PROMPT_PS1.len());
    script.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    script.extend_from_slice(NEBULA_PROMPT_PS1.as_bytes());
    write_if_changed(&path, &script).then_some(path)
}

const NEBULA_BASH_RC: &str = r#"
# Nebula Bash integration. Source the user's bashrc first, then keep the
# terminal-visible prompt/title/cwd contract stable for tabs and splits.
if [ -f "$HOME/.bashrc" ] && [ -z "${NEBULA_BASHRC_SOURCED:-}" ]; then
    export NEBULA_BASHRC_SOURCED=1
    . "$HOME/.bashrc"
fi

__nebula_settings_file() {
    if command -v cygpath >/dev/null 2>&1 && [ -n "${APPDATA:-}" ]; then
        printf '%s/Nebula/nebula_settings.txt' "$(cygpath -u "$APPDATA")"
    elif [ -n "${APPDATA:-}" ]; then
        printf '%s/Nebula/nebula_settings.txt' "$APPDATA"
    elif [ -n "${HOME:-}" ]; then
        printf '%s/.config/Nebula/nebula_settings.txt' "$HOME"
    else
        printf ''
    fi
}

__nebula_setting() {
    local key="$1" default="$2" file
    file="$(__nebula_settings_file)"
    if [ -n "$file" ] && [ -r "$file" ]; then
        awk -F= -v key="$key" -v default="$default" '
            $1 == key { sub(/^[ \t]+/, "", $2); sub(/[ \t]+$/, "", $2); print $2; found = 1; exit }
            END { if (!found) print default }
        ' "$file"
    else
        printf '%s' "$default"
    fi
}

__nebula_bool_on() {
    case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
        1|true|yes|on) return 0 ;;
        *) return 1 ;;
    esac
}

# Turn a shell path into the Windows drive form Nebula's OSC 7 consumer needs.
# MSYS/Git-bash reports "/d/x", WSL "/mnt/c/x", Cygwin "/cygdrive/c/x"; the
# terminal's chdir on spawn only understands "/D:/x". Pure bash param expansion
# (no subprocess) keeps this off the hot path per Nebula's startup-speed rule.
# A genuinely posix path (WSL-internal "/home/…") has no Windows mapping and is
# left as-is (that cwd just isn't reachable from a Windows child).
__nebula_win_path() {
    local p="$1"
    case "$p" in
        /mnt/[a-zA-Z]/*|/mnt/[a-zA-Z])
            local d="${p:5:1}"; printf '/%s:%s' "${d^^}" "${p:6}" ;;
        /cygdrive/[a-zA-Z]/*|/cygdrive/[a-zA-Z])
            local d="${p:10:1}"; printf '/%s:%s' "${d^^}" "${p:11}" ;;
        /[a-zA-Z]/*|/[a-zA-Z])
            local d="${p:1:1}"; printf '/%s:%s' "${d^^}" "${p:2}" ;;
        *)
            printf '%s' "$p" ;;
    esac
}

__nebula_precmd() {
    local cwd="$PWD" branch="" loc="${PWD/#$HOME/~}"
    # OSC 133;D — the previous command finished (see the PowerShell prompt).
    printf '\033]133;D\007'
    if command -v git >/dev/null 2>&1; then
        branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
    fi

    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$(__nebula_win_path "$cwd")"
    printf '\033]133;A\007'
    printf '\033]2;NEBULA|%s|%s\007' "$cwd" "$branch"

    if __nebula_bool_on "$(__nebula_setting powerline 1)"; then
        # ANSI-16 only: 35=Magenta 提示符（同 PowerShell 侧），主题表决定实际色值。
        PS1='\[\033[35m\]❯ \[\033[0m\]'
    else
        PS1='\[\033[90m\]\w \[\033[35m\]❯ \[\033[0m\]'
    fi
}

PROMPT_COMMAND=__nebula_precmd
# OSC 133;C right before each command executes (bash >= 4.4), pairing with
# the 133;D in precmd for Nebula's finished-command notification.
PS0=$'\033]133;C\a'

# Clickable ls entries via OSC 8 hyperlinks (same mechanism as Nushell's
# osc8 and Nebula's PowerShell Nebula-List). Guarded: only when this
# coreutils build understands --hyperlink.
if ls --hyperlink=auto -d . >/dev/null 2>&1; then
    alias ls='ls --color=auto --hyperlink=auto'
    alias ll='ls -l --color=auto --hyperlink=auto'
    alias la='ls -lA --color=auto --hyperlink=auto'
    alias dir='ls --color=auto --hyperlink=auto'
fi
"#;

fn nebula_bash_rc_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("nebula_bashrc");
    write_if_changed(&path, NEBULA_BASH_RC.as_bytes()).then_some(path)
}

fn nebula_bash_shell() -> Shell {
    if let Some(program) = nebula_find_bash() {
        let mut args = Vec::new();
        if let Some(rc) = nebula_bash_rc_path() {
            args.push("--rcfile".to_owned());
            args.push(rc.display().to_string());
        }
        args.push("-i".to_owned());
        Shell::new(program, args)
    } else {
        Shell::new(
            "wsl.exe".to_owned(),
            vec!["--exec".to_owned(), "bash".to_owned(), "-i".to_owned()],
        )
    }
}

/// Build the default shell, injecting the Nebula prompt when possible.
fn nebula_default_shell(settings: NebulaRuntimeSettings) -> Shell {
    if settings.shell == NebulaShellExecutor::Bash {
        return nebula_bash_shell();
    }

    match nebula_prompt_script_path() {
        Some(path) => Shell::new(
            "powershell".to_owned(),
            vec![
                "-NoLogo".to_owned(),
                // Skip $PROFILE: Nebula's integration script owns the prompt,
                // aliases and PSReadLine setup, so the user profile would be
                // mostly overridden anyway — and it is the single biggest
                // uncontrollable startup cost (conda/nvm/oh-my-posh routinely
                // add seconds).
                "-NoProfile".to_owned(),
                "-NoExit".to_owned(),
                "-ExecutionPolicy".to_owned(),
                "Bypass".to_owned(),
                "-Command".to_owned(),
                format!(". '{}'", path.display()),
            ],
        ),
        None => Shell::new("powershell".to_owned(), Vec::new()),
    }
}

fn cmdline(config: &Options) -> String {
    let default_shell = nebula_default_shell(nebula_runtime_settings());
    let using_default_shell = config.shell.is_none();
    let shell = config.shell.as_ref().unwrap_or(&default_shell);

    let mut cmd = String::new();
    push_escaped_arg(&mut cmd, &shell.program);

    for arg in &shell.args {
        cmd.push(' ');
        if config.escape_args || using_default_shell {
            push_escaped_arg(&mut cmd, arg);
        } else {
            cmd.push_str(arg)
        }
    }
    cmd
}

/// Converts the string slice into a Windows-standard representation for "W"-
/// suffixed function variants, which accept UTF-16 encoded string values.
pub fn win32_string<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

#[cfg(test)]
mod test {
    use crate::tty::windows::{cmdline, push_escaped_arg};
    use crate::tty::{Options, Shell};

    #[test]
    fn test_escape() {
        let test_set = vec![
            // Basic cases - no escaping needed
            ("abc", "abc"),
            // Cases requiring quotes (space/tab)
            ("", "\"\""),
            (" ", "\" \""),
            ("ab c", "\"ab c\""),
            ("ab\tc", "\"ab\tc\""),
            // Cases with backslashes only (no spaces, no quotes) - no quotes added
            ("ab\\c", "ab\\c"),
            // Cases with quotes only (no spaces) - quotes escaped but no outer quotes
            ("ab\"c", "ab\\\"c"),
            ("\"", "\\\""),
            ("a\"b\"c", "a\\\"b\\\"c"),
            // Cases requiring both quotes and escaping (contains spaces)
            ("ab \"c", "\"ab \\\"c\""),
            ("a \"b\" c", "\"a \\\"b\\\" c\""),
            // Complex real-world cases
            ("C:\\Program Files\\", "\"C:\\Program Files\\\\\""),
            ("C:\\Program Files\\a.txt", "\"C:\\Program Files\\a.txt\""),
            (
                r#"sh -c "cd /home/user; ARG='abc' \""'${SHELL:-sh}" -i -c '"'echo hello'""#,
                r#""sh -c \"cd /home/user; ARG='abc' \\\"\"'${SHELL:-sh}\" -i -c '\"'echo hello'\"""#,
            ),
        ];

        for (input, expected) in test_set {
            let mut escaped_arg = String::new();
            push_escaped_arg(&mut escaped_arg, input);
            assert_eq!(escaped_arg, expected, "Failed for input: {}", input);
        }
    }

    #[test]
    fn test_cmdline() {
        let mut options = Options {
            shell: Some(Shell {
                program: "echo".to_string(),
                args: vec!["hello world".to_string()],
            }),
            working_directory: None,
            drain_on_exit: true,
            env: Default::default(),
            escape_args: false,
        };
        assert_eq!(cmdline(&options), "echo hello world");

        options.escape_args = true;
        assert_eq!(cmdline(&options), "echo \"hello world\"");
    }
}
