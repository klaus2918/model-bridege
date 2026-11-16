//! 接入认证：客户端访问网关本身的准入控制。
//!
//! 语义（全局中间件，一条规则管所有路由）：
//!
//! - `auth.enabled = false`：关闭验证，所有来源放行（与未引入认证前的行为一致）。
//! - 开启时：来源 IP 命中 `auth.ipAllowlist` **或** 携带有效认证码即放行，
//!   否则 401。默认白名单仅本机回环（127.0.0.1 / ::1）——本机客户端零配置可用，
//!   远程来源必须显式加白或携带认证码。
//!
//! 认证码（`mgk_` 前缀）与上游 apiKey 的立场一致：明文只在生成时返回一次，
//! 配置文件只存 SHA-256 摘要；任何接口（含本模块）都不会再吐出明文。
//!
//! 客户端 IP 取自 TCP 连接（`ConnectInfo`），不读 `X-Forwarded-For`——后者可被
//! 伪造，作为准入依据不安全。`ConnectInfo` 缺失（仅测试/内部直调路由会这样）
//! 按本机回环处理：生产路径由 `into_make_service_with_connect_info` 保证注入。

use std::net::IpAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::config::{AuthKeyRecord, AuthConfig, AUTH_KEYS_MAX};
use crate::proxy::{error_json, AppState};

/// 认证码前缀：与上游 key 视觉区分，面板与文档按它识别。
pub const KEY_PREFIX: &str = "mgk_";

// ─────────────────────────── 判定核心 ───────────────────────────

/// 请求来源 IP。`ConnectInfo` 缺失（测试直调）按本机回环处理，理由见模块注释。
fn client_ip(req: &axum::extract::Request) -> IpAddr {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]))
}

/// 从请求头提取候选认证码：`Authorization: Bearer <code>` 优先，其次 `x-api-key`。
/// OpenAI 与 Anthropic 两种客户端习惯都照顾到，不再加第三个头。
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let trimmed = value.trim();
        if let Some(token) = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .or_else(|| trimmed.strip_prefix("BEARER "))
        {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
        return None; // 带 Authorization 头但不是 Bearer 形态：不再看 x-api-key
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// 明文认证码的 SHA-256 hex 摘要。
fn digest_hex(plain: &str) -> String {
    let digest = Sha256::digest(plain.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 摘要是否命中已登记的认证码。
fn digest_matches(auth: &AuthConfig, digest: &str) -> bool {
    auth.keys.iter().any(|record| record.sha256 == digest)
}

/// 单次请求的准入判定（纯函数，便于直接单测）。
/// 返回 None = 放行；Some(code) = 拒绝原因码。
fn admit(cfg: &AuthConfig, ip: IpAddr, token: Option<&str>) -> Option<&'static str> {
    if !cfg.enabled {
        return None;
    }
    let ip_text = ip.to_string();
    if cfg.ip_allowlist.iter().any(|allowed| *allowed == ip_text) {
        return None;
    }
    match token.map(digest_hex).filter(|digest| digest_matches(cfg, digest)) {
        Some(_) => None,
        None => Some("unauthorized"),
    }
}

/// 全局认证中间件。挂载顺序在最外层：被拒请求不进入任何业务提取器。
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let auth = state.cfg().auth.clone();
    let ip = client_ip(&req);
    let token = bearer_token(req.headers());
    if let Some(code) = admit(&auth, ip, token.as_deref()) {
        tracing::warn!(
            "接入认证拒绝：ip={ip} path={} reason={code}",
            req.uri().path()
        );
        return unauthorized_response();
    }
    next.run(req).await
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": "接入认证未通过：来源不在 IP 白名单，且未携带有效认证码。"
                    .to_string()
                    + "可携带 Authorization: Bearer <认证码> 或 x-api-key 头；"
                    + "本机来源默认放行。",
                "type": "invalid_request_error",
                "code": "unauthorized",
            }
        })),
    )
        .into_response()
}

// ─────────────────────────── 认证码生成 ───────────────────────────

/// 生成一条新认证码。返回 `(明文, 存储记录)`；明文只在此处出现一次。
///
/// 随机源：两个 UUIDv4（合计 244 位随机量，足够对抗离线穷举），编码为
/// base64url（43 字符）拼上 `mgk_` 前缀。
pub fn generate_key(name: &str) -> (String, AuthKeyRecord) {
    let mut bytes = uuid::Uuid::new_v4().as_bytes().to_vec();
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let plain = format!("{}{}", KEY_PREFIX, URL_SAFE_NO_PAD.encode(&bytes[..32]));
    let record = AuthKeyRecord {
        id: format!("authkey-{}", uuid::Uuid::new_v4().simple()),
        name: name.trim().to_string(),
        prefix: plain[..12].to_string(),
        sha256: digest_hex(&plain),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    (plain, record)
}

/// 生成认证码并写入配置（不落盘的辅助入口，供管理 API 与测试复用）。
/// 返回 `(明文, 新的 auth 配置)`。
pub fn append_key(auth: &AuthConfig, name: &str) -> anyhow::Result<(String, AuthConfig)> {
    if auth.keys.len() >= AUTH_KEYS_MAX {
        anyhow::bail!("认证码已达上限（{AUTH_KEYS_MAX} 条），请先吊销不再使用的条目");
    }
    let (plain, record) = generate_key(name);
    let mut next = auth.clone();
    next.keys.push(record);
    Ok((plain, next))
}

// ─────────────────────────── 管理 API ───────────────────────────
// 挂在 panel 模块（/api/auth*）：与其它管理接口同一批挂载与保护。
// 写盘复用配置链路（备份 → 原子写 → 热生效），改动即时生效，无需重启。

/// 当前认证配置的展示形态：只含摘要，绝不含明文。
fn auth_view(auth: &AuthConfig) -> Value {
    json!({
        "enabled": auth.enabled,
        "ipAllowlist": auth.ip_allowlist,
        "keys": auth.keys.iter().map(|k| json!({
            "id": k.id,
            "name": k.name,
            "prefix": k.prefix,
            "createdAt": k.created_at,
        })).collect::<Vec<_>>(),
        "keysMax": AUTH_KEYS_MAX,
        "keyPrefix": KEY_PREFIX,
    })
}

fn diagnostics_blocked(candidate: &crate::config::Config) -> Option<Response> {
    let mut diagnostics = candidate.diagnostics();
    diagnostics.retain(|d| d.level == "error");
    let Some(first) = diagnostics.first() else {
        return None;
    };
    Some(error_json(
        StatusCode::BAD_REQUEST,
        format!("配置有 {} 处错误，未保存：{}", diagnostics.len(), first.message),
        "invalid_request_error",
        "invalid_config",
    ))
}

/// 修改 raw 里的 auth 块后走完整落盘链路（备份 → 原子写 → 热生效）。
/// `mutate` 只被授权改 `auth` 子对象，其余配置原样保留。
fn persist_auth<F>(state: &Arc<AppState>, mutate: F) -> Response
where
    F: FnOnce(&mut Value),
{
    let mut raw = state.cfg().raw.clone();
    mutate(&mut raw);
    let store = crate::secret_store::SecretStore::load(&state.config_path);
    let candidate = crate::config::Config::from_value_with_store(raw, &store);
    if let Some(blocked) = diagnostics_blocked(&candidate) {
        return blocked;
    }
    let backup = crate::config_store::backup_config(
        &state.config_path,
        crate::config_store::BackupPolicy::Enabled,
    );
    let backup = match backup {
        Ok(outcome) => outcome,
        Err(err) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("配置备份失败: {err}"),
                "server_error",
                "config_backup_failed",
            );
        }
    };
    let mut to_write = candidate.raw.clone();
    crate::config_store::strip_secrets(&mut to_write);
    if let Err(err) = crate::config_store::save_atomic(&state.config_path, &to_write) {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("配置写入失败: {err}"),
            "server_error",
            "config_write_failed",
        );
    }
    state.replace_config(candidate);
    let cfg = state.cfg();
    Json(json!({
        "ok": true,
        "note": "已保存并热生效",
        "backup": {
            "created": backup.created,
            "forcedFirst": backup.forced_first,
            "path": backup.path.as_ref().map(|p| p.display().to_string()),
        },
        "auth": auth_view(&cfg.auth),
    }))
    .into_response()
}

/// `GET /api/auth`：当前认证配置（认证码只回前缀，不回明文）。
pub(crate) async fn get_auth(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.cfg();
    Json(json!({
        "ok": true,
        "auth": auth_view(&cfg.auth),
        "clientHint": "认证码通过 Authorization: Bearer <认证码> 或 x-api-key 头携带",
    }))
    .into_response()
}

/// `PUT /api/auth`：更新开关与 IP 白名单。缺省字段保持原值。
pub(crate) async fn put_auth(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Response {
    let enabled = match payload.get("enabled") {
        None => None,
        Some(Value::Bool(v)) => Some(*v),
        Some(_) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "enabled 必须是布尔值",
                "invalid_request_error",
                "invalid_auth",
            );
        }
    };
    let allowlist = match payload.get("ipAllowlist") {
        None => None,
        Some(Value::Array(items)) => {
            let mut allow: Vec<String> = Vec::new();
            for item in items {
                let Some(text) = item.as_str() else {
                    return error_json(
                        StatusCode::BAD_REQUEST,
                        "ipAllowlist 条目必须是字符串",
                        "invalid_request_error",
                        "invalid_auth",
                    );
                };
                match text.trim().parse::<IpAddr>() {
                    Ok(ip) => {
                        let normalized = ip.to_string();
                        if !allow.contains(&normalized) {
                            allow.push(normalized);
                        }
                    }
                    Err(_) => {
                        return error_json(
                            StatusCode::BAD_REQUEST,
                            format!("白名单条目不是合法 IP：{text}（只接受精确 IP，不支持网段）"),
                            "invalid_request_error",
                            "invalid_auth",
                        );
                    }
                }
            }
            Some(allow)
        }
        Some(_) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "ipAllowlist 必须是 IP 字符串数组",
                "invalid_request_error",
                "invalid_auth",
            );
        }
    };
    // 防呆：白名单清空 + 无认证码 + 验证开启 = 所有来源（含本机面板）都被拒，
    // 唯一恢复途径是手改配置文件。先给认证码，或一并关闭验证。
    let current = state.cfg().auth.clone();
    let will_be_enabled = enabled.unwrap_or(current.enabled);
    if will_be_enabled
        && allowlist.as_ref().is_some_and(|list| list.is_empty())
        && current.keys.is_empty()
    {
        return error_json(
            StatusCode::BAD_REQUEST,
            "拒绝保存：白名单清空且没有任何认证码，会导致包括本机面板在内的所有来源被拒绝。请先生成认证码，或同时把 enabled 置为 false",
            "invalid_request_error",
            "auth_lockout_guard",
        );
    }
    persist_auth(&state, |raw| {
        let obj = raw.as_object_mut().expect("raw 必须是对象");
        let auth = obj
            .entry("auth")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("auth 必须保持对象形态");
        if let Some(v) = enabled {
            auth.insert("enabled".into(), json!(v));
        }
        if let Some(list) = allowlist {
            auth.insert("ipAllowlist".into(), json!(list));
        }
    })
}

/// `POST /api/auth/keys`：生成认证码。响应里的 `key` 是明文唯一一次露面。
pub(crate) async fn create_key(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Response {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .chars()
        .take(60)
        .collect::<String>();
    let current = state.cfg().auth.clone();
    let (plain, next) = match append_key(&current, &name) {
        Ok(pair) => pair,
        Err(err) => {
            return error_json(
                StatusCode::BAD_REQUEST,
                err.to_string(),
                "invalid_request_error",
                "auth_keys_limit",
            );
        }
    };
    let response = persist_auth(&state, |raw| {
        let obj = raw.as_object_mut().expect("raw 必须是对象");
        let auth = obj
            .entry("auth")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("auth 必须保持对象形态");
        let keys = auth
            .entry("keys")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("keys 必须保持数组形态");
        let record = next.keys.last().expect("append_key 必产生一条记录");
        keys.push(json!({
            "id": record.id,
            "name": record.name,
            "prefix": record.prefix,
            "sha256": record.sha256,
            "createdAt": record.created_at,
        }));
    });
    if !response.status().is_success() {
        return response;
    }
    Json(json!({
        "ok": true,
        "note": "认证码已生成：明文仅本次返回，请立即保存",
        "key": plain,
        "auth": next.keys.last().map(|k| json!({
            "id": k.id,
            "name": k.name,
            "prefix": k.prefix,
            "createdAt": k.created_at,
        })),
    }))
    .into_response()
}

/// `DELETE /api/auth/keys/{id}`：吊销认证码。被吊销的明文即刻失效。
pub(crate) async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let exists = state.cfg().auth.keys.iter().any(|k| k.id == id);
    if !exists {
        return error_json(
            StatusCode::NOT_FOUND,
            format!("认证码不存在：{id}"),
            "invalid_request_error",
            "auth_key_not_found",
        );
    }
    persist_auth(&state, |raw| {
        let obj = raw.as_object_mut().expect("raw 必须是对象");
        let auth = obj
            .entry("auth")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("auth 必须保持对象形态");
        if let Some(keys) = auth.get_mut("keys").and_then(Value::as_array_mut) {
            keys.retain(|k| k.get("id").and_then(Value::as_str) != Some(id.as_str()));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AUTH_DEFAULT_ALLOWLIST;

    fn auth_with_keys(keys: Vec<AuthKeyRecord>) -> AuthConfig {
        AuthConfig {
            enabled: true,
            ip_allowlist: AUTH_DEFAULT_ALLOWLIST
                .iter()
                .map(|s| s.to_string())
                .collect(),
            keys,
            issues: Vec::new(),
        }
    }

    fn record_of(plain: &str) -> AuthKeyRecord {
        AuthKeyRecord {
            id: format!("authkey-{}", uuid::Uuid::new_v4().simple()),
            name: "test".into(),
            prefix: plain[..12].to_string(),
            sha256: digest_hex(plain),
            created_at: "2026-11-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn generated_key_shape_is_stable() {
        let (plain, record) = generate_key("笔记本");
        assert!(plain.starts_with("mgk_"), "前缀固定为 mgk_");
        let body = plain.trim_start_matches(KEY_PREFIX);
        assert_eq!(body.len(), 43, "32 字节 base64url 无填充应为 43 字符");
        assert!(
            body.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "base64url 字母表"
        );
        assert_eq!(record.sha256, digest_hex(&plain));
        assert_eq!(record.prefix, plain[..12]);
        assert_eq!(record.name, "笔记本");
        // 每次生成的明文与 id 都不同。
        let (plain2, record2) = generate_key("");
        assert_ne!(plain, plain2);
        assert_ne!(record.id, record2.id);
    }

    #[test]
    fn admit_allows_loopback_by_default() {
        let auth = auth_with_keys(vec![]);
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let remote: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(admit(&auth, loopback, None).is_none(), "本机默认放行");
        assert_eq!(admit(&auth, remote, None), Some("unauthorized"));
    }

    #[test]
    fn admit_disabled_allows_everything() {
        let mut auth = auth_with_keys(vec![]);
        auth.enabled = false;
        let remote: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(admit(&auth, remote, None).is_none(), "关闭验证全放行");
    }

    #[test]
    fn admit_key_grants_remote_access() {
        let (plain, record) = generate_key("远程");
        let auth = auth_with_keys(vec![record]);
        let remote: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(admit(&auth, remote, Some(&plain)).is_none(), "有效认证码放行");
        assert_eq!(
            admit(&auth, remote, Some("mgk_wrong")),
            Some("unauthorized"),
            "错误认证码拒绝"
        );
        assert_eq!(
            admit(&auth, remote, None),
            Some("unauthorized"),
            "不带认证码拒绝"
        );
    }

    #[test]
    fn admit_empty_allowlist_requires_key_even_locally() {
        let (plain, record) = generate_key("严格");
        let mut auth = auth_with_keys(vec![record]);
        auth.ip_allowlist = vec![];
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            admit(&auth, loopback, None),
            Some("unauthorized"),
            "白名单清空后本机也需认证码"
        );
        assert!(admit(&auth, loopback, Some(&plain)).is_none());
    }

    #[test]
    fn bearer_token_reads_both_headers_and_rejects_non_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer mgk_abc".parse().expect("合法头值"),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("mgk_abc"));

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "mgk_xyz".parse().expect("合法头值"));
        assert_eq!(bearer_token(&headers).as_deref(), Some("mgk_xyz"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().expect("合法头值"),
        );
        assert_eq!(
            bearer_token(&headers),
            None,
            "非 Bearer 的 Authorization 不再回退 x-api-key"
        );
    }

    #[test]
    fn append_key_enforces_limit() {
        let keys: Vec<AuthKeyRecord> = (0..AUTH_KEYS_MAX)
            .map(|i| {
                let (_, r) = generate_key(&format!("k{i}"));
                r
            })
            .collect();
        let auth = auth_with_keys(keys);
        assert!(append_key(&auth, "one-more").is_err(), "达到上限后拒绝再生成");
    }

    #[test]
    fn revoked_digest_stops_granting() {
        let (plain, record) = generate_key("吊销");
        let mut auth = auth_with_keys(vec![record]);
        let remote: IpAddr = "10.0.0.9".parse().unwrap();
        assert!(admit(&auth, remote, Some(&plain)).is_none());
        auth.keys.clear();
        assert_eq!(admit(&auth, remote, Some(&plain)), Some("unauthorized"));
    }
}
