[CmdletBinding()]
param(
    # 每轮爆发的字节数。必须显著超过 event_loop 的 MAX_LOCKED_READ(65535)：
    # 只有在 `processed >= MAX_LOCKED_READ` 上提前 break 的那一轮，pty_read 才会
    # 留下「这次读到过数据(所以 piper 已经摘掉 read waker)、但又不再补读一次去
    # 发现管道空」的状态。读不到 64 KiB 的爆发会一路读到 Pending，走的是安全路径。
    [ValidateRange(65536, 8388608)]
    [int]$BurstBytes = 262144,

    # 爆发与尾部之间的间隙 —— 这是 2026-08-20 版 pty_drain_probe.ps1 缺掉的那半个
    # 条件，也是它复现率一直上不来的原因。这段时间要让 nebula 把爆发读完、在
    # MAX_LOCKED_READ 上 break、并且把管道读空；只有在那之后写入的字节才会撞上
    # 空 waker。爆发与尾部连续写入的话两者一起进管道，break 时 `!is_empty()` 成立，
    # 连修复前的旧判据都会补投成功，复现率是 0。
    [ValidateRange(50, 10000)]
    [int]$SettleMs = 400,

    # 写完尾部后的静默时长。源头静默是把间歇 bug 变成永久 bug 的开关：不再有新
    # 写入，就永远不会有下一次 wake 来替滞留的尾巴投递。这段时间同时是取证窗口。
    [ValidateRange(100, 60000)]
    [int]$SilenceMs = 900,

    [ValidateRange(1, 100000)]
    [int]$Rounds = 20,

    # 目标 pane。默认取环境契约里的 NEBULA_PANE_ID —— 也就是本脚本自己所在的
    # pane，我们测的正是自己这条 PTY。
    [string]$PaneId = $env:NEBULA_PANE_ID,

    # 多窗口时必须显式给出；单窗口(GPUI 默认 window_id=1)留空即可。
    [string]$WindowId = '',

    [string]$LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "nebula-pty-stall-$PID.jsonl")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$esc = [string][char]27
$maxLockedRead = 65535

$script:round = 0
$script:stalls = 0
$script:probeErrors = 0
$script:startedAt = [DateTime]::UtcNow
$script:stdout = [Console]::OpenStandardOutput()
$script:runId = [guid]::NewGuid().ToString('N').Substring(0, 6)
$script:cols = 100

# ConPTY 下 conhost 会按**控制台输出代码页**解码我们写进 stdout 的字节，再重新编码
# 送进 PTY。中文 Windows 默认 936(GBK)，于是直写的 UTF-8 字节被逐字节拆错 —— 实测
# 「滞留」显示成「婊炵暀」，判据行也就没法读了。切到 65001 只影响本进程的控制台，
# 进程退出即失效，不动用户的 chcp。
#
# 对爆发大小无影响：爆发体是纯 ASCII，两个代码页下字节数一致，MAX_LOCKED_READ
# 那道边界仍然精确。
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch { }

# 直写 stdout 字节：绕开 PowerShell 的格式化与换行改写，让 ANSI 序列和字节数
# 都精确可控（爆发大小必须可信，否则 MAX_LOCKED_READ 那道边界就测不准）。
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
    $record = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString('O')
        event = $EventName
        pid = $PID
        run_id = $script:runId
        round = $script:round
        stalls = $script:stalls
        elapsed_ms = [long]([DateTime]::UtcNow - $script:startedAt).TotalMilliseconds
    }
    foreach ($k in $Data.Keys) { $record[$k] = $Data[$k] }
    Add-Content -LiteralPath $LogPath -Encoding UTF8 -Value ($record | ConvertTo-Json -Compress -Depth 4)
}

# 解析 CLI 一次性响应。失败一律返回 $null，让调用方把它记成探针错误而不是
# 「复现」—— 把通信故障算进复现率会让修复验证得出假阳性。
function Invoke-RuntimeCall {
    param([Parameter(Mandatory = $true)][string[]]$CliArgs)
    try {
        $raw = & $script:cli @CliArgs 2>$null | Out-String
    }
    catch {
        return $null
    }
    if ([string]::IsNullOrWhiteSpace($raw)) { return $null }
    try { $doc = $raw | ConvertFrom-Json } catch { return $null }
    if (-not $doc.ok) { return $null }
    return $doc
}

# 判据：needle 到没到真实的 Term grid 里。
#
# `pane.read` 锁 Term 走 bounds_to_string，读的是网格与回滚缓冲，既不是截图也不是
# 渲染快照 —— 所以它能一刀切开「渲染没画出来」和「字节根本没进解析器」：滞留时
# grid 里本身就没有这段内容。
#
# 关键前提：子进程的 stdout 被 PowerShell 捕获进管道，不写回 PTY，因此这次取证
# 不会让 needs_write 翻转、不会触发 reregister 补投，也就不会把现场冲掉。换成
# Write-Host 打印检查结果就会自己把 bug 治好，从而永远测不出来。
function Test-GridHasNeedle {
    param([Parameter(Mandatory = $true)][string]$Needle)

    $cliArgs = @('ctl', 'read', '--pane', $script:paneId, '--lines', '40')
    if ($script:windowArg) { $cliArgs += $script:windowArg }
    $doc = Invoke-RuntimeCall -CliArgs $cliArgs
    if ($null -eq $doc) { return $null }
    return [bool]($doc.result.text.Contains($Needle))
}

# 爆发体：纯文本长行，不用 synchronized update、不用 alt screen。复现与
# CSI ?2026 无关，撇清它才能让命中直接指向管道层。
function New-Burst {
    param([Parameter(Mandatory = $true)][int]$TargetBytes)

    $line = 'x' * ($script:cols - 12)
    $sb = [System.Text.StringBuilder]::new($TargetBytes + 4096)
    $n = 0
    while ($sb.Length -lt $TargetBytes) {
        $n++
        [void]$sb.Append('B').Append('{0:D6}' -f $n).Append(' ').Append($line).Append("`r`n")
    }
    return $sb.ToString()
}

# 尾部：几百字节，形态照 AI CLI 的收尾抄 —— 分隔线 + 输入框 + 状态栏。
# 真实滞留的就是这一批：一屏内容把 pty_read 顶过 64 KiB 之后紧跟着打印的
# 那点 UI。所以「屏幕上缺输入框」才是这个 bug 最可靠的肉眼判据。
function New-Tail {
    param([Parameter(Mandatory = $true)][string]$Needle)
    $rule = '─' * ($script:cols - 4)
    return "`r`n$rule`r`n❯ $Needle`r`n$rule`r`n  probe status bar · round $($script:round)`r`n"
}

try {
    if ([string]::IsNullOrWhiteSpace($PaneId)) {
        throw '拿不到 NEBULA_PANE_ID：请在 Nebula 的本地 pane 里运行本脚本，或显式传 -PaneId。'
    }
    if ($env:NEBULA_PANE_REMOTE -eq '1') {
        throw '这是 SSH pane：远端主机上没有 nebula CLI，控制面判据不可用。请在本地 pane 里跑。'
    }

    # 环境契约：NEBULA_CLI 是提供控制面的那个 exe 的绝对路径（便携版不一定在
    # PATH 上）；NEBULA_BIN_DIR 已被前置到 PATH，所以裸 nebula 也能用作回落。
    $script:cli = if ($env:NEBULA_CLI) { $env:NEBULA_CLI } else { 'nebula' }
    $script:paneId = $PaneId
    $script:windowArg = if ($WindowId) { @('--window', $WindowId) } else { @() }

    try { $script:cols = [Math]::Max(40, [Console]::WindowWidth) } catch { }

    Set-Content -LiteralPath $LogPath -Encoding UTF8 -Value ''
    Clear-Content -LiteralPath $LogPath

    # 预检：先确认控制面真的能答话，否则 20 轮跑完才发现每一轮都是探针错误。
    $describe = Invoke-RuntimeCall -CliArgs @('ctl', 'describe')
    if ($null -eq $describe) {
        throw "控制面无响应（cli=$script:cli）。确认这个 exe 属于正在运行的那个 Nebula 实例。"
    }
    $appVersion = $describe.result.app_version
    if ($null -eq (Test-GridHasNeedle -Needle "nebula-stall-probe-preflight-$script:runId")) {
        throw "pane.read 调用失败：pane_id=$script:paneId 可能不对，多窗口时请补 -WindowId。"
    }

    $burst = New-Burst -TargetBytes $BurstBytes
    $burstBytes = [System.Text.Encoding]::UTF8.GetByteCount($burst)

    Write-Text "$esc[2J$esc[H"
    Write-Text "Nebula PTY 滞留复现探针  pid=$PID  run=$script:runId`r`n"
    Write-Text "  被测实例 : $appVersion   pane=$script:paneId`r`n"
    Write-Text "  爆发     : $burstBytes B  (MAX_LOCKED_READ=$maxLockedRead)`r`n"
    Write-Text "  时序     : 爆发 -> settle ${SettleMs}ms -> 尾部 -> 静默 ${SilenceMs}ms -> 取证`r`n"
    Write-Text "  判据     : 尾部 needle 有没有进 Term grid（pane.read，零 PTY 写入）`r`n"
    Write-Text "  日志     : $LogPath`r`n`r`n"

    Write-ProbeEvent -EventName 'probe-start' -Data @{
        app_version = $appVersion
        pane_id = $script:paneId
        burst_bytes = $burstBytes
        max_locked_read = $maxLockedRead
        settle_ms = $SettleMs
        silence_ms = $SilenceMs
        rounds = $Rounds
        verdict_rule = 'tail needle absent from Term grid => bytes stranded in the piper pipe'
        powershell = $PSVersionTable.PSVersion.ToString()
    }

    while ($script:round -lt $Rounds) {
        $script:round++
        $needle = "TAIL-$script:runId-$('{0:D3}' -f $script:round)"

        # 1) 爆发：把 pty_read 顶过 MAX_LOCKED_READ，让它在 break 时已经摘掉 waker。
        Write-Text $burst

        # 2) 间隙：等 nebula 读完并把管道读空。缺这一步就复现不了（见 -SettleMs）。
        Start-Sleep -Milliseconds $SettleMs

        # 3) 尾部：这批字节的 wake() 会打在空 waker 上。
        Write-Text (New-Tail -Needle $needle)

        # 4) 静默：不再产生任何输出，滞留由此变成永久状态。
        Start-Sleep -Milliseconds $SilenceMs

        # 5) 取证：只读不写，现场保持原样。
        $arrived = Test-GridHasNeedle -Needle $needle

        if ($null -eq $arrived) {
            $script:probeErrors++
            Write-ProbeEvent -EventName 'probe-error' -Data @{ needle = $needle }
        }
        elseif (-not $arrived) {
            $script:stalls++
            Write-ProbeEvent -EventName 'stall-detected' -Data @{
                needle = $needle
                verdict = 'tail never reached the grid: stranded behind a taken waker'
            }
        }

        # 6) 打印本轮结论。这一步会产生 PTY 写入，从而顺带把可能滞留的尾部冲出来
        #    —— 正好给下一轮清场。顺序很重要：必须在取证之后。
        $mark = if ($null -eq $arrived) { '?? 探针错误' } elseif ($arrived) { 'ok 已到达' } else { '!! 滞留' }
        $rate = [Math]::Round(100.0 * $script:stalls / $script:round, 1)
        Write-Text "[round $('{0:D3}' -f $script:round)/$Rounds] $mark   累计滞留 $script:stalls  复现率 $rate%`r`n"
    }
}
catch {
    Write-Text "`r`n$esc[0m探针无法运行: $($_.Exception.Message)`r`n"
    exit 2
}
finally {
    if ($script:round -gt 0) {
        $elapsed = [DateTime]::UtcNow - $script:startedAt
        $rate = [Math]::Round(100.0 * $script:stalls / $script:round, 1)
        Write-Text "`r`n$esc[0m================ 结果 ================`r`n"
        Write-Text "轮数 $script:round   滞留 $script:stalls   复现率 $rate%   探针错误 $script:probeErrors   耗时 $([Math]::Round($elapsed.TotalSeconds, 1))s`r`n"
        if ($script:stalls -gt 0) {
            Write-Text "判定: 有 bug —— 尾部字节被扣在 piper 管道里。修复未生效，或这个构建不含修复。`r`n"
        }
        elseif ($script:probeErrors -ge $script:round) {
            Write-Text "判定: 无效 —— 每一轮都是探针错误，没测到任何东西。先修 pane_id / 控制面连通性。`r`n"
        }
        else {
            Write-Text "判定: 干净 —— 所有尾部都按时进了 grid。`r`n"
        }
        Write-ProbeEvent -EventName 'probe-stop' -Data @{
            stall_rate_percent = $rate
            probe_errors = $script:probeErrors
        }
    }
}
