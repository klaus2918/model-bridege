# -*- coding: utf-8 -*-
"""面板 API 路由覆盖检查：从 app.js 提取所有 /api/* 调用，逐一打到 Rust 内核。

目的：前端不改一行，确认面板会用到的接口在 Rust 侧都存在且不返回 404/5xx。
用法：py scripts\\panel_api_smoke.py [--exe PATH]
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PORT = 18941


def wait_ready(base: str) -> None:
    for _ in range(120):
        try:
            with urllib.request.urlopen(base + "/health", timeout=1) as resp:
                if resp.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.1)
    raise SystemExit("Rust 服务未就绪")


def call(base: str, path: str, method: str, body: dict | None = None,
         limit: int = 200) -> tuple[int, str]:
    url = base + path
    data = json.dumps(body).encode("utf-8") if body is not None else (b"{}" if method == "POST" else None)
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, resp.read(limit).decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(limit).decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return 0, str(exc)


def check_config_roundtrip(base: str, cfg_path: Path) -> list[str]:
    """面板契约：profiles 数据驱动 + modelOverrides 保存往返 + 非法覆盖被拦截。"""
    failures: list[str] = []
    full = 1 << 20
    status, body = call(base, "/api/config", "GET", limit=full)
    if status != 200:
        return [f"GET /api/config -> {status}"]
    data = json.loads(body)
    profile_ids = [row.get("id") for row in (data.get("profiles") or [])]
    expect_ids = ["openai", "opencode", "anthropic", "deepseek", "commandcode"]
    if profile_ids != expect_ids:
        failures.append(f"profiles 缺失或顺序不对：{profile_ids}")
    if "modelOverrides" not in ((data.get("effective") or [{}])[0]):
        failures.append("effective 缺少 modelOverrides")

    cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
    cfg["upstreams"]["oai"]["modelOverrides"] = {
        "m-*": {"limits": {"maxOutputTokens": 16384}},
        "m-1": {"capabilities": {"vision": True}},
    }
    status, body = call(base, "/api/config", "POST", {"config": cfg}, limit=full)
    if status != 200:
        failures.append(f"保存 modelOverrides -> {status} {body[:120]}")
    else:
        status, body = call(base, "/api/config", "GET", limit=full)
        saved = ((json.loads(body).get("config") or {}).get("upstreams") or {}).get("oai") or {}
        overrides = saved.get("modelOverrides") or {}
        if overrides.get("m-1", {}).get("capabilities", {}).get("vision") is not True:
            failures.append("保存后 modelOverrides 字段丢失")
        eff = (json.loads(body).get("effective") or [{}])[0]
        if "m-1" not in (eff.get("modelOverrides") or {}):
            failures.append("effective 未回显模型覆盖")

    bad = json.loads(cfg_path.read_text(encoding="utf-8"))
    bad["upstreams"]["oai"]["modelOverrides"] = {"a*b*c": {}}
    status, body = call(base, "/api/config", "POST", {"config": bad}, limit=full)
    if status != 400 or "multi_wildcard_override" not in body:
        failures.append(f"非法 modelOverrides 未被拦截：{status} {body[:120]}")
    return failures


def main() -> int:
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", default=str(ROOT / "dist" / "Model-Gate-rust.exe"))
    args = parser.parse_args()
    js = (ROOT / "modelgate" / "web" / "app.js").read_text(encoding="utf-8")
    paths = sorted(set(re.findall(r'["\'`](/api/[A-Za-z0-9_\-/]+)', js)))
    post_only = {
        "/api/config", "/api/config/reload", "/api/pricing", "/api/pricing/recompute",
        "/api/usage/clear", "/api/usage/retention/apply", "/api/logs/clear",
        "/api/upstreams/probe", "/api/upstreams/secret",
    }
    tmp = Path(tempfile.mkdtemp(prefix="mg-panel-"))
    cfg = tmp / "bridge.config.json"
    cfg.write_text(json.dumps({
        "port": PORT, "host": "127.0.0.1", "defaultModel": "m", "models": ["m"],
        "upstreams": {"oai": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1",
                              "apiKey": "k", "models": ["m"], "default": True}},
    }), encoding="utf-8")
    proc = subprocess.Popen([args.exe, "--no-gui", "--port", str(PORT),
                             "--config", str(cfg), "--db", str(tmp / "data" / "modelgate.db"),
                             "--force-multi"], cwd=ROOT)
    base = f"http://127.0.0.1:{PORT}"
    failures = []
    try:
        wait_ready(base)
        for path in paths:
            method = "POST" if path in post_only else "GET"
            probe = path
            if method == "GET" and path.rstrip("/") in (
                    "/api/usage/totals", "/api/usage/summary", "/api/usage/list",
                    "/api/usage/models", "/api/usage/upstreams", "/api/usage/export"):
                probe += "?range=all&group_by=model"
            body = None
            if path == "/api/config":
                body = {"config": json.loads(cfg.read_text(encoding="utf-8"))}
            elif path == "/api/upstreams/probe":
                body = {"name": "oai", "timeout": 8}
            elif path == "/api/upstreams/secret":
                body = {"upstream": "oai", "apiKey": ""}
            status, resp = call(base, probe, method, body)
            ok = status not in (0, 404, 500, 501)
            print(f"[{'ok' if ok else 'FAIL'}] {method} {path} -> {status} {resp[:80]}")
            if not ok:
                failures.append(path)
        for item in check_config_roundtrip(base, cfg):
            print(f"[FAIL] 配置往返：{item}")
            failures.append(item)
    finally:
        proc.terminate()
    if failures:
        print("缺失/异常接口: " + ", ".join(failures))
        return 1
    print(f"面板 {len(paths)} 个接口全部可达；profiles 与 modelOverrides 往返通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
