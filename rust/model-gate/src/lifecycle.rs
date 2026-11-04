//! 关闭与安装编排：让「安装并重启」真的能把新版本装上。
//!
//! 这里解决两个历史上没做对的问题：
//!
//! 1. **关闭会卡死**：Inno Setup 在静默模式下会调用 Windows Restart Manager
//!    去「温柔地」关掉占用文件的进程——发的是 `WM_CLOSE`。本应用是托盘常驻
//!    程序，托盘窗口不处理 `WM_CLOSE`，Inno 就会一直等下去（用户看到的
//!    「卡死在关闭上」）。因此安装参数里必须带 `/NOCLOSEAPPLICATIONS`，
//!    把「关闭」这件事交给我们自己按确定步骤做完。
//!
//! 2. **装完起不来**：应用自己退出后没人再拉起新版本。这里用一个独立的
//!    看护脚本完成「等本进程退出 → 静默安装 → 拉起新版本」三步，
//!    脚本由本进程在退出前启动，因此不受本进程退出影响。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// 托盘消息循环专用消息：收到即表示「请退出消息循环」。
///
/// 取 `WM_APP + 1`：属于应用自定义消息区间，不会和系统消息冲突。
pub const WM_APP_SHUTDOWN: u32 = 0x8001;

/// 关闭协调器：把「谁发起关闭」与「关闭要做的事」解耦。
///
/// - HTTP 服务线程用它做优雅退出（`axum::serve` 的 graceful shutdown）；
/// - 托盘线程用它退出消息循环；
/// - 更新流程用它触发「关掉自己 → 让安装器接手」。
pub struct Shutdown {
    /// 是否已经请求关闭（幂等）。
    requested: AtomicBool,
    /// HTTP 服务是否已经停止（端口已释放）。
    server_stopped: AtomicBool,
    /// 托盘线程 id：用于把「请退出」投递到它的消息队列。
    tray_thread: AtomicU32,
    /// 通知 HTTP 服务停止的信号。
    server_tx: tokio::sync::watch::Sender<bool>,
    server_rx: tokio::sync::watch::Receiver<bool>,
    /// 「本进程即将退出」的信号文件路径（安装看护脚本在等它）。
    exit_marker: std::sync::Mutex<Option<PathBuf>>,
}

impl Shutdown {
    /// 新建协调器。
    pub fn new() -> Arc<Self> {
        let (server_tx, server_rx) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            requested: AtomicBool::new(false),
            server_stopped: AtomicBool::new(false),
            tray_thread: AtomicU32::new(0),
            server_tx,
            server_rx,
            exit_marker: std::sync::Mutex::new(None),
        })
    }

    /// 登记「本进程即将退出」的信号文件（安装看护脚本在等它出现）。
    pub fn set_exit_marker(&self, path: PathBuf) {
        if let Ok(mut slot) = self.exit_marker.lock() {
            *slot = Some(path);
        }
    }

    /// 写出「本进程即将退出」信号：看护脚本看到它就开始安装。
    ///
    /// 必须在**真正退出之前**调用：exe 句柄是在进程结束才释放的，
    /// 看护脚本拿到信号后还会再等一会儿，确保文件不被占用。
    pub fn signal_exit(&self) {
        let path = match self.exit_marker.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => None,
        };
        let Some(path) = path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&path, b"exit") {
            tracing::warn!("写退出信号文件失败 {}：{err}", path.display());
        }
    }

    /// 请求关闭：通知服务停止，并唤醒托盘消息循环。
    ///
    /// 幂等：重复调用只生效一次（避免重复投递退出消息）。
    pub fn request(&self) {
        if self.requested.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.server_tx.send(true);
        self.wake_tray();
    }

    /// 是否已经请求关闭。
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// HTTP 服务停止信号（交给 `axum::serve` 的 graceful shutdown 等待）。
    pub fn server_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.server_rx.clone()
    }

    /// 标记 HTTP 服务已经停止（端口已释放）。
    pub fn mark_server_stopped(&self) {
        self.server_stopped.store(true, Ordering::SeqCst);
    }

    /// HTTP 服务是否已经停止。
    pub fn server_stopped(&self) -> bool {
        self.server_stopped.load(Ordering::SeqCst)
    }

    /// 登记托盘线程 id（托盘线程启动时调用）。
    ///
    /// 若关闭请求已经先一步到达，这里立刻补投一条退出消息，
    /// 避免「请求早于登记」导致消息丢失、进程退不出去。
    pub fn set_tray_thread(&self, thread_id: u32) {
        self.tray_thread.store(thread_id, Ordering::SeqCst);
        if self.is_requested() {
            self.wake_tray();
        }
    }

    /// 向托盘线程投递退出消息。
    fn wake_tray(&self) {
        let thread_id = self.tray_thread.load(Ordering::SeqCst);
        if thread_id == 0 {
            return;
        }
        post_shutdown_message(thread_id);
    }
}

/// 向指定线程投递「请退出消息循环」。
#[cfg(windows)]
fn post_shutdown_message(thread_id: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
    // SAFETY：PostThreadMessageW 只投递消息，不解引用任何指针；
    // 目标线程的消息队列由托盘线程自己创建（GetMessageW 会创建）。
    unsafe {
        PostThreadMessageW(thread_id, WM_APP_SHUTDOWN, 0, 0);
    }
}

#[cfg(not(windows))]
fn post_shutdown_message(_thread_id: u32) {}

/// 一次「安装并重启」的完整计划。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    /// 安装包路径（已下载并通过 SHA256 校验的那个文件）。
    pub installer: PathBuf,
    /// 安装目标目录：必须是**原安装目录**，升级才装回原处。
    pub install_dir: PathBuf,
    /// 安装完成后要拉起的新 exe。
    pub new_exe: PathBuf,
    /// 安装日志路径（便于事后排查）。
    pub log_file: PathBuf,
    /// 本进程 pid：看护脚本要等它退出后再动手。
    pub parent_pid: u32,
    /// 「本进程即将退出」的信号文件。
    ///
    /// 看护脚本等这个文件出现，而不是干等 PID 消失：PID 可能被系统回收再分配
    /// 给别的进程，等错对象就会白等好几分钟。
    pub exit_marker: PathBuf,
}

/// 静默安装的命令行参数。
///
/// 关键点：
///
/// - **不带** `/CLOSEAPPLICATIONS`：静默模式下 Inno 会主动调 Restart Manager
///   去关占用文件的进程，`WM_CLOSE` 没人处理就会一直等（卡死）。
/// - **带** `/NOCLOSEAPPLICATIONS`：明确告诉 Inno 不要走 Restart Manager，
///   关闭由我们自己按顺序做完（先停服务释放端口，再退出释放 exe 句柄）。
/// - `/VERYSILENT`：不弹任何窗口；`/SUPPRESSMSGBOXES` 抑制剩余弹窗。
/// - `/DIR=`：显式指定安装目录，保证「装回原处」而不是走默认路径。
pub fn installer_args(plan: &InstallPlan) -> String {
    format!(
        "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCLOSEAPPLICATIONS /NORESTARTAPPLICATIONS \
         /LOG=\"{}\" /DIR=\"{}\"",
        plan.log_file.display(),
        plan.install_dir.display()
    )
}

/// 生成看护脚本内容（纯函数，便于单测）。
///
/// 脚本做三件事：
/// 1. 等本进程退出（exe 句柄释放后安装器才能覆盖文件）；
/// 2. 静默安装到 `install_dir`；
/// 3. 安装成功后拉起新版本。
///
/// **脚本正文一律用 ASCII**：Windows PowerShell 5.1 读取无 BOM 的 .ps1 时按
/// 系统 ANSI 代码页解码，UTF-8 中文注释会被误解码，甚至把换行吞掉导致语法错误。
pub fn build_watch_script(plan: &InstallPlan) -> String {
    // 进程名（不含 .exe）：安装成功后用它确认新版本真的起来了。
    let proc_name = plan
        .new_exe
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Model-Gate");
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$installer = '{installer}'
$argLine = '{args}'
$newExe = '{new_exe}'
$log = '{log}'
$marker = '{marker}'
"watch-started $(Get-Date -Format o)" | Out-File -FilePath $log -Encoding UTF8 -Append
# 1) wait until this app signals that it is about to exit
$deadline = (Get-Date).AddSeconds(300)
while ((Get-Date) -lt $deadline) {{
    if (Test-Path $marker) {{ break }}
    if (-not (Get-Process -Id {pid} -ErrorAction SilentlyContinue)) {{ break }}
    Start-Sleep -Milliseconds 200
}}
# the exe handle is released a moment after the signal; let it go
Start-Sleep -Milliseconds 800
# 2) silent install; /NOCLOSEAPPLICATIONS keeps Restart Manager out of the way
$p = Start-Process -FilePath $installer -ArgumentList $argLine -PassThru -Wait
# 3) relaunch the new version
if ($p -and $p.ExitCode -eq 0) {{
    Start-Process -FilePath $newExe
    # installer exit code alone is not enough: confirm the app is really running
    Start-Sleep -Seconds 2
    if (Get-Process -Name '{proc_name}' -ErrorAction SilentlyContinue) {{
        "install-ok exit=0 relaunch=ok" | Out-File -FilePath $log -Encoding UTF8 -Append
    }} else {{
        "install-ok exit=0 relaunch=failed" | Out-File -FilePath $log -Encoding UTF8 -Append
    }}
}} else {{
    $code = if ($p) {{ $p.ExitCode }} else {{ 'unknown' }}
    "install-failed exit=$code" | Out-File -FilePath $log -Encoding UTF8 -Append
}}
Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
"#,
        installer = ps_escape(&plan.installer.display().to_string()),
        args = ps_escape(&installer_args(plan)),
        new_exe = ps_escape(&plan.new_exe.display().to_string()),
        log = ps_escape(&plan.log_file.display().to_string()),
        marker = ps_escape(&plan.exit_marker.display().to_string()),
        pid = plan.parent_pid,
        proc_name = ps_escape(proc_name),
    )
}

/// PowerShell 单引号字符串内部需要把单引号写成两个。
///
/// 调用方负责在值两侧补上单引号（这里只做转义），否则会出现 `''path''`
/// 这种 PowerShell 解析不了的写法。
fn ps_escape(value: &str) -> String {
    value.replace('\'', "''")
}

/// 看护脚本的落盘位置：临时目录下的 `Model-Gate/install-<pid>.ps1`。
pub fn watch_script_path(parent_pid: u32) -> PathBuf {
    std::env::temp_dir()
        .join("Model-Gate")
        .join(format!("install-{parent_pid}.ps1"))
}

/// 「本进程即将退出」信号文件的落盘位置：临时目录下的 `Model-Gate/exit-<pid>.flag`。
pub fn exit_marker_path(parent_pid: u32) -> PathBuf {
    std::env::temp_dir()
        .join("Model-Gate")
        .join(format!("exit-{parent_pid}.flag"))
}

/// 写看护脚本：UTF-8 **带 BOM**。
///
/// Windows PowerShell 5.1 读取无 BOM 的 .ps1 时按系统 ANSI 代码页解码，
/// 脚本里的中文注释会被误解码，甚至把换行吞掉造成语法错误。
/// 带 BOM 时它才会按 UTF-8 解析。
fn write_script_with_bom(path: &Path, content: &str) -> std::io::Result<()> {
    const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
    let mut bytes = Vec::with_capacity(BOM.len() + content.len());
    bytes.extend_from_slice(&BOM);
    bytes.extend_from_slice(content.as_bytes());
    std::fs::write(path, bytes)
}

/// 启动看护脚本：脱离当前进程独立运行，等本进程退出后接管安装。
///
/// 返回错误时调用方应回退到「不自动重启」的降级路径。
pub fn spawn_watch_script(plan: &InstallPlan) -> Result<PathBuf, String> {
    let script_path = watch_script_path(plan.parent_pid);
    if let Some(parent) = script_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("创建看护脚本目录失败 {}：{err}", parent.display()))?;
    }
    let script = build_watch_script(plan);
    write_script_with_bom(&script_path, &script)
        .map_err(|err| format!("写看护脚本失败 {}：{err}", script_path.display()))?;

    let spawned = spawn_powershell(&script_path);
    if spawned.is_ok() {
        return Ok(script_path);
    }

    // PowerShell 不可用（极少见）：退回 cmd 延时链，功能降级但不中断安装。
    tracing::warn!("PowerShell 不可用，改用 cmd 延时链执行安装");
    spawn_cmd_fallback(plan)
        .map_err(|err| format!("PowerShell 与 cmd 两种方式都无法启动安装器：{err}"))?;
    Ok(script_path)
}

/// 用 PowerShell 启动看护脚本（无窗口、脱离当前进程）。
///
/// 关键点：
/// - `CREATE_NO_WINDOW`：本应用是 GUI 子系统程序，没有控制台；用这个标志
///   启动控制台子程序，窗口不会出现，子进程也不会因为「没有控制台可依附」
///   而启动失败。
/// - stdout/stderr 落到文件：看护脚本万一报错，事后能看到原因。
#[cfg(windows)]
fn spawn_powershell(script_path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：子进程不弹控制台窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // CREATE_NEW_PROCESS_GROUP：独立进程组，避免被 Ctrl+C 之类打断。
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let log_path = script_path.with_extension("out");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| format!("打开看护脚本输出文件失败 {}：{err}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .map_err(|err| format!("复制看护脚本输出文件失败：{err}"))?;

    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script_path)
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("启动看护脚本失败：{err}"))
}

#[cfg(not(windows))]
fn spawn_powershell(_script_path: &Path) -> Result<(), String> {
    Err("非 Windows 平台不支持自动安装".to_string())
}

/// 兜底方案：`cmd` 延时后执行安装器并拉起新版本。
///
/// 与看护脚本的差别：靠固定延时等待本进程退出，不够精确；
/// 只在 PowerShell 不可用时才会走到。
fn spawn_cmd_fallback(plan: &InstallPlan) -> Result<(), String> {
    let wait_secs = 8;
    let cmd_line = format!(
        "ping -n {} 127.0.0.1 >nul & \"{}\" {} & start \"\" \"{}\"",
        wait_secs + 1,
        plan.installer.display(),
        installer_args(plan),
        plan.new_exe.display(),
    );
    std::process::Command::new("cmd")
        .args(["/C", &cmd_line])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("启动 cmd 安装链失败：{err}"))
}

/// 一次「退出并由看护脚本拉起自己（换参数）」的计划：托盘模式切换用。
///
/// 与 [`InstallPlan`] 的差别：没有安装器，纯「等本进程退出 → 带新参数
/// 重新启动同一个 exe」。参数由调用方按目标模式组装好（见
/// [`build_relaunch_args`]），这里只负责等待与拉起。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaunchPlan {
    /// 要重新启动的 exe（= 当前进程的 exe）。
    pub exe: PathBuf,
    /// 目标参数行：已按 Windows 规则引号化（见 [`join_relaunch_args`]）。
    pub arg_line: String,
    /// 本进程 pid：看护脚本等它退出。
    pub parent_pid: u32,
    /// 「本进程即将退出」信号文件：与安装流程共用同一套 marker。
    pub exit_marker: PathBuf,
    /// 脚本状态日志（relaunch=ok / relaunch=failed）。
    pub log_file: PathBuf,
}

/// 组装「切换模式重启」的目标参数：保留用户原有参数，剥掉旧的
/// `--minimal`（两种写法都认），再按目标模式决定是否补上。
pub fn build_relaunch_args(original: &[String], minimal: bool) -> Vec<String> {
    let mut out: Vec<String> = original
        .iter()
        .filter(|arg| !arg.starts_with("--minimal"))
        .cloned()
        .collect();
    if minimal {
        out.push("--minimal".to_string());
    }
    out
}

/// 把参数数组拼成可交给 `Start-Process -ArgumentList` 的单行字符串：
/// 含空格/制表符的参数用双引号包住（路径带空格是常态）。
pub fn join_relaunch_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty() || arg.contains([' ', '\t']) {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 生成重启看护脚本（纯函数，便于单测）。正文一律 ASCII，理由同
/// [`build_watch_script`]：PowerShell 5.1 对无 BOM 脚本按 ANSI 解码。
pub fn build_relaunch_script(plan: &RelaunchPlan) -> String {
    let proc_name = plan
        .exe
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Model-Gate");
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$newExe = '{exe}'
$argLine = '{arg_line}'
$marker = '{marker}'
$log = '{log}'
"relaunch-started $(Get-Date -Format o)" | Out-File -FilePath $log -Encoding UTF8 -Append
# 1) wait until the old process exits (marker first, pid as fallback)
$deadline = (Get-Date).AddSeconds(60)
while ((Get-Date) -lt $deadline) {{
    if (Test-Path $marker) {{ break }}
    if (-not (Get-Process -Id {pid} -ErrorAction SilentlyContinue)) {{ break }}
    Start-Sleep -Milliseconds 200
}}
# the old exe handle is released a moment after exit; let it go
Start-Sleep -Milliseconds 800
# 2) relaunch the same exe with the new mode args
if ($argLine -ne '') {{
    Start-Process -FilePath $newExe -ArgumentList $argLine
}} else {{
    Start-Process -FilePath $newExe
}}
# 3) confirm the app is really running (exit code alone is not enough)
Start-Sleep -Seconds 2
if (Get-Process -Name '{proc_name}' -ErrorAction SilentlyContinue) {{
    "relaunch=ok" | Out-File -FilePath $log -Encoding UTF8 -Append
}} else {{
    "relaunch=failed" | Out-File -FilePath $log -Encoding UTF8 -Append
}}
Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
"#,
        exe = ps_escape(&plan.exe.display().to_string()),
        arg_line = ps_escape(&plan.arg_line),
        marker = ps_escape(&plan.exit_marker.display().to_string()),
        log = ps_escape(&plan.log_file.display().to_string()),
        pid = plan.parent_pid,
        proc_name = ps_escape(proc_name),
    )
}

/// 重启看护脚本的落盘位置：临时目录下的 `Model-Gate/relaunch-<pid>.ps1`。
pub fn relaunch_script_path(parent_pid: u32) -> PathBuf {
    std::env::temp_dir()
        .join("Model-Gate")
        .join(format!("relaunch-{parent_pid}.ps1"))
}

/// 重启看护脚本的状态日志：`Model-Gate/relaunch-<pid>.log`。
pub fn relaunch_log_path(parent_pid: u32) -> PathBuf {
    std::env::temp_dir()
        .join("Model-Gate")
        .join(format!("relaunch-{parent_pid}.log"))
}

/// 写并启动重启看护脚本（复用安装流程的 BOM 写盘与无窗口 PowerShell）。
///
/// 与安装流程不同：PowerShell 不可用时**不做** cmd 降级——降级链容易把
/// 自定义参数（--config/--db）丢掉，重启出一个配置错位的实例比不重启更糟；
/// 此时返回错误，托盘记日志，应用继续以当前模式运行。
pub fn spawn_relaunch_script(plan: &RelaunchPlan) -> Result<PathBuf, String> {
    let script_path = relaunch_script_path(plan.parent_pid);
    if let Some(parent) = script_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("创建重启脚本目录失败 {}：{err}", parent.display()))?;
    }
    let script = build_relaunch_script(plan);
    write_script_with_bom(&script_path, &script)
        .map_err(|err| format!("写重启脚本失败 {}：{err}", script_path.display()))?;
    spawn_powershell(&script_path)?;
    Ok(script_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> InstallPlan {
        InstallPlan {
            installer: PathBuf::from(r"C:\temp\Downloads\Model-Gate-v2.24.0-setup.exe"),
            install_dir: PathBuf::from(r"D:\mysoft\Model-Gate"),
            new_exe: PathBuf::from(r"D:\mysoft\Model-Gate\Model-Gate.exe"),
            log_file: PathBuf::from(r"C:\temp\Downloads\install.log"),
            parent_pid: 4242,
            exit_marker: PathBuf::from(r"C:\temp\Model-Gate\exit-4242.flag"),
        }
    }

    #[test]
    fn installer_args_disable_close_applications() {
        let args = installer_args(&sample_plan());
        // 卡死的根因就是 /CLOSEAPPLICATIONS：必须没有它，且显式 /NOCLOSEAPPLICATIONS。
        assert!(!args.contains("/CLOSEAPPLICATIONS "), "{args}");
        assert!(args.contains("/NOCLOSEAPPLICATIONS"), "{args}");
        assert!(args.contains("/VERYSILENT"), "{args}");
        assert!(args.contains("/SUPPRESSMSGBOXES"), "{args}");
        assert!(args.contains("/NORESTART"), "{args}");
    }

    #[test]
    fn installer_args_pin_install_dir_and_log() {
        let args = installer_args(&sample_plan());
        assert!(args.contains(r#"/DIR="D:\mysoft\Model-Gate""#), "{args}");
        assert!(
            args.contains(r#"/LOG="C:\temp\Downloads\install.log""#),
            "{args}"
        );
    }

    #[test]
    fn watch_script_waits_for_parent_then_installs_and_relaunches() {
        let script = build_watch_script(&sample_plan());
        assert!(script.contains("Get-Process -Id 4242"), "{script}");
        assert!(
            script.contains("Start-Process -FilePath $installer"),
            "{script}"
        );
        assert!(
            script.contains("Start-Process -FilePath $newExe"),
            "{script}"
        );
        assert!(script.contains("/NOCLOSEAPPLICATIONS"), "{script}");
        // 顺序必须「先等退出、再安装、最后拉起」。
        let wait = script.find("Get-Process -Id").expect("缺少等待步骤");
        let install = script
            .find("Start-Process -FilePath $installer")
            .expect("缺少安装步骤");
        let relaunch = script
            .find("Start-Process -FilePath $newExe")
            .expect("缺少拉起步骤");
        assert!(wait < install, "{script}");
        assert!(install < relaunch, "{script}");
    }

    #[test]
    fn watch_script_escapes_single_quotes() {
        let mut plan = sample_plan();
        plan.installer = PathBuf::from(r"C:\temp\o'brien\Downloads\setup.exe");
        let script = build_watch_script(&plan);
        assert!(script.contains("o''brien"), "{script}");
    }

    #[test]
    fn watch_script_has_no_doubled_quotes() {
        // 回归：`''path''` 是 PowerShell 解析不了的写法（引号被加了两遍）。
        let script = build_watch_script(&sample_plan());
        assert!(!script.contains("''"), "{script}");
        assert!(
            script.contains("$installer = 'C:\\temp\\Downloads\\"),
            "{script}"
        );
    }

    #[test]
    fn watch_script_is_ascii_only() {
        // 回归：脚本里混入非 ASCII 时，PowerShell 5.1 按 ANSI 解码会吞掉换行。
        let script = build_watch_script(&sample_plan());
        assert!(script.is_ascii(), "看护脚本必须只含 ASCII 字符");
    }

    #[test]
    fn watch_script_file_starts_with_utf8_bom() {
        // 回归：无 BOM 的 .ps1 会被 PowerShell 5.1 按 ANSI 解码。
        let dir = std::env::temp_dir().join("model-gate-bom-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.ps1");
        write_script_with_bom(&path, "Write-Output 'ok'\n").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watch_script_reports_relaunch_result_and_cleans_marker() {
        // 安装失败时应用已经退出，用户看不到任何界面；日志是唯一线索。
        let script = build_watch_script(&sample_plan());
        assert!(script.contains("Get-Process -Name 'Model-Gate'"), "{script}");
        assert!(script.contains("relaunch=ok"), "{script}");
        assert!(script.contains("relaunch=failed"), "{script}");
        assert!(
            script.contains("Remove-Item -LiteralPath $marker"),
            "{script}"
        );
        // 清理必须在安装之后，否则脚本可能在安装前就把信号删掉。
        let install = script
            .find("Start-Process -FilePath $installer")
            .expect("缺少安装步骤");
        let cleanup = script
            .find("Remove-Item -LiteralPath $marker")
            .expect("缺少清理步骤");
        assert!(install < cleanup, "{script}");
    }

    #[test]
    fn watch_script_waits_for_exit_marker_before_installing() {
        // 回归：看护脚本必须先等「退出信号」，再动安装器。
        let script = build_watch_script(&sample_plan());
        assert!(script.contains("Test-Path $marker"), "{script}");
        assert!(script.contains("exit-4242.flag"), "{script}");
        let wait_at = script.find("Test-Path $marker").expect("缺少等待步骤");
        let install_at = script
            .find("Start-Process -FilePath $installer")
            .expect("缺少安装步骤");
        assert!(wait_at < install_at, "{script}");
    }

    #[test]
    fn shutdown_is_idempotent() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_requested());
        shutdown.request();
        assert!(shutdown.is_requested());
        shutdown.request();
        assert!(shutdown.is_requested());
    }

    #[test]
    fn server_stopped_flag_round_trips() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.server_stopped());
        shutdown.mark_server_stopped();
        assert!(shutdown.server_stopped());
    }

    #[tokio::test]
    async fn server_signal_fires_after_request() {
        let shutdown = Shutdown::new();
        let mut signal = shutdown.server_signal();
        assert!(!*signal.borrow());
        shutdown.request();
        // watch 通道会记住最新值，等待立即返回。
        let _ = signal.changed().await;
        assert!(*signal.borrow());
    }

    #[test]
    fn watch_script_path_is_per_process() {
        let path = watch_script_path(1234);
        assert!(
            path.to_string_lossy().contains("install-1234.ps1"),
            "{path:?}"
        );
    }

    fn sample_relaunch_plan() -> RelaunchPlan {
        RelaunchPlan {
            exe: PathBuf::from(r"D:\mysoft\Model-Gate\Model-Gate.exe"),
            arg_line: "--config \"D:\\my dir\\bridge.config.json\" --minimal".to_string(),
            parent_pid: 4242,
            exit_marker: PathBuf::from(r"C:\temp\Model-Gate\exit-4242.flag"),
            log_file: PathBuf::from(r"C:\temp\Model-Gate\relaunch-4242.log"),
        }
    }

    #[test]
    fn relaunch_args_strip_old_minimal_and_apply_target_mode() {
        let original: Vec<String> = ["--port", "18990", "--minimal"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // 目标精简：剥旧 --minimal 后补回一个，不重复。
        assert_eq!(
            build_relaunch_args(&original, true),
            vec!["--port", "18990", "--minimal"]
        );
        // 目标完整：--minimal 被剥掉。
        assert_eq!(
            build_relaunch_args(&original, false),
            vec!["--port", "18990"]
        );
        // clap 的 = 写法同样剥掉。
        let with_eq: Vec<String> = ["--minimal=true", "--no-gui"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(build_relaunch_args(&with_eq, false), vec!["--no-gui"]);
    }

    #[test]
    fn relaunch_args_quote_args_with_spaces() {
        let args: Vec<String> = ["--config", "D:\\my dir\\c.json", "--no-gui"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            join_relaunch_args(&args),
            "--config \"D:\\my dir\\c.json\" --no-gui"
        );
    }

    #[test]
    fn relaunch_script_waits_exit_then_relaunches_with_args() {
        let script = build_relaunch_script(&sample_relaunch_plan());
        // 等待（marker 优先、pid 兜底）→ 拉起 → 确认，顺序不可颠倒。
        let wait = script.find("Test-Path $marker").expect("缺少等待步骤");
        let pid_wait = script.find("Get-Process -Id 4242").expect("缺少 pid 兜底");
        let relaunch = script
            .find("Start-Process -FilePath $newExe")
            .expect("缺少拉起步骤");
        let confirm = script
            .find("Get-Process -Name 'Model-Gate'")
            .expect("缺少确认步骤");
        assert!(wait < relaunch, "{script}");
        assert!(pid_wait < relaunch, "{script}");
        assert!(relaunch < confirm, "{script}");
        // 带参数拉起走 ArgumentList。
        assert!(script.contains("-ArgumentList $argLine"), "{script}");
        assert!(script.contains("relaunch=ok"), "{script}");
        assert!(script.contains("relaunch=failed"), "{script}");
    }

    #[test]
    fn relaunch_script_is_ascii_only_and_escapes_quotes() {
        let mut plan = sample_relaunch_plan();
        plan.exe = PathBuf::from(r"D:\my 'soft\Model-Gate.exe");
        let script = build_relaunch_script(&plan);
        assert!(script.is_ascii(), "重启脚本必须只含 ASCII 字符");
        assert!(script.contains("my ''soft"), "{script}");
        assert!(!script.contains("''''"), "{script}");
    }

    #[test]
    fn relaunch_script_path_is_per_process() {
        let path = relaunch_script_path(5678);
        assert!(
            path.to_string_lossy().contains("relaunch-5678.ps1"),
            "{path:?}"
        );
    }

    /// 端到端重启冒烟辅助（人工触发）：按**运行时指定的 pid/参数**把重启脚本
    /// 写到 `%TEMP%\Model-Gate\smoke-relaunch.ps1`，供外部冒烟驱动真实进程。
    ///
    /// 用法：
    /// `RELAUNCH_SMOKE_PID=<pid> RELAUNCH_SMOKE_EXE=<exe路径> \
    ///  RELAUNCH_SMOKE_ARGS=<参数行> cargo test dump_relaunch_script_for_smoke \
    ///  -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_relaunch_script_for_smoke() {
        let pid: u32 = std::env::var("RELAUNCH_SMOKE_PID")
            .expect("需要 RELAUNCH_SMOKE_PID")
            .parse()
            .expect("RELAUNCH_SMOKE_PID 必须是数字");
        let plan = RelaunchPlan {
            exe: std::env::var("RELAUNCH_SMOKE_EXE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("Model-Gate.exe")),
            arg_line: std::env::var("RELAUNCH_SMOKE_ARGS").unwrap_or_default(),
            parent_pid: pid,
            exit_marker: exit_marker_path(pid),
            log_file: relaunch_log_path(pid),
        };
        let dir = std::env::temp_dir().join("Model-Gate");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("smoke-relaunch.ps1");
        write_script_with_bom(&path, &build_relaunch_script(&plan)).unwrap();
        println!("重启脚本已生成：{}", path.display());
    }
}
