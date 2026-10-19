# 项目目录与维护规范

适用 Model-Gate 2.x。最后核对：2026-10-19。

## 一、目录职责

| 路径 | 职责 | 是否入库 |
|---|---|---|
| `modelgate/` | 当前产品源码：配置、SQLite、协议适配、代理、桌面壳、面板资产 | 是 |
| `tests/` | pytest 测试；文件名与被测模块对应 | 是 |
| `entry.py` | 桌面版入口（窗口/托盘/单实例） | 是 |
| `assets/` | 构建资产（图标等） | 是 |
| `docs/` | 长期文档：架构、集成记录、面板素材、维护规范 | 是 |
| `scripts/` | 活跃工具：构建、测试、安全扫描、运行诊断、发布 | 是 |
| `scripts/archive/` | 已过期的一次性版本验证、探测、文档补丁脚本 | 是（仅作历史证据） |
| `legacy/node-bridge/` | 冻结的 Node 单文件桥、测试与历史 changelog | 是 |
| `ModelGate.spec`、`ModelGate.iss`、`build.bat` | PyInstaller / Inno Setup 打包定义与唯一构建入口 | 是 |
| `bridge.config.example.json` | 零密钥配置模板 | 是 |
| `bridge.config.json` | 本机真实配置（可能含路径与密钥引用） | **否** |
| `dist/`、`build/` | 打包产物、构建中间目录 | **否** |
| `data/`、`dist/data/` | 运行数据库与日志 | **否** |
| `.op/` | 变更计划、验证证据、归档记录（含本机路径） | **否** |

## 二、新增文件的归属规则

1. **产品代码**只进 `modelgate/`；跨模块共享逻辑放 `modelgate/` 顶层模块，不建新的根目录。
2. **测试**只进 `tests/`，命名 `test_<主题>.py`；临时库写到 `tmp_path`，不写仓库根目录。
3. **脚本**：
   - 构建/测试/发布/安全扫描等会重复使用的，放 `scripts/` 根目录；
   - 运行诊断统一走 `scripts/doctor.py`（`port` / `config` / `env` / `db` / `upstream` / `cli` / `runtime` / `gui`），不再新增 `check_*` / `diag_*` 单文件；
   - 只服务于某次版本或某次排查的一次性脚本，放 `scripts/archive/<类别>/`，不再放根目录；
   - 所有脚本默认从仓库根目录运行，路径以仓库根为基准。
4. **文档**按主题进 `docs/`；使用说明只维护根 `README.md`，不在多处复制。
5. **生成物**一律进 `dist/`、`build/`、`data/` 或系统临时目录，不回写源码目录。
6. **Legacy Node 版**只做兼容修复，不新增面板、用量、故障切换等桌面版能力；新增 Node 相关文件放 `legacy/node-bridge/`。

## 三、生成物与清理策略

| 对象 | 生成方式 | 清理规则 |
|---|---|---|
| `build/` | PyInstaller | 可随时整目录删除；排查打包问题时再保留 |
| `dist/Model-Gate.exe` | `build.bat` | 保留当前构建；运行中的 exe 被锁定，不要删 |
| `dist/Model-Gate-v*.exe` | 升级时的并存实例 | 保留当前运行版与上一版用于回滚，其余可删 |
| `dist/data/` | 桌面实例运行 | **活数据**，运行中禁止删除；备份用面板导出/备份功能 |
| `dist/preview/` | 面板预览/测试 | 可删；下次预览会重建 |
| `.pytest_cache/`、`__pycache__/` | pytest / Python | 可随时删除，已被 `.gitignore` 排除 |
| `ModelGate.v*.spec` | 升级时临时复制 | 验证完成即删，已被 `.gitignore` 排除 |
| `docs/live-panel/*.mp4` | `render.py --out` | 体积大，本地生成不入库；需要时按该目录 README 重生成 |

建议在每次发布后或每季度执行一次（先确认没有正在运行的构建）：

```powershell
Remove-Item -Recurse -Force build,.pytest_cache -ErrorAction SilentlyContinue
Get-ChildItem -Recurse -Directory -Filter __pycache__ | Remove-Item -Recurse -Force
Get-ChildItem -Filter 'ModelGate.v*.spec' | Remove-Item -Force
```

`scripts/archive/` 是历史证据，不参与日常运行；确认某段历史不再需要时可整目录删除，但不要把它当作活跃工具目录继续加文件。

## 四、敏感信息边界

以下内容**只允许存在于本机**，并且必须被 `.gitignore` 覆盖：

| 路径/模式 | 内容 | 处理方式 |
|---|---|---|
| `bridge.config.json` | 真实上游地址、`apiKeyEnv`/`apiKeyFile` 引用，可能含本机路径 | 不入库；分享用 `bridge.config.example.json` |
| `.env`、`.jcode/.env`、`*.env` | 环境变量密钥 | 不入库 |
| `~/.jcode/.env`、`~/.commandcode/auth.json` | 用户级凭据 | 仓库外，不入库 |
| `**/auth.json`、`**/credentials*.json`、`*.key`、`*.pem`、`*.token` | 凭据兜底规则 | 不入库 |
| `dist/data/`、`data/` | SQLite 用量库、运行日志 | 不入库 |
| `.op/` | 变更记录，可能含本机绝对路径与源码片段 | 不入库 |
| `.sensitive-terms` | 本地敏感词表 | 不入库 |

原则：

- **不会被提交的敏感文件不需要脱敏**；但必须确认它确实未被跟踪，且忽略规则有效。
- 需要入库的示例只能使用占位符：`apiKeyEnv`、`apiKeyFile`、`example.com`，不写真实密钥、账号、Cookie、请求头。
- 日志只记录上游名、状态码、耗时与用量字段，不记录请求头与正文。
- 若敏感文件曾被提交：先 `git rm --cached` 并补忽略规则，再**轮换对应密钥**；仅删除文件不等于从历史移除。

核验命令（只读）：

```powershell
git ls-files | Select-String -Pattern 'bridge\.config\.json$|(^|/)\.env$|auth\.json$|credentials.*\.json$|\.(key|pem|token)$'
git check-ignore -v bridge.config.json dist/data/modelgate.db .op/index.json
python scripts/audit_secrets.py
bash scripts/scan-sensitive.sh
```

第一条应无输出；`git check-ignore` 应命中忽略规则；扫描脚本发现真实密钥时应立即处理。

## 五、版本与发布

- 当前产品版本唯一来源：`modelgate/__init__.py` 的 `__version__`。
- 桌面 exe：Windows 本机运行 `build.bat`（测试门禁 → PyInstaller → Inno Setup）。
- Legacy Node 版：版本在 `legacy/node-bridge/package.json`，发布 tag 形如 `legacy-v1.1.0`；不要用 `v2.x` tag 触发旧流水线。

## 六、改动前检查清单

1. `git status --short`：确认没有把 `dist/`、`build/`、`data/`、`.op/`、真实配置加入暂存区。
2. 新增/移动文件后同步更新 `README.md`、`docs/README.md` 或本文件中的路径。
3. 运行 `python -m pytest -q`；Node 相关改动运行 `node --check`。
4. 环境/配置异常先运行 `py -3 scripts/doctor.py all`（只读，不修改配置或数据库）。
5. 运行敏感扫描；确认 `bridge.config.example.json` 仍为零密钥模板。
6. 不覆盖正在运行的实例：换端口、独立库、按 PID 停旧实例。
