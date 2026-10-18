# -*- coding: utf-8 -*-
"""面板静态页与本地 REST 的冒烟（配置页 / 用量页所依赖的接口与资源）。"""
from __future__ import annotations

import json

import httpx

from conftest import make_config


def test_ui_serves_index_js_css(bridge_factory):
    handle = bridge_factory()

    index = httpx.get(f"{handle.base}/ui/", timeout=10)
    assert index.status_code == 200
    assert index.headers["content-type"].startswith("text/html")
    assert "Model-Gate" in index.text
    assert 'data-tab="usage"' in index.text  # 用量页签
    assert 'data-tab="config"' in index.text  # 配置页签
    assert "app.js" in index.text and "style.css" in index.text

    js = httpx.get(f"{handle.base}/ui/app.js", timeout=10)
    assert js.status_code == 200
    assert js.headers["content-type"].startswith("application/javascript")
    assert "/api/usage/summary" in js.text
    assert "/api/config" in js.text

    css = httpx.get(f"{handle.base}/ui/style.css", timeout=10)
    assert css.status_code == 200
    assert css.headers["content-type"].startswith("text/css")
    assert "--brand" in css.text


def test_ui_assets_are_never_browser_cached(bridge_factory):
    """面板资产必须禁缓存。

    这个坑真实发生过：代码改了、服务端也是新文件，但浏览器按启发式规则继续用旧的
    app.js/style.css，用户看到的是「用量界面毫无改进」。
    """
    handle = bridge_factory()
    for path in ("/ui/", "/ui/app.js", "/ui/style.css"):
        resp = httpx.get(f"{handle.base}{path}", timeout=10)
        assert resp.status_code == 200, path
        cache_control = resp.headers.get("cache-control", "")
        assert "no-store" in cache_control, f"{path} 缺少 no-store：{cache_control!r}"
        assert resp.headers.get("pragma") == "no-cache", path


def test_ui_rejects_path_traversal(bridge_factory):
    handle = bridge_factory()
    for evil in ("/ui/../config.py", "/ui/..%2F..%2Fbridge.config.json",
                 "/ui/%2e%2e/proxy.py"):
        res = httpx.get(f"{handle.base}{evil}", timeout=10,
                        follow_redirects=False)
        assert res.status_code in (404, 400), f"{evil} → {res.status_code}"
        # 绝不能把源码或配置泄漏出去
        assert "def " not in res.text
        assert "apiKey" not in res.text


def test_ui_missing_page_404(bridge_factory):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/ui/nope.html", timeout=10)
    assert res.status_code == 404
    assert res.json()["error"]["code"] == "not_implemented" or \
           res.json()["error"]["code"] == "upstream_failed"


def test_api_upstreams_and_reload(bridge_factory):
    handle = bridge_factory()

    ups = httpx.get(f"{handle.base}/api/upstreams", timeout=10)
    assert ups.status_code == 200
    rows = ups.json()["upstreams"]
    assert [r["name"] for r in rows] == ["anth", "oai", "cc"]
    assert rows[0]["keyConfigured"] is True
    assert rows[0]["keySource"] == "literal"
    # 密钥明文绝不外泄
    assert "test-key" not in ups.text

    reload_res = httpx.post(f"{handle.base}/api/config/reload", json={}, timeout=10)
    assert reload_res.status_code == 200
    assert reload_res.json()["ok"] is True
    assert reload_res.json()["upstreams"] == 3


def test_api_unknown_endpoint_404(bridge_factory):
    handle = bridge_factory()
    res = httpx.get(f"{handle.base}/api/nope", timeout=10)
    assert res.status_code == 404
    assert res.json()["error"]["code"] == "not_implemented"


def test_upstream_health_probe_reports_status_and_hint(bridge_factory, stub_upstream):
    """连通性自检：可用的报可用，不可用的必须带可操作建议。"""
    handle = bridge_factory()

    # anthropic 上游可用（桩返回 200）
    stub_upstream.set("/v1/messages", {
        "id": "m", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }, status=200)
    # commandcode 上游余额不足
    stub_upstream.set("/alpha/generate",
                      json.dumps({"success": False, "error": {
                          "code": "BAD_REQUEST", "status": 400,
                          "message": "You have insufficient credits"}}),
                      status=400)
    # openai 上游 401
    stub_upstream.set("/v1/chat/completions",
                      {"error": {"message": "Invalid API key."}}, status=401)

    res = httpx.get(f"{handle.base}/api/upstreams/health?timeout=5", timeout=30)
    assert res.status_code == 200, res.text
    rows = {r["name"]: r for r in res.json()["rows"]}
    assert set(rows) >= {"anth", "oai", "cc"}

    assert rows["anth"]["ok"] is True
    assert rows["anth"]["status"] == 200

    assert rows["oai"]["ok"] is False
    assert rows["oai"]["status"] == 401
    assert rows["oai"]["hint"], "401 必须给出可操作建议"
    assert "密钥" in rows["oai"]["hint"] or "apiKey" in rows["oai"]["hint"]

    assert rows["cc"]["ok"] is False
    assert rows["cc"]["hint"], "余额不足必须给出可操作建议"
    assert "余额" in rows["cc"]["hint"] or "充值" in rows["cc"]["hint"]
    assert rows["oai"]["message"] and rows["cc"]["message"]


def test_upstream_health_probe_unreachable_upstream(bridge_factory):
    """指向不存在的本机地址 → 报不可达并提示改 baseUrl。"""
    cfg = make_config("http://127.0.0.1:1")
    handle = bridge_factory(cfg)
    res = httpx.get(f"{handle.base}/api/upstreams/health?timeout=3", timeout=30)
    assert res.status_code == 200, res.text
    rows = res.json()["rows"]
    assert rows, "应返回上游列表"
    for row in rows:
        assert row["ok"] is False
        assert row["message"], "必须给出失败原因"
        assert row["hint"], "必须给出可操作建议"
def test_config_save_updates_upstreams_and_defaults(bridge_factory, stub_upstream):
    """配置页保存：新增/删除/改默认上游后，运行态立即生效。"""
    handle = bridge_factory()
    cfg = httpx.get(f"{handle.base}/api/config", timeout=10).json()["config"]

    # 删掉 oai，改默认上游为 cc
    cfg["upstreams"].pop("oai")
    for name, up in cfg["upstreams"].items():
        up["default"] = name == "cc"
    cfg["defaultModel"] = "stub-cc"

    saved = httpx.post(f"{handle.base}/api/config", json={"config": cfg}, timeout=10)
    assert saved.status_code == 200

    health = httpx.get(f"{handle.base}/health", timeout=10).json()
    names = [u["name"] for u in health["upstreams"]]
    assert names == ["anth", "cc"]
    assert health["upstreams"][1]["default"] is True

    # 删掉的上游前缀现在应该 404
    res = httpx.get(f"{handle.base}/oai/v1/models", timeout=10)
    assert res.status_code == 404
    assert "/anth" in res.json()["error"]["message"]
