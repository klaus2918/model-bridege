# -*- coding: utf-8 -*-
"""SQLite 数据层：请求级用量明细、配置 settings。

设计要点（对齐参考工程 command-code-gauge 的 db.py）：
- 一次请求一条记录，id 由调用方给（uuid），写入幂等
- ts 存 epoch 秒，便于范围过滤与按天/小时分组聚合
- 时间统一存本机时区的 ISO 文本，便于人工核对日志
- WAL 模式 + 线程锁，供代理线程与 GUI 线程并发访问
- 明细不存请求正文与响应正文，只存元数据与 token 计数（隐私最小化）
"""
from __future__ import annotations

import json
import sqlite3
import threading
import time
import uuid
from pathlib import Path
from typing import Iterable, Optional

from .util import stamp

SCHEMA = """CREATE TABLE IF NOT EXISTS usage_records (
    id                TEXT PRIMARY KEY,
    ts                INTEGER NOT NULL,
    created_at        TEXT NOT NULL,
    upstream          TEXT,
    protocol          TEXT,
    requested_model   TEXT,
    upstream_model    TEXT,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    ttft_ms           INTEGER NOT NULL DEFAULT 0,
    status            INTEGER,
    stream            INTEGER NOT NULL DEFAULT 0,
    truncated         INTEGER NOT NULL DEFAULT 0,
    paused            INTEGER NOT NULL DEFAULT 0,
    error             TEXT,
    cost              REAL NOT NULL DEFAULT 0,
    session           TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_ts     ON usage_records(ts DESC);
CREATE INDEX IF NOT EXISTS idx_usage_model  ON usage_records(requested_model);
CREATE INDEX IF NOT EXISTS idx_usage_up     ON usage_records(upstream);
CREATE INDEX IF NOT EXISTS idx_usage_day    ON usage_records(created_at);
-- 注意：session 索引不能在 SCHEMA 里建 —— 老库此时还没有 session 列，
-- 建索引会报 "no such column"。它放在 MIGRATION_INDEXES，补列之后再建。

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
"""

# 增量迁移：给老库补列（SQLite 无 ADD COLUMN IF NOT EXISTS，故先探测后执行）。
# 只做 ADD COLUMN：不重建表、不丢数据、可重复执行。
MIGRATIONS: tuple[tuple[str, str], ...] = (
    ("usage_records", "cost REAL NOT NULL DEFAULT 0"),
    ("usage_records", "session TEXT"),
    ("usage_records", "ttft_ms INTEGER NOT NULL DEFAULT 0"),
)

# 迁移后补建索引（新库由 SCHEMA 建；老库在这里补）
MIGRATION_INDEXES: tuple[str, ...] = (
    "CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_records(session)",
)


def percentile(values: list[int], ratio: float) -> float:
    """线性插值百分位（``values`` 需已升序）。

    为什么不用 SQL：SQLite 没有 percentile 函数，而把全部耗时取回本地排序
    在本地工具的数据量级（万级）完全够用，且口径清晰可测。
    """
    if not values:
        return 0.0
    if len(values) == 1:
        return float(values[0])
    position = ratio * (len(values) - 1)
    low = int(position)
    high = min(low + 1, len(values) - 1)
    weight = position - low
    return round(float(values[low]) * (1 - weight) + float(values[high]) * weight, 1)


def hit_rate(cache_read: Optional[int], prompt: Optional[int]) -> Optional[float]:
    """缓存命中率 = 命中输入 / 总输入（prompt 含缓存读取部分）。

    分母为 0 时返回 ``None`` 而不是 0：没有输入 token 时「0% 命中」会把
    「无数据」误读成「缓存完全没命中」，前端统一显示「—」。
    """
    read = int(cache_read or 0)
    total = int(prompt or 0)
    if total <= 0:
        return None
    return round(read / total, 6)



# 「只看异常」快捷筛选：SQL 片段白名单，避免任何拼接注入
USAGE_ONLY_CLAUSES = {
    "error": "status >= 400",
    "slow": "duration_ms >= 10000",
    "truncated": "truncated = 1",
    "paused": "paused = 1",
    "cache": "cache_read_tokens > 0",
    "ttft": "ttft_ms > 0",
}

# 明细排序白名单：外部传入的 sort 只在表内命中，未知值一律回落 ts
USAGE_SORT_COLUMNS = {
    "ts": "ts",
    "duration_ms": "duration_ms",
    "ttft_ms": "ttft_ms",
    "prompt_tokens": "prompt_tokens",
    "completion_tokens": "completion_tokens",
    "cache_read_tokens": "cache_read_tokens",
    "total_tokens": "total_tokens",
    "cost": "cost",
    "status": "status",
}


class Database:
    """SQLite 封装（线程安全）。"""

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self._conn = sqlite3.connect(str(self.path), check_same_thread=False)
        self._conn.row_factory = sqlite3.Row
        with self._lock:
            self._conn.execute("PRAGMA journal_mode=WAL")
            self._conn.execute("PRAGMA synchronous=NORMAL")
            self._conn.executescript(SCHEMA)
            self._migrate_locked()
            self._conn.commit()

    def _migrate_locked(self) -> None:
        """幂等增量迁移（调用方需持有锁）。

        老库缺少 cost/session 时补列。任何一步失败都被吞掉：迁移失败不应让
        整个程序打不开 —— 缺列只影响新功能，代理主流程仍可用。
        """
        try:
            for table, column_def in MIGRATIONS:
                column = column_def.split()[0]
                existing = {
                    str(r["name"]) for r in
                    self._conn.execute(f"PRAGMA table_info({table})").fetchall()
                }
                if column not in existing:
                    self._conn.execute(f"ALTER TABLE {table} ADD COLUMN {column_def}")
            for statement in MIGRATION_INDEXES:
                self._conn.execute(statement)
        except Exception:
            pass

    # ── 用量明细 ──────────────────────────────────────────────────────────
    def record_usage(self, *, upstream: str, protocol: str, requested_model: str,
                     upstream_model: str, prompt_tokens: int = 0,
                     completion_tokens: int = 0, cache_read_tokens: int = 0,
                     cache_write_tokens: int = 0, duration_ms: int = 0,
                     ttft_ms: int = 0,
                     status: int = 200, stream: bool = False,
                     truncated: bool = False, paused: bool = False,
                     error: Optional[str] = None, cost: float = 0.0,
                     session: str = "") -> str:
        """写入一条用量明细，返回记录 id。任何字段异常都不会打断请求主流程。"""
        try:
            now = time.time()
            record_id = uuid.uuid4().hex
            prompt = int(prompt_tokens or 0)
            completion = int(completion_tokens or 0)
            with self._lock:
                self._conn.execute(
                    """
                    INSERT INTO usage_records (
                        id, ts, created_at, upstream, protocol, requested_model,
                        upstream_model, prompt_tokens, completion_tokens,
                        cache_read_tokens, cache_write_tokens, total_tokens,
                        duration_ms, ttft_ms, status, stream, truncated, paused, error,
                        cost, session
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        record_id,
                        int(now),
                        stamp(now),
                        upstream,
                        protocol,
                        requested_model,
                        upstream_model,
                        prompt,
                        completion,
                        int(cache_read_tokens or 0),
                        int(cache_write_tokens or 0),
                        prompt + completion,
                        int(duration_ms or 0),
                        int(ttft_ms or 0),
                        int(status or 0),
                        1 if stream else 0,
                        1 if truncated else 0,
                        1 if paused else 0,
                        error or None,
                        float(cost or 0),
                        str(session or "")[:64] or None,
                    ),
                )
                self._conn.commit()
            return record_id
        except Exception:
            # 落库失败绝不能影响代理主流程
            return ""

    def _usage_filters(self, *, since_ts: Optional[int] = None,
                       until_ts: Optional[int] = None, model: Optional[str] = None,
                       upstream: Optional[str] = None,
                       session: Optional[str] = None,
                       only: Optional[str] = None) -> tuple[str, list]:
        """构造明细查询的 WHERE 子句与参数（导出与列表共用，保证口径一致）。"""
        clauses: list[str] = []
        args: list = []
        if since_ts is not None:
            clauses.append("ts >= ?")
            args.append(int(since_ts))
        if until_ts is not None:
            clauses.append("ts <= ?")
            args.append(int(until_ts))
        if model:
            clauses.append("requested_model = ?")
            args.append(model)
        if upstream:
            clauses.append("upstream = ?")
            args.append(upstream)
        if session:
            clauses.append("session = ?")
            args.append(session)
        only_sql = USAGE_ONLY_CLAUSES.get(only or "")
        if only_sql:
            clauses.append(only_sql)
        where = f" WHERE {' AND '.join(clauses)}" if clauses else ""
        return where, args

    def list_usage(self, *, limit: int = 100, offset: int = 0,
                   since_ts: Optional[int] = None, until_ts: Optional[int] = None,
                   model: Optional[str] = None, upstream: Optional[str] = None,
                   session: Optional[str] = None, only: Optional[str] = None,
                   sort: str = "ts", order: str = "desc",
                   ascending: bool = False) -> list[dict]:
        """按条件列出明细。

        - ``only`` 走 USAGE_ONLY_CLAUSES 白名单（error/slow/truncated/paused/cache/ttft）
        - ``sort``/``order`` 走 USAGE_SORT_COLUMNS 白名单，非白名单值一律回落 ts
        - ``ascending=True`` 供导出使用（按时间正序更易读），优先于 order
        """
        where, args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, model=model,
            upstream=upstream, session=session, only=only)
        column = USAGE_SORT_COLUMNS.get(sort or "ts", "ts")
        direction = "ASC" if ascending or str(order).lower() == "asc" else "DESC"
        # 非时间列排序时以 ts 兜底，保证同值行的顺序稳定（翻页不会串行）
        tie = "" if column == "ts" else ", ts DESC"
        with self._lock:
            rows = self._conn.execute(
                f"SELECT * FROM usage_records{where} "
                f"ORDER BY {column} {direction}{tie} LIMIT ? OFFSET ?",
                (*args, int(limit), int(offset)),
            ).fetchall()
        return [dict(r) for r in rows]

    def clear_usage(self, *, since_ts: Optional[int] = None) -> int:
        """按区间清空用量明细；since_ts 为空表示全部清空。返回删除行数。"""
        with self._lock:
            if since_ts is None:
                cursor = self._conn.execute("DELETE FROM usage_records")
            else:
                cursor = self._conn.execute(
                    "DELETE FROM usage_records WHERE ts >= ?", (int(since_ts),)
                )
            removed = cursor.rowcount if cursor.rowcount is not None else 0
            self._conn.commit()
            return int(removed)

    def count_usage(self, *, since_ts: Optional[int] = None,
                    until_ts: Optional[int] = None, model: Optional[str] = None,
                    upstream: Optional[str] = None,
                    session: Optional[str] = None,
                    only: Optional[str] = None) -> int:
        """统计明细条数，过滤条件与 list_usage 完全一致（分页总数才不会虚高）。"""
        where, args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, model=model,
            upstream=upstream, session=session, only=only)
        with self._lock:
            row = self._conn.execute(
                f"SELECT COUNT(*) AS n FROM usage_records{where}", args).fetchone()
        return int(row["n"])

    def summary(self, *, since_ts: Optional[int] = None,
                until_ts: Optional[int] = None,
                group_by: str = "day",
                model: Optional[str] = None,
                upstream: Optional[str] = None,
                session: Optional[str] = None) -> list[dict]:
        """按 day / hour / model / upstream / session 分组聚合用量。

        过滤口径与明细一致（时间区间 + 上游 + 请求模型 + 会话），
        返回 [{bucket, requests, prompt_tokens, completion_tokens, total_tokens,
                cache_read_tokens, cache_hit_rate, cost, errors,
                avg_duration_ms, avg_ttft_ms, p50_duration_ms, p95_duration_ms}]，按桶升序。
        """
        if group_by == "day":
            bucket_expr = "substr(created_at, 1, 10)"
        elif group_by == "hour":
            bucket_expr = "substr(created_at, 1, 13)"
        elif group_by == "model":
            bucket_expr = "COALESCE(requested_model, '(unknown)')"
        elif group_by == "upstream":
            bucket_expr = "COALESCE(upstream, '(unknown)')"
        elif group_by == "session":
            bucket_expr = "COALESCE(NULLIF(session, ''), '(无会话)')"
        else:
            raise ValueError(f"不支持的分组维度: {group_by}")
        where, args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, model=model,
            upstream=upstream, session=session)
        args = tuple(args)
        with self._lock:
            rows = self._conn.execute(
                f"""
                SELECT {bucket_expr} AS bucket,
                       COUNT(*)                AS requests,
                       COALESCE(SUM(prompt_tokens), 0)     AS prompt_tokens,
                       COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                       COALESCE(SUM(total_tokens), 0)      AS total_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                       COALESCE(SUM(cost), 0)              AS cost,
                       COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0) AS errors,
                       COALESCE(AVG(duration_ms), 0)       AS avg_duration_ms,
                       COALESCE(AVG(NULLIF(ttft_ms, 0)), 0) AS avg_ttft_ms
                FROM usage_records{where}
                GROUP BY bucket
                ORDER BY bucket
                """,
                args,
            ).fetchall()
            duration_rows = self._conn.execute(
                f"SELECT {bucket_expr} AS bucket, duration_ms FROM usage_records{where} "
                f"ORDER BY bucket, duration_ms",
                args,
            ).fetchall()
        grouped: dict[str, list[int]] = {}
        for item in duration_rows:
            grouped.setdefault(str(item["bucket"]), []).append(int(item["duration_ms"] or 0))

        out = []
        for row in rows:
            item = dict(row)
            samples = grouped.get(str(item.get("bucket")), [])
            item["cost"] = round(float(item.get("cost") or 0), 6)
            item["cache_hit_rate"] = hit_rate(
                item.get("cache_read_tokens"), item.get("prompt_tokens"))
            item["avg_duration_ms"] = round(float(item.get("avg_duration_ms") or 0), 1)
            item["avg_ttft_ms"] = round(float(item.get("avg_ttft_ms") or 0), 1)
            item["p50_duration_ms"] = percentile(samples, 0.50)
            item["p95_duration_ms"] = percentile(samples, 0.95)
            out.append(item)
        return out

    def totals(self, *, since_ts: Optional[int] = None,
               until_ts: Optional[int] = None,
               model: Optional[str] = None,
               upstream: Optional[str] = None,
               session: Optional[str] = None) -> dict:
        """全局合计：请求数、token、费用、成功率、耗时（均值 + P50/P95）+ TTFT。

        过滤口径与明细列表一致（``_usage_filters``），供页面级作用域复用：
        时间区间 + 上游 + 请求模型 + 会话。``until_ts`` 同时服务「与上一区间对比」。
        """
        where, raw_args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, model=model,
            upstream=upstream, session=session)
        args = tuple(raw_args)

        with self._lock:
            row = self._conn.execute(
                f"""
                SELECT COUNT(*)                AS requests,
                       COALESCE(SUM(prompt_tokens), 0)     AS prompt_tokens,
                       COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                       COALESCE(SUM(total_tokens), 0)      AS total_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost), 0)              AS cost,
                       COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0) AS errors,
                       COALESCE(SUM(CASE WHEN status IS NULL OR status < 400
                                         THEN 1 ELSE 0 END), 0) AS ok,
                       COALESCE(AVG(duration_ms), 0)       AS avg_duration_ms,
                       COALESCE(AVG(NULLIF(ttft_ms, 0)), 0) AS avg_ttft_ms,
                       COALESCE(SUM(CASE WHEN ttft_ms > 0 THEN 1 ELSE 0 END), 0) AS ttft_samples
                FROM usage_records{where}
                """,
                args,
            ).fetchone()
            durations = [
                int(r["duration_ms"] or 0) for r in
                self._conn.execute(
                    f"SELECT duration_ms FROM usage_records{where} ORDER BY duration_ms",
                    args).fetchall()
            ]
        data = dict(row)
        requests = int(data.get("requests") or 0)
        ok = int(data.get("ok") or 0)
        data["requests"] = requests
        data["ok"] = ok
        data["errors"] = int(data.get("errors") or 0)
        data["success_rate"] = round(ok / requests, 4) if requests else 0.0
        data["cache_hit_rate"] = hit_rate(
            data.get("cache_read_tokens"), data.get("prompt_tokens"))
        data["cost"] = round(float(data.get("cost") or 0), 6)
        data["avg_duration_ms"] = round(float(data.get("avg_duration_ms") or 0), 1)
        data["avg_ttft_ms"] = round(float(data.get("avg_ttft_ms") or 0), 1)
        data["ttft_samples"] = int(data.get("ttft_samples") or 0)
        data["p50_duration_ms"] = percentile(durations, 0.50)
        data["p95_duration_ms"] = percentile(durations, 0.95)
        data["max_duration_ms"] = float(durations[-1]) if durations else 0.0
        return data

    def model_usage(self, *, since_ts: Optional[int] = None,
                    until_ts: Optional[int] = None,
                    upstream: Optional[str] = None,
                    model: Optional[str] = None,
                    session: Optional[str] = None,
                    limit: int = 100) -> list[dict]:
        """按模型聚合用量（供「模型用量」表）。

        过滤口径与明细一致：时间区间 + 上游 + 会话；返回
        ``[{model, requests, errors, success_rate, prompt_tokens,
        completion_tokens, total_tokens, cache_read_tokens, cache_write_tokens,
        cache_hit_rate, cost, avg_duration_ms, p50_duration_ms, p95_duration_ms,
        avg_ttft_ms, last_ts}]``，按总 token 降序 —— 用量最大的模型排最前，
        这样「哪些模型在吃 token」一眼可见。
        """
        where, raw_args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, model=model,
            upstream=upstream, session=session)
        args = tuple(raw_args)
        with self._lock:
            rows = self._conn.execute(
                f"""
                SELECT COALESCE(requested_model, '(unknown)') AS model,
                       COUNT(*)                AS requests,
                       COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0) AS errors,
                       COALESCE(SUM(prompt_tokens), 0)     AS prompt_tokens,
                       COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                       COALESCE(SUM(total_tokens), 0)      AS total_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost), 0)              AS cost,
                       COALESCE(AVG(duration_ms), 0)       AS avg_duration_ms,
                       COALESCE(AVG(NULLIF(ttft_ms, 0)), 0) AS avg_ttft_ms,
                       MAX(ts)                             AS last_ts
                FROM usage_records{where}
                GROUP BY model
                ORDER BY total_tokens DESC
                LIMIT ?
                """,
                (*args, int(limit)),
            ).fetchall()
            duration_rows = self._conn.execute(
                f"SELECT COALESCE(requested_model, '(unknown)') AS model, duration_ms "
                f"FROM usage_records{where} ORDER BY model, duration_ms",
                args,
            ).fetchall()

        grouped: dict[str, list[int]] = {}
        for item in duration_rows:
            grouped.setdefault(str(item["model"]), []).append(int(item["duration_ms"] or 0))

        out = []
        for row in rows:
            item = dict(row)
            samples = grouped.get(str(item["model"]), [])
            requests = int(item.get("requests") or 0)
            errors = int(item.get("errors") or 0)
            item["success_rate"] = round((requests - errors) / requests, 4) if requests else 0.0
            item["cost"] = round(float(item.get("cost") or 0), 6)
            item["cache_hit_rate"] = hit_rate(
                item.get("cache_read_tokens"), item.get("prompt_tokens"))
            item["avg_duration_ms"] = round(float(item.get("avg_duration_ms") or 0), 1)
            item["avg_ttft_ms"] = round(float(item.get("avg_ttft_ms") or 0), 1)
            item["p50_duration_ms"] = percentile(samples, 0.50)
            item["p95_duration_ms"] = percentile(samples, 0.95)
            out.append(item)
        return out

    def upstream_usage(self, *, since_ts: Optional[int] = None,
                       until_ts: Optional[int] = None,
                       upstream: Optional[str] = None,
                       model: Optional[str] = None,
                       session: Optional[str] = None,
                       limit: int = 200) -> list[dict]:
        """按上游聚合用量（供「上游用量」表）。

        过滤口径与明细一致：时间区间 + 上游 + 请求模型 + 会话。返回
        ``[{upstream, models_count, requests, errors, success_rate,
        prompt_tokens, completion_tokens, total_tokens, cache_read_tokens,
        cache_write_tokens, cache_hit_rate, cost, avg_duration_ms,
        p50_duration_ms, p95_duration_ms, avg_ttft_ms, last_ts}]``。
        """
        where, raw_args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, upstream=upstream,
            model=model, session=session)
        args = tuple(raw_args)
        with self._lock:
            rows = self._conn.execute(
                f"""
                SELECT COALESCE(upstream, '(unknown)') AS upstream,
                       COUNT(DISTINCT COALESCE(requested_model, '(unknown)')) AS models_count,
                       COUNT(*)                AS requests,
                       COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0) AS errors,
                       COALESCE(SUM(prompt_tokens), 0)     AS prompt_tokens,
                       COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                       COALESCE(SUM(total_tokens), 0)      AS total_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost), 0)              AS cost,
                       COALESCE(AVG(duration_ms), 0)       AS avg_duration_ms,
                       COALESCE(AVG(NULLIF(ttft_ms, 0)), 0) AS avg_ttft_ms,
                       MAX(ts)                             AS last_ts
                FROM usage_records{where}
                GROUP BY upstream
                ORDER BY total_tokens DESC
                LIMIT ?
                """,
                (*args, int(limit)),
            ).fetchall()
            duration_rows = self._conn.execute(
                f"SELECT COALESCE(upstream, '(unknown)') AS upstream, duration_ms "
                f"FROM usage_records{where} ORDER BY upstream, duration_ms",
                args,
            ).fetchall()

        grouped: dict[str, list[int]] = {}
        for item in duration_rows:
            grouped.setdefault(str(item["upstream"]), []).append(int(item["duration_ms"] or 0))

        out = []
        for row in rows:
            item = dict(row)
            samples = grouped.get(str(item["upstream"]), [])
            requests = int(item.get("requests") or 0)
            errors = int(item.get("errors") or 0)
            item["success_rate"] = round((requests - errors) / requests, 4) if requests else 0.0
            item["cache_hit_rate"] = hit_rate(
                item.get("cache_read_tokens"), item.get("prompt_tokens"))
            item["cost"] = round(float(item.get("cost") or 0), 6)
            item["avg_duration_ms"] = round(float(item.get("avg_duration_ms") or 0), 1)
            item["avg_ttft_ms"] = round(float(item.get("avg_ttft_ms") or 0), 1)
            item["p50_duration_ms"] = percentile(samples, 0.50)
            item["p95_duration_ms"] = percentile(samples, 0.95)
            out.append(item)
        return out

    def upstream_model_usage(self, *, since_ts: Optional[int] = None,
                             until_ts: Optional[int] = None,
                             upstream: Optional[str] = None,
                             model: Optional[str] = None,
                             session: Optional[str] = None,
                             limit: int = 500) -> list[dict]:
        """按「上游 × 请求模型」聚合，供上游行展开下钻。

        返回 ``[{upstream, model, requests, errors, success_rate,
        prompt_tokens, completion_tokens, total_tokens, cache_read_tokens,
        cache_hit_rate, cost, avg_duration_ms, avg_ttft_ms, last_ts}]``。
        """
        where, raw_args = self._usage_filters(
            since_ts=since_ts, until_ts=until_ts, upstream=upstream,
            model=model, session=session)
        args = tuple(raw_args)
        with self._lock:
            rows = self._conn.execute(
                f"""
                SELECT COALESCE(upstream, '(unknown)') AS upstream,
                       COALESCE(requested_model, '(unknown)') AS model,
                       COUNT(*)                AS requests,
                       COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0) AS errors,
                       COALESCE(SUM(prompt_tokens), 0)     AS prompt_tokens,
                       COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
                       COALESCE(SUM(total_tokens), 0)      AS total_tokens,
                       COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                       COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens,
                       COALESCE(SUM(cost), 0)              AS cost,
                       COALESCE(AVG(duration_ms), 0)       AS avg_duration_ms,
                       COALESCE(AVG(NULLIF(ttft_ms, 0)), 0) AS avg_ttft_ms,
                       MAX(ts)                             AS last_ts
                FROM usage_records{where}
                GROUP BY upstream, model
                ORDER BY upstream, total_tokens DESC
                LIMIT ?
                """,
                (*args, int(limit)),
            ).fetchall()

        out = []
        for row in rows:
            item = dict(row)
            requests = int(item.get("requests") or 0)
            errors = int(item.get("errors") or 0)
            item["success_rate"] = round((requests - errors) / requests, 4) if requests else 0.0
            item["cache_hit_rate"] = hit_rate(
                item.get("cache_read_tokens"), item.get("prompt_tokens"))
            item["cost"] = round(float(item.get("cost") or 0), 6)
            item["avg_duration_ms"] = round(float(item.get("avg_duration_ms") or 0), 1)
            item["avg_ttft_ms"] = round(float(item.get("avg_ttft_ms") or 0), 1)
            out.append(item)
        return out

    # ── 数据管理：保留策略、空间回收、费用重算 ────────────────────────────
    def delete_older_than(self, *, days: int) -> int:
        """删除 ``days`` 天之前的明细，返回删除条数。``days<=0`` 表示不清理。"""
        try:
            days = int(days or 0)
        except (TypeError, ValueError):
            return 0
        if days <= 0:
            return 0
        cutoff = int(time.time()) - days * 86400
        with self._lock:
            cursor = self._conn.execute(
                "DELETE FROM usage_records WHERE ts < ?", (cutoff,))
            removed = cursor.rowcount or 0
            self._conn.commit()
        return int(removed)

    def usage_count_and_size(self) -> dict:
        """返回 {records, bytes}，供保留策略前后对比。"""
        with self._lock:
            row = self._conn.execute(
                "SELECT COUNT(*) AS n FROM usage_records").fetchone()
        records = int(row["n"]) if row else 0
        try:
            size = self.path.stat().st_size
        except OSError:
            size = 0
        return {"records": records, "bytes": int(size)}

    def vacuum(self) -> int:
        """回收已删除数据占用的空间，返回回收后的文件字节数。"""
        with self._lock:
            try:
                self._conn.commit()
                self._conn.execute("VACUUM")
            except Exception:
                pass
        try:
            return int(self.path.stat().st_size)
        except OSError:
            return 0

    def recompute_costs(self, pricing) -> int:
        """按给定单价表重算全部历史明细的费用，返回被更新的条数。

        改单价后必须能重算历史 —— 否则改价只影响新记录，统计会自相矛盾。
        逐行比对后再批量 UPDATE，避免无意义的写放大。
        """
        from .pricing import cost_of

        updated = 0
        with self._lock:
            rows = self._conn.execute(
                "SELECT id, upstream, requested_model, prompt_tokens, completion_tokens,"
                " cache_read_tokens, cache_write_tokens, cost FROM usage_records"
            ).fetchall()
            batch: list[tuple[float, str]] = []
            for row in rows:
                new_cost = cost_of(
                    pricing,
                    upstream=row["upstream"],
                    model=row["requested_model"],
                    prompt_tokens=int(row["prompt_tokens"] or 0),
                    completion_tokens=int(row["completion_tokens"] or 0),
                    cache_read_tokens=int(row["cache_read_tokens"] or 0),
                    cache_write_tokens=int(row["cache_write_tokens"] or 0),
                )
                if abs(new_cost - float(row["cost"] or 0)) > 1e-9:
                    batch.append((new_cost, str(row["id"])))
            if batch:
                self._conn.executemany(
                    "UPDATE usage_records SET cost = ? WHERE id = ?", batch)
                updated = len(batch)
            self._conn.commit()
        return updated

    # ── settings（运行时配置事实源） ──────────────────────────────────────
    def get_setting(self, key: str, default: Optional[str] = None) -> Optional[str]:
        with self._lock:
            row = self._conn.execute(
                "SELECT value FROM settings WHERE key = ?", (key,)
            ).fetchone()
        return row["value"] if row else default

    def set_setting(self, key: str, value: str) -> None:
        with self._lock:
            self._conn.execute(
                "INSERT INTO settings (key, value) VALUES (?, ?) "
                "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (key, value),
            )
            self._conn.commit()

    def all_settings(self) -> dict[str, str]:
        with self._lock:
            rows = self._conn.execute("SELECT key, value FROM settings").fetchall()
        return {r["key"]: r["value"] for r in rows}

    def load_config_override(self) -> dict:
        """把 settings 里保存的 JSON 配置读出来（没有则空字典）。"""
        raw = self.get_setting("config")
        if not raw:
            return {}
        try:
            data = json.loads(raw)
            return data if isinstance(data, dict) else {}
        except Exception:
            return {}

    def save_config_override(self, config: dict) -> None:
        self.set_setting("config", json.dumps(config, ensure_ascii=False))

    def clear_usage(self, *, since_ts: Optional[int] = None,
                    until_ts: Optional[int] = None) -> int:
        """清空指定时间区间的用量明细，返回删除条数。

        只删 ``usage_records``，不动 settings；两端都为空表示全部清空。
        与页面作用域同口径，避免自定义区间下「清空」误删全表。
        """
        where, args = self._usage_filters(since_ts=since_ts, until_ts=until_ts)
        with self._lock:
            cur = self._conn.execute(
                f"DELETE FROM usage_records{where}", tuple(args))
            removed = cur.rowcount or 0
            self._conn.commit()
        return int(removed)

    def settings(self) -> dict:
        return self.all_settings()

    def close(self) -> None:
        with self._lock:
            try:
                self._conn.close()
            except Exception:
                pass


def open_db(path: str | Path) -> Database:
    """便捷入口：确保父目录存在并打开数据库。"""
    return Database(path)
