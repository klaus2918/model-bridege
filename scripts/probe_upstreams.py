# -*- coding: utf-8 -*-
"""对每个真实上游做一次最小连通性探测，定位「代理不通」到底卡在哪一层。

用法：py -3.11 scripts\\probe_upstreams.py [--stream]
"""
from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

from modelgate.config import load_config, load_env_file, upstreams_of  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
USE_STREAM = "--stream" in sys.argv


def main() -> int:
    cfg_path = ROOT / "bridge.config.json"
    cfg = load_config(cfg_path)
    load_env_file(cfg)
    upstreams = upstreams_of(cfg)

    print(f"=== 上游连通性探测（{'流式' if USE_STREAM else '非流式'}）===\n")
    results = []
    for up in upstreams:
        name = up["name"]
        proto = up["protocol"]
        key = up.get("apiKey") or ""
        source = up.get("apiKeySource")
        print(f"[{name}] protocol={proto} baseUrl={up['baseUrl']}")
        print(f"        key source={source} "
              f"{'(已配置)' if source in ('env', 'file', 'literal') and key and key != 'any-key' else '(未配置真实密钥)'}")

        model = ""
        for candidate in (cfg.get("defaultModel"), *(up.get("models") or [])):
            if candidate:
                model = candidate
                break
        print(f"        尝试模型={model}")

        if proto == "anthropic":
            url = f"{up['baseUrl']}/v1/messages"
            payload = {"model": model, "max_tokens": 16,
                       "messages": [{"role": "user", "content": "ping"}]}
            headers = {"content-type": "application/json"}
            if key:
                headers["x-api-key"] = key
                headers["authorization"] = f"Bearer {key}"
            headers["anthropic-version"] = up.get("anthropicVersion", "2023-06-01")
        elif proto == "openai":
            url = f"{up['baseUrl']}/v1/chat/completions"
            payload = {"model": model, "max_tokens": 16, "stream": USE_STREAM,
                       "messages": [{"role": "user", "content": "ping"}]}
            headers = {"content-type": "application/json"}
            if key:
                headers["authorization"] = f"Bearer {key}"
            if up.get("sessionHeader"):
                headers["x-opencode-session"] = "probe-0000000000000000"
        else:  # commandcode
            url = f"{up['baseUrl']}/alpha/generate"
            payload = {
                "config": {"workingDir": str(ROOT), "date": "2026-10-09",
                           "environment": "windows", "structure": [],
                           "isGitRepo": False, "currentBranch": "",
                           "mainBranch": "", "gitStatus": "", "recentCommits": []},
                "memory": None, "taste": None, "skills": None,
                "permissionMode": "standard",
                "threadId": "00000000-0000-4000-8000-000000000000",
                "params": {"model": model, "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "ping"}]}],
                    "tools": [], "system": [],
                    "max_tokens": 32, "stream": True},
            }
            headers = {"content-type": "application/json", "user-agent": "cli",
                       "authorization": f"Bearer {key}"}
            if up.get("cmdZdr", True):
                headers["x-cmd-zdr"] = "1"

        started = time.time()
        try:
            if proto == "commandcode" or USE_STREAM:
                with httpx.Client(timeout=30.0) as client:
                    with client.stream("POST", url, headers=headers,
                                       json=payload) as res:
                        first = ""
                        for line in res.iter_lines():
                            if line.strip():
                                first = line[:300]
                                break
                        elapsed = int((time.time() - started) * 1000)
                        print(f"        -> HTTP {res.status_code}  {elapsed}ms")
                        if first:
                            print(f"           首行: {first}")
                        ok = res.status_code < 400
            else:
                res = httpx.post(url, headers=headers, json=payload, timeout=30.0)
                elapsed = int((time.time() - started) * 1000)
                body = res.text[:400].replace("\n", " ")
                print(f"        -> HTTP {res.status_code}  {elapsed}ms")
                print(f"           {body}")
                ok = res.status_code < 400
        except Exception as exc:  # noqa: BLE001
            elapsed = int((time.time() - started) * 1000)
            print(f"        -> 异常 {type(exc).__name__}: {exc}  {elapsed}ms")
            ok = False
        results.append((name, ok))
        print()

    print("=== 结论 ===")
    for name, ok in results:
        print(f"  {name}: {'可用' if ok else '不可用'}")
    print()
    print("=== 相关环境变量 ===")
    for var in ("OPENCODE_API_KEY", "COMMAND_CODE_API_KEY"):
        value = os.environ.get(var)
        print(f"  {var}: {'已设置' if value else '未设置'}")
    auth = Path.home() / ".commandcode" / "auth.json"
    print(f"  ~/.commandcode/auth.json: {'存在' if auth.is_file() else '不存在'}")

    return 0 if all(ok for _, ok in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
