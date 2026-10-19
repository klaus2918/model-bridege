# -*- coding: utf-8 -*-
"""追加新组件样式到 style.css（幂等：已存在则跳过）。"""
from __future__ import annotations

import sys
from pathlib import Path

CSS = Path(__file__).resolve().parent.parent / "modelgate" / "web" / "style.css"

BLOCK = """
/* ══ appended: 2.1 新组件 ══════════════════════════════ */
.section-head {
  display: flex; align-items: center; justify-content: space-between;
  gap: 12px; margin: 26px 0 10px; flex-wrap: wrap;
}
.section-head h2 { margin: 0; }
.section-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }

.kpi-row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px; margin-bottom: 4px;
}
.kpi {
  background: var(--surface); border: 1px solid var(--border);
  border-radius: var(--radius); padding: 14px 16px;
  box-shadow: var(--shadow); position: relative; overflow: hidden;
}
.kpi::after { content: ""; position: absolute; inset: 0 0 auto 0; height: 2px; background: var(--border); }
.kpi.accent::after { background: var(--brand); }
.kpi.warn::after { background: var(--warn); }
.kpi.danger::after { background: var(--danger); }
.kpi.ok::after { background: var(--ok); }

.callouts { display: grid; gap: 10px; }
.callout {
  border: 1px solid var(--border); border-left: 3px solid var(--brand);
  border-radius: 8px; background: var(--surface-2); padding: 10px 14px; font-size: 13px;
}
.callout .t { font-weight: 600; margin-bottom: 4px; }
.callout.warn { border-left-color: var(--warn); }
.callout.danger { border-left-color: var(--danger); }
.mono-block {
  font-family: var(--mono); font-size: 12.5px; background: var(--surface-2);
  border: 1px solid var(--border); border-radius: 6px; padding: 6px 8px;
  display: block; margin-top: 6px; white-space: pre-wrap; word-break: break-all;
}

.box {
  background: var(--surface); border: 1px solid var(--border);
  border-radius: var(--radius); padding: 14px 16px; box-shadow: var(--shadow);
}
.box + .box, .split-2 + .box { margin-top: 18px; }
.box-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.box-head h2 { margin: 0; }
.split-2 { display: grid; grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); gap: 18px; }
.box .table-wrap { margin-top: 10px; }

.seg.small { border-radius: 7px; }
.seg.small button { padding: 4px 10px; font-size: 12px; }

.filter-select, .filter-input {
  background: var(--surface-2); border: 1px solid var(--border);
  border-radius: 8px; padding: 7px 10px; color: var(--text); font-size: 13px; font-family: inherit;
}
.filter-input { min-width: 200px; }

.chart {
  min-height: 190px; display: flex; align-items: flex-end;
  gap: 6px; padding: 14px 2px 0; overflow-x: auto;
}
.chart.legend { flex-direction: column; align-items: stretch; padding-top: 12px; overflow: visible; }
.col-bar {
  flex: 1 1 0; min-width: 26px; max-width: 68px; height: 100%;
  display: flex; flex-direction: column; justify-content: flex-end;
  align-items: center; position: relative;
}
.bar-stack {
  width: 100%; border-radius: 5px 5px 0 0; overflow: hidden;
  background: var(--surface-2); display: flex; flex-direction: column-reverse;
}
.bar-seg { width: 100%; }
.bar-in { background: #6366f1; }
.bar-out { background: #f59e0b; }
.bar-err { background: var(--danger); }
.bar-req { background: var(--brand); }
.col-bar .label { font-size: 10.5px; color: var(--text-3); margin-top: 6px; white-space: nowrap; }
.col-bar:hover .bar-tip, .col-bar:focus .bar-tip { opacity: 1; visibility: visible; }
.bar-tip {
  position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translateX(-50%);
  background: var(--text); color: var(--bg); padding: 5px 9px; border-radius: 6px;
  font-size: 11.5px; white-space: nowrap; opacity: 0; visibility: hidden;
  transition: opacity .12s; z-index: 5; pointer-events: none;
}
.legend-row { margin-bottom: 10px; }
.legend-row:last-child { margin-bottom: 0; }
.legend-head { display: flex; justify-content: space-between; gap: 12px; font-size: 12.5px; margin-bottom: 5px; }
.legend-head .label { color: var(--text-2); font-family: var(--mono); font-size: 12px; }
.legend-head .val { color: var(--text-3); font-variant-numeric: tabular-nums; }
.chart-legend { display: flex; gap: 14px; flex-wrap: wrap; margin-top: 10px; font-size: 12px; color: var(--text-2); }
.chart-legend .key { display: inline-flex; align-items: center; gap: 6px; }
.swatch { width: 10px; height: 10px; border-radius: 3px; display: inline-block; }
.swatch.in { background: #6366f1; }
.swatch.out { background: #f59e0b; }
.swatch.err { background: var(--danger); }

.log-viewer {
  margin: 0; padding: 12px; max-height: 430px; overflow: auto;
  background: var(--surface-2); border: 1px solid var(--border); border-radius: 8px;
  font-family: var(--mono); font-size: 12px; line-height: 1.65;
  white-space: pre-wrap; word-break: break-word; color: var(--text);
}
.log-viewer .ln { display: block; padding: 1px 2px; border-radius: 3px; }
.log-viewer .ln:hover { background: var(--surface); }
.log-viewer .ln.err { color: var(--danger); }
.log-viewer .ln.warn { color: var(--warn); }
.log-viewer .ln .no { color: var(--text-3); margin-right: 10px; user-select: none; }

td .row { justify-content: flex-start; }
.up-name { font-weight: 600; font-family: var(--mono); font-size: 12.5px; }
.up-sub { color: var(--text-3); font-size: 11.5px; }
th.sortable { cursor: pointer; user-select: none; }
th.sortable:hover { color: var(--text); }
.sticky-actions { position: sticky; bottom: 0; background: var(--surface); padding: 12px 0 4px; z-index: 5; }

@media (max-width: 720px) {
  .split-2 { grid-template-columns: 1fr; }
  .chart { min-height: 140px; }
  .log-viewer { max-height: 300px; }
}
"""


def main() -> int:
    text = CSS.read_text(encoding="utf-8")
    if "kpi-row" in text and ".log-viewer" in text and ".bar-stack" in text:
        print("CSS_ALREADY_APPENDED")
        return 0
    if not text.endswith("\n"):
        text += "\n"
    CSS.write_text(text + BLOCK, encoding="utf-8")
    print(f"CSS_APPENDED +{len(BLOCK)} chars, total {len(CSS.read_text(encoding='utf-8'))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
