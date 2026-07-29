# Launch a debug nebula instance for ui_probe and print its PID.
#
# It runs a COPY, never target/debug/nebula.exe itself. Two reasons, both hit:
#
#   1. Windows will not let a running exe be overwritten. Probing the build
#      output directly makes the next `cargo build` die with
#      "linking with x86_64-w64-mingw32-gcc failed", while the real cause
#      (file in use) is buried under a wall of mingw .drectve warnings.
#
#   2. The probe instance must be disposable, and the user's own Nebula window
#      must never be collateral. Different exe paths remove any temptation to
#      clean up by image name. NEVER run `taskkill /IM nebula.exe` here: the
#      active Claude Code session may itself be hosted in a Nebula window.
#
# conpty.dll and OpenConsole.exe are copied alongside: the Windows PTY backend
# looks for them next to the exe, and the instance cannot open a shell without
# them.

param(
    [string]$SourceDir  = "D:\temp_build\nebula\target\debug",
    [string]$ProbeDir   = "D:\temp_build\nebula\.probe",
    [string]$WorkingDir = "D:\temp_build",
    [int]$WarmupSeconds = 7
)

$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $ProbeDir | Out-Null

foreach ($name in @("nebula.exe", "conpty.dll", "OpenConsole.exe")) {
    $src = Join-Path $SourceDir $name
    if (-not (Test-Path $src)) {
        Write-Error "missing build artifact: $src (run cargo build -p nebula --bin nebula first)"
    }
    # A previous probe may still hold the same file. A failed copy means it is
    # alive; say so, and point at killing it BY PID rather than by image name.
    try {
        Copy-Item $src (Join-Path $ProbeDir $name) -Force
    } catch {
        Write-Error "cannot overwrite $name - a previous probe instance is probably still running. Close it with ui_probe.ps1 -ProcId <pid> -Kill."
    }
}

$exe = Join-Path $ProbeDir "nebula.exe"
$p = Start-Process $exe -ArgumentList '--working-directory', $WorkingDir -PassThru
Start-Sleep -Seconds $WarmupSeconds
$p.Refresh()
Write-Output ("pid=" + $p.Id)
Write-Output ("exe=" + $exe)
