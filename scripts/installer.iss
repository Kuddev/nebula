#ifndef AppVersion
  #define AppVersion "1.1.0"
#endif

#ifndef NumericVersion
  #define NumericVersion "1.1.0.0"
#endif

#ifndef Configuration
  #define Configuration "release"
#endif

#define RepoRoot ".."
#ifndef BuildRoot
  #define BuildRoot RepoRoot + "\target\" + Configuration
#endif

[Setup]
AppId={{61022144-7D0A-4E54-94F2-C329A8F58656}
AppName=Nebula Terminal
AppVersion={#AppVersion}
AppVerName=Nebula Terminal {#AppVersion}
AppPublisher=Kuddev
AppPublisherURL=https://github.com/Kuddev/nebula
AppSupportURL=https://github.com/Kuddev/nebula/issues
AppUpdatesURL=https://github.com/Kuddev/nebula/releases
VersionInfoVersion={#NumericVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoCompany=Kuddev
VersionInfoDescription=Nebula Terminal Installer
VersionInfoProductName=Nebula Terminal
VersionInfoProductVersion={#NumericVersion}
VersionInfoProductTextVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Nebula Terminal
DefaultGroupName=Nebula Terminal
DisableProgramGroupPage=yes
DisableWelcomePage=no
DisableDirPage=no
DisableReadyPage=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
MinVersion=10.0.17763
LicenseFile={#RepoRoot}\LICENSE
SetupIconFile={#RepoRoot}\nebula_app\windows\nebula.ico
UninstallDisplayIcon={app}\nebula.exe
OutputDir={#RepoRoot}\dist
OutputBaseFilename=NebulaTerminal-{#AppVersion}-windows-x64-setup
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
english.AutoStart=Start Nebula Terminal when I sign in to Windows
english.InstallFont=Install Maple Mono font for the current user
english.AddToPath=Add Nebula Terminal to the user PATH
english.OpenInNebula=Open in Nebula
english.LaunchProgram=Launch Nebula Terminal
english.UninstallProgram=Uninstall Nebula Terminal
chinesesimplified.DesktopIcon=创建桌面快捷方式
chinesesimplified.AutoStart=登录 Windows 后启动 Nebula Terminal
chinesesimplified.InstallFont=为当前用户安装 Maple Mono 字体
chinesesimplified.AddToPath=将 Nebula Terminal 添加到当前用户 PATH
chinesesimplified.OpenInNebula=在 Nebula 中打开
chinesesimplified.LaunchProgram=启动 Nebula Terminal
chinesesimplified.UninstallProgram=卸载 Nebula Terminal

[Tasks]
Name: "installfont"; Description: "{cm:InstallFont}"
Name: "addtopath"; Description: "{cm:AddToPath}"
Name: "desktopicon"; Description: "{cm:DesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "{cm:AutoStart}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BuildRoot}\nebula.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildRoot}\nebula-hook.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#BuildRoot}\conpty.dll"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#BuildRoot}\OpenConsole.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#RepoRoot}\assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf"; DestDir: "{app}\fonts"; Flags: ignoreversion
Source: "{#RepoRoot}\assets\fonts\MapleMonoNormal-NF-CN-Regular.ttf"; DestDir: "{autofonts}"; FontInstall: "Maple Mono Normal NF CN"; Tasks: installfont; Flags: onlyifdoesntexist uninsneveruninstall
Source: "{#RepoRoot}\CHANGELOG.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\INSTALL.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\docs\lua-configuration.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\licenses\LICENSE-LUA"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\licenses\LICENSE-MLUA"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}\THIRD-PARTY-NOTICES"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\Nebula Terminal"; Filename: "{app}\nebula.exe"; WorkingDir: "{%USERPROFILE}"
Name: "{group}\{cm:UninstallProgram}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Nebula Terminal"; Filename: "{app}\nebula.exe"; WorkingDir: "{%USERPROFILE}"; Tasks: desktopicon
Name: "{userstartup}\Nebula Terminal"; Filename: "{app}\nebula.exe"; WorkingDir: "{%USERPROFILE}"; Tasks: autostart

[Registry]
Root: HKCU; Subkey: "Software\Nebula Terminal"; ValueType: dword; ValueName: "InstallerAddedToPath"; ValueData: "1"; Tasks: addtopath; Check: NeedsAddToPath; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddToPath; Flags: preservestringtype
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\nebula.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\nebula.exe"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\nebula.exe"; ValueType: string; ValueName: "Path"; ValueData: "{app}"
; 目录背景使用 %V，选中的目录对象使用 %1；两者必须由 Explorer 展开后再交给 CLI。
; 每个动词使用独立的应用子键，卸载时只删除 Nebula 自己注册的菜单。
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\NebulaTerminal"; ValueType: string; ValueName: "MUIVerb"; ValueData: "{cm:OpenInNebula}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\NebulaTerminal"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\nebula.exe,0"
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\NebulaTerminal\command"; ValueType: string; ValueName: ""; ValueData: """{app}\nebula.exe"" --working-directory ""%V"""
Root: HKCU; Subkey: "Software\Classes\Directory\shell\NebulaTerminal"; ValueType: string; ValueName: "MUIVerb"; ValueData: "{cm:OpenInNebula}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\NebulaTerminal"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\nebula.exe,0"
Root: HKCU; Subkey: "Software\Classes\Directory\shell\NebulaTerminal\command"; ValueType: string; ValueName: ""; ValueData: """{app}\nebula.exe"" --working-directory ""%1"""

[Run]
Filename: "{app}\nebula.exe"; Description: "{cm:LaunchProgram}"; WorkingDir: "{%USERPROFILE}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 必须在 Inno 删除 nebula.exe 前调用应用自己的结构化清理逻辑，避免直接改写用户配置。
Filename: "{app}\nebula.exe"; Parameters: "setup-ai --remove"; WorkingDir: "{app}"; RunOnceId: "RemoveNebulaAiHooks"; Flags: runhidden skipifdoesntexist

[Code]
function NeedsAddToPath: Boolean;
var
  ExistingPath: string;
begin
  ExistingPath := '';
  Result := True;
  if RegQueryStringValue(HKCU, 'Environment', 'Path', ExistingPath) then
    Result := Pos(';' + ExpandConstant('{app}') + ';', ';' + ExistingPath + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ExistingPath: string;
  WrappedPath: string;
  Target: string;
begin
  if CurUninstallStep <> usUninstall then
    Exit;

  if not RegValueExists(HKCU, 'Software\Nebula Terminal', 'InstallerAddedToPath') then
    Exit;

  ExistingPath := '';
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', ExistingPath) then
    Exit;

  WrappedPath := ';' + ExistingPath + ';';
  Target := ';' + ExpandConstant('{app}') + ';';
  if StringChangeEx(WrappedPath, Target, ';', True) > 0 then begin
    ExistingPath := Copy(WrappedPath, 2, Length(WrappedPath) - 2);
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', ExistingPath);
  end;
end;
