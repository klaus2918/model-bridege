# -*- coding: utf-8 -*-
"""代理端到端冒烟：真实 ThreadingHTTPServer + 本地桩上游，覆盖核心契约。"""
from __future__ import annotations

import json

import httpx

from conftest import make_config

JSON_CT = "application/json; charset=utf-8"
SSE_CT = "text/event-stream; charset=utf-8"
NDJSON_CT = "application/x-ndjson; charset=utf-8"


ANTHROPIC_JSON = {
    "id": "msg_1", "stop_reason": "end_turn",
    "content": [{"type": "text", "text": "来自 anthropic 上游"}],
    "usage": {"input_tokens": 21, "output_tokens": 7},
}

OPENAI_JSON = {
    "id": "chatcmpl-1", "model": "stub-openai",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "来自 openai 上游"},
                 "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 5, "completion_tokens": 3},
}

CC_NDJSON = "\n".join([
    json.dumps({"type": "text-delta", "text": "来自 "}, ensure_ascii=False),
    json.dumps({"type": "text-delta", "text": "commandcode 上游"}, ensure_ascii=False),
    json.dumps({"type": "reasoning-delta", "text": "思考"}, ensure_ascii=False),
    json.dumps({"type": "finish", "finishReason": "stop",
                "totalUsage": {"inputTokens": 30, "outputTokens": 12,
                               "inputTokenDetails": {"cacheReadTokens": 8}}}),
    "",
])

ANTHROPIC_SSE = "".join(
    f"event: {event}\ndata: {json.dumps(data, ensure_ascii=False)}\n\n"
    for event, data in [
        ("message_start", {"message": {"usage": {"input_tokens": 10}}}),
        ("content_block_delta", {"delta": {"type": "text_delta", "text": "流式"}}),
        ("message_delta", {"delta": {"stop_reason": "end_turn"},
                           "usage": {"output_tokens": 4}}),
        ("message_stop", {}),
    ]
)

OPENAI_SSE = "".join(
    f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n"
    for chunk in [
        {"id": "c1", "model": "stub-openai",
         "choices": [{"index": 0, "delta": {"role": "assistant", "content": "开"},
                      "finish_reason": None}]},
        {"id": "c1", "model": "stub-openai",
         "choices": [{"index": 0, "delta": {"content": "流"}, "finish_reason": None}]},
        {"id": "c1", "model": "stub-openai",
         "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
         "usage": {"prompt_tokens": 6, "completion_tokens": 2}},
        {"id": "c1"},
    ]
) + "data: [DONE]\n\n"


def delta_texts(chunks: list[dict]) -> list[str]:
    """从 OpenAI 流 chunk 里取出 content 增量（容忍无 choices 的尾块）。"""
    out = []
    for c in chunks:
        choices = c.get("choices") or []
        if not choices:
            continue
        text = (choices[0].get("delta") or {}).get("content")
        if text:
            out.append(text)
    return out


def chat(client, base, *, model, stream=False, extra=None):
    payload = {"model": model, "messages": [{"role": "user", "content": "hi"}],
               "stream": stream}
    payload.update(extra or {})
    return client.post(f"{base}/v1/chat/completions", json=payload)


# ── 基础端点 ──────────────────────────────────────────────────────────────
def test_health_lists_upstreams(bridge_factory):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/health", timeout=10)
    assert res.status_code == 200
    data = res.json()
    assert data["status"] == "ok"
    names = [u["name"] for u in data["upstreams"]]
    assert names == ["anth", "oai", "cc"]
    assert data["upstreams"][0]["default"] is True
    assert data["upstreams"][0]["endpoint"].endswith("/anth/v1")


def test_models_returns_whitelist(bridge_factory):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/v1/models", timeout=10)
    assert res.status_code == 200
    body = res.json()
    assert body["object"] == "list"
    ids = [m["id"] for m in body["data"]]
    # 顶层 models 是精确白名单，再按设计追加别名（alias-a → stub-anthropic）
    assert set(ids) >= {"stub-anthropic", "stub-openai", "stub-cc", "alias-a"}
    owner = {m["id"]: m["owned_by"] for m in body["data"]}
    assert owner["stub-anthropic"] == "anth"
    assert owner["stub-cc"] == "cc"


def test_unknown_prefix_returns_404_with_available(bridge_factory):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/nope/v1/models", timeout=10)
    assert res.status_code == 404
    err = res.json()["error"]
    assert err["code"] == "unknown_upstream"
    assert "/anth" in err["message"] and "/cc" in err["message"]


def test_unimplemented_endpoint_404(bridge_factory):
    handle = bridge_factory()
    res = httpx.post(f"{handle.base}/v1/embeddings", json={}, timeout=10)
    assert res.status_code == 404
    assert res.json()["error"]["code"] == "not_implemented"


def test_invalid_json_returns_400(bridge_factory):
    handle = bridge_factory()
    res = httpx.post(f"{handle.base}/v1/chat/completions",
                     content=b"{not json",
                     headers={"content-type": "application/json"}, timeout=10)
    assert res.status_code == 400
    assert res.json()["error"]["code"] == "invalid_json"


# ── anthropic 上游 ────────────────────────────────────────────────────────
def test_non_stream_chat_via_anthropic(bridge_factory, stub_upstream, db):
    stub_upstream.set("/v1/messages", ANTHROPIC_JSON, content_type=JSON_CT)
    handle = bridge_factory()

    res = chat(httpx.Client(timeout=15), handle.base, model="stub-anthropic")
    assert res.status_code == 200
    body = res.json()
    assert res.headers["x-bridge-upstream"] == "anth"
    assert res.headers["x-bridge-upstream-model"] == "stub-anthropic"
    assert body["model"] == "stub-anthropic"
    assert body["choices"][0]["message"]["content"] == "来自 anthropic 上游"
    assert body["usage"] == {"prompt_tokens": 21, "completion_tokens": 7, "total_tokens": 28}

    # 上游收到的是 Anthropic 形状
    upstream_req = stub_upstream.last("/v1/messages")
    assert upstream_req["headers"]["x-api-key"] == "test-key"
    assert upstream_req["headers"]["anthropic-version"] == "2023-06-01"
    assert upstream_req["body"]["model"] == "stub-anthropic"
    assert upstream_req["body"]["messages"][0]["role"] == "user"

    # 用量已落库
    totals = db.totals()
    assert totals["requests"] == 1
    assert totals["prompt_tokens"] == 21
    assert totals["completion_tokens"] == 7
    rows = db.list_usage(limit=5)
    assert rows[0]["upstream"] == "anth"
    assert rows[0]["protocol"] == "anthropic"
    assert rows[0]["status"] == 200
    assert rows[0]["stream"] == 0


def test_stream_chat_via_anthropic(bridge_factory, stub_upstream, db):
    stub_upstream.set("/v1/messages", ANTHROPIC_SSE, content_type=SSE_CT, chunked=True)
    handle = bridge_factory()

    with httpx.Client(timeout=15) as client:
        with client.stream("POST", f"{handle.base}/v1/chat/completions",
                           json={"model": "stub-anthropic", "stream": True,
                                 "messages": [{"role": "user", "content": "hi"}]}) as res:
            assert res.status_code == 200
            assert res.headers["content-type"].startswith("text/event-stream")
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
    texts = delta_texts(chunks)
    assert texts == ["流式"]
    final = chunks[-1]
    assert final["choices"][0]["finish_reason"] == "end_turn" or "stop" in (
        final["choices"][0]["finish_reason"] or "")
    assert final["usage"]["prompt_tokens"] == 10
    assert final["usage"]["completion_tokens"] == 4

    rows = db.list_usage(limit=5)
    assert rows and rows[0]["stream"] == 1
    assert rows[0]["prompt_tokens"] == 10
    assert rows[0]["completion_tokens"] == 4


def test_upstream_error_is_forwarded(bridge_factory, stub_upstream, db):
    stub_upstream.set("/v1/messages", {"error": {"message": "上游炸了"}},
                      status=503, content_type=JSON_CT)
    handle = bridge_factory()
    res = chat(httpx.Client(timeout=15), handle.base, model="stub-anthropic")
    assert res.status_code == 503
    assert "上游返回 503" in res.json()["error"]["message"]
    assert "上游炸了" in res.json()["error"]["message"]
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 503
    assert "上游炸了" in (rows[0]["error"] or "")


def test_unreachable_upstream_returns_502(bridge_factory, stub_upstream, db):
    cfg = make_config("http://127.0.0.1:1")
    handle = bridge_factory(cfg)
    res = chat(httpx.Client(timeout=10), handle.base, model="stub-anthropic")
    assert res.status_code == 502
    assert res.json()["error"]["code"] == "upstream_failed"
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["status"] == 502


# ── openai 上游与前缀路由 ─────────────────────────────────────────────────
def test_prefix_routing_forces_upstream(bridge_factory, stub_upstream):
    stub_upstream.set("/v1/chat/completions", OPENAI_JSON, content_type=JSON_CT)
    handle = bridge_factory()

    res = httpx.post(f"{handle.base}/oai/v1/chat/completions",
                     json={"model": "stub-openai",
                           "messages": [{"role": "user", "content": "hi"}]},
                     timeout=15)
    assert res.status_code == 200
    assert res.headers["x-bridge-upstream"] == "oai"
    upstream_req = stub_upstream.last("/v1/chat/completions")
    assert upstream_req["headers"]["authorization"] == "Bearer oai-key"
    assert "x-opencode-session" in upstream_req["headers"]
    assert upstream_req["body"]["model"] == "stub-openai"


def test_models_per_upstream_prefix(bridge_factory, stub_upstream):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/oai/v1/models", timeout=10)
    assert res.status_code == 200
    assert [m["id"] for m in res.json()["data"]] == ["stub-openai"]
    assert res.json()["data"][0]["owned_by"] == "oai"


def test_alias_routing_picks_target_upstream(bridge_factory, stub_upstream):
    """全局别名 alias-a → stub-anthropic，路由应命中 anthropic 上游。"""
    stub_upstream.set("/v1/messages", ANTHROPIC_JSON, content_type=JSON_CT)
    handle = bridge_factory()
    res = chat(httpx.Client(timeout=15), handle.base, model="alias-a")
    assert res.status_code == 200
    assert res.headers["x-bridge-upstream"] == "anth"
    # 下游看到的仍是别名，上游收到的是真实模型名
    assert res.json()["model"] == "alias-a"
    assert stub_upstream.last("/v1/messages")["body"]["model"] == "stub-anthropic"


def test_stream_via_openai_passthrough(bridge_factory, stub_upstream, db):
    stub_upstream.set("/v1/chat/completions", OPENAI_SSE, content_type=SSE_CT, chunked=True)
    handle = bridge_factory()
    with httpx.Client(timeout=15) as client:
        with client.stream("POST", f"{handle.base}/v1/chat/completions",
                           json={"model": "stub-openai", "stream": True,
                                 "messages": [{"role": "user", "content": "hi"}]}) as res:
            chunks, done = [], False
            for line in res.iter_lines():
                if line.startswith("data: "):
                    payload = line[6:]
                    if payload == "[DONE]":
                        done = True
                    else:
                        chunks.append(json.loads(payload))
    assert done is True
    # model 被改写成下游请求的名字
    assert all(c.get("model") == "stub-openai" for c in chunks if "model" in c)
    texts = delta_texts(chunks)
    assert "".join(texts) == "开流"
    rows = db.list_usage(limit=5)
    assert rows and rows[0]["prompt_tokens"] == 6
    assert rows[0]["cache_read_tokens"] == 0


# ── commandcode 上游 ──────────────────────────────────────────────────────
def test_commandcode_non_stream_aggregates_ndjson(bridge_factory, stub_upstream, db):
    stub_upstream.set("/alpha/generate", CC_NDJSON, content_type=NDJSON_CT)
    handle = bridge_factory()
    res = chat(httpx.Client(timeout=15), handle.base, model="stub-cc")
    assert res.status_code == 200
    body = res.json()
    message = body["choices"][0]["message"]
    assert message["content"] == "来自 commandcode 上游"
    assert message["reasoning_content"] == "思考"
    assert body["usage"] == {
        "prompt_tokens": 30, "completion_tokens": 12, "total_tokens": 42,
        "prompt_tokens_details": {"cached_tokens": 8},
    }

    upstream_req = stub_upstream.last("/alpha/generate")
    assert upstream_req["headers"]["x-cmd-zdr"] == "1"
    assert upstream_req["headers"]["x-command-code-version"] == "1.2.3"
    assert upstream_req["headers"]["authorization"] == "Bearer cc-key"
    assert upstream_req["body"]["params"]["stream"] is True
    assert upstream_req["body"]["params"]["model"] == "stub-cc"
    assert upstream_req["body"]["threadId"]

    rows = db.list_usage(limit=5)
    assert rows and rows[0]["cache_read_tokens"] == 8
    assert rows[0]["protocol"] == "commandcode"


def test_commandcode_stream_marks_cache_usage(bridge_factory, stub_upstream, db):
    stub_upstream.set("/alpha/generate", CC_NDJSON, content_type=NDJSON_CT)
    handle = bridge_factory()
    with httpx.Client(timeout=15) as client:
        with client.stream("POST", f"{handle.base}/v1/chat/completions",
                           json={"model": "stub-cc", "stream": True,
                                 "messages": [{"role": "user", "content": "hi"}]}) as res:
            chunks, done = [], False
            for line in res.iter_lines():
                if line.startswith("data: "):
                    payload = line[6:]
                    if payload == "[DONE]":
                        done = True
                    else:
                        chunks.append(json.loads(payload))
    assert done is True
    texts = delta_texts(chunks)
    assert "".join(texts) == "来自 commandcode 上游"
    final = chunks[-1]
    assert final["usage"]["prompt_tokens_details"]["cached_tokens"] == 8

    rows = db.list_usage(limit=5)
    assert rows and rows[0]["stream"] == 1
    assert rows[0]["prompt_tokens"] == 30


def test_commandcode_missing_credentials_returns_401(db, stub_upstream):
    from modelgate.config import save_config, upstreams_of
    from modelgate.proxy import Bridge, create_server
    import threading

    cfg = make_config(stub_upstream.url)
    cfg["upstreams"]["cc"]["apiKey"] = ""
    cfg["upstreams"]["cc"]["apiKeyFile"] = "/nonexistent/auth.json"
    cfg["upstreams"]["cc"]["apiKeyEnv"] = "NO_SUCH_ENV_VAR_FOR_TEST"
    cfg["defaultModel"] = "stub-cc"
    cfg["models"] = ["stub-cc"]
    cfg["upstreams"] = {"cc": cfg["upstreams"]["cc"]}

    bridge = Bridge(cfg, db, config_path=None)
    server = create_server(bridge)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05},
                     daemon=True).start()
    try:
        res = httpx.post(f"http://127.0.0.1:{port}/v1/chat/completions",
                         json={"model": "stub-cc",
                               "messages": [{"role": "user", "content": "hi"}]},
                         timeout=10)
        assert res.status_code == 401
        err = res.json()["error"]
        assert err["code"] == "missing_api_key"
        # 本地判定失败时不打上游
        assert stub_upstream.count("/alpha/generate") == 0
    finally:
        server.shutdown()
        server.server_close()
        bridge.close()


# ── 配置与统计 API ────────────────────────────────────────────────────────
def test_config_api_masks_and_roundtrips(bridge_factory, stub_upstream):
    handle = bridge_factory()

    got = httpx.get(f"{handle.base}/api/config", timeout=10)
    assert got.status_code == 200
    payload = got.json()
    # 明文密钥不外泄，只给来源与是否已配置
    assert "apiKey" not in payload["config"]["upstreams"]["anth"]
    assert payload["config"]["upstreams"]["anth"]["_keyConfigured"] is True
    assert payload["config"]["upstreams"]["anth"]["_keySource"] == "literal"
    assert payload["config_path"].endswith("bridge.config.json")

    updated = payload["config"]
    updated["defaultModel"] = "stub-openai"
    updated["maxTokens"] = 4096
    saved = httpx.post(f"{handle.base}/api/config",
                       json={"config": updated}, timeout=10)
    assert saved.status_code == 200
    assert saved.json()["ok"] is True

    # 双写：配置文件与运行时都生效
    assert handle.bridge.config["defaultModel"] == "stub-openai"
    assert handle.bridge.config["maxTokens"] == 4096
    on_disk = json.loads((handle.bridge.config_path).read_text(encoding="utf-8"))
    assert on_disk["defaultModel"] == "stub-openai"
    assert handle.bridge.db.load_config_override()["maxTokens"] == 4096
    # 明文密钥硬约束：前端从未拿到明文，回传也就不含明文，文件与 settings 都不落明文
    assert on_disk["upstreams"]["anth"].get("apiKey") in (None, "")
    assert handle.bridge.db.load_config_override()["upstreams"]["anth"].get("apiKey") in (None, "")
    # 运行态保留既有明文（保存前 bridge 已持有），上游不失效
    anth = next(u for u in handle.bridge.upstreams if u["name"] == "anth")
    assert anth["apiKey"] == "test-key"
    assert anth["apiKeySource"] == "literal"


def test_config_save_preserves_key_when_client_omits_it(bridge_factory, stub_upstream):
    handle = bridge_factory()
    got = httpx.get(f"{handle.base}/api/config", timeout=10).json()["config"]
    # 前端只回传 keyConfigured，不回传 apiKey
    assert "apiKey" not in got["upstreams"]["anth"]
    got["upstreams"]["anth"].pop("apiKey", None)
    saved = httpx.post(f"{handle.base}/api/config", json={"config": got}, timeout=10)
    assert saved.status_code == 200
    on_disk = json.loads((handle.bridge.config_path).read_text(encoding="utf-8"))
    # 新契约：不回传明文则文件里不会有明文；运行态仍按 literal 持有原 key
    assert on_disk["upstreams"]["anth"].get("apiKey") in (None, "")
    anth = next(u for u in handle.bridge.upstreams if u["name"] == "anth")
    assert anth["apiKey"] == "test-key"
    assert anth["apiKeySource"] == "literal"


def test_usage_stats_endpoints(bridge_factory, stub_upstream, db):
    stub_upstream.set("/v1/messages", ANTHROPIC_JSON, content_type=JSON_CT)
    handle = bridge_factory()
    client = httpx.Client(timeout=15)
    for _ in range(3):
        assert chat(client, handle.base, model="stub-anthropic").status_code == 200

    totals = httpx.get(f"{handle.base}/api/usage/totals?range=24h", timeout=10).json()["totals"]
    assert totals["requests"] == 3
    assert totals["prompt_tokens"] == 63
    assert totals["completion_tokens"] == 21
    assert totals["total_tokens"] == 84
    assert totals["errors"] == 0

    by_day = httpx.get(f"{handle.base}/api/usage/summary?group_by=day", timeout=10).json()
    assert by_day["group_by"] == "day"
    assert len(by_day["rows"]) == 1
    assert by_day["rows"][0]["requests"] == 3
    assert by_day["rows"][0]["bucket"]

    by_model = httpx.get(f"{handle.base}/api/usage/summary?group_by=model", timeout=10).json()
    assert by_model["rows"][0]["bucket"] == "stub-anthropic"
    assert by_model["rows"][0]["prompt_tokens"] == 63

    by_up = httpx.get(f"{handle.base}/api/usage/summary?group_by=upstream", timeout=10).json()
    assert by_up["rows"][0]["bucket"] == "anth"

    rows = httpx.get(f"{handle.base}/api/usage/list?limit=2", timeout=10).json()
    assert rows["total"] == 3
    assert len(rows["rows"]) == 2

    bad = httpx.get(f"{handle.base}/api/usage/summary?group_by=nope", timeout=10)
    assert bad.status_code == 400


def test_info_endpoint_reports_version(bridge_factory):
    handle = bridge_factory()
    info = httpx.get(f"{handle.base}/api/info", timeout=10).json()
    assert info["app"] == "Model-Gate"
    assert info["version"]
    assert info["upstream_count"] == 3
    assert info["db_path"].endswith(".db")
    assert info["thinking"]["enabled"] is True
