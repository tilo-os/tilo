#define MyAppName "Tilo Imager"
#define MyAppPublisher "Tilo Project"
#define MyAppExeName "tilo-imager.exe"
#ifndef MyVersion
  #define MyVersion "0.1.0"
#endif
#ifndef MyOut
  #define MyOut "dist/tilo-imager"
#endif

[Setup]
AppId={{A47B49F8-4B63-4CE8-886D-3C124393028C}
AppName={#MyAppName}
AppVersion={#MyVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\Tilo Imager
DefaultGroupName=Tilo
OutputDir={#MyOut}
OutputBaseFilename=tilo-imager-setup-windows-x86_64
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
WizardStyle=modern
UninstallDisplayIcon={app}\{#MyAppExeName}

[Files]
Source: "dist\tilo-imager\tilo-imager-windows-x86_64\tilo-imager.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "dist\tilo-imager\tilo-imager-windows-x86_64\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "dist\tilo-imager\tilo-imager-windows-x86_64\Start-Tilo-Imager.ps1"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Tilo Imager"; Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; IconFilename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\Tilo Imager"; Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; IconFilename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Run]
Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; Description: "Launch Tilo Imager"; Flags: nowait postinstall skipifsilent
