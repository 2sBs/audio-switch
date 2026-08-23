#ifndef AppVersion
  #error AppVersion must be provided by scripts/package-installer.ps1
#endif

#ifndef SourceExe
  #define SourceExe "..\target\release\audio-switch.exe"
#endif

[Setup]
AppId={{D8550910-54AF-4788-96F8-F32E82AD971E}
AppName=Audio Switch
AppVersion={#AppVersion}
AppVerName=Audio Switch {#AppVersion}
AppPublisher=2sBs
AppPublisherURL=https://github.com/2sBs/audio-switch
AppSupportURL=https://github.com/2sBs/audio-switch/issues
AppUpdatesURL=https://github.com/2sBs/audio-switch/releases
DefaultDirName={localappdata}\Programs\Audio Switch
DefaultGroupName=Audio Switch
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\dist
OutputBaseFilename=audio-switch-{#AppVersion}-setup-windows-x86_64
SetupIconFile=..\crates\audio-switch-app\assets\tray\primary.ico
UninstallDisplayIcon={app}\audio-switch.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "audio-switch.exe"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Audio Switch"; Filename: "{app}\audio-switch.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\audio-switch.exe"; Description: "Launch Audio Switch and complete setup"; Flags: nowait postinstall skipifsilent
Filename: "{app}\audio-switch.exe"; Flags: nowait; Check: IsUpdateInstall

[UninstallRun]
Filename: "{app}\audio-switch.exe"; Parameters: "--remove-autostart"; Flags: runhidden; RunOnceId: "RemoveAudioSwitchAutostart"

[Code]
function IsUpdateInstall: Boolean;
begin
  Result := ExpandConstant('{param:UPDATE|0}') = '1';
end;
