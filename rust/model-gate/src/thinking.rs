//! 思考内容回传缓存（DeepSeek/Anthropic 思考链）。
//!
//! 为什么需要：DeepSeek 的思考模式要求多轮把上一轮的 thinking + signature
//! 原样回传，否则思考链断裂；而多数 Agent 只会回传 assistant 文本与 tool_calls。
//! 这里按 tool_call id / 文本指纹缓存上游返回的 thinking，下一轮自动补回。
//!
//! 开关分两层：
//! - 全局 `thinking.passthrough` / `thinking.cacheSize`
//! - 上游 `compat.thinking.passthrough`（覆盖全局，仅 anthropic 协议参与请求改写）
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use serde_json::{json, Value};

#[derive(Clone, Default)]
pub struct Entry {
    pub thinking: String,
    pub signature: String,
}

pub struct ThinkingCache {
    enabled: bool,
    max_size: usize,
    by_tool_id: Mutex<HashMap<String, Entry>>,
    by_text: Mutex<HashMap<u64, Entry>>,
}

impl ThinkingCache {
    pub fn new(enabled: bool, max_size: usize) -> Self {
        Self {
            enabled,
            max_size: max_size.max(1),
            by_tool_id: Mutex::new(HashMap::new()),
            by_text: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn fingerprint(text: &str) -> u64 {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut hasher = DefaultHasher::new();
        normalized.hash(&mut hasher);
        hasher.finish()
    }

    /// 记住一轮思考内容。thinking 为空时不记录（避免污染缓存）。
    pub fn remember(&self, thinking: &str, signature: &str, text: &str, tool_ids: &[String]) {
        if !self.enabled || thinking.is_empty() {
            return;
        }
        let entry = Entry {
            thinking: thinking.to_string(),
            signature: signature.to_string(),
        };
        if let Ok(mut map) = self.by_tool_id.lock() {
            for id in tool_ids.iter().filter(|id| !id.is_empty()) {
                map.insert(id.clone(), entry.clone());
            }
            while map.len() > self.max_size {
                if let Some(key) = map.keys().next().cloned() {
                    map.remove(&key);
                } else {
                    break;
                }
            }
        }
        if !text.is_empty() {
            if let Ok(mut map) = self.by_text.lock() {
                map.insert(Self::fingerprint(text), entry);
                while map.len() > self.max_size {
                    if let Some(key) = map.keys().next().cloned() {
                        map.remove(&key);
                    } else {
                        break;
                    }
                }
            }
        }
    }

    /// 取回思考内容：先按 tool_call id 命中，再按文本指纹兜底。
    pub fn recall(&self, text: &str, tool_ids: &[String]) -> Option<Entry> {
        if !self.enabled {
            return None;
        }
        if let Ok(map) = self.by_tool_id.lock() {
            for id in tool_ids.iter().filter(|id| !id.is_empty()) {
                if let Some(hit) = map.get(id) {
                    return Some(hit.clone());
                }
            }
        }
        if !text.is_empty() {
            if let Ok(map) = self.by_text.lock() {
                return map.get(&Self::fingerprint(text)).cloned();
            }
        }
        None
    }

    /// 构造要回传给上游的 thinking 块；取不到或未开启则返回 None。
    ///
    /// 客户端显式带了 `reasoning_content` / `thinking` 时以其为准，
    /// 不被 passthrough 开关影响（这是调用方的明确意图）。
    pub fn build_block(
        &self,
        message: &Value,
        text: &str,
        tool_ids: &[String],
        passthrough: bool,
    ) -> Option<Value> {
        let explicit = message
            .get("reasoning_content")
            .or_else(|| message.get("thinking"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        if let Some(thinking) = explicit {
            let mut block = json!({"type": "thinking", "thinking": thinking});
            if let Some(sig) = message
                .get("reasoning_signature")
                .or_else(|| message.get("signature"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                block["signature"] = json!(sig);
            }
            return Some(block);
        }
        if !passthrough {
            return None;
        }
        let hit = self.recall(text, tool_ids)?;
        let mut block = json!({"type": "thinking", "thinking": hit.thinking});
        if !hit.signature.is_empty() {
            block["signature"] = json!(hit.signature);
        }
        Some(block)
    }

    pub fn clear(&self) {
        if let Ok(mut map) = self.by_tool_id.lock() {
            map.clear();
        }
        if let Ok(mut map) = self.by_text.lock() {
            map.clear();
        }
    }

    pub fn stats(&self) -> Value {
        let by_tool = self.by_tool_id.lock().map(|m| m.len()).unwrap_or(0);
        let by_text = self.by_text.lock().map(|m| m.len()).unwrap_or(0);
        json!({
            "enabled": self.enabled,
            "max_size": self.max_size,
            "by_tool_id": by_tool,
            "by_text": by_text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_recalls_by_tool_id() {
        let cache = ThinkingCache::new(true, 10);
        cache.remember("chain", "sig", "answer", &["call_1".into()]);
        let hit = cache.recall("", &["call_1".into()]).expect("hit");
        assert_eq!(hit.thinking, "chain");
        assert_eq!(hit.signature, "sig");
    }

    #[test]
    fn explicit_reasoning_wins_without_cache() {
        let cache = ThinkingCache::new(false, 10);
        let msg = json!({"role": "assistant", "reasoning_content": "given", "signature": "s"});
        let block = cache.build_block(&msg, "", &[], true).expect("block");
        assert_eq!(block["thinking"], "given");
        assert_eq!(block["signature"], "s");
    }

    #[test]
    fn passthrough_off_skips_recall() {
        let cache = ThinkingCache::new(true, 10);
        cache.remember("chain", "", "answer", &[]);
        assert!(cache.build_block(&json!({}), "answer", &[], false).is_none());
    }
}
