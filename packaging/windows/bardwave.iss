; Bardwave - Inno Setup 6 installer script.
;
; Build (from the repository root, after `cargo build --release`):
;
;   ISCC.exe /DMyAppVersion=1.4.0 /DMyAppNumVersion=1.4.0.0 /Odist packaging\windows\bardwave.iss
;
; Parameters (all optional):
;   /DMyAppVersion=<x.y.z[-pre]>   shown in Programs and Features, used in the file name
;   /DMyAppNumVersion=<a.b.c.d>    numeric version stamped on the setup.exe (no suffixes)
;   /DMySourceExe=<path>           the release binary (default: target\release\soundshared.exe)
;   /O<dir>                        output directory (default: target\dist)
;
; What it does:
;   * installs Bardwave.exe into Program Files (needs administrator rights);
;   * Start menu shortcut, optional desktop shortcut;
;   * adds an inbound Windows Firewall rule for TCP 8080 (private and public
;     profiles) and removes it on uninstall. The rule has the same name that the
;     program itself uses ("Bardwave TCP <port>"), so the two never duplicate;
;   * never touches user data on uninstall: config.toml and the sound library
;     are not installed by this script, so the uninstaller does not know them.
;
; Note: config.toml is deliberately NOT shipped. Its presence next to the
; executable switches Bardwave into portable mode, and Program Files is not
; writable for normal users. Without it the program uses the per-user data
; directory (see PLAN.md, stage 0, data_dir()). If the port is changed in
; config.toml, the program adds its own rule for the new port the next time it
; runs elevated, or Windows asks on first listen.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef MySourceExe
  #define MySourceExe "..\..\target\release\soundshared.exe"
#endif

#define MyAppName "Bardwave"
#define MyAppPublisher "Bardwave"
#define MyAppURL "https://github.com/adamantida/Bardwave"
#define MyAppExeName "Bardwave.exe"
#define MyFirewallPort "8080"
#define MyFirewallRule "Bardwave TCP " + MyFirewallPort

[Setup]
; Never change AppId: it is how upgrades and the uninstaller find this install.
AppId={{6B0B5C0E-3E64-4D4E-9F0B-8A1D2C7F4B11}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
#ifdef MyAppNumVersion
VersionInfoVersion={#MyAppNumVersion}
#endif
VersionInfoDescription={#MyAppName} installer
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; Program Files and a firewall rule both need administrator rights.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
OutputDir=..\..\target\dist
OutputBaseFilename=Bardwave-v{#MyAppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Close a running Bardwave during an upgrade so the exe can be replaced.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "firewall"; Description: "Allow Bardwave through Windows Firewall (inbound TCP {#MyFirewallPort}, private and public networks) so players can connect"; GroupDescription: "Network:"

[Files]
Source: "{#MySourceExe}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.txt"; DestDir: "{app}"; Flags: ignoreversion
; The commented example lives in {app}\examples, not next to the exe (see the note above).
Source: "..\config.toml"; DestDir: "{app}\examples"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; Replace, never duplicate: drop a rule with the same name first (it may exist
; from an earlier version or from the program's own elevated start), then add.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#MyFirewallRule}"""; Flags: runhidden; Tasks: firewall; StatusMsg: "Configuring Windows Firewall..."
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#MyFirewallRule}"" dir=in action=allow protocol=TCP localport={#MyFirewallPort} profile=private,public enable=yes"; Flags: runhidden; Tasks: firewall; StatusMsg: "Configuring Windows Firewall..."
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Runs regardless of the "firewall" task: deleting a rule that is not there is harmless.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#MyFirewallRule}"""; Flags: runhidden; RunOnceId: "RemoveBardwaveFirewallRule"

; No [UninstallDelete] on purpose: config.toml, bardwave-library and the user
; data directory are the user's and survive uninstall and reinstall.
