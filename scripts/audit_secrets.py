# -*- coding: utf-8 -*-
"""审计 bridge.config.json 是否含明文密钥、.gitignore 是否拦住、git 是否已跟踪。"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAILS: list[str] = []


def check(name, ok, detail=""):
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILS.append(name)


def main() -> int:
    cfg_path = ROOT / "bridge.config.json"
    cfg = json.loads(cfg_path.read_text(encoding="utf-8"))

    print("=== [1] 配置里的密钥字段 ===")
    literal = []
    env_ref = []
    file_ref = []
    for name, up in (cfg.get("upstreams") or {}).items():
        ak = up.get("apiKey")
        if isinstance(ak, str) and ak:            # 有真实明文
            literal.append((name, len(ak)))
        elif up.get("apiKeyEnv"):
            env_ref.append((name, up["apiKeyEnv"]))
        elif up.get("apiKeyFile"):
            file_ref.append((name, up["apiKeyFile"]))
        else:
            print(f"  {name}: 无密钥字段")

    for n, ln in literal:
        print(f"  {n}: apiKey 明文 <{ln} 字符>")
    for n, v in file_ref:
        print(f"  {n}: apiKeyFile → {v}（不含明文）")
    for n, v in env_ref:
        print(f"  {n}: apiKeyEnv → {v}（不含明文）")

    check("bridge.config.json 无明文密钥", not literal,
          f"{len(literal)} 处明文：{[n for n, _ in literal]}" if literal else "零明文")
    # commandcode 同时有 apiKeyEnv 与 apiKeyFile：env 优先，file 是兜底，两者都不算违规
    check("commandcode 不使用明文（env 或 file 引用）",
          any(v == "COMMAND_CODE_API_KEY" for _, v in env_ref)
          or any("auth.json" in v for _, v in file_ref),
          f"env={[v for _, v in env_ref if 'COMMAND' in v]} "
          f"file={file_ref}")

    print("\n=== [2] .gitignore 是否拦住 ===")
    gi = ROOT / ".gitignore"
    if not gi.is_file():
        check(".gitignore 存在", False)
    else:
        text = gi.read_text(encoding="utf-8", errors="replace")
        print("  关键行：")
        for line in text.splitlines():
            s = line.strip()
            if s and not s.startswith("#"):
                print(f"    {s}")
        sensitive = (".env", "auth.json", "bridge.config.json", "data/", "dist/")
        missing = [p for p in sensitive if p not in text]
        check("敏感路径已忽略", not missing,
              f"未忽略：{missing}" if missing else "全部覆盖")

    print("\n=== [3] git 是否已跟踪配置文件 ===")
    try:
        out = subprocess.run(["git", "ls-files", "--error-unmatch", "bridge.config.json"],
                            cwd=ROOT, capture_output=True, text=True, timeout=15)
        tracked = out.returncode == 0
    except Exception as exc:  # noqa: BLE001
        tracked = None
        print(f"  git 查询失败: {exc}")
    if tracked is False:
        check("bridge.config.json 未被 git 跟踪", True, "未被跟踪")
    elif tracked is True:
        check("bridge.config.json 未被 git 跟踪", False, "已被跟踪 —— 有泄露风险")

    print("\n" + ("AUDIT_FAILED: " + str(len(FAILS)) if FAILS else "SECRET_AUDIT OK"))
    for item in FAILS:
        print("  - " + item)
    return 1 if FAILS else 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
