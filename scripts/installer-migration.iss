// Included by the product installer and the isolated migration fixture.
const
  ProductUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{61022144-7D0A-4E54-94F2-C329A8F58656}_is1';
  ProductSettingsKey = 'Software\Pebrel';
  LegacySettingsKey = 'Software\Nebula Terminal';

var
  PreviousInstallDir: string;
  LegacyInstallDir: string;
  LegacyUninstaller: string;
  MigrationFailed: Boolean;

function MigrationFileAttributes(FileName: string): LongWord;
  external 'GetFileAttributesW@kernel32.dll stdcall';

function NormalizedDirectory(Value: string): string;
begin
  Result := RemoveBackslashUnlessRoot(ExpandFileName(RemoveQuotes(Trim(Value))));
end;

function SameDirectory(Left, Right: string): Boolean;
begin
  Result := CompareText(NormalizedDirectory(Left), NormalizedDirectory(Right)) = 0;
end;

function SuggestedInstallDir(Previous, Fallback: string): string;
begin
  Result := Fallback;
  if Previous = '' then
    Exit;
  Result := NormalizedDirectory(Previous);
  if CompareText(ExtractFileName(Result), 'Nebula Terminal') = 0 then
    Result := AddBackslash(ExtractFileDir(Result)) + 'Pebrel';
end;

function PathContainsDirectory(Value, Directory: string): Boolean;
var
  Entry: string;
  Separator: Integer;
begin
  Result := False;
  repeat
    Separator := Pos(';', Value);
    if Separator = 0 then
      Separator := Length(Value) + 1;
    Entry := Copy(Value, 1, Separator - 1);
    if (Trim(Entry) <> '') and SameDirectory(Entry, Directory) then begin
      Result := True;
      Exit;
    end;
    Delete(Value, 1, Separator);
  until Value = '';
end;

function RemovePathDirectory(Value, Directory: string): string;
var
  Entry: string;
  Separator: Integer;
  FirstEntry: Boolean;
begin
  Result := '';
  FirstEntry := True;
  repeat
    Separator := Pos(';', Value);
    if Separator = 0 then
      Separator := Length(Value) + 1;
    Entry := Copy(Value, 1, Separator - 1);
    if (Trim(Entry) = '') or not SameDirectory(Entry, Directory) then begin
      if not FirstEntry then
        Result := Result + ';';
      Result := Result + Entry;
      FirstEntry := False;
    end;
    Delete(Value, 1, Separator);
  until Value = '';
end;

function IsLegacyUninstaller(Directory, FileName: string): Boolean;
var
  Name: string;
  Index: Integer;
begin
  Name := Lowercase(ExtractFileName(FileName));
  Result := SameDirectory(ExtractFileDir(FileName), Directory) and
    (Length(Name) = 12) and (Copy(Name, 1, 5) = 'unins') and
    (Copy(Name, 9, 4) = '.exe');
  if Result then
    for Index := 6 to 8 do
      if (Name[Index] < '0') or (Name[Index] > '9') then
        Result := False;
end;

function DefaultInstallDir(Param: string): string;
begin
  Result := SuggestedInstallDir(PreviousInstallDir,
    ExpandConstant('{localappdata}\Programs\Pebrel'));
end;

procedure DiscoverLegacyInstallation;
var
  Pending, UninstallCommand: string;
begin
  RegQueryStringValue(HKCU, ProductUninstallKey, 'Inno Setup: App Path', PreviousInstallDir);
  if (PreviousInstallDir <> '') and
    FileExists(AddBackslash(PreviousInstallDir) + 'nebula.exe') then begin
    LegacyInstallDir := NormalizedDirectory(PreviousInstallDir);
    if RegQueryStringValue(HKCU, ProductUninstallKey, 'UninstallString', UninstallCommand) then begin
      UninstallCommand := RemoveQuotes(Trim(UninstallCommand));
      if IsLegacyUninstaller(LegacyInstallDir, UninstallCommand) then
        LegacyUninstaller := UninstallCommand;
    end;
  end;
  if RegQueryStringValue(HKCU, ProductSettingsKey, 'PendingLegacyInstallDir', Pending) then begin
    if Trim(Pending) = '' then
      RaiseException('The pending legacy installation directory is empty.');
    if (LegacyInstallDir <> '') and not SameDirectory(LegacyInstallDir, Pending) then
      RaiseException('Another legacy installation is awaiting migration: ' + Pending);
    LegacyInstallDir := NormalizedDirectory(Pending);
    RegQueryStringValue(HKCU, ProductSettingsKey, 'PendingLegacyUninstaller', LegacyUninstaller);
    if not IsLegacyUninstaller(LegacyInstallDir, LegacyUninstaller) then
      LegacyUninstaller := '';
  end;
end;

function IsExecutableRunning(FileName: string): Boolean;
var
  Locator, Service, Processes, Process: Variant;
  Index: Integer;
  Name: string;
begin
  Result := False;
  if not FileExists(FileName) then
    Exit;
  Name := ExtractFileName(FileName);
  StringChangeEx(Name, '''', '''''', True);
  Locator := CreateOleObject('WbemScripting.SWbemLocator');
  Service := Locator.ConnectServer('', 'root\CIMV2');
  Processes := Service.ExecQuery('SELECT ExecutablePath FROM Win32_Process WHERE Name = ''' + Name + '''');
  for Index := 0 to Processes.Count - 1 do begin
    Process := Processes.ItemIndex(Index);
    if not VarIsNull(Process.ExecutablePath) then
      if SameDirectory(Process.ExecutablePath, FileName) then begin
        Result := True;
        Exit;
      end;
  end;
end;

function SafePayloadPath(Root, Relative: string): string;
var
  Part, Parent: string;
  Attributes: LongWord;
begin
  Root := NormalizedDirectory(Root);
  Result := ExpandFileName(AddBackslash(Root) + Relative);
  if Pos(Lowercase(AddBackslash(Root)), Lowercase(Result)) <> 1 then
    RaiseException('Invalid legacy payload path: ' + Result);
  Part := Result;
  repeat
    Attributes := MigrationFileAttributes(Part);
    if (Attributes <> $FFFFFFFF) and ((Attributes and $400) <> 0) then
      RaiseException('Linked legacy installation path must be migrated manually: ' + Part);
    Parent := ExtractFileDir(Part);
    if SameDirectory(Parent, Part) then
      Break;
    Part := Parent;
  until Part = '';
end;

procedure RemoveLegacyFile(Root, Relative: string);
var
  FileName: string;
begin
  FileName := SafePayloadPath(Root, Relative);
  if FileExists(FileName) and not DeleteFile(FileName) then
    RaiseException('Unable to remove old installation file: ' + FileName);
end;

procedure RemoveEmptyLegacyDir(Root, Relative: string);
var
  Directory: string;
begin
  Directory := SafePayloadPath(Root, Relative);
  if DirExists(Directory) then
    RemoveDir(Directory);
end;

procedure RemoveLegacyPayload(OldDir, NewDir, Uninstaller: string);
begin
  if OldDir = '' then
    Exit;
  RemoveLegacyFile(OldDir, 'runtime\nebula-hook.exe');
  RemoveLegacyFile(OldDir, 'nebula-hook.exe');
  RemoveLegacyFile(OldDir, 'skills\nebula-runtime\agents\openai.yaml');
  RemoveLegacyFile(OldDir, 'skills\nebula-runtime\SKILL.md');
  RemoveEmptyLegacyDir(OldDir, 'skills\nebula-runtime\agents');
  RemoveEmptyLegacyDir(OldDir, 'skills\nebula-runtime');
  if not SameDirectory(OldDir, NewDir) then begin
    RemoveLegacyFile(OldDir, 'runtime\conpty.dll');
    RemoveLegacyFile(OldDir, 'runtime\OpenConsole.exe');
    RemoveLegacyFile(OldDir, 'conpty.dll');
    RemoveLegacyFile(OldDir, 'OpenConsole.exe');
    RemoveLegacyFile(OldDir, 'README.md');
    RemoveLegacyFile(OldDir, 'docs\CHANGELOG.md');
    RemoveLegacyFile(OldDir, 'docs\INSTALL.md');
    RemoveLegacyFile(OldDir, 'docs\lua-configuration.md');
    RemoveLegacyFile(OldDir, 'docs\runtime-control-api.md');
    RemoveLegacyFile(OldDir, 'docs\runtime-api-v1.schema.json');
    RemoveLegacyFile(OldDir, 'fonts\MapleMonoNormal-NF-CN-Regular.ttf');
    RemoveLegacyFile(OldDir, 'licenses\LICENSE');
    RemoveLegacyFile(OldDir, 'licenses\LICENSE-LUA');
    RemoveLegacyFile(OldDir, 'licenses\LICENSE-MLUA');
    RemoveLegacyFile(OldDir, 'licenses\THIRD-PARTY-NOTICES');
    if IsLegacyUninstaller(OldDir, Uninstaller) then begin
      RemoveLegacyFile(OldDir, ExtractFileName(Uninstaller));
      RemoveLegacyFile(OldDir, ChangeFileExt(ExtractFileName(Uninstaller), '.dat'));
      RemoveLegacyFile(OldDir, ChangeFileExt(ExtractFileName(Uninstaller), '.msg'));
    end;
    RemoveEmptyLegacyDir(OldDir, 'runtime');
    RemoveEmptyLegacyDir(OldDir, 'docs');
    RemoveEmptyLegacyDir(OldDir, 'fonts');
    RemoveEmptyLegacyDir(OldDir, 'licenses');
    RemoveEmptyLegacyDir(OldDir, 'skills');
  end;
  RemoveLegacyFile(OldDir, 'nebula.exe');
  if not SameDirectory(OldDir, NewDir) then
    RemoveDir(OldDir);
end;

procedure RemoveOwnedShortcut(FileName, Target: string);
var
  Shell, Shortcut: Variant;
begin
  if not FileExists(FileName) then
    Exit;
  Shell := CreateOleObject('WScript.Shell');
  Shortcut := Shell.CreateShortcut(FileName);
  if SameDirectory(Shortcut.TargetPath, Target) and not DeleteFile(FileName) then
    RaiseException('Unable to remove old shortcut: ' + FileName);
end;

procedure RemoveOwnedRegistryKey(Key, ValueName, Expected: string);
var
  Value: string;
begin
  if RegQueryStringValue(HKCU, Key, ValueName, Value) and
    (CompareText(Value, Expected) = 0) and not RegDeleteKeyIncludingSubkeys(HKCU, Key) then
    RaiseException('Unable to remove old registration: ' + Key);
end;

procedure RemoveOwnedContextMenu(Key, ExpectedCommand: string);
var
  Command: string;
begin
  if RegQueryStringValue(HKCU, Key + '\command', '', Command) and
    (CompareText(Command, ExpectedCommand) = 0) and not RegDeleteKeyIncludingSubkeys(HKCU, Key) then
    RaiseException('Unable to remove old context menu: ' + Key);
end;

procedure MigrateLegacyIntegrations;
var
  Executable, ExistingPath, Key: string;
begin
  Executable := AddBackslash(LegacyInstallDir) + 'nebula.exe';
  RemoveOwnedShortcut(ExpandConstant('{autodesktop}\Nebula Terminal.lnk'), Executable);
  RemoveOwnedShortcut(ExpandConstant('{autodesktop}\Pebrel.lnk'), Executable);
  RemoveOwnedShortcut(ExpandConstant('{userstartup}\Nebula Terminal.lnk'), Executable);
  RemoveOwnedShortcut(ExpandConstant('{userstartup}\Pebrel.lnk'), Executable);
  RemoveOwnedShortcut(ExpandConstant('{userprograms}\Nebula Terminal\Nebula Terminal.lnk'), Executable);
  RemoveOwnedShortcut(ExpandConstant('{userprograms}\Pebrel\Pebrel.lnk'), Executable);
  if LegacyUninstaller <> '' then begin
    RemoveOwnedShortcut(ExpandConstant('{userprograms}\Nebula Terminal\Uninstall Nebula Terminal.lnk'), LegacyUninstaller);
    RemoveOwnedShortcut(ExpandConstant('{userprograms}\Nebula Terminal\卸载 Nebula Terminal.lnk'), LegacyUninstaller);
  end;
  RemoveDir(ExpandConstant('{userprograms}\Nebula Terminal'));
  RemoveOwnedRegistryKey('Software\Microsoft\Windows\CurrentVersion\App Paths\nebula.exe', '', Executable);
  Key := 'Software\Classes\Directory\Background\shell\NebulaTerminal';
  RemoveOwnedContextMenu(Key, '"' + Executable + '" --gpui --working-directory "%V"');
  Key := 'Software\Classes\Directory\shell\NebulaTerminal';
  RemoveOwnedContextMenu(Key, '"' + Executable + '" --gpui --working-directory "%1"');
  if RegValueExists(HKCU, LegacySettingsKey, 'InstallerAddedToPath') then begin
    if SameDirectory(LegacyInstallDir, ExpandConstant('{app}')) then begin
      if not RegWriteDWordValue(HKCU, ProductSettingsKey, 'InstallerAddedToPath', 1) then
        RaiseException('Unable to transfer PATH ownership.');
    end else if RegQueryStringValue(HKCU, 'Environment', 'Path', ExistingPath) then begin
      if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path',
        RemovePathDirectory(ExistingPath, LegacyInstallDir)) then
        RaiseException('Unable to remove old installation directory from PATH.');
    end;
    if not RegDeleteValue(HKCU, LegacySettingsKey, 'InstallerAddedToPath') then
      RaiseException('Unable to clear previous PATH ownership.');
    RegDeleteKeyIfEmpty(HKCU, LegacySettingsKey);
  end;
end;

#ifndef MigrationFixture
function InitializeSetup: Boolean;
begin
  Result := True;
  try
    DiscoverLegacyInstallation;
  except
    Result := False;
    SuppressibleMsgBox(FmtMessage(CustomMessage('MigrationPreflightFailed'), [GetExceptionMessage]),
      mbError, MB_OK, IDOK);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): string;
var
  Executable: string;
begin
  Result := '';
  if LegacyInstallDir = '' then
    Exit;
  try
    Executable := SafePayloadPath(LegacyInstallDir, 'nebula.exe');
    if IsExecutableRunning(Executable) then
      Result := FmtMessage(CustomMessage('CloseLegacyProgram'), [Executable]);
  except
    Result := FmtMessage(CustomMessage('MigrationPreflightFailed'), [GetExceptionMessage]);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep <> ssPostInstall) or (LegacyInstallDir = '') then
    Exit;
  try
    if not RegWriteStringValue(HKCU, ProductSettingsKey, 'PendingLegacyInstallDir', LegacyInstallDir) or
      not RegWriteStringValue(HKCU, ProductSettingsKey, 'PendingLegacyUninstaller', LegacyUninstaller) then
      RaiseException('Unable to record the previous installation for migration.');
    MigrateLegacyIntegrations;
    RemoveLegacyPayload(LegacyInstallDir, ExpandConstant('{app}'), LegacyUninstaller);
    if not RegDeleteValue(HKCU, ProductSettingsKey, 'PendingLegacyInstallDir') or
      not RegDeleteValue(HKCU, ProductSettingsKey, 'PendingLegacyUninstaller') then
      RaiseException('Unable to clear the completed migration record.');
  except
    MigrationFailed := True;
    Log('Legacy migration failed: ' + GetExceptionMessage);
    SuppressibleMsgBox(FmtMessage(CustomMessage('MigrationFailed'), [GetExceptionMessage]),
      mbError, MB_OK, IDOK);
  end;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if MigrationFailed then
    Result := 1;
end;
#endif
