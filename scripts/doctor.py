# -*- coding: utf-8 -*-
"""Model-Gate 统一诊断入口。

把原先分散的 check_* / config_check / show_runtime / diag_* 脚本收敛到一个
命令，按子命令执行，避免 scripts/ 根目录继续膨胀。

用法：
  py -3 scripts/doctor.py port                 # 端口占用 + 最近日志
  py -3 scripts/doctor.py config               # 配置可加载性与结构
  py -3 scripts/doctor.py env                  # 密钥文件/环境变量就位情况（不打印明文）
  py -3 scripts/doctor.py db                   # SQLite 设置、用量计数与最近记录
  py -3 scripts/doctor.py upstream [go ...]    # 逐条上游密钥体检（需网络）
  py -3 scripts/doctor.py cli                  # command-code CLI 版本
  py -3 scripts/doctor.py runtime              # 监听/进程/产物/本地上游连通性
  py -3 scripts/doctor.py gui [--exe ...]      # 启动 exe 验证窗口 + 面板，只停自己启动的 PID
  py -3 scripts/doctor.py all                  # config + env + port + runtime + db

退出码：0 正常；1 发现需要处理的问题；2 用法错误。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:  # pragma: no cover - 老解释器兜底
    pass

DEFAULT_PORTS = (8900, 8911)
RUNTIME_PORTS = (8900, 8911, 8912, 15721)
CONFIG_CANDIDATES = (ROOT / "bridge.config.json", ROOT / "dist" / "bridge.config.json")
DB_CANDIDATES = (ROOT / "dist" / "data" / "modelgate.db", ROOT / "data" / "modelgate.db")
LOG_CANDIDATES = (ROOT / "dist" / "data" / "modelgate.log", ROOT / "data" / "modelgate.log")


# ── 通用工具 ──────────────────────────────────────────────────────────────

def rel(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def ps(command: str, timeout: int = 30) -> str:
    """执行 PowerShell 并返回 stdout；失败时返回可读错误串。"""
    try:
        proc = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "$OutputEncoding=[Console]::OutputEncoding=[Text.Encoding]::UTF8; " + command],
            capture_output=True, text=True, encoding="utf-8", errors="replace",
            timeout=timeout,
        )
        return (proc.stdout or "").strip()
    except Exception as exc:  # noqa: BLE001
        return f"<powershell failed: {type(exc).__name__}: {exc}>"


def is_listening(port: int, host: str = "127.0.0.1") -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.8)
        return sock.connect_ex((host, port)) == 0


def port_owner(port: int) -> str:
    raw = ps(f"Get-NetTCPConnection -LocalPort {port} -State Listen | "
             "ForEach-Object { $_.OwningProcess }")
    pids = [line.strip() for line in raw.split() if line.strip().isdigit()]
    if not pids:
        return "无监听"
    names = []
    for pid in pids[:3]:
        name = ps(f"(Get-Process -Id {pid} -ErrorAction SilentlyContinue).ProcessName")
        names.append(f"{pid}:{name or '?'}")
    return ", ".join(names)


def tail(path: Path, lines: int) -> None:
    if not path.is_file():
        print(f"\n--- {rel(path)} 不存在 ---")
        return
    content = path.read_text(encoding="utf-8", errors="replace").splitlines()
    print(f"\n--- {rel(path)} 尾部（{lines} 行）---")
    for line in content[-lines:]:
        print("  " + line)


def mask(value: str) -> str:
    if not value:
        return "(空)"
    if len(value) <= 12:
        return "***"
    return f"{value[:6]}…{value[-4:]}（长度 {len(value)}）"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


# ── 子命令 ────────────────────────────────────────────────────────────────

def cmd_port(args) -> int:
    for port in getattr(args, "ports", None) or DEFAULT_PORTS:
        print(f"port {port}: {'监听中' if is_listening(port) else '空闲'}  "
              f"owner={port_owner(port)}")
    tail(ROOT / "dist" / "data" / "modelgate.log", args.lines)
    return 0


def cmd_config(args) -> int:
    from modelgate.config import load_config, upstreams_of, validate_config_shape

    paths = [Path(args.config)] if args.config else list(CONFIG_CANDIDATES)
    bad = 0
    for path in paths:
        if not path.is_file():
            print(f"[--  ] {rel(path)}: 不存在（跳过）")
            continue
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001
            print(f"[FAIL] {rel(path)}: JSON 解析失败 {exc}")
            bad += 1
            continue
        problem = validate_config_shape(raw)
        try:
            config = load_config(path)
            names = [up["name"] for up in upstreams_of(config)]
        except Exception as exc:  # noqa: BLE001
            print(f"[FAIL] {rel(path)}: 加载失败 {type(exc).__name__}: {exc}")
            bad += 1
            continue
        print(f"[{'OK  ' if not problem else 'WARN'}] {rel(path)}: "
              f"port={config['port']} models={len(config['models'])} upstreams={names}")
        if problem:
            print(f"       结构问题：{problem}")
            bad += 1
    print()
    if bad:
        print(f"CONFIG_CHECK FAILED: {bad}")
        return 1
    print("CONFIG_CHECK OK")
    return 0


def cmd_env(args) -> int:
    env_path = Path.home() / ".jcode" / ".env"
    print(f"env file: {env_path}")
    print(f"exists  : {env_path.is_file()}")
    if not env_path.is_file():
        print("=> 没有 env 文件，这里是 key 缺失的直接原因")
        return 1

    found: dict[str, int] = {}
    for line in env_path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$", line)
        if match and match.group(2):
            found[match.group(1)] = len(match.group(2))

    print(f"\n加载到的变量（{len(found)} 个，值均非空）：")
    for name in sorted(found):
        print(f"  {name} = <{found[name]} chars>")

    targets = {"OPENCODE_API_KEY": "go", "COMMAND_CODE_API_KEY": "commandcode"}
    print("\n=== 上游密钥就位情况 ===")
    missing = []
    for var, upstream in targets.items():
        ok = var in found
        print(f"  {var:<24} -> 上游 {upstream:<12} {'OK' if ok else 'MISSING'}")
        if not ok:
            missing.append(var)

    print("\n=== 进程环境变量（os.environ） ===")
    for var in targets:
        print(f"  {var}: {'已设置' if os.environ.get(var) else '未设置'}")

    if missing:
        print(f"\n=> 缺失：{', '.join(missing)}")
        return 1
    print("\n=> 密钥齐备")
    return 0


def cmd_db(args) -> int:
    paths = [Path(args.db)] if args.db else list(DB_CANDIDATES)
    for path in paths:
        if not path.is_file():
            print(f"=== {rel(path)} : 不存在 ===")
            continue
        print(f"=== {rel(path)} ({path.stat().st_size} bytes) ===")
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row
        try:
            rows = conn.execute("SELECT key, length(value) AS n FROM settings").fetchall()
            print(f"  settings keys: { {r['key']: r['n'] for r in rows} }")

            row = conn.execute("SELECT value FROM settings WHERE key='config'").fetchone()
            if row:
                try:
                    config = json.loads(row[0])
                    print(f"  settings.config: port={config.get('port')} "
                          f"defaultModel={config.get('defaultModel')}")
                    for name, up in (config.get("upstreams") or {}).items():
                        print(f"    {name}: baseUrl={up.get('baseUrl')} "
                              f"apiKeyEnv={up.get('apiKeyEnv')} "
                              f"apiKeyFile={up.get('apiKeyFile')} "
                              f"models={up.get('models')}")
                except Exception as exc:  # noqa: BLE001
                    print(f"  settings.config 解析失败: {exc}")
            else:
                print("  settings.config: 无")

            total = conn.execute("SELECT COUNT(*) AS n FROM usage_records").fetchone()[0]
            print(f"  usage_records: {total} 条")
            if total and args.limit:
                recent = conn.execute(
                    "SELECT ts, created_at, upstream, protocol, requested_model, "
                    "prompt_tokens, completion_tokens, duration_ms, status, stream, error "
                    "FROM usage_records ORDER BY ts DESC LIMIT ?", (args.limit,)
                ).fetchall()
                print(f"  最近 {len(recent)} 条：")
                for item in recent:
                    print("    " + json.dumps(dict(item), ensure_ascii=False))
        finally:
            conn.close()

    for log in LOG_CANDIDATES:
        if log.is_file():
            tail(log, args.lines)
    return 0


def cmd_upstream(args) -> int:
    from modelgate.config import env_file_path, load_config, load_env_file, upstreams_of

    config = load_config(args.config or str(ROOT / "bridge.config.json"))
    load_env_file(config)
    upstreams = upstreams_of(config)
    if args.names:
        upstreams = [up for up in upstreams if up["name"] in args.names]

    env_path = env_file_path(config)
    print(f"配置文件: {args.config or rel(ROOT / 'bridge.config.json')}")
    print(f"密钥文件: {env_path}（存在: {env_path.is_file() if env_path else False}）\n")

    problems = 0
    for up in upstreams:
        name = up["name"]
        key = up.get("apiKey") or ""
        source = up.get("apiKeySource")
        print(f"[{name}] protocol={up.get('protocol')} baseUrl={up.get('baseUrl')}")
        if source == "env":
            print(f"  来源: 环境变量 {up.get('apiKeyEnv')} = {mask(key)}")
        elif source == "file":
            print(f"  来源: 密钥文件 {up.get('apiKeyFile')} = {mask(key)}")
        elif source == "literal":
            print(f"  来源: 配置内 apiKey = {mask(key)}")
        else:
            print("  来源: 未配置密钥")

        status, body = probe_upstream(str(up.get("baseUrl") or ""), key or None, args.timeout)
        hint = ""
        if status == 200:
            verdict = "可用"
        elif status in (401, 403):
            text = body.lower()
            if "expired" in text or "api_key_expired" in text:
                verdict = "密钥已过期"
                hint = ("需到上游控制台新建 API Key 并替换"
                        + (f"（环境变量 {up.get('apiKeyEnv')}）" if source == "env" else "")
                        + "；重新登录不会续期")
                problems += 1
            elif not key:
                verdict = "未配置密钥"
                hint = "请在配置页填写 apiKey 或设置 apiKeyEnv"
                problems += 1
            else:
                verdict = "密钥无效"
                hint = "请核对密钥是否完整（注意空格、换行、复制截断）"
                problems += 1
        elif status == -1:
            verdict = "网络不可达"
            hint = body[:120]
        else:
            verdict = f"HTTP {status}"
        print(f"  GET /v1/models → {status} {verdict}")
        if hint:
            print(f"  建议: {hint}")
        if status not in (200, -1):
            print(f"  上游原文: {body[:200].replace(chr(10), ' ')}")
        print()

    print(f"结论：{len(upstreams)} 条上游，需处理 {problems} 条")
    return 1 if problems else 0


def probe_upstream(base_url: str, key: str | None, timeout: float = 15.0) -> tuple[int, str]:
    """探测上游 /v1/models；只返回状态码与截断后的响应。"""
    url = base_url.rstrip("/") + "/v1/models"
    headers = {"user-agent": "model-gate-doctor", "accept": "application/json"}
    if key:
        headers["authorization"] = f"Bearer {key}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read(200).decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(400).decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def cmd_cli(args) -> int:
    import httpx
    from modelgate.config import command_code_roots

    print("=== 候选安装路径 ===")
    for root in command_code_roots():
        package = Path(root) / "package.json"
        if package.is_file():
            try:
                data = json.loads(package.read_text(encoding="utf-8"))
                print(f"  {package}")
                print(f"    name={data.get('name')} version={data.get('version')}")
            except Exception as exc:  # noqa: BLE001
                print(f"  {package} 解析失败 {exc}")
        else:
            print(f"  {root} (无 package.json)")

    print("\n=== npm registry 最新版本 ===")
    try:
        response = httpx.get("https://registry.npmjs.org/command-code/latest", timeout=15)
        if response.is_success:
            print(f"  latest = {(response.json() or {}).get('version')}")
        else:
            print(f"  HTTP {response.status_code}")
    except Exception as exc:  # noqa: BLE001
        print(f"  查询失败: {exc}")

    print("\n=== 上游要求 ===")
    print("  minVersion = 0.18.10（来自 403 upgrade_required 响应）")
    return 0


def cmd_runtime(args) -> int:
    print("=== [1] listeners ===")
    for port in getattr(args, "ports", None) or RUNTIME_PORTS:
        if is_listening(port):
            print(f"  port {port} LISTEN owner={port_owner(port)}")
        else:
            print(f"  port {port} (nothing)")

    print("=== [2] Model-Gate processes ===")
    pids = [line.strip() for line in
            ps("Get-Process -ErrorAction SilentlyContinue | "
               "Where-Object { $_.ProcessName -like 'Model-Gate*' } | "
               "ForEach-Object { $_.Id }").split() if line.strip().isdigit()]
    print("  " + (", ".join(f"pid={pid}" for pid in pids) if pids else "none"))

    print("=== [3] artifacts ===")
    for path in (*LOG_CANDIDATES, *DB_CANDIDATES,
                 ROOT / "dist" / "bridge.config.json"):
        if path.is_file():
            stat = path.stat()
            print(f"  OK   {rel(path)} {stat.st_size} bytes  {stat.st_mtime:.0f}")
        else:
            print(f"  MISS {rel(path)}")

    print("=== [4] local reachability ===")
    for port in (15721, 8900):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(1.5)
            ok = sock.connect_ex(("127.0.0.1", port)) == 0
        print(f"  127.0.0.1:{port} {'reachable' if ok else 'unreachable'}")
    return 0


def cmd_gui(args) -> int:
    exe = Path(args.exe) if args.exe else _pick_exe()
    if not exe or not exe.is_file():
        print(f"未找到 exe：{exe or ROOT / 'dist' / 'Model-Gate.exe'}")
        return 1

    port = args.port or free_port()
    out_path = ROOT / "dist" / "gui.out"
    err_path = ROOT / "dist" / "gui.err"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    print(f"EXE={exe}")
    print(f"PROBE_PORT={port}")

    with out_path.open("w", encoding="utf-8", errors="replace") as out, \
            err_path.open("w", encoding="utf-8", errors="replace") as err:
        proc = subprocess.Popen([str(exe), "--port", str(port)],
                                cwd=str(ROOT), stdout=out, stderr=err)

    listening = False
    title = ""
    try:
        for _ in range(30):
            time.sleep(1)
            listening = listening or is_listening(port)
            if not title:
                title = ps(f"(Get-Process -Id {proc.pid} -ErrorAction SilentlyContinue)"
                           ".MainWindowTitle")
                if not title:
                    title = ps("Get-Process -ErrorAction SilentlyContinue | "
                               "Where-Object { $_.ProcessName -like 'Model-Gate*' -and "
                               "$_.MainWindowTitle } | "
                               "Select-Object -First 1 -ExpandProperty MainWindowTitle")
            if listening and title:
                break
            if proc.poll() is not None:
                break

        ui_ok = False
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/ui/", timeout=10) as response:
                ui_ok = response.status == 200
        except Exception:  # noqa: BLE001
            ui_ok = False

        print(f"EXE_PID={proc.pid}")
        print(f"EXE_EXITED={proc.poll() is not None}")
        print(f"LISTENING={listening} port={port}")
        print(f"WINDOW_TITLE={title}")
        print(f"UI_OK={ui_ok}")
    finally:
        # 只清理本次启动的进程树，绝不按进程名批量结束
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True, text=True, encoding="utf-8",
                       errors="replace")
        print("STOPPED")

    if listening and ui_ok:
        print("GUI PROBE OK" if title else "GUI PROBE PARTIAL (server ok, window not observed)")
        return 0
    print("GUI PROBE FAILED")
    return 1


def _pick_exe() -> Path | None:
    dist = ROOT / "dist"
    candidates = sorted(
        (p for p in dist.glob("Model-Gate-v*.exe") if "setup" not in p.name.lower()),
        key=lambda p: p.stat().st_mtime, reverse=True)
    if candidates:
        return candidates[0]
    fallback = dist / "Model-Gate.exe"
    return fallback if fallback.is_file() else None


def cmd_all(args) -> int:
    results = {
        "config": cmd_config(args),
        "env": cmd_env(args),
        "port": cmd_port(args),
        "runtime": cmd_runtime(args),
        "db": cmd_db(args),
    }
    bad = [name for name, code in results.items() if code]
    print("\n=== doctor all ===")
    for name, code in results.items():
        print(f"  {name}: {'OK' if not code else 'CHECK'}")
    return 1 if bad else 0


# ── 入口 ──────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Model-Gate 统一诊断入口")
    sub = parser.add_subparsers(dest="command")

    p = sub.add_parser("port", help="端口占用与最近日志")
    p.add_argument("--ports", type=int, nargs="*")
    p.add_argument("--lines", type=int, default=8)
    p.set_defaults(func=cmd_port)

    p = sub.add_parser("config", help="配置可加载性与结构")
    p.add_argument("--config")
    p.set_defaults(func=cmd_config)

    p = sub.add_parser("env", help="密钥文件与环境变量就位情况")
    p.set_defaults(func=cmd_env)

    p = sub.add_parser("db", help="SQLite 设置、用量计数与最近记录")
    p.add_argument("--db")
    p.add_argument("--limit", type=int, default=10)
    p.add_argument("--lines", type=int, default=20)
    p.set_defaults(func=cmd_db)

    p = sub.add_parser("upstream", help="逐条上游密钥体检（需网络）")
    p.add_argument("names", nargs="*")
    p.add_argument("--config")
    p.add_argument("--timeout", type=float, default=15.0)
    p.set_defaults(func=cmd_upstream)

    p = sub.add_parser("cli", help="command-code CLI 版本")
    p.set_defaults(func=cmd_cli)

    p = sub.add_parser("runtime", help="监听/进程/产物/本地连通性")
    p.add_argument("--ports", type=int, nargs="*")
    p.set_defaults(func=cmd_runtime)

    p = sub.add_parser("gui", help="启动 exe 验证窗口与面板（只停自己启动的 PID）")
    p.add_argument("--exe")
    p.add_argument("--port", type=int, default=0)
    p.set_defaults(func=cmd_gui)

    p = sub.add_parser("all", help="config + env + port + runtime + db")
    p.add_argument("--config")
    p.add_argument("--db")
    p.add_argument("--limit", type=int, default=5)
    p.add_argument("--lines", type=int, default=8)
    p.set_defaults(func=cmd_all)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    if not getattr(args, "command", None):
        parser.print_help()
        return 2
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
