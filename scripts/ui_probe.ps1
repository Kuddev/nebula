# Interactive UI probe pinned to a specific nebula PID so the user's own
# instance is never touched. Uses PrintWindow so screenshots capture the target
# window even when it is occluded. Clicks require foreground; retry + verify.
param(
    [int]$ProcId = 0,
    [string]$Click = "",
    [int]$Scroll = 0,
    [string]$Shot = "",
    [string]$TypeText = "",
    [switch]$Kill
)
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class W {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  public struct RECT { public int L, T, R, B; }
}
'@
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

if ($ProcId -eq 0) {
    $p = Start-Process "D:\temp_build\nebula\target\release\nebula.exe" -ArgumentList '--working-directory','D:\temp_build' -PassThru
    Start-Sleep -Seconds 6
    $p.Refresh()
    Write-Output "started pid=$($p.Id)"
    $ProcId = $p.Id
}
$proc = Get-Process -Id $ProcId -ErrorAction SilentlyContinue
if (-not $proc) { Write-Output "PROCESS $ProcId GONE"; exit 1 }
if ($Kill) { $proc.Kill(); Write-Output "killed $ProcId"; exit 0 }
$h = $proc.MainWindowHandle
if ($h -eq 0) { Write-Output "NO WINDOW for $ProcId"; exit 1 }
$r = New-Object W+RECT
[void][W]::GetWindowRect($h, [ref]$r)
Write-Output "pid=$ProcId rect=($($r.L),$($r.T),$($r.R),$($r.B)) size=$($r.R-$r.L)x$($r.B-$r.T) dpi=$([W]::GetDpiForWindow($h))"

if ($Click -ne "" -or $Scroll -ne 0 -or $TypeText -ne "") {
    # Activation needed for input. Retry until we really own the foreground.
    for ($i = 0; $i -lt 5; $i++) {
        [void][W]::ShowWindow($h, 9)  # SW_RESTORE
        [void][W]::SetForegroundWindow($h)
        Start-Sleep -Milliseconds 400
        if ([W]::GetForegroundWindow() -eq $h) { break }
    }
    if ([W]::GetForegroundWindow() -ne $h) { Write-Output "FOREGROUND FAILED"; exit 1 }
}
if ($Click -ne "") {
    $parts = $Click.Split(','); $cx = [int]$parts[0] + $r.L; $cy = [int]$parts[1] + $r.T
    Write-Output "click client($($parts[0]),$($parts[1])) -> screen($cx,$cy)"
    [void][W]::SetCursorPos($cx, $cy); Start-Sleep -Milliseconds 250
    [W]::mouse_event(2,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    [W]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
}
if ($Scroll -ne 0) {
    $sx = [int]($r.L + ($r.R - $r.L) * 0.6); $sy = [int]($r.T + ($r.B - $r.T) * 0.5)
    [void][W]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 150
    [W]::mouse_event(0x0800, 0, 0, [uint32]($Scroll * 120), [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}
if ($TypeText -ne "") {
    [System.Windows.Forms.SendKeys]::SendWait($TypeText)
    Start-Sleep -Milliseconds 500
}
if ($Shot -ne "") {
    $wr = New-Object W+RECT
    [void][W]::GetWindowRect($h, [ref]$wr)
    $w = $wr.R - $wr.L; $ht = $wr.B - $wr.T
    $bmp = New-Object System.Drawing.Bitmap($w, $ht)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    # PW_RENDERFULLCONTENT (2) captures GPU-composited windows.
    [void][W]::PrintWindow($h, $dc, 2)
    $g.ReleaseHdc($dc)
    $bmp.Save($Shot, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "shot=$Shot"
}
