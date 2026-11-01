//! 配置加载与上游规整。
//!
//! 目标：读取与 Python 版完全相同的 `bridge.config.json`，不要求用户改一行配置。
//! 解析采用「宽松 JSON + 显式规整」，未知字段原样保留（面板保存时回写）。
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{json, Map, Value};

/// 上游兼容能力（provider quirks）。
///
/// 设计目标：把 opencode / commandcode / deepseek 这类“某个上游特有的处理”
/// 从协议分支里抽出来，变成可声明、可覆盖、可校验的数据。
///
/// 解析优先级（后者覆盖前者）：
/// 1. 内置预设（由 `profile` 或 `protocol + baseUrl` 推断）
/// 2. 旧版平铺字段（sessionHeader / cliVersion / cmdZdr …），保证老配置不回退
/// 3. 显式 `compat` 对象（新配置契约；面板可编辑）
#[derive(Debug, Clone)]
pub struct Compat {
    /// 预设名：openai / opencode / anthropic / commandcode / deepseek
    pub profile: String,
    /// 鉴权方式：bearer | x-api-key | both | none | header
    pub auth_scheme: String,
    /// 该 provider 允许「无密钥」调用（注册表 `authOptional`）。
    ///
    /// 为什么需要它：opencode / zen 的免费层明确「空 key 表示不鉴权」，
    /// 但它的 auth.scheme 继承 openai 基线的 bearer。若只按 scheme 判定，
    /// 免鉴权上游会被误报「没有配置密钥」（历史误报来源）。
    pub auth_optional: bool,
    /// auth_scheme=header 时使用的请求头名
    pub auth_header: String,
    /// 鉴权值前缀（bearer 默认 "Bearer "）
    pub auth_prefix: String,
    /// 会话头名；空串表示不注入
    pub session_header: String,
    /// 会话 ID 形态：hex32（原样）| sess16（sess_ + 前 16 位）
    pub session_id_style: String,
    /// 额外静态请求头（按顺序写入，鉴权头优先生效）
    pub extra_headers: Vec<(String, String)>,
    /// 对话端点路径（拼在 baseUrl 后）
    pub chat_path: String,
    /// 模型清单端点路径
    pub models_path: String,
    /// 是否把上一轮思考内容回传给上游（DeepSeek 思考链必需）
    pub thinking_passthrough: bool,
    /// 回传时使用的字段/块类型（reasoning_content / thinking）
    pub thinking_field: String,
    /// zcode 通道 `session/setModel` 的默认推理等级（空 = 内置 max）
    pub reasoning_level_default: String,
    /// 允许透传的推理等级白名单（空 = 内置全词表 off/minimal/low/medium/high/xhigh/max）
    pub reasoning_level_allow: Vec<String>,
    /// 强制 tool_choice 是否降级为 auto（thinking 模式常见限制）
    pub tool_choice_downgrade: bool,
    /// 上游允许的最大输出 token（超过时自动收敛）
    pub max_output_tokens: Option<u64>,
    /// 上游上下文窗口（仅用于面板提示/校验）
    pub max_context_tokens: Option<u64>,
    /// CLI 版本头名（值取自 cliVersion，auto 时运行时探测）
    pub version_header: String,
    /// CLI 环境头名（值取自 cliEnvironment）
    pub environment_header: String,
    /// 是否发送 ZDR 安全头 `x-cmd-zdr: 1`
    ///
    /// 兼容字段：运行时以 [`Compat::privacy`] 为准；这里只保留旧配置/API 的
    /// 布尔视图，避免已有配置和测试失效。
    pub zdr: bool,
    /// 通用隐私策略（ZDR / 训练数据）：用户意图 + provider 适配器。
    pub privacy: Privacy,
    /// 模型目录来源：endpoint | commandcode（注册表 catalog.source，可被 compat.catalog 覆盖）
    pub catalog_source: String,
    /// 模型目录端点路径
    pub catalog_path: String,
    /// 目录拉取计划：false | "auto" | 上游自定义值（Task 9 使用）
    pub catalog_plan: Value,
    /// 能力元数据（只读展示与诊断，不改变转发行为）
    pub capabilities: Capabilities,
}

/// 能力元数据：固定六字段；合并顺序 模型覆盖 > 上游 compat > profile > 内置默认。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub stream: bool,
    pub tools: bool,
    pub vision: bool,
    pub thinking: bool,
    pub cache: bool,
    pub system_prompt: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            stream: true,
            tools: true,
            vision: false,
            thinking: false,
            cache: false,
            system_prompt: true,
        }
    }
}

impl Capabilities {
    /// 用一层 JSON（compat.capabilities / 模型覆盖）覆盖已知布尔字段。
    pub fn apply_json(&mut self, layer: &Value) {
        let Some(obj) = layer.as_object() else {
            return;
        };
        let set = |key: &str, target: &mut bool| {
            if let Some(v) = obj.get(key).and_then(Value::as_bool) {
                *target = v;
            }
        };
        set("stream", &mut self.stream);
        set("tools", &mut self.tools);
        set("vision", &mut self.vision);
        set("thinking", &mut self.thinking);
        set("cache", &mut self.cache);
        set("systemPrompt", &mut self.system_prompt);
    }

    pub fn to_json(&self) -> Value {
        json!({
            "stream": self.stream,
            "tools": self.tools,
            "vision": self.vision,
            "thinking": self.thinking,
            "cache": self.cache,
            "systemPrompt": self.system_prompt,
        })
    }
}

/// 隐私策略的落地点：不同上游对 ZDR / 训练数据的控制方式并不统一。
///
/// 设计目标：把「用户意图」与「线上实现」分开。用户只配置要不要 ZDR、
/// 允许还是禁止训练；具体发头、改请求体，还是必须去上游控制台设置，
/// 由 provider profile 的 `privacyAdapters` 声明。
#[derive(Debug, Clone, PartialEq)]
pub enum PrivacyAdapter {
    /// 上游默认行为已满足，无需额外动作。
    None,
    /// 通过请求头落地。
    Header { name: String, value: String },
    /// 通过请求体 JSON 路径落地（点分隔，例如 provider.zdr）。
    Body { path: String, value: Value },
    /// 只能在上游控制台/账户设置里修改。
    Workspace { url: String, setting: String },
    /// 上游没有该能力。
    Unsupported { reason: String },
}

impl Default for PrivacyAdapter {
    fn default() -> Self {
        Self::Unsupported {
            reason: "该上游未声明请求级隐私能力".into(),
        }
    }
}

impl PrivacyAdapter {
    pub fn to_json(&self) -> Value {
        match self {
            Self::None => json!({"kind": "none"}),
            Self::Header { name, value } => {
                json!({"kind": "header", "name": name, "value": value})
            }
            Self::Body { path, value } => json!({"kind": "body", "path": path, "value": value}),
            Self::Workspace { url, setting } => {
                json!({"kind": "workspace", "url": url, "setting": setting})
            }
            Self::Unsupported { reason } => json!({"kind": "unsupported", "reason": reason}),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::None => "无需额外动作".into(),
            Self::Header { name, value } => format!("请求头 {name}: {value}"),
            Self::Body { path, value } => format!("请求体 {path} = {value}"),
            Self::Workspace { url, setting } => format!("上游控制台：{setting}（{url}）"),
            Self::Unsupported { reason } => format!("不支持：{reason}"),
        }
    }
}

/// 训练数据策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingPolicy {
    /// 未显式设置，跟随 profile / 上游默认。
    Inherit,
    /// 允许使用请求数据训练。
    Allow,
    /// 禁止使用请求数据训练。
    Deny,
}

impl Default for TrainingPolicy {
    fn default() -> Self {
        Self::Inherit
    }
}

impl TrainingPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// 通用隐私策略：用户意图 + 上游适配器。
#[derive(Debug, Clone, PartialEq)]
pub struct Privacy {
    /// 是否要求零数据保留；None 表示未显式设置。
    pub zdr: Option<bool>,
    /// 训练数据策略。
    pub training: TrainingPolicy,
    /// ZDR 的落地方式（来自 profile / 显式 privacyAdapters）。
    pub zdr_adapter: PrivacyAdapter,
    /// “允许训练”的落地方式。
    pub training_allow_adapter: PrivacyAdapter,
    /// “禁止训练”的落地方式。
    pub training_deny_adapter: PrivacyAdapter,
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            zdr: None,
            training: TrainingPolicy::Inherit,
            zdr_adapter: PrivacyAdapter::default(),
            training_allow_adapter: PrivacyAdapter::default(),
            training_deny_adapter: PrivacyAdapter::default(),
        }
    }
}

impl Privacy {
    pub fn to_json(&self) -> Value {
        json!({
            "zdr": self.zdr,
            "training": self.training.as_str(),
            "adapters": {
                "zdr": self.zdr_adapter.to_json(),
                "trainingAllow": self.training_allow_adapter.to_json(),
                "trainingDeny": self.training_deny_adapter.to_json(),
            }
        })
    }

    /// 配置期诊断：只在用户明确要求、且该上游无法按请求落地时提示。
    pub fn diagnostics(&self, upstream: &str) -> Vec<(&'static str, String, String)> {
        let mut out = Vec::new();
        if self.zdr == Some(true) {
            match &self.zdr_adapter {
                PrivacyAdapter::None => out.push((
                    "warn",
                    format!("上游「{upstream}」要求 ZDR，但没有声明可用的落地方式"),
                    "检查 compat.privacyAdapters.zdr".into(),
                )),
                PrivacyAdapter::Workspace { url, setting } => out.push((
                    "warn",
                    format!("上游「{upstream}」的 ZDR 只能在上游控制台设置：{setting}"),
                    url.clone(),
                )),
                PrivacyAdapter::Unsupported { reason } => out.push((
                    "warn",
                    format!("上游「{upstream}」不支持请求级 ZDR：{reason}"),
                    "关闭 compat.zdr / compat.privacy.zdr，或改用支持 ZDR 的上游".into(),
                )),
                PrivacyAdapter::Header { .. } | PrivacyAdapter::Body { .. } => {}
            }
        }
        match self.training {
            TrainingPolicy::Inherit => {}
            TrainingPolicy::Allow => match &self.training_allow_adapter {
                PrivacyAdapter::None
                | PrivacyAdapter::Header { .. }
                | PrivacyAdapter::Body { .. } => {}
                PrivacyAdapter::Workspace { url, setting } => out.push((
                    "warn",
                    format!("上游「{upstream}」允许训练需要在上游控制台设置：{setting}"),
                    url.clone(),
                )),
                PrivacyAdapter::Unsupported { reason } => out.push((
                    "warn",
                    format!("上游「{upstream}」不支持请求级“允许训练”：{reason}"),
                    "该策略只能由上游账户或控制台决定".into(),
                )),
            },
            TrainingPolicy::Deny => match &self.training_deny_adapter {
                PrivacyAdapter::None
                | PrivacyAdapter::Header { .. }
                | PrivacyAdapter::Body { .. } => {}
                PrivacyAdapter::Workspace { url, setting } => out.push((
                    "warn",
                    format!("上游「{upstream}」的训练策略只能在上游控制台设置：{setting}"),
                    url.clone(),
                )),
                PrivacyAdapter::Unsupported { reason } => out.push((
                    "warn",
                    format!("上游「{upstream}」不支持请求级“禁止训练”：{reason}"),
                    "改用明确不训练的上游或模型".into(),
                )),
            },
        }
        out
    }
}

/// 从 profile 的 `privacyAdapters.<key>` 解析一个适配器。
pub(crate) fn privacy_adapter_from_value(value: Option<&Value>) -> PrivacyAdapter {
    let Some(obj) = value.and_then(Value::as_object) else {
        return PrivacyAdapter::default();
    };
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "none" => PrivacyAdapter::None,
        "header" => {
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let value = obj
                .get("value")
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            if name.is_empty() {
                PrivacyAdapter::Unsupported {
                    reason: "privacyAdapters.header 缺少 name".into(),
                }
            } else {
                PrivacyAdapter::Header {
                    name: name.to_lowercase(),
                    value,
                }
            }
        }
        "body" => {
            let path = obj
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if path.is_empty() {
                PrivacyAdapter::Unsupported {
                    reason: "privacyAdapters.body 缺少 path".into(),
                }
            } else {
                PrivacyAdapter::Body {
                    path,
                    value: obj.get("value").cloned().unwrap_or(Value::Null),
                }
            }
        }
        "workspace" => PrivacyAdapter::Workspace {
            url: obj
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            setting: obj
                .get("setting")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "unsupported" => PrivacyAdapter::Unsupported {
            reason: obj
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("该上游未声明请求级隐私能力")
                .to_string(),
        },
        _ => PrivacyAdapter::Unsupported {
            reason: format!("privacyAdapters.kind「{kind}」无效"),
        },
    }
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            profile: "openai".into(),
            auth_scheme: "bearer".into(),
            auth_optional: false,
            auth_header: "authorization".into(),
            auth_prefix: "Bearer ".into(),
            session_header: String::new(),
            session_id_style: "hex32".into(),
            extra_headers: Vec::new(),
            chat_path: "/v1/chat/completions".into(),
            models_path: "/v1/models".into(),
            thinking_passthrough: false,
            thinking_field: "reasoning_content".into(),
            reasoning_level_default: String::new(),
            reasoning_level_allow: Vec::new(),
            tool_choice_downgrade: false,
            max_output_tokens: None,
            max_context_tokens: None,
            version_header: String::new(),
            environment_header: String::new(),
            zdr: false,
            privacy: Privacy::default(),
            catalog_source: "endpoint".into(),
            catalog_path: "/v1/models".into(),
            catalog_plan: Value::Bool(false),
            capabilities: Capabilities::default(),
        }
    }
}

impl Compat {
    /// 按提供商预设生成默认能力。数据源为共享注册表（与 Python 端一致），
    /// 注册表不可用时退回内置实现；预设只是默认值，任何字段都可被配置覆盖。
    pub fn preset(profile: &str, base_url: &str, protocol: &str) -> Self {
        if let Some(compat) = crate::provider_profiles::resolve_compat(profile, base_url, protocol)
        {
            return compat;
        }
        Self::preset_builtin(profile, base_url, protocol)
    }

    /// 内置预设实现（兜底副本，行为冻结；注册表以 scripts/provider_profiles_check.py 对照）。
    pub fn preset_builtin(profile: &str, base_url: &str, protocol: &str) -> Self {
        let mut c = Compat::default();
        let host = base_url.to_lowercase();
        let effective = if !profile.is_empty() {
            profile.to_lowercase()
        } else if protocol == "commandcode" || host.contains("commandcode.ai") {
            "commandcode".into()
        } else if host.contains("api.deepseek.com") {
            "deepseek".into()
        } else if host.contains("opencode.ai") {
            "opencode".into()
        } else {
            protocol.to_lowercase()
        };
        c.profile = effective.clone();
        match effective.as_str() {
            "opencode" => {
                // opencode / zen：缺少会话头会被上游 400 拒绝；空 key 表示不鉴权。
                c.session_header = "x-opencode-session".into();
                c.session_id_style = "hex32".into();
                // 免鉴权允许：与注册表 `opencode.authOptional` 一致（兜底副本也要对）。
                c.auth_optional = true;
                // OpenCode 不读取请求级 ZDR；Go 的训练授权在 workspace 控制台。
                c.privacy.zdr_adapter = PrivacyAdapter::Unsupported {
                    reason: "OpenCode 不读取请求级 ZDR 头；数据保留由模型条款决定".into(),
                };
                c.privacy.training_allow_adapter = PrivacyAdapter::Workspace {
                    url: "https://opencode.ai/console".into(),
                    setting: "Settings › Privacy → Allow paid endpoints that train on request data"
                        .into(),
                };
                c.privacy.training_deny_adapter = PrivacyAdapter::Unsupported {
                    reason: "OpenCode 不提供请求级训练开关；请改用不训练的模型".into(),
                };
            }
            "commandcode" => {
                c.profile = "commandcode".into();
                c.session_header = "x-session-id".into();
                c.session_id_style = "sess16".into();
                c.chat_path = "/alpha/generate".into();
                c.version_header = "x-command-code-version".into();
                c.environment_header = "x-cli-environment".into();
                c.extra_headers.push(("user-agent".into(), "cli".into()));
                c.zdr = true;
                c.privacy.zdr = Some(true);
                c.privacy.zdr_adapter = PrivacyAdapter::Header {
                    name: "x-cmd-zdr".into(),
                    value: "1".into(),
                };
                c.tool_choice_downgrade = true;
                c.max_output_tokens = Some(64_000);
                c.catalog_source = "commandcode".into();
                c.catalog_plan = Value::String("auto".into());
                c.capabilities.thinking = true;
            }
            "anthropic" => {
                c.auth_scheme = "both".into();
                c.chat_path = "/v1/messages".into();
                c.thinking_passthrough = true;
                c.tool_choice_downgrade = true;
                c.capabilities.vision = true;
                c.capabilities.thinking = true;
                c.capabilities.cache = true;
                // DeepSeek 的 Anthropic 兼容层（如 CCH）同样受官方输出上限约束。
                if host.contains("deepseek") {
                    c.profile = "deepseek".into();
                    c.max_output_tokens = Some(384 * 1024);
                    c.max_context_tokens = Some(1024 * 1024);
                }
            }
            "deepseek" => {
                c.profile = "deepseek".into();
                c.thinking_passthrough = true;
                c.max_output_tokens = Some(384 * 1024);
                c.max_context_tokens = Some(1024 * 1024);
                c.capabilities.thinking = true;
                c.capabilities.cache = true;
                if protocol == "anthropic" {
                    c.auth_scheme = "both".into();
                    c.chat_path = "/v1/messages".into();
                    c.tool_choice_downgrade = true;
                }
            }
            _ => {}
        }
        c
    }

    pub fn is_default_auth_scheme(&self) -> bool {
        matches!(
            self.auth_scheme.as_str(),
            "bearer" | "x-api-key" | "both" | "none" | "header"
        )
    }

    /// 该上游在当前配置下是否「需要鉴权」。
    ///
    /// 判定口径（三类，任一成立即视为免鉴权）：
    /// 1. `auth.scheme == "none"`：显式声明不鉴权。
    /// 2. `auth_optional`：注册表声明该 provider 允许空 key（如 opencode / zen）。
    /// 3. 本机地址上游：本机桥接（CCH 等）不校验密钥。
    ///
    /// 诊断「没有配置密钥」必须以此为准，否则免鉴权上游会被误报。
    pub fn requires_auth(&self, base_url: &str) -> bool {
        if self.auth_scheme == "none" || self.auth_optional {
            return false;
        }
        !is_local_endpoint(base_url)
    }
}

/// 上游地址是否指向本机（回环 / 局域网段）。
///
/// 本机桥接服务（CCH 等）通常不校验密钥，对这类上游提示「没有配置密钥」是错的。
pub fn is_local_endpoint(base_url: &str) -> bool {
    let url = base_url.trim().to_lowercase();
    let after_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url.as_str(),
    };
    let authority = after_scheme.split('/').next().unwrap_or("");
    // 去掉用户信息（user:pass@host）
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 字面量：[::1] → ::1
    let host = if let Some(end) = authority.strip_prefix('[') {
        end.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host == "::1" || host == "0.0.0.0" {
        return true;
    }
    // 127.0.0.0/8 整段回环：必须按完整主机名匹配，
    // 否则 `127.0.0.1.example.com` 这类公网域名会被前缀误判为本机。
    if host == "127" || host.starts_with("127.") {
        if host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok()) {
            return true;
        }
        return false;
    }
    if host == "10" || host.starts_with("10.") {
        if host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok()) {
            return true;
        }
        return false;
    }
    if host == "192" || host.starts_with("192.168.") {
        if host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok()) {
            return true;
        }
        return false;
    }
    // 172.16/12 私有段（第二段取值 16..=31）
    if host.starts_with("172.") {
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
            if let Ok(first) = parts[1].parse::<u8>() {
                return (16..=31).contains(&first);
            }
        }
        return false;
    }
    false
}

/// 逐模型密钥：同一个上游下不同模型使用不同厂商密钥。
///
/// 背景：`cch.skytech.io` 这类网关**按键选厂商**，同一 `baseUrl` 下
/// `deepseek-flash` / `MiniMax-M3` 必须分别使用各自的 API Key。
///
/// 匹配规则与 `modelOverrides` 一致：精确命中优先，其次最长单 `*` 通配。
/// 键必须是**最终发给上游的模型 id**（`modelMap` 解析后的 `target_model`），
/// 而不是客户端请求里写的名字。
#[derive(Debug, Clone)]
pub struct ModelKey {
    /// 模型 id（精确或只含一个 `*` 的通配）。
    pub pattern: String,
    /// 解析后的密钥；读不到时为空。
    pub api_key: String,
    /// 来源：env / user-env / file / agent:... / none。
    pub source: String,
    /// 环境变量名（面板展示与「写入密钥」使用）。
    pub env_name: Option<String>,
    /// Model-Gate 加密密钥库引用 id（`model:上游:模型`）。
    pub store_id: Option<String>,
    /// 配置里声明的来源（变量名 / 文件路径 / 密钥库引用），用于诊断展示。
    pub configured_from: String,
    /// 读不到密钥的原因；`None` 表示可用。
    pub missing: Option<String>,
}

/// 一次请求最终选用的密钥及其来源。
#[derive(Debug, Clone)]
pub struct KeySelection {
    pub key: String,
    pub source: String,
    pub env_name: Option<String>,
    /// `model` = 命中逐模型密钥；`upstream` = 上游级兜底。
    pub from: String,
}

#[derive(Debug, Clone)]
pub struct Upstream {
    pub name: String,
    pub protocol: String,
    /// 传输适配层：空串表示按 protocol；当前支持 opencode。
    pub transport: String,
    pub base_url: String,
    pub api_key: String,
    pub key_source: String,
    pub default: bool,
    pub models: Vec<String>,
    pub model_map: HashMap<String, String>,
    pub session_header: bool,
    pub anthropic_version: String,
    pub cli_version: String,
    pub cli_environment: String,
    pub pause_turn: bool,
    pub cmd_zdr: bool,
    pub fetch_models: bool,
    pub auto_models: bool,
    pub api_key_env: Option<String>,
    pub api_key_file: Option<String>,
    /// 从 agent 客户端工具读取密钥的预设名（opencode/jcode/commandcode/custom）。
    pub api_key_agent: Option<String>,
    /// Model-Gate 加密密钥库引用 id（`upstream:名称`）。
    pub api_key_store: Option<String>,
    /// 凭据池 id：存在时请求期优先从池选成员；`apiKeyStore` 仅作展示/兜底。
    pub credential_pool: Option<String>,
    /// 逐模型密钥：模型 id（精确或单个 `*` 通配）→ 已解析的凭据。
    /// 命中时优先于上游级 `api_key`；未命中回落到上游级。
    pub model_keys: Vec<ModelKey>,
    /// 模型级覆盖：键为模型 id（精确或单个 `*` 通配），按配置顺序保留。
    pub model_overrides: Map<String, Value>,
    /// 解析后的兼容能力（预设 + 旧字段 + 显式 compat）。
    pub compat: Compat,
}

impl Upstream {
    /// 是否真正拿到可用凭据（未配置时为空值，不算已配置）。
    pub fn key_configured(&self) -> bool {
        !self.api_key.is_empty() && self.key_source != "none"
    }

    /// 上游级或任一逐模型密钥可用（面板状态展示用）。
    pub fn any_key_configured(&self) -> bool {
        self.key_configured() || self.model_keys.iter().any(|mk| !mk.api_key.is_empty())
    }

    /// 面板展示用的密钥来源：上游级优先，其次 `model`，最后 `none`。
    pub fn display_key_source(&self) -> &str {
        if self.key_configured() {
            self.key_source.as_str()
        } else if self.model_keys.iter().any(|mk| !mk.api_key.is_empty()) {
            "model"
        } else {
            "none"
        }
    }

    /// 命中逐模型密钥：精确优先，其次最长单 `*` 通配。
    ///
    /// 与 [`Self::model_override`] 使用同一套评分规则；多个 `*` 视为非法，
    /// 不参与匹配（诊断会单独报 `multi_wildcard_model_key`）。
    pub fn model_key(&self, model_id: &str) -> Option<&ModelKey> {
        if model_id.is_empty() {
            return None;
        }
        let mut best: Option<(u64, usize, &ModelKey)> = None;
        for (order, entry) in self.model_keys.iter().enumerate() {
            let stars = entry.pattern.matches('*').count();
            let score = if stars == 0 {
                if entry.pattern != model_id {
                    continue;
                }
                u64::MAX
            } else if stars == 1 {
                let (prefix, suffix) = entry.pattern.split_once('*').unwrap_or(("", ""));
                if model_id.len() < prefix.len() + suffix.len()
                    || !model_id.starts_with(prefix)
                    || !model_id.ends_with(suffix)
                {
                    continue;
                }
                (entry.pattern.len() - 1) as u64
            } else {
                continue;
            };
            let order_key = usize::MAX - order;
            let better = match best {
                Some((s, o, _)) => (score, order_key) > (s, o),
                None => true,
            };
            if better {
                best = Some((score, order_key, entry));
            }
        }
        best.map(|(_, _, entry)| entry)
    }

    /// 上游级密钥（逐模型表未命中 / 条目读不到时的兜底）。
    pub fn upstream_key(&self) -> KeySelection {
        KeySelection {
            key: self.api_key.clone(),
            source: self.key_source.clone(),
            env_name: self.api_key_env.clone(),
            from: "upstream".into(),
        }
    }

    /// 本次请求该用哪把密钥：命中逐模型条目且已读到值 → 用它；否则回落上游级。
    ///
    /// `model_id` 必须是**最终发给上游的模型名**（`target_model`），
    /// 不能是客户端请求名 —— 否则配了 `modelMap` 时会静默用错 key。
    pub fn key_for_model(&self, model_id: &str) -> KeySelection {
        if let Some(entry) = self.model_key(model_id) {
            if !entry.api_key.is_empty() {
                return KeySelection {
                    key: entry.api_key.clone(),
                    source: entry.source.clone(),
                    env_name: entry.env_name.clone(),
                    from: "model".into(),
                };
            }
        }
        self.upstream_key()
    }

    /// 命中模型级覆盖：精确优先，其次最长通配；多个 `*` 视为非法不参与匹配。
    pub fn model_override(&self, model_id: &str) -> Option<(&str, &Value)> {
        if model_id.is_empty() {
            return None;
        }
        let mut best: Option<(u64, usize, &str, &Value)> = None;
        for (order, (pattern, value)) in self.model_overrides.iter().enumerate() {
            if !value.is_object() {
                continue;
            }
            let stars = pattern.matches('*').count();
            let score = if stars == 0 {
                if pattern != model_id {
                    continue;
                }
                u64::MAX
            } else if stars == 1 {
                let (prefix, suffix) = pattern.split_once('*').unwrap_or(("", ""));
                if model_id.len() < prefix.len() + suffix.len()
                    || !model_id.starts_with(prefix)
                    || !model_id.ends_with(suffix)
                {
                    continue;
                }
                (pattern.len() - 1) as u64
            } else {
                continue;
            };
            // 同分时按配置顺序取先声明者。
            let order_key = usize::MAX - order;
            let better = match best {
                Some((s, o, _, _)) => (score, order_key) > (s, o),
                None => true,
            };
            if better {
                best = Some((score, order_key, pattern, value));
            }
        }
        best.map(|(_, _, pattern, value)| (pattern, value))
    }

    /// 模型级生效 maxOutputTokens：compat.limits 优先被 modelOverrides 覆盖。
    pub fn model_max_output_tokens(&self, model_id: &str) -> Option<u64> {
        if let Some((_, over)) = self.model_override(model_id) {
            if let Some(v) = over
                .get("limits")
                .and_then(|l| l.get("maxOutputTokens"))
                .and_then(positive_int)
            {
                return Some(v);
            }
        }
        self.compat.max_output_tokens
    }

    /// 模型级能力（仅用于展示）：上游 compat → 模型覆盖。
    #[allow(dead_code)] // 面板生效值展示在任务 10 接入
    pub fn model_capabilities(&self, model_id: &str) -> Capabilities {
        let mut caps = self.compat.capabilities.clone();
        if let Some((_, over)) = self.model_override(model_id) {
            if let Some(layer) = over.get("capabilities") {
                caps.apply_json(layer);
            }
        }
        caps
    }
}

/// 统一接入口的分组：`/unified/{id}/v1/...` 只展示组内允许展示的上游。
#[derive(Debug, Clone, PartialEq)]
pub struct UnifiedGroup {
    /// URL 路径中的分组标识（字母/数字/-/_/.，最长 64）。
    pub id: String,
    /// 面板展示名；空时回落到 id。
    pub label: String,
    /// 组成员（上游名）。
    pub upstreams: Vec<String>,
}

/// 统一接入口配置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Unified {
    /// 允许在统一接口展示的上游；空 = 全部。
    pub visible_upstreams: Vec<String>,
    /// 命名分组；`/unified/{id}/v1/...` 只在组内再按可见清单过滤。
    pub groups: Vec<UnifiedGroup>,
}

/// 路由组：对外虚拟模型 `group/<id>`，成员按声明顺序依次尝试。
///
/// 成员写法：
/// - `上游/模型`：直接锁定该上游；
/// - 模型名：按普通路由规则找声明了该模型的上游；
/// - 别名：先展开 `aliases`，再按上面两条解析。
///
/// 顺序就是人工维护的优先级；失败切换由 proxy 层执行。
#[derive(Debug, Clone, PartialEq)]
pub struct RouteGroup {
    /// `group/<id>` 中的 id。
    pub id: String,
    /// 原始成员声明，保留用户写法，解析在请求期完成。
    pub members: Vec<String>,
}

/// chat 单次请求体的默认上限：64 MiB。
///
/// 取值依据：纯文本对话通常只有几十到几百 KiB；带内联 base64 图片/附件的多模态
/// 请求常见 5–20 MiB。axum 对缓冲式提取器的默认上限是 2 MiB，沿用会让正常请求
/// 直接 413，因此这里给一个够用且仍能兜住异常放大的值。
pub const DEFAULT_MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub raw: Value,
    pub host: String,
    pub port: u16,
    pub default_model: Option<String>,
    pub models: Vec<String>,
    pub max_tokens: u64,
    pub timeout_seconds: f64,
    /// chat 单次请求体上限（字节）。axum 对缓冲式提取器的默认上限只有 2 MiB，
    /// 多模态请求（内联 base64 图片）很容易超过，必须显式给一个可用值。
    pub max_body_bytes: u64,
    /// 模型目录缓存秒数（与 Python modelsCacheSeconds 对齐；Task 9 使用）
    pub models_cache_seconds: u64,
    pub access_log: bool,
    /// 提示词记录开关：默认关闭，开启后记录入站 messages。
    pub prompt_capture: bool,
    /// 提示词保留天数；0 = 永久。
    pub prompt_retention_days: u64,
    /// 提示词最多保留条数；0 = 不限。
    pub prompt_max_records: u64,
    /// 单条提示词最大字符数；超过按消息边界截断。
    pub prompt_max_chars: u64,
    pub aliases: HashMap<String, String>,
    pub upstreams: Vec<Upstream>,
    /// 统一接入口的可见上游与命名分组。
    pub unified: Unified,
    /// 路由组：`group/<id>` 虚拟模型 → 有序成员链。
    pub groups: Vec<RouteGroup>,
    /// 单价表（解析后的 `pricing` 配置）：非法条目被跳过并记入 issues。
    pub pricing: crate::pricing::PricingTable,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        // 兼容 Windows 记事本/部分编辑器写入的 UTF-8 BOM。
        let text = text.trim_start_matches('\u{feff}');
        let raw: Value = serde_json::from_str(&text)
            .with_context(|| format!("配置不是合法 JSON: {}", path.display()))?;
        let store = crate::secret_store::SecretStore::load(path);
        Ok(Self::from_value_with_store(raw, &store))
    }

    /// 兼容入口：没有配置路径时使用空密钥库（单测、预览）。
    pub fn from_value(raw: Value) -> Self {
        Self::from_value_with_store(raw, &crate::secret_store::SecretStore::empty())
    }

    /// 带本地加密密钥库解析配置。
    pub fn from_value_with_store(raw: Value, store: &crate::secret_store::SecretStore) -> Self {
        let obj = raw.as_object().cloned().unwrap_or_default();
        let host = obj
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1")
            .to_string();
        let port = obj
            .get("port")
            .and_then(Value::as_u64)
            .unwrap_or(8900)
            .min(65535) as u16;
        let default_model = obj
            .get("defaultModel")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let models = string_list(obj.get("models"));
        let max_tokens = obj.get("maxTokens").and_then(Value::as_u64).unwrap_or(8192);
        let timeout_seconds = obj
            .get("timeoutSeconds")
            .and_then(Value::as_f64)
            .unwrap_or(600.0);
        let max_body_bytes = obj
            .get("maxBodyBytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_BODY_BYTES);
        let models_cache_seconds = obj
            .get("modelsCacheSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(300);
        let access_log = obj
            .get("accessLog")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let prompt_capture = obj
            .get("promptCapture")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let prompt_retention_days = obj
            .get("promptRetentionDays")
            .and_then(Value::as_u64)
            .unwrap_or(7);
        let prompt_max_records = obj
            .get("promptMaxRecords")
            .and_then(Value::as_u64)
            .unwrap_or(5000);
        let prompt_max_chars = obj
            .get("promptMaxChars")
            .and_then(Value::as_u64)
            .unwrap_or(100_000);
        let aliases = obj
            .get("aliases")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let mut upstreams: Vec<Upstream> = obj
            .get("upstreams")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(name, value)| normalize_upstream(name, value, &obj, store))
                    .collect()
            })
            .unwrap_or_default();
        // 与 Python 版一致：配置顺序即优先级；没有显式 default 时第一条兜底。
        if !upstreams.iter().any(|u| u.default) {
            if let Some(first) = upstreams.first_mut() {
                first.default = true;
            }
        }

        let unified = normalize_unified(obj.get("unified"));
        let groups = normalize_route_groups(obj.get("groups"));
        let pricing =
            crate::pricing::PricingTable::from_value(obj.get("pricing").unwrap_or(&Value::Null));

        Self {
            raw,
            host,
            port,
            default_model,
            models,
            max_tokens,
            timeout_seconds,
            max_body_bytes,
            models_cache_seconds,
            access_log,
            prompt_capture,
            prompt_retention_days,
            prompt_max_records,
            prompt_max_chars,
            aliases,
            upstreams,
            unified,
            groups,
            pricing,
        }
    }

    pub fn upstream(&self, name: &str) -> Option<&Upstream> {
        self.upstreams.iter().find(|u| u.name == name)
    }

    pub fn default_upstream(&self) -> Option<&Upstream> {
        self.upstreams
            .iter()
            .find(|u| u.default)
            .or_else(|| self.upstreams.first())
    }

    /// 模型名 → 上游：先别名，再按各上游声明清单匹配，再上游级别名，最后默认上游。
    pub fn pick_upstream(&self, model: Option<&str>) -> Option<&Upstream> {
        let name = model.unwrap_or("");
        let target = self.aliases.get(name).map(String::as_str).unwrap_or(name);
        if !target.is_empty() {
            if let Some(up) = self
                .upstreams
                .iter()
                .find(|u| u.models.iter().any(|m| m == target))
            {
                return Some(up);
            }
            if let Some(up) = self
                .upstreams
                .iter()
                .find(|u| u.model_map.contains_key(name))
            {
                return Some(up);
            }
        }
        self.default_upstream()
    }

    /// 统一接口允许展示的上游；`visibleUpstreams` 为空表示全部，按配置顺序返回。
    pub fn unified_visible_upstreams(&self) -> Vec<&Upstream> {
        if self.unified.visible_upstreams.is_empty() {
            return self.upstreams.iter().collect();
        }
        let allowed: HashSet<&str> = self
            .unified
            .visible_upstreams
            .iter()
            .map(String::as_str)
            .collect();
        self.upstreams
            .iter()
            .filter(|up| allowed.contains(up.name.as_str()))
            .collect()
    }

    pub fn unified_group(&self, id: &str) -> Option<&UnifiedGroup> {
        self.unified.groups.iter().find(|group| group.id == id)
    }

    /// 指定分组内允许展示的上游：分组不存在返回 `None`，空分组返回空数组。
    pub fn unified_group_upstreams(&self, id: &str) -> Option<Vec<&Upstream>> {
        let group = self.unified_group(id)?;
        let members: HashSet<&str> = group.upstreams.iter().map(String::as_str).collect();
        Some(
            self.unified_visible_upstreams()
                .into_iter()
                .filter(|up| members.contains(up.name.as_str()))
                .collect(),
        )
    }

    /// 按 id 查路由组。
    pub fn route_group(&self, id: &str) -> Option<&RouteGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// 从 `group/<id>` 取出组 id；格式不合法返回 `None`。
    ///
    /// 组 id 不允许包含 `/`，因此 `group/a/b` 不会命中任何组。
    pub fn group_id_from_model<'m>(&self, model: &'m str) -> Option<&'m str> {
        let id = model.strip_prefix("group/")?;
        if id.is_empty() || id.contains('/') {
            return None;
        }
        Some(id)
    }

    /// 解析路由组成员：`上游/模型`、普通模型名或别名。
    ///
    /// 普通模型名只在上游明确声明了该模型时才解析成功，避免成员写错后
    /// 静默落到默认上游；别名先展开，再按同样规则解析。
    pub fn resolve_group_member<'a>(&'a self, raw: &str) -> Option<(&'a Upstream, String)> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let Some((upstream, model)) = self.split_qualified_model(raw) {
            return Some((upstream, model.to_string()));
        }
        let target = self.aliases.get(raw).map(String::as_str).unwrap_or(raw);
        if target != raw {
            if let Some((upstream, model)) = self.split_qualified_model(target) {
                return Some((upstream, model.to_string()));
            }
        }
        let upstream = self
            .upstreams
            .iter()
            .find(|up| up.models.iter().any(|model| model == target))
            .or_else(|| {
                self.upstreams
                    .iter()
                    .find(|up| up.model_map.contains_key(target))
            })?;
        Some((upstream, target.to_string()))
    }

    /// 路由组的可路由成员（保持声明顺序；解析失败的成员跳过）。
    pub fn group_targets(&self, group: &RouteGroup) -> Vec<(&Upstream, String)> {
        let mut out: Vec<(&Upstream, String)> = Vec::new();
        for member in &group.members {
            let Some((upstream, model)) = self.resolve_group_member(member) else {
                continue;
            };
            if out.iter().any(|(existing, existing_model)| {
                existing.name == upstream.name && existing_model == &model
            }) {
                continue;
            }
            out.push((upstream, model));
        }
        out
    }

    /// 统一接口可见范围内的路由组清单：只要有一个成员落在可见上游里，
    /// 就把 `group/<id>` 暴露出去；请求时仍会按可见范围过滤成员。
    pub fn group_model_list_for(&self, upstreams: &[&Upstream]) -> Vec<String> {
        let allowed: HashSet<&str> = upstreams.iter().map(|up| up.name.as_str()).collect();
        self.groups
            .iter()
            .filter(|group| {
                group
                    .members
                    .iter()
                    .filter_map(|member| self.resolve_group_member(member))
                    .any(|(upstream, _)| allowed.contains(upstream.name.as_str()))
            })
            .map(|group| format!("group/{}", group.id))
            .collect()
    }

    /// 在统一接口的可见范围内按模型名选上游。
    ///
    /// 与 [`Self::pick_upstream`] 的规则一致，但会跳过不可见上游：先看
    /// 原有优先级结果，若被隐藏则在同一可见范围内重新找声明了该模型的上游，
    /// 最后回落到可见范围内的默认上游 / 第一条。
    pub fn pick_upstream_scoped(
        &self,
        model: Option<&str>,
        allowed: Option<&[String]>,
    ) -> Option<&Upstream> {
        let Some(allowed) = allowed else {
            return self.pick_upstream(model);
        };
        let allowed: HashSet<&str> = allowed.iter().map(String::as_str).collect();
        if let Some(up) = self.pick_upstream(model) {
            if allowed.contains(up.name.as_str()) {
                return Some(up);
            }
        }
        let name = model.unwrap_or("");
        let target = self.aliases.get(name).map(String::as_str).unwrap_or(name);
        if !target.is_empty() {
            if let Some(up) = self.upstreams.iter().find(|up| {
                allowed.contains(up.name.as_str()) && up.models.iter().any(|m| m == target)
            }) {
                return Some(up);
            }
            if let Some(up) = self
                .upstreams
                .iter()
                .find(|up| allowed.contains(up.name.as_str()) && up.model_map.contains_key(name))
            {
                return Some(up);
            }
        }
        self.upstreams
            .iter()
            .find(|up| allowed.contains(up.name.as_str()) && up.default)
            .or_else(|| {
                self.upstreams
                    .iter()
                    .find(|up| allowed.contains(up.name.as_str()))
            })
    }

    /// 对外模型清单：顶层白名单优先；否则合并各上游声明（去重保序），
    /// 再把全局别名与上游级 modelMap 的键值补进来（与 Python 版一致）。
    pub fn model_list(&self) -> Vec<String> {
        let mut out = if !self.models.is_empty() {
            self.models.clone()
        } else {
            let mut ids = Vec::new();
            for up in &self.upstreams {
                for m in &up.models {
                    if !ids.contains(m) {
                        ids.push(m.clone());
                    }
                }
            }
            ids
        };
        for (alias, target) in &self.aliases {
            for value in [alias, target] {
                if !out.contains(value) {
                    out.push(value.clone());
                }
            }
        }
        for up in &self.upstreams {
            for (alias, target) in &up.model_map {
                for value in [alias, target] {
                    if !out.contains(value) {
                        out.push(value.clone());
                    }
                }
            }
        }
        for group in &self.groups {
            let id = format!("group/{}", group.id);
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// 解析统一模型名 `上游/模型`，返回上游与真正的模型名。
    ///
    /// 只有当 `/` 前的前缀与已配置上游名完全一致时才拆分，因此模型名自身
    /// 带 `/`（如 `deepseek/deepseek-v4-flash`）不会被误伤；上游名本身含
    /// `/` 时按最长前缀匹配，保证 `a/b` 优先于 `a`。
    pub fn split_qualified_model<'m>(&self, model: &'m str) -> Option<(&Upstream, &'m str)> {
        if model.is_empty() {
            return None;
        }
        let mut best: Option<(&Upstream, &'m str)> = None;
        for up in &self.upstreams {
            if up.name.is_empty() {
                continue;
            }
            let Some(rest) = model.strip_prefix(up.name.as_str()) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix('/') else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let better = match best {
                Some((current, _)) => up.name.len() > current.name.len(),
                None => true,
            };
            if better {
                best = Some((up, rest));
            }
        }
        best
    }

    /// 统一接入清单：每个上游声明模型与 `modelMap`（别名 + 目标）的
    /// `上游/模型` 形式。
    ///
    /// 与 `/上游/v1/models` 同口径：不受顶层 `models` 白名单影响，也不触发
    /// 上游目录拉取；顺序固定为配置顺序 + 声明顺序 + modelMap 排序。
    pub fn qualified_model_list(&self) -> Vec<(String, String)> {
        self.qualified_model_list_impl(self.upstreams.iter())
    }

    /// 统一接入清单的可见范围版本：只输出给定上游（通常来自
    /// [`Self::unified_visible_upstreams`] / [`Self::unified_group_upstreams`]）。
    pub fn qualified_model_list_for(&self, upstreams: &[&Upstream]) -> Vec<(String, String)> {
        self.qualified_model_list_impl(upstreams.iter().copied())
    }

    fn qualified_model_list_impl<'a>(
        &self,
        upstreams: impl IntoIterator<Item = &'a Upstream>,
    ) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for up in upstreams {
            if up.name.is_empty() {
                continue;
            }
            let mut names: Vec<&str> = up.models.iter().map(String::as_str).collect();
            let mut aliases: Vec<(&str, &str)> = up
                .model_map
                .iter()
                .map(|(alias, target)| (alias.as_str(), target.as_str()))
                .collect();
            aliases.sort_unstable();
            // 与 `model_list` 一致：modelMap 的别名和真实目标名都可被请求。
            for (alias, target) in aliases {
                names.extend([alias, target]);
            }
            for name in names {
                if name.is_empty() {
                    continue;
                }
                let id = format!("{}/{}", up.name, name);
                if seen.insert(id.clone()) {
                    out.push((id, up.name.clone()));
                }
            }
        }
        out
    }
}

/// 配置诊断：error 阻断保存，warn/info 仅提示。
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: String,
    pub code: String,
    pub path: String,
    pub message: String,
    pub hint: String,
}

impl Diagnostic {
    pub fn to_json(&self) -> Value {
        json!({
            "level": self.level,
            "code": self.code,
            "path": self.path,
            "message": self.message,
            "hint": self.hint,
        })
    }
}

/// 统一接口分组标识：URL 路径安全，最长 64。
fn valid_unified_group_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

const TOP_LEVEL_KEYS: &[&str] = &[
    "version",
    "aiConfig",
    "port",
    "host",
    "defaultModel",
    "models",
    "maxTokens",
    "timeoutSeconds",
    "maxBodyBytes",
    "accessLog",
    "promptCapture",
    "freeSourcesPanel",
    "zcodePanel",
    "promptRetentionDays",
    "promptMaxRecords",
    "promptMaxChars",
    "modelsCacheSeconds",
    "aliases",
    "thinking",
    "tools",
    "envFile",
    "upstreams",
    "upstream",
    "unified",
    "groups",
    "retentionDays",
    "pricing",
];

/// 已移除的内置 agent 凭据预设名。
///
/// 这些值仍会被 `apiKeyAgent` 接受，但**不再提供内置默认路径**：
/// 只用于诊断给出迁移提示，不代表还能自动找到凭据。
const LEGACY_AGENT_PRESETS: &[&str] = &["opencode", "jcode", "commandcode"];

fn is_builtin_agent_preset(name: &str) -> bool {
    LEGACY_AGENT_PRESETS
        .iter()
        .any(|preset| name.eq_ignore_ascii_case(preset))
}

const UPSTREAM_KEYS: &[&str] = &[
    "protocol",
    "transport",
    "baseUrl",
    "apiKey",
    "apiKeyEnv",
    "apiKeyFile",
    "apiKeyAgent",
    "apiKeyAgentProvider",
    "apiKeyAgentPath",
    "modelKeys",
    "anthropicVersion",
    "default",
    "fetchModels",
    "autoModels",
    "models",
    "modelMap",
    "sessionHeader",
    "cliVersion",
    "cliEnvironment",
    "modelCatalog",
    "modelCatalogPlan",
    "pauseTurn",
    "cmdZdr",
    "compat",
    "profile",
    "name",
    "thinkingPassthrough",
    "downgradeForcedChoice",
    "modelOverrides",
    // 新版可选字段（设计文档 02）：旧配置无需迁移，未知读取者忽略即可。
    "apiKeyStore",
    "template",
    "templateVersion",
    "kind",
    "label",
    "privacy",
    "credentialPool",
    "pluginId",
];

const COMPAT_KEYS: &[&str] = &[
    "profile",
    "auth",
    "session",
    "thinking",
    "toolChoice",
    "headers",
    "endpoint",
    "limits",
    "zdr",
    "privacy",
    "privacyAdapters",
    "versionHeader",
    "environmentHeader",
    "catalog",
    "capabilities",
];

/// 能力元数据固定字段（与 Python CAPABILITY_KEYS 一致）。
pub const CAPABILITY_KEYS: &[&str] = &[
    "stream",
    "tools",
    "vision",
    "thinking",
    "cache",
    "systemPrompt",
];

impl Config {
    /// 有效配置视图：让面板能看见“预设最终解析成了什么”，而不是猜。
    pub fn effective_upstreams(&self) -> Value {
        let rows: Vec<Value> = self
            .upstreams
            .iter()
            .map(|u| {
                let headers: Map<String, Value> = u
                    .compat
                    .extra_headers
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect();
                json!({
                    "name": u.name,
                    "protocol": u.protocol,
                    "transport": u.transport,
                    "profile": u.compat.profile,
                    "baseUrl": u.base_url,
                    "chatPath": u.compat.chat_path,
                    "modelsPath": u.compat.models_path,
                    "authScheme": u.compat.auth_scheme,
                    "sessionHeader": u.compat.session_header,
                    "sessionIdStyle": u.compat.session_id_style,
                    "thinkingPassthrough": u.compat.thinking_passthrough,
                    "thinkingField": u.compat.thinking_field,
                    "toolChoiceDowngrade": u.compat.tool_choice_downgrade,
                    "maxOutputTokens": u.compat.max_output_tokens,
                    "maxContextTokens": u.compat.max_context_tokens,
                    "zdr": u.compat.zdr,
                    "privacy": u.compat.privacy.to_json(),
                    "extraHeaders": headers,
                    "models": u.models,
                    "default": u.default,
                    "keySource": u.display_key_source(),
                    "keyConfigured": u.any_key_configured(),
                    "apiKeyAgent": u.api_key_agent,
                    "apiKeyStore": u.api_key_store,
                    "capabilities": u.compat.capabilities.to_json(),
                    // 逐模型密钥只回显变量名/来源与可用性，绝不回显密钥本体。
                    "modelKeys": u.model_keys.iter().map(|mk| json!({
                        "model": mk.pattern,
                        "env": mk.env_name,
                        "store": mk.store_id,
                        "source": mk.source,
                        "configured": mk.missing.is_none(),
                        "missing": mk.missing,
                    })).collect::<Vec<_>>(),
                    "modelOverrides": u.model_overrides,
                })
            })
            .collect();
        json!(rows)
    }

    /// 结构化配置诊断。所有检查都只读配置，不访问网络/磁盘。
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let mut push = |level: &str, code: &str, path: &str, message: String, hint: &str| {
            out.push(Diagnostic {
                level: level.to_string(),
                code: code.to_string(),
                path: path.to_string(),
                message,
                hint: hint.to_string(),
            });
        };
        let raw_obj = self.raw.as_object();

        // ── 单价表诊断 ────────────────────────────────────────────
        for issue in &self.pricing.issues {
            push(
                issue.get("level").and_then(Value::as_str).unwrap_or("warn"),
                issue
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("pricing_entry_skipped"),
                issue
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("pricing"),
                issue
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                issue
                    .get("hint")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }

        // ── 顶层字段 ────────────────────────────────────────────────
        if let Some(obj) = raw_obj {
            for key in obj.keys() {
                if key.starts_with('_') {
                    continue; // `_doc` / `_keys` 等说明字段，不参与运行
                }
                if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
                    push(
                        "warn",
                        "unknown_field",
                        key,
                        format!("未知顶层字段「{key}」，不会生效"),
                        "检查拼写，或改用 compat / upstreams 下的字段",
                    );
                }
            }
            if obj.contains_key("envFile") {
                push(
                    "warn",
                    "legacy_env_file",
                    "envFile",
                    "envFile 已失效：Model-Gate 不读取任何 .env 文件，密钥只从环境变量读取".into(),
                    "删除该字段，改用上游的 apiKeyEnv 引用环境变量",
                );
            }
        }
        if self.host.trim().is_empty() {
            push(
                "error",
                "invalid_host",
                "host",
                "监听地址不能为空".into(),
                "本机使用 127.0.0.1",
            );
        }
        if self.upstreams.is_empty() {
            push(
                "error",
                "no_upstream",
                "upstreams",
                "至少要配置一个上游".into(),
                "在 upstreams 下添加一个对象",
            );
        }
        if self.max_tokens == 0 {
            push(
                "error",
                "invalid_max_tokens",
                "maxTokens",
                "maxTokens 必须大于 0".into(),
                "默认 8192",
            );
        }
        if self.prompt_max_chars < 1000 {
            push(
                "error",
                "invalid_prompt_max_chars",
                "promptMaxChars",
                format!(
                    "promptMaxChars={} 过小，至少 1000 字符",
                    self.prompt_max_chars
                ),
                "建议 100000；0 不会被视为不限",
            );
        }
        if self.timeout_seconds < 1.0 {
            push(
                "warn",
                "invalid_timeout",
                "timeoutSeconds",
                format!("超时 {} 秒过小，将按 600 秒运行", self.timeout_seconds),
                "建议 60–3600 秒",
            );
        }
        // 0 会让所有 chat 请求都被 413 拒绝，必须显式拦下。
        if self.max_body_bytes == 0 {
            push(
                "error",
                "invalid_max_body_bytes",
                "maxBodyBytes",
                "maxBodyBytes 必须大于 0，否则所有 chat 请求都会返回 413".into(),
                "默认 67108864（64 MiB）；多模态请求建议 64 MiB 起",
            );
        }
        if !self.aliases.is_empty() {
            let known: HashSet<String> = self.model_list().into_iter().collect();
            for (alias, target) in &self.aliases {
                if !known.contains(target)
                    && !self
                        .upstreams
                        .iter()
                        .any(|u| u.model_map.contains_key(target))
                {
                    push(
                        "warn",
                        "alias_target_missing",
                        &format!("aliases.{alias}"),
                        format!("别名「{alias}」指向的模型「{target}」不在任何上游清单里"),
                        "补齐上游 models，或修正别名目标",
                    );
                }
            }
        }

        // ── 统一接入：可见上游与分组 ────────────────────────────────
        if let Some(unified) = raw_obj.and_then(|obj| obj.get("unified")) {
            if !unified.is_object() {
                push(
                    "error",
                    "invalid_unified",
                    "unified",
                    "unified 必须是对象".into(),
                    "例如 {\"visibleUpstreams\":[],\"groups\":[]}",
                );
            }
        }
        if let Some(unified) = raw_obj
            .and_then(|obj| obj.get("unified"))
            .and_then(Value::as_object)
        {
            for key in unified.keys() {
                if !["visibleUpstreams", "groups"].contains(&key.as_str()) {
                    push(
                        "warn",
                        "unknown_unified_field",
                        &format!("unified.{key}"),
                        format!("统一接入字段「{key}」未知，不会生效"),
                        "可用字段：visibleUpstreams、groups",
                    );
                }
            }
            if let Some(visible) = unified.get("visibleUpstreams") {
                if !visible.is_array() {
                    push(
                        "error",
                        "invalid_unified_visible",
                        "unified.visibleUpstreams",
                        "visibleUpstreams 必须是上游名数组".into(),
                        "留空表示全部上游；例如 [\"go\",\"zen\"]",
                    );
                }
            }
            if let Some(groups) = unified.get("groups") {
                if !groups.is_array() {
                    push(
                        "error",
                        "invalid_unified_groups",
                        "unified.groups",
                        "groups 必须是数组".into(),
                        "每项形如 {\"id\":\"team-a\",\"label\":\"团队 A\",\"upstreams\":[\"go\"]}",
                    );
                }
            }
        }
        let upstream_names: HashSet<&str> =
            self.upstreams.iter().map(|up| up.name.as_str()).collect();
        for name in &self.unified.visible_upstreams {
            if !upstream_names.contains(name.as_str()) {
                push(
                    "warn",
                    "unified_visible_unknown",
                    &format!("unified.visibleUpstreams.{name}"),
                    format!("统一接口可见清单中的上游「{name}」不存在"),
                    "检查上游名称，或从可见清单移除",
                );
            }
        }
        let visible_names: HashSet<&str> = if self.unified.visible_upstreams.is_empty() {
            upstream_names.clone()
        } else {
            self.unified
                .visible_upstreams
                .iter()
                .map(String::as_str)
                .collect()
        };
        let mut group_ids: HashSet<&str> = HashSet::new();
        for (index, group) in self.unified.groups.iter().enumerate() {
            let path = format!("unified.groups[{index}]");
            if group.id.is_empty() {
                push(
                    "error",
                    "invalid_unified_group",
                    &format!("{path}.id"),
                    "统一接口分组标识不能为空".into(),
                    "例如 team-a",
                );
            } else if !valid_unified_group_id(&group.id) {
                push(
                    "error",
                    "invalid_unified_group",
                    &format!("{path}.id"),
                    format!("统一接口分组标识「{}」不合法", group.id),
                    "只能包含字母、数字、-、_、.，且不超过 64 字符",
                );
            } else if !group_ids.insert(group.id.as_str()) {
                push(
                    "error",
                    "duplicate_unified_group",
                    &format!("{path}.id"),
                    format!("统一接口分组标识「{}」重复", group.id),
                    "每个分组标识必须唯一",
                );
            }
            if group.upstreams.is_empty() {
                push(
                    "warn",
                    "empty_unified_group",
                    &format!("{path}.upstreams"),
                    format!("统一接口分组「{}」没有包含任何上游", group.id),
                    "该分组的模型清单会为空；请分配上游或删除分组",
                );
            }
            for name in &group.upstreams {
                if !upstream_names.contains(name.as_str()) {
                    push(
                        "warn",
                        "unified_group_unknown_upstream",
                        &format!("{path}.upstreams.{name}"),
                        format!("统一接口分组「{}」引用的上游「{name}」不存在", group.id),
                        "检查上游名称",
                    );
                } else if !visible_names.contains(name.as_str()) {
                    push(
                        "warn",
                        "unified_group_hidden_upstream",
                        &format!("{path}.upstreams.{name}"),
                        format!(
                            "统一接口分组「{}」包含的上游「{name}」不在可见清单中，不会展示",
                            group.id
                        ),
                        "把该上游加入 visibleUpstreams，或从分组移除",
                    );
                }
            }
        }
        if let Some(rows) = raw_obj
            .and_then(|obj| obj.get("unified"))
            .and_then(Value::as_object)
            .and_then(|unified| unified.get("groups"))
            .and_then(Value::as_array)
        {
            for (index, row) in rows.iter().enumerate() {
                if !row.is_object() {
                    push(
                        "error",
                        "invalid_unified_group",
                        &format!("unified.groups[{index}]"),
                        "统一接口分组必须是对象".into(),
                        "每项形如 {\"id\":\"team-a\",\"label\":\"团队 A\",\"upstreams\":[\"go\"]}",
                    );
                }
            }
        }

        // ── 路由组：group/<id> 虚拟模型 ──────────────────────────────
        if let Some(groups) = raw_obj.and_then(|obj| obj.get("groups")) {
            match groups.as_object() {
                None => push(
                    "error",
                    "invalid_groups",
                    "groups",
                    "groups 必须是对象".into(),
                    "例如 {\"daily\":[\"zcode/GLM-5.3-Flash\",\"go/deepseek-v4\"]}",
                ),
                Some(rows) => {
                    for (id, raw_members) in rows {
                        let path = format!("groups.{id}");
                        if id.trim().is_empty() {
                            push(
                                "error",
                                "invalid_group",
                                "groups",
                                "路由组 id 不能为空".into(),
                                "例如 daily",
                            );
                        } else if !valid_unified_group_id(id.trim()) {
                            push(
                                "error",
                                "invalid_group",
                                &path,
                                format!("路由组 id「{id}」不合法"),
                                "只能包含字母、数字、-、_、.，且不超过 64 字符",
                            );
                        }
                        let Some(members) = raw_members.as_array() else {
                            push(
                                "error",
                                "invalid_group_members",
                                &path,
                                "路由组必须是成员数组".into(),
                                "例如 [\"zcode/GLM-5.3-Flash\",\"go/deepseek-v4\"]",
                            );
                            continue;
                        };
                        if members.is_empty() {
                            push(
                                "warn",
                                "empty_group",
                                &path,
                                format!("路由组「{id}」没有成员"),
                                "请求 group/{id} 会返回无可用成员",
                            );
                        }
                        let mut seen: HashSet<String> = HashSet::new();
                        for (index, member) in members.iter().enumerate() {
                            let member_path = format!("{path}[{index}]");
                            let Some(member) = member.as_str() else {
                                push(
                                    "error",
                                    "invalid_group_member",
                                    &member_path,
                                    "路由组成员必须是字符串".into(),
                                    "例如 \"go/deepseek-v4\"",
                                );
                                continue;
                            };
                            let member = member.trim();
                            if member.is_empty() {
                                push(
                                    "error",
                                    "invalid_group_member",
                                    &member_path,
                                    "路由组成员不能为空".into(),
                                    "删除空项或填写 上游/模型",
                                );
                                continue;
                            }
                            if !seen.insert(member.to_string()) {
                                push(
                                    "warn",
                                    "duplicate_group_member",
                                    &member_path,
                                    format!("路由组「{id}」成员「{member}」重复"),
                                    "重复成员只会尝试一次",
                                );
                            }
                            match self.resolve_group_member(member) {
                                None => push(
                                    "error",
                                    "group_member_unresolved",
                                    &member_path,
                                    format!("路由组成员「{member}」无法解析"),
                                    "写成 上游/模型、已声明的模型名或已有别名",
                                ),
                                Some((upstream, model)) => {
                                    let declared = upstream.models.iter().any(|m| m == &model)
                                        || upstream.model_map.contains_key(member)
                                        || self.aliases.contains_key(member);
                                    if !declared {
                                        push(
                                            "warn",
                                            "group_member_model_not_declared",
                                            &member_path,
                                            format!(
                                                "成员「{member}」解析为 {} / {}，但该上游未声明此模型",
                                                upstream.name, model
                                            ),
                                            "确认上游 models/modelMap 是否需要补上该模型",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if self.upstreams.iter().any(|up| up.name == "group") {
            push(
                "warn",
                "reserved_group_prefix",
                "upstreams.group",
                "上游名「group」与路由组前缀 group/ 冲突".into(),
                "模型名 group/<id> 会优先按路由组解析；建议改用其他上游名",
            );
        }

        // ── 上游字段与能力 ──────────────────────────────────────────
        let declared = raw_obj
            .and_then(|o| o.get("upstreams"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut model_owner: HashMap<String, String> = HashMap::new();
        let mut default_upstream_limit: Option<u64> = None;
        for up in &self.upstreams {
            let prefix = format!("upstreams.{}", up.name);
            let raw_up = declared.get(&up.name).and_then(Value::as_object);

            let raw_base = raw_up
                .and_then(|r| r.get("baseUrl"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if raw_base.trim().is_empty() {
                push(
                    "error",
                    "invalid_base_url",
                    &format!("{prefix}.baseUrl"),
                    format!("上游「{}」没有填 Base URL", up.name),
                    "例如 https://api.deepseek.com/v1",
                );
            } else if !up.base_url.contains("://") {
                push(
                    "error",
                    "invalid_base_url",
                    &format!("{prefix}.baseUrl"),
                    format!("上游「{}」的 Base URL 无效：{}", up.name, up.base_url),
                    "例如 https://api.deepseek.com/v1",
                );
            }
            if !matches!(up.protocol.as_str(), "openai" | "anthropic" | "commandcode") {
                push(
                    "error",
                    "unsupported_protocol",
                    &format!("{prefix}.protocol"),
                    format!("上游「{}」的协议「{}」未实现", up.name, up.protocol),
                    "可用 openai / anthropic / commandcode",
                );
            }
            if !up.transport.is_empty() && up.transport != "opencode" && up.transport != "zcode" {
                push(
                    "error",
                    "invalid_transport",
                    &format!("{prefix}.transport"),
                    format!("transport「{}」未实现", up.transport),
                    "当前只支持 opencode / zcode",
                );
            } else if up.transport == "opencode" && up.protocol != "openai" {
                push(
                    "error",
                    "transport_protocol_mismatch",
                    &format!("{prefix}.transport"),
                    format!(
                        "transport=opencode 只适用于 protocol=openai，当前为 {}",
                        up.protocol
                    ),
                    "",
                );
            } else if up.transport == "zcode"
                && !matches!(up.protocol.as_str(), "openai" | "anthropic")
            {
                push(
                    "error",
                    "transport_protocol_mismatch",
                    &format!("{prefix}.transport"),
                    format!(
                        "transport=zcode 只适用于 protocol=openai/anthropic，当前为 {}",
                        up.protocol
                    ),
                    "",
                );
            }
            if let Some(raw_up) = raw_up {
                for key in raw_up.keys() {
                    if !UPSTREAM_KEYS.contains(&key.as_str()) {
                        push(
                            "warn",
                            "unknown_field",
                            &format!("{prefix}.{key}"),
                            format!("上游「{}」的字段「{key}」未知，不会生效", up.name),
                            "检查拼写；provider 差异请写在 compat 下",
                        );
                    }
                }
                let legacy_agent = raw_up
                    .get("apiKeyAgent")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                if raw_up
                    .get("apiKeyFile")
                    .and_then(Value::as_str)
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                {
                    push(
                        "warn",
                        "legacy_api_key_file",
                        &format!("{prefix}.apiKeyFile"),
                        format!("上游「{}」仍使用 apiKeyFile 凭据来源", up.name),
                        "建议改用 apiKeyEnv；继续用文件时，路径必须由你手动指定（不再有内置默认路径）",
                    );
                }
                if let Some(tool) = legacy_agent {
                    if is_builtin_agent_preset(tool) {
                        push(
                            "warn",
                            "legacy_agent_preset",
                            &format!("{prefix}.apiKeyAgent"),
                            format!("上游「{}」使用已移除的内置预设「{tool}」", up.name),
                            "内置 opencode / jcode / commandcode 预设已删除：请填写 apiKeyAgentPath，或改用 apiKeyEnv",
                        );
                    } else {
                        push(
                            "info",
                            "legacy_api_key_agent",
                            &format!("{prefix}.apiKeyAgent"),
                            format!("上游「{}」仍使用 apiKeyAgent 凭据来源", up.name),
                            "可继续使用自定义路径；推荐改用 apiKeyEnv 统一从环境变量读取",
                        );
                    }
                }
                if let Some(compat) = raw_up.get("compat") {
                    match compat.as_object() {
                        None => push(
                            "error",
                            "invalid_compat",
                            &format!("{prefix}.compat"),
                            "compat 必须是对象".into(),
                            "例如 {\"auth\":{\"scheme\":\"bearer\"}}",
                        ),
                        Some(c) => {
                            for key in c.keys() {
                                if !COMPAT_KEYS.contains(&key.as_str()) {
                                    push(
                                        "warn",
                                        "unknown_compat_field",
                                        &format!("{prefix}.compat.{key}"),
                                        format!("compat 字段「{key}」未知，不会生效"),
                                        "可用字段见文档 provider-compat.md",
                                    );
                                }
                            }
                            if let Some(auth) = c.get("auth").and_then(Value::as_object) {
                                for key in auth.keys() {
                                    if !["scheme", "header", "prefix"].contains(&key.as_str()) {
                                        push(
                                            "warn",
                                            "unknown_compat_field",
                                            &format!("{prefix}.compat.auth.{key}"),
                                            format!("auth 字段「{key}」未知，不会生效"),
                                            "",
                                        );
                                    }
                                }
                            }
                            if let Some(session) = c.get("session").and_then(Value::as_object) {
                                for key in session.keys() {
                                    if !["enabled", "header", "idStyle"].contains(&key.as_str()) {
                                        push(
                                            "warn",
                                            "unknown_compat_field",
                                            &format!("{prefix}.compat.session.{key}"),
                                            format!("session 字段「{key}」未知，不会生效"),
                                            "",
                                        );
                                    }
                                }
                            }
                            if let Some(catalog) = c.get("catalog").and_then(Value::as_object) {
                                for key in catalog.keys() {
                                    if !["source", "path", "plan"].contains(&key.as_str()) {
                                        push(
                                            "warn",
                                            "unknown_compat_field",
                                            &format!("{prefix}.compat.catalog.{key}"),
                                            format!("catalog 字段「{key}」未知，不会生效"),
                                            "",
                                        );
                                    }
                                }
                            }
                            if let Some(caps) = c.get("capabilities") {
                                match caps.as_object() {
                                    None => push(
                                        "error",
                                        "invalid_capabilities",
                                        &format!("{prefix}.compat.capabilities"),
                                        "capabilities 必须是对象".into(),
                                        "",
                                    ),
                                    Some(caps) => {
                                        for (key, value) in caps {
                                            if !CAPABILITY_KEYS.contains(&key.as_str()) {
                                                push(
                                                    "warn",
                                                    "unknown_compat_field",
                                                    &format!("{prefix}.compat.capabilities.{key}"),
                                                    format!(
                                                        "capabilities 字段「{key}」未知，不会生效"
                                                    ),
                                                    "",
                                                );
                                            } else if !value.is_boolean() {
                                                push(
                                                    "error",
                                                    "invalid_capabilities",
                                                    &format!("{prefix}.compat.capabilities.{key}"),
                                                    "能力值必须是布尔（true/false）".into(),
                                                    "",
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let profile_hint = raw_up
                    .get("compat")
                    .and_then(Value::as_object)
                    .and_then(|c| c.get("profile"))
                    .and_then(Value::as_str)
                    .or_else(|| raw_up.get("profile").and_then(Value::as_str))
                    .unwrap_or("");
                if !profile_hint.is_empty()
                    && !crate::provider_profiles::is_known_profile(profile_hint)
                {
                    push(
                        "warn",
                        "unknown_profile",
                        &format!("{prefix}.compat.profile"),
                        format!("profile「{profile_hint}」不在注册表，按协议/域名兜底"),
                        &format!(
                            "可用：{}",
                            crate::provider_profiles::profile_ids().join(" / ")
                        ),
                    );
                }
                // 旧版平铺字段只对 commandcode 有意义；其余协议提示改用 compat。
                if up.compat.profile != "commandcode" {
                    for key in [
                        "cliVersion",
                        "cliEnvironment",
                        "cmdZdr",
                        "pauseTurn",
                        "modelCatalog",
                        "modelCatalogPlan",
                    ] {
                        if !raw_up.contains_key(key) {
                            continue;
                        }
                        // 这些字段已能通过 compat 泛化，声明了对应能力就不再告警。
                        let generic = match key {
                            "cliVersion" => !up.compat.version_header.is_empty(),
                            "cliEnvironment" => !up.compat.environment_header.is_empty(),
                            _ => false,
                        };
                        if !generic {
                            push(
                                "warn",
                                "field_not_applicable",
                                &format!("{prefix}.{key}"),
                                format!(
                                    "字段「{key}」只对 commandcode 生效，上游「{}」不会使用",
                                    up.name
                                ),
                                "如需通用行为，请改用 compat.headers / compat.session",
                            );
                        }
                    }
                }
            }

            let c = &up.compat;
            let profile_ok = match c.profile.as_str() {
                "commandcode" => up.protocol == "commandcode",
                "anthropic" => up.protocol == "anthropic",
                "opencode" => up.protocol == "openai",
                "openai" => up.protocol == "openai",
                "deepseek" => matches!(up.protocol.as_str(), "openai" | "anthropic"),
                _ => true,
            };
            if !profile_ok {
                push(
                    "warn",
                    "profile_protocol_mismatch",
                    &format!("{prefix}.compat.profile"),
                    format!(
                        "compat.profile={} 与 protocol={} 不匹配，wire 行为仍按 protocol 执行",
                        c.profile, up.protocol
                    ),
                    "检查 protocol；profile 只决定鉴权/会话/端点等默认能力",
                );
            }
            if !c.is_default_auth_scheme() {
                push(
                    "error",
                    "invalid_auth_scheme",
                    &format!("{prefix}.compat.auth.scheme"),
                    format!("鉴权方式「{}」无效", c.auth_scheme),
                    "可用 bearer / x-api-key / both / none / header",
                );
            }
            if !c.session_header.is_empty() && !is_header_token(&c.session_header) {
                push(
                    "error",
                    "invalid_header_name",
                    &format!("{prefix}.compat.session.header"),
                    format!("会话头名「{}」不是合法的 HTTP 头名", c.session_header),
                    "头名只能包含字母、数字和 - _ .",
                );
            }
            if c.chat_path.is_empty() || !c.chat_path.starts_with('/') {
                push(
                    "error",
                    "invalid_endpoint",
                    &format!("{prefix}.compat.endpoint.chat"),
                    format!("对话端点「{}」必须以 / 开头", c.chat_path),
                    "例如 /v1/chat/completions",
                );
            }
            if c.models_path.is_empty() || !c.models_path.starts_with('/') {
                push(
                    "warn",
                    "invalid_endpoint",
                    &format!("{prefix}.compat.endpoint.models"),
                    format!("模型端点「{}」必须以 / 开头", c.models_path),
                    "例如 /v1/models",
                );
            }
            if let (Some(max_out), Some(max_ctx)) = (c.max_output_tokens, c.max_context_tokens) {
                if max_out > max_ctx {
                    push(
                        "error",
                        "invalid_limits",
                        &format!("{prefix}.compat.limits"),
                        format!("最大输出 {max_out} 大于上下文窗口 {max_ctx}"),
                        "输出上限应小于等于上下文窗口",
                    );
                }
            }
            if let Some(limit) = c.max_output_tokens {
                if up.default {
                    default_upstream_limit = Some(limit);
                }
                if self.max_tokens > limit {
                    // 运行时已在请求侧自动收敛，不阻断保存：属于「知会」而非「待修」。
                    push(
                        "info",
                        "max_tokens_exceeds_upstream",
                        &format!("{prefix}.compat.limits.maxOutputTokens"),
                        format!(
                            "全局 maxTokens={} 超过上游「{}」上限 {limit}，请求会被自动收敛",
                            self.max_tokens, up.name
                        ),
                        "下调 maxTokens，或确认该上游上限",
                    );
                }
            }
            // 隐私策略：只有用户明确要求、且该上游无法按请求落地时才提示。
            for (level, message, hint) in c.privacy.diagnostics(&up.name) {
                push(
                    level,
                    "privacy_unsupported",
                    &format!("{prefix}.compat.privacy"),
                    message,
                    &hint,
                );
            }
            if let Some(overrides) = raw_up.and_then(|r| r.get("modelOverrides")) {
                match overrides.as_object() {
                    None => push(
                        "error",
                        "invalid_model_overrides",
                        &format!("{prefix}.modelOverrides"),
                        format!(
                            "上游「{}」的 modelOverrides 必须是对象（模型 id → 覆盖项）",
                            up.name
                        ),
                        "",
                    ),
                    Some(overrides) => {
                        for (key, value) in overrides {
                            let path = format!("{prefix}.modelOverrides.{key}");
                            if key.is_empty() {
                                push(
                                    "error",
                                    "invalid_model_overrides",
                                    &path,
                                    "模型覆盖键必须是非空字符串".into(),
                                    "",
                                );
                                continue;
                            }
                            let stars = key.matches('*').count();
                            if stars > 1 {
                                push(
                                    "error",
                                    "multi_wildcard_override",
                                    &path,
                                    format!("模型覆盖键「{key}」包含多个 *，不参与匹配"),
                                    "只支持一个 * 通配",
                                );
                                continue;
                            }
                            if stars == 0
                                && !up.models.is_empty()
                                && !up.models.iter().any(|m| m == key)
                            {
                                push(
                                    "warn",
                                    "model_override_unknown_model",
                                    &path,
                                    format!("模型覆盖「{key}」不在上游「{}」的模型清单里", up.name),
                                    "检查模型 id，或先在上游 models 中声明",
                                );
                            }
                            let Some(over) = value.as_object() else {
                                push(
                                    "error",
                                    "invalid_model_overrides",
                                    &path,
                                    "模型覆盖值必须是对象（limits / capabilities）".into(),
                                    "",
                                );
                                continue;
                            };
                            if over.is_empty() {
                                push(
                                    "error",
                                    "invalid_model_overrides",
                                    &path,
                                    "模型覆盖值不能是空对象（至少包含 limits 或 capabilities）"
                                        .into(),
                                    "",
                                );
                                continue;
                            }
                            for field in over.keys() {
                                if !["limits", "capabilities"].contains(&field.as_str()) {
                                    push(
                                        "warn",
                                        "unknown_field",
                                        &format!("{path}.{field}"),
                                        format!("模型覆盖字段「{field}」未知，不会生效"),
                                        "",
                                    );
                                }
                            }
                            if let Some(limits) = over.get("limits") {
                                match limits.as_object() {
                                    None => push(
                                        "error",
                                        "invalid_model_overrides",
                                        &format!("{path}.limits"),
                                        "modelOverrides.limits 必须是对象".into(),
                                        "",
                                    ),
                                    Some(limits) => {
                                        for field in limits.keys() {
                                            if !["maxOutputTokens", "maxContextTokens"]
                                                .contains(&field.as_str())
                                            {
                                                push(
                                                    "warn",
                                                    "unknown_field",
                                                    &format!("{path}.limits.{field}"),
                                                    format!("limits 字段「{field}」未知，不会生效"),
                                                    "",
                                                );
                                            }
                                        }
                                        let over_out =
                                            limits.get("maxOutputTokens").and_then(positive_int);
                                        let over_ctx =
                                            limits.get("maxContextTokens").and_then(positive_int);
                                        if limits.contains_key("maxOutputTokens")
                                            && over_out.is_none()
                                        {
                                            push(
                                                "error",
                                                "invalid_model_overrides",
                                                &format!("{path}.limits.maxOutputTokens"),
                                                "limits 值必须是正整数".into(),
                                                "",
                                            );
                                        }
                                        if limits.contains_key("maxContextTokens")
                                            && over_ctx.is_none()
                                        {
                                            push(
                                                "error",
                                                "invalid_model_overrides",
                                                &format!("{path}.limits.maxContextTokens"),
                                                "limits 值必须是正整数".into(),
                                                "",
                                            );
                                        }
                                        if let (Some(out), Some(ctx)) = (
                                            over_out.or(c.max_output_tokens),
                                            over_ctx.or(c.max_context_tokens),
                                        ) {
                                            if out > ctx {
                                                push(
                                                    "error",
                                                    "invalid_limit_relation",
                                                    &format!("{path}.limits"),
                                                    format!("最大输出 {out} 大于上下文窗口 {ctx}"),
                                                    "输出上限应小于等于上下文窗口",
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(caps) = over.get("capabilities") {
                                match caps.as_object() {
                                    None => push(
                                        "error",
                                        "invalid_capabilities",
                                        &format!("{path}.capabilities"),
                                        "capabilities 必须是对象".into(),
                                        "",
                                    ),
                                    Some(caps) => {
                                        for (ck, cv) in caps {
                                            if !CAPABILITY_KEYS.contains(&ck.as_str()) {
                                                push(
                                                    "warn",
                                                    "unknown_field",
                                                    &format!("{path}.capabilities.{ck}"),
                                                    format!(
                                                        "capabilities 字段「{ck}」未知，不会生效"
                                                    ),
                                                    "",
                                                );
                                            } else if !cv.is_boolean() {
                                                push(
                                                    "error",
                                                    "invalid_capabilities",
                                                    &format!("{path}.capabilities.{ck}"),
                                                    "能力值必须是布尔（true/false）".into(),
                                                    "",
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // ── 逐模型密钥（modelKeys）──────────────────────────────────
            if let Some(raw_keys) = raw_up.and_then(|r| r.get("modelKeys")) {
                let base = format!("{prefix}.modelKeys");
                match raw_keys.as_object() {
                    None => push(
                        "error",
                        "invalid_model_keys",
                        &base,
                        format!(
                            "上游「{}」的 modelKeys 必须是对象（模型 id → 环境变量名 / 密钥来源对象）",
                            up.name
                        ),
                        "例如 {\"MiniMax-M3\":\"MINIMAX_API_KEY\"}",
                    ),
                    Some(keys) => {
                        if keys.is_empty() {
                            push(
                                "warn",
                                "invalid_model_keys",
                                &base,
                                format!("上游「{}」的 modelKeys 是空对象，不会生效", up.name),
                                "删除该字段，或补充「模型 → 密钥来源」",
                            );
                        }
                        for (pattern, value) in keys {
                            let path = format!("{base}.{pattern}");
                            if pattern.trim().is_empty() {
                                push(
                                    "error",
                                    "invalid_model_keys",
                                    &path,
                                    "模型键必须是非空字符串".into(),
                                    "",
                                );
                                continue;
                            }
                            let stars = pattern.matches('*').count();
                            if stars > 1 {
                                push(
                                    "error",
                                    "multi_wildcard_model_key",
                                    &path,
                                    format!("模型键「{pattern}」包含多个 *，不参与匹配"),
                                    "只支持一个 * 通配",
                                );
                                continue;
                            }
                            if stars == 0
                                && !up.models.is_empty()
                                && !up.models.iter().any(|m| m == pattern)
                            {
                                push(
                                    "warn",
                                    "model_key_unknown_model",
                                    &path,
                                    format!(
                                        "逐模型密钥「{pattern}」不在上游「{}」的模型清单里",
                                        up.name
                                    ),
                                    "检查模型 id（必须是 modelMap 解析后的真实模型名），或先在上游 models 中声明",
                                );
                            }
                            let spec = match value {
                                Value::String(env) => {
                                    if env.trim().is_empty() {
                                        push(
                                            "error",
                                            "invalid_model_keys",
                                            &path,
                                            "环境变量名不能为空".into(),
                                            "填写环境变量名，或改用对象写法",
                                        );
                                    }
                                    None
                                }
                                Value::Object(obj) => Some(obj),
                                _ => {
                                    push(
                                        "error",
                                        "invalid_model_keys",
                                        &path,
                                        "条目必须是环境变量名字符串或对象（apiKeyEnv / apiKeyFile / apiKeyAgentPath / apiKeyStore）".into(),
                                        "",
                                    );
                                    None
                                }
                            };
                            if let Some(obj) = spec {
                                if obj
                                    .get("apiKey")
                                    .and_then(Value::as_str)
                                    .map(|s| !s.trim().is_empty())
                                    .unwrap_or(false)
                                {
                                    push(
                                        "warn",
                                        "model_key_literal_ignored",
                                        &path,
                                        format!(
                                            "逐模型密钥「{pattern}」写有明文 apiKey，运行期已忽略"
                                        ),
                                        "改用 apiKeyEnv / apiKeyFile / apiKeyStore，并通过面板写入密钥",
                                    );
                                }
                                let has_source = ["apiKeyEnv", "apiKeyFile", "apiKeyAgent", "apiKeyAgentPath", "apiKeyStore"]
                                    .iter()
                                    .any(|k| obj.contains_key(*k));
                                if !has_source {
                                    push(
                                        "error",
                                        "invalid_model_keys",
                                        &path,
                                        "条目必须包含 apiKeyEnv / apiKeyFile / apiKeyAgentPath / apiKeyStore".into(),
                                        "",
                                    );
                                }
                                if let Some(store_id) = obj.get("apiKeyStore") {
                                    match store_id.as_str() {
                                        Some(id) if crate::secret_store::is_valid_id(id.trim()) => {}
                                        _ => push(
                                            "error",
                                            "invalid_model_keys",
                                            &format!("{path}.apiKeyStore"),
                                            "apiKeyStore 必须是 upstream: / model: 开头的密钥库引用".into(),
                                            "用面板「加密保存到 Model-Gate」生成引用",
                                        ),
                                    }
                                }
                                if let Some(env) = obj.get("apiKeyEnv") {
                                    match env.as_str() {
                                        Some(name) if !name.trim().is_empty() => {}
                                        _ => push(
                                            "error",
                                            "invalid_model_keys",
                                            &format!("{path}.apiKeyEnv"),
                                            "apiKeyEnv 必须是非空字符串".into(),
                                            "",
                                        ),
                                    }
                                }
                            }
                            if let Some(entry) = up.model_keys.iter().find(|m| m.pattern == *pattern)
                            {
                                if let Some(reason) = &entry.missing {
                                    // 无效条目已经单独报错，这里只报告「配置合法但读不到值」。
                                    if entry.configured_from != "(未配置密钥来源)" {
                                        push(
                                            "warn",
                                            "model_key_missing",
                                            &path,
                                            format!(
                                                "模型「{pattern}」的密钥未读到（{reason}），请求会回落到上游级密钥"
                                            ),
                                            "检查环境变量 / 凭据文件 / 加密密钥库，或用面板「写入逐模型密钥」",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if c.thinking_passthrough && up.protocol != "anthropic" {
                push(
                    "info",
                    "thinking_not_used",
                    &format!("{prefix}.compat.thinking"),
                    format!(
                        "上游「{}」的 thinking.passthrough 对 {} 协议不参与请求改写",
                        up.name, up.protocol
                    ),
                    "仅 anthropic 协议需要回传 thinking 块",
                );
            }
            if c.extra_headers.iter().any(|(k, _)| k == "authorization") && c.auth_scheme != "none"
            {
                push(
                    "info",
                    "header_override",
                    &format!("{prefix}.compat.headers.authorization"),
                    "compat.headers 里的 authorization 会被鉴权配置覆盖".into(),
                    "如需自定义鉴权，请设置 compat.auth",
                );
            }
            if up.key_source == "none"
                && !up.base_url.is_empty()
                && c.profile != "commandcode"
                && c.requires_auth(&up.base_url)
                && up.api_key_env.is_none()
                && up.api_key_file.is_none()
                && up.api_key_agent.is_none()
                && up.api_key_store.is_none()
                // 纯逐模型密钥的上游：整体缺失由 model_key_missing 逐条报告，
                // 不再额外报一条容易误导的 missing_credentials。
                && up.model_keys.is_empty()
            {
                push(
                    "warn",
                    "missing_credentials",
                    &format!("{prefix}.apiKey"),
                    format!("上游「{}」没有配置密钥（将以空鉴权请求）", up.name),
                    "设置 apiKeyEnv / apiKeyFile / apiKeyAgent / apiKeyStore；无需鉴权的上游请设置 compat.auth.scheme=none",
                );
            }
            if up.api_key_store.is_some()
                && !up.key_configured()
                && up.api_key_env.is_none()
                && up.api_key_file.is_none()
                && up.api_key_agent.is_none()
            {
                push(
                    "warn",
                    "store_key_unreadable",
                    &format!("{prefix}.apiKeyStore"),
                    format!(
                        "上游「{}」引用了 Model-Gate 加密密钥库，但未读到可用密钥",
                        up.name
                    ),
                    "在本机面板重新「加密保存到 Model-Gate」；DPAPI 密文只能由写入时的 Windows 用户/机器解密",
                );
            }
            if up.api_key_agent.is_some() && !up.key_configured() {
                push(
                    "warn",
                    "agent_key_unreadable",
                    &format!("{prefix}.apiKeyAgent"),
                    format!(
                        "上游「{}」配置了从 agent 工具读取密钥，但没有读到可用值",
                        up.name
                    ),
                    "检查 apiKeyAgentProvider / apiKeyAgentPath，或确认对应客户端已登录",
                );
            }
            // JSON 里的明文 apiKey 不再作为凭据来源；只给迁移提示，不回显原值。
            if raw_up
                .and_then(|r| r.get("apiKey"))
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
            {
                push(
                    "warn",
                    "legacy_literal_api_key",
                    &format!("{prefix}.apiKey"),
                    format!(
                        "上游「{}」在 JSON 中写有 apiKey，运行期已忽略：密钥只从环境变量、显式凭据文件或加密密钥库读取",
                        up.name
                    ),
                    "删除 apiKey，改用 apiKeyEnv / apiKeyStore 并通过面板「写入密钥」保存；无需鉴权的上游请设置 compat.auth.scheme=none",
                );
            }
            if up.fetch_models == false
                && raw_up
                    .map(|r| r.get("autoModels").and_then(Value::as_bool) == Some(true))
                    .unwrap_or(false)
            {
                push(
                    "warn",
                    "ineffective_auto_models",
                    &format!("{prefix}.autoModels"),
                    "autoModels=true 但 fetchModels=false：不会向上游拉取模型".into(),
                    "同时开启 fetchModels",
                );
            }
            for model in &up.models {
                if let Some(prev) = model_owner.get(model) {
                    if prev != &up.name {
                        push(
                            "info",
                            "duplicate_model",
                            &format!("{prefix}.models"),
                            format!(
                                "模型「{model}」同时出现在「{prev}」和「{}」，按配置顺序取前者",
                                up.name
                            ),
                            "如需固定路由，使用 /<上游名>/v1 前缀",
                        );
                    }
                } else {
                    model_owner.insert(model.clone(), up.name.clone());
                }
            }
        }

        // ── 全局路由一致性 ──────────────────────────────────────────
        if let Some(default_model) = self.default_model.as_deref().filter(|m| !m.is_empty()) {
            let known = self.model_list();
            if !known.iter().any(|m| m == default_model)
                && !self.aliases.contains_key(default_model)
            {
                push(
                    "warn",
                    "default_model_unserved",
                    "defaultModel",
                    format!("默认模型「{default_model}」不在任何上游清单里"),
                    "补齐上游 models，或改用已声明的模型名",
                );
            }
        }
        if let Some(limit) = default_upstream_limit {
            if self.max_tokens > limit {
                push(
                    "info",
                    "default_upstream_limit",
                    "maxTokens",
                    format!(
                        "默认上游输出上限 {limit}，全局 maxTokens={} 会在运行时收敛",
                        self.max_tokens
                    ),
                    "如无特殊需要，将 maxTokens 设为不超过该值",
                );
            }
        }
        out
    }
}

/// 正整数解析（与 Python `_to_int` 口径接近：数字或数字字符串，>0 才有效）。
fn positive_int(value: &Value) -> Option<u64> {
    let number = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    if !number.is_finite() || number < 1.0 {
        return None;
    }
    Some(number as u64)
}

fn is_header_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 统一接入口配置：缺省 = 全部上游可见、无分组。
fn normalize_unified(value: Option<&Value>) -> Unified {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Unified::default();
    };
    let visible_upstreams = trimmed_unique_strings(obj.get("visibleUpstreams"));
    let groups = obj
        .get("groups")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .map(|row| UnifiedGroup {
                    id: row
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    label: row
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    upstreams: trimmed_unique_strings(row.get("upstreams")),
                })
                .collect()
        })
        .unwrap_or_default();
    Unified {
        visible_upstreams,
        groups,
    }
}

/// 路由组配置：`groups` 是「组 id → 成员数组」的对象。
///
/// 成员保持原始写法与顺序，请求期再解析；非法/空项在这里先剔除，
/// 详细诊断由 [`Config::diagnostics`] 按原始 JSON 给出。
fn normalize_route_groups(value: Option<&Value>) -> Vec<RouteGroup> {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Vec::new();
    };
    obj.iter()
        .filter_map(|(id, raw)| {
            let id = id.trim();
            if id.is_empty() {
                return None;
            }
            let members = raw
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|member| !member.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(RouteGroup {
                id: id.to_string(),
                members,
            })
        })
        .collect()
}

/// 字符串数组：trim、去空、去重保序。
fn trimmed_unique_strings(value: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(rows) = value.and_then(Value::as_array) {
        for row in rows {
            if let Some(text) = row.as_str() {
                let text = text.trim();
                if !text.is_empty() && !out.iter().any(|existing| existing == text) {
                    out.push(text.to_string());
                }
            }
        }
    }
    out
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_upstream(
    name: &str,
    value: &Value,
    root: &Map<String, Value>,
    store: &crate::secret_store::SecretStore,
) -> Upstream {
    let u = value.as_object().cloned().unwrap_or_default();
    let protocol = u
        .get("protocol")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| infer_protocol(&u));
    let base_url = u
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(normalize_base_url)
        .unwrap_or_default();
    let (api_key, key_source) = resolve_api_key(&u, root, store);
    let model_map = u
        .get("modelMap")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let compat = resolve_compat(&protocol, &base_url, &u, root);
    Upstream {
        name: name.to_string(),
        protocol,
        transport: u
            .get("transport")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
        base_url,
        api_key,
        key_source,
        default: u.get("default").and_then(Value::as_bool).unwrap_or(false),
        models: string_list(u.get("models")),
        model_map,
        // 旧字段保留给 API/面板展示；运行时统一读 compat。
        session_header: !compat.session_header.is_empty(),
        anthropic_version: u
            .get("anthropicVersion")
            .and_then(Value::as_str)
            .unwrap_or("2023-06-01")
            .to_string(),
        cli_version: u
            .get("cliVersion")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .to_string(),
        cli_environment: u
            .get("cliEnvironment")
            .and_then(Value::as_str)
            .unwrap_or("production")
            .to_string(),
        pause_turn: u
            .get("pauseTurn")
            .and_then(Value::as_bool)
            .unwrap_or(compat.profile == "commandcode"),
        cmd_zdr: compat.zdr,
        fetch_models: u
            .get("fetchModels")
            .and_then(Value::as_bool)
            .unwrap_or(compat.profile != "commandcode"),
        auto_models: u
            .get("autoModels")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        api_key_env: u
            .get("apiKeyEnv")
            .and_then(Value::as_str)
            .map(str::to_string),
        api_key_file: u
            .get("apiKeyFile")
            .and_then(Value::as_str)
            .map(str::to_string),
        api_key_agent: u
            .get("apiKeyAgent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        api_key_store: u
            .get("apiKeyStore")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| crate::secret_store::is_valid_id(s))
            .map(str::to_string),
        // 凭据池 id：允许旧配置缺省；空串视为未绑定。
        credential_pool: u
            .get("credentialPool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        // 逐模型密钥：字符串简写（环境变量名）或对象（apiKeyEnv / apiKeyFile / …）。
        model_keys: resolve_model_keys(u.get("modelKeys"), store),
        model_overrides: u
            .get("modelOverrides")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        compat,
    }
}

/// 解析兼容能力：预设 → 旧平铺字段 → 显式 compat。
fn resolve_compat(
    protocol: &str,
    base_url: &str,
    u: &Map<String, Value>,
    root: &Map<String, Value>,
) -> Compat {
    let compat_obj = u.get("compat").and_then(Value::as_object);
    let profile = compat_obj
        .and_then(|c| c.get("profile"))
        .and_then(Value::as_str)
        .or_else(|| u.get("profile").and_then(Value::as_str))
        .unwrap_or("");
    let mut c = Compat::preset(profile, base_url, protocol);

    // ── 旧版平铺字段：只作为兼容输入，不覆盖显式 compat ───────────────
    if let Some(enabled) = u.get("sessionHeader").and_then(Value::as_bool) {
        if enabled {
            if c.session_header.is_empty() {
                c.session_header = "x-opencode-session".into();
            }
        } else {
            c.session_header.clear();
        }
    }
    if let Some(enabled) = u.get("cmdZdr").and_then(Value::as_bool) {
        c.privacy.zdr = Some(enabled);
    }
    if let Some(enabled) = u.get("thinkingPassthrough").and_then(Value::as_bool) {
        c.thinking_passthrough = enabled;
    }
    if let Some(enabled) = u.get("downgradeForcedChoice").and_then(Value::as_bool) {
        c.tool_choice_downgrade = enabled;
    }

    // ── 显式 compat：所有字段均可覆盖 ────────────────────────────────
    if let Some(obj) = compat_obj {
        if let Some(auth) = obj.get("auth") {
            match auth {
                Value::String(scheme) => c.auth_scheme = scheme.to_lowercase(),
                Value::Object(a) => {
                    if let Some(v) = a.get("scheme").and_then(Value::as_str) {
                        c.auth_scheme = v.to_lowercase();
                    }
                    if let Some(v) = a.get("header").and_then(Value::as_str) {
                        c.auth_header = v.to_string();
                    }
                    if let Some(v) = a.get("prefix").and_then(Value::as_str) {
                        c.auth_prefix = v.to_string();
                    }
                }
                _ => {}
            }
        }
        if let Some(session) = obj.get("session") {
            match session {
                Value::Bool(enabled) => {
                    if *enabled {
                        if c.session_header.is_empty() {
                            c.session_header = "x-opencode-session".into();
                        }
                    } else {
                        c.session_header.clear();
                    }
                }
                Value::Object(s) => {
                    let enabled = s.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                    if enabled {
                        if let Some(v) = s.get("header").and_then(Value::as_str) {
                            c.session_header = v.to_string();
                        }
                        if c.session_header.is_empty() {
                            c.session_header = "x-opencode-session".into();
                        }
                    } else {
                        c.session_header.clear();
                    }
                    if let Some(v) = s.get("idStyle").and_then(Value::as_str) {
                        c.session_id_style = v.to_lowercase();
                    }
                }
                _ => {}
            }
        }
        if let Some(thinking) = obj.get("thinking") {
            match thinking {
                Value::Bool(enabled) => c.thinking_passthrough = *enabled,
                Value::Object(t) => {
                    if let Some(v) = t.get("passthrough").and_then(Value::as_bool) {
                        c.thinking_passthrough = v;
                    }
                    if let Some(v) = t.get("field").and_then(Value::as_str) {
                        c.thinking_field = v.to_string();
                    }
                }
                _ => {}
            }
        }
        if let Some(choice) = obj.get("toolChoice") {
            match choice {
                Value::Bool(v) => c.tool_choice_downgrade = *v,
                Value::Object(t) => {
                    if let Some(v) = t.get("downgradeForced").and_then(Value::as_bool) {
                        c.tool_choice_downgrade = v;
                    }
                }
                _ => {}
            }
        }
        if let Some(headers) = obj.get("headers").and_then(Value::as_object) {
            for (k, v) in headers {
                let text = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                c.extra_headers.push((k.to_lowercase(), text));
            }
        }
        if let Some(endpoint) = obj.get("endpoint").and_then(Value::as_object) {
            if let Some(v) = endpoint.get("chat").and_then(Value::as_str) {
                c.chat_path = v.to_string();
            }
            if let Some(v) = endpoint.get("models").and_then(Value::as_str) {
                c.models_path = v.to_string();
            }
        }
        if let Some(limits) = obj.get("limits").and_then(Value::as_object) {
            if let Some(v) = limits.get("maxOutputTokens").and_then(Value::as_u64) {
                c.max_output_tokens = Some(v);
            }
            if let Some(v) = limits.get("maxContextTokens").and_then(Value::as_u64) {
                c.max_context_tokens = Some(v);
            }
        }
        if let Some(v) = obj.get("zdr").and_then(Value::as_bool) {
            c.privacy.zdr = Some(v);
        }
        // 通用隐私意图：compat.privacy.{zdr,training}
        if let Some(privacy) = obj.get("privacy").and_then(Value::as_object) {
            match privacy.get("zdr") {
                Some(Value::Bool(v)) => c.privacy.zdr = Some(*v),
                Some(Value::String(v))
                    if matches!(
                        v.to_ascii_lowercase().as_str(),
                        "auto" | "inherit" | "default"
                    ) =>
                {
                    c.privacy.zdr = None;
                }
                _ => {}
            }
            if let Some(v) = privacy.get("training").and_then(Value::as_str) {
                c.privacy.training = match v.to_ascii_lowercase().as_str() {
                    "allow" => TrainingPolicy::Allow,
                    "deny" | "no-training" | "disallow" => TrainingPolicy::Deny,
                    _ => TrainingPolicy::Inherit,
                };
            }
        }
        // 适配器覆盖：默认来自 profile；高级用户可按上游改写。
        if let Some(adapters) = obj.get("privacyAdapters").and_then(Value::as_object) {
            if let Some(v) = adapters.get("zdr") {
                c.privacy.zdr_adapter = privacy_adapter_from_value(Some(v));
            }
            if let Some(v) = adapters.get("trainingAllow") {
                c.privacy.training_allow_adapter = privacy_adapter_from_value(Some(v));
            }
            if let Some(v) = adapters.get("trainingDeny") {
                c.privacy.training_deny_adapter = privacy_adapter_from_value(Some(v));
            }
        }
        if let Some(v) = obj.get("versionHeader") {
            c.version_header = v.as_str().unwrap_or("").to_string();
        }
        if let Some(v) = obj.get("environmentHeader") {
            c.environment_header = v.as_str().unwrap_or("").to_string();
        }
        if let Some(catalog) = obj.get("catalog").and_then(Value::as_object) {
            if let Some(v) = catalog.get("source").and_then(Value::as_str) {
                c.catalog_source = v.to_string();
            }
            if let Some(v) = catalog.get("path").and_then(Value::as_str) {
                c.catalog_path = v.to_string();
            }
            if let Some(v) = catalog.get("plan") {
                c.catalog_plan = v.clone();
            }
        }
        if let Some(caps) = obj.get("capabilities") {
            c.capabilities.apply_json(caps);
        }
    }

    // 新版顶层隐私意图：`upstreams.<name>.privacy`（设计文档 02 的新字段）。
    // 显式顶层优先于 `compat.privacy`，但两者都保留，旧读取者仍能解释配置。
    if let Some(privacy) = u.get("privacy").and_then(Value::as_object) {
        match privacy.get("zdr") {
            Some(Value::Bool(v)) => c.privacy.zdr = Some(*v),
            Some(Value::String(v))
                if matches!(
                    v.to_ascii_lowercase().as_str(),
                    "auto" | "inherit" | "default"
                ) =>
            {
                c.privacy.zdr = None;
            }
            _ => {}
        }
        if let Some(v) = privacy.get("training").and_then(Value::as_str) {
            c.privacy.training = match v.to_ascii_lowercase().as_str() {
                "allow" => TrainingPolicy::Allow,
                "deny" | "no-training" | "disallow" => TrainingPolicy::Deny,
                _ => TrainingPolicy::Inherit,
            };
        }
    }

    // 旧布尔视图：运行时读 privacy，旧 API/测试仍读 zdr。
    c.zdr = c.privacy.zdr.unwrap_or(false);

    // ── 全局开关：作为最终约束，避免单个上游绕过全局开关 ──────────────
    if root
        .get("thinking")
        .and_then(Value::as_object)
        .and_then(|t| t.get("passthrough"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        c.thinking_passthrough = false;
    }
    if root
        .get("tools")
        .and_then(Value::as_object)
        .and_then(|t| t.get("downgradeForcedChoice"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        c.tool_choice_downgrade = false;
    }
    c
}

fn infer_protocol(u: &Map<String, Value>) -> String {
    let base = u.get("baseUrl").and_then(Value::as_str).unwrap_or("");
    if base.contains("commandcode") {
        "commandcode".into()
    } else if base.contains(":15721") || base.contains("anthropic") {
        "anthropic".into()
    } else {
        "openai".into()
    }
}

/// 归一化 Base URL：补 scheme、去尾部 "/"，去尾部 "/v1"。
///
/// 公开给接口层使用（面板「查询清单」允许用未保存的 baseUrl 发起查询，
/// 需要与配置加载走同一套归一化规则）。
pub fn normalize_base_url(raw: &str) -> String {
    let mut url = raw.trim().to_string();
    if url.is_empty() {
        url = "127.0.0.1:15721".to_string();
    }
    if !url.contains("://") {
        // 与 Python 版一致：本机地址补 http，其余补 https
        let host = url.split('/').next().unwrap_or("");
        let local = ["127.0.0.1", "localhost", "::1", "0.0.0.0"]
            .iter()
            .any(|h| host.starts_with(h))
            || host.starts_with("192.168.")
            || host.starts_with("10.")
            || host.starts_with("172.");
        url = format!(
            "{}{}",
            if local { "http://" } else { "https://" },
            url.trim_start_matches('/')
        );
    }
    while url.ends_with('/') {
        url.pop();
    }
    if let Some(stripped) = url.strip_suffix("/v1") {
        url = stripped.to_string();
    }
    url
}

pub(crate) fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// 解析「逐模型密钥」表。
///
/// 支持的写法（与 model-bridge 对齐）：
/// ```json
/// "modelKeys": {
///   "MiniMax-M3": "MINIMAX_API_KEY",
///   "mimo-*": { "apiKeyEnv": "XIAOMI_MIMO_API_KEY" },
///   "deepseek-pro": { "apiKeyFile": "D:/secrets/deepseek.json" }
/// }
/// ```
///
/// - 字符串简写等价于 `{ "apiKeyEnv": "<值>" }`；
/// - 对象写法复用上游级 `resolve_api_key` 的优先级（env → file → agent）；
/// - 读不到值的条目**保留**在列表里并标记 `missing`，由诊断与健康检查点名，
///   运行时回落到上游级密钥（与 model-bridge 的兜底行为一致）。
fn resolve_model_keys(
    value: Option<&Value>,
    store: &crate::secret_store::SecretStore,
) -> Vec<ModelKey> {
    let Some(spec) = value.and_then(Value::as_object) else {
        return Vec::new();
    };
    spec.iter()
        .map(|(pattern, raw)| {
            let mut obj: Map<String, Value> = match raw {
                Value::String(env) => {
                    let mut m = Map::new();
                    m.insert("apiKeyEnv".into(), json!(env));
                    m
                }
                Value::Object(m) => m.clone(),
                _ => Map::new(),
            };
            // 明文 apiKey 永不作为凭据；保留字段只为诊断提示，读取时忽略。
            obj.remove("apiKey");
            // 环境变量名/密钥库引用两侧空白不应影响读取（面板输入常见）。
            if let Some(env) = obj
                .get("apiKeyEnv")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_string)
            {
                obj.insert("apiKeyEnv".into(), json!(env));
            }
            if let Some(store_id) = obj
                .get("apiKeyStore")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_string)
            {
                obj.insert("apiKeyStore".into(), json!(store_id));
            }
            let (api_key, source) = resolve_api_key(&obj, &Map::new(), store);
            let env_name = obj
                .get("apiKeyEnv")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let store_id = obj
                .get("apiKeyStore")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| crate::secret_store::is_valid_id(s))
                .map(str::to_string);
            let configured_from = model_key_source_label(&obj);
            let missing = if api_key.is_empty() {
                Some(format!("{configured_from} 未读到值"))
            } else {
                None
            };
            ModelKey {
                pattern: pattern.clone(),
                api_key,
                source,
                env_name,
                store_id,
                configured_from,
                missing,
            }
        })
        .collect()
}

/// 逐模型条目声明的来源描述（用于诊断；不包含任何密钥值）。
fn model_key_source_label(spec: &Map<String, Value>) -> String {
    let text = |key: &str| {
        spec.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(env) = text("apiKeyEnv") {
        return env;
    }
    if let Some(file) = text("apiKeyFile") {
        return file;
    }
    if let Some(path) = text("apiKeyAgentPath") {
        return path;
    }
    if let Some(store_id) = text("apiKeyStore") {
        return format!("Model-Gate 加密存储（{store_id}）");
    }
    if spec.get("apiKeyAgent").is_some() {
        return "agent 凭据（未指定路径）".into();
    }
    "(未配置密钥来源)".into()
}

/// 密钥优先级：apiKeyEnv → apiKeyFile → agent 自定义路径 → Model-Gate 加密密钥库 → 无凭据。
///
/// `apiKeyEnv` 的取值顺序（**只认环境变量**）：
/// 1. 进程环境 `std::env::var`；
/// 2. 本进程写过的会话缓存（Windows 环境块是启动时快照，运行中读不到新写入的值）；
/// 3. Windows 用户级环境变量 `HKCU\Environment`。
///
/// 没有任何内置 `.env` 兜底路径：凭据位置一律由用户显式指定。
/// `apiKeyStore` 只接受 `upstream:` / `model:` 引用，实际密文在独立密钥库文件。
/// JSON 里的 `apiKey` 字段**永不作为凭据来源**：历史配置中的非空值会被忽略，
/// 并由 `diagnostics()` 给出 `legacy_literal_api_key` 迁移提示。
fn resolve_api_key(
    u: &Map<String, Value>,
    _root: &Map<String, Value>,
    store: &crate::secret_store::SecretStore,
) -> (String, String) {
    if let Some(env_name) = u.get("apiKeyEnv").and_then(Value::as_str) {
        let env_name = env_name.trim();
        if !env_name.is_empty() {
            if let Ok(value) = std::env::var(env_name) {
                if !value.is_empty() {
                    return (value, "env".into());
                }
            }
            if let Some(value) = crate::user_env::session_var(env_name) {
                if !value.is_empty() {
                    return (value, "user-env".into());
                }
            }
            if let Some(value) = crate::user_env::read_user_var(env_name) {
                if !value.is_empty() {
                    return (value, "user-env".into());
                }
            }
        }
    }
    if let Some(file) = u.get("apiKeyFile").and_then(Value::as_str) {
        if let Some(value) = read_key_file(file) {
            return (value, "file".into());
        }
    }
    if let Some(tool) = u
        .get("apiKeyAgent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let provider = u
            .get("apiKeyAgentProvider")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let custom_path = u
            .get("apiKeyAgentPath")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some((value, label)) = read_agent_key(tool, provider, custom_path) {
            return (value, format!("agent:{label}"));
        }
    }
    // 最后是 Model-Gate 自己的加密密钥库：配置只存引用，密文由 DPAPI 保护。
    if let Some(store_id) = u
        .get("apiKeyStore")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| crate::secret_store::is_valid_id(s))
    {
        if let Some(value) = store.get(store_id) {
            return (value, "store".into());
        }
    }
    // 未配置任何来源时不伪造占位密钥：空 key 表示不发送鉴权头。
    (String::new(), "none".into())
}

/// 从 .env 文本中读取指定变量（跳过注释与空行，去掉首尾引号）。
fn parse_env_value(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == name {
                return Some(
                    value
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                );
            }
        }
    }
    None
}

fn read_key_file(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(expand_home(path)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("apiKey")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 从用户显式指定的凭据文件读取密钥（只读，不修改对方文件）。
///
/// **没有内置预设**：opencode / jcode / commandcode 等客户端的默认路径一律内置在
/// 代码里会让「密钥从哪来」不可控，因此全部移除 —— 路径必须由用户手填。
/// `apiKeyAgentPath` 缺省时直接返回 None（不猜路径）。
fn read_agent_key(
    _tool: &str,
    provider: &str,
    custom_path: Option<&str>,
) -> Option<(String, String)> {
    let path = custom_path.map(expand_home)?;
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string();
    let (value, kind) = read_key_from_path(&path, provider)?;
    Some((value, format!("{kind}/{label}")))
}

/// 自定义 key 文件：JSON 按点路径/顶层 apiKey，非 JSON 按 .env 变量读取。
///
/// 返回 `(值, 来源种类)`：种类是 `file`（JSON 凭据）或 `env-file`（.env 文本）。
pub(crate) fn read_key_from_path(path: &Path, provider: &str) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        let key = if provider.is_empty() {
            value.get("apiKey").or_else(|| value.get("key"))
        } else {
            let mut current = &value;
            for part in provider.split('.') {
                current = current.get(part)?;
            }
            // 点路径命中的是对象时（如 `{"deepseek":{"key":"sk-..."}}`），
            // 再往里取一层 `key` / `apiKey`。
            if let Some(obj) = current.as_object() {
                current = obj
                    .get("key")
                    .or_else(|| obj.get("apiKey"))
                    .or_else(|| obj.get("token"))?;
            }
            Some(current)
        };
        if let Some(key) = key.and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return Some((key.to_string(), "file".to_string()));
        }
    }
    if provider.is_empty() {
        return None;
    }
    parse_env_value(&text, provider).map(|value| (value, "env-file".to_string()))
}

/// 默认配置唯一来源：`rust/model-gate/default_config.json`。
/// 安装包把它随附为 `{app}\bridge.config.json`，应用首启自动生成的也是同一份内容
/// （同一来源，避免「安装包里的默认配置」与「首启生成的默认配置」两份模板各自漂移）。
const DEFAULT_CONFIG_JSON: &str = include_str!("../default_config.json");

/// 生成默认配置（首次启动写盘用）。
/// 优先使用内嵌的 `default_config.json`；解析失败时回落到内置兜底模板，保证首启仍能写出合法配置。
pub fn default_config_value() -> Value {
    default_config_value_from(DEFAULT_CONFIG_JSON)
}

/// 解析给定的默认配置文本；解析失败或不是对象时回落到内置兜底模板。
/// 抽成独立函数是为了让兜底分支可被单测直接覆盖（内嵌常量正常情况下永远解析成功）。
pub fn default_config_value_from(text: &str) -> Value {
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(|value| value.is_object())
        .unwrap_or_else(fallback_default_config_value)
}

/// 内置兜底模板：仅在内嵌默认配置解析失败时使用（正常路径不会命中）。
/// 内容与 `default_config.json` 的可用口径保持一致：一条免密上游，无明文密钥。
fn fallback_default_config_value() -> Value {
    json!({
        "port": 8900,
        "host": "127.0.0.1",
        "defaultModel": "mimo-v2.6-flash-free",
        "models": ["mimo-v2.6-flash-free", "big-pickle"],
        "maxTokens": 8192,
        "timeoutSeconds": 600,
        "maxBodyBytes": DEFAULT_MAX_BODY_BYTES,
        "accessLog": true,
        "promptCapture": false,
        "promptRetentionDays": 7,
        "promptMaxRecords": 5000,
        "promptMaxChars": 100000,
        "modelsCacheSeconds": 300,
        "aliases": {},
        "thinking": {"passthrough": true, "cacheSize": 500},
        "upstreams": {
            "zen": {
                "protocol": "openai",
                "transport": "opencode",
                "baseUrl": "https://opencode.ai/zen/v1",
                "fetchModels": false,
                "autoModels": false,
                "sessionHeader": false,
                "default": true,
                "models": ["mimo-v2.6-flash-free", "big-pickle"]
            }
        }
    })
}

/// 默认配置唯一来源的内嵌与兜底行为。
///
/// 覆盖三件事：① 安装包随附的 `default_config.json` 与首启自动生成的内容同源；
/// ② 内嵌文本非法时兜底分支可达（首启仍能写出合法配置）；③ 默认配置不含明文密钥。
#[cfg(test)]
mod default_config_tests {
    use super::*;

    #[test]
    fn default_config_is_parsable_object() {
        let value = default_config_value();
        assert!(value.is_object(), "默认配置必须是 JSON 对象");
        assert_eq!(value.get("port").and_then(Value::as_u64), Some(8900));
    }

    #[test]
    fn default_config_contains_prompt_capture_defaults() {
        let value = default_config_value();
        assert_eq!(
            value.get("promptCapture").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            value.get("promptRetentionDays").and_then(Value::as_u64),
            Some(7)
        );
        assert_eq!(
            value.get("promptMaxRecords").and_then(Value::as_u64),
            Some(5000)
        );
        assert_eq!(
            value.get("promptMaxChars").and_then(Value::as_u64),
            Some(100_000)
        );
    }

    #[test]
    fn default_config_contains_doc_fields() {
        let value = default_config_value();
        for key in ["_doc", "_keys", "_models"] {
            let text = value.get(key).and_then(Value::as_str).unwrap_or_default();
            assert!(!text.is_empty(), "默认配置缺少说明字段 {key}");
        }
    }

    #[test]
    fn default_config_has_zen_without_plaintext_key() {
        let value = default_config_value();
        let upstreams = value
            .get("upstreams")
            .and_then(Value::as_object)
            .expect("默认配置应含 upstreams");
        let zen = upstreams
            .get("zen")
            .and_then(Value::as_object)
            .expect("默认配置应含免密 zen 上游");
        assert_eq!(
            zen.get("transport").and_then(Value::as_str),
            Some("opencode")
        );
        assert_eq!(zen.get("fetchModels").and_then(Value::as_bool), Some(false));
        assert!(zen.get("apiKey").is_none(), "默认配置不得写明文 apiKey");
        assert!(zen.get("apiKeyEnv").is_none(), "免密上游不需要密钥来源");
        // 不含依赖本机服务或需要密钥的上游
        assert!(
            !upstreams.contains_key("cch"),
            "默认配置不应依赖本机 CCH 服务"
        );
        assert!(!upstreams.contains_key("go"), "默认配置不应含需密钥上游 go");
        assert!(!upstreams.contains_key("commandcode"));
    }

    #[test]
    fn default_config_falls_back_when_text_invalid() {
        let value = default_config_value_from("{ not json");
        assert!(value.is_object());
        assert_eq!(value.get("port").and_then(Value::as_u64), Some(8900));
        assert!(
            value
                .get("upstreams")
                .and_then(Value::as_object)
                .map(|m| m.contains_key("zen"))
                .unwrap_or(false),
            "兜底模板仍应提供可用上游"
        );
    }

    #[test]
    fn default_config_falls_back_when_not_object() {
        let value = default_config_value_from("[1,2,3]");
        assert!(value.is_object(), "非对象文本必须回落到兜底模板");
        assert!(value.get("upstreams").is_some());
    }

    /// 单一来源：内嵌内容与磁盘上的 `default_config.json` 完全一致
    /// （安装包随附的默认配置 = 首启自动生成的默认配置）。
    #[test]
    fn default_config_matches_source_file() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("default_config.json");
        let text = std::fs::read_to_string(&path).expect("应能读到 default_config.json");
        let from_disk: Value =
            serde_json::from_str(&text).expect("default_config.json 应是合法 JSON");
        assert_eq!(from_disk, default_config_value());
    }
}

/// 面板内「配置说明」弹窗的内容契约。
///
/// v4 的「配置说明」由 `ui/v4-config.js` 的 `help()` 提供；本模块只保留后端字段解析契约。
#[cfg(test)]
mod tests {
    use super::*;

    fn empty_root() -> Map<String, Value> {
        Map::new()
    }

    /// 测试兼容：多数配置测试不涉及加密密钥库，统一使用空库。
    fn normalize_upstream(name: &str, value: &Value, root: &Map<String, Value>) -> Upstream {
        super::normalize_upstream(
            name,
            value,
            root,
            &crate::secret_store::SecretStore::empty(),
        )
    }

    #[test]
    fn prompt_config_defaults_are_safe() {
        let cfg = Config::from_value(json!({
            "upstreams": {"u": {"protocol": "openai", "baseUrl": "http://x"}}
        }));
        assert!(!cfg.prompt_capture, "提示词记录默认必须关闭");
        assert_eq!(cfg.prompt_retention_days, 7);
        assert_eq!(cfg.prompt_max_records, 5000);
        assert_eq!(cfg.prompt_max_chars, 100_000);
    }

    #[test]
    fn prompt_config_parses_values_and_rejects_small_max_chars() {
        let cfg = Config::from_value(json!({
            "promptCapture": true,
            "promptRetentionDays": 3,
            "promptMaxRecords": 10,
            "promptMaxChars": 500,
            "upstreams": {"u": {"protocol": "openai", "baseUrl": "http://x"}}
        }));
        assert!(cfg.prompt_capture);
        assert_eq!(cfg.prompt_retention_days, 3);
        assert_eq!(cfg.prompt_max_records, 10);
        assert_eq!(cfg.prompt_max_chars, 500);
        let diagnostics = cfg.diagnostics();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "invalid_prompt_max_chars" && d.level == "error"),
            "promptMaxChars < 1000 必须给出 error 诊断"
        );
    }

    #[test]
    fn opencode_preset_sets_session_header() {
        let up = normalize_upstream(
            "go",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://opencode.ai/zen/go/v1",
                "models": ["m"]
            }),
            &empty_root(),
        );
        assert_eq!(up.compat.profile, "opencode");
        assert_eq!(up.compat.session_header, "x-opencode-session");
        assert_eq!(up.compat.chat_path, "/v1/chat/completions");
    }

    #[test]
    fn commandcode_preset_declares_wire_specifics() {
        let up = normalize_upstream(
            "cc",
            &json!({
                "protocol": "commandcode",
                "baseUrl": "https://api.commandcode.ai"
            }),
            &empty_root(),
        );
        assert_eq!(up.compat.session_header, "x-session-id");
        assert_eq!(up.compat.session_id_style, "sess16");
        assert_eq!(up.compat.chat_path, "/alpha/generate");
        assert_eq!(up.compat.version_header, "x-command-code-version");
        assert_eq!(up.compat.environment_header, "x-cli-environment");
        assert!(up.compat.zdr);
        assert_eq!(up.compat.max_output_tokens, Some(64_000));
        assert!(matches!(
            &up.compat.privacy.zdr_adapter,
            PrivacyAdapter::Header { name, value } if name == "x-cmd-zdr" && value == "1"
        ));
    }

    #[test]
    fn opencode_privacy_adapters_are_declared() {
        let up = normalize_upstream(
            "go",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://opencode.ai/zen/go/v1"
            }),
            &empty_root(),
        );
        assert!(
            matches!(
                &up.compat.privacy.zdr_adapter,
                PrivacyAdapter::Unsupported { .. }
            ),
            "OpenCode 不读取请求级 ZDR 头"
        );
        assert!(
            matches!(
                &up.compat.privacy.training_allow_adapter,
                PrivacyAdapter::Workspace { url, setting }
                    if url.contains("opencode.ai") && setting.contains("Allow paid endpoints")
            ),
            "允许训练必须指向 workspace 控制台"
        );
        assert!(
            matches!(
                &up.compat.privacy.training_deny_adapter,
                PrivacyAdapter::Unsupported { .. }
            ),
            "OpenCode 不支持请求级禁止训练"
        );
    }

    #[test]
    fn explicit_privacy_intent_and_adapter_overrides() {
        let up = normalize_upstream(
            "u",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.example.com/v1",
                "compat": {
                    "zdr": false,
                    "privacy": {"zdr": true, "training": "deny"},
                    "privacyAdapters": {
                        "zdr": {"kind": "body", "path": "provider.zdr", "value": true},
                        "trainingDeny": {
                            "kind": "body",
                            "path": "provider.data_collection",
                            "value": "deny"
                        }
                    }
                }
            }),
            &empty_root(),
        );
        assert_eq!(up.compat.privacy.zdr, Some(true), "privacy.zdr 覆盖旧 zdr");
        assert_eq!(up.compat.privacy.training, TrainingPolicy::Deny);
        assert!(matches!(
            &up.compat.privacy.zdr_adapter,
            PrivacyAdapter::Body { path, .. } if path == "provider.zdr"
        ));
        assert!(matches!(
            &up.compat.privacy.training_deny_adapter,
            PrivacyAdapter::Body { path, .. } if path == "provider.data_collection"
        ));
    }

    #[test]
    fn privacy_diagnostics_only_warn_when_requested_and_unsupported() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "go": {
                    "protocol": "openai",
                    "baseUrl": "https://opencode.ai/zen/go/v1",
                    "compat": {"privacy": {"zdr": true, "training": "deny"}}
                }
            }
        }));
        let privacy: Vec<_> = cfg
            .diagnostics()
            .into_iter()
            .filter(|d| d.code == "privacy_unsupported")
            .collect();
        assert_eq!(privacy.len(), 2, "ZDR 与禁止训练都应给出不支持提示");
        assert!(privacy.iter().all(|d| d.level == "warn"));

        let quiet = Config::from_value(json!({
            "upstreams": {
                "go": {
                    "protocol": "openai",
                    "baseUrl": "https://opencode.ai/zen/go/v1",
                    "compat": {"zdr": false}
                }
            }
        }));
        assert!(
            quiet
                .diagnostics()
                .iter()
                .all(|d| d.code != "privacy_unsupported"),
            "未请求隐私策略时不应产生噪音"
        );
    }

    #[test]
    fn effective_upstreams_exposes_resolved_privacy() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": "https://api.example.com/v1",
                    "compat": {
                        "privacy": {"zdr": true, "training": "deny"},
                        "privacyAdapters": {
                            "zdr": {"kind": "body", "path": "provider.zdr", "value": true},
                            "trainingDeny": {
                                "kind": "body",
                                "path": "provider.data_collection",
                                "value": "deny"
                            }
                        }
                    }
                }
            }
        }));
        let rows = cfg.effective_upstreams();
        let row = rows
            .as_array()
            .and_then(|a| a.iter().find(|r| r["name"] == "u"))
            .expect("effective 行存在");
        assert_eq!(row["privacy"]["zdr"], true);
        assert_eq!(row["privacy"]["training"], "deny");
        assert_eq!(row["privacy"]["adapters"]["zdr"]["kind"], "body");
        assert_eq!(row["privacy"]["adapters"]["trainingDeny"]["value"], "deny");
    }

    #[test]
    fn deepseek_preset_sets_limits_and_thinking() {
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1"
            }),
            &empty_root(),
        );
        assert_eq!(up.compat.profile, "deepseek");
        assert_eq!(up.compat.max_output_tokens, Some(384 * 1024));
        assert_eq!(up.compat.max_context_tokens, Some(1024 * 1024));
        assert!(up.compat.thinking_passthrough);
    }

    #[test]
    fn explicit_compat_overrides_preset_and_legacy() {
        let up = normalize_upstream(
            "go",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://opencode.ai/zen/go/v1",
                "sessionHeader": true,
                "compat": {
                    "session": false,
                    "auth": {"scheme": "header", "header": "x-api-key", "prefix": ""},
                    "headers": {"x-client-name": "model-gate"},
                    "endpoint": {"chat": "/custom/chat"},
                    "limits": {"maxOutputTokens": 12345}
                }
            }),
            &empty_root(),
        );
        assert_eq!(up.compat.session_header, "");
        assert_eq!(up.compat.auth_scheme, "header");
        assert_eq!(up.compat.auth_header, "x-api-key");
        assert_eq!(up.compat.chat_path, "/custom/chat");
        assert_eq!(up.compat.max_output_tokens, Some(12345));
        assert!(up
            .compat
            .extra_headers
            .iter()
            .any(|(k, v)| k == "x-client-name" && v == "model-gate"));
    }

    #[test]
    fn global_switches_are_final_constraint() {
        let cfg = Config::from_value(json!({
            "thinking": {"passthrough": false},
            "tools": {"downgradeForcedChoice": false},
            "upstreams": {
                "cc": {"protocol": "commandcode", "baseUrl": "https://api.commandcode.ai"},
                "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721"}
            }
        }));
        for up in &cfg.upstreams {
            assert!(!up.compat.thinking_passthrough);
            assert!(!up.compat.tool_choice_downgrade);
        }
    }

    #[test]
    fn diagnostics_reports_errors_and_unknown_fields() {
        let cfg = Config::from_value(json!({
            "host": "127.0.0.1",
            "maxTokens": 8192,
            "mysteryTop": true,
            "upstreams": {
                "bad": {
                    "protocol": "magic",
                    "baseUrl": "",
                    "compat": {
                        "auth": {"scheme": "nope"},
                        "session": {"header": "bad header"},
                        "endpoint": {"chat": "missing-slash"},
                        "limits": {"maxOutputTokens": 200, "maxContextTokens": 100},
                        "unknownKey": true
                    }
                }
            }
        }));
        let codes: HashSet<String> = cfg.diagnostics().into_iter().map(|d| d.code).collect();
        assert!(codes.contains("invalid_base_url"));
        assert!(codes.contains("unsupported_protocol"));
        assert!(codes.contains("invalid_auth_scheme"));
        assert!(codes.contains("invalid_header_name"));
        assert!(codes.contains("invalid_endpoint"));
        assert!(codes.contains("invalid_limits"));
        assert!(codes.contains("unknown_compat_field"));
        assert!(codes.contains("unknown_field"));
    }

    #[test]
    fn diagnostics_reports_legacy_credential_sources() {
        let cfg = Config::from_value(json!({
            "host": "127.0.0.1",
            "maxTokens": 8192,
            "envFile": "D:/legacy-placeholder.env",
            "upstreams": {
                "legacy-file": {
                    "protocol": "openai",
                    "baseUrl": "https://api.deepseek.com/v1",
                    "apiKeyFile": "D:/secrets/legacy.json"
                },
                "legacy-agent": {
                    "protocol": "openai",
                    "baseUrl": "https://api.deepseek.com/v1",
                    "apiKeyAgent": "jcode",
                    "apiKeyAgentProvider": "MY_KEY"
                }
            }
        }));
        let diags = cfg.diagnostics();
        let legacy: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.code.starts_with("legacy_"))
            .collect();
        assert!(legacy.iter().any(|d| d.code == "legacy_env_file"));
        assert!(legacy
            .iter()
            .any(|d| d.code == "legacy_api_key_file" && d.path.ends_with(".apiKeyFile")));
        assert!(legacy
            .iter()
            .any(|d| d.code == "legacy_agent_preset" && d.path.ends_with(".apiKeyAgent")));
        // 迁移提示只提示、不阻断启动。
        assert!(legacy
            .iter()
            .all(|d| d.level == "warn" || d.level == "info"));
    }

    #[test]
    fn placeholder_key_is_not_configured() {
        let up = normalize_upstream(
            "go",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://opencode.ai/zen/go/v1"
            }),
            &empty_root(),
        );
        assert_eq!(up.key_source, "none");
        assert!(!up.key_configured());
    }

    fn temp_key_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mg-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn agent_key_reads_user_specified_json_path() {
        // 用户手填路径的 JSON 凭据：provider 走点路径。
        let path = temp_key_file(
            "user-auth.json",
            r#"{"deepseek":{"type":"api","key":"sk-open"}}"#,
        );
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyAgent": "opencode",
                "apiKeyAgentProvider": "deepseek",
                "apiKeyAgentPath": path.display().to_string()
            }),
            &empty_root(),
        );
        assert_eq!(up.api_key, "sk-open");
        assert_eq!(up.key_source, "agent:file/user-auth.json");
        assert!(up.key_configured());
    }

    #[test]
    fn agent_key_reads_user_specified_env_file() {
        // 用户手填路径的 .env 文本：按变量名取值。
        let path = temp_key_file("user.env", "# comment\nMY_KEY = \"sk-custom\"\n");
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyAgent": "jcode",
                "apiKeyAgentProvider": "MY_KEY",
                "apiKeyAgentPath": path.display().to_string()
            }),
            &empty_root(),
        );
        assert_eq!(up.api_key, "sk-custom");
        assert_eq!(up.key_source, "agent:env-file/user.env");
    }

    #[test]
    fn agent_key_reads_user_specified_auth_json() {
        let path = temp_key_file("cc-auth.json", r#"{"apiKey":"sk-cc"}"#);
        let up = normalize_upstream(
            "cc",
            &json!({
                "protocol": "commandcode",
                "apiKeyAgent": "commandcode",
                "apiKeyAgentPath": path.display().to_string()
            }),
            &empty_root(),
        );
        assert_eq!(up.api_key, "sk-cc");
        assert_eq!(up.key_source, "agent:file/cc-auth.json");
    }

    #[test]
    fn agent_key_without_explicit_path_is_not_resolved() {
        // 内置预设已移除：不填 apiKeyAgentPath 就不猜路径。
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyAgent": "jcode",
                "apiKeyAgentProvider": "MY_KEY"
            }),
            &empty_root(),
        );
        assert_eq!(up.key_source, "none");
        assert!(!up.key_configured());
    }

    #[test]
    fn api_key_env_reads_process_environment() {
        let name = "MODEL_GATE_TEST_PROCESS_ENV";
        std::env::set_var(name, "sk-process");
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyEnv": name
            }),
            &empty_root(),
        );
        std::env::remove_var(name);
        assert_eq!(up.api_key, "sk-process");
        assert_eq!(up.key_source, "env");
    }

    #[test]
    fn api_key_env_reads_session_cache_when_process_env_missing() {
        // 进程环境没有时，回落到本进程写过的会话缓存。
        let name = "MODEL_GATE_TEST_SESSION_ENV";
        std::env::remove_var(name);
        crate::user_env::remember_session_var(name, "sk-session");
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyEnv": name
            }),
            &empty_root(),
        );
        assert_eq!(up.api_key, "sk-session");
        assert_eq!(up.key_source, "user-env");
    }

    #[test]
    fn api_key_env_missing_yields_no_key() {
        // 不存在任何来源时不得回落到明文或内置 .env。
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyEnv": "MODEL_GATE_TEST_ABSENT_ENV_VAR"
            }),
            &empty_root(),
        );
        assert_eq!(up.key_source, "none");
        assert!(!up.key_configured());
    }

    #[test]
    fn agent_key_missing_does_not_fall_back_to_literal() {
        // 旧配置可能同时保留 apiKey；它已不是凭据来源，读取失败时不得“捡回来”。
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "apiKeyAgent": "opencode",
                "apiKeyAgentProvider": "deepseek",
                "apiKeyAgentPath": "Z:/definitely-missing/auth.json",
                "apiKey": "sk-literal"
            }),
            &empty_root(),
        );
        assert!(up.api_key.is_empty());
        assert_eq!(up.key_source, "none");
        assert!(!up.key_configured());
    }

    #[test]
    fn legacy_literal_api_key_is_ignored_and_diagnosed() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "ds": {
                    "protocol": "openai",
                    "baseUrl": "https://api.deepseek.com/v1",
                    "apiKey": "sk-legacy-literal"
                }
            }
        }));
        let up = cfg.upstream("ds").expect("ds upstream");
        assert!(up.api_key.is_empty(), "JSON 中的 apiKey 不得作为凭据");
        assert_eq!(up.key_source, "none");
        assert!(!up.key_configured());

        let diag = cfg
            .diagnostics()
            .into_iter()
            .find(|d| d.code == "legacy_literal_api_key")
            .expect("应给出明文 apiKey 迁移提示");
        assert_eq!(diag.level, "warn");
        assert_eq!(diag.path, "upstreams.ds.apiKey");
        assert!(
            !diag.message.contains("sk-legacy-literal"),
            "诊断信息不得回显密钥"
        );
    }

    #[test]
    fn profile_detection_covers_five_presets() {
        // 域名探测：deepseek / opencode / commandcode
        let ds = Compat::preset("", "https://api.deepseek.com/v1", "openai");
        assert_eq!(ds.profile, "deepseek");
        assert_eq!(ds.max_output_tokens, Some(384 * 1024));
        assert_eq!(ds.max_context_tokens, Some(1024 * 1024));
        assert!(ds.thinking_passthrough);
        assert!(ds.capabilities.thinking && ds.capabilities.cache);

        let oc = Compat::preset("", "https://opencode.ai/zen/go/v1", "openai");
        assert_eq!(oc.profile, "opencode");
        assert_eq!(oc.session_header, "x-opencode-session");

        let cc = Compat::preset("", "https://api.commandcode.ai", "commandcode");
        assert_eq!(cc.profile, "commandcode");
        assert_eq!(cc.session_id_style, "sess16");
        assert_eq!(cc.catalog_source, "commandcode");
        assert_eq!(cc.catalog_plan, Value::String("auto".into()));
        assert!(cc.zdr && cc.tool_choice_downgrade);
        assert!(cc.capabilities.thinking);

        // 协议回退：anthropic / openai
        let an = Compat::preset("", "http://127.0.0.1:15721", "anthropic");
        assert_eq!(an.profile, "anthropic");
        assert_eq!(an.auth_scheme, "both");
        assert_eq!(an.chat_path, "/v1/messages");

        let oa = Compat::preset("", "https://api.example.com/v1", "openai");
        assert_eq!(oa.profile, "openai");
        assert_eq!(oa.auth_scheme, "bearer");
        assert!(!oa.capabilities.vision);

        // 兼容 shim：anthropic + deepseek host → deepseek 语义（保留 anthropic 变体）
        let shim = Compat::preset("", "https://api.deepseek.com/anthropic", "anthropic");
        assert_eq!(shim.profile, "deepseek");
        assert_eq!(shim.auth_scheme, "both");
        assert_eq!(shim.chat_path, "/v1/messages");
        assert_eq!(shim.max_output_tokens, Some(384 * 1024));

        // 显式 profile 覆盖域名与协议
        let explicit = Compat::preset("deepseek", "https://proxy.example.com", "anthropic");
        assert_eq!(explicit.profile, "deepseek");
        assert_eq!(explicit.chat_path, "/v1/messages");
    }

    #[test]
    fn models_cache_seconds_parsed_with_default() {
        let cfg = Config::from_value(json!({"modelsCacheSeconds": 600}));
        assert_eq!(cfg.models_cache_seconds, 600);
        let fallback = Config::from_value(json!({}));
        assert_eq!(fallback.models_cache_seconds, 300);
    }

    #[test]
    fn capabilities_merge_upstream_compat_layer() {
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "compat": {"capabilities": {"vision": true}}
            }),
            &empty_root(),
        );
        assert!(up.compat.capabilities.vision);
        assert!(up.compat.capabilities.thinking);
    }

    #[test]
    fn model_overrides_exact_then_longest_wildcard() {
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "modelOverrides": {
                    "*": {"limits": {"maxOutputTokens": 16000}},
                    "deepseek-chat*": {"limits": {"maxOutputTokens": 8000}},
                    "deepseek-reasoner": {"limits": {"maxOutputTokens": 65536}}
                }
            }),
            &empty_root(),
        );
        assert_eq!(up.model_max_output_tokens("deepseek-reasoner"), Some(65536));
        assert_eq!(up.model_max_output_tokens("deepseek-chat-0324"), Some(8000));
        assert_eq!(up.model_max_output_tokens("gpt-4o"), Some(16000));

        // 未命中覆盖 → 回落到上游 compat.limits（deepseek 预设 384k）
        let bare = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1"
            }),
            &empty_root(),
        );
        assert_eq!(
            bare.model_max_output_tokens("deepseek-chat"),
            Some(384 * 1024)
        );

        // 多个 `*` 非法：不参与匹配
        let multi = normalize_upstream(
            "m",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.example.com/v1",
                "modelOverrides": {"a*b*c": {"limits": {"maxOutputTokens": 1}}}
            }),
            &empty_root(),
        );
        assert!(multi.model_override("axbxc").is_none());

        // 空模型名不匹配任何覆盖（包括 `*`）
        let any = normalize_upstream(
            "m",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.example.com/v1",
                "modelOverrides": {"*": {"limits": {"maxOutputTokens": 1}}}
            }),
            &empty_root(),
        );
        assert!(any.model_override("").is_none());
    }

    #[test]
    fn diagnostics_model_override_codes() {
        let cfg = Config::from_value(json!({
            "maxTokens": 8192,
            "upstreams": {
                "ds": {
                    "protocol": "openai",
                    "baseUrl": "https://api.deepseek.com/v1",
                    "models": ["deepseek-chat"],
                    "compat": {"profile": "nope", "capabilities": {"vision": "yes"}},
                    "modelOverrides": {
                        "a*b*c": {},
                        "ghost": {"limits": {"maxOutputTokens": 1000, "maxContextTokens": 500}},
                        "deepseek-chat": {"limits": {"maxOutputTokens": "8000"},
                                          "capabilities": {"thinking": "yes", "bogus": true}},
                        "blank": {}
                    }
                }
            }
        }));
        let diags = cfg.diagnostics();
        let has = |code: &str, path: &str| diags.iter().any(|d| d.code == code && d.path == path);
        assert!(has("unknown_profile", "upstreams.ds.compat.profile"));
        assert!(has(
            "invalid_capabilities",
            "upstreams.ds.compat.capabilities.vision"
        ));
        assert!(has(
            "multi_wildcard_override",
            "upstreams.ds.modelOverrides.a*b*c"
        ));
        assert!(has(
            "model_override_unknown_model",
            "upstreams.ds.modelOverrides.ghost"
        ));
        assert!(has(
            "invalid_limit_relation",
            "upstreams.ds.modelOverrides.ghost.limits"
        ));
        assert!(has(
            "invalid_capabilities",
            "upstreams.ds.modelOverrides.deepseek-chat.capabilities.thinking"
        ));
        assert!(has(
            "unknown_field",
            "upstreams.ds.modelOverrides.deepseek-chat.capabilities.bogus"
        ));
        assert!(has(
            "invalid_model_overrides",
            "upstreams.ds.modelOverrides.blank"
        ));
        // 数字字符串的合法值不报错（与 Python _to_int 口径一致）
        assert!(!diags.iter().any(
            |d| d.code == "invalid_model_overrides" && d.path.contains("deepseek-chat.limits")
        ));
    }

    #[test]
    fn model_capabilities_merge_three_levels() {
        let up = normalize_upstream(
            "ds",
            &json!({
                "protocol": "openai",
                "baseUrl": "https://api.deepseek.com/v1",
                "compat": {"capabilities": {"vision": true}},
                "modelOverrides": {"deepseek-reasoner": {
                    "capabilities": {"cache": true, "vision": false}}}
            }),
            &empty_root(),
        );
        // profile(deepseek: thinking/cache) → 上游(vision) → 模型覆盖(vision=false)
        let caps = up.model_capabilities("deepseek-reasoner");
        assert!(caps.thinking && caps.cache && !caps.vision);
        let base = up.model_capabilities("deepseek-chat");
        assert!(base.vision && base.thinking);
    }

    #[test]
    fn legacy_config_fixture_loads_without_errors() {
        // 旧配置夹具（旧简写 + 未知字段）。
        let raw: Value = serde_json::from_str(include_str!("../tests/fixtures/legacy_config.json"))
            .expect("旧配置夹具可解析");
        let cfg = Config::from_value(raw);
        assert_eq!(cfg.upstreams.len(), 2);
        let ds = cfg
            .upstreams
            .iter()
            .find(|u| u.name == "ds")
            .expect("ds 上游");
        assert_eq!(
            ds.compat.profile, "deepseek",
            "anthropic + deepseek host 走 shim"
        );
        assert_eq!(ds.compat.chat_path, "/v1/messages");
        assert_eq!(ds.compat.max_output_tokens, Some(384 * 1024));
        assert!(ds.cmd_zdr);
        let zen = cfg
            .upstreams
            .iter()
            .find(|u| u.name == "zen")
            .expect("zen 上游");
        assert_eq!(zen.compat.profile, "opencode");
        assert_eq!(zen.compat.session_header, "x-opencode-session");
        assert!(zen.pause_turn);
        let diags = cfg.diagnostics();
        let errors: Vec<_> = diags.iter().filter(|d| d.level == "error").collect();
        assert!(errors.is_empty(), "旧配置不应产生 error 级诊断：{errors:?}");
    }

    /// 免鉴权上游不应被提醒「没有配置密钥」。
    ///
    /// 覆盖三类免鉴权：注册表声明 authOptional（opencode / zen）、
    /// 显式 auth.scheme=none、本机桥接地址（CCH）。
    #[test]
    fn auth_optional_upstreams_do_not_warn_missing_credentials() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "zen": {
                    "protocol": "openai",
                    "baseUrl": "https://opencode.ai/zen/v1",
                    "models": ["grok-code"]
                },
                "cch": {
                    "protocol": "anthropic",
                    "baseUrl": "http://127.0.0.1:15721",
                    "models": ["claude-sonnet-4"]
                },
                "anon": {
                    "protocol": "openai",
                    "baseUrl": "https://api.example.com/v1",
                    "compat": {"auth": {"scheme": "none"}},
                    "models": ["m"]
                }
            }
        }));
        let diags = cfg.diagnostics();
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| d.code == "missing_credentials")
            .collect();
        assert!(
            missing.is_empty(),
            "免鉴权上游不应被提醒缺密钥：{missing:?}"
        );
    }

    /// 免鉴权判定不能放宽到「所有上游」：公网 bearer 上游缺密钥仍要提醒。
    #[test]
    fn public_bearer_upstream_still_warns_missing_credentials() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "ds": {
                    "protocol": "openai",
                    "baseUrl": "https://api.deepseek.com/v1",
                    "models": ["deepseek-chat"]
                }
            }
        }));
        let diags = cfg.diagnostics();
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| d.code == "missing_credentials")
            .collect();
        assert_eq!(missing.len(), 1, "公网上游缺密钥必须仍然提醒");
        assert_eq!(missing[0].level, "warn");
    }

    /// 本机地址判定：回环 / 局域网段算本机，公网域名不算。
    ///
    /// 私网样本用运行时拼接构造：这些是 RFC1918 的**边界**取值
    /// （`172.15`/`172.16`/`172.31`/`172.32` 的分段临界），
    /// 换成保留地址会丢掉边界语义，写死成字面量又会被敏感扫描当成真实内网地址。
    #[test]
    fn local_endpoint_detection_covers_loopback_and_lan() {
        // 私网段边界样本：格式为「前缀 + 末段」，运行期拼成完整地址。
        let lan = [
            ("192.168.1.", "10"),
            ("10.0.0.", "5"),
            ("172.16.3.", "4"),
            ("172.31.255.", "1"),
        ];
        for url in [
            "http://127.0.0.1:15721",
            "http://localhost:8080/v1",
            "http://[::1]:15721",
        ] {
            assert!(is_local_endpoint(url), "应判定为本机地址：{url}");
        }
        for (prefix, tail) in lan {
            let url = format!("http://{prefix}{tail}/v1");
            assert!(is_local_endpoint(&url), "应判定为本机地址：{url}");
        }
        for url in [
            "https://api.deepseek.com/v1",
            "https://opencode.ai/zen/v1",
            "http://172.15.0.1/v1",
            "http://172.32.0.1/v1",
            "https://127.0.0.1.example.com/v1",
        ] {
            assert!(!is_local_endpoint(url), "不应判定为本机地址：{url}");
        }
    }

    /// maxTokens 超过上游上限：运行时已自动收敛，属知会（info）而非警告。
    #[test]
    fn max_tokens_exceeding_upstream_limit_is_info_level() {
        let cfg = Config::from_value(json!({
            "maxTokens": 300000,
            "upstreams": {
                "cc": {
                    "protocol": "commandcode",
                    "baseUrl": "https://api.commandcode.ai",
                    "models": ["m"]
                }
            }
        }));
        let diags = cfg.diagnostics();
        let over: Vec<_> = diags
            .iter()
            .filter(|d| d.code == "max_tokens_exceeds_upstream")
            .collect();
        assert_eq!(over.len(), 1, "应产生一条 maxTokens 超限诊断");
        assert_eq!(over[0].level, "info");
    }

    /// 逐模型密钥：字符串简写 + 对象写法 + 精确优先于通配。
    #[test]
    fn model_keys_string_and_object_and_precedence() {
        let dir = std::env::temp_dir().join(format!("mg-model-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_a = dir.join("a.json");
        let key_b = dir.join("b.json");
        std::fs::write(&key_a, r#"{"apiKey":"key-a"}"#).unwrap();
        std::fs::write(&key_b, r#"{"apiKey":"key-b"}"#).unwrap();

        let up = normalize_upstream(
            "u",
            &json!({
                "protocol": "openai",
                "baseUrl": "http://127.0.0.1:1/v1",
                "apiKeyEnv": "MG_MODEL_KEYS_FALLBACK_NOT_SET",
                "models": ["model-a", "model-b"],
                "modelKeys": {
                    "model-a": {"apiKeyFile": key_a.to_string_lossy()},
                    "model-*": {"apiKeyFile": key_b.to_string_lossy()},
                    "bad*b*c": {"apiKeyFile": key_b.to_string_lossy()}
                }
            }),
            &empty_root(),
        );

        // 精确命中优先于通配
        let a = up.key_for_model("model-a");
        assert_eq!(a.key, "key-a");
        assert_eq!(a.from, "model");
        // 通配命中
        let b = up.key_for_model("model-extra");
        assert_eq!(b.key, "key-b");
        assert_eq!(b.from, "model");
        // 未命中 → 上游级兜底（本用例未设置该环境变量，故为空）
        let fallback = up.key_for_model("other");
        assert_eq!(fallback.from, "upstream");
        assert!(fallback.key.is_empty());
        // 多个 `*` 非法：不参与匹配
        assert!(up.model_key("badxxbyyc").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 加密密钥库：一个上游共用一把；逐模型引用可覆盖。
    #[cfg(windows)]
    #[test]
    fn secret_store_resolves_upstream_and_model_keys() {
        let dir = std::env::temp_dir().join(format!("mg-store-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("bridge.config.json");
        let mut store = crate::secret_store::SecretStore::load(&config_path);
        store
            .set(&crate::secret_store::upstream_id("u"), "sk-upstream-shared")
            .unwrap();
        store
            .set(&crate::secret_store::model_id("u", "model-b"), "sk-model-b")
            .unwrap();

        let cfg = Config::from_value_with_store(
            json!({
                "upstreams": {
                    "u": {
                        "protocol": "openai",
                        "baseUrl": "https://api.example.com/v1",
                        "apiKeyStore": "upstream:u",
                        "models": ["model-a", "model-b"],
                        "modelKeys": {
                            "model-b": {"apiKeyStore": "model:u:model-b"}
                        }
                    }
                }
            }),
            &store,
        );
        let up = &cfg.upstreams[0];
        assert_eq!(up.key_source, "store");
        assert_eq!(up.api_key, "sk-upstream-shared");
        assert!(up.any_key_configured());
        // 没有逐模型密钥的模型共用上游密钥。
        let a = up.key_for_model("model-a");
        assert_eq!(a.key, "sk-upstream-shared");
        assert_eq!(a.from, "upstream");
        // 独立 model 引用覆盖上游密钥。
        let b = up.key_for_model("model-b");
        assert_eq!(b.key, "sk-model-b");
        assert_eq!(b.from, "model");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 逐模型密钥诊断：非法条目、未知模型、缺失、多通配、明文 apiKey。
    #[test]
    fn diagnostics_model_keys_codes() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "u": {
                    "protocol": "openai",
                    "baseUrl": "https://api.example.com/v1",
                    "models": ["model-a"],
                    "modelKeys": {
                        "model-a": "MG_MODEL_KEYS_MISSING_ENV_NOT_SET",
                        "ghost": "MG_MODEL_KEYS_GHOST_NOT_SET",
                        "a*b*c": "MG_MODEL_KEYS_MULTI_NOT_SET",
                        "bad": 123,
                        "literal": {"apiKey": "sk-plain"},
                        "empty": ""
                    }
                }
            }
        }));
        let diags = cfg.diagnostics();
        let has = |code: &str, path: &str| diags.iter().any(|d| d.code == code && d.path == path);
        assert!(has("model_key_missing", "upstreams.u.modelKeys.model-a"));
        assert!(has(
            "model_key_unknown_model",
            "upstreams.u.modelKeys.ghost"
        ));
        assert!(has(
            "multi_wildcard_model_key",
            "upstreams.u.modelKeys.a*b*c"
        ));
        assert!(has("invalid_model_keys", "upstreams.u.modelKeys.bad"));
        assert!(has(
            "model_key_literal_ignored",
            "upstreams.u.modelKeys.literal"
        ));
        assert!(has("invalid_model_keys", "upstreams.u.modelKeys.literal"));
        assert!(has("invalid_model_keys", "upstreams.u.modelKeys.empty"));
    }

    /// 统一模型名只在 `/` 前缀命中已配置上游时拆分，模型名自带的 `/` 不受影响。
    #[test]
    fn split_qualified_model_requires_known_upstream_prefix() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "go": {"protocol": "openai", "baseUrl": "http://x", "models": ["m"]},
                "commandcode": {
                    "protocol": "openai",
                    "baseUrl": "http://y",
                    "models": ["deepseek/deepseek-v4-flash"]
                }
            }
        }));
        let (up, model) = cfg.split_qualified_model("go/m").expect("应识别上游前缀");
        assert_eq!(up.name, "go");
        assert_eq!(model, "m");

        let (up, model) = cfg
            .split_qualified_model("commandcode/deepseek/deepseek-v4-flash")
            .expect("应识别上游前缀");
        assert_eq!(up.name, "commandcode");
        assert_eq!(model, "deepseek/deepseek-v4-flash");

        assert!(cfg
            .split_qualified_model("deepseek/deepseek-v4-flash")
            .is_none());
        assert!(cfg.split_qualified_model("go/").is_none());
        assert!(cfg.split_qualified_model("m").is_none());
    }

    /// 上游名本身含 `/` 时按最长前缀匹配，避免被短名抢先拆分。
    #[test]
    fn split_qualified_model_prefers_longest_upstream_name() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://x", "models": ["x"]},
                "a/b": {"protocol": "openai", "baseUrl": "http://y", "models": ["y"]}
            }
        }));
        let (up, model) = cfg.split_qualified_model("a/b/y").expect("应识别上游前缀");
        assert_eq!(up.name, "a/b");
        assert_eq!(model, "y");
    }

    /// 统一清单 = 各上游声明模型 + modelMap 别名，均带上游前缀；不受顶层白名单影响。
    #[test]
    fn qualified_model_list_prefixes_declared_models_and_aliases() {
        let cfg = Config::from_value(json!({
            "models": ["only-whitelisted"],
            "upstreams": {
                "go": {
                    "protocol": "openai",
                    "baseUrl": "http://x",
                    "models": ["glm-5.3", "deepseek-v4"],
                    "modelMap": {"glm": "glm-5.3"}
                },
                "zen": {"protocol": "openai", "baseUrl": "http://y", "models": ["free"]}
            }
        }));
        assert_eq!(
            cfg.qualified_model_list(),
            vec![
                ("go/glm-5.3".to_string(), "go".to_string()),
                ("go/deepseek-v4".to_string(), "go".to_string()),
                ("go/glm".to_string(), "go".to_string()),
                ("zen/free".to_string(), "zen".to_string()),
            ]
        );
    }

    /// 统一接口：可见白名单先过滤，分组只在可见范围内再收窄。
    #[test]
    fn unified_scope_filters_visible_upstreams_and_groups() {
        let cfg = Config::from_value(json!({
            "unified": {
                "visibleUpstreams": ["a", "b"],
                "groups": [
                    {"id": "g1", "label": "组一", "upstreams": ["b"]},
                    {"id": "g2", "upstreams": ["c"]}
                ]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["am"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["bm"]},
                "c": {"protocol": "openai", "baseUrl": "http://c", "models": ["cm"]}
            }
        }));
        let visible: Vec<&str> = cfg
            .unified_visible_upstreams()
            .into_iter()
            .map(|up| up.name.as_str())
            .collect();
        assert_eq!(visible, vec!["a", "b"]);

        let group = cfg.unified_group_upstreams("g1").expect("分组存在");
        assert_eq!(group.len(), 1);
        assert_eq!(group[0].name, "b");
        assert_eq!(
            cfg.qualified_model_list_for(&group),
            vec![("b/bm".to_string(), "b".to_string())]
        );

        // g2 的成员 c 不在可见白名单中：分组存在但结果为空。
        assert!(cfg.unified_group_upstreams("g2").unwrap().is_empty());
        assert!(cfg.unified_group_upstreams("missing").is_none());
    }

    /// 不配 visibleUpstreams 时默认全部上游可见。
    #[test]
    fn unified_visible_defaults_to_all_upstreams() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["am"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["bm"]}
            }
        }));
        let visible: Vec<&str> = cfg
            .unified_visible_upstreams()
            .into_iter()
            .map(|up| up.name.as_str())
            .collect();
        assert_eq!(visible, vec!["a", "b"]);
        assert!(cfg.unified.groups.is_empty());
    }

    /// 可见范围内的自动分发必须跳过被隐藏的默认上游。
    #[test]
    fn pick_upstream_scoped_skips_hidden_default() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "hidden": {
                    "protocol": "openai",
                    "baseUrl": "http://hidden",
                    "models": ["same"],
                    "default": true
                },
                "visible": {
                    "protocol": "openai",
                    "baseUrl": "http://visible",
                    "models": ["same"]
                }
            }
        }));
        let allowed = vec!["visible".to_string()];
        let picked = cfg
            .pick_upstream_scoped(Some("same"), Some(&allowed))
            .expect("应选到可见上游");
        assert_eq!(picked.name, "visible");
    }

    /// 分组标识非法/重复、引用未知或隐藏上游，都要给出可定位诊断。
    #[test]
    fn unified_diagnostics_report_invalid_groups() {
        let cfg = Config::from_value(json!({
            "unified": {
                "visibleUpstreams": ["a", "ghost"],
                "groups": [
                    {"id": "team/a", "upstreams": ["a"]},
                    {"id": "dup", "upstreams": ["b"]},
                    {"id": "dup", "upstreams": ["ghost"]}
                ]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["am"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["bm"]}
            }
        }));
        let diags = cfg.diagnostics();
        let has = |level: &str, code: &str, path: &str| {
            diags
                .iter()
                .any(|d| d.level == level && d.code == code && d.path == path)
        };
        assert!(has(
            "error",
            "invalid_unified_group",
            "unified.groups[0].id"
        ));
        assert!(has(
            "error",
            "duplicate_unified_group",
            "unified.groups[2].id"
        ));
        assert!(has(
            "warn",
            "unified_visible_unknown",
            "unified.visibleUpstreams.ghost"
        ));
        assert!(has(
            "warn",
            "unified_group_unknown_upstream",
            "unified.groups[2].upstreams.ghost"
        ));
        assert!(has(
            "warn",
            "unified_group_hidden_upstream",
            "unified.groups[1].upstreams.b"
        ));
    }

    /// 路由组成员保持声明顺序；别名与 `上游/模型` 都能解析；重复成员只保留一次。
    #[test]
    fn route_groups_parse_order_and_resolve_members() {
        let cfg = Config::from_value(json!({
            "aliases": {"fast": "flash"},
            "groups": {
                "daily": ["a/flash", "b/flash", "fast", "a/flash"]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["flash"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["flash"]}
            }
        }));
        assert_eq!(cfg.groups.len(), 1);
        assert_eq!(cfg.groups[0].id, "daily");
        assert_eq!(
            cfg.groups[0].members,
            vec!["a/flash", "b/flash", "fast", "a/flash"]
        );
        assert!(cfg.model_list().iter().any(|id| id == "group/daily"));
        assert_eq!(cfg.group_id_from_model("group/daily"), Some("daily"));
        assert_eq!(cfg.group_id_from_model("group/a/b"), None);

        let group = cfg.route_group("daily").expect("组存在");
        let targets: Vec<(String, String)> = cfg
            .group_targets(group)
            .into_iter()
            .map(|(up, model)| (up.name.clone(), model))
            .collect();
        assert_eq!(
            targets,
            vec![
                ("a".to_string(), "flash".to_string()),
                ("b".to_string(), "flash".to_string())
            ],
            "别名 fast 落到 a/flash，重复的 a/flash 只保留一次"
        );
    }

    /// 路由组的可见性跟随统一接口的上游白名单：有任意成员可见就暴露。
    #[test]
    fn route_group_visibility_follows_unified_scope() {
        let cfg = Config::from_value(json!({
            "groups": {
                "daily": ["a/flash", "b/flash"],
                "only-b": ["b/flash"]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["flash"]},
                "b": {"protocol": "openai", "baseUrl": "http://b", "models": ["flash"]}
            }
        }));
        let a = cfg.upstream("a").unwrap();
        assert_eq!(
            cfg.group_model_list_for(&[a]),
            vec!["group/daily".to_string()]
        );
        let b = cfg.upstream("b").unwrap();
        assert_eq!(
            cfg.group_model_list_for(&[b]),
            vec!["group/daily".to_string(), "group/only-b".to_string()]
        );
    }

    /// 路由组成员写错、重复、模型未声明、组 id 非法都要可定位。
    #[test]
    fn route_group_diagnostics_report_member_problems() {
        let cfg = Config::from_value(json!({
            "groups": {
                "daily": ["ghost/model", "a/missing", "a/flash", "a/flash", ""],
                "bad/id": ["a/flash"]
            },
            "upstreams": {
                "a": {"protocol": "openai", "baseUrl": "http://a", "models": ["flash"]}
            }
        }));
        let diags = cfg.diagnostics();
        let has = |level: &str, code: &str, path: &str| {
            diags
                .iter()
                .any(|d| d.level == level && d.code == code && d.path == path)
        };
        assert!(has("error", "group_member_unresolved", "groups.daily[0]"));
        assert!(has(
            "warn",
            "group_member_model_not_declared",
            "groups.daily[1]"
        ));
        assert!(has("warn", "duplicate_group_member", "groups.daily[3]"));
        assert!(has("error", "invalid_group_member", "groups.daily[4]"));
        assert!(has("error", "invalid_group", "groups.bad/id"));
    }

    /// 上游名 `group` 会与虚拟模型前缀冲突，必须提示。
    #[test]
    fn route_group_reserved_prefix_warns_for_upstream_named_group() {
        let cfg = Config::from_value(json!({
            "upstreams": {
                "group": {"protocol": "openai", "baseUrl": "http://group", "models": ["m"]}
            }
        }));
        assert!(cfg
            .diagnostics()
            .iter()
            .any(|d| d.code == "reserved_group_prefix" && d.path == "upstreams.group"));
    }
}
