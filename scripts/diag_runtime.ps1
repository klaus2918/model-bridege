# Real-world diagnostic: is the proxy actually up, can it reach upstreams,
# and are logs/DB being written?
# ASCII-only (PowerShell reads BOM-less .ps1 as ANSI).
$ErrorActionPreference = "Continue"
$root = (Get-Location).Path
Set-Location $root

Write-Output "=== [1] listeners ==="
foreach ($p in @(8900, 8911, 8912, 15721)) {
    $c = Get-NetTCPConnection -LocalPort $p -State Listen -ErrorAction SilentlyContinue
    if ($c) {
        foreach ($item in $c) {
            $proc = Get-Process -Id $item.OwningProcess -ErrorAction SilentlyContinue
            $name = "?"
            if ($proc) { $name = $proc.ProcessName }
            Write-Output ("  port " + $p + " LISTEN pid=" + $item.OwningProcess + " " + $name)
        }
    } else {
        Write-Output ("  port " + $p + " (nothing)")
    }
}

Write-Output "=== [2] Model-Gate processes ==="
$mg = Get-Process Model-Gate -ErrorAction SilentlyContinue
if ($mg) { $mg | ForEach-Object { Write-Output ("  pid=" + $_.Id) } }
else { Write-Output "  none" }

Write-Output "=== [3] artifacts next to exe ==="
foreach ($rel in @("dist\bridge.config.json", "dist\data\modelgate.db",
                   "dist\data\modelgate.log", "data\modelgate.db",
                   "data\modelgate.log")) {
    $path = Join-Path $root $rel
    if (Test-Path $path) {
        $item = Get-Item $path
        Write-Output ("  OK   " + $rel + " " + $item.Length + " bytes  " + $item.LastWriteTime)
    } else {
        Write-Output ("  MISS " + $rel)
    }
}

Write-Output "=== [4] probe local upstreams (is proxy chain reachable) ==="
foreach ($p in @(15721, 8900)) {
    $t = New-Object Net.Sockets.TcpClient
    try {
        $ok = $t.ConnectAsync("127.0.0.1", $p).Wait(1500)
        if ($ok) { Write-Output ("  127.0.0.1:" + $p + " reachable") }
        else { Write-Output ("  127.0.0.1:" + $p + " timeout") }
    } catch {
        Write-Output ("  127.0.0.1:" + $p + " FAIL " + $_.Exception.Message)
    } finally {
        $t.Close()
    }
}
exit 0
