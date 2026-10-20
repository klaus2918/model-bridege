# -*- coding: utf-8 -*-
"""注册表一致性检查：provider_profiles.json 结构校验 + 与当前 Python 预设行为对照。

用法：
    py -X utf8 scripts/provider_profiles_check.py

判据：
1. 注册表结构合法（五套 profile、compat 字段、capabilities / catalog 形态）；
2. golden fixtures 与当前 `_compat_preset` 输出一致（行为漂移即失败）；
3. 除未知 profile 外，注册表解析结果与当前实现逐字段一致。

退出码：0 全部通过；1 存在失败项。
"""
from __future__ import annotations

import copy
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))

from modelgate.config import _compat_preset  # noqa: E402

REGISTRY_PATH = REPO / "modelgate" / "provider_profiles.json"
GOLDEN_PATH = REPO / "tests" / "fixtures" / "provider_profiles_golden.json"

LEGACY_COMPAT_KEYS = (
    "profile", "auth", "session", "headers", "endpoint", "thinking",
    "toolChoice", "limits", "zdr", "versionHeader", "environmentHeader",
)
CAPABILITY_KEYS = ("stream", "tools", "vision", "thinking", "cache", "systemPrompt")
CATALOG_KEYS = ("source", "path", "plan")
PROFILE_IDS = ("openai", "opencode", "anthropic", "commandcode", "deepseek")

_failures: list[str] = []


def fail(message: str) -> None:
    _failures.append(message)


def deep_merge(base: dict, extra: dict) -> dict:
    out = copy.deepcopy(base)
    for key, value in (extra or {}).items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = deep_merge(out[key], value)
        else:
            out[key] = copy.deepcopy(value)
    return out


def resolve_from_registry(registry: dict, profile_hint: str, protocol: str, base_url: str):
    """按注册表解析预设；未知 profile 返回 None（由诊断码 unknown_profile 处理）。"""
    profiles = registry["profiles"]
    effective = str(profile_hint or "").strip().lower()
    host = str(base_url or "").lower()
    if not effective:
        if protocol == "commandcode":
            effective = "commandcode"
        else:
            detected = [
                pid for pid, entry in profiles.items()
                if any(s in host for s in entry.get("detect", {}).get("baseUrlContains", []))
            ]
            if detected:
                effective = max(detected, key=lambda pid: profiles[pid].get("priority", 0))
            else:
                effective = protocol if protocol in profiles else "openai"
    if effective not in profiles:
        return None
    # 兼容 shim：显式 anthropic + 含 deepseek 的 host → DeepSeek 语义（历史行为，保留）。
    if effective == "anthropic" and "deepseek" in host:
        effective = "deepseek"
    entry = profiles[effective]
    compat = deep_merge(profiles["openai"]["compat"], entry.get("compat", {}))
    variant = (entry.get("variants") or {}).get(protocol, {})
    compat = deep_merge(compat, variant)
    compat["profile"] = effective
    return compat


def validate_registry(registry: dict) -> None:
    if registry.get("version") != 1:
        fail("registry.version 必须为 1")
    profiles = registry.get("profiles")
    if not isinstance(profiles, dict) or not profiles:
        fail("registry.profiles 缺失或为空")
        return
    for pid in PROFILE_IDS:
        entry = profiles.get(pid)
        if not isinstance(entry, dict):
            fail(f"缺少 profile: {pid}")
            continue
        for key in ("label", "description", "priority", "protocols", "compat"):
            if key not in entry:
                fail(f"{pid}: 缺少字段 {key}")
        protocols = entry.get("protocols")
        if not isinstance(protocols, list) or not protocols:
            fail(f"{pid}: protocols 必须是非空数组")
        detect = entry.get("detect", {})
        contains = detect.get("baseUrlContains", [])
        if not isinstance(contains, list) or any(not isinstance(s, str) for s in contains):
            fail(f"{pid}: detect.baseUrlContains 必须是字符串数组")
        defaults = entry.get("defaults")
        if not isinstance(defaults, dict) or set(defaults) != {"fetchModels", "autoModels", "pauseTurn"}:
            fail(f"{pid}: defaults 必须包含且仅包含 fetchModels/autoModels/pauseTurn")
        compat = entry.get("compat")
        if not isinstance(compat, dict):
            fail(f"{pid}: compat 必须是对象")
            continue
        allowed = set(LEGACY_COMPAT_KEYS) | {"catalog", "capabilities"}
        unknown = set(compat) - allowed
        if unknown:
            fail(f"{pid}: compat 含未允许字段 {sorted(unknown)}")
        capabilities = compat.get("capabilities")
        if capabilities is not None:
            if not isinstance(capabilities, dict) or set(capabilities) - set(CAPABILITY_KEYS):
                fail(f"{pid}: capabilities 字段必须属于 {CAPABILITY_KEYS}")
            elif any(not isinstance(v, bool) for v in capabilities.values()):
                fail(f"{pid}: capabilities 值必须为布尔")
        catalog = compat.get("catalog")
        if catalog is not None:
            if not isinstance(catalog, dict) or set(catalog) - set(CATALOG_KEYS) or "source" not in catalog:
                fail(f"{pid}: catalog 必须包含 source 等合法键 {CATALOG_KEYS}")
        for variant_name, variant in (entry.get("variants") or {}).items():
            if variant_name not in protocols:
                fail(f"{pid}: variants.{variant_name} 不在 protocols 中")
            if not isinstance(variant, dict) or set(variant) - allowed:
                fail(f"{pid}: variants.{variant_name} 含未允许字段")
    base = profiles.get("openai", {}).get("compat", {})
    # profile 由解析器按生效 profile 回填，基线不必显式声明；其余字段必须齐全。
    missing = [k for k in LEGACY_COMPAT_KEYS if k != "profile" and k not in base]
    if missing:
        fail(f"openai 基线 compat 必须显式声明全部字段，缺少 {missing}")


def main() -> int:
    registry = json.loads(REGISTRY_PATH.read_text(encoding="utf-8"))
    golden = json.loads(GOLDEN_PATH.read_text(encoding="utf-8"))
    validate_registry(registry)

    cases = golden.get("cases") or []
    if not cases:
        fail("golden fixtures 为空")
    for case in cases:
        name = case.get("name", "?")
        inp = case.get("input", {})
        expect = case.get("expect", {})
        live_full = _compat_preset(inp.get("profile", ""), inp.get("protocol", ""), inp.get("baseUrl", ""))
        live = {key: live_full.get(key) for key in LEGACY_COMPAT_KEYS}
        if live != expect:
            fail(f"{name}: fixtures 与当前实现漂移，请重新生成 golden")
        if not isinstance(live_full.get("catalog"), dict) or not live_full.get("catalog"):
            fail(f"{name}: 解析结果缺少 catalog")
        caps = live_full.get("capabilities")
        if not isinstance(caps, dict) or set(caps) != set(CAPABILITY_KEYS):
            fail(f"{name}: 解析结果 capabilities 形态不正确")
        resolved = resolve_from_registry(registry, inp.get("profile", ""), inp.get("protocol", ""), inp.get("baseUrl", ""))
        if resolved is None:
            continue  # 未知 profile：注册表不覆盖场景
        got = {key: resolved.get(key) for key in LEGACY_COMPAT_KEYS}
        want = {key: expect.get(key) for key in LEGACY_COMPAT_KEYS}
        if got != want:
            fail(f"{name}: 注册表解析结果与当前实现不一致\n  got ={json.dumps(got, ensure_ascii=False, sort_keys=True)}\n  want={json.dumps(want, ensure_ascii=False, sort_keys=True)}")

    if _failures:
        for item in _failures:
            print(f"FAIL: {item}")
        print(f"存在 {len(_failures)} 项失败")
        return 1
    print(f"OK: registry check passed ({len(cases)} cases, {len(PROFILE_IDS)} profiles)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
