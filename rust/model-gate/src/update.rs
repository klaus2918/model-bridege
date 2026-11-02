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
use crate::update_paths;

/// 仓库网页地址（非 API）。
const REPO_URL: &str = "https://github.com/klaus2918/model-bridege";
/// `releases/latest` 会 302 到最新正式版的 tag 页，不占用 REST API 配额。
const LATEST_RELEASE_URL: &str = "https://github.com/klaus2918/model-bridege/releases/latest";
/// Atom feed 是 GitHub 公开的 Release 列表，不占用 REST API 配额。
const RELEASES_ATOM_URL: &str = "https://github.com/klaus2918/model-bridege/releases.atom";
const AUTO_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 1024 * 1024;
const MAX_FEED_BYTES: u64 = 2 * 1024 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const TEMP_PREFIX: &str = "model-gate-update-";

/// 启动安装器后，本进程延迟多久才真正退出。
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
/// 现在的做法（见 `crate::lifecycle`）：
///
/// - 安装参数带 `/NOCLOSEAPPLICATIONS`，彻底不让 Inno 走 Restart Manager；
/// - 本进程收到关闭请求后按序做三件事：停 HTTP 服务（释放端口）→
///   数据库 WAL 落盘 → 退出进程（释放 exe 句柄）；
/// - 由独立的看护脚本等本进程退出后再执行安装，装完拉起新版本。
///
/// 因此这里不再需要「固定延时退出」，只保留一个很短的延时，
/// 让这次 HTTP 响应先写回浏览器。
const INSTALL_EXIT_DELAY_MS: u64 = 300;

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
fn build_client_with(redirect: reqwest::redirect::Policy) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(redirect)
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

/// 下载用客户端：跟随 GitHub → CDN 的 302 跳转。
fn build_client() -> reqwest::Client {
    build_client_with(reqwest::redirect::Policy::limited(5))
}

/// 检查更新用客户端：不自动跟随 302，直接读 `Location` 里的 tag。
///
/// 这样只需一个响应头就能拿到最新版本，不必下载整个 tag 页面，
/// 也不会触发 `api.github.com` 的匿名配额。
fn build_check_client() -> reqwest::Client {
    build_client_with(reqwest::redirect::Policy::none())
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

/// Atom feed 中的一条 Release 记录。
///
/// Atom 是 GitHub 公开的订阅源，不占用 REST API 配额；它提供 tag、
/// 更新时间和 Release 说明（HTML 转义后的内容），足以驱动检查更新。
#[derive(Clone, Debug)]
struct AtomEntry {
    tag: String,
    notes: String,
    published_at: Option<String>,
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
    /// 已下载安装包对应的 tag。
    ///
    /// 没有它就无法判断「重新检查后这个包还算不算最新」：检查成功会把
    /// phase 改回 `Available`，面板于是既不给下载按钮（`installer_ready`）
    /// 也不给安装按钮（phase 不是 `Ready`），用户就会卡在「下载完了装不上」。
    installer_tag: Option<String>,
    /// 一次性标记：本次下载完成后自动触发安装（「下载并安装」按钮）。
    /// 与持久化的 `update.auto_install` 设置无关，只影响下一次下载。
    install_after_download: bool,
}

pub struct UpdateService {
    current: Version,
    client: reqwest::Client,
    /// 检查更新专用：不跟随 302，只读 `Location`。
    check_client: reqwest::Client,
    state: Mutex<UpdateState>,
    db: Arc<Db>,
    install_dir: PathBuf,
    /// 当前运行的 exe 路径（取不到时为 None）。
    exe_path: Option<PathBuf>,
    /// Inno 卸载项记录的安装目录（取不到时为 None）。
    ///
    /// 已安装状态下 Inno 会忽略 `/DIR=`，改用这个值——它才是「实际装到哪」的答案。
    registered_install_dir: Option<PathBuf>,
    /// 关闭协调器：安装时用它让本进程按序退出。
    shutdown: Arc<crate::lifecycle::Shutdown>,
}

impl UpdateService {
    pub fn new(
        current: Version,
        db: Arc<Db>,
        install_dir: PathBuf,
        shutdown: Arc<crate::lifecycle::Shutdown>,
    ) -> Self {
        cleanup_stale_temp();
        let client = build_client();
        let check_client = build_check_client();
        let exe_path = std::env::current_exe().ok();
        let registered_install_dir = update_paths::registered_install_dir();
        Self {
            current,
            client,
            check_client,
            state: Mutex::new(UpdateState::default()),
            db,
            install_dir,
            exe_path,
            registered_install_dir,
            shutdown,
        }
    }

    /// 生效安装目录：注册表记录优先，回退当前 exe 所在目录。
    fn effective_install_dir(&self) -> PathBuf {
        update_paths::effective_install_dir_with(
            self.registered_install_dir.as_deref(),
            &self.install_dir,
        )
    }

    /// 生效安装目标：生效安装目录下的 `Model-Gate.exe`（升级时被覆盖的那个文件）。
    fn effective_install_target(&self) -> PathBuf {
        update_paths::effective_install_target_with(
            self.registered_install_dir.as_deref(),
            &self.install_dir,
        )
    }

    /// 生效下载目录：`update.download_dir` 设置优先，缺失/空值用默认目录
    /// （Windows 的「下载」文件夹）。
    fn download_dir(&self) -> PathBuf {
        let custom = self.db.get_setting(update_paths::DOWNLOAD_DIR_SETTING);
        update_paths::resolve_download_dir(custom.as_deref())
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
                let same_tag = state.installer_tag.as_deref() == Some(release.tag.as_str());
                // 已下载的安装包属于更早的版本时作废：否则它会一直占着
                // `installer_ready`，新版本既不能下载也不能安装。
                if state.installer_tag.is_some() && !same_tag {
                    state.installer_path = None;
                    state.installer_sha256 = None;
                    state.installer_size = None;
                    state.installer_tag = None;
                }
                // 安装包就是最新版时保持 Ready：重新检查不能把「装得上」
                // 冲成「可下载」，否则面板只剩「关闭」，用户卡在下载完装不上。
                let ready = newer && same_tag;
                state.latest = Some(release);
                state.phase = if ready {
                    Phase::Ready
                } else if newer {
                    Phase::Available
                } else {
                    Phase::Idle
                };
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
        let (setup, sums, tag) = {
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
            (setup, sums, release.tag.clone())
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
            match service.run_download(setup, sums, tag).await {
                Ok(()) => {
                    // 「下载并安装」一次性标记 / 「自动安装」设置：
                    // 下载校验通过后直接走静默安装（便携版无法自覆盖，只记日志）。
                    let one_shot = {
                        let mut st = service.state.lock().await;
                        std::mem::take(&mut st.install_after_download)
                    };
                    if one_shot || service.auto_install_enabled() {
                        if service.install_supported() {
                            match service.install().await {
                                Ok(_) => tracing::info!("下载完成后已触发安装"),
                                Err(err) => {
                                    tracing::warn!("下载后自动安装失败：{}", err.message)
                                }
                            }
                        } else {
                            tracing::info!("当前为便携版：安装包已就绪，请在面板里启动安装");
                        }
                    }
                }
                Err(err) => {
                    let mut state = service.state.lock().await;
                    state.phase = Phase::Error;
                    state.error = Some(err);
                }
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
            // 便携版：没有 Inno 卸载器，无法静默自覆盖；但可以直接把下载好的
            // 安装包拉起来——Inno 安装器自带界面与「关闭应用」提示，会引导
            // 用户完成安装并替换当前 exe。哈希照常校验，防止文件被替换。
            if !path.exists() {
                return Err(UpdateError::install_failed("安装包已被删除，请重新下载"));
            }
            let actual = hash_file(&path).await?;
            if actual != expected_sha {
                return Err(UpdateError::checksum_mismatch());
            }
            update_paths::open_in_explorer(&path, false).map_err(UpdateError::install_failed)?;
            let mut state = self.state.lock().await;
            state.error = None;
            let mut status = self.status_json(&state);
            status["launched"] = json!(true);
            return Ok(status);
        }
        if !path.exists() {
            return Err(UpdateError::install_failed("安装包已被删除，请重新下载"));
        }
        let actual = hash_file(&path).await?;
        if actual != expected_sha {
            return Err(UpdateError::checksum_mismatch());
        }

        // 安装目录 = **原安装目录**：注册表卸载项记录的安装位置优先，
        // 读不到才回退当前 exe 所在目录。安装参数里显式带上 `/DIR=`，
        // 保证「面板显示的安装位置」与「安装器实际写入的位置」完全一致，
        // 不再出现每次都要人工选目录的情况。
        let install_dir = self.effective_install_dir();
        let new_exe = install_dir.join(update_paths::INNO_EXE_NAME);
        let log_file = path
            .parent()
            .map(|dir| dir.join("model-gate-install.log"))
            .unwrap_or_else(|| std::env::temp_dir().join("model-gate-install.log"));

        // 看护脚本接管后续三步：等本进程退出 → 静默安装 → 拉起新版本。
        //
        // 关键：安装参数带 `/NOCLOSEAPPLICATIONS`，不让 Inno 用 Restart Manager
        // 去「温柔关闭」本应用——托盘程序不响应 `WM_CLOSE`，那一步会一直卡住。
        // 关闭改由本进程自己做（停服务释放端口 → 落盘数据库 → 退出释放 exe 句柄）。
        let plan = crate::lifecycle::InstallPlan {
            installer: path.clone(),
            install_dir: install_dir.clone(),
            new_exe,
            log_file,
            parent_pid: std::process::id(),
            exit_marker: crate::lifecycle::exit_marker_path(std::process::id()),
        };
        crate::lifecycle::spawn_watch_script(&plan)
            .map_err(|err| UpdateError::install_failed(format!("安装程序启动失败：{err}")))?;
        // 登记退出信号文件：退出前由主流程写出，看护脚本据此开始安装。
        self.shutdown.set_exit_marker(plan.exit_marker.clone());

        {
            let mut state = self.state.lock().await;
            state.phase = Phase::Installing;
            state.error = None;
        }
        let status = {
            let state = self.state.lock().await;
            self.status_json(&state)
        };

        // 触发关闭：HTTP 服务优雅停止（端口释放）→ 托盘退出 → 进程退出。
        //
        // 延迟一小段再触发：让这次 HTTP 响应先写回浏览器，
        // 否则面板会看到「连接被重置」而不是明确的「正在关闭并安装」。
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(INSTALL_EXIT_DELAY_MS)).await;
            tracing::info!("安装已交给看护脚本，开始关闭应用");
            shutdown.request();
        });

        Ok(status)
    }

    async fn run_download(&self, setup: Asset, sums: Asset, tag: String) -> Result<(), UpdateError> {
        let sums_text = self.fetch_text(&sums.url, MAX_SUMS_BYTES).await?;
        let expected_sha = parse_sha256sums(&sums_text, &setup.name)?;

        // 下载目录可配置（默认 Windows 的「下载」文件夹）：
        // 用户要能自己找到安装包，随机临时目录做不到这一点。
        let dir = self.download_dir();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| UpdateError::download_failed(format!("创建下载目录失败：{err}")))?;
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
        state.installer_tag = Some(tag);
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
        // 两条请求都走 github.com 网页/CDN，不占 REST API 配额：
        // 1) releases/latest 的 302 Location 给出最新正式版 tag；
        // 2) releases.atom 给出更新说明与发布时间。
        let (tag_result, feed_result) =
            tokio::join!(self.fetch_latest_tag(), self.fetch_atom_feed());
        let feed = feed_result.unwrap_or_default();

        let tag = match tag_result {
            Ok(tag) => tag,
            // 重定向失败时退回 Atom 的第一条：Atom 本身也是公开源。
            Err(err) => match feed.first() {
                Some(entry) => entry.tag.clone(),
                None => return Err(err),
            },
        };

        let version_text = tag
            .strip_prefix('v')
            .or_else(|| tag.strip_prefix('V'))
            .unwrap_or(&tag);
        let version = Version::parse(version_text)
            .map_err(|err| UpdateError::bad_release(format!("版本号无效：{err}")))?;

        let entry = feed
            .iter()
            .find(|entry| entry.tag == tag)
            .or_else(|| feed.first());
        let notes = entry.map(|entry| entry.notes.clone()).unwrap_or_default();
        let published_at = entry.and_then(|entry| entry.published_at.clone());

        // 资产地址是 GitHub Release 的固定路径，不需要 API 列举；
        // 用 HEAD 确认文件存在并顺便拿到大小（失败时保留链接，让下载阶段报真实错误）。
        let setup_name = format!("Model-Gate-{tag}-rust-setup.exe");
        let sums_name = "SHA256SUMS".to_string();
        let (setup, sums) = tokio::join!(
            self.probe_asset(setup_name.clone(), release_asset_url(&tag, &setup_name)),
            self.probe_asset(sums_name.clone(), release_asset_url(&tag, &sums_name)),
        );

        Ok(ReleaseInfo {
            version,
            tag: tag.clone(),
            notes,
            release_url: format!("{REPO_URL}/releases/tag/{tag}"),
            published_at,
            setup,
            sums,
        })
    }

    /// 读取 `releases/latest` 的 302 `Location`，从中取出最新 tag。
    async fn fetch_latest_tag(&self) -> Result<String, UpdateError> {
        let response = self
            .check_client
            .get(LATEST_RELEASE_URL)
            .header("Accept", "text/html")
            .header("User-Agent", format!("Model-Gate/{}", self.current))
            .timeout(CHECK_TIMEOUT)
            .send()
            .await
            .map_err(|err| classify_send_error(&err))?;

        let status = response.status();
        if status.is_redirection() {
            if let Some(tag) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(tag_from_location)
            {
                return Ok(tag);
            }
        }
        if matches!(status.as_u16(), 403 | 429) {
            return Err(UpdateError::rate_limited());
        }
        Err(UpdateError::network(format!(
            "GitHub 返回 HTTP {}",
            status.as_u16()
        )))
    }

    /// 拉取并解析公开的 Release Atom feed。
    async fn fetch_atom_feed(&self) -> Result<Vec<AtomEntry>, UpdateError> {
        let response = self
            .client
            .get(RELEASES_ATOM_URL)
            .header("Accept", "application/atom+xml")
            .header("User-Agent", format!("Model-Gate/{}", self.current))
            .timeout(CHECK_TIMEOUT)
            .send()
            .await
            .map_err(|err| classify_send_error(&err))?;

        let status = response.status().as_u16();
        if matches!(status, 403 | 429) {
            return Err(UpdateError::rate_limited());
        }
        if !response.status().is_success() {
            return Err(UpdateError::network(format!("GitHub 返回 HTTP {status}")));
        }
        if response
            .content_length()
            .map(|size| size > MAX_FEED_BYTES)
            .unwrap_or(false)
        {
            return Err(UpdateError::bad_release("Release 列表超过大小上限"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| UpdateError::network(format!("读取 Release 列表失败：{err}")))?;
        if bytes.len() as u64 > MAX_FEED_BYTES {
            return Err(UpdateError::bad_release("Release 列表超过大小上限"));
        }
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|_| UpdateError::bad_release("Release 列表不是 UTF-8 文本"))?;
        Ok(parse_atom_feed(&text))
    }

    /// 用 HEAD 确认 Release 资产是否存在；成功时返回大小。
    ///
    /// 404/410 视为该资产缺失；网络抖动或 5xx 不做否定判断，
    /// 保留资产链接，避免因为一次探测失败而误报“没有安装包”。
    async fn probe_asset(&self, name: String, url: String) -> Option<Asset> {
        let response = self.client.head(&url).timeout(CHECK_TIMEOUT).send().await;
        match response {
            Ok(response) if response.status().is_success() => Some(Asset {
                name,
                // HEAD 响应里 reqwest 的 `content_length()` 可能返回 0，
                // 直接读头才能拿到真实大小。
                size: response
                    .headers()
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0),
                url,
            }),
            Ok(response) if matches!(response.status().as_u16(), 404 | 410) => None,
            _ => Some(Asset { name, size: 0, url }),
        }
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
        // 下载目录可被用户指定；面板要能区分「自定义」与「默认」。
        let download_dir = self.download_dir();
        let download_dir_custom = download_dir != update_paths::download_dir();
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
            // 便捷字段：面板用它区分「安装版可静默升级」与「便携版需手动跑安装包」
            "portable": !self.install_supported(),
            "auto_download": self.auto_download_enabled(),
            "auto_install": self.auto_install_enabled(),
            "installer_ready": state.installer_path.is_some(),
            "installer_size": state.installer_size,
            "installer_tag": state.installer_tag.clone(),
            // ── 路径事实：让用户看得见「下到哪」「装到哪」 ──
            "installer_path": state.installer_path.as_ref().map(|path| path.display().to_string()),
            "installer_dir": state
                .installer_path
                .as_ref()
                .and_then(|path| path.parent())
                .map(|dir| dir.display().to_string()),
            "download_dir": download_dir.display().to_string(),
            "download_dir_custom": download_dir_custom,
            "exe_path": self.exe_path.as_ref().map(|path| path.display().to_string()),
            "install_dir": self.install_dir.display().to_string(),
            "registered_install_dir": self
                .registered_install_dir
                .as_ref()
                .map(|dir| dir.display().to_string()),
            "effective_install_dir": self.effective_install_dir().display().to_string(),
            // 安装目录的来源：注册表（= 原安装目录）还是当前 exe 所在目录。
            "install_dir_source": if self.registered_install_dir.is_some() {
                "registry"
            } else {
                "exe"
            },
            "install_target": self.effective_install_target().display().to_string(),
            "install_dir_matches_exe": self.effective_install_dir() == self.install_dir,
            // 关闭请求已发出：面板据此提示「正在关闭并安装」。
            "restart_pending": self.shutdown.is_requested(),
            "error": state.error.as_ref().map(|error| json!({
                "code": error.code.clone(),
                "message": error.message.clone(),
            })),
        })
    }

    /// 是否支持自动安装：安装目录里得有 Inno 的卸载器（unins*.exe）。
    ///
    /// 两个目录都查：注册表记录的安装目录，以及当前 exe 所在目录
    /// （便携版升级、目录被手工搬过等情况下两者会不一致）。
    fn install_supported(&self) -> bool {
        find_uninstaller(&self.effective_install_dir()).is_some()
            || find_uninstaller(&self.install_dir).is_some()
    }

    fn last_check_at(&self) -> Option<i64> {
        self.db
            .get_setting("update.last_check_at")
            .and_then(|value| value.parse::<i64>().ok())
    }

    /// 数据库布尔设置（"1"/"0"）；缺省按关闭处理。
    fn bool_setting(&self, key: &str) -> bool {
        self.db
            .get_setting(key)
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    /// 「发现新版后自动在后台下载」设置（仍不自动安装）。
    pub fn auto_download_enabled(&self) -> bool {
        self.bool_setting(AUTO_DOWNLOAD_SETTING)
    }

    /// 「下载完成后自动静默安装」设置（仅安装版生效；便携版仍需手动启动安装包）。
    pub fn auto_install_enabled(&self) -> bool {
        self.bool_setting(AUTO_INSTALL_SETTING)
    }
}

/// 持久化在数据库设置里的更新行为开关。
const AUTO_DOWNLOAD_SETTING: &str = "update.auto_download";
const AUTO_INSTALL_SETTING: &str = "update.auto_install";

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
                // 「自动下载新版本」开关（面板更新弹窗里可改）：发现新版后
                // 直接在后台下载并校验；安装仍需用户在面板里点一次（或开自动安装）。
                if service.auto_download_enabled() {
                    match service.download().await {
                        Ok(_) => tracing::info!("已按设置自动下载新版本"),
                        Err(err) => tracing::warn!("自动下载失败：{}", err.message),
                    }
                }
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

/// 「下载并安装」：一次性设置 install_after_download 标记后立即开始下载。
/// 下载校验通过后自动走静默安装（安装版）；便携版下载完成后在面板里手动启动。
pub async fn api_update_download_install(State(state): State<Arc<AppState>>) -> Response {
    {
        let mut st = state.update.state.lock().await;
        st.install_after_download = true;
    }
    match state.update.download().await {
        Ok(status) => Json(status).into_response(),
        Err(err) => error_response(err),
    }
}

/// 更新行为设置（保存在数据库设置，即时生效）：
/// `{ "autoDownload": bool, "autoInstall": bool }`，字段可省略 = 不修改。
pub async fn api_update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(flag) = body.get("autoDownload").and_then(Value::as_bool) {
        state
            .update
            .db
            .set_setting(AUTO_DOWNLOAD_SETTING, if flag { "1" } else { "0" });
    }
    if let Some(flag) = body.get("autoInstall").and_then(Value::as_bool) {
        state
            .update
            .db
            .set_setting(AUTO_INSTALL_SETTING, if flag { "1" } else { "0" });
    }
    Json(state.update.status().await).into_response()
}

/// 「打开位置」支持的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenLocationTarget {
    /// 下载目录（已下载时定位并选中安装包）。
    Download,
    /// 生效安装目录。
    Install,
}

/// 解析「打开位置」的目标枚举。
///
/// **只接受 `download` / `install` 两种固定取值**，其余一律拒绝——
/// 这个接口绝不能变成任意路径打开器。
fn parse_open_location_target(target: &str) -> Option<OpenLocationTarget> {
    match target.trim() {
        "download" => Some(OpenLocationTarget::Download),
        "install" => Some(OpenLocationTarget::Install),
        _ => None,
    }
}

/// 打开更新相关位置（下载目录 / 生效安装目录）。
///
/// 安全约定：**只接受 `download` / `install` 两种枚举**，路径一律由服务端推导，
/// 不接受调用方传入的任意路径——否则这个接口会变成任意文件打开器。
pub async fn api_update_open_location(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(target) = parse_open_location_target(raw) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "不支持的位置类型（只允许 download / install）",
            "bad_request",
            "update_open_location_target",
        );
    };
    let update = state.update.clone();
    let (path, select) = match target {
        OpenLocationTarget::Download => {
            let state = update.state.lock().await;
            match state.installer_path.clone() {
                // 已下载：在资源管理器里定位并选中安装包文件。
                Some(installer) => (installer, true),
                // 未下载：打开下载目录本身（目录可能还不存在，先建出来）。
                None => {
                    let dir = update.download_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    (dir, false)
                }
            }
        }
        OpenLocationTarget::Install => (update.effective_install_dir(), false),
    };
    match update_paths::open_in_explorer(&path, select) {
        Ok(()) => Json(json!({
            "ok": true,
            "target": raw.trim(),
            "path": path.display().to_string(),
            "selected": select,
        }))
        .into_response(),
        Err(err) => error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            err,
            "update_open_location_failed",
            "update_open_location_failed",
        ),
    }
}

/// 保存下载目录设置。请求体 `{ "dir": "<绝对路径>" }`；空字符串表示恢复默认。
///
/// 只接受校验通过的目录（绝对路径、可创建、可写），其余一律拒绝并给出中文原因。
pub async fn api_update_download_dir(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body
        .get("dir")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if raw.is_empty() {
        // 空值 = 恢复默认：清掉设置（读回空串时按默认目录处理）。
        state
            .update
            .db
            .set_setting(update_paths::DOWNLOAD_DIR_SETTING, "");
    } else {
        match update_paths::validate_download_dir(&raw) {
            Ok(dir) => {
                state.update.db.set_setting(
                    update_paths::DOWNLOAD_DIR_SETTING,
                    &dir.display().to_string(),
                );
            }
            Err(message) => {
                return error_json(
                    StatusCode::BAD_REQUEST,
                    message,
                    "bad_request",
                    "update_download_dir_invalid",
                )
            }
        }
    }
    Json(state.update.status().await).into_response()
}

fn error_response(err: UpdateError) -> Response {
    let status = StatusCode::from_u16(err.http_status).unwrap_or(StatusCode::BAD_GATEWAY);
    error_json(status, err.message, "upstream_error", &err.code)
}

/// Release 资产的固定下载地址（不经过 API）。
fn release_asset_url(tag: &str, name: &str) -> String {
    format!("{REPO_URL}/releases/download/{tag}/{name}")
}

/// 从 `releases/tag/<tag>` 形式的 URL / Location 中取出 tag。
fn tag_from_location(location: &str) -> Option<String> {
    let (_, tail) = location.split_once("/releases/tag/")?;
    let tag = tail
        .split(|ch| ch == '?' || ch == '#')
        .next()?
        .trim_end_matches('/');
    if tag.is_empty() {
        return None;
    }
    percent_encoding::percent_decode_str(tag)
        .decode_utf8()
        .ok()
        .map(|value| value.into_owned())
}

/// 解析 Atom feed，按出现顺序返回各条 Release。
fn parse_atom_feed(xml: &str) -> Vec<AtomEntry> {
    let mut entries = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<entry>") {
        let after = &rest[start + "<entry>".len()..];
        let Some(end) = after.find("</entry>") else {
            break;
        };
        if let Some(entry) = parse_atom_entry(&after[..end]) {
            entries.push(entry);
        }
        rest = &after[end + "</entry>".len()..];
    }
    entries
}

fn parse_atom_entry(block: &str) -> Option<AtomEntry> {
    let href = link_href(block)?;
    let tag = tag_from_location(&href)?;
    let notes = element_text(block, "content")
        .map(|html| html_to_text(&unescape_xml(html)))
        .unwrap_or_default();
    let published_at = element_text(block, "updated")
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    Some(AtomEntry {
        tag,
        notes,
        published_at,
    })
}

fn link_href(block: &str) -> Option<String> {
    let mut rest = block;
    while let Some(start) = rest.find("<link") {
        let after = &rest[start..];
        let end = after.find('>')?;
        let tag_text = &after[..end];
        if tag_text.contains("rel=\"alternate\"") {
            if let Some(href) = attr_value(tag_text, "href") {
                return Some(href);
            }
        }
        rest = &after[end + 1..];
    }
    None
}

fn attr_value(tag_text: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag_text.find(&needle)? + needle.len();
    let rest = &tag_text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn element_text<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let start = block.find(&open)?;
    let after_open = &block[start..];
    let content_start = after_open.find('>')? + 1;
    let rest = &after_open[content_start..];
    let close = format!("</{name}>");
    let end = rest.find(&close)?;
    Some(&rest[..end])
}

/// 反转义 XML 文本节点中的实体。
fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// 把 Atom 里的 HTML 说明转成面板 `<pre>` 适合展示的纯文本。
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();

    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let name = tag
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('/')
                    .to_ascii_lowercase();
                match name.as_str() {
                    "br" => out.push('\n'),
                    "p" | "/p" | "div" | "/div" | "ul" | "/ul" | "ol" | "/ol" | "h1" | "/h1"
                    | "h2" | "/h2" | "h3" | "/h3" | "h4" | "/h4" | "h5" | "/h5" | "h6" | "/h6" => {
                        out.push('\n')
                    }
                    "li" => {
                        out.push('\n');
                        out.push_str("- ");
                    }
                    _ => {}
                }
                tag.clear();
            } else {
                tag.push(ch);
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            tag.clear();
        } else {
            out.push(ch);
        }
    }

    // 合并连续空行，避免 HTML 结构留下大片空白。
    let mut text = String::with_capacity(out.len());
    let mut blank_lines = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text.trim().to_string()
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
    fn extracts_tag_from_release_location() {
        assert_eq!(
            tag_from_location("https://github.com/klaus2918/model-bridege/releases/tag/v2.27.0")
                .as_deref(),
            Some("v2.27.0")
        );
        assert_eq!(
            tag_from_location("https://github.com/klaus2918/model-bridege/releases/tag/v2.27.0/")
                .as_deref(),
            Some("v2.27.0")
        );
        assert_eq!(
            tag_from_location("https://github.com/klaus2918/model-bridege/releases"),
            None
        );
    }

    #[test]
    fn parses_atom_feed_into_release_entries() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <updated>2026-10-30T14:33:16Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/klaus2918/model-bridege/releases/tag/v2.27.0"/>
    <title>Model-Gate v2.27.0 (Rust)</title>
    <content type="html">&lt;p&gt;本次更新&lt;/p&gt;&lt;ul&gt;&lt;li&gt;修复问题&lt;/li&gt;&lt;/ul&gt;</content>
  </entry>
  <entry>
    <updated>2026-10-30T00:28:45Z</updated>
    <link rel="alternate" type="text/html" href="https://github.com/klaus2918/model-bridege/releases/tag/v2.26.0"/>
    <title>Model-Gate v2.26.0 (Rust)</title>
    <content type="html">&lt;p&gt;旧版本&lt;/p&gt;</content>
  </entry>
</feed>"#;

        let entries = parse_atom_feed(xml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tag, "v2.27.0");
        assert_eq!(
            entries[0].published_at.as_deref(),
            Some("2026-10-30T14:33:16Z")
        );
        assert!(entries[0].notes.contains("本次更新"));
        assert!(entries[0].notes.contains("- 修复问题"));
        assert!(!entries[0].notes.contains('<'));
    }

    #[test]
    fn builds_asset_urls_from_tag() {
        let setup = "Model-Gate-v2.27.0-rust-setup.exe";
        assert_eq!(
            release_asset_url("v2.27.0", setup),
            "https://github.com/klaus2918/model-bridege/releases/download/v2.27.0/Model-Gate-v2.27.0-rust-setup.exe"
        );
        assert_eq!(
            release_asset_url("v2.27.0", "SHA256SUMS"),
            "https://github.com/klaus2918/model-bridege/releases/download/v2.27.0/SHA256SUMS"
        );
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
        assert!(client.get(LATEST_RELEASE_URL).build().is_ok());
        let check_client = build_check_client();
        assert!(check_client.get(LATEST_RELEASE_URL).build().is_ok());
    }

    #[test]
    fn open_location_accepts_only_known_targets() {
        assert_eq!(
            parse_open_location_target("download"),
            Some(OpenLocationTarget::Download)
        );
        assert_eq!(
            parse_open_location_target(" install "),
            Some(OpenLocationTarget::Install)
        );
        // 任意路径必须被拒掉：这个接口不能变成任意文件打开器。
        assert_eq!(parse_open_location_target(r"C:\Windows\System32"), None);
        assert_eq!(parse_open_location_target(""), None);
        assert_eq!(parse_open_location_target("file"), None);
    }
}
