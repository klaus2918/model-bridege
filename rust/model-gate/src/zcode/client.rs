//! `backend=appserver` 的可用实现：调用本机 ZCode CLI 的一次性 `-p --json`。
//!
//! 为什么不是自制 JSON-RPC：实测 `zcode.cjs -p ... --json` 能直接走 Start Plan
//! 并返回结构化 response/usage；app-server 的 JSON-RPC 会话/工具权限/回合锁
//! 复杂度高，先用 CLI 作为可靠 backend。后续可替换为常驻 app-server。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use super::config::ZcodeConfig;
use super::store::ZcodeAccount;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliUsage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliResponse {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub usage: Option<CliUsage>,
    #[serde(default)]
    pub event_count: i64,
}

#[derive(Debug, Clone)]
pub struct ZcodeCli {
    pub cli_path: PathBuf,
    pub node_path: String,
    pub data_dir: PathBuf,
    pub builtin_dir: String,
    pub account_dir: PathBuf,
    pub timeout_seconds: u64,
}

impl ZcodeCli {
    pub fn from_config(config: &ZcodeConfig, account: &ZcodeAccount) -> Result<Self, String> {
        let cli_path = config.resolved_cli_path();
        if !cli_path.is_file() {
            return Err(format!(
                "未找到 ZCode CLI：{}（请在 ZCode 设置中配置 cliPath）",
                cli_path.display()
            ));
        }
        let data_dir = config.resolved_data_dir();
        let account_dir = config
            .resolved_data_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("zcode")
            .join("accounts")
            .join(&account.id);
        Ok(Self {
            cli_path,
            node_path: config.resolved_node(),
            data_dir,
            builtin_dir: config.builtin_dir.clone(),
            account_dir,
            timeout_seconds: 180,
        })
    }

    /// 为账号准备独立 `ZCODE_DATA_BASE_DIR`：写一份加密 credentials，避免多个
    /// 账号共用 `~/.zcode` 的登录态。CLI 的 provider 配置仍指向真实 dataDir。
    pub(crate) fn prepare_account_home(&self, account: &ZcodeAccount) -> Result<PathBuf, String> {
        let home = self.account_dir.join(".zcode");
        let v2 = home.join("v2");
        std::fs::create_dir_all(&v2).map_err(|err| format!("创建 ZCode 账号目录失败：{err}"))?;
        let token = account.plain_secret()?;
        if account.mode != "jwt" {
            return Err("backend=appserver 目前仅支持 JWT 账号".to_string());
        }
        let encrypted =
            super::credentials::encrypt_for_zcode(&token, &super::credentials::default_secret())?;
        let credentials = serde_json::json!({
            "zcodejwttoken": encrypted,
            "oauth:active_provider": if account.provider.trim().is_empty() {
                "zai"
            } else {
                account.provider.as_str()
            },
        });
        let text = serde_json::to_string_pretty(&credentials)
            .map_err(|err| format!("序列化 ZCode 凭据失败：{err}"))?;
        std::fs::write(v2.join("credentials.json"), text)
            .map_err(|err| format!("写入 ZCode 账号凭据失败：{err}"))?;
        Ok(home)
    }

    pub(crate) fn personal_provider_path(&self) -> PathBuf {
        // PoC 同款：app-server 读取真实个人 provider 配置（provider/model 规则）。
        self.data_dir.join("v2").join("provider_config.json")
    }

    /// 账号隔离的 ZCode home（`ZCODE_DATA_BASE_DIR`）。
    pub(crate) fn account_home(&self) -> PathBuf {
        self.account_dir.join(".zcode")
    }

    pub(crate) fn builtin_provider_path(&self) -> Result<(String, PathBuf), String> {
        let explicit = self.builtin_dir.trim();
        if !explicit.is_empty() {
            let path = PathBuf::from(explicit);
            if path.is_file() {
                let version = path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("3.16.3")
                    .to_string();
                return Ok((version, path));
            }
            if path.is_dir() {
                if let Some(found) = find_builtin_in(&path) {
                    return Ok(found);
                }
            }
            return Err(format!("builtinDir 不可用：{explicit}"));
        }
        detect_builtin_runtime(&self.data_dir)
            .ok_or_else(|| "未找到 ZCode builtin provider 配置（请配置 builtinDir）".to_string())
    }

    pub async fn run(&self, account: &ZcodeAccount, prompt: &str) -> Result<CliResponse, String> {
        let home = self.prepare_account_home(account)?;
        let (version, builtin) = self.builtin_provider_path()?;
        let personal = self.personal_provider_path();
        let mut command = Command::new(&self.node_path);
        command
            .arg(&self.cli_path)
            .arg("-p")
            .arg(prompt)
            .arg("--json")
            .arg("--no-color")
            .arg("--surface")
            .arg("terminal")
            .arg("--mode")
            .arg("guarded")
            .arg("--disallowed-tools")
            .arg("Bash Edit Write Read Glob Grep WebSearch WebFetch Task TodoWrite NotebookEdit")
            .current_dir(&home)
            .env("ZCODE_SERVICE_AUTHORITY_MODE", "desktop-local")
            .env("ZCODE_APP_VERSION", version)
            .env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", builtin)
            .env("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE", personal)
            .env("ZCODE_BASE_URL", "https://zcode.z.ai")
            .env("ZCODE_DATA_BASE_DIR", home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：避免 CLI 调用弹出控制台窗口。
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        }
        let output =
            tokio::time::timeout(Duration::from_secs(self.timeout_seconds), command.output())
                .await
                .map_err(|_| format!("ZCode CLI 超时（{}s）", self.timeout_seconds))?
                .map_err(|err| format!("启动 ZCode CLI 失败：{err}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            let detail = stderr.trim();
            return Err(format!(
                "ZCode CLI 退出码 {}：{}",
                output.status.code().unwrap_or(-1),
                if detail.is_empty() {
                    stdout.trim()
                } else {
                    detail
                }
            ));
        }
        let text = extract_json(&stdout)
            .ok_or_else(|| format!("ZCode CLI 输出不是 JSON：{}", stdout.trim()))?;
        let parsed: CliResponse = serde_json::from_str(&text)
            .map_err(|err| format!("解析 ZCode CLI JSON 失败：{err}"))?;
        Ok(parsed)
    }
}

fn find_builtin_in(root: &Path) -> Option<(String, PathBuf)> {
    let mut candidates = Vec::new();
    collect_builtin(root, &mut candidates, 0);
    candidates
        .into_iter()
        .max_by_key(|(version, _)| version.clone())
}

fn collect_builtin(dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|s| s.to_str()) == Some("zcode-builtin.json")
        {
            let version = path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("3.16.3")
                .to_string();
            out.push((version, path));
        } else if path.is_dir() {
            collect_builtin(&path, out, depth + 1);
        }
    }
}

pub fn detect_builtin_runtime(data_dir: &Path) -> Option<(String, PathBuf)> {
    let platform = if cfg!(windows) {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    };
    let root = data_dir
        .join("v2")
        .join("runtime")
        .join("provider")
        .join(platform);
    find_builtin_in(&root)
}

fn extract_json(stdout: &str) -> Option<String> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(stdout[start..=end].to_string())
}

/// 把 OpenAI/Anthropic 的 payload 拍平成 CLI prompt。保留 system 与多轮文本；
/// 工具/图片首期只提取文本，避免把二进制塞进命令行。
pub fn prompt_from_payload(payload: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = payload.get("system") {
        if let Some(text) = value_text(system) {
            if !text.trim().is_empty() {
                parts.push(format!("[system]\n{}", text.trim()));
            }
        }
    }
    if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            if let Some(text) = message.get("content").and_then(value_text) {
                if !text.trim().is_empty() {
                    parts.push(format!("[{role}]\n{}", text.trim()));
                }
            }
        }
    }
    if parts.is_empty() {
        payload
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("ping")
            .to_string()
    } else {
        parts.join("\n\n")
    }
}

fn value_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(items) = value.as_array() {
        let mut out = Vec::new();
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                out.push(text.to_string());
            } else if item.get("type").and_then(Value::as_str) == Some("image") {
                out.push("[image omitted]".to_string());
            }
        }
        return Some(out.join("\n"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_includes_system_and_messages() {
        let payload = serde_json::json!({
            "system": "be nice",
            "messages": [
                {"role":"user","content":"hi"},
                {"role":"assistant","content":[{"type":"text","text":"hello"}]}
            ]
        });
        let prompt = prompt_from_payload(&payload);
        assert!(prompt.contains("[system]\nbe nice"));
        assert!(prompt.contains("[user]\nhi"));
        assert!(prompt.contains("[assistant]\nhello"));
    }

    #[test]
    fn extract_json_ignores_log_prefix() {
        let text = "log line\n{\"response\":\"PONG\"}\n";
        assert_eq!(
            extract_json(text).as_deref(),
            Some("{\"response\":\"PONG\"}")
        );
    }
}
