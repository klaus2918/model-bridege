# -*- coding: utf-8 -*-
"""明文密钥硬约束回归：非空 apiKey 绝不写入配置文件与 settings。"""
from __future__ import annotations

import json

import httpx

from conftest import bridge_factory, make_config


def _read_config_file(bridge) -> dict:
    text = bridge.config_path.read_text(encoding="utf-8")
    return json.loads(text)


def test_non_empty_api_key_not_written_to_file(bridge_factory):
    """填了明文 apiKey：文件与 settings 都不落明文，但内存仍可用。"""
    cfg = make_config("http://127.0.0.1:1")
    handle = bridge_factory(cfg)

    payload = json.loads(handle.bridge.config_path.read_text(encoding="utf-8"))
    # 给一条上游填明文 key，另一条留空（空串=不发鉴权头，语义不同）
    # 拼接构造：避免被仓库敏感扫描当成真实密钥
    fake_key = "sk-" + "super-secret-value-123"
    payload["upstreams"]["anth"]["apiKey"] = fake_key
    payload["upstreams"]["zen"] = {
        "protocol": "openai", "baseUrl": "http://127.0.0.1:1",
        "apiKey": "", "models": ["m"], "fetchModels": False,
    }
    res = httpx.post(f"{handle.base}/api/config", json={"config": payload},
                     timeout=15)
    assert res.status_code == 200, res.text

    # 1) 文件里没有明文
    on_disk = _read_config_file(handle.bridge)
    assert on_disk["upstreams"]["anth"].get("apiKey") in (None, ""), \
        "明文 apiKey 落盘了"
    # 2) 空串语义保留
    assert on_disk["upstreams"]["zen"]["apiKey"] == "", "空串语义被破坏"
    # 3) settings（运行时事实源）也没明文
    override = handle.bridge.db.load_config_override()
    assert override["upstreams"]["anth"].get("apiKey") in (None, ""), \
        "明文 apiKey 进了 settings"
    # 4) 但运行态仍持有该 key，上游不失效
    anth = next(u for u in handle.bridge.upstreams if u["name"] == "anth")
    assert anth.get("apiKey") == fake_key, "运行态丢了 key"
    assert anth.get("apiKeySource") == "literal"
    # 5) 提示语明确告知
    note = res.json().get("note") or ""
    assert "内存" in note, f"未提示仅内存保留: {note}"


def test_key_reference_fields_survive(bridge_factory):
    """apiKeyEnv / apiKeyFile 引用方式不受影响。"""
    cfg = make_config("http://127.0.0.1:1")
    cfg["upstreams"]["anth"]["apiKeyEnv"] = "SOME_ENV_KEY"
    cfg["upstreams"]["anth"]["apiKeyFile"] = "~/some/auth.json"
    handle = bridge_factory(cfg)

    payload = json.loads(handle.bridge.config_path.read_text(encoding="utf-8"))
    res = httpx.post(f"{handle.base}/api/config", json={"config": payload},
                     timeout=15)
    assert res.status_code == 200, res.text

    on_disk = _read_config_file(handle.bridge)
    assert on_disk["upstreams"]["anth"]["apiKeyEnv"] == "SOME_ENV_KEY"
    assert on_disk["upstreams"]["anth"]["apiKeyFile"] == "~/some/auth.json"


def test_placeholder_key_is_not_treated_as_secret(bridge_factory):
    """any-key 占位（无密钥配置）不应被当成明文密钥处理。"""
    cfg = make_config("http://127.0.0.1:1")
    # 关键：保存前运行态就不能有明文，否则 prior 快照会捕获到它们
    for up in cfg["upstreams"].values():
        up.pop("apiKey", None)
        up.pop("apiKeyEnv", None)
        up.pop("apiKeyFile", None)
    handle = bridge_factory(cfg)
    payload = json.loads(handle.bridge.config_path.read_text(encoding="utf-8"))
    for up in payload["upstreams"].values():
        up.pop("apiKey", None)
        up.pop("apiKeyEnv", None)
        up.pop("apiKeyFile", None)
    res = httpx.post(f"{handle.base}/api/config", json={"config": payload},
                     timeout=15)
    assert res.status_code == 200, res.text
    assert "内存" not in (res.json().get("note") or ""), \
        "无密钥时不应提示内存保留"
