# -*- coding: utf-8 -*-
"""提交前敏感信息扫描（scan-sensitive.sh 的 Windows 原生等价物）。

规则与 bash 版一致，分两级：
  阻断级（exit 1，禁止提交/发布）：私钥 / 云厂商 / 代码托管令牌 / sk- 密钥 /
    Bearer / JWT / 非空 apiKey / 密钥环境变量落值 / 个人邮箱 / 非白名单外部地址
  提示级（不拦）：本地绝对路径 / 本地专有词表

用法：
  py -3.11 scripts\\scan_secrets.py                  # 只扫工作区受跟踪文件
  py -3.11 scripts\\scan_secrets.py --all-history    # 追加扫全部 git 历史 blob
  py -3.11 scripts\\scan_secrets.py --terms <文件>    # 指定本地词表（不入库）
  py -3.11 scripts\\scan_secrets.py --install-hook   # 安装 pre-commit hook
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAX_BLOB = 1024 * 1024

# Windows 控制台默认 GBK：命中片段里出现非 GBK 字符（如 U+FFFD）会让打印直接抛异常，
# 门禁因此以「崩溃」而非「命中」退出。统一按 UTF-8 输出，坏字节替换不中断扫描。
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
except Exception:  # noqa: BLE001
    pass

BLOCK_RULES: list[tuple[str, str]] = [
    ("私钥文件", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ("AWS Access Key", r"AKIA[0-9A-Z]{16}"),
    ("GitHub 令牌", r"gh[pousr]_[A-Za-z0-9]{36,}"),
    ("GitHub 细粒度令牌", r"github_pat_[A-Za-z0-9_]{20,}"),
    ("通用 sk- 密钥", r"sk-[A-Za-z0-9_\-]{20,}"),
    ("Bearer 令牌", r"[Bb]earer[ \t]+[A-Za-z0-9._~+/\-]{20,}={0,2}"),
    ("JWT", r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\."),
    ("配置里的非空 apiKey", r'"apiKey"[ \t]*:[ \t]*"[^"]{12,}"'),
    ("密钥环境变量落值",
     r"(OPENCODE_API_KEY|OPENAI_API_KEY|ANTHROPIC_API_KEY)[ \t]*=[ \t]*[^\s\"']{12,}"),
]

HINT_RULES: list[tuple[str, str]] = [
    ("本地绝对路径", r"[A-Za-z]:\\"),
]

ALLOW_SUFFIXES = (
    "127.0.0.1", "localhost", "opencode.ai", "commandcode.ai",
    "npmjs.org", "github.com", "githubusercontent.com", "example.com",
    # 产品实际对接 / 文档引用的厂商域名
    "deepseek.com", "jrsoftware.org",
    "anthropic.com", "openai.com", "microsoft.com",
    # 文档里 SVG 的 XML 命名空间（不是网络端点）
    "w3.org",
)

EMAIL_RE = re.compile(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")
URL_RE = re.compile(r"https?://[A-Za-z0-9.\-]+(:[0-9]+)?")

BLOCK_HITS = 0
HINT_HITS = 0


def fail() -> None:
    global BLOCK_HITS
    BLOCK_HITS += 1


def hint() -> None:
    global HINT_HITS
    HINT_HITS += 1


def report(name: str, where: str, line_no: int, text: str) -> None:
    snippet = text[:160]
    print(f"  [{name}] {where}:{line_no}")
    print(f"      {snippet}")


def git(*args: str) -> tuple[int, str]:
    out = subprocess.run(["git", *args], cwd=ROOT, capture_output=True,
                         text=True, encoding="utf-8", errors="replace", timeout=120)
    return out.returncode, out.stdout


def tracked_files() -> list[str]:
    code, out = git("ls-files")
    if code != 0:
        raise SystemExit("不是 git 仓库")
    return [l.strip() for l in out.splitlines() if l.strip()]


def scan_text(name: str, where: str, text: str) -> None:
    """按规则扫一段文本。"""
    lines = text.splitlines()
    for line_no, line in enumerate(lines, start=1):
        for rule_name, pattern in BLOCK_RULES:
            if re.search(pattern, line):
                report(rule_name, where, line_no, line)
                fail()
        for rule_name, pattern in HINT_RULES:
            if re.search(pattern, line):
                report(rule_name, where, line_no, line)
                hint()
        m = EMAIL_RE.search(line)
        if m and "users.noreply.github.com" not in line:
            report("个人邮箱（非 noreply）", where, line_no, line)
            fail()
        for murl in URL_RE.finditer(line):
            url = murl.group(0)
            flagged = ""
            host = url.split("://", 1)[-1].split("/")[0].split(":")[0]
            if not host or not re.match(r"^[A-Za-z0-9.\-]+$", host) or "." not in host:
                continue
            allowed = any(host == s or host.endswith("." + s)
                         for s in ALLOW_SUFFIXES)
            if not allowed:
                flagged = host
            if flagged:
                report("非白名单外部地址", where, line_no, line)
                fail()
                break


def scan_workspace(files: list[str]) -> None:
    print(f"扫描工作区受跟踪文件：{len(files)} 个")
    for rel in files:
        path = ROOT / rel
        try:
            data = path.read_bytes()
        except OSError:
            continue
        if b"\x00" in data[:1024]:
            continue
        try:
            text = data.decode("utf-8", errors="replace")
        except Exception:
            continue
        scan_text(rel, rel, text)


def scan_terms(terms_file: Path, files: list[str]) -> None:
    if not terms_file.is_file():
        print(f"（可选）未找到本地词表 {terms_file.name}，跳过")
        return
    print(f"读取本地词表：{terms_file}")
    words = [w.strip() for w in terms_file.read_text(encoding="utf-8",
                                                    errors="replace").splitlines()
             if w.strip() and not w.strip().startswith("#")]
    for word in words:
        for rel in files:
            path = ROOT / rel
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            for line_no, line in enumerate(text.splitlines(), start=1):
                if word in line:
                    report(f"本地词表:{word}", rel, line_no, line)
                    hint()


def scan_history() -> None:
    print("扫描全部历史对象（不只 HEAD 树）")
    combined = "|".join(p for _, p in BLOCK_RULES)
    code, out = git("cat-file", "--batch-all-objects", "--batch-check",
                    "%(objectname) %(objecttype) %(objectsize)")
    if code != 0:
        raise SystemExit("历史对象枚举失败")
    count = 0
    for line in out.splitlines():
        parts = line.split()
        if len(parts) != 3:
            continue
        oid, otype, osize = parts
        if otype != "blob":
            continue
        try:
            if int(osize) > MAX_BLOB:
                continue
        except ValueError:
            continue
        code, blob = git("cat-file", "blob", oid)
        if code != 0:
            continue
        count += 1
        if re.search(combined, blob):
            report("阻断级", f"history:{oid}", 0, f"blob {osize} 字节命中")
            fail()
    print(f"历史 blob 扫描完成，共检查 {count} 个对象")


HOOK_TEMPLATE = """#!/bin/sh
# Model-Gate 敏感信息门禁（由 scripts/scan_secrets.py --install-hook 生成）
# 任何提交都会先跑阻断级扫描；命中即拒绝提交。
cd "%~1" && py -3.11 scripts/scan_secrets.py --staged || exit 1
exit 0
"""


def install_hook() -> int:
    code, hooks = git("rev-parse", "--git-path", "hooks")
    if code != 0:
        raise SystemExit("无法定位 hooks 目录")
    hooks = hooks.strip().replace("/", "\\")
    hooks_dir = Path(hooks) if not Path(hooks).is_absolute() else Path(hooks)
    if not hooks_dir.is_absolute():
        hooks_dir = ROOT / hooks
    hooks_dir.mkdir(parents=True, exist_ok=True)
    hook = hooks_dir / "pre-commit"
    body = ("#!/bin/sh\n"
            "# Model-Gate 敏感信息门禁（scripts/scan_secrets.py --install-hook 生成）\n"
            "# 命中阻断级规则即拒绝提交。跳过请用 git commit --no-verify（不推荐）。\n"
            f'cd "{ROOT.as_posix()}" && py -3.11 scripts/scan_secrets.py --staged || exit 1\n'
            "exit 0\n")
    hook.write_text(body, encoding="utf-8")
    print(f"已安装 pre-commit hook：{hook}")
    print("提示：Windows 下需在支持 sh 的 Git 环境执行；PowerShell 用户可手动执行：")
    print("      py -3.11 scripts\\scan_secrets.py --staged")
    return 0


def scan_staged() -> int:
    """只扫本次已暂存内容（供 pre-commit 使用）。"""
    code, out = git("diff", "--cached", "--name-only", "--diff-filter=ACM")
    if code != 0:
        raise SystemExit("无法读取暂存区")
    files = [l.strip() for l in out.splitlines() if l.strip()]
    if not files:
        print("暂存区无可扫文件")
        return 0
    print(f"扫描暂存文件：{len(files)} 个")
    for rel in files:
        code, text = git("show", f":{rel}")
        if code != 0:
            continue
        scan_text(rel, f"staged:{rel}", text)
    return 1 if BLOCK_HITS else 0


def main() -> int:
    args = sys.argv[1:]
    all_history = False
    terms_file = ROOT / ".sensitive-terms"
    if "--install-hook" in args:
        return install_hook()
    if "--staged" in args:
        return scan_staged()
    if "--all-history" in args:
        all_history = True
    if "--terms" in args:
        i = args.index("--terms")
        if i + 1 < len(args):
            terms_file = Path(args[i + 1])

    files = tracked_files()
    scan_workspace(files)
    scan_terms(terms_file, files)
    if all_history:
        scan_history()

    print()
    if BLOCK_HITS:
        print(f"[FAIL] 阻断级命中 {BLOCK_HITS} 处 —— 禁止提交/发布")
        return 1
    print(f"[OK] 阻断级零命中（提示级 {HINT_HITS} 处，公开前可酌情清理）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
