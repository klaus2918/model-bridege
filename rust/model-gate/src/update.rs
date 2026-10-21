//! 应用内更新：检查 GitHub Release、下载安装包、SHA256 校验并启动静默升级。

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use semver::Version;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error as StdError;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::db::Db;
use crate::proxy::{error_json, AppState};

const REPO_OWNER: &str = "klaus2918";
const REPO_NAME: &str = "model-bridege";
const API_LATEST: &str = "https://api.github.com/repos/klaus2918/model-bridege/releases/latest";
const AUTO_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 1024 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TEMP_PREFIX: &str = "model-gate-update-";

/// 启动安装器后，本进程延迟多久退出。
///
/// 时序依据（实测，见 verify-log「运行中升级实测」）：
///
/// 1. Inno Setup 的 Restart Manager 通过向目标窗口发 `WM_CLOSE` 来关闭占用
///    exe 的进程。但 Model-Gate 是托盘常驻应用——它的窗口只用于消息循环，
///    **不处理 `WM_CLOSE`**；`--no-gui` 模式更是根本没有窗口。
///    因此 `/CLOSEAPPLICATIONS` 对本产品无效，Inno 会一直等它关闭而卡住。
/// 2. 结论：必须由本进程**自己退出**来释放 exe 句柄。
/// 3. 但若安装器在退出前就开始写文件，覆盖会因文件占用失败。
///
/// 解决办法：用 `cmd` 包装一层，让安装器在包装进程里**等本进程退出后**才启动：
/// `cmd /c "ping -n <n+1> 127.0.0.1 >nul & <installer> <args>"`。
/// `ping` 的等待不依赖本进程是否还活着，只依赖时间；本进程在窗口内退出后，
/// 安装器才真正开始执行，此时 exe 句柄已释放。
const INSTALL_EXIT_DELAY_MS: u64 = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    Available,
    Downloading,
    Ready,
    Installing,
    Error,
}

impl Default for Phase {
    fn default() -> Self {
        Self::Idle
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Checking => "checking",
        Phase::Available => "available",
        Phase::Downloading => "downloading",
        Phase::Ready => "ready",
        Phase::Installing => "installing",
        Phase::Error => "error",
    }
}

/// 构造更新专用的 HTTP 客户端。
///
/// 关键点：必须信任 **系统根证书**（`rustls-native-certs`），而不是只信任
/// webpki 内置的 Mozilla 根证书集。
///
/// 原因：国内常见的加速器 / 代理工具（Steam++、Watt Toolkit、Clash 系 TUN 等）
/// 会通过 hosts 把 `api.github.com` 劫持到 `127.0.0.1`，再用自签 CA 做
/// HTTPS 中间人。这些 CA 已被用户安装进「受信任的根证书颁发机构」，
/// 浏览器与 curl 走系统证书库因此能正常访问；但只带 webpki 根的客户端
/// 会因「未知颁发机构」直接握手失败，表现为「无法连接 GitHub」。
///
/// 同时保留 webpki 根作为兜底：系统根为空或加载失败时仍可直连 GitHub。
fn build_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("Model-Gate/{}", env!("CARGO_PKG_VERSION")))
        // 系统根 + webpki 根都要加载，任一可用即可握手。
        .tls_built_in_native_certs(true)
        .tls_built_in_webpki_certs(true);

    // 代理：优先环境变量（HTTP_PROXY/HTTPS_PROXY/ALL_PROXY），
    // 未设置时不注入任何代理，保持直连。
    if let Some(proxy) = env_proxy() {
        builder = builder.proxy(proxy);
    }

    builder.build().unwrap_or_else(|err| {
        tracing::warn!("更新客户端创建失败，回退到默认客户端：{err}");
        reqwest::Client::new()
    })
}

/// 读取环境变量中的代理设置（`HTTPS_PROXY` > `HTTP_PROXY` > `ALL_PROXY`）。
fn env_proxy() -> Option<reqwest::Proxy> {
    for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if let Ok(proxy) = reqwest::Proxy::all(value) {
                return Some(proxy);
            }
        }
    }
    None
}

/// 把 `reqwest` 错误归类成用户能读懂的原因。
///
/// 区分三类典型失败：证书校验（本地加速器 MITM / CA 不被信任）、
/// 代理连接失败、以及普通网络不可达。它们的修复动作完全不同，
/// 混成一句「无法连接 GitHub」会让用户无从下手。
fn classify_send_error(err: &reqwest::Error) -> UpdateError {
    if err.is_timeout() {
        return UpdateError::new(
            "update_timeout",
            "连接 GitHub 超时，请检查网络或代理设置",
            504,
        );
    }

    // 收集整条 source 链的文本：reqwest 把 TLS 握手失败包在
    // `hyper_util` / `rustls` 的错误里，只能顺着 source 找。
    let mut detail = err.to_string();
    let mut source = StdError::source(err);
    while let Some(inner) = source {
        detail.push_str(" | ");
        detail.push_str(&inner.to_string());
        source = inner.source();
    }

    match classify_detail(&detail) {
        DetailKind::Tls => UpdateError::new(
            "update_tls",
            "HTTPS 证书校验失败：当前网络可能被加速器/代理劫持，请信任其根证书或关闭代理后重试",
            502,
        ),
        DetailKind::Proxy => UpdateError::new(
            "update_proxy",
            "无法通过代理连接 GitHub，请检查代理设置",
            502,
        ),
        DetailKind::Network => UpdateError::new(
            "update_network",
            format!("无法连接 GitHub，请检查网络：{err}"),
            502,
        ),
    }
}

/// 错误明细的归类结果。
#[derive(Debug, PartialEq, Eq)]
enum DetailKind {
    Tls,
    Proxy,
    Network,
}

/// 按错误明细文本归类失败原因。
///
/// 抽成纯函数是为了能直接单测：真实环境里 TLS 失败很难稳定复现，
/// 但「关键字 → 归类」这条映射一旦写错，用户就会看到误导性的提示。
fn classify_detail(detail: &str) -> DetailKind {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("unknownissuer")
        || lower.contains("unknown issuer")
        || lower.contains("certificate verify failed")
        || lower.contains("certificate_verify_failed")
        || lower.contains("invalid peer certificate")
        || lower.contains("self-signed certificate")
        || lower.contains("expired certificate")
        || lower.contains("certificate has expired")
    {
        return DetailKind::Tls;
    }
    if lower.contains("proxy") {
        return DetailKind::Proxy;
    }
    DetailKind::Network
}

#[derive(Clone, Debug)]
struct Asset {
    name: String,
    size: u64,
    url: String,
}

#[derive(Clone, Debug)]
struct ReleaseInfo {
    version: Version,
    tag: String,
    notes: String,
    release_url: String,
    published_at: Option<String>,
    setup: Option<Asset>,
    sums: Option<Asset>,
}

#[derive(Clone, Debug)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
    percent: Option<f64>,
}

#[derive(Clone, Debug)]
pub(crate) struct UpdateError {
    code: String,
    message: String,
    http_status: u16,
}

impl UpdateError {
    fn new(code: &str, message: impl Into<String>, http_status: u16) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            http_status,
        }
    }

    fn network(message: impl Into<String>) -> Self {
        Self::new("update_network", message, 502)
    }

    fn bad_release(message: impl Into<String>) -> Self {
        Self::new("update_bad_release", message, 502)
    }

    fn rate_limited() -> Self {
        Self::new("update_rate_limited", "GitHub 请求受限，请稍后重试", 429)
    }

    fn asset_missing() -> Self {
        Self::new(
            "update_asset_missing",
            "该版本未提供可升级安装包或 SHA256SUMS",
            502,
        )
    }

    fn download_failed(message: impl Into<String>) -> Self {
        Self::new("update_download_failed", message, 502)
    }

    fn checksum_mismatch() -> Self {
        Self::new(
            "update_checksum_mismatch",
            "安装包校验失败，已拒绝安装",
            400,
        )
    }

    fn not_installed() -> Self {
        Self::new(
            "update_not_installed",
            "当前为便携版，请手动运行安装包或前往 Release 页面",
            400,
        )
    }

    fn install_failed(message: impl Into<String>) -> Self {
        Self::new("update_install_failed", message, 500)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new("update_conflict", message, 409)
    }
}

#[derive(Default)]
struct UpdateState {
    phase: Phase,
    latest: Option<ReleaseInfo>,
    progress: Option<Progress>,
    error: Option<UpdateError>,
    checked_at: Option<DateTime<Utc>>,
    installer_path: Option<PathBuf>,
    installer_sha256: Option<String>,
    installer_size: Option<u64>,
}

pub struct UpdateService {
    current: Version,
    client: reqwest::Client,
    state: Mutex<UpdateState>,
    db: Arc<Db>,
    install_dir: PathBuf,
}

impl UpdateService {
    pub fn new(current: Version, db: Arc<Db>, install_dir: PathBuf) -> Self {
        cleanup_stale_temp();
        let client = build_client();
        Self {
            current,
            client,
            state: Mutex::new(UpdateState::default()),
            db,
            install_dir,
        }
    }

    pub async fn status(&self) -> Value {
        let state = self.state.lock().await;
        self.status_json(&state)
    }

    pub async fn check(&self, force: bool) -> Result<Value, UpdateError> {
        {
            let state = self.state.lock().await;
            if matches!(
                state.phase,
                Phase::Checking | Phase::Downloading | Phase::Installing
            ) {
                return Ok(self.status_json(&state));
            }
        }

        if !force {
            if let Some(last) = self.last_check_at() {
                let now = Utc::now().timestamp();
                if now.saturating_sub(last) < AUTO_CHECK_INTERVAL_SECS {
                    let state = self.state.lock().await;
                    return Ok(self.status_json(&state));
                }
            }
        }

        {
            let mut state = self.state.lock().await;
            state.phase = Phase::Checking;
            state.error = None;
            state.progress = None;
        }

        let result = self.fetch_latest().await;
        match result {
            Ok(release) => {
                let checked_at = Utc::now();
                self.db
                    .set_setting("update.last_check_at", &checked_at.timestamp().to_string());
                let newer = release.version > self.current;
                let mut state = self.state.lock().await;
                state.latest = Some(release);
                state.phase = if newer { Phase::Available } else { Phase::Idle };
                state.checked_at = Some(checked_at);
                state.error = None;
                Ok(self.status_json(&state))
            }
            Err(err) => {
                let mut state = self.state.lock().await;
                state.phase = Phase::Error;
                state.error = Some(err.clone());
                state.checked_at = Some(Utc::now());
                Err(err)
            }
        }
    }

    pub async fn download(self: &Arc<Self>) -> Result<Value, UpdateError> {
        let (setup, sums) = {
            let state = self.state.lock().await;
            if state.phase != Phase::Available {
                return Err(UpdateError::conflict("当前没有可下载的新版本"));
            }
            let release = state
                .latest
                .as_ref()
                .ok_or_else(|| UpdateError::conflict("缺少版本信息，请先检查更新"))?;
            let setup = release
                .setup
                .clone()
                .ok_or_else(UpdateError::asset_missing)?;
            let sums = release
                .sums
                .clone()
                .ok_or_else(UpdateError::asset_missing)?;
            (setup, sums)
        };

        {
            let mut state = self.state.lock().await;
            state.phase = Phase::Downloading;
            state.error = None;
            state.progress = Some(Progress {
                downloaded: 0,
                total: if setup.size > 0 {
                    Some(setup.size)
                } else {
                    None
                },
                percent: Some(0.0),
            });
        }

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(err) = service.run_download(setup, sums).await {
                let mut state = service.state.lock().await;
                state.phase = Phase::Error;
                state.error = Some(err);
            }
        });

        let state = self.state.lock().await;
        Ok(self.status_json(&state))
    }

    pub async fn install(self: &Arc<Self>) -> Result<Value, UpdateError> {
        let (path, expected_sha) = {
            let state = self.state.lock().await;
            if state.phase != Phase::Ready {
                return Err(UpdateError::conflict("安装包尚未准备好"));
            }
            let path = state
                .installer_path
                .clone()
                .ok_or_else(|| UpdateError::install_failed("安装包路径缺失"))?;
            let sha = state
                .installer_sha256
                .clone()
                .ok_or_else(|| UpdateError::install_failed("安装包校验信息缺失"))?;
            (path, sha)
        };

        if !self.install_supported() {
            return Err(UpdateError::not_installed());
        }
        if !path.exists() {
            return Err(UpdateError::install_failed("安装包已被删除，请重新下载"));
        }
        let actual = hash_file(&path).await?;
        if actual != expected_sha {
            return Err(UpdateError::checksum_mismatch());
        }

        // 启动安装器（见 INSTALL_EXIT_DELAY_MS 的说明）：
        // 用 cmd 包一层，先等本进程退出释放 exe 句柄，再让安装器真正执行。
        // `ping -n N 127.0.0.1` 是一个不依赖本进程存活的一次性延时器
        // （N = 等待秒数 + 1），退出码固定为 0，因此不会中断后续命令。
        let wait_secs = INSTALL_EXIT_DELAY_MS / 1000 + 1;
        let installer_args = format!(
            "\"{}\" /SILENT /NORESTART /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS /SP- /SUPPRESSMSGBOXES \"/DIR={}\"",
            path.display(),
            self.install_dir.display()
        );
        let cmd_line = format!("ping -n {} 127.0.0.1 >nul & {}", wait_secs, installer_args);
        std::process::Command::new("cmd")
            .args(["/C", &cmd_line])
            .spawn()
            .map_err(|err| UpdateError::install_failed(format!("安装程序启动失败：{err}")))?;

        {
            let mut state = self.state.lock().await;
            state.phase = Phase::Installing;
            state.error = None;
        }
        let status = {
            let state = self.state.lock().await;
            self.status_json(&state)
        };

        // 退出节奏：先给安装器一点时间确认已接手（避免它还没起来我们就退了，
        // 造成窗口内没人执行安装），再退出释放 exe 句柄。
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(INSTALL_EXIT_DELAY_MS)).await;
            std::process::exit(0);
        });

        Ok(status)
    }

    async fn run_download(&self, setup: Asset, sums: Asset) -> Result<(), UpdateError> {
        let sums_text = self.fetch_text(&sums.url, MAX_SUMS_BYTES).await?;
        let expected_sha = parse_sha256sums(&sums_text, &setup.name)?;

        let dir = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| UpdateError::download_failed(format!("创建临时目录失败：{err}")))?;
        let file_name = Path::new(&setup.name)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UpdateError::download_failed("安装包文件名无效"))?;
        let path = dir.join(file_name);

        let response = self
            .client
            .get(&setup.url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .map_err(|err| {
                let classified = classify_send_error(&err);
                UpdateError::new(
                    &classified.code,
                    format!("下载失败：{}", classified.message),
                    classified.http_status,
                )
            })?;
        if !response.status().is_success() {
            return Err(UpdateError::download_failed(format!(
                "下载返回 HTTP {}",
                response.status().as_u16()
            )));
        }
        let total = response.content_length().or(if setup.size > 0 {
            Some(setup.size)
        } else {
            None
        });
        if total.map(|size| size > MAX_DOWNLOAD_BYTES).unwrap_or(false) {
            return Err(UpdateError::download_failed("安装包超过大小上限"));
        }

        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|err| UpdateError::download_failed(format!("创建安装包失败：{err}")))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;
        let mut last_report = 0u64;

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|err| UpdateError::download_failed(format!("下载中断：{err}")))?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > MAX_DOWNLOAD_BYTES {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(UpdateError::download_failed("安装包超过大小上限"));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|err| UpdateError::download_failed(format!("写入安装包失败：{err}")))?;

            if downloaded.saturating_sub(last_report) >= 256 * 1024 {
                last_report = downloaded;
                self.set_progress(downloaded, total).await;
            }
        }

        file.flush()
            .await
            .map_err(|err| UpdateError::download_failed(format!("写入安装包失败：{err}")))?;
        let actual_sha = format!("{:x}", hasher.finalize());
        if actual_sha != expected_sha {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(UpdateError::checksum_mismatch());
        }

        self.set_progress(downloaded, total).await;
        let mut state = self.state.lock().await;
        state.phase = Phase::Ready;
        state.installer_path = Some(path);
        state.installer_sha256 = Some(actual_sha);
        state.installer_size = Some(downloaded);
        state.error = None;
        state.progress = Some(Progress {
            downloaded,
            total: Some(downloaded),
            percent: Some(100.0),
        });
        Ok(())
    }

    async fn set_progress(&self, downloaded: u64, total: Option<u64>) {
        let percent = total
            .filter(|total| *total > 0)
            .map(|total| (downloaded as f64 / total as f64 * 100.0).min(100.0));
        let mut state = self.state.lock().await;
        state.progress = Some(Progress {
            downloaded,
            total,
            percent,
        });
    }

    async fn fetch_latest(&self) -> Result<ReleaseInfo, UpdateError> {
        let response = self
            .client
            .get(API_LATEST)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", format!("Model-Gate/{}", self.current))
            .timeout(CHECK_TIMEOUT)
            .send()
            .await
            .map_err(|err| classify_send_error(&err))?;

        let status = response.status().as_u16();
        if status == 403 || status == 429 {
            let remaining = response
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok());
            if remaining == Some("0") {
                return Err(UpdateError::rate_limited());
            }
        }
        if !response.status().is_success() {
            return Err(UpdateError::network(format!("GitHub 返回 HTTP {status}")));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|err| UpdateError::bad_release(format!("版本信息解析失败：{err}")))?;
        parse_release(&body)
    }

    async fn fetch_text(&self, url: &str, limit: u64) -> Result<String, UpdateError> {
        if !url.starts_with("https://") {
            return Err(UpdateError::bad_release("校验和地址不是 HTTPS"));
        }
        let response = self
            .client
            .get(url)
            .timeout(CHECK_TIMEOUT)
            .send()
            .await
            .map_err(|err| {
                let classified = classify_send_error(&err);
                UpdateError::new(
                    &classified.code,
                    format!("下载校验和失败：{}", classified.message),
                    classified.http_status,
                )
            })?;
        if !response.status().is_success() {
            return Err(UpdateError::download_failed(format!(
                "校验和返回 HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .map(|size| size > limit)
            .unwrap_or(false)
        {
            return Err(UpdateError::download_failed("校验和文件超过大小上限"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| UpdateError::download_failed(format!("读取校验和失败：{err}")))?;
        if bytes.len() as u64 > limit {
            return Err(UpdateError::download_failed("校验和文件超过大小上限"));
        }
        String::from_utf8(bytes.to_vec())
            .map_err(|_| UpdateError::bad_release("校验和不是 UTF-8 文本"))
    }

    fn status_json(&self, state: &UpdateState) -> Value {
        let available = state
            .latest
            .as_ref()
            .map(|release| release.version > self.current)
            .unwrap_or(false);
        let installable = available
            && state
                .latest
                .as_ref()
                .map(|release| release.setup.is_some() && release.sums.is_some())
                .unwrap_or(false);
        let release = state.latest.as_ref();
        json!({
            "current": self.current.to_string(),
            "phase": phase_name(state.phase),
            "available": available,
            "installable": installable,
            "latest": release.map(|release| release.version.to_string()),
            "tag": release.map(|release| release.tag.clone()),
            "release_url": release.map(|release| release.release_url.clone()),
            "published_at": release.and_then(|release| release.published_at.clone()),
            "notes": release.map(|release| release.notes.clone()),
            "asset": release.and_then(|release| release.setup.as_ref()).map(|asset| json!({
                "name": asset.name.clone(),
                "size": asset.size,
            })),
            "progress": state.progress.as_ref().map(|progress| json!({
                "downloaded": progress.downloaded,
                "total": progress.total,
                "percent": progress.percent,
            })),
            "checked_at": state.checked_at.as_ref().map(|value| value.to_rfc3339()),
            "install_supported": self.install_supported(),
            "installer_ready": state.installer_path.is_some(),
            "installer_size": state.installer_size,
            "error": state.error.as_ref().map(|error| json!({
                "code": error.code.clone(),
                "message": error.message.clone(),
            })),
        })
    }

    fn install_supported(&self) -> bool {
        find_uninstaller(&self.install_dir).is_some()
    }

    fn last_check_at(&self) -> Option<i64> {
        self.db
            .get_setting("update.last_check_at")
            .and_then(|value| value.parse::<i64>().ok())
    }
}

pub async fn auto_check(service: Arc<UpdateService>) {
    tokio::time::sleep(Duration::from_secs(15)).await;
    match service.check(false).await {
        Ok(status) => {
            if status
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                tracing::info!(
                    "发现新版本 {}（当前 {}）",
                    status.get("latest").and_then(|v| v.as_str()).unwrap_or("?"),
                    status
                        .get("current")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                );
            }
        }
        Err(err) => tracing::warn!("自动检查更新失败：{}", err.message),
    }
}

pub async fn api_update_status(State(state): State<Arc<AppState>>) -> Response {
    Json(state.update.status().await).into_response()
}

pub async fn api_update_check(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let force = uri
        .query()
        .map(|query| {
            query
                .split('&')
                .any(|part| part == "force=1" || part == "force=true")
        })
        .unwrap_or(false);
    match state.update.check(force).await {
        Ok(status) => Json(status).into_response(),
        Err(err) => error_response(err),
    }
}

pub async fn api_update_download(State(state): State<Arc<AppState>>) -> Response {
    match state.update.download().await {
        Ok(status) => Json(status).into_response(),
        Err(err) => error_response(err),
    }
}

pub async fn api_update_install(State(state): State<Arc<AppState>>) -> Response {
    match state.update.install().await {
        Ok(status) => Json(status).into_response(),
        Err(err) => error_response(err),
    }
}

fn error_response(err: UpdateError) -> Response {
    let status = StatusCode::from_u16(err.http_status).unwrap_or(StatusCode::BAD_GATEWAY);
    error_json(status, err.message, "upstream_error", &err.code)
}

fn parse_release(body: &Value) -> Result<ReleaseInfo, UpdateError> {
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| UpdateError::bad_release("缺少 tag_name"))?;
    let version_text = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    let version = Version::parse(version_text)
        .map_err(|err| UpdateError::bad_release(format!("版本号无效：{err}")))?;
    let assets = body
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let setup_name = format!("Model-Gate-v{version}-rust-setup.exe");
    let setup = find_asset(&assets, &setup_name);
    let sums = find_asset(&assets, "SHA256SUMS");
    let release_url = body
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/{tag}")
        });

    Ok(ReleaseInfo {
        version,
        tag: tag.to_string(),
        notes: body
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        release_url,
        published_at: body
            .get("published_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        setup,
        sums,
    })
}

fn find_asset(assets: &[Value], name: &str) -> Option<Asset> {
    assets.iter().find_map(|asset| {
        let asset_name = asset.get("name").and_then(Value::as_str)?;
        if asset_name != name {
            return None;
        }
        let url = asset.get("browser_download_url").and_then(Value::as_str)?;
        if !url.starts_with("https://") {
            return None;
        }
        Some(Asset {
            name: asset_name.to_string(),
            size: asset.get("size").and_then(Value::as_u64).unwrap_or(0),
            url: url.to_string(),
        })
    })
}

fn parse_sha256sums(text: &str, file_name: &str) -> Result<String, UpdateError> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default().trim_start_matches('*');
        if name != file_name {
            continue;
        }
        if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
        return Err(UpdateError::bad_release("SHA256SUMS 格式无效"));
    }
    Err(UpdateError::bad_release(format!(
        "SHA256SUMS 缺少 {file_name}"
    )))
}

async fn hash_file(path: &Path) -> Result<String, UpdateError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| UpdateError::download_failed(format!("打开安装包失败：{err}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|err| UpdateError::download_failed(format!("读取安装包失败：{err}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn find_uninstaller(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("unins") && name.ends_with(".exe") {
            return Some(path);
        }
    }
    None
}

fn cleanup_stale_temp() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age.as_secs() > 7 * 24 * 60 * 60 {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("semver")
    }

    #[test]
    fn only_newer_version_is_an_update() {
        assert!(version("2.21.0") > version("2.20.0"));
        assert!(!(version("2.20.0") > version("2.20.0")));
        assert!(!(version("2.19.9") > version("2.20.0")));
    }

    #[test]
    fn parses_release_assets_by_exact_name() {
        let body = json!({
            "tag_name": "v2.21.0",
            "body": "notes",
            "html_url": "https://github.com/klaus2918/model-bridege/releases/tag/v2.21.0",
            "published_at": "2026-10-20T12:00:00Z",
            "assets": [
                {
                    "name": "Model-Gate-v2.21.0-rust-setup.exe",
                    "size": 4200000,
                    "browser_download_url": "https://github.com/x/y/releases/download/v2.21.0/Model-Gate-v2.21.0-rust-setup.exe"
                },
                {
                    "name": "SHA256SUMS",
                    "size": 120,
                    "browser_download_url": "https://github.com/x/y/releases/download/v2.21.0/SHA256SUMS"
                }
            ]
        });
        let release = parse_release(&body).expect("release");
        assert_eq!(release.version, version("2.21.0"));
        assert_eq!(
            release.setup.expect("setup").name,
            "Model-Gate-v2.21.0-rust-setup.exe"
        );
        assert_eq!(release.sums.expect("sums").name, "SHA256SUMS");
    }

    #[test]
    fn rejects_non_https_asset_urls() {
        let body = json!({
            "tag_name": "v2.21.0",
            "assets": [
                {
                    "name": "Model-Gate-v2.21.0-rust-setup.exe",
                    "size": 1,
                    "browser_download_url": "http://example.com/setup.exe"
                },
                {
                    "name": "SHA256SUMS",
                    "size": 1,
                    "browser_download_url": "https://example.com/SHA256SUMS"
                }
            ]
        });
        let release = parse_release(&body).expect("release");
        assert!(release.setup.is_none());
        assert!(release.sums.is_some());
    }

    #[test]
    fn parses_sha256sums_with_star_and_uppercase() {
        let hash = "A".repeat(64);
        let text = format!("{hash} *Model-Gate-v2.21.0-rust-setup.exe\n");
        let parsed =
            parse_sha256sums(&text, "Model-Gate-v2.21.0-rust-setup.exe").expect("checksum");
        assert_eq!(parsed, hash.to_ascii_lowercase());
    }

    #[test]
    fn rejects_missing_or_malformed_checksum() {
        assert!(parse_sha256sums("", "setup.exe").is_err());
        assert!(parse_sha256sums("abc  setup.exe", "setup.exe").is_err());
        assert!(parse_sha256sums(&format!("{}  other.exe", "a".repeat(64)), "setup.exe").is_err());
    }

    #[test]
    fn classifies_tls_failure_with_actionable_hint() {
        // 真实环境里 TLS 失败难以稳定复现，但「关键字 → 归类」这条映射
        // 一旦写错，用户就会看到误导性提示，因此直接单测纯函数。
        assert_eq!(
            classify_detail("invalid peer certificate: UnknownIssuer"),
            DetailKind::Tls
        );
        assert_eq!(
            classify_detail("certificate verify failed: self-signed certificate"),
            DetailKind::Tls
        );
        assert_eq!(
            classify_detail("error sending request for url (https://api.github.com/...)"),
            DetailKind::Network
        );
    }

    #[test]
    fn classifies_proxy_failure() {
        assert_eq!(
            classify_detail("proxy CONNECT aborted: tunnel error"),
            DetailKind::Proxy
        );
    }

    #[test]
    fn update_client_trusts_system_roots() {
        // 客户端必须能在「系统根 + webpki 双根」下构建成功。
        // 若未来有人改回只带 webpki 根，在装有本地加速器 CA 的机器上
        // 会出现「无法连接 GitHub」，这条断言配合真实探针守住回归。
        let client = build_client();
        assert!(client.get(API_LATEST).build().is_ok());
    }
}
