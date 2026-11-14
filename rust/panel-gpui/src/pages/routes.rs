//! 路由与分组：可见上游矩阵、命名分组（/unified/{组}/v1）、路由组（group/<id>）。
//!
//! 设计意图（config.rs 文档）：路由组成员的合法写法是 `上游/模型`、
//! 已声明的模型名、别名；裸上游名不是合法成员。成员选择器只给
//! `上游/模型` 组合，自由输入留给模型名 / 别名。

use super::{h_flex, v_flex, card, jstr};
use crate::app::PanelApp;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, button::{Button, ButtonVariants}};
use gpui_kit::*;
use serde_json::Value;

/// 每个上游的 `上游/模型` 候选（models + modelMap 键）。
pub fn qualified_members(config: &Value) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let Some(ups) = config.get("upstreams").and_then(Value::as_object) else {
        return out;
    };
    for (name, row) in ups {
        let mut items = Vec::new();
        if let Some(models) = row.get("models").and_then(Value::as_array) {
            for m in models {
                if let Some(s) = m.as_str() {
                    if !s.trim().is_empty() {
                        items.push(format!("{name}/{}", s.trim()));
                    }
                }
            }
        }
        if let Some(map) = row.get("modelMap").and_then(Value::as_object) {
            for key in map.keys() {
                let candidate = format!("{name}/{key}");
                if !items.contains(&candidate) {
                    items.push(candidate);
                }
            }
        }
        if !items.is_empty() {
            out.push((name.clone(), items));
        }
    }
    out
}

#[derive(Clone)]
pub struct RouteGroupDraft {
    pub id: String,
    pub members: Vec<String>,
    pub add_input: Option<Entity<InputState>>,
}

#[derive(Clone)]
pub struct UnifiedGroupDraft {
    pub id: String,
    pub label: String,
    pub upstreams: String,
}

#[derive(Default)]
pub struct RoutesPage {
    pub dirty: bool,
    pub route_groups: Vec<RouteGroupDraft>,
    pub unified_groups: Vec<UnifiedGroupDraft>,
    pub new_group_id: Option<Entity<InputState>>,
    pub member_filter: Option<Entity<InputState>>,
    pub saving: bool,
}

fn route_groups_from(config: &Value) -> Vec<RouteGroupDraft> {
    let mut out = Vec::new();
    if let Some(obj) = config.get("groups").and_then(Value::as_object) {
        for (id, members) in obj {
            out.push(RouteGroupDraft {
                id: id.clone(),
                members: members
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                add_input: None,
            });
        }
    }
    out
}

pub fn render(app: &mut PanelApp, window: &mut Window, cx: &mut Context<PanelApp>) -> AnyElement {
    let theme = cx.theme().colors.clone();
    let entity = cx.entity();
    let config = app.config.clone().unwrap_or(Value::Null);

    // 进入页面时从最新 config 装载一次草稿（输入实体需要 window，这里创建）。
    if app.routes_page.route_groups.is_empty() && app.routes_page.unified_groups.is_empty() {
        let unified_groups = config
            .get("unified")
            .and_then(|u| u.get("groups"))
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| UnifiedGroupDraft {
                        id: jstr(row, "/id"),
                        label: jstr(row, "/label"),
                        upstreams: row
                            .get("upstreams")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let new_group_id = Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("新组标识，例如 fast")
        }));
        let member_filter = Some(cx.new(|cx| {
            InputState::new(window, cx).placeholder("按名称过滤候选成员")
        }));
        let page = &mut app.routes_page;
        page.unified_groups = unified_groups;
        page.route_groups = route_groups_from(&config);
        page.new_group_id = new_group_id;
        page.member_filter = member_filter;
    }

    let mut root = v_flex().id("routes-root").gap_4().overflow_y_scroll();

    // ── 头部 ──
    let entity_reload = entity.clone();
    let entity_save = entity.clone();
    let dirty = app.routes_page.dirty;
    let saving = app.routes_page.saving;
    root = root.child(
        h_flex()
            .items_center()
            .justify_between()
            .child(
                h_flex().gap_2().items_center()
                    .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("路由与分组"))
                    .child(div().text_size(px(11.)).text_color(theme.muted_foreground)
                        .child(if dirty { "有未保存修改" } else { "保存即热重载" })),
            )
            .child(
                h_flex().gap_2()
                    .child(Button::new("routes-reload").label("放弃修改").ghost().with_size(gpui_kit::component::Size::Small)
                        .disabled(!dirty)
                        .on_click(move |_, _, cx| {
                            let entity = entity_reload.clone();
                            entity.update(cx, |app, cx| {
                                app.routes_page = RoutesPage::default();
                                app.refresh_config(cx);
                                cx.notify();
                            });
                        }))
                    .child(Button::new("routes-save").label(if saving { "保存中…" } else { "保存路由" })
                        .primary().with_size(gpui_kit::component::Size::Small).disabled(!dirty || saving)
                        .on_click(move |_, _, cx| {
                            let entity = entity_save.clone();
                            entity.update(cx, |app, cx| save_routes(app, cx));
                        })),
            ),
    );

    // ── 可见上游矩阵 ──
    let visible: Vec<String> = config
        .get("unified")
        .and_then(|u| u.get("visibleUpstreams"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let upstream_names: Vec<String> = config
        .get("upstreams")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut matrix = v_flex().gap_1();
    if upstream_names.is_empty() {
        matrix = matrix.child(div().text_size(px(12.)).text_color(theme.muted_foreground).child("还没有上游。先在接入管理添加。"));
    }
    for name in &upstream_names {
        let entity = entity.clone();
        let name2 = name.clone();
        let checked = visible.is_empty() || visible.contains(name);
        matrix = matrix.child(
            h_flex().items_center().gap_2().py_0p5()
                .child(div().text_size(px(13.)).child(name.clone()))
                .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(format!("/{name}/v1")))
                .child(
                    Button::new(SharedString::from(format!("vis-{name}")))
                        .label(if checked { "可见" } else { "隐藏" })
                        .ghost()
                        .with_size(gpui_kit::component::Size::XSmall)
                        .on_click(move |_, _, cx| {
                            let entity = entity.clone();
                            let name = name2.clone();
                            entity.update(cx, |app, cx| {
                                toggle_visible(app, &name);
                                cx.notify();
                            });
                        }),
                ),
        );
    }
    root = root.child(card(&theme, v_flex().gap_2()
        .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child("统一入口可见上游"))
        .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child("/unified/v1 只展示白名单内的上游；留空 = 全部可见。"))
        .child(matrix)));

    // ── 路由组 ──
    let route_groups = app.routes_page.route_groups.clone();
    let mut groups_area = v_flex().gap_3();
    if route_groups.is_empty() {
        groups_area = groups_area.child(
            div().text_size(px(12.)).text_color(theme.muted_foreground).child(
                "还没有路由组。填写新组标识创建；对外暴露虚拟模型 group/<id>。",
            ),
        );
    }
    let candidates = qualified_members(&config);
    for (_index, group) in route_groups.iter().enumerate() {
        let entity = entity.clone();
        let gid = group.id.clone();
        let mut card_body = v_flex().gap_2();

        // 成员有序列表。
        if group.members.is_empty() {
            card_body = card_body.child(
                div().text_size(px(12.)).text_color(theme.muted_foreground).child("还没有成员。"),
            );
        }
        for (mi, member) in group.members.iter().enumerate() {
            let entity_up = entity.clone();
            let gid_up = gid.clone();
            let entity_down = entity.clone();
            let gid_down = gid.clone();
            let entity_rm = entity.clone();
            let gid_rm = gid.clone();
            let member_rm = member.clone();
            let hint = match member.split_once('/') {
                Some((up, _model)) if upstream_names.contains(&up.to_string()) => {
                    format!("上游 {up}")
                }
                Some(_) => "上游未声明".to_string(),
                None => "模型名 / 别名 · 未锁定上游".to_string(),
            };
            card_body = card_body.child(
                h_flex().items_center().gap_2().py_0p5()
                    .child(div().w(px(20.)).text_size(px(11.)).text_color(theme.muted_foreground).child(format!("{}", mi + 1)))
                    .child(div().text_size(px(13.)).flex_1().child(member.clone()))
                    .child(div().text_size(px(11.)).text_color(if hint == "上游未声明" { theme.danger } else { theme.muted_foreground }).child(hint))
                    .child(Button::new(SharedString::from(format!("up-{gid}-{mi}"))).label("↑").ghost().with_size(gpui_kit::component::Size::XSmall)
                        .disabled(mi == 0)
                        .on_click(move |_, _, cx| {
                            let entity = entity_up.clone();
                            let gid = gid_up.clone();
                            let mi = mi;
                            entity.update(cx, |app, cx| {
                                move_member(app, &gid, mi, -1);
                                cx.notify();
                            });
                        }))
                    .child(Button::new(SharedString::from(format!("down-{gid}-{mi}"))).label("↓").ghost().with_size(gpui_kit::component::Size::XSmall)
                        .on_click(move |_, _, cx| {
                            let entity = entity_down.clone();
                            let gid = gid_down.clone();
                            let mi = mi;
                            entity.update(cx, |app, cx| {
                                move_member(app, &gid, mi, 1);
                                cx.notify();
                            });
                        }))
                    .child(Button::new(SharedString::from(format!("rm-{gid}-{mi}"))).label("移除").ghost().with_size(gpui_kit::component::Size::XSmall)
                        .on_click(move |_, _, cx| {
                            let entity = entity_rm.clone();
                            let gid = gid_rm.clone();
                            let member = member_rm.clone();
                            entity.update(cx, |app, cx| {
                                remove_member(app, &gid, &member);
                                cx.notify();
                            });
                        })),
            );
        }

        // 成员候选（按过滤词过滤后的 `上游/模型` chips）。
        let filter = app
            .routes_page
            .member_filter
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        let mut chips = h_flex().gap_1().flex_wrap();
        let mut shown = 0usize;
        for (up, items) in &candidates {
            let _up = up.clone();
            for item in items {
                if group.members.contains(item) {
                    continue;
                }
                if !filter.is_empty() && !item.to_lowercase().contains(&filter) {
                    continue;
                }
                if shown >= 16 {
                    break;
                }
                shown += 1;
                let entity_c = entity.clone();
                let gid_c = gid.clone();
                let item_c = item.clone();
                chips = chips.child(
                    Button::new(SharedString::from(format!("add-{}-{}", gid, item)))
                        .label(item.clone())
                        .ghost()
                        .with_size(gpui_kit::component::Size::XSmall)
                        .on_click(move |_, _, cx| {
                            let entity = entity_c.clone();
                            let gid = gid_c.clone();
                            let item = item_c.clone();
                            entity.update(cx, |app, cx| {
                                add_member(app, &gid, item);
                                cx.notify();
                            });
                        }),
                );
            }
        }
        let candidates_block = v_flex().gap_1()
            .child(h_flex().gap_2().items_center()
                .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child("添加成员（上游/模型）"))
                .child(div().flex_1()))
            .child(chips)
            .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(
                "也可以在下方输入模型名 / 别名（裸上游名不是合法成员）。",
            ));

        // 自由输入（模型名 / 别名）。
        let add_input = group.add_input.as_ref().map(|s| Input::new(s));
        let entity_a = entity.clone();
        let gid_a = gid.clone();
        let mut add_row = h_flex().gap_2().items_center();
        if let Some(input) = add_input {
            add_row = add_row.child(div().flex_1().child(input));
        }
        add_row = add_row.child(
            Button::new(SharedString::from(format!("addm-{gid}"))).label("加入").ghost().with_size(gpui_kit::component::Size::XSmall)
                .on_click(move |_, window, cx| {
                    let entity = entity_a.clone();
                    let gid = gid_a.clone();
                    entity.update(cx, |app, cx| {
                        let Some(group) = app.routes_page.route_groups.iter_mut().find(|g| g.id == gid) else { return };
                        let value = group.add_input.as_ref().map(|s| s.read(cx).value().trim().to_string()).unwrap_or_default();
                        if value.is_empty() {
                            return;
                        }
                        if !group.members.contains(&value) {
                            group.members.push(value);
                            if let Some(state) = group.add_input.clone() {
                                state.update(cx, |s, cx| s.set_value("".to_string(), window, cx));
                            }
                            app.routes_page.dirty = true;
                        }
                        cx.notify();
                    });
                }),
        );

        // 删除组。
        let entity_d = entity.clone();
        let gid_d = gid.clone();
        card_body = card_body
            .child(candidates_block)
            .child(add_row)
            .child(h_flex().justify_end().child(
                Button::new(SharedString::from(format!("del-{gid}"))).label("删除组").ghost().with_size(gpui_kit::component::Size::XSmall)
                    .on_click(move |_, _, cx| {
                        let entity = entity_d.clone();
                        let gid = gid_d.clone();
                        entity.update(cx, |app, cx| {
                            app.routes_page.route_groups.retain(|g| g.id != gid);
                            app.routes_page.dirty = true;
                            cx.notify();
                        });
                    }),
            ));

        groups_area = groups_area.child(
            card(&theme, v_flex().gap_2()
                .child(h_flex().items_center().gap_2()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(format!("group/{}", group.id)))
                    .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(format!("{} 个成员 · 按顺序尝试", group.members.len()))))
                .child(card_body)),
        );
    }

    // 新建组。
    let new_group_input = app.routes_page.new_group_id.as_ref().map(|s| Input::new(s));
    let entity_n = entity.clone();
    let mut new_group_row = h_flex().gap_2().items_center();
    if let Some(input) = new_group_input {
        new_group_row = new_group_row.child(div().w(px(192.)).child(input));
    }
    new_group_row = new_group_row.child(
        Button::new("create-group").label("+ 新建路由组").ghost().with_size(gpui_kit::component::Size::Small).on_click(move |_, _, cx| {
            let entity = entity_n.clone();
            entity.update(cx, |app, cx| {
                let Some(state) = app.routes_page.new_group_id.clone() else { return };
                let id = state.read(cx).value().trim().to_string();
                if id.is_empty()
                    || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
                {
                    app.toast_err("组标识只允许字母、数字、- _ .", cx);
                    return;
                }
                if app.routes_page.route_groups.iter().any(|g| g.id == id) {
                    app.toast_err("组标识已存在", cx);
                    return;
                }
                app.routes_page.route_groups.push(RouteGroupDraft { id, members: Vec::new(), add_input: None });
                app.routes_page.dirty = true;
                cx.notify();
            });
        }),
    );

    let entity_f = entity.clone();
    let filter_input = app.routes_page.member_filter.as_ref().map(|s| Input::new(s));
    let mut filter_row = h_flex().gap_2().items_center();
    if let Some(input) = filter_input {
        filter_row = filter_row.child(div().w(px(192.)).child(input));
    }
    filter_row = filter_row.child(
        Button::new("apply-filter").label("刷新候选").ghost().with_size(gpui_kit::component::Size::XSmall).on_click(move |_, _, cx| {
            let entity = entity_f.clone();
            entity.update(cx, |_, cx| cx.notify());
        }),
    );

    root = root.child(card(&theme, v_flex().gap_3()
        .child(h_flex().items_center().gap_2()
            .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child("路由组（失败按成员顺序切换）"))
            .child(filter_row))
        .child(groups_area)
        .child(new_group_row)));

    // ── 命名分组（统一入口） ──
    let mut unified_area = v_flex().gap_2();
    let unified_groups = app.routes_page.unified_groups.clone();
    for (ui, group) in unified_groups.iter().enumerate() {
        let entity_u = entity.clone();
        let ui_s = ui;
        unified_area = unified_area.child(
            h_flex().gap_2().items_center()
                .child(div().w(px(96.)).text_size(px(12.)).child(format!("/unified/{}/v1", group.id)))
                .child(div().flex_1().text_size(px(12.)).text_color(theme.muted_foreground).child(
                    if group.upstreams.is_empty() { "—".to_string() } else { group.upstreams.clone() },
                ))
                .child(Button::new(SharedString::from(format!("rmu-{ui}"))).label("移除").ghost().with_size(gpui_kit::component::Size::XSmall)
                    .on_click(move |_, _, cx| {
                        let entity = entity_u.clone();
                        let ui = ui_s;
                        entity.update(cx, |app, cx| {
                            app.routes_page.unified_groups.remove(ui);
                            app.routes_page.dirty = true;
                            cx.notify();
                        });
                    })),
        );
    }
    root = root.child(card(&theme, v_flex().gap_2()
        .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child("命名分组 /unified/{组}/v1"))
        .child(div().text_size(px(11.)).text_color(theme.muted_foreground).child(
            "成员为上游名清单（在配置文件或浏览器面板里细化）；此处仅展示与移除。",
        ))
        .child(unified_area)));

    root.into_any_element()
}

// ── 草稿编辑动作 ──────────────────────────────────────────────────

fn toggle_visible(app: &mut PanelApp, name: &str) {
    // 直接改 config 缓存里的 unified.visibleUpstreams（保存时整体提交）。
    let Some(config) = app.config.as_mut() else { return };
    let Some(obj) = config.as_object_mut() else { return };
    let unified = obj.entry("unified".to_string()).or_insert_with(|| serde_json::json!({}));
    let Some(unified) = unified.as_object_mut() else { return };
    let list = unified
        .entry("visibleUpstreams".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if let Some(arr) = list.as_array_mut() {
        if let Some(pos) = arr.iter().position(|v| v.as_str() == Some(name)) {
            arr.remove(pos);
        } else {
            arr.push(serde_json::json!(name));
        }
    }
    app.routes_page.dirty = true;
}

fn add_member(app: &mut PanelApp, gid: &str, member: String) {
    let Some(group) = app.routes_page.route_groups.iter_mut().find(|g| g.id == gid) else { return };
    if !group.members.contains(&member) {
        group.members.push(member);
        app.routes_page.dirty = true;
    }
}

fn remove_member(app: &mut PanelApp, gid: &str, member: &str) {
    let Some(group) = app.routes_page.route_groups.iter_mut().find(|g| g.id == gid) else { return };
    group.members.retain(|m| m != member);
    app.routes_page.dirty = true;
}

fn move_member(app: &mut PanelApp, gid: &str, index: usize, delta: i32) {
    let Some(group) = app.routes_page.route_groups.iter_mut().find(|g| g.id == gid) else { return };
    let to = index as i32 + delta;
    if to < 0 || to as usize >= group.members.len() {
        return;
    }
    group.members.swap(index, to as usize);
    app.routes_page.dirty = true;
}

/// 保存：拉最新 config → 合并本页草稿 → 校验 → 落盘。
fn save_routes(app: &mut PanelApp, cx: &mut Context<PanelApp>) {
    app.routes_page.saving = true;
    cx.notify();
    let client = app.client.clone();
    let unified_groups: Vec<Value> = app
        .routes_page
        .unified_groups
        .iter()
        .map(|g| {
            serde_json::json!({
                "id": g.id,
                "label": g.label,
                "upstreams": g.upstreams.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let route_groups: Value = serde_json::json!(app
        .routes_page
        .route_groups
        .iter()
        .map(|g| (g.id.clone(), g.members.clone()))
        .collect::<std::collections::BTreeMap<String, Vec<String>>>());
    let visible: Vec<String> = app
        .config
        .as_ref()
        .and_then(|c| c.get("unified"))
        .and_then(|u| u.get("visibleUpstreams"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    cx.spawn(async move |this, cx| {
        let outcome = cx
            .background_executor()
            .spawn(async move {
                let fresh = client.get("/api/config")?;
                let mut config = fresh
                    .get("config")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("配置响应缺少 config 字段"))?;
                let obj = config
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("配置不是 JSON 对象"))?;
                let unified = obj
                    .entry("unified".to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(u) = unified.as_object_mut() {
                    u.insert("visibleUpstreams".into(), serde_json::json!(visible));
                    u.insert("groups".into(), serde_json::json!(unified_groups));
                }
                obj.insert("groups".into(), route_groups);
                let validate = client.post("/api/config/validate", &config)?;
                let errors: Vec<Value> = validate
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter(|d| d.get("level").and_then(Value::as_str) == Some("error"))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if !errors.is_empty() {
                    let first = errors
                        .first()
                        .map(|d| {
                            format!(
                                "{} {}",
                                d.get("path").and_then(Value::as_str).unwrap_or(""),
                                d.get("message").and_then(Value::as_str).unwrap_or(""),
                            )
                        })
                        .unwrap_or_default();
                    return Err(anyhow::anyhow!("校验未通过（{}/{} 处错误）：{}", errors.len(), errors.len(), first));
                }
                client.post("/api/config", &serde_json::json!({ "config": config, "backup": true }))?;
                Ok(errors.len())
            })
            .await;
        this.update(cx, |app, cx| {
            app.routes_page.saving = false;
            match outcome {
                Ok(_) => {
                    app.routes_page.dirty = false;
                    app.routes_page = RoutesPage::default();
                    app.toast_ok("路由配置已保存并热重载", cx);
                    app.refresh_config(cx);
                }
                Err(err) => {
                    app.toast_err(crate::api::brief_error(err), cx);
                }
            }
            cx.notify();
        })
        .ok();
    })
    .detach();
}
