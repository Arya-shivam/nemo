; The Windows setup, gyotaku-setup-x86_64.exe. Built by CI with Inno Setup 6
; from the same files as the release zip:
;
;   iscc /DPayload=C:\path\to\gyotaku-x86_64-windows contrib\windows\gyotaku.iss
;
; It installs for the current user only, needs no administrator rights, and
; puts gyotaku in the same folder and Start menu entry install.ps1 does, so
; either one updates what the other installed.

#ifndef Payload
  #define Payload "..\..\dist\gyotaku-x86_64-windows"
#endif
; The version is read from the program itself, which has it built in.
#ifndef AppVersion
  #define AppVersion GetStringFileInfo(Payload + "\gyotaku-app.exe", "ProductVersion")
#endif

[Setup]
AppId=gyotaku
AppName=Nemo
AppVersion={#AppVersion}
AppVerName=Nemo {#AppVersion}
AppPublisher=xevrion
AppPublisherURL=https://github.com/xevrion/gyotaku
AppSupportURL=https://github.com/xevrion/gyotaku/issues
AppUpdatesURL=https://github.com/xevrion/gyotaku/releases
AppComments=Search screenshots and images, and organize files by name
DefaultDirName={localappdata}\Programs\gyotaku
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
UsedUserAreasWarning=no
; ARM64 Windows runs the x64 build through its built-in emulation.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\..\dist
OutputBaseFilename=gyotaku-setup-x86_64
SetupIconFile=..\..\assets\gyotaku.ico
WizardSmallImageFile=wizard-small-55.bmp,wizard-small-110.bmp
WizardStyle=modern
UninstallDisplayIcon={app}\gyotaku-app.exe
UninstallDisplayName=Nemo
Compression=lzma2/max
SolidCompression=yes
; gyotaku is stopped by PrepareToInstall below. Restart Manager would ask
; first, and can't close a program that's waiting with no window anyway.
CloseApplications=no

[Tasks]
Name: startwithwindows; Description: "Start with Windows, so new screenshots are read as you take them"; Flags: unchecked

[Files]
Source: "{#Payload}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{userprograms}\Nemo"; Filename: "{app}\gyotaku-app.exe"; WorkingDir: "{app}"; Comment: "Search screenshots and images"

[Registry]
; The same value the switch in gyotaku's settings writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "gyotaku"; ValueData: """{app}\gyotaku-app.exe"" --background"; Tasks: startwithwindows

[Run]
Filename: "{app}\gyotaku-app.exe"; Description: "Open Nemo"; Flags: nowait postinstall skipifsilent
; A silent update over a running gyotaku brings it back the way sign-in
; does: waiting for the summon key, with no window.
Filename: "{app}\gyotaku-app.exe"; Parameters: "--background"; Flags: nowait; Check: RestartQuietly

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

var
  WasRunning: Boolean;

{ Ends the window app and the reader. True if the window app was running.
  taskkill exits with 0 when it ended something. }
function StopGyotaku(): Boolean;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM gyotaku-app.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := Code = 0;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM gyotaku.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  { Give Windows a moment to release the files before they're replaced. }
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  WasRunning := StopGyotaku();
  Result := '';
end;

function RestartQuietly(): Boolean;
begin
  Result := WizardSilent() and WasRunning;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  { Background reading turned on from the app, by an older install somewhere
    else, points at this copy now. }
  if (CurStep = ssPostInstall) and RegValueExists(HKCU, RunKey, 'gyotaku') then
    RegWriteStringValue(HKCU, RunKey, 'gyotaku', '"' + ExpandConstant('{app}\gyotaku-app.exe') + '" --background');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    StopGyotaku();
    RegDeleteValue(HKCU, RunKey, 'gyotaku');
  end;
  { The index and settings stay unless asked, so reinstalling picks up where
    it left off. Silent uninstalls always keep them. }
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent() then
    if MsgBox('Also delete the index and settings?' + #13#10#13#10 + 'Your screenshots are never touched either way.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\gyotaku'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\gyotaku'), True, True, True);
    end;
end;
