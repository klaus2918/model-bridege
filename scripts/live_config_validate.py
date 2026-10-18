# -*- coding: utf-8 -*-
"""在线实例的「保存校验」只读复核。

用冻结在产物里的 app.js（从在线实例取，不是源码）在 node 里跑
validateConfig + normalizeBaseUrl，喂真实配置，回答两个问题：
  1. 当前配置在面板上会报几条 error / warn？
  2. 如果某个上游 Base URL 缺协议，规范化后是否还报错（这正是「有 1 处错误」的成因）？

只读：不 POST /api/config，不改 dist\\bridge.config.json。
"""
from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

LIVE = "http://127.0.0.1:8900"


def fetch(url: str) -> str:
    with urllib.request.urlopen(url, timeout=15) as r:
        return r.read().decode("utf-8", "replace")


def balanced(src: str, start: int) -> str:
    """从 start 处的 '{' 起，返回配对到 '}' 的子串。"""
    depth = 0
    i = start
    while i < len(src):
        c = src[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return src[start:i + 1]
        elif c in "\"'`":
            quote = c
            i += 1
            while i < len(src) and src[i] != quote:
                if src[i] == "\\":
                    i += 1
                i += 1
        i += 1
    raise SystemExit(f"brace not balanced from {start}")


def grab(src: str, name: str) -> str:
    m = re.search(r"function\s+" + name + r"\s*\(", src)
    if not m:
        raise SystemExit(f"function {name} not found")
    brace = src.find("{", m.end())
    return "function " + name + src[m.end() - 1:brace] + balanced(src, brace)


def main() -> int:
    node = shutil.which("node")
    if not node:
        print("node 不可用")
        return 2
    js_src = fetch(LIVE + "/ui/app.js")
    page = fetch(LIVE + "/ui/")
    m = re.search(r"app\.js\?v=([0-9.]+)", page)
    print(f"在线面板资产版本戳 = {m.group(1) if m else '?'}")

    parts = [grab(js_src, n) for n in ("fmtInt", "normalizeBaseUrl", "parseCount", "validateConfig")]
    cfg = json.loads(fetch(LIVE + "/api/config"))["config"]

    def run(cfg_obj, label: str):
        script = (
            "const DEEPSEEK_MAX_OUTPUT = 384 * 1024;\n"
            "const state = { autoFixNotes: [] };\n"
            + "\n".join(parts) + "\n"
            + "const cfg = " + json.dumps(cfg_obj, ensure_ascii=False) + ";\n"
            + "const issues = validateConfig(cfg);\n"
            + "console.log(JSON.stringify({errors: issues.filter(i=>i.level==='error'),"
              " warns: issues.filter(i=>i.level==='warn').length,"
              " warnMsgs: issues.filter(i=>i.level==='warn').map(i=>i.message)}));\n"
        )
        out = subprocess.run([node, "-e", script], capture_output=True, text=True,
                             encoding="utf-8", errors="replace")
        if out.returncode != 0:
            print(f"[{label}] node 失败: {out.stderr.strip()[:300]}")
            return None
        res = json.loads(out.stdout.strip())
        print(f"[{label}] error={len(res['errors'])} warn={res['warns']}")
        for e in res["errors"]:
            print(f"    ERROR {e.get('field')}: {e.get('message')}")
        for w in res.get("warnMsgs", []):
            print(f"    warn  {w}")
        return res

    print("—— 现状配置（面板当前会显示什么）——")
    run(cfg, "current")

    print("—— 其余「只报 error」的触发器（确认边界没被放宽过头）——")
    cases = {
        "port=70000": lambda c: c.update({"port": 70000}),
        "host=''": lambda c: c.update({"host": ""}),
        "upstreams 清空": lambda c: c.update({"upstreams": {}}),
        "baseUrl 留空": lambda c: c["upstreams"][next(iter(c["upstreams"]))].update({"baseUrl": ""}),
        "retentionDays=-1": lambda c: c.update({"retentionDays": -1}),
        "modelsCacheSeconds=-1": lambda c: c.update({"modelsCacheSeconds": -1}),
    }
    for label, mutate in cases.items():
        one = json.loads(json.dumps(cfg))
        if one.get("upstreams"):
            mutate(one)
        run(one, label)

    scheme_less = json.loads(json.dumps(cfg))
    ups = scheme_less.get("upstreams") or {}
    if ups:
        first = next(iter(ups))
        ups[first]["baseUrl"] = "api.deepseek.com/v1"
        print(f"—— 模拟：把上游 {first} 的 Base URL 写成缺协议的 api.deepseek.com/v1 ——")
        run(scheme_less, "raw(scheme-less)")
        fixed = json.loads(json.dumps(scheme_less))
        fixups = fixed.get("upstreams") or {}
        for n, u in fixups.items():
            u["baseUrl"] = js_via_node(node, parts, u.get("baseUrl", ""))
        run(fixed, "after-normalize")
        print(f"    -> 归一化后 {first}.baseUrl = {fixups[first]['baseUrl']}")
    return 0


def js_via_node(node: str, parts: list[str], raw: str) -> str:
    script = ("\n".join(p for p in parts if "normalizeBaseUrl" in p) + "\n"
              + "console.log(normalizeBaseUrl(" + json.dumps(raw, ensure_ascii=False) + "));\n")
    out = subprocess.run([node, "-e", script], capture_output=True, text=True,
                         encoding="utf-8", errors="replace")
    return out.stdout.strip()


if __name__ == "__main__":
    sys.exit(main())
