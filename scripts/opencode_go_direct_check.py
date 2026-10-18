#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""opencode go 直连自检（直接接入，不经代理中转）。

验证对象：https://opencode.ai/zen/go/v1 （opencode Go 订阅，OpenAI 兼容）

直连三要素：
  1) 端点 + Bearer 鉴权（Authorization: Bearer <OPENCODE_API_KEY>）
  2) GET /models 无需会话头
  3) POST /chat/completions 必须带 x-opencode-session 头（任意非空值），
     缺失时上游返回 400 MissingSessionID。

凭据来源优先级：
  --key <值>  >  环境变量（--key-env，默认 OPENCODE_API_KEY）  >  --env-file（默认 ~/.jcode/.env）

注意 1：opencode CLI 自身登录态（~/.local/share/opencode/auth.json 的 opencode-go.key）
实测不能用作外部直连凭据（401 Invalid credential），本脚本不复用它。
注意 2：opencode.ai 前置 Cloudflare 会拦截 Python-urllib 默认 UA（403，error code 1010），
因此本脚本显式设置常规 User-Agent（同 curl，已验证可过）。

安全：不打印密钥明文，仅显示长度；本脚本只读取本地 env 文件、只访问 opencode.ai。

用法：
  py -3.11 scripts/opencode_go_direct_check.py
  py -3.11 scripts/opencode_go_direct_check.py --model glm-5.3-flash
退出码：0 = 直连可用；2 = 不可用（含凭据缺失）。
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
import uuid

BASE_DEFAULT = "https://opencode.ai/zen/go/v1"
# opencode.ai 前置 Cloudflare 拦截 Python-urllib 默认 UA（403 / error code 1010），
# 必须显式设置常规 User-Agent（curl 默认 UA 已验证可过；Model-Gate 探测侧亦设 "cli"）。
USER_AGENT = "curl/8.5.0"


def read_key_from_env_file(path: str, var: str) -> str | None:
    """从 KEY=VALUE 形式的 env 文件里读取一个变量（失败返回 None）。"""
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, _, v = line.partition("=")
                if k.strip() == var:
                    return v.strip().strip('"').strip("'")
    except OSError:
        return None
    return None


def http(method: str, url: str, key: str, headers: dict | None = None,
         body: dict | None = None, timeout: int = 30) -> tuple[int, str]:
    req = urllib.request.Request(url, method=method)
    req.add_header("Authorization", f"Bearer {key}")
    req.add_header("User-Agent", USER_AGENT)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, data=data, timeout=timeout) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001 - 网络错误统一报告
        return -1, f"network error: {e}"


def main() -> int:
    ap = argparse.ArgumentParser(description="opencode go direct-access self check")
    ap.add_argument("--base-url", default=BASE_DEFAULT)
    ap.add_argument("--key", help="直接传入 API key（默认不用，避免泄漏到历史记录）")
    ap.add_argument("--key-env", default="OPENCODE_API_KEY", help="密钥环境变量名")
    ap.add_argument("--env-file", default=os.path.expanduser("~/.jcode/.env"),
                    help="KEY=VALUE 环境文件路径")
    ap.add_argument("--model", default="mimo-v2.5", help="chat 测试用模型")
    args = ap.parse_args()

    key = args.key or os.environ.get(args.key_env) or read_key_from_env_file(args.env_file, args.key_env)
    if not key:
        print(f"[FAIL] 未找到 API key（--key / 环境变量 {args.key_env} / {args.env_file}）")
        return 2
    print(f"凭据：len={len(key)}（不显示明文）  端点：{args.base_url}")

    ok = True

    # 1) GET /models —— 只验证鉴权与连通性
    status, text = http("GET", f"{args.base_url}/models", key)
    count = 0
    try:
        count = len(json.loads(text).get("data", []))
    except Exception:  # noqa: BLE001
        pass
    t1 = status == 200
    ok &= t1
    print(f"[{'PASS' if t1 else 'FAIL'}] GET  /models -> HTTP {status}, models={count}")

    # 2) POST chat 无会话头 —— 预期被拒（证明会话头是硬性要求）
    body = {"model": args.model, "messages": [{"role": "user", "content": "ping"}], "max_tokens": 8}
    status, text = http("POST", f"{args.base_url}/chat/completions", key, body=body)
    etype = ""
    try:
        etype = json.loads(text).get("error", {}).get("type", "")
    except Exception:  # noqa: BLE001
        pass
    t2 = status == 400 and "MissingSessionID" in text
    print(f"[{'INFO' if t2 else 'WARN'}] POST chat（无 x-opencode-session）-> HTTP {status}"
          f"{', error=' + etype if etype else ''}"
          + ("  （符合预期：会话头为硬性要求）" if t2 else "  （上游行为可能已变化）"))

    # 3) POST chat 带会话头 —— 直连可用性正证
    sid = str(uuid.uuid4())
    status, text = http("POST", f"{args.base_url}/chat/completions", key,
                        headers={"x-opencode-session": sid}, body=body)
    finish = ""
    usage = ""
    try:
        j = json.loads(text)
        finish = (j.get("choices") or [{}])[0].get("finish_reason") or ""
        u = j.get("usage") or {}
        if u:
            usage = f"tokens={u.get('prompt_tokens', '?')}+{u.get('completion_tokens', '?')}"
    except Exception:  # noqa: BLE001
        pass
    t3 = status == 200
    ok &= t3
    print(f"[{'PASS' if t3 else 'FAIL'}] POST chat（带 x-opencode-session）-> HTTP {status}"
          + (f", finish={finish}, {usage}" if t3 else ""))

    print("RESULT:", "DIRECT_OK" if ok else "DIRECT_FAILED")
    return 0 if ok else 2


if __name__ == "__main__":
    sys.exit(main())
