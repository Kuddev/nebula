# Drive the tab-badge symbol language without waiting for a real agent.
#
# The badges have three different sources and only one of them is easy to
# trigger by hand:
#
#   hand     <- AiHookKind::NeedsAttention, i.e. a `Notification` hook from
#               claude. Reproduced here by writing the hook envelope straight
#               into the per-instance named pipe.
#   triangle <- a command that exits non-zero AND ran longer than
#               notify::COMMAND_NOTIFY_MIN (10 s). Needs shell integration.
#   check    <- same, but exit 0. Only visible for ~1.1 s (display::BADGE_FLASH)
#               before it settles back into the unread dot.
#
# Badges only paint on NON-focused tabs -- focusing a tab is what marks its
# result as seen. So open a second tab first, then fire at the background one.
#
# NOTE: comments must stay pure ASCII. Windows PowerShell 5.1 reads a BOM-less
# UTF-8 script as ANSI (GBK here), which corrupts non-ASCII bytes.
param(
    [ValidateSet('hand', 'commands', 'list')]
    [string]$Kind = 'hand',
    [int]$Pid = 0,
    [long]$Pane = -1
)

function Get-NebulaPids {
    Get-Process nebula -ErrorAction SilentlyContinue | ForEach-Object { $_.Id }
}

if ($Kind -eq 'list') {
    Get-Process nebula -ErrorAction SilentlyContinue |
        Select-Object Id, Path, @{n = 'Pipe'; e = { "\\.\pipe\nebula-notify-$($_.Id)" } } |
        Format-Table -AutoSize
    return
}

if ($Kind -eq 'commands') {
    Write-Host 'Run these INSIDE a background Nebula tab (needs shell integration):'
    Write-Host ''
    Write-Host '  # red triangle -- non-zero exit after the 10 s notify floor'
    Write-Host '  Start-Sleep 11; cmd /c exit 3'
    Write-Host ''
    Write-Host '  # green check (~1.1 s) then the unread dot'
    Write-Host '  Start-Sleep 11'
    Write-Host ''
    Write-Host 'Switch to another tab BEFORE it finishes -- a focused tab'
    Write-Host 'consumes its own badge by definition.'
    return
}

# --- hand: write one hook envelope into the instance pipe -------------------
if ($Pid -eq 0) {
    $pids = @(Get-NebulaPids)
    if ($pids.Count -eq 0) { throw 'no nebula process found' }
    if ($pids.Count -gt 1) {
        throw "several nebula instances ($($pids -join ', ')); pass -Pid to pick one"
    }
    $Pid = $pids[0]
}

$pipe = "nebula-notify-$Pid"
$header = if ($Pane -ge 0) { "nebula-hook/1 source=claude pane=$Pane" } else { 'nebula-hook/1 source=claude' }
$payload = '{"hook_event_name":"Notification","message":"Claude needs your permission to run: rm -rf build/"}'

$client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipe, [System.IO.Pipes.PipeDirection]::Out)
try {
    $client.Connect(3000)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes("$header`n$payload")
    $client.Write($bytes, 0, $bytes.Length)
    $client.Flush()
    "sent NeedsAttention to pid=$Pid via \\.\pipe\$pipe"
    if ($Pane -lt 0) { '  (no pane id -> lands on the FOCUSED pane; switch tabs to see the badge)' }
}
finally {
    $client.Dispose()
}
