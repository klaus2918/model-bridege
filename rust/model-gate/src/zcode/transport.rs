//! `transport=zcode`：把 OpenAI Chat 请求交给本机 ZCode 客户端 CLI。
//!
//! 为什么用 CLI `-p --json` 而不是自制 JSON-RPC：已实测该路径能走 Start Plan
//! 并返回结构化 response/usage；app-server 的会话/权限/回合锁复杂度更高。
//! CLI 是内部 transport worker，不持有账号库，也不提供 UI。
//!
//! 账号池：按 `last_used_at`/`use_count` 轮询；失败按错误语义标 invalid/冷却，
//! 换下一个账号。任何路径都不记录 JWT、验证码或完整上游错误。

use std::time::Instant;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::Upstream;
use crate::prompt::RequestCtx;
use crate::proxy::{error_json, AppState};

use super::appserver;
use super::store::{ZcodeAccount, STATUS_ACTIVE, STATUS_COOLING, STATUS_DISABLED, STATUS_INVALID};

/// 单次代理请求最多尝试的账号数，避免一个坏池拖死请求。
const MAX_ATTEMPTS: usize = 3;
/// 风控/异常活动的冷却时间：15 分钟。
const RISK_COOLDOWN_MS: i64 = 15 * 60 * 1000;
/// 普通失败的短冷却：60 秒。
const SHORT_COOLDOWN_MS: i64 = 60 * 1000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 归一化 CLI 错误：决定账号状态与冷却时间。
///
/// 只做文本分类（CLI 不一定暴露 HTTP 状态）；分类错误不会泄露 token。
pub fn classify_failure(text: &str, fail_count: i64) -> (&'static str, Option<i64>) {
    let lower = text.to_lowercase();
    let auth = [
        "401",
        "unauthorized",
        "invalid jwt",
        "invalid token",
        "登录失效",
        "not logged in",
        "authentication failed",
    ];
    if auth.iter().any(|needle| lower.contains(needle)) {
        return (STATUS_INVALID, None);
    }
    let risk = ["3012", "405", "unusual activity", "risk control", "风控"];
    if risk.iter().any(|needle| lower.contains(needle)) {
        if fail_count >= 4 {
            return (STATUS_DISABLED, None);
        }
        return (STATUS_COOLING, Some(now_ms() + RISK_COOLDOWN_MS));
    }
    let rate = ["429", "rate limit", "too many requests", "限流"];
    if rate.iter().any(|needle| lower.contains(needle)) {
        return (STATUS_COOLING, Some(now_ms() + SHORT_COOLDOWN_MS));
    }
    (STATUS_COOLING, Some(now_ms() + SHORT_COOLDOWN_MS))
}

fn brief(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn usage_of(usage: Option<&Value>) -> Value {
    let read = |camel: &str, snake: &str| -> i64 {
        usage
            .and_then(|u| u.get(camel).or_else(|| u.get(snake)))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let input = read("inputTokens", "input_tokens");
    let output = read("outputTokens", "output_tokens");
    let total = read("totalTokens", "total_tokens");
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": if total > 0 { total } else { input + output },
        "cacheReadTokens": read("cacheReadTokens", "cache_read_tokens"),
        "cacheWriteTokens": read("cacheWriteTokens", "cache_write_tokens"),
    })
}

fn completion_body(text: &str, model: &str, usage: &Value) -> Value {
    json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop",
        }],
        "usage": usage,
    })
}

/// 一次性把完整结果包装成 OpenAI SSE：CLI 是整段返回，故只发一个 delta。
fn stream_body(text: &str, model: &str, usage: &Value) -> String {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = chrono::Utc::now().timestamp();
    let first = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}],
    });
    let last = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": usage,
    });
    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
}

fn upstream_headers(response: &mut Response, upstream: &Upstream, model: &str) {
    if let Ok(value) = header::HeaderValue::from_str(&upstream.name) {
        response.headers_mut().insert("x-bridge-upstream", value);
    }
    if let Ok(value) = header::HeaderValue::from_str(model) {
        response
            .headers_mut()
            .insert("x-bridge-upstream-model", value);
    }
}

fn record_attempt(
    state: &AppState,
    upstream: &Upstream,
    requested: &str,
    target: &str,
    ctx: &RequestCtx,
    started: Instant,
    status: u16,
    stream: bool,
    usage: Option<&Value>,
    error: Option<String>,
) {
    crate::proxy::record_ex(
        state, upstream, requested, target, ctx, started, status, stream, usage, error, 0, false,
        false,
    );
}

/// 主入口：由 `proxy::chat_inner` 在识别 `transport=zcode` 后调用。
pub async fn chat(
    state: &AppState,
    upstream: &Upstream,
    payload: &Value,
    requested_model: &str,
    target_model: &str,
    ctx: &RequestCtx,
    started: Instant,
    stream: bool,
) -> Response {
    let backend = state.zcode.config().backend.clone();
    if backend != "appserver" {
        // direct 已实测 405/3012；不做自动回退，避免把风控噪音当正常路由。
        let message = "ZCode backend=direct 已实测被上游拒绝（405/3012），请改用 backend=appserver";
        record_attempt(
            state,
            upstream,
            requested_model,
            target_model,
            ctx,
            started,
            501,
            stream,
            None,
            Some(message.to_string()),
        );
        return error_json(
            StatusCode::NOT_IMPLEMENTED,
            message,
            "upstream_error",
            "zcode_direct_unavailable",
        );
    }
    if !state.zcode.config().enabled {
        let message = "ZCode 插件未启用：请先在 ZCode 面板启用插件";
        record_attempt(
            state,
            upstream,
            requested_model,
            target_model,
            ctx,
            started,
            503,
            stream,
            None,
            Some(message.to_string()),
        );
        return error_json(
            StatusCode::SERVICE_UNAVAILABLE,
            message,
            "upstream_error",
            "zcode_disabled",
        );
    }

    let prompt = super::client::prompt_from_payload(payload);
    if prompt.trim().is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "请求没有可发送的文本内容",
            "invalid_request_error",
            "zcode_empty_prompt",
        );
    }
    // 客户端已按模型 thinkingLevelMap 把 UI 等级映射成 wire 值，这里只校验归一后
    // 透传；解析一次放在重试循环外，不随账号重试重复。
    let raw_level = payload.get("reasoning_effort").and_then(Value::as_str);
    let reasoning_level = appserver::resolve_reasoning_level(
        raw_level,
        &upstream.compat.reasoning_level_default,
        &upstream.compat.reasoning_level_allow,
    );
    let config = state.zcode.config().clone();
    let attempts = config.account_concurrency.clamp(1, 4) as usize;
    let max_attempts = attempts.min(MAX_ATTEMPTS);
    let mut excluded: Vec<String> = Vec::new();
    let mut last_error = String::new();
    let mut last_account: Option<ZcodeAccount> = None;

    for _ in 0..max_attempts {
        let Some(account) = state.db.zcode_pick_account(&excluded) else {
            break;
        };
        excluded.push(account.id.clone());
        last_account = Some(account.clone());
        match appserver::chat(&config, &account, target_model, &prompt, &reasoning_level).await {
            Ok(response) => {
                let usage = usage_of(response.usage.as_ref());
                state.db.zcode_touch_used(&account.id, true);
                state.db.zcode_update_quota(
                    &account.id,
                    None,
                    None,
                    Some(STATUS_ACTIVE),
                    None,
                    None,
                );
                record_attempt(
                    state,
                    upstream,
                    requested_model,
                    target_model,
                    ctx,
                    started,
                    200,
                    stream,
                    Some(&usage),
                    None,
                );
                let text = response.text;
                if stream {
                    let body = stream_body(&text, requested_model, &usage);
                    let mut out = Response::new(Body::from(body));
                    out.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("text/event-stream"),
                    );
                    out.headers_mut().insert(
                        header::CACHE_CONTROL,
                        header::HeaderValue::from_static("no-cache"),
                    );
                    upstream_headers(&mut out, upstream, target_model);
                    return out;
                }
                let mut out = Json(completion_body(&text, requested_model, &usage)).into_response();
                upstream_headers(&mut out, upstream, target_model);
                return out;
            }
            Err(err) => {
                last_error = err;
                state.db.zcode_touch_used(&account.id, false);
                let fail_count = state
                    .db
                    .zcode_get_account(&account.id)
                    .map(|a| a.fail_count)
                    .unwrap_or(1);
                let (status, cooling) = classify_failure(&last_error, fail_count);
                let message = brief(&last_error, 300);
                tracing::warn!(
                    account = %account.id,
                    status,
                    "ZCode transport 失败，切换账号：{}",
                    message
                );
                state.db.zcode_update_quota(
                    &account.id,
                    None,
                    None,
                    Some(status),
                    Some(&message),
                    cooling,
                );
            }
        }
    }

    let has_account = last_account.is_some();
    let message = if let Some(account) = last_account {
        format!(
            "ZCode 账号 {} 均不可用：{}",
            account.name,
            brief(&last_error, 300)
        )
    } else {
        "没有可用的 ZCode 账号：请先在 ZCode 面板导入/登录账号".to_string()
    };
    let status = if has_account {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    record_attempt(
        state,
        upstream,
        requested_model,
        target_model,
        ctx,
        started,
        status.as_u16(),
        stream,
        None,
        Some(message.clone()),
    );
    error_json(
        status,
        message,
        "upstream_error",
        if has_account {
            "zcode_transport_failed"
        } else {
            "no_available_account"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_errors_invalidate_account() {
        assert_eq!(
            classify_failure("HTTP 401 Unauthorized", 0).0,
            STATUS_INVALID
        );
        assert_eq!(
            classify_failure("登录失效，请重新授权", 0).0,
            STATUS_INVALID
        );
    }

    #[test]
    fn risk_errors_cool_then_disable() {
        assert_eq!(
            classify_failure("code=3012 unusual activity", 0).0,
            STATUS_COOLING
        );
        assert_eq!(
            classify_failure("code=3012 unusual activity", 4).0,
            STATUS_DISABLED
        );
    }

    #[test]
    fn rate_limit_cools_but_keeps_account() {
        let (status, cooling) = classify_failure("HTTP 429 too many requests", 1);
        assert_eq!(status, STATUS_COOLING);
        assert!(cooling.unwrap() > now_ms());
    }

    #[test]
    fn completion_body_is_openai_shaped() {
        let usage = json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":3});
        let body = completion_body("pong", "GLM-5.3-Flash", &usage);
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "pong");
        assert_eq!(body["usage"]["total_tokens"], 3);
    }

    #[test]
    fn stream_body_ends_with_done() {
        let usage = json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":3});
        let body = stream_body("pong", "GLM-5.3-Flash", &usage);
        assert!(body.starts_with("data: "));
        assert!(body.ends_with("data: [DONE]\n\n"));
        assert!(body.contains("\"content\":\"pong\""));
    }

    #[test]
    fn brief_never_returns_more_than_limit() {
        let text = "x".repeat(500);
        assert_eq!(brief(&text, 10).chars().count(), 11); // 10 + 省略号
    }
}
