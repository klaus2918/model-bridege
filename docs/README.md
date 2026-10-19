# 文档索引

本目录只放**长期有效**的说明、设计与维护资料；一次性排查记录、变更过程与验证证据放 `.op/`（不入库）。

| 路径 | 内容 | 何时看 |
|---|---|---|
| [`maintenance/project-layout.md`](maintenance/project-layout.md) | 目录职责、文件归属、清理节奏、敏感信息边界 | 新增文件、清理仓库、处理配置与日志前 |
| [`architecture/model-gate-exe-architecture.html`](architecture/model-gate-exe-architecture.html) | exe 架构图（可直接用浏览器打开） | 了解入口、代理、面板、SQLite 的分层 |
| [`architecture/model-gate-exe-architecture.png`](architecture/model-gate-exe-architecture.png) | 架构图静态版 | 文档/分享引用 |
| [`integration/opencode-go-direct-integration.md`](integration/opencode-go-direct-integration.md) | opencode go 直连（不经代理）的实测结论与前置条件 | 配置直连或排查 400/403 时 |
| [`live-panel/README.md`](live-panel/README.md) | 动态架构图素材与重新生成方法 | 更新演示素材时 |

## 维护约定

1. 产品使用说明以仓库根目录 [`README.md`](../README.md) 为准；本目录不复制整篇说明。
2. 当前产品（桌面版）版本唯一来源是 `modelgate/__init__.py` 的 `__version__`；冻结的 Legacy Node 包另用 `legacy/node-bridge/package.json`，只通过 `legacy-v*` tag 发布，两者不要混用。
3. 新增文档按主题放入 `architecture/`、`integration/`、`maintenance/`、`live-panel/`；不确定时先放 `maintenance/`。
4. 文档中不要写入真实密钥、本机绝对路径、账号或请求头；示例统一用 `example.com` 与占位变量。
5. 一次性脚本与临时输出不要留在 `docs/`；脚本放 `scripts/archive/`，输出放 `dist/` 或系统临时目录。
