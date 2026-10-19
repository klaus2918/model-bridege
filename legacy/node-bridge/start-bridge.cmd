@echo off
chcp 65001 >nul
REM 一键启动 model-bridge（按模型名分发到多条上游）
REM 上游：cch（本地代理 15721）/ go（opencode Go 套餐）/ zen（opencode 免费模型）
REM 用法：start-bridge.cmd [端口]      不传端口时用 bridge.config.json 里的 port
cd /d "%~dp0"

set PORT=%~1
set NODE_ARGS=
if defined PORT set NODE_ARGS=--port %PORT%
if not defined PORT set PORT=8900

echo [1/3] 检查 cch 上游 127.0.0.1:15721 ...
powershell -NoProfile -Command "if (Test-NetConnection 127.0.0.1 -Port 15721 -InformationLevel Quiet -WarningAction SilentlyContinue) { exit 0 } else { exit 1 }"
if errorlevel 1 (
  echo   [警告] 15721 未监听：cch 上游的模型会失败，请先打开本地代理。
) else (
  echo   OK
)

echo [2/3] 检查 OPENCODE_API_KEY（go 上游需要）...
powershell -NoProfile -Command "$k=$env:OPENCODE_API_KEY; if (-not $k) { $f=Join-Path $env:USERPROFILE '.jcode\.env'; if (Test-Path $f) { $line = Select-String -Path $f -Pattern '^\s*OPENCODE_API_KEY\s*=' | Select-Object -First 1; if ($line) { $k='found' } } }; if ($k) { Write-Host '  OK'; exit 0 } else { Write-Host '  [警告] 未找到 OPENCODE_API_KEY：go 上游会 401（zen 免费模型不受影响）'; exit 0 }"

echo [3/3] 启动 model-bridge（端口 %PORT%）...
node "%~dp0model-bridge.js" %NODE_ARGS%
