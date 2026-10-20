# Rust 桌面壳（托盘 + 系统浏览器面板）

> 默认产品 `dist\Model-Gate.exe` 的桌面形态：代理常驻托盘，面板在系统默认浏览器打开，
> 进程树内不出现 `msedgewebview2.exe`。本文替换旧版「wry/WebView2 桌面壳」设计。

关联文档：`docs/architecture/tech-stack-selection.md`（选型与指标）、
`docs/architecture/upstream-config-model.md`（配置模型）。

## 1. 目标与边界

| 目标 | 验收口径 |
|---|---|
| 常驻 working set ≤20MB | GUI 模式空闲 60s 的进程 working set（不含浏览器） |
| 安装包 ≤6MB | `dist\Model-Gate-v<ver>-rust-setup.exe` |
| 启动 ≤1s | 进程启动到 `GET /health` 返回 200 |
| 无内嵌浏览器 | Model-Gate 进程树内 0 个 `msedgewebview2.exe` |

边界：不做 WebView2 可选后端；不做内嵌窗口；不改面板前端（`modelgate/web/` 原样复用）；
托盘动作不依赖页面加载成功。浏览器打开失败时弹窗给出可复制的 URL。

## 2. 进程与线程模型

- **单进程**：axum 服务线程 + 主线程 Win32 消息循环；发布构建 `windows_subsystem = "windows"`，无控制台窗口。
- **托盘**：`tray-icon`（Win32 后端），主线程 `GetMessageW` 循环消费菜单事件。
- **面板**：`ShellExecuteW` 打开 `http://<host>:<实际端口>/ui/`，浏览器为独立进程，不计入 Model-Gate 进程树。
- **前端资源**：`modelgate/web/` 通过 `include_str!` 编译进二进制，无外部资源文件。

## 3. 启动流程

1. 解析参数：`--config --port --host --db --no-gui --no-generate-config --force-multi --open --no-browser --diagnose`；
2. `--diagnose`：只做只读检查，输出 `data/diagnose.txt`（脱敏：不含 apiKey 明文），不启动服务、不进入单实例判断；
3. 安装 panic hook（写 `data/crash.log`）；启动致命错误写 `data/fatal.log` 并弹窗；
4. 单实例守卫：按 db 路径生成命名互斥体（`CreateMutexW`）。已存在时：
   - `--no-browser` 直接退出（供脚本/自动化使用）；
   - 否则读 `settings.lastPort`（只读连接，重试约 3s 覆盖首实例写入窗口），打开已有面板后退出；
5. 加载配置与 SQLite；`bind_with_fallback` 从配置端口起逐个尝试，端口占用时自动降级；
6. 首启判定：`settings.lastPort` 缺失视为首次运行；写回实际端口；
7. 是否打开面板：`!no_browser && (open || first_run)`；
8. `--no-gui` 走纯服务模式（Ctrl+C 退出）；否则进入托盘消息循环。

## 4. 托盘菜单

| 菜单项 | 行为 |
|---|---|
| 打开面板 | Shell 打开 `http://<host>:<端口>/ui/` |
| 刷新模型目录 | 触发 `autoModels` 上游目录拉取（失败保留旧缓存） |
| 复制接入地址 | 剪贴板写入 `http://<host>:<端口>/v1` |
| 打开数据目录 | Shell 打开 `data/`（配置、日志、SQLite） |
| 接入地址（只读项） | 展示当前实际端口，端口降级后同步更新 |
| 退出 | 停止服务并退出进程 |

## 5. 单实例与端口

- 互斥体粒度与 `--db` 相同：同一数据目录只允许一个实例；并存需 `--force-multi` + 独立端口/库；
- `settings.lastPort` 由运行实例写入，二次启动只读不迁移；
- 端口降级后，面板 URL、托盘只读项、`lastPort` 均以实际端口为准。

## 6. 崩溃与诊断

| 产物 | 触发 | 内容 |
|---|---|---|
| `data/modelgate.log` | 运行日志 | 访问与错误日志（不记请求头/密钥） |
| `data/crash.log` | panic hook | 时间、线程、位置、可选 backtrace（`RUST_BACKTRACE=1`） |
| `data/fatal.log` | 启动致命错误 | 错误链；同时弹窗提示路径 |
| `data/diagnose.txt` | `--diagnose` | 环境、配置路径、端口可绑定性、`lastPort`；密钥脱敏 |

## 7. 验收脚本与实测（2026-10-20）

`powershell -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1`（release）10/10 通过：

| 检查项 | 实测 |
|---|---|
| 启动到 `/health` 200 | 170ms（另一次负载下 670ms） |
| 空闲 60s working set | 19.32MB（另一次 19.35MB） |
| 进程树 `msedgewebview2` | 0（无子进程） |
| 二次启动激活 | 46ms 退出，端口监听仍为首实例 |
| exe 体积 | 4.19MB |
| 安装包 | 3.8MB（旧构建；装 Inno Setup 后按 `build-installer.ps1` 重打） |
| `profiles` / `modelOverrides` 往返 | 通过 |

构建侧入口：`rust\model-gate\build-release.ps1`（可选 `-Acceptance` 自动跑上述验收）、
`build-installer.ps1`（Inno Setup，ISCC 缺失时明确报错）。

## 8. 回退

- `--no-gui` 纯服务模式不受桌面壳影响；
- 直接运行 `dist\Model-Gate-python.exe`（配置/DB 双向兼容，见 tech-stack-selection.md）；
- 桌面壳异常时崩溃信息落在 `data/crash.log`，可用 `--diagnose` 收集环境信息。

## 9. 设计决策

- **不保留 wry/WebView2 可选后端**：内嵌渲染进程是内存大头，保留它会让「进程树无 msedgewebview2」
  永远存在例外，安装包与文档也继续背着运行时依赖；
- **系统浏览器而非本地窗口**：面板是同源 HTTP 页面，浏览器天然隔离；托盘动作（打开/复制/刷新/退出）
  不依赖页面；
- **托盘 + 单实例 + lastPort**：避免二次启动重复监听，保持「双击 exe = 打开面板」的旧习惯。
