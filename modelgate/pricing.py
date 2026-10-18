# -*- coding: utf-8 -*-
"""费用估算：按本地单价表把 token 用量折算成金额。

设计（见 .op/changes/model-gate-desktop/design-completeness.md §2.2）：

- 单价单位是**每 100 万 token 的金额**，字段 ``input`` / ``output`` /
  ``cacheRead`` / ``cacheWrite``；币种由 ``currency`` 声明（默认 CNY）。
- 规则匹配优先级（先命中先用）：``<upstream>/<model>`` → ``<upstream>``
  → ``*/<model>`` → ``default``。
- 单价表随配置持久化（``bridge.config.json`` 的 ``pricing`` 键，也进 SQLite
  的 settings 覆盖），**不含任何密钥**。
- 本工具只做**本地口径估算**，不对账上游账单；未配置单价时费用为 0，
  UI 需据此显示「未配置单价」而不是「0 元」。

容错优先：单价表来自用户手写 JSON，任何非法输入都退化为空表而不抛异常 ——
费用统计坏掉不能影响代理主流程。
"""
from __future__ import annotations

import json
from typing import Any, Optional

DEFAULT_CURRENCY = "CNY"
PER_UNIT = 1_000_000.0
RATE_FIELDS = ("input", "output", "cacheRead", "cacheWrite")


def _num(value: Any) -> float:
    try:
        return float(value or 0)
    except (TypeError, ValueError):
        return 0.0


def _rate(value: Any, fallback: Optional[dict] = None) -> dict:
    """规整一条费率。

    **字段级继承**：规则里「没写」的字段回落到 default，而显式写 0 就是 0。
    这条语义很关键 —— 否则只写了 input/output 的规则会把缓存价悄悄算成免费，
    费用被系统性低估。
    """
    value = value if isinstance(value, dict) else {}
    fallback = fallback or {}
    out: dict = {}
    for field in RATE_FIELDS:
        if field in value and value[field] is not None:
            out[field] = _num(value[field])
        else:
            out[field] = _num(fallback.get(field))
    return out


def normalize_pricing(raw: Any) -> dict:
    """把任意输入规整为 ``{currency, default, rules}``（永不抛异常）。"""
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except Exception:
            raw = {}
    if not isinstance(raw, dict):
        raw = {}

    rules_in = raw.get("rules")
    if not isinstance(rules_in, dict):
        # 兼容扁平写法：{"go": {...}, "*/glm-5.3-flash": {...}} 直接放在顶层
        rules_in = {
            str(k): v for k, v in raw.items()
            if isinstance(v, dict) and k not in ("default", "currency")
        }

    default_rate = _rate(raw.get("default"))
    return {
        "currency": str(raw.get("currency") or DEFAULT_CURRENCY),
        "default": default_rate,
        "rules": {
            str(k): _rate(v, default_rate)
            for k, v in rules_in.items()
        },
    }


def candidate_keys(upstream: Optional[str], model: Optional[str]) -> list[str]:
    """按优先级返回候选规则键（先命中先用）。"""
    up = str(upstream or "").strip()
    md = str(model or "").strip()
    keys: list[str] = []
    if up and md:
        keys.append(f"{up}/{md}")
    if up:
        keys.append(up)
    if md:
        keys.append(f"*/{md}")
    return keys


def resolve_rate(pricing: Any, upstream: Optional[str],
                 model: Optional[str]) -> tuple[dict, str]:
    """解析费率：返回 ``(费率, 命中键)``；未命中任何规则时返回 default。"""
    table = normalize_pricing(pricing)
    for key in candidate_keys(upstream, model):
        if key in table["rules"]:
            return table["rules"][key], key
    return table["default"], "default"


def is_configured(pricing: Any) -> bool:
    """是否配置了任何非零单价（用于区分「0 元」与「未配置单价」）。"""
    table = normalize_pricing(pricing)
    rates = list(table["rules"].values()) + [table["default"]]
    return any(any(rate[field] > 0 for field in RATE_FIELDS) for rate in rates)


def currency_of(pricing: Any) -> str:
    return normalize_pricing(pricing)["currency"]


def cost_of(pricing: Any, *, upstream: Optional[str], model: Optional[str],
            prompt_tokens: int = 0, completion_tokens: int = 0,
            cache_read_tokens: int = 0, cache_write_tokens: int = 0) -> float:
    """计算单条记录的费用（保留 6 位小数，便于与 SQLite REAL 比对）。"""
    rate, _key = resolve_rate(pricing, upstream, model)
    total = (
        int(prompt_tokens or 0) * rate["input"]
        + int(completion_tokens or 0) * rate["output"]
        + int(cache_read_tokens or 0) * rate["cacheRead"]
        + int(cache_write_tokens or 0) * rate["cacheWrite"]
    ) / PER_UNIT
    return round(total, 6)


def save_currency(pricing: Any, currency: str) -> dict:
    """返回一份只替换币种的新单价表（供 API 层局部更新用）。"""
    table = normalize_pricing(pricing)
    table["currency"] = str(currency or DEFAULT_CURRENCY)
    return table
