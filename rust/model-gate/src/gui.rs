//! 桌面壳：托盘常驻 + 系统浏览器面板（无内嵌 WebView2）。
//!
//! 面板由 Rust 内核直接嵌入并交给系统默认浏览器打开；
//! 本进程只保留托盘、剪贴板、Shell 打开等轻量交互。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, DispatchMessageW, EnumThreadWindows, GetClassNameW,
    GetMessageW, KillTimer, MessageBoxW, PostQuitMessage, PostThreadMessageW, SetTimer,
    SetWindowLongPtrW, TranslateMessage, GWLP_WNDPROC, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MSG,
    SW_SHOWNORMAL, WM_CLOSE, WM_TIMER, WNDPROC,
};

#[cfg(windows)]
include!(concat!(env!("OUT_DIR"), "/tray_icon.rs"));

#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;

/// 托盘窗口的窗口类名（`tray-icon` crate 内部注册）。
///
/// 我们需要找到这个窗口并子类化它的窗口过程：Windows Restart Manager
/// （Inno Setup 用它来关闭占用文件的进程）只会给**进程的一个顶层窗口**发
/// `WM_CLOSE`。托盘窗口正是本进程唯一的顶层窗口，而它默认不处理 `WM_CLOSE`，
/// 于是窗口被销毁、进程却不退出 —— Inno 就会一直等下去（用户看到的「卡死」）。
#[cfg(windows)]
const TRAY_WINDOW_CLASS: &str = "tray_icon_app";

/// 托盘窗口原始窗口过程（子类化后转发到这里）。
#[cfg(windows)]
static TRAY_ORIGINAL_WNDPROC: AtomicUsize = AtomicUsize::new(0);

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
///
/// `shutdown` 是关闭协调器：更新流程或外部 `WM_CLOSE` 都会通过它让本循环退出。
#[cfg(windows)]
pub fn run(
    options: ShellOptions,
    refresh_models: Box<dyn Fn()>,
    today_stats: Box<dyn Fn() -> serde_json::Value>,
    shutdown: Arc<crate::lifecycle::Shutdown>,
) -> anyhow::Result<()> {
    // 登记托盘线程：关闭请求要靠它把「请退出」投递回本线程的消息队列。
    shutdown.set_tray_thread(unsafe { GetCurrentThreadId() });
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

    // 让托盘窗口正确响应 WM_CLOSE：外部（Inno 的 Restart Manager）发来的关闭
    // 请求必须变成「优雅退出」，而不是「窗口没了但进程还在」——
    // 后者正是安装器卡在关闭上的根因。
    if !hook_tray_window_close() {
        tracing::warn!("未能接管托盘窗口的 WM_CLOSE，外部关闭请求可能无法优雅退出");
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
        if msg.message == crate::lifecycle::WM_APP_SHUTDOWN {
            // 「请退出」：来源可能是更新流程（要装新版本），也可能是外部
            // WM_CLOSE 被托盘窗口转投递过来的。统一走优雅关闭流程。
            shutdown.request();
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

/// 找到本线程里 `tray-icon` 创建的托盘窗口，并子类化它的窗口过程。
///
/// 返回是否接管成功。失败不影响托盘本身（只是外部 `WM_CLOSE` 不会优雅退出）。
#[cfg(windows)]
fn hook_tray_window_close() -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};

    /// 枚举用的上下文：找到目标窗口就停。
    struct Search {
        found: HWND,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = &mut *(lparam as *mut Search);
        if search.found.is_null() {
            let mut buffer = [0u16; 64];
            let len = GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            if len > 0 {
                let name = String::from_utf16_lossy(&buffer[..len as usize]);
                if name == TRAY_WINDOW_CLASS {
                    search.found = hwnd;
                }
            }
        }
        // 返回 0 结束枚举。
        if search.found.is_null() {
            1
        } else {
            0
        }
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    let mut search = Search {
        found: std::ptr::null_mut(),
    };
    unsafe {
        EnumThreadWindows(
            thread_id,
            Some(enum_proc),
            &mut search as *mut Search as LPARAM,
        );
    }
    if search.found.is_null() {
        return false;
    }
    let previous = unsafe {
        SetWindowLongPtrW(
            search.found,
            GWLP_WNDPROC,
            tray_window_proc as *const () as isize,
        )
    };
    if previous == 0 {
        return false;
    }
    TRAY_ORIGINAL_WNDPROC.store(previous as usize, Ordering::SeqCst);
    true
}

/// 托盘窗口的窗口过程（子类）。
///
/// 只拦一件事：`WM_CLOSE`。
///
/// 背景：Inno Setup 的 Restart Manager 通过 `WM_CLOSE` 请求关闭占用文件的进程。
/// 托盘窗口默认走 `DefWindowProc` → 窗口被销毁、**进程却不退出**，
/// Inno 就会一直等下去（安装卡在「关闭」这一步）。
///
/// 这里把 `WM_CLOSE` 翻译成「请退出消息循环」，由主循环统一走优雅关闭流程
/// （停 HTTP 服务 → 数据库落盘 → 退出进程），并且**不销毁托盘窗口**。
#[cfg(windows)]
unsafe extern "system" fn tray_window_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    if msg == WM_CLOSE {
        let thread_id = unsafe { GetCurrentThreadId() };
        unsafe {
            PostThreadMessageW(thread_id, crate::lifecycle::WM_APP_SHUTDOWN, 0, 0);
        }
        return 0;
    }
    let original = TRAY_ORIGINAL_WNDPROC.load(Ordering::SeqCst);
    if original == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let original: WNDPROC = std::mem::transmute(original);
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
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
    _shutdown: Arc<crate::lifecycle::Shutdown>,
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
