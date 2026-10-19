# -*- coding: utf-8 -*-
"""用量查询回归：排序白名单 / 「只看异常」筛选 / 口径一致 / 环比窗口。

覆盖用量面板深化的后端契约：
- ``list_usage`` 的 sort/order/only 只走白名单，未知值回落 ts，不做字符串拼接；
- ``count_usage`` 与 ``list_usage`` 过滤口径一致（分页 total 不虚高）；
- ``totals`` 支持 until_ts，``api._previous_window`` 提供等长的上一区间。
"""
from __future__ import annotations

import time

import pytest

from modelgate import api
from modelgate.db import USAGE_ONLY_CLAUSES, USAGE_SORT_COLUMNS, Database


@pytest.fixture()
def db(tmp_path):
    database = Database(tmp_path / "usage.db")
    rows = [
        # duration, ttft, status, truncated, paused, cache_read, model, upstream
        (1200, 240, 200, False, False, 100, "m-a", "go"),
        (9000, 480, 200, False, False, 0, "m-a", "go"),
        (52000, 0, 200, False, False, 0, "m-b", "cch"),
        (800, 120, 500, False, False, 0, "m-b", "cch"),
        (4200, 300, 400, False, True, 900, "m-c", "zen"),
        (60000, 900, 200, True, False, 0, "m-a", "cch"),
    ]
    for i, (dur, ttft, status, trunc, paused, cache, model, up) in enumerate(rows):
        database.record_usage(
            upstream=up, protocol="openai", requested_model=model,
            upstream_model=model, prompt_tokens=1000 + i, completion_tokens=10 * i,
            cache_read_tokens=cache, duration_ms=dur, ttft_ms=ttft,
            status=status, stream=True, truncated=trunc, paused=paused,
            error="boom" if status >= 400 else None, session=f"s{i % 2}",
        )
    yield database
    database.close()


def test_sort_whitelist_orders_by_column(db):
    """按耗时降序/升序都生效，且白名单外的列名回落 ts（不报错、不注入）。"""
    desc = db.list_usage(limit=10, sort="duration_ms", order="desc")
    assert [r["duration_ms"] for r in desc] == [60000, 52000, 9000, 4200, 1200, 800]

    asc = db.list_usage(limit=10, sort="duration_ms", order="asc")
    assert [r["duration_ms"] for r in asc] == [800, 1200, 4200, 9000, 52000, 60000]

    # 未知列名：回落 ts，不抛异常，也不把外部字符串带进 SQL
    fallback = db.list_usage(limit=3, sort="ts; DROP TABLE usage_records")
    assert len(fallback) == 3
    assert db.count_usage() == 6  # 明细表仍在


def test_sort_handles_missing_ttft(db):
    """ttft_ms 为 0（非流式或未采样）的行排在末尾，不会挤掉真实采样。"""
    rows = db.list_usage(limit=10, sort="ttft_ms", order="desc")
    assert [r["ttft_ms"] for r in rows][:3] == [900, 480, 300]
    assert rows[-1]["ttft_ms"] == 0


def test_only_filters_and_total_agree(db):
    """「只看异常」各档筛选命中的行数与分页 total 完全一致。"""
    checks = {
        "error": lambda r: r["status"] >= 400,
        "slow": lambda r: r["duration_ms"] >= 10000,
        "truncated": lambda r: bool(r["truncated"]),
        "paused": lambda r: bool(r["paused"]),
        "cache": lambda r: r["cache_read_tokens"] > 0,
        "ttft": lambda r: r["ttft_ms"] > 0,
    }
    all_rows = db.list_usage(limit=50)
    for key, predicate in checks.items():
        rows = db.list_usage(limit=50, only=key)
        total = db.count_usage(only=key)
        assert rows and all(predicate(r) for r in rows), key
        assert total == len(rows) == sum(1 for r in all_rows if predicate(r)), key

    # 未登记的筛选值一律忽略（等价于不过滤），不报错
    assert db.count_usage(only="'; DROP TABLE usage_records; --") == 6


def test_only_combinable_with_filters(db):
    """快捷筛选与模型/上游筛选可叠加，且列表与总数同步收窄。"""
    rows = db.list_usage(limit=50, only="ttft", model="m-a")
    assert len(rows) == 3 and all(r["requested_model"] == "m-a" for r in rows)
    assert db.count_usage(only="ttft", model="m-a") == 3
    assert db.count_usage(only="error", upstream="cch") == 1


def test_totals_window_and_previous(db):
    """totals 支持右边界；环比窗口长度与当前区间一致且不重叠。"""
    now = int(time.time())
    assert db.totals(since_ts=now - 60)["requests"] == 6
    assert db.totals(since_ts=now - 60, until_ts=now - 7200)["requests"] == 0

    prev_since, prev_until = api._previous_window({"range": "1h"})
    assert prev_until - prev_since == 3600
    assert prev_until <= now - 3600 + 2
    assert api._previous_window({"range": "all"}) == (None, None)
    assert set(USAGE_ONLY_CLAUSES) >= {"error", "slow", "truncated", "paused", "cache", "ttft"}


def test_totals_filters_by_upstream_and_model(db):
    """页面级作用域：totals 同时按上游、请求模型、会话收窄。"""
    assert db.totals()["requests"] == 6
    assert db.totals(upstream="go")["requests"] == 2
    assert db.totals(upstream="cch")["requests"] == 3
    assert db.totals(upstream="go", model="m-a")["requests"] == 2
    assert db.totals(upstream="cch", model="m-b")["requests"] == 2
    assert db.totals(upstream="cch", model="m-a")["requests"] == 1
    assert db.totals(upstream="cch", model="m-a", session="s1")["requests"] == 1


def test_summary_respects_scope_and_reports_hit_rate(db):
    """summary 的时间/上游/模型过滤与明细同口径，并返回命中率派生字段。"""
    rows = db.summary(group_by="upstream")
    go = next(r for r in rows if r["bucket"] == "go")
    assert go["requests"] == 2
    # go: prompt 1000 + 1001，cache_read 100 + 0
    assert go["cache_hit_rate"] == pytest.approx(100 / 2001, rel=1e-4)

    filtered = db.summary(group_by="model", upstream="cch")
    assert {r["bucket"] for r in filtered} == {"m-a", "m-b"}

    empty = db.summary(group_by="upstream", since_ts=int(time.time()) + 3600)
    assert empty == []


def test_upstream_usage_and_drilldown(db):
    """上游聚合含模型数与命中率；上游×模型可按上游下钻。"""
    rows = db.upstream_usage()
    assert {r["upstream"] for r in rows} == {"go", "cch", "zen"}
    cch = next(r for r in rows if r["upstream"] == "cch")
    assert cch["models_count"] == 2          # cch 下有 m-a / m-b
    assert cch["requests"] == 3

    drill = db.upstream_model_usage(upstream="cch")
    assert {"m-a", "m-b"} == {r["model"] for r in drill}
    assert all(r["upstream"] == "cch" for r in drill)

    # 模型过滤同时作用于上游聚合与下钻
    only_mb = db.upstream_usage(model="m-b")
    assert [r["upstream"] for r in only_mb] == ["cch"]
    assert [r["model"] for r in db.upstream_model_usage(model="m-b")] == ["m-b"]


def test_hit_rate_null_when_prompt_zero():
    """没有输入 token 时命中率为 None，不能显示成 0%。"""
    from modelgate.db import hit_rate

    assert hit_rate(10, 0) is None
    assert hit_rate(0, 100) == 0.0
    assert hit_rate(1, 4) == 0.25


def test_usage_scope_custom_window():
    """自定义时间窗口：from/to 优先于 range，倒置区间报错且不比静默回落。"""
    since, until = api._usage_scope({
        "from": "2026-10-01T00:00", "to": "2026-10-02T00:00"})
    assert until - since == 86400

    with pytest.raises(ValueError):
        api._usage_scope({"from": "2026-10-03T00:00", "to": "2026-10-02T00:00"})
    with pytest.raises(ValueError):
        api._usage_scope({"from": "not-a-time"})

    assert api._usage_scope({"range": "all"}) == (None, None)
    prev_since, prev_until = api._previous_window({
        "from": "2026-10-01T00:00", "to": "2026-10-02T00:00"})
    assert prev_until - prev_since == 86400
    assert prev_until == since

