# -*- coding: utf-8 -*-
"""检查 ~/.jcode/.env 是否提供了上游密钥（只报存在性，不打印明文）。"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path

env_path = Path.home() / ".jcode" / ".env"
print(f"env file: {env_path}")
print(f"exists  : {env_path.is_file()}")

if not env_path.is_file():
    print("=> 没有 env 文件，这里是 key 缺失的直接原因")
    raise SystemExit(1)

text = env_path.read_text(encoding="utf-8", errors="replace")
found = {}
for line in text.splitlines():
    m = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$", line)
    if not m:
        continue
    key, value = m.group(1), m.group(2)
    if value:
        found[key] = len(value)

print(f"\n加载到的变量（{len(found)} 个，值均非空）：")
for name in sorted(found):
    print(f"  {name} = <{found[name]} chars>")

target = {"OPENCODE_API_KEY": "go", "COMMAND_CODE_API_KEY": "commandcode"}
print("\n=== 上游密钥就位情况 ===")
missing = []
for var, upstream in target.items():
    ok = var in found
    print(f"  {var:<24} -> 上游 {upstream:<12} {'OK' if ok else 'MISSING'}")
    if not ok:
        missing.append(var)

print(f"\n=== 进程环境变量（os.environ） ===")
for var in target:
    value = os.environ.get(var)
    print(f"  {var}: {'已设置' if value else '未设置'}")

if missing:
    print(f"\n=> 缺失：{', '.join(missing)}")
    raise SystemExit(1)
print("\n=> 密钥齐备")
raise SystemExit(0)
