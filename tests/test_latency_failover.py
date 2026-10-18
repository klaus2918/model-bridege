# -*- coding: utf-8 -*-
"""耗时口径与「代理不中断」回归：TTFT、百分位、上游自动切换、热重载不断连。

背景（用户诉求）：
1. 「接口请求耗时还不完善」——只有平均值会掩盖长尾，需要 P50/P95 与首 token 延迟；
2. 「必须保证代理不中断」——上游 5xx/连接失败时应有限度地切换上游，
   而改配置/换密钥必须**不重绑端口**（否则客户端会瞬断）。
"""
from __future__ import annotations

import json

from modelgate.db import percentile
from modelgate.proxy import BridgeHandler

OPENAI_JSON = {
    "id": "chatcmpl-1",
    "object": "chat.completion",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                 "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 5, "completion_tokens": 2},
}
SSE_BODY = (
    'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n'
    'data: {"choices":[{"delta":{},"finish_reason":"stop"}],'
    '"usage":{"prompt_tokens":7,"completion_tokens":3}}\n\n'
    "data: [DONE]\n\n"
)


def test_percentile_interpolates_and_handles_edges():
    assert percentile([], 0.5) == 0.0
    assert percentile([7], 0.95) == 7.0
    assert percentile([10, 20, 30, 40], 0.5) == 25.0
    assert percentile([10, 20, 30, 40], 0.95) == 38.5
    assert percentile([100, 200], 0.0) == 100.0
    assert percentile([100, 200], 1.0) == 200.0


def test_totals_summary_and_model_usage_expose_latency_fields(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    for duration, ttft, status in ((1000, 200, 200), (2000, 300, 200), (9000, 400, 500)):
        db.record_usage(upstream="oai", protocol="openai", requested_model="m",
                        upstream_model="m", prompt_tokens=10, completion_tokens=2,
                        duration_ms=duration, ttft_ms=ttft, status=status, stream=True)

    totals = db.totals()
    assert totals["avg_duration_ms"] == 4000.0
    assert totals["p50_duration_ms"] == 2000.0
    # 3 个样本的 P95 走线性插值：2000*0.1 + 9000*0.9 = 8300
    assert totals["p95_duration_ms"] == 8300.0
    assert totals["max_duration_ms"] == 9000.0
    assert totals["avg_ttft_ms"] == 300.0
    assert totals["ttft_samples"] == 3

    summary = db.summary(group_by="model")[0]
    assert summary["p95_duration_ms"] == 8300.0
    assert summary["avg_ttft_ms"] == 300.0

    usage = db.model_usage()[0]
    assert usage["model"] == "m"
    assert usage["p50_duration_ms"] == 2000.0
    assert usage["p95_duration_ms"] == 8300.0
    assert usage["avg_ttft_ms"] == 300.0
    assert usage["success_rate"] == round(2 / 3, 4)


def test_model_usage_endpoint(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="oai", protocol="openai", requested_model="alpha",
                    upstream_model="alpha", prompt_tokens=100, completion_tokens=50,
                    duration_ms=500, ttft_ms=120, status=200, stream=True)
    db.record_usage(upstream="oai", protocol="openai", requested_model="beta",
                    upstream_model="beta", prompt_tokens=10, duration_ms=100, status=200)
    data = handle.client.get(f"{handle.base}/api/usage/models?range=all").json()
    models = [row["model"] for row in data["rows"]]
    assert models[0] == "alpha"  # 按总 token 降序
    assert set(models) == {"alpha", "beta"}
    assert "p95_duration_ms" in data["rows"][0]


def test_stream_records_ttft(bridge_factory):
    """流式请求必须记录首 token 延迟（TTFT）。"""
    from conftest import StubUpstream, make_config

    stub = StubUpstream()
    try:
        stub.set("/v1/chat/completions", SSE_BODY,
                 content_type="text/event-stream", chunked=True)
        config = make_config("http://127.0.0.1:1")
        config["upstreams"] = {
            "oai": {"protocol": "openai", "baseUrl": stub.url, "apiKey": "k",
                    "models": ["m"], "default": True},
        }
        handle = bridge_factory(config)
        with handle.client.stream(
                "POST", f"{handle.base}/v1/chat/completions",
                json={"model": "m", "stream": True,
                      "messages": [{"role": "user", "content": "hi"}]}) as resp:
            assert resp.status_code == 200
            for _line in resp.iter_lines():
                pass
        rows = handle.bridge.db.list_usage(limit=5)
        stream_row = next(r for r in rows if r["stream"])
        assert stream_row["ttft_ms"] > 0, "流式请求应记录 TTFT"
        assert stream_row["ttft_ms"] <= max(1, stream_row["duration_ms"])
    finally:
        stub.close()


def test_non_stream_ttft_is_zero(bridge_factory):
    """非流式拿不到「首个增量」时刻，TTFT 记 0（UI 显示「—」），不伪造数值。"""
    from conftest import StubUpstream, make_config

    stub = StubUpstream()
    try:
        stub.set("/v1/chat/completions", OPENAI_JSON)
        config = make_config("http://127.0.0.1:1")
        config["upstreams"] = {
            "oai": {"protocol": "openai", "baseUrl": stub.url, "apiKey": "k",
                    "models": ["m"], "default": True},
        }
        handle = bridge_factory(config)
        resp = handle.client.post(
            f"{handle.base}/v1/chat/completions",
            json={"model": "m", "messages": [{"role": "user", "content": "hi"}]})
        assert resp.status_code == 200
        row = handle.bridge.db.list_usage(limit=1)[0]
        assert row["stream"] == 0
        assert row["ttft_ms"] == 0
    finally:
        stub.close()


def test_failover_to_next_upstream_on_5xx(bridge_factory):
    """主上游 500 → 自动切到同样声明该模型的候补上游（客户端无感）。"""
    from conftest import StubUpstream, make_config

    bad = StubUpstream()
    good = StubUpstream()
    try:
        bad.set("/v1/chat/completions", {"error": {"message": "upstream boom"}},
                status=500)
        good.set("/v1/chat/completions", OPENAI_JSON)
        config = make_config("http://127.0.0.1:1")
        config["upstreams"] = {
            "bad": {"protocol": "openai", "baseUrl": bad.url, "apiKey": "k",
                    "models": ["m"], "default": True},
            "good": {"protocol": "openai", "baseUrl": good.url, "apiKey": "k",
                     "models": ["m"]},
        }
        handle = bridge_factory(config)
        resp = handle.client.post(
            f"{handle.base}/v1/chat/completions",
            json={"model": "m", "messages": [{"role": "user", "content": "hi"}]})
        assert resp.status_code == 200, resp.text
        assert good.count("/v1/chat/completions") == 1
        assert bad.count("/v1/chat/completions") == 1
        # 落库记录的是「真正服务的那条上游」
        row = handle.bridge.db.list_usage(limit=1)[0]
        assert row["upstream"] == "good"
        assert row["status"] == 200
    finally:
        bad.close()
        good.close()


def test_no_failover_on_401(bridge_factory):
    """401 是凭据问题：换上游只会掩盖问题，必须原样回给客户端。"""
    from conftest import StubUpstream, make_config

    bad = StubUpstream()
    good = StubUpstream()
    try:
        bad.set("/v1/chat/completions", {"error": {"message": "Invalid API key"}},
                status=401)
        good.set("/v1/chat/completions", OPENAI_JSON)
        config = make_config("http://127.0.0.1:1")
        config["upstreams"] = {
            "bad": {"protocol": "openai", "baseUrl": bad.url, "apiKey": "k",
                    "models": ["m"], "default": True},
            "good": {"protocol": "openai", "baseUrl": good.url, "apiKey": "k",
                     "models": ["m"]},
        }
        handle = bridge_factory(config)
        resp = handle.client.post(
            f"{handle.base}/v1/chat/completions",
            json={"model": "m", "messages": [{"role": "user", "content": "hi"}]})
        assert resp.status_code == 401
        assert good.count("/v1/chat/completions") == 0, "4xx 不应触发切换"
    finally:
        bad.close()
        good.close()


def test_prefix_route_does_not_failover(bridge_factory):
    """前缀路由是用户显式指定上游：不切换（忠于用户选择）。"""
    from conftest import StubUpstream, make_config

    bad = StubUpstream()
    good = StubUpstream()
    try:
        bad.set("/v1/chat/completions", {"error": {"message": "boom"}}, status=500)
        good.set("/v1/chat/completions", OPENAI_JSON)
        config = make_config("http://127.0.0.1:1")
        config["upstreams"] = {
            "bad": {"protocol": "openai", "baseUrl": bad.url, "apiKey": "k",
                    "models": ["m"], "default": True},
            "good": {"protocol": "openai", "baseUrl": good.url, "apiKey": "k",
                     "models": ["m"]},
        }
        handle = bridge_factory(config)
        resp = handle.client.post(
            f"{handle.base}/bad/v1/chat/completions",
            json={"model": "m", "messages": [{"role": "user", "content": "hi"}]})
        assert resp.status_code == 500
        assert good.count("/v1/chat/completions") == 0
    finally:
        bad.close()
        good.close()


def test_reload_keeps_listener_alive(bridge_factory):
    """热重载不得重绑端口：reload 后同一端口必须继续可用（客户端不瞬断）。"""
    handle = bridge_factory()
    port = int(handle.base.rsplit(":", 1)[1])
    assert handle.client.get(f"{handle.base}/health").status_code == 200
    resp = handle.client.post(f"{handle.base}/api/config/reload", json={})
    assert resp.status_code == 200, resp.text
    # 端口没变、服务仍在
    again = handle.client.get(f"{handle.base}/health")
    assert again.status_code == 200
    assert again.json()["status"] == "ok"
    assert int(handle.base.rsplit(":", 1)[1]) == port


def test_retryable_status_matrix():
    for status in (500, 502, 503, 504, 429):
        assert BridgeHandler._retryable_upstream_status(status) is True
    for status in (400, 401, 403, 404, 422):
        assert BridgeHandler._retryable_upstream_status(status) is False
