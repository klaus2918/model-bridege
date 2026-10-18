# -*- coding: utf-8 -*-
"""端口漂移回归（实机）：配置端口空闲时，必须**原样使用**该端口。

背景：修复前的实现把「可用性自检」放在绑定之后，导致本进程自己的监听
被误判为「端口被占用」，于是空闲端口也被 +1 漂移，用户按配置的端口接入
就会连不上。这里用真实 exe 钉住修复结果。
"""
from __future__ import annotations

import json
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
EXE = DIST / "Model-Gate.exe"

CHECKS: list[tuple[str, bool, str]] = []


def check(name, ok, detail=""):
    CHECKS.append((name, bool(ok), detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def free_port(start: int) -> int:
    for candidate in range(start, start + 50):
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            sock.bind(("127.0.0.1", candidate))
            return candidate
        except OSError:
            continue
        finally:
            sock.close()
    raise RuntimeError("找不到空闲端口")


def health(port: int, timeout: float = 1.5):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except Exception:  # noqa: BLE001
        return -1, ""


def main() -> int:
    port = free_port(8930)
    print(f"挑一个空闲端口：{port}")
    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(port)],
        cwd=str(DIST), stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    try:
        status = -1
        for _ in range(60):
            if proc.poll() is not None:
                break
            status, body = health(port)
            if status == 200:
                break
            time.sleep(0.5)
        check(f"空闲端口 {port} 原样可用（无 +1 漂移）", status == 200,
              f"HTTP {status}")

        drifted_status, _ = health(port + 1)
        check("未意外占用 port+1", drifted_status == -1, f"HTTP {drifted_status}")

        if status == 200:
            _s, body = health(port)
            try:
                data = json.loads(body)
                names = [u.get("name") for u in data.get("upstreams", [])]
            except Exception:
                names = []
            check("/health 上游齐全", len(names) >= 1, str(names))
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                proc.kill()

    failed = [n for n, ok, _ in CHECKS if not ok]
    print(f"\n共 {len(CHECKS)} 项，失败 {len(failed)} 项")
    print("\nPORT_DRIFT " + ("OK" if not failed else "FAILED"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
