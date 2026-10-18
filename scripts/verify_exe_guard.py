# -*- coding: utf-8 -*-
"""验证**打包后的 exe** 是否包含明文密钥硬约束。

做法：起 exe（真实配置+真实 .env，端口 8915）→ POST /api/config 带明文 key
→ 读回文件与 settings，断言零明文、且运行态仍持有。
"""
from __future__ import annotations

import json
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "dist" / "Model-Gate.exe"
CFG = ROOT / "dist" / "bridge.config.json"
DATA = ROOT / "dist" / "data"
LOG = DATA / "modelgate.log"
DB = DATA / "modelgate.db"
PORT = 8915
BASE = f"http://127.0.0.1:{PORT}"
SECRET = "sk-" + "exe-verify-secret-1234567890"
FAILS: list[str] = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def wait_listen(timeout=45):
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.8)
            if s.connect_ex(("127.0.0.1", PORT)) == 0:
                return True
        time.sleep(0.5)
    return False


def get(path, timeout=20):
    req = urllib.request.Request(BASE + path)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        return -1, f"{type(e).__name__}: {e}"


def post(path, payload, timeout=30):
    data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(BASE + path, data=data, method="POST",
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        return -1, f"{type(e).__name__}: {e}"


def main() -> int:
    if not EXE.is_file():
        print("MISSING exe")
        return 1

    shutil.copyfile(ROOT / "bridge.config.json", CFG)
    for p in DATA.glob("modelgate*"):
        p.unlink()

    cfg = json.loads(CFG.read_text(encoding="utf-8"))
    cfg["upstreams"]["cch"] = {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:1",
                               "apiKey": SECRET, "models": ["m"], "default": True,
                               "fetchModels": False}
    CFG.write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"=== 启动 exe（端口 {PORT}），cch 带明文 key ===\n")

    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(PORT), "--config", str(CFG)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_listen():
            check("exe 监听", False)
            return 1
        check("exe 监听", True, f":{PORT}")

        print("=== [1] 启动即读取后：如实反映文件现状（读取路径不重写文件）===")
        on_disk = json.loads(CFG.read_text(encoding="utf-8"))
        # 硬约束管的是「保存」路径；读取路径应如实反映文件现状，不重写文件
        print(f"  cch.apiKey on disk = {on_disk['upstreams']['cch'].get('apiKey')!r}")
        check("读取不重写配置文件",
              on_disk["upstreams"]["cch"].get("apiKey") == SECRET,
              "文件保持原样（明文是本次测试自己写入的）")

        print("\n=== [2] 走 /api/config 保存一次（同内容）===")
        status, body = post("/api/config", {"config": cfg})
        print(f"  HTTP {status}")
        if status != 200:
            check("保存接口 200", False, body[:200])
            return 1
        check("保存接口 200", True)

        on_disk_after = json.loads(CFG.read_text(encoding="utf-8"))
        leaked = SECRET in json.dumps(on_disk_after, ensure_ascii=False)
        print(f"  cch.apiKey after save = "
              f"{on_disk_after['upstreams']['cch'].get('apiKey')!r}")
        check("保存后文件零明文", not leaked)

        print("\n=== [3] SQLite settings 里是否有明文 ===")
        conn = sqlite3.connect(str(DB))
        row = conn.execute("SELECT value FROM settings WHERE key='config'").fetchone()
        conn.close()
        settings_text = row[0] if row else ""
        leaked_db = SECRET in settings_text
        check("settings 零明文", not leaked_db,
              f"settings len={len(settings_text)}")

        print("\n=== [4] 运行态仍持有明文（/api/upstreams 的 keyConfigured）===")
        status, body = get("/api/upstreams")
        ok = status == 200
        if ok:
            ups = {u["name"]: u for u in json.loads(body)["upstreams"]}
            cch = ups.get("cch", {})
            print(f"  cch.keyConfigured = {cch.get('keyConfigured')} "
                  f"keySource = {cch.get('keySource')}")
            check("运行态已配置密钥", cch.get("keyConfigured") is True)
        else:
            check("/api/upstreams 200", False, body[:200])

        print("\n=== [5] 日志无明文泄漏 ===")
        log_text = LOG.read_text(encoding="utf-8", errors="replace") if LOG.is_file() else ""
        check("日志不含明文", SECRET not in log_text,
              f"log {len(log_text)} bytes")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except Exception:
            proc.kill()
        # 还原真实配置
        shutil.copyfile(ROOT / "bridge.config.json", CFG)
        print("\n=== 已停止 exe 并还原配置 ===")

    print()
    if FAILS:
        print(f"EXE_GUARD FAILED: {len(FAILS)}")
        for f in FAILS:
            print("  - " + f)
        return 1
    print("EXE_GUARD OK: 打包产物已包含明文密钥硬约束")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
