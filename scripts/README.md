# 脚本目录

所有脚本默认从**仓库根目录**运行；路径以仓库根为基准。Python 脚本用 `py -3` / `python`，Bash 脚本用 Git Bash。

根目录只保留会被重复使用的工具；新增前先确认能否并入现有脚本（诊断类统一进 `doctor.py`）。

## 活跃脚本

### 构建与测试

| 脚本 | 用途 |
|---|---|
| `version.py` | 读取 `modelgate/__init__.py` 的版本号（构建注入用） |
| `make_icon.py` | 生成 `assets/ModelGate.ico` |
| `run_tests.py` | 逐文件有界运行 pytest（`build.bat` 的测试门禁） |

### 打包与发布（Legacy Node 流水线）

| 脚本 | 用途 |
|---|---|
| `build-package.sh` | 打 Legacy Node 发布 zip；`--list` 查看包内条目 |
| `verify-package.sh` | 解包、语法、模板一致性、真实启动 smoke |
| `verify-release.sh` | 发布后反向拉取资产复核 |
| `generate_checksums.sh` | 生成 `SHA256SUMS` |
| `render-notes.mjs` | 由 `legacy/node-bridge/changelog/` 渲染发行说明 |
| `retrigger-release.sh` | 删除并重建 `legacy-v*` tag（需 `--dry-run` 先演练） |
| `append-assets-section.sh` | 幂等追加发行说明中的资产段 |
| `lib/common.sh` | 上述脚本共用的日志、版本、GitHub API 函数 |

### 安全与凭据

| 脚本 | 用途 |
|---|---|
| `scan-sensitive.sh` | 工作区/历史敏感信息扫描（`--all-history` 用于发布前） |
| `scan_secrets.py` | Python 敏感信息扫描器 |
| `selftest_scan_secrets.py` | 扫描器自测 |
| `audit_secrets.py` | 检查 `bridge.config.json` 等敏感文件未被跟踪 |
| `audit_git_history_secrets.py` | 扫描 git 历史对象 |
| `validate_text_staged.py` | 校验暂存区文本编码/换行/冲突标记 |
| `describe_credentials.py` | 只读描述凭据来源（不打印密钥） |

### 运行、诊断与验收

| 脚本 | 用途 |
|---|---|
| `doctor.py` | **统一诊断入口**：`port` / `config` / `env` / `db` / `upstream` / `cli` / `runtime` / `gui` / `all` |
| `live_config_validate.py` | 在线配置校验（需要网络） |
| `upgrade_and_run.ps1` | 按 PID 停旧实例、启动版本化 exe 并等待 `/api/info` 就绪 |
| `verify_exe_guard.py` / `verify_port_drift.py` | exe 产物保护与端口漂移断言 |
| `find_iscc.ps1` / `get_innosetup.ps1` / `make_config_template.py` | 本机环境辅助 |

### 上游探测与端到端

| 脚本 | 用途 |
|---|---|
| `probe_all.py` / `probe_upstreams.py` | 探测本机/上游可用模型与协议 |
| `real_e2e.py` / `stub_upstream.py` | 真实链路端到端与本地 stub 上游 |

## 归档脚本

`scripts/archive/` 只保留历史证据，不参与日常运行，可能仍引用旧版本路径：

| 子目录 | 内容 |
|---|---|
| `versions/` | `verify_v216` 至 `verify_v25` 的逐版本验证脚本 |
| `docs-tools/` | 向 README/计划批量追加章节的一次性脚本 |
| `probes/` | 早期免费模型、网关、CLI 探测脚本 |
| `diagnostics/` | 已由 `doctor.py` 取代的 `check_*`、`config_check`、`show_runtime`、`diag_*`、`gui_probe` |
| `one-off/` | 一次性修复验证、冷启动、导入/面板 smoke、交付物检查、直连排查脚本 |

新增一次性排查脚本请直接放 `scripts/archive/`；需要长期复用的才放本目录根。

## 约定

- 排查顺序：先跑 `py -3 scripts/doctor.py all`，再用具体子命令深入；不要再新增 `check_*` / `diag_*` 单文件，能力补进 `doctor.py`。
- 脚本输出不得包含密钥、Cookie、Authorization 头或完整请求体；只打印来源、状态码、耗时。
- 临时文件写到系统临时目录或 `dist/`，不要写仓库根目录；相关模式已在 `.gitignore` 中。
- 删除/停止实例时按 **PID**，不要按进程名批量结束。
- 改动脚本后至少运行 `bash -n <script>`（Bash）或 `python -m py_compile <script>`（Python）。
