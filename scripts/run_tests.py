# -*- coding: utf-8 -*-
"""有界测试运行器：逐文件独立子进程 + 单文件超时，输出可判定的回归结论。

存在理由：整目录一次性 `pytest tests` 在本机会出现无界挂起（历史上多次复现，
原因通常是某个用例在等一个不会到达的网络响应，或端口/连接池没有收尾）。
挂起的测试跑等于没有测试 —— 这条命令把「没有结论」变成「每个文件都有结论」。

用法：
  python scripts/run_tests.py            # 全部测试文件
  python scripts/run_tests.py --timeout 240
退出码：全部通过 0；有失败/超时 1。
"""
from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"


def main() -> int:
    parser = argparse.ArgumentParser(description="逐文件有界跑 pytest")
    parser.add_argument("--timeout", type=int, default=180,
                        help="单个测试文件的超时秒数（默认 180）")
    parser.add_argument("--pattern", default="test_*.py", help="文件名通配（默认 test_*.py）")
    args = parser.parse_args()

    files = sorted(TESTS.glob(args.pattern))
    if not files:
        print(f"未找到测试文件：{TESTS / args.pattern}")
        return 1

    results: list[tuple[str, str, float, str]] = []
    for path in files:
        start = time.time()
        try:
            proc = subprocess.run(
                [sys.executable, "-m", "pytest", str(path), "-q",
                 "-p", "no:cacheprovider", "-rf"],
                cwd=str(ROOT), capture_output=True, text=True,
                timeout=args.timeout,
            )
            lines = (proc.stdout or "").strip().splitlines()
            summary = lines[-1] if lines else ""
            # 只报「1 failed」等于没报：把失败用例名一并带出来，便于判断是否与本次改动相关
            failed = [l.strip()[len("FAILED "):] for l in lines
                      if l.strip().startswith("FAILED ")]
            if failed:
                summary = f"{summary} ← {'; '.join(failed[:4])}"
            status = "PASS" if proc.returncode == 0 else "FAIL"
        except subprocess.TimeoutExpired:
            summary = f"TIMEOUT(>{args.timeout}s)"
            status = "HANG"
        elapsed = time.time() - start
        results.append((path.name, status, elapsed, summary))
        print(f"{status:5} {elapsed:6.1f}s  {path.name}  | {summary[:150]}", flush=True)

    bad = [r for r in results if r[1] != "PASS"]
    print(f"\n共 {len(results)} 个文件，未通过 {len(bad)} 个")
    for name, status, elapsed, summary in bad:
        print(f"  {status} {name}: {summary[:120]}")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
