# -*- coding: utf-8 -*-
"""费用模型回归：单价匹配优先级、字段级继承、计算与历史重算。

背景：费用是「token 统计」承诺的一部分，但它最容易被悄悄算错 ——
例如只给某条规则写了 input/output，缓存价却回落到 0，费用被系统性低估。
这里把语义钉死。
"""
from __future__ import annotations

import json

from modelgate.pricing import (
    cost_of,
    currency_of,
    is_configured,
    normalize_pricing,
    resolve_rate,
)

PRICING = {
    "currency": "CNY",
    "default": {"input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.5},
    "rules": {
        "go/glm-5.3-flash": {"input": 0.6, "output": 2.4},
        "go": {"input": 0.9, "output": 1.8},
        "*/deepseek-flash": {"input": 1.5},
    },
}


def test_normalize_tolerates_garbage():
    for bad in (None, "", "not json", 123, [], {"rules": "x"}, "{bad json"):
        table = normalize_pricing(bad)
        assert set(table) == {"currency", "default", "rules"}
        assert table["currency"] == "CNY"
    # 扁平写法也要认
    flat = normalize_pricing({"default": {"input": 1}, "go": {"output": 2}})
    assert "go" in flat["rules"] and flat["rules"]["go"]["output"] == 2


def test_rate_priority():
    table = normalize_pricing(PRICING)
    assert resolve_rate(table, "go", "glm-5.3-flash")[1] == "go/glm-5.3-flash"
    assert resolve_rate(table, "go", "other-model")[1] == "go"
    assert resolve_rate(table, "zen", "deepseek-flash")[1] == "*/deepseek-flash"
    assert resolve_rate(table, "zen", "unknown")[1] == "default"
    # 上游/模型都为空时落到 default
    assert resolve_rate(table, "", "")[1] == "default"


def test_rule_inherits_unspecified_fields_from_default():
    """规则里没写的字段继承 default；显式写 0 就是 0。"""
    table = normalize_pricing(PRICING)
    rate, _ = resolve_rate(table, "go", "glm-5.3-flash")
    assert rate == {"input": 0.6, "output": 2.4, "cacheRead": 0.1, "cacheWrite": 0.5}

    explicit_zero = normalize_pricing({
        "default": {"input": 1.0, "cacheRead": 0.9},
        "rules": {"go": {"input": 2.0, "cacheRead": 0}},
    })
    rate, _ = resolve_rate(explicit_zero, "go", "m")
    assert rate["input"] == 2.0
    assert rate["cacheRead"] == 0  # 显式 0 不被 default 覆盖


def test_cost_math():
    cost = cost_of(PRICING, upstream="go", model="glm-5.3-flash",
                   prompt_tokens=1_000_000, completion_tokens=1_000_000,
                   cache_read_tokens=1_000_000, cache_write_tokens=1_000_000)
    assert abs(cost - (0.6 + 2.4 + 0.1 + 0.5)) < 1e-9


def test_is_configured_distinguishes_zero_from_unset():
    assert is_configured(PRICING) is True
    assert is_configured({}) is False
    assert is_configured({"default": {"input": 0, "output": 0}}) is False
    assert currency_of({"currency": "USD"}) == "USD"
    assert currency_of({}) == "CNY"


def test_pricing_endpoints_roundtrip(bridge_factory):
    handle = bridge_factory()
    # 初始：未配置
    data = handle.client.get(f"{handle.base}/api/pricing").json()
    assert data["configured"] is False
    assert data["retention_days"] == 0

    before = json.loads(handle.bridge.config_path.read_text(encoding="utf-8"))

    # 保存 + 重算
    resp = handle.client.post(f"{handle.base}/api/pricing", json={
        "pricing": PRICING, "recompute": True,
    })
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["configured"] is True
    assert body["currency"] == "CNY"

    # 最小改动写入：只新增 pricing，其余内容逐字保持不变
    # （配置文件是用户的对外契约，里面可能有用户自己手写的字段，
    #   整文件覆盖会把「运行态视图」当事实源写回去，静默丢配置。）
    after = json.loads(handle.bridge.config_path.read_text(encoding="utf-8"))
    assert after["pricing"]["rules"]["go/glm-5.3-flash"]["input"] == 0.6
    assert {k: v for k, v in after.items() if k != "pricing"} == before

    # 运行态即时生效
    again = handle.client.get(f"{handle.base}/api/pricing").json()
    assert again["pricing"]["rules"]["go/glm-5.3-flash"]["input"] == 0.6


def test_pricing_save_does_not_introduce_plaintext_keys(bridge_factory):
    """保存单价表不得把明文密钥写进文件（明文只允许由用户自己手写）。"""
    from conftest import make_config

    config = make_config("http://127.0.0.1:1")
    config["upstreams"] = {
        "ref": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1",
                "apiKeyEnv": "SOME_KEY_ENV", "models": ["m"]},
    }
    handle = bridge_factory(config)

    handle.client.post(f"{handle.base}/api/pricing",
                       json={"pricing": {"default": {"input": 1.0}}})

    text = handle.bridge.config_path.read_text(encoding="utf-8")
    assert "apiKeyEnv" in text  # 引用方式原样保留
    assert "test-key" not in text and "sk-" not in text and "Bearer" not in text
    assert json.loads(text)["pricing"]["default"]["input"] == 1.0


def test_recompute_rewrites_history_cost(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="go", protocol="openai", requested_model="glm-5.3-flash",
                    upstream_model="glm-5.3-flash", prompt_tokens=1_000_000,
                    completion_tokens=1_000_000, duration_ms=10, status=200)
    assert db.totals()["cost"] == 0

    resp = handle.client.post(f"{handle.base}/api/pricing/recompute", json={
        "pricing": {"default": {"input": 1.0, "output": 2.0}},
    })
    assert resp.status_code == 200, resp.text
    assert resp.json()["updated"] == 1
    assert abs(db.totals()["cost"] - 3.0) < 1e-9

    # 未配置单价时重算应被拒绝（而不是把费用清零）
    empty = handle.client.post(f"{handle.base}/api/pricing/recompute", json={})
    assert empty.status_code == 400
