@echo off
REM Model-Gate build script (aligned with reference project command-code-gauge)
REM   [1/6] probe interpreter
REM   [2/6] generate icon
REM   [3/6] run tests  (gate: stop on failure)
REM   [4/6] PyInstaller onefile  -> dist\Model-Gate.exe
REM   [5/6] Inno Setup          -> dist\Model-Gate-v<ver>-setup.exe
REM   [6/6] summary
REM Single source of version: modelgate\__init__.py, injected via iscc /DMyAppVersion.
REM NOTE: this file must stay ASCII-only, otherwise cmd.exe (OEM codepage) misparses it.

setlocal enabledelayedexpansion
cd /d "%~dp0"

REM -- [1/6] probe interpreter: python first, fall back to the py launcher ----
set "PY="
where python >nul 2>nul && set "PY=python"
if not defined PY (
  where py >nul 2>nul && set "PY=py -3.11"
)
if not defined PY (
  echo [build] Python not found. Install Python 3.11+ and add it to PATH.
  exit /b 1
)
for /f "delims=" %%v in ('%PY% --version 2^>^&1') do echo [build] interpreter: %%v

REM -- version (single source: modelgate\__init__.py) ------------------------
set "VERSION="
%PY% scripts\version.py > "%TEMP%\mg_version.txt" 2>nul
if exist "%TEMP%\mg_version.txt" set /p VERSION=<"%TEMP%\mg_version.txt"
if not defined VERSION (
  echo [build] Failed to read version from modelgate\__init__.py
  exit /b 1
)
echo [build] version: %VERSION%

REM -- [2/6] icon ------------------------------------------------------------
echo [build] [2/6] generating icon...
%PY% scripts\make_icon.py
if errorlevel 1 (
  echo [build] icon generation failed.
  exit /b 1
)

REM -- [3/6] test gate -------------------------------------------------------
REM 逐文件有界跑：整目录一次性跑会出现无界挂起（跑不完 = 没有结论）。
echo [build] [3/6] running tests (bounded, per-file)...
%PY% scripts\run_tests.py
if errorlevel 1 (
  echo [build] tests failed, packaging aborted.
  exit /b 1
)

REM -- [4/6] PyInstaller onefile --------------------------------------------
echo [build] [4/6] PyInstaller packaging...
%PY% -m PyInstaller --noconfirm --clean ModelGate.spec
if errorlevel 1 (
  echo [build] PyInstaller failed.
  exit /b 1
)
if not exist "dist\Model-Gate.exe" (
  echo [build] dist\Model-Gate.exe was not produced.
  exit /b 1
)

REM -- [5/6] Inno Setup (skip gracefully when iscc is absent) ---------------
echo [build] [5/6] Inno Setup installer...
where iscc >nul 2>nul
if errorlevel 1 (
  echo [build] iscc not found, installer skipped. Only dist\Model-Gate.exe delivered.
  goto summary
)
iscc /DMyAppVersion=%VERSION% ModelGate.iss
if errorlevel 1 (
  echo [build] Inno Setup compilation failed.
  exit /b 1
)

:summary
REM -- [6/6] summary ---------------------------------------------------------
echo.
echo [build] done
echo   version    %VERSION%
echo   exe        dist\Model-Gate.exe
if exist "dist\Model-Gate-v%VERSION%-setup.exe" (
  echo   installer  dist\Model-Gate-v%VERSION%-setup.exe
)
echo   runtime    data\  (modelgate.db / modelgate.log)
echo   config     bridge.config.json
exit /b 0
