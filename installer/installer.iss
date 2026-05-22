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
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=ais-monitor-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
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
Name: "installdeps"; \
  Description: "Install runtime dependencies (Azure CLI)"; \
  GroupDescription: "Runtime dependencies:"; \
  Flags: checkedonce

[Files]
Source: "..\target\release\ais-monitor.exe";     DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\WebView2Loader.dll";   DestDir: "{app}"; Flags: ignoreversion
Source: "..\scripts\setup-windows.ps1";           DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{commondesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "powershell.exe"; \
  Parameters: "-ExecutionPolicy Bypass -NoProfile -File ""{app}\setup-windows.ps1"" -NoPrompt"; \
  Tasks: installdeps; \
  StatusMsg: "Installing runtime dependencies..."; \
  Flags: waituntilterminated

Filename: "{app}\{#MyAppExeName}"; \
  Description: "Launch {#MyAppName}"; \
  Flags: nowait postinstall skipifsilent
