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

/// 把同步 SQLite 查询挪出 tokio worker：面板统计在 GB 级库上是秒级操作，
/// 直接在 async fn 里跑会占死 worker 线程，连 chat 转发一起卡住。
/// JoinError 按取消传播（resume_unwind），与 `.expect` 语义一致。
pub(crate) async fn run_blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(value) => value,
        Err(err) => std::panic::resume_unwind(err.into_panic()),
    }
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

/// from/to 兼容 epoch 秒与 datetime-local 字符串（面板自定义区间直接传字符串）。
/// 不兼容字符串会把「清空自定义区间」降级成「清空全部」，属于数据破坏，必须拦住。
fn parse_body_ts(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => crate::proxy::parse_ts(s),
        _ => None,
    }
}

pub async fn usage_clear(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    let mut filters = crate::db::Filters {
        since_ts: parse_body_ts(body.get("from")),
        until_ts: parse_body_ts(body.get("to")),
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
        // 面板传 range（today/yesterday/24h/3d/7d/30d/all）；缺省按 all 清空，与 Python 版一致。
        let range = body.get("range").and_then(Value::as_str).unwrap_or("all");
        match range {
            "all" => {}
            "today" => filters.since_ts = Some(crate::proxy::today_start()),
            "yesterday" => {
                filters.since_ts = Some(crate::proxy::day_start(1));
                filters.until_ts = Some(crate::proxy::today_start());
            }
            "1h" => filters.since_ts = Some(now() - 3600),
            "24h" => filters.since_ts = Some(now() - 86400),
            "3d" => filters.since_ts = Some(now() - 3 * 86400),
            "7d" => filters.since_ts = Some(now() - 7 * 86400),
            "30d" => filters.since_ts = Some(now() - 30 * 86400),
            _ => filters.since_ts = Some(now() - 86400),
        }
    }
    let removed = run_blocking(move || state.db.clear_usage(&filters)).await;
    Json(json!({"ok": true, "removed": removed})).into_response()
}

pub async fn retention_apply(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let days = body.get("days").and_then(Value::as_i64).unwrap_or(0);
    // 「删除 + VACUUM」在 GB 级库上以分钟计，整个流程放阻塞线程池。
    let payload = run_blocking(move || {
        let before = state.db.usage_count_and_size();
        let removed = state.db.delete_older_than(days);
        if removed > 0 {
            state.db.vacuum();
        }
        let after = state.db.usage_count_and_size();
        (before, removed, after)
    })
    .await;
    let (before, removed, after) = payload;
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
    if query.get("group").map(String::as_str) == Some("session") {
        let sort = query
            .get("sort")
            .cloned()
            .unwrap_or_else(|| "last_ts".into());
        let order = query.get("order").cloned().unwrap_or_else(|| "desc".into());
        let rows =
            run_blocking(move || state.db.session_usage(&filters, 1000, &sort, &order)).await;
        let columns = [
            "session",
            "requests",
            "errors",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "cost",
            "avg_duration_ms",
            "avg_ttft_ms",
            "p95_duration_ms",
            "first_ts",
            "last_ts",
            "models_count",
            "models",
        ];
        return csv_attachment("modelgate-sessions", &columns, &rows);
    }
    let limit = 100_000_i64;
    let rows = run_blocking(move || state.db.list_usage(&filters, limit, 0, "ts", "asc")).await;
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

fn csv_attachment(prefix: &str, columns: &[&str], rows: &[Value]) -> Response {
    let mut csv = String::from("\u{feff}");
    csv.push_str(&columns.join(","));
    csv.push('\n');
    for row in rows {
        let mut fields = Vec::new();
        for col in columns {
            let value = row.get(*col).cloned().unwrap_or(Value::Null);
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
        "{prefix}-{}.csv",
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
            ("x-row-count", &rows.len().to_string()),
        ],
        csv,
    )
        .into_response()
}

fn prompt_filters(uri: &Uri) -> crate::db::PromptFilters {
    let query = params(uri);
    let f = filters_from_query(uri.query().unwrap_or(""), now());
    crate::db::PromptFilters {
        since_ts: f.since_ts,
        until_ts: f.until_ts,
        model: f.model,
        session: f.session,
        upstream: None,
        q: query.get("q").filter(|v| !v.is_empty()).cloned(),
    }
}

pub async fn prompts_list(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let filters = prompt_filters(&uri);
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let offset = query
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let sort = query.get("sort").cloned().unwrap_or_else(|| "ts".into());
    let order = query.get("order").cloned().unwrap_or_else(|| "desc".into());
    // count/list/stats/facets 全部是同步 SQLite 查询；GB 级库上任何一次
    // 都可能上百毫秒，整体挪进阻塞线程池，别占住 tokio worker。
    let inner_state = Arc::clone(&state);
    let payload = run_blocking(move || {
        let total = inner_state.db.count_prompts(&filters);
        let rows = inner_state
            .db
            .list_prompts(&filters, limit, offset, &sort, &order);
        (
            total,
            rows,
            inner_state.db.prompt_count_and_size(),
            inner_state.db.prompt_facets(),
        )
    })
    .await;
    let (total, rows, stats, facets) = payload;
    Json(json!({
        "total": total,
        "rows": rows,
        "limit": limit,
        "offset": offset,
        "capture": state.cfg().prompt_capture,
        "stats": stats,
        "facets": facets,
    }))
    .into_response()
}

/// 提示词统计概览：capture 开关 + 全表统计 + 筛选项。
/// 总览页只需要这一份轻量信息（列表查询对它毫无意义）；
/// 统计本体走 stale-while-revalidate 缓存，GB 级库上也恒为毫秒级。
pub async fn prompts_stats(State(state): State<Arc<AppState>>) -> Response {
    let inner_state = Arc::clone(&state);
    let payload = run_blocking(move || {
        (
            inner_state.db.prompt_count_and_size(),
            inner_state.db.prompt_facets(),
        )
    })
    .await;
    let (stats, facets) = payload;
    Json(json!({
        "capture": state.cfg().prompt_capture,
        "stats": stats,
        "facets": facets,
    }))
    .into_response()
}

pub async fn prompt_detail(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    // messages 是单条可达数百 KB～1MB 的大字段，载入 + JSON 解析放阻塞线程池。
    let found = run_blocking(move || {
        state.db.get_prompt(&id).map(|mut value| {
            value["neighbors"] = state.db.prompt_neighbors(&id);
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            value["call_chain"] = crate::prompt::summarize_call_chain(&messages);
            value
        })
    })
    .await;
    match found {
        Some(value) => Json(value).into_response(),
        None => error_json(
            StatusCode::NOT_FOUND,
            "提示词记录不存在",
            "invalid_request_error",
            "prompt_not_found",
        ),
    }
}

pub async fn prompt_download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(value) = state.db.get_prompt(&id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            "提示词记录不存在",
            "invalid_request_error",
            "prompt_not_found",
        );
    };
    let body = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
    let filename = format!("modelgate-prompt-{id}.json");
    (
        StatusCode::OK,
        [
            ("content-type", "application/json; charset=utf-8"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

pub async fn prompts_export(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let filters = prompt_filters(&uri);
    let rows = run_blocking(move || state.db.export_prompts(&filters, 100_000)).await;
    let mut body = String::new();
    for row in &rows {
        if let Ok(line) = serde_json::to_string(row) {
            body.push_str(&line);
            body.push('\n');
        }
    }
    let filename = format!(
        "modelgate-prompts-{}.jsonl",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    (
        StatusCode::OK,
        [
            ("content-type", "application/x-ndjson; charset=utf-8"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
            ("x-row-count", &rows.len().to_string()),
        ],
        body,
    )
        .into_response()
}

pub async fn prompts_clear(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let mut filters = crate::db::PromptFilters {
        since_ts: parse_body_ts(body.get("from")),
        until_ts: parse_body_ts(body.get("to")),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        session: body
            .get("session")
            .and_then(Value::as_str)
            .map(str::to_string),
        upstream: None,
        q: None,
    };
    if filters.since_ts.is_none() && filters.until_ts.is_none() {
        let range = body.get("range").and_then(Value::as_str).unwrap_or("all");
        match range {
            "all" => {}
            "today" => filters.since_ts = Some(crate::proxy::today_start()),
            "yesterday" => {
                filters.since_ts = Some(crate::proxy::day_start(1));
                filters.until_ts = Some(crate::proxy::today_start());
            }
            "1h" => filters.since_ts = Some(now() - 3600),
            "24h" => filters.since_ts = Some(now() - 86400),
            "3d" => filters.since_ts = Some(now() - 3 * 86400),
            "7d" => filters.since_ts = Some(now() - 7 * 86400),
            "30d" => filters.since_ts = Some(now() - 30 * 86400),
            _ => filters.since_ts = Some(now() - 86400),
        }
    }
    // 删除 + VACUUM 可能在 GB 级库上耗时数十秒：放阻塞线程池，
    // 避免 long-running VACUUM 占住 tokio worker。
    let db = state.db.clone();
    let result = tokio::task::spawn_blocking(move || {
        let before = std::fs::metadata(&db.path).map(|m| m.len()).unwrap_or(0);
        let removed = db.clear_prompts(&filters);
        let mut vacuumed = 0i64;
        if removed > 0 {
            let after = db.vacuum();
            vacuumed = (before as i64 - after as i64).max(0);
        }
        // 清空/回收后统计已失真：同步重算（本操作已在阻塞线程池，
        // 且用户刚做过重活，理应立刻看到准确数字）。
        db.recompute_prompt_stats();
        (removed, before, vacuumed)
    })
    .await
    .unwrap_or((0, 0, 0));
    let (removed, before, vacuumed) = result;
    Json(json!({
        "ok": true,
        "removed": removed,
        "bytes_before": before,
        "bytes_reclaimed": vacuumed,
    }))
    .into_response()
}

/// 提示词删除后 SQLite 不会缩小文件；此接口手动回收空闲页，
/// 把 VACUUM 放阻塞线程池执行并回报前后体积。
pub async fn prompts_vacuum(State(state): State<Arc<AppState>>) -> Response {
    let db = state.db.clone();
    let (before, after) = tokio::task::spawn_blocking(move || {
        let before = std::fs::metadata(&db.path).map(|m| m.len()).unwrap_or(0);
        let after = db.vacuum();
        db.recompute_prompt_stats();
        (before, after)
    })
    .await
    .unwrap_or((0, 0));
    Json(json!({
        "ok": true,
        "bytes_before": before,
        "bytes_after": after,
        "bytes_reclaimed": (before as i64 - after as i64).max(0),
    }))
    .into_response()
}

fn role_filtered(messages: &[Value], role: Option<&str>) -> Vec<Value> {
    messages
        .iter()
        .filter(|m| {
            role.filter(|r| !r.is_empty())
                .and_then(|r| m.get("role").and_then(Value::as_str).map(|v| v == r))
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

fn prompt_timeline_row(state: &AppState, id: &str) -> Option<Value> {
    let value = state.db.get_prompt(id)?;
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut row = value.clone();
    row.as_object_mut()?.remove("messages");
    row["summary"] = crate::prompt::summarize_messages(&messages);
    row["call_chain"] = crate::prompt::summarize_call_chain(&messages);
    row["first_user"] = crate::prompt::content_text(
        messages
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            .unwrap_or(&Value::Null),
    )
    .chars()
    .take(200)
    .collect::<String>()
    .into();
    Some(row)
}

pub async fn prompts_evolution(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let Some(session) = query.get("session").filter(|v| !v.is_empty()).cloned() else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 session 参数",
            "invalid_request_error",
            "session_required",
        );
    };
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(20)
        .clamp(2, 50);
    let filters = crate::db::PromptFilters {
        session: Some(session.clone()),
        ..Default::default()
    };
    // N+1：时间线每一行都要完整载入 messages 做摘要，GB 级库上整体放阻塞线程池。
    let timeline = run_blocking(move || {
        let mut rows = state.db.list_prompts(&filters, limit, 0, "ts", "desc");
        rows.reverse();
        rows.iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .filter_map(|id| prompt_timeline_row(&state, id))
            .collect::<Vec<Value>>()
    })
    .await;
    Json(json!({"session": session, "rows": timeline})).into_response()
}

pub async fn prompts_diff(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let (Some(base_id), Some(target_id)) =
        (query.get("base").cloned(), query.get("target").cloned())
    else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 base/target 参数",
            "invalid_request_error",
            "diff_params_required",
        );
    };
    let role = query.get("role").cloned();
    let context = query
        .get("context")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3)
        .min(20);
    // 两条全文记录载入内存做 diff，放阻塞线程池。
    let payload = run_blocking(move || {
        let base = state.db.get_prompt(&base_id);
        let target = state.db.get_prompt(&target_id);
        (base, target)
    })
    .await;
    let (Some(base), Some(target)) = payload else {
        let (base_missing, target_missing) = match &payload {
            (None, _) => (true, false),
            (_, None) => (false, true),
            _ => (false, false),
        };
        return error_json(
            StatusCode::NOT_FOUND,
            if base_missing {
                "基准提示词记录不存在"
            } else {
                "目标提示词记录不存在"
            },
            "invalid_request_error",
            "prompt_not_found",
        );
    };
    let base_messages = role_filtered(
        base.get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        role.as_deref(),
    );
    let target_messages = role_filtered(
        target
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        role.as_deref(),
    );
    let diff = crate::prompt::diff_messages(&base_messages, &target_messages, context);
    Json(json!({
        "base": {"id": base.get("id"), "ts": base.get("ts"), "session": base.get("session"),
                 "requested_model": base.get("requested_model")},
        "target": {"id": target.get("id"), "ts": target.get("ts"), "session": target.get("session"),
                   "requested_model": target.get("requested_model")},
        "diff": diff,
    }))
    .into_response()
}

pub async fn prompts_diff_download(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let (Some(base_id), Some(target_id)) = (query.get("base"), query.get("target")) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 base/target 参数",
            "invalid_request_error",
            "diff_params_required",
        );
    };
    let role = query.get("role").cloned();
    let context = query
        .get("context")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3)
        .min(20);
    let base_id = base_id.to_string();
    let target_id = target_id.to_string();
    let payload = run_blocking(move || {
        let base = state.db.get_prompt(&base_id);
        let target = state.db.get_prompt(&target_id);
        (base, target, base_id, target_id)
    })
    .await;
    let (base, target, base_id, target_id) = payload;
    let (Some(base), Some(target)) = (base, target) else {
        return error_json(
            StatusCode::NOT_FOUND,
            "提示词记录不存在",
            "invalid_request_error",
            "prompt_not_found",
        );
    };
    let base_messages = role_filtered(
        base.get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        role.as_deref(),
    );
    let target_messages = role_filtered(
        target
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        role.as_deref(),
    );
    let diff = crate::prompt::diff_messages(&base_messages, &target_messages, context);
    let body = crate::prompt::unified_diff(&base_id, &target_id, &diff);
    let filename = format!("modelgate-prompt-diff-{base_id}-{target_id}.diff");
    (
        StatusCode::OK,
        [
            ("content-type", "text/plain; charset=utf-8"),
            (
                "content-disposition",
                &format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

pub async fn usage_sessions(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let query = params(&uri);
    let filters = filters_from_query(uri.query().unwrap_or(""), now());
    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let sort = query
        .get("sort")
        .cloned()
        .unwrap_or_else(|| "last_ts".into());
    let order = query.get("order").cloned().unwrap_or_else(|| "desc".into());
    let rows = run_blocking(move || state.db.session_usage(&filters, limit, &sort, &order)).await;
    Json(json!({"rows": rows, "limit": limit})).into_response()
}

fn escape_csv(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// 日志尾读窗口：只读文件末尾 512 KiB。
///
/// 高频自动刷新时整文件读取在长日志下会反复搬运几十 MB；
/// 面板只展示最近若干行，因此按窗口从文件尾部读即可。
const LOG_TAIL_BYTES: u64 = 512 * 1024;

pub async fn logs_get(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    use std::io::{Read, Seek, SeekFrom};

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
                           "total": 0, "size": 0, "tail": false,
                           "note": "日志文件尚未生成"}))
        .into_response();
    }
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let start_at = file_size.saturating_sub(LOG_TAIL_BYTES);
    let text = (|| -> std::io::Result<String> {
        let mut file = std::fs::File::open(&path)?;
        if start_at > 0 {
            file.seek(SeekFrom::Start(start_at))?;
        }
        let mut buffer = Vec::with_capacity((file_size - start_at) as usize + 1);
        file.read_to_end(&mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer).to_string())
    })()
    .unwrap_or_default();
    let mut all: Vec<&str> = text.lines().collect();
    // 从中间截断时首行通常是半截，直接丢弃，避免展示残缺行。
    if start_at > 0 && !all.is_empty() {
        all.remove(0);
    }
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
        "size": file_size,
        "total": total,
        "matched": matched_count,
        "tail": start_at > 0,
        "windowBytes": LOG_TAIL_BYTES,
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
    if !pricing.is_null() && !pricing.is_object() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "pricing 必须是 JSON 对象",
            "invalid_request_error",
            "invalid_pricing",
        );
    }
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
    let store = crate::secret_store::SecretStore::load(&state.config_path);
    let mut new_cfg = crate::config::Config::from_value_with_store(merged, &store);
    new_cfg.raw = to_write;
    // 走 replace_config：它会把运行态的端口/主机回填到 raw，
    // 避免端口降级场景把文件里的旧端口写回（与 /api/config 同一口径）。
    state.replace_config(new_cfg);
    // 「保存后重算历史费用」：按新单价表把 usage_records.cost 全部重刷一遍。
    // 之前该参数只是 UI 空壳，后端直接忽略。
    let recomputed = if body
        .get("recompute")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let table = state.cfg().pricing.clone();
        state.db.recompute_costs(|row| {
            table.cost_for(
                &row.upstream,
                &row.requested_model,
                &row.upstream_model,
                row.ts,
                row.prompt_tokens,
                row.completion_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
            )
        })
    } else {
        0
    };
    let table = &state.cfg().pricing;
    Json(json!({"ok": true, "pricing": pricing, "currency": "CNY",
    "configured": table.configured(),
    "entries": table.entries.len(),
    "diagnostics": table.issues,
    "recomputed": recomputed,
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
    let (upstreams, aliases, default_model, pricing) = {
        let cfg = state.cfg();
        (
            cfg.upstreams.clone(),
            cfg.aliases.clone(),
            cfg.default_model.clone(),
            cfg.pricing.clone(),
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
    // 单价提示：按「每个上游 + 模型名」分别解析单价表。
    // 同名模型在不同上游可能完全不同的价格，因此不能只取第一个上游。
    for row in rows.iter_mut() {
        let model = row["model"].as_str().unwrap_or("").to_string();
        let upstreams: Vec<String> = row["upstreams"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut hints: Vec<Value> = Vec::new();
        for upstream in &upstreams {
            if let Some(quote) = pricing.quote(upstream, &model) {
                hints.push(json!({"upstream": upstream, "quote": quote}));
            }
        }
        // 兜底显示：没有任何上游命中时，再看「任意上游」条目的报价。
        if hints.is_empty() {
            if let Some(quote) = pricing.quote("", &model) {
                hints.push(json!({"upstream": "", "quote": quote}));
            }
        }
        row["costHints"] = json!(hints);
        row["costHint"] = hints
            .first()
            .map(|hint| hint["quote"].clone())
            .unwrap_or(Value::Null);
    }
    // 「单价」胶囊筛选：已配置 = 命中任何单价条目（按量或额度制）
    let price_filter = query.get("price").cloned().unwrap_or_default();
    if price_filter == "priced" {
        rows.retain(|r| !r["costHint"].is_null());
    } else if price_filter == "missing" {
        rows.retain(|r| r["costHint"].is_null());
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
        "fx": { "USD": pricing.fx_usd },
        "pricing_configured": pricing.configured(),
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
            // 目录来源：CommandCode 没有 HTTP 端点，目录来自本机 CLI 包内 models.md。
            let local_catalog = upstream.compat.catalog_source == "commandcode";
            let (source, path, hint) = if local_catalog {
                let hint = if models.is_empty() {
                    "未找到 CommandCode 本地模型目录（需安装 command-code CLI，或手动填写 models）"
                        .to_string()
                } else {
                    String::new()
                };
                ("local-catalog", "models.md".to_string(), hint)
            } else {
                (
                    "endpoint",
                    upstream.compat.models_path.clone(),
                    String::new(),
                )
            };
            Json(json!({
                "ok": true,
                "upstream": name,
                "models": models,
                "count": models.len(),
                "source": source,
                "path": path,
                "declared": declared,
                "baseUrl": upstream.base_url,
                "usedOverride": used_override,
                "elapsedMs": elapsed_ms,
                "hint": hint,
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

    /// 逐模型密钥写入的拒绝路径：未知模型、非 env 来源、file 目标。
    #[test]
    fn upstream_secret_model_key_rejects_invalid_targets() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": "https://example.invalid/v1",
                    "apiKeyEnv": "UPSTREAM_FALLBACK_ENV",
                    "modelKeys": {
                        "model-a": "MODEL_A_ENV",
                        "model-b": {"apiKeyFile": "D:/keys/b.json"}
                    }
                }
            }
        }));
        let dir = temp_dir("secret-model");
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        let (status, body) = runtime.block_on(async {
            body_json(
                upstream_secret(
                    State(state.clone()),
                    Json(json!({
                        "name": "u", "apiKey": "k-test",
                        "target": "env", "model": "missing"
                    })),
                )
                .await,
            )
            .await
        });
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("unknown_model_key"));

        let (status, body) = runtime.block_on(async {
            body_json(
                upstream_secret(
                    State(state.clone()),
                    Json(json!({
                        "name": "u", "apiKey": "k-test",
                        "target": "env", "model": "model-b"
                    })),
                )
                .await,
            )
            .await
        });
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("model_key_missing_env"));

        let (status, body) = runtime.block_on(async {
            body_json(
                upstream_secret(
                    State(state.clone()),
                    Json(json!({
                        "name": "u", "apiKey": "k-test",
                        "target": "file", "model": "model-a",
                        "filePath": "D:/keys/a.json"
                    })),
                )
                .await,
            )
            .await
        });
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("unsupported_model_key_target"));
    }

    fn uri_with(path: &str, query: &str) -> Uri {
        if query.is_empty() {
            path.parse().unwrap()
        } else {
            format!("{path}?{query}").parse().unwrap()
        }
    }

    /// 构造一个 `catalog.source = commandcode` 的上游。
    ///
    /// `baseUrl` 指向不可达地址：这类上游**不该**发 HTTP 请求，
    /// 断言 200 即证明走的是本地目录分支。
    fn state_with_commandcode(catalog_plan: Value) -> Arc<AppState> {
        let cfg = Config::from_value(json!({
            "modelsCacheSeconds": 300,
            "upstreams": {
                "cc": {
                    "protocol": "commandcode",
                    "baseUrl": "http://127.0.0.1:1",
                    "apiKey": "k",
                    "models": ["m"],
                    "fetchModels": true,
                    "autoModels": false,
                    "compat": {
                        "catalog": { "source": "commandcode", "path": "/v1/models", "plan": catalog_plan }
                    }
                }
            }
        }));
        let dir = temp_dir("cc-state");
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        Arc::new(AppState::new(cfg, db, dir.join("c.json")))
    }

    #[tokio::test]
    async fn commandcode_reads_local_catalog_instead_of_http_endpoint() {
        // 造一个 models.md，写明模型 id 与 Min plan。
        let dir = temp_dir("cc-catalog");
        let models_md = dir.join("models.md");
        std::fs::write(
            &models_md,
            "| Model | Name | Ctx | Think | Price | Min plan | Scope |\n\
             | --- | --- | --- | --- | --- | --- | --- |\n\
             | `deepseek/deepseek-v4-pro` | DeepSeek | 128K | yes | 3 | GOAT | all |\n\
             | `moonshotai/Kimi-K3` | Kimi | 256K | no | 2 | Max | all |\n",
        )
        .unwrap();

        let state = state_with_commandcode(json!(models_md.to_str().unwrap()));
        let resp = upstream_models(
            State(state),
            Path("cc".to_string()),
            uri_with("/api/upstreams/cc/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;

        assert_eq!(status, StatusCode::OK, "本地目录分支不应返回 404");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["source"], json!("local-catalog"));
        assert_eq!(
            body["models"],
            json!(["deepseek/deepseek-v4-pro", "moonshotai/Kimi-K3"])
        );
        assert_eq!(body["count"], json!(2));
    }

    #[tokio::test]
    async fn commandcode_without_local_catalog_returns_empty_with_hint() {
        // 本机没装 CLI → 200 + 空清单 + 明确提示，**不是 404**
        // （404 会把用户引向「改 compat.endpoint.models 路径」的错误方向）。
        let state = state_with_commandcode(json!("definitely/not/here/models.md"));
        let resp = upstream_models(
            State(state),
            Path("cc".to_string()),
            uri_with("/api/upstreams/cc/models", ""),
        )
        .await;
        let (status, body) = body_json(resp).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["models"], json!([]));
        assert_eq!(body["count"], json!(0));
        let hint = body["hint"].as_str().unwrap_or("");
        assert!(
            hint.contains("CommandCode") && hint.contains("目录"),
            "空清单必须给出「未找到本地目录」的可读提示，实际：{hint}"
        );
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
        rows.push(probe(&state, up, timeout, "").await);
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
    // 可选 model：用该模型的逐模型密钥探测；缺省用上游级密钥。
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let up = {
        let cfg = state.cfg();
        cfg.upstream(name).cloned()
    };
    match up {
        Some(up) => Json(json!({"row": probe(&state, &up, timeout, &model).await})).into_response(),
        None => error_json(
            StatusCode::NOT_FOUND,
            format!("未知上游 {name}"),
            "invalid_request_error",
            "unknown_upstream",
        ),
    }
}

async fn probe(state: &AppState, up: &crate::config::Upstream, timeout: f64, model: &str) -> Value {
    let url = format!("{}/v1/models", up.base_url.trim_end_matches('/'));
    // 与 chat 共用鉴权头构造：x-api-key / bearer / both / header 都按上游 compat 走。
    // 指定 model 时用该模型的密钥，否则用上游级密钥。
    let selection = if model.is_empty() {
        up.upstream_key()
    } else {
        up.key_for_model(model)
    };
    let mut headers = reqwest::header::HeaderMap::new();
    crate::proxy::apply_auth_headers(&mut headers, up, &selection.key);
    let req = state
        .client
        .get(&url)
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .headers(headers);
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            json!({
                "name": up.name,
                "ok": resp.status().is_success(),
                "status": status,
                "model": if model.is_empty() { Value::Null } else { json!(model) },
                "keySource": selection.source,
                "keyFrom": selection.from,
                "modelKeys": up.model_keys.iter().map(|mk| json!({
                    "model": mk.pattern,
                    "env": mk.env_name,
                    "configured": mk.missing.is_none(),
                })).collect::<Vec<_>>(),
                "message": if resp.status().is_success() { "模型列表可访问".to_string() }
                           else { format!("HTTP {status}") },
                "hint": if resp.status().is_success() { "" }
                        else { "检查 apiKeyEnv / 凭据文件路径 / baseUrl / 网络；CommandCode 无 /v1/models，属正常" },
            })
        }
        Err(err) => json!({
            "name": up.name,
            "ok": false,
            "status": 0,
            "model": if model.is_empty() { Value::Null } else { json!(model) },
            "keySource": selection.source,
            "keyFrom": selection.from,
            "message": err.to_string(),
            "hint": "检查 baseUrl 是否可达、端口是否监听、本机代理是否拦截",
        }),
    }
}

/// 写入上游密钥：Windows 用户级环境变量（`HKCU\Environment`）、用户指定文件，
/// 或 Model-Gate 本机加密密钥库（DPAPI）。
///
/// 入参：
/// - `name` / `apiKey`：必填。
/// - `target`：`env`（默认，写用户变量）、`file`（写用户手填的凭据文件）或
///   `store`（用 Windows DPAPI 加密保存到 Model-Gate 密钥库）。
/// - `envName`：变量名，缺省时用该上游配置的 `apiKeyEnv`。
/// - `filePath`：文件路径，缺省时用该上游配置的 `apiKeyFile` / `apiKeyAgentPath`。
/// - `fileFormat`：`json`（默认）或 `env`。
/// - `field`：JSON 点路径（缺省 `apiKey`）或 `.env` 变量名。
/// - `model`：逐模型密钥的模型 id；缺省表示上游级默认密钥。
///
/// 约定：
/// - 写入成功 → 更新运行态并热重载，响应只回变量名/路径/密钥库引用与来源，**不回显明文**。
/// - 写入失败 → 返回可诊断错误，**不**静默降级为「只进内存」。
/// - `target=store` 时会把 `apiKeyStore` 引用写入 `bridge.config.json`；密钥本体只
///   存在于独立的 DPAPI 密钥库文件。
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
    if let Err(err) = crate::key_file::validate_key(key) {
        return error_json(
            StatusCode::BAD_REQUEST,
            err.to_string(),
            "invalid_request_error",
            "invalid_key",
        );
    }
    let target = body
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("env")
        .to_ascii_lowercase();
    if target != "env" && target != "file" && target != "store" {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!(
                "未知写入目标 {target}：只支持 env（用户变量）/ file（指定文件）/ store（本机加密密钥库）"
            ),
            "invalid_request_error",
            "invalid_target",
        );
    }

    let cfg: crate::config::Config = (**state.cfg()).clone();
    let upstream = match cfg.upstreams.iter().find(|u| u.name == name) {
        Some(up) => up.clone(),
        None => {
            return error_json(
                StatusCode::NOT_FOUND,
                format!("未知上游 {name}"),
                "invalid_request_error",
                "unknown_upstream",
            )
        }
    };
    // 原始配置条目：agent 路径与 provider 只存在于 raw 里。
    let raw_up = cfg
        .raw
        .get("upstreams")
        .and_then(Value::as_object)
        .and_then(|m| m.get(name))
        .cloned()
        .unwrap_or(Value::Null);

    // 逐模型密钥写入：v1 只支持环境变量目标（凭据文件请直接编辑）。
    let has_model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if target == "file" && has_model {
        return error_json(
            StatusCode::BAD_REQUEST,
            "逐模型密钥暂只支持写入环境变量（target=env）；凭据文件请直接编辑对应文件",
            "invalid_request_error",
            "unsupported_model_key_target",
        );
    }
    if target == "store" {
        return write_secret_to_store(&state, cfg, &upstream, &body, key);
    }
    if target == "file" {
        return write_secret_to_file(&state, cfg, &upstream, &raw_up, &body, key);
    }
    write_secret_to_user_env(&state, cfg, &upstream, &body, key)
}

/// 加密保存到 Model-Gate 本机密钥库，并把 `apiKeyStore` 引用写入配置。
///
/// 与 env/file 不同：这里必须同时落配置引用，否则重启后无法判断该用哪把密钥。
/// 写入顺序是「备份配置 → 写密钥库 → 写配置」；配置写失败时回滚新密钥，避免
/// 留下无法被引用的密文。
fn write_secret_to_store(
    state: &AppState,
    cfg: crate::config::Config,
    upstream: &crate::config::Upstream,
    body: &Value,
    key: &str,
) -> Response {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(model) = model.as_deref() {
        if model.matches('*').count() > 1 {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!("模型键「{model}」包含多个 *，只支持一个通配符"),
                "invalid_request_error",
                "invalid_model_key",
            );
        }
    }
    let store_id = match model.as_deref() {
        Some(model) => crate::secret_store::model_id(&upstream.name, model),
        None => crate::secret_store::upstream_id(&upstream.name),
    };
    let scope = if model.is_some() { "model" } else { "upstream" };

    // 只改密钥来源相关字段：保留上游的其它配置。
    let mut raw = cfg.raw.clone();
    let Some(raw_up) = raw
        .get_mut("upstreams")
        .and_then(Value::as_object_mut)
        .and_then(|ups| ups.get_mut(&upstream.name))
        .and_then(Value::as_object_mut)
    else {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置里找不到上游「{}」", upstream.name),
            "server_error",
            "upstream_not_in_config",
        );
    };
    if let Some(model) = model.as_deref() {
        // 逐模型条目：对象写法；原来的字符串/文件/环境变量来源被替换为密钥库引用。
        let model_keys = raw_up.entry("modelKeys").or_insert_with(|| json!({}));
        if !model_keys.is_object() {
            *model_keys = json!({});
        }
        let entry = model_keys
            .as_object_mut()
            .expect("modelKeys 已归一为对象")
            .entry(model.to_string())
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        let entry = entry.as_object_mut().expect("逐模型条目已归一为对象");
        for field in [
            "apiKey",
            "apiKeyEnc",
            "apiKeyEnv",
            "apiKeyFile",
            "apiKeyAgent",
            "apiKeyAgentPath",
            "apiKeyAgentProvider",
        ] {
            entry.remove(field);
        }
        entry.insert("apiKeyStore".into(), json!(store_id.clone()));
    } else {
        // 上游级默认密钥：一个上游的多个模型共用；逐模型条目仍可覆盖。
        for field in [
            "apiKey",
            "apiKeyEnc",
            "apiKeyEnv",
            "apiKeyFile",
            "apiKeyAgent",
            "apiKeyAgentPath",
            "apiKeyAgentProvider",
        ] {
            raw_up.remove(field);
        }
        raw_up.insert("apiKeyStore".into(), json!(store_id.clone()));
    }
    let mut to_write = raw;
    crate::config_store::strip_secrets(&mut to_write);

    let backup = match crate::config_store::backup_config(
        &state.config_path,
        crate::config_store::BackupPolicy::Enabled,
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("保存密钥前备份配置失败：{err}"),
                "server_error",
                "config_backup_failed",
            )
        }
    };

    let mut store = crate::secret_store::SecretStore::load(&state.config_path);
    let existed = store.contains(&store_id);
    if let Err(err) = store.set(&store_id, key) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("加密保存密钥失败：{err}"),
            "server_error",
            "store_write_failed",
        );
    }
    if let Err(err) = crate::config_store::save_atomic(&state.config_path, &to_write) {
        if !existed {
            let _ = store.remove(&store_id);
        }
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("密钥已加密写入，但配置引用保存失败：{err}"),
            "server_error",
            "config_write_failed",
        );
    }

    // 用刚写入的内存密钥库解析，避免再读一次文件；随后热替换运行态。
    let new_cfg = crate::config::Config::from_value_with_store(to_write, &store);
    state.replace_config(new_cfg);
    Json(json!({
        "ok": true,
        "name": upstream.name,
        "target": "store",
        "scope": scope,
        "model": model,
        "keyId": store_id,
        "keySource": "store",
        "encrypted": true,
        "backup": {
            "created": backup.created,
            "forcedFirst": backup.forced_first,
            "path": backup.path.as_ref().map(|p| p.display().to_string()),
        },
        "note": "已用 Windows DPAPI 加密保存到本机密钥库，并把 apiKeyStore 引用写入配置；密钥本体不会出现在 bridge.config.json 或接口响应中",
    }))
    .into_response()
}

/// 写入 Windows 用户级环境变量（`HKCU\Environment`）。
fn write_secret_to_user_env(
    state: &AppState,
    mut cfg: crate::config::Config,
    upstream: &crate::config::Upstream,
    body: &Value,
    key: &str,
) -> Response {
    // 逐模型写入：由 modelKeys 条目决定变量名；未指定 model 时沿用上游级逻辑。
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let env_name = if let Some(model) = model.as_deref() {
        let Some(entry) = upstream.model_keys.iter().find(|mk| mk.pattern == model) else {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!(
                    "模型「{model}」不在上游「{}」的 modelKeys 中",
                    upstream.name
                ),
                "invalid_request_error",
                "unknown_model_key",
            );
        };
        let Some(env) = entry
            .env_name
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!(
                    "模型「{model}」没有配置 apiKeyEnv，无法写入环境变量；请先把该条目改为字符串或 {{\"apiKeyEnv\":\"...\"}}"
                ),
                "invalid_request_error",
                "model_key_missing_env",
            );
        };
        env
    } else {
        body.get("envName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                upstream
                    .api_key_env
                    .clone()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default()
    };
    if env_name.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少密钥环境变量名：请填写「密钥环境变量」或指定 envName",
            "invalid_request_error",
            "missing_env_name",
        );
    }
    if !is_valid_env_name(&env_name) {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("环境变量名不合法：{env_name}"),
            "invalid_request_error",
            "invalid_env_name",
        );
    }

    // 真正写入用户级环境变量。
    if let Err(err) = crate::user_env::write_user_var(&env_name, key) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入用户变量 {env_name} 失败：{err}"),
            "internal_error",
            "write_user_var_failed",
        );
    }
    // Windows 环境块是启动时快照：当前进程要靠会话缓存才能立刻读到新值。
    crate::user_env::remember_session_var(&env_name, key);
    crate::user_env::notify_environment_changed();

    // 热更新运行态：让本次会话立即按新密钥工作。
    for up in cfg.upstreams.iter_mut() {
        if up.name != upstream.name {
            continue;
        }
        // 同一变量可能被多个模型共用：全部更新，避免只改中一个。
        for mk in up.model_keys.iter_mut() {
            if mk.env_name.as_deref() == Some(env_name.as_str()) {
                mk.api_key = key.to_string();
                mk.source = "user-env".into();
                mk.missing = None;
            }
        }
        // 只有「写入的正是上游级变量」或「未指定 model」时才动上游级密钥；
        // 否则会把某厂商的 key 误当成上游兜底 key。
        if model.is_none() || up.api_key_env.as_deref() == Some(env_name.as_str()) {
            up.api_key = key.to_string();
            up.key_source = "user-env".into();
        }
    }
    state.cfg.store(Arc::new(cfg));
    Json(json!({
        "ok": true,
        "name": upstream.name,
        "target": "env",
        "model": model,
        "envName": env_name,
        "scope": "user",
        "keySource": "user-env",
        "note": "已写入 Windows 用户环境变量（HKCU\\Environment）；新进程自动继承",
    }))
    .into_response()
}

/// 写入**用户手动指定**的凭据文件（JSON / `.env`）。
///
/// 路径没有内置默认值：入参 `filePath` 优先，缺省时回退该上游已配置的
/// `apiKeyFile` / `apiKeyAgentPath`（都是用户自己填的路径）。
fn write_secret_to_file(
    state: &AppState,
    mut cfg: crate::config::Config,
    upstream: &crate::config::Upstream,
    raw_up: &Value,
    body: &Value,
    key: &str,
) -> Response {
    let file_path_raw = body
        .get("filePath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            upstream
                .api_key_file
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            raw_up
                .get("apiKeyAgentPath")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if file_path_raw.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少凭据文件路径：请填写「凭据文件路径」或先在配置里指定 apiKeyFile",
            "invalid_request_error",
            "missing_file_path",
        );
    }
    let format_raw = body
        .get("fileFormat")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("json")
        .to_string();
    let format = match crate::key_file::KeyFileFormat::parse(&format_raw) {
        Some(f) => f,
        None => {
            return error_json(
                StatusCode::BAD_REQUEST,
                format!("未知凭据文件格式 {format_raw}：只支持 json / env"),
                "invalid_request_error",
                "invalid_file_format",
            )
        }
    };
    // `.env` 必须有变量名：沿用上游已配置的 provider（`apiKeyAgentProvider`）。
    // JSON 缺省写根字段 `apiKey`。
    let field = body
        .get("field")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| match format {
            crate::key_file::KeyFileFormat::Env => raw_up
                .get("apiKeyAgentProvider")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            crate::key_file::KeyFileFormat::Json => None,
        })
        .unwrap_or_default();

    let path = crate::config::expand_home(&file_path_raw);
    if !path.is_absolute() {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("凭据文件路径必须是绝对路径：{file_path_raw}"),
            "invalid_request_error",
            "invalid_file_path",
        );
    }
    // 禁止把密钥写进 Model-Gate 自己的配置文件。
    let protected = vec![state.config_path.clone()];
    let written = match crate::key_file::write_key_file(&path, format, &field, key, &protected) {
        Ok(w) => w,
        Err(err) => {
            let status = match err {
                crate::key_file::KeyFileError::Invalid(_) => StatusCode::BAD_REQUEST,
                crate::key_file::KeyFileError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return error_json(
                status,
                err.to_string(),
                "invalid_request_error",
                "write_key_file_failed",
            );
        }
    };
    // 回读校验：用与运行时相同的读取语义确认密钥真的落到了文件里。
    if !crate::key_file::verify_key_file(&written.path, &written.field) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "写入 {} 后回读校验失败，请检查文件格式与字段名",
                written.path.display()
            ),
            "internal_error",
            "verify_key_file_failed",
        );
    }

    let label = written
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    // 与配置里保存的引用保持一致：根字段 `apiKey` 走 `apiKeyFile`，其余走 agent 自定义路径。
    let key_source = match format {
        crate::key_file::KeyFileFormat::Json if written.field == "apiKey" => "file".to_string(),
        crate::key_file::KeyFileFormat::Json => format!("agent:file/{label}"),
        crate::key_file::KeyFileFormat::Env => format!("agent:env-file/{label}"),
    };
    for up in cfg.upstreams.iter_mut() {
        if up.name == upstream.name {
            up.api_key = key.to_string();
            up.key_source = key_source.clone();
        }
    }
    state.cfg.store(Arc::new(cfg));

    // 环境变量优先级高于文件：已配置 apiKeyEnv 时给出明确提示。
    let warning = match upstream.api_key_env.as_ref().map(|s| s.trim()) {
        Some(env_name) if !env_name.is_empty() => {
            format!("该上游配置了 apiKeyEnv={env_name}：环境变量优先于文件，文件里的密钥暂不生效")
        }
        _ => String::new(),
    };
    Json(json!({
        "ok": true,
        "name": upstream.name,
        "target": "file",
        "scope": "file",
        "filePath": written.path.display().to_string(),
        "fileFormat": written.format,
        "field": written.field,
        "created": written.created,
        "keySource": key_source,
        "warning": warning,
        "note": "已写入用户指定文件；请点「保存配置」把路径引用写入配置文件（配置文件里只存路径，不存密钥）",
    }))
    .into_response()
}

/// Windows 环境变量名合法性：不能含 `=`，不能为空，不能以 `=` 开头。
fn is_valid_env_name(name: &str) -> bool {
    crate::key_file::is_valid_env_name(name)
}
