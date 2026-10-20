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
| 配置解析 / 密钥解析（env → file → agent → 字面值，空 key 保留） | `config.rs` 单元测试 |
| SQLite 同构（迁移、索引、读写分库） | `db.rs` 单元测试 |
| 统计口径（totals/summary/list/models/upstreams，SQL 窗口分位） | `db.rs` 单元测试 |
| 面板全量 `/api/*`（config 保存/热重载、pricing、catalog、logs、export、清空、保留策略、健康探测、密钥写入） | `scripts\panel_api_smoke.ps1` |
| OpenAI / Anthropic / CommandCode（请求转换、非流式、SSE/NDJSON、tool/thinking、usage、**pause_turn 续跑**） | `cargo test` |
| 面板静态资源（自有副本 `rust/model-gate/ui/`，编译期内嵌） | `/ui/` 正常 |
| 桌面壳：托盘常驻 + 系统浏览器面板（打开面板、复制地址、数据目录、退出、今日指标） | `scripts\rust_shell_acceptance.ps1` |
| 单实例（签名互斥）/ 端口降级 / 请求调用日志（5 MB×3 轮转）、崩溃落盘 | 启动路径验证 |
| 从 agent 工具读取密钥（opencode / jcode / commandcode / 自定义） | `config.rs` 单元测试 |
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

## 从 agent 工具读取密钥

上游新增 `apiKeyAgent` 预设（`opencode` / `jcode` / `commandcode` / `custom`），
只读本机客户端凭据。优先级：`apiKeyEnv` → `apiKeyFile` → `apiKeyAgent` → `apiKey`。

| 预设 | 默认路径 | Provider 语义 |
|---|---|---|
| opencode | `~/.local/share/opencode/auth.json`（或 `~/.config/opencode/auth.json`） | provider 名，读取 `provider.key` |
| jcode | `~/.jcode/.env` | 变量名 |
| commandcode | `~/.commandcode/auth.json` | 无需填写，读取 `apiKey` |
| custom | 用 `apiKeyAgentPath` 指定 | JSON 路径（`a.b`）或 .env 变量名 |

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

面板「上游」页签可直接选择预设、填 Provider / 变量名与路径。

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
| 写入方式 | 临时文件 → 回读校验 → 原子替换；明文 `apiKey` 永不落盘 |

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
