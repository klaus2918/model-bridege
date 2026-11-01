# 为本仓库的 Rust 构建启用 LLD 链接器（Windows / MSVC）。
#
# 为什么要这一步：MSVC 自带的 link.exe 在 10 MB 级二进制上明显慢于 LLD，
# 实测 model-gate release 链接 41 s -> 32 s。Rust 工具链自带 rust-lld.exe，
# 复制成 lld-link.exe 就是现成的 COFF 驱动，不需要额外装 LLVM。
#
# 做法：生成 rust/model-gate/.cargo/config.toml（已在 .gitignore 中）。
# 没跑过本脚本时仓库里没有该文件，构建走默认 link.exe，一切照常。
# CI 里同样先跑一次本脚本再 cargo build，保证本地/CI 链接器一致。
#
# 用法：
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\enable-lld.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\enable-lld.ps1 -Off   # 撤销
param(
    [switch]$Off
)
$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$crateDir = Join-Path $root "rust\model-gate"
$cargoDir = Join-Path $crateDir ".cargo"
$configPath = Join-Path $cargoDir "config.toml"

if ($Off) {
    if (Test-Path $configPath) {
        Remove-Item $configPath -Force
        Write-Host "已移除 $configPath，链接器回到默认 link.exe"
    } else {
        Write-Host "本就没有 $configPath"
    }
    return
}

# 1) 从当前工具链取 rust-lld.exe，副本命名成 lld-link.exe（LLD 按文件名选 COFF 驱动）。
$sysroot = (& rustc --print sysroot).Trim()
if ($LASTEXITCODE -ne 0 -or -not $sysroot) { throw "取不到 rustc sysroot，先确认 rustc 在 PATH 上" }
$rustLld = Join-Path $sysroot "lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
if (-not (Test-Path $rustLld)) {
    throw "找不到 $rustLld（非 Windows MSVC 目标或工具链不完整）"
}

$binDir = Join-Path $env:LOCALAPPDATA "model-gate-build"
New-Item -ItemType Directory -Force $binDir | Out-Null
$lldLink = Join-Path $binDir "lld-link.exe"
Copy-Item $rustLld $lldLink -Force

# 2) 写 .cargo/config.toml（路径按本机生成，所以不进版本库）。
New-Item -ItemType Directory -Force $cargoDir | Out-Null
$escaped = $lldLink.Replace("\", "\\")
$config = @"
# 由 scripts/enable-lld.ps1 生成，勿手工维护（不进版本库）。
[target.x86_64-pc-windows-msvc]
linker = "$escaped"
rustflags = ["-C", "link-arg=/OPT:REF", "-C", "link-arg=/OPT:ICF"]
"@
[System.IO.File]::WriteAllText($configPath, $config + "`n", (New-Object System.Text.UTF8Encoding($false)))

# 3) 自检：能链接成功才算数。
Write-Host "lld-link: $lldLink"
Write-Host "config:   $configPath"
Push-Location $crateDir
try {
    # 注意：不要用 2>&1 —— PS 5.1 在 $ErrorActionPreference=Stop 下会把 cargo 的
    # 第一行 stderr 当成终止性错误，构建明明是好的也会抛。
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "启用 LLD 后构建失败，用 -Off 回滚" }
} finally {
    Pop-Location
}
Write-Host "完成：后续 cargo build 走 LLD。"
