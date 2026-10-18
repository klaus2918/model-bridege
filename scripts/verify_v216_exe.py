# -*- coding: utf-8 -*-
"""v2.16 打包产物隔离验证（黑盒）：直接跑 dist\\Model-Gate-v2.16.exe。

覆盖：
1. 产物可启动、面板资产带 2.16.0 版本戳；
2. 冻结的 app.js 里确实带 normalizeBaseUrl 与新文案（防「源码改了没打进包」）；
3. 缺协议 Base URL 的配置经 API 保存成功，运行期规范化正确；
4. 现场保护：dist 其他 exe / dist\\bridge.config.json / dist\\data 与在跑实例不被碰。
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
DIST = ROOT / "dist"
EXE = DIST / "Model-Gate-v2.16.exe"
LIVE = "http://127.0.0.1:8900"
CHECKS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    print(f"  [{'OK  ' if ok else 'FAIL'}] {name}" + (f"  — {detail}" if detail else ""))


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
    print(f"\nV216_EXE_VERIFY {'OK' if ok == len(CHECKS) else 'FAIL'} {ok}/{len(CHECKS)}")
    return 0 if ok == len(CHECKS) else 1


def main() -> int:
    if not EXE.is_file():
        print(f"缺少产物：{EXE}")
        return 1
    watched = [DIST / "bridge.config.json", DIST / "Model-Gate.exe"]
    watched += [p for p in DIST.glob("Model-Gate-v*.exe") if p.name != EXE.name]
    before = {p.name: (p.stat().st_mtime, p.stat().st_size) for p in watched if p.exists()}
    db = DIST / "data" / "modelgate.db"
    db_before = db.stat().st_mtime if db.exists() else None

    tmp = Path(tempfile.mkdtemp(prefix="mg-v216-exe-"))
    (tmp / "data").mkdir(parents=True, exist_ok=True)
    cfg_path = tmp / "bridge.config.json"
    cfg = {
        "port": 0, "host": "127.0.0.1", "defaultModel": "deepseek-flash",
        "models": ["deepseek-flash"], "maxTokens": 8192, "timeoutSeconds": 600,
        "accessLog": False, "modelsCacheSeconds": 300, "aliases": {},
        "thinking": {"passthrough": True, "cacheSize": 500},
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "models": ["deepseek-flash"], "default": True},
        },
    }
    cfg_path.write_text(json.dumps(cfg, ensure_ascii=False), encoding="utf-8")
    port = free_port()
    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(port),
         "--config", str(cfg_path), "--db", str(tmp / "data" / "modelgate.db")],
        cwd=str(tmp), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace")
    base = f"http://127.0.0.1:{port}"
    try:
        started = False
        deadline = time.time() + 90
        while time.time() < deadline:
            if proc.poll() is not None:
                break
            try:
                http(base + "/health", timeout=2.0)
                started = True
                break
            except Exception:
                time.sleep(0.5)
        if not started:
            out = ""
            if proc.poll() is not None and proc.stdout:
                out = (proc.stdout.read() or "")[-400:]
            check("打包产物可启动并响应 /health", False, out or "90s 内未就绪")
            return finish()
        check("打包产物可启动并响应 /health", True, f"exe={EXE.name} port={port}")

        _, health = http(base + "/health")
        check("产物 /health 就绪", str(health.get("status")) == "ok",
              str(health.get("status")))

        # 版本唯一证据：产物在服务面板时注入的资产戳（由 modelgate.__version__ 生成）
        html = http(base + "/ui/")[1]
        check("面板 HTML 带 app.js?v=2.16.0（产物版本落点）", "app.js?v=2.16.0" in html)
        with urllib.request.urlopen(base + "/ui/app.js", timeout=8.0) as resp:
            cc = resp.headers.get("cache-control", "")
        check("面板资产禁缓存头生效", "no-store" in cc.lower(), cc)

        js = http(base + "/ui/app.js")[1]
        check("冻结 app.js 含 normalizeBaseUrl", "function normalizeBaseUrl" in js)
        check("冻结 app.js 采集时先补全协议", "normalizeBaseUrl(before)" in js)
        check("冻结 app.js 含 autoFixNotes 兜底说明", "state.autoFixNotes" in js)
        check("冻结 app.js 点名第一条错误", "errors[0].message" in js)
        css = http(base + "/ui/style.css")[1]
        check("冻结 style.css 非空", len(css) > 5000, f"{len(css)} bytes")

        remote = dict(cfg)
        remote["port"] = port
        remote["upstreams"] = {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "models": ["deepseek-flash"], "default": True},
            "go": {"protocol": "openai", "baseUrl": "api.deepseek.com/v1",
                   "models": ["deepseek-flash"], "apiKeyEnv": "OPENCODE_API_KEY"},
        }
        status, _ = http(base + "/api/config", {"config": remote})
        check("缺协议 Base URL 可保存（产物链路）", status == 200, f"status={status}")

        _, h2 = http(base + "/health")
        rt = ""
        for up in h2.get("upstreams", []):
            if up.get("name") == "go":
                rt = str(up.get("baseUrl") or "")
        check("运行期补 https:// 并剥离 /v1", rt == "https://api.deepseek.com", rt)

        stored = http(base + "/api/config")[1]["config"]["upstreams"]["go"]["baseUrl"]
        check("配置文件保留用户原文", stored == "api.deepseek.com/v1", stored)

        try:
            http(LIVE + "/health", timeout=4.0)
            check("在线 8900 实例仍在服务", True)
        except Exception as exc:  # noqa: BLE001
            check("在线 8900 实例仍在服务", False, str(exc))

        after = {p.name: (p.stat().st_mtime, p.stat().st_size) for p in watched if p.exists()}
        check("dist 现场未被本次验证改写", before == after)
        check("dist\\data 未被本次验证改写",
              db_before == (db.stat().st_mtime if db.exists() else None))
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=15)
        except Exception:  # noqa: BLE001
            proc.kill()
    return finish()


if __name__ == "__main__":
    sys.exit(main())
