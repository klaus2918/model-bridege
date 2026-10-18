# -*- coding: utf-8 -*-
"""桌面入口：本地代理服务 + pywebview 窗口 + pystray 托盘 + 单实例。

生命周期：
1. 先启动本地代理（ThreadingHTTPServer 独立线程）。端口被占用即视为已有实例，
   打印可操作提示并退出 —— 这同时充当「单实例守卫」。
2. 再打开 pywebview 窗口加载 /ui/ 页面（配置页与用量统计页）。
3. 托盘图标提供 打开窗口 / 暂停代理提示 / 退出；窗口关闭时整体退出。
任何环节缺少依赖都降级为「纯代理模式」，不让桌面能力拖垮代理可用性。
"""
from __future__ import annotations

import threading
import time
import traceback
from http.server import ThreadingHTTPServer

from . import APP_NAME, APP_TITLE, __version__
from .config import model_list
from .proxy import _bind_available, _port_free, port_error_message, resolve_port
from .util import log, write_crash


# ── 图标（运行时生成，避免打包时漏带资源） ────────────────────────────────
def make_icon_image(size: int = 64):
    """用 Pillow 画一个「网关」风格图标：圆角底 + 三条汇聚线。"""
    from PIL import Image, ImageDraw

    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=size // 5,
                           fill=(30, 64, 175, 255))
    margin = size // 4
    # 右侧三个节点，左侧一条汇聚线（象征多上游 → 单网关）
    draw.line([(margin, size // 2), (size - margin, margin)], fill=(255, 255, 255, 255),
              width=max(2, size // 16))
    draw.line([(margin, size // 2), (size - margin, size // 2)], fill=(255, 255, 255, 255),
              width=max(2, size // 16))
    draw.line([(margin, size // 2), (size - margin, size - margin)],
              fill=(255, 255, 255, 255), width=max(2, size // 16))
    r = max(2, size // 10)
    for cx, cy in ((size - margin, margin), (size - margin, size // 2),
                   (size - margin, size - margin)):
        draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 255, 255, 255))
    return img


def start_proxy_thread(bridge) -> tuple[ThreadingHTTPServer, threading.Thread]:
    """在后台线程启动代理；端口冲突时抛 OSError（由调用方转成用户提示）。"""
    server = _create_server(bridge)
    thread = threading.Thread(target=server.serve_forever,
                              kwargs={"poll_interval": 0.25}, daemon=True,
                              name="modelgate-proxy")
    thread.start()
    bridge.start_background_tasks()
    return server, thread


def _create_server(bridge) -> ThreadingHTTPServer:
    from .proxy import create_server

    return create_server(bridge)


def _port_conflict_message(bridge, exc: OSError) -> str:
    """绑定失败消息：统一走 proxy.port_error_message（先探测占用，再谈权限）。"""
    host = bridge.config.get("host") or "127.0.0.1"
    return port_error_message(host, resolve_port(bridge.config), exc)


def _print_startup_banner(bridge, port: int) -> None:
    """启动横幅：同时进控制台与文件日志（exe 无控制台，文件是唯一可见处）。"""
    host = bridge.config.get("host") or "127.0.0.1"
    log(f"{APP_NAME} v{__version__} 已启动 http://{host}:{port}/v1")
    for up in bridge.upstreams:
        suffix = "  [默认]" if up["default"] else ""
        session = "  会话头注入" if up["sessionHeader"] and up["protocol"] == "openai" else ""
        log(f"  上游 {up['name']:<10} {up['protocol']:<9} {up['baseUrl']}{suffix}{session}")
    models = model_list(bridge.config, bridge.upstreams)
    log(f"声明的模型: {', '.join(models) or '(未配置)'}")
    log(f"thinking 回传: {'开启' if bridge.thinking_cache.enabled else '关闭'}"
        f"（缓存上限 {bridge.thinking_cache.max_size} 条）")
    log(f"用量已落库: {bridge.db.path}")


# ── 托盘 ──────────────────────────────────────────────────────────────────
def start_tray(url: str, window, on_quit, bridge=None):
    """启动系统托盘；任何失败都只提示不阻断窗口。返回 (icon, thread) 或 (None, None)。

    运维动作（刷新模型目录 / 打开数据目录 / 复制接入地址）全部包在 try 内：
    托盘只是便利入口，绝不能因为某个动作失败把窗口带崩。
    """
    try:
        import pystray
    except Exception as exc:
        print(f"[model-gate] 托盘不可用（未安装 pystray）：{exc}")
        return None, None

    def show(_icon=None, _item=None):
        try:
            window.show()
            window.restore()
            window.focus()
        except Exception:
            pass

    def hide(_icon=None, _item=None):
        try:
            window.hide()
        except Exception:
            pass

    def quit(_icon=None, _item=None):
        try:
            window.destroy()
        except Exception:
            pass
        on_quit()

    def refresh_models(_icon=None, _item=None):
        """强制刷新上游模型目录（跳过缓存），并在日志里给出结果。"""
        try:
            if bridge is None:
                return
            cache = bridge.refresh_models_cache(force=True)
            log(f"托盘动作：已刷新模型目录，共 {len(cache.get('ids') or [])} 个模型")
        except Exception as exc:  # noqa: BLE001
            log(f"托盘动作：刷新模型目录失败 {exc}")

    def open_data_dir(_icon=None, _item=None):
        """打开数据目录（DB 与日志所在处），便于备份与排障。"""
        try:
            import os
            import subprocess

            if bridge is None:
                return
            folder = Path(bridge.db.path).resolve().parent
            folder.mkdir(parents=True, exist_ok=True)
            if hasattr(os, "startfile"):
                os.startfile(str(folder))  # noqa: S606 - 本地固定目录
            else:  # pragma: no cover - 非 Windows 兜底
                subprocess.Popen(["xdg-open", str(folder)])
            log(f"托盘动作：已打开数据目录 {folder}")
        except Exception as exc:  # noqa: BLE001
            log(f"托盘动作：打开数据目录失败 {exc}")

    def copy_endpoint(_icon=None, _item=None):
        """把接入地址（含 /v1）复制到剪贴板，省得手打。"""
        try:
            import ctypes

            text = url if url.rstrip("/").endswith("/v1") else url.rstrip("/") + "/v1"
            GMEM_MOVEABLE, CF_UNICODETEXT = 0x0002, 13
            kernel32, user32 = ctypes.windll.kernel32, ctypes.windll.user32
            user32.OpenClipboard(0)
            try:
                user32.EmptyClipboard()
                buf = ctypes.create_unicode_buffer(text)
                size = ctypes.sizeof(buf)
                handle = kernel32.GlobalAlloc(GMEM_MOVEABLE, size)
                pointer = kernel32.GlobalLock(handle)
                ctypes.memmove(pointer, buf, size)
                kernel32.GlobalUnlock(handle)
                user32.SetClipboardData(CF_UNICODETEXT, handle)
            finally:
                user32.CloseClipboard()
            log(f"托盘动作：已复制接入地址 {text}")
        except Exception as exc:  # noqa: BLE001
            log(f"托盘动作：复制接入地址失败 {exc}")

    try:
        icon = pystray.Icon(
            "modelgate",
            make_icon_image(),
            f"{APP_NAME} v{__version__}",
            menu=pystray.Menu(
                pystray.MenuItem("打开窗口", show, default=True),
                pystray.MenuItem("隐藏窗口", hide),
                pystray.Menu.SEPARATOR,
                pystray.MenuItem("刷新模型目录", refresh_models),
                pystray.MenuItem("复制接入地址", copy_endpoint),
                pystray.MenuItem("打开数据目录", open_data_dir),
                pystray.Menu.SEPARATOR,
                pystray.MenuItem(f"代理地址 {url}", None, enabled=False),
                pystray.MenuItem("退出", quit),
            ),
        )
    except Exception as exc:
        print(f"[model-gate] 托盘初始化失败：{exc}")
        return None, None

    thread = threading.Thread(target=icon.run, daemon=True, name="modelgate-tray")
    thread.start()
    return icon, thread


# ── 主流程 ────────────────────────────────────────────────────────────────
def run_gui(bridge, *, allow_port_fallback: bool = True) -> int:
    """打开桌面窗口；缺依赖或无显示环境时降级为纯代理模式。

    allow_port_fallback=False 时端口冲突直接报错退出（供测试与
    --no-gui 严格模式使用），默认 True：桌面版以「能打开」为优先。
    """
    _dev_no_fallback = not allow_port_fallback

    server = None
    try:
        # 先判可用性、再绑定。顺序不能反：本进程一旦已在监听，探测用的 bind 必然
        # 失败，于是「空闲端口」也会被判成被占用，白白漂移一个端口 —— 用户按
        # 配置里的端口接入就会连不上。这里同时覆盖 Windows SO_REUSEADDR 的
        # 「能 bind 但连不上」情形（探测用的是未设 REUSEADDR 的裸 socket）。
        if not _port_free("127.0.0.1", resolve_port(bridge.config)):
            raise OSError(10048, "port already in use")
        server, _thread = start_proxy_thread(bridge)
    except OSError as exc:
        if _dev_no_fallback:
            _fail(bridge, exc)
            return 1
        server = _escalate_port(bridge, exc)  # 端口冲突 → 自动降级并重启
    _thread = None

    port = server.server_address[1]
    # 先记下配置里声明的端口（降级后用于判断是否发生了漂移）
    configured = int((bridge.config or {}).get("port") or 0)
    # 把实际端口回写配置（port=0 或降级场景），供 /health 与页面展示使用
    bridge.config["port"] = port
    bridge.note_active_port(port)
    _print_startup_banner(bridge, port)
    title = APP_TITLE + f" v{__version__}"
    if configured and configured != port:
        title += f"（端口 {configured} 被占用，已用 {port}）"
    url = f"http://{bridge.config.get('host', '127.0.0.1')}:{port}/ui/"

    try:
        import webview
    except Exception as exc:
        print(f"[model-gate] 桌面窗口不可用（未安装 pywebview）：{exc}")
        print(f"[model-gate] 已降级为纯代理模式，打开 {url} 查看面板")
        try:
            server.serve_forever(poll_interval=0.25)
        except KeyboardInterrupt:
            print("\n[model-gate] 已停止")
        finally:
            server.server_close()
            bridge.close()
        return 0

    stopped = threading.Event()

    def on_quit() -> None:
        stopped.set()

    try:
        window = webview.create_window(
            title,
            url,
            width=1200,
            height=800,
            min_size=(960, 640),
        )
    except Exception as exc:
        print(f"[model-gate] 创建窗口失败：{exc}")
        traceback.print_exc()
        return 1

    tray_icon, _tray_thread = start_tray(url, window, on_quit, bridge)

    try:
        webview.start(debug=False)
    except BaseException as exc:  # noqa: BLE001 — 窗口崩溃也要让用户看见原因
        # 无控制台：打印不可见。同时写崩溃日志 + 弹系统对话框，再降级为纯代理。
        _window_failure(exc, url, server, tray_icon, bridge)
        return 0
    finally:
        # 窗口关闭 → 收托盘、停代理、关数据库
        stopped.set()
        if tray_icon is not None:
            try:
                tray_icon.stop()
            except Exception:
                pass
        try:
            server.shutdown()
            server.server_close()
        except Exception:
            pass
        bridge.close()
        print("[model-gate] 已退出")
    return 0


def _fail(bridge, exc: OSError) -> None:
    """启动失败：打印可操作信息、弹系统对话框、关资源。"""
    host = bridge.config.get("host") or "127.0.0.1"
    port = resolve_port(bridge.config)
    message = port_error_message(host, port, exc)
    print(f"[model-gate] {message}", flush=True)
    _notify_failure(message)
    bridge.close()


def _escalate_port(bridge, exc: OSError):
    """端口不可用 → 自动降级到可用端口，重启服务与后台任务。

    返回新 server；降级失败时打印原因并 SystemExit(1)。
    """
    original = resolve_port(bridge.config)
    try:
        _bind_available(bridge.config, original, exc)
    except OSError as exc2:
        _fail(bridge, exc2)
        raise SystemExit(1) from exc2

    server = _create_server(bridge)
    threading.Thread(target=server.serve_forever,
                     kwargs={"poll_interval": 0.25}, daemon=True,
                     name="modelgate-proxy").start()
    bridge.start_background_tasks()
    return server


def _notify_failure(message: str) -> None:
    """启动失败时尽可能弹一个系统对话框（无 GUI 依赖时静默）。"""
    try:
        import threading as _threading

        def show():
            try:
                import ctypes

                ctypes.windll.user32.MessageBoxW(0, message, f"{APP_NAME} 启动失败", 0x10)
            except Exception:
                pass

        _threading.Thread(target=show, daemon=True).start()
        time.sleep(0.2)
    except Exception:
        pass


def _dialog(title: str, message: str) -> None:
    """通用系统对话框：无控制台 exe 里唯一能把信息推到用户眼前的通道。"""
    try:
        import threading as _threading

        def show():
            try:
                import ctypes

                ctypes.windll.user32.MessageBoxW(0, message, title, 0x40)
            except Exception:
                pass

        _threading.Thread(target=show, daemon=True).start()
        time.sleep(0.2)
    except Exception:
        pass


def _window_failure(exc: BaseException, url: str, server, tray_icon, bridge) -> None:
    """窗口阶段异常：落盘 + 弹窗 + 降级为后端代理，绝不静默退出。

    无控制台的窗口程序最怕「窗口没出来、进程也消失」：用户既看不到原因也无法
    反馈。这里保证三层兜底 —— crash.log 有堆栈、弹窗有结论、代理仍在跑。
    """
    write_crash(exc)
    _dialog(
        f"{APP_NAME} 窗口异常已记录到 data/crash.log。\n\n"
        f"代理已转为后台模式，可访问：\n{url}\n\n"
        f"{type(exc).__name__}: {exc}",
    )
    print(f"[model-gate] 窗口异常已记录：{type(exc).__name__}: {exc}", flush=True)
    print(f"[model-gate] 已转为后端代理模式：{url}", flush=True)
    try:
        server.serve_forever(poll_interval=0.25)
    except KeyboardInterrupt:
        pass
    finally:
        if tray_icon is not None:
            try:
                tray_icon.stop()
            except Exception:
                pass
        bridge.close()
