# -*- coding: utf-8 -*-
"""只输出凭据文件的字段名与值长度，绝不打印明文。"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def describe(label: str, path: Path, key_field: str = "apiKey") -> None:
    print(f"\n=== {label} ===")
    print(f"路径: {path}")
    if not path.is_file():
        print("状态: 不存在")
        return
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        print(f"状态: 解析失败 {type(exc).__name__}")
        return
    if not isinstance(data, dict):
        print(f"状态: 顶层是 {type(data).__name__}，不是对象")
        return
    print(f"状态: 存在，字段 {len(data)} 个")
    for name in sorted(data):
        value = data[name]
        if isinstance(value, str):
            hint = f"<字符串 {len(value)} 字符>"
        elif isinstance(value, (dict, list)):
            hint = f"<{type(value).__name__} {len(value)} 项>"
        else:
            hint = f"<{type(value).__name__}>"
        marker = "  ← 就是它" if name == key_field else ""
        print(f"  {name}: {hint}{marker}")


def main() -> int:
    home = Path.home()
    describe("commandcode 凭据文件", home / ".commandcode" / "auth.json")
    describe("codex 风格凭据文件", home / ".jcode" / "auth.json")
    describe("env 文件（KEY=VALUE 文本）", home / ".jcode" / ".env")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
