# Model-Gate 仓库约定

本文件约束在本仓库工作的 agent 行为；与全局 `~/.dsh/AGENTS.md` 冲突时以更严格者为准。

## 1. 收尾与发布：只有一个入口

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ship.ps1 -MessageFile .tmp\msg.txt [-Tag vX.Y.Z]
```

- **禁止**手工拼 `git add` / `commit` / `push` / `tag` 流程；`--no-verify`、`--no-gpg-sign` 任何情况禁用。
- 提交信息先写成 UTF-8 文件放 `.tmp/`（不入库），首行格式：
  `<type>(<scope>): 【<领域名词>】<title> @AI G`；`【】` 只放业务名词，禁止动作词。
- 版本号唯一源是 `rust/model-gate/Cargo.toml`；`-Tag` 必须等于 `v<版本>`，由脚本校验，不一致拒绝发布。
- 门禁（噪音 / 格式 / BOM / 敏感信息 / 文本校验）失败即中止，不带病提交。

## 2. 发布不等人

- `ship.ps1` 推完 tag 就结束，**同一轮内不得等待流水线**：不许在前台跑 `ship-diagnose -Wait`，
  也不许手写轮询循环（2026-11-01 因此空耗 17 分钟）。
- 需要结果时：起**后台任务**跑 `ship-diagnose`，只在有结论时读一次输出；或直接等用户/GitHub 通知。
- 流水线失败只取一次结论（失败步骤 + `[FAIL]` 行），不做连续轮询、不重复拉日志。

## 3. 卫生

- 临时脚本、截图、调试产物一律写 `.tmp/`（已忽略）；不要往仓库丢 `*.b64.png`、`debug*`、`*.out/.err/.bak`。
- 设计稿只提交同名 `.png`；base64 内联图属于中间产物。
- 改 `.ps1`（含中文）后必须恢复 UTF-8 BOM，否则 PowerShell 5.1 按 GBK 解码报错；`ship.ps1` 会自动补并提示。

## 4. 发布链路保持简单

- 发布流水线只做必需动作；**不要为了提速引入新的外部依赖**（sccache 一类需要探测/回退的组件是
  发布失败的高发点，2026-11-01 为此连挂两轮）。
- 要提速优先用已有的 `Swatinem/rust-cache` 或并行 job，不引入运行时可能不可用的组件。
