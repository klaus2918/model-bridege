# 02 配置模型与迁移

## 1. 两个视图，一份事实

```
逻辑视图（面板/AI 看到）                物理视图（磁盘/SQLite）
┌────────────────────────┐            ┌─────────────────────────────┐
│ AccessObject           │  聚合/投影  │ bridge.config.json          │
│  ├─ 连接定义            │ ◄───────── │  upstreams.<name>           │
│  ├─ 凭据绑定            │            │  unified / groups / pricing │
│  ├─ 池统计(脱敏)        │            │ bridge.config.secrets.json  │
│  ├─ 健康/用量/诊断      │            │  （DPAPI 密文）              │
│  └─ 来源/模板           │            │ modelgate.db                │
└────────────────────────┘            │  credential_pools/members   │
                                      │  zcode_accounts（legacy）    │
                                      └─────────────────────────────┘
```

**没有 `access.json`**。逻辑视图由后端实时聚合，避免双事实源与同步问题。面板保存仍写 `bridge.config.json`。

## 2. 配置结构（增量，不破坏）

```jsonc
{
  "version": 2,                    // 新增，缺省=1；仅用于提示，不阻断
  "host": "127.0.0.1",
  "port": 8900,
  "defaultModel": "deepseek-v4.1-flash",
  "models": [],
  "aliases": {},
  "unified": { "visibleUpstreams": [], "groups": [] },
  "groups": [],
  "pricing": {},
  "upstreams": {
    "deepseek": {
      // ── 旧字段，全部原样 ──
      "protocol": "openai",
      "baseUrl": "https://api.deepseek.com",
      "apiKeyStore": "upstream:deepseek",
      "models": ["deepseek-v4.1-flash"],
      "modelMap": {},
      "compat": { "profile": "deepseek" },

      // ── 新增可选字段 ──
      "template": "deepseek",
      "templateVersion": 1,
      "kind": "official",
      "label": "DeepSeek 官方",
      "privacy": { "zdr": true, "training": "deny" }
    },

    "my-gateway": {
      "protocol": "openai",
      "baseUrl": "https://gateway.example.com/v1",
      "apiKeyStore": "upstream:my-gateway",
      "template": "custom",
      "kind": "custom",
      "label": "我的中转",
      "privacy": { "zdr": true, "training": "deny" },
      "fetchModels": true,
      "autoModels": false
    },

    "zcode": {
      "protocol": "anthropic",
      "transport": "zcode",
      "baseUrl": "https://zcode.z.ai/api/v1/zcode-plan/anthropic",
      "credentialPool": "legacy_zcode",   // 新增：绑定池
      "kind": "account",
      "template": "zcode"
    }
  }
}
```

### 2.1 新增字段契约

| 字段 | 类型 | 缺省 | 说明 |
|---|---|---|---|
| `version` | number | 1 | 配置版本；仅提示 |
| `template` | string | 由 profile/域名推导 | 模板 id |
| `templateVersion` | number | 0 | 模板版本，落后时提示 |
| `kind` | string | `custom` | official/custom/free/account/plugin |
| `label` | string | `name` | 展示名 |
| `privacy.zdr` | bool | 模板值 | 是否请求 ZDR |
| `privacy.training` | string | 模板值 | `allow`/`deny` |
| `credentialPool` | string | 空 | 绑定池 id；优先于 `apiKeyStore` |

**保留字**：`kind`、`label`、`privacy`、`credentialPool`、`template`、`templateVersion` 不得作为上游名以外的用途；上游名仍禁止 `/` 与空串。

### 2.2 校验规则

| 规则 | 级别 | 处理 |
|---|---|---|
| `name` 非空、无 `/`、不与保留字冲突 | error | 拒绝保存 |
| `protocol ∈ {openai, anthropic, commandcode, opencode, zcode}` | error | 拒绝保存；插件 transport 可扩展 |
| `baseUrl` 为空且模板无默认 | error | 拒绝保存 |
| 官方模板 `requiredFields` 未满足 | error | 返回缺字段列表 |
| `credentialPool` 不存在 / 不属于该上游 | error | 拒绝保存 |
| 池无可用成员 | warn | 允许保存，接入状态标"待补充凭据" |
| `privacy.zdr=true` 但模板 `support=none` | info | 保存但提示"该厂商无请求级开关" |
| 未知字段 | info | 保留，不报错 |

## 3. 模板默认值（官方厂商）

模板是"默认值包"，不是"配置副本"。选择后只填空字段，已有值不覆盖（除非用户点"用模板覆盖"）。

| 模板 | 协议 | 默认地址 | 鉴权 | 隐私默认 | 必填 | 模型目录 |
|---|---|---|---|---|---|---|
| `deepseek` | openai | `https://api.deepseek.com` | bearer | ZDR 开 / deny | apiKey | 自动 |
| `openai` | openai | `https://api.openai.com/v1` | bearer | ZDR 开 / deny | apiKey | 自动 |
| `anthropic` | anthropic | `https://api.anthropic.com` | x-api-key | ZDR 开 / deny | apiKey | 自动 |
| `opencode` | openai | `https://opencode.ai/zen/v1` | bearer（可空） | 账户级 | 无 | 自动 |
| `commandcode` | commandcode | 现有 profile 值 | bearer | 请求头 `x-cmd-zdr: 1` | apiKey | 套餐 |
| `kimi` / `zhipu` / `minimax` / `dashscope` / `ark` / `siliconflow` / `modelscope` / `hunyuan` / `kilo` | 现有 profile 值 | 现有 profile 值 | 现有 profile 值 | ZDR 开 / deny（若支持） | apiKey | 自动 |

> 具体地址以 `provider_profiles.json` 的 `access` 为准，本文不重复维护，避免漂移。新增模板必须同时提供 `access`、`privacy`、`requiredFields`。

### 3.1 隐私能力的诚实表达

`privacy.support` 三档：

| 值 | 含义 | 面板文案 |
|---|---|---|
| `request` | 可通过请求头/请求体表达 | "已随请求发送 ZDR 标记" |
| `account` | 只能在工作区/账户设置里开 | "该厂商需在控制台开启；此处仅记录意图" |
| `none` | 无请求级开关 | "该厂商不支持请求级 ZDR；请阅读其数据政策" |

**不得**因为用户勾选 ZDR 就宣称"已保护"。CommandCode 发 `x-cmd-zdr: 1`；OpenRouter/Kilo 写 `provider.zdr` 与 `data_collection`；OpenCode 只记录意图并在 UI 明示。

## 4. 快速添加（官方厂商）

### 4.1 交互契约（2 步）

```
Step 1  选择厂商（搜索/网格）        → templateId
Step 2  填 Key（可展开"高级设置"）   → apiKey
完成    显示：接入地址、协议、默认模型、隐私状态
```

### 4.2 API 契约

```http
POST /api/access/quick
Content-Type: application/json

{
  "templateId": "deepseek",
  "name": "deepseek",          // 可省略，默认模板 id；冲突时返回 409
  "apiKey": "sk-...",          // 仅本机回环；落盘前加密
  "privacy": { "zdr": true },  // 可省略，用模板默认
  "setDefault": false
}
```

响应（脱敏）：

```jsonc
{
  "ok": true,
  "access": { "id": "deepseek", "kind": "official", "protocol": "openai",
              "baseUrl": "https://api.deepseek.com",
              "auth": { "mode": "single", "configured": true, "source": "store" },
              "privacy": { "zdr": true, "support": "request" } },
  "diagnostics": []
}
```

**密钥处理**：`apiKey` 只在请求体中出现一次；服务端立即写入 DPAPI 密钥库，配置只落 `apiKeyStore` 引用；响应、日志、审计均不含密钥。

### 4.3 自定义厂商（5 项）

```http
POST /api/access/custom
{
  "name": "my-gateway",
  "vendor": "自定义",
  "baseUrl": "https://gateway.example.com/v1",
  "protocol": "openai",
  "apiKey": "sk-...",
  "privacy": { "zdr": true },
  "fetchModels": true
}
```

`vendor` 用于展示与模板推荐，不参与协议判定。模型目录拉取失败不阻断保存，状态标"目录待拉取"。

## 5. 迁移

### 5.1 迁移原则

- **零强制迁移**：启动时旧配置直接可用；新字段按需补齐。
- **单向兼容**：新 exe 读旧配置；旧 exe 读新配置时忽略未知字段（已验证 JSON 解析策略）。
- **可回滚**：每次写配置前备份；提供"恢复上一版本"。
- **可预演**：`/api/config/preview` 与 `/api/ai/plan` 都不落盘。

### 5.2 启动时的补齐（惰性，不写盘）

1. 读 `upstreams`；
2. 对每项推导 `template`（显式 → `compat.profile` → 域名 → 协议）；
3. 推导 `kind`（有 `transport=zcode` → account；由插件 attach → plugin；无 `apiKeyStore/apiKeyEnv` 且本机地址 → custom；否则 official/custom）；
4. 计算 `privacy` 默认值（模板）；
5. 生成 `AccessObject` 供 API 读取；
6. **不自动写回**；用户首次保存该上游时才落新字段。

### 5.3 显式迁移（可选）

设置页提供"整理配置"：

- 为所有上游补齐 `template/kind/label/privacy`；
- 把旧 `apiKeyEnv` 指向的内置变量名建议迁移到 `apiKeyStore`（需用户确认，不自动读值）；
- 为 `zcode` 创建 `legacy_zcode` 池并映射现有账号（只建引用，不复制凭据）。

迁移前后均备份，失败自动回滚。

## 6. 兼容矩阵

| 场景 | 期望 | 验证 |
|---|---|---|
| 旧 exe + 旧配置 | 不变 | 现有测试 |
| 新 exe + 旧配置 | 正常启动、旧接口一致、面板显示推导后的 AccessObject | 金样本回归 |
| 新 exe + 新配置（含新字段） | 正常 | 单测 + 冒烟 |
| 旧 exe + 新配置 | 忽略新字段，仍可运行 | 用旧二进制启动验证 |
| 旧数据库 + 新 exe | 只追加新表，旧表不动 | `PRAGMA table_info` 对比 |
| 新数据库 + 旧 exe | 旧 exe 忽略新表 | 启动验证 |

## 7. 回滚方案

1. 配置：`bridge.config.json.bak.<timestamp>`，设置页一键恢复。
2. 密钥库：写入前复制 `*.secrets.json`；恢复配置时同步恢复密钥库。
3. 数据库：新表不参与旧功能；回滚只需停用新代码，无需删表。
4. 池：`status=disabled` 即可停用，不删成员。

## 8. 验收标准

- [ ] 旧配置启动后 `/v1/models`、`/v1/chat/completions` 响应与 v2.28 一致（金样本）。
- [ ] 官方模板快速添加只需 2 次用户输入（选模板、填 Key）。
- [ ] 自定义厂商表单默认只显示 5 个必填项，高级项折叠。
- [ ] 勾选 ZDR 后，按厂商能力发送对应字段；不支持的厂商显示明确文案。
- [ ] 保存失败不会留下半写配置；备份可恢复。
- [ ] 新增字段不改变旧 exe 的读取结果。
