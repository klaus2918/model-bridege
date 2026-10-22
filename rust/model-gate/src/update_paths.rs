//! 应用内更新的「路径事实」：下载目录、安装目录与「打开位置」。
//!
//! 背景：自动更新此前全程黑盒——安装包下到哪个目录、最终装到哪个目录，
//! 用户都看不到；面板里也只有「下载并安装」这种含糊按钮。本模块把路径
//! 这件事收成一处：
//!
//! - **下载目录**默认是 Windows 的「下载」文件夹（Known Folder `FOLDERID_Downloads`，
//!   跟随系统重定向，OneDrive 场景也正确）；取不到时回退 `%USERPROFILE%\Downloads`，
//!   再取不到才回退历史默认 `%LOCALAPPDATA%\Model-Gate\updates`；
//! - **安装目录**以 Windows 注册表里 Inno Setup 卸载项记录的安装位置为准：
//!   `InstallLocation` → `DisplayIcon` → `UninstallString` 逐级回退
//!   （真实机器上这三条不一定都存在，只有一条也能定位到安装目录），
//!   都读不到时回退 exe 所在目录；
//! - **打开位置**只允许打开服务端已知的路径，不接受任意路径入参。

use std::path::{Path, PathBuf};

/// Inno Setup 的 AppId，必须与 `ModelGate.iss` 的 `AppId` 保持一致。
///
/// Inno 把 AppId 原样写成卸载项子键名，并追加 `_is1` 后缀。
pub const APP_ID: &str = "{8F3C4A21-6D9E-4B7A-9C15-2B0E7D5F4A63}";

/// 安装器写入的 exe 文件名（与 `ModelGate.iss` 的 `MyAppExeName` 一致）。
pub const INNO_EXE_NAME: &str = "Model-Gate.exe";

/// 旧版：应用数据目录名（仅用于识别「历史默认下载目录」）。
const LEGACY_APP_DATA_DIR: &str = "Model-Gate";

/// 旧版：下载子目录名（历史默认下载目录 = `%LOCALAPPDATA%\Model-Gate\updates`）。
const LEGACY_DOWNLOAD_SUBDIR: &str = "updates";

/// 没有 `LOCALAPPDATA` 时的回退目录名（放在临时目录下）。
const FALLBACK_SUBDIR: &str = "model-gate-updates";

/// 安装包默认下载目录：**Windows 的「下载」文件夹**。
///
/// 取值顺序：
/// 1. `SHGetKnownFolderPath(FOLDERID_Downloads)` —— 系统真实的下载目录
///    （跟随用户把下载重定向到 OneDrive 等场景）；
/// 2. `%USERPROFILE%\Downloads`；
/// 3. 历史默认 `%LOCALAPPDATA%\Model-Gate\updates`（最后兜底，保证功能不丢）。
pub fn download_dir() -> PathBuf {
    downloads_dir().unwrap_or_else(legacy_download_dir)
}

/// Windows 的「下载」目录；取不到时返回 `None`（由调用方决定回退）。
pub fn downloads_dir() -> Option<PathBuf> {
    imp::downloads_dir().or_else(user_profile_downloads)
}

/// 历史默认下载目录：`%LOCALAPPDATA%\Model-Gate\updates`。
///
/// 只用于「把用户此前保存下来的旧默认目录迁到新的 Windows「下载」目录」。
pub fn legacy_download_dir() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").ok();
    legacy_download_dir_with(local.as_deref(), &std::env::temp_dir())
}

/// `%USERPROFILE%\Downloads` 兜底（Known Folder 不可用时）。
fn user_profile_downloads() -> Option<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")?;
    let dir = PathBuf::from(profile).join("Downloads");
    dir.is_dir().then_some(dir)
}

/// 用户自定义下载目录的 DB setting key（存绝对路径；空值表示用默认目录）。
pub const DOWNLOAD_DIR_SETTING: &str = "update.download_dir";

/// 按设置值解析下载目录：自定义路径优先，空值/缺失回退 [`download_dir`]。
///
/// **历史默认目录自动迁移**：旧版本把默认目录写进了设置（用户点过「保存目录」），
/// 这里把它视为「未自定义」，从而跟着新默认一起落到 Windows「下载」目录。
pub fn resolve_download_dir(custom: Option<&str>) -> PathBuf {
    match custom.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if is_legacy_download_dir(&path) {
                download_dir()
            } else {
                path
            }
        }
        None => download_dir(),
    }
}

/// 判断路径是否就是「历史默认下载目录」（`%LOCALAPPDATA%\Model-Gate\updates`）。
pub fn is_legacy_download_dir(path: &Path) -> bool {
    same_dir(path, &legacy_download_dir())
}

/// 校验用户输入的下载目录。
///
/// 规则：必须是非空绝对路径、不能指向文件、必须能创建且能写入。
/// 校验通过时返回规范化后的路径；失败时返回可直接展示给用户的中文原因。
pub fn validate_download_dir(raw: &str) -> Result<PathBuf, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("下载目录不能为空".to_string());
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!(
            "请填写绝对路径（例如 D:\\Model-Gate\\updates），当前填的是：{text}"
        ));
    }
    if path.is_file() {
        return Err(format!("这个路径是文件而不是目录：{}", path.display()));
    }
    std::fs::create_dir_all(&path)
        .map_err(|err| format!("无法创建下载目录 {}：{err}", path.display()))?;
    // 写探针：能建目录不等于能写入（如 Program Files 下的子目录）。
    let probe = path.join(".modelgate-write-test");
    std::fs::write(&probe, b"ok")
        .map_err(|err| format!("下载目录不可写 {}：{err}", path.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(path)
}

/// [`legacy_download_dir`] 的纯函数版本，便于单测。
pub fn legacy_download_dir_with(local_app_data: Option<&str>, temp: &Path) -> PathBuf {
    match local_app_data
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(base) => Path::new(base)
            .join(LEGACY_APP_DATA_DIR)
            .join(LEGACY_DOWNLOAD_SUBDIR),
        None => temp.join(FALLBACK_SUBDIR),
    }
}

/// 安装器实际写入的目录：Inno 卸载项记录的安装位置。
///
/// 读不到（未安装过 / 非 Windows / 权限不足 / 记录已失效）时返回 `None`。
pub fn registered_install_dir() -> Option<PathBuf> {
    imp::registered_install_dir()
}

/// [`effective_install_dir`] 的纯函数版本，便于单测。
///
/// 注册表位置由调用方传入：更新服务启动时读一次并缓存，避免每次渲染状态
/// 都去查注册表。
pub fn effective_install_dir_with(registered: Option<&Path>, exe_dir: &Path) -> PathBuf {
    match registered {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => exe_dir.to_path_buf(),
    }
}

/// 生效安装目标：生效安装目录下的安装器 exe 名（升级时被覆盖的那个文件）。
///
/// 纯函数版本：注册表位置由调用方传入，便于单测与复用已读到的值。
pub fn effective_install_target_with(registered: Option<&Path>, exe_dir: &Path) -> PathBuf {
    effective_install_dir_with(registered, exe_dir).join(INNO_EXE_NAME)
}

/// 从 `DisplayIcon` 值里取程序所在目录。
///
/// 卸载项里的 `DisplayIcon` 形如 `D:\mysoft\Model-Gate\Model-Gate.exe`
/// 或带图标序号 `...,0`；两种写法都要能定位到目录。
pub fn dir_from_icon_value(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim().trim_matches('"');
    // 只有末尾是纯数字时才当成图标序号剥掉（路径里本身可能带逗号）。
    let without_index = match trimmed.rsplit_once(',') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => trimmed,
    };
    let path = PathBuf::from(without_index);
    if !path.is_absolute() {
        return None;
    }
    path.parent().map(Path::to_path_buf)
}

/// 从 `UninstallString` 值里取程序所在目录。
///
/// 形如 `"D:\mysoft\Model-Gate\unins000.exe"`，也可能带 `/VERYSILENT` 之类参数。
pub fn dir_from_uninstall_string(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    // 取第一对引号之间的内容；没有引号时退回整串（去掉尾部参数）。
    let first = trimmed
        .split('"')
        .nth(1)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| trimmed.split_whitespace().next().unwrap_or(""));
    let path = PathBuf::from(first);
    if !path.is_absolute() {
        return None;
    }
    path.parent().map(Path::to_path_buf)
}

/// 比较两个目录是否指向同一位置（Windows 下忽略大小写与末尾分隔符）。
pub fn same_dir(left: &Path, right: &Path) -> bool {
    normalize_dir(left) == normalize_dir(right)
}

/// 目录归一化：去掉末尾分隔符（Windows 下再忽略大小写）。
fn normalize_dir(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    let trimmed = text.trim_end_matches(['\\', '/']);
    let value = if trimmed.is_empty() {
        text.as_str()
    } else {
        trimmed
    };
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.to_string()
    }
}

/// 在系统文件管理器中打开路径。
///
/// `select` 为真时表示「定位并选中该文件」，否则表示「打开该目录」。
pub fn open_in_explorer(path: &Path, select: bool) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    imp::open_in_explorer(path, select)
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_QUERY_VALUE, KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SZ,
    };
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// Inno 卸载项子键路径。
    fn uninstall_subkey(app_id: &str) -> String {
        format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{app_id}_is1")
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 读取一个注册表字符串值（`REG_SZ` / `REG_EXPAND_SZ`）。
    fn query_string_value(
        root: HKEY,
        flags: u32,
        subkey: &str,
        value_name: &str,
    ) -> Option<String> {
        let subkey_wide = to_wide(subkey);
        let mut handle: HKEY = std::ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                subkey_wide.as_ptr(),
                0,
                KEY_QUERY_VALUE | flags,
                &mut handle,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let name_wide = to_wide(value_name);
        // 第一次只问长度；REG_SZ 会被补一个结尾 NUL，长度含它。
        let mut size: u32 = 0;
        let mut value_type: u32 = 0;
        let status = unsafe {
            RegQueryValueExW(
                handle,
                name_wide.as_ptr(),
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
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            unsafe { RegCloseKey(handle) };
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        let mut read_size = size;
        let status = unsafe {
            RegQueryValueExW(
                handle,
                name_wide.as_ptr(),
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
        let trimmed = text.trim_end_matches('\0').trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Windows「下载」目录：`SHGetKnownFolderPath(FOLDERID_Downloads)`。
    ///
    /// 走 Known Folder 而不是拼 `%USERPROFILE%\Downloads`：用户把「下载」
    /// 重定向到 OneDrive / 其他盘时，这里拿到的是系统真实路径。
    pub(super) fn downloads_dir() -> Option<PathBuf> {
        unsafe {
            let mut path: *mut u16 = std::ptr::null_mut();
            // KF_FLAG_DEFAULT = 0；htoken = NULL 表示当前用户。
            let hr = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut path);
            // S_OK = 0；失败时 ppszPath 不保证被赋值。
            if hr != 0 || path.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *path.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
            CoTaskMemFree(path.cast());
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        }
    }

    /// 从卸载项里定位安装目录：逐个来源尝试，任一命中即可。
    ///
    /// 真实机器上这三条不一定都存在（例如注册表被清理过、卸载项键名不同），
    /// 因此每条都要能独立定位到目录。
    pub(super) fn registered_install_dir() -> Option<PathBuf> {
        let subkey = uninstall_subkey(super::APP_ID);
        // 顺序：当前用户（安装器以 lowest 权限安装时写这里）→ 本机 64 位视图 → 本机 32 位视图。
        let roots: [(HKEY, u32); 3] = [
            (HKEY_CURRENT_USER, 0),
            (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
            (HKEY_LOCAL_MACHINE, 0),
        ];
        for (root, flags) in roots {
            // 来源 1：InstallLocation（Inno 标准写法，直接就是目录）。
            if let Some(value) = query_string_value(root, flags, &subkey, "InstallLocation") {
                let dir = PathBuf::from(value);
                if dir.is_dir() {
                    return Some(dir);
                }
            }
            // 来源 2：DisplayIcon（`...Model-Gate.exe` 或 `...,0`）。
            if let Some(value) = query_string_value(root, flags, &subkey, "DisplayIcon") {
                if let Some(dir) = super::dir_from_icon_value(&value) {
                    if dir.is_dir() {
                        return Some(dir);
                    }
                }
            }
            // 来源 3：UninstallString（`"...\unins000.exe"`）。
            if let Some(value) = query_string_value(root, flags, &subkey, "UninstallString") {
                if let Some(dir) = super::dir_from_uninstall_string(&value) {
                    if dir.is_dir() {
                        return Some(dir);
                    }
                }
            }
        }
        None
    }

    pub(super) fn open_in_explorer(path: &Path, select: bool) -> Result<(), String> {
        if select {
            // explorer.exe /select,"<path>"：在资源管理器中定位并选中文件。
            let arg = format!("/select,\"{}\"", path.display());
            return std::process::Command::new("explorer")
                .arg(arg)
                .spawn()
                .map(|_| ())
                .map_err(|err| format!("打开资源管理器失败：{err}"));
        }
        let file = to_wide(&path.display().to_string());
        let verb = to_wide("open");
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if (result as isize) > 32 {
            return Ok(());
        }
        // ShellExecute 失败时退回 explorer（explorer 的退出码不可信，只看能否启动）。
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("打开目录失败：{err}"))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    pub(super) fn downloads_dir() -> Option<PathBuf> {
        None
    }

    pub(super) fn registered_install_dir() -> Option<PathBuf> {
        None
    }

    pub(super) fn open_in_explorer(path: &Path, select: bool) -> Result<(), String> {
        let target = if select {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(program)
            .arg(target)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("打开目录失败：{err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_download_dir_prefers_local_app_data() {
        let dir = legacy_download_dir_with(
            Some(r"C:\Users\<user>\AppData\Local"),
            Path::new(r"C:\Temp"),
        );
        assert_eq!(
            dir,
            PathBuf::from(r"C:\Users\<user>\AppData\Local")
                .join("Model-Gate")
                .join("updates")
        );
    }

    #[test]
    fn legacy_download_dir_falls_back_to_temp_without_local_app_data() {
        let dir = legacy_download_dir_with(None, Path::new(r"C:\Temp"));
        assert_eq!(dir, PathBuf::from(r"C:\Temp").join("model-gate-updates"));
    }

    #[test]
    fn legacy_download_dir_treats_blank_local_app_data_as_missing() {
        let dir = legacy_download_dir_with(Some("   "), Path::new(r"C:\Temp"));
        assert_eq!(dir, PathBuf::from(r"C:\Temp").join("model-gate-updates"));
    }

    #[test]
    fn resolve_download_dir_prefers_custom_path() {
        let dir = resolve_download_dir(Some(r"D:\Model-Gate\updates"));
        assert_eq!(dir, PathBuf::from(r"D:\Model-Gate\updates"));
    }

    #[test]
    fn resolve_download_dir_treats_blank_as_default() {
        let dir = resolve_download_dir(Some("   "));
        assert_eq!(dir, download_dir());
        let dir = resolve_download_dir(None);
        assert_eq!(dir, download_dir());
    }

    #[test]
    fn resolve_download_dir_migrates_legacy_default_to_downloads() {
        // 旧版本把默认目录存进了设置：升级后必须回到 Windows「下载」目录，
        // 不能继续留在 %LOCALAPPDATA% 里。
        let legacy = legacy_download_dir();
        let dir = resolve_download_dir(Some(&legacy.display().to_string()));
        assert_eq!(dir, download_dir());
        assert!(!same_dir(&dir, &legacy));
    }

    #[test]
    fn download_dir_is_downloads_when_available() {
        // Windows 上「下载」目录一定拿得到（Known Folder 或 USERPROFILE 兜底）。
        if cfg!(windows) {
            assert!(downloads_dir().is_some());
            assert_eq!(download_dir(), downloads_dir().unwrap());
        }
    }

    #[test]
    fn validate_download_dir_rejects_relative_path() {
        let err = validate_download_dir("updates").unwrap_err();
        assert!(err.contains("绝对路径"), "{err}");
    }

    #[test]
    fn validate_download_dir_rejects_empty() {
        let err = validate_download_dir("   ").unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
    }

    #[test]
    fn validate_download_dir_rejects_file() {
        let file = std::env::temp_dir().join("modelgate-validate-file.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = validate_download_dir(&file.display().to_string()).unwrap_err();
        assert!(err.contains("不是目录"), "{err}");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn validate_download_dir_creates_writable_dir() {
        let dir = std::env::temp_dir()
            .join("modelgate-validate-dir")
            .join(format!("{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resolved = validate_download_dir(&dir.display().to_string()).unwrap();
        assert!(resolved.is_dir());
        assert!(!resolved.join(".modelgate-write-test").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effective_install_dir_prefers_registered_location() {
        let dir = effective_install_dir_with(
            Some(Path::new(r"C:\Program Files\Model-Gate")),
            Path::new(r"D:\tools\Model-Gate"),
        );
        assert_eq!(dir, PathBuf::from(r"C:\Program Files\Model-Gate"));
    }

    #[test]
    fn effective_install_dir_falls_back_to_exe_dir() {
        let dir = effective_install_dir_with(None, Path::new(r"D:\tools\Model-Gate"));
        assert_eq!(dir, PathBuf::from(r"D:\tools\Model-Gate"));
    }

    #[test]
    fn install_target_uses_inno_exe_name() {
        let target =
            effective_install_dir_with(None, Path::new(r"D:\tools\Model-Gate")).join(INNO_EXE_NAME);
        assert_eq!(target, PathBuf::from(r"D:\tools\Model-Gate\Model-Gate.exe"));
    }

    #[test]
    fn install_target_with_prefers_registered_location() {
        let target = effective_install_target_with(
            Some(Path::new(r"C:\Program Files\Model-Gate")),
            Path::new(r"D:\tools\Model-Gate"),
        );
        assert_eq!(
            target,
            PathBuf::from(r"C:\Program Files\Model-Gate\Model-Gate.exe")
        );
    }

    #[test]
    fn dir_from_icon_value_handles_plain_and_indexed() {
        assert_eq!(
            dir_from_icon_value(r"D:\mysoft\Model-Gate\Model-Gate.exe"),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
        assert_eq!(
            dir_from_icon_value(r"D:\mysoft\Model-Gate\Model-Gate.exe,0"),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
        assert_eq!(
            dir_from_icon_value(r#""D:\mysoft\Model-Gate\Model-Gate.exe""#),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
    }

    #[test]
    fn dir_from_icon_value_keeps_comma_in_path() {
        assert_eq!(
            dir_from_icon_value(r"D:\a,b\Model-Gate.exe"),
            Some(PathBuf::from(r"D:\a,b"))
        );
    }

    #[test]
    fn dir_from_icon_value_rejects_relative() {
        assert_eq!(dir_from_icon_value("Model-Gate.exe"), None);
    }

    #[test]
    fn dir_from_uninstall_string_handles_quoted_and_args() {
        assert_eq!(
            dir_from_uninstall_string(r#""D:\mysoft\Model-Gate\unins000.exe""#),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
        assert_eq!(
            dir_from_uninstall_string(r#""D:\mysoft\Model-Gate\unins000.exe" /VERYSILENT"#),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
        assert_eq!(
            dir_from_uninstall_string(r"D:\mysoft\Model-Gate\unins000.exe"),
            Some(PathBuf::from(r"D:\mysoft\Model-Gate"))
        );
    }

    #[test]
    fn same_dir_ignores_trailing_separator_and_case() {
        assert!(same_dir(
            Path::new(r"D:\mysoft\Model-Gate"),
            Path::new(r"D:\mysoft\Model-Gate\")
        ));
        assert!(same_dir(
            Path::new(r"D:\mysoft\Model-Gate"),
            Path::new(r"d:\MYSOFT\model-gate")
        ));
        assert!(!same_dir(
            Path::new(r"D:\mysoft\Model-Gate"),
            Path::new(r"D:\mysoft\Other")
        ));
    }
}
