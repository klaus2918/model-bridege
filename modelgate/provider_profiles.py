# -*- coding: utf-8 -*-
"""provider 预设注册表：加载 provider_profiles.json，提供 profile 判定与 compat 解析。

与 Rust 端共用同一份数据（`modelgate/provider_profiles.json`）；
判定与合并规则见 docs/architecture/upstream-config-model.md。
"""
from __future__ import annotations

import copy
import json
from pathlib import Path

REGISTRY_PATH = Path(__file__).with_name("provider_profiles.json")

# compat 中由注册表/解析器负责的完整键集合（与旧 _compat_preset 输出保持一致）。
_COMPAT_KEYS = (
    "profile", "auth", "session", "headers", "endpoint", "thinking",
    "toolChoice", "limits", "zdr", "versionHeader", "environmentHeader",
)

_BASE_PROFILE = "openai"

_registry_cache: dict | None = None


class ProviderProfilesError(RuntimeError):
    """注册表缺失或损坏；调用方按兼容策略回退。"""


def load_registry(force: bool = False) -> dict:
    """读取注册表（进程内缓存）。损坏时抛 ProviderProfilesError。"""
    global _registry_cache
    if _registry_cache is not None and not force:
        return _registry_cache
    try:
        raw = json.loads(REGISTRY_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise ProviderProfilesError(f"注册表不存在: {REGISTRY_PATH}") from exc
    except Exception as exc:  # noqa: BLE001
        raise ProviderProfilesError(f"注册表解析失败: {exc}") from exc
    profiles = raw.get("profiles")
    if not isinstance(profiles, dict) or not profiles:
        raise ProviderProfilesError("注册表缺少 profiles")
    _registry_cache = raw
    return raw


def profile_ids() -> list[str]:
    """注册表内的 profile id 列表（保持文件顺序）。"""
    return list(load_registry()["profiles"].keys())


def is_known_profile(profile_id: str) -> bool:
    return str(profile_id or "").lower() in load_registry()["profiles"]


def profiles_meta() -> list[dict]:
    """供 /api/config 与 /api/config/validate 展示的预设元数据。"""
    rows = []
    for pid, entry in load_registry()["profiles"].items():
        rows.append({
            "id": pid,
            "label": entry.get("label") or pid,
            "description": entry.get("description") or "",
            "capabilities": merge_capabilities(pid),
        })
    return rows


def detect_profile(profile_hint: str, protocol: str, base_url: str) -> str:
    """判定生效 profile：显式 > 域名（按 priority）> 协议预设；保留 DeepSeek shim。"""
    registry = load_registry()
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
                effective = protocol if protocol in profiles else _BASE_PROFILE
    # 兼容 shim：显式 anthropic + 含 deepseek 的 host → DeepSeek 语义（历史行为，保留）。
    if effective == "anthropic" and "deepseek" in host:
        effective = "deepseek"
    return effective


def _deep_merge(base: dict, extra: dict) -> dict:
    out = copy.deepcopy(base)
    for key, value in (extra or {}).items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _deep_merge(out[key], value)
        else:
            out[key] = copy.deepcopy(value)
    return out


def profile_compat(profile_id: str) -> dict:
    """注册表中某 profile 的完整 compat（基线 → profile → 变体不参与）。"""
    registry = load_registry()
    profiles = registry["profiles"]
    entry = profiles.get(profile_id) or profiles[_BASE_PROFILE]
    return _deep_merge(profiles[_BASE_PROFILE].get("compat", {}), entry.get("compat", {}))


def resolve_compat(profile_hint: str, protocol: str, base_url: str) -> dict:
    """注册表解析：返回旧 `_compat_preset` 全字段 + profiles → catalog / capabilities。"""
    registry = load_registry()
    profiles = registry["profiles"]
    effective = detect_profile(profile_hint, protocol, base_url)
    if effective in profiles:
        entry = profiles[effective]
        compat = _deep_merge(profiles[_BASE_PROFILE].get("compat", {}), entry.get("compat", {}))
        variant = (entry.get("variants") or {}).get(protocol, {})
        compat = _deep_merge(compat, variant)
    else:
        # 未知 profile：保持原名 + 基线默认值（与旧实现一致，交由诊断层提示）。
        compat = copy.deepcopy(profiles[_BASE_PROFILE].get("compat", {}))
    compat["profile"] = effective
    out = {key: compat.get(key) for key in _COMPAT_KEYS}
    out["catalog"] = copy.deepcopy(compat.get("catalog") or {})
    out["capabilities"] = merge_capabilities(effective, compat.get("capabilities"))
    return out


def merge_capabilities(profile_id: str, *overrides: dict | None) -> dict:
    """能力合并：内置默认 → 注册表 profile → 上游 compat → 模型级（后者覆盖前者）。"""
    defaults = {"stream": True, "tools": True, "vision": False,
                "thinking": False, "cache": False, "systemPrompt": True}
    merged = dict(defaults)
    try:
        registry_caps = profile_compat(profile_id).get("capabilities")
    except ProviderProfilesError:
        registry_caps = None
    for layer in (registry_caps, *overrides):
        if isinstance(layer, dict):
            for key in defaults:
                if isinstance(layer.get(key), bool):
                    merged[key] = layer[key]
    return merged
