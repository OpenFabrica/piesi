; Windows CLI 的当前用户安装程序；仅清理本安装程序添加的 PATH 项。
[Setup]
AppId={{B3079E72-46DA-4690-A51A-21B2E6FC1F54}
AppName=Piesi CLI
AppVersion={#AppVersion}
AppPublisher=OpenFabrica
AppPublisherURL=https://github.com/OpenFabrica/piesi
DefaultDirName={localappdata}\Programs\Piesi CLI
DefaultGroupName=Piesi CLI
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=piesi-cli-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
UninstallDisplayName=Piesi CLI
WizardStyle=modern
LicenseFile={#StageDir}\LICENSE-MIT

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "addtopath"; Description: "Add Piesi CLI to the current user's PATH"; Flags: checkedonce

[Icons]
Name: "{group}\Piesi CLI Help"; Filename: "{app}\README.md"
Name: "{group}\Uninstall Piesi CLI"; Filename: "{uninstallexe}"

[Code]
// 按 PATH 分量比较，避免把相似前缀或其它程序的路径当作本程序。
function HasPath(Value, Entry: String): Boolean;
begin
  Result := Pos(';' + Lowercase(Entry) + ';', ';' + Lowercase(Value) + ';') > 0;
end;

// 安装完成后只添加当前用户 PATH；升级时保留原有的卸载清理标记。
procedure CurStepChanged(CurStep: TSetupStep);
var
  Value, Entry: String;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then begin
    Entry := ExpandConstant('{app}');
    RegQueryStringValue(HKCU, 'Environment', 'Path', Value);
    if not HasPath(Value, Entry) then begin
      if (Value <> '') and (Value[Length(Value)] <> ';') then
        Value := Value + ';';
      if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Value + Entry) then
        RaiseException('Unable to update the current user PATH');
      if not RegWriteStringValue(HKCU, 'Software\OpenFabrica\Piesi CLI', 'AddedPath', Entry) then
        RaiseException('Unable to record the PATH entry');
    end;
  end;
end;

// 卸载时逐项移除本程序曾添加的路径，保留后来加入的其它 PATH 项。
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Value, Entry, Item, Updated: String;
  Split: Integer;
begin
  if (CurUninstallStep = usPostUninstall) and
     RegQueryStringValue(HKCU, 'Software\OpenFabrica\Piesi CLI', 'AddedPath', Entry) then begin
    RegQueryStringValue(HKCU, 'Environment', 'Path', Value);
    Updated := '';
    while Value <> '' do begin
      Split := Pos(';', Value);
      if Split = 0 then Split := Length(Value) + 1;
      Item := Copy(Value, 1, Split - 1);
      Delete(Value, 1, Split);
      if Lowercase(Item) <> Lowercase(Entry) then begin
        if Updated <> '' then Updated := Updated + ';';
        Updated := Updated + Item;
      end;
    end;
    if not RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Updated) then
      RaiseException('Unable to remove the Piesi CLI PATH entry');
    RegDeleteValue(HKCU, 'Software\OpenFabrica\Piesi CLI', 'AddedPath');
    RegDeleteKeyIfEmpty(HKCU, 'Software\OpenFabrica\Piesi CLI');
  end;
end;
