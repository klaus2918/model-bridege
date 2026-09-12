# model-bridge — 使用说明

一个本地代理，把 **OpenAI 格式**的请求按模型名分发到**三条上游**，并自动处理各上游的协议差异。

```
Agent（OpenAI 格式）
   ↓  http://127.0.0.1:8900/v1            ← 按模型名自动分发
   ↓  http://127.0.0.1:8900/go/v1         ← 用 URL 前缀强制指定上游
model-bridge
   ├─ cch   (Anthropic 协议) → 127.0.0.1:15721 → CCH → DeepSeek
   ├─ go    (OpenAI 协议)    → opencode.ai/zen/go/v1  ← Go 订阅套餐
   └─ zen   (OpenAI 协议)    → opencode.ai/zen/v1     ← 免费模型
```

---

## 一、怎么指定与识别上游

**两种方式，可混用。**

### 方式一：按模型名自动分发（默认）

用统一的 `http://127.0.0.1:8900/v1`，bridge 按 `model` 字段查各上游的 `models` 清单，命中谁走谁：

```
model: deepseek-flash    → cch
model: glm-5.3-flash     → go
model: big-pickle        → zen
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
{"error":{"message":"未知的上游前缀 \"nosuch\"。可用前缀：/cch、/go、/zen；..."}}
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

1. **协议错配**：Agent 只会发 OpenAI 格式的 `/v1/chat/completions`，而 CCH 那条上游只认 Anthropic 格式的 `/v1/messages`，需要在中间翻译
2. **opencode 的会话头要求**：opencode.ai 要求每个请求带 `x-opencode-session` 头，缺失直接 400 拒绝（`MissingSessionID`），而普通客户端不会发这个头

model-bridge 把这两件事都收敛到一个进程里，Agent 只需指向一个地址。

---

## 三、快速开始

### 1. 启动

双击 `start-bridge.cmd`，或：

```bat
cd /d D:\githup-workspace\Model-Gate
node model-bridge.js
```

启动成功会列出各上游：

```
[model-bridge] 已启动 http://127.0.0.1:8900/v1
  上游 cch        anthropic http://127.0.0.1:15721  [默认]
  上游 go         openai    https://opencode.ai/zen/go  会话头注入
  上游 zen        openai    https://opencode.ai/zen  会话头注入
[model-bridge] 声明的模型: deepseek-flash, glm-5.3-flash, ...
```

> 窗口要**保持开着**，关掉即服务停止。

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

### go（opencode Go 订阅，需 `OPENCODE_API_KEY`）

已实测在 chat 路径可用的 **25 个**：

| 系列 | 模型 |
|------|------|
| GLM | `glm-5.3-flash`、`glm-5.3`、`glm-5.2`、`glm-5.1` |
| Kimi | `kimi-k3`、`kimi-k2.7-code`、`kimi-k2.6` |
| DeepSeek | `deepseek-v4.1-flash`、`deepseek-v4-flash`、`deepseek-v4-flash-vision-exp`、`deepseek-v4-pro`、`deepseek-flash` |
| MiniMax | `minimax-m3`、`minimax-m2.5` |
| Qwen | `qwen3.8-max`、`qwen3.8-flash`、`qwen3.7-max`、`qwen3.7-plus`、`qwen3.6-plus` |
| MiMo | `mimo-v2.5-pro`、`mimo-v2.5` |
| 其它 | `longcat-2.0`、`hy4-preview`、`hy3`、`omen-alpha` |

### zen（免费，无需密钥）

| 模型 | 说明 |
|------|------|
| `mimo-v2.5-free`、`ling-3.0-flash-fin-free`、`nemotron-3-ultra-free`、`nemotron-3.5-lightning-free`、`big-pickle` | 免费，**有限流** |

### 同名冲突说明

`deepseek-flash` 在 cch 和 go 里都有。不带前缀时会走 **cch**（配置里在前）。要明确走 go：

```bat
curl http://127.0.0.1:8900/go/v1/chat/completions -H "Content-Type: application/json" ^
  -d "{\"model\":\"deepseek-flash\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
```

### Go 里当前不可用的模型（已实测排除，未列入配置）

| 模型 | 状态 | 原因 |
|------|------|------|
| `grok-4.6`、`grok-4.5` | ✗ | 需 `/v1/responses` 协议，走 chat 报 `not supported for format oa-compat` / `Model unavailable` |
| `gpt-5.6-luna` | ✗ | 需 `/v1/responses` 协议，走 chat 报 500 |
| `muse-spark-1.3-contributor`、`muse-spark-1.2-contributor` | ✗ | 403 `not available in your country`（地区限制） |
| `minimax-m2.7` | ✗ | 500 Internal server error（重试两次均失败） |
| `kimi-k2.5`、`glm-5`、`qwen3.5-plus`、`mimo-v2-pro`、`mimo-v2-omni`、`hy3-preview` | ✗ | 400 `Model is unavailable`（套餐内未开通或已下线） |

> 上表是 2026-09-12 的实测快照。上游随时可能调整，想重新确认可以给 go 加 `"autoModels": true` 拉全量对比（见第七节）。

### 两个上游的限制

- **Zen 免费模型有限流**：用多了返回 `429 FreeUsageLimitError`，稍等恢复。上游配额策略，非本工具问题
- **Go 需要密钥**：从 `~/.jcode/.env` 的 `OPENCODE_API_KEY` 读取（见第六节）

### 想自己核对上游到底有哪些模型

```bat
:: 只看 go 声明了什么（不联网）
curl http://127.0.0.1:8900/go/v1/models

:: 看上游实时全量（需给该上游配 "autoModels": true）
curl http://127.0.0.1:8900/v1/models
```

## 六、配置文件 `bridge.config.json`

```json
{
  "port": 8900,
  "host": "127.0.0.1",
  "defaultModel": "deepseek-flash",
  "maxTokens": 8192,
  "timeoutSeconds": 600,
  "accessLog": true,
  "modelsCacheSeconds": 300,
  "thinking": { "passthrough": true, "cacheSize": 500 },
  "upstreams": {
    "cch": {
      "protocol": "anthropic",
      "baseUrl": "http://127.0.0.1:15721",
      "apiKey": "any-key",
      "default": true,
      "models": ["deepseek-flash"]
    },
    "go": {
      "protocol": "openai",
      "baseUrl": "https://opencode.ai/zen/go/v1",
      "apiKeyEnv": "OPENCODE_API_KEY",
      "sessionHeader": true,
      "models": ["glm-5.3-flash", "..."]
    },
    "zen": {
      "protocol": "openai",
      "baseUrl": "https://opencode.ai/zen/v1",
      "apiKey": "",
      "sessionHeader": true,
      "models": ["mimo-v2.5-free", "..."]
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
| `envFile` | 密钥文件路径，默认 `~/.jcode/.env`；设 `false` 禁用 |

### 每个上游的字段

| 字段 | 含义 |
|------|------|
| `protocol` | `anthropic`（走 `/v1/messages`）或 `openai`（走 `/v1/chat/completions`） |
| `baseUrl` | 上游地址。**带不带 `/v1` 都行**，工具会自动规范化，不会拼出 `/v1/v1` |
| `apiKey` | 直接写密钥；**写空字符串 `""` 表示不发送鉴权头**（Zen 免费模型就是这种） |
| `apiKeyEnv` | 从环境变量/`.env` 文件读取密钥（推荐，避免密钥落盘） |
| `default` | 设为 `true` 的上游承接未匹配到任何模型的请求 |
| `models` | 该上游负责的模型名清单，用于按名分发 |
| `sessionHeader` | 是否注入 `x-opencode-session`（opencode 类上游必须为 `true`） |
| `fetchModels` | 是否从上游拉取模型清单（默认 `true`） |
| `autoModels` | 是否把上游**全部**模型也暴露给下游（默认 `false`，只暴露 `models` 里声明的） |

**改完配置需重启才生效。**

### 密钥放哪

推荐用 `apiKeyEnv`，密钥从 `~/.jcode/.env` 读取（可用 `envFile` 改路径）：

```
# ~/.jcode/.env
OPENCODE_API_KEY=sk-xxxxx
```

也可以启动前设环境变量：
```bat
set OPENCODE_API_KEY=sk-xxxxx
node model-bridge.js
```
环境变量优先于 `.env` 文件。

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
| `/go/v1/models` | go 的清单（25 个） |
| `/zen/v1/models` | zen 的清单（5 个） |

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

**仅对 `protocol: anthropic` 的上游生效**（cch）。openai 类上游原样透传，不做处理。

---

## 十、日志怎么看

```
[时间] 请求模型 -> 上游名:实际模型 状态码 耗时 [stream] tokens=输入+输出 [think=补回条数]
```

例：
```
[2026-09-12T17:46:20.123Z] deepseek-flash -> cch:deepseek-flash 200 782ms tokens=336+25 think=1
[2026-09-12T17:46:22.456Z] glm-5.3-flash -> go:glm-5.3-flash 200 1090ms stream tokens=289+55
```

---

## 十一、常见问题

| 现象 | 原因 | 处理 |
|------|------|------|
| `EADDRINUSE` | 8900 被占用 | 换端口，或结束占用进程（提示里已给出命令） |
| 请求 502 | 上游连不上 | cch：确认 15721 在监听；go/zen：确认能访问 opencode.ai |
| **429 限流** | Zen 免费模型配额用尽 | 稍后重试，或改用其他模型 |
| **401 Invalid API key** | 该上游不该带 key 却带了，或 key 无效 | Zen 免费模型的 `apiKey` 必须是 `""`；Go 需要有效 `OPENCODE_API_KEY` |
| 401 Missing API key | Go 没读到密钥 | 检查 `~/.jcode/.env` 里的 `OPENCODE_API_KEY` |
| 400 `MissingSessionID` | 该上游未注入会话头 | 确认该上游 `sessionHeader` 不为 `false` |
| 模型报不存在 | 该模型不在任何上游的 `models` 里，被送到默认上游 | 把模型名加到对应上游的 `models` |
| 流式没有 `[DONE]` | 部分上游（如 minimax-m3）本身不发 | 上游行为，客户端按连接关闭结束即可 |

---

## 十二、运行前提

- **Node.js**：需要（本机已装）。无第三方依赖，不用 `npm install`
- **cch 上游**：需本地代理在 `127.0.0.1:15721` 运行
- **go 上游**：需 `OPENCODE_API_KEY` 且在 `~/.jcode/.env` 里
- **zen 上游**：无需密钥
- **系统**：Windows（`start-bridge.cmd`）；`model-bridge.js` 本身跨平台

---

## 十三、文件清单

| 文件 | 作用 |
|------|------|
| `model-bridge.js` | 主程序，零依赖单文件 |
| `bridge.config.json` | 配置（三条上游定义） |
| `start-bridge.cmd` | 一键启动 |
| `README.md` | 本说明 |
| `package.json` | 版本唯一真源（`version` 字段）与 `type: module` |
| `changelog/v<版本>.json` | 每个版本的发行说明来源（缺失时发布流水线直接失败） |
| `scripts/` | 发布流水线脚本（扫描 / 打包 / 校验和 / 自检），CI 与本地共用 |
| `.github/workflows/release.yml` | tag 驱动的发布流水线

---

## 十四、发布流程（维护者）

发布口只有一个：推 `v*` tag，其余全自动。

```
push tag v1.2.3
  └─ create-release：校验 tag 与 package.json 版本一致 → 由 changelog/v1.2.3.json 渲染说明 → 建 draft release
       └─ package：敏感信息扫描（工作区 + 全历史对象）→ 语法自检 → 打包（显式清单，不含 .git）→ 产物自检（真实启动 smoke）→ 生成 SHA256SUMS
            └─ finalize：必达资产校验（缺一即保持 draft）→ 上传资产 → 转 public → 发布后自检（重新下载已公开产物核对）
```

### 本地演练（不触碰远程）

```bash
bash scripts/scan-sensitive.sh --all-history          # 敏感信息门禁
bash scripts/build-package.sh                         # 产出 dist/model-bridge-v<版本>.zip
bash scripts/verify-package.sh dist/model-bridge-v1.0.0.zip   # 产物自检（含真实启动）
bash scripts/generate_checksums.sh dist SHA256SUMS .zip       # 生成校验和
```

### 正式发布

```bash
# 1. 先补 changelog/v<版本>.json（缺失时 CI 会直接失败，不会静默发出去）
# 2. 提交版本与 changelog
git add package.json changelog/ && git commit -m "release: v1.2.3"
git push origin master
# 3. 推 tag 触发流水线
git tag -a v1.2.3 -m "release: v1.2.3" && git push origin v1.2.3
```

### tag 事件被丢弃 / 发布链路刚改过

```bash
bash scripts/retrigger-release.sh --dry-run v1.2.3     # 只查前提，不动任何东西
bash scripts/retrigger-release.sh --at HEAD v1.2.3     # 重建到 HEAD 后重推（--at 必需：tag 指旧提交会跑旧 workflow）
```

### 发布后独立复核

```bash
bash scripts/verify-release.sh v1.2.3                  # 会重新下载已公开资产，核对元数据/资产集合/校验和/产物内容
# 下载资产后本地核对
sha256sum -c SHA256SUMS                                # Linux/macOS
Get-FileHash model-bridge-v1.2.3.zip -Algorithm SHA256 # Windows
```

### 硬规则

- **版本唯一真源**是 `package.json` 的 `version`，tag 名必须与它一致（CI 强制校验，防止「tag 是 v1.0.0 但产物报别的版本」）
- **tag 只增不改**：已公开的 release 绝不删 tag 重推（会破坏消费者与校验和的可追溯性），要改就发新的 patch 版本
- **发布提交只放三类文件**：版本声明（`package.json`）+ changelog；不要混入代码改动
- **词表与扫描器分离**：扫描规则在 `scripts/scan-sensitive.sh` 里可入库；组织专有敏感词放不入库的 `.sensitive-terms`
- **改写历史后必须复核**：推送后独立克隆再扫一遍，本地视角不能证明远程状态（见知识库《Git 仓库开源发布安全流程》）
