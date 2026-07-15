$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$packageScript = Join-Path $repo 'scripts\package-release.ps1'
$output = Join-Path $repo 'target\package-script-test'
$expectedRoot = [System.IO.Path]::GetFullPath((Join-Path $repo 'target'))
$resolvedOutput = [System.IO.Path]::GetFullPath($output)

if (-not $resolvedOutput.StartsWith($expectedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to use test output outside target: $resolvedOutput"
}

if (Test-Path -LiteralPath $resolvedOutput) {
    Remove-Item -LiteralPath $resolvedOutput -Recurse -Force
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
        throw 'Release builds must set debug = 0 so DWARF sections do not inflate nebula.exe'
    }
    if ($releaseBody -notmatch '(?m)^strip\s*=\s*"debuginfo"\s*$') {
        throw 'Release builds must strip debuginfo before packaging'
    }

    & $packageScript -Version 'unreleased' -SkipBuild -OutputDirectory $resolvedOutput
    $zipPath = Join-Path $resolvedOutput 'NebulaTerminal-unreleased-windows-x64.zip'
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
        'docs/CHANGELOG.md'
        'docs/INSTALL.md'
        'fonts/MapleMonoNormal-NF-CN-Regular.ttf'
        'licenses/LICENSE'
        'licenses/THIRD-PARTY-NOTICES'
        'nebula.exe'
        'runtime/OpenConsole.exe'
        'runtime/conpty.dll'
        'runtime/nebula-hook.exe'
    ) | Sort-Object

    $difference = @(Compare-Object -ReferenceObject $expected -DifferenceObject $actual)
    if ($difference.Count -ne 0) {
        throw "ZIP file manifest differs from the required layout:`n$($difference | Out-String)"
    }

    $rootFiles = @($actual | Where-Object { -not $_.Contains('/') })
    if (@(Compare-Object -ReferenceObject @('README.md', 'nebula.exe') -DifferenceObject $rootFiles).Count -ne 0) {
        throw "ZIP root must contain only README.md and nebula.exe"
    }

    Write-Output "package-release.tests.ps1: PASS ($($actual.Count) files)"
} finally {
    if (Test-Path -LiteralPath $resolvedOutput) {
        Remove-Item -LiteralPath $resolvedOutput -Recurse -Force
    }
}
