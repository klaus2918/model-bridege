# -*- coding: utf-8 -*-
"""本地可通的桩上游：同时说 anthropic / openai / commandcode 三种协议。

用途：在没有任何可用真实上游的环境里，验证「客户端 → Model-Gate.exe → 上游」
整条代理链路是通的，并且响应体、流式、用量落库、访问日志都正确。

用法：py -3.11 scripts\\stub_upstream.py --port 18901
"""
from __future__ import annotations

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = "桩上游已收到请求，代理链路正常。"


class StubHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # 静默
        return

    def do_GET(self) -> None:  # noqa: N802
        if self.path.startswith("/v1/models"):
            payload = {"object": "list", "data": [
                {"id": "stub-anthropic", "object": "model", "created": 0,
                 "owned_by": "stub"},
                {"id": "stub-openai", "object": "model", "created": 0,
                 "owned_by": "stub"},
                {"id": "stub-cc", "object": "model", "created": 0,
                 "owned_by": "stub"},
            ]}
            self._json(200, payload)
        else:
            self._json(404, {"error": {"message": "stub 未实现"}})

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw.decode("utf-8") or "{}")
        except Exception:
            body = {}
        stream = bool(body.get("stream")) if isinstance(body, dict) else False

        if self.path.startswith("/v1/messages"):
            self._anthropic(body, stream)
        elif self.path.startswith("/v1/chat/completions"):
            self._openai(body, stream)
        elif self.path.startswith("/alpha/generate"):
            self._commandcode()
        else:
            self._json(404, {"error": {"message": "stub 未实现该路径"}})

    # ── anthropic ─────────────────────────────────────────────────────────
    def _anthropic(self, body: dict, stream: bool) -> None:
        payload = {
            "id": "msg_stub",
            "type": "message",
            "role": "assistant",
            "model": body.get("model"),
            "content": [
                {"type": "thinking", "thinking": "思考中", "signature": "sig1"},
                {"type": "text", "text": ANSWER},
                {"type": "tool_use", "id": "tu_1", "name": "read",
                 "input": {"path": "a"}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 11, "output_tokens": 7},
        }
        if not stream:
            self._json(200, payload)
            return
        events = [
            ("message_start", {"message": {"usage": {"input_tokens": 11}}}),
            ("content_block_start", {"index": 0,
                                     "content_block": {"type": "thinking",
                                                       "thinking": ""}}),
            ("content_block_delta", {"index": 0, "delta": {
                "type": "thinking_delta", "thinking": "思考中"}}),
            ("content_block_start", {"index": 1,
                                     "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"index": 1, "delta": {
                "type": "text_delta", "text": ANSWER}}),
            ("content_block_start", {"index": 2, "content_block": {
                "type": "tool_use", "id": "tu_1", "name": "read", "input": {}}}),
            ("content_block_delta", {"index": 2, "delta": {
                "type": "input_json_delta", "partial_json": '{"path":"a"}'}}),
            ("message_delta", {"delta": {"stop_reason": "tool_use"},
                               "usage": {"output_tokens": 7}}),
            ("message_stop", {}),
        ]
        lines = "".join(
            f"event: {ev}\ndata: {json.dumps(data, ensure_ascii=False)}\n\n"
            for ev, data in events)
        self._text(200, lines, "text/event-stream; charset=utf-8", close=True)

    # ── openai ────────────────────────────────────────────────────────────
    def _openai(self, body: dict, stream: bool) -> None:
        payload = {
            "id": "chatcmpl_stub",
            "object": "chat.completion",
            "model": body.get("model"),
            "choices": [{"index": 0, "message": {"role": "assistant",
                                                 "content": ANSWER},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 5,
                      "total_tokens": 14},
        }
        if not stream:
            self._json(200, payload)
            return
        chunks = [
            {"id": "c", "model": body.get("model"),
             "choices": [{"index": 0, "delta": {"role": "assistant"},
                          "finish_reason": None}]},
            {"id": "c", "model": body.get("model"),
             "choices": [{"index": 0, "delta": {"content": ANSWER},
                          "finish_reason": None}]},
            {"id": "c", "model": body.get("model"),
             "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
             "usage": {"prompt_tokens": 9, "completion_tokens": 5,
                       "prompt_tokens_details": {"cached_tokens": 4}}},
            {"id": "c"},
        ]
        lines = "".join(f"data: {json.dumps(c, ensure_ascii=False)}\n\n"
                        for c in chunks) + "data: [DONE]\n\n"
        self._text(200, lines, "text/event-stream; charset=utf-8", close=True)

    # ── commandcode ───────────────────────────────────────────────────────
    def _commandcode(self) -> None:
        events = [
            {"type": "text-delta", "text": ANSWER[:6]},
            {"type": "text-delta", "text": ANSWER[6:]},
            {"type": "finish", "finishReason": "stop",
             "totalUsage": {"inputTokens": 21, "outputTokens": 13,
                            "inputTokenDetails": {"cacheReadTokens": 6,
                                                  "cacheWriteTokens": 2}}},
        ]
        lines = "".join(json.dumps(e, ensure_ascii=False) + "\n" for e in events)
        self._text(200, lines, "application/x-ndjson; charset=utf-8", close=False)

    # ── helpers ───────────────────────────────────────────────────────────
    def _json(self, status: int, payload) -> None:
        self._text(status, json.dumps(payload, ensure_ascii=False),
                   "application/json; charset=utf-8", close=False)

    def _text(self, status: int, body: str, content_type: str, close: bool) -> None:
        data = body.encode("utf-8")
        try:
            self.send_response(status)
            self.send_header("content-type", content_type)
            if close:
                self.send_header("connection", "close")
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True
            else:
                self.send_header("content-length", str(len(data)))
                self.send_header("connection", "close")
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True
        except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
            self.close_connection = True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18901)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), StubHandler)
    print(f"[stub] listening http://127.0.0.1:{args.port}", flush=True)
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
