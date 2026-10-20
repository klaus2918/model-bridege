//! 桌面壳：托盘常驻 + 系统浏览器面板（无内嵌 WebView2）。
//!
//! 面板仍是同一份 `modelgate/web/`，宿主从 pywebview/wry 换成系统默认浏览器；
//! 本进程只保留托盘、剪贴板、Shell 打开等轻量交互。
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

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
    DispatchMessageW, GetMessageW, MessageBoxW, PostQuitMessage, TranslateMessage, MB_ICONERROR,
    MB_OK, MB_SETFOREGROUND, MSG, SW_SHOWNORMAL,
};

#[cfg(windows)]
include!(concat!(env!("OUT_DIR"), "/tray_icon.rs"));

#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;

/// 桌面壳启动参数。
pub struct ShellOptions {
    /// 面板地址，形如 `http://127.0.0.1:8900/ui/`。
    pub url: String,
    /// 接入地址，形如 `http://127.0.0.1:8900/v1`。
    pub endpoint: String,
    /// 数据目录（SQLite/日志所在目录）。
    pub data_dir: String,
    /// 托盘 tooltip 标题。
    pub title: String,
    /// 是否在托盘就绪后自动打开面板（首次运行或 --open）。
    pub open_on_start: bool,
}

/// 启动托盘壳并阻塞在主线程消息循环；`refresh_models` 由「刷新模型目录」菜单触发。
#[cfg(windows)]
pub fn run(options: ShellOptions, refresh_models: Box<dyn Fn()>) -> anyhow::Result<()> {
    let icon = tray_icon::Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), TRAY_ICON_SIZE, TRAY_ICON_SIZE)?;

    let menu = Menu::new();
    let open = MenuItem::new("打开面板", true, None);
    let refresh = MenuItem::new("刷新模型目录", true, None);
    let copy = MenuItem::new("复制接入地址", true, None);
    let open_dir = MenuItem::new("打开数据目录", true, None);
    let address = MenuItem::new(format!("服务地址 {}", options.endpoint), false, None);
    let quit = MenuItem::new("退出", true, None);
    let _ = menu.append(&open);
    let _ = menu.append(&refresh);
    let _ = menu.append(&copy);
    let _ = menu.append(&open_dir);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&address);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!("{} {}", options.title, options.endpoint))
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
        let mut should_quit = false;
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id == open.id() {
                if !open_url(&options.url) {
                    tracing::warn!("打开面板失败：{}", options.url);
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
    drop(tray);
    Ok(())
}

#[cfg(not(windows))]
pub fn run(_options: ShellOptions, _refresh_models: Box<dyn Fn()>) -> anyhow::Result<()> {
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
