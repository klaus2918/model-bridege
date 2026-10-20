# 构建 Rust 内核并投放发布候选。
# 用法：
#   powershell -ExecutionPolicy Bypass -File rust\model-gate\build-release.ps1
#   powershell -ExecutionPolicy Bypass -File rust\model-gate\build-release.ps1 -Promote
# 参数：
#   -Promote     额外覆盖 dist\Model-Gate.exe（正式入口切换；Python 版请先备份）
#   -Acceptance  构建后运行黑盒验收 scripts\rust_shell_acceptance.ps1（指标 + 无 WebView2 + 二次启动）
param(
    [switch]$Promote,
    [switch]$Acceptance
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

$exe = Join-Path $PSScriptRoot "target\release\model-gate.exe"
$dist = Join-Path $PSScriptRoot "..\..\dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$dest = Join-Path $dist "Model-Gate-rust.exe"
Copy-Item $exe $dest -Force
$size = [math]::Round((Get-Item $dest).Length / 1MB, 2)
Write-Host "已生成 $dest ($size MB)"

if ($Promote) {
    $official = Join-Path $dist "Model-Gate.exe"
    $backup = Join-Path $dist "Model-Gate-python.exe"
    if ((Test-Path $official) -and -not (Test-Path $backup)) {
        Copy-Item $official $backup -Force
        Write-Host "已备份 Python 入口到 $backup"
    } elseif (Test-Path $backup) {
        Write-Host "保留既有回退入口 $backup（不覆盖）"
    }
    Copy-Item $exe $official -Force
    Write-Host "已切换正式入口：$official"
}

Write-Host "验证：$dest --no-gui --port 8911 --config <副本> --db <独立目录> --force-multi"

if ($Acceptance) {
    $root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
    $script = Join-Path $root "scripts\rust_shell_acceptance.ps1"
    if (-not (Test-Path $script)) { throw "黑盒验收脚本不存在：$script" }
    Write-Host "运行黑盒验收：$script"
    & powershell -NoProfile -ExecutionPolicy Bypass -File $script -Exe $dest
    if ($LASTEXITCODE -ne 0) { throw "黑盒验收未通过（exit code $LASTEXITCODE）" }
}
