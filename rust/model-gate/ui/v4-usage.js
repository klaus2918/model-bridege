// 用量页（N1 骨架）：时间范围 → KPI → 统计卡（按模型 / 按上游，点行过滤）→ 请求明细。
// 日常主视图：各种统计 + 基础检索（范围 / 上游 / 模型 / 状态快捷筛选），会话维度在会话页。
(function () {
  const MG = window.MG;
  const { $, $$, api, esc, num, compactNum, fmtTime, fmtMs, toast, state } = MG;

  const u = {
    range: "today",
    upstream: "",
    model: "",
    only: "",
    page: 1,
    pageSize: 50,
    total: 0,
    rows: [],
    models: [],
    upstreams: [],
    totals: null,
    autoTimer: null,
    loaded: false,
  };

  const SCOPE_KEY = "mg.v4.usage.scope";
  const RANGE_LABEL = { "1h": "近 1 小时", today: "今天", yesterday: "昨天", "7d": "近 7 天", "30d": "近 30 天", all: "全部" };

  function fmtCost(value) {
    const n = Number(value || 0);
    if (!n) return "—";
    if (Math.abs(n) < 0.01) return `¥${n.toFixed(4)}`;
    return `¥${n.toFixed(2)}`;
  }

  function query(extra = {}) {
    const params = new URLSearchParams({ range: u.range });
    if (u.upstream) params.set("upstream", u.upstream);
    if (u.model) params.set("model", u.model);
    if (u.only) params.set("only", u.only);
    for (const [key, value] of Object.entries(extra)) params.set(key, value);
    return params;
  }

  function saveScope() {
    try {
      localStorage.setItem(SCOPE_KEY, JSON.stringify({ range: u.range, upstream: u.upstream, model: u.model, only: u.only, pageSize: u.pageSize }));
    } catch {
      // 本地记忆失败不影响使用。
    }
  }

  function restoreScope() {
    try {
      const saved = JSON.parse(localStorage.getItem(SCOPE_KEY) || "{}");
      if (saved.range && RANGE_LABEL[saved.range]) u.range = saved.range;
      u.upstream = saved.upstream || "";
      u.model = saved.model || "";
      u.only = saved.only || "";
      u.pageSize = Number(saved.pageSize) || 50;
    } catch {
      // 忽略损坏的本地数据。
    }
  }

  function viewActive() {
    const el = $("#view-usage");
    return Boolean(el && el.classList.contains("active")) && document.visibilityState !== "hidden";
  }

  // ───────────────────────── 加载 ─────────────────────────

  async function load() {
    const [totals, models, upstreams] = await Promise.all([
      api(`/api/usage/totals?${query()}`),
      api(`/api/usage/models?${query({ limit: 8 })}`),
      api(`/api/usage/upstreams?${query({ limit: 8 })}`),
    ]);
    u.totals = totals.totals || totals || {};
    u.models = Array.isArray(models.rows) ? models.rows : [];
    u.upstreams = Array.isArray(upstreams.rows) ? upstreams.rows : [];
    renderKpis();
    renderStats();
    await loadList();
    u.loaded = true;
  }

  async function loadList() {
    const data = await api(`/api/usage/list?${query({ limit: u.pageSize, offset: (u.page - 1) * u.pageSize })}`);
    u.rows = Array.isArray(data.rows) ? data.rows : [];
    u.total = Number(data.total || 0);
    renderList();
  }

  // ───────────────────────── 渲染 ─────────────────────────

  function renderKpis() {
    const host = $("#usage-kpis");
    if (!host) return;
    const t = u.totals || {};
    const cells = [
      ["请求", num(t.requests)],
      ["Token", compactNum(t.total_tokens)],
      ["费用", fmtCost(t.cost), "a"],
      ["错误", num(t.errors), Number(t.errors) > 0 ? "bad" : ""],
      ["P95 耗时", fmtMs(t.p95_duration_ms)],
      ["缓存命中", `${Math.round(Number(t.cache_hit_rate || 0) * 100)}%`],
      ["成功率", `${Math.round(Number(t.success_rate || 0) * 100)}%`],
    ];
    host.innerHTML = cells
      .map(([label, value, tone]) => `<div><div class="kv ${tone || ""}">${esc(value)}</div><div class="kl">${esc(label)}</div></div>`)
      .join("");
  }

  function statRows(rows, kind) {
    const max = Math.max(...rows.map((row) => Number(row.requests || 0)), 1);
    return rows
      .map((row) => {
        const name = kind === "model" ? row.model : row.upstream;
        const active = (kind === "model" ? u.model : u.upstream) === name;
        const width = Math.max(3, Math.round((Number(row.requests || 0) / max) * 100));
        return `<button class="statrow${active ? " sel" : ""}" data-stat-${kind}="${esc(name || "")}">
          <span class="bn" title="${esc(name || "")}">${esc(name || "(unknown)")}</span>
          <span class="bt"><i style="width:${width}%"></i></span>
          <span class="bv">${num(row.requests)} · ${fmtCost(row.cost)}</span>
        </button>`;
      })
      .join("");
  }

  function renderStats() {
    const models = $("#usage-stat-models");
    const upstreams = $("#usage-stat-upstreams");
    if (models) models.innerHTML = u.models.length ? statRows(u.models, "model") : '<div class="empty-line">区间内没有请求</div>';
    if (upstreams) upstreams.innerHTML = u.upstreams.length ? statRows(u.upstreams, "upstream") : '<div class="empty-line">区间内没有请求</div>';
  }

  function statusPill(status) {
    const code = Number(status || 0);
    return `<span class="pill ${code >= 400 ? "danger" : "signal"}">${code || "—"}</span>`;
  }

  function renderList() {
    const tbody = $("#usage-detail-table tbody");
    if (!tbody) return;
    tbody.innerHTML = u.rows
      .map((row) => {
        const session = row.session || "";
        return `<tr data-usage-row="${esc(row.id)}">
          <td class="mono">${esc(fmtTime(row.ts))}</td>
          <td>${esc(row.requested_model || "—")}</td>
          <td>${esc(row.upstream || "—")}</td>
          <td class="num mono">${compactNum(row.prompt_tokens)} / ${compactNum(row.completion_tokens)}</td>
          <td class="num mono">${row.cache_read_tokens ? compactNum(row.cache_read_tokens) : "—"}</td>
          <td class="num mono">${fmtCost(row.cost)}</td>
          <td class="num mono">${fmtMs(row.duration_ms)}</td>
          <td class="num mono">${row.ttft_ms ? fmtMs(row.ttft_ms) : "—"}</td>
          <td>${statusPill(row.status)}</td>
          <td>${session ? `<button class="linklike" data-usage-session="${esc(session)}" title="在会话页打开">${esc(session.length > 18 ? `${session.slice(0, 17)}…` : session)}</button>` : '<span class="hint">无</span>'}</td>
        </tr>`;
      })
      .join("") || '<tr><td colspan="10"><div class="empty-line">没有匹配的请求</div></td></tr>';
    const pages = Math.max(1, Math.ceil(u.total / u.pageSize));
    const info = $("#usage-page-info");
    if (info) info.textContent = `${num(u.total)} 条 · 第 ${u.page} / ${pages} 页 · ${RANGE_LABEL[u.range] || u.range}`;
    const hint = $("#usage-list-hint");
    if (hint) {
      const filters = [u.upstream && `上游 ${u.upstream}`, u.model && `模型 ${u.model}`, u.only && `筛选 ${u.only}`].filter(Boolean).join(" · ");
      hint.textContent = filters ? `已过滤：${filters}` : "点行看详情；点会话列跳转会话页";
    }
  }

  // ───────────────────────── 动作 ─────────────────────────

  async function openDetail(id) {
    const row = u.rows.find((item) => String(item.id) === String(id));
    if (!row) return;
    if (typeof MG.openRequestDrawer === "function") MG.openRequestDrawer(row);
  }

  function exportUsage() {
    window.open(`/api/usage/export?${query({ format: "csv", limit: 100000 })}`);
  }

  async function applyRetention() {
    const days = Number($("#usage-retention-days").value) || 0;
    if (!confirm(`按保留 ${days} 天清理用量与请求数据？0 表示不删除。`)) return;
    try {
      const data = await api("/api/usage/retention/apply", { method: "POST", body: JSON.stringify({ days }) });
      toast(`删除 ${data.removed || 0} 条${data.after?.bytes ? `，数据库 ${(data.after.bytes / 1024 / 1024).toFixed(1)} MB` : ""}`, "ok");
      await load();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function applyAutoRefresh() {
    clearInterval(u.autoTimer);
    u.autoTimer = setInterval(() => {
      if (u.loaded && viewActive()) load().catch(() => {});
    }, 30000);
  }

  // ───────────────────────── 绑定 ─────────────────────────

  let bound = false;

  function syncControls() {
    $$("#usage-range [data-usage-range]").forEach((button) => button.classList.toggle("active", button.dataset.usageRange === u.range));
    const upstream = $("#usage-upstream");
    const model = $("#usage-model");
    if (upstream) upstream.value = u.upstream;
    if (model) model.value = u.model;
    const only = $("#usage-only");
    if (only) only.value = u.only;
    const size = $("#usage-page-size");
    if (size) size.value = String(u.pageSize);
  }

  function bind() {
    if (bound) return;
    bound = true;
    restoreScope();
    syncControls();

    $$("#usage-range [data-usage-range]").forEach((button) => {
      button.addEventListener("click", () => {
        u.range = button.dataset.usageRange;
        u.page = 1;
        saveScope();
        syncControls();
        load().catch((error) => toast(error.message, "error"));
      });
    });
    $("#usage-upstream")?.addEventListener("change", () => {
      u.upstream = $("#usage-upstream").value;
      u.page = 1;
      saveScope();
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-model")?.addEventListener("change", () => {
      u.model = $("#usage-model").value;
      u.page = 1;
      saveScope();
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-only")?.addEventListener("change", () => {
      u.only = $("#usage-only").value;
      u.page = 1;
      saveScope();
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#usage-page-size")?.addEventListener("change", () => {
      u.pageSize = Number($("#usage-page-size").value) || 50;
      u.page = 1;
      saveScope();
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#usage-refresh").addEventListener("click", () => load().catch((error) => toast(error.message, "error")));
    $("#usage-export").addEventListener("click", exportUsage);
    $("#usage-goto-sessions").addEventListener("click", () => MG.navigate("sessions"));

    $$("[data-usage-page]").forEach((button) =>
      button.addEventListener("click", () => {
        const pages = Math.max(1, Math.ceil(u.total / u.pageSize));
        if (button.dataset.usagePage === "first") u.page = 1;
        if (button.dataset.usagePage === "prev") u.page = Math.max(1, u.page - 1);
        if (button.dataset.usagePage === "next") u.page = Math.min(pages, u.page + 1);
        if (button.dataset.usagePage === "last") u.page = pages;
        loadList().catch((error) => toast(error.message, "error"));
      })
    );

    $("#usage-detail-table").addEventListener("click", (event) => {
      const sessionButton = event.target.closest("[data-usage-session]");
      if (sessionButton) {
        event.stopPropagation();
        MG.focusPromptSession?.(sessionButton.dataset.usageSession || "");
        return;
      }
      const row = event.target.closest("[data-usage-row]");
      if (row) openDetail(row.dataset.usageRow);
    });

    // 统计卡：点行 = 设置该维度过滤并整页重查；再点一次取消。
    $("#usage-stat-models")?.addEventListener("click", (event) => {
      const button = event.target.closest("[data-stat-model]");
      if (!button) return;
      u.model = u.model === button.dataset.statModel ? "" : button.dataset.statModel;
      u.page = 1;
      saveScope();
      syncControls();
      load().catch((error) => toast(error.message, "error"));
    });
    $("#usage-stat-upstreams")?.addEventListener("click", (event) => {
      const button = event.target.closest("[data-stat-upstream]");
      if (!button) return;
      u.upstream = u.upstream === button.dataset.statUpstream ? "" : button.dataset.statUpstream;
      u.page = 1;
      saveScope();
      syncControls();
      load().catch((error) => toast(error.message, "error"));
    });

    // 数据清理按钮位于「设置 → 维护」，同属本模块职责。
    $("#usage-retention-apply").addEventListener("click", applyRetention);

    applyAutoRefresh();
  }

  function fillFacets() {
    const upstream = $("#usage-upstream");
    const model = $("#usage-model");
    if (!upstream || !model) return;
    const keep = [upstream.value, model.value];
    const upstreamNames = [...new Set(u.upstreams.map((row) => row.upstream).filter(Boolean))];
    const modelNames = [...new Set(u.models.map((row) => row.model).filter(Boolean))];
    upstream.innerHTML = '<option value="">全部上游</option>' + upstreamNames.map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("");
    model.innerHTML = '<option value="">全部模型</option>' + modelNames.map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("");
    upstream.value = keep[0] && (upstreamNames.includes(keep[0]) || u.upstream === keep[0]) ? keep[0] : "";
    model.value = keep[1] && (modelNames.includes(keep[1]) || u.model === keep[1]) ? keep[1] : "";
  }

  const origLoad = load;
  load = async function () {
    await origLoad();
    fillFacets();
    syncControls();
  };

  MG.registerView("usage", async () => {
    if (!bound) bind();
    syncControls();
    await load();
  });

  document.addEventListener("DOMContentLoaded", bind);
})();
