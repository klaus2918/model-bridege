# -*- coding: utf-8 -*-
"""v2.3 产物验证（不触碰在线实例）：独立端口 + 临时库 + 临时配置。

v22 脚本会删 dist/data 并改写 dist/bridge.config.json —— 有实例在跑时是危险动作。
本脚本一切都在临时目录，可随时运行。
"""
from __future__ import annotations

import json
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXE = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "dist" / "Model-Gate-v2.3.exe"
PORT = 8966
OK: list[str] = []
BAD: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    (OK if ok else BAD).append(name)
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def get(path: str, timeout: float = 5.0):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=timeout) as r:
            return r.status, r.read()
    except Exception as exc:  # noqa: BLE001
        return -1, str(exc).encode()


def main() -> int:
    if not EXE.is_file():
        print("MISSING", EXE)
        return 1
    print(f"exe: {EXE.name} {EXE.stat().st_size} bytes")
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        probe.bind(("127.0.0.1", PORT))
    except OSError:
        print(f"端口 {PORT} 被占用")
        return 1
    finally:
        probe.close()

    tmp = pathlib.Path(tempfile.mkdtemp(prefix="mg-v23-"))
    cfg = tmp / "bridge.config.json"
    shutil.copyfile(ROOT / "bridge.config.json", cfg)
    data = json.loads(cfg.read_text(encoding="utf-8"))
    data["port"] = PORT
    cfg.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")

    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(PORT), "--db", str(tmp / "data" / "m.db"),
         "--config", str(cfg)],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    print("隔离实例 pid =", proc.pid)
    ready = False
    for _ in range(40):
        if proc.poll() is not None:
            break
        if get("/health", 1.5)[0] == 200:
            ready = True
            break
        time.sleep(0.5)
    check("隔离实例启动并监听", ready, f"port {PORT}")
    if ready:
        status, body = get("/api/info")
        info = json.loads(body.decode("utf-8", "replace")) if status == 200 else {}
        check("版本号 = 2.3.0", info.get("version") == "2.3.0", str(info.get("version")))

        status, ui = get("/ui/")
        html = ui.decode("utf-8", "replace")
        check("/ui/ 可达", status == 200, f"{len(ui)} 字节")
        check("含 Token 构成 / 耗时分布",
              'id="token-mix"' in html and 'id="latency-rows"' in html)
        check("含模型用量表", 'id="model-usage-table"' in html)
        check("含主题切换", 'id="theme-toggle"' in html)

        status, js = get("/ui/app.js")
        js_text = js.decode("utf-8", "replace")
        check("app.js 含新逻辑",
              all(k in js_text for k in ("renderTokenMix", "renderLatency",
                                         "loadModelUsage", "initTheme")))
        status, css = get("/ui/style.css")
        css_text = css.decode("utf-8", "replace")
        check("样式为工业仪表盘版", "Bahnschrift" in css_text and "--brand:" in css_text)

        status, body = get("/api/usage/totals?range=all")
        totals = (json.loads(body.decode("utf-8", "replace")).get("totals", {})
                  if status == 200 else {})
        need = ("p50_duration_ms", "p95_duration_ms", "max_duration_ms",
                "avg_ttft_ms", "ttft_samples")
        missing = [k for k in need if k not in totals]
        check("totals 含百分位与 TTFT", not missing,
              f"缺失 {missing}" if missing else "字段齐全")

        status, body = get("/api/usage/models?range=all")
        check("模型用量接口可用",
              status == 200 and isinstance(
                  json.loads(body.decode("utf-8", "replace")).get("rows"), list),
              f"HTTP {status}")

        status, body = get("/api/usage/export?range=all&format=csv")
        head = body.decode("utf-8-sig", "replace").splitlines()[0] if status == 200 else ""
        check("导出 CSV 含 ttft_ms 列", "ttft_ms" in head, f"HTTP {status}")

    proc.terminate()
    try:
        proc.wait(timeout=8)
    except subprocess.TimeoutExpired:
        proc.kill()
    shutil.rmtree(tmp, ignore_errors=True)
    print(f"\n共 {len(OK) + len(BAD)} 项，失败 {len(BAD)} 项")
    print("V23_VERIFY " + ("OK" if not BAD else "FAILED"))
    return 0 if not BAD else 1


if __name__ == "__main__":
    raise SystemExit(main())
