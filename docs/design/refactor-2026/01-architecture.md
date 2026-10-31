# 01 总体架构

## 1. 分层

```
┌──────────────────────────────────────────────────────────────┐
│ 接入面 Access Surface                                        │
│  面板 / 内置 AI / CLI / 插件市场                               │
│  只依赖：AccessObject、Template、Plan、Diagnostics             │
└───────────────────────────┬──────────────────────────────────┘
                            │ 统一命令/查询（不直接碰配置与表）
┌───────────────────────────▼──────────────────────────────────┐
│ 接入编排 Access Orchestrator                                 │
│  TemplateRegistry · Assembler · Validator · CredentialBinder  │
│  AccessObject 聚合 · 诊断归一 · 变更计划(Plan) · 审计           │
└───────────┬───────────────────────────────┬──────────────────┘
            │                               │
┌───────────▼─────────────┐     ┌───────────▼──────────────────┐
│ 内核 Core               │     │ 插件运行时 Plugin Runtime     │
│ 路由 · 协议转换 · 流式   │     │ declarative / native         │
│ 鉴权注入 · 重试/切换     │     │ transport · scheduler        │
│ 用量 · 日志 · 提示词     │     │ 只声明，不写配置              │
└───────────┬─────────────┘     └───────────┬──────────────────┘
            │                               │
┌───────────▼───────────────────────────────▼──────────────────┐
│ 存储 Storage（唯一事实源，不新增第二份）                       │
│ bridge.config.json + *.secrets.json(DPAPI)                   │
│ SQLite: 稳态表 + 新增 credential_pools / credential_members    │
│ 旧资产原样：free_sources.json / provider_profiles.json /       │
│           data/zcode.json / zcode_accounts / zcode_claims     │
└──────────────────────────────────────────────────────────────┘
```

**关键点**：接入面不再直接拼 `upstreams.<name>`，而是提交"意图"给编排层。编排层负责模板合并、校验、凭据绑定、计划生成、落盘与回读。这样面板、AI、CLI、插件走同一条路径，不会各写一套。

## 2. 领域模型

### 2.1 AccessObject（接入对象，读模型）

运行时聚合，不落盘。一个 `AccessObject` 对应一个 `upstreams.<name>`，并叠加模板、凭据、池、插件、健康与用量。

```jsonc
{
  "id": "deepseek",                 // 即 upstream 名
  "label": "DeepSeek 官方",
  "kind": "official",               // official | custom | free | account | plugin
  "source": { "type": "template", "id": "deepseek", "pluginId": null },
  "protocol": "openai",
  "baseUrl": "https://api.deepseek.com",
  "transport": "",
  "auth": {
    "mode": "single",               // none | single | pool
    "scheme": "bearer",
    "configured": true,
    "source": "store",              // env | file | store | pool | none
    "secretRef": "upstream:deepseek",
    "poolId": null
  },
  "privacy": {
    "zdr": true,
    "training": "deny",
    "support": "request"            // request | account | none
  },
  "models": { "declared": 12, "fetched": 12, "aliases": 3 },
  "routing": { "default": true, "groups": ["chat"], "visible": true },
  "health": { "state": "ok", "lastProbeAt": "...", "latencyMs": 420 },
  "usage": { "todayRequests": 128, "todayTokens": 920000 },
  "diagnostics": []
}
```

### 2.2 Upstream（写模型，保持旧结构）

物理写入仍是 `bridge.config.json#upstreams.<name>`。新增字段全部可选：

| 字段 | 类型 | 说明 |
|---|---|---|
| `template` | string | 模板 id；等价旧 `compat.profile`，但语义是"接入模板" |
| `kind` | string | `official/custom/free/account/plugin`，仅展示与筛选 |
| `label` | string | 展示名，缺省用 name |
| `privacy` | object | `{ zdr: bool, training: "allow"\|"deny" }`，缺省由模板给 |
| `credentialPool` | string | 池 id；存在时优先于 `apiKeyStore` |
| `templateVersion` | number | 模板版本，用于提示"模板已更新" |

其余旧字段（`protocol/baseUrl/apiKeyEnv/apiKeyStore/modelKeys/modelOverrides/compat/...`）原样保留。

### 2.3 Template（模板）

来源优先级：
1. 插件 manifest 的 `upstream` 模板（declarative/native）；
2. 内置 `provider_profiles.json`（扩展现有 14 套）；
3. 用户自定义模板（`data/templates/*.json`，可选，P2 后开放）。

模板结构在现有 profile 上**增量**扩展：

```jsonc
{
  "id": "deepseek",
  "label": "DeepSeek 官方",
  "kind": "official",
  "protocols": ["openai", "anthropic"],
  "access": {
    "protocol": "openai",
    "baseUrl": "https://api.deepseek.com",
    "authScheme": "bearer",
    "apiKeyEnv": "DEEPSEEK_API_KEY",
    "defaultModels": ["deepseek-v4.1-flash"],
    "docsUrl": "https://platform.deepseek.com"
  },
  "privacy": { "zdr": true, "training": "deny", "support": "request" },
  "defaults": { "fetchModels": true, "autoModels": false, "pauseTurn": false },
  "compat": { "...": "沿用现有 profile 字段" },
  "requiredFields": ["apiKey"],       // 快速添加只校验这些
  "advancedFields": ["modelKeys", "modelOverrides", "sessionHeader"]
}
```

`requiredFields` 是"只填 Key"的契约：官方模板只声明 `apiKey`；自定义模板声明 `name/protocol/baseUrl/apiKey`。

### 2.4 Credential / CredentialPool

```jsonc
// 单凭据：仍用 upstream.apiKeyStore
{ "apiKeyStore": "upstream:deepseek" }

// 池绑定
{ "credentialPool": "pool_deepseek" }
```

池定义（SQLite）：

```sql
CREATE TABLE IF NOT EXISTS credential_pools (
  id TEXT PRIMARY KEY,
  upstream TEXT NOT NULL,
  plugin_id TEXT,
  strategy TEXT NOT NULL DEFAULT 'priority', -- priority | round_robin | weighted | least_used
  status TEXT NOT NULL DEFAULT 'active',
  cooldown_seconds INTEGER NOT NULL DEFAULT 60,
  failure_threshold INTEGER NOT NULL DEFAULT 3,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS credential_members (
  id TEXT PRIMARY KEY,
  pool_id TEXT NOT NULL,
  label TEXT NOT NULL,
  kind TEXT NOT NULL,              -- api_key | account | oauth
  secret_ref TEXT NOT NULL,        -- store:xxx / legacy:zcode:<id>
  priority INTEGER NOT NULL DEFAULT 100,
  weight INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'active',
  cooldown_until TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0,
  quota_json TEXT,
  last_used_at TEXT,
  usage_json TEXT
);
```

约束：
- `secret_ref` 只存引用；密钥本体仍在 DPAPI 密钥库；ZCode 旧账号用 `legacy:zcode:<id>` 映射，不复制凭据。
- 删除池不删除成员密钥，先置 `status=disabled`；需二次确认"同时清理密钥"。
- 池与 `apiKeyStore` 二选一；同时存在时池优先，诊断给 `pool_overrides_store` 提示。

### 2.5 Plugin（插件）

沿用 `docs/architecture/plugin-system.md` 的三层与五规则，只补两点：
1. manifest 增加 `auth.mode: "pool"` 与 `pool` 段（策略默认值、成员 kind、配额字段）；
2. manifest 增加 `access.requiredFields` 与 `access.advancedFields`，供快速添加和 AI 读取。

## 3. 接入装配管线

```
用户/AI/插件 意图
   │  { templateId, name, protocol, baseUrl, apiKey?, privacy? }
   ▼
1. ResolveTemplate  ── 模板不存在 → 报错并给出近似模板
2. Normalize        ── 旧字段 → 新字段；空值 → 模板默认值
3. Validate         ── 必填、URL、协议、重名、保留字、密钥来源
4. BindCredential   ── single: 写 secret_store + apiKeyStore
   │                    pool:   校验池存在/归属/至少 1 个可用成员
5. Preview          ── 返回 AccessObject + diff + 诊断（不落盘）
6. Commit           ── 备份 → 原子写 → 回读 → reload
7. Audit            ── 记录 actor(ui/ai/cli/plugin)、planId、字段 diff（脱敏）
```

**幂等**：同一 `planId` 重复 Commit 返回首次结果；`name` 冲突返回 409 + diff，不覆盖。

**失败回滚**：Commit 任一步失败，恢复备份并返回具体阶段；绝不留下"配置已写、密钥未写"的半状态。

## 4. 请求链路

```
入站 /v1 | /{upstream}/v1 | /unified/v1 | /unified/{group}/v1
  → RouteResolver        解析 上游/模型、group、alias、modelMap
  → UpstreamSelector     默认上游 / 声明匹配 / 路由组有序链
  → CredentialResolver   single → store/env/file；pool → 策略选成员
  → PrivacyAdapter       按 provider 能力注入 ZDR/training 字段
  → TransportAdapter     openai/anthropic/commandcode/opencode/zcode/...
  → 上游
  ← 响应 / 流
  ← 失败：未出响应头 → 标记成员冷却 → 切换下一成员/下一上游
```

**重试边界**：
- 连接失败、429、5xx：可重试；流式在未发送响应头前可重试。
- 4xx（除 429）：不重试，直接返回。
- 池成员熔断：连续失败达 `failure_threshold`，冷却 `cooldown_seconds`；冷却结束半开一次。
- 上游切换：沿用现有 `groups` 有序链；不改变现有语义。

## 5. 后端模块规划

```
rust/model-gate/src/
  access/
    mod.rs            # 对外用例：list/get/create/update/delete/preview/apply
    object.rs         # AccessObject 聚合与序列化
    template.rs       # TemplateRegistry：内置 + 插件 + 用户模板
    assemble.rs       # Normalize/Validate/Bind/Commit
    diagnostics.rs    # 统一诊断码与级别
  credential/
    mod.rs
    pool.rs           # 池 CRUD、策略、选择算法、冷却
    resolver.rs       # 请求期解析（single/pool/legacy）
    legacy.rs         # zcode_accounts 适配
  ai_api/
    mod.rs            # manifest/schema/templates/inspect/plan/apply/audit
    redact.rs         # 统一脱敏
    plan.rs           # Plan 存储（内存 + 短期 TTL）
  plugin/
    mod.rs            # 现有 plugin-system 落地
    manifest.rs
    attach.rs
```

**不新建** `config_v2.rs`：新字段解析仍进 `config.rs` 的 `normalize_upstream`，避免双解析器。

## 6. 前端模块规划

```
ui/
  index.html          # 7 个一级视图的骨架
  app.js              # 启动、路由、全局状态（沿用现有无框架方式）
  views/
    overview.js
    access.js         # 列表/卡片/详情抽屉/快速添加
    credentials.js    # 池与成员
    plugins.js
    traffic.js        # 用量 + 日志
    observe.js        # 提示词 + 诊断
    settings.js
  components/
    drawer.js modal.js table.js badge.js toast.js
  style.css
```

首版不引入框架，继续原生 JS + 模板字符串，降低打包与回归风险；组件以函数返回 HTML 片段。

## 7. 兼容层

| 旧对象 | 映射 |
|---|---|
| 旧 `upstreams.<name>` 无 `kind/template` | `kind=custom`，`template` 由现有 profile 判定逻辑推导 |
| 旧 `compat.profile` | 继续生效；同时作为 `template` 的兜底 |
| 旧 `apiKeyStore` | `auth.mode=single` |
| `zcode` 上游 + `zcode_accounts` | `auth.mode=pool`，`poolId=legacy_zcode`，成员动态映射 |
| 免费来源 attach 生成的上游 | `kind=free`，`source.pluginId` 记录来源插件 |
| `/api/config` 直接读写 | 保留；内部走同一 Validator，但允许原始 JSON 高级编辑 |

## 8. 可观测与诊断

统一诊断结构：

```jsonc
{ "code": "pool_member_cooling", "level": "warn", "upstream": "zcode",
  "message": "账号 #3 冷却中，剩余 42s", "fix": "等待冷却或禁用该成员" }
```

诊断码分区：`config.*`、`template.*`、`credential.*`、`pool.*`、`plugin.*`、`route.*`。面板"接入"页顶部只显示阻断级（error），其余进详情抽屉。

## 9. 非目标

- 不重写代理内核，不改协议转换与流式实现。
- 不引入 ORM、不引入前端框架、不做多用户权限系统。
- 不改变 `/v1` 的响应格式与错误码。
