# Model-Gate Rust 内核

> **唯一产品实现**：`dist\Model-Gate.exe` 由本目录构建。Python 版与 legacy Node 桥接
> 已随 Rust-only 改造全部删除，仓库只保留这一条产品线。

## 构建与投放

```bat
cd rust\model-gate
cargo build --release
:: 或一步到位（投放 dist，Promote 覆盖正式入口）
powershell -ExecutionPolicy Bypass -File build-release.ps1 -Promote
:: 安装包（需 Inno Setup 6+；自动查找 ISCC.exe）
powershell -ExecutionPolicy Bypass -File build-installer.ps1
```

开发期用 `cargo build` 迭代更快；release 使用
`opt-level="z" + lto + codegen-units=1 + strip`。

## 能力清单

| 能力 | 验证 |
|---|---|
| 配置解析 / 密钥解析（env → file → agent；**无字面量凭据、无内置预设路径**） | `config.rs` 单元测试 |
| SQLite 同构（迁移、索引、读写分库） | `db.rs` 单元测试 |
| 统计口径（totals/summary/list/models/upstreams，SQL 窗口分位） | `db.rs` 单元测试 |
| 面板全量 `/api/*`（config 保存/热重载、pricing、catalog、logs、export、清空、保留策略、健康探测、密钥写入） | `scripts\panel_api_smoke.ps1` |
| OpenAI / Anthropic / CommandCode（请求转换、非流式、SSE/NDJSON、tool/thinking、usage、**pause_turn 续跑**） | `cargo test` |
| 面板静态资源（自有副本 `rust/model-gate/ui/`，编译期内嵌） | `/ui/` 正常 |
| 桌面壳：托盘常驻 + 系统浏览器面板（打开面板、复制地址、数据目录、退出、今日指标） | `scripts\rust_shell_acceptance.ps1` |
| 单实例（签名互斥）/ 端口降级 / 请求调用日志（5 MB×3 轮转）、崩溃落盘 | 启动路径验证 |
| 从 agent 工具读取密钥（**仅自定义路径**，无内置 opencode / jcode / commandcode 预设） | `config.rs` 单元测试 |
| **应用内更新**：GitHub Release 检查、SHA256 校验、Inno 静默覆盖升级 | `cargo test` + `tests\update_probe.rs` |

## 对等回归

```bat
cd rust\model-gate
cargo test
:: 面板接口冒烟（需先构建 dist\Model-Gate-rust.exe）
powershell -NoProfile -ExecutionPolicy Bypass -File ..\..\scripts\panel_api_smoke.ps1 -Exe ..\..\dist\Model-Gate-rust.exe
```

覆盖：`/v1/models`、`/api/upstreams`、三协议非流式 + 流式、用量 totals/summary。

## 应用内更新与网络环境

更新模块访问 `https://api.github.com/repos/klaus2918/model-bridege/releases/latest`。
客户端**同时信任系统根证书与 webpki 内置根证书**，因此在装有本地加速器
（Steam++ / Watt Toolkit 等，通过 hosts 把 GitHub 劫持到 `127.0.0.1` 并用自签 CA
做 HTTPS 中间人）的机器上也能正常握手。

失败会给出可读分类，便于定位：

| 错误码 | 含义 |
|---|---|
| `update_tls` | 证书校验失败：网络被加速器/代理劫持 |
| `update_proxy` | 代理无法连通 |
| `update_timeout` | 连接超时 |
| `update_network` | 普通网络不可达 |
| `update_rate_limited` | GitHub API 限流 |
| `update_asset_missing` | 该 Release 缺少安装包或 `SHA256SUMS` |
| `update_checksum_mismatch` | SHA256 不匹配，已拒绝安装 |

## 请求调用日志

每个请求完成后写一行 access log（受 `accessLog` 控制，默认开启）：

```text
deepseek-flash -> deepseek:deepseek-chat 200 1234ms ttft=210ms stream tokens=1800+420
deepseek-flash -> deepseek:deepseek-chat 502 120ms 失败: 无法连接上游 ...
```

日志文件为 `data/modelgate.log`，启动时超过 5 MB 自动轮转，保留 `modelgate.log.1` ~ `.3`。
面板「日志」页签保持 4 秒自动刷新。

## 密钥来源

密钥只从环境变量或你显式指定的凭据文件读取。`apiKeyEnv` 指定变量名，取值顺序：

1. 进程环境变量；
2. 本进程写入的会话缓存（Windows 环境块是启动时快照，运行中读不到新写入的注册表值）；
3. Windows 用户变量 `HKCU\Environment`。

面板 API Key 行的「写入密钥」有两个目标，**都没有内置默认路径**：

| 写入目标 | 落点 | 说明 |
|---|---|---|
| 写入用户环境变量 | `HKCU\Environment` | 写 `apiKeyEnv` 指定的变量名（缺省用该上游已配置的 `apiKeyEnv`）；写完广播 `WM_SETTINGCHANGE`，当前进程靠会话缓存立即生效，新进程由系统继承 |
| 写入指定文件 | 你手填的绝对路径 | JSON（根字段 `apiKey` 或点路径，如 `deepseek.key`）或 `.env`（变量名）；读-改-写保留其它字段，同目录临时文件 + `rename` 原子替换，**不生成 `.bak`**；写完用运行时同一套读取语义回读校验 |

写入成功后运行态立即生效（`keySource` = `user-env` / `file` / `agent:file/<名>` / `agent:env-file/<名>`）；
配置文件里只保存**路径引用**（`apiKeyFile` / `apiKeyAgentPath`），不保存密钥本体。
若该上游同时配置了 `apiKeyEnv`，环境变量优先级更高，界面会提示「文件里的密钥暂不生效」。
写入 Model-Gate 自己的 `bridge.config.json` 会被拒绝。

旧字段 `apiKeyFile` / `apiKeyAgent` 仍兼容，但**没有任何内置预设路径**：

| 字段 | 行为 |
|---|---|
| `apiKey` | 已废弃：JSON 中的明文值不再作为凭据读取，只给 `legacy_literal_api_key` 迁移提示，不回显原值 |
| `apiKeyFile` | 仅当你显式填写路径时，读取该 JSON 的 `apiKey` 字段；**路径本身可以写入 `bridge.config.json`**（只保存路径，不保存密钥） |
| `apiKeyAgentPath` | 仅当你显式填写路径时读取；支持 JSON 点路径或 `.env` 变量名；路径同样可写入 `bridge.config.json` |
| `envFile` | 已失效：启动时给迁移提示，不阻断启动 |

```json
{
  "upstreams": {
    "deepseek": {
      "protocol": "openai",
      "baseUrl": "https://api.deepseek.com/v1",
      "apiKeyEnv": "DEEPSEEK_API_KEY"
    }
  }
}
```

面板「上游」页签可直接填环境变量名，在 API Key 行选「写入用户环境变量」或「写入指定文件」（后者需填路径、格式、字段名）后点「写入密钥」；也可以在「密钥文件（apiKeyFile）」里填绝对路径，从该 JSON 的 `apiKey` 字段读取；其他格式用「自定义凭据文件」填字段名/变量名与路径。凭据文件路径必须由你手动填写。

## 托盘今日指标

托盘 tooltip 与右键菜单显示今日请求数、缓存命中率（`cache_read / prompt`）。
每 10 秒刷新（后台任务查库，主线程只读快照）；无采样时命中率显示 `—`。

Windows 托盘不支持常驻鼠标正文字，
因此不调用仅 macOS 生效的 `set_title`。

## 配置即 JSON：落盘事实源与备份

`bridge.config.json` 是配置的**唯一事实源**：面板的每一次保存都写回该文件，
数据库中的快照只是运行时补充，不能把用户已删除的上游恢复回来。

| 行为 | 规则 |
|------|------|
| 新增 / 删除上游 | 保存时按 `upstreams` 同步增删（删除是真的，不会被顶回），重启后不复活 |
| 局部保存 | 只改传入的字段（如 `{"defaultModel": ...}`），其余顶层键与上游保持原样 |
| 未展示字段 | 面板不展示的兼容字段（`cliVersion` / `compat` / `modelCatalog`…）逐字段保留 |
| 写入方式 | 临时文件 → 回读校验 → 原子替换；面板保存永不携带 `apiKey`，后端落盘前再剥离 |

**备份**（面板配置落盘与备份区）：

- **首次强制备份**：目录里没有任何备份时，首次保存强制生成 `bridge.config.json.bak`（不受开关影响）。
- **后续自主选择**：可保存前备份。开关默认开；关闭后不再生成新备份，已有备份不删除。
- **轮转**：保留最近 5 份（`.bak` + `.bak.1`…`.bak.4`）。
- **恢复**：`POST /api/config/backups/restore`（或面板「恢复上一次备份」按钮）。恢复前另存 `.bak.pre-restore`。
- 备份内容是*保存前*的旧配置。

**升级衔接**：安装脚本只覆盖 exe，`bridge.config.json` 与 `data\modelgate.db` 原地保留；
默认库路径与配置文件同目录（`data/modelgate.db`），从任意目录启动都能读同一份旧库，
不会因工作目录变化而丢历史或清理升级时在别处生成空库。

## 代码结构

```
src/
  main.rs        CLI / 启动 / 单实例 / 日志
  config.rs      配置、上游归一化、密钥解析
  config_store.rs 配置落盘：合并语义 + 原子写 + 备份策略
  key_file.rs    凭据文件写入：JSON / .env 读-改-写 + 原子替换 + 安全护栏
  user_env.rs    Windows 用户级环境变量写入（HKCU\Environment）
  db.rs          SQLite（读写分库 + SQL 窗口分位）
  proxy.rs       HTTP 路由、代理、流式、面板 API
  api_extra.rs   面板扩展接口（catalog/logs/export/pricing/retention/backups 等）
  anthropic.rs   Anthropic 协议适配
  commandcode.rs CommandCode 协议适配
  opencode_transport.rs opencode 传输层（会话头、工具、SSE 聚合）
  free_sources.rs / free_sources_api.rs 免费来源探测
  provider_profiles.rs Provider 预设注册表
  thinking.rs    thinking 回传缓存
  update.rs      应用内更新（Release 检查 / 下载 / SHA256 / 静默安装）
  gui.rs         托盘 + Win32 消息循环 + 系统浏览器
  ui_assets.rs   面板资源契约守卫
ui/              面板静态资源（index.html / app.js / style.css / free_sources.js）
tests/fixtures/  Rust 单测夹具（legacy 配置、golden 快照）
```

前端资源通过 `include_str!` 编译进二进制，发布时无需额外文件。
