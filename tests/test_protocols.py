# -*- coding: utf-8 -*-
"""协议适配层单测：请求转换与流解析（离线，不依赖真实上游）。"""
from __future__ import annotations

import io
import json

from modelgate.protocols.base import Ctx, SseWriter
from modelgate.protocols.anthropic import AnthropicAdapter
from modelgate.protocols.commandcode import CommandCodeAdapter
from modelgate.protocols.openai_cc import OpenAIAdapter, SessionRegistry
from modelgate.protocols.thinking import ThinkingCache


def make_writer(model: str = "stub") -> tuple[SseWriter, io.BytesIO]:
    buf = io.BytesIO()
    return SseWriter(buf, model), buf


def sse_lines(payloads: list[tuple[str, dict]]):
    """构造 SSE 文本行（含空行分隔）。"""
    out: list[str] = []
    for event, data in payloads:
        out.append(f"event: {event}")
        out.append(f"data: {json.dumps(data, ensure_ascii=False)}")
        out.append("")
    return out


def ndjson_lines(events: list[dict]) -> list[str]:
    return [json.dumps(e, ensure_ascii=False) for e in events]


def parse_chunks(buf: io.BytesIO) -> list[dict]:
    text = buf.getvalue().decode("utf-8")
    out = []
    for line in text.splitlines():
        if line.startswith("data: ") and line != "data: [DONE]":
            out.append(json.loads(line[6:]))
    return out


# ── anthropic ─────────────────────────────────────────────────────────────
def test_anthropic_request_converts_system_and_tools():
    adapter = AnthropicAdapter(ThinkingCache(enabled=True))
    body = {
        "model": "alias-a",
        "max_tokens": 123,
        "messages": [
            {"role": "system", "content": "你是助手"},
            {"role": "user", "content": "你好"},
            {"role": "assistant", "content": "在", "tool_calls": [
                {"id": "t1", "type": "function",
                 "function": {"name": "read", "arguments": '{"path":"a"}'}}]},
            {"role": "tool", "content": "结果", "tool_call_id": "t1"},
        ],
        "tools": [{"type": "function", "function": {
            "name": "read", "description": "读文件", "parameters": {"type": "object"}}}],
        "tool_choice": "auto",
        "stream": True,
        "temperature": 0.2,
    }
    payload, stats = adapter.to_anthropic(body, {"maxTokens": 8192})

    assert payload["system"] == "你是助手"
    assert payload["max_tokens"] == 123
    assert payload["stream"] is True
    assert payload["temperature"] == 0.2
    assert payload["messages"][0]["role"] == "user"
    assert payload["messages"][1]["role"] == "assistant"
    blocks = payload["messages"][1]["content"]
    assert blocks[0]["type"] == "text" and blocks[0]["text"] == "在"
    assert blocks[1]["type"] == "tool_use" and blocks[1]["id"] == "t1"
    assert blocks[1]["input"] == {"path": "a"}
    assert payload["messages"][2]["content"][0]["type"] == "tool_result"
    assert payload["tools"][0]["name"] == "read"
    assert payload["tool_choice"] == {"type": "auto"}
    assert stats == {"injected": 0, "missed": 1, "downgraded": 0}


def test_anthropic_forced_tool_choice_downgrades_by_default():
    adapter = AnthropicAdapter(ThinkingCache(enabled=False), downgrade_forced_choice=True)
    payload, stats = adapter.to_anthropic({
        "messages": [{"role": "user", "content": "x"}],
        "tools": [{"type": "function", "function": {"name": "f"}}],
        "tool_choice": {"type": "function", "function": {"name": "f"}},
    }, {"maxTokens": 10})
    assert "tool_choice" not in payload
    assert stats["downgraded"] == 1

    strict = AnthropicAdapter(ThinkingCache(enabled=False), downgrade_forced_choice=False)
    payload2, stats2 = strict.to_anthropic({
        "messages": [{"role": "user", "content": "x"}],
        "tools": [{"type": "function", "function": {"name": "f"}}],
        "tool_choice": {"type": "function", "function": {"name": "f"}},
    }, {"maxTokens": 10})
    assert payload2["tool_choice"] == {"type": "tool", "name": "f"}
    assert stats2["downgraded"] == 0


def test_anthropic_thinking_cache_roundtrip():
    cache = ThinkingCache(enabled=True, max_size=10)
    cache.remember("我在思考", "sig-1", "答案", ["t9"])

    # 按 tool_call id 命中
    block = cache.build_block({}, "别的文本", ["t9"])
    assert block == {"type": "thinking", "thinking": "我在思考", "signature": "sig-1"}

    # 按文本指纹命中
    block2 = cache.build_block({}, "答案", [])
    assert block2 and block2["thinking"] == "我在思考"

    # 客户端显式带 reasoning_content 时以它为准
    block3 = cache.build_block({"reasoning_content": "显式思考"}, "答案", [])
    assert block3["thinking"] == "显式思考"

    # 关闭开关后不再自动补回
    cache.enabled = False
    assert cache.build_block({}, "答案", []) is None


def test_anthropic_response_to_openai():
    adapter = AnthropicAdapter(ThinkingCache(enabled=True))
    data = {
        "id": "msg_1",
        "stop_reason": "tool_use",
        "content": [
            {"type": "thinking", "thinking": "想", "signature": "s"},
            {"type": "text", "text": "答"},
            {"type": "tool_use", "id": "tu", "name": "read", "input": {"p": 1}},
        ],
        "usage": {"input_tokens": 11, "output_tokens": 5},
    }
    out = adapter.to_openai(data, "stub-anthropic")
    assert out["object"] == "chat.completion"
    assert out["model"] == "stub-anthropic"
    msg = out["choices"][0]["message"]
    assert msg["content"] == "答"
    assert msg["reasoning_content"] == "想"
    assert msg["tool_calls"][0]["function"]["name"] == "read"
    assert out["choices"][0]["finish_reason"] == "tool_calls"
    assert out["usage"] == {"prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16}
    # 思考内容已被缓存，可被后续轮次回传
    assert adapter.thinking.recall("答", ["tu"]) is not None


def test_anthropic_stream_rewrites_to_openai_sse():
    adapter = AnthropicAdapter(ThinkingCache(enabled=True))
    writer, buf = make_writer("stub-anthropic")
    ctx = Ctx(body={}, requested_model="stub-anthropic", upstream_model="m",
              upstream={"name": "anth", "protocol": "anthropic"})
    lines = sse_lines([
        ("message_start", {"message": {"usage": {"input_tokens": 7}}}),
        ("content_block_delta", {"delta": {"type": "text_delta", "text": "你好"}}),
        ("content_block_delta", {"delta": {"type": "thinking_delta", "thinking": "想"}}),
        ("content_block_start", {"content_block": {"type": "tool_use",
                                                   "id": "t1", "name": "read"}}),
        ("content_block_delta", {"delta": {"type": "input_json_delta",
                                           "partial_json": '{"p":1}'}}),
        ("message_delta", {"delta": {"stop_reason": "tool_use"},
                           "usage": {"output_tokens": 3}}),
        ("message_stop", {}),
    ])
    usage = adapter.parse_stream(lines, writer, ctx)
    chunks = parse_chunks(buf)

    assert usage == {"prompt_tokens": 7, "completion_tokens": 3}
    assert chunks[0]["choices"][0]["delta"] == {"role": "assistant", "content": ""}
    contents = [c for c in chunks if c["choices"][0]["delta"].get("content")]
    assert contents and contents[0]["choices"][0]["delta"]["content"] == "你好"
    reasoning = [c for c in chunks if c["choices"][0]["delta"].get("reasoning_content")]
    assert reasoning
    tool_starts = [c for c in chunks
                   if c["choices"][0]["delta"].get("tool_calls", [{}])[0].get("id")]
    assert tool_starts and tool_starts[0]["choices"][0]["delta"]["tool_calls"][0]["id"] == "t1"
    final = chunks[-1]
    assert final["choices"][0]["finish_reason"] == "tool_calls"
    assert final["usage"]["total_tokens"] == 10
    assert buf.getvalue().decode().endswith("data: [DONE]\n\n")


def test_anthropic_stream_without_stop_sends_done():
    adapter = AnthropicAdapter(ThinkingCache(enabled=True))
    writer, buf = make_writer()
    ctx = Ctx(body={}, requested_model="m", upstream_model="m", upstream={"name": "a"})
    usage = adapter.parse_stream(sse_lines([
        ("message_start", {"message": {"usage": {"input_tokens": 2}}}),
        ("content_block_delta", {"delta": {"type": "text_delta", "text": "x"}}),
    ]), writer, ctx)
    assert usage == {"prompt_tokens": 2, "completion_tokens": 0}
    assert buf.getvalue().decode().rstrip().endswith("data: [DONE]")


# ── openai ────────────────────────────────────────────────────────────────
def test_openai_build_request_sets_session_header_and_model():
    adapter = OpenAIAdapter(SessionRegistry())
    up = {"name": "oai", "baseUrl": "http://u", "apiKey": "k",
          "protocol": "openai", "sessionHeader": True}
    ctx = Ctx(body={"model": "alias-a", "messages": [{"role": "user", "content": "hi"}]},
              requested_model="alias-a", upstream_model="stub-openai", upstream=up)
    build = adapter.build_request(ctx, {})
    assert build.url == "http://u/v1/chat/completions"
    assert build.headers["authorization"] == "Bearer k"
    assert "x-opencode-session" in build.headers
    assert build.payload["model"] == "stub-openai"

    # 同一请求内容重复构建，会话头保持稳定
    again = adapter.build_request(ctx, {})
    assert again.headers["x-opencode-session"] == build.headers["x-opencode-session"]


def test_openai_empty_key_does_not_send_authorization():
    adapter = OpenAIAdapter()
    up = {"name": "zen", "baseUrl": "http://u", "apiKey": "",
          "protocol": "openai", "sessionHeader": False}
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="m", upstream=up)
    build = adapter.build_request(ctx, {})
    assert "authorization" not in build.headers


def test_openai_stream_rewrites_model_and_captures_usage():
    adapter = OpenAIAdapter()
    writer, buf = make_writer("alias-a")
    ctx = Ctx(body={}, requested_model="alias-a", upstream_model="stub-openai",
              upstream={"name": "oai", "protocol": "openai"})
    lines = sse_lines([
        ("message", {"id": "1", "model": "stub-openai",
                     "choices": [{"index": 0, "delta": {"content": "a"},
                                  "finish_reason": None}]}),
        ("message", {"id": "1", "model": "stub-openai",
                     "choices": [{"index": 0, "delta": {},
                                  "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 4, "completion_tokens": 6,
                               "prompt_tokens_details": {"cached_tokens": 3}}}),
        ("message", {"id": "1"}),
    ])
    usage = adapter.parse_stream(lines, writer, ctx)
    chunks = parse_chunks(buf)
    with_model = [c for c in chunks if "model" in c]
    assert with_model and all(c["model"] == "alias-a" for c in with_model)
    assert usage == {"prompt_tokens": 4, "completion_tokens": 6, "cacheReadTokens": 3}


def test_openai_parse_response_rewrites_model():
    adapter = OpenAIAdapter()
    ctx = Ctx(body={}, requested_model="alias-a", upstream_model="stub-openai",
              upstream={"name": "oai"})
    parsed = adapter.parse_response(json.dumps({
        "id": "x", "model": "stub-openai",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "y"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1},
    }), ctx)
    assert parsed["body"]["model"] == "alias-a"
    assert parsed["meta"]["usage"]["prompt_tokens"] == 2
    assert adapter.parse_response("not json", ctx)["error"].startswith("上游响应不是合法 JSON")


# ── commandcode ───────────────────────────────────────────────────────────
def test_commandcode_request_headers_include_zdr_and_session():
    adapter = CommandCodeAdapter(SessionRegistry(), post_lines=None)
    up = {"name": "cc", "baseUrl": "http://u", "apiKey": "ck",
          "apiKeySource": "literal", "protocol": "commandcode",
          "cliVersion": "1.2.3", "cliEnvironment": "production",
          "cmdZdr": True}
    ctx = Ctx(body={"model": "m", "messages": [{"role": "user", "content": "hi"}]},
              requested_model="m", upstream_model="stub-cc", upstream=up)
    build = adapter.build_request(ctx, {})
    assert build.error is None
    assert build.url == "http://u/alpha/generate"
    assert build.headers["x-cmd-zdr"] == "1"
    assert build.headers["x-command-code-version"] == "1.2.3"
    assert build.headers["x-cli-environment"] == "production"
    assert build.headers["authorization"] == "Bearer ck"
    assert build.headers["x-session-id"].startswith("sess_")
    assert build.payload["params"]["stream"] is True
    assert build.payload["params"]["model"] == "stub-cc"
    # threadId 必须是合法 UUID，否则上游会静默丢弃
    assert len(build.payload["threadId"]) == 36


def test_commandcode_missing_credentials_fails_locally():
    adapter = CommandCodeAdapter(SessionRegistry())
    up = {"name": "cc", "baseUrl": "http://u", "apiKey": "", "apiKeySource": "none",
          "protocol": "commandcode"}
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="m", upstream=up)
    build = adapter.build_request(ctx, {})
    assert build.error is not None
    assert build.error.status == 401
    assert build.error.code == "missing_api_key"


def test_commandcode_event_union_aggregation():
    adapter = CommandCodeAdapter(SessionRegistry())
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="stub-cc",
              upstream={"name": "cc", "protocol": "commandcode"})
    raw = "\n".join(ndjson_lines([
        {"type": "text-delta", "text": "你"},
        {"type": "text-delta", "text": "好"},
        {"type": "reasoning-delta", "text": "想"},
        {"type": "tool-input-start", "id": "tc1", "toolName": "read"},
        {"type": "tool-input-delta", "id": "tc1", "delta": '{"p":'},
        {"type": "tool-input-delta", "id": "tc1", "delta": "1}"},
        {"type": "finish", "finishReason": "tool_use",
         "totalUsage": {"inputTokens": 9, "outputTokens": 4,
                        "inputTokenDetails": {"cacheReadTokens": 5}}},
    ]))
    parsed = adapter.parse_response(raw, ctx)
    result = parsed["body"]
    assert result["choices"][0]["message"]["content"] == "你好"
    assert result["choices"][0]["message"]["reasoning_content"] == "想"
    assert result["choices"][0]["finish_reason"] == "tool_calls"
    assert result["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] == '{"p":1}'
    assert result["usage"] == {
        "prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13,
        "prompt_tokens_details": {"cached_tokens": 5},
    }
    assert parsed["meta"]["truncated"] is False


def test_commandcode_truncated_without_finish():
    adapter = CommandCodeAdapter(SessionRegistry())
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="m",
              upstream={"name": "cc", "protocol": "commandcode"})
    parsed = adapter.parse_response(
        "\n".join(ndjson_lines([{"type": "text-delta", "text": "半截"}])), ctx)
    assert parsed["meta"]["truncated"] is True


def test_commandcode_stream_and_pause_turn_resume():
    """pause_turn 触发续跑：同一份 body 重发，usage 跨次累加，文本连续下发。"""
    calls = []

    def post_lines(url, headers, payload, timeout):
        calls.append({"url": url, "payload": payload})
        # 第一次续跑仍返回 pause_turn，第二次给终态（验证上限内的多次续跑）
        if len(calls) == 1:
            events = [
                {"type": "text-delta", "text": "第二段"},
                {"type": "finish", "finishReason": "pause_turn",
                 "totalUsage": {"inputTokens": 3, "outputTokens": 1}},
            ]
        else:
            events = [
                {"type": "text-delta", "text": "第三段"},
                {"type": "finish", "finishReason": "stop",
                 "totalUsage": {"inputTokens": 2, "outputTokens": 1}},
            ]
        return iter(ndjson_lines(events))

    adapter = CommandCodeAdapter(SessionRegistry(), post_lines=post_lines)
    writer, buf = make_writer("m")
    build = type("B", (), {"url": "http://u/alpha/generate", "headers": {},
                           "payload": {"params": {}}})()
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="stub-cc",
              upstream={"name": "cc", "protocol": "commandcode"},
              pause_turn=True, request=build)
    lines = ndjson_lines([
        {"type": "text-delta", "text": "第一段"},
        {"type": "finish", "finishReason": "pause_turn",
         "totalUsage": {"inputTokens": 5, "outputTokens": 2}},
    ])
    usage = adapter.parse_stream(lines, writer, ctx)
    chunks = parse_chunks(buf)

    assert len(calls) == 2  # 续跑了两次（首次流 + 两次续跑中的一次 pause）
    texts = [c["choices"][0]["delta"].get("content") for c in chunks]
    assert "第一段" in texts and "第二段" in texts and "第三段" in texts
    # usage 跨续跑累加：5+3+2 / 2+1+1
    assert usage["prompt_tokens"] == 10
    assert usage["completion_tokens"] == 4
    assert usage["paused"] is False
    assert usage["truncated"] is False
    assert buf.getvalue().decode().rstrip().endswith("data: [DONE]")


def test_commandcode_pause_turn_gives_up_at_limit():
    def post_lines(url, headers, payload, timeout):
        return iter(ndjson_lines([
            {"type": "finish", "finishReason": "pause_turn",
             "totalUsage": {"inputTokens": 1, "outputTokens": 1}},
        ]))

    adapter = CommandCodeAdapter(SessionRegistry(), post_lines=post_lines)
    writer, _buf = make_writer("m")
    build = type("B", (), {"url": "http://u", "headers": {}, "payload": {}})()
    ctx = Ctx(body={"model": "m"}, requested_model="m", upstream_model="m",
              upstream={"name": "cc", "protocol": "commandcode"},
              pause_turn=True, request=build)
    usage = adapter.parse_stream(
        ndjson_lines([{"type": "finish", "finishReason": "pause_turn",
                       "totalUsage": {"inputTokens": 1, "outputTokens": 1}}]),
        writer, ctx)
    # 首次 + 5 次续跑后仍 pause_turn → 标记未续完
    assert usage["paused"] is True
    assert usage["truncated"] is False


def test_commandcode_messages_and_system():
    messages = CommandCodeAdapter.to_messages([
        {"role": "system", "content": "sys"},
        {"role": "user", "content": [{"type": "text", "text": "hi"}]},
        {"role": "assistant", "content": "ok",
         "reasoning_content": "想", "tool_calls": [
             {"id": "t1", "function": {"name": "f", "arguments": "{}"}}]},
        {"role": "tool", "content": "res", "tool_call_id": "t1"},
    ])
    # system 被剥离（由 build_request 单独处理），其余三类保留
    assert [m["role"] for m in messages] == ["user", "assistant", "tool"]
    assert messages[1]["content"][0] == {"type": "reasoning", "text": "想"}
    assert messages[2]["content"][0]["toolName"] == "f"
    assert CommandCodeAdapter.to_system(["a", "b"]) == [
        {"type": "text", "text": "a\n"}, {"type": "text", "text": "b"}]
