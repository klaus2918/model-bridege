# -*- coding: utf-8 -*-
"""模型级覆盖（modelOverrides）：精确/通配匹配、limits 解析与 capabilities 三级合并。"""
from __future__ import annotations

from modelgate.config import (effective_upstreams, model_override_for,
                              normalize_upstream, resolve_model_capabilities,
                              resolve_model_limits)


def _up(**extra) -> dict:
    base = {"protocol": "openai", "baseUrl": "https://api.deepseek.com/v1"}
    base.update(extra)
    return normalize_upstream("ds", base)


def test_exact_wins_then_longest_wildcard():
    up = _up(modelOverrides={
        "*": {"limits": {"maxOutputTokens": 16000}},
        "deepseek-chat*": {"limits": {"maxOutputTokens": 8000}},
        "deepseek-reasoner": {"limits": {"maxOutputTokens": 65536}},
    })
    assert resolve_model_limits(up, "deepseek-reasoner")["maxOutputTokens"] == 65536
    assert resolve_model_limits(up, "deepseek-chat-0324")["maxOutputTokens"] == 8000
    assert resolve_model_limits(up, "gpt-4o")["maxOutputTokens"] == 16000


def test_unmatched_model_falls_back_to_upstream_limits():
    up = _up()
    limits = resolve_model_limits(up, "deepseek-chat")
    assert limits["maxOutputTokens"] == 384 * 1024
    assert limits["maxContextTokens"] == 1024 * 1024


def test_multi_wildcard_is_ignored():
    up = _up(modelOverrides={"a*b*c": {"limits": {"maxOutputTokens": 1}}})
    assert model_override_for(up, "axbxc") == (None, None)


def test_empty_model_never_matches():
    up = _up(modelOverrides={"*": {"limits": {"maxOutputTokens": 1}}})
    assert model_override_for(up, "") == (None, None)


def test_override_without_limits_keeps_upstream():
    up = _up(modelOverrides={"deepseek-chat": {"capabilities": {"vision": True}}})
    assert resolve_model_limits(up, "deepseek-chat")["maxOutputTokens"] == 384 * 1024


def test_model_capabilities_merge_profile_upstream_override():
    up = _up(compat={"capabilities": {"vision": True}},
             modelOverrides={"deepseek-reasoner": {
                 "capabilities": {"cache": True, "vision": False}}})
    caps = resolve_model_capabilities(up, "deepseek-reasoner")
    assert caps["thinking"] is True
    assert caps["cache"] is True
    assert caps["vision"] is False
    base = resolve_model_capabilities(up, "deepseek-chat")
    assert base["vision"] is True
    assert base["thinking"] is True


def test_effective_view_exposes_model_overrides():
    up = _up(modelOverrides={"deepseek-chat": {"limits": {"maxOutputTokens": 1}}})
    row = effective_upstreams([up])[0]
    assert "deepseek-chat" in row["modelOverrides"]
    assert "capabilities" in row
