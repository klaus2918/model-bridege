//! ZCode app-server（JSON-RPC over stdio）transport worker。
//!
//! 为什么不用 `zcode.cjs -p`：CLI 的 print 模式依赖已有会话默认模型，
//! 未选择模型时直接报 `Select a model before sending a message`，且没有
//! `--model` 开关。app-server 提供 `session/setModel`，可显式指定
//! `providerId/modelId`，与 PoC 一致。
//!
//! 边界：app-server 只作为内部 transport worker；账号库、UI、claim 都在
//! Model-Gate。工具/浏览器权限一律 deny，首期只做纯文本。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::client::ZcodeCli;
use super::config::ZcodeConfig;
use super::store::ZcodeAccount;

const STARTUP_WAIT: Duration = Duration::from_secs(3);
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(150);

#[derive(Debug, Clone)]
pub struct AppServerResponse {
    pub text: String,
    pub usage: Option<Value>,
}

struct Rpc {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    home: PathBuf,
    next_id: u64,
    token: String,
    app_version: String,
    text: String,
    usage: Option<Value>,
    turn_done: bool,
    turn_error: Option<String>,
}

impl Rpc {
    async fn write(&mut self, msg: &Value) -> Result<(), String> {
        let text = serde_json::to_string(msg).map_err(|err| format!("序列化 RPC 失败：{err}"))?;
        self.stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|err| format!("写入 app-server 失败：{err}"))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|err| format!("写入 app-server 失败：{err}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|err| format!("刷新 app-server 失败：{err}"))?;
        Ok(())
    }

    async fn next_message(&mut self, timeout: Duration) -> Result<Option<Value>, String> {
        let line = tokio::time::timeout(timeout, self.lines.next_line())
            .await
            .map_err(|_| "等待 app-server 消息超时".to_string())?
            .map_err(|err| format!("读取 app-server 失败：{err}"))?;
        let Some(line) = line else {
            return Ok(None);
        };
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        serde_json::from_str::<Value>(line)
            .map(Some)
            .map_err(|err| format!("app-server 输出不是 JSON：{err}"))
    }

    async fn handle_server_request(&mut self, msg: &Value) -> Result<(), String> {
        let Some(id) = msg.get("id").cloned() else {
            return Ok(());
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "session/requestRuntimePreferences" => json!({
                "nativeSearchEnhancementsEnabled": false,
                "memoryEnabled": false,
                "askUserQuestionAutoResolutionEnabled": true,
                "modelContextBudgetStrategy": "preflight-v1",
            }),
            "interaction/requestProviderRuntimeHeaders" => json!({
                "headersApplied": true,
                "requestAuth": {
                    "apiKey": self.token,
                    "headers": {
                        "http-referer": "https://zcode.z.ai",
                        "user-agent": format!("ZCode/{}", self.app_version),
                        "x-zcode-app-version": self.app_version,
                        "x-title": "Z Code@cli",
                    }
                }
            }),
            "interaction/requestPermission" => json!({"decision": "deny"}),
            "interaction/browserList" => json!({"browsers": []}),
            "interaction/browserExecute" => json!({
                "ok": false,
                "error": {"code": "host_unavailable", "message": "model-gate denies browser"}
            }),
            other
                if other.to_lowercase().contains("approval")
                    || other.to_lowercase().contains("permission") =>
            {
                json!({"decision": "deny"})
            }
            _ => json!({}),
        };
        self.write(&json!({"id": id, "result": result})).await
    }

    fn handle_event(&mut self, params: &Value) {
        let payload = params.get("payload").unwrap_or(&Value::Null);
        let ty = params
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| payload.get("type").and_then(Value::as_str))
            .unwrap_or("");
        match ty {
            "model.streaming" => {
                if let Some(delta) = payload.get("delta").and_then(Value::as_str) {
                    self.text.push_str(delta);
                }
            }
            "turn.completed" => {
                self.usage = payload
                    .get("usage")
                    .cloned()
                    .or_else(|| params.get("usage").cloned());
                self.turn_done = true;
            }
            "turn.failed" => {
                self.turn_error = Some(format!(
                    "turn.failed：{}",
                    payload
                        .get("error")
                        .cloned()
                        .unwrap_or_else(|| payload.clone())
                ));
                self.turn_done = true;
            }
            _ => {}
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let Some(msg) = self.next_message(timeout).await? else {
                return Err(self.error_with_stderr(format!("{method}：app-server 已关闭")));
            };
            if let Some(msg_id) = msg.get("id") {
                if msg.get("method").is_none() {
                    if msg_id.as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(err) = msg.get("error") {
                        return Err(self.error_with_stderr(format!(
                            "{method} 失败：{}",
                            err.get("message").and_then(Value::as_str).unwrap_or("")
                        )));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                // 服务端请求的 id 可能是字符串/UUID；不能只按 u64 判断。
                self.handle_server_request(&msg).await?;
                continue;
            }
            if msg.get("method").is_some() {
                self.handle_event(msg.get("params").unwrap_or(&Value::Null));
            }
        }
    }

    async fn wait_turn(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.turn_done {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err("等待 ZCode 回合完成超时".to_string());
            }
            let Some(msg) = self.next_message(remaining).await? else {
                return Err(self.error_with_stderr("app-server 在回合完成前关闭".to_string()));
            };
            if msg.get("id").is_some() && msg.get("method").is_some() {
                self.handle_server_request(&msg).await?;
            } else if msg.get("method").is_some() {
                self.handle_event(msg.get("params").unwrap_or(&Value::Null));
            }
        }
        match self.turn_error.take() {
            Some(err) => Err(self.error_with_stderr(err)),
            None => Ok(()),
        }
    }

    fn error_with_stderr(&self, message: String) -> String {
        let stderr = self
            .stderr
            .try_lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        if stderr.trim().is_empty() {
            message
        } else {
            format!("{message}（{}）", brief(stderr.trim(), 240))
        }
    }

    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

fn brief(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

fn provider_id(account: &ZcodeAccount) -> String {
    let provider = if account.provider.trim().is_empty() {
        "zai"
    } else {
        account.provider.as_str()
    };
    format!("account:{provider}-start-plan")
}

/// 从 builtin provider 配置读取该账户 provider 的模型清单。
fn read_json(path: &Path) -> Option<Value> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!("读取 builtin 配置失败（{}）：{err}", path.display());
            return None;
        }
    };
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    match serde_json::from_slice(bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("解析 builtin 配置失败（{}）：{err}", path.display());
            None
        }
    }
}

/// 从 builtin provider 配置读取该账户 provider 的模型清单。
fn builtin_models(path: &Path, provider: &str) -> Vec<String> {
    builtin_provider_rule(path, provider)
        .and_then(|rule| {
            rule.get("builtinModelIds")
                .and_then(Value::as_array)
                .cloned()
        })
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 从 builtin provider 配置读取该账户 provider 的完整定义。
fn builtin_provider_rule(path: &Path, provider: &str) -> Option<Value> {
    let raw = read_json(path)?;
    let rules = raw
        .pointer("/config/providerConfigRules/providerRules")?
        .as_array()?;
    rules
        .iter()
        .find(|item| item.get("providerId").and_then(Value::as_str) == Some(provider))
        .cloned()
}

fn builtin_revision(path: &Path) -> String {
    let revision = read_json(path)
        .and_then(|raw| {
            raw.get("revision").and_then(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| value.as_i64().map(|n| n.to_string()))
                    .or_else(|| value.as_u64().map(|n| n.to_string()))
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let mut hasher = Sha256::new();
    hasher.update(path.display().to_string().as_bytes());
    format!("zcode-builtin:{revision}:{:x}", hasher.finalize())
}

/// 选一个目标模型：优先请求值，其次 builtin 清单第一个。
fn select_model(requested: &str, available: &[String]) -> String {
    let requested = requested.trim();
    if !requested.is_empty() && (available.is_empty() || available.iter().any(|m| m == requested)) {
        return requested.to_string();
    }
    available
        .first()
        .cloned()
        .unwrap_or_else(|| "GLM-5.3-Flash".to_string())
}

async fn spawn_rpc(
    cli: &ZcodeCli,
    account: &ZcodeAccount,
    config: &ZcodeConfig,
) -> Result<(Rpc, String), String> {
    let _home = cli.prepare_account_home(account)?;
    let real_data_dir = config.resolved_data_dir();
    let real_home = real_data_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let (version, builtin_path) = cli.builtin_provider_path()?;
    let personal = cli.personal_provider_path();
    let token = account.plain_secret()?;
    let mut command = Command::new(config.resolved_node());
    command
        .arg(&cli.cli_path)
        .arg("app-server")
        .arg("--surface")
        .arg("terminal")
        .arg("--no-color")
        .current_dir(&real_home)
        .env("ZCODE_SERVICE_AUTHORITY_MODE", "desktop-local")
        .env("ZCODE_APP_VERSION", &version)
        .env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", &builtin_path)
        .env("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE", &personal)
        .env("ZCODE_BASE_URL", "https://zcode.z.ai")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| format!("启动 ZCode app-server 失败：{err}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "app-server stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "app-server stdout 不可用".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "app-server stderr 不可用".to_string())?;
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_slot = stderr_buf.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut buf) = stderr_slot.try_lock() {
                if buf.len() < 2000 {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        }
    });
    let rpc = Rpc {
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
        stderr: stderr_buf,
        home: real_home,
        next_id: 1,
        token,
        app_version: version.clone(),
        text: String::new(),
        usage: None,
        turn_done: false,
        turn_error: None,
    };
    Ok((rpc, builtin_path.display().to_string()))
}

/// 执行一次纯文本对话。
pub async fn chat(
    config: &ZcodeConfig,
    account: &ZcodeAccount,
    requested_model: &str,
    prompt: &str,
) -> Result<AppServerResponse, String> {
    let cli = ZcodeCli::from_config(config, account)?;
    let (mut rpc, builtin_path) = spawn_rpc(&cli, account, config).await?;
    let result = run_session(
        &mut rpc,
        account,
        requested_model,
        prompt,
        Path::new(&builtin_path),
    )
    .await;
    rpc.shutdown().await;
    result
}

async fn run_session(
    rpc: &mut Rpc,
    account: &ZcodeAccount,
    requested_model: &str,
    prompt: &str,
    builtin_path: &Path,
) -> Result<AppServerResponse, String> {
    // 等启动稳定；期间的 server request 会在第一个 request 循环里处理。
    tokio::time::sleep(STARTUP_WAIT).await;
    let provider = provider_id(account);
    let available = builtin_models(builtin_path, &provider);
    let model = select_model(requested_model, &available);
    let models = if available.is_empty() {
        vec![model.clone()]
    } else {
        available
    };
    let revision = builtin_revision(builtin_path);
    tracing::info!(
        "ZCode patch: provider={} revision={} builtin={}",
        provider,
        revision,
        builtin_path.display()
    );
    let mut providers = serde_json::Map::new();
    let mut states = serde_json::Map::new();
    for candidate in ["account:zai-start-plan", "account:bigmodel-start-plan"] {
        let current = candidate == provider;
        let candidate_models = builtin_models(builtin_path, candidate);
        providers.insert(
            candidate.to_string(),
            json!({
                "access": {"type": "zhipu-account", "entitled": current},
                "builtinModelIds": if candidate_models.is_empty() { models.clone() } else { candidate_models },
            }),
        );
        states.insert(
            candidate.to_string(),
            json!({
                "availability": if current { "available" } else { "pending" },
                "entitled": current,
                "current": current,
            }),
        );
    }
    let mut states = serde_json::Map::new();
    states.insert(
        provider.clone(),
        json!({"availability": "available", "entitled": true, "current": true}),
    );
    let patch = json!({
        "revision": format!("model-gate-zcode:{}", uuid::Uuid::new_v4().simple()),
        "basedOnZCodeBuiltinRevision": revision,
        "providers": providers,
        "states": states,
    });
    let updated = rpc
        .request("provider/updateAccountConfig", patch, RPC_TIMEOUT)
        .await?;
    tracing::info!("ZCode account config: {}", brief(&updated.to_string(), 240));
    match rpc
        .request(
            "provider/refreshRegistry",
            json!({"reason": "model-gate"}),
            RPC_TIMEOUT,
        )
        .await
    {
        Ok(value) => tracing::info!("ZCode provider refresh: {}", brief(&value.to_string(), 120)),
        Err(err) => tracing::warn!("ZCode provider refresh skipped: {}", brief(&err, 160)),
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let workspace = rpc_home(rpc);
    let created = rpc
        .request(
            "session/create",
            json!({"workspace": {"workspacePath": workspace, "workspaceKey": "model-gate-zcode"}}),
            RPC_TIMEOUT,
        )
        .await?;
    let session_id = created
        .pointer("/session/sessionId")
        .or_else(|| created.get("sessionId"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("session/create 未返回 sessionId：{created}"))?
        .to_string();
    tracing::info!("ZCode session/create: {}", brief(&created.to_string(), 600));
    let subscribed = rpc
        .request(
            "session/subscribe",
            json!({"sessionId": session_id, "deliveryKind": "desktop-continuous", "afterSeq": 0, "includeSnapshot": true}),
            RPC_TIMEOUT,
        )
        .await?;
    tracing::info!(
        "ZCode session/subscribe: {}",
        brief(&subscribed.to_string(), 600)
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut provider_candidates = vec![
        provider.clone(),
        "account:zai-start-plan".to_string(),
        "account:bigmodel-start-plan".to_string(),
        "zai-start-plan".to_string(),
        "bigmodel-start-plan".to_string(),
    ];
    provider_candidates.dedup();
    let mut model_candidates = vec![
        model.clone(),
        "GLM-5.3".to_string(),
        "GLM-5.3-Flash".to_string(),
        "GLM-5.2".to_string(),
        "GLM-5-Turbo".to_string(),
    ];
    model_candidates.dedup();
    let mut set_error: Option<String> = None;
    let mut selected: Option<(String, String)> = None;
    'outer: for candidate in provider_candidates {
        for candidate_model in &model_candidates {
            let attempt = rpc
                .request(
                    "session/setModel",
                    json!({
                        "sessionId": session_id,
                        "model": {
                            "providerId": candidate,
                            "modelId": candidate_model,
                            "options": {"reasoningLevel": "max"},
                        }
                    }),
                    RPC_TIMEOUT,
                )
                .await;
            match attempt {
                Ok(_) => {
                    selected = Some((candidate, candidate_model.clone()));
                    break 'outer;
                }
                Err(err) => set_error = Some(err),
            }
        }
    }
    let (selected_provider, selected_model) = match selected {
        Some((provider, model)) => {
            tracing::info!("ZCode setModel: provider={} model={}", provider, model);
            (provider, model)
        }
        None => {
            tracing::warn!(
                "ZCode setModel 未命中，使用 session/send.modelSelection 兜底：{}",
                brief(&set_error.unwrap_or_default(), 160)
            );
            (provider.clone(), model.clone())
        }
    };
    tokio::time::sleep(Duration::from_millis(800)).await;
    rpc.request(
        "session/send",
        json!({
            "sessionId": session_id,
            "content": prompt,
            "modelSelection": {
                "providerId": selected_provider,
                "modelId": selected_model,
                "options": {"reasoningLevel": "max"},
            }
        }),
        RPC_TIMEOUT,
    )
    .await?;
    rpc.wait_turn(TURN_TIMEOUT).await?;
    Ok(AppServerResponse {
        text: rpc.text.clone(),
        usage: rpc.usage.clone(),
    })
}

/// session/create 的 workspace 路径：使用账号隔离 home 的绝对路径。
fn rpc_home(rpc: &Rpc) -> String {
    rpc.home.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_uses_account_provider() {
        let mut acc = ZcodeAccount::new("t", "bigmodel", "jwt", "a.b.c").unwrap();
        assert_eq!(provider_id(&acc), "account:bigmodel-start-plan");
        acc.provider = "zai".into();
        assert_eq!(provider_id(&acc), "account:zai-start-plan");
    }

    #[test]
    fn select_model_falls_back_to_available() {
        let available = vec!["GLM-5.3-Flash".to_string()];
        assert_eq!(select_model("GLM-5.3", &available), "GLM-5.3-Flash");
        assert_eq!(select_model("GLM-5.3-Flash", &available), "GLM-5.3-Flash");
        assert_eq!(select_model("", &available), "GLM-5.3-Flash");
    }

    #[test]
    fn builtin_revision_is_stable_shape() {
        let path = std::env::temp_dir().join("mg-zcode-revision-test.json");
        std::fs::write(&path, r#"{"revision":"abc"}"#).unwrap();
        let value = builtin_revision(&path);
        let _ = std::fs::remove_file(&path);
        assert!(value.starts_with("zcode-builtin:abc:"));
    }
}
