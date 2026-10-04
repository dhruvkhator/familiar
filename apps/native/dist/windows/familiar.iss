; Familiar for Windows — per-user installer (Inno Setup 6). Adapted from zeron's dist/windows/zeron.iss (MIT).
;
; Built by .github/workflows/release.yml:
;   ISCC.exe /DAppVersion=0.1.0 /DPackageDir=<staged dir> /DOutputDir=<out> familiar.iss
;
; Installs into %LOCALAPPDATA%\Programs\Familiar without admin rights. User data lives in %USERPROFILE%\.familiar
; and is never touched by install, upgrade or uninstall.

#ifndef AppVersion
  #error AppVersion must be defined (/DAppVersion=x.y.z)
#endif
#ifndef PackageDir
  #error PackageDir must be defined (/DPackageDir=<staged package directory>)
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Never change AppId: it identifies the installation across upgrades.
AppId={{B031A143-7CAF-46CB-BC54-235853633C54}
AppName=Familiar
AppVersion={#AppVersion}
AppVerName=Familiar {#AppVersion}
AppPublisher=Familiar contributors
AppPublisherURL=https://github.com/dhruvkhator/familiar
AppSupportURL=https://github.com/dhruvkhator/familiar/issues
AppUpdatesURL=https://github.com/dhruvkhator/familiar/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Familiar
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=Familiar-{#AppVersion}-setup
SetupIconFile=familiar.ico
UninstallDisplayIcon={app}\familiar-native.exe
UninstallDisplayName=Familiar
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; A running Familiar is closed (it drains its work and stops its database) before files are replaced.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#PackageDir}\familiar-native.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Familiar"; Filename: "{app}\familiar-native.exe"
Name: "{autodesktop}\Familiar"; Filename: "{app}\familiar-native.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\familiar-native.exe"; Description: "{cm:LaunchProgram,Familiar}"; Flags: nowait postinstall skipifsilent
