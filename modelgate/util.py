# -*- coding: utf-8 -*-
"""通用小工具：日志时间戳、JSON 兜底、OpenAI content 压平、消息合并。"""
from __future__ import annotations

import json
import threading
import time

_log_lock = threading.Lock()
_access_log_enabled = True
_file_handle = None


def setup_file_log(path, *, max_bytes: int = 5 * 1024 * 1024, keep: int = 3) -> None:
    """把 log() 同时写入文件（exe 是无控制台窗口程序，stdout 看不见，必须落文件）。

    超过 max_bytes 时做简单轮转：modelgate.log → modelgate.log.1 → …
    任何失败都不影响主流程（只是没有文件日志）。
    """
    global _file_handle
    try:
        from pathlib import Path

        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists() and target.stat().st_size > max_bytes:
            for i in range(keep - 1, 0, -1):
                src = target.with_name(f"{target.name}.{i}")
                dst = target.with_name(f"{target.name}.{i + 1}")
                if src.exists():
                    src.replace(dst)
            target.replace(target.with_name(f"{target.name}.1"))
        _file_handle = target.open("a", encoding="utf-8")
    except OSError:
        _file_handle = None


def close_file_log() -> None:
    global _file_handle
    with _log_lock:
        if _file_handle is not None:
            try:
                _file_handle.close()
            finally:
                _file_handle = None


def set_access_log(enabled: bool) -> None:
    """由配置层在启动时设置；关闭后 log() 不再输出（仍返回 True 便于调用方分支）。"""
    global _access_log_enabled
    _access_log_enabled = bool(enabled)


def access_log_enabled() -> bool:
    return _access_log_enabled


def stamp(ts: float | None = None) -> str:
    """本机时区的 ISO 风格时间，带 UTC 偏移（UTC 时间戳对不上本地日志）。"""
    from datetime import datetime, timezone

    d = datetime.fromtimestamp(ts if ts is not None else time.time()).astimezone()
    off = d.utcoffset() or timezone.utc.utcoffset(d)
    assert off is not None
    total = int(off.total_seconds())
    sign = "-" if total < 0 else "+"
    total = abs(total)
    tz = f"{sign}{total // 3600:02d}:{(total % 3600) // 60:02d}"
    return f"{d.strftime('%Y-%m-%dT%H:%M:%S')}.{int(d.microsecond / 1000):03d}{tz}"


def log(line: str) -> None:
    global _file_handle
    with _log_lock:
        text = f"[{stamp()}] {line}"
        try:
            print(text, flush=True)
        except Exception:
            pass
        if _file_handle is not None:
            try:
                _file_handle.write(text + "\n")
                _file_handle.flush()
            except Exception:
                pass


def log_if(condition: bool, line: str) -> None:
    if condition:
        log(line)


def safe_json(text: str | None) -> dict:
    if not text:
        return {}
    try:
        out = json.loads(text)
        return out if isinstance(out, dict) else {}
    except Exception:
        return {}


def text_of(content) -> str:
    """把 OpenAI 的 content（字符串或分片数组）压成纯文本。"""
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return ""
    parts = []
    for p in content:
        if isinstance(p, str):
            parts.append(p)
        elif isinstance(p, dict) and p.get("type") == "text":
            parts.append(p.get("text") or "")
    return "".join(parts)


def push_message(lst: list, role: str, content: str) -> None:
    """按序合并相邻同角色消息，满足 Anthropic 角色交替要求。"""
    if content == "" or content is None:
        return
    last = lst[-1] if lst else None
    if (
        last
        and last.get("role") == role
        and isinstance(last.get("content"), str)
        and isinstance(content, str)
    ):
        last["content"] += f"\n{content}"
        return
    lst.append({"role": role, "content": content})


def now_epoch_ms() -> int:
    return int(time.time() * 1000)


def now_epoch() -> int:
    return int(time.time())


# ── 崩溃落盘（无控制台窗口程序唯一可追溯处） ──────────────────────────────
def crash_dir():
    """崩溃日志目录：与 setup_file_log 同约定（frozen=exe/data，开发态=项目根/data）。"""
    from pathlib import Path
    import sys

    base = Path(sys.executable).resolve().parent if getattr(sys, "frozen", False) \
        else Path(__file__).resolve().parent.parent
    return base / "data"


def write_crash(exc: BaseException) -> None:
    """把未捕获异常写进 data/crash.log。

    PyInstaller console=False 的窗口程序没有 stdout：异常只会让进程一闪而过，
    用户既看不到堆栈也没法反馈。这里保证「静默死亡」至少留下证据。
    """
    import traceback

    try:
        directory = crash_dir()
        directory.mkdir(parents=True, exist_ok=True)
        with (directory / "crash.log").open("a", encoding="utf-8") as fh:
            fh.write(f"\n=== {time.strftime('%Y-%m-%d %H:%M:%S')} ===\n")
            fh.write("".join(traceback.format_exception(exc)).strip() + "\n")
    except Exception:
        pass
