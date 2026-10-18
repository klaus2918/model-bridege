# -*- coding: utf-8 -*-
"""交付物清单核验：逐项确认「exe 工具 + 模型配置 + token 统计 + 本地数据库」。

用法：py -3.11 scripts\\deliverables_check.py
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAILS: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" - {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def main() -> int:
    print("== 1. 打包产物 ==")
    exe = ROOT / "dist" / "Model-Gate.exe"
    check("dist/Model-Gate.exe 存在", exe.is_file(),
          f"{exe.stat().st_size // (1024 * 1024)} MB" if exe.is_file() else "缺失")
    check("图标 assets/ModelGate.ico", (ROOT / "assets" / "ModelGate.ico").is_file())
    check("打包脚本 build.bat", (ROOT / "build.bat").is_file())
    check("PyInstaller spec", (ROOT / "ModelGate.spec").is_file())
    check("Inno Setup 脚本（未编译，见阻塞项）",
          (ROOT / "ModelGate.iss").is_file())
    check("PyInstaller 入口 entry.py", (ROOT / "entry.py").is_file())

    print("\n== 2. 源码模块（模型接入） ==")
    pkg = ROOT / "modelgate"
    for rel in ("__init__.py", "__main__.py", "config.py", "db.py", "proxy.py",
                "api.py", "app.py", "util.py",
                "protocols/__init__.py", "protocols/base.py",
                "protocols/anthropic.py", "protocols/openai_cc.py",
                "protocols/commandcode.py", "protocols/thinking.py",
                "web/index.html", "web/app.js", "web/style.css"):
        check(f"modelgate/{rel}", (pkg / rel).is_file())

    print("\n== 3. 模型配置 ==")
    src = (pkg / "config.py").read_text(encoding="utf-8")
    check("读写 bridge.config.json 兼容格式", "load_config" in src and "save_config" in src)
    check("密钥三源解析 env/file/literal",
          "apiKeyEnv" in src and "apiKeyFile" in src and "resolve_api_key" in src)
    check("上游归一化（协议/baseUrl/模型/别名）",
          "normalize_upstream" in src and "pick_upstream" in src)
    repo_cfg = json.loads((ROOT / "bridge.config.json").read_text(encoding="utf-8"))
    check("既有配置仍可解析", isinstance(repo_cfg.get("upstreams"), dict),
          f"{len(repo_cfg.get('upstreams') or {})} 个上游")

    print("\n== 4. token 统计（用量明细 + 聚合） ==")
    db_src = (pkg / "db.py").read_text(encoding="utf-8")
    check("usage_records 表", "CREATE TABLE IF NOT EXISTS usage_records" in db_src)
    check("settings 表（运行时配置事实源）",
          "CREATE TABLE IF NOT EXISTS settings" in db_src)
    check("按天/模型/上游聚合 summary()",
          "def summary" in db_src and "group_by" in db_src)
    check("WAL + 线程锁", "journal_mode=WAL" in db_src and "RLock" in db_src)
    api_src = (pkg / "api.py").read_text(encoding="utf-8")
    check("统计 API /api/usage/{totals,summary,list}",
          all(p in api_src for p in ("/api/usage/totals", "/api/usage/summary",
                                     "/api/usage/list")))
    check("配置 API 密钥脱敏", "_keyConfigured" in api_src and "_keySource" in api_src)

    print("\n== 5. 本地数据库留存 ==")
    check("落库路径在 exe 同目录 data/",
          'default_db_path' in (pkg / "config.py").read_text(encoding="utf-8"))
    exe_db = ROOT / "dist" / "data" / "modelgate.db"
    check("exe 冷启动已产出 modelgate.db", exe_db.is_file(),
          f"{exe_db.stat().st_size} bytes" if exe_db.is_file() else "缺失")
    exe_log = ROOT / "dist" / "data" / "modelgate.log"
    check("访问日志落盘 modelgate.log", exe_log.is_file(),
          f"{exe_log.stat().st_size} bytes" if exe_log.is_file() else "缺失")

    print("\n== 6. 验证资产 ==")
    tests = ROOT / "tests"
    names = sorted(p.name for p in tests.glob("test_*.py"))
    check("测试文件 4 个", len(names) >= 4, ", ".join(names))
    check("冷启动验收脚本", (ROOT / "scripts" / "coldstart_all.ps1").is_file())
    check("GUI 验收脚本", (ROOT / "scripts" / "gui_probe.ps1").is_file())
    check("方案与任务清单", (ROOT / ".op" / "changes" / "model-gate-desktop"
                          / "plan.md").is_file())

    print("\n== 7. 变更档案 ==")
    tasks = (ROOT / ".op" / "changes" / "model-gate-desktop" / "tasks.md").read_text(
        encoding="utf-8")
    pending = re.findall(r"status:\s*(\w+)", tasks)
    check("10 个任务全部 done", len(pending) == 10 and all(
        s == "done" for s in pending), ", ".join(pending))

    print()
    if FAILS:
        print(f"DELIVERABLES FAILED: {len(FAILS)}")
        for f in FAILS:
            print("  - " + f)
        return 1
    print("DELIVERABLES OK: 全部通过")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace") \
        if hasattr(sys.stdout, "reconfigure") else None
    raise SystemExit(main())
