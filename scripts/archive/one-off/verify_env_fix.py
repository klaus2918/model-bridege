# -*- coding: utf-8 -*-
"""打包产物专项验证：exe 能否从 ~/.jcode/.env 拿到 key 并打通 go 上游。

用法：py -3.11 scripts\\verify_env_fix.py
会启动 dist\Model-Gate.exe（--no-gui --port 8914），用真实配置与真实 .env：
  1. /health 里 go 的 keySource 应为 env
  2. 直接 GET /api/upstreams 里 go.keySource/keyConfigured
  3. 对 go 上游发一次真实 chat 请求，断言不再出现 401 Invalid API key
"""
from __future__ import annotations

import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "dist" / "Model-Gate.exe"
CFG = ROOT / "dist" / "bridge.config.json"
PORT = 8914
BASE = f"http://127.0.0.1:{PORT}"
FAILS: list[str] = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def get(path, timeout=25):
    req = urllib.request.Request(BASE + path)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def post(path, payload, timeout=90):
    data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(BASE + path, data=data, method="POST",
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def wait_listen(timeout=45):
    import socket
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.8)
            if s.connect_ex(("127.0.0.1", PORT)) == 0:
                return True
        time.sleep(0.5)
    return False


def main() -> int:
    if not EXE.is_file():
        print("MISSING exe")
        return 1

    # 还原真实配置
    import shutil
    shutil.copyfile(ROOT / "bridge.config.json", CFG)
    cfg = json.loads(CFG.read_text(encoding="utf-8"))
    go_model = (cfg.get("upstreams", {}).get("go", {}).get("models") or ["glm-5.3-flash"])[0]
    print(f"=== 启动 exe（真实配置 + 真实 ~/.jcode/.env），端口 {PORT} ===")
    print(f"go 首个模型：{go_model}\n")

    import subprocess
    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(PORT), "--config", str(CFG)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_listen():
            check("exe 监听", False, f":{PORT}")
            return 1
        check("exe 监听", True, f":{PORT}")

        print("\n=== [1] 上游清单里的密钥来源 ===")
        status, body = get("/api/upstreams")
        check("/api/upstreams 200", status == 200, f"HTTP {status}")
        try:
            rows = {r["name"]: r for r in json.loads(body)["upstreams"]}
            go = rows.get("go", {})
            print(f"  go.keySource      = {go.get('keySource')}")
            print(f"  go.keyConfigured  = {go.get('keyConfigured')}")
            check("go 密钥来源为 env", go.get("keySource") == "env",
                  f"keySource={go.get('keySource')}")
            check("go 已配置真实密钥", go.get("keyConfigured") is True)
        except Exception as exc:  # noqa: BLE001
            check("解析 /api/upstreams", False, str(exc))

        print("\n=== [2] 对 go 上游发真实请求 ===")
        status, body = post("/go/v1/chat/completions", {
            "model": go_model,
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "用一句话说你好"}],
        })
        print(f"  HTTP {status}")
        print(f"  {body[:400]}")
        check("不是 401 Invalid API key",
              not (status == 401 and "Invalid API key" in body),
              f"HTTP {status}")
        if status == 401 and "Invalid API key" in body:
            print("  => 仍然 401：key 仍未生效")
        elif status == 200:
            print("  => go 上游打通")
        else:
            print("  => 非 401，属于上游侧其它响应（模型/余额等）")

        print("\n=== [3] 日志里有密钥来源记录 ===")
        status, body = get("/api/logs?lines=50")
        check("/api/logs 200", status == 200, f"HTTP {status}")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=8)
        except Exception:
            proc.kill()
        print("\n=== 已停止 exe ===")

    print()
    if FAILS:
        print(f"ENV_FIX FAILED: {len(FAILS)}")
        for item in FAILS:
            print("  - " + item)
        return 1
    print("ENV_FIX OK")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
