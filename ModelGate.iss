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

; 打包源 exe 与输出名可由 ISCC /D 覆盖：正式打包默认 dist\Model-Gate.exe；
; 验证版可指向独立文件名，避免覆盖正在使用的 exe。
#ifndef MyAppSourceExe
  #define MyAppSourceExe "dist\Model-Gate.exe"
#endif
#ifndef MyOutputBaseFilename
  #define MyOutputBaseFilename "Model-Gate-v" + MyAppVersion + "-setup"
#endif

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
; 安装位置由用户自由指定（lowest 权限下 auto 会因 {autopf} 指向用户目录而隐藏该页）
DisableDirPage=no
LicenseFile=
OutputDir=dist
OutputBaseFilename={#MyOutputBaseFilename}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
CloseApplications=yes

[Languages]
; 默认且唯一的安装向导语言：简体中文。语言文件随仓库提供，避免依赖本机 Inno Setup 的额外语言包。
Name: "chinesesimplified"; MessagesFile: "assets\inno\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#MyAppSourceExe}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; \
  Flags: ignoreversion signonce

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{#MyAppName} 数据目录"; Filename: "{app}\data"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; \
  Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; \
  Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 运行期数据默认保留（用量记录与配置属于用户数据），仅在用户手动删除
Type: files; Name: "{app}\data\modelgate.log"
