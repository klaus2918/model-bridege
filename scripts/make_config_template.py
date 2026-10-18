# -*- coding: utf-8 -*-
"""生成脱敏配置模板 bridge.config.example.json（无密文，可安全提交/分享）。

从当前 bridge.config.json 派生：保留全部结构与说明，密钥字段统一替换成占位引用。
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "bridge.config.json"
DST = ROOT / "bridge.config.example.json"

# 各上游推荐的密钥引用方式（不改变原语义，只把「怎么拿到 key」写清楚）
PLACEHOLDERS = {
    "go": {
        "apiKeyEnv": "OPENCODE_API_KEY",
        "_comment": "从环境变量读取：密钥放在 ~/.jcode/.env（该文件已被 .gitignore 拦截）",
    },
    "commandcode": {
        "apiKeyEnv": "COMMAND_CODE_API_KEY",
        "apiKeyFile": "~/.commandcode/auth.json",
        "_comment": "先尝试环境变量，再回落到 auth.json；两者都不含明文",
    },
}


def main() -> int:
    cfg = json.loads(SRC.read_text(encoding="utf-8"))

    ups: dict = {}
    for name, up in (cfg.get("upstreams") or {}).items():
        entry = dict(up)
        entry.pop("apiKey", None)          # 绝不把明文写进模板
        if name in PLACEHOLDERS:
            entry.update({k: v for k, v in PLACEHOLDERS[name].items()
                          if k != "_comment"})
        ups[name] = entry

    example = {
        "_doc": (
            "Model-Gate 配置模板。复制为 bridge.config.json 后按需修改；"
            "切勿把明文 apiKey 写进本文件——请用 apiKeyEnv 或 apiKeyFile 引用。"
        ),
        "_keys": (
            "密钥三选一：apiKey（明文，不推荐）/ apiKeyEnv（环境变量名，"
            "密钥放 ~/.jcode/.env）/ apiKeyFile（本地 JSON 的 apiKey 字段，"
            "如 ~/.commandcode/auth.json）。工具只读不写这些文件。"
        ),
        "port": cfg.get("port", 8900),
        "host": cfg.get("host", "127.0.0.1"),
        "defaultModel": cfg.get("defaultModel", ""),
        "models": cfg.get("models", []),
        "maxTokens": cfg.get("maxTokens", 8192),
        "timeoutSeconds": cfg.get("timeoutSeconds", 600),
        "accessLog": cfg.get("accessLog", True),
        "modelsCacheSeconds": cfg.get("modelsCacheSeconds", 300),
        "aliases": cfg.get("aliases", {}),
        "thinking": cfg.get("thinking", {"passthrough": True, "cacheSize": 500}),
        "tools": cfg.get("tools", {"downgradeForcedChoice": True}),
        "upstreams": ups,
    }

    DST.write_text(json.dumps(example, ensure_ascii=False, indent=2) + "\n",
                   encoding="utf-8")
    size = DST.stat().st_size
    print(f"已生成 {DST.name}（{size} bytes）")

    # 自检：模板里不得出现任何疑似明文 key
    text = DST.read_text(encoding="utf-8")
    if '"apiKey"' in text:
        print("!! 模板仍含 apiKey 字段，已中止")
        return 1
    leaked = [name for name, up in ups.items() if up.get("apiKey")]
    if leaked:
        print(f"!! 模板含明文的上游：{leaked}")
        return 1
    print("自检通过：模板零明文密钥，仅含 apiKeyEnv / apiKeyFile 引用")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
