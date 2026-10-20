# Rust 面板接口冒烟：启动 --no-gui 实例，检查 /api/info、/api/update/status 与 /ui/ 资产。
# 用法：
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\panel_api_smoke.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\panel_api_smoke.ps1 -Exe dist\Model-Gate-rust.exe -Port 18941
param(
    [string]$Exe = "",
    [int]$Port = 18941
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $Root "dist\Model-Gate-rust.exe" }
if (-not (Test-Path -LiteralPath $Exe)) { throw "找不到待测 exe：$Exe" }

$TempDir = Join-Path ([IO.Path]::GetTempPath()) ("mg-panel-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
$ConfigPath = Join-Path $TempDir "bridge.config.json"
$DbPath = Join-Path $TempDir "data\modelgate.db"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$Config = @{
    port = $Port
    host = "127.0.0.1"
    defaultModel = "m"
    models = @("m")
    upstreams = @{
        oai = @{
            protocol = "openai"
            baseUrl = "http://127.0.0.1:1"
            apiKey = "k"
            models = @("m")
            default = $true
        }
    }
}
[IO.File]::WriteAllText($ConfigPath, ($Config | ConvertTo-Json -Depth 8), $Utf8NoBom)

$Base = "http://127.0.0.1:$Port"
$Failures = New-Object System.Collections.Generic.List[string]
$Process = $null

function Invoke-PanelRequest {
    param([string]$Path)
    try {
        $Response = Invoke-WebRequest -Uri ($Base + $Path) -UseBasicParsing -TimeoutSec 15
        return [pscustomobject]@{ Status = [int]$Response.StatusCode; Body = [string]$Response.Content }
    } catch {
        $Status = 0
        if ($_.Exception.Response) {
            $Status = [int]$_.Exception.Response.StatusCode
        }
        return [pscustomobject]@{ Status = $Status; Body = [string]$_.Exception.Message }
    }
}

function Assert-PanelCheck {
    param([string]$Name, [string]$Path, [scriptblock]$Validate)
    $Result = Invoke-PanelRequest -Path $Path
    if ($Result.Status -ne 200) {
        $Failures.Add("$Name -> HTTP $($Result.Status)")
        Write-Host "[FAIL] $Name ($Path) -> $($Result.Status)"
        return
    }
    try {
        & $Validate $Result.Body
        Write-Host "[ok] $Name ($Path)"
    } catch {
        $Failures.Add("$Name -> $($_.Exception.Message)")
        Write-Host "[FAIL] $Name ($Path) -> $($_.Exception.Message)"
    }
}

try {
    $Process = Start-Process -FilePath $Exe `
        -ArgumentList @("--no-gui", "--port", "$Port", "--config", $ConfigPath, "--db", $DbPath, "--force-multi") `
        -WorkingDirectory $Root -WindowStyle Hidden -PassThru

    $Ready = $false
    for ($i = 0; $i -lt 120; $i++) {
        if ($Process.HasExited) { break }
        $Health = Invoke-PanelRequest -Path "/health"
        if ($Health.Status -eq 200) { $Ready = $true; break }
        Start-Sleep -Milliseconds 250
    }
    if (-not $Ready) { throw "Rust 服务未就绪（port=$Port）" }

    Assert-PanelCheck "info" "/api/info" {
        param($Body)
        $Info = $Body | ConvertFrom-Json
        if (-not $Info.version) { throw "缺少 version" }
        if (-not $Info.port) { throw "缺少 port" }
    }

    Assert-PanelCheck "update-status" "/api/update/status" {
        param($Body)
        $Status = $Body | ConvertFrom-Json
        if (-not $Status.current) { throw "缺少 current" }
        if (-not $Status.phase) { throw "缺少 phase" }
        if ($null -eq $Status.install_supported) { throw "缺少 install_supported" }
    }

    Assert-PanelCheck "ui-index" "/ui/" {
        param($Body)
        if ($Body -notmatch 'id="update-btn"') { throw "首页缺少更新入口" }
    }

    Assert-PanelCheck "ui-app" "/ui/app.js" {
        param($Body)
        foreach ($Marker in @("/api/update/check", "/api/update/download", "/api/update/install")) {
            if ($Body -notmatch [regex]::Escape($Marker)) { throw "app.js 缺少 $Marker" }
        }
    }

    Assert-PanelCheck "ui-style" "/ui/style.css" {
        param($Body)
        if ($Body -notmatch '\.update-progress') { throw "style.css 缺少更新进度样式" }
    }
} finally {
    if ($Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
}

if ($Failures.Count -gt 0) {
    Write-Host ("失败项：" + ($Failures -join "；"))
    exit 1
}
Write-Host "面板接口与更新资源冒烟通过"
exit 0
