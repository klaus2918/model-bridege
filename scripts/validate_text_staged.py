# -*- coding: utf-8 -*-
"""提交前兜底：暂存区文本文件 UTF-8 可解码、无 U+FFFD、无 BOM。"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

TEXT_EXT = {".py", ".js", ".css", ".html", ".md", ".json", ".yaml", ".yml",
            ".txt", ".ps1", ".cmd", ".bat", ".spec", ".iss", ".toml", ".cfg", ".ini"}
out = subprocess.run(["git", "diff", "--cached", "--name-only"], capture_output=True,
                     text=True, encoding="utf-8", errors="replace")
files = [f for f in out.stdout.splitlines() if f.strip()]
bad: list[str] = []
checked = 0
for name in files:
    p = Path(name)
    if p.suffix.lower() not in TEXT_EXT or not p.is_file():
        continue
    checked += 1
    raw = p.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        bad.append(f"{name}: UTF-8 解码失败 ({exc})")
        continue
    if "\ufffd" in text:
        bad.append(f"{name}: 含 U+FFFD 乱码 {text.count(chr(0xFFFD))} 处")
    if raw.startswith(b"\xef\xbb\xbf"):
        bad.append(f"{name}: 含 BOM")

print(f"checked={checked} files")
if bad:
    print("FAIL")
    for b in bad:
        print("  -", b)
    sys.exit(1)
print("VALIDATE_TEXT OK")
