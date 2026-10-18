# -*- coding: utf-8 -*-
"""v2.5 隔离验证：用量界面深化（排序 / 快捷筛选 / 环比 / 分页 / 展开详情）。

安全约定（务必保持）：
* 只用临时目录里的 config/db，**绝不指向 dist/**；在跑的 8900 实例不受影响；
* 端口由系统分配空闲端口后再传给被测进程；
* 断言失败即退出非零，最后打印 ``V25_VERIFY OK n/n``。
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKS: list[tuple[str, bool, str]] = []

# 版本号唯一来源：modelgate/__init__.py（面板资产版本戳用的是同一个值）
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
from modelgate import __version__ as _version  # noqa: E402


def check(name: str, ok: bool, detail: str = "") -> None:
    CHECKS.append((name, bool(ok), detail))
    print(f"  [{'OK  ' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))


def http(url: str, payload=None, timeout: float = 8.0):
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers,
                                 method="POST" if data else "GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read().decode("utf-8", "replace")
        ctype = resp.headers.get("content-type", "")
        return resp.status, (json.loads(body) if "json" in ctype else body)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def seed(db_path: Path) -> None:
    """预埋可判定的用量样本：慢/错/缓存/截断各一，便于锁定排序与筛选语义。"""
    sys.path.insert(0, str(ROOT))
    from modelgate.db import Database

    db = Database(db_path)
    rows = [
        # duration, ttft, status, truncated, cache_read, model
        (1200, 240, 200, False, 0, "m-fast"),
        (45000, 0, 200, False, 0, "m-slow"),
        (800, 120, 500, False, 0, "m-err"),
        (30000, 300, 200, True, 5000, "m-slow"),
    ]
    for dur, ttft, status, trunc, cache, model in rows:
        db.record_usage(
            upstream="cch", protocol="anthropic", requested_model=model,
            upstream_model=model, prompt_tokens=100, completion_tokens=20,
            cache_read_tokens=cache, duration_ms=dur, ttft_ms=ttft,
            status=status, stream=True, truncated=trunc,
            error="boom" if status >= 400 else None, session="s-1")
    db.close()


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="mg-v25-"))
    cfg_path = tmp / "bridge.config.json"
    db_path = tmp / "modelgate.db"
    port = free_port()

    cfg_path.write_text(json.dumps({
        "port": port, "host": "127.0.0.1", "defaultModel": "m-fast",
        "models": ["m-fast", "m-slow"], "maxTokens": 8192, "timeoutSeconds": 30,
        "accessLog": False, "modelsCacheSeconds": 300,
        "thinking": {"passthrough": True, "cacheSize": 100},
        "tools": {"downgradeForcedChoice": True},
        "upstreams": {"cch": {"protocol": "anthropic",
                              "baseUrl": "http://127.0.0.1:15721", "apiKey": "k",
                              "models": ["m-fast", "m-slow"], "default": True,
                              "fetchModels": False}},
    }, ensure_ascii=False), encoding="utf-8")
    seed(db_path)

    env = dict(os.environ)
    env["PYTHONIOENCODING"] = "utf-8"
    proc = subprocess.Popen(
        [sys.executable, "-m", "modelgate", "--no-gui", "--port", str(port),
         "--config", str(cfg_path), "--db", str(db_path)],
        cwd=str(ROOT), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace")
    base = f"http://127.0.0.1:{port}"
    try:
        started = False
        deadline = time.time() + 40
        while time.time() < deadline:
            if proc.poll() is not None:
                break
            try:
                http(base + "/health", timeout=2.0)
                started = True
                break
            except Exception:
                time.sleep(0.4)
        if not started:
            tail = ""
            try:
                tail = (proc.stdout.read() or "")[-600:] if proc.stdout else ""
            except Exception:
                pass
            check("代理进程可启动并响应 /health", False, tail or "未在 40s 内就绪")
            return finish()

        check("代理进程可启动并响应 /health", True, f"port={port}")

        # 1) 排序：按耗时降序 / 升序（服务端排序，不是前端假排序）
        _, desc = http(base + "/api/usage/list?range=24h&sort=duration_ms&order=desc")
        durs = [r["duration_ms"] for r in desc["rows"]]
        check("明细按耗时可降序排序", durs == sorted(durs, reverse=True), str(durs))
        _, asc = http(base + "/api/usage/list?range=24h&sort=duration_ms&order=asc")
        durs_asc = [r["duration_ms"] for r in asc["rows"]]
        check("明细按耗时可升序排序", durs_asc == sorted(durs_asc), str(durs_asc))

        # 2) 排序白名单：非法列名回落时间序，不报错
        status, weird = http(base + "/api/usage/list?range=24h&sort=ts;DROP+TABLE+x")
        check("非法排序键被白名单拦下且不报错",
              status == 200 and len(weird.get("rows", [])) == 4, f"status={status}")

        # 3) 快捷筛选：only=error / slow / cache / truncated，且 total 与行数一致
        for key, predicate in (
            ("error", lambda r: r["status"] >= 400),
            ("slow", lambda r: r["duration_ms"] >= 10000),
            ("cache", lambda r: r["cache_read_tokens"] > 0),
            ("truncated", lambda r: bool(r["truncated"])),
        ):
            _, data = http(base + f"/api/usage/list?range=24h&only={key}&limit=50")
            rows = data.get("rows", [])
            ok = bool(rows) and all(predicate(r) for r in rows) and data.get("total") == len(rows)
            check(f"快捷筛选 only={key} 命中且 total 一致", ok,
                  f"rows={len(rows)} total={data.get('total')}")

        _, bogus = http(base + "/api/usage/list?range=24h&only=no_such_filter")
        check("未知筛选值被忽略（等价不过滤）",
              len(bogus.get("rows", [])) == 4, str(len(bogus.get("rows", []))))

        # 4) 环比：compare=1 必须带回上一区间，且区间长度与当前一致
        _, cmp_data = http(base + "/api/usage/totals?range=24h&compare=1")
        prev = cmp_data.get("previous")
        check("总计支持与上一区间对比（compare=1）",
              isinstance(prev, dict) and "requests" in prev,
              f"previous={type(prev).__name__}")
        _, plain = http(base + "/api/usage/totals?range=24h")
        check("未请求对比时不返回 previous", "previous" not in plain)

        # 5) 分页：limit=2 两页不重复，total 稳定
        _, p1 = http(base + "/api/usage/list?range=24h&limit=2&offset=0&sort=ts&order=desc")
        _, p2 = http(base + "/api/usage/list?range=24h&limit=2&offset=2&sort=ts&order=desc")
        ids1 = {r["id"] for r in p1["rows"]}
        ids2 = {r["id"] for r in p2["rows"]}
        check("分页两页不重叠且 total 一致",
              len(ids1) == 2 and not (ids1 & ids2) and p1["total"] == p2["total"] == 4,
              f"total={p1['total']}/{p2['total']}")

        # 6) 界面资产：新控件与接线必须真的在产物里
        status, ui = http(base + "/ui/")
        ui = str(ui)
        need_html = ('id="only-chips"', 'data-sort="duration_ms"', 'data-sort="ttft_ms"',
                     'id="page-size"', 'id="page-jump"', 'id="first-page"',
                     'id="last-page"', 'id="usage-auto"', 'id="usage-fresh"',
                     'id="model-only-chips"', 'data-model-sort="price"',
                     'id="model-jump"', 'id="model-page-size"', 'id="model-first"',
                     'id="model-last"', 'id="model-table-hint"',
                     'id="upstream-table-hint"', 'data-up-sort="models"',
                     'id="log-lines"', 'id="log-auto"', 'id="log-copy"',
                     'id="log-bottom"', 'id="log-fresh"',
                     'id="config-dirty-bar"', 'id="config-dirty-text"',
                     'id="config-discard"', 'id="config-issues"', 'id="save-config"')
        missing = [t for t in need_html if t not in ui]
        check("用量面板含各深化控件", status == 200 and not missing,
              f"缺失 {missing}" if missing else "全部就位")
        _, js = http(base + "/ui/app.js")
        js = str(js)
        need_js = ("renderSortMarks", "usageDetailRow", "setAutoRefresh", "deltaTag",
                   "markFresh", "renderTableHint")
        missing_js = [t for t in need_js if t not in js]
        check("app.js 已接线排序/详情/自动刷新/环比", not missing_js,
              f"缺失 {missing_js}" if missing_js else "全部就位")
        _, css = http(base + "/ui/style.css")
        css = str(css)
        missing_css = [c for c in (".kpi-group", ".chip", ".delta", ".detail-grid",
                                   "tr.usage-row", "details.fold") if c not in css]
        check("style.css 含分组/胶囊/详情/折叠样式", not missing_css,
              f"缺失 {missing_css}" if missing_css else "全部就位")

        # 6b) 模型页深化：单价/来源筛选、服务端排序、参数回显、前端接线
        _, cat_all = http(base + "/api/models/catalog?page_size=200")
        _, cat_missing = http(base + "/api/models/catalog?price=missing&page_size=200")
        check("模型清单支持 price=missing 筛选",
              isinstance(cat_missing.get("rows"), list)
              and all(not r.get("costHint") for r in cat_missing["rows"]),
              f"total={cat_missing.get('total')}")
        _, cat_bogus = http(base + "/api/models/catalog?source=nonsense&page_size=200")
        check("未登记的来源取值被忽略（等价不过滤）",
              cat_bogus.get("total") == cat_all.get("total"),
              f"{cat_bogus.get('total')} vs {cat_all.get('total')}")
        _, cat_sorted = http(base + "/api/models/catalog?sort=price&order=desc&page_size=200")
        check("模型清单排序参数被回显（前端据此提示当前排序）",
              cat_sorted.get("sort") == "price" and cat_sorted.get("order") == "desc")
        _, js2 = http(base + "/ui/app.js")
        missing_model_js = [t for t in ("renderModelSortMarks", "renderModelTableHint",
                                        "CATALOG_SORT_LABELS") if t not in str(js2)]
        check("模型页排序/筛选已接线", not missing_model_js,
              f"缺失 {missing_model_js}" if missing_model_js else "全部就位")

        # 6c) 日志页与概览页深化：行数/自动刷新/复制/跳末尾、关键词高亮、上游表排序与展开
        _, logs = http(base + "/api/logs?lines=800&level=error")
        check("日志接口支持行数与级别参数",
              isinstance(logs.get("lines"), list) and logs.get("matched") is not None,
              f"matched={logs.get('matched')} total={logs.get('total')}")
        _, logs2 = http(base + "/api/logs?lines=200&level=all")
        check("日志接口 level=all 不过滤（matched == total）",
              logs2.get("matched") == logs2.get("total") and (logs2.get("total") or 0) > 0,
              f"{logs2.get('matched')}/{logs2.get('total')}")
        check("日志级别筛选只收窄 matched、不改 total",
              logs.get("total") == logs2.get("total")
              and (logs.get("matched") or 0) <= (logs2.get("matched") or 0),
              f"error={logs.get('matched')} all={logs2.get('matched')}")
        missing_more_js = [t for t in ("sortedUpstreams", "renderUpstreamSortMarks",
                                       "upstreamDetailRow", "highlightLog", "setLogAuto")
                           if t not in str(js2)]
        check("概览/日志页深化已接线", not missing_more_js,
              f"缺失 {missing_more_js}" if missing_more_js else "全部就位")
        _, css2 = http(base + "/ui/style.css")
        check("style.css 含日志高亮样式", ".log-viewer mark" in str(css2))

        # 面板资产禁缓存：否则改完前端用户仍看到旧界面（真实踩过的坑）
        with urllib.request.urlopen(base + "/ui/app.js", timeout=5) as resp:
            cache_control = resp.headers.get("cache-control", "")
        check("面板资产禁用浏览器缓存（no-store）",
              "no-store" in cache_control, cache_control or "缺少 cache-control")

        # 6d) 配置页深化：脏值条 + 字段级校验清单
        missing_cfg_js = [t for t in ("setConfigDirty", "validateConfig",
                                      "renderConfigIssues") if t not in str(js2)]
        check("配置页脏值/校验已接线", not missing_cfg_js,
              f"缺失 {missing_cfg_js}" if missing_cfg_js else "全部就位")
        _, css3 = http(base + "/ui/style.css")
        missing_cfg_css = [c for c in (".dirty-bar", ".issues", "button.issue",
                                       ".issue-tag") if c not in str(css3)]
        check("style.css 含配置页脏值/校验样式", not missing_cfg_css,
              f"缺失 {missing_cfg_css}" if missing_cfg_css else "全部就位")
        status, ack = http(base + "/api/config", {
            "config": {"host": "127.0.0.1", "port": port, "maxTokens": 512},
        })
        check("配置保存接口可用（隔离实例）",
              status == 200 and isinstance(ack, dict), f"status={status}")

        # 6e) 折叠 + 弹窗 + 面板资产禁缓存（「代码改了、界面没变」的根因）
        missing_fold_html = [t for t in ('data-fold="config-service"',
                                         'data-fold="config-upstreams"',
                                         'data-fold="pricing"',
                                         'data-fold="usage-retention"',
                                         'id="modal-root"') if t not in ui]
        check("界面含折叠组与弹窗容器", not missing_fold_html,
              f"缺失 {missing_fold_html}" if missing_fold_html else "全部就位")
        missing_fold_js = [t for t in ("openModal", "confirmModal", "closeModal",
                                       "initFolds", "openUpstreamEditor",
                                       "applyUpstreamEdits") if t not in str(js2)]
        check("折叠/弹窗已接线", not missing_fold_js,
              f"缺失 {missing_fold_js}" if missing_fold_js else "全部就位")
        check("原生 confirm 已清零", "!confirm(" not in str(js2))
        missing_cn = [t for t in ("function fmtCn", "function fmtTok", "万亿", "亿") if t not in str(js2)]
        check("token 数量使用中文单位（万/亿/万亿）", not missing_cn,
              f"缺失 {missing_cn}" if missing_cn else "全部就位")
        missing_svc = [t for t in ("renderServiceFacts", "updateRestartHint", "serviceUrl")
                       if t not in str(js2)]
        check("服务分区（当前值对照/端口提示/恢复默认）已接线", not missing_svc,
              f"缺失 {missing_svc}" if missing_svc else "全部就位")
        missing_chart = [t for t in ("trendMetricInfo", "trend-svg", "pie-wrap",
                                     "pie-legend", "avg-line", "pie-arc")
                         if t not in str(js2)]
        check("用量图表为 SVG（趋势柱 + 甜甜圈占比 + 均值线）", not missing_chart,
              f"缺失 {missing_chart}" if missing_chart else "全部就位")
        _, css4 = http(base + "/ui/style.css")
        missing_spec = [t for t in ("details.fold > :not(summary)", "th:first-child",
                                    "nth-child(even)", "scrollbar-gutter")
                        if t not in str(css4)]
        check("边界与列表规范层已生效（折叠留白 / 表格首尾列 / 斑马纹 / 滚动上限）",
              not missing_spec, f"缺失 {missing_spec}" if missing_spec else "全部就位")
        missing_rate = [t for t in ("function cacheHitRate", "function fmtRate",
                                    'kpiCard("缓存命中率"', "cache_hit_rate")
                        if t not in str(js2)]
        check("缓存命中率已接线（KPI / 模型表列 / 构成条 / 明细）", not missing_rate,
              f"缺失 {missing_rate}" if missing_rate else "全部就位")
        missing_cfg = [t for t in ("function renderUpstreamCfgTable", "upstreamDetailRowCfg",
                                   "cfgUpstreamExpanded") if t not in str(js2)]
        check("配置页上游为列表+详情模式", not missing_cfg and "upstream-cfg-table" in ui,
              f"缺失 {missing_cfg}" if missing_cfg else "全部就位")
        check("数值输入支持 30万/3,000,000/384K（前端解析）",
              "function parseCount" in str(js2) and "max-tokens-options" in ui)
        check("DeepSeek 官方输出上限提示在位",
              "DEEPSEEK_MAX_OUTPUT" in str(js2) and "最大输出 384K" in ui)
        status_ok, _ = http(base + "/api/config", {"config": {"maxTokens": 393216}})
        try:
            http(base + "/api/config", {"config": {"maxTokens": "30万"}})
            status_bad = 200
        except urllib.error.HTTPError as exc:
            status_bad = exc.code
        check("服务端契约：数字可写、带单位的字符串被拒（前端负责转换）",
              status_ok == 200 and status_bad == 400, f"{status_ok}/{status_bad}")

        with urllib.request.urlopen(base + "/ui/app.js", timeout=5) as resp:
            js_headers = {k.lower(): v for k, v in resp.headers.items()}
        check("静态资产禁缓存（no-store）",
              "no-store" in js_headers.get("cache-control", ""),
              js_headers.get("cache-control", "（无 Cache-Control）"))
        check("HTML 注入资源版本戳",
              f"app.js?v={_version}" in ui and f"style.css?v={_version}" in ui,
              f"version={_version}")
        status, stamped = http(base + f"/ui/app.js?v={_version}")
        check("带 ?v= 的资产请求仍可命中（查询串被剥离）",
              status == 200 and "/api/usage/summary" in str(stamped), f"status={status}")

        # 7) 代理不中断：重载配置后端口不重绑、统计仍可查
        status, ack = http(base + "/api/config/reload", {})
        _, after = http(base + "/api/usage/list?range=24h&only=slow")
        check("重载配置后代理与统计均正常",
              status == 200 and ack.get("ok") is True
              and len(after.get("rows", [])) == 2 and proc.poll() is None)
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=10)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        shutil.rmtree(tmp, ignore_errors=True)
    return finish()


def finish() -> int:
    passed = sum(1 for _, ok, _ in CHECKS if ok)
    total = len(CHECKS)
    print()
    if passed == total:
        print(f"V25_VERIFY OK {passed}/{total}")
        return 0
    print(f"V25_VERIFY FAILED {passed}/{total}")
    for name, ok, detail in CHECKS:
        if not ok:
            print(f"  - {name} {detail}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
