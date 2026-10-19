# -*- coding: utf-8 -*-
"""v2.18 文档追加：去掉最大输出 token 旁的大段说明。"""
from pathlib import Path

DOC = Path(".op/changes/model-gate-desktop/design-v25.md")
PLAN = Path(".op/changes/model-gate-desktop/plan.md")

SEC = """

## 二十六、字段旁不再堆说明：最大输出 token 只留一行（v2.18）

**诉求**：最大输出 token 旁边那一大段说明占地方，删掉。

**做法**
- 删掉 `maxTokens` 标签内的 `<small class="field-hint">…</small>`（占一整行、11px 灰字），字段回到**一行**：标签 + 输入框。
- 信息不丢，全部并入该字段的 `title` 悬停提示：可自填 `300000 / 30万 / 384K / 1M`（K、M 按 1024）、留空按 8192、DeepSeek 官方上限 384K（393,216）超出会被拒。
- 候选列表（`datalist`）保留但只在聚焦时出现，不再常驻占位。

**验证**
- `tests/test_ui_assets.py` **22 passed**：新增断言——`maxTokens` 字段块内**不得**再出现 `field-hint`
- `scripts/verify_v217_exe.py` **V217_EXE_VERIFY OK 18/18**（黑盒跑 `dist\\Model-Gate-v2.18.exe`）：冻结进包的 index.html 里该字段块无 `field-hint`，其余布局断言与保存链路照旧通过
- `scripts/verify_v217.py` **15/15**（隔离实例）：自填 300000 / 393216 / 1048576 / 1 仍能保存并回读

**上线**：8900 由 v2.17 → v2.18，停机 **3.59s**；在线核验：资产戳 `app.js?v=2.18.0`、`maxTokens` 字段块内无 `field-hint`、`/v1/models` 200、`/v1/chat/completions` 200（usage 31/16）。
"""

ADD = """

### 验证补充（v2.18，去掉字段旁赘述）

| 层 | 证据 | 结论 |
|---|---|---|
| 源码守卫 | `tests/test_ui_assets.py` 22 passed | `maxTokens` 字段块内不得出现 `field-hint` |
| 隔离实例 | `scripts/verify_v217.py` 15/15 | 自填值仍可保存回读 |
| 打包产物 | `scripts/verify_v217_exe.py` 18/18 | 冻结 index.html 无赘述；链路与现场保护通过 |
| 上线 | 8900 = v2.18.0，停机 3.59s | 字段回到一行；`/v1/*` 200 |
"""

for path, text in ((DOC, SEC), (PLAN, ADD)):
    body = path.read_text(encoding="utf-8")
    marker = text.strip().splitlines()[0]
    if marker in body:
        print(f"skip (already present): {path}")
        continue
    path.write_text(body + text, encoding="utf-8")
    print(f"appended -> {path} ({path.stat().st_size} bytes)")
