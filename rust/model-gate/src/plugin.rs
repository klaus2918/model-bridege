//! 插件登记、manifest 校验与幂等接入。
//!
//! 五条规则（docs/architecture/plugin-system.md）：
//! 1. 安装/卸载不执行 DDL；
//! 2. 插件不直接写 bridge.config.json，接入必须经 attach；
//! 3. 插件不接管自己的配置存储；
//! 4. 卸载默认保留用户资产与上游条目；
//! 5. native 插件不可卸载，只能禁用。
//!
//! 设计增量（docs/design/refactor-2026/03-plugin-token-pool.md）：
//! - manifest 可声明 `auth.mode = pool` 与池默认值；
//! - manifest 可声明 `access.requiredFields/advancedFields/privacy`。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};

use crate::access;
use crate::config::Config;
use crate::proxy::{error_json, AppState};

/// AI/面板共用的计划输出。
pub struct PlannedPlugin {
    pub output: Value,
    pub diff: Vec<Value>,
    pub status: String,
}

fn now_text() -> String {
    chrono::Local::now().to_rfc3339()
}

fn plugin_error(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    error_json(status, message, "invalid_request_error", code)
}

fn builtin_manifests() -> Vec<Value> {
    vec![
        json!({
            "schema": "modelgate.plugin/v1",
            "id": "modelgate.account.zcode",
            "name": "ZCode 账号池",
            "description": "ZCode / GLM 计划账号池；账号与领取记录仍由 ZCode 面板管理。",
            "version": "3.0.0",
            "kind": "account",
            "tier": "native",
            "auth": {
                "mode": "pool",
                "schemes": ["bearer"],
                "pool": {
                    "strategy": "priority",
                    "memberKind": "account",
                    "minMembers": 1,
                    "cooldownSeconds": 60,
                    "failureThreshold": 3,
                }
            },
            "access": {
                "requiredFields": [],
                "advancedFields": ["models", "compat"],
                "privacy": { "support": "none", "zdrDefault": false }
            },
            "upstream": {
                "protocol": "anthropic",
                "transport": "zcode",
                "baseUrl": "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
                "models": [],
                "compat": { "profile": "anthropic" }
            }
        }),
        json!({
            "schema": "modelgate.plugin/v1",
            "id": "modelgate.free.kilo",
            "name": "Kilo 免费池",
            "description": "Kilo AI 公共网关的 OpenRouter 兼容端点；免密钥，按 IP 限流。",
            "version": "1.0.0",
            "kind": "free",
            "tier": "declarative",
            "auth": { "mode": "none", "schemes": [] },
            "access": {
                "requiredFields": [],
                "advancedFields": ["models"],
                "privacy": { "support": "request", "zdrDefault": true }
            },
            "upstream": {
                "protocol": "openai",
                "baseUrl": "https://api.kilo.ai/api/openrouter",
                "models": [],
                "compat": { "profile": "kilo" }
            }
        }),
        json!({
            "schema": "modelgate.plugin/v1",
            "id": "modelgate.free.opencode",
            "name": "OpenCode Zen 免费层",
            "description": "OpenCode Zen 免费模型；空 key 表示不鉴权，需要官方客户端指纹。",
            "version": "1.0.0",
            "kind": "free",
            "tier": "native",
            "auth": { "mode": "none", "schemes": [] },
            "access": {
                "requiredFields": [],
                "advancedFields": ["models"],
                "privacy": { "support": "account", "zdrDefault": true }
            },
            "upstream": {
                "protocol": "openai",
                "transport": "opencode",
                "baseUrl": "https://opencode.ai/zen/v1",
                "models": [],
                "compat": { "profile": "opencode" }
            }
        }),
    ]
}

fn forbidden_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "apikey"
            | "api_key"
            | "apikeyenv"
            | "apikeystore"
            | "token"
            | "access_token"
            | "refresh_token"
            | "secret"
            | "client_secret"
            | "password"
            | "cookie"
            | "authorization"
    ) || key.ends_with("_token")
        || key.contains("apikey")
}

fn find_forbidden_key(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let current = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if forbidden_key(key) {
                    return Some(current);
                }
                if let Some(found) = find_forbidden_key(value, &current) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, value)| find_forbidden_key(value, &format!("{path}[{index}]"))),
        _ => None,
    }
}

fn validate_manifest(manifest: &Value) -> Result<(), String> {
    if !manifest.is_object() {
        return Err("manifest 必须是 JSON 对象".into());
    }
    if let Some(found) = find_forbidden_key(manifest, "") {
        return Err(format!("manifest 禁止包含密钥字段：{found}"));
    }
    let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("manifest.id 不合法（字母/数字/-/_/.）".into());
    }
    let schema = manifest
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("modelgate.plugin/v1");
    if schema != "modelgate.plugin/v1" {
        return Err(format!("不支持的 manifest schema「{schema}」"));
    }
    let kind = manifest.get("kind").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "provider" | "free" | "account") {
        return Err("manifest.kind 必须是 provider/free/account".into());
    }
    let tier = manifest.get("tier").and_then(Value::as_str).unwrap_or("");
    if !matches!(tier, "declarative" | "native" | "sidecar") {
        return Err("manifest.tier 必须是 declarative/native/sidecar".into());
    }
    let upstream = manifest
        .get("upstream")
        .and_then(Value::as_object)
        .ok_or_else(|| "manifest.upstream 缺失".to_string())?;
    let base_url = upstream
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("");
    if base_url.trim().is_empty() {
        return Err("manifest.upstream.baseUrl 不能为空".into());
    }
    let protocol = upstream
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    if !matches!(
        protocol,
        "openai" | "anthropic" | "commandcode" | "opencode" | "zcode"
    ) {
        return Err(format!("不支持的协议「{protocol}」"));
    }
    let auth_mode = manifest
        .get("auth")
        .and_then(|auth| auth.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("single");
    if !matches!(auth_mode, "none" | "single" | "managed" | "pool") {
        return Err("manifest.auth.mode 必须是 none/single/managed/pool".into());
    }
    if auth_mode == "pool" {
        if upstream
            .get("apiKey")
            .or_else(|| upstream.get("apiKeyStore"))
            .or_else(|| upstream.get("apiKeyEnv"))
            .is_some()
        {
            return Err("auth.mode=pool 时 upstream 不得声明密钥来源".into());
        }
    }
    Ok(())
}

fn stored_plugins(state: &AppState) -> Vec<(String, Value, bool)> {
    let conn = state.db.read_conn();
    let mut out = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, manifest_json, enabled FROM plugins ORDER BY installed_at")
    {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        }) {
            for (id, text, enabled) in rows.flatten() {
                if let Ok(manifest) = serde_json::from_str::<Value>(&text) {
                    out.push((id, manifest, enabled));
                }
            }
        }
    }
    out
}

fn find_manifest(state: &AppState, id: &str) -> Option<(Value, bool, bool)> {
    if let Some(manifest) = builtin_manifests()
        .into_iter()
        .find(|manifest| manifest["id"] == id)
    {
        let stored = stored_plugins(state);
        let (enabled, installed) = stored
            .iter()
            .find(|(stored_id, _, _)| stored_id == id)
            .map(|(_, _, enabled)| (*enabled, true))
            .unwrap_or((true, false));
        return Some((manifest, enabled, installed));
    }
    stored_plugins(state)
        .into_iter()
        .find(|(stored_id, _, _)| stored_id == id)
        .map(|(_, manifest, enabled)| (manifest, enabled, true))
}

/// 供 AI 计划注入 manifest：只返回 manifest 本身（已禁止密钥字段）。
pub fn manifest_for(state: &AppState, id: &str) -> Option<Value> {
    find_manifest(state, id).map(|(manifest, _, _)| manifest)
}

fn attached_upstreams(state: &AppState, plugin_id: &str) -> Vec<String> {
    let cfg = state.cfg();
    cfg.upstreams
        .iter()
        .filter(|up| {
            cfg.raw
                .get("upstreams")
                .and_then(|rows| rows.get(&up.name))
                .and_then(|row| row.get("pluginId"))
                .and_then(Value::as_str)
                == Some(plugin_id)
        })
        .map(|up| up.name.clone())
        .collect()
}

fn plugin_summary(state: &AppState, manifest: &Value, enabled: bool, installed: bool) -> Value {
    let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    json!({
        "id": id,
        "name": manifest.get("name").and_then(Value::as_str).unwrap_or(id),
        "description": manifest.get("description").and_then(Value::as_str).unwrap_or(""),
        "version": manifest.get("version").and_then(Value::as_str).unwrap_or("0.0.0"),
        "kind": manifest.get("kind").and_then(Value::as_str).unwrap_or("provider"),
        "tier": manifest.get("tier").and_then(Value::as_str).unwrap_or("declarative"),
        "enabled": enabled,
        "installed": installed,
        "builtin": !installed,
        "attached": attached_upstreams(state, id),
    })
}

fn plugin_json(state: &AppState, manifest: &Value) -> Value {
    json!({
        "manifest": manifest,
        "auth": manifest.get("auth").cloned().unwrap_or_else(|| json!({"mode":"single"})),
        "access": manifest.get("access").cloned().unwrap_or_else(|| json!({})),
        "upstream": manifest.get("upstream").cloned().unwrap_or_else(|| json!({})),
    })
}

// ─────────────────────────── 计划（AI 用） ───────────────────────────

fn default_upstream_name(manifest: &Value) -> String {
    manifest
        .get("upstream")
        .and_then(|upstream| upstream.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            manifest
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("plugin")
                .rsplit('.')
                .next()
                .unwrap_or("plugin")
                .to_string()
        })
}

fn build_attach_entry(manifest: &Value, name: &str) -> Map<String, Value> {
    let upstream = manifest
        .get("upstream")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let auth_mode = manifest
        .get("auth")
        .and_then(|auth| auth.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("single");
    let mut entry = Map::new();
    for key in [
        "protocol",
        "baseUrl",
        "transport",
        "models",
        "modelMap",
        "modelOverrides",
        "compat",
        "sessionHeader",
        "anthropicVersion",
        "cliVersion",
        "cliEnvironment",
        "pauseTurn",
        "fetchModels",
        "autoModels",
        "authOptional",
    ] {
        if let Some(value) = upstream.get(key) {
            entry.insert(key.to_string(), value.clone());
        }
    }
    let profile = upstream
        .get("compat")
        .and_then(|compat| compat.get("profile"))
        .and_then(Value::as_str)
        .unwrap_or("custom");
    entry.insert("template".into(), json!(profile));
    entry.insert("templateVersion".into(), json!(1));
    entry.insert("kind".into(), json!("plugin"));
    entry.insert(
        "pluginId".into(),
        manifest.get("id").cloned().unwrap_or(json!("")),
    );
    entry.insert(
        "label".into(),
        manifest.get("name").cloned().unwrap_or_else(|| json!(name)),
    );
    entry
        .entry("fetchModels".to_string())
        .or_insert_with(|| json!(true));
    let privacy = manifest
        .get("access")
        .and_then(|access| access.get("privacy"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let zdr = privacy
        .get("zdrDefault")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let training = privacy
        .get("trainingDefault")
        .and_then(Value::as_str)
        .unwrap_or("deny");
    entry.insert(
        "privacy".into(),
        json!({ "zdr": zdr, "training": training }),
    );
    if let Some(compat) = entry.get_mut("compat").and_then(Value::as_object_mut) {
        let privacy_obj = compat
            .entry("privacy".to_string())
            .or_insert_with(|| json!({}));
        if let Some(privacy_obj) = privacy_obj.as_object_mut() {
            privacy_obj.insert("zdr".into(), json!(zdr));
            privacy_obj.insert("training".into(), json!(training));
        }
        if profile != "custom" {
            compat
                .entry("profile".to_string())
                .or_insert_with(|| json!(profile));
        }
    }
    if auth_mode == "single" {
        entry.insert(
            "apiKeyStore".into(),
            json!(crate::secret_store::upstream_id(name)),
        );
    }
    entry
}

fn pool_id_for(manifest: &Value, name: &str) -> String {
    let plugin = manifest
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("plugin")
        .rsplit('.')
        .next()
        .unwrap_or("plugin");
    format!("pool_{name}_{plugin}")
}

pub fn plan_action(
    action: &str,
    input: &Value,
    cfg: &Config,
) -> Result<PlannedPlugin, (StatusCode, String, &'static str)> {
    let manifest = if let Some(manifest) = input.get("manifest") {
        manifest.clone()
    } else {
        let id = input
            .get("pluginId")
            .or_else(|| input.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        // 这里只做计划，不访问 state；插件清单由输入携带或由调用方注入。
        input.get("manifest").cloned().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("缺少插件 manifest（pluginId={id}）"),
                "manifest_required",
            )
        })?
    };
    validate_manifest(&manifest)
        .map_err(|message| (StatusCode::BAD_REQUEST, message, "invalid_manifest"))?;
    let plugin_id = manifest
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if action == "attach_plugin" {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| default_upstream_name(&manifest));
        if let Some(existing) = cfg.upstream(&name) {
            let existing_plugin = cfg
                .raw
                .get("upstreams")
                .and_then(|rows| rows.get(&name))
                .and_then(|row| row.get("pluginId"))
                .and_then(Value::as_str);
            if existing_plugin == Some(plugin_id.as_str()) {
                return Ok(PlannedPlugin {
                    output: json!({
                        "kind": "plugin_attach",
                        "pluginId": plugin_id,
                        "name": name,
                        "alreadyAttached": true,
                        "entry": Value::Null,
                    }),
                    diff: vec![],
                    status: "ready".into(),
                });
            }
            return Err((
                StatusCode::CONFLICT,
                format!("接入「{name}」已存在且来源不是该插件"),
                "name_conflict",
            ));
        }
        let mut entry = build_attach_entry(&manifest, &name);
        let auth_mode = manifest
            .get("auth")
            .and_then(|auth| auth.get("mode"))
            .and_then(Value::as_str)
            .unwrap_or("single");
        let pool = if auth_mode == "pool" {
            let pool_id = pool_id_for(&manifest, &name);
            entry.insert("credentialPool".into(), json!(pool_id));
            Some(json!({
                "id": pool_id,
                "strategy": manifest.pointer("/auth/pool/strategy").and_then(Value::as_str).unwrap_or("priority"),
                "cooldownSeconds": manifest.pointer("/auth/pool/cooldownSeconds").and_then(Value::as_i64).unwrap_or(60),
                "failureThreshold": manifest.pointer("/auth/pool/failureThreshold").and_then(Value::as_i64).unwrap_or(3),
            }))
        } else {
            None
        };
        let mut diff: Vec<Value> = entry
            .iter()
            .map(|(key, value)| {
                json!({
                    "path": format!("upstreams.{name}.{key}"),
                    "op": "add",
                    "value": crate::ai_api::redact(value),
                })
            })
            .collect();
        if let Some(pool) = &pool {
            diff.push(json!({
                "path": format!("credential_pools.{}", pool["id"].as_str().unwrap_or("")),
                "op": "add",
                "value": {"strategy": pool["strategy"]},
            }));
        }
        let status = if auth_mode == "none" {
            "ready"
        } else {
            "needs_secret"
        };
        return Ok(PlannedPlugin {
            output: json!({
                "kind": "plugin_attach",
                "pluginId": plugin_id,
                "name": name,
                "entry": entry,
                "pool": pool,
                "alreadyAttached": false,
            }),
            diff,
            status: status.into(),
        });
    }
    if action == "detach_plugin" {
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        if cfg.upstream(name).is_none() {
            return Err((
                StatusCode::NOT_FOUND,
                format!("接入「{name}」不存在"),
                "access_not_found",
            ));
        }
        let purge = input.get("purge").and_then(Value::as_bool).unwrap_or(false);
        return Ok(PlannedPlugin {
            output: json!({
                "kind": "plugin_detach",
                "pluginId": plugin_id,
                "name": name,
                "purge": purge,
            }),
            diff: vec![json!({
                "path": format!("upstreams.{name}.pluginId"),
                "op": if purge { "remove" } else { "update" },
            })],
            status: "ready".into(),
        });
    }
    Err((
        StatusCode::BAD_REQUEST,
        format!("插件动作「{action}」不支持"),
        "invalid_action",
    ))
}

pub fn apply_attach(state: &AppState, output: &Value) -> Result<Value, Response> {
    let name = output.get("name").and_then(Value::as_str).unwrap_or("");
    if output
        .get("alreadyAttached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(json!({
            "target": format!("upstreams.{name}"),
            "alreadyAttached": true,
            "access": access::finalize_access(state, name),
        }));
    }
    let entry = output
        .get("entry")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if entry.is_empty() {
        return Err(plugin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "计划缺少 upstream 条目",
            "invalid_plan",
        ));
    }
    if let Some(pool) = output.get("pool") {
        let pool_id = pool.get("id").and_then(Value::as_str).unwrap_or("");
        if !pool_id.is_empty() && crate::credential_pool::get_pool(state, pool_id).is_none() {
            let upstream = entry
                .get("pluginId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| name.to_string());
            let strategy = pool
                .get("strategy")
                .and_then(Value::as_str)
                .unwrap_or("priority");
            let cooldown = pool
                .get("cooldownSeconds")
                .and_then(Value::as_i64)
                .unwrap_or(60);
            let threshold = pool
                .get("failureThreshold")
                .and_then(Value::as_i64)
                .unwrap_or(3);
            crate::credential_pool::create_pool(
                state,
                pool_id,
                &upstream,
                strategy,
                cooldown,
                threshold,
                entry.get("pluginId").and_then(Value::as_str),
            )
            .map_err(|message| {
                plugin_error(StatusCode::BAD_REQUEST, message, "pool_write_failed")
            })?;
        }
    }
    let next = access::next_config_with(state, name, Value::Object(entry));
    let committed = access::commit_raw(state, &next)?;
    Ok(json!({
        "target": format!("upstreams.{name}"),
        "backup": committed.get("backup").cloned().unwrap_or(Value::Null),
        "access": access::finalize_access(state, name),
        "needsSecret": output.get("pool").is_none(),
    }))
}

pub fn apply_detach(state: &AppState, output: &Value) -> Result<Value, Response> {
    let name = output.get("name").and_then(Value::as_str).unwrap_or("");
    let purge = output
        .get("purge")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut entry = state
        .cfg()
        .raw
        .get("upstreams")
        .and_then(|rows| rows.get(name))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if purge {
        let mut next = state.cfg().raw.clone();
        if let Some(rows) = next.get_mut("upstreams").and_then(Value::as_object_mut) {
            rows.remove(name);
        }
        let committed = access::commit_raw(state, &next)?;
        return Ok(json!({
            "target": format!("upstreams.{name}"),
            "purged": true,
            "backup": committed.get("backup").cloned().unwrap_or(Value::Null),
        }));
    }
    entry.remove("pluginId");
    let next = access::next_config_with(state, name, Value::Object(entry));
    let committed = access::commit_raw(state, &next)?;
    Ok(json!({
        "target": format!("upstreams.{name}"),
        "purged": false,
        "backup": committed.get("backup").cloned().unwrap_or(Value::Null),
        "access": access::finalize_access(state, name),
    }))
}

// ─────────────────────────── HTTP API ───────────────────────────

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let mut items: Vec<Value> = builtin_manifests()
        .iter()
        .map(|manifest| plugin_summary(&state, manifest, true, false))
        .collect();
    for (id, manifest, enabled) in stored_plugins(&state) {
        if let Some(existing) = items
            .iter_mut()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            *existing = plugin_summary(&state, &manifest, enabled, true);
        } else {
            items.push(plugin_summary(&state, &manifest, enabled, true));
        }
    }
    Json(json!({
        "items": items,
        "total": items.len(),
        "note": "安装不会修改你的配置；接入才会创建上游。",
    }))
    .into_response()
}

pub async fn detail(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some((manifest, enabled, installed)) = find_manifest(&state, &id) else {
        return plugin_error(
            StatusCode::NOT_FOUND,
            format!("插件「{id}」不存在"),
            "plugin_not_found",
        );
    };
    let mut value = plugin_summary(&state, &manifest, enabled, installed);
    value["detail"] = plugin_json(&state, &manifest);
    Json(json!({ "plugin": value })).into_response()
}

pub async fn install(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let manifest = body.get("manifest").cloned().unwrap_or(body);
    if let Err(message) = validate_manifest(&manifest) {
        return plugin_error(StatusCode::BAD_REQUEST, message, "invalid_manifest");
    }
    let id = manifest["id"].as_str().unwrap_or("");
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0");
    let kind = manifest
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("provider");
    let tier = manifest
        .get("tier")
        .and_then(Value::as_str)
        .unwrap_or("declarative");
    let now = now_text();
    let conn = state.db.write_conn();
    let result = conn.execute(
        "INSERT INTO plugins (id, version, kind, tier, name, manifest_json, enabled, source, installed_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 'local', ?7, ?7) ON CONFLICT(id) DO UPDATE SET version = excluded.version, kind = excluded.kind, tier = excluded.tier, name = excluded.name, manifest_json = excluded.manifest_json, updated_at = excluded.updated_at",
        rusqlite::params![
            id,
            version,
            kind,
            tier,
            manifest.get("name").and_then(Value::as_str).unwrap_or(id),
            manifest.to_string(),
            now,
        ],
    );
    drop(conn);
    match result {
        Ok(_) => {
            let _ = crate::credential_pool::record_audit(
                &state,
                "ui",
                "plugin_install",
                None,
                &format!("plugins.{id}"),
                &["manifest".to_string()],
                "ok",
                None,
            );
            Json(json!({
                "ok": true,
                "plugin": plugin_summary(&state, &manifest, true, true),
                "note": "已登记；接入请调用 attach。",
            }))
            .into_response()
        }
        Err(err) => plugin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("插件登记失败: {err}"),
            "plugin_write_failed",
        ),
    }
}

pub async fn set_enabled(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let Some((manifest, _, installed)) = find_manifest(&state, &id) else {
        return plugin_error(
            StatusCode::NOT_FOUND,
            format!("插件「{id}」不存在"),
            "plugin_not_found",
        );
    };
    if !installed {
        return plugin_error(
            StatusCode::BAD_REQUEST,
            "内置插件不能修改状态；native 插件只能整体禁用配置",
            "builtin_immutable",
        );
    }
    let conn = state.db.write_conn();
    let changed = conn.execute(
        "UPDATE plugins SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, if enabled { 1 } else { 0 }, now_text()],
    );
    drop(conn);
    match changed {
        Ok(0) => plugin_error(StatusCode::NOT_FOUND, "插件不存在", "plugin_not_found"),
        Ok(_) => Json(json!({
            "ok": true,
            "plugin": plugin_summary(&state, &manifest, enabled, true),
        }))
        .into_response(),
        Err(err) => plugin_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新失败: {err}"),
            "plugin_write_failed",
        ),
    }
}

pub async fn uninstall(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some((manifest, _, installed)) = find_manifest(&state, &id) else {
        return plugin_error(
            StatusCode::NOT_FOUND,
            format!("插件「{id}」不存在"),
            "plugin_not_found",
        );
    };
    if !installed {
        return plugin_error(
            StatusCode::BAD_REQUEST,
            "内置插件不可卸载",
            "builtin_immutable",
        );
    }
    if manifest.get("tier").and_then(Value::as_str) == Some("native") {
        return plugin_error(
            StatusCode::BAD_REQUEST,
            "native 插件不可卸载，只能禁用",
            "native_not_removable",
        );
    }
    let conn = state.db.write_conn();
    let _ = conn.execute("DELETE FROM plugins WHERE id = ?1", [&id]);
    drop(conn);
    Json(json!({
        "ok": true,
        "removed": id,
        "note": "已卸载插件；已创建的上游与凭据按规则保留。",
    }))
    .into_response()
}

/// 面板直接接入（内部仍走同一装配与保存管线）。
pub async fn attach(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some((manifest, enabled, _)) = find_manifest(&state, &id) else {
        return plugin_error(
            StatusCode::NOT_FOUND,
            format!("插件「{id}」不存在"),
            "plugin_not_found",
        );
    };
    if !enabled {
        return plugin_error(
            StatusCode::BAD_REQUEST,
            "插件已禁用；请先启用",
            "plugin_disabled",
        );
    }
    let mut input = body;
    input["manifest"] = manifest;
    let cfg = state.cfg();
    let planned = match plan_action("attach_plugin", &input, &cfg) {
        Ok(planned) => planned,
        Err((status, message, code)) => return plugin_error(status, message, code),
    };
    drop(cfg);
    match apply_attach(&state, &planned.output) {
        Ok(result) => Json(json!({
            "ok": true,
            "result": result,
            "status": planned.status,
            "needsSecret": planned.status == "needs_secret",
        }))
        .into_response(),
        Err(response) => response,
    }
}

pub async fn detach(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let Some((manifest, _, _)) = find_manifest(&state, &id) else {
        return plugin_error(
            StatusCode::NOT_FOUND,
            format!("插件「{id}」不存在"),
            "plugin_not_found",
        );
    };
    let mut input = body;
    input["manifest"] = manifest;
    let cfg = state.cfg();
    let planned = match plan_action("detach_plugin", &input, &cfg) {
        Ok(planned) => planned,
        Err((status, message, code)) => return plugin_error(status, message, code),
    };
    drop(cfg);
    match apply_detach(&state, &planned.output) {
        Ok(result) => Json(json!({ "ok": true, "result": result })).into_response(),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_manifests_are_valid_and_secret_free() {
        for manifest in builtin_manifests() {
            validate_manifest(&manifest).expect("内置 manifest 必须合法");
            assert!(
                find_forbidden_key(&manifest, "").is_none(),
                "内置 manifest 不得含密钥字段"
            );
        }
    }

    #[test]
    fn manifest_with_secret_is_rejected() {
        let manifest = json!({
            "schema": "modelgate.plugin/v1",
            "id": "test.bad",
            "kind": "provider",
            "tier": "declarative",
            "auth": {"mode": "single"},
            "upstream": {"protocol": "openai", "baseUrl": "https://x", "apiKey": "sk-x"}
        });
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(err.contains("密钥字段"));
    }

    #[test]
    fn pool_manifest_must_not_declare_key() {
        let manifest = json!({
            "schema": "modelgate.plugin/v1",
            "id": "test.pool",
            "kind": "provider",
            "tier": "declarative",
            "auth": {"mode": "pool", "pool": {"strategy": "priority"}},
            "upstream": {"protocol": "openai", "baseUrl": "https://x", "apiKeyStore": "upstream:x"}
        });
        let err = validate_manifest(&manifest).unwrap_err();
        // 通用密钥字段检查先于 pool 专项检查；两者都是明确拒绝。
        assert!(
            err.contains("不得声明密钥") || err.contains("密钥字段"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn attach_entry_uses_pool_without_key() {
        let manifest = builtin_manifests()
            .into_iter()
            .find(|m| m["id"] == "modelgate.account.zcode")
            .unwrap();
        let entry = build_attach_entry(&manifest, "zcode");
        assert_eq!(entry["kind"], "plugin");
        assert_eq!(entry["pluginId"], "modelgate.account.zcode");
        assert!(!entry.contains_key("apiKeyStore"));
        assert!(!entry.contains_key("apiKeyEnv"));
        assert_eq!(entry["privacy"]["zdr"], false);
    }
}
