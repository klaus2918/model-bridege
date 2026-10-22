# 端到端实测：应用内「下载并安装」全链路。
#
# 覆盖用户诉求的四点：
#   1. 下载默认落到 Windows「下载」目录；
#   2. 「安装并重启」真的能把新版本装上；
#   3. 安装前正确关闭原有服务（不卡死、不残留端口占用）；
#   4. 装回原安装目录，而不是每次人工选择。
#
# 手法：本地构建一个「低版本」应用（默认 2.22.0，含本次全部改动）装到隔离目录，
# 让它从 GitHub 发现真实发布的更高版本，走完整 下载→校验→关闭→安装→重启。
# 这样既能测到本次改动，也测到真实发行包的安装行为。
#
# 重要：测试用的是与正式版相同的 AppId，会改写 HKCU 卸载项键。
# 因此「prepare」阶段先备份并删除正式卸载项键，「cleanup」阶段还原。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File scripts\update_install_e2e.ps1 -Phase build,prepare,install-old,start,verify-paths,update,install-update,cleanup
#   powershell -ExecutionPolicy Bypass -File scripts\update_install_e2e.ps1 -Phase cleanup
param(
    [string]$Phase = "all",
    [string]$InstallDir = "D:\mg-e2e\install",
    [int]$Port = 8931,
    [string]$OldVersion = "2.22.0",
    [string]$Work = "",
    [int]$ExitTimeoutSec = 90,
    [int]$InstallTimeoutSec = 300
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$cargoDir = Join-Path $root "rust\model-gate"
$distDir = Join-Path $root "dist"
if (-not $Work) { $Work = Join-Path $env:TEMP "mg-e2e" }
$iscc = "D:\webSoft\InnoSetup6\ISCC.exe"
$appId = "{8F3C4A21-6D9E-4B7A-9C15-2B0E7D5F4A63}"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId`_is1"
$regBackup = Join-Path $Work "uninstall-backup.reg"
$setupOld = Join-Path $distDir "Model-Gate-v$OldVersion-e2e-setup.exe"
$base = "http://127.0.0.1:$Port"

function Say([string]$text) { Write-Host ("[e2e] " + $text) }
function Fail([string]$text) { throw ("[e2e][FAIL] " + $text) }

# 读 JSON 接口。
#
# 为什么不用 Invoke-RestMethod：PowerShell 5.1 解析含非 ASCII（Release 说明是中文）
# 的 JSON 时会把响应按 Latin-1 解码，个别情况下返回的不是对象而是字符串，
# 于是 .phase 取到空值。Invoke-WebRequest + ConvertFrom-Json 没有这个问题。
function Get-Json([string]$url, [int]$timeout = 30) {
    $resp = Invoke-WebRequest $url -UseBasicParsing -TimeoutSec $timeout
    return ([string]$resp.Content | ConvertFrom-Json)
}

# 解析要跑哪些阶段。
$allPhases = @("build", "prepare", "install-old", "start", "verify-paths", "update", "install-update", "cleanup")
$phases = if ($Phase -eq "all") { $allPhases } else { $Phase.Split(",") | ForEach-Object { $_.Trim() } }
function Want([string]$name) { return $phases -contains $name }

New-Item -ItemType Directory -Path $Work -Force | Out-Null

# ── build ────────────────────────────────────────────────────────────────
# 把版本临时降一档，让应用能从 GitHub 发现真实发布的更高版本。
if (Want "build") {
    Say "build: 临时把版本降到 $OldVersion 并构建"
    $cargoToml = Join-Path $cargoDir "Cargo.toml"
    $cargoLock = Join-Path $cargoDir "Cargo.lock"
    $original = [System.IO.File]::ReadAllText($cargoToml)
    $originalLock = $null
    if (Test-Path $cargoLock) { $originalLock = [System.IO.File]::ReadAllText($cargoLock) }
    try {
        $patched = [regex]::Replace($original, '(?m)^(version\s*=\s*)"[^"]+"', ('${1}"' + $OldVersion + '"'), 1)
        if ($patched -eq $original) { Fail "Cargo.toml 里没找到 version 字段" }
        [System.IO.File]::WriteAllText($cargoToml, $patched, (New-Object System.Text.UTF8Encoding($false)))
        Push-Location $cargoDir
        # 原生命令的 stderr 在 PowerShell 里会被当成错误记录：这里放宽策略，改看退出码。
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        & cargo build 2>&1 | Select-Object -Last 5 | ForEach-Object { Write-Host ("    " + $_) }
        $buildExit = $LASTEXITCODE
        $ErrorActionPreference = $prevEap
        Pop-Location
        if ($buildExit -ne 0) { Fail "cargo build 失败 exit=$buildExit" }
    } finally {
        # 无论成败都还原版本号，避免污染仓库。
        [System.IO.File]::WriteAllText($cargoToml, $original, (New-Object System.Text.UTF8Encoding($false)))
        # cargo 会把版本号写进 Cargo.lock，一并还原。
        if ($originalLock) {
            [System.IO.File]::WriteAllText($cargoLock, $originalLock, (New-Object System.Text.UTF8Encoding($false)))
        }
        Say "build: Cargo.toml / Cargo.lock 版本已还原"
    }
    $debugExe = Join-Path $cargoDir "target\debug\model-gate.exe"
    if (-not (Test-Path $debugExe)) { Fail "找不到构建产物 $debugExe" }
    Say "build: 编译安装包 $setupOld"
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $iscc "/DMyAppVersion=$OldVersion" "/DMyAppSourceExe=$debugExe" `
        "/DMyOutputBaseFilename=Model-Gate-v$OldVersion-e2e-setup" (Join-Path $root "ModelGate.iss") |
        Select-Object -Last 6 | ForEach-Object { Write-Host ("    " + $_) }
    $isccExit = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    if ($isccExit -ne 0) { Fail "ISCC 失败 exit=$isccExit" }
    if (-not (Test-Path $setupOld)) { Fail "安装包未生成：$setupOld" }
    Say ("build: 完成 -> " + $setupOld)
}

# ── prepare ──────────────────────────────────────────────────────────────
# 备份并删除正式卸载项键：测试会用同一个 AppId 改写它。
if (Want "prepare") {
    Say "prepare: 备份并删除正式卸载项键"
    if (Test-Path $uninstallKey) {
        $regPath = "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\${appId}_is1"
        & reg.exe export $regPath $regBackup /y | Out-Null
        if (-not (Test-Path $regBackup)) { Fail "卸载项键备份失败" }
        Remove-Item -LiteralPath $uninstallKey -Recurse -Force
        Say "prepare: 已备份到 $regBackup 并删除原键"
    } else {
        Say "prepare: 正式卸载项键不存在，跳过"
    }
}

# ── install-old ──────────────────────────────────────────────────────────
if (Want "install-old") {
    Say "install-old: 静默安装旧版本到 $InstallDir"
    # 先停掉上一轮遗留的进程：它会占着 data\modelgate.db。
    Get-Process -Name "Model-Gate" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.Id -Force }
    Start-Sleep -Milliseconds 500
    if (Test-Path $InstallDir) { Remove-Item -LiteralPath $InstallDir -Recurse -Force }
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    & $setupOld /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- "/DIR=$InstallDir" | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "旧版本安装失败 exit=$LASTEXITCODE" }
    $exe = Join-Path $InstallDir "Model-Gate.exe"
    if (-not (Test-Path $exe)) { Fail "旧版本 exe 不存在：$exe" }
    if (-not (Test-Path (Join-Path $InstallDir "unins000.exe"))) { Fail "旧版本缺少 unins000.exe（不是安装版）" }
    # 固定端口：重启后的新实例也要用同一个端口（它不带命令行参数）。
    $cfg = Join-Path $InstallDir "bridge.config.json"
    if (Test-Path $cfg) {
        $text = [System.IO.File]::ReadAllText($cfg)
        $text = [regex]::Replace($text, '("port"\s*:\s*)\d+', ('${1}' + $Port), 1)
        [System.IO.File]::WriteAllText($cfg, $text, (New-Object System.Text.UTF8Encoding($false)))
    }
    Say ("install-old: 完成，端口固定为 " + $Port)
}

# ── start ────────────────────────────────────────────────────────────────
if (Want "start") {
    Say "start: 启动隔离目录里的应用"
    Get-Process -Name "Model-Gate" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.Id -Force }
    $exe = Join-Path $InstallDir "Model-Gate.exe"
    $proc = Start-Process -FilePath $exe -WorkingDirectory $InstallDir -PassThru
    Say ("start: pid=" + $proc.Id)
    $ready = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 500
        try {
            $info = Invoke-RestMethod "$base/api/info" -TimeoutSec 2
            if ($info.app -eq "Model-Gate") {
                Say ("start: 就绪 version=" + $info.version)
                $ready = $true
                break
            }
        } catch { }
    }
    if (-not $ready) { Fail "应用未在 30 秒内就绪" }
}

# ── verify-paths ─────────────────────────────────────────────────────────
if (Want "verify-paths") {
    Say "verify-paths: 校验下载目录与安装目录"
    $status = Get-Json "$base/api/update/status"
    Say ("  download_dir        = " + $status.download_dir)
    Say ("  download_dir_custom = " + $status.download_dir_custom)
    Say ("  effective_install   = " + $status.effective_install_dir)
    Say ("  install_dir_source  = " + $status.install_dir_source)
    Say ("  registered_install  = " + $status.registered_install_dir)
    Say ("  install_supported   = " + $status.install_supported)

    $downloads = [Environment]::GetFolderPath("UserProfile")
    $known = (New-Object -ComObject Shell.Application).Namespace("shell:Downloads")
    if ($known) { $downloads = $known.Self.Path }
    if ($status.download_dir -ne $downloads) {
        Fail ("下载目录不是 Windows「下载」目录：期望 " + $downloads + "，实际 " + $status.download_dir)
    }
    if ($status.install_dir_source -ne "registry") { Fail "安装目录来源不是注册表" }
    if ($status.effective_install_dir.TrimEnd('\') -ne $InstallDir.TrimEnd('\')) {
        Fail ("生效安装目录不是原安装目录：期望 " + $InstallDir + "，实际 " + $status.effective_install_dir)
    }
    if (-not $status.install_supported) { Fail "install_supported 为 false（目录里没有卸载器）" }
    Say "verify-paths: 全部符合预期"
}

# ── update ───────────────────────────────────────────────────────────────
if (Want "update") {
    Say "update: 检查更新并下载"
    $status = Get-Json "$base/api/update/check?force=1" 90
    Say ("  phase=" + $status.phase + " latest=" + $status.latest + " available=" + $status.available)
    if (-not $status.available) { Fail "没有检测到可更新版本" }
    $resp = Invoke-WebRequest "$base/api/update/download" -Method Post -ContentType "application/json" `
        -Body "{}" -UseBasicParsing -TimeoutSec 90
    $status = ([string]$resp.Content | ConvertFrom-Json)
    Say ("  下载已启动 phase=" + $status.phase)
    # 网络慢时 4MB 安装包可能要一两分钟，这里给足时间。
    for ($i = 0; $i -lt 360; $i++) {
        Start-Sleep -Milliseconds 500
        $status = Get-Json "$base/api/update/status"
        if ($status.phase -eq "ready") { break }
        if ($status.phase -eq "error") { Fail ("下载失败：" + $status.error.message) }
    }
    if ($status.phase -ne "ready") { Fail ("下载未在 180 秒内完成（phase=" + $status.phase + "）") }
    Say ("  installer_path = " + $status.installer_path)
    if (-not (Test-Path $status.installer_path)) { Fail "安装包文件不存在" }
    $downloads = (New-Object -ComObject Shell.Application).Namespace("shell:Downloads").Self.Path
    if (-not $status.installer_path.StartsWith($downloads, [StringComparison]::OrdinalIgnoreCase)) {
        Fail ("安装包没有落在 Windows「下载」目录：" + $status.installer_path)
    }
    Say "update: 安装包已落在 Windows「下载」目录"
}

# ── install-update ───────────────────────────────────────────────────────
if (Want "install-update") {
    Say "install-update: 触发安装并重启"
    $before = Get-Process -Name "Model-Gate" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        Select-Object -First 1
    if (-not $before) { Fail "找不到待升级的应用进程" }
    $oldPid = $before.Id
    Say ("  旧进程 pid=" + $oldPid + " exe=" + $before.Path)

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $resp = Invoke-WebRequest "$base/api/update/install" -Method Post -ContentType "application/json" `
            -Body "{}" -UseBasicParsing -TimeoutSec 90
        $status = ([string]$resp.Content | ConvertFrom-Json)
        Say ("  接口返回 phase=" + $status.phase + " restart_pending=" + $status.restart_pending)
    } catch {
        # 进程可能在响应写回前就退出：连接被重置也算流程已启动。
        Say ("  安装接口返回异常（可能是进程已开始退出）：" + $_.Exception.Message)
    }

    $exited = $false
    while ($sw.Elapsed.TotalSeconds -lt $ExitTimeoutSec) {
        if (-not (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) { $exited = $true; break }
        Start-Sleep -Milliseconds 200
    }
    $exitSec = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    if (-not $exited) { Fail ("旧进程在 $ExitTimeoutSec 秒内没有退出（卡死）") }
    Say ("  旧进程已退出，耗时 " + $exitSec + " 秒")

    # 端口必须释放：新实例要能立刻绑定同一个端口。
    $released = $false
    $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, $Port)
    try { $listener.Start(); $released = $true } catch { $released = $false } finally { $listener.Stop() }
    if (-not $released) { Fail "旧进程退出后端口 $Port 仍被占用" }
    Say "  端口已释放"

    # 等新实例起来（看护脚本装完会拉起它）。
    $newInfo = $null
    for ($i = 0; $i -lt $InstallTimeoutSec; $i++) {
        Start-Sleep -Seconds 1
        try {
            $info = Get-Json "$base/api/info" 5
            if ($info.app -eq "Model-Gate") { $newInfo = $info; break }
        } catch { }
    }
    if (-not $newInfo) { Fail "新版本未在 $InstallTimeoutSec 秒内自动拉起" }
    Say ("  新实例已就绪 version=" + $newInfo.version)

    $newProc = Get-Process -Name "Model-Gate" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        Select-Object -First 1
    if (-not $newProc) { Fail "新实例不在原安装目录里运行" }
    Say ("  新实例 pid=" + $newProc.Id + " 路径=" + $newProc.Path)
    $newStatus = Get-Json "$base/api/update/status"
    Say ("  新实例 install_dir_source=" + $newStatus.install_dir_source + " effective_install=" + $newStatus.effective_install_dir)
    if ($newStatus.effective_install_dir.TrimEnd('\') -ne $InstallDir.TrimEnd('\')) {
        Fail "新实例的生效安装目录不对"
    }
    Say "install-update: 全部符合预期"
}

# ── cleanup ──────────────────────────────────────────────────────────────
if (Want "cleanup") {
    Say "cleanup: 停止应用、还原卸载项键、清理目录"
    Get-Process -Name "Model-Gate" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.Id -Force }
    if (Test-Path $uninstallKey) { Remove-Item -LiteralPath $uninstallKey -Recurse -Force }
    if (Test-Path $regBackup) {
        & reg.exe import $regBackup | Out-Null
        Say ("cleanup: 已还原正式卸载项键（来自 " + $regBackup + "）")
    }
    if (Test-Path $InstallDir) { Remove-Item -LiteralPath $InstallDir -Recurse -Force }
    if (Test-Path $setupOld) { Remove-Item -LiteralPath $setupOld -Force }
    Say "cleanup: 完成"
}

Say "所有请求的阶段已执行完毕"
