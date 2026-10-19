# -*- coding: utf-8 -*-
"""经本地网关(127.0.0.1:8900)验证免费模型端到端可用性。

约束:仅使用免费模型白名单(id 含 free / pickle),不触碰任何付费模型。
用法:py -3.11 scripts\\probe_gateway_free.py
"""
from __future__ import annotations

import json
import sys
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8900"
FREE_MODELS = [
    "longcat-2.5-preview-free",
    "cat-jam-free",
    "space-bunny-free",
    "step-5-preview-free",
]

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass


def get(path: str, retries: int = 6):
    last = None
    for _ in range(retries):
        try:
            with urllib.request.urlopen(BASE + path, timeout=8) as res:
                return res.status, res.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            return exc.code, exc.read().decode("utf-8", "replace")
        except Exception as exc:  # noqa: BLE001
            last = f"{type(exc).__name__}: {exc}"
            time.sleep(1)
    return -1, str(last)


def post_chat(model: str):
    payload = json.dumps({
        "model": model, "max_tokens": 24,
        "messages": [{"role": "user", "content": "只回复两个字:pong"}],
    }, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        BASE + "/v1/chat/completions", data=payload,
        headers={"content-type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=90) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def main() -> int:
    print("== [1] /health(go / zen 模型数)==\n")
    status, body = get("/health")
    if status == 200:
        try:
            info = json.loads(body)
            for up in info.get("upstreams", []):
                if up.get("name") in ("go", "zen"):
                    print(f"  {up['name']:<4} models={up.get('models')} "
                          f"modelsAll={up.get('modelsAll')}")
        except Exception as exc:  # noqa: BLE001
            print("  解析失败:", exc, body[:200])
    else:
        print(f"  [{status}] {body[:200]}")

    print("\n== [2] /v1/models(免费模型清单)==\n")
    status, body = get("/v1/models")
    if status == 200:
        try:
            data = json.loads(body)
            rows = data.get("data") or []
            ids = [str(r.get("id") or r.get("model") or "") for r in rows
                   if isinstance(r, dict)]
            free = [i for i in ids if "free" in i.lower() or "pickle" in i.lower()]
            print(f"  共 {len(ids)} 个模型;免费/隐身相关 {len(free)} 个:")
            for i in free:
                print("    -", i)
        except Exception as exc:  # noqa: BLE001
            print("  解析失败:", exc, body[:200])
    else:
        print(f"  [{status}] {body[:200]}")

    print("\n== [3] 经网关调用免费模型(端到端)==\n")
    for mid in FREE_MODELS:
        status, body = post_chat(mid)
        snippet = body[:200].replace("\n", " ")
        print(f"  [{status}] {mid}\n      {snippet}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
