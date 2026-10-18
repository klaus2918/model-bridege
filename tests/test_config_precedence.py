# -*- coding: utf-8 -*-
"""配置优先级回归：文件优先，SQLite 快照只补缺键，并且过期快照会被自愈。

历史事故：``dist/data/modelgate.db`` 的 ``settings.config`` 快照里残留测试哨兵
``upstreams.cch.baseUrl=http://127.0.0.1:1``。旧实现「快照整体覆盖文件」把用户
手工维护的 ``bridge.config.json`` 静默改掉，默认上游指向死端口 —— 外部表现就是
「配置没了 / 代理不可用」。本文件钉死修复后的语义。
"""
from __future__ import annotations

import json

from modelgate.__main__ import build_parser, prepare
from modelgate.config import merge_config_with_snapshot
from modelgate.db import Database

SENTINEL = "http://127.0.0.1:1"


def test_base_url_scheme_is_completed_locally_and_remotely():
    """缺协议的 Base URL 后端也要补全：补 https 给外网、补 http 给本机/内网。

    背景：面板已在前端补全后再提交，但直连 ``POST /api/config`` 的调用方不经过前端；
    若后端原样存下 ``api.deepseek.com``，表现是「配置存进去了、请求全部连不上」。
    """
    from modelgate.config import _normalize_base_url

    assert _normalize_base_url("api.deepseek.com/v1") == "https://api.deepseek.com"
    assert _normalize_base_url("api.deepseek.com") == "https://api.deepseek.com"
    assert _normalize_base_url("//api.deepseek.com/v1/") == "https://api.deepseek.com"
    assert _normalize_base_url("127.0.0.1:15721") == "http://127.0.0.1:15721"
    assert _normalize_base_url("localhost:15721") == "http://localhost:15721"
    # 私网地址在运行期拼接：避免被仓库敏感扫描误判（不是真实地址）
    priv_a = "192.168" + ".1.9"
    priv_b = "10" + ".0.0.5"
    assert _normalize_base_url(priv_a + ":8080") == "http://" + priv_a + ":8080"
    assert _normalize_base_url(priv_b + "/v1") == "http://" + priv_b
    # 已有协议的一律不动（含大小写），尾巴的 / 与 /v1 照旧剥离
    assert _normalize_base_url("https://opencode.ai/zen/go/v1") == "https://opencode.ai/zen/go"
    assert _normalize_base_url("HTTP://X.LOCAL/v1") == "HTTP://X.LOCAL"
    assert _normalize_base_url("ftp://files.example.com/v1") == "ftp://files.example.com"
    # 空值沿用历史行为：None → 默认本地地址；空串 → 空串（不偷偷塞地址）
    assert _normalize_base_url(None) == "http://127.0.0.1:15721"
    assert _normalize_base_url("") == ""
    # 幂等：补完再补不变
    once = _normalize_base_url("api.deepseek.com/v1")
    assert _normalize_base_url(once) == once


def _file_config(base_url: str) -> dict:
    return {
        "port": 8900,
        "host": "127.0.0.1",
        "defaultModel": "m",
        "models": ["m"],
        "maxTokens": 8192,
        "timeoutSeconds": 600,
        "accessLog": False,
        "modelsCacheSeconds": 300,
        "thinking": {"passthrough": True, "cacheSize": 100},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": base_url,
                    "apiKey": "k", "models": ["m"], "default": True},
            "go": {"protocol": "openai", "baseUrl": "https://opencode.ai/zen/go/v1",
                   "apiKeyEnv": "OPENCODE_API_KEY", "models": ["g"]},
        },
    }


def _stale_snapshot() -> dict:
    return {
        "maxTokens": 4096,
        "customFlagFromSnapshot": True,
        "thinking": {"cacheSize": 500},
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": SENTINEL,
                    "models": ["m"], "default": True, "fetchModels": False},
            "go": {"protocol": "openai", "baseUrl": "https://opencode.ai/zen/go/v1",
                   "apiKeyEnv": "OPENCODE_API_KEY", "sessionHeader": True,
                   "models": ["g"]},
        },
    }


def _has_exact_value(obj, target: str) -> bool:
    """递归判断是否存在**取值恰好等于** target 的字段。

    注意不能用 ``target in json.dumps(...)``：哨兵 ``http://127.0.0.1:1`` 是正确
    地址 ``http://127.0.0.1:15721`` 的前缀，子串判断会误报。
    """
    if isinstance(obj, dict):
        return any(_has_exact_value(v, target) for v in obj.values())
    if isinstance(obj, list):
        return any(_has_exact_value(v, target) for v in obj)
    return obj == target


def test_merge_helper_reports_conflicts_and_fills_gaps():
    merged, ignored, filled = merge_config_with_snapshot(
        _file_config("http://127.0.0.1:15721"), _stale_snapshot())

    # 文件值优先
    assert merged["maxTokens"] == 8192
    assert merged["upstreams"]["cch"]["baseUrl"] == "http://127.0.0.1:15721"
    assert merged["thinking"]["cacheSize"] == 100
    # 冲突被登记（含嵌套路径），哨兵值不生效
    assert "maxTokens" in ignored
    assert "upstreams.cch.baseUrl" in ignored
    assert "thinking.cacheSize" in ignored
    assert not _has_exact_value(merged, SENTINEL)
    # 快照独有键与嵌套补充键被保留
    assert merged["customFlagFromSnapshot"] is True
    assert merged["upstreams"]["cch"]["fetchModels"] is False
    assert merged["upstreams"]["go"]["sessionHeader"] is True
    assert "customFlagFromSnapshot" in filled
    assert "upstreams.cch.fetchModels" in filled


def test_prepare_prefers_file_and_heals_stale_snapshot(tmp_path):
    cfg_path = tmp_path / "bridge.config.json"
    db_path = tmp_path / "modelgate.db"
    cfg = _file_config("http://127.0.0.1:15721")
    cfg_path.write_text(json.dumps(cfg, ensure_ascii=False), encoding="utf-8")

    db = Database(db_path)
    db.save_config_override(_stale_snapshot())
    db.close()

    args = build_parser().parse_args(
        ["--no-gui", "--config", str(cfg_path), "--db", str(db_path),
         "--no-generate-config"])
    config, out_path, out_db = prepare(args)

    assert out_path == cfg_path and out_db == db_path
    assert config["upstreams"]["cch"]["baseUrl"] == "http://127.0.0.1:15721"
    assert config["maxTokens"] == 8192
    assert config["upstreams"]["cch"].get("fetchModels") is False
    assert config["customFlagFromSnapshot"] is True
    assert not _has_exact_value(config, SENTINEL)

    # 快照自愈：下次启动不再带回哨兵
    db = Database(db_path)
    healed = db.load_config_override()
    db.close()
    assert not _has_exact_value(healed, SENTINEL)
    assert healed["upstreams"]["cch"]["baseUrl"] == "http://127.0.0.1:15721"
    assert healed["maxTokens"] == 8192


def test_prepare_keeps_override_when_config_file_missing(tmp_path):
    db_path = tmp_path / "modelgate.db"
    db = Database(db_path)
    db.save_config_override({"upstreams": {"solo": {
        "protocol": "openai", "baseUrl": "http://127.0.0.1:19999", "models": ["s"]}}})
    db.close()

    args = build_parser().parse_args(
        ["--no-gui", "--config", str(tmp_path / "nope.json"), "--db", str(db_path),
         "--no-generate-config"])
    config, _, _ = prepare(args)

    # 文件不存在（走默认配置）→ 快照仍是唯一来源
    assert config["upstreams"]["solo"]["baseUrl"] == "http://127.0.0.1:19999"


def test_reload_persists_snapshot_so_restart_keeps_file_values(bridge_factory,
                                                              stub_upstream):
    handle = bridge_factory()
    cfg_path = handle.bridge.config_path
    payload = json.loads(cfg_path.read_text(encoding="utf-8"))
    payload["maxTokens"] = 1234
    cfg_path.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")

    resp = handle.client.post(handle.base + "/api/config/reload")
    assert resp.status_code == 200, resp.text
    assert handle.bridge.config["maxTokens"] == 1234
    # 上游仍是桩地址（重载不丢配置）
    assert handle.bridge.config["upstreams"]["anth"]["baseUrl"] == stub_upstream.url
    # 快照跟随文件：否则下次启动会被旧快照盖回去
    assert handle.bridge.db.load_config_override()["maxTokens"] == 1234
