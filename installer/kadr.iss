#ifndef AppVersion
  #error AppVersion is required
#endif

[Setup]
AppId={{2B19CCF9-6BB1-4FCA-9EB2-BCC843A47F3F}
AppName=Кадр
AppVersion={#AppVersion}
AppPublisher=qqew666
AppPublisherURL=https://github.com/qqew666/kadr-wallpaper
AppUpdatesURL=https://github.com/qqew666/kadr-wallpaper/releases
DefaultDirName={localappdata}\Programs\KadrWallpaper
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
MinVersion=10.0
CloseApplications=yes
CloseApplicationsFilter=kadr-wallpaper.exe
RestartApplications=no
UninstallDisplayIcon={app}\kadr-wallpaper.exe
SetupIconFile=..\assets\kadr.ico
OutputDir=..\dist
OutputBaseFilename=Kadr-Setup
Compression=lzma2
SolidCompression=yes

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\dist\kadr-wallpaper.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Кадр"; Filename: "{app}\kadr-wallpaper.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\kadr-wallpaper.exe"; Description: "{cm:LaunchProgram,Кадр}"; Flags: nowait postinstall skipifsilent

[Code]
procedure CurUninstallStepChanged(Step: TUninstallStep);
var
  Key, Entry, Command, InstalledCommand: string;
begin
  if Step = usPostUninstall then begin
    Key := 'Software\Microsoft\Windows\CurrentVersion\Run';
    Entry := 'KadrWallpaper';
    InstalledCommand := '"' + ExpandConstant('{app}\kadr-wallpaper.exe') + '" --startup';
    if RegQueryStringValue(HKCU, Key, Entry, Command) and
       (CompareText(Command, InstalledCommand) = 0) then
      RegDeleteValue(HKCU, Key, Entry);
  end;
end;
