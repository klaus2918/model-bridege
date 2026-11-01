//! ZCode 插件配置：路径全部可配置，并提供本机默认探测。
//!
//! 配置保存在 `data/zcode.json`，不包含 JWT/API Key；账号密文在 SQLite 中。
//! `cli_path` / `node_path` / `data_dir` / `browser_path` 用于适配任意安装位置
//! （例如 `%LOCALAPPDATA%\Programs\ZCode\resources\glm\zcode.cjs`）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeConfig {
    pub enabled: bool,
    pub backend: String,
    pub cli_path: String,
    pub node_path: String,
    pub data_dir: String,
    pub builtin_dir: String,
    pub browser_path: String,
    pub captcha_scene: String,
    pub captcha_region: String,
    pub captcha_prefix: String,
    /// 每日领取时间（本地时区 `HH:MM`；空 = 不定时）。
    #[serde(default)]
    pub claim_daily_time: String,
    pub claim_interval_seconds: u64,
    pub account_concurrency: u32,
}

impl Default for ZcodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: "appserver".to_string(),
            cli_path: detect_zcode_cli(),
            node_path: detect_node(),
            data_dir: detect_zcode_data_dir(),
            builtin_dir: String::new(),
            browser_path: detect_browser(),
            captcha_scene: super::captcha::default_scene(),
            captcha_region: super::captcha::default_region(),
            captcha_prefix: super::captcha::default_prefix(),
            claim_daily_time: String::new(),
            claim_interval_seconds: 0,
            account_concurrency: 2,
        }
    }
}

impl ZcodeConfig {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Self>(&text).unwrap_or_else(|err| {
                tracing::warn!("ZCode 配置解析失败（使用默认值）：{err}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let value = serde_json::to_value(self)?;
        crate::config_store::save_atomic(path, &value)
    }

    /// 解析后的 CLI 路径（空串时回退默认）。
    pub fn resolved_cli_path(&self) -> PathBuf {
        let value = self.cli_path.trim();
        if value.is_empty() {
            PathBuf::from(detect_zcode_cli())
        } else {
            PathBuf::from(value)
        }
    }

    /// 解析后的数据目录（空串时回退 `~/.zcode`）。
    pub fn resolved_data_dir(&self) -> PathBuf {
        let value = self.data_dir.trim();
        if value.is_empty() {
            PathBuf::from(detect_zcode_data_dir())
        } else {
            PathBuf::from(value)
        }
    }

    /// 解析后的 Node 路径（空串时用 PATH 中的 `node`）。
    pub fn resolved_node(&self) -> String {
        let value = self.node_path.trim();
        if value.is_empty() {
            "node".to_string()
        } else {
            value.to_string()
        }
    }

    /// 解析后的浏览器路径（空串时回退探测结果）。
    pub fn resolved_browser(&self) -> PathBuf {
        let value = self.browser_path.trim();
        if value.is_empty() {
            PathBuf::from(detect_browser())
        } else {
            PathBuf::from(value)
        }
    }
}

pub fn config_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("data")
        .join("zcode.json")
}

pub fn detect_zcode_cli() -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("ZCODE_HOME") {
        candidates.push(
            PathBuf::from(dir)
                .join("resources")
                .join("glm")
                .join("zcode.cjs"),
        );
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(&local)
                .join("Programs")
                .join("ZCode")
                .join("resources")
                .join("glm")
                .join("zcode.cjs"),
        );
        // 部分安装器把应用放在 `Programs\ZCode\ZCode.exe`，CLI 仍在 resources 下。
        candidates.push(
            PathBuf::from(&local)
                .join("ZCode")
                .join("resources")
                .join("glm")
                .join("zcode.cjs"),
        );
    }
    if let Ok(pf) = std::env::var("PROGRAMFILES") {
        candidates.push(
            PathBuf::from(&pf)
                .join("ZCode")
                .join("resources")
                .join("glm")
                .join("zcode.cjs"),
        );
    }
    if let Ok(pf) = std::env::var("PROGRAMFILES(X86)") {
        candidates.push(
            PathBuf::from(pf)
                .join("ZCode")
                .join("resources")
                .join("glm")
                .join("zcode.cjs"),
        );
    }
    candidates.push(
        PathBuf::from(r"C:\Program Files\ZCode")
            .join("resources")
            .join("glm")
            .join("zcode.cjs"),
    );
    candidates.push(
        PathBuf::from(r"C:\Program Files (x86)\ZCode")
            .join("resources")
            .join("glm")
            .join("zcode.cjs"),
    );
    for path in candidates {
        if path.is_file() {
            return path.display().to_string();
        }
    }
    // 自定义安装目录（例如 D:\mysoft\ZCode）无法靠固定路径枚举覆盖：
    // 从正在运行的 ZCode 客户端反推安装目录。只在固定候选全部落空时执行一次，
    // 避免每次请求都拉起 PowerShell。
    #[cfg(windows)]
    if let Some(path) = detect_cli_from_running_process() {
        return path.display().to_string();
    }
    String::new()
}

/// 从运行中的 ZCode 客户端进程路径反推 `<install>/resources/glm/zcode.cjs`。
///
/// 用户可以把 ZCode 装到任意盘符/目录；卸载注册表项又可能是 GUID 子键，
/// 直接读进程映像路径最稳。PowerShell 只用于这一次探测，且不创建控制台窗口。
#[cfg(windows)]
fn detect_cli_from_running_process() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Process -Name ZCode -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Path)",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let exe = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if exe.is_empty() {
        return None;
    }
    let cli = PathBuf::from(exe)
        .parent()?
        .join("resources")
        .join("glm")
        .join("zcode.cjs");
    cli.is_file().then_some(cli)
}

pub fn detect_node() -> String {
    if let Ok(pf) = std::env::var("PROGRAMFILES") {
        let path = PathBuf::from(pf).join("nodejs").join("node.exe");
        if path.exists() {
            return path.display().to_string();
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let path = PathBuf::from(local)
            .join("Programs")
            .join("nodejs")
            .join("node.exe");
        if path.exists() {
            return path.display().to_string();
        }
    }
    "node".to_string()
}

pub fn detect_zcode_data_dir() -> String {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile).join(".zcode").display().to_string();
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".zcode").display().to_string();
    }
    ".zcode".to_string()
}

pub fn detect_browser() -> String {
    let candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_backend_and_interval() {
        let cfg = ZcodeConfig::default();
        assert_eq!(cfg.backend, "appserver");
        assert_eq!(cfg.claim_interval_seconds, 0);
        assert!(!cfg.enabled);
    }

    #[test]
    fn explicit_paths_are_resolved() {
        let cfg = ZcodeConfig {
            cli_path: r"C:\Tools\ZCode\resources\glm\zcode.cjs".into(),
            node_path: r"C:\node\node.exe".into(),
            data_dir: r"D:\profiles\demo\.zcode".into(),
            browser_path: r"C:\chrome.exe".into(),
            ..ZcodeConfig::default()
        };
        assert_eq!(
            cfg.resolved_cli_path(),
            PathBuf::from(r"C:\Tools\ZCode\resources\glm\zcode.cjs")
        );
        assert_eq!(cfg.resolved_node(), r"C:\node\node.exe");
        assert_eq!(
            cfg.resolved_data_dir(),
            PathBuf::from(r"D:\profiles\demo\.zcode")
        );
        assert_eq!(cfg.resolved_browser(), PathBuf::from(r"C:\chrome.exe"));
    }

    #[test]
    fn config_path_is_data_zcode_json() {
        let path = config_path_for(Path::new(r"D:\ModelGate\bridge.config.json"));
        assert!(path.ends_with(Path::new("data").join("zcode.json")));
    }
}
