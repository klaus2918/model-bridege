# -*- mode: python ; coding: utf-8 -*-
"""Model-Gate PyInstaller 打包配置（onefile，对齐参考工程 command-code-gauge）。

产出：dist/Model-Gate.exe
版本号唯一来源：modelgate/__init__.py 的 __version__

仓库只保留这一份打包定义。升级时若要与在跑实例并存：
  复制本文件为 ModelGate.vX.Y.spec，把 name="Model-Gate" 改成 name="Model-Gate-vX.Y"，
  再用 pyinstaller 打包；验证与切换完成后删掉那份临时 spec。
"""
import os

from PyInstaller.utils.hooks import collect_submodules

# pywebview 在 Windows 上走 EdgeChromium；pystray 走 win32 后端
hiddenimports = [
    "webview.platforms.edgechromium",
    "pystray._win32",
]
hiddenimports += collect_submodules("webview")
hiddenimports += collect_submodules("pystray")

# 图标由 scripts/make_icon.py 生成；缺失时不影响打包（只是没有图标）
icon_path = os.path.join("assets", "ModelGate.ico")

a = Analysis(
    ["entry.py"],
    pathex=[],
    binaries=[],
    # 面板静态页必须随包带上：proxy.handle_ui 按 __file__ 定位
    # profile 注册表必须随包带上：provider_profiles.py 按 __file__ 同目录定位
    datas=[
        (os.path.join("modelgate", "web"), os.path.join("modelgate", "web")),
        (os.path.join("modelgate", "provider_profiles.json"), os.path.join("modelgate")),
    ],
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        # 只剔除本机存在但程序不用的大包，避免 onefile 体积失控
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
    name="Model-Gate",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    # 窗口程序：无控制台。访问日志由 modelgate/util.py 落 data/modelgate.log
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=icon_path if os.path.exists(icon_path) else None,
)
