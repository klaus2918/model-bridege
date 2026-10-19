# opencode go 直连接入（不经代理中转）——探索记录

> 2026-10-17 本机实测。目标：弄清"把 opencode go 直接接入客户端（不经 Model-Gate 8900 代理中转）"
> 的可行路径与全部前置条件。
>
> 结论先行：**可以直接接入**。任何"能自定义 base_url + Bearer key + 自定义请求头"的
> OpenAI 兼容客户端都能直连；唯一硬性门槛是 **chat 请求必须带 `x-opencode-session` 头**。

---

## 一、直连三要素（实测结论）

| 项 | 值 | 说明 |
|----|----|------|
| 端点 | `https://opencode.ai/zen/go/v1` | OpenAI 兼容（chat/completions、models） |
| 鉴权 | `Authorization: Bearer <OPENCODE_API_KEY>` | 独立 API key（非 opencode CLI 登录态，见第二节） |
| 会话头 | `x-opencode-session: <任意非空值>` | **chat/completions 必需**；`/models` 不需要 |
| User-Agent | `<常规 UA，如 curl/8.5.0>` | **禁用 Python-urllib 默认值**（被 Cloudflare 403 拦，见下） |

### 实测记录（2026-10-17，`mimo-v2.5`）

| # | 请求 | 结果 |
|---|------|------|
| 1 | `GET /models`（Bearer，无会话头） | **200**，返回 37 个模型 |
| 2 | `POST /chat/completions`（Bearer，无会话头） | **400** `MissingSessionID`：*"Request is missing x-opencode-session and cannot be routed efficiently"* |
| 3 | `POST /chat/completions`（Bearer + `x-opencode-session: <uuid>`） | **200**，正常返回补全与 usage |
| 4 | `GET /models`（UA 为 `Python-urllib/3.x` 默认值） | **403**，Cloudflare `error code: 1010`（换常规 UA 即 200） |

**两个容易踩的坑**：

1. **会话头**：chat 请求缺 `x-opencode-session` 必被 400 拒。值不需要是真实会话——
   **任意非空字符串/UUID 即可**（上游用它做"路由效率"标记，不做校验）。只要客户端能发一个
   固定或随机的 `x-opencode-session` 头，直连即成立。
2. **User-Agent**：opencode.ai 前置 Cloudflare 会按浏览器签名拦 UA——`Python-urllib/3.x`
   默认 UA 直接 403（`error code: 1010`）；`curl/x`、`python-httpx/x`（Model-Gate 实测可过）
   等常规 UA 正常。自定义 Python 客户端务必显式设置 UA：本仓库自检脚本设为 `curl/8.5.0`，
   Model-Gate 探测侧设为 `"cli"`，均实测可过。

### 模型快照（2026-10-17，37 个）

```
deepseek-v4.1-flash, deepseek-v4-pro, glm-5.1, glm-5.2, glm-5.3, glm-5.3-flash,
grok-4.6, grok-4.7, gpt-5.6-luna, gpt-6-luna, claude-haiku-5-5, omen-alpha,
muse-spark-1.2-contributor, muse-spark-1.3-contributor, gemini-4-argon-contributor,
hy3, hy4-preview, kimi-k2.6, kimi-k2.7-code, kimi-k3, minimax-m2.5, minimax-m2.7,
minimax-m3, mimo-v2.5, mimo-v2.5-pro, mimo-v2.6-flash, mimo-v2.6-pro,
space-bunny, longcat-2.0, longcat-2.5-preview-free, step-5-preview, beam-free,
qwen3.6-plus, qwen3.7-max, qwen3.7-plus, qwen3.8-max, qwen3.8-flash
```

> 注：`/models` 只列目录；部分模型实际调用时仍会上游报错（协议不支持、地区限制、
> 套餐未开通等），与 Model-Gate README 第五节的实测表一致，直连与代理的命运相同。

---

## 二、凭据：两把 67 字符 key 的实测差异（重要）

本机存在两把长度相同（67 字符）的"opencode-key"风格凭据，但直连实测结果不同：

| 来源 | 实测 | 结论 |
|------|------|------|
| `~/.jcode/.env` 的 `OPENCODE_API_KEY` | `GET /models` → **200** | ✅ 可作为直连凭据 |
| `~/.local/share/opencode/auth.json` 的 `opencode-go.key`（opencode CLI 登录态） | `GET /models` → **401 Invalid credential** | ❌ 不能直接外接复用 |

**推论**：opencode CLI 的登录凭据（auth.json）是其客户端内部凭据（可能与刷新/客户端机制绑定），
**不要试图"抄"它给其他客户端用**。直连必须使用独立发放的 API key（即 `OPENCODE_API_KEY` 体系；
本机已存在 `~/.jcode/.env`，由 jcode/Model-Gate 共用）。

> 这也直接回答了一个常见误解："opencode go 的配置接入" ≠ "读取 opencode CLI 的配置文件"——
> 正确做法是用 API key 直连，而不是复用 CLI 登录态。

---

## 三、各客户端落地方式

### 1) jcode（本机参考实现，已具备 = "直接接入"的现成案例）

`~/.jcode/config.toml`：

```toml
[provider]
default_model = "mimo-v2.5"
default_provider = "opencode-go"

[providers.opencode-go]
api = "openai"
api_key_env = "OPENCODE_API_KEY"   # 从 ~/.jcode/.env 读取
auth = "bearer"
base_url = "https://opencode.ai/zen/go/v1"
default_model = "mimo-v2.5"
type = "openai-compatible"

[[providers.opencode-go.models]]
context_window = 1000000
id = "mimo-v2.5"
# …（其余模型条目见 config.toml）
```

- jcode 二进制内置了 `x-opencode-session` 头的处理（无需额外配置）；
- `jcode provider current` 已验证：`resolved_provider = opencode-go, selected_model = mimo-v2.5`；
- `jcode usage` 显示 "Opencode Go" 有历史消耗记录（$0.09 all-time）。
- 用法：`jcode -p opencode-go`（或 `--provider-profile opencode-go`）直接使用；默认即为它。
- 可选端到端复核：`jcode run -p opencode-go -m mimo-v2.5 --quiet "Reply with exactly: DIRECT_OK"`
  （会真实调用一次上游；日常用依赖默认 provider 即可，无需手工指定）。

### 2) opencode CLI 自身

原生支持（官方客户端）：`opencode auth login` 登录 opencode 后即用，无需手工配置端点/会话头。

### 3) 其他 OpenAI 兼容客户端（通用条件）

**必要条件（缺一不可）**：
1. 可自定义 base_url → `https://opencode.ai/zen/go/v1`
2. 可配置 Bearer API key → `OPENCODE_API_KEY`
3. **可附加自定义请求头**（至少能加静态头 `x-opencode-session: <任意值>`）

支持 1+2 但不支持 3 的客户端 **不能直连**（chat 会稳定 400 `MissingSessionID`）——
这类场景要么等客户端支持自定义头，要么改用代理中转。

---

## 四、直连 vs 代理中转（Model-Gate 8900）

| 维度 | 直连 | 代理中转（Model-Gate 8900） |
|------|------|------------------------------|
| 客户端要求 | 支持自定义请求头 | 任意 OpenAI 客户端即可 |
| 会话头 | 客户端自己带 | 由代理注入（`sessionHeader: true`） |
| 本地依赖 | 无（无进程、无端口） | 需要 Model-Gate 常驻 |
| 概览 | 单一上游、点对点 | 四上游聚合、按名/前缀路由 |
| 用量统计 | 无 | 有（SQLite 落库、面板可视化） |
| 协议翻译 | 无 | CCH（Anthropic）/ CommandCode 均支持 |
| 密钥管理 | 每个客户端各自管理 | 一处配置、多客户端共享（且可脱敏） |
| 适用场景 | 单客户端原生接入、最小依赖 | 多工具统一入口、需要统计与翻译 |

**使用建议**：
- 只有一个客户端要接开放生态的 opencode go → 直连（少一层）。
- 多个工具、或需要统计/协议兼容/密钥一处管理 → 走 Model-Gate 代理。
- 二者可并存（本机现状：jcode 的 `opencode-go` 直连；`command-code` 走 8900 代理）。

---

## 五、自检脚本

`scripts/archive/one-off/opencode_go_direct_check.py`（一次性脚本，已归档）——对直连三要素做端到端自检：

```bat
py -3.11 scripts\archive\one-off\opencode_go_direct_check.py
:: 可选：--model glm-5.3-flash / --key-env OPENCODE_API_KEY / --env-file <path>
```

日常排查优先用统一入口：`py -3 scripts/doctor.py env`（密钥就位）、
`py -3 scripts/doctor.py upstream go`（端点连通与密钥有效性）。

输出示例（2026-10-17 实测）：

```
凭据：len=67（不显示明文）  端点：https://opencode.ai/zen/go/v1
[PASS] GET  /models -> HTTP 200, models=37
[INFO] POST chat（无 x-opencode-session）-> HTTP 400, error=MissingSessionID  （符合预期：会话头为硬性要求）
[PASS] POST chat（带 x-opencode-session）-> HTTP 200, finish=length, tokens=8+8
RESULT: DIRECT_OK
```

退出码：`0` = 直连可用；`2` = 不可用。

---

## 六、给 Model-Gate 的边界说明（本项目）

- 本记录不改变 Model-Gate 的任何行为；Model-Gate 的 `go` 上游（代理中转）照常工作。
- 若未来在 Model-Gate 中呈现"直连配置指导"，核心要点即本文第一节（三要素）+ 第二节（凭据边界）。
- 安全：`OPENCODE_API_KEY` 仅存于 `~/.jcode/.env`（外部凭据文件，只读、不落日志、不外发）；
  文档与脚本均不保存明文密钥。

---

## 七、平台侧接入（wenzflow / op-worker）——对象修正与落地（2026-10-17/18）

> **对象修正**：opencode 在平台上应作为 **Agent CLI 接入**（`agent.providers`，与
> codex/claude/jcode/reasonix 并列的"外部 Agent CLI"，设计依据：`000-task/01-设计/` 契约
> 明确"opencode 首版可纯 config 映射，不改代码"），**而不是**"模型接入"
> （`ai.providers`，平台直连模型端点）。此前误配的 `ai.providers.opencode-go` 已移除。

### 已落地的配置（CLI 接入）

`~/.op-worker/config.json → agent.providers.opencode`（备份：`config.json.bak-20261018-002612`）：

```json
{
  "agent": {
    "providers": {
      "opencode": {
        "display": "OpenCode",
        "command": "C:\\Users\\thk\\AppData\\Roaming\\npm\\node_modules\\opencode-ai\\bin\\opencode.exe",
        "args": ["run", "--auto", "-m", "opencode-go/mimo-v2.5"],
        "prompt": "stdin",
        "parser": "line"
      }
    }
  }
}
```

字段要点（对照 `crates/agent-adapter/src/main.rs` 的 CLI 接入实现）：
- `command`：指向 **opencode.exe 本体**（npm 的 `opencode.cmd/.ps1` 是包装脚本，Rust 子进程不宜直接拉起）；
- `args`：**不含 `{prompt}`**（prompt 走 stdin）；`--auto` 自动批准权限（非交互必需）；
  `-m opencode-go/mimo-v2.5` 指定使用 opencode-go 订阅模型（换模型即改此处）；
- `prompt: "stdin"`：**关键**——① `opencode run` 从 stdin 读取消息；② 实测**空 stdin 会导致
  opencode 启动崩溃**（`EUNKNOWN: unknown error, read`，bun 运行时读管道失败），写入 stdin 即规避；
- `parser: "line"`：逐行文本转 progress。

### 接入过程中发现并解决的两个问题

| # | 问题 | 处置 |
|---|------|------|
| 1 | opencode 的 `opencode-go` 凭据失效：`opencode run` 报 `Upstream request failed: Invalid credential`（与用该 key 直接请求 opencode.ai 得 401 一致） | 用 `~/.jcode/.env` 的有效 `OPENCODE_API_KEY` 替换 `~/.local/share/opencode/auth.json → opencode-go.key`（备份 `auth.json.bak-20261018-002410`）；不打印明文 |
| 2 | 空 stdin 启动崩溃（EUNKNOWN） | 配置改用 `prompt: "stdin"`（agent-adapter 把任务写入子进程 stdin） |

### 验证（2026-10-18）

- **端到端**：`"Reply with exactly: OC_DIRECT_OK" | opencode run --auto -m opencode-go/mimo-v2.5`
  → **exit=0**，模型在线（build agent 正常运行：读写文件、权限放行、agent 循环）；
- 对照实验：无 stdin 输入 → `EUNKNOWN` 启动崩溃（复现 2 次）；有 stdin → 正常启动；
- 配置层：写入后回读一致；`agent.active` 保持 `jcode` 不变（现有业务不中断）。

### 使用与回滚

- 平台内切换：工作台设置「当前接入」选择 **OpenCode**（或把 `agent.active` 改为 `"opencode"`）；
- 回滚配置：`copy /Y "%USERPROFILE%\.op-worker\config.json.bak-20261018-002612" "%USERPROFILE%\.op-worker\config.json"`；
- 回滚凭据：还原 `auth.json.bak-20261018-002410`（或用 `opencode providers login` 重新登录）。
