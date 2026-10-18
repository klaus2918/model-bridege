# -*- coding: utf-8 -*-
"""密钥过期 / 轮换回归。

背景（真实用户场景）：opencode go 的 API Key 在上游侧过期，代理返回
401 `api_key_expired`。这类故障最容易被误判成「工具坏了」：

1) 提示必须明确说「去新建 key + 改哪里 + 重新登录不会续期」，
   而不是泛泛的「密钥无效」；
2) 改完 ~/.jcode/.env 后应当能「重新载入」立即生效，不必重启程序；
3) 重载只能覆盖「本工具自己从 env 文件注入」的变量，
   不能篡改用户在 shell 里显式设置的环境变量。
"""
from __future__ import annotations

import os

from modelgate import config as cfg_mod
from modelgate.config import load_env_file
from modelgate.proxy import upstream_error_hint

EXPIRED_BODY = ("This API key has expired. Create a new API key in OpenCode "
                "Console and replace it in your client or secret store. "
                "Signing in again will not renew this key.")


def _upstream(source: str = "env") -> dict:
    item = {
        "name": "go",
        "baseUrl": "https://opencode.ai/zen/go/v1",
        "apiKey": "sk-dummy",
        "apiKeySource": source,
        "apiKeyEnv": "MG_TEST_EXPIRED_KEY",
    }
    if source == "file":
        item["apiKeyFile"] = "C:/keys/auth.json"
    return item


def test_expired_key_hint_is_actionable():
    hint = upstream_error_hint(_upstream(), 401, EXPIRED_BODY)
    assert "过期" in hint
    assert "新建 API Key" in hint or "新建 key" in hint
    assert "MG_TEST_EXPIRED_KEY" in hint      # 告诉用户改哪个变量
    assert "重新载入" in hint                  # 免重启
    assert "重新登录不会续期" in hint.replace("：", "").replace("注意", "")
    # 不能退化成「你没配密钥」——那会把用户引向错误动作
    assert "未配置真实密钥" not in hint


def test_invalid_key_hint_points_at_source():
    hint = upstream_error_hint(_upstream("file"), 401,
                               '{"error":{"message":"Invalid API key provided"}}')
    assert "无效" in hint
    assert "apiKey / apiKeyEnv / apiKeyFile" not in hint  # 已能定位到具体来源
    assert "密钥文件" in hint


def test_placeholder_still_gives_config_guidance():
    upstream = {"name": "x", "apiKey": "any-key", "apiKeySource": "none",
                "baseUrl": "https://example.com/v1", "apiKeyEnv": "SOME_ENV"}
    hint = upstream_error_hint(upstream, 401, "unauthorized")
    assert "未配置真实密钥" in hint
    assert "SOME_ENV" in hint


def _restore(name: str, old) -> None:
    if old is None:
        os.environ.pop(name, None)
        cfg_mod._ENV_FILE_VARS.discard(name)
    else:
        os.environ[name] = old


def test_load_env_file_refresh_overrides_file_values(tmp_path):
    name = "MG_TEST_ROTATE_KEY"
    old = os.environ.get(name)
    env_file = tmp_path / "test.env"
    try:
        os.environ.pop(name, None)
        cfg_mod._ENV_FILE_VARS.discard(name)

        env_file.write_text(f"{name}=first-value\n", encoding="utf-8")
        load_env_file({"envFile": str(env_file)})
        assert os.environ[name] == "first-value"

        # 文件更新后：不 refresh 不覆盖（保持旧语义）
        env_file.write_text(f"{name}=second-value\n", encoding="utf-8")
        load_env_file({"envFile": str(env_file)})
        assert os.environ[name] == "first-value"

        # refresh=True 才覆盖「本函数注入过」的变量
        load_env_file({"envFile": str(env_file)}, refresh=True)
        assert os.environ[name] == "second-value"
    finally:
        _restore(name, old)


def test_load_env_file_never_clobbers_shell_variable(tmp_path):
    name = "MG_TEST_SHELL_KEY"
    old = os.environ.get(name)
    env_file = tmp_path / "test.env"
    try:
        os.environ[name] = "from-shell"
        cfg_mod._ENV_FILE_VARS.discard(name)  # 不是本函数注入的
        env_file.write_text(f"{name}=from-file\n", encoding="utf-8")

        load_env_file({"envFile": str(env_file)}, refresh=True)
        assert os.environ[name] == "from-shell", "不得篡改用户在 shell 里设置的值"
    finally:
        _restore(name, old)


def test_reload_endpoint_picks_up_rotated_env_key(bridge_factory, tmp_path):
    """改 env 文件 + POST /api/config/reload → 运行态用上新密钥（免重启）。"""
    name = "MG_TEST_RELOAD_KEY"
    old = os.environ.get(name)
    env_file = tmp_path / "rotate.env"
    try:
        os.environ.pop(name, None)
        cfg_mod._ENV_FILE_VARS.discard(name)
        env_file.write_text(f"{name}=old-key\n", encoding="utf-8")

        from conftest import make_config

        config = make_config("http://127.0.0.1:1")
        config["envFile"] = str(env_file)
        config["upstreams"] = {
            "go": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1",
                   "apiKeyEnv": name, "models": ["m"], "sessionHeader": True},
        }
        handle = bridge_factory(config)
        load_env_file(config)
        resp = handle.client.post(f"{handle.base}/api/config/reload", json={})
        assert resp.status_code == 200, resp.text
        go = next(u for u in handle.bridge.upstreams if u["name"] == "go")
        assert go["apiKey"] == "old-key"
        assert go["apiKeySource"] == "env"

        # 轮换密钥（模拟用户在 OpenCode Console 新建 key 后写回 env 文件）
        env_file.write_text(f"{name}=new-key\n", encoding="utf-8")
        resp = handle.client.post(f"{handle.base}/api/config/reload", json={})
        assert resp.status_code == 200, resp.text
        go = next(u for u in handle.bridge.upstreams if u["name"] == "go")
        assert go["apiKey"] == "new-key", "重新载入后应使用新密钥"
        assert resp.json()["keySources"]["go"] == "env"
    finally:
        _restore(name, old)
