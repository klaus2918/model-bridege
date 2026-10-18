# Locate ISCC.exe (Inno Setup compiler). ASCII-only.
$ErrorActionPreference = "Continue"
$candidates = @(
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe",
    "C:\Program Files (x86)\Inno Setup 5\ISCC.exe",
    "C:\Program Files (x86)\Inno Setup 4\ISCC.exe",
    "C:\Program Files (x86)\Inno Setup 6\iscc.exe"
)
$found = ""
foreach ($c in $candidates) {
    if (Test-Path $c) { $found = $c; break }
}
if (-not $found) {
    $cmd = Get-Command iscc -ErrorAction SilentlyContinue
    if ($cmd) { $found = $cmd.Source }
}
if (-not $found) {
    # last resort: scan Program Files trees (shallow)
    foreach ($base in @("C:\Program Files (x86)", "C:\Program Files")) {
        if (-not (Test-Path $base)) { continue }
        $hit = Get-ChildItem -Path $base -Filter "ISCC.exe" -Recurse -Depth 3 `
            -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($hit) { $found = $hit.FullName; break }
    }
}
if ($found) {
    Write-Output ("ISCC_FOUND=" + $found)
    exit 0
}
Write-Output "ISCC_MISSING"
exit 1
