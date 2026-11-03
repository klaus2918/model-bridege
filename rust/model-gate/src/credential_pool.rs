//! 凭据池：多 Key / 多账号的统一事实源与请求期选择器。
//!
//! 安全口径：
//! - 数据库只存 `secret_ref`（`store:...`），不存密钥本体；
//! - API 响应不返回 secret_ref、密钥、账号、Cookie；
//! - 删除池/成员默认只停用，不删除密钥；清理密钥需显式参数。
//!
//! 状态机：active → cooling（失败达阈值）→ half_open（冷却结束）→ active；
//! exhausted（配额耗尽，窗口重置后惰性恢复）；disabled（人工停用）。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::proxy::{error_json, AppState};

static MEMBER_SEQ: AtomicU64 = AtomicU64::new(1);
static WEIGHTED_SEQ: AtomicU64 = AtomicU64::new(0);

fn now_text() -> String {
    chrono::Local::now().to_rfc3339()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn new_member_id() -> String {
    format!(
        "pm_{}_{}",
        now_millis(),
        MEMBER_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[derive(Debug, Clone)]
pub struct Pool {
    pub id: String,
    pub upstream: String,
    pub plugin_id: Option<String>,
    pub strategy: String,
    pub status: String,
    pub cooldown_seconds: i64,
    pub failure_threshold: i64,
    pub fallback_to_single: bool,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub id: String,
    pub pool_id: String,
    pub label: String,
    pub kind: String,
    pub secret_ref: String,
    pub priority: i64,
    pub weight: i64,
    pub status: String,
    pub cooldown_until: Option<String>,
    pub failure_count: i64,
    pub quota_json: Option<String>,
    pub last_used_at: Option<String>,
    pub usage_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PoolCredential {
    pub pool_id: String,
    pub member_id: String,
    pub label: String,
    pub key: String,
}

#[derive(Debug, Clone, Default)]
pub struct PoolSummary {
    pub members: i64,
    pub available: i64,
    pub cooling: i64,
    pub disabled: i64,
    pub exhausted: i64,
    pub needs_secret: i64,
    pub quota_used: i64,
    pub quota_limit: i64,
}

impl PoolSummary {
    pub fn to_json(&self, pool: &Pool) -> Value {
        json!({
            "id": pool.id,
            "upstream": pool.upstream,
            "strategy": pool.strategy,
            "status": pool.status,
            "members": self.members,
            "available": self.available,
            "cooling": self.cooling,
            "disabled": self.disabled,
            "exhausted": self.exhausted,
            "needsSecret": self.needs_secret,
            "quota": { "used": self.quota_used, "limit": self.quota_limit },
        })
    }
}

/// 池/成员快照缓存：TTL 内选择凭据零 SQL。
///
/// 为什么能缓存：`credential_members` 的全部写路径都在本文件内（已核对无
/// 旁路写），结构性写入后显式失效；TTL 只是跨进程改动的兜底。冷却判定
/// 不依赖快照新鲜度——`cooldown_active` 用真实时间对快照里的
/// `cooldown_until` 求值；`last_used_at`/`failure_count` 等影响选择排序
/// 的字段在写入后**原位更新**缓存（`cache_touch`），轮转公平性不受影响。
#[derive(Default)]
pub struct PoolCache {
    pools: HashMap<String, (Instant, Option<Pool>)>,
    members: HashMap<String, (Instant, Vec<Member>)>,
}

const POOL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

/// 密钥库解密缓存：按文件 mtime 失效。
///
/// 为什么：池选择要对每个成员解一次密钥（读盘 + JSON 解析 + DPAPI 解密），
/// 5 成员池每次尝试 6 次磁盘读。mtime 不变时直接复用解密结果；面板写密钥
/// 走 `set → save` 原子替换文件，mtime 变化自动触发重载，无需显式钩子。
#[derive(Default)]
pub struct SecretsCache {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
    keys: BTreeMap<String, String>,
}

impl SecretsCache {
    fn lookup(&mut self, config_path: &std::path::Path, id: &str) -> Option<String> {
        let path = crate::secret_store::path_for_config(config_path);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if self.path.as_deref() != Some(path.as_path()) || self.mtime != mtime {
            let store = crate::secret_store::SecretStore::load(config_path);
            self.keys = store.decrypted_all();
            self.path = Some(path);
            self.mtime = mtime;
        }
        self.keys.get(id).cloned()
    }
}

fn pool_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pool> {
    Ok(Pool {
        id: row.get(0)?,
        upstream: row.get(1)?,
        plugin_id: row.get(2)?,
        strategy: row.get(3)?,
        status: row.get(4)?,
        cooldown_seconds: row.get(5)?,
        failure_threshold: row.get(6)?,
        fallback_to_single: row.get::<_, i64>(7)? != 0,
    })
}

const POOL_COLS: &str =
    "id, upstream, plugin_id, strategy, status, cooldown_seconds, failure_threshold, fallback_to_single";

fn member_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Member> {
    Ok(Member {
        id: row.get(0)?,
        pool_id: row.get(1)?,
        label: row.get(2)?,
        kind: row.get(3)?,
        secret_ref: row.get(4)?,
        priority: row.get(5)?,
        weight: row.get(6)?,
        status: row.get(7)?,
        cooldown_until: row.get(8)?,
        failure_count: row.get(9)?,
        quota_json: row.get(10)?,
        last_used_at: row.get(11)?,
        usage_json: row.get(12)?,
    })
}

const MEMBER_COLS: &str =
    "id, pool_id, label, kind, secret_ref, priority, weight, status, cooldown_until, failure_count, quota_json, last_used_at, usage_json";

/// 读缓存条目；过期条目顺手清掉。命中与否都不打库。
fn cache_pools_get(state: &AppState, id: &str) -> Option<Option<Pool>> {
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    match cache.pools.get(id) {
        Some((at, pool)) if at.elapsed() < POOL_CACHE_TTL => Some(pool.clone()),
        _ => {
            cache
                .pools
                .retain(|_, (at, _)| at.elapsed() < POOL_CACHE_TTL);
            None
        }
    }
}

/// 读成员快照；过期条目顺手清掉。
fn cache_members_get(state: &AppState, pool_id: &str) -> Option<Vec<Member>> {
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    match cache.members.get(pool_id) {
        Some((at, members)) if at.elapsed() < POOL_CACHE_TTL => Some(members.clone()),
        _ => {
            cache
                .members
                .retain(|_, (at, _)| at.elapsed() < POOL_CACHE_TTL);
            None
        }
    }
}

/// 原位更新缓存里的成员字段：选择排序/冷却判定立即可见，不触发整池重建。
fn cache_touch(state: &AppState, member_id: &str, apply: impl Fn(&mut Member)) {
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    for (_, members) in cache.members.values_mut() {
        for member in members.iter_mut() {
            if member.id == member_id {
                apply(member);
            }
        }
    }
}

/// 结构性写入（增删成员/池、批量同步）后调用：下一请求重建快照。
pub fn invalidate_pool_cache(state: &AppState) {
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    cache.pools.clear();
    cache.members.clear();
}

pub fn get_pool(state: &AppState, id: &str) -> Option<Pool> {
    if let Some(hit) = cache_pools_get(state, id) {
        return hit;
    }
    let conn = state.db.read_conn();
    let pool = conn
        .query_row(
            &format!("SELECT {POOL_COLS} FROM credential_pools WHERE id = ?1"),
            [id],
            pool_from_row,
        )
        .ok();
    // 池不存在的判断同样值得缓存：expand_attempt_targets 靠它拒绝回落。
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .pools
        .insert(id.to_string(), (Instant::now(), pool.clone()));
    pool
}

pub fn list_pool_ids(state: &AppState) -> Vec<String> {
    let conn = state.db.read_conn();
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id FROM credential_pools ORDER BY created_at") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            out.extend(rows.flatten());
        }
    }
    out
}

pub fn pool_for_upstream(state: &AppState, upstream: &str) -> Option<Pool> {
    let conn = state.db.read_conn();
    conn.query_row(
        &format!("SELECT {POOL_COLS} FROM credential_pools WHERE upstream = ?1 AND status != 'disabled' ORDER BY created_at LIMIT 1"),
        [upstream],
        pool_from_row,
    )
    .ok()
}

pub fn members(state: &AppState, pool_id: &str) -> Vec<Member> {
    if let Some(hit) = cache_members_get(state, pool_id) {
        return hit;
    }
    // 快照构建（≤1 次/秒）：惰性推进状态机只在这里跑，而不是每次选择凭据。
    recover_cooldowns(state);
    let rows = load_members(state, pool_id);
    let mut cache = state.pool_cache.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .members
        .insert(pool_id.to_string(), (Instant::now(), rows.clone()));
    rows
}

fn load_members(state: &AppState, pool_id: &str) -> Vec<Member> {
    let conn = state.db.read_conn();
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {MEMBER_COLS} FROM credential_members WHERE pool_id = ?1 ORDER BY priority, id"
    )) {
        if let Ok(rows) = stmt.query_map([pool_id], member_from_row) {
            out.extend(rows.flatten());
        }
    }
    out
}

fn cooldown_active(member: &Member, now: i64) -> bool {
    if member.status != "cooling" {
        return false;
    }
    member
        .cooldown_until
        .as_deref()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|until| until.timestamp() > now)
        .unwrap_or(false)
}

/// 惰性推进状态机：cooling 到期 → half_open；exhausted 窗口到期 → active。
pub fn recover_cooldowns(state: &AppState) {
    let now = now_unix();
    let conn = state.db.read_conn();
    let mut updates: Vec<(String, &str)> = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, status, cooldown_until, quota_json FROM credential_members")
    {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }) {
            for (id, status, until, quota) in rows.flatten() {
                if status == "cooling" {
                    let expired = until
                        .as_deref()
                        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                        .map(|until| until.timestamp() <= now)
                        .unwrap_or(true);
                    if expired {
                        updates.push((id, "half_open"));
                    }
                } else if status == "exhausted" {
                    let reset = quota
                        .as_deref()
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                        .and_then(|value| {
                            value
                                .get("resetAt")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .and_then(|text| chrono::DateTime::parse_from_rfc3339(&text).ok())
                        .map(|until| until.timestamp() <= now);
                    if reset.unwrap_or(true) {
                        updates.push((id, "active"));
                    }
                }
            }
        }
    }
    drop(conn);
    if updates.is_empty() {
        return;
    }
    let write = state.db.write_conn();
    for (id, status) in updates {
        let _ = write.execute(
            "UPDATE credential_members SET status = ?2, cooldown_until = NULL WHERE id = ?1",
            rusqlite::params![id, status],
        );
    }
}

fn quota_numbers(member: &Member) -> (i64, i64) {
    let limit = member
        .quota_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| {
            v.get("dailyTokens")
                .or_else(|| v.get("dailyRequests"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);
    let used = member
        .usage_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|v| {
            v.get("dailyTokens")
                .or_else(|| v.get("dailyRequests"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);
    (used, limit)
}

pub fn summary(state: &AppState, pool: &Pool) -> PoolSummary {
    let rows = members(state, &pool.id);
    let now = now_unix();
    let mut out = PoolSummary::default();
    for member in &rows {
        out.members += 1;
        let (used, limit) = quota_numbers(member);
        out.quota_used += used;
        out.quota_limit += limit;
        let status = member.status.as_str();
        if status == "disabled" {
            out.disabled += 1;
        } else if status == "exhausted" {
            out.exhausted += 1;
        } else if status == "needs_secret" {
            out.needs_secret += 1;
        } else if cooldown_active(member, now) {
            out.cooling += 1;
        } else if matches!(status, "active" | "half_open") {
            out.available += 1;
        }
    }
    out
}

/// 成员脱敏视图（不含 secret_ref / 密钥 / last4）。
pub fn member_json(member: &Member) -> Value {
    let now = now_unix();
    let cooling = cooldown_active(member, now);
    json!({
        "id": member.id,
        "poolId": member.pool_id,
        "label": member.label,
        "kind": member.kind,
        "priority": member.priority,
        "weight": member.weight,
        "status": if member.status == "cooling" && !cooling { "half_open" } else { member.status.as_str() },
        "cooling": cooling,
        "cooldownUntil": member.cooldown_until,
        "failureCount": member.failure_count,
        "quota": member.quota_json.as_deref().and_then(|t| serde_json::from_str::<Value>(t).ok()),
        "lastUsedAt": member.last_used_at,
        "hasSecret": !member.secret_ref.is_empty(),
    })
}

fn resolve_secret(state: &AppState, secret_ref: &str) -> Option<String> {
    let id = secret_ref.strip_prefix("store:")?;
    state
        .secrets_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .lookup(&state.config_path, id)
}

/// 选择可用成员；失败原因用字符串返回（供 503 诊断）。
pub fn select_credential(
    state: &AppState,
    pool: &Pool,
    exclude: &[String],
) -> Result<PoolCredential, String> {
    recover_cooldowns(state);
    let now = now_unix();
    let mut rows = members(state, &pool.id);
    rows.retain(|member| {
        !exclude.iter().any(|id| id == &member.id)
            && !matches!(
                member.status.as_str(),
                "disabled" | "exhausted" | "needs_secret"
            )
            && { !cooldown_active(member, now) || member.status == "half_open" }
            && member.status != "cooling"
    });
    if rows.is_empty() {
        let all = members(state, &pool.id);
        let cooling = all.iter().filter(|m| cooldown_active(m, now)).count();
        let disabled = all.iter().filter(|m| m.status == "disabled").count();
        let exhausted = all.iter().filter(|m| m.status == "exhausted").count();
        return Err(format!(
            "pool_unavailable: 可用成员 0（冷却 {cooling}，停用 {disabled}，配额耗尽 {exhausted}）"
        ));
    }
    // 过滤掉解不开的成员（标记 needs_secret，避免每次请求重复尝试）。
    let mut unusable = Vec::new();
    rows.retain(|member| {
        if resolve_secret(state, &member.secret_ref).is_some() {
            true
        } else {
            unusable.push(member.id.clone());
            false
        }
    });
    if !unusable.is_empty() {
        let write = state.db.write_conn();
        for id in unusable {
            let _ = write.execute(
                "UPDATE credential_members SET status = 'needs_secret' WHERE id = ?1",
                [id.as_str()],
            );
            cache_touch(state, &id, |member| member.status = "needs_secret".into());
        }
    }
    if rows.is_empty() {
        return Err("pool_unavailable: 所有成员都缺少可用密钥".into());
    }

    // 轮转/加权都需要一个稳定的起点；priority 保持人工顺序。
    rows.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.last_used_at.cmp(&b.last_used_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let chosen = match pool.strategy.as_str() {
        "round_robin" | "least_used" => rows
            .iter()
            .min_by_key(|member| {
                (
                    member.last_used_at.clone().unwrap_or_default(),
                    member.id.clone(),
                )
            })
            .cloned()
            .unwrap_or_else(|| rows[0].clone()),
        "weighted" => {
            let total: i64 = rows.iter().map(|member| member.weight.max(0)).sum();
            if total <= 0 {
                rows[0].clone()
            } else {
                let mut slot = (WEIGHTED_SEQ.fetch_add(1, Ordering::Relaxed) % total as u64) as i64;
                let mut picked = rows[0].clone();
                for member in &rows {
                    slot -= member.weight.max(0);
                    if slot < 0 {
                        picked = member.clone();
                        break;
                    }
                }
                picked
            }
        }
        // priority（默认）
        _ => rows[0].clone(),
    };
    let Some(key) = resolve_secret(state, &chosen.secret_ref) else {
        return Err("pool_unavailable: 选中成员读取密钥失败".into());
    };
    let used_at = now_text();
    let write = state.db.write_conn();
    let _ = write.execute(
        "UPDATE credential_members SET last_used_at = ?2 WHERE id = ?1",
        rusqlite::params![chosen.id, used_at],
    );
    drop(write);
    // 轮转/最久未用排序依赖 last_used_at：原位更新缓存，同一秒内的后续
    // 请求不会重复选中同一成员。
    cache_touch(state, &chosen.id, |member| {
        member.last_used_at = Some(used_at.clone());
    });
    Ok(PoolCredential {
        pool_id: pool.id.clone(),
        member_id: chosen.id,
        label: chosen.label,
        key,
    })
}

pub fn mark_success(state: &AppState, member_id: &str) {
    let now = now_text();
    let write = state.db.write_conn();
    let _ = write.execute(
        "UPDATE credential_members SET status = 'active', failure_count = 0, cooldown_until = NULL, last_used_at = ?2 WHERE id = ?1",
        rusqlite::params![member_id, now],
    );
    drop(write);
    // 影响选择排序/冷却判定的字段原位更新缓存，下一次选择立即可见。
    cache_touch(state, member_id, |member| {
        member.status = "active".into();
        member.failure_count = 0;
        member.cooldown_until = None;
        member.last_used_at = Some(now.clone());
    });
}

pub fn mark_failure(state: &AppState, member_id: &str, hard: bool) {
    let pool = {
        let conn = state.db.read_conn();
        conn.query_row(
            "SELECT p.cooldown_seconds, p.failure_threshold FROM credential_members m JOIN credential_pools p ON p.id = m.pool_id WHERE m.id = ?1",
            [member_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .ok()
    };
    let (cooldown, threshold) = pool.unwrap_or((60, 3));
    let write = state.db.write_conn();
    // 单语句自增并取回：原来是 SELECT + UPDATE 两次锁往返，RETURNING 把读
    // 合并进写语句（rusqlite 的 execute 不接受返回行，这里用 query_row）。
    let failures: Option<i64> = write
        .query_row(
            "UPDATE credential_members SET failure_count = failure_count + 1 WHERE id = ?1 RETURNING failure_count",
            [member_id],
            |row| row.get(0),
        )
        .ok();
    let Some(failures) = failures else {
        // 成员已不存在（并发删除），无事可做。
        return;
    };
    let cooling = hard || failures >= threshold.max(1);
    let mut cooling_until: Option<String> = None;
    if cooling {
        let until = chrono::Local::now()
            + chrono::Duration::seconds(if hard { 300 } else { cooldown.max(1) });
        cooling_until = Some(until.to_rfc3339());
        let _ = write.execute(
            "UPDATE credential_members SET status = 'cooling', cooldown_until = ?2 WHERE id = ?1",
            rusqlite::params![member_id, cooling_until],
        );
    }
    drop(write);
    let status = if cooling { "cooling" } else { "active" };
    cache_touch(state, member_id, |member| {
        member.failure_count = failures;
        member.status = status.into();
        member.cooldown_until = cooling_until.clone();
    });
}

/// 新增成员：密钥只写本机加密库，DB 只存引用。
pub fn add_member(
    state: &AppState,
    pool_id: &str,
    label: &str,
    kind: &str,
    api_key: Option<&str>,
    secret_ref: Option<&str>,
    priority: i64,
    weight: i64,
    quota: Option<Value>,
) -> Result<String, String> {
    let member_id = new_member_id();
    let ref_id = if let Some(key) = api_key.filter(|value| !value.trim().is_empty()) {
        let store_id = format!("pool:{pool_id}:{member_id}");
        let mut store = crate::secret_store::SecretStore::load(&state.config_path);
        store
            .set(&store_id, key)
            .map_err(|err| format!("密钥保存失败: {err}"))?;
        format!("store:{store_id}")
    } else if let Some(secret_ref) = secret_ref.filter(|value| !value.trim().is_empty()) {
        secret_ref.to_string()
    } else {
        return Err("缺少 apiKey（或 secretRef）".into());
    };
    let conn = state.db.write_conn();
    conn.execute(
        "INSERT INTO credential_members (id, pool_id, label, kind, secret_ref, priority, weight, status, quota_json, usage_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9)",
        rusqlite::params![
            member_id,
            pool_id,
            if label.trim().is_empty() { "成员" } else { label },
            kind,
            ref_id,
            priority,
            weight.max(1),
            quota.map(|value| value.to_string()),
            json!({"dailyTokens": 0, "dailyRequests": 0}).to_string(),
        ],
    )
    .map_err(|err| format!("成员写入失败: {err}"))?;
    drop(conn);
    invalidate_pool_cache(state);
    Ok(member_id)
}

pub fn create_pool(
    state: &AppState,
    id: &str,
    upstream: &str,
    strategy: &str,
    cooldown: i64,
    threshold: i64,
    plugin_id: Option<&str>,
) -> Result<(), String> {
    let now = now_text();
    let conn = state.db.write_conn();
    conn.execute(
        "INSERT INTO credential_pools (id, upstream, plugin_id, strategy, status, cooldown_seconds, failure_threshold, fallback_to_single, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, 0, ?7, ?7)",
        rusqlite::params![id, upstream, plugin_id, strategy, cooldown, threshold, now],
    )
    .map_err(|err| format!("池创建失败: {err}"))?;
    drop(conn);
    invalidate_pool_cache(state);
    Ok(())
}

pub fn record_audit(
    state: &AppState,
    actor: &str,
    action: &str,
    plan_id: Option<&str>,
    target: &str,
    changed_paths: &[String],
    result: &str,
    backup_id: Option<&str>,
) -> String {
    let id = format!(
        "audit_{}_{}",
        now_millis(),
        MEMBER_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let conn = state.db.write_conn();
    let _ = conn.execute(
        "INSERT INTO config_audit (id, at, actor, action, plan_id, target, changed_paths, result, backup_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            id,
            now_text(),
            actor,
            action,
            plan_id,
            target,
            serde_json::to_string(changed_paths).unwrap_or_else(|_| "[]".into()),
            result,
            backup_id,
        ],
    );
    id
}

// ─────────────────────────── HTTP API ───────────────────────────

fn pool_json(state: &AppState, pool: &Pool) -> Value {
    let summary = summary(state, pool);
    let mut value = summary.to_json(pool);
    value["fallbackToSingle"] = json!(pool.fallback_to_single);
    value["pluginId"] = json!(pool.plugin_id);
    value["cooldownSeconds"] = json!(pool.cooldown_seconds);
    value["failureThreshold"] = json!(pool.failure_threshold);
    value
}

fn pool_error(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    error_json(status, message, "invalid_request_error", code)
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    recover_cooldowns(&state);
    let items: Vec<Value> = list_pool_ids(&state)
        .iter()
        .filter_map(|id| get_pool(&state, id))
        .map(|pool| pool_json(&state, &pool))
        .collect();
    Json(json!({ "items": items, "total": items.len() })).into_response()
}

pub async fn create(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let upstream = body
        .get("upstream")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if upstream.is_empty() || state.cfg().upstream(upstream).is_none() {
        return pool_error(
            StatusCode::BAD_REQUEST,
            format!("上游「{upstream}」不存在"),
            "unknown_upstream",
        );
    }
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("pool_{}_{}", upstream, now_millis()));
    if !valid_id(&id) {
        return pool_error(StatusCode::BAD_REQUEST, "池 id 不合法", "invalid_id");
    }
    if get_pool(&state, &id).is_some() {
        return pool_error(
            StatusCode::CONFLICT,
            format!("池「{id}」已存在"),
            "pool_exists",
        );
    }
    let strategy = body
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("priority");
    if !matches!(
        strategy,
        "priority" | "round_robin" | "weighted" | "least_used"
    ) {
        return pool_error(StatusCode::BAD_REQUEST, "未知策略", "invalid_strategy");
    }
    let cooldown = body
        .get("cooldownSeconds")
        .and_then(Value::as_i64)
        .unwrap_or(60)
        .clamp(1, 3600);
    let threshold = body
        .get("failureThreshold")
        .and_then(Value::as_i64)
        .unwrap_or(3)
        .clamp(1, 100);
    if let Err(err) = create_pool(&state, &id, upstream, strategy, cooldown, threshold, None) {
        return pool_error(StatusCode::INTERNAL_SERVER_ERROR, err, "pool_write_failed");
    }
    let pool = get_pool(&state, &id);
    Json(json!({
        "ok": true,
        "pool": pool.as_ref().map(|pool| pool_json(&state, pool)),
    }))
    .into_response()
}

pub async fn detail(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    recover_cooldowns(&state);
    let Some(pool) = get_pool(&state, &id) else {
        return pool_error(
            StatusCode::NOT_FOUND,
            format!("池「{id}」不存在"),
            "pool_not_found",
        );
    };
    let mut value = pool_json(&state, &pool);
    value["memberList"] = json!(members(&state, &id)
        .iter()
        .map(member_json)
        .collect::<Vec<_>>());
    Json(json!({ "pool": value })).into_response()
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if get_pool(&state, &id).is_none() {
        return pool_error(
            StatusCode::NOT_FOUND,
            format!("池「{id}」不存在"),
            "pool_not_found",
        );
    }
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(strategy) = body.get("strategy").and_then(Value::as_str) {
        if !matches!(
            strategy,
            "priority" | "round_robin" | "weighted" | "least_used"
        ) {
            return pool_error(StatusCode::BAD_REQUEST, "未知策略", "invalid_strategy");
        }
        sets.push(format!("strategy = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(strategy.to_string()));
    }
    if let Some(status) = body.get("status").and_then(Value::as_str) {
        if !matches!(status, "active" | "disabled") {
            return pool_error(StatusCode::BAD_REQUEST, "未知状态", "invalid_status");
        }
        sets.push(format!("status = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(status.to_string()));
    }
    for (key, column) in [
        ("cooldownSeconds", "cooldown_seconds"),
        ("failureThreshold", "failure_threshold"),
    ] {
        if let Some(value) = body.get(key).and_then(Value::as_i64) {
            sets.push(format!("{column} = ?{}", params.len() + 1));
            params.push(rusqlite::types::Value::Integer(value));
        }
    }
    if let Some(value) = body.get("fallbackToSingle").and_then(Value::as_bool) {
        sets.push(format!("fallback_to_single = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Integer(if value { 1 } else { 0 }));
    }
    if sets.is_empty() {
        return pool_error(StatusCode::BAD_REQUEST, "没有可更新字段", "empty_patch");
    }
    sets.push(format!("updated_at = ?{}", params.len() + 1));
    params.push(rusqlite::types::Value::Text(now_text()));
    let sql = format!(
        "UPDATE credential_pools SET {} WHERE id = ?{}",
        sets.join(", "),
        params.len() + 1
    );
    params.push(rusqlite::types::Value::Text(id.clone()));
    let conn = state.db.write_conn();
    if let Err(err) = conn.execute(&sql, rusqlite::params_from_iter(params)) {
        return pool_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新失败: {err}"),
            "pool_write_failed",
        );
    }
    drop(conn);
    invalidate_pool_cache(&state);
    let pool = get_pool(&state, &id);
    Json(json!({ "ok": true, "pool": pool.as_ref().map(|pool| pool_json(&state, pool)) }))
        .into_response()
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if get_pool(&state, &id).is_none() {
        return pool_error(
            StatusCode::NOT_FOUND,
            format!("池「{id}」不存在"),
            "pool_not_found",
        );
    }
    let conn = state.db.write_conn();
    let _ = conn.execute(
        "UPDATE credential_pools SET status = 'disabled', updated_at = ?2 WHERE id = ?1",
        rusqlite::params![id, now_text()],
    );
    drop(conn);
    invalidate_pool_cache(&state);
    Json(json!({ "ok": true, "disabled": id })).into_response()
}

pub async fn add_member_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if get_pool(&state, &id).is_none() {
        return pool_error(
            StatusCode::NOT_FOUND,
            format!("池「{id}」不存在"),
            "pool_not_found",
        );
    }
    let label = body.get("label").and_then(Value::as_str).unwrap_or("成员");
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("api_key");
    if !matches!(kind, "api_key" | "account" | "oauth") {
        return pool_error(StatusCode::BAD_REQUEST, "未知成员类型", "invalid_kind");
    }
    let api_key = body.get("apiKey").and_then(Value::as_str);
    let secret_ref = body.get("secretRef").and_then(Value::as_str);
    let priority = body.get("priority").and_then(Value::as_i64).unwrap_or(100);
    let weight = body.get("weight").and_then(Value::as_i64).unwrap_or(1);
    let quota = body.get("quota").cloned();
    match add_member(
        &state, &id, label, kind, api_key, secret_ref, priority, weight, quota,
    ) {
        Ok(member_id) => Json(json!({ "ok": true, "memberId": member_id })).into_response(),
        Err(err) => pool_error(StatusCode::BAD_REQUEST, err, "member_write_failed"),
    }
}

pub async fn update_member(
    State(state): State<Arc<AppState>>,
    Path((id, member_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(label) = body.get("label").and_then(Value::as_str) {
        sets.push(format!("label = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(label.to_string()));
    }
    if let Some(status) = body.get("status").and_then(Value::as_str) {
        if !matches!(status, "active" | "disabled") {
            return pool_error(StatusCode::BAD_REQUEST, "未知状态", "invalid_status");
        }
        sets.push(format!("status = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(status.to_string()));
    }
    for (key, column) in [("priority", "priority"), ("weight", "weight")] {
        if let Some(value) = body.get(key).and_then(Value::as_i64) {
            sets.push(format!("{column} = ?{}", params.len() + 1));
            params.push(rusqlite::types::Value::Integer(value));
        }
    }
    if let Some(quota) = body.get("quota") {
        sets.push(format!("quota_json = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(quota.to_string()));
    }
    if sets.is_empty() {
        return pool_error(StatusCode::BAD_REQUEST, "没有可更新字段", "empty_patch");
    }
    let sql = format!(
        "UPDATE credential_members SET {} WHERE id = ?{} AND pool_id = ?{}",
        sets.join(", "),
        params.len() + 1,
        params.len() + 2
    );
    params.push(rusqlite::types::Value::Text(member_id.clone()));
    params.push(rusqlite::types::Value::Text(id));
    let conn = state.db.write_conn();
    match conn.execute(&sql, rusqlite::params_from_iter(params)) {
        Ok(0) => pool_error(StatusCode::NOT_FOUND, "成员不存在", "member_not_found"),
        Ok(_) => {
            drop(conn);
            invalidate_pool_cache(&state);
            Json(json!({ "ok": true, "memberId": member_id })).into_response()
        }
        Err(err) => pool_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新失败: {err}"),
            "member_write_failed",
        ),
    }
}

pub async fn delete_member(
    State(state): State<Arc<AppState>>,
    Path((id, member_id)): Path<(String, String)>,
) -> Response {
    let conn = state.db.write_conn();
    let _ = conn.execute(
        "UPDATE credential_members SET status = 'disabled' WHERE id = ?1 AND pool_id = ?2",
        rusqlite::params![member_id, id],
    );
    drop(conn);
    invalidate_pool_cache(&state);
    Json(json!({ "ok": true, "disabled": member_id })).into_response()
}

/// 逐个成员探测（脱敏）：只验证该成员的密钥能否拉取模型目录。
pub async fn probe(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(pool) = get_pool(&state, &id) else {
        return pool_error(
            StatusCode::NOT_FOUND,
            format!("池「{id}」不存在"),
            "pool_not_found",
        );
    };
    let Some(up) = state.cfg().upstream(&pool.upstream).cloned() else {
        return pool_error(
            StatusCode::BAD_REQUEST,
            "池绑定的上游不存在",
            "unknown_upstream",
        );
    };
    let mut out = Vec::new();
    for member in members(&state, &id) {
        let key = resolve_secret(&state, &member.secret_ref);
        let Some(key) = key else {
            out.push(json!({
                "memberId": member.id,
                "label": member.label,
                "ok": false,
                "message": "缺少可用密钥",
            }));
            continue;
        };
        let url = format!(
            "{}{}",
            up.base_url.trim_end_matches('/'),
            up.compat.models_path
        );
        let mut headers = reqwest::header::HeaderMap::new();
        crate::proxy::apply_auth_headers(&mut headers, &up, &key);
        let started = std::time::Instant::now();
        let result = state
            .client
            .get(&url)
            .headers(headers)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await;
        let latency = started.elapsed().as_millis() as u64;
        match result {
            Ok(response) => out.push(json!({
                "memberId": member.id,
                "label": member.label,
                "ok": response.status().is_success(),
                "status": response.status().as_u16(),
                "latencyMs": latency,
            })),
            Err(err) => out.push(json!({
                "memberId": member.id,
                "label": member.label,
                "ok": false,
                "latencyMs": latency,
                "message": err.to_string(),
            })),
        }
    }
    Json(json!({ "pool": id, "members": out })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_url_safe() {
        assert!(valid_id("pool_deepseek_1"));
        assert!(!valid_id(""));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a b"));
    }

    #[test]
    fn member_ids_are_unique() {
        let a = new_member_id();
        let b = new_member_id();
        assert_ne!(a, b);
        assert!(a.starts_with("pm_"));
    }

    #[test]
    fn quota_numbers_tolerate_bad_json() {
        let member = Member {
            id: "m".into(),
            pool_id: "p".into(),
            label: "l".into(),
            kind: "api_key".into(),
            secret_ref: "store:x".into(),
            priority: 100,
            weight: 1,
            status: "active".into(),
            cooldown_until: None,
            failure_count: 0,
            quota_json: Some("not-json".into()),
            last_used_at: None,
            usage_json: None,
        };
        assert_eq!(quota_numbers(&member), (0, 0));
    }

    #[cfg(windows)]
    #[test]
    fn secrets_cache_reloads_on_mtime_change() {
        let dir = std::env::temp_dir().join(format!(
            "mg-pool-secrets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("bridge.config.json");
        let mut cache = SecretsCache::default();
        // 空库 → 未命中。
        assert_eq!(cache.lookup(&config, "upstream:u"), None);
        {
            let mut store = crate::secret_store::SecretStore::load(&config);
            store.set("upstream:u", "sk-test-1").unwrap();
        }
        assert_eq!(
            cache.lookup(&config, "upstream:u"),
            Some("sk-test-1".into())
        );
        // 文件被改写（面板保存新密钥）→ mtime 变化必须重载，不能永远拿旧值。
        std::thread::sleep(std::time::Duration::from_millis(150));
        {
            let mut store = crate::secret_store::SecretStore::load(&config);
            store.set("upstream:u", "sk-test-2").unwrap();
        }
        assert_eq!(
            cache.lookup(&config, "upstream:u"),
            Some("sk-test-2".into()),
            "mtime 变化必须重载解密缓存"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
