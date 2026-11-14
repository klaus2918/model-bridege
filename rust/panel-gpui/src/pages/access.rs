//! 接入管理：列表 + 添加向导 + 详情抽屉（凭据四选一）。
//!
//! 凭据写入目标与设计文档一致（互斥四选一）：
//! 本机加密库（推荐）/ 用户环境变量 / 凭据文件 / 暂不配置（不点保存）。

use super::{card, h_flex, jstr, v_flex};
use crate::app::PanelApp;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, Selectable, Size, button::{Button, ButtonVariants},
};
use gpui_kit::*;
use serde_json::Value;

/// 凭据写入目标（不含「暂不配置」——它等于不点保存）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyTargetSel {
    Store,
    Env,
    File,
}

impl KeyTargetSel {
    fn as_str(self) -> &'static str {
        match self {
            KeyTargetSel::Store => "store",
            KeyTargetSel::Env => "env",
            KeyTargetSel::File => "file",
        }
    }

    fn label(self) -> &'static str {
        match self {
            KeyTargetSel::Store => "本机加密库（推荐）",
            KeyTargetSel::Env => "用户环境变量",
            KeyTargetSel::File => "凭据文件",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrawerTab {
    Overview,
    Credential,
    Models,
    Diagnostics,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WizardMode {
    Official,
    Custom,
}

#[derive(Clone)]
pub struct Wizard {
    pub mode: WizardMode,
    pub step: u8,
    pub template_id: Option<String>,
    pub name: Option<Entity<InputState>>,
    pub base_url: Option<Entity<InputState>>,
    pub vendor: Option<Entity<InputState>>,
    pub key: Option<Entity<InputState>>,
    pub env_name: Option<Entity<InputState>>,
    pub file_path: Option<Entity<InputState>>,
    pub target: KeyTargetSel,
    pub busy: bool,
}

#[derive(Clone)]
pub struct Drawer {
    pub id: String,
    pub tab: DrawerTab,
    pub secret: Option<Entity<InputState>>,
    pub env_name: Option<Entity<InputState>>,
    pub file_path: Option<Entity<InputState>>,
    pub target: KeyTargetSel,
}

#[derive(Default)]
pub struct AccessPage {
    pub wizard: Option<Wizard>,
    pub drawer: Option<Drawer>,
}

fn suggested_env_name(name: &str) -> String {
    let upper: String = name
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if upper.is_empty() {
        String::new()
    } else {
        format!("{upper}_API_KEY")
    }
}

fn access_items(app: &PanelApp) -> Vec<Value> {
    app.access
        .as_ref()
        .and_then(|a| a.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn raw_entry(app: &PanelApp, id: &str) -> Value {
    app.config
        .as_ref()
        .and_then(|c| c.get("upstreams"))
        .and_then(|u| u.get(id))
        .cloned()
        .unwrap_or(Value::Null)
}

/// 当前来源的推荐写入目标（抽屉缺省跟随现有来源，不默认覆盖）。
fn default_target(source: &str) -> KeyTargetSel {
    if source.contains("env") {
        KeyTargetSel::Env
    } else if source.contains("file") || source.contains("agent") {
        KeyTargetSel::File
    } else {
        KeyTargetSel::Store
    }
}

pub fn render(app: &mut PanelApp, window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let mut root = v_flex().id("access-root").gap_4().overflow_y_scroll();

    // ── 头部 ──
    let ent_open_official = cx.entity();
    let ent_open_custom = cx.entity();
    root = root.child(
        h_flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .child("接入管理"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("add-official")
                            .label("+ 官方厂商")
                            .primary()
                            .with_size(Size::Small)
                            .on_click(move |_, window, cx| {
                                ent_open_official.update(cx, |app, cx| {
                                    app.access_page.wizard =
                                        Some(open_wizard(WizardMode::Official, None, window, cx));
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("add-custom")
                            .label("+ 自定义 / 中转")
                            .ghost()
                            .with_size(Size::Small)
                            .on_click(move |_, window, cx| {
                                ent_open_custom.update(cx, |app, cx| {
                                    app.access_page.wizard =
                                        Some(open_wizard(WizardMode::Custom, None, window, cx));
                                    cx.notify();
                                });
                            }),
                    ),
            ),
    );

    // ── 列表 ──
    let items = access_items(app);
    if items.is_empty() {
        root = root.child(card(
            &theme,
            v_flex()
                .gap_2()
                .items_center()
                .py_8()
                .child(div().child(if app.online {
                    "还没有接入。点右上角添加。"
                } else {
                    "网关未连接。"
                })),
        ));
    } else {
        let mut list = v_flex().gap_2();
        for item in &items {
            let id = jstr(item, "/id");
            let label = jstr(item, "/label");
            let protocol = jstr(item, "/protocol");
            let source = jstr(item, "/auth/source");
            let models = jstr(item, "/models/declared");
            let ent_detail = cx.entity();
            let drawer_id = id.clone();
            let ent_probe = cx.entity();
            let ent_delete = cx.entity();
            let probe_id = id.clone();
            let delete_id = id.clone();
            list = list.child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .px_3()
                    .py_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(if label.is_empty() {
                                                id.clone()
                                            } else {
                                                format!("{id} · {label}")
                                            }),
                                    )
                                    .child(
                                        div()
                                            .px_1p5()
                                            .rounded_sm()
                                            .text_size(px(11.))
                                            .bg(theme.button)
                                            .text_color(theme.button_foreground)
                                            .child(protocol),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "密钥来源 {} · {} 个模型",
                                        if source.is_empty() { "—".to_string() } else { source },
                                        models,
                                    )),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("detail-{id}")))
                            .label("详情")
                            .ghost()
                            .with_size(Size::Small)
                            .on_click(move |_, window, cx| {
                                ent_detail.update(cx, |app, cx| {
                                    open_drawer(app, &drawer_id, window, cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("probe-{id}")))
                            .label("测试")
                            .ghost()
                            .with_size(Size::Small)
                            .on_click(move |_, _, cx| {
                                ent_probe.update(cx, |app, cx| {
                                    let _client = app.client.clone();
                                    let probe_id = probe_id.clone();
                                    app.run_api(
                                        cx,
                                        move |client| {
                                            client.post_empty(&format!(
                                                "/api/access/{probe_id}/probe"
                                            ))
                                        },
                                        |app, value, cx| {
                                            let ok =
                                                value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                                            let message = jstr(&value, "/message");
                                            if ok {
                                                app.toast_ok(format!("测试通过 {}", message), cx);
                                            } else {
                                                app.toast_err(format!("测试失败 {}", message), cx);
                                            }
                                        },
                                    );
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("delete-{id}")))
                            .label("删除")
                            .ghost()
                            .with_size(Size::Small)
                            .on_click(move |_, _, cx| {
                                ent_delete.update(cx, |app, cx| {
                                    let _client = app.client.clone();
                                    let delete_id = delete_id.clone();
                                    app.run_api(
                                        cx,
                                        move |client| {
                                            client.delete(&format!("/api/access/{delete_id}"))
                                        },
                                        |app, value, cx| {
                                            let refs = value
                                                .get("cleanedRefs")
                                                .and_then(Value::as_array)
                                                .cloned()
                                                .unwrap_or_default();
                                            app.toast_ok(
                                                if refs.is_empty() {
                                                    "接入已删除".to_string()
                                                } else {
                                                    format!("接入已删除；清理了 {} 处分组引用", refs.len())
                                                },
                                                cx,
                                            );
                                            app.refresh_access(cx);
                                            app.refresh_config(cx);
                                        },
                                    );
                                });
                            })),
            );
        }
        root = root.child(list);
    }

    // ── 覆盖层：向导 / 抽屉 ──
    if let Some(wizard) = app.access_page.wizard.clone() {
        root = root.child(render_wizard(app, wizard, window, cx));
    }
    if app.access_page.drawer.is_some() {
        root = root.child(render_drawer(app, window, cx));
    }

    root.into_any_element()
}

// ── 添加向导 ──────────────────────────────────────────────────────

fn open_wizard(mode: WizardMode, template_id: Option<String>, window: &mut Window, cx: &mut App) -> Wizard {
    let suggested = suggested_env_name(template_id.as_deref().unwrap_or(""));
    Wizard {
        mode,
        step: 1,
        template_id,
        name: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("上游标识，例如 deepseek")
        })),
        base_url: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://gateway.example.com/v1")
        })),
        vendor: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("自定义 / 备注")
        })),
        key: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("粘贴密钥（只写，不回显）")
        })),
        env_name: Some(cx.new(|cx| {
            InputState::new(window, cx).default_value(suggested)
        })),
        file_path: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("/home/you/.secrets/key.json")
        })),
        target: KeyTargetSel::Store,
        busy: false,
    }
}

fn render_wizard(app: &mut PanelApp, wizard: Wizard, _window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let Wizard {
        mode,
        step,
        template_id,
        ref name,
        ref base_url,
        ref vendor,
        ref key,
        ref env_name,
        ref file_path,
        target,
        busy,
    } = wizard;

    let mut body = v_flex().gap_3();
    match (mode, step) {
        (WizardMode::Official, 1) => {
            let ent_tpl = cx.entity();
            let templates = app
                .access
                .as_ref()
                .and_then(|a| a.get("templates"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut grid = v_flex().gap_2();
            for template in templates.iter().filter(|t| jstr(t, "/kind") == "official") {
                let tid = jstr(template, "/id");
                let selected = template_id.as_deref() == Some(tid.as_str());
                let label = jstr(template, "/label");
                let tpl_base = jstr(template, "/defaults/baseUrl");
                let protocol = jstr(template, "/defaults/protocol");
                let ent_tpl = ent_tpl.clone();
                let tid_click = tid.clone();
                grid = grid.child(
                    div()
                        .id(SharedString::from(format!("tpl-{tid}")))
                        .cursor_pointer()
                        .rounded_md()
                        .border_1()
                        .border_color(if selected { theme.accent } else { theme.border })
                        .bg(if selected { theme.accent.opacity(0.12) } else { theme.secondary })
                        .px_3()
                        .py_2()
                        .on_click(move |_, _, cx| {
                            ent_tpl.update(cx, |app, cx| {
                                if let Some(w) = app.access_page.wizard.as_mut() {
                                    w.template_id = Some(tid_click.clone());
                                }
                                cx.notify();
                            });
                        })
                        .child(
                            v_flex()
                                .gap_0p5()
                                .child(div().text_size(px(13.)).child(if label.is_empty() {
                                    tid.clone()
                                } else {
                                    format!("{tid} · {label}")
                                }))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{protocol} · {tpl_base}")),
                                ),
                        ),
                );
            }
            body = body.child(grid);
        }
        _ => {
            let mut form = v_flex().gap_2();
            if mode == WizardMode::Custom {
                if let Some(state) = name {
                    form = form.child(field("上游标识（必填）", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
                if let Some(state) = vendor {
                    form = form.child(field("厂商 / 备注", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
                if let Some(state) = base_url {
                    form = form.child(field("请求地址（必填）", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
            } else if let Some(state) = name {
                form = form.child(field("上游标识", theme.muted_foreground, Input::new(&state).h(px(34.))));
            }
            if let Some(state) = key {
                form = form.child(field("API Key", theme.muted_foreground, Input::new(&state).h(px(34.))));
            }

            // 写入目标四选一（互斥）。
            let ent_target = cx.entity();
            let mut target_row = h_flex().gap_2();
            for sel in [KeyTargetSel::Store, KeyTargetSel::Env, KeyTargetSel::File] {
                let ent_target = ent_target.clone();
                target_row = target_row.child(
                    Button::new(SharedString::from(format!("w-target-{}", sel.as_str())))
                        .label(sel.label())
                        .ghost()
                        .with_size(Size::Small)
                        .selected(target == sel)
                        .on_click(move |_, _, cx| {
                            ent_target.update(cx, |app, cx| {
                                if let Some(w) = app.access_page.wizard.as_mut() {
                                    w.target = sel;
                                }
                                cx.notify();
                            });
                        }),
                );
            }
            form = form
                .child(field_container(
                    "写入目标（互斥：保存后只保留所选来源）",
                    theme.muted_foreground,
                    target_row,
                ));
            if target == KeyTargetSel::Env {
                if let Some(state) = env_name {
                    form = form.child(field("环境变量名", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
            }
            if target == KeyTargetSel::File {
                if let Some(state) = file_path {
                    form = form.child(field("凭据文件路径（绝对路径）", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
            }
            body = body.child(form);
        }
    }

    // 底部按钮。
    let ent_back = cx.entity();
    let ent_cancel = cx.entity();
    let ent_next = cx.entity();
    let mut actions = h_flex().gap_2().justify_end();
    if step == 2 {
        actions = actions.child(
            Button::new("wiz-back")
                .label("上一步")
                .ghost()
                .with_size(Size::Small)
                .on_click(move |_, _, cx| {
                    ent_back.update(cx, |app, cx| {
                        if let Some(w) = app.access_page.wizard.as_mut() {
                            w.step = 1;
                        }
                        cx.notify();
                    });
                }),
        );
    }
    let next_label = if step == 1 {
        "下一步"
    } else if busy {
        "添加中…"
    } else {
        "添加并测试"
    };
    let next_disabled = busy || (mode == WizardMode::Official && step == 1 && template_id.is_none());
    actions = actions
        .child(
            Button::new("wiz-cancel")
                .label("取消")
                .ghost()
                .with_size(Size::Small)
                .on_click(move |_, _, cx| {
                    ent_cancel.update(cx, |app, cx| {
                        app.access_page.wizard = None;
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new("wiz-next")
                .label(next_label)
                .primary()
                .with_size(Size::Small)
                .disabled(next_disabled)
                .on_click(move |_, window, cx| {
                    ent_next.update(cx, |app, cx| {
                        handle_wizard_next(app, mode, window, cx);
                    });
                }),
        );

    // 遮罩 + 居中卡片。
    div()
        .absolute()
        .inset_0()
        .bg(theme.overlay)
        .flex()
        .items_center()
        .justify_center()
        .child(
            v_flex()
                .w(px(448.))
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .shadow_lg()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .child(match mode {
                            WizardMode::Official => "添加官方厂商",
                            WizardMode::Custom => "添加自定义 / 中转",
                        }),
                )
                .child(body)
                .child(actions),
        )
        .into_any_element()
}

fn handle_wizard_next(app: &mut PanelApp, mode: WizardMode, _window: &mut Window, cx: &mut Context<PanelApp>) {
    let Some(wizard) = app.access_page.wizard.clone() else { return };
    if mode == WizardMode::Official && wizard.step == 1 {
        if let Some(w) = app.access_page.wizard.as_mut() {
            w.step = 2;
        }
        cx.notify();
        return;
    }
    let (name, base_url, key, env_name, file_path) = {
        let read = |state: &Option<Entity<InputState>>| {
            state
                .as_ref()
                .map(|s| s.read(cx).value().trim().to_string())
                .unwrap_or_default()
        };
        (
            read(&wizard.name),
            read(&wizard.base_url),
            read(&wizard.key),
            read(&wizard.env_name),
            read(&wizard.file_path),
        )
    };
    let target = wizard.target;

    if name.is_empty() {
        app.toast_err("请填写上游标识", cx);
        return;
    }
    if mode == WizardMode::Custom && base_url.is_empty() {
        app.toast_err("请填写请求地址", cx);
        return;
    }
    if key.is_empty() {
        app.toast_err("请填写 API Key", cx);
        return;
    }
    if target == KeyTargetSel::Env && env_name.is_empty() {
        app.toast_err("请填写环境变量名", cx);
        return;
    }
    if target == KeyTargetSel::File && file_path.is_empty() {
        app.toast_err("请填写凭据文件路径", cx);
        return;
    }

    if let Some(w) = app.access_page.wizard.as_mut() {
        w.busy = true;
    }
    cx.notify();

    let mut body = serde_json::json!({
        "name": name,
        "apiKey": key,
        "keyTarget": target.as_str(),
    });
    if target == KeyTargetSel::Env {
        body["envName"] = serde_json::json!(env_name);
    }
    if target == KeyTargetSel::File {
        body["filePath"] = serde_json::json!(file_path);
        let format = if file_path.ends_with(".env") { "env" } else { "json" };
        body["fileFormat"] = serde_json::json!(format);
    }
    let path = match mode {
        WizardMode::Official => {
            body["templateId"] = serde_json::json!(wizard.template_id.clone().unwrap_or_default());
            "/api/access/quick".to_string()
        }
        WizardMode::Custom => {
            let vendor = wizard
                .vendor
                .as_ref()
                .map(|s| s.read(cx).value().trim().to_string())
                .unwrap_or_default();
            body["baseUrl"] = serde_json::json!(base_url);
            body["protocol"] = serde_json::json!("openai");
            if !vendor.is_empty() {
                body["vendor"] = serde_json::json!(vendor);
            }
            "/api/access/custom".to_string()
        }
    };
    let _client = app.client.clone();
    app.run_api(cx, move |client| client.post(&path, &body), move |app, value, cx| {
        let note = jstr(&value, "/credential/note");
        app.access_page.wizard = None;
        app.toast_ok(if note.is_empty() { "接入已创建".to_string() } else { note }, cx);
        app.refresh_access(cx);
        app.refresh_config(cx);
        app.refresh_all(cx);
        cx.notify();
    });
}

// ── 详情抽屉 ──────────────────────────────────────────────────────

fn open_drawer(app: &mut PanelApp, id: &str, window: &mut Window, cx: &mut App) {
    let raw = raw_entry(app, id);
    let source = jstr(&raw, "/_keySource");
    let target = default_target(&source);
    let env_name = jstr(&raw, "/apiKeyEnv");
    let file_path = jstr(&raw, "/apiKeyFile");
    app.access_page.drawer = Some(Drawer {
        id: id.to_string(),
        tab: DrawerTab::Overview,
        secret: Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("粘贴密钥（只写，不回显）")
        })),
        env_name: Some(cx.new(|cx| InputState::new(window, cx).default_value(env_name))),
        file_path: Some(cx.new(|cx| InputState::new(window, cx).default_value(file_path))),
        target,
    });
}

fn render_drawer(app: &mut PanelApp, _window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let Some(drawer) = app.access_page.drawer.clone() else {
        return div().into_any_element();
    };
    let items = access_items(app);
    let item = items
        .iter()
        .find(|i| jstr(i, "/id") == drawer.id)
        .cloned()
        .unwrap_or(Value::Null);

    // 页签。
    let ent_tab = cx.entity();
    let mut tabs = h_flex().gap_1();
    for (tab, label) in [
        (DrawerTab::Overview, "概览"),
        (DrawerTab::Credential, "凭据"),
        (DrawerTab::Models, "模型"),
        (DrawerTab::Diagnostics, "诊断"),
    ] {
        let ent_tab = ent_tab.clone();
        tabs = tabs.child(
            Button::new(SharedString::from(format!("dtab-{}", label)))
                .label(label)
                .ghost()
                .with_size(Size::Small)
                .selected(drawer.tab == tab)
                .on_click(move |_, _, cx| {
                    ent_tab.update(cx, |app, cx| {
                        if let Some(d) = app.access_page.drawer.as_mut() {
                            d.tab = tab;
                        }
                        cx.notify();
                    });
                }),
        );
    }

    let mut body = v_flex().gap_2();
    match drawer.tab {
        DrawerTab::Overview => {
            let rows = [
                ("协议", jstr(&item, "/protocol")),
                ("地址", jstr(&item, "/baseUrl")),
                ("模板", jstr(&item, "/template")),
                ("密钥来源", jstr(&item, "/auth/source")),
                ("凭据池", jstr(&item, "/auth/poolId")),
                ("隐私", jstr(&item, "/privacy/training")),
            ];
            for (k, v) in rows {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(80.))
                                .text_size(px(12.))
                                .text_color(theme.muted_foreground)
                                .child(k),
                        )
                        .child(div().text_size(px(13.)).child(if v.is_empty() { "—".to_string() } else { v })),
                );
            }
        }
        DrawerTab::Credential => {
            if let Some(state) = drawer.secret.clone() {
                body = body.child(field("API Key（只写，不回显）", theme.muted_foreground, Input::new(&state).h(px(34.))));
            }
            let ent_target = cx.entity();
            let mut target_row = h_flex().gap_2();
            for sel in [KeyTargetSel::Store, KeyTargetSel::Env, KeyTargetSel::File] {
                let ent_target = ent_target.clone();
                target_row = target_row.child(
                    Button::new(SharedString::from(format!("d-target-{}", sel.as_str())))
                        .label(sel.label())
                        .ghost()
                        .with_size(Size::Small)
                        .selected(drawer.target == sel)
                        .on_click(move |_, _, cx| {
                            ent_target.update(cx, |app, cx| {
                                if let Some(d) = app.access_page.drawer.as_mut() {
                                    d.target = sel;
                                }
                                cx.notify();
                            });
                        }),
                );
            }
            body = body.child(field_container(
                "写入目标（互斥：保存后只保留所选来源）",
                theme.muted_foreground,
                target_row,
            ));
            if drawer.target == KeyTargetSel::Env {
                if let Some(state) = drawer.env_name.clone() {
                    body = body.child(field("环境变量名", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
            }
            if drawer.target == KeyTargetSel::File {
                if let Some(state) = drawer.file_path.clone() {
                    body = body.child(field("凭据文件路径（绝对路径）", theme.muted_foreground, Input::new(&state).h(px(34.))));
                }
            }
            let note = match drawer.target {
                KeyTargetSel::Store => "密钥经 DPAPI 加密写入本机；env/file 引用会被移除（互斥）。",
                KeyTargetSel::Env => "密钥写入用户级环境变量（Windows HKCU / Linux shell rc 托管块），新终端自动继承。",
                KeyTargetSel::File => "密钥写入你指定的文件；配置只保存路径引用。",
            };
            body = body.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(note),
            );
            let ent_save = cx.entity();
            let drawer_id = drawer.id.clone();
            let target = drawer.target;
            body = body.child(
                Button::new("save-secret")
                    .label("保存密钥")
                    .primary()
                    .with_size(Size::Small)
                    .on_click(move |_, window, cx| {
                        ent_save.update(cx, |app, cx| {
                            save_secret(app, &drawer_id, target, window, cx);
                        });
                    }),
            );
        }
        DrawerTab::Models => {
            let raw = raw_entry(app, &drawer.id);
            let models = raw.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
            if models.is_empty() {
                body = body.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child("未声明模型（由上游目录自动拉取）。"),
                );
            }
            for m in models.iter().take(30) {
                body = body.child(div().text_size(px(12.)).child(m.as_str().unwrap_or("").to_string()));
            }
        }
        DrawerTab::Diagnostics => {
            let diagnostics = app
                .access
                .as_ref()
                .and_then(|a| a.get("diagnostics"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if diagnostics.is_empty() {
                body = body.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child("没有诊断问题。"),
                );
            }
            for d in diagnostics.iter().take(20) {
                let level = jstr(d, "/level");
                let path = jstr(d, "/path");
                let message = jstr(d, "/message");
                body = body.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(match level.as_str() {
                                    "error" => theme.danger,
                                    "warn" => theme.warning,
                                    _ => theme.muted_foreground,
                                })
                                .child(level),
                        )
                        .child(
                            v_flex()
                                .child(div().text_size(px(12.)).child(path))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.muted_foreground)
                                        .child(message),
                                ),
                        ),
                );
            }
        }
    }

    // 底部动作。
    let ent_probe = cx.entity();
    let ent_delete = cx.entity();
    let ent_close = cx.entity();
    let probe_id = drawer.id.clone();
    let delete_id = drawer.id.clone();
    let actions = h_flex()
        .gap_2()
        .justify_between()
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("d-probe")
                        .label("测试连接")
                        .ghost()
                        .with_size(Size::Small)
                        .on_click(move |_, _, cx| {
                            ent_probe.update(cx, |app, cx| {
                                let _client = app.client.clone();
                                let probe_id = probe_id.clone();
                                app.run_api(
                                    cx,
                                    move |client| {
                                        client.post_empty(&format!("/api/access/{probe_id}/probe"))
                                    },
                                    |app, value, cx| {
                                        let ok =
                                            value.get("ok").and_then(Value::as_bool).unwrap_or(false);
                                        let message = jstr(&value, "/message");
                                        if ok {
                                            app.toast_ok(format!("测试通过 {message}"), cx);
                                        } else {
                                            app.toast_err(format!("测试失败 {message}"), cx);
                                        }
                                    },
                                );
                            });
                        }),
                )
                .child(
                    Button::new("d-delete")
                        .label("删除接入")
                        .ghost()
                        .with_size(Size::Small)
                        .on_click(move |_, _, cx| {
                            ent_delete.update(cx, |app, cx| {
                                let _client = app.client.clone();
                                let delete_id = delete_id.clone();
                                app.run_api(
                                    cx,
                                    move |client| client.delete(&format!("/api/access/{delete_id}")),
                                    |app, value, cx| {
                                        let refs = value
                                            .get("cleanedRefs")
                                            .and_then(Value::as_array)
                                            .cloned()
                                            .unwrap_or_default();
                                        app.access_page.drawer = None;
                                        app.toast_ok(
                                            if refs.is_empty() {
                                                "接入已删除".to_string()
                                            } else {
                                                format!("接入已删除；清理了 {} 处分组引用", refs.len())
                                            },
                                            cx,
                                        );
                                        app.refresh_access(cx);
                                        app.refresh_config(cx);
                                        cx.notify();
                                    },
                                );
                            });
                        }),
                ),
        )
        .child(
            Button::new("d-close")
                .label("关闭")
                .ghost()
                .with_size(Size::Small)
                .on_click(move |_, _, cx| {
                    ent_close.update(cx, |app, cx| {
                        app.access_page.drawer = None;
                        cx.notify();
                    });
                }),
        );

    v_flex()
        .id("drawer-scroll")
        .w(px(420.))
        .flex_shrink_0()
        .h_full()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.background)
        .p_4()
        .overflow_y_scroll()
        .gap_3()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .child(format!("接入 {}", drawer.id)),
        )
        .child(tabs)
        .child(body)
        .child(div().mt_auto().child(actions))
        .into_any_element()
}

fn save_secret(app: &mut PanelApp, id: &str, target: KeyTargetSel, window: &mut Window, cx: &mut Context<PanelApp>) {
    let Some(drawer) = app.access_page.drawer.clone() else { return };
    let read = |state: &Option<Entity<InputState>>| {
        state
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_string())
            .unwrap_or_default()
    };
    let key = read(&drawer.secret);
    let env_name = read(&drawer.env_name);
    let file_path = read(&drawer.file_path);
    if key.is_empty() {
        app.toast_err("请先粘贴密钥", cx);
        return;
    }
    if target == KeyTargetSel::Env && env_name.is_empty() {
        app.toast_err("请填写环境变量名", cx);
        return;
    }
    if target == KeyTargetSel::File && file_path.is_empty() {
        app.toast_err("请填写凭据文件路径", cx);
        return;
    }
    let mut body = serde_json::json!({ "apiKey": key, "target": target.as_str() });
    if target == KeyTargetSel::Env {
        body["envName"] = serde_json::json!(env_name);
    }
    if target == KeyTargetSel::File {
        body["filePath"] = serde_json::json!(file_path);
        let format = if file_path.ends_with(".env") { "env" } else { "json" };
        body["fileFormat"] = serde_json::json!(format);
    }
    let path = format!("/api/access/{id}/secret");
    let _client = app.client.clone();
    if let Some(state) = drawer.secret.clone() {
        state.update(cx, |s, cx| s.set_value("".to_string(), window, cx));
    }
    app.run_api(cx, move |client| client.post(&path, &body), move |app, value, cx| {
        let note = jstr(&value, "/credential/note");
        app.toast_ok(if note.is_empty() { "密钥已保存".to_string() } else { note }, cx);
        app.refresh_access(cx);
        app.refresh_config(cx);
        cx.notify();
    });
}

// ── 表单小部件 ────────────────────────────────────────────────────

fn field(label: &str, muted: gpui_kit::Hsla, input: impl IntoElement) -> Div {
    field_container(label, muted, div().w_full().child(input))
}

fn field_container(label: &str, muted: gpui_kit::Hsla, control: impl IntoElement) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_size(px(12.)).text_color(muted).child(label.to_string()))
        .child(div().w_full().child(control))
}
