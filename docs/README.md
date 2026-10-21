# 文档索引

本目录只放**长期有效**的说明、设计与维护资料；一次性排查记录、变更过程与验证证据放 `.op/`（不入库）。

| 路径 | 内容 | 何时看 |
|---|---|---|
| [`maintenance/project-layout.md`](maintenance/project-layout.md) | 目录职责、文件归属、清理节奏、敏感信息边界 | 新增文件、清理仓库、处理配置与日志前 |
| [`architecture/rust-desktop-shell.md`](architecture/rust-desktop-shell.md) | Rust 桌面壳、托盘、系统浏览器面板与进程边界 | 了解入口、代理、面板、SQLite 的分层 |
| [`architecture/provider-compat.md`](architecture/provider-compat.md) | Provider 兼容能力、预设与模型级覆盖 | 新增上游或排查协议差异时 |
| [`architecture/free-sources.md`](architecture/free-sources.md) | 免费来源（免密池）：判定规则、拒因字典、隐私红线、退避策略 | 接入或排查免密免费模型时 |
| [`architecture/upstream-config-model.md`](architecture/upstream-config-model.md) | 上游配置模型、诊断码与密钥来源契约 | 修改配置 schema、接入凭据时 |
| [`architecture/tech-stack-selection.md`](architecture/tech-stack-selection.md) | Rust-only 选型与迁移结论（历史背景） | 了解为何移除 Python/Node |
| [`integration/opencode-go-direct-integration.md`](integration/opencode-go-direct-integration.md) | opencode go 直连（不经代理）的实测结论与前置条件 | 配置直连或排查 400/403 时 |
| [`kilo-free-models-report.md`](kilo-free-models-report.md) | Kilo 免密池免费模型的完整实测报告（20 模型、限流、隐私条款） | 核对免费模型可用性、隐私红线与拒因时 |

## 维护约定

1. 产品使用说明以仓库根目录 [`README.md`](../README.md) 为准；本目录不复制整篇说明。
2. 当前产品版本唯一来源是 `rust/model-gate/Cargo.toml` 的 `version`；安装包、Release 资产与应用内更新均从它派生。
3. 新增文档按主题放入 `architecture/`、`integration/`、`maintenance/`；不确定时先放 `maintenance/`。
4. 文档中不要写入真实密钥、本机绝对路径、账号或请求头；示例统一用 `example.com` 与占位变量。
5. 一次性脚本与临时输出不要留在 `docs/`；输出放 `dist/` 或系统临时目录，长期脚本只保留在 `scripts/` 的 PowerShell 入口。
