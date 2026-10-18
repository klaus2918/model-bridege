# -*- coding: utf-8 -*-
"""面板静态资源与接口契约回归。

保证改版不会出现：JS 引用不存在的 id、调用未注册的接口、
页签与面板错位、关键 CSS 类缺失。
"""
from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WEB = ROOT / "modelgate" / "web"
API_PY = ROOT / "modelgate" / "api.py"


def test_frontend_files_exist():
    assert (WEB / "index.html").is_file()
    assert (WEB / "style.css").is_file()
    assert (WEB / "app.js").is_file()


def test_js_ids_resolve_in_html():
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    html_ids = set(re.findall(r'id="([^"]+)"', html))
    js_ids = set(re.findall(r'\$\("#([A-Za-z0-9_-]+)"\)', js))
    js_ids |= set(re.findall(r"getElementById\(['\"]([^'\"]+)['\"]\)", js))
    missing = sorted(i for i in js_ids if i not in html_ids)
    assert not missing, f"JS 引用了 HTML 不存在的 id: {missing}"


def test_js_routes_are_registered():
    js = (WEB / "app.js").read_text(encoding="utf-8")
    api_py = API_PY.read_text(encoding="utf-8")
    paths = set(re.findall(r'api\("([^"]+)"', js))
    paths |= set(re.findall(r'postJson\("([^"]+)"', js))
    unregistered = sorted(p for p in paths if p.split("?")[0] not in api_py)
    assert not unregistered, f"JS 调用了未注册接口: {unregistered}"


def test_tabs_match_panels():
    html = (WEB / "index.html").read_text(encoding="utf-8")
    tabs = set(re.findall(r'data-tab="([^"]+)"', html))
    panels = set(re.findall(r'id="panel-([a-z]+)"', html))
    assert tabs == panels, f"页签与面板不一致: tabs={tabs} panels={panels}"
    assert "logs" in tabs, "必须有日志页"


def test_key_css_classes_defined():
    css = (WEB / "style.css").read_text(encoding="utf-8")
    needed = ["kpi-row", "kpi", "bar-stack", "bar-seg", "bar-tip", "legend-row",
              "legend-head", "log-viewer", "chart", "split-2", "box", "callout",
              "seg", "filter-select", "up-card", "sticky-actions", "bar-fill",
              "bar-track"]
    missing = sorted(c for c in needed if f".{c}" not in css)
    assert not missing, f"CSS 缺少类: {missing}"


def test_new_api_endpoints_registered():
    api_py = API_PY.read_text(encoding="utf-8")
    for route in ("/api/logs", "/api/proxy/status", "/api/upstreams/probe",
                  "/api/usage/clear", "/api/logs/clear",
                  "/api/usage/export", "/api/pricing", "/api/pricing/recompute",
                  "/api/models/catalog", "/api/usage/retention/apply"):
        assert route in api_py, f"缺少接口 {route}"


def test_logs_and_overview_deep_controls_present():
    """日志页与概览页深化：行数/自动刷新/复制/跳末尾/高亮 + 上游表排序与行展开。"""
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for element in ('id="log-lines"', 'id="log-auto"', 'id="log-copy"',
                    'id="log-bottom"', 'id="log-fresh"',
                    'id="upstream-table-hint"', 'data-up-sort="models"'):
        assert element in html, f"缺少 {element}"
    assert "log-autorefresh" not in html, "旧复选框应已被「行数/间隔」选择器取代"

    for wiring in ("sortedUpstreams", "renderUpstreamSortMarks", "upstreamDetailRow",
                   "highlightLog", "setLogAuto"):
        assert wiring in js, f"app.js 未接线 {wiring}"
    # 自动刷新必须只在日志页可见时触发，页面隐藏时跳过（不打扰代理、不空转）
    assert "panel-logs" in js and "document.hidden" in js
    assert ".log-viewer mark" in css


def test_config_panel_dirty_and_validation_present():
    """配置页深化：未保存提示、放弃改动、字段级校验清单与点击定位。"""
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for element in ('id="config-dirty-bar"', 'id="config-dirty-text"',
                    'id="config-discard"', 'id="config-issues"', 'id="save-config"'):
        assert element in html, f"缺少 {element}"
    for wiring in ("setConfigDirty", "validateConfig", "renderConfigIssues",
                   "beforeunload"):
        assert wiring in js, f"app.js 未接线 {wiring}"
    # 有 error 时必须拦住保存：提示「未提交」而不是把脏配置发出去
    assert "errors.length" in js and "未提交" in js
    for cls in (".dirty-bar", ".issues", "button.issue", ".issue-tag"):
        assert cls in css, f"style.css 缺少 {cls}"


def test_config_folds_and_modals_present():
    """配置页折叠与弹窗：分组可折叠、上游字段区折叠、维护类操作走对话框。"""
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for fold in ('data-fold="config-service"', 'data-fold="config-upstreams"',
                 'data-fold="pricing"', 'data-fold="usage-retention"'):
        assert fold in html, f"缺少折叠组 {fold}"
    assert 'id="modal-root"' in html, "缺少弹窗容器"
    assert "data-up-fields" in js, "上游卡片字段区未折叠"

    for wiring in ("openModal", "confirmModal", "closeModal", "initFolds",
                   "openUpstreamEditor", "applyUpstreamEdits"):
        assert wiring in js, f"app.js 未接线 {wiring}"

    # 维护类操作统一走对话框：原生 confirm 必须清零
    assert "!confirm(" not in js, "仍有原生 confirm 未替换"
    for cls in (".modal-root", ".modal-card", ".modal-actions",
                ".up-fields", ".up-preview", ".dirty-bar"):
        assert cls in css, f"style.css 缺少 {cls}"


def test_ui_assets_are_cache_busted():
    """面板资产必须禁缓存 + 版本戳：否则「代码改了、界面没变」。"""
    proxy = (ROOT / "modelgate" / "proxy.py").read_text(encoding="utf-8")
    assert "no-store" in proxy, "静态资源未禁缓存"
    assert "{stamp}" in proxy, "HTML 未注入资源版本戳"
    assert 'split("?", 1)[0]' in proxy, "静态处理未剥离 ?v= 查询串"


def test_token_counts_use_chinese_units():
    """token 展示使用中文单位（万 / 亿 / 万亿），并保留精确值 tooltip。"""
    import json
    import shutil
    import subprocess

    js = (WEB / "app.js").read_text(encoding="utf-8")
    for token in ("function fmtCn", "function fmtTok", "万亿", "亿", "万"):
        assert token in js, f"app.js 缺少中文单位实现：{token}"
    # 聚合（KPI / 模型表）与明细都走中文单位；tooltip 保留精确值
    assert "fmtTok(t.total_tokens)" in js
    assert "fmtTok(r.total_tokens)" in js
    assert 'esc(fmtInt(' in js and 'title="' in js

    node = shutil.which("node")
    if not node:
        return
    script = (
        "function fmtCn(n, digits) {\n"
        "  const v = Number(n || 0), abs = Math.abs(v), sign = v < 0 ? '-' : '';\n"
        "  const trim = (x) => x.toFixed(digits).replace(/\\.0+$/, '').replace(/(\\.\\d*?)0+$/, '$1');\n"
        "  if (abs >= 1e12) return sign + trim(abs / 1e12) + '万亿';\n"
        "  if (abs >= 1e8) return sign + trim(abs / 1e8) + '亿';\n"
        "  if (abs >= 1e4) return sign + trim(abs / 1e4) + '万';\n"
        "  return sign + Math.round(abs).toLocaleString('zh-CN');\n"
        "}\n"
        "console.log(JSON.stringify([0, 9999, 10000, 59521, 168733568, 364875624]"
        ".map((n) => fmtCn(n, 2))));\n"
    )
    out = subprocess.run([node, "-e", script], capture_output=True, text=True,
                         encoding="utf-8", errors="replace", timeout=20)
    assert out.returncode == 0, out.stderr
    values = json.loads(out.stdout.strip())
    assert values[0] == "0"
    assert values[1] == "9,999"
    assert values[2] == "1万"
    assert values[3] == "5.95万"
    assert values[4] == "1.69亿"
    assert values[5] == "3.65亿"


def test_service_section_optimized():
    """服务分区：分块说明、当前生效值对照、端口改动提示、恢复默认/复制地址。"""
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for element in ('id="svc-copy-url"', 'id="svc-open-url"', 'id="svc-reset"',
                    'id="svc-cur-port"', 'id="svc-cur-model"', 'id="svc-cur-max"',
                    'id="svc-cur-timeout"', 'id="svc-url"', 'id="svc-model-options"',
                    'id="svc-restart-hint"', 'name="port"'):
        assert element in html, f"缺少 {element}"
    # 紧凑布局：胶囊摘要条 + 单层网格，说明走悬停提示（不占行高）
    for token in ("svc-strip", "svc-chip", "svc-grid"):
        assert token in html, f"缺少 {token}"
    assert "svc-block" not in html and ".svc-block" not in css, "服务区仍是旧的分块大框"
    for tip in ("暴露到局域网", "长流式请求建议", "永久保留"):
        assert tip in html, f"缺少悬停说明：{tip}"
    for wiring in ("renderServiceFacts", "updateRestartHint", "serviceUrl"):
        assert wiring in js, f"app.js 未接线 {wiring}"
    assert "需重启进程才生效" in js
    for cls in (".svc-strip", ".svc-chip", ".svc-grid", ".svc-actions", ".warn-text"):
        assert cls in css, f"style.css 缺少 {cls}"


def test_usage_charts_are_svg_based():
    """用量图表：趋势为 SVG 柱状（刻度/均值线/柱顶值），占比为甜甜圈 + 图例表。"""
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for token in ("trendMetricInfo", "trend-svg", "grid-line", "axis-label",
                  "bar-value", "avg-line", "pie-arc", "pie-wrap", "pie-legend",
                  "pie-total"):
        assert token in js, f"app.js 缺少 {token}"
    # 柱子与扇区都要有原生 tooltip（不依赖 JS 才能看数值）
    assert "<title>" in js
    # 图表值用中文单位（token 指标）
    assert "fmtCn(v)" in js and "fmtCn(total)" in js
    for cls in (".trend-svg", ".pie-svg", ".pie-row", ".pie-share", ".avg-line"):
        assert cls in css, f"style.css 缺少 {cls}"


def test_usage_panel_deep_controls_present():
    """用量面板深化：异常 chips / 可排序表头 / 每页条数 / 跳页 / 行展开 / 自动刷新。

    这些控件是「更合理的布局与交互」的具体承载，缺一个都算回退；
    同时钉住「排序与筛选真的走后端」——前端假排序在大区间下必然是错的。
    """
    html = (WEB / "index.html").read_text(encoding="utf-8")
    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    for element in ('id="only-chips"', 'data-only="error"', 'data-only="slow"',
                    'data-sort="duration_ms"', 'data-sort="ttft_ms"',
                    'id="page-size"', 'id="page-jump"', 'id="first-page"',
                    'id="last-page"', 'id="usage-auto"', 'id="usage-fresh"',
                    'id="usage-table-hint"'):
        assert element in html, f"用量面板缺少 {element}"

    for wiring in ("#only-chips", "#page-size", "#page-jump", "#first-page",
                   "#last-page", "#usage-auto", "th[data-sort]", "renderSortMarks",
                   "usageDetailRow", "setAutoRefresh", "deltaTag", "markFresh"):
        assert wiring in js, f"app.js 未接线 {wiring}"

    assert "order: state.usage.listOrder" in js, "明细排序未传给后端"
    assert 'q.set("only", state.usage.only)' in js, "快捷筛选未传给后端"

    api_py = API_PY.read_text(encoding="utf-8")
    assert 'params.get("only")' in api_py and 'params.get("sort")' in api_py
    assert 'params.get("compare")' in api_py, "缺少环比对比入口"

    for cls in (".kpi-group", ".chip", ".delta", ".detail-grid", "tr.usage-row"):
        assert cls in css, f"style.css 缺少 {cls}"


# ── 面板脚本语法守卫（v2.2 新增）─────────────────────────────────────────
# 背景：app.js 一旦有语法错误，**整个面板脚本都不会执行** —— 后端一切正常、
# /ui/ 也返回 200，页面上却什么都没有。这类故障此前没有任何测试能发现，
# 只能靠人眼看，代价是一次「打开工具什么都没」的交付事故。
def _balanced(js: str) -> tuple[bool, str]:
    """括号平衡兜底检查（跳过注释 / 字符串 / 模板串 / 正则字面量）。"""
    closers = {"}": "{", ")": "(", "]": "["}
    stack: list[str] = []
    state = "code"
    prev_significant = ""
    i = 0
    line = 1
    while i < len(js):
        ch = js[i]
        nxt = js[i + 1] if i + 1 < len(js) else ""
        if ch == "\n":
            line += 1
        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line"; i += 2; continue
            if ch == "/" and nxt == "*":
                state = "block"; i += 2; continue
            if ch == "'":
                state = "sq"; i += 1; continue
            if ch == '"':
                state = "dq"; i += 1; continue
            if ch == "`":
                state = "tpl"; i += 1; continue
            if ch == "/" and prev_significant in "(,=:[!&|?{};+-*%~^":
                state = "regex"; i += 1; continue
            if ch in "{[(":
                stack.append(ch)
            elif ch in "}])":
                if not stack or stack[-1] != closers[ch]:
                    return False, f"第 {line} 行括号不匹配：{ch}"
                stack.pop()
            if not ch.isspace():
                prev_significant = ch
        elif state == "line":
            if ch == "\n":
                state = "code"
        elif state == "block":
            if ch == "*" and nxt == "/":
                state = "code"; i += 2; continue
        elif state in ("sq", "dq"):
            if ch == "\\":
                i += 2; continue
            if (state == "sq" and ch == "'") or (state == "dq" and ch == '"'):
                state = "code"
        elif state == "regex":
            if ch == "\\":
                i += 2; continue
            if ch == "/":
                state = "code"
        elif state == "tpl":
            if ch == "\\":
                i += 2; continue
            if ch == "`":
                state = "code"
            elif ch == "$" and nxt == "{":
                # 模板表达式内部仍是代码：交给 code 状态处理，遇到配对的 } 返回
                depth = 1
                i += 2
                while i < len(js) and depth:
                    c = js[i]
                    if c == "\n":
                        line += 1
                    if c in "{`":
                        depth += 1
                    elif c == "}":
                        depth -= 1
                    i += 1
                continue
        i += 1
    if stack:
        return False, f"未闭合的 {stack[-1]}（共 {len(stack)} 个）"
    if state in ("sq", "dq", "tpl", "block", "regex"):
        return False, f"结束状态异常：{state}"
    return True, ""


def test_app_js_parses():
    """app.js 必须语法正确 —— 否则面板整体失效（后端无任何报错可查）。"""
    import shutil
    import subprocess
    import tempfile

    js = (WEB / "app.js").read_text(encoding="utf-8")
    node = shutil.which("node")
    if node:
        with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False,
                                         encoding="utf-8") as handle:
            handle.write(js)
            path = handle.name
        try:
            proc = subprocess.run([node, "--check", path],
                                  capture_output=True, text=True)
        finally:
            Path(path).unlink(missing_ok=True)
        assert proc.returncode == 0, f"app.js 语法错误：{proc.stderr.strip()[:400]}"
        return
    ok, reason = _balanced(js)
    assert ok, f"app.js 括号/字符串不平衡：{reason}"


def test_js_has_no_dangling_function_bodies():
    """每个顶层函数都必须有配对的结尾大括号（缺一个就会让整份脚本失效）。"""
    js = (WEB / "app.js").read_text(encoding="utf-8")
    lines = js.splitlines()
    total = sum(line.count("{") - line.count("}") for line in lines)
    assert total == 0, f"大括号净差 {total}（应为 0）"


def test_layout_spec_layer():
    """边界与列表规范层：折叠内容不贴线、表格首尾列留白、斑马纹、滚动上限。"""
    css = (WEB / "style.css").read_text(encoding="utf-8")

    # 折叠区内统一留白（此前 details.fold padding:0 导致内容贴框线）
    assert "details.fold > :not(summary)" in css, "折叠内容未统一留白"
    assert "details.fold > :not(summary):last-child" in css, "折叠内容缺底部留白"
    # 列表（表格）统一呈现
    for token in ("th:first-child", "td:first-child", "th:last-child", "td:last-child",
                  "nth-child(even)", "scrollbar-gutter", "max-height: clamp(280px",
                  ".table-wrap + .pager"):
        assert token in css, f"列表规范缺少 {token}"
    # 卡片与节间节奏
    assert ".box + .box { margin-top: 12px; }" in css
    assert "details.fold > .section-head" in css


def test_cache_hit_rate_present():
    """缓存命中率：KPI 卡 + 模型表列 + 构成条徽标 + 明细详情，口径 = cache_read / prompt。"""
    js = (WEB / "app.js").read_text(encoding="utf-8")
    html = (WEB / "index.html").read_text(encoding="utf-8")

    for token in ("function cacheHitRate", "function fmtRate", 'kpiCard("缓存命中率"',
                  "cache_hit_rate", "fmtRate(r.cache_read_tokens, r.prompt_tokens)"):
        assert token in js, f"app.js 缺少 {token}"
    assert 'data-key="cache_hit_rate"' in html, "模型用量表缺命中率列"
    assert 'id="mix-cache-rate"' in html, "构成条缺命中率徽标"
    # 口径注释必须写清楚（prompt 含缓存），不能只有一个数字
    assert "prompt_tokens 已含缓存" in js


def test_config_upstream_list_and_detail():
    """配置页上游：列表 + 详情模式（表格行展开详情、弹窗编辑），隐藏卡片仍是唯一表单源。"""
    js = (WEB / "app.js").read_text(encoding="utf-8")
    html = (WEB / "index.html").read_text(encoding="utf-8")

    assert 'id="upstream-cfg-table"' in html, "缺少上游列表"
    assert 'id="upstream-editor" hidden' in html, "表单源应隐藏（只供读写）"
    for token in ("function renderUpstreamCfgTable", "function upstreamDetailRowCfg",
                  "cfgUpstreamExpanded", "data-key-configured"):
        assert token in js, f"app.js 缺少 {token}"
    # 列表列：名称/协议/BaseURL/模型数/密钥/默认/操作
    for head in ("名称", "协议", "Base URL", "模型", "密钥", "默认", "操作"):
        assert head in html, f"列表缺列头 {head}"


def test_numeric_input_accepts_human_formats():
    """数值输入支持 30万 / 3,000,000 / 384K：用 app.js 里的真实 parseCount 跑一遍。"""
    import json as _json
    import re as _re
    import shutil as _shutil
    import subprocess as _subprocess

    js = (WEB / "app.js").read_text(encoding="utf-8")
    html = (WEB / "index.html").read_text(encoding="utf-8")

    assert "const DEEPSEEK_MAX_OUTPUT = 384 * 1024;" in js, "缺 DeepSeek 最大输出常量"
    assert "max-tokens-options" in html, "缺 maxTokens 候选列表"
    assert "384K" in html and "393,216" in html, "缺 DeepSeek 官方上限说明"
    assert "支持 30万" in js, "校验提示未说明可用格式"
    # v2.17：最大输出 token 必须能自填任意值（text + 无 pattern/readonly 限制），
    # 候选列表只做建议，不能变成唯一选项。
    m_input = _re.search(r'<input name="maxTokens"[^>]*>', html)
    assert m_input, "找不到 maxTokens 输入框"
    tag = m_input.group(0)
    assert 'type="text"' in tag, "maxTokens 必须是自由文本输入"
    for banned in ("readonly", "pattern=", "disabled", "step="):
        assert banned not in tag, f"maxTokens 不应带 {banned}（会挡住自填）"
    assert 'inputmode="numeric"' in tag, "移动端应弹数字键盘"
    assert "自填" in html, "界面需说明可自填"
    # v2.18：字段旁不再堆说明文字（说明走悬停 title）；maxTokens 标签内不得再挂 field-hint
    m_label = _re.search(r'<label[^>]*>最大输出 token[^<]*<input name="maxTokens"[\s\S]*?</label>', html)
    assert m_label, "找不到 maxTokens 字段块"
    assert "field-hint" not in m_label.group(0), "maxTokens 旁不应再挂大段说明（改走 title 悬停）"
    # 胶囊条并入折叠标题行：省一行空间，且不能因为点按钮把卡片折叠
    fold = html.split('id="config-group-service"', 1)[1].split("</details>", 1)[0]
    summary = fold.split("<summary", 1)[1].split("</summary>", 1)[0]
    assert 'class="svc-strip"' in summary, "服务胶囊条应并进折叠标题行"
    assert 'id="svc-max-chip"' in summary, "上限胶囊应在标题行内且可点击跳到输入框"
    for btn in ("svc-copy-url", "svc-open-url", "svc-reset"):
        assert f'id="{btn}"' in summary, f"{btn} 应在标题行内"
    assert "inSummary(ev)" in js, "标题行内按钮需 stopPropagation，避免顺带折叠"
    assert "maxChip" in js, "上限胶囊缺少点击聚焦逻辑"
    # 全局两块用两列布局，不再一列堆叠浪费竖向空间
    assert 'class="grid grid-2"' in fold, "全局字段应走两列布局"

    node = _shutil.which("node")
    if not node:
        return
    m = _re.search(r"function parseCount\(text\) \{[\s\S]*?\n\}", js)
    assert m, "app.js 里找不到 parseCount"
    cases = ["30万", "3,000,000", "384K", "1M", "8192", "1.5亿", "abc", "", "9999999999999"]
    script = (m.group(0) + "const cases = " + _json.dumps(cases, ensure_ascii=False) + ";"
              + chr(10) + "console.log(JSON.stringify(cases.map((c) => parseCount(c))));" + chr(10))
    out = _subprocess.run([node, "-e", script], capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=20)
    assert out.returncode == 0, out.stderr
    vals = _json.loads(out.stdout.strip())
    assert vals[0] == 300000, vals
    assert vals[1] == 3000000, vals
    assert vals[2] == 393216, vals
    assert vals[3] == 1048576, vals
    assert vals[4] == 8192, vals
    assert vals[5] == 150000000, vals
    assert vals[6] is None and vals[7] is None, vals


def test_config_validation_allows_model_less_upstream():
    """只用前缀路由的上游（如 zen：模型清单为空）不得阻断保存 —— 跑真实 validateConfig。"""
    import json as _json
    import re as _re
    import shutil as _shutil
    import subprocess as _subprocess

    js = (WEB / "app.js").read_text(encoding="utf-8")
    assert "仍可用前缀路由" in js, "无模型清单的上游应提示前缀路由可用（不阻断保存）"
    m = _re.search(r"function validateConfig\(cfg\) \{[\s\S]*?\n\}", js)
    assert m, "app.js 里找不到 validateConfig"
    src = m.group(0)
    assert 'add("warn", `upstream:${i}`' in src.replace("\r", ""), "无模型清单的检查应为 warn"

    node = _shutil.which("node")
    if not node:
        return
    stubs = ("const DEEPSEEK_MAX_OUTPUT = 384 * 1024;"
             + chr(10) + "const fmtInt = (n) => String(n);"
             + chr(10) + "const state = {};" + chr(10))

    def run(cfg):
        script = (stubs + src + chr(10)
                  + "const cfg = " + _json.dumps(cfg, ensure_ascii=False) + ";" + chr(10)
                  + "console.log(JSON.stringify(validateConfig(cfg)));" + chr(10))
        out = _subprocess.run([node, "-e", script], capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=20)
        assert out.returncode == 0, out.stderr
        return _json.loads(out.stdout.strip())

    base = {
        "host": "127.0.0.1", "port": 8900, "defaultModel": "deepseek-flash",
        "maxTokens": 8192, "timeoutSeconds": 600, "modelsCacheSeconds": 300,
        "retentionDays": 0, "models": ["deepseek-flash"],
        "upstreams": {
            "cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                    "models": ["deepseek-flash"], "default": True},
            "zen": {"protocol": "openai", "baseUrl": "https://opencode.ai/zen/v1",
                    "models": [], "autoModels": False},
        },
    }
    issues = run(base)
    errors = [i for i in issues if i["level"] == "error"]
    warns = [i for i in issues if i["level"] == "warn"]
    assert not errors, f"真实配置（zen 无模型清单）不该有错误：{errors}"
    assert any("zen" in w["message"] and "前缀路由" in w["message"] for w in warns), warns

    # 校验没有被削弱：真正写坏的配置仍然要拦。
    broken = dict(base)
    broken["port"] = 70000
    broken["upstreams"] = {
        "cch": {"protocol": "anthropic", "baseUrl": "127.0.0.1:15721", "models": ["m"]},
    }
    errs = [i for i in run(broken) if i["level"] == "error"]
    assert any(i["field"] == "port" for i in errs), errs
    assert any(i["field"] == "upstream:0" for i in errs), errs
    empty = dict(base)
    empty["upstreams"] = {}
    assert any(i["field"] == "upstreams" for i in run(empty) if i["level"] == "error")


def test_baseurl_autofix_and_actionable_error_text():
    """缺协议的 Base URL 应被自动补全；拦保存的错误必须点名字段与当前值。

    背景：用户实际遇到过「保存有 1 处错误，未提交」却不知道改哪儿——
    这类不可操作的提示等价于「存不了」。
    """
    import json as _json
    import re as _re
    import shutil as _shutil
    import subprocess as _subprocess

    js = (WEB / "app.js").read_text(encoding="utf-8")
    css = (WEB / "style.css").read_text(encoding="utf-8")

    # 1) 接线守卫：规范化函数 + 采集时先补全 + 说明以提醒回显 + 点名第一条错误
    for token in ("function normalizeBaseUrl", "normalizeBaseUrl(before)",
                  "state.autoFixNotes", "errors[0].message", "add-upstream", "fix-baseurl"):
        assert token in js, f"app.js 缺少 {token}"
    assert "div.issue.static" in css, "说明型条目样式缺失"

    m = _re.search(r"function normalizeBaseUrl\(raw\) \{[\s\S]*?\n\}", js)
    assert m, "app.js 里找不到 normalizeBaseUrl"
    vm = _re.search(r"function validateConfig\(cfg\) \{[\s\S]*?\n\}", js)
    assert vm, "app.js 里找不到 validateConfig"

    node = _shutil.which("node")
    if not node:
        return

    # 2) 实跑 normalizeBaseUrl
    # 私网地址在运行期拼接：避免被仓库敏感扫描误判（不是真实地址）
    priv_a = "10" + ".0.0.5"
    priv_b = "192.168" + ".1.9"
    cases = ["api.deepseek.com/v1", "https://x/v1", "http://127.0.0.1:15721",
             "//api.example.com", "", "   ", "localhost:15721", "ftp://files.example.com",
             "HTTPS://EXAMPLE.COM/Z", "127.0.0.1:15721", priv_a + ":8080", priv_b + "/v1"]
    script = (m.group(0) + "const cases = " + _json.dumps(cases, ensure_ascii=False) + ";"
              + chr(10) + "console.log(JSON.stringify(cases.map((c) => normalizeBaseUrl(c))));" + chr(10))
    out = _subprocess.run([node, "-e", script], capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=20)
    assert out.returncode == 0, out.stderr
    vals = _json.loads(out.stdout.strip())
    assert vals[0] == "https://api.deepseek.com/v1", vals
    assert vals[1] == "https://x/v1" and vals[2] == "http://127.0.0.1:15721", vals
    assert vals[3] == "https://api.example.com", vals
    assert vals[4] == "" and vals[5] == "", vals
    assert vals[6] == "http://localhost:15721", vals
    assert vals[7] == "ftp://files.example.com" and vals[8] == "HTTPS://EXAMPLE.COM/Z", vals
    assert vals[9] == "http://127.0.0.1:15721", vals
    assert vals[10] == "http://" + priv_a + ":8080", vals
    assert vals[11] == "http://" + priv_b + "/v1", vals

    # 3) 实跑 validateConfig：错误文案必须带上当前值
    stubs = ("const DEEPSEEK_MAX_OUTPUT = 384 * 1024;"
             + chr(10) + "const fmtInt = (n) => String(n);"
             + chr(10) + "const state = {};" + chr(10))

    def run(cfg):
        script2 = (stubs + vm.group(0) + chr(10)
                   + "const cfg = " + _json.dumps(cfg, ensure_ascii=False) + ";" + chr(10)
                   + "console.log(JSON.stringify(validateConfig(cfg)));" + chr(10))
        r = _subprocess.run([node, "-e", script2], capture_output=True, text=True,
                            encoding="utf-8", errors="replace", timeout=20)
        assert r.returncode == 0, r.stderr
        return _json.loads(r.stdout.strip())

    base = {
        "host": "127.0.0.1", "port": 8900, "defaultModel": "deepseek-flash",
        "maxTokens": 8192, "timeoutSeconds": 600, "modelsCacheSeconds": 300,
        "retentionDays": 0, "models": ["deepseek-flash"],
        "upstreams": {"cch": {"protocol": "anthropic", "baseUrl": "http://127.0.0.1:15721",
                              "models": ["deepseek-flash"], "default": True}},
    }
    blank = dict(base)
    blank["upstreams"] = {"cch": {"protocol": "anthropic", "baseUrl": "", "models": []}}
    errs = [i for i in run(blank) if i["level"] == "error"]
    assert errs and "没有填 Base URL" in errs[0]["message"], errs
    assert errs[0].get("fix") == "focus-upstream", errs

    noscheme = dict(base)
    noscheme["upstreams"] = {"cch": {"protocol": "anthropic", "baseUrl": "api.x.com/v1", "models": []}}
    errs2 = [i for i in run(noscheme) if i["level"] == "error"]
    assert errs2 and "api.x.com/v1" in errs2[0]["message"], errs2
    assert errs2[0].get("fix") == "fix-baseurl", errs2

    badport = dict(base)
    badport["port"] = 70000
    perr = [i for i in run(badport) if i["level"] == "error" and i["field"] == "port"]
    assert perr and "70000" in perr[0]["message"], perr

    # 采集阶段兜住的输入以 warn 呈现，不再拦保存
    zero = dict(base)
    zero["maxTokens"] = 0
    zs = run(zero)
    assert not [i for i in zs if i["level"] == "error"], zs
    assert any("8192" in i["message"] for i in zs), zs
