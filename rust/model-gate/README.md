# Model-Gate Rust 内核

> 2.19 起为**默认产品实现**：`dist\Model-Gate.exe` 由本目录构建。
> 配置格式、SQLite 表结构、HTTP 契约与面板 HTML/CSS/JS 与 Python 版完全一致；
> Python 版保留为 `dist\Model-Gate-python.exe` 一个发布周期用于回退。

## 构建与投放

```bat
cd rust\model-gate
cargo build --release
:: 或一步到位（含投放 dist，-Promote 覆盖正式入口并备份 Python 版）
powershell -ExecutionPolicy Bypass -File build-release.ps1 -Promote
:: 安装包（需 Inno Setup 6+；自动查找 ISCC.exe）
powershell -ExecutionPolicy Bypass -File build-installer.ps1
```

开发期 `cargo build` 迭代更快；release 使用
`opt-level="z" + lto + codegen-units=1 + strip`。

## 能力清单（全部已验证）

| 能力 | 验证 |
|---|---|
| 配置解析 / 密钥解析（env → file → agent → 字面值，空 key 保留） | 与 Python 同配置启动 |
| SQLite 同构表（迁移、索引、读写分离） | Python `Database` 直接打开 Rust 写的库 |
| 统计口径（totals/summary/list/models/upstreams，SQL 窗口百分位） | 同一库逐字段一致 |
| 面板全量 `/api/*`（config 保存/热重载、pricing、catalog、logs、export、清空、保留策略、健康探测、密钥写入） | `panel_api_smoke.py` 20/20 |
| OpenAI / Anthropic / CommandCode：请求转换、非流式、SSE/NDJSON、tool/thinking、usage、**pause_turn 续跑** | `parity_rust.py` 全部 `[ok]` |
| 面板静态资源（复用 `modelgate/web/`，零改动） | `/ui/` 正常；五页 CDP 冒烟通过 |
| 桌面壳：托盘常驻 + 系统浏览器面板（打开面板、复制地址、数据目录、退出、今日指标） | `scripts\rust_shell_acceptance.ps1` 10/10（含无 msedgewebview2） |
| 单实例（命名互斥体）/ 端口降级 / 请求调用日志（5 MB×3 轮转）/ 崩溃落盘 | 启动路径验证 |
| 从 agent 工具读取密钥（opencode / jcode / commandcode / 自定义） | `config.rs` 单元测试 |

## 对等回归

```bat
py scripts\parity_rust.py
py scripts\panel_api_smoke.py --exe dist\Model-Gate.exe
:: Python 回退版（pywebview/WebView2）真实五页 CDP 冒烟：
set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333
dist\Model-Gate-python.exe --port 18952 --db %TEMP%\mg\data\modelgate.db --force-multi
node scripts\ui_smoke_cdp.mjs
```

覆盖：`/v1/models`、`/api/upstreams`、三协议非流式 + 流式、用量 totals/summary，
除时间戳/时延类字段外逐字段一致。

## 实测（同配置、headless）

| 指标 | Python 版 | Rust 版 |
|---|---|---|
| exe | 31.4 MB | **4.5 MB** |
| 启动到 `/health` | 2423 ms | **553 ms**（热启动约 130 ms） |
| 进程树 working set | 61.4 MB | **15.7 MB** |
| 进程树 private | 37.6 MB | **7.2 MB** |

安装版长时间运行（v2.19.0，10.5 小时）：working set 39.33 MB、private 11.8 MB、
26 线程、438 句柄。该口径包含托盘与共享系统 DLL，与上表 headless 启动采样不可直接比较。

## 请求调用日志

每个请求完成后写一行 access log（受 `accessLog` 控制，默认开启）：

```text
deepseek-flash -> deepseek:deepseek-chat 200 1234ms ttft=210ms stream tokens=1800+420
deepseek-flash -> deepseek:deepseek-chat 502 120ms 失败: 无法连接上游 ...
```

日志文件为 `data/modelgate.log`；启动时超过 5 MB 自动轮转，保留
`modelgate.log.1` ~ `.3`，最旧一份丢弃。面板「日志」页保持 4 秒自动刷新。

## 从 agent 工具读取密钥

上游新增 `apiKeyAgent` 预设（`opencode` / `jcode` / `commandcode` / `custom`），
只读本机客户端凭据。优先级：`apiKeyEnv` → `apiKeyFile` → `apiKeyAgent` → `apiKey`。

| 预设 | 默认路径 | Provider 含义 |
|---|---|---|
| opencode | `~/.local/share/opencode/auth.json`（或 `~/.config/opencode/auth.json`） | provider 名，读取 `provider.key` |
| jcode | `~/.jcode/.env` | 变量名 |
| commandcode | `~/.commandcode/auth.json` | 无需填写，读取 `apiKey` |
| custom | 由 `apiKeyAgentPath` 指定 | JSON 点路径（`a.b`）或 .env 变量名 |

```json
{
  "upstreams": {
    "deepseek": {
      "protocol": "openai",
      "baseUrl": "https://api.deepseek.com/v1",
      "apiKeyAgent": "opencode",
      "apiKeyAgentProvider": "deepseek"
    }
  }
}
```

面板「上游 → 密钥来源（agent 工具）」可直接选择预设、填写 Provider / 变量名与路径。

## 托盘今日指标

托盘 tooltip 与右键菜单显示「今日请求数」「缓存命中率」（`cache_read / prompt`），
每 10 秒刷新（后台任务查库、主线程只读快照）；无样本时命中率显示「—」。Windows 托盘不支持常驻标题文字，
因此不调用仅 macOS 生效的 `set_title`。

## 配置即 JSON：唯一事实源与备份

`bridge.config.json` 是配置的**唯一事实源**：面板的每一次保存都写回该文件，
数据库里的快照只是运行态补充，不能把用户已删除的上游复活。

| 行为 | 规则 |
|------|------|
| 新增 / 删除上游 | 保存后 `upstreams` 同步增删（删除是真实的，不被顶回），重启后不复活 |
| 局部保存 | 只改传入的字段（如 `{"defaultModel": ...}`），其余顶层键与上游保持原值 |
| 未渲染字段 | 面板不展示的兼容字段（`cliVersion` / `compat` / `modelCatalog`…）逐字段保留 |
| 写入方式 | 临时文件 → 回读校验 → 原子替换；明文 `apiKey` 永不落盘 |

**备份**（面板「配置落盘与备份」区）：

- **首次强制备份**：目录里没有任何备份时，第一次保存强制生成 `bridge.config.json.bak`（不受开关影响）。
- **后续自主选择**：「保存前备份」开关默认开启；关闭后不再生成新备份，已有备份不删除。
- **轮转**：保留最近 5 份（`.bak` + `.bak.1`～`.bak.4`）。
- **恢复**：`POST /api/config/backups/restore`（或面板「恢复上一次备份」按钮）；恢复前另存 `.bak.pre-restore`。
- 备份内容是**保存前**的旧配置。

**升级衔接**：安装脚本只覆盖 exe，`bridge.config.json` 与 `data\modelgate.db` 原地保留；
默认库路径与配置文件同目录（`data\modelgate.db`），从任意目录启动都读同一份旧库，
不会因工作目录变化而「历史清零」或在别处生成空库。

## 代码结构

```
src/
  main.rs        CLI / 启动 / 单实例 / 日志
  config.rs      配置、上游规整、密钥解析
  config_store.rs 配置落盘：合并语义 + 原子写 + 备份策略
  db.rs          SQLite（读写分离 + SQL 百分位）
  proxy.rs       HTTP 路由、代理、流式、面板 API
  api_extra.rs   面板扩展接口（catalog/logs/export/pricing/retention/backups 等）
  anthropic.rs   Anthropic 协议适配
  commandcode.rs CommandCode 协议适配
  gui.rs         托盘 + Win32 消息循环 + 系统浏览器
```

前端资源通过 `include_str!` 编译进二进制，发布时无需额外文件。
