$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$installerPath = Join-Path $repo 'scripts\installer.iss'
$builderPath = Join-Path $repo 'scripts\build-installer.ps1'

$installer = Get-Content -LiteralPath $installerPath -Raw -Encoding UTF8
$requiredPatterns = [ordered]@{
    'per-user installation' = 'DefaultDirName=\{localappdata\}\\Programs\\Nebula Terminal'
    'non-admin installation' = 'PrivilegesRequired=lowest'
    'Windows 10 1809 floor' = 'MinVersion=10\.0\.17763'
    'application closing' = 'CloseApplications=yes'
    'no application restart during uninstall' = 'RestartApplications=no'
    'desktop shortcut task' = 'Tasks: desktopicon'
    'login startup task' = '\{userstartup\}\\Nebula Terminal'
    'hook cleanup command' = 'Parameters: "setup-ai --remove"'
    'idempotent cleanup entry' = 'RunOnceId: "RemoveNebulaAiHooks"'
    'hook helper payload' = 'nebula-hook\.exe'
    'ConPTY payload' = 'conpty\.dll'
    'ConPTY host payload' = 'OpenConsole\.exe'
    'font payload' = 'MapleMonoNormal-NF-CN-Regular\.ttf'
    'optional font installation task' = 'Tasks: installfont'
    'pinned Chinese language file' = 'target\\installer-tools\\ChineseSimplified\.isl'
    'localized context menu label' = 'english\.OpenInNebula=Open in Nebula'
    'localized Chinese context menu label' = 'chinesesimplified\.OpenInNebula=\S.+'
    'directory background context menu' = 'Software\\Classes\\Directory\\Background\\shell\\NebulaTerminal'
    'selected directory context menu' = 'Software\\Classes\\Directory\\shell\\NebulaTerminal'
    'context menu executable icon' = 'ValueName: "Icon"; ValueData: "\{app\}\\nebula\.exe,0"'
    'background working-directory command' = '--working-directory ""%V""'
    'selected directory working-directory command' = '--working-directory ""%1""'
    'PATH task' = 'Name: "addtopath"; Description: "\{cm:AddToPath\}"'
    'PATH registry entry' = 'Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"'
    'PATH ownership marker' = 'ValueName: "InstallerAddedToPath"'
    'Win+R App Paths registration' = 'App Paths\\nebula\.exe'
    'environment change notification' = 'ChangesEnvironment=yes'
    'PATH uninstall cleanup' = 'CurUninstallStepChanged\(CurUninstallStep: TUninstallStep\)'
}

foreach ($entry in $requiredPatterns.GetEnumerator()) {
    if ($installer -notmatch $entry.Value) {
        throw "Installer is missing $($entry.Key): $($entry.Value)"
    }
}

$uninstallRun = $installer.IndexOf('[UninstallRun]', [System.StringComparison]::Ordinal)
$cleanup = $installer.IndexOf('setup-ai --remove', [System.StringComparison]::Ordinal)
if ($uninstallRun -lt 0 -or $cleanup -lt $uninstallRun) {
    throw 'Hook cleanup must be an [UninstallRun] action so it executes before installed files are deleted.'
}

$contextMenuRoots = @(
    'Software\Classes\Directory\Background\shell\NebulaTerminal'
    'Software\Classes\Directory\shell\NebulaTerminal'
)
foreach ($root in $contextMenuRoots) {
    $escapedRoot = [regex]::Escape($root)
    if ($installer -notmatch "Subkey: `"$escapedRoot`";.*Flags: uninsdeletekey") {
        throw "Context-menu key must be removed during uninstall: $root"
    }
}

& $builderPath -SkipBuild -ValidateOnly

$builder = Get-Content -LiteralPath $builderPath -Raw -Encoding UTF8
if ($builder -notmatch 'c495623a97376d524f298b1b160e8fd612375c62' -or
    $builder -notmatch '6753BE2C5E2740D859900FD902824DB2EC568DA5C5B52486524C9762D778B0B0') {
    throw 'The Chinese installer translation must use a pinned source commit and SHA-256.'
}

Write-Output "installer.tests.ps1: PASS ($($requiredPatterns.Count) invariants)"
