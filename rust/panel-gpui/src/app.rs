//! 面板应用骨架：侧边导航 + 页面切换 + 数据缓存 + 轻量 toast。

use crate::api::{brief_error, GatewayClient};
use crate::pages;
use gpui_kit::component::{ActiveTheme, Sizable, TitleBar, button::{Button, ButtonVariants}};
use gpui_kit::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Workbench,
    Access,
    Routes,
    Usage,
    Logs,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Workbench,
        Page::Access,
        Page::Routes,
        Page::Usage,
        Page::Logs,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Page::Workbench => "workbench",
            Page::Access => "access",
            Page::Routes => "routes",
            Page::Usage => "usage",
            Page::Logs => "logs",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Workbench => "工作台",
            Page::Access => "接入管理",
            Page::Routes => "路由与分组",
            Page::Usage => "用量",
            Page::Logs => "日志",
        }
    }
}

/// 轻量 toast：右下角浮层，几秒后自动消失。
#[derive(Clone)]
pub struct Toast {
    pub message: String,
    pub ok: bool,
    pub at: Instant,
}

pub struct PanelApp {
    pub client: Arc<GatewayClient>,
    pub page: Page,
    pub online: bool,
    pub loading: bool,
    pub toast: Option<Toast>,
    // 数据缓存（serde_json::Value 为主：网关接口字段丰富且演进快，取值处再断言）。
    pub info: Option<Value>,
    pub status: Option<Value>,
    pub health: Option<Value>,
    pub access: Option<Value>,
    pub config: Option<Value>,
    pub summary: Option<Value>,
    pub totals: Option<Value>,
    pub logs: Option<Value>,
    // 页面局部状态
    pub access_page: pages::access::AccessPage,
    pub routes_page: pages::routes::RoutesPage,
    pub logs_page: pages::logs::LogsPage,
}

impl PanelApp {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let client = Arc::new(GatewayClient::new(&GatewayClient::default_url()));
        Self {
            client,
            page: Page::Workbench,
            online: false,
            loading: false,
            toast: None,
            info: None,
            status: None,
            health: None,
            access: None,
            config: None,
            summary: None,
            totals: None,
            logs: None,
            access_page: pages::access::AccessPage::default(),
            routes_page: pages::routes::RoutesPage::default(),
            logs_page: pages::logs::LogsPage::default(),
        }
    }

    // ── 异步数据加载 ────────────────────────────────────────────────

    /// 后台线程拉取一批核心数据，完成后一次性更新。
    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
        let client = self.client.clone();
        let logs_filter = self.logs_page.filter.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let online = client.ping();
                let info = client.get("/api/info").ok();
                let status = client.get("/api/proxy/status").ok();
                let health = client.get("/api/upstreams/health").ok();
                let access = client.get("/api/access").ok();
                let config = client.get("/api/config").ok().map(|v| {
                    v.get("config").cloned().unwrap_or(Value::Null)
                });
                let summary = client.get("/api/usage/summary?group=today").ok();
                let totals = client.get("/api/usage/totals").ok();
                let logs = client
                    .get(&format!("/api/logs?limit=400{}", logs_filter.as_query()))
                    .ok();
                (online, info, status, health, access, config, summary, totals, logs)
            });
            let (online, info, status, health, access, config, summary, totals, logs) =
                result.await;
            this.update(cx, |app, cx| {
                app.online = online;
                app.info = info;
                app.status = status;
                app.health = health;
                app.access = access;
                if let Some(config) = config.clone() {
                    app.config = Some(config);
                }
                app.summary = summary;
                app.totals = totals;
                app.logs = logs;
                app.loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 刷新接入列表（增删改接入后调用）。
    pub fn refresh_access(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let access = cx
                .background_executor()
                .spawn(async move { client.get("/api/access").ok() })
                .await;
            this.update(cx, |app, cx| {
                if let Some(access) = access {
                    app.access = Some(access);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 刷新配置（保存分组 / 修改接入后调用）。
    pub fn refresh_config(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let config = cx
                .background_executor()
                .spawn(async move {
                    client
                        .get("/api/config")
                        .ok()
                        .map(|v| v.get("config").cloned().unwrap_or(Value::Null))
                })
                .await;
            this.update(cx, |app, cx| {
                if let Some(config) = config {
                    app.config = Some(config);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 刷新日志。
    pub fn refresh_logs(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let filter = self.logs_page.filter.clone();
        cx.spawn(async move |this, cx| {
            let logs = cx
                .background_executor()
                .spawn(async move {
                    client
                        .get(&format!("/api/logs?limit=400{}", filter.as_query()))
                        .ok()
                })
                .await;
            this.update(cx, |app, cx| {
                if let Some(logs) = logs {
                    app.logs = Some(logs);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ── 交互辅助 ───────────────────────────────────────────────────

    pub fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        match page {
            Page::Logs => self.refresh_logs(cx),
            Page::Usage => {}
            _ => {}
        }
        cx.notify();
    }

    pub fn toast_ok(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some(Toast {
            message: message.into(),
            ok: true,
            at: Instant::now(),
        });
        self.schedule_toast_clear(cx);
        cx.notify();
    }

    pub fn toast_err(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some(Toast {
            message: message.into(),
            ok: false,
            at: Instant::now(),
        });
        self.schedule_toast_clear(cx);
        cx.notify();
    }

    fn schedule_toast_clear(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(4))
                .await;
            this.update(cx, |app, cx| {
                let expired = app
                    .toast
                    .as_ref()
                    .map(|t| t.at.elapsed() >= Duration::from_secs(4))
                    .unwrap_or(true);
                if expired {
                    app.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 在后台执行一个 API 调用，成功/失败都弹 toast，然后跑 after 回调。
    pub fn run_api<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(Arc<GatewayClient>) -> Result<R, anyhow::Error> + Send + 'static,
        after: impl FnOnce(&mut Self, R, &mut Context<Self>) + Send + 'static,
    ) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job(client) }).await;
            this.update(cx, |app, cx| match result {
                Ok(value) => {
                    after(app, value, cx);
                }
                Err(err) => {
                    app.toast_err(brief_error(err), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

// ── 渲染 ──────────────────────────────────────────────────────────

impl Render for PanelApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.theme().colors.clone();
        let online = self.online;

        let sidebar = pages::sidebar(self, window, cx);

        // 接入页:详情抽屉作为右侧并排栏渲染(普通流布局,
        // Input 组件在 absolute overlay 内渲染会中断)。
        let show_drawer = self.page == Page::Access && self.access_page.drawer.is_some();
        let content = match self.page {
            Page::Workbench => pages::workbench::render(self, window, cx),
            Page::Access => pages::access::render(self, window, cx),
            Page::Routes => pages::routes::render(self, window, cx),
            Page::Usage => pages::usage::render(self, window, cx),
            Page::Logs => pages::logs::render(self, window, cx),
        };

        // 顶栏：品牌 + 连接状态 + 刷新。
        let topbar = TitleBar::new()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(theme.accent)
                            .font_weight(FontWeight::BOLD)
                            .child("Model-Gate"),
                    )
                    .child(div().text_color(theme.muted_foreground).child("原生面板")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .bg(if online {
                                theme.success
                            } else {
                                theme.danger
                            }),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .text_size(px(12.))
                            .child(if online {
                                format!(
                                    "已连接 {}",
                                    self.info
                                        .as_ref()
                                        .and_then(|i| i.get("version"))
                                        .and_then(Value::as_str)
                                        .map(|v| format!("v{v}"))
                                        .unwrap_or_default()
                                )
                            } else {
                                "网关未连接".to_string()
                            }),
                    ),
            )
            .child(
                Button::new("refresh-all")
                    .label(if self.loading { "刷新中…" } else { "刷新" })
                    .ghost()
                    .with_size(gpui_kit::component::Size::Small)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |app, cx| app.refresh_all(cx));
                    }),
            );

        let toast_overlay = self.toast.as_ref().map(|t| {
            div()
                .absolute()
                .bottom_4()
                .right_4()
                .px_3()
                .py_2()
                .rounded_md()
                .shadow_lg()
                .bg(if t.ok { theme.success } else { theme.danger })
                .text_color(theme.background)
                .max_w(px(384.))
                .child(t.message.clone())
        });

        div()
            .id("panel-root")
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .flex()
            .flex_col()
            .child(topbar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .px_5()
                            .py_4()
                            .child(content),
                    ),
            )
            .children(toast_overlay)
    }
}
