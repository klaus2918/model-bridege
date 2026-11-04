//! 面板与管理接口处理器：`/ui` 页面资源、配置读写、用量与提示词查询的 HTTP 入口。
//!
//! 从 `proxy.rs` 拆出的原因：proxy.rs 早期把「转发核心」与「管理面」混在一个
//! 5000+ 行文件里；本文件承接后者，让转发内核保持单一职责。通用查询辅助
//! （`filters_from_query` / `previous_window` / `parse_ts` 等）留在 proxy.rs——
//! `api_extra.rs` 与测试也在用它们。模块开关见 `config::Modules`：`panel`
//! 关闭时这些路由整体不挂载，对应处理器成为死代码但不出现在路由表里。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::Config;
use crate::db::Filters;
use crate::proxy::{error_json, AppState};

// ---------------------------------------------------------------------------
// 面板页面资源与信息接口（panel 模块）
// ---------------------------------------------------------------------------

fn ui_page_html(source: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut html = source.to_string();
    // 资源带版本号，避免升级后浏览器复用旧 JS/CSS。
    for file in [
        "v4-base.css",
        "v4.css",
        "v4-core.js",
        "v4-config.js",
        "v4-models.js",
        "v4-usage.js",
        "v4-prompts.js",
        "v4-resources.js",
        "v4.js",
    ] {
        html = html.replace(
            &format!("href=\"{file}\""),
            &format!("href=\"{file}?v={version}\""),
        );
        html = html.replace(
            &format!("src=\"{file}\""),
            &format!("src=\"{file}?v={version}\""),
        );
    }
    html
}

pub(crate) async fn ui_index() -> Response {
    // v4 控制台是唯一入口。
    let Some(source) = crate::ui_assets::load("v4.html") else {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "面板资源缺失：请检查 ui/ 目录或用 --ui-dir 指定",
            "server_error",
            "ui_assets_missing",
        );
    };
    let html = ui_page_html(source.as_ref());
    (
        [
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        html,
    )
        .into_response()
}

pub(crate) async fn ui_classic() -> impl IntoResponse {
    // 旧面板已由 v4 完整替代；保留路径做永久跳转，避免旧书签 404。
    axum::response::Redirect::permanent("/ui/")
}

pub(crate) async fn ui_asset(Path(path): Path<String>) -> Response {
    // 只允许单层文件名（见 crate::ui_assets::sanitize），天然挡住路径穿越。
    // release 只认内嵌清单；debug/`--ui-dir` 直接读磁盘，改前端不必重编译。
    let Some(body) = crate::ui_assets::load(&path) else {
        return error_json(
            StatusCode::NOT_FOUND,
            "页面不存在",
            "invalid_request_error",
            "not_found",
        );
    };
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                crate::ui_assets::content_type(&path),
            ),
            (
                axum::http::header::CACHE_CONTROL,
                "no-store, no-cache, must-revalidate, max-age=0",
            ),
        ],
        body.into_owned(),
    )
        .into_response()
}

pub(crate) async fn api_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(json!({
        "app": "Model-Gate",
        "version": env!("CARGO_PKG_VERSION"),
        "port": state.cfg().port,
        "host": state.cfg().host,
        "upstream_count": state.cfg().upstreams.len(),
        "db_path": state.db.path.display().to_string(),
        "config_path": state.config_path.display().to_string(),
        "thinking": state.thinking.stats(),
        "server_time": chrono::Utc::now().timestamp(),
        "prompt_capture": state.cfg().prompt_capture,
        "runtime": "rust",
    }))
}

pub(crate) async fn api_proxy_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let default_upstream = cfg
        .upstreams
        .iter()
        .find(|u| u.default)
        .or_else(|| cfg.upstreams.first())
        .map(|u| u.name.clone())
        .unwrap_or_default();
    Json(json!({
        "running": true,
        "version": env!("CARGO_PKG_VERSION"),
        "host": state.cfg().host,
        "port": state.cfg().port,
        "endpoint": format!("http://{}:{}/v1", state.cfg().host, state.cfg().port),
        "access_log": state.cfg().access_log,
        "thinking": {"enabled": true, "size": 0, "max_size": 500},
        "upstream_count": state.cfg().upstreams.len(),
        "model_count": state.cfg().model_list().len(),
        "default_upstream": default_upstream,
        "db_path": state.db.path.display().to_string(),
        "config_path": state.config_path.display().to_string(),
        "uptime_seconds": state.started.elapsed().as_secs(),
        "runtime": "rust",
    }))
}

pub(crate) async fn api_config_save(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Response {
    let incoming = match payload.get("config") {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "缺少 config 对象",
                "invalid_request_error",
                "invalid_config",
            );
        }
    };
    if !incoming.is_object() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "配置必须是 JSON 对象",
            "invalid_request_error",
            "invalid_config",
        );
    }
    // 备份策略：面板的 backup 开关（缺省开启）。首次无备份时强制备份一次。
    let policy = crate::config_store::BackupPolicy::from_flag(
        payload.get("backup").and_then(Value::as_bool),
    );
    // 面板不回传明文密钥：写盘时剔除 apiKey，内存里沿用当前解析出的密钥。
    let current = state.cfg();
    let current_raw = current.raw.clone();
    let keys: Vec<(String, String, String)> = current
        .upstreams
        .iter()
        .map(|u| (u.name.clone(), u.api_key.clone(), u.key_source.clone()))
        .collect();
    drop(current);

    // 合并语义：顶层按键合并 + upstreams 按名增删改。
    // 传入的集合即最终集合 —— 面板删掉的上游从结果中消失，不再被顶回。
    let merged = crate::config_store::merge_incoming(&current_raw, &incoming);
    // 面板只认识常用字段：未渲染的兼容字段（cliVersion、compat、modelCatalog…）
    // 由 merge_incoming 逐字段保留，避免一次保存静默丢配置。
    // 明文密钥永不落盘。
    let mut to_write = merged.clone();
    crate::config_store::strip_secrets(&mut to_write);

    // 先诊断、后落盘：error 级问题不写入配置文件。
    // 带上本地密钥库解析，避免把已加密保存的密钥误报为缺失。
    let store = crate::secret_store::SecretStore::load(&state.config_path);
    let candidate = Config::from_value_with_store(merged.clone(), &store);
    let diagnostics = candidate.diagnostics();
    let errors: Vec<&crate::config::Diagnostic> =
        diagnostics.iter().filter(|d| d.level == "error").collect();
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": {
                    "message": format!("配置有 {} 处错误，未保存：{}", errors.len(), errors[0].message),
                    "type": "invalid_request_error",
                    "code": "invalid_config",
                },
                "diagnostics": diagnostics.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
                "effective": candidate.effective_upstreams(),
            })),
        )
            .into_response();
    }

    // 备份（在写入前）：存的是旧内容，替换失败也不丢原始配置。
    let backup = match crate::config_store::backup_config(&state.config_path, policy) {
        Ok(outcome) => outcome,
        Err(err) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("配置备份失败: {err}"),
                "server_error",
                "config_backup_failed",
            );
        }
    };
    // 原子写：临时文件 → 回读校验 → 替换。
    if let Err(err) = crate::config_store::save_atomic(&state.config_path, &to_write) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置写入失败: {err}"),
            "server_error",
            "config_write_failed",
        );
    }

    let mut new_cfg = candidate;
    for up in new_cfg.upstreams.iter_mut() {
        if up.api_key.is_empty() {
            if let Some((_, key, source)) = keys.iter().find(|(name, _, _)| *name == up.name) {
                up.api_key = key.clone();
                up.key_source = source.clone();
            }
        }
    }
    let count = new_cfg.upstreams.len();
    let issues: Vec<Value> = new_cfg.diagnostics().iter().map(|d| d.to_json()).collect();
    let effective = new_cfg.effective_upstreams();
    state.replace_config(new_cfg);
    state.clear_models_cache().await;
    Json(json!({
        "ok": true,
        "upstreams": count,
        "note": "已保存并热重载",
        "config_path": state.config_path.display().to_string(),
        "backup": backup_json(&backup),
        "diagnostics": issues,
        "effective": effective,
    }))
    .into_response()
}

/// 备份结果的回显结构（面板提示语直接用）。
fn backup_json(outcome: &crate::config_store::BackupOutcome) -> Value {
    json!({
        "created": outcome.created,
        "forcedFirst": outcome.forced_first,
        "path": outcome.path.as_ref().map(|p| p.display().to_string()),
    })
}

pub(crate) async fn api_config_reload(State(state): State<Arc<AppState>>) -> Response {
    match Config::load(&state.config_path) {
        Ok(cfg) => {
            let count = cfg.upstreams.len();
            state.replace_config(cfg);
            state.clear_models_cache().await;
            Json(json!({"ok": true, "upstreams": count})).into_response()
        }
        Err(err) => error_json(
            StatusCode::BAD_REQUEST,
            format!("重新载入失败: {err}"),
            "invalid_request_error",
            "config_load_failed",
        ),
    }
}

pub(crate) async fn api_pricing(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.cfg();
    let pricing = cfg.raw.get("pricing").cloned().unwrap_or_else(|| json!({}));
    let retention = cfg
        .raw
        .get("retentionDays")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Json(json!({
        "pricing": pricing,
        // 落库本位币固定人民币；美元单价按 fx.USD 换算成人民币入账。
        "currency": "CNY",
        "configured": cfg.pricing.configured(),
        "fx": { "USD": cfg.pricing.fx_usd },
        "entries": cfg.pricing.entries.len(),
        "diagnostics": cfg.pricing.issues,
        "retention_days": retention,
    }))
    .into_response()
}

pub(crate) async fn api_upstreams(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let rows: Vec<Value> = cfg
        .upstreams
        .iter()
        .map(|u| {
            json!({
                "name": u.name,
                "protocol": u.protocol,
                "transport": u.transport,
                "profile": u.compat.profile,
                "baseUrl": u.base_url,
                "chatPath": u.compat.chat_path,
                "authScheme": u.compat.auth_scheme,
                "sessionHeader": u.compat.session_header,
                "thinkingPassthrough": u.compat.thinking_passthrough,
                "maxOutputTokens": u.compat.max_output_tokens,
                "default": u.default,
                "models": u.models,
                "modelsAll": u.models,
                "keySource": u.display_key_source(),
                "keyConfigured": u.any_key_configured(),
                "apiKeyEnv": u.api_key_env,
                "apiKeyFile": u.api_key_file,
                "apiKeyAgent": u.api_key_agent,
                "apiKeyStore": u.api_key_store,
                // 逐模型密钥：只回显变量名/来源与可用性，绝不回显密钥本体。
                "modelKeys": u.model_keys.iter().map(|mk| json!({
                    "model": mk.pattern,
                    "env": mk.env_name,
                    "store": mk.store_id,
                    "source": mk.source,
                    "configured": mk.missing.is_none(),
                    "missing": mk.missing,
                })).collect::<Vec<_>>(),
                "plan": Value::Null,
                "planError": Value::Null,
            })
        })
        .collect();
    Json(json!({"upstreams": rows}))
}

pub(crate) async fn api_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let mut payload = cfg.raw.clone();
    if let Some(ups) = payload.get_mut("upstreams").and_then(Value::as_object_mut) {
        for up in cfg.upstreams.iter() {
            if let Some(target) = ups.get_mut(&up.name).and_then(Value::as_object_mut) {
                target.remove("apiKey");
                target.remove("apiKeyEnc");
                // 逐模型条目里的历史明文 apiKey 也不得回显给面板。
                if let Some(keys) = target.get_mut("modelKeys").and_then(Value::as_object_mut) {
                    for (_, spec) in keys.iter_mut() {
                        if let Some(spec) = spec.as_object_mut() {
                            spec.remove("apiKey");
                            spec.remove("apiKeyEnc");
                        }
                    }
                }
                target.insert("_keyConfigured".into(), json!(up.any_key_configured()));
                target.insert("_keySource".into(), json!(up.display_key_source()));
            }
        }
    }
    let diagnostics: Vec<Value> = cfg.diagnostics().iter().map(|d| d.to_json()).collect();
    Json(json!({
        "config": payload,
        "config_path": state.config_path.display().to_string(),
        "diagnostics": diagnostics,
        "effective": cfg.effective_upstreams(),
        "profiles": crate::provider_profiles::profiles_meta(),
        "thinking": state.thinking.stats(),
    }))
}

/// 只校验、不落盘：面板保存前可先拿结构化诊断。
pub(crate) async fn api_config_validate(Json(payload): Json<Value>) -> Response {
    let incoming = payload.get("config").cloned().unwrap_or(payload);
    if !incoming.is_object() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "配置必须是 JSON 对象",
            "invalid_request_error",
            "invalid_config",
        );
    }
    let cfg = Config::from_value(incoming);
    let issues: Vec<Value> = cfg.diagnostics().iter().map(|d| d.to_json()).collect();
    let errors = issues
        .iter()
        .filter(|d| d.get("level").and_then(Value::as_str) == Some("error"))
        .count();
    Json(json!({
        "ok": errors == 0,
        "errors": errors,
        "warnings": issues.len() - errors,
        "diagnostics": issues,
        "effective": cfg.effective_upstreams(),
        "profiles": crate::provider_profiles::profiles_meta(),
    }))
    .into_response()
}

/// 只读预览：给新增/编辑表单用，返回这对 protocol/baseUrl/profile 的生效值。
/// 不写配置、不发外网请求，也不读取密钥。
pub(crate) async fn api_config_preview(Json(payload): Json<Value>) -> Response {
    let protocol = payload
        .get("protocol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai");
    let base_url = payload
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let profile = payload
        .get("compat")
        .and_then(Value::as_object)
        .and_then(|c| c.get("profile"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("profile").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("");

    let mut upstream = serde_json::Map::new();
    upstream.insert("protocol".into(), json!(protocol));
    upstream.insert("baseUrl".into(), json!(base_url));
    if !profile.is_empty() {
        upstream.insert("compat".into(), json!({"profile": profile}));
    }
    let cfg = Config::from_value(json!({"upstreams": {"preview": upstream}}));
    let effective = cfg
        .effective_upstreams()
        .get(0)
        .cloned()
        .unwrap_or(Value::Null);
    let diagnostics: Vec<Value> = cfg.diagnostics().iter().map(|d| d.to_json()).collect();
    Json(json!({
        "effective": effective,
        "diagnostics": diagnostics,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// 用量查询接口（stats 模块）
// ---------------------------------------------------------------------------

fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_encoding::percent_decode_str(k)
                .decode_utf8_lossy()
                .to_string();
            let plus = v.replace('+', " ");
            let value = percent_encoding::percent_decode_str(&plus)
                .decode_utf8_lossy()
                .to_string();
            out.insert(key, value);
        }
    }
    out
}

fn scoped_filters(uri: &Uri) -> Filters {
    crate::proxy::filters_from_query(uri.query().unwrap_or(""), chrono::Utc::now().timestamp())
}

pub(crate) async fn api_usage_totals(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let f = scoped_filters(&uri);
    // compare=1：与「上一等长区间」对比，前端据此展示 KPI 环比箭头。
    // 口径与 Python 版一致（range=all 无可比区间时跳过）。
    let prev_window = if query_map(&uri).get("compare").is_some() {
        crate::proxy::previous_window(uri.query().unwrap_or(""), chrono::Utc::now().timestamp())
    } else {
        None
    };
    // totals 在大区间上是全范围窗口扫描，放阻塞线程池，别占住 tokio worker。
    let payload = crate::api_extra::run_blocking(move || {
        let mut payload = json!({"totals": state.db.totals(&f)});
        if let Some((prev_since, prev_until)) = prev_window {
            let mut prev = f.clone();
            prev.since_ts = Some(prev_since);
            prev.until_ts = Some(prev_until);
            payload["previous"] = state.db.totals(&prev);
        }
        payload
    })
    .await;
    Json(payload).into_response()
}

pub(crate) async fn api_usage_summary(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let group = params
        .get("group_by")
        .cloned()
        .unwrap_or_else(|| "day".into());
    let f = scoped_filters(&uri);
    let group_for_query = group.clone();
    let result =
        crate::api_extra::run_blocking(move || state.db.summary(&f, &group_for_query)).await;
    match result {
        Ok(rows) => Json(json!({"group_by": group, "rows": rows})).into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response(),
    }
}

pub(crate) async fn api_usage_list(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let sort = params.get("sort").cloned().unwrap_or_else(|| "ts".into());
    let order = params
        .get("order")
        .cloned()
        .unwrap_or_else(|| "desc".into());
    let mut f = scoped_filters(&uri);
    f.only = params.get("only").filter(|v| !v.is_empty()).cloned();
    let only = f.only.clone();
    let (list_sort, list_order) = (sort.clone(), order.clone());
    let (rows, total) = crate::api_extra::run_blocking(move || {
        (
            state
                .db
                .list_usage(&f, limit, offset, &list_sort, &list_order),
            state.db.count_usage(&f),
        )
    })
    .await;
    Json(json!({
        "rows": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
        "sort": sort,
        "order": order,
        "only": only.unwrap_or_default(),
    }))
    .into_response()
}

pub(crate) async fn api_usage_models(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let f = scoped_filters(&uri);
    let inner_state = Arc::clone(&state);
    let rows = crate::api_extra::run_blocking(move || inner_state.db.model_usage(&f, limit)).await;
    let pricing = state.cfg().pricing.clone();
    // 附带该模型的单价信息：额度制条目让面板显示「额度制」而不是 ¥0
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|mut row| {
            let model = row
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(quote) = pricing.quote("", &model) {
                row["pricing"] = quote;
            }
            row
        })
        .collect();
    Json(json!({
        "rows": rows,
        "range": params.get("range").cloned().unwrap_or_else(|| "24h".into()),
        "filters": {},
        "currency": "CNY",
        "pricing_configured": pricing.configured(),
        "fx": { "USD": pricing.fx_usd },
    }))
    .into_response()
}

pub(crate) async fn api_usage_upstreams(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 500);
    let f = scoped_filters(&uri);
    let rows = crate::api_extra::run_blocking(move || state.db.upstream_usage(&f, limit)).await;
    Json(json!({
        "rows": rows,
        "models": [],
        "range": params.get("range").cloned().unwrap_or_else(|| "24h".into()),
        "filters": {},
    }))
    .into_response()
}
