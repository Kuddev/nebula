[CmdletBinding()]
param([Parameter(Mandatory = $true)][string] $ArchivePath)

$ErrorActionPreference = 'Stop'
$prepare = Join-Path $PSScriptRoot '../prepare-windows-runtime.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) "pebrel-runtime-test-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $destination = Join-Path $root 'runtime'
    & $prepare -Destination $destination -ArchivePath $ArchivePath
    $before = @(Get-ChildItem $destination | Sort-Object Name | ForEach-Object { $_.LastWriteTimeUtc.Ticks })
    & $prepare -Destination $destination -ArchivePath $ArchivePath
    $after = @(Get-ChildItem $destination | Sort-Object Name | ForEach-Object { $_.LastWriteTimeUtc.Ticks })
    if (@(Compare-Object $before $after).Count -ne 0) { throw 'Verified runtime files were unnecessarily replaced.' }
    if (@(Get-ChildItem $destination).Count -ne 2) { throw 'Unexpected files were extracted.' }

    $invalid = Join-Path $root 'different.zip'
    [IO.File]::WriteAllBytes($invalid, [byte[]]@(80, 75, 0, 0))
    $rejected = $false
    try { & $prepare -Destination (Join-Path $root 'rejected') -ArchivePath $invalid }
    catch {
        if ($_.Exception.Message -notlike '*SHA256 verification*') { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw 'A different archive was accepted.' }
    if (Test-Path (Join-Path $root 'rejected')) { throw 'Files were created before archive verification.' }
    Write-Output 'windows-runtime.tests.ps1: PASS (pinned files, incremental reuse, hash mismatch)'
} finally { Remove-Item -LiteralPath $root -Recurse -Force }
