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
| 配置解析 / 密钥解析（env → file → 字面值，空 key 保留） | 与 Python 同配置启动 |
| SQLite 同构表（迁移、索引、读写分离） | Python `Database` 直接打开 Rust 写的库 |
| 统计口径（totals/summary/list/models/upstreams，SQL 窗口百分位） | 同一库逐字段一致 |
| 面板全量 `/api/*`（config 保存/热重载、pricing、catalog、logs、export、清空、保留策略、健康探测、密钥写入） | `panel_api_smoke.py` 20/20 |
| OpenAI / Anthropic / CommandCode：请求转换、非流式、SSE/NDJSON、tool/thinking、usage、**pause_turn 续跑** | `parity_rust.py` 全部 `[ok]` |
| 面板静态资源（复用 `modelgate/web/`，零改动） | `/ui/` 正常；五页 CDP 冒烟通过 |
| 桌面壳：托盘常驻 + 系统浏览器面板（打开面板、复制地址、数据目录、退出） | `scripts\rust_shell_acceptance.ps1` 10/10（含无 msedgewebview2） |
| 单实例（命名互斥体）/ 端口降级 / 文件日志 / 崩溃落盘 | 启动路径验证 |

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

## 代码结构

```
src/
  main.rs        CLI / 启动 / 单实例 / 日志
  config.rs      配置、上游规整、密钥解析
  db.rs          SQLite（读写分离 + SQL 百分位）
  proxy.rs       HTTP 路由、代理、流式、面板 API
  api_extra.rs   面板扩展接口（catalog/logs/export/pricing/retention 等）
  anthropic.rs   Anthropic 协议适配
  commandcode.rs CommandCode 协议适配
  gui.rs         托盘 + Win32 消息循环 + 系统浏览器
```

前端资源通过 `include_str!` 编译进二进制，发布时无需额外文件。
