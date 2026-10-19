@echo off
chcp 65001 >nul
REM 一键重启 model-bridge：先结束占用端口的旧进程，再启动新进程并自检。
REM 改了 model-bridge.js 或 bridge.config.json 之后用它生效（Node 没有热加载）。
REM 用法：restart-bridge.cmd [端口]      不传端口时按 8900 检查
cd /d "%~dp0"

set PORT=%~1
set NODE_ARGS=
if defined PORT set NODE_ARGS=--port %PORT%
if not defined PORT set PORT=8900

set OLD_PID=
echo [1/3] 检查端口 %PORT% 是否被旧进程占用 ...
for /f "tokens=5" %%p in ('netstat -ano ^| findstr ":%PORT% " ^| findstr "LISTENING"') do set OLD_PID=%%p
if defined OLD_PID (
  echo   结束旧进程 PID %OLD_PID%
  taskkill /F /PID %OLD_PID% >nul 2>&1
  ping -n 2 127.0.0.1 >nul
) else (
  echo   没有旧进程在跑
)

echo [2/3] 启动新进程（新窗口，日志在其中；关掉窗口即停止）...
start "model-bridge" cmd /k node "%~dp0model-bridge.js" %NODE_ARGS%

echo [3/3] 自检 http://127.0.0.1:%PORT%/health ...
ping -n 3 127.0.0.1 >nul
curl.exe -s -m 5 http://127.0.0.1:%PORT%/health
if errorlevel 1 (
  echo.
  echo   [警告] 自检失败：请查看新窗口里的日志
) else (
  echo.
  echo   OK - 模型列表：curl http://127.0.0.1:%PORT%/v1/models
)
pause
