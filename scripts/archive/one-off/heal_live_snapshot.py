# -*- coding: utf-8 -*-
"""把在线实例的 SQLite 快照修正为与 bridge.config.json 一致（先备份，不删任何数据）。

背景：在线跑的 v2.2 实例在启动时用 settings.config 整体覆盖文件，快照里残留的
测试哨兵 ``upstreams.cch.baseUrl=http://127.0.0.1:1`` 会把正确的文件值盖掉 —— 默认
上游因此指向死端口。v2.4 已改为「文件优先 + 快照自愈」，但**用户当前跑的是旧版**，
所以要主动把旧库的快照修正一次，保证下次重启也是对的。

安全：
* 先用 sqlite3 backup API 做一致性备份到 dist/data/backups/（WAL 下直接拷文件不可靠）。
* 只 UPDATE settings 里 key='config' 这一行，不删表、不删库、不动 usage_records。
* 在线实例正在用内存里的配置服务，本操作不重启、不占端口、不影响在途请求。
"""
from __future__ import annotations

import json
import shutil
import sqlite3
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

LIVE_DB = ROOT / "dist" / "data" / "modelgate.db"
CFG = ROOT / "dist" / "bridge.config.json"


def has_value(obj, target: str) -> bool:
    if isinstance(obj, dict):
        return any(has_value(v, target) for v in obj.values())
    if isinstance(obj, list):
        return any(has_value(v, target) for v in obj)
    return obj == target


def main() -> int:
    from modelgate.config import load_config

    if not LIVE_DB.is_file():
        print(f"SKIP 未找到 {LIVE_DB}")
        return 1

    backup_dir = LIVE_DB.parent / "backups"
    backup_dir.mkdir(exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    backup = backup_dir / f"modelgate-{stamp}.db"

    src = sqlite3.connect(f"file:{LIVE_DB.as_posix()}?mode=ro", uri=True)
    dst = sqlite3.connect(str(backup))
    with dst:
        src.backup(dst)
    dst.close()
    src.close()
    print(f"BACKUP {backup} ({backup.stat().st_size}B)")

    desired = load_config(CFG)
    print("文件里的默认上游：",
          next((u.get("baseUrl") for u in desired.get("upstreams", {}).values()
                if u.get("default")), None))

    con = sqlite3.connect(str(LIVE_DB), timeout=15)
    con.execute("PRAGMA busy_timeout=15000")
    before = con.execute("SELECT value FROM settings WHERE key='config'").fetchone()
    before_obj = json.loads(before[0]) if before else {}
    print("修正前快照含哨兵：", has_value(before_obj, "http://127.0.0.1:1"),
          "| maxTokens:", before_obj.get("maxTokens"))

    con.execute(
        "INSERT INTO settings (key, value) VALUES ('config', ?) "
        "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (json.dumps(desired, ensure_ascii=False),),
    )
    con.commit()
    after = json.loads(con.execute("SELECT value FROM settings WHERE key='config'")
                       .fetchone()[0])
    con.close()

    ok = (not has_value(after, "http://127.0.0.1:1")
          and after.get("upstreams", {}).get("cch", {}).get("baseUrl")
          == desired["upstreams"]["cch"]["baseUrl"])
    print("修正后快照含哨兵：", has_value(after, "http://127.0.0.1:1"),
          "| cch.baseUrl:", after.get("upstreams", {}).get("cch", {}).get("baseUrl"))
    print("LIVE_SNAPSHOT_HEALED OK" if ok else "LIVE_SNAPSHOT_HEALED FAILED")

    # 在线实例仍在服务（未被本次操作打断）
    import urllib.request
    try:
        health = json.loads(urllib.request.urlopen(
            "http://127.0.0.1:8900/health", timeout=8).read().decode("utf-8"))
        print("在线实例默认上游：", health.get("upstream"),
              "| 状态：", health.get("status"))
    except Exception as exc:
        print("在线实例健康检查失败：", exc)
        return 1
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
