# -*- coding: utf-8 -*-
"""端口自动降级回归。

背景：用户双击 dist/Model-Gate.exe 时 8900 被旧 Node 桥占用，
旧逻辑直接 return 1 退出 → 窗口从未打开 → 「根本无法使用、配置也没有」。
修复：配置端口不可用时自动向后寻找可用端口，GUI 与 --no-gui 共用。
"""
from __future__ import annotations

import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from modelgate.config import load_config
from modelgate.db import Database
from modelgate.proxy import Bridge, _bind_available, create_server, run_forever

ROOT = Path(__file__).resolve().parent.parent


def _occupy(port: int, exclusive: bool = True) -> socket.socket:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    if exclusive:
        # 必须独占：SO_REUSEADDR 下 Windows 允许在已被占用的端口上 bind 成功，
        # 那样连降级分支都进不去，测试会假绿。
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
    else:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", port))
    sock.listen(1)
    return sock


def _make_bridge(tmp_path, monkeypatch):
    monkeypatch.setattr(Path, "cwd", classmethod(lambda cls: tmp_path))
    cfg = cfg = load_config(ROOT / "bridge.config.json")
    cfg["port"] = 0  # 由调用方指定
    db = Database(tmp_path / "t.db")
    return Bridge(cfg, db, config_path=tmp_path / "bridge.config.json")


def test_bind_available_skips_occupied_port(tmp_path, monkeypatch):
    """占用端口后，_bind_available 必须跳过并落到下一个空闲端口。"""
    blocker = _occupy(54321)
    cfg = {"port": 54321}
    try:
        new_port = _bind_available(cfg, 54321, OSError(10048, "in use"))
        assert new_port > 54321
        assert new_port != 54321
        assert cfg["port"] == new_port
        # 新端口确实可用
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            probe.bind(("127.0.0.1", new_port))
        finally:
            probe.close()
    finally:
        blocker.close()


def test_bind_available_raises_when_all_taken():
    """连续占满 3 个候选端口后，_bind_available 必须抛异常。"""
    base = 54330
    holders = [_occupy(base + 1 + i) for i in range(3)]  # 54331/54332/54333
    cfg = {"port": base}
    try:
        with pytest.raises(OSError):
            _bind_available(cfg, base, OSError(10048, "in use"), attempts=3)
    finally:
        for h in holders:
            h.close()


def test_run_forever_falls_back_instead_of_exiting(tmp_path, monkeypatch):
    """端口被占用时 run_forever 必须降级启动，而不是 SystemExit。"""
    blocker = _occupy(54330)
    monkeypatch.setattr(Path, "cwd", classmethod(lambda cls: tmp_path))
    cfg = cfg = load_config(ROOT / "bridge.config.json")
    cfg["port"] = 54330
    cfg["host"] = "127.0.0.1"
    cfg["upstreams"] = {}
    db = Database(tmp_path / "t.db")
    bridge = Bridge(cfg, db, config_path=tmp_path / "bridge.config.json")
    try:
        # run_forever 会阻塞，放到线程里跑，稍后断言服务已起来
        import threading

        t = threading.Thread(target=run_forever, args=(bridge,), daemon=True)
        t.start()
        # 等它降级并监听
        deadline = __import__("time").time() + 10
        port = None
        while __import__("time").time() < deadline:
            port = int(bridge.config.get("port") or 0)
            if port and port != 54330:
                probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                probe.settimeout(0.5)
                try:
                    if probe.connect_ex(("127.0.0.1", port)) == 0:
                        break
                finally:
                    probe.close()
            __import__("time").sleep(0.3)
        assert port and port != 54330, f"未发生降级，port={port}"
        assert t.is_alive(), "run_forever 不应因端口占用而退出"
    finally:
        blocker.close()
        bridge.close()


def test_run_forever_keeps_configured_port_when_free(tmp_path, monkeypatch):
    """端口空闲时必须**原样**监听配置里的端口（不得漂移）。

    回归：早期实现把「可用性自检」放在绑定之后，等于用自己的监听去判定
    「端口被占用」—— 结果每次启动都白白 +1（8915 → 8916），按配置端口接入的
    客户端全部连不上。
    """
    freedom = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    freedom.bind(("127.0.0.1", 0))
    free_port = freedom.getsockname()[1]
    freedom.close()

    monkeypatch.setattr(Path, "cwd", classmethod(lambda cls: tmp_path))
    cfg = load_config(ROOT / "bridge.config.json")
    cfg["port"] = free_port
    cfg["host"] = "127.0.0.1"
    cfg["upstreams"] = {}
    db = Database(tmp_path / "t.db")
    bridge = Bridge(cfg, db, config_path=tmp_path / "bridge.config.json")
    try:
        t = threading.Thread(target=run_forever, args=(bridge,), daemon=True)
        t.start()

        deadline = time.time() + 10
        listened = False
        while time.time() < deadline:
            probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probe.settimeout(0.5)
            try:
                if probe.connect_ex(("127.0.0.1", free_port)) == 0:
                    listened = True
                    break
            finally:
                probe.close()
            time.sleep(0.3)

        assert listened, f"未在配置端口 {free_port} 上监听（发生了漂移）"
        assert int(bridge.config.get("port") or 0) == free_port, \
            f"端口漂移：配置 {free_port}，实际 {bridge.config.get('port')}"
        assert t.is_alive()
    finally:
        bridge.close()


def test_run_forever_strict_mode_still_exits(tmp_path, monkeypatch):
    """allow_port_fallback=False 时保持原有严格语义（SystemExit）。"""
    blocker = _occupy(54340)
    monkeypatch.setattr(Path, "cwd", classmethod(lambda cls: tmp_path))
    cfg = load_config(ROOT / "bridge.config.json")
    cfg["port"] = 54340
    cfg["host"] = "127.0.0.1"
    cfg["upstreams"] = {}
    db = Database(tmp_path / "t.db")
    bridge = Bridge(cfg, db, config_path=tmp_path / "bridge.config.json")
    try:
        with pytest.raises(SystemExit):
            run_forever(bridge, allow_port_fallback=False)
    finally:
        blocker.close()
        bridge.close()


def test_run_gui_falls_back_and_serves(tmp_path, monkeypatch):
    """GUI 路径：端口被占用时必须降级启动，且服务真的能连上。

    这是用户场景的直接复现：8900 被旧 Node 桥占着 → 双击 exe → 旧代码直接
    return 1，窗口从未打开（「根本无法使用、配置也没有」）。
    """
    from modelgate import app as gui_app

    blocker = _occupy(54350)
    monkeypatch.setattr(Path, "cwd", classmethod(lambda cls: tmp_path))
    cfg = load_config(ROOT / "bridge.config.json")
    cfg["port"] = 54350
    cfg["host"] = "127.0.0.1"
    cfg["upstreams"] = {}
    db = Database(tmp_path / "t.db")
    bridge = Bridge(cfg, db, config_path=tmp_path / "bridge.config.json")

    # 不让 GUI 真正开窗：拦住 webview
    monkeypatch.setattr(gui_app, "start_tray", lambda *a, **k: (None, None))
    monkeypatch.setattr(gui_app, "_notify_failure", lambda *a, **k: None)

    started = threading.Event()
    release = threading.Event()

    class _FakeWebview:
        created: dict = {}

        @staticmethod
        def create_window(title, url, **kw):
            _FakeWebview.created = {"title": title, "url": url}
            return object()

        @staticmethod
        def start(debug=False):
            # 模拟「窗口一直开着」：阻塞直到测试确认服务可用后放行
            started.set()
            release.wait(timeout=10)
            return

    monkeypatch.setitem(__import__("sys").modules, "webview", _FakeWebview)
    try:
        t = threading.Thread(target=gui_app.run_gui, args=(bridge,), daemon=True)
        t.start()
        started.wait(timeout=10)
        port = int(bridge.config.get("port") or 0)
        # 关键断言：端口已从被占的 54350 漂走，并且新端口真的能连上
        assert port and port != 54350, f"未发生降级，port={port}"
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        probe.settimeout(1.5)
        try:
            ok = False
            for _ in range(20):
                if probe.connect_ex(("127.0.0.1", port)) == 0:
                    ok = True
                    break
                __import__("time").sleep(0.25)
            assert ok, f"降级后服务未监听 (port={port})"
        finally:
            probe.close()
        assert _FakeWebview.created.get("url", "").endswith(f":{port}/ui/")
    finally:
        release.set()
        blocker.close()
        bridge.close()
