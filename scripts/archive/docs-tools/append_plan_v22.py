# -*- coding: utf-8 -*-
"""把 envFile 缺省语义缺陷与修复记录追加到 plan.md（幂等）。"""
from __future__ import annotations

from pathlib import Path

PLAN = Path(__file__).resolve().parent.parent / ".op" / "changes" / "model-gate-desktop" / "plan.md"

BLOCK = """

## 第三轮缺陷修复：apiKeyEnv 全部失效（上游 401 Invalid API key）

### 现象
`go` 上游原本可正常使用（有 key），移植后所有请求返回
`401 Invalid API key`；`/api/upstreams/health` 也显示 `keySource=none, key=any-key`。

### 根因
`modelgate/config.py` 的 `load_env_file()` 把缺省值写反了：

```python
# 移植时写的（错）
spec = config.get("envFile", False)   # 缺省 = 禁用
# Node 版 model-bridge 原逻辑
config.envFile === false ? null : (config.envFile ?? ~/.jcode/.env)   # 缺省 = 读取
```

用户配置未写 `envFile` ⇒ 取到 `False` ⇒ 函数直接 return ⇒
`~/.jcode/.env` 从未被读取 ⇒ `OPENCODE_API_KEY` 不在 `os.environ` ⇒
实际发出占位 key ⇒ 上游 401。

**key 本身一直都在**：`~/.jcode/.env` 中 `OPENCODE_API_KEY` 为 67 字符。
两个自检脚本结论曾相反，是因为 `scripts/check_env_key.py` 自行直读文件，
而真实链路走 `load_env_file()`。

### 修复
缺省改为 `True`，并支持 `envFile` 传自定义路径字符串；显式 `false` 仍禁用，
与 Node 语义一致。空值不注入、不覆盖已有环境变量，均保持原语义。

### 验证
| 项 | 结果 |
|----|------|
| `scripts/probe_key_resolution.py` | `source=env`，key 67 字符 |
| **`scripts/verify_env_fix.py`（打包后 exe + 真实 `.env` + 真实配置）** | `go` 上游 **HTTP 200**，返回 `glm-5.3-flash` 正常补全（含 `reasoning_content` 与 `usage`）；`go.keySource=env`、`keyConfigured=true` |
| `tests/test_env_file.py`（新增 8 条） | passed：缺省读取 / 空值不注入 / 不覆盖已有 / 显式 false 禁用 / 自定义路径 / 文件缺失不报错 / 端到端 key 解析 / 占位 key |
| 全量 `pytest` | **72 passed** |

### 安全边界（对应用户此前的隐私要求）
- `~/.jcode/.env` 属于外部凭据文件：只读取、从不写入、不进日志、不外发
- `bridge.config.json` 与 `data/modelgate.db` 中的密钥仅本机留存
- `/api/config` 始终脱敏返回（仅 `keySource` / `keyConfigured`）
- `zh-cn` 无第三方上报通道，所有请求只指向用户自己配置的上游
"""


def main() -> int:
    text = PLAN.read_text(encoding="utf-8")
    if "apiKeyEnv 全部失效" in text:
        print("PLAN_ALREADY_UPDATED")
        return 0
    if not text.endswith("\n"):
        text += "\n"
    PLAN.write_text(text + BLOCK, encoding="utf-8")
    print(f"PLAN_APPENDED +{len(BLOCK)} chars, total {len(PLAN.read_text(encoding='utf-8'))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
