# -*- coding: utf-8 -*-
"""v2.2 能力验收（对**打包后的 exe**）：模型清单 / 单价表 / 导出 / 保留 / 统计口径。

设计要点：
- 用临时配置与临时数据库启动 exe（不污染 dist 目录），验证「交付产物」而非源码；
- 全部走真实 HTTP，和用户双击后浏览器里发生的事同一条链路；
- 结束后清理进程与临时目录，可重复运行。

用法：py -3.11 scripts/verify_v22_features.py [--exe dist/Model-Gate.exe]
退出码：全部通过 0，否则 1。
"""
from __future__ import annotations

import argparse
import json
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAILS: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" - {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def http(port: int, path: str, *, method: str = "GET", body: dict | None = None,
         timeout: float = 6.0):
    url = f"http://127.0.0.1:{port}{path}"
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if data:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers or {})
    except Exception as exc:  # noqa: BLE001
        return None, str(exc).encode("utf-8"), {}


def wait_health(proc: subprocess.Popen, ports: list[int], timeout: float = 45.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return None, f"进程提前退出（code={proc.returncode}）"
        for port in ports:
            status, body, _ = http(port, "/health", timeout=1.5)
            if status == 200:
                return port, body.decode("utf-8", "replace")
        time.sleep(0.4)
    return None, "超时未就绪"


def port_in_use(port: int) -> bool:
    probe = socket.socket()
    probe.settimeout(0.3)
    try:
        probe.connect(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        probe.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", default=str(ROOT / "dist" / "Model-Gate.exe"))
    parser.add_argument("--port", type=int, default=8961)
    args = parser.parse_args()

    exe = Path(args.exe)
    if not exe.is_file():
        print(f"找不到 exe：{exe}")
        return 1

    workdir = Path(tempfile.mkdtemp(prefix="mg-v22-"))
    config_path = workdir / "bridge.config.json"
    db_path = workdir / "data" / "modelgate.db"
    config_path.write_text(json.dumps({
        "port": args.port,
        "host": "127.0.0.1",
        "defaultModel": "probe-model",
        "models": ["probe-model", "probe-alias-target"],
        "aliases": {"probe-alias": "probe-alias-target"},
        "accessLog": True,
        "retentionDays": 0,
        "upstreams": {
            "up1": {"protocol": "openai", "baseUrl": "http://127.0.0.1:9/v1",
                    "apiKeyEnv": "PROBE_KEY_ENV", "models": ["probe-model"],
                    "fetchModels": False, "default": True},
            "up2": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:9",
                    "apiKey": "", "models": ["probe-claude"],
                    "fetchModels": False},
        },
    }, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"exe : {exe} ({exe.stat().st_size} bytes)")
    print(f"临时工作目录: {workdir}")
    proc = subprocess.Popen(
        [str(exe), "--config", str(config_path), "--db", str(db_path),
         "--no-generate-config"],
        cwd=str(workdir), stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    try:
        port, health = wait_health(proc, [args.port, args.port + 1, args.port + 2,
                                          args.port + 3])
        check("exe 启动并 /health 可达", port is not None,
              f"端口={port}" if port else health)
        if port is None:
            return 1
        health_data = json.loads(health)
        check("健康检查列出 2 个上游", len(health_data.get("upstreams", [])) == 2,
              str([u.get("name") for u in health_data.get("upstreams", [])]))

        # 1) 新页面在包里
        status, body, _ = http(port, "/ui/index.html")
        html = body.decode("utf-8", "replace")
        check("面板含「模型」页签", 'data-tab="model"' in html and 'id="panel-model"' in html)
        check("面板含单价表编辑区", 'id="pricing-json"' in html)
        status, body, _ = http(port, "/ui/app.js")
        js = body.decode("utf-8", "replace")
        check("面板脚本已更新（含模型页逻辑）",
              "loadCatalog" in js and "loadPricing" in js and "exportUsage" in js)

        # 2) 单价表
        status, body, _ = http(port, "/api/pricing")
        data = json.loads(body)
        check("GET /api/pricing", status == 200 and data["configured"] is False,
              f"configured={data.get('configured')}")

        status, body, _ = http(port, "/api/pricing", method="POST", body={
            "pricing": {"currency": "CNY",
                        "default": {"input": 1.0, "output": 2.0},
                        "rules": {"up1/probe-model": {"input": 0.5, "output": 1.5}}},
            "recompute": True,
        })
        saved = json.loads(body)
        check("POST /api/pricing 保存成功",
              status == 200 and saved.get("configured") is True,
              f"recomputed={saved.get('recomputed')}")
        persisted = json.loads(config_path.read_text(encoding="utf-8"))
        check("单价表持久化到配置文件", "pricing" in persisted)
        check("保存单价表未破坏上游配置",
              set(persisted.get("upstreams", {})) == {"up1", "up2"})

        # 3) 模型清单
        status, body, _ = http(port, "/api/models/catalog?page=1&page_size=10")
        catalog = json.loads(body)
        models = {row["model"] for row in catalog.get("rows", [])}
        check("GET /api/models/catalog 返回全部模型",
              status == 200 and {"probe-model", "probe-claude", "probe-alias-target"} <= models,
              f"total={catalog.get('total')}")
        row = next((r for r in catalog["rows"] if r["model"] == "probe-model"), {})
        check("清单标注单价命中", bool(row.get("costHint")) and row.get("costRule") == "up1/probe-model",
              f"rule={row.get('costRule')}")
        alias_row = next((r for r in catalog["rows"] if r["model"] == "probe-alias-target"), {})
        check("清单显示别名", alias_row.get("aliases") == ["probe-alias"],
              str(alias_row.get("aliases")))

        status, body, _ = http(port, "/api/models/catalog?q=probe-claude")
        check("清单检索生效", json.loads(body)["total"] == 1)

        # 4) 导出
        status, body, headers = http(port, "/api/usage/export?range=all&format=csv")
        check("GET /api/usage/export CSV",
              status == 200 and body[:3] == b"\xef\xbb\xbf"
              and b"cost" in body.split(b"\r\n")[0],
              f"rows={headers.get('x-row-count')}")
        status, body, _ = http(port, "/api/usage/export?range=all&format=json")
        check("GET /api/usage/export JSON", status == 200 and "rows" in json.loads(body))

        # 5) 保留策略
        status, body, _ = http(port, "/api/usage/retention/apply", method="POST",
                               body={"days": 0})
        check("POST /api/usage/retention/apply（0 = 不清理）",
              status == 200 and json.loads(body)["removed"] == 0)

        # 6) 统计口径
        status, body, _ = http(port, "/api/usage/totals?range=all")
        totals = json.loads(body)["totals"]
        check("totals 含费用/成功率/平均耗时",
              {"cost", "success_rate", "avg_duration_ms", "ok"} <= set(totals),
              json.dumps({k: totals[k] for k in ("cost", "success_rate")}, ensure_ascii=False))

        # 7) 交付目录干净
        crash = workdir / "data" / "crash.log"
        fault = workdir / "data" / "fault.log"
        check("无 crash.log / fault.log", not crash.is_file() and not fault.is_file())
        check("进程存活", proc.poll() is None)
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                proc.kill()
        import shutil

        shutil.rmtree(workdir, ignore_errors=True)

    print()
    if FAILS:
        print(f"V22 CHECK FAILED: {len(FAILS)}")
        for item in FAILS:
            print("  - " + item)
        return 1
    print("V22 CHECK OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
