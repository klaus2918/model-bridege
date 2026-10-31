//! AI 可读配置接口（只读 + 草案 + 人工确认应用）。
//!
//! 安全边界（设计文档 04）：
//! - 响应永不包含 apiKey / token / secret / cookie / 账号；
//! - plan 不落盘，只存内存，TTL 10 分钟；
//! - apply 必须携带 plan 返回的一次性确认令牌；AI 默认无 apply 权限；
//! - apply 只应用计划中声明的非敏感字段；含密钥的计划返回 409 needs_secret；
//! - 每次 plan/apply 记录审计（只记字段路径，不记值）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

use crate::access;
use crate::proxy::{error_json, AppState};

const PLAN_TTL_SECONDS: i64 = 600;
const PLAN_MAX: usize = 100;

#[derive(Debug, Clone)]
struct PlanRecord {
    id: String,
    action: String,
    input: Value,
    output: Value,
    status: String,
    diff: Vec<Value>,
    missing: Vec<Value>,
    warnings: Vec<Value>,
    created: i64,
    used: bool,
}

fn plans() -> &'static Mutex<Vec<PlanRecord>> {
    static PLANS: OnceLock<Mutex<Vec<PlanRecord>>> = OnceLock::new();
    PLANS.get_or_init(|| Mutex::new(Vec::new()))
}

fn now_unix() -> i64 {
    chrono::Local::now().timestamp()
}

fn now_text() -> String {
    chrono::Local::now().to_rfc3339()
}

fn plan_id() -> String {
    format!("plan_{}_{}", now_unix(), nanoid())
}

fn nanoid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    format!("{:x}{:x}", now_unix(), SEQ.fetch_add(1, Ordering::Relaxed))
}

fn prune_plans() {
    let mut guard = plans().lock().unwrap_or_else(|e| e.into_inner());
    let now = now_unix();
    guard.retain(|plan| now - plan.created <= PLAN_TTL_SECONDS);
    while guard.len() > PLAN_MAX {
        guard.remove(0);
    }
}

pub fn config_revision(raw: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    raw.to_string().hash(&mut hasher);
    format!("cfgrev_{:x}", hasher.finish())
}

// ─────────────────────────── 脱敏 ───────────────────────────

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "apikey"
            | "apikeyenc"
            | "api_key"
            | "token"
            | "access_token"
            | "refresh_token"
            | "secret"
            | "client_secret"
            | "password"
            | "cookie"
            | "authorization"
            | "credentials"
            | "secret_enc"
            | "secret_ref"
            | "secretref"
    ) || key.contains("password")
        || key.ends_with("_token")
}

/// 递归脱敏：按键名剔除敏感字段；数组逐项处理。
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                if is_secret_key(key) {
                    continue;
                }
                out.insert(key.clone(), redact(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

fn json_error(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    error_json(status, message, "invalid_request_error", code)
}

// ─────────────────────────── 设置 / 权限 ───────────────────────────

fn ai_settings(state: &AppState) -> Value {
    // AI 设置属于运行偏好，写入 bridge.config.json 顶层 `aiConfig`，
    // 不写 settings 表：配置仍只有一个事实源，避免“快照复活”类缺陷。
    let stored = state
        .cfg()
        .raw
        .get("aiConfig")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "enabled": stored.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "allowApply": stored.get("allowApply").and_then(Value::as_bool).unwrap_or(false),
        "allowPoolStats": stored.get("allowPoolStats").and_then(Value::as_bool).unwrap_or(true),
    })
}

fn actor(headers: &HeaderMap) -> String {
    headers
        .get("x-mg-actor")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("ui")
        .to_ascii_lowercase()
}

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({ "settings": ai_settings(&state) })).into_response()
}

pub async fn put_settings(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let settings = body.get("settings").cloned().unwrap_or(body);
    let allowed = json!({
        "enabled": settings.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "allowApply": settings.get("allowApply").and_then(Value::as_bool).unwrap_or(false),
        "allowPoolStats": settings.get("allowPoolStats").and_then(Value::as_bool).unwrap_or(true),
    });
    let mut next = state.cfg().raw.clone();
    next["aiConfig"] = allowed.clone();
    match crate::access::commit_raw(&state, &next) {
        Ok(_) => Json(json!({ "ok": true, "settings": allowed })).into_response(),
        Err(response) => response,
    }
}

// ─────────────────────────── 目录 / Schema ───────────────────────────

pub async fn manifest(State(state): State<Arc<AppState>>) -> Response {
    let settings = ai_settings(&state);
    let permissions = json!({
        "readConfig": true,
        "readSecrets": false,
        "readAccounts": false,
        "createDraft": settings["enabled"].as_bool().unwrap_or(true),
        "applyNonSecret": settings["allowApply"].as_bool().unwrap_or(false),
        "applySecret": false,
        "applyFromPanel": true,
        "readPoolStats": settings["allowPoolStats"].as_bool().unwrap_or(true),
    });
    Json(json!({
        "schema": "modelgate.ai/v1",
        "coreVersion": env!("CARGO_PKG_VERSION"),
        "generatedAt": now_text(),
        "baseUrl": format!("http://{}:{}", state.cfg().host, state.cfg().port),
        "endpoints": {
            "manifest": "/api/ai/manifest",
            "schema": "/api/ai/schema",
            "templates": "/api/ai/templates",
            "inspect": "/api/ai/inspect",
            "plan": "/api/ai/plan",
            "apply": "/api/ai/apply",
            "audit": "/api/ai/audit",
            "settings": "/api/ai/settings",
            "llms": "/llms.txt",
        },
        "permissions": permissions,
        "redaction": {
            "secretFields": ["apiKey", "apiKeyEnc", "token", "secret", "password", "cookie", "authorization"],
            "strategy": "omit",
            "allowLast4": false,
            "maskedExample": "sk-****",
        },
        "actions": [
            "create_upstream_from_template",
            "create_custom_upstream",
            "update_upstream_non_secret",
            "attach_plugin",
            "detach_plugin",
            "create_pool",
            "update_pool_policy",
        ],
        "notes": [
            "密钥只能由人工在本机 UI 写入；AI 可创建待补充凭据的接入。",
            "privacy.zdr 表示用户意图，实际发送取决于厂商能力（见 template.privacy.support）。",
            "apply 必须携带 plan 返回的 confirmation；UI actor 或 allowApply=true 才可应用。",
        ],
    }))
    .into_response()
}

pub async fn schema() -> Response {
    Json(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "modelgate.ai/v1",
        "definitions": {
            "AccessDraft": {
                "type": "object",
                "required": ["templateId", "name"],
                "properties": {
                    "templateId": { "type": "string" },
                    "name": { "type": "string", "pattern": "^[^/\\s]+$" },
                    "protocol": { "enum": ["openai", "anthropic", "commandcode", "opencode", "zcode"] },
                    "baseUrl": { "type": "string", "format": "uri" },
                    "privacy": {
                        "type": "object",
                        "properties": {
                            "zdr": { "type": "boolean" },
                            "training": { "enum": ["allow", "deny"] }
                        }
                    },
                    "apiKey": { "type": "string", "writeOnly": true, "x-mg-secret": true }
                },
                "additionalProperties": false
            },
            "PoolDraft": {
                "type": "object",
                "required": ["upstream"],
                "properties": {
                    "upstream": { "type": "string" },
                    "strategy": { "enum": ["priority", "round_robin", "weighted", "least_used"] },
                    "cooldownSeconds": { "type": "integer", "minimum": 1 },
                    "failureThreshold": { "type": "integer", "minimum": 1 }
                }
            },
            "PlanRequest": {
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {
                        "enum": [
                            "create_upstream_from_template",
                            "create_custom_upstream",
                            "update_upstream_non_secret",
                            "attach_plugin",
                            "detach_plugin",
                            "create_pool",
                            "update_pool_policy"
                        ]
                    },
                    "input": { "type": "object" }
                }
            },
            "ApplyRequest": {
                "type": "object",
                "required": ["planId", "confirmation"],
                "properties": {
                    "planId": { "type": "string" },
                    "confirmation": { "type": "string" },
                    "expectedRevision": { "type": "string" }
                }
            }
        }
    }))
    .into_response()
}

pub async fn templates() -> Response {
    let items: Vec<Value> = access::templates()
        .iter()
        .map(access::template_json)
        .collect();
    Json(json!({ "items": items, "total": items.len() })).into_response()
}

fn enrich_access(state: &AppState, mut value: Value, allow_pool_stats: bool) -> Value {
    let Some(pool_id) = value
        .get("auth")
        .and_then(|auth| auth.get("poolId"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return value;
    };
    if let Some(pool) = crate::credential_pool::get_pool(state, &pool_id) {
        let summary = crate::credential_pool::summary(state, &pool);
        if allow_pool_stats {
            value["pool"] = summary.to_json(&pool);
        } else {
            value["pool"] = json!({
                "id": pool.id,
                "strategy": pool.strategy,
                "members": summary.members,
                "available": summary.available,
            });
        }
        value["auth"]["configured"] = json!(summary.available > 0);
    }
    value
}

pub async fn inspect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let settings = ai_settings(&state);
    if !settings["enabled"].as_bool().unwrap_or(true) {
        return json_error(StatusCode::FORBIDDEN, "AI 配置接口已停用", "ai_disabled");
    }
    let allow_pool_stats = settings["allowPoolStats"].as_bool().unwrap_or(true);
    let query: std::collections::HashMap<String, String> = uri
        .query()
        .map(|text| {
            text.split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let upstream = query.get("upstream").map(String::as_str);
    let cfg = state.cfg();
    let mut items: Vec<Value> = cfg
        .upstreams
        .iter()
        .map(|up| {
            let cooling = {
                let now = std::time::Instant::now();
                let health = state.route_health.lock().unwrap_or_else(|e| e.into_inner());
                health.iter().any(|(key, until)| {
                    key.starts_with(&format!("{}\u{0}", up.name)) && *until > now
                })
            };
            enrich_access(
                &state,
                access::access_object(&cfg, up, cooling),
                allow_pool_stats,
            )
        })
        .collect();
    items.retain(|row| upstream.map(|name| row["id"] == name).unwrap_or(true));
    let selected = items.first().cloned();
    Json(json!({
        "actor": actor(&headers),
        "revision": config_revision(&cfg.raw),
        "upstreams": redact(&Value::Array(items.clone())),
        "selected": selected.map(|value| redact(&value)),
        "total": items.len(),
    }))
    .into_response()
}

// ─────────────────────────── Plan ───────────────────────────

fn non_secret_patch(input: &Value) -> Result<Map<String, Value>, String> {
    let Some(obj) = input.as_object() else {
        return Err("input 必须是对象".into());
    };
    let allowed = [
        "protocol",
        "baseUrl",
        "label",
        "models",
        "modelMap",
        "modelOverrides",
        "fetchModels",
        "autoModels",
        "pauseTurn",
        "privacy",
        "transport",
        "template",
        "kind",
        "credentialPool",
        "default",
    ];
    let mut out = Map::new();
    for (key, value) in obj {
        if is_secret_key(key) || key.starts_with("apiKey") || key == "modelKeys" {
            return Err(format!("字段「{key}」属于敏感字段，AI 不可写入"));
        }
        if allowed.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        }
    }
    Ok(out)
}

fn diff_for_upstream(name: &str, entry: &Map<String, Value>) -> Vec<Value> {
    entry
        .iter()
        .map(|(key, value)| {
            json!({
                "path": format!("upstreams.{name}.{key}"),
                "op": "add",
                "value": redact(value),
            })
        })
        .collect()
}

fn build_plan(action: &str, input: &Value, state: &AppState) -> Result<PlanRecord, Response> {
    let cfg = state.cfg();
    let mut missing: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let (output, diff, status) = match action {
        "create_upstream_from_template" | "create_custom_upstream" => {
            let template_id = input.get("templateId").and_then(Value::as_str).unwrap_or(
                if action == "create_custom_upstream" {
                    "custom"
                } else {
                    ""
                },
            );
            let Some(template) = access::template(template_id) else {
                return Err(json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("未知模板「{template_id}」"),
                    "template_unknown",
                ));
            };
            let name = input
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(template_id)
                .to_string();
            if name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace) {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "上游标识不合法",
                    "invalid_name",
                ));
            }
            if cfg.upstream(&name).is_some() {
                return Err(json_error(
                    StatusCode::CONFLICT,
                    format!("接入「{name}」已存在"),
                    "name_conflict",
                ));
            }
            let protocol = input
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or(&template.protocol)
                .to_string();
            if !matches!(
                protocol.as_str(),
                "openai" | "anthropic" | "commandcode" | "opencode" | "zcode"
            ) {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    format!("不支持的协议「{protocol}」"),
                    "invalid_protocol",
                ));
            }
            let base_url = input
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or(&template.base_url)
                .to_string();
            if base_url.trim().is_empty() {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "缺少 baseUrl",
                    "missing_field",
                ));
            }
            if !template.auth_optional {
                missing.push(json!({
                    "field": "apiKey",
                    "reason": "需要人工在本机面板填写；AI 不可写入密钥",
                }));
            }
            if template.privacy["support"] == "none" {
                warnings.push(json!({
                    "code": "privacy_not_supported",
                    "message": "该厂商不支持请求级 ZDR；privacy.zdr 仅记录意图。",
                }));
            }
            let entry = access::build_upstream_entry(
                &template,
                &name,
                &protocol,
                &base_url,
                None,
                input,
                if action == "create_custom_upstream" {
                    "custom"
                } else {
                    "official"
                },
                input.get("vendor").and_then(Value::as_str),
            );
            let diff = diff_for_upstream(&name, &entry);
            (
                json!({
                    "kind": "upstream",
                    "name": name,
                    "entry": entry,
                    "access": access::finalize_access(state, &name),
                }),
                diff,
                if missing.is_empty() {
                    "ready".to_string()
                } else {
                    "needs_secret".to_string()
                },
            )
        }
        "update_upstream_non_secret" => {
            let name = input
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if cfg.upstream(&name).is_none() {
                return Err(json_error(
                    StatusCode::NOT_FOUND,
                    format!("接入「{name}」不存在"),
                    "access_not_found",
                ));
            }
            let patch = non_secret_patch(input.get("patch").unwrap_or(&Value::Null))
                .map_err(|message| json_error(StatusCode::BAD_REQUEST, message, "invalid_patch"))?;
            if patch.is_empty() {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "没有可更新的非敏感字段",
                    "empty_patch",
                ));
            }
            let diff: Vec<Value> = patch
                .iter()
                .map(|(key, value)| {
                    json!({
                        "path": format!("upstreams.{name}.{key}"),
                        "op": "update",
                        "value": redact(value),
                    })
                })
                .collect();
            (
                json!({ "kind": "upstream_patch", "name": name, "patch": patch }),
                diff,
                "ready".to_string(),
            )
        }
        "create_pool" => {
            let upstream = input.get("upstream").and_then(Value::as_str).unwrap_or("");
            if cfg.upstream(upstream).is_none() {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    format!("上游「{upstream}」不存在"),
                    "unknown_upstream",
                ));
            }
            let strategy = input
                .get("strategy")
                .and_then(Value::as_str)
                .unwrap_or("priority");
            (
                json!({
                    "kind": "pool",
                    "upstream": upstream,
                    "strategy": strategy,
                    "cooldownSeconds": input.get("cooldownSeconds").cloned().unwrap_or(json!(60)),
                    "failureThreshold": input.get("failureThreshold").cloned().unwrap_or(json!(3)),
                }),
                vec![
                    json!({"path": format!("pools.{upstream}"), "op": "add", "value": {"strategy": strategy}}),
                ],
                "ready".to_string(),
            )
        }
        "update_pool_policy" => {
            let pool_id = input.get("poolId").and_then(Value::as_str).unwrap_or("");
            if crate::credential_pool::get_pool(state, pool_id).is_none() {
                return Err(json_error(
                    StatusCode::NOT_FOUND,
                    format!("池「{pool_id}」不存在"),
                    "pool_not_found",
                ));
            }
            // 池策略不是上游字段，单独白名单；密钥/成员仍不可经 AI 写入。
            let allowed = [
                "strategy",
                "cooldownSeconds",
                "failureThreshold",
                "fallbackToSingle",
                "status",
            ];
            let mut patch = Map::new();
            if let Some(obj) = input.get("patch").and_then(Value::as_object) {
                for (key, value) in obj {
                    if allowed.contains(&key.as_str()) {
                        patch.insert(key.clone(), value.clone());
                    }
                }
            }
            if patch.is_empty() {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "没有可更新的池策略字段",
                    "empty_patch",
                ));
            }
            if let Some(strategy) = patch.get("strategy").and_then(Value::as_str) {
                if !matches!(
                    strategy,
                    "priority" | "round_robin" | "weighted" | "least_used"
                ) {
                    return Err(json_error(
                        StatusCode::BAD_REQUEST,
                        "未知策略",
                        "invalid_strategy",
                    ));
                }
            }
            (
                json!({ "kind": "pool_patch", "poolId": pool_id, "patch": patch }),
                vec![json!({"path": format!("pools.{pool_id}"), "op": "update"})],
                "ready".to_string(),
            )
        }
        "attach_plugin" | "detach_plugin" => {
            let mut input = input.clone();
            if input.get("manifest").is_none() {
                let plugin_id = input
                    .get("pluginId")
                    .or_else(|| input.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let Some(manifest) = crate::plugin::manifest_for(state, plugin_id) else {
                    return Err(json_error(
                        StatusCode::NOT_FOUND,
                        format!("插件「{plugin_id}」不存在"),
                        "plugin_not_found",
                    ));
                };
                input["manifest"] = manifest;
            }
            let planned = crate::plugin::plan_action(action, &input, &cfg)
                .map_err(|(status, message, code)| json_error(status, message, code))?;
            (planned.output, planned.diff, planned.status)
        }
        other => {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                format!("未知动作「{other}」"),
                "invalid_action",
            ))
        }
    };
    Ok(PlanRecord {
        id: plan_id(),
        action: action.to_string(),
        input: input.clone(),
        output,
        status: status.to_string(),
        diff,
        missing,
        warnings,
        created: now_unix(),
        used: false,
    })
}

pub async fn plan(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let settings = ai_settings(&state);
    if !settings["enabled"].as_bool().unwrap_or(true) {
        return json_error(StatusCode::FORBIDDEN, "AI 配置接口已停用", "ai_disabled");
    }
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let input = body.get("input").cloned().unwrap_or_else(|| json!({}));
    let record = match build_plan(action, &input, &state) {
        Ok(record) => record,
        Err(response) => return response,
    };
    let confirmation = format!("confirm-{}", record.id);
    let response = json!({
        "planId": record.id,
        "action": record.action,
        "expiresAt": chrono::DateTime::from_timestamp(record.created + PLAN_TTL_SECONDS, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        "status": record.status,
        "summary": summarize(&record.action, &record.output),
        "diff": record.diff,
        "missing": record.missing,
        "warnings": record.warnings,
        "confirmation": confirmation,
        "expectedRevision": config_revision(&state.cfg().raw),
    });
    prune_plans();
    plans()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(record);
    Json(response).into_response()
}

fn summarize(action: &str, output: &Value) -> String {
    match action {
        "create_upstream_from_template" | "create_custom_upstream" => format!(
            "将创建接入 {}，使用模板默认协议与地址。",
            output.get("name").and_then(Value::as_str).unwrap_or("")
        ),
        "update_upstream_non_secret" => format!(
            "将更新接入 {} 的非敏感字段。",
            output.get("name").and_then(Value::as_str).unwrap_or("")
        ),
        "create_pool" => format!(
            "将为上游 {} 创建凭据池（策略 {}）。",
            output.get("upstream").and_then(Value::as_str).unwrap_or(""),
            output
                .get("strategy")
                .and_then(Value::as_str)
                .unwrap_or("priority")
        ),
        "update_pool_policy" => format!(
            "将更新凭据池 {} 的策略。",
            output.get("poolId").and_then(Value::as_str).unwrap_or("")
        ),
        "attach_plugin" => "将按插件 manifest 接入上游（不会写入密钥）。".into(),
        "detach_plugin" => "将解绑插件来源标记（默认保留上游与凭据）。".into(),
        _ => "配置变更计划".into(),
    }
}

// ─────────────────────────── Apply ───────────────────────────

fn apply_record(state: &AppState, record: &PlanRecord) -> Result<Value, Response> {
    match record.output.get("kind").and_then(Value::as_str) {
        Some("upstream") => {
            let name = record.output["name"].as_str().unwrap_or("");
            let entry = record.output["entry"].clone();
            let next = access::next_config_with(state, name, entry);
            let committed = access::commit_raw(state, &next)?;
            Ok(json!({
                "target": format!("upstreams.{name}"),
                "backup": committed.get("backup").cloned().unwrap_or(Value::Null),
                "access": access::finalize_access(state, name),
            }))
        }
        Some("upstream_patch") => {
            let name = record.output["name"].as_str().unwrap_or("");
            let patch = record.output["patch"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut entry = state
                .cfg()
                .raw
                .get("upstreams")
                .and_then(|upstreams| upstreams.get(name))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in patch {
                entry.insert(key, value);
            }
            let next = access::next_config_with(state, name, Value::Object(entry));
            let committed = access::commit_raw(state, &next)?;
            Ok(json!({
                "target": format!("upstreams.{name}"),
                "backup": committed.get("backup").cloned().unwrap_or(Value::Null),
                "access": access::finalize_access(state, name),
            }))
        }
        Some("pool") => {
            let upstream = record.output["upstream"].as_str().unwrap_or("");
            let strategy = record.output["strategy"].as_str().unwrap_or("priority");
            let cooldown = record.output["cooldownSeconds"].as_i64().unwrap_or(60);
            let threshold = record.output["failureThreshold"].as_i64().unwrap_or(3);
            let id = format!("pool_{}_{}", upstream, now_unix());
            crate::credential_pool::create_pool(
                state, &id, upstream, strategy, cooldown, threshold, None,
            )
            .map_err(|message| json_error(StatusCode::BAD_REQUEST, message, "pool_write_failed"))?;
            Ok(json!({ "target": format!("credential_pools.{id}"), "poolId": id }))
        }
        Some("pool_patch") => {
            let pool_id = record.output["poolId"].as_str().unwrap_or("");
            let patch = record.output["patch"].clone();
            apply_pool_patch(state, pool_id, &patch)?;
            Ok(json!({ "target": format!("credential_pools.{pool_id}"), "poolId": pool_id }))
        }
        Some("plugin_attach") => crate::plugin::apply_attach(state, &record.output),
        Some("plugin_detach") => crate::plugin::apply_detach(state, &record.output),
        other => Err(json_error(
            StatusCode::BAD_REQUEST,
            format!("计划缺少可应用类型：{other:?}"),
            "invalid_plan",
        )),
    }
}

fn apply_pool_patch(state: &AppState, pool_id: &str, patch: &Value) -> Result<(), Response> {
    let mut sets: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(strategy) = patch.get("strategy").and_then(Value::as_str) {
        if !matches!(
            strategy,
            "priority" | "round_robin" | "weighted" | "least_used"
        ) {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "未知策略",
                "invalid_strategy",
            ));
        }
        sets.push(format!("strategy = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(strategy.to_string()));
    }
    for (key, column) in [
        ("cooldownSeconds", "cooldown_seconds"),
        ("failureThreshold", "failure_threshold"),
    ] {
        if let Some(value) = patch.get(key).and_then(Value::as_i64) {
            sets.push(format!("{column} = ?{}", params.len() + 1));
            params.push(rusqlite::types::Value::Integer(value));
        }
    }
    if let Some(status) = patch.get("status").and_then(Value::as_str) {
        if !matches!(status, "active" | "disabled") {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "未知状态",
                "invalid_status",
            ));
        }
        sets.push(format!("status = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Text(status.to_string()));
    }
    if let Some(value) = patch.get("fallbackToSingle").and_then(Value::as_bool) {
        sets.push(format!("fallback_to_single = ?{}", params.len() + 1));
        params.push(rusqlite::types::Value::Integer(if value { 1 } else { 0 }));
    }
    if sets.is_empty() {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "没有可更新字段",
            "empty_patch",
        ));
    }
    let conn = state.db.write_conn();
    let sql = format!(
        "UPDATE credential_pools SET {} WHERE id = ?{}",
        sets.join(", "),
        params.len() + 1
    );
    params.push(rusqlite::types::Value::Text(pool_id.to_string()));
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(|err| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("池更新失败: {err}"),
                "pool_write_failed",
            )
        })?;
    Ok(())
}

pub async fn apply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let settings = ai_settings(&state);
    if !settings["enabled"].as_bool().unwrap_or(true) {
        return json_error(StatusCode::FORBIDDEN, "AI 配置接口已停用", "ai_disabled");
    }
    let actor = actor(&headers);
    if actor != "ui" && !settings["allowApply"].as_bool().unwrap_or(false) {
        return json_error(
            StatusCode::FORBIDDEN,
            "AI apply 未启用；请在设置中允许或由面板确认",
            "ai_apply_disabled",
        );
    }
    let plan_id = body.get("planId").and_then(Value::as_str).unwrap_or("");
    let confirmation = body
        .get("confirmation")
        .and_then(Value::as_str)
        .unwrap_or("");
    let expected_revision = body.get("expectedRevision").and_then(Value::as_str);
    if plan_id.is_empty() || confirmation != format!("confirm-{plan_id}") {
        return json_error(
            StatusCode::BAD_REQUEST,
            "缺少或错误的 confirmation；必须使用 plan 返回的一次性确认令牌",
            "invalid_plan",
        );
    }
    if let Some(expected) = expected_revision {
        let current = config_revision(&state.cfg().raw);
        if expected != current {
            return json_error(
                StatusCode::CONFLICT,
                "配置已变化，请重新生成计划",
                "revision_conflict",
            );
        }
    }
    // 注意：prune_plans 自己会锁 plans；必须在获取 guard 之前调用，否则死锁。
    prune_plans();
    let record = {
        let mut guard = plans().lock().unwrap_or_else(|e| e.into_inner());
        let Some(record) = guard.iter_mut().find(|record| record.id == plan_id) else {
            return json_error(
                StatusCode::BAD_REQUEST,
                "计划不存在或已过期",
                "invalid_plan",
            );
        };
        if record.used {
            return json_error(StatusCode::BAD_REQUEST, "计划已应用", "invalid_plan");
        }
        if now_unix() - record.created > PLAN_TTL_SECONDS {
            return json_error(StatusCode::BAD_REQUEST, "计划已过期", "invalid_plan");
        }
        if record.status == "needs_secret" {
            return json_error(
                StatusCode::CONFLICT,
                "计划需要人工补充密钥，AI 不可写入",
                "needs_secret",
            );
        }
        if record.status == "invalid" {
            return json_error(StatusCode::BAD_REQUEST, "计划无效", "invalid_plan");
        }
        record.clone()
    };
    let changed_paths: Vec<String> = record
        .diff
        .iter()
        .filter_map(|row| row.get("path").and_then(Value::as_str).map(str::to_string))
        .collect();
    match apply_record(&state, &record) {
        Ok(result) => {
            let audit_id = crate::credential_pool::record_audit(
                &state,
                &actor,
                "apply",
                Some(plan_id),
                result.get("target").and_then(Value::as_str).unwrap_or(""),
                &changed_paths,
                "ok",
                result
                    .get("backup")
                    .and_then(|backup| backup.get("path"))
                    .and_then(Value::as_str),
            );
            if let Some(record) = plans()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter_mut()
                .find(|row| row.id == plan_id)
            {
                record.used = true;
            }
            state.clear_models_cache().await;
            Json(json!({
                "ok": true,
                "auditId": audit_id,
                "result": result,
                "access": result.get("access").cloned().map(|value| redact(&value)),
            }))
            .into_response()
        }
        Err(response) => {
            let _ = crate::credential_pool::record_audit(
                &state,
                &actor,
                "apply",
                Some(plan_id),
                "",
                &changed_paths,
                "failed",
                None,
            );
            response
        }
    }
}

// ─────────────────────────── Audit ───────────────────────────

pub async fn audit(State(state): State<Arc<AppState>>) -> Response {
    let conn = state.db.read_conn();
    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, at, actor, action, plan_id, target, changed_paths, result, backup_id FROM config_audit ORDER BY at DESC LIMIT 100",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "at": row.get::<_, String>(1)?,
                "actor": row.get::<_, Option<String>>(2)?,
                "action": row.get::<_, Option<String>>(3)?,
                "planId": row.get::<_, Option<String>>(4)?,
                "target": row.get::<_, Option<String>>(5)?,
                "changedPaths": row.get::<_, Option<String>>(6)?
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .unwrap_or_else(|| json!([])),
                "result": row.get::<_, Option<String>>(7)?,
                "backupId": row.get::<_, Option<String>>(8)?,
            }))
        }) {
            items.extend(rows.flatten());
        }
    }
    Json(json!({ "items": items, "total": items.len() })).into_response()
}

pub async fn llms_txt() -> Response {
    let text = "# Model-Gate AI Configuration\nBase: http://127.0.0.1:8900\nRead:  GET /api/ai/manifest, /api/ai/schema, /api/ai/templates, /api/ai/inspect\nDraft: POST /api/ai/plan\nApply: POST /api/ai/apply (requires plan confirmation; AI apply is disabled by default)\nNever: read or write apiKey/token/account/cookie.\nOfficial provider: choose template, ask human for apiKey, plan, wait for confirmation.\nCustom provider: name, vendor, baseUrl, protocol, apiKey.\nPrivacy: zdr=true is user intent; check template.privacy.support before claiming protection.\nPools: GET /api/pools returns counts only; members and secrets are never exposed to AI.\n";
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_removes_nested_secrets() {
        let value = json!({
            "apiKey": "sk-secret",
            "nested": { "token": "t", "ok": 1 },
            "items": [{ "password": "p", "label": "x" }]
        });
        let redacted = redact(&value);
        let text = redacted.to_string();
        assert!(!text.contains("sk-secret"));
        assert!(!text.contains("\"t\""));
        assert!(!text.contains("\"p\""));
        assert!(text.contains("\"ok\":1"));
        assert!(text.contains("\"label\":\"x\""));
    }

    #[test]
    fn revision_is_stable_and_changes() {
        let a = json!({"a": 1});
        let b = json!({"a": 2});
        assert_eq!(config_revision(&a), config_revision(&a));
        assert_ne!(config_revision(&a), config_revision(&b));
        assert!(config_revision(&a).starts_with("cfgrev_"));
    }

    #[test]
    fn secret_patch_is_rejected() {
        let err = non_secret_patch(&json!({"apiKey": "x"})).unwrap_err();
        assert!(err.contains("敏感"));
        assert!(non_secret_patch(&json!({"label": "x"})).is_ok());
    }
}
