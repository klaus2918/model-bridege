# -*- coding: utf-8 -*-
"""PyInstaller 打包入口：直接进入桌面模式（等价 python -m modelgate）。

打包命令见 build.bat；版本号唯一来源是 modelgate/__init__.py。
"""
from __future__ import annotations

import faulthandler
import sys


def _run() -> int:
    from modelgate.__main__ import main

    return main()


def _install_crash_safety() -> None:
    """按需启用解释器级崩溃转储（--diagnose）。

    默认不启用：faulthandler 需要常驻文件句柄，会在交付目录留下空
    fault.log，既不是交付物也会让用户困惑。
    排障时加 --diagnose，即可把引导期/解释器级崩溃（段错误、abort、
    以及在 sys.excepthook 设置之前的导入期异常）落盘到 data/fault.log。
    """
    if "--diagnose" not in sys.argv:
        return
    base = sys.executable if getattr(sys, "frozen", False) else __file__
    try:
        from pathlib import Path

        directory = Path(base).resolve().parent
        if not getattr(sys, "frozen", False):
            directory = directory / "data"
        directory.mkdir(parents=True, exist_ok=True)
        handle = (directory / "fault.log").open("a", encoding="utf-8")
        faulthandler.enable(file=handle, all_threads=True)
    except Exception:
        pass


_install_crash_safety()


if __name__ == "__main__":
    sys.exit(_run())
