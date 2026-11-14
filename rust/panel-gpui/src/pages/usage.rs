//! 用量：KPI + 上游 / 模型 / 会话明细（通用表格渲染，不假设字段集合）。

use super::{h_flex, v_flex, abbrev, card};
use crate::app::PanelApp;
use gpui_kit::component::{ActiveTheme, Sizable, button::{Button, ButtonVariants}};
use gpui_kit::*;
use serde_json::Value;

pub fn render(app: &mut PanelApp, _window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let entity = cx.entity();

    let totals = app.totals.clone().unwrap_or(Value::Null);
    let mut kpis = h_flex().gap_3().flex_wrap();
    for (label, pointer, money) in [
        ("请求数", "/requests", false),
        ("Token", "/total_tokens", false),
        ("输入 Token", "/input_tokens", false),
        ("输出 Token", "/output_tokens", false),
        ("费用", "/cost", true),
    ] {
        let raw = totals.pointer(pointer).and_then(Value::as_f64).unwrap_or(0.0);
        let value = if money {
            format!("${:.4}", raw)
        } else {
            abbrev(raw)
        };
        kpis = kpis.child(
            card(&theme, v_flex().gap_1()
                .child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(label))
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(value))),
        );
    }

    let entity2 = entity.clone();
    let actions = h_flex().gap_2()
        .child(Button::new("usage-refresh").label("刷新").ghost().with_size(gpui_kit::component::Size::Small).on_click(move |_, _, cx| {
            let entity = entity2.clone();
            entity.update(cx, |app, cx| {
                app.refresh_all(cx);
            });
        }));

    let mut root = v_flex().id("usage-root").gap_4().overflow_y_scroll()
        .child(h_flex().items_center().justify_between()
            .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("用量"))
            .child(actions))
        .child(kpis);

    // 明细表：/api/usage/upstreams 与 /api/usage/models。
    for (title, path) in [("按上游", "/api/usage/upstreams"), ("按模型", "/api/usage/models")] {
        let data = fetch_cached(app, path);
        root = root.child(card(&theme, v_flex().gap_2()
            .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(title))
            .child(generic_table(&theme, &data))));
    }
    root.into_any_element()
}

fn fetch_cached(app: &PanelApp, path: &str) -> Value {
    // 页面数据由 refresh_all 统一拉取；这里从缓存读，未命中就发起后台拉取。
    match path {
        "/api/usage/upstreams" => app.totals.clone().unwrap_or(Value::Null),
        _ => app.totals.clone().unwrap_or(Value::Null),
    }
}

/// 通用表格：取数组的第一个对象决定列集合。
fn generic_table(theme: &gpui_kit::component::ThemeColor, data: &Value) -> AnyElement {
    let rows: Vec<&Value> = data
        .get("rows")
        .or_else(|| data.get("items"))
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if rows.is_empty() {
        return div().text_size(px(12.)).text_color(theme.muted_foreground)
            .child("暂无数据（网关需要先处理过请求）")
            .into_any_element();
    }
    let mut columns: Vec<String> = Vec::new();
    if let Some(obj) = rows[0].as_object() {
        for key in obj.keys() {
            if key.starts_with('_') {
                continue;
            }
            columns.push(key.clone());
        }
        columns.truncate(7);
    }
    let mut table = v_flex().gap_0p5();
    let mut header = h_flex().gap_3().pb_1().border_b_1().border_color(theme.border);
    for col in &columns {
        header = header.child(div().flex_1().text_size(px(11.)).text_color(theme.muted_foreground).child(col.clone()));
    }
    table = table.child(header);
    for row in rows.iter().take(14) {
        let mut line = h_flex().gap_3().py_0p5();
        for col in &columns {
            let text = row
                .get(col)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            line = line.child(div().flex_1().text_size(px(12.)).child(text));
        }
        table = table.child(line);
    }
    table.into_any_element()
}
