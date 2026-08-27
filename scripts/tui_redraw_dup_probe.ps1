[CmdletBinding()]
param(
    # 重绘轮数。每轮 = 打印若干行新内容(产生滚动) + 原地重画一次 UI 块，
    # 也就是 AI CLI 刷新 spinner/todo 时做的事。
    [ValidateRange(2, 5000)]
    [int]$Rounds = 40,

    # 每轮新增的正文行数。每行带一个全局单调序号，跑完后数「有没有序号出现两次、
    # 有没有缺号」—— 这是本脚本的主判据，不依赖对重绘机制的任何猜测。
    # 想压滚动压力就把 -Rounds 调大（200 轮 × 12 行 = 2400 行仍装得进 scrollback）。
    [ValidateRange(1, 40)]
    [int]$BodyLines = 12,

    # 块重绘判据每几轮抽检一次。每次抽检是一趟 CLI 往返，轮数大时全查太慢；
    # 序号完整性是结尾一次性全量查，不受这个值影响。
    [ValidateRange(1, 1000)]
    [int]$CheckEvery = 5,

    # UI 块高度。判据依赖 `CSI {H}A` 精确回到块首，所以块内每行都会被裁到
    # 60 列以内 —— 一旦软换行，实际占用行数就不再等于 H，cursor up 会算错，
    # 那是探针自己的 bug 而不是产品的。
    [ValidateRange(3, 20)]
    [int]$SpinnerLines = 5,

    [ValidateRange(0, 5000)]
    [int]$IntervalMs = 60,

    # 对照开关一：每轮正文后插一发 >64 KiB 爆发 + settle，把「PTY 字节滞留」
    # 那条路径叠加进来。用法是跑两遍做对照：不开时干净、开了才出现重复，
    # 就说明重复是滞留解除时成批解析的下游产物；两种情况都重复，说明另有根因。
    [switch]$WithBurst,

    [ValidateRange(65536, 8388608)]
    [int]$BurstBytes = 262144,

    [ValidateRange(50, 10000)]
    [int]$SettleMs = 400,

    # 对照开关二：跑到中途暂停这么久，期间请手工拖窗口边框或拖出/收起分屏，
    # 把 resize 那条路径叠加进来。0 = 不暂停。
    #
    # 解读要留一分余地：resize 期间终端与应用对「一块 UI 占几行」的认知本就会
    # 短暂不一致，真实 CLI 靠 SIGWINCH 之后的整屏重绘自我纠正。所以这里测出的
    # 重复要结合 -WithBurst 的结果一起看，别单独下结论。
    [ValidateRange(0, 120000)]
    [int]$ResizePauseMs = 0,

    [string]$PaneId = $env:NEBULA_PANE_ID,
    [string]$WindowId = '',
    [string]$LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "nebula-tui-dup-$PID.jsonl")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$esc = [string][char]27

$script:round = 0
$script:startedAt = [DateTime]::UtcNow
$script:stdout = [Console]::OpenStandardOutput()
$script:runId = [guid]::NewGuid().ToString('N').Substring(0, 6)
$script:blockWidth = 56
$script:probeWindow = 120
$script:dupRounds = New-Object System.Collections.Generic.List[string]
$script:stallRounds = New-Object System.Collections.Generic.List[string]
$script:probeErrors = 0
$script:lastMark = 'probe status bar'
$script:seqEmitted = 0

# ConPTY 下 conhost 会按**控制台输出代码页**解码我们写进 stdout 的字节，再重新编码
# 送进 PTY。中文 Windows 默认 936(GBK)，于是直写的 UTF-8 字节被逐字节拆错 —— 实测
# 「滞留」显示成「婊炵暀」，判据行也就没法读了。切到 65001 只影响本进程的控制台，
# 进程退出即失效，不动用户的 chcp。
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch { }

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
        elapsed_ms = [long]([DateTime]::UtcNow - $script:startedAt).TotalMilliseconds
    }
    foreach ($k in $Data.Keys) { $record[$k] = $Data[$k] }
    Add-Content -LiteralPath $LogPath -Encoding UTF8 -Value ($record | ConvertTo-Json -Compress -Depth 4)
}

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

# 判据：视口附近活着几个**不同轮次**的 UI 块。
#
# 原地重绘的语义是「只留最后一版」：每轮开工前都 `CSI {H}A` + `CSI J` 把上一版
# 擦掉，所以任何时刻都只应当看到 1 个 SPIN 标记。数出 2 个以上，就说明有中间版本
# 没被覆盖掉、留在了历史里 —— 那正是屏幕上读起来「回到了一部分上文」的东西。
#
# 为什么按轮取证而不是跑完一次性数：`-WithBurst` 下每轮爆发会插进 1300 多个显示行，
# 把相邻两轮的 UI 块拉开那么远。一次性数就得读上万行，而 scrollback 本身有上限，
# 早期轮次直接被挤掉 —— 那样这个模式**永远**报「干净」，是假阴性。所以窗口按
# 爆发行数自动放大，并且每轮都看一次。
#
# 同脚本 pty_stall_repro.ps1：子进程 stdout 被捕获进管道、不写回 PTY，所以取证
# 本身不会触发 reregister 补投去改变现场。
function Measure-SurvivingBlocks {
    param([int]$Lines = 0)

    if ($Lines -le 0) { $Lines = $script:probeWindow }
    $cliArgs = @('ctl', 'read', '--pane', $script:paneId, '--lines', [string]$Lines)
    if ($script:windowArg) { $cliArgs += $script:windowArg }
    $doc = Invoke-RuntimeCall -CliArgs $cliArgs
    if ($null -eq $doc) { return $null }

    # $matches 是 PowerShell 的自动变量，不能拿来存自己的结果。
    $hits = [regex]::Matches($doc.result.text, "SPIN-$script:runId-(\d{4})")
    $rounds = @{}
    foreach ($m in $hits) { $rounds[$m.Groups[1].Value] = $true }
    return [pscustomobject]@{
        Distinct = $rounds.Count
        Total = $hits.Count
        Rounds = ($rounds.Keys | Sort-Object)
        HistoryAvailable = $doc.result.history_available
    }
}

# UI 块：形态照 AI CLI 的底部界面抄 —— 分隔线、spinner 行、输入框、状态栏。
# 每行都裁到 blockWidth 以内，保证「H 行」这个数字在任何窗口宽度下都成立。
function New-SpinnerBlock {
    $tag = "SPIN-$script:runId-$('{0:D4}' -f $script:round)"
    $rule = '─' * $script:blockWidth
    $frames = @('⠋', '⠙', '⠹', '⠸', '⠼', '⠴')
    $frame = $frames[$script:round % $frames.Length]

    $rows = New-Object System.Collections.Generic.List[string]
    $rows.Add($rule)
    $rows.Add("$frame $tag  working…")
    $rows.Add('❯ ')
    $rows.Add($rule)
    # 上一轮的判据结果借状态栏行显示 —— 块高度因此恒等于 SpinnerLines，
    # 下一轮的 `CSI {H}A` 永远落在块首。单独打印进度行会破坏这个不变量。
    while ($rows.Count -lt $SpinnerLines) { $rows.Add("  $script:lastMark") }
    while ($rows.Count -gt $SpinnerLines) { $rows.RemoveAt($rows.Count - 1) }

    return (($rows | ForEach-Object { if ($_.Length -gt $script:blockWidth) { $_.Substring(0, $script:blockWidth) } else { $_ } }) -join "`r`n") + "`r`n"
}

function New-BodyChunk {
    $sb = [System.Text.StringBuilder]::new()
    for ($i = 1; $i -le $BodyLines; $i++) {
        $script:seqEmitted++
        # 全局单调序号。纯 append 的字节流里，每个序号必须**不多不少各出现一次** ——
        # 这条判据不依赖对重绘机制的任何猜测，所以它能同时抓住「终端把同一段写了两遍」
        # 和「写丢了一段」。行长压在 40 字符以内，任何窗口宽度下都不软换行。
        [void]$sb.Append("SEQ-$script:runId-$('{0:D6}' -f $script:seqEmitted)  ")
        [void]$sb.Append('.' * 14).Append("`r`n")
    }
    return $sb.ToString()
}

# 序号完整性取证：读回全部 BODY 行，数重复号与缺号。
#
# 只在「预计总行数装得进 scrollback」时才有意义 —— 装不下的话老行本来就会被挤掉，
# 那会报出一整片假缺号，结论一文不值。所以装不下时直接跳过并说明，不假装测过。
function Test-SequenceIntegrity {
    param([Parameter(Mandatory = $true)][int]$Lines)

    $cliArgs = @('ctl', 'read', '--pane', $script:paneId, '--lines', [string]$Lines)
    if ($script:windowArg) { $cliArgs += $script:windowArg }
    $doc = Invoke-RuntimeCall -CliArgs $cliArgs
    if ($null -eq $doc) { return $null }

    $counts = @{}
    foreach ($m in [regex]::Matches($doc.result.text, "SEQ-$script:runId-(\d{6})")) {
        $k = [int]$m.Groups[1].Value
        if ($counts.ContainsKey($k)) { $counts[$k]++ } else { $counts[$k] = 1 }
    }
    if ($counts.Count -eq 0) {
        return [pscustomobject]@{ Seen = 0; MaxSeen = 0; Duplicated = @(); Missing = @(); Extra = 0 }
    }

    $maxSeen = ($counts.Keys | Measure-Object -Maximum).Maximum
    $dup = @($counts.Keys | Where-Object { $counts[$_] -gt 1 } | Sort-Object)
    # 缺号只在「已到达的最大序号」以内算 —— 尾部没到的那些是滞留，不是丢失。
    $missing = @(1..$maxSeen | Where-Object { -not $counts.ContainsKey($_) })
    $extra = 0
    foreach ($k in $dup) { $extra += ($counts[$k] - 1) }

    return [pscustomobject]@{
        Seen = $counts.Count
        MaxSeen = $maxSeen
        Duplicated = $dup
        Missing = $missing
        Extra = $extra
    }
}

function New-Burst {
    param([Parameter(Mandatory = $true)][int]$TargetBytes)
    $line = 'x' * 88
    $sb = [System.Text.StringBuilder]::new($TargetBytes + 4096)
    $n = 0
    while ($sb.Length -lt $TargetBytes) {
        $n++
        [void]$sb.Append('F').Append('{0:D6}' -f $n).Append(' ').Append($line).Append("`r`n")
    }
    return $sb.ToString()
}

try {
    if ([string]::IsNullOrWhiteSpace($PaneId)) {
        throw '拿不到 NEBULA_PANE_ID：请在 Nebula 的本地 pane 里运行本脚本，或显式传 -PaneId。'
    }
    if ($env:NEBULA_PANE_REMOTE -eq '1') {
        throw '这是 SSH pane：远端主机上没有 nebula CLI，控制面判据不可用。请在本地 pane 里跑。'
    }

    $script:cli = if ($env:NEBULA_CLI) { $env:NEBULA_CLI } else { 'nebula' }
    $script:paneId = $PaneId
    $script:windowArg = if ($WindowId) { @('--window', $WindowId) } else { @() }

    Set-Content -LiteralPath $LogPath -Encoding UTF8 -Value ''
    Clear-Content -LiteralPath $LogPath

    $describe = Invoke-RuntimeCall -CliArgs @('ctl', 'describe')
    if ($null -eq $describe) {
        throw "控制面无响应（cli=$script:cli）。确认这个 exe 属于正在运行的那个 Nebula 实例。"
    }
    $appVersion = $describe.result.app_version
    if ($null -eq (Measure-SurvivingBlocks)) {
        throw "pane.read 调用失败：pane_id=$script:paneId 可能不对，多窗口时请补 -WindowId。"
    }

    $burst = if ($WithBurst) { New-Burst -TargetBytes $BurstBytes } else { '' }
    $resizeRound = if ($ResizePauseMs -gt 0) { [int]($Rounds / 2) } else { -1 }

    # 取证窗口必须够跨过一整发爆发 —— 相邻两轮的 UI 块正好被它拉开这么远，窗口不够
    # 就看不见「上一轮没被擦掉」，判据静默失效。上限 4000 是为了别把单次 pane.read
    # 撑成几 MB 的 JSON；真撞到上限就明说判据变弱了，不假装还测得准。
    $burstLines = ([regex]::Matches($burst, "`n")).Count
    $script:probeWindow = [Math]::Min(4000, $burstLines + (($BodyLines + $SpinnerLines) * 3) + 60)
    $windowNote = if ($burstLines -gt 0 -and $script:probeWindow -ge 4000) {
        '  (已撞 4000 行上限：爆发太大，判据可能漏检早期轮次，请调小 -BurstBytes)'
    } else { '' }

    Write-Text "$esc[2J$esc[H"
    Write-Text "Nebula TUI 原地重绘重复探针  pid=$PID  run=$script:runId`r`n"
    Write-Text "  被测实例 : $appVersion   pane=$script:paneId`r`n"
    Write-Text "  轮数     : $Rounds  (正文 $BodyLines 行/轮, UI 块 $SpinnerLines 行)`r`n"
    Write-Text "  对照     : WithBurst=$($WithBurst.IsPresent)  ResizePause=${ResizePauseMs}ms`r`n"
    Write-Text "  判据     : 每轮取证，视口附近只应有 1 个 SPIN 标记；>1 即上一版没被擦掉`r`n"
    Write-Text "  取证窗口 : $script:probeWindow 行（爆发 $burstLines 行/轮）$windowNote`r`n"
    Write-Text "  日志     : $LogPath`r`n`r`n"

    Write-ProbeEvent -EventName 'probe-start' -Data @{
        app_version = $appVersion
        pane_id = $script:paneId
        rounds = $Rounds
        body_lines = $BodyLines
        spinner_lines = $SpinnerLines
        with_burst = [bool]$WithBurst
        burst_bytes = if ($WithBurst) { [System.Text.Encoding]::UTF8.GetByteCount($burst) } else { 0 }
        burst_lines = $burstLines
        probe_window_lines = $script:probeWindow
        resize_pause_ms = $ResizePauseMs
        verdict_rule = 'more than one distinct SPIN tag visible in the probe window => an in-place redraw was not overwritten'
    }

    $blockPrinted = $false
    while ($script:round -lt $Rounds) {
        $script:round++
        $tag = '{0:D4}' -f $script:round

        # 擦掉上一轮的 UI 块：回到块首，再擦到视口末。这是 CLI 刷新底部界面的
        # 标准做法，也是「只留最后一版」这个语义的来源。
        if ($blockPrinted) {
            Write-Text "$esc[${SpinnerLines}A$esc[J"
        }

        Write-Text (New-BodyChunk)

        if ($WithBurst) {
            Write-Text $burst
            Start-Sleep -Milliseconds $SettleMs
        }

        Write-Text (New-SpinnerBlock)
        $blockPrinted = $true

        # 给解析留点时间再取证。上一轮的判据结果通过 $script:lastMark 显示在**下一轮**
        # 的状态栏行里 —— 不能单独打印一行进度，那会让块变高，下一轮的 `CSI {H}A`
        # 就擦错位置，探针自己制造出重复来。
        if ($IntervalMs -gt 0) { Start-Sleep -Milliseconds $IntervalMs }

        # 块重绘判据抽检。序号完整性是结尾全量查的，不在这里。
        $snap = if (($script:round % $CheckEvery) -eq 0 -or $script:round -eq $Rounds) {
            Measure-SurvivingBlocks
        } else { 'skip' }

        if ($snap -is [string]) {
            # 本轮不抽检，沿用上一次的状态栏文字。
        }
        elseif ($null -eq $snap) {
            $script:probeErrors++
            $script:lastMark = "r$tag ?? 探针错误"
            Write-ProbeEvent -EventName 'probe-error'
        }
        elseif ($snap.Rounds -notcontains $tag) {
            # 本轮的块根本没进 grid —— 这是 PTY 字节滞留（pty_stall_repro.ps1 的目标），
            # 不是重绘问题：擦除判据在这一轮无从成立，所以单独归类，不算进复现。
            $script:stallRounds.Add($tag)
            $script:lastMark = "r$tag .. 本轮块未到达(滞留)"
            Write-ProbeEvent -EventName 'block-missing' -Data @{
                distinct = $snap.Distinct
                visible = ($snap.Rounds -join ',')
            }
        }
        elseif ($snap.Distinct -gt 1) {
            $script:dupRounds.Add($tag)
            $script:lastMark = "r$tag !! 重复 $($snap.Distinct) 版同时可见"
            Write-ProbeEvent -EventName 'duplicate-detected' -Data @{
                distinct = $snap.Distinct
                total_tags = $snap.Total
                visible = ($snap.Rounds -join ',')
            }
        }
        else {
            $script:lastMark = "r$tag ok  dup=$($script:dupRounds.Count) stall=$($script:stallRounds.Count)"
        }

        if ($script:round -eq $resizeRound) {
            Write-Text "`r`n>>> 现在请拖动窗口边框改变终端宽高（${ResizePauseMs}ms）...`r`n"
            Start-Sleep -Milliseconds $ResizePauseMs
            # resize 之后本地那份「UI 块占 H 行」的假设可能已经不成立，
            # 重新起一版，别让探针自己的 cursor up 算错行数而误报。
            $blockPrinted = $false
            Write-ProbeEvent -EventName 'resize-window-passed'
        }
    }

    Start-Sleep -Milliseconds 600
    $result = Measure-SurvivingBlocks

    $dupCount = $script:dupRounds.Count
    $stallCount = $script:stallRounds.Count

    # 序号完整性：预计总行数装不进 scrollback 就不做 —— 老行被挤掉会报出一整片
    # 假缺号，那种结论毫无价值。
    $expectedLines = ($Rounds * ($BodyLines + $burstLines)) + $SpinnerLines + 40
    $seq = $null
    $seqSkipReason = ''
    if ($expectedLines -le 9000) {
        $seq = Test-SequenceIntegrity -Lines ([Math]::Min(9000, $expectedLines + 200))
        if ($null -eq $seq) { $seqSkipReason = 'pane.read 取证失败' }
    }
    else {
        $seqSkipReason = "预计 $expectedLines 行 > 9000，装不进 scrollback（调小 -Rounds 或 -BurstBytes）"
    }

    Write-Text "`r`n$esc[0m================ 结果 ================`r`n"
    Write-Text "轮数 $script:round   正文行 $script:seqEmitted 行（每行一个唯一序号）`r`n"
    if ($null -ne $seq) {
        Write-Text "序号判据: 到达 $($seq.Seen) 个（最大 $($seq.MaxSeen)）  重复 $($seq.Duplicated.Count) 个/多出 $($seq.Extra) 份  缺号 $($seq.Missing.Count) 个`r`n"
    }
    else {
        Write-Text "序号判据: 跳过 —— $seqSkipReason`r`n"
    }
    Write-Text "块判据  : 重复轮次 $dupCount   本轮块未到达 $stallCount   探针错误 $script:probeErrors`r`n"
    if ($null -ne $result) {
        Write-Text "收尾时窗口内存活 SPIN 轮次 $($result.Distinct) 个   history=$($result.HistoryAvailable)`r`n"
    }
    Write-Text "`r`n"

    # 判定按「证据强度」排序：序号重复是铁证（同一段内容被写进了两个位置），
    # 块残留次之（可能是擦除算错行数），滞留单列（那是另一个 bug）。
    if ($null -ne $seq -and $seq.Duplicated.Count -gt 0) {
        $shown = ($seq.Duplicated | Select-Object -First 10) -join ', '
        Write-Text "判定: 复现（重复）—— $($seq.Duplicated.Count) 个序号出现了不止一次：$shown`r`n"
        Write-Text "      纯 append 的流里每个序号只该出现一次，出现两次即终端把同一段写进了两个位置。`r`n"
        Write-Text "      这就是屏幕上「回到了一部分上文」的直接形态，与重绘擦除无关。`r`n"
    }
    elseif ($dupCount -gt 0) {
        $shown = ($script:dupRounds | Select-Object -First 12) -join ', '
        Write-Text "判定: 复现（重绘残留）—— $dupCount 轮出现了没被擦掉的旧版 UI 块：$shown`r`n"
        Write-Text "      序号没有重复，所以不是写两遍，而是 cursor up / CSI J 的擦除范围不对。`r`n"
    }
    elseif ($null -ne $seq -and $seq.Missing.Count -gt 0) {
        $shown = ($seq.Missing | Select-Object -First 10) -join ', '
        Write-Text "判定: 复现（丢行）—— $($seq.Missing.Count) 个序号在已到达范围内缺失：$shown`r`n"
        Write-Text "      不是滞留（滞留只会让尾部没到，不会在中间挖洞）。`r`n"
    }
    elseif ($script:probeErrors -ge [Math]::Max(1, [int]($script:round / $CheckEvery))) {
        Write-Text "判定: 无效 —— 抽检全是探针错误，什么都没测到。先修 pane_id / 控制面连通性。`r`n"
    }
    elseif ($stallCount -gt 0) {
        Write-Text "判定: 未见重复，但有 $stallCount 次抽检发现 UI 块没能按时进 grid —— 那是 PTY 滞留，`r`n"
        Write-Text "      用 pty_stall_repro.ps1 单独量它。字节滞留解除后是保序解析的，`r`n"
        Write-Text "      所以本次结果同时说明「重复不是滞留的下游产物」。`r`n"
    }
    else {
        Write-Text "判定: 干净 —— 序号不多不少，UI 块每版都被正确覆盖。`r`n"
        if (-not $WithBurst) {
            Write-Text "      提示: 这是低压力基线。加 -WithBurst 叠加字节爆发，或把 -Rounds 调到 200+ 压滚动。`r`n"
        }
    }

    Write-ProbeEvent -EventName 'probe-stop' -Data @{
        body_lines_total = $script:seqEmitted
        seq_seen = if ($null -ne $seq) { $seq.Seen } else { $null }
        seq_max = if ($null -ne $seq) { $seq.MaxSeen } else { $null }
        seq_duplicated = if ($null -ne $seq) { ($seq.Duplicated -join ',') } else { $null }
        seq_extra_copies = if ($null -ne $seq) { $seq.Extra } else { $null }
        seq_missing = if ($null -ne $seq) { ($seq.Missing | Select-Object -First 200) -join ',' } else { $null }
        seq_skip_reason = $seqSkipReason
        duplicate_rounds = ($script:dupRounds -join ',')
        duplicate_count = $dupCount
        stall_rounds = ($script:stallRounds -join ',')
        stall_count = $stallCount
        probe_errors = $script:probeErrors
        tail_distinct_blocks = if ($null -ne $result) { $result.Distinct } else { $null }
        history_available = if ($null -ne $result) { $result.HistoryAvailable } else { $null }
        verdict = if ($null -ne $seq -and $seq.Duplicated.Count -gt 0) { 'duplicated-seq' }
                  elseif ($dupCount -gt 0) { 'duplicated-redraw' }
                  elseif ($null -ne $seq -and $seq.Missing.Count -gt 0) { 'lost-lines' }
                  elseif ($stallCount -gt 0) { 'stalled-not-duplicated' }
                  else { 'clean' }
    }
}
catch {
    Write-Text "`r`n$esc[0m探针无法运行: $($_.Exception.Message)`r`n"
    exit 2
}
