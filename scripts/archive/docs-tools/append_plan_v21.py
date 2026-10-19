# -*- coding: utf-8 -*-
"""向 plan.md 追加 v2.1 改版记录（幂等）。"""
from __future__ import annotations

from pathlib import Path

PLAN = Path(__file__).resolve().parent.parent / ".op" / "changes" / "model-gate-desktop" / "plan.md"

BLOCK = """

## 面板 v2.1 改版（界面交互 / 统计展示 / 代理等价）

### 新增后端能力

| 接口 | 用途 |
|------|------|
| `GET  /api/logs` | 日志查看（支持 `lines` / `level=error\|warn\|all` / `q=` 关键字）——exe 无控制台，这是面板排障主入口 |
| `GET  /api/proxy/status` | 运行态快照：端点、上游/模型数、默认上游、运行时长、日志开关、thinking 缓存、库与配置路径 |
| `POST /api/upstreams/probe` | 单条上游即时测试（配置页改完 URL/密钥即可验证） |
| `POST /api/usage/clear` | 按区间或全量清空用量明细 |
| `POST /api/logs/clear` | 清空运行日志 |
| `bridge.started_at` | 支撑「运行时长」展示 |

### 前端信息架构（4 页）

- **概览**：KPI 行（端口/上游数/模型数/端点/运行时长/日志状态）+ 上游表（**内联健康徽章 + 一键重测**）+「代理接入信息」说明块（OpenAI 入口、前缀路由、健康检查、本地数据位置，可一键复制端点）
- **配置**：上游卡片内直接提示「未配置密钥 → 上游会 401」；保存操作粘性悬浮
- **用量**：7 张 KPI 卡（含错误率、平均耗时）+ **趋势图**（按天/小时/模型/上游 × 总 token/请求数/错误，堆叠柱 + hover 提示）+ **占比分布**（按模型/上游，带百分比与错误数）+ 明细表（上游/模型筛选下拉 + 分页）+ 清空按钮
- **日志**：等级分段（全部/错误/注意）+ 关键字搜索 + 自动刷新开关 + 错误/警告行着色 + 行号 + 清空

### 对「完全取代原有代理中转」的补齐

- 日志读取、单上游测试、运行态查询此前缺失，现均经面板可用
- 上游健康状态直接出现在概览表与配置页，401/余额/baseUrl 类问题可当场定位
- 面板 UI 结构纳入回归：`tests/test_ui_assets.py` 校验 JS 引用的 id、调用的接口、页签与面板对应、关键 CSS 类，防止改版断裂

### 验收

- `py -3.11 -m pytest tests -q` → **64 passed**
- `scripts\\ui_smoke.py` → `UI_SMOKE OK`（6 项）
- `scripts\\import_smoke.py` → `IMPORT_SMOKE OK`（17 项）
- `scripts\\real_e2e.py`（桩上游→exe→客户端）→ `REAL E2E OK`（30/30）
- `scripts\\probe_exe.ps1` → `EXE PROBE ACCEPTED` / `NO_5XX`
- `scripts\\gui_probe.ps1` → `GUI PROBE OK (window + server)`
"""


def main() -> int:
    text = PLAN.read_text(encoding="utf-8")
    if "面板 v2.1 改版" in text:
        print("PLAN_ALREADY_UPDATED")
        return 0
    if not text.endswith("\n"):
        text += "\n"
    PLAN.write_text(text + BLOCK, encoding="utf-8")
    print(f"PLAN_APPENDED +{len(BLOCK)} chars, total {len(PLAN.read_text(encoding='utf-8'))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
