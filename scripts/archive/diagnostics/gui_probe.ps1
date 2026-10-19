# GUI mode probe: launch exe (no --no-gui) with a free port, verify window,
# listener and panel, then stop.
# NOTE: keep this file ASCII-only. PowerShell reads a BOM-less .ps1 as ANSI,
#       so non-ASCII comments silently shift line numbers and break parsing.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\gui_probe.ps1
$ErrorActionPreference = "Continue"
$root = (Get-Location).Path
Set-Location $root

$exe = Join-Path $root "dist\Model-Gate.exe"
if (-not (Test-Path $exe)) {
    Write-Output "MISSING: $exe"
    exit 1
}

# 8900 may be held by the existing Node bridge; probe on 8911 to also
# verify that the --port override works in GUI mode.
$port = "8911"
Write-Output ("PROBE_PORT=" + $port)

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

$proc = Start-Process -FilePath $exe `
    -ArgumentList "--port",$port `
    -RedirectStandardOutput (Join-Path $root "gui.out") `
    -RedirectStandardError (Join-Path $root "gui.err") `
    -PassThru

$listening = $false
$title = ""
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        $listening = $true
    }
    if (-not $proc.HasExited) {
        $proc.Refresh()
        if ($proc.MainWindowHandle -ne 0) { $title = $proc.MainWindowTitle }
    }
    if (-not $title) {
        $named = Get-Process Model-Gate -ErrorAction SilentlyContinue |
            Where-Object { $_.MainWindowTitle -match "Model-Gate" }
        if ($named) { $title = ($named | Select-Object -First 1).MainWindowTitle }
    }
    if ($listening -and $title) { break }
    if ($proc.HasExited) { break }
}
Start-Sleep -Seconds 2
if (-not $proc.HasExited) { $proc.Refresh() }

Write-Output ("EXE_PID=" + $proc.Id)
Write-Output ("EXE_EXITED=" + $proc.HasExited)
Write-Output ("LISTENING=" + $listening + " port=" + $port)
Write-Output ("WINDOW_TITLE=" + $title)
Write-Output ("HAS_WINDOW=" + ($title -ne ""))

$uiOk = $false
try {
    $res = Invoke-WebRequest "http://127.0.0.1:$port/ui/" -TimeoutSec 10 -UseBasicParsing
    $uiOk = ($res.StatusCode -eq 200)
} catch {
    $uiOk = $false
}
Write-Output ("UI_OK=" + $uiOk)

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output "STOPPED"

if ($listening -and $uiOk) {
    if ($title) {
        Write-Output "GUI PROBE OK (window + server)"
        exit 0
    }
    Write-Output "GUI PROBE PARTIAL (server ok, window not observed)"
    exit 0
}
Write-Output "GUI PROBE FAILED"
exit 1
