; ModelGate.iss — Inno Setup 安装脚本
; 版本号不在此硬编码：由 rust/model-gate/build-installer.ps1 读取 Cargo.toml（唯一版本源）后经
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
#define MyAppURL "https://github.com/klaus2918/model-bridege"

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
RestartApplications=yes
UsePreviousAppDir=yes

; 关闭与安装目录的约定（与应用内更新 src/update.rs、src/lifecycle.rs 配套）：
; 1) 应用内更新走静默安装，命令行带 /NOCLOSEAPPLICATIONS。静默模式下 Inno
;    默认会调 Windows Restart Manager 去关闭占用文件的进程，而本应用是托盘
;    常驻程序，那一步处理不好就会一直等下去（用户看到的「卡死在关闭上」）。
;    因此更新流程由应用自己按序关闭：停 HTTP 服务释放端口、数据库落盘、
;    退出释放 exe 句柄；安装参数显式关掉 Restart Manager 这一步。
; 2) 手动双击安装包（非静默）时保留 CloseApplications=yes：由应用侧正确
;    响应 WM_CLOSE 完成关闭，不会卡死。
; 3) 安装目录：UsePreviousAppDir=yes 保证默认位置就是原本的安装位置；应用内
;    更新还会显式传 /DIR=，两者一致，不会出现「装到别处」。

[Languages]
; 默认且唯一的安装向导语言：简体中文。语言文件随仓库提供，避免依赖本机 Inno Setup 的额外语言包。
Name: "chinesesimplified"; MessagesFile: "assets\inno\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#MyAppSourceExe}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; \
  Flags: ignoreversion signonce
; 随包文件：默认配置（安装后落在程序目录，改过就永远保留）。
; onlyifdoesntexist：目标已存在就跳过，绝不覆盖用户已有配置（升级与静默更新同样不覆盖）。
; uninsneveruninstall：卸载时不删除这些文件（用户配置与数据不随卸载消失）。
; 配置说明不再随包发独立页面：面板顶部「配置说明」按钮打开软件内弹窗（同一套样式与主题）。
Source: "rust\model-gate\default_config.json"; DestDir: "{app}"; DestName: "bridge.config.json"; \
  Flags: onlyifdoesntexist uninsneveruninstall

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
