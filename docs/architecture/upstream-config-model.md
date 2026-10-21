# 上游配置模型（注册表 + 模型级覆盖）

> 目标：把「某类厂商的默认兼容行为」从代码硬编码抽成数据（`rust/model-gate/provider_profiles.json`），
> 并在上游级 `compat` 之上增加模型级 `modelOverrides` 与只读 `capabilities`，
> 由 Rust 内核使用唯一的判定与合并规则。

关联文档：`docs/architecture/provider-compat.md`（现有 `compat` 字段契约，字段含义不变）。

## 1. 分层模型

自上而下：后者覆盖前者。

```
provider_profiles.json（唯一数据源，五套预设）
        │ 按 profile 判定加载
        ▼
profile compat 默认值
        │ 旧扁平字段映射（sessionHeader / cmdZdr / thinkingPassthrough / downgradeForcedChoice / …）
        ▼
upstream.compat 高级覆盖（现有字段 + 新增 catalog / capabilities）
        │ 全局 thinking / tools 开关（最终约束）
        ▼
effective upstream（API / 面板展示）
        │
        ▼
modelOverrides（精确 > 最长 * 通配 > 上游 compat；只覆盖 limits / capabilities）
```

## 2. profile 判定优先级

1. 显式 `compat.profile`（或旧字段 `profile`）；
2. 域名匹配（按注册表 `priority` 取最高，现为 `commandcode.ai` 100 > `api.deepseek.com` 90 > `opencode.ai` 80）；
3. 协议对应预设（`openai` → `openai`，`anthropic` → `anthropic`，`commandcode` → `commandcode`）。

未知 profile：诊断码 `unknown_profile`（warn），兼容行为保持现状（字段回退基础默认值）。

历史怪癖（兼容 shim，代码中显式注释保留）：显式 `anthropic` + 含 `deepseek` 的 host → 切换到 `deepseek` 语义
（Anthropic 兼容层同样受 DeepSeek 输出上限约束）。

## 3. 注册表结构

`rust/model-gate/provider_profiles.json` 顶层：`version`、`profiles`。

每个 profile：

| 字段 | 类型 | 说明 |
|---|---|---|
| `label` / `description` | string | 面板下拉与诊断展示 |
| `priority` | number | 域名探测冲突时的优先级，越大越优先 |
| `protocols` | string[] | 该 profile 适用的 wire 协议 |
| `detect.baseUrlContains` | string[] | 域名探测片段 |
| `compat` | object | 预设值；只需声明与「openai 基线」不同的字段 |
| `variants.<protocol>` | object | 按协议的差异覆盖（如 `deepseek.variants.anthropic`） |
| `defaults` | object | `fetchModels` / `autoModels` / `pauseTurn` 三键，必须齐全 |
| `authOptional` | boolean | 该 provider **允许空密钥调用**（默认 `false`）。声明在 `compat` **之外**，不参与 compat 合并 |

`authOptional` 独立于 `compat` 的原因：`compat` 会被「openai 基线 → profile → variants」三层合并覆盖，
而「是否需要鉴权」是 **profile 级**语义（`opencode` 允许空 key，但它的 `auth.scheme` 继承基线的 `bearer`）。
放在 `compat` 内会被基线 `bearer` 覆盖掉。

`compat` 内除现有字段外新增：

- `catalog`：`{ source: "endpoint" \| "commandcode", path, plan }`。CommandCode 的旧扁平字段
  `modelCatalog` / `modelCatalogPlan` 映射到 `catalog.source=commandcode`，行为不变；非 CommandCode 默认
  `{ source: "endpoint", path: "/v1/models", plan: false }`。
- `capabilities`：只读能力声明（见 §5）。

合并顺序：`openai 基线 compat` → 目标 profile `compat` → 目标 profile 对应协议 `variants`。

五套预设：`openai`（基线）、`opencode`（会话头）、`anthropic`（双写鉴权 + 思考回传 + messages 端点）、
`deepseek`（思考回传 + 384K/1M 上限，含 anthropic 变体）、`commandcode`（会话头 + ZDR + CLI 头 + 64K 上限）。

## 4. 模型级覆盖 `modelOverrides`

位置：`upstreams.<name>.modelOverrides`（顶层字段，不放进 `compat`，避免与「上游级覆盖」混淆）。

```json
{
  "modelOverrides": {
    "deepseek-v4.1-pro": {
      "limits": { "maxOutputTokens": 393216, "maxContextTokens": 1048576 },
      "capabilities": { "thinking": true, "vision": false, "cache": true }
    },
    "deepseek-*": {
      "limits": { "maxOutputTokens": 65536 }
    }
  }
}
```

规则：

- key 为路由解析后的模型 id；支持只含一个 `*` 的前缀/后缀通配；
- 优先级：精确匹配 > 最长通配 > 上游 `compat.limits`；
- `limits.maxOutputTokens` 参与请求 `max_tokens` 裁剪（Rust `proxy.rs` 统一解析）；
- `capabilities` 仅用于展示与配置诊断，不改变转发行为（避免误判导致请求被错误拒绝）；
- 未知模型 key：警告 `model_override_unknown_model`，不报错（模型目录可能动态变化）。

## 5. 能力元数据 `capabilities`

固定字段：`stream`、`tools`、`vision`、`thinking`、`cache`、`systemPrompt`。

合并顺序：模型覆盖 > 上游 `compat.capabilities` > profile `compat.capabilities` > 内置默认
（`stream` / `tools` / `systemPrompt` = true，其余 false）。

`/api/config` 的 `effective` 输出解析结果，面板以徽标展示。

## 6. 兼容与迁移

- 所有旧字段原样保留：`profile`、`sessionHeader`、`cmdZdr`、`thinkingPassthrough`、`downgradeForcedChoice`、
  `modelCatalog`、`modelCatalogPlan`、`pauseTurn` 等；
- 旧 `compat` 简写（`auth: "bearer"`、`session: true`、`thinking: false`、`toolChoice: true`）继续支持；
- 新增字段全部可选；未知字段不报错、面板保存不丢失；
- 无数据库结构变更；`settings.lastPort` 为新增键；
- 旧版 exe 读取新配置时忽略新字段，可继续运行。

### 6.1 密钥来源契约（`env-only-secrets`）

密钥**只有一个来源方向**：环境变量优先，其次是用户显式指定的凭据文件。
代码与模板中**没有任何内置厂商凭据路径**（opencode / jcode / commandcode 的默认路径一律移除）。

**读取优先级**（`config.rs::resolve_api_key`，同一上游取第一个非空值）：

| 顺序 | 来源 | `keySource` | 说明 |
|---|---|---|---|
| 1 | `apiKeyEnv` 指定的变量 | `env` / `user-env` | 取值顺序：进程环境 → 本进程会话缓存 → `HKCU\Environment` |
| 2 | `apiKeyFile` 指定的文件 | `file` | 读该 JSON 的 `apiKey` 字段（路径由用户手填） |
| 3 | `apiKeyAgentPath` 指定的文件 | `agent:file/<名>` / `agent:env-file/<名>` | JSON 点路径或 `.env` 变量名（`apiKeyAgent` 内置预设已移除） |
| 4 | 都没有 | `none` | **不发送鉴权头**，不生成占位密钥 |

**JSON 里的明文 `apiKey` 永不作为凭据**：历史配置中的非空值被忽略，
由 `diagnostics()` 给出 `legacy_literal_api_key` 迁移提示，且**不回显原值**。

**失效字段**（只给迁移提示，不阻断启动）：

| 字段 | 诊断码 | 级别 | 失效原因 | 替代 |
|---|---|---|---|---|
| `envFile` | `legacy_env_file` | warn | 内置 `.env` 兜底路径已移除 | `apiKeyEnv` |
| `apiKeyAgent` 内置预设（opencode / jcode / commandcode） | `legacy_agent_preset` | warn | 内置路径已移除 | `apiKeyEnv`，或 `apiKeyAgent=custom` + `apiKeyAgentPath` |
| `apiKeyFile` | `legacy_api_key_file` | warn | 不再内置默认路径 | 仍可用，但路径必须显式填写；建议改用 `apiKeyEnv` |
| `apiKeyAgent`（自定义路径） | `legacy_api_key_agent` | info | 旧凭据来源（仍可用） | 推荐改用 `apiKeyEnv` |
| JSON 明文 `apiKey` | `legacy_literal_api_key` | warn | 明文不再作为凭据 | `apiKeyEnv` + 面板「写入密钥」 |

> 上述诊断**都不阻断启动**，也**不回显原值**；warn 级别的会在面板提示区与启动日志点名。

**写入目标**（`POST /api/upstreams/secret`，见 §8.1）：

| 目标 | 落点 | 语义 |
|---|---|---|
| `env`（默认） | `HKCU\Environment` | 写 `apiKeyEnv` 指定的变量名，写后广播 `WM_SETTINGCHANGE`；当前进程靠会话缓存立即生效，新进程由系统继承 |
| `file` | 用户手填的绝对路径 | JSON（根字段 `apiKey` 或点路径）或 `.env`（变量名）；读-改-写保留其它字段，同目录临时文件 + `rename` 原子替换，**不生成 `.bak`**；写完用运行时同一套读取语义回读校验 |

写入 Model-Gate 自己的 `bridge.config.json` 会被拒绝；密钥含换行/控制字符会被拒绝；
错误信息只含路径与原因，**不含密钥**。配置文件里只保存路径引用，不保存密钥本体。

## 7. 校验与诊断

新增诊断码：

| 码 | 级别 | 条件 |
|---|---|---|
| `unknown_profile` | warn | 显式 profile 不在注册表 |
| `invalid_model_overrides` | error | `modelOverrides` 非对象、值为空对象、limits 非正整数 |
| `invalid_limit_relation` | error | `maxOutputTokens > maxContextTokens`（二者都存在时） |
| `invalid_capabilities` | error | capabilities 值非布尔 |
| `model_override_unknown_model` | warn | key 既非精确模型也不含 `*`，且不在本置 `models` / 目录中 |
| `multi_wildcard_override` | error | key 含一个以上 `*` |

### 7.1 免鉴权判定口径（`missing_credentials` 的前提）

诊断「没有配置密钥」必须与**运行时的实际行为**一致：不需要鉴权的上游不该被提醒。
判定为「需要鉴权」需**同时**满足下列三条，缺一即视为免鉴权、不产生该诊断：

| # | 条件 | 覆盖场景 |
|---|---|---|
| 1 | `compat.auth.scheme != "none"` | 显式声明不鉴权（`scheme=none`）即免鉴权 |
| 2 | profile 未声明 `authOptional: true` | 如 `opencode` / `zen`：注册表 description 明写「空 key 表示不鉴权」 |
| 3 | 上游地址不是本机 | 回环 / 局域网段（`127.0.0.0/8`、`localhost`、`::1`、`10/8`、`172.16/12`、`192.168/16`）上的本机桥接（如 CCH）通常不校验密钥 |

**本机地址判定必须按完整 IPv4 四段精确匹配**，不能按前缀 —— 否则
`127.0.0.1.example.com` 这类公网域名会被误判为本机而**漏报**真实缺密钥的上游。

反例（必须仍然产生 `missing_credentials`）：普通公网 bearer 上游无密钥。
判定放宽到「所有上游」等于把检查关掉，正例须有用例钉住。

### 7.2 等级口径

| 诊断 | 级别 | 理由 |
|---|---|---|
| `max_tokens_exceeds_upstream` | **info** | 运行时已在请求侧自动收敛，**不阻断保存** —— 属「知会」而非「待修」 |
| `thinking_not_used` | **info** | 只有 anthropic 协议才回传 thinking 块；openai 协议下 `thinking.passthrough` 不参与请求改写，同属「知会」 |

属实但等级不当的诊断（此例）与纯属误报的诊断（免鉴权上游）要分开处理：
前者降级并改中性样式，后者直接不产生。

现有诊断码与行为不变。

## 8. API 契约（只增不删）

- `GET /api/config`：`config` / `effective` / `diagnostics` 保持；新增 `profiles: [{id, label, description, capabilities}]`；
- `GET /api/config/validate`：同上新增 `profiles`；`effective` 每个上游新增 `profile`（生效 id）、
  `capabilities`（解析结果）、`modelOverrides`（原样回显）；
- `effective.limits` 语义不变（上游级）；模型级通过 `modelOverrides` 回显，由面板展示。

### 8.1 写入上游密钥 `POST /api/upstreams/secret`

入参：

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | 是 | 上游名 |
| `apiKey` | 是 | 明文密钥；为空 → 400 `empty_key`；含换行/控制字符 → 400 `invalid_key` |
| `target` | 否 | `env`（默认，写用户变量）/ `file`（写用户指定文件）；其它值 → 400 `invalid_target` |
| `envName` | 否 | 变量名；缺省时用该上游已配置的 `apiKeyEnv`；都没有 → 400 `missing_env_name`；含 `=` → 400 `invalid_env_name` |
| `filePath` | 否 | 凭据文件路径；缺省时回退该上游的 `apiKeyFile` / `apiKeyAgentPath`；都没有 → 400 `missing_file_path`；非绝对路径 → 400 `invalid_file_path` |
| `fileFormat` | 否 | `json`（默认）/ `env`；其它值 → 400 `invalid_file_format` |
| `field` | 否 | JSON 点路径（缺省 `apiKey`）/ `.env` 变量名（`.env` 必填） |

成功响应（**不含明文**）：

| 目标 | 响应字段 |
|---|---|
| `env` | `ok` / `name` / `target=env` / `envName` / `scope=user` / `keySource=user-env` / `note` |
| `file` | `ok` / `name` / `target=file` / `scope=file` / `filePath` / `fileFormat` / `field` / `created` / `keySource` / `warning` / `note` |

- `file` 目标下 `warning`：该上游已配置 `apiKeyEnv` 时提示「环境变量优先于文件，文件里的密钥暂不生效」；
- `keySource` 取值：`user-env` / `file` / `agent:file/<名>` / `agent:env-file/<名>`；
- 写入失败**不降级为「只进内存」**：注册表写入失败 → 500 `write_user_var_failed`；
  文件写入失败 → 400 `write_key_file_failed`（参数不合法：路径/字段/密钥内容）或 500 `write_key_file_failed`（IO 失败）；
  文件写后回读校验失败 → 500 `verify_key_file_failed`；
- 写入成功后运行态立即更新（`key_source`），**路径引用需用户另存配置**才落盘到 `bridge.config.json`。

## 9. 面板行为

- 预设下拉改读 `config.profiles`（缺失时回退现有硬编码，兼容旧后端）；当前 `compat.profile` 不在列表时追加显示；
- 高级区新增「模型级覆盖（JSON）」文本域，保存前 `JSON.parse` 校验对象结构；非法时阻止保存并提示；
- 新增只读「能力」徽标与「生效值」摘要（profile、上游 limits、模型覆盖条数）；
- 保存沿用未知字段保留策略；`modelOverrides` 与既有字段一样参与 `collectConfig`。

### 9.2 密钥写入区（`env-only-secrets`）

上游编辑器的 API Key 行有「写入目标」下拉，两档都**没有内置默认路径**：

| 目标 | 面板字段 | 行为 |
|---|---|---|
| 写入用户环境变量 | 「密钥环境变量（apiKeyEnv）」 | 变量名为空时**拒绝写入**并提示先填变量名；确认框写明落点为 `HKCU\Environment → <变量名>` |
| 写入指定文件 | 「凭据文件路径」+「文件格式」+「字段 / 变量名」 | 路径为空 → 拒绝并提示；选 `.env` 文本但字段名为空 → 拒绝并提示；确认框写明落点路径、格式与字段名 |

- 密钥输入框的值**只用于本次写入**，不进入 `bridge.config.json`（保存时主动剔除 `apiKey`）；
- 面板与编辑弹窗共用同一组「写入目标」字段，切换目标时显隐对应输入；
- 写文件成功后，面板把**路径引用回填到表单字段**（JSON 根字段 `apiKey` → `apiKeyFile`；
  自定义字段 / `.env` 变量名 → `apiKeyAgent=custom` + `apiKeyAgentPath` + `apiKeyAgentProvider`），
  并标记「有未保存的改动」：**路径要落盘仍需用户点「保存配置」**，配置文件里只有路径引用。

### 9.1 提醒区（诊断展示）

诊断的**位置与呈现**遵循「不干扰配置填写」：

| 规则 | 说明 |
|---|---|
| 位置 | 提醒区位于**保存按钮行正上方**，不在表单顶部 —— 主展示区（服务与全局、上游列表）回到第一屏 |
| 默认折叠 | 折叠为一行摘要，按 `error` / `warn` / `info` 显示计数；有 `error` 时**自动展开** |
| 单条关闭 | 每条可单独关闭，**仅本次页面会话内生效**（刷新恢复） |
| 全局静音 | 「不再提示」持久化到 `localStorage`；**只压制展示，不压制 error** —— error 仍自动展开、仍阻断保存 |
| 按需检查 | 「检查」按钮主动触发一次完整诊断并强制展开 |

「检查」**不得重置表单**：只重拉服务端诊断并合并前端本地校验，
不能覆盖用户尚未保存的改动。

## 10. 验证工具

- `cargo test`（`provider_profiles.rs` / `config.rs`）：注册表结构、判定覆盖、golden 样例、旧配置零 error；
- `rust/model-gate/tests/fixtures/legacy_config.json`：旧简写与未知字段兼容夹具；
- `scripts/panel_api_smoke.ps1`：启动 Rust 实例，检查 `/api/config` 的 profiles 顺序、
  `effective.modelOverrides`、保存往返与非法通配 400；
- `rust/model-gate/tests/user_env_contracts.rs` / `key_file_contracts.rs`：密钥写入的静态守卫
  （源码与模板中无内置凭据路径、配置无明文密钥、拒写自身配置文件、`.env` 变量名校验）；
- `.op/changes/env-only-secrets/secret_api_smoke.ps1`：`POST /api/upstreams/secret` 的 env / file
  双目标端到端冒烟（写后注册表可读、文件保留其它字段、响应不含明文、非法 target/空 key 400、
  拒写自身 `bridge.config.json`）。

## 11. 示例配置

`docs/examples/providers/` 提供五套可直接加载的单上游完整配置（`openai.json` / `opencode.json` /
`anthropic.json` / `deepseek.json` / `commandcode.json`）：

- 每份均能通过 Rust 配置加载与校验（`config_load=ok`、**无 error 级诊断**），profile 判定与文件名一致；
- `anthropic.json` 演示 `modelOverrides` 精确 key + 单模型 `capabilities`；
- `deepseek.json` 演示精确 key 与 `deepseek-*` 通配叠加；该示例会带出 1 条 **info 级** `thinking_not_used`
  （deepseek profile 默认 `thinking.passthrough=true`，而 openai 协议本就不回传 thinking 块）——属「知会」，不阻断加载与启动；
- `commandcode.json` 演示旧扁平字段（`modelCatalog` / `modelCatalogPlan` / `pauseTurn` / `cmdZdr`）映射；
- 复制任意一份为 `bridge.config.json` 即可使用，密钥统一走 `apiKeyEnv` / `apiKeyFile`。
