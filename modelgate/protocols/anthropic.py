# -*- coding: utf-8 -*-
"""anthropic 协议适配：OpenAI 请求 → /v1/messages，Anthropic SSE → OpenAI SSE。

对应 Node 版 model-bridge.js 的 toAnthropic / toOpenAI / pipeStream / anthropicProtocol。
"""
from __future__ import annotations

import json
import time
from typing import Iterable

from ..util import access_log_enabled, log, push_message, safe_json, text_of
from .base import BuildResult, Ctx, ProtocolError, SseWriter, iter_sse_blocks, parse_sse_json


class AnthropicAdapter:
    protocol = "anthropic"

    def __init__(self, thinking_cache, *, downgrade_forced_choice: bool = True) -> None:
        self.thinking = thinking_cache
        self.downgrade_forced_choice = downgrade_forced_choice

    # ── 请求转换 ─────────────────────────────────────────────────────────
    def to_anthropic(self, body: dict, config: dict) -> tuple[dict, dict]:
        """OpenAI Chat 请求体 → Anthropic Messages 请求体。返回 (payload, stats)。"""
        system_parts: list[str] = []
        messages: list[dict] = []
        injected = 0
        missed = 0
        default_max_tokens = int((config or {}).get("maxTokens") or 8192)

        for m in body.get("messages") or []:
            role = (m or {}).get("role")
            if role in ("system", "developer"):
                system_parts.append(text_of(m.get("content")))
                continue
            if role == "tool":
                messages.append({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.get("tool_call_id"),
                        "content": text_of(m.get("content")),
                    }],
                })
                continue
            if role == "assistant" and isinstance(m.get("tool_calls"), list) and m["tool_calls"]:
                blocks: list[dict] = []
                text = text_of(m.get("content"))
                tool_ids = [tc.get("id") for tc in m["tool_calls"]]
                # Anthropic 要求 thinking 块位于 assistant 消息首位，回传时带原 signature
                think = self.thinking.build_block(m, text, tool_ids)
                if think:
                    blocks.append(think)
                    injected += 1
                elif self.thinking.enabled:
                    missed += 1
                if text:
                    blocks.append({"type": "text", "text": text})
                for tc in m["tool_calls"]:
                    blocks.append({
                        "type": "tool_use",
                        "id": tc.get("id"),
                        "name": (tc.get("function") or {}).get("name") or "unknown",
                        "input": safe_json((tc.get("function") or {}).get("arguments")),
                    })
                messages.append({"role": "assistant", "content": blocks})
                continue
            if role == "assistant" and self.thinking.enabled:
                text = text_of(m.get("content"))
                think = self.thinking.build_block(m, text, [])
                if think:
                    blocks = [think]
                    if text:
                        blocks.append({"type": "text", "text": text})
                    messages.append({"role": "assistant", "content": blocks})
                    injected += 1
                    continue
                missed += 1
            push_message(messages, "assistant" if role == "assistant" else "user",
                         text_of(m.get("content")))

        out: dict = {
            "model": "",
            "max_tokens": int(body.get("max_tokens") or body.get("max_completion_tokens")
                              or default_max_tokens),
            "messages": messages,
        }
        system = "\n\n".join(p for p in system_parts if p)
        if system:
            out["system"] = system
        if body.get("temperature") is not None:
            out["temperature"] = body["temperature"]
        if body.get("top_p") is not None:
            out["top_p"] = body["top_p"]
        if body.get("stop"):
            stop = body["stop"]
            out["stop_sequences"] = stop if isinstance(stop, list) else [stop]
        if body.get("stream"):
            out["stream"] = True

        choice = body.get("tool_choice")
        # 客户端声明本轮不调用工具时，不下发工具定义（等价 tool_choice: none）
        if choice == "none":
            tools: list[dict] = []
        else:
            tools = [
                t for t in (body.get("tools") or [])
                if (t or {}).get("type") == "function" and ((t.get("function") or {}).get("name"))
            ]
        downgraded = 0
        if tools:
            out["tools"] = [
                {
                    "name": t["function"]["name"],
                    "description": t["function"].get("description") or "",
                    "input_schema": t["function"].get("parameters")
                    or {"type": "object", "properties": {}},
                }
                for t in tools
            ]
            if choice == "auto":
                out["tool_choice"] = {"type": "auto"}
            elif choice in ("required", "any") or (
                isinstance(choice, dict)
                and (choice.get("type") == "function")
                and ((choice.get("function") or {}).get("name"))
            ):
                # thinking 模式不支持强制选择：默认降级为 auto（模型仍可自主调用工具）
                if self.downgrade_forced_choice:
                    downgraded += 1
                elif isinstance(choice, dict) and (choice.get("function") or {}).get("name"):
                    out["tool_choice"] = {"type": "tool",
                                          "name": choice["function"]["name"]}
                else:
                    out["tool_choice"] = {"type": "any"}
        return out, {"injected": injected, "missed": missed, "downgraded": downgraded}

    @staticmethod
    def to_finish_reason(stop_reason) -> str:
        if stop_reason == "max_tokens":
            return "length"
        if stop_reason == "tool_use":
            return "tool_calls"
        return "stop"

    def to_openai(self, data: dict, requested_model: str) -> dict:
        """Anthropic 响应体 → OpenAI 响应体（非流式）。"""
        blocks = data.get("content") or []
        text = "".join(b.get("text") or "" for b in blocks if b.get("type") == "text")
        thinking = "".join(b.get("thinking") or "" for b in blocks if b.get("type") == "thinking")
        signature = next((b.get("signature") or "" for b in blocks
                          if b.get("type") == "thinking"), "")
        tool_uses = [b for b in blocks if b.get("type") == "tool_use"]
        # 缓存本轮思考内容，供下一轮补回
        self.thinking.remember(thinking, signature, text, [b.get("id") for b in tool_uses])

        message: dict = {"role": "assistant", "content": text}
        if thinking:
            message["reasoning_content"] = thinking
        if tool_uses:
            message["tool_calls"] = [
                {
                    "index": i,
                    "id": b.get("id"),
                    "type": "function",
                    "function": {
                        "name": b.get("name"),
                        "arguments": json.dumps(b.get("input") or {}, ensure_ascii=False),
                    },
                }
                for i, b in enumerate(tool_uses)
            ]

        prompt = int(((data.get("usage") or {}).get("input_tokens")) or 0)
        completion = int(((data.get("usage") or {}).get("output_tokens")) or 0)
        return {
            "id": data.get("id"),
            "object": "chat.completion",
            "created": int(time.time()),
            "model": requested_model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": self.to_finish_reason(data.get("stop_reason")),
            }],
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": prompt + completion,
            },
        }

    # ── 主入口 ───────────────────────────────────────────────────────────
    def build_request(self, ctx: Ctx, config: dict) -> BuildResult:
        payload, stats = self.to_anthropic(ctx.body, config)
        headers = {"content-type": "application/json"}
        headers["anthropic-version"] = ctx.upstream.get("anthropicVersion", "2023-06-01")
        if ctx.upstream.get("apiKey"):
            headers["authorization"] = ctx.client_auth or f"Bearer {ctx.upstream['apiKey']}"
            headers["x-api-key"] = ctx.upstream["apiKey"]
        # thinking 缺失提示：要求必须回传却没命中缓存时给出告警，便于排查思考链断裂
        if self.thinking.enabled and stats["missed"] > 0 and access_log_enabled():
            log(f"注意: {stats['missed']} 条助手消息需要 thinking 但缓存未命中（思考链可能断裂）")
        if stats["downgraded"] > 0 and access_log_enabled():
            log("注意: 强制 tool_choice 已降级为 auto（上游 thinking 模式不支持强制工具选择）")
        payload = {**payload, "model": ctx.upstream_model}
        return BuildResult(
            url=f"{ctx.upstream['baseUrl']}/v1/messages",
            headers=headers,
            payload=payload,
            log_suffix=f" think={stats['injected']}",
        )

    def parse_response(self, raw: str, ctx: Ctx) -> dict:
        try:
            data = json.loads(raw)
        except Exception:
            return {"error": f"上游响应不是合法 JSON: {raw[:200]}"}
        return {"body": self.to_openai(data, ctx.requested_model)}

    def parse_stream(self, lines: Iterable[str], writer: SseWriter, ctx: Ctx) -> dict:
        """Anthropic SSE 流 → OpenAI SSE 流（逐事件改写）。返回 usage。"""
        finish_reason = "stop"
        usage = {"prompt_tokens": 0, "completion_tokens": 0}
        finished = False
        tool_index = -1
        acc_thinking = ""
        acc_signature = ""
        acc_text = ""
        acc_tool_ids: list[str] = []

        def emit(event: str, data: dict) -> None:
            nonlocal finish_reason, finished, tool_index, acc_thinking, acc_signature, acc_text
            if event == "message_start":
                usage["prompt_tokens"] = int(
                    (((data.get("message") or {}).get("usage") or {}).get("input_tokens")) or 0)
                writer.write_chunk({"role": "assistant", "content": ""})
                return
            if event == "content_block_start":
                block = data.get("content_block") or {}
                if block.get("type") == "tool_use":
                    tool_index += 1
                    acc_tool_ids.append(block.get("id"))
                    writer.write_chunk({
                        "tool_calls": [{
                            "index": tool_index,
                            "id": block.get("id"),
                            "type": "function",
                            "function": {"name": block.get("name"), "arguments": ""},
                        }]
                    })
                return
            if event == "content_block_delta":
                d = data.get("delta") or {}
                dtype = d.get("type")
                if dtype == "text_delta":
                    acc_text += d.get("text") or ""
                    writer.write_chunk({"content": d.get("text") or ""})
                elif dtype == "thinking_delta":
                    acc_thinking += d.get("thinking") or ""
                    writer.write_chunk({"reasoning_content": d.get("thinking") or ""})
                elif dtype == "signature_delta":
                    acc_signature = d.get("signature") or acc_signature
                elif dtype == "input_json_delta" and tool_index >= 0:
                    writer.write_chunk({
                        "tool_calls": [{
                            "index": tool_index,
                            "function": {"arguments": d.get("partial_json") or ""},
                        }]
                    })
                return
            if event == "message_delta":
                stop = (data.get("delta") or {}).get("stop_reason")
                if stop:
                    finish_reason = self.to_finish_reason(stop)
                out_tokens = (data.get("usage") or {}).get("output_tokens")
                if out_tokens is not None:
                    usage["completion_tokens"] = int(out_tokens or 0)
                return
            if event == "message_stop":
                finished = True
                self.thinking.remember(acc_thinking, acc_signature, acc_text, acc_tool_ids)
                writer.write_chunk({}, finish_reason=finish_reason, usage={
                    "prompt_tokens": usage["prompt_tokens"],
                    "completion_tokens": usage["completion_tokens"],
                    "total_tokens": usage["prompt_tokens"] + usage["completion_tokens"],
                })
                writer.write_done()

        for event, data_lines in iter_sse_blocks(lines):
            parsed = parse_sse_json(event, data_lines)
            if parsed is None:
                continue
            emit(event, parsed)
        if not finished:
            writer.write_done()
        return dict(usage)
