//! 密钥通道不变量守卫（env-only-secrets）。
//!
//! 需求口径（用户确认）：
//! 1. 密钥**只写入** Windows 用户级环境变量（`HKCU\Environment`）；
//! 2. 密钥**只读取**进程环境变量与本进程刚写入的用户变量；
//! 3. 代码与配置模板中**不得内置** jcode / opencode / commandcode 等
//!    厂商客户端的凭据文件地址；
//! 4. `bridge.config.json` 中不得写死明文密钥。
//!
//! 这些约束属于「只靠自觉维持」的不变量：只要有人日后为了兼容性
//! 加回一行内置路径，用户「不落盘、不猜路径」的安全承诺就会被静默破坏。
//! 本文件把它们固化为纯静态断言，不启动服务、不访问网络。

use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn repo_root() -> PathBuf {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .expect("crate 应位于 <repo>/rust/model-gate")
        .to_path_buf()
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn rust_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_sources(&crate_root().join("src"), &mut files);
    files.sort();
    files
}

/// 厂商客户端凭据路径的识别片段。
///
/// 用「路径片段」而不是完整字符串，避免有人只改前缀（例如把 `~` 换成
/// `$HOME` 或 `%USERPROFILE%`）就绕过守卫。
const FORBIDDEN_CREDENTIAL_PATH_FRAGMENTS: &[&str] = &[
    ".jcode/.env",
    "local/share/opencode/auth.json",
    ".config/opencode/auth.json",
    "commandcode/auth.json",
];

#[test]
fn rust_sources_never_embed_vendor_credential_paths() {
    let mut violations: Vec<String> = Vec::new();
    for path in rust_sources() {
        let text = fs::read_to_string(&path).expect("read rust source");
        for (idx, line) in text.lines().enumerate() {
            for fragment in FORBIDDEN_CREDENTIAL_PATH_FRAGMENTS {
                if line.contains(fragment) {
                    violations.push(format!(
                        "{}:{} 出现内置凭据路径片段 {:?}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        idx + 1,
                        fragment
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "不得内置厂商凭据文件地址（只能由用户显式填写 apiKeyAgentPath）：\n{}",
        violations.join("\n")
    );
}

#[test]
fn user_env_module_targets_current_user_hive_only() {
    let text = fs::read_to_string(crate_root().join("src/user_env.rs")).expect("read user_env.rs");
    assert!(
        text.contains("HKEY_CURRENT_USER"),
        "用户变量必须写入 HKEY_CURRENT_USER"
    );
    assert!(
        !text.contains("HKEY_LOCAL_MACHINE"),
        "不得写入机器级环境变量（HKLM）——作用域越权"
    );
    assert!(
        text.contains("Environment"),
        "用户环境变量子键应为 Environment"
    );
}

#[test]
fn example_config_has_no_embedded_secret_or_env_file() {
    let path = repo_root().join("bridge.config.example.json");
    let text = fs::read_to_string(&path).expect("read bridge.config.example.json");

    for fragment in FORBIDDEN_CREDENTIAL_PATH_FRAGMENTS {
        assert!(
            !text.contains(fragment),
            "配置模板仍内置凭据路径 {:?} —— 请删除或改为空占位",
            fragment
        );
    }
    assert!(
        !text.contains("\"envFile\""),
        "envFile 已失效：配置模板不得再出现该字段"
    );

    let value: serde_json::Value = serde_json::from_str(&text).expect("模板必须是合法 JSON");
    let mut hits: Vec<String> = Vec::new();
    walk_for_literal_keys(&value, "$", &mut hits);
    assert!(
        hits.is_empty(),
        "配置模板写死了明文密钥（禁止落盘）：\n{}",
        hits.join("\n")
    );
}

#[test]
fn panel_exposes_explicit_key_file_path() {
    let text = fs::read_to_string(crate_root().join("ui/app.js")).expect("read ui/app.js");
    assert!(
        text.contains("data-field=\"apiKeyFile\""),
        "面板必须提供「密钥文件（apiKeyFile）」输入框，允许用户手动指定文件"
    );
    assert!(
        text.contains("entry.apiKeyFile = keyFile"),
        "保存配置时必须把 apiKeyFile 路径写回，否则面板输入不生效"
    );
    assert!(
        text.contains("else delete entry.apiKeyFile"),
        "清空路径时必须删除旧 apiKeyFile，避免沿用已失效的文件"
    );
}

/// 遍历 JSON，找出所有非空的 `apiKey` 字面量。
///
/// `apiKeyEnv` / `apiKeyFile` / `apiKeyAgent*` 是引用，不是密钥本体，故放行。
fn walk_for_literal_keys(value: &serde_json::Value, path: &str, hits: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if key == "apiKey" {
                    if let Some(secret) = child.as_str() {
                        if !secret.trim().is_empty() {
                            hits.push(format!("{} 含非空明文 apiKey", child_path));
                        }
                    }
                }
                walk_for_literal_keys(child, &child_path, hits);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_for_literal_keys(child, &format!("{path}[{index}]"), hits);
            }
        }
        _ => {}
    }
}
