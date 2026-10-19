# -*- coding: utf-8 -*-
"""查本机 command-code CLI 版本 vs npm registry 最新版本。"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import httpx

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from modelgate.config import command_code_roots  # noqa: E402


def main() -> int:
    print("=== 候选安装路径 ===")
    for root in command_code_roots():
        pkg = Path(root) / "package.json"
        if pkg.is_file():
            try:
                data = json.loads(pkg.read_text(encoding="utf-8"))
                print(f"  {pkg}")
                print(f"    name={data.get('name')} version={data.get('version')}")
            except Exception as exc:  # noqa: BLE001
                print(f"  {pkg} 解析失败 {exc}")
        else:
            print(f"  {root} (无 package.json)")

    print("\n=== npm registry 最新版本 ===")
    try:
        res = httpx.get("https://registry.npmjs.org/command-code/latest", timeout=15)
        if res.is_success:
            print(f"  latest = {(res.json() or {}).get('version')}")
        else:
            print(f"  HTTP {res.status_code}")
    except Exception as exc:  # noqa: BLE001
        print(f"  查询失败: {exc}")

    print("\n=== 上游要求 ===")
    print("  minVersion = 0.18.10（来自 403 upgrade_required 响应）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
