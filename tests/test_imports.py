# -*- coding: utf-8 -*-
"""全包导入烟测：任一模块的顶层 import 出错都在这里拦住。

背景：app.py 曾从 config 导入实际定义在 proxy 的 resolve_port，
`--no-gui` 路径不经过 app.py，导致该错误直到打包后 GUI 冷启动才暴露。
"""
from __future__ import annotations

import importlib
import pkgutil

import modelgate


def test_every_module_imports_cleanly():
    failures = []
    for info in pkgutil.walk_packages(modelgate.__path__, modelgate.__name__ + "."):
        if ".__pycache__" in info.name:
            continue
        try:
            importlib.import_module(info.name)
        except Exception as exc:  # noqa: BLE001 - 汇总所有失败再一次性报出
            failures.append(f"{info.name}: {type(exc).__name__}: {exc}")
    assert not failures, "以下模块导入失败：\n  " + "\n  ".join(failures)


def test_app_exposes_expected_entrypoints():
    from modelgate import app

    assert callable(app.run_gui)
    assert callable(app.start_proxy_thread)
    assert callable(app.make_icon_image)


def test_proxy_exposes_expected_entrypoints():
    from modelgate import proxy

    assert callable(proxy.create_server)
    assert callable(proxy.run_forever)
    assert callable(proxy.resolve_port)
    assert proxy.resolve_port({"port": 0}) == 0
    assert proxy.resolve_port({}) == 8900
    assert proxy.resolve_port({"port": 8901}) == 8901


def test_port_error_message_distinguishes_in_use_from_permission():
    """Windows 上端口被占用可能报 10013，必须靠实际探测判定，不能只看 errno。"""
    from modelgate import proxy

    # 真实占用的端口 → 必须报「被占用」
    import socket

    blocker = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    blocker.bind(("127.0.0.1", 0))
    blocker.listen(1)
    port = blocker.getsockname()[1]
    try:
        exc = OSError(10013, "Permission denied")
        exc.errno = 10013
        msg = proxy.port_error_message("127.0.0.1", port, exc)
        assert "已被占用" in msg
        assert "netstat" in msg
    finally:
        blocker.close()

    # 没人监听 + 权限错误 → 报权限
    exc2 = OSError(10013, "Permission denied")
    exc2.errno = 10013
    msg2 = proxy.port_error_message("127.0.0.1", 0, exc2)
    assert "权限" in msg2

    # 明确的 ADDRINUSE → 报占用
    exc3 = OSError(10048, "Only one usage of each socket address")
    exc3.errno = 10048
    assert "已被占用" in proxy.port_error_message("127.0.0.1", 1, exc3)
