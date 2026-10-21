//! 桌面壳：托盘常驻 + 系统浏览器面板（无内嵌 WebView2）。
//!
//! 面板由 Rust 内核直接嵌入并交给系统默认浏览器打开；
//! 本进程只保留托盘、剪贴板、Shell 打开等轻量交互。
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(windows)]
use windows_sys::Win32::Foundation::GlobalFree;
#[cfg(windows)]
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MessageBoxW, PostQuitMessage, SetTimer,
    TranslateMessage, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MSG, SW_SHOWNORMAL, WM_TIMER,
};

#[cfg(windows)]
include!(concat!(env!("OUT_DIR"), "/tray_icon.rs"));

#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;

/// 托盘指标刷新周期。
#[cfg(windows)]
const TRAY_STATS_INTERVAL_MS: u32 = 10_000;

/// 桌面壳启动参数。
pub struct ShellOptions {
    /// 面板地址，形如 `http://127.0.0.1:8900/ui/`。
    pub url: String,
    /// 接入地址，形如 `http://127.0.0.1:8900/v1`。
    pub endpoint: String,
    /// 数据目录（SQLite/日志所在目录）。
    pub data_dir: String,
    /// 是否在托盘就绪后自动打开面板（首次运行或 --open）。
    pub open_on_start: bool,
}

/// 启动托盘壳并阻塞在主线程消息循环；`refresh_models` 由「刷新模型目录」菜单触发，
/// `today_stats` 每 10 秒刷新托盘指标（tooltip + 只读菜单项）。
#[cfg(windows)]
pub fn run(
    options: ShellOptions,
    refresh_models: Box<dyn Fn()>,
    today_stats: Box<dyn Fn() -> serde_json::Value>,
) -> anyhow::Result<()> {
    let icon = tray_icon::Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), TRAY_ICON_SIZE, TRAY_ICON_SIZE)?;

    let menu = Menu::new();
    let open = MenuItem::new("打开面板", true, None);
    let check_update = MenuItem::new("检查更新", true, None);
    let refresh = MenuItem::new("刷新模型目录", true, None);
    let copy = MenuItem::new("复制接入地址", true, None);
    let open_dir = MenuItem::new("打开数据目录", true, None);
    let today = MenuItem::new("今日请求 —", false, None);
    let hit = MenuItem::new("缓存命中率 —", false, None);
    let address = MenuItem::new(format!("服务地址 {}", options.endpoint), false, None);
    let quit = MenuItem::new("退出", true, None);
    let _ = menu.append(&open);
    let _ = menu.append(&check_update);
    let _ = menu.append(&refresh);
    let _ = menu.append(&copy);
    let _ = menu.append(&open_dir);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&today);
    let _ = menu.append(&hit);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&address);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("今日请求 —\n缓存命中率 —")
        .with_icon(icon)
        .build()
    {
        Ok(tray) => tray,
        Err(err) => {
            tracing::warn!("托盘初始化失败：{err}；降级为仅浏览器面板");
            let _ = open_url(&options.url);
            anyhow::bail!("托盘不可用：{err}");
        }
    };
    if options.open_on_start && !open_url(&options.url) {
        tracing::warn!("打开面板失败：{}", options.url);
    }

    // 启动即读一次快照，之后每 10 秒刷新（WM_TIMER 投递到本线程消息队列）。
    apply_tray_stats(&tray, &today, &hit, &today_stats());
    // SAFETY: SetTimer 只登记本线程的定时器；窗口句柄为空时 WM_TIMER 进入线程队列。
    // hWnd 为 NULL 时 nIDEvent 会被系统忽略，WM_TIMER 的 wParam 是系统分配的返回值，
    // 因此后续必须按 timer_id 判定，不能再用传入值 1。
    let timer_id = unsafe { SetTimer(std::ptr::null_mut(), 1, TRAY_STATS_INTERVAL_MS, None) };
    if timer_id == 0 {
        tracing::warn!("托盘指标定时器创建失败：仅显示启动时快照");
    }

    let options = std::sync::Arc::new(options);
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if is_stats_timer_message(msg.message, msg.wParam, timer_id) {
            apply_tray_stats(&tray, &today, &hit, &today_stats());
        }
        let mut should_quit = false;
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id == open.id() {
                if !open_url(&options.url) {
                    tracing::warn!("打开面板失败：{}", options.url);
                }
            } else if event.id == check_update.id() {
                let url = format!("{}?update=1", options.url);
                if !open_url(&url) {
                    tracing::warn!("打开更新面板失败：{}", url);
                }
            } else if event.id == refresh.id() {
                refresh_models();
            } else if event.id == copy.id() {
                if !copy_to_clipboard(&options.endpoint) {
                    tracing::warn!("复制接入地址失败：剪贴板不可用");
                }
            } else if event.id == open_dir.id() {
                if !open_url(&options.data_dir) {
                    tracing::warn!("打开数据目录失败：{}", options.data_dir);
                }
            } else if event.id == quit.id() {
                should_quit = true;
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) && !open_url(&options.url) {
                tracing::warn!("打开面板失败：{}", options.url);
            }
        }
        if should_quit {
            unsafe { PostQuitMessage(0) };
        }
    }
    if timer_id != 0 {
        unsafe { KillTimer(std::ptr::null_mut(), timer_id) };
    }
    drop(tray);
    Ok(())
}

/// 判断消息是否为托盘指标定时器触发。
///
/// `SetTimer(NULL, nIDEvent, ...)` 会忽略传入的 `nIDEvent`，返回系统分配的
/// 定时器 ID；`WM_TIMER` 的 `wParam` 携带该返回值。`timer_id == 0` 表示创建失败，
/// 此时不匹配任何消息，避免误刷。
#[cfg(windows)]
fn is_stats_timer_message(message: u32, wparam: usize, timer_id: usize) -> bool {
    timer_id != 0 && message == WM_TIMER && wparam == timer_id
}

/// 把今日统计写入托盘 tooltip 与菜单只读项（命中率无样本时显示「—」）。
#[cfg(windows)]
fn apply_tray_stats(tray: &TrayIcon, today: &MenuItem, hit: &MenuItem, stats: &serde_json::Value) {
    let requests = stats
        .get("requests")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let rate = stats
        .get("cache_hit_rate")
        .and_then(serde_json::Value::as_f64);
    let rate_text = match rate {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => "—".to_string(),
    };
    today.set_text(format!("今日请求 {requests}"));
    hit.set_text(format!("缓存命中率 {rate_text}"));
    let _ = tray.set_tooltip(Some(format!("今日请求 {requests}\n缓存命中率 {rate_text}")));
}

#[cfg(not(windows))]
pub fn run(
    _options: ShellOptions,
    _refresh_models: Box<dyn Fn()>,
    _today_stats: Box<dyn Fn() -> serde_json::Value>,
) -> anyhow::Result<()> {
    anyhow::bail!("桌面壳仅支持 Windows")
}

/// 用系统 Shell 打开 URL、文件或目录；返回是否成功。
#[cfg(windows)]
pub fn open_url(target: &str) -> bool {
    let operation = wide("open");
    let file = wide(target);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    result as isize > 32
}

#[cfg(not(windows))]
pub fn open_url(_target: &str) -> bool {
    false
}

#[cfg(windows)]
fn copy_to_clipboard(text: &str) -> bool {
    let text = wide(text);
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let bytes = text.len() * std::mem::size_of::<u16>();
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if handle.is_null() {
            CloseClipboard();
            return false;
        }
        let target = GlobalLock(handle) as *mut u16;
        if target.is_null() {
            GlobalFree(handle);
            CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
        GlobalUnlock(handle);
        if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
            GlobalFree(handle);
            CloseClipboard();
            return false;
        }
        CloseClipboard();
        true
    }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 启动致命错误弹窗；`--no-gui` 场景由调用方决定是否使用。
pub fn show_error(message: &str) {
    #[cfg(windows)]
    {
        let body = wide(message);
        let caption = wide("Model-Gate");
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                caption.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = message;
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{is_stats_timer_message, WM_TIMER};

    #[test]
    fn stats_timer_matches_returned_id_only() {
        assert!(is_stats_timer_message(WM_TIMER, 28_617, 28_617));
    }

    #[test]
    fn stats_timer_rejects_old_nidevent_check() {
        // 回归：SetTimer(NULL, 1, ..) 返回非 1 时，旧写法 `wParam == 1` 永不命中。
        assert!(!is_stats_timer_message(WM_TIMER, 1, 28_617));
        assert!(!is_stats_timer_message(WM_TIMER, 28_617, 1));
    }

    #[test]
    fn stats_timer_ignores_other_messages_and_zero_id() {
        assert!(!is_stats_timer_message(0x0100, 28_617, 28_617)); // WM_KEYDOWN
        assert!(!is_stats_timer_message(WM_TIMER, 0, 0)); // 创建失败：不匹配任何 tick
        assert!(!is_stats_timer_message(WM_TIMER, 28_617, 0));
    }
}
