# -*- coding: utf-8 -*-
"""op-done 步骤 3：清理过程噪音（只删过程产物，保留交付证据）。

保留：源码、tests/、scripts/（含 verify_*.py 验证脚本）、docs/、dist/、.op/ 变更文档、README。
删除：根目录 _tmp_*.*、*.out、*.err、*.bak-*、临时 spec 备份。
"""
from pathlib import Path

ROOT = Path(".")
CHANGE = ROOT / ".op" / "changes" / "model-gate-desktop"
patterns = ["_tmp_*.*", "*.out", "*.err", "*.bak-*", "*.spec.bak", "*.stackdump"]

removed: list[str] = []
kept_note: list[str] = []
for pat in patterns:
    for p in sorted(ROOT.glob(pat)):
        if not p.is_file():
            continue
        if p.name in ("model-bridge.js",):  # 绝不误删源码
            kept_note.append(p.name)
            continue
        try:
            p.unlink()
            removed.append(p.name)
        except Exception as exc:  # noqa: BLE001
            kept_note.append(f"{p.name} (删除失败: {exc})")

# 交付证据：确认仍在
evidence = ["tests", "scripts", "docs", "dist", "modelgate", "README.md"]
missing = [e for e in evidence if not (ROOT / e).exists()]

report = [
    "# cleanup-report（model-gate-desktop）",
    "",
    f"- 删除文件数：**{len(removed)}**",
    f"- 保留证据：{', '.join(e for e in evidence if e not in missing)}",
    f"- 缺失证据：{', '.join(missing) if missing else '无'}",
    "",
    "## 删除清单（过程噪音）",
    "",
]
report += [f"- `{n}`" for n in removed]
if kept_note:
    report += ["", "## 待人工确认（未删除）", ""] + [f"- {n}" for n in kept_note]
CHANGE.mkdir(parents=True, exist_ok=True)
(CHANGE / "cleanup-report.md").write_text("\n".join(report) + "\n", encoding="utf-8")
print(f"removed={len(removed)} kept_note={len(kept_note)} missing_evidence={missing}")
print("report ->", CHANGE / "cleanup-report.md")
