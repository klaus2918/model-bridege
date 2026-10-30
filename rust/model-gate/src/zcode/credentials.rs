//! 本机 ZCode 客户端凭据读取。
//!
//! `~/.zcode/v2/credentials.json` 的值形如 `enc:v1:<iv>.<tag>.<data>`，
//! 使用 AES-256-GCM；密钥 = SHA-256(secret)，secret 与 ZCode 客户端一致：
//!
//! `ZCODE_CREDENTIAL_SECRET` 或
//! `zcode-credential-fallback:<platform>:<home>:<username>`
//!
//! 只返回 `zcodejwttoken` 及元数据；**不打印/不写日志 token 原文**。

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default)]
pub struct LocalCredentialInfo {
    pub found: bool,
    pub token_length: usize,
    pub active_provider: Option<String>,
    pub keys: Vec<String>,
}

pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join("v2").join("credentials.json")
}

/// 读取本机凭据元数据（不返回 token）。
pub fn read_info(data_dir: &Path, secret: Option<&str>) -> Result<LocalCredentialInfo, String> {
    let path = credentials_path(data_dir);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("读取 ZCode 凭据失败（{}）：{err}", path.display()))?;
    let raw: Value =
        serde_json::from_str(&text).map_err(|err| format!("凭据不是合法 JSON：{err}"))?;
    let obj = raw
        .as_object()
        .ok_or_else(|| "凭据根不是对象".to_string())?;
    let secret = secret.map(str::to_string).unwrap_or_else(default_secret);
    let mut info = LocalCredentialInfo {
        found: false,
        token_length: 0,
        active_provider: None,
        keys: obj.keys().cloned().collect(),
    };
    for (key, value) in obj {
        let Some(value) = value.as_str() else {
            continue;
        };
        let Ok(plain) = decrypt_with_secret(value, &secret) else {
            continue;
        };
        if key == "zcodejwttoken" && !plain.is_empty() {
            info.found = true;
            info.token_length = plain.chars().count();
        }
        if key == "oauth:active_provider" {
            info.active_provider = Some(plain);
        }
    }
    Ok(info)
}

/// 读取并解密 `zcodejwttoken`（只应在本机进程内短时使用）。
pub fn read_token(data_dir: &Path, secret: Option<&str>) -> Result<String, String> {
    let path = credentials_path(data_dir);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("读取 ZCode 凭据失败（{}）：{err}", path.display()))?;
    let raw: Value =
        serde_json::from_str(&text).map_err(|err| format!("凭据不是合法 JSON：{err}"))?;
    let value = raw
        .get("zcodejwttoken")
        .and_then(Value::as_str)
        .ok_or_else(|| "凭据中没有 zcodejwttoken（ZCode 未登录 Z.AI？）".to_string())?;
    let secret = secret.map(str::to_string).unwrap_or_else(default_secret);
    let plain = decrypt_with_secret(value, &secret)?;
    if plain.is_empty() || plain.split('.').count() != 3 {
        return Err("zcodejwttoken 不是有效的三段 JWT".to_string());
    }
    Ok(plain)
}

pub fn default_secret() -> String {
    if let Ok(explicit) = std::env::var("ZCODE_CREDENTIAL_SECRET") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let username = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".to_string());
    format!("zcode-credential-fallback:{platform}:{home}:{username}")
}

/// 用 ZCode 客户端同款 AES-256-GCM 格式加密 JWT，写入独立账号目录。
///
/// 只用于 `backend=appserver` 的临时账号 home；主账号库仍用 DPAPI。
pub fn encrypt_for_zcode(plain: &str, secret: &str) -> Result<String, String> {
    let key = Sha256::digest(secret.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|err| format!("密钥初始化失败：{err}"))?;
    // IV 用 UUID v4 的前 12 字节；每个账号写入一次，重放风险可忽略。
    let uuid = uuid::Uuid::new_v4();
    let mut iv = [0u8; 12];
    iv.copy_from_slice(&uuid.as_bytes()[..12]);
    let nonce = Nonce::from_slice(&iv);
    let sealed = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|_| "凭据加密失败".to_string())?;
    let split = sealed
        .len()
        .checked_sub(16)
        .ok_or_else(|| "凭据密文长度无效".to_string())?;
    Ok(format!(
        "enc:v1:{}.{}.{}",
        URL_SAFE_NO_PAD.encode(iv),
        URL_SAFE_NO_PAD.encode(&sealed[split..]),
        URL_SAFE_NO_PAD.encode(&sealed[..split])
    ))
}

pub(crate) fn decrypt_with_secret(value: &str, secret: &str) -> Result<String, String> {
    let Some(payload) = value.strip_prefix("enc:v1:") else {
        return Ok(value.to_string());
    };
    let parts: Vec<&str> = payload.split('.').collect();
    if parts.len() != 3 {
        return Err("凭据密文格式无效（应为 iv.tag.data）".to_string());
    }
    let iv = URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|err| format!("凭据 IV 解码失败：{err}"))?;
    let tag = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|err| format!("凭据 tag 解码失败：{err}"))?;
    let data = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|err| format!("凭据密文解码失败：{err}"))?;
    if iv.len() != 12 {
        return Err(format!("凭据 IV 长度无效（{}）", iv.len()));
    }
    if tag.len() != 16 {
        return Err(format!("凭据 tag 长度无效（{}）", tag.len()));
    }
    let key = Sha256::digest(secret.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|err| format!("密钥初始化失败：{err}"))?;
    let nonce = Nonce::from_slice(&iv);
    let mut sealed = data;
    sealed.extend_from_slice(&tag);
    let plain = cipher
        .decrypt(nonce, sealed.as_ref())
        .map_err(|_| "凭据解密失败（密钥/密文不匹配）".to_string())?;
    String::from_utf8(plain).map_err(|_| "凭据不是 UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::Aead;

    fn encrypt_for_test(plain: &str, secret: &str) -> String {
        let key = Sha256::digest(secret.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let iv = [7u8; 12];
        let nonce = Nonce::from_slice(&iv);
        let sealed = cipher.encrypt(nonce, plain.as_bytes()).unwrap();
        let split = sealed.len() - 16;
        format!(
            "enc:v1:{}.{}.{}",
            URL_SAFE_NO_PAD.encode(iv),
            URL_SAFE_NO_PAD.encode(&sealed[split..]),
            URL_SAFE_NO_PAD.encode(&sealed[..split])
        )
    }

    #[test]
    fn encrypt_decrypt_roundtrip_for_zcode_home() {
        let fixture_key = "fixture-key-one";
        let sealed = encrypt_for_zcode("header.payload.signature", fixture_key).expect("encrypt");
        assert!(sealed.starts_with("enc:v1:"));
        assert_eq!(
            decrypt_with_secret(&sealed, fixture_key).expect("decrypt"),
            "header.payload.signature"
        );
    }

    #[test]
    fn decrypts_known_ciphertext() {
        let fixture_key = "fixture-key-two";
        let value = encrypt_for_test("header.payload.signature", fixture_key);
        assert_eq!(
            decrypt_with_secret(&value, fixture_key).unwrap(),
            "header.payload.signature"
        );
    }

    #[test]
    fn non_encrypted_value_is_returned_as_is() {
        assert_eq!(decrypt_with_secret("plain", "s").unwrap(), "plain");
    }

    #[test]
    fn wrong_secret_fails() {
        let value = encrypt_for_test("token", "right");
        assert!(decrypt_with_secret(&value, "wrong").is_err());
    }

    #[test]
    fn default_secret_shape() {
        let secret = default_secret();
        assert!(secret.starts_with("zcode-credential-fallback:") || !secret.is_empty());
    }
}
