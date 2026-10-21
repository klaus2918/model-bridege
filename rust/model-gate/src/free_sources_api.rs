//! 免费来源接口层：来源清单、目录拉取、可用性探测与一键接入。
//!
//! 端点：
//! - `GET  /api/free-sources`         列出免费来源 + 当前配置状态 + 鱼缸水位（零生成）
//! - `POST /api/free-sources/catalog` 拉取并筛选免费模型目录（零生成、零额度消耗）
//! - `POST /api/free-sources/probe`   单模型真实对话探测（消耗额度，需显式触发）
//! - `POST /api/free-sources/probe-all` 批量探测（受退避与并发上限保护）
//! - `POST /api/free-sources/attach`  一键接入（写配置到 bridge.config.json）
//!
//! 设计约束（plan.md D3 / D6）：
//! - **不泄露**：响应体不含上游请求头、指纹头、会话 ID 或任何凭据。
//! - **不消耗（默认）**：目录与水位查询不产生任何生成请求。
//! - **不进全局**：一键接入固定 `autoModels:false`，且不写顶层 `models`。

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::Config;
use crate::free_sources as fs;
use crate::opencode_transport as opencode;
use crate::proxy::{error_json, AppState};

/// 探测超时上限（秒）。
///
/// 为什么比普通上游探测宽：免费池里有**本身就慢**的模型——
/// Kilo 的 `kilo-auto/free` 实测 68.4s（按会话动态路由到候选免费模型），
/// `stealth/glyph-cluster` 26s。用 10s 上限会把它们误判成"网络不可达"。
const PROBE_TIMEOUT_MAX: f64 = 90.0;
const PROBE_TIMEOUT_DEFAULT: f64 = 30.0;
/// 批量探测的并发上限：避免同时打满免密池的 IP 限流额度。
const PROBE_ALL_CONCURRENCY: usize = 3;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 当前配置里是否已接入该来源。
fn upstream_configured<'a>(
    cfg: &'a Config,
    source: &fs::FreeSource,
) -> Option<&'a crate::config::Upstream> {
    cfg.upstreams.iter().find(|u| u.name == source.upstream)
}

/// 鱼缸水位：可用模型数 / 候选模型总数。
///
/// 「可用」在此只统计**已探测确认可用**的模型；未探测过的候选不计入分母以外，
/// 也绝不计入分子——宁可显示"尚未探测"，也不显示虚高的可用数。
fn level_of(candidates: usize, available: usize) -> Value {
    json!({
        "candidates": candidates,
        "available": available,
        "ratio": if candidates > 0 {
            (available as f64 / candidates as f64 * 100.0 * 10.0).round() / 10.0
        } else {
            0.0
        },
    })
}

/// `GET /api/free-sources`：来源清单 + 接入状态 + 隐私提示（零生成）。
pub async fn free_sources(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.cfg();
    let mut rows = Vec::new();
    for source in fs::all_sources() {
        let upstream = upstream_configured(&cfg, &source);
        let configured = upstream.is_some();
        let declared_models = upstream.map(|u| u.models.len()).unwrap_or(0);
        rows.push(json!({
            "id": source.id,
            "label": source.label,
            "upstream": source.upstream,
            "baseUrl": source.base_url,
            "protocol": source.protocol,
            "transport": source.transport,
            "auth": source.auth_scheme(),
            "configured": configured,
            "declaredModels": declared_models,
            "transportActive": upstream.map(|u| u.transport == source.transport).unwrap_or(false),
            "privacy": {
                "level": source.privacy_level,
                "note": source.privacy_note,
            },
            "docsUrl": source.docs_url,
            "probe": {
                "minMaxTokens": source.min_max_tokens,
            },
        }));
    }
    Json(json!({
        "sources": rows,
        "states": fs::states(),
        "rejectReasons": fs::reject_reason_codes(),
        "checkedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

/// 拉取上游模型目录并筛选免费候选。
///
/// Zen 免密层的 `/models` 同样需要客户端指纹与匿名 key（实测：裸请求拿不到可用目录），
/// 因此走 `transport=opencode` 的来源必须带上同样的头，不能只对 chat 请求带。
async fn fetch_catalog(source: &fs::FreeSource) -> Result<Vec<(String, Value)>, String> {
    let raw_url = format!("{}{}", source.normalized_base_url(), source.catalog_path);
    // 与 chat 路径保持一致：`transport=opencode` 的来源要把 `/zen/...` 改写成
    // `/inference/...`（OpenCode 免密层自 2026-10-14 起迁到新端点；
    // 仍打旧端点会拿到 404）。
    let url = if source.transport == "opencode" {
        opencode::rewrite_free_url(&raw_url)
    } else {
        raw_url
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client.get(&url);
    if source.transport == "opencode" {
        let mut headers = reqwest::header::HeaderMap::new();
        let session = opencode::session_for(&opencode::session_seed(&json!({
            "model": "",
            "messages": [],
        })));
        opencode::apply_headers(&mut headers, &session);
        request = request.headers(headers);
    }
    let response = request.send().await.map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let data: Value = response.json().await.map_err(|err| err.to_string())?;
    let items: Vec<Value> = data
        .get("data")
        .or_else(|| data.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(fs::filter_free_models(source, &items))
}

/// 模型卡基础信息（目录条目 → 展示用字段）。
fn model_card(source: &fs::FreeSource, id: &str, entry: &Value) -> Value {
    let context = entry
        .get("context_length")
        .or_else(|| {
            entry
                .get("top_provider")
                .and_then(|p| p.get("context_length"))
        })
        .and_then(Value::as_u64);
    let max_output = entry
        .get("top_provider")
        .and_then(|p| p.get("max_completion_tokens"))
        .and_then(Value::as_u64);
    let modalities = entry
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .or_else(|| {
            entry
                .get("architecture")
                .and_then(|a| a.get("input_modalities"))
        })
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let efforts: Vec<String> = entry
        .get("opencode")
        .and_then(|o| o.get("reasoning_options"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    opt.get("values").and_then(Value::as_array).map(|vals| {
                        vals.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": id,
        "source": source.id,
        "name": entry.get("name").and_then(Value::as_str).unwrap_or(id),
        "contextLength": context,
        "maxOutputTokens": max_output,
        "inputModalities": modalities,
        "reasoningEfforts": efforts,
        // mayTrainOnYourPrompts：Kilo 目录里的隐私标记，true 表示上游可能用提示词训练。
        "mayTrainOnYourPrompts": entry
            .get("mayTrainOnYourPrompts")
            .and_then(Value::as_bool),
        "isModerated": entry
            .get("top_provider")
            .and_then(|p| p.get("is_moderated"))
            .and_then(Value::as_bool),
    })
}

/// `POST /api/free-sources/catalog`：拉目录 + 筛免费候选（零生成）。
pub async fn free_sources_catalog(Json(body): Json<Value>) -> Response {
    let id = body.get("source").and_then(Value::as_str).unwrap_or("");
    let Some(source) = fs::source(id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知免费来源 {id}"),
            "invalid_request_error",
            "unknown_free_source",
        );
    };
    match fetch_catalog(&source).await {
        Ok(models) => {
            let cards: Vec<Value> = models
                .iter()
                .map(|(mid, entry)| model_card(&source, mid, entry))
                .collect();
            Json(json!({
                "ok": true,
                "source": source.id,
                "count": cards.len(),
                "models": cards,
                "level": level_of(cards.len(), 0),
                "checkedAt": chrono::Utc::now().to_rfc3339(),
            }))
            .into_response()
        }
        Err(message) => error_json(
            StatusCode::BAD_GATEWAY,
            format!("拉取免费来源目录失败：{message}"),
            "upstream_error",
            "free_source_catalog_failed",
        ),
    }
}

/// 单次探测的真实结果。
struct ProbeResult {
    state: fs::ProbeState,
    code: String,
    status: u16,
    raw: String,
    latency_ms: u64,
    usage: Option<Value>,
}

/// 对一个模型发一次真实对话，判定可用性。
///
/// - Zen 免密层：复用 `transport=opencode` 的客户端指纹、强制流式与 SSE 聚合。
/// - Kilo 免密池：标准 OpenAI 兼容端点，不带任何凭据。
///
/// `max_tokens` 用来源声明的 `minMaxTokens`（默认 512）：NVIDIA 系模型在
/// `max_tokens=64` 时 reasoning 会吃满预算、返回 200 但正文为空，
/// 那不是"不可用"，所以探测必须给足预算。
async fn probe_model(
    state: &AppState,
    source: &fs::FreeSource,
    model: &str,
    timeout: f64,
) -> ProbeResult {
    let started = std::time::Instant::now();
    let is_opencode = source.transport == "opencode";
    // Zen 免密层**强制流式**（transport 契约），必须走 SSE 聚合；
    // Kilo 是标准 OpenAI 兼容端点，用非流式即可拿到
    // `choices[0].message.content`，解析成本更低也不会把 SSE 帧误当 JSON。
    let mut payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": source.probe_prompt}],
        "max_tokens": source.min_max_tokens,
        "stream": is_opencode,
    });
    let mut tool_state = opencode::ToolState::default();
    if is_opencode {
        tool_state = opencode::prepare_tools(&mut payload);
    }
    let url = {
        let raw = format!("{}{}", source.normalized_base_url(), source.chat_path);
        if is_opencode {
            opencode::rewrite_free_url(&raw)
        } else {
            raw
        }
    };

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    if is_opencode {
        // 免密层：固定匿名 key + 客户端指纹（transport 接管，不读配置里的 apiKey）。
        let session = opencode::session_for(&opencode::session_seed(&payload));
        opencode::apply_headers(&mut headers, &session);
    }

    let result = state
        .client
        .post(&url)
        .headers(headers)
        .json(&payload)
        .timeout(Duration::from_secs_f64(timeout))
        .send()
        .await;

    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Err(err) => {
            // 超时与连不上要分开：前者是上游太慢（可加长超时/换模型），
            // 后者是网络不可达（查本机网络与代理）。
            let rejection = if err.is_timeout() {
                fs::classify_timeout()
            } else {
                fs::classify_network_error(&err.to_string())
            };
            ProbeResult {
                state: rejection.state,
                code: rejection.code,
                status: rejection.status,
                raw: rejection.raw,
                latency_ms,
                usage: None,
            }
        }
        Ok(response) => {
            let status = response.status().as_u16();
            let succeeded = response.status().is_success();
            let body = response.text().await.unwrap_or_default();
            if !succeeded {
                let rejection = fs::classify_error(status, &body);
                return ProbeResult {
                    state: rejection.state,
                    code: rejection.code,
                    status: rejection.status,
                    raw: rejection.raw,
                    latency_ms,
                    usage: None,
                };
            }
            // 成功：Zen 走 SSE 聚合，Kilo 是普通 JSON。
            let (content, usage) = if is_opencode {
                let (value, usage) = opencode::aggregate_sse(&body, &tool_state, model);
                let content = value
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                (content, usage)
            } else {
                let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let content = value
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                (content, value.get("usage").cloned())
            };
            if content.trim().is_empty() {
                // 200 但正文为空：疑似 reasoning 吃满预算，判「疑似可用」而非不可用。
                let rejection = fs::classify_empty_body();
                return ProbeResult {
                    state: rejection.state,
                    code: rejection.code,
                    status,
                    raw: rejection.raw,
                    latency_ms,
                    usage,
                };
            }
            ProbeResult {
                state: fs::ProbeState::Available,
                code: String::new(),
                status,
                raw: String::new(),
                latency_ms,
                usage,
            }
        }
    }
}

/// 把探测结果组装成面板用的结构。
///
/// 状态的**单一事实源是拒因字典**：`rejectReasons[code].state`。
/// 分类器（`free_sources::classify_error`）给出的状态只作兜底——当字典里
/// 没有该拒因码时用它。这样"某个拒因该归 transient 还是 unavailable"
/// 只需改数据，不必改代码，也避免两处判定漂移。
fn probe_payload(source: &fs::FreeSource, model: &str, result: &ProbeResult) -> Value {
    let reason = if result.code.is_empty() {
        None
    } else {
        fs::reject_reason(&result.code)
    };
    let state = reason
        .as_ref()
        .map(|r| r.state.clone())
        .unwrap_or_else(|| result.state.as_str().to_string());
    let reason_code = reason
        .as_ref()
        .map(|r| r.code.clone())
        .unwrap_or_else(|| result.code.clone());
    json!({
        "source": source.id,
        "model": model,
        "state": state,
        "reasonCode": if reason_code.is_empty() { Value::Null } else { json!(reason_code) },
        "reasonLabel": reason.as_ref().map(|r| r.label.clone()).unwrap_or_default(),
        "advice": reason.as_ref().map(|r| r.advice.clone()).unwrap_or_default(),
        "status": result.status,
        "raw": result.raw,
        "latencyMs": result.latency_ms,
        "probedAt": chrono::Utc::now().to_rfc3339(),
        "usage": result.usage.clone().unwrap_or(Value::Null),
    })
}

/// `POST /api/free-sources/probe`：单模型真实对话探测。
pub async fn free_sources_probe(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("source").and_then(Value::as_str).unwrap_or("");
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let timeout = body
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(PROBE_TIMEOUT_DEFAULT)
        .clamp(2.0, PROBE_TIMEOUT_MAX);
    let Some(source) = fs::source(id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知免费来源 {id}"),
            "invalid_request_error",
            "unknown_free_source",
        );
    };
    if model.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 model 字段",
            "invalid_request_error",
            "model_required",
        );
    }
    if !fs::is_free_entry(&source, model, None) {
        return error_json(
            StatusCode::BAD_REQUEST,
            format!("模型 {model} 不是 {} 的免费候选", source.label),
            "invalid_request_error",
            "model_not_free",
        );
    }
    let result = probe_model(&state, &source, model, timeout).await;
    Json(json!({"ok": true, "result": probe_payload(&source, model, &result)})).into_response()
}

/// `POST /api/free-sources/probe-all`：批量探测（受并发上限与退避保护）。
pub async fn free_sources_probe_all(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("source").and_then(Value::as_str).unwrap_or("");
    let timeout = body
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(PROBE_TIMEOUT_DEFAULT)
        .clamp(2.0, PROBE_TIMEOUT_MAX);
    let Some(source) = fs::source(id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知免费来源 {id}"),
            "invalid_request_error",
            "unknown_free_source",
        );
    };
    let models = match fetch_catalog(&source).await {
        Ok(items) => items.into_iter().map(|(mid, _)| mid).collect::<Vec<_>>(),
        Err(message) => {
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("拉取免费来源目录失败：{message}"),
                "upstream_error",
                "free_source_catalog_failed",
            )
        }
    };
    let selected: Vec<String> = match body.get("models").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .filter(|m| models.contains(m))
            .collect(),
        None => models.clone(),
    };
    if selected.is_empty() {
        return Json(json!({"ok": true, "results": [], "count": 0})).into_response();
    }

    // 退避：处于退避期的模型不重复消耗额度，直接标记并跳过。
    let now = now_ms();
    let mut skipped = Vec::new();
    let mut targets = Vec::new();
    for model in &selected {
        let (backing_off, remaining) = {
            let cache = state
                .free_probe_cache
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            (
                cache.is_backing_off(&source.id, model, now),
                cache.backoff_remaining_minutes(&source.id, model, now),
            )
        };
        if backing_off {
            let reason = fs::reject_reason("rate_limited_upstream");
            skipped.push(json!({
                "source": source.id,
                "model": model,
                "state": reason.as_ref().map(|r| r.state.clone()).unwrap_or_else(|| "transient".into()),
                "reasonCode": "rate_limited_upstream",
                "reasonLabel": reason.as_ref().map(|r| r.label.clone()).unwrap_or_default(),
                "advice": reason.as_ref().map(|r| r.advice.clone()).unwrap_or_default(),
                "status": 0,
                "raw": "",
                "latencyMs": 0,
                "probedAt": chrono::Utc::now().to_rfc3339(),
                "usage": Value::Null,
                "skipped": true,
                "backoffMinutes": remaining,
            }));
        } else {
            targets.push(model.clone());
        }
    }

    // 分批并发（上限 PROBE_ALL_CONCURRENCY），避免同时打满 IP 限流额度。
    let mut results = skipped;
    for chunk in targets.chunks(PROBE_ALL_CONCURRENCY) {
        let mut handles = Vec::new();
        for model in chunk {
            let state = state.clone();
            let source = source.clone();
            let model = model.clone();
            handles.push(tokio::spawn(async move {
                let result = probe_model(&state, &source, &model, timeout).await;
                (model, result)
            }));
        }
        for handle in handles {
            if let Ok((model, result)) = handle.await {
                {
                    let mut cache = state
                        .free_probe_cache
                        .lock()
                        .unwrap_or_else(|err| err.into_inner());
                    cache.record(&source.id, &model, result.state, now_ms());
                }
                results.push(probe_payload(&source, &model, &result));
            }
        }
    }

    let available = results.iter().filter(|r| r["state"] == "available").count();
    Json(json!({
        "ok": true,
        "source": source.id,
        "count": results.len(),
        "results": results,
        "level": level_of(selected.len(), available),
        "checkedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

/// `POST /api/free-sources/attach`：一键接入（写配置到 bridge.config.json）。
///
/// 复用既有的配置落盘通道（合并 → 备份 → 原子写 → 回读），与面板「保存配置」同一套
/// 备份策略与校验流程。
pub async fn free_sources_attach(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let id = body.get("source").and_then(Value::as_str).unwrap_or("");
    let Some(source) = fs::source(id) else {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("未知免费来源 {id}"),
            "invalid_request_error",
            "unknown_free_source",
        );
    };
    let cfg = state.cfg();
    let current_raw = cfg.raw.clone();
    let top_models_empty = cfg.models.is_empty();
    drop(cfg);

    // 模型清单：优先用请求里给的，否则实时拉目录。
    let models: Vec<String> = match body.get("models").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .filter(|m| fs::is_free_entry(&source, m, None))
            .collect(),
        None => match fetch_catalog(&source).await {
            Ok(items) => items.into_iter().map(|(mid, _)| mid).collect(),
            Err(message) => {
                return error_json(
                    StatusCode::BAD_GATEWAY,
                    format!("拉取免费来源目录失败：{message}"),
                    "upstream_error",
                    "free_source_catalog_failed",
                )
            }
        },
    };

    let outcome = fs::build_attach(&source, &models, top_models_empty);

    // 合并进配置：upstreams 按名字增删改，其余顶层键保持不变。
    let mut incoming = current_raw.clone();
    let obj = incoming.as_object_mut().expect("配置根是对象");
    let ups = obj
        .entry("upstreams".to_string())
        .or_insert_with(|| json!({}));
    if let Some(ups) = ups.as_object_mut() {
        ups.insert(source.upstream.clone(), outcome.upstream.clone());
    }

    let merged = crate::config_store::merge_incoming(&current_raw, &incoming);
    let candidate = Config::from_value(merged.clone());
    let diagnostics = candidate.diagnostics();
    let errors: Vec<&crate::config::Diagnostic> =
        diagnostics.iter().filter(|d| d.level == "error").collect();
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": {
                    "message": format!("配置有 {} 处未通过：{}", errors.len(), errors[0].message),
                    "type": "invalid_request_error",
                    "code": "invalid_config",
                },
                "diagnostics": diagnostics.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            })),
        )
            .into_response();
    }

    // 落盘：与面板保存同一套备份策略（首次保存强制备份，保留 5 份）。
    let policy =
        crate::config_store::BackupPolicy::from_flag(body.get("backup").and_then(Value::as_bool));
    if let Err(err) = crate::config_store::backup_config(&state.config_path, policy) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("备份配置失败：{err}"),
            "server_error",
            "config_backup_failed",
        );
    }
    let mut to_write = merged.clone();
    crate::config_store::strip_secrets(&mut to_write);
    if let Err(err) = crate::config_store::save_atomic(&state.config_path, &to_write) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入配置失败：{err}"),
            "server_error",
            "config_write_failed",
        );
    }
    // 回读校验：写完重新加载，确认落盘内容可用。
    match Config::load(&state.config_path) {
        Ok(loaded) => {
            state.cfg.store(Arc::new(loaded));
            state.clear_models_cache().await;
            Json(json!({
                "ok": true,
                "source": source.id,
                "upstream": source.upstream,
                "modelsWritten": outcome.models_written,
                "modelCount": models.len(),
                "notices": outcome.notices,
                "configPath": state.config_path.display().to_string(),
            }))
            .into_response()
        }
        Err(err) => error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入后回读失败：{err}"),
            "server_error",
            "config_reload_failed",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kilo() -> fs::FreeSource {
        fs::source("kilo-free").expect("kilo 来源")
    }

    fn zen() -> fs::FreeSource {
        fs::source("zen-free").expect("zen 来源")
    }

    /// 同一份错误正文必须稳定归入同一拒因码（面板据此做「同一原因」聚合）。
    ///
    /// 用例取自 Kilo 免密池的真实报错（2026-10-20 实测），状态码与正文一并使用。
    #[test]
    fn same_error_body_maps_to_same_reason_code() {
        let cases: &[(u16, &str, &str)] = &[
            (
                401,
                "{\"code\":\"PAID_MODEL_AUTH_REQUIRED\",\"message\":\"You need to sign in to use this model.\"}",
                "paid_model_auth_required",
            ),
            (
                429,
                "Provider returned error; code: 429; metadata={\"limit_source\":\"upstream_provider_shared_pool\"}",
                "rate_limited_upstream",
            ),
            (404, "Provider returned error; code: 404", "model_not_found"),
            (403, "not available in your country", "region_blocked"),
            (403, "{\"type\":\"FreeTierError\"}", "free_tier_rejected"),
        ];
        for (status, body, expected) in cases {
            let first = fs::classify_error(*status, body);
            let second = fs::classify_error(*status, body);
            assert_eq!(first.code, *expected, "拒因码不符：{body}");
            assert_eq!(first.code, second.code, "同一正文两次归类不一致：{body}");
            // 分类器的状态必须与拒因字典一致（否则面板状态会与字典漂移）。
            let reason = fs::reject_reason(&first.code).expect("拒因在字典中");
            assert_eq!(
                reason.state,
                first.state.as_str(),
                "拒因 {code} 的字典状态与分类器不一致",
                code = first.code
            );
        }
    }

    /// 拒因字典是状态的单一事实源：分类器与字典不得漂移。
    #[test]
    fn every_classified_reason_agrees_with_dictionary() {
        let cases: &[u16] = &[400, 401, 403, 404, 429, 500, 502, 503, 504];
        let bodies = [
            "plain error body",
            "not available in your country",
            "daily limit reached",
            "model is unavailable",
            "Provider returned error; code: 429",
        ];
        for status in cases {
            for body in bodies {
                let rejection = fs::classify_error(*status, body);
                let reason = fs::reject_reason(&rejection.code)
                    .unwrap_or_else(|| panic!("拒因 {} 不在字典中", rejection.code));
                assert_eq!(
                    reason.state,
                    rejection.state.as_str(),
                    "状态漂移：status={status} body={body} code={}",
                    rejection.code
                );
            }
        }
    }

    /// 探测响应不得包含任何凭据或指纹头。
    #[test]
    fn probe_payload_never_leaks_credentials_or_fingerprint() {
        let source = kilo();
        let result = ProbeResult {
            state: fs::ProbeState::Transient,
            code: "rate_limited_upstream".into(),
            status: 429,
            raw: "Provider returned error; code: 429".into(),
            latency_ms: 3121,
            usage: None,
        };
        let payload = probe_payload(&source, "minimax/minimax-m3.1-flash:free", &result);
        let text = payload.to_string();
        for forbidden in [
            "Bearer",
            "authorization",
            "x-opencode-session",
            "x-opencode-request",
            "public",
            "apiKey",
        ] {
            assert!(
                !text.contains(forbidden),
                "探测响应泄露了 {forbidden}：{text}"
            );
        }
        assert_eq!(payload["state"], "transient");
        assert_eq!(payload["reasonCode"], "rate_limited_upstream");
        assert!(!payload["advice"].as_str().unwrap_or("").is_empty());
    }

    /// 可用结果不带拒因码，但状态为 available。
    #[test]
    fn available_payload_has_no_reason_code() {
        let source = zen();
        let result = ProbeResult {
            state: fs::ProbeState::Available,
            code: String::new(),
            status: 200,
            raw: String::new(),
            latency_ms: 1200,
            usage: Some(json!({"prompt_tokens": 10, "completion_tokens": 5})),
        };
        let payload = probe_payload(&source, "big-pickle", &result);
        assert_eq!(payload["state"], "available");
        assert!(payload["reasonCode"].is_null());
    }

    /// 鱼缸水位：候选为 0 时不得出现除零。
    #[test]
    fn level_handles_zero_candidates() {
        let level = level_of(0, 0);
        assert_eq!(level["candidates"], 0);
        assert_eq!(level["available"], 0);
        assert_eq!(level["ratio"], 0.0);
        let level = level_of(20, 14);
        assert_eq!(level["ratio"], 70.0);
    }

    /// 非免费模型不得被当成免费候选探测。
    #[test]
    fn probe_rejects_non_free_model() {
        let source = kilo();
        assert!(!fs::is_free_entry(
            &source,
            "anthropic/claude-haiku-5.5",
            None
        ));
        assert!(fs::is_free_entry(
            &source,
            "minimax/minimax-m3.1-flash:free",
            None
        ));
    }
}
