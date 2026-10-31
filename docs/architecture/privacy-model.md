# 隐私协议统一模型

> 目标：回答“ZDR / 训练数据 / 数据收集 / 区域”到底是不是标准请求协议，并给出一套可扩展的统一架构。
> 本文是设计文档；当前实现状态见第 9 节。

## 0. 结论先行

1. **没有标准的 `zed` 请求头，也没有跨厂商的 ZDR 标准请求字段。**
   IANA 的 HTTP 字段注册表里没有 `zdr`、`zero-data-retention`、`training` 之类的字段。
   “ZDR”是厂商/合同层面的概念，不是一个可移植的 HTTP 协议。

2. **如果 `zed` 指 Zed 编辑器：**
   - Zed 会发送 `x-zed-version`、`x-zed-preferred-experiment`、`X-Zed-Predict-Edits-Mode` 等头，
     这些是**客户端标识/功能协商**，不是隐私开关。
   - Zed 的编辑预测请求体里有一个 `can_collect_data` 字段，表示用户是否同意采样；
     它不是 ZDR 头，也不适用于普通聊天请求。
   - Zed-hosted 模型的“不训练 + ZDR”是 Zed 与模型供应商的**合同承诺**，
     不是每个请求携带的字段。

3. **如果 `zed` 指 ZDR：**
   - OpenRouter 用请求体 `provider.zdr` / `provider.data_collection`；
   - Vercel AI Gateway 用 `providerOptions.gateway.zeroDataRetention` /
     `disallowPromptTraining`；
   - CommandCode 用请求头 `x-cmd-zdr: 1`；
   - OpenCode Go 只能在工作区 `Settings › Privacy` 设置；
   - OpenAI / Anthropic 的 ZDR 是账户/合同级，不是请求头。
   因此：**统一的是配置入口和适配层，不是线上协议。**

4. 当前 `iceman-contributor-free` 的 400 不是 Model-Gate 的配置没生效：
   OpenCode 官方客户端本身不发任何隐私头，服务端只认 workspace 设置。
   需要在 `opencode.ai/console → Settings › Privacy` 打开
   `Allow paid endpoints that train on request data`，或改用不训练的模型。

## 1. 事实调研

### 1.1 Zed 编辑器

| 项目 | 事实 |
|---|---|
| 客户端头 | `x-zed-version`、`x-zed-preferred-experiment`、`x-zed-expired-token`、`X-Zed-Predict-Edits-Mode` 等 |
| 编辑预测 | 请求体字段 `can_collect_data`（旧名 `data_collection_permission`）表示用户是否同意采样 |
| Zed-hosted 模型 | 官方文档：供应商协议禁止训练，并要求推理请求零数据保留；Anthropic Covered Models 等安全保留例外 |
| 普通聊天 | 没有 ZDR / training 请求头 |

结论：Zed 不是隐私协议标准；它只是另一个客户端。

### 1.2 OpenRouter

请求体：

```json
{
  "provider": {
    "zdr": true,
    "data_collection": "deny"
  }
}
```

- `zdr: true`：只路由到 ZDR 端点；`false` 或不传不生效。
- `data_collection: "deny"`：只使用不非临时存储、且不会训练的用户数据的供应商。
- 两者都是**路由过滤**，不是对“所有供应商绝对不保留”的保证。
- 账户级也有 ZDR / data policy 设置；请求级 `zdr` 与账户级/guardrail 是 OR 关系。

### 1.3 Vercel AI Gateway

`providerOptions.gateway`：

```ts
{
  zeroDataRetention?: boolean,          // 只路由到有 ZDR 协议的供应商
  disallowPromptTraining?: boolean      // 只路由到不训练提示词的供应商
}
```

- 同样是路由过滤。
- 通过 AI SDK 的 provider options 进入请求体，不是 HTTP 头。

### 1.4 CommandCode

- 观察到/已实现的请求头：`x-cmd-zdr: 1`。
- 这是厂商私有的 ZDR 信号，不是公开标准。
- 没有公开文档表明它支持请求级“禁止训练”字段；因此训练策略标记为不支持请求级。
- Model-Gate 通过 `provider_profiles.json` 的 `privacyAdapters.zdr` 声明这个头。

### 1.5 OpenCode / OpenCode Go

- 官方 Go 文档：工作区隐私设置适用于 Go；默认阻止会训练请求数据的付费端点；
  owner/admin 在 `Settings › Privacy` 打开 `Allow paid endpoints that train on request data`。
- 模型隐私表区分 `Training` 与 `Data retention`：
  - `Iceman Contributor Free`：Training = Yes，Retention = Not ZDR；
  - `Muse Spark 1.x Contributor`：Training = Yes，Retention = Not ZDR；
  - 多数其他模型：Training = Not used，Retention = 0 days；
  - 部分模型（Grok/GPT/Claude Haiku/Beam Free）：Training = Not used，Retention = 30 days。
- OpenCode 官方客户端请求头只有会话/项目/客户端标识，没有 ZDR/training 头。
- 因此：请求级无法改变训练授权；只能在 workspace 设置或换模型。

### 1.6 OpenAI / Anthropic

- OpenAI：数据保留/ZDR 是账户或企业协议级；API 请求没有统一的 ZDR 头。
- Anthropic：ZDR 是合同级；部分 Covered Models 仍有安全保留（至少 30 天），
  即使使用 ZDR 协议也不能关闭。
- 这类供应商的隐私能力应标记为 `account` / `contract`，而不是 `header`。

## 2. 语义拆分

“隐私”至少是四个独立维度，不能用一个布尔值表达：

| 维度 | 含义 | 典型控制 |
|---|---|---|
| `retention` | 请求/响应是否被非临时保留 | ZDR 协议、`provider.zdr`、`x-cmd-zdr`、workspace/account 设置 |
| `training` | 请求/响应是否用于训练 | OpenRouter `data_collection`、Vercel `disallowPromptTraining`、OpenCode workspace 授权 |
| `dataCollection` | 是否允许非临时存储/收集（可能包含训练） | OpenRouter `data_collection` |
| `region` | 请求在哪个区域处理 | OpenRouter 区域主机、OpenCode allowed regions、账户设置 |
| `safetyRetention` | 供应商强制安全保留，无法关闭 | Anthropic Covered Models、OpenAI abuse monitoring |

关键区别：

- **ZDR ≠ 不训练**：模型可以“不训练但保留 30 天”，也可以“训练且不 ZDR”。
- **OpenRouter 的 `data_collection` 是复合语义**：它同时描述“是否非临时存储”和“是否可能训练”。
  因此 `training=deny` 映射到 `data_collection="deny"` 是“最接近的可用控制”，
  会比单纯的“禁止训练”更严格。
- **安全保留优先于 ZDR**：Anthropic Covered Models 等模型即使配置了 ZDR 也会保留；
  这类冲突必须在模型级能力里标注，不能靠请求头解决。

## 3. 三层架构

```
用户意图（配置）
  compat.privacy: { retention, training, dataCollection, region }
        │
        ▼
Provider 能力矩阵（provider_profiles.json）
  privacyAdapters: { retention.zdr, training.deny, training.allow, ... }
        │
        ▼
解析结果（运行时）
  actions:   header / body / query / baseUrl
  scope:     request / workspace / account / unsupported
  diagnostics + enforcement
        │
        ▼
代理层
  写入请求；不支持时告警或阻断；记录可观测字段
```

设计原则：

1. **意图与实现分离**：用户只表达“我要 ZDR / 禁止训练”，不关心是头还是 body。
2. **能力声明在注册表**：新增供应商只改 `provider_profiles.json`，不改 Rust 分支。
3. **不静默降级**：不支持或只能去控制台时，必须给出诊断；严格模式可阻断请求。
4. **模型级优先**：模型自身的强制保留/训练条款高于上游默认配置。
5. **可审计**：每次请求都能回答“实际执行了什么、没执行什么、为什么”。

## 4. 配置契约

> 本节是**目标 schema**；当前实现使用 `compat.privacy.zdr` 和
> `compat.privacyAdapters.zdr / trainingAllow / trainingDeny` 三个兼容键，
> 迁移路径见第 8 节。新增维度时优先按本节的目标键扩展，旧键保留两个版本。

### 4.1 用户意图

```json
"compat": {
  "privacy": {
    "retention": "zdr",
    "training": "deny",
    "dataCollection": "deny",
    "region": "inherit",
    "onUnsupported": "warn"
  }
}
```

取值：

| 字段 | 取值 | 说明 |
|---|---|---|
| `retention` | `zdr` / `standard` / `inherit` | `zdr` 要求零数据保留；`standard` 不额外要求 |
| `training` | `deny` / `allow` / `inherit` | `deny` 禁止训练；`allow` 显式同意训练 |
| `dataCollection` | `deny` / `allow` / `inherit` | 是否允许非临时存储/收集 |
| `region` | `global` / `eu` / `us` / `inherit` | 处理区域；需要上游支持 |
| `onUnsupported` | `warn` / `block` | 请求级无法落实时告警还是阻断 |

> 旧字段 `compat.zdr` / `cmdZdr` 继续兼容：`true` 等价于 `retention="zdr"`，
> `false` 等价于 `retention="standard"`。新配置应使用 `compat.privacy`。

### 4.2 Provider 能力矩阵

```json
"compat": {
  "privacyAdapters": {
    "retention": {
      "zdr": { "kind": "header", "name": "x-cmd-zdr", "value": "1" },
      "standard": { "kind": "none" }
    },
    "training": {
      "deny": { "kind": "body", "path": "provider.data_collection", "value": "deny" },
      "allow": { "kind": "workspace", "url": "https://opencode.ai/console", "setting": "Settings › Privacy → Allow paid endpoints that train on request data" }
    },
    "dataCollection": {
      "deny": { "kind": "body", "path": "provider.data_collection", "value": "deny" },
      "allow": { "kind": "none" }
    },
    "region": {
      "eu": { "kind": "baseUrl", "value": "https://eu.openrouter.ai" },
      "us": { "kind": "baseUrl", "value": "https://us.openrouter.ai" }
    }
  }
}
```

适配器 `kind`：

| kind | 行为 | 作用域 |
|---|---|---|
| `header` | 写请求头 | request |
| `body` | 按点分路径写请求体 | request |
| `query` | 写 URL 查询参数 | request |
| `baseUrl` | 替换上游 base URL（区域路由） | request |
| `workspace` | 只能在上游控制台设置 | workspace |
| `account` | 只能由账户/合同设置 | account |
| `none` | 上游默认行为已满足 | — |
| `unsupported` | 上游没有该能力 | — |

### 4.3 内置矩阵（目标）

| 上游 | retention=zdr | training=deny | training=allow | dataCollection=deny | region |
|---|---|---|---|---|---|
| CommandCode | header `x-cmd-zdr: 1` | unsupported | unsupported | unsupported | unsupported |
| OpenCode / Go | unsupported | unsupported（改模型） | workspace 授权 | unsupported | workspace |
| Kilo / OpenRouter 兼容 | body `provider.zdr=true` | body `provider.data_collection="deny"` | none | body `provider.data_collection="deny"` | baseUrl `eu.` / `us.`（如支持） |
| Vercel AI Gateway | body `providerOptions.gateway.zeroDataRetention=true` | body `providerOptions.gateway.disallowPromptTraining=true` | none | body 同 deny | upstream |
| OpenAI / Anthropic | account/contract | account/contract | account/contract | account/contract | account |

`account` / `workspace` 不是“失败”，而是“需要上游侧动作”。UI 必须展示动作和链接，
而不是显示一个假的“已开启”。

## 5. 解析与冲突规则

### 5.1 优先级

```
模型级 privacy 覆盖
  > 上游 compat.privacy
  > provider profile 的默认意图
  > 上游默认行为
```

### 5.2 合并规则

- **同维度**：显式值覆盖默认值；`inherit` 表示不覆盖。
- **同字段多维度**：例如 OpenRouter 的 `training=deny` 和 `dataCollection=deny` 都写
  `provider.data_collection`，合并时取更严格值（`deny` > `allow`）。
- **请求级 + 账户级**：OpenRouter 的请求级 `zdr` 与账户级/guardrail 是 OR；
  请求级为 `false` 不会关闭账户级 ZDR。
- **模型级强制策略**：如果模型标记 `safetyRetention=true`，用户请求 `retention=zdr`
  必须产生 `privacy_conflict`，不能宣称已满足 ZDR。

### 5.3 能力解析结果

```rust
struct PrivacyResolution {
    intent: PrivacyIntent,
    actions: Vec<PrivacyAction>,      // 实际要写入的 header/body/query/baseUrl
    scope: PrivacyScope,              // request / workspace / account / unsupported
    diagnostics: Vec<PrivacyDiagnostic>,
    enforcement: Enforcement,         // enforced / partial / unsupported
}
```

## 6. 失败策略

| 模式 | 行为 |
|---|---|
| `warn`（默认） | 发送请求；记录告警；响应头标记 `partial`；配置诊断可见 |
| `block` | 在发送前返回 409/400，错误码 `privacy_unsupported`，附上游要求的操作 |

建议：

- 对 `retention=zdr` / `training=deny` 这类安全承诺，提供 `block` 选项；
- 默认保持 `warn`，避免破坏已有配置；
- UI 必须在选择“不支持请求级”的维度时提示作用域，而不是静默保存。

## 7. 可观测性

### 7.1 配置诊断

| code | 触发 |
|---|---|
| `privacy_unsupported` | 用户要求某维度，但该上游没有请求级能力 |
| `privacy_workspace_required` | 只能去上游控制台设置 |
| `privacy_conflict` | 模型强制策略与用户意图冲突 |
| `privacy_adapter_invalid` | 注册表里的适配器配置非法 |

### 7.2 每请求

- 响应头：`x-model-gate-privacy: enforced` / `partial; retention=workspace` / `unsupported`
- 用量记录：`privacy_retention`、`privacy_training`、`privacy_enforcement`
- 日志：只记录维度、作用域和上游名，不记录请求内容

## 8. 迁移与兼容

1. `compat.zdr=true` → `compat.privacy.retention="zdr"`；
   `false` → `"standard"`。
2. `cmdZdr` 同样映射；保留两个版本，产生 `legacy_privacy_zdr` 提示。
3. `privacyAdapters` 的旧键 `zdr / trainingAllow / trainingDeny` 保留；
   新键 `retention / training / dataCollection / region` 逐步引入。
4. 旧配置不需要手工迁移；面板保存时写新键。

## 9. 当前实现与差距

当前已实现（本仓库）：

| 能力 | 状态 |
|---|---|
| `compat.privacy.zdr` + `compat.privacy.training` | ✅ |
| `privacyAdapters.zdr / trainingAllow / trainingDeny` | ✅ |
| `header` / `body` / `workspace` / `none` / `unsupported` | ✅ |
| 旧 `compat.zdr` / `cmdZdr` 兼容 | ✅ |
| `privacy_unsupported` 配置诊断 | ✅ |
| 上游训练授权 400 的中文提示 | ✅ |
| 面板 ZDR / 训练策略选择与生效方式提示 | ✅ |

尚未实现（建议下一步）：

| 能力 | 说明 |
|---|---|
| `dataCollection` 独立维度 | 当前 `training=deny` 在 OpenRouter 上复用 `data_collection`，是近似映射 |
| `region` 维度 | OpenRouter 区域主机、OpenCode allowed regions |
| `onUnsupported: block` | 严格模式，发送前阻断 |
| `x-model-gate-privacy` 响应头 | 每请求可观测性 |
| 模型级 `safetyRetention` | Anthropic Covered Models 等冲突检测 |
| `query` / `baseUrl` 适配器 | 区域路由、查询参数场景 |
| 用量表隐私字段 | 审计与统计 |

## 10. 对当前 `iceman-contributor-free` 400 的结论

- 该模型官方隐私表：Training = Yes，Retention = Not ZDR。
- OpenCode Go 默认阻止会训练请求数据的付费端点。
- 请求头 `x-cmd-zdr` 对 OpenCode 无效；官方客户端也不发送任何隐私头。
- 解决路径只有：
  1. workspace owner/admin 在 `Settings › Privacy` 打开
     `Allow paid endpoints that train on request data`；
  2. 或改用不训练的模型（如 `deepseek-v4.1-flash`、`glm-5.3-flash`）。
- 这不是 Model-Gate 的配置未生效，而是上游没有请求级能力。
