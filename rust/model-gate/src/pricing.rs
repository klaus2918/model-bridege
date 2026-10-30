//! 单价表：按「上游 + 模型」匹配的每百万 token 单价，支持峰谷双价与美元/人民币换算。
//!
//! 设计要点（与面板「单价表（费用估算）」编辑器对齐）：
//! - `pricing` 是配置里的一个顶层 JSON 对象；`entries` 为**有序数组，先匹配先用**，
//!   支持模型通配（`deepseek-*`）与上游限定，从而覆盖「不同上游同模型价不同」的场景；
//! - `type: "per-token"` 按量计费；`type: "quota"` 只标记额度/订阅制（如 Go 订阅、
//!   zen 免费池、CommandCode 套餐），不产生费用但让面板显示「额度制」而不是「未配置单价」；
//! - 单价可用 USD 或 CNY 填写，落库成本**统一换算为 CNY**（`fx.USD` 为 1 USD 兑人民币）；
//! - 峰谷：`peak.windows` 为谷/峰窗口（本地时区分钟），窗口内用基础价，
//!   窗口外按 `offPeak.multiplier` 或绝对价折算。
//!
//! 解析永不失败：非法条目被跳过并记入 `issues`（面板诊断以 warn 展示），
//! 保证一条坏单价不会影响代理主链路。

use serde_json::{json, Map, Value};

/// 当前单价表结构版本（写入 `pricing.version`，便于将来迁移）。
pub const PRICING_VERSION: i64 = 1;

/// 允许的计价币种：美元按 `fx.USD` 换算成人民币落库。
pub const CURRENCIES: &[&str] = &["USD", "CNY"];

#[derive(Debug, Clone, Copy, PartialEq)]
struct PeakWindow {
    /// 起止分钟数（本地时区的一天内）；`end <= start` 表示跨午夜窗口。
    start_min: i32,
    end_min: i32,
}

impl PeakWindow {
    fn contains(&self, minute: i32) -> bool {
        if self.end_min > self.start_min {
            minute >= self.start_min && minute < self.end_min
        } else {
            // 跨午夜：如 22:00-06:00 → [22:00,24:00) ∪ [00:00,06:00)
            minute >= self.start_min || minute < self.end_min
        }
    }
}

/// 峰谷策略：窗口内为「峰时」（用基础单价），窗口外为「谷时」。
#[derive(Debug, Clone)]
pub struct PeakPolicy {
    /// 本地时区相对 UTC 的偏移分钟（东八区 = 480）。默认 +08:00。
    tz_offset_minutes: i32,
    windows: Vec<PeakWindow>,
    /// 仅周一至周五为峰时；周末整天按谷时（对齐 DeepSeek 官方口径）。
    weekdays_only: bool,
    /// 谷时折扣：优先用绝对价（`off_peak`），否则乘数。
    off_peak_multiplier: f64,
    off_peak: Option<RateSet>,
}

impl PeakPolicy {
    /// 判断时间戳是否处于「峰时」。`ts` 为 Unix 秒。
    fn is_peak(&self, ts: i64) -> bool {
        let local = ts + (self.tz_offset_minutes as i64) * 60;
        let days = local.div_euclid(86_400);
        let minute = (local.rem_euclid(86_400) / 60) as i32;
        // 1970-01-01 是周四：(days+4) 对齐 0=周日、1=周一 … 5=周五、6=周六。
        let weekday = (days + 4).rem_euclid(7);
        if self.weekdays_only && !(1..=5).contains(&weekday) {
            return false;
        }
        if self.windows.is_empty() {
            // 没写窗口 = 无峰谷区分，基础价全局生效。
            return true;
        }
        self.windows.iter().any(|w| w.contains(minute))
    }
}

/// 一组每百万 token 单价（条目币种）。
#[derive(Debug, Clone, Copy, Default)]
pub struct RateSet {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

impl RateSet {
    fn scale(&self, factor: f64) -> RateSet {
        RateSet {
            input: self.input * factor,
            output: self.output * factor,
            cache_read: self.cache_read.map(|v| v * factor),
            cache_write: self.cache_write.map(|v| v * factor),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    PerToken {
        rates: RateSet,
        peak: Option<PeakPolicy>,
    },
    Quota {
        plan: String,
    },
}

#[derive(Debug, Clone)]
pub struct PricingEntry {
    /// 模型匹配模式：支持 `*` 通配；空 = 任意模型。
    pub model: String,
    /// 上游名精确匹配；空 = 任意上游。
    pub upstream: String,
    /// 展示用标签（如「DeepSeek 官方」「Go 订阅」）。
    pub label: String,
    pub currency: String,
    pub kind: EntryKind,
}

impl PricingEntry {
    fn model_matches(&self, model: &str) -> bool {
        if self.model.is_empty() {
            return true;
        }
        glob_match(&self.model, model)
    }

    fn upstream_matches(&self, upstream: &str) -> bool {
        self.upstream.is_empty() || self.upstream.eq_ignore_ascii_case(upstream)
    }
}

/// 极简通配符匹配：`*` 匹配任意字符序列，其余按大小写不敏感全等。
/// 在 `Vec<char>` 上匹配，避免大小写转换后字节切片越界。
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim();
    let text = text.trim();
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(text);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let segs: Vec<Vec<char>> = pattern
        .to_lowercase()
        .split('*')
        .map(|s| s.chars().collect())
        .collect();
    let mut pos = 0usize;
    let first = &segs[0];
    if !first.is_empty() {
        if t.len() < first.len() || t[..first.len()] != first[..] {
            return false;
        }
        pos = first.len();
    }
    let last = &segs[segs.len() - 1];
    if !last.is_empty() {
        if t.len() < last.len() || t[t.len() - last.len()..] != last[..] {
            return false;
        }
    }
    let tail_start = t.len().saturating_sub(last.len());
    if tail_start < pos {
        return false; // 头尾字面段在短文本上重叠（如 "a*a" 匹配 "a"）
    }
    for seg in &segs[1..segs.len() - 1] {
        if seg.is_empty() {
            continue;
        }
        let window = &t[pos..tail_start];
        match window.windows(seg.len()).position(|w| w == seg) {
            Some(offset) => pos += offset + seg.len(),
            None => return false,
        }
    }
    true
}

/// 解析后的单价表。`issues` 收集被跳过的非法条目（面板诊断展示）。
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    pub fx_usd: f64,
    pub entries: Vec<PricingEntry>,
    pub issues: Vec<Value>,
}

impl PricingTable {
    pub fn configured(&self) -> bool {
        !self.entries.is_empty()
    }

    /// 解析 `pricing` 配置节点。`None`/空对象 → 空表。
    pub fn from_value(value: &Value) -> Self {
        let mut table = PricingTable {
            fx_usd: 7.2,
            entries: Vec::new(),
            issues: Vec::new(),
        };
        let Some(obj) = value.as_object() else {
            if !value.is_null() {
                table.issue(value, "", "pricing 必须是 JSON 对象，已忽略整段配置");
            }
            return table;
        };
        table.fx_usd = obj
            .get("fx")
            .and_then(|f| f.get("USD"))
            .and_then(Value::as_f64)
            .filter(|v| *v > 0.0)
            .unwrap_or(7.2);
        let entries = obj
            .get("entries")
            .or_else(|| obj.get("rules"))
            .and_then(Value::as_array);
        let Some(entries) = entries else {
            if obj.get("entries").is_some() || obj.get("rules").is_some() {
                table.issue(value, "entries", "entries 必须是数组，未加载任何单价");
            }
            return table;
        };
        for (idx, item) in entries.iter().enumerate() {
            match parse_entry(item) {
                Ok(entry) => table.entries.push(entry),
                Err(message) => {
                    let path = format!("entries[{idx}]");
                    table.issue(item, &path, &message);
                }
            }
        }
        table
    }

    fn issue(&mut self, item: &Value, path: &str, message: &str) {
        self.issues.push(json!({
            "level": "warn",
            "code": "pricing_entry_skipped",
            "path": format!("pricing.{path}"),
            "message": message,
            "hint": item.to_string(),
        }));
    }

    /// 按「上游 + 模型」解析条目：有序数组先匹配先用。
    /// 请求名与上游实际模型名都参与匹配（别名/映射也能命中）。
    pub fn resolve(&self, upstream: &str, requested: &str, target: &str) -> Option<&PricingEntry> {
        self.entries.iter().find(|e| {
            e.upstream_matches(upstream) && (e.model_matches(requested) || e.model_matches(target))
        })
    }

    /// 面板「单价表」展示用：某模型（任一上游）的报价概要。
    pub fn quote(&self, upstream: &str, model: &str) -> Option<Value> {
        let entry = self.resolve(upstream, model, model)?;
        Some(self.quote_of(entry))
    }

    fn quote_of(&self, entry: &PricingEntry) -> Value {
        match &entry.kind {
            EntryKind::Quota { plan } => json!({
                "type": "quota",
                "plan": plan,
                "label": entry.label,
                "currency": entry.currency,
            }),
            EntryKind::PerToken { rates, peak } => {
                let factor = self.currency_factor(&entry.currency);
                let peak_json = peak.as_ref().map(|p| {
                    json!({
                        "tzOffsetMinutes": p.tz_offset_minutes,
                        "weekdaysOnly": p.weekdays_only,
                        "windows": p.windows.iter().map(|w| json!({
                            "start": minutes_to_hhmm(w.start_min),
                            "end": minutes_to_hhmm(w.end_min),
                        })).collect::<Vec<_>>(),
                        "offPeakMultiplier": if p.off_peak.is_none() { json!(p.off_peak_multiplier) } else { Value::Null },
                        "offPeakRates": p.off_peak.map(|r| rates_json(&r)),
                    })
                });
                json!({
                    "type": "per-token",
                    "label": entry.label,
                    "currency": entry.currency,
                    "input": round4(rates.input * factor),
                    "output": round4(rates.output * factor),
                    "cacheRead": rates.cache_read.map(|v| round4(v * factor)),
                    "cacheWrite": rates.cache_write.map(|v| round4(v * factor)),
                    "peak": peak_json,
                })
            }
        }
    }

    fn currency_factor(&self, currency: &str) -> f64 {
        match currency.to_ascii_uppercase().as_str() {
            "USD" => self.fx_usd,
            _ => 1.0,
        }
    }

    /// 计算一次请求的费用（人民币）。`ts` 为请求时间戳（Unix 秒），用于峰谷判定。
    #[allow(clippy::too_many_arguments)]
    pub fn cost_for(
        &self,
        upstream: &str,
        requested: &str,
        target: &str,
        ts: i64,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read: i64,
        cache_write: i64,
    ) -> f64 {
        let Some(entry) = self.resolve(upstream, requested, target) else {
            return 0.0;
        };
        let EntryKind::PerToken { rates, peak } = &entry.kind else {
            return 0.0; // 额度制不计费
        };
        let scaled = rates.scale(peak.as_ref().map(|p| p.off_peak_multiplier).unwrap_or(1.0));
        let effective = match peak {
            Some(policy) if !policy.is_peak(ts) => match &policy.off_peak {
                Some(off) => off,
                None => &scaled,
            },
            _ => rates,
        };
        let cache_read_price = effective.cache_read.unwrap_or(effective.input);
        let cache_write_price = effective.cache_write.unwrap_or(effective.input);
        let uncached_in = (prompt_tokens - cache_read).max(0);
        let raw = (uncached_in as f64 * effective.input
            + cache_read.max(0) as f64 * cache_read_price
            + cache_write.max(0) as f64 * cache_write_price
            + completion_tokens.max(0) as f64 * effective.output)
            / 1_000_000.0;
        raw * self.currency_factor(&entry.currency)
    }
}

fn rates_json(rates: &RateSet) -> Value {
    json!({
        "input": rates.input,
        "output": rates.output,
        "cacheRead": rates.cache_read,
        "cacheWrite": rates.cache_write,
    })
}

fn parse_hhmm(text: &str) -> Option<i32> {
    let (h, m) = text.trim().split_once(':')?;
    let h: i32 = h.trim().parse().ok()?;
    let m: i32 = m.trim().parse().ok()?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

fn minutes_to_hhmm(min: i32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn parse_entry(item: &Value) -> Result<PricingEntry, String> {
    let obj = item
        .as_object()
        .ok_or_else(|| "单价条目必须是 JSON 对象".to_string())?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let upstream = obj
        .get("upstream")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if model.is_empty() && upstream.is_empty() {
        return Err(
            "条目缺少 model 与 upstream，会被当成匹配一切的兜底条目；请显式填写 model（如 \"*\"）"
                .into(),
        );
    }
    let label = obj
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let currency = obj
        .get("currency")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "CNY".into());
    if !CURRENCIES.contains(&currency.as_str()) {
        return Err(format!("不支持的币种 {currency}，可用：USD / CNY"));
    }
    let kind_name = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("per-token")
        .trim()
        .to_ascii_lowercase();
    let kind = match kind_name.as_str() {
        "quota" | "subscription" | "额度" => EntryKind::Quota {
            plan: obj
                .get("plan")
                .and_then(Value::as_str)
                .unwrap_or("额度/订阅")
                .to_string(),
        },
        "per-token" | "token" | "" => {
            let rates = parse_rates(obj)?;
            let peak = match obj.get("peak") {
                None | Some(Value::Null) => None,
                Some(p) => Some(parse_peak(p).map_err(|e| format!("peak: {e}"))?),
            };
            EntryKind::PerToken { rates, peak }
        }
        other => return Err(format!("未知 type \"{other}\"，可用：per-token / quota")),
    };
    Ok(PricingEntry {
        model,
        upstream,
        label,
        currency,
        kind,
    })
}

fn parse_rates(obj: &Map<String, Value>) -> Result<RateSet, String> {
    let get = |key: &str| obj.get(key).and_then(Value::as_f64);
    let input = get("input").unwrap_or(0.0);
    let output = get("output").unwrap_or(0.0);
    for (name, value) in [("input", input), ("output", output)] {
        if value < 0.0 {
            return Err(format!("{name} 单价不能为负数"));
        }
    }
    let cache_read = get("cacheRead");
    let cache_write = get("cacheWrite");
    if cache_read.is_some_and(|v| v < 0.0) || cache_write.is_some_and(|v| v < 0.0) {
        return Err("缓存单价不能为负数".into());
    }
    Ok(RateSet {
        input,
        output,
        cache_read,
        cache_write,
    })
}

fn parse_peak(value: &Value) -> Result<PeakPolicy, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "peak 必须是 JSON 对象".to_string())?;
    let tz_offset_minutes = obj
        .get("tzOffsetMinutes")
        .and_then(Value::as_i64)
        .unwrap_or(480)
        .clamp(-720, 840) as i32;
    let weekdays_only = obj
        .get("weekdaysOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut windows = Vec::new();
    match obj.get("windows").and_then(Value::as_array) {
        Some(list) => {
            for (idx, w) in list.iter().enumerate() {
                let Some(wobj) = w.as_object() else {
                    return Err(format!("windows[{idx}] 必须是对象"));
                };
                let start = wobj
                    .get("start")
                    .and_then(Value::as_str)
                    .and_then(parse_hhmm)
                    .ok_or_else(|| format!("windows[{idx}].start 需要 \"HH:MM\" 格式"))?;
                let end = wobj
                    .get("end")
                    .and_then(Value::as_str)
                    .and_then(parse_hhmm)
                    .ok_or_else(|| format!("windows[{idx}].end 需要 \"HH:MM\" 格式"))?;
                windows.push(PeakWindow {
                    start_min: start,
                    end_min: end,
                });
            }
        }
        None => return Err("peak.windows 必须是数组".into()),
    }
    let (off_peak_multiplier, off_peak) = match obj.get("offPeak") {
        None | Some(Value::Null) => (0.5, None),
        Some(off) => {
            let offobj = off
                .as_object()
                .ok_or_else(|| "offPeak 必须是对象（multiplier 或绝对价）".to_string())?;
            match offobj.get("multiplier").and_then(Value::as_f64) {
                Some(m) => {
                    if !(0.0..=1.0).contains(&m) {
                        return Err("offPeak.multiplier 需在 0 与 1 之间".into());
                    }
                    (m, None)
                }
                None => {
                    let rates = parse_rates(offobj).map_err(|e| format!("offPeak: {e}"))?;
                    (1.0, Some(rates))
                }
            }
        }
    };
    Ok(PeakPolicy {
        tz_offset_minutes,
        windows,
        weekdays_only,
        off_peak_multiplier,
        off_peak,
    })
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(v: Value) -> PricingTable {
        PricingTable::from_value(&v)
    }

    fn deepseek_table() -> PricingTable {
        table(json!({
            "version": 1,
            "fx": { "USD": 7.0 },
            "entries": [{
                "model": "deepseek-*",
                "upstream": "cch",
                "label": "DeepSeek 官方",
                "type": "per-token",
                "currency": "USD",
                "input": 0.30,
                "output": 1.20,
                "cacheRead": 0.006,
                "peak": {
                    "tzOffsetMinutes": 480,
                    "weekdaysOnly": true,
                    "windows": [ {"start": "09:00", "end": "12:00"}, {"start": "14:00", "end": "18:00"} ],
                    "offPeak": { "multiplier": 0.5 }
                }
            }]
        }))
    }

    #[test]
    fn empty_and_invalid_configs_are_tolerated() {
        assert!(!table(json!(null)).configured());
        assert!(!table(json!({})).configured());
        let bad = table(json!({ "entries": "nope" }));
        assert!(!bad.configured());
        assert_eq!(bad.issues.len(), 1);
        // 非法条目被跳过，合法条目保留
        let mixed = table(json!({ "entries": [
            { "model": "a", "input": -1.0, "output": 1.0 },
            { "model": "b", "output": 2.0 }
        ]}));
        assert_eq!(mixed.entries.len(), 1);
        assert_eq!(mixed.entries[0].model, "b");
        assert_eq!(mixed.issues.len(), 1);
    }

    #[test]
    fn match_precedence_upstream_scoped_first() {
        let t = table(json!({
            "fx": { "USD": 7.0 },
            "entries": [
                { "model": "deepseek-v4-flash", "upstream": "go", "type": "quota", "plan": "Go 订阅" },
                { "model": "deepseek-*", "input": 1.0, "output": 2.0 },
                { "model": "*", "input": 9.0, "output": 9.0 }
            ]
        }));
        // 精确 + 上游限定 → 额度制
        assert!(matches!(
            t.resolve("go", "deepseek-v4-flash", "deepseek-v4-flash")
                .unwrap()
                .kind,
            EntryKind::Quota { .. }
        ));
        // 同模型、别的上游 → 通配 per-token
        assert!(matches!(
            t.resolve("cch", "deepseek-v4-flash", "deepseek-v4-flash")
                .unwrap()
                .kind,
            EntryKind::PerToken { .. }
        ));
        // 未知模型 → 兜底 "*"
        assert!(t.resolve("zen", "whatever", "whatever").is_some());
        // 上游模型名（映射后的目标）也能命中
        assert!(t.resolve("cch", "alias-name", "deepseek-chat").is_some());
    }

    #[test]
    fn cost_uses_cache_split_and_currency_conversion() {
        let t = deepseek_table();
        // 周三 10:00 北京时间（峰时）：100 万未命中输入 + 100 万缓存读 + 100 万输出
        let peak_ts = utc_ts(2026, 10, 28, 2, 0);
        let cost = t.cost_for(
            "cch",
            "deepseek-chat",
            "deepseek-chat",
            peak_ts,
            2_000_000,
            1_000_000,
            1_000_000,
            0,
        );
        // (1_000_000 * 0.30 + 1_000_000 * 0.006 + 1_000_000 * 1.20) / 1e6 = 1.506 USD → ×7
        assert!((cost - 1.506 * 7.0).abs() < 1e-9);
    }

    #[test]
    fn off_peak_halves_the_price() {
        let t = deepseek_table();
        // 周三 20:00 北京时间（窗口外，谷时）
        let ts = utc_ts(2026, 10, 28, 12, 0); // 2026-10-28 20:00 +08
        let off = t.cost_for(
            "cch",
            "deepseek-chat",
            "deepseek-chat",
            ts,
            1_000_000,
            0,
            0,
            0,
        );
        assert!((off - 0.15 * 7.0).abs() < 1e-9);
        // 同日 10:00 北京时间（在 09:00-12:00 窗口内，峰时）
        let peak_ts = utc_ts(2026, 10, 28, 2, 0);
        let peak = t.cost_for(
            "cch",
            "deepseek-chat",
            "deepseek-chat",
            peak_ts,
            1_000_000,
            0,
            0,
            0,
        );
        assert!((peak - 0.30 * 7.0).abs() < 1e-9);
    }

    #[test]
    fn weekends_are_always_off_peak() {
        let t = deepseek_table();
        // 2026-10-31 是周六，10:00 北京时间虽在窗口内也按谷时
        let ts = utc_ts(2026, 10, 31, 2, 0);
        let cost = t.cost_for(
            "cch",
            "deepseek-chat",
            "deepseek-chat",
            ts,
            1_000_000,
            0,
            0,
            0,
        );
        assert!((cost - 0.15 * 7.0).abs() < 1e-9);
    }

    #[test]
    fn peak_window_supports_absolute_off_peak_rates_and_wrap() {
        let t = table(json!({
            "entries": [{
                "model": "glm-*", "currency": "CNY",
                "input": 2.0, "output": 8.0,
                "peak": {
                    "windows": [ {"start": "22:00", "end": "06:00"} ],
                    "offPeak": { "input": 1.0, "output": 4.0 }
                }
            }]
        }));
        // 2026-10-30 01:00 北京时间：在跨午夜窗口内 → 峰价
        let in_window = utc_ts(2026, 10, 29, 17, 0);
        assert!(
            (t.cost_for("", "glm-4.6", "glm-4.6", in_window, 1_000_000, 0, 0, 0) - 2.0).abs()
                < 1e-9
        );
        // 同日 12:00 → 谷时绝对价
        let out_window = utc_ts(2026, 10, 30, 4, 0);
        assert!(
            (t.cost_for("", "glm-4.6", "glm-4.6", out_window, 1_000_000, 0, 0, 0) - 1.0).abs()
                < 1e-9
        );
    }

    #[test]
    fn quota_entries_cost_nothing() {
        let t = table(json!({
            "entries": [
                { "model": "mimo-*", "upstream": "go", "type": "quota", "plan": "Go 订阅" },
                { "model": "*", "upstream": "zen", "type": "quota", "plan": "免费额度" }
            ]
        }));
        let cost = t.cost_for(
            "go",
            "mimo-v2.6-flash",
            "mimo-v2.6-flash",
            0,
            5_000_000,
            5_000_000,
            0,
            0,
        );
        assert_eq!(cost, 0.0);
        let quote = t.quote("go", "mimo-v2.6-flash").unwrap();
        assert_eq!(quote["type"], "quota");
        assert_eq!(quote["plan"], "Go 订阅");
    }

    #[test]
    fn glob_patterns_match_families() {
        assert!(glob_match("deepseek-*", "deepseek-v4-flash"));
        assert!(glob_match("deepseek-*", "DeepSeek-V4-Pro"));
        assert!(!glob_match("deepseek-*", "glm-4.6"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("glm-4*", "glm-4.6"));
        assert!(glob_match("kimi-k2.7*", "kimi-k2.7-code"));
        assert!(!glob_match("a*b*c", "aXb")); // 中段缺失
        assert!(glob_match("a*b*c", "aXbYc"));
    }

    #[test]
    fn missing_fx_falls_back_to_default_rate() {
        let t = table(
            json!({ "entries": [{ "model": "gpt-*", "currency": "USD", "input": 1.0, "output": 0.0 }] }),
        );
        let cost = t.cost_for("", "gpt-5.2", "gpt-5.2", 0, 1_000_000, 0, 0, 0);
        assert!((cost - t.fx_usd).abs() < 1e-9);
    }

    /// 构造指定 UTC 时间的 Unix 时间戳（测试用，避免依赖时区库）。
    fn utc_ts(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        // days from civil (Howard Hinnant 算法)
        let y = year as i64 - if month <= 2 { 1 } else { 0 };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (month as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60
    }
}
