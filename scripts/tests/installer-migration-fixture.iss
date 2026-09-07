#ifndef FixtureRoot
  #error FixtureRoot must point at an isolated directory under the workspace tmp directory.
#endif
#define MigrationFixture

[Setup]
AppId=PebrelInstallerMigrationFixture
AppName=Pebrel Installer Migration Fixture
AppVersion=0.0.0
DefaultDirName={#FixtureRoot}
CreateAppDir=no
Uninstallable=no
PrivilegesRequired=lowest
OutputBaseFilename=installer-migration-fixture
Compression=none
SetupLogging=no

[Code]
#include "..\installer-migration.iss"

var
  Checks: Integer;

function FixtureOpenFile(FileName: string; Access, Share: LongWord;
  Security: Integer; Creation, Flags: LongWord; Template: Integer): LongWord;
  external 'CreateFileW@kernel32.dll stdcall';
function FixtureCloseHandle(Handle: LongWord): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

procedure Check(Condition: Boolean; Name: string);
begin
  if not Condition then
    RaiseException(Name);
  Checks := Checks + 1;
end;

procedure WriteFixtureFile(Root, Relative: string);
var
  FileName: string;
begin
  FileName := AddBackslash(Root) + Relative;
  Check(ForceDirectories(ExtractFileDir(FileName)), 'create fixture parent');
  Check(SaveStringToFile(FileName, 'fixture data', False), 'write fixture file');
end;

procedure CheckPlanning;
begin
  Check(SuggestedInstallDir('', 'D:\Programs\Pebrel') = 'D:\Programs\Pebrel', 'fresh install default');
  Check(SuggestedInstallDir('D:\Program Files\Nebula Terminal', '') =
    'D:\Program Files\Pebrel', 'legacy directory uses the same parent');
  Check(SuggestedInstallDir('D:\apps\Custom Terminal', '') =
    'D:\apps\Custom Terminal', 'custom install directory is preserved');
  Check(SuggestedInstallDir('D:\apps\Pebrel', '') = 'D:\apps\Pebrel', 'Pebrel upgrades stay in place');
  Check(SuggestedInstallDir('D:\apps\NEBULA TERMINAL\', '') =
    'D:\apps\Pebrel', 'legacy directory comparison is case insensitive');
  Check(PathContainsDirectory('D:\tools;"D:\Programs\Pebrel\";D:\other',
    'd:\programs\pebrel'), 'PATH recognizes quotes, case and trailing slash');
  Check(not PathContainsDirectory('D:\apps\Pebrel-old', 'D:\apps\Pebrel'), 'PATH requires a full entry');
  Check(RemovePathDirectory('D:\tools;d:\apps\NEBULA TERMINAL\;D:\other',
    'D:\apps\Nebula Terminal') = 'D:\tools;D:\other', 'PATH preserves unrelated entries');
  Check(RemovePathDirectory('D:\apps\Nebula Terminal;D:\apps\Nebula Terminal',
    'D:\apps\Nebula Terminal') = '', 'PATH removes duplicate owned entries');
  Check(IsLegacyUninstaller('D:\apps\Nebula Terminal',
    'D:\apps\Nebula Terminal\unins003.exe'), 'registered numbered uninstaller accepted');
  Check(not IsLegacyUninstaller('D:\apps\Nebula Terminal',
    'D:\other\unins000.exe'), 'uninstaller must belong to the previous installation');
  Check(not IsLegacyUninstaller('D:\apps\Nebula Terminal',
    'D:\apps\Nebula Terminal\other.exe'), 'arbitrary executable is not an uninstaller');
end;

procedure CheckPayloadMigration(Root: string);
var
  OldDir, NewDir: string;
begin
  OldDir := Root + '\move\Nebula Terminal';
  NewDir := Root + '\move\Pebrel';
  WriteFixtureFile(OldDir, 'nebula.exe');
  WriteFixtureFile(OldDir, 'runtime\nebula-hook.exe');
  WriteFixtureFile(OldDir, 'runtime\conpty.dll');
  WriteFixtureFile(OldDir, 'docs\CHANGELOG.md');
  WriteFixtureFile(OldDir, 'data\nebula.toml');
  WriteFixtureFile(OldDir, 'notes.txt');
  WriteFixtureFile(OldDir, 'docs\custom.md');
  WriteFixtureFile(OldDir, 'unins003.exe');
  WriteFixtureFile(OldDir, 'unins003.dat');
  WriteFixtureFile(NewDir, 'pebrel.exe');
  RemoveLegacyPayload(OldDir, NewDir, OldDir + '\unins003.exe');
  Check(not FileExists(OldDir + '\nebula.exe'), 'old executable removed');
  Check(not FileExists(OldDir + '\runtime\nebula-hook.exe'), 'old helper removed');
  Check(not FileExists(OldDir + '\unins003.dat'), 'only registered old uninstall log removed');
  Check(FileExists(OldDir + '\data\nebula.toml'), 'configuration preserved');
  Check(FileExists(OldDir + '\notes.txt'), 'unknown root file preserved');
  Check(FileExists(OldDir + '\docs\custom.md'), 'unknown child file preserved');
  Check(FileExists(NewDir + '\pebrel.exe'), 'new executable preserved');
  RemoveLegacyPayload(OldDir, NewDir, OldDir + '\unins003.exe');
  Check(FileExists(OldDir + '\data\nebula.toml'), 'retry is idempotent');

  OldDir := Root + '\custom';
  WriteFixtureFile(OldDir, 'nebula.exe');
  WriteFixtureFile(OldDir, 'runtime\nebula-hook.exe');
  WriteFixtureFile(OldDir, 'pebrel.exe');
  WriteFixtureFile(OldDir, 'runtime\pebrel-hook.exe');
  WriteFixtureFile(OldDir, 'runtime\conpty.dll');
  WriteFixtureFile(OldDir, 'docs\CHANGELOG.md');
  WriteFixtureFile(OldDir, 'unins000.dat');
  RemoveLegacyPayload(OldDir, OldDir, OldDir + '\unins000.exe');
  Check(not FileExists(OldDir + '\nebula.exe'), 'in-place rename removes old executable');
  Check(FileExists(OldDir + '\pebrel.exe'), 'in-place rename preserves new executable');
  Check(FileExists(OldDir + '\runtime\pebrel-hook.exe'), 'in-place rename preserves new helper');
  Check(FileExists(OldDir + '\runtime\conpty.dll'), 'in-place rename preserves shared runtime');
  Check(FileExists(OldDir + '\docs\CHANGELOG.md'), 'in-place rename preserves current documentation');
  Check(FileExists(OldDir + '\unins000.dat'), 'in-place upgrade keeps the appended uninstall log');
end;

procedure CheckFailures(Root: string);
var
  OldDir, Ignored: string;
  Failed: Boolean;
  Handle: LongWord;
begin
  OldDir := Root + '\locked';
  WriteFixtureFile(OldDir, 'nebula.exe');
  Handle := FixtureOpenFile(OldDir + '\nebula.exe', $80000000, 0, 0, 3, 0, 0);
  Check(Handle <> $FFFFFFFF, 'lock fixture executable');
  Failed := False;
  try
    try
      RemoveLegacyPayload(OldDir, Root + '\new', '');
    except
      Failed := True;
    end;
  finally
    FixtureCloseHandle(Handle);
  end;
  Check(Failed, 'locked old executable produces an error');
  Check(FileExists(OldDir + '\nebula.exe'), 'locked file remains intact');
  RemoveLegacyPayload(OldDir, Root + '\new', '');
  Check(not FileExists(OldDir + '\nebula.exe'), 'retry succeeds after unlocking');

  Failed := False;
  try
    Ignored := SafePayloadPath(Root, '..\outside.exe');
  except
    Failed := True;
  end;
  Check(Failed, 'relative path cannot escape the old installation');
  Failed := False;
  try
    RemoveLegacyFile(Root + '\linked-install', 'nebula.exe');
  except
    Failed := True;
  end;
  Check(Failed, 'linked directory is rejected');
  Check(FileExists(Root + '\link-target\nebula.exe'), 'linked target file is preserved');
  Check(IsExecutableRunning(ExpandConstant('{srcexe}')), 'running executable is detected without termination');
  Check(not IsExecutableRunning(Root + '\not-running.exe'), 'absent executable does not block installation');
end;

procedure CheckShortcuts(Root: string);
var
  Shell, Shortcut: Variant;
  OldExe, NewExe, OldLink, NewLink: string;
begin
  OldExe := Root + '\shortcuts\nebula.exe';
  NewExe := Root + '\shortcuts\pebrel.exe';
  OldLink := Root + '\shortcuts\Nebula Terminal.lnk';
  NewLink := Root + '\shortcuts\Pebrel.lnk';
  WriteFixtureFile(Root, 'shortcuts\nebula.exe');
  WriteFixtureFile(Root, 'shortcuts\pebrel.exe');
  Shell := CreateOleObject('WScript.Shell');
  Shortcut := Shell.CreateShortcut(OldLink);
  Shortcut.TargetPath := OldExe;
  Shortcut.Save;
  Shortcut := Shell.CreateShortcut(NewLink);
  Shortcut.TargetPath := NewExe;
  Shortcut.Save;
  RemoveOwnedShortcut(OldLink, OldExe);
  RemoveOwnedShortcut(NewLink, OldExe);
  Check(not FileExists(OldLink), 'shortcut pointing at the old program is removed');
  Check(FileExists(NewLink), 'shortcut pointing at another program is preserved');
end;

function InitializeSetup: Boolean;
var
  Root, Report: string;
begin
  Root := ExpandConstant('{#FixtureRoot}');
  try
    CheckPlanning;
    CheckPayloadMigration(Root);
    CheckFailures(Root);
    CheckShortcuts(Root);
    Report := 'PASS: ' + IntToStr(Checks) + ' migration checks';
  except
    Report := 'FAIL: ' + GetExceptionMessage;
  end;
  SaveStringToFile(Root + '\result.txt', Report, False);
  Result := False;
end;
