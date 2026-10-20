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
        write.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;")?;
        write.execute_batch(SCHEMA)?;
        migrate(&write);
        let readers = (0..2)
            .map(|_| connect(path).map(Mutex::new))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { path: path.to_path_buf(), write: Mutex::new(write), readers })
    }

    fn reader(&self) -> std::sync::MutexGuard<'_, Connection> {
        // 轮转使用只读连接；两条连接足够覆盖面板的并发统计请求。
        let idx = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.readers.len();
        self.readers[idx].lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn record_usage(&self, r: &UsageRecord) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
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

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.reader();
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
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

    pub fn list_usage(&self, f: &Filters, limit: i64, offset: i64,
                      sort: &str, order: &str) -> Vec<Value> {
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        let column = match sort {
            "duration_ms" | "ttft_ms" | "prompt_tokens" | "completion_tokens"
            | "cache_read_tokens" | "total_tokens" | "cost" | "status" => sort,
            _ => "ts",
        };
        let direction = if order.eq_ignore_ascii_case("asc") { "ASC" } else { "DESC" };
        let tie = if column == "ts" { "" } else { ", ts DESC" };
        let sql = format!(
            "SELECT * FROM usage_records{where_sql} ORDER BY {column} {direction}{tie} LIMIT ? OFFSET ?"
        );
        let mut all = args;
        all.push(SqlValue::Integer(limit));
        all.push(SqlValue::Integer(offset));
        let conn = self.reader();
        let mut stmt = match conn.prepare(&sql) { Ok(s) => s, Err(_) => return Vec::new() };
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
        let mut stmt = match conn.prepare(&sql) { Ok(s) => s, Err(_) => return json!({}) };
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
        row.unwrap_or_else(|_| json!({
            "requests": 0, "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
            "cache_read_tokens": 0, "cache_write_tokens": 0, "cost": 0.0, "errors": 0,
            "ok": 0, "success_rate": 0.0, "cache_hit_rate": Value::Null,
            "avg_duration_ms": 0.0, "avg_ttft_ms": 0.0, "ttft_samples": 0,
            "p50_duration_ms": 0.0, "p95_duration_ms": 0.0, "max_duration_ms": 0.0
        }))
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
        let mut stmt = match conn.prepare(&sql) { Ok(s) => s, Err(_) => return Vec::new() };
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
        conn.execute(&format!("DELETE FROM usage_records{where_sql}"), params_from_iter(args.iter()))
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
        std::fs::metadata(&self.path).map(|m| m.len() as i64).unwrap_or(0)
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
        clauses.push("session = ?");
        args.push(SqlValue::Text(v.clone()));
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
    format!("{}, {}, MAX(duration_ms) AS max_duration_ms",
            pair("0.50", "p50"), pair("0.95", "p95"))
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
    for key in ["samples", "p50_lo", "p50_hi", "p95_lo", "p95_hi", "max_duration_ms"] {
        obj.remove(key);
    }
    let requests = obj.get("requests").and_then(Value::as_i64).unwrap_or(0);
    let errors = obj.get("errors").and_then(Value::as_i64).unwrap_or(0);
    let prompt = obj.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
    let cache_read = obj.get("cache_read_tokens").and_then(Value::as_i64).unwrap_or(0);
    obj.insert("success_rate".into(), json!(if requests > 0 {
        round4((requests - errors) as f64 / requests as f64)
    } else { 0.0 }));
    obj.insert("cache_hit_rate".into(), match hit_rate(cache_read, prompt) {
        Some(v) => json!(v),
        None => Value::Null,
    });
    obj.insert("cost".into(), json!(round6(obj.get("cost").and_then(Value::as_f64).unwrap_or(0.0))));
    obj.insert("avg_duration_ms".into(), json!(round1(obj.get("avg_duration_ms").and_then(Value::as_f64).unwrap_or(0.0))));
    obj.insert("avg_ttft_ms".into(), json!(round1(obj.get("avg_ttft_ms").and_then(Value::as_f64).unwrap_or(0.0))));
    obj.insert("p50_duration_ms".into(), json!(percentile_from_edges(samples, p50_lo, p50_hi, 0.50)));
    obj.insert("p95_duration_ms".into(), json!(percentile_from_edges(samples, p95_lo, p95_hi, 0.95)));
    row
}

fn hit_rate(cache_read: i64, prompt: i64) -> Option<f64> {
    if prompt <= 0 { None } else { Some(round6(cache_read as f64 / prompt as f64)) }
}

fn round1(v: f64) -> f64 { (v * 10.0).round() / 10.0 }
fn round4(v: f64) -> f64 { (v * 10_000.0).round() / 10_000.0 }
fn round6(v: f64) -> f64 { (v * 1_000_000.0).round() / 1_000_000.0 }
