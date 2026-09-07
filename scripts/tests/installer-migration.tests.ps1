[CmdletBinding()]
param([string] $InnoCompiler)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [Console]::OutputEncoding

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
    $InnoCompiler = Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'
}
if (-not (Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) {
    throw "Inno Setup compiler not found: $InnoCompiler"
}
$fixture = Join-Path $PSScriptRoot 'installer-migration-fixture.iss'
$root = Join-Path $repo ('tmp\installer-migration-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$previousTemp = $env:TEMP
$previousTmp = $env:TMP
try {
    $env:TEMP = $root
    $env:TMP = $root
    $target = Join-Path $root 'link-target'
    New-Item -ItemType Directory -Path $target | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $target 'nebula.exe'), 'fixture data')
    New-Item -ItemType Junction -Path (Join-Path $root 'linked-install') -Target $target | Out-Null
    & $InnoCompiler '/Q' "/DFixtureRoot=$root" "/O$root" $fixture
    if ($LASTEXITCODE -ne 0) { throw 'Migration fixture compilation failed.' }
    $executable = Join-Path $root 'installer-migration-fixture.exe'
    $process = Start-Process -FilePath $executable -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', "/LOG=`"$(Join-Path $root 'setup.log')`""
    ) -Wait -PassThru
    # InitializeSetup returns false before any install, registry, shortcut or PATH actions.
    $reportPath = Join-Path $root 'result.txt'
    if (-not (Test-Path -LiteralPath $reportPath)) {
        throw "Migration fixture did not produce a report (exit $($process.ExitCode)); see $root"
    }
    $report = Get-Content -LiteralPath $reportPath -Raw -Encoding UTF8
    if (-not $report.StartsWith('PASS: ')) { throw "$report; see $root" }
    Write-Output "installer-migration.tests.ps1: $report"
    $payload = Join-Path $root 'syntax-payload'
    $syntaxOutput = Join-Path $root 'syntax-only-do-not-install'
    New-Item -ItemType Directory -Path $payload, $syntaxOutput | Out-Null
    foreach ($name in @('pebrel.exe', 'pebrel-hook.exe', 'conpty.dll', 'OpenConsole.exe')) {
        [System.IO.File]::WriteAllText((Join-Path $payload $name), 'Syntax fixture, not a runtime executable.')
    }
    & $InnoCompiler '/Q' '/DAppVersion=0.0.0' '/DNumericVersion=0.0.0.0' `
        '/DPackageBrand=PebrelInstallerSyntaxFixture' "/DBuildRoot=$payload" "/O$syntaxOutput" `
        (Join-Path $repo 'scripts\installer.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Full installer syntax compilation failed.' }
    Write-Output 'installer-migration.tests.ps1: full installer compilation PASS (not executed)'
    Write-Output "Fixture evidence: $root"
} finally {
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
}
