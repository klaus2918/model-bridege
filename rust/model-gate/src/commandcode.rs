//! CommandCode 协议适配：OpenAI Chat → `/alpha/generate`，NDJSON 事件 → OpenAI SSE。
use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::anthropic::text_of;

#[derive(Default)]
pub struct StreamState {
    pub text: String,
    pub reasoning: String,
    pub calls: Vec<ToolCall>,
    pub by_id: HashMap<String, usize>,
    pub finish: String,
    pub prompt: i64,
    pub completion: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub saw_finish: bool,
    pub paused: bool,
    pub error: String,
}

#[derive(Default, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: String,
}

impl StreamState {
    pub fn new() -> Self {
        Self { finish: "stop".into(), ..Default::default() }
    }

    pub fn usage(&self) -> Value {
        json!({
            "prompt_tokens": self.prompt,
            "completion_tokens": self.completion,
            "cacheReadTokens": self.cache_read,
            "cacheWriteTokens": self.cache_write,
        })
    }

    /// 对外 SSE/非流式用的 OpenAI 形状 usage（缓存读映射到 cached_tokens）。
    pub fn openai_usage(&self) -> Value {
        let mut out = json!({
            "prompt_tokens": self.prompt,
            "completion_tokens": self.completion,
            "total_tokens": self.prompt + self.completion,
        });
        if self.cache_read > 0 {
            out["prompt_tokens_details"] = json!({"cached_tokens": self.cache_read});
        }
        out
    }

    fn tool_index(&mut self, id: &str, name: &str) -> usize {
        if !id.is_empty() {
            if let Some(idx) = self.by_id.get(id) {
                return *idx;
            }
        }
        let idx = self.calls.len();
        self.calls.push(ToolCall { id: id.to_string(), name: name.to_string(), args: String::new() });
        if !id.is_empty() {
            self.by_id.insert(id.to_string(), idx);
        }
        idx
    }
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

fn stop_reason(ev: &Value) -> &'static str {
    let raw = str_of(ev.get("finishReason").or_else(|| ev.get("rawFinishReason"))).to_lowercase();
    match raw.as_str() {
        "tool_use" | "tool-calls" | "tool_calls" => "tool_calls",
        "length" | "max_tokens" | "max_output_tokens" | "model_context_window_exceeded" => "length",
        _ => "stop",
    }
}

fn usage_into(st: &mut StreamState, ev: &Value) {
    let u = ev.get("totalUsage").or_else(|| ev.get("usage")).cloned().unwrap_or_else(|| json!({}));
    st.prompt += u.get("inputTokens").and_then(Value::as_i64).unwrap_or(0);
    st.completion += u.get("outputTokens").and_then(Value::as_i64).unwrap_or(0);
    if let Some(detail) = u.get("inputTokenDetails") {
        st.cache_read += detail.get("cacheReadTokens").and_then(Value::as_i64).unwrap_or(0);
        st.cache_write += detail.get("cacheWriteTokens").and_then(Value::as_i64).unwrap_or(0);
    }
}

pub fn chunk(model: &str, delta: Value, finish: Option<&str>, usage: Option<Value>) -> String {
    let mut obj = json!({
        "id": "chatcmpl-bridge",
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    if let Some(usage) = usage {
        obj["usage"] = usage;
    }
    format!("data: {}\n\n", serde_json::to_string(&obj).unwrap_or_default())
}

/// 处理一个 NDJSON 事件；返回需要写给下游的 SSE 文本（可能为空）。
pub fn handle_event(ev: &Value, st: &mut StreamState, model: &str) -> String {
    let etype = str_of(ev.get("type"));
    match etype.as_str() {
        "text-delta" => {
            let text = str_of(ev.get("text"));
            st.text.push_str(&text);
            chunk(model, json!({"content": text}), None, None)
        }
        "reasoning-delta" => {
            let text = str_of(ev.get("text"));
            st.reasoning.push_str(&text);
            chunk(model, json!({"reasoning_content": text}), None, None)
        }
        "tool-input-start" => {
            let id = str_of(ev.get("id").or_else(|| ev.get("toolCallId")));
            let name = str_of(ev.get("toolName"));
            let idx = st.tool_index(&id, &name);
            chunk(model, json!({"tool_calls": [{
                "index": idx, "id": id, "type": "function",
                "function": {"name": name, "arguments": ""},
            }]}), None, None)
        }
        "tool-input-delta" | "tool-delta" => {
            let id = str_of(ev.get("id").or_else(|| ev.get("toolCallId")));
            let name = str_of(ev.get("toolName"));
            let idx = st.tool_index(&id, &name);
            let delta = str_of(ev.get("delta").or_else(|| ev.get("partial")));
            if let Some(call) = st.calls.get_mut(idx) {
                call.args.push_str(&delta);
            }
            chunk(model, json!({"tool_calls": [{
                "index": idx, "function": {"arguments": delta},
            }]}), None, None)
        }
        "tool-call" | "tool-use" => {
            let id = str_of(ev.get("toolCallId").or_else(|| ev.get("id")));
            let name = str_of(ev.get("toolName"));
            let known = st.by_id.contains_key(&id);
            let idx = st.tool_index(&id, &name);
            let input = ev.get("input").or_else(|| ev.get("args")).cloned().unwrap_or_else(|| json!({}));
            let encoded = crate::anthropic::dumps_like_python(&input);
            if let Some(call) = st.calls.get_mut(idx) {
                call.args = encoded.clone();
            }
            if known {
                String::new()
            } else {
                chunk(model, json!({"tool_calls": [{
                    "index": idx, "id": id, "type": "function",
                    "function": {"name": name, "arguments": encoded},
                }]}), None, None)
            }
        }
        "finish" => {
            st.finish = stop_reason(ev).to_string();
            usage_into(st, ev);
            st.saw_finish = true;
            let raw = str_of(ev.get("rawFinishReason").or_else(|| ev.get("finishReason"))).to_lowercase();
            st.paused = raw == "pause_turn";
            String::new()
        }
        "error" => {
            let err = ev.get("error").cloned().unwrap_or(Value::Null);
            st.error = match err {
                Value::String(s) => s,
                Value::Object(o) => o.get("message").and_then(Value::as_str).unwrap_or("上游返回错误").to_string(),
                _ => "上游返回未知错误".into(),
            };
            String::new()
        }
        _ => String::new(),
    }
}

/// 非流式：把整段 NDJSON 聚合成 OpenAI chat.completion。
pub fn aggregate(raw: &str, requested_model: &str) -> (Value, StreamState) {
    let st = consume_ndjson(raw);
    (body_from_state(&st, requested_model), st)
}

pub fn consume_ndjson(raw: &str) -> StreamState {
    let mut st = StreamState::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(line) {
            handle_event(&ev, &mut st, "");
        }
    }
    st
}

pub fn body_from_state(st: &StreamState, requested_model: &str) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), if st.text.is_empty() { Value::Null } else { json!(st.text) });
    if !st.reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(st.reasoning));
    }
    if !st.calls.is_empty() {
        let calls: Vec<Value> = st.calls.iter().enumerate().map(|(i, c)| json!({
            "index": i,
            "id": c.id,
            "type": "function",
            "function": {
                "name": c.name,
                "arguments": if c.args.is_empty() { "{}".to_string() } else { c.args.clone() },
            },
        })).collect();
        message.insert("tool_calls".into(), json!(calls));
        if st.text.is_empty() {
            message.insert("content".into(), Value::Null);
        }
    }
    let usage = st.usage();
    let mut usage_out = json!({
        "prompt_tokens": usage.get("prompt_tokens"),
        "completion_tokens": usage.get("completion_tokens"),
        "total_tokens": st.prompt + st.completion,
    });
    if st.cache_read > 0 {
        usage_out["prompt_tokens_details"] = json!({"cached_tokens": st.cache_read});
    }
    json!({
        "id": format!("chatcmpl-{:x}", chrono::Utc::now().timestamp_millis()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": requested_model,
        "choices": [{"index": 0, "message": Value::Object(message), "finish_reason": st.finish}],
        "usage": usage_out,
    })
}

impl StreamState {
    /// pause_turn 续跑：把后续响应的文本/工具/用量并入已有状态。
    pub fn merge(&mut self, other: StreamState) {
        self.text.push_str(&other.text);
        self.reasoning.push_str(&other.reasoning);
        for call in other.calls {
            if !call.id.is_empty() {
                if let Some(idx) = self.by_id.get(&call.id) {
                    if let Some(existing) = self.calls.get_mut(*idx) {
                        existing.args.push_str(&call.args);
                        continue;
                    }
                }
                self.by_id.insert(call.id.clone(), self.calls.len());
            }
            self.calls.push(call);
        }
        self.prompt += other.prompt;
        self.completion += other.completion;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        if other.saw_finish {
            self.finish = other.finish;
            self.saw_finish = true;
            self.paused = other.paused;
        }
        if !other.error.is_empty() {
            self.error = other.error;
        }
    }
}

/// OpenAI 消息 → CommandCode wire 消息。
pub fn to_messages(messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut name_by_id: HashMap<String, String> = HashMap::new();
    for m in messages {
        let role = str_of(m.get("role"));
        match role.as_str() {
            "system" | "developer" => continue,
            "assistant" => {
                let mut blocks = Vec::new();
                let reasoning = m.get("reasoning_content").or_else(|| m.get("thinking"));
                if let Some(text) = reasoning.and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    blocks.push(json!({"type": "reasoning", "text": text}));
                }
                let text = text_of(m.get("content"));
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for tc in calls {
                        let function = tc.get("function").cloned().unwrap_or_else(|| json!({}));
                        let name = function.get("name").and_then(Value::as_str).unwrap_or("unknown");
                        let id = str_of(tc.get("id"));
                        if !id.is_empty() {
                            name_by_id.insert(id.clone(), name.to_string());
                        }
                        let args = function.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                        let input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool-call",
                            "toolCallId": tc.get("id"),
                            "toolName": name,
                            "input": input,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            "tool" => {
                let id = str_of(m.get("tool_call_id"));
                out.push(json!({
                    "role": "tool",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": id,
                        "toolName": name_by_id.get(&id).cloned().unwrap_or_else(|| "unknown".into()),
                        "output": {"type": "text", "value": text_of(m.get("content"))},
                    }],
                }));
            }
            _ => {
                let mut parts = Vec::new();
                match m.get("content") {
                    Some(Value::String(s)) if !s.is_empty() => {
                        parts.push(json!({"type": "text", "text": s}));
                    }
                    Some(Value::Array(items)) => {
                        for p in items {
                            if let Some(s) = p.as_str() {
                                if !s.is_empty() {
                                    parts.push(json!({"type": "text", "text": s}));
                                }
                            } else if p.get("type").and_then(Value::as_str) == Some("text") {
                                if let Some(t) = p.get("text").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                                    parts.push(json!({"type": "text", "text": t}));
                                }
                            } else if p.get("type").and_then(Value::as_str) == Some("image_url") {
                                let image = p.get("image_url").cloned().unwrap_or(Value::Null);
                                let url = image.as_str().map(str::to_string).unwrap_or_else(|| {
                                    image.get("url").and_then(Value::as_str).unwrap_or("").to_string()
                                });
                                if let Some(rest) = url.strip_prefix("data:") {
                                    if let Some((mime, _)) = rest.split_once(';') {
                                        parts.push(json!({"type": "image", "image": url, "mimeType": mime}));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if !parts.is_empty() {
                    out.push(json!({"role": "user", "content": parts}));
                }
            }
        }
    }
    out
}

pub fn to_tools(tools: Option<&Vec<Value>>) -> Vec<Value> {
    tools.map(|arr| {
        arr.iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("function")
                && t.pointer("/function/name").and_then(Value::as_str).is_some())
            .map(|t| {
                let f = t.get("function").cloned().unwrap_or_else(|| json!({}));
                json!({
                    "name": f.get("name"),
                    "description": f.get("description").cloned().unwrap_or_else(|| json!("")),
                    "input_schema": f.get("parameters").cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                })
            })
            .collect()
    }).unwrap_or_default()
}

pub fn to_system(parts: &[String]) -> Vec<Value> {
    let texts: Vec<&String> = parts.iter().filter(|p| !p.is_empty()).collect();
    texts.iter().enumerate().map(|(i, t)| {
        json!({"type": "text", "text": if i == texts.len() - 1 { t.to_string() } else { format!("{t}\n") }})
    }).collect()
}

/// 构造 /alpha/generate 请求体（上游恒为流式）。
pub fn build_payload(body: &Value, model: &str, max_tokens: u64) -> Value {
    let empty = Vec::new();
    let messages = body.get("messages").and_then(Value::as_array).unwrap_or(&empty);
    let mut system_parts = Vec::new();
    let mut rest = Vec::new();
    for m in messages {
        if matches!(str_of(m.get("role")).as_str(), "system" | "developer") {
            system_parts.push(text_of(m.get("content")));
        } else {
            rest.push(m.clone());
        }
    }
    let mut params = Map::new();
    params.insert("model".into(), json!(model));
    params.insert("messages".into(), json!(to_messages(&rest)));
    params.insert("tools".into(), json!(to_tools(body.get("tools").and_then(Value::as_array))));
    params.insert("system".into(), json!(to_system(&system_parts)));
    let max_tokens = body.get("max_tokens").or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64).unwrap_or(max_tokens);
    params.insert("max_tokens".into(), json!(max_tokens));
    params.insert("stream".into(), json!(true));
    for key in ["temperature", "reasoning_effort"] {
        if let Some(v) = body.get(key).filter(|v| !v.is_null()) {
            params.insert(key.into(), v.clone());
        }
    }
    json!({
        "config": {
            "workingDir": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
            "date": chrono::Local::now().format("%Y-%m-%d").to_string(),
            "environment": std::env::consts::OS,
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "memory": Value::Null,
        "taste": Value::Null,
        "skills": Value::Null,
        "permissionMode": "standard",
        "threadId": thread_id(body),
        "params": Value::Object(params),
    })
}

/// 稳定 threadId（合法 UUID v4 形态）：同一对话重复请求得到同一个值。
fn thread_id(body: &Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let empty = Vec::new();
    let seed = body.get("messages").and_then(Value::as_array).unwrap_or(&empty)
        .first().map(|m| m.to_string()).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    let n = hasher.finish();
    let hex = format!("{n:016x}{n:016x}");
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..32]
    )
}
