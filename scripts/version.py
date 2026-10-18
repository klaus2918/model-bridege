# -*- coding: utf-8 -*-
"""打印 modelgate/__init__.py 里的 __version__（供 build.bat 读取）。

拆成独立脚本是为了避免在 .bat 里嵌套引号与正则。
"""
import re
import sys
from pathlib import Path


def main() -> int:
    path = Path(__file__).resolve().parent.parent / "modelgate" / "__init__.py"
    text = path.read_text(encoding="utf-8")
    match = re.search(r'__version__\s*=\s*"([^"]+)"', text)
    if not match:
        print("unknown", file=sys.stderr)
        return 1
    sys.stdout.write(match.group(1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
