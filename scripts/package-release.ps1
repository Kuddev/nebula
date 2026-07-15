[CmdletBinding()]
param(
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z.-]*$')]
    [string] $Version = 'unreleased',

    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',

    [switch] $SkipBuild,
    [switch] $Force,
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo 'dist'
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo $OutputDirectory
}
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$targetRoot = Join-Path $repo "target\$Configuration"
$stage = Join-Path $outputRoot ".stage-$Version-$PID"
$zipPath = Join-Path $outputRoot "NebulaTerminal-$Version-windows-x64.zip"
$temporaryZip = Join-Path $outputRoot ".NebulaTerminal-$Version-windows-x64-$PID.tmp.zip"

$manifest = [ordered]@{
    'nebula.exe'                                     = Join-Path $targetRoot 'nebula.exe'
    'README.md'                                      = Join-Path $repo 'README.md'
    'runtime/nebula-hook.exe'                        = Join-Path $targetRoot 'nebula-hook.exe'
    'runtime/conpty.dll'                             = Join-Path $targetRoot 'conpty.dll'
    'runtime/OpenConsole.exe'                        = Join-Path $targetRoot 'OpenConsole.exe'
    'fonts/MapleMonoNormal-NF-CN-Regular.ttf'        = Join-Path $repo 'assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf'
    'docs/CHANGELOG.md'                              = Join-Path $repo 'CHANGELOG.md'
    'docs/INSTALL.md'                                = Join-Path $repo 'INSTALL.md'
    'licenses/LICENSE'                               = Join-Path $repo 'LICENSE'
    'licenses/THIRD-PARTY-NOTICES'                   = Join-Path $repo 'THIRD-PARTY-NOTICES'
}

function Assert-Manifest([string] $Root, [string[]] $Expected) {
    $rootPrefix = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $actual = @(Get-ChildItem -LiteralPath $Root -Recurse -File | ForEach-Object {
        $fullPath = [System.IO.Path]::GetFullPath($_.FullName)
        if (-not $fullPath.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Package entry escaped the staging directory: $fullPath"
        }
        $fullPath.Substring($rootPrefix.Length).Replace('\', '/')
    } | Sort-Object)
    $difference = @(Compare-Object -ReferenceObject @($Expected | Sort-Object) -DifferenceObject $actual)
    if ($difference.Count -ne 0) {
        throw "Package manifest differs from the required layout:`n$($difference | Out-String)"
    }
}

function Remove-StageSafely([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    $prefix = $outputRoot.TrimEnd('\') + '\.stage-'
    if (-not $resolved.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove path outside the package staging boundary: $resolved"
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}

if (-not $SkipBuild) {
    Push-Location $repo
    try {
        if ($Configuration -eq 'release') {
            & cargo build --workspace --release
        } else {
            & cargo build --workspace
        }
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

$missing = @($manifest.GetEnumerator() | Where-Object {
    -not (Test-Path -LiteralPath $_.Value -PathType Leaf)
} | ForEach-Object { "$($_.Key) <- $($_.Value)" })
if ($missing.Count -ne 0) {
    throw "Required package files are missing:`n$($missing -join "`n")"
}

New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
if (Test-Path -LiteralPath $zipPath) {
    if (-not $Force) {
        throw "Package already exists: $zipPath (pass -Force to replace it)"
    }
    Remove-Item -LiteralPath $zipPath -Force
}
if (Test-Path -LiteralPath $temporaryZip) {
    Remove-Item -LiteralPath $temporaryZip -Force
}

try {
    Remove-StageSafely $stage
    New-Item -ItemType Directory -Path $stage | Out-Null
    foreach ($entry in $manifest.GetEnumerator()) {
        $destination = Join-Path $stage $entry.Key.Replace('/', '\')
        $parent = Split-Path -Parent $destination
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
        Copy-Item -LiteralPath $entry.Value -Destination $destination
    }

    Assert-Manifest $stage @($manifest.Keys)
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $temporaryZip -CompressionLevel Optimal

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($temporaryZip)
    try {
        $zipManifest = @($archive.Entries |
            Where-Object { -not $_.FullName.EndsWith('/') } |
            ForEach-Object { $_.FullName.Replace('\', '/') } |
            Sort-Object)
        $difference = @(
            Compare-Object -ReferenceObject @($manifest.Keys | Sort-Object) -DifferenceObject $zipManifest
        )
        if ($difference.Count -ne 0) {
            throw "ZIP manifest differs from the required layout:`n$($difference | Out-String)"
        }
        $unpackedSize = ($archive.Entries | Measure-Object -Property Length -Sum).Sum
    } finally {
        $archive.Dispose()
    }

    Move-Item -LiteralPath $temporaryZip -Destination $zipPath
    $zip = Get-Item -LiteralPath $zipPath
    $sha256 = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
    [PSCustomObject]@{
        Path         = $zip.FullName
        Files        = $manifest.Count
        Size         = $zip.Length
        UnpackedSize = $unpackedSize
        SHA256       = $sha256
    } | Format-List
} finally {
    Remove-StageSafely $stage
    if (Test-Path -LiteralPath $temporaryZip) {
        Remove-Item -LiteralPath $temporaryZip -Force
    }
}
