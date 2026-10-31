//! ZCode 领取（claim）与账号池调度。
//!
//! 流程与 `zcode2api` 对齐：preview 可领套餐 → 取验证码 → claim → 按业务码
//! 更新账号状态/冷却/额度。所有状态落 Model-Gate SQLite，不复制账号。
//!
//! 安全：报告只含业务码/文案，不含 JWT 或验证码原文。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::proxy::AppState;

use super::billing::{self, BillingError, ClaimOutcome, Plan};
use super::store::{
    ZcodeAccount, ZcodeClaim, STATUS_ACTIVE, STATUS_COOLING, STATUS_DISABLED, STATUS_EXHAUSTED,
    STATUS_INVALID,
};

/// 后台调度检查间隔：30 秒一次；实际 claim 由 `claimIntervalSeconds` 控制。
const SCHEDULER_TICK: Duration = Duration::from_secs(30);
/// 启动后延迟，避免和首次面板加载/服务启动争资源。
const SCHEDULER_START_DELAY: Duration = Duration::from_secs(30);
/// 网络/5xx 失败后的短冷却：不把账号标 invalid，也不立刻重复打上游。
const TRANSIENT_COOLDOWN_MS: i64 = 60_000;
/// 1005（今日名额用完）的默认下次检查时间：1 小时。
const QUOTA_RESET_MS: i64 = 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct ClaimReport {
    pub account_id: String,
    pub account_name: String,
    pub plan_id: Option<String>,
    pub plan_name: Option<String>,
    pub ok: bool,
    pub code: Option<i64>,
    pub message: Option<String>,
    pub next_at: Option<i64>,
    pub status: String,
}

impl ClaimReport {
    pub fn to_json(&self) -> Value {
        json!({
            "accountId": self.account_id,
            "accountName": self.account_name,
            "planId": self.plan_id,
            "planName": self.plan_name,
            "ok": self.ok,
            "code": self.code,
            "message": self.message,
            "nextAt": self.next_at,
            "status": self.status,
        })
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn plan_json(plan: &Plan) -> Value {
    json!({
        "planId": plan.plan_id,
        "name": plan.name,
        "priority": plan.priority,
        "grants": plan.grants,
    })
}

fn plans_json(plans: &[Plan]) -> Value {
    json!({
        "plans": plans.iter().map(plan_json).collect::<Vec<_>>(),
        "checkedAt": now_ms(),
    })
}

/// 拉取单个账号的可领套餐并回写展示数据；不做 claim。
pub async fn preview_account(
    state: &AppState,
    account: &ZcodeAccount,
) -> Result<Vec<Plan>, String> {
    match billing::preview(&state.client, account).await {
        Ok(plans) => {
            let quota = plans_json(&plans).to_string();
            let best = plans
                .iter()
                .max_by_key(|p| p.priority)
                .map(plan_json)
                .map(|v| v.to_string());
            state.db.zcode_update_quota(
                &account.id,
                best.as_deref(),
                Some(&quota),
                Some(STATUS_ACTIVE),
                None,
                None,
            );
            Ok(plans)
        }
        Err(BillingError::Auth(message)) => {
            state.db.zcode_update_quota(
                &account.id,
                None,
                None,
                Some(STATUS_INVALID),
                Some(&message),
                None,
            );
            Err(message)
        }
        Err(err) => {
            let message = err.message();
            state
                .db
                .zcode_update_quota(&account.id, None, None, None, Some(&message), None);
            Err(message)
        }
    }
}

/// 对单个账号执行一次 claim（先 preview，再按需取验证码）。
pub async fn claim_account(state: &AppState, account: &ZcodeAccount) -> ClaimReport {
    let mut report = ClaimReport {
        account_id: account.id.clone(),
        account_name: account.name.clone(),
        plan_id: None,
        plan_name: None,
        ok: false,
        code: None,
        message: None,
        next_at: None,
        status: account.status.clone(),
    };
    let plans = match billing::preview(&state.client, account).await {
        Ok(plans) => plans,
        Err(BillingError::Auth(message)) => {
            state.db.zcode_update_quota(
                &account.id,
                None,
                None,
                Some(STATUS_INVALID),
                Some(&message),
                None,
            );
            report.message = Some(message);
            report.status = STATUS_INVALID.to_string();
            return report;
        }
        Err(err) => {
            let message = err.message();
            state.db.zcode_update_quota(
                &account.id,
                None,
                None,
                Some(STATUS_COOLING),
                Some(&message),
                Some(now_ms() + TRANSIENT_COOLDOWN_MS),
            );
            report.message = Some(message);
            report.status = STATUS_COOLING.to_string();
            return report;
        }
    };
    let best = plans.iter().max_by_key(|p| p.priority).cloned();
    let quota = plans_json(&plans).to_string();
    let Some(best) = best else {
        let message = "当前没有可领取的套餐".to_string();
        state.db.zcode_update_quota(
            &account.id,
            None,
            Some(&quota),
            Some(account.status.as_str()),
            None,
            None,
        );
        report.message = Some(message);
        return report;
    };
    report.plan_id = Some(best.plan_id.clone());
    report.plan_name = Some(best.name.clone());

    let captcha = match state
        .zcode
        .captcha
        .acquire(state.zcode.captcha_solver())
        .await
    {
        Ok(token) => token,
        Err(message) => {
            let message = format!("验证码不可用：{message}");
            state.db.zcode_update_quota(
                &account.id,
                Some(&plan_json(&best).to_string()),
                Some(&quota),
                None,
                Some(&message),
                None,
            );
            report.message = Some(message);
            return report;
        }
    };

    let outcome = match billing::claim(&state.client, account, &captcha, Some(&best.plan_id)).await
    {
        Ok(outcome) => outcome,
        Err(BillingError::Auth(message)) => {
            state.db.zcode_update_quota(
                &account.id,
                Some(&plan_json(&best).to_string()),
                Some(&quota),
                Some(STATUS_INVALID),
                Some(&message),
                None,
            );
            report.message = Some(message);
            report.status = STATUS_INVALID.to_string();
            return report;
        }
        Err(BillingError::Business {
            code,
            message,
            next_at,
        }) => ClaimOutcome {
            plan_id: best.plan_id.clone(),
            plan_name: best.name.clone(),
            code: Some(code),
            ok: false,
            next_at,
            message: Some(message),
            starts_at: None,
            ends_at: None,
        },
        Err(err) => {
            let message = err.message();
            state.db.zcode_update_quota(
                &account.id,
                Some(&plan_json(&best).to_string()),
                Some(&quota),
                Some(STATUS_COOLING),
                Some(&message),
                Some(now_ms() + TRANSIENT_COOLDOWN_MS),
            );
            report.message = Some(message);
            report.status = STATUS_COOLING.to_string();
            return report;
        }
    };

    let status = billing::status_after_claim(account, &outcome);
    if outcome.code == Some(3007) {
        state.zcode.captcha.invalidate_all();
    }
    let next_at = outcome.next_at.or_else(|| match outcome.code {
        Some(1005) => Some(now_ms() + QUOTA_RESET_MS),
        _ => None,
    });
    let cooling = if status == STATUS_COOLING {
        Some(now_ms() + TRANSIENT_COOLDOWN_MS)
    } else {
        None
    };
    state.db.zcode_update_quota(
        &account.id,
        Some(&plan_json(&best).to_string()),
        Some(&quota),
        Some(&status),
        outcome.message.as_deref(),
        cooling,
    );
    let claim = ZcodeClaim {
        id: uuid::Uuid::new_v4().simple().to_string(),
        account_id: account.id.clone(),
        plan_id: Some(outcome.plan_id.clone()),
        plan_name: Some(outcome.plan_name.clone()),
        code: outcome.code,
        ok: outcome.ok,
        next_at,
        message: outcome.message.clone(),
        created_at: now_ms(),
    };
    state.db.zcode_record_claim(&claim);

    report.ok = outcome.ok;
    report.code = outcome.code;
    report.message = outcome.message;
    report.next_at = next_at;
    report.status = status;
    report
}

/// 判断账号是否到了下一次 claim 时间。
fn claim_due(state: &AppState, account: &ZcodeAccount, interval_seconds: u64) -> bool {
    let now = now_ms();
    if account.status == STATUS_DISABLED || account.status == STATUS_INVALID {
        return false;
    }
    if account.cooling_until.map_or(false, |until| until > now) {
        return false;
    }
    if let Some(last) = state.db.zcode_last_claim(&account.id) {
        if last.next_at.map_or(false, |next| next > now) {
            return false;
        }
        if interval_seconds > 0 && last.created_at + (interval_seconds as i64) * 1000 > now {
            return false;
        }
    }
    true
}

/// 领取全部（或指定）账号；`force=true` 表示手动触发，忽略间隔但仍尊重 next_at。
pub async fn claim_all(
    state: &AppState,
    account_id: Option<&str>,
    force: bool,
) -> Result<Vec<ClaimReport>, String> {
    let interval = state.zcode.config().claim_interval_seconds;
    let mut accounts: Vec<ZcodeAccount> = state.db.zcode_list_accounts();
    if let Some(id) = account_id {
        accounts.retain(|a| a.id == id);
        if accounts.is_empty() {
            return Err("ZCode 账号不存在".to_string());
        }
    } else {
        accounts.retain(|a| a.enabled && a.mode == "jwt");
        accounts.retain(|a| {
            a.status == STATUS_ACTIVE || a.status == STATUS_EXHAUSTED || a.status == STATUS_COOLING
        });
        if !force {
            accounts.retain(|a| claim_due(state, a, interval));
        } else {
            let now = now_ms();
            accounts.retain(|a| a.cooling_until.map_or(true, |until| until <= now));
            accounts.retain(|a| {
                state
                    .db
                    .zcode_last_claim(&a.id)
                    .and_then(|last| last.next_at)
                    .map_or(true, |next| next <= now)
            });
        }
    }

    let mut reports = Vec::new();
    for account in accounts {
        reports.push(claim_account(state, &account).await);
    }
    Ok(reports)
}

/// 解析每日领取时间（本地时区 `HH:MM`）。
fn parse_daily_time(value: &str) -> Option<chrono::NaiveTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::NaiveTime::parse_from_str(value, "%H:%M").ok()
}

/// 后台自动领取：
/// - `claimDailyTime`（`HH:MM`，本地时区）优先：每天到点执行一次；
/// - 否则回退到 `claimIntervalSeconds` 间隔模式（0=关闭）。
/// 领取只依赖账号 JWT + 无头浏览器，不依赖 ZCode CLI 是否安装。
pub async fn scheduler_loop(state: Arc<AppState>) {
    tokio::time::sleep(SCHEDULER_START_DELAY).await;
    let mut last_daily_key: Option<String> = None;
    loop {
        let (daily_time, interval) = {
            let cfg = state.zcode.config();
            (cfg.claim_daily_time.clone(), cfg.claim_interval_seconds)
        };
        if let Some(target) = parse_daily_time(&daily_time) {
            use chrono::Local;
            let now = Local::now();
            let today = now.date_naive();
            if let Some(target_at) = today.and_time(target).and_local_timezone(Local).single() {
                let key = format!("{} {}", today, target.format("%H:%M"));
                if now >= target_at && last_daily_key.as_deref() != Some(key.as_str()) {
                    // 重启后不重复领取：今日目标时间之后已有领取记录就只记标记。
                    let target_ms = target_at.timestamp_millis();
                    let already = state
                        .db
                        .zcode_recent_claims(1)
                        .first()
                        .map(|c| c.created_at >= target_ms)
                        .unwrap_or(false);
                    if !already {
                        match claim_all(&state, None, true).await {
                            Ok(reports) if !reports.is_empty() => {
                                let ok = reports.iter().filter(|r| r.ok).count();
                                tracing::info!(
                                    "ZCode 每日领取完成（{}）：{} 个账号，成功 {}，失败 {}",
                                    target.format("%H:%M"),
                                    reports.len(),
                                    ok,
                                    reports.len() - ok
                                );
                            }
                            Ok(_) => {}
                            Err(err) => tracing::warn!("ZCode 每日领取跳过：{err}"),
                        }
                    }
                    last_daily_key = Some(key);
                }
            }
        } else if interval > 0 {
            match claim_all(&state, None, false).await {
                Ok(reports) if !reports.is_empty() => {
                    let ok = reports.iter().filter(|r| r.ok).count();
                    tracing::info!(
                        "ZCode 自动领取完成：{} 个账号，成功 {}，失败 {}",
                        reports.len(),
                        ok,
                        reports.len() - ok
                    );
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("ZCode 自动领取跳过：{err}"),
            }
        }
        tokio::time::sleep(SCHEDULER_TICK).await;
    }
}

/// 手动刷新账号额度（preview only）；返回展示用套餐。
pub async fn refresh_account(state: &AppState, account_id: &str) -> Result<Value, String> {
    let account = state
        .db
        .zcode_get_account(account_id)
        .ok_or_else(|| "ZCode 账号不存在".to_string())?;
    let plans = preview_account(state, &account).await?;
    Ok(json!({
        "accountId": account.id,
        "plans": plans.iter().map(plan_json).collect::<Vec<_>>(),
        "checkedAt": now_ms(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_json_has_no_secret_fields() {
        let plan = Plan {
            plan_id: "p1".into(),
            name: "Start Plan".into(),
            priority: 2,
            grants: vec![json!({"name":"GLM-5.3-Flash","units":1000})],
        };
        let text = plan_json(&plan).to_string();
        assert!(text.contains("GLM-5.3-Flash"));
        assert!(!text.contains("Bearer"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn claim_report_json_shape() {
        let report = ClaimReport {
            account_id: "a".into(),
            account_name: "n".into(),
            plan_id: Some("p".into()),
            plan_name: Some("plan".into()),
            ok: false,
            code: Some(1005),
            message: Some("今日名额已用完".into()),
            next_at: Some(123),
            status: STATUS_EXHAUSTED.into(),
        };
        let value = report.to_json();
        assert_eq!(value["code"], 1005);
        assert_eq!(value["nextAt"], 123);
        assert!(value.get("token").is_none());
    }

    #[test]
    fn daily_time_accepts_hhmm_only() {
        assert!(parse_daily_time("08:30").is_some());
        assert!(parse_daily_time(" 23:59 ").is_some());
        assert!(parse_daily_time("").is_none());
        assert!(parse_daily_time("25:00").is_none());
        assert!(parse_daily_time("abc").is_none());
    }
}
