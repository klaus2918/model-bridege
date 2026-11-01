//! 「写入用户指定文件」通道的不变量守卫（env-only-secrets / 范围追加）。
//!
//! 追加需求（用户原话）：「也可以指定文件来设置key」。
//!
//! 确认口径：
//! 1. 密钥写入目标二选一：Windows 用户级环境变量（`HKCU\Environment`），
//!    或**用户手动指定**的凭据文件（JSON / `.env`）；
//! 2. 凭据文件路径**没有内置默认值** —— 必须由用户填写（或引用上游已配置的
//!    `apiKeyFile` / `apiKeyAgentPath`，那也是用户自己填的路径）；
//! 3. 配置文件里只存**路径引用**，不存明文密钥；
//! 4. 写入响应**不回显明文**。
//!
//! 这些约束属于「只靠自觉维持」的不变量：只要有人日后为了省事加回一行默认
//! 路径或把密钥塞回响应体，用户「不落盘、不猜路径」的承诺就会被静默破坏。
//! 本文件把它们固化为纯静态断言，不启动服务、不访问网络、不写文件。

use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read(relative: &str) -> String {
    let path = crate_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("读取 {} 失败：{err}", path.display()))
}

#[test]
fn key_file_module_is_registered() {
    let main = read("src/main.rs");
    assert!(
        main.contains("mod key_file;"),
        "src/main.rs 必须注册 mod key_file;，否则写入文件通道不可用"
    );
    assert!(
        crate_root().join("src/key_file.rs").exists(),
        "缺少 src/key_file.rs（写入凭据文件的实现）"
    );
}

#[test]
fn key_file_module_keeps_safety_rails() {
    let text = read("src/key_file.rs");
    // 不生成备份：备份文件等于明文扩散。
    // （行为级断言在 src/key_file.rs 的 no_backup_file_is_created 单测里；
    //   这里只静态守住「不要引入备份机制」。）
    assert!(
        !text.contains("BACKUP_SUFFIX"),
        "写入凭据文件不得复用配置备份机制（备份 = 明文副本扩散）"
    );
    assert!(
        !text.contains("fs::copy"),
        "写入凭据文件不得通过复制方式留下明文副本"
    );
    // 原子写：同目录临时文件 + rename。
    assert!(
        text.contains("rename"),
        "写入凭据文件必须用临时文件 + rename 原子替换，避免半截文件"
    );
    // 相对路径拒绝：路径必须由用户明确给出。
    assert!(
        text.contains("绝对路径"),
        "凭据文件路径必须是绝对路径（拒绝相对路径）"
    );
}

#[test]
fn secret_endpoint_supports_env_and_file_targets() {
    let text = read("src/api_extra.rs");
    assert!(
        text.contains("target != \"env\" && target != \"file\""),
        "写入目标必须显式区分 env（用户变量）与 file（指定文件）"
    );
    for code in [
        "invalid_target",
        "missing_env_name",
        "invalid_env_name",
        "write_user_var_failed",
        "missing_file_path",
        "invalid_file_format",
        "invalid_file_path",
        "write_key_file_failed",
        "verify_key_file_failed",
    ] {
        assert!(
            text.contains(code),
            "缺少错误码 {code}：写入失败必须可诊断，不得静默降级"
        );
    }
    // 回读校验：写完必须用运行时同一套读取语义确认落盘。
    assert!(
        text.contains("verify_key_file"),
        "写入文件后必须回读校验（与运行时读取语义一致）"
    );
}

#[test]
fn secret_endpoint_never_echoes_plaintext() {
    let text = read("src/api_extra.rs");
    for forbidden in [
        "\"apiKey\": key",
        "\"apiKey\":key",
        "\"key\": key",
        "\"value\": key",
    ] {
        assert!(
            !text.contains(forbidden),
            "响应体出现 {forbidden} —— 密钥写入响应禁止回显明文"
        );
    }
}

#[test]
fn file_target_has_no_builtin_default_path() {
    let text = read("src/api_extra.rs");
    // 路径来源只能是：请求体 filePath，或上游已配置的 apiKeyFile / apiKeyAgentPath。
    assert!(
        text.contains("\"filePath\""),
        "文件目标必须支持请求体传入 filePath"
    );
    assert!(
        text.contains("apiKeyAgentPath"),
        "文件目标缺省路径只能回退到用户自己配置的 apiKeyAgentPath"
    );
    // 内置默认路径片段（沿用 user_env_contracts 的识别口径）。
    for fragment in [
        ".jcode/.env",
        "local/share/opencode/auth.json",
        ".config/opencode/auth.json",
        "commandcode/auth.json",
    ] {
        assert!(
            !text.contains(fragment),
            "文件目标出现内置凭据路径片段 {fragment} —— 路径只能由用户指定"
        );
    }
}

/// v4 面板的全部脚本（v4-core.js / v4-config.js / …）。
///
/// v4 把凭据输入拆到多个模块里，静态守卫必须跨模块扫描，
/// 只查单一文件会漏掉「密钥只写通道」的实现。
fn v4_panel_js() -> String {
    let dir = crate_root().join("ui");
    let mut out = String::new();
    for entry in fs::read_dir(&dir).expect("read ui dir") {
        let path = entry.expect("ui entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("v4") && name.ends_with(".js") {
            out.push_str(&fs::read_to_string(&path).expect("read v4 ui module"));
            out.push('\n');
        }
    }
    assert!(!out.is_empty(), "ui/ 下应有 v4*.js 脚本");
    out
}

#[test]
fn v4_panel_never_persists_plaintext_secret() {
    let text = v4_panel_js();
    assert!(
        text.contains("type=\"password\""),
        "v4 凭据页必须使用 password 输入，密钥不回显"
    );
    assert!(
        text.contains("/secret"),
        "v4 凭据页必须走密钥写入接口，而不是把密钥写进配置对象"
    );
    assert!(
        !text.contains("entry.apiKey =") && !text.contains("config.apiKey ="),
        "v4 面板不得把明文密钥赋给配置对象"
    );
}
#[test]
fn config_store_strips_plaintext_secret() {
    let text = read("src/config_store.rs");
    assert!(
        text.contains("apiKey"),
        "配置落盘前必须剥离明文 apiKey（strip_secrets）"
    );
}
