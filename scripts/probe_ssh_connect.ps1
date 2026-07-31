# Isolated probe instance for the SSH connect card.
#
# One APPDATA isolates every mutable file, mux.port included -- without that
# the launch just pings the existing instance and exits, so no new window.
# USERPROFILE / CLAUDE_CONFIG_DIR / CODEX_HOME / XDG_CONFIG_HOME are isolated
# too, so the AI hook self-heal cannot rewrite the real user config.
#
# NOTE: comments must stay pure ASCII. Windows PowerShell 5.1 reads a BOM-less
# UTF-8 script as ANSI (GBK here), which corrupts non-ASCII bytes and breaks
# the parser -- same trap as .cmd files.
param([switch]$Kill)

$root = 'D:\temp_build\.probe-ssh'

if ($Kill) {
    Get-Process nebula -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like '*\target\debug\nebula.exe' } |
        Stop-Process -Force
    return
}

Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$root\appdata\Nebula", "$root\home" | Out-Null

# saved_hosts is what actually populates the sidebar list; pinned_hosts only
# reorders it (see merge_ssh_hosts). Writing as ASCII on purpose: PS 5.1's
# -Encoding UTF8 emits a BOM, and the BOM would end up glued to the first key
# so the settings parser would never match it.
#
# 192.0.2.1 is RFC 5737 TEST-NET-1 and unroutable: the connection sits in the
# Connect stage for a long time, which is exactly what we need to watch the
# particles. 127.0.0.1 has no sshd here, so it fails fast -- that covers the
# failure state.
$settings = @(
    'saved_hosts=root@192.0.2.1,root@127.0.0.1',
    'pinned_hosts=root@192.0.2.1',
    'keep_session=false'
) -join [Environment]::NewLine
Set-Content -Path "$root\appdata\Nebula\nebula_settings.txt" -Value $settings -Encoding ascii

$env:APPDATA = "$root\appdata"
$env:USERPROFILE = "$root\home"
$env:CLAUDE_CONFIG_DIR = "$root\home\.claude"
$env:CODEX_HOME = "$root\home\.codex"
$env:XDG_CONFIG_HOME = "$root\home\.config"
$env:NEBULA_DEBUG_LOG = '1'

$exe = 'D:\temp_build\nebula\target\debug\nebula.exe'
$proc = Start-Process -FilePath $exe -PassThru
Write-Output ("pid=" + $proc.Id)
