//! provider 预设注册表（编译期嵌入 `modelgate/provider_profiles.json`，与 Python 端共用）。
//!
//! 判定与合并规则见 docs/architecture/upstream-config-model.md：
//! 显式 profile > 域名（按 priority）> 协议预设；显式 anthropic + 含 deepseek 的 host 走 DeepSeek 语义（兼容 shim）。

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::config::{Capabilities, Compat};

static REGISTRY: OnceLock<Option<Value>> = OnceLock::new();

fn registry() -> Option<&'static Value> {
    REGISTRY
        .get_or_init(|| {
            serde_json::from_str::<Value>(include_str!("../../../modelgate/provider_profiles.json")).ok()
        })
        .as_ref()
}

fn profiles() -> Option<&'static Map<String, Value>> {
    registry()?.get("profiles")?.as_object()
}

/// 生效 profile 判定（不做未知回退；未知返回原名）。
pub fn detect_profile(profile_hint: &str, protocol: &str, base_url: &str) -> String {
    let hint = profile_hint.trim().to_lowercase();
    let host = base_url.to_lowercase();
    let mut effective = hint;
    if effective.is_empty() {
        if protocol == "commandcode" {
            effective = "commandcode".into();
        } else if let Some(profiles) = profiles() {
            let mut detected: Vec<(u64, String)> = Vec::new();
            for (pid, entry) in profiles.iter() {
                let contains = entry
                    .get("detect")
                    .and_then(|d| d.get("baseUrlContains"))
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().filter_map(Value::as_str))
                    .map(|mut iter| iter.any(|s| host.contains(&s.to_lowercase())))
                    .unwrap_or(false);
                if contains {
                    let priority = entry.get("priority").and_then(Value::as_u64).unwrap_or(0);
                    detected.push((priority, pid.clone()));
                }
            }
            if let Some((_, pid)) = detected.into_iter().max() {
                effective = pid;
            } else if profiles.contains_key(protocol) {
                effective = protocol.to_string();
            } else {
                effective = "openai".into();
            }
        } else {
            effective = if protocol.is_empty() { "openai".into() } else { protocol.to_string() };
        }
    }
    // 兼容 shim：显式 anthropic + 含 deepseek 的 host → DeepSeek 语义（历史行为，保留）。
    if effective == "anthropic" && host.contains("deepseek") {
        effective = "deepseek".into();
    }
    effective
}

/// 注册表解析 compat；注册表不可用时返回 None（调用方回退内置实现）。
pub fn resolve_compat(profile_hint: &str, base_url: &str, protocol: &str) -> Option<Compat> {
    let profiles = profiles()?;
    let base = profiles.get("openai")?.get("compat")?.clone();
    let effective = detect_profile(profile_hint, protocol, base_url);
    let known = profiles.get(&effective);
    let mut merged = merge_values(&base, known.and_then(|e| e.get("compat")).unwrap_or(&Value::Null));
    if let Some(variant) = known
        .and_then(|e| e.get("variants"))
        .and_then(|v| v.get(protocol))
    {
        merged = merge_values(&merged, variant);
    }
    let mut compat = compat_from_value(&merged);
    compat.profile = effective;
    Some(compat)
}

/// 注册表里的能力声明（不含调用方覆盖）。
pub fn capabilities_for(profile_id: &str) -> Capabilities {
    let mut caps = Capabilities::default();
    if let Some(compat) = profiles()
        .and_then(|p| p.get(profile_id))
        .and_then(|e| e.get("compat"))
        .and_then(|c| c.get("capabilities"))
    {
        caps.apply_json(compat);
    }
    caps
}

/// profile 是否在注册表中（诊断 unknown_profile 用）。
pub fn is_known_profile(profile: &str) -> bool {
    profiles().map(|p| p.contains_key(profile)).unwrap_or(false)
}

/// 注册表内的 profile id（配置顺序，供诊断提示与面板下拉）。
pub fn profile_ids() -> Vec<String> {
    profiles()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default()
}

/// 供 `/api/config` 展示的预设元数据（与 Python `profiles_meta()` 对齐）。
pub fn profiles_meta() -> Vec<Value> {
    let Some(profiles) = profiles() else {
        return Vec::new();
    };
    profiles
        .iter()
        .map(|(pid, entry)| {
            let mut caps = capabilities_for(pid);
            caps.apply_json(
                entry
                    .get("compat")
                    .and_then(|c| c.get("capabilities"))
                    .unwrap_or(&Value::Null),
            );
            json!({
                "id": pid,
                "label": entry.get("label").and_then(Value::as_str).unwrap_or(pid),
                "description": entry.get("description").and_then(Value::as_str).unwrap_or(""),
                "capabilities": caps.to_json(),
            })
        })
        .collect()
}

fn merge_values(base: &Value, extra: &Value) -> Value {
    match (base, extra) {
        (Value::Object(b), Value::Object(e)) => {
            let mut out = b.clone();
            for (key, value) in e {
                let merged = match out.get(key) {
                    Some(existing) => merge_values(existing, value),
                    None => value.clone(),
                };
                out.insert(key.clone(), merged);
            }
            Value::Object(out)
        }
        // Null 表示「这一层没有覆盖」（未知 profile / 无变体），语义与 Python `_deep_merge(base, extra or {})` 一致。
        (_, Value::Null) => base.clone(),
        (_, extra) => extra.clone(),
    }
}

fn compat_from_value(value: &Value) -> Compat {
    let mut c = Compat::default();
    let get = |path: &[&str]| -> Option<&Value> {
        let mut current = value;
        for key in path {
            current = current.get(*key)?;
        }
        Some(current)
    };
    let str_at = |path: &[&str]| get(path).and_then(Value::as_str).unwrap_or("");
    let bool_at = |path: &[&str]| get(path).and_then(Value::as_bool).unwrap_or(false);
    let u64_at = |path: &[&str]| get(path).and_then(Value::as_u64);

    c.auth_scheme = str_at(&["auth", "scheme"]).to_string();
    c.auth_header = str_at(&["auth", "header"]).to_string();
    c.auth_prefix = str_at(&["auth", "prefix"]).to_string();
    c.session_header = str_at(&["session", "header"]).to_string();
    c.session_id_style = str_at(&["session", "idStyle"]).to_string();
    if let Some(headers) = get(&["headers"]).and_then(Value::as_object) {
        c.extra_headers = headers
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
            .collect();
    }
    c.chat_path = str_at(&["endpoint", "chat"]).to_string();
    c.models_path = str_at(&["endpoint", "models"]).to_string();
    c.thinking_passthrough = bool_at(&["thinking", "passthrough"]);
    c.thinking_field = str_at(&["thinking", "field"]).to_string();
    c.tool_choice_downgrade = bool_at(&["toolChoice", "downgradeForced"]);
    c.max_output_tokens = u64_at(&["limits", "maxOutputTokens"]);
    c.max_context_tokens = u64_at(&["limits", "maxContextTokens"]);
    c.zdr = bool_at(&["zdr"]);
    c.version_header = str_at(&["versionHeader"]).to_string();
    c.environment_header = str_at(&["environmentHeader"]).to_string();
    c.catalog_source = str_at(&["catalog", "source"]).to_string();
    c.catalog_path = str_at(&["catalog", "path"]).to_string();
    if let Some(plan) = get(&["catalog", "plan"]) {
        c.catalog_plan = plan.clone();
    }
    c.capabilities = {
        let mut caps = Capabilities::default();
        if let Some(layer) = get(&["capabilities"]) {
            caps.apply_json(layer);
        }
        caps
    };
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compat → golden fixtures 的 11 键快照（Python `_COMPAT_KEYS` 同构）。
    fn compat_snapshot(c: &Compat) -> Value {
        let mut limits = Map::new();
        if let Some(v) = c.max_output_tokens {
            limits.insert("maxOutputTokens".into(), json!(v));
        }
        if let Some(v) = c.max_context_tokens {
            limits.insert("maxContextTokens".into(), json!(v));
        }
        let headers: Map<String, Value> = c
            .extra_headers
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!({
            "profile": c.profile,
            "auth": {"scheme": c.auth_scheme, "header": c.auth_header, "prefix": c.auth_prefix},
            "session": {"header": c.session_header, "idStyle": c.session_id_style},
            "headers": headers,
            "endpoint": {"chat": c.chat_path, "models": c.models_path},
            "thinking": {"passthrough": c.thinking_passthrough, "field": c.thinking_field},
            "toolChoice": {"downgradeForced": c.tool_choice_downgrade},
            "limits": limits,
            "zdr": c.zdr,
            "versionHeader": c.version_header,
            "environmentHeader": c.environment_header,
        })
    }

    #[test]
    fn golden_fixtures_match_rust_resolution() {
        // 与 Python（tests/test_provider_profiles.py）读取同一份 fixtures，不允许各自维护预期值。
        let golden: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/provider_profiles_golden.json"
        ))
        .expect("golden fixtures 可解析");
        let cases = golden.get("cases").and_then(Value::as_array).expect("cases");
        assert!(!cases.is_empty(), "golden fixtures 为空");
        for case in cases {
            let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
            let inp = case.get("input").expect("input");
            let hint = inp.get("profile").and_then(Value::as_str).unwrap_or("");
            let protocol = inp.get("protocol").and_then(Value::as_str).unwrap_or("");
            let base = inp.get("baseUrl").and_then(Value::as_str).unwrap_or("");
            let expect = case.get("expect").expect("expect");
            assert_eq!(
                detect_profile(hint, protocol, base),
                expect["profile"].as_str().unwrap_or(""),
                "case {name}: profile 判定不一致"
            );
            let compat = resolve_compat(hint, base, protocol).expect("注册表可解析");
            assert_eq!(
                compat_snapshot(&compat),
                *expect,
                "case {name}: 注册表解析与 fixtures 不一致"
            );
        }
    }

    #[test]
    fn profiles_meta_exposes_five_profiles_in_registry_order() {
        let meta = profiles_meta();
        let ids: Vec<&str> = meta.iter().filter_map(|v| v["id"].as_str()).collect();
        assert_eq!(ids, ["openai", "opencode", "anthropic", "deepseek", "commandcode"]);
        for row in &meta {
            for key in ["stream", "tools", "vision", "thinking", "cache", "systemPrompt"] {
                assert!(row["capabilities"].get(key).is_some(), "缺少能力字段 {key}");
            }
        }
        assert!(is_known_profile("deepseek"));
        assert!(!is_known_profile("mystery"));
    }
}
