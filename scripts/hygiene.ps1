<#
.SYNOPSIS
  Model-Gate 噪音卫生：清理不该入库的中间产物，并报告游离文件。
.DESCRIPTION
  只按白名单模式清理，避免误删：
    未跟踪：*.b64.png、debug*.js|ps1|png、*.stackdump、*.out、*.err、*.bak、*.tmp
    已跟踪：同上（命中即 git rm，删文件并暂存删除）
  其余可疑项（根目录游离的 .ps1/.js）只报告不动。
.PARAMETER ReportOnly
  只列出命中项，不删除。
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\hygiene.ps1 -ReportOnly
#>
param([switch]$ReportOnly)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

# 已知噪音模式：命中即清理（白名单，宁窄不宽）
$NoiseRe = '\.b64\.png$|(^|/)debug[a-z0-9_-]*\.(js|ps1|png)$|\.stackdump$|\.(out|err|bak|tmp)$'

$actions = New-Object System.Collections.Generic.List[string]

# 1) 已跟踪的噪音：删文件 + 暂存删除
foreach ($file in (git ls-files)) {
    if ($file -notmatch $NoiseRe) { continue }
    if (-not (Test-Path -LiteralPath $file)) { continue }
    if ($ReportOnly) {
        $actions.Add("已跟踪(仅报告): $file")
    } else {
        git rm -q -f -- $file
        $actions.Add("已跟踪(已删): $file")
    }
}

# 2) 未跟踪的噪音：直接删
$untracked = git status --porcelain -uall |
    Where-Object { $_ -match '^\?\? ' } |
    ForEach-Object { $_.Substring(3).Trim('"') }
foreach ($file in $untracked) {
    if ($file -notmatch $NoiseRe) { continue }
    if ($ReportOnly) {
        $actions.Add("未跟踪(仅报告): $file")
    } else {
        Remove-Item -LiteralPath $file -Force -ErrorAction SilentlyContinue
        $actions.Add("未跟踪(已删): $file")
    }
}

# 3) 根目录游离脚本：只报告，交给人工判断
$stray = Get-ChildItem -Path (Join-Path $Root '*') -File -Include *.ps1, *.js -ErrorAction SilentlyContinue

$label = if ($ReportOnly) { "噪音检查" } else { "噪音清理" }
Write-Host ("{0}：{1} 项" -f $label, $actions.Count)
foreach ($a in $actions) { Write-Host ("  - " + $a) }
foreach ($s in $stray) { Write-Host ("  ! 根目录游离文件，未处理: " + $s.Name) }

if ($ReportOnly -and $actions.Count -gt 0) { exit 1 }
exit 0
