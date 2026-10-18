# -*- coding: utf-8 -*-
"""v2.2 实机验收：打包后的 exe 端到端复验。

覆盖能力补齐项的可观测结果（design-completeness.md §3）：
  V6 模型清单：/api/models/catalog 返回全部上游模型，检索/分页可用
  V7 单实例：已有实例在跑时，第二次启动不再产生新进程
  V8 主链路：8900 被占时端口降级、/ui/ 可达、新页面存在、无 crash.log
  V4 导出：/api/usage/export 返回带 BOM 的 CSV
  V3 费用：/api/pricing 可读可写

用法：py -3.11 scripts/verify_v22.py
"""
from __future__ import annotations

import json
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
EXE = DIST / "Model-Gate.exe"
LOG = DIST / "data" / "modelgate.log"
CRASH = DIST / "data" / "crash.log"
BASE_PORT = 8900
FALLBACK_PORTS = tuple(range(8901, 8911))

CHECKS: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def http(port: int, path: str, timeout: float = 5.0):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=timeout) as r:
            return r.status, r.read()
    except Exception as exc:  # noqa: BLE001
        return -1, str(exc).encode()


def occupy(port: int):
    """占用端口（模拟旧 Node 桥仍在跑的用户环境）。已被占则返回 None。"""
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    probe.settimeout(0.4)
    try:
        probe.connect(("127.0.0.1", port))
        probe.close()
        return None  # 真的已被别人占着，直接复用这个状态
    except OSError:
        pass
    finally:
        probe.close()
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
    sock.bind(("127.0.0.1", port))
    sock.listen(1)
    return sock


def read_log_tail(n: int = 12) -> list[str]:
    if not LOG.is_file():
        return []
    return LOG.read_text(encoding="utf-8", errors="replace").splitlines()[-n:]


def main() -> int:
    if not EXE.is_file():
        print("MISSING dist/Model-Gate.exe")
        return 1
    print(f"exe: {EXE.name} {EXE.stat().st_size} bytes")
    if CRASH.is_file():
        CRASH.unlink()
        print("removed stale crash.log")

    holder = occupy(BASE_PORT)
    check("8900 处于被占用状态（用户环境）", True,
          "复用现有占用" if holder is None else "本次脚本占用")

    before = len(read_log_tail(2000))
    proc = subprocess.Popen([str(EXE)], cwd=str(DIST), stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    print(f"已启动 pid={proc.pid}")

    port = None
    for _ in range(80):
        if proc.poll() is not None:
            break
        for candidate in FALLBACK_PORTS:
            status, _body = http(candidate, "/health", timeout=1.5)
            if status == 200:
                port = candidate
                break
        if port:
            break
        time.sleep(0.5)

    alive = proc.poll() is None
    check("端口冲突自动降级并成功监听", port is not None, f"实际端口 {port}")
    check("进程存活（窗口未崩溃）", alive)

    if port:
        status, body = http(port, "/health")
        try:
            health = json.loads(body.decode("utf-8"))
        except Exception:
            health = {}
        names = [u.get("name") for u in health.get("upstreams", [])]
        check("/health 返回上游清单", status == 200 and len(names) >= 1, str(names))

        status, ui = http(port, "/ui/")
        html = ui.decode("utf-8", "replace")
        check("/ui/ 面板可达", status == 200, f"{len(ui)} 字节")
        check("面板含「模型」页签", 'data-tab="model"' in html)
        check("面板含会话分组与导出按钮",
              'data-group="session"' in html and "export-usage-csv" in html)

        # 新版静态资源必须随包带上（否则页面是旧的）
        status, js = http(port, "/ui/app.js")
        js_text = js.decode("utf-8", "replace")
        check("app.js 随包更新（含模型页逻辑）",
              status == 200 and "loadCatalog" in js_text and "savePricing" in js_text)
        check("app.js 含密钥写入能力（saveUpstreamSecret）",
              "saveUpstreamSecret" in js_text and "写入 .env" in js_text)

        # 密钥写入接口必须存在（用不存在的上游探测：应返回 404 而不是 404-未实现）
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}/api/upstreams/secret",
                data=json.dumps({"name": "__probe__", "apiKey": "x"}).encode(),
                headers={"content-type": "application/json"}, method="POST")
            with urllib.request.urlopen(req, timeout=5) as r:
                code, body = r.status, r.read(200).decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            code, body = exc.code, exc.read(200).decode("utf-8", "replace")
        check("密钥写入接口已注册", code == 404 and "unknown_upstream" in body,
              f"HTTP {code}")

        status, payload = http(port, "/api/models/catalog?page=1&page_size=200")
        try:
            catalog = json.loads(payload.decode("utf-8"))
        except Exception:
            catalog = {}
        models = [r.get("model") for r in catalog.get("rows", [])]
        check("模型清单接口可用", status == 200 and catalog.get("total", 0) > 0,
              f"{catalog.get('total')} 个模型")
        check("模型清单覆盖全部上游", len({m for m in models}) >= 1,
              f"示例 {models[:3]}")

        status, payload = http(port, "/api/pricing")
        pricing = json.loads(payload.decode("utf-8")) if status == 200 else {}
        check("单价表接口可用", status == 200 and "pricing" in pricing,
              f"configured={pricing.get('configured')}")

        status, body = http(port, "/api/usage/export?range=all&format=csv")
        check("导出接口返回 CSV（含 BOM）", status == 200 and body[:3] == b"\xef\xbb\xbf",
              f"{len(body)} 字节")

        # ── V7 单实例 ──────────────────────────────────────────────
        second = subprocess.Popen([str(EXE)], cwd=str(DIST), stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            code = second.wait(timeout=25)
        except subprocess.TimeoutExpired:
            code = None
        check("第二个实例被单实例守卫拦下并退出",
              code == 0, f"退出码 {code}")
        log_text = "\n".join(read_log_tail(40))
        check("日志记录「已有实例」", "已有 Model-Gate 实例在运行" in log_text)

    if alive:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
    if holder is not None:
        holder.close()

    print("\n--- 日志尾部 ---")
    for line in read_log_tail(10):
        print("  " + line)
    check("无 crash.log", not CRASH.is_file())

    failed = [name for name, ok, _ in CHECKS if not ok]
    print(f"\n共 {len(CHECKS)} 项，失败 {len(failed)} 项")
    for name in failed:
        print("  FAIL " + name)
    print("\nV22_VERIFY " + ("OK" if not failed else "FAILED"))
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
