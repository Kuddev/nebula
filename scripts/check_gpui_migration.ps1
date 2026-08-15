[CmdletBinding()]
param(
    [string]$TargetDir = ".target-gpui-check",
    [switch]$Locked,
    [switch]$Offline,
    [switch]$SkipTests,
    [ValidateRange(1, 64)]
    [int]$Jobs = 2
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$resolvedTarget = if ([System.IO.Path]::IsPathRooted($TargetDir)) {
    $TargetDir
} else {
    Join-Path $repoRoot $TargetDir
}
$previousTarget = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR", "Process")

function Invoke-CargoStep {
    param(
        [Parameter(Mandatory)]
        [string]$Label,
        [Parameter(Mandatory)]
        [string[]]$Arguments
    )

    Write-Host "==> $Label"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE"
    }
}

Get-Command cargo -ErrorAction Stop | Out-Null
Get-Command git -ErrorAction Stop | Out-Null

Push-Location $repoRoot
try {
    [Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", $resolvedTarget, "Process")
    $common = @("-p", "nebula", "--features", "gpui-shell", "--jobs", "$Jobs")
    if ($Locked) {
        $common += "--locked"
    }
    if ($Offline) {
        $common += "--offline"
    }

    Invoke-CargoStep "GPUI feature check" (@("check") + $common)

    if (-not $SkipTests) {
        Invoke-CargoStep "Config source tests" (@("test") + $common + "config::source::tests")
        Invoke-CargoStep "Font catalog tests" (@("test") + $common + "font_install::tests")
        Invoke-CargoStep "GPUI workspace tests" (@("test") + $common + "gpui_shell::workspace::tests")
        Invoke-CargoStep "Provider tests" (@("test") + $common + "ai_providers")
    }

    Write-Host "==> Diff whitespace check"
    & git diff --check
    if ($LASTEXITCODE -ne 0) {
        throw "git diff --check failed with exit code $LASTEXITCODE"
    }

    Write-Host "GPUI migration checks passed."
} finally {
    Pop-Location
    [Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", $previousTarget, "Process")
}
