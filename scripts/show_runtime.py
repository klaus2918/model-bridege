# -*- coding: utf-8 -*-
"""打印 exe 的运行日志与数据库记录（诊断用）。"""
from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

ROOT = Path(__file__).resolve().parent.parent


def show_log(path: Path, tail: int = 60) -> None:
    print(f"=== {path.relative_to(ROOT)} "
          f"({path.stat().st_size} bytes) ===")
    if not path.is_file():
        print("  (missing)")
        return
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    for line in lines[-tail:]:
        print("  " + line)
    if len(lines) > tail:
        print(f"  ... (共 {len(lines)} 行)")


def show_db(path: Path) -> None:
    print(f"\n=== {path.relative_to(ROOT)} "
          f"({path.stat().st_size} bytes) ===")
    if not path.is_file():
        print("  (missing)")
        return
    conn = sqlite3.connect(str(path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT ts, created_at, upstream, protocol, requested_model, "
            "prompt_tokens, completion_tokens, duration_ms, status, stream, "
            "error FROM usage_records ORDER BY ts DESC LIMIT 20"
        ).fetchall()
        total = conn.execute("SELECT COUNT(*) AS n FROM usage_records").fetchone()[0]
        print(f"  usage_records 总数 = {total}")
        for r in rows:
            print("  " + json.dumps(dict(r), ensure_ascii=False))
        cfg = conn.execute("SELECT value FROM settings WHERE key='config'").fetchone()
        print(f"  settings.config = {'有' if cfg else '无'}")
    finally:
        conn.close()


def main() -> int:
    dist_log = ROOT / "dist" / "data" / "modelgate.log"
    dist_db = ROOT / "dist" / "data" / "modelgate.db"
    if dist_log.is_file():
        show_log(dist_log, 80)
    else:
        print("(dist log missing)")
    if dist_db.is_file():
        show_db(dist_db)
    print("\n=== dist 配置端口与上游 ===")
    cfg_path = ROOT / "dist" / "bridge.config.json"
    if cfg_path.is_file():
        cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
        print(f"  port={cfg.get('port')} host={cfg.get('host')} "
              f"accessLog={cfg.get('accessLog')}")
        for name, up in (cfg.get("upstreams") or {}).items():
            print(f"  {name}: {up.get('protocol')} {up.get('baseUrl')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
