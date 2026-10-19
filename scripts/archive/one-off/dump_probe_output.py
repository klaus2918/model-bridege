# -*- coding: utf-8 -*-
"""打印冷启动/GUI 探测的输出文件（诊断用）。"""
import sys
from pathlib import Path

# 控制台是 GBK：先把 stdout 换成可替换错误的 UTF-8，避免诊断脚本自己崩
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.cwd()
for name in ("gui.out", "gui.err", "coldstart.out", "coldstart.err"):
    path = root / name
    if not path.exists():
        print(f"--- {name} (missing) ---")
        continue
    text = path.read_text(encoding="utf-8", errors="replace")
    if not text.strip():
        print(f"--- {name} (empty, {path.stat().st_size} bytes) ---")
        continue
    print(f"--- {name} ({path.stat().st_size} bytes) ---")
    lines = text.splitlines()
    for line in lines[-40:]:
        print("  " + line)
