; Inno Setup script for LeanPDF (the native app). Built by tools\publish.ps1 -Installer (or by the release workflow), which passes:
;   /DAppVersion=0.1.0   the version shown in the installer and in Windows' list of apps
;   /DSourceDir=...      the folder holding LeanPDF.exe, pdfium.dll and licenses\
;   /DOutputDir=...      where the setup program is written

#ifndef AppVersion
  #define AppVersion "0.4.1"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\LeanPDF-0.4.1-win-x64"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#ifndef AppName
  #define AppName "LeanPDF"
#endif
#ifndef AppGuid
  #define AppGuid "{{6F2A9C41-3B7E-4D58-9A16-C0E5D81B7F23}"
#endif
#define AppExe "LeanPDF.exe"
#define RepoUrl "https://github.com/srinadhmanchikalapudi/leanpdf"

[Setup]
; The AppId identifies the program to Windows: it must never change, so that a new version upgrades the old one instead of installing beside it.
AppId={#AppGuid}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=srinadhmanchikalapudi
AppPublisherURL={#RepoUrl}
AppSupportURL={#RepoUrl}/issues
AppUpdatesURL={#RepoUrl}/releases
VersionInfoVersion={#AppVersion}

; Installs for the current user only (no administrator prompt) into %LOCALAPPDATA%\Programs; the wizard offers "all users" for those who want it.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041

; A new version replaces the old one, closing the app if it is running.
CloseApplications=yes
RestartApplications=no

LicenseFile=..\LICENSE
SetupIconFile=..\native\assets\app.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
OutputDir={#OutputDir}
OutputBaseFilename=LeanPDF-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "openwith"; Description: "Show LeanPDF in the &Open with list for PDF files (does not change your default PDF app)"; GroupDescription: "File types:"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Adds LeanPDF to "Open with" for .pdf. It never becomes the default: Windows only changes that when the person chooses it.
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%1"""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".pdf"; ValueData: ""; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\.pdf\OpenWithList\{#AppExe}"; ValueType: none; Flags: uninsdeletekey; Tasks: openwith

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName}"; Flags: nowait postinstall skipifsilent
; An update started from inside the program passes /RESTARTAPP=1, so the program comes back by itself when the silent install is done.
Filename: "{app}\{#AppExe}"; Flags: nowait runasoriginaluser; Check: RestartRequested

[Code]
function RestartRequested: Boolean;
begin
  Result := ExpandConstant('{param:RESTARTAPP|0}') = '1';
end;

// Uninstalling removes the program only. The PDFs are never touched. LeanPDF's small settings file and its search index live in
// %APPDATA%\app.leanpdf.desktop; they are removed only if the person says so (never for a silent uninstall).
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: string;
begin
  if (CurUninstallStep = usPostUninstall) and (not UninstallSilent) then
  begin
    DataDir := ExpandConstant('{userappdata}\app.leanpdf.desktop');
    if DirExists(DataDir) then
      if MsgBox('Also delete LeanPDF''s settings and search index?' + #13#10#13#10 +
                'Your PDFs are not touched either way. They are stored in ' + DataDir + '.',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
