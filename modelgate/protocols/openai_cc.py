# -*- coding: utf-8 -*-
"""openai 协议适配：请求原样透传（只替换模型名），SSE 转发并抓取 usage。

对应 Node 版 model-bridge.js 的 openaiProtocol / passThroughStream。
会话头（x-opencode-session）由 build_request 注入：opencode 类上游缺失会被 400 拒绝。
"""
from __future__ import annotations

import json
import time
import uuid
import threading
from typing import Iterable

from .base import BuildResult, Ctx, SseWriter, iter_sse_blocks, parse_sse_json

OPENCODE_SESSION_HEADER = "x-opencode-session"


class SessionRegistry:
    """由请求内容推导稳定的会话 ID（同一对话反复请求得到同一个值）。"""

    def __init__(self, max_size: int = 2000) -> None:
        self._map: dict[str, str] = {}
        self._lock = threading.Lock()
        self._max_size = max_size

    @staticmethod
    def seed(body: dict) -> str:
        first_user = next(
            (m for m in (body.get("messages") or []) if (m or {}).get("role") == "user"), None
        )
        content = (first_user or {}).get("content")
        return f"{body.get('model') or ''}|{json.dumps(content, ensure_ascii=False, sort_keys=True, default=str)}"

    def get(self, body: dict) -> str:
        key = self.seed(body)
        with self._lock:
            hit = self._map.get(key)
            if hit:
                return hit
            value = uuid.uuid4().hex
            while len(self._map) > self._max_size:
                del self._map[next(iter(self._map))]
            self._map[key] = value
            return value


class OpenAIAdapter:
    protocol = "openai"

    def __init__(self, sessions: SessionRegistry | None = None) -> None:
        self.sessions = sessions or SessionRegistry()

    def session_id(self, body: dict) -> str:
        return self.sessions.get(body)

    def build_request(self, ctx: Ctx, config: dict) -> BuildResult:
        headers = {"content-type": "application/json"}
        # 空 key 表示该上游无需鉴权（如 Zen 免费模型），不能发空的 Authorization 头
        if ctx.upstream.get("apiKey"):
            headers["authorization"] = f"Bearer {ctx.upstream['apiKey']}"
        if ctx.upstream.get("sessionHeader"):
            headers[OPENCODE_SESSION_HEADER] = self.session_id(ctx.body)
        return BuildResult(
            url=f"{ctx.upstream['baseUrl']}/v1/chat/completions",
            headers=headers,
            payload={**ctx.body, "model": ctx.upstream_model},
            log_suffix="",
        )

    def parse_response(self, raw: str, ctx: Ctx) -> dict:
        try:
            result = json.loads(raw)
        except Exception:
            return {"error": f"上游响应不是合法 JSON: {raw[:200]}"}
        result["model"] = ctx.requested_model
        usage = result.get("usage") or {}
        return {
            "body": result,
            "meta": {
                "usage": {
                    "prompt_tokens": int(usage.get("prompt_tokens") or 0),
                    "completion_tokens": int(usage.get("completion_tokens") or 0),
                    "cacheReadTokens": int(
                        ((usage.get("prompt_tokens_details") or {}).get("cached_tokens")) or 0),
                }
            },
        }

    def parse_stream(self, lines: Iterable[str], writer: SseWriter, ctx: Ctx) -> dict:
        """上游 OpenAI SSE 原样转发：只改写 model 字段并抓取 usage。"""
        usage = {"prompt_tokens": 0, "completion_tokens": 0, "cacheReadTokens": 0}
        for event, data_lines in iter_sse_blocks(lines):
            joined = "\n".join(data_lines)
            if not joined.strip():
                continue
            if joined.strip() == "[DONE]":
                writer.write_done()
                continue
            try:
                chunk = json.loads(joined)
            except Exception:
                # 无法解析的块原样透传，保持连接活性
                writer.write_raw(f"data: {joined}\n\n")
                continue
            if isinstance(chunk, dict) and "model" in chunk:
                chunk["model"] = ctx.requested_model
            if isinstance(chunk, dict) and chunk.get("usage"):
                u = chunk["usage"] or {}
                usage["prompt_tokens"] = int(u.get("prompt_tokens") or usage["prompt_tokens"])
                usage["completion_tokens"] = int(
                    u.get("completion_tokens") or usage["completion_tokens"])
                cached = ((u.get("prompt_tokens_details") or {}).get("cached_tokens")) or 0
                usage["cacheReadTokens"] = int(cached or usage["cacheReadTokens"])
            writer.write_raw(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n")
        return dict(usage)
