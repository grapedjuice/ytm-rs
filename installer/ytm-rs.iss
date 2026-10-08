; Inno Setup script for ytm-rs.
; Build: cargo build --release, then
;   iscc installer\ytm-rs.iss
; Output: installer\Output\ytm-rs-setup-<version>.exe

#define AppName "ytm-rs"
#define AppVersion "0.2.6"
#define AppExe "ytm-rs.exe"

[Setup]
AppId={{6E0B7C4A-2D1F-4C8E-9B3A-7F5D1E2A9C40}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=grapedjuice
AppPublisherURL=https://github.com/grapedjuice/ytm-rs
AppSupportURL=https://github.com/grapedjuice/ytm-rs/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default (no UAC prompt); the user can pick all-users instead.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputBaseFilename=ytm-rs-setup-{#AppVersion}
Compression=none
SolidCompression=no
WizardStyle=modern
SetupIconFile=..\assets\icon\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
