# 诊断脚本 v3：用 wmic process call create 以「无控制台父进程」启动 GUI 模式 exe
$ErrorActionPreference = 'Continue'
$exe = 'D:\githup-workspace\Model-Gate\dist\Model-Gate.exe'
$db  = 'D:\githup-workspace\Model-Gate\dist\data\probe_shell.db'

Get-Process Model-Gate -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# WMIC create 返回 ProcessId 与 ReturnValue
$wmic = & wmic process call create "$exe --port 8910 --db $db" 2>&1 | Out-String
Write-Output '--- WMIC ---'
Write-Output $wmic

Start-Sleep -Seconds 10

$procs = Get-Process Model-Gate -ErrorAction SilentlyContinue
if ($procs) {
    Write-Output ("RESULT=RUNNING pid=" + ($procs.Id -join ','))
    $procs | Stop-Process -Force
} else {
    Write-Output 'RESULT=GONE'
}

Write-Output '--- modelgate.log tail ---'
$log = 'D:\githup-workspace\Model-Gate\dist\data\modelgate.log'
if (Test-Path $log) { Get-Content $log -Tail 10 } else { Write-Output '(no log)' }
