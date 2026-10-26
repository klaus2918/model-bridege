# 脚本目录

所有脚本默认从**仓库根目录**运行；路径以仓库根为基准，统一使用 Windows PowerShell 5.1。

根目录只保留会被重复使用的 PowerShell 工具；一次性排查脚本用完即删，不再保留 `archive/`。

## 活跃脚本

| 脚本 | 用途 |
|---|---|
| `panel_api_smoke.ps1` | 启动 `--no-gui` 实例，检查 `/api/info`、`/api/update/status`、`/ui/` 与更新资源 |
| `ui_smoke.js` | 无头浏览器 UI 冒烟：自动启动实例、注入提示词，验证页签/弹窗/详情/展开折叠/会话演进等真实交互 |
| `db_migration_smoke.js` | 历史数据库兼容冒烟：旧 schema + 旧数据启动，验证自动迁移、旧数据可读与新写入 |
| `rust_shell_acceptance.ps1` | 托盘壳黑盒验收：启动耗时、内存、无 WebView2、二次启动、配置保存 |
| `upgrade_and_run.ps1` | 按 PID 停旧实例、启动版本化 exe 并等待 `/api/info` 就绪 |
| `install_config_guard.ps1` | 安装包随包文件回归：默认配置落地 / 不随包发独立说明页（说明在面板弹窗里） / 已有配置与 `modelgate.db` 不被覆盖 / 升级 / 卸载 / 便携首启 五场景断言 |
| `find_iscc.ps1` | 查找本机 `ISCC.exe`（Inno Setup 6） |
| `get_innosetup.ps1` | 下载并安装 Inno Setup 到 `tools/`（或提示手动安装） |

## 常用命令

```powershell
# 面板接口 + 更新资源冒烟（需先构建 dist\Model-Gate-rust.exe）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\panel_api_smoke.ps1 -Exe dist\Model-Gate-rust.exe

# 无头浏览器 UI 冒烟（自动起停实例；失败时保留现场目录）
node scripts\ui_smoke.js --exe dist\Model-Gate-rust.exe

# 历史数据库兼容冒烟（旧 schema + 旧数据，自动迁移后读写验证）
node scripts\db_migration_smoke.js --exe dist\Model-Gate-rust.exe

# 保留现场并指定端口/浏览器
node scripts\ui_smoke.js --exe dist\Model-Gate-rust.exe --port 18943 --debug-port 19223 --keep

# 桌面壳验收
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\rust_shell_acceptance.ps1 -Exe dist\Model-Gate.exe

# 查找/安装 Inno Setup
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\find_iscc.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\get_innosetup.ps1
```

`ui_smoke.js` / `db_migration_smoke.js` 需要 Node 22+（内置 WebSocket / `node:sqlite`）与本机 Chrome/Edge（仅 UI 冒烟）；默认自动探测，找不到时用 `--chrome` 指定。`db_migration_smoke.js` 会打印 Node 的实验性 SQLite 警告，不影响结果。

## 约定

- 脚本输出不得包含密钥、Cookie、Authorization 头或完整请求体；只打印来源、状态码、耗时。
- 临时文件写到系统临时目录或 `dist/`，不要写仓库根目录；相关模式已在 `.gitignore` 中。
- 删除/停止实例时按 **PID**，不要按进程名批量结束。
- 改动 PowerShell 脚本后至少做一次语法解析（`[System.Management.Automation.Language.Parser]::ParseFile`）
  并跑对应冒烟；中文脚本必须保存为 UTF-8 带 BOM，避免 PowerShell 5.1 按 GBK 解码。
