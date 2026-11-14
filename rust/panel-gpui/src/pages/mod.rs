//! 五个页面 + 共享渲染辅助。

pub mod access;
pub mod logs;
pub mod routes;
pub mod usage;
pub mod workbench;

use crate::app::{Page, PanelApp};
use gpui_kit::component::{ActiveTheme, Sizable, Selectable, button::{Button, ButtonVariants}};
use gpui_kit::*;
use serde_json::Value;

/// 纵向 flex 容器（gpui 无内置 helper，语义命名提升可读性）。
pub fn v_flex() -> Div {
    div().flex().flex_col()
}

/// 横向 flex 容器。
pub fn h_flex() -> Div {
    div().flex()
}

/// 卡片容器。
pub fn card(theme: &gpui_kit::component::ThemeColor, child: impl IntoElement) -> Div {
    div()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .p_4()
        .child(child)
}

/// 一个字段值取值工具：`{"a":{"b":"x"}}` → pointer。
pub fn jstr<'a>(value: &'a Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// 数字缩写：12345 → 12.3k。
pub fn abbrev(num: f64) -> String {
    if num.abs() >= 1_000_000.0 {
        format!("{:.1}m", num / 1_000_000.0)
    } else if num.abs() >= 1_000.0 {
        format!("{:.1}k", num / 1_000.0)
    } else if num.abs() >= 1.0 {
        format!("{num:.0}")
    } else {
        "0".to_string()
    }
}

/// 侧边导航。
pub fn sidebar(app: &mut PanelApp, _window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let entity = cx.entity();
    let current = app.page;

    let mut list = v_flex()
        .w(px(208.))
        .flex_shrink(0.0)
        .h_full()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .p_2()
        .gap_1();
    for page in Page::ALL {
        let entity = entity.clone();
        let active = current == page;
        list = list.child(
            Button::new(SharedString::from(format!("nav-{}", page.id())))
                .label(page.label())
                .ghost()
                .with_size(gpui_kit::component::Size::Small)
                .selected(active)
                .on_click(move |_, _, cx| {
                    let entity = entity.clone();
                    entity.update(cx, |app, cx| app.set_page(page, cx));
                }),
        );
    }
    // 底部：打开浏览器面板入口。
    list = list.child(
        v_flex()
            .mt_auto()
            .gap_1()
            .child(div().text_size(px(11.)).text_color(theme.muted_foreground).px_2().child(
                "密钥不经过面板；\n接口不回显密钥。",
            )),
    );
    list.into_any_element()
}
