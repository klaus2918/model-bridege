//! 网关 REST 客户端：面板是网关的纯前端，一切经由 127.0.0.1:8900 的 /api。
//!
//! 密钥永不经过面板（网关接口不回显密钥）；这里的错误信息也不含密钥。

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct GatewayClient {
    agent: ureq::Agent,
    pub base: String,
}

impl GatewayClient {
    pub fn new(base: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(20))
            .build();
        Self {
            agent,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub fn default_url() -> String {
        std::env::var("MODEL_GATE_URL").unwrap_or_else(|_| "http://127.0.0.1:8900".to_string())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        let resp = self.agent.get(&self.url(path)).call()?;
        let text = resp.into_string()?;
        serde_json::from_str(&text).map_err(|e| anyhow!("响应不是合法 JSON: {e}"))
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self
            .agent
            .post(&self.url(path))
            .send_json(body.clone())?;
        let text = resp.into_string()?;
        serde_json::from_str(&text).map_err(|e| anyhow!("响应不是合法 JSON: {e}"))
    }

    pub fn post_empty(&self, path: &str) -> Result<Value> {
        let resp = self
            .agent
            .post(&self.url(path))
            .set("Content-Type", "application/json")
            .send_string("{}")?;
        let text = resp.into_string()?;
        serde_json::from_str(&text).map_err(|e| anyhow!("响应不是合法 JSON: {e}"))
    }

    #[allow(dead_code)] // 供后续 PATCH 流程使用
    pub fn patch(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self.agent.patch(&self.url(path)).send_json(body.clone())?;
        let text = resp.into_string()?;
        serde_json::from_str(&text).map_err(|e| anyhow!("响应不是合法 JSON: {e}"))
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        let resp = self.agent.delete(&self.url(path)).call()?;
        let text = resp.into_string()?;
        serde_json::from_str(&text).map_err(|e| anyhow!("响应不是合法 JSON: {e}"))
    }

    /// 探活：/health，1 秒超时。
    pub fn ping(&self) -> bool {
        self.agent
            .get(&self.url("/health"))
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
    }
}

/// 把 API 错误转成用户可读的一句话（ureq 的错误链较长，取根因）。
pub fn brief_error(err: anyhow::Error) -> String {
    let mut msg = err.to_string();
    // Status 分支的 response 只能被消耗一次：downcast 按值取出。
    if let Ok(ureq::Error::Status(code, resp)) = err.downcast::<ureq::Error>() {
        {
            // 网关错误响应带结构化 message，优先用它。
            if let Ok(body) = resp.into_string() {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if let Some(m) = v
                        .pointer("/error/message")
                        .or_else(|| v.get("message"))
                        .and_then(Value::as_str)
                    {
                        return m.to_string();
                    }
                }
            }
            msg = format!("HTTP {code}");
        }
    }
    if msg.contains("Connection refused") {
        return "网关未运行（连接被拒绝）".to_string();
    }
    msg
}
