# 脚本目录

所有脚本默认从**仓库根目录**运行；路径以仓库根为基准，统一使用 Windows PowerShell 5.1。

根目录只保留会被重复使用的 PowerShell 工具；一次性排查脚本用完即删，不再保留 `archive/`。

## 活跃脚本

| 脚本 | 用途 |
|---|---|
| `ship.ps1` | **收尾单一入口**：噪音卫生 → 提交信息格式 → 编码/BOM → 敏感信息扫描 → 文本兜底校验 → 提交 → 推送 →（可选）打 tag 触发发布 |
| `hygiene.ps1` | 噪音清理/报告：`*.b64.png`、`debug*`、`*.stackdump`、`*.out/*.err/*.bak/*.tmp`；`-ReportOnly` 只看不删 |
| `ship-diagnose.ps1` | 查/等 release 流水线结论：失败时输出失败步骤与 `[FAIL]`/`::error::` 关键行（`-Wait` 轮询） |
| `panel_api_smoke.ps1` | 启动 `--no-gui` 实例，检查 `/api/info`、`/api/update/status`、`/ui/` 与更新资源 |
| `ui_smoke_v4.js` | v4 控制台无头浏览器冒烟：页面无 JS 异常、6 个一级入口、配置中心子页、接入抽屉、命令面板、用量分析、提示词、日志与诊断、设置与截图（需先启动实例并传 `--base`） |
| `db_migration_smoke.js` | 历史数据库兼容冒烟：旧 schema + 旧数据启动，验证自动迁移、旧数据可读与新写入 |
| `rust_shell_acceptance.ps1` | 托盘壳黑盒验收：启动耗时、内存、无 WebView2、二次启动、配置保存 |
| `upgrade_and_run.ps1` | 按 PID 停旧实例、启动版本化 exe 并等待 `/api/info` 就绪 |
| `install_config_guard.ps1` | 安装包随包文件回归：默认配置落地 / 不随包发独立说明页（说明在面板弹窗里） / 已有配置与 `modelgate.db` 不被覆盖 / 升级 / 卸载 / 便携首启 五场景断言 |
| `find_iscc.ps1` | 查找本机 `ISCC.exe`（Inno Setup 6） |
| `get_innosetup.ps1` | 下载并安装 Inno Setup 到 `tools/`（或提示手动安装） |

## 常用命令

```powershell
# 收尾一条命令：清理噪音 + 门禁 + 提交 + 推送（提交信息写 UTF-8 文件）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt

# 带发布：提交推送后打 tag（tag 必须与 Cargo.toml 版本一致）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt -Tag v4.0.1

# 只看门禁，不提交
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt -DryRun

# 发布后查结论（-Wait 轮询到完成；失败自动打印失败步骤与关键行）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship-diagnose.ps1 -Wait

# 面板接口 + 更新资源冒烟（需先构建 dist\Model-Gate-rust.exe）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\panel_api_smoke.ps1 -Exe dist\Model-Gate-rust.exe

# v4 控制台冒烟（先启动实例，例如 dist\Model-Gate-rust.exe --port 8912；失败时保留现场）
node scripts\ui_smoke_v4.js --base http://127.0.0.1:8912

# 历史数据库兼容冒烟（旧 schema + 旧数据，自动迁移后读写验证）
node scripts\db_migration_smoke.js --exe dist\Model-Gate-rust.exe

# v4 冒烟并刷新文档截图
node scripts\ui_smoke_v4.js --base http://127.0.0.1:8912 --screenshot-dir docs\design\refactor-2026\screenshots-v4

# 桌面壳验收
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1 -Exe dist\Model-Gate.exe

# 查找/安装 Inno Setup
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\find_iscc.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\get_innosetup.ps1
```

`ui_smoke_v4.js` / `db_migration_smoke.js` 需要 Node 22+（内置 WebSocket / `node:sqlite`）与本机 Chrome/Edge（仅 UI 冒烟）；默认自动探测，找不到时用 `--chrome` 指定。`db_migration_smoke.js` 会打印 Node 的实验性 SQLite 警告，不影响结果。

## 约定

- 脚本输出不得包含密钥、Cookie、Authorization 头或完整请求体；只打印来源、状态码、耗时。
- 临时文件写到系统临时目录或 `dist/`，不要写仓库根目录；相关模式已在 `.gitignore` 中。
- 删除/停止实例时按 **PID**，不要按进程名批量结束。
- 改动 PowerShell 脚本后至少做一次语法解析（`[System.Management.Automation.Language.Parser]::ParseFile`）
  并跑对应冒烟；中文脚本必须保存为 UTF-8 带 BOM，避免 PowerShell 5.1 按 GBK 解码。
