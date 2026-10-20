//! OpenCode Zen 免费层 transport：客户端指纹、ID、URL 重写、工具适配与 SSE 聚合。
//!
//! 仅服务 `transport=opencode` 的免费模型；不读取 apiKey，不处理 go/付费。
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{json, Map, Value};
use uuid::Uuid;

pub const USER_AGENT: &str = "opencode/1.19.0 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";
pub const PUBLIC_KEY: &str = "public";
pub const STUB_DESCRIPTION: &str = "Not available in this session. Never call this tool.";
pub const STUB_NOTE: &str = "(The model asked for a tool this session doesn't have.)";
const OPENCODE_TOOLS: [&str; 2] = ["bash", "read"];
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub fn is_free_model(model: &str) -> bool {
    let name = model.trim();
    name.ends_with("-free") || matches!(name, "big-pickle" | "grok-code")
}

fn is_open_code_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "opencode.ai" || host.ends_with(".opencode.ai")
}

/// 把 /zen/... 重写到 /inference/...；/zen/go/... 与其它主机保持原样。
pub fn rewrite_free_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    if !is_open_code_host(parsed.host_str().unwrap_or("")) {
        return url.to_string();
    }
    let path = parsed.path().to_string();
    let rest = if path == "/zen" {
        Some("")
    } else {
        path.strip_prefix("/zen/")
    };
    let Some(rest) = rest else {
        return url.to_string();
    };
    if rest == "go" || rest.starts_with("go/") {
        return url.to_string();
    }
    let new_path = if rest.is_empty() {
        "/inference".to_string()
    } else {
        format!("/inference/{rest}")
    };
    parsed.set_path(&new_path);
    parsed.to_string()
}

fn valid_id_shape(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 30
        && b[..3].iter().all(u8::is_ascii_lowercase)
        && b[3] == b'_'
        && b[4..16].iter().all(u8::is_ascii_hexdigit)
        && b[16..].iter().all(u8::is_ascii_alphanumeric)
}

fn real_session(id: &str) -> bool {
    if !id.starts_with("ses_") || !valid_id_shape(id) {
        return false;
    }
    u16::from_str_radix(&id[13..16], 16)
        .map(|low| 0xfff - low < 16)
        .unwrap_or(false)
}

struct IdState {
    last: i64,
    counter: u64,
}

static ID_STATE: OnceLock<Mutex<IdState>> = OnceLock::new();

fn new_id(prefix: &str, descending: bool) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let state = ID_STATE.get_or_init(|| {
        Mutex::new(IdState {
            last: 0,
            counter: 0,
        })
    });
    let counter = {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if now_ms != guard.last {
            guard.last = now_ms;
            guard.counter = 0;
        }
        guard.counter += 1;
        guard.counter
    };
    let mut current = ((now_ms as u64) << 12).wrapping_add(counter);
    if descending {
        current = !current;
    }
    let random = Uuid::new_v4().into_bytes();
    let suffix: String = random
        .iter()
        .take(14)
        .map(|byte| BASE62[(*byte as usize) % BASE62.len()] as char)
        .collect();
    format!("{}_{:012x}{}", prefix, current & 0xFFFF_FFFF_FFFF, suffix)
}

pub fn session_seed(payload: &Value) -> String {
    let model = payload.get("model").and_then(Value::as_str).unwrap_or("");
    let content = payload
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|m| m.get("content"))
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "{model}|{}",
        serde_json::to_string(&content).unwrap_or_default()
    )
}

struct SessionState {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}

static SESSIONS: OnceLock<Mutex<SessionState>> = OnceLock::new();

pub fn session_for(seed: &str) -> String {
    if real_session(seed) {
        return seed.to_string();
    }
    if seed.is_empty() {
        return new_id("ses", true);
    }
    let state = SESSIONS.get_or_init(|| {
        Mutex::new(SessionState {
            map: HashMap::new(),
            order: VecDeque::new(),
        })
    });
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = guard.map.get(seed).cloned() {
        if let Some(pos) = guard.order.iter().position(|k| k.as_str() == seed) {
            guard.order.remove(pos);
        }
        guard.order.push_back(seed.to_string());
        return hit;
    }
    let value = new_id("ses", true);
    guard.map.insert(seed.to_string(), value.clone());
    guard.order.push_back(seed.to_string());
    while guard.order.len() > 1000 {
        if let Some(old) = guard.order.pop_front() {
            guard.map.remove(&old);
        }
    }
    value
}

/// 写入 OpenCode 客户端指纹头；调用方应先移除冲突的旧会话头。
pub fn apply_headers(headers: &mut HeaderMap, session: &str) {
    for name in [
        "x-session-affinity",
        "x-session-id",
        "x-opencode-session",
        "x-opencode-session-id",
        "x-opencode-request",
    ] {
        headers.remove(name);
    }
    let request = new_id("msg", false);
    for (name, value) in [
        ("authorization", format!("Bearer {PUBLIC_KEY}")),
        ("user-agent", USER_AGENT.to_string()),
        ("x-opencode-client", "cli".to_string()),
        ("x-opencode-project", "global".to_string()),
        ("x-opencode-session", session.to_string()),
        ("x-opencode-session-id", session.to_string()),
        ("x-opencode-request", request),
    ] {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolState {
    pub back: HashMap<String, String>,
    pub stubs: HashSet<String>,
}

fn tool_schema(name: &str) -> Value {
    match name {
        "bash" => json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to execute"},
                "timeout": {"type": "integer", "description": "Optional timeout in milliseconds"},
                "workdir": {"type": "string", "description": "The working directory to run the command in"}
            },
            "required": ["command"]
        }),
        _ => json!({
            "type": "object",
            "properties": {
                "filePath": {"type": "string", "description": "The absolute path to the file or directory to read"},
                "offset": {"type": "integer", "description": "The line number to start reading from (1-indexed)"},
                "limit": {"type": "integer", "description": "The maximum number of lines to read"}
            },
            "required": ["filePath"]
        }),
    }
}

fn lower_tool(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "bash" => Some("bash"),
        "read" => Some("read"),
        _ => None,
    }
}

pub fn prepare_tools(payload: &mut Value) -> ToolState {
    let mut state = ToolState::default();
    let Some(obj) = payload.as_object_mut() else {
        return state;
    };
    let mut tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = HashSet::new();
    for tool in tools.iter_mut() {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(name) = function
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if let Some(lower) = lower_tool(&name) {
            seen.insert(lower.to_string());
            if name != lower {
                state.back.entry(lower.to_string()).or_insert(name.clone());
                function.insert("name".to_string(), json!(lower));
            }
        }
    }
    for need in OPENCODE_TOOLS {
        if seen.contains(need) {
            continue;
        }
        tools.push(json!({
            "type": "function",
            "function": {
                "name": need,
                "description": STUB_DESCRIPTION,
                "parameters": tool_schema(need)
            }
        }));
        state.stubs.insert(need.to_string());
    }
    obj.insert("tools".to_string(), Value::Array(tools));

    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            let Some(message) = message.as_object_mut() else {
                continue;
            };
            if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for call in calls.iter_mut() {
                    let Some(function) = call.get_mut("function").and_then(Value::as_object_mut)
                    else {
                        continue;
                    };
                    let Some(name) = function
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    if let Some(lower) = lower_tool(&name) {
                        state.back.entry(lower.to_string()).or_insert(name.clone());
                        function.insert("name".to_string(), json!(lower));
                    }
                }
            }
            if matches!(
                message.get("role").and_then(Value::as_str),
                Some("tool") | Some("function")
            ) {
                if let Some(name) = message
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    if let Some(lower) = lower_tool(&name) {
                        state.back.entry(lower.to_string()).or_insert(name.clone());
                        message.insert("name".to_string(), json!(lower));
                    }
                }
            }
        }
    }
    if let Some(choice) = obj.get_mut("tool_choice").and_then(Value::as_object_mut) {
        if let Some(function) = choice.get_mut("function").and_then(Value::as_object_mut) {
            if let Some(name) = function
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                if let Some(lower) = lower_tool(&name) {
                    state.back.entry(lower.to_string()).or_insert(name.clone());
                    function.insert("name".to_string(), json!(lower));
                }
            }
        }
    }
    state
}

pub struct StreamRewriter {
    back: HashMap<String, String>,
    stubs: HashSet<String>,
    dropping: HashSet<u64>,
    calls: HashSet<u64>,
    said: bool,
    stubbed: bool,
    requested_model: String,
}

impl StreamRewriter {
    pub fn new(state: &ToolState, requested_model: &str) -> Self {
        Self {
            back: state.back.clone(),
            stubs: state.stubs.clone(),
            dropping: HashSet::new(),
            calls: HashSet::new(),
            said: false,
            stubbed: false,
            requested_model: requested_model.to_string(),
        }
    }

    pub fn rewrite(&mut self, chunk: &mut Value) {
        if let Some(obj) = chunk.as_object_mut() {
            if obj.contains_key("model") {
                obj.insert("model".to_string(), json!(self.requested_model));
            }
        }
        let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
            return;
        };
        for choice in choices.iter_mut() {
            let Some(choice) = choice.as_object_mut() else {
                continue;
            };
            let mut delta = choice
                .remove("delta")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            if delta
                .get("content")
                .and_then(Value::as_str)
                .map(str::is_empty)
                == Some(false)
            {
                self.said = true;
            }
            if let Some(calls) = delta
                .remove("tool_calls")
                .and_then(|v| v.as_array().cloned())
            {
                let mut kept = Vec::new();
                for mut call in calls {
                    let Some(call_obj) = call.as_object_mut() else {
                        continue;
                    };
                    let index = call_obj.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let name = call_obj
                        .get("function")
                        .and_then(Value::as_object)
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(name) = name {
                        if self.stubs.contains(&name) {
                            self.dropping.insert(index);
                            self.stubbed = true;
                            continue;
                        }
                        if let Some(mapped) = self.back.get(&name) {
                            if let Some(function) =
                                call_obj.get_mut("function").and_then(Value::as_object_mut)
                            {
                                function.insert("name".to_string(), json!(mapped));
                            }
                        }
                        self.calls.insert(index);
                    } else if self.dropping.contains(&index) {
                        continue;
                    }
                    kept.push(call);
                }
                if kept.is_empty() {
                    delta.remove("tool_calls");
                } else {
                    delta.insert("tool_calls".to_string(), Value::Array(kept));
                }
            }
            if let Some(finish) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                if self.stubbed && self.calls.is_empty() {
                    if finish == "tool_calls" {
                        choice.insert("finish_reason".to_string(), json!("stop"));
                    }
                    if !self.said {
                        delta.insert("content".to_string(), json!(STUB_NOTE));
                        self.said = true;
                    }
                }
            }
            choice.insert("delta".to_string(), Value::Object(delta));
        }
    }

    pub fn finalize(&mut self) -> Option<Value> {
        if !self.stubbed || !self.calls.is_empty() || self.said {
            return None;
        }
        self.said = true;
        Some(json!({
            "id": "chatcmpl-opencode",
            "object": "chat.completion.chunk",
            "created": now_seconds(),
            "model": self.requested_model,
            "choices": [{
                "index": 0,
                "delta": {"content": STUB_NOTE},
                "finish_reason": "stop"
            }]
        }))
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Default)]
struct ToolCallSlot {
    id: String,
    name: String,
    arguments: String,
}

/// 把上游 SSE 聚合成非流式 chat.completion。
pub fn aggregate_sse(
    raw: &str,
    state: &ToolState,
    requested_model: &str,
) -> (Value, Option<Value>) {
    let mut rewriter = StreamRewriter::new(state, requested_model);
    let mut text = String::new();
    let mut calls: HashMap<u64, ToolCallSlot> = HashMap::new();
    let mut finish_reason = "stop".to_string();
    let mut usage: Option<Value> = None;
    let mut first = Map::new();
    for line in raw.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let Ok(mut chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if first.is_empty() {
            if let Some(obj) = chunk.as_object() {
                first = obj.clone();
            }
        }
        if let Some(u) = chunk.get("usage") {
            usage = Some(u.clone());
        }
        rewriter.rewrite(&mut chunk);
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for choice in choices {
            let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
                continue;
            };
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                text.push_str(content);
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let slot = calls.entry(index).or_default();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        slot.id = id.to_string();
                    }
                    if let Some(function) = call.get("function").and_then(Value::as_object) {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            slot.name = name.to_string();
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            slot.arguments.push_str(arguments);
                        }
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                finish_reason = reason.to_string();
            }
        }
    }
    if let Some(tail) = rewriter.finalize() {
        if let Some(choices) = tail.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(content) = choice
                    .get("delta")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("content"))
                    .and_then(Value::as_str)
                {
                    text.push_str(content);
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = reason.to_string();
                }
            }
        }
    }
    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert(
        "content".to_string(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !calls.is_empty() {
        let mut indexes: Vec<u64> = calls.keys().copied().collect();
        indexes.sort_unstable();
        let tool_calls: Vec<Value> = indexes
            .into_iter()
            .map(|index| {
                let slot = calls.remove(&index).unwrap_or_default();
                json!({
                    "id": slot.id,
                    "type": "function",
                    "function": {"name": slot.name, "arguments": slot.arguments}
                })
            })
            .collect();
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        if finish_reason == "stop" {
            finish_reason = "tool_calls".to_string();
        }
    }
    let body = json!({
        "id": first.get("id").and_then(Value::as_str).unwrap_or("chatcmpl-opencode"),
        "object": "chat.completion",
        "created": first.get("created").and_then(Value::as_i64).unwrap_or_else(now_seconds),
        "model": requested_model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason
        }],
        "usage": usage.clone().unwrap_or_else(|| json!({
            "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0
        }))
    });
    (body, usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_models_and_url() {
        assert!(is_free_model("mimo-v2.6-flash-free"));
        assert!(is_free_model("big-pickle"));
        assert!(is_free_model("grok-code"));
        assert!(!is_free_model("paid"));
        assert_eq!(
            rewrite_free_url("https://opencode.ai/zen/v1/chat/completions"),
            "https://opencode.ai/inference/v1/chat/completions"
        );
        assert_eq!(
            rewrite_free_url("https://opencode.ai/zen/go/v1/chat/completions"),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
    }

    #[test]
    fn ids_have_opencode_shape() {
        let id = new_id("ses", true);
        assert!(valid_id_shape(&id));
        assert!(real_session(&id));
        let request = new_id("msg", false);
        assert!(valid_id_shape(&request));
        assert!(request.starts_with("msg_"));
    }

    #[test]
    fn tools_are_renamed_and_stubbed() {
        let mut payload = json!({
            "tools": [{"type": "function", "function": {"name": "Bash", "parameters": {}}}],
            "tool_choice": {"type": "function", "function": {"name": "Bash"}}
        });
        let state = prepare_tools(&mut payload);
        assert_eq!(state.back.get("bash").map(String::as_str), Some("Bash"));
        assert!(state.stubs.contains("read"));
        let names: Vec<&str> = payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["bash", "read"]);
        assert_eq!(payload["tool_choice"]["function"]["name"], "bash");
    }

    #[test]
    fn aggregate_maps_and_filters() {
        let raw = concat!(
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",",
            "\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]},",
            "\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},",
            "\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        let state = ToolState {
            back: HashMap::from([("bash".to_string(), "Bash".to_string())]),
            stubs: HashSet::from(["read".to_string()]),
        };
        let (body, usage) = aggregate_sse(raw, &state, "m");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "Bash"
        );
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        assert_eq!(usage.unwrap()["prompt_tokens"], 1);
    }

    #[test]
    fn stub_only_gets_note() {
        let raw = concat!(
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":",
            "{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",",
            "\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        let state = ToolState {
            back: HashMap::new(),
            stubs: HashSet::from(["read".to_string()]),
        };
        let (body, _) = aggregate_sse(raw, &state, "m");
        assert_eq!(body["choices"][0]["message"]["content"], STUB_NOTE);
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
    }
}
