# -*- coding: utf-8 -*-
"""自测 scan_secrets.py 的阻断能力：造 7 种典型payload，逐个暂存并扫描。

要求全部 exit 1（命中阻断级），否则说明扫描器漏了，不能拿它当门禁。
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

CASES: list[tuple[str, str]] = [
    ("空 apiKey 引号（不应命中）", '"apiKey": ""\n'),
    ("非空 apiKey 明文", '"apiKey": "' + "sk-" + "live-abcdef1234567890" + '"\n'),
    ("密钥环境变量落值", "OPENCODE_API_KEY = " + "abcdef1234567890abcdef" + "\n"),
    ("AWS Access Key", "AKIA" + "IOSFODNN7EXAMPLE\n"),
    ("GitHub 令牌", "ghp_" + "a" * 36 + "\n"),
    ("私钥头", "-----BEGIN " + "RSA PRIVATE KEY-----\n"),
    ("sk- 通用密钥", "sk-" + "proj-0123456789abcdefghij\n"),
    ("JWT", "eyJhbGciOiJIUzI1NiJ9." + "eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc\n"),
]

# 期望命中的用例（空 apiKey 是阴性对照，期望通过）
EXPECT_BLOCK = {
    "空 apiKey 引号（不应命中）": False,
    "非空 apiKey 明文": True,
    "密钥环境变量落值": True,
    "AWS Access Key": True,
    "GitHub 令牌": True,
    "私钥头": True,
    "sk- 通用密钥": True,
    "JWT": True,
}

PROBE = "_secret_selftest.tmp"


def stage_and_scan(content: str) -> tuple[int, str]:
    """把内容写入临时文件、git add，跑 --staged，最后重置。"""
    path = ROOT / PROBE
    path.write_text(content, encoding="utf-8")
    subprocess.run(["git", "add", PROBE], cwd=ROOT, capture_output=True)
    out = subprocess.run(
        [sys.executable, "scripts/scan_secrets.py", "--staged"],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8",
        errors="replace", timeout=120)
    subprocess.run(["git", "reset", "-q", "HEAD", PROBE], cwd=ROOT,
                   capture_output=True)
    path.unlink(missing_ok=True)
    return out.returncode, out.stdout + out.stderr


def main() -> int:
    print(f"=== scan_secrets 自测（{len(CASES)} 例）===\n")
    bad: list[str] = []
    for name, payload in CASES:
        code, log = stage_and_scan(payload)
        expect = EXPECT_BLOCK[name]
        hit = code == 1
        ok = hit == expect
        mark = "PASS" if ok else "FAIL"
        print(f"[{mark}] {name}: exit={code} 期望命中={expect}")
        if not ok:
            bad.append(name)
            print("      " + log.strip().replace("\n", "\n      ")[:600])

    # 收尾：清理暂存态
    subprocess.run(["git", "reset", "-q"], cwd=ROOT, capture_output=True)
    print()
    if bad:
        print(f"SELFTEST FAILED: {bad}")
        return 1
    print("SELFTEST OK: 阻断规则全部生效，阴性对照未被误伤")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
