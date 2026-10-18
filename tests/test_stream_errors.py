# -*- coding: utf-8 -*-
"""流式请求 + 上游报错 的回归。

背景（真实用户场景）：客户端要求 `stream:true`，我们用 `send(stream=True)` 拿上游；
若上游此刻返回 4xx/5xx，旧代码直接取 `response.text` 抛
`httpx.ResponseNotRead` → 被兜底成 500「内部错误」→ 既无响应也无落库。
"""
from __future__ import annotations

import json

import httpx

JSON_CT = "application/json; charset=utf-8"
SSE_CT = "text/event-stream; charset=utf-8"
NDJSON_CT = "application/x-ndjson; charset=utf-8"

CHAT = "/v1/chat/completions"


def _post(base, payload):
    return httpx.post(base + CHAT, json=payload, timeout=20)


def test_stream_request_upstream_json_error(bridge_factory, stub_upstream, db):
    """流式请求 + 上游 JSON 错误 → 必须回上游状态码与原文，不能 500。"""
    stub_upstream.set("/v1/messages",
                      {"error": {"message": "rate limited"}},
                      status=429, content_type=JSON_CT)
    handle = bridge_factory()
    res = _post(handle.base, {"model": "stub-anthropic", "stream": True,
                              "messages": [{"role": "user", "content": "hi"}]})
    assert res.status_code == 429, res.text
    assert "rate limited" in res.text
    assert "internal_error" not in res.text
    # 错误也必须落库
    rows = db.list_usage(limit=5)
    assert rows, "上游错误未落库"
    assert rows[0]["status"] == 429
    assert "rate limited" in (rows[0]["error"] or "")


def test_stream_request_upstream_sse_error_body(bridge_factory, stub_upstream, db):
    """流式请求 + 上游以 SSE 形状返回错误 → 同样不能 500。"""
    stub_upstream.set("/v1/messages",
                      'event: error\ndata: {"error":{"message":"boom"}}\n\n',
                      status=503, content_type=SSE_CT, chunked=True)
    handle = bridge_factory()
    res = _post(handle.base, {"model": "stub-anthropic", "stream": True,
                              "messages": [{"role": "user", "content": "hi"}]})
    assert res.status_code == 503, res.text
    assert "boom" in res.text
    assert "internal_error" not in res.text
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 503


def test_stream_request_upstream_error_openai_protocol(bridge_factory, stub_upstream, db):
    """openai 协议（/go 那条）流式 + 上游报错。"""
    stub_upstream.set("/v1/chat/completions",
                      {"error": {"message": "invalid api key", "code": "401"}},
                      status=401, content_type=JSON_CT)
    handle = bridge_factory()
    res = _post(handle.base, {"model": "stub-openai", "stream": True,
                              "messages": [{"role": "user", "content": "hi"}]})
    assert res.status_code == 401, res.text
    assert "invalid api key" in res.text
    assert "internal_error" not in res.text
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 401


def test_stream_request_upstream_error_commandcode(bridge_factory, stub_upstream, db):
    """commandcode 协议流式 + 上游报错。"""
    stub_upstream.set("/alpha/generate", "not-a-json-line", status=403,
                      content_type=NDJSON_CT)
    handle = bridge_factory()
    res = _post(handle.base, {"model": "stub-cc", "stream": True,
                              "messages": [{"role": "user", "content": "hi"}]})
    assert res.status_code == 403, res.text
    assert "internal_error" not in res.text
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 403


def test_stream_success_still_works_after_fix(bridge_factory, stub_upstream, db):
    """修复不能破坏正常流式路径。"""
    body = "".join(
        f"event: {ev}\ndata: {json.dumps(data)}\n\n"
        for ev, data in [
            ("message_start", {"message": {"usage": {"input_tokens": 3}}}),
            ("content_block_delta", {"delta": {"type": "text_delta", "text": "ok"}}),
            ("message_delta", {"delta": {"stop_reason": "end_turn"},
                               "usage": {"output_tokens": 2}}),
            ("message_stop", {}),
        ]
    )
    stub_upstream.set("/v1/messages", body, content_type=SSE_CT, chunked=True)
    handle = bridge_factory()
    with httpx.Client(timeout=20) as client:
        with client.stream("POST", handle.base + CHAT,
                           json={"model": "stub-anthropic", "stream": True,
                                 "messages": [{"role": "user", "content": "hi"}]}) as res:
            assert res.status_code == 200
            chunks = []
            done = False
            for line in res.iter_lines():
                if line.startswith("data: "):
                    payload = line[6:]
                    if payload == "[DONE]":
                        done = True
                    else:
                        chunks.append(json.loads(payload))
    assert done is True
    texts = [c["choices"][0]["delta"].get("content") for c in chunks
             if c.get("choices") and c["choices"][0]["delta"].get("content")]
    assert "".join(texts) == "ok"
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 200
    assert rows[0]["prompt_tokens"] == 3
    assert rows[0]["completion_tokens"] == 2
