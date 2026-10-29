//! 提示词记录、脱敏与本地差异分析。
//!
//! 本模块不调用任何模型或网络：脱敏是规则匹配，差异是 LCS/行级 diff，
//! 统计只基于消息文本。所有输出均可指回具体消息与行号。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const REDACTED_BEARER: &str = "[REDACTED:bearer]";
pub const REDACTED_KEY: &str = "[REDACTED:key]";
pub const REDACTED_FIELD: &str = "[REDACTED:field]";
pub const REDACTED_URL: &str = "[REDACTED:url]";
pub const REDACTED_PRIVATE_KEY: &str = "[REDACTED:private_key]";

/// 脱敏规则版本：写入记录时固化，便于审查时判断当时使用的规则集。
/// 修改 `redact_text` 的规则后必须递增。
pub const REDACTION_POLICY_VERSION: i64 = 1;

#[derive(Debug, Clone, Default)]
pub struct EncodedPrompt {
    pub messages: String,
    pub truncated: bool,
    pub message_count: i64,
    pub size_bytes: i64,
    pub preview: String,
    pub redactions: usize,
    /// 截断审查元数据（只含计数、索引与策略，不含任何消息原文）。
    pub truncation: Option<Value>,
    /// 脱敏后完整 messages 的 SHA-256 前 16 位，用于跨记录比对。
    pub content_hash: String,
    /// system 消息的稳定摘要，用于发现 system prompt 漂移。
    pub system_hash: String,
    /// 角色分布 JSON，例如 `{"system":1,"user":2}`；不含内容。
    pub role_counts: String,
}

/// 同一请求 ID 下，提示词记录和用量记录使用的上下文。
#[derive(Debug, Clone, Default)]
pub struct RequestCtx {
    pub id: String,
    pub session: String,
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "apikey"
            | "api_key"
            | "authorization"
            | "token"
            | "access_token"
            | "refresh_token"
            | "secret"
            | "password"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
    ) || lower.contains("secret")
        || lower.contains("password")
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

fn scan_token_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut i = start;
    while i < bytes.len() && is_token_byte(bytes[i]) {
        i += 1;
    }
    i
}

/// 从 `index`（必须是字符边界）前进一个完整字符；用于扫描无匹配时避免落入 UTF-8 多字节中间。
fn next_char_boundary(input: &str, index: usize) -> usize {
    input[index..]
        .chars()
        .next()
        .map(|c| index + c.len_utf8())
        .unwrap_or(index)
}

fn redact_private_keys(input: &str, count: &mut usize) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let Some(begin) = rest.find("-----BEGIN ") else {
            out.push_str(rest);
            break;
        };
        let after_begin = &rest[begin..];
        let Some(header_rel) = after_begin.find("PRIVATE KEY-----") else {
            out.push_str(&rest[..begin + "-----BEGIN ".len()]);
            rest = &rest[begin + "-----BEGIN ".len()..];
            continue;
        };
        let header_end = begin + header_rel + "PRIVATE KEY-----".len();
        let after_header = &rest[header_end..];
        let Some(end_rel) = after_header.find("-----END ") else {
            out.push_str(rest);
            break;
        };
        let end_start = header_end + end_rel;
        let after_end = &rest[end_start..];
        let Some(footer_rel) = after_end.find("PRIVATE KEY-----") else {
            out.push_str(rest);
            break;
        };
        let end = end_start + footer_rel + "PRIVATE KEY-----".len();
        out.push_str(&rest[..begin]);
        out.push_str(REDACTED_PRIVATE_KEY);
        *count += 1;
        rest = &rest[end..];
    }
    out
}

fn redact_bearer(input: &str, count: &mut usize) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(pos) = lower[search..].find("bearer ") {
        let start = search + pos;
        let token_start = start + "bearer ".len();
        let token_end = scan_token_end(input, token_start);
        if token_end > token_start + 6 {
            out.push_str(&input[last..token_start]);
            out.push_str(REDACTED_BEARER);
            last = token_end;
            *count += 1;
        }
        search = if token_end > token_start {
            token_end
        } else {
            next_char_boundary(input, token_start)
        };
    }
    out.push_str(&input[last..]);
    out
}

fn redact_authorization(input: &str, count: &mut usize) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(pos) = lower[search..].find("authorization") {
        let start = search + pos;
        let after = &input[start + "authorization".len()..];
        let Some(colon) = after.find(':') else {
            search = start + "authorization".len();
            continue;
        };
        let value_start = start + "authorization".len() + colon + 1;
        let mut token_start = value_start;
        let bytes = input.as_bytes();
        while token_start < bytes.len() && (bytes[token_start] as char).is_whitespace() {
            token_start += 1;
        }
        let token_end = scan_token_end(input, token_start);
        if token_end > token_start + 6 {
            out.push_str(&input[last..token_start]);
            out.push_str(REDACTED_BEARER);
            last = token_end;
            *count += 1;
        }
        search = if token_end > token_start {
            token_end
        } else {
            next_char_boundary(input, token_start)
        };
    }
    out.push_str(&input[last..]);
    out
}

fn redact_query_param(input: &str, name: &str, marker: &str, count: &mut usize) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = format!("{name}=");
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(pos) = lower[search..].find(&needle) {
        let start = search + pos;
        let prev = input[..start].chars().next_back();
        if !matches!(prev, None | Some('?') | Some('&') | Some(';'))
            && !matches!(prev, Some(c) if c.is_whitespace())
        {
            search = start + needle.len();
            continue;
        }
        let value_start = start + needle.len();
        let bytes = input.as_bytes();
        let mut value_end = value_start;
        while value_end < bytes.len()
            && !matches!(
                bytes[value_end],
                b'&' | b'#' | b'"' | b'\'' | b' ' | b'\n' | b'\r'
            )
        {
            value_end += 1;
        }
        if value_end > value_start {
            out.push_str(&input[last..value_start]);
            out.push_str(marker);
            last = value_end;
            *count += 1;
        }
        search = if value_end > value_start {
            value_end
        } else {
            next_char_boundary(input, value_start)
        };
    }
    out.push_str(&input[last..]);
    out
}

fn redact_prefix(input: &str, prefix: &str, min_token: usize, count: &mut usize) -> String {
    let lower = input.to_ascii_lowercase();
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(pos) = lower[search..].find(&prefix_lower) {
        let start = search + pos;
        let prev = input[..start].chars().next_back();
        if matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == '_') {
            search = start + prefix.len();
            continue;
        }
        let token_end = scan_token_end(input, start);
        if token_end >= start + min_token {
            out.push_str(&input[last..start]);
            out.push_str(REDACTED_KEY);
            last = token_end;
            *count += 1;
        }
        search = token_end.max(start + prefix.len());
    }
    out.push_str(&input[last..]);
    out
}

/// 对单段文本做规则脱敏；返回脱敏文本与替换次数。
pub fn redact_text(input: &str) -> (String, usize) {
    let mut count = 0usize;
    let mut text = redact_private_keys(input, &mut count);
    text = redact_bearer(&text, &mut count);
    text = redact_authorization(&text, &mut count);
    for (name, marker) in [
        ("api_key", REDACTED_URL),
        ("access_token", REDACTED_URL),
        ("token", REDACTED_URL),
        ("key", REDACTED_URL),
    ] {
        text = redact_query_param(&text, name, marker, &mut count);
    }
    for (prefix, min) in [
        ("sk-ant-", 12usize),
        ("sk-", 10),
        ("ghp_", 10),
        ("xoxb-", 10),
        ("AIza", 10),
        ("AKIA", 10),
        ("eyJ", 20),
    ] {
        text = redact_prefix(&text, prefix, min, &mut count);
    }
    (text, count)
}

fn redact_value(value: &Value, count: &mut usize) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, val) in map {
                if is_sensitive_key(key) {
                    *count += 1;
                    out.insert(key.clone(), json!(REDACTED_FIELD));
                } else {
                    out.insert(key.clone(), redact_value(val, count));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redact_value(item, count)).collect())
        }
        Value::String(text) => {
            let (redacted, n) = redact_text(text);
            *count += n;
            Value::String(redacted)
        }
        other => other.clone(),
    }
}

/// 消息内容转文本：字符串直接用；多模态数组拼 text/占位。
pub fn content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "[{}]",
                            part.get("type").and_then(Value::as_str).unwrap_or("part")
                        )
                    })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn preview_of(messages: &[Value]) -> String {
    let text = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .or_else(|| messages.last())
        .map(content_text)
        .unwrap_or_default();
    text.chars().take(200).collect()
}

fn sha256_hex16(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex.chars().take(16).collect()
}

fn role_counts_json(messages: &[Value]) -> String {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *counts.entry(role).or_insert(0) += 1;
    }
    serde_json::to_string(&counts).unwrap_or_else(|_| "{}".into())
}

/// 把单条消息的 content 收窄到指定字符数；返回（新消息，保留字符数，原字符数）。
/// 只用于预算兜底，会把结构化 content 合并为文本，与原有截断行为一致。
fn shorten_message(message: &Value, keep_chars: usize) -> (Value, usize, usize) {
    let text = content_text(message);
    let original_chars = text.chars().count();
    let keep = keep_chars.max(16).min(original_chars);
    let cut: String = text.chars().take(keep).collect();
    let mut shortened = message.clone();
    if let Some(obj) = shortened.as_object_mut() {
        obj.insert(
            "content".into(),
            Value::String(format!("{cut}…[truncated]")),
        );
    }
    (shortened, keep, original_chars)
}

fn rebuild_messages(kept: &[(usize, Value)]) -> String {
    Value::Array(kept.iter().map(|(_, message)| message.clone()).collect()).to_string()
}

/// 头尾保留截断：头部保留 system/开头上下文，尾部保留最近消息与工具结果，
/// 中间丢弃的消息只记录数量与索引，不写入任何原文。
fn encode_head_tail(messages: &[Value], max_chars: usize) -> (String, bool, Option<Value>) {
    let max_chars = max_chars.max(1000);
    let full = Value::Array(messages.to_vec()).to_string();
    let original_chars = full.chars().count();
    if original_chars <= max_chars {
        return (full, false, None);
    }

    let original_messages = messages.len();
    // 预留数组括号、逗号与序列化余量；预算按 40% 头部 / 60% 尾部拆分。
    let overhead = 2 + original_messages.saturating_sub(1) + 8;
    let budget = max_chars.saturating_sub(overhead);
    let head_budget = (budget * 40 / 100).max(64);
    let tail_budget = budget.saturating_sub(head_budget).max(64);

    let mut head: Vec<(usize, Value)> = Vec::new();
    let mut head_used = 0usize;
    let mut cuts: Vec<Value> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let len = message.to_string().chars().count() + 1;
        if !head.is_empty() && head_used + len > head_budget {
            break;
        }
        if head.is_empty() && len > head_budget {
            let (shortened, kept_chars, original) =
                shorten_message(message, head_budget.saturating_sub(64));
            head_used += shortened.to_string().chars().count() + 1;
            head.push((index, shortened));
            cuts.push(json!({
                "side": "head",
                "original_index": index,
                "kept_chars": kept_chars,
                "original_chars": original,
            }));
            break;
        }
        head.push((index, message.clone()));
        head_used += len;
        if head_used >= head_budget {
            break;
        }
    }

    let head_end = head.last().map(|(index, _)| index + 1).unwrap_or(0);
    let mut tail: Vec<(usize, Value)> = Vec::new();
    let mut tail_used = 0usize;
    for (index, message) in messages.iter().enumerate().rev() {
        if index < head_end {
            break;
        }
        let len = message.to_string().chars().count() + 1;
        if !tail.is_empty() && tail_used + len > tail_budget {
            break;
        }
        if tail.is_empty() && len > tail_budget {
            let (shortened, kept_chars, original) =
                shorten_message(message, tail_budget.saturating_sub(64));
            tail_used += shortened.to_string().chars().count() + 1;
            tail.push((index, shortened));
            cuts.push(json!({
                "side": "tail",
                "original_index": index,
                "kept_chars": kept_chars,
                "original_chars": original,
            }));
            break;
        }
        tail.push((index, message.clone()));
        tail_used += len;
        if tail_used >= tail_budget {
            break;
        }
    }
    tail.reverse();

    let mut kept: Vec<(usize, Value)> = head;
    kept.extend(tail);
    let mut text = rebuild_messages(&kept);

    // 兜底 1：预算估算仍有偏差时，只收窄最后一条消息，保留头部与最新上下文。
    if text.chars().count() > max_chars {
        if let Some((index, message)) = kept.last().cloned() {
            let over = text.chars().count().saturating_sub(max_chars) + 32;
            let keep = content_text(&message)
                .chars()
                .count()
                .saturating_sub(over)
                .max(16);
            let (shortened, kept_chars, original) = shorten_message(&message, keep);
            kept.pop();
            kept.push((index, shortened));
            cuts.push(json!({
                "side": "tail",
                "original_index": index,
                "kept_chars": kept_chars,
                "original_chars": original,
            }));
            text = rebuild_messages(&kept);
        }
    }

    // 兜底 2：仍超限时，从尾部较旧的消息开始丢弃，至少保留头部第一条与最后一条。
    while text.chars().count() > max_chars && kept.len() > 2 {
        kept.remove(kept.len() - 2);
        text = rebuild_messages(&kept);
    }

    // 兜底 3：只剩一条仍超限时硬截断 content，保证落库内容不超过预算。
    if text.chars().count() > max_chars {
        if let Some((index, message)) = kept.last().cloned() {
            let keep = max_chars.saturating_sub(96).max(16);
            let (shortened, kept_chars, original) = shorten_message(&message, keep);
            kept.pop();
            kept.push((index, shortened));
            cuts.push(json!({
                "side": "tail",
                "original_index": index,
                "kept_chars": kept_chars,
                "original_chars": original,
            }));
            text = rebuild_messages(&kept);
        }
    }

    // 依据最终保留的原始索引重新计算头/尾边界，确保审查元数据与内容一致。
    let head_count = kept
        .iter()
        .enumerate()
        .take_while(|(position, entry)| entry.0 == *position)
        .count();
    let mut tail_start = original_messages;
    for (index, _) in kept.iter().rev() {
        if *index + 1 == tail_start {
            tail_start = *index;
        } else {
            break;
        }
    }

    let kept_messages = kept.len();
    let kept_chars = text.chars().count();
    let dropped_messages = original_messages.saturating_sub(kept_messages);
    let dropped_chars = original_chars.saturating_sub(kept_chars);
    let truncated = dropped_messages > 0 || !cuts.is_empty();
    let truncation = if truncated {
        Some(json!({
            "strategy": "head_tail",
            "max_chars": max_chars,
            "original_chars": original_chars,
            "kept_chars": kept_chars,
            "original_messages": original_messages,
            "kept_messages": kept_messages,
            "head_messages": head_count,
            "tail_messages": kept_messages.saturating_sub(head_count),
            "head_end_index": head_count,
            "tail_start_index": tail_start,
            "dropped_messages": dropped_messages,
            "dropped_chars": dropped_chars,
            "cuts": cuts,
        }))
    } else {
        None
    };
    (text, truncated, truncation)
}

/// 先脱敏、再做大小预算截断，生成可直接落库的提示词记录。
///
/// 脱敏必须先于截断和所有元数据计算：预览、哈希、角色统计都只能基于脱敏后的内容。
pub fn prepare_messages(messages: &[Value], max_chars: usize) -> EncodedPrompt {
    let mut redactions = 0usize;
    let redacted = Value::Array(
        messages
            .iter()
            .map(|message| redact_value(message, &mut redactions))
            .collect(),
    );
    let arr = redacted.as_array().cloned().unwrap_or_default();
    let message_count = arr.len() as i64;
    let preview = preview_of(&arr);
    let content_hash = sha256_hex16(&Value::Array(arr.clone()).to_string());
    let system_hash_value = system_hash(&arr);
    let role_counts = role_counts_json(&arr);
    let (text, truncated, truncation) = encode_head_tail(&arr, max_chars);
    EncodedPrompt {
        messages: text.clone(),
        truncated,
        message_count,
        size_bytes: text.len() as i64,
        preview,
        redactions,
        truncation,
        content_hash,
        system_hash: system_hash_value,
        role_counts,
    }
}

/// system prompt 的稳定摘要，用于判断是否变化（不保存额外原文）。
pub fn system_hash(messages: &[Value]) -> String {
    let system = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

/// 单条请求内消息调用链摘要：突出 tool_calls 与 tool 结果的配对、角色顺序与关键预览。
/// 只含预览与计数，不包含完整消息原文。
pub fn summarize_call_chain(messages: &[Value]) -> Value {
    let mut role_sequence: Vec<&str> = Vec::with_capacity(messages.len());
    let mut assistant_turns = 0usize;
    let mut user_turns = 0usize;
    let mut system_turns = 0usize;
    let mut tool_turns = 0usize;
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: std::collections::BTreeMap<String, Value> =
        std::collections::BTreeMap::new();
    let mut system_preview = String::new();
    let mut last_user_preview = String::new();
    let mut last_assistant_preview = String::new();
    let mut message_previews: Vec<Value> = Vec::with_capacity(messages.len());

    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        role_sequence.push(role);
        let text = content_text(message);
        let mut preview_tool_names: Vec<String> = Vec::new();
        match role {
            "system" => {
                system_turns += 1;
                if system_preview.is_empty() {
                    system_preview = text.chars().take(160).collect();
                }
            }
            "user" => {
                user_turns += 1;
                last_user_preview = text.chars().take(200).collect();
            }
            "assistant" => {
                assistant_turns += 1;
                if !text.trim().is_empty() {
                    last_assistant_preview = text.chars().take(200).collect();
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let function = call.get("function");
                        let name = function
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        let arguments = function
                            .and_then(|f| f.get("arguments"))
                            .map(|v| {
                                if let Some(s) = v.as_str() {
                                    s.to_string()
                                } else {
                                    v.to_string()
                                }
                            })
                            .unwrap_or_default();
                        let arguments_preview: String = arguments.chars().take(200).collect();
                        preview_tool_names.push(name.clone());
                        tool_calls.push(json!({
                            "id": id,
                            "name": name,
                            "arguments_preview": arguments_preview,
                            "assistant_index": index,
                        }));
                    }
                }
            }
            "tool" => {
                tool_turns += 1;
                let id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = message
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let preview: String = text.chars().take(200).collect();
                tool_results.insert(
                    id.clone(),
                    json!({
                        "tool_call_id": id,
                        "name": name,
                        "content_preview": preview,
                        "message_index": index,
                    }),
                );
            }
            _ => {}
        }

        let preview_text = if text.trim().is_empty() && !preview_tool_names.is_empty() {
            format!("[tool_calls] {}", preview_tool_names.join(", "))
        } else {
            text.chars().take(160).collect()
        };
        let mut entry = json!({
            "index": index,
            "role": role,
            "chars": text.chars().count(),
            "preview": preview_text,
        });
        if !preview_tool_names.is_empty() {
            entry["tool_names"] = json!(preview_tool_names);
        }
        if role == "tool" {
            entry["tool_name"] = message
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            entry["tool_call_id"] = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
        }
        message_previews.push(entry);
    }

    let mut pairs: Vec<Value> = Vec::with_capacity(tool_calls.len());
    for call in &tool_calls {
        let id = call.get("id").and_then(Value::as_str).unwrap_or("");
        let result = tool_results.get(id);
        pairs.push(json!({
            "id": id,
            "name": call.get("name").cloned().unwrap_or(Value::Null),
            "arguments_preview": call.get("arguments_preview").cloned().unwrap_or(Value::Null),
            "assistant_index": call.get("assistant_index").cloned().unwrap_or(Value::Null),
            "has_result": result.is_some(),
            "result_preview": result
                .and_then(|r| r.get("content_preview").cloned())
                .unwrap_or(Value::Null),
            "result_index": result
                .and_then(|r| r.get("message_index").cloned())
                .unwrap_or(Value::Null),
        }));
    }

    let message_previews = if message_previews.len() > 80 {
        let total = message_previews.len();
        let mut selected = message_previews[..10].to_vec();
        selected.push(json!({
            "index": -1,
            "role": "_gap",
            "chars": 0,
            "preview": format!("中间省略 {} 条消息", total - 40),
        }));
        selected.extend_from_slice(&message_previews[total - 30..]);
        selected
    } else {
        message_previews
    };

    json!({
        "message_count": messages.len(),
        "role_sequence": role_sequence,
        "assistant_turns": assistant_turns,
        "user_turns": user_turns,
        "system_turns": system_turns,
        "tool_turns": tool_turns,
        "tool_call_count": tool_calls.len(),
        "tool_result_count": tool_results.len(),
        "unmatched_tool_calls": tool_calls.len().saturating_sub(tool_results.len()),
        "tool_calls": tool_calls,
        "tool_results": tool_results.values().cloned().collect::<Vec<_>>(),
        "pairs": pairs,
        "system_preview": system_preview,
        "last_user_preview": last_user_preview,
        "last_assistant_preview": last_assistant_preview,
        "message_previews": message_previews,
    })
}

/// 单条消息的确定性统计，供会话演进时间轴使用。
pub fn summarize_messages(messages: &[Value]) -> Value {
    let char_count: usize = messages
        .iter()
        .map(|m| content_text(m).chars().count())
        .sum();
    let system_chars: usize = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .map(|m| content_text(m).chars().count())
        .sum();
    let user_chars: usize = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|m| content_text(m).chars().count())
        .sum();
    let tool_call_count: usize = messages
        .iter()
        .filter_map(|m| m.get("tool_calls").and_then(Value::as_array))
        .map(|calls| calls.len())
        .sum();
    let tool_result_count = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
        .count();
    let assistant_messages = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .count();
    let user_messages = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .count();
    let mut tool_names: Vec<String> = messages
        .iter()
        .filter_map(|m| m.get("tool_calls").and_then(Value::as_array))
        .flatten()
        .filter_map(|call| {
            call.get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| call.get("name").and_then(Value::as_str).map(str::to_string))
        })
        .collect();
    tool_names.sort();
    tool_names.dedup();
    json!({
        "message_count": messages.len(),
        "char_count": char_count,
        "system_chars": system_chars,
        "user_chars": user_chars,
        "system_hash": system_hash(messages),
        "estimated_tokens": (char_count / 4) as i64,
        "assistant_messages": assistant_messages,
        "user_messages": user_messages,
        "tool_call_count": tool_call_count,
        "tool_result_count": tool_result_count,
        "tool_names": tool_names,
    })
}

fn lcs_ops(base: &[String], target: &[String]) -> (Vec<(u8, usize, usize)>, bool) {
    let n = base.len();
    let m = target.len();
    let cap = 200usize;
    let bn = n.min(cap);
    let tm = m.min(cap);
    let truncated = n > cap || m > cap;
    let mut dp = vec![vec![0usize; tm + 1]; bn + 1];
    for i in 0..bn {
        for j in 0..tm {
            dp[i + 1][j + 1] = if base[i] == target[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (bn, tm);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && base[i - 1] == target[j - 1] {
            ops.push((0u8, i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push((1u8, 0, j - 1));
            j -= 1;
        } else {
            ops.push((2u8, i - 1, 0));
            i -= 1;
        }
    }
    ops.reverse();
    (ops, truncated)
}

fn line_ops(base: &[String], target: &[String]) -> Vec<(u8, Option<usize>, Option<usize>, String)> {
    let n = base.len();
    let m = target.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if base[i] == target[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && base[i - 1] == target[j - 1] {
            ops.push((0u8, Some(i - 1), Some(j - 1), base[i - 1].clone()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push((1u8, None, Some(j - 1), target[j - 1].clone()));
            j -= 1;
        } else {
            ops.push((2u8, Some(i - 1), None, base[i - 1].clone()));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

fn line_diff(base: &str, target: &str, context: usize) -> (Vec<Value>, bool) {
    const LINE_CAP: usize = 2000;
    let mut base_lines: Vec<String> = base.split('\n').map(str::to_string).collect();
    let mut target_lines: Vec<String> = target.split('\n').map(str::to_string).collect();
    let mut truncated = false;
    if base_lines.len() > LINE_CAP {
        base_lines.truncate(LINE_CAP);
        truncated = true;
    }
    if target_lines.len() > LINE_CAP {
        target_lines.truncate(LINE_CAP);
        truncated = true;
    }
    let ops = line_ops(&base_lines, &target_lines);
    let changed: Vec<bool> = ops.iter().map(|(kind, _, _, _)| *kind != 0).collect();
    let mut include = vec![false; ops.len()];
    for (idx, change) in changed.iter().enumerate() {
        if *change {
            let start = idx.saturating_sub(context);
            let end = (idx + context + 1).min(ops.len());
            for slot in include.iter_mut().take(end).skip(start) {
                *slot = true;
            }
        }
    }
    let mut out = Vec::new();
    for (idx, (kind, base_idx, target_idx, text)) in ops.iter().enumerate() {
        if !include[idx] {
            continue;
        }
        out.push(json!({
            "kind": match kind { 0 => "context", 1 => "add", _ => "del" },
            "base_line": base_idx.map(|v| v + 1),
            "target_line": target_idx.map(|v| v + 1),
            "text": text,
        }));
        if out.len() >= LINE_CAP {
            truncated = true;
            break;
        }
    }
    (out, truncated)
}

fn role_of(message: &Value) -> String {
    message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn message_key(message: &Value) -> String {
    format!("{}\u{1f}{}", role_of(message), content_text(message))
}

/// 消息级 LCS + 修改消息行级 diff。返回结构化 JSON（无自然语言建议）。
pub fn diff_messages(base: &[Value], target: &[Value], context: usize) -> Value {
    let base_keys: Vec<String> = base.iter().map(message_key).collect();
    let target_keys: Vec<String> = target.iter().map(message_key).collect();
    let (ops, mut truncated) = lcs_ops(&base_keys, &target_keys);
    let mut rows = Vec::new();
    let mut pending_del: Vec<usize> = Vec::new();
    let mut pending_ins: Vec<usize> = Vec::new();
    let mut stats = Stats::default();
    stats.base_chars = base.iter().map(|m| content_text(m).chars().count()).sum();
    stats.target_chars = target.iter().map(|m| content_text(m).chars().count()).sum();

    let mut flush = |rows: &mut Vec<Value>,
                     del: &mut Vec<usize>,
                     ins: &mut Vec<usize>,
                     stats: &mut Stats,
                     truncated: &mut bool| {
        let mut used_ins = vec![false; ins.len()];
        for (di, bi) in del.iter().enumerate() {
            let base_role = role_of(&base[*bi]);
            let mut paired = None;
            for (ii, ti) in ins.iter().enumerate() {
                if !used_ins[ii] && role_of(&target[*ti]) == base_role {
                    paired = Some((ii, *ti));
                    break;
                }
            }
            if let Some((ii, ti)) = paired {
                used_ins[ii] = true;
                let base_text = content_text(&base[*bi]);
                let target_text = content_text(&target[ti]);
                let (diff, cut) = line_diff(&base_text, &target_text, context);
                *truncated |= cut;
                stats.modified += 1;
                *stats.by_role.entry(base_role.clone()).or_insert(0) += 1;
                rows.push(json!({
                    "status": "modified",
                    "role": base_role,
                    "base_index": bi,
                    "target_index": ti,
                    "base_chars": base_text.chars().count(),
                    "target_chars": target_text.chars().count(),
                    "diff": diff,
                }));
            } else {
                let text = content_text(&base[*bi]);
                stats.removed += 1;
                *stats.by_role.entry(role_of(&base[*bi])).or_insert(0) += 1;
                rows.push(json!({
                    "status": "removed",
                    "role": role_of(&base[*bi]),
                    "base_index": bi,
                    "target_index": Value::Null,
                    "base_chars": text.chars().count(),
                    "target_chars": 0,
                    "content": text.chars().take(4000).collect::<String>(),
                }));
            }
            let _ = di;
        }
        for (ii, ti) in ins.iter().enumerate() {
            if used_ins[ii] {
                continue;
            }
            let text = content_text(&target[*ti]);
            stats.added += 1;
            *stats.by_role.entry(role_of(&target[*ti])).or_insert(0) += 1;
            rows.push(json!({
                "status": "added",
                "role": role_of(&target[*ti]),
                "base_index": Value::Null,
                "target_index": ti,
                "base_chars": 0,
                "target_chars": text.chars().count(),
                "content": text.chars().take(4000).collect::<String>(),
            }));
        }
        del.clear();
        ins.clear();
    };

    for (kind, bi, ti) in ops {
        match kind {
            0 => {
                flush(
                    &mut rows,
                    &mut pending_del,
                    &mut pending_ins,
                    &mut stats,
                    &mut truncated,
                );
                stats.same += 1;
                *stats.by_role.entry(role_of(&base[bi])).or_insert(0) += 1;
                rows.push(json!({
                    "status": "same",
                    "role": role_of(&base[bi]),
                    "base_index": bi,
                    "target_index": ti,
                    "base_chars": content_text(&base[bi]).chars().count(),
                    "target_chars": content_text(&target[ti]).chars().count(),
                }));
            }
            1 => pending_ins.push(ti),
            _ => pending_del.push(bi),
        }
    }
    flush(
        &mut rows,
        &mut pending_del,
        &mut pending_ins,
        &mut stats,
        &mut truncated,
    );

    let base_system = base
        .iter()
        .filter(|m| role_of(m) == "system")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    let target_system = target
        .iter()
        .filter(|m| role_of(m) == "system")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    let system_changed = base_system != target_system;
    let (repeat_count, repeat_chars) = repeated(&target);
    let stats_value = json!({
        "base_messages": base.len(),
        "target_messages": target.len(),
        "delta_messages": target.len() as i64 - base.len() as i64,
        "base_chars": stats.base_chars,
        "target_chars": stats.target_chars,
        "delta_chars": stats.target_chars as i64 - stats.base_chars as i64,
        "delta_percent": if stats.base_chars > 0 {
            ((stats.target_chars as f64 - stats.base_chars as f64) / stats.base_chars as f64 * 100.0 * 100.0).round() / 100.0
        } else { 0.0 },
        "added": stats.added,
        "removed": stats.removed,
        "modified": stats.modified,
        "same": stats.same,
        "by_role": stats.by_role,
        "system_changed": system_changed,
        "estimated_base_tokens": stats.base_chars / 4,
        "estimated_target_tokens": stats.target_chars / 4,
        "repeated_messages": repeat_count,
        "duplicate_chars": repeat_chars,
        "duplicate_percent": if stats.target_chars > 0 {
            (repeat_chars as f64 / stats.target_chars as f64 * 100.0 * 100.0).round() / 100.0
        } else { 0.0 },
    });
    let mut insights = Vec::new();
    if system_changed {
        let (changed_lines, _) = line_diff(&base_system, &target_system, 0);
        let added = changed_lines
            .iter()
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some("add"))
            .count();
        let removed = changed_lines
            .iter()
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some("del"))
            .count();
        insights.push(json!({"code":"system_changed","added_lines":added,"removed_lines":removed}));
    }
    if stats.target_chars != stats.base_chars {
        insights.push(json!({
            "code":"context_growth",
            "delta_chars": stats.target_chars as i64 - stats.base_chars as i64,
            "delta_percent": stats_value["delta_percent"],
        }));
    }
    if repeat_count > 0 {
        insights.push(json!({
            "code":"repeated_messages",
            "count": repeat_count,
            "duplicate_chars": repeat_chars,
            "duplicate_percent": stats_value["duplicate_percent"],
        }));
    }
    json!({
        "messages": rows,
        "stats": stats_value,
        "insights": insights,
        "truncated": truncated,
    })
}

#[derive(Default)]
struct Stats {
    same: i64,
    added: i64,
    removed: i64,
    modified: i64,
    base_chars: usize,
    target_chars: usize,
    by_role: std::collections::BTreeMap<String, i64>,
}

fn repeated(messages: &[Value]) -> (i64, usize) {
    let mut counts = std::collections::HashMap::new();
    for message in messages {
        if role_of(message) == "system" {
            continue;
        }
        let text = content_text(message);
        if text.chars().count() < 20 {
            continue;
        }
        *counts.entry(text).or_insert(0usize) += 1;
    }
    let mut count = 0i64;
    let mut chars = 0usize;
    for (text, n) in counts {
        if n > 1 {
            count += 1;
            chars += text.chars().count() * (n - 1);
        }
    }
    (count, chars)
}

/// 结构化 diff 转 unified diff 文本；只包含发生变化的消息。
pub fn unified_diff(base_id: &str, target_id: &str, diff: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- {base_id}\n+++ {target_id}\n"));
    if let Some(rows) = diff.get("messages").and_then(Value::as_array) {
        for row in rows {
            let status = row.get("status").and_then(Value::as_str).unwrap_or("");
            if status == "same" {
                continue;
            }
            let role = row.get("role").and_then(Value::as_str).unwrap_or("unknown");
            let base_index = row.get("base_index").and_then(Value::as_u64);
            let target_index = row.get("target_index").and_then(Value::as_u64);
            out.push_str(&format!(
                "@@ {role} base={:?} target={:?} @@\n",
                base_index, target_index
            ));
            if status == "added" {
                if let Some(content) = row.get("content").and_then(Value::as_str) {
                    for line in content.split('\n') {
                        out.push_str(&format!("+{line}\n"));
                    }
                }
            } else if status == "removed" {
                if let Some(content) = row.get("content").and_then(Value::as_str) {
                    for line in content.split('\n') {
                        out.push_str(&format!("-{line}\n"));
                    }
                }
            } else if status == "modified" {
                if let Some(lines) = row.get("diff").and_then(Value::as_array) {
                    for line in lines {
                        let kind = line
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("context");
                        let text = line.get("text").and_then(Value::as_str).unwrap_or("");
                        let prefix = match kind {
                            "add" => "+",
                            "del" => "-",
                            _ => " ",
                        };
                        out.push_str(&format!("{prefix}{text}\n"));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_secret_shapes() {
        let input = "Authorization: Bearer abcdef123456\napi_key=sk-1234567890abcdef\nsk-abcdef1234567890\n{\"apiKey\":\"secret-value\",\"max_tokens\":10}";
        let (text, count) = redact_text(input);
        assert!(text.contains(REDACTED_BEARER));
        assert!(text.contains(REDACTED_KEY));
        assert!(text.contains(REDACTED_URL));
        assert!(!text.contains("abcdef123456"));
        assert!(!text.contains("sk-1234567890abcdef"));
        assert!(count >= 3);
        let value = json!({"apiKey":"secret-value","max_tokens":10});
        let mut n = 0;
        let redacted = redact_value(&value, &mut n);
        assert_eq!(redacted["apiKey"], REDACTED_FIELD);
        assert_eq!(redacted["max_tokens"], 10);
    }

    #[test]
    fn redacts_private_key_and_jwt() {
        let private = format!(
            "-----BEGIN {} PRIVATE KEY-----\nMIIE\n-----END {} PRIVATE KEY-----",
            "RSA", "RSA"
        );
        let (text, _) = redact_text(&private);
        assert!(text.contains(REDACTED_PRIVATE_KEY));
        assert!(!text.contains("MIIE"));
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature";
        let (text, _) = redact_text(jwt);
        assert!(text.contains(REDACTED_KEY));
        assert!(!text.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn redaction_handles_chinese_after_markers_without_panic() {
        // 回归：`bearer ` 后直接跟中文时，旧实现按 +1 字节前进会落入 UTF-8 中间并 panic。
        let (text, count) = redact_text("请检查 bearer 鉴权说明与 Authorization: 中文");
        assert_eq!(count, 0);
        assert_eq!(text, "请检查 bearer 鉴权说明与 Authorization: 中文");

        let (text, _) = redact_text("https://example.test/?token=中文");
        assert!(text.contains("token=[REDACTED:url]"), "{text}");
        assert!(!text.contains("中文"));

        // 其他“标记后直接跟中文”的路径也不能 panic。
        for sample in [
            "sk-中文",
            "api_key=中文",
            "-----BEGIN 中文",
            "Bearer 鉴权 / bearer 说明 / Authorization: 中文",
        ] {
            let _ = redact_text(sample);
        }
    }

    #[test]
    fn prepare_messages_truncates_to_valid_json() {
        let messages = vec![json!({"role":"user","content":"x".repeat(5000)})];
        let encoded = prepare_messages(&messages, 1000);
        assert!(encoded.truncated);
        assert!(encoded.messages.len() <= 1200);
        let parsed: Value = serde_json::from_str(&encoded.messages).expect("valid json");
        assert!(parsed.is_array());
    }

    #[test]
    fn diff_marks_added_removed_modified() {
        let base = vec![
            json!({"role":"system","content":"be helpful"}),
            json!({"role":"user","content":"line1\nline2"}),
        ];
        let target = vec![
            json!({"role":"system","content":"be helpful\nbe concise"}),
            json!({"role":"user","content":"line1\nline2 changed"}),
            json!({"role":"assistant","content":"ok"}),
        ];
        let diff = diff_messages(&base, &target, 1);
        assert_eq!(diff["stats"]["modified"], 2);
        assert_eq!(diff["stats"]["added"], 1);
        assert_eq!(diff["stats"]["system_changed"], true);
        assert!(diff["messages"].as_array().unwrap().iter().any(|m| {
            m["status"] == "modified"
                && m["diff"].as_array().map(|a| !a.is_empty()).unwrap_or(false)
        }));
    }

    #[test]
    fn unified_diff_contains_headers_and_changes() {
        let base = vec![json!({"role":"user","content":"a\nb"})];
        let target = vec![json!({"role":"user","content":"a\nc"})];
        let diff = diff_messages(&base, &target, 1);
        let text = unified_diff("base", "target", &diff);
        assert!(text.contains("--- base"));
        assert!(text.contains("+++ target"));
        assert!(text.contains("-b"));
        assert!(text.contains("+c"));
    }

    #[test]
    fn prepare_messages_keeps_head_and_tail_under_budget() {
        let mut messages = Vec::new();
        for i in 0..20 {
            messages
                .push(json!({"role":"user","content": format!("message {i}: {}", "x".repeat(80))}));
        }
        messages.insert(0, json!({"role":"system","content":"SYSTEM PROMPT"}));
        let encoded = prepare_messages(&messages, 1000);
        assert!(encoded.truncated);
        let parsed: Value = serde_json::from_str(&encoded.messages).expect("valid json");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr[0]["role"], "system");
        assert!(arr.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("message 19"));
        let truncation = encoded.truncation.as_ref().expect("truncation metadata");
        assert_eq!(truncation["strategy"], "head_tail");
        assert!(truncation["dropped_messages"].as_i64().unwrap() > 0);
        assert!(truncation["dropped_chars"].as_i64().unwrap() > 0);
        assert!(truncation["kept_chars"].as_i64().unwrap() <= 1000);
        let meta_text = serde_json::to_string(truncation).unwrap();
        assert!(!meta_text.contains("message 0:"));
        assert!(!meta_text.contains("SYSTEM PROMPT"));
    }

    #[test]
    fn prepare_messages_redacts_before_truncation() {
        let secret = format!("sk-{}", "a".repeat(32));
        let messages =
            vec![json!({"role":"user","content": format!("{} {}", secret, "y".repeat(5000))})];
        let encoded = prepare_messages(&messages, 1000);
        assert!(!encoded.messages.contains(&secret));
        assert!(encoded.messages.contains("[REDACTED:key]"));
        assert!(encoded.redactions >= 1);
    }

    #[test]
    fn summarize_call_chain_extracts_tools_and_roles() {
        let messages = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":"do x"}),
            json!({"role":"assistant","content":"","tool_calls":[{"id":"c1","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","name":"read","content":"file contents"}),
            json!({"role":"assistant","content":"done"}),
        ];
        let cc = summarize_call_chain(&messages);
        assert_eq!(cc["message_count"], 5);
        assert_eq!(cc["assistant_turns"], 2);
        assert_eq!(cc["tool_turns"], 1);
        assert_eq!(cc["tool_call_count"], 1);
        assert_eq!(cc["tool_result_count"], 1);
        assert_eq!(cc["unmatched_tool_calls"], 0);
        let pairs = cc["pairs"].as_array().unwrap();
        assert_eq!(pairs[0]["name"], "read");
        assert_eq!(pairs[0]["has_result"], true);
        assert!(pairs[0]["result_preview"]
            .as_str()
            .unwrap()
            .contains("file contents"));
        let seq = cc["role_sequence"].as_array().unwrap();
        assert_eq!(seq[0], "system");
        let previews = cc["message_previews"].as_array().unwrap();
        assert_eq!(previews.len(), 5);
        assert_eq!(previews[0]["role"], "system");
        assert_eq!(previews[2]["role"], "assistant");
        assert_eq!(previews[2]["tool_names"][0], "read");
        assert!(previews[2]["preview"]
            .as_str()
            .unwrap()
            .contains("[tool_calls] read"));
        assert_eq!(previews[3]["role"], "tool");
        assert_eq!(previews[3]["tool_name"], "read");
    }

    #[test]
    fn summarize_call_chain_caps_long_previews() {
        let messages: Vec<Value> = (0..100)
            .map(|i| json!({"role":"user","content":format!("m{i}")}))
            .collect();
        let cc = summarize_call_chain(&messages);
        let previews = cc["message_previews"].as_array().unwrap();
        assert_eq!(previews.len(), 41);
        assert_eq!(previews[10]["role"], "_gap");
        assert!(previews[10]["preview"].as_str().unwrap().contains("60"));
        assert_eq!(previews[11]["index"], 70);
        assert_eq!(previews.last().unwrap()["index"], 99);
    }

    #[test]
    fn prepare_messages_role_counts_and_hashes_are_present() {
        let messages = vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":"usr"}),
        ];
        let encoded = prepare_messages(&messages, 100000);
        assert!(!encoded.truncated);
        assert!(encoded.truncation.is_none());
        assert!(encoded.content_hash.len() == 16);
        assert!(encoded.system_hash.len() == 16);
        let roles: Value = serde_json::from_str(&encoded.role_counts).unwrap();
        assert_eq!(roles["system"], 1);
        assert_eq!(roles["user"], 1);
    }
}
