# -*- coding: utf-8 -*-
"""provider 注册表单测 + golden fixtures 双端比对 + 旧配置夹具加载。

fixtures 是任务 4 的共同依据：Python（本文件）与 Rust（provider_profiles.rs /
config.rs 的 #[cfg(test)]）读取同一份 tests/fixtures 文件，不允许各自维护预期值。
"""
from __future__ import annotations

import json
from pathlib import Path

from modelgate import provider_profiles
from modelgate.config import _compat_preset, upstreams_of, validate_config

REPO = Path(__file__).resolve().parent.parent
GOLDEN = json.loads(
    (REPO / "tests" / "fixtures" / "provider_profiles_golden.json").read_text(encoding="utf-8"))
LEGACY = json.loads(
    (REPO / "tests" / "fixtures" / "legacy_config.json").read_text(encoding="utf-8"))

COMPAT_KEYS = ("profile", "auth", "session", "headers", "endpoint", "thinking",
               "toolChoice", "limits", "zdr", "versionHeader", "environmentHeader")
CAPABILITY_KEYS = {"stream", "tools", "vision", "thinking", "cache", "systemPrompt"}
EXPECTED_PROFILE_ORDER = ["openai", "opencode", "anthropic", "deepseek", "commandcode"]


def _cases() -> list[dict]:
    cases = GOLDEN.get("cases") or []
    assert cases, "golden fixtures 为空"
    return cases


def test_registry_exposes_five_profiles_in_order():
    assert provider_profiles.profile_ids() == EXPECTED_PROFILE_ORDER
    for pid in EXPECTED_PROFILE_ORDER:
        assert provider_profiles.is_known_profile(pid)
    assert not provider_profiles.is_known_profile("mystery")


def test_profiles_meta_shape_matches_panel_contract():
    rows = {row["id"]: row for row in provider_profiles.profiles_meta()}
    assert list(rows) == EXPECTED_PROFILE_ORDER
    for pid, row in rows.items():
        assert row["label"], pid
        assert isinstance(row["description"], str)
        assert set(row["capabilities"]) == CAPABILITY_KEYS


def test_detect_profile_covers_explicit_domain_protocol_and_unknown():
    detect = provider_profiles.detect_profile
    assert detect("", "openai", "https://api.deepseek.com/v1") == "deepseek"
    assert detect("", "openai", "https://opencode.ai/zen/go/v1") == "opencode"
    assert detect("", "commandcode", "https://api.commandcode.ai") == "commandcode"
    assert detect("", "openai", "https://api.example.com/v1") == "openai"
    assert detect("", "anthropic", "http://127.0.0.1:15721") == "anthropic"
    assert detect("deepseek", "anthropic", "https://api.deepseek.com/anthropic") == "deepseek"
    assert detect("", "anthropic", "https://api.deepseek.com/anthropic") == "deepseek"
    assert detect("mystery", "openai", "https://api.example.com/v1") == "mystery"


def test_preset_matches_golden_fixtures():
    for case in _cases():
        inp = case["input"]
        live = _compat_preset(inp["profile"], inp["protocol"], inp["baseUrl"])
        got = {key: live.get(key) for key in COMPAT_KEYS}
        assert got == case["expect"], f"{case['name']} 与 fixtures 漂移"
        assert set(live["capabilities"]) == CAPABILITY_KEYS, case["name"]


def test_registry_resolution_matches_golden_fixtures():
    for case in _cases():
        inp = case["input"]
        resolved = provider_profiles.resolve_compat(inp["profile"], inp["protocol"], inp["baseUrl"])
        got = {key: resolved.get(key) for key in COMPAT_KEYS}
        assert got == case["expect"], f"{case['name']} 注册表解析与 fixtures 不一致"


def test_legacy_config_fixture_loads_without_errors():
    issues = validate_config(LEGACY)
    errors = [item for item in issues if item["level"] == "error"]
    assert not errors, f"旧配置不应产生 error 级诊断：{errors}"


def test_legacy_config_keeps_old_shorthand_semantics():
    ups = {up["name"]: up for up in upstreams_of(LEGACY)}
    ds = ups["ds"]
    assert ds["compat"]["profile"] == "deepseek"
    assert ds["compat"]["endpoint"]["chat"] == "/v1/messages"
    assert ds["compat"]["limits"]["maxOutputTokens"] == 384 * 1024
    assert ds["cmdZdr"] is True
    zen = ups["zen"]
    assert zen["compat"]["profile"] == "opencode"
    assert zen["compat"]["session"]["header"] == "x-opencode-session"
    assert "mysteryField" not in zen
