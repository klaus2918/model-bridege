# Fetch Inno Setup: parse the official downloads page for the real installer URL,
# download it, then install silently into a user-writable directory.
# ASCII-only (PowerShell reads BOM-less .ps1 as ANSI).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\get_innosetup.ps1
$ErrorActionPreference = "Continue"
$root = (Get-Location).Path

$dest = Join-Path $root "tools"
if (-not (Test-Path $dest)) { New-Item -ItemType Directory -Path $dest | Out-Null }

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# 1) grab the downloads page and extract the installer link
$page = Join-Path $dest "isdl.html"
try {
    Invoke-WebRequest -Uri "https://jrsoftware.org/isdl.php" -OutFile $page `
        -TimeoutSec 90 -UseBasicParsing
} catch {
    Write-Output ("PAGE_FAILED: " + $_.Exception.Message)
    exit 1
}
$html = Get-Content $page -Raw
$link = ""
$m = [regex]::Matches($html, 'https?://[^"''\s>]*innosetup-[\d.]+\.exe')
if ($m.Count -gt 0) { $link = $m[$m.Count - 1].Value }
if (-not $link) {
    $m2 = [regex]::Matches($html, '(?:href|src)="([^"]*innosetup[^"]*\.exe)"')
    if ($m2.Count -gt 0) { $link = $m2[$m2.Count - 1].Groups[1].Value }
}
if (-not $link) {
    Write-Output "NO_LINK_FOUND"
    exit 1
}
Write-Output ("LINK=" + $link)

# 2) download installer
$setup = Join-Path $dest "innosetup.exe"
if (Test-Path $setup) { Remove-Item $setup -Force }
try {
    Invoke-WebRequest -Uri $link -OutFile $setup -TimeoutSec 180 -UseBasicParsing
} catch {
    Write-Output ("DOWNLOAD_FAILED: " + $_.Exception.Message)
    exit 1
}
$size = (Get-Item $setup).Length
Write-Output ("DOWNLOADED_BYTES=" + $size)
if ($size -lt 1000000) { Write-Output "DOWNLOAD_TOO_SMALL"; exit 1 }

# 3) silent install into user-writable dir
$target = Join-Path $dest "InnoSetup6"
$log = Join-Path $dest "inno.log"
$args = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
          "/DIR=" + $target, "/LOG=" + $log)
Write-Output ("INSTALLING_TO=" + $target)
$proc = Start-Process -FilePath $setup -ArgumentList $args -PassThru -Wait
Write-Output ("INSTALLER_EXIT=" + $proc.ExitCode)

$iscc = Join-Path $target "ISCC.exe"
if (Test-Path $iscc) { Write-Output ("ISCC_OK=" + $iscc); exit 0 }
$hit = Get-ChildItem -Path $dest -Filter "ISCC.exe" -Recurse -Depth 4 `
    -ErrorAction SilentlyContinue | Select-Object -First 1
if ($hit) { Write-Output ("ISCC_OK=" + $hit.FullName); exit 0 }
Write-Output "ISCC_MISSING"
exit 1
