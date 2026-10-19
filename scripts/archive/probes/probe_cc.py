# -*- coding: utf-8 -*-
"""用**我们自己的适配器**构造真实上游请求并发出，验证代理实际发出的头部/载荷。

用法：py -3.11 scripts\\probe_cc.py [model]
"""
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

from modelgate.config import load_config, load_env_file, upstreams_of  # noqa: E402
from modelgate.protocols.base import Ctx  # noqa: E402
from modelgate.protocols.commandcode import CommandCodeAdapter  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    cfg = load_config(ROOT / "bridge.config.json")
    load_env_file(cfg)
    upstreams = upstreams_of(cfg)
    up = next(u for u in upstreams if u["name"] == "commandcode")

    model = sys.argv[1] if len(sys.argv) > 1 else (up["models"] or ["deepseek/deepseek-v4.1-flash"])[0]
    print(f"=== 用真实适配器构造请求 ===")
    print(f"  baseUrl = {up['baseUrl']}")
    print(f"  model   = {model}")
    print(f"  key src = {up['apiKeySource']}")

    body = {
        "model": model,
        "messages": [{"role": "user", "content": "用一句话回答：1+1=?"}],
        "max_tokens": 64,
        "stream": True,
    }
    adapter = CommandCodeAdapter()
    ctx = Ctx(body=body, requested_model=model, upstream_model=model,
              upstream=up, timeout=60.0, pause_turn=bool(up.get("pauseTurn")),
              stream=True)
    build = adapter.build_request(ctx, cfg)
    if build.error:
        print(f"  本地失败: {build.error.message}")
        return 1

    print("\n=== 实际发出的头 ===")
    for key in sorted(build.headers):
        value = build.headers[key]
        if key.lower() == "authorization":
            value = value[:18] + "..."
        print(f"  {key}: {value}")
    print(f"  url: {build.url}")

    started = __import__("time").time()
    try:
        with httpx.Client(timeout=60.0) as client:
            with client.stream("POST", build.url, headers=build.headers,
                               json=build.payload) as res:
                elapsed = int((__import__("time").time() - started) * 1000)
                print(f"\n=== 响应 HTTP {res.status_code}  {elapsed}ms ===")
                lines = []
                for line in res.iter_lines():
                    if line.strip():
                        lines.append(line)
                    if len(lines) >= 12:
                        break
                for line in lines:
                    print("  " + line[:300])
                if not lines:
                    print("  (空响应)")
                if res.status_code < 400:
                    print("\nRESULT: commandcode 可用")
                    return 0
                print("\nRESULT: commandcode 仍不可用")
                return 1
    except Exception as exc:  # noqa: BLE001
        print(f"\n  异常 {type(exc).__name__}: {exc}")
        print("\nRESULT: 连接失败")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
