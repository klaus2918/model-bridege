# 技术栈选型与落地结果（Model-Gate）

> 结论：Model-Gate 当前**仅保留 Rust 实现**，配置、SQLite 表、HTTP 契约与
> 面板 HTML/CSS/JS 与迁移前保持一致。`dist\Model-Gate.exe` 为 Rust 产物，
> 桌面壳为**托盘常驻 + 系统浏览器面板**（不内嵌 WebView2）。
> 本文保留迁移前的对照数据与决策背景，不代表仍有第二套运行时。

## 一、为什么是 Rust

1. **常驻内存与启动**：Rust 单进程；Python 版是 PyInstaller onefile（父/子两进程）。
2. **长流无 GC 抖动**：SSE / NDJSON 可能持续数分钟，Rust 无 GC 停顿。
3. **SQLite 同构**：`rusqlite(bundled)` 直接复用现有表结构与 SQL 口径，
   10 万行统计用窗口函数只取两个边界样本，峰值内存从 MB 级降到 KB 级。
4. **界面零改动**：面板迁入 `rust/model-gate/ui/` 并编译期内嵌，由系统默认浏览器打开；
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

### 2.2 headless 对照（`--no-gui`，迁移前基线，历史数据）

| 指标 | Python 版 | Rust 版 | 变化 |
|---|---|---|---|
| 可执行文件 | 31.4 MB | **4.5 MB** | −86% |
| 启动到 `/health` 200 | 2423 ms | **553 ms**（热启动约 130 ms） | −77% |
| 进程树 working set | 61.4 MB | **15.7 MB** | −74% |
| 进程树 private | 37.6 MB | **7.2 MB** | −81% |
| 10 万行统计峰值内存 | 13.9–20.5 MB/端点 | **4–31 KB/端点** | ≈ 3 个数量级 |

## 三、迁移期对等验证（历史结果）

| 验证 | 当时命令 | 结果 |
|---|---|---|
| 三协议 × 非流式/流式 + 用量口径 | 迁移期对等脚本（已删除） | 10 项全部 `[ok]` |
| 面板接口（含 config 保存/热重载、pricing、catalog、logs、export、保留策略） | 迁移期冒烟脚本（已删除） | 全部可达 |
| 桌面壳指标（内存/启动/无 WebView2/二次启动/配置保存） | `scripts\rust_shell_acceptance.ps1` | 10/10 通过 |
| 面板五页渲染 | 迁移期 CDP 冒烟（已删除） | 五页均渲染、0 控制台错误 |

当前回归入口：`cargo test` + `scripts\panel_api_smoke.ps1` + `scripts\rust_shell_acceptance.ps1`。

## 四、迁移阶段（已完成）

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 | 选型与 POC | ✅ |
| P1 | 配置/密钥、SQLite 同构、OpenAI 透传 | ✅ |
| P2 | Anthropic、CommandCode、SSE/NDJSON、pause_turn、故障切换 | ✅ |
| P3 | 面板全量 API、配置热重载、静态资源内嵌 | ✅ |
| P4 | 托盘 + 系统浏览器桌面壳、单实例、端口降级、文件日志 | ✅ |
| P5 | 单 exe 投放、入口切换、构建脚本、对等回归 | ✅ |
| P6 | 删除 Python/Node 产品代码、脚本与 legacy 发布链路，文档改为 Rust-only | ✅ |

## 五、风险与后续

| 风险 | 处理 |
|---|---|
| 系统浏览器缺失/打开失败 | 托盘动作不依赖页面；打开失败弹窗给出可复制 URL，可手动访问 |
| 安装包重建待工具链 | 本机当前无 `iscc`；`ModelGate.iss` 已参数化，装 Inno Setup 后 `build-installer.ps1` 一条命令出包并复测 ≤6MB |
| 上游协议变更 | 三协议适配各自独立模块，`cargo test` + 桩上游可快速回归 |
| 回退 | 保留上一版安装包，手动重装即可；配置/DB 原地保留并完全兼容 |

桌面壳细节见 [`rust-desktop-shell.md`](rust-desktop-shell.md)；配置模型见
[`upstream-config-model.md`](upstream-config-model.md)。
