//! 接入编排层：模板目录、AccessObject 聚合、快速添加与受控保存。
//!
//! 设计见 `docs/design/refactor-2026/01-architecture.md`、`02-config-and-migration.md`。
//!
//! 兼容约束：
//! - 物理事实源仍是 `bridge.config.json`；本模块不新建第二份配置。
//! - 新字段全部可选；旧字段原样保留（保存时以上游为单位逐字段合并）。
//! - 密钥只写 `*.secrets.json`（DPAPI），配置只落 `apiKeyStore` 引用。
//! - 保存前备份、写后回读、失败恢复备份；绝不留下半写状态。

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};

use crate::config::{Compat, Config, PrivacyAdapter, TrainingPolicy, Upstream};
use crate::config_store::BackupPolicy;
use crate::proxy::{error_json, AppState};

/// 内置模板版本：模板默认值变化时递增，用于面板提示“模板已更新”。
pub const TEMPLATE_VERSION: u64 = 1;

/// 官方模板的展示与默认值。
#[derive(Debug, Clone)]
pub struct Template {
    pub id: String,
    pub label: String,
    pub description: String,
    pub kind: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key_env: String,
    pub docs_url: String,
    pub default_models: Vec<String>,
    pub auth_optional: bool,
    pub fetch_models: bool,
    pub auto_models: bool,
    pub pause_turn: bool,
    pub privacy: Value,
    pub required_fields: Vec<String>,
    pub advanced_fields: Vec<String>,
    pub compat: Compat,
}

fn adapter_support(adapter: &PrivacyAdapter) -> &'static str {
    match adapter {
        PrivacyAdapter::Header { .. } | PrivacyAdapter::Body { .. } => "request",
        PrivacyAdapter::Workspace { .. } => "account",
        PrivacyAdapter::None | PrivacyAdapter::Unsupported { .. } => "none",
    }
}

fn privacy_json(compat: &Compat) -> Value {
    let zdr_support = adapter_support(&compat.privacy.zdr_adapter);
    // 用户要求“重要隐私默认勾选”：所有官方模板默认记录 ZDR 意图；
    // 是否真正生效由 `support` 如实表达，不支持的厂商不伪造保护。
    let zdr_default = true;
    let training_support = adapter_support(&compat.privacy.training_deny_adapter);
    json!({
        "zdrDefault": zdr_default,
        "trainingDefault": "deny",
        "support": zdr_support,
        "trainingSupport": training_support,
        "adapters": compat.privacy.to_json()["adapters"].clone(),
    })
}

fn builtin_templates() -> Vec<Template> {
    let mut out = Vec::new();
    for meta in crate::provider_profiles::profiles_meta() {
        let id = meta
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let access = meta.get("access").cloned().unwrap_or_else(|| json!({}));
        let protocol = access
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("openai")
            .to_string();
        let base_url = access
            .get("baseUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let compat = crate::provider_profiles::resolve_compat(&id, &base_url, &protocol)
            .unwrap_or_else(|| {
                let mut c = Compat::default();
                c.profile = id.clone();
                c
            });
        let auth_optional = compat.auth_optional;
        let defaults = crate::provider_profiles::profiles_meta()
            .into_iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .and_then(|row| row.get("defaults").cloned())
            .unwrap_or(Value::Null);
        let fetch_models = defaults
            .get("fetchModels")
            .and_then(Value::as_bool)
            .unwrap_or(compat.profile != "commandcode");
        let auto_models = defaults
            .get("autoModels")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pause_turn = defaults
            .get("pauseTurn")
            .and_then(Value::as_bool)
            .unwrap_or(compat.profile == "commandcode");
        out.push(Template {
            label: meta
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string(),
            description: meta
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            id,
            kind: "official".into(),
            protocol,
            base_url,
            api_key_env: access
                .get("apiKeyEnv")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            docs_url: access
                .get("docsUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            default_models: access
                .get("defaultModels")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            auth_optional,
            required_fields: if auth_optional {
                vec![]
            } else {
                vec!["apiKey".into()]
            },
            advanced_fields: vec![
                "models".into(),
                "modelMap".into(),
                "modelKeys".into(),
                "modelOverrides".into(),
                "sessionHeader".into(),
                "timeoutSeconds".into(),
                "compat".into(),
            ],
            fetch_models,
            auto_models,
            pause_turn,
            privacy: privacy_json(&compat),
            compat,
        });
    }
    out
}

/// 自定义/中转模板（不依赖厂商预设）。
pub fn custom_template() -> Template {
    let mut compat = Compat::default();
    compat.profile = "openai".into();
    Template {
        id: "custom".into(),
        label: "自定义 / 中转".into(),
        description: "任意 OpenAI 兼容或 Anthropic 兼容端点；填写标识、地址、协议与密钥即可。"
            .into(),
        kind: "custom".into(),
        protocol: "openai".into(),
        base_url: String::new(),
        api_key_env: String::new(),
        docs_url: String::new(),
        default_models: vec![],
        auth_optional: false,
        fetch_models: true,
        auto_models: false,
        pause_turn: false,
        privacy: privacy_json(&compat),
        required_fields: vec![
            "name".into(),
            "baseUrl".into(),
            "protocol".into(),
            "apiKey".into(),
        ],
        advanced_fields: vec![
            "models".into(),
            "modelMap".into(),
            "modelKeys".into(),
            "modelOverrides".into(),
            "sessionHeader".into(),
            "timeoutSeconds".into(),
            "compat".into(),
        ],
        compat,
    }
}

pub fn templates() -> Vec<Template> {
    let mut rows = builtin_templates();
    rows.push(custom_template());
    rows
}

pub fn template(id: &str) -> Option<Template> {
    templates().into_iter().find(|row| row.id == id)
}

/// 对外模板目录（脱敏；不含任何密钥）。
pub fn template_json(row: &Template) -> Value {
    json!({
        "id": row.id,
        "label": row.label,
        "description": row.description,
        "kind": row.kind,
        "version": TEMPLATE_VERSION,
        "protocols": [row.protocol.clone()],
        "defaults": {
            "protocol": row.protocol,
            "baseUrl": row.base_url,
            "apiKeyEnv": row.api_key_env,
            "defaultModels": row.default_models,
            "fetchModels": row.fetch_models,
            "autoModels": row.auto_models,
            "pauseTurn": row.pause_turn,
        },
        "privacy": row.privacy,
        "requiredFields": row.required_fields,
        "advancedFields": row.advanced_fields,
        "docsUrl": row.docs_url,
    })
}

fn entry_of<'a>(raw: &'a Value, name: &str) -> Option<&'a Map<String, Value>> {
    raw.get("upstreams")?.get(name)?.as_object()
}

fn str_at<'a>(entry: &'a Map<String, Value>, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 推导模板 id：显式 template → compat.profile → 现有 profile 判定。
fn infer_template(up: &Upstream, entry: Option<&Map<String, Value>>) -> String {
    if let Some(raw) = entry {
        if let Some(template) = raw.get("template").and_then(Value::as_str) {
            if !template.trim().is_empty() {
                return template.to_string();
            }
        }
    }
    if !up.compat.profile.is_empty() {
        return up.compat.profile.clone();
    }
    crate::provider_profiles::detect_profile("", &up.protocol, &up.base_url)
}

fn infer_kind(up: &Upstream, entry: Option<&Map<String, Value>>, template_id: &str) -> String {
    if let Some(raw) = entry {
        if let Some(kind) = raw.get("kind").and_then(Value::as_str) {
            if matches!(kind, "official" | "custom" | "free" | "account" | "plugin") {
                return kind.to_string();
            }
        }
        if raw.get("pluginId").and_then(Value::as_str).is_some() {
            return "plugin".into();
        }
    }
    let known = crate::provider_profiles::is_known_profile(template_id);
    if known {
        "official".into()
    } else {
        "custom".into()
    }
}

/// 单个接入的脱敏读模型。
pub fn access_object(cfg: &Config, up: &Upstream, cooling: bool) -> Value {
    let raw = &cfg.raw;
    let entry = entry_of(raw, &up.name);
    let template_id = infer_template(up, entry);
    let kind = infer_kind(up, entry, &template_id);
    let pool_id = entry
        .and_then(|row| row.get("credentialPool"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // 池统计由 `/api/pools` 单独提供（需要数据库）；这里只表达绑定关系，
    // 避免读模型依赖全局状态。绑定池视为“已有凭据来源”，可用性由池摘要回答。
    let pool_summary = Value::Null;
    let configured = up.any_key_configured() || up.compat.auth_optional || pool_id.is_some();
    let auth_mode = if pool_id.is_some() {
        "pool"
    } else if up.compat.auth_optional && !up.key_configured() {
        "none"
    } else {
        "single"
    };
    let auth_source = if pool_id.is_some() {
        "pool"
    } else {
        up.display_key_source()
    };
    let group_ids: Vec<String> = cfg
        .groups
        .iter()
        .filter(|group| {
            group
                .members
                .iter()
                .any(|member| member.starts_with(&format!("{}/", up.name)))
        })
        .map(|group| group.id.clone())
        .collect();
    let visible = cfg.unified.visible_upstreams.is_empty()
        || cfg
            .unified
            .visible_upstreams
            .iter()
            .any(|name| name == &up.name);
    let privacy = up.compat.privacy.to_json();
    let zdr_support = adapter_support(&up.compat.privacy.zdr_adapter);
    let training_support = adapter_support(&up.compat.privacy.training_deny_adapter);
    // 停用的接入不表达健康状态：它既不参与健康检查，也不该被算进“待处理”。
    let health = if !up.enabled {
        "disabled"
    } else if cooling {
        "warn"
    } else if configured {
        "ok"
    } else if up.compat.auth_optional {
        "ok"
    } else {
        "warn"
    };
    let label = entry
        .and_then(|row| row.get("label"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&up.name);
    let diagnostics: Vec<Value> = cfg
        .diagnostics()
        .iter()
        .filter(|d| d.path.starts_with(&format!("upstreams.{}", up.name)))
        .map(|d| d.to_json())
        .collect();
    let mut privacy_out = json!({
        "zdr": up.compat.privacy.zdr.unwrap_or(false),
        "training": up.compat.privacy.training.as_str(),
        "support": zdr_support,
        "trainingSupport": training_support,
        "adapters": privacy.get("adapters").cloned().unwrap_or(Value::Null),
    });
    if up.compat.privacy.zdr == Some(true) && zdr_support == "none" {
        privacy_out["notice"] = json!("该厂商不支持请求级 ZDR；勾选仅记录意图，不会宣称已保护。");
    }
    json!({
        "id": up.name,
        "label": label,
        "kind": kind,
        "enabled": up.enabled,
        "source": {
            "type": if pool_id.is_some() { "pool" } else if template_id == "custom" { "custom" } else { "template" },
            "id": template_id,
            "pluginId": entry.and_then(|row| row.get("pluginId")).and_then(Value::as_str),
        },
        "template": template_id,
        "templateVersion": entry.and_then(|row| row.get("templateVersion")).and_then(Value::as_u64).unwrap_or(0),
        "protocol": up.protocol,
        "baseUrl": up.base_url,
        "transport": up.transport,
        "auth": {
            "mode": auth_mode,
            "scheme": up.compat.auth_scheme,
            "configured": configured,
            "source": auth_source,
            "secretRef": up.api_key_store,
            "poolId": pool_id,
            "hasSecret": up.key_configured(),
        },
        "privacy": privacy_out,
        "models": {
            "declared": up.models.len(),
            "fetched": up.models.len(),
            "aliases": up.model_map.len(),
        },
        "routing": {
            "default": up.default,
            "groups": group_ids,
            "visible": visible,
        },
        "health": {
            "state": health,
            "lastProbeAt": Value::Null,
            "latencyMs": Value::Null,
        },
        "usage": { "todayRequests": 0, "todayTokens": 0 },
        "pool": pool_summary,
        "diagnostics": diagnostics,
    })
}

fn cooling(state: &AppState, up: &Upstream) -> bool {
    let now = Instant::now();
    let health = state.route_health.lock().unwrap_or_else(|e| e.into_inner());
    health
        .iter()
        .any(|(key, until)| key.starts_with(&format!("{}\u{0}", up.name)) && *until > now)
}

/// 补充凭据池统计（需要数据库，故不放进纯读模型构造）。
fn with_pool_summary(state: &AppState, mut value: Value) -> Value {
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
        value["pool"] = summary.to_json(&pool);
        // 池存在但无可用成员时，接入状态应显示待处理。
        value["auth"]["configured"] = json!(summary.available > 0);
    }
    value
}

/// 全部接入（脱敏）。
pub fn access_list(state: &AppState) -> Vec<Value> {
    let cfg = state.cfg();
    cfg.upstreams
        .iter()
        .map(|up| with_pool_summary(state, access_object(&cfg, up, cooling(state, up))))
        .collect()
}

/// 统一的错误响应（沿用既有错误格式）。
fn access_error(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    error_json(status, message, "invalid_request_error", code)
}

fn validate_name(name: &str) -> Result<(), Response> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace) {
        return Err(access_error(
            StatusCode::BAD_REQUEST,
            "上游标识不能为空，且不能包含 / 或空白字符",
            "invalid_name",
        ));
    }
    if matches!(
        name,
        "upstreams"
            | "unified"
            | "groups"
            | "models"
            | "aliases"
            | "pricing"
            | "version"
            | "upstream"
    ) {
        return Err(access_error(
            StatusCode::BAD_REQUEST,
            format!("「{name}」是保留字段，不能作为上游标识"),
            "reserved_name",
        ));
    }
    Ok(())
}

fn validate_protocol(protocol: &str) -> Result<(), Response> {
    if matches!(
        protocol,
        "openai" | "anthropic" | "commandcode" | "opencode"
    ) {
        Ok(())
    } else {
        Err(access_error(
            StatusCode::BAD_REQUEST,
            format!("不支持的协议「{protocol}」"),
            "invalid_protocol",
        ))
    }
}

/// 保存变更：备份 → 原子写 → 回读校验 → 热加载。
pub(crate) fn commit_raw(state: &AppState, next: &Value) -> Result<Value, Response> {
    let mut to_write = next.clone();
    crate::config_store::strip_secrets(&mut to_write);
    let store = crate::secret_store::SecretStore::load(&state.config_path);
    let candidate = Config::from_value_with_store(to_write.clone(), &store);
    let diagnostics = candidate.diagnostics();
    let errors: Vec<&crate::config::Diagnostic> =
        diagnostics.iter().filter(|d| d.level == "error").collect();
    if !errors.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": {
                    "message": format!("配置有 {} 处错误，未保存：{}", errors.len(), errors[0].message),
                    "type": "invalid_request_error",
                    "code": "invalid_config",
                },
                "diagnostics": diagnostics.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            })),
        )
            .into_response());
    }
    let backup = crate::config_store::backup_config(&state.config_path, BackupPolicy::Enabled)
        .map_err(|err| {
            error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("配置备份失败: {err}"),
                "server_error",
                "config_backup_failed",
            )
        })?;
    crate::config_store::save_atomic(&state.config_path, &to_write).map_err(|err| {
        error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置写入失败: {err}"),
            "server_error",
            "config_write_failed",
        )
    })?;
    state.replace_config(candidate);
    Ok(json!({
        "backup": {
            "created": backup.created,
            "forcedFirst": backup.forced_first,
            "path": backup.path.as_ref().map(|p| p.display().to_string()),
        }
    }))
}

fn write_key(state: &AppState, name: &str, key: &str) -> Result<String, Response> {
    let mut store = crate::secret_store::SecretStore::load(&state.config_path);
    let id = crate::secret_store::upstream_id(name);
    store.set(&id, key).map_err(|err| {
        access_error(
            StatusCode::BAD_REQUEST,
            format!("密钥保存失败: {err}"),
            "secret_write_failed",
        )
    })?;
    Ok(id)
}

fn remove_key(state: &AppState, name: &str) {
    let id = crate::secret_store::upstream_id(name);
    let mut store = crate::secret_store::SecretStore::load(&state.config_path);
    let _ = store.remove(&id);
}

fn privacy_overrides(input: Option<&Value>) -> (Option<bool>, Option<String>) {
    let Some(privacy) = input.and_then(Value::as_object) else {
        return (None, None);
    };
    let zdr = privacy.get("zdr").and_then(Value::as_bool);
    let training = privacy
        .get("training")
        .and_then(Value::as_str)
        .map(str::to_string);
    (zdr, training)
}

fn apply_privacy(entry: &mut Map<String, Value>, template: &Template, input: Option<&Value>) {
    let (zdr, training) = privacy_overrides(input);
    let template_zdr = template
        .privacy
        .get("zdrDefault")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let zdr = zdr.unwrap_or(template_zdr);
    let training = training.unwrap_or_else(|| "deny".into());
    entry.insert(
        "privacy".into(),
        json!({ "zdr": zdr, "training": training }),
    );
    // 同步写入 compat.privacy：现有解析器（以及旧版本读取者）只认这里。
    let compat = entry
        .entry("compat".to_string())
        .or_insert_with(|| json!({}));
    if let Some(compat) = compat.as_object_mut() {
        let privacy = compat
            .entry("privacy".to_string())
            .or_insert_with(|| json!({}));
        if let Some(privacy) = privacy.as_object_mut() {
            privacy.insert("zdr".into(), json!(zdr));
            privacy.insert("training".into(), json!(training));
        }
    }
}

fn ensure_profile(entry: &mut Map<String, Value>, template: &Template) {
    if template.kind != "official" {
        return;
    }
    let compat = entry
        .entry("compat".to_string())
        .or_insert_with(|| json!({}));
    if let Some(compat) = compat.as_object_mut() {
        compat
            .entry("profile".to_string())
            .or_insert_with(|| json!(template.id));
    }
}

pub(crate) fn build_upstream_entry(
    template: &Template,
    name: &str,
    protocol: &str,
    base_url: &str,
    api_key: Option<&str>,
    input: &Value,
    kind: &str,
    vendor: Option<&str>,
) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("protocol".into(), json!(protocol));
    entry.insert(
        "template".into(),
        json!(if kind == "custom" {
            "custom"
        } else {
            &template.id
        }),
    );
    entry.insert("templateVersion".into(), json!(TEMPLATE_VERSION));
    entry.insert("kind".into(), json!(kind));
    let label = input
        .get("label")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| vendor.map(|v| format!("{v} / {name}")))
        .unwrap_or_else(|| template.label.clone());
    entry.insert("label".into(), json!(label));
    entry.insert("baseUrl".into(), json!(base_url));
    entry.insert("fetchModels".into(), json!(template.fetch_models));
    entry.insert("autoModels".into(), json!(template.auto_models));
    // pauseTurn 只对 CommandCode 生效；其它协议写 false 会产生
    // field_not_applicable 噪音，且没有任何语义。
    if template.pause_turn || protocol == "commandcode" {
        entry.insert("pauseTurn".into(), json!(template.pause_turn));
    }
    if let Some(models) = input.get("models").and_then(Value::as_array) {
        entry.insert("models".into(), Value::Array(models.clone()));
    } else if !template.default_models.is_empty() {
        entry.insert("models".into(), json!(template.default_models));
    }
    if let Some(model_map) = input.get("modelMap").and_then(Value::as_object) {
        entry.insert("modelMap".into(), Value::Object(model_map.clone()));
    }
    if let Some(overrides) = input.get("modelOverrides").and_then(Value::as_object) {
        entry.insert("modelOverrides".into(), Value::Object(overrides.clone()));
    }
    if let Some(fetch) = input.get("fetchModels").and_then(Value::as_bool) {
        entry.insert("fetchModels".into(), json!(fetch));
    }
    if let Some(default) = input.get("default").and_then(Value::as_bool) {
        entry.insert("default".into(), json!(default));
    }
    apply_privacy(&mut entry, template, input.get("privacy"));
    ensure_profile(&mut entry, template);
    if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
        entry.insert(
            "apiKeyStore".into(),
            json!(crate::secret_store::upstream_id(name)),
        );
        // 避免 env 优先级压过密钥库（resolve_api_key 的顺序是 env → file → agent → store）。
        entry.remove("apiKeyEnv");
        entry.remove("apiKeyFile");
        entry.remove("apiKeyAgent");
        entry.remove("apiKeyAgentPath");
        entry.remove("apiKeyAgentProvider");
        let _ = key;
    }
    entry
}

fn raw_upstreams_mut(raw: &mut Value) -> &mut Map<String, Value> {
    if !raw.get("upstreams").map(Value::is_object).unwrap_or(false) {
        raw["upstreams"] = json!({});
    }
    raw["upstreams"].as_object_mut().expect("upstreams object")
}

/// 写盘前把上游条目合并进完整配置（保留其它上游和未知字段）。
pub(crate) fn next_config_with(state: &AppState, name: &str, entry: Value) -> Value {
    let mut next = state.cfg().raw.clone();
    let upstreams = raw_upstreams_mut(&mut next);
    match upstreams.get_mut(name).and_then(Value::as_object_mut) {
        Some(existing) => {
            if let Some(entry) = entry.as_object() {
                for (key, value) in entry {
                    if value.is_null() {
                        existing.remove(key);
                    } else {
                        existing.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        None => {
            upstreams.insert(name.to_string(), entry);
        }
    }
    next
}

pub(crate) fn finalize_access(state: &AppState, name: &str) -> Value {
    let cfg = state.cfg();
    cfg.upstream(name)
        .map(|up| with_pool_summary(state, access_object(&cfg, up, cooling(state, up))))
        .unwrap_or_else(|| json!({ "id": name }))
}

/// POST /api/access/quick：官方厂商快速添加（选模板 + 填 Key）。
pub async fn quick_add(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let template_id = body
        .get("templateId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let Some(template) = template(template_id) else {
        return access_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("未知模板「{template_id}」"),
            "template_unknown",
        );
    };
    if template.kind == "custom" {
        return access_error(
            StatusCode::BAD_REQUEST,
            "自定义接入请使用 /api/access/custom",
            "invalid_template",
        );
    }
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(template_id)
        .to_string();
    if let Err(resp) = validate_name(&name) {
        return resp;
    }
    if state.cfg().upstream(&name).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "ok": false,
                "error": { "message": format!("接入「{name}」已存在"), "code": "name_conflict" },
                "access": finalize_access(&state, &name),
            })),
        )
            .into_response();
    }
    let protocol = body
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or(&template.protocol)
        .to_string();
    if let Err(resp) = validate_protocol(&protocol) {
        return resp;
    }
    let base_url = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&template.base_url)
        .to_string();
    if base_url.is_empty() {
        return access_error(
            StatusCode::BAD_REQUEST,
            "模板没有默认地址，请填写 baseUrl",
            "missing_base_url",
        );
    }
    let api_key = body.get("apiKey").and_then(Value::as_str);
    if !template.auth_optional && api_key.map(str::trim).unwrap_or("").is_empty() {
        return access_error(
            StatusCode::BAD_REQUEST,
            "缺少 apiKey（官方厂商只需填写 Key）",
            "missing_field",
        );
    }
    let entry = build_upstream_entry(
        &template, &name, &protocol, &base_url, api_key, &body, "official", None,
    );
    let next = next_config_with(&state, &name, Value::Object(entry));
    // 先写密钥：失败时不改配置；配置写失败时回滚刚写入的密钥。
    let wrote_key = if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        if let Err(resp) = write_key(&state, &name, key) {
            return resp;
        }
        true
    } else {
        false
    };
    if let Err(resp) = commit_raw(&state, &next) {
        if wrote_key {
            remove_key(&state, &name);
        }
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "access": finalize_access(&state, &name),
        "diagnostics": state.cfg().diagnostics().iter()
            .filter(|d| d.path.starts_with(&format!("upstreams.{name}")))
            .map(|d| d.to_json()).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// POST /api/access/custom：自定义/中转（5 项）。
pub async fn custom_add(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if let Err(resp) = validate_name(&name) {
        return resp;
    }
    if state.cfg().upstream(&name).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "ok": false,
                "error": { "message": format!("接入「{name}」已存在"), "code": "name_conflict" },
                "access": finalize_access(&state, &name),
            })),
        )
            .into_response();
    }
    let protocol = body
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("openai")
        .to_string();
    if let Err(resp) = validate_protocol(&protocol) {
        return resp;
    }
    let base_url = body
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if base_url.is_empty() {
        return access_error(
            StatusCode::BAD_REQUEST,
            "缺少 baseUrl（自定义接入必填）",
            "missing_field",
        );
    }
    let api_key = body.get("apiKey").and_then(Value::as_str);
    if api_key.map(str::trim).unwrap_or("").is_empty() {
        return access_error(
            StatusCode::BAD_REQUEST,
            "缺少 apiKey（自定义接入必填）",
            "missing_field",
        );
    }
    let template = custom_template();
    let vendor = body.get("vendor").and_then(Value::as_str);
    let entry = build_upstream_entry(
        &template, &name, &protocol, &base_url, api_key, &body, "custom", vendor,
    );
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = write_key(&state, &name, api_key.unwrap_or("")) {
        return resp;
    }
    if let Err(resp) = commit_raw(&state, &next) {
        remove_key(&state, &name);
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "access": finalize_access(&state, &name),
    }))
    .into_response()
}

/// PATCH /api/access/{id}：更新非敏感字段；不接受密钥。
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let current = state.cfg();
    let Some(up) = current.upstream(&name).cloned() else {
        return access_error(
            StatusCode::NOT_FOUND,
            format!("接入「{name}」不存在"),
            "access_not_found",
        );
    };
    drop(current);
    let mut entry = state
        .cfg()
        .raw
        .get("upstreams")
        .and_then(|u| u.get(&name))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(protocol) = body.get("protocol").and_then(Value::as_str) {
        if let Err(resp) = validate_protocol(protocol) {
            return resp;
        }
        entry.insert("protocol".into(), json!(protocol));
    }
    if let Some(base_url) = body.get("baseUrl").and_then(Value::as_str) {
        if base_url.trim().is_empty() {
            return access_error(StatusCode::BAD_REQUEST, "baseUrl 不能为空", "missing_field");
        }
        entry.insert("baseUrl".into(), json!(base_url.trim()));
    }
    for key in ["label", "transport", "pluginId", "credentialPool"] {
        if let Some(value) = body.get(key).and_then(Value::as_str) {
            entry.insert(key.to_string(), json!(value));
        }
    }
    for key in [
        "fetchModels",
        "autoModels",
        "pauseTurn",
        "default",
        "sessionHeader",
        "enabled",
    ] {
        if let Some(value) = body.get(key).and_then(Value::as_bool) {
            entry.insert(key.to_string(), json!(value));
        }
    }
    for key in ["models", "modelMap", "modelOverrides"] {
        if let Some(value) = body.get(key) {
            entry.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = body.get("timeoutSeconds") {
        match value {
            Value::Null => {
                entry.insert("timeoutSeconds".into(), Value::Null);
            }
            Value::Number(number) if number.as_u64().is_some() => {
                entry.insert("timeoutSeconds".into(), value.clone());
            }
            _ => {
                return access_error(
                    StatusCode::BAD_REQUEST,
                    "timeoutSeconds 必须是正整数或 null",
                    "invalid_field",
                );
            }
        }
    }
    if let Some(value) = body.get("compat") {
        if value.is_null() {
            entry.insert("compat".into(), Value::Null);
        } else if value.is_object() {
            entry.insert("compat".into(), value.clone());
        } else {
            return access_error(
                StatusCode::BAD_REQUEST,
                "compat 必须是 JSON 对象或 null",
                "invalid_field",
            );
        }
    }
    if body.get("privacy").is_some() {
        let template = template(&up.compat.profile).unwrap_or_else(custom_template);
        apply_privacy(&mut entry, &template, body.get("privacy"));
    }
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = commit_raw(&state, &next) {
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({ "ok": true, "access": finalize_access(&state, &name) })).into_response()
}

/// POST /api/access/{id}/secret：人工写入或替换密钥（仅本机回环）。
pub async fn write_secret(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let api_key = body.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if api_key.trim().is_empty() {
        return access_error(StatusCode::BAD_REQUEST, "apiKey 不能为空", "missing_field");
    }
    if state.cfg().upstream(&name).is_none() {
        return access_error(
            StatusCode::NOT_FOUND,
            format!("接入「{name}」不存在"),
            "access_not_found",
        );
    }
    let mut entry = state
        .cfg()
        .raw
        .get("upstreams")
        .and_then(|u| u.get(&name))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    entry.insert(
        "apiKeyStore".into(),
        json!(crate::secret_store::upstream_id(&name)),
    );
    entry.remove("apiKeyEnv");
    entry.remove("apiKeyFile");
    entry.remove("apiKeyAgent");
    entry.remove("apiKeyAgentPath");
    entry.remove("apiKeyAgentProvider");
    if let Err(resp) = write_key(&state, &name, api_key) {
        return resp;
    }
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = commit_raw(&state, &next) {
        remove_key(&state, &name);
        return resp;
    }
    Json(json!({ "ok": true, "access": finalize_access(&state, &name) })).into_response()
}

/// DELETE /api/access/{id}：删除接入；默认保留密钥。
pub async fn delete(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    if state.cfg().upstream(&name).is_none() {
        return access_error(
            StatusCode::NOT_FOUND,
            format!("接入「{name}」不存在"),
            "access_not_found",
        );
    }
    let mut next = state.cfg().raw.clone();
    if let Some(upstreams) = next.get_mut("upstreams").and_then(Value::as_object_mut) {
        upstreams.remove(&name);
    }
    if let Err(resp) = commit_raw(&state, &next) {
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({ "ok": true, "removed": name })).into_response()
}

/// GET /api/access：列表 + 模板目录摘要。
pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let items = access_list(&state);
    let diagnostics: Vec<Value> = state
        .cfg()
        .diagnostics()
        .iter()
        .map(|d| d.to_json())
        .collect();
    Json(json!({
        "items": items,
        "total": items.len(),
        "diagnostics": diagnostics,
        "templates": templates().iter().map(template_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// GET /api/access/{id}：详情（含诊断）。
pub async fn detail(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let cfg = state.cfg();
    let Some(up) = cfg.upstream(&name) else {
        return access_error(
            StatusCode::NOT_FOUND,
            format!("接入「{name}」不存在"),
            "access_not_found",
        );
    };
    Json(json!({
        "access": with_pool_summary(&state, access_object(&cfg, up, cooling(&state, up)))
    }))
    .into_response()
}

/// GET /api/access/templates：模板目录。
pub async fn list_templates() -> Response {
    Json(json!({
        "items": templates().iter().map(template_json).collect::<Vec<_>>(),
        "total": templates().len(),
        "version": TEMPLATE_VERSION,
    }))
    .into_response()
}

/// POST /api/access/{id}/probe：测试连接（脱敏，不返回密钥）。
pub async fn probe(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let cfg = state.cfg();
    let Some(up) = cfg.upstream(&name) else {
        return access_error(
            StatusCode::NOT_FOUND,
            format!("接入「{name}」不存在"),
            "access_not_found",
        );
    };
    let started = Instant::now();
    let url = format!(
        "{}{}",
        up.base_url.trim_end_matches('/'),
        up.compat.models_path
    );
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in &up.compat.extra_headers {
        if let (Ok(key), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            headers.insert(key, value);
        }
    }
    let selection = up.upstream_key();
    crate::proxy::apply_auth_headers(&mut headers, up, &selection.key);
    let result = state
        .client
        .get(&url)
        .headers(headers)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await;
    match result {
        Ok(response) => {
            let status = response.status();
            let latency = started.elapsed().as_millis() as u64;
            let body = response.text().await.unwrap_or_default();
            let models = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("data")
                        .or_else(|| v.get("models"))
                        .and_then(Value::as_array)
                        .map(|arr| arr.len())
                })
                .unwrap_or(0);
            Json(json!({
                "ok": status.is_success(),
                "status": status.as_u16(),
                "latencyMs": latency,
                "models": models,
                "message": if status.is_success() {
                    "连接成功".to_string()
                } else {
                    format!("上游返回 {}", status.as_u16())
                },
            }))
            .into_response()
        }
        Err(err) => Json(json!({
            "ok": false,
            "status": 0,
            "latencyMs": started.elapsed().as_millis() as u64,
            "message": format!("连接失败: {err}"),
        }))
        .into_response(),
    }
}

/// 供 AI 接口使用的脱敏快照。
pub fn inspect(state: &AppState, name: Option<&str>) -> Value {
    let items = access_list(state);
    let selected = match name {
        Some(name) => items
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(name))
            .cloned(),
        None => None,
    };
    json!({
        "upstreams": items,
        "selected": selected,
        "total": items.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_official_and_custom() {
        let rows = templates();
        assert!(rows.iter().any(|row| row.id == "deepseek"));
        assert!(rows.iter().any(|row| row.id == "custom"));
        let deepseek = rows.iter().find(|row| row.id == "deepseek").unwrap();
        assert_eq!(deepseek.kind, "official");
        assert!(deepseek.required_fields.contains(&"apiKey".to_string()));
        assert!(!deepseek.base_url.is_empty());
        assert!(matches!(
            deepseek.privacy["support"].as_str(),
            Some("request" | "account" | "none")
        ));
        assert_eq!(deepseek.privacy["zdrDefault"], true);
    }

    #[test]
    fn privacy_defaults_are_checked_but_support_is_honest() {
        let openai = template("openai").unwrap();
        assert_eq!(openai.privacy["support"], "none");
        // 默认勾选是“用户意图”；不支持请求级开关时不伪造保护。
        assert_eq!(openai.privacy["zdrDefault"], true);
        let kilo = template("kilo").unwrap();
        assert_eq!(kilo.privacy["support"], "request");
        assert_eq!(kilo.privacy["zdrDefault"], true);
    }

    #[test]
    fn template_json_never_contains_secret_fields() {
        for row in templates() {
            let text = template_json(&row).to_string();
            assert!(!text.contains("apiKey\":"));
            assert!(!text.contains("apiKeyStore"));
            assert!(!text.contains("secret"));
        }
    }
}
