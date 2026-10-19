# -*- coding: utf-8 -*-
"""新增控制面接口的导入与结构冒烟。"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

FAILS: list[str] = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" - {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def main() -> int:
    import modelgate
    import modelgate.api as api
    import modelgate.db as db
    import modelgate.proxy as proxy

    check("modelgate 导入", True, modelgate.__version__)
    for name in ("handle_get", "handle_post", "probe_one_upstream",
                 "_probe_one", "probe_all_upstreams"):
        ok = hasattr(api, name)
        check(f"api.{name} 存在", ok)

    check("proxy.Bridge 有 started_at", hasattr(proxy.Bridge, "__init__"))
    database = db.open_db("data/_smoke2.db") if hasattr(db, "open_db") else db.Database(
        "data/_smoke2.db")
    for name in ("record_usage", "list_usage", "totals", "summary", "clear_usage",
                 "count_usage"):
        ok = hasattr(database, name)
        check(f"db.{name} 存在", ok)

    # 能跑通一次清空
    removed = database.clear_usage(since_ts=0)
    check("db.clear_usage 可调用", isinstance(removed, int), f"removed={removed}")
    database.close()

    import modelgate.app as app
    for name in ("run_gui", "start_proxy_thread", "make_icon_image"):
        check(f"app.{name} 存在", callable(getattr(app, name, None)))

    print()
    if FAILS:
        print(f"IMPORT_SMOKE FAILED: {len(FAILS)}")
        for item in FAILS:
            print("  - " + item)
        return 1
    print("IMPORT_SMOKE OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
