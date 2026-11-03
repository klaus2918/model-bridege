//! 凭据的静态加密（DPAPI）。
//!
//! Windows 使用 DPAPI（`CryptProtectData` / `CryptUnprotectData`），密文只能由
//! 同一用户在同一台机器上解开；非 Windows 暂时拒绝（Model-Gate 当前是 Windows
//! 桌面产品，避免把密钥以明文写进 SQLite）。
//!
//! 存储格式：`dpapi:v1:` + DPAPI blob。前缀用于未来迁移/识别。

const PREFIX: &[u8] = b"dpapi:v1:";

#[cfg(windows)]
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "DPAPI 加密失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let blob =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    let mut out = Vec::with_capacity(PREFIX.len() + blob.len());
    out.extend_from_slice(PREFIX);
    out.extend_from_slice(&blob);
    Ok(out)
}

#[cfg(windows)]
pub fn open(sealed: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let Some(blob) = sealed.strip_prefix(PREFIX) else {
        return Err("密钥格式不是 dpapi:v1".to_string());
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "DPAPI 解密失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    Ok(plain)
}

#[cfg(not(windows))]
pub fn seal(_plain: &[u8]) -> Result<Vec<u8>, String> {
    Err("凭据加密当前仅支持 Windows DPAPI".to_string())
}

#[cfg(not(windows))]
pub fn open(_sealed: &[u8]) -> Result<Vec<u8>, String> {
    Err("凭据解密当前仅支持 Windows DPAPI".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let plain = b"credential-for-test";
        let sealed = seal(plain).expect("seal");
        assert!(sealed.starts_with(PREFIX));
        assert_ne!(&sealed[PREFIX.len()..], plain);
        let opened = open(&sealed).expect("open");
        assert_eq!(opened, plain);
    }

    #[test]
    fn open_rejects_non_dpapi_blob() {
        assert!(open(b"plain-token").is_err());
    }
}
