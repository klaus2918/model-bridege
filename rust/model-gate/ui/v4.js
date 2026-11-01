/* Model-Gate v4 控制台交互层。
   设计依据：docs/design/refactor-2026/09-console-v4-design.md
   约束：复用 v4 内核的 window.MG 数据层与页面渲染；本文件只负责 v4 外壳、总览、
   配置中心、命令面板、日志诊断与设置分栏，不改变后端契约。 */
(() => {
  "use strict";

  const MG = window.MG;
  if (!MG) return;

  const { $, $$, esc, num, compactNum, fmtTime, fmtMs, api, toast, state } = MG;

  // ───────────────────────── 信息架构 ─────────────────────────

  const TOP = {
    overview: { title: "总览", tag: "OVERVIEW" },
    traffic: { title: "用量分析", tag: "USAGE" },
    prompts: { title: "提示词", tag: "PROMPTS" },
    access: { title: "接入管理", tag: "ACCESS" },
    logs: { title: "日志与诊断", tag: "OBSERVE" },
    config: { title: "配置中心", tag: "CONFIG" },
  };

  const CONFIG = {
    models: { group: "模型与路由", title: "模型目录", tag: "CATALOG", view: "models", tab: "catalog" },
    pricing: { group: "模型与路由", title: "单价表", tag: "PRICING", view: "models", tab: "pricing" },
    routes: { group: "模型与路由", title: "统一入口", tag: "ENTRY", view: "routes" },
    groups: { group: "模型与路由", title: "路由组", tag: "GROUPS", view: "routes" },
    pools: { group: "凭据池", title: "凭据池", tag: "POOLS", view: "credentials" },
    zcode: { group: "凭据池", title: "ZCode 账号", tag: "ZCODE", view: "zcode" },
    plugins: { group: "资源库", title: "插件", tag: "PLUGINS", view: "plugins" },
    free: { group: "资源库", title: "免费来源", tag: "FREE", view: "free" },
    templates: { group: "资源库", title: "模板", tag: "TEMPLATES", view: "templates" },
    settings: { group: "设置", title: "设置", tag: "SETTINGS", view: "settings", section: "service" },
    advanced: { group: "设置", title: "高级维护", tag: "ADVANCED", view: "settings", section: "advanced" },
  };

  const CONFIG_GROUPS = [
    { label: "模型与路由", items: ["models", "pricing", "routes", "groups"] },
    { label: "凭据池", items: ["pools", "zcode"] },
    { label: "资源库", items: ["plugins", "free", "templates"] },
    { label: "设置", items: ["settings", "advanced"] },
  ];

  const LEGACY_TO_CONFIG = {
    models: "models",
    routes: "routes",
    credentials: "pools",
    plugins: "plugins",
    free: "free",
    zcode: "zcode",
    settings: "settings",
  };

  const SETTINGS_SECTIONS = [
    { id: "service", label: "服务与全局", hint: "监听、模型、保留" },
    { id: "privacy", label: "隐私与记录", hint: "提示词与 AI 接口" },
    { id: "routes", label: "统一入口", hint: "可见性、分组、路由组" },
    { id: "templates", label: "模板", hint: "官方与自定义默认值" },
    { id: "advanced", label: "高级维护", hint: "备份、校验、更新" },
  ];

  const v4 = {
    page: "overview",
    sub: "",
    booted: false,
    hub: null,
    hubLoading: false,
    overview: null,
    overviewLoading: false,
    ovRange: 1,
    ovMetric: "requests",
    observe: "logs",
    cmdkIndex: 0,
    cmdkItems: [],
    settingsSection: "service",
    gPending: 0,
  };

  // ───────────────────────── 工具 ─────────────────────────

  function pct(value) {
    if (value == null || Number.isNaN(Number(value))) return "—";
    return `${(Number(value) * 100).toFixed(1)}%`;
  }

  function money(value) {
    const n = Number(value || 0);
    if (!n) return "¥0";
    if (n < 0.01) return `¥${n.toFixed(4)}`;
    return `¥${n.toFixed(2)}`;
  }

  function deltaText(now, prev) {
    const a = Number(now || 0);
    const b = Number(prev || 0);
    if (!b) return { text: "较上一周期 —", cls: "" };
    const diff = ((a - b) / b) * 100;
    const sign = diff >= 0 ? "+" : "";
    return {
      text: `较上一周期 ${sign}${diff.toFixed(1)}%`,
      cls: diff > 0 ? "up" : diff < 0 ? "warn" : "",
    };
  }

  function relTime(value) {
    if (!value) return "—";
    const date = typeof value === "number" ? new Date(value * 1000) : new Date(value);
    if (Number.isNaN(date.getTime())) return String(value);
    const diff = Date.now() - date.getTime();
    if (diff < 60_000) return "刚刚";
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
    return date.toLocaleString("zh-CN", { hour12: false });
  }

  function setHTML(selector, html) {
    const el = $(selector);
    if (el) el.innerHTML = html;
    return el;
  }

  function show(el, visible) {
    if (el) el.hidden = !visible;
  }

  function isTyping(target) {
    if (!target || !target.tagName) return false;
    const tag = target.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
  }

  function overlayOpen() {
    return Boolean(
      document.querySelector("#v4-cmdk:not([hidden])") ||
      document.querySelector(".modal:not([hidden])") ||
      (document.querySelector("#drawer") && !document.querySelector("#drawer").hidden) ||
      (document.querySelector("#request-drawer") && !document.querySelector("#request-drawer").hidden)
    );
  }

  function retry(fn, timeout = 900) {
    const started = Date.now();
    const tick = () => {
      try {
        if (fn()) return;
      } catch {
        /* 继续等待 */
      }
      if (Date.now() - started < timeout) setTimeout(tick, 100);
    };
    tick();
  }

  function healthDot(healthState) {
    const k = healthState === "ok" ? "ok" : healthState === "error" ? "error" : "warn";
    return `<span class="dot ${k}"></span>`;
  }

  function issuesFromState() {
    const issues = [];
    for (const item of state.access || []) {
      if (item.health && item.health.state && item.health.state !== "ok") {
        issues.push({
          level: item.health.state === "error" ? "error" : "warn",
          title: `${item.label || item.id} 需要处理`,
          detail: item.health.message || "健康检查未通过，请打开接入详情查看诊断。",
          action: { page: "access", id: item.id },
        });
      }
      if (Array.isArray(item.diagnostics)) {
        for (const diag of item.diagnostics) {
          if (diag.level === "error" || diag.level === "warn") {
            issues.push({
              level: diag.level,
              title: `${item.label || item.id} · ${diag.code || "诊断"}`,
              detail: diag.message || "",
              action: { page: "access", id: item.id },
            });
          }
        }
      }
      if (item.pool && Number(item.pool.cooling || 0) > 0) {
        issues.push({
          level: "warn",
          title: `${item.label || item.id} 有成员冷却`,
          detail: `${item.pool.cooling} 个成员冷却中，可用 ${item.pool.available}/${item.pool.members}`,
          action: { page: "config", sub: "pools" },
        });
      }
    }
    for (const pool of state.pools || []) {
      if (Number(pool.exhausted || 0) > 0) {
        issues.push({
          level: "warn",
          title: `凭据池 ${pool.id} 配额耗尽`,
          detail: `${pool.exhausted} 个成员耗尽，请补充或调整配额。`,
          action: { page: "config", sub: "pools" },
        });
      }
    }
    const seen = new Set();
    return issues.filter((issue) => {
      const key = `${issue.level}|${issue.title}|${issue.detail}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  }

  // ───────────────────────── 路由 ─────────────────────────

  function parseLocation() {
    const url = new URL(location.href);
    let view = url.searchParams.get("view") || "";
    let sub = url.searchParams.get("sub") || "";
    const hash = (url.hash || "").replace(/^#\/?/, "");
    if (hash) {
      const parts = hash.split("/").filter(Boolean);
      if (parts[0] === "config") {
        view = "config";
        sub = parts[1] || "";
      } else if (parts[0]) {
        view = parts[0];
      }
    }
    if (view === "config") return { page: "config", sub: CONFIG[sub] ? sub : "" };
    if (LEGACY_TO_CONFIG[view]) return { page: "config", sub: LEGACY_TO_CONFIG[view] };
    if (TOP[view]) return { page: view, sub: "" };
    return { page: "overview", sub: "" };
  }

  function updateUrl(replace) {
    const url = new URL(location.href);
    url.hash = "";
    url.searchParams.delete("view");
    url.searchParams.delete("sub");
    if (v4.page === "config") {
      url.searchParams.set("view", "config");
      if (v4.sub) url.searchParams.set("sub", v4.sub);
    } else if (v4.page !== "overview") {
      url.searchParams.set("view", v4.page);
    }
    const method = replace ? "replaceState" : "pushState";
    history[method](null, "", url);
  }

  function renderConfigSubnav() {
    const host = $("#config-subnav");
    if (!host) return;
    host.innerHTML = CONFIG_GROUPS.map((group) => {
      const links = group.items
        .map((id) => {
          const item = CONFIG[id];
          const active = v4.page === "config" && v4.sub === id ? " active" : "";
          return `<button class="cs-link${active}" data-config-sub="${id}" title="${esc(item.title)}">${esc(item.title)}</button>`;
        })
        .join("");
      return `<span class="cs-group">${esc(group.label)}</span>${links}`;
    }).join("");
  }

  function applyChrome() {
    const configSub = v4.page === "config" && v4.sub ? CONFIG[v4.sub] : null;
    const pageInfo = configSub || TOP[v4.page] || TOP.overview;
    const title = pageInfo.title;
    const tag = pageInfo.tag;

    const pageTitle = $("#page-title");
    if (pageTitle) pageTitle.textContent = title;
    const pageTag = $("#page-tag");
    if (pageTag) pageTag.textContent = tag;

    const crumb = $("#page-crumb");
    if (crumb) {
      if (configSub) {
        crumb.innerHTML = `<span class="crumb-root">MODEL-GATE</span><span class="crumb-sep">/</span><span class="crumb-mid">配置中心</span><span class="crumb-sep">/</span><b id="page-title">${esc(title)}</b><span class="page-tag" id="page-tag">${esc(tag)}</span>`;
      } else {
        crumb.innerHTML = `<span class="crumb-root">MODEL-GATE</span><span class="crumb-sep">/</span><b id="page-title">${esc(title)}</b><span class="page-tag" id="page-tag">${esc(tag)}</span>`;
      }
    }

    $$(".nav-item").forEach((button) => {
      const active = v4.page === "config" ? button.dataset.view === "config" : button.dataset.view === v4.page;
      button.classList.toggle("active", active);
    });

    const subbar = $("#config-subbar");
    if (subbar) subbar.hidden = v4.page !== "config";
    renderConfigSubnav();

    const rev = $("#config-sub-rev");
    if (rev) {
      const info = state.info || {};
      rev.textContent = info.version ? `v${info.version} · bridge.config.json` : "bridge.config.json";
    }

    document.body.dataset.page = v4.page;
  }

  function applyConfigTarget() {
    const sub = v4.page === "config" ? v4.sub : "";
    if (!sub) return;
    const item = CONFIG[sub];
    if (!item) return;

    if (item.tab) {
      retry(() => {
        const button = $(`#model-tabs [data-model-tab="${item.tab}"]`);
        if (!button) return false;
        if (!button.classList.contains("active")) button.click();
        return true;
      });
      const label = $(`#model-tabs [data-model-tab="catalog"]`);
      if (label && label.textContent.trim() === "模型清单") label.textContent = "模型目录";
    }

    if (item.section) {
      applySettingsSection(item.section);
    }

    if (sub === "routes") {
      retry(() => {
        const cards = $$("#view-routes .card");
        if (!cards.length) return false;
        cards[0].scrollIntoView({ block: "start", behavior: "smooth" });
        return true;
      });
    }
    if (sub === "groups") {
      retry(() => {
        const cards = $$("#view-routes .card");
        if (cards.length < 2) return false;
        cards[1].scrollIntoView({ block: "start", behavior: "smooth" });
        return true;
      });
    }
  }

  function go(page, sub, options = {}) {
    if (page === "config" && sub && !CONFIG[sub]) sub = "";
    const viewId = page === "config" ? (sub ? CONFIG[sub].view : "config") : page;
    if (!viewId) return;

    v4.page = page;
    v4.sub = sub || "";
    applyChrome();
    MG.navigate(viewId);
    applyChrome();
    applyConfigTarget();
    updateUrl(options.replace === true);

    if (page === "overview") loadOverview();
    if (page === "logs" && v4.observe === "diag") loadDiagnostics();
    if (page === "access") {
      enhanceAccess();
      setTimeout(enhanceAccessTable, 60);
    }
    if (page === "traffic") setTimeout(enhanceUsage, 60);
    if (page === "prompts") setTimeout(enhancePrompts, 60);
    if (page === "settings") buildSettingsLayout();
  }

  // ───────────────────────── 总览 ─────────────────────────

  async function loadOverview() {
    if (v4.overviewLoading) return;
    v4.overviewLoading = true;
    try {
      const range = v4.ovRange;
      const rangeParam = range === 1 ? "1" : `${range}d`;
      const groupBy = range === 1 ? "hour" : "day";
      const requests = [
        MG.refreshAccess().catch(() => null),
        MG.refreshPools().catch(() => null),
        api(`/api/usage/totals?range=${rangeParam}&compare=1`).catch(() => ({})),
        api(`/api/usage/summary?range=${rangeParam}&group_by=${groupBy}`).catch(() => ({ rows: [] })),
        api(`/api/usage/list?limit=6`).catch(() => ({ rows: [] })),
        api(`/api/usage/upstreams?range=${rangeParam}`).catch(() => ({ rows: [] })),
        api("/api/proxy/status").catch(() => ({})),
        api("/api/prompts?limit=1").catch(() => ({})),
        api("/api/ai/audit").catch(() => ({ items: [] })),
      ];
      const [, , totals, summary, recent, upstreams, proxy, prompts, audit] = await Promise.all(requests);
      v4.overview = {
        totals: totals.totals || {},
        previous: totals.previous || null,
        summary: Array.isArray(summary.rows) ? summary.rows : [],
        recent: Array.isArray(recent.rows) ? recent.rows : [],
        upstreams: Array.isArray(upstreams.rows) ? upstreams.rows : [],
        proxy: proxy || {},
        prompts: prompts || {},
        audit: Array.isArray(audit.items) ? audit.items : [],
      };
      renderOverview();
    } finally {
      v4.overviewLoading = false;
    }
  }

  function sparkline(values, width = 92, height = 26) {
    const list = values.map((v) => Number(v || 0));
    if (!list.length) return "";
    const max = Math.max(1, ...list);
    const step = list.length > 1 ? width / (list.length - 1) : width;
    const points = list
      .map((value, index) => `${(index * step).toFixed(1)},${(height - (value / max) * (height - 4) - 2).toFixed(1)}`)
      .join(" ");
    return `<svg class="spark" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" aria-hidden="true"><polyline points="${points}" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
  }

  function renderOverview() {
    const data = v4.overview;
    if (!data) return;
    const access = state.access || [];
    const totals = data.totals || {};
    const previous = data.previous || {};
    const issues = issuesFromState();
    const okCount = access.filter((item) => item.health && item.health.state === "ok").length;
    const warningCount = access.length - okCount;

    const hero = $("#ov-hero");
    if (hero) {
      if (!access.length) hero.textContent = "先接入一个上游。";
      else if (issues.length) hero.textContent = `有 ${issues.length} 项需要处理。`;
      else hero.textContent = "今天，网关运行良好。";
    }
    const sub = $("#ov-sub");
    if (sub) {
      sub.textContent = access.length
        ? `${okCount}/${access.length} 个接入健康 · 今日 ${num(totals.requests)} 次请求 · 成功率 ${pct(totals.success_rate)}`
        : "添加第一个接入后，这里会给出健康、用量与下一步。";
    }

    const info = state.info || {};
    const status = setHTML(
      "#ov-status",
      `<span class="live">${healthDot(warningCount ? "warn" : "ok")}${warningCount ? "部分接入需要注意" : access.length ? "网关运行正常" : "等待接入"}</span>
       <span class="sep-v"></span>
       <div class="meta">
         <span>监听 <b>${esc(info.host || "127.0.0.1")}:${esc(info.port || 8900)}</b></span>
         <span>版本 <b>v${esc(info.version || "—")}</b></span>
         <span>运行 <b>${uptimeText(data.proxy.uptime_seconds)}</b></span>
         <span>接入 <b>${access.length}</b></span>
         <span>池成员可用 <b>${(state.pools || []).reduce((sum, pool) => sum + Number(pool.available || 0), 0)}</b></span>
       </div>
       <span class="band-right">${new Date().toLocaleTimeString("zh-CN", { hour12: false })} · 自动刷新</span>`
    );
    if (status) status.dataset.tone = warningCount ? "warn" : "ok";

    const summaryRows = data.summary || [];
    const reqSeries = summaryRows.map((row) => row.requests);
    const tokenSeries = summaryRows.map((row) => row.total_tokens);
    const costSeries = summaryRows.map((row) => row.cost);
    const deltaRequests = deltaText(totals.requests, previous.requests);
    const deltaTokens = deltaText(totals.total_tokens, previous.total_tokens);
    const deltaCost = deltaText(totals.cost, previous.cost);

    setHTML(
      "#ov-kpis",
      `<div class="ov-kpi clickable" data-v4-go="traffic">
         <div class="k">今日请求</div><span class="corner">REQ</span>
         <div class="v">${compactNum(totals.requests)}</div>
         <div class="s ${deltaRequests.cls}">${esc(deltaRequests.text)}</div>
         ${sparkline(reqSeries)}
       </div>
       <div class="ov-kpi clickable" data-v4-go="traffic">
         <div class="k">成功率</div><span class="corner">OK</span>
         <div class="v">${pct(totals.success_rate)}</div>
         <div class="s ${Number(totals.errors || 0) ? "warn" : "up"}">${num(totals.errors)} 次失败 · P95 ${fmtMs(totals.p95_duration_ms)}</div>
         ${sparkline(summaryRows.map((row) => row.requests - row.errors))}
       </div>
       <div class="ov-kpi clickable" data-v4-go="traffic">
         <div class="k">Token</div><span class="corner">TOK</span>
         <div class="v">${compactNum(totals.total_tokens)}</div>
         <div class="s ${deltaTokens.cls}">${esc(deltaTokens.text)}</div>
         ${sparkline(tokenSeries)}
       </div>
       <div class="ov-kpi clickable" data-v4-go="traffic">
         <div class="k">估算费用</div><span class="corner">COST</span>
         <div class="v">${money(totals.cost)}</div>
         <div class="s ${deltaCost.cls}">${esc(deltaCost.text)} · 汇率 ${Number((data.totals && data.totals.fx) || 7.2).toFixed(2)}</div>
         ${sparkline(costSeries)}
       </div>`
    );

    renderOverviewChart();
    renderOverviewHealth();
    renderOverviewIssues(issues);
    renderOverviewRecent();
    renderOverviewSnapshot(totals, previous);
    renderOverviewPrompts();

    const quickstart = $("#ov-quickstart");
    if (quickstart) quickstart.hidden = access.length > 0;
  }

  function uptimeText(seconds) {
    const n = Number(seconds || 0);
    if (!n) return "—";
    const days = Math.floor(n / 86400);
    const hours = Math.floor((n % 86400) / 3600);
    const mins = Math.floor((n % 3600) / 60);
    if (days) return `${days} 天 ${hours} 小时`;
    if (hours) return `${hours} 小时 ${mins} 分`;
    return `${mins} 分`;
  }

  function renderOverviewChart() {
    const host = $("#ov-chart");
    if (!host) return;
    const rows = (v4.overview?.summary || []).slice(-24);
    const metric = v4.ovMetric;
    const field = metric === "tokens" ? "total_tokens" : metric === "errors" ? "errors" : "requests";
    if (!rows.length) {
      host.innerHTML = `<div class="empty-line">当前范围没有用量数据。</div>`;
      return;
    }
    const max = Math.max(1, ...rows.map((row) => Number(row[field] || 0)));
    // 刻度密度按可用宽度计算：窄屏少显示几个，避免标签互相压住。
    const hostWidth = host.clientWidth || 900;
    const maxLabels = Math.max(3, Math.floor(hostWidth / 64));
    const every = Math.max(1, Math.ceil(rows.length / maxLabels));
    // 末尾刻度总要显示；若它与上一个刻度挨得太近，就把上一个让出来。
    const showAt = new Set();
    for (let index = 0; index < rows.length; index += every) showAt.add(index);
    const last = rows.length - 1;
    const shown = [...showAt].sort((a, b) => a - b);
    const prev = shown[shown.length - 1];
    if (last - prev < Math.max(2, every - 1)) showAt.delete(prev);
    showAt.add(last);
    const labelOf = (bucket) => {
      const text = String(bucket || "");
      // 小时桶形如 2026-11-01T15（没有分钟），渲染成 15:00 更符合排障习惯。
      const t = text.indexOf("T");
      if (t > 0) return `${text.slice(t + 1, t + 3)}:00`;
      return text.slice(5);
    };
    host.innerHTML = `<div class="ov-bars">${rows
      .map((row, index) => {
        const value = Number(row[field] || 0);
        const label = metric === "tokens" ? compactNum(value) : metric === "errors" ? num(value) : compactNum(value);
        return `<div class="ov-bar-col" title="${esc(row.bucket)} · ${label}">
          <div class="ov-bar ${metric}" style="height:${Math.max(3, (value / max) * 100)}%"></div>
          <span>${showAt.has(index) ? esc(labelOf(row.bucket)) : ""}</span>
        </div>`;
      })
      .join("")}</div>`;
  }

  function renderOverviewHealth() {
    const host = $("#ov-health");
    if (!host) return;
    const rows = (state.access || [])
      .slice()
      .sort((a, b) => (a.health?.state === "ok" ? 1 : 0) - (b.health?.state === "ok" ? 1 : 0));
    if (!rows.length) {
      host.innerHTML = `<div class="empty-line">还没有接入。点击「添加接入」开始。</div>`;
      return;
    }
    const usage = new Map((v4.overview?.upstreams || []).map((row) => [row.upstream, row]));
    host.innerHTML = rows
      .map((item) => {
        const stat = usage.get(item.id) || {};
        const credential =
          item.auth?.mode === "pool"
            ? `池 ${item.pool?.available ?? 0}/${item.pool?.members ?? 0}`
            : item.auth?.mode === "none"
              ? "无需"
              : item.auth?.configured
                ? "单 Key"
                : "待补";
        const health = item.health?.state === "ok" ? "正常" : item.health?.state === "error" ? "异常" : "待关注";
        return `<button class="health-row" data-v4-access="${esc(item.id)}">
          ${healthDot(item.health?.state)}
          <span class="nm">${esc(item.label || item.id)} <span class="pill">${esc((MG.KIND_LABEL || {})[item.kind] || item.kind || "自定义")}</span></span>
          <span class="tiny muted">${esc(item.protocol || "")}</span>
          <span class="num">${compactNum(stat.requests ?? item.usage?.todayRequests)}<small>${stat.success_rate != null ? pct(stat.success_rate) : "今日"}</small></span>
          <span class="health ${item.health?.state === "ok" ? "" : "warn"}">${health}</span>
          <span class="num">${fmtMs(stat.avg_duration_ms ?? item.health?.latencyMs)}</span>
          <span class="chev">›</span>
        </button>`;
      })
      .join("");
  }

  function renderOverviewIssues(issues) {
    const count = $("#todo-count");
    if (count) count.textContent = String(issues.length);
    const host = $("#ov-issues");
    if (!host) return;
    if (!issues.length) {
      host.innerHTML = `<div class="empty-line">当前没有阻断项。</div>`;
      return;
    }
    host.innerHTML = issues
      .slice(0, 5)
      .map((issue) => {
        const level = issue.level === "error" ? "" : issue.level === "info" ? "info" : "warn";
        const action = issue.action
          ? `<button class="btn ghost sm" data-v4-issue='${esc(JSON.stringify(issue.action))}'>处理</button>`
          : "";
        return `<div class="issue-row ${level}">
          <span class="mark">${issue.level === "error" ? "!" : "?"}</span>
          <div class="grow"><div class="nm">${esc(issue.title)}</div><div class="sub">${esc(issue.detail)}</div></div>
          ${action}
        </div>`;
      })
      .join("");
  }

  function renderOverviewRecent() {
    const host = $("#ov-recent");
    if (!host) return;
    const rows = v4.overview?.recent || [];
    if (!rows.length) {
      host.innerHTML = `<div class="empty-line">还没有请求记录。</div>`;
      return;
    }
    host.innerHTML = rows
      .map((row) => {
        const ok = Number(row.status || 200) < 400;
        return `<div class="mini-row">
          <span class="status-code ${ok ? "" : "err"}">${esc(row.status || "—")}</span>
          <div class="grow"><div class="nm">${esc(row.requested_model || row.model || "—")}</div><div class="sub">${esc(row.upstream || "—")} · ${fmtTime(row.ts)}</div></div>
          <span class="num">${fmtMs(row.duration_ms)}<br>${compactNum(row.total_tokens)} tok</span>
        </div>`;
      })
      .join("");
  }

  function renderOverviewSnapshot(totals, previous) {
    const host = $("#ov-snapshot");
    if (!host) return;
    const delta = deltaText(totals.total_tokens, previous.total_tokens);
    host.innerHTML = `<div class="snap-main">
        <div><div class="snap-label">TOKENS / 今日</div><div class="snap-token">${compactNum(totals.total_tokens)}</div></div>
        <div class="snap-cost"><div class="snap-label">COST</div><b>${money(totals.cost)}</b></div>
      </div>
      <div class="s ${delta.cls}" style="margin-top:8px;color:var(--faint);font-size:10px">${esc(delta.text)}</div>`;
  }

  function renderOverviewPrompts() {
    const pill = $("#ov-prompt-pill");
    const host = $("#ov-prompts");
    const prompts = v4.overview?.prompts || {};
    const capture = Boolean(prompts.capture || state.config?.promptCapture);
    if (pill) {
      pill.textContent = capture ? "记录中" : "未开启";
      pill.className = `pill ${capture ? "signal" : "amber"}`;
    }
    if (!host) return;
    if (!capture) {
      host.innerHTML = `<div class="hint" style="line-height:1.65">提示词记录未开启。开启后只记录入站 messages，不记录模型响应，并对常见密钥脱敏。</div>`;
      return;
    }
    const stats = prompts.stats || {};
    host.innerHTML = `<div class="prompt-line"><span class="big">${compactNum(prompts.total)}</span><span class="hint">条记录</span></div>
      <div class="hint" style="margin-top:6px">占用 ${(Number(stats.size_bytes || 0) / 1024 / 1024).toFixed(1)} MB · 最近 ${prompts.rows?.[0] ? relTime(prompts.rows[0].ts) : "—"}</div>`;
  }

  // ───────────────────────── 配置中心 ─────────────────────────

  async function loadConfigHub() {
    if (v4.hubLoading) return;
    v4.hubLoading = true;
    try {
      const results = await Promise.all([
        MG.refreshAccess().catch(() => null),
        MG.refreshPools().catch(() => null),
        MG.refreshPlugins().catch(() => null),
        MG.refreshConfig().catch(() => null),
        MG.refreshTemplates().catch(() => null),
        api("/api/pricing").catch(() => ({})),
        api("/api/models/catalog?limit=500").catch(() => ({ rows: [] })),
        api("/api/ai/audit").catch(() => ({ items: [] })),
        api("/api/free-sources").catch(() => ({})),
        api("/api/ai/settings").catch(() => ({})),
        api("/api/config/backups").catch(() => ({})),
      ]);
      const [, , , , , pricing, catalog, audit, free, ai, backups] = results;
      v4.hub = {
        pricing: pricing || {},
        catalog: catalog || {},
        audit: audit || {},
        free: free || {},
        ai: ai.settings || ai || {},
        backups: backups || {},
      };
      renderConfigHub();
    } finally {
      v4.hubLoading = false;
    }
  }

  function countGroups() {
    const groups = state.config?.groups;
    if (Array.isArray(groups)) return groups.length;
    if (groups && typeof groups === "object") return Object.keys(groups).length;
    return 0;
  }

  function countFreeModels() {
    const free = v4.hub?.free || {};
    const rows = Array.isArray(free.sources) ? free.sources : Array.isArray(free.items) ? free.items : [];
    return rows.reduce((sum, row) => {
      const models = row.models;
      if (Array.isArray(models)) return sum + models.filter((m) => m && (m.available !== false && m.status !== "unavailable")).length;
      return sum + Number(row.available || 0);
    }, 0);
  }

  function configDomains() {
    const access = state.access || [];
    const catalogRows = Array.isArray(v4.hub?.catalog?.rows) ? v4.hub.catalog.rows : [];
    const priced = catalogRows.filter((row) => row.costHint && !Array.isArray(row.costHint) ? true : Array.isArray(row.costHint) ? row.costHint.length > 0 : Boolean(row.costHint)).length;
    const unpriced = Math.max(0, catalogRows.length - priced);
    const pools = state.pools || [];
    const poolMembers = pools.reduce((sum, pool) => sum + Number(pool.members || 0), 0);
    const cooling = pools.reduce((sum, pool) => sum + Number(pool.cooling || 0), 0);
    const exhausted = pools.reduce((sum, pool) => sum + Number(pool.exhausted || 0), 0);
    const plugins = state.plugins || [];
    const unattached = plugins.filter((plugin) => !Array.isArray(plugin.attached) || plugin.attached.length === 0).length;
    const pricingEntries = Number(v4.hub?.pricing?.entries || 0);
    const capture = Boolean(state.config?.promptCapture);
    const ai = v4.hub?.ai || {};

    return [
      {
        id: "models",
        no: "01",
        tone: "signal",
        title: "模型与路由",
        desc: "模型目录、单价表、统一入口与路由组",
        state: unpriced ? `${unpriced} 个未定价` : "定价完整",
        pillCls: unpriced ? "amber" : "signal",
        metrics: [
          ["模型", compactNum(catalogRows.length)],
          ["已定价 / 未定价", `${priced} / ${unpriced}`],
          ["路由组", countGroups()],
          ["统一入口", state.config?.unified?.visibleUpstreams?.length ? "白名单" : "全部可见"],
        ],
        hint: unpriced ? `优先补齐 ${unpriced} 个未定价模型` : "单价与路由均已配置",
        action: { label: unpriced ? "去定价" : "管理入口", page: "config", sub: unpriced ? "pricing" : "routes" },
      },
      {
        id: "pools",
        no: "02",
        tone: "violet",
        title: "凭据池",
        desc: "多 Key / 多账号、轮换、冷却与配额",
        state: cooling ? `${cooling} 个冷却` : "运行正常",
        pillCls: cooling ? "amber" : "signal",
        metrics: [
          ["池", num(pools.length)],
          ["密钥 / 成员", num(poolMembers)],
          ["冷却中", num(cooling)],
          ["配额耗尽", num(exhausted)],
        ],
        hint: cooling || exhausted ? "打开池成员表查看冷却原因" : "成员均可用",
        action: { label: cooling ? "查看冷却成员" : "管理凭据池", page: "config", sub: "pools" },
      },
      {
        id: "resources",
        no: "03",
        tone: "amber",
        title: "资源库",
        desc: "插件、免费来源与官方模板",
        state: unattached ? `${unattached} 个未接入` : "全部已接入",
        pillCls: unattached ? "amber" : "signal",
        metrics: [
          ["插件", num(plugins.length)],
          ["未接入插件", num(unattached)],
          ["免费模型", num(countFreeModels())],
          ["模板", num((state.templates || []).length)],
        ],
        hint: unattached ? "安装只登记；接入才创建上游" : "资源均已接入",
        action: { label: unattached ? "处理插件接入" : "打开资源库", page: "config", sub: "plugins" },
      },
      {
        id: "settings",
        no: "04",
        tone: "",
        title: "设置",
        desc: "服务、隐私、AI 接口与高级维护",
        state: capture ? "提示词记录中" : "提示词未开启",
        pillCls: capture ? "signal" : "",
        metrics: [
          ["提示词记录", capture ? "开" : "关"],
          ["AI 配置", ai.enabled === false ? "停用" : "启用"],
          ["已定价条目", num(pricingEntries)],
          ["备份", num(Array.isArray(v4.hub?.backups?.items) ? v4.hub.backups.items.length : v4.hub?.backups?.total || 0)],
        ],
        hint: capture ? "配置保存前自动备份" : "如需审计入站消息，请开启提示词记录",
        action: { label: capture ? "继续上次修改" : "开启提示词记录", page: "config", sub: "settings" },
      },
    ];
  }

  function renderConfigHub() {
    const host = $("#config-domains");
    if (!host) return;
    const domains = configDomains();
    host.innerHTML = domains
      .map(
        (domain) => `<article class="domain-card ${domain.tone}" data-domain="${domain.id}">
          <div class="domain-head">
            <span class="domain-no">${domain.no}</span>
            <div class="grow"><div class="domain-title">${esc(domain.title)}</div><div class="domain-desc">${esc(domain.desc)}</div></div>
            <span class="pill ${domain.pillCls}">${esc(domain.state)}</span>
          </div>
          <div class="domain-metrics">
            ${domain.metrics.map(([k, v]) => `<div class="domain-metric"><div class="k">${esc(k)}</div><div class="v">${esc(v)}</div></div>`).join("")}
          </div>
          <div class="domain-foot"><span class="hint">${esc(domain.hint)}</span><button class="btn ghost sm" data-config-sub="${domain.action.sub}">${esc(domain.action.label)}</button></div>
        </article>`
      )
      .join("");

    const tasks = [
      { icon: "＋", title: "添加接入", sub: "官方只填 Key；自定义 5 项", time: "约 1 分钟", run: () => $("#quick-add")?.click() },
      { icon: "⚿", title: "创建凭据池", sub: "多 Key / 多账号统一轮换", time: "约 2 分钟", page: "config", sub: "pools", click: "#pool-add" },
      { icon: "⑃", title: "配置统一入口", sub: "可见上游与命名分组", time: "约 2 分钟", page: "config", sub: "routes" },
      { icon: "◎", title: "开启提示词记录", sub: "只记录入站 messages，默认脱敏", time: "约 30 秒", page: "config", sub: "settings", section: "privacy" },
      { icon: "▤", title: "补齐未定价模型", sub: "按上游固定价优先匹配", time: "约 3 分钟", page: "config", sub: "pricing" },
      { icon: "⤓", title: "导出诊断", sub: "配置、接入、凭据、价格、插件", time: "约 10 秒", run: () => exportDiagnostics() },
    ];
    setHTML(
      "#config-tasks",
      tasks
        .map(
          (task, index) => `<button class="task-row" data-task="${index}">
            <span class="mark">${task.icon}</span>
            <span class="grow"><span class="nm">${esc(task.title)}</span><span class="sub">${esc(task.sub)}</span></span>
            <span class="time">${esc(task.time)}</span>
          </button>`
        )
        .join("")
    );
    $$("#config-tasks [data-task]").forEach((button) => {
      button.addEventListener("click", () => {
        const task = tasks[Number(button.dataset.task)];
        if (!task) return;
        if (task.run) task.run();
        else {
          go(task.page, task.sub);
          if (task.section) setTimeout(() => applySettingsSection(task.section), 150);
          if (task.click) retry(() => {
            const el = $(task.click);
            if (!el) return false;
            el.click();
            return true;
          });
        }
      });
    });

    const auditItems = Array.isArray(v4.hub?.audit?.items) ? v4.hub.audit.items : [];
    setHTML(
      "#config-changes",
      auditItems.length
        ? auditItems
            .slice(0, 6)
            .map(
              (item) => `<div class="change-row">
                <span class="grow"><span class="nm">${esc(item.action || "配置变更")} · ${esc(item.target || "bridge.config.json")}</span><span class="sub">${esc(item.actor || "ui")} · ${esc(item.result || "ok")} · ${relTime(item.at)}</span></span>
                <span class="chev">›</span>
              </div>`
            )
            .join("")
        : `<div class="empty-line">还没有配置审计记录。保存设置后这里会出现最近变更。</div>`
    );
  }

  function filterConfigHub(query) {
    const q = String(query || "").trim().toLowerCase();
    $$("#config-domains [data-domain]").forEach((card) => {
      card.hidden = Boolean(q) && !card.textContent.toLowerCase().includes(q);
    });
    $$("#config-tasks .task-row").forEach((row) => {
      row.hidden = Boolean(q) && !row.textContent.toLowerCase().includes(q);
    });
  }

  // ───────────────────────── 日志与诊断 ─────────────────────────

  function setObserveTab(tab) {
    v4.observe = tab === "diag" ? "diag" : "logs";
    $$("#observe-tabs [data-observe-tab]").forEach((button) => {
      button.classList.toggle("active", button.dataset.observeTab === v4.observe);
    });
    show($("#observe-logs"), v4.observe === "logs");
    show($("#observe-diag"), v4.observe === "diag");
    if (v4.observe === "diag") loadDiagnostics();
  }

  async function loadDiagnostics() {
    setHTML("#diag-meta", "检查中…");
    const [pricing, free] = await Promise.all([
      api("/api/pricing").catch(() => ({})),
      api("/api/free-sources").catch(() => ({})),
    ]);
    renderDiagnostics(pricing || {}, free || {});
  }

  function diagnosticsFrom(pricing) {
    const groups = new Map();
    const push = (group, level, title, detail, action) => {
      if (!groups.has(group)) groups.set(group, []);
      groups.get(group).push({ level, title, detail, action });
    };

    const access = state.access || [];
    const pools = state.pools || [];
    const config = state.config || {};
    const upstreams = config.upstreams && typeof config.upstreams === "object" ? Object.keys(config.upstreams) : [];

    if (!config || !Object.keys(config).length) {
      push("配置", "error", "配置文件未读取", "无法读取 bridge.config.json，请检查文件权限或格式。", { page: "config", sub: "advanced" });
    } else {
      push("配置", "ok", "配置文件可读", `${upstreams.length} 个上游、${Array.isArray(config.models) ? config.models.length : 0} 个对外模型；保存前默认备份。`, { page: "config", sub: "advanced" });
    }

    if (!access.length) {
      push("接入", "warn", "还没有接入", "添加第一个官方模板或自定义上游后，网关才会转发请求。", { page: "access" });
    }
    for (const item of access) {
      const stateName = item.health?.state || "warn";
      if (stateName !== "ok") {
        push("接入", stateName === "error" ? "error" : "warn", `${item.label || item.id} 健康异常`, item.health?.message || "打开接入详情查看诊断页签。", { page: "access", id: item.id });
      }
      if (item.auth?.mode !== "none" && !item.auth?.configured) {
        push("接入", "warn", `${item.label || item.id} 缺少凭据`, "密钥只写不回显；请写入或绑定凭据池。", { page: "access", id: item.id });
      }
    }

    for (const pool of pools) {
      if (Number(pool.cooling || 0) > 0) {
        push("凭据", "warn", `池 ${pool.id} 有 ${pool.cooling} 个成员冷却`, `可用 ${pool.available}/${pool.members}；可在成员表重置冷却或补充成员。`, { page: "config", sub: "pools" });
      }
      if (Number(pool.exhausted || 0) > 0) {
        push("凭据", "error", `池 ${pool.id} 配额耗尽`, `${pool.exhausted} 个成员已耗尽，请调整配额或替换成员。`, { page: "config", sub: "pools" });
      }
      if (Number(pool.needs_secret || 0) > 0) {
        push("凭据", "warn", `池 ${pool.id} 有成员待补密钥`, "成员只保存 secret 引用；请重新写入密钥。", { page: "config", sub: "pools" });
      }
    }

    const entries = Number(pricing.entries || 0);
    if (!entries) {
      push("价格", "warn", "单价表为空", "费用统计会显示 0；至少添加一个默认条目或按上游固定价。", { page: "config", sub: "pricing" });
    } else {
      push("价格", "ok", `单价表 ${entries} 条`, `汇率 USD→CNY ${Number(pricing.fx?.USD || 7.2).toFixed(2)}；上游固定价优先于任意上游。`, { page: "config", sub: "pricing" });
    }
    for (const diag of Array.isArray(pricing.diagnostics) ? pricing.diagnostics : []) {
      push("价格", diag.level === "error" ? "error" : "warn", diag.code || "定价诊断", diag.message || "", { page: "config", sub: "pricing" });
    }

    for (const plugin of state.plugins || []) {
      if (!plugin.enabled) {
        push("插件", "warn", `插件 ${plugin.name || plugin.id} 已停用`, "停用后不会参与接入与请求路由。", { page: "config", sub: "plugins" });
      } else if (!Array.isArray(plugin.attached) || plugin.attached.length === 0) {
        push("插件", "info", `插件 ${plugin.name || plugin.id} 未接入`, "安装只登记；点击「接入」才创建上游。", { page: "config", sub: "plugins" });
      }
    }

    if (!state.templates?.length) {
      push("模板", "warn", "模板目录为空", "官方模板不可用；请检查内置模板或重新加载配置。", { page: "config", sub: "templates" });
    }

    return groups;
  }

  function renderDiagnostics(pricing) {
    if (v4.hub) v4.hub.pricing = pricing;
    const groups = diagnosticsFrom(pricing);
    const flat = [...groups.values()].flat();
    const errors = flat.filter((item) => item.level === "error").length;
    const warns = flat.filter((item) => item.level === "warn").length;
    const ok = flat.filter((item) => item.level === "ok").length;

    setHTML(
      "#diag-summary",
      `<div class="diag-stat ${errors ? "danger" : "signal"}"><span class="k">错误</span><b>${errors}</b></div>
       <div class="diag-stat ${warns ? "amber" : "signal"}"><span class="k">警告</span><b>${warns}</b></div>
       <div class="diag-stat"><span class="k">通过</span><b>${ok}</b></div>
       <div class="diag-stat"><span class="k">检查项</span><b>${flat.length}</b></div>`
    );

    const order = ["配置", "接入", "凭据", "价格", "插件", "模板"];
    const host = $("#diag-groups");
    if (!host) return;
    host.innerHTML = order
      .filter((group) => groups.has(group))
      .map((group) => {
        const items = groups.get(group).sort((a, b) => {
          const rank = { error: 0, warn: 1, info: 2, ok: 3 };
          return rank[a.level] - rank[b.level];
        });
        return `<div class="card flush diag-card">
          <div class="card-head"><h2>${esc(group)}</h2><span class="pill ${items.some((i) => i.level === "error") ? "danger" : items.some((i) => i.level === "warn") ? "amber" : "signal"}">${items.length} 项</span></div>
          ${items
            .map(
              (item) => `<div class="diag-row ${item.level}">
                <span class="mark">${item.level === "error" ? "!" : item.level === "warn" ? "?" : item.level === "info" ? "i" : "✓"}</span>
                <div class="grow"><div class="nm">${esc(item.title)}</div><div class="sub">${esc(item.detail)}</div></div>
                ${item.action ? `<button class="btn ghost sm" data-v4-issue='${esc(JSON.stringify(item.action))}'>查看</button>` : ""}
              </div>`
            )
            .join("")}
        </div>`;
      })
      .join("");
    setHTML("#diag-meta", `${flat.length} 项检查 · ${relTime(Date.now() / 1000)}`);
  }

  function diagnosticsText() {
    const groups = diagnosticsFrom(v4.hub?.pricing || {});
    const lines = [`Model-Gate 诊断报告 ${new Date().toLocaleString("zh-CN", { hour12: false })}`];
    for (const [group, items] of groups.entries()) {
      lines.push(`\n[${group}]`);
      for (const item of items) lines.push(`- [${item.level}] ${item.title}: ${item.detail}`);
    }
    return lines.join("\n");
  }

  async function exportDiagnostics() {
    const text = diagnosticsText();
    try {
      await navigator.clipboard.writeText(text);
      toast("诊断报告已复制到剪贴板");
    } catch {
      MG.openModal("诊断报告", "复制以下内容", `<pre class="log-viewer">${esc(text)}</pre>`);
    }
  }

  // ───────────────────────── 设置分栏 ─────────────────────────

  function cardSection(card) {
    const title = card.querySelector(".card-head h2")?.textContent?.trim() || "";
    if (title === "服务与全局") return "service";
    if (title === "提示词记录" || title === "AI 配置接口") return "privacy";
    if (title === "统一接入可见性" || title === "统一入口分组" || title === "路由组") return "routes";
    if (title === "模板") return "templates";
    if (title === "高级维护") return "advanced";
    return "other";
  }

  function buildSettingsLayout() {
    const view = $("#view-settings");
    if (!view || view.dataset.layoutReady === "1") return;
    const grid = view.querySelector(".grid.two");
    if (!grid) return;

    const layout = document.createElement("div");
    layout.className = "settings-layout";
    const nav = document.createElement("aside");
    nav.className = "settings-nav";
    nav.id = "settings-nav";
    const body = document.createElement("div");
    body.className = "settings-main";
    grid.parentNode.insertBefore(layout, grid);
    layout.appendChild(nav);
    layout.appendChild(body);
    body.appendChild(grid);

    $$(".card", grid).forEach((card) => {
      card.dataset.settingsSection = cardSection(card);
    });

    nav.innerHTML = SETTINGS_SECTIONS.map(
      (section) => `<button class="settings-nav-item" data-settings-section="${section.id}"><span>${esc(section.label)}</span><small>${esc(section.hint)}</small></button>`
    ).join("");
    $$("#settings-nav [data-settings-section]").forEach((button) => {
      button.addEventListener("click", () => applySettingsSection(button.dataset.settingsSection));
    });

    view.dataset.layoutReady = "1";
    applySettingsSection(v4.settingsSection || "service");
  }

  function applySettingsSection(section) {
    const view = $("#view-settings");
    if (!view) return;
    buildSettingsLayout();
    const target = SETTINGS_SECTIONS.some((item) => item.id === section) ? section : "service";
    v4.settingsSection = target;
    $$("#settings-nav [data-settings-section]").forEach((button) => {
      button.classList.toggle("active", button.dataset.settingsSection === target);
    });
    const cards = $$("#view-settings .card[data-settings-section]");
    cards.forEach((card) => {
      const cardSectionName = card.dataset.settingsSection;
      const visible = target === "all" || cardSectionName === target || (target === "privacy" && cardSectionName === "privacy") || (target === "routes" && cardSectionName === "routes");
      card.hidden = !visible;
    });
    const grid = view.querySelector(".grid.two");
    if (grid) grid.classList.toggle("single", target !== "service");
  }

  // ───────────────────────── 接入页增强 ─────────────────────────

  function enhanceAccess() {
    const toolbar = $("#view-access .toolbar");
    const select = $("#access-kind");
    if (!toolbar || !select || toolbar.dataset.enhanced === "1") return;
    toolbar.dataset.enhanced = "1";
    const seg = document.createElement("div");
    seg.className = "seg source-seg";
    seg.innerHTML = [
      ["", "全部"],
      ["official", "官方"],
      ["custom", "自定义"],
      ["free", "免费"],
      ["account", "账号池"],
      ["plugin", "插件"],
    ]
      .map(([value, label]) => `<button type="button" data-kind="${value}" class="${value === "" ? "active" : ""}">${label}</button>`)
      .join("");
    select.hidden = true;
    select.parentNode.insertBefore(seg, select.nextSibling);
    $$("button", seg).forEach((button) => {
      button.addEventListener("click", () => {
        select.value = button.dataset.kind;
        $$("button", seg).forEach((item) => item.classList.toggle("active", item === button));
        select.dispatchEvent(new Event("change", { bubbles: true }));
      });
    });
  }

  // ───────────────────────── 用量分析 v4 结构 ─────────────────────────

  function enhanceUsage() {
    const view = $("#view-traffic");
    if (!view || view.dataset.usageReady === "1") return;
    const toolbar = view.querySelector(".toolbar");
    const kpis = $("#usage-kpis");
    if (!toolbar) return;
    view.dataset.usageReady = "1";
    toolbar.classList.add("usage-scope");
    if (kpis) kpis.classList.add("usage-kpis");

    const cards = $$("#view-traffic .card");
    const byTitle = (title) => cards.find((card) => card.querySelector(".card-head h2")?.textContent.trim() === title);
    const trend = byTitle("趋势");
    if (trend) trend.classList.add("usage-trend-card");
    const mix = byTitle("Token 构成");
    if (mix) mix.classList.add("usage-mini-card");
    const latency = byTitle("耗时分布");
    if (latency) latency.classList.add("usage-mini-card");
    const pie = byTitle("分布");
    if (pie) pie.classList.add("usage-mini-card");

    const upstream = byTitle("上游用量");
    const models = byTitle("模型用量");
    const sessions = byTitle("会话统计");
    const requests = byTitle("请求明细");
    const retention = byTitle("数据保留与清理");
    if (!upstream || !requests) return;

    const tabs = document.createElement("div");
    tabs.className = "seg-tabs usage-tabs";
    tabs.id = "usage-tabs";
    tabs.innerHTML = [
      ["upstreams", "上游"],
      ["models", "模型"],
      ["sessions", "会话"],
      ["requests", "请求"],
    ]
      .map(([id, label]) => `<button class="seg-tab ${id === "upstreams" ? "active" : ""}" data-usage-tab="${id}">${label}</button>`)
      .join("");
    upstream.parentNode.insertBefore(tabs, upstream);

    const panels = document.createElement("div");
    panels.className = "usage-panels";
    upstream.parentNode.insertBefore(panels, upstream);
    const map = new Map([
      [upstream, "upstreams"],
      [models, "models"],
      [sessions, "sessions"],
      [requests, "requests"],
      [retention, "requests"],
    ]);
    for (const [card, panel] of map) {
      if (!card) continue;
      card.classList.add("usage-panel");
      card.dataset.usagePanel = panel;
      panels.appendChild(card);
    }
    tabs.addEventListener("click", (event) => {
      const button = event.target.closest("[data-usage-tab]");
      if (!button) return;
      setUsagePanel(button.dataset.usageTab);
    });
    setUsagePanel("upstreams");
  }

  function setUsagePanel(panel) {
    const target = panel || "upstreams";
    $$("#usage-tabs [data-usage-tab]").forEach((button) => button.classList.toggle("active", button.dataset.usageTab === target));
    $$("#view-traffic .usage-panel").forEach((card) => {
      card.hidden = card.dataset.usagePanel !== target;
    });
  }

  // ───────────────────────── 提示词左右分栏 ─────────────────────────

  function enhancePrompts() {
    const view = $("#view-prompts");
    const list = $("#prompt-list-view");
    if (!view || !list || view.dataset.promptReady === "1") return;
    view.dataset.promptReady = "1";
    const split = document.createElement("div");
    split.className = "prompt-split";
    list.parentNode.insertBefore(split, list);
    split.appendChild(list);
    list.classList.add("prompt-list");
    const side = document.createElement("aside");
    side.className = "prompt-side";
    side.id = "prompt-side";
    side.innerHTML = `<div class="card">
      <div class="card-head"><h2>提示词详情</h2><span class="hint" id="prompt-side-meta">选择左侧记录</span></div>
      <div id="prompt-side-body"><div class="empty-line">从左侧列表选择一条记录，在这里查看消息树、角色分布与脱敏统计。</div></div>
    </div>`;
    split.appendChild(side);
    $("#prompt-side-body")?.addEventListener("click", (event) => {
      const button = event.target.closest("[data-prompt-side-download]");
      if (button) window.open(`/api/prompts/${encodeURIComponent(button.dataset.promptSideDownload)}/download`, "_blank");
    });
  }

  async function loadPromptSide(id) {
    const host = $("#prompt-side-body");
    if (!host) return;
    host.innerHTML = `<div class="empty-line">加载中…</div>`;
    try {
      const data = await api(`/api/prompts/${encodeURIComponent(id)}`);
      const messages = Array.isArray(data.messages) ? data.messages : [];
      const meta = $("#prompt-side-meta");
      if (meta) meta.textContent = `${data.requested_model || "—"} · ${fmtTime(data.ts)}`;
      host.innerHTML = `<div class="kv prompt-side-kv">
          <dt>会话</dt><dd><code>${esc(data.session || "—")}</code></dd>
          <dt>消息</dt><dd>${num(data.message_count)}</dd>
          <dt>大小</dt><dd>${num(data.size_bytes)} 字符</dd>
          <dt>脱敏</dt><dd>${num(data.redactions)} 处</dd>
        </div>
        <div class="section-title">消息树</div>
        <div class="prompt-messages">${
          messages
            .map(
              (message, index) => `<details class="message-block" ${index === 0 ? "open" : ""}>
                <summary><span class="pill">${esc(message.role || "?")}</span><span class="muted">#${index + 1} · ${num(String(message.content || "").length)} 字符</span></summary>
                <pre>${esc(message.content || "")}</pre>
              </details>`
            )
            .join("") || '<div class="empty-line">没有消息。</div>'
        }</div>
        <div class="card-actions" style="margin-top:12px">
          <button class="btn ghost sm" data-prompt-side-download="${esc(id)}">下载 JSONL</button>
        </div>`;
    } catch (error) {
      host.innerHTML = `<div class="empty-line">读取失败：${esc(error.message)}</div>`;
    }
  }

  // ───────────────────────── 接入批量操作 ─────────────────────────

  function ensureAccessBatch() {
    if ($("#access-batch")) return;
    const view = $("#view-access");
    if (!view) return;
    const bar = document.createElement("div");
    bar.className = "batch-bar";
    bar.id = "access-batch";
    bar.hidden = true;
    bar.innerHTML = `<span class="batch-count" id="access-batch-count">已选 0</span>
      <button class="btn ghost sm" data-batch="test">测试连接</button>
      <button class="btn ghost sm" data-batch="export">导出 JSON</button>
      <span class="spacer"></span>
      <button class="btn ghost sm" data-batch="clear">清空选择</button>`;
    view.appendChild(bar);
    bar.addEventListener("click", async (event) => {
      const button = event.target.closest("[data-batch]");
      if (!button) return;
      const ids = [...(v4.accessSelected || new Set())];
      if (button.dataset.batch === "clear") {
        v4.accessSelected = new Set();
        $$("#access-table input[data-access-id]").forEach((box) => {
          box.checked = false;
        });
        updateAccessBatch();
        return;
      }
      if (!ids.length) {
        toast("先选择接入", "error");
        return;
      }
      if (button.dataset.batch === "test") {
        toast(`正在测试 ${ids.length} 个接入…`);
        const results = await Promise.all(
          ids.map(async (id) => {
            try {
              const result = await api(`/api/access/${encodeURIComponent(id)}/probe`, { method: "POST" });
              return { id, ok: Boolean(result.ok) };
            } catch (error) {
              return { id, ok: false, message: error.message };
            }
          })
        );
        const ok = results.filter((item) => item.ok).length;
        toast(`测试完成：${ok}/${results.length} 成功`, ok === results.length ? "" : "error");
        return;
      }
      if (button.dataset.batch === "export") {
        const rows = (state.access || []).filter((item) => ids.includes(item.id));
        const blob = new Blob([JSON.stringify(rows, null, 2)], { type: "application/json" });
        const link = document.createElement("a");
        link.href = URL.createObjectURL(blob);
        link.download = `model-gate-access-${Date.now()}.json`;
        link.click();
        URL.revokeObjectURL(link.href);
      }
    });
  }

  function updateAccessBatch() {
    const count = (v4.accessSelected || new Set()).size;
    const bar = $("#access-batch");
    if (bar) bar.hidden = count === 0;
    const label = $("#access-batch-count");
    if (label) label.textContent = `已选 ${count}`;
    const all = $("#access-select-all");
    if (all) {
      const boxes = $$("#access-table input[data-access-id]");
      all.checked = boxes.length > 0 && boxes.every((box) => box.checked);
    }
  }

  function enhanceAccessTable() {
    const table = $("#access-table");
    if (!table) return;
    ensureAccessBatch();
    if (table.dataset.batchReady !== "1") {
      table.dataset.batchReady = "1";
      const head = table.querySelector("thead tr");
      if (head && !head.querySelector(".batch-cell")) {
        const th = document.createElement("th");
        th.className = "batch-cell";
        th.innerHTML = `<input type="checkbox" id="access-select-all" aria-label="全选">`;
        head.prepend(th);
      }
      $("#access-select-all")?.addEventListener("change", (event) => {
        const checked = event.target.checked;
        v4.accessSelected = v4.accessSelected || new Set();
        $$("#access-table input[data-access-id]").forEach((box) => {
          box.checked = checked;
          if (checked) v4.accessSelected.add(box.dataset.accessId);
          else v4.accessSelected.delete(box.dataset.accessId);
        });
        updateAccessBatch();
      });
      const toolbar = $("#view-access .toolbar");
      if (toolbar && !toolbar.querySelector(".density-seg")) {
        const seg = document.createElement("div");
        seg.className = "seg density-seg";
        seg.innerHTML = `<button class="active" data-density="comfortable">舒适</button><button data-density="compact">紧凑</button>`;
        toolbar.appendChild(seg);
        seg.addEventListener("click", (event) => {
          const button = event.target.closest("[data-density]");
          if (!button) return;
          $$("button", seg).forEach((item) => item.classList.toggle("active", item === button));
          table.classList.toggle("compact", button.dataset.density === "compact");
        });
      }
      const tbody = table.querySelector("tbody");
      if (tbody) {
        new MutationObserver(() => decorateAccessRows()).observe(tbody, { childList: true });
      }
    }
    decorateAccessRows();
  }

  function decorateAccessRows() {
    const tbody = $("#access-table tbody");
    if (!tbody) return;
    v4.accessSelected = v4.accessSelected || new Set();
    $$("tr[data-open]", tbody).forEach((row) => {
      if (row.querySelector(".batch-cell")) return;
      const id = row.dataset.open;
      const td = document.createElement("td");
      td.className = "batch-cell";
      td.innerHTML = `<input type="checkbox" data-access-id="${esc(id)}" aria-label="选择 ${esc(id)}">`;
      row.prepend(td);
    });
    $$("input[data-access-id]", tbody).forEach((box) => {
      if (box.dataset.bound === "1") return;
      box.dataset.bound = "1";
      box.checked = v4.accessSelected.has(box.dataset.accessId);
      box.addEventListener("click", (event) => event.stopPropagation());
      box.addEventListener("change", () => {
        if (box.checked) v4.accessSelected.add(box.dataset.accessId);
        else v4.accessSelected.delete(box.dataset.accessId);
        updateAccessBatch();
      });
    });
    updateAccessBatch();
  }

  // ───────────────────────── 命令面板 ─────────────────────────

  function ensureCmdk() {
    if ($("#v4-cmdk")) return;
    const host = document.createElement("div");
    host.className = "cmdk";
    host.id = "v4-cmdk";
    host.hidden = true;
    host.innerHTML = `<div class="cmdk-panel" role="dialog" aria-modal="true" aria-label="全局搜索">
      <div class="cmdk-input"><span class="ico">⌕</span><input id="v4-cmdk-input" placeholder="搜索页面、接入、模型、配置任务…" autocomplete="off"><kbd>ESC</kbd></div>
      <div class="cmdk-list" id="v4-cmdk-list"></div>
      <div class="cmdk-foot"><span>↑↓ 选择</span><span>↵ 打开</span><span>Esc 关闭</span></div>
    </div>`;
    document.body.appendChild(host);
    host.addEventListener("click", (event) => {
      if (event.target === host) closeCmdk();
    });
    const input = $("#v4-cmdk-input");
    input.addEventListener("input", () => {
      v4.cmdkIndex = 0;
      renderCmdk();
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        v4.cmdkIndex = Math.min(v4.cmdkItems.length - 1, v4.cmdkIndex + 1);
        updateCmdkActive();
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        v4.cmdkIndex = Math.max(0, v4.cmdkIndex - 1);
        updateCmdkActive();
      } else if (event.key === "Enter") {
        event.preventDefault();
        runCmdkItem(v4.cmdkItems[v4.cmdkIndex]);
      }
    });
  }

  function commandItems() {
    const items = [];
    for (const [id, page] of Object.entries(TOP)) {
      items.push({ group: "页面", icon: id === "config" ? "⚙" : "◈", label: page.title, sub: `打开${page.title}`, run: () => go(id) });
    }
    for (const [id, item] of Object.entries(CONFIG)) {
      items.push({ group: "配置中心", icon: "▸", label: item.title, sub: `${item.group} · 配置中心`, run: () => go("config", id) });
    }
    for (const item of (state.access || []).slice(0, 20)) {
      items.push({
        group: "接入",
        icon: "⇄",
        label: item.label || item.id,
        sub: `${item.protocol || ""} · ${item.id}`,
        run: () => openAccessDrawer(item.id),
      });
    }
    for (const row of (v4.hub?.catalog?.rows || []).slice(0, 20)) {
      items.push({
        group: "模型",
        icon: "▤",
        label: row.model,
        sub: `模型目录 · ${(row.upstreams || []).join(" / ")}`,
        run: () => go("config", "models"),
      });
    }
    items.push({ group: "任务", icon: "＋", label: "添加接入", sub: "打开添加向导", run: () => $("#quick-add")?.click() });
    items.push({ group: "任务", icon: "⚿", label: "新建凭据池", sub: "配置中心 · 凭据池", run: () => { go("config", "pools"); retry(() => { const el = $("#pool-add"); if (!el) return false; el.click(); return true; }); } });
    items.push({ group: "任务", icon: "▤", label: "补齐未定价模型", sub: "配置中心 · 单价表", run: () => go("config", "pricing") });
    items.push({ group: "任务", icon: "⑃", label: "配置统一入口", sub: "配置中心 · 统一入口", run: () => go("config", "routes") });
    items.push({ group: "任务", icon: "◎", label: "开启提示词记录", sub: "配置中心 · 设置", run: () => { go("config", "settings"); setTimeout(() => applySettingsSection("privacy"), 150); } });
    items.push({ group: "任务", icon: "⤓", label: "导出诊断", sub: "复制诊断报告", run: () => exportDiagnostics() });
    return items;
  }

  function openCmdk() {
    ensureCmdk();
    v4.cmdkIndex = 0;
    const host = $("#v4-cmdk");
    host.hidden = false;
    const input = $("#v4-cmdk-input");
    input.value = "";
    renderCmdk();
    setTimeout(() => input.focus(), 0);
  }

  function closeCmdk() {
    const host = $("#v4-cmdk");
    if (host) host.hidden = true;
  }

  function filteredCmdk() {
    const input = $("#v4-cmdk-input");
    const q = String(input?.value || "").trim().toLowerCase();
    const all = commandItems();
    if (!q) return all.slice(0, 40);
    return all.filter((item) => `${item.label} ${item.sub} ${item.group}`.toLowerCase().includes(q)).slice(0, 40);
  }

  function renderCmdk() {
    const list = $("#v4-cmdk-list");
    if (!list) return;
    v4.cmdkItems = filteredCmdk();
    if (!v4.cmdkItems.length) {
      list.innerHTML = `<div class="cmdk-empty">没有匹配项</div>`;
      return;
    }
    let lastGroup = "";
    list.innerHTML = v4.cmdkItems
      .map((item, index) => {
        const head = item.group !== lastGroup ? `<div class="cmdk-group">${esc(item.group)}</div>` : "";
        lastGroup = item.group;
        return `${head}<button class="cmdk-item ${index === v4.cmdkIndex ? "active" : ""}" data-cmdk="${index}">
          <span class="ico">${item.icon}</span>
          <span class="grow"><span class="nm">${esc(item.label)}</span><span class="sub">${esc(item.sub)}</span></span>
        </button>`;
      })
      .join("");
    $$("#v4-cmdk-list [data-cmdk]").forEach((button) => {
      button.addEventListener("click", () => runCmdkItem(v4.cmdkItems[Number(button.dataset.cmdk)]));
    });
    updateCmdkActive();
  }

  function updateCmdkActive() {
    const buttons = $$("#v4-cmdk-list [data-cmdk]");
    buttons.forEach((button, index) => button.classList.toggle("active", index === v4.cmdkIndex));
    const active = buttons[v4.cmdkIndex];
    if (active) active.scrollIntoView({ block: "nearest" });
  }

  function runCmdkItem(item) {
    if (!item) return;
    closeCmdk();
    try {
      item.run();
    } catch (error) {
      toast(error.message || "执行失败", "error");
    }
  }

  function closeRequestDrawer() {
    const drawer = $("#request-drawer");
    if (drawer) drawer.hidden = true;
    const scrim = $("#scrim");
    if (scrim) scrim.hidden = true;
  }

  function openRequestDrawer(row) {
    if (!row) return;
    const drawer = $("#request-drawer");
    if (!drawer) return;
    setHTML("#request-title", esc(row.requested_model || row.upstream_model || "请求详情"));
    setHTML(
      "#request-sub",
      `<span class="pill">${esc(row.upstream || "—")}</span><span class="pill ${Number(row.status) >= 400 ? "danger" : "signal"}">${esc(row.status || "—")}</span><span class="pill">${fmtMs(row.duration_ms)}</span>`
    );
    const fields = [
      ["请求 ID", row.id],
      ["时间", fmtTime(row.ts)],
      ["上游", row.upstream],
      ["请求模型", row.requested_model],
      ["上游模型", row.upstream_model],
      ["协议", row.protocol],
      ["状态", row.status],
      ["耗时", fmtMs(row.duration_ms)],
      ["首 Token", fmtMs(row.ttft_ms)],
      ["输入 / 输出", `${num(row.prompt_tokens)} / ${num(row.completion_tokens)}`],
      ["缓存读取", num(row.cache_read_tokens)],
      ["费用", money(row.cost)],
      ["会话", row.session || "—"],
      ["错误", row.error || "—"],
    ];
    setHTML(
      "#request-body",
      `<div class="kv request-kv">${fields
        .map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v == null || v === "" ? "—" : v)}</dd>`)
        .join("")}</div>
       <div class="inline-note" style="margin-top:14px">脱敏视图：请求头与提示词正文不在此处展示；提示词请到「提示词」页查看。</div>
       <div class="card-actions wrap" style="margin-top:12px">
         <button class="btn ghost sm" data-v4-go="prompts">打开提示词</button>
         <button class="btn ghost sm" data-v4-go="logs">打开日志</button>
       </div>`
    );
    drawer.hidden = false;
    const scrim = $("#scrim");
    if (scrim) scrim.hidden = false;
  }

  MG.openRequestDrawer = openRequestDrawer;

  function setUsageTab(tab) {
    const target = ["upstream", "model", "session", "request"].includes(tab) ? tab : "upstream";
    $$("#usage-tabs [data-usage-tab]").forEach((button) => {
      button.classList.toggle("active", button.dataset.usageTab === target);
    });
    ["upstream", "model", "session", "request"].forEach((name) => {
      const panel = $(`#usage-panel-${name}`);
      if (panel) panel.hidden = name !== target;
    });
  }

  function openAccessDrawer(id) {
    go("access");
    retry(() => {
      const row = document.querySelector(`#access-table [data-open="${id.replaceAll('"', '\\"')}"]`);
      if (!row) return false;
      row.click();
      return true;
    }, 1600);
  }

  // ───────────────────────── 初始化 ─────────────────────────

  function bindEvents() {
    document.addEventListener(
      "click",
      (event) => {
        const nav = event.target.closest(".nav-item");
        if (nav && nav.dataset.view) {
          event.preventDefault();
          event.stopPropagation();
          go(nav.dataset.view);
          return;
        }
        const configLink = event.target.closest("[data-config-sub]");
        if (configLink) {
          event.preventDefault();
          event.stopPropagation();
          go("config", configLink.dataset.configSub);
          return;
        }
        const v4go = event.target.closest("[data-v4-go]");
        if (v4go) {
          event.preventDefault();
          event.stopPropagation();
          const [page, sub] = String(v4go.dataset.v4Go || "").split("/");
          go(page, sub);
          return;
        }
        const issue = event.target.closest("[data-v4-issue]");
        if (issue) {
          event.preventDefault();
          event.stopPropagation();
          try {
            const action = JSON.parse(issue.dataset.v4Issue);
            if (action.page === "access" && action.id) openAccessDrawer(action.id);
            else go(action.page || "config", action.sub || "");
          } catch {
            /* 忽略坏数据 */
          }
          return;
        }
        const accessRow = event.target.closest("[data-v4-access]");
        if (accessRow) {
          event.preventDefault();
          event.stopPropagation();
          openAccessDrawer(accessRow.dataset.v4Access);
          return;
        }
        const promptRow = event.target.closest("#prompt-table tbody tr[data-prompt-id]");
        if (promptRow && !event.target.closest("button")) {
          event.preventDefault();
          event.stopPropagation();
          loadPromptSide(promptRow.dataset.promptId);
          return;
        }
        const usageTab = event.target.closest("#usage-tabs [data-usage-tab]");
        if (usageTab) {
          event.preventDefault();
          event.stopPropagation();
          setUsageTab(usageTab.dataset.usageTab);
          return;
        }
        const legacyGo = event.target.closest("[data-go]");
        if (legacyGo) {
          event.preventDefault();
          event.stopPropagation();
          const value = legacyGo.dataset.go;
          if (LEGACY_TO_CONFIG[value]) go("config", LEGACY_TO_CONFIG[value]);
          else if (value === "observe") go("logs");
          else go(value);
          return;
        }
        const observe = event.target.closest("[data-observe-tab]");
        if (observe) {
          event.preventDefault();
          event.stopPropagation();
          setObserveTab(observe.dataset.observeTab);
        }
      },
      true
    );

    document.addEventListener(
      "keydown",
      (event) => {
        if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
          // 内核也监听 Ctrl+K 并会跳到接入页，这里必须在捕获阶段截断。
          event.preventDefault();
          event.stopImmediatePropagation();
          openCmdk();
        } else if (event.key === "/" && !event.ctrlKey && !event.metaKey && !event.altKey && !isTyping(event.target) && !overlayOpen()) {
          event.preventDefault();
          event.stopImmediatePropagation();
          openCmdk();
        } else if (!event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey && !isTyping(event.target) && !overlayOpen()) {
          const key = event.key.toLowerCase();
          if (key === "g") {
            v4.gPending = Date.now();
          } else if (v4.gPending && Date.now() - v4.gPending < 1500) {
            const target = {
              o: ["overview"],
              u: ["traffic"],
              p: ["prompts"],
              a: ["access"],
              l: ["logs"],
              c: ["config"],
              s: ["config", "settings"],
              d: ["logs"],
            }[key];
            v4.gPending = 0;
            if (target) {
              event.preventDefault();
              event.stopImmediatePropagation();
              if (key === "d") {
                go("logs");
                setObserveTab("diag");
              } else {
                go(target[0], target[1]);
              }
            }
          }
        } else if (event.key === "Escape") {
          if ($("#v4-cmdk") && !$("#v4-cmdk").hidden) {
            event.stopImmediatePropagation();
            closeCmdk();
          } else if ($("#request-drawer") && !$("#request-drawer").hidden) {
            event.stopImmediatePropagation();
            closeRequestDrawer();
          }
        }
      },
      true
    );

    $("#global-search")?.addEventListener("click", (event) => {
      event.preventDefault();
      openCmdk();
    });

    window.addEventListener("popstate", () => {
      const route = parseLocation();
      go(route.page, route.sub, { replace: true });
    });

    $("#scrim")?.addEventListener("click", () => closeRequestDrawer());
    $("#request-close")?.addEventListener("click", () => closeRequestDrawer());
    $("#config-refresh")?.addEventListener("click", () => loadConfigHub());
    $("#config-audit-all")?.addEventListener("click", () => go("config", "advanced"));
    $("#config-search")?.addEventListener("input", (event) => filterConfigHub(event.target.value));
    $("#diag-refresh")?.addEventListener("click", () => loadDiagnostics());
    $("#diag-copy")?.addEventListener("click", () => exportDiagnostics());

    $("#overview-refresh")?.addEventListener("click", async () => {
      await Promise.all([MG.refreshAccess(), MG.refreshPools()]);
      await loadOverview();
    });
    $("#ov-add")?.addEventListener("click", () => $("#quick-add")?.click());
    $("#ov-export")?.addEventListener("click", () => go("traffic"));

    $$("#ov-range [data-range]").forEach((button) => {
      button.addEventListener("click", () => {
        v4.ovRange = Number(button.dataset.range || 1);
        $$("#ov-range [data-range]").forEach((item) => item.classList.toggle("active", item === button));
        loadOverview();
      });
    });
    $$("#ov-metric [data-metric]").forEach((button) => {
      button.addEventListener("click", () => {
        v4.ovMetric = button.dataset.metric || "requests";
        $$("#ov-metric [data-metric]").forEach((item) => item.classList.toggle("active", item === button));
        renderOverviewChart();
      });
    });

  }

  function waitForBoot() {
    return new Promise((resolve) => {
      const started = Date.now();
      const timer = setInterval(() => {
        const text = $("#conn-text")?.textContent || "";
        if (text !== "连接中…" || Date.now() - started > 6000) {
          clearInterval(timer);
          v4.booted = true;
          resolve();
        }
      }, 100);
    });
  }

  async function init() {
    document.documentElement.dataset.ui = "v4";
    ensureCmdk();
    buildSettingsLayout();
    bindEvents();
    applyChrome();
    await waitForBoot();
    const route = parseLocation();
    go(route.page, route.sub, { replace: true });
    // 首次进入配置中心时预取任务板数据，后续切换直接复用。
    if (route.page === "config" || route.page === "overview") loadConfigHub();
  }

  // 在脚本加载阶段注册，确保内核 boot 的 initialView 能识别 config。
  MG.registerView("config", loadConfigHub);

  document.addEventListener("DOMContentLoaded", init);
})();
