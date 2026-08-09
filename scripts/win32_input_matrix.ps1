# Win32-input-mode key matrix replay test (future_planning: 9001 端到端矩阵 / 诊断与回放工具).
#
# Drives a minimized Nebula instance via PostMessage (no focus stealing — safe
# to run while the user is typing elsewhere) and asserts the byte sequence a
# node reader (= Claude Code's input path) receives through the sideloaded
# OpenConsole, against a checked-in baseline.
#
#   powershell -File scripts\win32_input_matrix.ps1           # assert vs baseline
#   powershell -File scripts\win32_input_matrix.ps1 -Record   # (re)record baseline
#   powershell -File scripts\win32_input_matrix.ps1 -Exe <path-to-nebula.exe>
#
# LIMITS: PostMessage does not alter the real keyboard state, so modifier
# CHORDS (Shift+Enter, Ctrl+Space…) cannot be tested unattended — the layout
# code would read the real (unpressed) modifier state. Chord coverage needs a
# foreground SendInput run on an idle machine; keep that a manual step.
param(
    [switch]$Record,
    [string]$Exe = 'D:\temp_build\nebula\target\release\nebula.exe'
)
$ErrorActionPreference = 'Continue'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PM {
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, UIntPtr w, IntPtr l);
}
'@

$root = 'D:\temp_build\nebula'
$log = Join-Path $root '.tmp_win32_matrix.log'
$baselineFile = Join-Path $root 'scripts\win32_input_matrix.baseline.txt'
Remove-Item $log -ErrorAction SilentlyContinue

$p = Start-Process $Exe -WindowStyle Minimized -ArgumentList @(
    '--working-directory', $root,
    '-e','node', (Join-Path $root 'scripts\win32_input_matrix_probe.cjs'), $log
) -PassThru
$ready = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    if ((Test-Path $log) -and (Get-Content $log -Raw) -match 'ready') { $ready = $true; break }
}
if (-not $ready) { Write-Output 'FAIL: probe never ready'; if (-not $p.HasExited) { $p.Kill() }; exit 1 }
$p.Refresh()
$h = $p.MainWindowHandle
Start-Sleep -Seconds 1

$WM_KEYDOWN = 0x100; $WM_KEYUP = 0x101
function Send-Key([IntPtr]$hwnd, [int]$vk, [int]$scan, [bool]$ext = $false) {
    $extBit = if ($ext) { [int64]1 -shl 24 } else { 0 }
    $down = [IntPtr]([int64]($scan -shl 16) -bor 1 -bor $extBit)
    $up   = [IntPtr]([int64]0xC0000000 + ($scan -shl 16) + 1 + $extBit)
    [void][PM]::PostMessageW($hwnd, $WM_KEYDOWN, [UIntPtr][uint32]$vk, $down)
    Start-Sleep -Milliseconds 60
    [void][PM]::PostMessageW($hwnd, $WM_KEYUP, [UIntPtr][uint32]$vk, $up)
    Start-Sleep -Milliseconds 240
}

# Matrix: name, vk, scan, extended. Unmodified keys only (see LIMITS above).
$matrix = @(
    @('a',         0x41, 0x1E, $false),
    @('Esc',       0x1B, 0x01, $false),
    @('Enter',     0x0D, 0x1C, $false),
    @('Tab',       0x09, 0x0F, $false),
    @('Backspace', 0x08, 0x0E, $false),
    @('Up',        0x26, 0x48, $true),
    @('Down',      0x28, 0x50, $true),
    @('Home',      0x24, 0x47, $true),
    @('Delete',    0x2E, 0x53, $true),
    @('F1',        0x70, 0x3B, $false),
    @('F5',        0x74, 0x3F, $false),
    @('ShiftTap',  0x10, 0x2A, $false),   # bare modifier: must produce NO bytes
    @('b',         0x42, 0x30, $false)    # sentinel after the silent modifier
)
foreach ($k in $matrix) { Send-Key $h $k[1] $k[2] $k[3] }
Send-Key $h 0x51 0x10   # q -> probe exits
Start-Sleep -Seconds 2
if (-not $p.HasExited) { $p.Kill() }

# Concatenate every rx chunk into one hex string (order-preserving).
$rx = (Select-String -Path $log -Pattern 'rx ([0-9a-f]+)' -AllMatches).Matches |
    ForEach-Object { $_.Groups[1].Value }
$sequence = ($rx -join '')

if ($Record) {
    Set-Content -Path $baselineFile -Value $sequence
    Write-Output "RECORDED baseline: $sequence"
    exit 0
}
if (-not (Test-Path $baselineFile)) {
    Write-Output "FAIL: no baseline (run with -Record first). Got: $sequence"
    exit 1
}
$baseline = (Get-Content $baselineFile -Raw).Trim()
if ($sequence -eq $baseline) {
    Write-Output "PASS ($($rx.Count) chunks): $sequence"
    exit 0
} else {
    Write-Output "FAIL"
    Write-Output "  expected: $baseline"
    Write-Output "  got:      $sequence"
    exit 1
}
