//! 用户级环境变量读写：Windows 走 `HKCU\Environment`，Linux 走 shell rc 托管块。
//!
//! 背景：Model-Gate 的密钥只允许走环境变量通道 —— 写入只写用户级变量，
//! 读取只认「进程环境」与「本进程写过的用户变量」。
//!
//! Windows 为什么不用 `setx`：
//! - `setx` 会把值截断到 1024 字符，且对特殊字符不友好；
//! - `setx` 只写不读，读用户变量仍得走注册表 API；
//! - `setx` 不广播 `WM_SETTINGCHANGE`，新进程拿不到新值。
//!
//! Linux 写入 `~/.bashrc`（及已存在的 `~/.zshrc`）里的幂等托管块
//! （`# >>> model-gate env (managed) >>>` … `<<<`）：只增删自己的 export 行，
//! 不碰用户其它内容；新开终端自动继承，已运行 shell 不受影响。
//!
//! 安全约定：
//! - 密钥值**永不**进入日志与错误信息，错误只带变量名与 Win32 错误码；
//! - Windows 读取只认 `REG_SZ` / `REG_EXPAND_SZ` 两类字符串值。

/// 读取 Windows 用户级环境变量。
///
/// 读取顺序由调用方决定（进程环境优先，见 `config::resolve_api_key`）；
/// 本函数只负责「从注册表读」这一段。
///
/// 非 Windows 平台恒返回 `None`。
pub fn read_user_var(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    imp::read_user_var_impl(name)
}

/// 写入 Windows 用户级环境变量（`HKCU\Environment`）。
///
/// 写成功后广播 `WM_SETTINGCHANGE`，让**新启动**的进程继承该值。
/// 已运行的进程读不到注册表里的新值 —— 本模块用会话内缓存兜底
/// （见 `remember_session_var`）。
///
/// 非 Windows 平台返回 `Err`。
pub fn write_user_var(name: &str, value: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("环境变量名不能为空".to_string());
    }
    if value.is_empty() {
        return Err("密钥值不能为空".to_string());
    }
    imp::write_user_var_impl(name, value)
}

/// 会话内缓存：记录本进程写过的用户变量。
///
/// 用途：Windows 的环境块是**进程启动时快照**的，运行中的进程读不到
/// 之后才写入注册表的值。写入后立刻把值记在这里，读取时优先查缓存，
/// 使「写入即生效」在当前进程内成立。
///
/// 缓存只存在于内存，不落盘、不进日志。
pub fn remember_session_var(name: &str, value: &str) {
    imp::session_var_set(name, Some(value));
}

/// 查询会话内缓存的值。
pub fn session_var(name: &str) -> Option<String> {
    imp::session_var_set(name, None)
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    /// 用户变量所在的子键：标准 Windows 用户环境变量位置。
    const ENVIRONMENT_KEY: &str = "Environment";

    /// 会话内缓存：变量名 -> 值。
    fn cache() -> &'static Mutex<HashMap<String, String>> {
        static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) fn session_var_set(name: &str, set: Option<&str>) -> Option<String> {
        let mut guard = cache().lock().ok()?;
        match set {
            Some(value) => {
                guard.insert(name.to_string(), value.to_string());
                None
            }
            None => guard.get(name).cloned(),
        }
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 打开 `HKCU\Environment`（不存在则创建）。
    fn open_environment_key(writable: bool) -> Result<HKEY, String> {
        let subkey = to_wide(ENVIRONMENT_KEY);
        let mut handle: HKEY = std::ptr::null_mut();
        let mut disposition = 0u32;
        let access = if writable {
            KEY_SET_VALUE | KEY_QUERY_VALUE
        } else {
            KEY_QUERY_VALUE
        };
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                access,
                std::ptr::null(),
                &mut handle,
                &mut disposition,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "打开 HKCU\\{ENVIRONMENT_KEY} 失败（错误码 {status}）"
            ));
        }
        Ok(handle)
    }

    pub(super) fn read_user_var_impl(name: &str) -> Option<String> {
        let handle = open_environment_key(false).ok()?;
        let value_name = to_wide(name);
        // 第一次调用只问长度；REG_SZ 会被补一个结尾 NUL，长度含它。
        let mut size: u32 = 0;
        let mut value_type: u32 = 0;
        let status = unsafe {
            RegQueryValueExW(
                handle,
                value_name.as_ptr(),
                std::ptr::null(),
                &mut value_type,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS || size == 0 {
            unsafe { RegCloseKey(handle) };
            return None;
        }
        if value_type != REG_SZ && value_type != 2 {
            // 只接受字符串类型；其它类型（BINARY / DWORD 等）不是环境变量。
            unsafe { RegCloseKey(handle) };
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let mut read_size = size;
        let status = unsafe {
            RegQueryValueExW(
                handle,
                value_name.as_ptr(),
                std::ptr::null(),
                &mut value_type,
                buffer.as_mut_ptr(),
                &mut read_size,
            )
        };
        unsafe { RegCloseKey(handle) };
        if status != ERROR_SUCCESS {
            return None;
        }
        // REG_SZ 是 UTF-16LE，按 u16 还原后去掉结尾 NUL。
        let words: Vec<u16> = buffer[..read_size as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16_lossy(&words);
        let trimmed = text.trim_end_matches('\0').to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    pub(super) fn write_user_var_impl(name: &str, value: &str) -> Result<(), String> {
        let handle = open_environment_key(true)?;
        let value_name = to_wide(name);
        let wide = to_wide(value);
        let byte_len = (wide.len() * std::mem::size_of::<u16>()) as u32;
        let status = unsafe {
            RegSetValueExW(
                handle,
                value_name.as_ptr(),
                0,
                REG_SZ,
                wide.as_ptr() as *const u8,
                byte_len,
            )
        };
        unsafe { RegCloseKey(handle) };
        if status != ERROR_SUCCESS {
            return Err(format!("写入用户变量 {name} 失败（错误码 {status}）"));
        }
        Ok(())
    }

    /// 删除用户变量（测试清理与显式撤销写入）。
    #[allow(dead_code)] // 预留：面板「撤销写入」接入后才有生产调用方
    pub(super) fn delete_user_var_impl(name: &str) -> Result<(), String> {
        use windows_sys::Win32::System::Registry::RegDeleteValueW;
        let handle = open_environment_key(true)?;
        let value_name = to_wide(name);
        let status = unsafe { RegDeleteValueW(handle, value_name.as_ptr()) };
        unsafe { RegCloseKey(handle) };
        if status != ERROR_SUCCESS {
            return Err(format!("删除用户变量 {name} 失败（错误码 {status}）"));
        }
        Ok(())
    }

    /// 广播 `WM_SETTINGCHANGE("Environment")`：让新进程继承新值。
    pub(super) fn notify_environment_changed() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let message = to_wide("Environment");
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                message.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                2000,
                std::ptr::null_mut(),
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::managed_block;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::OnceLock;

    fn cache() -> &'static Mutex<HashMap<String, String>> {
        static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub(super) fn session_var_set(name: &str, set: Option<&str>) -> Option<String> {
        let mut guard = cache().lock().ok()?;
        match set {
            Some(value) => {
                guard.insert(name.to_string(), value.to_string());
                None
            }
            None => guard.get(name).cloned(),
        }
    }

    /// 用户主目录。测试可用 `*_in` 变体注入。
    fn home_dir() -> Result<PathBuf, String> {
        match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
            _ => Err("无法确定用户主目录（HOME 未设置）".to_string()),
        }
    }

    pub(super) fn read_user_var_impl(name: &str) -> Option<String> {
        let home = home_dir().ok()?;
        read_user_var_in(&home, name)
    }

    pub(super) fn write_user_var_impl(name: &str, value: &str) -> Result<(), String> {
        let home = home_dir()?;
        write_user_var_in(&home, name, value)
    }

    #[allow(dead_code)] // 预留：面板「撤销写入」接入后才有生产调用方
    pub(super) fn delete_user_var_impl(name: &str) -> Result<(), String> {
        let home = home_dir()?;
        delete_user_var_in(&home, name)
    }

    pub(super) fn notify_environment_changed() {
        // Linux 没有 WM_SETTINGCHANGE 等价物：已运行的 shell 不会自动刷新，
        // 新开的终端/进程按 rc 文件继承。会话内缓存保证网关自身立即生效。
    }

    // ── 可注入 home 的实现（测试直接调用，生产包装 HOME） ──────────────

    /// shell rc 文件清单：`~/.bashrc` 总是主写入目标；`~/.zshrc` 已存在才同步
    /// （不存在说明用户不用 zsh，不要替他创建）。
    fn rc_files(home: &Path) -> Vec<PathBuf> {
        let mut files = vec![home.join(".bashrc")];
        let zshrc = home.join(".zshrc");
        if zshrc.is_file() {
            files.push(zshrc);
        }
        files
    }

    /// 测试访问器：托管块测试直接检查文件清单逻辑。
    #[cfg(test)]
    pub(super) fn rc_files_for_test(home: &Path) -> Vec<PathBuf> {
        rc_files(home)
    }

    pub(super) fn read_user_var_in(home: &Path, name: &str) -> Option<String> {
        for file in rc_files(home) {
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };
            if let Some((entries, _)) = managed_block::parse(&content) {
                if let Some((_, value)) = entries.iter().find(|(n, _)| n == name) {
                    return Some(value.clone());
                }
            }
        }
        None
    }

    pub(super) fn write_user_var_in(home: &Path, name: &str, value: &str) -> Result<(), String> {
        if !crate::key_file::is_valid_env_name(name) {
            return Err(format!("环境变量名不合法：{name}"));
        }
        for file in rc_files(home) {
            let content = std::fs::read_to_string(&file).unwrap_or_default();
            let (mut entries, _) = managed_block::parse(&content).unwrap_or_default();
            match entries.iter_mut().find(|(n, _)| n == name) {
                Some(slot) => slot.1 = value.to_string(),
                None => entries.push((name.to_string(), value.to_string())),
            }
            managed_block::write_file(&file, &content, &entries)?;
        }
        Ok(())
    }

    pub(super) fn delete_user_var_in(home: &Path, name: &str) -> Result<(), String> {
        for file in rc_files(home) {
            let content = std::fs::read_to_string(&file).unwrap_or_default();
            let Some((entries, _)) = managed_block::parse(&content) else {
                continue;
            };
            let kept: Vec<(String, String)> = entries
                .iter()
                .filter(|(n, _)| n != name)
                .cloned()
                .collect();
            if kept.len() != entries.len() {
                managed_block::write_file(&file, &content, &kept)?;
            }
        }
        Ok(())
    }
}

/// `~/.bashrc` / `~/.zshrc` 里的 Model-Gate 托管块：幂等的 `export` 行，
/// 非该块负责的内容一律不动。纯文本处理，与平台无关（Windows 侧不使用）。
#[cfg(not(windows))]
mod managed_block {
    pub const BLOCK_BEGIN: &str = "# >>> model-gate env (managed) >>>";
    pub const BLOCK_END: &str = "# <<< model-gate env (managed) <<<";

    /// 单引号包裹：单引号内除 `'` 外没有特殊字符，`'` 用 `'\''` 闭合再开。
    pub fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    pub fn shell_unquote(raw: &str) -> Option<String> {
        let inner = raw.trim();
        let inner = inner.strip_prefix('\'')?;
        let inner = inner.strip_suffix('\'')?;
        Some(inner.replace("'\\''", "'"))
    }

    /// 解析 rc 文件内容：返回托管块内的 (name, value) 列表；
    /// 块内不属于 `export NAME=...` 形式的行原样保留在 passthrough 里。
    /// 没有（或块不完整时按找到的行处理）返回 None 表示文件里没有托管块。
    pub fn parse(content: &str) -> Option<(Vec<(String, String)>, Vec<String>)> {
        let begin = content.find(BLOCK_BEGIN)?;
        let after_begin = begin + BLOCK_BEGIN.len();
        let end = content[after_begin..].find(BLOCK_END).map(|i| after_begin + i);
        let body_end = end.unwrap_or(content.len());
        let body = &content[after_begin..body_end];
        let mut entries = Vec::new();
        let mut passthrough = Vec::new();
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("export ") {
                if let Some(eq) = rest.find('=') {
                    let name = rest[..eq].trim();
                    let value_raw = rest[eq + 1..].trim();
                    if crate::key_file::is_valid_env_name(name) {
                        if let Some(value) = shell_unquote(value_raw) {
                            entries.push((name.to_string(), value));
                            continue;
                        }
                    }
                }
            }
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                passthrough.push(line.to_string());
            }
        }
        Some((entries, passthrough))
    }

    pub fn render(entries: &[(String, String)]) -> String {
        let mut out = String::new();
        out.push_str(BLOCK_BEGIN);
        out.push('\n');
        out.push_str("# 由 Model-Gate 管理（密钥环境变量）。请勿手动修改本块；\n");
        out.push_str("# 新终端会话自动生效：export 只写在 shell rc 里。\n");
        for (name, value) in entries {
            out.push_str(&format!("export {}={}\n", name, shell_quote(value)));
        }
        out.push_str(BLOCK_END);
        out.push('\n');
        out
    }

    /// 把新块写回文件：已有块就原位替换（含残缺块），没有就追加。
    /// 条目为空时整体移除托管块。原子写：临时文件 + rename。
    pub fn write_file(
        path: &std::path::Path,
        original: &str,
        entries: &[(String, String)],
    ) -> Result<(), String> {
        let mut updated = String::new();
        match original.find(BLOCK_BEGIN) {
            Some(begin) => {
                let after_begin = begin + BLOCK_BEGIN.len();
                let end = original[after_begin..]
                    .find(BLOCK_END)
                    .map(|i| after_begin + i + BLOCK_END.len());
                updated.push_str(&original[..begin]);
                if !entries.is_empty() {
                    updated.push_str(&render(entries));
                }
                if let Some(end) = end {
                    updated.push_str(original[end..].trim_start_matches('\n'));
                }
            }
            None => {
                updated.push_str(original);
                if !entries.is_empty() {
                    if !updated.is_empty() && !updated.ends_with('\n') {
                        updated.push('\n');
                    }
                    updated.push_str(&render(entries));
                }
            }
        }
        // 临时名唯一化：两个进程/线程并发写同一个 rc 文件时，共用固定名会让
        // 先 rename 的一方把临时文件挪走，后 rename 的一方直接失败。
        let tmp = path.with_extension(format!("mg-tmp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&tmp, updated.as_bytes())
            .map_err(|err| format!("写入 {} 失败：{err}", tmp.display()))?;
        let replaced = std::fs::rename(&tmp, path)
            .map_err(|err| format!("替换 {} 失败：{err}", path.display()));
        if replaced.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        replaced
    }
}

/// 写入成功后广播环境变更（供调用方在写完后调用）。
pub fn notify_environment_changed() {
    imp::notify_environment_changed()
}

/// 删除用户级环境变量（供测试清理与「撤销写入」使用）。
///
/// 不属于常规流程：密钥轮换场景下覆盖写即可，删除是显式动作。
#[allow(dead_code)] // 预留：面板「撤销写入」接入后才有生产调用方
pub fn delete_user_var(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("环境变量名不能为空".to_string());
    }
    imp::delete_user_var_impl(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用变量名：避免与用户真实变量冲突。
    const TEST_VAR: &str = "MODEL_GATE_TEST_USER_ENV_VAR";

    /// 临时 HOME：单测走可注入路径，绝不动用户真实的 ~/.bashrc
    /// （并行测试共享同一真实文件时读写会互相覆盖）。
    fn temp_home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mg-user-env-home-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时 HOME");
        dir
    }

    // 非 Windows 用可注入 home 的实现（隔离真实 rc 文件）；
    // Windows 写 HKCU\Environment（按值原子写，无共享文件竞争），沿用公开入口。
    #[cfg(not(windows))]
    fn write_in(home: &std::path::Path, name: &str, value: &str) -> Result<(), String> {
        imp::write_user_var_in(home, name, value)
    }
    #[cfg(windows)]
    fn write_in(_home: &std::path::Path, name: &str, value: &str) -> Result<(), String> {
        write_user_var(name, value)
    }

    #[cfg(not(windows))]
    fn read_in(home: &std::path::Path, name: &str) -> Option<String> {
        imp::read_user_var_in(home, name)
    }
    #[cfg(windows)]
    fn read_in(_home: &std::path::Path, name: &str) -> Option<String> {
        read_user_var(name)
    }

    fn cleanup_in(home: &std::path::Path, name: &str) {
        #[cfg(not(windows))]
        {
            let _ = imp::delete_user_var_in(home, name);
        }
        #[cfg(windows)]
        {
            let _ = home;
            let _ = delete_user_var(name);
        }
    }

    #[test]
    fn write_then_read_user_var_round_trips() {
        let home = temp_home("roundtrip");
        cleanup_in(&home, TEST_VAR);
        let value = "sk-test-value-1234567890";
        write_in(&home, TEST_VAR, value).expect("写入用户变量应成功");
        let read_back = read_in(&home, TEST_VAR);
        cleanup_in(&home, TEST_VAR);
        assert_eq!(read_back.as_deref(), Some(value));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_overwrites_existing_value() {
        // 与 round_trips 用不同变量名：cargo test 默认并行，共用变量会互相覆盖。
        const OVERWRITE_VAR: &str = "MODEL_GATE_TEST_USER_ENV_VAR_OVERWRITE";
        let home = temp_home("overwrite");
        cleanup_in(&home, OVERWRITE_VAR);
        write_in(&home, OVERWRITE_VAR, "first").expect("首次写入应成功");
        write_in(&home, OVERWRITE_VAR, "second").expect("覆盖写入应成功");
        let read_back = read_in(&home, OVERWRITE_VAR);
        cleanup_in(&home, OVERWRITE_VAR);
        assert_eq!(read_back.as_deref(), Some("second"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_var_reads_as_none() {
        let home = temp_home("missing");
        cleanup_in(&home, TEST_VAR);
        assert!(
            read_in(&home, "MODEL_GATE_TEST_VAR_SHOULD_NOT_EXIST").is_none()
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn empty_name_and_empty_value_are_rejected() {
        assert!(write_user_var("", "v").is_err());
        assert!(write_user_var("MODEL_GATE_TEST_VAR_X", "").is_err());
    }

    #[test]
    fn session_cache_remembers_written_value() {
        let name = "MODEL_GATE_TEST_SESSION_CACHE_VAR";
        assert!(session_var(name).is_none());
        remember_session_var(name, "cached");
        assert_eq!(session_var(name).as_deref(), Some("cached"));
    }
}

/// Linux shell rc 托管块测试（纯文本 + 临时目录，Windows 上不编译）。
#[cfg(all(test, not(windows)))]
mod managed_block_tests {
    use super::imp::rc_files_for_test;
    use super::managed_block;
    use std::path::Path;

    fn unique_home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mg-user-env-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时 HOME");
        dir
    }

    #[test]
    fn quote_round_trips_special_characters() {
        for value in [
            "sk-plain-key-123",
            "with 'single' quotes",
            "back\\slash\\path",
            "中文值『引号』",
            "double\"quote",
        ] {
            let quoted = managed_block::shell_quote(&value);
            assert_eq!(managed_block::shell_unquote(&quoted), Some(value.to_string()));
        }
    }

    #[test]
    fn write_read_delete_round_trip_in_sandboxed_home() {
        let home = unique_home("roundtrip");
        let value = "sk-test-value-456";
        super::imp::write_user_var_in(&home, "MG_TEST_KEY", value).expect("写入应成功");
        // bashrc 里出现托管块，且 export 行用单引号包裹。
        let bashrc = std::fs::read_to_string(home.join(".bashrc")).expect("bashrc 应存在");
        assert!(bashrc.contains(managed_block::BLOCK_BEGIN));
        assert!(bashrc.contains("export MG_TEST_KEY='sk-test-value-456'"));
        assert_eq!(
            super::imp::read_user_var_in(&home, "MG_TEST_KEY"),
            Some(value.to_string())
        );
        super::imp::delete_user_var_in(&home, "MG_TEST_KEY").expect("删除应成功");
        assert!(super::imp::read_user_var_in(&home, "MG_TEST_KEY").is_none());
        // 最后一个条目删掉后托管块应整体移除，不留垃圾。
        let bashrc = std::fs::read_to_string(home.join(".bashrc")).expect("bashrc 应存在");
        assert!(!bashrc.contains(managed_block::BLOCK_BEGIN));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_preserves_user_content_and_is_idempotent() {
        let home = unique_home("preserve");
        let bashrc = home.join(".bashrc");
        std::fs::write(&bashrc, "# my own rc\nalias ll='ls -l'\n").expect("预置内容");
        super::imp::write_user_var_in(&home, "MG_TEST_A", "a1").expect("首次写入");
        super::imp::write_user_var_in(&home, "MG_TEST_A", "a2").expect("覆盖写入");
        super::imp::write_user_var_in(&home, "MG_TEST_B", "b1").expect("追加写入");
        let content = std::fs::read_to_string(&bashrc).expect("读取");
        // 用户内容原样保留，且只有一个托管块。
        assert!(content.contains("# my own rc"));
        assert!(content.contains("alias ll='ls -l'"));
        assert_eq!(content.matches(managed_block::BLOCK_BEGIN).count(), 1);
        assert!(content.contains("export MG_TEST_A='a2'"));
        assert!(content.contains("export MG_TEST_B='b1'"));
        // 不存在 ~/.zshrc 时不创建。
        assert!(!home.join(".zshrc").exists());
        // zshrc 已存在时同步写入。
        std::fs::write(home.join(".zshrc"), "# zsh\n").expect("预置 zshrc");
        super::imp::write_user_var_in(&home, "MG_TEST_A", "a3").expect("同步 zshrc");
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).expect("读取 zshrc");
        assert!(zshrc.contains("export MG_TEST_A='a3'"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn parse_keeps_foreign_lines_in_block() {
        let content = format!(
            "{}\nexport MG_A='1'\n# comment\nweird line\n{}\n",
            managed_block::BLOCK_BEGIN,
            managed_block::BLOCK_END
        );
        let (entries, passthrough) = managed_block::parse(&content).expect("应识别托管块");
        assert_eq!(entries, vec![("MG_A".to_string(), "1".to_string())]);
        assert_eq!(passthrough, vec!["weird line".to_string()]);
    }

    #[test]
    fn rc_files_only_lists_existing_zshrc() {
        let home = unique_home("rcfiles");
        assert_eq!(rc_files_for_test(&home), vec![home.join(".bashrc")]);
        std::fs::write(home.join(".zshrc"), "").expect("创建 zshrc");
        assert_eq!(
            rc_files_for_test(&home),
            vec![home.join(".bashrc"), home.join(".zshrc")]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn paths_actually_resolved_under_home() {
        let home = Path::new("/nonexistent-home-for-test");
        // 只验证清单的构造逻辑；不真正读写。
        let files = rc_files_for_test(home);
        assert!(files.iter().all(|p| p.starts_with(home)));
    }
}
