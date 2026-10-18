# -*- coding: utf-8 -*-
"""检查 dist/data/modelgate.db 的 settings.config 是否为测试残留配置。"""
from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CANDIDATES = [
    ROOT / "dist" / "data" / "modelgate.db",
    ROOT / "data" / "modelgate.db",
]


def main() -> int:
    for db_path in CANDIDATES:
        if not db_path.is_file():
            print(f"=== {db_path.relative_to(ROOT)} : 不存在 ===")
            continue
        print(f"=== {db_path.relative_to(ROOT)} ({db_path.stat().st_size} bytes) ===")
        conn = sqlite3.connect(str(db_path))
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            "SELECT key, length(value) AS n FROM settings").fetchall()
        settings = {r["key"]: r["n"] for r in rows}
        print(f"  settings keys: {settings}")
        row = conn.execute(
            "SELECT value FROM settings WHERE key='config'").fetchone()
        if not row:
            print("  settings.config: 无")
            conn.close()
            continue
        try:
            cfg = json.loads(row[0])
        except Exception as exc:
            print(f"  settings.config 解析失败: {exc}")
            conn.close()
            continue
        print(f"  settings.config: port={cfg.get('port')} "
              f"defaultModel={cfg.get('defaultModel')}")
        for name, up in (cfg.get("upstreams") or {}).items():
            base = up.get("baseUrl")
            key_env = up.get("apiKeyEnv")
            key_file = up.get("apiKeyFile")
            models = up.get("models")
            marker = ""
            if base and ("127.0.0.1:1" in base or base == "http://127.0.0.1:1"):
                marker = "  <== 测试残留桩地址"
            print(f"    {name}: baseUrl={base} apiKeyEnv={key_env} "
                  f"apiKeyFile={key_file} models={models}{marker}")
        cnt = conn.execute("SELECT COUNT(*) AS n FROM usage_records").fetchone()[0]
        print(f"  usage_records: {cnt} 条")
        conn.close()
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
