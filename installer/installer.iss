; AIS Monitor — Windows Installer
; Build: iscc /DMyAppVersion=X.Y.Z installer\installer.iss
; Output: dist\ais-monitor-setup.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "AIS Monitor"
#define MyAppPublisher "Bennekrouf"
#define MyAppURL       "https://github.com/Bennekrouf/ais-monitor"
#define MyAppExeName   "ais-monitor.exe"

[Setup]
AppId={{A3C7E1D4-5B2F-4A8E-9D6C-F1E3B7A2C5D8}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases/latest
; Install per-user into %LOCALAPPDATA%\AIS Monitor — no UAC prompt needed.
DefaultDirName={localappdata}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=ais-monitor-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName} {#MyAppVersion}
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
  Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\release\ais-monitor.exe";     DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\WebView2Loader.dll";   DestDir: "{app}"; Flags: ignoreversion
Source: "..\scripts\setup-windows.ps1";           DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{commondesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; Always install runtime dependencies — the script skips anything already present.
Filename: "powershell.exe"; \
  Parameters: "-ExecutionPolicy Bypass -NoProfile -File ""{app}\setup-windows.ps1"" -NoPrompt"; \
  StatusMsg: "Installing runtime dependencies — skipping already-installed tools..."; \
  Flags: waituntilterminated

; Offer to launch the app on the Finish page
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Launch {#MyAppName}"; \
  Flags: nowait postinstall skipifsilent

; ── Upgrade detection ─────────────────────────────────────────────────────────
[Code]

function GetInstalledVersion(): String;
var
  RegKey: String;
  Ver:    String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{A3C7E1D4-5B2F-4A8E-9D6C-F1E3B7A2C5D8}_is1';
  if not RegQueryStringValue(HKCU, RegKey, 'DisplayVersion', Ver) then
    Ver := '';
  Result := Ver;
end;

function GetUninstallString(): String;
var
  RegKey:    String;
  UninstStr: String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{A3C7E1D4-5B2F-4A8E-9D6C-F1E3B7A2C5D8}_is1';
  if not RegQueryStringValue(HKCU, RegKey, 'QuietUninstallString', UninstStr) then
    UninstStr := '';
  Result := UninstStr;
end;

function InitializeSetup(): Boolean;
var
  InstalledVer: String;
  NewVer:       String;
  Msg:          String;
  UninstStr:    String;
  ResultCode:   Integer;
  NL:           String;
begin
  Result := True;
  NL := #13#10;

  InstalledVer := GetInstalledVersion();
  if InstalledVer = '' then
    Exit;

  NewVer := '{#MyAppVersion}';

  if InstalledVer = NewVer then
    Msg := 'Version ' + InstalledVer + ' of {#MyAppName} is already installed.' + NL + NL +
           'Do you want to reinstall it?'
  else
    Msg := '{#MyAppName} is already installed.' + NL + NL +
           '  Installed version:  ' + InstalledVer + NL +
           '  New version:        ' + NewVer + NL + NL +
           'The old version will be removed before installing the new one.' + NL +
           'Your settings and data will be preserved.' + NL + NL +
           'Continue?';

  if MsgBox(Msg, mbConfirmation, MB_YESNO) = IDNO then
  begin
    Result := False;
    Exit;
  end;

  UninstStr := GetUninstallString();
  if UninstStr <> '' then
  begin
    Exec('>', UninstStr, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);
  end;
end;
