# -*- coding: utf-8 -*-
"""v2.17 打包产物隔离验证（黑盒）：跑 dist\\Model-Gate-v2.17.exe。

覆盖：
1. 产物可启动、面板资产带当前版本戳；
2. 冻结资产：maxTokens 可自填（无 pattern/readonly）、服务胶囊条在折叠标题行内、
   上限胶囊有聚焦逻辑、全局两块两列布局；
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
LIVE = "http://127.0.0.1:8900"
sys.path.insert(0, str(ROOT))
from modelgate import __version__ as VERSION  # noqa: E402

EXE = DIST / f"Model-Gate-v{'.'.join(VERSION.split('.')[:2])}.exe"
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
    print(f"\nV217_EXE_VERIFY {'OK' if ok == len(CHECKS) else 'FAIL'} {ok}/{len(CHECKS)}")
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

    tmp = Path(tempfile.mkdtemp(prefix="mg-v217-exe-"))
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

        html = http(base + "/ui/")[1]
        check(f"面板 HTML 带 app.js?v={VERSION}（产物版本落点）",
              f"app.js?v={VERSION}" in html, f"app.js?v={VERSION}")
        with urllib.request.urlopen(base + "/ui/app.js", timeout=8.0) as resp:
            cc = resp.headers.get("cache-control", "")
        check("面板资产禁缓存头生效", "no-store" in cc.lower(), cc)

        js = http(base + "/ui/app.js")[1]
        check("冻结 app.js 含 normalizeBaseUrl", "function normalizeBaseUrl" in js)
        check("冻结 app.js 标题行按钮 stopPropagation", "inSummary(ev)" in js)
        check("冻结 app.js 上限胶囊聚焦逻辑", "maxChip" in js)
        check("冻结 app.js 胶囊显示可读数值", "fmtInt(raw)" in js)

        fold = html.split('id="config-group-service"', 1)[-1].split("</details>", 1)[0]
        summary = fold.split("<summary", 1)[-1].split("</summary>", 1)[0]
        check("冻结 index.html：胶囊条并入折叠标题行", 'class="svc-strip"' in summary)
        check("冻结 index.html：上限胶囊在标题行内", 'id="svc-max-chip"' in summary)
        check("冻结 index.html：全局两列布局", 'class="grid grid-2"' in fold)
        import re as _re
        m = _re.search(r'<input name="maxTokens"[^>]*>', fold)
        tag = m.group(0) if m else ""
        check("冻结 index.html：maxTokens 可自填（无 pattern/readonly）",
              'type="text"' in tag and not any(b in tag for b in ("readonly", "pattern=")),
              tag[:90])
        check("冻结 index.html：界面写明可自填", "自填" in fold)
        m_lab = _re.search(r'<label[^>]*>最大输出 token[^<]*<input name="maxTokens"[\s\S]*?</label>', fold)
        check("冻结 index.html：maxTokens 旁无大段说明（说明走悬停）",
              bool(m_lab) and "field-hint" not in m_lab.group(0))

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
