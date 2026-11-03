//! 上游密钥的本地加密存储。
//!
//! 安全口径：
//! - 只保存 Windows DPAPI 密文；密文绑定当前 Windows 用户与机器，复制到其他
//!   用户/机器后无法解密；
//! - 文件与 `bridge.config.json` 分离，配置只保存 `apiKeyStore` 引用，便于配置
//!   分享/提交时不带任何密钥材料；
//! - 不提供明文导出接口；错误信息只包含路径/原因，不包含密钥；
//! - 原子写：同目录临时文件 → 回读校验 → 替换，避免半截密钥库。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{Map, Value};

const FILE_VERSION: u64 = 1;

/// 本地加密密钥库。
#[derive(Debug, Clone, Default)]
pub struct SecretStore {
    /// 绑定的密钥库文件路径；`None` 表示只读空库（单测/预览用）。
    path: Option<PathBuf>,
    /// `id -> base64(DPAPI blob)`。
    entries: BTreeMap<String, String>,
}

impl SecretStore {
    /// 不绑定文件的空库：用于 `Config::from_value` 等没有配置路径的场景。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从配置文件路径推导密钥库并加载。
    ///
    /// 文件不存在视为空库；文件损坏时按空库处理并记录告警，避免整个应用起不来。
    pub fn load(config_path: &Path) -> Self {
        let path = path_for_config(config_path);
        let mut store = Self {
            path: Some(path.clone()),
            entries: BTreeMap::new(),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return store,
        };
        let value: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "密钥库不是合法 JSON，已按空库处理：{}（{err}）",
                    path.display()
                );
                return store;
            }
        };
        if let Some(entries) = value.get("entries").and_then(Value::as_object) {
            for (id, sealed) in entries {
                if let Some(sealed) = sealed.as_str().filter(|s| !s.is_empty()) {
                    store.entries.insert(id.clone(), sealed.to_string());
                }
            }
        }
        store
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// 读取并解密；任何一步失败都只返回 `None`，调用方按“未读到”处理。
    pub fn get(&self, id: &str) -> Option<String> {
        let sealed = self.entries.get(id)?;
        let blob = base64::engine::general_purpose::STANDARD
            .decode(sealed)
            .ok()?;
        let plain = crate::zcode::secret::open(&blob).ok()?;
        String::from_utf8(plain).ok()
    }

    /// 解密全部条目，供进程内缓存一次性构建（条目数量级是个位数）。
    /// 解不开的条目按“未读到”跳过，与 `get` 的口径一致。
    pub fn decrypted_all(&self) -> BTreeMap<String, String> {
        self.entries
            .keys()
            .filter_map(|id| self.get(id).map(|key| (id.clone(), key)))
            .collect()
    }

    /// 加密并保存一把密钥；成功返回后立即可被 `get` 读到。
    pub fn set(&mut self, id: &str, key: &str) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("密钥库条目 id 不能为空".into());
        }
        crate::key_file::validate_key(key).map_err(|err| err.to_string())?;
        let sealed = crate::zcode::secret::seal(key.as_bytes())?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(sealed);
        self.entries.insert(id.to_string(), encoded);
        self.save()
    }

    /// 删除一条密钥；返回是否真的删除了。
    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        let removed = self.entries.remove(id).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// 原子落盘。
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Err("密钥库没有绑定配置文件路径".into());
        };
        let mut entries = Map::new();
        for (id, sealed) in &self.entries {
            entries.insert(id.clone(), Value::String(sealed.clone()));
        }
        let value = serde_json::json!({
            "version": FILE_VERSION,
            "entries": entries,
        });
        crate::config_store::save_atomic(path, &value).map_err(|err| err.to_string())
    }
}

/// 密钥库路径：与配置文件同目录，例如 `bridge.config.json` →
/// `bridge.config.secrets.json`。独立文件便于排除在配置分享/备份之外。
pub fn path_for_config(config_path: &Path) -> PathBuf {
    config_path.with_extension("secrets.json")
}

/// 上游级密钥引用 id。
pub fn upstream_id(upstream: &str) -> String {
    format!("upstream:{upstream}")
}

/// 逐模型密钥引用 id。
pub fn model_id(upstream: &str, pattern: &str) -> String {
    format!("model:{upstream}:{pattern}")
}

/// 配置里的引用只接受带命名空间的 id，避免把“看起来像密钥”的字符串写进
/// `apiKeyStore` 后被当作明文保存。
pub fn is_valid_id(id: &str) -> bool {
    id.starts_with("upstream:") || id.starts_with("model:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mg-secret-store-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_is_next_to_config() {
        let path = PathBuf::from(r"D:\app\bridge.config.json");
        assert_eq!(
            path_for_config(&path),
            PathBuf::from(r"D:\app\bridge.config.secrets.json")
        );
    }

    #[test]
    fn id_namespaces_are_stable() {
        assert_eq!(upstream_id("deepseek"), "upstream:deepseek");
        assert_eq!(
            model_id("deepseek", "deepseek-chat"),
            "model:deepseek:deepseek-chat"
        );
        assert!(is_valid_id("upstream:deepseek"));
        assert!(is_valid_id("model:deepseek:deepseek-chat"));
        assert!(!is_valid_id("sk-plaintext-looking"));
        assert!(!is_valid_id(""));
    }

    #[cfg(windows)]
    #[test]
    fn set_saves_ciphertext_and_reloads() {
        let dir = temp_dir("roundtrip");
        let config = dir.join("bridge.config.json");
        let id = upstream_id("u");
        let mut store = SecretStore::load(&config);
        store.set(&id, "sk-test-store-key").unwrap();

        // 文件里不能出现明文。
        let text = std::fs::read_to_string(path_for_config(&config)).unwrap();
        assert!(!text.contains("sk-test-store-key"));
        assert!(text.contains(&id));

        // 重新加载后仍能解出同一把 key。
        let reloaded = SecretStore::load(&config);
        assert_eq!(reloaded.get(&id).as_deref(), Some("sk-test-store-key"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn remove_deletes_entry() {
        let dir = temp_dir("remove");
        let config = dir.join("bridge.config.json");
        let id = upstream_id("u");
        let mut store = SecretStore::load(&config);
        store.set(&id, "sk-remove-me").unwrap();
        assert!(store.remove(&id).unwrap());
        assert!(!store.contains(&id));
        assert!(SecretStore::load(&config).get(&id).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
