# -*- coding: utf-8 -*-
"""对比 zen / inference / inference-go 三个端点的模型目录,找出免费模型分布。

用法:py -3.11 scripts\\probe_free_catalog.py
"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path

import httpx

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

ENV = Path.home() / ".jcode" / ".env"
if ENV.is_file():
    for line in ENV.read_text(encoding="utf-8", errors="replace").splitlines():
        m = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$", line)
        if m and m.group(1) not in os.environ and m.group(2):
            os.environ[m.group(1)] = m.group(2)
KEY = os.environ.get("OPENCODE_API_KEY", "")

URLS = [
    ("zen", "https://opencode.ai/zen/v1/models"),
    ("inference", "https://opencode.ai/inference/v1/models"),
    ("inference-go", "https://opencode.ai/inference/go/v1/models"),
]


def main() -> int:
    for name, url in URLS:
        headers = {"user-agent": "model-gate-probe/1.0"}
        if KEY:
            headers["authorization"] = f"Bearer {KEY}"
        try:
            res = httpx.get(url, headers=headers, timeout=30.0)
        except Exception as exc:  # noqa: BLE001
            print(f"\n[{name}] ERR {type(exc).__name__}: {exc}")
            continue
        print(f"\n[{name}] {url} -> HTTP {res.status_code}")
        if res.status_code >= 400:
            print("  " + res.text[:200].replace("\n", " "))
            continue
        try:
            data = res.json()
        except Exception:  # noqa: BLE001
            print("  非 JSON:", res.text[:200])
            continue
        rows = data.get("data") if isinstance(data, dict) else data
        rows = [r for r in (rows or []) if isinstance(r, dict)]
        ids = [str(r.get("id") or r.get("slug") or r.get("name") or "") for r in rows]
        print(f"  共 {len(ids)} 个模型")
        frees = [i for i in ids if "free" in i.lower() or i == "big-pickle"]
        print(f"  免费候选({len(frees)}):")
        for i in frees:
            print("    -", i)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
