# -*- coding: utf-8 -*-
"""协议适配层公共设施：上下文对象、SSE 写出器、流解析辅助。"""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from typing import Callable, Iterable, Optional


@dataclass
class ProtocolError:
    """协议在本地即可判定的失败（不打上游）。"""

    message: str
    status: int = 500
    type: str = "upstream_error"
    code: str = "upstream_failed"

    def body(self) -> dict:
        return {"error": {"message": self.message, "type": self.type, "code": self.code}}


@dataclass
class BuildResult:
    """build_request 的产物。error 非空时主流程直接返回，不再发起上游请求。"""

    url: str = ""
    headers: dict = field(default_factory=dict)
    payload: dict = field(default_factory=dict)
    log_suffix: str = ""
    error: Optional[ProtocolError] = None


@dataclass
class Ctx:
    """协议适配层的上下文。"""

    body: dict
    requested_model: str
    upstream_model: str
    upstream: dict
    client_auth: Optional[str] = None
    on_usage: Optional[Callable[[dict], None]] = None
    # 供 pause_turn 续跑复用同一份请求
    request: Optional[BuildResult] = None
    pause_turn: bool = False
    timeout: float = 600.0
    # 是否处于流式响应模式
    stream: bool = False


class SseWriter:
    """把 OpenAI 形状的 chunk 写成 SSE 流（下游看到的是标准 OpenAI SSE）。"""

    def __init__(self, wfile, requested_model: str) -> None:
        self._wfile = wfile
        self._model = requested_model
        self.broken = False  # 客户端断开后置 True，后续写入直接丢弃

    def write_raw(self, text: str) -> None:
        """写出一段 SSE 文本；客户端断开时静默收尾（流式场景客户端随时可能离开）。"""
        try:
            self._wfile.write(text.encode("utf-8"))
            try:
                self._wfile.flush()
            except Exception:
                pass
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError,
                OSError):
            self.broken = True

    def chunk(self, delta: dict, *, finish_reason=None, usage: Optional[dict] = None,
              extra: Optional[dict] = None) -> str:
        payload = {
            "id": "chatcmpl-bridge",
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": self._model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        }
        if usage is not None:
            payload["usage"] = usage
        if extra:
            payload.update(extra)
        return f"data: {json.dumps(payload, ensure_ascii=False)}\n\n"

    def write_chunk(self, delta: dict, **kwargs) -> None:
        self.write_raw(self.chunk(delta, **kwargs))

    def write_done(self) -> None:
        self.write_raw("data: [DONE]\n\n")


def iter_sse_blocks(lines: Iterable[str]):
    """把文本行流切成 SSE 块。yield (event_name, [data...])。"""
    event = "message"
    data_lines: list[str] = []
    for line in lines:
        line = line.rstrip("\r")
        if line == "":
            if data_lines:
                yield event, data_lines
            event = "message"
            data_lines = []
            continue
        if line.startswith("event:"):
            event = line[6:].strip()
        elif line.startswith("data:"):
            data_lines.append(line[5:].strip())
    if data_lines:
        yield event, data_lines


def parse_sse_json(event: str, data_lines: list[str]):
    """把 SSE 块的 data 行拼成 JSON；非法 JSON 返回 None。"""
    try:
        return json.loads("\n".join(data_lines))
    except Exception:
        return None


def iter_ndjson(lines: Iterable[str]):
    """逐行产出 NDJSON 解析结果；非 JSON 行静默跳过（上游语义）。"""
    for line in lines:
        text = line.strip()
        if not text:
            continue
        try:
            yield json.loads(text)
        except Exception:
            continue


def usage_payload(usage: dict) -> dict:
    """归一 usage 为 OpenAI 形状（含缓存命中字段）。"""
    prompt = int(usage.get("prompt_tokens") or 0)
    completion = int(usage.get("completion_tokens") or 0)
    out = {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    }
    cache_read = int(usage.get("cacheReadTokens") or 0)
    if cache_read > 0:
        out["prompt_tokens_details"] = {"cached_tokens": cache_read}
    return out


def split_system(messages: list[dict]) -> tuple[list[str], list[dict]]:
    """把 system/developer 消息与其余消息分开。"""
    system_parts: list[str] = []
    rest: list[dict] = []
    for m in messages or []:
        role = (m or {}).get("role")
        if role in ("system", "developer"):
            from ..util import text_of

            system_parts.append(text_of(m.get("content")))
        else:
            rest.append(m)
    return system_parts, rest
