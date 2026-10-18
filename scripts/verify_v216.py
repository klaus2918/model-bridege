# -*- coding: utf-8 -*-
"""v2.16 隔离验证：配置保存链路的「不可操作错误」修复。

覆盖：
1. 缺协议的 Base URL 经 API 保存后落库/落盘为带协议的地址（外网补 https，本机补 http）；
2. 面板资产提供 normalizeBaseUrl，且不再出现只报数量、不点名原因的旧文案；
3. 现场保护：全程不动 dist\\ 与在跑的实例。

安全约定：只用临时目录里的 config/db，端口为系统分配的空闲端口。
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKS: list[tuple[str, bool, str]] = []

if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
from modelgate import __version__ as _version  # noqa: E402

DIST_DB = ROOT / "dist" / "data" / "modelgate.db"
LIVE = "http://127.0.0.1:8900"


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    print(f"  [{'OK  ' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def http(url: str, payload=None, timeout: float = 8.0):
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers,
                                 method="POST" if data else "GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read().decode("utf-8", "replace")
        ctype = resp.headers.get("content-type", "")
        return resp.status, (json.loads(body) if "json" in ctype else body)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def finish() -> int:
    ok = sum(1 for _, o, _ in CHECKS if o)
    print(f"\nV216_VERIFY {'OK' if ok == len(CHECKS) else 'FAIL'} {ok}/{len(CHECKS)}")
    return 0 if ok == len(CHECKS) else 1


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="mg-v216-"))
    cfg_path = tmp / "bridge.config.json"
    db_path = tmp / "data" / "modelgate.db"
    base_cfg = {
        "port": 0, "host": "127.0.0.1", "defaultModel": "deepseek-flash",
        "models": ["deepseek-flash"], "maxTokens": 8192, "timeoutSeconds": 600,
        "accessLog": False, "modelsCacheSeconds": 300, "aliases": {},
        "thinking": {"passthrough": True, "cacheSize": 500},
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "models": ["deepseek-flash"], "default": True},
        },
    }
    port = free_port()
    cfg_path.parent.mkdir(parents=True, exist_ok=True)
    cfg_path.write_text(json.dumps(base_cfg, ensure_ascii=False), encoding="utf-8")

    before = DIST_DB.stat().st_mtime if DIST_DB.exists() else None
    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    proc = subprocess.Popen(
        [sys.executable, "-m", "modelgate", "--no-gui", "--port", str(port),
         "--config", str(cfg_path), "--db", str(db_path)],
        cwd=str(ROOT), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace")
    base = f"http://127.0.0.1:{port}"
    try:
        started = False
        deadline = time.time() + 40
        while time.time() < deadline:
            if proc.poll() is not None:
                break
            try:
                http(base + "/health", timeout=2.0)
                started = True
                break
            except Exception:
                time.sleep(0.4)
        if not started:
            check("隔离实例可启动并响应 /health", False, "未在 40s 内就绪")
            return finish()
        check("隔离实例可启动并响应 /health", True, f"port={port}")

        # 1) 面板资产版本戳与实现
        html = http(base + "/ui/")[1]
        stamp = f"app.js?v={_version}"
        check("面板资产带当前版本戳", stamp in html, stamp)

        js = http(base + "/ui/app.js")[1]
        check("app.js 提供 normalizeBaseUrl", "function normalizeBaseUrl" in js)
        check("app.js 采集时先补全协议", "normalizeBaseUrl(before)" in js)
        check("app.js 声明兜底说明（autoFixNotes）", "state.autoFixNotes" in js)
        check("旧文案（只说数量）已移除", "处需要先修正（见上方清单）" not in js)
        check("新文案点名第一条错误", "errors[0].message" in js)

        # 2) 缺协议地址经 API 保存 → 规范化后落库
        remote = dict(base_cfg)
        remote["port"] = port
        remote["upstreams"] = {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "models": ["deepseek-flash"], "default": True},
            "go": {"protocol": "openai", "baseUrl": "api.deepseek.com/v1",
                   "models": ["deepseek-flash"], "apiKeyEnv": "OPENCODE_API_KEY"},
        }
        status, _ = http(base + "/api/config", {"config": remote})
        check("缺协议 Base URL 的配置可保存", status == 200, f"status={status}")

        def runtime_base(name: str) -> str:
            _, h = http(base + "/health")
            for up in h.get("upstreams", []):
                if up.get("name") == name:
                    return str(up.get("baseUrl") or "")
            return ""

        # 契约：配置文件是用户原文，不偷改；规范化发生在运行期。
        stored = http(base + "/api/config")[1]["config"]["upstreams"]["go"]["baseUrl"]
        check("配置文件保留用户原文（不偷改用户契约）",
              stored == "api.deepseek.com/v1", stored)
        rt = runtime_base("go")
        check("运行期外网地址补 https:// 并剥离 /v1",
              rt == "https://api.deepseek.com", rt)

        local = json.loads(json.dumps(remote))
        local["upstreams"]["go"]["baseUrl"] = "127.0.0.1:15721"
        http(base + "/api/config", {"config": local})
        rt2 = runtime_base("go")
        check("运行期本机地址补 http://（不能补成 https）",
              rt2 == "http://127.0.0.1:15721", rt2)

        on_disk = json.loads(cfg_path.read_text(encoding="utf-8"))
        check("双写文件里是该上游的最新原文",
              on_disk["upstreams"]["go"]["baseUrl"] == "127.0.0.1:15721",
              str(on_disk["upstreams"]["go"]["baseUrl"]))

        # 3) 现场保护：在跑的 8900 与 dist 数据没被动过
        try:
            _, live = http(LIVE + "/health", timeout=4.0)
            check("在线 8900 实例仍在服务", live.get("status") == "ok")
        except Exception as exc:  # pragma: no cover - 现场可能没起
            check("在线 8900 实例仍在服务", False, str(exc)[:120])
        after = DIST_DB.stat().st_mtime if DIST_DB.exists() else None
        check("dist\\data 未被本次验证改写", before == after, f"{before} -> {after}")
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=10)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
    return finish()


if __name__ == "__main__":
    raise SystemExit(main())
