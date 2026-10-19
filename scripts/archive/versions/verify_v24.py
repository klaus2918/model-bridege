# -*- coding: utf-8 -*-
"""v2.4 隔离验证：文件优先 + 快照自愈 + 统计/界面能力 + 重载不重绑端口。

安全约定（务必保持）：
* 只用临时目录里的 config/db，**绝不指向 dist/**；在跑的 8900 实例不受影响。
* 端口由系统分配空闲端口后再传给被测进程。
* 断言失败即退出非零，最后打印 ``V24_VERIFY OK n/n``。
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SENTINEL = "http://127.0.0.1:1"
CHECKS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    flag = "OK  " if ok else "FAIL"
    print(f"  [{flag}] {name}" + (f" — {detail}" if detail else ""))


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


def find_value(obj, target: str) -> bool:
    """递归查找恰好等于 target 的取值（哨兵是正确地址的前缀，不能用子串判断）。"""
    if isinstance(obj, dict):
        return any(find_value(v, target) for v in obj.values())
    if isinstance(obj, list):
        return any(find_value(v, target) for v in obj)
    return obj == target


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="mg-v24-"))
    cfg_path = tmp / "bridge.config.json"
    db_path = tmp / "modelgate.db"
    port = free_port()

    file_cfg = {
        "port": port, "host": "127.0.0.1", "defaultModel": "m1",
        "models": ["m1"], "maxTokens": 8192, "timeoutSeconds": 30,
        "accessLog": False, "modelsCacheSeconds": 300,
        "thinking": {"passthrough": True, "cacheSize": 100},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "apiKey": "k", "models": ["m1"], "default": True,
                    "fetchModels": False},
        },
    }
    cfg_path.write_text(json.dumps(file_cfg, ensure_ascii=False), encoding="utf-8")

    # 用「与线上同款事故」预埋陈旧快照：cch 指向哨兵死端口
    sys.path.insert(0, str(ROOT))
    from modelgate.db import Database

    db = Database(db_path)
    db.save_config_override({
        "maxTokens": 4096,
        "snapshotOnlyFlag": True,
        "upstreams": {"cch": {"protocol": "anthropic", "baseUrl": SENTINEL,
                              "models": ["m1"], "default": True,
                              "sessionHeader": True}},
    })
    db.close()

    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    proc = subprocess.Popen(
        [sys.executable, "-m", "modelgate", "--no-gui",
         "--port", str(port), "--config", str(cfg_path), "--db", str(db_path)],
        cwd=str(ROOT), env=env,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        encoding="utf-8", errors="replace",
    )
    base = f"http://127.0.0.1:{port}"
    started = False
    stderr_tail = ""
    try:
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
            try:
                stderr_tail = (proc.stdout.read() or "")[-800:] if proc.stdout else ""
            except Exception:
                pass
            check("代理进程可启动并响应 /health", False, stderr_tail or "未在 40s 内就绪")
            return finish()

        check("代理进程可启动并响应 /health", True, f"port={port}")

        _, health = http(base + "/health")
        check("默认上游取文件值（快照哨兵被忽略）",
              health.get("upstream") == "http://127.0.0.1:15721",
              f"upstream={health.get('upstream')}")
        ups = {u["name"]: u for u in health.get("upstreams", [])}
        check("cch.baseUrl 未被快照哨兵覆盖",
              ups.get("cch", {}).get("baseUrl") == "http://127.0.0.1:15721",
              str(ups.get("cch", {}).get("baseUrl")))

        _, cfg = http(base + "/api/config")
        live = cfg.get("config") or {}
        check("/api/config 不再暴露哨兵值", not find_value(live, SENTINEL))
        check("快照独有键仍被补入（不丢配置）",
              live.get("snapshotOnlyFlag") is True,
              str(live.get("snapshotOnlyFlag")))
        check("嵌套补充键生效（cch.sessionHeader）",
              live.get("upstreams", {}).get("cch", {}).get("sessionHeader") is True)
        check("文件值优先（maxTokens=8192 而非快照 4096）",
              live.get("maxTokens") == 8192, str(live.get("maxTokens")))

        db = Database(db_path)
        healed = db.load_config_override()
        db.close()
        check("陈旧快照已自愈（重启不会再带回哨兵）",
              not find_value(healed, SENTINEL)
              and healed.get("upstreams", {}).get("cch", {}).get("baseUrl")
              == "http://127.0.0.1:15721")

        # 统计与界面能力
        status, models = http(base + "/api/usage/models")
        check("GET /api/usage/models 可用", status == 200 and isinstance(models, dict),
              f"status={status}")
        status, totals = http(base + "/api/usage/totals")
        tf = (totals or {}).get("totals") or {}
        need = ("p50_duration_ms", "p95_duration_ms", "avg_ttft_ms", "ttft_samples")
        check("总计含 P50/P95/TTFT 字段",
              status == 200 and all(k in tf for k in need),
              ",".join(k for k in need if k not in tf) or "all present")
        status, models2 = http(base + "/api/usage/models?limit=5")
        check("模型用量聚合返回 rows",
              status == 200 and isinstance(models2, dict) and "rows" in models2)
        status, page = http(base + "/ui/")
        check("面板 UI 可访问", status == 200 and "<html" in str(page).lower())
        for asset, token in (("/ui/app.js", "loadModelUsage"),
                             ("/ui/style.css", "prefers-reduced-motion"),
                             ("/ui/", "模型用量")):
            status, text = http(base + asset)
            check(f"界面资产 {asset} 含 {token}", status == 200 and token in str(text))

        # 重载不重绑端口：请求前后同一 socket 仍在服务
        status, ack = http(base + "/api/config/reload", {})
        check("POST /api/config/reload 返回成功", status == 200 and ack.get("ok") is True)
        _, health2 = http(base + "/health")
        check("重载后端口未变化（代理不中断）",
              health2.get("upstream") == "http://127.0.0.1:15721")
        check("重载后进程仍是同一个（未重启）", proc.poll() is None)
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=10)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        import shutil
        shutil.rmtree(tmp, ignore_errors=True)

    return finish()


def finish() -> int:
    passed = sum(1 for _, ok, _ in CHECKS if ok)
    total = len(CHECKS)
    print()
    if passed == total:
        print(f"V24_VERIFY OK {passed}/{total}")
        return 0
    print(f"V24_VERIFY FAILED {passed}/{total}")
    for name, ok, detail in CHECKS:
        if not ok:
            print(f"  - {name} {detail}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
