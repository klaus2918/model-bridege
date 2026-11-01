//! 控制台静态资源（`ui/`）的取用层。
//!
//! 为什么单独成模块：v4 面板的 HTML/CSS/JS 原先用 `include_str!` 直接内嵌在
//! `proxy.rs` 里，改一行 CSS 就要重编译 190 KB 的 proxy.rs 再整包重链接
//! （fat LTO 下实测约 3 分钟，thin LTO 约 1.5 分钟）。这里把资源挪出来，
//! 并按构建类型分流：
//!
//! - debug 构建：只读磁盘（默认 `CARGO_MANIFEST_DIR/ui`），改前端零重建；
//! - release 构建：内嵌进二进制，保持单文件分发；
//! - 任意构建：`--ui-dir` 或 `MODEL_GATE_UI_DIR` 可强制走磁盘（改面板不用重编）。
//!
//! 白名单按「磁盘目录存在则该目录下的合法文件名都放行，否则只认内嵌表」处理，
//! 因此 release 产物不会因为磁盘上多放一个文件就多出一个可访问资源。

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// 内嵌资源清单：`(文件名, 内容)`。仅在 release 构建参与编译。
#[cfg(not(debug_assertions))]
const EMBEDDED: &[(&str, &str)] = &[
    ("v4.html", include_str!("../ui/v4.html")),
    ("v4-base.css", include_str!("../ui/v4-base.css")),
    ("v4.css", include_str!("../ui/v4.css")),
    ("v4.js", include_str!("../ui/v4.js")),
    ("v4-core.js", include_str!("../ui/v4-core.js")),
    ("v4-config.js", include_str!("../ui/v4-config.js")),
    ("v4-models.js", include_str!("../ui/v4-models.js")),
    ("v4-usage.js", include_str!("../ui/v4-usage.js")),
    ("v4-prompts.js", include_str!("../ui/v4-prompts.js")),
    ("v4-resources.js", include_str!("../ui/v4-resources.js")),
];

/// 面板依赖的全部资源名（不含 index）。供启动自检/文档使用。
pub const ASSET_NAMES: &[&str] = &[
    "v4-base.css",
    "v4.css",
    "v4.js",
    "v4-core.js",
    "v4-config.js",
    "v4-models.js",
    "v4-usage.js",
    "v4-prompts.js",
    "v4-resources.js",
];

/// 面板目录：优先环境变量（`--ui-dir` 会写进它），其次编译期 crate 目录，
/// 最后是 exe 同级的 `ui/`（便于解压即用）。只有真的存在 `v4.html` 才算数。
pub fn ui_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("MODEL_GATE_UI_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            candidates.push(PathBuf::from(dir));
        }
    }
    // debug 下默认指向源码目录：`cargo run` / `cargo test` 在任何 CWD 下都能工作。
    if cfg!(debug_assertions) {
        candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/ui")));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("ui"));
        }
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("v4.html").is_file())
}

/// 读取一个面板资源。磁盘命中优先，否则回落到内嵌副本。
pub fn load(name: &str) -> Option<Cow<'static, str>> {
    let name = sanitize(name)?;
    if let Some(root) = ui_dir() {
        if let Ok(text) = std::fs::read_to_string(root.join(name)) {
            return Some(Cow::Owned(text));
        }
    }
    embedded(name).map(Cow::Borrowed)
}

/// 资源名合法性：只允许单层文件名，挡住 `../` 与绝对路径。
fn sanitize(name: &str) -> Option<&str> {
    if name.is_empty() || name.len() > 64 {
        return None;
    }
    if name == "." || name == ".." || name.starts_with('.') {
        return None;
    }
    let ok = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if !ok {
        return None;
    }
    // 必须带扩展名，且不能是纯扩展名。
    let stem_len = name.split('.').next().map(str::len).unwrap_or(0);
    (stem_len > 0 && name.contains('.')).then_some(name)
}

/// 按扩展名给出 content-type（与旧实现逐项一致）。
pub fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(not(debug_assertions))]
fn embedded(name: &str) -> Option<&'static str> {
    EMBEDDED
        .iter()
        .find(|(file, _)| *file == name)
        .map(|(_, body)| *body)
}

// debug 构建不内嵌：否则改一眼 `ui/*.css` 依然会触发 proxy.rs 重编译。
#[cfg(debug_assertions)]
fn embedded(_name: &str) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize("../secret").is_none());
        assert!(sanitize("..\\secret").is_none());
        assert!(sanitize("a/b.css").is_none());
        assert!(sanitize("C:evil.css").is_none());
        assert!(sanitize(".env").is_none());
        assert!(sanitize("").is_none());
        assert!(sanitize("v4.css").is_some());
        assert!(sanitize("v4-base.css").is_some());
    }

    #[test]
    fn content_types_match_contract() {
        assert_eq!(content_type("v4.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("v4.css"), "text/css; charset=utf-8");
        assert_eq!(
            content_type("v4.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(content_type("x.bin"), "application/octet-stream");
    }
}
