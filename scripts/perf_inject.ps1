# 向 nebula 实例内部的 shell 注入按键：附加其 ConPTY 控制台（sideload 的
# OpenConsole），对 CONIN$ 调 WriteConsoleInputW。不涉及窗口焦点，不打扰
# 前台用户。供 perf_baseline.ps1 等探针复用。
param(
    [Parameter(Mandatory)][int]$HostPid,
    [Parameter(Mandatory)][string]$Text,
    [switch]$Enter
)
$ErrorActionPreference = 'Stop'

# 沿进程树向下找 shell：nebula -> OpenConsole/conhost -> pwsh
# （不同宿主上 pwsh 的父进程可能是 nebula 也可能是 OpenConsole）。
$procs = Get-CimInstance Win32_Process
$kids = { param($parent) $procs | Where-Object { $_.ParentProcessId -eq $parent } }
$queue = @($HostPid); $shell = $null
while ($queue.Count -gt 0 -and -not $shell) {
    $cur, $queue = $queue
    foreach ($child in (& $kids $cur)) {
        if ($child.Name -match '^(pwsh|powershell|cmd)') { $shell = $child; break }
        $queue += $child.ProcessId
    }
}
if (-not $shell) { Write-Output "NO SHELL under $HostPid"; exit 1 }

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class ConInject {
  [StructLayout(LayoutKind.Sequential)]
  public struct KEY_EVENT_RECORD {
    public int bKeyDown; public ushort wRepeatCount; public ushort wVirtualKeyCode;
    public ushort wVirtualScanCode; public ushort UnicodeChar; public uint dwControlKeyState;
  }
  [StructLayout(LayoutKind.Explicit)]
  public struct INPUT_RECORD {
    [FieldOffset(0)] public ushort EventType;
    [FieldOffset(4)] public KEY_EVENT_RECORD KeyEvent;
  }
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeConsole();
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern IntPtr CreateFileW([MarshalAs(UnmanagedType.LPWStr)] string name, uint access, uint share, IntPtr sec, uint disp, uint flags, IntPtr tmpl);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] recs, uint n, out uint written);
  public static INPUT_RECORD Key(bool down, ushort vk, ushort uc) {
    var r = new INPUT_RECORD(); r.EventType = 1;
    r.KeyEvent.bKeyDown = down ? 1 : 0; r.KeyEvent.wRepeatCount = 1;
    r.KeyEvent.wVirtualKeyCode = vk; r.KeyEvent.wVirtualScanCode = 0;
    r.KeyEvent.UnicodeChar = uc; r.KeyEvent.dwControlKeyState = 0;
    return r;
  }
}
'@

[void][ConInject]::FreeConsole()
if (-not [ConInject]::AttachConsole([uint32]$shell.ProcessId)) {
    Write-Output ("ATTACH FAILED err=" + [Runtime.InteropServices.Marshal]::GetLastWin32Error())
    exit 1
}
$conin = [ConInject]::CreateFileW('CONIN$', [uint32]3221225472, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)

$records = New-Object System.Collections.Generic.List[ConInject+INPUT_RECORD]
foreach ($ch in $Text.ToCharArray()) {
    $code = [uint16][char]$ch
    $records.Add([ConInject]::Key($true, 0, $code))
    $records.Add([ConInject]::Key($false, 0, $code))
}
if ($Enter) {
    $records.Add([ConInject]::Key($true, 13, 13))
    $records.Add([ConInject]::Key($false, 13, 13))
}
$written = [uint32]0
$ok = [ConInject]::WriteConsoleInputW($conin, $records.ToArray(), [uint32]$records.Count, [ref]$written)
Write-Output ("inject ok=$ok written=$written of " + $records.Count + " shell=" + $shell.ProcessId)
if (-not $ok) { exit 1 }
