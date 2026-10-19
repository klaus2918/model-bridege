# -*- coding: utf-8 -*-
"""v2.17 文档追加：最大输出 token 自填 + 配置页空间压缩。"""
from pathlib import Path

DOC = Path(".op/changes/model-gate-desktop/design-v25.md")
PLAN = Path(".op/changes/model-gate-desktop/plan.md")

SEC = """

## 二十五、最大输出 token 自填 + 配置页空间压缩（v2.17）

**诉求**：① 最大输出 token 要能自己填；② 不要浪费页面空间。

**① 自填**
- 输入框本就是 `type="text" inputmode="numeric"`，但标签写着「最大输出 token」、候选列表又摆在最显眼处，看起来像只能从 8192/64K/128K/384K 里挑。改为「**最大输出 token（可自填）**」+ 占位符 `384K / 30万 / 300000`，候选列表降级为建议。
- 接受任意正整数，也接受 `300000` / `3,000,000` / `30万` / `1.5亿` / `384K` / `1M`（K、M 按 1024）；留空按 8192。**换算在前端 `parseCount()` 完成**，提交的永远是数字——后端不猜字符串（带单位的字符串依旧 400，行为有测试锁定）。
- 折叠标题行的「上限」胶囊改成显示可读数值（`fmtInt`），悬停给精确值，**点击直接聚焦并全选输入框**。

**② 空间**
| 位置 | 改前 | 改后 |
|---|---|---|
| 服务折叠区顶部 | 独立一行胶囊条（面板/端口/默认模型/上限/超时 + 三颗按钮） | 胶囊条与按钮**并入折叠标题行**（`summary` 内右对齐），省掉整整一行 |
| 「服务」小节标题 | 额外一个 `section-head`（标题 + 说明句） | 删除——折叠标题已说明用途；说明移到 `summary.title` |
| 全局字段 | 两个 textarea 各占整行（一列堆叠）+ 两个 checkbox 再各占一行 | 两个 textarea 走 `.grid-2` **左右并排**；两个 checkbox 也并排 |
| 服务字段网格 | `minmax(168px,1fr)`、`gap 8px 12px` | `minmax(148px,1fr)`、`gap 7px 10px`，7 个字段更容易收进一行 |
| 胶囊与输入 | 胶囊 `padding 2px 9px / 11.5px`、输入 `padding 5px 8px` | 胶囊 `1px 8px / 11px`，字段说明 10.5px 行高 1.45 |

标题行内的按钮统一 `stopPropagation`（`inSummary(ev)`），点「复制面板/打开/恢复默认/上限胶囊」不会顺带折叠卡片。

**验证**
- `tests/test_ui_assets.py` **22 passed**：新增断言——`maxTokens` 必须是 `type=text` 且不含 `readonly`/`pattern=`/`step=`/`disabled`、界面写明「自填」、胶囊条与按钮必须在 `summary` 内、`inSummary(ev)` 与 `maxChip` 必须存在、全局必须走 `grid-2`
- `scripts/verify_v217.py` **V217_VERIFY OK 15/15**（隔离实例）：自填 300000 / 393216 / 1048576 / 1 都能保存并原样回读；带单位字符串仍 400；布局标记全在；`dist\\data` 未被改写
- `scripts/verify_v217_exe.py` **V217_EXE_VERIFY OK 17/17**（黑盒跑 `dist\\Model-Gate-v2.17.exe`）：冻结进包的 app.js / index.html 确实带这些改动（防「源码改了没打进包」）

**上线**：8900 由 v2.16 → v2.17，停机 **2.77s**（按 PID 停旧实例）；在线核验：面板资产戳 `app.js?v=2.17.0`、标题行胶囊/两列布局/自填标记全在、`/v1/models` 200、`/v1/chat/completions` 200（usage 37/17）、`/go/v1/chat/completions` 200（usage 32/16）。
"""

ADD = """

### 验证补充（v2.17，自填 + 空间）

| 层 | 证据 | 结论 |
|---|---|---|
| 源码守卫 | `tests/test_ui_assets.py` 22 passed | 自填约束 + 标题行收纳 + 两列布局的结构断言 |
| 隔离实例 | `scripts/verify_v217.py` 15/15 | 自填 300000/393216/1048576/1 保存并回读；带单位字符串仍 400 |
| 打包产物 | `scripts/verify_v217_exe.py` 17/17 | 冻结资产含全部改动；现场保护通过 |
| 上线 | 8900 = v2.17.0，停机 2.77s | `/v1/*` 与 `/go/v1/*` 均 200；布局标记在线可见 |
"""

for path, text in ((DOC, SEC), (PLAN, ADD)):
    body = path.read_text(encoding="utf-8")
    marker = text.strip().splitlines()[0]
    if marker in body:
        print(f"skip (already present): {path}")
        continue
    path.write_text(body + text, encoding="utf-8")
    print(f"appended -> {path} ({path.stat().st_size} bytes)")
