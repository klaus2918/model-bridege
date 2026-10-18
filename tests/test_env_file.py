# -*- coding: utf-8 -*-
"""envFile 缺省语义回归。

历史缺陷：`load_env_file` 曾把缺省值写成 False（禁用），与 Node 版 model-bridge
「缺省读 ~/.jcode/.env，显式 false 才禁用」相反，导致所有依赖 apiKeyEnv 的上游
拿不到 key、上游返回 401 Invalid API key。
"""
from __future__ import annotations

import os
from pathlib import Path

import pytest

from modelgate.config import load_env_file, resolve_api_key, upstreams_of

ENV_TEXT = "MY_TEST_KEY =   \nOPENCODE_TEST_KEY = abc123\n# comment line\n"


@pytest.fixture()
def fake_home(tmp_path, monkeypatch):
    home = tmp_path / "home"
    (home / ".jcode").mkdir(parents=True)
    (home / ".jcode" / ".env").write_text(ENV_TEXT, encoding="utf-8")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    for var in ("OPENCODE_TEST_KEY", "MY_TEST_KEY"):
        os.environ.pop(var, None)
    return home


def test_env_file_loaded_by_default(fake_home):
    """缺省（不配 envFile）必须读 ~/.jcode/.env。"""
    load_env_file({})
    assert os.environ.get("OPENCODE_TEST_KEY") == "abc123"


def test_env_file_blank_value_not_injected(fake_home):
    """空值不注入，避免把上游 key 覆盖成空串。"""
    load_env_file({})
    assert "MY_TEST_KEY" not in os.environ


def test_env_file_does_not_override_existing(fake_home):
    """已有环境变量不被覆盖。"""
    os.environ["OPENCODE_TEST_KEY"] = "from-shell"
    load_env_file({})
    assert os.environ["OPENCODE_TEST_KEY"] == "from-shell"


def test_env_file_false_disables(fake_home):
    """显式 envFile: false 才禁用。"""
    load_env_file({"envFile": False})
    assert "OPENCODE_TEST_KEY" not in os.environ


def test_env_file_custom_path(fake_home, tmp_path):
    """自定义 envFile 路径生效。"""
    custom = tmp_path / "custom.env"
    custom.write_text("CUSTOM_KEY = zzz\n", encoding="utf-8")
    os.environ.pop("CUSTOM_KEY", None)
    load_env_file({"envFile": str(custom)})
    assert os.environ.get("CUSTOM_KEY") == "zzz"


def test_missing_env_file_is_not_an_error(tmp_path, monkeypatch):
    """文件不存在不是错误。"""
    home = tmp_path / "empty"
    home.mkdir()
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    load_env_file({})  # 不应抛异常


def test_api_key_env_upstream_resolves_label(fake_home):
    """端到端：apiKeyEnv 指向 .env 里的变量时，归一化结果是 env 且有真实 key。"""
    os.environ.pop("OPENCODE_TEST_KEY", None)
    cfg = {"upstreams": {"go": {
        "protocol": "openai",
        "baseUrl": "https://example.com",
        "apiKeyEnv": "OPENCODE_TEST_KEY",
        "models": [],
    }}}
    load_env_file(cfg)
    ups = {u["name"]: u for u in upstreams_of(cfg)}
    assert ups["go"]["apiKeySource"] == "env"
    assert ups["go"]["apiKey"] == "abc123"
    assert ups["go"]["apiKey"] != "any-key"


def test_placeholder_key_when_nothing_configured():
    """什么都不配时用 any-key 占位，且 source=none。"""
    key, source = resolve_api_key({"protocol": "openai", "baseUrl": "http://x"})
    assert source == "none"
    assert key == "any-key"
