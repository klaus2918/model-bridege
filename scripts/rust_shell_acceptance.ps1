# Black-box acceptance for the Rust desktop shell (tray + system-browser panel).
#
# Verifies the strict metrics from the change plan:
#   - idle working set <= 20MB (default after 60s idle, browser excluded)
#   - installer <= 6MB when a *rust-setup.exe artifact exists in dist\
#   - startup <= 1s (process start -> /health 200)
#   - no msedgewebview2.exe anywhere in the Model-Gate process tree
#   - second launch exits (single-instance activate) without a duplicate listener
#   - panel config save round-trip (profiles + modelOverrides) against the live Rust server
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1 -Exe dist\Model-Gate-rust.exe -IdleSeconds 30
#
# Notes:
#   - Only processes started by this script are stopped; user browsers are never touched.
#   - The script passes --no-browser to both launches: it validates the non-intrusive
#     activation path (no duplicate server, lastPort reuse). Visual "panel opens in the
#     default browser" behavior is covered by the manual tray checks (usage step 4).

param(
    [string]$Exe = "",
    [int]$Port = 0,
    [int]$IdleSeconds = 60,
    [int]$StartupBudgetMs = 1000,
    [double]$WorkingSetLimitMB = 20,
    [double]$InstallerLimitMB = 6,
    [switch]$KeepRunning
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot

function Resolve-Exe([string]$explicit) {
    if ($explicit) { return (Resolve-Path -LiteralPath $explicit).Path }
    $candidates = @(
        (Join-Path $root "rust\model-gate\target\release\model-gate.exe"),
        (Join-Path $root "dist\Model-Gate-rust.exe"),
        (Join-Path $root "rust\model-gate\target\debug\model-gate.exe")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) { return (Resolve-Path -LiteralPath $candidate).Path }
    }
    throw "Model-Gate exe not found. Pass -Exe <path> or run: cargo build --release (rust\model-gate)"
}

$Exe = Resolve-Exe $Exe

$work = Join-Path $env:TEMP ("mg-accept-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$cfgPath = Join-Path $work "bridge.config.json"
$dbPath = Join-Path $work "data\modelgate.db"

if (-not $Port) {
    $tcp = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $tcp.Start()
    $Port = [int]$tcp.LocalEndpoint.Port
    $tcp.Stop()
}

$cfgObject = [ordered]@{
    host = "127.0.0.1"
    port = $Port
    defaultModel = "m"
    models = @("m")
    upstreams = [ordered]@{
        oai = [ordered]@{
            protocol = "openai"
            baseUrl = "http://127.0.0.1:1"
            apiKey = "<acceptance-key>"
            models = @("m")
            default = $true
        }
    }
}
[IO.File]::WriteAllText($cfgPath, ($cfgObject | ConvertTo-Json -Depth 8), (New-Object System.Text.UTF8Encoding($false)))

$results = New-Object System.Collections.Generic.List[object]
function Add-Result([string]$name, [bool]$ok, [string]$detail) {
    $results.Add([pscustomobject]@{ Name = $name; Ok = $ok; Detail = $detail })
    $tag = if ($ok) { "PASS" } else { "FAIL" }
    Write-Host ("[{0}] {1} - {2}" -f $tag, $name, $detail)
}

function Get-ProcessTree([int]$rootPid) {
    $all = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Select-Object ProcessId, ParentProcessId, Name)
    $seen = New-Object System.Collections.Generic.HashSet[int]
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($rootPid)
    while ($queue.Count -gt 0) {
        $current = $queue.Dequeue()
        if (-not $seen.Add($current)) { continue }
        foreach ($child in $all) {
            if ([int]$child.ParentProcessId -eq $current) { $queue.Enqueue([int]$child.ProcessId) }
        }
    }
    return @($all | Where-Object { $seen.Contains([int]$_.ProcessId) })
}

function Get-ListeningPid([int]$port) {
    try {
        $conn = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction Stop |
            Select-Object -First 1
        if ($conn) { return [int]$conn.OwningProcess }
    } catch { }
    $line = (netstat -ano | Select-String -Pattern (":{0}\s+.*LISTENING" -f $port) |
        Select-Object -First 1)
    if ($line -and $line.Line -match "(\d+)\s*$") { return [int]$Matches[1] }
    return 0
}

function Invoke-Json([string]$method, [string]$path, $body = $null) {
    $uri = "http://127.0.0.1:$Port$path"
    if ($null -eq $body) {
        return Invoke-RestMethod -Method $method -Uri $uri -TimeoutSec 10
    }
    $json = $body | ConvertTo-Json -Depth 10 -Compress
    return Invoke-RestMethod -Method $method -Uri $uri -TimeoutSec 10 -ContentType "application/json" -Body $json
}

$proc = $null
$startedPids = New-Object System.Collections.Generic.List[int]
try {
    $arguments = "--config `"$cfgPath`" --db `"$dbPath`" --port $Port --no-browser"
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru
    $startedPids.Add($proc.Id)

    # 1) startup budget: process start -> /health 200
    $healthy = $false
    while ($sw.ElapsedMilliseconds -lt 30000) {
        if ($proc.HasExited) { break }
        try {
            $resp = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 1
            if ($resp.StatusCode -eq 200) { $healthy = $true; break }
        } catch { }
        Start-Sleep -Milliseconds 25
    }
    $startupMs = [int]$sw.ElapsedMilliseconds
    if ($healthy) {
        Add-Result "startup <= ${StartupBudgetMs}ms" ($startupMs -le $StartupBudgetMs) "health 200 in ${startupMs}ms"
    } else {
        Add-Result "startup <= ${StartupBudgetMs}ms" $false "health not reachable within 30s (exited=$($proc.HasExited))"
        throw "server never became healthy; aborting remaining checks"
    }

    $ui = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/ui/" -TimeoutSec 5
    Add-Result "/ui/ serves 200" ($ui.StatusCode -eq 200) "status $($ui.StatusCode)"

    # 2) idle working set (browser excluded; this process tree only)
    Write-Host ("... measuring idle working set for {0}s ..." -f $IdleSeconds)
    Start-Sleep -Seconds $IdleSeconds
    $proc.Refresh()
    if ($proc.HasExited) { throw "process exited during idle measurement" }
    $wsMB = [math]::Round($proc.WorkingSet64 / 1MB, 2)
    Add-Result "idle working set <= ${WorkingSetLimitMB}MB" ($wsMB -le $WorkingSetLimitMB) "${wsMB}MB after ${IdleSeconds}s idle"

    # 3) no WebView2 anywhere in the process tree
    $tree = Get-ProcessTree $proc.Id
    $webview = @($tree | Where-Object { $_.Name -like "msedgewebview2*" })
    $treeNames = ($tree | Where-Object { [int]$_.ProcessId -ne $proc.Id } |
        Select-Object -ExpandProperty Name -Unique) -join ", "
    Add-Result "no msedgewebview2.exe in process tree" ($webview.Count -eq 0) `
        ("tree children: " + $(if ($treeNames) { $treeNames } else { "(none)" }))

    # 4) installer size (only when a Rust setup artifact exists)
    $setup = Get-ChildItem (Join-Path $root "dist") -Filter "*rust-setup.exe" -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($setup) {
        $setupMB = [math]::Round($setup.Length / 1MB, 2)
        Add-Result "installer <= ${InstallerLimitMB}MB" ($setupMB -le $InstallerLimitMB) `
            ("{0} = {1}MB" -f $setup.Name, $setupMB)
    } else {
        Add-Result "installer <= ${InstallerLimitMB}MB" $true "no dist\*rust-setup.exe yet (packaging task); skipped"
    }

    # 5) second launch: single-instance activate, no duplicate listener
    $sw2 = [System.Diagnostics.Stopwatch]::StartNew()
    $proc2 = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru
    $startedPids.Add($proc2.Id)
    $exited = $proc2.WaitForExit(8000)
    $sw2.Stop()
    Add-Result "second launch exits (activate existing)" $exited `
        ("waited {0}ms; exit code {1}" -f $sw2.ElapsedMilliseconds, $(if ($exited) { $proc2.ExitCode } else { "n/a" }))
    Start-Sleep -Milliseconds 400
    $proc.Refresh()
    $listeningPid = Get-ListeningPid $Port
    Add-Result "single listener on port $Port (first process)" `
        ((-not $proc.HasExited) -and ($listeningPid -eq $proc.Id)) `
        ("listener pid=$listeningPid, first pid=$($proc.Id)")

    # 6) config save round-trip: profiles data + modelOverrides persist
    $live = Invoke-Json "GET" "/api/config"
    $profileIds = @($live.profiles | ForEach-Object { $_.id })
    $expectedIds = @("openai", "opencode", "anthropic", "deepseek", "moonshot", "zhipu", "minimax", "dashscope", "ark", "siliconflow", "modelscope", "hunyuan", "commandcode")
    Add-Result "GET /api/config exposes profiles" `
        (($profileIds -join ",") -eq ($expectedIds -join ",")) ("profiles: " + ($profileIds -join ","))

    $cfg = $live.config
    if (-not $cfg.upstreams.oai.modelOverrides) {
        $cfg.upstreams.oai | Add-Member -NotePropertyName modelOverrides -NotePropertyValue (@{ "m-*" = @{ limits = @{ maxOutputTokens = 16384 } } }) -Force
    }
    $saved = Invoke-Json "POST" "/api/config" @{ config = $cfg }
    $live2 = Invoke-Json "GET" "/api/config"
    $override = $live2.config.upstreams.oai.modelOverrides
    Add-Result "modelOverrides survives config save" `
        ($null -ne $override -and $null -ne $override.'m-*') ("saved=" + ($saved.ok -eq $true))

    $eff = @($live2.effective)[0]
    Add-Result "effective echoes modelOverrides" `
        ($null -ne $eff.modelOverrides) ("keys: " + (($eff.modelOverrides | Get-Member -MemberType NoteProperty | Select-Object -ExpandProperty Name) -join ","))
}
finally {
    if (-not $KeepRunning) {
        foreach ($startPid in $startedPids) {
            try {
                $tree = Get-ProcessTree $startPid
                foreach ($node in $tree) { Stop-Process -Id ([int]$node.ProcessId) -Force -ErrorAction SilentlyContinue }
            } catch { }
        }
        if ($work -and (Test-Path -LiteralPath $work)) {
            Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

$failed = @($results | Where-Object { -not $_.Ok })
Write-Host ""
Write-Host ("summary: {0} passed, {1} failed  (exe: {2})" -f ($results.Count - $failed.Count), $failed.Count, $Exe)
if ($failed.Count -gt 0) {
    foreach ($item in $failed) { Write-Host ("  FAIL: {0} - {1}" -f $item.Name, $item.Detail) }
    exit 1
}
exit 0
