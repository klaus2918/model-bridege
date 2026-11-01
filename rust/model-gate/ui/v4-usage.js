/* v4 用量分析、请求明细与运行日志。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, $$, esc, num, compactNum, fmtTime, fmtMs, api, toast, openModal, closeModal } = MG;

  const u = {
    range: "7d",
    upstream: "",
    model: "",
    session: "",
    currency: "CNY",
    page: 1,
    pageSize: 50,
    total: 0,
    rows: [],
    only: "",
    autoTimer: null,
    loading: false,
  };
  const logs = { timer: null, bound: false };
  let bound = false;

  // 作用域持久化：用量页是高频页面，刷新后应保留上次的时间范围与筛选。
  const SCOPE_KEY = "mg.v4.usage.scope";

  function saveScope() {
    try {
      localStorage.setItem(
        SCOPE_KEY,
        JSON.stringify({
          range: u.range,
          upstream: u.upstream,
          model: u.model,
          session: u.session,
          currency: u.currency,
          only: u.only,
          pageSize: u.pageSize,
          auto: $("#usage-auto")?.value || "0",
          metric: $("#usage-trend-metric")?.value || "total_tokens",
        })
      );
    } catch {
      /* 隐私模式下不可写，忽略 */
    }
  }

  function restoreScope() {
    let saved = {};
    try {
      saved = JSON.parse(localStorage.getItem(SCOPE_KEY) || "{}") || {};
    } catch {
      saved = {};
    }
    if (typeof saved.range === "string" && saved.range) u.range = saved.range;
    if (typeof saved.upstream === "string") u.upstream = saved.upstream;
    if (typeof saved.model === "string") u.model = saved.model;
    if (typeof saved.session === "string") u.session = saved.session;
    if (saved.currency === "USD" || saved.currency === "CNY") u.currency = saved.currency;
    if (typeof saved.only === "string") u.only = saved.only;
    if (Number(saved.pageSize)) u.pageSize = Number(saved.pageSize);
    const set = (selector, value) => {
      if (value == null || value === "") return; // 没有保存过就沿用 HTML 默认值
      const el = $(selector);
      if (el && Array.from(el.options).some((option) => option.value === String(value))) el.value = String(value);
    };
    set("#usage-range", u.range);
    set("#usage-currency", u.currency);
    set("#usage-only", u.only);
    set("#usage-page-size", u.pageSize);
    set("#usage-auto", saved.auto);
    set("#usage-trend-metric", saved.metric);
  }

  // 按时间范围选择分桶粒度：日内看小时曲线，跨天看日曲线，避免 24h 只有一两根柱子。
  function groupByFor(range) {
    return ["today", "yesterday", "24h"].includes(range) ? "hour" : "day";
  }

  function bucketLabel(bucket, groupBy) {
    const text = String(bucket || "");
    if (groupBy === "hour") {
      // 后端小时桶形如 2026-11-01T15；渲染成 15:00 更贴近看日志/排障的习惯。
      const hour = text.match(/T(\d{2})/);
      if (hour) return `${hour[1]}:00`;
      const clock = text.match(/(\d{2}:\d{2})/);
      return clock ? clock[1] : text;
    }
    return text.slice(5);
  }

  function fx() {
    return Number(u.fx || 7.2) || 7.2;
  }

  function money(value) {
    const n = Number(value || 0);
    if (u.currency === "USD") return `$${(n / fx()).toFixed(4)}`;
    return `¥${n.toFixed(4)}`;
  }

  function pct(value) {
    if (value == null || Number.isNaN(Number(value))) return "—";
    return `${(Number(value) * 100).toFixed(1)}%`;
  }

  function query(extra = {}) {
    const params = new URLSearchParams({ range: u.range });
    if (u.upstream) params.set("upstream", u.upstream);
    if (u.model) params.set("model", u.model);
    if (u.session) params.set("session", u.session);
    Object.entries(extra).forEach(([key, value]) => {
      if (value !== "" && value != null) params.set(key, String(value));
    });
    return params;
  }

  function fillSelect(selector, values, selected, blank) {
    const host = $(selector);
    if (!host) return;
    const current = selected == null ? host.value : selected;
    host.innerHTML = `<option value="">${esc(blank)}</option>${values
      .map((value) => `<option value="${esc(value)}">${esc(value)}</option>`)
      .join("")}`;
    host.value = values.includes(current) ? current : "";
  }

  async function load() {
    if (u.loading) return;
    u.loading = true;
    try {
      const pageParams = query({ limit: u.pageSize, offset: (u.page - 1) * u.pageSize, only: u.only });
      const base = query({ limit: 500 });
      const [totalsData, summaryData, upstreamData, modelData, sessionData, listData] = await Promise.all([
        api(`/api/usage/totals?${query()}`),
        api(`/api/usage/summary?${query({ group_by: groupByFor(u.range) })}`),
        api(`/api/usage/upstreams?${base}`),
        api(`/api/usage/models?${base}`),
        api(`/api/usage/sessions?${query({ limit: 300 })}`),
        api(`/api/usage/list?${pageParams}`),
      ]);
      u.totals = totalsData.totals || {};
      u.summary = Array.isArray(summaryData.rows) ? summaryData.rows : [];
      u.upstreams = Array.isArray(upstreamData.rows) ? upstreamData.rows : [];
      u.models = Array.isArray(modelData.rows) ? modelData.rows : [];
      u.sessions = Array.isArray(sessionData.rows) ? sessionData.rows : [];
      u.rows = Array.isArray(listData.rows) ? listData.rows : [];
      u.total = Number(listData.total || u.rows.length);
      u.fx = totalsData.fx?.USD || summaryData.fx?.USD || modelData.fx?.USD || 7.2;
      fillSelect("#usage-upstream", u.upstreams.map((row) => row.upstream), u.upstream, "全部上游");
      fillSelect("#usage-model", u.models.map((row) => row.model), u.model, "全部模型");
      fillSelect("#usage-session", u.sessions.map((row) => row.session).filter(Boolean), u.session, "全部会话");
      render();
    } finally {
      u.loading = false;
    }
  }

  function renderKpis() {
    const t = u.totals || {};
    $("#usage-kpis").innerHTML = `
      <div class="usage-kpi"><div class="k">请求数</div><div class="v">${num(t.requests)}</div><div class="s">错误 ${num(t.errors)}</div></div>
      <div class="usage-kpi"><div class="k">总 Token</div><div class="v">${compactNum(t.total_tokens)}</div><div class="s">输入 ${compactNum(t.prompt_tokens)} · 输出 ${compactNum(t.completion_tokens)}</div></div>
      <div class="usage-kpi ${Number(t.errors || 0) ? "warn" : "signal"}"><div class="k">成功率</div><div class="v">${pct(t.success_rate)}</div><div class="s">缓存命中 ${pct(t.cache_hit_rate)}</div></div>
      <div class="usage-kpi"><div class="k">估算费用</div><div class="v">${money(t.cost)}</div><div class="s">${u.currency} · 汇率 ${fx().toFixed(2)}</div></div>
      <div class="usage-kpi"><div class="k">P95 耗时</div><div class="v">${fmtMs(t.p95_duration_ms)}</div><div class="s">均值 ${fmtMs(t.avg_duration_ms)}</div></div>`;
  }

  function renderMix() {
    const t = u.totals || {};
    const total = Number(t.prompt_tokens || 0) + Number(t.completion_tokens || 0);
    const inPct = total ? (Number(t.prompt_tokens || 0) / total) * 100 : 0;
    const outPct = total ? (Number(t.completion_tokens || 0) / total) * 100 : 0;
    const cachePct = Number(t.prompt_tokens || 0) ? (Number(t.cache_read_tokens || 0) / Number(t.prompt_tokens)) * 100 : 0;
    $("#usage-mix").innerHTML = `
      <div class="mixbar-row"><span>输入</span><div class="mixbar-track"><i style="width:${Math.min(100, inPct)}%"></i></div><b>${compactNum(t.prompt_tokens)}</b></div>
      <div class="mixbar-row"><span>输出</span><div class="mixbar-track"><i class="out" style="width:${Math.min(100, outPct)}%"></i></div><b>${compactNum(t.completion_tokens)}</b></div>
      <div class="mixbar-row"><span>缓存命中</span><div class="mixbar-track"><i class="cache" style="width:${Math.min(100, cachePct)}%"></i></div><b>${pct(t.cache_hit_rate)}</b></div>`;
    $("#usage-mix-hint").textContent = total ? `${compactNum(total)} tokens` : "—";
    $("#usage-mix-legend").innerHTML = `<div class="row"><span class="grow muted">输入 / 输出</span><span class="trail">${compactNum(t.prompt_tokens)} / ${compactNum(t.completion_tokens)}</span></div>
      <div class="row"><span class="grow muted">缓存读取</span><span class="trail">${compactNum(t.cache_read_tokens)} tokens</span></div>`;
  }

  function renderLatency() {
    const t = u.totals || {};
    $("#usage-latency").innerHTML = `
      <div class="row"><span class="grow muted">均值</span><span class="trail">${fmtMs(t.avg_duration_ms)}</span></div>
      <div class="row"><span class="grow muted">P50</span><span class="trail">${fmtMs(t.p50_duration_ms)}</span></div>
      <div class="row"><span class="grow muted">P95</span><span class="trail">${fmtMs(t.p95_duration_ms)}</span></div>
      <div class="row"><span class="grow muted">最长</span><span class="trail">${fmtMs(t.max_duration_ms)}</span></div>
      <div class="row"><span class="grow muted">TTFT 均值</span><span class="trail">${fmtMs(t.avg_ttft_ms)}</span></div>`;
  }

  function renderTrend() {
    const metric = $("#usage-trend-metric").value;
    const groupBy = groupByFor(u.range);
    // 不再截断：30d 也要能看到完整曲线；刻度密度按容器宽度收敛，避免窄屏重叠。
    const rows = u.summary || [];
    const max = Math.max(1, ...rows.map((row) => Number(row[metric] || 0)));
    const host = $("#usage-trend");
    const hostWidth = host.clientWidth || 900;
    const maxLabels = Math.max(3, Math.floor(hostWidth / 64));
    const every = Math.max(1, Math.ceil(rows.length / maxLabels));
    host.innerHTML = rows.length
      ? `<div class="bars">${rows
          .map(
            (row, index) => `<div class="bar-col" title="${esc(row.bucket)} · ${num(row[metric])}">
              <div class="bar" style="height:${Math.max(3, (Number(row[metric] || 0) / max) * 100)}%"></div>
              <span>${index % every === 0 || index === rows.length - 1 ? esc(bucketLabel(row.bucket, groupBy)) : ""}</span>
            </div>`
          )
          .join("")}</div>`
      : `<div class="empty-line">当前范围没有数据。</div>`;
  }

  function renderPie() {
    let rows;
    let hint;
    if (u.model) {
      rows = u.upstreams.map((row) => ({ label: row.upstream, value: row.total_tokens }));
      hint = "按上游";
    } else if (u.upstream) {
      rows = u.models.map((row) => ({ label: row.model, value: row.total_tokens }));
      hint = "按模型";
    } else {
      rows = u.upstreams.map((row) => ({ label: row.upstream, value: row.total_tokens }));
      hint = "按上游";
    }
    rows = rows.sort((a, b) => Number(b.value || 0) - Number(a.value || 0)).slice(0, 12);
    const total = rows.reduce((sum, row) => sum + Number(row.value || 0), 0);
    $("#usage-pie-hint").textContent = hint;
    $("#usage-pie").innerHTML = rows.length
      ? rows
          .map((row) => {
            const p = total ? (Number(row.value || 0) / total) * 100 : 0;
            return `<div class="pie-row"><span class="pie-label">${esc(row.label || "(无)")}</span><div class="pie-track"><i style="width:${p}%"></i></div><b>${p.toFixed(1)}%</b></div>`;
          })
          .join("")
      : `<div class="empty-line">当前范围没有数据。</div>`;
  }

  function renderUpstreamTable() {
    const tbody = $("#usage-upstream-table tbody");
    tbody.innerHTML = (u.upstreams || []).length
      ? u.upstreams
          .map(
            (row) => `<tr>
              <td>${esc(row.upstream)}</td><td class="num">${num(row.models_count)}</td><td class="num">${pct(row.cache_hit_rate)}</td>
              <td class="num">${num(row.requests)}</td><td class="num">${pct(row.success_rate)}</td><td class="num">${fmtMs(row.p95_duration_ms)}</td><td class="num">${money(row.cost)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有上游用量。</div></td></tr>`;
  }

  function renderSessionTable() {
    const tbody = $("#usage-session-table tbody");
    tbody.innerHTML = (u.sessions || []).length
      ? u.sessions
          .slice(0, 100)
          .map(
            (row) => `<tr class="clickable" data-usage-session="${esc(row.session || "")}">
              <td><code>${esc(row.session || "(无会话)")}</code></td><td class="num">${num(row.requests)}</td><td class="num">${num(row.errors)}</td>
              <td class="num">${compactNum(row.total_tokens)}</td><td class="num">${pct(row.success_rate)}</td><td class="num">${fmtMs(row.p95_duration_ms)}</td>
              <td>${fmtTime(row.first_ts)} → ${fmtTime(row.last_ts)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有会话记录。</div></td></tr>`;
  }

  function renderModelTable() {
    const tbody = $("#usage-model-table tbody");
    tbody.innerHTML = (u.models || []).length
      ? u.models
          .map(
            (row) => `<tr>
              <td><code>${esc(row.model)}</code></td><td class="num">${num(row.requests)}</td>
              <td class="num">${compactNum(row.prompt_tokens)}</td><td class="num">${compactNum(row.completion_tokens)}</td>
              <td class="num">${compactNum(row.total_tokens)}</td><td class="num">${money(row.cost)}</td><td class="num">${pct(row.success_rate)}</td>
              <td class="num">${fmtMs(row.p95_duration_ms)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="8"><div class="empty-line">没有模型用量。</div></td></tr>`;
  }

  function renderDetails() {
    const tbody = $("#usage-detail-table tbody");
    tbody.innerHTML = u.rows.length
      ? u.rows
          .map(
            (row) => `<tr class="clickable" data-usage-row="${esc(row.id)}">
              <td>${fmtTime(row.ts)}</td><td><code>${esc(row.requested_model || row.upstream_model)}</code></td><td>${esc(row.upstream)}</td>
              <td class="num">${compactNum(row.prompt_tokens)} / ${compactNum(row.completion_tokens)}</td>
              <td class="num">${money(row.cost)}</td><td class="num">${fmtMs(row.duration_ms)}</td>
              <td>${row.status >= 400 || row.status === 0 ? `<span class="pill danger">${esc(row.status || "ERR")}</span>` : `<span class="pill signal">${esc(row.status)}</span>`}</td>
              <td><code>${esc(row.session || "—")}</code></td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="8"><div class="empty-line">没有请求明细。</div></td></tr>`;
    const pages = Math.max(1, Math.ceil(u.total / u.pageSize));
    $("#usage-page-info").textContent = `第 ${u.page} / ${pages} 页 · 共 ${num(u.total)} 条`;
    $("#usage-retention-hint").textContent = `${u.total ? `${num(u.total)} 条明细` : "0 条明细"} · 0 = 永久保留`;
  }

  function render() {
    renderKpis();
    renderMix();
    renderLatency();
    renderTrend();
    renderPie();
    renderUpstreamTable();
    renderSessionTable();
    renderModelTable();
    renderDetails();
  }

  function openDetail(id) {
    const row = u.rows.find((item) => String(item.id) === String(id));
    if (!row) return;
    if (typeof MG.openRequestDrawer === "function") {
      MG.openRequestDrawer(row);
      return;
    }
    openModal(
      "请求详情",
      `${row.upstream} · ${row.requested_model || row.upstream_model} · ${fmtTime(row.ts)}`,
      `<div class="kv">
        ${Object.entries(row)
          .map(([key, value]) => `<dt>${esc(key)}</dt><dd>${esc(typeof value === "object" ? JSON.stringify(value) : value ?? "—")}</dd>`)
          .join("")}
      </div>
      <div class="modal-actions"><button class="btn primary" id="detail-close">关闭</button></div>`
    );
    $("#detail-close").addEventListener("click", closeModal);
  }

  function exportUsage(format) {
    const params = query({ format, limit: 100000 });
    window.open(`/api/usage/export?${params}`, "_blank");
  }

  async function clearUsage() {
    if (!confirm("清空当前筛选范围的用量明细？该操作不可撤销。")) return;
    try {
      const data = await api("/api/usage/clear", { method: "POST", body: JSON.stringify({ range: u.range, upstream: u.upstream, model: u.model, session: u.session }) });
      toast(`已清空 ${data.removed || 0} 条`, "ok");
      await load();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function applyRetention() {
    const days = Number($("#usage-retention-days").value) || 0;
    if (!confirm(`按保留 ${days} 天清理？0 表示不删除。`)) return;
    try {
      const data = await api("/api/usage/retention/apply", { method: "POST", body: JSON.stringify({ days }) });
      toast(`删除 ${data.removed || 0} 条，数据库 ${data.after?.bytes ? `${(data.after.bytes / 1024 / 1024).toFixed(1)} MB` : "已整理"}`, "ok");
      await load();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  // 自动刷新只在页面可见且该视图处于激活态时触发，避免在别的页面里空转请求。
  function viewActive(view) {
    const el = $(`#view-${view}`);
    return Boolean(el && el.classList.contains("active")) && document.visibilityState !== "hidden";
  }

  function applyAutoRefresh() {
    clearInterval(u.autoTimer);
    const seconds = Number($("#usage-auto")?.value) || 0;
    u.autoTimer = seconds > 0
      ? setInterval(() => {
          if (viewActive("traffic")) load().catch(() => {});
        }, seconds * 1000)
      : null;
  }

  let lastLogText = null;

  async function loadLogs() {
    const lines = $("#runtime-lines").value;
    const level = $("#runtime-level").value;
    const q = $("#runtime-search").value.trim();
    const data = await api(`/api/logs?lines=${encodeURIComponent(lines)}&level=${encodeURIComponent(level)}&q=${encodeURIComponent(q)}`);
    const viewer = $("#runtime-viewer");
    const text = (data.lines || []).map((line) => line.text).join("\n") || "（无匹配日志）";
    // 自动刷新时只替换变化过的内容：内容没变就不动 DOM，避免滚动位置与选中文本被打断。
    if (text !== lastLogText) {
      const atBottom = viewer.scrollHeight - viewer.scrollTop - viewer.clientHeight < 24;
      viewer.textContent = text;
      lastLogText = text;
      if (atBottom) viewer.scrollTop = viewer.scrollHeight;
    }
    const size = data.size ? `${(data.size / 1024 / 1024).toFixed(1)} MB` : "—";
    const tail = data.tail ? ` · 尾部 ${Math.round(Number(data.windowBytes || 0) / 1024)} KB` : "";
    $("#runtime-meta").textContent = `${data.matched ?? data.lines?.length ?? 0} / ${data.total ?? 0} 行 · ${size}${tail}`;
  }

  // 日志自动刷新同理：只在该视图激活时轮询，离开页面立即停止。
  function applyLogAutoRefresh() {
    clearInterval(logs.timer);
    const seconds = Number($("#runtime-auto")?.value) || 0;
    logs.timer = seconds > 0
      ? setInterval(() => {
          if (viewActive("logs")) loadLogs().catch(() => {});
        }, seconds * 1000)
      : null;
  }

  function bind() {
    if (bound) return;
    bound = true;
    restoreScope();
    // 作用域控件改动即写回 localStorage，刷新页面后保持同一视角。
    $("#usage-scope")?.addEventListener("change", saveScope);
    $("#usage-only")?.addEventListener("change", saveScope);
    $("#usage-page-size")?.addEventListener("change", saveScope);
    $("#usage-trend-metric")?.addEventListener("change", saveScope);
    $("#usage-refresh").addEventListener("click", () => load().catch((error) => toast(error.message, "error")));
    $("#usage-range").addEventListener("change", () => {
      u.range = $("#usage-range").value;
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-upstream").addEventListener("change", () => {
      u.upstream = $("#usage-upstream").value;
      u.model = "";
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-model").addEventListener("change", () => {
      u.model = $("#usage-model").value;
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-session").addEventListener("change", () => {
      u.session = $("#usage-session").value;
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-only").addEventListener("change", () => {
      u.only = $("#usage-only").value;
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-currency").addEventListener("change", () => {
      u.currency = $("#usage-currency").value;
      render();
    });
    $("#usage-trend-metric").addEventListener("change", renderTrend);
    $("#usage-clear-filter").addEventListener("click", () => {
      u.upstream = "";
      u.model = "";
      u.session = "";
      u.only = "";
      u.page = 1;
      $("#usage-upstream").value = "";
      $("#usage-model").value = "";
      $("#usage-session").value = "";
      $("#usage-only").value = "";
      saveScope();
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-page-size").addEventListener("change", () => {
      u.pageSize = Number($("#usage-page-size").value) || 50;
      u.page = 1;
      load().catch((error) => toast(error.message, "error"));
    });
    $$("[data-usage-page]").forEach((button) =>
      button.addEventListener("click", () => {
        const pages = Math.max(1, Math.ceil(u.total / u.pageSize));
        if (button.dataset.usagePage === "first") u.page = 1;
        if (button.dataset.usagePage === "prev") u.page = Math.max(1, u.page - 1);
        if (button.dataset.usagePage === "next") u.page = Math.min(pages, u.page + 1);
        if (button.dataset.usagePage === "last") u.page = pages;
        load().catch((error) => toast(error.message, "error"));
      })
    );
    $("#usage-detail-table").addEventListener("click", (event) => {
      const row = event.target.closest("[data-usage-row]");
      if (row) openDetail(row.dataset.usageRow);
    });
    $("#usage-session-table").addEventListener("click", (event) => {
      const row = event.target.closest("[data-usage-session]");
      if (!row) return;
      u.session = row.dataset.usageSession;
      $("#usage-session").value = u.session;
      u.page = 1;
      saveScope();
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-export-csv").addEventListener("click", () => exportUsage("csv"));
    $("#usage-export-json").addEventListener("click", () => exportUsage("json"));
    $("#usage-clear").addEventListener("click", clearUsage);
    $("#usage-retention-apply").addEventListener("click", applyRetention);
    $("#usage-auto").addEventListener("change", applyAutoRefresh);

    $("#runtime-refresh").addEventListener("click", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-level").addEventListener("change", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-lines").addEventListener("change", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-search").addEventListener("input", () => {
      clearTimeout(logs.searchTimer);
      logs.searchTimer = setTimeout(() => loadLogs().catch(() => {}), 250);
    });
    $("#runtime-auto").addEventListener("change", applyLogAutoRefresh);
    $("#runtime-copy").addEventListener("click", async () => {
      await navigator.clipboard.writeText($("#runtime-viewer").textContent);
      toast("已复制当前日志", "ok");
    });
    $("#runtime-bottom").addEventListener("click", () => {
      const viewer = $("#runtime-viewer");
      viewer.scrollTop = viewer.scrollHeight;
    });
    $("#runtime-clear").addEventListener("click", async () => {
      if (!confirm("清空运行日志文件？不影响用量记录。")) return;
      try {
        await api("/api/logs/clear", { method: "POST", body: "{}" });
        await loadLogs();
        toast("运行日志已清空", "ok");
      } catch (error) {
        toast(error.message, "error");
      }
    });
  }

  MG.registerView("traffic", async () => {
    u.range = $("#usage-range").value || u.range;
    if (!u.autoTimer) applyAutoRefresh();
    await load();
  });
  MG.registerView("logs", async () => {
    if (!logs.timer) applyLogAutoRefresh();
    await loadLogs();
  });
  document.addEventListener("DOMContentLoaded", bind);
})();
