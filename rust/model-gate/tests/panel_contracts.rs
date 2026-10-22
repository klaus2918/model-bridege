//! 面板接口与 Rust 路由注册的一致性守卫。
//!
//! 背景（config-as-json）：Rust 版是唯一产品实现，面板资源内嵌在
//! `rust/model-gate/ui/`，由 `src/proxy.rs` 的 `include_str!` 提供。
//! 但**路由注册此前没有任何测试覆盖** —— 面板调用了未注册的接口时无人发现
//! （本次新增 `/api/config/backups` 与 `/api/config/backups/restore` 即命中该缺口）。
//!
//! 本文件补上这个缺口：扫 Rust 源码里的 `.route(...)` 注册表，
//! 与面板 JS 实际调用的接口清单对齐。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// 扫 Rust 源码里注册的全部路由。
///
/// 必须**跨行**扫描：路由注册为了控制行宽，多处写成
/// `.route(\n    "/api/xxx", get(handler),\n)` 的形式。逐行扫描会漏掉它们，
/// 造成「面板调用了未注册接口」的假阳性。
fn rust_routes() -> BTreeSet<String> {
    let src = crate_root().join("src");
    let mut routes = BTreeSet::new();
    for entry in fs::read_dir(&src).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read rust source");
        for route in parse_route_literals(&text) {
            routes.insert(route);
        }
    }
    routes
}

/// 从源码文本里抽出所有 `.route("...")` 的路径字面量。
///
/// 做法：找到 `.route(` 后跳过空白与换行，再读取紧跟的字符串字面量。
fn parse_route_literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut routes = Vec::new();
    let mut idx = 0usize;
    while let Some(pos) = text[idx..].find(".route(") {
        let mut cursor = idx + pos + ".route(".len();
        // 跳过 `.route(` 与路径字面量之间的空白（含换行）。
        while cursor < bytes.len() && (bytes[cursor] as char).is_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b'"' {
            let rest = &text[cursor + 1..];
            if let Some(end) = rest.find('"') {
                routes.push(rest[..end].to_string());
            }
        }
        idx = cursor.max(idx + pos + ".route(".len());
    }
    routes
}

/// 面板 JS 里实际调用的接口路径。
///
/// 与 [`parse_route_literals`] 同理，JS 侧也可能把调用写成多行
/// （`api(\n  "/api/x"\n)`），因此同样跨行解析。
fn panel_api_calls(ui_dir: &Path) -> BTreeSet<String> {
    let js = fs::read_to_string(ui_dir.join("app.js")).expect("read ui/app.js");
    let mut calls = BTreeSet::new();
    for quote in ['"', '\''] {
        // marker 只到左括号为止：引号由解析器自己跳过空白后读取，
        // 这样 `api(` 与 `postJson(` 两种调用都能统一处理。
        for marker in ["api(", "postJson("] {
            for path in parse_call_literals(&js, marker, quote) {
                let path = path.split('?').next().unwrap_or(&path);
                if path.starts_with("/api/") {
                    calls.insert(path.to_string());
                }
            }
        }
    }
    calls
}

/// 抽出所有 `marker` 之后紧跟的字符串字面量（允许跨空白）。
///
/// `marker` 不含引号；解析器跳过空白后应读到 `quote`，再取到配对的结束引号。
fn parse_call_literals(text: &str, marker: &str, quote: char) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut idx = 0usize;
    while let Some(pos) = text[idx..].find(marker) {
        let mut cursor = idx + pos + marker.len();
        while cursor < bytes.len() && (bytes[cursor] as char).is_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] as char == quote {
            let rest = &text[cursor + 1..];
            if let Some(end) = rest.find(quote) {
                found.push(rest[..end].to_string());
            }
        }
        idx = cursor.max(idx + pos + marker.len());
    }
    found
}

fn ui_dir() -> PathBuf {
    crate_root().join("ui")
}

#[test]
fn backup_endpoints_are_registered() {
    let routes = rust_routes();
    assert!(
        routes.contains("/api/config/backups"),
        "Rust 未注册 GET /api/config/backups（面板备份状态读不到）"
    );
    assert!(
        routes.contains("/api/config/backups/restore"),
        "Rust 未注册 POST /api/config/backups/restore（面板恢复备份不可用）"
    );
}

/// 「从文件重载」的后端语义：重读磁盘的 bridge.config.json 并覆盖运行态。
///
/// 这与「放弃改动」方向相反（后者只把表单恢复成运行态，不碰磁盘）。
/// 面板靠这个差异给用户两个不同的出口，后端语义一旦被改成「读运行态」，
/// 手工编辑配置文件后就没法让它立即生效（必须重启进程）。
#[test]
fn reload_from_file_reads_disk_not_runtime() {
    let source = fs::read_to_string(crate_root().join("src/proxy.rs")).expect("read src/proxy.rs");
    let start = source
        .find("async fn api_config_reload")
        .expect("找不到 api_config_reload");
    let body = &source[start..];
    let end = body
        .find("\nasync fn ")
        .or_else(|| body.find("\npub "))
        .unwrap_or(body.len());
    let fn_body = &body[..end];

    assert!(
        fn_body.contains("Config::load(&state.config_path)"),
        "「从文件重载」必须从磁盘重读配置文件（Config::load(&state.config_path)）"
    );
    assert!(
        fn_body.contains("replace_config"),
        "「从文件重载」必须把磁盘内容应用到运行态（replace_config）"
    );
    assert!(
        !fn_body.contains("state.cfg().clone()"),
        "「从文件重载」不得只回显当前运行态 —— 那会与「放弃改动」无区别"
    );
}

#[test]
fn config_endpoints_are_registered() {
    let routes = rust_routes();
    for path in [
        "/api/config",
        "/api/config/validate",
        "/api/config/reload",
        "/api/pricing",
    ] {
        assert!(routes.contains(path), "Rust 未注册 {path}");
    }
}

#[test]
fn prompt_endpoints_are_registered() {
    let routes = rust_routes();
    for path in [
        "/api/prompts",
        "/api/prompts/export",
        "/api/prompts/evolution",
        "/api/prompts/diff",
        "/api/prompts/diff/download",
        "/api/prompts/clear",
        "/api/prompts/{id}",
        "/api/prompts/{id}/download",
        "/api/usage/sessions",
    ] {
        assert!(routes.contains(path), "Rust 未注册 {path}");
    }
}

#[test]
fn every_panel_api_call_has_a_registered_route() {
    let routes = rust_routes();
    let calls = panel_api_calls(&ui_dir());
    let missing: Vec<&String> = calls.iter().filter(|p| !routes.contains(*p)).collect();
    assert!(
        missing.is_empty(),
        "面板调用了 Rust 未注册的接口: {missing:?}"
    );
}

#[test]
fn panel_backup_controls_exist_in_html() {
    let html = fs::read_to_string(ui_dir().join("index.html")).expect("read ui/index.html");
    for id in [
        "config-backup-toggle",
        "config-path-display",
        "config-backup-hint",
        "restore-config",
    ] {
        assert!(html.contains(id), "面板缺少 #{id}");
    }
    assert!(
        html.contains("从文件重载"),
        "配置页底部按钮应为「从文件重载」（原名「重新载入」与其实际行为相反）"
    );
}

/// 「从文件重载」必须先确认再覆盖，否则面板上未保存的改动会被静默丢弃。
///
/// 背景（config-as-json）：「从文件重载」= 重读磁盘的 bridge.config.json 并覆盖运行态。
/// 面板上有未保存改动时点它，改动会**静默消失**且无任何提示 —— 实测确认过这个缺陷。
/// 修复是加确认框；本守卫钉死它不被回退。
#[test]
fn reload_from_file_guards_unsaved_edits() {
    let js = fs::read_to_string(ui_dir().join("app.js")).expect("read ui/app.js");
    assert!(
        js.contains("state.configDirty"),
        "面板未跟踪未保存改动状态 —— 「从文件重载」无法判断是否要先确认"
    );
    assert!(
        js.contains("放弃改动并重载"),
        "「从文件重载」缺少确认文案 —— 未保存的改动会被静默覆盖"
    );
    assert!(
        js.contains("这些未保存的改动会丢失"),
        "确认框必须说明未保存改动会丢失，否则用户无法判断风险"
    );
    // 旧名不应复活：它与实际行为相反（听像重读面板，实为重读磁盘覆盖运行态）。
    assert!(
        !js.contains("已重新载入配置"),
        "旧提示「已重新载入配置」不应存在（与「从文件重载」语义混淆）"
    );
}

/// 扫描器自检：跨行写法必须被识别。
///
/// 这个守卫本身曾因「逐行扫描」而误报 12 条路由缺失。解析器一旦退化回逐行，
/// 就会重新产生假阳性，因此用单测把跨行能力钉死。
#[test]
fn route_scanner_handles_multiline_registration() {
    let source = r#"
        router
            .route("/api/one", get(one))
            .route(
                "/api/two",
                get(two),
            )
            .route(
                "/api/three",
                post(three),
            );
    "#;
    let routes = parse_route_literals(source);
    assert!(routes.contains(&"/api/one".to_string()));
    assert!(routes.contains(&"/api/two".to_string()));
    assert!(routes.contains(&"/api/three".to_string()));
}

#[test]
fn call_scanner_handles_multiline_invocation() {
    let source = r#"
        await api("/api/one");
        await api(
            "/api/two"
        );
        await postJson("/api/three", body);
    "#;
    let calls = parse_call_literals(source, "api(", '"');
    assert!(calls.contains(&"/api/one".to_string()));
    assert!(calls.contains(&"/api/two".to_string()));
    let posts = parse_call_literals(source, "postJson(", '"');
    assert!(posts.contains(&"/api/three".to_string()));
}

/// `api(` 与 `postJson(` 两种调用必须各自独立命中，不能互相误判。
///
/// 这里固化的是「`api(` 不是 `postJson(` 的子串」这一事实：
/// 若将来有人把 marker 改成含引号的形式，解析会静默失配。
#[test]
fn call_scanner_markers_do_not_confuse_each_other() {
    let source = r#"await postJson("/api/only-post", body);"#;
    let api_calls = parse_call_literals(source, "api(", '"');
    let post_calls = parse_call_literals(source, "postJson(", '"');
    assert!(
        !api_calls.contains(&"/api/only-post".to_string()),
        "`api(` marker 误命中了 postJson 调用"
    );
    assert!(post_calls.contains(&"/api/only-post".to_string()));
}
