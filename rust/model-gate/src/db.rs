//! SQLite 数据层：与 Python 版同库、同表、同 SQL 口径。
//!
//! 读写分离：一条写连接 + 一组只读连接（WAL）。百分位在 SQLite 侧用窗口函数
//! 取两个边界样本，避免把全表 `duration_ms` 搬进内存。
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Local;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Map, Value};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS usage_records (
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
CREATE TABLE IF NOT EXISTS prompt_records (
    id              TEXT PRIMARY KEY,
    ts              INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    session         TEXT,
    requested_model TEXT,
    message_count   INTEGER NOT NULL DEFAULT 0,
    size_bytes      INTEGER NOT NULL DEFAULT 0,
    truncated       INTEGER NOT NULL DEFAULT 0,
    preview         TEXT,
    messages        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prompt_ts      ON prompt_records(ts DESC);
CREATE INDEX IF NOT EXISTS idx_prompt_session ON prompt_records(session);
CREATE INDEX IF NOT EXISTS idx_prompt_model   ON prompt_records(requested_model);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
"#;

const MIGRATIONS: &[(&str, &str)] = &[
    ("usage_records", "cost REAL NOT NULL DEFAULT 0"),
    ("usage_records", "session TEXT"),
    ("usage_records", "ttft_ms INTEGER NOT NULL DEFAULT 0"),
];

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub since_ts: Option<i64>,
    pub until_ts: Option<i64>,
    pub model: Option<String>,
    pub upstream: Option<String>,
    pub session: Option<String>,
    /// 快捷筛选：error/slow/truncated/paused/cache/ttft（白名单在 where_clause 内）。
    pub only: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    /// 可选请求 ID；为空时由 record_usage 生成。用于与 prompt_records 一一关联。
    pub id: String,
    pub upstream: String,
    pub protocol: String,
    pub requested_model: String,
    pub upstream_model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub duration_ms: i64,
    pub ttft_ms: i64,
    pub status: i64,
    pub stream: bool,
    pub truncated: bool,
    pub paused: bool,
    pub error: Option<String>,
    pub cost: f64,
    pub session: String,
}

/// 入站提示词记录；`id` 与同一次请求的 usage_records.id 相同。
#[derive(Debug, Clone, Default)]
pub struct PromptRecord {
    pub id: String,
    /// 0 表示使用当前时间（导入/测试可指定历史时间）。
    pub ts: i64,
    pub session: String,
    pub requested_model: String,
    pub message_count: i64,
    pub size_bytes: i64,
    pub truncated: bool,
    pub preview: String,
    /// 入站 messages 的 JSON 数组字符串。
    pub messages: String,
}

/// 提示词查询条件；与用量 `Filters` 分开，避免把 LIKE 条件带进用量查询。
#[derive(Debug, Clone, Default)]
pub struct PromptFilters {
    pub since_ts: Option<i64>,
    pub until_ts: Option<i64>,
    pub model: Option<String>,
    pub session: Option<String>,
    pub upstream: Option<String>,
    pub q: Option<String>,
}

pub struct Db {
    pub path: PathBuf,
    write: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let write = connect(path)?;
        write.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )?;
        write.execute_batch(SCHEMA)?;
        migrate(&write);
        let readers = (0..2)
            .map(|_| connect(path).map(Mutex::new))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            path: path.to_path_buf(),
            write: Mutex::new(write),
            readers,
        })
    }

    fn reader(&self) -> std::sync::MutexGuard<'_, Connection> {
        // 轮转使用只读连接；两条连接足够覆盖面板的并发统计请求。
        let idx = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.readers.len();
        self.readers[idx].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 把 WAL 日志落盘并截断：退出前调用，确保用量数据真正写进主库文件。
    ///
    /// 为什么需要：数据库开的是 WAL，最近的写入可能还留在 `modelgate.db-wal`
    /// 里。升级安装时旧进程会被替换掉，WAL 文件若没落盘，最新的用量记录会丢。
    /// `wal_checkpoint(TRUNCATE)` 会把 WAL 内容合并回主库并清空 WAL。
    ///
    /// 失败只记日志不报错：checkpoint 失败不该阻断退出流程。
    pub fn checkpoint(&self) {
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            tracing::warn!("数据库 WAL checkpoint 失败：{err}");
        }
    }

    pub fn record_usage(&self, r: &UsageRecord) -> String {
        let id = if r.id.is_empty() {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            r.id.clone()
        };
        let now = Local::now();
        let result = (|| -> rusqlite::Result<()> {
            let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute(
                "INSERT INTO usage_records (
                    id, ts, created_at, upstream, protocol, requested_model,
                    upstream_model, prompt_tokens, completion_tokens,
                    cache_read_tokens, cache_write_tokens, total_tokens,
                    duration_ms, ttft_ms, status, stream, truncated, paused, error,
                    cost, session
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                rusqlite::params![
                    id,
                    now.timestamp(),
                    now.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
                    r.upstream.as_str(),
                    r.protocol.as_str(),
                    r.requested_model.as_str(),
                    r.upstream_model.as_str(),
                    r.prompt_tokens,
                    r.completion_tokens,
                    r.cache_read_tokens,
                    r.cache_write_tokens,
                    r.prompt_tokens + r.completion_tokens,
                    r.duration_ms,
                    r.ttft_ms,
                    r.status,
                    r.stream as i64,
                    r.truncated as i64,
                    r.paused as i64,
                    r.error.clone(),
                    r.cost,
                    if r.session.is_empty() { None } else { Some(r.session.clone()) },
                ],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => id,
            Err(err) => {
                tracing::warn!("用量落库失败: {err}");
                String::new()
            }
        }
    }

    pub fn record_prompt(&self, r: &PromptRecord) -> String {
        let id = if r.id.is_empty() {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            r.id.clone()
        };
        let mut record = r.clone();
        record.id = id.clone();
        if self.record_prompts_batch(std::slice::from_ref(&record)) == 1 {
            id
        } else {
            String::new()
        }
    }

    /// 批量写入提示词记录：一次事务、一次写锁，供后台 worker 聚合写入。
    ///
    /// 为什么批量：单条写入会让后台每来一个请求就争抢一次 SQLite 写锁；
    /// 聚合成一批后，代理路径的用量写入最多等一个短事务，而不是等 N 次提交。
    pub fn record_prompts_batch(&self, records: &[PromptRecord]) -> usize {
        if records.is_empty() {
            return 0;
        }
        let now = Local::now();
        let mut conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO prompt_records (
                        id, ts, created_at, session, requested_model,
                        message_count, size_bytes, truncated, preview, messages
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                )?;
                for r in records {
                    let id = if r.id.is_empty() {
                        uuid::Uuid::new_v4().simple().to_string()
                    } else {
                        r.id.clone()
                    };
                    let ts = if r.ts > 0 { r.ts } else { now.timestamp() };
                    stmt.execute(rusqlite::params![
                        id.as_str(),
                        ts,
                        now.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
                        if r.session.is_empty() {
                            None
                        } else {
                            Some(r.session.clone())
                        },
                        r.requested_model.as_str(),
                        r.message_count,
                        r.size_bytes,
                        r.truncated as i64,
                        r.preview.as_str(),
                        r.messages.as_str(),
                    ])?;
                }
            }
            tx.commit()
        })();
        match result {
            Ok(()) => records.len(),
            Err(err) => {
                tracing::warn!("提示词批量落库失败: {err}");
                0
            }
        }
    }

    pub fn list_prompts(
        &self,
        f: &PromptFilters,
        limit: i64,
        offset: i64,
        sort: &str,
        order: &str,
    ) -> Vec<Value> {
        let (where_sql, args) = prompt_where_clause(f);
        let column = match sort {
            "size_bytes" => "p.size_bytes",
            "message_count" => "p.message_count",
            _ => "p.ts",
        };
        let direction = if order.eq_ignore_ascii_case("asc") {
            "ASC"
        } else {
            "DESC"
        };
        // ts 只有秒级精度；同一秒的多条记录用 rowid 兜底，保证时间轴与翻页稳定。
        let tie = if column == "p.ts" {
            format!(", p.rowid {direction}")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT p.id, p.ts, p.created_at, p.session, p.requested_model, \
             u.upstream, u.protocol, u.status, p.message_count, p.size_bytes, \
             p.truncated, p.preview \
             FROM prompt_records p LEFT JOIN usage_records u ON u.id = p.id{where_sql} \
             ORDER BY {column} {direction}{tie} LIMIT ? OFFSET ?"
        );
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::warn!("提示词列表查询失败: {err}");
                return Vec::new();
            }
        };
        let mut query_args = args;
        query_args.push(SqlValue::Integer(limit.clamp(1, 1000)));
        query_args.push(SqlValue::Integer(offset.max(0)));
        stmt.query_map(params_from_iter(query_args.iter()), prompt_row_json)
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn count_prompts(&self, f: &PromptFilters) -> i64 {
        let (where_sql, args) = prompt_where_clause(f);
        let conn = self.reader();
        conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM prompt_records p \
                 LEFT JOIN usage_records u ON u.id = p.id{where_sql}"
            ),
            params_from_iter(args.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    pub fn get_prompt(&self, id: &str) -> Option<Value> {
        let conn = self.reader();
        let sql = "SELECT p.*, u.upstream, u.protocol, u.status \
                   FROM prompt_records p LEFT JOIN usage_records u ON u.id = p.id \
                   WHERE p.id = ?1";
        let mut stmt = conn.prepare(sql).ok()?;
        let mut rows = stmt.query([id]).ok()?;
        let row = match rows.next() {
            Ok(Some(row)) => row,
            _ => return None,
        };
        let mut value = prompt_row_json(&row).ok()?;
        let raw: String = row.get("messages").ok()?;
        value["messages"] = serde_json::from_str(&raw).unwrap_or_else(|_| Value::Array(Vec::new()));
        Some(value)
    }

    pub fn export_prompts(&self, f: &PromptFilters, limit: i64) -> Vec<Value> {
        let (where_sql, args) = prompt_where_clause(f);
        let sql = format!(
            "SELECT p.*, u.upstream, u.protocol, u.status \
             FROM prompt_records p LEFT JOIN usage_records u ON u.id = p.id{where_sql} \
             ORDER BY p.ts ASC, p.rowid ASC LIMIT ?"
        );
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::warn!("提示词导出查询失败: {err}");
                return Vec::new();
            }
        };
        let mut query_args = args;
        query_args.push(SqlValue::Integer(limit.clamp(1, 100_000)));
        stmt.query_map(params_from_iter(query_args.iter()), |row| {
            let mut value = prompt_row_json(row)?;
            let raw: String = row.get("messages")?;
            value["messages"] =
                serde_json::from_str(&raw).unwrap_or_else(|_| Value::Array(Vec::new()));
            Ok(value)
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn clear_prompts(&self, f: &PromptFilters) -> i64 {
        let (where_sql, args) = prompt_where_clause(f);
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            &format!(
                "DELETE FROM prompt_records WHERE id IN (\
                 SELECT p.id FROM prompt_records p \
                 LEFT JOIN usage_records u ON u.id = p.id{where_sql})"
            ),
            params_from_iter(args.iter()),
        )
        .map(|n| n as i64)
        .unwrap_or(0)
    }

    pub fn delete_prompts_older_than(&self, days: i64) -> i64 {
        if days <= 0 {
            return 0;
        }
        let cutoff = Local::now().timestamp() - days * 86400;
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM prompt_records WHERE ts < ?1", [cutoff])
            .map(|n| n as i64)
            .unwrap_or(0)
    }

    pub fn trim_prompts(&self, max_records: i64) -> i64 {
        if max_records <= 0 {
            return 0;
        }
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM prompt_records WHERE id IN (\
             SELECT id FROM prompt_records ORDER BY ts DESC LIMIT -1 OFFSET ?1)",
            [max_records],
        )
        .map(|n| n as i64)
        .unwrap_or(0)
    }

    pub fn prompt_count_and_size(&self) -> Value {
        let conn = self.reader();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM prompt_records",
            [],
            |r| {
                Ok(json!({
                    "records": r.get::<_, i64>(0)?,
                    "bytes": r.get::<_, i64>(1)?,
                }))
            },
        )
        .unwrap_or_else(|_| json!({"records": 0, "bytes": 0}))
    }

    /// 提示词页的筛选项：只取非空值，避免把 `(无会话)` 当作可选会话。
    pub fn prompt_facets(&self) -> Value {
        let conn = self.reader();
        let mut models = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT requested_model FROM prompt_records \
             WHERE requested_model IS NOT NULL AND requested_model <> '' ORDER BY 1 LIMIT 200",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                models = rows.flatten().collect();
            }
        }
        let mut sessions = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT session FROM prompt_records \
             WHERE session IS NOT NULL AND session <> '' ORDER BY 1 LIMIT 200",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                sessions = rows.flatten().collect();
            }
        }
        json!({"models": models, "sessions": sessions})
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.reader();
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        if let Ok(conn) = self.write.lock() {
            let _ = conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            );
        }
    }

    pub fn count_usage(&self, f: &Filters) -> i64 {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let conn = self.reader();
        conn.query_row(
            &format!("SELECT COUNT(*) AS n FROM usage_records{where_sql}"),
            params_from_iter(args.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    pub fn list_usage(
        &self,
        f: &Filters,
        limit: i64,
        offset: i64,
        sort: &str,
        order: &str,
    ) -> Vec<Value> {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let column = match sort {
            "duration_ms" | "ttft_ms" | "prompt_tokens" | "completion_tokens"
            | "cache_read_tokens" | "total_tokens" | "cost" | "status" => sort,
            _ => "ts",
        };
        let direction = if order.eq_ignore_ascii_case("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let tie = if column == "ts" { "" } else { ", ts DESC" };
        let sql = format!(
            "SELECT * FROM usage_records{where_sql} ORDER BY {column} {direction}{tie} LIMIT ? OFFSET ?"
        );
        let mut all = args;
        all.push(SqlValue::Integer(limit));
        all.push(SqlValue::Integer(offset));
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(params_from_iter(all.iter()), |row| {
            let mut map = Map::new();
            for (i, name) in cols.iter().enumerate() {
                map.insert(name.clone(), sql_to_json(row, i));
            }
            Ok(Value::Object(map))
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn totals(&self, f: &Filters) -> Value {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let sql = format!(
            "WITH ranked AS (
                SELECT duration_ms, status, ttft_ms, prompt_tokens, completion_tokens,
                       total_tokens, cache_read_tokens, cache_write_tokens, cost,
                       ROW_NUMBER() OVER (ORDER BY duration_ms) - 1 AS _rn,
                       COUNT(*) OVER () AS _n
                FROM usage_records{where_sql}
             )
             SELECT COUNT(*) AS requests,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens,
                    COALESCE(SUM(completion_tokens),0) AS completion_tokens,
                    COALESCE(SUM(total_tokens),0) AS total_tokens,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(cache_write_tokens),0) AS cache_write_tokens,
                    COALESCE(SUM(cost),0) AS cost,
                    COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END),0) AS errors,
                    COALESCE(SUM(CASE WHEN status IS NULL OR status < 400 THEN 1 ELSE 0 END),0) AS ok,
                    COALESCE(AVG(duration_ms),0) AS avg_duration_ms,
                    COALESCE(AVG(NULLIF(ttft_ms,0)),0) AS avg_ttft_ms,
                    COALESCE(SUM(CASE WHEN ttft_ms > 0 THEN 1 ELSE 0 END),0) AS ttft_samples,
                    MAX(_n) AS samples, {} 
             FROM ranked",
            percentile_cols()
        );
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return json!({}),
        };
        let row = stmt.query_row(params_from_iter(args.iter()), |row| {
            let requests: i64 = row.get("requests")?;
            let ok: i64 = row.get("ok")?;
            let prompt: i64 = row.get("prompt_tokens")?;
            let cache_read: i64 = row.get("cache_read_tokens")?;
            let samples: i64 = row.get("samples").unwrap_or(0);
            let p50_lo: Option<i64> = row.get("p50_lo").unwrap_or(None);
            let p50_hi: Option<i64> = row.get("p50_hi").unwrap_or(None);
            let p95_lo: Option<i64> = row.get("p95_lo").unwrap_or(None);
            let p95_hi: Option<i64> = row.get("p95_hi").unwrap_or(None);
            let max_d: Option<i64> = row.get("max_duration_ms").unwrap_or(None);
            Ok(json!({
                "requests": requests,
                "prompt_tokens": prompt,
                "completion_tokens": row.get::<_, i64>("completion_tokens")?,
                "total_tokens": row.get::<_, i64>("total_tokens")?,
                "cache_read_tokens": cache_read,
                "cache_write_tokens": row.get::<_, i64>("cache_write_tokens")?,
                "cost": round6(row.get::<_, f64>("cost")?),
                "errors": row.get::<_, i64>("errors")?,
                "ok": ok,
                "success_rate": if requests > 0 { round4(ok as f64 / requests as f64) } else { 0.0 },
                "cache_hit_rate": hit_rate(cache_read, prompt),
                "avg_duration_ms": round1(row.get::<_, f64>("avg_duration_ms")?),
                "avg_ttft_ms": round1(row.get::<_, f64>("avg_ttft_ms")?),
                "ttft_samples": row.get::<_, i64>("ttft_samples")?,
                "p50_duration_ms": percentile_from_edges(samples, p50_lo, p50_hi, 0.50),
                "p95_duration_ms": percentile_from_edges(samples, p95_lo, p95_hi, 0.95),
                "max_duration_ms": max_d.unwrap_or(0) as f64,
            }))
        });
        row.unwrap_or_else(|_| {
            json!({
                "requests": 0, "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
                "cache_read_tokens": 0, "cache_write_tokens": 0, "cost": 0.0, "errors": 0,
                "ok": 0, "success_rate": 0.0, "cache_hit_rate": Value::Null,
                "avg_duration_ms": 0.0, "avg_ttft_ms": 0.0, "ttft_samples": 0,
                "p50_duration_ms": 0.0, "p95_duration_ms": 0.0, "max_duration_ms": 0.0
            })
        })
    }

    /// 今日（本地日期）请求数与缓存命中率，供托盘轻量刷新。
    pub fn today_stats(&self) -> Value {
        let conn = self.reader();
        let row = conn.query_row(
            "SELECT COUNT(*) AS requests,
                    COALESCE(SUM(CASE WHEN status IS NULL OR status < 400 THEN 1 ELSE 0 END),0) AS ok,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens
             FROM usage_records
             WHERE substr(created_at,1,10) = date('now','localtime')",
            [],
            |row| {
                let requests: i64 = row.get(0)?;
                let ok: i64 = row.get(1)?;
                let cache_read: i64 = row.get(2)?;
                let prompt: i64 = row.get(3)?;
                Ok(json!({
                    "requests": requests,
                    "ok": ok,
                    "success_rate": if requests > 0 { round4(ok as f64 / requests as f64) } else { 0.0 },
                    "cache_read_tokens": cache_read,
                    "prompt_tokens": prompt,
                    "cache_hit_rate": hit_rate(cache_read, prompt),
                }))
            },
        );
        row.unwrap_or_else(|_| {
            json!({
                "requests": 0, "ok": 0, "success_rate": 0.0,
                "cache_read_tokens": 0, "prompt_tokens": 0, "cache_hit_rate": Value::Null
            })
        })
    }

    pub fn summary(&self, f: &Filters, group_by: &str) -> Result<Vec<Value>, String> {
        let bucket = match group_by {
            "day" => "substr(created_at,1,10)",
            "hour" => "substr(created_at,1,13)",
            "model" => "COALESCE(requested_model,'(unknown)')",
            "upstream" => "COALESCE(upstream,'(unknown)')",
            "session" => "COALESCE(NULLIF(session,''),'(无会话)')",
            other => return Err(format!("不支持的分组维度: {other}")),
        };
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let sql = format!(
            "WITH ranked AS (
                SELECT *, {bucket} AS bucket,
                       ROW_NUMBER() OVER (PARTITION BY {bucket} ORDER BY duration_ms) - 1 AS _rn,
                       COUNT(*) OVER (PARTITION BY {bucket}) AS _n
                FROM usage_records{where_sql}
             )
             SELECT bucket, COUNT(*) AS requests,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens,
                    COALESCE(SUM(completion_tokens),0) AS completion_tokens,
                    COALESCE(SUM(total_tokens),0) AS total_tokens,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(cost),0) AS cost,
                    COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END),0) AS errors,
                    COALESCE(AVG(duration_ms),0) AS avg_duration_ms,
                    COALESCE(AVG(NULLIF(ttft_ms,0)),0) AS avg_ttft_ms,
                    MAX(_n) AS samples, {}
             FROM ranked GROUP BY bucket ORDER BY bucket",
            percentile_cols()
        );
        let conn = self.reader();
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt
            .query_map(params_from_iter(args.iter()), |row| {
                let mut map = Map::new();
                for (i, name) in cols.iter().enumerate() {
                    map.insert(name.clone(), sql_to_json(row, i));
                }
                Ok(Value::Object(map))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            let mut value = finish_group_row(row);
            // Python 版的 summary 行不含 success_rate（model_usage/upstream_usage 才有）。
            if let Some(obj) = value.as_object_mut() {
                obj.remove("success_rate");
            }
            out.push(value);
        }
        Ok(out)
    }

    pub fn model_usage(&self, f: &Filters, limit: i64) -> Vec<Value> {
        self.grouped_usage(f, limit, "model")
    }

    pub fn upstream_usage(&self, f: &Filters, limit: i64) -> Vec<Value> {
        self.grouped_usage(f, limit, "upstream")
    }

    /// 按会话聚合；排序只接受白名单，其余回退到最近活跃时间。
    pub fn session_usage(&self, f: &Filters, limit: i64, sort: &str, order: &str) -> Vec<Value> {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let expr = "COALESCE(NULLIF(session,''),'(无会话)')";
        let column = match sort {
            "requests" => "requests",
            "total_tokens" => "total_tokens",
            "errors" => "errors",
            "avg_duration_ms" => "avg_duration_ms",
            _ => "last_ts",
        };
        let direction = if order.eq_ignore_ascii_case("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let sql = format!(
            "WITH ranked AS (
                SELECT *, {expr} AS grp,
                       ROW_NUMBER() OVER (PARTITION BY {expr} ORDER BY duration_ms) - 1 AS _rn,
                       COUNT(*) OVER (PARTITION BY {expr}) AS _n
                FROM usage_records{where_sql}
             )
             SELECT grp AS session,
                    COUNT(*) AS requests,
                    COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END),0) AS errors,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens,
                    COALESCE(SUM(completion_tokens),0) AS completion_tokens,
                    COALESCE(SUM(total_tokens),0) AS total_tokens,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(cache_write_tokens),0) AS cache_write_tokens,
                    COALESCE(SUM(cost),0) AS cost,
                    COALESCE(AVG(duration_ms),0) AS avg_duration_ms,
                    COALESCE(AVG(NULLIF(ttft_ms,0)),0) AS avg_ttft_ms,
                    COALESCE(MIN(ts),0) AS first_ts,
                    COALESCE(MAX(ts),0) AS last_ts,
                    COUNT(DISTINCT COALESCE(requested_model,'(unknown)')) AS models_count,
                    COALESCE(GROUP_CONCAT(DISTINCT requested_model),'') AS models,
                    MAX(_n) AS samples, {}
             FROM ranked GROUP BY grp ORDER BY {column} {direction}, grp ASC LIMIT ?",
            percentile_cols()
        );
        let mut all = args;
        all.push(SqlValue::Integer(limit.clamp(1, 1000)));
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::warn!("会话聚合查询失败: {err}");
                return Vec::new();
            }
        };
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(params_from_iter(all.iter()), |row| {
            let mut map = Map::new();
            for (i, name) in cols.iter().enumerate() {
                map.insert(name.clone(), sql_to_json(row, i));
            }
            Ok(Value::Object(map))
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).map(finish_group_row).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn grouped_usage(&self, f: &Filters, limit: i64, kind: &str) -> Vec<Value> {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let (expr, alias) = if kind == "model" {
            ("COALESCE(requested_model,'(unknown)')", "model")
        } else {
            ("COALESCE(upstream,'(unknown)')", "upstream")
        };
        let extra = if kind == "upstream" {
            format!("COUNT(DISTINCT COALESCE(requested_model,'(unknown)')) AS models_count,")
        } else {
            String::new()
        };
        let sql = format!(
            "WITH ranked AS (
                SELECT *, {expr} AS grp,
                       ROW_NUMBER() OVER (PARTITION BY {expr} ORDER BY duration_ms) - 1 AS _rn,
                       COUNT(*) OVER (PARTITION BY {expr}) AS _n
                FROM usage_records{where_sql}
             )
             SELECT grp AS {alias}, {extra} COUNT(*) AS requests,
                    COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END),0) AS errors,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens,
                    COALESCE(SUM(completion_tokens),0) AS completion_tokens,
                    COALESCE(SUM(total_tokens),0) AS total_tokens,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(cache_write_tokens),0) AS cache_write_tokens,
                    COALESCE(SUM(cost),0) AS cost,
                    COALESCE(AVG(duration_ms),0) AS avg_duration_ms,
                    COALESCE(AVG(NULLIF(ttft_ms,0)),0) AS avg_ttft_ms,
                    MAX(ts) AS last_ts, MAX(_n) AS samples, {}
             FROM ranked GROUP BY grp ORDER BY total_tokens DESC LIMIT ?",
            percentile_cols()
        );
        let mut all = args;
        all.push(SqlValue::Integer(limit));
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(params_from_iter(all.iter()), |row| {
            let mut map = Map::new();
            for (i, name) in cols.iter().enumerate() {
                map.insert(name.clone(), sql_to_json(row, i));
            }
            Ok(Value::Object(map))
        });
        match rows {
            Ok(iter) => iter.flatten().map(finish_group_row).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn usage_count_and_size(&self) -> Value {
        let records = self.count_usage(&Filters::default());
        let bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        json!({"records": records, "bytes": bytes})
    }

    pub fn clear_usage(&self, f: &Filters) -> i64 {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            &format!("DELETE FROM usage_records{where_sql}"),
            params_from_iter(args.iter()),
        )
        .map(|n| n as i64)
        .unwrap_or(0)
    }

    pub fn delete_older_than(&self, days: i64) -> i64 {
        if days <= 0 {
            return 0;
        }
        let cutoff = chrono::Utc::now().timestamp() - days * 86400;
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM usage_records WHERE ts < ?1", [cutoff])
            .map(|n| n as i64)
            .unwrap_or(0)
    }

    pub fn vacuum(&self) -> i64 {
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = conn.execute("VACUUM", []);
        std::fs::metadata(&self.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0)
    }
}

static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn connect(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    Ok(conn)
}

fn migrate(conn: &Connection) {
    for (table, def) in MIGRATIONS {
        let column = def.split_whitespace().next().unwrap_or("");
        let exists = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .and_then(|mut s| {
                let names: Vec<String> = s
                    .query_map([], |r| r.get::<_, String>(1))?
                    .flatten()
                    .collect();
                Ok(names.iter().any(|n| n == column))
            })
            .unwrap_or(true);
        if !exists {
            let _ = conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {def}"), []);
        }
    }
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_records(session)",
        [],
    );
}

/// LIKE 模式转义：用户输入里的 `%` / `_` / `\` 按字面量匹配。
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn prompt_where_clause(f: &PromptFilters) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut args = Vec::new();
    if let Some(v) = f.since_ts {
        clauses.push("p.ts >= ?");
        args.push(SqlValue::Integer(v));
    }
    if let Some(v) = f.until_ts {
        clauses.push("p.ts <= ?");
        args.push(SqlValue::Integer(v));
    }
    if let Some(v) = &f.model {
        clauses.push("p.requested_model = ?");
        args.push(SqlValue::Text(v.clone()));
    }
    if let Some(v) = &f.session {
        if v == "(无会话)" {
            clauses.push("(p.session IS NULL OR p.session = '')");
        } else {
            clauses.push("p.session = ?");
            args.push(SqlValue::Text(v.clone()));
        }
    }
    if let Some(v) = &f.upstream {
        clauses.push("p.id IN (SELECT id FROM usage_records WHERE upstream = ?)");
        args.push(SqlValue::Text(v.clone()));
    }
    if let Some(v) = f.q.as_deref().filter(|v| !v.is_empty()) {
        clauses.push("p.messages LIKE ? ESCAPE '\\'");
        args.push(SqlValue::Text(format!("%{}%", escape_like(v))));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}

fn prompt_row_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let truncated: i64 = row.get("truncated")?;
    Ok(json!({
        "id": row.get::<_, String>("id")?,
        "ts": row.get::<_, i64>("ts")?,
        "created_at": row.get::<_, String>("created_at")?,
        "session": row.get::<_, Option<String>>("session")?,
        "requested_model": row.get::<_, Option<String>>("requested_model")?,
        "upstream": row.get::<_, Option<String>>("upstream")?,
        "protocol": row.get::<_, Option<String>>("protocol")?,
        "status": row.get::<_, Option<i64>>("status")?,
        "message_count": row.get::<_, i64>("message_count")?,
        "size_bytes": row.get::<_, i64>("size_bytes")?,
        "truncated": truncated != 0,
        "preview": row.get::<_, Option<String>>("preview")?,
    }))
}

pub fn where_clause(f: &Filters, only: Option<&str>) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut args = Vec::new();
    if let Some(v) = f.since_ts {
        clauses.push("ts >= ?");
        args.push(SqlValue::Integer(v));
    }
    if let Some(v) = f.until_ts {
        clauses.push("ts <= ?");
        args.push(SqlValue::Integer(v));
    }
    if let Some(v) = &f.model {
        clauses.push("requested_model = ?");
        args.push(SqlValue::Text(v.clone()));
    }
    if let Some(v) = &f.upstream {
        clauses.push("upstream = ?");
        args.push(SqlValue::Text(v.clone()));
    }
    if let Some(v) = &f.session {
        if v == "(无会话)" {
            clauses.push("(session IS NULL OR session = '')");
        } else {
            clauses.push("session = ?");
            args.push(SqlValue::Text(v.clone()));
        }
    }
    if let Some(sql) = only {
        let allowed = match sql {
            "error" => Some("status >= 400"),
            "slow" => Some("duration_ms >= 10000"),
            "truncated" => Some("truncated = 1"),
            "paused" => Some("paused = 1"),
            "cache" => Some("cache_read_tokens > 0"),
            "ttft" => Some("ttft_ms > 0"),
            _ => None,
        };
        if let Some(clause) = allowed {
            clauses.push(clause);
        }
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}

fn percentile_cols() -> String {
    let pair = |ratio: &str, alias: &str| {
        format!(
            "MAX(CASE WHEN _rn = CAST((_n - 1) * {ratio} AS INTEGER) THEN duration_ms END) AS {alias}_lo, \
             MAX(CASE WHEN _rn = MIN(CAST((_n - 1) * {ratio} AS INTEGER) + 1, _n - 1) THEN duration_ms END) AS {alias}_hi"
        )
    };
    format!(
        "{}, {}, MAX(duration_ms) AS max_duration_ms",
        pair("0.50", "p50"),
        pair("0.95", "p95")
    )
}

pub fn percentile_from_edges(n: i64, lo: Option<i64>, hi: Option<i64>, ratio: f64) -> f64 {
    if n <= 0 {
        return 0.0;
    }
    let low = lo.unwrap_or(0) as f64;
    if n == 1 {
        return low;
    }
    let pos = ratio * (n - 1) as f64;
    let idx = pos.floor();
    let weight = pos - idx;
    let high = hi.unwrap_or(low as i64) as f64;
    round1(low * (1.0 - weight) + high * weight)
}

fn sql_to_json(row: &rusqlite::Row<'_>, i: usize) -> Value {
    use rusqlite::types::Type;
    match row.get_ref(i) {
        Ok(v) => match v {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            rusqlite::types::ValueRef::Real(f) => json!(f),
            rusqlite::types::ValueRef::Text(t) => {
                json!(String::from_utf8_lossy(t).to_string())
            }
            rusqlite::types::ValueRef::Blob(_) => Value::Null,
        },
        Err(_) => Value::Null,
    }
}

fn finish_group_row(mut row: Value) -> Value {
    let obj = row.as_object_mut().expect("row is object");
    let samples = obj.get("samples").and_then(Value::as_i64).unwrap_or(0);
    let p50_lo = obj.get("p50_lo").and_then(Value::as_i64);
    let p50_hi = obj.get("p50_hi").and_then(Value::as_i64);
    let p95_lo = obj.get("p95_lo").and_then(Value::as_i64);
    let p95_hi = obj.get("p95_hi").and_then(Value::as_i64);
    for key in [
        "samples",
        "p50_lo",
        "p50_hi",
        "p95_lo",
        "p95_hi",
        "max_duration_ms",
    ] {
        obj.remove(key);
    }
    let requests = obj.get("requests").and_then(Value::as_i64).unwrap_or(0);
    let errors = obj.get("errors").and_then(Value::as_i64).unwrap_or(0);
    let prompt = obj
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_read = obj
        .get("cache_read_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    obj.insert(
        "success_rate".into(),
        json!(if requests > 0 {
            round4((requests - errors) as f64 / requests as f64)
        } else {
            0.0
        }),
    );
    obj.insert(
        "cache_hit_rate".into(),
        match hit_rate(cache_read, prompt) {
            Some(v) => json!(v),
            None => Value::Null,
        },
    );
    obj.insert(
        "cost".into(),
        json!(round6(
            obj.get("cost").and_then(Value::as_f64).unwrap_or(0.0)
        )),
    );
    obj.insert(
        "avg_duration_ms".into(),
        json!(round1(
            obj.get("avg_duration_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        )),
    );
    obj.insert(
        "avg_ttft_ms".into(),
        json!(round1(
            obj.get("avg_ttft_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        )),
    );
    obj.insert(
        "p50_duration_ms".into(),
        json!(percentile_from_edges(samples, p50_lo, p50_hi, 0.50)),
    );
    obj.insert(
        "p95_duration_ms".into(),
        json!(percentile_from_edges(samples, p95_lo, p95_hi, 0.95)),
    );
    row
}

fn hit_rate(cache_read: i64, prompt: i64) -> Option<f64> {
    if prompt <= 0 {
        None
    } else {
        Some(round6(cache_read as f64 / prompt as f64))
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}
fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!("mg-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("modelgate.db")).unwrap()
    }

    #[test]
    fn today_stats_counts_requests_and_cache_hit_rate() {
        let db = temp_db();
        db.record_usage(&UsageRecord {
            upstream: "deepseek".into(),
            requested_model: "m".into(),
            upstream_model: "m".into(),
            prompt_tokens: 100,
            completion_tokens: 10,
            cache_read_tokens: 40,
            status: 200,
            ..Default::default()
        });
        db.record_usage(&UsageRecord {
            upstream: "deepseek".into(),
            requested_model: "m".into(),
            upstream_model: "m".into(),
            prompt_tokens: 100,
            completion_tokens: 10,
            cache_read_tokens: 60,
            status: 500,
            ..Default::default()
        });
        let stats = db.today_stats();
        assert_eq!(stats["requests"].as_i64(), Some(2));
        assert_eq!(stats["ok"].as_i64(), Some(1));
        assert_eq!(stats["cache_read_tokens"].as_i64(), Some(100));
        assert_eq!(stats["prompt_tokens"].as_i64(), Some(200));
        assert_eq!(stats["cache_hit_rate"].as_f64(), Some(0.5));
    }

    #[test]
    fn today_stats_hit_rate_is_null_without_prompt_tokens() {
        let db = temp_db();
        let stats = db.today_stats();
        assert_eq!(stats["requests"].as_i64(), Some(0));
        assert!(stats["cache_hit_rate"].is_null());
    }

    #[test]
    fn prompt_records_table_and_indexes_exist() {
        let db = temp_db();
        let conn = db.write.lock().unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'prompt_records'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1, "prompt_records 表必须由 SCHEMA 建立");
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name IN \
                 ('idx_prompt_ts', 'idx_prompt_session', 'idx_prompt_model')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 3, "prompt_records 的三个索引必须建立");
    }

    fn sample_prompt(id: &str, model: &str, session: &str, text: &str) -> PromptRecord {
        let messages = format!(r#"[{{"role":"user","content":"{text}"}}]"#);
        PromptRecord {
            id: id.into(),
            session: session.into(),
            requested_model: model.into(),
            message_count: 1,
            size_bytes: messages.len() as i64,
            preview: text.into(),
            messages,
            ..Default::default()
        }
    }

    #[test]
    fn prompt_record_roundtrip() {
        let db = temp_db();
        let id = db.record_prompt(&sample_prompt("req-1", "m1", "s1", "hello"));
        assert_eq!(id, "req-1");
        let got = db.get_prompt("req-1").expect("prompt 应可读取");
        assert_eq!(got["session"], "s1");
        assert_eq!(got["requested_model"], "m1");
        assert_eq!(got["message_count"], 1);
        assert_eq!(got["messages"][0]["content"], "hello");
        assert!(got["upstream"].is_null());
    }

    #[test]
    fn prompt_filters_trim_export_and_clear() {
        let db = temp_db();
        db.record_prompt(&sample_prompt("a", "m1", "s1", "alpha"));
        db.record_prompt(&sample_prompt("b", "m2", "s2", "beta"));
        db.record_prompt(&PromptRecord {
            id: "c".into(),
            requested_model: "m3".into(),
            messages: "[]".into(),
            ..Default::default()
        });

        let by_model = PromptFilters {
            model: Some("m1".into()),
            ..Default::default()
        };
        assert_eq!(db.count_prompts(&by_model), 1);
        assert_eq!(db.list_prompts(&by_model, 50, 0, "ts", "desc").len(), 1);

        let by_query = PromptFilters {
            q: Some("beta".into()),
            ..Default::default()
        };
        assert_eq!(db.count_prompts(&by_query), 1);

        let no_session = PromptFilters {
            session: Some("(无会话)".into()),
            ..Default::default()
        };
        assert_eq!(db.count_prompts(&no_session), 1);

        let exported = db.export_prompts(&PromptFilters::default(), 100);
        assert_eq!(exported.len(), 3);
        assert!(exported[0]["messages"].is_array());

        assert!(db.trim_prompts(2) >= 1);
        assert_eq!(db.count_prompts(&PromptFilters::default()), 2);
        assert_eq!(db.clear_prompts(&PromptFilters::default()), 2);
        assert_eq!(db.count_prompts(&PromptFilters::default()), 0);
    }

    #[test]
    fn prompt_delete_older_than_and_size_stats() {
        let db = temp_db();
        let mut old = sample_prompt("old", "m", "s", "old");
        old.ts = Local::now().timestamp() - 10 * 86400;
        db.record_prompt(&old);
        db.record_prompt(&sample_prompt("new", "m", "s", "new"));
        let stats = db.prompt_count_and_size();
        assert_eq!(stats["records"], 2);
        assert!(stats["bytes"].as_i64().unwrap_or(0) > 0);
        assert_eq!(db.delete_prompts_older_than(7), 1);
        assert_eq!(db.count_prompts(&PromptFilters::default()), 1);
    }

    #[test]
    fn prompt_write_latency_is_below_10ms() {
        let db = temp_db();
        let mut record = sample_prompt("perf-0", "m", "s", "latency probe");
        let start = std::time::Instant::now();
        for i in 0..20 {
            record.id = format!("perf-{i}");
            db.record_prompt(&record);
        }
        let avg_ms = start.elapsed().as_secs_f64() * 1000.0 / 20.0;
        println!("prompt write avg: {avg_ms:.3}ms");
        assert!(avg_ms < 10.0, "单次提示词写入平均 {avg_ms:.2}ms，超过 10ms");
    }

    #[test]
    fn usage_record_respects_given_id() {
        let db = temp_db();
        let id = db.record_usage(&UsageRecord {
            id: "usage-1".into(),
            upstream: "u".into(),
            status: 200,
            ..Default::default()
        });
        assert_eq!(id, "usage-1");
        let rows = db.list_usage(&Filters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows[0]["id"], "usage-1");
    }

    fn usage(session: &str, model: &str, prompt_tokens: i64) -> UsageRecord {
        UsageRecord {
            session: session.into(),
            upstream: "u".into(),
            requested_model: model.into(),
            prompt_tokens,
            completion_tokens: 1,
            status: 200,
            ..Default::default()
        }
    }

    #[test]
    fn session_usage_groups_and_filters_empty_session() {
        let db = temp_db();
        db.record_usage(&usage("s1", "m1", 10));
        db.record_usage(&usage("s1", "m2", 20));
        db.record_usage(&usage("s2", "m1", 30));
        db.record_usage(&usage("", "m3", 40));

        let rows = db.session_usage(&Filters::default(), 10, "last_ts", "desc");
        assert_eq!(rows.len(), 3);
        let s1 = rows.iter().find(|r| r["session"] == "s1").expect("s1 会话");
        assert_eq!(s1["requests"], 2);
        assert_eq!(s1["prompt_tokens"], 30);
        assert_eq!(s1["models_count"], 2);
        let none = rows
            .iter()
            .find(|r| r["session"] == "(无会话)")
            .expect("无会话分组");
        assert_eq!(none["requests"], 1);

        let by_requests = db.session_usage(&Filters::default(), 10, "requests", "desc");
        assert_eq!(by_requests[0]["requests"], 2);

        let empty = Filters {
            session: Some("(无会话)".into()),
            ..Default::default()
        };
        let filtered = db.session_usage(&empty, 10, "last_ts", "desc");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["session"], "(无会话)");
        assert_eq!(
            db.list_usage(&empty, 10, 0, "ts", "desc").len(),
            1,
            "明细的 (无会话) 过滤必须命中 NULL"
        );
    }
}
