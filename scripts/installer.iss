#ifndef AppVersion
  #define AppVersion "0.6.0"
#endif

#ifndef NumericVersion
  #define NumericVersion "0.6.0.0"
#endif

#ifndef Configuration
  #define Configuration "release"
#endif

#define RepoRoot ".."
#define BuildRoot RepoRoot + "\target\" + Configuration

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
ShowLanguageDialog=auto

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "{#RepoRoot}\target\installer-tools\ChineseSimplified.isl"

[CustomMessages]
english.DesktopIcon=Create a desktop shortcut
english.AutoStart=Start Nebula Terminal when I sign in to Windows
english.InstallFont=Install Maple Mono font for the current user
english.LaunchProgram=Launch Nebula Terminal
english.UninstallProgram=Uninstall Nebula Terminal
chinesesimplified.DesktopIcon=创建桌面快捷方式
chinesesimplified.AutoStart=登录 Windows 后启动 Nebula Terminal
chinesesimplified.InstallFont=为当前用户安装 Maple Mono 字体
chinesesimplified.LaunchProgram=启动 Nebula Terminal
chinesesimplified.UninstallProgram=卸载 Nebula Terminal

[Tasks]
Name: "installfont"; Description: "{cm:InstallFont}"
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

[Run]
Filename: "{app}\nebula.exe"; Description: "{cm:LaunchProgram}"; WorkingDir: "{%USERPROFILE}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 必须在 Inno 删除 nebula.exe 前调用应用自己的结构化清理逻辑，避免直接改写用户配置。
Filename: "{app}\nebula.exe"; Parameters: "setup-ai --remove"; WorkingDir: "{app}"; RunOnceId: "RemoveNebulaAiHooks"; Flags: runhidden skipifdoesntexist
