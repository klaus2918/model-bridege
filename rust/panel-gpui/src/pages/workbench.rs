//! 工作台：网关状态 + 今日 KPI + 接入健康 + 快捷动作。

use super::{h_flex, v_flex, abbrev, card, jstr};
use crate::app::PanelApp;
use gpui_kit::component::{ActiveTheme, Sizable, button::{Button, ButtonVariants}};

use gpui_kit::*;
use serde_json::Value;

pub fn render(app: &mut PanelApp, _window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let entity = cx.entity();

    // KPI
    let summary = app.summary.clone().unwrap_or(Value::Null);
    let rows = summary.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
    let (requests, tokens, cost): (f64, f64, f64) = {
        let mut r = (0.0, 0.0, 0.0);
        for row in &rows {
            r.0 += row.get("requests").and_then(Value::as_f64).unwrap_or(0.0);
            r.1 += row.get("total_tokens").and_then(Value::as_f64).unwrap_or(0.0);
            r.2 += row.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        }
        r
    };

    let mut kpis = h_flex().gap_3();
    for (label, value, hint) in [
        ("请求数", abbrev(requests), "今日"),
        ("Token", abbrev(tokens), "今日"),
        ("费用", format!("${:.4}", cost), "今日（按单价表）"),
        (
            "接入",
            jstr(app.info.as_ref().unwrap_or(&Value::Null), "/upstream_count"),
            "已配置上游",
        ),
    ] {
        kpis = kpis.child(
            card(&theme, v_flex().gap_1()
                .child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(label))
                .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).child(value))
                .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(hint)))
        );
    }

    // 上游健康
    let mut health_list = v_flex().gap_1();
    let health = app.health.clone().unwrap_or(Value::Null);
    let items = health.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    if items.is_empty() {
        health_list = health_list.child(
            div().text_size(px(12.)).text_color(theme.muted_foreground).child(
                if app.online { "暂无健康数据" } else { "网关未连接" },
            ),
        );
    }
    for item in items.iter().take(12) {
        let name = jstr(item, "/name");
        let ok = item.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let latency = jstr(item, "/latency_ms");
        health_list = health_list.child(
            h_flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(div().size_2().rounded_full().bg(if ok { theme.success } else { theme.danger }))
                .child(div().text_size(px(13.)).child(name))
                .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(
                    if ok { format!("{latency} ms") } else { "不可用".to_string() },
                )),
        );
    }

    // 快捷动作
    let actions = h_flex().gap_2()
        .child(
            Button::new("open-web")
                .label("打开浏览器面板")
                .ghost()
                .with_size(gpui_kit::component::Size::Small)
                .on_click(|_, _, _| {
                    let url = crate::api::GatewayClient::default_url();
                    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                }),
        )
        .child(
            Button::new("start-gateway")
                .label("启动网关")
                .ghost()
                .with_size(gpui_kit::component::Size::Small)
                .on_click(move |_, _, cx| {
                    let entity = entity.clone();
                    // 网关二进制与面板同目录（一并分发）；找不到给 toast。
                    let exe = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("model-gate")));
                    match exe.filter(|p| p.exists()) {
                        Some(path) => {
                            let _ = std::process::Command::new(path).spawn();
                            entity.update(cx, |app, cx| {
                                app.toast_ok("网关进程已启动（稍后自动重连）", cx);
                            });
                        }
                        None => {
                            entity.update(cx, |app, cx| {
                                app.toast_err("未在同目录找到 model-gate 可执行文件", cx);
                            });
                        }
                    }
                }),
        );

    let status_line = if app.online {
        format!(
            "网关 v{} · 端口 {} · 配置 {}",
            jstr(app.info.as_ref().unwrap_or(&Value::Null), "/version"),
            jstr(app.info.as_ref().unwrap_or(&Value::Null), "/port"),
            jstr(app.info.as_ref().unwrap_or(&Value::Null), "/config_path"),
        )
    } else {
        "网关未连接：请确认 model-gate 正在运行，或点「启动网关」。".to_string()
    };

    v_flex()
        .id("wb-root")
        .gap_4()
        .overflow_y_scroll()
        .child(
            card(&theme, v_flex().gap_2()
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("状态"))
                .child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(status_line))
                .child(actions)),
        )
        .child(kpis)
        .child(card(&theme, v_flex().gap_1()
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("上游健康"))
            .child(health_list)))
        .into_any_element()
}
