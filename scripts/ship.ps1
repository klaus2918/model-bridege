<#
.SYNOPSIS
  Model-Gate 收尾单一入口：噪音卫生 → 门禁 → 提交 → 推送 →（可选）打 tag 发布。
.DESCRIPTION
  一次调用完成以前要多轮手工执行的收尾动作。任一门禁失败即中止，不会带病提交。
  门禁：噪音清理 / 提交信息格式 / 敏感信息扫描 / 文本兜底校验 / .ps1 的 UTF-8 BOM。
  成功后最后一行固定为：SHIPPED <sha> master[ + tag]。
.PARAMETER Message
  提交信息（含换行用 -MessageFile 更稳，避免命令行编码问题）。
.PARAMETER MessageFile
  UTF-8 提交信息文件路径；优先于 -Message。
.PARAMETER Tag
  提交后创建/移动该 tag 并推送（例如 v4.0.1），会触发 release-rust.yml。
.PARAMETER DryRun
  只跑到门禁为止，不提交、不推送。
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt -Tag v4.0.1
#>
param(
    [string]$Message = "",
    [string]$MessageFile = "",
    [string]$Tag = "",
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $Root

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$Utf8Bom = New-Object System.Text.UTF8Encoding($true)
$script:Problems = New-Object System.Collections.Generic.List[string]

function Step([string]$name) { Write-Host ""; Write-Host "== $name" }
function Good([string]$text) { Write-Host "[ok] $text" }
function Bad([string]$text) { Write-Host "[x] $text"; $script:Problems.Add($text) }

function Exit-WithProblems {
    if ($script:Problems.Count -eq 0) { return }
    Write-Host ""
    Write-Host ("门禁未通过（{0} 项）：" -f $script:Problems.Count)
    foreach ($p in $script:Problems) { Write-Host ("  - " + $p) }
    exit 1
}

# 调用原生命令：git 会把进度写到 stderr（如 "To https://..."），在 $ErrorActionPreference=Stop
# 下配合 2>&1 会被当作 NativeCommandError 直接终止脚本。原生调用一律走这里。
function Invoke-Native([string]$exe, [string[]]$argv) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $out = & $exe @argv 2>&1
        return [pscustomobject]@{ Output = @($out); Code = $LASTEXITCODE }
    } finally {
        $ErrorActionPreference = $saved
    }
}

# ── 依赖探测（不内置密钥、不跳过门禁）─────────────────────────
function Resolve-Python {
    if ($env:MG_PYTHON -and (Test-Path -LiteralPath $env:MG_PYTHON)) { return @($env:MG_PYTHON) }
    $c = Get-Command python -ErrorAction SilentlyContinue
    if ($c) { return @($c.Source) }
    $c = Get-Command py -ErrorAction SilentlyContinue
    if ($c) { return @($c.Source, '-3') }
    return @()
}
function Resolve-GateScript([string]$name) {
    $candidates = @()
    if ($env:MG_GATES) { $candidates += (Join-Path $env:MG_GATES $name) }
    $candidates += (Join-Path $Root "..\op-skills\skills\safe-git-write\scripts\$name")
    $candidates += (Join-Path $env:USERPROFILE ".dsh\skills\safe-git-write\scripts\$name")
    $candidates += "D:\githup-workspace\op-skills\skills\safe-git-write\scripts\$name"
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return (Resolve-Path -LiteralPath $c).Path }
    }
    return ""
}

$Python = Resolve-Python
$ScanScript = Resolve-GateScript "scan-sensitive.py"
$ValidateScript = Resolve-GateScript "validate-text.py"

# ── 1. 提交信息 ────────────────────────────────────────────
Step "提交信息"
if ($MessageFile) {
    if (-not (Test-Path -LiteralPath $MessageFile)) { throw "找不到提交信息文件：$MessageFile" }
    $msgText = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $MessageFile), [Text.Encoding]::UTF8)
} elseif ($Message) {
    $msgText = $Message
} else {
    throw "需要 -Message 或 -MessageFile"
}
$msgText = $msgText.TrimEnd()
if ($msgText.Contains([char]0xFFFD)) {
    throw "提交信息含 U+FFFD（编码已损坏），请改用 -MessageFile 传 UTF-8 文件"
}
$subject = ($msgText -split "\r?\n")[0].Trim()

$FormatRe = '^(feat|fix|refactor|docs|style|test|chore|perf|update)\([a-z0-9][a-z0-9._-]*\): 【[^】]{2,}】.+ @AI G$'
if ($subject -notmatch $FormatRe) {
    Bad "提交信息首行不符合规范：$subject"
    Write-Host "     期望：<type>(<scope>): 【<领域名词>】<title> @AI G"
} else {
    Good "格式合法：$subject"
}
if ($subject -match '【([^】]*)】' -and $Matches[1] -match '新增|修改|优化|更新|完善|调整|修复|重构|支持|增强|能力') {
    Bad "【】内出现动作词/评价词（应只放业务领域名词）：$($Matches[1])"
}

# ── 2. 噪音卫生 ────────────────────────────────────────────
Step "噪音卫生"
& (Join-Path $PSScriptRoot "hygiene.ps1") | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Bad "噪音卫生脚本异常退出（$LASTEXITCODE）" }

# ── 3. 暂存 ────────────────────────────────────────────────
Step "暂存改动"
git add -A
if ($LASTEXITCODE -ne 0) { throw "git add 失败" }
$staged = @(git diff --cached --name-only --diff-filter=ACMR)
$stagedDeleted = @(git diff --cached --name-only --diff-filter=D)
if ($staged.Count -eq 0 -and $stagedDeleted.Count -eq 0) {
    Write-Host "工作区没有改动，无需提交"
    exit 0
}
Good ("暂存 {0} 个文件（删除 {1} 个）" -f $staged.Count, $stagedDeleted.Count)

# ── 4. 编码闸门：中文 .ps1 必须带 UTF-8 BOM ────────────────
Step "编码闸门"
$textExt = @('.md', '.json', '.js', '.ts', '.vue', '.css', '.html', '.yml', '.yaml', '.toml', '.ps1', '.rs', '.txt', '.lock')
$textFiles = @($staged | Where-Object { $textExt -contains [IO.Path]::GetExtension($_).ToLower() })
foreach ($file in $staged) {
    if (-not (Test-Path -LiteralPath $file)) { continue }
    $bytes = [IO.File]::ReadAllBytes($file)
    if ($bytes.Length -ge 2 -and (($bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) -or ($bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF))) {
        Bad "UTF-16 编码文件（应为 UTF-8）：$file"
        continue
    }
    if ($textExt -notcontains [IO.Path]::GetExtension($file).ToLower()) { continue }
    $text = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $file), [Text.Encoding]::UTF8)
    if ($text.Contains([char]0xFFFD)) { Bad "含 U+FFFD 乱码：$file"; continue }
    $hasBom = ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)
    if (-not $hasBom -and $file -match '\.ps1$' -and $text -match '[^\x00-\x7F]') {
        # PS 5.1 用 -File 运行无 BOM 的 UTF-8 脚本会按 GBK 解码，中文全乱；这类文件自动补 BOM
        [IO.File]::WriteAllText((Resolve-Path -LiteralPath $file), $text, $Utf8Bom)
        git add -- $file
        Write-Host "  补 UTF-8 BOM：$file"
    }
}
Good ("编码检查完成（文本 {0} 个）" -f $textFiles.Count)

# ── 5. 敏感信息扫描 ────────────────────────────────────────
Step "敏感信息扫描"
if ($Python.Count -eq 0) {
    Bad "找不到 Python，无法运行敏感信息扫描（可设 MG_PYTHON 指定解释器）"
} elseif (-not $ScanScript) {
    Bad "找不到 scan-sensitive.py（可设 MG_GATES 指向 safe-git-write\scripts 目录）"
} else {
    $py = $Python[0]
    $pyPre = @()
    if ($Python.Count -gt 1) { $pyPre = $Python[1..($Python.Count - 1)] }
    & $py @pyPre -X utf8 $ScanScript
    if ($LASTEXITCODE -ne 0) { Bad "敏感信息扫描命中阻断级规则（见上）" } else { Good "敏感信息扫描通过" }
}

# ── 6. 文本兜底校验 ────────────────────────────────────────
Step "文本兜底校验"
if ($Python.Count -eq 0 -or -not $ValidateScript) {
    Bad "缺少 validate-text.py 或 Python，文本兜底校验无法执行"
} else {
    $targets = @($textFiles | Where-Object { Test-Path -LiteralPath $_ })
    if ($targets.Count -eq 0) {
        Good "没有需要校验的文本文件"
    } else {
        $py = $Python[0]
        $pyPre = @()
        if ($Python.Count -gt 1) { $pyPre = $Python[1..($Python.Count - 1)] }
        & $py @pyPre -X utf8 $ValidateScript @targets
        if ($LASTEXITCODE -ne 0) { Bad "文本兜底校验失败（见上）" } else { Good ("文本兜底校验通过（{0} 个）" -f $targets.Count) }
    }
}

Exit-WithProblems

if ($DryRun) {
    Write-Host ""
    Write-Host "DRY-RUN：门禁通过，未提交"
    exit 0
}

# ── 7. 提交 ────────────────────────────────────────────────
Step "提交"
$msgPath = Join-Path $env:TEMP ("mg-commit-" + [guid]::NewGuid().ToString("N") + ".txt")
[IO.File]::WriteAllText($msgPath, $msgText + "`n", $Utf8NoBom)
try {
    git -c i18n.commitEncoding=UTF-8 commit -F $msgPath | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "git commit 失败（$LASTEXITCODE）" }
} finally {
    Remove-Item -LiteralPath $msgPath -Force -ErrorAction SilentlyContinue
}
$sha = (git rev-parse --short HEAD).Trim()
Good "已提交 $sha"

# ── 8. 推送 ────────────────────────────────────────────────
Step "推送"
$pushed = $false
for ($i = 1; $i -le 6; $i++) {
    $r = Invoke-Native "git" @("-c", "http.version=HTTP/1.1", "push", "origin", "HEAD")
    if ($r.Code -eq 0) { $pushed = $true; break }
    Write-Host ("  第 {0} 次推送失败：{1}" -f $i, ($r.Output -join " "))
    if ($i -lt 6) { Start-Sleep -Seconds 6 }
}
if (-not $pushed) { throw "推送失败（重试 6 次）" }
Good "已推送 origin"

# ── 9. tag（可选，触发发布）───────────────────────────────
$tagOut = ""
if ($Tag) {
    Step "打 tag 并发布：$Tag"
    if ($Tag -notmatch '^v\d+\.\d+\.\d+$') { throw "tag 必须形如 v4.0.1：$Tag" }
    $cargoVersion = (Select-String -Path "rust/model-gate/Cargo.toml" -Pattern '^\s*version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches.Groups[1].Value
    if ($Tag -ne "v$cargoVersion") {
        throw "tag $Tag 与 Cargo.toml 版本 v$cargoVersion 不一致（版本唯一源在 Cargo.toml）"
    }
    $tagResult = Invoke-Native "git" @("tag", "-f", $Tag, $sha)
    if ($tagResult.Code -ne 0) { throw ("创建 tag 失败：" + ($tagResult.Output -join " ")) }
    $tagPushed = $false
    for ($i = 1; $i -le 6; $i++) {
        $r = Invoke-Native "git" @("-c", "http.version=HTTP/1.1", "push", "--force", "origin", "refs/tags/$Tag")
        if ($r.Code -eq 0) { $tagPushed = $true; break }
        Write-Host ("  第 {0} 次 tag 推送失败：{1}" -f $i, ($r.Output -join " "))
        if ($i -lt 6) { Start-Sleep -Seconds 6 }
    }
    if (-not $tagPushed) { throw "tag 推送失败（重试 6 次）" }
    Good "tag $Tag 已推送，发布流水线已触发"
    $tagOut = " + $Tag"
}

Write-Host ""
Write-Host ("SHIPPED {0} master{1}" -f $sha, $tagOut)
exit 0
