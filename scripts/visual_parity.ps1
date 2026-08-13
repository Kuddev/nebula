# 视觉对账（G3 闸门）：向终端注入一份固定样张，截图存档，供新旧两壳
# 并排对比。样张覆盖 GPUI 迁移最容易翻车的渲染面：
#   boxdraw 全家桶（单/重/双线、圆角、虚线、对角线）、块元素与浓度块、
#   象限块、Powerline 分隔符（三角/箭头/圆头）、CJK 对齐标尺、
#   256 色与真彩渐变。
# fastfetch 若在 PATH 上则追加真实 fastfetch 输出。
#
# 用法:
#   .\scripts\visual_parity.ps1 -Exe <nebula.exe> -Tag winit-old -ExeArgs '--working-directory D:\temp_build\nebula'
#   .\scripts\visual_parity.ps1 -Exe <nebula-gpui.exe> -Tag gpui-lab
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Tag,
    [string]$ExeArgs = '',
    [int]$SettleSec = 4
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$contentFile = Join-Path $root '.tmp_parity_content.ps1'
$injector = Join-Path $PSScriptRoot 'perf_inject.ps1'
$outPng = Join-Path $root ".tmp_parity_$Tag.png"

# ---- 1. 样张（每次重写，保持权威版本在本脚本内）---------------------------
$e = [char]27
@"
`$e = [char]27
Write-Host "== visual parity sample =="
Write-Host '-- boxdraw: light / heavy / double / rounded / dashes / diagonals'
Write-Host '┌───┬───┐  ┏━━━┳━━━┓  ╔═══╦═══╗  ╭───╮'
Write-Host '│ a │ b │  ┃ c ┃ d ┃  ║ e ║ f ║  │ g │'
Write-Host '├───┼───┤  ┣━━━╋━━━┫  ╠═══╬═══╣  ╰───╯'
Write-Host '└───┴───┘  ┗━━━┻━━━┛  ╚═══╩═══╝  ┄┄┅┅┈┈┉┉ ╌╌╍╍'
Write-Host 'mixed: ┍┑┕┙ ┎┒┖┚ ╒╕╘╛ ╓╖╙╜  half: ╴╵╶╷╸╹╺╻  trans: ╼╽╾╿'
Write-Host 'diag: ╱╲╳╱╲╳  vertical dashes: ┆┇┊┋╎╏'
Write-Host '-- blocks / shades / quadrants'
Write-Host '▁▂▃▄▅▆▇█  ▏▎▍▌▋▊▉█  ░░▒▒▓▓██  ▀▄▌▐▔▕'
Write-Host '▖▗▘▝▚▞▙▛▜▟  legacy: 🬀🬁🬂🬃🬄🬅🬆🬇🬈🬉🬊'
Write-Host '-- powerline separators'
Write-Host "`$e[48;5;25m`$e[38;5;15m one `$e[0m`$e[38;5;25m`$e[48;5;208m$([char]0xE0B0)`$e[38;5;15m two `$e[0m`$e[38;5;208m`$e[48;5;28m$([char]0xE0B0)`$e[38;5;15m three `$e[0m`$e[38;5;28m$([char]0xE0B0)`$e[0m done"
Write-Host "chevrons: $([char]0xE0B1) $([char]0xE0B1) rtl: $([char]0xE0B2) $([char]0xE0B3)  round: $([char]0xE0B4) $([char]0xE0B6)"
Write-Host '-- CJK alignment ruler (竖线必须逐列对齐)'
Write-Host '|12345678901234567890123456789012|'
Write-Host '|中文宽字符对齐测试一二三四五六七八|'
Write-Host '|あいうえおかきくけこさしすせそたち|'
Write-Host '|ab中cd文ef字gh宽ij齐kl测mn试op对qr|'
Write-Host '-- colors'
`$bar = ''
foreach (`$i in 16..51) { `$bar += "`$e[48;5;`${i}m " }
Write-Host "`$bar`$e[0m"
`$grad = ''
foreach (`$i in 0..31) { `$r = `$i * 8; `$grad += "`$e[48;2;`${r};64;`$((255 - `$r))m " }
Write-Host "`$grad`$e[0m"
if (Get-Command fastfetch -ErrorAction SilentlyContinue) { fastfetch }
Write-Host '== end of sample =='
"@ | Set-Content -Path $contentFile -Encoding UTF8

# ---- 2. Win32 助手 ----------------------------------------------------------
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class ParityWin {
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    public struct RECT { public int L, T, R, B; }
    delegate bool EnumProc(IntPtr h, IntPtr lp);
    public static List<IntPtr> WindowsOf(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, lp) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid && IsWindowVisible(h)) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
'@
Add-Type -AssemblyName System.Drawing
Add-Type -MemberDefinition '[DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int value);' -Name Dpi -Namespace ParityDpi
[void][ParityDpi.Dpi]::SetProcessDpiAwareness(2)

function Get-MainWindow([int]$ProcessId) {
    for ($i = 0; $i -lt 40; $i++) {
        $wins = [ParityWin]::WindowsOf($ProcessId)
        if ($wins.Count -gt 0) {
            $best = $null; $bestW = -1
            foreach ($h in $wins) {
                $r = New-Object ParityWin+RECT
                [void][ParityWin]::GetWindowRect($h, [ref]$r)
                if (($r.R - $r.L) -gt $bestW) { $bestW = $r.R - $r.L; $best = $h }
            }
            return $best
        }
        Start-Sleep -Milliseconds 500
    }
    throw "no visible window for pid $ProcessId"
}

# ---- 3. 驱动：启动 → 注入 → 截图 → 清理 ------------------------------------
Write-Host "launching $Exe $ExeArgs"
$proc = if ($ExeArgs) {
    Start-Process -FilePath $Exe -ArgumentList $ExeArgs -PassThru
} else {
    Start-Process -FilePath $Exe -PassThru
}
Start-Sleep -Seconds 7
if ($proc.HasExited) { throw "terminal exited immediately (mux hand-over? pass -ExeArgs)" }
$hwnd = Get-MainWindow -ProcessId $proc.Id
# 可见但不抢焦点（HWND_TOPMOST, SWP_NOSIZE|NOMOVE|NOACTIVATE）。
[void][ParityWin]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x13)

$command = "powershell -NoProfile -ExecutionPolicy Bypass -File $contentFile"
$ok = $false
for ($attempt = 0; $attempt -lt 5; $attempt++) {
    $out = & powershell -NoProfile -ExecutionPolicy Bypass -File $injector `
        -HostPid $proc.Id -Text $command -Enter 2>&1 | Out-String
    if ($out -match 'ok=True') { $ok = $true; break }
    Start-Sleep -Seconds 2
}
if (-not $ok) { Stop-Process -Id $proc.Id -Force; throw "inject failed: $out" }
Start-Sleep -Seconds $SettleSec

$r = New-Object ParityWin+RECT
[void][ParityWin]::GetWindowRect($hwnd, [ref]$r)
$w = $r.R - $r.L; $h = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
# PW_RENDERFULLCONTENT = 2：GPU 合成（D3D/GL）表面必需。
$shot = [ParityWin]::PrintWindow($hwnd, $dc, 2)
$g.ReleaseHdc($dc); $g.Dispose()
$bmp.Save($outPng, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "shot ok=$shot ${w}x${h} -> $outPng"

Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
