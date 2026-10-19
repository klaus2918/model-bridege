# -*- coding: utf-8 -*-
"""v2.17 隔离验证：① 最大输出 token 可自填任意值（含中文/带单位写法）；② 布局不再浪费竖向空间。

安全约定：临时目录里的 config/db + 系统分配的空闲端口；不碰 dist\\ 与在跑实例。
"""
from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKS: list[tuple[str, bool, str]] = []
sys.path.insert(0, str(ROOT))
from modelgate import __version__ as VERSION  # noqa: E402

DIST_DB = ROOT / "dist" / "data" / "modelgate.db"
LIVE = "http://127.0.0.1:8900"


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    print(f"  [{'OK  ' if ok else 'FAIL'}] {name}" + (f"  — {detail}" if detail else ""))


def http(url: str, payload=None, timeout: float = 10.0):
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


def parse_human(text: str):
    """镜像 app.js 的 parseCount：供 node 不在时兜底断言，不与前端实现共享代码。"""
    node = shutil.which("node")
    if not node:
        return None
    js = (ROOT / "modelgate" / "web" / "app.js").read_text(encoding="utf-8")
    m = re.search(r"function parseCount\(text\) \{[\s\S]*?\n\}", js)
    cases = [
        ["300000", 300000], ["3,000,000", 3000000], ["30万", 300000],
        ["384K", 393216], ["1M", 1048576], ["1.5亿", 150000000], ["", None], ["abc", None],
    ]
    script = (m.group(0) + "\nconst cases = " + json.dumps([c[0] for c in cases], ensure_ascii=False)
              + ";\nconsole.log(JSON.stringify(cases.map((c) => parseCount(c))));\n")
    out = subprocess.run([node, "-e", script], capture_output=True, text=True,
                         encoding="utf-8", errors="replace", timeout=20)
    if out.returncode != 0:
        return None
    got = json.loads(out.stdout.strip())
    bad = [(cases[i][0], cases[i][1], got[i]) for i in range(len(cases)) if cases[i][1] != got[i]]
    return bad


def finish() -> int:
    ok = sum(1 for _, o, _ in CHECKS if o)
    print(f"\nV217_VERIFY {'OK' if ok == len(CHECKS) else 'FAIL'} {ok}/{len(CHECKS)}")
    return 0 if ok == len(CHECKS) else 1


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="mg-v217-"))
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
    cfg_path.write_text(json.dumps(base_cfg, ensure_ascii=False), encoding="utf-8")
    db_before = DIST_DB.stat().st_mtime if DIST_DB.exists() else None

    bad = parse_human("")
    check("parseCount 把 30万/384K/1M/300000 都读成数字", bad == [],
          "全通过" if bad == [] else f"错例 {bad}")
    check("parseCount 读不出来的返回 null", bad is not None)

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

        # ① maxTokens 自填任意值
        for raw, expect in ((300000, 300000), (393216, 393216), (1048576, 1048576), (1, 1)):
            remote = json.loads(json.dumps(base_cfg))
            remote["port"] = port
            remote["maxTokens"] = raw
            status, _ = http(base + "/api/config", {"config": remote})
            got = http(base + "/api/config")[1]["config"]["maxTokens"]
            ok = status == 200 and got == expect
            check(f"自填最大输出 {raw} → 保存并回读 {expect}", ok, f"status={status} readback={got}")

        # 带单位的字符串仍应被拒（前端负责换算，后端不猜）
        remote = json.loads(json.dumps(base_cfg))
        remote["port"] = port
        remote["maxTokens"] = "30万"
        try:
            status, _ = http(base + "/api/config", {"config": remote})
        except urllib.error.HTTPError as exc:  # noqa: BLE001
            status = exc.code
        check("后端不猜带单位的字符串（前端换算后才提交）", status == 400, f"status={status}")

        # ② 布局：标题行收纳 + 两列 + 可自填
        html = http(base + "/ui/")[1]
        fold = html.split('id="config-group-service"', 1)[-1].split("</details>", 1)[0]
        summary = fold.split("<summary", 1)[-1].split("</summary>", 1)[0]
        check("服务胶囊条并入折叠标题行（省一行）", 'class="svc-strip"' in summary)
        check("标题行内动作按钮同排", all(f'id="{b}"' in summary
              for b in ("svc-copy-url", "svc-open-url", "svc-reset")))
        check("全局字段两列布局（不再一列堆叠）", 'class="grid grid-2"' in fold)
        m = re.search(r'<input name="maxTokens"[^>]*>', fold)
        tag = m.group(0) if m else ""
        check("maxTokens 是自由文本输入（无 pattern/readonly）",
              'type="text"' in tag and "readonly" not in tag and "pattern=" not in tag)
        check("面板资产带当前版本戳", f"app.js?v={VERSION}" in html, f"app.js?v={VERSION}")

        try:
            http(LIVE + "/health", timeout=4.0)
            check("在线 8900 实例仍在服务", True)
        except Exception as exc:  # noqa: BLE001
            check("在线 8900 实例仍在服务", False, str(exc))
        check("dist\\data 未被本次验证改写",
              db_before == (DIST_DB.stat().st_mtime if DIST_DB.exists() else None))
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=15)
        except Exception:  # noqa: BLE001
            proc.kill()
    return finish()


if __name__ == "__main__":
    sys.exit(main())
