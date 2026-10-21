//! 用户指定的凭据文件写入（JSON / .env）。
//!
//! 口径（用户确认）：
//! - 凭据文件位置**由用户手填**，代码里没有任何内置默认路径；
//! - 密钥只写用户点名的那个文件，Model-Gate 自己的配置（`bridge.config.json`）只存路径引用；
//! - 写入后用与运行时**完全相同**的读取语义回读校验，保证「写完即生效」。
//!
//! 安全约定：
//! - 明文只落在目标文件里；错误信息只出现路径与原因，**不含密钥**；
//! - 拒绝写入 Model-Gate 自己的配置文件；
//! - 拒绝含换行/控制字符的密钥（`.env` 单行语义 + JSON 注入风险）；
//! - 不生成 `.bak` 备份（避免明文副本扩散）；
//! - 原子写：同目录临时文件 → `rename` 覆盖。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// 凭据文件格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFileFormat {
    /// JSON 对象：按点路径写入（缺省 `apiKey`）。
    Json,
    /// `.env` 文本：按变量名写入（`NAME=sk-...`）。
    Env,
}

impl KeyFileFormat {
    /// 解析用户填写的格式名（`json` / `env`），未知值返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" => Some(KeyFileFormat::Json),
            "env" | "dotenv" | ".env" => Some(KeyFileFormat::Env),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            KeyFileFormat::Json => "json",
            KeyFileFormat::Env => "env",
        }
    }
}

/// 写入失败原因。
#[derive(Debug, Clone)]
pub enum KeyFileError {
    /// 参数不合法（路径、字段、密钥内容）：用户可修正。
    Invalid(String),
    /// 读写文件失败：环境问题。
    Io(String),
}

impl std::fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyFileError::Invalid(msg) | KeyFileError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

/// 写入结果（不含明文）。
#[derive(Debug, Clone)]
pub struct KeyFileWrite {
    /// 实际写入的绝对路径。
    pub path: PathBuf,
    /// 写入格式（`json` / `env`）。
    pub format: &'static str,
    /// JSON 点路径或 `.env` 变量名。
    pub field: String,
    /// 目标文件此前不存在（本次新建）。
    pub created: bool,
}

/// Windows / POSIX 环境变量名合法性：非空、不含 `=`、不以 `=` 开头。
pub fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('=') && !name.starts_with('=')
}

/// 密钥内容合法性：非空且不含换行/控制字符。
pub fn validate_key(key: &str) -> Result<(), KeyFileError> {
    if key.is_empty() {
        return Err(KeyFileError::Invalid("密钥不能为空".into()));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(KeyFileError::Invalid("密钥不能包含换行或控制字符".into()));
    }
    Ok(())
}

/// 两个路径是否指向同一个文件（按 Windows 语义大小写不敏感）。
pub fn same_path(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let abs = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let text = abs.to_string_lossy().to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    }
    norm(a) == norm(b)
}

/// 把密钥写进用户指定的凭据文件。
///
/// - `path`：用户手填的路径（`~/` 会展开）；**必须**是绝对路径。
/// - `format`：JSON 或 `.env`。
/// - `field`：JSON 点路径（`apiKey` / `deepseek.key`）；`.env` 变量名。JSON 留空按 `apiKey` 处理。
/// - `key`：明文密钥。
/// - `protected`：禁止写入的路径（通常是 Model-Gate 自己的配置文件）。
pub fn write_key_file(
    path: &Path,
    format: KeyFileFormat,
    field: &str,
    key: &str,
    protected: &[PathBuf],
) -> Result<KeyFileWrite, KeyFileError> {
    validate_key(key)?;
    if !path.is_absolute() {
        return Err(KeyFileError::Invalid(format!(
            "凭据文件路径必须是绝对路径：{}",
            path.display()
        )));
    }
    for guard in protected {
        if same_path(path, guard) {
            return Err(KeyFileError::Invalid(
                "不能把密钥写进 Model-Gate 自己的配置文件".into(),
            ));
        }
    }

    let (field, created) = match format {
        KeyFileFormat::Json => {
            let field = field.trim();
            let field = if field.is_empty() { "apiKey" } else { field };
            let created = write_json(path, field, key)?;
            (field.to_string(), created)
        }
        KeyFileFormat::Env => {
            let field = field.trim();
            if field.is_empty() {
                return Err(KeyFileError::Invalid(".env 格式必须指定变量名".into()));
            }
            if !is_valid_env_name(field) {
                return Err(KeyFileError::Invalid(format!("环境变量名不合法：{field}")));
            }
            let created = write_env(path, field, key)?;
            (field.to_string(), created)
        }
    };

    Ok(KeyFileWrite {
        path: path.to_path_buf(),
        format: format.as_str(),
        field,
        created,
    })
}

/// 回读校验：用与运行时一致的读取语义确认密钥真的落到了文件里。
///
/// `provider` 语义与 `config::read_key_from_path` 一致：JSON 传点路径，
/// `.env` 传变量名。读不到返回 `false`。
pub fn verify_key_file(path: &Path, provider: &str) -> bool {
    crate::config::read_key_from_path(path, provider).is_some()
}

/// JSON 读-改-写：保留其他字段，缺中间层建对象。
fn write_json(path: &Path, field: &str, key: &str) -> Result<bool, KeyFileError> {
    let existed = path.exists();
    let mut root = if existed {
        let text = std::fs::read_to_string(path)
            .map_err(|e| KeyFileError::Io(format!("读取 {} 失败：{e}", path.display())))?;
        if text.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str::<Value>(&text).map_err(|_| {
                KeyFileError::Invalid(format!(
                    "目标文件不是合法 JSON，已放弃写入：{}",
                    path.display()
                ))
            })?
        }
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        return Err(KeyFileError::Invalid(format!(
            "目标 JSON 根节点不是对象，已放弃写入：{}",
            path.display()
        )));
    }
    let parts: Vec<&str> = field.split('.').filter(|p| !p.trim().is_empty()).collect();
    if parts.is_empty() {
        return Err(KeyFileError::Invalid("JSON 字段路径不能为空".into()));
    }
    set_json_path(&mut root, &parts, key)?;
    let text = serde_json::to_string_pretty(&root)
        .map_err(|e| KeyFileError::Io(format!("序列化目标 JSON 失败：{e}")))?;
    write_atomic(path, &text)?;
    Ok(!existed)
}

/// 在 JSON 对象里按点路径写字符串值，缺中间层自动建对象。
///
/// 目标节点已经是对象且内含 `key` / `apiKey` / `token` 时，写进**内层**那个字段，
/// 同级的 `type` 等其它字段原样保留 —— 与运行时读取语义
/// （`config::read_key_from_path` 命中对象后会再往里取一层）保持一致。
fn set_json_path(node: &mut Value, parts: &[&str], key: &str) -> Result<(), KeyFileError> {
    let (head, rest) = match parts.split_first() {
        Some(v) => v,
        None => return Err(KeyFileError::Invalid("JSON 字段路径不能为空".into())),
    };
    if rest.is_empty() {
        // 先只读地判断目标是否已有「内层 key 字段」的对象节点。
        let inner_field = node
            .as_object()
            .and_then(|m| m.get(*head))
            .and_then(|v| v.as_object())
            .and_then(|obj| {
                ["key", "apiKey", "token"]
                    .iter()
                    .find(|k| obj.contains_key(**k))
                    .map(|k| (*k).to_string())
            });
        let map = node.as_object_mut().ok_or_else(|| {
            KeyFileError::Invalid(format!("字段路径 {} 的目标节点不是对象", parts.join(".")))
        })?;
        match inner_field {
            Some(inner) => {
                if let Some(obj) = map.get_mut(*head).and_then(|v| v.as_object_mut()) {
                    obj.insert(inner, Value::String(key.to_string()));
                }
            }
            None => {
                map.insert((*head).to_string(), Value::String(key.to_string()));
            }
        }
        return Ok(());
    }
    let map = node.as_object_mut().ok_or_else(|| {
        KeyFileError::Invalid(format!("字段路径 {} 的中间节点不是对象", parts.join(".")))
    })?;
    let entry = map
        .entry((*head).to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        return Err(KeyFileError::Invalid(format!(
            "字段路径 {} 的中间节点 {head} 不是对象",
            parts.join(".")
        )));
    }
    set_json_path(entry, rest, key)
}

/// `.env` 读-改-写：同名变量就地替换，没有则追加到末尾。
fn write_env(path: &Path, var: &str, key: &str) -> Result<bool, KeyFileError> {
    let existed = path.exists();
    let text = if existed {
        std::fs::read_to_string(path)
            .map_err(|e| KeyFileError::Io(format!("读取 {} 失败：{e}", path.display())))?
    } else {
        String::new()
    };
    let new_line = format!("{var}={key}");
    let mut replaced = false;
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let is_comment = line.trim_start().starts_with('#');
        let is_target = !is_comment
            && line
                .split_once('=')
                .map(|(k, _)| k.trim() == var)
                .unwrap_or(false);
        if is_target {
            // 同名变量只保留第一条，后面的重复项丢弃。
            if !replaced {
                lines.push(new_line.clone());
                replaced = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.push(new_line);
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    write_atomic(path, &out)?;
    Ok(!existed)
}

/// 原子写：同目录临时文件 → rename 覆盖（Windows 下 `rename` 会替换已存在文件）。
fn write_atomic(path: &Path, content: &str) -> Result<(), KeyFileError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                KeyFileError::Io(format!("创建目录 {} 失败：{e}", parent.display()))
            })?;
        }
    }
    let tmp = temp_sibling(path);
    std::fs::write(&tmp, content)
        .map_err(|e| KeyFileError::Io(format!("写入临时文件失败：{e}")))?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(KeyFileError::Io(format!("替换目标文件失败：{err}")));
    }
    Ok(())
}

/// 目标文件同目录下的临时文件名。
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model-gate-key");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{name}.mg-tmp-{}-{stamp}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mg-key-file-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn json_write_creates_file_with_default_field() {
        let dir = temp_dir("json-default");
        let path = dir.join("auth.json");
        let out = write_key_file(&path, KeyFileFormat::Json, "", "sk-test", &[]).unwrap();
        assert!(out.created);
        assert_eq!(out.field, "apiKey");
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value.get("apiKey").unwrap().as_str().unwrap(), "sk-test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_write_keeps_other_fields_and_creates_missing_levels() {
        let dir = temp_dir("json-merge");
        let path = dir.join("auth.json");
        std::fs::write(&path, r#"{"other":"keep","deepseek":{"type":"api"}}"#).unwrap();
        write_key_file(&path, KeyFileFormat::Json, "deepseek.key", "sk-merge", &[]).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value.get("other").unwrap().as_str().unwrap(), "keep");
        assert_eq!(value["deepseek"]["type"].as_str().unwrap(), "api");
        assert_eq!(value["deepseek"]["key"].as_str().unwrap(), "sk-merge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_write_reuses_inner_key_field_of_object_node() {
        let dir = temp_dir("json-inner");
        let path = dir.join("auth.json");
        std::fs::write(&path, r#"{"deepseek":{"type":"api","key":"old","id":123}}"#).unwrap();
        write_key_file(&path, KeyFileFormat::Json, "deepseek", "sk-inner", &[]).unwrap();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // 目标节点本身是对象且含内层 key 字段 → 写进内层，同级字段原样保留。
        assert_eq!(value["deepseek"]["key"].as_str().unwrap(), "sk-inner");
        assert_eq!(value["deepseek"]["type"].as_str().unwrap(), "api");
        assert_eq!(value["deepseek"]["id"].as_u64().unwrap(), 123);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_write_refuses_non_json_target() {
        let dir = temp_dir("json-invalid");
        let path = dir.join("broken.json");
        std::fs::write(&path, "not json at all").unwrap();
        let err = write_key_file(&path, KeyFileFormat::Json, "apiKey", "sk-x", &[]).unwrap_err();
        assert!(err.to_string().contains("合法 JSON"));
        // 原文件内容不被破坏。
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_write_replaces_existing_variable() {
        let dir = temp_dir("env-replace");
        let path = dir.join("app.env");
        std::fs::write(&path, "# comment\nOLD=1\nAPI_KEY=old-value\nTAIL=2\n").unwrap();
        write_key_file(&path, KeyFileFormat::Env, "API_KEY", "sk-new", &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# comment"));
        assert!(text.contains("OLD=1"));
        assert!(text.contains("TAIL=2"));
        assert!(text.contains("API_KEY=sk-new"));
        assert!(!text.contains("old-value"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_write_appends_missing_variable() {
        let dir = temp_dir("env-append");
        let path = dir.join("app.env");
        std::fs::write(&path, "OLD=1\n").unwrap();
        write_key_file(&path, KeyFileFormat::Env, "API_KEY", "sk-appended", &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("OLD=1"));
        assert!(text.contains("API_KEY=sk-appended"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_format_requires_variable_name() {
        let dir = temp_dir("env-novar");
        let path = dir.join("app.env");
        let err = write_key_file(&path, KeyFileFormat::Env, "  ", "sk-x", &[]).unwrap_err();
        assert!(err.to_string().contains("变量名"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_key_with_newline() {
        let dir = temp_dir("newline");
        let path = dir.join("auth.json");
        let err =
            write_key_file(&path, KeyFileFormat::Json, "apiKey", "sk-a\nsk-b", &[]).unwrap_err();
        assert!(err.to_string().contains("换行"));
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_relative_path() {
        let err = write_key_file(
            Path::new("relative/auth.json"),
            KeyFileFormat::Json,
            "apiKey",
            "sk-x",
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("绝对路径"));
    }

    #[test]
    fn rejects_protected_path() {
        let dir = temp_dir("protected");
        let path = dir.join("bridge.config.json");
        std::fs::write(&path, "{}").unwrap();
        let err = write_key_file(
            &path,
            KeyFileFormat::Json,
            "apiKey",
            "sk-x",
            &[dir.join("bridge.config.json")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("不能把密钥写进"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_missing_parent_directory() {
        let dir = temp_dir("mkdir");
        let path = dir.join("nested/deep/auth.json");
        write_key_file(&path, KeyFileFormat::Json, "apiKey", "sk-nested", &[]).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_backup_file_is_created() {
        let dir = temp_dir("nobak");
        let path = dir.join("auth.json");
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        write_key_file(&path, KeyFileFormat::Json, "apiKey", "sk-x", &[]).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".bak")));
        assert!(names.iter().all(|n| !n.contains("mg-tmp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_name_validation_matches_runtime_rules() {
        assert!(is_valid_env_name("API_KEY"));
        assert!(!is_valid_env_name(""));
        assert!(!is_valid_env_name("A=B"));
        assert!(!is_valid_env_name("=A"));
    }

    #[test]
    fn format_parsing() {
        assert_eq!(KeyFileFormat::parse("JSON"), Some(KeyFileFormat::Json));
        assert_eq!(KeyFileFormat::parse(" env "), Some(KeyFileFormat::Env));
        assert_eq!(KeyFileFormat::parse("yaml"), None);
    }

    #[test]
    fn written_key_is_readable_by_runtime_reader() {
        // 与 config::read_key_from_path 同语义：写完必须能读回。
        let dir = temp_dir("roundtrip");
        let json_path = dir.join("auth.json");
        write_key_file(
            &json_path,
            KeyFileFormat::Json,
            "deepseek.key",
            "sk-rt",
            &[],
        )
        .unwrap();
        assert!(verify_key_file(&json_path, "deepseek.key"));
        assert_eq!(
            crate::config::read_key_from_path(&json_path, "deepseek.key")
                .map(|(v, _)| v)
                .unwrap_or_default(),
            "sk-rt"
        );

        let env_path = dir.join("app.env");
        write_key_file(&env_path, KeyFileFormat::Env, "API_KEY", "sk-rt-env", &[]).unwrap();
        assert!(verify_key_file(&env_path, "API_KEY"));
        assert_eq!(
            crate::config::read_key_from_path(&env_path, "API_KEY")
                .map(|(v, _)| v)
                .unwrap_or_default(),
            "sk-rt-env"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
