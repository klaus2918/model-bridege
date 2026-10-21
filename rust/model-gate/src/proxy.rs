//! HTTP 代理内核（迁移中：先落地 OpenAI 上游，Anthropic/CommandCode 在 P2 补齐）。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::TimeZone;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::anthropic;
use crate::commandcode;
use crate::config::{Config, Upstream};
use crate::db::{Db, Filters, UsageRecord};
use crate::opencode_transport as opencode;

use arc_swap::ArcSwap;

/// 拉取到的模型目录缓存（与 Python `_models_cache` 一致：`at`/`ids`/`owner`）。
#[derive(Clone, Default)]
pub struct ModelsCache {
    pub at_ms: i64,
    pub ids: Vec<String>,
    pub owner: HashMap<String, String>,
}

/// 查询上游模型清单失败的原因。
///
/// 面板需要区分「上游不可达 / 鉴权失败 / 返回体无法解析」与「上游确实没有模型」，
/// 否则会把故障显示成空清单，用户无从判断该改密钥还是该改代码。
#[derive(Debug, Clone)]
pub struct ModelQueryError {
    /// 面向用户的一句话原因。
    pub message: String,
    /// 修复提示（面板直接展示）。
    pub hint: String,
    /// 错误分类：`network` / `timeout` / `http` / `parse`。
    pub kind: &'static str,
}

/// 单次查询返回的模型条数上限。
///
/// 上游清单可达数百上千条（OpenRouter 级别过万），全量返回会让响应体
/// 与内存无谓膨胀。超出部分截断，面板仍可看到足够多候选。
pub const MODEL_QUERY_MAX_ITEMS: usize = 2000;

pub struct AppState {
    pub cfg: ArcSwap<Config>,
    pub db: Arc<Db>,
    pub client: reqwest::Client,
    pub started: Instant,
    pub config_path: std::path::PathBuf,
    pub thinking: crate::thinking::ThinkingCache,
    pub models_cache: tokio::sync::RwLock<ModelsCache>,
    pub update: Arc<crate::update::UpdateService>,
    /// 免费来源探测缓存：记录每个 (source, model) 的最近探测与退避次数。
    ///
    /// 为什么需要它：免密池的失败主因是上游池限流（429），反复重试只会
    /// 更快耗尽本机 IP 的免费额度（Kilo 匿名 200 次/小时）。命中退避期的
    /// 模型直接跳过探测并回填上次的拒因。
    pub free_probe_cache: std::sync::Mutex<crate::free_sources::ProbeCache>,
    /// 服务线程的 Tokio 句柄；托盘「刷新模型目录」用它触发异步刷新。
    pub runtime: std::sync::OnceLock<tokio::runtime::Handle>,
}

impl AppState {
    pub fn new(mut cfg: Config, db: Arc<Db>, config_path: std::path::PathBuf) -> Self {
        // 实际监听地址回写 raw，保证 /api/config 与 /api/info 展示一致
        // （--port/端口降级后，文件里的旧端口不应再冒充“当前端口”）。
        cfg.raw["port"] = json!(cfg.port);
        cfg.raw["host"] = json!(cfg.host);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(cfg.timeout_seconds))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client");
        let thinking_enabled = cfg
            .raw
            .get("thinking")
            .and_then(Value::as_object)
            .and_then(|t| t.get("passthrough"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let thinking_size = cfg
            .raw
            .get("thinking")
            .and_then(Value::as_object)
            .and_then(|t| t.get("cacheSize"))
            .and_then(Value::as_u64)
            .unwrap_or(500) as usize;
        let install_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let update = Arc::new(crate::update::UpdateService::new(
            env!("CARGO_PKG_VERSION").parse().expect("crate version"),
            db.clone(),
            install_dir,
        ));
        Self {
            cfg: ArcSwap::from_pointee(cfg),
            db,
            client,
            started: Instant::now(),
            config_path,
            thinking: crate::thinking::ThinkingCache::new(thinking_enabled, thinking_size),
            models_cache: tokio::sync::RwLock::new(ModelsCache::default()),
            update,
            free_probe_cache: std::sync::Mutex::new(crate::free_sources::ProbeCache::default()),
            runtime: std::sync::OnceLock::new(),
        }
    }

    pub fn cfg(&self) -> arc_swap::Guard<Arc<Config>> {
        self.cfg.load()
    }

    /// 拉取单个上游的模型清单；失败/未开启返回空（对齐 `proxy.py:fetch_upstream_models`）。
    ///
    /// 这是「运行期缓存」用的宽松版本：任何失败都退化成空数组。
    /// 需要区分失败原因（面板查询清单）用 [`Self::query_upstream_models`]。
    async fn fetch_upstream_models(&self, up: &Upstream) -> Vec<String> {
        // 守卫必须留在这里：`fetchModels=false` 表示「运行期不要自动拉取」，
        // 手动查询（query_upstream_models）不受此限制。
        if !up.fetch_models {
            return Vec::new();
        }
        match self.query_upstream_models(up, 8.0).await {
            Ok(models) => models,
            Err(_) => Vec::new(),
        }
    }

    /// 拉取单个上游的模型清单，**保留失败原因**。
    ///
    /// 与 [`Self::fetch_upstream_models`] 的唯一区别是错误不再被吞掉：
    /// 面板需要区分「上游不可达」「鉴权失败」「返回体无法解析」
    /// 与「上游确实没有模型」，否则会把故障显示成空清单。
    ///
    /// 通用约定：`GET {baseUrl}{compat.endpoint.models}`（默认 `/v1/models`），
    /// 解析 `data` / `models` 数组里的 `id` / `slug` / `model`。
    ///
    /// **transport 感知**：`transport=opencode` 的上游（OpenCode Zen 免费层）
    /// 目录端点同样只对官方客户端开放——裸请求会被拒（实测 401）。
    /// 因此这类上游必须走与 chat 相同的两件事：路径重写（`/zen/...` → `/inference/...`）
    /// 与客户端指纹头。缺任何一样，查询清单就会失败。
    ///
    /// **目录来源感知**：`compat.catalog_source == "commandcode"` 的上游
    /// （CommandCode）**没有** `/v1/models` 端点，模型目录来自本机 CLI 包内的
    /// `models.md`。这类上游改走本地目录解析，绝不请求 HTTP 端点。
    pub async fn query_upstream_models(
        &self,
        up: &Upstream,
        timeout_secs: f64,
    ) -> Result<Vec<String>, ModelQueryError> {
        if up.compat.catalog_source == "commandcode" {
            return self.query_commandcode_catalog(up);
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &up.compat.extra_headers {
            if let (Ok(n), Ok(v)) = (
                name.parse::<reqwest::header::HeaderName>(),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(n, v);
            }
        }
        // 未配置凭据时 apiKey 为空：不伪造占位密钥，也不发送鉴权头。
        if !up.api_key.is_empty() && up.compat.auth_scheme != "none" {
            let key = up.api_key.clone();
            let bearer = key_with_prefix(&up.compat.auth_prefix, &key);
            match up.compat.auth_scheme.as_str() {
                "x-api-key" => insert_header(&mut headers, "x-api-key", &key),
                "both" => {
                    insert_header(&mut headers, "authorization", &format!("Bearer {key}"));
                    insert_header(&mut headers, "x-api-key", &key);
                }
                "header" => {
                    let name = if up.compat.auth_header.is_empty() {
                        "authorization"
                    } else {
                        &up.compat.auth_header
                    };
                    insert_header(&mut headers, name, &bearer);
                }
                _ => insert_header(&mut headers, "authorization", &bearer),
            }
        }
        let is_opencode = up.transport == "opencode";
        if is_opencode {
            // 与 chat 路径完全一致：客户端指纹（匿名 key + UA + 会话/请求 ID）。
            let session = crate::opencode_transport::session_for(
                &crate::opencode_transport::session_seed(&json!({
                    "model": "",
                    "messages": [],
                })),
            );
            crate::opencode_transport::apply_headers(&mut headers, &session);
        }
        let mut url = format!(
            "{}{}",
            up.base_url.trim_end_matches('/'),
            up.compat.models_path
        );
        if is_opencode {
            // `/zen/...` → `/inference/...`（go 订阅路径不改写）。
            url = crate::opencode_transport::rewrite_free_url(&url);
        }
        let response = self
            .client
            .get(&url)
            .headers(headers)
            .timeout(std::time::Duration::from_secs_f64(
                timeout_secs.clamp(1.0, 60.0),
            ))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                let timed_out = err.is_timeout();
                return Err(ModelQueryError {
                    message: if timed_out {
                        format!("上游请求超时（{timeout_secs:.0}s）：{url}")
                    } else {
                        format!("上游请求失败：{err}")
                    },
                    hint: if timed_out {
                        "上游响应太慢，可稍后重试或调大超时".to_string()
                    } else {
                        "检查 baseUrl 是否可达、端点是否被墙".to_string()
                    },
                    kind: if timed_out { "timeout" } else { "network" },
                });
            }
        };
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(ModelQueryError {
                message: format!("上游返回 HTTP {status}"),
                hint: if is_opencode {
                    // transport=opencode 的上游不需要 apiKey：401/403 通常是
                    // 客户端指纹缺失或免费层契约变更，不是"没配密钥"。
                    match status {
                        401 | 403 => "该上游使用 transport=opencode（免密层）：不填 apiKey，\
                             请确认客户端指纹头未被自定义头覆盖"
                            .to_string(),
                        404 => "该上游使用 transport=opencode：目录路径应自动改写为 \
                             /inference/v1/models；请确认 compat.endpoint.models 路径"
                            .to_string(),
                        _ => "该上游使用 transport=opencode（免密层）：检查客户端指纹与网络"
                            .to_string(),
                    }
                } else {
                    match status {
                        401 | 403 => "检查 apiKey / apiKeyEnv / agent 密钥来源".to_string(),
                        404 => "检查 compat.endpoint.models 路径是否正确（默认 /v1/models）"
                            .to_string(),
                        429 => "上游限流，请稍后重试".to_string(),
                        _ => "检查 apiKey / baseUrl / 网络".to_string(),
                    }
                },
                kind: "http",
            });
        }
        let data = match response.json::<Value>().await {
            Ok(data) => data,
            Err(err) => {
                return Err(ModelQueryError {
                    message: format!("上游返回的不是合法 JSON：{err}"),
                    hint: "确认该端点返回 OpenAI 兼容的模型清单格式".to_string(),
                    kind: "parse",
                });
            }
        };
        let items = data
            .get("data")
            .or_else(|| data.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out: Vec<String> = Vec::new();
        // 去重用 HashSet：线性复杂度。上游清单可达数百上千条，
        // 用 Vec::contains 做去重会退化成 O(n²) 次字符串比较。
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in &items {
            if !item.is_object() {
                continue;
            }
            let ident = item
                .get("id")
                .or_else(|| item.get("slug"))
                .or_else(|| item.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string);
            // 去重并保持上游返回顺序。
            if let Some(ident) = ident {
                if !ident.is_empty() && seen.insert(ident.clone()) {
                    out.push(ident);
                }
            }
            // 上限保护：超大清单不让内存与响应体无限膨胀。
            if out.len() >= MODEL_QUERY_MAX_ITEMS {
                break;
            }
        }
        Ok(out)
    }

    /// 读取 CommandCode 的**本地**模型目录（本机 CLI 包内 `models.md`）。
    ///
    /// CommandCode 没有 `/v1/models` 端点，请求 HTTP 只会得到 404，
    /// 而 404 提示会把用户引向「改端点路径」这个错误方向。因此这类上游
    /// 改走本地目录解析；本机没装 CLI 时返回空清单，**不报 404**。
    ///
    /// 目录来源由 `compat.catalog_source == "commandcode"` 判定；
    /// 具体文件路径取自 `compat.catalog_plan`（`auto` 表示自动探测 CLI 安装根）。
    fn query_commandcode_catalog(&self, up: &Upstream) -> Result<Vec<String>, ModelQueryError> {
        let spec = match &up.compat.catalog_plan {
            serde_json::Value::String(text) if !text.is_empty() => Some(text.as_str()),
            serde_json::Value::Bool(false) => {
                return Ok(Vec::new());
            }
            _ => None,
        };
        let entries = crate::commandcode_catalog::read_catalog(spec);
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        // 按套餐过滤：只有当 catalog_plan 是具体档位名时才过滤。
        // "auto" 表示套餐未知（探测失败），此时不过滤，避免误伤用户。
        let plan = match &up.compat.catalog_plan {
            serde_json::Value::String(text) => text.clone(),
            _ => String::new(),
        };
        let rank = crate::commandcode_catalog::plan_rank(&plan);
        let entries = crate::commandcode_catalog::filter_by_plan(entries, &plan, rank);
        Ok(entries.into_iter().map(|entry| entry.id).collect())
    }

    /// 刷新模型目录缓存；`force=true` 时失败不清空旧缓存（对齐 `proxy.py:refresh_models_cache`）。
    pub async fn refresh_models_cache(&self, force: bool) -> ModelsCache {
        let ttl_ms = self.cfg().models_cache_seconds.saturating_mul(1000) as i64;
        let now_ms = chrono::Utc::now().timestamp_millis();
        {
            let cache = self.models_cache.read().await;
            if !force && now_ms - cache.at_ms <= ttl_ms {
                return cache.clone();
            }
        }
        let upstreams: Vec<Upstream> = self.cfg().upstreams.clone();
        let mut ids: Vec<String> = Vec::new();
        let mut owner: HashMap<String, String> = HashMap::new();
        for up in &upstreams {
            if !up.auto_models {
                continue;
            }
            for id in self.fetch_upstream_models(up).await {
                if !ids.iter().any(|existing| existing == &id) {
                    ids.push(id.clone());
                }
                owner.entry(id).or_insert_with(|| up.name.clone());
            }
        }
        let mut cache = self.models_cache.write().await;
        if force && ids.is_empty() && !cache.ids.is_empty() {
            return cache.clone();
        }
        *cache = ModelsCache {
            at_ms: now_ms,
            ids,
            owner,
        };
        cache.clone()
    }

    /// 配置重载后清空目录缓存，避免旧上游的模型继续出现在清单里。
    pub async fn clear_models_cache(&self) {
        *self.models_cache.write().await = ModelsCache::default();
    }

    /// 热重载：替换运行态配置（监听地址/端口由调用方保持不变）。
    pub fn replace_config(&self, mut cfg: Config) {
        let current = self.cfg();
        cfg.port = current.port;
        cfg.host = current.host.clone();
        cfg.raw["port"] = json!(cfg.port);
        cfg.raw["host"] = json!(cfg.host);
        self.cfg.store(Arc::new(cfg));
    }
}

pub fn router(state: Arc<AppState>) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/{upstream}/v1/models", get(models_forced))
        .route("/v1/chat/completions", post(chat))
        .route("/{upstream}/v1/chat/completions", post(chat_forced))
        .route("/ui/", get(ui_index))
        .route("/ui/{*path}", get(ui_asset))
        .route("/api/info", get(api_info))
        .route("/api/update/status", get(crate::update::api_update_status))
        .route("/api/update/check", get(crate::update::api_update_check))
        .route(
            "/api/update/download",
            post(crate::update::api_update_download),
        )
        .route(
            "/api/update/install",
            post(crate::update::api_update_install),
        )
        .route("/api/proxy/status", get(api_proxy_status))
        .route("/api/upstreams", get(api_upstreams))
        .route("/api/config", get(api_config).post(api_config_save))
        .route("/api/config/validate", post(api_config_validate))
        .route("/api/config/reload", axum::routing::post(api_config_reload))
        .route("/api/config/backups", get(crate::api_extra::backups_list))
        .route(
            "/api/config/backups/restore",
            post(crate::api_extra::backups_restore),
        )
        .route(
            "/api/pricing",
            get(api_pricing).post(crate::api_extra::pricing_save),
        )
        .route("/api/models/catalog", get(crate::api_extra::models_catalog))
        .route("/api/usage/clear", post(crate::api_extra::usage_clear))
        .route(
            "/api/usage/retention/apply",
            post(crate::api_extra::retention_apply),
        )
        .route("/api/usage/export", get(crate::api_extra::usage_export))
        .route("/api/logs", get(crate::api_extra::logs_get))
        .route("/api/logs/clear", post(crate::api_extra::logs_clear))
        .route(
            "/api/upstreams/health",
            get(crate::api_extra::upstreams_health),
        )
        .route(
            "/api/upstreams/probe",
            post(crate::api_extra::upstream_probe),
        )
        .route(
            "/api/upstreams/{name}/models",
            get(crate::api_extra::upstream_models),
        )
        .route(
            "/api/upstreams/models",
            post(crate::api_extra::upstream_models_post),
        )
        .route(
            "/api/upstreams/secret",
            post(crate::api_extra::upstream_secret),
        )
        .route(
            "/api/free-sources",
            get(crate::free_sources_api::free_sources),
        )
        .route(
            "/api/free-sources/catalog",
            post(crate::free_sources_api::free_sources_catalog),
        )
        .route(
            "/api/free-sources/probe",
            post(crate::free_sources_api::free_sources_probe),
        )
        .route(
            "/api/free-sources/probe-all",
            post(crate::free_sources_api::free_sources_probe_all),
        )
        .route(
            "/api/free-sources/attach",
            post(crate::free_sources_api::free_sources_attach),
        )
        .route("/api/usage/totals", get(api_usage_totals))
        .route("/api/usage/summary", get(api_usage_summary))
        .route("/api/usage/list", get(api_usage_list))
        .route("/api/usage/models", get(api_usage_models))
        .route("/api/usage/upstreams", get(api_usage_upstreams))
        .fallback(not_found)
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let ups: Vec<Value> = cfg
        .upstreams
        .iter()
        .map(|u| {
            json!({
                "name": u.name,
                "protocol": u.protocol,
                "transport": u.transport,
                "baseUrl": u.base_url,
                "endpoint": format!("http://{}:{}/{}/v1", cfg.host, cfg.port, u.name),
                "default": u.default,
                "models": u.models.len(),
                "keyConfigured": u.key_configured(),
                "keySource": u.key_source,
            })
        })
        .collect();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "runtime": "rust",
        "uptimeSeconds": state.started.elapsed().as_secs(),
        "upstreams": ups,
    }))
}

async fn models(State(state): State<Arc<AppState>>) -> Response {
    // 与 Python 一致：显式 models 白名单存在时不拉取；否则合并拉取缓存。
    let (mut ids, whitelist_empty) = {
        let cfg = state.cfg();
        (cfg.model_list(), cfg.models.is_empty())
    };
    let cached = if whitelist_empty {
        Some(state.refresh_models_cache(false).await)
    } else {
        None
    };
    if let Some(cache) = &cached {
        for id in &cache.ids {
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.clone());
            }
        }
    }
    let cfg = state.cfg();
    let data: Vec<Value> = ids
        .iter()
        .map(|id| {
            let owner = cached
                .as_ref()
                .and_then(|c| c.owner.get(id))
                .cloned()
                .or_else(|| {
                    cfg.upstreams
                        .iter()
                        .find(|u| u.models.iter().any(|m| m == id))
                        .map(|u| u.name.clone())
                })
                .unwrap_or_else(|| "model-gate".to_string());
            json!({"id": id, "object": "model", "created": 0, "owned_by": owner})
        })
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn models_forced(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let upstream = {
        let cfg = state.cfg();
        cfg.upstream(&name).cloned()
    };
    match upstream {
        Some(up) => {
            let data: Vec<Value> = up
                .models
                .iter()
                .map(|m| json!({"id": m, "object": "model", "created": 0, "owned_by": name}))
                .collect();
            Json(json!({"object": "list", "data": data})).into_response()
        }
        None => unknown_upstream(&state, &name),
    }
}

async fn chat(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    chat_inner(state, None, headers, body).await
}

async fn chat_forced(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    chat_inner(state, Some(name), headers, body).await
}

async fn chat_inner(
    state: Arc<AppState>,
    forced: Option<String>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "请求体不是合法 JSON",
                "invalid_request_error",
                "invalid_json",
            )
        }
    };
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| state.cfg().default_model.clone())
        .unwrap_or_default();
    if requested_model.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 model 字段且未配置 defaultModel",
            "invalid_request_error",
            "model_required",
        );
    }
    let cfg_guard = state.cfg();
    let upstream: Upstream = match forced.as_deref() {
        Some(name) => match cfg_guard.upstream(name) {
            Some(u) => u.clone(),
            None => return unknown_upstream(&state, name),
        },
        None => match cfg_guard.pick_upstream(Some(&requested_model)) {
            Some(u) => u.clone(),
            None => {
                return error_json(
                    StatusCode::BAD_GATEWAY,
                    "未配置任何上游",
                    "upstream_error",
                    "no_upstream",
                )
            }
        },
    };
    let max_tokens = cfg_guard.max_tokens;
    drop(cfg_guard);
    let target_model = upstream
        .model_map
        .get(&requested_model)
        .cloned()
        .unwrap_or_else(|| requested_model.clone());

    let client_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_opencode = upstream.transport == "opencode";
    if is_opencode && !opencode::is_free_model(&target_model) {
        return error_json(StatusCode::BAD_REQUEST,
                          format!("模型 {target_model} 不是 OpenCode 免费模型；transport=opencode 只接受 -free、big-pickle 或 grok-code"),
                          "invalid_request_error", "model_not_free");
    }
    let stream = client_stream || is_opencode;
    let base = upstream.base_url.trim_end_matches('/');
    // 上游/模型声明的最大输出：超过时收敛，避免上游直接拒绝或截断。
    let model_limit = upstream.model_max_output_tokens(&target_model);
    if let Some(limit) = model_limit {
        for key in ["max_tokens", "max_completion_tokens"] {
            if let Some(requested) = payload.get(key).and_then(Value::as_u64) {
                if requested > limit {
                    payload[key] = json!(limit);
                    tracing::warn!(
                        "上游 {} 的最大输出为 {}，已将请求的 {} 收敛到该值",
                        upstream.name,
                        limit,
                        requested
                    );
                }
            }
        }
    }
    let fallback_max = match model_limit {
        Some(limit) => max_tokens.min(limit),
        None => max_tokens,
    };
    let mut url = format!("{base}{}", upstream.compat.chat_path);
    if is_opencode {
        url = opencode::rewrite_free_url(&url);
    }
    let mut headers = reqwest::header::HeaderMap::new();
    let outbound: Value;
    let mut tool_state = opencode::ToolState::default();
    // 只有 wire 形态不同的协议需要分支；provider 差异全部走 compat。
    match upstream.protocol.as_str() {
        "openai" => {
            payload["model"] = json!(target_model);
            if is_opencode {
                payload["stream"] = json!(true);
                tool_state = opencode::prepare_tools(&mut payload);
            }
            outbound = payload.clone();
        }
        "anthropic" => {
            outbound = anthropic::to_anthropic(
                &payload,
                fallback_max,
                &target_model,
                &state.thinking,
                upstream.compat.thinking_passthrough,
            );
        }
        "commandcode" => {
            if (upstream.api_key.is_empty() || upstream.key_source == "none")
                && upstream.compat.auth_scheme != "none"
            {
                return error_json(StatusCode::UNAUTHORIZED,
                                  "未配置 CommandCode 凭据：请设置 COMMAND_CODE_API_KEY 环境变量，或在面板「写入密钥」保存",
                                  "authentication_error", "missing_api_key");
            }
            outbound = commandcode::build_payload(&payload, &target_model, fallback_max);
        }
        other => {
            return error_json(
                StatusCode::NOT_IMPLEMENTED,
                format!("Rust 内核尚未实现 {other} 协议（迁移中）"),
                "server_error",
                "not_implemented",
            );
        }
    }

    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    // 1) 额外静态头（provider 特有头都声明在这里；鉴权头随后覆盖同名项）
    for (name, value) in &upstream.compat.extra_headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    // Anthropic wire 必需版本头；值仍由上游字段控制（旧配置兼容）。
    if upstream.protocol == "anthropic" {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&upstream.anthropic_version) {
            headers.insert("anthropic-version", value);
        }
    }
    // 2) 鉴权：bearer / x-api-key / both / header；空 key 表示不发送鉴权头。
    if !upstream.api_key.is_empty() && upstream.compat.auth_scheme != "none" {
        let key = upstream.api_key.as_str();
        match upstream.compat.auth_scheme.as_str() {
            "x-api-key" => {
                if let Ok(value) = reqwest::header::HeaderValue::from_str(key) {
                    headers.insert("x-api-key", value);
                }
            }
            "both" => {
                if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                {
                    headers.insert(reqwest::header::AUTHORIZATION, value);
                }
                if let Ok(value) = reqwest::header::HeaderValue::from_str(key) {
                    headers.insert("x-api-key", value);
                }
            }
            "header" => {
                let name = upstream.compat.auth_header.to_lowercase();
                let value = format!("{}{key}", upstream.compat.auth_prefix);
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&value),
                ) {
                    headers.insert(name, value);
                }
            }
            _ => {
                let prefix = if upstream.compat.auth_prefix.is_empty() {
                    "Bearer "
                } else {
                    upstream.compat.auth_prefix.as_str()
                };
                if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("{prefix}{key}"))
                {
                    headers.insert(reqwest::header::AUTHORIZATION, value);
                }
            }
        }
    }
    // 3) 会话头：opencode / commandcode 等由 compat 声明，不再写死协议名。
    if !upstream.compat.session_header.is_empty() {
        let raw = session_id(&payload);
        let value = if upstream.compat.session_id_style == "sess16" {
            format!("sess_{}", raw.chars().take(16).collect::<String>())
        } else {
            raw
        };
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(
                upstream.compat.session_header.to_lowercase().as_bytes(),
            ),
            reqwest::header::HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    }
    // OpenCode 免费层：固定匿名 key + 客户端指纹；覆盖旧会话头。
    if is_opencode {
        let session = opencode::session_for(&opencode::session_seed(&payload));
        opencode::apply_headers(&mut headers, &session);
    }
    // 4) CLI 版本/环境头（commandcode 等）：名字由 compat 声明，值来自上游字段。
    if !upstream.compat.version_header.is_empty()
        && upstream.cli_version != "auto"
        && !upstream.cli_version.is_empty()
    {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(
                upstream.compat.version_header.to_lowercase().as_bytes(),
            ),
            reqwest::header::HeaderValue::from_str(&upstream.cli_version),
        ) {
            headers.insert(name, value);
        }
    }
    if !upstream.compat.environment_header.is_empty() && !upstream.cli_environment.is_empty() {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(
                upstream.compat.environment_header.to_lowercase().as_bytes(),
            ),
            reqwest::header::HeaderValue::from_str(&upstream.cli_environment),
        ) {
            headers.insert(name, value);
        }
    }
    if upstream.compat.zdr {
        headers.insert("x-cmd-zdr", "1".parse().unwrap());
    }

    let retry_headers = headers.clone();
    let retry_payload = outbound.clone();
    let response = match state
        .client
        .post(&url)
        .headers(headers)
        .json(&outbound)
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            record(
                &state,
                &upstream,
                &requested_model,
                &target_model,
                started,
                502,
                false,
                None,
                Some(err.to_string()),
            );
            return error_json(
                StatusCode::BAD_GATEWAY,
                format!("无法连接上游 {}: {err}", upstream.base_url),
                "upstream_error",
                "upstream_failed",
            );
        }
    };
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.clone());
        record(
            &state,
            &upstream,
            &requested_model,
            &target_model,
            started,
            status.as_u16(),
            stream,
            None,
            Some(message.clone()),
        );
        return error_json(
            status,
            format!("上游返回 {}: {message}", status.as_u16()),
            "upstream_error",
            "upstream_failed",
        );
    }
    if is_opencode && !client_stream {
        let text = response.text().await.unwrap_or_default();
        let (value, usage) = opencode::aggregate_sse(&text, &tool_state, &requested_model);
        record_ex(
            &state,
            &upstream,
            &requested_model,
            &target_model,
            started,
            200,
            false,
            usage.as_ref(),
            None,
            0,
            false,
            false,
        );
        let mut out = Json(value).into_response();
        out.headers_mut()
            .insert("x-bridge-upstream", upstream.name.parse().unwrap());
        out.headers_mut()
            .insert("x-bridge-upstream-model", target_model.parse().unwrap());
        return out;
    }
    if !stream {
        let text = response.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<Value>(&text).ok();
        let (usage, mut value, err, truncated, paused) = match upstream.protocol.as_str() {
            "anthropic" => {
                let data = parsed.unwrap_or_else(|| json!({}));
                (
                    Some(anthropic::usage_of(
                        data.get("usage").unwrap_or(&Value::Null),
                    )),
                    anthropic::to_openai(&data, &requested_model, &state.thinking),
                    None,
                    false,
                    false,
                )
            }
            "commandcode" => {
                let mut st = commandcode::consume_ndjson(&text);
                let mut attempts = 0;
                while st.paused && upstream.pause_turn && attempts < 5 {
                    attempts += 1;
                    st.paused = false;
                    let retry = state
                        .client
                        .post(&url)
                        .headers(retry_headers.clone())
                        .json(&retry_payload)
                        .send()
                        .await;
                    match retry {
                        Ok(resp) if resp.status().is_success() => {
                            let more = resp.text().await.unwrap_or_default();
                            st.merge(commandcode::consume_ndjson(&more));
                        }
                        _ => break,
                    }
                }
                let body = commandcode::body_from_state(&st, &requested_model);
                let err = if st.error.is_empty() {
                    None
                } else {
                    Some(st.error.clone())
                };
                (Some(st.usage()), body, err, !st.saw_finish, st.paused)
            }
            _ => {
                let parsed = parsed
                    .unwrap_or_else(|| json!({"error": {"message": "上游响应不是合法 JSON"}}));
                (parsed.get("usage").cloned(), parsed, None, false, false)
            }
        };
        record_ex(
            &state,
            &upstream,
            &requested_model,
            &target_model,
            started,
            200,
            false,
            usage.as_ref(),
            err,
            0,
            truncated,
            paused,
        );
        if upstream.protocol == "openai" {
            value["model"] = json!(requested_model);
        }
        let mut out = Json(value).into_response();
        out.headers_mut()
            .insert("x-bridge-upstream", upstream.name.parse().unwrap());
        out.headers_mut()
            .insert("x-bridge-upstream-model", target_model.parse().unwrap());
        return out;
    }
    let pause_turn = upstream.pause_turn;
    match upstream.protocol.as_str() {
        "anthropic" => {
            stream_anthropic(state, upstream, requested_model, target_model, response).await
        }
        "commandcode" => {
            stream_commandcode(
                state,
                upstream,
                requested_model,
                target_model,
                response,
                url,
                retry_headers,
                retry_payload,
                pause_turn,
            )
            .await
        }
        _ if is_opencode => {
            stream_opencode(
                state,
                upstream,
                requested_model,
                target_model,
                response,
                tool_state,
            )
            .await
        }
        _ => stream_response(state, upstream, requested_model, target_model, response).await,
    }
}

async fn stream_response(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
) -> Response {
    let started = Instant::now();
    let name = upstream.name.clone();
    let header_model = target_model.clone();
    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer = String::new();
        let mut usage: Option<Value> = None;
        let mut ttft_ms = 0i64;
        while let Some(chunk) = byte_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    // 逐块扫描 SSE，抓取末尾的 usage（不改变转发字节）。
                    while let Some(pos) = buffer.find("\n\n") {
                        let block: String = buffer.drain(..pos + 2).collect();
                        for line in block.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data.trim() == "[DONE]" { continue; }
                                if let Ok(v) = serde_json::from_str::<Value>(data) {
                                    if let Some(u) = v.get("usage") { usage = Some(u.clone()); }
                                }
                            }
                        }
                    }
                    if ttft_ms == 0 {
                        ttft_ms = started.elapsed().as_millis() as i64;
                    }
                    yield Ok::<Bytes, std::convert::Infallible>(bytes);
                }
                Err(err) => {
                    yield Ok(Bytes::from(format!("data: {{\"error\":\"{err}\"}}\n\n")));
                    break;
                }
            }
        }
        record_ex(&state, &upstream, &requested_model, &target_model, started, 200, true,
                  usage.as_ref(), None, ttft_ms, false, false);
    };
    let mut out = Response::new(Body::from_stream(stream));
    out.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    out.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream", name.parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream-model", header_model.parse().unwrap());
    out
}

async fn stream_opencode(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
    tool_state: opencode::ToolState,
) -> Response {
    let started = Instant::now();
    let name = upstream.name.clone();
    let header_model = target_model.clone();
    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer: Vec<u8> = Vec::new();
        let mut rewriter = opencode::StreamRewriter::new(&tool_state, &requested_model);
        let mut usage: Option<Value> = None;
        let mut done = false;
        let mut ttft_ms = 0i64;
        'outer: while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(err) => {
                    yield Ok::<Bytes, std::convert::Infallible>(Bytes::from(format!(
                        "data: {{\"error\":\"{err}\"}}\n\n")));
                    break;
                }
            };
            buffer.extend_from_slice(&bytes);
            while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&buffer[..pos])
                    .trim_end_matches('\r')
                    .to_string();
                buffer.drain(..=pos);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    if let Some(tail) = rewriter.finalize() {
                        yield Ok(Bytes::from(format!(
                            "data: {}\n\n", serde_json::to_string(&tail).unwrap_or_default())));
                    }
                    yield Ok(Bytes::from("data: [DONE]\n\n"));
                    done = true;
                    break 'outer;
                }
                let Ok(mut value) = serde_json::from_str::<Value>(data) else {
                    yield Ok(Bytes::from(format!("data: {data}\n\n")));
                    continue;
                };
                if let Some(u) = value.get("usage") {
                    usage = Some(u.clone());
                }
                rewriter.rewrite(&mut value);
                if ttft_ms == 0 {
                    ttft_ms = started.elapsed().as_millis() as i64;
                }
                yield Ok(Bytes::from(format!(
                    "data: {}\n\n", serde_json::to_string(&value).unwrap_or_default())));
            }
        }
        if !done {
            if let Some(tail) = rewriter.finalize() {
                yield Ok(Bytes::from(format!(
                    "data: {}\n\n", serde_json::to_string(&tail).unwrap_or_default())));
            }
            yield Ok(Bytes::from("data: [DONE]\n\n"));
        }
        record_ex(&state, &upstream, &requested_model, &target_model, started, 200, true,
                  usage.as_ref(), None, ttft_ms, false, false);
    };
    let mut out = Response::new(Body::from_stream(stream));
    out.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    out.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream", name.parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream-model", header_model.parse().unwrap());
    out
}

async fn stream_commandcode(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
    url: String,
    headers: reqwest::header::HeaderMap,
    payload: Value,
    pause_turn: bool,
) -> Response {
    let started = Instant::now();
    let name = upstream.name.clone();
    let header_model = target_model.clone();
    let mut current = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer = String::new();
        let mut st = commandcode::StreamState::new();
        let mut attempts = 0;
        let mut ttft_ms = 0i64;
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from(commandcode::chunk(
            &requested_model, json!({"role": "assistant", "content": ""}), None, None)));
        loop {
            while let Some(chunk) = current.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(pos) = buffer.find('\n') {
                            let line: String = buffer.drain(..pos + 1).collect();
                            let line = line.trim();
                            if line.is_empty() { continue; }
                            if let Ok(ev) = serde_json::from_str::<Value>(line) {
                                let out = commandcode::handle_event(&ev, &mut st, &requested_model);
                                if !out.is_empty() {
                                    if ttft_ms == 0 {
                                        ttft_ms = started.elapsed().as_millis() as i64;
                                    }
                                    yield Ok(Bytes::from(out));
                                }
                            }
                        }
                    }
                    Err(err) => {
                        yield Ok(Bytes::from(format!("data: {{\"error\":\"{err}\"}}\n\n")));
                        break;
                    }
                }
            }
            if !(st.paused && pause_turn && attempts < 5) {
                break;
            }
            attempts += 1;
            st.paused = false;
            match state.client.post(&url).headers(headers.clone()).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    current = resp.bytes_stream();
                }
                _ => break,
            }
        }
        yield Ok(Bytes::from(commandcode::chunk(
            &requested_model, json!({}), Some(&st.finish), Some(st.openai_usage()))));
        yield Ok(Bytes::from("data: [DONE]\n\n"));
        let err = if st.error.is_empty() { None } else { Some(st.error.clone()) };
        record_ex(&state, &upstream, &requested_model, &target_model, started, 200, true,
                  Some(&st.usage()), err, ttft_ms, !st.saw_finish, st.paused);
    };
    let mut out = Response::new(Body::from_stream(stream));
    out.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    out.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream", name.parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream-model", header_model.parse().unwrap());
    out
}

async fn stream_anthropic(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
) -> Response {
    let started = Instant::now();
    let name = upstream.name.clone();
    let header_model = target_model.clone();
    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        let mut buffer = String::new();
        let mut st = anthropic::StreamState::new();
        let mut ttft_ms = 0i64;
        while let Some(chunk) = byte_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buffer.find("\n\n") {
                        let block: String = buffer.drain(..pos + 2).collect();
                        let text = anthropic::translate_sse_block(&block, &requested_model, &mut st);
                        if !text.is_empty() {
                            if ttft_ms == 0 {
                                ttft_ms = started.elapsed().as_millis() as i64;
                            }
                            yield Ok::<Bytes, std::convert::Infallible>(Bytes::from(text));
                        }
                    }
                }
                Err(err) => {
                    yield Ok(Bytes::from(format!("data: {{\"error\":\"{err}\"}}\n\n")));
                    break;
                }
            }
        }
        if !st.finished {
            yield Ok(Bytes::from("data: [DONE]\n\n"));
        }
        // 流结束即缓存本轮思考内容，供下一轮 DeepSeek 思考链回传。
        st.remember(&state.thinking);
        let usage = json!({
            "prompt_tokens": st.prompt,
            "completion_tokens": st.completion,
            "cacheReadTokens": st.cache_read,
            "cacheWriteTokens": st.cache_write,
        });
        record_ex(&state, &upstream, &requested_model, &target_model, started, 200, true,
                  Some(&usage), None, ttft_ms, false, false);
    };
    let mut out = Response::new(Body::from_stream(stream));
    out.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    out.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream", name.parse().unwrap());
    out.headers_mut()
        .insert("x-bridge-upstream-model", header_model.parse().unwrap());
    out
}

fn record(
    state: &AppState,
    up: &Upstream,
    requested: &str,
    target: &str,
    started: Instant,
    status: u16,
    stream: bool,
    usage: Option<&Value>,
    error: Option<String>,
) {
    record_ex(
        state, up, requested, target, started, status, stream, usage, error, 0, false, false,
    );
}

fn record_ex(
    state: &AppState,
    up: &Upstream,
    requested: &str,
    target: &str,
    started: Instant,
    status: u16,
    stream: bool,
    usage: Option<&Value>,
    error: Option<String>,
    ttft_ms: i64,
    truncated: bool,
    paused: bool,
) {
    let usage = usage.cloned().unwrap_or(Value::Null);
    let rec = UsageRecord {
        upstream: up.name.clone(),
        protocol: up.protocol.clone(),
        requested_model: requested.to_string(),
        upstream_model: target.to_string(),
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .or_else(|| usage.get("cacheReadTokens").and_then(Value::as_i64))
            .unwrap_or(0),
        cache_write_tokens: usage
            .get("cacheWriteTokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        duration_ms: started.elapsed().as_millis() as i64,
        ttft_ms,
        status: status as i64,
        stream,
        truncated,
        paused,
        error,
        cost: 0.0,
        session: String::new(),
    };
    state.db.record_usage(&rec);
    log_access(state, &rec, ttft_ms, truncated, paused);
}

/// 请求调用日志：受 `accessLog` 开关控制，格式对齐 Python 版（模型 -> 上游:目标 状态 耗时 tokens）。
fn log_access(state: &AppState, rec: &UsageRecord, ttft_ms: i64, truncated: bool, paused: bool) {
    if !state.cfg().access_log {
        return;
    }
    let mut extra = String::new();
    if rec.stream {
        extra.push_str(&format!(" ttft={ttft_ms}ms stream"));
    }
    if truncated {
        extra.push_str(" truncated");
    }
    if paused {
        extra.push_str(" paused");
    }
    let route = format!(
        "{} -> {}:{}",
        rec.requested_model, rec.upstream, rec.upstream_model
    );
    match rec.error.as_deref().filter(|err| !err.is_empty()) {
        Some(err) => tracing::warn!(
            "{} {} {}ms 失败: {}{}",
            route,
            rec.status,
            rec.duration_ms,
            brief_text(err, 200),
            extra
        ),
        None if rec.status >= 400 => {
            tracing::warn!("{} {} {}ms{}", route, rec.status, rec.duration_ms, extra)
        }
        None => tracing::info!(
            "{} {} {}ms{} tokens={}+{}",
            route,
            rec.status,
            rec.duration_ms,
            extra,
            rec.prompt_tokens,
            rec.completion_tokens
        ),
    }
}

/// 日志里的错误摘要：按字符截断，避免上游长 JSON 撑爆日志行。
fn brief_text(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

pub fn session_id(payload: &Value) -> String {
    // 与 Python 版一致：同一对话（模型 + 首条 user 内容）得到稳定会话 ID。
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let first_user = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        });
    let seed = format!(
        "{}|{}",
        payload.get("model").and_then(Value::as_str).unwrap_or(""),
        first_user
            .and_then(|m| m.get("content"))
            .map(|c| c.to_string())
            .unwrap_or_default()
    );
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn unknown_upstream(state: &AppState, name: &str) -> Response {
    let names: Vec<String> = state
        .cfg()
        .upstreams
        .iter()
        .map(|u| u.name.clone())
        .collect();
    error_json(
        StatusCode::NOT_FOUND,
        format!("未知的上游前缀 \"{name}\"。可用前缀：{}", names.join("、")),
        "invalid_request_error",
        "unknown_upstream",
    )
}

pub(crate) fn error_json(
    status: StatusCode,
    message: impl Into<String>,
    type_: &str,
    code: &str,
) -> Response {
    (
        status,
        Json(json!({"error": {"message": message.into(), "type": type_, "code": code}})),
    )
        .into_response()
}

async fn ui_index() -> impl IntoResponse {
    let version = env!("CARGO_PKG_VERSION");
    let html = include_str!("../ui/index.html")
        .replace(
            "href=\"style.css\"",
            &format!("href=\"style.css?v={version}\""),
        )
        .replace("src=\"app.js\"", &format!("src=\"app.js?v={version}\""))
        .replace(
            "src=\"free_sources.js\"",
            &format!("src=\"free_sources.js?v={version}\""),
        );
    (
        [
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", "no-store"),
        ],
        html,
    )
}

async fn ui_asset(Path(path): Path<String>) -> Response {
    // 只允许固定文件名，天然挡住路径穿越。
    let (content_type, body): (&str, &'static str) = match path.as_str() {
        "app.js" => (
            "application/javascript; charset=utf-8",
            include_str!("../ui/app.js"),
        ),
        "free_sources.js" => (
            "application/javascript; charset=utf-8",
            include_str!("../ui/free_sources.js"),
        ),
        "style.css" => ("text/css; charset=utf-8", include_str!("../ui/style.css")),
        _ => {
            return error_json(
                StatusCode::NOT_FOUND,
                "页面不存在",
                "invalid_request_error",
                "not_found",
            )
        }
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (
                axum::http::header::CACHE_CONTROL,
                "no-store, no-cache, must-revalidate, max-age=0",
            ),
        ],
        body,
    )
        .into_response()
}

async fn api_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(json!({
        "app": "Model-Gate",
        "version": env!("CARGO_PKG_VERSION"),
        "port": state.cfg().port,
        "host": state.cfg().host,
        "upstream_count": state.cfg().upstreams.len(),
        "db_path": state.db.path.display().to_string(),
        "thinking": state.thinking.stats(),
        "server_time": chrono::Utc::now().timestamp(),
        "runtime": "rust",
    }))
}

async fn api_proxy_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

async fn api_config_save(
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

async fn api_config_reload(State(state): State<Arc<AppState>>) -> Response {
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

async fn api_pricing(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.cfg();
    let pricing = cfg.raw.get("pricing").cloned().unwrap_or_else(|| json!({}));
    let retention = cfg
        .raw
        .get("retentionDays")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Json(json!({
        "pricing": pricing,
        "currency": "CNY",
        "configured": false,
        "retention_days": retention,
    }))
    .into_response()
}

async fn api_upstreams(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
                "keySource": u.key_source,
                "keyConfigured": u.key_configured(),
                "apiKeyEnv": u.api_key_env,
                "apiKeyFile": u.api_key_file,
                "apiKeyAgent": u.api_key_agent,
                "plan": Value::Null,
                "planError": Value::Null,
            })
        })
        .collect();
    Json(json!({"upstreams": rows}))
}

async fn api_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.cfg();
    let mut payload = cfg.raw.clone();
    if let Some(ups) = payload.get_mut("upstreams").and_then(Value::as_object_mut) {
        for up in cfg.upstreams.iter() {
            if let Some(target) = ups.get_mut(&up.name).and_then(Value::as_object_mut) {
                target.remove("apiKey");
                target.insert("_keyConfigured".into(), json!(up.key_configured()));
                target.insert("_keySource".into(), json!(up.key_source));
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
async fn api_config_validate(Json(payload): Json<Value>) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 一次性假上游：最多服务 `serves` 个请求后关闭监听（用于模拟“上游下线”）。
    fn spawn_models_server(body: &'static str, serves: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(serves) {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn state_with_upstream(base_url: &str, auto: bool, fetch: bool) -> Arc<AppState> {
        let cfg = crate::config::Config::from_value(json!({
            "modelsCacheSeconds": 300,
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": base_url,
                    "apiKey": "k",
                    "autoModels": auto,
                    "fetchModels": fetch
                }
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-proxy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        Arc::new(AppState::new(cfg, db, dir.join("c.json")))
    }

    #[test]
    fn models_cache_fetches_and_keeps_previous_on_force_failure() {
        let url = spawn_models_server(
            r#"{"data":[{"id":"a"},{"slug":"b"},{"model":"c"},{"nope":1}]}"#,
            1,
        );
        let state = state_with_upstream(&url, true, true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cache = rt.block_on(state.refresh_models_cache(false));
        assert_eq!(cache.ids, vec!["a", "b", "c"]);
        assert_eq!(cache.owner.get("a").map(String::as_str), Some("u"));
        // 上游已下线：强制刷新失败时保留旧缓存（宁可旧，不要空）
        let retained = rt.block_on(state.refresh_models_cache(true));
        assert_eq!(retained.ids, vec!["a", "b", "c"]);
        // TTL 内再次读取直接命中缓存（不再打上游）
        let cached = rt.block_on(state.refresh_models_cache(false));
        assert_eq!(cached.ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn models_cache_skips_when_fetch_or_auto_models_disabled() {
        let url = spawn_models_server(r#"{"data":[{"id":"x"}]}"#, 1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (auto, fetch) in [(true, false), (false, true)] {
            let state = state_with_upstream(&url, auto, fetch);
            let cache = rt.block_on(state.refresh_models_cache(false));
            assert!(cache.ids.is_empty());
        }
    }

    #[test]
    fn brief_text_truncates_by_chars() {
        assert_eq!(brief_text("abc", 5), "abc");
        assert_eq!(brief_text("abcdef", 3), "abc…");
        assert_eq!(brief_text("中文错误", 2), "中文…");
    }

    /// 假上游：返回指定状态码与响应体（用于断言失败原因被保留）。
    fn spawn_status_server(status: &'static str, body: &'static str, serves: usize) -> String {
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

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// 通用清单解析：data / models 数组 + id / slug / model 三种字段名，去重保序。
    #[test]
    fn query_upstream_models_parses_common_shapes_and_dedupes() {
        let url = spawn_models_server(
            r#"{"data":[{"id":"a"},{"slug":"b"},{"model":"c"},{"id":"a"},{"nope":1},"x"]}"#,
            1,
        );
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let models = rt()
            .block_on(state.query_upstream_models(&upstream, 5.0))
            .unwrap();
        assert_eq!(models, vec!["a", "b", "c"]);
    }

    /// 超大清单会被截断到上限（防止响应体与内存无谓膨胀）。
    #[test]
    fn query_upstream_models_truncates_at_max_items() {
        let count = MODEL_QUERY_MAX_ITEMS + 50;
        let items: Vec<String> = (0..count).map(|i| format!("{{\"id\":\"m{i}\"}}")).collect();
        let body = format!(r#"{{"data":[{}]}}"#, items.join(","));
        let url = spawn_models_server(Box::leak(body.into_boxed_str()), 1);
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let models = rt()
            .block_on(state.query_upstream_models(&upstream, 10.0))
            .unwrap();
        assert_eq!(models.len(), MODEL_QUERY_MAX_ITEMS);
        // 截断保留前 N 条（顺序不变），且首条仍是第一条
        assert_eq!(models[0], "m0");
    }

    /// 去重是线性复杂度：清单很大时也不该退化成 O(n²)。
    #[test]
    fn query_upstream_models_dedupes_many_items_quickly() {
        // 全部重复的 id：若去重用 Vec::contains，这里会退化成 O(n²)。
        let count = 20_000;
        let items: Vec<String> = (0..count)
            .map(|_| "{\"id\":\"same\"}".to_string())
            .collect();
        let body = format!(r#"{{"data":[{}]}}"#, items.join(","));
        let url = spawn_models_server(Box::leak(body.into_boxed_str()), 1);
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let started = std::time::Instant::now();
        let models = rt()
            .block_on(state.query_upstream_models(&upstream, 10.0))
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(models, vec!["same"], "重复 id 应只剩一条");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "去重应为线性复杂度，实际耗时 {elapsed:?}"
        );
    }

    /// 上游返回空清单：这是**成功**，不是错误。
    #[test]
    fn query_upstream_models_accepts_empty_catalog() {
        let url = spawn_models_server(r#"{"data":[]}"#, 1);
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let models = rt()
            .block_on(state.query_upstream_models(&upstream, 5.0))
            .unwrap();
        assert!(models.is_empty());
    }

    /// 鉴权失败：保留状态码与修复提示（不静默变成空清单）。
    #[test]
    fn query_upstream_models_preserves_http_error_reason() {
        let url = spawn_status_server("401 Unauthorized", r#"{"error":"unauthorized"}"#, 1);
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let err = rt()
            .block_on(state.query_upstream_models(&upstream, 5.0))
            .expect_err("应返回错误");
        assert_eq!(err.kind, "http");
        assert!(err.message.contains("401"), "message={}", err.message);
        assert!(err.hint.contains("apiKey"), "hint={}", err.hint);
    }

    /// 返回体不是 JSON：保留解析失败原因。
    #[test]
    fn query_upstream_models_preserves_parse_error_reason() {
        let url = spawn_status_server("200 OK", r#"<html>not json</html>"#, 1);
        let state = state_with_upstream(&url, false, true);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        let err = rt()
            .block_on(state.query_upstream_models(&upstream, 5.0))
            .expect_err("应返回错误");
        assert_eq!(err.kind, "parse");
    }

    /// 通用性：baseUrl 归一化后拼出的清单端点必须正确。
    ///
    /// 这是「模型清单获取是通用约定」的核心保证：填带 /v1 或不带 /v1 的
    /// baseUrl，都必须打到同一个 `{baseUrl}/v1/models`。
    #[test]
    fn models_endpoint_url_is_derived_from_normalized_base_url() {
        for (input, expected) in [
            (
                "https://ai-gateway.kilo.ai/api/v1",
                "https://ai-gateway.kilo.ai/api/v1/models",
            ),
            (
                "https://ai-gateway.kilo.ai/api",
                "https://ai-gateway.kilo.ai/api/v1/models",
            ),
            (
                "https://api.kilo.ai/api/openrouter",
                "https://api.kilo.ai/api/openrouter/v1/models",
            ),
            (
                "https://opencode.ai/zen/v1",
                "https://opencode.ai/zen/v1/models",
            ),
        ] {
            let base = crate::config::normalize_base_url(input);
            let url = format!("{}{}", base.trim_end_matches('/'), "/v1/models");
            assert_eq!(url, expected, "input={input}");
        }
    }

    /// fetchModels=false 只影响运行期自动拉取；手动查询不受限（D6）。
    #[test]
    fn query_upstream_models_works_when_fetch_models_disabled() {
        let url = spawn_models_server(r#"{"data":[{"id":"x"}]}"#, 1);
        let state = state_with_upstream(&url, false, false);
        let upstream = state.cfg().upstream("u").cloned().unwrap();
        assert!(!upstream.fetch_models, "前置条件：fetchModels=false");
        let models = rt()
            .block_on(state.query_upstream_models(&upstream, 5.0))
            .unwrap();
        assert_eq!(models, vec!["x"]);
    }

    /// 显式 models 白名单非空时，对外 /v1/models 仍只返回白名单（不受查询影响）。
    #[test]
    fn explicit_model_whitelist_still_wins_over_fetched_catalog() {
        let cfg = crate::config::Config::from_value(json!({
            "models": ["only-whitelisted"],
            "upstreams": {
                "u": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "models": ["declared"]}
            }
        }));
        assert_eq!(cfg.model_list(), vec!["only-whitelisted"]);
    }

    /// 「上一等长区间」的口径：预设区间向前平移一整段。
    ///
    /// 背景：面板 KPI 的环比箭头依赖 /api/usage/totals?compare=1 返回 previous 基线；
    /// Rust 侧此前未实现 compare（Python 版有），导致环比标记永远不显示。
    #[test]
    fn previous_window_shifts_one_full_span_back() {
        let now = 1_700_000_000i64;
        // 24h：上一段是 [now-2d, now-1d)
        assert_eq!(
            previous_window("range=24h", now),
            Some((now - 2 * 86400, now - 86400))
        );
        assert_eq!(
            previous_window("range=1h", now),
            Some((now - 2 * 3600, now - 3600))
        );
        assert_eq!(
            previous_window("range=7d", now),
            Some((now - 14 * 86400, now - 7 * 86400))
        );
        assert_eq!(
            previous_window("range=30d", now),
            Some((now - 60 * 86400, now - 30 * 86400))
        );
        // all：没有可比区间 → 跳过对比
        assert_eq!(previous_window("range=all", now), None);
    }

    /// 自定义区间：按实际跨度向前平移（不是按固定 24h）。
    #[test]
    fn previous_window_uses_actual_span_for_custom_range() {
        let now = 1_700_000_000i64;
        // [now-3600, now)：跨度 1h → 上一段 [now-7200, now-3600)
        assert_eq!(
            previous_window(&format!("from={}&to={}", now - 3600, now), now),
            Some((now - 7200, now - 3600))
        );
        // 只给 from：右边界取 now
        assert_eq!(
            previous_window(&format!("from={}", now - 600), now),
            Some((now - 1200, now - 600))
        );
        // 起始晚于结束 → 无效区间，不对比
        assert_eq!(
            previous_window(&format!("from={}&to={}", now + 10, now), now),
            None
        );
    }

    /// today：对比「今天 00:00 之前等长的上一段」（与 24h 口径不同）。
    #[test]
    fn previous_window_for_today_uses_midnight_boundary() {
        let now = chrono::Utc::now().timestamp();
        let (since, until) = previous_window("range=today", now).expect("today 应有可比区间");
        assert_eq!(until, today_start(), "右边界是今天 00:00");
        assert_eq!(since, until - (now - until), "左边界是等长的一段");
        assert!(since < until);
    }
}

fn key_with_prefix(prefix: &str, key: &str) -> String {
    let effective = if prefix.is_empty() { "Bearer " } else { prefix };
    format!("{effective}{key}")
}

fn insert_header(headers: &mut reqwest::header::HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        name.parse::<reqwest::header::HeaderName>(),
        value.parse::<reqwest::header::HeaderValue>(),
    ) {
        headers.insert(name, value);
    }
}

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
    filters_from_query(uri.query().unwrap_or(""), chrono::Utc::now().timestamp())
}

async fn api_usage_totals(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let f = scoped_filters(&uri);
    let mut payload = json!({"totals": state.db.totals(&f)});
    // compare=1：与「上一等长区间」对比，前端据此展示 KPI 环比箭头。
    // 口径与 Python 版一致（range=all 无可比区间时跳过）。
    if query_map(&uri).get("compare").is_some() {
        let now = chrono::Utc::now().timestamp();
        if let Some((prev_since, prev_until)) = previous_window(uri.query().unwrap_or(""), now) {
            let mut prev = f.clone();
            prev.since_ts = Some(prev_since);
            prev.until_ts = Some(prev_until);
            payload["previous"] = state.db.totals(&prev);
        }
    }
    Json(payload).into_response()
}

async fn api_usage_summary(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let group = params.get("group_by").map(String::as_str).unwrap_or("day");
    let f = scoped_filters(&uri);
    match state.db.summary(&f, group) {
        Ok(rows) => Json(json!({"group_by": group, "rows": rows})).into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response(),
    }
}

async fn api_usage_list(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
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
    let rows = state.db.list_usage(&f, limit, offset, &sort, &order);
    let total = state.db.count_usage(&f);
    Json(json!({
        "rows": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
        "sort": sort,
        "order": order,
        "only": f.only.unwrap_or_default(),
    }))
    .into_response()
}

async fn api_usage_models(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let f = scoped_filters(&uri);
    let rows = state.db.model_usage(&f, limit);
    Json(json!({
        "rows": rows,
        "range": params.get("range").cloned().unwrap_or_else(|| "24h".into()),
        "filters": {},
        "currency": "CNY",
        "pricing_configured": false,
    }))
    .into_response()
}

async fn api_usage_upstreams(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let params = query_map(&uri);
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 500);
    let f = scoped_filters(&uri);
    let rows = state.db.upstream_usage(&f, limit);
    Json(json!({
        "rows": rows,
        "models": [],
        "range": params.get("range").cloned().unwrap_or_else(|| "24h".into()),
        "filters": {},
    }))
    .into_response()
}

async fn not_found() -> Response {
    error_json(
        StatusCode::NOT_FOUND,
        "not implemented",
        "invalid_request_error",
        "not_implemented",
    )
}

/// 面板用量接口的公共解析：range/from/to/upstream/model/session。
pub fn filters_from_query(query: &str, now: i64) -> Filters {
    let mut f = Filters::default();
    let mut range = "24h".to_string();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_encoding::percent_decode_str(k)
            .decode_utf8_lossy()
            .to_string();
        let plus = v.replace('+', " ");
        let value = percent_encoding::percent_decode_str(&plus)
            .decode_utf8_lossy()
            .to_string();
        match key.as_str() {
            "range" => range = value,
            "from" => f.since_ts = parse_ts(&value),
            "to" => f.until_ts = parse_ts(&value),
            "upstream" => f.upstream = Some(value),
            "model" => f.model = Some(value),
            "session" => f.session = Some(value),
            _ => {}
        }
    }
    if f.since_ts.is_none() {
        f.since_ts = match range.as_str() {
            "all" => None,
            "today" => Some(today_start()),
            "1h" => Some(now - 3600),
            "7d" => Some(now - 7 * 86400),
            "30d" => Some(now - 30 * 86400),
            _ => Some(now - 86400),
        };
    }
    f
}

/// 上一等长区间的 (since_ts, until_ts)，用于「与上一区间对比」。
///
/// 口径与 Python 版一致：
/// - `range=all` 没有可比区间 → 返回 None（调用方跳过对比）；
/// - 自定义区间按 `[from, to]` 的实际跨度向前平移；
/// - `today` 对比「今天 00:00 之前等长的上一段」（避免与 24h 混淆）。
pub fn previous_window(query: &str, now: i64) -> Option<(i64, i64)> {
    let params = query_map_from(query);
    if params.contains_key("from") || params.contains_key("to") {
        let since = params.get("from").and_then(|v| parse_ts(v))?;
        let until = params.get("to").and_then(|v| parse_ts(v)).unwrap_or(now);
        if since >= until {
            return None;
        }
        let span = (until - since).max(1);
        return Some((since - span, since));
    }
    let range = params.get("range").map(String::as_str).unwrap_or("24h");
    if range == "today" {
        let start = today_start();
        let span = (now - start).max(1);
        return Some((start - span, start));
    }
    let seconds = match range {
        "1h" => 3600,
        "24h" => 86400,
        "7d" => 7 * 86400,
        "30d" => 30 * 86400,
        _ => return None,
    };
    let end = now - seconds;
    Some((end - seconds, end))
}

/// 解析裸查询串为键值对（与 query_map 同语义，但不依赖 Uri）。
fn query_map_from(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
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
    out
}

pub fn parse_ts(raw: &str) -> Option<i64> {
    if let Ok(n) = raw.parse::<i64>() {
        return Some(n);
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M")
        .ok()
        .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
        .map(|dt| dt.timestamp())
}

pub fn today_start() -> i64 {
    let now = chrono::Local::now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}
