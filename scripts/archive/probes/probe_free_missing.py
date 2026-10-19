# -*- coding: utf-8 -*-
"""补测官方文档列出、但尚未实测的免费模型(仅这 4 个,均为免费模型)。

  exo-free / mimo-v2.5-free / nemotron-3-ultra-free / big-pickle

用法:py -3.11 scripts\\probe_free_missing.py
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

MODELS = ["exo-free", "mimo-v2.5-free", "nemotron-3-ultra-free", "big-pickle"]
UA = "model-gate-probe/1.0"

TARGETS = [
    ("zen(匿名)", "https://opencode.ai/zen/v1/chat/completions", None),
    ("inference(带key)", "https://opencode.ai/inference/openai/v1/chat/completions", KEY),
]


def main() -> int:
    for label, url, key in TARGETS:
        print(f"== {label} ==")
        for mid in MODELS:
            h = {"content-type": "application/json", "user-agent": UA}
            if key:
                h["authorization"] = f"Bearer {key}"
            payload = {"model": mid, "max_tokens": 16,
                       "messages": [{"role": "user", "content": "ping"}]}
            try:
                r = httpx.post(url, headers=h, json=payload, timeout=60.0)
                body = r.text[:220].replace("\n", " ")
                print(f"  [{r.status_code}] {mid}\n      {body}")
            except Exception as exc:  # noqa: BLE001
                print(f"  [ERR] {mid} {type(exc).__name__}: {exc}")
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
