//! 配置单一事实源的不变量守卫。
//!
//! 背景（config-as-json）：`bridge.config.json` 是配置的**唯一事实源**。
//! 数据库 `settings` 表里的配置快照是 Python 时代的派生产物 —— 它曾导致
//! 一个严重缺陷：用户从 JSON 删掉的上游，会被快照递归补回运行态，
//! 随后任意一次面板保存又把它写回文件，**用户在 JSON 里的删除被静默撤销**。
//!
//! Rust 版从设计上就不读配置快照（只读 `lastPort` 与更新检查时间戳），
//! 因此天然没有该缺陷。但「不读」是一个**只靠自觉维持的不变量** ——
//! 任何人日后加一行 `db.get_setting("config")` 就会把它悄悄带回来。
//!
//! 本文件把该不变量固化为可执行断言：
//! 1. Rust 源码不得读取 `settings` 里的配置快照（只允许已知的安全键）；
//! 2. `config_store` 必须提供「合并式落盘」与「备份」这两组能力；
//! 3. 备份份数口径必须是「总共 5 份」（含 `.bak` 本身）。
//!
//! 注意：这些都是**纯静态检查**，不启动任何服务、不占用端口。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn rust_sources() -> Vec<PathBuf> {
    let src = crate_root().join("src");
    let mut files: Vec<PathBuf> = fs::read_dir(&src)
        .expect("read src dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    files.sort();
    files
}

/// `settings` 表里**允许**被读取的键。
///
/// `lastPort` 用于「二次启动时打开已有面板」；
/// `update.*` 是更新检查的时间戳。
/// **配置快照（`config`）不在允许列表内** —— 它就是复活缺陷的根源。
const ALLOWED_SETTING_KEYS: &[&str] = &["lastPort", "update."];

#[test]
fn rust_never_reads_config_snapshot_from_settings() {
    // 扫出所有 get_setting("...") 调用，断言键名都在允许列表内。
    let mut violations: Vec<String> = Vec::new();
    for path in rust_sources() {
        let text = fs::read_to_string(&path).expect("read rust source");
        for (idx, line) in text.lines().enumerate() {
            if !line.contains("get_setting(") {
                continue;
            }
            // 只看本行里的字符串字面量参数（多行调用不在此列，见下一条测试）。
            let Some(start) = line.find('"') else {
                continue;
            };
            let Some(end) = line[start + 1..].find('"') else {
                continue;
            };
            let key = &line[start + 1..start + 1 + end];
            if !ALLOWED_SETTING_KEYS.iter().any(|k| key.starts_with(k)) {
                violations.push(format!(
                    "{}:{} 读取了 settings 里的 {:?}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    idx + 1,
                    key
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Rust 不得读取配置快照（settings.config）——那会让用户从 JSON 删除的上游复活：\n{}",
        violations.join("\n")
    );
}

#[test]
fn rust_never_defines_a_config_snapshot_loader() {
    // 更严格的一层：源码里不得出现「加载/保存配置快照」这类函数名。
    //
    // 注意：不能用 `"config"` 这个字面量做判据 —— 面板接口的 config 字段
    // （`payload.get("config")`）是合法用法，与快照无关。
    // 真正的判据是「从 settings 表读配置」这件事，故只盯函数名。
    let forbidden = ["load_config_override", "save_config_override"];
    let mut hits: Vec<String> = Vec::new();
    for path in rust_sources() {
        let text = fs::read_to_string(&path).expect("read rust source");
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            // 跳过测试代码与注释（它们会故意提到这些名字）。
            if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("/*") {
                continue;
            }
            for token in forbidden.iter() {
                if trimmed.contains(token) {
                    hits.push(format!(
                        "{}:{} 出现 {:?}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        idx + 1,
                        token
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "Rust 不得引入配置快照读写路径：\n{}",
        hits.join("\n")
    );
}

#[test]
fn config_store_exposes_merge_and_backup_capabilities() {
    // 单一事实源的三件套必须齐备：合并式落盘、原子写、备份。
    let text =
        fs::read_to_string(crate_root().join("src/config_store.rs")).expect("read config_store.rs");
    for symbol in [
        "pub fn merge_incoming",
        "pub fn save_atomic",
        "pub fn backup_config",
        "pub fn restore_backup",
        "pub fn list_backups",
        "pub fn strip_secrets",
    ] {
        assert!(
            text.contains(symbol),
            "config_store 缺少 {} —— 配置落盘能力被削弱了",
            symbol
        );
    }
}

#[test]
fn backup_keep_is_five_including_the_latest() {
    // 口径：总共 5 份 = `.bak` + `.bak.1`~`.bak.4`。
    // 历史上算错过（写成 `.bak` + `.bak.1..bak.5` = 6 份），这里钉死。
    let text =
        fs::read_to_string(crate_root().join("src/config_store.rs")).expect("read config_store.rs");
    assert!(
        text.contains("pub const BACKUP_KEEP: usize = 5;"),
        "BACKUP_KEEP 口径必须是 5（含 .bak 本身）"
    );
    // 历史备份编号上界应为 BACKUP_KEEP - 1（.bak.1 ~ .bak.4）。
    assert!(
        text.contains("for index in 1..BACKUP_KEEP"),
        "历史备份编号上界应为 BACKUP_KEEP-1（.bak.1~.bak.4），共 5 份"
    );
}

#[test]
fn db_path_follows_config_directory_not_cwd() {
    // 升级衔接：库路径必须跟随配置文件目录，不能用 CWD 相对路径。
    // CWD 相对会让「从快捷方式启动」读不到旧库（用量历史看似清零），
    // 并在别处生成一个空的影子库。
    let text = fs::read_to_string(crate_root().join("src/main.rs")).expect("read main.rs");
    assert!(
        text.contains("fn default_db_path(config_path: &std::path::Path) -> PathBuf"),
        "缺少 default_db_path(config_path)：库路径未跟随配置文件目录"
    );
    assert!(
        text.contains("base.join(\"data\").join(\"modelgate.db\")"),
        "库路径必须由配置文件目录推导（base.join(\"data\")）"
    );
    // CWD 相对写法只允许出现在测试断言里，不得出现在生产路径推导中。
    // 判据：出现在 `#[cfg(test)]` 之前的生产代码段里即违规。
    let production = match text.find("#[cfg(test)]") {
        Some(idx) => &text[..idx],
        None => text.as_str(),
    };
    assert!(
        !production.contains("PathBuf::from(\"data\")"),
        "生产代码不得用 CWD 相对的库路径（升级后从快捷方式启动会读不到旧库）"
    );
}

#[test]
fn allowed_setting_keys_are_the_only_snapshot_reads() {
    // 兜底：确认允许列表本身没被悄悄放宽。
    let allowed: BTreeSet<&str> = ALLOWED_SETTING_KEYS.iter().copied().collect();
    assert!(!allowed.contains("config"), "配置快照键不得加入允许列表");
    assert_eq!(
        allowed.len(),
        2,
        "允许的 settings 键应恰为 lastPort 与 update. 两类"
    );
}
