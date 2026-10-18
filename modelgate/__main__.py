# -*- coding: utf-8 -*-
"""命令行入口：python -m modelgate [--no-gui] [--config PATH] [--port N] [--db PATH]。

默认启动桌面窗口（pywebview）并同时运行本地代理；
--no-gui 以纯代理模式运行，适合脚本、服务化或排障场景。
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .config import (
    apply_runtime_flags,
    default_config_path,
    default_db_path,
    load_config,
    load_env_file,
    merge_config_with_snapshot,
    save_config,
)
from .db import Database
from .proxy import Bridge, resolve_port, run_forever
from .util import log, setup_file_log, write_crash as _write_crash


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="modelgate",
        description="Model-Gate 桌面代理：模型接入、模型配置与 token 用量统计",
    )
    parser.add_argument("--config", help="配置文件路径（默认同目录 bridge.config.json）")
    parser.add_argument("--port", type=int, help="覆盖配置中的监听端口")
    parser.add_argument("--host", help="覆盖配置中的监听地址")
    parser.add_argument("--db", help="SQLite 数据库路径（默认 exe 同目录 data/modelgate.db）")
    parser.add_argument("--no-gui", action="store_true", help="以纯代理模式运行，不打开窗口")
    parser.add_argument("--no-generate-config", action="store_true",
                        help="配置文件不存在时不自动生成默认配置")
    parser.add_argument("--diagnose", action="store_true",
                        help="排障模式：把解释器级崩溃（段错误/导入期异常）落盘 data/fault.log")
    parser.add_argument("--force-multi", action="store_true",
                        help="允许多开：跳过单实例守卫（调试/对比测试用）")
    return parser


def _saved_port(db_path) -> "int | None":
    """读上次实际监听的端口（供激活已有实例用）；任何失败都返回 None。"""
    try:
        db = Database(db_path)
        raw = db.get_setting("lastPort")
        db.close()
        return int(raw) if raw else None
    except Exception:
        return None


def prepare(args) -> tuple[dict, Path, Path]:
    """加载配置（SQLite settings 覆盖 → 配置文件），返回 (config, config_path, db_path)。"""
    config_path = Path(args.config) if args.config else default_config_path()
    db_path = Path(args.db) if args.db else default_db_path()

    config = load_config(config_path)
    load_env_file(config)
    if args.port:
        config["port"] = int(args.port)
    if args.host:
        config["host"] = args.host
    apply_runtime_flags(config)

    # 配置优先级：**文件优先**，SQLite settings 快照只补文件没有定义的键。
    # 历史事故：快照里残留测试哨兵（upstreams.cch.baseUrl=http://127.0.0.1:1），
    # 旧实现「快照整体覆盖文件」把用户手工维护的 bridge.config.json 静默改掉，
    # 默认上游指向死端口 —— 表现为「配置没了 / 代理不可用」。
    if db_path.is_file():
        db = None
        try:
            db = Database(db_path)
            override = db.load_config_override()
            if override:
                config, conflicts, filled = merge_config_with_snapshot(config, override)
                if args.port:
                    config["port"] = int(args.port)
                if args.host:
                    config["host"] = args.host
                if conflicts or filled:
                    db.save_config_override(config)  # 自愈：快照回到与文件一致
                    detail = []
                    if conflicts:
                        detail.append("忽略快照过期值 " + ", ".join(conflicts[:6]))
                    if filled:
                        detail.append("补入快照补充键 " + ", ".join(filled[:6]))
                    print("[model-gate] 配置以文件为准（" + "；".join(detail) + "）",
                          file=sys.stderr)
        except Exception as exc:
            print(f"[model-gate] 读取数据库配置失败，改用文件配置：{exc}", file=sys.stderr)
        finally:
            if db is not None:
                try:
                    db.close()
                except Exception:
                    pass

    if not config_path.is_file() and not args.no_generate_config:
        try:
            save_config(config_path, config)
            print(f"[model-gate] 已生成默认配置 {config_path}")
        except Exception as exc:
            print(f"[model-gate] 生成配置失败：{exc}", file=sys.stderr)
    return config, config_path, db_path


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        config, config_path, db_path = prepare(args)
        # exe 是无控制台程序，stdout 不可见：访问日志同步落文件
        setup_file_log(db_path.parent / "modelgate.log")

        # 单实例守卫：判据是命名互斥体，**不是端口**。
        # 端口被别的程序（例如旧 Node 桥）占用属于正常情况，应降级继续服务；
        # 只有「自己已经有一个实例在跑」才让路，并把已有实例推到前台。
        if not args.force_multi:
            from .single import SingleInstance, activate_existing

            guard = SingleInstance(db_path)
            if not guard.acquire():
                port = _saved_port(db_path) or resolve_port(config)
                action = activate_existing(port)
                log(f"已有 Model-Gate 实例在运行（动作：{action}），本次不再重复启动")
                if action == "none":
                    _notify_duplicate(port)
                print(f"[model-gate] 已有实例在运行，未启动第二个进程"
                      f"（面板 http://127.0.0.1:{port}/ui/）")
                return 0

        db = Database(db_path)
        bridge = Bridge(config, db, config_path=config_path)

        if args.no_gui:
            run_forever(bridge)
            return 0

        from .app import run_gui

        return run_gui(bridge)
    except SystemExit:
        raise
    except BaseException as exc:  # noqa: BLE001 — 崩溃落盘后原样退出
        _write_crash(exc)
        raise


def _notify_duplicate(port) -> None:
    """已有实例时的可视提示（无控制台 exe 唯一能把信息递给用户的通道）。"""
    try:
        import ctypes

        message = (f"Model-Gate 已经在运行。\n\n"
                   f"面板地址：http://127.0.0.1:{port}/ui/\n\n"
                   f"如需同时运行多个实例，请加 --force-multi 启动。")
        ctypes.windll.user32.MessageBoxW(0, message, "Model-Gate 已在运行", 0x40)
    except Exception:
        pass


def _install_crash_hook() -> None:
    """兜底：非 main() 路径的未捕获异常（导入期、线程外、GUI 主循环）也落盘。"""
    original = sys.excepthook

    def hook(exc_type, exc, tb):
        _write_crash(exc)
        original(exc_type, exc, tb)

    sys.excepthook = hook


# 模块导入即生效：覆盖 main() 之前的全部启动阶段（含 Database 打开失败）
_install_crash_hook()


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
