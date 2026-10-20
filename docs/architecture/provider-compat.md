# Provider 兼容能力（compat）

> 目标：把 opencode、commandcode、deepseek 这类“某个上游特有的处理”从协议代码里抽出来，
> 变成可声明、可覆盖、可校验的数据。新增同类上游时，优先选最接近的 wire 协议 + `compat` 覆盖，
> 不再需要改代码里的 `if protocol == ...`。

## 1. 分层模型

一份上游配置由三层组成，后者覆盖前者：

| 层 | 来源 | 作用 |
|---|---|---|
| 1. provider 预设 | `protocol` + `baseUrl` 自动推断，或 `compat.profile` 显式指定 | 给出该厂商的一组默认能力 |
| 2. 旧版平铺字段 | `sessionHeader` / `cmdZdr` / `cliVersion` / `thinkingPassthrough` / `downgradeForcedChoice` | 兼容历史配置，不要求迁移 |
| 3. 显式 `compat` | `upstreams.<name>.compat` | 最终覆盖；面板“兼容能力”区域可编辑 |

预设名：

| profile | 触发条件 | 默认能力 |
|---|---|---|
| `opencode` | `baseUrl` 含 `opencode.ai`，或 `compat.profile=opencode` | Bearer；`x-opencode-session: <hex32>`；`/v1/chat/completions` |
| `commandcode` | `protocol=commandcode` 或域名 `commandcode.ai` | Bearer；`x-session-id: sess_<16>`；`/alpha/generate`；`x-command-code-version`、`x-cli-environment`、`x-cmd-zdr: 1`；强制 `tool_choice` 降级；输出上限 64000 |
| `anthropic` | `protocol=anthropic` | Bearer + `x-api-key`；`/v1/messages`；思考回传开启；强制 `tool_choice` 降级 |
| `deepseek` | 域名 `api.deepseek.com`，或 `compat.profile=deepseek` | 思考回传开启；输出上限 384K；上下文 1M |
| `openai` | 其余 `protocol=openai` | Bearer；`/v1/chat/completions`；无会话头 |

> `protocol` 仍然只决定 **wire 形态**（OpenAI Chat / Anthropic Messages / CommandCode NDJSON），
> 这是请求/响应结构差异，不能仅靠配置消除；其余厂商差异都走 `compat`。

`transport` 是独立于 `protocol` 的传输适配器，优先级高于 `protocol` 推断，当前只有 `opencode`。
它处理普通 compat 无法表达的部分：客户端指纹、每次请求重新生成的 ID、URL 重写、强制流式、
工具注入/改名/过滤，以及 SSE 聚合。`transport=opencode` 只能搭配 `protocol=openai`。

## 2. `compat` 字段

```json
{
  "compat": {
    "profile": "opencode",
    "auth": { "scheme": "bearer", "header": "authorization", "prefix": "Bearer " },
    "session": { "enabled": true, "header": "x-opencode-session", "idStyle": "hex32" },
    "headers": { "x-client-name": "model-gate" },
    "endpoint": { "chat": "/v1/chat/completions", "models": "/v1/models" },
    "thinking": { "passthrough": true, "field": "reasoning_content" },
    "toolChoice": { "downgradeForced": true },
    "limits": { "maxOutputTokens": 393216, "maxContextTokens": 1048576 },
    "zdr": true,
    "versionHeader": "x-command-code-version",
    "environmentHeader": "x-cli-environment"
  }
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `profile` | string | 强制使用某个预设，优先级高于自动推断 |
| `auth.scheme` | `bearer` / `x-api-key` / `both` / `none` / `header` | 鉴权头形态；`none` 表示不发鉴权头 |
| `auth.header` | string | `scheme=header` 时的头名 |
| `auth.prefix` | string | 鉴权值前缀，默认 `Bearer ` |
| `session.enabled` | bool | 是否注入会话头 |
| `session.header` | string | 会话头名；空串表示不注入 |
| `session.idStyle` | `hex32` / `sess16` | `sess16` 生成 `sess_` + 前 16 位 |
| `headers` | object | 额外静态请求头；同名鉴权头会被 `auth` 覆盖 |
| `endpoint.chat` | string | 对话端点，必须 `/` 开头 |
| `endpoint.models` | string | 模型清单端点，必须 `/` 开头 |
| `catalog` | object | 模型目录来源：`{ "source": "endpoint" \| "commandcode", "path": "/v1/models", "plan": false }`；CommandCode 旧字段 `modelCatalog` / `modelCatalogPlan` 映射到 `source=commandcode` |
| `thinking.passthrough` | bool | 是否把上一轮 thinking + signature 回传给上游（仅 anthropic 协议参与请求改写） |
| `thinking.field` | string | 思考字段名，默认 `reasoning_content` |
| `toolChoice.downgradeForced` | bool | 强制 `tool_choice` 是否降级为 `auto` |
| `limits.maxOutputTokens` | number | 上游最大输出；请求超过时自动收敛 |
| `limits.maxContextTokens` | number | 上下文窗口，仅用于校验/提示 |
| `zdr` | bool | 发送 `x-cmd-zdr: 1` |
| `versionHeader` | string | CLI 版本头名，值来自 `cliVersion`（`auto` 时不发） |
| `environmentHeader` | string | CLI 环境头名，值来自 `cliEnvironment` |

未知字段不会生效，`/api/config/validate` 会返回 `unknown_compat_field` 警告。

### 2.1 预设注册表与模型级覆盖

预设不再硬编码在 `config.rs`，统一由 `rust/model-gate/provider_profiles.json` 定义
（Rust 编译期 `include_str!` 嵌入，随 exe 分发）。判定优先级：

1. 显式 `compat.profile`（或旧字段 `profile`）；
2. 域名匹配，按注册表 `priority` 取最高（`commandcode.ai` 100 > `api.deepseek.com` 90 > `opencode.ai` 80）；
3. 协议对应预设（`openai` / `anthropic` / `commandcode`）。

`upstreams.<name>.modelOverrides`（顶层字段，不放进 `compat`）在预设/覆盖之上做模型级差异化：

```json
"modelOverrides": {
  "deepseek-chat": { "limits": { "maxOutputTokens": 8192 }, "capabilities": { "thinking": false } },
  "deepseek-*": { "limits": { "maxOutputTokens": 65536 } }
}
```

- key 为路由解析后的模型 id，支持只含一个 `*` 的前缀/后缀通配；精确 > 最长通配 > 上游 `compat.limits`；
- `limits.maxOutputTokens` 与 `compat.limits` 走同一套 `max_tokens` 裁剪；`capabilities` 仅用于展示与诊断；
- 能力字段 `stream / tools / vision / thinking / cache / systemPrompt`，合并顺序：
  模型覆盖 > `compat.capabilities` > profile > 内置默认。

相关诊断码（双端一致）：`unknown_profile`、`invalid_model_overrides`、`invalid_limit_relation`、
`invalid_capabilities`、`model_override_unknown_model`、`multi_wildcard_override`。
完整规则与 API 契约见 [`upstream-config-model.md`](upstream-config-model.md)。

## 3. 典型配置

### 3.1 opencode（go / zen）

```json
"go": {
  "protocol": "openai",
  "baseUrl": "https://opencode.ai/zen/go/v1",
  "apiKeyEnv": "OPENCODE_API_KEY",
  "models": ["glm-5.3-flash", "deepseek-v4.1-flash"]
}
```

预设自动补 `x-opencode-session`。若要关闭：

```json
"compat": { "session": false }
```

#### OpenCode Zen 免费层：`transport=opencode`

```json
"zen": {
  "protocol": "openai",
  "transport": "opencode",
  "baseUrl": "https://opencode.ai/zen/v1",
  "models": ["mimo-v2.6-flash-free", "big-pickle"],
  "fetchModels": false,
  "sessionHeader": false,
  "autoModels": false
}
```

该 transport 只服务免费层，不读取 apiKey，也不涉及 `go` 订阅：

- 固定 `Authorization: Bearer public` 与 OpenCode CLI UA；生成 `ses_...` 会话 ID 和每请求新的 `msg_...` 请求 ID。
- `opencode.ai` 的 `/zen/...` 改写为 `/inference/...`；`/zen/go/...` 不改写。
- 上游强制 `stream: true`；客户端非流式时聚合 SSE。
- 自动注入 `bash` / `read` 工具（含 schema），客户端同名工具转小写，响应工具名映射回原名；仅调用 stub 工具时返回提示并以 `stop` 结束。
- 非免费模型（`-free` / `big-pickle` / `grok-code` 之外）本地 400 `model_not_free`，不请求上游。
- 免费层失败不自动切换上游，避免静默降级到付费路径。
- 诊断：`opencode_free_tier_rejected`、`opencode_cloudflare_blocked`、`opencode_rate_limited`。
- `grok-code` 仍未用本桥实测，默认不启用。

> 行为基线来自 magpie 的 `internal/provider/opencode_client.go` 与
> `internal/gateway/zenfree.go`，并用 2026-10-20 真机请求复核。
> 这是 OpenCode 未公开的客户端契约，可能随上游升级失效；不要把它用于付费/Go 上游。

### 3.2 CommandCode

```json
"commandcode": {
  "protocol": "commandcode",
  "baseUrl": "https://api.commandcode.ai",
  "apiKeyEnv": "COMMAND_CODE_API_KEY",
  "apiKeyFile": "~/.commandcode/auth.json",
  "compat": {
    "zdr": false,
    "versionHeader": "x-command-code-version",
    "limits": { "maxOutputTokens": 64000 }
  }
}
```

### 3.3 DeepSeek 官方 / Anthropic 兼容层

```json
"deepseek": {
  "protocol": "openai",
  "baseUrl": "https://api.deepseek.com/v1",
  "apiKeyEnv": "DEEPSEEK_API_KEY",
  "models": ["deepseek-chat"]
}
```

预设自动识别 `deepseek`，补输出/上下文上限与思考字段。
若走 Anthropic 兼容网关（例如 CCH），用：

```json
"cch": {
  "protocol": "anthropic",
  "baseUrl": "http://127.0.0.1:15721",
  "models": ["deepseek-flash"],
  "compat": { "thinking": { "passthrough": true } }
}
```

### 3.4 任意 OpenAI 兼容厂商

```json
"myprovider": {
  "protocol": "openai",
  "baseUrl": "https://api.example.com/v1",
  "apiKeyEnv": "MY_API_KEY",
  "models": ["my-model"],
  "compat": {
    "auth": { "scheme": "header", "header": "x-api-key", "prefix": "" },
    "endpoint": { "chat": "/v1/chat/completions", "models": "/v1/models" }
  }
}
```

## 4. 控制与校验

| 入口 | 行为 |
|---|---|
| `GET /api/config` | 返回 `config`、`diagnostics`（结构化诊断）、`effective`（解析后的能力） |
| `POST /api/config/validate` | 只校验不落盘，返回 `ok / errors / warnings / diagnostics / effective` |
| `POST /api/config` | 先诊断；存在 `error` 级问题返回 400 且不写文件；成功响应也带 `diagnostics` |
| 面板 | 加载/保存后展示诊断；每个上游可编辑 compat 字段；保存时保留未渲染字段 |

诊断级别：

- `error`：会阻断保存（无效 Base URL、未知协议、非法鉴权方式/头名/端点、输出大于上下文等）
- `warn`：可保存但可能不生效（字段不适用、未知字段、缺少凭据、上限被收敛等）
- `info`：仅提示（重复模型、密钥优先级、thinking 对非 anthropic 不参与改写等）

## 5. 兼容性

- 面板「兼容能力」中：留空 = 跟随 provider 预设；会话头填 `-` 表示不注入；版本/环境头填 `-` 表示不发送。
- 旧配置无需迁移：`sessionHeader`、`cmdZdr`、`cliVersion`、`cliEnvironment`、`pauseTurn`、`modelCatalog*` 仍然生效。
- `compat` 与旧字段冲突时，`compat` 优先。
- 全局 `thinking.passthrough=false` 或 `tools.downgradeForcedChoice=false` 是最终约束，单上游无法绕过。
- 面板保存时会保留所有未渲染的上游字段，避免“保存一次丢配置”。
- 预设注册表随包分发：`rust/model-gate/provider_profiles.json` 在 Rust 编译期嵌入；升级时只需修改该 JSON 并重新构建。
