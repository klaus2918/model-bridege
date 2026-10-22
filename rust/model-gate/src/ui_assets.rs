//! 面板静态资源一致性断言：页签与面板一一对应、JS 引用的 id 存在、
//! JS 调用的后端路由已注册、关键 CSS 类已定义。
//!
//! 为什么放在 Rust 侧：面板资源已随 `rust-only-autoupdate` 迁到
//! `rust/model-gate/ui/`，由 Rust 内核 `include_str!` 内嵌并对外提供，
//! 断言必须与实际提供资源的宿主同源。

use std::collections::BTreeSet;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const FREE_JS: &str = include_str!("../ui/free_sources.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");
const PROXY_RS: &str = include_str!("proxy.rs");

/// 取 HTML 里的 `id="..."`。
fn html_ids() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = INDEX_HTML;
    while let Some(pos) = rest.find("id=\"") {
        rest = &rest[pos + 4..];
        let end = rest.find('"').unwrap_or(0);
        out.insert(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// 取 JS 里 `$("#...")` / `getElementById("...")` 引用的 id。
///
/// 只取**纯 id**：`$("#x tbody")` 是 CSS 选择器，取的是首个片段 `x`。
fn js_ids(js: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = js;
    while let Some(pos) = rest.find("$(\"#") {
        rest = &rest[pos + 4..];
        let end = rest.find('"').unwrap_or(0);
        let raw = &rest[..end];
        // 选择器形态：`#id tbody` → 只取第一段（纯 id）。
        let first = raw.split_whitespace().next().unwrap_or("");
        if !first.is_empty() {
            out.insert(first.to_string());
        }
        rest = &rest[end..];
    }
    let mut rest = js;
    while let Some(pos) = rest.find("getElementById(\"") {
        rest = &rest[pos + 15..];
        let end = rest.find('"').unwrap_or(0);
        out.insert(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// 取 JS 里 `api("...")` / `postJson("...")` 调用的路径。
fn js_api_paths(js: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for marker in ["api(\"", "postJson(\""] {
        let mut rest = js;
        while let Some(pos) = rest.find(marker) {
            rest = &rest[pos + marker.len()..];
            let end = rest.find('"').unwrap_or(0);
            let raw = &rest[..end];
            // 只取字面量路径（模板字符串里的插值路径另有断言）。
            if !raw.contains("${") {
                out.insert(raw.split('?').next().unwrap_or(raw).to_string());
            }
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn tabs_match_panels() {
    let mut tabs = BTreeSet::new();
    let mut rest = INDEX_HTML;
    while let Some(pos) = rest.find("data-tab=\"") {
        rest = &rest[pos + 10..];
        let end = rest.find('"').unwrap_or(0);
        tabs.insert(rest[..end].to_string());
        rest = &rest[end..];
    }
    let mut panels = BTreeSet::new();
    let mut rest = INDEX_HTML;
    while let Some(pos) = rest.find("id=\"panel-") {
        rest = &rest[pos + 10..];
        let end = rest.find('"').unwrap_or(0);
        panels.insert(rest[..end].to_string());
        rest = &rest[end..];
    }
    assert_eq!(tabs, panels, "页签与面板必须一一对应");
    for expected in [
        "overview", "model", "config", "usage", "prompts", "free", "logs",
    ] {
        assert!(tabs.contains(expected), "缺少页签 {expected}");
    }
}

#[test]
fn js_ids_resolve_in_html() {
    let ids = html_ids();
    let mut missing: Vec<String> = js_ids(APP_JS)
        .into_iter()
        .chain(js_ids(FREE_JS))
        // 既有例外：更新弹窗容器由 JS 运行时插入，且调用处显式做了
        // `if (!host) return;` 存在性保护，静态 HTML 里没有该节点。
        .filter(|id| id != "update-modal")
        .filter(|id| !ids.contains(id))
        .collect();
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "JS 引用了 HTML 中不存在的 id: {missing:?}"
    );
}

#[test]
fn free_source_ids_exist_in_html() {
    let ids = html_ids();
    for required in [
        "panel-free",
        "free-sources",
        "free-models-box",
        "free-model-table",
        "free-refresh",
        "free-probe-all",
        "free-probe-hint",
        "free-model-hint",
        "free-models-title",
        "free-models-meta",
    ] {
        assert!(ids.contains(required), "免费来源页缺少元素 {required}");
    }
}

#[test]
fn free_source_routes_are_registered_in_backend() {
    let paths = js_api_paths(FREE_JS);
    assert!(!paths.is_empty(), "免费来源脚本未调用任何后端接口");
    for path in &paths {
        assert!(
            PROXY_RS.contains(path.as_str()),
            "前端调用了未注册的路由 {path}"
        );
    }
}

/// 主面板（app.js）调用的每个后端接口都必须在 Rust 侧注册。
///
/// 历史缺口（config-as-json）：前端调用的接口若在 Rust 侧没注册，
/// 面板会 404/5xx 而无人发现 —— 当时只有 Python 侧的路由注册被测到。
/// Rust-only 后本文件成为唯一的面板契约守卫，这条断言不能缺。
#[test]
fn app_routes_are_registered_in_backend() {
    let paths = js_api_paths(APP_JS);
    assert!(!paths.is_empty(), "主面板未调用任何后端接口");
    let mut missing: Vec<String> = paths
        .into_iter()
        .filter(|p| p.starts_with("/api/"))
        .filter(|p| !PROXY_RS.contains(p.as_str()))
        .collect();
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "主面板调用了 Rust 未注册的接口: {missing:?}"
    );
}

/// 面板必须提供「配置提醒」入口：折叠摘要头 + 检查按钮 + 不再提示开关。
///
/// 提醒区是用户明确要求「可关闭、不占主展示区」的能力，缺任何一项都算回退。
#[test]
fn config_issues_panel_controls_present() {
    for id in [
        "config-issues",
        "config-issues-body",
        "config-issues-toggle",
        "config-issues-summary",
        "config-issues-check",
        "config-issues-mute",
    ] {
        assert!(html_ids().contains(id), "配置提醒区缺少元素 {id}");
    }
    for wiring in [
        "renderConfigIssues",
        "setIssuesExpanded",
        "focusIssueTarget",
        "issueSignature",
        "ISSUE_MUTE_KEY",
    ] {
        assert!(APP_JS.contains(wiring), "app.js 未接线 {wiring}");
    }
    // 单条关闭按钮由渲染函数产出
    assert!(APP_JS.contains("issue-close"), "提醒条目缺少单条关闭按钮");
    for cls in [
        ".issues-head",
        ".issues-toggle",
        ".issues-body",
        ".issue-close",
        ".issue.info",
    ] {
        assert!(STYLE_CSS.contains(cls), "style.css 缺少 {cls}");
    }
}

/// 提醒区必须位于保存按钮行之前（不占主展示区）。
///
/// 位置是产品要求：填表时提醒不能挤在表单顶部。
#[test]
fn config_issues_precede_save_actions_in_dom() {
    let issues = INDEX_HTML
        .find("id=\"config-issues\"")
        .expect("缺少 config-issues");
    let actions = INDEX_HTML
        .find("class=\"actions sticky-actions\"")
        .expect("缺少 sticky-actions");
    assert!(
        issues < actions,
        "提醒区必须排在保存按钮行之前（实际 {issues} vs {actions}）"
    );
}

#[test]
fn free_source_endpoints_exist_in_router() {
    for route in [
        "/api/free-sources",
        "/api/free-sources/catalog",
        "/api/free-sources/probe",
        "/api/free-sources/probe-all",
        "/api/free-sources/attach",
    ] {
        assert!(PROXY_RS.contains(route), "后端缺少路由 {route}");
    }
}

#[test]
fn free_source_script_is_served() {
    // 资源必须被 ui_asset 白名单放行，否则页面拿不到脚本。
    assert!(
        PROXY_RS.contains("free_sources.js"),
        "ui_asset 未放行 free_sources.js"
    );
    assert!(
        INDEX_HTML.contains("free_sources.js"),
        "index.html 未引入 free_sources.js"
    );
}

#[test]
fn key_css_classes_are_defined() {
    for class in [
        "free-source-list",
        "free-source-card",
        "badge",
        "callout",
        "box",
        "grid-2",
        "detail-item",
        "actions",
    ] {
        assert!(
            STYLE_CSS.contains(&format!(".{class}")),
            "CSS 缺少 .{class}"
        );
    }
}

#[test]
fn free_source_badge_tones_exist() {
    // 前端只允许使用 style.css 里已定义的徽标变体。
    for tone in ["ok", "warn", "bad"] {
        assert!(
            STYLE_CSS.contains(&format!(".badge.{tone}")),
            "CSS 缺少 .badge.{tone}"
        );
    }
    assert!(
        !FREE_JS.contains("\"brand\""),
        "免费来源脚本使用了不存在的徽标变体 brand"
    );
}
