//! 配置落盘：合并语义 + 原子写 + 备份策略。
//!
//! 核心约束：**`bridge.config.json` 是配置的唯一事实源**。
//! 面板的每一次保存都必须把完整配置写回该文件；数据库快照不得成为
//! 第二条可独立演进的配置事实源。
//!
//! 三个职责：
//! 1. [`merge_incoming`]：把面板传来的部分配置合并进当前配置。
//!    - 顶层按键合并：只改传入的键，其余保持原值（支持 `{defaultModel}` 这类局部保存）。
//!    - `upstreams` 按**名称**做增删改：传入集合即最终集合（删是真删），
//!      同名项逐字段合并，保留面板未渲染的兼容字段（`cliVersion` / `compat` / …）。
//! 2. [`save_atomic`]：写临时文件 → 回读校验 → 原子替换。
//! 3. [`BackupPolicy`] / [`backup_config`]：保存前的备份策略（首次强制、后续可选、轮转）。
//!
//! 明文密钥策略不变：`apiKey` 永不落盘（`strip_secrets`）。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use serde_json::{Map, Value};

/// 备份文件保留份数（超出时删除最旧的）。
pub const BACKUP_KEEP: usize = 5;

/// 备份文件后缀（最近一次）。
pub const BACKUP_SUFFIX: &str = ".bak";

/// 备份策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupPolicy {
    /// 保存前生成备份（面板开关打开）。
    Enabled,
    /// 不生成新备份；但**首次**（目录中无任何备份）仍强制备份一次。
    Disabled,
}

impl BackupPolicy {
    /// 面板开关的取值（`backup` 字段）；缺省视为开启。
    pub fn from_flag(enabled: Option<bool>) -> Self {
        match enabled {
            Some(false) => BackupPolicy::Disabled,
            _ => BackupPolicy::Enabled,
        }
    }
}

/// 备份结果（供接口回显与提示语）。
#[derive(Debug, Clone, Default)]
pub struct BackupOutcome {
    /// 本次是否实际生成了备份。
    pub created: bool,
    /// 生成的备份文件路径（绝对）。
    pub path: Option<PathBuf>,
    /// 是否属于「首次强制备份」（目录中原本没有任何备份）。
    pub forced_first: bool,
}

/// 备份文件路径：`<config>.bak`。
pub fn backup_path(config_path: &Path) -> PathBuf {
    let mut name = config_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bridge.config.json".to_string());
    name.push_str(BACKUP_SUFFIX);
    config_path.with_file_name(name)
}

/// 历史备份路径：`<config>.bak.N`（N 从 1 开始，1 为次新）。
pub fn backup_history_path(config_path: &Path, index: usize) -> PathBuf {
    let mut name = config_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bridge.config.json".to_string());
    name.push_str(BACKUP_SUFFIX);
    name.push('.');
    name.push_str(&index.to_string());
    config_path.with_file_name(name)
}

/// 目录中是否已存在任何备份（`.bak` 或 `.bak.N`）。
pub fn has_any_backup(config_path: &Path) -> bool {
    if backup_path(config_path).exists() {
        return true;
    }
    for index in 1..BACKUP_KEEP {
        if backup_history_path(config_path, index).exists() {
            return true;
        }
    }
    false
}

/// 剥离明文密钥：落盘前必须调用。
///
/// `bridge.config.json` 会被分享/提交，明文密钥一旦落盘便随文件扩散。
/// 只剥离 `apiKey` 字段本身，`apiKeyEnv` / `apiKeyFile` 等**引用**保留。
///
/// 覆盖两种形态：
/// - 新版 `upstreams{名:{apiKey}}`（按名收集的字典）；
/// - 旧版单上游 `upstream{apiKey}`（**单个对象**，不是字典集合）。
///
/// 同时清掉面板回传的只读内部字段（`_keyConfigured` / `_keySource`）：
/// 它们是接口的展示产物，不属于用户配置，落盘会污染文件。
pub fn strip_secrets(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    // 新版：upstreams 是「上游名 -> 上游对象」的字典。
    if let Some(upstreams) = obj.get_mut("upstreams").and_then(Value::as_object_mut) {
        for (_, upstream) in upstreams.iter_mut() {
            if let Some(entry) = upstream.as_object_mut() {
                entry.remove("apiKey");
                entry.remove("_keyConfigured");
                entry.remove("_keySource");
            }
        }
    }
    // 旧版：upstream 是单个上游对象。
    if let Some(entry) = obj.get_mut("upstream").and_then(Value::as_object_mut) {
        entry.remove("apiKey");
        entry.remove("_keyConfigured");
        entry.remove("_keySource");
    }
}

/// 合并面板传入的部分配置。
///
/// 顶层：只覆盖传入的键，其余保持不变。
/// `upstreams`：传入的集合即最终集合 —— 面板删掉的上游从结果中消失（不再被顶回）；
/// 同名上游逐字段合并，传入未覆盖的字段保留旧值（兼容字段 `cliVersion` / `compat` 等）。
pub fn merge_incoming(current: &Value, incoming: &Value) -> Value {
    let mut result = current.clone();
    let Some(current_obj) = result.as_object_mut() else {
        return incoming.clone();
    };
    let Some(incoming_obj) = incoming.as_object() else {
        return result;
    };

    for (key, value) in incoming_obj {
        if key == "upstreams" {
            let merged = merge_upstreams(
                current_obj.get("upstreams").and_then(Value::as_object),
                value.as_object(),
            );
            current_obj.insert("upstreams".to_string(), Value::Object(merged));
            continue;
        }
        current_obj.insert(key.clone(), value.clone());
    }
    result
}

/// `upstreams` 集合的增删改合并。
fn merge_upstreams(
    current: Option<&Map<String, Value>>,
    incoming: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let mut result: Map<String, Value> = Map::new();
    let Some(incoming) = incoming else {
        // 面板没传 upstreams：保持原样（局部保存不动上游集合）。
        if let Some(current) = current {
            return current.clone();
        }
        return result;
    };
    for (name, new_up) in incoming {
        let merged = match current.and_then(|c| c.get(name)).and_then(Value::as_object) {
            Some(old) => {
                let mut entry = old.clone();
                if let Some(new_obj) = new_up.as_object() {
                    for (key, value) in new_obj {
                        entry.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(entry)
            }
            None => new_up.clone(),
        };
        result.insert(name.clone(), merged);
    }
    result
}

/// 原子写配置：临时文件 → 回读校验 → 替换。
///
/// 回读校验的意义：确保落盘的是**合法 JSON**且与预期一致，
/// 避免半截配置导致下次启动直接失败。
pub fn save_atomic(path: &Path, value: &Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .with_context(|| format!("配置序列化失败: {}", path.display()))?;
    let payload = format!("{text}\n");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, payload.as_bytes())
        .with_context(|| format!("写临时配置文件失败: {}", tmp.display()))?;
    // 回读校验：写进去的必须是合法 JSON，否则原文件保持不动。
    let written = fs::read_to_string(&tmp)
        .with_context(|| format!("回读临时配置文件失败: {}", tmp.display()))?;
    serde_json::from_str::<Value>(&written)
        .with_context(|| format!("回读校验失败，配置不是合法 JSON: {}", tmp.display()))?;

    fs::rename(&tmp, path).with_context(|| format!("替换配置文件失败: {}", path.display()))?;
    Ok(())
}

/// 备份策略执行：按策略决定是否在保存前生成备份。
///
/// - 目录中**没有任何备份**时：强制备份一次（首次强制，不受开关影响）。
/// - 已有备份时：按开关决定是否再备份。
/// - 备份内容是**保存前**的旧配置（写后备份存的是新内容，没有回滚价值）。
pub fn backup_config(path: &Path, policy: BackupPolicy) -> anyhow::Result<BackupOutcome> {
    let first_time = !has_any_backup(path);
    if policy == BackupPolicy::Disabled && !first_time {
        return Ok(BackupOutcome::default());
    }
    if !path.exists() {
        // 还没有配置文件可备份（首次生成），不产生空备份。
        return Ok(BackupOutcome::default());
    }

    let target = backup_path(path);
    // 轮转：把 .bak.N 依次后移，.bak 变成 .bak.1。
    rotate_backups(path)?;

    fs::copy(path, &target)
        .with_context(|| format!("备份配置失败: {} -> {}", path.display(), target.display()))?;
    Ok(BackupOutcome {
        created: true,
        path: Some(target),
        forced_first: first_time,
    })
}

/// 备份轮转：`.bak` → `.bak.1` → `.bak.2` …，超出 `BACKUP_KEEP` 的最旧一份丢弃。
///
/// 总份数 = `BACKUP_KEEP`（含最近一次 `.bak`）：即 `.bak` + `.bak.1..bak.N-1`。
fn rotate_backups(path: &Path) -> anyhow::Result<()> {
    // 先删掉最旧的一份，为后移腾位置。
    let oldest = backup_history_path(path, BACKUP_KEEP - 1);
    if oldest.exists() {
        let _ = fs::remove_file(&oldest);
    }
    // 从次新往最新依次后移：.bak.N → .bak.(N+1)
    for index in (1..BACKUP_KEEP - 1).rev() {
        let from = backup_history_path(path, index);
        if from.exists() {
            let to = backup_history_path(path, index + 1);
            let _ = fs::remove_file(&to);
            fs::rename(&from, &to)
                .with_context(|| format!("轮转备份失败: {} -> {}", from.display(), to.display()))?;
        }
    }
    // .bak → .bak.1
    let newest = backup_path(path);
    if newest.exists() {
        let first = backup_history_path(path, 1);
        let _ = fs::remove_file(&first);
        fs::rename(&newest, &first).with_context(|| {
            format!("轮转备份失败: {} -> {}", newest.display(), first.display())
        })?;
    }
    Ok(())
}

/// 列出可用备份（最近一次在前），供面板展示。
pub fn list_backups(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let newest = backup_path(path);
    if newest.exists() {
        out.push(newest);
    }
    for index in 1..BACKUP_KEEP {
        let candidate = backup_history_path(path, index);
        if candidate.exists() {
            out.push(candidate);
        }
    }
    out
}

/// 用最近一次备份覆盖当前配置。
///
/// 覆盖前先把**当前**内容另存一份 `.bak.pre-restore`：恢复动作本身不可逆，
/// 保留现场以便回退。
pub fn restore_backup(path: &Path) -> anyhow::Result<PathBuf> {
    let source = backup_path(path);
    if !source.exists() {
        return Err(anyhow!("没有可用的备份文件：{}", source.display()));
    }
    let safety = {
        let mut name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "bridge.config.json".to_string());
        name.push_str(BACKUP_SUFFIX);
        name.push_str(".pre-restore");
        path.with_file_name(name)
    };
    if path.exists() {
        let _ = fs::remove_file(&safety);
        fs::copy(path, &safety)
            .with_context(|| format!("保存恢复前现场失败: {}", safety.display()))?;
    }
    let text = fs::read_to_string(&source)
        .with_context(|| format!("读取备份失败: {}", source.display()))?;
    serde_json::from_str::<Value>(&text)
        .with_context(|| format!("备份内容不是合法 JSON: {}", source.display()))?;
    fs::write(path, text.as_bytes())
        .with_context(|| format!("写回配置失败: {}", path.display()))?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("modelgate-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn merge_keeps_other_top_level_keys() {
        let current = json!({
            "port": 8900,
            "host": "127.0.0.1",
            "defaultModel": "a",
            "upstreams": {"u1": {"protocol": "openai", "baseUrl": "https://x"}}
        });
        let incoming = json!({"defaultModel": "b"});
        let merged = merge_incoming(&current, &incoming);
        assert_eq!(merged["defaultModel"], json!("b"));
        // 其余顶层键不得丢失
        assert_eq!(merged["port"], json!(8900));
        assert_eq!(merged["host"], json!("127.0.0.1"));
        assert!(merged.get("upstreams").is_some());
    }

    #[test]
    fn merge_adds_updates_and_deletes_upstreams() {
        let current = json!({
            "upstreams": {
                "keep":   {"protocol": "openai", "baseUrl": "https://k", "cliVersion": "1.2.3"},
                "change": {"protocol": "openai", "baseUrl": "https://old"},
                "drop":   {"protocol": "openai", "baseUrl": "https://d"}
            }
        });
        // 面板：保留 keep（不改 baseUrl）、改 change、删 drop、新增 brand
        let incoming = json!({
            "upstreams": {
                "keep":   {"default": true},
                "change": {"baseUrl": "https://new"},
                "brand":  {"protocol": "anthropic", "baseUrl": "https://b"}
            }
        });
        let merged = merge_incoming(&current, &incoming);
        let ups = merged["upstreams"].as_object().unwrap();
        // 删除是真实的
        assert!(!ups.contains_key("drop"), "删除的上游不应留在结果中");
        // 新增
        assert_eq!(ups["brand"]["protocol"], json!("anthropic"));
        // 同名改字段
        assert_eq!(ups["change"]["baseUrl"], json!("https://new"));
        assert_eq!(
            ups["change"]["protocol"],
            json!("openai"),
            "未传的字段应保留旧值"
        );
        // 同名补字段 + 未渲染的兼容字段保留
        assert_eq!(ups["keep"]["default"], json!(true));
        assert_eq!(ups["keep"]["cliVersion"], json!("1.2.3"));
    }

    #[test]
    fn merge_without_upstreams_keeps_current_set() {
        let current = json!({"upstreams": {"a": {"protocol": "openai"}}});
        let merged = merge_incoming(&current, &json!({"port": 9000}));
        assert!(merged["upstreams"].get("a").is_some());
        assert_eq!(merged["port"], json!(9000));
    }

    #[test]
    fn strip_secrets_removes_literal_key_only() {
        let mut v = json!({
            "upstreams": {
                "a": {"apiKey": "secret", "apiKeyEnv": "MY_ENV", "baseUrl": "https://x"}
            }
        });
        strip_secrets(&mut v);
        let up = &v["upstreams"]["a"];
        assert!(up.get("apiKey").is_none(), "明文密钥不得留在落盘内容里");
        assert_eq!(up["apiKeyEnv"], json!("MY_ENV"), "密钥引用应保留");
    }

    #[test]
    fn strip_secrets_removes_readonly_internal_fields() {
        // 面板回传的只读内部字段不得落盘（会污染用户的配置文件）。
        let mut v = json!({
            "upstreams": {
                "a": {"apiKey": "s", "_keyConfigured": true, "_keySource": "literal",
                      "baseUrl": "https://x"}
            }
        });
        strip_secrets(&mut v);
        let up = &v["upstreams"]["a"];
        assert!(up.get("_keyConfigured").is_none());
        assert!(up.get("_keySource").is_none());
    }

    #[test]
    fn strip_secrets_covers_legacy_single_upstream_field() {
        // 旧版单上游形态 config.upstream.apiKey 同样不得落盘。
        // 用占位值而非像密钥的字符串：避免被敏感信息门禁误判为真凭据。
        let mut v = json!({
            "upstream": {"protocol": "openai", "apiKey": "<REDACTED>",
                         "apiKeyFile": "~/.key", "baseUrl": "https://x"}
        });
        strip_secrets(&mut v);
        assert!(
            v["upstream"].get("apiKey").is_none(),
            "旧版单上游的明文密钥也不得落盘"
        );
        assert_eq!(v["upstream"]["apiKeyFile"], json!("~/.key"));
    }

    #[test]
    fn save_atomic_roundtrips_valid_json() {
        let dir = temp_dir("atomic");
        let path = dir.join("bridge.config.json");
        let value = json!({"port": 8900, "upstreams": {"a": {"protocol": "openai"}}});
        save_atomic(&path, &value).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["port"], json!(8900));
        // 不应留下临时文件
        assert!(!path.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_save_forces_backup_even_when_disabled() {
        let dir = temp_dir("first-backup");
        let path = dir.join("bridge.config.json");
        fs::write(&path, "{\"port\": 1}").unwrap();
        // 开关关闭，但目录里没有任何备份 → 仍强制备份一次
        let outcome = backup_config(&path, BackupPolicy::Disabled).unwrap();
        assert!(outcome.created);
        assert!(outcome.forced_first);
        assert!(backup_path(&path).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_policy_with_existing_backup_creates_nothing() {
        let dir = temp_dir("disabled");
        let path = dir.join("bridge.config.json");
        fs::write(&path, "{\"port\": 1}").unwrap();
        let first = backup_config(&path, BackupPolicy::Enabled).unwrap();
        assert!(first.created);
        let before = fs::read_to_string(&backup_path(&path)).unwrap();

        // 已有备份 + 开关关闭 → 不生成新备份，且旧备份内容不变
        fs::write(&path, "{\"port\": 2}").unwrap();
        let second = backup_config(&path, BackupPolicy::Disabled).unwrap();
        assert!(!second.created);
        assert_eq!(fs::read_to_string(&backup_path(&path)).unwrap(), before);
        // 旧备份仍在
        assert!(backup_path(&path).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_policy_rotates_and_keeps_latest_five() {
        let dir = temp_dir("rotate");
        let path = dir.join("bridge.config.json");
        // 连续 8 次保存 → 保留最近 5 份
        for i in 0..8 {
            fs::write(&path, format!("{{\"port\": {i}}}")).unwrap();
            backup_config(&path, BackupPolicy::Enabled).unwrap();
        }
        assert!(backup_path(&path).exists(), "最近一次备份应存在");
        for index in 1..BACKUP_KEEP {
            assert!(
                backup_history_path(&path, index).exists(),
                "历史备份 .bak.{index} 应存在"
            );
        }
        assert!(
            !backup_history_path(&path, BACKUP_KEEP).exists(),
            "超出保留份数的备份应被清理"
        );
        // 总份数 = BACKUP_KEEP
        assert_eq!(list_backups(&path).len(), BACKUP_KEEP);
        // 最近一次备份的内容 = 倒数第二次保存的旧内容
        let newest: Value =
            serde_json::from_str(&fs::read_to_string(&backup_path(&path)).unwrap()).unwrap();
        assert_eq!(newest["port"], json!(7));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_backup_writes_back_and_keeps_pre_restore_copy() {
        let dir = temp_dir("restore");
        let path = dir.join("bridge.config.json");
        fs::write(&path, "{\"port\": 1}").unwrap();
        backup_config(&path, BackupPolicy::Enabled).unwrap();
        fs::write(&path, "{\"port\": 2}").unwrap();

        let used = restore_backup(&path).unwrap();
        assert_eq!(used, backup_path(&path));
        let restored: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["port"], json!(1), "恢复后应是备份时的内容");
        // 恢复前的现场被保留
        let safety = path.with_file_name("bridge.config.json.bak.pre-restore");
        assert!(safety.exists(), "恢复前现场应被保留");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_without_backup_fails_without_touching_config() {
        let dir = temp_dir("restore-missing");
        let path = dir.join("bridge.config.json");
        fs::write(&path, "{\"port\": 7}").unwrap();
        assert!(restore_backup(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"port\": 7}");
        let _ = fs::remove_dir_all(&dir);
    }
}
