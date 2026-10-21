//! Model-Gate Rust 内核入口：本地代理 + 托盘常驻/系统浏览器面板（可用 --no-gui 纯代理）。
// 始终使用 Windows GUI 子系统（含 debug 版）：任何构建都不弹控制台窗口，日志写入 data/modelgate.log。
#![windows_subsystem = "windows"]
mod anthropic;
mod api_extra;
mod commandcode;
mod commandcode_catalog;
mod config;
mod config_store;
mod db;
mod free_sources;
mod free_sources_api;
mod gui;
mod opencode_transport;
mod provider_profiles;
mod proxy;
mod thinking;
mod ui_assets;
mod update;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::db::Db;
use crate::proxy::AppState;

#[derive(Parser, Debug)]
#[command(name = "model-gate", version, about = "Model-Gate 本地模型代理")]
struct Args {
    /// 配置文件路径（默认同目录 bridge.config.json）
    #[arg(long)]
    config: Option<PathBuf>,
    /// 覆盖监听端口
    #[arg(long)]
    port: Option<u16>,
    /// 覆盖监听地址
    #[arg(long)]
    host: Option<String>,
    /// SQLite 数据库路径（默认与配置文件同目录的 data/modelgate.db）
    #[arg(long)]
    db: Option<PathBuf>,
    /// 纯代理模式：不创建托盘、不打开浏览器
    #[arg(long = "no-gui", default_value_t = false)]
    no_gui: bool,
    /// 配置文件不存在时不自动生成默认配置
    #[arg(long = "no-generate-config", default_value_t = false)]
    no_generate_config: bool,
    /// 跳过单实例守卫（调试/并行对比用）
    #[arg(long = "force-multi", default_value_t = false)]
    force_multi: bool,
    /// 启动后强制打开浏览器面板（覆盖「仅首次自动打开」策略）
    #[arg(long = "open", default_value_t = false)]
    open: bool,
    /// 启动后不自动打开浏览器面板（优先级高于 --open）
    #[arg(long = "no-browser", default_value_t = false)]
    no_browser: bool,
    /// 输出诊断报告到 data/diagnose.txt 后退出（不启动服务/托盘）
    #[arg(long = "diagnose", default_value_t = false)]
    diagnose: bool,
}

fn main() {
    let args = Args::parse();
    let config_path = args.config.clone().unwrap_or_else(default_config_path);
    // 库路径跟随配置文件所在目录（与 Python 冻结版「exe 同目录 data/」同口径）。
    // 为什么不用 CWD 相对：安装版从快捷方式启动时 CWD 可能是系统目录，
    // 会读不到旧库（用量历史看似清零）并在别处新建空库，破坏升级衔接。
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| default_db_path(&config_path));
    let data_dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // 诊断模式：只读检查并写 diagnose.txt；不启动服务/托盘，也不参与单实例判定。
    if args.diagnose {
        match run_diagnose(&config_path, &db_path) {
            Ok(_) => std::process::exit(0),
            Err(err) => {
                gui::show_error(&format!("诊断文件写入失败：{err:#}"));
                std::process::exit(1);
            }
        }
    }

    install_panic_hook(data_dir.clone());
    init_logging(&db_path);
    let no_gui = args.no_gui;
    if let Err(err) = run(args, config_path, db_path) {
        let path = write_fatal_log(&data_dir, &err);
        eprintln!("致命错误：{err:#}");
        tracing::error!("致命错误：{err:#}（详见 {}）", path.display());
        if !no_gui {
            gui::show_error(&format!(
                "Model-Gate 启动失败：{err}\n\n详情见日志：{}",
                path.display()
            ));
        }
        std::process::exit(1);
    }
}

fn run(args: Args, config_path: PathBuf, db_path: PathBuf) -> anyhow::Result<()> {
    if !args.force_multi && !acquire_single_instance(&db_path) {
        tracing::warn!("已有 Model-Gate 实例在运行（同一数据库），尝试打开已有面板");
        activate_existing(&db_path, &args);
        return Ok(());
    }

    if !config_path.exists() && !args.no_generate_config {
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(&config::default_config_value())? + "\n";
        std::fs::write(&config_path, text)?;
        tracing::info!("已生成默认配置 {}", config_path.display());
    }

    let mut cfg = Config::load(&config_path)?;
    if let Some(port) = args.port {
        cfg.port = port;
    }
    if let Some(host) = args.host.clone() {
        cfg.host = host;
    }

    let (listener, actual_port) = bind_with_fallback(&cfg.host, cfg.port)?;
    cfg.port = actual_port;
    let db = Arc::new(Db::open(&db_path)?);
    let first_run = db.get_setting("lastPort").is_none();
    db.set_setting("lastPort", &actual_port.to_string());
    tracing::info!(
        "Model-Gate (rust) v{} 已启动 http://{}:{}/v1  db={}",
        env!("CARGO_PKG_VERSION"),
        cfg.host,
        actual_port,
        db_path.display()
    );
    for up in &cfg.upstreams {
        tracing::info!(
            "  上游 {:<10} {:<10} {}{}",
            up.name,
            up.protocol,
            up.base_url,
            if up.default { "  [默认]" } else { "" }
        );
    }

    let url = format!("http://{}:{}/ui/", cfg.host, actual_port);
    let endpoint = format!("http://{}:{}/v1", cfg.host, actual_port);
    let state = Arc::new(AppState::new(cfg, db, config_path));
    let app = proxy::router(state.clone());
    // 托盘指标快照：后台每 10 秒查库，主线程只读快照，避免阻塞消息循环。
    let today_snapshot = Arc::new(std::sync::Mutex::new(state.db.today_stats()));
    let server = spawn_server(listener, app, state.clone(), today_snapshot.clone());

    if args.no_gui {
        let _ = server.join();
        return Ok(());
    }
    let data_dir = db_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let open_on_start = !args.no_browser && (args.open || first_run);
    let options = gui::ShellOptions {
        url: url.clone(),
        endpoint,
        data_dir,
        open_on_start,
    };
    let tray_state = state.clone();
    let refresh_models = Box::new(move || {
        let Some(handle) = tray_state.runtime.get() else {
            tracing::warn!("刷新模型目录：服务尚未就绪");
            return;
        };
        let state = tray_state.clone();
        handle.spawn(async move {
            let cache = state.refresh_models_cache(true).await;
            tracing::info!("模型目录已刷新（{} 个模型）", cache.ids.len());
        });
    });
    let stats_slot = today_snapshot.clone();
    let today_stats = Box::new(move || {
        stats_slot
            .lock()
            .map(|value| value.clone())
            .unwrap_or(serde_json::Value::Null)
    });
    match gui::run(options, refresh_models, today_stats) {
        Ok(()) => {
            // 托盘「退出」：关闭托盘后直接结束进程
            std::process::exit(0);
        }
        Err(err) => {
            tracing::warn!("桌面壳不可用（{err}），降级为纯代理模式：{url}");
            let _ = server.join();
        }
    }
    Ok(())
}

/// 端口被占用时顺序尝试后续端口（与 Python 版「不中断接入」的降级策略一致）。
fn bind_with_fallback(host: &str, port: u16) -> anyhow::Result<(std::net::TcpListener, u16)> {
    let candidates: Vec<u16> = if port == 0 {
        vec![0]
    } else {
        (port..port.saturating_add(20)).collect()
    };
    for candidate in candidates {
        if let Ok(listener) = std::net::TcpListener::bind((host, candidate)) {
            let actual = listener.local_addr()?.port();
            listener.set_nonblocking(true)?;
            return Ok((listener, actual));
        }
    }
    anyhow::bail!("端口 {port} 起连续 20 个端口均不可用")
}

fn spawn_server(
    listener: std::net::TcpListener,
    app: axum::Router,
    state: Arc<AppState>,
    today_snapshot: Arc<std::sync::Mutex<serde_json::Value>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                tracing::error!("Tokio 运行时初始化失败：{err}");
                return;
            }
        };
        // 托盘菜单需要跨线程触发异步刷新：把运行时句柄登记到共享状态。
        let _ = state.runtime.set(runtime.handle().clone());
        // 今日指标：后台任务查库写快照，托盘定时器只读快照。
        let stats_db = state.db.clone();
        let stats_slot = today_snapshot.clone();
        runtime.spawn(async move {
            loop {
                let db = stats_db.clone();
                let value = tokio::task::spawn_blocking(move || db.today_stats())
                    .await
                    .unwrap_or(serde_json::Value::Null);
                if !value.is_null() {
                    if let Ok(mut slot) = stats_slot.lock() {
                        *slot = value;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });
        let update = state.update.clone();
        runtime.spawn(async move {
            crate::update::auto_check(update).await;
        });
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(err) => {
                    tracing::error!("监听器转换失败：{err}");
                    return;
                }
            };
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await;
        });
    })
}

fn init_logging(db_path: &std::path::Path) {
    let dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    rotate_log_if_needed(&dir);
    let appender = tracing_appender::rolling::never(&dir, "modelgate.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=warn".into());
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .try_init();
    // guard 必须活到进程结束，否则后台日志线程会被提前回收。
    std::mem::forget(guard);
}

/// 单文件日志上限：超过即轮转，避免请求调用日志长期运行后无限增长。
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 轮转保留的备份份数（modelgate.log.1 ~ .3）。
const LOG_KEEP: u32 = 3;

/// 启动时按大小轮转 `modelgate.log`：log → .1 → .2 → .3，超出保留份数的最旧文件丢弃。
fn rotate_log_if_needed(dir: &std::path::Path) {
    let path = dir.join("modelgate.log");
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    if meta.len() <= LOG_MAX_BYTES {
        return;
    }
    for i in (1..LOG_KEEP).rev() {
        let from = dir.join(format!("modelgate.log.{i}"));
        let to = dir.join(format!("modelgate.log.{}", i + 1));
        if from.is_file() {
            let _ = std::fs::remove_file(&to);
            let _ = std::fs::rename(&from, &to);
        }
    }
    let _ = std::fs::remove_file(dir.join("modelgate.log.1"));
    let _ = std::fs::rename(&path, dir.join("modelgate.log.1"));
}

/// 单实例：以数据库路径哈希命名互斥体（与 Python 版「同一数据目录只允许一个实例」一致）。
#[cfg(windows)]
fn acquire_single_instance(db_path: &std::path::Path) -> bool {
    use std::hash::{Hash, Hasher};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    db_path.to_string_lossy().to_lowercase().hash(&mut hasher);
    let name = format!("Local\\ModelGate-{:016x}", hasher.finish());
    let wide: Vec<u16> = std::ffi::OsStr::new(&name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, wide.as_ptr());
        if handle.is_null() {
            return true;
        }
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

#[cfg(not(windows))]
fn acquire_single_instance(_db_path: &std::path::Path) -> bool {
    true
}

/// 二次启动：读取已有实例写入的端口并打开面板；读不到时给出明确提示。
fn activate_existing(db_path: &std::path::Path, args: &Args) {
    if args.no_browser {
        return;
    }
    match read_last_port(db_path) {
        Some(port) => {
            let url = format!("http://127.0.0.1:{port}/ui/");
            if !gui::open_url(&url) {
                tracing::warn!("打开已有面板失败：{url}");
                gui::show_error(&format!("Model-Gate 已在运行。请手动打开：{url}"));
            }
        }
        None => {
            let message = "Model-Gate 已在运行，但未找到面板端口。请查看 data/modelgate.log。";
            tracing::warn!("{message}");
            gui::show_error(message);
        }
    }
}

/// 只读读取运行中实例写入的 `settings.lastPort`；首实例启动初期可能尚未写入，重试约 3s。
fn read_last_port(db_path: &std::path::Path) -> Option<u16> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Ok(port) = read_last_port_once(db_path) {
            return Some(port);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

fn read_last_port_once(db_path: &std::path::Path) -> Result<u16, ()> {
    read_last_port_opt(db_path).map_err(|_| ())?.ok_or(())
}

/// 只读查询 `settings.lastPort`：库不存在/无该键 → `Ok(None)`，读取失败 → `Err(原因)`。
fn read_last_port_opt(db_path: &std::path::Path) -> Result<Option<u16>, String> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| err.to_string())?;
    match conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        ["lastPort"],
        |row| row.get::<_, String>(0),
    ) {
        Ok(value) => Ok(value.parse::<u16>().ok()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// 崩溃/致命日志统一的时间戳（本地时区，精确到毫秒）。
fn now_stamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f %:z")
        .to_string()
}

/// 追加写并 `sync_all`，保证崩溃现场立即落盘。
fn append_line_sync(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// panic hook：崩溃信息写入 `data/crash.log`；`RUST_BACKTRACE` 可用时附带栈。
fn install_panic_hook(data_dir: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(text) = info.payload().downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "<非字符串 panic 载荷>".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<未知位置>".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("<未命名线程>")
            .to_string();
        let backtrace = if std::env::var_os("RUST_BACKTRACE").is_some() {
            format!("\n{}", std::backtrace::Backtrace::force_capture())
        } else {
            String::new()
        };
        let entry = format!(
            "[{}] panicked: {payload}\n  线程: {thread}\n  位置: {location}{backtrace}\n\n",
            now_stamp()
        );
        if let Err(err) = append_line_sync(&data_dir.join("crash.log"), &entry) {
            eprintln!("写 crash.log 失败：{err}");
            tracing::error!("写 crash.log 失败：{err}");
        }
    }));
}

/// 启动致命错误：写入 `data/fatal.log` 并返回路径（供弹窗提示）。
fn write_fatal_log(data_dir: &std::path::Path, err: &anyhow::Error) -> PathBuf {
    let path = data_dir.join("fatal.log");
    let entry = format!("[{}] 致命错误：{err:#}\n\n", now_stamp());
    if let Err(write_err) = append_line_sync(&path, &entry) {
        tracing::error!("写 fatal.log 失败：{write_err}");
    }
    path
}

/// `--diagnose`：只读收集运行环境与配置状态，输出 `data/diagnose.txt`；不含 apiKey 等敏感值。
fn run_diagnose(
    config_path: &std::path::Path,
    db_path: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let mut out = String::new();
    out.push_str("Model-Gate 诊断报告\n");
    out.push_str(&format!("time={}\n", now_stamp()));
    out.push_str(&format!("version={}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!(
        "os={} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    if let Ok(exe) = std::env::current_exe() {
        out.push_str(&format!("exe={}\n", exe.display()));
    }
    out.push_str(&format!("config_path={}\n", config_path.display()));
    out.push_str(&format!("config_exists={}\n", config_path.exists()));
    out.push_str(&format!("db_path={}\n", db_path.display()));

    match Config::load(config_path) {
        Ok(cfg) => {
            out.push_str("config_load=ok\n");
            let diags = cfg.diagnostics();
            out.push_str(&format!("config_diagnostics={}\n", diags.len()));
            for d in &diags {
                out.push_str(&format!(
                    "  [{}] {} @ {} — {}（{}）\n",
                    d.level, d.code, d.path, d.message, d.hint
                ));
            }
            out.push_str(&format!("host={}\nport={}\n", cfg.host, cfg.port));
            out.push_str(&format!("upstreams={}\n", cfg.upstreams.len()));
            for up in &cfg.upstreams {
                out.push_str(&format!(
                    "  upstream={} protocol={} profile={} base_url={} key_configured={} default={} models={}\n",
                    up.name,
                    up.protocol,
                    up.compat.profile,
                    up.base_url,
                    up.key_configured(),
                    up.default,
                    up.models.len()
                ));
                out.push_str(&format!("  model_overrides={}\n", up.model_overrides.len()));
                for (pattern, value) in &up.model_overrides {
                    let limits = value
                        .get("limits")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".into());
                    out.push_str(&format!("    override={pattern} limits={limits}\n"));
                }
            }
            match bind_with_fallback(&cfg.host, cfg.port) {
                Ok((listener, actual)) => {
                    out.push_str(&format!("port_bindable=true\nport_actual={actual}\n"));
                    drop(listener);
                }
                Err(err) => {
                    out.push_str(&format!("port_bindable=false\nport_error={err:#}\n"));
                }
            }
        }
        Err(err) => {
            out.push_str("config_load=error\n");
            out.push_str(&format!("config_error={err:#}\n"));
        }
    }

    if !db_path.exists() {
        out.push_str("db_open=missing\n");
    } else {
        match read_last_port_opt(db_path) {
            Ok(port) => {
                out.push_str("db_open=ok\n");
                out.push_str(&format!(
                    "db_last_port={}\n",
                    port.map(|p| p.to_string()).unwrap_or_default()
                ));
            }
            Err(err) => {
                out.push_str("db_open=error\n");
                out.push_str(&format!("db_error={err}\n"));
            }
        }
    }

    let path = db_path
        .parent()
        .map(|p| p.join("diagnose.txt"))
        .unwrap_or_else(|| PathBuf::from("diagnose.txt"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &out)?;
    Ok(path)
}

fn default_config_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("bridge.config.json");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge.config.json");
    if dev.exists() {
        return dev;
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("bridge.config.json")
}

/// 默认数据库路径：与配置文件同目录的 `data\modelgate.db`。
///
/// 与 Python 冻结版（`Path(sys.executable).parent / "data" / "modelgate.db"`）同口径：
/// 配置与数据同处一个目录，升级安装后旧库原地被接管，不随 CWD 漂移。
fn default_db_path(config_path: &std::path::Path) -> PathBuf {
    let base = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("data").join("modelgate.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("modelgate-test-{tag}-{}", std::process::id()))
    }

    #[test]
    fn default_db_path_follows_config_directory() {
        // 升级衔接：库必须与配置同目录，不随 CWD 漂移。
        let config = PathBuf::from(r"C:\Program Files\Model-Gate\bridge.config.json");
        let db = default_db_path(&config);
        assert_eq!(
            db,
            PathBuf::from(r"C:\Program Files\Model-Gate\data\modelgate.db")
        );
    }

    #[test]
    fn default_db_path_handles_relative_config() {
        let db = default_db_path(std::path::Path::new("bridge.config.json"));
        assert_eq!(db, PathBuf::from("data").join("modelgate.db"));
    }

    #[test]
    fn crash_log_appends_entries() {
        let dir = temp_dir("crash");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("crash.log");
        append_line_sync(&path, "[t1] first\n").unwrap();
        append_line_sync(&path, "[t2] second\n").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[t1] first") && text.contains("[t2] second"));
        assert_eq!(text.lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fatal_log_contains_error_chain() {
        let dir = temp_dir("fatal");
        let _ = std::fs::remove_dir_all(&dir);
        let err = anyhow::anyhow!("底层原因").context("外层包装");
        let path = write_fatal_log(&dir, &err);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("外层包装") && text.contains("底层原因"));
        assert!(text.contains("致命错误"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnose_report_written_without_api_key() {
        let dir = temp_dir("diag");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("bridge.config.json");
        let mut value = config::default_config_value();
        value["port"] = serde_json::json!(0);
        let upstreams = value["upstreams"]
            .as_object_mut()
            .expect("默认配置应包含 upstreams 对象");
        let (_, first_upstream) = upstreams.iter_mut().next().expect("至少一个上游");
        first_upstream["apiKey"] = serde_json::json!("leaky-secret-should-not-appear");
        first_upstream["modelOverrides"] = serde_json::json!({
            "model-*": {"limits": {"maxOutputTokens": 1234}}
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let db_path = dir.join("modelgate.db");
        let report = run_diagnose(&config_path, &db_path).unwrap();
        let text = std::fs::read_to_string(&report).unwrap();
        assert!(text.contains("config_load=ok"));
        assert!(text.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("db_open=missing"));
        assert!(text.contains("port_bindable=true"));
        assert!(text.contains("key_configured=true"));
        assert!(text.contains("model_overrides=1"));
        assert!(text.contains("override=model-* limits=") && text.contains("1234"));
        assert!(!text.contains("leaky-secret-should-not-appear"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
