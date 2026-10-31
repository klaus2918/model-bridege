# 03 插件与 Token 池

> 前置阅读：`docs/architecture/plugin-system.md`。本文不重复其三层职责与五条规则，只补"插件如何声明池""池如何工作""如何与旧账号池共存"。

## 1. 插件声明池

### 1.1 manifest 增量

```jsonc
{
  "schema": "modelgate.plugin/v1",
  "id": "modelgate.provider.example",
  "kind": "provider",              // free | account | provider
  "tier": "declarative",           // declarative | native | sidecar

  "auth": {
    "mode": "pool",                // none | single | managed | pool
    "schemes": ["bearer"],
    "pool": {
      "strategy": "priority",
      "memberKind": "api_key",     // api_key | account | oauth
      "minMembers": 1,
      "cooldownSeconds": 60,
      "failureThreshold": 3,
      "quota": { "field": "dailyTokens", "default": 1000000 }
    }
  },

  "access": {
    "requiredFields": ["apiKey"],
    "advancedFields": ["modelMap", "modelOverrides", "sessionHeader"],
    "privacy": { "support": "request", "zdrDefault": true }
  },

  "upstream": {
    "protocol": "openai",
    "baseUrl": "https://api.example.com/v1",
    "compat": { "profile": "example" }
  }
}
```

约束：
- `auth.mode=pool` 时，`upstream` 模板**不得**写 `apiKey`/`apiKeyStore`/`apiKeyEnv`；由内核在 attach 时补 `credentialPool`。
- `pool` 只能声明默认值，实际策略以用户配置为准；插件不能强制。
- `requiredFields` 只能声明"用户必须提供"的字段，不能把高级字段设为必填。
- manifest 仍禁止任何密钥字段（`apiKey`/`token`/`secret`）。

### 1.2 attach 行为

```
安装插件      → 只写 plugins 两行，不改配置
attach(plugin, name)
  1. 取 manifest.upstream 模板
  2. auth.mode=pool → 创建池（若不存在）→ 生成 upstream { credentialPool: poolId }
     auth.mode=single → 生成 upstream { apiKeyStore: upstream:<name> }
     auth.mode=none → 生成 upstream，不写凭据
  3. 合并到 bridge.config.json（备份 → 原子写 → 回读）
  4. 返回 AccessObject + 待补充凭据提示
detach(plugin, name)
  → 默认保留 upstream 与池；仅解绑插件来源标记
  → `--purge` 才删除 upstream（需二次确认，不删凭据）
```

**幂等**：`attach` 已存在同名同模板时返回 `already_attached`，不重复写。`409` 返回现有对象与 diff。

## 2. Token 池模型

### 2.1 为什么需要

现有凭据形态：

| 形态 | 位置 | 能力 |
|---|---|---|
| 单 Key | `upstream.apiKeyStore` | 一把 key，无轮换 |
| 逐模型 Key | `upstream.modelKeys` | 按模型分 key，无池 |
| ZCode 账号 | `zcode_accounts` | 有领取/状态，但只能 ZCode 用 |
| 免费来源 | 无密钥 | 不适用 |

插件化后，任何 `auth.mode=pool` 的上游都需要同一套：多成员、轮换、冷却、配额、故障转移、脱敏展示。

### 2.2 状态机

```
active ──失败达阈值──► cooling ──冷却结束──► half_open
  ▲                       │                     │
  └────成功一次───────────┴────失败─────────────┘
  │
  ├──配额耗尽──► exhausted（按配额窗口重置）
  └──用户禁用──► disabled（不自动恢复）
```

| 状态 | 可被选中 | 说明 |
|---|---|---|
| `active` | ✅ | 正常 |
| `cooling` | ❌ | 冷却中，显示剩余秒数 |
| `half_open` | ✅（仅探测） | 冷却结束后的第一次尝试 |
| `exhausted` | ❌ | 配额耗尽，等待窗口重置 |
| `disabled` | ❌ | 用户手动禁用 |

### 2.3 选择算法

输入：`poolId`、请求模型、当前时间、排除集合（本次已失败成员）。
输出：成员 + 选择原因（用于日志/诊断）。

| 策略 | 规则 |
|---|---|
| `priority`（默认） | 过滤可用 → 按 priority 升序（数值小优先）→ 同优先级按 last_used_at 最早 |
| `round_robin` | 过滤可用 → 按 last_used_at 最早轮转 |
| `weighted` | 过滤可用 → 平滑加权轮询（weight） |
| `least_used` | 过滤可用 → 按窗口内请求数最少 |

**排除集合**保证单次请求不会重复选择同一失败成员。全池不可用时：
1. 返回 `503 pool_unavailable`，附脱敏原因（全部冷却/全部禁用/全部配额耗尽）；
2. 若该上游还有 `apiKeyStore` 兜底，按配置决定是否回落（默认**不回落**，避免误用错误凭据）。

### 2.4 故障转移

```
请求 → 选成员 m1 → 上游 429/5xx/连接失败
  → 标记 m1 失败（failure_count+1）
  → 达阈值 → cooling(60s)
  → 排除 m1，重选 m2（最多 N 次，N=池大小，上限 5）
  → 全部失败 → 503 / 切换下一上游（若在 groups 中）
```

**不重试**：400/401/403/404/422（配置或权限问题，换成员通常无效；401/403 仍标记该成员失败）。
**流式**：仅在未发送响应头前重试；已发送则终止并记录。

### 2.5 配额

`quota_json` 结构：

```jsonc
{ "dailyTokens": 1000000, "dailyRequests": 500, "resetAt": "2026-11-01T00:00:00+08:00" }
```

- 计数写入 `usage_json`（按窗口）；内核在每次成功后累加；
- 请求前检查，耗尽置 `exhausted`；
- 窗口重置由后台任务（每小时）执行；无后台任务时惰性重置。

### 2.6 与 ZCode 兼容

```
credential_pools: legacy_zcode (plugin_id=modelgate.account.zcode, strategy=priority)
credential_members:
  legacy:zcode:<account_id> → secret_ref="legacy:zcode:<id>"，priority 取账号优先级
```

- 不复制 `zcode_accounts` 的密文；读取时仍走原表解密路径；
- 领取/验证码/状态仍由 ZCode 面板管理；
- 账号新增/删除时，通过 legacy adapter 同步成员行（触发器式，在 API 层完成，不用 DB trigger）；
- 池状态（冷却/配额）可与 ZCode 现有状态合并展示，但两者不双写：以 ZCode 表为准，池行只做缓存。

## 3. 数据库

```sql
-- 内核启动时随 SCHEMA 一次建好；安装插件不执行 DDL
CREATE TABLE IF NOT EXISTS credential_pools (
  id TEXT PRIMARY KEY,
  upstream TEXT NOT NULL,
  plugin_id TEXT,
  strategy TEXT NOT NULL DEFAULT 'priority',
  status TEXT NOT NULL DEFAULT 'active',
  cooldown_seconds INTEGER NOT NULL DEFAULT 60,
  failure_threshold INTEGER NOT NULL DEFAULT 3,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_credential_pools_upstream ON credential_pools(upstream);

CREATE TABLE IF NOT EXISTS credential_members (
  id TEXT PRIMARY KEY,
  pool_id TEXT NOT NULL,
  label TEXT NOT NULL,
  kind TEXT NOT NULL,
  secret_ref TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 100,
  weight INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL DEFAULT 'active',
  cooldown_until TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0,
  quota_json TEXT,
  last_used_at TEXT,
  usage_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_credential_members_pool ON credential_members(pool_id);
```

`failure_count` 与 `cooldown_until` 是热状态，允许进程内缓存；启动时从表恢复。

## 4. API

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/pools` | 池列表 + 脱敏统计（成员数/可用/冷却/配额） |
| POST | `/api/pools` | 创建池（upstream、strategy、阈值） |
| GET | `/api/pools/{id}` | 池详情 + 成员（**不含密钥**，只含 label/status/priority/last4?） |
| PATCH | `/api/pools/{id}` | 改策略/阈值/状态 |
| DELETE | `/api/pools/{id}` | 默认仅停用；`?purge=1` 才删 |
| POST | `/api/pools/{id}/members` | 添加成员；密钥走 `secret` 字段，仅本机回环 |
| PATCH | `/api/pools/{id}/members/{mid}` | 改 label/priority/weight/status/quota |
| DELETE | `/api/pools/{id}/members/{mid}` | 删除成员，默认保留 secret |
| POST | `/api/pools/{id}/probe` | 逐个探测（可选模型），返回脱敏结果 |

**脱敏**：成员响应只有 `id/label/kind/status/priority/weight/cooldownUntil/quota/lastUsedAt/last4`；`last4` 可选，默认关闭。

## 5. UI

「凭据」一级页：

- 顶部：池卡片（上游、策略、成员可用数、今日请求、异常数）；
- 展开：成员表（标签、类型、状态、优先级、冷却、配额、最近使用）；
- 操作：添加成员（弹窗只填标签 + 密钥）、禁用/启用、调优先级、探测；
- 池设置抽屉：策略、冷却秒数、失败阈值、是否允许回落单 Key；
- 空状态：引导"从接入页绑定池"。

接入页上游行显示 `凭据：池 3/4 可用` 或 `凭据：单 Key`。

## 6. 安全边界

- 池成员密钥只能通过本机 UI 或本机回环 API 写入；AI 接口只能创建"待补充"成员（无 secret）。
- 列表/详情/日志/审计均不返回密钥；`last4` 默认关闭。
- 导出配置不导出密钥库；导入不含密钥的配置时，池成员标记 `needs_secret`。
- 删除池/成员默认不删密钥；清理密钥需二次确认并单独审计。

## 7. 测试

| 用例 | 期望 |
|---|---|
| priority 选择 | 取最高优先级可用成员 |
| 429 后切换 | 第一次失败→第二次成功；失败成员进入冷却 |
| 连续失败熔断 | 达阈值后冷却 60s；冷却结束 half_open |
| 全池冷却 | 503 `pool_unavailable`，不误用单 Key |
| 配额耗尽 | 成员 exhausted，不再被选中；窗口重置恢复 |
| ZCode legacy | 账号池请求正常；新增账号后成员同步；不复制密文 |
| 并发选择 | 同一成员不被并发请求同时"轮询"到超出权重 |
| 脱敏 | 所有响应无密钥；日志无密钥 |
