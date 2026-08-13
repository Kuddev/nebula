# 性能基线（G1 闸门）：向指定终端 exe 注入同一份大流量混合负载，
# 在终端内部自计时（cmd /c type 大文件），量吞吐与内存，追加到 CSV。
#
# 用法:
#   .\scripts\perf_baseline.ps1 -Exe <nebula.exe> -Tag winit-old -ExeArgs '--working-directory D:\temp_build\nebula'
#   .\scripts\perf_baseline.ps1 -Exe <nebula-gpui.exe> -Tag gpui-lab
#
# 方法论:
# - 负载混合 ANSI 256 色/真彩、CJK 宽字符、boxdraw 框线、Powerline 分隔符，
#   贴近 fastfetch/TUI/powerline 的真实着色与整形压力。
# - 计时在终端会话内完成（Stopwatch 包住 type），只量"终端消费 PTY 输出"
#   的速度，不含进程启动与提示符渲染。
# - 命令经 ConPTY 控制台输入缓冲注入（perf_inject.ps1），不碰窗口焦点；
#   窗口置顶但不激活（NOACTIVATE），保证两壳都真实渲染——旧渲染器被完全
#   遮挡时会跳帧，会虚高吞吐。
# - 第一轮为预热（文件缓存/JIT），丢弃；报告中位数。
# - 两壳窗口尺寸可能不同（网格越大每帧工作越多），rect 一并记录供对账。
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Tag,
    # 旧壳必须传 '--working-directory <dir>' 之类的显式意图参数，
    # 否则普通启动会转交常驻实例后退出（mux hand-over），拿不到独立进程。
    [string]$ExeArgs = '',
    [int]$Runs = 3,
    [int]$PayloadMB = 50,
    [int]$TimeoutSec = 240
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$payload = Join-Path $root '.tmp_perf_payload.txt'
$cmdFile = Join-Path $root '.tmp_perf_cmd.ps1'
$resultFile = Join-Path $root '.tmp_perf_result.txt'
$csv = Join-Path $root '.tmp_perf_baseline.csv'
$injector = Join-Path $PSScriptRoot 'perf_inject.ps1'

# ---- 1. 负载生成（按目标大小幂等）----------------------------------------
$targetBytes = $PayloadMB * 1MB
if (-not (Test-Path $payload) -or (Get-Item $payload).Length -lt $targetBytes) {
    Write-Host "generating payload ~${PayloadMB}MB ..."
    $e = [char]27
    $block = [Text.StringBuilder]::new()
    for ($i = 0; $i -lt 240; $i++) {
        $line = switch ($i % 6) {
            0 { "$e[38;5;$(($i * 7) % 256)m colored line $i lorem ipsum dolor sit amet consectetur adipiscing elit$e[0m" }
            1 { "plain ascii line $i the quick brown fox jumps over the lazy dog 0123456789" }
            2 { "中文宽字符混排 第${i}行 终端网格渲染必须逐格对齐 妙笔生花 龍飛鳳舞 テスト" }
            3 { "┌─────┬─────┐ │ box $i │ ▀▄█░▒▓ ├─────┼─────┤ └─────┴─────┘" }
            4 { "$e[48;5;$(($i * 3) % 256)m$e[38;5;15m$([char]0xE0B0) powerline seg $i $([char]0xE0B0)$e[0m tail" }
            5 { "$e[1;31mbold red$e[0m $e[4;32munderline green$e[0m $e[38;2;$(($i*5)%256);128;200mtruecolor$e[0m line $i" }
        }
        [void]$block.AppendLine($line)
    }
    $blockText = $block.ToString()
    $writer = [IO.StreamWriter]::new($payload, $false, [Text.UTF8Encoding]::new($false))
    try {
        while ($writer.BaseStream.Length -lt $targetBytes) { $writer.Write($blockText) }
    } finally { $writer.Dispose() }
}
$payloadLen = (Get-Item $payload).Length

# ---- 2. 终端内自计时的 runner ---------------------------------------------
@"
`$sw = [Diagnostics.Stopwatch]::StartNew()
cmd /c type "$payload"
`$sw.Stop()
"`$(`$sw.Elapsed.TotalMilliseconds)" | Out-File -FilePath "$resultFile" -Encoding ascii
"@ | Set-Content -Path $cmdFile -Encoding ascii

# ---- 3. Win32：按 PID 找顶层窗口、置顶但不激活 -----------------------------
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class PerfWin {
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lp);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
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

function Get-MainWindow([int]$ProcessId) {
    for ($i = 0; $i -lt 40; $i++) {
        $wins = [PerfWin]::WindowsOf($ProcessId)
        if ($wins.Count -gt 0) {
            # 取最宽者（宿主可能带辅助小窗）。
            $best = $null; $bestW = -1
            foreach ($h in $wins) {
                $r = New-Object PerfWin+RECT
                [void][PerfWin]::GetWindowRect($h, [ref]$r)
                if (($r.R - $r.L) -gt $bestW) { $bestW = $r.R - $r.L; $best = $h }
            }
            return $best
        }
        Start-Sleep -Milliseconds 500
    }
    throw "no visible window for pid $ProcessId"
}

# ---- 4. 驱动 ----------------------------------------------------------------
Write-Host "launching $Exe $ExeArgs"
$proc = if ($ExeArgs) {
    Start-Process -FilePath $Exe -ArgumentList $ExeArgs -PassThru
} else {
    Start-Process -FilePath $Exe -PassThru
}
Start-Sleep -Seconds 7   # 等 shell 与提示符就绪
if ($proc.HasExited) { throw "terminal exited immediately (mux hand-over? pass -ExeArgs)" }
$hwnd = Get-MainWindow -ProcessId $proc.Id
$rect = New-Object PerfWin+RECT
[void][PerfWin]::GetWindowRect($hwnd, [ref]$rect)
$rectStr = "$($rect.R - $rect.L)x$($rect.B - $rect.T)"
# HWND_TOPMOST=-1; SWP_NOSIZE|NOMOVE|NOACTIVATE = 0x13：可见但不抢焦点。
[void][PerfWin]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x13)
Write-Host "window $rectStr pid $($proc.Id)"

$command = "powershell -NoProfile -ExecutionPolicy Bypass -File $cmdFile"

function Invoke-Injected([int]$HostPid, [string]$Text) {
    for ($attempt = 0; $attempt -lt 5; $attempt++) {
        $out = & powershell -NoProfile -ExecutionPolicy Bypass -File $injector `
            -HostPid $HostPid -Text $Text -Enter 2>&1 | Out-String
        if ($out -match 'ok=True') { return $true }
        Start-Sleep -Seconds 2
    }
    Write-Warning "inject failed: $out"
    return $false
}

$times = @()
for ($run = 0; $run -le $Runs; $run++) {
    Remove-Item $resultFile -ErrorAction SilentlyContinue
    if (-not (Invoke-Injected -HostPid $proc.Id -Text $command)) { continue }

    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while (-not (Test-Path $resultFile) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    if (-not (Test-Path $resultFile)) {
        Write-Warning "run ${run}: timeout waiting for result"
        continue
    }
    Start-Sleep -Milliseconds 300
    $ms = [double](Get-Content $resultFile -Raw).Trim()
    $mbps = [Math]::Round($payloadLen / 1MB / ($ms / 1000.0), 1)
    if ($run -eq 0) {
        Write-Host ("warmup: {0,8:N0} ms  {1,6} MB/s (discarded)" -f $ms, $mbps)
    } else {
        Write-Host ("run ${run}:  {0,8:N0} ms  {1,6} MB/s" -f $ms, $mbps)
        $times += $ms
    }
    Start-Sleep -Seconds 1
}

$ws = [Math]::Round((Get-Process -Id $proc.Id).WorkingSet64 / 1MB, 1)
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue

if ($times.Count -eq 0) { throw "no successful runs" }
$sorted = $times | Sort-Object
$median = $sorted[[int](($sorted.Count - 1) / 2)]
$medianMbps = [Math]::Round($payloadLen / 1MB / ($median / 1000.0), 1)
Write-Host ("== $Tag  median {0:N0} ms  {1} MB/s  ws {2} MB  window $rectStr" -f $median, $medianMbps, $ws)

if (-not (Test-Path $csv)) {
    'timestamp,tag,window,payload_mb,runs,median_ms,median_mbps,workingset_mb' | Set-Content $csv -Encoding ascii
}
"$(Get-Date -Format s),$Tag,$rectStr,$([Math]::Round($payloadLen/1MB)),$($times.Count),$([Math]::Round($median)),$medianMbps,$ws" |
    Add-Content $csv -Encoding ascii
