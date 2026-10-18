# model-bridge — 使用说明

> **桌面版（推荐）**：`dist\Model-Gate.exe` 是同一套能力的 Windows 桌面版：本地代理 + 模型配置面板
> + token 用量统计（SQLite 落库）。启动后访问 `http://127.0.0.1:8900/ui/` 打开面板；
> 无 GUI 环境可用 `Model-Gate.exe --no-gui --port 8911` 以纯代理模式运行。
> 本文其余小节描述的行为对 Node 版（`node model-bridge.js`）与桌面版**完全一致**。
>
> **密钥安全**：桌面包会把**非空明文 `apiKey` 拦在内存里**，不写入 `bridge.config.json`
> 与 SQLite。要长期使用请用 `apiKeyEnv`（读 `~/.jcode/.env`）或 `apiKeyFile`
> （读 `~/.commandcode/auth.json`）；分享配置用 `bridge.config.example.json`（零明文）。
> `bridge.config.json` 已在 `.gitignore` 中，不会进版本库。

一个本地代理，把 **OpenAI 格式**的请求按模型名分发到**四条上游**，并自动处理各上游的协议差异。

```
Agent（OpenAI 格式）
   ↓  http://127.0.0.1:8900/v1            ← 按模型名自动分发
   ↓  http://127.0.0.1:8900/go/v1         ← 用 URL 前缀强制指定上游
model-bridge
   ├─ cch         (Anthropic 协议)   → 127.0.0.1:15721 → CCH → DeepSeek
   ├─ go          (OpenAI 协议)      → opencode.ai/zen/go/v1  ← Go 订阅套餐
   ├─ zen         (OpenAI 协议)      → opencode.ai/zen/v1     ← 免费模型
   └─ commandcode (CommandCode 协议) → api.commandcode.ai/alpha/generate ← CommandCode 订阅
```

---

## 一、怎么指定与识别上游

**两种方式，可混用。**

### 方式一：按模型名自动分发（默认）

用统一的 `http://127.0.0.1:8900/v1`，bridge 按 `model` 字段查各上游的 `models` 清单，命中谁走谁：

```
model: deepseek-flash    → cch
model: mimo-v2.6-flash    → go
model: space-bunny-free   → zen
model: (未声明的名字)     → 默认上游（cch）
```

简单，但**同名的模型会取配置里靠前的那个上游**，你无法指定。

### 方式二：用 URL 前缀强制指定（推荐）

在地址里带上游名，直接锁定，不看模型名：

| 地址 | 效果 |
|------|------|
| `http://127.0.0.1:8900/cch/v1` | 强制走 cch |
| `http://127.0.0.1:8900/go/v1` | 强制走 go |
| `http://127.0.0.1:8900/zen/v1` | 强制走 zen |
| `http://127.0.0.1:8900/commandcode/v1` | 强制走 commandcode |

例：
```bat
:: 同样叫 deepseek-v4-flash 的模型，明确走 go 而不是 cch
curl http://127.0.0.1:8900/go/v1/chat/completions ^
  -H "Content-Type: application/json" ^
  -d "{\"model\":\"deepseek-v4-flash\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"

:: 只看 go 有哪些模型
curl http://127.0.0.1:8900/go/v1/models
```

前缀写错会明确报错并列出可用前缀，**不会静默走错上游**：
```json
{"error":{"message":"未知的上游前缀 \"nosuch\"。可用前缀：/cch、/go、/zen、/commandcode；..."}}
```

### 怎么知道这次实际用了哪个上游

**看响应头**，每次响应都带：

```
x-bridge-upstream: go
x-bridge-upstream-model: deepseek-v4-flash
```

流式响应同样带这两个头。另外控制台日志每行也有：
```
deepseek-v4-flash -> go:deepseek-v4-flash 200 2100ms tokens=41+49
                        ↑上游   ↑实际发给上游的模型名
```

`GET /health` 会列出每条上游的访问地址：
```bat
curl http://127.0.0.1:8900/health
```
```json
{"upstreams":[{"name":"go","endpoint":"http://127.0.0.1:8900/go/v1",...}]}
```

### 该选哪种

- **只有一个上游、或模型名不冲突** → 方式一，省事
- **想明确控制、或模型名跨上游重复** → 方式二
- **给不同 Agent 分配不同上游** → 方式二最合适，各填各的带前缀地址

---

## 二、它解决什么问题

两个痛点：

1. **协议错配**：Agent 只会发 OpenAI 格式的 `/v1/chat/completions`，而 CCH 那条上游只认 Anthropic 格式的 `/v1/messages`、CommandCode 只认自己的 `POST /alpha/generate`（返回 **NDJSON 事件流**而非 SSE），都得在中间翻译
2. **opencode 的会话头要求**：opencode.ai 要求每个请求带 `x-opencode-session` 头，缺失直接 400 拒绝（`MissingSessionID`），而普通客户端不会发这个头
3. **CLI 专属的凭据与版本门槛**：CommandCode 要求 `x-command-code-version` 头（缺失或过旧直接 403 `upgrade_required`），凭据也只存在 CLI 自己的登录文件里

model-bridge 把这些差异都收敛到一个进程里，Agent 只需指向一个地址。

---

## 三、快速开始

### 1. 启动

桌面版（推荐）：双击 `dist\Model-Gate.exe`。托盘出现图标，面板自动打开 `http://127.0.0.1:8900/ui/`；**关窗不退出**，托盘菜单退出才停。

旧版 Node 单文件仍可用（行为一致）：`node model-bridge.js`（窗口要开着，关掉即停）。

### 两种运行模式（同一个 exe）

| 模式 | 启动 | 形态 | 适用 |
|---|---|---|---|
| 桌面（默认） | `Model-Gate.exe` | 面板 + 托盘；关窗不退出 | 本机日常 |
| 无界面（headless） | `Model-Gate.exe --no-gui --port 8911` | 纯代理，无窗口无托盘，Ctrl+C 停 | 服务器 / CI / 远程开发 |

**关系**：同一份 exe、同一套配置解析、同一个库结构，差别只在「有没有窗口」；`--no-gui` 不会跳过任何代理能力。

**不能共用同一个数据目录**：单实例锁以 `modelgate.db` 所在目录为粒度，第二个实例会被判为重复启动并退出。要并存（灰度切换 / 并行验证）必须同时满足「端口不同 + `--db` 指向不同目录」，配置也建议用副本：

```bat
:: 桌面实例：默认 8900，读写 dist\bridge.config.json + dist\data\modelgate.db
Model-Gate.exe
:: 并行验证实例：8911 + 独立配置副本 + 独立库
Model-Gate.exe --no-gui --port 8911 --config D:\tmp\mg\bridge.config.json --db D:\tmp\mg\data\modelgate.db
```

切换版本时按 PID 停旧实例（`dist\bridge.config.json` 与 `dist\data` 不动），客户端 `base_url` 无需修改。

### 2. 验证

```bat
curl http://127.0.0.1:8900/v1/models
```
返回模型列表即正常。

---

## 四、Agent 侧怎么填

| 配置项 | 填写值 |
|--------|--------|
| base_url / API 地址 | `http://127.0.0.1:8900/v1` |
| api_key / API Key | 任意值（本地不校验） |
| model / 模型名 | 见下方模型清单 |

```bat
set OPENAI_BASE_URL=http://127.0.0.1:8900/v1
set OPENAI_API_KEY=any-key
```

---

## 五、可用模型

### cch（本地代理 → CCH，默认上游）

配置里固定声明这三个模型名：

| 模型名 | 说明 |
|--------|------|
| `deepseek-flash` | 默认上游；未匹配到其它上游的模型名也都走这里 |
| `mimo-v2.5-pro` | |
| `glm5.3` | 注意是 `glm5.3`（无横线），与 go 的 `glm-5.3` 是两个不同的名字 |

> **重要：CCH 这条上游不按请求里的模型名路由。**
> 实际用哪个模型由 **cc-switch 那边的当前供应商**决定。也就是说上面三个名字目前都会打到
> cc-switch 此刻选中的那个模型上，请求里的 `model` 只是被透传、并不生效。
> 想真正切换模型，要去 cc-switch 里换供应商；本工具负责的是「把模型名原样送过去」。
>
> 实测佐证：用任意编造的名字（如 `made-up-xyz`）请求，同样返回 200，响应 `model` 字段恒为
> cc-switch 当前选中的模型。

### go（opencode Go 订阅）

上游 `/zen/go/v1/models` 当前列 **43 个**（2026-10-08 快照）。但 `bridge.config.json` 里 go 只声明了 **4 个**
——按名分发**只认这份手工清单**，上游有的模型不写进来就路由不到：

| 配置中已声明 | 说明 |
|------|------|
| `glm-5.3-flash` | GLM 系列的 flash 档 |
| `deepseek-v4.1-flash` | DeepSeek 系列的 flash 档 |
| `mimo-v2.6-flash` | MiMo V2.6，与 CommandCode 上的 MiMo V2.6 Flash 同款 |
| `longcat-2.5-preview-free` | 免费层，在 go 上可外部调用（同名模型在 zen 上是 403） |

2026-09-22 曾逐个实测 chat 路径可用 29 个；2026-10-08 抽查其中 9 个（`glm-5.3-flash`、
`deepseek-v4.1-flash`、`mimo-v2.6-flash`、`longcat-2.5-preview-free`、`space-bunny`、`hy4-preview`、
`hy3`、`omen-alpha`、`minimax-m3`）**全部仍返回 200**：

| 系列 | 模型 |
|------|------|
| GLM | `glm-5.3-flash`、`glm-5.3`、`glm-5.2`、`glm-5.1` |
| Kimi | `kimi-k3`、`kimi-k2.7-code`、`kimi-k2.6` |
| DeepSeek | `deepseek-v4.1-flash`、`deepseek-v4-flash`、`deepseek-v4-flash-vision-exp`、`deepseek-v4-pro`、`deepseek-flash` |
| MiniMax | `minimax-m3`、`minimax-m2.5` |
| Qwen | `qwen3.8-max`、`qwen3.8-flash`、`qwen3.7-max`、`qwen3.7-plus`、`qwen3.6-plus` |
| MiMo | `mimo-v2.6-flash`、`mimo-v2.6-pro`、`mimo-v2.5-pro`、`mimo-v2.5` |
| 其它 | `longcat-2.0`、`longcat-2.5-preview-free`、`hy4-preview`、`hy3`、`omen-alpha`、`space-bunny` |

> ⚠️ **`mimo-v2.6-flash-free` 不在 go 上**（2026-10-08 实测 400 `Model is unavailable`）：它只存在于 zen 目录，
> 而 zen 侧又被免费层限制挡住（见下一节）。配到 go 里只会稳定报 400，已移除。

### zen（免费，无需密钥）

上游 `/zen/v1/models` 当前列 **87 个**（2026-10-08 快照），但能被外部客户端调用的仍然**只有 1 个**
（2026-09-22 与 2026-10-08 两轮复测结论一致）：

| 模型 | 说明 |
|------|------|
| `space-bunny-free` | 免费、无需密钥，**有限流** |

> **其余 86 个为什么没列**：
> - 所有 `-free` 后缀的免费层模型 → 403 `FreeTierError: OpenCode's free tier can only be used from
>   within OpenCode`（**带上 API key 也一样**，只能在 OpenCode 客户端内部用，外部代理进不去）：
>   `mimo-v2.6-flash-free`、`longcat-2.5-preview-free`、`ling-3.1-flash-free`、`ling-3.0-flash-fin-free`、
>   `fledge-alpha-free`、`nemotron-3-ultra-free`、`nemotron-3.5-lightning-free`、`exo-free`
> - `muse-spark-1.2-contributor-free`、`muse-spark-1.3-contributor-free` → 403 `RegionError`（地区限制）
> - `jev-1.13-free` → 500（上游自身故障）
> - 付费档（`claude-*`、`gpt-*`、`grok-*`、`mistral-large-4`、`qwen3.8-max` 等）→ 不带 key 直接 401
>   `AuthError`；带 key 后 `claude-*` / `gpt-6*` / `grok-*` 又是 400 `ModelProtocolUnsupported`（需
>   `/v1/responses` 协议），而 `qwen3.8-max`、`deepseek-v4.1-flash` 虽可用，go 里已有同名、按名分发优先
>   命中 go，配进 zen 毫无意义
>
> **旧清单已全部失效**：`mimo-v2.5-free`（401 `ModelError`）、`big-pickle`（403 `FreeTierError`）
> 已从配置移除。

### commandcode（CommandCode 订阅，复用你已登录的凭据）

| 模型 | 说明 |
|------|------|
| `deepseek/deepseek-v4-pro`、`deepseek/deepseek-v4-flash`、`deepseek/deepseek-v4.1-flash`、`moonshotai/Kimi-K3`、`zai-org/GLM-5.3`、`MiniMaxAI/MiniMax-M3`、`xiaomi/mimo-v2.5-pro` | 配置里已声明；完整目录见 CommandCode 的 `/model` |

- **协议**：CommandCode 是私有接口（`POST /alpha/generate`），既不是 OpenAI 也不是 Anthropic。
  本桥在内部把 OpenAI 请求翻译过去、再把它的 **NDJSON 事件流**翻译回 OpenAI 流，**Agent 侧无感**。
- **凭据**：桥按 CommandCode 自己的顺序取 key——先 `COMMAND_CODE_API_KEY` 环境变量，
  再本机 `~/.commandcode/auth.json` 的 `apiKey`。所以**只要本机 `cmd` 登录过就能直接跑**，无需额外配置；
  两处都没有时本地直接返回 401，不会把必然失败的请求打到上游。密钥只驻内存，不落盘、不进日志。
- **版本头**：`x-command-code-version` 是服务端硬门槛（缺失或过旧 → 403 `upgrade_required`）。
  桥默认读本机已安装的 command-code 版本，读不到则退回 npm registry（30 分钟缓存），
  也可用 `cliVersion` 固定一个值。
- **前缀**：`http://127.0.0.1:8900/commandcode/v1`
- **模型名必须精确**：上游按完整 id 匹配，不接受缩写（如要写 `deepseek/deepseek-v4.1-flash`，
  而不是 `deepseek-v4.1-flash`）。需要短名可给该上游配 `modelMap`。
- **已知差异**：本桥不复刻 CLI 的机器指纹上报与遥测（与生成请求无关）；
  图片需为 data URL（非 data URL 会被跳过）；
  下游若要求**强制工具选择**（`tool_choice: required`），本桥不下发该字段、按 `auto` 处理并打日志（与 cch 分支的降级行为一致）；
  套餐档位**在启动时探测一次**，升级套餐后需重启桥才会刷新清单。
- **缓存用量**：上游的缓存命中量透出为 `usage.prompt_tokens_details.cached_tokens`，访问日志同步可见。
- **`pause_turn` 自动续跑**：上游有时会在回答中途返回 `pause_turn`（表示"这一轮先到这，请继续"）。
  本桥收到后会**用同一份请求体重发**把回答接完，最多续 **5 次**（共 6 次请求）——与上游 CLI 的做法一致，
  续跑期间正文/思考/工具调用连续下发，`usage` 跨次累加。想关掉就设 `"pauseTurn": false`（此时收到就收尾，并在访问日志里标 `pause_turn 未续完`）。

配置形态（`bridge.config.json` 已内置，密钥只以引用形式出现）：

```json
"commandcode": {
  "protocol": "commandcode",
  "baseUrl": "https://api.commandcode.ai",
  "apiKeyEnv": "COMMAND_CODE_API_KEY",
  "apiKeyFile": "~/.commandcode/auth.json",
  "cliVersion": "auto",
  "cliEnvironment": "production",
  "modelCatalog": "auto",
  "modelCatalogPlan": "auto",
  "pauseTurn": true,
  "fetchModels": false,
  "models": [
    "deepseek/deepseek-v4.1-flash",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash",
    "moonshotai/Kimi-K3",
    "zai-org/GLM-5.3",
    "MiniMaxAI/MiniMax-M3",
    "xiaomi/mimo-v2.5-pro"
  ]
}
```

> **模型清单怎么来**：CommandCode 没有 `/v1/models` 端点，所以清单 = 配置里 `models` 声明的
> **＋ 本机 CLI 自带权威目录**（`command-code/dist/bundled/.../models.md`）合并去重。
> `modelCatalog` 取值：`"auto"`（读本机 CLI，默认）/ `false`（只用配置声明）/ 具体文件路径。
> 本机没装 CLI 时自动退回配置声明的那几个，不报错。
>
> **清单会自动按你的套餐过滤**：桥启动后查一次 `/alpha/billing/subscriptions` 取套餐档位
> （如 `individual-goat` → GOAT），再用目录里的「Min plan」列剔除超出套餐的模型。
> 实测（GOAT 账号）：目录 70 个 → 对外 **50** 个；Max 档 `claude-opus-5`、Pro 档 `claude-sonnet-5` 被剔除。
> `modelCatalogPlan` 取值：`"auto"`（探测，默认）/ `go` / `goat` / `pro` / `max`（显式指定）/ `false`（不过滤）。
> 探测失败时**不过滤**，原因写在 `/health` 的 `planError` 里。
>
> **被过滤掉的模型仍可按名直呼**（例如你临时买了按需额度）：桥照旧路由到 CommandCode，
> 放行与否交给上游——超出套餐会得到 403 `MODEL_NOT_IN_PLAN`（终态错误，不重试）。
>
> `/health` 每个上游会报 `models`（对外数量）、`modelsAll`（全集）、`plan`、`planError`，便于排障。
>
> `modelMap` 是**上游级**短别名（可选，本仓库默认未配；不污染全局 `aliases`）：配置后下游可直接用 `cc-flash`，
> 桥按「上游级别名 → 全局别名 → 原样」解析，并把短名原样回给下游。
> **`cliVersion`** 留 `"auto"` 即可（读本机安装的 command-code 版本）；要固定版本就写死如 `"1.53.1"`。

### 同名冲突说明

`deepseek-flash` 在 cch 和 go 里都有。不带前缀时会走 **cch**（配置里在前）。要明确走 go：

```bat
curl http://127.0.0.1:8900/go/v1/chat/completions -H "Content-Type: application/json" ^
  -d "{\"model\":\"deepseek-flash\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
```

### Go 里当前不可用的模型（已实测排除，未列入配置）

| 模型 | 状态 | 原因 |
|------|------|------|
| `grok-4.6`、`grok-4.7`、`gpt-5.6-luna`、`gpt-6-luna`、`claude-haiku-5-5`、`minimax-m2.7` | ✗ | 400 `ModelProtocolUnsupported: Model does not support this protocol.`——需 `/v1/responses` 协议，本桥只发 chat（2026-09-22 同日早些时候这几个报的是 503 `Endpoint is unavailable`，上游当天有波动） |
| `muse-spark-1.3-contributor`、`muse-spark-1.2-contributor` | ✗ | 403 `RegionError: This model is not available in your country`（同日早些时候报的是 400「需开启允许在请求数据上训练的付费端点」，两种都过不去） |
| `grok-4.5` | ✗ | 需 `/v1/responses` 协议；当前上游 `/models` 已不列出 |
| `kimi-k2.5`、`glm-5`、`qwen3.5-plus`、`mimo-v2-pro`、`mimo-v2-omni`、`hy3-preview` | ✗ | 400 `Model is unavailable`（套餐内未开通或已下线） |
| `mimo-v2.6-flash-free` | ✗ | 400 `Model is unavailable`——go 目录里根本没有这个模型，它只存在于 zen，而 zen 侧被免费层挡住 |

> 上表初版为 2026-09-12 快照，2026-09-22 与 2026-10-08 两次复测更新（2026-10-08 复测 `gpt-6-luna`、
> `grok-4.7` 仍是协议不支持，`muse-spark-1.3-contributor` 仍是地区限制，`mimo-v2-pro`、`mimo-v2-omni`
> 仍是 400 无此模型）。上游随时可能调整；注意桥的 go 清单是**手工声明**的，不会自动跟着上游变，新模型上线后需手动补进 `models`。

### 各上游的限制

- **Zen 免费模型有限流**：用多了返回 `429 FreeUsageLimitError`，稍等恢复。上游配额策略，非本工具问题
- **Go 的密钥**：配置里是 `apiKeyEnv: OPENCODE_API_KEY`，但 2026-10-08 实测没配也能调通（见第十二节）
- **CommandCode 需要本机已登录**：凭据按 `COMMAND_CODE_API_KEY` → `~/.commandcode/auth.json` 的顺序取，两处都没有时**本地直接 401**（不打上游）
- **CommandCode 版本头是硬门槛**：`x-command-code-version` 缺失或过旧会 `403 upgrade_required`；桥默认自动读本机 CLI 版本（见第五节）
- **CommandCode 套餐外模型**：清单启动时按套餐过滤一次，超出套餐的模型仍可直呼，但会得到终态 `403 MODEL_NOT_IN_PLAN`（不重试）；换套餐后需重启桥刷新清单

### 想自己核对上游到底有哪些模型

```bat
:: 只看 go 声明了什么（不联网）
curl http://127.0.0.1:8900/go/v1/models

:: 看上游实时全量（需给该上游配 "autoModels": true，且顶层 models 白名单为空）
curl http://127.0.0.1:8900/v1/models
```

> **两个容易踩的前提**：
> 1. `/v1/models`（不带前缀）的"实时全量"要求 ① 该上游配了 `"autoModels": true`；② 顶层 `models`
>    白名单为空——配了白名单时**严格只返回那几个**，根本不碰上游。带前缀的 `/go/v1/models`
>    则永远只列配置里声明的，不联网。
> 2. `autoModels` 只影响**列表展示**，不影响按名分发。新模型要能被不带前缀的 `/v1` 路由到，
>    必须写进该上游的 `models`；否则会被当成"未声明的名字"落到默认上游（见第一节「方式一」）。

## 六、配置文件 `bridge.config.json`

```json
{
  "port": 8900,
  "host": "127.0.0.1",
  "defaultModel": "deepseek-flash",
  "models": ["deepseek-flash", "mimo-v2.5-pro", "glm5.3"],
  "maxTokens": 8192,
  "timeoutSeconds": 600,
  "accessLog": true,
  "modelsCacheSeconds": 300,
  "thinking": { "passthrough": true, "cacheSize": 500 },
  "upstreams": {
    "cch": {
      "protocol": "anthropic",
      "baseUrl": "http://127.0.0.1:15721",
      "apiKey": "",
      "anthropicVersion": "2023-06-01",
      "default": true,
      "fetchModels": false,
      "autoModels": false,
      "models": ["deepseek-flash", "mimo-v2.5-pro", "glm5.3"]
    },
    "go": {
      "protocol": "openai",
      "baseUrl": "https://opencode.ai/zen/go/v1",
      "apiKeyEnv": "OPENCODE_API_KEY",
      "sessionHeader": true,
      "models": ["glm-5.3-flash", "deepseek-v4.1-flash", "mimo-v2.6-flash", "longcat-2.5-preview-free"]
    },
    "zen": {
      "protocol": "openai",
      "baseUrl": "https://opencode.ai/zen/v1",
      "apiKey": "",
      "sessionHeader": true,
      "models": ["space-bunny-free"]
    },
    "commandcode": {
      "protocol": "commandcode",
      "baseUrl": "https://api.commandcode.ai",
      "apiKeyEnv": "COMMAND_CODE_API_KEY",
      "apiKeyFile": "~/.commandcode/auth.json",
      "cliVersion": "auto",
      "modelCatalog": "auto",
      "modelCatalogPlan": "auto",
      "pauseTurn": true,
      "models": ["deepseek/deepseek-v4.1-flash", "..."]
    }
  }
}
```

### 顶层字段

| 字段 | 含义 |
|------|------|
| `port` / `host` | 监听地址，默认 `8900` / `127.0.0.1` |
| `defaultModel` | 请求未带 `model` 时使用 |
| `maxTokens` | 请求未带 `max_tokens` 时的默认上限 |
| `timeoutSeconds` | 单请求整体超时（秒） |
| `accessLog` | 是否打印每请求一行日志 |
| `modelsCacheSeconds` | 模型列表缓存秒数（默认 300） |
| `thinking.passthrough` | 是否自动回传思考内容（多轮会话必需，见第九节） |
| `thinking.cacheSize` | 思考内容缓存条数上限，默认 `500` |
| `tools.downgradeForcedChoice` | 上游 thinking 模式下把强制的 `tool_choice` 降级为 `auto`（默认 `true`） |
| `models` | 顶层模型白名单，配了则 `/v1/models` 只返回这几个（见第七节） |
| `envFile` | 密钥文件路径，默认 `~/.jcode/.env`；设 `false` 禁用 |

### 每个上游的字段

| 字段 | 含义 |
|------|------|
| `protocol` | `anthropic`（走 `/v1/messages`）、`openai`（走 `/v1/chat/completions`）或 `commandcode`（走 `/alpha/generate`）；不写则按 `baseUrl` 推断 |
| `baseUrl` | 上游地址。**带不带 `/v1` 都行**，工具会自动规范化，不会拼出 `/v1/v1` |
| `apiKey` | 直接写密钥；**写空字符串 `""` 表示不发送鉴权头**（Zen 免费模型就是这种） |
| `apiKeyEnv` | 从环境变量/`.env` 文件读取密钥（推荐，避免密钥落盘） |
| `apiKeyFile` | 从凭据文件读取密钥（如 `~/.commandcode/auth.json`），优先级在 `apiKeyEnv` 之后 |
| `anthropicVersion` | `anthropic` 协议的 `anthropic-version` 头，默认 `2023-06-01` |
| `default` | 设为 `true` 的上游承接未匹配到任何模型的请求 |
| `models` | 该上游负责的模型名清单，用于按名分发 |
| `modelMap` | 上游级短别名（短名 → 真实模型 id），不污染全局 `aliases`，见第五节 |
| `sessionHeader` | 是否注入 `x-opencode-session`（opencode 类上游必须为 `true`；`commandcode` 不适用，默认关闭） |
| `fetchModels` | 是否从上游拉取模型清单（默认 `true`；`commandcode` 没有该端点，默认 `false`） |
| `autoModels` | 是否把上游**全部**模型也暴露给下游（默认 `false`，只暴露 `models` 里声明的） |
| `cliVersion` | `commandcode` 专用：`x-command-code-version` 取值，默认 `"auto"`（读本机 CLI 版本，读不到则查 npm registry） |
| `cliEnvironment` | `commandcode` 专用：`x-cli-environment` 头，默认 `"production"` |
| `modelCatalog` | `commandcode` 专用：模型目录来源，`"auto"`（读本机 CLI，默认）/ `false`（只用配置声明）/ 具体文件路径 |
| `modelCatalogPlan` | `commandcode` 专用：按套餐过滤对外清单，`"auto"`（探测，默认）/ `go` / `goat` / `pro` / `max` / `false`（不过滤） |
| `pauseTurn` | `commandcode` 专用：收到 `pause_turn` 时用同一请求体自动续跑（默认 `true`，最多续 5 次） |
| `cmdZdr` | `commandcode` 专用：是否发送 `x-cmd-zdr: 1`（ZDR 安全头，零数据留存信号），默认 `true`；设 `false` 可关闭 |

**改完配置需重启才生效。**

### 密钥放哪：两种凭据模式

| 模式 | 配置写法 | 密钥存放 | 适用 |
|---|---|---|---|
| **环境变量**（推荐） | `"apiKeyEnv": "OPENCODE_API_KEY"` | 进程环境优先，其次 `~/.jcode/.env`（`envFile` 可改路径） | 长期使用、多上游共用、轮换方便 |
| **本地凭据文件** | `"apiKeyFile": "~/.commandcode/auth.json"` | 该 JSON 的 `apiKey` 字段（厂商 CLI 已登录的凭据） | 复用已登录凭据，不复制明文 |

```json
"upstreams": {
  "go":  { "protocol": "openai", "baseUrl": "https://opencode.ai/zen/go", "apiKeyEnv": "OPENCODE_API_KEY", "models": ["deepseek-v4.1-flash"] },
  "cc":  { "protocol": "commandcode", "baseUrl": "https://api.commandcode.ai", "apiKeyFile": "~/.commandcode/auth.json", "models": [] }
}
```

**关系：同一上游按此顺序取第一个非空值**

1. `apiKeyEnv` —— 进程环境 > `~/.jcode/.env`
2. `apiKeyFile` —— 文件里的 `apiKey` 字段
3. `apiKey` —— 面板里直接粘贴；**只驻内存**，不写 `bridge.config.json`、不写 SQLite，进程重启即丢
4. 都没配 —— 用占位 `any-key`（本地不校验；上游会回 401）

同一上游同时写 `apiKeyEnv` 与 `apiKeyFile` 时**前者生效**，后者被忽略；两个都写不算错，但配置页会以「实际生效来源」提示（`keySource` = env / file / literal / none）。

**敏感隔离（不变式）**

- `bridge.config.json` 里**永远不出现明文密钥**：面板保存时先剥离 `apiKey` 再落盘，明文只进内存。
- `.gitignore` 已覆盖 `/bridge.config.json`、`/data/`、`*.env`、`**/auth.json`、`**/credentials*.json`、`*.key|*.pem|*.token`；对外分享只用 `bridge.config.example.json`（零明文）。
- 只读接口不回显密钥：`/api/config` 与 `/api/upstreams` 仅给 `apiKeyEnv` / `apiKeyFile` 引用、`keySource` 与 `keyConfigured` 布尔值。
- 日志只记上游名、状态码与耗时，不记请求头；用量导出（CSV/JSON）只含统计字段与模型名。

### 加新上游

在 `upstreams` 里加一项即可，例如接一个 OpenAI 兼容的第三方：

```json
"myvendor": {
  "protocol": "openai",
  "baseUrl": "https://api.example.com/v1",
  "apiKeyEnv": "MY_VENDOR_KEY",
  "models": ["some-model"]
}
```

`protocol` 可写 `anthropic` / `openai` / `commandcode`；省略时按 `baseUrl` 推断（opencode 系 → `openai`，
commandcode.ai → `commandcode`，其余 → `anthropic`）。接私有协议还得在 `model-bridge.js` 的协议适配层加一个适配器。

---

## 七、模型列表（`/v1/models`）

### 两个层级，各管一段

| 层级 | 配置位置 | 作用 |
|------|---------|------|
| **对外展示** | 顶层 `models` | Agent 调 `/v1/models` 看到什么 |
| **内部分发** | 各上游的 `models` | 某个模型名该送去哪条上游 |

### 顶层 `models` 是精确白名单

配了顶层 `models`，`/v1/models` 就**只返回这几个，一个不多**，也不去上游拉取：

```json
{
  "models": ["deepseek-flash", "mimo-v2.5-pro", "glm5.3"],
  "upstreams": { "...": {} }
}
```

实测：`GET /v1/models` 返回 3 个，就是这三个。

不配顶层 `models` 时，才退化为合并各上游声明的模型（开箱可用，但列表会长）。

### 想看某个上游的完整清单

用带前缀的地址，不受顶层白名单影响：

| 地址 | 返回 |
|------|------|
| `/cch/v1/models` | cch 的清单（3 个） |
| `/go/v1/models` | go 的清单（4 个） |
| `/zen/v1/models` | zen 的清单（1 个） |
| `/commandcode/v1/models` | commandcode 的清单（配置声明 ＋ 本机 CLI 权威目录合并，再按账号套餐过滤，数量随套餐变） |

### 上游拉取的相关开关

仅在**未配**顶层 `models` 时才可能生效（配了白名单就完全不碰上游）：

| 开关 | 默认 | 作用 |
|------|------|------|
| `fetchModels` | `true` | 是否允许从该上游 `/v1/models` 拉清单 |
| `autoModels` | `false` | 拉到的全量模型是否也暴露给下游 |

cch 已显式关掉两者：

```json
"cch": { "fetchModels": false, "autoModels": false, "models": ["deepseek-flash", "mimo-v2.5-pro", "glm5.3"] }
```

原因：cch 上游 `/v1/models` 返回的是它自己那套名字（`deepseek-v4-flash` 等），与对外提供的名字不一致，拉进来只会造成混乱。

### 同名模型的归属

合并列表里去重后归**配置里排在前面的上游**。想让 Agent 明确用某条上游，给它配带前缀的地址：

```json
{ "base_url": "http://127.0.0.1:8900/go/v1" }
```

---

## 八、提供的接口

| 接口 | 方法 | 说明 |
|------|------|------|
| `/v1/chat/completions` | POST | 对话主接口，支持流式与工具调用 |
| `/v1/models` | GET | 模型列表 |
| `/health` | GET | 健康检查，列出各上游状态 |

不支持 `/v1/embeddings`、`/v1/completions` 等，返回 404。

---

## 九、关于 thinking 回传

DeepSeek 思考模式要求：多轮对话（尤其带工具调用）必须把上一轮的**思考内容**传回，否则思考链断裂。但多数 Agent 不会回传该字段，所以本工具会自动缓存并在后续轮次补回。

日志里的 `think=N` 是本次补回的条数：

```
... 200 1086ms tokens=289+55 think=0   ← 第一轮，无历史
... 200 1122ms tokens=369+26 think=1   ← 第二轮，补回 1 条
```

出现「思考链可能断裂」告警说明该轮需要思考内容但缓存没有（如服务重启后客户端发了带历史的请求），属提示而非错误。

**仅对 `protocol: anthropic` 的上游生效**（cch）。openai 类上游原样透传，不做处理；
CommandCode 的思考内容由它自己的事件流直接透出（不依赖这个缓存），`pause_turn` 续跑期间也连续下发。

---

## 十、日志怎么看

```
[时间] 请求模型 -> 上游名:实际模型 状态码 耗时 [stream] tokens=输入+输出 [think=补回条数]
```

时间戳是**本机时区**的 ISO 风格时间，带 UTC 偏移（`+08:00` 这种），直接和本地时间对得上；
进程时区跟随系统（`TZ` 环境变量可改），不会像 `Z` 结尾那样只能看到 UTC。

例：
```
[2026-09-12T17:46:20.123+08:00] deepseek-flash -> cch:deepseek-flash 200 782ms tokens=336+25 think=1
[2026-09-12T17:46:22.456+08:00] glm-5.3-flash -> go:glm-5.3-flash 200 1090ms stream tokens=289+55
[2026-09-13T10:00:00.000+08:00] deepseek/deepseek-v4.1-flash -> commandcode:deepseek/deepseek-v4.1-flash 200 3200ms stream tokens=7599+120 cache=7424+0
```

`tokens=` 之后按上游协议追加附加信息：`think=补回条数`（anthropic）、
`cache=缓存读+缓存写`、`截断(无 finish)`、`pause_turn 未续完`、`上游错误: ...`（后四项来自 CommandCode 协议）。

---

## 十一、常见问题

| 现象 | 原因 | 处理 |
|------|------|------|
| `EADDRINUSE` | 8900 被占用 | 换端口，或结束占用进程（提示里已给出命令） |
| 请求 502 | 上游连不上 | cch：确认 15721 在监听；go/zen：确认能访问 opencode.ai；commandcode：确认能访问 api.commandcode.ai |
| **429 限流** | Zen 免费模型配额用尽 | 稍后重试，或改用其他模型 |
| **401 Invalid API key** | 该上游不该带 key 却带了，或 key 无效 | Zen 免费模型的 `apiKey` 必须是 `""`；Go 需要有效 `OPENCODE_API_KEY` |
| 401 Missing API key | Go 没读到密钥 | 检查 `~/.jcode/.env` 里的 `OPENCODE_API_KEY` |
| 400 `MissingSessionID` | 该上游未注入会话头 | 确认该上游 `sessionHeader` 不为 `false` |
| 模型报不存在 | 该模型不在任何上游的 `models` 里，被送到默认上游 | 把模型名加到对应上游的 `models` |
| 流式没有 `[DONE]` | 部分上游（如 minimax-m3）本身不发 | 上游行为，客户端按连接关闭结束即可 |
| CommandCode 本地 401 | 没读到凭据（不打上游） | 先在本机登录 command-code，或设 `COMMAND_CODE_API_KEY` |
| 403 `upgrade_required` | `x-command-code-version` 缺失或过旧 | 装/升级本机 command-code CLI，或给该上游写死 `cliVersion` |
| 403 `MODEL_NOT_IN_PLAN` | 模型超出当前套餐档位 | 改用套餐内模型；终态错误，桥不重试 |
| CommandCode 报模型不存在 | 该上游按完整 id 匹配，不接受缩写 | 写全如 `deepseek/deepseek-v4.1-flash`，或给该上游配 `modelMap` 短名 |

---

## 十二、运行前提

- **Node.js**：需要（本机已装）。无第三方依赖，不用 `npm install`
- **cch 上游**：需本地代理在 `127.0.0.1:15721` 运行
- **go 上游**：配置里写的是 `apiKeyEnv: OPENCODE_API_KEY`。但 2026-10-08 实测本机 **Process / User / Machine
  三处都没有这个变量**，go 的模型照样全部返回 200——即当前 Go 订阅的可用性并不依赖这个 key；
  `/zen/go/v1/models` 更是公开可读。若哪天开始 401，再去 `~/.jcode/.env` 补 key 不迟
- **zen 上游**：无需密钥
- **commandcode 上游**：需本机登录过 command-code（凭据 `~/.commandcode/auth.json`）或设 `COMMAND_CODE_API_KEY`；`x-command-code-version` 默认自动探测，读不到时可显式配 `cliVersion`
- **系统**：Windows（`start-bridge.cmd`）；`model-bridge.js` 本身跨平台

---

## 十三、文件清单

| 文件 | 作用 |
|------|------|
| `model-bridge.js` | 主程序，零依赖单文件 |
| `bridge.config.json` | 配置（四条上游定义） |
| `start-bridge.cmd` | 一键启动 |
| `restart-bridge.cmd` | 一键重启（先结束旧进程再启动） |
| `README.md` | 本说明 |
| `package.json` | Node 工程信息（零依赖，声明 `type: module`） |
| `test-bridge.mjs` | 冒烟测试：对着**运行中**的真实上游跑全链路（`npm test`） |
| `test-commandcode.mjs` | commandcode 协议**离线**回归：自带 stub 上游与独立实例，零外网（`npm run test:commandcode`） |
| `probe-upstream.mjs` | 探测本地 cch 代理（cc-switch）支持哪些协议与模型名（`npm run probe`） |
| `changelog/` | 发行说明源：每版本一份 `v<版本>.json`，`index.json` 为索引（见第十四节） |
| `scripts/` | 打包发布脚本（打包、产物自检、校验和、发行说明渲染、重触发），CI 与本地共用 |
| `.github/workflows/release.yml` | tag 驱动的发布流水线 |
| `dist/`、`SHA256SUMS` | 打包产物与校验和（`.gitignore` 排除，不入库） |

---

## 十四、版本与发布

版本号唯一来源是 `package.json` 的 `version`，发布口只留 tag，两者必须一致。

### 发布前要同步的位置

| 位置 | 内容 |
|------|------|
| `package.json` → `version` | 本次版本号（如 `1.1.0`） |
| `changelog/v<版本>.json` | 发行说明源：`title` / `date` / `highlights` / `improvements` / `fixes`（写给用户看效果，纯内部改动不列） |
| `changelog/index.json` | 在 `entries` 里补一条索引（版本、日期、标题、文件名） |

缺 `changelog/v<版本>.json` 时流水线第一步就失败，**不允许**退化成自动提交清单。

### 怎么发

```bat
git tag -a v1.1.0 -m "v1.1.0"    :: 注释标签，指向当前提交
git push origin v1.1.0           :: 推送 tag 即触发发布
```

### 流水线做什么（`.github/workflows/release.yml`）

1. **建 draft release**：校验 tag 与 `package.json` 版本一致 → 渲染发行说明 → 建 draft（或更新既有 draft）；已公开的 release 拒绝覆盖
2. **门禁 + 打包 + 自检**：敏感信息扫描（工作区 + 全历史对象）→ 语法自检 → 按显式必达清单打包 → 产物自检（条目集合、解包后语法、真实启动 smoke）→ 生成 `SHA256SUMS`
3. **收尾**：校验资产集合齐全才上传 → 幂等追加「资产与校验方式」说明 → 转 public → 反向拉取已公开产物自检

任一环节失败，release **停在 draft**，用户看不到半成品。

### 产物

`dist/model-bridge-v<版本>.zip` ＋ `SHA256SUMS`。zip 内是显式列出的产品文件（`model-bridge.js`、`bridge.config.json`、`start-bridge.cmd`、`restart-bridge.cmd`、`test-bridge.mjs`、`probe-upstream.mjs`、`README.md`、`.gitignore`），不含 `.git` / `dist` / 未跟踪文件；打包时间戳固定，**同一份源码两次打包得到同一 SHA256**。

### 本地复核与重跑

```bat
bash scripts/build-package.sh --list              :: 只看将入包的文件清单
bash scripts/build-package.sh                     :: 本地打包
bash scripts/verify-release.sh v1.1.0             :: 独立下载已发布资产逐项核对
bash scripts/retrigger-release.sh --dry-run v1.1.0   :: 只查重触发前提，不动任何东西
bash scripts/retrigger-release.sh --at HEAD v1.1.0   :: 删除并按 HEAD 重建同名 tag 后重推
```

