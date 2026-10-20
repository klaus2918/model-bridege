//! 面板其余接口：日志、导出、清理、保留策略、单价、模型目录、上游探测/密钥。
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::proxy::{error_json, filters_from_query, AppState};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn params(uri: &Uri) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
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

pub async fn usage_clear(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let mut filters = crate::db::Filters {
        since_ts: body.get("from").and_then(Value::as_i64),
        until_ts: body.get("to").and_then(Value::as_i64),
        upstream: body
            .get("upstream")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        session: body
            .get("session")
            .and_then(Value::as_str)
            .map(str::to_string),
        only: None,
    };
    if filters.since_ts.is_none() && filters.until_ts.is_none() {
        // 面板传 range（24h/7d/all）；缺省按 all 清空，与 Python 版一致。
        let range = body.get("range").and_then(Value::as_str).unwrap_or("all");
        filters.since_ts = match range {
            "all" => None,
            "today" => Some(crate::proxy::today_start()),
            "1h" => Some(now() - 3600),
            "7d" => Some(now() - 7 * 86400),
            "30d" => Some(now() - 30 * 86400),
            _ => Some(now() - 86400),
        };
    }
    let removed = state.db.clear_usage(&filters);
    Json(json!({"ok": true, "removed": removed})).into_response()
}

pub async fn retention_apply(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let days = body.get("days").and_then(Value::as_i64).unwrap_or(0);
    let before = state.db.usage_count_and_size();
    let removed = state.db.delete_older_than(days);
    if removed > 0 {
        state.db.vacuum();
    }
    let after = state.db.usage_count_and_size();
    Json(json!({"ok": true, "removed": removed, "before": before, "after": after})).into_response()
}

pub async fn usage_export(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let mut filters = filters_from_query(uri.query().unwrap_or(""), now());
    if let Some(up) = query.get("upstream").filter(|v| !v.is_empty()) {
        filters.upstream = Some(up.clone());
    }
    if let Some(model) = query.get("model").filter(|v| !v.is_empty()) {
        filters.model = Some(model.clone());
    }
    if let Some(session) = query.get("session").filter(|v| !v.is_empty()) {
        filters.session = Some(session.clone());
    }
    let limit = 100_000_i64;
    let rows = state.db.list_usage(&filters, limit, 0, "ts", "asc");
    let count = rows.len();
    let format = query.get("format").cloned().unwrap_or_else(|| "csv".into());
    if format == "json" {
        let body = json!({"count": count, "rows": rows, "truncated": false});
        return Json(body).into_response();
    }
    let columns = [
        "created_at",
        "upstream",
        "protocol",
        "requested_model",
        "upstream_model",
        "prompt_tokens",
        "completion_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "total_tokens",
        "duration_ms",
        "ttft_ms",
        "status",
        "stream",
        "truncated",
        "paused",
        "error",
        "cost",
        "session",
    ];
    let mut csv = String::from("\u{feff}");
    csv.push_str(&columns.join(","));
    csv.push('\n');
    for row in &rows {
        let mut fields = Vec::new();
        for col in columns {
            let value = row.get(col).cloned().unwrap_or(Value::Null);
            let text = match value {
                Value::Null => String::new(),
                Value::String(s) => s,
                other => other.to_string(),
            };
            fields.push(escape_csv(&text));
        }
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    let filename = format!(
        "modelgate-usage-{}.csv",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    (
        StatusCode::OK,
        [
            ("content-type", "text/csv; charset=utf-8"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
            ("x-row-count", &count.to_string()),
            ("x-truncated", "0"),
        ],
        csv,
    )
        .into_response()
}

fn escape_csv(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

pub async fn logs_get(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let lines = query
        .get("lines")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(300)
        .clamp(1, 2000);
    let keyword = query.get("q").cloned().unwrap_or_default().to_lowercase();
    let level = query.get("level").cloned().unwrap_or_else(|| "all".into());
    let path = state
        .db
        .path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("modelgate.log");
    if !path.is_file() {
        return Json(json!({"path": path.display().to_string(), "lines": [],
                           "total": 0, "size": 0, "note": "日志文件尚未生成"}))
        .into_response();
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let mut matched: Vec<(usize, &str)> = all
        .iter()
        .enumerate()
        .map(|(i, line)| (i + 1, *line))
        .filter(|(_, line)| keyword.is_empty() || line.to_lowercase().contains(&keyword))
        .filter(|(_, line)| match level.as_str() {
            "error" => ["异常", "失败", "错误", "Traceback", "Error"]
                .iter()
                .any(|m| line.contains(m)),
            "warn" => ["注意", "警告", "WARN", "截断"]
                .iter()
                .any(|m| line.contains(m)),
            _ => true,
        })
        .collect();
    let total = all.len();
    let matched_count = matched.len();
    if matched.len() > lines {
        matched = matched.split_off(matched.len() - lines);
    }
    let payload: Vec<Value> = matched
        .into_iter()
        .map(|(no, line)| json!({"no": no, "text": line}))
        .collect();
    Json(json!({
        "path": path.display().to_string(),
        "size": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        "total": total,
        "matched": matched_count,
        "lines": payload,
    }))
    .into_response()
}

pub async fn logs_clear(State(state): State<Arc<AppState>>) -> Response {
    let path = state
        .db
        .path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("modelgate.log");
    let _ = std::fs::write(&path, "");
    Json(json!({"ok": true})).into_response()
}

pub async fn pricing_save(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let cfg: crate::config::Config = (**state.cfg()).clone();
    let pricing = body.get("pricing").cloned().unwrap_or_else(|| json!({}));
    // 与 /api/config 共用同一套落盘路径：合并式 + 备份 + 原子写。
    // 之前这里直接整文件覆盖，绕过了备份与回读校验（config-as-json 任务 3）。
    let policy =
        crate::config_store::BackupPolicy::from_flag(body.get("backup").and_then(Value::as_bool));
    let incoming = json!({ "pricing": pricing });
    let merged = crate::config_store::merge_incoming(&cfg.raw, &incoming);
    let mut to_write = merged.clone();
    crate::config_store::strip_secrets(&mut to_write);

    let backup = match crate::config_store::backup_config(&state.config_path, policy) {
        Ok(outcome) => outcome,
        Err(err) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("单价备份失败: {err}"),
                "server_error",
                "pricing_backup_failed",
            )
        }
    };
    if let Err(err) = crate::config_store::save_atomic(&state.config_path, &to_write) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("单价写入失败: {err}"),
            "server_error",
            "pricing_write_failed",
        );
    }
    let mut new_cfg = crate::config::Config::from_value(merged);
    new_cfg.raw = to_write;
    // 走 replace_config：它会把运行态的端口/主机回填到 raw，
    // 避免端口降级场景把文件里的旧端口写回（与 /api/config 同一口径）。
    state.replace_config(new_cfg);
    Json(json!({"ok": true, "pricing": pricing, "currency": "CNY",
    "config_path": state.config_path.display().to_string(),
    "backup": {
        "created": backup.created,
        "forcedFirst": backup.forced_first,
        "path": backup.path.as_ref().map(|p| p.display().to_string()),
    }}))
    .into_response()
}

/// 备份状态查询：面板展示「是否有备份 / 最近备份路径 / 可恢复哪些」。
pub async fn backups_list(State(state): State<Arc<AppState>>) -> Response {
    let path = state.config_path.clone();
    let backups = crate::config_store::list_backups(&path);
    Json(json!({
        "ok": true,
        "config_path": path.display().to_string(),
        "hasBackup": !backups.is_empty(),
        "backups": backups
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<String>>(),
    }))
    .into_response()
}

/// 恢复上一次备份：把 `.bak` 覆盖回配置文件并热重载。
///
/// 恢复前会把当前内容另存一份 `.bak.pre-restore`，恢复动作本身可回退。
pub async fn backups_restore(State(state): State<Arc<AppState>>) -> Response {
    let path = state.config_path.clone();
    let used = match crate::config_store::restore_backup(&path) {
        Ok(used) => used,
        Err(err) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!("恢复失败: {err}"),
                "invalid_request_error",
                "backup_restore_failed",
            );
        }
    };
    // 热重载：恢复后立即生效。
    match crate::config::Config::load(&path) {
        Ok(cfg) => {
            let count = cfg.upstreams.len();
            state.replace_config(cfg);
            state.clear_models_cache().await;
            Json(json!({
                "ok": true,
                "restored_from": used.display().to_string(),
                "config_path": path.display().to_string(),
                "upstreams": count,
            }))
            .into_response()
        }
        Err(err) => error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("恢复后配置无法加载: {err}"),
            "server_error",
            "config_load_failed",
        ),
    }
}

pub async fn models_catalog(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let (upstreams, aliases, default_model) = {
        let cfg = state.cfg();
        (
            cfg.upstreams.clone(),
            cfg.aliases.clone(),
            cfg.default_model.clone(),
        )
    };
    let cache = state.refresh_models_cache(false).await;
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |id: &str, owner: &str, source: &str, rows: &mut Vec<Value>| {
        if id.is_empty() {
            return;
        }
        if let Some(existing) = rows.iter_mut().find(|r| r["model"] == json!(id)) {
            if let Some(list) = existing.get_mut("upstreams").and_then(Value::as_array_mut) {
                if !owner.is_empty() && !list.iter().any(|v| v == &json!(owner)) {
                    list.push(json!(owner));
                }
            }
            if let Some(list) = existing.get_mut("sources").and_then(Value::as_array_mut) {
                if !list.iter().any(|v| v == &json!(source)) {
                    list.push(json!(source));
                }
            }
            return;
        }
        if seen.insert(id.to_string()) {
            rows.push(json!({
                "model": id,
                "upstreams": if owner.is_empty() { Vec::<Value>::new() } else { vec![json!(owner)] },
                "aliases": [],
                "sources": [source],
                "default": false,
                "costHint": Value::Null,
                "minPlan": Value::Null,
            }));
        }
    };
    for up in &upstreams {
        for model in &up.models {
            push(model, &up.name, "explicit", &mut rows);
        }
    }
    for id in &cache.ids {
        let owner = cache.owner.get(id).map(String::as_str).unwrap_or("");
        push(id, owner, "fetched", &mut rows);
    }
    for (alias, target) in &aliases {
        for row in rows.iter_mut() {
            if row["model"] == json!(target) {
                if let Some(list) = row.get_mut("aliases").and_then(Value::as_array_mut) {
                    if !list.iter().any(|v| v == &json!(alias)) {
                        list.push(json!(alias));
                    }
                }
            }
        }
    }
    if let Some(default) = &default_model {
        for row in rows.iter_mut() {
            if row["model"] == json!(default) {
                row["default"] = json!(true);
            }
        }
    }
    let keyword = query.get("q").cloned().unwrap_or_default().to_lowercase();
    let upstream_filter = query.get("upstream").cloned().unwrap_or_default();
    if !keyword.is_empty() {
        rows.retain(|r| {
            r["model"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains(&keyword)
                || r["aliases"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .any(|v| v.as_str().unwrap_or("").to_lowercase().contains(&keyword))
                    })
                    .unwrap_or(false)
        });
    }
    if !upstream_filter.is_empty() {
        rows.retain(|r| {
            r["upstreams"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .any(|v| v.as_str() == Some(upstream_filter.as_str()))
                })
                .unwrap_or(false)
        });
    }
    rows.sort_by(|a, b| {
        let da = a["default"].as_bool().unwrap_or(false);
        let db = b["default"].as_bool().unwrap_or(false);
        db.cmp(&da).then_with(|| {
            a["model"]
                .as_str()
                .unwrap_or("")
                .cmp(b["model"].as_str().unwrap_or(""))
        })
    });
    let page = query
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let page_size = query
        .get("page_size")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 500);
    let total = rows.len();
    let start = (page - 1) * page_size;
    let slice: Vec<Value> = rows.into_iter().skip(start).take(page_size).collect();
    Json(json!({
        "rows": slice,
        "total": total,
        "page": page,
        "page_size": page_size,
        "sort": query.get("sort").cloned().unwrap_or_else(|| "model".into()),
        "order": query.get("order").cloned().unwrap_or_else(|| "asc".into()),
        "currency": "CNY",
        "pricing_configured": false,
        "upstreams": upstreams.iter().map(|u| json!({
            "name": u.name, "protocol": u.protocol, "models": u.models.len(),
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// 查询单个上游的模型清单（面板「查询清单」· GET）。
///
/// 通用约定：`GET {baseUrl}{compat.endpoint.models}`（默认 `/v1/models`），
/// 解析 `data` / `models` 数组里的 `id` / `slug` / `model`。
///
/// `baseUrl` 可由 query 覆盖（面板可能刚改完 baseUrl 还没保存，优先用请求值）。
/// **密钥不放在 URL**：URL 会进访问日志与浏览器历史；带密钥的查询走 POST。
pub async fn upstream_models(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    uri: Uri,
) -> Response {
    let query = params(&uri);
    let timeout = query
        .get("timeout")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(8.0)
        .clamp(2.0, 20.0);
    query_upstream_models(
        &state,
        &name,
        timeout,
        query.get("baseUrl").map(String::as_str),
        None,
    )
    .await
}

/// 查询上游模型清单（POST）：请求体可带 `baseUrl` / `apiKey`（表单当前值）。
///
/// 密钥走请求体而非 URL。
pub async fn upstream_models_post(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 name",
            "invalid_request_error",
            "invalid_request",
        );
    }
    let timeout = body
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(8.0)
        .clamp(2.0, 20.0);
    let base = body.get("baseUrl").and_then(Value::as_str);
    let key = body.get("apiKey").and_then(Value::as_str);
    query_upstream_models(&state, name, timeout, base, key).await
}

/// 查询实现（GET / POST 共用）。
///
/// - `base_override`：面板未保存的 Base URL（优先于已保存配置）。
/// - `key_override`：面板未保存的明文密钥（仅 POST 传入）。
async fn query_upstream_models(
    state: &Arc<AppState>,
    name: &str,
    timeout: f64,
    base_override: Option<&str>,
    key_override: Option<&str>,
) -> Response {
    // 已保存的上游：作为基底（鉴权来源、compat、已声明清单）。
    let saved = {
        let cfg = state.cfg();
        cfg.upstream(name).cloned()
    };
    let Some(saved) = saved else {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知上游 {name}"),
            "invalid_request_error",
            "unknown_upstream",
        );
    };

    // 表单当前值（未保存）优先：用它构造临时上游，保证「改完地址就查」查的是新地址。
    let mut upstream = saved.clone();
    let mut used_override = false;
    if let Some(base) = base_override.map(str::trim).filter(|s| !s.is_empty()) {
        upstream.base_url = crate::config::normalize_base_url(base);
        used_override = true;
    }
    if let Some(key) = key_override.map(str::trim).filter(|s| !s.is_empty()) {
        upstream.api_key = key.to_string();
        upstream.key_source = "literal".into();
        used_override = true;
    }
    // fetchModels=false 只影响「运行期自动拉取」，手动查询不受限（B3/D6）。
    upstream.fetch_models = true;

    let started = std::time::Instant::now();
    let result = state.query_upstream_models(&upstream, timeout).await;
    let elapsed_ms = started.elapsed().as_millis() as i64;

    match result {
        Ok(models) => {
            let declared: Vec<String> = saved.models.clone();
            Json(json!({
                "ok": true,
                "upstream": name,
                "models": models,
                "count": models.len(),
                "source": "endpoint",
                "path": upstream.compat.models_path,
                "declared": declared,
                "baseUrl": upstream.base_url,
                "usedOverride": used_override,
                "elapsedMs": elapsed_ms,
            }))
            .into_response()
        }
        Err(err) => {
            let status = match err.kind {
                "timeout" => StatusCode::GATEWAY_TIMEOUT,
                _ => StatusCode::BAD_GATEWAY,
            };
            (
                status,
                Json(json!({
                    "ok": false,
                    "error": {
                        "message": err.message,
                        "type": "invalid_request_error",
                        "code": "upstream_models_failed",
                    },
                    "kind": err.kind,
                    "hint": err.hint,
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    //! 「查询清单」接口层测试。
    //!
    //! 覆盖 Python 侧同名测试迁来后的关键行为（Python 实现随 rust-only 改造删除）：
    //! 成功查询、未知上游 404、baseUrl 覆盖、只读不改配置、白名单优先。
    //!
    //! 直接调用 handler（不经 router）：本 crate 只有 bin target，
    //! 集成测试（tests/）访问不到内部模块。

    use super::*;
    use crate::config::Config;
    use crate::db::Db;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;

    /// 假上游：返回固定状态码与响应体。
    fn spawn_upstream(status: &'static str, body: &'static str, serves: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(serves) {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mg-api-extra-{tag}-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 构造只有一个上游 `u` 的运行态。
    fn state_with_upstream(base_url: &str, declared: &[&str], fetch: bool) -> Arc<AppState> {
        let cfg = Config::from_value(json!({
            "modelsCacheSeconds": 300,
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": base_url,
                    "apiKey": "k",
                    "models": declared,
                    "fetchModels": fetch,
                    "autoModels": false
                }
            }
        }));
        let dir = temp_dir("state");
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        Arc::new(AppState::new(cfg, db, dir.join("c.json")))
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// 读出响应体并解析为 JSON。
    async fn body_json(resp: axum::response::Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn uri_with(path: &str, query: &str) -> Uri {
        if query.is_empty() {
            path.parse().unwrap()
        } else {
            format!("{path}?{query}").parse().unwrap()
        }
    }

    #[tokio::test]
    async fn get_returns_models_and_declared() {
        let url = spawn_upstream("200 OK", r#"{"data":[{"id":"m1"},{"id":"m2"}]}"#, 1);
        let state = state_with_upstream(&url, &["declared-model"], true);
        let resp = upstream_models(
            State(state),
            Path("u".to_string()),
            uri_with("/api/upstreams/u/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["models"], json!(["m1", "m2"]));
        assert_eq!(body["count"], json!(2));
        // declared = 该上游当前配置里已声明的模型（面板据此默认勾选）
        assert_eq!(body["declared"], json!(["declared-model"]));
        assert_eq!(body["path"], json!("/v1/models"));
        assert_eq!(body["source"], json!("endpoint"));
        // 响应体不得含密钥
        let text = body.to_string();
        assert!(!text.contains("apiKey"), "响应体泄漏了 apiKey");
        assert!(!text.contains("\"k\""), "响应体泄漏了密钥值");
    }

    #[tokio::test]
    async fn get_unknown_upstream_returns_404() {
        let url = spawn_upstream("200 OK", r#"{"data":[]}"#, 1);
        let state = state_with_upstream(&url, &[], true);
        let resp = upstream_models(
            State(state),
            Path("nope".to_string()),
            uri_with("/api/upstreams/nope/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], json!("unknown_upstream"));
    }

    #[tokio::test]
    async fn get_base_url_override_uses_query_value() {
        // 桩上游只服务 1 次；先让「已保存配置」的地址不可达，再用 query 覆盖成可达地址。
        let url = spawn_upstream("200 OK", r#"{"data":[{"id":"m1"}]}"#, 1);
        let state = state_with_upstream("http://127.0.0.1:1", &[], true);
        let resp = upstream_models(
            State(state),
            Path("u".to_string()),
            uri_with("/api/upstreams/u/models", &format!("baseUrl={url}")),
        )
        .await;
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["models"], json!(["m1"]));
        assert_eq!(body["usedOverride"], json!(true));
        // baseUrl 已归一化（去尾部 /v1）
        assert_eq!(body["baseUrl"], json!(url));
    }

    #[tokio::test]
    async fn get_preserves_auth_failure_reason() {
        let url = spawn_upstream("401 Unauthorized", r#"{"error":"unauthorized"}"#, 1);
        let state = state_with_upstream(&url, &[], true);
        let resp = upstream_models(
            State(state),
            Path("u".to_string()),
            uri_with("/api/upstreams/u/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;
        assert!(status.is_client_error() || status.is_server_error());
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["error"]["code"], json!("upstream_models_failed"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("401"),
            "应保留状态码"
        );
        assert!(
            !body["hint"].as_str().unwrap_or("").is_empty(),
            "应带修复提示"
        );
    }

    #[tokio::test]
    async fn get_works_when_fetch_models_disabled() {
        // D6：fetchModels=false 只挡运行期自动拉取，手动查询照常可用。
        let url = spawn_upstream("200 OK", r#"{"data":[{"id":"m1"}]}"#, 1);
        let state = state_with_upstream(&url, &[], false);
        let resp = upstream_models(
            State(state),
            Path("u".to_string()),
            uri_with("/api/upstreams/u/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["models"], json!(["m1"]));
    }

    #[tokio::test]
    async fn get_does_not_modify_config_file() {
        // 只读接口：查询不得改动配置文件。
        let url = spawn_upstream("200 OK", r#"{"data":[{"id":"m1"}]}"#, 1);
        let state = state_with_upstream(&url, &["m1"], true);
        let config_path = state.config_path.clone();
        std::fs::write(&config_path, b"{\"models\":[\"m1\"]}").unwrap();
        let before = std::fs::read(&config_path).unwrap();
        let resp = upstream_models(
            State(state),
            Path("u".to_string()),
            uri_with("/api/upstreams/u/models", ""),
        )
        .await;
        let (status, _) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        let after = std::fs::read(&config_path).unwrap();
        assert_eq!(before, after, "查询接口改动了配置文件");
    }

    #[tokio::test]
    async fn post_accepts_body_and_key() {
        // 面板走 POST：baseUrl 与 apiKey 都在请求体里（密钥不进 URL）。
        let url = spawn_upstream("200 OK", r#"{"data":[{"id":"m1"}]}"#, 1);
        let state = state_with_upstream("http://127.0.0.1:1", &[], true);
        let body = json!({"name": "u", "baseUrl": url, "apiKey": "secret"});
        let resp = upstream_models_post(State(state), Json(body)).await;
        let (status, payload) = body_json(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload["models"], json!(["m1"]));
        assert_eq!(payload["usedOverride"], json!(true));
        // 密钥不得出现在响应体里
        assert!(!payload.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn post_requires_name() {
        let state = state_with_upstream("http://127.0.0.1:1", &[], true);
        let resp = upstream_models_post(State(state), Json(json!({}))).await;
        let (status, body) = body_json(resp).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("invalid_request"));
    }

    #[tokio::test]
    async fn explicit_whitelist_still_wins_over_fetched_catalog() {
        // 显式 models 白名单非空时，对外 /v1/models 仍只返回白名单（不被查询影响）。
        let cfg = Config::from_value(json!({
            "models": ["only-whitelisted"],
            "upstreams": {
                "u": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "models": ["declared"]}
            }
        }));
        assert_eq!(cfg.model_list(), vec!["only-whitelisted"]);
    }
}

pub async fn upstreams_health(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let timeout = query
        .get("timeout")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(6.0)
        .clamp(2.0, 20.0);
    let cfg = state.cfg();
    let mut rows = Vec::new();
    for up in &cfg.upstreams {
        rows.push(probe(&state, up, timeout).await);
    }
    Json(json!({"checked_at": now(), "rows": rows})).into_response()
}

pub async fn upstream_probe(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let timeout = body
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(6.0)
        .clamp(2.0, 20.0);
    let up = {
        let cfg = state.cfg();
        cfg.upstream(name).cloned()
    };
    match up {
        Some(up) => Json(json!({"row": probe(&state, &up, timeout).await})).into_response(),
        None => error_json(
            StatusCode::NOT_FOUND,
            format!("未知上游 {name}"),
            "invalid_request_error",
            "unknown_upstream",
        ),
    }
}

async fn probe(state: &AppState, up: &crate::config::Upstream, timeout: f64) -> Value {
    let url = format!("{}/v1/models", up.base_url.trim_end_matches('/'));
    let mut req = state
        .client
        .get(&url)
        .timeout(std::time::Duration::from_secs_f64(timeout));
    if !up.api_key.is_empty() {
        req = req.header("authorization", format!("Bearer {}", up.api_key));
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            json!({
                "name": up.name,
                "ok": resp.status().is_success(),
                "status": status,
                "message": if resp.status().is_success() { "模型列表可访问".to_string() }
                           else { format!("HTTP {status}") },
                "hint": if resp.status().is_success() { "" }
                        else { "检查 apiKey / baseUrl / 网络；CommandCode 无 /v1/models，属正常" },
            })
        }
        Err(err) => json!({
            "name": up.name,
            "ok": false,
            "status": 0,
            "message": err.to_string(),
            "hint": "检查 baseUrl 是否可达、端口是否监听、本机代理是否拦截",
        }),
    }
}

pub async fn upstream_secret(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let key = body.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if key.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "apiKey 不能为空",
            "invalid_request_error",
            "empty_key",
        );
    }
    let mut cfg: crate::config::Config = (**state.cfg()).clone();
    let mut found = false;
    for up in cfg.upstreams.iter_mut() {
        if up.name == name {
            up.api_key = key.to_string();
            up.key_source = "literal".into();
            found = true;
        }
    }
    if !found {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知上游 {name}"),
            "invalid_request_error",
            "unknown_upstream",
        );
    }
    state.cfg.store(Arc::new(cfg));
    Json(json!({"ok": true, "name": name, "note": "密钥仅驻内存，不落盘"})).into_response()
}
