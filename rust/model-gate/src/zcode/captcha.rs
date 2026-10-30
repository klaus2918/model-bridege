//! ZCode 验证码 worker：用本机 Chrome/Edge + 阿里云官方 SDK 求 `VERIFY_PARAM`。
//!
//! 与 `zcode2api/captcha_node/solver_pw.js` 同形：真实无头浏览器加载官方 SDK，
//! `initAliyunCaptcha` → `startTracelessVerification` → success 回调取参。
//! 不模拟 DOM、不绕过 SDK；浏览器路径由配置提供（可指向 Chrome/Edge）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use headless_chrome::{Browser, LaunchOptions};

const PAGE_HTML: &str = include_str!("../../assets/zcode_captcha.html");

/// 验证码池上限：一次最多缓存 2 个已求参数，兼顾并发 claim 与内存。
pub const POOL_MAX: usize = 2;
/// 验证码参数 TTL：上游校验窗口约 100s，这里留 5s 余量。
pub const POOL_TTL: Duration = Duration::from_secs(95);

#[derive(Debug, Clone)]
pub struct CaptchaToken {
    pub param: String,
    pub region: String,
    pub solved_ms: u128,
    solved_at: Instant,
}

impl CaptchaToken {
    fn fresh(&self) -> bool {
        self.solved_at.elapsed() < POOL_TTL
    }
}

/// 进程内验证码池：避免每次 claim 都冷启动浏览器。
///
/// 池只保存验证码参数（不落盘、不写日志）；`invalidate_all` 用于上游返回
/// 3007（验证码校验失败）时立即废弃整池，避免继续用已失效参数。
#[derive(Debug)]
pub struct CaptchaPool {
    entries: Mutex<VecDeque<CaptchaToken>>,
    /// 同一时刻只启动一个浏览器求解，避免并发爆内存。
    solve_lock: tokio::sync::Mutex<()>,
    invalidations: AtomicU64,
}

impl Default for CaptchaPool {
    fn default() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            solve_lock: tokio::sync::Mutex::new(()),
            invalidations: AtomicU64::new(0),
        }
    }
}

impl CaptchaPool {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, VecDeque<CaptchaToken>> {
        self.entries.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn prune_locked(&self, entries: &mut VecDeque<CaptchaToken>) {
        entries.retain(CaptchaToken::fresh);
        while entries.len() > POOL_MAX {
            entries.pop_front();
        }
    }

    fn take_fresh(&self) -> Option<CaptchaToken> {
        let mut entries = self.lock_entries();
        self.prune_locked(&mut entries);
        entries.pop_back()
    }

    /// 池内仍新鲜的验证码数量。
    pub fn available(&self) -> usize {
        let mut entries = self.lock_entries();
        self.prune_locked(&mut entries);
        entries.len()
    }

    /// 废弃整池；返回被清除的数量。3007 时调用。
    pub fn invalidate_all(&self) -> usize {
        let mut entries = self.lock_entries();
        let count = entries.len();
        entries.clear();
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        count
    }

    /// 取一个验证码：池内有新鲜参数直接复用，否则求解一次。
    ///
    /// 求解锁保证同一时刻只有一个浏览器实例；等到锁后再次检查池，
    /// 避免并发请求重复求解。
    pub async fn acquire(&self, solver: ZcodeCaptcha) -> Result<CaptchaToken, String> {
        if let Some(token) = self.take_fresh() {
            return Ok(token);
        }
        let _guard = self.solve_lock.lock().await;
        if let Some(token) = self.take_fresh() {
            return Ok(token);
        }
        solver.solve().await
    }

    /// 预先求解 `want` 个参数（上限 POOL_MAX），供批量 claim 使用。
    /// 返回池内最终可用的数量。
    pub async fn fill(&self, solver: ZcodeCaptcha, want: usize) -> Result<usize, String> {
        let target = want.clamp(1, POOL_MAX);
        let _guard = self.solve_lock.lock().await;
        loop {
            let current = self.available();
            if current >= target {
                return Ok(current);
            }
            let token = solver.solve().await?;
            let mut entries = self.lock_entries();
            self.prune_locked(&mut entries);
            if entries.len() < POOL_MAX {
                entries.push_back(token);
            }
        }
    }

    /// 诊断计数：累计失效次数（不含 TTL 自然过期）。
    pub fn invalidations(&self) -> u64 {
        self.invalidations.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct ZcodeCaptcha {
    pub browser_path: PathBuf,
    pub scene: String,
    pub region: String,
    pub prefix: String,
}

impl ZcodeCaptcha {
    pub fn new(
        browser_path: impl Into<PathBuf>,
        scene: impl Into<String>,
        region: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        Self {
            browser_path: browser_path.into(),
            scene: scene.into(),
            region: region.into(),
            prefix: prefix.into(),
        }
    }

    fn page_path(&self) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join("model-gate-zcode");
        std::fs::create_dir_all(&dir).map_err(|err| format!("创建验证码临时目录失败：{err}"))?;
        let path = dir.join("captcha.html");
        std::fs::write(&path, PAGE_HTML).map_err(|err| format!("写入验证码页面失败：{err}"))?;
        Ok(path)
    }

    /// 同步求解；调用方用 `spawn_blocking` 包一层，避免阻塞 Tokio worker。
    pub fn solve_blocking(&self) -> Result<CaptchaToken, String> {
        if !self.browser_path.is_file() {
            return Err(format!(
                "未找到浏览器：{}（请在 ZCode 设置里配置 Chrome/Edge 路径）",
                self.browser_path.display()
            ));
        }
        let page = self.page_path()?;
        let page_url = format!("file:///{}", page.display().to_string().replace('\\', "/"));
        let started = Instant::now();
        let options = LaunchOptions::default_builder()
            .path(Some(self.browser_path.clone()))
            .headless(true)
            .args(vec![
                std::ffi::OsStr::new("--disable-blink-features=AutomationControlled"),
                std::ffi::OsStr::new("--lang=zh-CN"),
                std::ffi::OsStr::new("--no-sandbox"),
                std::ffi::OsStr::new("--disable-dev-shm-usage"),
            ])
            .build()
            .map_err(|err| format!("创建浏览器启动参数失败：{err}"))?;
        let browser = Browser::new(options).map_err(|err| format!("启动浏览器失败：{err}"))?;
        let tab = browser
            .new_tab()
            .map_err(|err| format!("创建浏览器页面失败：{err}"))?;
        tab.set_user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36",
            Some("zh-CN,zh;q=0.9"),
            Some("Win32"),
        )
        .map_err(|err| format!("设置浏览器 UA 失败：{err}"))?;
        tab.enable_stealth_mode()
            .map_err(|err| format!("启用浏览器 stealth 失败：{err}"))?;
        tab.navigate_to(&page_url)
            .map_err(|err| format!("打开验证码页面失败：{err}"))?
            .wait_until_navigated()
            .map_err(|err| format!("等待验证码页面加载失败：{err}"))?;

        let ready_started = Instant::now();
        loop {
            let ready = tab
                .evaluate("typeof window.initAliyunCaptcha === 'function'", false)
                .ok()
                .and_then(|r| r.value)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if ready {
                break;
            }
            if ready_started.elapsed() > Duration::from_secs(20) {
                return Err("阿里云验证码 SDK 20 秒内未就绪".to_string());
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let script = format!(
            r##"(async () => {{
  window.AliyunCaptchaConfig = {{ region: "{region}", prefix: "{prefix}" }};
  return await new Promise((resolve) => {{
    let done = false;
    const finish = (v) => {{ if (!done) {{ done = true; resolve(v || null); }} }};
    try {{
      window.initAliyunCaptcha({{
        SceneId: "{scene}",
        mode: "popup",
        language: "zh-CN",
        showErrorTip: false,
        element: "#cap-holder",
        button: "#cap-btn",
        getInstance: (instance) => {{
          try {{ instance.startTracelessVerification(); }}
          catch (e) {{ window.__zcodeError = "getInstance: " + String(e); finish(null); }}
        }},
        success: (p) => {{
          const v = typeof p === "string" ? p : (p && p.captchaVerifyParam);
          finish(v);
        }},
        fail: (e) => {{ window.__zcodeError = "fail: " + String(e); finish(null); }},
        onError: (e) => {{ window.__zcodeError = "onError: " + String(e); finish(null); }},
      }});
    }} catch (e) {{ window.__zcodeError = "init: " + String(e); finish(null); }}
    setTimeout(() => finish(null), 20000);
  }});
}})()"##,
            region = self.region,
            prefix = self.prefix,
            scene = self.scene
        );
        let result = tab
            .evaluate(&script, true)
            .map_err(|err| format!("执行验证码脚本失败：{err}"))?;
        let param = result
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty());
        let Some(param) = param else {
            let err = tab
                .evaluate("window.__zcodeError || ''", false)
                .ok()
                .and_then(|r| r.value)
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            return Err(if err.is_empty() {
                "验证码求解未产出 VERIFY_PARAM".to_string()
            } else {
                format!("验证码求解失败：{err}")
            });
        };
        Ok(CaptchaToken {
            param,
            region: self.region.clone(),
            solved_ms: started.elapsed().as_millis(),
            solved_at: Instant::now(),
        })
    }

    pub async fn solve(&self) -> Result<CaptchaToken, String> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.solve_blocking())
            .await
            .map_err(|err| format!("验证码任务失败：{err}"))?
    }
}

pub fn default_scene() -> String {
    "11xygtvd".to_string()
}

pub fn default_region() -> String {
    "cn".to_string()
}

pub fn default_prefix() -> String {
    "no8xfe".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_html_contains_sdk_and_slots() {
        assert!(PAGE_HTML.contains("AliyunCaptcha.js"));
        assert!(PAGE_HTML.contains("cap-holder"));
        assert!(PAGE_HTML.contains("cap-btn"));
    }

    #[test]
    fn default_config_shape() {
        assert!(!default_scene().is_empty());
        assert_eq!(default_region(), "cn");
        assert!(!default_prefix().is_empty());
    }

    fn token(param: &str, age: Duration) -> CaptchaToken {
        CaptchaToken {
            param: param.to_string(),
            region: "cn".to_string(),
            solved_ms: 1,
            solved_at: Instant::now() - age,
        }
    }

    #[test]
    fn pool_prunes_expired_and_keeps_fresh() {
        let pool = CaptchaPool::new();
        pool.lock_entries()
            .push_back(token("old", POOL_TTL + Duration::from_secs(1)));
        pool.lock_entries()
            .push_back(token("fresh", Duration::from_secs(1)));
        assert_eq!(pool.available(), 1);
        let got = pool.take_fresh().expect("fresh token");
        assert_eq!(got.param, "fresh");
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn pool_invalidate_all_clears_and_counts() {
        let pool = CaptchaPool::new();
        pool.lock_entries()
            .push_back(token("a", Duration::from_secs(1)));
        pool.lock_entries()
            .push_back(token("b", Duration::from_secs(2)));
        assert_eq!(pool.invalidate_all(), 2);
        assert_eq!(pool.available(), 0);
        assert_eq!(pool.invalidations(), 1);
    }

    #[test]
    fn pool_caps_at_max_entries() {
        let pool = CaptchaPool::new();
        for index in 0..(POOL_MAX + 3) {
            pool.lock_entries()
                .push_back(token(&format!("t{index}"), Duration::from_secs(1)));
        }
        assert_eq!(pool.available(), POOL_MAX);
    }
}
