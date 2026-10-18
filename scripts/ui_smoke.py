# -*- coding: utf-8 -*-
"""UI 静态资源与结构冒烟：JS 引用的接口/CSS 类名必须都存在。

防止改版后出现「按钮还在但 JS 已不引用」「JS 调用了不存在的 CSS 类」这类断裂。
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WEB = ROOT / "modelgate" / "web"
FAILS: list[str] = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" - {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def main() -> int:
    html = (WEB / "index.html").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")

    # 1) HTML 里的 id 必须都被 JS 引用（反之，JS 引用的 id 必须存在于 HTML）
    html_ids = set(re.findall(r'id="([^"]+)"', html))
    js_ids = set(re.findall(r'\$\("#([A-Za-z0-9_-]+)"\)', js))
    js_ids |= set(re.findall(r"getElementById\(['\"]([^'\"]+)['\"]\)", js))
    missing_in_html = sorted(i for i in js_ids if i not in html_ids)
    check("JS 引用的 id 全部存在于 HTML", not missing_in_html,
          f"缺失: {missing_in_html}" if missing_in_html else f"共 {len(js_ids)} 个")

    # 2) JS 调用的 REST 接口必须登记在 api.py
    api_py = (ROOT / "modelgate" / "api.py").read_text(encoding="utf-8")
    js_paths = sorted(set(re.findall(r'api\("([^"]+)"', js))
                       | set(re.findall(r'postJson\("([^"]+)"', js))
                       | set(re.findall(r'fetch\(["\'`]([^"\'`]+)["\'`]', js)))
    missing_routes = [p for p in js_paths
                      if p.split("?")[0] not in api_py]
    check("JS 调用的接口都在 api.py 注册", not missing_routes,
          f"缺失: {missing_routes}" if missing_routes else f"共 {len(js_paths)} 个")

    # 3) 页签声明与面板声明一致
    tabs = set(re.findall(r'data-tab="([^"]+)"', html))
    panels = set(re.findall(r'id="panel-([a-z]+)"', html))
    check("页签与面板一一对应", tabs == panels,
          f"tabs={sorted(tabs)} panels={sorted(panels)}")

    # 4) JS 里用到的关键 CSS 类都在样式表里
    key_classes = ["kpi-row", "kpi", "bar-stack", "bar-seg", "bar-tip",
                   "legend-row", "legend-head", "log-viewer", "chart",
                   "split-2", "box", "callout", "seg", "filter-select",
                   "up-card", "sticky-actions", "bar-fill", "bar-track"]
    missing_css = sorted(c for c in key_classes if f".{c}" not in css)
    check("关键 CSS 类均已定义", not missing_css,
          f"缺失: {missing_css}" if missing_css else f"共 {len(key_classes)} 个")

    # 5) HTML 里的 id 至少有一个被 JS 使用（防死代码面板）
    used = html_ids & js_ids
    check("HTML id 被 JS 引用", len(used) >= 10, f"{len(used)} 个被引用")

    # 6) 不再有旧容器引用（防止改版后遗留）
    stale = [i for i in ("today-cards",) if f'id="{i}"' in html and i not in js_ids]
    check("无孤立旧容器", not stale, f"孤立: {stale}")

    print()
    if FAILS:
        print(f"UI_SMOKE FAILED: {len(FAILS)}")
        for item in FAILS:
            print("  - " + item)
        return 1
    print("UI_SMOKE OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
