//! Z.AI OAuth CLI 流程（与 ZCode 客户端同源）。
//!
//! 只做 init/poll 转发；JWT 入池由调用方完成。flow 存在内存，TTL 300s，
//! 不写日志、不落盘。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

const OAUTH_BASE: &str = "https://zcode.z.ai/api/v1";
const FLOW_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
struct Flow {
    poll_token: String,
    /// 使用墙上时钟而非 `Instant`：`Instant` 在 Windows 上从系统启动计时，
    /// 启动不足 TTL 时无法构造“过去时间”，测试会因减法下溢而 panic。
    created: SystemTime,
}

#[derive(Debug, Default)]
pub struct ZcodeOAuth {
    flows: Mutex<HashMap<String, Flow>>,
}

#[derive(Debug, Clone)]
pub struct OAuthStart {
    pub flow_id: String,
    pub authorize_url: String,
    pub expires_in: u64,
}

impl ZcodeOAuth {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(&self, client: &reqwest::Client) -> Result<OAuthStart, String> {
        self.gc();
        let poll_token = uuid::Uuid::new_v4().simple().to_string();
        let response = client
            .post(format!("{OAUTH_BASE}/oauth/cli/init"))
            .bearer_auth(&poll_token)
            .json(&json!({ "provider": "zai" }))
            .send()
            .await
            .map_err(|err| format!("OAuth init 请求失败：{err}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "OAuth init HTTP {}：{}",
                status.as_u16(),
                truncate(&text)
            ));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|err| format!("OAuth init 响应不是 JSON：{err}"))?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            return Err(format!(
                "OAuth init 被上游拒绝（code={code}）：{}",
                truncate(&text)
            ));
        }
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        let flow_id = data
            .get("flow_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "OAuth init 响应缺少 flow_id".to_string())?
            .to_string();
        let authorize_url = data
            .get("authorize_url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "OAuth init 响应缺少 authorize_url".to_string())?
            .to_string();
        let server_poll = data
            .get("poll_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&poll_token)
            .to_string();
        self.flows.lock().unwrap_or_else(|e| e.into_inner()).insert(
            flow_id.clone(),
            Flow {
                poll_token: server_poll,
                created: SystemTime::now(),
            },
        );
        Ok(OAuthStart {
            flow_id,
            authorize_url,
            expires_in: FLOW_TTL.as_secs(),
        })
    }

    /// 返回上游 `data`；status 可能为 pending/ready/failed/expired。
    pub async fn poll(&self, client: &reqwest::Client, flow_id: &str) -> Result<Value, String> {
        let flow = self
            .flows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(flow_id)
            .cloned()
            .ok_or_else(|| "OAuth 会话不存在或已过期".to_string())?;
        if flow
            .created
            .elapsed()
            .map(|elapsed| elapsed > FLOW_TTL)
            .unwrap_or(true)
        {
            self.remove(flow_id);
            return Ok(json!({"status": "expired"}));
        }
        let response = client
            .get(format!("{OAUTH_BASE}/oauth/cli/poll/{flow_id}"))
            .bearer_auth(&flow.poll_token)
            .send()
            .await
            .map_err(|err| format!("OAuth poll 请求失败：{err}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.as_u16() == 404 {
            self.remove(flow_id);
            return Ok(json!({"status": "expired"}));
        }
        if !status.is_success() {
            // 4xx 多为会话过期；保留 pending 让调用方重试更安全，但 3004 明确过期。
            if let Ok(body) = serde_json::from_str::<Value>(&text) {
                if body.get("code").and_then(Value::as_i64) == Some(3004) {
                    self.remove(flow_id);
                    return Ok(json!({"status": "expired"}));
                }
            }
            return Ok(json!({"status": "pending", "upstreamStatus": status.as_u16()}));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|err| format!("OAuth poll 响应不是 JSON：{err}"))?;
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        let state = data
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if state == "ready" || state == "failed" || state == "expired" {
            self.remove(flow_id);
        }
        Ok(data)
    }

    fn remove(&self, flow_id: &str) {
        self.flows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(flow_id);
    }

    fn gc(&self) {
        let mut flows = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        flows.retain(|_, flow| {
            flow.created
                .elapsed()
                .map(|elapsed| elapsed <= FLOW_TTL)
                // 系统时钟回拨视为过期，避免留下无法清理的会话。
                .unwrap_or(false)
        });
    }
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= 200 {
        text.to_string()
    } else {
        text.chars().take(200).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_gc_removes_old_entries() {
        let oauth = ZcodeOAuth::new();
        oauth.flows.lock().unwrap().insert(
            "old".into(),
            Flow {
                poll_token: "x".into(),
                created: SystemTime::now()
                    .checked_sub(Duration::from_secs(301))
                    .expect("当前系统时钟应晚于 1970-01-01 + 301s"),
            },
        );
        oauth.gc();
        assert!(oauth.flows.lock().unwrap().is_empty());
    }
}
