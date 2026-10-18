# -*- coding: utf-8 -*-
"""检查配置文件是否可被安全加载（含损坏恢复能力）。"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from modelgate.config import (  # noqa: E402
    load_config, upstreams_of, validate_config_shape,
)

ROOT = Path(__file__).resolve().parent.parent
CANDIDATES = [
    ROOT / "bridge.config.json",
    ROOT / "dist" / "bridge.config.json",
]


def main() -> int:
    bad = 0
    for path in CANDIDATES:
        rel = path.relative_to(ROOT)
        if not path.is_file():
            print(f"[--  ] {rel}: 不存在（跳过）")
            continue
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001
            print(f"[FAIL] {rel}: JSON 解析失败 {exc}")
            bad += 1
            continue
        problem = validate_config_shape(raw)
        if problem:
            print(f"[WARN] {rel}: 结构问题 {problem}")
        try:
            cfg = load_config(path)
            ups = upstreams_of(cfg)
        except Exception as exc:  # noqa: BLE001
            print(f"[FAIL] {rel}: 加载失败 {type(exc).__name__}: {exc}")
            bad += 1
            continue
        names = [u["name"] for u in ups]
        print(f"[{'OK  ' if not problem else 'WARN'}] {rel}: "
              f"port={cfg['port']} models={len(cfg['models'])} "
              f"upstreams={names}")
        if problem:
            bad += 1
    print()
    if bad:
        print(f"CONFIG_CHECK FAILED: {bad}")
        return 1
    print("CONFIG_CHECK OK")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
