# -*- coding: utf-8 -*-
"""真实链路端到端验收：桩上游 → 打包后的 Model-Gate.exe → 客户端。

验证三件事（对应用户反馈）：
1. 代理真的能转发（三协议、流式 + 非流式）
2. 访问日志真的写进 data/modelgate.log
3. 用量明细真的写进 data/modelgate.db，且统计接口能查出来

用法：py -3.11 scripts\\real_e2e.py
"""
from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "dist" / "Model-Gate.exe"
STUB_PORT = 18901
PROXY_PORT = 8913
DATA_DIR = ROOT / "dist" / "data"
LOG = DATA_DIR / "modelgate.log"
DB = DATA_DIR / "modelgate.db"
CFG = ROOT / "dist" / "e2e.config.json"
ANSWER = "桩上游已收到请求，代理链路正常。"

FAILS: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def request(path: str, payload=None, timeout: int = 30, stream_lines: bool = False):
    """返回 (status, body_or_lines)。"""
    url = f"http://127.0.0.1:{PROXY_PORT}{path}"
    if payload is None:
        req = urllib.request.Request(url)
    else:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        req = urllib.request.Request(url, data=data, method="POST",
                                     headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            body = res.read().decode("utf-8", "replace")
            if stream_lines:
                return res.status, body.splitlines()
            return res.status, body
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def wait_listening(port: int, timeout: int = 40) -> bool:
    import socket

    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.8)
            if sock.connect_ex(("127.0.0.1", port)) == 0:
                return True
        time.sleep(0.5)
    return False


def write_config() -> None:
    stub = f"http://127.0.0.1:{STUB_PORT}"
    cfg = {
        "port": PROXY_PORT,
        "host": "127.0.0.1",
        "defaultModel": "stub-openai",
        "models": ["stub-anthropic", "stub-openai", "stub-cc"],
        "maxTokens": 1024,
        "timeoutSeconds": 30,
        "accessLog": True,
        "modelsCacheSeconds": 300,
        "aliases": {"fast": "stub-openai"},
        "thinking": {"passthrough": True, "cacheSize": 50},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": {
            "anth": {"protocol": "anthropic", "baseUrl": stub, "apiKey": "k",
                     "models": ["stub-anthropic"], "default": False,
                     "fetchModels": False},
            "oai": {"protocol": "openai", "baseUrl": stub, "apiKey": "k",
                    "sessionHeader": True, "models": ["stub-openai"],
                    "fetchModels": False},
            "cc": {"protocol": "commandcode", "baseUrl": stub, "apiKey": "k",
                   "apiKeySource": "literal", "cliVersion": "9.9.9",
                   "modelCatalog": False, "modelCatalogPlan": False,
                   "pauseTurn": True, "cmdZdr": True, "models": ["stub-cc"],
                   "fetchModels": False},
        },
    }
    CFG.write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")


def main() -> int:
    if not EXE.is_file():
        print(f"MISSING {EXE}")
        return 1

    # 清掉上一轮运行期数据，确保日志与落库来自本次
    if DATA_DIR.exists():
        for path in DATA_DIR.glob("modelgate*"):
            path.unlink()
    CFG.unlink(missing_ok=True)
    write_config()

    print("=== [1] 启动桩上游 ===")
    stub = subprocess.Popen(
        [sys.executable, str(ROOT / "scripts" / "stub_upstream.py"),
         "--port", str(STUB_PORT)],
        stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    if not wait_listening(STUB_PORT, 15):
        print("桩上游未就绪")
        stub.terminate()
        return 1
    check("桩上游就绪", True, f":{STUB_PORT}")

    print("\n=== [2] 启动 Model-Gate.exe ===")
    exe = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(PROXY_PORT),
         "--config", str(CFG)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        ok_listen = wait_listening(PROXY_PORT, 45)
    except Exception:
        ok_listen = False
    check("exe 监听", ok_listen, f":{PROXY_PORT}")
    if not ok_listen:
        stub.terminate()
        exe.terminate()
        return 1

    try:
        # ── 代理能力 ────────────────────────────────────────────────────
        print("\n=== [3] 代理转发 ===")
        status, body = request("/health")
        check("/health", status == 200, f"HTTP {status}")

        # anthropic 非流式
        status, body = request("/v1/chat/completions", {
            "model": "stub-anthropic",
            "messages": [{"role": "user", "content": "hi"}]})
        ok = status == 200 and ANSWER in body
        check("anthropic 非流式代理", ok, f"HTTP {status}")
        try:
            parsed = json.loads(body)
            check("anthropic 返回 usage",
                  parsed["usage"]["prompt_tokens"] == 11
                  and parsed["usage"]["completion_tokens"] == 7,
                  json.dumps(parsed.get("usage"), ensure_ascii=False))
            check("anthropic 返回思考与工具调用",
                  parsed["choices"][0]["message"].get("reasoning_content")
                  and parsed["choices"][0]["message"].get("tool_calls"))
        except Exception as exc:  # noqa: BLE001
            check("anthropic 响应可解析", False, str(exc))

        # anthropic 流式
        status, lines = request("/v1/chat/completions", {
            "model": "stub-anthropic", "stream": True,
            "messages": [{"role": "user", "content": "hi"}]}, stream_lines=True)
        text = "".join(
            json.loads(l[6:])["choices"][0]["delta"].get("content", "")
            for l in lines if l.startswith("data: ")
            and l != "data: [DONE]" and "choices" in json.loads(l[6:]))
        check("anthropic 流式代理", status == 200 and text == ANSWER,
              f"HTTP {status} text={text[:30]}")

        # openai 非流式 + 流式
        status, body = request("/v1/chat/completions", {
            "model": "stub-openai",
            "messages": [{"role": "user", "content": "hi"}]})
        check("openai 非流式代理", status == 200 and ANSWER in body, f"HTTP {status}")

        status, lines = request("/v1/chat/completions", {
            "model": "stub-openai", "stream": True,
            "messages": [{"role": "user", "content": "hi"}]}, stream_lines=True)
        done = any(l.strip() == "data: [DONE]" for l in lines)
        check("openai 流式代理", status == 200 and done, f"HTTP {status} done={done}")

        # commandcode 非流式 + 流式
        status, body = request("/v1/chat/completions", {
            "model": "stub-cc",
            "messages": [{"role": "user", "content": "hi"}]})
        ok = status == 200 and ANSWER in body
        check("commandcode 非流式代理", ok, f"HTTP {status}")
        try:
            parsed = json.loads(body)
            check("commandcode usage 含缓存",
                  parsed["usage"]["prompt_tokens_details"]["cached_tokens"] == 6,
                  json.dumps(parsed.get("usage"), ensure_ascii=False))
        except Exception as exc:  # noqa: BLE001
            check("commandcode usage", False, str(exc))

        status, lines = request("/v1/chat/completions", {
            "model": "stub-cc", "stream": True,
            "messages": [{"role": "user", "content": "hi"}]}, stream_lines=True)
        done = any(l.strip() == "data: [DONE]" for l in lines)
        check("commandcode 流式代理", status == 200 and done, f"HTTP {status}")

        # 前缀路由
        status, body = request("/oai/v1/chat/completions", {
            "model": "any-model", "messages": [{"role": "user", "content": "hi"}]})
        check("前缀路由 /oai", status == 200 and ANSWER in body,
              f"HTTP {status} {str(body)[:200]}")

        # 别名
        status, body = request("/v1/chat/completions", {
            "model": "fast", "messages": [{"role": "user", "content": "hi"}]})
        check("别名 fast → openai 上游", status == 200 and ANSWER in body,
              f"HTTP {status}")

        # ── 访问日志 ────────────────────────────────────────────────────
        print("\n=== [4] 访问日志 data/modelgate.log ===")
        time.sleep(0.6)
        check("modelgate.log 已生成", LOG.is_file(),
              f"{LOG.stat().st_size if LOG.is_file() else 0} bytes")
        if LOG.is_file():
            log_text = LOG.read_text(encoding="utf-8", errors="replace")
            check("日志含启动信息", "已启动" in log_text)
            check("日志含访问记录（tokens=）",
                  "tokens=" in log_text,
                  f"{log_text.count('tokens=')} 条")
            check("日志含流式记录", "stream tokens=" in log_text)
            check("日志含上游归属",
                  all(tag in log_text for tag in ("-> anth:", "-> oai:", "-> cc:")),
                  "anth/oai/cc 三类")

        # ── 用量落库 ────────────────────────────────────────────────────
        print("\n=== [5] 用量落库 data/modelgate.db ===")
        check("modelgate.db 已生成", DB.is_file(),
              f"{DB.stat().st_size if DB.is_file() else 0} bytes")
        rows = []
        if DB.is_file():
            conn = sqlite3.connect(str(DB))
            conn.row_factory = sqlite3.Row
            rows = [dict(r) for r in conn.execute(
                "SELECT * FROM usage_records ORDER BY ts DESC").fetchall()]
            conn.close()
        check("usage_records 有记录", len(rows) >= 8, f"{len(rows)} 条")
        if rows:
            by_up = {}
            for row in rows:
                by_up.setdefault(row["upstream"], 0)
                by_up[row["upstream"]] += 1
            check("覆盖三个上游", set(by_up) >= {"anth", "oai", "cc"},
                  json.dumps(by_up, ensure_ascii=False))
            check("token 合计 > 0",
                  sum(r["prompt_tokens"] for r in rows) > 0
                  and sum(r["completion_tokens"] for r in rows) > 0,
                  f"入 {sum(r['prompt_tokens'] for r in rows)} / "
                  f"出 {sum(r['completion_tokens'] for r in rows)}")
            check("流式与非流式都已记录",
                  any(r["stream"] for r in rows) and any(not r["stream"] for r in rows))
            check("缓存读命中已记录",
                  any(r["cache_read_tokens"] for r in rows))
            check("无 5xx 记录", not any(r["status"] >= 500 for r in rows),
                  str([r["status"] for r in rows]))

        # ── 统计接口（面板数据源） ──────────────────────────────────────
        print("\n=== [6] 统计接口 ===")
        status, body = request("/api/usage/totals?range=all")
        totals = json.loads(body)["totals"] if status == 200 else {}
        check("/api/usage/totals 与库一致",
              status == 200 and totals.get("requests") == len(rows),
              json.dumps(totals, ensure_ascii=False))

        status, body = request("/api/usage/summary?group_by=upstream&range=all")
        rows_up = json.loads(body)["rows"] if status == 200 else []
        check("/api/usage/summary 按上游",
              status == 200 and len(rows_up) == 3,
              json.dumps([r["bucket"] for r in rows_up], ensure_ascii=False))

        status, body = request("/api/usage/summary?group_by=model&range=all")
        rows_m = json.loads(body)["rows"] if status == 200 else []
        buckets = {r["bucket"] for r in rows_m}
        # 记录的是「客户端请求的名字」，因此别名 fast 与前缀请求的原名也会成桶
        check("/api/usage/summary 按模型",
              status == 200 and {"stub-anthropic", "stub-openai", "stub-cc"} <= buckets,
              json.dumps(sorted(buckets), ensure_ascii=False))

        status, body = request("/api/usage/list?range=all&limit=5")
        listed = json.loads(body)["rows"] if status == 200 else []
        check("/api/usage/list 返回明细", status == 200 and len(listed) == 5,
              f"{len(listed)} 条")

        status, body = request("/api/info")
        check("/api/info", status == 200, f"HTTP {status}")
    finally:
        print("\n=== [7] 清理 ===")
        for proc in (exe, stub):
            try:
                proc.terminate()
                proc.wait(timeout=8)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass
        print("  已停止 exe 与桩上游")

    print()
    if FAILS:
        print(f"REAL E2E FAILED: {len(FAILS)}")
        for item in FAILS:
            print("  - " + item)
        return 1
    print("REAL E2E OK: 代理、访问日志、用量落库全部通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
