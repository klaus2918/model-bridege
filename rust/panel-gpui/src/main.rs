//! Model-Gate 原生桌面面板（GPUI）。
//!
//! 面板是网关（model-gate）的纯前端：一切数据经由 127.0.0.1:8900 的 /api，
//! 密钥永不经过面板。网关未运行时提供「启动网关」。

mod api;
mod app;
mod pages;

use gpui_kit::component::Root;
use gpui_kit::*;

use crate::app::PanelApp;

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1180.), px(760.)),
                    cx,
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let view = cx.new(|cx| PanelApp::new(cx));
                view.update(cx, |app, cx| app.refresh_all(cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("打开主窗口失败");
        cx.activate(true);
    });
}
