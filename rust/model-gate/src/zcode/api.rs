//! ZCode 插件 HTTP API：配置、账号、本机导入、OAuth、状态。
//!
//! 不返回 JWT/API Key 明文；只返回掩码/长度。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::proxy::{error_json, AppState};

use super::claim;
use super::config::ZcodeConfig;
use super::credentials;
use super::store::ZcodeAccount;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 池状态汇总：只统计，不触发任何上游请求。
fn pool_summary(accounts: &[ZcodeAccount]) -> Value {
    let now = now_ms();
    let mut active = 0;
    let mut cooling = 0;
    let mut invalid = 0;
    let mut disabled = 0;
    let mut exhausted = 0;
    for account in accounts {
        if !account.enabled || account.status == super::store::STATUS_DISABLED {
            disabled += 1;
            continue;
        }
        match account.status.as_str() {
            super::store::STATUS_ACTIVE => active += 1,
            super::store::STATUS_COOLING => {
                if account.cooling_until.map_or(false, |until| until > now) {
                    cooling += 1;
                } else {
                    active += 1;
                }
            }
            super::store::STATUS_INVALID => invalid += 1,
            super::store::STATUS_EXHAUSTED => exhausted += 1,
            _ => {}
        }
    }
    json!({
        "total": accounts.len(),
        "active": active,
        "cooling": cooling,
        "invalid": invalid,
        "disabled": disabled,
        "exhausted": exhausted,
    })
}

pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.zcode.config();
    let accounts = state.db.zcode_list_accounts();
    let active = accounts
        .iter()
        .filter(|a| a.enabled && a.status == super::store::STATUS_ACTIVE)
        .count();
    let last_claim = state.db.zcode_recent_claims(1).first().cloned();
    let captcha_available = state.zcode.captcha.available();
    Json(json!({
        "ok": true,
        "enabled": cfg.enabled,
        "backend": cfg.backend,
        "cliPath": cfg.cli_path,
        "cliExists": state.zcode.cli_exists(),
        "nodePath": cfg.node_path,
        "dataDir": cfg.data_dir,
        "browserPath": cfg.browser_path,
        "browserExists": state.zcode.browser_exists(),
        "claimIntervalSeconds": cfg.claim_interval_seconds,
        "accountConcurrency": cfg.account_concurrency,
        "accounts": {
            "total": accounts.len(),
            "active": active,
        },
        "pool": pool_summary(&accounts),
        "models": ["GLM-5.3-Flash", "GLM-5.3"],
        "captcha": {
            "available": captcha_available,
            "invalidations": state.zcode.captcha.invalidations(),
        },
        "lastClaim": last_claim.map(|c| c.to_json()),
        "checkedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

pub async fn get_config(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({"ok": true, "config": state.zcode.config().clone()})).into_response()
}

pub async fn put_config(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let incoming: ZcodeConfig = match serde_json::from_value(body) {
        Ok(cfg) => cfg,
        Err(err) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!("ZCode 配置不合法：{err}"),
                "invalid_request_error",
                "invalid_zcode_config",
            )
        }
    };
    if incoming.backend != "appserver" && incoming.backend != "direct" {
        return error_json(
            StatusCode::BAD_REQUEST,
            "backend 只能是 appserver 或 direct",
            "invalid_request_error",
            "invalid_zcode_backend",
        );
    }
    {
        let mut cfg = state.zcode.config_mut();
        *cfg = incoming;
    }
    if let Err(err) = state.zcode.save() {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("保存 ZCode 配置失败：{err}"),
            "server_error",
            "zcode_config_save_failed",
        );
    }
    Json(json!({"ok": true, "config": state.zcode.config().clone()})).into_response()
}

pub async fn list_accounts(State(state): State<Arc<AppState>>) -> Response {
    let accounts: Vec<Value> = state
        .db
        .zcode_list_accounts()
        .iter()
        .map(ZcodeAccount::public_view)
        .collect();
    let claims = state.db.zcode_recent_claims(20);
    Json(json!({
        "ok": true,
        "accounts": accounts,
        "claims": claims.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
    }))
    .into_response()
}

pub async fn import_local_info(State(state): State<Arc<AppState>>) -> Response {
    let data_dir = state.zcode.data_dir();
    match credentials::read_info(&data_dir, None) {
        Ok(info) => Json(json!({
            "ok": true,
            "dataDir": data_dir.display().to_string(),
            "found": info.found,
            "tokenLength": info.token_length,
            "activeProvider": info.active_provider,
            "keys": info.keys,
        }))
        .into_response(),
        Err(message) => error_json(
            StatusCode::BAD_REQUEST,
            message,
            "invalid_request_error",
            "zcode_local_credentials_failed",
        ),
    }
}

pub async fn import_local(State(state): State<Arc<AppState>>) -> Response {
    let data_dir = state.zcode.data_dir();
    let token = match credentials::read_token(&data_dir, None) {
        Ok(token) => token,
        Err(message) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                message,
                "invalid_request_error",
                "zcode_local_credentials_failed",
            )
        }
    };
    let provider = credentials::read_info(&data_dir, None)
        .ok()
        .and_then(|info| info.active_provider)
        .unwrap_or_else(|| "zai".to_string());
    let name = format!(
        "zcode-local-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    add_account(&state, &name, &provider, &token)
}

pub async fn add(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if token.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 token",
            "invalid_request_error",
            "zcode_token_required",
        );
    }
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "zcode-account".to_string());
    let provider = body
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| matches!(*s, "zai" | "bigmodel"))
        .unwrap_or("zai");
    add_account(&state, &name, provider, token)
}

fn add_account(state: &AppState, name: &str, provider: &str, token: &str) -> Response {
    let mode = if token.split('.').count() == 3 {
        "jwt"
    } else {
        "apikey"
    };
    let account = match ZcodeAccount::new(name, provider, mode, token) {
        Ok(account) => account,
        Err(message) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                message,
                "invalid_request_error",
                "zcode_account_invalid",
            )
        }
    };
    if let Some(existing) = state.db.zcode_find_by_fp(&account.secret_fp) {
        return Json(json!({
            "ok": true,
            "duplicate": true,
            "account": existing.public_view(),
        }))
        .into_response();
    }
    if let Err(err) = state.db.zcode_upsert_account(&account) {
        return error_json(
            StatusCode::CONFLICT,
            format!("写入 ZCode 账号失败：{err}"),
            "invalid_request_error",
            "zcode_account_save_failed",
        );
    }
    Json(json!({
        "ok": true,
        "duplicate": false,
        "account": account.public_view(),
    }))
    .into_response()
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some(mut account) = state.db.zcode_get_account(&id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            "ZCode 账号不存在",
            "invalid_request_error",
            "zcode_account_missing",
        );
    };
    if let Some(name) = body.get("name").and_then(Value::as_str).map(str::trim) {
        if !name.is_empty() {
            account.name = name.to_string();
        }
    }
    if let Some(enabled) = body.get("enabled").and_then(Value::as_bool) {
        account.enabled = enabled;
        if !enabled {
            account.status = super::store::STATUS_DISABLED.to_string();
        } else if account.status == super::store::STATUS_DISABLED {
            account.status = super::store::STATUS_ACTIVE.to_string();
        }
    }
    if let Some(token) = body.get("token").and_then(Value::as_str).map(str::trim) {
        if !token.is_empty() {
            match ZcodeAccount::new(
                &account.name,
                &account.provider,
                if token.split('.').count() == 3 {
                    "jwt"
                } else {
                    "apikey"
                },
                token,
            ) {
                Ok(fresh) => {
                    account.secret_enc = fresh.secret_enc;
                    account.secret_fp = fresh.secret_fp;
                    account.mode = fresh.mode;
                    account.status = super::store::STATUS_ACTIVE.to_string();
                    account.last_error = None;
                }
                Err(message) => {
                    return error_json(
                        StatusCode::BAD_REQUEST,
                        message,
                        "invalid_request_error",
                        "zcode_account_invalid",
                    )
                }
            }
        }
    }
    account.last_checked_at = Some(now_ms());
    if let Err(err) = state.db.zcode_upsert_account(&account) {
        return error_json(
            StatusCode::CONFLICT,
            format!("更新 ZCode 账号失败：{err}"),
            "invalid_request_error",
            "zcode_account_save_failed",
        );
    }
    Json(json!({"ok": true, "account": account.public_view()})).into_response()
}

pub async fn delete(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let ids: Vec<String> = body
        .get("ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 ids",
            "invalid_request_error",
            "zcode_ids_required",
        );
    }
    let count = state.db.zcode_delete_accounts(&ids);
    Json(json!({"ok": true, "deleted": count})).into_response()
}

pub async fn oauth_start(State(state): State<Arc<AppState>>) -> Response {
    match state.zcode.oauth.start(&state.client).await {
        Ok(start) => Json(json!({
            "ok": true,
            "flowId": start.flow_id,
            "authorizeUrl": start.authorize_url,
            "expiresIn": start.expires_in,
        }))
        .into_response(),
        Err(message) => error_json(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            "zcode_oauth_start_failed",
        ),
    }
}

pub async fn oauth_poll(
    State(state): State<Arc<AppState>>,
    Path(flow_id): Path<String>,
) -> Response {
    let data = match state.zcode.oauth.poll(&state.client, &flow_id).await {
        Ok(data) => data,
        Err(message) => {
            return error_json(
                StatusCode::BAD_GATEWAY,
                message,
                "upstream_error",
                "zcode_oauth_poll_failed",
            )
        }
    };
    let status = data
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    if status != "ready" {
        return Json(json!({
            "ok": true,
            "status": status,
            "message": data.get("message").cloned().unwrap_or(Value::Null),
        }))
        .into_response();
    }
    let token = match data
        .get("token")
        .and_then(Value::as_str)
        .filter(|s| s.split('.').count() == 3)
    {
        Some(token) => token.to_string(),
        None => {
            return error_json(
                StatusCode::BAD_GATEWAY,
                "OAuth 授权结果中没有有效 JWT",
                "upstream_error",
                "zcode_oauth_no_token",
            )
        }
    };
    let name = format!(
        "zcode-oauth-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let account = match ZcodeAccount::new(&name, "zai", "jwt", &token) {
        Ok(account) => account,
        Err(message) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                message,
                "server_error",
                "zcode_account_invalid",
            )
        }
    };
    if let Some(existing) = state.db.zcode_find_by_fp(&account.secret_fp) {
        return Json(json!({
            "ok": true,
            "status": "ready",
            "duplicate": true,
            "account": existing.public_view(),
        }))
        .into_response();
    }
    if let Err(err) = state.db.zcode_upsert_account(&account) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入 OAuth 账号失败：{err}"),
            "server_error",
            "zcode_account_save_failed",
        );
    }
    Json(json!({
        "ok": true,
        "status": "ready",
        "duplicate": false,
        "account": account.public_view(),
    }))
    .into_response()
}

/// `POST /api/zcode/attach`：把 GLM 池接入为 `upstreams.zcode`。
///
/// 复用免费来源的配置落盘通道（备份 → 合并 → 原子写 → 回读校验），
/// 避免维护第二套配置写入逻辑。
pub async fn attach(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let models: Vec<String> = body
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .filter(|items: &Vec<String>| !items.is_empty())
        .unwrap_or_else(|| vec!["GLM-5.3-Flash".to_string(), "GLM-5.3".to_string()]);
    let payload = json!({
        "source": "zcode",
        "models": models,
        "backup": body.get("backup").and_then(Value::as_bool),
    });
    crate::free_sources_api::free_sources_attach(State(state), Json(payload)).await
}

pub async fn claim_preview(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let requested = params
        .get("account")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut accounts: Vec<ZcodeAccount> = state.db.zcode_list_accounts();
    if let Some(id) = requested.as_deref() {
        accounts.retain(|a| a.id == id);
        if accounts.is_empty() {
            return error_json(
                StatusCode::NOT_FOUND,
                "ZCode 账号不存在",
                "invalid_request_error",
                "zcode_account_missing",
            );
        }
    } else {
        accounts.retain(|a| a.enabled && a.mode == "jwt");
        accounts.retain(|a| {
            a.status != super::store::STATUS_INVALID && a.status != super::store::STATUS_DISABLED
        });
        accounts.truncate(10);
    }

    let mut rows = Vec::new();
    for account in accounts {
        match claim::preview_account(&state, &account).await {
            Ok(plans) => rows.push(json!({
                "accountId": account.id,
                "accountName": account.name,
                "status": account.status,
                "plans": plans.iter().map(|p| json!({
                    "planId": p.plan_id,
                    "name": p.name,
                    "priority": p.priority,
                    "grants": p.grants,
                })).collect::<Vec<_>>(),
            })),
            Err(message) => rows.push(json!({
                "accountId": account.id,
                "accountName": account.name,
                "status": account.status,
                "plans": [],
                "error": message,
            })),
        }
    }
    Json(json!({
        "ok": true,
        "accounts": rows,
        "checkedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

/// `POST /api/zcode/claim`：立即领取；body `{accountId?}`，缺省领取所有到期账号。
pub async fn claim_now(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let account_id = body
        .get("accountId")
        .or_else(|| body.get("account"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match claim::claim_all(&state, account_id, true).await {
        Ok(reports) => {
            let ok = reports.iter().filter(|r| r.ok).count();
            Json(json!({
                "ok": true,
                "claimed": ok,
                "reports": reports.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
                "checkedAt": chrono::Utc::now().to_rfc3339(),
            }))
            .into_response()
        }
        Err(message) => error_json(
            StatusCode::BAD_REQUEST,
            message,
            "invalid_request_error",
            "zcode_claim_failed",
        ),
    }
}

/// `POST /api/zcode/accounts/{id}/refresh`：刷新额度（preview only）。
pub async fn refresh_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match claim::refresh_account(&state, &id).await {
        Ok(data) => Json(json!({"ok": true, "refresh": data})).into_response(),
        Err(message) => error_json(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            "zcode_refresh_failed",
        ),
    }
}

/// `POST /api/zcode/captcha/test`：求解一次验证码但不 claim；不返回参数原文。
pub async fn captcha_test(State(state): State<Arc<AppState>>) -> Response {
    let solver = state.zcode.captcha_solver();
    match state.zcode.captcha.acquire(solver).await {
        Ok(token) => Json(json!({
            "ok": true,
            "solvedMs": token.solved_ms,
            "paramLength": token.param.chars().count(),
            "region": token.region,
            "available": state.zcode.captcha.available(),
        }))
        .into_response(),
        Err(message) => error_json(
            StatusCode::BAD_GATEWAY,
            message,
            "upstream_error",
            "captcha_unavailable",
        ),
    }
}
