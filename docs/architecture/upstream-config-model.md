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

现有诊断码与行为不变。

## 8. API 契约（只增不删）

- `GET /api/config`：`config` / `effective` / `diagnostics` 保持；新增 `profiles: [{id, label, description, capabilities}]`；
- `GET /api/config/validate`：同上新增 `profiles`；`effective` 每个上游新增 `profile`（生效 id）、
  `capabilities`（解析结果）、`modelOverrides`（原样回显）；
- `effective.limits` 语义不变（上游级）；模型级通过 `modelOverrides` 回显，由面板展示。

## 9. 面板行为

- 预设下拉改读 `config.profiles`（缺失时回退现有硬编码，兼容旧后端）；当前 `compat.profile` 不在列表时追加显示；
- 高级区新增「模型级覆盖（JSON）」文本域，保存前 `JSON.parse` 校验对象结构；非法时阻止保存并提示；
- 新增只读「能力」徽标与「生效值」摘要（profile、上游 limits、模型覆盖条数）；
- 保存沿用未知字段保留策略；`modelOverrides` 与既有字段一样参与 `collectConfig`。

## 10. 验证工具

- `cargo test`（`provider_profiles.rs` / `config.rs`）：注册表结构、判定覆盖、golden 样例、旧配置零 error；
- `rust/model-gate/tests/fixtures/legacy_config.json`：旧简写与未知字段兼容夹具；
- `scripts/panel_api_smoke.ps1`：启动 Rust 实例，检查 `/api/config` 的 profiles 顺序、
  `effective.modelOverrides`、保存往返与非法通配 400。

## 11. 示例配置

`docs/examples/providers/` 提供五套可直接加载的单上游完整配置（`openai.json` / `opencode.json` /
`anthropic.json` / `deepseek.json` / `commandcode.json`）：

- 每份均通过 Rust 配置加载与校验零 error，profile 判定与文件名一致；
- `anthropic.json` 演示 `modelOverrides` 精确 key + 单模型 `capabilities`；
- `deepseek.json` 演示精确 key 与 `deepseek-*` 通配叠加；
- `commandcode.json` 演示旧扁平字段（`modelCatalog` / `modelCatalogPlan` / `pauseTurn` / `cmdZdr`）映射；
- 复制任意一份为 `bridge.config.json` 即可使用，密钥统一走 `apiKeyEnv` / `apiKeyFile`。
