# -*- coding: utf-8 -*-
"""检查 8900 占用 + dist 日志尾部，定位 GUI 启动失败原因。"""
from __future__ import annotations

import socket
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def owner_of(port: int) -> str:
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "Get-NetTCPConnection -LocalPort %d -State Listen | "
         "ForEach-Object { $_.OwningProcess }" % port],
        capture_output=True, text=True, timeout=30)
    pids = [l.strip() for l in out.stdout.split() if l.strip().isdigit()]
    if not pids:
        return "无监听"
    names = []
    for pid in pids[:3]:
        r = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "(Get-Process -Id %s).ProcessName" % pid],
            capture_output=True, text=True, timeout=30)
        names.append(f"{pid}:{r.stdout.strip() or '?'}")
    return ", ".join(names)


def main() -> int:
    for port in (8900, 8911):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.8)
            listening = s.connect_ex(("127.0.0.1", port)) == 0
        print(f"port {port}: {'监听中' if listening else '空闲'}  owner={owner_of(port)}")

    log = ROOT / "dist" / "data" / "modelgate.log"
    if log.is_file():
        print(f"\n--- {log.relative_to(ROOT)} 尾部 ---")
        lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
        for line in lines[-8:]:
            print("  " + line)
    else:
        print("\ndist log 不存在")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
