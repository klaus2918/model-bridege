# 04 AI 可读配置接口

> 目标：让内置 AI 能"看懂能配什么、安全地生成配置草案、在人工确认后应用"，同时**永不读取密钥、账号、Cookie、Token**。

## 1. 设计原则

| # | 原则 | 后果 |
|---|---|---|
| A1 | 默认只读 + 草案 | AI 不能直接改生产配置 |
| A2 | 敏感值单向写入 | 密钥只能从 UI/本机回环写入，AI 永远读不到 |
| A3 | 所有变更可预演 | `plan` 返回 diff、风险、缺失字段 |
| A4 | 应用需确认令牌 | `apply` 必须带 `planId + confirmation` |
| A5 | 最小暴露 | 响应只给"是否配置/来源类型/池统计"，不给值 |
| A6 | 可审计 | 每次 plan/apply 记录 actor、字段、结果（脱敏） |

## 2. 身份与权限

| 调用方 | 识别方式 | 默认权限 |
|---|---|---|
| 面板 UI | 本机回环 + 同源 | 全部 |
| 内置 AI（MCP/本地） | 本机回环 + `X-MG-Actor: ai` + 可选 `X-MG-AI-Token` | 读 + plan；apply 需人工确认 |
| CLI | 本机回环 + `X-MG-Actor: cli` | 读 + plan；apply 需 `--confirm` |
| 远程 | **禁止**（绑定 127.0.0.1） | 无 |

设置项 `aiConfig.enabled`（默认 true）、`aiConfig.allowApply`（默认 false）、`aiConfig.allowPoolStats`（默认 true）。

## 3. 端点总览

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/api/ai/manifest` | 能力声明、版本、端点、权限、脱敏规则 |
| GET | `/api/ai/schema` | 配置 JSON Schema（逻辑视图 + 写模型） |
| GET | `/api/ai/templates` | 模板目录（官方/自定义/插件） |
| GET | `/api/ai/templates/{id}` | 单模板详情（默认值、必填、隐私能力） |
| GET | `/api/ai/inspect` | 脱敏现状：上游、凭据状态、池统计、诊断 |
| POST | `/api/ai/plan` | 生成变更计划（不落盘） |
| POST | `/api/ai/apply` | 应用计划（需确认） |
| GET | `/api/ai/audit` | 审计列表（脱敏） |
| GET | `/llms.txt` | 面向 AI 的纯文本说明书（稳定、可缓存） |

## 4. manifest

```jsonc
{
  "schema": "modelgate.ai/v1",
  "coreVersion": "3.0.0",
  "generatedAt": "2026-11-01T10:00:00+08:00",
  "baseUrl": "http://127.0.0.1:8900",
  "endpoints": {
    "templates": "/api/ai/templates",
    "schema": "/api/ai/schema",
    "inspect": "/api/ai/inspect",
    "plan": "/api/ai/plan",
    "apply": "/api/ai/apply",
    "audit": "/api/ai/audit"
  },
  "permissions": {
    "readConfig": true,
    "readSecrets": false,          // 永远 false
    "readAccounts": false,
    "createDraft": true,
    "applyNonSecret": false,       // 由设置决定
    "applySecret": false           // 永远 false
  },
  "redaction": {
    "secretFields": ["apiKey", "token", "secret", "password", "cookie", "authorization"],
    "strategy": "omit",
    "allowLast4": false,
    "maskedExample": "sk-****"
  },
  "actions": [
    "create_upstream_from_template",
    "create_custom_upstream",
    "update_upstream_non_secret",
    "attach_plugin",
    "detach_plugin",
    "create_pool",
    "update_pool_policy"
  ],
  "notes": [
    "密钥只能由人工在本机 UI 写入；AI 可创建待补充凭据的接入。",
    "privacy.zdr 表示用户意图，实际发送取决于厂商能力（见 template.privacy.support）。"
  ]
}
```

## 5. Schema

`GET /api/ai/schema` 返回 JSON Schema（draft 2020-12），覆盖：

- `AccessDraft`：创建/更新接入的请求体；
- `PoolDraft`：池创建/策略；
- `Plan` / `ApplyRequest`；
- `InspectResponse`（只读）。

关键约束写入 Schema：

```jsonc
{
  "$id": "modelgate.access-draft/v1",
  "type": "object",
  "required": ["templateId", "name"],
  "properties": {
    "templateId": { "type": "string" },
    "name": { "type": "string", "pattern": "^[^/\\s]+$" },
    "protocol": { "enum": ["openai", "anthropic", "commandcode", "opencode", "zcode"] },
    "baseUrl": { "type": "string", "format": "uri" },
    "privacy": {
      "type": "object",
      "properties": { "zdr": { "type": "boolean" }, "training": { "enum": ["allow", "deny"] } }
    },
    "apiKey": { "type": "string", "writeOnly": true, "x-mg-secret": true }
  },
  "additionalProperties": false
}
```

`writeOnly` + `x-mg-secret` 明确告知 AI：该字段不可读、不可回显。

## 6. Templates

```http
GET /api/ai/templates?kind=official
```

```jsonc
{
  "items": [
    {
      "id": "deepseek",
      "label": "DeepSeek 官方",
      "kind": "official",
      "protocols": ["openai", "anthropic"],
      "defaults": {
        "protocol": "openai",
        "baseUrl": "https://api.deepseek.com",
        "authScheme": "bearer"
      },
      "privacy": { "zdrDefault": true, "trainingDefault": "deny", "support": "request" },
      "requiredFields": ["apiKey"],
      "advancedFields": ["modelOverrides", "modelKeys", "sessionHeader"],
      "docsUrl": "https://platform.deepseek.com"
    }
  ],
  "total": 14
}
```

AI 使用方式：先选 `kind=official` + `requiredFields=["apiKey"]` 的模板，提示用户只填 Key。

## 7. Inspect（脱敏）

```http
GET /api/ai/inspect?upstream=deepseek
```

```jsonc
{
  "upstream": {
    "id": "deepseek",
    "kind": "official",
    "template": "deepseek",
    "protocol": "openai",
    "baseUrl": "https://api.deepseek.com",
    "auth": {
      "mode": "single",
      "configured": true,
      "source": "store",
      "hasSecret": true,
      "secretRef": "upstream:deepseek",
      "last4": null
    },
    "privacy": { "zdr": true, "support": "request" },
    "models": { "declared": 12, "fetched": 12 },
    "health": "ok",
    "diagnostics": []
  },
  "pool": null
}
```

池模式：

```jsonc
{
  "pool": {
    "id": "pool_gateway",
    "strategy": "priority",
    "members": 4,
    "available": 2,
    "cooling": 1,
    "disabled": 1,
    "exhausted": 0,
    "quota": { "used": 320000, "limit": 1000000 }
  }
}
```

**不返回**：成员 label 列表（除非 `allowPoolStats=false` 时只返回计数）、密钥、账号、Cookie、请求头。

## 8. Plan

```http
POST /api/ai/plan
{
  "action": "create_upstream_from_template",
  "input": {
    "templateId": "deepseek",
    "name": "deepseek",
    "privacy": { "zdr": true }
  }
}
```

响应：

```jsonc
{
  "planId": "plan_01J...",
  "action": "create_upstream_from_template",
  "expiresAt": "2026-11-01T10:10:00+08:00",
  "status": "needs_secret",
  "summary": "将创建官方接入 deepseek，使用模板默认协议与地址。",
  "diff": [
    { "path": "upstreams.deepseek.template", "op": "add", "value": "deepseek" },
    { "path": "upstreams.deepseek.protocol", "op": "add", "value": "openai" },
    { "path": "upstreams.deepseek.baseUrl", "op": "add", "value": "https://api.deepseek.com" },
    { "path": "upstreams.deepseek.privacy.zdr", "op": "add", "value": true }
  ],
  "missing": [
    { "field": "apiKey", "reason": "需要人工在本机面板填写；AI 不可写入密钥" }
  ],
  "warnings": [
    { "code": "privacy_request_supported", "message": "该厂商支持请求级 ZDR。" }
  ],
  "confirmation": "required"
}
```

`status` 枚举：`ready`（可 apply）、`needs_secret`（需人工补 Key）、`needs_input`（缺非敏感字段）、`invalid`。

**Plan 不落盘**：只存内存，TTL 10 分钟，最多 100 条；进程重启失效。

## 9. Apply

```http
POST /api/ai/apply
{
  "planId": "plan_01J...",
  "confirmation": "user-confirmed-7f3a",
  "expectedRevision": "cfgrev_123"
}
```

规则：
- `confirmation` 由面板在用户点击"确认应用"时生成，绑定 `planId`，一次性；
- `expectedRevision` 防止并发覆盖；
- 只允许 `plan` 中声明且非敏感的字段；
- 若计划含 `needs_secret`，apply 返回 `409 needs_secret`，不得跳过；
- 成功返回新 `AccessObject`（脱敏）+ 备份 id + 审计 id；
- 失败回滚并返回阶段。

**AI 不能**：写 `apiKey`、`apiKeyStore`、`apiKeyEnv`、`modelKeys.*.apiKey`、池成员 secret、ZCode 账号。

## 10. 审计

```jsonc
{
  "id": "audit_01J...",
  "at": "2026-11-01T10:02:00+08:00",
  "actor": "ai",
  "action": "apply",
  "planId": "plan_01J...",
  "target": "upstreams.deepseek",
  "changedPaths": ["template", "protocol", "baseUrl", "privacy.zdr"],
  "result": "ok",
  "backupId": "bak_01J..."
}
```

审计保留 90 天，可在设置页查看/导出；导出不含密钥。

## 11. 脱敏规则

| 数据 | 读取 | 响应 | 日志 | 审计 |
|---|---|---|---|---|
| `apiKey` / `token` / `secret` | ❌ | 省略 | 省略 | 省略 |
| `apiKeyStore` 引用 | ✅ | 返回引用 id | 返回 id | 返回 id |
| 环境变量名 | ✅ | 返回变量名 | 返回变量名 | 返回变量名 |
| 账号/邮箱/Cookie | ❌ | 仅计数 | 仅计数 | 仅计数 |
| 池成员标签 | 可配置 | 默认不返回 | 默认不返回 | 默认不返回 |
| `last4` | 可配置 | 默认 null | 不记录 | 不记录 |

统一实现 `ai_api::redact.rs`：所有 AI 响应先过 `Redactor`，按字段名和 `x-mg-secret` 标记双重过滤；单测覆盖嵌套对象、数组、错误信息、诊断文本。

## 12. 错误码

| HTTP | code | 含义 |
|---|---|---|
| 400 | `invalid_plan` | 计划不存在/过期/已用 |
| 400 | `missing_field` | 缺非敏感必填 |
| 403 | `ai_apply_disabled` | 设置未允许 apply |
| 409 | `needs_secret` | 计划需要人工补密钥 |
| 409 | `revision_conflict` | 配置已变化 |
| 409 | `name_conflict` | 上游重名 |
| 422 | `template_unknown` | 模板不存在 |
| 503 | `pool_unavailable` | 池无可用成员 |

## 13. `llms.txt` 示例

```text
# Model-Gate AI Configuration
Base: http://127.0.0.1:8900
Read:  GET /api/ai/manifest, /api/ai/templates, /api/ai/inspect
Draft: POST /api/ai/plan
Apply: POST /api/ai/apply (requires human confirmation)
Never: read or write apiKey/token/account/cookie.
Official provider: choose template, ask human for apiKey, plan, wait for confirmation.
Custom provider: name, vendor, baseUrl, protocol, apiKey.
Privacy: zdr=true is user intent; check template.privacy.support before claiming protection.
```

## 14. 验收

- [ ] AI 能根据 manifest + templates 独立生成官方/自定义接入草案。
- [ ] `inspect` 对所有上游均不返回密钥、账号、Cookie。
- [ ] `apply` 无确认令牌必失败；含密钥的计划必返回 `needs_secret`。
- [ ] 并发修改触发 `revision_conflict`，不覆盖。
- [ ] 审计可追溯 plan → apply → backup。
- [ ] 脱敏单测覆盖嵌套/数组/错误文本。
