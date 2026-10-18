# -*- coding: utf-8 -*-
"""本地控制面 REST：供桌面 GUI 读写配置与用量统计。

安全边界：仅绑定在本机代理端口（127.0.0.1），不对外暴露；
配置写入时双写「配置文件（对外契约）」与「SQLite settings（运行时事实源）」。
"""
from __future__ import annotations

import json
import time
from pathlib import Path

from . import __version__
from .config import model_list, save_config, upstreams_of, validate_config_shape
from .util import log

TIME_RANGES = {"1h": 3600, "24h": 86400, "7d": 604800, "30d": 2592000, "all": None}


def _query(path: str) -> tuple[str, dict]:
    if "?" not in path:
        return path, {}
    base, qs = path.split("?", 1)
    params = {}
    for pair in qs.split("&"):
        if not pair:
            continue
        key, _, value = pair.partition("=")
        params[key] = value
    return base, params


def _since_ts(params: dict) -> int | None:
    rng = params.get("range") or "24h"
    seconds = TIME_RANGES.get(rng, 86400)
    if seconds is None:
        return None
    return int(time.time()) - seconds


def _previous_window(params: dict) -> tuple[int | None, int | None]:
    """上一等长区间的 (since_ts, until_ts)，用于「与上一区间对比」。

    range=all 没有可比区间，返回 (None, None) 由调用方跳过对比。
    """
    rng = params.get("range") or "24h"
    seconds = TIME_RANGES.get(rng, 86400)
    if not seconds:
        return None, None
    end = int(time.time()) - seconds
    return end - seconds, end


def probe_one_upstream(bridge, upstream: dict, timeout: float = 6.0) -> dict:
    """单条上游即时测试（对外名称，供 /api/upstreams/probe 调用）。"""
    return _probe_one(bridge, upstream, timeout)


def _probe_one(bridge, up: dict, timeout: float = 6.0) -> dict:
    """对单个上游做最小连通性探测，返回 {ok, status, elapsed_ms, message, hint}。

    目的：面板能一眼看出「这条上游为什么不通」，而不是只看到客户端一句超时。
    """
    import httpx as _httpx

    from .proxy import upstream_error_hint

    proto = up.get("protocol")
    model = ""
    for candidate in ((bridge.config.get("defaultModel") or ""),
                      *(up.get("models") or [])):
        if candidate:
            model = candidate
            break
    headers: dict = {"content-type": "application/json"}
    key = up.get("apiKey") or ""

    if proto == "anthropic":
        url = f"{up['baseUrl']}/v1/messages"
        payload = {"model": model, "max_tokens": 1,
                   "messages": [{"role": "user", "content": "ping"}]}
        if key:
            headers["x-api-key"] = key
            headers["authorization"] = f"Bearer {key}"
        headers["anthropic-version"] = up.get("anthropicVersion", "2023-06-01")
        streaming = False
    elif proto == "commandcode":
        url = f"{up['baseUrl']}/alpha/generate"
        payload = {
            "config": {"workingDir": ".", "date": "2026-01-01",
                       "environment": "windows", "structure": [],
                       "isGitRepo": False, "currentBranch": "",
                       "mainBranch": "", "gitStatus": "", "recentCommits": []},
            "memory": None, "taste": None, "skills": None,
            "permissionMode": "standard",
            "threadId": "00000000-0000-4000-8000-000000000000",
            "params": {"model": model, "messages": [
                {"role": "user", "content": [{"type": "text", "text": "ping"}]}],
                "tools": [], "system": [], "max_tokens": 1, "stream": True},
        }
        headers["user-agent"] = "cli"
        if key:
            headers["authorization"] = f"Bearer {key}"
        if up.get("cliEnvironment"):
            headers["x-cli-environment"] = up["cliEnvironment"]
        headers["x-session-id"] = "sess_0000000000000000"
        if up.get("cmdZdr", True):
            headers["x-cmd-zdr"] = "1"
        adapter = bridge.adapters.get("commandcode")
        if adapter is not None:
            version = adapter.cli_version(up)
            if version:
                headers["x-command-code-version"] = version
        streaming = True
    else:
        url = f"{up['baseUrl']}/v1/chat/completions"
        payload = {"model": model, "max_tokens": 1,
                   "messages": [{"role": "user", "content": "ping"}]}
        if key:
            headers["authorization"] = f"Bearer {key}"
        if up.get("sessionHeader"):
            headers["x-opencode-session"] = "probe_0000000000000000"
        streaming = False

    started = time.time()
    status = 0
    message = ""
    try:
        with _httpx.Client(timeout=_httpx.Timeout(timeout, connect=4.0)) as client:
            if streaming:
                with client.stream("POST", url, headers=headers,
                                   json=payload) as res:
                    status = res.status_code
                    for line in res.iter_lines():
                        if line.strip():
                            message = line.strip()[:300]
                            break
            else:
                res = client.post(url, headers=headers, json=payload)
                status = res.status_code
                message = (res.text or "").strip()[:300]
    except Exception as exc:  # noqa: BLE001
        message = f"{type(exc).__name__}: {exc}"

    elapsed = int((time.time() - started) * 1000)
    # 尽量抽出上游最有信息量的一句
    detail = message
    try:
        data = json.loads(message)
        detail = (data.get("error", {}).get("message")
                  or data.get("message") or message) if isinstance(data, dict) else message
        if isinstance(data, dict) and data.get("error") and isinstance(data["error"], dict):
            detail = data["error"].get("message") or message
    except Exception:
        pass
    hint = upstream_error_hint(up, status, str(detail))
    if not hint and status == 0:
        hint = upstream_error_hint(up, 0, str(detail))
    return {
        "ok": bool(status and 200 <= status < 400),
        "status": status,
        "elapsed_ms": elapsed,
        "message": str(detail)[:400],
        "hint": hint,
        "model": model,
    }


def probe_all_upstreams(bridge, timeout: float = 6.0) -> list[dict]:
    """并行探测全部上游（每条独立超时，总耗时约等于最慢那条）。"""
    import concurrent.futures as _futures

    results: dict[str, dict] = {}
    with _futures.ThreadPoolExecutor(max_workers=max(1, len(bridge.upstreams))) as pool:
        futures = {
            pool.submit(_probe_one, bridge, up, timeout): up["name"]
            for up in bridge.upstreams
        }
        for future in _futures.as_completed(futures):
            name = futures[future]
            try:
                results[name] = future.result()
            except Exception as exc:  # noqa: BLE001
                results[name] = {"ok": False, "status": 0, "elapsed_ms": 0,
                                 "message": f"{type(exc).__name__}: {exc}",
                                 "hint": "探测线程异常", "model": ""}
    ordered = []
    for up in bridge.upstreams:
        item = {"name": up["name"], "protocol": up["protocol"],
                "baseUrl": up["baseUrl"], "keySource": up.get("apiKeySource"),
                "default": up.get("default", False)}
        item.update(results.get(up["name"], {"ok": False, "status": 0,
                                             "elapsed_ms": 0,
                                             "message": "未探测", "hint": ""}))
        ordered.append(item)
    return ordered


# ── 导出 / 模型清单 / 单价表（能力补齐 v2.2） ────────────────────────────
EXPORT_LIMIT = 100_000  # 单次导出行数上限：超限截断并在响应头标注

EXPORT_COLUMNS = (
    "created_at", "upstream", "protocol", "requested_model", "upstream_model",
    "prompt_tokens", "completion_tokens", "cache_read_tokens", "cache_write_tokens",
    "total_tokens", "cost", "duration_ms", "ttft_ms", "status", "stream",
    "truncated", "paused", "session", "error",
)


def export_payload(bridge, params: dict) -> tuple[bytes, str, str, int, bool]:
    """构造导出内容，返回 (body, content_type, filename, row_count, truncated)。

    安全：只导出统计字段与模型名，**不含请求体、响应体、任何密钥**。
    CSV 用 ``utf-8-sig``（Excel 直接双击可正确识别中文）。
    """
    import csv
    import io

    rows = bridge.db.list_usage(
        limit=EXPORT_LIMIT + 1, offset=0,
        since_ts=_since_ts(params),
        model=params.get("model") or None,
        upstream=params.get("upstream") or None,
        session=params.get("session") or None,
        ascending=True,
    )
    truncated = len(rows) > EXPORT_LIMIT
    if truncated:
        rows = rows[:EXPORT_LIMIT]

    stamp = time.strftime("%Y%m%d-%H%M%S")
    fmt = (params.get("format") or "csv").lower()
    if fmt == "json":
        payload = {
            "exported_at": stamp,
            "range": params.get("range") or "24h",
            "filters": {k: params.get(k) for k in ("upstream", "model", "session")
                        if params.get(k)},
            "count": len(rows),
            "truncated": truncated,
            "rows": rows,
        }
        return (json.dumps(payload, ensure_ascii=False, indent=2).encode("utf-8"),
                "application/json; charset=utf-8",
                f"modelgate-usage-{stamp}.json", len(rows), truncated)

    buffer = io.StringIO()
    writer = csv.writer(buffer, lineterminator="\r\n")
    writer.writerow(EXPORT_COLUMNS)
    for row in rows:
        writer.writerow([
            "" if row.get(col) is None else row.get(col) for col in EXPORT_COLUMNS
        ])
    return (buffer.getvalue().encode("utf-8-sig"),
            "text/csv; charset=utf-8",
            f"modelgate-usage-{stamp}.csv", len(rows), truncated)


def model_catalog(bridge) -> list[dict]:
    """跨上游模型清单（一行 = 一个模型 ID）。

    数据全部来自当前运行态：配置白名单 + commandcode 目录 + 已拉取的模型缓存，
    **不发起任何网络请求**（清单页刷新是纯本地操作，不该阻塞或触发上游限流）。
    """
    from .pricing import resolve_rate

    config = bridge.config
    pricing = config.get("pricing")

    aliases_by_model: dict[str, list[str]] = {}
    for alias, target in (config.get("aliases") or {}).items():
        aliases_by_model.setdefault(str(target), []).append(str(alias))

    fetched_owner: dict[str, str] = {}
    try:
        cache = bridge.refresh_models_cache()
        fetched_owner = {str(k): str(v) for k, v in (cache.get("owner") or {}).items()}
    except Exception:
        fetched_owner = {}

    entries: dict[str, dict] = {}

    def item_for(model_id: str) -> dict:
        return entries.setdefault(model_id, {
            "model": model_id,
            "upstreams": [],
            "protocols": [],
            "sources": [],
            "aliases": list(aliases_by_model.get(model_id, [])),
            "minPlan": "",
            "default": False,
            "costHint": None,
            "costRule": "",
        })

    for up in bridge.upstreams:
        catalog_ids = {
            str((entry or {}).get("id") or ""): int((entry or {}).get("minPlanRank") or 0)
            for entry in (up.get("catalogEntries") or [])
        }
        for model_id in (up.get("models") or []):
            model_id = str(model_id)
            if not model_id:
                continue
            item = item_for(model_id)
            if up["name"] not in item["upstreams"]:
                item["upstreams"].append(up["name"])
            if up["protocol"] not in item["protocols"]:
                item["protocols"].append(up["protocol"])
            rank = catalog_ids.get(model_id, 0)
            if rank:
                source = "catalog"
                label = {1: "go", 2: "goat", 3: "pro", 4: "max"}.get(rank, "")
                if label and not item["minPlan"]:
                    item["minPlan"] = label
            else:
                source = "explicit"
            if source not in item["sources"]:
                item["sources"].append(source)

    for model_id, owner in fetched_owner.items():
        if not model_id:
            continue
        item = item_for(model_id)
        if "fetched" not in item["sources"]:
            item["sources"].append("fetched")
        if owner and owner not in item["upstreams"]:
            item["upstreams"].append(owner)

    default_model = str(config.get("defaultModel") or "")
    for model_id, item in entries.items():
        item["default"] = bool(default_model) and model_id == default_model
        rate, rule = resolve_rate(pricing, item["upstreams"][0] if item["upstreams"] else "",
                                  model_id)
        if any(rate.values()):
            item["costHint"] = rate
            item["costRule"] = rule

    return sorted(entries.values(), key=lambda r: (not r["default"], r["model"]))


def handle_get(handler, raw_path: str) -> None:
    """分发 GET /api/*。"""
    path, params = _query(raw_path)
    bridge = handler.bridge

    if path == "/api/info":
        handler.send_json(200, {
            "app": "Model-Gate",
            "version": __version__,
            "port": bridge.config.get("port"),
            "host": bridge.config.get("host"),
            "upstream_count": len(bridge.upstreams),
            "db_path": str(bridge.db.path),
            "thinking": bridge.thinking_cache.stats(),
            "server_time": int(time.time()),
        })
        return

    if path == "/api/config":
        # 密钥只暴露来源与是否已配置，不回传明文
        payload = json.loads(json.dumps(bridge.config, ensure_ascii=False))
        for name, up in ((u["name"], u) for u in bridge.upstreams):
            target = (payload.get("upstreams") or {}).get(name)
            if not isinstance(target, dict):
                continue
            target.pop("apiKey", None)
            target["_keyConfigured"] = bool(up.get("apiKey"))
            target["_keySource"] = up.get("apiKeySource")
        handler.send_json(200, {"config": payload, "config_path": str(bridge.config_path or "")})
        return

    if path == "/api/usage/totals":
        payload = {"totals": bridge.db.totals(since_ts=_since_ts(params))}
        if params.get("compare"):
            # 与「上一等长区间」对比：前端据此展示环比箭头
            prev_since, prev_until = _previous_window(params)
            if prev_since is not None:
                payload["previous"] = bridge.db.totals(
                    since_ts=prev_since, until_ts=prev_until)
        handler.send_json(200, payload)
        return

    if path == "/api/usage/summary":
        group = params.get("group_by") or "day"
        try:
            rows = bridge.db.summary(since_ts=_since_ts(params), group_by=group)
        except ValueError as exc:
            handler.send_json(400, {"error": str(exc)})
            return
        handler.send_json(200, {"group_by": group, "rows": rows})
        return

    if path == "/api/usage/list":
        try:
            limit = max(1, min(500, int(params.get("limit") or 100)))
            offset = max(0, int(params.get("offset") or 0))
        except ValueError:
            limit, offset = 100, 0
        row_filter = {
            "since_ts": _since_ts(params),
            "model": params.get("model") or None,
            "upstream": params.get("upstream") or None,
            "session": params.get("session") or None,
            # 快捷筛选（error/slow/truncated/paused/cache/ttft）+ 排序白名单均在 DB 层把关
            "only": params.get("only") or None,
        }
        rows = bridge.db.list_usage(
            limit=limit, offset=offset,
            sort=params.get("sort") or "ts",
            order=params.get("order") or "desc",
            **row_filter)
        handler.send_json(200, {
            "rows": rows,
            "total": bridge.db.count_usage(**row_filter),
            "limit": limit,
            "offset": offset,
            "sort": params.get("sort") or "ts",
            "order": params.get("order") or "desc",
            "only": params.get("only") or "",
        })
        return

    if path == "/api/usage/models":
        # 按模型聚合的用量表（请求/token/缓存/费用/成功率/均耗/P95/TTFT）
        try:
            limit = max(1, min(500, int(params.get("limit") or 100)))
        except ValueError:
            limit = 100
        rows = bridge.db.model_usage(since_ts=_since_ts(params), limit=limit)
        from .pricing import currency_of, is_configured

        table = bridge.config.get("pricing")
        handler.send_json(200, {
            "rows": rows,
            "range": params.get("range") or "24h",
            "currency": currency_of(table),
            "pricing_configured": is_configured(table),
        })
        return

    if path == "/api/usage/export":
        # 明细导出（CSV/JSON）。只含统计字段与模型名，不含请求体与任何密钥。
        try:
            body, content_type, filename, count, truncated = export_payload(bridge, params)
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"导出失败: {exc}", "type": "server_error",
                "code": "export_failed"}})
            return
        handler._send(200, body, content_type, {
            "content-disposition": f'attachment; filename="{filename}"',
            "x-row-count": str(count),
            "x-truncated": "1" if truncated else "0",
        })
        if bridge.access_log:
            log(f"已导出用量明细 {count} 行（{filename}）"
                + ("（超出上限已截断）" if truncated else ""))
        return

    if path == "/api/pricing":
        from .pricing import currency_of, is_configured, normalize_pricing

        table = normalize_pricing(bridge.config.get("pricing"))
        handler.send_json(200, {
            "pricing": table,
            "currency": currency_of(table),
            "configured": is_configured(table),
            "retention_days": int(bridge.config.get("retentionDays") or 0),
        })
        return

    if path == "/api/models/catalog":
        from .pricing import currency_of, is_configured

        rows = model_catalog(bridge)
        query = (params.get("q") or "").strip().lower()
        upstream_filter = (params.get("upstream") or "").strip()
        if query:
            rows = [
                row for row in rows
                if query in row["model"].lower()
                or any(query in alias.lower() for alias in row["aliases"])
                or any(query in name.lower() for name in row["upstreams"])
            ]
        if upstream_filter:
            rows = [row for row in rows if upstream_filter in row["upstreams"]]

        # 单价状态 / 来源筛选：与用量页的「只看异常」同构，都是服务端先筛再分页，
        # 保证 total 与当前列表说的一致（前端假筛选在分页下必然矛盾）。
        price_filter = (params.get("price") or "").strip()
        if price_filter == "priced":
            rows = [row for row in rows if row.get("costHint")]
        elif price_filter == "missing":
            rows = [row for row in rows if not row.get("costHint")]
        elif price_filter:
            price_filter = ""       # 未登记取值一律忽略（与用量页 only 同口径）
        source_filter = (params.get("source") or "").strip()
        if source_filter in ("explicit", "fetched", "catalog"):
            rows = [row for row in rows if source_filter in (row.get("sources") or [])]
        elif source_filter:
            source_filter = ""

        # 排序同样是服务端职责：先在完整结果集上排，再切片交给分页
        sort_key = (params.get("sort") or "model").strip()
        order = (params.get("order") or "asc").strip().lower()

        def _sort_key(row: dict):
            if sort_key == "upstream":
                return (row["upstreams"][0] if row["upstreams"] else "")
            if sort_key == "price":
                hint = row.get("costHint") or {}
                return float(hint.get("input") or 0) + float(hint.get("output") or 0)
            if sort_key == "source":
                return ",".join(row.get("sources") or [])
            if sort_key == "plan":
                return row.get("minPlan") or ""
            return row["model"].lower()

        if sort_key == "model":
            # 默认模型始终置顶，其余按名称；显式反序时不强留置顶
            rows = sorted(rows, key=lambda r: (not r["default"], r["model"].lower()),
                          reverse=(order == "desc"))
        else:
            rows = sorted(rows, key=_sort_key, reverse=(order == "desc"))

        try:
            page = max(1, int(params.get("page") or 1))
            page_size = max(1, min(500, int(params.get("page_size") or 50)))
        except ValueError:
            page, page_size = 1, 50
        start = (page - 1) * page_size
        pricing_table = bridge.config.get("pricing")
        handler.send_json(200, {
            "rows": rows[start:start + page_size],
            "total": len(rows),
            "page": page,
            "page_size": page_size,
            "sort": sort_key,
            "order": order,
            "price": price_filter,
            "source": source_filter,
            "currency": currency_of(pricing_table),
            "pricing_configured": is_configured(pricing_table),
            "upstreams": [
                {"name": u["name"], "protocol": u["protocol"],
                 "models": len(u.get("models") or [])}
                for u in bridge.upstreams
            ],
        })
        return

    if path == "/api/upstreams":
        handler.send_json(200, {"upstreams": [
            {
                "name": u["name"],
                "protocol": u["protocol"],
                "baseUrl": u["baseUrl"],
                "default": u["default"],
                "models": u["models"],
                "modelsAll": u["modelsAll"],
                "keySource": u["apiKeySource"],
                "keyConfigured": bool(u.get("apiKey")),
                "apiKeyEnv": u.get("apiKeyEnv"),
                "apiKeyFile": u.get("apiKeyFile"),
                "plan": u.get("modelPlan"),
                "planError": u.get("modelPlanError"),
            }
            for u in bridge.upstreams
        ]})
        return

    if path == "/api/upstreams/health":
        # 每条上游独立超时（默认 6s）并行探测，总耗时约等于最慢那条
        timeout = 6.0
        try:
            if params.get("timeout"):
                timeout = max(2.0, min(20.0, float(params["timeout"])))
        except ValueError:
            pass
        try:
            rows = probe_all_upstreams(bridge, timeout=timeout)
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"探测失败: {exc}", "type": "server_error",
                "code": "probe_failed"}})
            return
        handler.send_json(200, {"checked_at": int(time.time()), "rows": rows})
        return

    if path == "/api/logs":
        # 日志查看：exe 无控制台，面板里能看日志是排障的关键入口
        try:
            lines = max(1, min(2000, int(params.get("lines") or 300)))
        except ValueError:
            lines = 300
        level = (params.get("level") or "all").lower()
        log_path = Path(bridge.db.path).parent / "modelgate.log"
        if not log_path.is_file():
            handler.send_json(200, {"path": str(log_path), "lines": [],
                                    "total": 0, "size": 0,
                                    "note": "日志文件尚未生成"})
            return
        try:
            text = log_path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            handler.send_json(500, {"error": {
                "message": f"读取日志失败: {exc}", "type": "server_error",
                "code": "log_read_failed"}})
            return
        all_lines = text.splitlines()
        # 先带行号过滤，避免重复内容用 index() 回查导致行号错位
        keyed = list(enumerate(all_lines, start=1))
        keyword = (params.get("q") or "").strip()
        if keyword:
            needle = keyword.lower()
            keyed = [(no, line) for no, line in keyed if needle in line.lower()]
        if level in ("error", "warn"):
            markers = ("异常", "失败", "错误", "Traceback", "Error") if level == "error" \
                else ("注意", "警告", "WARN", "截断")
            keyed = [(no, line) for no, line in keyed
                     if any(m in line for m in markers)]
        handler.send_json(200, {
            "path": str(log_path),
            "size": log_path.stat().st_size,
            "total": len(all_lines),
            "matched": len(keyed),
            "lines": [{"no": no, "text": line} for no, line in keyed[-lines:]],
        })
        return

    if path == "/api/proxy/status":
        # 代理运行态快照：面板顶部状态条与「能否取代旧桥」的判断依据
        upstreams = bridge.upstreams
        handler.send_json(200, {
            "running": True,
            "version": __version__,
            "host": bridge.config.get("host"),
            "port": bridge.config.get("port"),
            "endpoint": f"http://{bridge.config.get('host')}:"
                        f"{bridge.config.get('port')}/v1",
            "access_log": bool(bridge.access_log),
            "thinking": bridge.thinking_cache.stats(),
            "upstream_count": len(upstreams),
            "model_count": len(model_list(bridge.config, upstreams)),
            "default_upstream": next(
                (u["name"] for u in upstreams if u["default"]),
                upstreams[0]["name"] if upstreams else ""),
            "db_path": str(bridge.db.path),
            "config_path": str(bridge.config_path or ""),
            "uptime_seconds": int(time.time() - bridge.started_at),
        })
        return

    handler.send_json(404, {"error": {"message": f"未实现的接口 {path}",
                                      "type": "invalid_request_error",
                                      "code": "not_implemented"}})


def handle_post(handler, raw_path: str) -> None:
    """分发 POST /api/*。"""
    path, _ = _query(raw_path)
    bridge = handler.bridge
    try:
        payload = json.loads(handler.read_body().decode("utf-8") or "{}")
    except Exception:
        handler.send_json(400, {"error": {"message": "请求体不是合法 JSON",
                                          "type": "invalid_request_error",
                                          "code": "invalid_json"}})
        return
    if not isinstance(payload, dict):
        payload = {}

    if path == "/api/config":
        _save_config(handler, bridge, payload)
        return

    if path == "/api/config/reload":
        _reload_config(handler, bridge)
        return

    if path == "/api/pricing":
        _save_pricing(handler, bridge, payload)
        return

    if path == "/api/pricing/recompute":
        from .pricing import is_configured, normalize_pricing

        table = normalize_pricing(
            payload.get("pricing") or bridge.config.get("pricing"))
        if not is_configured(table):
            handler.send_json(400, {"error": {
                "message": "尚未配置任何非零单价，重算没有意义",
                "type": "invalid_request_error", "code": "pricing_missing"}})
            return
        try:
            updated = bridge.db.recompute_costs(table)
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"重算失败: {exc}", "type": "server_error",
                "code": "recompute_failed"}})
            return
        if bridge.access_log:
            log(f"已按当前单价重算历史费用，更新 {updated} 条")
        handler.send_json(200, {"ok": True, "updated": updated,
                                "totals": bridge.db.totals()})
        return

    if path == "/api/usage/retention/apply":
        raw_days = payload.get("days", bridge.config.get("retentionDays"))
        try:
            days = int(raw_days or 0)
        except (TypeError, ValueError):
            days = 0
        before = bridge.db.usage_count_and_size()
        try:
            removed = bridge.db.delete_older_than(days=days)
            size_after = bridge.db.vacuum() if removed else before["bytes"]
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"清理失败: {exc}", "type": "server_error",
                "code": "retention_failed"}})
            return
        if bridge.access_log:
            log(f"保留策略：删除 {removed} 条（保留 {days} 天）")
        handler.send_json(200, {
            "ok": True, "days": days, "removed": removed,
            "before": before, "size_after": size_after,
            "after": bridge.db.usage_count_and_size(),
        })
        return

    if path == "/api/upstreams/probe":
        # 单条上游即时测试（配置页里改完 URL/密钥就能立刻验证）
        name = str(payload.get("name") or "")
        target = next((u for u in bridge.upstreams if u["name"] == name), None)
        if target is None:
            handler.send_json(404, {"error": {
                "message": f"没有名为 {name!r} 的上游",
                "type": "invalid_request_error", "code": "unknown_upstream"}})
            return
        try:
            timeout = max(2.0, min(20.0, float(payload.get("timeout") or 8)))
        except (TypeError, ValueError):
            timeout = 8.0
        try:
            result = _probe_one(bridge, target, timeout)
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"探测失败: {exc}", "type": "server_error",
                "code": "probe_failed"}})
            return
        result.update({"name": target["name"], "protocol": target["protocol"],
                       "baseUrl": target["baseUrl"]})
        handler.send_json(200, {"row": result})
        return

    if path == "/api/upstreams/secret":
        _save_upstream_secret(handler, bridge, payload)
        return

    if path == "/api/usage/clear":
        # 清空用量明细（只清明细，不动配置与 settings）
        rng = str(payload.get("range") or "")
        since = None
        if rng in TIME_RANGES:
            since = _since_ts({"range": rng})
        try:
            removed = bridge.db.clear_usage(since_ts=since)
        except Exception as exc:  # noqa: BLE001
            handler.send_json(500, {"error": {
                "message": f"清空失败: {exc}", "type": "server_error",
                "code": "clear_failed"}})
            return
        if bridge.access_log:
            log(f"已清空用量明细 {removed} 条"
                + (f"（区间 {rng}）" if since else "（全部）"))
        handler.send_json(200, {"ok": True, "removed": removed, "range": rng or "all"})
        return

    if path == "/api/logs/clear":
        log_path = Path(bridge.db.path).parent / "modelgate.log"
        try:
            if log_path.is_file():
                log_path.write_text("", encoding="utf-8")
            handler.send_json(200, {"ok": True, "path": str(log_path)})
        except OSError as exc:
            handler.send_json(500, {"error": {
                "message": f"清空日志失败: {exc}", "type": "server_error",
                "code": "log_clear_failed"}})
        return

    handler.send_json(404, {"error": {"message": f"未实现的接口 {path}",
                                      "type": "invalid_request_error",
                                      "code": "not_implemented"}})


def _save_upstream_secret(handler, bridge, payload: dict) -> None:
    """把某个上游的密钥写进 env 文件并立即生效（不回显密钥）。

    为什么需要它：**密钥过期是必然发生的运维事件**（上游会明确返回
    ``api_key_expired``）。如果恢复路径是「手动找文件 → 改一行 → 重启程序」，
    用户很容易误判成「工具坏了」。这里给出两步路径：
    粘贴新 key → 立即生效（写入专用密钥文件，不碰 bridge.config.json）。
    """
    from .config import load_env_file, write_env_var

    name = str(payload.get("name") or "")
    secret = str(payload.get("apiKey") or "").strip()
    target = next((u for u in bridge.upstreams if u["name"] == name), None)
    if target is None:
        handler.send_json(404, {"error": {
            "message": f"没有名为 {name!r} 的上游",
            "type": "invalid_request_error", "code": "unknown_upstream"}})
        return
    env_name = target.get("apiKeyEnv")
    if not env_name:
        handler.send_json(400, {"error": {
            "message": "该上游未使用 apiKeyEnv（可改用 apiKeyFile，或在配置页直接填 apiKey）",
            "type": "invalid_request_error", "code": "no_env_ref"}})
        return
    if not secret:
        handler.send_json(400, {"error": {
            "message": "密钥不能为空",
            "type": "invalid_request_error", "code": "empty_secret"}})
        return

    try:
        path = write_env_var(bridge.config, env_name, secret)
        load_env_file(bridge.config, refresh=True)
        _apply_config(bridge, dict(bridge.config))
    except Exception as exc:  # noqa: BLE001
        handler.send_json(500, {"error": {
            "message": f"写入密钥失败: {exc}", "type": "server_error",
            "code": "secret_save_failed"}})
        return

    refreshed = next((u for u in bridge.upstreams if u["name"] == name), None)
    applied = bool(refreshed and refreshed.get("apiKey"))
    if bridge.access_log:
        log(f"上游 {name} 的密钥已写入 {path.name}（{env_name}）并热更新")
    handler.send_json(200, {
        "ok": True,
        "envFile": str(path),
        "envName": env_name,
        "applied": applied,
        "keyConfigured": applied,
        "note": "已写入密钥文件并热更新；密钥值不会回显",
    })


def _save_config(handler, bridge, payload: dict) -> None:
    """保存配置：合并 → 落盘 → 双写 → 热更新上游。"""
    incoming = payload.get("config") if isinstance(payload.get("config"), dict) else payload
    if not isinstance(incoming, dict):
        handler.send_json(400, {"error": {"message": "配置必须是 JSON 对象",
                                          "type": "invalid_request_error",
                                          "code": "invalid_config"}})
        return
    allowed = {
        "port", "host", "defaultModel", "models", "maxTokens", "timeoutSeconds",
        "accessLog", "modelsCacheSeconds", "aliases", "thinking", "tools",
        "envFile", "upstreams", "upstream", "retentionDays", "pricing",
    }
    updated = dict(bridge.config)
    for key in allowed:
        if key in incoming:
            updated[key] = incoming[key]

    # ── 明文密钥硬约束 ──────────────────────────────────────────────
    # 非空明文 apiKey 只进内存，绝不写进 bridge.config.json：
    # 该文件是唯一对外契约（会被分享/提交），明文一旦落盘便随文件扩散。
    # 空字符串（表示「不发鉴权头」）与 apiKeyEnv / apiKeyFile 引用不受影响。
    # 键统一用「归一化后的上游名」：条目里可能没有 name 字段，靠上游自身键名最稳。
    literal_secrets: dict[str, str] = {}

    def _strip(ups: dict) -> None:
        if not isinstance(ups, dict):
            return
        for name, item in ups.items():
            if isinstance(item, dict) and item.get("apiKey"):
                literal_secrets[name] = item["apiKey"]
                item.pop("apiKey", None)
            elif not isinstance(item, dict) and isinstance(name, str):
                pass

    for container in ("upstreams", "upstream"):
        obj = updated.get(container)
        if isinstance(obj, dict):
            _strip(obj)

    # 客户端未回传密钥时保留既有引用（apiKeyEnv / apiKeyFile 原样保留即可）
    updated_upstreams = updated.get("upstreams") if isinstance(updated.get("upstreams"), dict) else {}
    for name, new_up in updated_upstreams.items():
        if isinstance(new_up, dict) and not (new_up.get("apiKeyEnv") or new_up.get("apiKeyFile")):
            old = next((u for u in bridge.upstreams if u["name"] == name), None)
            if old:
                if old.get("apiKeyEnv"):
                    new_up["apiKeyEnv"] = old["apiKeyEnv"]
                elif old.get("apiKeyFile"):
                    new_up["apiKeyFile"] = old["apiKeyFile"]

    # 先校验、后持久化：结构不合法直接 400，绝不把坏配置写进文件
    problem = validate_config_shape(updated)
    if problem:
        handler.send_json(400, {
            "error": {"message": problem, "type": "invalid_request_error",
                      "code": "invalid_config"},
        })
        return

    try:
        if bridge.config_path:
            save_config(bridge.config_path, updated)
        bridge.db.save_config_override(updated)
    except Exception as exc:
        handler.send_json(500, {"error": {"message": f"保存配置失败: {exc}",
                                          "type": "server_error",
                                          "code": "save_failed"}})
        return

    # 保存前的运行态明文快照（必须在 _apply_config 之前取，否则拿到的是重建后的 any-key）
    prior = {u["name"]: u.get("apiKey") for u in bridge.upstreams
             if u.get("apiKey") and u.get("apiKey") != "any-key"}

    try:
        _apply_config(bridge, updated)
    except Exception as exc:  # 兜底：校验已过，仍失败只回 500 且不半初始化
        handler.send_json(500, {"error": {
            "message": f"应用配置失败: {exc}", "type": "server_error",
            "code": "apply_failed"}})
        return
    # 明文密钥只活在内存：文件与 settings 均无明文，运行态补回，避免保存配置后上游失效。
    # 两个来源：本次 payload 里的明文（_strip 摘出）+ 保存前运行态快照 prior。
    # 只用本次 payload 会漏掉「前端不回传明文」的主流路径，导致既有上游被降级成 any-key。
    for up in bridge.upstreams:
        secret = literal_secrets.get(up["name"]) or prior.get(up["name"])
        if secret:
            up["apiKey"] = secret
            up["apiKeySource"] = "literal"
    if bridge.access_log:
        log(f"配置已保存：{len(bridge.upstreams)} 个上游，端口 {bridge.config.get('port')}")
    note = "端口变更需重启代理后生效"
    restored = set(literal_secrets) | set(prior)
    if restored:
        note += f"；{len(restored)} 个上游的明文密钥仅保留在内存，未写入配置文件"
    handler.send_json(200, {"ok": True, "note": note})


def _write_pricing_to_file(path: Path, table: dict) -> None:
    """把单价表写进配置文件，**只改 pricing 一个键**。

    为什么不整文件重写：配置文件是用户的对外契约，里面可能有用户手写的
    apiKey 等字段。整文件覆盖会把「运行态视图」当成事实源写回去，
    一旦运行态与文件有差异（例如明文密钥只在内存），就会静默丢配置 ——
    这正是历史事故（保存配置后上游 401）的同一类风险。
    """
    try:
        current = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(current, dict):
            current = {}
    except FileNotFoundError:
        current = {}
    current["pricing"] = table
    path.write_text(json.dumps(current, ensure_ascii=False, indent=2), encoding="utf-8")


def _save_pricing(handler, bridge, payload: dict) -> None:
    """保存单价表：更新运行态 → 写文件（只改 pricing）+ 写 settings → 可选重算。

    settings 里存的是「运行态事实源」整体（与 /api/config 一致），
    这样启动时能恢复最近一次写入；配置文件侧则做最小改动写入。
    """
    from .pricing import currency_of, is_configured, normalize_pricing

    raw = payload.get("pricing")
    if raw is None:
        raw = {k: v for k, v in payload.items() if k != "recompute"}
    table = normalize_pricing(raw)

    updated = dict(bridge.config)
    updated["pricing"] = table

    try:
        if bridge.config_path:
            _write_pricing_to_file(Path(bridge.config_path), table)
        bridge.db.save_config_override(updated)
    except Exception as exc:  # noqa: BLE001
        handler.send_json(500, {"error": {"message": f"保存单价表失败: {exc}",
                                          "type": "server_error",
                                          "code": "pricing_save_failed"}})
        return

    bridge.config = updated
    ack: dict = {
        "ok": True,
        "pricing": table,
        "currency": currency_of(table),
        "configured": is_configured(table),
    }
    if payload.get("recompute"):
        try:
            ack["recomputed"] = bridge.db.recompute_costs(table)
        except Exception as exc:  # noqa: BLE001
            ack["recomputed"] = 0
            ack["recompute_error"] = str(exc)
    if bridge.access_log:
        log(f"单价表已更新：{len(table['rules'])} 条规则，币种 {table['currency']}"
            + (f"，重算 {ack.get('recomputed', 0)} 条历史" if payload.get("recompute") else ""))
    handler.send_json(200, ack)


def _reload_config(handler, bridge) -> None:
    """重新载入配置与 env 文件。

    ``refresh=True`` 让 env 文件里更新的密钥立即生效 —— 这是「上游 key 过期，
    换完 key 不想重启程序」的主路径：改 ~/.jcode/.env → 点「重新载入」即可。
    """
    try:
        from .config import load_config, load_env_file

        if bridge.config_path and Path(bridge.config_path).is_file():
            updated = load_config(bridge.config_path)
        else:
            updated = dict(bridge.config)
        load_env_file(updated, refresh=True)
        _apply_config(bridge, updated)
        try:  # 快照跟随文件：否则下次启动又会被旧快照盖回去
            bridge.db.save_config_override(dict(bridge.config))
        except Exception:
            pass
    except Exception as exc:
        handler.send_json(500, {"error": {"message": f"重载配置失败: {exc}",
                                          "type": "server_error",
                                          "code": "reload_failed"}})
        return
    sources = {u["name"]: u.get("apiKeySource") for u in bridge.upstreams}
    handler.send_json(200, {"ok": True, "upstreams": len(bridge.upstreams),
                            "keySources": sources})


def _apply_config(bridge, updated: dict) -> None:
    """把新配置应用到运行态（上游热更新；端口变更需重启）。"""
    bridge.config = updated
    bridge.upstreams = upstreams_of(updated)
    bridge.timeout = float(updated.get("timeoutSeconds") or 600)
    bridge.access_log = updated.get("accessLog") is not False
    bridge.thinking_cache.enabled = updated.get("thinking", {}).get("passthrough") is not False
    bridge.thinking_cache.max_size = int(
        updated.get("thinking", {}).get("cacheSize") or 500)
    bridge.models_cache_ms = int(updated.get("modelsCacheSeconds") or 300) * 1000
    # 复用同一 client（不重建，避免影响在途请求），只更新超时
    import httpx as _httpx

    try:
        bridge.client.timeout = _httpx.Timeout(bridge.timeout, connect=10.0)
    except Exception:
        pass
    if bridge.adapters.get("commandcode") is not None:
        bridge.start_background_tasks()
