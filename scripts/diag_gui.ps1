# 一次性诊断脚本：以 GUI 模式启动 exe，捕获退出码与 stdout/stderr
$ErrorActionPreference = 'Continue'
$exe  = 'D:\githup-workspace\Model-Gate\dist\Model-Gate.exe'
$out  = 'D:\githup-workspace\Model-Gate\dist\gui3_out.txt'
$err  = 'D:\githup-workspace\Model-Gate\dist\gui3_err.txt'
$db   = 'D:\githup-workspace\Model-Gate\dist\data\probe_gui3.db'

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
