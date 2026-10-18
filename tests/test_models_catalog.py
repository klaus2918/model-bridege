# -*- coding: utf-8 -*-
"""模型清单回归：跨上游聚合、来源标注、别名、套餐门槛、检索与分页。"""
from __future__ import annotations


def _catalog(handle, **params):
    query = "&".join(f"{k}={v}" for k, v in params.items())
    url = f"{handle.base}/api/models/catalog" + (f"?{query}" if query else "")
    resp = handle.client.get(url)
    assert resp.status_code == 200, resp.text
    return resp.json()


def test_catalog_lists_all_upstream_models(bridge_factory):
    handle = bridge_factory()
    data = _catalog(handle)
    models = {row["model"] for row in data["rows"]}
    # conftest 的桩配置：anth 声明 stub-anthropic、oai 声明 stub-openai、cc 声明 stub-cc
    assert {"stub-anthropic", "stub-openai", "stub-cc"} <= models
    assert data["total"] == len(models)

    by_model = {row["model"]: row for row in data["rows"]}
    anth = by_model["stub-anthropic"]
    assert anth["upstreams"] == ["anth"]
    assert anth["protocols"] == ["anthropic"]
    assert "explicit" in anth["sources"]
    assert anth["default"] is True
    assert anth["aliases"] == ["alias-a"]


def test_catalog_search_matches_model_alias_and_upstream(bridge_factory):
    handle = bridge_factory()
    assert _catalog(handle, q="openai")["total"] == 1
    # 别名 alias-a 指向 stub-anthropic
    alias_hit = _catalog(handle, q="alias-a")
    assert alias_hit["total"] == 1
    assert alias_hit["rows"][0]["model"] == "stub-anthropic"
    # 上游名也能命中
    assert _catalog(handle, q="oai")["total"] == 1
    assert _catalog(handle, q="不存在的模型")["total"] == 0


def test_catalog_upstream_filter_and_pagination(bridge_factory):
    handle = bridge_factory()
    filtered = _catalog(handle, upstream="oai")
    assert filtered["total"] == 1
    assert filtered["rows"][0]["upstreams"] == ["oai"]

    page1 = _catalog(handle, page=1, page_size=2)
    page2 = _catalog(handle, page=2, page_size=2)
    assert page1["page_size"] == 2 and len(page1["rows"]) == 2
    assert page1["total"] == page2["total"]
    assert {r["model"] for r in page1["rows"]} & {r["model"] for r in page2["rows"]} == set()

    # 超出范围的页返回空列表而不是报错
    far = _catalog(handle, page=99, page_size=2)
    assert far["rows"] == []
    # page_size 有上限保护
    assert _catalog(handle, page_size=99999)["page_size"] <= 500


def test_catalog_reports_upstream_summary_and_pricing_state(bridge_factory):
    handle = bridge_factory()
    data = _catalog(handle)
    names = {u["name"] for u in data["upstreams"]}
    assert {"anth", "oai", "cc"} <= names
    assert data["pricing_configured"] is False
    assert data["currency"] == "CNY"

    handle.client.post(f"{handle.base}/api/pricing", json={
        "pricing": {"default": {"input": 1.5, "output": 3.0}},
    })
    after = _catalog(handle)
    assert after["pricing_configured"] is True
    stub = next(r for r in after["rows"] if r["model"] == "stub-anthropic")
    assert stub["costHint"]["input"] == 1.5
    assert stub["costRule"] == "default"


def test_catalog_default_flag_follows_config(bridge_factory):
    handle = bridge_factory()
    handle.client.post(f"{handle.base}/api/config",
                       json={"config": {"defaultModel": "stub-openai"}})
    data = _catalog(handle)
    flags = {row["model"]: row["default"] for row in data["rows"]}
    assert flags["stub-openai"] is True
    assert flags["stub-anthropic"] is False
    # 默认模型排在最前，便于一眼看到
    assert data["rows"][0]["model"] == "stub-openai"


def test_catalog_does_not_issue_network_requests(bridge_factory, monkeypatch):
    """清单页刷新必须是纯本地操作：不触发上游调用（避免限流与白等）。"""
    handle = bridge_factory()
    calls = {"n": 0}

    def bomb(*args, **kwargs):  # pragma: no cover - 只用于断言不被调用
        calls["n"] += 1
        raise AssertionError("模型清单不应发起网络请求")

    monkeypatch.setattr(handle.bridge.client, "get", bomb, raising=False)
    monkeypatch.setattr(handle.bridge.client, "send", bomb, raising=False)
    data = _catalog(handle)
    assert data["total"] >= 3
    assert calls["n"] == 0


def test_catalog_price_and_source_filters_are_server_side(bridge_factory):
    """单价状态 / 来源筛选先筛后分页：total 与列表必须自洽（不能是前端假筛选）。"""
    handle = bridge_factory()
    # 只给一个上游的某个模型配价：既有 costHint 的行，也有没有的行
    handle.client.post(f"{handle.base}/api/pricing", json={
        "pricing": {"default": {"input": 0, "output": 0},
                    "rules": {"anth/stub-anthropic": {"input": 1.5, "output": 3.0}}},
    })
    all_rows = _catalog(handle, page_size=200)
    priced = _catalog(handle, price="priced", page_size=200)
    missing = _catalog(handle, price="missing", page_size=200)

    assert priced["total"] + missing["total"] == all_rows["total"]
    assert priced["total"] > 0 and missing["total"] > 0
    assert all(row.get("costHint") for row in priced["rows"])
    assert all(not row.get("costHint") for row in missing["rows"])
    # 每页只取 1 条时，total 仍是筛选后的完整数量
    page = _catalog(handle, price="priced", page=1, page_size=1)
    assert page["total"] == priced["total"] and len(page["rows"]) == 1

    explicit = _catalog(handle, source="explicit", page_size=200)
    assert explicit["total"] > 0
    assert all("explicit" in row["sources"] for row in explicit["rows"])
    # 未登记的取值一律忽略（等价于不过滤），不报错
    assert _catalog(handle, price="nonsense")["total"] == all_rows["total"]
    assert _catalog(handle, source="nonsense")["total"] == all_rows["total"]


def test_catalog_sort_is_server_side_and_echoed(bridge_factory):
    """排序在完整结果集上完成，且把 sort/order 回显给前端做「当前排序」提示。"""
    handle = bridge_factory()
    asc = _catalog(handle, sort="model", order="asc", page_size=200)
    names = [r["model"] for r in asc["rows"]]
    assert names[0] == "stub-anthropic"          # 默认模型置顶
    assert names[1:] == sorted(names[1:], key=str.lower)

    desc = _catalog(handle, sort="model", order="desc", page_size=200)
    others = [n for n in (r["model"] for r in desc["rows"]) if n != "stub-anthropic"]
    assert others == sorted(others, key=str.lower, reverse=True)
    assert desc["sort"] == "model" and desc["order"] == "desc"

    def price_key(row):
        hint = row.get("costHint") or {}
        return float(hint.get("input") or 0) + float(hint.get("output") or 0)

    by_price = _catalog(handle, sort="price", order="desc", page_size=200)
    keys = [price_key(r) for r in by_price["rows"]]
    assert keys == sorted(keys, reverse=True)
