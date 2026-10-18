# -*- coding: utf-8 -*-
"""向 plan.md 追加密钥安全加固记录（幂等）。"""
from __future__ import annotations

from pathlib import Path

PLAN = Path(__file__).resolve().parent.parent / ".op" / "changes" / "model-gate-desktop" / "plan.md"

BLOCK = """

## 密钥安全加固（方案 A：配置文件不入库 + 明文不落盘）

### 背景
`bridge.config.json` 此前被 git 跟踪，且保存逻辑会把明文 `apiKey` 写回文件。
一旦将来有人在配置页填写明文密钥，提交即泄入 git 历史且不可擦除。

### 已执行

1. **git 历史审计**（`scripts/audit_git_history_secrets.py`）：3 个历史版本 + 20 个文件
   全量嗅探，**未发现明文密钥** ⇒ 无需吊销/更换 key。
2. **脱离跟踪**：`git rm --cached bridge.config.json`（本地文件保留）。
   注意：`.gitignore` 对已索引文件无效，必须显式移除；后续 `git add` 曾把它加回，
   已再次移除并复验 `git ls-files` 为空。
3. **.gitignore 补强**：新增 `/bridge.config.json`、`/data/`（含 modelgate.db）、
   `.env.*`、`.jcode/`、`**/auth.json`、`*.key/*.pem/*.token`、`/build/`、探测临时输出。
4. **模板文件**：`bridge.config.example.json`（由 `scripts/make_config_template.py`
   生成，含 `_doc`/`_keys` 说明与 apiKeyEnv/apiKeyFile 引用，**零明文**）。
5. **工具侧硬约束**（`modelgate/api.py::_save_config`）：
   - 非空明文 `apiKey` 只进内存，**绝不写入 `bridge.config.json` 与 SQLite settings**
   - 空字符串（＝不发鉴权头）与 `apiKeyEnv`/`apiKeyFile` 引用不受影响
   - 保存后回灌运行态，上游不失效；响应 note 明确告知「仅保留在内存」
   - 曾因 `not up.get("apiKey")` 误判（占位值 `any-key` 是真值）导致回灌失效，
     已改为无条件重建，由 `tests/test_secret_guard.py` 4 条用例钉住
6. **提交门禁**：`scripts/scan_secrets.py`（scan-sensitive.sh 的 Windows 原生等价物，
   本机无 bash），规则一致：私钥 / AWS / GitHub 令牌 / sk- / Bearer / JWT /
   非空 apiKey / 密钥环境变量落值 / 个人邮箱 / 非白名单外链 → 阻断级 exit 1。
   已安装 `.git/hooks/pre-commit`，并用真实 `git commit` 验证拦截生效。
   自测 `scripts/selftest_scan_secrets.py` 8/8 通过（含阴性对照不误伤）。

### 验证
| 项 | 结果 |
|----|------|
| git 历史明文扫描 | 3 版本 + 20 文件，零命中 |
| `scripts/audit_secrets.py` | `SECRET_AUDIT OK`（零明文 / 路径全覆盖 / 未跟踪） |
| 扫描器自测 | 8/8 通过（7 命中 + 1 阴性） |
| pre-commit 拦截 | 真实提交被拦，仓库历史未受影响 |
| pytest 全量 | **75 passed** |
"""


def main() -> int:
    text = PLAN.read_text(encoding="utf-8")
    if "密钥安全加固" in text:
        print("PLAN_ALREADY_UPDATED")
        return 0
    if not text.endswith("\n"):
        text += "\n"
    PLAN.write_text(text + BLOCK, encoding="utf-8")
    print(f"PLAN_APPENDED +{len(BLOCK)} chars, total {len(PLAN.read_text(encoding='utf-8'))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
