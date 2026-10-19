# -*- coding: utf-8 -*-
"""实测：load_env_file → resolve_api_key 链在 go 上游上是否拿到 key。"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from modelgate.config import load_config, load_env_file, resolve_api_key, upstreams_of  # noqa: E402


def main() -> int:
    cfg = load_config(ROOT / "bridge.config.json")
    print("=== 调用 load_env_file 之前 ===")
    go_raw = (cfg.get("upstreams") or {}).get("go") or {}
    print(f"  配置里 go.apiKeyEnv = {go_raw.get('apiKeyEnv')!r}")
    print(f"  配置里 go.apiKey    = {go_raw.get('apiKey', '<未设>')!r}")

    import os
    print(f"  os.environ[OPENCODE_API_KEY] = "
          f"{'已设置' if os.environ.get('OPENCODE_API_KEY') else '未设置'}")

    load_env_file(cfg)
    print("\n=== 调用 load_env_file 之后 ===")
    from modelgate.util import access_log_enabled  # noqa: F401,E402
    value = os.environ.get("OPENCODE_API_KEY")
    print(f"  os.environ[OPENCODE_API_KEY] = "
          f"{'已设置 (' + str(len(value)) + ' chars)' if value else '未设置'}")

    print("\n=== resolve_api_key(go) ===")
    key, source = resolve_api_key(go_raw)
    print(f"  source = {source}")
    print(f"  key    = {'<' + str(len(key)) + ' chars>' if key and key != 'any-key' else repr(key)}")

    print("\n=== upstreams_of(cfg) 归一化结果 ===")
    ups = {u["name"]: u for u in upstreams_of(cfg)}
    for name in ("go", "commandcode"):
        u = ups[name]
        print(f"  {name}: source={u['apiKeySource']} "
              f"key={'<已配置>' if u['apiKey'] and u['apiKey'] != 'any-key' else repr(u['apiKey'])}")

    ok = ups["go"]["apiKeySource"] == "env" and ups["go"]["apiKey"] != "any-key"
    print("\n" + ("=> go 密钥解析正常" if ok else "=> go 密钥解析异常"))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
