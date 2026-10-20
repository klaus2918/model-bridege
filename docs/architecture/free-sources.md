# 免费来源（Free Sources）

> Model-Gate 的"免密免费模型车道"：不登录、不注册、不填 API Key，就能用上免费模型，
> 并看清**此刻到底哪个模型真能用、为什么不能用、下一步该做什么**。
>
> 对应变更：`.op/changes/free-token-sources/`（plan.md / tasks.md / expert-review.md / verify-log.md）
> 真机验收记录：[`verify-log.md`](../../.op/changes/free-token-sources/verify-log.md)

---

## 1. 两条免密来源

| 来源 id | 名称 | 端点 | 鉴权 | 传输层 | 隐私级别 |
|---------|------|------|------|--------|----------|
| `zen-free` | OpenCode Zen 免密池 | `https://opencode.ai/zen/v1` | 匿名 `Bearer public` + 客户端指纹 | `transport: opencode` | warn |
| `kilo-free` | Kilo 免密免费池 | `https://api.kilo.ai/api/openrouter` | **完全不发送鉴权头** | 标准 OpenAI 兼容 | **danger** |

两者都**不写 `apiKey`、不写 `apiKeyEnv`**，一键接入后配置里没有任何凭据。

- Zen 侧的传输层（指纹头、`ses_`/`msg_` ID、`/zen→/inference` 重写、强制流式、`bash`/`read` 注入）
  由 `upstream-compat` 的 R8 契约实现，本模块只做**启用与呈现**，不重复实现。
- Kilo 侧是标准 OpenAI 兼容端点 + 无鉴权，桥不需要任何特殊处理。

---

## 2. 三条设计红线

### 2.1 筛选 ≠ 可用

免费判定只负责把**候选**挑出来，最终可用性由**真实对话探测**决定。

```
免费候选 = id 以后缀结尾（zen: -free；kilo: :free）
        OR id 命中固定 id 集合（zen: big-pickle / grok-code）
        OR 目录条目 isFree == true
        OR 目录条目 pricing.prompt == 0 且 pricing.completion == 0
```

典型反例：Kilo 的 `google/lyria-3-*` 零价、`isFree` 未标，会被筛成候选，
但真实调用返回 `401 PAID_MODEL_AUTH_REQUIRED` —— **零价不等于免鉴权**。
面板会把它标成「明确不可用 / 需登录(非免费)」并留下原文。

### 2.2 "暂时不可用" ≠ "明确不可用"

这是本模块最核心的判定规则。

| 判定 | 充分条件 | 例子 |
|------|----------|------|
| `unavailable`（明确不可用） | **上游明确拒绝**：404 模型不存在 / 401 需登录 / 地区 403 / 免费层拒绝 | `nvidia/nemotron-3-super-120b-a12b:free` → 404 |
| `transient`（暂时不可用） | 失败原因与模型本身无关：429 限流 / 5xx / 超时 / 网络 | `xingchen-agi/*` → 429 上游池限流 |
| `suspect`（疑似可用） | 200 但正文为空（多为 reasoning 吃满预算） | `max_tokens` 给太小 |
| `region`（地区受限） | 上游对当前出口 IP 不放行 | 403 `not available in your country` |
| `available`（可用） | 真实生成成功且正文非空 | — |

**为什么必须分开**：Kilo 免密池 20 个候选里，失败的主因是**上游 provider 池限流**，
不是模型下线。若把 429 判成"不可用"，模型会频繁从清单里消失，用户会以为池子空了。
参考仓库（`dsh-our-free-model`）明确采用同一口径：「网关自身故障（5xx）、配额限制（429）、
超时与断网不属于对模型的判定，一律保持可达」。

**"明确拒绝"才判不可用**这条规则落在 `free_sources::classify_error`，
拒因与状态的对应关系由 `free_sources.json` 的 `rejectReasons` 字典定义（**单一事实源**，
分类器与字典不许漂移，有测试守着）。

### 2.3 隐私红线（前置告知，不是事后补文档）

| 来源 | 事实 | 依据 |
|------|------|------|
| Kilo | 20 个免费模型中 **19 个 `mayTrainOnYourPrompts=true`** | Kilo 目录字段实测 |
| Kilo | NVIDIA 免费端点官方明示"会记录会话用于改进产品" | kilo.ai/docs/gateway/models-and-providers |
| Kilo | `kilo-auto/free` 可能路由到会记录提示词的提供商 | 官方原文 |
| Zen | 免密层是**未公开契约**，可能随时变更或限流 | upstream-compat R8 契约 |

面板对每个免费来源**常驻**警示条（Kilo 为红色 danger 级），每个模型卡标注
`mayTrainOnYourPrompts`。**免费池不适合承载机密或敏感内容。**

---

## 3. 不进对外全局清单（D6）

**免费模型默认不进入对外的 `/v1/models`。**

`/v1/models` 是 OpenAI 生态约定的通用清单接口（客户端只认它），因此"对外不暴露"
必须真正成立。有**两条**独立的合并路径，只堵一条会漏：

| 路径 | 机制 | 阻断方式 |
|------|------|----------|
| ① 运行期实时目录 | `autoModels: true` 的上游，其实时拉取的目录被合并进来 | 免密来源固定 `autoModels: false` |
| ② 上游已声明清单 | **顶层 `models` 为空时**，`model_list` 无条件合并各上游自己声明的 `models` | 顶层 `models` 非空时全局清单只按白名单展示 |

因此一键接入的规则是：

- 固定写 `autoModels: false`（阻断路径 ①）；
- **不写顶层 `models`**（免费模型只登记在来源上游自己的清单里）；
- 顶层 `models` 为空时**连上游自己的 `models` 声明也不写**，并提示
  「顶层 models 为空：此时上游声明的模型会被自动并入对外的 /v1/models，与『免费模型不进入全局』冲突」。

结果（真机实测，隔离环境）：

```
接入前 GET /v1/models        → 4 个
接入后 GET /v1/models        → 4 个（新增 0 个）✅
接入后 GET /kilo/v1/models   → 19 个（内部可路由）✅
```

需要对外暴露时，在模型卡上显式点**「加入全局清单」**（带二次确认，写入顶层 `models`）。

---

## 4. 接口

| 端点 | 方法 | 作用 | 消耗额度 |
|------|------|------|----------|
| `/api/free-sources` | GET | 来源清单 + 接入状态 + 状态字典 + 拒因码 | 否 |
| `/api/free-sources/catalog` | POST | 拉取并筛选免费候选模型（零生成） | 否 |
| `/api/free-sources/probe` | POST | 单模型真实对话探测 | **是** |
| `/api/free-sources/probe-all` | POST | 批量探测（受退避与并发上限保护） | **是** |
| `/api/free-sources/attach` | POST | 一键接入（写 `bridge.config.json`） | 否 |

探测结果结构：

```jsonc
{
  "source": "kilo-free",
  "model": "minimax/minimax-m3.1-flash:free",
  "state": "available",          // available|transient|suspect|region|unavailable
  "reasonCode": null,             // 命中 rejectReasons 的键
  "reasonLabel": "上游池限流",
  "advice": "上游共享池已满，稍后重试；可换用其它免费模型",
  "status": 429,
  "raw": "Provider returned error; code: 429; ...",
  "latencyMs": 3121,
  "probedAt": "2026-10-21T00:00:00+08:00",
  "usage": { "promptTokens": 0, "completionTokens": 0 }
}
```

**不泄露**：响应体不含上游请求头、指纹头、会话 ID 或任何凭据；`raw` 只截断保留上游错误正文。

---

## 5. 额度保护

免费池的额度是稀缺资源（Kilo 匿名 **200 次/小时/IP**；Zen 免密层按 IP 限流），
探测本身也在消耗它。因此：

| 机制 | 行为 |
|------|------|
| 默认零生成 | 目录与水位查询不产生任何生成请求 |
| `full` 显式 | 真实对话探测需显式触发，UI 明示消耗 |
| 退避 | 同一 `(source, model)` 在 `transient` 状态下按 **30 → 120 分钟**指数退避；命中退避期的模型直接跳过并回填上次拒因 |
| 并发上限 | 批量探测分批并发，上限 3，避免同时打满 IP 额度 |
| 超时区分 | 超时（`upstream_timeout`）与网络不可达（`network_error`）分开归类，建议不同 |
| 探测预算 | `max_tokens ≥ 512`（默认 30s、上限 90s）：NVIDIA 系模型在 `max_tokens=64` 时 reasoning 吃满预算、返回 200 但正文为空，那不是"不可用" |

超时上限特意放宽到 90s：Kilo 的 `kilo-auto/free` 实测 **68.4s**（按会话动态路由），
`stealth/glyph-cluster` 26s。用 10s 上限会把正常模型误判成"网络不可达"——
2026-10-21 真机就踩到过这个坑（3 个 1.8–2.8s 的正常模型被判 `network_error`）。

---

## 6. 面板

第 6 个 tab「**免费来源**」：

- **来源卡**：名称、端点、免密标识、接入状态、**鱼缸水位**（可用模型占比）、隐私警示条、一键接入/移除。
- **模型卡**：模型 id、状态徽标、上下文长度、输入模态、思考档位、实测延迟、拒因 + 下一步建议、
  探测时间、单次基准测试、**「加入全局清单」**（显式，带二次确认）。
- **「不在可用清单」分组**：明确不可用的模型**保留可见**并附拒因与探测时间；
  后续探测通过后自动回到可用分组。

一键接入的确认框会明确写出「不填任何 API Key」「免费模型只登记在该来源自己的清单里，
不会自动出现在 /v1/models」。

---

## 7. 文件位置

| 文件 | 作用 |
|------|------|
| `rust/model-gate/free_sources.json` | 来源定义 + 拒因字典 + 状态定义（编译期内嵌，单一事实源） |
| `rust/model-gate/src/free_sources.rs` | 免费判定、拒因归一化、退避、一键接入配置构造 |
| `rust/model-gate/src/free_sources_api.rs` | 四个端点的实现 |
| `rust/model-gate/provider_profiles.json` | `kilo` provider 预设（免密） |
| `rust/model-gate/ui/free_sources.js` | 面板「免费来源」页 |
| `rust/model-gate/tests/fixtures/free_sources_golden.json` | 目录样本 + 拒因样本（golden） |

面板静态资源与 provider 预设随 `rust-only-autoupdate` 迁到 `rust/model-gate/`，
由 Rust 内核 `include_str!` 内嵌 —— **本模块是 Rust-only**。

---

## 8. 已知边界

- **Zen 免密层是未公开契约**：可能随时 403 / 429（2026-10-21 真机即遇到本机 IP 额度耗尽，
  全部免费模型返回 `429 FreeUsageLimitError`）。面板会把它归为「暂时不可用 / 上游池限流」，
  而不是"模型已下线"。
- **Kilo 免密池的可用性随时变**：官方明确「Availability changes; check the live model catalog」，
  面板的清单是实时拉取的，实测 2026-10-21 鱼缸水位 **11/20（55%）**。
- **不做**：账号登录/OAuth、Go/付费模型、额度绕过、Anthropic Messages / Responses 端点、
  参考仓库的 75 家聚合注册表、省 Token 压缩、combo 降级链、局域网中继、公告中心、应用内升级。
- **不自动进全局**：见 §3。
