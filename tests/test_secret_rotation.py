# -*- coding: utf-8 -*-
"""密钥写入接口回归：写 .env 保留其它行、立即生效、不回显、拒绝无 env 引用的上游。"""
from __future__ import annotations

import os

from modelgate import config as cfg_mod
from modelgate.config import load_env_file, write_env_var


def test_write_env_var_preserves_other_lines(tmp_path):
    env_file = tmp_path / ".env"
    env_file.write_text("# 注释行\nOTHER=keep-me\n\nMG_TEST_WRITE=old\n", encoding="utf-8")
    path = write_env_var({"envFile": str(env_file)}, "MG_TEST_WRITE", "new-value")
    assert path == env_file
    text = env_file.read_text(encoding="utf-8")
    assert "# 注释行" in text
    assert "OTHER=keep-me" in text
    assert "MG_TEST_WRITE=new-value" in text
    assert "MG_TEST_WRITE=old" not in text


def test_write_env_var_appends_when_missing(tmp_path):
    env_file = tmp_path / ".env"
    env_file.write_text("A=1\n", encoding="utf-8")
    write_env_var({"envFile": str(env_file)}, "NEW_KEY", "v")
    assert "A=1" in env_file.read_text(encoding="utf-8")
    assert "NEW_KEY=v" in env_file.read_text(encoding="utf-8")


def test_write_env_var_refuses_when_envfile_disabled(tmp_path):
    try:
        write_env_var({"envFile": False}, "X", "y")
    except ValueError as exc:
        assert "envFile" in str(exc)
    else:  # pragma: no cover
        raise AssertionError("envFile=false 时应拒绝写入")


def test_secret_endpoint_writes_and_applies(bridge_factory, tmp_path):
    name = "MG_TEST_SECRET_KEY"
    old = os.environ.get(name)
    env_file = tmp_path / "secret.env"
    env_file.write_text("OTHER=1\n", encoding="utf-8")
    try:
        os.environ.pop(name, None)
        cfg_mod._ENV_FILE_VARS.discard(name)

        from conftest import make_config

        config = make_config("http://127.0.0.1:1")
        config["envFile"] = str(env_file)
        config["upstreams"] = {
            "go": {"protocol": "openai", "baseUrl": "http://127.0.0.1:1",
                   "apiKeyEnv": name, "models": ["m"]},
        }
        handle = bridge_factory(config)
        load_env_file(config, refresh=True)

        resp = handle.client.post(f"{handle.base}/api/upstreams/secret",
                                  json={"name": "go", "apiKey": "brand" + "-new-key"})
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["envName"] == name
        assert body["applied"] is True
        # 不回显密钥
        assert "brand-new-key" not in resp.text

        text = env_file.read_text(encoding="utf-8")
        assert f"{name}=brand-new-key" in text
        assert "OTHER=1" in text  # 其它行保留

        go = next(u for u in handle.bridge.upstreams if u["name"] == "go")
        assert go["apiKey"] == "brand-new-key", "写完后应热更新到运行态"
        assert go["apiKeySource"] == "env"
    finally:
        if old is None:
            os.environ.pop(name, None)
            cfg_mod._ENV_FILE_VARS.discard(name)
        else:
            os.environ[name] = old


def test_secret_endpoint_rejects_upstream_without_env(bridge_factory):
    handle = bridge_factory()
    # conftest 的 anth 用 apiKey 字面量，没有 apiKeyEnv
    resp = handle.client.post(f"{handle.base}/api/upstreams/secret",
                              json={"name": "anth", "apiKey": "x"})
    assert resp.status_code == 400
    assert resp.json()["error"]["code"] == "no_env_ref"


def test_secret_endpoint_rejects_unknown_upstream(bridge_factory):
    handle = bridge_factory()
    resp = handle.client.post(f"{handle.base}/api/upstreams/secret",
                              json={"name": "nope", "apiKey": "x"})
    assert resp.status_code == 404
