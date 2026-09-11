#ifndef AppVersion
  #define AppVersion "1.6.0"
#endif

#ifndef NumericVersion
  #define NumericVersion "1.6.0.0"
#endif

#ifndef Configuration
  #define Configuration "release"
#endif

#ifndef PackageBrand
  #define PackageBrand "Pebrel"
#endif

#define RepoRoot ".."
#ifndef BuildRoot
  #define BuildRoot RepoRoot + "\target\" + Configuration
#endif

[Setup]
AppId={{61022144-7D0A-4E54-94F2-C329A8F58656}
AppName=Pebrel
AppVersion={#AppVersion}
AppVerName=Pebrel {#AppVersion}
AppPublisher=Kuddev
AppPublisherURL=https://github.com/Kuddev/pebrel
AppSupportURL=https://github.com/Kuddev/pebrel/issues
AppUpdatesURL=https://github.com/Kuddev/pebrel/releases
VersionInfoVersion={#NumericVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoCompany=Kuddev
VersionInfoDescription=Pebrel Installer
VersionInfoProductName=Pebrel
VersionInfoProductVersion={#NumericVersion}
VersionInfoProductTextVersion={#AppVersion}
DefaultDirName={code:DefaultInstallDir}
UsePreviousAppDir=no
DefaultGroupName=Pebrel
UsePreviousGroup=no
DisableProgramGroupPage=yes
DisableWelcomePage=no
DisableDirPage=no
DisableReadyPage=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
MinVersion=10.0.17763
LicenseFile={#RepoRoot}\LICENSE
SetupIconFile={#RepoRoot}\nebula_app\windows\nebula.ico
UninstallDisplayIcon={app}\pebrel.exe
OutputDir={#RepoRoot}\dist
OutputBaseFilename={#PackageBrand}-v{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
RestartIfNeededByRun=no
SetupLogging=yes
ChangesEnvironment=yes
ShowLanguageDialog=auto

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "{#RepoRoot}\target\installer-tools\ChineseSimplified.isl"

[CustomMessages]
english.DesktopIcon=Create a desktop shortcut
english.AutoStart=Start Pebrel when I sign in to Windows
english.InstallFont=Install Maple Mono font for the current user
english.AddToPath=Add Pebrel to the user PATH
english.OpenInPebrel=Open in Pebrel
english.LaunchProgram=Launch Pebrel
english.UninstallProgram=Uninstall Pebrel
english.CloseLegacyProgram=Close the application at %1, then retry the installation.
english.MigrationFailed=Pebrel was installed, but the old installation could not be completely migrated: %1. Close the old application and run this installer again. Your other files and configuration were preserved.
english.MigrationPreflightFailed=Unable to check the previous installation: %1. Installation has not started.
english.RemovePathFailed=Unable to remove the Pebrel installation directory from PATH.
chinesesimplified.DesktopIcon=创建桌面快捷方式
chinesesimplified.AutoStart=登录 Windows 后启动 Pebrel
chinesesimplified.InstallFont=为当前用户安装 Maple Mono 字体
chinesesimplified.AddToPath=将 Pebrel 添加到当前用户 PATH
chinesesimplified.OpenInPebrel=在 Pebrel 中打开
chinesesimplified.LaunchProgram=启动 Pebrel
chinesesimplified.UninstallProgram=卸载 Pebrel
chinesesimplified.CloseLegacyProgram=请关闭 %1 中运行的程序，然后重试安装。
chinesesimplified.MigrationFailed=Pebrel 已安装，但旧安装未能完全迁移：%1。请关闭旧程序后重新运行此安装器。其他文件和配置已保留。
chinesesimplified.MigrationPreflightFailed=无法检查旧安装：%1。尚未开始安装。
chinesesimplified.RemovePathFailed=无法从 PATH 中移除 Pebrel 安装目录。

[Tasks]
Name: "installfont"; Description: "{cm:InstallFont}"
Name: "addtopath"; Description: "{cm:AddToPath}"
Name: "desktopicon"; Description: "{cm:DesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "{cm:AutoStart}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BuildRoot}\pebrel.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.zh-CN.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildRoot}\pebrel-hook.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#BuildRoot}\conpty.dll"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#BuildRoot}\OpenConsole.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#RepoRoot}\assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf"; DestDir: "{app}\fonts"; Flags: ignoreversion
Source: "{#RepoRoot}\assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf"; DestDir: "{autofonts}"; FontInstall: "Maple Mono Normal NF CN"; Tasks: installfont; Flags: onlyifdoesntexist uninsneveruninstall
Source: "{#RepoRoot}\CHANGELOG.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\INSTALL.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\lua-configuration.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\runtime-control-api.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\runtime-api-v1.schema.json"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\skills\pebrel-runtime\SKILL.md"; DestDir: "{app}\skills\pebrel-runtime"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\skills\pebrel-runtime\agents\openai.yaml"; DestDir: "{app}\skills\pebrel-runtime\agents"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\licenses\LICENSE-LUA"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\licenses\LICENSE-MLUA"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\THIRD-PARTY-NOTICES"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\Pebrel"; Filename: "{app}\pebrel.exe"; Parameters: "--gpui"; WorkingDir: "{%USERPROFILE}"; AppUserModelID: "com.pebrel.terminal"
Name: "{group}\{cm:UninstallProgram}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Pebrel"; Filename: "{app}\pebrel.exe"; Parameters: "--gpui"; WorkingDir: "{%USERPROFILE}"; AppUserModelID: "com.pebrel.terminal"; Tasks: desktopicon
Name: "{userstartup}\Pebrel"; Filename: "{app}\pebrel.exe"; Parameters: "--gpui"; WorkingDir: "{%USERPROFILE}"; AppUserModelID: "com.pebrel.terminal"; Tasks: autostart

[Registry]
Root: HKCU; Subkey: "Software\Pebrel"; ValueType: dword; ValueName: "InstallerAddedToPath"; ValueData: "1"; Tasks: addtopath; Check: NeedsAddToPath; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddToPath; Flags: preservestringtype
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\pebrel.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\pebrel.exe"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\pebrel.exe"; ValueType: string; ValueName: "Path"; ValueData: "{app}"
; 目录背景使用 %V，选中的目录对象使用 %1；两者必须由 Explorer 展开后再交给 CLI。
; 每个动词使用独立的应用子键，卸载时只删除 Pebrel 自己注册的菜单。
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Pebrel"; ValueType: string; ValueName: "MUIVerb"; ValueData: "{cm:OpenInPebrel}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Pebrel"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\pebrel.exe,0"
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Pebrel\command"; ValueType: string; ValueName: ""; ValueData: """{app}\pebrel.exe"" --gpui --working-directory ""%V"""
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pebrel"; ValueType: string; ValueName: "MUIVerb"; ValueData: "{cm:OpenInPebrel}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pebrel"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\pebrel.exe,0"
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Pebrel\command"; ValueType: string; ValueName: ""; ValueData: """{app}\pebrel.exe"" --gpui --working-directory ""%1"""

[Run]
Filename: "{app}\pebrel.exe"; Parameters: "--gpui"; Description: "{cm:LaunchProgram}"; WorkingDir: "{%USERPROFILE}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 必须在 Inno 删除 pebrel.exe 前调用应用自己的结构化清理逻辑，避免直接改写用户配置。
Filename: "{app}\pebrel.exe"; Parameters: "setup-ai --remove"; WorkingDir: "{app}"; RunOnceId: "RemovePebrelAiHooks"; Flags: runhidden skipifdoesntexist

[Code]
#include "installer-migration.iss"

function NeedsAddToPath: Boolean;
var
  ExistingPath: string;
begin
  ExistingPath := '';
  Result := True;
  if RegQueryStringValue(HKCU, 'Environment', 'Path', ExistingPath) then
    Result := not PathContainsDirectory(ExistingPath, ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ExistingPath: string;
begin
  if CurUninstallStep <> usUninstall then
    Exit;

  if not RegValueExists(HKCU, 'Software\Pebrel', 'InstallerAddedToPath') then
    Exit;

  ExistingPath := '';
  if RegQueryStringValue(HKCU, 'Environment', 'Path', ExistingPath) and
    not RegWriteExpandStringValue(HKCU, 'Environment', 'Path',
      RemovePathDirectory(ExistingPath, ExpandConstant('{app}'))) then
    RaiseException(CustomMessage('RemovePathFailed'));
  if not RegDeleteValue(HKCU, 'Software\Pebrel', 'InstallerAddedToPath') then
    RaiseException(CustomMessage('RemovePathFailed'));
  RegDeleteKeyIfEmpty(HKCU, 'Software\Pebrel');
end;
