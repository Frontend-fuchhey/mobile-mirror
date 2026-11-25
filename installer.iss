; Inno Setup 6 Script for Mobile Mirror (Windows 64-bit)
; Compiles dist\stage\* into a single portable installer: dist\MobileMirror-Setup.exe

#ifndef AppVersion
  #define AppVersion "1.1.0"
#endif

#ifndef StageDir
  #define StageDir "dist\stage"
#endif

#ifndef OutputDir
  #define OutputDir "dist"
#endif

#define AppName "Mobile Mirror"
#define AppExeName "Mobile Mirror.exe"
#define AppPublisher "Mobile Mirror"
#define AppURL "https://github.com/Frontend-fuchhey/mobile-mirror"

[Setup]
AppId={{8F4C2A1E-9B7D-4E5F-A3C1-2D6E8F0B4A9C}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
AppCopyright=Copyright (C) 2026 Mobile Mirror Contributors
VersionInfoVersion={#AppVersion}.0
VersionInfoCompany={#AppPublisher}
VersionInfoDescription={#AppName} Setup Installer
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=MobileMirror-Setup
SetupIconFile=client\assets\app_icon.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
#ifdef SignToolCmd
SignTool=mysigntool
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Dirs]
Name: "{app}\recordings"; Permissions: users-modify

[Files]
; Main executable, Android DEX server, bundled ADB, SDL3, and FFmpeg runtime DLLs
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
