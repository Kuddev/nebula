use std::ffi::OsStr;
use std::io::{self, Result};
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

use windows_sys::Win32::System::Threading::TerminateProcess;

use crate::event::{OnResize, WindowSize};
use crate::tty::windows::child::ChildExitWatcher;
use crate::tty::{ChildEvent, EventedPty, EventedReadWrite, Options, Shell};

mod blocking;
mod child;
mod conpty;
mod environment;

use blocking::{UnblockedReader, UnblockedWriter};
use conpty::Conpty as Backend;
pub use environment::refresh_environment;
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

impl Drop for Pty {
    fn drop(&mut self) {
        // Stop the shell before tearing down the console, so a busy process
        // tree can't keep producing output mid-teardown; the console host
        // CTRL_CLOSEs its remaining clients when it exits. A no-op when the
        // child already exited.
        unsafe {
            TerminateProcess(self.child_watcher.raw_handle(), 0);
        }
        // `backend` drops right after this body, and its ClosePseudoConsole
        // blocks until the host has flushed conout. Nothing polls the
        // terminal anymore at that point: a full pipe would park the reader
        // thread forever and deadlock the close — the "window closed but
        // nebula.exe lingers in task manager" failure. Hand conout to a
        // detached drain thread so the flush always has a consumer.
        self.conout.drain_detached();
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

    fn child_pid(&self) -> Option<u32> {
        self.child_watcher.pid().map(std::num::NonZeroU32::get)
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
    Wsl,
}

#[derive(Clone, Copy, Debug)]
struct NebulaRuntimeSettings {
    shell: NebulaShellExecutor,
}

fn nebula_data_dir() -> PathBuf {
    // The GUI and the injected shell prompt must read the same settings file.
    // Portable runs set this override on the parent process; falling back to
    // APPDATA preserves the normal installed-layout behavior.
    if let Some(path) = std::env::var_os("NEBULA_CONFIG_DIR").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Nebula")
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
        Some("bash" | "git-bash" | "gitbash") => NebulaShellExecutor::Bash,
        Some("wsl") => NebulaShellExecutor::Wsl,
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

    for root in ["LOCALAPPDATA", "USERPROFILE"].into_iter().filter_map(std::env::var_os) {
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
$global:PSDefaultParameterValues['Get-Content:Encoding'] = 'utf8'
$global:NebArrow = [char]0xE0B0
$global:NebLeftRound = [char]0xE0B6
$global:NebRightRound = [char]0xE0B4
$global:NebPromptArrow = [char]0x276F
$global:NebFolderIcon = [char]0xE70F
$global:NebGitBranchIcon = [char]0xF418
$global:NebClockIcon = [char]0xF017
$global:NebulaPromptCount = 0
$global:NebulaSettingsFile = if ($env:NEBULA_CONFIG_DIR) {
    Join-Path $env:NEBULA_CONFIG_DIR 'nebula_settings.txt'
} elseif ($env:APPDATA) {
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

# 用户自己的提示符（$PROFILE 里的 oh-my-posh / starship / 手写 prompt）是不是
# 已经就位。判据只能看函数体：PowerShell 内置 prompt 固定引用
# $executionContext.SessionState.Path.CurrentLocation；而 oh-my-posh 那一类是经
# Invoke-Expression 装进来的，ScriptBlock.File 为空，靠 File 判断会把它们全部
# 误判成内置提示符。
function global:Test-NebulaUserPrompt {
    param($ScriptBlock)

    if (-not $ScriptBlock) { return $false }
    $body = $ScriptBlock.ToString()
    # 重复 source 本脚本时看到的是 Nebula 自己的 wrapper，它不算用户提示符。
    if ($body.Contains('NebulaPreviousPrompt')) { return $false }
    return -not $body.Contains('$executionContext.SessionState.Path.CurrentLocation')
}

# 脚本可能在已有集成之后被 source。只在第一次安装时保存原 prompt，
# 之后重载 Nebula 脚本也不能把自己的 wrapper 当成“用户 prompt”递归调用。
if (-not $global:NebulaPromptInstalled) {
    $existingPrompt = Get-Command prompt -CommandType Function -ErrorAction SilentlyContinue
    $global:NebulaPreviousPrompt = if ($existingPrompt) { $existingPrompt.ScriptBlock } else { $null }
    # 视觉归属。用户已经有提示符时，Nebula 只补协议标记，不画自己的 powerline：
    # 终端只负责补充协议标记，不替 shell 决定提示符外观。
    $global:NebulaUserOwnsPrompt = Test-NebulaUserPrompt $global:NebulaPreviousPrompt
    $global:NebulaPromptInstalled = $true
}

function global:prompt {
    # Same principle as Oh My Posh: prompt rendering may execute external
    # commands, so preserve the previous command status. Errors inside the
    # prompt must stay silent — but ONLY inside: assigning here is scoped to
    # this function. (A top-level assignment once silenced the whole session,
    # eating every user-facing error, e.g. a failed `cd`.)
    $originalDollarQuestion = $global:?
    $originalLastExitCode = $global:LASTEXITCODE
    $ErrorActionPreference = 'SilentlyContinue'
    $global:NebulaLastCommandSucceeded = $originalDollarQuestion
    $global:NebulaLastCommandExitCode = $originalLastExitCode
    $global:NebulaLastCommandDurationMs = $null
    try {
        $lastHistory = Get-History -Count 1 -ErrorAction SilentlyContinue
        if ($lastHistory -and $lastHistory.EndExecutionTime -ge $lastHistory.StartExecutionTime) {
            $global:NebulaLastCommandDurationMs = [math]::Max(
                0,
                [math]::Round(($lastHistory.EndExecutionTime - $lastHistory.StartExecutionTime).TotalMilliseconds)
            )
        }
    } catch {}

    # 旧 prompt 的环境管理器、历史或目录 hook 等副作用要保留。它的视觉输出则
    # 分两种：用户自己的提示符继续可见（Nebula 只在它前面补协议标记），内置的
    # 默认提示符才丢掉、由 Nebula 渲染——否则两个提示符会叠在一起。
    $userPrompt = ''
    if ($global:NebulaPreviousPrompt -and -not $global:NebulaPreviousPromptRunning) {
        $global:NebulaPreviousPromptRunning = $true
        try {
            $global:LASTEXITCODE = $originalLastExitCode
            if (-not $originalDollarQuestion) {
                Write-Error '' -ErrorAction Ignore
            }
            $previousOutput = & $global:NebulaPreviousPrompt
            if ($global:NebulaUserOwnsPrompt) {
                # 提示符函数可以返回多个对象，宿主是按顺序拼起来显示的。
                $userPrompt = (@($previousOutput) | ForEach-Object { [string]$_ }) -join ''
            }
        } catch {
        } finally {
            $global:NebulaPreviousPromptRunning = $false
        }
    }

    $e = $NebE
    # OSC 133;D;<code> — the previous command just finished (this prompt proves
    # it), carrying its exit status for the assistant's error recovery. `$?` is
    # the arbiter (it is False for BOTH failed cmdlets and non-zero native
    # commands, and unlike $LASTEXITCODE it resets every command — a stale
    # non-zero $LASTEXITCODE after a successful cmdlet must not read as
    # failure). The code detail comes from $LASTEXITCODE when it agrees,
    # otherwise a plain 1. First prompt of a session: $? is True → 0.
    $nebExit = 0
    if (-not $originalDollarQuestion) {
        $nebExit = if ($null -ne $originalLastExitCode -and $originalLastExitCode -ne 0) { $originalLastExitCode } else { 1 }
    }
    [Console]::Write("$e]133;D;$nebExit$([char]7)")
    $reset = "$e[0m"
    $cwd = (Get-Location).Path
    $loc = $cwd
    $hp = $env:USERPROFILE
    # `loc` 只负责提示符的紧凑显示；功能协议必须发送绝对 cwd。否则 home
    # 下的 `~\repo` 会被宿主文件树与 Git 当成无法解析的相对路径。
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

    # Segment colors come from the terminal's 256-color palette, slots
    # 16..=23 (icon bg/fg, path bg/fg, branch bg/fg, time bg/fg), published
    # per-theme by Nebula (theme.rs::apply_term_colors). Indexed colors mean a
    # theme switch recolors every prompt already in scrollback — truecolor
    # (the old scheme) is frozen the moment it prints. No theme file, no polling.

    if ($userPrompt) {
        # 视觉全部来自用户提示符；Nebula 只补协议：133;A 标出提示符起点，标题
        # 里带上宿主需要的绝对 cwd 与分支。换行留给用户提示符自己决定。
        $output = "$e]133;A$([char]7)$e]2;NEBULA|$cwd|$branch$([char]7)$userPrompt"
    } elseif (-not (Get-NebulaBoolSetting 'powerline' $true)) {
        $branchText = if ($branch) { " ($branch)" } else { "" }
        $output = "$leadingNewline$e]133;A$([char]7)$e]2;NEBULA|$cwd|$branch$([char]7)$e[38;5;19m$loc$branchText $e[35m$NebPromptArrow $reset"
    } else {
        $segs = New-Object System.Collections.ArrayList
        [void]$segs.Add(@{ bg=16; fg=17; t=" $NebFolderIcon " })
        [void]$segs.Add(@{ bg=18; fg=19; t="  $loc  " })
        if ($branch) { [void]$segs.Add(@{ bg=20; fg=21; t=" $NebGitBranchIcon $branch  " }) }
        [void]$segs.Add(@{ bg=22; fg=23; t=" $NebClockIcon $time  " })

        # 49 = default background on both caps: the cap cell's square corners
        # always match the real terminal bg (any theme / wallpaper).
        $out = "$reset$e[38;5;$($segs[0].bg)m$e[49m$NebLeftRound$reset"
        for ($i = 0; $i -lt $segs.Count; $i++) {
            $s = $segs[$i]
            $out += "$e[48;5;$($s.bg)m$e[38;5;$($s.fg)m$($s.t)"
            if ($i -lt $segs.Count - 1) {
                $nb = $segs[$i + 1].bg
                $out += "$reset$e[38;5;$($s.bg)m$e[48;5;${nb}m$NebArrow$reset"
            } else {
                $out += "$reset$e[38;5;$($s.bg)m$e[49m$NebRightRound$reset"
            }
        }
        $output = "$leadingNewline$e]133;A$([char]7)$e]2;NEBULA|$cwd|$branch$([char]7)$out`n`n$e[35m$NebPromptArrow $reset"
    }

    try { Set-PSReadLineOption -ExtraPromptLineCount (($output | Measure-Object -Line).Lines - 1) } catch {}

    # prompt 返回值先输出，状态恢复必须放到整个函数的最后；否则任意一次
    # Measure-Object、git 或赋值都会让下一条命令看到错误的 $?。
    $output
    $global:LASTEXITCODE = $originalLastExitCode
    if ($global:? -ne $originalDollarQuestion) {
        if ($originalDollarQuestion) {
            $null = 1
        } else {
            Write-Error '' -ErrorAction Ignore
        }
    }
}

# 重新安装用的引用：下面的 ReadLine wrapper 发现 prompt 被别人换掉时，用它把
# Nebula 的 wrapper 包回去。
$global:NebulaPromptScriptBlock = (Get-Command prompt -CommandType Function).ScriptBlock

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

    # OSC 133;C — wrap PSConsoleHostReadLine (the shell integration protocol's
    # approach, same signal nushell emits natively before executing): the host
    # only returns from ReadLine once it has a *complete* command, so C fires
    # exactly once, right before execution. The previous Enter-key-handler
    # emission misfired on multiline continuations (`{` + Enter) and blank
    # Enters, spinning Nebula's sidebar spinner for commands that never ran.
    # Defined after the Set-PSReadLineOption calls above so PSReadLine is
    # already imported and this global override sticks.
    if (-not $global:NebulaReadLineInstalled) {
        $existingReadLine = Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction SilentlyContinue
        $global:NebulaPreviousPSConsoleHostReadLine = if ($existingReadLine) {
            $existingReadLine.ScriptBlock
        } else {
            $null
        }
        $global:NebulaReadLineInstalled = $true
    }

    function global:PSConsoleHostReadLine {
        # 会话里再 source 一次 $PROFILE（或手动跑 oh-my-posh init）会用它们的
        # prompt 覆盖 Nebula 的 wrapper：OSC 133;A/D 从此不再发出，宿主侧的命令
        # 状态、耗时与历史定位就此静默失效。ReadLine 仍然是我们的，所以在这里把
        # prompt 包回去，并把刚出现的用户提示符记成视觉所有者。
        try {
            $installed = Get-Command prompt -CommandType Function -ErrorAction SilentlyContinue
            if ($global:NebulaPromptScriptBlock -and $installed -and
                -not $installed.ScriptBlock.ToString().Contains('NebulaPreviousPrompt')) {
                $global:NebulaPreviousPrompt = $installed.ScriptBlock
                $global:NebulaUserOwnsPrompt = Test-NebulaUserPrompt $installed.ScriptBlock
                Set-Item -Path function:global:prompt -Value $global:NebulaPromptScriptBlock
            }
        } catch {}

        $line = $null
        if ($global:NebulaPreviousPSConsoleHostReadLine -and -not $global:NebulaPreviousReadLineRunning) {
            $global:NebulaPreviousReadLineRunning = $true
            try {
                $previousResult = @(& $global:NebulaPreviousPSConsoleHostReadLine)
                $line = if ($previousResult.Count -gt 0) {
                    [string]$previousResult[-1]
                } else {
                    ''
                }
            } catch {
                $line = $null
            } finally {
                $global:NebulaPreviousReadLineRunning = $false
            }
        }
        if ($null -eq $line) {
            $line = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine($Host.Runspace, $ExecutionContext)
        }
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
# 先记下 source 之前的 PS1：系统级 rc（Git Bash 的 /etc/bash.bashrc）此刻已经跑
# 过，所以之后出现的差异只可能来自用户自己的配置。
__nebula_ps1_before="${PS1-}"
if [ -f "$HOME/.bashrc" ] && [ -z "${NEBULA_BASHRC_SOURCED:-}" ]; then
    export NEBULA_BASHRC_SOURCED=1
    . "$HOME/.bashrc"
fi

__nebula_settings_file() {
    if [ -n "${NEBULA_CONFIG_DIR:-}" ]; then
        printf '%s/nebula_settings.txt' "$NEBULA_CONFIG_DIR"
    elif command -v cygpath >/dev/null 2>&1 && [ -n "${APPDATA:-}" ]; then
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
        awk -F= -v key="$key" -v fallback="$default" '
            $1 == key { sub(/^[ \t]+/, "", $2); sub(/[ \t]+$/, "", $2); print $2; found = 1; exit }
            END { if (!found) print fallback }
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

__nebula_set_return() {
    return "${1:-0}"
}

__nebula_now_ms() {
    local now seconds fraction
    if [[ -n ${EPOCHREALTIME-} ]]; then
        now="$EPOCHREALTIME"
        seconds="${now%%.*}"
        fraction="${now#*.}000"
        printf '%s' "$((10#$seconds * 1000 + 10#${fraction:0:3}))"
    else
        # Bash 4 没有 EPOCHREALTIME；SECONDS 精度较低但仍保持单调，
        # 比为了提示符时长每次再启动 date 子进程更稳妥。
        printf '%s' "$((SECONDS * 1000))"
    fi
}

__nebula_run_saved_prompt_command() {
    local status="$1" command
    if [[ ${__nebula_saved_prompt_kind-} == array ]]; then
        for command in "${__nebula_saved_prompt_commands[@]}"; do
            __nebula_set_return "$status"
            eval -- "$command"
        done
    elif [[ -n ${__nebula_saved_prompt_command-} ]]; then
        __nebula_set_return "$status"
        eval -- "$__nebula_saved_prompt_command"
    fi
}

__nebula_precmd() {
    # 同一个赋值语句会在任何 helper 改写状态前展开两者；分成两行会让
    # PIPESTATUS 只剩下 local/assignment 的结果，而不是用户的管道结果。
    NEBULA_CMD_STATUS=$? NEBULA_PIPE_STATUS=("${PIPESTATUS[@]}")
    local cmd_status="$NEBULA_CMD_STATUS" end_ms=""

    if [[ -n ${NEBULA_COMMAND_START_MS-} ]]; then
        end_ms="$(__nebula_now_ms)"
        if [[ $NEBULA_COMMAND_START_MS =~ ^[0-9]+$ && $end_ms =~ ^[0-9]+$ ]]; then
            NEBULA_COMMAND_DURATION_MS=$((end_ms - NEBULA_COMMAND_START_MS))
        fi
        unset NEBULA_COMMAND_START_MS
    fi

    # OSC 133;D;<code> 要尽早发出：用户的旧 precmd 即使较慢，也不应拖延终端
    # 对“上一条命令已经结束”的判断。退出码供助手的错误恢复判定。
    printf '\033]133;D;%s\007' "$cmd_status"

    # 旧 PROMPT_COMMAND 的非视觉副作用（历史、环境管理器、目录 hook）仍然执行。
    local ps1_before_hooks="${PS1-}"
    __nebula_run_saved_prompt_command "$cmd_status"
    # starship 一类是在自己的 precmd 里每轮重写 PS1 的，rc 加载时看不出来；
    # 一旦发现它改了 PS1，视觉就归它，Nebula 只留 OSC 标记与标题。
    if [[ ${PS1-} != "$ps1_before_hooks" ]]; then
        __nebula_user_ps1=1
    fi

    local cwd="$PWD" branch="" loc="${PWD/#$HOME/~}"
    if command -v git >/dev/null 2>&1; then
        branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
    fi

    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$(__nebula_win_path "$cwd")"
    printf '\033]133;A\007'
    printf '\033]2;NEBULA|%s|%s\007' "$cwd" "$branch"

    if [[ -z ${__nebula_user_ps1-} ]]; then
        if __nebula_bool_on "$(__nebula_setting powerline 1)"; then
            # ANSI-16 only: 35=Magenta 提示符（同 PowerShell 侧），主题表决定实际色值。
            PS1='\[\033[35m\]❯ \[\033[0m\]'
        else
            PS1='\[\033[90m\]\w \[\033[35m\]❯ \[\033[0m\]'
        fi
    fi

    # PROMPT_COMMAND 自身的最终状态会成为交互式 shell 下一次看到的 $?。
    # 返回原值，`false; echo $?` 才不会被提示符内部的 git/printf 伪装成 0。
    return "$cmd_status"
}

if [[ -z ${NEBULA_PROMPT_INSTALLED-} ]]; then
    NEBULA_PROMPT_INSTALLED=1
    # 用户 rc 自己设过 PS1（手写提示符、oh-my-posh 之类）就把视觉留给它，
    # Nebula 只通过 PROMPT_COMMAND 补 OSC 标记和标题。
    if [[ ${PS1-} != "${__nebula_ps1_before-}" ]]; then
        __nebula_user_ps1=1
    fi
    __nebula_prompt_decl="$(declare -p PROMPT_COMMAND 2>/dev/null || :)"
    if [[ $__nebula_prompt_decl == declare\ -a* ]]; then
        __nebula_saved_prompt_kind=array
        declare -a __nebula_saved_prompt_commands=("${PROMPT_COMMAND[@]}")
    else
        __nebula_saved_prompt_kind=string
        __nebula_saved_prompt_command="${PROMPT_COMMAND-}"
    fi

    # 数组变量只给下标 0 赋值不会删除其余元素，必须先 unset，避免旧 hook
    # 又被 Bash 原生执行一次、再被上面的组合器执行第二次。
    unset PROMPT_COMMAND
    PROMPT_COMMAND=__nebula_precmd

    # bash >= 4.4 会在命令执行前展开 PS0。前缀保留用户原 PS0，并用
    # Starship 同类的参数展开技巧在当前 shell 设置开始时间，不覆盖 DEBUG trap。
    __nebula_saved_ps0="${PS0-}"
    PS0='${NEBULA_COMMAND_START_MS:$((NEBULA_COMMAND_START_MS="$(__nebula_now_ms)",0)):0}'$'\033]133;C\a'"$__nebula_saved_ps0"
fi

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

fn explicit_bash_integration_args(rc: &std::path::Path) -> Vec<String> {
    // 显式 shell 的 Options::escape_args=false；路径必须在参数自身带引号，
    // 否则用户名含空格时 Bash 会把 rcfile 路径截成两段。
    vec!["--rcfile".to_owned(), format!("\"{}\"", rc.display()), "-i".to_owned()]
}

/// 给三点菜单显式选择的 Bash 装上 Nebula 的 OSC/提示符契约。
///
/// `--rcfile` 只可靠接管交互式非 login shell，因此这里不能保留检测结果里的
/// `--login`。生成的 rcfile 会先 source `~/.bashrc`，bash-completion、alias
/// 与用户函数仍在，随后才安装 OSC 133;A/C/D hook。
pub fn bash_with_nebula_integration(program: String, fallback_args: Vec<String>) -> Shell {
    match nebula_bash_rc_path() {
        Some(rc) => Shell::new(program, explicit_bash_integration_args(&rc)),
        None => Shell::new(program, fallback_args),
    }
}

fn nebula_bash_shell() -> Shell {
    if let Some(program) = nebula_find_bash() {
        // 默认 shell 的参数会由 cmdline 统一转义，不能复用上面显式 shell
        // 已自带引号的参数，否则路径会被二次转义。
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

/// 旧设置值 `shell=wsl` 没有发行版身份，只能交给 WSL 的默认发行版。
/// 不追加 `--exec bash`：否则会绕过来宾账户通过 `chsh` 配置的默认 shell。
fn nebula_wsl_shell() -> Shell {
    Shell::new("wsl.exe".to_owned(), Vec::new())
}

fn powershell_integration_args(mut args: Vec<String>, script: &std::path::Path) -> Vec<String> {
    args.extend([
        "-NoExit".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-Command".to_owned(),
        format!(". '{}'", script.display()),
    ]);
    args
}

/// 给三点菜单显式选择的 Windows PowerShell / PowerShell 7 装上同一份
/// OSC/提示符契约。原参数排在前面且不追加 `-NoProfile`，因此用户 Profile、
/// PSReadLine 与原生 Tab completer 都会先正常加载。
pub fn powershell_with_nebula_integration(program: String, args: Vec<String>) -> Shell {
    match nebula_prompt_script_path() {
        Some(path) => Shell::new(program, powershell_integration_args(args, &path)),
        None => Shell::new(program, args),
    }
}

/// Build the default shell, injecting the Nebula prompt when possible.
fn nebula_default_shell(settings: NebulaRuntimeSettings) -> Shell {
    match settings.shell {
        NebulaShellExecutor::Bash => return nebula_bash_shell(),
        NebulaShellExecutor::Wsl => return nebula_wsl_shell(),
        NebulaShellExecutor::PowerShell => {},
    }

    match nebula_prompt_script_path() {
        Some(path) => Shell::new(
            "powershell".to_owned(),
            powershell_integration_args(
                vec![
                    "-NoLogo".to_owned(),
                    // Match the native Windows terminal path: do not silently
                    // skip the user's $PROFILE. Nebula's integration script is
                    // appended after PowerShell finishes its normal startup.
                ],
                &path,
            ),
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
    use std::io::Write;
    use std::process::{Command, Stdio};

    use crate::tty::windows::{
        NEBULA_BASH_RC, NEBULA_PROMPT_PS1, NebulaRuntimeSettings, NebulaShellExecutor, cmdline,
        explicit_bash_integration_args, nebula_default_shell, nebula_find_bash,
        powershell_integration_args, push_escaped_arg,
    };
    use crate::tty::{Options, Shell};

    #[test]
    fn powershell_cat_defaults_to_utf8() {
        assert!(NEBULA_PROMPT_PS1.contains("PSDefaultParameterValues['Get-Content:Encoding']"));
    }

    #[test]
    fn powershell_prompt_preserves_previous_hooks_and_both_status_channels() {
        assert!(NEBULA_PROMPT_PS1.contains("NebulaPreviousPrompt"));
        assert!(NEBULA_PROMPT_PS1.contains("NebulaPreviousPSConsoleHostReadLine"));
        assert!(NEBULA_PROMPT_PS1.contains("$originalDollarQuestion = $global:?"));
        assert!(NEBULA_PROMPT_PS1.contains("$global:LASTEXITCODE = $originalLastExitCode"));
        assert!(NEBULA_PROMPT_PS1.contains("Write-Error '' -ErrorAction Ignore"));
        assert!(
            !NEBULA_PROMPT_PS1.contains("return $output"),
            "an early return would skip common $? restoration"
        );
    }

    /// 用户提示符的归属判据必须留在脚本里：靠 `ScriptBlock.File` 判断会把
    /// oh-my-posh（经 Invoke-Expression 装载）误判成 PowerShell 内置提示符。
    #[test]
    fn powershell_prompt_detects_a_user_prompt_by_its_body() {
        assert!(NEBULA_PROMPT_PS1.contains("function global:Test-NebulaUserPrompt"));
        assert!(
            NEBULA_PROMPT_PS1
                .contains("$body.Contains('$executionContext.SessionState.Path.CurrentLocation')"),
            "内置提示符的判据只能是函数体"
        );
        assert!(NEBULA_PROMPT_PS1.contains("$global:NebulaUserOwnsPrompt"));
        assert!(
            NEBULA_PROMPT_PS1.contains("Set-Item -Path function:global:prompt"),
            "prompt 被会话中途替换后必须能重新包回来"
        );
    }

    #[test]
    fn powershell_reports_absolute_cwd_while_displaying_home_as_tilde() {
        assert!(NEBULA_PROMPT_PS1.contains("$cwd = (Get-Location).Path"));
        assert!(NEBULA_PROMPT_PS1.contains("$loc = $cwd"));
        assert!(NEBULA_PROMPT_PS1.contains("$loc = '~' + $loc.Substring($hp.Length)"));
        assert!(NEBULA_PROMPT_PS1.contains("NEBULA|$cwd|$branch"));
        assert!(
            !NEBULA_PROMPT_PS1.contains("NEBULA|$loc|$branch"),
            "the display-only tilde path must not leak into cwd consumers"
        );
    }

    #[test]
    fn disabling_powerline_keeps_the_complete_osc_lifecycle() {
        let toggle = NEBULA_PROMPT_PS1
            .find("Get-NebulaBoolSetting 'powerline'")
            .expect("powerline visual branch");
        let done = NEBULA_PROMPT_PS1.find("]133;D;").expect("OSC command done");
        let prompt = NEBULA_PROMPT_PS1[toggle..].find("]133;A").expect("plain prompt mark");
        let start = NEBULA_PROMPT_PS1[toggle..].find("]133;C").expect("OSC command start");

        assert!(done < toggle, "command completion must not depend on the visual branch");
        assert!(prompt < start, "the powerline-off prompt and ReadLine wrapper must both stay");
    }

    #[test]
    fn shell_integrations_share_the_portable_settings_override() {
        assert!(
            NEBULA_PROMPT_PS1.contains("NEBULA_CONFIG_DIR"),
            "PowerShell must read the same override as the GUI"
        );
        assert!(
            NEBULA_BASH_RC.contains("NEBULA_CONFIG_DIR"),
            "Bash must read the same override as the GUI"
        );
    }

    #[test]
    fn explicit_powershell_keeps_existing_args_and_adds_only_integration() {
        let script = std::path::Path::new(r"C:\Temp Folder\nebula_prompt.ps1");
        let args = powershell_integration_args(vec!["-NoLogo".to_owned()], script);

        assert_eq!(args.first().map(String::as_str), Some("-NoLogo"));
        assert!(!args.iter().any(|arg| arg.eq_ignore_ascii_case("-NoProfile")));
        assert_eq!(args[args.len() - 2], "-Command");
        assert_eq!(args.last().map(String::as_str), Some(". 'C:\\Temp Folder\\nebula_prompt.ps1'"));
    }

    /// `-NoProfile` on the default-shell path silently skipped the user's
    /// `$PROFILE`, so functions added there never loaded in a new tab.
    /// The explicit-shell path had its own
    /// guard already; this covers the path that actually regressed.
    #[test]
    fn default_powershell_loads_the_user_profile_and_ends_with_the_integration() {
        let shell =
            nebula_default_shell(NebulaRuntimeSettings { shell: NebulaShellExecutor::PowerShell });
        let args = shell.args();

        assert_eq!(shell.program(), "powershell");
        assert!(
            !args.iter().any(|arg| arg.eq_ignore_ascii_case("-NoProfile")),
            "the default shell must not skip the user's $PROFILE: {args:?}"
        );
        // The remaining shape only exists when the prompt script was written;
        // the no-`-NoProfile` contract above holds on both branches.
        let Some(command) = args.iter().position(|arg| arg == "-Command") else {
            return;
        };
        assert_eq!(args.first().map(String::as_str), Some("-NoLogo"));
        assert_eq!(command, args.len() - 2, "the dot-source must be the trailing argument");
        assert!(
            args.last().is_some_and(|arg| arg.starts_with(". '")),
            "the trailing argument must dot-source the prompt script: {args:?}"
        );
    }

    #[test]
    fn legacy_wsl_setting_preserves_the_guest_default_shell() {
        let shell = nebula_default_shell(NebulaRuntimeSettings { shell: NebulaShellExecutor::Wsl });
        assert_eq!(shell.program(), "wsl.exe");
        assert!(shell.args().is_empty(), "WSL must choose the guest account's default shell");
    }

    #[test]
    fn explicit_bash_quotes_the_generated_rcfile_without_touching_user_config() {
        let rc = std::path::Path::new(r"C:\Temp Folder\nebula_bashrc");
        assert_eq!(
            explicit_bash_integration_args(rc),
            vec!["--rcfile", r#""C:\Temp Folder\nebula_bashrc""#, "-i"]
        );
        assert!(NEBULA_BASH_RC.contains(r#"[ -f "$HOME/.bashrc" ]"#));
    }

    #[test]
    fn bash_prompt_script_contains_composition_and_status_contracts() {
        for required in [
            "NEBULA_CMD_STATUS=$? NEBULA_PIPE_STATUS=(\"${PIPESTATUS[@]}\")",
            "declare -a __nebula_saved_prompt_commands",
            "__nebula_run_saved_prompt_command \"$cmd_status\"",
            "return \"$cmd_status\"",
            "__nebula_saved_ps0=\"${PS0-}\"",
            "-v fallback=\"$default\"",
            // 视觉归属：用户设过 PS1 就不再覆盖（rc 阶段和 precmd 阶段各一处判据）。
            "__nebula_ps1_before=\"${PS1-}\"",
            "if [[ -z ${__nebula_user_ps1-} ]]; then",
        ] {
            assert!(
                NEBULA_BASH_RC.contains(required),
                "missing Bash lifecycle contract: {required}"
            );
        }
    }

    /// 真跑一遍嵌入的 PowerShell 脚本。提示符归属的判据在 PowerShell 语义里，
    /// 字符串断言证明不了"用户提示符还看得见"，所以这里和 Bash 侧一样起进程。
    fn run_powershell_integration_case(prelude: &str, checks: &str) {
        static PS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = PS_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path =
            std::env::temp_dir().join(format!("nebula-ps-integration-{}.ps1", std::process::id()));
        // Windows PowerShell 5.1 把无 BOM 的 UTF-8 当本地代码页；脚本里有中文
        // 注释和 ❯，必须带 BOM（与 nebula_prompt_script_path 同理）。
        let mut script = vec![0xEF, 0xBB, 0xBF];
        script.extend_from_slice(format!("{prelude}\n{NEBULA_PROMPT_PS1}\n{checks}\n").as_bytes());
        std::fs::write(&path, &script).expect("write the PowerShell integration script");
        let output = Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&path)
            .output();
        let _ = std::fs::remove_file(&path);
        // 沙箱里可能起不了 powershell.exe；结构断言仍然覆盖脚本内容。
        let Ok(output) = output else { return };
        assert!(
            output.status.success(),
            "PowerShell integration failed ({}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 每个用例都指向一个不存在的配置目录，这样 powerline 等设置一律取默认值，
    /// 不受开发机上真实 nebula_settings.txt 的影响。
    ///
    /// 另外补一个占位的 `Set-PSReadLineOption`：脚本里的 ReadLine wrapper 由
    /// `Get-Command Set-PSReadLineOption` 门控，而非交互的 powershell.exe 看不到
    /// 真正的 PSReadLine —— 不占位，整段 wrapper 就不会安装，用例会静默跳过。
    const PS_PRELUDE: &str = r#"
$env:NEBULA_CONFIG_DIR = Join-Path ([System.IO.Path]::GetTempPath()) 'nebula-ps-test-no-config'
function global:Set-PSReadLineOption { }
"#;

    /// #80 的第二半：用户 `$PROFILE` 里的提示符（oh-my-posh/starship/手写）会
    /// 正常加载，但过去被 Nebula 的 powerline 盖掉，看起来就像 profile 没生效。
    #[test]
    fn powershell_keeps_a_user_prompt_visible_and_only_adds_the_markers() {
        run_powershell_integration_case(
            &format!("{PS_PRELUDE}function global:prompt {{ 'USER-PROMPT> ' }}\n"),
            r#"
if (-not $global:NebulaUserOwnsPrompt) { exit 30 }
$rendered = prompt
if ($rendered -notlike '*USER-PROMPT> *') { exit 31 }
if ($rendered -notlike "*$([char]27)]133;A*") { exit 32 }
if ($rendered -notlike '*NEBULA|*') { exit 33 }
if ($rendered -like '*❯*') { exit 34 }
exit 0
"#,
        );
    }

    /// 没有用户提示符时，Nebula 自己的 powerline 依旧是开箱观感。
    #[test]
    fn powershell_renders_its_own_prompt_when_the_shell_has_no_user_prompt() {
        run_powershell_integration_case(
            PS_PRELUDE,
            r#"
if ($global:NebulaUserOwnsPrompt) { exit 40 }
$rendered = prompt
if ($rendered -notlike '*❯*') { exit 41 }
if ($rendered -notlike "*$([char]27)]133;A*") { exit 42 }
if ($rendered -notlike '*NEBULA|*') { exit 43 }
exit 0
"#,
        );
    }

    /// 会话中途再 source 一次 `$PROFILE` 会用用户的 prompt 覆盖 Nebula 的
    /// wrapper，OSC 133 从此静默失效。ReadLine wrapper 负责把它包回去。
    #[test]
    fn powershell_rewraps_a_prompt_replaced_mid_session() {
        run_powershell_integration_case(
            PS_PRELUDE,
            r#"
if (-not (Get-Command PSConsoleHostReadLine -CommandType Function -ErrorAction SilentlyContinue)) { exit 49 }
function global:prompt { 'LATE-PROMPT> ' }
$global:NebulaPreviousPSConsoleHostReadLine = { 'echo test' }
$line = PSConsoleHostReadLine
if ($line -ne 'echo test') { exit 50 }
$installed = (Get-Command prompt -CommandType Function).ScriptBlock.ToString()
if (-not $installed.Contains('NebulaPreviousPrompt')) { exit 51 }
if (-not $global:NebulaUserOwnsPrompt) { exit 52 }
$rendered = prompt
if ($rendered -notlike '*LATE-PROMPT> *') { exit 53 }
if ($rendered -notlike "*$([char]27)]133;A*") { exit 54 }
exit 0
"#,
        );
    }

    fn run_bash_integration_case(prelude: &str, checks: &str) {
        static BASH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = BASH_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(program) = nebula_find_bash() else {
            // Git Bash is optional; structural assertions still run everywhere.
            return;
        };
        let child = Command::new(program)
            .args(["--noprofile", "--norc", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = child else {
            // Some Windows CI/sandbox tokens can locate Git Bash but cannot
            // create its MSYS login session (ERROR_NO_SUCH_LOGON_SESSION).
            return;
        };
        let script = format!("{prelude}\n{NEBULA_BASH_RC}\n{checks}\n");
        child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        let output = child.wait_with_output().expect("wait for Git Bash");
        assert!(
            output.status.success(),
            "Git Bash integration failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn bash_prompt_composes_string_hook_and_preserves_status_pipeline_and_ps0() {
        run_bash_integration_case(
            r#"
PATH=/usr/bin:/mingw64/bin:$PATH
NEBULA_BASHRC_SOURCED=1
HOME=/__nebula_test_missing_home__
APPDATA=
PROMPT_COMMAND='user_prompt_hook'
PS0='user-ps0'
hook_count=0
observed_status=99
user_prompt_hook() { observed_status=$?; hook_count=$((hook_count + 1)); }
"#,
            r#"
[[ "$(__nebula_setting powerline 1)" == 1 ]] || exit 10
NEBULA_COMMAND_START_MS=0
false
__nebula_precmd >/dev/null
restored=$?
[[ $restored -eq 1 ]] || exit 11
[[ $observed_status -eq 1 ]] || exit 12
[[ $hook_count -eq 1 ]] || exit 13
[[ $PS1 == *'❯ '* ]] || exit 14
[[ $PS0 == *'user-ps0' ]] || exit 15
[[ ${NEBULA_COMMAND_DURATION_MS-} =~ ^[0-9]+$ ]] || exit 16
false | true
__nebula_precmd >/dev/null
[[ ${NEBULA_PIPE_STATUS[0]} -eq 1 && ${NEBULA_PIPE_STATUS[1]} -eq 0 ]] || exit 17
"#,
        );
    }

    /// 用户自己的提示符（starship / oh-my-posh 那类在 PROMPT_COMMAND 里每轮
    /// 重写 PS1 的，以及 `~/.bashrc` 里直接写死 PS1 的）必须继续可见：Nebula
    /// 只补 OSC 标记和标题，不再覆盖 PS1。
    #[test]
    fn bash_prompt_leaves_a_user_supplied_ps1_visible() {
        run_bash_integration_case(
            r#"
PATH=/usr/bin:/mingw64/bin:$PATH
NEBULA_BASHRC_SOURCED=1
HOME=/__nebula_test_missing_home__
APPDATA=
PROMPT_COMMAND='fake_starship_precmd'
fake_starship_precmd() { PS1='starship-ps1'; }
"#,
            r#"
observed="$(__nebula_precmd)"
[[ $observed == *$'\033]133;A\a'* ]] || exit 31
[[ $observed == *'NEBULA|'* ]] || exit 32
__nebula_precmd >/dev/null
[[ $PS1 == 'starship-ps1' ]] || exit 33
[[ $PS1 != *'❯'* ]] || exit 34
"#,
        );
    }

    /// `~/.bashrc` 在 source 阶段就写死 PS1 的情形：判据来自 rc 加载前后的快照，
    /// 顺带钉住"用户 bashrc 仍然会被 source"这个前提。
    #[test]
    fn bash_prompt_detects_a_ps1_set_by_the_user_bashrc() {
        run_bash_integration_case(
            r#"
PATH=/usr/bin:/mingw64/bin:$PATH
APPDATA=
HOME="$(mktemp -d)"
printf 'PS1="user-rc-ps1"\n' > "$HOME/.bashrc"
"#,
            r#"
[[ $PS1 == 'user-rc-ps1' ]] || exit 41
__nebula_precmd >/dev/null
[[ $PS1 == 'user-rc-ps1' ]] || exit 42
rm -rf "$HOME"
"#,
        );
    }

    #[test]
    fn bash_prompt_composes_array_hooks_once_and_gives_each_original_status() {
        run_bash_integration_case(
            r#"
PATH=/usr/bin:/mingw64/bin:$PATH
NEBULA_BASHRC_SOURCED=1
HOME=/__nebula_test_missing_home__
APPDATA=
declare -a PROMPT_COMMAND=('hook_one' 'hook_two')
hook_one_count=0
hook_two_count=0
hook_one_status=99
hook_two_status=99
hook_one() { hook_one_status=$?; hook_one_count=$((hook_one_count + 1)); }
hook_two() { hook_two_status=$?; hook_two_count=$((hook_two_count + 1)); }
"#,
            r#"
false
__nebula_precmd >/dev/null
restored=$?
[[ $restored -eq 1 ]] || exit 21
[[ $hook_one_count -eq 1 && $hook_two_count -eq 1 ]] || exit 22
[[ $hook_one_status -eq 1 && $hook_two_status -eq 1 ]] || exit 23
[[ ${PROMPT_COMMAND-} == __nebula_precmd ]] || exit 24
"#,
        );
    }

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
            env_is_complete: false,
            escape_args: false,
        };
        assert_eq!(cmdline(&options), "echo hello world");

        options.escape_args = true;
        assert_eq!(cmdline(&options), "echo \"hello world\"");
    }
}
