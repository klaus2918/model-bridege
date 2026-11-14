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

/// 密钥写入目标：与设计文档「密钥来源互斥四选一」对应。
///
/// - `Store`：DPAPI 加密库（推荐，Windows）；配置只留 `apiKeyStore` 引用。
/// - `Env`：写入用户级环境变量并让配置引用 `apiKeyEnv`。
/// - `File`：写入用户指定的凭据文件并让配置引用 `apiKeyFile`/`apiKeyAgentPath`。
///
/// 互斥语义（设计意图）：写入任何一种来源时移除其它来源引用，
/// 避免 env 优先级压过 store 导致新写入的密钥不生效。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KeyTarget {
    Store,
    Env { env_name: Option<String> },
    File {
        file_path: Option<String>,
        file_format: Option<String>,
        field: Option<String>,
    },
}

/// 从请求体解析写入目标（`keyTarget` / `target`，缺省 store）。
pub(crate) fn parse_key_target(body: &Value) -> Result<KeyTarget, Response> {
    let raw = body
        .get("keyTarget")
        .or_else(|| body.get("target"))
        .and_then(Value::as_str)
        .unwrap_or("store")
        .trim()
        .to_ascii_lowercase();
    let field = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match raw.as_str() {
        "" | "store" => Ok(KeyTarget::Store),
        "env" => Ok(KeyTarget::Env {
            env_name: field("envName"),
        }),
        "file" => Ok(KeyTarget::File {
            file_path: field("filePath"),
            file_format: field("fileFormat"),
            field: field("field"),
        }),
        other => Err(access_error(
            StatusCode::BAD_REQUEST,
            format!("未知密钥写入目标「{other}」：只支持 store / env / file"),
            "invalid_key_target",
        )),
    }
}

/// `persist_credential` 的结果：密钥已真正落盘后，配置条目应持有的引用。
#[derive(Clone, Debug)]
pub(crate) enum CredentialRef {
    Store { id: String },
    Env { env_name: String },
    File { path: String, json_root: bool, field: String },
}

impl CredentialRef {
    /// 响应里的人类可读说明（不含密钥值）。
    pub(crate) fn describe(&self) -> Value {
        match self {
            CredentialRef::Store { id } => json!({ "target": "store", "ref": id }),
            CredentialRef::Env { env_name } => json!({
                "target": "env",
                "envName": env_name,
                "note": env_target_note(),
            }),
            CredentialRef::File { path, json_root, field } => json!({
                "target": "file",
                "filePath": path,
                "field": field,
                "jsonRoot": json_root,
            }),
        }
    }
}

fn env_target_note() -> String {
    if cfg!(windows) {
        "已写入 Windows 用户环境变量（HKCU\\Environment）；新进程自动继承".to_string()
    } else {
        "已写入用户 shell 配置（~/.bashrc 托管块，~/.zshrc 若存在则同步）；新开终端自动继承".to_string()
    }
}

/// 把密钥真正写到目标位置（加密库 / 用户环境变量 / 凭据文件），
/// 返回配置条目应写入的引用。失败时目标存储零残留（env 变量写失败即返回错误）。
pub(crate) fn persist_credential(
    state: &AppState,
    name: &str,
    key: &str,
    target: &KeyTarget,
) -> Result<CredentialRef, Response> {
    match target {
        KeyTarget::Store => {
            let id = write_key(state, name, key)?;
            Ok(CredentialRef::Store { id })
        }
        KeyTarget::Env { env_name } => {
            let env_name = env_name.clone().unwrap_or_default();
            if env_name.is_empty() {
                return Err(access_error(
                    StatusCode::BAD_REQUEST,
                    "缺少密钥环境变量名：请填写 envName",
                    "missing_env_name",
                ));
            }
            if !crate::key_file::is_valid_env_name(&env_name) {
                return Err(access_error(
                    StatusCode::BAD_REQUEST,
                    format!("环境变量名不合法：{env_name}"),
                    "invalid_env_name",
                ));
            }
            crate::user_env::write_user_var(&env_name, key).map_err(|err| {
                access_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("写入用户变量 {env_name} 失败：{err}"),
                    "write_user_var_failed",
                )
            })?;
            // Windows 环境块是启动快照：靠会话缓存让本进程立刻读到新值。
            crate::user_env::remember_session_var(&env_name, key);
            crate::user_env::notify_environment_changed();
            Ok(CredentialRef::Env { env_name })
        }
        KeyTarget::File {
            file_path,
            file_format,
            field,
        } => {
            let path_raw = file_path.clone().unwrap_or_default();
            if path_raw.is_empty() {
                return Err(access_error(
                    StatusCode::BAD_REQUEST,
                    "缺少凭据文件路径：请填写 filePath",
                    "missing_file_path",
                ));
            }
            let format_raw = file_format.clone().unwrap_or_else(|| "json".to_string());
            let format = match crate::key_file::KeyFileFormat::parse(&format_raw) {
                Some(f) => f,
                None => {
                    return Err(access_error(
                        StatusCode::BAD_REQUEST,
                        format!("未知凭据文件格式 {format_raw}：只支持 json / env"),
                        "invalid_file_format",
                    ))
                }
            };
            // `.env` 必须有变量名；JSON 缺省写根字段 `apiKey`。
            let field_name = field.clone().unwrap_or_default();
            let path = crate::config::expand_home(&path_raw);
            if !path.is_absolute() {
                return Err(access_error(
                    StatusCode::BAD_REQUEST,
                    format!("凭据文件路径必须是绝对路径：{path_raw}"),
                    "invalid_file_path",
                ));
            }
            // 禁止把密钥写进 Model-Gate 自己的配置文件。
            let protected = vec![state.config_path.clone()];
            let written =
                match crate::key_file::write_key_file(&path, format, &field_name, key, &protected) {
                    Ok(w) => w,
                    Err(err) => {
                        return Err(access_error(
                            match err {
                                crate::key_file::KeyFileError::Invalid(_) => {
                                    StatusCode::BAD_REQUEST
                                }
                                crate::key_file::KeyFileError::Io(_) => {
                                    StatusCode::INTERNAL_SERVER_ERROR
                                }
                            },
                            err.to_string(),
                            "write_key_file_failed",
                        ))
                    }
                };
            // 回读校验：用与运行时相同的读取语义确认密钥真的落到了文件里。
            if !crate::key_file::verify_key_file(&written.path, &written.field) {
                return Err(access_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "写入 {} 后回读校验失败，请检查文件格式与字段名",
                        written.path.display()
                    ),
                    "verify_key_file_failed",
                ));
            }
            let json_root = matches!(format, crate::key_file::KeyFileFormat::Json)
                && written.field == "apiKey";
            Ok(CredentialRef::File {
                path: written.path.display().to_string(),
                json_root,
                field: written.field,
            })
        }
    }
}

/// 按互斥语义把凭据引用写入上游条目：只保留选中来源，移除其它来源引用。
pub(crate) fn apply_credential_ref(entry: &mut Map<String, Value>, credential: &CredentialRef) {
    match credential {
        CredentialRef::Store { id } => {
            entry.insert("apiKeyStore".into(), json!(id));
            for key in SOURCE_KEYS_ENV_FILE_AGENT {
                entry.remove(key);
            }
        }
        CredentialRef::Env { env_name } => {
            entry.insert("apiKeyEnv".into(), json!(env_name));
            for key in SOURCE_KEYS_STORE_FILE_AGENT {
                entry.remove(key);
            }
        }
        CredentialRef::File {
            path,
            json_root,
            field,
        } => {
            entry.remove("apiKeyStore");
            entry.remove("apiKeyEnv");
            entry.remove("apiKeyAgent");
            if *json_root {
                // 根字段 apiKey 走 apiKeyFile；其余字段走 agent 自定义路径。
                entry.insert("apiKeyFile".into(), json!(path));
                entry.remove("apiKeyAgentPath");
                entry.remove("apiKeyAgentProvider");
            } else {
                entry.insert("apiKeyAgentPath".into(), json!(path));
                entry.insert("apiKeyAgentProvider".into(), json!(field));
                entry.remove("apiKeyFile");
            }
        }
    }
}

/// store 目标要移除的其它来源引用。
const SOURCE_KEYS_ENV_FILE_AGENT: [&str; 5] = [
    "apiKeyEnv",
    "apiKeyFile",
    "apiKeyAgent",
    "apiKeyAgentPath",
    "apiKeyAgentProvider",
];

/// env 目标要移除的其它来源引用。
const SOURCE_KEYS_STORE_FILE_AGENT: [&str; 5] = [
    "apiKeyStore",
    "apiKeyFile",
    "apiKeyAgent",
    "apiKeyAgentPath",
    "apiKeyAgentProvider",
];

/// 配置写失败时撤销已落盘的密钥。store 撤加密库条目；
/// env / file 的本体写在用户环境（shell rc / 用户文件）里，
/// 配置没写引用就不会被网关读取，保留本体，不替用户清理。
fn rollback_credential(state: &AppState, name: &str, credential: Option<&CredentialRef>) {
    if matches!(credential, Some(CredentialRef::Store { .. })) {
        remove_key(state, name);
    }
}

/// env / file 目标缺省沿用上游已有的引用：
/// envName 缺省取当前 `apiKeyEnv`；filePath 缺省取当前 `apiKeyFile` / `apiKeyAgentPath`
/// （按扩展名猜测格式）；field 缺省取当前 `apiKeyAgentProvider`。
fn with_default_target_refs(state: &AppState, name: &str, target: KeyTarget) -> KeyTarget {
    let entry = state
        .cfg()
        .raw
        .get("upstreams")
        .and_then(|u| u.get(name))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let raw_field = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match target {
        KeyTarget::Env { env_name: None } => KeyTarget::Env {
            env_name: raw_field("apiKeyEnv"),
        },
        KeyTarget::File {
            file_path: None,
            file_format,
            field,
        } => {
            let path = raw_field("apiKeyFile").or_else(|| raw_field("apiKeyAgentPath"));
            let file_format = file_format.or_else(|| {
                path.as_ref().map(|p| {
                    if p.ends_with(".env") {
                        "env".to_string()
                    } else {
                        "json".to_string()
                    }
                })
            });
            KeyTarget::File {
                file_path: path,
                file_format,
                field: field.or_else(|| raw_field("apiKeyAgentProvider")),
            }
        }
        other => other,
    }
}

/// 删除接入时清理路由组 / 统一分组对该上游的引用：
/// 不清理会让 commit_raw 的诊断（group_member_unresolved 等）阻断保存。
/// 返回被清理的引用位置（供响应与 UI 提示）。
pub(crate) fn strip_upstream_refs(next: &mut Value, name: &str) -> Vec<String> {
    let mut cleaned: Vec<String> = Vec::new();
    // 路由组成员：`name/模型` 前缀成员（斜杠边界避免 go/golang 误伤）。
    if let Some(groups) = next.get_mut("groups").and_then(Value::as_object_mut) {
        for (gid, members) in groups.iter_mut() {
            if let Some(arr) = members.as_array_mut() {
                let before = arr.len();
                arr.retain(|m| {
                    !m.as_str()
                        .map(|s| s.strip_prefix(name).unwrap_or("").starts_with('/'))
                        .unwrap_or(false)
                });
                if arr.len() != before {
                    cleaned.push(format!("groups.{gid}"));
                }
            }
        }
    }
    // 统一分组：命名分组成员上游 + 可见上游白名单。
    if let Some(unified) = next.get_mut("unified").and_then(Value::as_object_mut) {
        if let Some(rows) = unified.get_mut("groups").and_then(Value::as_array_mut) {
            for row in rows.iter_mut() {
                let Some(obj) = row.as_object_mut() else {
                    continue;
                };
                if let Some(ups) = obj.get_mut("upstreams").and_then(Value::as_array_mut) {
                    let before = ups.len();
                    ups.retain(|u| u.as_str() != Some(name));
                    if ups.len() != before {
                        let id = obj
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("(无 id)")
                            .to_string();
                        cleaned.push(format!("unified.groups.{id}"));
                    }
                }
            }
        }
        if let Some(vis) = unified.get_mut("visibleUpstreams").and_then(Value::as_array_mut) {
            let before = vis.len();
            vis.retain(|u| u.as_str() != Some(name));
            if vis.len() != before {
                cleaned.push("unified.visibleUpstreams".to_string());
            }
        }
    }
    cleaned
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
    credential: Option<&CredentialRef>,
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
    // 密钥引用按调用方选择的写入目标注入（互斥：只留选中来源）。
    // 密钥本体由调用方先经 persist_credential 落盘，这里只写引用。
    if let Some(credential) = credential {
        apply_credential_ref(&mut entry, credential);
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
    // 写入目标四选一（默认加密库）：解析在前，参数错误不产生任何副作用。
    let target = match parse_key_target(&body) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    // 先写密钥：失败时不改配置；配置写失败时回滚刚写入的密钥。
    let credential = match api_key.map(str::trim).filter(|s| !s.is_empty()) {
        Some(key) => match persist_credential(&state, &name, key, &target) {
            Ok(c) => Some(c),
            Err(resp) => return resp,
        },
        None => None,
    };
    let entry = build_upstream_entry(
        &template,
        &name,
        &protocol,
        &base_url,
        credential.as_ref(),
        &body,
        "official",
        None,
    );
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = commit_raw(&state, &next) {
        rollback_credential(&state, &name, credential.as_ref());
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "access": finalize_access(&state, &name),
        "credential": credential.as_ref().map(|c| c.describe()),
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
    // 写入目标四选一（默认加密库）：解析在前，参数错误不产生任何副作用。
    let target = match parse_key_target(&body) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let credential = match api_key.map(str::trim).filter(|s| !s.is_empty()) {
        Some(key) => match persist_credential(&state, &name, key, &target) {
            Ok(c) => Some(c),
            Err(resp) => return resp,
        },
        None => None,
    };
    let entry = build_upstream_entry(
        &template,
        &name,
        &protocol,
        &base_url,
        credential.as_ref(),
        &body,
        "custom",
        vendor,
    );
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = commit_raw(&state, &next) {
        rollback_credential(&state, &name, credential.as_ref());
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "access": finalize_access(&state, &name),
        "credential": credential.as_ref().map(|c| c.describe()),
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
///
/// `target` 四选一（缺省 store）：
/// - `store`：DPAPI 加密库（推荐，Windows）。互斥移除 env/file 引用 —— 设计如此，
///   env 优先级高于 store，保留会让新密钥不生效；响应里列出被移除的引用。
/// - `env`：写入用户级环境变量（Windows HKCU / Linux shell rc 托管块），
///   配置引用 `apiKeyEnv`。envName 缺省沿用当前值。
/// - `file`：写入用户指定凭据文件，配置引用 `apiKeyFile`/`apiKeyAgentPath`。
///   filePath 缺省沿用当前值。
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
    let target = match parse_key_target(&body) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let target = with_default_target_refs(&state, &name, target);
    let mut entry = state
        .cfg()
        .raw
        .get("upstreams")
        .and_then(|u| u.get(&name))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // 记录覆盖前的原有来源引用（互斥语义会移除它们；响应里如实列出）。
    let replaced: Vec<String> = [
        "apiKeyEnv",
        "apiKeyFile",
        "apiKeyAgent",
        "apiKeyAgentPath",
        "apiKeyAgentProvider",
        "apiKeyStore",
    ]
    .iter()
    .filter(|key| entry.get(**key).is_some())
    .map(|key| key.to_string())
    .collect();
    let credential = match persist_credential(&state, &name, api_key.trim(), &target) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    apply_credential_ref(&mut entry, &credential);
    let next = next_config_with(&state, &name, Value::Object(entry));
    if let Err(resp) = commit_raw(&state, &next) {
        rollback_credential(&state, &name, Some(&credential));
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "credential": credential.describe(),
        "replaced": replaced,
        "access": finalize_access(&state, &name),
    }))
    .into_response()
}

/// DELETE /api/access/{id}：删除接入；默认保留密钥。
///
/// 路由组 / 统一分组里对该上游的引用会被一并清理（响应 `cleanedRefs` 列出），
/// 否则 `group_member_unresolved` 诊断会阻断保存，用户还得先手动改分组。
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
    let cleaned_refs = strip_upstream_refs(&mut next, &name);
    if let Err(resp) = commit_raw(&state, &next) {
        return resp;
    }
    state.clear_models_cache().await;
    Json(json!({ "ok": true, "removed": name, "cleanedRefs": cleaned_refs })).into_response()
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

    /// 写入目标解析：缺省 store；keyTarget / target 两个参数名都接受。
    #[test]
    fn parse_key_target_defaults_to_store_and_validates() {
        assert_eq!(parse_key_target(&json!({})).unwrap(), KeyTarget::Store);
        assert_eq!(parse_key_target(&json!({"target": "store"})).unwrap(), KeyTarget::Store);
        assert_eq!(
            parse_key_target(&json!({"target": "env", "envName": " K " })).unwrap(),
            KeyTarget::Env { env_name: Some("K".into()) }
        );
        assert!(matches!(
            parse_key_target(&json!({"keyTarget": "file", "filePath": "/tmp/k.json"})).unwrap(),
            KeyTarget::File { .. }
        ));
        assert!(parse_key_target(&json!({"target": "bogus"})).is_err());
    }

    /// 互斥语义：写入任一来源都要移除其它来源引用，避免 env 优先级
    /// 压过新写入的密钥（这是「保存 key 后环境变量失效」的根因）。
    #[test]
    fn apply_credential_ref_keeps_only_selected_source() {
        let mut entry = Map::new();
        entry.insert("apiKeyEnv".into(), json!("OLD_VAR"));
        entry.insert("apiKeyStore".into(), json!("upstream:x"));
        // env 目标：写 apiKeyEnv，清掉 store / file / agent 引用。
        apply_credential_ref(
            &mut entry,
            &CredentialRef::Env { env_name: "NEW_VAR".into() },
        );
        assert_eq!(entry.get("apiKeyEnv"), Some(&json!("NEW_VAR")));
        assert!(entry.get("apiKeyStore").is_none());
        // store 目标：写 apiKeyStore，清掉 env / file / agent 引用。
        apply_credential_ref(
            &mut entry,
            &CredentialRef::Store { id: "upstream:y".into() },
        );
        assert_eq!(entry.get("apiKeyStore"), Some(&json!("upstream:y")));
        assert!(entry.get("apiKeyEnv").is_none());
        // file（JSON 根字段）：apiKeyFile；file（非根 / env 格式）：agent 路径。
        apply_credential_ref(
            &mut entry,
            &CredentialRef::File {
                path: "/tmp/k.json".into(),
                json_root: true,
                field: "apiKey".into(),
            },
        );
        assert_eq!(entry.get("apiKeyFile"), Some(&json!("/tmp/k.json")));
        assert!(entry.get("apiKeyStore").is_none());
        apply_credential_ref(
            &mut entry,
            &CredentialRef::File {
                path: "/tmp/k.env".into(),
                json_root: false,
                field: "K_ENV".into(),
            },
        );
        assert_eq!(entry.get("apiKeyAgentPath"), Some(&json!("/tmp/k.env")));
        assert_eq!(entry.get("apiKeyAgentProvider"), Some(&json!("K_ENV")));
        assert!(entry.get("apiKeyFile").is_none());
    }

    /// 删除接入时清理组引用：`name/模型` 成员、命名分组成员上游、可见白名单。
    /// 斜杠边界保证 go 与 go2 不互相误伤。
    #[test]
    fn strip_upstream_refs_cleans_groups_and_unified() {
        let mut next = json!({
            "groups": {
                "daily": ["go/deepseek", "x/deepseek"],
                "night": ["x/gpt", "y/gpt"],
                "prefix": ["go2/m", "go/m"]
            },
            "unified": {
                "visibleUpstreams": ["x", "y"],
                "groups": [
                    {"id": "a", "label": "A", "upstreams": ["x", "y"]},
                    {"id": "b", "label": "B", "upstreams": ["y"]}
                ]
            }
        });
        let cleaned = strip_upstream_refs(&mut next, "x");
        assert_eq!(next["groups"]["daily"], json!(["go/deepseek"]));
        assert_eq!(next["groups"]["night"], json!(["y/gpt"]));
        assert_eq!(next["unified"]["visibleUpstreams"], json!(["y"]));
        assert_eq!(next["unified"]["groups"][0]["upstreams"], json!(["y"]));
        assert_eq!(next["unified"]["groups"][1]["upstreams"], json!(["y"]));
        assert!(cleaned.iter().any(|s| s == "groups.daily"));
        assert!(cleaned.iter().any(|s| s == "groups.night"));
        assert!(cleaned.iter().any(|s| s.contains("unified.groups.a")));
        assert!(cleaned.iter().any(|s| s == "unified.visibleUpstreams"));
        // go2/m 不受 go 清理影响（斜杠边界）。
        let cleaned2 = strip_upstream_refs(&mut next, "go");
        assert_eq!(next["groups"]["prefix"], json!(["go2/m"]));
        assert!(cleaned2.iter().any(|s| s == "groups.prefix"));
    }

    /// 端到端：删除被路由组引用的接入不再被诊断阻断，引用被自动清理。
    #[tokio::test]
    async fn delete_cleans_route_group_refs_instead_of_failing() {
        let cfg = crate::config::Config::from_value(json!({
            "upstreams": {
                "x": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "MG_X_KEY", "models": ["m"]},
                "y": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "MG_Y_KEY", "models": ["m"]}
            },
            "groups": {"daily": ["x/m", "y/m"]},
            "unified": {"visibleUpstreams": ["x", "y"]}
        }));
        let dir = std::env::temp_dir().join(format!("mg-access-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(crate::db::Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let resp = delete(State(state.clone()), Path("x".into())).await;
        assert!(resp.status().is_success(), "删除应成功");
        let raw = state.cfg().raw.clone();
        assert_eq!(raw["groups"]["daily"], json!(["y/m"]));
        assert_eq!(raw["unified"]["visibleUpstreams"], json!(["y"]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
