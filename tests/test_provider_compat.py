# -*- coding: utf-8 -*-
"""provider 兼容能力（compat）与结构化配置诊断。"""
from __future__ import annotations

from modelgate.config import (effective_upstreams, normalize_upstream,
                              upstreams_of, validate_config)


def test_model_override_diagnostics_codes():
    config = _config({
        "ds": {
            "protocol": "openai",
            "baseUrl": "https://api.deepseek.com/v1",
            "models": ["deepseek-chat"],
            "compat": {"profile": "nope", "capabilities": {"vision": "yes"}},
            "modelOverrides": {
                "a*b*c": {},
                "ghost": {"limits": {"maxOutputTokens": 1000, "maxContextTokens": 500}},
                "deepseek-chat": {"limits": {"maxOutputTokens": "8000"},
                                  "capabilities": {"thinking": "yes", "bogus": True}},
                "blank": {},
            },
        }
    })
    found = {(item["code"], item["path"]) for item in validate_config(config)}
    assert ("unknown_profile", "upstreams.ds.compat.profile") in found
    assert ("invalid_capabilities", "upstreams.ds.compat.capabilities.vision") in found
    assert ("multi_wildcard_override", "upstreams.ds.modelOverrides.a*b*c") in found
    assert ("model_override_unknown_model", "upstreams.ds.modelOverrides.ghost") in found
    assert ("invalid_limit_relation", "upstreams.ds.modelOverrides.ghost.limits") in found
    assert ("invalid_capabilities",
            "upstreams.ds.modelOverrides.deepseek-chat.capabilities.thinking") in found
    assert ("invalid_model_overrides", "upstreams.ds.modelOverrides.blank") in found


def _config(upstreams: dict, **overrides) -> dict:
    config = {
        "host": "127.0.0.1",
        "port": 8900,
        "maxTokens": 8192,
        "defaultModel": "m",
        "thinking": {"passthrough": True},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": upstreams,
    }
    config.update(overrides)
    return config


def test_opencode_preset_has_session_header():
    up = normalize_upstream("go", {
        "protocol": "openai",
        "baseUrl": "https://opencode.ai/zen/go/v1",
        "models": ["m"],
    })
    assert up["compat"]["profile"] == "opencode"
    assert up["compat"]["session"]["header"] == "x-opencode-session"
    assert up["compat"]["endpoint"]["chat"] == "/v1/chat/completions"


def test_commandcode_preset_declares_wire_specifics():
    up = normalize_upstream("cc", {
        "protocol": "commandcode",
        "baseUrl": "https://api.commandcode.ai",
        "models": [],
    })
    compat = up["compat"]
    assert compat["session"] == {"header": "x-session-id", "idStyle": "sess16"}
    assert compat["endpoint"]["chat"] == "/alpha/generate"
    assert compat["zdr"] is True
    assert compat["versionHeader"] == "x-command-code-version"
    assert compat["limits"]["maxOutputTokens"] == 64000


def test_deepseek_preset_sets_limits_and_thinking():
    up = normalize_upstream("ds", {
        "protocol": "openai",
        "baseUrl": "https://api.deepseek.com/v1",
        "models": ["m"],
    })
    assert up["compat"]["profile"] == "deepseek"
    assert up["compat"]["limits"]["maxOutputTokens"] == 384 * 1024
    assert up["compat"]["limits"]["maxContextTokens"] == 1024 * 1024
    assert up["compat"]["thinking"]["passthrough"] is True


def test_explicit_compat_overrides_preset_and_legacy():
    up = normalize_upstream("go", {
        "protocol": "openai",
        "baseUrl": "https://opencode.ai/zen/go/v1",
        "sessionHeader": True,
        "compat": {
            "session": False,
            "auth": {"scheme": "header", "header": "x-api-key", "prefix": ""},
            "headers": {"x-client-name": "model-gate"},
            "endpoint": {"chat": "/custom/chat"},
            "limits": {"maxOutputTokens": 12345},
        },
    })
    compat = up["compat"]
    assert compat["session"]["header"] == ""
    assert compat["auth"]["scheme"] == "header"
    assert compat["headers"]["x-client-name"] == "model-gate"
    assert compat["endpoint"]["chat"] == "/custom/chat"
    assert compat["limits"]["maxOutputTokens"] == 12345


def test_global_switches_are_final_constraint():
    ups = upstreams_of(_config({
        "cc": {"protocol": "commandcode", "baseUrl": "https://api.commandcode.ai"},
        "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721"},
    }, thinking={"passthrough": False}, tools={"downgradeForcedChoice": False}))
    for up in ups:
        assert up["compat"]["thinking"]["passthrough"] is False
        assert up["compat"]["toolChoice"]["downgradeForced"] is False


def test_validation_reports_actionable_errors():
    issues = validate_config(_config({
        "bad": {
            "protocol": "magic",
            "baseUrl": "",
            "compat": {
                "auth": {"scheme": "nope"},
                "session": {"header": "bad header"},
                "endpoint": {"chat": "missing-slash"},
                "limits": {"maxOutputTokens": 200, "maxContextTokens": 100},
                "unknownKey": True,
            },
        },
    }))
    codes = {item["code"] for item in issues}
    assert "invalid_base_url" in codes
    assert "unsupported_protocol" in codes
    assert "invalid_auth_scheme" in codes
    assert "invalid_header_name" in codes
    assert "invalid_endpoint" in codes
    assert "invalid_limits" in codes
    assert "unknown_compat_field" in codes
    assert all(item["level"] == "error" or item["level"] == "warn" for item in issues)


def test_validation_warns_on_provider_field_mismatch_and_unknown_fields():
    issues = validate_config(_config({
        "go": {
            "protocol": "openai",
            "baseUrl": "https://opencode.ai/zen/go/v1",
            "models": ["m"],
            "cliVersion": "1.2.3",
            "mystery": True,
        },
    }))
    by_code = {}
    for item in issues:
        by_code.setdefault(item["code"], []).append(item)
    assert "field_not_applicable" in by_code
    assert "unknown_field" in by_code
    assert all(item["level"] == "warn" for item in by_code["unknown_field"])


def test_valid_config_has_no_errors():
    issues = validate_config(_config({
        "cch": {
            "protocol": "anthropic",
            "baseUrl": "http://127.0.0.1:15721",
            "models": ["m"],
            "default": True,
        },
    }))
    assert not [item for item in issues if item["level"] == "error"]


def test_effective_view_marks_placeholder_key_as_not_configured():
    config = _config({
        "go": {"protocol": "openai", "baseUrl": "https://opencode.ai/zen/go/v1"},
    })
    rows = effective_upstreams(upstreams_of(config))
    assert rows[0]["keyConfigured"] is False
    assert rows[0]["profile"] == "opencode"
