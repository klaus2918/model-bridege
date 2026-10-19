# 一次性诊断脚本：以 GUI 模式启动 exe，捕获退出码与 stdout/stderr
# 从仓库根目录运行：powershell -File scripts\diag_gui.ps1
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$exe  = Join-Path $root 'dist\Model-Gate.exe'
$out  = Join-Path $root 'dist\gui3_out.txt'
$err  = Join-Path $root 'dist\gui3_err.txt'
$db   = Join-Path $root 'dist\data\probe_gui3.db'

$proc = Start-Process -FilePath $exe `
    -ArgumentList '--port','8904','--db',$db `
    -RedirectStandardOutput $out -RedirectStandardError $err `
    -PassThru

Start-Sleep -Seconds 8

$exited = $proc.HasExited
if ($exited) {
    Write-Output ("RESULT=EXITED code=" + $proc.ExitCode)
} else {
    Write-Output "RESULT=STILL_RUNNING (killing probe)"
    Stop-Process -Id $proc.Id -Force
}

Write-Output '--- STDOUT ---'
if (Test-Path $out) { Get-Content $out -Raw } else { Write-Output '(no stdout file)' }
Write-Output '--- STDERR ---'
if (Test-Path $err) { Get-Content $err -Raw } else { Write-Output '(no stderr file)' }
