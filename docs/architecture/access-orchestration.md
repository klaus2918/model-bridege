# 接入编排、凭据池与 AI 配置接口（v3 实现说明）

> 对应设计：`docs/design/refactor-2026/`。本文记录**已落地实现**的边界、契约与验证口径，
> 供后续维护与排查使用。

## 1. 目标与不可破坏的兼容约束

v3 不新建第二份配置，也不替换代理内核。所有接入（官方、自定义、免费、账号、插件）
最终仍归一为 `bridge.config.json#upstreams.<name>`。

| # | 约束 | 实现 |
|---|---|---|
| C1 | `bridge.config.json` 唯一事实源 | 新字段全部可选；未知字段保留；保存仍走备份 → 原子写 → 回读 |
| C2 | 旧调用方式可用 | `/v1`、`/{upstream}/v1`、`/unified/v1`、`/unified/{group}/v1` 未改语义 |
| C3 | SQLite 只增不删 | 新表只进 `SCHEMA` 的 `CREATE TABLE IF NOT EXISTS`；不改旧列 |
| C4 | 旧配置零迁移启动 | 新字段缺省；旧 `apiKeyStore` / `compat.profile` 继续生效 |
| C5 | 明文密钥不落配置 | 只认 env / file / store 引用；池成员只存 `secret_ref` |
| C6 | 插件安装不改配置 | 安装只写 `plugins` 表；接入必须经 `attach` |

## 2. 模块与职责

| 模块 | 职责 |
|---|---|
| `access.rs` | 模板目录、AccessObject 聚合、快速添加/自定义添加、更新、写密钥、删除、探测 |
| `credential_pool.rs` | 池/成员 CRUD、四种选择策略、冷却/半开/耗尽状态机、故障计数、ZCode legacy 映射、审计写入 |
| `plugin.rs` | manifest 校验、安装登记、attach/detach 幂等、`auth.mode=pool` 建池 |
| `ai_api.rs` | AI manifest/schema/templates/inspect/plan/apply/audit/settings、统一脱敏 |
| `config.rs` | 新可选字段解析、顶层 `privacy` 覆盖、诊断白名单 |
| `config_store.rs` | 既有备份/合并/原子写，未改语义 |
| `db.rs` | 新表：`credential_pools`、`credential_members`、`plugins`、`config_audit` |
| `proxy.rs` | 新路由；请求链路按池成员展开候选并做成员级故障转移 |

## 3. 新增配置字段（全部可选）

| 字段 | 类型 | 说明 |
|---|---|---|
| `template` | string | 模板 id；旧 `compat.profile` 仍作为兜底 |
| `templateVersion` | number | 模板版本；仅提示 |
| `kind` | string | `official/custom/free/account/plugin`，仅展示与筛选 |
| `label` | string | 展示名，缺省用上游名 |
| `privacy` | object | `{zdr: bool, training: "allow"\|"deny"}`；同时兼容 `compat.privacy` |
| `credentialPool` | string | 池 id；存在时请求期优先从池选成员 |

`UPSTREAM_KEYS` 已包含上述字段与 `apiKeyStore`，不会再产生 `unknown_field` 噪音。

隐私的诚实表达：

- 模板默认**勾选** ZDR（记录用户意图）；
- 是否真正生效由 `privacy.support` 决定（`request` / `account` / `none`）；
- 不支持请求级的厂商只写意图，不发送伪造头，并在接入详情与诊断中说明。

## 4. 接入装配与保存管线

```
意图 {templateId,name,protocol,baseUrl,apiKey?,privacy?}
  → 模板解析（official/custom）
  → 归一化（协议、地址、隐私、kind/template/label）
  → 校验（名称、协议、地址、重名、必填）
  → 凭据写入（仅本机加密库；配置只落 apiKeyStore）
  → 备份 → 原子写 → 回读校验 → 热加载
```

关键行为：

- 官方快速添加：模板已提供协议/地址/鉴权/隐私默认值，用户只补 Key。
- 自定义添加：最少 5 项（标识、厂商、地址、协议、Key）。
- 写密钥时删除 `apiKeyEnv/file/agent`，避免 env 优先级压过密钥库。
- 配置写失败会删除刚写入的密钥（新增场景），不留半状态。
- 删除接入默认保留密钥；密钥清理需显式操作。

## 5. 凭据池

### 5.1 表

```sql
credential_pools(id, upstream, plugin_id, strategy, status,
                 cooldown_seconds, failure_threshold, fallback_to_single,
                 created_at, updated_at)
credential_members(id, pool_id, label, kind, secret_ref,
                   priority, weight, status, cooldown_until,
                   failure_count, quota_json, last_used_at, usage_json)
```

`secret_ref` 只存引用：

- `store:<id>`：本机 DPAPI 密钥库条目；
- `legacy:zcode:<account_id>`：ZCode 旧账号表映射，不复制密文。

### 5.2 状态机

`active → cooling（失败达阈值）→ half_open（冷却结束）→ active`；
`exhausted`（配额窗口耗尽）惰性恢复；`disabled` 人工停用；`needs_secret` 解不开密钥。

### 5.3 选择策略

| 策略 | 规则 |
|---|---|
| `priority`（默认） | 可用成员按 priority 升序，其次 last_used_at 最早 |
| `round_robin` | 可用成员按 last_used_at 最早轮转 |
| `weighted` | 按 weight 平滑轮转 |
| `least_used` | 窗口内请求数最少（以 last_used_at 兜底） |

### 5.4 请求链路

- 绑定池的上游在候选链中展开为**每个可用成员一个候选**（单请求最多 5 个）；
- 429/5xx/连接失败：标记该成员失败，冷却或熔断，自动切换下一成员；
- 池全部不可用：返回 `503 pool_unavailable`；默认**不回落**单 Key；
  仅池声明 `fallback_to_single` 时回落；
- 流式请求只在未发送响应头前重试。

已实测：`key1=429 -> key2=200`，`x-bridge-route-chain` 记录成员标签，
失败成员进入 `cooling`。

## 6. 插件

manifest（`modelgate.plugin/v1`）关键段：

```jsonc
{
  "id": "example.provider",
  "kind": "provider",
  "tier": "declarative",
  "auth": { "mode": "pool", "pool": { "strategy": "priority" } },
  "access": { "requiredFields": ["apiKey"], "privacy": { "zdrDefault": true } },
  "upstream": { "protocol": "openai", "baseUrl": "https://api.example.com/v1" }
}
```

- manifest 任意层级出现 `apiKey/token/secret/password/cookie/authorization` 一律拒绝；
- `auth.mode=pool` 时 upstream 模板不得声明密钥来源；
- 安装只写 `plugins` 表；`attach` 才创建/绑定上游，已存在同名同来源返回幂等结果；
- `detach` 默认保留上游与凭据，`purge` 才删除上游。

## 7. AI 配置接口

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/ai/manifest` | 能力、权限、脱敏规则、动作清单 |
| GET | `/api/ai/schema` | AccessDraft / PoolDraft / Plan / ApplyRequest |
| GET | `/api/ai/templates` | 模板目录（含隐私能力、必填、高级字段） |
| GET | `/api/ai/inspect` | 脱敏现状（上游、凭据状态、池计数、诊断） |
| POST | `/api/ai/plan` | 生成变更计划（内存 TTL 10 分钟，不落盘） |
| POST | `/api/ai/apply` | 应用计划（一次性 confirmation + revision 校验） |
| GET | `/api/ai/audit` | 审计列表（只含字段路径） |
| GET/PUT | `/api/ai/settings` | enabled / allowApply / allowPoolStats |
| GET | `/llms.txt` | 面向 AI 的纯文本说明书 |

安全规则：

- 响应经过递归 `redact`，密钥字段直接省略；
- `plan` 的 `missing` 明确写出「apiKey 需人工填写」；
- 含密钥的 plan `status=needs_secret`，apply 返回 `409 needs_secret`；
- AI actor 默认无 apply 权限；面板 actor=ui 或设置 `allowApply=true` 才可应用；
- 审计只记录 actor / action / target / changedPaths / result，不记录值。

## 8. UI

- 唯一入口：`/ui/`（`v3.html + v3.css + v3.js + v3-*.js`）；
- `/ui/classic` 仅返回 308 永久跳转到 `/ui/`，旧面板资产已移除；
- 侧栏 5 个分区、12 个子视图：总览 / 全部接入 / 模型与定价 / 路由组 / 凭据池 /
  插件 / 免费来源 / ZCode / 用量分析 / 提示词 / 运行日志 / 设置；
- 接入详情使用右侧抽屉，子页签：概览 / 连接 / 凭据 / 模型 / 高级 / 诊断；
- 「模型」页可维护模型、别名、模型级覆盖与目录行为；「高级」页可维护 `compat`、
  上游超时与会话头；设置页提供保存前校验、备份开关与配置/数据库路径；
- 快速添加为两步向导（官方）或 5 项表单（自定义）；
- 所有密钥输入为 `type=password`，保存后清空且不回显。

实际截图：`docs/design/refactor-2026/screenshots-v3/`。

## 9. 诊断码

| 码 | 含义 |
|---|---|
| `privacy_unsupported` | 勾选 ZDR 但厂商不支持请求级落地 |
| `store_key_unreadable` | 配置引用了密钥库但读不到值 |
| `field_not_applicable` | 字段对该协议无效（如非 CommandCode 的 `pauseTurn`） |
| `pool_unavailable`（运行时 503） | 池无可用成员 |
| `needs_secret`（plan/apply） | 计划需要人工补密钥 |

## 10. 验证

```powershell
cd rust\model-gate
cargo test                      # 334 单元测试 + 集成/契约套件
node --check ui\v3.js           # UI 脚本语法

# 启动临时实例后
node scripts\ui_smoke_v3.js --base http://127.0.0.1:8912 `
  --screenshot-dir docs\design\refactor-2026\screenshots-v3
```

已核对：

- `/ui/` 加载 v3、`/ui/classic` 返回 308 -> `/ui/`；
- `/api/access/templates` 15 个模板；`/api/pools`、`/api/plugins` 可读；
- `/api/ai/inspect` 不含 `sk-` / `secretRef` / `secret`；
- `/v1/models`、`/unified/v1/models`、`/demo/v1/models` 均正常；
- v3 smoke：16 通过、0 失败（含抽屉 6 页签、设置校验、旧路径跳转）。
