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

const STARTUP_WAIT: Duration = Duration::from_secs(8);
const STARTUP_SETTLE: Duration = Duration::from_millis(2500);
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
const TURN_TIMEOUT: Duration = Duration::from_secs(600);
/// Start Plan 账户注入时兜底暴露的模型。旧版 `zcode-builtin.json` 用
/// `providerRules[].builtinModelIds` 声明，新版改由
/// `builtinProviderModelRules`（modelId + providerId）描述；两条路径都要读。
const START_PLAN_MODELS: [&str; 3] = ["GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"];
const START_PLAN_REASONING_LEVEL: &str = "max";

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
    session_id: Option<String>,
    channel: Option<Value>,
}

/// 从 `model.streaming` 事件提取正文增量。
///
/// 只接受 `kind=text_delta`（缺省按正文兼容旧版本）；`reasoning_delta` 是思考过程，
/// 混进 OpenAI 的 `content` 会把思考内容返回给客户端。
fn event_text_delta(payload: &Value) -> Option<&str> {
    if payload.get("done").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("text_delta");
    if kind != "text_delta" {
        return None;
    }
    payload.get("delta").and_then(Value::as_str)
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
        loop {
            let line = tokio::time::timeout(timeout, self.lines.next_line())
                .await
                .map_err(|_| "等待 app-server 消息超时".to_string())?
                .map_err(|err| format!("读取 app-server 失败：{err}"))?;
            let Some(line) = line else {
                return Err(self.error_with_stderr("app-server 输出已关闭".to_string()));
            };
            let line = line.trim();
            if line.is_empty() {
                // JSON-RPC 之间可能夹空行；继续读，不把它当 EOF。
                continue;
            }
            return serde_json::from_str::<Value>(line)
                .map(Some)
                .map_err(|err| format!("app-server 输出不是 JSON：{err}"));
        }
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
            "interaction/requestProviderRuntimeHeaders" => {
                let version = if self.app_version.trim().is_empty() {
                    "0.17.1"
                } else {
                    self.app_version.as_str()
                };
                json!({
                    "headersApplied": true,
                    "requestAuth": {
                        "apiKey": self.token,
                        "headers": {
                            "http-referer": "https://zcode.z.ai",
                            "user-agent": format!("ZCode/{version}"),
                            "x-zcode-app-version": version,
                            "x-title": "Z Code@cli",
                        }
                    }
                })
            }
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
        // 只认当前会话的事件：app-server 可能把启动期/其他会话的事件也推到 stdout。
        if let (Some(expected), Some(actual)) = (
            self.session_id.as_deref(),
            params.get("sessionId").and_then(Value::as_str),
        ) {
            if expected != actual {
                return;
            }
        }
        let payload = params.get("payload").unwrap_or(&Value::Null);
        // GUI 事件把 type 放在 params 顶层；部分遥测事件放在 payload.type。
        let ty = params
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| payload.get("type").and_then(Value::as_str))
            .unwrap_or("");
        match ty {
            "model.streaming" => {
                if let Some(delta) = event_text_delta(payload) {
                    self.text.push_str(delta);
                }
            }
            "session.updated" => {
                if let Some(base_url) = payload.get("baseURL").and_then(Value::as_str) {
                    self.channel = Some(json!({
                        "providerId": payload.get("providerId").cloned().unwrap_or(Value::Null),
                        "modelId": payload.get("modelId").cloned().unwrap_or(Value::Null),
                        "baseURL": base_url,
                    }));
                }
            }
            "turn.completed" => {
                self.usage = payload
                    .get("usage")
                    .cloned()
                    .or_else(|| params.get("usage").cloned());
                // 有的版本不发 model.streaming 的 text_delta，只在 turn.completed
                // 的 response 里给最终文本；只在没有增量时兜底，避免重复。
                if self.text.is_empty() {
                    if let Some(response) = payload.get("response").and_then(Value::as_str) {
                        self.text.push_str(response);
                    }
                }
                self.turn_done = true;
            }
            "turn.failed" => {
                let error = payload
                    .get("error")
                    .cloned()
                    .unwrap_or_else(|| payload.clone());
                let detail = error
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("code").and_then(Value::as_str))
                    .map(str::to_string)
                    .unwrap_or_else(|| error.to_string());
                let attribution = error.get("attribution").cloned().unwrap_or(Value::Null);
                let suffix = if attribution.is_null() {
                    String::new()
                } else {
                    format!("（{attribution}）")
                };
                self.turn_error = Some(format!("turn.failed：{detail}{suffix}"));
                self.turn_done = true;
            }
            _ => {}
        }
    }

    /// 等到 app-server 运行时进入 ready/committing 阶段。
    ///
    /// 参考实现实测：启动后不等就 `session/create` + `session/send`，回合会被静默丢弃
    /// （send 已被接受，但事件流为空）。这里最多等 8s；超时不算失败，继续走后续请求，
    /// 避免旧版本不发 startup 通知时把整次请求判死。
    async fn wait_ready(&mut self, timeout: Duration) -> Result<bool, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            match self.next_message(remaining).await {
                Ok(Some(msg)) => {
                    if msg.get("method").is_some() {
                        if msg.get("id").is_some() {
                            self.handle_server_request(&msg).await?;
                        } else {
                            let params = msg.get("params").unwrap_or(&Value::Null);
                            let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                            let phase = params.get("phase").and_then(Value::as_str).unwrap_or("");
                            if method.starts_with("startup/")
                                && (phase == "ready" || phase == "committing")
                            {
                                return Ok(true);
                            }
                            self.handle_event(params);
                        }
                    }
                }
                Ok(None) => continue,
                Err(err) if err.contains("超时") => return Ok(false),
                Err(err) => return Err(err),
            }
        }
    }

    /// 发送一条通知（没有 id、没有响应）。用于 `session/stop`。
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({"method": method, "params": params}))
            .await
    }

    /// 补发 `session/stop`：app-server 的回合锁在超时/中止后不会自己释放，
    /// 不补这一刀，同一会话的下一轮会被拒 `A prompt is already running`。
    async fn stop_turn(&mut self, reason: &str) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        if let Err(err) = self
            .notify("session/stop", json!({"sessionId": session_id}))
            .await
        {
            tracing::warn!("ZCode 补发 session/stop 失败（{reason}）：{}", brief(&err, 160));
        } else {
            tracing::warn!("ZCode 已补发 session/stop（{reason}）");
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
///
/// 3.17.x 的 `providerRules[]` 不再带 `builtinModelIds`，模型清单改由
/// `builtinProviderModelRules[]`（modelId + providerId）描述；两条路径都读，
/// 保证新旧客户端都能注入正确的 `builtinModelIds`。
fn builtin_models(path: &Path, provider: &str) -> Vec<String> {
    let Some(raw) = read_json(path) else {
        return Vec::new();
    };
    let mut models: Vec<String> = Vec::new();
    if let Some(rule) = builtin_provider_rule(path, provider) {
        if let Some(items) = rule.get("builtinModelIds").and_then(Value::as_array) {
            for item in items {
                if let Some(model) = item.as_str() {
                    push_unique_model(&mut models, model);
                }
            }
        }
    }
    if let Some(items) = raw
        .pointer("/config/modelConfigRules/builtinProviderModelRules")
        .and_then(Value::as_array)
    {
        for item in items {
            if item.get("providerId").and_then(Value::as_str) != Some(provider) {
                continue;
            }
            if let Some(model) = item.get("modelId").and_then(Value::as_str) {
                push_unique_model(&mut models, model);
            }
        }
    }
    models
}

fn push_unique_model(models: &mut Vec<String>, model: &str) {
    if !models.iter().any(|existing| existing == model) {
        models.push(model.to_string());
    }
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
    // registry 合并守卫做逐字符比较：`zcode-builtin:<revision>:<sha256(activeFilePath)>`。
    // 必须按绝对路径计算；`std::path::absolute` 在 Windows 上可能带 `\\?\` 前缀，
    // 需要去掉才能与 Node `path.resolve` 的结果一致。
    hasher.update(normalized_path_for_hash(path).as_bytes());
    format!("zcode-builtin:{revision}:{:x}", hasher.finalize())
}

fn normalized_path_for_hash(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = absolute.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
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
    let account_home = cli.prepare_account_home(account)?;
    // ZCode CLI 以 `ZCODE_DATA_BASE_DIR/.zcode` 为数据根；账号隔离目录必须同时
    // 作为 cwd 和该环境变量，否则 CLI 会读真实 ~/.zcode 的登录态，账号池隔离失效。
    let data_base = account_home
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| account_home.clone());
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
        .current_dir(&data_base)
        .env("ZCODE_SERVICE_AUTHORITY_MODE", "desktop-local")
        .env("ZCODE_APP_VERSION", &version)
        .env("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE", &builtin_path)
        .env("ZCODE_PERSONAL_PROVIDER_CONFIG_FILE", &personal)
        .env("ZCODE_BASE_URL", "https://zcode.z.ai")
        .env("ZCODE_DATA_BASE_DIR", &data_base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：本应用是 GUI 子系统程序，启动 node 控制台子进程时
        // 不创建控制台窗口，避免每次请求弹出 zcode-cli 窗口。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
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
        session_id: None,
        channel: None,
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
    if result.is_err() {
        // 超时 / 中止 / 发送失败 / turn.failed 都要补 stop，避免回合锁泄漏。
        rpc.stop_turn("本轮未正常结束").await;
    }
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
    // 参考实现实测：启动后立刻 create/send 会被静默丢弃。先等 startup ready
    // （最多 8s），再按 GUI 同款的 2.5s 稳定期；旧版本没有 startup 通知时退回
    // 原来的 3s 固定等待，避免误伤。
    let ready = rpc.wait_ready(STARTUP_WAIT).await?;
    if ready {
        tracing::info!("ZCode app-server 运行时就绪");
        tokio::time::sleep(STARTUP_SETTLE).await;
    } else {
        tracing::warn!("ZCode app-server 未上报 startup/ready，按固定稳定期继续");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let provider = provider_id(account);
    let available = builtin_models(builtin_path, &provider);
    let model = select_model(requested_model, &available);
    let models = if available.is_empty() {
        START_PLAN_MODELS.iter().map(|m| m.to_string()).collect()
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
    // 与 GUI 同款：只注入当前账号对应的 Start Plan provider。参考实现确认
    // access 只接受 {type:'zhipu-account', entitled}，states 必须带 current；
    // 多余的候选 provider 会让 registry 合并出现歧义。
    let mut providers = serde_json::Map::new();
    providers.insert(
        provider.clone(),
        json!({
            "access": {"type": "zhipu-account", "entitled": true},
            "builtinModelIds": models.clone(),
        }),
    );
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
    rpc.session_id = Some(session_id.clone());
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
    // 探测时序：create→subscribe、subscribe→send 之间各垫 1.5s；零延迟时
    // 回合会被静默丢弃（send accepted 后事件流为空，参考实现多次复现）。
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut provider_candidates = vec![provider.clone()];
    for fallback in ["account:zai-start-plan", "account:bigmodel-start-plan"] {
        if !provider_candidates.iter().any(|item| item == fallback) {
            provider_candidates.push(fallback.to_string());
        }
    }
    let mut model_candidates = vec![model.clone()];
    for fallback in START_PLAN_MODELS {
        if !model_candidates.iter().any(|item| item == fallback) {
            model_candidates.push(fallback.to_string());
        }
    }
    let mut set_error: Option<String> = None;
    let mut selected: Option<(String, String)> = None;
    // 每次 setModel 失败通常立即返回；限制总尝试次数，避免协议异常时
    // 2 个 provider × 4 个模型逐个等到 RPC 超时。
    let mut attempts_left = 4usize;
    'outer: for candidate in provider_candidates {
        for candidate_model in &model_candidates {
            if attempts_left == 0 {
                break 'outer;
            }
            attempts_left -= 1;
            let attempt = rpc
                .request(
                    "session/setModel",
                    json!({
                        "sessionId": session_id,
                        "model": {
                            "providerId": candidate,
                            "modelId": candidate_model,
                            "options": {"reasoningLevel": START_PLAN_REASONING_LEVEL},
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
    match &selected {
        Some((selected_provider, selected_model)) => {
            tracing::info!(
                "ZCode setModel: provider={} model={}",
                selected_provider,
                selected_model
            );
        }
        None => tracing::warn!(
            "ZCode setModel 未命中，继续使用会话默认模型：{}",
            brief(&set_error.unwrap_or_default(), 160)
        ),
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    // 与参考实现一致：setModel 已把会话切到目标模型，send 只发 sessionId + content。
    // 额外的 modelSelection 不是所有 app-server 版本都接受，可能被 schema 拒绝。
    rpc.request(
        "session/send",
        json!({
            "sessionId": session_id,
            "content": prompt,
        }),
        RPC_TIMEOUT,
    )
    .await?;
    rpc.wait_turn(TURN_TIMEOUT).await?;
    if let Some(channel) = rpc.channel.clone() {
        tracing::info!("ZCode 本轮执行渠道：{}", brief(&channel.to_string(), 240));
    }
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

    #[test]
    fn reasoning_delta_is_not_content() {
        assert_eq!(
            event_text_delta(&json!({"kind":"reasoning_delta","delta":"think"})),
            None
        );
        assert_eq!(
            event_text_delta(&json!({"kind":"text_delta","delta":"hi"})),
            Some("hi")
        );
        assert_eq!(
            event_text_delta(&json!({"kind":"text_delta","delta":"hi","done":true})),
            None
        );
        // 旧版本不带 kind：按正文兼容。
        assert_eq!(
            event_text_delta(&json!({"delta":"legacy"})),
            Some("legacy")
        );
    }

    #[test]
    fn builtin_models_reads_provider_model_rules() {
        let path = std::env::temp_dir().join("mg-zcode-builtin-models.json");
        std::fs::write(
            &path,
            r#"{
              "revision": 43,
              "config": {
                "providerConfigRules": {
                  "providerRules": [{"providerId":"account:zai-start-plan"}]
                },
                "modelConfigRules": {
                  "builtinProviderModelRules": [
                    {"modelId":"GLM-5.3-Flash","providerId":"account:zai-start-plan"},
                    {"modelId":"GLM-5.2","providerId":"account:zai-start-plan"},
                    {"modelId":"GLM-5-Turbo","providerId":"account:zai-start-plan"},
                    {"modelId":"other","providerId":"account:other"}
                  ]
                }
              }
            }"#,
        )
        .unwrap();
        let models = builtin_models(&path, "account:zai-start-plan");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            models,
            vec![
                "GLM-5.3-Flash".to_string(),
                "GLM-5.2".to_string(),
                "GLM-5-Turbo".to_string()
            ]
        );
    }

    #[test]
    fn builtin_revision_uses_normalized_path_hash() {
        let path = std::env::temp_dir().join("mg-zcode-revision-path-test.json");
        std::fs::write(&path, r#"{"revision":43}"#).unwrap();
        let value = builtin_revision(&path);
        let _ = std::fs::remove_file(&path);
        assert!(value.starts_with("zcode-builtin:43:"));
        assert_eq!(value.rsplit(':').next().unwrap().len(), 64);
        assert!(!value.contains(r"\\?\"));
    }
}
