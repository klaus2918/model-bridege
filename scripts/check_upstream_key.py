# -*- coding: utf-8 -*-
"""上游密钥体检：逐条检查配置里的上游密钥是否仍可用（不回显明文）。

用途：当客户端报 401 / 403 时，先跑这个脚本区分三种情况 ——
  1) 上游站点正常但没有可用密钥 → 需要配置密钥；
  2) 密钥无效（拼写/截断）→ 需要改正；
  3) 密钥已过期（api_key_expired）→ 必须到上游控制台新建，重新登录不会续期。

用法：
  py -3.11 scripts/check_upstream_key.py            # 检查 bridge.config.json 里全部上游
  py -3.11 scripts/check_upstream_key.py go         # 只检查指定上游
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

# Windows 控制台默认 GBK：出现 ✓/✗ 这类字符会直接抛 UnicodeEncodeError，
# 让「体检脚本」自己在控制台崩掉。统一切到 UTF-8 输出（终端显示可能乱码，但不再崩）。
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:  # pragma: no cover - 老解释器兜底
    pass

from modelgate.config import (  # noqa: E402
    env_file_path,
    load_config,
    load_env_file,
    upstreams_of,
)


def mask(value: str) -> str:
    if not value:
        return "(空)"
    if len(value) <= 12:
        return "***"
    return f"{value[:6]}…{value[-4:]}（长度 {len(value)}）"


def probe(base_url: str, key: str | None, timeout: float = 15.0):
    """探测上游 /v1/models。

    注意：``normalize_upstream`` 会把 baseUrl 尾部的 ``/v1`` 去掉（拼接时才加回），
    所以这里必须显式补 ``/v1/models``，否则会打到站点首页拿到 HTML 404。
    """
    url = base_url.rstrip("/") + "/v1/models"
    headers = {"user-agent": "model-gate-check", "accept": "application/json"}
    if key:
        headers["authorization"] = f"Bearer {key}"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read(200).decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(400).decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def main() -> int:
    parser = argparse.ArgumentParser(description="上游密钥体检")
    parser.add_argument("names", nargs="*", help="只检查这些上游（默认全部）")
    parser.add_argument("--config", default=str(ROOT / "bridge.config.json"))
    args = parser.parse_args()

    config = load_config(args.config)
    load_env_file(config)
    upstreams = upstreams_of(config)
    if args.names:
        upstreams = [u for u in upstreams if u["name"] in args.names]

    env_path = env_file_path(config)
    print(f"配置文件: {args.config}")
    print(f"密钥文件: {env_path}（存在: {env_path.is_file() if env_path else False}）")
    print()

    problems = 0
    for up in upstreams:
        name = up["name"]
        key = up.get("apiKey") or ""
        source = up.get("apiKeySource")
        print(f"[{name}] protocol={up.get('protocol')} baseUrl={up.get('baseUrl')}")
        if source == "env":
            print(f"  来源: 环境变量 {up.get('apiKeyEnv')} = {mask(key)}")
        elif source == "file":
            print(f"  来源: 密钥文件 {up.get('apiKeyFile')} = {mask(key)}")
        elif source == "literal":
            print(f"  来源: 配置内 apiKey = {mask(key)}")
        else:
            print("  来源: 未配置密钥")

        status, body = probe(str(up.get("baseUrl") or ""), key or None)
        hint = ""
        if status == 200:
            verdict = "可用 ✓"
        elif status in (401, 403):
            text = body.lower()
            if "expired" in text or "api_key_expired" in text:
                verdict = "密钥已过期 ✗"
                hint = ("需到上游控制台新建 API Key 并替换"
                        + (f"（环境变量 {up.get('apiKeyEnv')}）" if source == "env" else "")
                        + "；重新登录不会续期")
                problems += 1
            elif not key:
                verdict = "未配置密钥 ✗"
                hint = "请在配置页填写 apiKey 或设置 apiKeyEnv"
                problems += 1
            else:
                verdict = "密钥无效 ✗"
                hint = "请核对密钥是否完整（注意空格、换行、复制截断）"
                problems += 1
        elif status == -1:
            verdict = "网络不可达"
            hint = body[:120]
        else:
            verdict = f"HTTP {status}"
        print(f"  GET /models → {status} {verdict}")
        if hint:
            print(f"  建议: {hint}")
        if status not in (200, -1):
            print(f"  上游原文: {body[:200].replace(chr(10), ' ')}")
        print()

    print(f"结论：{len(upstreams)} 条上游，需处理 {problems} 条")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
