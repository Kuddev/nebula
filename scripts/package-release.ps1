[CmdletBinding()]
param(
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z.-]*$')]
    [string] $Version = 'unreleased',

    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',

    [switch] $SkipBuild,
    # 与 -SkipBuild 联用：跳过「exe 必须比源码新」的陈旧检查。仅用于脚本
    # 自测；发布产物一律走全新构建。
    [switch] $AllowStale,
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
    # 1.1.0 起 zip 不再附带 20MB 字体副本：nebula.exe 内嵌同一份字节，
    # 「安装字体」提示会把它落盘（font_install::ensure_bundled_font_on_disk）。
    # 安装包仍带 ttf——Inno 的 FontInstall 任务需要真实文件。
    'docs/CHANGELOG.md'                              = Join-Path $repo 'CHANGELOG.md'
    'docs/INSTALL.md'                                = Join-Path $repo 'INSTALL.md'
    'docs/lua-configuration.md'                      = Join-Path $repo 'docs\lua-configuration.md'
    'docs/runtime-control-api.md'                    = Join-Path $repo 'docs\runtime-control-api.md'
    'docs/runtime-api-v1.schema.json'                = Join-Path $repo 'docs\runtime-api-v1.schema.json'
    'skills/nebula-runtime/SKILL.md'                 = Join-Path $repo 'docs\skills\nebula-runtime\SKILL.md'
    'skills/nebula-runtime/agents/openai.yaml'       = Join-Path $repo 'docs\skills\nebula-runtime\agents\openai.yaml'
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

function Get-NewestSourceTime([string[]] $Roots) {
    $newest = [datetime]::MinValue
    foreach ($root in $Roots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        if (Test-Path -LiteralPath $root -PathType Leaf) {
            $times = @((Get-Item -LiteralPath $root).LastWriteTime)
        } else {
            $times = Get-ChildItem -LiteralPath $root -Recurse -File -ErrorAction SilentlyContinue |
                Where-Object { $_.Extension -in '.rs', '.toml' } |
                ForEach-Object { $_.LastWriteTime }
        }
        foreach ($time in @($times)) {
            if ($time -gt $newest) { $newest = $time }
        }
    }
    $newest
}

function Assert-FreshBinaries {
    # 判据：打包的二进制不得早于其源码的最新改动（2026-08-18 事故：修复
    # 已写进源码，包里却是前一晚的 exe）。cargo 对未变更目标不重链接，
    # 所以不能拿「本次运行开始时刻」当基准。conpty.dll / OpenConsole.exe
    # 是预编译供给件，不参与判定。
    if ($AllowStale) { return }
    $memberDirs = @(
        'nebula_app', 'nebula_terminal', 'nebula_config', 'nebula_config_derive',
        'nebula-completions', 'nebula_gpui', 'nebula_settings', 'nebula_split'
    ) | ForEach-Object { Join-Path $repo $_ }
    $appSources = $memberDirs + @(
        (Join-Path $repo 'Cargo.toml'),
        (Join-Path $repo '..\gpui-component-fork\crates')
    )
    $checks = @(
        @{ Binary = $manifest['nebula.exe']; Newest = Get-NewestSourceTime $appSources },
        @{ Binary = $manifest['runtime/nebula-hook.exe']; Newest = Get-NewestSourceTime @(
            (Join-Path $repo 'nebula_hook'), (Join-Path $repo 'Cargo.toml')) }
    )
    foreach ($check in $checks) {
        $item = Get-Item -LiteralPath $check.Binary
        if ($item.LastWriteTime -lt $check.Newest) {
            throw "Stale binary: $($check.Binary) ($($item.LastWriteTime)) is older than the newest source change ($($check.Newest)). Rebuild before packaging, or pass -AllowStale if you really mean it."
        }
    }
}

if (-not $SkipBuild) {
    Push-Location $repo
    $previousTargetDirectory = $env:CARGO_TARGET_DIR
    try {
        $env:CARGO_TARGET_DIR = $cargoTargetRoot
        # Never build nebula without gpui-shell first: a workspace default
        # binary overwrites the product exe with the legacy winit shell.
        # Exclude nebula from the workspace build, then link GPUI last.
        if ($Configuration -eq 'release') {
            & cargo build --workspace --release --exclude nebula
            if ($LASTEXITCODE -ne 0) {
                throw "Cargo workspace build failed with exit code $LASTEXITCODE"
            }
            & cargo build -p nebula --bin nebula --release --features gpui-shell
        } else {
            & cargo build --workspace --exclude nebula
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

$packagedExe = $manifest['nebula.exe']
Assert-FreshBinaries
$helpText = & $packagedExe --help 2>&1 | Out-String
if ($helpText -notmatch '--gpui') {
    throw "nebula.exe at $packagedExe is the legacy shell (no --gpui in --help). Rebuild with --features gpui-shell; do not package a workspace-default binary."
}
if ($Version -ne 'unreleased') {
    $versionText = & $packagedExe --version 2>&1 | Out-String
    if ($versionText -notmatch [regex]::Escape($Version)) {
        throw "nebula.exe reports `"$($versionText.Trim())`" but the package version is $Version. The staged exe does not match this release."
    }
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
