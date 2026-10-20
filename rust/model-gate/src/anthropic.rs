//! Anthropic 协议适配：OpenAI Chat 请求 → `/v1/messages`，SSE 事件 → OpenAI SSE。
use serde_json::{json, Map, Value};

use crate::thinking::ThinkingCache;

/// 以 Python `json.dumps(..., ensure_ascii=False)` 的默认分隔符序列化：
/// `", "` 与 `": "`（下游可能做字符串级比较/回放，保持逐字节一致）。
pub fn dumps_like_python(value: &Value) -> String {
    let compact = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
    let mut out = String::with_capacity(compact.len() + 8);
    let mut in_string = false;
    let mut escape = false;
    let mut chars = compact.chars().peekable();
    while let Some(ch) = chars.next() {
        out.push(ch);
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == ':' || ch == ',' {
            if !matches!(chars.peek(), Some(' ') | Some('}') | Some(']')) {
                out.push(' ');
            }
        }
    }
    out
}

pub fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if let Some(s) = p.as_str() {
                    Some(s.to_string())
                } else {
                    p.get("text").and_then(Value::as_str).map(str::to_string)
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn push_message(messages: &mut Vec<Value>, role: &str, content: String) {
    if content.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let (Some(existing), true) = (last.get("content").and_then(Value::as_str), true) {
                let merged = format!("{existing}\n{content}");
                last["content"] = json!(merged);
                return;
            }
        }
    }
    messages.push(json!({"role": role, "content": content}));
}

fn safe_json(text: Option<&Value>) -> Value {
    let raw = text.and_then(Value::as_str).unwrap_or("");
    if raw.is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// OpenAI Chat 请求体 → Anthropic Messages 请求体。
///
/// `thinking_passthrough` 来自上游 compat：开启时，除客户端显式回传外，
/// 还会从缓存补回上一轮的 thinking + signature，避免 DeepSeek 思考链断裂。
pub fn to_anthropic(
    body: &Value,
    max_tokens: u64,
    model: &str,
    cache: &ThinkingCache,
    thinking_passthrough: bool,
) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let empty = Vec::new();
    let source = body
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    for m in source {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = text_of(m.get("content"));
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "tool" => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.get("tool_call_id"),
                        "content": text_of(m.get("content")),
                    }],
                }));
            }
            "assistant" => {
                let calls = m.get("tool_calls").and_then(Value::as_array);
                let text = text_of(m.get("content"));
                let tool_ids: Vec<String> = calls
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|tc| {
                                tc.get("id").and_then(Value::as_str).map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let think = cache.build_block(m, &text, &tool_ids, thinking_passthrough);
                if let Some(calls) = calls.filter(|c| !c.is_empty()) {
                    let mut blocks: Vec<Value> = Vec::new();
                    if let Some(block) = think {
                        blocks.push(block);
                    }
                    if !text.is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    for tc in calls {
                        let function = tc.get("function").cloned().unwrap_or_else(|| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.get("id"),
                            "name": function.get("name").and_then(Value::as_str).unwrap_or("unknown"),
                            "input": safe_json(function.get("arguments")),
                        }));
                    }
                    messages.push(json!({"role": "assistant", "content": blocks}));
                } else if let Some(block) = think {
                    let mut blocks = vec![block];
                    if !text.is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    messages.push(json!({"role": "assistant", "content": blocks}));
                } else {
                    push_message(&mut messages, "assistant", text);
                }
            }
            _ => push_message(&mut messages, "user", text_of(m.get("content"))),
        }
    }

    let max_tokens = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(max_tokens);
    let mut out = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    let system = system_parts.join("\n\n");
    if !system.is_empty() {
        out["system"] = json!(system);
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = body.get(key) {
            if !v.is_null() {
                out[key] = v.clone();
            }
        }
    }
    if let Some(stop) = body.get("stop").filter(|v| !v.is_null()) {
        out["stop_sequences"] = if stop.is_array() {
            stop.clone()
        } else {
            json!([stop])
        };
    }
    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        out["stream"] = json!(true);
    }

    let choice = body
        .get("tool_choice")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tools: Vec<Value> = if choice == "none" {
        Vec::new()
    } else {
        body.get("tools")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter(|t| {
                        t.get("type").and_then(Value::as_str) == Some("function")
                            && t.pointer("/function/name")
                                .and_then(Value::as_str)
                                .is_some()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    if !tools.is_empty() {
        out["tools"] = json!(tools
            .iter()
            .map(|t| {
                let f = t.get("function").cloned().unwrap_or_else(|| json!({}));
                json!({
                    "name": f.get("name").cloned().unwrap_or(Value::Null),
                    "description": f.get("description").cloned().unwrap_or_else(|| json!("")),
                    "input_schema": f.get("parameters").cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                })
            })
            .collect::<Vec<_>>());
        if choice == "auto" {
            out["tool_choice"] = json!({"type": "auto"});
        }
        // required/any/指定函数：thinking 模式不支持强制选择，按 Python 版降级为不下发。
    }
    out
}

pub fn finish_reason(stop_reason: Option<&str>) -> &'static str {
    match stop_reason {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    }
}

/// Anthropic usage → 内部用量（总输入含缓存，与 Python 版一致）。
pub fn usage_of(raw: &Value) -> Value {
    let input = raw.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
    let cache_read = raw
        .get("cache_read_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let cache_write = raw
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut out = Map::new();
    out.insert(
        "prompt_tokens".into(),
        json!(input + cache_read + cache_write),
    );
    out.insert(
        "completion_tokens".into(),
        json!(raw
            .get("output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0)),
    );
    if cache_read > 0 {
        out.insert("cacheReadTokens".into(), json!(cache_read));
    }
    if cache_write > 0 {
        out.insert("cacheWriteTokens".into(), json!(cache_write));
    }
    Value::Object(out)
}

/// Anthropic 非流式响应 → OpenAI Chat Completion。
pub fn to_openai(data: &Value, requested_model: &str, cache: &ThinkingCache) -> Value {
    let empty = Vec::new();
    let blocks = data
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let text: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    let thinking: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .filter_map(|b| b.get("thinking").and_then(Value::as_str))
        .collect();
    let tool_uses: Vec<&Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .collect();
    // 缓存本轮思考内容，供下一轮补回（DeepSeek 思考链必需）。
    let signature = blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .and_then(|b| b.get("signature"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let tool_ids: Vec<String> = tool_uses
        .iter()
        .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    cache.remember(&thinking, signature, &text, &tool_ids);

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    if !thinking.is_empty() {
        message.insert("reasoning_content".into(), json!(thinking));
    }
    if !tool_uses.is_empty() {
        let calls: Vec<Value> = tool_uses
            .iter()
            .enumerate()
            .map(|(i, b)| {
                json!({
                    "index": i,
                    "id": b.get("id"),
                    "type": "function",
                    "function": {
                        "name": b.get("name"),
                        "arguments": dumps_like_python(b.get("input").unwrap_or(&json!({}))),
                    },
                })
            })
            .collect();
        message.insert("tool_calls".into(), json!(calls));
    }

    let usage = usage_of(data.get("usage").unwrap_or(&Value::Null));
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut usage_out = json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    });
    if let Some(cache_read) = usage.get("cacheReadTokens").and_then(Value::as_i64) {
        usage_out["prompt_tokens_details"] = json!({"cached_tokens": cache_read});
    }

    json!({
        "id": data.get("id"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": requested_model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason(data.get("stop_reason").and_then(Value::as_str)),
        }],
        "usage": usage_out,
    })
}

pub fn chunk(model: &str, delta: Value, finish: Option<&str>, usage: Option<Value>) -> String {
    let mut obj = json!({
        "id": "chatcmpl-bridge",
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish,
        }],
    });
    if let Some(usage) = usage {
        obj["usage"] = usage;
    }
    format!(
        "data: {}\n\n",
        serde_json::to_string(&obj).unwrap_or_default()
    )
}

/// 把一段 Anthropic SSE（event + data）翻译成 OpenAI SSE 文本。
pub fn translate_sse_block(block: &str, model: &str, state: &mut StreamState) -> String {
    let mut event = "";
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return String::new();
    }
    let data: Value = match serde_json::from_str(&data_lines.join("\n")) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    match event {
        "message_start" => {
            if let Some(usage) = data.pointer("/message/usage") {
                state.apply_usage(usage);
            }
            out.push_str(&chunk(
                model,
                json!({"role": "assistant", "content": ""}),
                None,
                None,
            ));
        }
        "content_block_start" => {
            let block = data
                .get("content_block")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                state.tool_index += 1;
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    state.tool_ids.push(id.to_string());
                }
                out.push_str(&chunk(
                    model,
                    json!({"tool_calls": [{
                        "index": state.tool_index,
                        "id": block.get("id"),
                        "type": "function",
                        "function": {"name": block.get("name"), "arguments": ""},
                    }]}),
                    None,
                    None,
                ));
            } else if block.get("type").and_then(Value::as_str) == Some("thinking") {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    state.thinking.push_str(text);
                }
                if let Some(sig) = block.get("signature").and_then(Value::as_str) {
                    state.signature = sig.to_string();
                }
            }
        }
        "content_block_delta" => {
            let d = data.get("delta").cloned().unwrap_or_else(|| json!({}));
            match d.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = d.get("text").and_then(Value::as_str) {
                        state.text.push_str(text);
                    }
                    out.push_str(&chunk(
                        model,
                        json!({"content": d.get("text").unwrap_or(&json!(""))}),
                        None,
                        None,
                    ));
                }
                Some("thinking_delta") => {
                    if let Some(text) = d.get("thinking").and_then(Value::as_str) {
                        state.thinking.push_str(text);
                    }
                    out.push_str(&chunk(
                        model,
                        json!({"reasoning_content": d.get("thinking").unwrap_or(&json!(""))}),
                        None,
                        None,
                    ));
                }
                Some("signature_delta") => {
                    if let Some(sig) = d.get("signature").and_then(Value::as_str) {
                        state.signature = sig.to_string();
                    }
                }
                Some("input_json_delta") if state.tool_index >= 0 => {
                    out.push_str(&chunk(
                        model,
                        json!({"tool_calls": [{
                            "index": state.tool_index,
                            "function": {"arguments": d.get("partial_json").unwrap_or(&json!(""))},
                        }]}),
                        None,
                        None,
                    ));
                }
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(stop) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                state.finish = finish_reason(Some(stop)).to_string();
            }
            if let Some(out_tokens) = data.pointer("/usage/output_tokens").and_then(Value::as_i64) {
                state.completion = out_tokens;
            }
            if let Some(usage) = data.get("usage") {
                state.apply_usage(usage);
            }
        }
        "message_stop" => {
            state.finished = true;
            let usage = json!({
                "prompt_tokens": state.prompt,
                "completion_tokens": state.completion,
                "total_tokens": state.prompt + state.completion,
            });
            out.push_str(&chunk(model, json!({}), Some(&state.finish), Some(usage)));
            out.push_str("data: [DONE]\n\n");
        }
        _ => {}
    }
    out
}

#[derive(Default)]
pub struct StreamState {
    pub prompt: i64,
    pub completion: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub tool_index: i64,
    pub finish: String,
    pub finished: bool,
    /// 本轮累计的思考内容/签名/文本/工具调用 id，用于回传缓存。
    pub thinking: String,
    pub signature: String,
    pub text: String,
    pub tool_ids: Vec<String>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            finish: "stop".into(),
            tool_index: -1,
            ..Default::default()
        }
    }

    /// 流结束时把思考内容写入缓存，供下一轮补回。
    pub fn remember(&self, cache: &ThinkingCache) {
        cache.remember(&self.thinking, &self.signature, &self.text, &self.tool_ids);
    }

    fn apply_usage(&mut self, usage: &Value) {
        let input = usage.get("input_tokens").and_then(Value::as_i64);
        let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_i64);
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_i64);
        if cache_read.is_some() {
            self.cache_read = cache_read.unwrap_or(0);
        }
        if cache_write.is_some() {
            self.cache_write = cache_write.unwrap_or(0);
        }
        if input.is_some() || cache_read.is_some() || cache_write.is_some() {
            self.prompt = input.unwrap_or(0) + self.cache_read + self.cache_write;
        }
    }
}
