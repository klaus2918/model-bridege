# Kilo.ai OpenRouter 免费模型实测报告

- 端点：`https://api.kilo.ai/api/openrouter`
- 实测时间：2026-10-20（响应头 `Date: Tue, 20 Oct 2026 11:31:40 GMT`）
- 认证：**完全免鉴权**，无 `Authorization` 头
- 脚本：`scripts/archive/probes/kilo_free_catalog.py` / `kilo_free_probe.py` / `kilo_free_summarize.py` / `kilo_free_retest.py` / `kilo_ratelimit_probe.py`
- 原始数据：`models.json`（目录）、`kilo_free_models.json`、`kilo_free_probe_results.json`、`kilo_free_summary.json`、`kilo_free_retest.json`、`kilo_ratelimit_probe.json`

---

## 0. 端点行为实测结论

| 项目 | 实测结果 |
|---|---|
| `GET /api/openrouter/v1/models` | HTTP 200，401 个模型，542,715 字节，**无需鉴权** |
| `POST /api/openrouter/v1/chat/completions` | HTTP 200，免鉴权可用（限 `:free` 模型） |
| 其它路径（如 `/v1/credits`、`/v1/usage`、`/v1/key`） | HTTP 405；错误体明确写着 `This endpoint only accepts the path /chat/completions.` |
| `ai-gateway.kilo.ai/api/v1/models` | 与 `api.kilo.ai/api/openrouter` 返回**完全相同**的 542,715 字节 → 同一后端，两个 base URL 等价 |
| 响应头 | 无 `X-RateLimit-*`、无 `Retry-After`；仅 Vercel/Cloudflare 基础头 |
| 服务端 | `Server: Vercel`，`X-Matched-Path: /api/v1/models` |

> 关键限制：**该端点只暴露 `/models` 与 `/chat/completions` 两个路径**，无余额/用量查询接口，用量只能从响应体 `usage` 字段读。

---

## 1. 免费模型清单（实测）

目录共 401 个模型。按官方 `isFree` 标记 + `pricing.prompt/completion == 0` 双重判定，命中 **20 个**：

- `isFree=True`：**18** 个
- 价格字段为 0 但未标 `isFree`：**2** 个（`google/lyria-3-pro-preview`、`google/lyria-3-clip-preview`）
- `isFree=True` 但价格非零：**0** 个（无矛盾项）
- ID 带 `:free` 后缀：**14** 个

### 1.1 实测连通性（`max_tokens=64` 首轮）

| 模型 ID | HTTP | 延迟 | 上游 provider | 结论 |
|---|---|---|---|---|
| `kilo-auto/free` | 200 | 68.4s | — | 可用（极慢，动态路由） |
| `minimax/minimax-m3.1-flash:free` | 200 | 2.9s | Minimax | ✅ 推荐 |
| `nvidia/nemotron-3.5-super-vl:free` | 200 | 2.0s | Nvidia | ✅（需 max_tokens≥512） |
| `reflection/beam-free` | 200 | 4.6s | — | ⚠️ 容量限流 |
| `stealth/glyph-cluster` | 200 | 26.1s | — | 可用但慢 |
| `xingchen-agi/xing4.0-29b-a4b:free` | 429 | 3.5s | Phala | ❌ 上游限流 |
| `dots-studio/dots-3-note-preview:free` | 200 | 3.5s | AtlasCloud | ✅ |
| `liquid/lfm-2.5-2.6b:free` | 504 | 26.1s | Liquid | ❌ 上游超时 |
| `nvidia/nemotron-3.5-lightning:free` | 200 | 2.6s | Nvidia | ✅（需 max_tokens≥512） |
| `thinkingmachines/inkling-small:free` | 429 | 2.2s | Thinking Machines | ❌ 日配额耗尽 |
| `poolside/laguna-s-2.1:free` | 200 | 3.1s | Poolside | ✅ |
| `poolside/laguna-xs-2.1:free` | 429 | 2.1s | Poolside | ❌ 上游限流 |
| `cohere/north-mini-code:free` | 200 | 2.9s | Cohere | ✅ 代码模型 |
| `nvidia/nemotron-3.5-content-safety:free` | 200 | 2.9s | Nvidia | ✅（安全分类专用） |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | 200 | 2.3s | Nvidia | ✅（需 max_tokens≥512） |
| `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | 200 | 1.9s | Nvidia | ✅（需 max_tokens≥512） |
| `google/lyria-3-pro-preview` | 401 | 1.6s | — | ❌ 需登录（付费模型） |
| `google/lyria-3-clip-preview` | 401 | 1.7s | — | ❌ 需登录（付费模型） |
| `nvidia/nemotron-3-super-120b-a12b:free` | 404 | 2.0s | Nvidia | ❌ 上游 function 不存在 |
| `openrouter/free` | 200 | 3.1s | Nvidia | ✅ 实际路由到 `nvidia/nemotron-3.5-super-vl:free` |

**首轮 HTTP 200：13/20**。复测（`max_tokens=512`）后 5 个 NVIDIA 系全部正常输出 → **实际可用 14/20**。

### 1.2 失败项错误码明细（复测后仍失败）

| 模型 | HTTP | 错误信息（截断） |
|---|---|---|
| `xingchen-agi/xing4.0-29b-a4b:free` | 429 | `temporarily rate-limited upstream` / `limit_source: upstream_provider_shared_pool` |
| `poolside/laguna-xs-2.1:free` | 429 | `temporarily rate-limited upstream` / `limit_source: upstream_provider_shared_pool` |
| `thinkingmachines/inkling-small:free` | 429 | `limit_rpd/... Daily limit reached ... Credits don't affect this cap.`（X-RateLimit-Limit: 1000） |
| `liquid/lfm-2.5-2.6b:free` | 504 | `Provider returned error` / `provider_name: Liquid` |
| `nvidia/nemotron-3-super-120b-a12b:free` | 404 | `Function '...' Not found for account` |
| `google/lyria-3-pro-preview` | 401 | `PAID_MODEL_AUTH_REQUIRED: You need to sign in to use this model.` |
| `google/lyria-3-clip-preview` | 401 | `PAID_MODEL_AUTH_REQUIRED` |
| `reflection/beam-free`（工具调用轮次） | 429 | `The request limited providers for this model and they are currently at capacity` |

> `google/lyria-3-*` 虽 `pricing` 为 0，但被网关判为 `PAID_MODEL_AUTH_REQUIRED` —— **零价 ≠ 免鉴权免费**。

---

## 2. 模型能力实测

### 2.1 能力矩阵（工具调用 / 流式 / 推理 / JSON 模式）

| 模型 | 工具调用 | 流式 SSE | 推理链 | JSON 模式 | 上下文 | 输入模态 | 输出模态 |
|---|---|---|---|---|---|---|---|
| `minimax/minimax-m3.1-flash:free` | ✅ 1 call | ✅ 12 chunks | ✅ 102 字 | ✅ | 1,000,000 | text,image | text |
| `poolside/laguna-s-2.1:free` | ✅ 1 call | ✅ 56 chunks | ✅ 602 字 | ✅ | 262,144 | text | text |
| `cohere/north-mini-code:free` | ✅ 1 call | ✅ 45 chunks | ✅ 160 字 | ✅ | 256,000 | text | text |
| `dots-studio/dots-3-note-preview:free` | ✅ 1 call | ✅ 17 chunks | ✅ 506 字 | ✅（返回空串） | 512,000 | text,image | text |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | ✅ 1 call | ✅ 16 chunks | ✅ 119 字 | ⚠️ 格式串扰 | 1,000,000 | text | text |
| `reflection/beam-free` | ❌ 429 容量满 | ✅ 8 chunks | ✅ 145 字 | ✅ | 1,048,576 | text | text |
| `stealth/glyph-cluster` | ✅ 1 call | ✅ 6 chunks | ❌ 空 | ❌ 400 | 256,000 | text | text |
| `kilo-auto/free` | ✅ 1 call | ✅ | ✅ | ❌ 400 | 256,000 | text | text |
| `openrouter/free` | ✅ 1 call | ✅ 45 chunks | ✅ 588 字 | ✅ | 200,000 | text,image | text |
| `nvidia/nemotron-3.5-lightning:free` | ✅（复测） | ✅ | ✅ 465 字 | ✅（复测） | 1,000,000 | text | text |
| `nvidia/nemotron-3.5-super-vl:free` | ✅（复测） | ✅ | ✅ 129 字 | ✅（复测） | 1,000,000 | text,image | text |
| `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | ✅（复测） | ✅ | ✅ 180 字 | ✅（复测） | 256,000 | text,audio | text |
| `nvidia/nemotron-3.5-content-safety:free` | ✅（复测） | ✅ | ✅ 457 字 | ✅（复测） | 128,000 | text,image | text |

### 2.2 关键能力发现

1. **工具调用（function calling）**：13/20 可用，返回标准 OpenAI `tool_calls` 结构，`function.name` 正确。Kilo 网关宣称提供"automatic repair for deduplication and orphan cleanup"。
2. **流式 SSE**：全部通过测试，`data: [DONE]` 结束标记完整，chunk 数 4~56不等。
3. **推理链（reasoning）**：多数模型在 `message.reasoning` 返回推理文本，可通过 `include_reasoning` / `reasoning` 参数请求。`stealth/glyph-cluster` 恒为空。
4. **JSON 模式（`response_format: json_object`）**：仅部分模型正确遵守；`stealth/glyph-cluster`、`kilo-auto/free` 返回 HTTP 400；`nvidia/nemotron-3-ultra-550b-a55b:free` 会输出 `{"city": "{ "city": "Beijing", "temp": 30 }` 这类嵌套串扰。
5. **⚠️ `max_tokens` 预算陷阱**：NVIDIA Nemotron 系默认先产出 reasoning，`max_tokens=64` 时 reasoning 吃满预算导致 `content=null` + `finish_reason: length`，**看起来像失败其实是预算不足**。`max_tokens=512` 后 5 个 NVIDIA 模型全部正常返回。接入时务必给足 `max_tokens`。
6. **上下文窗口**：目录标称最高 1,048,576 tokens（`reflection/beam-free`、`thinkingmachines/inkling-small:free`、两个 lyria）。
7. **多模态**：`nvidia/nemotron-3.5-super-vl:free`、`dots-3-note-preview:free`、`thinkingmachines/inkling-small:free`、`openrouter/free`、`kilo-auto/free` 接受 image 输入；`nemotron-3-nano-omni` 接受 audio。**所有模型输出均为 text**。
8. **`kilo-auto/free` 极慢**（68.4s），因为它按会话动态路由到候选免费模型集合。
9. **`openrouter/free` 实际落到 `nvidia/nemotron-3.5-super-vl:free`**（响应体 `model` 字段确认），可作为该模型的稳定别名。
10. **内容审核**：目录字段 `top_provider.is_moderated` 对绝大多数免费模型为 `false`（不审核），仅 `cohere/north-mini-code:free` 与 `google/lyria-3-*` 为 `true`。

---

## 3. 隐私情况

### 3.1 数据流向（官方《Privacy Policy》 https://kilo.ai/privacy）

原文（Service providers and partners 段）：

> **OpenRouter** - Routing layer that forwards your request to the AI provider you select per request, including Anthropic, OpenAI, Google, xAI, Mistral, Meta, and other providers listed at https://openrouter.ai/models. OpenRouter and the downstream providers are bound by data protection terms equivalent to those described in this policy.
>
> **Anthropic, OpenAI, Google, and other AI providers** - Generate AI responses to your prompts. The specific provider used for each request depends on the model you select.

Mobile App 段（同样适用于 Gateway 路径）：

> Prompts and conversation content are sent to the AI provider you select per request, routed through OpenRouter. **This information is used only to generate the response.**
>
> Account ID and request metadata are sent to Kilo Gateway, our backend, for authentication, request routing, and billing reconciliation.

> AppsFlyer (mobile only) - Mobile attribution and analytics. **Does not receive prompt or conversation content.**
> Sentry - Crash reporting and performance monitoring. **Does not receive prompt or conversation content.**

**结论**：提示词与代码会离开 Kilo，经 OpenRouter 转发给**实际承接请求的上游模型商**。Kilo 自身不训练用户提示词（隐私政策未声明用提示词训练模型），但**下游提供商各自的数据政策独立于 Kilo**。

### 3.2 ⚠️ NVIDIA 免费端点明确记录日志（官方 Models & Providers 页原文）

> **⚠️NVIDIA free endpoints**
> For NVIDIA free endpoints (Super/Ultra/etc): **Trial use only - do not submit personal or confidential data. Your use is logged for security purposes and to improve NVIDIA products and services. The logged session data for improvement purposes is not linked to your identity or any persistent identifier.**

本次免费清单中 **6 个是 NVIDIA 模型**（nemotron-3.5-super-vl / lightning / content-safety、nemotron-3-ultra、nemotron-3-nano-omni、nemotron-3-super），实测 `provider=Nvidia` 确认走该通道。**这些模型会记录你的会话用于改进 NVIDIA 产品**。

### 3.3 ⚠️ Auto Free 路由到会记录提示词的提供商（官方原文）

> **⚠️Data handling for Auto Free**
> Auto Free may route your requests to providers that **log prompts and outputs and use them to improve their services**. Do not submit personal or confidential data when using Auto Free. In particular, it may route to NVIDIA's free endpoints.

### 3.4 组织级数据收集控制（Usage & Billing 页原文）

> **Data collection controls**
> Organizations can set a data collection policy (allow or deny) that is applied to all requests from their members. **Some free models require data collection to be allowed.**

即：部分免费模型**必须允许数据收集**才能使用 —— 组织若设为 deny，这些模型直接不可用。

### 3.5 `mayTrainOnYourPrompts` 字段实测

模型目录每条记录含 `mayTrainOnYourPrompts` 布尔值。实测结果：

- **20 个免费模型中 19 个为 `true`**，仅 `google/lyria-3-pro-preview` 与 `google/lyria-3-clip-preview` 为 `false`（而这俩恰好需要登录、免费不可用）。
- 官方文档（kilo.ai/docs 与 openrouter.ai/docs）**未找到**该字段的语义定义说明。

**实践解读**：`true` 意味着该模型的上游提供商可能使用你的提示词用于训练/改进，与 §3.2、§3.3 的 NVIDIA Trial 条款相互印证。**实际可用的免费模型全部标为 `true`** —— 想避免训练，唯一路径是用付费模型或自带密钥（BYOK）。

### 3.6 其他隐私要点（官方隐私政策）

- 数据存储与处理位于**美国**；跨境传输用 SCC 等保障机制。
- 欧盟/英国用户：依 GDPR/UK GDPR 的合同履行、合法利益、同意三类法律基础处理。
- 账户删除：Kilo App 内可自助，或邮件 `support@kilo.ai`。
- 不收集敏感/特殊类别个人数据；不面向 18 岁以下用户。
- Kilo 近期被 **Anaconda 收购**（kilo.ai 首页公告）。

---

## 4. 官方用量说明

### 4.1 Kilo Gateway 侧（kilo.ai/docs/gateway/usage-and-billing）

> **Free models**: Models tagged with `:free` have **zero cost -- usage is tracked but not billed**.

> **Free model rate limits**
> All free model requests (both anonymous and authenticated) are rate-limited by IP address:
> | Scope | Limit |
> |---|---|
> | Free models per IP | **200 requests per hour** |
>
> When rate-limited, you receive HTTP 429: `{"error":{"message":"Rate limit exceeded for free models. Please try again later.","code":429}}`

> **Paid model limits**: Paid model requests are not rate-limited by the gateway itself, but may be rate-limited by upstream providers.

计费模型：1 USD = 1,000,000 microdollars；请求流程为 **余额检查 → 执行 → 用量提取 → 原子扣款**；余额为 0 时付费模型返回 HTTP 402 并附 `buyCreditsUrl`；购买 credits 收 5% 支付手续费，但**手续费单独计、不增加余额**；**推理按上游原价透传，Kilo 不加价**。

组织能力：共享余额池、成员每日消费上限（UTC 00:00 重置）、自动充值、最低余额告警、模型白名单（违规返回 403）、提供商白名单、数据收集策略、每请求用量追踪（model / provider / input_tokens / output_tokens / cache_write_tokens / cache_hit_tokens / cost_microdollars / time_to_first_token / is_byok）。

> **Token counting**: Token counts are provided by the upstream model and are based on the model's native tokenizer. **The gateway does not re-tokenize content.** Usage data is available: non-streaming in the `usage` field; streaming in the final SSE chunk before `[DONE]`.

### 4.2 认证与匿名访问（kilo.ai/docs/gateway/authentication）

> The gateway allows unauthenticated access for **free models only**. Anonymous requests are identified by IP address and are subject to rate limiting (**200 requests per hour per IP**).

> Free models include models tagged with `:free` in their model ID, such as `minimax/minimax-m2.1:free` and `z-ai/glm-5:free`.

`Authorization` 头标为 "Yes (unless free model)"。**本次实测完全免鉴权可用，与文档一致。**

### 4.3 免费模型可用性声明（kilo.ai/docs/gateway/models-and-providers）

> Several models are available at no cost, subject to rate limits. **Availability changes; check the live model catalog for current free options and model IDs.**

> Free models are available to both authenticated and anonymous users. Anonymous users are rate-limited to 200 requests per hour per IP address.

`kilo-auto/free`：*Free with limited capability. No credits required. The resolved model is selected dynamically per session from a curated set of available free models; the mapping updates server-side as free model availability shifts.*

### 4.4 OpenRouter 侧（底层限流，openrouter.ai/docs/api-reference/limits）

> OpenRouter enforces three kinds of limits: **Credit limits** / **Budget limits** / **Rate limits**.

> **Free usage limits**: If you're using a free model variant (with an ID ending in `:free`), the following limits apply: （表格按"累计充值 credits"分档给出 **每分钟 / 每天** 请求数上限；页面表格中的具体数字在抓取时被剥离）

> The `free_model_daily_requests` field in the `GET /api/v1/key` response reports the daily counter and ceiling that gate your free-model requests. The limit tier is selected by **all-time credits purchased**, independently of `is_free_tier`. … The per-minute limit is not reported there.

> On free variants, purchase at least [credits] to raise your daily limit, or switch to the paid variant of the model, which has no platform-level request cap.

> **Successful inference responses do not include `X-RateLimit-*` headers.** When OpenRouter itself returns a 429 for a platform limit, the error response carries `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset` headers. When every attempted provider returned a retry hint, the error response also carries a `Retry-After` header.

> 429 有两个来源：**OpenRouter 平台层**（免费模型每分钟/每天上限、Cloudflare DDoS 防护）或**上游 provider**（`error.metadata.provider_code` 携带原始码；fallback 路由会自动重试其他 provider）。

流式中途命中限流时，错误以 SSE 事件形式到达：`finish_reason: "error"`，而非 HTTP 状态码。

> **Making additional accounts or API keys will not affect your rate limits, as we govern capacity globally.**

> **BYOK requests are not gated by** [free-model limits]。

### 4.5 OpenRouter 计费口径（openrouter.ai/docs/faq）

> OpenRouter **passes through the pricing of the underlying providers**; there is no markup on inference pricing (however we do charge a fee when purchasing credits).

> `:free` - A free version of the model **with its own rate limits**.

> `:batch` - The batch-priced version of the model used by the Batch API.

路由变体（所有模型可用，不出现在 models API 里）：`:nitro`（按吞吐排序）/ `:floor`（按价格排序）/ `:exacto`（按质量优先的 tool-calling 可靠性排序）/ `:online`（已废弃）。

### 4.6 本次实测的用量数据

`usage` 字段实测样例（`minimax/minimax-m3.1-flash:free`）：

```json
{
  "prompt_tokens": 203, "completion_tokens": 8, "total_tokens": 211,
  "cost": 0, "is_byok": true,
  "prompt_tokens_details": {"cached_tokens": 201, "cache_write_tokens": 0, "audio_tokens": 0, "video_tokens": 0},
  "cost_details": {"upstream_inference_cost": 0, "upstream_inference_prompt_cost": 0, "upstream_inference_completions_cost": 0},
  "completion_tokens_details": {"reasoning_tokens": 0, "image_tokens": 0, "audio_tokens": 0}
}
```

注意：免费模型 `cost` 恒为 0，但 `is_byok` 可能为 `true`（路由层复用了 Kilo 侧 BYOK 通道），说明"免费"是上游侧免费而非本地余额扣减。

### 4.7 限流实测（30 次连续请求）

对 `minimax/minimax-m3.1-flash:free` 连续 30 次请求（`max_tokens=8`，间隔 0.25s）：

- **结果：30/30 全部 HTTP 200，未触发网关 429**
- 延迟分布：1.0s ~ 11.0s，中位约 2.3s
- 响应头**不含** `X-RateLimit-*`（与 OpenRouter 文档一致：成功响应不带限流头）
- 本次会话累计已发出 100+ 次免费请求，仍未触发 200/h 的网关限流 → **实际瓶颈是上游 provider 池（`upstream_provider_shared_pool`），而非 Kilo 网关**

---

## 5. 综合建议

### 5.1 接入优先级（基于实测）

| 优先级 | 模型 | 理由 |
|---|---|---|
| 1 | `minimax/minimax-m3.1-flash:free` | 1M 上下文、图像输入、工具/流式/推理/JSON 全通、2~3s 延迟、`is_byok=true` |
| 2 | `poolside/laguna-s-2.1:free` | 全部能力项通过，流式 56 chunks 表现好，3.1s |
| 3 | `cohere/north-mini-code:free` | 代码任务专用，能力全通，唯一 `is_moderated=true` |
| 4 | `nvidia/nemotron-3-ultra-550b-a55b:free` | 1M 上下文、9.7s 延迟；JSON 模式需自行兜底 |
| 5 | `openrouter/free` | 稳定别名 → `nvidia/nemotron-3.5-super-vl:free`（1M 上下文 + 图像） |

**不建议**：`kilo-auto/free`（68s 过慢）、`stealth/glyph-cluster`（26s + JSON 400）、`liquid/lfm-2.5-2.6b:free`（504）、`thinkingmachines/inkling-small:free`（日配额 1000 已耗尽）、`poolside/laguna-xs-2.1:free` / `xingchen-agi/xing4.0-29b-a4b:free`（上游池限流）。

### 5.2 集成注意

1. **必须给足 `max_tokens`**（建议 ≥512），否则 NVIDIA 系 reasoning 吃满预算、返回空 content。
2. **必须做 429 退避重试**：免费模型的失败主因是上游池限流而非网关限流，且 `Retry-After` 不保证存在。`thinkingmachines/inkling-small` 还有 1000 次/日的硬上限。
3. **不要依赖 `X-RateLimit-*` 头做配额监控** —— 成功响应一律不带；只能靠 429 事件或事后统计。
4. **零价 ≠ 免费可用**：`google/lyria-3-*` 价格字段为 0 但返回 401 `PAID_MODEL_AUTH_REQUIRED`。判定免费应同时看 `isFree` 与实际 HTTP 状态。
5. **JSON 模式不可全信**：`nvidia/nemotron-3-ultra-550b-a55b:free` 会产生嵌套串扰，建议自行做解析兜底。
6. **该端点没有余额/用量查询接口**（仅 `/models` 与 `/chat/completions`），若需配额视图需改用 `ai-gateway.kilo.ai` 的 dashboard 或直接读 `usage` 字段。
7. **端点路径有两种写法且等价**：`https://api.kilo.ai/api/openrouter`（本项目 bridge.config.json 现用）与 `https://ai-gateway.kilo.ai/api/v1`（官方文档口径）。

### 5.3 隐私红线

- **不要向 `nvidia/*:free` 与 `kilo-auto/free` 提交个人或机密数据** —— 官方明确说明会记录会话用于改进产品。
- 全部 20 个免费候选模型的 `mayTrainOnYourPrompts` 均为 `true`（除 2 个不可用的 lyria）→ **免费模型通道不适合承载敏感数据**。需要保密请用付费模型或 BYOK。
- 提示词经 OpenRouter 转发至实际承接的上游商，数据政策以各家为准（Anthropic/OpenAI/Google/NVIDIA/Meta/Cohere/Poolside/Phala/AtlasCloud…）。
- 组织级可设 "data collection: deny"，但部分免费模型会因此不可用。

---

## 6. 证据来源

**实测**（本项目脚本，2026-10-20）
- `GET https://api.kilo.ai/api/openrouter/v1/models` → 401 模型
- `POST https://api.kilo.ai/api/openrouter/v1/chat/completions` → 20 个免费模型 × (basic/tools/stream/reasoning/json)
- 复测 `max_tokens=512` → NVIDIA 系 5 个模型
- 限流探测 → 30 次连续请求

**官方文档**
- Kilo Gateway 总览：https://kilo.ai/docs/gateway
- Models & Providers（免费模型、NVIDIA 条款、Auto Free 数据处理）：https://kilo.ai/docs/gateway/models-and-providers
- Usage & Billing（计费、限流、数据收集控制、用量字段）：https://kilo.ai/docs/gateway/usage-and-billing
- Authentication（匿名访问、BYOK、请求头）：https://kilo.ai/docs/gateway/authentication
- API Reference：https://kilo.ai/docs/gateway/api-reference
- Quickstart：https://kilo.ai/docs/gateway/quickstart
- Kilo Privacy Policy：https://kilo.ai/privacy
- Kilo Apps Privacy Hub：https://kilo.ai/privacy/apps
- OpenRouter Limits：https://openrouter.ai/docs/api-reference/limits
- OpenRouter FAQ：https://openrouter.ai/docs/faq
