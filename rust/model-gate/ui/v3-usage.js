/* v3 用量分析、请求明细与运行日志。依赖 window.MG。 */
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
        api(`/api/usage/summary?${query({ group_by: "day" })}`),
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
      <div class="stat"><span class="k">请求</span><span class="v">${num(t.requests)}</span><span class="s">成功 ${pct(t.success_rate)}</span></div>
      <div class="stat signal"><span class="k">总 Token</span><span class="v">${compactNum(t.total_tokens)}</span><span class="s">输入 ${compactNum(t.prompt_tokens)}</span></div>
      <div class="stat"><span class="k">缓存命中</span><span class="v">${pct(t.cache_hit_rate)}</span><span class="s">${compactNum(t.cache_read_tokens)} tokens</span></div>
      <div class="stat amber"><span class="k">费用</span><span class="v">${money(t.cost)}</span><span class="s">${u.currency}</span></div>
      <div class="stat"><span class="k">均值 / P95</span><span class="v">${fmtMs(t.avg_duration_ms)}</span><span class="s">P95 ${fmtMs(t.p95_duration_ms)}</span></div>
      <div class="stat danger"><span class="k">错误</span><span class="v">${num(t.errors)}</span><span class="s">成功率 ${pct(t.success_rate)}</span></div>`;
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
    const rows = (u.summary || []).slice(-14);
    const max = Math.max(1, ...rows.map((row) => Number(row[metric] || 0)));
    $("#usage-trend").innerHTML = rows.length
      ? `<div class="bars">${rows
          .map(
            (row) => `<div class="bar-col" title="${esc(row.bucket)} · ${num(row[metric])}">
              <div class="bar" style="height:${Math.max(3, (Number(row[metric] || 0) / max) * 100)}%"></div>
              <span>${esc(String(row.bucket).slice(5))}</span>
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
              <td class="num">${num(row.requests)}</td><td class="num">${compactNum(row.cache_read_tokens)}</td><td class="num">${compactNum(row.total_tokens)}</td>
              <td class="num">${pct(row.success_rate)}</td><td class="num">${fmtMs(row.avg_duration_ms)}</td><td class="num">${fmtMs(row.p95_duration_ms)}</td><td class="num">${money(row.cost)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="10"><div class="empty-line">没有上游用量。</div></td></tr>`;
  }

  function renderSessionTable() {
    const tbody = $("#usage-session-table tbody");
    tbody.innerHTML = (u.sessions || []).length
      ? u.sessions
          .slice(0, 100)
          .map(
            (row) => `<tr class="clickable" data-usage-session="${esc(row.session || "")}">
              <td><code>${esc(row.session || "(无会话)")}</code></td><td class="num">${num(row.requests)}</td><td class="num">${num(row.errors)}</td>
              <td class="num">${compactNum(row.prompt_tokens)}</td><td class="num">${compactNum(row.completion_tokens)}</td><td class="num">${compactNum(row.cache_read_tokens)}</td>
              <td class="num">${compactNum(row.total_tokens)}</td><td class="num">${pct(row.success_rate)}</td><td class="num">${fmtMs(row.avg_duration_ms)}</td>
              <td class="num">${fmtMs(row.p95_duration_ms)}</td><td>${fmtTime(row.first_ts)} → ${fmtTime(row.last_ts)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="11"><div class="empty-line">没有会话记录。</div></td></tr>`;
  }

  function renderModelTable() {
    const tbody = $("#usage-model-table tbody");
    tbody.innerHTML = (u.models || []).length
      ? u.models
          .map(
            (row) => `<tr>
              <td><code>${esc(row.model)}</code></td><td class="num">${pct(row.cache_hit_rate)}</td><td class="num">${num(row.requests)}</td>
              <td class="num">${compactNum(row.prompt_tokens)}</td><td class="num">${compactNum(row.completion_tokens)}</td><td class="num">${compactNum(row.cache_read_tokens)}</td>
              <td class="num">${compactNum(row.total_tokens)}</td><td class="num">${money(row.cost)}</td><td class="num">${pct(row.success_rate)}</td>
              <td class="num">${fmtMs(row.avg_duration_ms)}</td><td class="num">${fmtMs(row.p95_duration_ms)}</td><td class="num">${fmtMs(row.avg_ttft_ms)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="12"><div class="empty-line">没有模型用量。</div></td></tr>`;
  }

  function renderDetails() {
    const tbody = $("#usage-detail-table tbody");
    tbody.innerHTML = u.rows.length
      ? u.rows
          .map(
            (row) => `<tr class="clickable" data-usage-row="${esc(row.id)}">
              <td>${fmtTime(row.ts)}</td><td><code>${esc(row.requested_model || row.upstream_model)}</code></td><td>${esc(row.upstream)}</td>
              <td class="num">${compactNum(row.prompt_tokens)} / ${compactNum(row.completion_tokens)}</td><td class="num">${compactNum(row.cache_read_tokens)}</td>
              <td class="num">${money(row.cost)}</td><td class="num">${fmtMs(row.duration_ms)}</td><td class="num">${fmtMs(row.ttft_ms)}</td>
              <td>${row.status >= 400 || row.status === 0 ? `<span class="pill danger">${esc(row.status || "ERR")}</span>` : `<span class="pill signal">${esc(row.status)}</span>`}</td>
              <td><code>${esc(row.session || "—")}</code></td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="10"><div class="empty-line">没有请求明细。</div></td></tr>`;
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

  function applyAutoRefresh() {
    clearInterval(u.autoTimer);
    const seconds = Number($("#usage-auto").value) || 0;
    if (seconds > 0) u.autoTimer = setInterval(() => load().catch(() => {}), seconds * 1000);
  }

  async function loadLogs() {
    const lines = $("#runtime-lines").value;
    const level = $("#runtime-level").value;
    const q = $("#runtime-search").value.trim();
    const data = await api(`/api/logs?lines=${encodeURIComponent(lines)}&level=${encodeURIComponent(level)}&q=${encodeURIComponent(q)}`);
    const viewer = $("#runtime-viewer");
    viewer.textContent = (data.lines || []).map((line) => line.text).join("\n") || "（无匹配日志）";
    viewer.scrollTop = viewer.scrollHeight;
    $("#runtime-meta").textContent = `${data.matched ?? data.lines?.length ?? 0} / ${data.total ?? 0} 行 · ${data.size ? `${(data.size / 1024 / 1024).toFixed(1)} MB` : "—"}`;
  }

  function bind() {
    if (bound) return;
    bound = true;
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
    $("#runtime-auto").addEventListener("change", () => {
      clearInterval(logs.timer);
      const seconds = Number($("#runtime-auto").value) || 0;
      if (seconds > 0) logs.timer = setInterval(() => loadLogs().catch(() => {}), seconds * 1000);
    });
    $("#runtime-copy").addEventListener("click", async () => {
      await navigator.clipboard.writeText($("#runtime-viewer").textContent);
      toast("已复制当前日志", "ok");
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
    await load();
  });
  MG.registerView("logs", loadLogs);
  document.addEventListener("DOMContentLoaded", bind);
})();
