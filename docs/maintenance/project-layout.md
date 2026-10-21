# 项目目录与维护规范

适用 Model-Gate 2.21+（Rust-only）。最后核对：2026-10-21。

## 一、目录职责

| 路径 | 职责 | 是否入库 |
|---|---|---|
| `rust/model-gate/` | 唯一产品源码：代理内核、协议适配、配置、SQLite、更新、托盘壳 | 是 |
| `rust/model-gate/ui/` | 内嵌面板静态资源（`index.html` / `app.js` / `style.css` / `free_sources.js`）——**面板资源只有这一份** | 是 |
| `rust/model-gate/tests/` | 集成测试：契约守卫（`user_env_contracts.rs` / `key_file_contracts.rs`）与夹具（含 `legacy_config.json` 兼容样本） | 是 |
| `rust/model-gate/provider_profiles.json` | Provider 预设注册表（编译期内嵌） | 是 |
| `rust/model-gate/Cargo.toml` | 版本唯一源与依赖 | 是 |
| `assets/` | 构建资产（图标等） | 是 |
| `docs/` | 长期文档：架构、集成记录、维护规范 | 是 |
| `scripts/` | 活跃 PowerShell 工具：验收、升级、Inno Setup 查找/安装、面板冒烟 | 是 |
| `ModelGate.iss` | Inno Setup 打包定义（固定 AppId，支持覆盖升级） | 是 |
| `bridge.config.example.json` | 零密钥配置模板 | 是 |
| `bridge.config.json` | 本机真实配置（可能含路径与密钥引用） | **否** |
| `dist/` | 安装包、便携 exe、正式入口 | **否** |
| `data/`、`dist/data/` | 运行数据库与日志 | **否** |
| `target/` | Cargo 构建缓存 | **否** |
| `.op/` | 变更计划、验证证据、归档记录（含本机路径） | **否** |

> **Python 旧实现已彻底移除**：`modelgate/`、根 `tests/`、`entry.py`、`ModelGate.spec`、
> `build.bat`、`legacy/node-bridge/` 与 `scripts/` 下的 Python/Shell/MJS 脚本均已删除
> （历史保留在 git 历史中）。仓库只剩 Rust 一条产品线，面板资源只有
> `rust/model-gate/ui/` 一份。**不要**再引入第二份前端副本或 Python 回退实现。

## 二、新增文件的归属规则

1. **产品代码**只进 `rust/model-gate/src/`；面板资源只进 `rust/model-gate/ui/`，**不要新增第二份前端副本**。
2. **测试**只进 `rust/model-gate/src` 的 `#[cfg(test)]` 或 `rust/model-gate/tests/`；临时库写到系统临时目录，不写仓库根目录。
3. **脚本**只保留 PowerShell，放在 `scripts/`：构建/验收/升级/打包辅助各一个入口；一次性排查脚本用完即删，不建 `archive/`。
4. **文档**按主题进 `docs/`；使用说明只维护根 `README.md`，不在多处复制。
5. **生成物**一律进 `dist/`、`target/`、`data/` 或系统临时目录，不回写源码目录。

## 三、生成物与清理策略

| 对象 | 生成方式 | 清理规则 |
|---|---|---|
| `target/` | `cargo build` | 可 `cargo clean`；排查构建问题时保留 |
| `dist/Model-Gate.exe` | `build-release.ps1 -Promote` | 保留当前构建；运行中的 exe 被锁定，不要删 |
| `dist/Model-Gate-rust.exe` | `build-release.ps1` | 可随时重建 |
| `dist/Model-Gate-v*-rust-setup.exe` | `build-installer.ps1` / Release | 保留当前版与上一版用于回滚，其余可删 |
| `dist/data/`、`data/` | 桌面实例运行 | **活数据**，运行中禁止删除；备份用面板导出/备份功能 |
| `%TEMP%\model-gate-update-*` | 应用内更新下载 | 7 天后自动清理；失败残留可手动删除 |
| `rust/model-gate/cargo_*.log` | 本地构建日志 | 验证完成即删，不入库 |

建议每次发布后清理（先确认没有正在运行的构建）：

```powershell
cargo clean
Remove-Item -Recurse -Force target -ErrorAction SilentlyContinue
Remove-Item -Force rust\model-gate\cargo_*.log -ErrorAction SilentlyContinue
```

## 四、敏感信息边界

以下内容**只允许存在于本机**，并且必须被 `.gitignore` 覆盖：

| 路径/模式 | 内容 | 处理方式 |
|---|---|---|
| `bridge.config.json` | 真实上游地址、`apiKeyEnv`/`apiKeyFile` 引用，可能含本机路径 | 不入库；分享用 `bridge.config.example.json` |
| `.env`、`*.env` | 用户自管的环境变量文件（程序不再内置读取） | 不入库 |
| 用户显式指定的凭据文件（JSON / `.env`） | 用户自管凭据 | 仓库外；程序只在配置显式给出路径时读取，**无内置默认路径**；面板「写入指定文件」也只写用户手填的那个路径 |
| `**/auth.json`、`**/credentials*.json`、`*.key`、`*.pem`、`*.token` | 凭据兜底规则 | 不入库 |
| `data/`、`dist/data/` | SQLite 用量库、运行日志 | 不入库 |
| `.op/` | 变更记录，可能含本机绝对路径与源码片段 | 不入库 |

原则：

- **不会被提交的敏感文件不需要脱敏**；但必须确认它确实未被跟踪，且忽略规则有效。
- 需要入库的示例只能使用占位符：`apiKeyEnv`、空路径的 `apiKeyFile`/`apiKeyAgentPath`、`example.com`，不写真实密钥、账号、Cookie、请求头，也不写死具体厂商凭据路径。
- 日志只记录上游名、状态码、耗时与用量字段，不记录请求头与正文。
- 若敏感文件曾被提交：先 `git rm --cached` 并补忽略规则，再**轮换对应密钥**；仅删除文件不等于从历史移除。

核验命令（只读）：

```powershell
git ls-files | Select-String -Pattern 'bridge\.config\.json$|(^|/)\.env$|auth\.json$|credentials.*\.json$|\.(key|pem|token)$'
git check-ignore -v bridge.config.json data/modelgate.db .op/index.json
```

第一条应无输出；`git check-ignore` 应命中忽略规则。

## 五、版本与发布

- 当前产品版本唯一来源：`rust/model-gate/Cargo.toml` 的 `version`。
- 桌面 exe：`build-release.ps1`；安装包：`build-installer.ps1`（Inno Setup 6）。
- 发布：推送 `v<version>` tag，由 `.github/workflows/release-rust.yml` 构建并上传
  `Model-Gate-v<version>-rust-setup.exe`、`Model-Gate-rust.exe`、`SHA256SUMS`。
- 应用内更新只接受与 tag 精确匹配的安装包名；版本号不一致时发布失败，不生成资产。

## 六、改动前检查清单

1. `git status --short`：确认没有把 `dist/`、`target/`、`data/`、`.op/`、真实配置加入暂存区。
2. 新增/移动文件后同步更新 `README.md`、`docs/README.md` 或本文件中的路径。
3. 验证：`cargo fmt --check`、`cargo check`、`cargo test`；面板改动加跑 `scripts/panel_api_smoke.ps1`。
4. 更新相关改动加跑安装版双版本升级验收（见根 README「应用内更新」）。
5. 环境/配置异常先看 `data/modelgate.log` 与 `/health`，再检查 `bridge.config.json` 是否合法 JSON。
6. 不覆盖正在运行的实例：换端口、独立库、按 PID 停旧实例。
