# -*- coding: utf-8 -*-
"""扫描 git 历史中的 bridge.config.json 是否含明文密钥。

只读。只报告「是否出现 / 出现几次 / 长度」，绝不打印明文。
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = "bridge.config.json"
# 常见 API key 形状（用于在任意文本里嗅探，不输出内容）
KEY_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{16,}"),
    re.compile(r"\b[A-Za-z0-9_\-]{40,}\b"),
]


def git(*args: str) -> tuple[int, str]:
    out = subprocess.run(["git", *args], cwd=ROOT, capture_output=True,
                         text=True, encoding="utf-8", errors="replace", timeout=60)
    return out.returncode, out.stdout


def scan_blob(text: str) -> list[str]:
    """返回命中描述（只含字段名/长度，不含明文）。"""
    hits: list[str] = []
    try:
        data = json.loads(text)
    except Exception:
        for pat in KEY_PATTERNS:
            for m in pat.finditer(text):
                hits.append(f"疑似 key 形状 <{len(m.group(0))} 字符>")
        return hits
    if not isinstance(data, dict):
        return hits

    ups = data.get("upstreams")
    if isinstance(ups, dict):
        for name, up in ups.items():
            if not isinstance(up, dict):
                continue
            ak = up.get("apiKey")
            if isinstance(ak, str) and ak:
                hits.append(f"upstreams.{name}.apiKey 明文 <{len(ak)} 字符>")
            for f in ("apiKeyEnv", "apiKeyFile"):
                v = up.get(f)
                if isinstance(v, str) and v:
                    hits.append(f"upstreams.{name}.{f} = {v}")
    legacy = data.get("upstream")
    if isinstance(legacy, dict):
        ak = legacy.get("apiKey")
        if isinstance(ak, str) and ak:
            hits.append(f"upstream.apiKey 明文 <{len(ak)} 字符>")
    if isinstance(data.get("envFile"), str):
        hits.append(f"envFile = {data['envFile']}")
    return hits


def main() -> int:
    print(f"=== 扫描 git 历史中的 {TARGET} ===\n")

    code, _ = git("rev-parse", "--is-inside-work-tree")
    if code != 0:
        print("不是 git 仓库")
        return 1

    # 1) 该文件的所有历史提交
    code, commits = git("log", "--all", "--format=%H|%ad|%s", "--date=short",
                        "--", TARGET)
    lines = [l for l in commits.splitlines() if l.strip()]
    print(f"历史提交数：{len(lines)}")
    leaks: list[str] = []
    for line in lines:
        print(f"  {line}")

    # 2) 逐个版本内容扫描
    print("\n=== 逐版本内容扫描 ===")
    code, revs = git("log", "--all", "--format=%H", "--", TARGET)
    for rev in [r.strip() for r in revs.splitlines() if r.strip()]:
        code, text = git("show", f"{rev}:{TARGET}")
        if code != 0:
            continue
        hits = scan_blob(text)
        leak_hits = [h for h in hits if "明文" in h or "疑似" in h]
        state = "含明文" if leak_hits else "干净"
        print(f"  {rev[:10]}  {state}")
        for h in hits:
            mark = "!!" if ("明文" in h or "疑似" in h) else "  "
            print(f"      {mark} {h}")
        if leak_hits:
            leaks.append(rev[:10])

    # 3) 全历史任意文件嗅探（防止 key 被抄到别处）
    print("\n=== 全历史任意文件嗅探（防 key 被复制到别处） ===")
    code, files = git("log", "--all", "--pretty=format:", "--name-only")
    names = sorted({l.strip() for l in files.splitlines() if l.strip()})
    scanned = 0
    other: list[str] = []
    for name in names:
        if name == TARGET or name.startswith(".git"):
            continue
        if not re.search(r"\.(json|env|js|mjs|cmd|bat|sh|md|txt|yml|yaml)$", name):
            continue
        code, text = git("show", f"HEAD:{name}")
        if code != 0:
            continue
        scanned += 1
        hits = [h for h in scan_blob(text) if "明文" in h or "疑似" in h]
        if hits:
            other.append(f"{name}: {hits}")
    print(f"  已扫描 {scanned} 个文件")
    for item in other:
        print(f"  !! {item}")

    print("\n" + "=" * 50)
    if leaks or other:
        print("结论：git 历史中**存在明文密钥**")
        print("影响：仅移除跟踪不够，历史仍可被读取")
        print("建议：到上游平台吊销并更换这些 key，再考虑重写历史")
        return 2
    print("结论：git 历史**未发现明文密钥**")
    print("可以安全执行方案 A（移除跟踪 + 补 .gitignore + 模板）")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
