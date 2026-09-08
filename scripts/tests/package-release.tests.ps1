[CmdletBinding()]
param(
    [string] $TargetDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$packageScript = Join-Path $repo 'scripts\package-release.ps1'
$output = Join-Path $repo ("target\package-script-test-$PID-" + [guid]::NewGuid().ToString('N'))
$expectedRoot = [System.IO.Path]::GetFullPath((Join-Path $repo 'target')).TrimEnd('\') + '\'
$resolvedOutput = [System.IO.Path]::GetFullPath($output)

if (-not $resolvedOutput.StartsWith($expectedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to use test output outside target: $resolvedOutput"
}

try {
    if (-not (Test-Path -LiteralPath $packageScript -PathType Leaf)) {
        throw "Packaging script is missing: $packageScript"
    }

    $cargoManifest = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw -Encoding UTF8
    $releaseProfile = [regex]::Match(
        $cargoManifest,
        '(?ms)^\[profile\.release\]\s*(?<body>.*?)(?=^\[|\z)'
    )
    if (-not $releaseProfile.Success) {
        throw 'Cargo.toml is missing [profile.release]'
    }
    $releaseBody = $releaseProfile.Groups['body'].Value
    if ($releaseBody -notmatch '(?m)^debug\s*=\s*0\s*$') {
        throw 'Release builds must set debug = 0 so DWARF sections do not inflate pebrel.exe'
    }
    if ($releaseBody -notmatch '(?m)^strip\s*=\s*"symbols"\s*$') {
        throw 'Release builds must strip symbols before packaging (size budget: installer <30MB)'
    }

    $scriptBody = Get-Content -LiteralPath $packageScript -Raw -Encoding UTF8
    if ($scriptBody -notmatch '--features gpui-shell') {
        throw 'Release packaging must rebuild Pebrel with --features gpui-shell so the zip ships the GPUI shell.'
    }
    if ($scriptBody -notmatch '--bin pebrel') {
        throw 'Release packaging must build the Pebrel product executable.'
    }
    if ($scriptBody -match 'cargo build --workspace' -or $scriptBody -notmatch '--bin pebrel-hook') {
        throw 'Release packaging must build only the GPUI product and its hook, without another workspace/lab binary.'
    }

    if ($scriptBody -notmatch 'Assert-FreshBinaries') {
        throw 'Release packaging must refuse stale binaries (freshness guard missing).'
    }

    & $packageScript -Version 'unreleased' -SkipBuild -AllowStale -OutputDirectory $resolvedOutput -TargetDirectory $TargetDirectory
    $zipPath = Join-Path $resolvedOutput 'Pebrel-vunreleased-windows-x64.zip'
    if (-not (Test-Path -LiteralPath $zipPath -PathType Leaf)) {
        throw "Packaging script did not create $zipPath"
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        $actual = @($archive.Entries |
            Where-Object { -not $_.FullName.EndsWith('/') } |
            ForEach-Object { $_.FullName.Replace('\', '/') } |
            Sort-Object)
    } finally {
        $archive.Dispose()
    }

    $expected = @(
        'README.md'
        'README.zh-CN.md'
        'docs/CHANGELOG.md'
        'docs/INSTALL.md'
        'docs/lua-configuration.md'
        'docs/runtime-api-v1.schema.json'
        'docs/runtime-control-api.md'
        'licenses/LICENSE'
        'licenses/LICENSE-LUA'
        'licenses/LICENSE-MLUA'
        'licenses/THIRD-PARTY-NOTICES'
        'pebrel.exe'
        'runtime/OpenConsole.exe'
        'runtime/conpty.dll'
        'runtime/pebrel-hook.exe'
        'skills/pebrel-runtime/SKILL.md'
        'skills/pebrel-runtime/agents/openai.yaml'
    ) | Sort-Object

    $difference = @(Compare-Object -ReferenceObject $expected -DifferenceObject $actual)
    if ($difference.Count -ne 0) {
        throw "ZIP file manifest differs from the required layout:`n$($difference | Out-String)"
    }

    $rootFiles = @($actual | Where-Object { -not $_.Contains('/') })
    if (@(Compare-Object -ReferenceObject @('README.md', 'README.zh-CN.md', 'pebrel.exe') -DifferenceObject $rootFiles).Count -ne 0) {
        throw "ZIP root must contain the English and Chinese READMEs and pebrel.exe"
    }

    & $packageScript -Version 'unreleased' -PackageBrand NebulaTerminal -SkipBuild -AllowStale -OutputDirectory $resolvedOutput -TargetDirectory $TargetDirectory
    $legacyPath = Join-Path $resolvedOutput 'NebulaTerminal-vunreleased-windows-x64.zip'
    $legacyArchive = [System.IO.Compression.ZipFile]::OpenRead($legacyPath)
    try {
        $legacyFiles = @($legacyArchive.Entries |
            Where-Object { -not $_.FullName.EndsWith('/') } |
            ForEach-Object { $_.FullName.Replace('\', '/') } |
            Sort-Object)
        if (@(Compare-Object -ReferenceObject $expected -DifferenceObject $legacyFiles).Count -ne 0) {
            throw 'Legacy asset names must contain the same Pebrel runtime layout.'
        }
    } finally {
        $legacyArchive.Dispose()
    }

    Write-Output "package-release.tests.ps1: PASS ($($actual.Count) files, both package brands)"
} finally {
    if (Test-Path -LiteralPath $resolvedOutput) {
        Remove-Item -LiteralPath $resolvedOutput -Recurse -Force
    }
}
