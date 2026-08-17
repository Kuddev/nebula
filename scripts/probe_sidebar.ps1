# Seed the isolated probe with a few NAMED ssh hosts, then (re)start it.
#
# The sidebar's two-line host row only shows its second line when the host has
# a label -- an unnamed host would repeat the same string twice, so it falls
# back to one line. Seeding real labels is the only way to see the two-line
# layout without clicking through the editor.
#
# NOTE: comments must stay pure ASCII (PS 5.1 reads a BOM-less UTF-8 script as
# ANSI/GBK and would corrupt them).
param([string]$Root = 'D:\temp_build\.probe-badge')

# Only ever kill the probe WE started last time: its pid is on file, and we
# re-check the command line before firing so a recycled pid never hits an
# innocent process. Never kill by name or path -- the user dogfoods the very
# same target\debug exe, so a name/path sweep takes their window down too.
$pidFile = Join-Path $Root 'probe.pid'
if (Test-Path $pidFile) {
    $oldPid = (Get-Content $pidFile | Select-Object -First 1) -as [int]
    if ($oldPid) {
        $old = Get-CimInstance Win32_Process -Filter "ProcessId=$oldPid" -ErrorAction SilentlyContinue
        if ($old -and $old.Name -eq 'nebula.exe' -and $old.CommandLine -like '*--working-directory*') {
            Stop-Process -Id $oldPid -Force -ErrorAction SilentlyContinue
        }
    }
    Remove-Item $pidFile -ErrorAction SilentlyContinue
}

$dir = Join-Path $Root 'appdata\Nebula'
New-Item -ItemType Directory -Force -Path $dir, (Join-Path $Root 'home') | Out-Null

$hosts = @(
    # ASCII on purpose: PS 5.1 has no unicode escapes, and this file must
    # stay BOM-less ASCII to survive its ANSI/GBK reader.
    @{ destination = 'root@192.168.200.150'; label = 'prod-db' }
    @{ destination = 'deploy@10.0.0.15:2201'; label = 'web-01' }
    @{ destination = 'root@172.16.8.4'; label = 'edge-cache' }
    @{ destination = 'ubuntu@10.2.4.7'; label = 'staging-api' }
)
$profiles = @{
    version  = 1
    profiles = @($hosts | ForEach-Object {
            @{ destination = $_.destination; auth = 'Auto'; private_keys = @(); label = $_.label }
        })
}
# UTF8 without BOM: the loader is strict JSON and a BOM breaks the first key.
[System.IO.File]::WriteAllText(
    (Join-Path $dir 'ssh_profiles.json'),
    ($profiles | ConvertTo-Json -Depth 5),
    (New-Object System.Text.UTF8Encoding $false))

$settings = @(
    'opacity=1.00'
    'keep_session=false'
    'blur=0'
    'saved_hosts=' + (($hosts | ForEach-Object { $_.destination }) -join ',')
) -join [Environment]::NewLine
Set-Content -Path (Join-Path $dir 'nebula_settings.txt') -Value $settings -Encoding ascii

$env:APPDATA = Join-Path $Root 'appdata'
$env:USERPROFILE = Join-Path $Root 'home'
$proc = Start-Process -FilePath 'D:\temp_build\nebula\target\debug\nebula.exe' `
    -ArgumentList '--working-directory', 'D:\temp_build\nebula' -PassThru
# Remember the pid so the next run can kill exactly this probe and nothing else.
Set-Content -Path $pidFile -Value $proc.Id
Start-Sleep -Seconds 9
"pid=$($proc.Id) responding=$($proc.Responding) hosts=$($hosts.Count)"
