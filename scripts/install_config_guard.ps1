# install_config_guard.ps1
# Verify the installer ships the default config package (bridge.config.json)
# and never overwrites existing user data. The config documentation now lives
# in the panel itself (the "配置说明" dialog), so no standalone help file is
# shipped any more.
#
# Scenarios:
#   S1 clean silent install  -> exe + bridge.config.json exist;
#                               the config is byte-identical to
#                               rust\model-gate\default_config.json, parses as JSON,
#                               has _doc, has the zen upstream, has no plaintext
#                               apiKey; no advanced template / markdown guide /
#                               standalone help page is shipped; no database is
#                               shipped.
#   S2 pre-existing data     -> a custom bridge.config.json and a sentinel
#                               data\modelgate.db placed in the target dir before
#                               install are NOT overwritten (hash equal).
#   S3 upgrade (re-install)  -> after editing the config, re-running the same
#                               setup keeps the config hash and the db hash
#                               unchanged, and the exe is (re)installed.
#   S4 uninstall              -> bridge.config.json and data\modelgate.db survive
#                               the uninstaller.
#   S5 portable first run     -> a fresh portable exe generates a config whose
#                               value equals rust\model-gate\default_config.json.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install_config_guard.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install_config_guard.ps1 `
#       -SetupExe dist\Model-Gate-v2.22.0-rust-setup.exe
#
# Exit code: 0 = all assertions passed, 1 = at least one assertion failed.
param(
    [string]$SetupExe = "",
    [string]$WorkDir = "",
    [string]$LogFile = ""
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$defaultConfig = Join-Path $root "rust\model-gate\default_config.json"
$portableExe = Join-Path $root "dist\Model-Gate-rust.exe"

if (-not $SetupExe) {
    $candidates = @(Get-ChildItem -Path (Join-Path $root "dist") -Filter "Model-Gate-v*-rust-setup.exe" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending)
    if ($candidates.Count -eq 0) { throw "no setup exe found under dist\" }
    $SetupExe = $candidates[0].FullName
}
$SetupExe = (Resolve-Path $SetupExe).Path

if (-not (Test-Path $defaultConfig)) { throw "missing $defaultConfig" }
if (-not (Test-Path $portableExe)) { throw "missing $portableExe" }

if (-not $WorkDir) {
    $WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("modelgate-install-guard-" + $PID)
}
if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Force $WorkDir | Out-Null
$WorkDir = (Resolve-Path $WorkDir).Path

if (-not $LogFile) {
    $LogFile = Join-Path $root ".op\changes\install-default-config\install-config-guard.log"
}
$logDir = Split-Path -Parent $LogFile
if ($logDir -and -not (Test-Path $logDir)) { New-Item -ItemType Directory -Force $logDir | Out-Null }

$script:Failures = New-Object System.Collections.ArrayList
$script:Checks = 0

function Write-Log([string]$text) {
    $line = ("[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $text)
    Write-Host $line
    Add-Content -LiteralPath $LogFile -Value $line -Encoding UTF8
}

function Assert-True([bool]$condition, [string]$message) {
    $script:Checks++
    if ($condition) {
        Write-Log ("PASS  " + $message)
    } else {
        Write-Log ("FAIL  " + $message)
        [void]$script:Failures.Add($message)
    }
}

function Read-Utf8([string]$path) {
    return [System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8)
}

function Get-Sha256([string]$path) {
    return (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
}

function ConvertTo-CanonicalJson($value) {
    if ($null -eq $value) { return "null" }
    if ($value -is [bool]) { if ($value) { return "true" } else { return "false" } }
    if ($value -is [string]) { return (ConvertTo-Json -InputObject $value -Compress) }
    if ($value -is [System.Management.Automation.PSCustomObject]) {
        $parts = @()
        foreach ($prop in ($value.PSObject.Properties | Sort-Object -Property Name)) {
            $parts += (ConvertTo-Json -InputObject $prop.Name -Compress) + ":" + (ConvertTo-CanonicalJson $prop.Value)
        }
        return "{" + ($parts -join ",") + "}"
    }
    if ($value -is [System.Collections.IEnumerable]) {
        $items = @()
        foreach ($item in $value) { $items += (ConvertTo-CanonicalJson $item) }
        return "[" + ($items -join ",") + "]"
    }
    return [string]$value
}

function Invoke-Setup([string]$dir, [string]$tag) {
    $innoLog = Join-Path $WorkDir ("inno-" + $tag + ".log")
    $args = @(
        "/VERYSILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/NOCANCEL",
        "/SP-",
        "/CLOSEAPPLICATIONS",
        "/DIR=`"$dir`"",
        "/LOG=`"$innoLog`""
    )
    $proc = Start-Process -FilePath $SetupExe -ArgumentList $args -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        Write-Log ("setup failed in " + $tag + " with exit code " + $proc.ExitCode + "; inno log: " + $innoLog)
    }
    return $proc.ExitCode
}

function Invoke-Uninstaller([string]$dir, [string]$tag) {
    $unins = Join-Path $dir "unins000.exe"
    if (-not (Test-Path $unins)) { return -1 }
    $args = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/NOCANCEL")
    $proc = Start-Process -FilePath $unins -ArgumentList $args -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        Write-Log ("uninstall failed in " + $tag + " with exit code " + $proc.ExitCode)
    }
    return $proc.ExitCode
}

Write-Log ("setup   : " + $SetupExe)
Write-Log ("workdir : " + $WorkDir)
Write-Log ("default : " + $defaultConfig)
Write-Log ("help    : " + $configHelp)
Write-Log ("log     : " + $LogFile)

# ---------------------------------------------------------------- S1
Write-Log "--- S1 clean silent install ---"
$s1 = Join-Path $WorkDir "s1"
New-Item -ItemType Directory -Force $s1 | Out-Null
$code = Invoke-Setup $s1 "s1"
Assert-True ($code -eq 0) "S1 setup exit code is 0"

$exe = Join-Path $s1 "Model-Gate.exe"
$cfg = Join-Path $s1 "bridge.config.json"
$help = Join-Path $s1 "config-help.html"
$example = Join-Path $s1 "bridge.config.example.json"
$guide = Join-Path $s1 "CONFIG-GUIDE.md"
$s1Db = Join-Path $s1 "data\modelgate.db"
Assert-True (Test-Path $exe) "S1 Model-Gate.exe installed"
Assert-True (Test-Path $cfg) "S1 bridge.config.json installed"
Assert-True (-not (Test-Path $help)) "S1 standalone config help page is not shipped (doc lives in the panel dialog)"
Assert-True (-not (Test-Path $example)) "S1 advanced template is not shipped"
Assert-True (-not (Test-Path $guide)) "S1 markdown guide is not shipped"
Assert-True (-not (Test-Path $s1Db)) "S1 installer ships no database"

if (Test-Path $cfg) {
    Assert-True ((Get-Sha256 $cfg) -eq (Get-Sha256 $defaultConfig)) "S1 installed config is byte-identical to default_config.json"
    $raw = Read-Utf8 $cfg
    $obj = $null
    $parsed = $true
    try { $obj = $raw | ConvertFrom-Json } catch { $parsed = $false }
    Assert-True $parsed "S1 installed config parses as JSON"
    if ($parsed) {
        Assert-True ($null -ne $obj._doc) "S1 installed config has _doc"
        Assert-True ($null -ne $obj.upstreams.zen) "S1 installed config has the zen upstream"
        Assert-True (-not ($raw -match '"apiKey"\s*:\s*"[^"]+')) "S1 installed config has no plaintext apiKey"
    }
}

# ---------------------------------------------------------------- S2
Write-Log "--- S2 pre-existing config and database are not overwritten ---"
$s2 = Join-Path $WorkDir "s2"
New-Item -ItemType Directory -Force $s2 | Out-Null
$s2Cfg = Join-Path $s2 "bridge.config.json"
$s2Data = Join-Path $s2 "data"
New-Item -ItemType Directory -Force $s2Data | Out-Null
$s2Db = Join-Path $s2Data "modelgate.db"
$custom = @'
{
  "_doc": "custom user config written before install",
  "port": 8999,
  "host": "127.0.0.1",
  "defaultModel": "mimo-v2.6-flash-free",
  "models": ["mimo-v2.6-flash-free"],
  "upstreams": { "zen": { "protocol": "openai", "baseUrl": "https://opencode.ai/zen/v1", "default": true } }
}
'@
[System.IO.File]::WriteAllText($s2Cfg, $custom, (New-Object System.Text.UTF8Encoding($false)))
[System.IO.File]::WriteAllText($s2Db, "sentinel-db-before-install", (New-Object System.Text.UTF8Encoding($false)))
$beforeHash = Get-Sha256 $s2Cfg
$beforeDbHash = Get-Sha256 $s2Db
$code = Invoke-Setup $s2 "s2"
Assert-True ($code -eq 0) "S2 setup exit code is 0"
Assert-True (Test-Path $s2Cfg) "S2 pre-existing config still exists"
if (Test-Path $s2Cfg) {
    Assert-True ((Get-Sha256 $s2Cfg) -eq $beforeHash) "S2 pre-existing config hash unchanged after install"
    Assert-True ((Read-Utf8 $s2Cfg) -match "custom user config") "S2 pre-existing config content preserved"
}
Assert-True (Test-Path $s2Db) "S2 pre-existing database still exists"
if (Test-Path $s2Db) {
    Assert-True ((Get-Sha256 $s2Db) -eq $beforeDbHash) "S2 pre-existing database hash unchanged after install"
    Assert-True ((Read-Utf8 $s2Db) -match "sentinel-db-before-install") "S2 pre-existing database content preserved"
}
Assert-True (Test-Path (Join-Path $s2 "Model-Gate.exe")) "S2 Model-Gate.exe installed"
Assert-True (-not (Test-Path (Join-Path $s2 "config-help.html"))) "S2 no standalone config help page is installed"

# ---------------------------------------------------------------- S3
Write-Log "--- S3 upgrade keeps the existing config and database ---"
$edited = $custom.Replace('"port": 8999', '"port": 8998')
[System.IO.File]::WriteAllText($s2Cfg, $edited, (New-Object System.Text.UTF8Encoding($false)))
$upgradeHash = Get-Sha256 $s2Cfg
$code = Invoke-Setup $s2 "s3-upgrade"
Assert-True ($code -eq 0) "S3 upgrade setup exit code is 0"
Assert-True (Test-Path (Join-Path $s2 "Model-Gate.exe")) "S3 exe present after upgrade"
Assert-True (-not (Test-Path (Join-Path $s2 "config-help.html"))) "S3 no standalone config help page after upgrade"
if (Test-Path $s2Cfg) {
    Assert-True ((Get-Sha256 $s2Cfg) -eq $upgradeHash) "S3 config hash unchanged after upgrade install"
    Assert-True ((Read-Utf8 $s2Cfg) -match '"port": 8998') "S3 user edit still in place after upgrade"
}
if (Test-Path $s2Db) {
    Assert-True ((Get-Sha256 $s2Db) -eq $beforeDbHash) "S3 database hash unchanged after upgrade install"
    Assert-True ((Read-Utf8 $s2Db) -match "sentinel-db-before-install") "S3 database content still the user one after upgrade"
}

# ---------------------------------------------------------------- S4
Write-Log "--- S4 uninstall keeps config and data ---"
$code = Invoke-Uninstaller $s2 "s4"
Assert-True ($code -eq 0) "S4 uninstall exit code is 0"
Assert-True (Test-Path $s2Cfg) "S4 bridge.config.json survives uninstall"
Assert-True (Test-Path $s2Db) "S4 data\modelgate.db survives uninstall"
if (Test-Path $s2Cfg) {
    Assert-True ((Read-Utf8 $s2Cfg) -match '"port": 8998') "S4 config content still the user version"
}

# ---------------------------------------------------------------- S5
Write-Log "--- S5 portable first run generates the default config ---"
$s5 = Join-Path $WorkDir "s5"
New-Item -ItemType Directory -Force $s5 | Out-Null
Copy-Item -LiteralPath $portableExe -Destination (Join-Path $s5 "Model-Gate-rust.exe") -Force
$s5Cfg = Join-Path $s5 "bridge.config.json"
$s5Db = Join-Path $s5 "data\modelgate.db"
$port = 8971
$proc = Start-Process -FilePath (Join-Path $s5 "Model-Gate-rust.exe") `
    -ArgumentList @("--no-gui", "--no-browser", "--force-multi", "--port", "$port", "--config", "`"$s5Cfg`"", "--db", "`"$s5Db`"") `
    -WorkingDirectory $s5 -PassThru
$deadline = (Get-Date).AddSeconds(20)
while (-not (Test-Path $s5Cfg) -and (Get-Date) -lt $deadline -and -not $proc.HasExited) {
    Start-Sleep -Milliseconds 300
}
Start-Sleep -Milliseconds 500
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
Assert-True (Test-Path $s5Cfg) "S5 portable first run created bridge.config.json"
if (Test-Path $s5Cfg) {
    $gen = $null
    $genOk = $true
    try { $gen = (Read-Utf8 $s5Cfg) | ConvertFrom-Json } catch { $genOk = $false }
    Assert-True $genOk "S5 generated config parses as JSON"
    if ($genOk) {
        $ref = (Read-Utf8 $defaultConfig) | ConvertFrom-Json
        Assert-True ((ConvertTo-CanonicalJson $gen) -eq (ConvertTo-CanonicalJson $ref)) "S5 generated config equals default_config.json"
        Assert-True ($null -ne $gen._doc) "S5 generated config has _doc"
        Assert-True ($null -ne $gen.upstreams.zen) "S5 generated config has the zen upstream"
    }
}

# ---------------------------------------------------------------- summary
Write-Log ("checks: " + $script:Checks + ", failures: " + $script:Failures.Count)
if ($script:Failures.Count -gt 0) {
    foreach ($f in $script:Failures) { Write-Log ("failed: " + $f) }
    Write-Log "RESULT: FAILED"
    exit 1
}
Write-Log "RESULT: ALL PASSED"
exit 0
