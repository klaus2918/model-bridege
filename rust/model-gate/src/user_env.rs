//! Windows 用户级环境变量（`HKCU\Environment`）读写。
//!
//! 背景：Model-Gate 的密钥只允许走环境变量通道 —— 写入只写用户变量
//! （`HKCU\Environment`），读取只认「进程环境」与「本进程写过的用户变量」。
//!
//! 为什么不用 `setx`：
//! - `setx` 会把值截断到 1024 字符，且对特殊字符不友好；
//! - `setx` 只写不读，读用户变量仍得走注册表 API；
//! - `setx` 不广播 `WM_SETTINGCHANGE`，新进程拿不到新值。
//!
//! 安全约定：
//! - 密钥值**永不**进入日志与错误信息，错误只带变量名与 Win32 错误码；
//! - 读取只认 `REG_SZ` / `REG_EXPAND_SZ` 两类字符串值。

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
    use super::*;

    pub(super) fn session_var_set(_name: &str, _set: Option<&str>) -> Option<String> {
        None
    }

    pub(super) fn read_user_var_impl(_name: &str) -> Option<String> {
        None
    }

    pub(super) fn write_user_var_impl(_name: &str, _value: &str) -> Result<(), String> {
        Err("当前平台不支持用户级环境变量写入".to_string())
    }

    #[allow(dead_code)] // 预留：面板「撤销写入」接入后才有生产调用方
    pub(super) fn delete_user_var_impl(_name: &str) -> Result<(), String> {
        Err("当前平台不支持用户级环境变量删除".to_string())
    }

    pub(super) fn notify_environment_changed() {}
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

    fn cleanup() {
        let _ = delete_user_var(TEST_VAR);
    }

    #[test]
    fn write_then_read_user_var_round_trips() {
        cleanup();
        let value = "sk-test-value-1234567890";
        write_user_var(TEST_VAR, value).expect("写入用户变量应成功");
        let read_back = read_user_var(TEST_VAR);
        cleanup();
        assert_eq!(read_back.as_deref(), Some(value));
    }

    #[test]
    fn write_overwrites_existing_value() {
        cleanup();
        write_user_var(TEST_VAR, "first").expect("首次写入应成功");
        write_user_var(TEST_VAR, "second").expect("覆盖写入应成功");
        let read_back = read_user_var(TEST_VAR);
        cleanup();
        assert_eq!(read_back.as_deref(), Some("second"));
    }

    #[test]
    fn missing_var_reads_as_none() {
        cleanup();
        assert!(read_user_var("MODEL_GATE_TEST_VAR_SHOULD_NOT_EXIST").is_none());
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
