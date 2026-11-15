#ifndef AppVersion
  #error AppVersion must be supplied by package-windows.ps1
#endif
#ifndef StageDir
  #error StageDir must be supplied by package-windows.ps1
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by package-windows.ps1
#endif

[Setup]
AppId={{4EAB2D7E9-9EE9-4B45-A5CD-2583C95D5F7A}
AppName=Sugar Archive
AppVersion={#AppVersion}
DefaultDirName={autopf}\Sugar Archive
DefaultGroupName=Sugar Archive
OutputDir={#OutputDir}
OutputBaseFilename=Sugar-Archive-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
LicenseFile=..\LICENSE
UninstallDisplayIcon={app}\sugar-archive.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Sugar Archive"; Filename: "{app}\sugar-archive.exe"
Name: "{autodesktop}\Sugar Archive"; Filename: "{app}\sugar-archive.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Run]
Filename: "{app}\sugar-archive.exe"; Description: "{cm:LaunchProgram,Sugar Archive}"; Flags: nowait postinstall skipifsilent
