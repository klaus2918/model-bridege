<#
.SYNOPSIS
  Model-Gate 发布诊断：查（或等）某次提交的 release 流水线结果，失败时一句话给出原因。
.DESCRIPTION
  token 来源：GITHUB_TOKEN 环境变量 → 本机 git 凭据（git credential fill，非交互）。
  有 token 时直接抓失败步骤日志里的 [FAIL]/::error:: 行；无 token 时退回公开注解接口。
.PARAMETER Sha
  要查询的提交（默认 HEAD）。
.PARAMETER Wait
  未完成时轮询直到有结论（默认只查一次）。
.PARAMETER TimeoutMinutes
  -Wait 的最长等待时间，默认 25 分钟。
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship-diagnose.ps1
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship-diagnose.ps1 -Wait
#>
param(
    [string]$Sha = "HEAD",
    [switch]$Wait,
    [int]$TimeoutMinutes = 25
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

$Headers = @{ "User-Agent" = "model-gate-ship-diagnose"; "Accept" = "application/vnd.github+json" }

function Get-Token {
    if ($env:GITHUB_TOKEN) { return $env:GITHUB_TOKEN }
    $env:GCM_INTERACTIVE = "never"
    $env:GIT_TERMINAL_PROMPT = "0"
    try {
        $resp = "protocol=https`nhost=github.com`n`n" | & git credential fill 2>$null
        $line = $resp | Where-Object { $_ -like "password=*" } | Select-Object -First 1
        if ($line) { return $line.Substring("password=".Length).Trim() }
    } catch { }
    return ""
}

function Invoke-Api([string]$path) {
    return Invoke-RestMethod -Uri ("https://api.github.com" + $path) -Headers $Headers -TimeoutSec 30
}

$origin = (& git remote get-url origin).Trim()
if ($origin -notmatch 'github\.com[:/](?<owner>[^/]+)/(?<repo>[^/.]+)') {
    Write-Host "无法从 origin 解析 owner/repo：$origin"
    exit 2
}
$Owner = $Matches.owner
$Repo = $Matches.repo

$sha = (& git rev-parse $Sha).Trim()
$token = Get-Token
$auth = $token -ne ""
if ($auth) { $Headers["Authorization"] = "Bearer $token" }

Write-Host ("仓库 {0}/{1}，提交 {2}，鉴权 {3}" -f $Owner, $Repo, $sha.Substring(0, 7), $(if ($auth) { "token" } else { "匿名（配额 60/h）" }))

$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
while ($true) {
    $run = $null
    try {
        $list = Invoke-Api ("/repos/{0}/{1}/actions/runs?head_sha={2}&per_page=1" -f $Owner, $Repo, $sha)
        if ($list.workflow_runs.Count -gt 0) { $run = $list.workflow_runs[0] }
    } catch {
        Write-Host "查询失败：$($_.Exception.Message)"
        if (-not $Wait) { exit 3 }
    }

    if (-not $run) {
        Write-Host "该提交没有触发流水线（release-rust.yml 只在推 tag 时运行；推 tag 后再查）"
        exit 0
    }

    if ($run.status -eq "completed") { break }

    if (-not $Wait) {
        Write-Host ("流水线运行中：run {0} {1}" -f $run.id, $run.html_url)
        exit 0
    }
    if ((Get-Date) -gt $deadline) {
        Write-Host ("等待超时：run {0} 仍未结束 {1}" -f $run.id, $run.html_url)
        exit 4
    }
    Start-Sleep -Seconds 30
}

if ($run.conclusion -eq "success") {
    Write-Host ("CI 通过：run {0} {1}" -f $run.id, $run.html_url)
    exit 0
}

# ── 失败：定位步骤并抽出关键行 ────────────────────────────
Write-Host ("CI 失败（{0}）：run {1} {2}" -f $run.conclusion, $run.id, $run.html_url)

$failedJobs = @()
try {
    $jobs = Invoke-Api ("/repos/{0}/{1}/actions/runs/{2}/jobs" -f $Owner, $Repo, $run.id)
    foreach ($job in $jobs.jobs) {
        $failedSteps = @($job.steps | Where-Object { $_.conclusion -eq "failure" })
        if ($failedSteps.Count -gt 0) {
            $failedJobs += [pscustomobject]@{ Id = $job.id; Steps = $failedSteps }
            Write-Host ("失败步骤：{0}" -f (($failedSteps | ForEach-Object { $_.name }) -join "、"))
        }
    }
} catch { Write-Host "读取作业列表失败：$($_.Exception.Message)" }

$hint = @()
if ($auth) {
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    foreach ($job in $failedJobs) {
        if (-not $curl) { break }
        try {
            $logFile = Join-Path $env:TEMP ("mg-ci-log-" + [guid]::NewGuid().ToString("N") + ".log")
            # 日志接口是 302 跳到临时 blob，Invoke-WebRequest 在跳转上会异常；本机 schannel 需关闭吊销检查
            & $curl.Source -sSL --ssl-no-revoke `
                -H "Authorization: Bearer $token" -H "User-Agent: model-gate-ship-diagnose" `
                -o $logFile ("https://api.github.com/repos/{0}/{1}/actions/jobs/{2}/logs" -f $Owner, $Repo, $job.Id)
            if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath $logFile)) {
                $hint += (Get-Content -LiteralPath $logFile -Encoding UTF8) |
                    Where-Object { $_ -match '\[FAIL\]|\[fail\]|::error::|失败项|\[FATAL\]|panicked at|error\[|结果：' } |
                    Select-Object -First 8
                Remove-Item -LiteralPath $logFile -Force -ErrorAction SilentlyContinue
            }
        } catch { Write-Host "下载日志失败：$($_.Exception.Message)" }
    }
}

if ($hint.Count -eq 0) {
    # 无 token 或日志里没有固定标记：读公开注解（workflow 里用 ::error:: 写的那几行）
    try {
        $checks = Invoke-Api ("/repos/{0}/{1}/check-suites/{2}/check-runs" -f $Owner, $Repo, $run.check_suite_id)
        foreach ($check in $checks.check_runs) {
            try {
                $ann = Invoke-Api ("/repos/{0}/{1}/check-runs/{2}/annotations" -f $Owner, $Repo, $check.id)
                foreach ($a in $ann) { $hint += ("[{0}] {1}" -f $a.annotation_level, $a.message) }
            } catch { }
        }
    } catch { }
}

if ($hint.Count -eq 0) {
    Write-Host "没抽到失败细节（日志需 token，注解也没有）；打开上面的 run 链接看该步骤输出"
    exit 1
}

Write-Host "关键行："
foreach ($line in ($hint | Select-Object -First 10)) { Write-Host ("  " + $line.Trim()) }
exit 1
