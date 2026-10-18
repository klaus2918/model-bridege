# -*- coding: utf-8 -*-
"""pytest 公共夹具：本地桩上游 + 代理服务（真实端口，离线可跑）。"""
from __future__ import annotations

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))


class StubUpstream:
    """本地桩上游：按路径返回预置响应，并记录收到的请求（供断言鉴权/头/体）。"""

    def __init__(self) -> None:
        self.responses: dict[str, dict] = {}
        self.requests: list[dict] = []
        self._lock = threading.Lock()
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._make_handler())
        self.port = self._server.server_address[1]
        self.url = f"http://127.0.0.1:{self.port}"
        self._thread = threading.Thread(target=self._server.serve_forever,
                                        kwargs={"poll_interval": 0.05}, daemon=True)
        self._thread.start()

    def _make_handler(self):
        stub = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, fmt, *args):  # 静默
                return

            def do_GET(self):
                self._handle("GET")

            def do_POST(self):
                self._handle("POST")

            def _handle(self, method: str):
                length = int(self.headers.get("content-length") or 0)
                raw = self.rfile.read(length) if length else b""
                try:
                    parsed = json.loads(raw.decode("utf-8")) if raw else {}
                except Exception:
                    parsed = {}
                with stub._lock:
                    stub.requests.append({
                        "method": method,
                        "path": self.path,
                        "headers": {k.lower(): v for k, v in self.headers.items()},
                        "body": parsed,
                        "raw": raw.decode("utf-8", "replace"),
                    })
                spec = stub.responses.get(self.path)
                if spec is None:
                    body = json.dumps({"error": {"message": "stub 未配置该路径"}})
                    payload = body.encode("utf-8")
                    self.send_response(404)
                    self.send_header("content-type", "application/json")
                    self.send_header("content-length", str(len(payload)))
                    self.send_header("connection", "close")
                    self.end_headers()
                    self.wfile.write(payload)
                    self.close_connection = True
                    return
                status = spec.get("status", 200)
                content_type = spec.get("content_type", "application/json; charset=utf-8")
                body = spec.get("body", "")
                if isinstance(body, (dict, list)):
                    body = json.dumps(body, ensure_ascii=False)
                payload = body.encode("utf-8")
                self.send_response(status)
                self.send_header("content-type", content_type)
                if spec.get("chunked"):
                    # SSE / 流式：不写 content-length，靠关闭连接收尾
                    self.send_header("connection", "close")
                    self.end_headers()
                    self.wfile.write(payload)
                    self.close_connection = True
                    return
                self.send_header("content-length", str(len(payload)))
                self.send_header("connection", "close")
                self.end_headers()
                self.wfile.write(payload)
                self.close_connection = True

        return Handler

    def set(self, path: str, body, *, status: int = 200,
            content_type: str = "application/json; charset=utf-8",
            chunked: bool = False) -> None:
        self.responses[path] = {
            "status": status, "body": body,
            "content_type": content_type, "chunked": chunked,
        }

    def last(self, path: str | None = None) -> dict:
        with self._lock:
            rows = [r for r in self.requests if path is None or r["path"] == path]
        if not rows:
            raise AssertionError(f"桩上游未收到 {path} 请求")
        return rows[-1]

    def count(self, path: str | None = None) -> int:
        with self._lock:
            return sum(1 for r in self.requests if path is None or r["path"] == path)

    def reset(self) -> None:
        with self._lock:
            self.requests.clear()

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()


@pytest.fixture()
def stub_upstream():
    stub = StubUpstream()
    try:
        yield stub
    finally:
        stub.close()


@pytest.fixture()
def db(tmp_path):
    from modelgate.db import Database

    database = Database(tmp_path / "test.db")
    try:
        yield database
    finally:
        database.close()


def make_config(stub_url: str, *, port: int = 0) -> dict:
    """构造一个指向桩上游的最小配置（两条上游：anthropic + openai）。"""
    return {
        "port": port,
        "host": "127.0.0.1",
        "defaultModel": "stub-anthropic",
        "models": ["stub-anthropic", "stub-openai", "stub-cc"],
        "maxTokens": 1024,
        "timeoutSeconds": 30,
        "accessLog": False,
        "modelsCacheSeconds": 300,
        "aliases": {"alias-a": "stub-anthropic"},
        "thinking": {"passthrough": True, "cacheSize": 50},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": {
            "anth": {
                "protocol": "anthropic",
                "baseUrl": stub_url,
                "apiKey": "test-key",
                "models": ["stub-anthropic"],
                "default": True,
                "fetchModels": False,
            },
            "oai": {
                "protocol": "openai",
                "baseUrl": stub_url,
                "apiKey": "oai-key",
                "sessionHeader": True,
                "models": ["stub-openai"],
                "fetchModels": False,
            },
            "cc": {
                "protocol": "commandcode",
                "baseUrl": stub_url,
                "apiKey": "cc-key",
                "cliVersion": "1.2.3",
                "modelCatalog": False,
                "modelCatalogPlan": False,
                "pauseTurn": True,
                "cmdZdr": True,
                "models": ["stub-cc"],
                "fetchModels": False,
            },
        },
    }


@pytest.fixture()
def bridge_factory(stub_upstream, db, tmp_path):
    """返回 callable：给定配置 → 已启动的代理服务（随机端口），用完自动关闭。"""
    from modelgate.config import save_config, upstreams_of
    from modelgate.proxy import Bridge, create_server
    import httpx as _httpx

    started = []

    def factory(config: dict | None = None):
        cfg = config or make_config(stub_upstream.url)
        config_path = tmp_path / "bridge.config.json"
        save_config(config_path, cfg)
        bridge = Bridge(cfg, db, config_path=config_path)
        server = create_server(bridge)
        port = server.server_address[1]
        bridge.config["port"] = port
        thread = threading.Thread(target=server.serve_forever,
                                  kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        started.append((server, bridge))
        return type("Handle", (), {
            "base": f"http://127.0.0.1:{port}",
            "bridge": bridge,
            "client": _httpx.Client(timeout=15.0),
            "close": lambda: None,
        })()

    try:
        yield factory
    finally:
        for server, bridge in started:
            server.shutdown()
            server.server_close()
            bridge.close()
