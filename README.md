# Model-Gate — 使用说明

> **唯一实现：Rust 内核**。`dist\Model-Gate.exe`（约 4.3 MB）由 `rust/model-gate/`
> 构建：本地 HTTP 代理 + 模型配置面板 + SQLite 用量统计，桌面壳为托盘常驻 +
> 系统浏览器面板（无内嵌 WebView2），代理常驻内存约 7 MB。
> 构建/投放：`powershell -ExecutionPolicy Bypass -File rust\model-gate\build-release.ps1 -Promote`。
>
> **应用内更新**：安装版可在面板顶栏「检查更新」或托盘菜单检查 GitHub Release，
> 下载 `Model-Gate-v<版本>-rust-setup.exe` 并校验 `SHA256SUMS` 后静默覆盖升级；
> 便携版只提示手动运行安装包。启动时后台检查每 24 小时最多一次，只提示、不自动下载。
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
   ├─ zen         (OpenAI + opencode) → opencode.ai/zen/v1     ← 免费模型
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
model: mimo-v2.6-flash-free → zen
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

### zen（OpenCode 免费层，`transport: opencode`）

2026-10-20 实测：直接以普通 OpenAI 客户端请求 `-free` 模型仍会 403
`FreeTierError: OpenCode's free tier can only be used from within OpenCode`，
但按 OpenCode CLI 的指纹（匿名 `Bearer public`、UA、会话/请求 ID、强制流式、bash/read 工具）
请求 `/inference/v1/chat/completions` 可以 200。本桥把这件事收敛为显式 `transport`：

```json
"zen": {
  "protocol": "openai",
  "transport": "opencode",
  "baseUrl": "https://opencode.ai/zen/v1",
  "models": ["mimo-v2.6-flash-free", "big-pickle"],
  "fetchModels": false,
  "sessionHeader": false,
  "autoModels": false
}
```

| 行为 | 说明 |
|------|------|
| 匿名鉴权 | 固定 `Authorization: Bearer public`，**不需要也不读取 apiKey** |
| 指纹头 | `user-agent`、`x-opencode-client/project`、`x-opencode-session(-id)`、`x-opencode-request` 由桥生成 |
| 路径 | `opencode.ai` 的 `/zen/...` 自动改写为 `/inference/...`；`/zen/go/...` 不改写 |
| 流式 | 上游强制 `stream: true`；客户端要非流式时由桥聚合 SSE 成普通 `chat.completion` |
| 工具 | 自动补/改名 `bash`、`read`（含 schema）；响应工具名映射回客户端原名；stub 调用会过滤并给出提示 |
| 免费边界 | 只允许 `-free` / `big-pickle` / `grok-code`；其它模型本地 400 `model_not_free`，不会请求上游 |
| 失败 | 免费层失败不自动切换到付费或其它上游，避免静默降级 |

- `grok-code` 是否仍免费尚未用本桥实测，默认不写进清单。
- 这是 OpenCode 未公开的客户端契约，可能随上游升级失效；只用于免费层，不涉及 `go` 订阅、登录或付费。
- 配置校验：`transport=opencode` 必须搭配 `protocol=openai`；面板可直接选择「opencode 免费层」。

### 免费来源：免密免费模型（面板第 6 页「免费来源」）

不登录、不注册、不填 API Key，就能用上免费模型——并且**看清此刻到底哪个模型真能用**。

面板新增第 6 页「**免费来源**」，提供两条免密通道：

| 来源 | 端点 | 鉴权 | 隐私级别 |
|------|------|------|----------|
| OpenCode Zen 免密池 | `https://opencode.ai/zen/v1` | 匿名 `Bearer public` + 客户端指纹 | warn |
| Kilo 免密免费池 | `https://api.kilo.ai/api/openrouter` | **完全不发送鉴权头** | **danger** |

一键接入后配置里**没有任何凭据**（不写 `apiKey` / `apiKeyEnv`）。

```json
"kilo": {
  "protocol": "openai",
  "baseUrl": "https://api.kilo.ai/api/openrouter",
  "fetchModels": true,
  "autoModels": false,
  "sessionHeader": false,
  "default": false,
  "compat": { "auth": { "scheme": "none" } },
  "models": ["minimax/minimax-m3.1-flash:free", "..."]
}
```

页面上可以：

- 看**鱼缸水位**（当前真可用模型占比）——2026-10-21 实测 Kilo 池为 **11/20（55%）**；
- 逐个模型看**状态徽标**：可用 / 暂时不可用 / 疑似可用 / 地区受限 / 明确不可用，
  外加**拒因原文**与**下一步该做什么**（不是只告诉你"失败了"）；
- 点「全部探测」实测（会消耗免费额度，UI 明示）；限流中的模型自动退避跳过。

**为什么"暂时不可用"和"明确不可用"要分开**：免费池的失败主因是**上游池限流**，
不是模型下线。若把 429 判成"不可用"，模型会频繁从清单消失，用户会以为池子空了。
只有上游**明确拒绝**（404 模型不存在 / 401 需登录 / 地区 403 / 免费层拒绝）才算"明确不可用"。

> **零价 ≠ 免鉴权**：Kilo 的 `google/lyria-3-*` 零价却返回 401 `PAID_MODEL_AUTH_REQUIRED`，
> 面板会如实标出并解释。

> **免费模型默认不进对外的 `/v1/models`**。`/v1/models` 是客户端唯一认的通用清单接口，
> 因此对外不暴露必须真的成立：一键接入固定 `autoModels: false`，且**不写顶层 `models`**；
> 顶层 `models` 为空时连上游自己的声明也不写（否则会被自动并入全局）。
> 实测：接入前后 `GET /v1/models` 都是 4 个（新增 0 个），而 `GET /kilo/v1/models` 有 19 个——
> 内部可路由、对外不暴露。需要对外暴露时在模型卡上显式点「加入全局清单」。

> **隐私红线**：Kilo 免密池 20 个模型中 **19 个 `mayTrainOnYourPrompts=true`**，NVIDIA 免费端点
> 官方明示会记录会话用于改进产品。免费池**不适合承载机密或敏感内容**，面板会常驻警示。

接口：`GET /api/free-sources`、`POST /api/free-sources/{catalog,probe,probe-all,attach}`。
完整设计见 [`docs/architecture/free-sources.md`](docs/architecture/free-sources.md)。

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

**面板一键查询（推荐）**：配置页 → 上游 →「编辑」→ 模型清单旁点「**查询清单**」。

- 后端按 OpenAI 兼容约定拉 `GET {baseUrl}/v1/models`（路径可用 `compat.endpoint.models` 改），
  解析 `data` / `models` 数组里的 `id` / `slug` / `model`，**去重且保持上游顺序**。
- 弹出的面板里**多选**即可，写入方式二选一：
  - **替换**：用勾选结果整体覆盖该上游的 `models`；
  - **追加**：把勾选结果并入现有 `models`（去重）。
- 已配置的模型默认勾上，方便一眼看出「哪些已生效」；支持检索、全选、取消选择。
- 写入只改面板表单，**不立即落盘**——点页面底部「保存配置」才写进 `bridge.config.json`。
- 改了 `baseUrl` 但还没保存也能查：查询用**表单当前值**，打的是新地址。
- 查询失败会说明原因（401 / 超时 / 返回体不是 JSON）与修复提示，不会伪装成「没有模型」。

命令行同样可以查：

```bat
:: 查某个上游的可用模型（不写配置）
curl "http://127.0.0.1:8900/api/upstreams/kilo/models"

:: 带上未保存的 baseUrl（查改过地址的上游）
curl "http://127.0.0.1:8900/api/upstreams/kilo/models?baseUrl=https://ai-gateway.kilo.ai/api/v1"
```

> 密钥不会出现在 URL 里（URL 会进访问日志与浏览器历史）；需要用未保存的密钥查询时走
> `POST /api/upstreams/models`，密钥放请求体。

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
      "transport": "opencode",
      "baseUrl": "https://opencode.ai/zen/v1",
      "fetchModels": false,
      "sessionHeader": false,
      "models": ["mimo-v2.6-flash-free", "big-pickle"]
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
| `compat` | 通用兼容能力覆盖层：鉴权方式、会话头、额外请求头、端点、思考回传、输出上限、ZDR 等。见第六节「Provider 兼容能力」 |
| `modelOverrides` | 模型级覆盖（精确/通配），只影响 `limits` 与 `capabilities`；见下文「预设注册表与模型级覆盖」 |

**面板保存后立即热重载；手工改文件后调用 `POST /api/config/reload` 或重启。**

### 配置即 JSON：唯一事实源与备份

**`bridge.config.json` 是配置的唯一事实源。** 面板的每一次保存都会把配置写回这个文件；
数据库里的 `settings.config` 只是运行态快照，**不能**成为第二条能独立演进的配置来源。

| 行为 | 规则 |
|------|------|
| 面板新增上游 | 保存后 `upstreams` 里出现该条目，文件里同步可见 |
| 面板删除上游 | 该条目从 `upstreams` 里**真正消失**（不会被任何旧数据顶回），重启后也不会复活 |
| 局部保存 | 只改传入的字段，其余顶层键与上游保持原值；面板未渲染的兼容字段（`cliVersion` / `compat` / `modelCatalog`…）一并保留 |
| 写入方式 | 临时文件 → 回读校验 JSON 合法 → 原子替换；不会留下半截配置 |

**备份**（面板「配置落盘与备份」区可开关）：

| 规则 | 说明 |
|------|------|
| 首次强制备份 | 配置目录里**没有任何备份**时，第一次保存强制生成一份 `bridge.config.json.bak`，**不受开关影响**（避免第一次保存就丢掉原始配置） |
| 后续自主选择 | 「保存前备份」开关默认开启；关闭后保存不再生成新备份，但**已有备份不会被删除** |
| 轮转 | 保留最近 **5 份**：`.bak` 为最近一次，`.bak.1`～`.bak.4` 为历史 |
| 恢复 | 面板「恢复上一次备份」按钮，或 `POST /api/config/backups/restore`；恢复前会把当前内容另存一份 `.bak.pre-restore`，恢复动作本身可回退 |
| 备份内容 | 存的是**保存前**的旧配置（写后备份存的是新内容，没有回滚价值） |

接口：`GET /api/config/backups`（备份状态）、`POST /api/config/backups/restore`（恢复）。

**「从文件重载」按钮**：手工编辑过 `bridge.config.json` 后，用它把磁盘内容重新读入并热重载，
改动立即生效，不必重启进程。注意它与「放弃改动」方向相反：
「从文件重载」读的是**磁盘**并覆盖运行态；「放弃改动」只是把表单恢复成**运行态**，不碰磁盘。
面板上有未保存改动时，点「从文件重载」会先弹确认框（未保存的改动会丢失）。

**从快照恢复不会复活已删除的上游**：SQLite 快照只补充顶层标量键与**文件中已有**的上游的缺失字段；
用户从 `bridge.config.json` 里删掉的上游不会被补回，启动日志会点名说明「忽略了快照中已从配置删除的上游」。

**升级安装不会清理旧数据**：安装脚本只覆盖 exe，`bridge.config.json` 与 `data\modelgate.db` 原地保留；
Rust 版的数据库路径与配置文件同目录（`data\modelgate.db`），从快捷方式等任意目录启动都能读到旧库，
不会因工作目录变化而「历史清零」或在别处生成空库。

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
commandcode.ai → `commandcode`，其余 → `anthropic`）。接私有协议需在 `rust/model-gate/src/protocols/` 增加适配器。

### Provider 兼容能力（`compat`）

不同厂商的差异不再写死在协议代码里，而是「预设 + 覆盖」：

1. **预设**：按 `baseUrl` / `protocol` 自动识别 `opencode`、`commandcode`、`anthropic`、`deepseek`、`openai`；
2. **旧字段**：`sessionHeader` / `cmdZdr` / `cliVersion` 等继续生效；
3. **显式覆盖**：`upstreams.<name>.compat` 优先级最高，面板「兼容能力」区域可编辑。

```json
"commandcode": {
  "protocol": "commandcode",
  "baseUrl": "https://api.commandcode.ai",
  "apiKeyEnv": "COMMAND_CODE_API_KEY",
  "compat": {
    "zdr": false,
    "limits": { "maxOutputTokens": 64000 }
  }
}
```

常用覆盖：

| 需求 | 写法 |
|---|---|
| 自定义鉴权头 | `"compat": {"auth": {"scheme": "header", "header": "x-api-key", "prefix": ""}}` |
| 关闭会话头 | `"compat": {"session": false}` |
| 换端点 | `"compat": {"endpoint": {"chat": "/v1/chat/completions"}}` |
| 额外请求头 | `"compat": {"headers": {"x-client-name": "model-gate"}}` |
| 关闭思考回传 | `"compat": {"thinking": {"passthrough": false}}` |
| 收敛输出上限 | `"compat": {"limits": {"maxOutputTokens": 393216}}` |

完整字段、预设矩阵与示例见 [`docs/architecture/provider-compat.md`](docs/architecture/provider-compat.md)。

### 预设注册表与模型级覆盖

五套预设（`openai` / `opencode` / `anthropic` / `deepseek` / `commandcode`）由
`rust/model-gate/provider_profiles.json` 单点定义并由 Rust 内核内嵌；面板「预设」下拉与
`/api/config` 的 `profiles` 字段都来自它，不在代码里双写。

**模型级覆盖**（`upstreams.<name>.modelOverrides`，可选）在 `compat` 之上声明单模型差异：

| 规则 | 说明 |
|------|------|
| key | 路由解析后的模型 id；支持只含一个 `*` 的前缀/后缀通配 |
| 优先级 | 精确匹配 > 最长通配 > 上游 `compat.limits` |
| `limits.maxOutputTokens` | 参与请求 `max_tokens` 裁剪（与上游上限同一套逻辑） |
| `capabilities` | 只读展示与配置诊断，不改变转发行为 |

```json
"deepseek": {
  "protocol": "openai",
  "baseUrl": "https://api.deepseek.com/v1",
  "apiKeyEnv": "DEEPSEEK_API_KEY",
  "models": ["deepseek-chat"],
  "modelOverrides": {
    "deepseek-chat": { "limits": { "maxOutputTokens": 8192 }, "capabilities": { "thinking": false } },
    "deepseek-*": { "limits": { "maxOutputTokens": 65536 } }
  }
}
```

能力字段固定为 `stream / tools / vision / thinking / cache / systemPrompt`；合并顺序：
模型覆盖 > 上游 `compat.capabilities` > profile 预设 > 内置默认（`stream/tools/systemPrompt` 为 true，
其余 false）。`/api/config` 的 `effective` 回显解析结果，面板以徽标展示。

新增诊断码：`unknown_profile`、`invalid_model_overrides`、`invalid_limit_relation`、
`invalid_capabilities`、`model_override_unknown_model`、`multi_wildcard_override`
（error 阻断保存，warn 仅提示；完整条件见 `docs/architecture/upstream-config-model.md`）。

五套 provider 的完整示例配置见 [`docs/examples/providers/`](docs/examples/providers/)，均可直接加载。

**配置诊断**：`POST /api/config/validate` 只校验不落盘，返回 `error / warn / info` 与解析后的
`effective` 能力；`POST /api/config` 遇到 `error` 会返回 400 且不写文件。面板加载/保存后直接展示这些诊断。

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

**面板用法（桌面版）**：用量页顶部的作用域条（时间预设 / 自定义起止 / 上游 / 模型）控制整页统计——
KPI、趋势、占比、上游用量、模型用量与明细都会跟随，切换上游后模型下拉只保留该上游的模型。
「上游用量」表按上游聚合，点击行可展开该上游的模型用量，行内「只看此上游」可直接收窄作用域。
KPI 首位是**缓存命中率**（命中输入 / 总输入，分母为 0 显示「—」），成功率放在吞吐组作为健康度参考。
日志页每行 hover 出现「复制」，点击行空白处也可复制该行（不含行号）；框选文本时不会触发复制。

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

- **桌面版 / 安装版**：Windows 10/11，直接运行 `dist\Model-Gate.exe`，无需 Python/Node。
- **应用内更新**：安装版需要能访问 `api.github.com` 与 `github.com`（下载 Release 资产）；企业代理/私有网络下检查会失败，可手动下载安装包。
- **源码构建**：Rust stable + MSVC 工具链；生成安装包另需 Inno Setup 6（`build-installer.ps1` 会自动查找，缺失时可运行 `scripts\get_innosetup.ps1`）。
- **cch 上游**：需本地代理在 `127.0.0.1:15721` 运行。
- **go 上游**：配置里写的是 `apiKeyEnv: OPENCODE_API_KEY`。但 2026-10-08 实测本机 Process / User / Machine 三处都没有这个变量，go 的模型照样全部返回 200；若哪天开始 401，再去 `~/.jcode/.env` 补 key。
- **zen 上游**：无需密钥。
- **commandcode 上游**：需本机登录过 command-code（凭据 `~/.commandcode/auth.json`）或设 `COMMAND_CODE_API_KEY`；`x-command-code-version` 默认自动探测，读不到时可显式配 `cliVersion`。
- **系统**：Windows（桌面 exe / 安装包）。

---

## 十三、项目目录与维护

```
Model-Gate/
├─ README.md                      # 本说明
├─ rust/model-gate/               # 唯一产品源码（Rust 代理内核 + 托盘 + 系统浏览器面板）
│  ├─ src/                        # 代理、协议适配、配置、SQLite、更新、托盘
│  ├─ ui/                         # 内嵌面板（index.html / app.js / style.css）
│  ├─ tests/fixtures/             # Rust 单测夹具（含 legacy 配置）
│  ├─ provider_profiles.json      # Provider 预设注册表（编译期内嵌）
│  ├─ Cargo.toml                  # 版本唯一源
│  ├─ build-release.ps1           # 发布候选/正式入口
│  ├─ build-installer.ps1         # Inno Setup 安装包
│  └─ build.rs                    # Windows 版本资源 + 托盘图标
├─ assets/                        # 图标等构建资产
├─ docs/                          # 文档（architecture / integration / maintenance / examples）
├─ scripts/                       # 4 个 PowerShell 运维/验收脚本 + README
├─ ModelGate.iss                  # Inno Setup 打包定义
├─ bridge.config.example.json     # 零密钥配置模板（唯一入库配置）
└─ dist/ data/ target/ .op/       # 本地生成物/运行数据/构建缓存/变更工作区（均不入库）
```

维护规则、清理节奏与敏感信息处理见 [`docs/maintenance/project-layout.md`](docs/maintenance/project-layout.md)；
文档索引见 [`docs/README.md`](docs/README.md)。新增一次性排查脚本请放 `scripts/archive/`（该目录随
Rust-only 迁移删除），不要继续堆积在 `scripts/` 根目录。

---

## 十四、文件清单

| 路径 | 作用 |
|------|------|
| `rust/model-gate/` | 唯一产品源码：Rust 代理内核、托盘常驻 + 系统浏览器面板、更新、构建脚本 |
| `rust/model-gate/ui/` | 内嵌面板静态资源 |
| `rust/model-gate/provider_profiles.json` | Provider 预设注册表 |
| `rust/model-gate/Cargo.toml` | 版本唯一源与依赖 |
| `ModelGate.iss` | Inno Setup 打包定义（固定 AppId，支持静默覆盖升级） |
| `assets/ModelGate.ico` | 应用与托盘图标 |
| `docs/` | 架构、兼容性、维护规范与示例配置 |
| `scripts/panel_api_smoke.ps1` | 面板接口与更新资源冒烟 |
| `scripts/rust_shell_acceptance.ps1` | Rust 托盘壳黑盒验收 |
| `scripts/upgrade_and_run.ps1` | 不停机升级辅助 |
| `scripts/find_iscc.ps1` / `scripts/get_innosetup.ps1` | 查找/安装 Inno Setup |
| `bridge.config.example.json` | 配置模板（零密钥，可入库） |
| `bridge.config.json` | 本机真实配置（**不入库**） |
| `dist/`、`data/`、`target/`、`.op/` | 打包产物、运行数据、构建缓存、变更工作区（**均不入库**） |
| `.github/workflows/release-rust.yml` | tag 驱动发布：构建、安装包、SHA256SUMS、GitHub Release |

---

## 十五、版本、打包与发布

版本号**唯一来源**：`rust/model-gate/Cargo.toml` 的 `version`。`build.rs` 的 Windows 文件版本、
面板静态资源的 `?v=` 缓存参数、安装包版本与 Release 资产名都从它派生。

### 构建（Rust）

```bat
cargo build --release
powershell -ExecutionPolicy Bypass -File rust\model-gate\build-release.ps1
powershell -ExecutionPolicy Bypass -File rust\model-gate\build-release.ps1 -Promote
powershell -ExecutionPolicy Bypass -File rust\model-gate\build-installer.ps1
```

- `build-release.ps1` 产出 `dist\Model-Gate-rust.exe`（单文件、内嵌前端）。
- `-Promote` 额外覆盖 `dist\Model-Gate.exe`（正式入口）。
- `build-installer.ps1` 产出 `dist\Model-Gate-v<ver>-rust-setup.exe`（Inno Setup 中文向导），
  默认创建桌面图标与开始菜单项；静默安装/卸载与安装后启动已纳入验收。

也可以直接调用 ISCC：

```bat
iscc /DMyAppVersion=2.21.0 ^
     /DMyAppSourceExe=dist\Model-Gate-rust.exe ^
     /DMyOutputBaseFilename=Model-Gate-v2.21.0-rust-setup ^
     ModelGate.iss
```

### 应用内更新

| 场景 | 行为 |
|------|------|
| 启动自动检查 | 后台检查 GitHub `releases/latest`，24 小时内最多一次；只提示，不自动下载 |
| 手动检查 | 面板顶栏「检查更新」或托盘菜单「检查更新」打开 `?update=1` |
| 下载 | 精确选择 `Model-Gate-v<版本>-rust-setup.exe` 与 `SHA256SUMS`，流式下载并计算 SHA256 |
| 安装 | 校验通过后启动安装包：`/SILENT /NORESTART /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS /SP- /DIR=<当前exe目录>` |
| 便携版 | 同目录没有 `unins*.exe` 时不自动覆盖，只提示手动运行安装包或打开 Release 页面 |
| 失败 | 下载/校验/启动失败均保留原版本，不删除配置与用量库 |

安装版升级会保留 `bridge.config.json`、`data\modelgate.db` 与旧安装包；安装器使用固定 `AppId`，
不会产生第二份卸载项。

### 发布

1. 修改 `rust/model-gate/Cargo.toml` 的 `version`（例如 `2.21.0`）。
2. 本地通过 `cargo fmt --check`、`cargo check`、`cargo test` 与 `scripts\panel_api_smoke.ps1`。
3. 打 tag 并推送：`git tag v2.21.0`、`git push origin v2.21.0`（Git 写操作需人工确认）。
4. `.github/workflows/release-rust.yml` 构建并上传：
   - `Model-Gate-v<版本>-rust-setup.exe`
   - `Model-Gate-rust.exe`（便携版）
   - `SHA256SUMS`
5. 校验 Release 资产名与 `SHA256SUMS` 格式；应用内更新按精确文件名匹配。

### 回滚

- 保留上一版安装包与 Release；出现问题时手动重装上一版即可回滚，配置与用量库不动。
- 已发布版本不删除 tag；修复后发布更高 patch 版本，让自动更新继续前进。
- 便携版回滚只需换回旧 `Model-Gate-rust.exe`。

### GitHub 限流

未鉴权的 GitHub API 有速率限制。检查失败时面板显示错误原因并允许重试；不会影响代理转发。
若频繁触发，可稍后重试，或直接打开 Release 页面手动下载。

### 验证命令

```bat
cargo fmt --check
cargo check
cargo test
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\panel_api_smoke.ps1 -Exe dist\Model-Gate-rust.exe
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1 -Exe dist\Model-Gate-rust.exe
```

### 产物

| 产物 | 说明 |
|---|---|
| `dist\Model-Gate.exe` | 正式入口（Rust，GUI + `--no-gui`） |
| `dist\Model-Gate-v<版本>-rust-setup.exe` | Rust 安装包（Inno Setup） |
| `dist\Model-Gate-rust.exe` | Rust 候选/便携产物 |
| `dist\SHA256SUMS` | 安装包 SHA256 校验清单 |
| `dist\bridge.config.json` + `dist\data\` | 运行时配置与库（**不入库**） |
