//! 面板其余接口：日志、导出、清理、保留策略、单价、模型目录、上游探测/密钥。
use std::sync::Arc;

use axum::extract::State;
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
            let key = percent_encoding::percent_decode_str(k).decode_utf8_lossy().to_string();
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
        upstream: body.get("upstream").and_then(Value::as_str).map(str::to_string),
        model: body.get("model").and_then(Value::as_str).map(str::to_string),
        session: body.get("session").and_then(Value::as_str).map(str::to_string),
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

pub async fn retention_apply(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let days = body.get("days").and_then(Value::as_i64).unwrap_or(0);
    let before = state.db.usage_count_and_size();
    let removed = state.db.delete_older_than(days);
    if removed > 0 {
        state.db.vacuum();
    }
    let after = state.db.usage_count_and_size();
    Json(json!({"ok": true, "removed": removed, "before": before, "after": after}))
        .into_response()
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
        "created_at", "upstream", "protocol", "requested_model", "upstream_model",
        "prompt_tokens", "completion_tokens", "cache_read_tokens", "cache_write_tokens",
        "total_tokens", "duration_ms", "ttft_ms", "status", "stream", "truncated",
        "paused", "error", "cost", "session",
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
    let filename = format!("modelgate-usage-{}.csv", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    (
        StatusCode::OK,
        [
            ("content-type", "text/csv; charset=utf-8"),
            ("content-disposition", &format!("attachment; filename=\"{filename}\"")),
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
    let lines = query.get("lines").and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(300).clamp(1, 2000);
    let keyword = query.get("q").cloned().unwrap_or_default().to_lowercase();
    let level = query.get("level").cloned().unwrap_or_else(|| "all".into());
    let path = state.db.path.parent().unwrap_or(std::path::Path::new(".")).join("modelgate.log");
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
                .iter().any(|m| line.contains(m)),
            "warn" => ["注意", "警告", "WARN", "截断"]
                .iter().any(|m| line.contains(m)),
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
    let path = state.db.path.parent().unwrap_or(std::path::Path::new(".")).join("modelgate.log");
    let _ = std::fs::write(&path, "");
    Json(json!({"ok": true})).into_response()
}

pub async fn pricing_save(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let mut cfg: crate::config::Config = (**state.cfg()).clone();
    let pricing = body.get("pricing").cloned().unwrap_or_else(|| json!({}));
    if let Some(obj) = cfg.raw.as_object_mut() {
        obj.insert("pricing".into(), pricing.clone());
    }
    let text = match serde_json::to_string_pretty(&cfg.raw) {
        Ok(t) => format!("{t}\n"),
        Err(err) => return error_json(StatusCode::INTERNAL_SERVER_ERROR,
                                      format!("单价序列化失败: {err}"),
                                      "server_error", "pricing_serialize_failed"),
    };
    if let Err(err) = std::fs::write(&state.config_path, text) {
        return error_json(StatusCode::INTERNAL_SERVER_ERROR,
                          format!("单价写入失败: {err}"),
                          "server_error", "pricing_write_failed");
    }
    state.cfg.store(Arc::new(cfg));
    Json(json!({"ok": true, "pricing": pricing, "currency": "CNY"})).into_response()
}

pub async fn models_catalog(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let (upstreams, aliases, default_model) = {
        let cfg = state.cfg();
        (cfg.upstreams.clone(), cfg.aliases.clone(), cfg.default_model.clone())
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
            r["model"].as_str().unwrap_or("").to_lowercase().contains(&keyword)
                || r["aliases"].as_array().map(|a| a.iter().any(|v| {
                    v.as_str().unwrap_or("").to_lowercase().contains(&keyword)
                })).unwrap_or(false)
        });
    }
    if !upstream_filter.is_empty() {
        rows.retain(|r| r["upstreams"].as_array().map(|list| {
            list.iter().any(|v| v.as_str() == Some(upstream_filter.as_str()))
        }).unwrap_or(false));
    }
    rows.sort_by(|a, b| {
        let da = a["default"].as_bool().unwrap_or(false);
        let db = b["default"].as_bool().unwrap_or(false);
        db.cmp(&da).then_with(|| {
            a["model"].as_str().unwrap_or("").cmp(b["model"].as_str().unwrap_or(""))
        })
    });
    let page = query.get("page").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).max(1);
    let page_size = query.get("page_size").and_then(|v| v.parse::<usize>().ok()).unwrap_or(50).clamp(1, 500);
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

pub async fn upstreams_health(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let timeout = query.get("timeout").and_then(|v| v.parse::<f64>().ok()).unwrap_or(6.0).clamp(2.0, 20.0);
    let cfg = state.cfg();
    let mut rows = Vec::new();
    for up in &cfg.upstreams {
        rows.push(probe(&state, up, timeout).await);
    }
    Json(json!({"checked_at": now(), "rows": rows})).into_response()
}

pub async fn upstream_probe(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let timeout = body.get("timeout").and_then(Value::as_f64).unwrap_or(6.0).clamp(2.0, 20.0);
    let up = {
        let cfg = state.cfg();
        cfg.upstream(name).cloned()
    };
    match up {
        Some(up) => Json(json!({"row": probe(&state, &up, timeout).await})).into_response(),
        None => error_json(StatusCode::NOT_FOUND, format!("未知上游 {name}"),
                           "invalid_request_error", "unknown_upstream"),
    }
}

async fn probe(state: &AppState, up: &crate::config::Upstream, timeout: f64) -> Value {
    let url = format!("{}/v1/models", up.base_url.trim_end_matches('/'));
    let mut req = state.client.get(&url).timeout(std::time::Duration::from_secs_f64(timeout));
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

pub async fn upstream_secret(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let key = body.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if key.is_empty() {
        return error_json(StatusCode::BAD_REQUEST, "apiKey 不能为空",
                          "invalid_request_error", "empty_key");
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
        return error_json(StatusCode::NOT_FOUND, format!("未知上游 {name}"),
                          "invalid_request_error", "unknown_upstream");
    }
    state.cfg.store(Arc::new(cfg));
    Json(json!({"ok": true, "name": name, "note": "密钥仅驻内存，不落盘"})).into_response()
}
