//! ZCode billing/claim：把已验证的 `billing/preview` + `billing/claim` 语义
//! 落到 Model-Gate。请求头/业务码与 `zcode2api` 对齐，但不复制其代码。
//!
//! 这里只做 HTTP 与数据归一化；验证码由 `captcha` 模块提供，账号状态由 `store` 落库。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};

use super::captcha::CaptchaToken;
use super::store::{ZcodeAccount, STATUS_EXHAUSTED, STATUS_INVALID};

const BILLING_BASE: &str = "https://zcode.z.ai/api/v1/zcode-plan";
const MARKETING_TOUCH: &str = "https://zcode.z.ai/api/v1/marketing/touch";
/// 营销活动接口的防缓存序号；与客户端一致，进程内递增即可。
static MARKETING_SEQ: AtomicU64 = AtomicU64::new(0);
/// 与已安装 ZCode 客户端保持同一版本口径；领取资格与客户端版本有关。
const APP_VERSION: &str = "3.17.2";
const RELEASE_CHANNEL: &str = "production";

#[derive(Debug, Clone)]
pub struct Plan {
    pub plan_id: String,
    pub name: String,
    pub priority: i64,
    pub grants: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ClaimOutcome {
    pub plan_id: String,
    pub plan_name: String,
    pub code: Option<i64>,
    pub ok: bool,
    pub next_at: Option<i64>,
    pub message: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum BillingError {
    Network(String),
    Http(u16, String),
    Business {
        code: i64,
        message: String,
        next_at: Option<i64>,
    },
    Auth(String),
}

impl BillingError {
    pub fn message(&self) -> String {
        match self {
            BillingError::Network(m) => format!("billing 网络错误：{m}"),
            BillingError::Http(status, body) => format!("billing HTTP {status}：{body}"),
            BillingError::Business { code, message, .. } => {
                format!("billing 业务失败({code})：{message}")
            }
            BillingError::Auth(m) => m.clone(),
        }
    }
}

pub async fn preview(client: &Client, account: &ZcodeAccount) -> Result<Vec<Plan>, BillingError> {
    let response = client
        .get(format!(
            "{BILLING_BASE}/billing/preview?app_version={APP_VERSION}&platform={}",
            profile_platform(account)
        ))
        .headers(headers(account, None)?)
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|err| BillingError::Network(err.to_string()))?;
    let status = response.status().as_u16();
    let body: Value = response
        .json()
        .await
        .map_err(|err| BillingError::Network(format!("响应不是 JSON：{err}")))?;
    if status == 401 || status == 403 {
        return Err(BillingError::Auth("ZCode 登录失效，请重新授权".to_string()));
    }
    let code = business_code(&body);
    if code != 0 {
        return Err(BillingError::Business {
            code,
            message: body
                .get("msg")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            next_at: next_at(&body),
        });
    }
    let plans = body
        .get("data")
        .and_then(|d| d.get("plans"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_plan).collect())
        .unwrap_or_default();
    Ok(plans)
}

/// 已生效套餐（含余额口径的 Start Plan）；仅用于面板展示，不参与 claim 选择。
pub async fn balance_plans(
    client: &Client,
    account: &ZcodeAccount,
) -> Result<Vec<Plan>, BillingError> {
    let response = client
        .get(format!(
            "{BILLING_BASE}/billing/balance?app_version={APP_VERSION}&platform={}",
            profile_platform(account)
        ))
        .headers(headers(account, None)?)
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|err| BillingError::Network(err.to_string()))?;
    let status = response.status().as_u16();
    let body: Value = response
        .json()
        .await
        .map_err(|err| BillingError::Network(format!("响应不是 JSON：{err}")))?;
    if status == 401 || status == 403 {
        return Err(BillingError::Auth("ZCode 登录失效，请重新授权".to_string()));
    }
    let code = business_code(&body);
    if code != 0 {
        return Err(BillingError::Business {
            code,
            message: body
                .get("msg")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            next_at: None,
        });
    }
    let plans = body
        .get("data")
        .and_then(|d| d.get("plans"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_plan).collect())
        .unwrap_or_default();
    Ok(plans)
}

/// 从营销活动（`marketing/touch`）里找当前可领取的 Start Plan。
///
/// `billing/preview` 为空时，活动卡片仍可能带 `claim_zcode_plan` 动作；它的
/// `args.plan_id` 才是领取接口接受的 campaign plan（例如 `...-1103`），
/// 客户端配置里的 `zcode-v3-start-plan` 只是展示占位。
pub async fn marketing_claim_plan(
    client: &Client,
    account: &ZcodeAccount,
) -> Result<Option<Plan>, BillingError> {
    let seq = MARKETING_SEQ.fetch_add(1, Ordering::Relaxed);
    let response = client
        .get(format!("{MARKETING_TOUCH}?seq={seq}"))
        .headers(headers(account, None)?)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|err| BillingError::Network(err.to_string()))?;
    let status = response.status().as_u16();
    let body: Value = response
        .json()
        .await
        .map_err(|err| BillingError::Network(format!("响应不是 JSON：{err}")))?;
    if status == 401 || status == 403 {
        return Err(BillingError::Auth("ZCode 登录失效，请重新授权".to_string()));
    }
    let code = business_code(&body);
    if code != 0 {
        return Err(BillingError::Business {
            code,
            message: body
                .get("msg")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            next_at: None,
        });
    }
    let root = body.get("data").unwrap_or(&body);
    let Some(plan_id) = find_claim_plan_id(root) else {
        return Ok(None);
    };
    Ok(Some(find_zcode_plan(root, &plan_id).unwrap_or(Plan {
        plan_id,
        name: "Start Plan".to_string(),
        priority: 0,
        grants: Vec::new(),
    })))
}

/// 深度查找 `claim_zcode_plan` 动作里的 plan_id。
fn find_claim_plan_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(action) = map.get("action") {
                if action.get("type").and_then(Value::as_str) == Some("claim_zcode_plan") {
                    if let Some(plan_id) = action
                        .get("args")
                        .and_then(|args| args.get("plan_id"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    {
                        return Some(plan_id.to_string());
                    }
                }
            }
            map.values().find_map(find_claim_plan_id)
        }
        Value::Array(items) => items.iter().find_map(find_claim_plan_id),
        _ => None,
    }
}

/// 深度查找与 plan_id 对应的 `zcode_plan` 展示对象。
fn find_zcode_plan(value: &Value, plan_id: &str) -> Option<Plan> {
    match value {
        Value::Object(map) => {
            if let Some(plan) = map.get("zcode_plan") {
                if let Some(parsed) = parse_plan(plan) {
                    if parsed.plan_id == plan_id {
                        return Some(parsed);
                    }
                }
            }
            map.values().find_map(|item| find_zcode_plan(item, plan_id))
        }
        Value::Array(items) => items.iter().find_map(|item| find_zcode_plan(item, plan_id)),
        _ => None,
    }
}

pub async fn claim(
    client: &Client,
    account: &ZcodeAccount,
    captcha: &CaptchaToken,
    plan_id: Option<&str>,
) -> Result<ClaimOutcome, BillingError> {
    let (plan_id, plan_name, _grants) = match plan_id {
        Some(id) if !id.trim().is_empty() => (id.trim().to_string(), String::new(), Vec::new()),
        _ => {
            let plans = preview(client, account).await?;
            let best = plans
                .into_iter()
                .max_by_key(|p| p.priority)
                .ok_or_else(|| BillingError::Business {
                    code: -1,
                    message: "当前没有可领取的套餐".to_string(),
                    next_at: None,
                })?;
            (best.plan_id, best.name, best.grants)
        }
    };
    let mut headers = headers(account, Some(captcha))?;
    headers.insert(
        "X-ZCode-App-Version",
        APP_VERSION
            .parse()
            .map_err(|_| BillingError::Network("版本头无效".to_string()))?,
    );
    let response = client
        .post(format!("{BILLING_BASE}/billing/claim"))
        .headers(headers)
        .json(&json!({ "plan_id": plan_id }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|err| BillingError::Network(err.to_string()))?;
    let status = response.status().as_u16();
    let body: Value = response
        .json()
        .await
        .map_err(|err| BillingError::Network(format!("响应不是 JSON：{err}")))?;
    if status == 401 || status == 403 {
        return Err(BillingError::Auth("ZCode 登录失效，请重新授权".to_string()));
    }
    let code = business_code(&body);
    let plan = body
        .get("data")
        .and_then(|d| d.get("plan"))
        .filter(|value| value.is_object());
    if code == 0 && plan.is_some() {
        let plan = plan.unwrap_or(&Value::Null);
        return Ok(ClaimOutcome {
            plan_id,
            plan_name: plan
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&plan_name)
                .to_string(),
            code: Some(0),
            ok: true,
            next_at: None,
            message: None,
            starts_at: seconds_ms(plan.get("starts_at")),
            ends_at: seconds_ms(plan.get("ends_at")),
        });
    }
    let message = body
        .get("msg")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(ClaimOutcome {
        plan_id,
        plan_name,
        code: Some(code),
        ok: false,
        next_at: next_at(&body),
        message: Some(if !message.is_empty() {
            message
        } else if code == 0 {
            "领取响应缺少套餐信息".to_string()
        } else {
            claim_fail_text(code)
        }),
        starts_at: None,
        ends_at: None,
    })
}

/// direct transport 复用：账号鉴权 + ZCode 客户端头（不含验证码）。
pub(crate) fn client_headers(
    account: &ZcodeAccount,
) -> Result<reqwest::header::HeaderMap, BillingError> {
    headers(account, None)
}

fn headers(
    account: &ZcodeAccount,
    captcha: Option<&CaptchaToken>,
) -> Result<reqwest::header::HeaderMap, BillingError> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut insert = |name: &'static str, value: String| -> Result<(), BillingError> {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| BillingError::Network(format!("请求头 {name} 名称无效")))?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| BillingError::Network(format!("请求头 {name} 含非法字符")))?;
        headers.insert(name, value);
        Ok(())
    };
    insert("Content-Type", "application/json".to_string())?;
    insert("User-Agent", format!("ZCode/{APP_VERSION}"))?;
    insert("HTTP-Referer", "https://zcode.z.ai".to_string())?;
    insert("X-Title", "Z Code@electron".to_string())?;
    insert("X-ZCode-App-Version", APP_VERSION.to_string())?;
    insert("X-ZCode-Agent", "glm".to_string())?;
    insert("X-Platform", profile_platform(account))?;
    insert("X-Release-Channel", RELEASE_CHANNEL.to_string())?;
    insert("X-Client-Language", "zh-CN".to_string())?;
    insert("X-Client-Timezone", "Asia/Shanghai".to_string())?;
    insert("X-Os-Category", "windows".to_string())?;
    insert("X-Os-Version", "10.0.19045".to_string())?;
    insert("X-Device-Mid", device_mid(account))?;
    insert("x-request-id", uuid::Uuid::new_v4().to_string())?;
    if let Ok(secret) = account.plain_secret() {
        if account.mode == "jwt" {
            insert("Authorization", format!("Bearer {secret}"))?;
        } else {
            insert("x-api-key", secret)?;
        }
    } else {
        return Err(BillingError::Auth("账号凭据无法解密".to_string()));
    }
    if let Some(captcha) = captcha {
        insert("X-Aliyun-Captcha-Verify-Param", captcha.param.clone())?;
        if !captcha.region.trim().is_empty() {
            insert("X-Aliyun-Captcha-Verify-Region", captcha.region.clone())?;
        }
    }
    Ok(headers)
}

fn profile_platform(_account: &ZcodeAccount) -> String {
    // 本机 Windows 桌面形态；账号指纹 JSON 后续可覆盖。
    "win32-x64".to_string()
}

fn device_mid(account: &ZcodeAccount) -> String {
    // 由账号不可逆指纹派生，避免每次请求变设备；不是新的身份来源。
    let hex = &account.secret_fp;
    let bytes = hex.as_bytes();
    let mut raw = [0u8; 16];
    for (i, slot) in raw.iter_mut().enumerate() {
        *slot = *bytes.get(i % bytes.len()).unwrap_or(&b'0');
    }
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7], raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]
    )
}

fn parse_plan(raw: &Value) -> Option<Plan> {
    let plan_id = raw
        .get("plan_id")
        .or_else(|| raw.get("planId"))
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if plan_id.is_empty() {
        return None;
    }
    let grants = raw
        .get("entitlements")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_grant).collect())
        .unwrap_or_default();
    Some(Plan {
        plan_id,
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        priority: raw.get("priority").and_then(Value::as_i64).unwrap_or(0),
        grants,
    })
}

fn parse_grant(raw: &Value) -> Option<Value> {
    if raw.get("meter").and_then(Value::as_str) != Some("model_usage") {
        return None;
    }
    if raw.get("unit_type").and_then(Value::as_str) != Some("token") {
        return None;
    }
    let name = raw
        .get("show_name")
        .or_else(|| raw.get("showName"))
        .and_then(Value::as_str)?
        .trim();
    if name.is_empty() {
        return None;
    }
    Some(json!({
        "name": name,
        "units": raw.get("grant_units").or_else(|| raw.get("grantUnits")).and_then(Value::as_f64).unwrap_or(0.0),
        "period": raw.get("period").and_then(Value::as_str).unwrap_or("one_time"),
    }))
}

fn business_code(body: &Value) -> i64 {
    match body.get("code") {
        Some(Value::Number(value)) => value.as_i64().unwrap_or(-1),
        // 上游偶发把 code 序列化成字符串；客户端同样兼容。
        Some(Value::String(value)) => value.trim().parse().unwrap_or(-1),
        _ => -1,
    }
}

fn next_at(body: &Value) -> Option<i64> {
    body.get("data")
        .and_then(|d| d.get("plan"))
        .and_then(|p| p.get("ends_at").or_else(|| p.get("endsAt")))
        .and_then(Value::as_f64)
        .filter(|v| *v > 0.0)
        .map(|v| (v * 1000.0) as i64)
}

fn seconds_ms(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| *v > 0.0)
        .map(|v| (v * 1000.0) as i64)
}

fn claim_fail_text(code: i64) -> String {
    match code {
        1001 => "套餐不存在".to_string(),
        1002 => "活动已结束或套餐暂不可领取".to_string(),
        1003 => "该套餐已经领取过".to_string(),
        1004 => "不符合领取条件".to_string(),
        1005 => "今日领取名额已用完".to_string(),
        3001 => "领取参数错误".to_string(),
        3007 => "验证码校验失败".to_string(),
        401 => "请先登录后再领取".to_string(),
        _ => format!("领取失败（code={code}）"),
    }
}

pub fn status_after_claim(account: &ZcodeAccount, outcome: &ClaimOutcome) -> String {
    if outcome.ok {
        return super::store::STATUS_ACTIVE.to_string();
    }
    match outcome.code {
        Some(401) | Some(403) => STATUS_INVALID.to_string(),
        // 验证码失败是瞬时错误，不能把账号标成额度耗尽。
        Some(3007) => super::store::STATUS_COOLING.to_string(),
        // 活动/套餐已下架不代表账号没有额度，保持原状态等待下一轮活动。
        Some(1001) | Some(1002) | Some(1004) => account.status.clone(),
        Some(1005) => account.status.clone(),
        _ => STATUS_EXHAUSTED.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_parse_keeps_model_usage_grants() {
        let raw = json!({
            "plan_id": "p1",
            "name": "Start Plan",
            "priority": 3,
            "entitlements": [
                {"meter":"model_usage","unit_type":"token","show_name":"GLM-5.3-Flash","grant_units":1000,"period":"one_time"},
                {"meter":"other","unit_type":"token","show_name":"x","grant_units":1}
            ]
        });
        let plan = parse_plan(&raw).expect("plan");
        assert_eq!(plan.plan_id, "p1");
        assert_eq!(plan.priority, 3);
        assert_eq!(plan.grants.len(), 1);
        assert_eq!(plan.grants[0]["name"], "GLM-5.3-Flash");
    }

    #[test]
    fn business_codes_have_user_text() {
        assert!(claim_fail_text(1005).contains("名额"));
        assert!(claim_fail_text(3007).contains("验证码"));
        assert!(claim_fail_text(401).contains("登录"));
    }

    #[test]
    fn business_code_accepts_numeric_string() {
        assert_eq!(business_code(&json!({"code": 0})), 0);
        assert_eq!(business_code(&json!({"code": "3007"})), 3007);
        assert_eq!(business_code(&json!({"msg": "no code"})), -1);
    }

    #[test]
    fn marketing_touch_plan_is_extracted_from_banner_action() {
        let root = json!({
            "deliveries": [{
                "campaign_id": "c1",
                "resource_position": "banner",
                "banner": {
                    "buttons": [
                        {"action": {"type": "close", "args": null}},
                        {"action": {"type": "claim_zcode_plan", "args": {"plan_id": "zcode-v3-start-plan-1103"}}}
                    ],
                    "background": {
                        "type": "bundle",
                        "args": {"zcode_plan": {
                            "plan_id": "zcode-v3-start-plan-1103",
                            "name": "ZCODE GLOBAL BUILD",
                            "priority": 111,
                            "entitlements": [{
                                "show_name": "GLM-5.3-Flash",
                                "meter": "model_usage",
                                "unit_type": "token",
                                "grant_units": 300000000,
                                "period": "one_time"
                            }]
                        }}
                    }
                }
            }]
        });
        let plan_id = find_claim_plan_id(&root).expect("claim plan id");
        assert_eq!(plan_id, "zcode-v3-start-plan-1103");
        let plan = find_zcode_plan(&root, &plan_id).expect("plan");
        assert_eq!(plan.name, "ZCODE GLOBAL BUILD");
        assert_eq!(plan.grants[0]["units"], 300_000_000.0);
    }

    #[test]
    fn captcha_failure_cools_down_instead_of_exhausting() {
        let acc = ZcodeAccount::new("t", "zai", "jwt", "a.b.c").unwrap();
        let outcome = ClaimOutcome {
            plan_id: "p".into(),
            plan_name: "plan".into(),
            code: Some(3007),
            ok: false,
            next_at: None,
            message: Some("验证码校验失败".into()),
            starts_at: None,
            ends_at: None,
        };
        assert_eq!(
            status_after_claim(&acc, &outcome),
            super::super::store::STATUS_COOLING
        );
    }

    #[test]
    fn stale_campaign_keeps_account_status() {
        let acc = ZcodeAccount::new("t", "zai", "jwt", "a.b.c").unwrap();
        let outcome = ClaimOutcome {
            plan_id: "zcode-v3-start-plan".into(),
            plan_name: "Start Plan".into(),
            code: Some(1001),
            ok: false,
            next_at: None,
            message: Some("target not found".into()),
            starts_at: None,
            ends_at: None,
        };
        assert_eq!(status_after_claim(&acc, &outcome), acc.status);
    }

    #[test]
    fn device_mid_is_stable_and_uuid_shaped() {
        let mut acc = ZcodeAccount::new("t", "zai", "jwt", "a.b.c").unwrap();
        acc.secret_fp = "0123456789abcdef0123456789abcdef".to_string();
        let a = device_mid(&acc);
        let b = device_mid(&acc);
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
    }

    #[test]
    fn headers_accept_mixed_case_names_without_panic() {
        // 回归：HeaderName::from_static 对大小写混排会 panic；必须走 from_bytes。
        let acc = ZcodeAccount::new("t", "zai", "jwt", "a.b.c").unwrap();
        let map = headers(&acc, None).expect("headers");
        assert!(map.contains_key("authorization"));
        assert!(map.contains_key("x-platform"));
        assert!(!map.contains_key("x-aliyun-captcha-verify-param"));
    }
}
