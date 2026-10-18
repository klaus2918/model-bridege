# -*- mode: python ; coding: utf-8 -*-
"""Model-Gate v2.17 打包配置（与 ModelGate.spec 等价，仅输出名不同）。

为什么要单独一份：**升级时必须与在跑的实例并存** —— 运行中的 exe 文件被系统锁定，
PyInstaller 无法覆盖它。用不同输出名即可在代理不中断的前提下产出新版本，
随后按「备用端口并行 → 验证 → 切客户端 → 停旧实例」切换。
"""
import os

from PyInstaller.utils.hooks import collect_submodules

hiddenimports = ["webview.platforms.edgechromium", "pystray._win32"]
hiddenimports += collect_submodules("webview")
hiddenimports += collect_submodules("pystray")

icon_path = os.path.join("assets", "ModelGate.ico")

a = Analysis(
    ["entry.py"],
    pathex=[],
    binaries=[],
    datas=[(os.path.join("modelgate", "web"), os.path.join("modelgate", "web"))],
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        "tkinter", "matplotlib", "numpy", "pandas", "scipy",
        "torch", "torchvision", "IPython", "pytest",
    ],
    noarchive=False,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name="Model-Gate-v2.17",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=icon_path if os.path.exists(icon_path) else None,
)
