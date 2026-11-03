//! `backend=direct`：直接调用 ZCode coding-plan 的 Anthropic Messages 接口。
//!
//! 为什么不用 app-server：appserver 每次请求都要启动 Node、创建 session 并跑完整
//! agent loop，首字节常要十几秒；这里用账号 JWT 直连 Anthropic 兼容端点，
//! SSE 原样翻译给客户端，首字节通常几百毫秒。
//!
//! 账号凭据只在内存中拼 Authorization，不写日志；错误只保留业务码/文案。

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::anthropic;
use crate::config::Upstream;
use crate::prompt::RequestCtx;
use crate::proxy::{error_json, record_ex, AppState};

use super::billing;
use super::store::{ZcodeAccount, STATUS_ACTIVE};
use super::transport::{brief, classify_failure, upstream_headers};

const MAX_ATTEMPTS: usize = 3;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn mark_success(state: &AppState, account: &ZcodeAccount) {
    state.db.zcode_touch_used(&account.id, true);
    state
        .db
        .zcode_update_quota(&account.id, None, None, Some(STATUS_ACTIVE), None, None);
}

fn mark_failure(state: &AppState, account: &ZcodeAccount, code: Option<i64>, message: &str) {
    state.db.zcode_touch_used(&account.id, false);
    let fail_count = state
        .db
        .zcode_get_account(&account.id)
        .map(|item| item.fail_count)
        .unwrap_or(1);
    // 1005 是「额度暂不可用」而不是账号坏了：保持账号可用，避免 60s 冷却后
    // 把用户挡在「没有可用账号」的错误上。
    if code == Some(1005) {
        state.db.zcode_update_quota(
            &account.id,
            None,
            None,
            Some(STATUS_ACTIVE),
            Some(&brief(message, 300)),
            None,
        );
        return;
    }
    let (status, cooling) = classify_failure(message, fail_count);
    state.db.zcode_update_quota(
        &account.id,
        None,
        None,
        Some(status),
        Some(&brief(message, 300)),
        cooling.or_else(|| Some(now_ms() + 60_000)),
    );
}

/// 解析 ZCode 业务错误：HTTP 200 也可能是 `{code:1005}`。
fn business_error(data: &Value) -> Option<(i64, String)> {
    let code = match data.get("code") {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.trim().parse().ok(),
        _ => None,
    };
    if let Some(code) = code.filter(|code| *code != 0) {
        let message = data
            .get("msg")
            .or_else(|| data.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        return Some((
            code,
            if message.is_empty() {
                format!("code={code}")
            } else {
                message
            },
        ));
    }
    if data.get("type").and_then(Value::as_str) == Some("error") {
        let message = data
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        return Some((
            -1,
            if message.is_empty() {
                "上游返回错误".to_string()
            } else {
                message
            },
        ));
    }
    None
}

/// 主入口：由 `transport::chat` 在 `backend=direct` 时调用。
pub async fn chat(
    state: Arc<AppState>,
    upstream: &Upstream,
    payload: &Value,
    requested_model: &str,
    target_model: &str,
    ctx: &RequestCtx,
    started: Instant,
    stream: bool,
) -> Response {
    let outbound = anthropic::to_anthropic(
        payload,
        state.cfg().max_tokens.max(1),
        target_model,
        &state.thinking,
        upstream.compat.thinking_passthrough,
    );
    let url = format!("{}/v1/messages", upstream.base_url.trim_end_matches('/'));
    let max_attempts =
        (state.zcode.config().account_concurrency.clamp(1, 4) as usize).min(MAX_ATTEMPTS);
    let mut excluded: Vec<String> = Vec::new();
    let mut last_error = String::new();
    let mut last_account: Option<ZcodeAccount> = None;

    for _ in 0..max_attempts {
        let Some(account) = state.db.zcode_pick_account(&excluded) else {
            break;
        };
        excluded.push(account.id.clone());
        last_account = Some(account.clone());

        let mut headers = match billing::client_headers(&account) {
            Ok(headers) => headers,
            Err(err) => {
                last_error = err.message();
                mark_failure(&state, &account, None, &last_error);
                continue;
            }
        };
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        headers.insert("accept", accept.parse().unwrap());

        let response = state
            .client
            .post(&url)
            .headers(headers)
            .json(&outbound)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                last_error = format!("ZCode direct 连接失败：{err}");
                mark_failure(&state, &account, None, &last_error);
                continue;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            last_error = format!(
                "ZCode direct HTTP {}：{}",
                status.as_u16(),
                brief(&text, 300)
            );
            mark_failure(&state, &account, None, &last_error);
            continue;
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if stream && content_type.starts_with("text/event-stream") {
            mark_success(&state, &account);
            return stream_response(
                state,
                upstream.clone(),
                requested_model.to_string(),
                target_model.to_string(),
                response,
                ctx.clone(),
                started,
            );
        }

        let text = response.text().await.unwrap_or_default();
        let data: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
        if let Some((code, message)) = business_error(&data) {
            last_error = if code < 0 {
                format!("ZCode direct 上游错误：{message}")
            } else {
                format!("ZCode direct 业务失败({code})：{message}")
            };
            mark_failure(
                &state,
                &account,
                Some(code).filter(|value| *value > 0),
                &last_error,
            );
            continue;
        }
        mark_success(&state, &account);
        let usage = anthropic::usage_of(data.get("usage").unwrap_or(&Value::Null));
        let value = anthropic::to_openai(&data, requested_model, &state.thinking);
        record_ex(
            &state,
            upstream,
            requested_model,
            target_model,
            ctx,
            started,
            200,
            false,
            Some(&usage),
            None,
            0,
            false,
            false,
        );
        let mut out = Json(value).into_response();
        upstream_headers(&mut out, upstream, target_model);
        return out;
    }

    let has_account = last_account.is_some();
    let message = if let Some(account) = last_account {
        format!(
            "ZCode 账号 {} 均不可用：{}",
            account.name,
            brief(&last_error, 300)
        )
    } else {
        "当前没有可用的 ZCode 账号（可能都在冷却或已停用），请稍后重试或在面板检查账号状态"
            .to_string()
    };
    let status = if has_account {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    record_ex(
        &state,
        upstream,
        requested_model,
        target_model,
        ctx,
        started,
        status.as_u16(),
        stream,
        None,
        Some(message.clone()),
        0,
        false,
        false,
    );
    error_json(
        status,
        message,
        "upstream_error",
        if has_account {
            "zcode_direct_failed"
        } else {
            "no_available_account"
        },
    )
}

fn stream_response(
    state: Arc<AppState>,
    upstream: Upstream,
    requested_model: String,
    target_model: String,
    response: reqwest::Response,
    ctx: RequestCtx,
    started: Instant,
) -> Response {
    let mut byte_stream = response.bytes_stream();
    let header_upstream = upstream.clone();
    let header_model = target_model.clone();
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
        st.remember(&state.thinking);
        let usage = json!({
            "prompt_tokens": st.prompt,
            "completion_tokens": st.completion,
            "cacheReadTokens": st.cache_read,
            "cacheWriteTokens": st.cache_write,
        });
        record_ex(
            &state,
            &upstream,
            &requested_model,
            &target_model,
            &ctx,
            started,
            200,
            true,
            Some(&usage),
            None,
            ttft_ms,
            false,
            false,
        );
    };
    let mut out = Response::new(Body::from_stream(stream));
    out.headers_mut().insert(
        "content-type",
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    out.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    upstream_headers(&mut out, &header_upstream, &header_model);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_error_reads_numeric_and_string_codes() {
        let numeric = business_error(&json!({"code": 1005, "msg": "exceed quota limit"}))
            .expect("numeric code");
        assert_eq!(numeric.0, 1005);
        assert!(numeric.1.contains("quota"));
        let string =
            business_error(&json!({"code": "3007", "msg": "captcha"})).expect("string code");
        assert_eq!(string.0, 3007);
        assert!(business_error(&json!({"code": 0})).is_none());
    }

    #[test]
    fn anthropic_error_envelope_is_detected() {
        let error = business_error(&json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "bad request"}
        }))
        .expect("error envelope");
        assert_eq!(error.0, -1);
        assert!(error.1.contains("bad request"));
    }
}
