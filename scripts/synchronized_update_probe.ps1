[CmdletBinding()]
param(
    [ValidateSet('Timeout', 'WellFormed')]
    [string]$Mode = 'Timeout',

    [ValidateRange(5, 1000)]
    [int]$FrameIntervalMs = 40,

    [ValidateRange(1, 3600)]
    [int]$NestedBeginIntervalSeconds = 5,

    [ValidateRange(0, 86400)]
    [int]$DurationSeconds = 0,

    [ValidateRange(4, 1024)]
    [int]$MaxLogEvents = 64,

    [int[]]$ContextMilestonesSeconds = @(30, 120, 300, 600),

    [ValidateRange(65536, 2000000)]
    [int]$SyncBufferRotationBytes = 1500000,

    [string]$LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "nebula-sync-probe-$PID.log")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$esc = [char]27
$beginSync = [string]::Concat($esc, '[?2026h')
$endSync = [string]::Concat($esc, '[?2026l')
$eraseLine = [string]::Concat($esc, '[2K')
$startedAt = [DateTime]::UtcNow
$cycle = 0
$tick = [long]0
$syncOpen = $false
$lastSize = $null
$logEventCount = 0
$milestones = @($ContextMilestonesSeconds | Where-Object { $_ -gt 0 } | Sort-Object -Unique)

function Write-Raw {
    param([Parameter(Mandatory = $true)][string]$Text)

    [Console]::Out.Write($Text)
    [Console]::Out.Flush()
}

function Write-ProbeEvent {
    param(
        [Parameter(Mandatory = $true)][string]$Event,
        [hashtable]$Data = @{}
    )

    if ($script:logEventCount -ge $MaxLogEvents) {
        return
    }

    $record = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString('O')
        event = $Event
        pid = $PID
        mode = $Mode
        cycle = $script:cycle
        tick = $script:tick
        size = Get-ConsoleSizeKey
        sync_open = $script:syncOpen
        emitted_sync_bytes = $script:emittedSyncBytes
        nested_begin_count = $script:nestedBeginCount
    }
    foreach ($key in $Data.Keys) {
        $record[$key] = $Data[$key]
    }

    $json = $record | ConvertTo-Json -Compress -Depth 4
    Add-Content -LiteralPath $LogPath -Encoding UTF8 -Value $json
    $script:logEventCount++
}

function Get-ConsoleSizeKey {
    try {
        return '{0}x{1}' -f [Console]::WindowWidth, [Console]::WindowHeight
    }
    catch {
        return $null
    }
}

function Read-ProbeKey {
    try {
        if ([Console]::KeyAvailable) {
            return [Console]::ReadKey($true)
        }
    }
    catch {
        # 重定向运行时没有 Console input；定时退出模式仍可用于语法/流水线验证。
    }

    return $null
}

function Close-SyncFrame {
    if ($script:syncOpen) {
        Write-Raw $script:endSync
        $script:syncOpen = $false
    }
}

function Write-SyncPayload {
    param([Parameter(Mandatory = $true)][string]$Text)

    $script:emittedSyncBytes += [Text.Encoding]::UTF8.GetByteCount($Text)
    Write-Raw $Text
}

function Start-ProbeCycle {
    $script:cycle++
    $script:cycleStartedAt = [DateTime]::UtcNow
    $script:lastSize = Get-ConsoleSizeKey
    $script:nextNestedBegin = [DateTime]::UtcNow.AddSeconds($NestedBeginIntervalSeconds)
    $script:timeoutDeadlineLogged = $false
    $script:nextMilestoneIndex = 0
    $script:emittedSyncBytes = [long]0
    $script:nestedBeginCount = 0

    Write-Raw "`r`n"
    Write-Raw "=== Nebula synchronized-update probe ===`r`n"
    Write-Raw ("mode={0} cycle={1} pid={2} size={3}`r`n" -f $Mode, $script:cycle, $PID, $script:lastSize)
    Write-Raw "Log: $LogPath`r`n"
    Write-Raw "Leave this tab in the background, then return later.`r`n"
    Write-Raw "Q/Esc/Ctrl+C exits. Any other key starts a fresh cycle.`r`n"

    if ($Mode -eq 'Timeout') {
        Write-Raw "Expected: TICK becomes visible within about one second and keeps moving.`r`n"
        Write-Raw "Suspected bug: this WAITING line remains the live bottom indefinitely.`r`n"
        Write-Raw "TICK: WAITING FOR THE TERMINAL SYNC TIMEOUT"
        Write-SyncPayload $script:beginSync
        $script:syncOpen = $true
    }
    else {
        Write-Raw "Expected: well-formed synchronized frames keep updating continuously.`r`n"
        Write-Raw "TICK: STARTING"
    }

    Write-ProbeEvent -Event 'cycle-start' -Data @{
        frame_interval_ms = $FrameIntervalMs
        nested_begin_interval_seconds = $NestedBeginIntervalSeconds
        sync_buffer_rotation_bytes = $SyncBufferRotationBytes
    }
}

function Recover-ProbeCycle {
    param(
        [Parameter(Mandatory = $true)][string]$Reason,
        [Parameter(Mandatory = $true)][TimeSpan]$Elapsed
    )

    # 模拟 Codex/CC 在输入或 SIGWINCH 后完成一次绘制；先闭合旧帧，保证
    # 故障现场中的缓冲内容一次提交，同时避免探针污染后续 shell 会话。
    Close-SyncFrame
    Write-Raw "`r`n"
    Write-Raw ("RECOVERED reason={0} tick={1} elapsed={2:N1}s`r`n" -f $Reason, $script:tick, $Elapsed.TotalSeconds)
    Write-ProbeEvent -Event 'recovered' -Data @{
        reason = $Reason
        process_elapsed_ms = [long]$Elapsed.TotalMilliseconds
        cycle_elapsed_ms = [long]([DateTime]::UtcNow - $script:cycleStartedAt).TotalMilliseconds
    }
}

$initialRecord = [ordered]@{
    timestamp = $startedAt.ToString('O')
    event = 'probe-start'
    pid = $PID
    mode = $Mode
    frame_interval_ms = $FrameIntervalMs
    nested_begin_interval_seconds = $NestedBeginIntervalSeconds
    duration_seconds = $DurationSeconds
    max_log_events = $MaxLogEvents
    sync_buffer_rotation_bytes = $SyncBufferRotationBytes
    context_milestones_seconds = $milestones
    powershell = $PSVersionTable.PSVersion.ToString()
    term = $env:TERM
    term_program = $env:TERM_PROGRAM
    wt_session = $env:WT_SESSION
    size = Get-ConsoleSizeKey
}
Set-Content -LiteralPath $LogPath -Encoding UTF8 -Value ($initialRecord | ConvertTo-Json -Compress -Depth 4)
$logEventCount = 1

try {
    Start-ProbeCycle

    while ($true) {
        $now = [DateTime]::UtcNow
        $elapsed = $now - $startedAt
        if ($DurationSeconds -gt 0 -and $elapsed.TotalSeconds -ge $DurationSeconds) {
            break
        }

        $tick++
        if ($Mode -eq 'WellFormed') {
            # 对照组严格成对输出 BSU/ESU；仍以 Codex 相近的高频整帧更新压测
            # 后台 Tab 的 PTY reader、Wakeup 队列和 GPUI 重绘链。
            Write-Raw ($beginSync + "`r" + $eraseLine + ('TICK: {0:D12}' -f $tick) + $endSync)
        }
        else {
            # 故意不发送 ESU。VTE 规定 150ms 后应强制提交；短小帧保持 PTY
            # 活跃，又让 2 MiB 同步缓冲上限在数十分钟内不会掩盖 timeout 问题。
            Write-SyncPayload ("`r" + $eraseLine + ('TICK: {0:D12}' -f $tick))
            if ($now -ge $nextNestedBegin) {
                # 富 TUI 的连续 draw 可能在旧帧未闭合时开始下一帧；新的 BSU 会
                # 延长同步期限，是复现长期后台会话时序的关键压力条件。
                Write-SyncPayload $beginSync
                $nestedBeginCount++
                $nextNestedBegin = $now.AddSeconds($NestedBeginIntervalSeconds)
            }
        }

        $cycleElapsed = $now - $cycleStartedAt
        if ($Mode -eq 'Timeout' -and !$timeoutDeadlineLogged -and $cycleElapsed.TotalMilliseconds -ge 250) {
            $timeoutDeadlineLogged = $true
            Write-ProbeEvent -Event 'sync-timeout-deadline-crossed' -Data @{
                cycle_elapsed_ms = [long]$cycleElapsed.TotalMilliseconds
                expected_vte_timeout_ms = 150
            }
        }

        while ($nextMilestoneIndex -lt $milestones.Count -and $cycleElapsed.TotalSeconds -ge $milestones[$nextMilestoneIndex]) {
            $milestone = $milestones[$nextMilestoneIndex]
            Write-ProbeEvent -Event 'context-milestone' -Data @{
                milestone_seconds = $milestone
                cycle_elapsed_ms = [long]$cycleElapsed.TotalMilliseconds
            }
            $nextMilestoneIndex++
        }

        if ($Mode -eq 'Timeout' -and $emittedSyncBytes -ge $SyncBufferRotationBytes) {
            # 达到预算前主动闭帧并开启新周期，避免 VTE 的 2 MiB 强制提交把
            # 长时间测试误判为“自行恢复”。这类轮换频率通常低于每小时一次。
            Recover-ProbeCycle -Reason 'sync-buffer-budget-rotation' -Elapsed $elapsed
            Start-Sleep -Milliseconds 300
            Start-ProbeCycle
            continue
        }

        $key = Read-ProbeKey
        if ($null -ne $key) {
            Recover-ProbeCycle -Reason ("key:{0}" -f $key.Key) -Elapsed $elapsed
            if ($key.Key -eq [ConsoleKey]::Q -or $key.Key -eq [ConsoleKey]::Escape) {
                break
            }

            Start-Sleep -Milliseconds 300
            Start-ProbeCycle
            continue
        }

        $size = Get-ConsoleSizeKey
        if ($null -ne $size -and $null -ne $lastSize -and $size -ne $lastSize) {
            Recover-ProbeCycle -Reason ("resize:{0}->{1}" -f $lastSize, $size) -Elapsed $elapsed
            Start-Sleep -Milliseconds 500
            Start-ProbeCycle
            continue
        }
        if ($null -ne $size) {
            $lastSize = $size
        }

        Start-Sleep -Milliseconds $FrameIntervalMs
    }
}
finally {
    Close-SyncFrame
    $elapsed = [DateTime]::UtcNow - $startedAt
    Write-Raw "`r`n"
    Write-Raw ("Probe stopped at tick={0}, elapsed={1:N1}s.`r`n" -f $tick, $elapsed.TotalSeconds)
    Write-ProbeEvent -Event 'stopped' -Data @{
        process_elapsed_ms = [long]$elapsed.TotalMilliseconds
        log_event_count = $logEventCount
    }
}
