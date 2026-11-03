//! SQLite 数据层：与 Python 版同库、同表、同 SQL 口径。
//!
//! 读写分离：一条写连接 + 一组只读连接（WAL）。百分位在 SQLite 侧用窗口函数
//! 取两个边界样本，避免把全表 `duration_ms` 搬进内存。
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
-- 复合索引对齐真实查询形状：面板高频过滤是「时间窗 + 上游/模型」组合，
-- 单列索引只能选其一再回表过滤；复合索引让组合过滤在索引内完成。
CREATE INDEX IF NOT EXISTS idx_usage_model_ts ON usage_records(requested_model, ts);
CREATE INDEX IF NOT EXISTS idx_usage_up_ts    ON usage_records(upstream, ts);
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
    messages        TEXT NOT NULL,
    redactions      INTEGER NOT NULL DEFAULT 0,
    truncation      TEXT,
    content_hash    TEXT,
    system_hash     TEXT,
    role_counts     TEXT,
    policy_version  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_prompt_ts      ON prompt_records(ts DESC);
CREATE INDEX IF NOT EXISTS idx_prompt_session ON prompt_records(session);
CREATE INDEX IF NOT EXISTS idx_prompt_model   ON prompt_records(requested_model);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
-- 凭据池：多账号/多 Key 的统一事实源。密钥本体仍在本地加密库，
-- credential_members.secret_ref 只存引用（store:...）。
CREATE TABLE IF NOT EXISTS credential_pools (
    id               TEXT PRIMARY KEY,
    upstream         TEXT NOT NULL,
    plugin_id        TEXT,
    strategy         TEXT NOT NULL DEFAULT 'priority',
    status           TEXT NOT NULL DEFAULT 'active',
    cooldown_seconds INTEGER NOT NULL DEFAULT 60,
    failure_threshold INTEGER NOT NULL DEFAULT 3,
    fallback_to_single INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_credential_pools_upstream ON credential_pools(upstream);
CREATE TABLE IF NOT EXISTS credential_members (
    id            TEXT PRIMARY KEY,
    pool_id       TEXT NOT NULL,
    label         TEXT NOT NULL,
    kind          TEXT NOT NULL,
    secret_ref    TEXT NOT NULL,
    priority      INTEGER NOT NULL DEFAULT 100,
    weight        INTEGER NOT NULL DEFAULT 1,
    status        TEXT NOT NULL DEFAULT 'active',
    cooldown_until TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    quota_json    TEXT,
    last_used_at  TEXT,
    usage_json    TEXT
);
CREATE INDEX IF NOT EXISTS idx_credential_members_pool ON credential_members(pool_id);
-- 已安装插件：安装只写本表，不执行 DDL、不直接写 bridge.config.json。
-- removed=1 仅用于内置插件「移除」：登记保留、入口隐藏，可在内置库重装。
CREATE TABLE IF NOT EXISTS plugins (
    id            TEXT PRIMARY KEY,
    version       TEXT NOT NULL,
    kind          TEXT NOT NULL,
    tier          TEXT NOT NULL,
    name          TEXT,
    manifest_json TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    removed       INTEGER NOT NULL DEFAULT 0,
    source        TEXT NOT NULL DEFAULT 'local',
    installed_at  TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
-- 配置审计：AI/面板/插件的每次 plan/apply 只记录脱敏路径，不含密钥。
CREATE TABLE IF NOT EXISTS config_audit (
    id            TEXT PRIMARY KEY,
    at            TEXT NOT NULL,
    actor         TEXT,
    action        TEXT,
    plan_id       TEXT,
    target        TEXT,
    changed_paths TEXT,
    result        TEXT,
    backup_id     TEXT
);
"#;

const MIGRATIONS: &[(&str, &str)] = &[
    ("usage_records", "cost REAL NOT NULL DEFAULT 0"),
    ("usage_records", "session TEXT"),
    ("usage_records", "ttft_ms INTEGER NOT NULL DEFAULT 0"),
    ("prompt_records", "redactions INTEGER NOT NULL DEFAULT 0"),
    ("prompt_records", "truncation TEXT"),
    ("prompt_records", "content_hash TEXT"),
    ("prompt_records", "system_hash TEXT"),
    ("prompt_records", "role_counts TEXT"),
    (
        "prompt_records",
        "policy_version INTEGER NOT NULL DEFAULT 0",
    ),
    ("plugins", "removed INTEGER NOT NULL DEFAULT 0"),
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
    /// 写入前脱敏命中的次数；只记录计数，不记录命中的原文。
    pub redactions: i64,
    /// 截断元数据 JSON（策略、丢弃数量、头尾边界、被截断消息索引）。
    pub truncation: Option<String>,
    /// 脱敏后完整 messages 的 SHA-256 前 16 位。
    pub content_hash: String,
    /// system 消息的稳定摘要。
    pub system_hash: String,
    /// 角色分布 JSON。
    pub role_counts: String,
    /// 脱敏规则版本。
    pub policy_version: i64,
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

/// 重算历史费用所需的最小字段快照（`recompute_costs` 的输入）。
pub struct CostRow {
    pub id: String,
    pub ts: i64,
    pub upstream: String,
    pub requested_model: String,
    pub upstream_model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

pub struct Db {
    pub path: PathBuf,
    write: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    /// 提示词全表统计的共享缓存：跨线程后台重算时通过 Arc 克隆，
    /// 缓存本体不随 Db 移动。
    prompt_stats: Arc<PromptStats>,
    /// 聚合查询短 TTL 缓存（面板监控口径）。
    agg_cache: Arc<AggregateCache>,
}

/// 全表聚合（prompt_count_and_size）的短 TTL 缓存；只用于面板提示行，
/// 宁可短暂陈旧也不在每次列表刷新时全表扫描。过期后立即返回旧值并
/// 在后台线程重算（stale-while-revalidate），请求路径永不等待全表扫描。
#[derive(Default)]
struct PromptStats {
    cache: Mutex<Option<(std::time::Instant, Value)>>,
    /// 后台重算去重标记：同一时刻只允许一个线程在做全表统计。
    refreshing: std::sync::atomic::AtomicBool,
}

/// 面板聚合查询（totals/summary/model/upstream/session 用量）的短 TTL 缓存。
/// 这些查询在百万行级用量表上是一次全范围扫描；筛选切换、总览重载、
/// 自动刷新在 TTL 内直接命中缓存。属监控口径，10 秒陈旧可接受；
/// 清理/删除操作会显式失效。
const AGG_TTL: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Default)]
struct AggregateCache {
    entries: Mutex<HashMap<String, (std::time::Instant, Value)>>,
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
        let readers = (0..READER_POOL_SIZE)
            .map(|_| connect(path).map(Mutex::new))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let db = Self {
            path: path.to_path_buf(),
            write: Mutex::new(write),
            readers,
            prompt_stats: Arc::new(PromptStats::default()),
            agg_cache: Arc::new(AggregateCache::default()),
        };
        // 查询计划治理：启动时按需刷新统计，避免大库上查询计划长期劣化。
        db.optimize();
        Ok(db)
    }

    fn reader(&self) -> std::sync::MutexGuard<'_, Connection> {
        // 轮转使用只读连接；面板一次并发拉 6+ 个统计接口，
        // 连接数要覆盖并发度，否则请求会在读锁上排队、整页被最慢的查询拖住。
        let idx = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.readers.len();
        self.readers[idx].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 凭据池写连接（同库同事务域，避免再开一条 SQLite 连接）。
    pub(crate) fn write_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.write.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 凭据池读取连接（只读，轮转）。
    pub(crate) fn read_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader()
    }

    /// 聚合缓存命中取值：TTL 内直接返回，过期条目顺手清掉。
    fn agg_cache_get(&self, key: &str) -> Option<Value> {
        let mut entries = self
            .agg_cache
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((at, value)) = entries.get(key) {
            if at.elapsed() < AGG_TTL {
                return Some(value.clone());
            }
        }
        entries.retain(|_, (at, _)| at.elapsed() < AGG_TTL);
        None
    }

    fn agg_cache_put(&self, key: &str, value: Value) {
        let mut entries = self
            .agg_cache
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        entries.insert(key.to_string(), (std::time::Instant::now(), value));
    }

    /// 清理/删除类操作后失效聚合缓存：用户清空数据后面板必须立刻反映。
    fn invalidate_agg_cache(&self) {
        let mut entries = self
            .agg_cache
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        entries.clear();
    }

    /// 聚合缓存的筛选键：把过滤参数拼成稳定字符串。
    /// 时间边界按 5 分钟量化——面板的 since_ts 形如 `now-30d`，每秒都在变，
    /// 不量化的话键永远不重复、缓存永远不命中。实际查询仍用原始边界。
    fn agg_key(kind: &str, f: &Filters, extra: &str) -> String {
        const BUCKET: i64 = 300;
        let q = |v: Option<i64>| v.map(|t| t / BUCKET * BUCKET);
        format!(
            "{kind}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{extra}",
            q(f.since_ts),
            q(f.until_ts),
            f.upstream,
            f.model,
            f.session,
            f.only
        )
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
        let mut record = r.clone();
        if record.id.is_empty() {
            record.id = uuid::Uuid::new_v4().simple().to_string();
        }
        let id = record.id.clone();
        if self.record_usage_batch(std::slice::from_ref(&record)) == 1 {
            id
        } else {
            String::new()
        }
    }

    /// 批量写入用量记录：一次事务、一次写锁，供后台 worker 聚合写入。
    ///
    /// 为什么批量：代理路径每个请求都会产生一条用量，逐条写入会让热路径
    /// 每次都争抢全局写连接；聚合成一批后，并发请求共享一次事务提交。
    /// `INSERT OR REPLACE` 保持与单条写入相同的幂等语义（请求 ID 关联）。
    pub fn record_usage_batch(&self, records: &[UsageRecord]) -> usize {
        if records.is_empty() {
            return 0;
        }
        let now = Local::now();
        let created_at = now.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string();
        let ts = now.timestamp();
        let mut conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO usage_records (
                        id, ts, created_at, upstream, protocol, requested_model,
                        upstream_model, prompt_tokens, completion_tokens,
                        cache_read_tokens, cache_write_tokens, total_tokens,
                        duration_ms, ttft_ms, status, stream, truncated, paused, error,
                        cost, session
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                )?;
                for r in records {
                    let id = if r.id.is_empty() {
                        uuid::Uuid::new_v4().simple().to_string()
                    } else {
                        r.id.clone()
                    };
                    stmt.execute(rusqlite::params![
                        id,
                        ts,
                        created_at,
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
                        if r.session.is_empty() {
                            None
                        } else {
                            Some(r.session.clone())
                        },
                    ])?;
                }
            }
            tx.commit()
        })();
        match result {
            Ok(()) => records.len(),
            Err(err) => {
                tracing::warn!("用量批量落库失败: {err}");
                0
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
                        message_count, size_bytes, truncated, preview, messages,
                        redactions, truncation, content_hash, system_hash, role_counts, policy_version
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
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
                        r.redactions,
                        r.truncation.as_deref(),
                        r.content_hash.as_str(),
                        r.system_hash.as_str(),
                        r.role_counts.as_str(),
                        r.policy_version,
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

    /// 列表/导出共用的提示词列。刻意不含 `messages`：那是单条可达数百 KB 的
    /// 大字段，列表页只用 preview 与元数据；把整页的 messages 拖出来是
    /// 大库（GB 级）下列表查询慢的主因。消息正文只走 get_prompt / export。
    const PROMPT_LIST_COLUMNS: &str = "p.id, p.ts, p.created_at, p.session, p.requested_model, \
         p.message_count, p.size_bytes, p.truncated, p.preview, p.redactions, \
         p.truncation, p.content_hash, p.system_hash, p.role_counts, p.policy_version";

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
            "SELECT {} , u.upstream, u.protocol, u.status, \
             u.prompt_tokens, u.completion_tokens, u.cache_read_tokens, \
             u.cache_write_tokens, u.total_tokens, u.duration_ms, u.ttft_ms, \
             u.stream, u.paused, u.error, u.cost, u.upstream_model, \
             u.truncated AS usage_truncated \
             FROM prompt_records p LEFT JOIN usage_records u ON u.id = p.id{where_sql} \
             ORDER BY {column} {direction}{tie} LIMIT ? OFFSET ?",
            Self::PROMPT_LIST_COLUMNS
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
            .unwrap_or_else(|e| {
                eprintln!("export_prompts error: {e}");
                Vec::new()
            })
    }

    pub fn count_prompts(&self, f: &PromptFilters) -> i64 {
        let (where_sql, args) = prompt_where_clause(f);
        // 计数不需要 usage 列；只有按上游筛选时才需要关联，否则把百万级
        // 提示词逐条去做 PK lookup 纯属浪费。
        let join = if f.upstream.is_some() {
            " LEFT JOIN usage_records u ON u.id = p.id"
        } else {
            ""
        };
        let conn = self.reader();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM prompt_records p{join}{where_sql}"),
            params_from_iter(args.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    pub fn get_prompt(&self, id: &str) -> Option<Value> {
        let conn = self.reader();
        let sql = "SELECT p.*, u.upstream, u.protocol, u.status, \
                   u.prompt_tokens, u.completion_tokens, u.cache_read_tokens, \
                   u.cache_write_tokens, u.duration_ms, u.ttft_ms, u.total_tokens \
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

    /// 同会话中按时间相邻的记录，用于详情页直接发起进化对比。
    /// 只返回定位信息，不返回消息内容。
    pub fn prompt_neighbors(&self, id: &str) -> Value {
        let conn = self.reader();
        let Ok((session, ts, rowid)) = conn.query_row(
            "SELECT session, ts, rowid FROM prompt_records WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        ) else {
            return json!({"prev": null, "next": null});
        };
        let Some(session) = session else {
            // 无会话记录无法可靠判断相邻关系，宁可不提供快捷对比。
            return json!({"prev": null, "next": null});
        };
        let prev = conn
            .query_row(
                "SELECT id, ts, created_at FROM prompt_records
                 WHERE session = ?1 AND (ts < ?2 OR (ts = ?2 AND rowid < ?3))
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                rusqlite::params![session.as_str(), ts, rowid],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "created_at": r.get::<_, String>(2)?,
                    }))
                },
            )
            .ok();
        let next = conn
            .query_row(
                "SELECT id, ts, created_at FROM prompt_records
                 WHERE session = ?1 AND (ts > ?2 OR (ts = ?2 AND rowid > ?3))
                 ORDER BY ts ASC, rowid ASC LIMIT 1",
                rusqlite::params![session.as_str(), ts, rowid],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "created_at": r.get::<_, String>(2)?,
                    }))
                },
            )
            .ok();
        json!({"prev": prev, "next": next})
    }

    pub fn export_prompts(&self, f: &PromptFilters, limit: i64) -> Vec<Value> {
        let (where_sql, args) = prompt_where_clause(f);
        let sql = format!(
            "SELECT p.*, u.upstream, u.protocol, u.status, \
             u.prompt_tokens, u.completion_tokens, u.cache_read_tokens, \
             u.cache_write_tokens, u.total_tokens, u.duration_ms, u.ttft_ms, \
             u.stream, u.paused, u.error, u.cost, u.upstream_model, \
             u.truncated AS usage_truncated \
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
        // 只有按上游筛选时才需要关联 usage_records（口径与 count_prompts 一致）。
        let join = if f.upstream.is_some() {
            " LEFT JOIN usage_records u ON u.id = p.id"
        } else {
            ""
        };
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        // 分批删除：一次清掉百万行会长时间占住写锁、撑大 WAL；
        // 每批独立提交，写锁在批次之间释放，代理写入基本无感。
        let mut removed = 0i64;
        loop {
            let sql = format!(
                "DELETE FROM prompt_records WHERE rowid IN (\
                 SELECT p.rowid FROM prompt_records p{join}{where_sql} LIMIT {PROMPT_DELETE_BATCH})"
            );
            let n = conn
                .execute(&sql, params_from_iter(args.iter()))
                .unwrap_or(0) as i64;
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        removed
    }

    pub fn delete_prompts_older_than(&self, days: i64) -> i64 {
        if days <= 0 {
            return 0;
        }
        let cutoff = Local::now().timestamp() - days * 86400;
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut removed = 0i64;
        loop {
            let n = conn
                .execute(
                    "DELETE FROM prompt_records WHERE rowid IN (\
                     SELECT rowid FROM prompt_records WHERE ts < ?1 \
                     LIMIT ?2)",
                    rusqlite::params![cutoff, PROMPT_DELETE_BATCH],
                )
                .map(|n| n as i64)
                .unwrap_or(0);
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        removed
    }

    pub fn trim_prompts(&self, max_records: i64) -> i64 {
        if max_records <= 0 {
            return 0;
        }
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        // OFFSET 固定为保留条数：每批删除「保留窗口之外」的下一批，
        // 直到保留窗口之外不足一批为止。
        let mut removed = 0i64;
        loop {
            let n = conn
                .execute(
                    "DELETE FROM prompt_records WHERE rowid IN (\
                     SELECT rowid FROM prompt_records ORDER BY ts DESC \
                     LIMIT ?2 OFFSET ?1)",
                    rusqlite::params![max_records, PROMPT_DELETE_BATCH],
                )
                .map(|n| n as i64)
                .unwrap_or(0);
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        removed
    }

    pub fn prompt_count_and_size(&self) -> Value {
        // 全表 SUM/COUNT 在 GB 级库上是秒级（实测 9GB 库 15s）操作，而它只服务于
        // 提示词页的一行提示文案与总览卡片。策略：缓存新鲜直接返回；过期则
        // 立即返回旧值（没有旧值就返回零值）并在后台线程重算——请求路径
        // 永不等待全表扫描。
        let snapshot = {
            let cache = self
                .prompt_stats
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.clone()
        };
        match snapshot {
            Some((at, value)) if at.elapsed() < PROMPT_STATS_TTL => value,
            _ => {
                self.spawn_prompt_stats_refresh();
                snapshot.map(|(_, value)| value).unwrap_or_else(|| {
                    json!({
                        "records": 0, "bytes": 0, "truncated": 0,
                        "redacted_records": 0, "redactions": 0,
                        "max_bytes": 0, "avg_bytes": 0,
                        "stale": true,
                    })
                })
            }
        }
    }

    /// 在独立线程重算提示词全表统计并写入共享缓存；已有重算在跑时直接返回。
    /// 由 prompt_count_and_size 的过期路径与清理/回收之后的显式刷新调用。
    pub fn spawn_prompt_stats_refresh(&self) {
        use std::sync::atomic::Ordering;
        if self
            .prompt_stats
            .refreshing
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        // 后台线程自己开一条只读连接：Db 里的连接池被 Mutex 保护无法跨线程共享，
        // 而统计查询只读、错开运行也不抢业务读锁。
        let path = self.path.clone();
        let stats = Arc::clone(&self.prompt_stats);
        let result = std::thread::Builder::new()
            .name("prompt-stats".into())
            .spawn(move || {
                let value = compute_prompt_stats_path(&path).unwrap_or_else(|err| {
                    tracing::warn!("提示词统计后台重算失败: {err}");
                    zero_prompt_stats()
                });
                let mut cache = stats.cache.lock().unwrap_or_else(|e| e.into_inner());
                *cache = Some((std::time::Instant::now(), value));
                stats.refreshing.store(false, Ordering::Release);
            });
        if result.is_err() {
            // 线程创建失败：复位标记，下次过期路径再试。
            self.prompt_stats.refreshing.store(false, Ordering::Release);
        }
    }

    /// 同步重算提示词全表统计并写入缓存。
    /// 供清理/回收接口收尾（操作本身已是慢操作，不差这一次扫描）、
    /// 后台刷新线程与测试调用；请求路径不要直接用它。
    pub fn recompute_prompt_stats(&self) {
        let value = compute_prompt_stats_path(&self.path).unwrap_or_else(|err| {
            tracing::warn!("提示词统计重算失败: {err}");
            zero_prompt_stats()
        });
        let mut cache = self
            .prompt_stats
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cache = Some((std::time::Instant::now(), value));
    }

    /// 清理/回收后使统计缓存失效：下一次读取会触发后台重算，
    /// 避免面板继续展示清理前的旧总量。
    pub fn invalidate_prompt_stats(&self) {
        let mut cache = self
            .prompt_stats
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cache = None;
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

    /// 用回调重算全部历史记录的费用：按批流式「读 → 算 → 写」。
    ///
    /// 保存单价表后的「重算历史费用」入口。为什么分批：百万行级表一次性
    /// 读进内存再单事务全量 UPDATE，会长时间占住写连接并驻留一份全表
    /// 数据；改成 rowid 翻页，每批独立提交，代理写入在批间基本无感。
    /// 中途失败时已完成批次保留（费用是辅助字段，重跑即可补齐），绝不丢记录。
    pub fn recompute_costs(&self, compute: impl Fn(&CostRow) -> f64) -> usize {
        const BATCH: i64 = PROMPT_DELETE_BATCH;
        let mut last_rowid: i64 = 0;
        let mut updated = 0usize;
        loop {
            let rows: Vec<(i64, CostRow)> = {
                let conn = self.reader();
                let sql = "SELECT rowid, id, ts, upstream, requested_model, upstream_model, \
                           prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens \
                           FROM usage_records WHERE rowid > ?1 ORDER BY rowid LIMIT ?2";
                let mut stmt = match conn.prepare(sql) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!("重算费用读取失败: {err}");
                        return updated;
                    }
                };
                let mapped = stmt.query_map(rusqlite::params![last_rowid, BATCH], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        CostRow {
                            id: row.get("id")?,
                            ts: row.get("ts")?,
                            upstream: row
                                .get::<_, Option<String>>("upstream")?
                                .unwrap_or_default(),
                            requested_model: row
                                .get::<_, Option<String>>("requested_model")?
                                .unwrap_or_default(),
                            upstream_model: row
                                .get::<_, Option<String>>("upstream_model")?
                                .unwrap_or_default(),
                            prompt_tokens: row.get("prompt_tokens")?,
                            completion_tokens: row.get("completion_tokens")?,
                            cache_read_tokens: row.get("cache_read_tokens")?,
                            cache_write_tokens: row.get("cache_write_tokens")?,
                        },
                    ))
                });
                match mapped {
                    Ok(iter) => iter.filter_map(Result::ok).collect(),
                    Err(err) => {
                        tracing::warn!("重算费用遍历失败: {err}");
                        return updated;
                    }
                }
            };
            if rows.is_empty() {
                break;
            }
            last_rowid = rows.last().map(|(rowid, _)| *rowid).unwrap_or(last_rowid);
            let updates: Vec<(f64, &str)> = rows
                .iter()
                .map(|(_, row)| (compute(row), row.id.as_str()))
                .collect();
            let mut conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
            let result = (|| -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                {
                    let mut stmt =
                        tx.prepare("UPDATE usage_records SET cost = ?1 WHERE id = ?2")?;
                    for (cost, id) in &updates {
                        updated += stmt.execute(rusqlite::params![cost, id])?;
                    }
                }
                tx.commit()
            })();
            if let Err(err) = result {
                tracing::warn!("重算费用写入失败: {err}");
            }
            drop(conn);
            if rows.len() < BATCH as usize {
                break;
            }
        }
        updated
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
        let key = Self::agg_key("totals", f, "");
        if let Some(hit) = self.agg_cache_get(&key) {
            return hit;
        }
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
        let value = row.unwrap_or_else(|_| {
            json!({
                "requests": 0, "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
                "cache_read_tokens": 0, "cache_write_tokens": 0, "cost": 0.0, "errors": 0,
                "ok": 0, "success_rate": 0.0, "cache_hit_rate": Value::Null,
                "avg_duration_ms": 0.0, "avg_ttft_ms": 0.0, "ttft_samples": 0,
                "p50_duration_ms": 0.0, "p95_duration_ms": 0.0, "max_duration_ms": 0.0
            })
        });
        self.agg_cache_put(&key, value.clone());
        value
    }

    /// 今日（本地日期）请求数与缓存命中率，供托盘轻量刷新。
    pub fn today_stats(&self) -> Value {
        // 用本地零点的 epoch 秒做范围过滤，让 idx_usage_ts 可用；
        // 旧的 substr(created_at,1,10) 写法把索引废掉，每次都是全表扫。
        let midnight = Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_local_timezone(Local).single().map(|t| t.timestamp()))
            .flatten()
            .unwrap_or(0);
        let conn = self.reader();
        let row = conn.query_row(
            "SELECT COUNT(*) AS requests,
                    COALESCE(SUM(CASE WHEN status IS NULL OR status < 400 THEN 1 ELSE 0 END),0) AS ok,
                    COALESCE(SUM(cache_read_tokens),0) AS cache_read_tokens,
                    COALESCE(SUM(prompt_tokens),0) AS prompt_tokens
             FROM usage_records
             WHERE ts >= ?1",
            [midnight],
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
        let cache_key = format!("summary|{group_by}|{}", Self::agg_key("g", f, ""));
        if let Some(hit) = self.agg_cache_get(&cache_key) {
            return Ok(serde_json::from_value(hit).unwrap_or_default());
        }
        let bucket = match group_by {
            "day" => "substr(created_at,1,10)",
            "hour" => "substr(created_at,1,13)",
            "model" => "COALESCE(requested_model,'(unknown)')",
            "upstream" => "COALESCE(upstream,'(unknown)')",
            "session" => "COALESCE(NULLIF(session,''),'(无会话)')",
            other => return Err(format!("不支持的分组维度: {other}")),
        };
        let (where_sql, args) = where_clause(f, f.only.as_deref());
        // 分组桶的来源列按维度取；error/upstream_model 等宽列不进 CTE。
        let bucket_source = match group_by {
            "day" | "hour" => "created_at, ",
            "model" => "requested_model, ",
            "upstream" => "upstream, ",
            _ => "session, ",
        };
        let sql = format!(
            "WITH ranked AS (
                SELECT {bucket_source} {bucket} AS bucket,
                       {USAGE_AGG_COLUMNS},
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
        self.agg_cache_put(&cache_key, Value::Array(out.clone()));
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
        let cache_key = format!(
            "session|{}|{limit}|{sort}|{order}",
            Self::agg_key("s", f, "")
        );
        if let Some(hit) = self.agg_cache_get(&cache_key) {
            return serde_json::from_value(hit).unwrap_or_default();
        }
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
                SELECT {expr} AS grp, {USAGE_AGG_COLUMNS}, requested_model,
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
            Ok(iter) => {
                let out: Vec<Value> = iter.filter_map(Result::ok).map(finish_group_row).collect();
                self.agg_cache_put(&cache_key, Value::Array(out.clone()));
                out
            }
            Err(_) => Vec::new(),
        }
    }

    fn grouped_usage(&self, f: &Filters, limit: i64, kind: &str) -> Vec<Value> {
        let cache_key = format!("grouped|{kind}|{limit}|{}", Self::agg_key("g", f, ""));
        if let Some(hit) = self.agg_cache_get(&cache_key) {
            return serde_json::from_value(hit).unwrap_or_default();
        }
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
        // upstream 维度还要统计模型数，所以 requested_model 一并投影。
        let dims = if kind == "upstream" {
            "upstream, requested_model, "
        } else {
            "upstream, "
        };
        let sql = format!(
            "WITH ranked AS (
                SELECT {dims} {expr} AS grp,
                       {USAGE_AGG_COLUMNS},
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
            Ok(iter) => {
                let out: Vec<Value> = iter.flatten().map(finish_group_row).collect();
                self.agg_cache_put(&cache_key, Value::Array(out.clone()));
                out
            }
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
        // 与 clear_prompts 同样的分批策略：一次清掉百万行会长时间占住写锁、
        // 撑大 WAL；每批独立提交，写锁在批次之间释放，代理写入基本无感。
        let mut removed = 0i64;
        loop {
            let n = conn
                .execute(
                    &format!(
                        "DELETE FROM usage_records WHERE rowid IN (\
                         SELECT rowid FROM usage_records{where_sql} LIMIT {PROMPT_DELETE_BATCH})"
                    ),
                    params_from_iter(args.iter()),
                )
                .unwrap_or(0) as i64;
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        drop(conn);
        if removed > 0 {
            self.invalidate_agg_cache();
        }
        removed
    }

    pub fn delete_older_than(&self, days: i64) -> i64 {
        if days <= 0 {
            return 0;
        }
        let cutoff = chrono::Utc::now().timestamp() - days * 86400;
        // 与提示词清理同样的批处理：避免在百万行级表上单事务长锁。
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut removed = 0i64;
        loop {
            let n = conn
                .execute(
                    "DELETE FROM usage_records WHERE rowid IN (\
                     SELECT rowid FROM usage_records WHERE ts < ?1 LIMIT ?2)",
                    rusqlite::params![cutoff, PROMPT_DELETE_BATCH],
                )
                .map(|n| n as i64)
                .unwrap_or(0);
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        drop(conn);
        if removed > 0 {
            self.invalidate_agg_cache();
        }
        removed
    }

    pub fn vacuum(&self) -> i64 {
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = conn.execute("VACUUM", []);
        std::fs::metadata(&self.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0)
    }

    /// 查询计划治理：`PRAGMA optimize` 按需 ANALYZE，让查询计划器拿到新鲜统计。
    ///
    /// 启动时与保留清理大批量删除后调用。失败只记日志：优化失败不该阻断流程。
    pub fn optimize(&self) {
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = conn.execute_batch("PRAGMA optimize;") {
            tracing::warn!("数据库统计优化失败：{err}");
        }
    }

    /// 保留治理：配置审计只留最近 `max_records` 条（按 rowid 保序，插入即递增）。
    ///
    /// 为什么不用天数：`at` 是 RFC3339 本地时间文本，夏令时切换时字典序与
    /// 时间序会短暂错位；rowid 单调且与插入顺序一致，语义就是「最近 N 条」。
    pub fn trim_config_audit(&self, max_records: i64) -> i64 {
        if max_records <= 0 {
            return 0;
        }
        let conn = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut removed = 0i64;
        loop {
            let n = conn
                .execute(
                    "DELETE FROM config_audit WHERE rowid IN (\
                     SELECT rowid FROM config_audit ORDER BY rowid DESC LIMIT ?2 OFFSET ?1)",
                    rusqlite::params![max_records, PROMPT_DELETE_BATCH],
                )
                .map(|n| n as i64)
                .unwrap_or(0);
            removed += n;
            if n < PROMPT_DELETE_BATCH {
                break;
            }
        }
        removed
    }
}

/// 当前时间的毫秒时间戳。
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 只读连接池大小：面板一次并发拉 6+ 个统计接口（总览页 9 个），
/// 读连接数要覆盖并发度，否则会在读锁上排队、整页被最慢的查询拖住。
const READER_POOL_SIZE: usize = 6;

/// 提示词批量删除的批大小：把「清空百万行」拆成独立小事务，
/// 避免长时间占住写锁与一次性撑大 WAL。
const PROMPT_DELETE_BATCH: i64 = 5000;

/// 提示词统计（全表聚合）缓存 TTL：过期后返回旧值并后台重算，
/// 请求路径永不等待 GB 级库上的秒级全表扫描。TTL 取 5 分钟：
/// 统计只是提示行文案，不值得让它常态化吃掉一个核心做全表扫描。
const PROMPT_STATS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn connect(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    // temp_store=MEMORY：面板分位数查询的窗口排序会落临时文件，
    // 实测 30 万行排序 5.4s -> 1.5s。cache/mmap 提升大库扫描的页命中。
    // 都是连接级 PRAGMA，不落盘、不改变数据语义。
    conn.execute_batch(
        "PRAGMA busy_timeout=5000; \
         PRAGMA temp_store=MEMORY; \
         PRAGMA cache_size=-32000; \
         PRAGMA mmap_size=134217728;",
    )?;
    Ok(conn)
}

/// 后台统计线程用的独立连接：不参与连接池轮转。
fn compute_prompt_stats_path(path: &Path) -> rusqlite::Result<Value> {
    let conn = Connection::open(path)?;
    let _ = conn.execute_batch(
        "PRAGMA busy_timeout=5000; \
         PRAGMA temp_store=MEMORY; \
         PRAGMA cache_size=-32000; \
         PRAGMA mmap_size=134217728;",
    );
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0),
                COALESCE(SUM(CASE WHEN truncated != 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN redactions > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(redactions), 0),
                COALESCE(MAX(size_bytes), 0),
                COALESCE(CAST(AVG(size_bytes) AS INTEGER), 0)
         FROM prompt_records",
        [],
        |r| {
            Ok(json!({
                "records": r.get::<_, i64>(0)?,
                "bytes": r.get::<_, i64>(1)?,
                "truncated": r.get::<_, i64>(2)?,
                "redacted_records": r.get::<_, i64>(3)?,
                "redactions": r.get::<_, i64>(4)?,
                "max_bytes": r.get::<_, i64>(5)?,
                "avg_bytes": r.get::<_, i64>(6)?,
            }))
        },
    )
}

fn zero_prompt_stats() -> Value {
    json!({
        "records": 0, "bytes": 0, "truncated": 0,
        "redacted_records": 0, "redactions": 0,
        "max_bytes": 0, "avg_bytes": 0,
    })
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
    // 会话演进按「会话 + 时间」取最近 N 条；(session, ts) 复合索引避免逐会话排序。
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prompt_session_ts ON prompt_records(session, ts)",
        [],
    );
    // 旧版本的单列 upstream/model 索引已被 SCHEMA 里的复合索引取代。
    // SCHEMA 先建复合索引、这里再删旧索引：中断后重启可重试，两步都幂等；
    // 不删的话旧索引会在每次启动时被 SCHEMA 重建，大库上白付一次全索引构建。
    let _ = conn.execute("DROP INDEX IF EXISTS idx_usage_up", []);
    let _ = conn.execute("DROP INDEX IF EXISTS idx_usage_model", []);
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
    let truncation: Option<Value> = row
        .get::<_, Option<String>>("truncation")?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let role_counts: Value = row
        .get::<_, Option<String>>("role_counts")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));
    // policy_version 按 INTEGER 写入，但从 Python 版迁移的历史库可能存过文本；
    // 单行类型异常不该让整页列表被 flatten 静默吞掉，回退 0。
    let policy_version: i64 = match row.get::<_, SqlValue>("policy_version") {
        Ok(SqlValue::Integer(value)) => value,
        Ok(SqlValue::Real(value)) => value as i64,
        Ok(SqlValue::Text(value)) => value.trim().parse().unwrap_or(0),
        _ => 0,
    };
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
        "redactions": row.get::<_, i64>("redactions")?,
        "truncation": truncation,
        "content_hash": row.get::<_, Option<String>>("content_hash")?,
        "system_hash": row.get::<_, Option<String>>("system_hash")?,
        "role_counts": role_counts,
        "policy_version": policy_version,
        "usage": {
            "prompt_tokens": row.get::<_, Option<i64>>("prompt_tokens")?,
            "completion_tokens": row.get::<_, Option<i64>>("completion_tokens")?,
            "cache_read_tokens": row.get::<_, Option<i64>>("cache_read_tokens")?,
            "cache_write_tokens": row.get::<_, Option<i64>>("cache_write_tokens")?,
            "total_tokens": row.get::<_, Option<i64>>("total_tokens")?,
            "duration_ms": row.get::<_, Option<i64>>("duration_ms")?,
            "ttft_ms": row.get::<_, Option<i64>>("ttft_ms")?,
        }
    }))
}

/// 窗口函数 CTE 只投影聚合要用的列：usage_records 里有 error/upstream_model
/// 等文本列，SELECT * 会把整表拖进 CTE 物化，大库上白耗内存与时间。
/// WHERE/分组仍直接作用于基表，不受投影影响。
const USAGE_AGG_COLUMNS: &str = "duration_ms, status, ttft_ms, prompt_tokens, completion_tokens, \
     total_tokens, cache_read_tokens, cache_write_tokens, cost, ts";

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
    fn recompute_costs_rewrites_every_row_with_callback() {
        let db = temp_db();
        for prompt in [1_000_000i64, 500_000] {
            db.record_usage(&UsageRecord {
                upstream: "cch".into(),
                requested_model: "deepseek-chat".into(),
                upstream_model: "deepseek-chat".into(),
                prompt_tokens: prompt,
                completion_tokens: 100_000,
                status: 200,
                ..Default::default()
            });
        }
        let updated = db.recompute_costs(|row| {
            // 每 100 万输入 token 计 1 元、输出计 2 元（人民币）
            (row.prompt_tokens as f64 * 1.0 + row.completion_tokens as f64 * 2.0) / 1_000_000.0
        });
        assert_eq!(updated, 2);
        let f = Filters::default();
        let rows = db.list_usage(&f, 10, 0, "ts", "asc");
        let mut costs: Vec<f64> = rows
            .iter()
            .map(|r| r["cost"].as_f64().unwrap_or(-1.0))
            .collect();
        costs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((costs[0] - 0.7).abs() < 1e-9);
        assert!((costs[1] - 1.2).abs() < 1e-9);
    }

    #[test]
    fn legacy_prompt_database_migrates_without_data_loss() {
        let dir = std::env::temp_dir().join(format!("mg-db-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modelgate.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
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
                    error TEXT, cost REAL NOT NULL DEFAULT 0, session TEXT
                );
                CREATE TABLE prompt_records (
                    id TEXT PRIMARY KEY, ts INTEGER NOT NULL, created_at TEXT NOT NULL,
                    session TEXT, requested_model TEXT, message_count INTEGER NOT NULL DEFAULT 0,
                    size_bytes INTEGER NOT NULL DEFAULT 0, truncated INTEGER NOT NULL DEFAULT 0,
                    preview TEXT, messages TEXT NOT NULL
                );
                INSERT INTO usage_records (
                    id, ts, created_at, upstream, requested_model, prompt_tokens,
                    completion_tokens, total_tokens, status, session
                ) VALUES (
                    'old-usage', 1700000000, '2023-11-14T00:00:00.000+08:00',
                    'legacy', 'm', 10, 2, 12, 200, 'legacy-session'
                );
                INSERT INTO prompt_records (
                    id, ts, created_at, session, requested_model, message_count,
                    size_bytes, truncated, preview, messages
                ) VALUES (
                    'old-prompt', 1700000000, '2023-11-14T00:00:00.000+08:00',
                    'legacy-session', 'm', 1, 25, 0, 'hello',
                    '[{"role":"user","content":"hello"}]'
                );
                "#,
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        {
            let conn = db.write.lock().unwrap();
            let columns: Vec<String> = conn
                .prepare("PRAGMA table_info(prompt_records)")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .flatten()
                .collect();
            for column in [
                "redactions",
                "truncation",
                "content_hash",
                "system_hash",
                "role_counts",
                "policy_version",
            ] {
                assert!(
                    columns.iter().any(|name| name == column),
                    "迁移后缺少列 {column}"
                );
            }
        }

        let old = db.get_prompt("old-prompt").expect("旧提示词记录必须保留");
        assert_eq!(old["messages"][0]["content"], "hello");
        assert_eq!(old["redactions"], 0);
        assert!(old["truncation"].is_null());
        assert_eq!(old["role_counts"], json!({}));
        assert_eq!(old["policy_version"], 0);

        let rows = db.list_prompts(&PromptFilters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows.len(), 1, "旧记录必须仍可列出");
        assert_eq!(rows[0]["id"], "old-prompt");

        db.record_prompt(&sample_prompt("new-prompt", "m", "legacy-session", "new"));
        let new = db.get_prompt("new-prompt").expect("迁移后必须能写新记录");
        assert_eq!(new["messages"][0]["content"], "new");

        let neighbors = db.prompt_neighbors("old-prompt");
        assert_eq!(neighbors["next"]["id"], "new-prompt");
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

    /// 列表查询刻意不取 messages 大字段；这里守住列表行仍需携带的
    /// 联查元数据（上游、状态、token、耗时），防止窄列化时漏列。
    #[test]
    fn prompt_list_rows_carry_joined_usage_metadata() {
        let db = temp_db();
        db.record_prompt(&sample_prompt("req-u", "m1", "s1", "hello"));
        db.record_usage(&UsageRecord {
            id: "req-u".into(),
            upstream: "u1".into(),
            protocol: "openai".into(),
            requested_model: "m1".into(),
            upstream_model: "up-m1".into(),
            prompt_tokens: 11,
            completion_tokens: 3,
            cache_read_tokens: 4,
            duration_ms: 123,
            ttft_ms: 45,
            status: 200,
            ..Default::default()
        });
        let rows = db.list_prompts(&PromptFilters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["upstream"], "u1");
        assert_eq!(row["protocol"], "openai");
        assert_eq!(row["status"], 200);
        assert_eq!(row["usage"]["prompt_tokens"], 11);
        assert_eq!(row["usage"]["completion_tokens"], 3);
        assert_eq!(row["usage"]["cache_read_tokens"], 4);
        assert_eq!(row["usage"]["duration_ms"], 123);
        assert_eq!(row["usage"]["ttft_ms"], 45);
        assert_eq!(row["preview"], "hello");
        assert_eq!(row["session"], "s1");
        assert!(
            row.get("messages").is_none(),
            "列表行不得携带 messages 大字段"
        );
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
        // 请求路径的 prompt_count_and_size 是 stale-while-revalidate（首调返回占位值），
        // 断言前先同步重算一次，与清理接口收尾的口径一致。
        db.recompute_prompt_stats();
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

    #[test]
    fn record_usage_batch_keeps_ids_and_is_idempotent() {
        let db = temp_db();
        let record = |id: &str| UsageRecord {
            id: id.into(),
            upstream: "u".into(),
            requested_model: "m".into(),
            upstream_model: "m".into(),
            prompt_tokens: 10,
            completion_tokens: 5,
            status: 200,
            ..Default::default()
        };
        let written = db.record_usage_batch(&[record("req-a"), record("req-b")]);
        assert_eq!(written, 2, "批量写入条数必须等于输入条数");
        // 空批次直接返回。
        assert_eq!(db.record_usage_batch(&[]), 0);
        // 后台重试重放同 ID（INSERT OR REPLACE）不产生重复行。
        db.record_usage_batch(std::slice::from_ref(&record("req-a")));
        assert_eq!(db.count_usage(&Filters::default()), 2);
        // 空 ID 由库层补 UUID，且能落库。
        let id = db.record_usage(&UsageRecord {
            upstream: "u".into(),
            requested_model: "m".into(),
            upstream_model: "m".into(),
            status: 200,
            ..Default::default()
        });
        assert!(!id.is_empty());
        assert_eq!(db.count_usage(&Filters::default()), 3);
    }

    #[test]
    fn clear_usage_batches_deletes_beyond_one_transaction() {
        let db = temp_db();
        // PROMPT_DELETE_BATCH = 5000：造 5100 行强制清理走两批以上删除。
        let batch: Vec<UsageRecord> = (0..5_100)
            .map(|i| UsageRecord {
                id: format!("req-{i}"),
                upstream: "u".into(),
                requested_model: "m".into(),
                upstream_model: "m".into(),
                status: 200,
                ..Default::default()
            })
            .collect();
        assert_eq!(db.record_usage_batch(&batch), 5_100);
        let removed = db.clear_usage(&Filters::default());
        assert_eq!(removed, 5_100, "分批删除的总行数必须等于全表行数");
        assert_eq!(db.count_usage(&Filters::default()), 0);
    }

    #[test]
    fn composite_indexes_replace_legacy_single_column_ones() {
        let db = temp_db();
        let conn = db.write.lock().unwrap();
        let has_index = |name: &str| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
        };
        assert!(
            has_index("idx_usage_up_ts"),
            "复合索引 (upstream, ts) 必须建立"
        );
        assert!(
            has_index("idx_usage_model_ts"),
            "复合索引 (model, ts) 必须建立"
        );
        assert!(has_index("idx_usage_ts"), "时间窗索引保留");
        assert!(
            !has_index("idx_usage_up"),
            "旧单列 upstream 索引必须被复合索引取代"
        );
        assert!(
            !has_index("idx_usage_model"),
            "旧单列 model 索引必须被复合索引取代"
        );
    }

    #[test]
    fn legacy_single_column_indexes_are_dropped_on_reopen() {
        let dir = std::env::temp_dir().join(format!("mg-db-legacy-idx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modelgate.db");
        {
            let _db = Db::open(&path).unwrap();
        }
        // 模拟旧版本库文件：手工补回旧单列索引后再打开。
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_usage_up ON usage_records(upstream)",
                [],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let conn = db.write.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_usage_up'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "重开后旧单列索引必须被清理，避免每次启动重建");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_returning_is_supported_by_bundled_sqlite() {
        let db = temp_db();
        // mark_failure 依赖 UPDATE ... RETURNING（SQLite ≥3.35）；守住能力面，
        // 版本回退时这里先炸而不是热路径静默拿不到失败计数。
        let conn = db.write.lock().unwrap();
        conn.execute(
            "INSERT INTO credential_members (id, pool_id, label, kind, secret_ref) \
             VALUES ('m1', 'p1', '成员', 'api_key', 'store:x')",
            [],
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "UPDATE credential_members SET failure_count = failure_count + 1 \
                 WHERE id = 'm1' RETURNING failure_count",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row(
                "UPDATE credential_members SET failure_count = failure_count + 1 \
                 WHERE id = 'm1' RETURNING failure_count",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "自增必须累加而不是覆盖");
    }

    #[test]
    fn trim_config_audit_keeps_newest_rows() {
        let db = temp_db();
        {
            let conn = db.write.lock().unwrap();
            for i in 0..12 {
                conn.execute(
                    "INSERT INTO config_audit (id, at, action) VALUES (?1, ?2, 'test')",
                    rusqlite::params![
                        format!("audit-{i}"),
                        format!("2026-01-01T00:00:{i:02}+08:00")
                    ],
                )
                .unwrap();
            }
        }
        let removed = db.trim_config_audit(5);
        assert_eq!(removed, 7, "只保留最近 5 条");
        let conn = db.write.lock().unwrap();
        let remaining: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT id FROM config_audit ORDER BY rowid")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.flatten().collect()
        };
        assert_eq!(
            remaining,
            ["audit-7", "audit-8", "audit-9", "audit-10", "audit-11"]
        );
    }
}
