# Launch the packaged exe against a fail-fast config, run every probe,
# then stop. Captures exe stdout/stderr so tracebacks are visible.
# ASCII-only (PowerShell reads BOM-less .ps1 as ANSI).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\probe_exe.ps1
$ErrorActionPreference = "Continue"
$root = (Get-Location).Path
Set-Location $root

$exe = Join-Path $root "dist\Model-Gate.exe"
if (-not (Test-Path $exe)) { Write-Output "MISSING exe"; exit 1 }

$port = "8912"
Write-Output ("PROBE_PORT=" + $port)

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Remove-Item (Join-Path $root "probe.out"), (Join-Path $root "probe.err") `
    -ErrorAction SilentlyContinue
Remove-Item (Join-Path $root "dist\data\modelgate.db*") -ErrorAction SilentlyContinue
Remove-Item (Join-Path $root "dist\data\modelgate.log*") -ErrorAction SilentlyContinue

# Seed a config whose upstreams fail fast (closed port) so probes exercise OUR
# error handling instead of waiting on a slow real upstream.
$cfgPath = Join-Path $root "dist\bridge.config.json"
Copy-Item (Join-Path $root "bridge.config.json") $cfgPath -Force
$cfg = Get-Content $cfgPath -Raw | ConvertFrom-Json
$cfg.port = [int]$port
foreach ($name in $cfg.upstreams.PSObject.Properties.Name) {
    $up = $cfg.upstreams.$name
    $up.baseUrl = "http://127.0.0.1:1"
    # some upstreams omit apiKey (they use apiKeyEnv/apiKeyFile) -> add it
    $up | Add-Member -NotePropertyName apiKey -NotePropertyValue "probe-key" -Force
    $up.PSObject.Properties.Remove("apiKeyEnv")
    $up.PSObject.Properties.Remove("apiKeyFile")
    if ($up.PSObject.Properties.Name -contains "fetchModels") { $up.fetchModels = $false }
    if ($up.PSObject.Properties.Name -contains "autoModels") { $up.autoModels = $false }
}
$cc = $cfg.upstreams.commandcode
if ($cc) {
    $cc.modelCatalog = $false
    $cc.modelCatalogPlan = $false
}
$text = $cfg | ConvertTo-Json -Depth 12
[IO.File]::WriteAllText($cfgPath, $text)
Write-Output "    seeded fail-fast config"

$proc = Start-Process -FilePath $exe `
    -ArgumentList "--no-gui" `
    -RedirectStandardOutput (Join-Path $root "probe.out") `
    -RedirectStandardError (Join-Path $root "probe.err") `
    -PassThru

$listening = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        $listening = $true; break
    }
    if ($proc.HasExited) { break }
}
Write-Output ("LISTENING=" + $listening + " EXITED=" + $proc.HasExited)

$probeExit = 1
if ($listening) {
    Write-Output ""
    Write-Output "=== probe: normal endpoints ==="
    & py -3.11 scripts\probe_all.py ("http://127.0.0.1:" + $port)
    $allExit = $LASTEXITCODE
    Write-Output ""
    Write-Output "=== probe: edge payloads ==="
    & py -3.11 scripts\probe_edge.py ("http://127.0.0.1:" + $port)
    $edgeExit = $LASTEXITCODE
    $probeExit = 0
    if ($allExit -ne 0) { $probeExit = $allExit }
    if ($edgeExit -ne 0) { $probeExit = $edgeExit }
}

Write-Output ""
Write-Output "--- exe stderr (tail) ---"
if (Test-Path (Join-Path $root "probe.err")) {
    Get-Content (Join-Path $root "probe.err") -Tail 40 |
        ForEach-Object { Write-Output ("  " + $_) }
}
Write-Output "--- data\modelgate.log (tail) ---"
$logPath = Join-Path $root "dist\data\modelgate.log"
if (Test-Path $logPath) {
    Get-Content $logPath -Encoding UTF8 -Tail 25 |
        ForEach-Object { Write-Output ("  " + $_) }
} else {
    Write-Output "  (missing)"
}

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output "STOPPED"
if ($probeExit -eq 0) { Write-Output "EXE PROBE ACCEPTED" }
else { Write-Output "EXE PROBE REJECTED" }
exit $probeExit
