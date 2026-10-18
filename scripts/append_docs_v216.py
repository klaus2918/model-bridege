# -*- coding: utf-8 -*-
"""把 v2.16 的根因/修法/验证写进变更文档（design-v25.md 新一节 + plan.md 验证段）。"""
from pathlib import Path

DOC = Path(".op/changes/model-gate-desktop/design-v25.md")
PLAN = Path(".op/changes/model-gate-desktop/plan.md")

SEC = """

## 二十四、保存配置「有 1 处错误」的根因与修法（v2.16）

**现象**：v2.15 上线后点保存，面板只提示「保存有 1 处错误，未提交」，不点名是哪一处，用户无从下手。

**根因**：唯一能单条触发的硬错误是上游 Base URL 缺协议——`validateConfig` 用 `/^https?:\\/\\//` 判定，而用户习惯只写 `api.deepseek.com/v1`。旧文案只说数量、不说原因与当前值。

**修法（三层，缺一层都会复发）**

1. **采集层**：`normalizeBaseUrl()` 在 `collectConfig()` 里先补协议——外网补 `https://`、本机（`127.` / `localhost` / `::1` / `0.0.0.0` / `192.168.` / `10.` / `172.16-31`）补 `http://`，并剥离尾部 `/v1`；补过的项记入 `state.autoFixNotes`，保存后以「已自动修正」回显。
2. **校验层**：文案点名第一条错误（`errors[0].message`）并带当前值；`fix` 字段给一键修复动作（`fix-baseurl` / `focus-upstream` / `add-upstream`）。
3. **后端兜底**：`normalize_upstream()` 同样补全 baseUrl——绕过面板直调 `POST /api/config` 也不会 400；**磁盘上的 bridge.config.json 保留用户原文**（不偷改用户契约），规范化只发生在运行期。

**边界（没放宽过头）**：端口越界、监听地址为空、上游清空、Base URL 留空、retentionDays / modelsCacheSeconds 为负，仍是硬错误拦保存；只有「填了值但格式怪」降级为提醒，并写明采用了什么默认值。

**验证**

- `tests/test_ui_assets.py` **22 passed**（含 node 实跑 `normalizeBaseUrl` 9 例 + 错误文案带值）
- `scripts/verify_v216.py` **V216_VERIFY OK 14/14**（隔离实例：scheme-less 保存 200、运行期补 https、剥离 /v1、`dist\\data` 未被改写、在线 8900 未受影响）
- `scripts/verify_v216_exe.py` **V216_EXE_VERIFY OK 15/15**（黑盒跑 `dist\\Model-Gate-v2.16.exe`：冻结资产含 `normalizeBaseUrl`、缺协议可保存、现场保护断言）
- `scripts/live_config_validate.py`（新增，只读）：从在线实例取**冻结的** app.js 在 node 里跑 `validateConfig`——现状配置 `error=0 warn=1`（zen 无模型清单）；scheme-less 复现 1 条 error，归一化后 error=0

**上线**：8900 由 v2.15 → v2.16，停机 **2.6s**（按 PID 停旧实例，不按进程名）；在线 E2E：`/v1/models` 200、`/v1/chat/completions` 200、`/go/v1/chat/completions` 200（usage: prompt 33 / completion 16 / reasoning 16）；用量库累计 2472 请求、缓存命中 622.6M / 748.1M ≈ 83%。
"""

ADD = """

### 验证补充（v2.16，保存链路修复）

| 层 | 证据 | 结论 |
|---|---|---|
| 源码守卫 | `tests/test_ui_assets.py` 22 passed | `normalizeBaseUrl` 9 例 + 错误文案带值 |
| 隔离实例（源码） | `scripts/verify_v216.py` 14/14 | scheme-less 保存 200；运行期补 https；剥离 /v1；`dist\\data` 未变 |
| 打包产物 | `scripts/verify_v216_exe.py` 15/15 | 冻结 app.js 含修复；缺协议可保存；现场保护 |
| 在线只读复核 | `scripts/live_config_validate.py` | 现状 error=0；scheme-less 复现 1 error → 归一化后 0 |
| 上线 | 8900 = v2.16.0，停机 2.6s | `/v1/*` 与 `/go/v1/*` 均 200 |
"""

for path, text in ((DOC, SEC), (PLAN, ADD)):
    body = path.read_text(encoding="utf-8")
    if text.strip().splitlines()[0] in body:
        print(f"skip (already present): {path}")
        continue
    path.write_text(body + text, encoding="utf-8")
    print(f"appended -> {path} ({path.stat().st_size} bytes)")
