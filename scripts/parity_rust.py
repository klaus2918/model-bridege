# -*- coding: utf-8 -*-
"""Rust 内核 vs Python 内核的黑盒对等回归。

启动同一个桩上游，分别用 Python 版与 Rust 版代理同一份配置，发送相同的
OpenAI 请求（三种协议 × 非流式/流式），逐字段比较响应与用量统计。
配置侧同时比较五套 profile（显式指定/协议判定）、modelOverrides 的
`/api/config` effective 与 validate 诊断，确认双端配置语义一致。

用法：
    py scripts\\parity_rust.py
    py scripts\\parity_rust.py --rust path\\to\\model-gate.exe
退出码 0 表示对等；1 表示存在差异（差异会逐条打印）。
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STUB_PORT = 18921
PY_PORT = 18931
RS_PORT = 18932


def wait_ready(url: str, timeout: float = 20.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url + "/health", timeout=1) as resp:
                if resp.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.15)
    raise RuntimeError(f"服务未在 {timeout}s 内就绪: {url}")


def get_json(url: str):
    with urllib.request.urlopen(url, timeout=20) as resp:
        return json.loads(resp.read().decode("utf-8"))


def post_json(url: str, payload: dict):
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


def post_stream(url: str, payload: dict) -> list[dict]:
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"content-type": "application/json"})
    events = []
    with urllib.request.urlopen(req, timeout=60) as resp:
        for raw in resp:
            line = raw.decode("utf-8").strip()
            if not line.startswith("data:"):
                continue
            body = line[5:].strip()
            if body == "[DONE]":
                break
            try:
                events.append(json.loads(body))
            except json.JSONDecodeError:
                continue
    return events


def normalize(value):
    """去掉每次请求都会变的字段；其余逐字段比较。"""
    if isinstance(value, dict):
        return {
            k: normalize(v)
            for k, v in value.items()
            if k not in ("id", "created", "server_time", "uptimeSeconds", "uptime_seconds",
                         "avg_duration_ms", "avg_ttft_ms", "p50_duration_ms",
                         "p95_duration_ms", "max_duration_ms", "ttft_samples")
        }
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def compare(name: str, py_value, rs_value, failures: list[str]) -> None:
    if normalize(py_value) != normalize(rs_value):
        failures.append(f"{name} 不一致\n  python={json.dumps(normalize(py_value), ensure_ascii=False)}\n  rust  ={json.dumps(normalize(rs_value), ensure_ascii=False)}")
    else:
        print(f"[ok] {name}")


def diag_sorted(items) -> list:
    """诊断顺序不作为契约，按内容排序后比较。"""
    return sorted(items or [], key=lambda item: json.dumps(item, ensure_ascii=False, sort_keys=True))


# thinking_not_used 是 Rust 历史独有的 info 级提示（Python 从无此码，不影响保存）；
# 本变更只要求新增诊断码双端一致，比较时过滤该既有差异，避免误报。
KNOWN_DIAG_DIFF = {"thinking_not_used"}


def known_diag_filter(items) -> list:
    return diag_sorted([item for item in (items or []) if item.get("code") not in KNOWN_DIAG_DIFF])


def make_config(path: Path) -> None:
    cfg = {
        "port": 0,
        "host": "127.0.0.1",
        "defaultModel": "stub-anthropic",
        "models": ["stub-anthropic", "stub-openai", "stub-cc"],
        "maxTokens": 1024,
        "timeoutSeconds": 30,
        "accessLog": False,
        "upstreams": {
            "anth": {"protocol": "anthropic", "baseUrl": f"http://127.0.0.1:{STUB_PORT}",
                     "apiKey": "k", "models": ["stub-anthropic"], "default": True,
                     "fetchModels": False},
            "oai": {"protocol": "openai", "baseUrl": f"http://127.0.0.1:{STUB_PORT}",
                    "apiKey": "k", "models": ["stub-openai"], "fetchModels": False},
            "cc": {"protocol": "commandcode", "baseUrl": f"http://127.0.0.1:{STUB_PORT}",
                   "apiKey": "k", "cliVersion": "1.2.3", "models": ["stub-cc"],
                   "fetchModels": False, "modelCatalog": False, "modelCatalogPlan": False},
            "ds": {"protocol": "openai", "baseUrl": f"http://127.0.0.1:{STUB_PORT}",
                   "apiKey": "k", "models": ["stub-ds"], "fetchModels": False,
                   "compat": {"profile": "deepseek"},
                   "modelOverrides": {
                       "stub-ds": {"limits": {"maxOutputTokens": 512},
                                   "capabilities": {"thinking": True}},
                       "stub-*": {"limits": {"maxContextTokens": 1048576}},
                   }},
            "oc": {"protocol": "openai", "baseUrl": f"http://127.0.0.1:{STUB_PORT}",
                   "apiKey": "k", "models": ["stub-oc"], "fetchModels": False,
                   "compat": {"profile": "opencode"}},
        },
    }
    path.write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rust", default=str(ROOT / "rust" / "model-gate" / "target" / "release" / "model-gate.exe"))
    args = parser.parse_args()
    rust_exe = Path(args.rust)
    if not rust_exe.is_file():
        print(f"Rust 可执行文件不存在: {rust_exe}")
        return 1

    tmp = Path(tempfile.mkdtemp(prefix="mg-parity-"))
    cfg = tmp / "bridge.config.json"
    make_config(cfg)
    procs: list[subprocess.Popen] = []
    try:
        stub = subprocess.Popen([sys.executable, str(ROOT / "scripts" / "stub_upstream.py"),
                                 "--port", str(STUB_PORT)], cwd=ROOT)
        procs.append(stub)
        time.sleep(0.8)
        env = dict(os.environ, PYTHONPATH=str(ROOT))
        py = subprocess.Popen([sys.executable, "-m", "modelgate", "--no-gui",
                               "--port", str(PY_PORT), "--config", str(cfg),
                               "--db", str(tmp / "py" / "modelgate.db"), "--force-multi"],
                              cwd=ROOT, env=env)
        procs.append(py)
        rs = subprocess.Popen([str(rust_exe), "--no-gui", "--port", str(RS_PORT),
                               "--config", str(cfg), "--db", str(tmp / "rs" / "modelgate.db"),
                               "--force-multi"], cwd=ROOT)
        procs.append(rs)
        py_base = f"http://127.0.0.1:{PY_PORT}"
        rs_base = f"http://127.0.0.1:{RS_PORT}"
        wait_ready(py_base)
        wait_ready(rs_base)

        failures: list[str] = []
        compare("GET /v1/models", get_json(py_base + "/v1/models"),
                get_json(rs_base + "/v1/models"), failures)
        compare("GET /api/upstreams",
                get_json(py_base + "/api/upstreams")["upstreams"],
                get_json(rs_base + "/api/upstreams")["upstreams"], failures)

        py_cfg = get_json(py_base + "/api/config")
        rs_cfg = get_json(rs_base + "/api/config")
        compare("GET /api/config profiles", py_cfg.get("profiles"), rs_cfg.get("profiles"), failures)
        compare("GET /api/config effective", py_cfg.get("effective"), rs_cfg.get("effective"), failures)
        raw_cfg = json.loads(cfg.read_text(encoding="utf-8"))
        py_diag = post_json(py_base + "/api/config/validate", {"config": raw_cfg})
        rs_diag = post_json(rs_base + "/api/config/validate", {"config": raw_cfg})
        compare("POST /api/config/validate diagnostics",
                known_diag_filter(py_diag.get("diagnostics")),
                known_diag_filter(rs_diag.get("diagnostics")), failures)

        for model in ("stub-anthropic", "stub-openai", "stub-cc", "stub-ds", "stub-oc"):
            payload = {"model": model, "messages": [{"role": "user", "content": "hi"}]}
            py_res = post_json(py_base + "/v1/chat/completions", payload)
            rs_res = post_json(rs_base + "/v1/chat/completions", payload)
            compare(f"non-stream {model}", py_res, rs_res, failures)

            stream_payload = dict(payload, stream=True)
            py_events = post_stream(py_base + "/v1/chat/completions", stream_payload)
            rs_events = post_stream(rs_base + "/v1/chat/completions", stream_payload)
            compare(f"stream {model}", py_events, rs_events, failures)

        compare("usage totals",
                get_json(py_base + "/api/usage/totals?range=all")["totals"],
                get_json(rs_base + "/api/usage/totals?range=all")["totals"], failures)
        compare("usage summary(model)",
                get_json(py_base + "/api/usage/summary?group_by=model&range=all")["rows"],
                get_json(rs_base + "/api/usage/summary?group_by=model&range=all")["rows"], failures)

        if failures:
            print("\n对等失败：")
            for item in failures:
                print(" - " + item)
            return 1
        print("\n全部对等通过")
        return 0
    finally:
        for proc in reversed(procs):
            try:
                proc.terminate()
            except Exception:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
