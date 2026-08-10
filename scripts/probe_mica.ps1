# Isolated probe for the Windows 11 Mica backdrop.
#
# Everything mutable is redirected into one throwaway root: APPDATA carries
# both nebula_settings.txt (our own key/value store, owns `opacity`) and
# nebula.toml (the base config, owns `window.blur`). Isolating it
# keeps the real user settings untouched -- and, just as important, gives the
# probe its own mux.port so the launch does not just ping the resident
# instance and exit without ever opening a window.
#
# NOTE: comments must stay pure ASCII. Windows PowerShell 5.1 reads a BOM-less
# UTF-8 script as ANSI (GBK here), which corrupts non-ASCII bytes and breaks
# the parser.
param(
    [double]$Opacity = 0.75,
    [switch]$NoBlur,
    [string]$Build = 'D:\temp_build\nebula\dist\run-mica'
)

$root = 'D:\temp_build\.probe-mica'
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$root\appdata\Nebula", "$root\home" | Out-Null

# Written as ASCII on purpose: PS 5.1's -Encoding UTF8 emits a BOM, and the BOM
# would glue itself to the first key so the parser never matches it.
$settings = @(
    "opacity=$([math]::Round($Opacity, 2))",
    'keep_session=false'
) -join [Environment]::NewLine
Set-Content -Path "$root\appdata\Nebula\nebula_settings.txt" -Value $settings -Encoding ascii

$blur = if ($NoBlur) { 'false' } else { 'true' }
$toml = @(
    '[window]',
    "blur = $blur"
) -join [Environment]::NewLine
Set-Content -Path "$root\appdata\nebula\nebula.toml" -Value $toml -Encoding ascii

$env:APPDATA = "$root\appdata"
$env:USERPROFILE = "$root\home"
$env:XDG_CONFIG_HOME = "$root\appdata"

# --working-directory is what bypasses the single-instance mux forwarding.
$proc = Start-Process -FilePath "$Build\nebula.exe" `
    -ArgumentList '--working-directory', 'D:\temp_build\nebula' -PassThru
Start-Sleep -Seconds 7
"pid=$($proc.Id) exited=$($proc.HasExited) responding=$($proc.Responding) opacity=$Opacity blur=$blur"
