//! 免费来源注册表与判定：免密（无需账号/Key）上游的来源定义、免费模型筛选、拒因归一化与退避。
//!
//! 数据源为编译期嵌入的 `free_sources.json`（crate 根目录），与 provider 预设注册表同款做法。
//!
//! 设计要点（来自 plan.md 的 D3 与专家评估 §3.1）：
//! - **筛选 ≠ 可用**：免费判定只负责把候选模型挑出来，最终可用性由探测决定。
//!   零价但需要登录的模型（`google/lyria-3-*` 401 `PAID_MODEL_AUTH_REQUIRED`）会被筛进来，
//!   探测后归「明确不可用」并留档——这正是要展示给用户的事实。
//! - **明确不可用只以「上游明确拒绝」为充分条件**（404 模型不存在 / 401 需登录 /
//!   地区 403 / 免费层拒绝）。429 限流、5xx、超时、网络错误都归「暂时不可用」，
//!   因为它们的成因与模型本身无关（Kilo 实测 20 个免费模型里 5 个如此）。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

static REGISTRY: OnceLock<Option<Value>> = OnceLock::new();

fn registry() -> Option<&'static Value> {
    REGISTRY
        .get_or_init(|| serde_json::from_str::<Value>(include_str!("../free_sources.json")).ok())
        .as_ref()
}

fn sources() -> Option<&'static Map<String, Value>> {
    registry()?.get("sources")?.as_object()
}

fn reasons() -> Option<&'static Map<String, Value>> {
    registry()?.get("rejectReasons")?.as_object()
}

/// 免密来源的完整定义。
#[derive(Debug, Clone)]
pub struct FreeSource {
    pub id: String,
    pub label: String,
    /// 对应的上游名（写入 `upstreams` 的键）。
    pub upstream: String,
    pub protocol: String,
    /// 传输适配层；空串表示按 protocol。Zen 免密池为 `opencode`。
    pub transport: String,
    pub base_url: String,
    pub chat_path: String,
    /// 免费 id 后缀（zen: `-free`；kilo: `:free`）。
    pub suffixes: Vec<String>,
    /// 免费 id 固定集合（zen: big-pickle / grok-code）。
    pub fixed_ids: Vec<String>,
    /// 目录条目上的免费标记字段（如 `isFree`）。
    pub flags: Vec<String>,
    /// 是否把「零价」也算作免费候选。
    pub zero_price: bool,
    /// 模型目录端点路径。
    pub catalog_path: String,
    /// 探测时的最小 max_tokens（≥512，避免 reasoning 吃满预算导致正文为空）。
    pub min_max_tokens: u64,
    /// 探测提示词（最短可判定内容）。
    pub probe_prompt: String,
    /// 隐私警示级别：info | warn | danger。
    pub privacy_level: String,
    /// 隐私警示正文。
    pub privacy_note: String,
    /// 官方文档地址。
    pub docs_url: String,
}

impl FreeSource {
    /// 该来源是否声明了「免费层传输」（zen 走 opencode 指纹；kilo 走标准免鉴权）。
    pub fn uses_transport(&self) -> bool {
        !self.transport.is_empty()
    }

    /// 鉴权方式：免密来源一律 `none`（不发送任何凭据头）。
    pub fn auth_scheme(&self) -> &'static str {
        "none"
    }

    /// 规范化后的 Base URL：去尾部 `/` 与 `/v1`。
    ///
    /// 为什么必须去 `/v1`：来源定义里的 `baseUrl` 带 `/v1`（如
    /// `https://api.kilo.ai/api/openrouter` 本身不带，但
    /// `https://opencode.ai/zen/v1` 带），而 `chatPath`/`catalogPath`
    /// 也是以 `/v1/...` 开头，直接拼接会得到 `/v1/v1/chat/completions`。
    /// 这里复用配置层的同一套规范化规则（`config::normalize_base_url`），
    /// 避免两处规则漂移。
    pub fn normalized_base_url(&self) -> String {
        crate::config::normalize_base_url(&self.base_url)
    }
}

fn str_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 解析单个来源定义。
fn parse_source(id: &str, entry: &Value) -> Option<FreeSource> {
    let free = entry.get("freeMatch");
    let probe = entry.get("probe");
    let privacy = entry.get("privacy");
    Some(FreeSource {
        id: id.to_string(),
        label: entry
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        upstream: entry
            .get("upstream")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        protocol: entry
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("openai")
            .to_string(),
        transport: entry
            .get("transport")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        base_url: entry
            .get("baseUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        chat_path: entry
            .get("chatPath")
            .and_then(Value::as_str)
            .unwrap_or("/v1/chat/completions")
            .to_string(),
        suffixes: str_list(free.and_then(|f| f.get("suffixes"))),
        fixed_ids: str_list(free.and_then(|f| f.get("ids"))),
        flags: str_list(free.and_then(|f| f.get("flags"))),
        zero_price: free
            .and_then(|f| f.get("zeroPrice"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        catalog_path: entry
            .get("catalog")
            .and_then(|c| c.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("/v1/models")
            .to_string(),
        min_max_tokens: probe
            .and_then(|p| p.get("minMaxTokens"))
            .and_then(Value::as_u64)
            .unwrap_or(512),
        probe_prompt: probe
            .and_then(|p| p.get("prompt"))
            .and_then(Value::as_str)
            .unwrap_or("ping")
            .to_string(),
        privacy_level: privacy
            .and_then(|p| p.get("level"))
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_string(),
        privacy_note: privacy
            .and_then(|p| p.get("note"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        docs_url: entry
            .get("docsUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// 取一个免费来源定义。
pub fn source(id: &str) -> Option<FreeSource> {
    sources()
        .and_then(|s| s.get(id))
        .and_then(|entry| parse_source(id, entry))
}

/// 全部免费来源（注册表顺序）。
pub fn all_sources() -> Vec<FreeSource> {
    sources()
        .map(|s| {
            s.iter()
                .filter_map(|(id, entry)| parse_source(id, entry))
                .collect()
        })
        .unwrap_or_default()
}

/// 全部状态定义（按 order 升序，供面板分组）。
pub fn states() -> Vec<Value> {
    let Some(states) = registry()
        .and_then(|r| r.get("states"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut rows: Vec<(u64, Value)> = states
        .iter()
        .map(|(id, entry)| {
            let order = entry.get("order").and_then(Value::as_u64).unwrap_or(0);
            let mut v = entry.clone();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("id".into(), json!(id));
            }
            (order, v)
        })
        .collect();
    rows.sort_by_key(|(order, _)| *order);
    rows.into_iter().map(|(_, v)| v).collect()
}

/// 一条拒因定义。
#[derive(Debug, Clone)]
pub struct RejectReason {
    /// 拒因码（与 `rejectReasons` 的键一致）。
    pub code: String,
    pub label: String,
    /// 归一状态：available | transient | suspect | region | unavailable。
    /// 这是"拒因 → 状态"的单一事实源，接口层据此输出面板状态。
    pub state: String,
    pub advice: String,
}

/// 按拒因码取定义。
pub fn reject_reason(code: &str) -> Option<RejectReason> {
    let entry = reasons()?.get(code)?;
    Some(RejectReason {
        code: code.to_string(),
        label: entry
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(code)
            .to_string(),
        state: entry
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("transient")
            .to_string(),
        advice: entry
            .get("advice")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// 全部拒因码。
pub fn reject_reason_codes() -> Vec<String> {
    reasons()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default()
}

/// 目录条目的价格是否为零（prompt 与 completion 同时为 0）。
fn price_is_zero(entry: &Value) -> bool {
    let price = entry.get("pricing");
    let read = |key: &str| -> Option<f64> {
        price.and_then(|p| p.get(key)).and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    };
    match (read("prompt"), read("completion")) {
        (Some(p), Some(c)) => p == 0.0 && c == 0.0,
        _ => false,
    }
}

/// 判断一个目录条目是否属于「免费候选」。
///
/// 命中任一即算免费候选：id 后缀 / 固定 id 集合 / 目录标记 / 零价。
/// 判定只做筛选；可用与否由探测决定。
pub fn is_free_entry(source: &FreeSource, id: &str, entry: Option<&Value>) -> bool {
    let name = id.trim();
    if name.is_empty() {
        return false;
    }
    // Zen 侧直接委托 transport 的判定：同一条规则只有一个实现，
    // 避免「注册表说免费、transport 拒绝」这类口径漂移
    // （transport=opencode 只接受 -free / big-pickle / grok-code）。
    if source.uses_transport() && source.transport == "opencode" {
        return crate::opencode_transport::is_free_model(name);
    }
    if source.suffixes.iter().any(|s| name.ends_with(s.as_str())) {
        return true;
    }
    if source.fixed_ids.iter().any(|s| s == name) {
        return true;
    }
    let Some(entry) = entry else {
        return false;
    };
    if source
        .flags
        .iter()
        .any(|flag| entry.get(flag).and_then(Value::as_bool).unwrap_or(false))
    {
        return true;
    }
    if source.zero_price && price_is_zero(entry) {
        return true;
    }
    false
}

/// 从上游目录响应中筛出免费候选模型。
///
/// 返回 `(id, 条目)` 列表，保持上游目录原序。
pub fn filter_free_models(source: &FreeSource, items: &[Value]) -> Vec<(String, Value)> {
    items
        .iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .or_else(|| entry.get("slug"))
                .or_else(|| entry.get("model"))
                .and_then(Value::as_str)?;
            if !is_free_entry(source, id, Some(entry)) {
                return None;
            }
            Some((id.to_string(), entry.clone()))
        })
        .collect()
}

/// 探测结果的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeState {
    Available,
    Transient,
    Suspect,
    Region,
    Unavailable,
}

impl ProbeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProbeState::Available => "available",
            ProbeState::Transient => "transient",
            ProbeState::Suspect => "suspect",
            ProbeState::Region => "region",
            ProbeState::Unavailable => "unavailable",
        }
    }
}

/// 上游错误的归一化结果。
#[derive(Debug, Clone)]
pub struct Rejection {
    pub state: ProbeState,
    pub code: String,
    pub status: u16,
    /// 截断后的上游错误正文（不含任何凭据）。
    pub raw: String,
}

/// 错误正文截断上限：够看清拒因，又不至于把整页 HTML 塞进面板。
const RAW_LIMIT: usize = 400;

/// 归一化上游错误 → 状态 + 拒因码。
///
/// **判据**：只有「上游明确拒绝」才归 `unavailable`；限流 / 5xx / 超时 / 网络错误
/// 都归 `transient`（与模型本身无关，可退避重试）。
///
/// 判定顺序很关键：
/// 1. **先按状态码兜底**（404/401/403/429/5xx）——Kilo 的错误正文是
///    「Provider returned error; code: 429」这种**不含 "rate limit" 字样**的
///    模板句，只看文本会漏判成网关故障。
/// 2. **再用正文里的明确语义细化**（付费模型、地区限制、日配额、免费层拒绝、
///    Cloudflare 拦截、模型已下线）——这些语义比状态码精确。
/// 3. 最后兜底为网关故障。
pub fn classify_error(status: u16, body: &str) -> Rejection {
    let text = body.to_lowercase();
    let raw = truncate(body, RAW_LIMIT);

    // ① 正文里的明确语义优先（比状态码精确）。
    if text.contains("paid_model_auth_required") || text.contains("you need to sign in") {
        return Rejection {
            state: ProbeState::Unavailable,
            code: "paid_model_auth_required".into(),
            status,
            raw,
        };
    }
    if text.contains("not available in your country")
        || text.contains("not available in your region")
        || (text.contains("region") && text.contains("not available"))
    {
        return Rejection {
            state: ProbeState::Region,
            code: "region_blocked".into(),
            status,
            raw,
        };
    }
    if text.contains("model is unavailable")
        || text.contains("model_not_found")
        || text.contains("not found for account")
    {
        return Rejection {
            state: ProbeState::Unavailable,
            code: "model_not_found".into(),
            status,
            raw,
        };
    }
    if text.contains("freetiererror") || text.contains("free tier") {
        return Rejection {
            state: ProbeState::Unavailable,
            code: "free_tier_rejected".into(),
            status,
            raw,
        };
    }
    if text.contains("cloudflare") && (text.contains("1010") || text.contains("blocked")) {
        return Rejection {
            state: ProbeState::Unavailable,
            code: "cloudflare_blocked".into(),
            raw,
            status,
        };
    }
    if text.contains("daily limit reached") || text.contains("limit_rpd") {
        return Rejection {
            state: ProbeState::Transient,
            code: "daily_quota_exhausted".into(),
            status,
            raw,
        };
    }
    // Zen 免密层的额度提示（实测 2026-10-21：本机 IP 额度耗尽时
    // 全部免费模型统一返回该错误，不是单个模型下线）。
    if text.contains("freeusagelimiterror") || text.contains("free usage limit") {
        return Rejection {
            state: ProbeState::Transient,
            code: "rate_limited_upstream".into(),
            status,
            raw,
        };
    }
    if text.contains("rate limit")
        || text.contains("rate-limited")
        || text.contains("upstream_provider_shared_pool")
        || text.contains("temporarily rate-limited")
        || text.contains("at capacity")
    {
        return Rejection {
            state: ProbeState::Transient,
            code: "rate_limited_upstream".into(),
            status,
            raw,
        };
    }

    // ② 状态码兜底。
    match status {
        404 => Rejection {
            state: ProbeState::Unavailable,
            code: "model_not_found".into(),
            status,
            raw,
        },
        401 | 403 => Rejection {
            state: ProbeState::Unavailable,
            code: "free_tier_rejected".into(),
            status,
            raw,
        },
        429 => Rejection {
            state: ProbeState::Transient,
            code: "rate_limited_upstream".into(),
            status,
            raw,
        },
        500..=599 => Rejection {
            state: ProbeState::Transient,
            code: "upstream_timeout".into(),
            status,
            raw,
        },
        400..=499 => Rejection {
            state: ProbeState::Transient,
            code: "gateway_error".into(),
            status,
            raw,
        },
        _ => Rejection {
            state: ProbeState::Transient,
            code: "gateway_error".into(),
            status,
            raw,
        },
    }
}
/// 200 但正文为空：多半是 reasoning 吃满预算，判「疑似可用」而非不可用。
pub fn classify_empty_body() -> Rejection {
    Rejection {
        state: ProbeState::Suspect,
        code: "budget_exhausted".into(),
        status: 200,
        raw: String::new(),
    }
}

/// 网络层失败（连不上）。
pub fn classify_network_error(message: &str) -> Rejection {
    Rejection {
        state: ProbeState::Transient,
        code: "network_error".into(),
        status: 0,
        raw: truncate(message, RAW_LIMIT),
    }
}

/// 探测超时：慢模型（如 Kilo 的 `kilo-auto/free` 实测 68s）会撞上超时上限。
///
/// 这**不是**网络不可达——前者是上游太慢，后者是连都连不上。
/// 两者都归 transient，但给用户的下一步建议不同，因此必须分开。
pub fn classify_timeout() -> Rejection {
    Rejection {
        state: ProbeState::Transient,
        code: "upstream_timeout".into(),
        status: 0,
        raw: String::new(),
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let cleaned = text.trim();
    if cleaned.chars().count() <= limit {
        return cleaned.to_string();
    }
    let head: String = cleaned.chars().take(limit).collect();
    format!("{head}…")
}

/// 退避间隔（分钟）：transient 状态下 30 → 120 分钟指数递增。
pub fn backoff_minutes(attempt: u32) -> u64 {
    const BASE: u64 = 30;
    const MAX: u64 = 120;
    let shift = attempt.min(2);
    (BASE << shift).min(MAX)
}

/// 退避到期判断。
pub fn backoff_active(last_ms: i64, now_ms: i64, attempt: u32) -> bool {
    let wait_ms = backoff_minutes(attempt) as i64 * 60 * 1000;
    now_ms.saturating_sub(last_ms) < wait_ms
}

/// 一键接入的结果。
#[derive(Debug, Clone)]
pub struct AttachOutcome {
    /// 写入后的上游配置（可直接并入 `upstreams`）。
    pub upstream: Value,
    /// 是否写入了模型声明（顶层 `models` 为空时不写）。
    pub models_written: bool,
    /// 需要提示用户的事项。
    pub notices: Vec<String>,
}

/// 生成「一键接入」要写入的上游配置。
///
/// 关键约束（来自 plan.md D6，用户已确认「不进入全局」）：
/// - **不写 apiKey**：免密来源不需要凭据。
/// - **`autoModels: false`**：阻断运行期把上游实时目录并入 `/v1/models`。
/// - **不写顶层 `models`**：免费模型只登记在该来源自己的清单里。
/// - 顶层 `models` 为空时**不写该来源的 models 声明**——因为此时
///   `model_list` 会无条件合并各上游已声明的 models，免密模型会被意外对外暴露。
pub fn build_attach(
    source: &FreeSource,
    models: &[String],
    top_models_empty: bool,
) -> AttachOutcome {
    let mut upstream = Map::new();
    upstream.insert("protocol".into(), json!(source.protocol));
    if !source.transport.is_empty() {
        upstream.insert("transport".into(), json!(source.transport));
    }
    upstream.insert("baseUrl".into(), json!(source.base_url));
    upstream.insert(
        "compat".into(),
        json!({
            "auth": {"scheme": source.auth_scheme()},
            "endpoint": {"chat": source.chat_path, "models": source.catalog_path},
        }),
    );
    upstream.insert("fetchModels".into(), json!(true));
    // 阻断运行期目录合并：免密来源的模型不进入对外全局清单。
    upstream.insert("autoModels".into(), json!(false));
    upstream.insert("default".into(), json!(false));
    // 免密来源不注入会话头：Kilo 网关不校验会话头，Zen 由 transport 接管。
    upstream.insert("sessionHeader".into(), json!(false));

    let mut notices = Vec::new();
    let mut models_written = false;
    if !models.is_empty() {
        if top_models_empty {
            notices.push(
                "顶层 models 为空：此时上游声明的模型会被自动并入对外的 /v1/models，\
                 与「免费模型不进入全局」冲突，因此本次不写入该来源的模型清单；\
                 请先显式配置顶层 models，再重新加入。"
                    .to_string(),
            );
        } else {
            upstream.insert("models".into(), json!(models));
            models_written = true;
        }
    }
    AttachOutcome {
        upstream: Value::Object(upstream),
        models_written,
        notices,
    }
}

/// 探测结果缓存（按 source+model 记录最近一次 transient 探测时间与次数）。
#[derive(Debug, Default)]
pub struct ProbeCache {
    entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Copy)]
struct CacheEntry {
    last_ms: i64,
    attempt: u32,
    state: &'static str,
}

impl ProbeCache {
    /// 记录一次探测结果。
    pub fn record(&mut self, source: &str, model: &str, state: ProbeState, now_ms: i64) {
        let key = format!("{source}\u{1f}{model}");
        let entry = self.entries.entry(key).or_insert(CacheEntry {
            last_ms: now_ms,
            attempt: 0,
            state: "transient",
        });
        if state == ProbeState::Transient {
            entry.attempt = entry.attempt.saturating_add(1);
            entry.last_ms = now_ms;
            entry.state = "transient";
        } else {
            entry.attempt = 0;
            entry.last_ms = now_ms;
            entry.state = state.as_str();
        }
    }

    /// 该模型当前是否处于退避期（transient 且未到下次允许时间）。
    pub fn is_backing_off(&self, source: &str, model: &str, now_ms: i64) -> bool {
        let key = format!("{source}\u{1f}{model}");
        match self.entries.get(&key) {
            Some(entry) if entry.state == "transient" => {
                backoff_active(entry.last_ms, now_ms, entry.attempt.saturating_sub(1))
            }
            _ => false,
        }
    }

    /// 退避剩余分钟数（0 表示不在退避期）。
    pub fn backoff_remaining_minutes(&self, source: &str, model: &str, now_ms: i64) -> u64 {
        let key = format!("{source}\u{1f}{model}");
        match self.entries.get(&key) {
            Some(entry) if entry.state == "transient" => {
                let wait_ms = backoff_minutes(entry.attempt.saturating_sub(1)) * 60 * 1000;
                let elapsed = now_ms.saturating_sub(entry.last_ms).max(0) as u64;
                wait_ms.saturating_sub(elapsed) / 60000
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_parses_both_sources() {
        let all = all_sources();
        assert_eq!(all.len(), 2, "应有两个免费来源");
        let zen = source("zen-free").expect("zen 来源");
        assert_eq!(zen.transport, "opencode");
        assert_eq!(zen.suffixes, vec!["-free"]);
        assert!(zen.fixed_ids.contains(&"big-pickle".to_string()));
        let kilo = source("kilo-free").expect("kilo 来源");
        assert_eq!(kilo.transport, "");
        assert_eq!(kilo.suffixes, vec![":free"]);
        assert!(kilo.zero_price);
        assert_eq!(kilo.privacy_level, "danger");
    }

    #[test]
    fn zen_free_match_matches_transport_rule() {
        // Zen 侧判定必须与 opencode_transport::is_free_model 同口径。
        let zen = source("zen-free").unwrap();
        for id in ["mimo-v2.6-flash-free", "big-pickle", "grok-code"] {
            assert!(
                is_free_entry(&zen, id, None),
                "{id} 应判为免费候选（须与 is_free_model 一致）"
            );
            assert!(
                crate::opencode_transport::is_free_model(id),
                "{id} 应被 transport 接受"
            );
        }
        for id in ["glm-5.3-flash", "deepseek-v4.1-flash", ""] {
            assert!(!is_free_entry(&zen, id, None), "{id} 不应判为免费");
            assert!(
                !crate::opencode_transport::is_free_model(id),
                "{id} 不应被 transport 接受"
            );
        }
    }

    #[test]
    fn kilo_free_match_covers_three_paths() {
        let kilo = source("kilo-free").unwrap();
        // 路径一：id 后缀
        assert!(is_free_entry(
            &kilo,
            "minimax/minimax-m3.1-flash:free",
            None
        ));
        // 路径二：目录标记 isFree
        let flagged = json!({"id": "kilo-auto/free", "isFree": true});
        assert!(is_free_entry(&kilo, "kilo-auto/free", Some(&flagged)));
        // 路径三：零价
        let zero = json!({
            "id": "google/lyria-3-pro-preview",
            "pricing": {"prompt": "0", "completion": "0"}
        });
        assert!(is_free_entry(
            &kilo,
            "google/lyria-3-pro-preview",
            Some(&zero)
        ));
        // 负例：付费模型
        let paid = json!({
            "id": "anthropic/claude-haiku-5.5",
            "pricing": {"prompt": "1", "completion": "2"}
        });
        assert!(!is_free_entry(
            &kilo,
            "anthropic/claude-haiku-5.5",
            Some(&paid)
        ));
    }

    #[test]
    fn classify_error_separates_transient_from_unavailable() {
        // 暂时不可用：与模型本身无关，可退避重试。
        for (status, body, code) in [
            (
                429u16,
                "Provider returned error; code: 429",
                "rate_limited_upstream",
            ),
            (
                429,
                "Rate limit exceeded: limit_rpd/x. Daily limit reached",
                "daily_quota_exhausted",
            ),
            (
                504,
                "Provider returned error; code: 504",
                "upstream_timeout",
            ),
            (
                502,
                "Upstream error from Nvidia: ResourceExhausted",
                "upstream_timeout",
            ),
        ] {
            let r = classify_error(status, body);
            assert_eq!(r.state, ProbeState::Transient, "{body} 应为暂时不可用");
            assert_eq!(r.code, code, "{body} 拒因码不符");
        }
        // 明确不可用：上游明确拒绝。
        for (status, body, code) in [
            (
                401,
                "{\"code\":\"PAID_MODEL_AUTH_REQUIRED\",\"message\":\"You need to sign in to use this model.\"}",
                "paid_model_auth_required",
            ),
            (404, "Provider returned error; code: 404", "model_not_found"),
            (403, "{\"type\":\"FreeTierError\"}", "free_tier_rejected"),
        ] {
            let r = classify_error(status, body);
            assert_eq!(r.state, ProbeState::Unavailable, "{body} 应为明确不可用");
            assert_eq!(r.code, code, "{body} 拒因码不符");
        }
        // 地区受限：单列状态（保留可见但隔离），不与「明确不可用」混为一谈。
        let region = classify_error(403, "not available in your country");
        assert_eq!(region.state, ProbeState::Region);
        assert_eq!(region.code, "region_blocked");
    }

    #[test]
    fn region_state_is_distinct_from_unavailable() {
        let r = classify_error(403, "Model not available in your country");
        assert_eq!(r.state, ProbeState::Region);
        assert_eq!(r.code, "region_blocked");
    }

    #[test]
    fn empty_body_is_suspect_not_unavailable() {
        // 200 但正文为空：疑似 reasoning 吃满预算，不能判为不可用。
        let r = classify_empty_body();
        assert_eq!(r.state, ProbeState::Suspect);
        assert_eq!(r.code, "budget_exhausted");
    }

    #[test]
    fn every_reason_code_resolves_to_known_state() {
        for code in reject_reason_codes() {
            let reason = reject_reason(&code).expect("拒因应可解析");
            assert!(
                states().iter().any(|s| s["id"] == reason.state),
                "拒因 {code} 的状态 {} 未在 states 中定义",
                reason.state
            );
            assert!(!reason.advice.is_empty(), "拒因 {code} 缺少建议");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_minutes(0), 30);
        assert_eq!(backoff_minutes(1), 60);
        assert_eq!(backoff_minutes(2), 120);
        assert_eq!(backoff_minutes(9), 120);
        let now = 1_000_000i64;
        assert!(backoff_active(now, now + 60_000, 0));
        assert!(!backoff_active(now, now + 31 * 60_000, 0));
    }

    /// 超时与网络不可达必须分开：慢模型撞超时不是"连不上"。
    ///
    /// 真实案例（2026-10-21 真机）：探测超时上限为 10s 时，
    /// `nvidia/nemotron-3.5-super-vl:free`（实测 1.8s 可用）被判成
    /// `network_error` —— 因为当时那轮请求排队累积后才响应。
    /// 放宽到 30s 后同一模型立即返回 `available`。
    /// 两者都给 transient，但建议不同：超时说"上游慢/换模型"，
    /// 网络不可达说"查本机网络与代理"。
    #[test]
    fn timeout_is_distinct_from_network_error() {
        let timeout = classify_timeout();
        assert_eq!(timeout.state, ProbeState::Transient);
        assert_eq!(timeout.code, "upstream_timeout");
        let network = classify_network_error("connection refused");
        assert_eq!(network.state, ProbeState::Transient);
        assert_eq!(network.code, "network_error");
        assert_ne!(timeout.code, network.code);
        // 两个拒因码都必须在字典里。
        assert!(reject_reason(&timeout.code).is_some());
        assert!(reject_reason(&network.code).is_some());
    }

    /// 目录样本：免费筛选必须与 fixtures 一致。
    ///
    /// 样本取自 Kilo / Zen 的真实目录响应（2026-10-20 实测），
    /// 覆盖三条判定路径：id 后缀、目录标记、零价。
    #[test]
    fn golden_catalog_cases_match_free_filter() {
        let golden: Value =
            serde_json::from_str(include_str!("../tests/fixtures/free_sources_golden.json"))
                .expect("golden fixtures 可解析");
        let cases = golden
            .get("catalogCases")
            .and_then(Value::as_array)
            .expect("catalogCases");
        assert!(!cases.is_empty(), "catalog fixtures 为空");
        for case in cases {
            let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
            let source_id = case.get("source").and_then(Value::as_str).unwrap_or("");
            let source = source(source_id).unwrap_or_else(|| panic!("未知来源 {source_id}"));
            let items: Vec<Value> = case
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let got: Vec<String> = filter_free_models(&source, &items)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let expect: Vec<String> = case
                .get("expectFree")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(got, expect, "case {name}: 免费筛选与 fixtures 不一致");
        }
    }

    /// 拒因样本：同一份错误正文必须稳定归入同一拒因码与状态。
    #[test]
    fn golden_reject_cases_match_classifier() {
        let golden: Value =
            serde_json::from_str(include_str!("../tests/fixtures/free_sources_golden.json"))
                .expect("golden fixtures 可解析");
        let cases = golden
            .get("rejectCases")
            .and_then(Value::as_array)
            .expect("rejectCases");
        assert!(!cases.is_empty(), "reject fixtures 为空");
        for case in cases {
            let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = case.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
            let body = case.get("body").and_then(Value::as_str).unwrap_or("");
            let expect_code = case.get("expectCode").and_then(Value::as_str).unwrap_or("");
            let expect_state = case
                .get("expectState")
                .and_then(Value::as_str)
                .unwrap_or("");
            let rejection = classify_error(status, body);
            assert_eq!(rejection.code, expect_code, "case {name}: 拒因码不符");
            assert_eq!(
                rejection.state.as_str(),
                expect_state,
                "case {name}: 状态不符"
            );
            // 拒因码必须在字典里，且字典状态与分类器一致。
            let reason = reject_reason(&rejection.code)
                .unwrap_or_else(|| panic!("case {name}: 拒因 {} 不在字典中", rejection.code));
            assert_eq!(
                reason.state,
                rejection.state.as_str(),
                "case {name}: 状态漂移"
            );
        }
    }

    #[test]
    fn probe_cache_backs_off_only_for_transient() {
        let mut cache = ProbeCache::default();
        let now = 1_000_000i64;
        cache.record("kilo-free", "a:free", ProbeState::Transient, now);
        assert!(cache.is_backing_off("kilo-free", "a:free", now + 60_000));
        assert!(cache.backoff_remaining_minutes("kilo-free", "a:free", now) > 0);
        // 探测通过后立即解除退避。
        cache.record("kilo-free", "a:free", ProbeState::Available, now + 60_000);
        assert!(!cache.is_backing_off("kilo-free", "a:free", now + 60_000));
        assert_eq!(
            cache.backoff_remaining_minutes("kilo-free", "a:free", now + 60_000),
            0
        );
    }

    #[test]
    fn attach_never_writes_credentials() {
        let kilo = source("kilo-free").unwrap();
        let out = build_attach(&kilo, &["minimax/minimax-m3.1-flash:free".into()], false);
        let up = &out.upstream;
        assert!(up.get("apiKey").is_none(), "免密来源不得写入 apiKey");
        assert!(up.get("apiKeyEnv").is_none(), "免密来源不得写入 apiKeyEnv");
        assert_eq!(up["compat"]["auth"]["scheme"], "none");
    }

    #[test]
    fn attach_blocks_global_catalog_merge() {
        let kilo = source("kilo-free").unwrap();
        let out = build_attach(&kilo, &["minimax/minimax-m3.1-flash:free".into()], false);
        // 条件 A：autoModels=false 阻断运行期目录合并。
        assert_eq!(out.upstream["autoModels"], json!(false));
        // 条件 B：顶层 models 非空时不写顶层 models，只写上游自己的清单。
        assert!(out.models_written);
        assert_eq!(out.upstream["models"][0], "minimax/minimax-m3.1-flash:free");
    }

    #[test]
    fn attach_skips_models_when_top_level_empty() {
        // 顶层 models 为空时，model_list 会合并各上游声明的模型；
        // 此时写上游 models 会让免费模型意外对外暴露，因此不写并提示。
        let kilo = source("kilo-free").unwrap();
        let out = build_attach(&kilo, &["minimax/minimax-m3.1-flash:free".into()], true);
        assert!(!out.models_written);
        assert!(out.upstream.get("models").is_none(), "不应写上游 models");
        assert!(!out.notices.is_empty(), "应提示用户配置顶层 models");
    }

    #[test]
    fn attach_sets_transport_for_zen() {
        let zen = source("zen-free").unwrap();
        let out = build_attach(&zen, &["big-pickle".into()], false);
        assert_eq!(out.upstream["transport"], "opencode");
        assert_eq!(out.upstream["protocol"], "openai");
        // Zen 免密层由 transport 接管会话头，不走普通会话头注入。
        assert_eq!(out.upstream["sessionHeader"], json!(false));
    }
}
