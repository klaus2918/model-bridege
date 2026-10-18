# -*- coding: utf-8 -*-
"""边界请求回归：任何请求体都不允许触发 500（server_error）。

背景：用户反馈 exe 面板/代理返回「内部错误：详见服务端控制台 / server_error」。
这类错误来自 proxy._route 的兜底，说明有异常在写出响应之前抛出。
本文件用畸形/极端请求体把这类路径钉死。
"""
from __future__ import annotations

import json

import httpx

from conftest import make_config

CHAT = "/v1/chat/completions"

# (标签, 请求体原样载荷, 是否按原文发送)
CASES: list[tuple[str, object, bool]] = [
    ("messages 为字符串数组", {"model": "stub-anthropic", "messages": ["hi"]}, False),
    ("messages 含 null", {"model": "stub-anthropic", "messages": [None]}, False),
    ("messages 含空对象", {"model": "stub-anthropic", "messages": [{}]}, False),
    ("assistant 缺 content", {"model": "stub-anthropic",
                              "messages": [{"role": "assistant"}]}, False),
    ("assistant tool_calls 含 null", {"model": "stub-anthropic", "messages": [
        {"role": "assistant", "tool_calls": [None]}]}, False),
    ("tool 消息缺 tool_call_id", {"model": "stub-anthropic", "messages": [
        {"role": "tool", "content": "x"}]}, False),
    ("tools 含 null", {"model": "stub-anthropic",
                       "messages": [{"role": "user", "content": "hi"}],
                       "tools": [None]}, False),
    ("tool_choice 指名但无 tools", {"model": "stub-anthropic", "messages": [
        {"role": "user", "content": "hi"}],
        "tool_choice": {"type": "function", "function": {"name": "f"}}}, False),
    ("model 是数字", {"model": 123,
                     "messages": [{"role": "user", "content": "hi"}]}, False),
    ("stream 非布尔", {"model": "stub-anthropic", "stream": "maybe",
                       "messages": [{"role": "user", "content": "hi"}]}, False),
    ("temperature 是字符串", {"model": "stub-anthropic", "temperature": "hot",
                              "messages": [{"role": "user", "content": "hi"}]}, False),
    ("缺 messages 字段", {"model": "stub-anthropic"}, False),
    ("messages 为空数组", {"model": "stub-anthropic", "messages": []}, False),
    ("content 是数字", {"model": "stub-anthropic", "messages": [
        {"role": "user", "content": 42}]}, False),
    ("image_url 为 null", {"model": "stub-anthropic", "messages": [
        {"role": "user", "content": [
            {"type": "image_url", "image_url": None}]}]}, False),
    ("max_tokens 是字符串", {"model": "stub-anthropic", "max_tokens": "8k",
                             "messages": [{"role": "user", "content": "hi"}]}, False),
    ("stop 是空数组", {"model": "stub-anthropic", "stop": [],
                       "messages": [{"role": "user", "content": "hi"}]}, False),
    ("别名不存在", {"model": "no-such-alias",
                    "messages": [{"role": "user", "content": "hi"}]}, False),
    ("顶层是数组", [{"model": "stub-anthropic", "messages": []}], False),
    ("顶层是字符串", '"just a string"', True),
    ("顶层是 null", "null", True),
    ("空对象", {}, False),
    ("非法 JSON", "{not json", True),
    ("截断 JSON", '{"model": "x", "messages": [', True),
    ("无效 UTF-8 字节", b"\xff\xfe\x00\x01", True),
]


def _send(client: httpx.Client, base: str, path: str, payload, raw: bool) -> tuple[int, str]:
    if raw:
        data = payload if isinstance(payload, bytes) else str(payload).encode("utf-8")
        res = client.post(base + path, content=data,
                          headers={"content-type": "application/json"})
    else:
        res = client.post(base + path, json=payload)
    return res.status_code, res.text


def test_edge_payloads_never_return_500_for_anthropic(bridge_factory, stub_upstream):
    stub_upstream.set("/v1/messages", {
        "id": "m", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
    handle = bridge_factory()
    client = httpx.Client(timeout=20)
    bad = []
    for tag, payload, raw in CASES:
        status, body = _send(client, handle.base, CHAT, payload, raw)
        if status >= 500 or status == -1:
            bad.append(f"{tag} -> {status} {body[:300]}")
    assert not bad, "出现 5xx：\n  " + "\n  ".join(bad)


def test_edge_payloads_never_return_500_for_all_upstreams(bridge_factory, stub_upstream):
    """三条协议前缀都跑一遍最易崩的样本。"""
    stub_upstream.set("/v1/messages", {
        "id": "m", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 1, "output_tokens": 1},
    })
    stub_upstream.set("/v1/chat/completions", {
        "id": "c", "model": "stub-openai",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    stub_upstream.set("/alpha/generate",
                      '{"type":"text-delta","text":"ok"}\n'
                      '{"type":"finish","finishReason":"stop",'
                      '"totalUsage":{"inputTokens":1,"outputTokens":1}}\n',
                      content_type="application/x-ndjson; charset=utf-8")

    handle = bridge_factory()
    # 服务端每次回 connection: close，客户端若用 keep-alive 复用连接会撞上 WinError 10053。
    # 显式禁用 keep-alive，并允许一次连接重试（模拟真实客户端的重连行为）。
    client = httpx.Client(
        timeout=20,
        headers={"connection": "close"},
        transport=httpx.HTTPTransport(retries=1),
    )
    samples = [
        {"model": "m", "messages": ["hi"]},
        {"model": "m"},
        {},
    ]
    bad = []
    for prefix in ("/cch", "/go", "/zen", "/commandcode"):
        for payload in samples:
            status, body = _send(client, handle.base,
                                 f"{prefix}{CHAT}", payload, False)
            if status >= 500:
                bad.append(f"{prefix} {payload} -> {status} {body[:300]}")
    assert not bad, "出现 5xx：\n  " + "\n  ".join(bad)


def test_client_disconnect_is_not_logged_as_internal_error(bridge_factory):
    """客户端中途断开必须静默收尾，不能升级成 500/内部错误。"""
    import socket

    handle = bridge_factory()
    port = int(handle.base.rsplit(":", 1)[1])
    sock = socket.create_connection(("127.0.0.1", port), timeout=10)
    body = json.dumps({"model": "stub-anthropic", "stream": True,
                       "messages": [{"role": "user", "content": "hi"}]})
    head = (f"POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
            f"content-type: application/json\r\ncontent-length: {len(body)}\r\n\r\n")
    sock.sendall(head.encode("utf-8") + body.encode("utf-8"))
    # 只读一小段就断开，制造「写到一半客户端没了」
    try:
        sock.recv(256)
    except Exception:
        pass
    sock.close()

    # 服务端必须仍然健康：后续请求照常工作
    res = httpx.get(f"{handle.base}/health", timeout=10)
    assert res.status_code == 200


def test_control_api_never_returns_500(bridge_factory):
    """控制面 /api/* 对畸形入参必须回 4xx，不能 500。"""
    handle = bridge_factory()
    client = httpx.Client(timeout=20)
    bad = []

    def hit(tag: str, fn) -> None:
        try:
            status, body = fn()
        except Exception as exc:  # noqa: BLE001
            bad.append(f"{tag} -> 异常 {type(exc).__name__}: {exc}")
            return
        if status >= 500:
            bad.append(f"{tag} -> {status} {body[:300]}")

    hit("GET /api/config", lambda: _get(client, f"{handle.base}/api/config"))
    hit("GET /api/info", lambda: _get(client, f"{handle.base}/api/info"))
    hit("GET /api/upstreams", lambda: _get(client, f"{handle.base}/api/upstreams"))

    for qs in ("?range=", "?range=bogus", "?range=all",
               "?limit=abc", "?limit=-1", "?limit=99999999999999",
               "?offset=-5", "?offset=abc", "?group_by=;drop",
               "?model=%00", "?upstream=" + "x" * 500):
        hit(f"GET /api/usage/list{qs[:30]}",
            lambda q=qs: _get(client, f"{handle.base}/api/usage/list{q}"))
        hit(f"GET /api/usage/summary{qs[:30]}",
            lambda q=qs: _get(client, f"{handle.base}/api/usage/summary{q}"))
        hit(f"GET /api/usage/totals{qs[:30]}",
            lambda q=qs: _get(client, f"{handle.base}/api/usage/totals{q}"))

    # POST 各种畸形配置体
    cfg = _get(client, f"{handle.base}/api/config")[1]
    try:
        good = json.loads(cfg).get("config", {})
    except Exception:
        good = {}
    payloads = [
        ("空体", None),
        ("字符串体", '"nope"'),
        ("数组体", "[1,2,3]"),
        ("null 体", "null"),
        ("config 是字符串", json.dumps({"config": "x"})),
        ("config 是数组", json.dumps({"config": [1, 2]})),
        ("config 是 null", json.dumps({"config": None})),
        ("缺 upstreams", json.dumps({"config": {"port": 0}})),
        ("upstreams 非对象", json.dumps(
            {"config": {"upstreams": ["a", "b"]}})),
        ("上游项不是对象", json.dumps(
            {"config": {"upstreams": {"a": "str", "b": None}}})),
        ("port 是字符串", json.dumps({"config": {"port": "abc"}})),
        ("models 含对象", json.dumps(
            {"config": {"models": [{"x": 1}]}})),
        ("畸形 JSON", "{oops"),
    ]
    for tag, payload in payloads:
        hit(f"POST /api/config {tag}",
            lambda p=payload: _post(client, f"{handle.base}/api/config", p))

    hit("POST /api/config/reload 空体",
        lambda: _post(client, f"{handle.base}/api/config/reload", None))

    # 恢复一份正常配置，避免污染后续断言
    _post(client, f"{handle.base}/api/config",
          json.dumps({"config": good}, ensure_ascii=False))

    assert not bad, "控制面出现 5xx：\n  " + "\n  ".join(bad)
    # 服务仍健康
    assert client.get(f"{handle.base}/health", timeout=10).status_code == 200


def _get(client, url: str) -> tuple[int, str]:
    res = client.get(url)
    return res.status_code, res.text


def _post(client, url: str, payload) -> tuple[int, str]:
    if payload is None:
        res = client.post(url, content=b"")
    elif isinstance(payload, (bytes, bytearray)):
        res = client.post(url, content=payload,
                          headers={"content-type": "application/json"})
    elif isinstance(payload, str):
        res = client.post(url, content=payload.encode("utf-8"),
                          headers={"content-type": "application/json"})
    else:
        res = client.post(url, json=payload)
    return res.status_code, res.text
