# Legacy Node 单文件桥（冻结）

这里是 Model-Gate 的原始 Node 实现：零依赖、单文件，仍可用于跨平台或最小化部署。
**功能已冻结**，只做必要的协议兼容修复；面板、SQLite 用量、上游故障切换、配置热重载等能力只在桌面版 `modelgate/` 中提供。

## 快速开始

```bat
cd legacy\node-bridge
node model-bridge.js
:: 或双击 start-bridge.cmd
```

默认监听 `127.0.0.1:8900`，配置读取同目录的 `bridge.config.json`。首次使用：

```bat
copy ..\..\bridge.config.example.json bridge.config.json
:: 按需填写 apiKeyEnv / apiKeyFile；不要把真实配置提交到仓库
```

完整的行为说明（上游、模型、接口、日志、密钥）见仓库根 [`README.md`](../../README.md)；本目录只保留与 Node 版相关的差异。

## 测试与探测

```bat
npm test                  :: 对着运行中的真实上游跑全链路
npm run test:commandcode  :: commandcode 协议离线回归（自带 stub，无外网）
npm run probe             :: 探测本地 cch 代理支持的协议与模型名
```

需要 Node.js 18+；无第三方依赖，不需要 `npm install`。

## 发布

本目录的版本在 `package.json`，历史发行说明在 `changelog/`。发布使用 `legacy-v<version>` tag
触发 `.github/workflows/release.yml`，产物为 `dist/model-bridge-v<version>.zip`。
**不要**用 `v2.x` tag 触发该流水线（那是桌面版版本号）。

## 目录约定

- 只在本目录增加 Node 相关文件；新增能力优先进桌面版。
- 不要把 `bridge.config.json`、`.env`、`auth.json` 或任何密钥写入本目录并提交。
- 一次性排查脚本不要放这里，放 `scripts/archive/`。
