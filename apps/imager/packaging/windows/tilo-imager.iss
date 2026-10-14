#define MyAppName "Tilo Installer"
#define MyAppPublisher "Tilo Project"
#define MyAppExeName "tilo-imager.exe"
#ifndef MyVersion
  #define MyVersion "0.1.0"
#endif
#ifndef MyOut
  #define MyOut "dist/tilo-imager"
#endif

#ifndef MyArch
  #define MyArch "x86_64"
#endif
#ifndef MyInstallerArch
  #define MyInstallerArch "x64compatible"
#endif
#ifndef MyBundle
  #error MyBundle must be the absolute path to the verified package directory
#endif

[Setup]
AppId={{A47B49F8-4B63-4CE8-886D-3C124393028C}
AppName={#MyAppName}
AppVersion={#MyVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\Tilo Imager
DefaultGroupName=Tilo
OutputDir={#MyOut}
OutputBaseFilename=tilo-imager-setup-windows-{#MyArch}
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed={#MyInstallerArch}
ArchitecturesInstallIn64BitMode={#MyInstallerArch}
PrivilegesRequired=lowest
WizardStyle=modern
UninstallDisplayIcon={app}\{#MyAppExeName}

[Files]
Source: "{#MyBundle}\tilo-imager.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyBundle}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyBundle}\Start-Tilo-Imager.ps1"; DestDir: "{app}"; Flags: ignoreversion

; Remove only the shortcuts created under the previous display name.
; Keep the legacy install directory and AppId so upgrades reuse the installation.
[InstallDelete]
Type: files; Name: "{autoprograms}\Tilo Imager.lnk"
Type: files; Name: "{autodesktop}\Tilo Imager.lnk"

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; IconFilename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; IconFilename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Run]
Filename: "{sysnative}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\Start-Tilo-Imager.ps1"""; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent
