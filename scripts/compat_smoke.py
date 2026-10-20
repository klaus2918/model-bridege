# -*- coding: utf-8 -*-
"""Rust 内核 compat 端到端冒烟：用本地假上游验证请求头/端点确实由配置驱动。

用法（在项目根）：
    py scripts/compat_smoke.py

覆盖：
- 自定义鉴权头 + 会话头 + 额外请求头 + 自定义端点（openai wire）
- commandcode 预设：x-session-id / sess16 / ZDR / 版本头 / 环境头 / /alpha/generate
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "rust" / "model-gate" / "target" / "release" / "model-gate.exe"
CAPTURED: list[dict] = []


class FakeUpstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # noqa: D102
        return

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("content-length") or 0)
        self.rfile.read(length)
        CAPTURED.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}})
        if self.path.endswith("/alpha/generate"):
            self._send(200, "application/x-ndjson",
                       '{"type":"text-delta","text":"ok"}\n'
                       '{"type":"finish","finishReason":"stop",'
                       '"totalUsage":{"inputTokens":1,"outputTokens":1}}\n')
        else:
            self._send(200, "application/json", json.dumps({
                "id": "x", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1},
            }))

    def _send(self, status: int, ctype: str, body: str) -> None:
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_health(port: int, timeout: float = 8.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=0.5) as res:
                if res.status == 200:
                    return
        except Exception:
            time.sleep(0.15)
    raise RuntimeError("Rust 内核未在预期时间内就绪")


def post(port: int, prefix: str, model: str = "m") -> dict:
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/{prefix}/v1/chat/completions",
        data=json.dumps({"model": model, "messages": [{"role": "user", "content": "hi"}]}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as res:
        return json.loads(res.read().decode() or "{}")


def main() -> int:
    if not EXE.exists():
        print(f"缺少 release 内核：{EXE}（先 cargo build --release）", file=sys.stderr)
        return 2
    upstream_port, proxy_port = free_port(), free_port()
    server = ThreadingHTTPServer(("127.0.0.1", upstream_port), FakeUpstream)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    config = {
        "host": "127.0.0.1", "port": proxy_port, "defaultModel": "m",
        "models": ["m"], "maxTokens": 64, "timeoutSeconds": 30,
        "upstreams": {
            "probe": {
                "protocol": "openai", "baseUrl": f"http://127.0.0.1:{upstream_port}/v1",
                "apiKey": "sk-test", "models": ["m"],
                "compat": {
                    "auth": {"scheme": "header", "header": "x-api-key", "prefix": ""},
                    "session": {"enabled": True, "header": "x-opencode-session"},
                    "headers": {"x-custom": "1"},
                },
            },
            "cc": {
                "protocol": "commandcode", "baseUrl": f"http://127.0.0.1:{upstream_port}",
                "apiKey": "cc-test", "cliVersion": "1.2.3", "cliEnvironment": "staging",
                "models": ["m"],
            },
        },
    }
    tmpdir = tempfile.mkdtemp(prefix="modelgate-compat-")
    cfg = Path(tmpdir) / "bridge.config.json"
    cfg.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    env = dict(os.environ)
    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--force-multi", "--no-generate-config",
         "--port", str(proxy_port), "--config", str(cfg),
         "--db", str(Path(tmpdir) / "data" / "modelgate.db")],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env,
    )
    try:
        wait_health(proxy_port)
        post(proxy_port, "probe")
        post(proxy_port, "cc")
    finally:
        proc.terminate()
        server.shutdown()

    assert len(CAPTURED) == 2, CAPTURED
    probe, cc = CAPTURED
    failures: list[str] = []

    def expect(cond: bool, message: str) -> None:
        if not cond:
            failures.append(message)

    expect(probe["path"] == "/v1/v1/chat/completions" or probe["path"] == "/v1/chat/completions",
           f"probe 端点异常：{probe['path']}")
    expect(probe["headers"].get("x-api-key") == "sk-test", "自定义鉴权头未生效")
    expect("authorization" not in probe["headers"], "自定义鉴权不应再发 Bearer")
    expect(probe["headers"].get("x-opencode-session"), "会话头未注入")
    expect(probe["headers"].get("x-custom") == "1", "额外请求头未生效")

    expect(cc["path"] == "/alpha/generate", f"commandcode 端点异常：{cc['path']}")
    expect(cc["headers"].get("authorization") == "Bearer cc-test", "commandcode Bearer 未生效")
    expect(cc["headers"].get("x-session-id", "").startswith("sess_"), "commandcode 会话头异常")
    expect(cc["headers"].get("x-cmd-zdr") == "1", "ZDR 头未发送")
    expect(cc["headers"].get("x-command-code-version") == "1.2.3", "CLI 版本头未发送")
    expect(cc["headers"].get("x-cli-environment") == "staging", "CLI 环境头未发送")
    expect(cc["headers"].get("user-agent") == "cli", "commandcode user-agent 未生效")

    if failures:
        print("FAIL")
        for item in failures:
            print(" -", item)
        return 1
    print("OK: compat 请求头/端点全部由配置驱动")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
