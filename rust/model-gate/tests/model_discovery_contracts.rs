//! 面板「查询清单」能力的静态契约守卫。
//!
//! 对应变更 upstream-model-discovery。
//!
//! 保证不会出现：按钮没渲染、事件没接通、样式类缺失、
//! 前端调用的接口未注册、面板逻辑与实现漂移。
//!
//! 历史背景（config-as-json / rust-only-autoupdate）：契约守卫原先在
//! `tests/test_ui_assets.py`（Python 侧）。Rust-only 改造后 Python 测试目录
//! 已被删除，契约守卫迁到本文件（Rust 是唯一产品实现）。

use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn ui_dir() -> PathBuf {
    crate_root().join("ui")
}

fn read_ui(name: &str) -> String {
    fs::read_to_string(ui_dir().join(name)).expect("read ui asset")
}

fn src_text(name: &str) -> String {
    fs::read_to_string(crate_root().join("src").join(name)).expect("read rust source")
}

/// 面板里必须存在的「查询清单」相关标记。
fn assert_panel_markers(js: &str, label: &str) {
    for marker in [
        "data-cfg-act=\"query-models\"", // 表格行入口（用户可见）
        "data-act=\"query-models\"",     // 编辑弹窗内的按钮
        "function upstreamFormValues(",
        "async function openModelDiscovery(",
        "async function applyModelDiscovery(",
        "panel.className = \"model-discovery\"", // 内嵌面板（不另开弹窗）
        "query-models",                          // 事件接线
    ] {
        assert!(js.contains(marker), "{label} 面板缺少标记: {marker}");
    }
}

#[test]
fn query_models_controls_exist_in_panel() {
    let js = read_ui("app.js");
    assert_panel_markers(&js, "面板");
    assert!(js.contains("查询清单"), "面板缺少「查询清单」文案");
    assert!(js.contains("查清单"), "表格行缺少「查清单」文案");
}

#[test]
fn model_discovery_css_classes_defined() {
    let css = read_ui("style.css");
    for cls in [
        "models-field-row",
        "model-discovery",
        "model-discovery-bar",
        "model-discovery-list",
        "model-discovery-item",
        "model-discovery-foot",
    ] {
        assert!(css.contains(&format!(".{cls}")), "样式表缺少 .{cls}");
    }
}

#[test]
fn query_models_endpoints_registered() {
    let proxy = src_text("proxy.rs");
    assert!(
        proxy.contains("\"/api/upstreams/models\""),
        "Rust 未注册 POST /api/upstreams/models"
    );
    assert!(
        proxy.contains("\"/api/upstreams/{name}/models\""),
        "Rust 未注册 GET /api/upstreams/{{name}}/models"
    );
}

#[test]
fn api_key_never_travels_in_query_string() {
    /// 密钥不得进 URL（URL 会进访问日志与浏览器历史）：只走 POST 请求体。
    let js = read_ui("app.js");
    assert!(!js.contains("query.set(\"apiKey\""), "面板把密钥放进了 URL");
    assert!(
        js.contains("body.apiKey = apiKey"),
        "面板未通过请求体传密钥"
    );

    // 后端 GET 侧不接受 apiKey（只看代码行，注释里提到 apiKey 是正常的）。
    let api_extra = src_text("api_extra.rs");
    let get_block = api_extra
        .split("pub async fn upstream_models(")
        .nth(1)
        .expect("upstream_models handler");
    let get_block = get_block
        .split("pub async fn upstream_models_post(")
        .next()
        .expect("GET handler block");
    let code: String = get_block
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!code.contains("apiKey"), "Rust GET 接口仍在接受 apiKey");
}

#[test]
fn discovery_panel_is_embedded_not_nested_modal() {
    /// 面板必须内嵌在表单里：另开弹窗会冲掉上游编辑弹窗正在编辑的字段。
    let js = read_ui("app.js");
    let block = js
        .split("async function openModelDiscovery(")
        .nth(1)
        .expect("openModelDiscovery");
    let block = block
        .split("async function applyModelDiscovery(")
        .next()
        .expect("openModelDiscovery block");
    assert!(
        !block.contains("openModal("),
        "查询面板不应另开弹窗（会冲掉正在编辑的上游字段）"
    );
}

#[test]
fn selected_set_is_authoritative_source() {
    /// 勾选状态必须以 selected 集合为准：检索过滤后未显示的已选项不能丢。
    let js = read_ui("app.js");
    let block = js
        .split("async function applyModelDiscovery(")
        .nth(1)
        .expect("applyModelDiscovery");
    assert!(
        block.contains("Array.from(selected)"),
        "应从 selected 集合取勾选结果，而不是从 DOM 反推"
    );
}
