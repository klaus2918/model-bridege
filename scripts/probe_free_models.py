# -*- coding: utf-8 -*-
"""探测 go / zen 上游的模型目录,并逐个测试免费模型(-free / 0 价格)的可用性。

用法：
  py -3.11 scripts\\probe_free_models.py --list      # 只看模型目录
  py -3.11 scripts\\probe_free_models.py             # 列目录 + 测试所有 free 模型
  py -3.11 scripts\\probe_free_models.py --model X   # 只测指定模型
"""
from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import httpx

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

from modelgate.config import load_config, load_env_file, upstreams_of  # noqa: E402

LIST_ONLY = "--list" in sys.argv
ONLY_MODEL = None
if "--model" in sys.argv:
    ONLY_MODEL = sys.argv[sys.argv.index("--model") + 1]

TARGETS = ("go", "zen")


def headers_for(up: dict) -> dict:
    headers = {"content-type": "application/json"}
    key = up.get("apiKey") or ""
    if key:
        headers["authorization"] = f"Bearer {key}"
    if up.get("sessionHeader"):
        headers["x-opencode-session"] = "probe-free-0000000000000000"
    return headers


def fetch_catalog(up: dict) -> list[dict]:
    """GET {baseUrl}/v1/models(与代理内部拼接规则一致),兼容 data[] / dict 两种形态。"""
    url = f"{up['baseUrl']}/v1/models"
    try:
        res = httpx.get(url, headers=headers_for(up), timeout=30.0)
    except Exception as exc:  # noqa: BLE001
        print(f"  [ERR] GET {url} -> {type(exc).__name__}: {exc}")
        return []
    print(f"  [HTTP {res.status_code}] GET {url}")
    if res.status_code >= 400:
        print(f"        {res.text[:300]}")
        return []
    try:
        data = res.json()
    except Exception:  # noqa: BLE001
        print(f"        (非 JSON) {res.text[:200]}")
        return []
    rows = data.get("data") if isinstance(data, dict) else data
    if not isinstance(rows, list):
        print(f"        结构未知: {json.dumps(data, ensure_ascii=False)[:300]}")
        return []
    return [r for r in rows if isinstance(r, dict)]


def is_free(row: dict) -> bool:
    mid = str(row.get("id") or row.get("slug") or row.get("name") or "")
    if mid.endswith("-free") or "free" in mid.lower():
        return True
    # 价格字段为 0 也算免费
    pricing = row.get("pricing") or {}
    if isinstance(pricing, dict) and pricing:
        try:
            nums = [float(v) for v in pricing.values() if v is not None]
            if nums and all(n == 0 for n in nums):
                return True
        except (TypeError, ValueError):
            pass
    return False


def id_of(row: dict) -> str:
    return str(row.get("id") or row.get("slug") or row.get("name") or "")


def test_model(up: dict, model: str) -> tuple[bool, str]:
    url = f"{up['baseUrl']}/v1/chat/completions"
    payload = {
        "model": model,
        "max_tokens": 32,
        "messages": [{"role": "user", "content": "只回复两个字:pong"}],
    }
    started = time.time()
    try:
        res = httpx.post(url, headers=headers_for(up), json=payload, timeout=60.0)
    except Exception as exc:  # noqa: BLE001
        return False, f"异常 {type(exc).__name__}: {exc}"
    elapsed = int((time.time() - started) * 1000)
    body = res.text[:400].replace("\n", " ")
    if res.status_code >= 400:
        return False, f"HTTP {res.status_code} {elapsed}ms {body[:220]}"
    # 尝试从响应里取文本
    text = ""
    try:
        j = res.json()
        choices = j.get("choices") or []
        if choices:
            text = str((choices[0].get("message") or {}).get("content") or "")[:80]
    except Exception:  # noqa: BLE001
        pass
    return True, f"HTTP {res.status_code} {elapsed}ms text={text!r}"


def main() -> int:
    cfg = load_config(ROOT / "bridge.config.json")
    load_env_file(cfg)
    upstreams = {u["name"]: u for u in upstreams_of(cfg)}

    all_rows: dict[str, list[dict]] = {}
    print("=== [1] 模型目录 ===")
    for name in TARGETS:
        up = upstreams.get(name)
        if not up:
            print(f"[{name}] 不在配置中,跳过")
            continue
        print(f"\n[{name}] baseUrl={up['baseUrl']} key={'(已配置)' if up.get('apiKey') else '(无)'}")
        rows = fetch_catalog(up)
        all_rows[name] = rows
        print(f"  共 {len(rows)} 个模型:")
        for row in rows:
            tag = "FREE" if is_free(row) else "    "
            print(f"    [{tag}] {id_of(row)}")

    if LIST_ONLY:
        return 0

    print("\n=== [2] 免费模型连通性测试 ===")
    free_models: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()
    for name, rows in all_rows.items():
        for row in rows:
            mid = id_of(row)
            if mid and is_free(row) and (name, mid) not in seen:
                seen.add((name, mid))
                free_models.append((name, mid))

    # 配置里已声明但目录里没有的 free 模型也要测
    for name in TARGETS:
        up = upstreams.get(name)
        if not up:
            continue
        for mid in up.get("models") or []:
            if "free" in mid.lower() and (name, mid) not in seen:
                seen.add((name, mid))
                free_models.append((name, mid))

    if ONLY_MODEL:
        matched = [(n, m) for (n, m) in free_models if m == ONLY_MODEL]
        free_models = matched or [("go", ONLY_MODEL), ("zen", ONLY_MODEL)]

    results = []
    for name, mid in free_models:
        up = upstreams.get(name)
        if not up:
            continue
        print(f"\n[{name}] {mid}")
        ok, detail = test_model(up, mid)
        print(f"        -> {'OK  ' if ok else 'FAIL'} {detail}")
        results.append((name, mid, ok))

    print("\n=== [3] 结论 ===")
    for name, mid, ok in results:
        print(f"  {'可用' if ok else '不可用'}  [{name}] {mid}")
    n_ok = sum(1 for _, _, ok in results if ok)
    print(f"\n合计 {len(results)} 个,可用 {n_ok} 个")
    return 0 if n_ok == len(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
