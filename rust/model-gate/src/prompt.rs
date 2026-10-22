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

#[derive(Debug, Clone, Default)]
pub struct EncodedPrompt {
    pub messages: String,
    pub truncated: bool,
    pub message_count: i64,
    pub size_bytes: i64,
    pub preview: String,
    pub redactions: usize,
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
        search = token_end.max(token_start + 1);
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
        search = token_end.max(token_start + 1);
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
        search = value_end.max(value_start + 1);
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

fn encode_array(messages: &[Value], max_chars: usize) -> (String, bool) {
    let full = Value::Array(messages.to_vec()).to_string();
    if full.chars().count() <= max_chars {
        return (full, false);
    }
    let mut kept: Vec<Value> = Vec::new();
    let mut used = 2usize;
    let mut truncated = false;
    for message in messages {
        let candidate = message.clone();
        let serialized = candidate.to_string();
        if used + serialized.chars().count() + 1 > max_chars {
            let remaining = max_chars.saturating_sub(used + 64);
            if remaining > 20 {
                let text = content_text(&candidate);
                let cut: String = text.chars().take(remaining).collect();
                let mut shortened = candidate;
                if let Some(obj) = shortened.as_object_mut() {
                    obj.insert(
                        "content".into(),
                        Value::String(format!("{cut}…[truncated]")),
                    );
                    kept.push(shortened);
                }
            }
            truncated = true;
            break;
        }
        used += serialized.chars().count() + 1;
        kept.push(candidate);
    }
    let mut text = Value::Array(kept).to_string();
    while text.chars().count() > max_chars {
        let Ok(Value::Array(mut arr)) = serde_json::from_str::<Value>(&text) else {
            break;
        };
        if arr.len() > 1 {
            arr.pop();
            truncated = true;
            text = Value::Array(arr).to_string();
            continue;
        }
        if let Some(obj) = arr.get_mut(0).and_then(Value::as_object_mut) {
            let content = obj.get("content").map(content_text).unwrap_or_default();
            let cut: String = content.chars().take(max_chars.saturating_sub(80)).collect();
            obj.insert(
                "content".into(),
                Value::String(format!("{cut}…[truncated]")),
            );
            text = Value::Array(arr).to_string();
            truncated = true;
        }
        break;
    }
    (text, truncated)
}

/// 先脱敏、再做大小预算截断，生成可直接落库的提示词记录。
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
    let (text, truncated) = encode_array(&arr, max_chars.max(1000));
    EncodedPrompt {
        messages: text.clone(),
        truncated,
        message_count,
        size_bytes: text.len() as i64,
        preview,
        redactions,
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
    json!({
        "message_count": messages.len(),
        "char_count": char_count,
        "system_chars": system_chars,
        "user_chars": user_chars,
        "system_hash": system_hash(messages),
        "estimated_tokens": (char_count / 4) as i64,
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
}
