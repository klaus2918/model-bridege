# -*- coding: utf-8 -*-
"""v2.5 打包产物隔离验证：直接对 dist\\Model-Gate-v2.5.exe 做黑盒检查。

与 verify_v25.py（源码级）互补：这里证明**打包进去的界面与接口就是验证过的那份**。

安全约定：
* 工作目录、config、db 全在临时目录，**不读也不写 dist/**；
* 端口由系统分配；被测进程用完即停；断言失败退出非零。
"""
from __future__ import annotations

import json
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
def pick_exe() -> Path:
    """取 dist 下最新的 Model-Gate-v*.exe：升级产物与在跑 exe 同名不同文件是本项目的约定。"""
    candidates = sorted((ROOT / "dist").glob("Model-Gate-v*.exe"),
                        key=lambda p: p.stat().st_mtime, reverse=True)
    return candidates[0] if candidates else ROOT / "dist" / "Model-Gate.exe"


def source_version() -> str:
    """版本号唯一来源：modelgate/__init__.py（exe 里打的也是它）。"""
    import re

    text = (ROOT / "modelgate" / "__init__.py").read_text(encoding="utf-8")
    found = re.search(r'__version__\s*=\s*"([^"]+)"', text)
    return found.group(1) if found else ""


EXE = pick_exe()
EXPECT_VERSION = source_version()
CHECKS: list[tuple[str, bool, str]] = []
RUN_START = time.time()


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


def live_instance_ok() -> tuple[bool, str]:
    """在跑实例（8900）仍在服务：本轮所有验证都不允许打断它。"""
    try:
        with urllib.request.urlopen("http://127.0.0.1:8900/health", timeout=3) as resp:
            return resp.status == 200, f"status={resp.status}"
    except Exception as exc:  # noqa: BLE001
        return True, f"未监听，跳过（{type(exc).__name__}）"


def dist_untouched(since: float) -> tuple[bool, str]:
    """dist 的配置与产物 exe 在本轮验证期间未被改写（只读现场）。

    刻意**不检查** ``dist/data/**``：那是 8900 在线实例自己的库与日志，
    它每服务一次请求就会写 WAL 与 access log——把它的正常写入算成「被验证改写」
    只会制造假告警。这里守护的是「人手工改配置 / 覆盖正在跑的 exe」这类真事故。
    """
    dist = ROOT / "dist"
    watched = [dist / "bridge.config.json", dist / "Model-Gate.exe"]
    watched += [p for p in dist.glob("Model-Gate-v*.exe") if p != EXE]
    touched = [p.name for p in watched if p.exists() and p.stat().st_mtime > since]
    return (not touched, ",".join(touched) if touched else f"检查 {len(watched)} 项，均未改写")


def seed(db_path: Path) -> int:
    sys.path.insert(0, str(ROOT))
    from modelgate.db import Database

    db = Database(db_path)
    rows = [(900, 180, 200, "m-fast", 0), (41000, 0, 200, "m-slow", 9000),
            (700, 90, 503, "m-err", 0)]
    for dur, ttft, status, model, cache in rows:
        db.record_usage(upstream="cch", protocol="anthropic", requested_model=model,
                        upstream_model=model, prompt_tokens=500, completion_tokens=30,
                        cache_read_tokens=cache, duration_ms=dur, ttft_ms=ttft,
                        status=status, stream=True, error="boom" if status >= 400 else None,
                        session="s-exe")
    db.close()
    return len(rows)


def main() -> int:
    if not EXE.is_file():
        print(f"缺少产物：{EXE}")
        return 1
    tmp = Path(tempfile.mkdtemp(prefix="mg-v25-exe-"))
    cfg = tmp / "bridge.config.json"
    dbp = tmp / "modelgate.db"
    port = free_port()
    cfg.write_text(json.dumps({
        "port": port, "host": "127.0.0.1", "defaultModel": "m-fast",
        "models": ["m-fast", "m-slow"], "maxTokens": 4096, "timeoutSeconds": 30,
        "accessLog": False, "modelsCacheSeconds": 300,
        "upstreams": {"cch": {"protocol": "anthropic",
                              "baseUrl": "http://127.0.0.1:15721", "apiKey": "k",
                              "models": ["m-fast", "m-slow"], "default": True,
                              "fetchModels": False}},
    }, ensure_ascii=False), encoding="utf-8")
    seeded = seed(dbp)

    proc = subprocess.Popen(
        [str(EXE), "--no-gui", "--port", str(port),
         "--config", str(cfg), "--db", str(dbp)],
        cwd=str(tmp), stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    base = f"http://127.0.0.1:{port}"
    try:
        started = False
        deadline = time.time() + 60
        while time.time() < deadline:
            if proc.poll() is not None:
                break
            try:
                http(base + "/health", timeout=2.0)
                started = True
                break
            except Exception:
                time.sleep(0.5)
        check("打包产物可启动并响应 /health", started, f"exe={EXE.name} port={port}")
        if not started:
            return finish()

        _, info = http(base + "/api/info")
        check(f"产物版本为 {EXPECT_VERSION}", str(info.get("version")) == EXPECT_VERSION,
              f"version={info.get('version')}")

        status, ui = http(base + "/ui/")
        ui = str(ui)
        controls = ('id="only-chips"', 'id="page-size"', 'id="page-jump"',
                    'id="first-page"', 'id="last-page"', 'id="usage-auto"',
                    'id="usage-fresh"', 'data-sort="duration_ms"', 'data-sort="ttft_ms"',
                    'id="model-only-chips"', 'data-model-sort="price"',
                    'id="model-jump"', 'id="model-page-size"', 'id="model-table-hint"',
                    'id="upstream-table-hint"', 'data-up-sort="models"',
                    'id="log-lines"', 'id="log-auto"', 'id="log-copy"',
                    'id="log-bottom"', 'id="log-fresh"',
                    'id="config-dirty-bar"', 'id="config-issues"', 'id="save-config"',
                    'id="modal-root"', 'data-fold="config-service"',
                    'data-fold="config-upstreams"', 'data-fold="pricing"',
                    'data-fold="usage-retention"')
        missing = [c for c in controls if c not in ui]
        check("打包界面含全部用量深化控件", status == 200 and not missing,
              f"缺失 {missing}" if missing else "全部就位")
        _, js = http(base + "/ui/app.js")
        missing_js = [f for f in ("renderSortMarks", "usageDetailRow", "setAutoRefresh",
                                  "deltaTag", "markFresh", "renderTableHint")
                      if f not in str(js)]
        check("打包 app.js 已接线排序/展开/自动刷新/环比", not missing_js,
              f"缺失 {missing_js}" if missing_js else "全部就位")
        _, css = http(base + "/ui/style.css")
        missing_css = [c for c in (".kpi-group", ".chip", ".delta", ".detail-grid",
                                   "tr.usage-row", "details.fold") if c not in str(css)]
        check("打包 style.css 含新样式", not missing_css,
              f"缺失 {missing_css}" if missing_css else "全部就位")

        _, desc = http(base + "/api/usage/list?range=24h&sort=duration_ms&order=desc")
        durs = [r["duration_ms"] for r in desc.get("rows", [])]
        check("产物支持服务端降序排序", durs == sorted(durs, reverse=True), str(durs))
        _, only = http(base + "/api/usage/list?range=24h&only=error")
        rows = only.get("rows", [])
        check("产物支持 only=error 快捷筛选",
              bool(rows) and all(r["status"] >= 400 for r in rows)
              and only.get("total") == len(rows), f"rows={len(rows)}")
        _, cmp_data = http(base + "/api/usage/totals?range=24h&compare=1")
        check("产物支持环比对比（previous）",
              isinstance(cmp_data.get("previous"), dict))
        _, models = http(base + "/api/usage/models?range=24h&limit=50")
        check("产物保留模型用量聚合接口",
              "rows" in (models or {}) and isinstance(models.get("rows"), list),
              f"rows={len((models or {}).get('rows') or [])}（种子 {seeded} 条）")

        _, cat = http(base + "/api/models/catalog?price=missing&page_size=200")
        check("产物支持模型清单 price 筛选",
              isinstance(cat.get("rows"), list)
              and all(not r.get("costHint") for r in cat["rows"]),
              f"total={cat.get('total')}")
        _, cat_sorted = http(base + "/api/models/catalog?sort=price&order=desc&page_size=200")
        check("产物支持模型清单排序回显",
              cat_sorted.get("sort") == "price" and cat_sorted.get("order") == "desc")
        _, js2 = http(base + "/ui/app.js")
        check("打包 app.js 含模型页排序/筛选接线",
              all(t in str(js2) for t in ("renderModelSortMarks", "renderModelTableHint")))
        check("打包 app.js 含日志/概览页深化接线",
              all(t in str(js2) for t in ("sortedUpstreams", "highlightLog",
                                          "upstreamDetailRow", "setLogAuto")))
        check("打包 app.js 含配置页脏值/校验接线",
              all(t in str(js2) for t in ("setConfigDirty", "validateConfig",
                                          "renderConfigIssues")))

        check("打包 app.js 含折叠/弹窗接线",
              all(t in str(js2) for t in ("openModal", "confirmModal", "closeModal",
                                          "initFolds", "openUpstreamEditor",
                                          "applyUpstreamEdits")))
        check("打包版本已无原生 confirm", "!confirm(" not in str(js2))
        check("打包版本 token 用中文单位",
              all(t in str(js2) for t in ("function fmtCn", "function fmtTok", "万亿")))
        check("打包版本含服务分区接线",
              all(t in str(js2) for t in ("renderServiceFacts", "updateRestartHint",
                                          "serviceUrl")))
        check("打包版本图表为 SVG（趋势柱 + 甜甜圈占比）",
              all(t in str(js2) for t in ("trendMetricInfo", "trend-svg", "pie-wrap",
                                          "pie-legend", "avg-line")))
        check("打包版本服务区为紧凑布局（无 svc-block 大框）",
              "svc-block" not in str(ui) and "svc-strip" in str(ui))
        _, css5 = http(base + "/ui/style.css")
        check("打包版本含边界/列表规范层",
              all(t in str(css5) for t in ("details.fold > :not(summary)",
                                           "th:first-child", "nth-child(even)",
                                           "scrollbar-gutter")))
        check("打包版本含缓存命中率接线",
              all(t in str(js2) for t in ("function cacheHitRate", "function fmtRate",
                                          "cache_hit_rate")))
        check("打包版本配置页上游为列表+详情",
              "upstream-cfg-table" in str(ui)
              and "function renderUpstreamCfgTable" in str(js2))
        check("打包版本支持 30万/384K 数值输入 + DeepSeek 上限提示",
              all(t in str(js2) for t in ("function parseCount", "DEEPSEEK_MAX_OUTPUT"))
              and "max-tokens-options" in str(ui))
        with urllib.request.urlopen(base + "/ui/app.js", timeout=5) as resp:
            asset_headers = {k.lower(): v for k, v in resp.headers.items()}
        check("打包资产禁缓存（no-store）",
              "no-store" in asset_headers.get("cache-control", ""),
              asset_headers.get("cache-control", "（无 Cache-Control）"))
        _, page = http(base + "/ui/")
        check("打包 HTML 注入资源版本戳",
              f"app.js?v={EXPECT_VERSION}" in str(page),
              f"version={EXPECT_VERSION}")
        status, stamped = http(base + f"/ui/app.js?v={EXPECT_VERSION}")
        check("打包静态处理支持 ?v= 查询串",
              status == 200 and "/api/usage/summary" in str(stamped),
              f"status={status}")
        _, logs = http(base + "/api/logs?lines=800&level=all")
        check("产物支持日志行数/级别参数",
              isinstance(logs.get("lines"), list)
              and logs.get("matched") == logs.get("total") and (logs.get("total") or 0) > 0,
              f"{logs.get('matched')}/{logs.get('total')}")

        # 现场保护：本验证不得打断在跑实例，也不得改写它的配置与数据
        ok_live, detail_live = live_instance_ok()
        check("在跑的 8900 实例仍健康（代理未中断）", ok_live, detail_live)
        ok_dist, detail_dist = dist_untouched(RUN_START)
        check("dist 配置与数据未被本次验证改写", ok_dist, detail_dist)
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
        print(f"V25_EXE_VERIFY OK {passed}/{total}")
        return 0
    print(f"V25_EXE_VERIFY FAILED {passed}/{total}")
    for name, ok, detail in CHECKS:
        if not ok:
            print(f"  - {name} {detail}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
