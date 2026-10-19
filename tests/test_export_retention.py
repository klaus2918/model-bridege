# -*- coding: utf-8 -*-
"""数据管理回归：老库迁移幂等、导出（CSV/JSON）、保留策略、统计口径一致性。"""
from __future__ import annotations

import csv
import io
import sqlite3
import time

from modelgate.db import Database

LEGACY_SCHEMA = """
CREATE TABLE usage_records (
    id TEXT PRIMARY KEY, ts INTEGER NOT NULL, created_at TEXT NOT NULL,
    upstream TEXT, protocol TEXT, requested_model TEXT, upstream_model TEXT,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    status INTEGER, stream INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0, paused INTEGER NOT NULL DEFAULT 0,
    error TEXT
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
INSERT INTO usage_records (id, ts, created_at, upstream, requested_model,
                           prompt_tokens, total_tokens, status)
VALUES ('legacy', 1600000000, '2020-09-13T20:26:40.000+08:00', 'old', 'old-model',
        100, 100, 200);
"""


def _columns(db: Database) -> set[str]:
    return {r["name"] for r in
            db._conn.execute("PRAGMA table_info(usage_records)").fetchall()}


def test_legacy_db_migration_is_idempotent(tmp_path):
    """老库打开后自动补列，历史数据一行不少；重复打开不出错。"""
    path = tmp_path / "legacy.db"
    conn = sqlite3.connect(str(path))
    conn.executescript(LEGACY_SCHEMA)
    conn.commit()
    conn.close()

    db = Database(path)
    cols = _columns(db)
    assert {"cost", "session"} <= cols, f"迁移未补齐列：{cols}"
    rows = db.list_usage(limit=10)
    assert len(rows) == 1 and rows[0]["id"] == "legacy"
    assert rows[0]["cost"] == 0 and rows[0]["session"] is None
    db.close()

    # 再开一次：幂等
    again = Database(path)
    assert {"cost", "session"} <= _columns(again)
    assert again.count_usage() == 1
    again.close()


def test_session_and_cost_are_persisted(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="oai", protocol="openai", requested_model="m",
                    upstream_model="m", prompt_tokens=10, completion_tokens=5,
                    duration_ms=7, status=200, cost=0.123456, session="sess-1")
    row = db.list_usage(limit=1)[0]
    assert row["session"] == "sess-1"
    assert abs(row["cost"] - 0.123456) < 1e-9


def test_totals_exposes_cost_success_rate_and_avg(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="a", protocol="openai", requested_model="m",
                    upstream_model="m", prompt_tokens=100, completion_tokens=50,
                    duration_ms=100, status=200, cost=0.5, session="s1")
    db.record_usage(upstream="a", protocol="openai", requested_model="m",
                    upstream_model="m", prompt_tokens=10, duration_ms=300,
                    status=500, error="boom", session="s2")
    totals = db.totals()
    assert totals["requests"] == 2 and totals["ok"] == 1 and totals["errors"] == 1
    assert abs(totals["success_rate"] - 0.5) < 1e-9
    assert abs(totals["cost"] - 0.5) < 1e-9
    assert totals["avg_duration_ms"] == 200.0

    by_session = {r["bucket"]: r for r in db.summary(group_by="session")}
    assert set(by_session) == {"s1", "s2"}
    assert by_session["s2"]["errors"] == 1
    assert "cost" in by_session["s1"]


def test_session_group_handles_missing_session(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="a", protocol="openai", requested_model="m",
                    upstream_model="m", prompt_tokens=1, duration_ms=1, status=200)
    buckets = [r["bucket"] for r in db.summary(group_by="session")]
    assert buckets == ["(无会话)"]


def test_export_csv_has_bom_and_cost_column(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    for i in range(3):
        db.record_usage(upstream="oai", protocol="openai", requested_model=f"m{i}",
                        upstream_model=f"m{i}", prompt_tokens=10 * (i + 1),
                        duration_ms=5, status=200, cost=0.01 * (i + 1),
                        session="sess-x")

    resp = handle.client.get(f"{handle.base}/api/usage/export?range=all&format=csv")
    assert resp.status_code == 200
    assert resp.headers["content-type"].startswith("text/csv")
    assert "attachment" in resp.headers["content-disposition"]
    assert resp.headers["x-row-count"] == "3"
    assert resp.headers["x-truncated"] == "0"
    assert resp.content[:3] == b"\xef\xbb\xbf"  # Excel 直开所需的 BOM

    text = resp.content.decode("utf-8-sig")
    rows = list(csv.reader(io.StringIO(text)))
    assert rows[0][:3] == ["created_at", "upstream", "protocol"]
    assert "cost" in rows[0] and "session" in rows[0]
    assert len(rows) == 4  # 表头 + 3 行
    header = rows[0]
    assert rows[1][header.index("session")] == "sess-x"


def test_export_json_respects_filters(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="oai", protocol="openai", requested_model="keep",
                    upstream_model="keep", prompt_tokens=1, duration_ms=1, status=200)
    db.record_usage(upstream="anth", protocol="anthropic", requested_model="drop",
                    upstream_model="drop", prompt_tokens=1, duration_ms=1, status=200)

    data = handle.client.get(
        f"{handle.base}/api/usage/export?range=all&format=json&upstream=oai").json()
    assert data["count"] == 1
    assert data["rows"][0]["requested_model"] == "keep"
    assert data["truncated"] is False
    # 导出字段里不能出现任何密钥相关键
    assert not any("key" in key.lower() for key in data["rows"][0])


def test_export_payload_never_contains_secrets(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    db.record_usage(upstream="oai", protocol="openai", requested_model="m",
                    upstream_model="m", prompt_tokens=1, duration_ms=1, status=200,
                    session="s1")
    body = handle.client.get(
        f"{handle.base}/api/usage/export?range=all&format=json").text
    for leak in ("apiKey", "test-key", "oai-key", "Bearer", "authorization"):
        assert leak not in body, f"导出内容泄露了 {leak}"


def test_retention_deletes_only_old_rows(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    fresh = db.record_usage(upstream="a", protocol="openai", requested_model="new",
                            upstream_model="new", prompt_tokens=1, duration_ms=1,
                            status=200)
    old = db.record_usage(upstream="a", protocol="openai", requested_model="old",
                          upstream_model="old", prompt_tokens=1, duration_ms=1,
                          status=200)
    db._conn.execute("UPDATE usage_records SET ts = ? WHERE id = ?",
                     (int(time.time()) - 40 * 86400, old))
    db._conn.commit()

    # days=0 → 不清理
    assert db.delete_older_than(days=0) == 0
    assert db.count_usage() == 2

    resp = handle.client.post(f"{handle.base}/api/usage/retention/apply", json={"days": 7})
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body["removed"] == 1
    assert body["after"]["records"] == 1
    remaining = [r["id"] for r in db.list_usage(limit=10)]
    assert remaining == [fresh]


def test_retention_uses_configured_days(bridge_factory):
    """retentionDays 配在配置里时，后台任务按它清理（0 = 不清理）。"""
    handle = bridge_factory()
    db = handle.bridge.db
    row_id = db.record_usage(upstream="a", protocol="openai", requested_model="old",
                             upstream_model="old", prompt_tokens=1, duration_ms=1,
                             status=200)
    db._conn.execute("UPDATE usage_records SET ts = ? WHERE id = ?",
                     (int(time.time()) - 10 * 86400, row_id))
    db._conn.commit()

    handle.bridge.config["retentionDays"] = 3
    assert handle.bridge.apply_retention() == 1
    assert db.count_usage() == 0

    # 关掉保留策略后不再清理
    handle.bridge.config["retentionDays"] = 0
    assert handle.bridge.apply_retention() == 0


def test_list_total_matches_filters(bridge_factory):
    handle = bridge_factory()
    db = handle.bridge.db
    for upstream, model in (("a", "m1"), ("a", "m2"), ("b", "m1")):
        db.record_usage(upstream=upstream, protocol="openai", requested_model=model,
                        upstream_model=model, prompt_tokens=1, duration_ms=1, status=200)

    all_rows = handle.client.get(f"{handle.base}/api/usage/list?range=all").json()
    assert all_rows["total"] == 3
    filtered = handle.client.get(
        f"{handle.base}/api/usage/list?range=all&upstream=a").json()
    assert filtered["total"] == 2 and len(filtered["rows"]) == 2
    by_model = handle.client.get(
        f"{handle.base}/api/usage/list?range=all&model=m2").json()
    assert by_model["total"] == 1
    assert handle.client.get(
        f"{handle.base}/api/usage/list?range=all&upstream=b&model=m2").json()["total"] == 0


def test_usage_scope_endpoints(bridge_factory):
    """KPI / 汇总 / 模型表 / 上游表共用同一作用域，并返回命中率派生字段。"""
    handle = bridge_factory()
    db = handle.bridge.db
    for upstream, model in (("a", "m1"), ("a", "m2"), ("b", "m1")):
        db.record_usage(upstream=upstream, protocol="openai", requested_model=model,
                        upstream_model=model, prompt_tokens=100,
                        cache_read_tokens=40, duration_ms=10, status=200)

    totals = handle.client.get(
        f"{handle.base}/api/usage/totals?range=all&upstream=a").json()["totals"]
    assert totals["requests"] == 2
    assert totals["cache_hit_rate"] == 0.4

    summary = handle.client.get(
        f"{handle.base}/api/usage/summary?group_by=model&range=all&upstream=b").json()
    assert [r["bucket"] for r in summary["rows"]] == ["m1"]

    models = handle.client.get(
        f"{handle.base}/api/usage/models?range=all&upstream=a").json()
    assert {r["model"] for r in models["rows"]} == {"m1", "m2"}

    only_m1 = handle.client.get(
        f"{handle.base}/api/usage/models?range=all&model=m1").json()
    assert {r["model"] for r in only_m1["rows"]} == {"m1"}

    ups = handle.client.get(f"{handle.base}/api/usage/upstreams?range=all").json()
    assert {r["upstream"] for r in ups["rows"]} == {"a", "b"}
    a = next(r for r in ups["rows"] if r["upstream"] == "a")
    assert a["models_count"] == 2 and a["cache_hit_rate"] == 0.4
    assert {(m["upstream"], m["model"]) for m in ups["models"]} == {
        ("a", "m1"), ("a", "m2"), ("b", "m1")}

    # 页面作用域选定上游后，上游表与其下钻都必须同步收窄，不能只收窄 KPI
    scoped = handle.client.get(
        f"{handle.base}/api/usage/upstreams?range=all&upstream=a").json()
    assert [r["upstream"] for r in scoped["rows"]] == ["a"]
    assert {m["model"] for m in scoped["models"]} == {"m1", "m2"}

    bad = handle.client.get(
        f"{handle.base}/api/usage/totals?from=2026-10-03T00:00&to=2026-10-02T00:00")
    assert bad.status_code == 400


def test_clear_usage_respects_custom_window(bridge_factory):
    """自定义区间清空只删该窗口，不会把窗口外的明细一并删掉。"""
    handle = bridge_factory()
    db = handle.bridge.db
    now = int(time.time())
    old = db.record_usage(upstream="a", protocol="openai", requested_model="old",
                          upstream_model="old", prompt_tokens=1, duration_ms=1, status=200)
    db._conn.execute("UPDATE usage_records SET ts = ? WHERE id = ?", (now - 7200, old))
    fresh = db.record_usage(upstream="a", protocol="openai", requested_model="fresh",
                            upstream_model="fresh", prompt_tokens=1, duration_ms=1,
                            status=200)
    db._conn.commit()

    res = handle.client.post(f"{handle.base}/api/usage/clear",
                             json={"from": now - 10800, "to": now - 3600})
    assert res.status_code == 200
    assert res.json()["removed"] == 1
    assert [r["id"] for r in db.list_usage(limit=10)] == [fresh]
