//! HTTP 代理内核（迁移中：先落地 OpenAI 上游，Anthropic/CommandCode 在 P2 补齐）。
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::TimeZone;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::anthropic;
use crate::commandcode;
use crate::config::{Config, Privacy, PrivacyAdapter, TrainingPolicy, Upstream};
use crate::db::{Db, Filters, PromptRecord, UsageRecord};
use crate::opencode_transport as opencode;
use crate::prompt;

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

/// 提示词后台队列容量。队列满时直接丢弃本次记录，绝不反压代理请求。
const PROMPT_QUEUE_CAPACITY: usize = 256;
/// 单批最多聚合的记录数。
const PROMPT_BATCH_MAX: usize = 32;
/// 单批等待窗口：先到的一条最多等这么久，把并发请求聚成一批。
const PROMPT_BATCH_WAIT_MS: u64 = 50;
/// 保留清理按写入条数/时间触发，避免每批都执行 DELETE。
const PROMPT_CLEANUP_EVERY: u64 = 64;
const PROMPT_CLEANUP_INTERVAL_SECS: i64 = 300;
/// 用量后台队列容量。队列满时调用方同步直写兜底，绝不丢记录。
const USAGE_QUEUE_CAPACITY: usize = 4096;
/// 单批最多聚合的记录数。
const USAGE_BATCH_MAX: usize = 64;
/// 单批等待窗口：把并发请求的用量聚成一次事务提交。
const USAGE_BATCH_WAIT_MS: u64 = 50;

pub struct AppState {
    pub cfg: ArcSwap<Config>,
    pub db: Arc<Db>,
    /// 运行期模块开关：路由挂载、后台线程、请求路径记录都以它为准。
    ///
    /// **启动冻结**：`replace_config`（面板保存 / `/api/config/reload`）只换
    /// `cfg`，不改这里——路由表与后台线程是启动时装配的，模块增减必须
    /// 重启才生效，避免「路由没挂载但开始记录」的混合态。
    pub modules: crate::config::Modules,
    /// 提示词后台队列发送端；请求路径只做 try_send，不做等待。
    /// stats 模块关闭时为 None：writer 线程不启动，提示词一律不记录。
    pub prompt_tx: Option<std::sync::mpsc::SyncSender<PromptJob>>,
    /// 因后台队列已满而丢弃的提示词记录数（用于诊断，不影响代理）。
    pub prompt_dropped: AtomicU64,
    /// 用量后台队列发送端；请求路径只做 try_send，队列满/断开时同步直写兜底。
    /// stats 模块关闭时为 None：writer 线程不启动，用量一律不记录。
    pub usage_tx: Option<std::sync::mpsc::SyncSender<UsageRecord>>,
    /// 已投递但尚未写库的用量记录数；退出前等待归零，升级替换进程不丢最后一批。
    pub usage_pending: Arc<AtomicU64>,
    /// 今日指标内存计数器：托盘与轻量展示读这里，不再周期性查库。
    pub today: Arc<crate::today::TodayCounters>,
    /// 凭据池快照缓存（TTL + 写失效）：选择凭据的热路径不再每次打库。
    pub pool_cache: std::sync::Mutex<crate::credential_pool::PoolCache>,
    /// 密钥库解密缓存（按文件 mtime 失效）：池路径不再每成员读盘 + DPAPI 解密。
    pub secrets_cache: std::sync::Mutex<crate::credential_pool::SecretsCache>,
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
    /// 路由候选的短期冷却：key = `upstream\0model`。
    ///
    /// 只影响候选顺序：冷却中的候选排到链尾，仍保留兜底尝试；不改变
    /// `/上游/v1` 的强制路由，也不改变日志里的真实上游归属。
    pub route_health: std::sync::Mutex<HashMap<String, Instant>>,
    /// 服务线程的 Tokio 句柄；托盘「刷新模型目录」用它触发异步刷新。
    pub runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    /// 关闭协调器：更新流程用它触发「停服务 → 落盘 → 退出」。
    pub shutdown: std::sync::Arc<crate::lifecycle::Shutdown>,
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
            // 连接池显式收敛：reqwest 默认每主机空闲连接数不设上限，
            // 高并发突发后会留下一堆半关闭 socket；网关上游就那几个，8 个足够。
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(8)
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
        // 关闭协调器：HTTP 服务、托盘、更新流程共用同一个「请退出」信号。
        let shutdown = crate::lifecycle::Shutdown::new();
        let update = Arc::new(crate::update::UpdateService::new(
            env!("CARGO_PKG_VERSION").parse().expect("crate version"),
            db.clone(),
            install_dir,
            shutdown.clone(),
        ));
        let modules = cfg.modules.clone();
        // 今日指标计数器：托盘读内存快照，不再每 10 秒查库聚合。
        let today = Arc::new(crate::today::TodayCounters::default());
        // 跨重启不丢当日累计：stats 开启时用库里「今日」聚合预热一次。
        // 旧实现靠每 10 秒查库才能做到这一点；现在只在启动查一次，
        // 之后由请求路径增量维护。精简形态（stats 关）没有记录，无需预热。
        if modules.stats {
            let stats = db.today_stats();
            today.seed(
                stats.get("requests").and_then(Value::as_i64).unwrap_or(0),
                stats.get("ok").and_then(Value::as_i64).unwrap_or(0),
                stats
                    .get("prompt_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                stats
                    .get("cache_read_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            );
        }
        // 提示词子系统仅 stats 模块开启时活动：启动清理 + 后台 worker。
        // stats 关闭即提示词记录整体停摆（历史数据留在旧库文件里不动）。
        let prompt_tx = if modules.stats {
            // 启动即执行一次提示词保留策略：即使本次未开启记录，也清理上次遗留的超期数据。
            db.delete_prompts_older_than(cfg.prompt_retention_days as i64);
            db.trim_prompts(cfg.prompt_max_records as i64);
            let (prompt_tx, prompt_rx) = sync_channel(PROMPT_QUEUE_CAPACITY);
            let prompt_db = db.clone();
            if let Err(err) = std::thread::Builder::new()
                .name("prompt-writer".into())
                .spawn(move || prompt_writer_loop(prompt_db, prompt_rx))
            {
                tracing::warn!("提示词后台线程启动失败（记录将不可用）：{err}");
            }
            Some(prompt_tx)
        } else {
            None
        };
        // 用量子系统仅 stats 模块开启时活动：请求路径只投递，批量落库在这里完成；
        // 队列满/线程不可用时调用方同步直写兜底，绝不丢记录。
        let (usage_tx, usage_pending) = if modules.stats {
            let (usage_tx, usage_rx) = sync_channel(USAGE_QUEUE_CAPACITY);
            let usage_db = db.clone();
            let usage_pending = Arc::new(AtomicU64::new(0));
            let usage_pending_worker = usage_pending.clone();
            if let Err(err) = std::thread::Builder::new()
                .name("usage-writer".into())
                .spawn(move || usage_writer_loop(usage_db, usage_rx, usage_pending_worker))
            {
                tracing::warn!("用量后台线程启动失败（将同步直写）：{err}");
            }
            (Some(usage_tx), usage_pending)
        } else {
            (None, Arc::new(AtomicU64::new(0)))
        };
        Self {
            cfg: ArcSwap::from_pointee(cfg),
            db,
            modules,
            prompt_tx,
            prompt_dropped: AtomicU64::new(0),
            usage_tx,
            usage_pending,
            today,
            pool_cache: std::sync::Mutex::new(crate::credential_pool::PoolCache::default()),
            secrets_cache: std::sync::Mutex::new(crate::credential_pool::SecretsCache::default()),
            client,
            started: Instant::now(),
            config_path,
            thinking: crate::thinking::ThinkingCache::new(thinking_enabled, thinking_size),
            models_cache: tokio::sync::RwLock::new(ModelsCache::default()),
            update,
            free_probe_cache: std::sync::Mutex::new(crate::free_sources::ProbeCache::default()),
            route_health: std::sync::Mutex::new(HashMap::new()),
            runtime: std::sync::OnceLock::new(),
            shutdown,
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
        // 模型清单没有具体模型上下文：使用上游级密钥。
        // 与 chat 共用同一套鉴权头构造，避免两处行为漂移。
        let selection = up.upstream_key();
        apply_auth_headers(&mut headers, up, &selection.key);
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

    /// 用库里的「今日」聚合重新预热托盘计数器（阻塞查询：调用方须已离开
    /// async 上下文，如 `run_blocking` 闭包或启动路径）。
    ///
    /// 用途：清理用量后托盘数字与库对齐——`all` 清空归零，区间清理按剩余量累计。
    pub fn reseed_today(&self) {
        let stats = self.db.today_stats();
        self.today.seed(
            stats.get("requests").and_then(Value::as_i64).unwrap_or(0),
            stats.get("ok").and_then(Value::as_i64).unwrap_or(0),
            stats
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            stats
                .get("cache_read_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        );
    }
}

/// 统一接入口的作用域：默认全部可见上游，或指定分组。
#[derive(Debug, Clone)]
enum UnifiedScope {
    All,
    Group(String),
}

impl UnifiedScope {
    fn group_id(&self) -> Option<&str> {
        match self {
            Self::Group(id) => Some(id.as_str()),
            Self::All => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::All => "默认范围".to_string(),
            Self::Group(id) => format!("分组 \"{id}\""),
        }
    }
}

/// 非 chat 路由（面板 `/api/*` 等）缓冲式提取器的请求体上限：64 MiB。
///
/// axum 对 `Json` / `Bytes` 这类提取器的默认上限只有 2 MiB，面板保存稍大的配置、
/// 提交较长的定价表时会被 413 挡住。这一层只能在建路由时固定，所以是常量；
/// chat 路由不受它约束——`ChatBody` 按运行态的 `maxBodyBytes` 每请求限流。
const BUFFERED_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

pub fn router(state: Arc<AppState>) -> axum::Router {
    let modules = state.modules.clone();
    // 核心转发恒挂载：网关的最小事实，任何模块组合下都必须可用。
    let mut app = core_routes();
    // 可选子系统按 `modules` 开关挂载：关闭的模块不注册路由，
    // 面板/管理 API 一律 404，对应的后台线程与 IO 也一并缺席。
    if modules.panel {
        app = app.merge(panel_routes());
    }
    if modules.stats {
        app = app.merge(usage_routes());
    }
    if modules.plugins {
        app = app.merge(plugin_routes());
    }
    if modules.pool {
        app = app.merge(pool_routes());
    }
    if modules.access {
        app = app.merge(access_routes());
    }
    if modules.ai_config {
        app = app.merge(ai_routes());
    }
    if modules.update {
        app = app.merge(update_routes());
    }
    // 管理接口（面板）走 `Json` 提取器，抬到与 chat 同量级的入站上限。
    app.layer(DefaultBodyLimit::max(BUFFERED_BODY_LIMIT_BYTES))
        .with_state(state)
}

/// 核心转发路由：`/health`、模型目录与 chat 转发（含 unified/强制上游变体）。
fn core_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/unified/v1/models", get(models_unified))
        .route("/unified/{group}/v1/models", get(models_unified_group))
        .route("/{upstream}/v1/models", get(models_forced))
        // chat 路由的请求体由 `ChatBody` 提取器按 `maxBodyBytes` 自己限流，
        // 不用 `DefaultBodyLimit`：框架默认的 2 MiB 对多模态请求太小，且超限时
        // 只会返回纯文本 413、不留任何日志。
        .route("/v1/chat/completions", post(chat))
        .route("/unified/v1/chat/completions", post(chat_unified))
        .route(
            "/unified/{group}/v1/chat/completions",
            post(chat_unified_group),
        )
        .route("/{upstream}/v1/chat/completions", post(chat_forced))
        .fallback(not_found)
}

/// Web 面板与配置管理 API（`panel` 模块）。
fn panel_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ui/", get(crate::panel_api::ui_index))
        .route("/ui/classic", get(crate::panel_api::ui_classic))
        .route("/ui/{*path}", get(crate::panel_api::ui_asset))
        .route("/api/info", get(crate::panel_api::api_info))
        .route("/api/proxy/status", get(crate::panel_api::api_proxy_status))
        .route("/api/upstreams", get(crate::panel_api::api_upstreams))
        .route(
            "/api/config",
            get(crate::panel_api::api_config).post(crate::panel_api::api_config_save),
        )
        .route(
            "/api/config/validate",
            post(crate::panel_api::api_config_validate),
        )
        .route(
            "/api/config/preview",
            post(crate::panel_api::api_config_preview),
        )
        .route(
            "/api/config/reload",
            axum::routing::post(crate::panel_api::api_config_reload),
        )
        .route("/api/config/backups", get(crate::api_extra::backups_list))
        .route(
            "/api/config/backups/restore",
            post(crate::api_extra::backups_restore),
        )
        .route(
            "/api/pricing",
            get(crate::panel_api::api_pricing).post(crate::api_extra::pricing_save),
        )
        .route("/api/models/catalog", get(crate::api_extra::models_catalog))
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
}

/// 用量统计与提示词记录 API（`stats` 模块）。
fn usage_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/usage/clear", post(crate::api_extra::usage_clear))
        .route(
            "/api/usage/retention/apply",
            post(crate::api_extra::retention_apply),
        )
        .route("/api/usage/export", get(crate::api_extra::usage_export))
        .route("/api/usage/sessions", get(crate::api_extra::usage_sessions))
        .route("/api/usage/totals", get(crate::panel_api::api_usage_totals))
        .route(
            "/api/usage/summary",
            get(crate::panel_api::api_usage_summary),
        )
        .route("/api/usage/list", get(crate::panel_api::api_usage_list))
        .route("/api/usage/models", get(crate::panel_api::api_usage_models))
        .route(
            "/api/usage/upstreams",
            get(crate::panel_api::api_usage_upstreams),
        )
        .route("/api/prompts", get(crate::api_extra::prompts_list))
        .route("/api/prompts/stats", get(crate::api_extra::prompts_stats))
        .route("/api/prompts/export", get(crate::api_extra::prompts_export))
        .route(
            "/api/prompts/evolution",
            get(crate::api_extra::prompts_evolution),
        )
        .route("/api/prompts/diff", get(crate::api_extra::prompts_diff))
        .route(
            "/api/prompts/diff/download",
            get(crate::api_extra::prompts_diff_download),
        )
        .route("/api/prompts/clear", post(crate::api_extra::prompts_clear))
        .route(
            "/api/prompts/vacuum",
            post(crate::api_extra::prompts_vacuum),
        )
        .route(
            "/api/prompts/{id}/download",
            get(crate::api_extra::prompt_download),
        )
        .route("/api/prompts/{id}", get(crate::api_extra::prompt_detail))
}

/// 插件与免费来源 API（`plugins` 模块）。
fn plugin_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // 插件：登记、导入、启停、移除/重装、幂等接入/解绑。
        .route("/api/plugins", get(crate::plugin::list))
        .route("/api/plugins/install", post(crate::plugin::install))
        .route("/api/plugins/import", post(crate::plugin::import))
        .route(
            "/api/plugins/{id}",
            get(crate::plugin::detail)
                .patch(crate::plugin::set_enabled)
                .delete(crate::plugin::uninstall),
        )
        .route("/api/plugins/{id}/restore", post(crate::plugin::restore))
        .route("/api/plugins/{id}/attach", post(crate::plugin::attach))
        .route("/api/plugins/{id}/detach", post(crate::plugin::detach))
        // 插件页「模型 · 检测」：freeSource 指向注册表条目，目录零生成、探测显式触发。
        .route(
            "/api/plugins/{id}/models",
            post(crate::free_sources_api::plugin_models),
        )
        .route(
            "/api/plugins/{id}/probe",
            post(crate::free_sources_api::plugin_probe),
        )
        .route(
            "/api/plugins/{id}/probe-all",
            post(crate::free_sources_api::plugin_probe_all),
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
}

/// 凭据池 API（`pool` 模块）。
fn pool_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        // 凭据池：池与成员的脱敏 CRUD、探测。
        .route(
            "/api/pools",
            get(crate::credential_pool::list).post(crate::credential_pool::create),
        )
        .route(
            "/api/pools/{id}",
            get(crate::credential_pool::detail)
                .patch(crate::credential_pool::update)
                .delete(crate::credential_pool::delete),
        )
        .route(
            "/api/pools/{id}/members",
            post(crate::credential_pool::add_member_handler),
        )
        .route(
            "/api/pools/{id}/members/{mid}",
            axum::routing::patch(crate::credential_pool::update_member)
                .delete(crate::credential_pool::delete_member),
        )
        .route("/api/pools/{id}/probe", post(crate::credential_pool::probe))
}

/// 接入编排 API（`access` 模块）：模板、读模型、快速添加、受控更新与密钥写入。
fn access_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/api/access",
            get(crate::access::list).post(crate::access::quick_add),
        )
        .route("/api/access/templates", get(crate::access::list_templates))
        .route("/api/access/quick", post(crate::access::quick_add))
        .route("/api/access/custom", post(crate::access::custom_add))
        .route(
            "/api/access/{id}",
            get(crate::access::detail)
                .patch(crate::access::update)
                .delete(crate::access::delete),
        )
        .route("/api/access/{id}/secret", post(crate::access::write_secret))
        .route("/api/access/{id}/probe", post(crate::access::probe))
}

/// AI 可读配置 API（`aiConfig` 模块）：只读 + 草案 + 人工确认应用。
fn ai_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/llms.txt", get(crate::ai_api::llms_txt))
        .route("/api/ai/manifest", get(crate::ai_api::manifest))
        .route("/api/ai/schema", get(crate::ai_api::schema))
        .route("/api/ai/templates", get(crate::ai_api::templates))
        .route("/api/ai/inspect", get(crate::ai_api::inspect))
        .route("/api/ai/plan", post(crate::ai_api::plan))
        .route("/api/ai/apply", post(crate::ai_api::apply))
        .route("/api/ai/audit", get(crate::ai_api::audit))
        .route(
            "/api/ai/settings",
            get(crate::ai_api::get_settings).put(crate::ai_api::put_settings),
        )
}

/// 自更新 API（`update` 模块）。
fn update_routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/update/status", get(crate::update::api_update_status))
        .route("/api/update/check", get(crate::update::api_update_check))
        .route(
            "/api/update/download",
            post(crate::update::api_update_download),
        )
        .route(
            "/api/update/download-install",
            post(crate::update::api_update_download_install),
        )
        .route(
            "/api/update/install",
            post(crate::update::api_update_install),
        )
        .route(
            "/api/update/settings",
            post(crate::update::api_update_settings),
        )
        .route(
            "/api/update/open-location",
            post(crate::update::api_update_open_location),
        )
        .route(
            "/api/update/download-dir",
            post(crate::update::api_update_download_dir),
        )
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
                .or_else(|| cfg.group_id_from_model(id).map(|_| "group".to_string()))
                .unwrap_or_else(|| "model-gate".to_string());
            json!({"id": id, "object": "model", "created": 0, "owned_by": owner})
        })
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

/// 统一接入口的模型清单：`上游/模型` 作为模型参数，客户端只需配置一个 Base URL。
///
/// 与 `/上游/v1/models` 同口径：宣告各上游可路由的模型（声明清单 + `modelMap`
/// 别名），不受顶层 `models` 白名单影响，也不触发上游目录拉取。
async fn models_unified(State(state): State<Arc<AppState>>) -> Response {
    models_unified_inner(&state, UnifiedScope::All)
}

async fn models_unified_group(
    State(state): State<Arc<AppState>>,
    Path(group): Path<String>,
) -> Response {
    models_unified_inner(&state, UnifiedScope::Group(group))
}

fn models_unified_inner(state: &AppState, scope: UnifiedScope) -> Response {
    let cfg = state.cfg();
    let upstreams = match unified_upstreams_for(&cfg, &scope) {
        Some(upstreams) => upstreams,
        None => return unknown_unified_group(state, scope.group_id().unwrap_or_default()),
    };
    let mut data: Vec<Value> = cfg
        .group_model_list_for(&upstreams)
        .into_iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "group"}))
        .collect();
    data.extend(
        cfg.qualified_model_list_for(&upstreams).into_iter().map(
            |(id, owner)| json!({"id": id, "object": "model", "created": 0, "owned_by": owner}),
        ),
    );
    Json(json!({"object": "list", "data": data})).into_response()
}

/// 把统一作用域解析成可展示的上游；分组不存在返回 `None`。
fn unified_upstreams_for<'a>(cfg: &'a Config, scope: &UnifiedScope) -> Option<Vec<&'a Upstream>> {
    match scope {
        UnifiedScope::All => Some(cfg.unified_visible_upstreams()),
        UnifiedScope::Group(id) => cfg.unified_group_upstreams(id),
    }
}

fn unknown_unified_group(state: &AppState, group: &str) -> Response {
    let cfg = state.cfg();
    let groups: Vec<String> = cfg
        .unified
        .groups
        .iter()
        .map(|item| item.id.clone())
        .filter(|id| !id.is_empty())
        .collect();
    let message = if groups.is_empty() {
        format!("未知的统一接口分组 \"{group}\"；当前没有配置任何分组")
    } else {
        format!(
            "未知的统一接口分组 \"{group}\"。可用分组：{}",
            groups.join("、")
        )
    };
    error_json(
        StatusCode::NOT_FOUND,
        message,
        "invalid_request_error",
        "unknown_unified_group",
    )
}

fn unified_upstream_denied(scope: &UnifiedScope, upstream: &str) -> Response {
    error_json(
        StatusCode::FORBIDDEN,
        format!(
            "上游 \"{upstream}\" 不在统一接口{}的可见范围内",
            match scope {
                UnifiedScope::All => "默认范围".to_string(),
                UnifiedScope::Group(id) => format!("分组 \"{id}\""),
            }
        ),
        "invalid_request_error",
        "upstream_not_allowed",
    )
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

/// chat 请求体：带「实际大小 vs 上限」诊断的提取器。
///
/// 为什么不用 `Bytes` + `DefaultBodyLimit`：axum 对缓冲式提取器的默认上限只有
/// 2 MiB，超限时框架直接返回纯文本 413（`Failed to buffer the request body:
/// length limit exceeded`）——没有结构化错误码，也不写任何日志，客户端只看到
/// 一个不明所以的 413。这里自己按 `maxBodyBytes` 限流：超限时写一条 warn 日志，
/// 并回 OpenAI 形状的 JSON 错误，把「多大、上限多少、改哪个配置」讲清楚。
pub struct ChatBody(pub Bytes);

impl FromRequest<Arc<AppState>> for ChatBody {
    type Rejection = Response;

    async fn from_request(req: Request, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let limit = state.cfg().max_body_bytes;
        let method = req.method().as_str().to_string();
        let path = req.uri().path().to_string();
        let user_agent = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|v| brief_text(v, 80));
        // 声明的长度能提前判断就先拒，不把注定会被拒的正文读进内存。
        let declared = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(size) = declared.filter(|size| *size > limit) {
            return Err(reject_body_too_large(
                &method,
                &path,
                user_agent.as_deref(),
                size,
                limit,
            ));
        }
        // 没有 Content-Length（chunked）时只能边读边计数；这里同样按上限收口。
        let mut stream = req.into_body().into_data_stream();
        // 只按声明长度做有限预分配：声明很大、实际发得很小的正文不该预先吃满内存。
        let mut buf = Vec::with_capacity(declared.unwrap_or(0).min(256 * 1024) as usize);
        let mut total: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    tracing::warn!(
                        "{} {} 读取请求体失败（已读 {}）：{err}",
                        method,
                        path,
                        fmt_bytes(total)
                    );
                    return Err(error_json(
                        StatusCode::BAD_REQUEST,
                        format!("读取请求体失败：{err}"),
                        "invalid_request_error",
                        "body_read_failed",
                    ));
                }
            };
            total += chunk.len() as u64;
            if total > limit {
                return Err(reject_body_too_large(
                    &method,
                    &path,
                    user_agent.as_deref(),
                    total,
                    limit,
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(Self(Bytes::from(buf)))
    }
}

/// 请求体超限：写 warn 日志并返回 OpenAI 形状的 413。
///
/// 日志与响应都要给出「谁、多大、上限多少」，否则用户只能看到一句
/// `length limit exceeded`，既不知道是哪个请求，也不知道该改哪里。
fn reject_body_too_large(
    method: &str,
    path: &str,
    user_agent: Option<&str>,
    size: u64,
    limit: u64,
) -> Response {
    let client = match user_agent {
        Some(ua) => format!("，客户端 UA={ua}"),
        None => String::new(),
    };
    tracing::warn!(
        "{} {} 请求体超限 413：收到 {}{}，上限 {}（配置项 maxBodyBytes）；已拒绝，未转发上游",
        method,
        path,
        fmt_bytes(size),
        client,
        fmt_bytes(limit)
    );
    error_json(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!(
            "请求体 {} 超过单次请求上限 {}（配置项 maxBodyBytes）。请减小请求体，或在 bridge.config.json 里调大 maxBodyBytes 后保存配置（即时生效）。",
            fmt_bytes(size),
            fmt_bytes(limit)
        ),
        "invalid_request_error",
        "request_too_large",
    )
}

/// 字节数的人类可读写法：日志与 413 提示共用，避免只看到一串裸数字。
fn fmt_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

async fn chat(State(state): State<Arc<AppState>>, headers: HeaderMap, body: ChatBody) -> Response {
    chat_inner(state, None, headers, body.0).await
}

async fn chat_forced(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: ChatBody,
) -> Response {
    chat_inner(state, Some(name), headers, body.0).await
}

async fn chat_unified(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: ChatBody,
) -> Response {
    chat_scoped(state, None, Some(UnifiedScope::All), headers, body.0).await
}

async fn chat_unified_group(
    State(state): State<Arc<AppState>>,
    Path(group): Path<String>,
    headers: HeaderMap,
    body: ChatBody,
) -> Response {
    chat_scoped(
        state,
        None,
        Some(UnifiedScope::Group(group)),
        headers,
        body.0,
    )
    .await
}

async fn chat_inner(
    state: Arc<AppState>,
    forced: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    chat_scoped(state, forced, None, headers, body).await
}

async fn chat_scoped(
    state: Arc<AppState>,
    forced: Option<String>,
    scope: Option<UnifiedScope>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let payload: Value = match serde_json::from_slice(&body) {
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
    let ctx = prompt::RequestCtx {
        id: uuid::Uuid::new_v4().simple().to_string(),
        session: resolve_session(&headers, &payload),
    };
    // 门控留在克隆之前：capture_prompt 内部虽然还会再查一次开关，但
    // body.clone() 在这里是实打实的整份拷贝（多模态请求可达数十 MiB），
    // 提示词记录关闭时白克隆一次是纯浪费。stats 模块关闭时同样不记录。
    if state.modules.stats && state.cfg().prompt_capture {
        capture_prompt(&state, body.clone(), &ctx, &requested_model);
    }
    if requested_model.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "缺少 model 字段且未配置 defaultModel",
            "invalid_request_error",
            "model_required",
        );
    }
    let cfg_guard = state.cfg();
    // 统一接入口：先解析作用域，再解析模型名。
    let scope_allowed: Option<Vec<String>> = match &scope {
        None => None,
        Some(scope) => match unified_upstreams_for(&cfg_guard, scope) {
            Some(upstreams) => Some(upstreams.into_iter().map(|up| up.name.clone()).collect()),
            None => return unknown_unified_group(&state, scope.group_id().unwrap_or_default()),
        },
    };
    if let (Some(scope), Some(allowed)) = (&scope, &scope_allowed) {
        if allowed.is_empty() {
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("统一接口{}没有可用上游", scope.describe()),
                "upstream_error",
                "no_unified_upstream",
            );
        }
    }
    // URL 前缀强制指定优先；否则识别统一模型名 `上游/模型`。
    // `group/<id>` 是保留前缀：先于 `上游/模型` 解析，避免被同名上游截走。
    let (forced, route_model) = match forced {
        Some(name) => (Some(name), requested_model.clone()),
        None => {
            let effective = cfg_guard
                .aliases
                .get(&requested_model)
                .map(String::as_str)
                .unwrap_or(&requested_model);
            if effective.starts_with("group/") {
                (None, requested_model.clone())
            } else {
                match cfg_guard.split_qualified_model(&requested_model) {
                    Some((up, model)) => (Some(up.name.clone()), model.to_string()),
                    None => (None, requested_model.clone()),
                }
            }
        }
    };
    if let (Some(scope), Some(allowed), Some(name)) = (&scope, &scope_allowed, forced.as_deref()) {
        if !allowed.iter().any(|item| item == name) {
            return unified_upstream_denied(scope, name);
        }
    }

    // 解析候选链：强制上游 = 单条；group/<id> = 成员顺序；普通模型 = 主选 +
    // 其它声明了同一模型的上游（最多 3 条，避免放大上游压力）。
    let plan = match build_route_plan(
        &state,
        &cfg_guard,
        &requested_model,
        &route_model,
        forced.as_deref(),
        scope_allowed.as_deref(),
        scope.as_ref(),
    ) {
        Ok(plan) => plan,
        Err(response) => return response,
    };
    let max_tokens = cfg_guard.max_tokens;
    drop(cfg_guard);

    let group_header = plan.group.clone();
    let mut attempts: Vec<String> = Vec::new();
    let mut last_failure: Option<RouteFailure> = None;

    for (index, target) in plan.targets.iter().enumerate() {
        let has_more = index + 1 < plan.targets.len();
        let upstream = &target.upstream;
        // 真实发给上游的模型名在 prepare 阶段解析（modelMap）。
        let target_model = upstream
            .model_map
            .get(&target.model)
            .cloned()
            .unwrap_or_else(|| target.model.clone());
        let attempt_label = |status: u16| match &target.credential {
            Some(credential) => format!("{}/{}={status}", upstream.name, credential.label),
            None => format!("{}={status}", upstream.name),
        };

        let prepared = match prepare_attempt(
            &state,
            upstream,
            &target.model,
            &payload,
            max_tokens,
            target.credential.as_ref(),
        ) {
            Ok(prepared) => prepared,
            Err(response) => {
                let status = response.status();
                if has_more && is_retryable_failure(status, "") {
                    mark_route_failure(&state, upstream, &target_model, status, "");
                    mark_pool_failure(&state, target, status, "");
                    last_failure = Some(RouteFailure {
                        upstream: upstream.clone(),
                        model: target_model,
                        status,
                        message: "候选准备失败".to_string(),
                        recorded: false,
                    });
                    continue;
                }
                return response;
            }
        };
        let response = match state
            .client
            .post(&prepared.url)
            .headers(prepared.headers.clone())
            .json(&prepared.outbound)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                if has_more {
                    mark_route_failure(
                        &state,
                        upstream,
                        &prepared.target_model,
                        StatusCode::BAD_GATEWAY,
                        "",
                    );
                    mark_pool_failure(&state, target, StatusCode::BAD_GATEWAY, "");
                    attempts.push(format!("{}=network", upstream.name));
                    tracing::warn!(
                        requested = %requested_model,
                        upstream = %upstream.name,
                        model = %prepared.target_model,
                        error = %err,
                        "路由候选连接失败，切换下一个"
                    );
                    last_failure = Some(RouteFailure {
                        upstream: upstream.clone(),
                        model: prepared.target_model,
                        status: StatusCode::BAD_GATEWAY,
                        message: err.to_string(),
                        recorded: false,
                    });
                    continue;
                }
                mark_route_failure(
                    &state,
                    upstream,
                    &prepared.target_model,
                    StatusCode::BAD_GATEWAY,
                    "",
                );
                mark_pool_failure(&state, target, StatusCode::BAD_GATEWAY, "");
                record(
                    &state,
                    upstream,
                    &requested_model,
                    &prepared.target_model,
                    &ctx,
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
            let message = privacy_error_hint(upstream, message);
            attempts.push(attempt_label(status.as_u16()));
            if has_more && is_retryable_failure(status, &message) {
                mark_route_failure(&state, upstream, &prepared.target_model, status, &message);
                mark_pool_failure(&state, target, status, &message);
                tracing::warn!(
                    requested = %requested_model,
                    upstream = %upstream.name,
                    model = %prepared.target_model,
                    status = status.as_u16(),
                    error = %brief_text(&message, 200),
                    "路由候选失败，切换下一个"
                );
                last_failure = Some(RouteFailure {
                    upstream: upstream.clone(),
                    model: prepared.target_model,
                    status,
                    message,
                    recorded: false,
                });
                continue;
            }
            if is_retryable_failure(status, &message) {
                mark_route_failure(&state, upstream, &prepared.target_model, status, &message);
                mark_pool_failure(&state, target, status, &message);
            }
            record(
                &state,
                upstream,
                &requested_model,
                &prepared.target_model,
                &ctx,
                started,
                status.as_u16(),
                prepared.stream,
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

        mark_route_success(&state, upstream, &prepared.target_model);
        mark_pool_success(&state, target);
        attempts.push(attempt_label(200));
        log_route_success(&requested_model, group_header.as_deref(), &attempts);

        if prepared.is_opencode && !prepared.client_stream {
            let text = response.text().await.unwrap_or_default();
            let (value, usage) =
                opencode::aggregate_sse(&text, &prepared.tool_state, &requested_model);
            record_ex(
                &state,
                upstream,
                &requested_model,
                &prepared.target_model,
                &ctx,
                started,
                200,
                false,
                usage.as_ref(),
                None,
                0,
                false,
                false,
            );
            let response = Json(value).into_response();
            return with_route_headers(
                response,
                upstream,
                &prepared.target_model,
                group_header.as_deref(),
                &attempts,
            );
        }
        if !prepared.stream {
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
                    let mut attempts_count = 0;
                    while st.paused && upstream.pause_turn && attempts_count < 5 {
                        attempts_count += 1;
                        st.paused = false;
                        let retry = state
                            .client
                            .post(&prepared.url)
                            .headers(prepared.headers.clone())
                            .json(&prepared.outbound)
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
                upstream,
                &requested_model,
                &prepared.target_model,
                &ctx,
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
            let response = Json(value).into_response();
            return with_route_headers(
                response,
                upstream,
                &prepared.target_model,
                group_header.as_deref(),
                &attempts,
            );
        }
        let pause_turn = upstream.pause_turn;
        let response = match upstream.protocol.as_str() {
            "anthropic" => {
                stream_anthropic(
                    state.clone(),
                    upstream.clone(),
                    requested_model.clone(),
                    prepared.target_model.clone(),
                    response,
                    ctx.clone(),
                )
                .await
            }
            "commandcode" => {
                stream_commandcode(
                    state.clone(),
                    upstream.clone(),
                    requested_model.clone(),
                    prepared.target_model.clone(),
                    response,
                    prepared.url.clone(),
                    prepared.headers.clone(),
                    prepared.outbound.clone(),
                    pause_turn,
                    ctx.clone(),
                )
                .await
            }
            _ if prepared.is_opencode => {
                stream_opencode(
                    state.clone(),
                    upstream.clone(),
                    requested_model.clone(),
                    prepared.target_model.clone(),
                    response,
                    prepared.tool_state,
                    ctx.clone(),
                )
                .await
            }
            _ => {
                stream_response(
                    state.clone(),
                    upstream.clone(),
                    requested_model.clone(),
                    prepared.target_model.clone(),
                    response,
                    ctx.clone(),
                )
                .await
            }
        };
        return with_route_headers(
            response,
            upstream,
            &prepared.target_model,
            group_header.as_deref(),
            &attempts,
        );
    }

    // 所有候选都失败：按最后一个真实尝试记录用量，再返回聚合错误。
    if let Some(failure) = &last_failure {
        if !failure.recorded {
            record(
                &state,
                &failure.upstream,
                &requested_model,
                &failure.model,
                &ctx,
                started,
                failure.status.as_u16(),
                false,
                None,
                Some(failure.message.clone()),
            );
        }
    }
    let chain = if attempts.is_empty() {
        "无可用候选".to_string()
    } else {
        attempts.join(" -> ")
    };
    let message = match group_header.as_deref() {
        Some(group) => format!("路由组「group/{group}」全部候选失败：{chain}"),
        None => format!("模型「{requested_model}」的上游候选全部失败：{chain}"),
    };
    let status = if last_failure
        .as_ref()
        .map(|failure| failure.status == StatusCode::TOO_MANY_REQUESTS)
        .unwrap_or(false)
    {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::BAD_GATEWAY
    };
    error_json(status, message, "upstream_error", "all_candidates_failed")
}

/// 普通模型自动补位时的候选上限（主选 + 最多 2 条备选）。
const MAX_ROUTE_ATTEMPTS: usize = 3;

struct AttemptTarget {
    upstream: Upstream,
    model: String,
    /// 池成员（`credentialPool` 绑定时）。同一上游的多个成员会展开成多个候选，
    /// 复用现有候选链实现成员级故障转移。
    credential: Option<crate::credential_pool::PoolCredential>,
}

struct RoutePlan {
    targets: Vec<AttemptTarget>,
    /// `group/<id>` 的 id；普通模型为 `None`。
    group: Option<String>,
}

struct RouteFailure {
    upstream: Upstream,
    model: String,
    status: StatusCode,
    message: String,
    /// 候选失败是否已在别处记录过用量，避免重复覆盖。
    recorded: bool,
}

struct PreparedAttempt {
    target_model: String,
    url: String,
    headers: reqwest::header::HeaderMap,
    outbound: Value,
    client_stream: bool,
    stream: bool,
    is_opencode: bool,
    tool_state: opencode::ToolState,
}

/// 把一个上游展开成候选：绑定凭据池时，每个可用成员一个候选（最多 5 个），
/// 复用现有候选链实现成员级故障转移；未绑定池时保持单候选。
///
/// 设计约束：池存在但无可用成员时默认**不回落**单 Key，避免误用错误凭据；
/// 只有池显式声明 `fallbackToSingle` 才回落。
fn expand_attempt_targets(
    state: &AppState,
    upstream: &Upstream,
    model: &str,
) -> Vec<AttemptTarget> {
    // pool 模块关闭：忽略上游的池绑定，一律按单 Key 直连。
    // 池子系统整体缺席（路由/线程都不挂载）时读池表只会得到空结果，
    // 这里提前短路并给出明确语义，而不是让请求撞上不可用的池。
    if !state.modules.pool {
        return vec![AttemptTarget {
            upstream: upstream.clone(),
            model: model.to_string(),
            credential: None,
        }];
    }
    let Some(pool_id) = upstream.credential_pool.as_deref() else {
        return vec![AttemptTarget {
            upstream: upstream.clone(),
            model: model.to_string(),
            credential: None,
        }];
    };
    let Some(pool) = crate::credential_pool::get_pool(state, pool_id) else {
        tracing::warn!(
            upstream = %upstream.name,
            pool = %pool_id,
            "上游绑定的凭据池不存在，禁止回落单 Key"
        );
        return Vec::new();
    };
    let mut targets = Vec::new();
    let mut exclude: Vec<String> = Vec::new();
    for _ in 0..5 {
        match crate::credential_pool::select_credential(state, &pool, &exclude) {
            Ok(credential) => {
                exclude.push(credential.member_id.clone());
                targets.push(AttemptTarget {
                    upstream: upstream.clone(),
                    model: model.to_string(),
                    credential: Some(credential),
                });
            }
            Err(reason) => {
                if targets.is_empty() {
                    tracing::warn!(
                        upstream = %upstream.name,
                        pool = %pool_id,
                        %reason,
                        "凭据池没有可用成员"
                    );
                }
                break;
            }
        }
    }
    if targets.is_empty() && pool.fallback_to_single {
        targets.push(AttemptTarget {
            upstream: upstream.clone(),
            model: model.to_string(),
            credential: None,
        });
    }
    targets
}

fn pool_unavailable_response(upstream: &Upstream) -> Response {
    error_json(
        StatusCode::SERVICE_UNAVAILABLE,
        format!(
            "上游「{}」的凭据池没有可用成员（全部冷却/停用/配额耗尽）",
            upstream.name
        ),
        "upstream_error",
        "pool_unavailable",
    )
}

fn mark_pool_failure(state: &AppState, target: &AttemptTarget, status: StatusCode, message: &str) {
    let Some(credential) = &target.credential else {
        return;
    };
    let hard = status == StatusCode::TOO_MANY_REQUESTS || is_quota_message(message);
    crate::credential_pool::mark_failure(state, &credential.member_id, hard);
}

fn mark_pool_success(state: &AppState, target: &AttemptTarget) {
    if let Some(credential) = &target.credential {
        crate::credential_pool::mark_success(state, &credential.member_id);
    }
}

/// 解析本次请求的候选链。
///
/// - `forced`（URL 前缀或 `上游/模型`）：只走这一条，保持既有强制语义；
/// - `group/<id>`：按成员声明顺序；
/// - 普通模型：主选 + 其它声明了同一模型的上游，最多 [`MAX_ROUTE_ATTEMPTS`] 条。
fn build_route_plan(
    state: &AppState,
    cfg: &Config,
    requested_model: &str,
    route_model: &str,
    forced: Option<&str>,
    scope_allowed: Option<&[String]>,
    scope: Option<&UnifiedScope>,
) -> Result<RoutePlan, Response> {
    let scope_allows = |name: &str| {
        scope_allowed
            .map(|allowed| allowed.iter().any(|item| item == name))
            .unwrap_or(true)
    };
    if let Some(name) = forced {
        let upstream = cfg
            .upstream(name)
            .ok_or_else(|| unknown_upstream(state, name))?;
        if !upstream.enabled {
            return Err(error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("上游「{name}」已停用；请在接入管理中启用后再试"),
                "upstream_error",
                "upstream_disabled",
            ));
        }
        let targets = expand_attempt_targets(state, upstream, route_model);
        if targets.is_empty() {
            return Err(pool_unavailable_response(upstream));
        }
        return Ok(RoutePlan {
            targets,
            group: None,
        });
    }

    let effective = cfg
        .aliases
        .get(requested_model)
        .map(String::as_str)
        .unwrap_or(requested_model);
    if effective.starts_with("group/") {
        let group_id = cfg.group_id_from_model(effective).ok_or_else(|| {
            error_json(
                StatusCode::NOT_FOUND,
                format!("未知的路由组「{effective}」；路由组格式为 group/<id>，id 不能包含 /"),
                "invalid_request_error",
                "unknown_group",
            )
        })?;
        let group = cfg.route_group(group_id).ok_or_else(|| {
            let available: Vec<&str> = cfg.groups.iter().map(|group| group.id.as_str()).collect();
            let message = if available.is_empty() {
                format!("未知的路由组「group/{group_id}」；当前没有配置任何路由组")
            } else {
                format!(
                    "未知的路由组「group/{group_id}」。可用路由组：{}",
                    available.join("、")
                )
            };
            error_json(
                StatusCode::NOT_FOUND,
                message,
                "invalid_request_error",
                "unknown_group",
            )
        })?;
        let mut targets = Vec::new();
        for (upstream, model) in cfg.group_targets(group) {
            if !scope_allows(upstream.name.as_str()) {
                continue;
            }
            targets.extend(expand_attempt_targets(state, upstream, &model));
        }
        if targets.is_empty() {
            let message = match scope {
                Some(scope) => format!(
                    "路由组「group/{}」在统一接口{}没有可见成员",
                    group.id,
                    scope.describe()
                ),
                None => format!("路由组「group/{}」没有可用成员", group.id),
            };
            return Err(error_json(
                if scope.is_some() {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                message,
                "upstream_error",
                "group_unavailable",
            ));
        }
        order_targets_by_health(state, &mut targets);
        return Ok(RoutePlan {
            targets,
            group: Some(group.id.clone()),
        });
    }

    let primary = cfg
        .pick_upstream_scoped(Some(route_model), scope_allowed)
        .ok_or_else(|| {
            error_json(
                StatusCode::BAD_GATEWAY,
                "未配置任何上游",
                "upstream_error",
                "no_upstream",
            )
        })?;
    let mut targets = expand_attempt_targets(state, primary, route_model);
    if targets.is_empty() {
        return Err(pool_unavailable_response(primary));
    }
    // 自动故障切换：其它声明了同一模型的上游，按配置顺序补位。
    // 上限按“上游数”计，不按成员数计：一个池的多个成员仍算一个上游。
    let alias_target = cfg
        .aliases
        .get(route_model)
        .map(String::as_str)
        .unwrap_or(route_model);
    let mut upstream_count = 1;
    for upstream in &cfg.upstreams {
        if upstream_count >= MAX_ROUTE_ATTEMPTS {
            break;
        }
        if upstream.name == primary.name || !scope_allows(upstream.name.as_str()) {
            continue;
        }
        let declares = upstream.models.iter().any(|model| model == alias_target)
            || upstream.model_map.contains_key(route_model);
        if declares {
            let expanded = expand_attempt_targets(state, upstream, route_model);
            if !expanded.is_empty() {
                upstream_count += 1;
                targets.extend(expanded);
            }
        }
    }
    order_targets_by_health(state, &mut targets);
    Ok(RoutePlan {
        targets,
        group: None,
    })
}

fn route_key(upstream: &str, model: &str) -> String {
    format!("{upstream}\u{0}{model}")
}

/// 冷却中的候选排到链尾；稳定排序保留人工顺序。全部冷却时仍按原顺序尝试。
fn order_targets_by_health(state: &AppState, targets: &mut [AttemptTarget]) {
    let now = Instant::now();
    let health = state.route_health.lock().unwrap_or_else(|e| e.into_inner());
    targets.sort_by_key(|target| {
        health
            .get(&route_key(&target.upstream.name, &target.model))
            .map(|until| *until > now)
            .unwrap_or(false)
    });
}

fn mark_route_success(state: &AppState, upstream: &Upstream, model: &str) {
    state
        .route_health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&route_key(&upstream.name, model));
}

fn mark_route_failure(
    state: &AppState,
    upstream: &Upstream,
    model: &str,
    status: StatusCode,
    message: &str,
) {
    let seconds = if status == StatusCode::TOO_MANY_REQUESTS || is_quota_message(message) {
        300
    } else {
        60
    };
    let until = Instant::now() + Duration::from_secs(seconds);
    state
        .route_health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(route_key(&upstream.name, model), until);
}

fn is_quota_message(message: &str) -> bool {
    let text = message.to_lowercase();
    [
        "insufficient_quota",
        "quota exceeded",
        "quota_exhausted",
        "quota exhausted",
        "daily limit",
        "free usage limit",
        "credit balance",
        "out of credits",
        "余额",
        "额度",
        "配额",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// 可切换的失败：连接失败、408、429、5xx；少数上游用 400/403 + 明确配额
/// 文案表示耗尽，也允许切换。普通 400/401/403/404 直接透传，避免掩盖配置错误。
fn is_retryable_failure(status: StatusCode, message: &str) -> bool {
    let code = status.as_u16();
    if code == 408 || code == 429 || code >= 500 {
        return true;
    }
    (code == 400 || code == 403) && is_quota_message(message)
}

fn log_route_success(requested_model: &str, group: Option<&str>, attempts: &[String]) {
    if attempts.len() <= 1 {
        return;
    }
    tracing::info!(
        requested = %requested_model,
        group = group.unwrap_or(""),
        chain = %attempts.join(" -> "),
        "路由切换后成功"
    );
}

/// 补齐真实上游、候选链与路由组响应头；流式响应在开始写字节前调用。
fn with_route_headers(
    mut response: Response,
    upstream: &Upstream,
    target_model: &str,
    group: Option<&str>,
    attempts: &[String],
) -> Response {
    if let Ok(value) = upstream.name.parse() {
        response.headers_mut().insert("x-bridge-upstream", value);
    }
    if let Ok(value) = target_model.parse() {
        response
            .headers_mut()
            .insert("x-bridge-upstream-model", value);
    }
    if let Some(group) = group {
        if let Ok(value) = group.parse() {
            response.headers_mut().insert("x-bridge-group", value);
        }
    }
    if let Ok(value) = attempts.len().to_string().parse() {
        response
            .headers_mut()
            .insert("x-bridge-route-attempts", value);
    }
    if !attempts.is_empty() {
        if let Ok(value) = attempts.join(" -> ").parse() {
            response.headers_mut().insert("x-bridge-route-chain", value);
        }
    }
    response
}

/// 为一次候选尝试准备真实模型名、请求体、地址和鉴权头。
///
/// 从原 `chat_scoped` 中抽出：每个候选都必须用全新的 payload 克隆，避免
/// 前一个候选的 `model` / 隐私字段污染后一个候选。
fn prepare_attempt(
    state: &AppState,
    upstream: &Upstream,
    route_model: &str,
    payload: &Value,
    max_tokens: u64,
    credential: Option<&crate::credential_pool::PoolCredential>,
) -> Result<PreparedAttempt, Response> {
    let target_model = upstream
        .model_map
        .get(route_model)
        .cloned()
        .unwrap_or_else(|| route_model.to_string());
    // 池成员优先：同一上游的候选可能携带不同成员密钥；未绑定时回落到
    // 逐模型密钥（必须用最终发给上游的 target_model 选 key）。
    let model_key = match credential {
        Some(credential) => crate::config::KeySelection {
            key: credential.key.clone(),
            source: "pool".into(),
            env_name: None,
            from: "pool".into(),
        },
        None => upstream.key_for_model(&target_model),
    };
    if !upstream.model_keys.is_empty() {
        tracing::debug!(
            upstream = %upstream.name,
            model = %target_model,
            key_from = %model_key.from,
            key_source = %model_key.source,
            key_env = model_key.env_name.as_deref().unwrap_or(""),
            "逐模型密钥选择"
        );
    }

    let client_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_opencode = upstream.transport == "opencode";
    if is_opencode && !opencode::is_free_model(&target_model) {
        return Err(error_json(
            StatusCode::BAD_REQUEST,
            format!(
                "模型 {target_model} 不是 OpenCode 免费模型；transport=opencode 只接受 -free、big-pickle 或 grok-code"
            ),
            "invalid_request_error",
            "model_not_free",
        ));
    }
    let stream = client_stream || is_opencode;
    let base = upstream.base_url.trim_end_matches('/');
    let model_limit = upstream.model_max_output_tokens(&target_model);
    let mut payload = payload.clone();
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
    let mut outbound: Value;
    let mut tool_state = opencode::ToolState::default();
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
            if model_key.key.is_empty() && upstream.compat.auth_scheme != "none" {
                return Err(error_json(
                    StatusCode::UNAUTHORIZED,
                    "未配置 CommandCode 凭据：请设置 COMMAND_CODE_API_KEY 环境变量，或在面板「写入密钥」保存",
                    "authentication_error",
                    "missing_api_key",
                ));
            }
            outbound = commandcode::build_payload(&payload, &target_model, fallback_max);
        }
        other => {
            return Err(error_json(
                StatusCode::NOT_IMPLEMENTED,
                format!("Rust 内核尚未实现 {other} 协议（迁移中）"),
                "server_error",
                "not_implemented",
            ));
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
    apply_auth_headers(&mut headers, upstream, &model_key.key);
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
    // 通用隐私策略：用户意图由 compat.privacy 决定，落地方式由 provider profile 声明。
    for warning in apply_privacy(&upstream.compat.privacy, &mut headers, &mut outbound) {
        tracing::warn!(upstream = %upstream.name, "{warning}");
    }

    Ok(PreparedAttempt {
        target_model,
        url,
        headers,
        outbound,
        client_stream,
        stream,
        is_opencode,
        tool_state,
    })
}

/// SSE 事件边界 `\n\n` 的字节级查找（与旧实现 `String::find("\n\n")` 同口径）。
fn find_event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|w| w == b"\n\n")
}

/// 事件块是否值得做 JSON 解析：真实 usage 键在 JSON 里必然以 `"usage"` 字节
/// 序列出现。中间事件占一条流的绝大多数且都不含它，预判命中才解析——
/// 否则一次长流会产生数千次只为抓末尾 usage 的整事件 JSON parse。
/// 字符串内容里偶尔出现同名词只是多一次解析，不影响正确性。
fn block_may_contain_usage(block: &[u8]) -> bool {
    block.windows(7).any(|w| w == b"\"usage\"")
}

async fn stream_response(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
    ctx: prompt::RequestCtx,
) -> Response {
    let started = Instant::now();
    let name = upstream.name.clone();
    let header_model = target_model.clone();
    let mut byte_stream = response.bytes_stream();
    let stream = async_stream::stream! {
        // 保留原始字节：转发字节不变，usage 抓取只对通过预判的块做 UTF-8 转换。
        // 旧实现每个 chunk 都 from_utf8_lossy，跨 chunk 的多字节字符会被
        // replacement char 打断（影响 usage 抓取窗口），且平白多一次拷贝。
        let mut buffer: Vec<u8> = Vec::new();
        let mut usage: Option<Value> = None;
        let mut ttft_ms = 0i64;
        while let Some(chunk) = byte_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    // 逐块扫描 SSE，抓取末尾的 usage（不改变转发字节）。
                    while let Some(pos) = find_event_boundary(&buffer) {
                        let block: Vec<u8> = buffer.drain(..pos + 2).collect();
                        if !block_may_contain_usage(&block) {
                            continue;
                        }
                        let block = String::from_utf8_lossy(&block);
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
        record_ex(&state, &upstream, &requested_model, &target_model, &ctx, started, 200, true,
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
    ctx: prompt::RequestCtx,
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
        record_ex(&state, &upstream, &requested_model, &target_model, &ctx, started, 200, true,
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
    ctx: prompt::RequestCtx,
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
        record_ex(&state, &upstream, &requested_model, &target_model, &ctx, started, 200, true,
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
    ctx: prompt::RequestCtx,
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
        record_ex(&state, &upstream, &requested_model, &target_model, &ctx, started, 200, true,
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
    ctx: &prompt::RequestCtx,
    started: Instant,
    status: u16,
    stream: bool,
    usage: Option<&Value>,
    error: Option<String>,
) {
    record_ex(
        state, up, requested, target, ctx, started, status, stream, usage, error, 0, false, false,
    );
}

pub(crate) fn record_ex(
    state: &AppState,
    up: &Upstream,
    requested: &str,
    target: &str,
    ctx: &prompt::RequestCtx,
    started: Instant,
    status: u16,
    stream: bool,
    usage: Option<&Value>,
    error: Option<String>,
    ttft_ms: i64,
    truncated: bool,
    paused: bool,
) {
    let stats_on = state.modules.stats;
    let logging_on = state.modules.logging;
    if !stats_on && !logging_on {
        // 统计与日志全关：请求收尾只差记录，转发本身不受影响。
        return;
    }
    let usage = usage.cloned().unwrap_or(Value::Null);
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_read_tokens = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_i64)
        .or_else(|| usage.get("cacheReadTokens").and_then(Value::as_i64))
        .unwrap_or(0);
    let cache_write_tokens = usage
        .get("cacheWriteTokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // 费用按单价表即时估算（人民币落库）；峰谷判定用当前时间。
    // stats 关闭时跳过单价查询——访问日志不需要费用。
    let cost = if stats_on {
        state.cfg().pricing.cost_for(
            &up.name,
            requested,
            target,
            chrono::Utc::now().timestamp(),
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_write_tokens,
        )
    } else {
        0.0
    };
    let rec = UsageRecord {
        id: ctx.id.clone(),
        upstream: up.name.clone(),
        protocol: up.protocol.clone(),
        requested_model: requested.to_string(),
        upstream_model: target.to_string(),
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_write_tokens,
        duration_ms: started.elapsed().as_millis() as i64,
        ttft_ms,
        status: status as i64,
        stream,
        truncated,
        paused,
        error,
        cost,
        session: ctx.session.clone(),
    };
    // 访问日志先写（只借用），再投递后台队列：请求路径只投递，一次事务
    // 聚合多个并发请求。队列满/线程不可用时同步直写兜底——用量是计费与
    // 统计的事实源，绝不丢弃。
    if logging_on {
        log_access(state, &rec, ttft_ms, truncated, paused);
    }
    if !stats_on {
        return;
    }
    // 今日指标走内存计数：托盘零 SQL。
    state
        .today
        .record(rec.status, rec.prompt_tokens, rec.cache_read_tokens);
    state.usage_pending.fetch_add(1, Ordering::Relaxed);
    match state.usage_tx.as_ref() {
        Some(tx) => match tx.try_send(rec) {
            Ok(()) => {}
            Err(TrySendError::Full(rec)) | Err(TrySendError::Disconnected(rec)) => {
                state.usage_pending.fetch_sub(1, Ordering::Relaxed);
                state.db.record_usage(&rec);
            }
        },
        // stats 开启但 writer 不可用（仅线程 spawn 失败时可能）：同步直写兜底。
        None => {
            state.usage_pending.fetch_sub(1, Ordering::Relaxed);
            state.db.record_usage(&rec);
        }
    }
}

/// 请求调用日志：受 `accessLog` 开关控制，格式对齐 Python 版（模型 -> 上游:目标 状态 耗时 tokens）；
/// 行尾追加 `req=<请求 ID>`，面板请求抽屉「打开日志」按它回查该次请求的日志行。
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
    extra.push_str(&format!(" req={}", rec.id));
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

/// 读取入站会话标识：优先客户端显式头，缺失时回退「模型 + 首条 user」哈希。
fn resolve_session(headers: &HeaderMap, payload: &Value) -> String {
    const NAMES: [&str; 3] = ["x-session-id", "x-opencode-session", "x-conversation-id"];
    for name in NAMES {
        let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) else {
            continue;
        };
        let cleaned: String = value
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    session_id(payload)
}

/// 提示词后台任务：只携带原始 body（`Bytes` 克隆是引用计数，不复制正文）、
/// 请求上下文与本次请求生效的记录参数。
struct PromptJob {
    body: Bytes,
    ctx: prompt::RequestCtx,
    requested_model: String,
    max_chars: usize,
    retention_days: i64,
    max_records: i64,
}

/// 记录入站提示词：请求路径只做开关判断、参数快照和入队，JSON 解析、脱敏、
/// 批量写库、保留清理全部在专用后台线程完成。
///
/// 设计约束：
/// - 开关关闭时不克隆 body、不入队；
/// - 队列满时直接丢弃本次记录并计数，绝不等待或反压请求；
/// - 后台线程失败只记日志，不影响已经返回的代理响应。
fn capture_prompt(
    state: &Arc<AppState>,
    body: Bytes,
    ctx: &prompt::RequestCtx,
    requested_model: &str,
) {
    let cfg = state.cfg();
    if !cfg.prompt_capture {
        return;
    }
    let job = PromptJob {
        body,
        ctx: ctx.clone(),
        requested_model: requested_model.to_string(),
        max_chars: cfg.prompt_max_chars as usize,
        retention_days: cfg.prompt_retention_days as i64,
        max_records: cfg.prompt_max_records as i64,
    };
    drop(cfg);

    // stats 模块关闭时 writer 未启动；调用点已做门控，这里兜底直接丢弃。
    let Some(prompt_tx) = state.prompt_tx.as_ref() else {
        return;
    };
    match prompt_tx.try_send(job) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            let dropped = state.prompt_dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 100 == 0 {
                tracing::warn!("提示词后台队列已满，本次记录丢弃（累计 {dropped}）");
            }
        }
        Err(TrySendError::Disconnected(_)) => {
            tracing::warn!("提示词后台线程未运行，本次记录丢弃");
        }
    }
}

/// 专用后台线程：把并发请求聚成小批，一次事务写入；保留清理按批次/时间触发。
fn prompt_writer_loop(db: Arc<Db>, rx: Receiver<PromptJob>) {
    let mut written_since_cleanup: u64 = 0;
    let mut last_cleanup = chrono::Local::now().timestamp();
    loop {
        let first = match rx.recv() {
            Ok(job) => job,
            Err(_) => break,
        };
        let mut jobs = vec![first];
        let deadline = Instant::now() + Duration::from_millis(PROMPT_BATCH_WAIT_MS);
        while jobs.len() < PROMPT_BATCH_MAX {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(job) => jobs.push(job),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let records: Vec<PromptRecord> = jobs.iter().filter_map(parse_prompt_record).collect();
        if records.is_empty() {
            continue;
        }
        let written = db.record_prompts_batch(&records);
        written_since_cleanup += written as u64;
        let now = chrono::Local::now().timestamp();
        if written_since_cleanup >= PROMPT_CLEANUP_EVERY
            || now.saturating_sub(last_cleanup) >= PROMPT_CLEANUP_INTERVAL_SECS
        {
            if let Some(last) = jobs.last() {
                db.delete_prompts_older_than(last.retention_days);
                db.trim_prompts(last.max_records);
            }
            written_since_cleanup = 0;
            last_cleanup = now;
        }
    }
}

/// 用量专用后台线程：把并发请求的用量聚成小批，一次事务写入。
///
/// 与 `prompt_writer_loop` 同一套聚批策略；`pending` 计数供退出流程判断
/// 「还有多少条已投递未落库」，保证升级替换进程前把最后一批写完。
fn usage_writer_loop(db: Arc<Db>, rx: Receiver<UsageRecord>, pending: Arc<AtomicU64>) {
    loop {
        let first = match rx.recv() {
            Ok(record) => record,
            Err(_) => break,
        };
        let mut batch = vec![first];
        let deadline = Instant::now() + Duration::from_millis(USAGE_BATCH_WAIT_MS);
        while batch.len() < USAGE_BATCH_MAX {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(record) => batch.push(record),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        db.record_usage_batch(&batch);
        pending.fetch_sub(batch.len() as u64, Ordering::Relaxed);
    }
}

/// 后台线程内的解析与脱敏；独立函数便于单测直接验证。
fn parse_prompt_record(job: &PromptJob) -> Option<PromptRecord> {
    let payload: Value = serde_json::from_slice(&job.body).ok()?;
    let messages = payload.get("messages").and_then(Value::as_array)?;
    if messages.is_empty() {
        return None;
    }
    let encoded = prompt::prepare_messages(messages, job.max_chars);
    Some(PromptRecord {
        id: job.ctx.id.clone(),
        ts: 0,
        session: job.ctx.session.clone(),
        requested_model: job.requested_model.clone(),
        message_count: encoded.message_count,
        size_bytes: encoded.size_bytes,
        truncated: encoded.truncated,
        preview: encoded.preview,
        messages: encoded.messages,
        redactions: encoded.redactions as i64,
        truncation: encoded
            .truncation
            .and_then(|v| serde_json::to_string(&v).ok()),
        content_hash: encoded.content_hash,
        system_hash: encoded.system_hash,
        role_counts: encoded.role_counts,
        policy_version: crate::prompt::REDACTION_POLICY_VERSION,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[tokio::test]
    async fn config_preview_resolves_deepseek_profile() {
        let resp = crate::panel_api::api_config_preview(Json(json!({
            "protocol": "openai",
            "baseUrl": "https://api.deepseek.com/v1",
            "compat": {"profile": "deepseek"}
        })))
        .await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("读取 preview 响应");
        let body: Value = serde_json::from_slice(&bytes).expect("preview 响应是 JSON");
        assert_eq!(body["effective"]["profile"], "deepseek");
        assert_eq!(body["effective"]["chatPath"], "/v1/chat/completions");
        assert_eq!(body["effective"]["maxOutputTokens"], 393216);
        assert_eq!(body["effective"]["capabilities"]["thinking"], true);
    }

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

    /// 请求体超限：必须回结构化 413（而不是框架的纯文本），并指出上限与配置项。
    #[tokio::test]
    async fn chat_body_over_limit_returns_reasoned_413() {
        use tower::ServiceExt;

        let cfg = crate::config::Config::from_value(json!({
            "maxBodyBytes": 1024,
            "upstreams": {
                "u": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "apiKey": "k"}
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-body-limit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "x".repeat(4096)}]
        })
        .to_string();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/unified/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取 413 响应");
        let payload: Value = serde_json::from_slice(&bytes).expect("413 响应必须是 JSON");
        assert_eq!(payload["error"]["code"], "request_too_large");
        let message = payload["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("maxBodyBytes"),
            "错误信息要指出该改哪个配置：{message}"
        );
        assert!(
            message.contains("1.0 KiB"),
            "错误信息要给出当前上限：{message}"
        );
    }

    /// 管理接口的 `Json` 提取器也必须放宽：超过 axum 默认 2 MiB 的正文要能正常解析。
    #[tokio::test]
    async fn admin_json_body_above_default_limit_is_accepted() {
        use tower::ServiceExt;

        let cfg = crate::config::Config::from_value(json!({
            "upstreams": {
                "u": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "apiKey": "k"}
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-admin-body-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        // 3 MiB 正文：超过 axum 的 2 MiB 默认上限，低于本进程放宽后的 64 MiB。
        let body = json!({
            "config": {
                "upstreams": {
                    "u": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "apiKey": "k"}
                },
                "padding": "x".repeat(3 * 1024 * 1024)
            }
        })
        .to_string();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/config/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "3 MiB 的管理接口请求体不应再被 2 MiB 默认上限拒绝"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取校验响应");
        let payload: Value = serde_json::from_slice(&bytes).expect("校验响应必须是 JSON");
        assert!(
            payload.get("diagnostics").is_some(),
            "应返回配置诊断：{payload}"
        );
    }

    /// 假上游：记录每次请求的 `x-api-key` / `authorization`，并返回标准 OpenAI 响应。
    ///
    /// 用于断言「同一个上游、不同模型」实际发出的密钥不同。
    fn spawn_key_capture_server(
        serves: usize,
    ) -> (
        String,
        std::sync::mpsc::Receiver<(Option<String>, Option<String>)>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(serves) {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("复制流"));
                let mut content_length = 0usize;
                let mut x_api_key = None;
                let mut authorization = None;
                loop {
                    let mut line = String::new();
                    if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = trimmed.split_once(':') {
                        let name = name.trim().to_ascii_lowercase();
                        let value = value.trim().to_string();
                        match name.as_str() {
                            "x-api-key" => x_api_key = Some(value),
                            "authorization" => authorization = Some(value),
                            "content-length" => {
                                content_length = value.parse().unwrap_or(0);
                            }
                            _ => {}
                        }
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    let _ = std::io::Read::read_exact(&mut reader, &mut body);
                }
                let _ = tx.send((x_api_key, authorization));
                let body = r#"{"id":"chatcmpl-test","object":"chat.completion","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}"), rx)
    }

    /// 精简形态（modules 全关）：核心转发可用，面板/统计/扩展接口整体 404。
    #[tokio::test]
    async fn minimal_modules_keep_core_and_drop_panel() {
        use tower::ServiceExt;

        let url = spawn_fixed_chat_server("minimal-ok", 1);
        let cfg = crate::config::Config::from_value(json!({
            "modules": {
                "stats": false, "logging": false, "plugins": false, "pool": false,
                "panel": false, "access": false, "aiConfig": false, "update": false
            },
            "upstreams": {"u": {"protocol": "openai", "baseUrl": url, "apiKey": "k"}}
        }));
        let dir = std::env::temp_dir().join(format!("mg-minimal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        // 管理面整体缺席：每个模块组抽一个代表路由。
        for path in [
            "/api/info",
            "/ui/",
            "/api/usage/totals",
            "/api/plugins",
            "/api/pools",
            "/api/access",
            "/api/ai/manifest",
            "/api/update/status",
        ] {
            let response = router(state.clone())
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path} 应不挂载");
        }

        // 健康检查仍可用。
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 核心转发完整可用：请求打到假上游并返回其内容。
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "m",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取转发响应");
        let payload: Value = serde_json::from_slice(&bytes).expect("响应必须是 JSON");
        assert_eq!(payload["choices"][0]["message"]["content"], "minimal-ok");

        // stats 关闭：不落用量记录，今日计数器保持零。
        assert_eq!(
            state
                .db
                .list_usage(&Filters::default(), 10, 0, "ts", "desc")
                .len(),
            0
        );
        assert_eq!(state.today.snapshot()["requests"], 0);
    }

    /// 默认（全模块开启）：面板路由挂载，一次 chat 后用量落库且今日计数器 +1。
    #[tokio::test]
    async fn default_modules_record_usage_and_today_counters() {
        use tower::ServiceExt;

        let url = spawn_fixed_chat_server("counted", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {"u": {"protocol": "openai", "baseUrl": url, "apiKey": "k"}}
        }));
        let dir = std::env::temp_dir().join(format!("mg-counted-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "m",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 用量走后台聚批（50ms 窗口），给一次落库机会。
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            state
                .db
                .list_usage(&Filters::default(), 10, 0, "ts", "desc")
                .len(),
            1,
            "默认形态必须落一条用量记录"
        );
        let today = state.today.snapshot();
        assert_eq!(today["requests"], 1, "今日计数器即时累加");
        assert_eq!(today["ok"], 1);
        assert_eq!(today["prompt_tokens"], 1);

        // 默认形态下面板路由在位。
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// SSE 流假上游：chunked 逐块推送；首事件的多字节字符拆在两个 chunk 之间，
    /// filler 事件不含 usage，末尾事件带 usage，最后 [DONE] 收尾。
    fn spawn_sse_stream_server(filler_events: usize) -> String {
        use std::io::{Read, Write};
        fn chunk(sock: &mut std::net::TcpStream, data: &[u8]) -> std::io::Result<()> {
            use std::io::Write;
            write!(sock, "{:x}\r\n", data.len())?;
            sock.write_all(data)?;
            sock.write_all(b"\r\n")
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            // 多字节字符跨 chunk：旧实现逐 chunk from_utf8_lossy 会在这里插
            // replacement char（不影响转发字节，但会破坏 usage 抓取窗口的解析）。
            // 「中」= E4 B8 AD：前两个字节在上一块，第三字节在下一块开头。
            let _ = chunk(&mut sock, b"data: {\"id\":\"split\",\"delta\":\"\xe4\xb8");
            let _ = chunk(&mut sock, b"\xad\xe5\xad\x97\"}\n\n");
            for i in 0..filler_events {
                let event = format!("data: {{\"id\":\"e{i}\",\"delta\":\"中文增量内容{i}\"}}\n\n");
                let _ = chunk(&mut sock, event.as_bytes());
            }
            let _ = chunk(
                &mut sock,
                b"data: {\"id\":\"final\",\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
            );
            let _ = chunk(&mut sock, b"data: [DONE]\n\n");
            let _ = sock.write_all(b"0\r\n\r\n");
        });
        format!("http://{addr}")
    }

    /// 透传 SSE 热路径：中间事件不含 usage（预判跳过解析）也不丢末尾 usage，
    /// 跨 chunk 的多字节字符不再破坏事件边界；转发字节逐块原样。
    #[tokio::test]
    async fn stream_response_captures_usage_from_chunked_transparent_stream() {
        use tower::ServiceExt;

        let url = spawn_sse_stream_server(200);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {"u": {"protocol": "openai", "baseUrl": url, "apiKey": "k"}}
        }));
        let dir = std::env::temp_dir().join(format!("mg-sse-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "m",
                            "stream": true,
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取 SSE 流");
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("中字\""), "跨 chunk 多字节字符应完整透传");
        assert!(body.contains("中文增量内容0"), "中间事件应原样透传");
        assert!(body.contains("\"usage\""), "末尾 usage 事件应透传");
        assert!(body.contains("[DONE]"), "流应以 [DONE] 结尾");

        // usage 抓取仍有效：等聚批窗口后落库。
        std::thread::sleep(std::time::Duration::from_millis(300));
        let rows = state
            .db
            .list_usage(&Filters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows.len(), 1, "流结束后应记录一条用量");
        assert_eq!(
            rows[0].get("prompt_tokens").and_then(Value::as_i64),
            Some(7),
            "末尾事件的 usage 必须被抓到"
        );
        assert_eq!(state.today.snapshot()["prompt_tokens"], 7);
    }

    /// SSE 透传吞吐基准（人工触发，避免拖慢常规测试）：
    /// `cargo test bench_sse_stream_throughput -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn bench_sse_stream_throughput() {
        use tower::ServiceExt;

        let url = spawn_sse_stream_server(5000);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {"u": {"protocol": "openai", "baseUrl": url, "apiKey": "k"}}
        }));
        let dir = std::env::temp_dir().join(format!("mg-sse-bench-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));

        let started = Instant::now();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "m",
                            "stream": true,
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        println!(
            "SSE 5000 事件（{} KiB）透传耗时 {:?}",
            bytes.len() / 1024,
            started.elapsed()
        );
        assert!(!bytes.is_empty());
    }

    /// 假上游：返回带固定标记的 OpenAI 响应，用于断言请求实际落到了哪条上游。
    fn spawn_fixed_chat_server(marker: &'static str, serves: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(serves) {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("复制流"));
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = trimmed.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    let _ = std::io::Read::read_exact(&mut reader, &mut body);
                }
                let body = json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "model": "m",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": marker},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string();
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

    /// 同一个上游下，不同模型必须使用各自的密钥；`modelMap` 解析后的
    /// 真实模型名也必须命中对应密钥（不能用请求名查表）。
    #[test]
    fn chat_uses_per_model_keys_for_same_upstream() {
        let dir =
            std::env::temp_dir().join(format!("mg-proxy-model-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_a = dir.join("a.json");
        let key_b = dir.join("b.json");
        std::fs::write(&key_a, r#"{"apiKey":"key-a"}"#).unwrap();
        std::fs::write(&key_b, r#"{"apiKey":"key-b"}"#).unwrap();

        let (url, rx) = spawn_key_capture_server(3);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": url,
                    "models": ["real-a", "model-b"],
                    "modelMap": {"alias-a": "real-a"},
                    "compat": {"auth": {"scheme": "x-api-key"}},
                    "modelKeys": {
                        "real-a": {"apiKeyFile": key_a.to_string_lossy()},
                        "model-b": {"apiKeyFile": key_b.to_string_lossy()}
                    }
                }
            }
        }));
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        for model in ["real-a", "alias-a", "model-b"] {
            let body = Bytes::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            );
            let resp = runtime.block_on(chat_inner(state.clone(), None, HeaderMap::new(), body));
            assert_eq!(resp.status(), StatusCode::OK, "模型 {model} 应转发成功");
        }

        let first = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("第一个请求未到达上游");
        let second = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("第二个请求未到达上游");
        let third = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("第三个请求未到达上游");
        assert_eq!(first.0.as_deref(), Some("key-a"));
        assert_eq!(
            second.0.as_deref(),
            Some("key-a"),
            "modelMap 别名必须按真实模型 real-a 选 key"
        );
        assert_eq!(third.0.as_deref(), Some("key-b"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 统一模型名 `上游/模型` 必须锁定该上游；裸模型名仍按原规则分发。
    #[test]
    fn chat_routes_qualified_model_to_named_upstream() {
        let url_a = spawn_fixed_chat_server("from-a", 2);
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "a": {
                    "protocol": "openai",
                    "baseUrl": url_a,
                    "models": ["same"],
                    "default": true
                },
                "b": {
                    "protocol": "openai",
                    "baseUrl": url_b,
                    "models": ["same"]
                }
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-proxy-unified-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        let ask = |model: &str| {
            let body = Bytes::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            );
            let resp = runtime.block_on(chat_inner(state.clone(), None, HeaderMap::new(), body));
            let upstream = resp
                .headers()
                .get("x-bridge-upstream")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let target = resp
                .headers()
                .get("x-bridge-upstream-model")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = runtime
                .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
                .expect("读取响应体");
            let value: Value = serde_json::from_slice(&bytes).expect("响应是 JSON");
            (upstream, target, value)
        };

        // 裸模型名保持原有分发：两条上游都声明了 same，取配置里靠前的 a。
        let (upstream, target, value) = ask("same");
        assert_eq!(upstream, "a");
        assert_eq!(target, "same");
        assert_eq!(value["choices"][0]["message"]["content"], "from-a");

        // 统一模型名锁定 b，并回显调用方请求的完整模型名。
        let (upstream, target, value) = ask("b/same");
        assert_eq!(upstream, "b");
        assert_eq!(target, "same");
        assert_eq!(value["model"], "b/same");
        assert_eq!(value["choices"][0]["message"]["content"], "from-b");

        // 显式写 a/ 与原有 URL 前缀语义一致。
        let (upstream, _, value) = ask("a/same");
        assert_eq!(upstream, "a");
        assert_eq!(value["choices"][0]["message"]["content"], "from-a");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 统一接入口默认范围：被 visibleUpstreams 隐藏的上游必须拒绝。
    #[test]
    fn chat_unified_all_scope_rejects_hidden_upstream() {
        let url_a = spawn_fixed_chat_server("from-a", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "unified": {"visibleUpstreams": ["a"]},
            "upstreams": {
                "a": {
                    "protocol": "openai",
                    "baseUrl": url_a,
                    "models": ["same"]
                },
                "b": {
                    "protocol": "openai",
                    "baseUrl": "http://127.0.0.1:1",
                    "models": ["same"]
                }
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-proxy-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        let ask = |model: &str| {
            let body = Bytes::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            );
            let resp = runtime.block_on(chat_scoped(
                state.clone(),
                None,
                Some(UnifiedScope::All),
                HeaderMap::new(),
                body,
            ));
            let status = resp.status();
            let bytes = runtime
                .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
                .expect("读取响应体");
            let value: Value = serde_json::from_slice(&bytes).expect("响应是 JSON");
            (status, value)
        };

        let (status, value) = ask("b/same");
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(value["error"]["code"], "upstream_not_allowed");

        let (status, value) = ask("a/same");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["choices"][0]["message"]["content"], "from-a");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分组作用域：只允许组成员，未知分组 404，空分组 503。
    #[test]
    fn chat_unified_group_rejects_non_members_and_unknown_group() {
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "unified": {
                "groups": [
                    {"id": "g1", "upstreams": ["b"]},
                    {"id": "empty", "upstreams": []}
                ]
            },
            "upstreams": {
                "a": {
                    "protocol": "openai",
                    "baseUrl": "http://127.0.0.1:1",
                    "models": ["same"]
                },
                "b": {
                    "protocol": "openai",
                    "baseUrl": url_b,
                    "models": ["same"]
                }
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-proxy-group-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        let ask = |model: &str, group: &str| {
            let body = Bytes::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            );
            let resp = runtime.block_on(chat_scoped(
                state.clone(),
                None,
                Some(UnifiedScope::Group(group.to_string())),
                HeaderMap::new(),
                body,
            ));
            let status = resp.status();
            let bytes = runtime
                .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
                .expect("读取响应体");
            let value: Value = serde_json::from_slice(&bytes).expect("响应是 JSON");
            (status, value)
        };

        let (status, value) = ask("a/same", "g1");
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(value["error"]["code"], "upstream_not_allowed");

        let (status, value) = ask("b/same", "g1");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["choices"][0]["message"]["content"], "from-b");

        let (status, value) = ask("b/same", "nosuch");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["error"]["code"], "unknown_unified_group");

        let (status, value) = ask("b/same", "empty");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value["error"]["code"], "no_unified_upstream");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 统一清单：默认只列可见上游；分组再按成员收窄；未知分组 404。
    #[test]
    fn models_unified_scope_filters_visible_and_group() {
        let cfg = crate::config::Config::from_value(json!({
            "unified": {
                "visibleUpstreams": ["a", "b"],
                "groups": [
                    {"id": "g1", "upstreams": ["b"]},
                    {"id": "hidden", "upstreams": ["c"]}
                ]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["am"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["bm"]},
                "c": {"protocol": "openai", "baseUrl": "http://c", "models": ["cm"]}
            }
        }));
        let dir = std::env::temp_dir().join(format!("mg-proxy-models-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db, dir.join("c.json")));
        let runtime = rt();

        let read = |scope: UnifiedScope| {
            let resp = models_unified_inner(&state, scope);
            let status = resp.status();
            let bytes = runtime
                .block_on(axum::body::to_bytes(resp.into_body(), usize::MAX))
                .expect("读取响应体");
            let value: Value = serde_json::from_slice(&bytes).expect("响应是 JSON");
            let ids: Vec<String> = value["data"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row["id"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (status, ids, value)
        };

        let (status, ids, _) = read(UnifiedScope::All);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids, vec!["a/am", "b/bm"]);

        let (status, ids, _) = read(UnifiedScope::Group("g1".to_string()));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids, vec!["b/bm"]);

        // 组内成员 c 被 visibleUpstreams 隐藏 → 清单为空。
        let (status, ids, _) = read(UnifiedScope::Group("hidden".to_string()));
        assert_eq!(status, StatusCode::OK);
        assert!(ids.is_empty());

        let (status, _, value) = read(UnifiedScope::Group("nosuch".to_string()));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["error"]["code"], "unknown_unified_group");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn state_with_prompt_capture() -> Arc<AppState> {
        let cfg = crate::config::Config::from_value(json!({
            "promptCapture": true,
            "promptRetentionDays": 7,
            "promptMaxRecords": 100,
            "promptMaxChars": 100000,
            "modelsCacheSeconds": 300,
            "upstreams": {}
        }));
        let dir = std::env::temp_dir().join(format!("mg-prompt-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        Arc::new(AppState::new(cfg, db, dir.join("c.json")))
    }

    /// 提示词记录必须走后台：请求路径只做入队，不解析、不脱敏、不写库；
    /// body 再大也不能在调用方等待。后台则最终完成落库。
    #[test]
    fn prompt_capture_is_async_and_does_not_block() {
        let state = state_with_prompt_capture();
        let ctx = prompt::RequestCtx {
            id: "req-async".into(),
            session: "s-async".into(),
        };
        let long = "上下文".repeat(5000);
        let body = Bytes::from(
            json!({
                "model": "m",
                "messages": [
                    {"role": "system", "content": "s"},
                    {"role": "user", "content": format!("bearer 鉴权 Authorization: 中文 {long}")}
                ]
            })
            .to_string(),
        );

        let started = std::time::Instant::now();
        capture_prompt(&state, body, &ctx, "m");
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "capture_prompt 不得在请求路径解析或写库：{:?}",
            elapsed
        );

        // 直接查记录，不用 prompt_count_and_size：后者有 5 秒统计缓存，
        // 会让本测试等到缓存过期，正好压在超时边缘。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if state.db.get_prompt("req-async").is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "后台提示词写入未在 5s 内完成"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let row = state.db.get_prompt("req-async").expect("提示词应已落库");
        assert_eq!(row["session"], "s-async");
        assert!(row["messages"].to_string().contains("bearer 鉴权"));
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

    /// yesterday 对比前天一整天；24h/3d 沿用滚动区间口径。
    #[test]
    fn previous_window_for_yesterday_and_rolling_presets() {
        let now = 1_700_000_000i64;
        assert_eq!(
            previous_window("range=yesterday", now),
            Some((day_start(2), day_start(1))),
            "yesterday 的上一段是前天 00:00 ~ 昨天 00:00"
        );
        assert_eq!(
            previous_window("range=24h", now),
            Some((now - 2 * 86400, now - 86400))
        );
        assert_eq!(
            previous_window("range=3d", now),
            Some((now - 6 * 86400, now - 3 * 86400))
        );
    }

    /// 面板下拉新增预设：yesterday 是自然日口径（含上界），24h/3d 是滚动窗口。
    #[test]
    fn filters_from_query_supports_yesterday_24h_3d() {
        let now = chrono::Utc::now().timestamp();
        let f = filters_from_query("range=yesterday", now);
        assert_eq!(f.since_ts, Some(day_start(1)), "左边界是昨天 00:00");
        assert_eq!(f.until_ts, Some(today_start()), "右边界是今天 00:00");

        let f = filters_from_query("range=3d", now);
        assert_eq!(f.since_ts, Some(now - 3 * 86400));
        assert_eq!(f.until_ts, None);

        let f = filters_from_query("range=24h", now);
        assert_eq!(f.since_ts, Some(now - 86400));
    }

    #[test]
    fn resolve_session_prefers_client_header_and_falls_back_to_hash() {
        let payload = json!({
            "model": "mock-model",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "  client-session  ".parse().unwrap());
        assert_eq!(resolve_session(&headers, &payload), "client-session");

        let mut headers = HeaderMap::new();
        headers.insert("x-opencode-session", "opencode-session".parse().unwrap());
        assert_eq!(resolve_session(&headers, &payload), "opencode-session");

        let empty = HeaderMap::new();
        let hash = resolve_session(&empty, &payload);
        assert_eq!(hash, session_id(&payload), "缺失入站头时回退稳定哈希");
        assert_eq!(hash.len(), 16);
    }

    #[test]
    fn resolve_session_ignores_blank_and_control_only_headers() {
        let payload = json!({"model": "m", "messages": []});
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "   ".parse().unwrap());
        headers.insert("x-conversation-id", "conv-1".parse().unwrap());
        assert_eq!(resolve_session(&headers, &payload), "conv-1");
    }

    #[test]
    fn apply_privacy_writes_header_and_body_adapters() {
        let privacy = Privacy {
            zdr: Some(true),
            training: TrainingPolicy::Deny,
            zdr_adapter: PrivacyAdapter::Header {
                name: "x-test-zdr".into(),
                value: "1".into(),
            },
            training_allow_adapter: PrivacyAdapter::None,
            training_deny_adapter: PrivacyAdapter::Body {
                path: "provider.data_collection".into(),
                value: json!("deny"),
            },
        };
        let mut headers = reqwest::header::HeaderMap::new();
        let mut body = json!({"model": "m"});
        let warnings = apply_privacy(&privacy, &mut headers, &mut body);
        assert!(warnings.is_empty(), "支持的适配器不应产生警告");
        assert_eq!(headers.get("x-test-zdr").unwrap(), "1");
        assert_eq!(body["provider"]["data_collection"], "deny");
    }

    #[test]
    fn apply_privacy_reports_workspace_and_unsupported_without_sending() {
        let privacy = Privacy {
            zdr: Some(true),
            training: TrainingPolicy::Allow,
            zdr_adapter: PrivacyAdapter::Unsupported {
                reason: "no request-level zdr".into(),
            },
            training_allow_adapter: PrivacyAdapter::Workspace {
                url: "https://example.com/console".into(),
                setting: "Settings › Privacy".into(),
            },
            training_deny_adapter: PrivacyAdapter::None,
        };
        let mut headers = reqwest::header::HeaderMap::new();
        let mut body = json!({});
        let warnings = apply_privacy(&privacy, &mut headers, &mut body);
        assert_eq!(warnings.len(), 2);
        assert!(warnings
            .iter()
            .any(|w| w.contains("ZDR") && w.contains("不支持")));
        assert!(warnings.iter().any(|w| w.contains("控制台")));
        assert!(headers.is_empty());
        assert_eq!(body, json!({}));
    }

    #[test]
    fn set_json_path_creates_intermediate_objects() {
        let mut body = json!({});
        assert!(set_json_path(&mut body, "provider.zdr", json!(true)));
        assert_eq!(body["provider"]["zdr"], true);
        assert!(!set_json_path(&mut body, "", json!(true)));
    }

    fn pool_test_state(cfg: crate::config::Config) -> (Arc<AppState>, Arc<Db>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mg-proxy-pool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        let state = Arc::new(AppState::new(cfg, db.clone(), dir.join("c.json")));
        (state, db, dir)
    }

    /// 用量落库已改为后台批量写入：断言库内行数前先等队列清空（兜底 2s）。
    fn wait_usage_flush(state: &AppState) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while state.usage_pending.load(Ordering::Relaxed) > 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn header_of(response: &Response, name: &str) -> String {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    /// 路由组按成员顺序尝试：第一个 429，第二个成功；日志记录真实上游与真实模型。
    #[test]
    fn route_group_switches_to_next_member_on_429() {
        let url_a = spawn_status_server(
            "429 Too Many Requests",
            r#"{"error":{"message":"quota exhausted"}}"#,
            1,
        );
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "groups": {"daily": ["a/same", "b/same"]},
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": url_a, "models": ["same"]},
                "b": {"protocol": "openai", "baseUrl": url_b, "models": ["same"]}
            }
        }));
        let (state, db, dir) = pool_test_state(cfg);
        let runtime = rt();
        let body = Bytes::from(
            json!({
                "model": "group/daily",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let resp = runtime.block_on(chat_inner(state.clone(), None, HeaderMap::new(), body));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_of(&resp, "x-bridge-upstream"), "b");
        assert_eq!(header_of(&resp, "x-bridge-upstream-model"), "same");
        assert_eq!(header_of(&resp, "x-bridge-group"), "daily");
        assert_eq!(header_of(&resp, "x-bridge-route-attempts"), "2");

        wait_usage_flush(&state);
        let rows = db.list_usage(&Filters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows.len(), 1, "一次客户端请求只落一条最终用量");
        assert_eq!(rows[0]["upstream"], "b", "日志必须记录真实成功上游");
        assert_eq!(rows[0]["requested_model"], "group/daily");
        assert_eq!(rows[0]["upstream_model"], "same");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没有显式路由组时，普通模型名也会在同模型候选间自动切换。
    #[test]
    fn same_model_auto_failover_switches_to_declared_upstream() {
        let url_a = spawn_status_server(
            "502 Bad Gateway",
            r#"{"error":{"message":"upstream boom"}}"#,
            1,
        );
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": url_a, "models": ["same"]},
                "b": {"protocol": "openai", "baseUrl": url_b, "models": ["same"]}
            }
        }));
        let (state, db, dir) = pool_test_state(cfg);
        let runtime = rt();
        let body = Bytes::from(
            json!({
                "model": "same",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let resp = runtime.block_on(chat_inner(state.clone(), None, HeaderMap::new(), body));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_of(&resp, "x-bridge-upstream"), "b");
        assert_eq!(header_of(&resp, "x-bridge-route-attempts"), "2");
        wait_usage_flush(&state);
        let rows = db.list_usage(&Filters::default(), 10, 0, "ts", "desc");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["upstream"], "b");
        assert_eq!(rows[0]["requested_model"], "same");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// URL 前缀 / `上游/模型` 的强制语义不变：失败也不跨上游切换。
    #[test]
    fn forced_upstream_does_not_failover() {
        let url_a = spawn_status_server(
            "502 Bad Gateway",
            r#"{"error":{"message":"upstream boom"}}"#,
            1,
        );
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": url_a, "models": ["same"]},
                "b": {"protocol": "openai", "baseUrl": url_b, "models": ["same"]}
            }
        }));
        let (state, _db, dir) = pool_test_state(cfg);
        let runtime = rt();
        let body = Bytes::from(
            json!({
                "model": "same",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let resp = runtime.block_on(chat_inner(
            state,
            Some("a".to_string()),
            HeaderMap::new(),
            body,
        ));
        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "强制上游失败应直接返回，而不是切到 b 成功"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同一会话允许多种模型混用：每次请求各落一条，session 相同、真实上游不同。
    #[test]
    fn same_session_can_mix_models_and_logs_real_upstreams() {
        let url_a = spawn_fixed_chat_server("from-a", 1);
        let url_b = spawn_fixed_chat_server("from-b", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": url_a, "models": ["m1"]},
                "b": {"protocol": "openai", "baseUrl": url_b, "models": ["m2"]}
            }
        }));
        let (state, db, dir) = pool_test_state(cfg);
        let runtime = rt();
        for model in ["m1", "m2"] {
            let mut headers = HeaderMap::new();
            headers.insert("x-session-id", "sess-mix".parse().unwrap());
            let body = Bytes::from(
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            );
            let resp = runtime.block_on(chat_inner(state.clone(), None, headers, body));
            assert_eq!(resp.status(), StatusCode::OK, "模型 {model} 应成功");
        }
        let filters = Filters {
            session: Some("sess-mix".to_string()),
            ..Default::default()
        };
        wait_usage_flush(&state);
        let mut rows: Vec<(String, String, String)> = db
            .list_usage(&filters, 10, 0, "ts", "asc")
            .into_iter()
            .map(|row| {
                (
                    row["session"].as_str().unwrap_or("").to_string(),
                    row["upstream"].as_str().unwrap_or("").to_string(),
                    row["upstream_model"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("sess-mix".to_string(), "a".to_string(), "m1".to_string()),
                ("sess-mix".to_string(), "b".to_string(), "m2".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 统一接口的分组作用域会过滤路由组成员，不能绕过可见白名单。
    #[test]
    fn unified_scope_filters_route_group_members() {
        let url_hidden = spawn_fixed_chat_server("hidden", 1);
        let url_visible = spawn_fixed_chat_server("visible", 1);
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "unified": {"visibleUpstreams": ["visible"]},
            "groups": {"daily": ["hidden/same", "visible/same"]},
            "upstreams": {
                "hidden": {"protocol": "openai", "baseUrl": url_hidden, "models": ["same"]},
                "visible": {"protocol": "openai", "baseUrl": url_visible, "models": ["same"]}
            }
        }));
        let (state, _db, dir) = pool_test_state(cfg);
        let runtime = rt();
        let body = Bytes::from(
            json!({
                "model": "group/daily",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let resp = runtime.block_on(chat_scoped(
            state,
            None,
            Some(UnifiedScope::All),
            HeaderMap::new(),
            body,
        ));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_of(&resp, "x-bridge-upstream"),
            "visible",
            "隐藏上游不能因路由组而绕过统一接口白名单"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_route_group_returns_404() {
        let cfg = crate::config::Config::from_value(json!({
            "accessLog": false,
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1", "models": ["m"]}
            }
        }));
        let (state, _db, dir) = pool_test_state(cfg);
        let runtime = rt();
        let body = Bytes::from(
            json!({
                "model": "group/missing",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let resp = runtime.block_on(chat_inner(state.clone(), None, HeaderMap::new(), body));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 按上游声明的鉴权方式写入一个密钥。
///
/// 逐模型密钥（`modelKeys`）与上游级密钥共用这一条路径，保证：
/// - `x-api-key` / `both` / 自定义头 / bearer 的行为与旧实现一致；
/// - 空 key 或 `compat.auth.scheme=none` 时不发送任何鉴权头；
/// - 同一请求只发一种凭据头的值（`both` 除外，按配置双写相同值）。
pub(crate) fn apply_auth_headers(
    headers: &mut reqwest::header::HeaderMap,
    up: &Upstream,
    key: &str,
) {
    if key.is_empty() || up.compat.auth_scheme == "none" {
        return;
    }
    let bearer = key_with_prefix(&up.compat.auth_prefix, key);
    match up.compat.auth_scheme.as_str() {
        "x-api-key" => insert_header(headers, "x-api-key", key),
        "both" => {
            insert_header(headers, "authorization", &format!("Bearer {key}"));
            insert_header(headers, "x-api-key", key);
        }
        "header" => {
            let name = if up.compat.auth_header.is_empty() {
                "authorization"
            } else {
                &up.compat.auth_header
            };
            insert_header(headers, name, &bearer);
        }
        _ => insert_header(headers, "authorization", &bearer),
    }
}

/// 把通用隐私意图落到当前上游的 wire 上。
///
/// - Header：写请求头
/// - Body：按点分路径写请求体（自动创建中间对象）
/// - Workspace / Unsupported / None：不修改请求，返回警告交给调用方记录
pub(crate) fn apply_privacy(
    privacy: &Privacy,
    headers: &mut reqwest::header::HeaderMap,
    body: &mut Value,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if privacy.zdr == Some(true) {
        apply_privacy_adapter(&privacy.zdr_adapter, "ZDR", headers, body, &mut warnings);
    }
    match privacy.training {
        TrainingPolicy::Inherit => {}
        TrainingPolicy::Allow => apply_privacy_adapter(
            &privacy.training_allow_adapter,
            "允许训练",
            headers,
            body,
            &mut warnings,
        ),
        TrainingPolicy::Deny => apply_privacy_adapter(
            &privacy.training_deny_adapter,
            "禁止训练",
            headers,
            body,
            &mut warnings,
        ),
    }
    warnings
}

fn apply_privacy_adapter(
    adapter: &PrivacyAdapter,
    label: &str,
    headers: &mut reqwest::header::HeaderMap,
    body: &mut Value,
    warnings: &mut Vec<String>,
) {
    match adapter {
        PrivacyAdapter::None => {}
        PrivacyAdapter::Header { name, value } => {
            if let (Ok(name), Ok(value)) = (
                name.parse::<reqwest::header::HeaderName>(),
                value.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(name, value);
            } else {
                warnings.push(format!("{label} 请求头「{name}」无效，未发送"));
            }
        }
        PrivacyAdapter::Body { path, value } => {
            if !set_json_path(body, path, value.clone()) {
                warnings.push(format!("{label} 请求体路径「{path}」无效，未写入"));
            }
        }
        PrivacyAdapter::Workspace { url, setting } => {
            warnings.push(format!("{label} 需要在上游控制台设置：{setting}（{url}）"));
        }
        PrivacyAdapter::Unsupported { reason } => {
            warnings.push(format!("{label} 在该上游不支持：{reason}"));
        }
    }
}

/// 按点分路径写入 JSON；路径为空或中间节点不是对象时返回 false。
fn set_json_path(root: &mut Value, path: &str, value: Value) -> bool {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = json!({});
        }
        let obj = current.as_object_mut().expect("just ensured object");
        if !obj.contains_key(*part) {
            obj.insert((*part).to_string(), json!({}));
        }
        current = obj.get_mut(*part).expect("just inserted");
    }
    let last = parts[parts.len() - 1];
    if !current.is_object() {
        *current = json!({});
    }
    if let Some(obj) = current.as_object_mut() {
        obj.insert(last.to_string(), value);
        true
    } else {
        false
    }
}

/// 把上游「训练数据授权」类错误翻译成可操作的中文提示。
fn privacy_error_hint(upstream: &Upstream, message: String) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("trains on request data") && lower.contains("privacy settings") {
        let hint = match &upstream.compat.privacy.training_allow_adapter {
            PrivacyAdapter::Workspace { url, setting } => {
                format!("请让 workspace owner/admin 在 {url} → {setting} 打开授权")
            }
            PrivacyAdapter::Unsupported { reason } => {
                format!("该上游没有请求级授权开关：{reason}")
            }
            _ => "请在上游控制台的隐私设置中允许使用请求数据训练".to_string(),
        };
        return format!("{message}（该模型会使用请求数据训练；{hint}，或改用不训练的模型）");
    }
    message
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
        match range.as_str() {
            "all" => {}
            "today" => f.since_ts = Some(today_start()),
            "yesterday" => {
                // 昨日 = 完整的上一个自然日 [昨天 00:00, 今天 00:00)
                f.since_ts = Some(day_start(1));
                if f.until_ts.is_none() {
                    f.until_ts = Some(today_start());
                }
            }
            "1h" => f.since_ts = Some(now - 3600),
            "24h" => f.since_ts = Some(now - 86400),
            "3d" => f.since_ts = Some(now - 3 * 86400),
            "7d" => f.since_ts = Some(now - 7 * 86400),
            "30d" => f.since_ts = Some(now - 30 * 86400),
            _ => f.since_ts = Some(now - 86400),
        }
    }
    f
}

/// 上一等长区间的 (since_ts, until_ts)，用于「与上一区间对比」。
///
/// 口径与 Python 版一致：
/// - `range=all` 没有可比区间 → 返回 None（调用方跳过对比）；
/// - 自定义区间按 `[from, to]` 的实际跨度向前平移；
/// - `today` 对比「今天 00:00 之前等长的上一段」，`yesterday` 对比前天（避免与 24h 混淆）。
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
    if range == "today" || range == "yesterday" {
        let start = today_start();
        if range == "yesterday" {
            return Some((day_start(2), day_start(1)));
        }
        let span = (now - start).max(1);
        return Some((start - span, start));
    }
    let seconds = match range {
        "1h" => 3600,
        "24h" => 86400,
        "3d" => 3 * 86400,
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
    day_start(0)
}

/// n 天前（0 = 今天）的本地 00:00；今日/昨日这类自然日口径用它，避免夏令时下的 ±86400 漂移。
pub fn day_start(days_ago: i64) -> i64 {
    let now = chrono::Local::now();
    now.date_naive()
        .checked_sub_signed(chrono::Duration::days(days_ago))
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}
