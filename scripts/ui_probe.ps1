# Interactive UI probe pinned to a specific nebula PID so the user's own
# instance is never touched. Uses PrintWindow so screenshots capture the target
# window even when it is occluded. Clicks require foreground; retry + verify.
param(
    [int]$ProcId = 0,
    [string]$Click = "",
    [string]$RightClick = "",
    [int]$Scroll = 0,
    [int]$CtrlScroll = 0,
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
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr lparam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lparam);
  // Process.MainWindowHandle picks whatever top-level window answers first --
  // for nebula that is a 6x6 helper window (and a minimized one reports
  // -32000,-32000). Enumerate instead and keep the largest visible window
  // owned by the pid: that is the real shell window in both shells.
  public static IntPtr FindMainWindow(int pid) {
    IntPtr best = IntPtr.Zero;
    long bestArea = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      uint wpid;
      GetWindowThreadProcessId(h, out wpid);
      if (wpid != (uint)pid) return true;
      if (!IsWindowVisible(h)) return true;
      RECT r;
      GetWindowRect(h, out r);
      long area = (long)(r.R - r.L) * (long)(r.B - r.T);
      // Minimized windows sit at -32000; never let one win the comparison.
      if (r.L <= -30000 || r.T <= -30000) return true;
      if (area > bestArea) { bestArea = area; best = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
  public struct RECT { public int L, T, R, B; }
}
'@
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
# Physical-pixel truth: without this the probe process is DPI-unaware and
# PrintWindow/GetWindowRect return virtualized (shrunken) coordinates —
# screenshots silently lie about glyph sizes on high-DPI monitors.
Add-Type -MemberDefinition '[DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int value);' -Name Dpi -Namespace W2
[void][W2.Dpi]::SetProcessDpiAwareness(2)

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
$h = [W]::FindMainWindow($ProcId)
if ($h -eq [IntPtr]::Zero) {
    # Fall back to the (unreliable) handle so a minimized-only state still
    # reports something actionable instead of looking like a dead process.
    $h = $proc.MainWindowHandle
    if ($h -eq 0) { Write-Output "NO WINDOW for $ProcId"; exit 1 }
    Write-Output "WARN: no visible top-level window; falling back to MainWindowHandle"
}
$r = New-Object W+RECT
[void][W]::GetWindowRect($h, [ref]$r)
Write-Output "pid=$ProcId rect=($($r.L),$($r.T),$($r.R),$($r.B)) size=$($r.R-$r.L)x$($r.B-$r.T) dpi=$([W]::GetDpiForWindow($h))"

if ($Click -ne "" -or $RightClick -ne "" -or $Scroll -ne 0 -or $CtrlScroll -ne 0 -or $TypeText -ne "") {
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
if ($RightClick -ne "") {
    $parts = $RightClick.Split(','); $cx = [int]$parts[0] + $r.L; $cy = [int]$parts[1] + $r.T
    Write-Output "right-click client($($parts[0]),$($parts[1])) -> screen($cx,$cy)"
    [void][W]::SetCursorPos($cx, $cy); Start-Sleep -Milliseconds 250
    [W]::mouse_event(8,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    [W]::mouse_event(16,0,0,0,[UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
}
if ($Scroll -ne 0) {
    $sx = [int]($r.L + ($r.R - $r.L) * 0.6); $sy = [int]($r.T + ($r.B - $r.T) * 0.5)
    [void][W]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 150
    # WHEEL_DELTA is signed (negative scrolls down) but the P/Invoke signature
    # declares dwData as uint: casting a negative int straight to [uint32]
    # throws InvalidCast. Mask to the two's-complement bit pattern instead.
    $wheel = [uint32]([int64]($Scroll * 120) -band 0xFFFFFFFFL)
    [W]::mouse_event(0x0800, 0, 0, $wheel, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}
if ($CtrlScroll -ne 0) {
    # Ctrl held around the wheel ticks: terminal font zoom. The key-up runs
    # in `finally` — a mid-loop error must NEVER strand a system-wide
    # synthetic Ctrl (it turns every later wheel into font zoom).
    $sx = [int]($r.L + ($r.R - $r.L) * 0.6); $sy = [int]($r.T + ($r.B - $r.T) * 0.5)
    [void][W]::SetCursorPos($sx, $sy); Start-Sleep -Milliseconds 150
    [W]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)  # VK_CONTROL down
    try {
        Start-Sleep -Milliseconds 80
        $ticks = [Math]::Abs($CtrlScroll); $dir = [Math]::Sign($CtrlScroll)
        # Negative wheel deltas must go through a bit-pattern cast: [uint32](-120) throws.
        $delta = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]($dir * 120)), 0)
        for ($i = 0; $i -lt $ticks; $i++) {
            [W]::mouse_event(0x0800, 0, 0, $delta, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 120
        }
    } finally {
        [W]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)  # VK_CONTROL up (KEYEVENTF_KEYUP)
    }
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
