//! ZCode 插件服务：配置 + OAuth 会话的进程内状态。
//!
//! 账号事实源在 `Db`，本服务不复制账号；只保存配置与 OAuth 临时 flow。

use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::captcha::{CaptchaPool, ZcodeCaptcha};
use super::config::{config_path_for, ZcodeConfig};
use super::oauth::ZcodeOAuth;

pub struct ZcodeService {
    config_path: PathBuf,
    config: RwLock<ZcodeConfig>,
    pub oauth: ZcodeOAuth,
    /// 验证码参数池：只在内存，不落盘；关闭插件即随进程释放。
    pub captcha: CaptchaPool,
}

impl ZcodeService {
    pub fn new(config_path: &Path) -> Self {
        let path = config_path_for(config_path);
        let config = ZcodeConfig::load(&path);
        Self {
            config_path: path,
            config: RwLock::new(config),
            oauth: ZcodeOAuth::new(),
            captcha: CaptchaPool::new(),
        }
    }

    pub fn config(&self) -> RwLockReadGuard<'_, ZcodeConfig> {
        self.config.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config_mut(&self) -> RwLockWriteGuard<'_, ZcodeConfig> {
        self.config.write().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let config = self.config();
        config.save(&self.config_path)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.config().resolved_data_dir()
    }

    pub fn cli_exists(&self) -> bool {
        let path = self.config().resolved_cli_path();
        path.is_file()
    }

    pub fn browser_exists(&self) -> bool {
        self.config().resolved_browser().is_file()
    }

    /// 当前配置对应的验证码求解器（路径/场景/区域都从配置读取）。
    pub fn captcha_solver(&self) -> ZcodeCaptcha {
        let cfg = self.config();
        ZcodeCaptcha::new(
            cfg.resolved_browser(),
            cfg.captcha_scene.clone(),
            cfg.captcha_region.clone(),
            cfg.captcha_prefix.clone(),
        )
    }
}
