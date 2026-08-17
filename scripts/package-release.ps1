[CmdletBinding()]
param(
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z.-]*$')]
    [string] $Version = 'unreleased',

    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',

    [switch] $SkipBuild,
    [switch] $Force,
    [string] $OutputDirectory,

    # Cargo target directory to build into and package from. Defaults to the
    # repo's own `target/`.
    #
    # Windows refuses to overwrite a running exe, so packaging while a Nebula
    # instance is live out of `target/release/nebula.exe` dies with a bare
    # "拒绝访问 (os error 5)" that names the linker, not the real cause. Point
    # this at a separate directory to build a package without touching — let
    # alone killing — the instance the user is working in.
    [string] $TargetDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo 'dist'
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo $OutputDirectory
}
if ([string]::IsNullOrWhiteSpace($TargetDirectory)) {
    $TargetDirectory = Join-Path $repo 'target'
} elseif (-not [System.IO.Path]::IsPathRooted($TargetDirectory)) {
    $TargetDirectory = Join-Path $repo $TargetDirectory
}
$cargoTargetRoot = [System.IO.Path]::GetFullPath($TargetDirectory)
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$targetRoot = Join-Path $cargoTargetRoot $Configuration
$stage = Join-Path $outputRoot ".stage-$Version-$PID"
$zipPath = Join-Path $outputRoot "NebulaTerminal-v$Version-windows-x64.zip"
$temporaryZip = Join-Path $outputRoot ".NebulaTerminal-v$Version-windows-x64-$PID.tmp.zip"

$manifest = [ordered]@{
    'nebula.exe'                                     = Join-Path $targetRoot 'nebula.exe'
    'README.md'                                      = Join-Path $repo 'README.md'
    'runtime/nebula-hook.exe'                        = Join-Path $targetRoot 'nebula-hook.exe'
    'runtime/conpty.dll'                             = Join-Path $targetRoot 'conpty.dll'
    'runtime/OpenConsole.exe'                        = Join-Path $targetRoot 'OpenConsole.exe'
    'fonts/MapleMonoNormal-NF-CN-Regular.ttf'        = Join-Path $repo 'assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf'
    'docs/CHANGELOG.md'                              = Join-Path $repo 'CHANGELOG.md'
    'docs/INSTALL.md'                                = Join-Path $repo 'INSTALL.md'
    'docs/lua-configuration.md'                      = Join-Path $repo 'docs\lua-configuration.md'
    'licenses/LICENSE'                               = Join-Path $repo 'LICENSE'
    'licenses/LICENSE-LUA'                           = Join-Path $repo 'licenses\LICENSE-LUA'
    'licenses/LICENSE-MLUA'                          = Join-Path $repo 'licenses\LICENSE-MLUA'
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
    $previousTargetDirectory = $env:CARGO_TARGET_DIR
    try {
        $env:CARGO_TARGET_DIR = $cargoTargetRoot
        if ($Configuration -eq 'release') {
            & cargo build --workspace --release
            if ($LASTEXITCODE -ne 0) {
                throw "Cargo workspace build failed with exit code $LASTEXITCODE"
            }
            # 产品主窗是 GPUI（`nebula --gpui`），链接本地 gpui-component fork。
            # workspace 默认 feature 不含 gpui-shell，不补这一步装出来的是旧壳。
            & cargo build -p nebula --bin nebula --release --features gpui-shell
        } else {
            & cargo build --workspace
            if ($LASTEXITCODE -ne 0) {
                throw "Cargo workspace build failed with exit code $LASTEXITCODE"
            }
            & cargo build -p nebula --bin nebula --features gpui-shell
        }
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo gpui-shell build failed with exit code $LASTEXITCODE"
        }
    } finally {
        $env:CARGO_TARGET_DIR = $previousTargetDirectory
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
