# -*- coding: utf-8 -*-
"""单实例守卫：同一份 data/ 只允许一个 Model-Gate 实例。

**为什么不能用端口当判据**：端口「被占用」不等于「自己已有实例」。
8900 可能被别的程序（例如旧的 Node 桥）占着，此时新实例应当降级到下一个
端口继续服务；只有「自己已经有一个实例在跑」时才应该让路并激活已有窗口。
本模块用 Windows 命名互斥体做判据，与端口解耦。

互斥体名带 **数据目录路径哈希**：同一份数据目录互斥，不同目录的绿色版副本
可以并存（用户把 exe 复制到两处分别跑，是合法用法）。

非 Windows 或拿不到内核 API 时一律放行（不阻断启动，保持可测试）。
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
from typing import Optional

MUTEX_PREFIX = "Local\\ModelGate_SingleInstance_"
ERROR_ALREADY_EXISTS = 183
WINDOW_TITLE_PREFIX = "Model-Gate"


def instance_key(db_path) -> str:
    """由数据目录派生的互斥体名（同一份数据 → 同一个名字）。"""
    try:
        base = str(Path(db_path).resolve().parent).lower()
    except Exception:
        base = str(db_path or "").lower()
    digest = hashlib.sha1(base.encode("utf-8")).hexdigest()[:16]
    return MUTEX_PREFIX + digest


class SingleInstance:
    """持有互斥体句柄；进程退出时句柄释放，互斥体自动消失（无需显式清理）。"""

    def __init__(self, db_path) -> None:
        self.name = instance_key(db_path)
        self.already_running = False
        self._handle = None

    def acquire(self) -> bool:
        """尝试成为唯一实例。返回 True 表示可以继续启动。"""
        try:
            import ctypes
            from ctypes import wintypes

            kernel32 = ctypes.windll.kernel32
            kernel32.CreateMutexW.restype = wintypes.HANDLE
            kernel32.CreateMutexW.argtypes = [wintypes.LPCVOID, wintypes.BOOL,
                                              wintypes.LPCWSTR]
            handle = kernel32.CreateMutexW(None, wintypes.BOOL(True), self.name)
            if not handle:
                return True  # 拿不到句柄：不阻断启动
            self._handle = handle
            self.already_running = (int(kernel32.GetLastError()) == ERROR_ALREADY_EXISTS)
            return not self.already_running
        except Exception:
            return True


def _activate_window() -> bool:
    """把已有的主窗口前置（按标题前缀匹配，避免依赖固定标题）。"""
    try:
        import ctypes
        from ctypes import wintypes

        user32 = ctypes.windll.user32
        found: list[int] = []
        EnumProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND,
                                      wintypes.LPARAM)

        def callback(hwnd, _lparam):
            length = user32.GetWindowTextLengthW(hwnd)
            if length <= 0:
                return True
            buffer = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buffer, length + 1)
            title = buffer.value or ""
            if title.startswith(WINDOW_TITLE_PREFIX):
                found.append(int(hwnd))
                return False
            return True

        user32.EnumWindows(EnumProc(callback), 0)
        if not found:
            return False
        hwnd = found[0]
        SW_RESTORE = 9
        user32.ShowWindow(hwnd, SW_RESTORE)
        user32.SetForegroundWindow(hwnd)
        return True
    except Exception:
        return False


def activate_existing(port: Optional[int] = None) -> str:
    """激活已有实例：优先前置窗口，退化到打开它的面板页面。

    返回实际动作（``window`` / ``browser`` / ``none``），供日志与测试断言。
    """
    if _activate_window():
        return "window"
    try:
        if port:
            url = f"http://127.0.0.1:{int(port)}/ui/"
            if hasattr(os, "startfile"):  # Windows：默认浏览器打开
                os.startfile(url)  # noqa: S606 - 本地固定 URL，非用户输入
                return "browser"
            import webbrowser

            if webbrowser.open(url):
                return "browser"
    except Exception:
        pass
    return "none"
