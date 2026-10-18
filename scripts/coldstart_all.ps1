# One-shot cold start acceptance for dist/Model-Gate.exe:
#   seed config -> launch exe -> run acceptance checks -> report artifacts -> stop
# NOTE: keep this file ASCII-only. PowerShell reads a BOM-less .ps1 as ANSI,
#       so non-ASCII comments silently shift line numbers and break parsing.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\coldstart_all.ps1
$ErrorActionPreference = "Continue"
$root = (Get-Location).Path
Set-Location $root

$exe = Join-Path $root "dist\Model-Gate.exe"
$port = "8911"
if (-not (Test-Path $exe)) {
    Write-Output "MISSING: $exe"
    exit 1
}

# kill leftover instance from a previous run
Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# The packaged exe reads bridge.config.json next to itself by design.
# Seed it from the repo config so every acceptance run starts from a clean state.
$distCfg = Join-Path $root "dist\bridge.config.json"
Copy-Item (Join-Path $root "bridge.config.json") $distCfg -Force
Write-Output ("    seeded dist\bridge.config.json size=" + (Get-Item $distCfg).Length)

# drop previous runtime data so artifacts are produced by this run
Remove-Item (Join-Path $root "dist\data\modelgate.db*") -ErrorAction SilentlyContinue
Remove-Item (Join-Path $root "dist\data\modelgate.log*") -ErrorAction SilentlyContinue

Write-Output "[1/3] launching exe on port $port ..."
$proc = Start-Process -FilePath $exe `
    -ArgumentList "--no-gui","--port",$port `
    -RedirectStandardOutput (Join-Path $root "coldstart.out") `
    -RedirectStandardError (Join-Path $root "coldstart.err") `
    -PassThru

# wait for listener (onefile unpack takes a few seconds)
$listening = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        $listening = $true
        break
    }
    if ($proc.HasExited) { break }
}
Write-Output ("    listening=" + $listening + " exited=" + $proc.HasExited)

Write-Output "[2/3] running acceptance checks ..."
& py -3.11 scripts\coldstart_check.py "http://127.0.0.1:$port"
$checkExit = $LASTEXITCODE

Write-Output "[3/3] runtime artifacts (next to exe) ..."
foreach ($rel in @("dist\data\modelgate.db", "dist\data\modelgate.log",
                   "dist\bridge.config.json")) {
    $p = Join-Path $root $rel
    if (Test-Path $p) {
        $item = Get-Item $p
        Write-Output ("    OK  " + $rel + "  " + $item.Length + " bytes")
    } else {
        Write-Output ("    MISSING  " + $rel)
    }
}
$artOk = ((Test-Path (Join-Path $root "dist\data\modelgate.db")) -and
          (Test-Path (Join-Path $root "dist\data\modelgate.log")))

if (Test-Path (Join-Path $root "dist\data\modelgate.log")) {
    Write-Output "    --- log tail ---"
    Get-Content (Join-Path $root "dist\data\modelgate.log") -Tail 8 |
        ForEach-Object { Write-Output ("    " + $_) }
}

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output "    exe stopped"

if (($checkExit -eq 0) -and $artOk) {
    Write-Output "COLDSTART ACCEPTED"
    exit 0
}
Write-Output "COLDSTART REJECTED"
exit 1
