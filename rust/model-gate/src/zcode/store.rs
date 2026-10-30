//! ZCode 账号存储：Model-Gate SQLite 的唯一账号事实源。
//!
//! 账号只存密文（见 `secret.rs`）；`secret_fp` 是明文的 SHA-256 指纹，用于去重，
//! 不可逆。额度/套餐/指纹以 JSON 文本保存，UI 直接展示。

use rusqlite::{params, OptionalExtension, Row};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::db::Db;

use super::secret;

pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_EXHAUSTED: &str = "exhausted";
pub const STATUS_COOLING: &str = "cooling";
pub const STATUS_INVALID: &str = "invalid";
pub const STATUS_DISABLED: &str = "disabled";

#[derive(Debug, Clone)]
pub struct ZcodeAccount {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub mode: String,
    pub secret_enc: Vec<u8>,
    pub secret_fp: String,
    pub status: String,
    pub enabled: bool,
    pub quota_json: Option<String>,
    pub plan_json: Option<String>,
    pub fingerprint_json: Option<String>,
    pub created_at: i64,
    pub last_checked_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub cooling_until: Option<i64>,
    pub last_error: Option<String>,
    pub use_count: i64,
    pub fail_count: i64,
}

impl ZcodeAccount {
    /// 新建账号：加密明文并生成不可逆指纹。
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        mode: impl Into<String>,
        plain_secret: &str,
    ) -> Result<Self, String> {
        let plain = plain_secret.trim();
        if plain.is_empty() {
            return Err("凭据不能为空".to_string());
        }
        let secret_enc = secret::seal(plain.as_bytes())?;
        let secret_fp = fingerprint(plain);
        Ok(Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.into(),
            provider: provider.into(),
            mode: mode.into(),
            secret_enc,
            secret_fp,
            status: STATUS_ACTIVE.to_string(),
            enabled: true,
            quota_json: None,
            plan_json: None,
            fingerprint_json: None,
            created_at: now_ms(),
            last_checked_at: None,
            last_used_at: None,
            cooling_until: None,
            last_error: None,
            use_count: 0,
            fail_count: 0,
        })
    }

    /// 解密明文（只应在本机进程内短时使用，不写日志/不回显）。
    pub fn plain_secret(&self) -> Result<String, String> {
        let bytes = secret::open(&self.secret_enc)?;
        String::from_utf8(bytes).map_err(|_| "凭据不是 UTF-8".to_string())
    }

    /// 面板视图：只返回掩码/长度，不返回明文。
    pub fn public_view(&self) -> Value {
        let total = self.use_count;
        let fail = self.fail_count;
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "provider": self.provider,
            "mode": self.mode,
            "status": self.status,
            "enabled": self.enabled,
            "quota": self.quota_json.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            "plan": self.plan_json.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            "fingerprint": self.fingerprint_json.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            "createdAt": self.created_at,
            "lastCheckedAt": self.last_checked_at,
            "lastUsedAt": self.last_used_at,
            "coolingUntil": self.cooling_until,
            "lastError": self.last_error,
            "useCount": total,
            "failCount": fail,
            "secretMasked": mask_secret(&self.secret_fp),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ZcodeClaim {
    pub id: String,
    pub account_id: String,
    pub plan_id: Option<String>,
    pub plan_name: Option<String>,
    pub code: Option<i64>,
    pub ok: bool,
    pub next_at: Option<i64>,
    pub message: Option<String>,
    pub created_at: i64,
}

impl ZcodeClaim {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "accountId": self.account_id,
            "planId": self.plan_id,
            "planName": self.plan_name,
            "code": self.code,
            "ok": self.ok,
            "nextAt": self.next_at,
            "message": self.message,
            "createdAt": self.created_at,
        })
    }
}

impl Db {
    pub fn zcode_list_accounts(&self) -> Vec<ZcodeAccount> {
        let conn = self.read_conn();
        let mut stmt = match conn.prepare(
            "SELECT id,name,provider,mode,secret_enc,secret_fp,status,enabled,quota_json,plan_json,\
             fingerprint_json,created_at,last_checked_at,last_used_at,cooling_until,last_error,use_count,fail_count \
             FROM zcode_accounts ORDER BY created_at ASC",
        ) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("查询 ZCode 账号失败：{err}");
                return Vec::new();
            }
        };
        let rows = stmt.query_map([], row_to_account);
        match rows {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(err) => {
                tracing::warn!("读取 ZCode 账号失败：{err}");
                Vec::new()
            }
        }
    }

    pub fn zcode_get_account(&self, id: &str) -> Option<ZcodeAccount> {
        let conn = self.read_conn();
        conn.query_row(
            "SELECT id,name,provider,mode,secret_enc,secret_fp,status,enabled,quota_json,plan_json,\
             fingerprint_json,created_at,last_checked_at,last_used_at,cooling_until,last_error,use_count,fail_count \
             FROM zcode_accounts WHERE id = ?1",
            params![id],
            row_to_account,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn zcode_find_by_fp(&self, fp: &str) -> Option<ZcodeAccount> {
        let conn = self.read_conn();
        conn.query_row(
            "SELECT id,name,provider,mode,secret_enc,secret_fp,status,enabled,quota_json,plan_json,\
             fingerprint_json,created_at,last_checked_at,last_used_at,cooling_until,last_error,use_count,fail_count \
             FROM zcode_accounts WHERE secret_fp = ?1",
            params![fp],
            row_to_account,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 新建或按 id 更新。重复 secret_fp 由唯一索引拒绝，调用方转成业务错误。
    pub fn zcode_upsert_account(&self, account: &ZcodeAccount) -> Result<(), rusqlite::Error> {
        let conn = self.write_conn();
        conn.execute(
            "INSERT INTO zcode_accounts \
             (id,name,provider,mode,secret_enc,secret_fp,status,enabled,quota_json,plan_json,fingerprint_json,\
              created_at,last_checked_at,last_used_at,cooling_until,last_error,use_count,fail_count) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18) \
             ON CONFLICT(id) DO UPDATE SET \
              name=excluded.name, provider=excluded.provider, mode=excluded.mode, \
              secret_enc=excluded.secret_enc, secret_fp=excluded.secret_fp, \
              status=excluded.status, enabled=excluded.enabled, quota_json=excluded.quota_json, \
              plan_json=excluded.plan_json, fingerprint_json=excluded.fingerprint_json, \
              last_checked_at=excluded.last_checked_at, last_used_at=excluded.last_used_at, \
              cooling_until=excluded.cooling_until, last_error=excluded.last_error, \
              use_count=excluded.use_count, fail_count=excluded.fail_count",
            params![
                account.id,
                account.name,
                account.provider,
                account.mode,
                account.secret_enc,
                account.secret_fp,
                account.status,
                account.enabled as i64,
                account.quota_json,
                account.plan_json,
                account.fingerprint_json,
                account.created_at,
                account.last_checked_at,
                account.last_used_at,
                account.cooling_until,
                account.last_error,
                account.use_count,
                account.fail_count,
            ],
        )?;
        Ok(())
    }

    pub fn zcode_delete_accounts(&self, ids: &[String]) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let conn = self.write_conn();
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("DELETE FROM zcode_accounts WHERE id IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(ids.iter()))
            .unwrap_or(0)
    }

    pub fn zcode_set_enabled(&self, id: &str, enabled: bool) -> bool {
        let conn = self.write_conn();
        conn.execute(
            "UPDATE zcode_accounts SET enabled=?1, status=CASE WHEN ?1=0 THEN 'disabled' ELSE status END WHERE id=?2",
            params![enabled as i64, id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn zcode_set_status(
        &self,
        id: &str,
        status: &str,
        last_error: Option<&str>,
        cooling_until: Option<i64>,
    ) -> bool {
        let conn = self.write_conn();
        conn.execute(
            "UPDATE zcode_accounts SET status=?1, last_error=?2, cooling_until=?3, last_checked_at=?4 WHERE id=?5",
            params![status, last_error, cooling_until, now_ms(), id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn zcode_touch_used(&self, id: &str, ok: bool) {
        let conn = self.write_conn();
        let _ = conn.execute(
            "UPDATE zcode_accounts SET last_used_at=?1, use_count=use_count+1, fail_count=fail_count+?2 WHERE id=?3",
            params![now_ms(), if ok { 0 } else { 1 }, id],
        );
    }

    /// 账号池选择：启用、active、JWT、未冷却，按「最久未用 + 使用次数少」轮询。
    /// 池规模很小（个人账号池），在内存里筛选/排序比拼接动态 SQL 更直观。
    pub fn zcode_pick_account(&self, exclude: &[String]) -> Option<ZcodeAccount> {
        let now = now_ms();
        let mut candidates: Vec<ZcodeAccount> = self
            .zcode_list_accounts()
            .into_iter()
            .filter(|a| a.enabled)
            .filter(|a| a.status == STATUS_ACTIVE)
            .filter(|a| a.mode == "jwt")
            .filter(|a| a.cooling_until.map_or(true, |until| until <= now))
            .filter(|a| !exclude.iter().any(|id| id == &a.id))
            .collect();
        candidates.sort_by_key(|a| (a.last_used_at.unwrap_or(0), a.use_count, a.created_at));
        candidates.into_iter().next()
    }

    /// 更新额度/套餐展示与状态；`None` 表示保持原值。
    pub fn zcode_update_quota(
        &self,
        id: &str,
        plan_json: Option<&str>,
        quota_json: Option<&str>,
        status: Option<&str>,
        last_error: Option<&str>,
        cooling_until: Option<i64>,
    ) -> bool {
        let conn = self.write_conn();
        conn.execute(
            "UPDATE zcode_accounts SET \
             plan_json=COALESCE(?1, plan_json), quota_json=COALESCE(?2, quota_json), \
             status=COALESCE(?3, status), last_error=?4, cooling_until=?5, last_checked_at=?6 \
             WHERE id=?7",
            params![
                plan_json,
                quota_json,
                status,
                last_error,
                cooling_until,
                now_ms(),
                id
            ],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// 某账号最近一次领取记录（用于 claim 间隔/next_at 判断）。
    pub fn zcode_last_claim(&self, account_id: &str) -> Option<ZcodeClaim> {
        let conn = self.read_conn();
        conn.query_row(
            "SELECT id,account_id,plan_id,plan_name,code,ok,next_at,message,created_at \
             FROM zcode_claims WHERE account_id=?1 ORDER BY created_at DESC LIMIT 1",
            params![account_id],
            |row| {
                Ok(ZcodeClaim {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    plan_id: row.get(2)?,
                    plan_name: row.get(3)?,
                    code: row.get(4)?,
                    ok: row.get::<_, i64>(5)? != 0,
                    next_at: row.get(6)?,
                    message: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn zcode_record_claim(&self, claim: &ZcodeClaim) {
        let conn = self.write_conn();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO zcode_claims \
             (id,account_id,plan_id,plan_name,code,ok,next_at,message,created_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                claim.id,
                claim.account_id,
                claim.plan_id,
                claim.plan_name,
                claim.code,
                claim.ok as i64,
                claim.next_at,
                claim.message,
                claim.created_at
            ],
        );
    }

    pub fn zcode_recent_claims(&self, limit: i64) -> Vec<ZcodeClaim> {
        let conn = self.read_conn();
        let mut stmt = match conn.prepare(
            "SELECT id,account_id,plan_id,plan_name,code,ok,next_at,message,created_at \
             FROM zcode_claims ORDER BY created_at DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![limit.clamp(1, 200)], |row| {
            Ok(ZcodeClaim {
                id: row.get(0)?,
                account_id: row.get(1)?,
                plan_id: row.get(2)?,
                plan_name: row.get(3)?,
                code: row.get(4)?,
                ok: row.get::<_, i64>(5)? != 0,
                next_at: row.get(6)?,
                message: row.get(7)?,
                created_at: row.get(8)?,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<ZcodeAccount> {
    Ok(ZcodeAccount {
        id: row.get(0)?,
        name: row.get(1)?,
        provider: row.get(2)?,
        mode: row.get(3)?,
        secret_enc: row.get(4)?,
        secret_fp: row.get(5)?,
        status: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        quota_json: row.get(8)?,
        plan_json: row.get(9)?,
        fingerprint_json: row.get(10)?,
        created_at: row.get(11)?,
        last_checked_at: row.get(12)?,
        last_used_at: row.get(13)?,
        cooling_until: row.get(14)?,
        last_error: row.get(15)?,
        use_count: row.get(16)?,
        fail_count: row.get(17)?,
    })
}

pub fn fingerprint(plain: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plain.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn mask_secret(fp: &str) -> String {
    if fp.len() <= 12 {
        "••••".to_string()
    } else {
        format!("{}…{}", &fp[..6], &fp[fp.len() - 4..])
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_new_seals_and_masks() {
        let acc = ZcodeAccount::new("test", "zai", "jwt", "a.b.c").expect("account");
        assert_eq!(acc.mode, "jwt");
        assert!(acc.secret_enc.starts_with(b"dpapi:v1:"));
        assert_eq!(acc.plain_secret().expect("open"), "a.b.c");
        let view = acc.public_view().to_string();
        assert!(!view.contains("a.b.c"));
        assert!(view.contains("secretMasked"));
    }

    #[test]
    fn same_secret_has_same_fingerprint() {
        assert_eq!(fingerprint("x"), fingerprint("x"));
        assert_ne!(fingerprint("x"), fingerprint("y"));
    }
}
