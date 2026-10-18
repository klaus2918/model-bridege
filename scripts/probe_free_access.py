# -*- coding: utf-8 -*-
"""免费模型可用性矩阵:目录 header 对比 + 端点抽查。

安全约束:本脚本只对「免费模型」(id 含 free,或 big-pickle)发起调用,
未列白名单的模型一律跳过,避免对任何付费模型发请求。

用法:py -3.11 scripts\\probe_free_access.py
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
SESSION = "probe-free-0000000000000000"
UA = "model-gate-probe/1.0"

FREE_OK = lambda mid: ("free" in mid.lower()) or mid == "big-pickle"  # noqa: E731


def hdr(*, key=None, session=False, ua=None):
    h = {"content-type": "application/json"}
    if ua:
        h["user-agent"] = ua
    if key:
        h["authorization"] = f"Bearer {key}"
    if session:
        h["x-opencode-session"] = SESSION
    return h


def get_ids(url, headers):
    try:
        r = httpx.get(url, headers=headers, timeout=30.0)
    except Exception as exc:  # noqa: BLE001
        return None, f"ERR {type(exc).__name__}: {exc}"
    if r.status_code >= 400:
        return None, f"HTTP {r.status_code} {r.text[:120]}"
    try:
        data = r.json()
    except Exception:  # noqa: BLE001
        return None, "非 JSON"
    rows = data.get("data") if isinstance(data, dict) else data
    rows = [x for x in (rows or []) if isinstance(x, dict)]
    return [str(x.get("id") or x.get("slug") or x.get("name") or "") for x in rows], None


def call(url, mid, *, key=None, session=False, ua=None):
    if not FREE_OK(mid):
        return "SKIP(非免费,拒绝测试)"
    h = hdr(key=key, session=session, ua=ua)
    payload = {"model": mid, "max_tokens": 16,
               "messages": [{"role": "user", "content": "ping"}]}
    try:
        r = httpx.post(url, headers=h, json=payload, timeout=60.0)
    except Exception as exc:  # noqa: BLE001
        return f"ERR {type(exc).__name__}: {exc}"
    return f"HTTP {r.status_code} {r.text[:200].replace(chr(10), ' ')}"


def main() -> int:
    print("===== A. 目录 header 矩阵(仅 GET,不产生调用费用)=====\n")
    zen_url = "https://opencode.ai/zen/v1/models"
    combos = [
        ("no-hdr", hdr()),
        ("ua", hdr(ua=UA)),
        ("session", hdr(session=True)),
        ("key", hdr(key=KEY)),
        ("key+session", hdr(key=KEY, session=True)),
        ("key+ua+session", hdr(key=KEY, session=True, ua=UA)),
    ]
    for label, h in combos:
        ids, err = get_ids(zen_url, h)
        if err:
            print(f"[zen/{label}] {err}")
            continue
        free = [i for i in ids if FREE_OK(i)]
        print(f"[zen/{label}] count={len(ids)} free={len(free)}")
        if len(ids) <= 15:
            print("   ids:", ", ".join(ids))
        else:
            print("   free ids:", ", ".join(free))

    print()
    for label, h in [("no-key", hdr()), ("key", hdr(key=KEY)),
                     ("key+session", hdr(key=KEY, session=True))]:
        ids, err = get_ids("https://opencode.ai/inference/v1/models", h)
        if err:
            print(f"[inference/{label}] {err}")
            continue
        free = [i for i in ids if FREE_OK(i)]
        print(f"[inference/{label}] count={len(ids)} free={len(free)}")
        if len(ids) <= 15:
            print("   ids:", ", ".join(ids))

    print()
    ids, err = get_ids("https://opencode.ai/inference/go/v1/models", hdr(key=KEY))
    if err:
        print(f"[inference-go/key] {err}")
    else:
        free = [i for i in ids if FREE_OK(i)]
        print(f"[inference-go/key] count={len(ids)} free={len(free)} free_ids={', '.join(free)}")

    print("\n===== B. 免费模型调用抽查(全部为免费模型)=====\n")
    INF_CHAT = "https://opencode.ai/inference/openai/v1/chat/completions"
    ZEN_CHAT = "https://opencode.ai/zen/v1/chat/completions"
    INFGO_CHAT = "https://opencode.ai/inference/go/openai/v1/chat/completions"
    ZENGO_CHAT = "https://opencode.ai/zen/go/v1/chat/completions"

    spots = [
        ("inference+key", INF_CHAT, "glm-5.3-flash-free", dict(key=KEY, ua=UA)),
        ("inference+key", INF_CHAT, "longcat-2.5-preview-free", dict(key=KEY, ua=UA)),
        ("inference+key", INF_CHAT, "cat-jam-free", dict(key=KEY, ua=UA)),
        ("inference无key", INF_CHAT, "glm-5.3-flash-free", dict(ua=UA)),
        ("zen+key", ZEN_CHAT, "glm-5.3-flash-free", dict(key=KEY, ua=UA)),
        ("zen+key+session", ZEN_CHAT, "glm-5.3-flash-free", dict(key=KEY, session=True, ua=UA)),
        ("inference-go+key", INFGO_CHAT, "longcat-2.5-preview-free", dict(key=KEY, session=True, ua=UA)),
        ("inference-go+key", INFGO_CHAT, "cat-jam-free", dict(key=KEY, session=True, ua=UA)),
        ("zen-go+key", ZENGO_CHAT, "cat-jam-free", dict(key=KEY, session=True, ua=UA)),
    ]
    for label, url, mid, kw in spots:
        print(f"[{label}] {mid}\n    -> {call(url, mid, **kw)}\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
