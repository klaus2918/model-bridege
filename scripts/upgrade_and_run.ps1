param(
    [string]$Exe = "",
    [string]$WorkDir = "",
    [int]$Port = 0,
    [string]$ExpectedVersion = ""
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"

if (-not $WorkDir) { $WorkDir = $dist }

# Prefer a versioned copy so dist\Model-Gate.exe stays free for the next build.
if (-not $Exe) {
    $versioned = Get-ChildItem $dist -Filter "Model-Gate-v*.exe" -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notmatch "setup" } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if ($versioned) {
        $Exe = $versioned.FullName
    } else {
        $Exe = Join-Path $dist "Model-Gate.exe"
    }
}
if (-not (Test-Path $Exe)) { throw "Model-Gate exe not found: $Exe" }

if (-not $Port) {
    $cfg = Join-Path $WorkDir "bridge.config.json"
    if (Test-Path $cfg) {
        try { $Port = [int]((Get-Content $cfg -Raw | ConvertFrom-Json).port) } catch { }
    }
    if (-not $Port) { $Port = 8900 }
}

function Get-Instances {
    @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Name -like "Model-Gate*.exe" -and
        $_.ExecutablePath -and
        $_.ExecutablePath.StartsWith($dist, [StringComparison]::OrdinalIgnoreCase)
    })
}

$old = Get-Instances
if ($old.Count -gt 0) {
    Write-Output ("STOP {0} process(es) from {1}" -f $old.Count, $dist)
    foreach ($item in $old) {
        try {
            $proc = Get-Process -Id $item.ProcessId -ErrorAction Stop
            if ($proc.MainWindowHandle -ne 0) { [void]$proc.CloseMainWindow() }
        } catch { }
    }
    Start-Sleep -Seconds 2
    for ($i = 0; $i -lt 10; $i++) {
        $left = Get-Instances
        if ($left.Count -eq 0) { break }
        foreach ($item in $left) {
            Stop-Process -Id $item.ProcessId -Force -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 500
    }
    if ((Get-Instances).Count -gt 0) { throw "old Model-Gate processes did not exit" }
}

Write-Output ("START {0}" -f $Exe)
Start-Process -FilePath $Exe -WorkingDirectory $WorkDir

$found = $null
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 750
    foreach ($candidate in ($Port..($Port + 10))) {
        try {
            $info = Invoke-RestMethod ("http://127.0.0.1:{0}/api/info" -f $candidate) -TimeoutSec 1
            if ($info.app -eq "Model-Gate") {
                $found = [pscustomobject]@{ Port = $candidate; Version = $info.version }
                break
            }
        } catch { }
    }
    if ($found) { break }
}

if (-not $found) { throw "new Model-Gate instance did not become ready" }
if ($ExpectedVersion -and $found.Version -ne $ExpectedVersion) {
    throw ("version mismatch: expected {0}, got {1}" -f $ExpectedVersion, $found.Version)
}

Write-Output ("READY http://127.0.0.1:{0}/ui/ version={1}" -f $found.Port, $found.Version)
