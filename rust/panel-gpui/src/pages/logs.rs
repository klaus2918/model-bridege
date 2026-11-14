//! 日志：级别过滤 + 关键字 + 自动刷新（从 /api/logs 读取文件尾部）。

use super::{h_flex, v_flex};
use crate::app::PanelApp;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Sizable, Selectable, button::{Button, ButtonVariants}};
use gpui_kit::*;
use serde_json::Value;

/// 日志过滤状态；query 部分拼在 /api/logs 后面。
#[derive(Clone, Default)]
pub struct LogsFilter {
    pub keyword: String,
}

impl LogsFilter {
    pub fn as_query(&self) -> String {
        // 级别/关键字过滤在前端做，服务端只按行数截尾。
        String::new()
    }
}

#[derive(Default)]
pub struct LogsPage {
    pub filter: LogsFilter,
    pub keyword_input: Option<Entity<InputState>>,
    pub auto_refresh: bool,
    pub level: Option<String>,
}

pub fn render(app: &mut PanelApp, window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let entity = cx.entity();

    if app.logs_page.keyword_input.is_none() {
        app.logs_page.keyword_input = Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("关键字过滤")
        }));
    }

    // 过滤词读入（输入 on_change 里同步）。
    let keyword = app.logs_page.filter.keyword.clone();
    let level = app.logs_page.level.clone();
    let auto = app.logs_page.auto_refresh;

    let mut controls = h_flex().gap_2().items_center();
    for lv in ["info", "warn", "error"] {
        let entity_lv = entity.clone();
        let lv_s = lv.to_string();
        let selected = level.as_deref() == Some(lv);
        controls = controls.child(
            Button::new(SharedString::from(format!("log-{lv}")))
                .label(lv)
                .ghost()
                .with_size(gpui_kit::component::Size::XSmall)
                .selected(selected)
                .on_click(move |_, _, cx| {
                    let entity = entity_lv.clone();
                    let lv = lv_s.clone();
                    entity.update(cx, |app, cx| {
                        app.logs_page.level = if app.logs_page.level.as_deref() == Some(lv.as_str()) {
                            None
                        } else {
                            Some(lv)
                        };
                        app.refresh_logs(cx);
                        cx.notify();
                    });
                }),
        );
    }
    let entity_auto = entity.clone();
    controls = controls.child(
        Button::new("log-auto")
            .label(if auto { "自动刷新：开" } else { "自动刷新：关" })
            .ghost()
            .with_size(gpui_kit::component::Size::XSmall)
            .selected(auto)
            .on_click(move |_, _, cx| {
                let entity = entity_auto.clone();
                entity.update(cx, |app, cx| {
                    app.logs_page.auto_refresh = !app.logs_page.auto_refresh;
                    if app.logs_page.auto_refresh {
                        schedule_auto_refresh(cx);
                    }
                    cx.notify();
                });
            }),
    );

    // 日志正文。
    let logs = app.logs.clone().unwrap_or(Value::Null);
    let text = logs
        .get("logs")
        .or_else(|| logs.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| {
            if let Some(level) = &level {
                if !line.to_lowercase().contains(&format!(" {}", level.to_lowercase())) {
                    return false;
                }
            }
            if !keyword.is_empty() && !line.to_lowercase().contains(&keyword.to_lowercase()) {
                return false;
            }
            true
        })
        .collect();
    let total = text.lines().count();
    let mut body = v_flex().gap_0p5();
    for line in lines.iter().rev().take(120).rev() {
        let is_error = line.contains(" ERROR ") || line.contains(" error[");
        let is_warn = line.contains(" WARN ");
        body = body.child(
            div()
                .text_size(px(11.))
                .font_family("monospace")
                .text_color(if is_error {
                    theme.danger
                } else if is_warn {
                    theme.warning
                } else {
                    theme.foreground
                })
                .child((*line).to_string()),
        );
    }
    if lines.is_empty() {
        body = body.child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(
            if app.online { "没有匹配的日志。" } else { "网关未连接。" },
        ));
    }

    let _entity_kw = entity.clone();
    let keyword_input = app.logs_page.keyword_input.as_ref().map(|s| Input::new(s));
    let mut header = h_flex().gap_2().items_center()
        .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("日志与诊断"))
        .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(format!("{}/{} 行", lines.len(), total)))
        .child(controls);
    if let Some(input) = keyword_input {
        header = header.child(div().w(px(224.)).child(input));
    }
    let entity_apply = entity.clone();
    header = header.child(
        Button::new("log-apply").label("过滤").ghost().with_size(gpui_kit::component::Size::XSmall).on_click(move |_, _, cx| {
            let entity = entity_apply.clone();
            entity.update(cx, |app, cx| {
                if let Some(state) = app.logs_page.keyword_input.clone() {
                    app.logs_page.filter.keyword = state.read(cx).value().trim().to_string();
                }
                cx.notify();
            });
        }),
    );

    v_flex().gap_2()
        .child(header)
        .child(
            div()
                .id("logs-body")
                .flex_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .p_3()
                .overflow_y_scroll()
                .child(body),
        )
        .into_any_element()
}

fn schedule_auto_refresh(cx: &mut Context<PanelApp>) {
    cx.spawn(async move |this, cx| loop {
        cx.background_executor()
            .timer(std::time::Duration::from_secs(5))
            .await;
        let cont = this
            .update(cx, |app, cx| {
                if !app.logs_page.auto_refresh || app.page != crate::app::Page::Logs {
                    app.logs_page.auto_refresh = false;
                    cx.notify();
                    return false;
                }
                app.refresh_logs(cx);
                true
            })
            .ok()
            .unwrap_or(false);
        if !cont {
            return;
        }
    })
    .detach();
}
