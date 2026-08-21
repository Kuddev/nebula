[CmdletBinding()]
param(
    # 每轮爆发的字节数。必须显著超过 event_loop 的 MAX_LOCKED_READ(65535)，
    # 否则 pty_read 会一路读到管道为空，走的是「已注册 waker」的安全路径。
    [ValidateRange(65536, 8388608)]
    [int]$BurstBytes = 163840,

    # 爆发后的静默时长。这是复现的另一半条件：源头一静默，就再没有新的
    # 写入来唤醒 read waker，滞留的尾巴永远等不到下一次投递。
    [ValidateRange(100, 60000)]
    [int]$SilenceMs = 1200,

    # 等待 CSI 6n 的 CPR 回复。超时即判定这一轮的尾部字节没有被解析。
    [ValidateRange(200, 30000)]
    [int]$CprTimeoutMs = 2500,

    [ValidateRange(0, 100000)]
    [int]$Cycles = 0,

    [ValidateRange(4, 512)]
    [int]$MaxLogEvents = 64,

    [string]$LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "nebula-pty-drain-$PID.jsonl")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$esc = [string][char]27
$maxLockedRead = 65535
$milestones = @(10, 25, 100, 400, 1600)

$script:cycle = 0
$script:stalls = 0
$script:logEventCount = 0
$script:nextMilestone = 0
$script:startedAt = [DateTime]::UtcNow
$script:stdout = [Console]::OpenStandardOutput()
$script:lastCpr = ''

function Write-Text {
    param([Parameter(Mandatory = $true)][string]$Text)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $script:stdout.Write($bytes, 0, $bytes.Length)
    $script:stdout.Flush()
}

function Write-ProbeEvent {
    param(
        [Parameter(Mandatory = $true)][string]$EventName,
        [hashtable]$Data = @{}
    )
    if ($script:logEventCount -ge $MaxLogEvents) { return }
    $record = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString('O')
        event = $EventName
        pid = $PID
        cycle = $script:cycle
        stalls = $script:stalls
        elapsed_ms = [long]([DateTime]::UtcNow - $script:startedAt).TotalMilliseconds
    }
    foreach ($k in $Data.Keys) { $record[$k] = $Data[$k] }
    Add-Content -LiteralPath $LogPath -Encoding UTF8 -Value ($record | ConvertTo-Json -Compress -Depth 4)
    $script:logEventCount++
}

function Clear-StdinBacklog {
    try {
        while ([Console]::KeyAvailable) { [void][Console]::ReadKey($true) }
    }
    catch {
        # 输入被重定向时没有控制台输入，此时 CPR 判据不可用。
    }
}

# 等待终端把 CSI 6n 的回复写回 conin。收到即证明爆发的【尾部】已经被
# VT 解析器消费；超时则说明尾部字节还躺在管道里没人读。
function Wait-Cpr {
    param([Parameter(Mandatory = $true)][int]$TimeoutMs)

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $sb = [System.Text.StringBuilder]::new(24)
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $available = $false
        try { $available = [Console]::KeyAvailable } catch { return $null }
        if ($available) {
            $key = [Console]::ReadKey($true)
            $ch = $key.KeyChar
            if ($ch -ne [char]0) { [void]$sb.Append($ch) }
            # CPR 形如 ESC [ row ; col R，读到结尾的 R 就算完整。
            if ($ch -eq 'R') {
                $script:lastCpr = $sb.ToString() -replace [regex]::Escape($esc), '<ESC>'
                return [long]$sw.ElapsedMilliseconds
            }
            continue
        }
        Start-Sleep -Milliseconds 15
    }
    return $null
}

# 爆发体：整屏彩色重绘，凑到 BurstBytes 以上。刻意不使用 synchronized
# update —— 复现与 CSI ?2026 无关，这样一旦命中就排除了同步更新缓冲。
function New-Burst {
    param([Parameter(Mandatory = $true)][int]$TargetBytes)

    $cols = 100
    $rows = 20
    try { $cols = [Math]::Max(20, [Console]::WindowWidth); $rows = [Math]::Max(6, [Console]::WindowHeight - 6) } catch { }

    $glyphs = '#*=+~%'
    $sb = [System.Text.StringBuilder]::new($TargetBytes + 8192)
    $pass = 0
    while ($sb.Length -lt $TargetBytes) {
        for ($r = 0; $r -lt $rows; $r++) {
            [void]$sb.Append($esc).Append('[').Append($r + 4).Append(';1H')
            for ($c = 0; $c -lt $cols; $c++) {
                $t = ($r * 0.21) + ($c * 0.06) + ($pass * 0.7)
                $red = [int](140 + 110 * [Math]::Sin($t))
                $green = [int](140 + 110 * [Math]::Sin($t + 2.094))
                $blue = [int](140 + 110 * [Math]::Sin($t + 4.188))
                [void]$sb.Append($esc).Append('[38;2;').Append($red).Append(';').Append($green)
                    .Append(';').Append($blue).Append('m').Append($glyphs[($r + $c + $pass) % $glyphs.Length])
            }
            [void]$sb.Append($esc).Append('[0m')
        }
        $pass++
    }
    return $sb.ToString()
}

try {
    Set-Content -LiteralPath $LogPath -Encoding UTF8 -Value ''
    Clear-Content -LiteralPath $LogPath

    $burst = New-Burst -TargetBytes $BurstBytes
    $burstBytes = [System.Text.Encoding]::UTF8.GetByteCount($burst)

    Write-Text "$esc[2J$esc[H"
    Write-Text ("Nebula PTY drain probe  pid=$PID  burst=$burstBytes B  (MAX_LOCKED_READ=$maxLockedRead)`r`n")
    Write-Text ("silence=${SilenceMs}ms  cpr_timeout=${CprTimeoutMs}ms  NO synchronized-update`r`n")
    Write-Text ("log=$LogPath`r`n")

    Write-ProbeEvent -EventName 'probe-start' -Data @{
        burst_bytes = $burstBytes
        max_locked_read = $maxLockedRead
        silence_ms = $SilenceMs
        cpr_timeout_ms = $CprTimeoutMs
        verdict_rule = 'CPR timeout => burst tail was never parsed => bytes stranded in the pty pipe'
        powershell = $PSVersionTable.PSVersion.ToString()
    }

    while ($true) {
        if ($Cycles -gt 0 -and $script:cycle -ge $Cycles) { break }
        $script:cycle++

        Clear-StdinBacklog

        # 一次性灌入，让字节在 event_loop 的一轮 poll 之间堆进管道；
        # CSI 6n 紧贴爆发尾部，因此它必然落在可能被滞留的那一段里。
        Write-Text $burst
        Write-Text ("$esc[1;1H$esc[2KCYCLE $('{0:D6}' -f $script:cycle)  stalls=$($script:stalls)  last_cpr=$($script:lastCpr)")
        Write-Text "$esc[6n"

        $cprMs = Wait-Cpr -TimeoutMs $CprTimeoutMs
        if ($null -eq $cprMs) {
            $script:stalls++
            # 限流：首次必记，之后按 8 的倍数记，避免高复现率把日志刷满。
            if ($script:stalls -eq 1 -or $script:stalls % 8 -eq 0) {
                Write-ProbeEvent -EventName 'stall-detected' -Data @{
                    burst_bytes = $burstBytes
                    cpr_timeout_ms = $CprTimeoutMs
                    verdict = 'no CPR reply: burst tail unparsed, bytes stranded in pipe'
                }
            }
        }

        while ($script:nextMilestone -lt $milestones.Count -and $script:cycle -ge $milestones[$script:nextMilestone]) {
            $rate = if ($script:cycle -gt 0) { [Math]::Round(100.0 * $script:stalls / $script:cycle, 1) } else { 0 }
            Write-ProbeEvent -EventName 'cycle-milestone' -Data @{
                milestone_cycles = $milestones[$script:nextMilestone]
                stall_rate_percent = $rate
            }
            $script:nextMilestone++
        }

        try {
            if ([Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true)
                if ($key.Key -eq [ConsoleKey]::Q -or $key.Key -eq [ConsoleKey]::Escape) { break }
            }
        }
        catch { }

        Start-Sleep -Milliseconds $SilenceMs
    }
}
finally {
    $elapsed = [DateTime]::UtcNow - $script:startedAt
    $rate = if ($script:cycle -gt 0) { [Math]::Round(100.0 * $script:stalls / $script:cycle, 1) } else { 0 }
    Write-Text "$esc[0m`r`n"
    Write-Text ("probe stopped: cycles={0} stalls={1} ({2}%) elapsed={3:N1}s`r`n" -f `
            $script:cycle, $script:stalls, $rate, $elapsed.TotalSeconds)
    Write-ProbeEvent -EventName 'probe-stop' -Data @{
        stall_rate_percent = $rate
        log_event_count = $script:logEventCount
    }
}
