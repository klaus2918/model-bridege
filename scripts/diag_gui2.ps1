# 诊断脚本 v2：模拟「双击 exe」的真实场景 —— 由资源管理器/Shell 启动，
# 不带任何重定向、不带父控制台。用 WMI 轮询进程状态与退出码。
$ErrorActionPreference = 'Continue'
$exe = 'D:\githup-workspace\Model-Gate\dist\Model-Gate.exe'
$db  = 'D:\githup-workspace\Model-Gate\dist\data\probe_shell.db'

# 先清理可能的残留
Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# 用 ShellExecute 语义启动（等同双击），不继承当前控制台
$shell = New-Object -ComObject Shell.Application
$shell.ShellExecute($exe, '--port 8910 --db ' + $db, 'D:\githup-workspace\Model-Gate\dist', '', 1)

Start-Sleep -Seconds 10

$procs = Get-Process Model-Gate -ErrorAction SilentlyContinue
if ($procs) {
    Write-Output ("RESULT=RUNNING pid=" + ($procs.Id -join ','))
    $procs | Stop-Process -Force
} else {
    Write-Output 'RESULT=GONE (process exited on its own)'
}

Write-Output '--- modelgate.log tail ---'
$log = 'D:\githup-workspace\Model-Gate\dist\data\modelgate.log'
if (Test-Path $log) { Get-Content $log -Tail 8 } else { Write-Output '(no log)' }

Write-Output '--- port check 8910 ---'
$c = Get-NetTCPConnection -LocalPort 8910 -State Listen -ErrorAction SilentlyContinue
if ($c) { Write-Output 'LISTENING' } else { Write-Output 'NOT_LISTENING' }
