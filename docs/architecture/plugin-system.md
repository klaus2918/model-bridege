# 插件系统与插件市场

> **一句话边界**：市场管**分发**，插件管**适配**，内核管**网关**。
>
> 本文设计的是「上游接入的插件化」。**插件化不改变各来源现有的配置形态**——
> 免费来源仍是 `free_sources.json` 的那套字段，ZCode 仍是 `data/zcode.json` + 两张账号表；
> 插件化只统一它们的**登记方式**、**接入动作**与**生命周期**。
>
> 关联文档：[`upstream-config-model.md`](upstream-config-model.md)、[`free-sources.md`](free-sources.md)、[`provider-compat.md`](provider-compat.md)。
> 对应变更：`.op/changes/plugin-marketplace/`。

---

> **历史注（2026-11-04）**：ZCode 账号池插件已在 v4.5.0 移除（上游风控拦截非官方客户端）。
> 本文以 ZCode 为参考例的段落仅作历史设计记录；`legacy:zcode` 映射与 `data/zcode.json` 不再使用。
## 0. 结论先行

```
┌──────────────────────────────────────────────────────────────┐
│ 插件市场 Marketplace                                          │
│   official（随包）/ remote（可配置索引）/ local（导入包）        │
│   只做：发现 → 下载 → 校验 → 安装 → 更新 → 回滚 → 卸载          │
│   ⚠ 安装/卸载不执行任何 DDL，不改 bridge.config.json            │
└───────────────────────────┬──────────────────────────────────┘
                            │ 产出：data/plugins/<id>/<version>/ + plugins 表两行
┌───────────────────────────▼──────────────────────────────────┐
│ 插件 Plugin（上游接入包）                                      │
│   manifest 只描述「接什么、怎么连、要什么配置块、要什么权限」     │
│   不接管配置存储，不接管 UI，不接管数据表                        │
└───────────────────────────┬──────────────────────────────────┘
                            │ 接入动作：模板 → upstreams.<name>
┌───────────────────────────▼──────────────────────────────────┐
│ 内核 Core（网关，不动的部分）                                   │
│   入站协议 · 路由与故障切换 · 模型清单 · 鉴权注入                │
│   用量/日志/诊断 · bridge.config.json 唯一事实源 · DB 稳态表     │
└──────────────────────────────────────────────────────────────┘
```

五条不可越界的规则：

| # | 规则 | 为什么 |
|---|---|---|
| R1 | **插件安装/卸载不执行 DDL** | 所有插件相关表由内核启动时随 `SCHEMA` 建好；装插件只是写数据行 |
| R2 | **插件不写 L1 稳态表、不直接写 L2 配置** | 配置只能经内核的 `attach`/`detach` 动作改，保证备份、校验、回读 |
| R3 | **插件不接管自己的配置存储** | `data/zcode.json` 还是它自己；manifest 只声明「我需要这个配置块」 |
| R4 | **卸载默认保留用户资产与上游条目** | 账号、`modelMap`、模型选择都不是插件安装包的一部分 |
| R5 | **native 插件不可卸载，只能禁用** | 实现编译在内核里，删文件没有意义；禁用即停用 |

---

## 1. 现状与约束（已核对代码）

### 1.1 现有持久化清单

| 数据 | 位置 | 现状 | 插件化后 |
|---|---|---|---|
| 上游条目（baseUrl / compat / modelOverrides / modelKeys） | `bridge.config.json#upstreams` | **唯一事实源**，备份 + 原子写 + 回读 | 不变 |
| 顶层 `models`、全局开关 | `bridge.config.json` | 用户意图 | 不变 |
| Provider 预设（14 套） | `provider_profiles.json`（`include_str!`） | 只读代码资产 | 转成内置插件清单（只读） |
| 免费来源定义（3 个） | `free_sources.json`（`include_str!`） | 只读代码资产 | 转成内置插件清单（只读） |
| 免费探测缓存 | 进程内 `free_probe_cache` | 易失，重启即空 | **不变**，不落库 |
| ZCode 插件配置 | `data/zcode.json`（`zcode/config.rs:108`） | 独立文件，`save_atomic` | **不变** |
| ZCode 账号 / 领取 | `modelgate.db#zcode_accounts` / `#zcode_claims` | 用户资产（DPAPI 密文） | **不变**，登记为 legacy 私有表 |
| 用量 / 提示词 / 设置 | `modelgate.db#usage_records` / `#prompt_records` / `#settings` | 稳态 | **不变** |
| Schema 迁移 | `SCHEMA`（`CREATE TABLE IF NOT EXISTS`）+ `MIGRATIONS`（`PRAGMA table_info` 后加列） | 幂等、只增不减 | 新增表只进 `SCHEMA`，**不碰 `MIGRATIONS`** |

### 1.2 现有接入动作的真实行为

| 来源 | 接入做了什么 | 有没有 DB |
|---|---|---|
| 免费来源（zen / kilo） | `build_attach()` 生成上游 → 合并 → 备份配置 → `save_atomic` → 回读 | ❌ 无 |
| ZCode（账号型） | `build_attach()` 生成上游（`auth:none`、固定模型、`autoModels:false`）；账号另行导入 | ✅ 只写 `zcode_accounts` / `zcode_claims` |
| Provider 预设 | 面板「只填空字段」填充协议/地址/变量名，用户保存配置 | ❌ 无 |

**结论**：三类来源的接入**已经收敛在同一层**（`upstreams.<name>`），差异只在模板内容与凭据来源。
插件化要复用的正是这条既有路径，而不是另造一条。

### 1.3 必须守住的三个"稳态"

1. **配置稳态**：`bridge.config.json` 的字段与合并策略不变——未知字段保留、`merge_incoming` 语义不变、备份策略不变。
2. **数据库稳态**：`SCHEMA` 只追加 `CREATE TABLE IF NOT EXISTS`；`MIGRATIONS` 只允许加列；**不允许重建表、改列类型、删列**。
3. **来源配置稳态**：`free_sources.json` / `provider_profiles.json` / `data/zcode.json` 的字段一个不删、语义不变。

---

## 2. 三层职责

| 层 | 负责 | 明确不做 |
|---|---|---|
| **内核** | 入站协议；模型解析与 `modelMap`；上游选择与故障切换；鉴权注入；`/v1/models` 合并策略；用量/日志/诊断；配置读写与备份；**数据库 schema 与迁移** | 不内置厂商 URL/头/路径（从插件读） |
| **插件** | 声明上游模板与能力；声明免费判定/探测参数/隐私告知；声明需要哪个配置块与哪种面板；`native` 时提供 transport 实现 | 不改路由；不写配置；不建表；不读密钥值；不注入任意 UI |
| **市场** | 索引聚合；下载与校验；安装/更新/回滚/卸载；版本兼容判定；签名信任链 | 不启用上游；不改配置；不代持凭据；**不做 DDL** |

---

## 3. 插件分级

等级只回答「需不需要代码」：

| 等级 | 形态 | 可卸载 | 首版 | 典型 |
|---|---|---|---|---|
| **declarative** | 纯 `plugin.json` | ✅ | ✅ 开放 | `zen-free`、`kilo-free`、14 套 Provider 预设 |
| **native** | manifest + 内核内 Rust transport | ❌ 只能禁用 | ✅ 仅官方 | `opencode` 指纹传输、`zcode` 账号池 |
| **sidecar** | manifest + 进程外可执行体 | ✅ | ⛔ 只留接口 | 社区插件（后置） |

同 schema 覆盖三级，差异只在 `runtime`。**市场永远不下载 native 实现**，只安装它的 manifest。

---

## 4. 插件清单 `plugin.json`

### 4.1 契约要点

`schema: "modelgate.plugin/v1"`。**manifest 是"登记表"，不是"配置文件"**：

- 只描述**接什么、怎么连、需要什么**；
- 不复制 `data/zcode.json` 的字段，不复制 `freeMatch`/`probe` 之外的运行时状态；
- 不允许出现密钥字段（`apiKey` / `token` / `secret`），只允许 `apiKeyEnv` 变量名。

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `schema` | string | ✅ | 固定 `modelgate.plugin/v1` |
| `id` | string | ✅ | 反域名；官方 `modelgate.*` |
| `name` / `description` / `version` / `vendor` | string | ✅ | 展示与版本 |
| `kind` | enum | ✅ | `free` / `account` / `provider` |
| `tier` | enum | ✅ | `declarative` / `native` / `sidecar` |
| `minCoreVersion` | semver | ✅ | 双向校验（市场侧 + 内核侧） |
| `auth` | object | ✅ | `mode: none \| managed \| env` |
| `upstream` | object | ✅ | 上游模板，与 `upstreams.<name>` 同构（不含密钥） |
| `catalog` / `freeMatch` / `probe` | object | ⬜ | 沿用现有 `free_sources.json` 的字段语义，1:1 平移 |
| `config` | object | ⬜ | **需要哪个配置块**：`{ "file": "data/zcode.json", "schema": "...", "panel": "zcode" }`；只声明，不托管 |
| `runtime` | object | ⬜ | `transport: <id>` / `scheduler` / `sidecar` |
| `dataVersion` | number | ⬜ | `plugin_kv` 数据的 schema 版本；变化只记事件，内核不做自动迁移 |
| `permissions` | object | ⬜ | 仅 native/sidecar，见 §10 |
| `market` | object | ⬜ | `tags` / `privacy` / `docsUrl` / `icon` |
| `aliases` | string[] | ⬜ | 兼容旧 `transport` 字符串 |

### 4.2 ZCode 清单示例

```jsonc
{
  "schema": "modelgate.plugin/v1",
  "id": "modelgate.account.zcode",
  "name": "ZCode Start Plan（GLM 账号池）",
  "version": "1.0.0",
  "vendor": "modelgate",
  "kind": "account",
  "tier": "native",
  "minCoreVersion": "3.0.0",

  "auth": { "mode": "managed" },

  "upstream": {
    "protocol": "anthropic",
    "transport": "zcode",
    "baseUrl": "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
    "sessionHeader": false,
    "fetchModels": false,
    "autoModels": false,
    "compat": { "auth": { "scheme": "none" }, "endpoint": { "chat": "/v1/messages", "models": "" } }
  },

  "catalog": { "shape": "fixed", "fixedIds": ["GLM-5.3-Flash", "GLM-5.3"] },
  "probe":   { "mode": "chat", "minMaxTokens": 8, "prompt": "ping" },

  // 只声明配置块归属：文件、schema、面板入口。字段不复制进来。
  "config": {
    "file": "data/zcode.json",
    "schema": "modelgate.config.zcode/v1",
    "panel": "zcode"
  },

  "runtime": { "transport": "zcode", "scheduler": "zcode.claim" },

  "permissions": {
    "process":    { "spawn": ["node"] },
    "filesystem": { "read": ["%LOCALAPPDATA%/ZCode"], "write": ["%APPDATA%/Model-Gate/zcode"] },
    "network":    { "hosts": ["zcode.z.ai", "127.0.0.1"] },
    "browser":    { "headless": true }
  },

  "aliases": ["zcode"],
  "market": {
    "tags": ["免费额度", "账号池", "GLM"],
    "privacy": { "level": "warn", "note": "账号型免费额度：请求经本机 ZCode CLI worker 转发，不发送到第三方" }
  }
}
```

### 4.3 与现有数据的关系

| 现有 | 迁移后 | 说明 |
|---|---|---|
| `free_sources.json#sources.zen-free` | `modelgate.free.zen` | `freeMatch` / `catalog` / `probe` / `privacy` 1:1 平移 |
| `free_sources.json#sources.kilo-free` | `modelgate.free.kilo` | 同上 |
| `free_sources.json#sources.zcode` | `modelgate.account.zcode` | 同上 + `runtime` + `config` 声明 |
| `free_sources.json#rejectReasons` / `#states` | **留在内核** | 全来源共用的归一化词表，不属于任何插件 |
| `provider_profiles.json#profiles.<id>` | `modelgate.provider.<id>` | `access` / `compat` / `detect` 平移 |
| `data/zcode.json` | **原文件不变** | manifest 只声明 `config.file` |
| `zcode_accounts` / `zcode_claims` | **原表不变** | 登记为 legacy 私有表（§5.3） |

**生成而非手写**：P0 用一次性脚本从两份 JSON 生成内置清单，golden 测试保证「生成 → 解析 → attach」输出与当前 `build_attach()` **逐字节一致**。

---

## 5. 数据归属：四层模型

这是本设计的核心。**"现有的表是稳态的"不靠约定，靠结构保证**。

```
L1 内核稳态      modelgate.db: usage_records / prompt_records / settings
                 └─ 插件永不触碰，永不为插件改结构
L2 网关配置      bridge.config.json: upstreams / models / 全局开关
                 └─ 只有内核能写；插件通过 attach/detach 间接表达意图
L3 插件注册表    modelgate.db: plugins / plugin_events
                 └─ 内核写；插件只读自己的状态
L4 插件数据      4a plugin_kv（命名空间 KV） / 4b legacy 私有表白名单
                 └─ 插件通过受控 API 读写；卸载按策略处理
```

### 5.1 分层规则

| 规则 | 内容 | 落地 |
|---|---|---|
| **R1** | 插件安装/卸载**不执行任何 DDL** | 三张插件表随内核 `SCHEMA` 建好；安装只 INSERT |
| **R2** | 插件**不直接写** L1/L2 | 配置只能经 `attach`/`detach`（备份 + 校验 + 回读）；L1 无写接口 |
| **R3** | 插件数据必须带 **namespace** 与配额 | `plugin_kv.ns = plugin id`；单插件上限 1 MiB / 1000 键 |
| **R4** | legacy 表**白名单**，仅内置插件可用 | 只有 `zcode_accounts` / `zcode_claims` 在册，由 `src/zcode/store.rs` 直接管理 |
| **R5** | 卸载**不动 L1**，默认保留 L2/L4 | 见 §8.3 卸载矩阵 |
| **R6** | 所有写操作**单事务 + 事件** | 每个动作一条 `plugin_events`，失败可重试 |

### 5.2 为什么不让插件自己建表

| 问题 | 后果 |
|---|---|
| 表名冲突 | 两个插件都建 `accounts` |
| 僵尸表 | 卸载后无法安全 DROP（可能被别的东西引用） |
| 迁移不可逆 | 现有 `migrate()` 只会 `ADD COLUMN`，没有重建能力 |
| 备份/导出不可枚举 | `usage_export`、诊断、数据目录迁移都要知道有哪些表 |
| SQLite 的限制 | `ALTER TABLE` 能力弱，改列类型要重建表，风险高 |

因此：**插件一律不得执行 DDL**。需要结构化存储就用 `plugin_kv` 的 JSON 值；确实需要索引的，由内核在评审后加表，作为内核资产发布。

### 5.3 legacy 私有表

ZCode 的两张表是**存量资产**，不迁移、不改名：

| 表 | 内容 | 归属 | 卸载策略 |
|---|---|---|---|
| `zcode_accounts` | 账号、DPAPI 密文 JWT、状态、冷却、指纹 | ZCode 插件（legacy） | **永不删除**；用户显式「清空账号」才删 |
| `zcode_claims` | 领取记录、业务码、额度 | ZCode 插件（legacy） | 同上 |

它们在 `plugins` 表里以 `legacy_tables: ["zcode_accounts","zcode_claims"]` 登记，
仅用于**卸载时的风险提示**与**数据导出**，不参与通用 KV 机制。

### 5.4 `plugin_kv` 的版本、配额与迁移责任

`plugin_kv` 是**无 schema 的 JSON 值**，内核不做任何自动迁移：

| 事项 | 规则 |
|---|---|
| schema 演进 | 由插件自己负责。manifest 声明 `dataVersion`，变化时内核只记 `plugin_data_version_changed` 事件，不改数据 |
| 向后兼容 | `declarative` 插件没有代码，必须**读旧值有默认**；`native` 插件在加载时自行迁移 |
| 配额 | 单插件 1 MiB / 1000 键；超限写失败 `plugin_kv_quota_exceeded`，不静默截断 |
| 连接 | 复用 `Db` 的写连接（现有 `Mutex` 串行化），**不新开连接**，避免与用量写入争锁 |
| 清理 | `purge_data=true` 前先把该 namespace 导出到 `data/plugin-trash/<id>-<ts>.json`，再删除 |

---

## 6. 接入层面：模板 / 配置 / 运行

「接入」被拆成三个明确层次，每一层的输入输出都可验证：

### 6.1 三层定义

| 层 | 输入 | 输出 | 谁执行 | 失败表现 |
|---|---|---|---|---|
| **模板层** | `plugin.json#upstream` | 只读模板对象 | 内核（加载时） | 插件标 `invalid`，市场可装但不可接入 |
| **配置层** | 模板 + 用户选择（上游名、模型、是否默认） | `upstreams.<name>` 写入 `bridge.config.json` | 内核（`attach`） | 返回 `plugin_upstream_conflict` / `invalid_config`，**不落盘** |
| **运行层** | `upstreams.<name>` + 请求 | HTTP 请求 / transport 调用 | `proxy.rs` + transport 注册表 | 走现有错误归一化 |

### 6.2 按 kind 的写入边界

| kind | 写 `upstreams` | 写顶层 `models` | 写凭据 | 额外写 |
|---|---|---|---|---|
| `free` | 模板 + `autoModels:false` + `sessionHeader:false` | ❌（红线：免费模型不自动进全局） | ❌ | 无（探测缓存不落盘） |
| `account` | 模板 + `auth.scheme:none` + `fetchModels:false` + 固定 `models` | ❌ | ❌（凭据在 L4b） | 无（`data/zcode.json` 由插件自己管） |
| `provider` | 模板**只填空字段**，用户已有值优先 | ❌ | 只写 `apiKeyEnv` **变量名** | 无 |

### 6.3 attach 的四条硬约束

1. **幂等**：同参数重复 attach，结果字节相同（`merge_incoming` 保留未知字段，不整段替换）。
2. **冲突可见**：同名上游已存在且来源不同 → 不静默覆盖，返回 `plugin_upstream_conflict` + 字段级 diff，需 `overwrite:true` 二次确认。
3. **可回滚**：attach 前记录 `before`（配置哈希 + 备份路径）；`detach` 只移除该上游，其余字段与顺序不动。
4. **不自动启用**：attach 完成后请求立即生效（与现状一致），但**不会**顺带把插件设为 `enabled`——
   `enabled` 只控制后台调度（如 ZCode 领取），与路由无关。

### 6.4 attach 流程（伪代码）

```
attach(plugin_id, {upstream_name?, models?, overwrite?}):
  1. 校验插件状态：installed && manifest.upstream 存在
  2. 校验 transport：is_known(manifest.upstream.transport)   // 只有 native 会声明
  3. 生成模板：build_attach(template, kind, models, top_models_empty)
  4. 读当前配置 → merge（保留未知字段）
  5. 构造 candidate = Config::from_value(merged) → diagnostics
     ├─ 有 error → 返回 400，不落盘
  6. 冲突检查：同名 upstream 已存在且 templates_differ → 无 overwrite 则返回 409
  7. backup_config(按现有策略) → save_atomic → Config::load 回读
  8. 写 plugin_events(attach)
  9. 返回 { upstream, modelsWritten, notices, configPath }
```

---

## 7. 生命周期：三态 + 两个中间态

| 状态 | 含义 | 事实源 | 动作 |
|---|---|---|---|
| **installed** | 文件已就位于 `data/plugins/<id>/<version>/` | `plugins` 表 + 磁盘 | 安装 / 更新 / 卸载 |
| **enabled** | 运行时开关（后台调度、账号池） | 插件配置（`data/zcode.json` / `plugins` 表） | 启用 / 禁用 |
| **attached** | 已写入 `upstreams.<name>` | `bridge.config.json` **（唯一事实源不变）** | 接入 / 移除 |

中间态：`installing` / `uninstalling`（用于崩溃恢复；启动时把中间态回滚为终态）。

```
                install              enable              attach
 (market) ─────────────▶ installed ─────────▶ enabled ─────────▶ attached
                            │                  │                   │
                            │ uninstall        │ disable           │ detach
                            ▼                  ▼                   ▼
                        (removed)          installed           enabled
```

规则：

1. **安装不写配置**，默认 `enabled=false`。
2. **启用不写上游**：只有 `native` 且有 `runtime.scheduler` 的插件需要这一步。
3. **接入才写 `upstreams`**，复用 `build_attach()`。
4. **禁用已接入的插件**：改写 `upstreams.<name>.disabled = true`（新增可选字段，缺省 false），**不删上游**。
   路由、`model_list`、`/v1/models` 合并、健康检查全部跳过 `disabled`。
   > 不删的理由：删除会丢用户手调的 `modelMap` / `modelOverrides`，再启用无法还原。
5. **native 插件不可卸载**：只有「禁用 / 接入 / 移除接入」，防止把内核能力删成半残。

---

## 8. 安装、升级、卸载与数据库处理

### 8.1 安装：没有 DDL，只有两行 INSERT

```
安装(.mgplugin / 内置)：
  1. 下载/读取 → 校验 sha256 → 校验签名（remote）
  2. 解压到 data/plugins/.staging/<id>-<version>/
  3. manifest 校验：schema / id 命名空间 / minCoreVersion / 无密钥字段
  4. 原子 rename → data/plugins/<id>/<version>/
  5. BEGIN
       INSERT INTO plugins   (id, version, ... , active=1)
       INSERT INTO plugin_events (action='install', ...)
     COMMIT
  6. 清理 staging
```

对数据库的全部影响：**两条 INSERT**。没有 `CREATE TABLE`、没有 `ALTER TABLE`、没有索引变更。

安装失败（任一步）→ staging 删除 + 事务未提交，`plugins` 表无记录，**可原地重试**。

**顺序是有意的：先落文件，再写库。** 反过来若先写库、rename 失败，会留下"注册表里有、磁盘上没有"的坏状态，
而 orphan 目录是惰性的、可被启动扫描回收。若文件已就位但 DB 写失败：先尝试删除该版本目录做补偿，
删不掉就保留 orphan 并记 `install_orphan_cleaned`，下次启动清理。

### 8.2 升级与回滚

| 动作 | 数据库 | 文件 | 配置 |
|---|---|---|---|
| 升级 | `plugins.active` 切到新版本 + 事件 | 保留 N-1 个版本目录 | **不改** |
| 回滚 | `active` 指回旧版本 + 事件 | 不重新下载 | **不改** |
| 模板变更冲突 | 事件记 `template_changed` | — | 只提示 + 展示 diff，**不自动改写** |

升级引入新 `permissions` 时必须重新确认；拒绝则停在旧版本。

### 8.3 卸载：状态机 + 资产矩阵

```
uninstall(plugin_id, {detach?=false, purge_data?=false}):
  1. 校验：tier=native → 拒绝（只能 disable）
  2. 若有其他插件 dependsOn → 拒绝并列出依赖方
  3. 置状态 uninstalling（可恢复）
  4. detach? 则移除 upstreams.<name>；否则标 disabled=true
  5. BEGIN
       INSERT plugin_events(action='uninstall', detail=...)
       DELETE FROM plugins WHERE id=?
     COMMIT
  6. purge_data? 则 DELETE FROM plugin_kv WHERE ns=?；否则标 retained
  7. 删除 data/plugins/<id>/ 目录
  8. 清进程内缓存
```

**卸载资产矩阵**（默认列即默认行为）：

| 资产 | 默认 | 可选 | 理由 |
|---|---|---|---|
| 插件文件 `data/plugins/<id>/` | 删除 | — | 可重新下载 |
| `plugins` 行 | 删除 | — | 注册表 |
| `plugin_events` 行 | **保留** | 按 id 清理 | 审计与排障 |
| `plugin_kv`（L4a） | **保留**，标 `retained` | `purge_data=true` 彻底清除 | 重装可恢复用户选择（如模型清单） |
| 上游条目（L2） | **保留并 `disabled=true`** | `detach=true` 移除 | 不丢 `modelMap` / `modelOverrides` |
| `data/zcode.json` | **保留** | — | 用户路径配置 |
| `zcode_accounts` / `zcode_claims` | **永不删除** | 用户显式「清空账号」 | 账号是资产，删了要重新 OAuth/导入 |
| 进程内缓存 | 清理 | — | — |

> 卸载唯一的"破坏性"操作是删插件目录；**配置、账号、上游条目默认都留着**。
> 这是刻意的：插件包是"可替换的适配器"，用户资产不是。

### 8.4 数据库变更清单（分层评估）

| 变更 | 触发时机 | 机制 | 风险 | 守卫 |
|---|---|---|---|---|
| 新增 `plugins` / `plugin_events` / `plugin_kv` | **内核升级时一次性** | 追加到 `SCHEMA` 常量（`IF NOT EXISTS`） | 低，旧库自动补表 | 启动测试：空插件目录下表存在 |
| 插件安装 | 运行时 | 2 条 INSERT | **无 DDL** | 装/卸 100 次后表数量不变 |
| 插件升级 | 运行时 | 1 条 UPDATE + 1 条 INSERT | 无 DDL | 回滚后 `active` 正确 |
| 插件卸载 | 运行时 | 1 条 DELETE + 1 条 INSERT | 无 DDL | 卸载后 L1 行数不变 |
| 插件数据清理 | 用户显式 | `DELETE FROM plugin_kv WHERE ns=?` | 不可逆 | 二次确认 + 事件 |
| `disabled` 字段 | 内核升级时 | **不加列**：`upstreams` 在 JSON 里，只加可选键 | 零 DB 风险 | 旧版 exe 忽略该键 |
| ZCode 存量表 | **不改** | 保持 `zcode_accounts` / `zcode_claims` 原样 | 零 | 升级前后 `PRAGMA table_info` 一致 |

### 8.5 必须有的守卫测试（G1–G6）

| # | 测试 | 断言 |
|---|---|---|
| G1 | 空插件目录启动（旧库升级） | 三张新表存在；`usage_records` / `prompt_records` / `zcode_*` 的列与行数**完全不变** |
| G2 | 安装 + 卸载循环 100 次 | `sqlite_master` 的表/索引数量不变（证明零 DDL 泄漏） |
| G3 | 卸载后重装 | `plugin_kv` 数据可恢复；上游条目仍为 `disabled`，不自动启用 |
| G4 | 配置往返 | `attach` → `detach` 后，`bridge.config.json` 除目标 `upstreams` 键外**字节级不变** |
| G5 | 全量回归 | `cargo test` 全绿，含现有 provider / free_sources golden |
| G6 | 崩溃恢复 | 安装/卸载中途 kill 进程后重启：`installing` 清 staging、`uninstalling` 补删、`active` 始终指向可用版本、网关正常启动 |

### 8.6 失败、并发与崩溃恢复

| 场景 | 处理 |
|---|---|
| 同一插件并发安装/卸载 | 进程内 per-id 锁 + `installing` / `uninstalling` 状态；第二个请求返回 409 `plugin_busy` |
| 文件就位、DB 写失败 | 补偿删除该版本目录；失败则留 orphan，启动扫描清理 |
| DB 已删、文件删不掉（Windows 占用） | 状态置 `uninstall_pending_cleanup`，启动重试；**不阻塞其他操作** |
| 升级中途崩溃 | 文件先写新版本目录、最后才切 `active`；崩溃时 `active` 仍指旧版本，行为不变 |
| 启动发现中间态 | `installing` → 清 staging；`uninstalling` → 补删；均记事件，不阻断启动 |
| 已 attach 但 transport 不存在 | **网关照常启动**；该上游标记 unhealthy，诊断 `plugin_transport_unknown`，请求返回明确错误 |
| manifest 损坏 / 插件目录被手工删 | 启动扫描把 `plugins` 行标 `missing`，不删除记录，面板提示重装 |

---

## 9. 插件市场

### 9.1 渠道

| 渠道 | 来源 | 默认 | 校验 |
|---|---|---|---|
| `official` | 随包内置 | ✅ | 构建产物可信 |
| `remote` | 用户配置索引 URL | ⛔ **留空** | 索引签名 + 包签名；未签名只读不可装 |
| `local` | 导入 `.mgplugin` | ✅ | 默认要求签名，可显式信任未签名包 |

同 `id` 冲突：高版本胜出，同版本 official 胜出。

### 9.2 索引与包

索引 `index.json`（`modelgate.market/v1`）列出 `id` / `latest` / `versions[]`（`version` / `minCoreVersion` / `url` / `sha256` / `size` / `signature` / `notes`）。

包 `.mgplugin` = ZIP，根目录 `plugin.json` + 可选 `README.md` / `CHANGELOG.md` / `assets/`。

**静态拒绝**（`plugin_package_forbidden_entry`）：可执行体（`.exe/.dll/.so/.node/.ps1/.sh/.bat`）、符号链接、`..` 路径穿越、解压后 > 8 MiB 或 > 128 文件。

### 9.3 离线与失败降级

| 场景 | 行为 |
|---|---|
| 无网络 | `official` / `local` 正常；`remote` 显示缓存 + 过期提示 |
| 索引失败 | 保留旧缓存，不清空 |
| 安装中断 | staging 清理，可重试，不留半装 |
| 磁盘不可写 | 拒绝并提示 `data/plugins` 路径 |

---

## 10. 安全与权限

| 权限 | 字段 | 默认 | 适用 |
|---|---|---|---|
| 网络 | `permissions.network.hosts` | `[]` | L2；L1 编译期固定 |
| 进程 | `permissions.process.spawn` | `[]` | L1/L2 |
| 文件读 | `permissions.filesystem.read` | `[]` | L1/L2 |
| 文件写 | `permissions.filesystem.write` | `[]`（仅插件数据目录） | L1/L2 |
| 浏览器 | `permissions.browser.headless` | `false` | L1/L2 |

五条红线：

1. manifest 永不出现密钥（只允许 `apiKeyEnv` 变量名）。
2. 插件拿不到其他上游的密钥；凭据由内核在请求前注入。
3. 插件不能直接写 `bridge.config.json`。
4. 市场不执行代码（首版仅 L0/L1；L1 编译期内置）。
5. 免费模型不自动进 `/v1/models`（`autoModels:false` + 不写顶层 `models`）。

---

## 11. 内核改造点

| 文件 | 改动 | 风险控制 |
|---|---|---|
| `src/plugin/mod.rs`（新增） | manifest 解析、schema 校验、内置清单加载 | 纯读，不改现有路径 |
| `src/plugin/transport.rs`（新增） | `Transport` trait + 注册表；`opencode` / `zcode` 原样注册 | P0 行为零变化 |
| `src/plugin/state.rs`（新增） | `plugins` / `plugin_events` / `plugin_kv` 读写 | 只走新增表 |
| `src/plugin/market.rs`（新增） | 索引聚合、下载、校验、staging 安装、回滚 | 无 DDL |
| `src/plugin/api.rs`（新增） | §12 端点 | — |
| `src/config.rs:1372` | `transport` 白名单 → `plugin::transport::is_known()` | 旧字符串经 `aliases` 继续有效 |
| `src/config.rs`（`Upstream`） | 新增 `disabled: bool`（缺省 false） | JSON 可选键，无 DB 变更 |
| `src/proxy.rs:783` | `if transport == "zcode"` → `plugin::transport::dispatch()` | P0 分支等价搬迁 |
| `src/proxy.rs:219` | `is_opencode` 改由 transport 能力提供 | 同上 |
| `src/free_sources.rs` | 保留全部公开签名，内部改读插件注册表 | 兼容层 + golden |
| `src/provider_profiles.rs` | 注册表改读「内置插件 + 已安装插件」，内嵌 JSON 作 fallback | 现有 14 套顺序不变 |
| `src/main.rs:320` | 调度改由 `runtime.scheduler` 声明 | ZCode 行为不变 |
| `src/db.rs` | **只在 `SCHEMA` 追加三张表**；`MIGRATIONS` 不动 | G1/G2 守卫 |
| `ui/v4.html` + `ui/v4-resources.js` | 新增 v4「插件」视图 | 原有 v4 视图不动 |
| `src/zcode/*` | **不改** | 配置与表结构保持原样 |

---

## 12. 数据模型（新增表）

```sql
-- L3-1 已安装插件。active_version 通过 active=1 唯一约束表达
CREATE TABLE IF NOT EXISTS plugins (
    id              TEXT NOT NULL,
    version         TEXT NOT NULL,
    tier            TEXT NOT NULL,          -- declarative | native | sidecar
    kind            TEXT NOT NULL,          -- free | account | provider
    channel         TEXT NOT NULL,          -- official | remote | local
    manifest_json   TEXT NOT NULL,          -- 安装时快照
    checksum        TEXT NOT NULL,          -- 包 sha256；内置为 "builtin"
    active          INTEGER NOT NULL DEFAULT 0,
    retained        INTEGER NOT NULL DEFAULT 0,  -- 卸载后数据是否保留
    legacy_tables   TEXT NOT NULL DEFAULT '[]',
    installed_at    INTEGER NOT NULL,
    PRIMARY KEY (id, version)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plugins_active_one
    ON plugins(id) WHERE active = 1;

-- L3-2 事件（审计 + 排障，只增不删）
CREATE TABLE IF NOT EXISTS plugin_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    plugin_id   TEXT NOT NULL,
    action      TEXT NOT NULL,   -- install|update|rollback|enable|disable|attach|detach|uninstall|error
    version     TEXT,
    detail      TEXT,
    ts          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plugin_events_id ON plugin_events(plugin_id, ts DESC);

-- L4a 插件私有数据：命名空间 KV，插件不得建表
CREATE TABLE IF NOT EXISTS plugin_kv (
    ns          TEXT NOT NULL,             -- plugin id
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,             -- JSON
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (ns, key)
);
```

**为什么状态不全放 `bridge.config.json`**：安装状态是"机器事实"（文件在不在），配置是"用户意图"；混在一起会导致手工改配置把插件状态改坏。上游条目仍然只在配置里，唯一事实源不变。

---

## 13. API 契约

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/plugins` | GET | 已安装 + 三态 + 可更新标记 |
| `/api/plugins/{id}` | GET | manifest、权限、配置块路径、事件（最近 20 条） |
| `/api/plugins/market` | GET | 市场列表（`channel` / `q` / `kind`），标注渠道与信任级别 |
| `/api/plugins/install` | POST | `{id, version, channel}`；装完 `enabled=false` |
| `/api/plugins/{id}/update` · `/rollback` | POST | 升级 / 回滚 |
| `/api/plugins/{id}` | DELETE | `detach` / `purge_data` 参数见 §8.3；native 拒绝 |
| `/api/plugins/{id}/enable` · `/disable` | POST | 运行时开关 |
| `/api/plugins/{id}/attach` | POST | 生成并写入上游；`overwrite` 处理冲突 |
| `/api/plugins/{id}/detach` | POST | 只移除该上游 |
| `/api/plugins/reload` | POST | 重扫磁盘 |

兼容：

- `/api/free-sources*` 五个端点**全部保留**，内部转发到插件层；响应新增 `pluginId`，其余字段不变。
- `/api/config` 的 `profiles` 保留，来源改为插件注册表，顺序仍按 `priority`。
- 新诊断码：`plugin_manifest_invalid`、`plugin_version_incompatible`、`plugin_transport_unknown`、
  `plugin_permission_denied`、`plugin_unsigned_trusted`、`plugin_upstream_conflict`、
  `plugin_package_forbidden_entry`、`plugin_native_uninstallable`、`plugin_kv_quota_exceeded`、`plugin_busy`。
- 新事件（非诊断）：`install_orphan_cleaned`、`uninstall_pending_cleanup`、`plugin_data_version_changed`、`template_changed`。

---

## 14. 面板

新增「插件市场」tab（放在「配置」之后），三视图：已安装 / 市场 / 更新。
卡片展示 `kind`、`tier`、版本、三态、权限摘要、配置块路径（如 `data/zcode.json`）。

v3 视图**不重复建房**：

- 「免费来源」视图保持常驻（发现免费模型的入口），作为 `modelgate.free.*` 的详情页；
- 「ZCode」视图保持未启用时隐藏，作为 `modelgate.account.zcode` 的详情页；
- 插件卡片只提供「入口 + 状态 + 生命周期按钮」，具体操作仍在对应 v3 视图。

---

## 15. 分阶段落地

| 阶段 | 范围 | 验收 |
|---|---|---|
| **P0 抽象层**（行为零变化） | manifest schema；三份内置清单；transport 注册表；`disabled`；`free_sources` / `provider_profiles` 改读注册表；市场 tab 只读 | `cargo test` 全绿；`build_attach()` 逐字节一致；G1/G4 通过 |
| **P1 本地市场** | 三张表；安装/卸载/启停/接入/移除；official + local；API 全套 | G2/G3 通过；装→启→接→请求→卸 全链路无残留 |
| **P2 远程市场** | remote 渠道；签名；更新/回滚；权限确认 | 篡改包被拒；回滚行为一致；断网降级 |
| **P3 第三方** | sidecar 运行时 + 权限沙箱 | 默认关闭；越权拒绝；单插件崩溃不影响网关 |

---

## 16. 非目标

- ❌ 插件内任意 UI 注入（只允许声明式卡片 + Markdown 详情）
- ❌ 付费/计费市场；插件间依赖求解
- ❌ 插件改路由策略、模型合并策略、用量口径
- ❌ 免费模型自动进 `/v1/models`
- ❌ 插件执行 DDL（结构性禁止，不提供接口）
- ❌ 首版开放第三方代码执行

---

## 17. 风险与对策

| 风险 | 表现 | 对策 |
|---|---|---|
| **双事实源漂移** | manifest 的 `baseUrl` 与配置不一致 | 配置是唯一事实源；只展示 diff，不自动覆盖 |
| **三态混淆** | 用户以为"装了就能用" | 三个徽标分别展示；`attach` 按钮写明"会让请求生效" |
| **卸载误删资产** | 账号、modelMap 丢失 | 卸载矩阵默认保留；唯一破坏性操作是删插件目录 |
| **DB 被插件污染** | 僵尸表、表名冲突 | R1 零 DDL + `plugin_kv` + legacy 白名单 + G2 守卫 |
| **id 命名空间冲突** | 第三方占用 `modelgate.*` | 官方保留前缀；第三方必须含 `.` 且不以 `modelgate.` 开头 |
| **transport 兼容** | 旧配置 `transport:"zcode"` | `aliases` 永久有效 |
| **P0 改动面过大** | 重写 `free_sources.rs` 引入回归 | 保留签名做兼容层 + 逐字节 golden |

---

## 18. 待确认决策

| # | 决策 | 建议 |
|---|---|---|
| 1 | 内置插件是否也走「安装」流程 | **是**：随包预置为 installed，可卸载/重装；native 除外（只能禁用） |
| 2 | 卸载时上游条目默认保留还是移除 | **保留并 `disabled=true`**；需要干净移除时显式 `detach=true` |
| 3 | `plugin_kv` 是否开放给第三方 | P1/P2 **不开放**（只给内置插件用）；P3 随 sidecar 一起开放并加配额 |
| 4 | 免费来源 tab 是否保留 | **保留常驻**，作为插件详情页；市场只做入口与状态 |
| 5 | ZCode 领取调度是否由 manifest 声明 | **是**：`runtime.scheduler`，`main.rs` 不再写死 |
| 6 | 远程索引默认 URL | **留空**，用户显式填写 |
