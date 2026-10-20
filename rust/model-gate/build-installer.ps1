# Build the Inno Setup installer (Rust kernel).
# Usage:
#   powershell -ExecutionPolicy Bypass -File rust\model-gate\build-installer.ps1
#   powershell -ExecutionPolicy Bypass -File rust\model-gate\build-installer.ps1 -Version 2.19.0
# Prerequisite: Inno Setup 6+ (ISCC.exe in PATH, or the default install dirs).
param(
    [string]$Version = "2.19.0",
    [string]$SourceExe = "",
    [string]$OutputName = ""
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path

$iscc = $null
$cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($cmd) { $iscc = $cmd.Source }
if (-not $iscc) {
    foreach ($p in @(
        (Join-Path $env:LOCALAPPDATA "InnoSetup6\ISCC.exe"),
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe",
        "D:\webSoft\InnoSetup6\ISCC.exe"
    )) {
        if (Test-Path $p) { $iscc = $p; break }
    }
}
if (-not $iscc) {
    throw "ISCC.exe not found. Install Inno Setup 6+ first: https://jrsoftware.org/isdl.php"
}

if (-not $SourceExe) { $SourceExe = Join-Path $root "dist\Model-Gate-rust.exe" }
$SourceExe = (Resolve-Path $SourceExe).Path
if (-not $OutputName) { $OutputName = "Model-Gate-v$Version-rust-setup" }

Write-Host "ISCC: $iscc"
Write-Host "Source: $SourceExe"
Write-Host "Output: dist\$OutputName.exe"

& $iscc "/DMyAppVersion=$Version" "/DMyAppSourceExe=$SourceExe" `
        "/DMyOutputBaseFilename=$OutputName" (Join-Path $root "ModelGate.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }

$setup = Join-Path $root "dist\$OutputName.exe"
$size = [math]::Round((Get-Item $setup).Length / 1MB, 2)
Write-Host "Built: $setup ($size MB)"
