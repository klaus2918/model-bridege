//! CommandCode 本地模型目录：解析 CLI 包内 `models.md`。
//!
//! 为什么放在 Rust 侧：CommandCode **没有** `/v1/models` 端点，模型目录来自
//! 本机 CLI 包内的 `models.md`。Python 版有 `command_code_catalog_entries()`，
//! Rust-only 改造删除 Python 实现时把它一起删掉了，但 `Compat.catalog_source`
//! 的 `"commandcode"` 声明被保留了下来 —— 成了**没有任何消费方的死配置**。
//! 结果是面板「查清单」对 commandcode 恒返回 404，而 404 提示会让用户去改
//! `compat.endpoint.models` 路径（方向完全错误）。
//!
//! 本模块补回该能力，并让查询路径按 `catalog_source` 分流。

use std::path::{Path, PathBuf};

/// 目录中的一条模型记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// 模型完整 id（如 `deepseek/deepseek-v4-pro`）。
    pub id: String,
    /// 最低套餐档位名（`go` / `goat` / `pro` / `max`），空串表示不限。
    pub min_plan: String,
}

/// 套餐档位排序（数值越大档位越高），用于按套餐过滤。
pub fn plan_rank(text: &str) -> u8 {
    let t = text.trim().to_ascii_lowercase();
    if t.starts_with("max") {
        4
    } else if t.starts_with("pro") {
        3
    } else if t.starts_with("goat") {
        2
    } else if t.starts_with("go") {
        1
    } else {
        0
    }
}

/// 解析 `models.md` 文本，返回模型条目列表。
///
/// 表格行形如：
/// `| \`deepseek/deepseek-v4-pro\` | DeepSeek | 128K | yes | ... | GOAT | ... |`
///
/// 第 1 列是模型 id，第 6 列是「Min plan」。只取能解析出 id 的行，
/// 重复 id 保留首次出现的。
pub fn parse_models_md(text: &str) -> Vec<CatalogEntry> {
    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let Some(model_id) = first_backtick_cell(trimmed) else {
            continue;
        };
        if model_id.is_empty() || !seen.insert(model_id.clone()) {
            continue;
        }
        let min_plan = min_plan_cell(trimmed);
        entries.push(CatalogEntry {
            id: model_id,
            min_plan,
        });
    }
    entries
}

/// 取第一列反引号里的内容（模型 id）。
fn first_backtick_cell(line: &str) -> Option<String> {
    let start = line.find('`')?;
    let rest = &line[start + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].trim().to_string())
}

/// 取「Min plan」列的值。
///
/// 表头列序为：模型 | 名称 | 上下文 | 思考 | 价格 | Min plan | 适用。
/// 因此第 6 列（索引 5）是 Min plan。为容错，表头列数变化时回退到
/// 「包含 plan 的列」匹配。
fn min_plan_cell(line: &str) -> String {
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
    if cells.len() > 5 {
        let candidate = cells[5].trim_matches('*').trim();
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    for cell in &cells {
        let lower = cell.to_ascii_lowercase();
        if lower.starts_with("go") || lower.starts_with("pro") || lower.starts_with("max") {
            return cell.to_string();
        }
    }
    String::new()
}

/// 探测本机 CommandCode CLI 的安装根目录。
///
/// 依次尝试 npm 全局安装的常见位置。
pub fn command_code_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut roots = Vec::new();
    if !home.as_os_str().is_empty() {
        roots.push(
            home.join("AppData")
                .join("Roaming")
                .join("npm")
                .join("node_modules")
                .join("command-code"),
        );
        roots.push(
            home.join(".npm-global")
                .join("lib")
                .join("node_modules")
                .join("command-code"),
        );
    }
    roots.push(PathBuf::from("/usr/local/lib/node_modules/command-code"));
    roots.push(PathBuf::from("/usr/lib/node_modules/command-code"));
    roots
}

/// `models.md` 在 CLI 包内的相对路径。
const MODELS_MD_RELATIVE: [&str; 4] = ["dist", "bundled", "command-code-knowledge", "reference"];

/// 在 CLI 根目录下拼出 `models.md` 路径。
pub fn models_md_path(root: &Path) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in MODELS_MD_RELATIVE.iter() {
        path.push(part);
    }
    path.push("models.md");
    path
}

/// 读取本机 CLI 的模型目录。
///
/// `spec` 语义与 Python 版一致：
/// - `false` → 不读，返回空
/// - 具体路径字符串 → 直接读该文件
/// - `"auto"` 或空 → 依次探测 CLI 根目录，取第一个能解析出条目的文件
///
/// 读不到或解析不出条目时返回空列表（不报错）—— 交由调用方给出明确提示。
pub fn read_catalog(spec: Option<&str>) -> Vec<CatalogEntry> {
    let spec = spec.unwrap_or("auto");
    if spec == "false" {
        return Vec::new();
    }
    let files: Vec<PathBuf> = if spec != "auto" && !spec.is_empty() {
        vec![PathBuf::from(spec)]
    } else {
        command_code_roots()
            .iter()
            .map(|root| models_md_path(root))
            .collect()
    };
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let entries = parse_models_md(&text);
        if !entries.is_empty() {
            return entries;
        }
    }
    Vec::new()
}

/// 按套餐档位过滤目录。
///
/// `plan` 为空表示不过滤（套餐未知时不误伤用户）。
/// `rank` 是当前套餐的档位值（见 [`plan_rank`]）。
pub fn filter_by_plan(entries: Vec<CatalogEntry>, plan: &str, rank: u8) -> Vec<CatalogEntry> {
    if plan.is_empty() || rank == 0 {
        return entries;
    }
    entries
        .into_iter()
        .filter(|entry| {
            let needed = plan_rank(&entry.min_plan);
            needed == 0 || needed <= rank
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"| 模型 | 名称 | 上下文 | 思考 | 价格 | Min plan | 适用 |
| --- | --- | --- | --- | --- | --- | --- |
| `deepseek/deepseek-v4-pro` | DeepSeek | 128K | yes | $3 | GOAT | 全部 |
| `moonshotai/Kimi-K3` | Kimi | 256K | no | $2 | Max | 全部 |
| `zai-org/GLM-5.3` | GLM | 200K | yes | $1 | Pro | 全部 |
| `mimo-1` | MiMo | 32K | no | $0 | Go | 全部 |
| `free-1` | Free | 8K | no | $0 |  | 全部 |
| `deepseek/deepseek-v4-pro` | 重复行 | | | | | |
"#;

    #[test]
    fn parses_model_ids_and_min_plan() {
        let entries = parse_models_md(SAMPLE);
        assert_eq!(entries.len(), 5, "重复 id 应被去重");
        assert_eq!(entries[0].id, "deepseek/deepseek-v4-pro");
        assert_eq!(entries[0].min_plan, "GOAT");
        assert_eq!(entries[1].min_plan, "Max");
        assert_eq!(entries[2].min_plan, "Pro");
        assert_eq!(entries[3].min_plan, "Go");
        assert_eq!(entries[4].min_plan, "", "空 Min plan 应为空串");
    }

    #[test]
    fn plan_rank_orders_tiers() {
        assert_eq!(plan_rank("Max"), 4);
        assert_eq!(plan_rank("pro"), 3);
        assert_eq!(plan_rank("GOAT"), 2);
        assert_eq!(plan_rank("go"), 1);
        assert_eq!(plan_rank(""), 0);
        assert_eq!(plan_rank("unknown"), 0);
    }

    #[test]
    fn filters_by_plan_rank() {
        let entries = parse_models_md(SAMPLE);
        // GOAT 档（rank 2）应保留 go/goat/不限，排除 pro/max。
        let filtered = filter_by_plan(entries.clone(), "goat", plan_rank("goat"));
        let ids: Vec<&str> = filtered.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"deepseek/deepseek-v4-pro"));
        assert!(ids.contains(&"mimo-1"));
        assert!(ids.contains(&"free-1"));
        assert!(
            !ids.contains(&"moonshotai/Kimi-K3"),
            "Max 模型不应出现在 GOAT 档"
        );
        assert!(
            !ids.contains(&"zai-org/GLM-5.3"),
            "Pro 模型不应出现在 GOAT 档"
        );
    }

    #[test]
    fn empty_plan_keeps_everything() {
        let entries = parse_models_md(SAMPLE);
        let filtered = filter_by_plan(entries, "", 0);
        assert_eq!(filtered.len(), 5);
    }

    #[test]
    fn reads_explicit_file_path() {
        let dir = std::env::temp_dir().join(format!("mg-cc-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("models.md");
        std::fs::write(&file, SAMPLE).unwrap();
        let entries = read_catalog(Some(file.to_str().unwrap()));
        assert_eq!(entries.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_empty_without_error() {
        let entries = read_catalog(Some("/definitely/not/here/models.md"));
        assert!(entries.is_empty());
    }

    #[test]
    fn false_spec_disables_catalog() {
        assert!(read_catalog(Some("false")).is_empty());
    }

    #[test]
    fn models_md_path_joins_cli_layout() {
        let path = models_md_path(Path::new("/opt/command-code"));
        assert!(path.ends_with("dist/bundled/command-code-knowledge/reference/models.md"));
    }
}
