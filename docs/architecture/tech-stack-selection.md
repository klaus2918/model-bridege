# 技术栈选型与落地结果（Model-Gate）

> 结论：用 **Rust** 替换 Python 代理内核，保持配置、SQLite 表、HTTP 契约与
> 面板 HTML/CSS/JS 完全不变。2.19 起 `dist\Model-Gate.exe` 已是 Rust 产物，
> 桌面壳为**托盘常驻 + 系统浏览器面板**（不内嵌 WebView2）；
> Python 版保留为 `dist\Model-Gate-python.exe` 一个发布周期。

## 一、为什么是 Rust

1. **常驻内存与启动**：Rust 单进程；Python 版是 PyInstaller onefile（父/子两进程）。
2. **长流无 GC 抖动**：SSE / NDJSON 可能持续数分钟，Rust 无 GC 停顿。
3. **SQLite 同构**：`rusqlite(bundled)` 直接复用现有表结构与 SQL 口径，
   10 万行统计用窗口函数只取两个边界样本，峰值内存从 MB 级降到 KB 级。
4. **界面零改动**：面板复用 `modelgate/web/` 原文件，由系统默认浏览器打开；
   托盘用 `tray-icon` + Win32 消息循环，不引入 Tauri/WebView2 全家桶。

## 二、最终实测（Windows）

### 2.1 桌面壳（GUI，空闲 60s，2026-10-20 release）

| 指标 | 结果 |
|---|---|
| 可执行文件 | **4.19 MB** |
| 启动到 `/health` 200 | **170 ms**（复测负载下 670 ms） |
| 常驻 working set | **19.3 MB**（空闲 60s，不含浏览器） |
| 进程树 `msedgewebview2` | **0** |
| 二次启动激活 | 46 ms 退出，不重复监听 |
| 安装包 | 3.8 MB（旧构建；Inno Setup 装好后重打并复测 ≤6MB） |

> 面板由系统浏览器打开，浏览器进程独立于 Model-Gate 进程树；
> 因此「常驻内存」只统计代理进程，不再随面板页面数放大。

### 2.2 headless 对照（`--no-gui`，早期基线）

| 指标 | Python 版 | Rust 版 | 变化 |
|---|---|---|---|
| 可执行文件 | 31.4 MB | **4.5 MB** | −86% |
| 启动到 `/health` 200 | 2423 ms | **553 ms**（热启动约 130 ms） | −77% |
| 进程树 working set | 61.4 MB | **15.7 MB** | −74% |
| 进程树 private | 37.6 MB | **7.2 MB** | −81% |
| 10 万行统计峰值内存 | 13.9–20.5 MB/端点 | **4–31 KB/端点** | ≈ 3 个数量级 |

## 三、功能对等验证（全部通过）

| 验证 | 命令 | 结果 |
|---|---|---|
| 三协议 × 非流式/流式 + 用量口径 | `py scripts\parity_rust.py` | 10 项全部 `[ok]` |
| 面板 20 个接口（含 config 保存/热重载、pricing、catalog、logs、export、保留策略） | `py scripts\panel_api_smoke.py --exe dist\Model-Gate.exe` | 20/20 可达 |
| 桌面壳指标（内存/启动/无 WebView2/二次启动/配置保存） | `powershell -File scripts\rust_shell_acceptance.ps1` | 10/10 通过 |
| 面板五页渲染（Python 版 pywebview，历史验证） | `node scripts\ui_smoke_cdp.mjs` | 五页均渲染、0 控制台错误 |

## 四、迁移阶段（已完成）

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 | 选型与 POC | ✅ |
| P1 | 配置/密钥、SQLite 同构、OpenAI 透传 | ✅ |
| P2 | Anthropic、CommandCode、SSE/NDJSON、pause_turn、故障切换 | ✅ |
| P3 | 面板全量 API、配置热重载、静态资源内嵌 | ✅ |
| P4 | 托盘 + 系统浏览器桌面壳、单实例、端口降级、文件日志 | ✅ |
| P5 | 单 exe 投放、入口切换、构建脚本、对等回归 | ✅ |

## 五、风险与后续

| 风险 | 处理 |
|---|---|
| 系统浏览器缺失/打开失败 | 托盘动作不依赖页面；打开失败弹窗给出可复制 URL，可手动访问 |
| 安装包重建待工具链 | 本机当前无 `iscc`；`ModelGate.iss` 已参数化，装 Inno Setup 后 `build-installer.ps1` 一条命令出包并复测 ≤6MB |
| 上游协议变更 | 三协议适配各自独立模块，桩上游 + 对等脚本可快速回归 |
| 回退 | `dist\Model-Gate-python.exe` 保留一个发布周期，配置/DB 完全兼容 |

桌面壳细节见 [`rust-desktop-shell.md`](rust-desktop-shell.md)；配置模型见
[`upstream-config-model.md`](upstream-config-model.md)。
