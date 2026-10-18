# -*- coding: utf-8 -*-
"""对代理全量端点打请求，找出返回 5xx 的那个（复现 500）。

用法：py -3.11 scripts\\probe_all.py http://127.0.0.1:8912
"""
from __future__ import annotations

import json
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8912"
BAD: list[str] = []


def call(method: str, path: str, payload=None, timeout: int = 8) -> tuple[int, str]:
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["content-type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def is_internal_error(status: int, body: str) -> bool:
    """只有 500 / server_error 才是服务端缺陷；502/503 是上游不可达的正确语义。"""
    if status == 500:
        return True
    return '"code": "internal_error"' in body or '"code":"internal_error"' in body


def show(method: str, path: str, payload=None) -> None:
    status, body = call(method, path, payload)
    if status == -1:
        print(f"[WARN] {method} {path}  客户端未拿到响应（上游/超时）")
        return
    if is_internal_error(status, body):
        BAD.append(f"{method} {path} -> {status} {body[:260]}")
    snippet = body.replace("\n", " ")[:150]
    print(f"[{status:4d}] {method} {path}  {snippet}")


def main() -> int:
    print(f"probing {BASE}\n")

    # 只读端点
    for p in ("/health", "/v1/models", "/ui/", "/ui/app.js", "/ui/style.css",
              "/api/info", "/api/config", "/api/upstreams",
              "/api/upstreams/health?timeout=3",
              "/api/usage/totals?range=24h", "/api/usage/totals",
              "/api/usage/summary?group_by=day&range=7d",
              "/api/usage/summary?group_by=model",
              "/api/usage/summary?group_by=upstream",
              "/api/usage/summary?group_by=hour",
              "/api/usage/list?limit=5&offset=0",
              "/api/usage/list?range=all&limit=5",
              "/api/usage/list?group_by=bogus",
              "/nope/v1/models", "/v1/embeddings"):
        show("GET", p)

    # 写入端点
    status, body = call("GET", "/api/config")
    cfg = {}
    try:
        cfg = json.loads(body).get("config", {})
    except Exception:
        pass
    show("POST", "/api/config", {"config": cfg})
    show("POST", "/api/config/reload", {})
    show("POST", "/api/config/reload", None)   # 无 body

    # 连通性自检：结构必须完整（新功能；上游名随配置而变，不断言具体名字）
    status, body = call("GET", "/api/upstreams/health?timeout=3")
    try:
        payload = json.loads(body)
        rows = payload.get("rows") or []
        missing = [r for r in rows if not r.get("message") or "hint" not in r]
        if status == 200 and rows and not missing:
            ok_count = sum(1 for r in rows if r.get("ok"))
            print(f"[PASS] /api/upstreams/health  rows={len(rows)} ok={ok_count}")
        else:
            BAD.append(f"/api/upstreams/health -> {status} rows={len(rows)} "
                       f"缺字段={len(missing)} {body[:200]}")
    except Exception as exc:  # noqa: BLE001
        BAD.append(f"/api/upstreams/health 解析失败: {exc} {body[:200]}")

    # 聊天：桩上游不存在时也应当是 502/4xx 而不是 500
    show("POST", "/v1/chat/completions",
         {"model": "deepseek-flash", "messages": [{"role": "user", "content": "hi"}]})
    show("POST", "/v1/chat/completions",
         {"model": "deepseek-flash", "stream": True,
          "messages": [{"role": "user", "content": "hi"}]})
    show("POST", "/v1/chat/completions", {})
    show("POST", "/v1/chat/completions",
         {"model": "mimo-v2.5-pro", "messages": [{"role": "user", "content": "hi"}]})
    show("POST", "/v1/chat/completions",
         {"model": "deepseek/deepseek-v4.1-flash",
          "messages": [{"role": "user", "content": "hi"}]})

    print()
    if BAD:
        print(f"BAD_ENDPOINTS={len(BAD)}")
        for line in BAD:
            print("  " + line)
        return 1
    print("ALL_ENDPOINTS_OK")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
