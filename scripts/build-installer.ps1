[CmdletBinding()]
param(
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$')]
    [string] $Version,

    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',

    [switch] $SkipBuild,
    [switch] $Force,
    [switch] $ValidateOnly,
    [string] $OutputDirectory,
    [string] $InnoCompiler
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$manifestPath = Join-Path $repo 'nebula_app\Cargo.toml'
$installerScript = Join-Path $PSScriptRoot 'installer.iss'
$translationPath = Join-Path $repo 'target\installer-tools\ChineseSimplified.isl'
$translationUrl = 'https://raw.githubusercontent.com/jrsoftware/issrc/c495623a97376d524f298b1b160e8fd612375c62/Files/Languages/ChineseSimplified.isl'
$translationSha256 = '6753BE2C5E2740D859900FD902824DB2EC568DA5C5B52486524C9762D778B0B0'

if ([string]::IsNullOrWhiteSpace($Version)) {
    $cargoManifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8
    $match = [regex]::Match($cargoManifest, '(?m)^version\s*=\s*"(?<version>[^"]+)"\s*$')
    if (-not $match.Success) {
        throw "Unable to read the package version from $manifestPath"
    }
    $Version = $match.Groups['version'].Value
}

if ($Version -notmatch '^(?<major>[0-9]+)\.(?<minor>[0-9]+)\.(?<patch>[0-9]+)') {
    throw "Version must begin with three numeric components: $Version"
}
$numericVersion = "$($Matches.major).$($Matches.minor).$($Matches.patch).0"

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo 'dist'
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repo $OutputDirectory
}
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$targetRoot = Join-Path $repo "target\$Configuration"
$setupPath = Join-Path $outputRoot "NebulaTerminal-$Version-windows-x64-setup.exe"

$requiredFiles = @(
    (Join-Path $targetRoot 'nebula.exe'),
    (Join-Path $targetRoot 'nebula-hook.exe'),
    (Join-Path $targetRoot 'conpty.dll'),
    (Join-Path $targetRoot 'OpenConsole.exe'),
    (Join-Path $repo 'README.md'),
    (Join-Path $repo 'CHANGELOG.md'),
    (Join-Path $repo 'INSTALL.md'),
    (Join-Path $repo 'docs\lua-configuration.md'),
    (Join-Path $repo 'assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf'),
    (Join-Path $repo 'LICENSE'),
    (Join-Path $repo 'licenses\LICENSE-LUA'),
    (Join-Path $repo 'licenses\LICENSE-MLUA'),
    (Join-Path $repo 'THIRD-PARTY-NOTICES')
)

if (-not (Test-Path -LiteralPath $installerScript -PathType Leaf)) {
    throw "Installer definition is missing: $installerScript"
}

if (-not $SkipBuild) {
    Push-Location $repo
    try {
        $cargoArgs = @('build', '--workspace')
        if ($Configuration -eq 'release') {
            $cargoArgs += '--release'
        }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

$missing = @($requiredFiles | Where-Object { -not (Test-Path -LiteralPath $_ -PathType Leaf) })
if ($missing.Count -ne 0) {
    throw "Required installer files are missing:`n$($missing -join "`n")"
}

if ($ValidateOnly) {
    [PSCustomObject]@{
        InstallerScript = $installerScript
        Version = $Version
        Configuration = $Configuration
        Files = $requiredFiles.Count
    } | Format-List
    return
}

if (Test-Path -LiteralPath $setupPath) {
    if (-not $Force) {
        throw "Installer already exists: $setupPath (pass -Force to replace it)"
    }
    Remove-Item -LiteralPath $setupPath -Force
}
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
    $candidates = @(
        $env:ISCC_PATH,
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe')
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    $InnoCompiler = $candidates | Where-Object {
        Test-Path -LiteralPath $_ -PathType Leaf
    } | Select-Object -First 1
}
if ([string]::IsNullOrWhiteSpace($InnoCompiler) -or
    -not (Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) {
    throw 'ISCC.exe was not found. Install Inno Setup 6 or pass -InnoCompiler / set ISCC_PATH.'
}

# Inno 的基础安装不内置第三方翻译。固定提交与哈希可以保留中文向导，
# 同时避免发布构建悄悄接受上游后来被替换的内容。
$translationValid = $false
if (Test-Path -LiteralPath $translationPath -PathType Leaf) {
    $actualTranslationHash =
        (Get-FileHash -LiteralPath $translationPath -Algorithm SHA256).Hash
    $translationValid = $actualTranslationHash -eq $translationSha256
}
if (-not $translationValid) {
    $translationDirectory = Split-Path -Parent $translationPath
    New-Item -ItemType Directory -Path $translationDirectory -Force | Out-Null
    $temporaryTranslation = "$translationPath.$PID.tmp"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $translationUrl -OutFile $temporaryTranslation
        $actualHash = (Get-FileHash -LiteralPath $temporaryTranslation -Algorithm SHA256).Hash
        if ($actualHash -ne $translationSha256) {
            throw "Chinese translation hash mismatch: expected $translationSha256, got $actualHash"
        }
        Move-Item -LiteralPath $temporaryTranslation -Destination $translationPath -Force
    } finally {
        if (Test-Path -LiteralPath $temporaryTranslation) {
            Remove-Item -LiteralPath $temporaryTranslation -Force
        }
    }
}

Push-Location $PSScriptRoot
try {
    & $InnoCompiler "/DAppVersion=$Version" "/DNumericVersion=$numericVersion" "/DConfiguration=$Configuration" "/O$outputRoot" $installerScript
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compilation failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $setupPath -PathType Leaf)) {
    throw "Inno Setup did not create the expected installer: $setupPath"
}

$setup = Get-Item -LiteralPath $setupPath
[PSCustomObject]@{
    Path = $setup.FullName
    Size = $setup.Length
    SHA256 = (Get-FileHash -LiteralPath $setupPath -Algorithm SHA256).Hash
} | Format-List
