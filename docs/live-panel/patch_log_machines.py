# -*- coding: utf-8 -*-
"""给 scaffold 生成的面板配置补两台“日志节奏”机器（不改布局）。"""
import json
from pathlib import Path

p = Path(__file__).with_name("model-gate-config.json")
d = json.loads(p.read_text(encoding="utf-8"))

d["machines"]["beat"] = {
    "type": "cycle",
    "period": 3.0,
    "t0": 0.0,
    "values": [
        {"t": "route", "m": "deepseek-flash → go:deepseek-v4.1-flash"},
        {"t": "stream", "m": "200 · 812ms · ttft=214ms"},
        {"t": "usage", "m": "prompt=41 completion=49"},
        {"t": "cache", "m": "模型目录刷新 · 43 models"},
        {"t": "retention", "m": "保留检查 · 无需清理"},
    ],
    "log": {"who": "proxy", "c": "gr"},
}

d["machines"]["http"] = {
    "type": "cycle",
    "period": 2.2,
    "t0": 0.4,
    "values": [
        {"t": "GET", "m": "/health 200"},
        {"t": "GET", "m": "/ui/ 200"},
        {"t": "POST", "m": "/v1/chat/completions 200"},
        {"t": "GET", "m": "/api/usage/summary 200"},
    ],
    "log": {"who": "http", "c": "cy"},
}

p.write_text(json.dumps(d, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
print("patched:", p)
