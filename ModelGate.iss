; ModelGate.iss — Inno Setup 安装脚本
; 版本号不在此硬编码：由 build.bat 读取 modelgate/__init__.py（唯一版本源）后经
;   iscc /DMyAppVersion=<ver> ModelGate.iss
; 注入。单独编译而未注入时回落为 0.0.0-dev（明确标记非正式版本，避免假版本号）。
;
; 产物：dist\Model-Gate-v<ver>-setup.exe
; 特性：可选桌面快捷方式（默认不勾选）、开始菜单项、安装后启动、运行中检测。
; 数据落在 exe 同目录 data\，配置为 exe 同目录 bridge.config.json（可随程序带走）。

#define MyAppName "Model-Gate"
#ifndef MyAppVersion
  #define MyAppVersion "0.0.0-dev"
#endif
#define MyAppPublisher "Model-Gate"
#define MyAppExeName "Model-Gate.exe"
#define MyAppURL "https://github.com/"

[Setup]
AppId={{8F3C4A21-6D9E-4B7A-9C15-2B0E7D5F4A63}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=
OutputDir=dist
OutputBaseFilename=Model-Gate-v{#MyAppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
CloseApplications=yes

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Default.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "quicklaunchicon"; Description: "{cm:CreateQuickLaunchIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked; OnlyBelowVersion: 6.1

[Files]
Source: "dist\{#MyAppExeName}"; DestDir: "{app}"; \
  Flags: ignoreversion signonce

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{#MyAppName} 数据目录"; Filename: "{app}\data"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  Tasks: desktopicon
Name: "{userappdata}\Microsoft\Internet Explorer\Quick Launch\{#MyAppName}"; \
  Filename: "{app}\{#MyAppExeName}"; Tasks: quicklaunchicon

[Run]
Filename: "{app}\{#MyAppExeName}"; \
  Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; \
  Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 运行期数据默认保留（用量记录与配置属于用户数据），仅在用户手动删除
Type: files; Name: "{app}\data\modelgate.log"
