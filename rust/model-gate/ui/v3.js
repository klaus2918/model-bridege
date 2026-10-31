/* Model-Gate v3 控制台（原生 JS，无构建步骤）。
   设计约束：密钥不可回显；所有动态文本转义；旧接口保持可用。 */
(() => {
  "use strict";

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

  const KIND_LABEL = {
    official: "官方",
    custom: "自定义",
    free: "免费",
    account: "账号池",
    plugin: "插件",
  };
  const STRATEGY_LABEL = {
    priority: "优先级",
    round_robin: "轮询",
    weighted: "加权",
    least_used: "最少使用",
  };
  const SUPPORT_LABEL = {
    request: "请求级",
    account: "账户级",
    none: "不支持请求级",
  };
  const STATUS_LABEL = {
    active: "可用",
    cooling: "冷却中",
    half_open: "半开",
    exhausted: "配额耗尽",
    disabled: "已停用",
    needs_secret: "待补密钥",
  };

  const externalViews = new Map();

  const state = {
    view: "overview",
    access: [],
    templates: [],
    pools: [],
    poolDetails: {},
    plugins: [],
    config: null,
    info: null,
    totals: null,
    usageRows: [],
    logs: [],
    prompts: [],
    health: [],
    diagnostics: [],
    aiSettings: { enabled: true, allowApply: false, allowPoolStats: true },
    drawerId: null,
    drawerTab: "overview",
    addMode: "official",
    addStep: 1,
    addTemplate: null,
    addResult: null,
    trafficDays: 1,
    busy: false,
  };

  // ───────────────────────── 基础工具 ─────────────────────────

  function esc(value) {
    return String(value == null ? "" : value)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function num(value) {
    const n = Number(value || 0);
    if (!Number.isFinite(n)) return "0";
    return n.toLocaleString("zh-CN");
  }

  function compactNum(value) {
    const n = Number(value || 0);
    if (!Number.isFinite(n)) return "0";
    if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return String(n);
  }

  function fmtTime(ts) {
    if (!ts) return "—";
    const date = typeof ts === "number" ? new Date(ts * 1000) : new Date(ts);
    if (Number.isNaN(date.getTime())) return "—";
    return date.toLocaleString("zh-CN", { hour12: false });
  }

  function fmtMs(ms) {
    const n = Number(ms || 0);
    if (!n) return "—";
    if (n >= 1000) return `${(n / 1000).toFixed(2)}s`;
    return `${Math.round(n)}ms`;
  }

  function badge(kind, label) {
    const k = String(kind || "custom");
    return `<span class="badge ${esc(k)}">${esc(label || KIND_LABEL[k] || k)}</span>`;
  }

  function statusDot(health) {
    const k = health === "ok" ? "ok" : health === "error" ? "error" : health === "idle" ? "idle" : "warn";
    return `<span class="dot ${k}" style="width:7px;height:7px"></span>`;
  }

  function privacyText(privacy) {
    if (!privacy) return "—";
    const zdr = privacy.zdr ? "ZDR 开" : "ZDR 关";
    const support = privacy.support ? ` · ${SUPPORT_LABEL[privacy.support] || privacy.support}` : "";
    return zdr + support;
  }

  function authText(access) {
    const auth = access.auth || {};
    if (auth.mode === "pool") {
      const pool = access.pool;
      if (pool) return `池 ${pool.available}/${pool.members} 可用`;
      return "池（统计待刷新）";
    }
    if (auth.mode === "none") return auth.configured ? "无需" : "无需（未配置）";
    if (!auth.configured) return "待补充";
    const source = auth.source && auth.source !== "none" ? ` · ${auth.source}` : "";
    return `单 Key${source}`;
  }

  function toast(message, type = "") {
    const el = $("#toast");
    el.textContent = message;
    el.className = `toast ${type}`.trim();
    el.hidden = false;
    clearTimeout(toast._timer);
    toast._timer = setTimeout(() => {
      el.hidden = true;
    }, 3200);
  }

  async function api(path, options = {}) {
    const headers = { "X-MG-Actor": "ui", ...(options.headers || {}) };
    if (options.body && !headers["Content-Type"]) headers["Content-Type"] = "application/json";
    const response = await fetch(path, { ...options, headers });
    const text = await response.text();
    let data = {};
    try {
      data = text ? JSON.parse(text) : {};
    } catch {
      data = { raw: text };
    }
    if (!response.ok) {
      const message =
        (data.error && data.error.message) ||
        data.message ||
        (typeof data.raw === "string" ? data.raw : "") ||
        `HTTP ${response.status}`;
      const error = new Error(message);
      error.status = response.status;
      error.data = data;
      throw error;
    }
    return data;
  }

  function setConn(ok, text) {
    $("#conn-dot").className = `dot ${ok ? "ok" : "error"}`;
    $("#conn-text").textContent = text || (ok ? "运行中" : "连接失败");
  }

  // ───────────────────────── 数据加载 ─────────────────────────

  async function refreshAccess() {
    const data = await api("/api/access");
    state.access = Array.isArray(data.items) ? data.items : [];
    if (Array.isArray(data.templates) && data.templates.length) state.templates = data.templates;
    state.diagnostics = Array.isArray(data.diagnostics) ? data.diagnostics : [];
  }

  async function refreshTemplates() {
    if (state.templates.length) return;
    const data = await api("/api/access/templates");
    state.templates = Array.isArray(data.items) ? data.items : [];
  }

  async function refreshPools() {
    const data = await api("/api/pools");
    state.pools = Array.isArray(data.items) ? data.items : [];
  }

  async function refreshPoolDetails() {
    const details = {};
    await Promise.all(
      state.pools.map(async (pool) => {
        try {
          const data = await api(`/api/pools/${encodeURIComponent(pool.id)}`);
          if (data.pool) details[pool.id] = data.pool;
        } catch {
          // 单个池详情失败不阻断列表。
        }
      })
    );
    state.poolDetails = details;
  }

  async function refreshPlugins() {
    const data = await api("/api/plugins");
    state.plugins = Array.isArray(data.items) ? data.items : [];
  }

  async function refreshConfig() {
    const data = await api("/api/config");
    state.config = data.config && typeof data.config === "object" ? data.config : data;
  }

  async function refreshInfo() {
    state.info = await api("/api/info");
    $("#version").textContent = `v${state.info.version || "3"}`;
    $("#listen-addr").textContent = `${state.info.host || "127.0.0.1"}:${state.info.port || 8900}`;
  }

  async function refreshAISettings() {
    try {
      const data = await api("/api/ai/settings");
      if (data.settings) state.aiSettings = data.settings;
    } catch {
      // 旧版本无该接口时保持默认。
    }
  }

  async function refreshTotals(days = 1) {
    try {
      const range = days === 1 ? "1" : `${days}d`;
      const data = await api(`/api/usage/totals?range=${range}`);
      state.totals = data.totals || {};
    } catch {
      state.totals = {};
    }
  }

  async function boot() {
    initTheme();
    bindEvents();
    try {
      await Promise.all([
        refreshInfo(),
        refreshAccess(),
        refreshPools(),
        refreshPlugins(),
        refreshConfig(),
        refreshAISettings(),
        refreshTotals(1),
      ]);
      setConn(true, "运行中");
    } catch (error) {
      setConn(false, "连接失败");
      toast(`连接失败：${error.message}`, "error");
    }
    navigate(initialView());
  }

  // ───────────────────────── 导航 / 主题 ─────────────────────────

  function initialView() {
    const allowed = [
      "overview",
      "access",
      "models",
      "routes",
      "credentials",
      "plugins",
      "traffic",
      "prompts",
      "logs",
      "free",
      "zcode",
      "settings",
      ...externalViews.keys(),
    ];
    const value = new URLSearchParams(location.search).get("view");
    return allowed.includes(value) ? value : "overview";
  }

  function navigate(view) {
    state.view = view;
    const url = new URL(location.href);
    if (view === "overview") url.searchParams.delete("view");
    else url.searchParams.set("view", view);
    history.replaceState(null, "", url);
    $$(".nav-item").forEach((button) => {
      button.classList.toggle("active", button.dataset.view === view);
    });
    $$(".view").forEach((section) => {
      section.classList.toggle("active", section.id === `view-${view}`);
    });
    const loaders = {
      overview: async () => {
        await refreshTotals(1);
        renderOverview();
      },
      access: async () => {
        await refreshAccess();
        renderAccess();
      },
      models: async () => {
        await refreshAccess();
        renderModels();
      },
      routes: async () => {
        await refreshConfig();
        renderRoutes();
      },
      credentials: async () => {
        await Promise.all([refreshPools(), refreshAccess()]);
        await refreshPoolDetails();
        renderPools();
      },
      plugins: async () => {
        await Promise.all([refreshPlugins(), refreshAccess()]);
        renderPlugins();
      },
      traffic: async () => {
        await refreshTraffic();
      },
      observe: async () => {
        await refreshObserve();
      },
      settings: async () => {
        await Promise.all([refreshConfig(), refreshTemplates(), refreshAISettings(), refreshAccess()]);
        renderSettings();
      },
    };
    const loader = externalViews.get(view) || loaders[view];
    if (loader) {
      Promise.resolve(loader()).catch((error) => toast(error.message, "error"));
    }
  }

  function registerView(name, loader) {
    externalViews.set(name, loader);
  }

  function openModal(title, sub, html) {
    $("#modal-title").textContent = title || "";
    $("#modal-sub").textContent = sub || "";
    $("#modal-body").innerHTML = html || "";
    $("#modal").hidden = false;
    $("#scrim").hidden = false;
  }

  function updateModal(title, sub, html) {
    $("#modal-title").textContent = title || "";
    $("#modal-sub").textContent = sub || "";
    $("#modal-body").innerHTML = html || "";
  }

  function initTheme() {
    const saved = localStorage.getItem("mg-v3-theme");
    document.documentElement.dataset.theme = saved === "light" ? "light" : "dark";
  }

  function toggleTheme() {
    const next = document.documentElement.dataset.theme === "light" ? "dark" : "light";
    document.documentElement.dataset.theme = next;
    localStorage.setItem("mg-v3-theme", next);
  }

  // ───────────────────────── 总览 ─────────────────────────

  function renderOverview() {
    const access = state.access;
    const totals = state.totals || {};
    const configured = access.filter((item) => item.auth && item.auth.configured).length;
    const issues = collectIssues();
    const poolAvailable = state.pools.reduce((sum, pool) => sum + Number(pool.available || 0), 0);

    $("#overview-stats").innerHTML = [
      statCard("接入", access.length, `${configured} 个已配置凭据`, "", "access"),
      statCard("今日请求", compactNum(totals.requests), `${num(totals.errors)} 次失败`, "signal", "traffic"),
      statCard("今日 Token", compactNum(totals.total_tokens), `费用 ${Number(totals.cost || 0).toFixed(4)}`, "", "traffic"),
      statCard("待处理", issues.length, issues.length ? "点击查看诊断" : "暂无异常", issues.length ? "amber" : "", "observe"),
    ].join("");
    $$("#overview-stats .stat.clickable").forEach((el) => {
      el.addEventListener("click", () => navigate(el.dataset.go));
    });
    $("#overview-meta").textContent = access.length ? `共 ${access.length} 个接入 · ${poolAvailable} 个池成员可用` : "";
    $("#overview-access").innerHTML = access.length
      ? access
          .slice()
          .sort((a, b) => (a.health.state === "ok" ? 1 : 0) - (b.health.state === "ok" ? 1 : 0))
          .map((item) => {
            const pool = item.pool ? `池 ${item.pool.available}/${item.pool.members}` : authText(item);
            return `<div class="row clickable" data-open="${esc(item.id)}">
              ${statusDot(item.health.state)}
              <div class="grow">
                <div class="name">${esc(item.label || item.id)} ${badge(item.kind)}</div>
                <div class="meta">${esc(item.protocol)} · ${esc(item.baseUrl || "—")}</div>
              </div>
              <div class="trail">${esc(pool)}</div>
              <span class="pill">${item.models ? item.models.declared : 0} 模型</span>
            </div>`;
          })
          .join("")
      : `<div class="empty-line">还没有接入。点击右上角「添加接入」开始。</div>`;
    bindRowOpeners($("#overview-access"));

    const codes = issues.slice(0, 5);
    $("#overview-todos").innerHTML = codes.length
      ? codes
          .map(
            (issue) => `<div class="row">
              <span class="pill ${issue.level === "error" ? "danger" : "amber"}">${esc(issue.level)}</span>
              <div class="grow"><div class="name">${esc(issue.title)}</div><div class="meta">${esc(issue.detail)}</div></div>
            </div>`
          )
          .join("")
      : `<div class="empty-line">当前没有阻断项。</div>`;
    $("#todo-count").textContent = String(issues.length);

    renderRecentRequests();
  }

  function statCard(label, value, sub, tone, go) {
    return `<div class="stat clickable ${tone}" data-go="${go}">
      <div class="k">${esc(label)}</div>
      <div class="v">${esc(value)}</div>
      <div class="s">${esc(sub)}</div>
    </div>`;
  }

  function collectIssues() {
    const issues = [];
    for (const item of state.access) {
      if (item.health && item.health.state === "warn") {
        issues.push({
          level: "warn",
          title: `${item.label || item.id} 待处理`,
          detail: item.auth && item.auth.mode === "none" ? "免鉴权接入，请确认可用性" : "凭据未配置或池成员不可用",
        });
      }
      if (Array.isArray(item.diagnostics)) {
        for (const diag of item.diagnostics) {
          if (diag.level === "error" || diag.level === "warn") {
            issues.push({ level: diag.level, title: `${item.id} · ${diag.code}`, detail: diag.message });
          }
        }
      }
      if (item.pool && Number(item.pool.cooling || 0) > 0) {
        issues.push({
          level: "warn",
          title: `${item.label || item.id} 有成员冷却`,
          detail: `${item.pool.cooling} 个成员冷却中，可用 ${item.pool.available}/${item.pool.members}`,
        });
      }
    }
    return issues;
  }

  async function renderRecentRequests() {
    const host = $("#overview-recent");
    host.innerHTML = `<div class="empty-line">加载中…</div>`;
    try {
      const data = await api("/api/usage/list?limit=5");
      const rows = Array.isArray(data.rows) ? data.rows : Array.isArray(data.items) ? data.items : [];
      state.recentRows = rows;
      host.innerHTML = rows.length
        ? rows
            .map(
              (row) => `<div class="row">
                <span class="status-line"><span class="dot ${row.status >= 400 ? "error" : "ok"}" style="width:7px;height:7px"></span></span>
                <div class="grow"><div class="name">${esc(row.upstream || "—")}</div><div class="meta">${esc(row.requested_model || "—")}</div></div>
                <div class="trail">${fmtMs(row.duration_ms)}<br><span class="muted">${fmtTime(row.ts)}</span></div>
              </div>`
            )
            .join("")
        : `<div class="empty-line">还没有请求记录。</div>`;
    } catch (error) {
      host.innerHTML = `<div class="empty-line">用量记录不可用：${esc(error.message)}</div>`;
    }
  }

  // ───────────────────────── 接入列表 ─────────────────────────

  function filteredAccess() {
    const query = ($("#access-search").value || "").trim().toLowerCase();
    const kind = $("#access-kind").value;
    const health = $("#access-health").value;
    return state.access.filter((item) => {
      if (kind && item.kind !== kind) return false;
      if (health && item.health.state !== health) return false;
      if (query) {
        const haystack = [item.id, item.label, item.baseUrl, item.template, item.protocol, ...(item.routing.groups || [])]
          .join(" ")
          .toLowerCase();
        if (!haystack.includes(query)) return false;
      }
      return true;
    });
  }

  function renderAccess() {
    const rows = filteredAccess();
    const tbody = $("#access-table tbody");
    tbody.innerHTML = rows
      .map((item) => {
        const pool = item.pool ? `池 ${item.pool.available}/${item.pool.members}` : "";
        const credential = item.auth.mode === "pool" ? pool || "池" : item.auth.configured ? "单 Key" : item.auth.mode === "none" ? "无需" : "待补充";
        return `<tr data-open="${esc(item.id)}">
          <td><div class="name-cell">${badge(item.kind)}<b>${esc(item.label || item.id)}</b></div><div class="tiny muted">${esc(item.id)}${item.source && item.source.pluginId ? ` · ${esc(item.source.pluginId)}` : ""}</div></td>
          <td><div>${esc(item.protocol)}</div><div class="addr" title="${esc(item.baseUrl)}">${esc(item.baseUrl || "—")}</div></td>
          <td><div>${item.models ? item.models.declared : 0} 声明</div><div class="tiny muted">${item.models ? item.models.aliases : 0} 别名</div></td>
          <td><span class="pill ${item.auth.configured ? "signal" : "amber"}">${esc(credential)}</span></td>
          <td><span class="status-line">${statusDot(item.health.state)}${esc(item.health.state === "ok" ? "正常" : "待处理")}</span></td>
          <td class="num">${num(item.usage && item.usage.todayRequests)}</td>
          <td class="right"><span class="actions">
            <button class="btn ghost sm" data-probe="${esc(item.id)}">测试</button>
            <button class="btn ghost sm" data-open="${esc(item.id)}">详情</button>
          </span></td>
        </tr>`;
      })
      .join("");
    $("#access-empty").hidden = rows.length > 0;
    $("#access-table").hidden = rows.length === 0;
    bindRowOpeners(tbody);
    $$("[data-probe]", tbody).forEach((button) => {
      button.addEventListener("click", async (event) => {
        event.stopPropagation();
        await probeAccess(button.dataset.probe);
      });
    });
  }

  function bindRowOpeners(root) {
    $$("[data-open]", root).forEach((el) => {
      el.addEventListener("click", (event) => {
        const button = event.target.closest("button");
        // 行内的操作按钮（如“测试”）不应打开抽屉；但“详情”按钮本身带
        // data-open，必须放行，否则它会因 closest("button") 被误判为操作按钮。
        if (button && button !== el && !button.hasAttribute("data-open")) return;
        openDrawer(el.dataset.open);
      });
    });
  }

  async function probeAccess(id) {
    toast(`正在测试 ${id}…`);
    try {
      const data = await api(`/api/access/${encodeURIComponent(id)}/probe`, { method: "POST" });
      if (data.ok) {
        toast(`连接成功 · ${data.latencyMs}ms · ${data.models || 0} 个模型`, "ok");
      } else {
        toast(`连接失败：${data.message || data.status}`, "error");
      }
    } catch (error) {
      toast(`测试失败：${error.message}`, "error");
    }
  }

  // ───────────────────────── 详情抽屉 ─────────────────────────

  function currentAccess() {
    return state.access.find((item) => item.id === state.drawerId) || null;
  }

  function openDrawer(id) {
    state.drawerId = id;
    state.drawerTab = "overview";
    $("#drawer").hidden = false;
    $("#scrim").hidden = false;
    renderDrawer();
  }

  function closeDrawer() {
    $("#drawer").hidden = true;
    $("#scrim").hidden = true;
    state.drawerId = null;
  }

  function renderDrawer() {
    const item = currentAccess();
    if (!item) {
      closeDrawer();
      return;
    }
    $("#drawer-name").textContent = item.label || item.id;
    $("#drawer-badges").innerHTML = [
      badge(item.kind),
      `<span class="pill">${esc(item.protocol)}</span>`,
      item.source && item.source.id ? `<span class="pill">${esc(item.source.id)}</span>` : "",
      item.health.state === "ok" ? `<span class="pill signal">正常</span>` : `<span class="pill amber">待处理</span>`,
    ].join("");
    $$(".drawer-tab").forEach((tab) => {
      tab.classList.toggle("active", tab.dataset.dtab === state.drawerTab);
    });
    const body = $("#drawer-body");
    if (state.drawerTab === "connection") body.innerHTML = drawerConnection(item);
    else if (state.drawerTab === "credential") body.innerHTML = drawerCredential(item);
    else if (state.drawerTab === "models") body.innerHTML = drawerModels(item);
    else if (state.drawerTab === "advanced") body.innerHTML = drawerAdvanced(item);
    else if (state.drawerTab === "diagnostics") body.innerHTML = drawerDiagnostics(item);
    else body.innerHTML = drawerOverview(item);
    bindDrawerActions(item);
  }

  function rawAccessConfig(item) {
    const upstreams = state.config && state.config.upstreams;
    const raw = upstreams && upstreams[item.id];
    return raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
  }

  function drawerOverview(item) {
    return `<dl class="kv">
      <dt>标识</dt><dd class="mono">${esc(item.id)}</dd>
      <dt>地址</dt><dd class="mono">${esc(item.baseUrl || "—")}</dd>
      <dt>协议</dt><dd>${esc(item.protocol)}${item.transport ? ` · transport ${esc(item.transport)}` : ""}</dd>
      <dt>模板</dt><dd>${esc(item.template || "—")}${item.templateVersion ? ` · v${item.templateVersion}` : ""}</dd>
      <dt>来源</dt><dd>${esc(item.source ? item.source.type : "—")}${item.source && item.source.pluginId ? ` · ${esc(item.source.pluginId)}` : ""}</dd>
      <dt>模型</dt><dd>${item.models ? item.models.declared : 0} 个声明 · ${item.models ? item.models.aliases : 0} 个别名</dd>
      <dt>凭据</dt><dd>${esc(authText(item))}</dd>
      <dt>隐私</dt><dd>${esc(privacyText(item.privacy))}</dd>
      <dt>路由</dt><dd>${item.routing.default ? "默认接入" : "非默认"}${item.routing.groups.length ? ` · 组 ${esc(item.routing.groups.join("、"))}` : ""}</dd>
    </dl>
    ${item.privacy && item.privacy.notice ? `<div class="callout warn">${esc(item.privacy.notice)}</div>` : ""}
    ${item.pool ? `<div class="section-title">凭据池</div>${poolSummaryHtml(item.pool)}` : ""}`;
  }

  function drawerConnection(item) {
    return `<div class="form-grid">
      <label class="field span-2"><span>展示名</span><input class="input" id="edit-label" value="${esc(item.label || item.id)}"></label>
      <label class="field"><span>协议</span>
        <select class="input" id="edit-protocol">
          ${["openai", "anthropic", "commandcode", "opencode", "zcode"]
            .map((protocol) => `<option value="${protocol}"${item.protocol === protocol ? " selected" : ""}>${protocol}</option>`)
            .join("")}
        </select>
      </label>
      <label class="field"><span>传输</span><input class="input" id="edit-transport" value="${esc(item.transport || "")}" placeholder="留空 = 按协议"></label>
      <label class="field span-2"><span>请求地址</span><input class="input mono" id="edit-baseurl" value="${esc(item.baseUrl || "")}"></label>
      <label class="check"><input type="checkbox" id="edit-zdr" ${item.privacy && item.privacy.zdr ? "checked" : ""}> 请求零数据保留（ZDR）</label>
      <label class="field"><span>训练数据策略</span>
        <select class="input" id="edit-training">
          <option value="inherit"${!item.privacy || item.privacy.training === "inherit" ? " selected" : ""}>跟随模板</option>
          <option value="deny"${item.privacy && item.privacy.training === "deny" ? " selected" : ""}>禁止训练</option>
          <option value="allow"${item.privacy && item.privacy.training === "allow" ? " selected" : ""}>允许训练</option>
        </select>
      </label>
      <div class="inline-note span-2">${esc(privacyText(item.privacy))}。不支持请求级开关的厂商只会记录意图，不会宣称已保护。</div>
    </div>
    <div class="modal-actions"><button class="btn primary sm" id="save-connection">保存连接</button></div>`;
  }

  function drawerCredential(item) {
    const pool = item.pool;
    const poolOptions = state.pools
      .map((entry) => `<option value="${esc(entry.id)}"${item.auth.poolId === entry.id ? " selected" : ""}>${esc(entry.id)} · ${esc(entry.upstream)}</option>`)
      .join("");
    return `<dl class="kv">
      <dt>模式</dt><dd>${esc(item.auth.mode)}</dd>
      <dt>来源</dt><dd>${esc(item.auth.source || "—")}</dd>
      <dt>引用</dt><dd class="mono">${esc(item.auth.secretRef || "—")}</dd>
      <dt>状态</dt><dd>${item.auth.hasSecret ? "已配置（不可回显）" : item.auth.mode === "none" ? "无需密钥" : "待补充"}</dd>
    </dl>
    ${pool ? `<div class="section-title">池统计</div>${poolSummaryHtml(pool)}` : ""}
    <div class="section-title">写入 / 替换密钥</div>
    <div class="form-grid">
      <label class="field span-2"><span>API Key（只写，不回显）</span>
        <input class="input secret-input" id="secret-input" type="password" autocomplete="new-password" placeholder="粘贴密钥后保存">
      </label>
    </div>
    <div class="modal-actions"><button class="btn primary sm" id="save-secret">保存到本机加密库</button></div>
    <div class="section-title">绑定凭据池</div>
    <div class="form-grid">
      <label class="field span-2"><span>凭据池</span>
        <select class="input" id="bind-pool">
          <option value="">不绑定（使用单 Key）</option>
          ${poolOptions}
        </select>
      </label>
    </div>
    <div class="modal-actions"><button class="btn ghost sm" id="save-pool-binding">保存绑定</button></div>`;
  }

  function drawerModels(item) {
    const raw = rawAccessConfig(item);
    const models = Array.isArray(raw.models) ? raw.models.join("\n") : "";
    const aliases = raw.modelMap && typeof raw.modelMap === "object" ? raw.modelMap : {};
    const overrides = raw.modelOverrides && typeof raw.modelOverrides === "object" ? raw.modelOverrides : {};
    return `<div class="section-title">模型声明</div>
      <div class="inline-note">每行一个模型 id；留空时由上游目录自动拉取。这里的声明会参与 /v1/models 与路由匹配。</div>
      <label class="field"><span>模型列表</span><textarea class="input code-area" id="edit-models" rows="5" spellcheck="false" placeholder="deepseek-chat&#10;deepseek-reasoner">${esc(models)}</textarea></label>
      <div class="form-grid">
        <label class="field"><span>模型别名映射（JSON）</span><textarea class="input code-area" id="edit-model-map" rows="5" spellcheck="false" placeholder='{"gpt-4o-mini":"deepseek-chat"}'>${esc(Object.keys(aliases).length ? JSON.stringify(aliases, null, 2) : "")}</textarea></label>
        <label class="field"><span>模型级覆盖（JSON）</span><textarea class="input code-area" id="edit-model-overrides" rows="5" spellcheck="false" placeholder='{"deepseek-*":{"limits":{"maxOutputTokens":65536}}}'>${esc(Object.keys(overrides).length ? JSON.stringify(overrides, null, 2) : "")}</textarea></label>
      </div>
      <div class="section-title">目录行为</div>
      <div class="check-grid">
        <label class="check"><input type="checkbox" id="edit-fetch-models" ${raw.fetchModels === true ? "checked" : ""}> 从上游拉取模型目录</label>
        <label class="check"><input type="checkbox" id="edit-auto-models" ${raw.autoModels === true ? "checked" : ""}> 自动合并拉取结果</label>
        <label class="check"><input type="checkbox" id="edit-pause-turn" ${raw.pauseTurn === true ? "checked" : ""}> 暂停轮次兼容</label>
        <label class="check"><input type="checkbox" id="edit-default" ${raw.default === true ? "checked" : ""}> 设为默认接入</label>
      </div>
      <div class="inline-note">别名和覆盖必须是 JSON 对象；留空会清除该项。密钥与模型级密钥请在「凭据」页或凭据池维护。</div>
      <div class="modal-actions"><button class="btn primary sm" id="save-models">保存模型与目录</button></div>`;
  }

  function drawerAdvanced(item) {
    const raw = rawAccessConfig(item);
    const compat = raw.compat && typeof raw.compat === "object" ? raw.compat : {};
    const timeout = raw.timeoutSeconds == null ? "" : raw.timeoutSeconds;
    return `<div class="section-title">兼容配置</div>
      <div class="inline-note">高级项按 JSON 覆盖 upstream.compat；留空表示跟随模板预设。修改前请确认上游协议；错误配置会直接保存到 bridge.config.json（保存时自动备份）。</div>
      <label class="field"><span>compat JSON</span><textarea class="input code-area" id="edit-compat" rows="16" spellcheck="false">${esc(Object.keys(compat).length ? JSON.stringify(compat, null, 2) : "")}</textarea></label>
      <div class="form-grid">
        <label class="field"><span>上游超时（秒，留空跟随全局）</span><input class="input" id="edit-timeout" type="number" min="1" value="${esc(timeout)}"></label>
        <label class="check" style="align-self:end"><input type="checkbox" id="edit-session-header" ${raw.sessionHeader === true ? "checked" : ""}> 注入会话头（如上游支持）</label>
      </div>
      <div class="callout warn">请勿在 compat.headers 中粘贴长期密钥；密钥应写入「凭据」页的本机加密库。此页只编辑非敏感兼容参数。</div>
      <div class="modal-actions"><button class="btn primary sm" id="save-advanced">保存高级配置</button></div>`;
  }

  function drawerDiagnostics(item) {
    const rows = Array.isArray(item.diagnostics) ? item.diagnostics : [];
    if (!rows.length) return `<div class="callout">没有该接入的诊断项。</div>`;
    return `<div class="list compact">${rows
      .map(
        (diag) => `<div class="row">
          <span class="pill ${diag.level === "error" ? "danger" : diag.level === "warn" ? "amber" : ""}">${esc(diag.level)}</span>
          <div class="grow"><div class="name">${esc(diag.code)}</div><div class="meta">${esc(diag.message)}</div>${diag.hint ? `<div class="tiny muted">${esc(diag.hint)}</div>` : ""}</div>
        </div>`
      )
      .join("")}</div>`;
  }

  function poolSummaryHtml(pool) {
    return `<div class="tagline">
      <span class="pill ${pool.available ? "signal" : "amber"}">可用 ${num(pool.available)}/${num(pool.members)}</span>
      ${pool.cooling ? `<span class="pill amber">冷却 ${num(pool.cooling)}</span>` : ""}
      ${pool.disabled ? `<span class="pill">停用 ${num(pool.disabled)}</span>` : ""}
      ${pool.exhausted ? `<span class="pill danger">配额耗尽 ${num(pool.exhausted)}</span>` : ""}
      <span class="pill">${esc(STRATEGY_LABEL[pool.strategy] || pool.strategy)}</span>
    </div>`;
  }

  function bindDrawerActions(item) {
    const saveConnection = $("#save-connection");
    if (saveConnection) {
      saveConnection.addEventListener("click", async () => {
        try {
          await api(`/api/access/${encodeURIComponent(item.id)}`, {
            method: "PATCH",
            body: JSON.stringify({
              label: $("#edit-label").value.trim(),
              protocol: $("#edit-protocol").value,
              transport: $("#edit-transport").value.trim(),
              baseUrl: $("#edit-baseurl").value.trim(),
              privacy: { zdr: $("#edit-zdr").checked, training: $("#edit-training").value },
            }),
          });
          toast("连接已保存", "ok");
          await refreshAccess();
          renderDrawer();
          renderAccess();
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
    const saveSecret = $("#save-secret");
    if (saveSecret) {
      saveSecret.addEventListener("click", async () => {
        const value = $("#secret-input").value.trim();
        if (!value) {
          toast("请先粘贴密钥", "error");
          return;
        }
        saveSecret.disabled = true;
        try {
          await api(`/api/access/${encodeURIComponent(item.id)}/secret`, {
            method: "POST",
            body: JSON.stringify({ apiKey: value }),
          });
          $("#secret-input").value = "";
          toast("密钥已写入本机加密库", "ok");
          await refreshAccess();
          renderDrawer();
          renderAccess();
        } catch (error) {
          toast(error.message, "error");
        } finally {
          saveSecret.disabled = false;
        }
      });
    }
    const saveBinding = $("#save-pool-binding");
    if (saveBinding) {
      saveBinding.addEventListener("click", async () => {
        const poolId = $("#bind-pool").value;
        try {
          await api(`/api/access/${encodeURIComponent(item.id)}`, {
            method: "PATCH",
            body: JSON.stringify({ credentialPool: poolId }),
          });
          toast(poolId ? "已绑定凭据池" : "已解除池绑定", "ok");
          await refreshAccess();
          renderDrawer();
          renderAccess();
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
    const saveModels = $("#save-models");
    if (saveModels) {
      saveModels.addEventListener("click", async () => {
        const parseObject = (selector, label) => {
          const text = $(selector).value.trim();
          if (!text) return {};
          try {
            const parsed = JSON.parse(text);
            if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
              throw new Error(`${label}必须是 JSON 对象`);
            }
            return parsed;
          } catch (error) {
            throw new Error(`${label}：${error.message}`);
          }
        };
        try {
          const models = $("#edit-models")
            .value.split(/[\n,，]+/)
            .map((model) => model.trim())
            .filter(Boolean);
          const modelMap = parseObject("#edit-model-map", "模型别名映射");
          const modelOverrides = parseObject("#edit-model-overrides", "模型级覆盖");
          await api(`/api/access/${encodeURIComponent(item.id)}`, {
            method: "PATCH",
            body: JSON.stringify({
              models,
              modelMap,
              modelOverrides,
              fetchModels: $("#edit-fetch-models").checked,
              autoModels: $("#edit-auto-models").checked,
              pauseTurn: $("#edit-pause-turn").checked,
              default: $("#edit-default").checked,
            }),
          });
          await Promise.all([refreshAccess(), refreshConfig()]);
          renderDrawer();
          renderAccess();
          toast("模型与目录设置已保存", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
    const saveAdvanced = $("#save-advanced");
    if (saveAdvanced) {
      saveAdvanced.addEventListener("click", async () => {
        const text = $("#edit-compat").value.trim();
        let compat = null;
        if (text) {
          try {
            compat = JSON.parse(text);
          } catch (error) {
            toast(`compat JSON 解析失败：${error.message}`, "error");
            return;
          }
          if (!compat || typeof compat !== "object" || Array.isArray(compat)) {
            toast("compat 必须是 JSON 对象", "error");
            return;
          }
        }
        const timeoutText = $("#edit-timeout").value.trim();
        const timeoutSeconds = timeoutText ? Number(timeoutText) : null;
        if (timeoutText && (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 1)) {
          toast("上游超时必须是正整数", "error");
          return;
        }
        try {
          await api(`/api/access/${encodeURIComponent(item.id)}`, {
            method: "PATCH",
            body: JSON.stringify({
              compat,
              timeoutSeconds,
              sessionHeader: $("#edit-session-header").checked,
            }),
          });
          await Promise.all([refreshAccess(), refreshConfig()]);
          renderDrawer();
          renderAccess();
          toast("高级兼容配置已保存", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
  }

  // ───────────────────────── 模型目录 / 路由组 ─────────────────────────

  function renderModels() {
    const tbody = $("#models-table tbody");
    tbody.innerHTML = state.access
      .map(
        (item) => `<tr>
          <td><div class="name-cell">${badge(item.kind)}<b>${esc(item.label || item.id)}</b></div></td>
          <td>${esc(item.protocol)}</td>
          <td>${item.models ? item.models.declared : 0}</td>
          <td>${item.models ? item.models.aliases : 0}</td>
          <td class="muted tiny">${esc((item.models && item.models.sample) || "在「模型与定价」页查看完整清单")}</td>
          <td class="right"><button class="btn ghost sm" data-open="${esc(item.id)}">查看接入</button></td>
        </tr>`
      )
      .join("");
    bindRowOpeners(tbody);
  }

  function renderRoutes() {
    const config = state.config || {};
    const unified = config.unified || {};
    const visible = Array.isArray(unified.visibleUpstreams) ? unified.visibleUpstreams : [];
    const groups = Array.isArray(unified.groups) ? unified.groups : [];
    const routeGroups = Array.isArray(config.groups)
      ? config.groups
      : config.groups && typeof config.groups === "object"
        ? Object.entries(config.groups).map(([id, members]) => ({ id, members }))
        : [];
    $("#unified-count").textContent = `${visible.length || state.access.length} 个可见`;
    $("#unified-list").innerHTML = visible.length
      ? visible.map((name) => `<div class="row"><span class="dot ok" style="width:7px;height:7px"></span><div class="grow">${esc(name)}</div></div>`).join("")
      : `<div class="empty-line">未设置可见白名单：统一入口展示全部上游。</div>`;
    $("#group-count").textContent = `${routeGroups.length} 组`;
    $("#group-list").innerHTML = routeGroups.length
      ? routeGroups
          .map(
            (group) => `<div class="row">
              <div class="grow"><div class="name">group/${esc(group.id)}</div><div class="meta">${esc((group.members || []).join(" → "))}</div></div>
            </div>`
          )
          .join("")
      : `<div class="empty-line">尚未配置路由组。可在「设置 → 路由组」维护。</div>`;
    if (groups.length) {
      $("#unified-list").innerHTML += groups
        .map(
          (group) => `<div class="row"><span class="pill">分组</span><div class="grow"><div class="name">${esc(group.label || group.id)}</div><div class="meta">${esc((group.upstreams || []).join("、"))}</div></div></div>`
        )
        .join("");
    }
  }

  // ───────────────────────── 凭据池 ─────────────────────────

  function renderPools() {
    const host = $("#pool-list");
    if (!state.pools.length) {
      host.innerHTML = `<div class="card"><div class="empty">
        <div class="empty-mark">⚿</div><h3>还没有凭据池</h3>
        <p>多 Key 或账号型接入可创建池；池成员密钥只写本机加密库。</p>
        <button class="btn primary" id="pool-add-empty">+ 新建凭据池</button>
      </div></div>`;
      const button = $("#pool-add-empty");
      if (button) button.addEventListener("click", openPoolModal);
      return;
    }
    host.innerHTML = state.pools
      .map((pool) => {
        const detail = state.poolDetails[pool.id];
        const members = detail && Array.isArray(detail.memberList) ? detail.memberList : [];
        return `<div class="card">
          <div class="card-head">
            <div>
              <h2>${esc(pool.id)}</h2>
              <div class="tiny muted">绑定上游 ${esc(pool.upstream)} · ${esc(STRATEGY_LABEL[pool.strategy] || pool.strategy)}</div>
            </div>
            <span class="pill ${pool.status === "active" ? "signal" : ""}">${esc(pool.status || "active")}</span>
          </div>
          ${poolSummaryHtml(pool)}
          <div class="hr"></div>
          ${members.length
            ? `<div class="table-wrap"><table class="table">
                <thead><tr><th>成员</th><th>类型</th><th>状态</th><th class="num">优先级</th><th>最近使用</th><th class="right">操作</th></tr></thead>
                <tbody>${members
                  .map(
                    (member) => `<tr>
                      <td>${esc(member.label)}</td>
                      <td class="tiny">${esc(member.kind)}</td>
                      <td><span class="pill ${
                        member.status === "active" ? "signal" : member.cooling ? "amber" : member.status === "exhausted" ? "danger" : ""
                      }">${esc(STATUS_LABEL[member.status] || member.status)}</span></td>
                      <td class="num">${num(member.priority)}</td>
                      <td class="tiny muted">${fmtTime(member.lastUsedAt)}</td>
                      <td class="right"><span class="actions">
                        <button class="btn ghost sm" data-member-toggle="${esc(pool.id)}|${esc(member.id)}|${member.status === "disabled" ? "active" : "disabled"}">${member.status === "disabled" ? "启用" : "停用"}</button>
                        <button class="btn danger sm" data-member-delete="${esc(pool.id)}|${esc(member.id)}">删除</button>
                      </span></td>
                    </tr>`
                  )
                  .join("")}</tbody>
              </table></div>`
            : `<div class="empty-line">池里还没有成员。添加成员时只填标签与密钥。</div>`}
          <div class="card-actions wrap" style="margin-top:12px">
            <button class="btn primary sm" data-member-add="${esc(pool.id)}">+ 添加成员</button>
            <button class="btn ghost sm" data-pool-probe="${esc(pool.id)}">探测全部</button>
            <button class="btn ghost sm" data-pool-policy="${esc(pool.id)}">池设置</button>
          </div>
        </div>`;
      })
      .join("");
    $$("[data-member-add]").forEach((button) => button.addEventListener("click", () => openMemberModal(button.dataset.memberAdd)));
    $$("[data-pool-probe]").forEach((button) => button.addEventListener("click", () => probePool(button.dataset.poolProbe)));
    $$("[data-pool-policy]").forEach((button) => button.addEventListener("click", () => openPoolPolicyModal(button.dataset.poolPolicy)));
    $$("[data-member-toggle]").forEach((button) =>
      button.addEventListener("click", async () => {
        const [poolId, memberId, status] = button.dataset.memberToggle.split("|");
        try {
          await api(`/api/pools/${encodeURIComponent(poolId)}/members/${encodeURIComponent(memberId)}`, {
            method: "PATCH",
            body: JSON.stringify({ status }),
          });
          await refreshPoolDetails();
          renderPools();
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
    $$("[data-member-delete]").forEach((button) =>
      button.addEventListener("click", async () => {
        const [poolId, memberId] = button.dataset.memberDelete.split("|");
        if (!confirm(`删除成员 ${memberId}？密钥默认保留在本机加密库。`)) return;
        try {
          await api(`/api/pools/${encodeURIComponent(poolId)}/members/${encodeURIComponent(memberId)}`, { method: "DELETE" });
          await refreshPoolDetails();
          renderPools();
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
  }

  async function probePool(id) {
    toast(`正在探测 ${id}…`);
    try {
      const data = await api(`/api/pools/${encodeURIComponent(id)}/probe`, { method: "POST" });
      const members = Array.isArray(data.members) ? data.members : [];
      const ok = members.filter((member) => member.ok).length;
      toast(`${id}：${ok}/${members.length} 个成员可用`, ok ? "ok" : "error");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  // ───────────────────────── 插件 ─────────────────────────

  function renderPlugins() {
    const host = $("#plugin-list");
    host.innerHTML = state.plugins
      .map((plugin) => {
        const attached = Array.isArray(plugin.attached) ? plugin.attached : [];
        const mode = plugin.detail && plugin.detail.auth ? plugin.detail.auth.mode : "";
        return `<div class="card">
          <div class="card-head">
            <div>
              <h2>${esc(plugin.name || plugin.id)}</h2>
              <div class="tiny muted mono">${esc(plugin.id)} · v${esc(plugin.version)}</div>
            </div>
            <span class="pill ${plugin.enabled ? "signal" : ""}">${plugin.enabled ? "启用" : "停用"}</span>
          </div>
          <div class="tagline">
            ${badge(plugin.kind === "free" ? "free" : plugin.kind === "account" ? "account" : "plugin", plugin.kind)}
            <span class="pill">${esc(plugin.tier)}</span>
            ${mode ? `<span class="pill">auth ${esc(mode)}</span>` : ""}
            ${plugin.builtin ? `<span class="pill info">内置</span>` : `<span class="pill">已安装</span>`}
          </div>
          <p class="hint" style="margin:10px 0">${esc(plugin.description || "未提供描述。")}</p>
          ${attached.length ? `<div class="inline-note">已接入：${esc(attached.join("、"))}</div>` : ""}
          <div class="card-actions wrap">
            <button class="btn primary sm" data-plugin-attach="${esc(plugin.id)}">接入</button>
            ${attached.length ? `<button class="btn ghost sm" data-plugin-detach="${esc(plugin.id)}" data-name="${esc(attached[0])}">解绑</button>` : ""}
            <button class="btn ghost sm" data-plugin-toggle="${esc(plugin.id)}" data-enabled="${plugin.enabled ? "0" : "1"}">${plugin.enabled ? "禁用" : "启用"}</button>
            ${plugin.builtin ? "" : `<button class="btn danger sm" data-plugin-remove="${esc(plugin.id)}">卸载</button>`}
          </div>
        </div>`;
      })
      .join("");
    $$("[data-plugin-attach]").forEach((button) =>
      button.addEventListener("click", () => attachPlugin(button.dataset.pluginAttach))
    );
    $$("[data-plugin-detach]").forEach((button) =>
      button.addEventListener("click", () => detachPlugin(button.dataset.pluginDetach, button.dataset.name))
    );
    $$("[data-plugin-toggle]").forEach((button) =>
      button.addEventListener("click", async () => {
        try {
          await api(`/api/plugins/${encodeURIComponent(button.dataset.pluginToggle)}`, {
            method: "PATCH",
            body: JSON.stringify({ enabled: button.dataset.enabled === "1" }),
          });
          await refreshPlugins();
          renderPlugins();
          toast("插件状态已更新", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
    $$("[data-plugin-remove]").forEach((button) =>
      button.addEventListener("click", async () => {
        if (!confirm("卸载插件？已创建的上游与凭据会保留。")) return;
        try {
          await api(`/api/plugins/${encodeURIComponent(button.dataset.pluginRemove)}`, { method: "DELETE" });
          await refreshPlugins();
          renderPlugins();
          toast("插件已卸载", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
  }

  async function attachPlugin(id) {
    const plugin = state.plugins.find((item) => item.id === id);
    const suggested = id.split(".").pop() || id;
    const name = prompt(`接入「${plugin ? plugin.name : id}」的上游标识`, suggested);
    if (!name) return;
    try {
      const data = await api(`/api/plugins/${encodeURIComponent(id)}/attach`, {
        method: "POST",
        body: JSON.stringify({ name: name.trim() }),
      });
      toast(data.needsSecret ? "已接入；请到接入详情补充密钥" : "已接入", data.needsSecret ? "" : "ok");
      await Promise.all([refreshAccess(), refreshPlugins()]);
      renderPlugins();
      if (data.result && data.result.access) openDrawer(data.result.access.id);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function detachPlugin(id, name) {
    if (!name) {
      const value = prompt("要解绑哪个上游？");
      if (!value) return;
      name = value;
    }
    if (!confirm(`解绑 ${name}？默认保留上游与凭据。`)) return;
    try {
      await api(`/api/plugins/${encodeURIComponent(id)}/detach`, {
        method: "POST",
        body: JSON.stringify({ name, purge: false }),
      });
      await Promise.all([refreshAccess(), refreshPlugins()]);
      renderPlugins();
      toast("已解绑插件来源", "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  // ───────────────────────── 流量 / 观测 ─────────────────────────

  async function refreshTraffic() {
    const range = state.trafficDays === 1 ? "1" : `${state.trafficDays}d`;
    const [totals, upstreams, usage, logs] = await Promise.all([
      api(`/api/usage/totals?range=${range}`).catch(() => ({ totals: {} })),
      api(`/api/usage/upstreams?range=${range}&limit=20`).catch(() => ({})),
      api(`/api/usage/list?range=${range}&limit=50`).catch(() => ({ rows: [] })),
      api(`/api/logs?range=${range}&limit=50`).catch(() => ({})),
    ]);
    state.totals = totals.totals || {};
    state.usageRows = Array.isArray(usage.rows) ? usage.rows : Array.isArray(usage.items) ? usage.items : [];
    state.logs = Array.isArray(logs.logs) ? logs.logs : Array.isArray(logs.items) ? logs.items : Array.isArray(logs.rows) ? logs.rows : [];
    renderTraffic(upstreams);
  }

  function renderTraffic(upstreams) {
    const totals = state.totals || {};
    $("#traffic-stats").innerHTML = [
      statCard("请求", num(totals.requests), `成功 ${num(totals.ok)}`, "signal", ""),
      statCard("Token", compactNum(totals.total_tokens), `缓存读 ${compactNum(totals.cache_read_tokens)}`, "", ""),
      statCard("失败", num(totals.errors), "4xx / 5xx", Number(totals.errors) ? "danger" : "", ""),
      statCard("P95", fmtMs(totals.p95_duration_ms || totals.p95), `平均 ${fmtMs(totals.avg_duration_ms)}`, "", ""),
    ].join("");
    const rows = Array.isArray(upstreams.rows)
      ? upstreams.rows
      : Array.isArray(upstreams.upstreams)
        ? upstreams.upstreams
        : Array.isArray(upstreams.items)
          ? upstreams.items
          : [];
    $("#traffic-upstreams tbody").innerHTML = rows.length
      ? rows
          .map(
            (row) => `<tr><td>${esc(row.upstream || row.name || "—")}</td><td class="num">${num(row.requests)}</td><td class="num">${compactNum(row.total_tokens)}</td><td class="num">${num(row.errors)}</td></tr>`
          )
          .join("")
      : `<tr><td colspan="4" class="muted">暂无用量。</td></tr>`;
    $("#traffic-logs tbody").innerHTML = state.logs.length
      ? state.logs
          .slice(0, 50)
          .map(
            (row) => `<tr>
              <td class="tiny muted">${fmtTime(row.ts)}</td>
              <td>${esc(row.upstream || "—")}</td>
              <td class="tiny">${esc(row.requested_model || row.model || "—")}</td>
              <td><span class="pill ${Number(row.status) >= 400 ? "danger" : "signal"}">${esc(row.status || "—")}</span></td>
              <td class="num">${fmtMs(row.duration_ms)}</td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="5" class="muted">暂无请求日志。</td></tr>`;
  }

  async function refreshObserve() {
    await refreshAccess();
    const [health, prompts] = await Promise.all([
      api("/api/upstreams/health").catch(() => ({})),
      api("/api/prompts?limit=20").catch(() => ({})),
    ]);
    state.health = Array.isArray(health.upstreams) ? health.upstreams : Array.isArray(health.items) ? health.items : [];
    state.prompts = Array.isArray(prompts.items) ? prompts.items : Array.isArray(prompts.prompts) ? prompts.prompts : Array.isArray(prompts.rows) ? prompts.rows : [];
    renderObserve();
  }

  function renderObserve() {
    const diagnostics = collectIssues();
    $("#diag-count").textContent = String(diagnostics.length);
    $("#diagnostic-list").innerHTML = diagnostics.length
      ? diagnostics
          .map(
            (diag) => `<div class="row">
              <span class="pill ${diag.level === "error" ? "danger" : "amber"}">${esc(diag.level)}</span>
              <div class="grow"><div class="name">${esc(diag.title)}</div><div class="meta">${esc(diag.detail)}</div></div>
            </div>`
          )
          .join("")
      : `<div class="empty-line">最近没有 error / warn。</div>`;
    $("#prompt-list").innerHTML = state.prompts.length
      ? state.prompts
          .map(
            (prompt) => `<div class="row">
              <div class="grow">
                <div class="name">${esc(prompt.preview || prompt.requested_model || "记录")}</div>
                <div class="meta">${esc(prompt.requested_model || "—")} · ${fmtTime(prompt.ts)}</div>
              </div>
              <span class="pill">${num(prompt.message_count)} 条</span>
            </div>`
          )
          .join("")
      : `<div class="empty-line">未开启提示词记录，或暂无记录。</div>`;
  }

  // ───────────────────────── 设置 ─────────────────────────

  function renderSettings() {
    if (window.MG_SETTINGS && typeof window.MG_SETTINGS.render === "function") {
      window.MG_SETTINGS.render();
      return;
    }
    const config = state.config || {};
    $("#set-host").value = config.host || "127.0.0.1";
    $("#set-port").value = config.port || 8900;
    $("#set-default-model").value = config.defaultModel || "";
    $("#set-timeout").value = config.timeoutSeconds || 600;
    $("#set-models-cache").value = config.modelsCacheSeconds || 300;
    $("#set-access-log").checked = config.accessLog !== false;
    $("#set-prompt-capture").checked = Boolean(config.promptCapture);
    $("#set-ai-enabled").checked = state.aiSettings.enabled !== false;
    $("#set-ai-apply").checked = Boolean(state.aiSettings.allowApply);
    $("#set-ai-pool").checked = state.aiSettings.allowPoolStats !== false;
    $("#template-count").textContent = `${state.templates.length}`;
    $("#template-list").innerHTML = state.templates
      .slice(0, 10)
      .map(
        (template) => `<div class="row">
          <div class="grow"><div class="name">${esc(template.label || template.id)}</div><div class="meta">${esc(template.id)} · ${esc(template.kind)}</div></div>
          <span class="pill ${template.kind === "official" ? "signal" : ""}">${esc(template.kind)}</span>
        </div>`
      )
      .join("");
  }

  async function saveSettings() {
    if (window.MG_SETTINGS && typeof window.MG_SETTINGS.save === "function") {
      window.MG_SETTINGS.save();
      return;
    }
    const config = state.config || {};
    const next = {
      ...config,
      host: $("#set-host").value.trim(),
      port: Number($("#set-port").value),
      defaultModel: $("#set-default-model").value.trim(),
      timeoutSeconds: Number($("#set-timeout").value),
      modelsCacheSeconds: Number($("#set-models-cache").value),
      accessLog: $("#set-access-log").checked,
      promptCapture: $("#set-prompt-capture").checked,
    };
    try {
      await api("/api/config", { method: "POST", body: JSON.stringify({ config: next, backup: true }) });
      state.config = next;
      await api("/api/ai/settings", {
        method: "PUT",
        body: JSON.stringify({
          settings: {
            enabled: $("#set-ai-enabled").checked,
            allowApply: $("#set-ai-apply").checked,
            allowPoolStats: $("#set-ai-pool").checked,
          },
        }),
      });
      await refreshAISettings();
      toast("设置已保存并热重载", "ok");
      try {
        await refreshInfo();
      } catch {
        // 端口变化后旧连接可能立即断开，属预期。
      }
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function showBackups() {
    try {
      const data = await api("/api/config/backups");
      const items = Array.isArray(data.backups) ? data.backups : Array.isArray(data.items) ? data.items : [];
      openInfoModal(
        "配置备份",
        items.length
          ? `<div class="list compact">${items
              .map(
                (item) => `<div class="row"><div class="grow"><div class="name mono">${esc(item.path || item.name || item)}</div><div class="meta">${esc(item.createdAt || item.modifiedAt || "")}</div></div></div>`
              )
              .join("")}</div>`
          : `<div class="empty-line">还没有备份。首次保存会自动生成一份。</div>`
      );
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function showManifest() {
    try {
      const data = await api("/api/ai/manifest");
      openInfoModal("AI manifest", `<pre class="mono tiny" style="white-space:pre-wrap">${esc(JSON.stringify(data, null, 2))}</pre>`);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function showAudit() {
    try {
      const data = await api("/api/ai/audit");
      const items = Array.isArray(data.items) ? data.items : [];
      openInfoModal(
        "AI 配置审计",
        items.length
          ? `<div class="table-wrap"><table class="table"><thead><tr><th>时间</th><th>actor</th><th>动作</th><th>目标</th><th>结果</th></tr></thead><tbody>${items
              .map(
                (item) => `<tr><td class="tiny">${esc(item.at)}</td><td>${esc(item.actor || "")}</td><td>${esc(item.action || "")}</td><td class="mono tiny">${esc(item.target || "")}</td><td>${esc(item.result || "")}</td></tr>`
              )
              .join("")}</tbody></table></div>`
          : `<div class="empty-line">还没有审计记录。</div>`
      );
    } catch (error) {
      toast(error.message, "error");
    }
  }

  // ───────────────────────── 快速添加 ─────────────────────────

  function openAddModal(mode = "official") {
    state.addMode = mode;
    state.addStep = 1;
    state.addTemplate = null;
    state.addResult = null;
    $("#modal").hidden = false;
    $("#scrim").hidden = false;
    renderAddModal();
  }

  function closeModal() {
    $("#modal").hidden = true;
    $("#scrim").hidden = true;
  }

  function officialTemplates() {
    return state.templates.filter((template) => template.kind === "official");
  }

  function renderAddModal() {
    const title = $("#modal-title");
    const sub = $("#modal-sub");
    const body = $("#modal-body");
    if (state.addResult) {
      title.textContent = "添加完成";
      sub.textContent = "接入已创建；密钥不会回显。";
      body.innerHTML = renderAddResult();
      bindAddResult();
      return;
    }
    title.textContent = state.addMode === "official" ? "添加官方厂商" : "添加自定义 / 中转";
    sub.textContent =
      state.addMode === "official"
        ? "选模板 → 填 Key。其余默认值已由模板填好。"
        : "只需 5 项：标识、厂商、请求地址、协议、Key。";
    body.innerHTML =
      state.addMode === "official"
        ? state.addStep === 1
          ? renderTemplateStep()
          : renderCredentialStep()
        : renderCustomStep();
    bindAddModal();
  }

  function renderTemplateStep() {
    const templates = officialTemplates();
    return `<div class="stepper">
        <span class="step active"><span class="n">1</span>选择厂商</span><span class="line"></span>
        <span class="step"><span class="n">2</span>填写凭据</span><span class="line"></span>
        <span class="step"><span class="n">3</span>完成</span>
      </div>
      <div class="form-grid">
        <label class="field span-2"><span>搜索厂商</span><input class="input" id="template-search" placeholder="DeepSeek / OpenAI / Anthropic …"></label>
      </div>
      <div class="template-grid" id="template-grid">
        ${templates
          .map(
            (template) => `<button class="template-card ${state.addTemplate === template.id ? "active" : ""}" data-template="${esc(template.id)}">
              <span class="t">${esc(template.label || template.id)}</span>
              <span class="m">${esc(template.defaults.protocol)} · ${esc(template.defaults.baseUrl || "自定义地址")}</span>
              <span class="d">${esc(template.description || "")}</span>
              <span class="d">隐私：${esc(SUPPORT_LABEL[template.privacy.support] || template.privacy.support)}</span>
            </button>`
          )
          .join("")}
      </div>
      <div class="modal-actions">
        <button class="btn ghost" data-add-mode="custom">改为自定义 / 中转</button>
        <div class="spacer"></div>
        <button class="btn ghost" id="modal-cancel">取消</button>
        <button class="btn primary" id="template-next" ${state.addTemplate ? "" : "disabled"}>下一步</button>
      </div>`;
  }

  function selectedTemplate() {
    return state.templates.find((template) => template.id === state.addTemplate) || null;
  }

  function renderCredentialStep() {
    const template = selectedTemplate();
    if (!template) return renderTemplateStep();
    const support = template.privacy.support;
    return `<div class="stepper">
        <span class="step"><span class="n">1</span>选择厂商</span><span class="line"></span>
        <span class="step active"><span class="n">2</span>填写凭据</span><span class="line"></span>
        <span class="step"><span class="n">3</span>完成</span>
      </div>
      <div class="callout">已选 <b>${esc(template.label)}</b> · 协议 ${esc(template.defaults.protocol)} · 地址 <span class="mono">${esc(template.defaults.baseUrl)}</span></div>
      <div class="form-grid" style="margin-top:14px">
        <label class="field"><span>上游标识</span><input class="input" id="quick-name" value="${esc(template.id)}"></label>
        <label class="field"><span>模板</span><input class="input" value="${esc(template.id)}" disabled></label>
        <label class="field span-2"><span>API Key ${template.requiredFields.includes("apiKey") ? "（必填）" : "（可留空）"}</span>
          <input class="input secret-input" id="quick-key" type="password" autocomplete="new-password" placeholder="粘贴后只写入本机加密库">
        </label>
        <label class="check span-2"><input type="checkbox" id="quick-zdr" ${template.privacy.zdrDefault ? "checked" : ""}>
          请求零数据保留（ZDR） · 该厂商：${esc(SUPPORT_LABEL[support] || support)}</label>
        <div class="inline-note span-2">${
          support === "request"
            ? "保存后按厂商能力发送 ZDR 标记；不会伪造不支持的头。"
            : support === "account"
              ? "该厂商只能在工作区/控制台开启；此处仅记录意图。"
              : "该厂商不支持请求级 ZDR；勾选仅记录意图。"
        }</div>
      </div>
      <div class="modal-actions">
        <button class="btn ghost" id="quick-back">上一步</button>
        <div class="spacer"></div>
        <button class="btn ghost" id="modal-cancel">取消</button>
        <button class="btn primary" id="quick-submit">添加并测试</button>
      </div>`;
  }

  function renderCustomStep() {
    return `<div class="form-grid">
        <label class="field"><span>上游标识（必填）</span><input class="input" id="custom-name" placeholder="my-gateway"></label>
        <label class="field"><span>厂商 / 备注</span><input class="input" id="custom-vendor" placeholder="自定义"></label>
        <label class="field span-2"><span>请求地址（必填）</span><input class="input mono" id="custom-baseurl" placeholder="https://gateway.example.com/v1"></label>
        <label class="field"><span>协议</span>
          <select class="input" id="custom-protocol">
            <option value="openai">openai</option>
            <option value="anthropic">anthropic</option>
            <option value="commandcode">commandcode</option>
            <option value="opencode">opencode</option>
          </select>
        </label>
        <label class="field"><span>API Key（必填）</span><input class="input secret-input" id="custom-key" type="password" autocomplete="new-password"></label>
        <label class="check span-2"><input type="checkbox" id="custom-zdr" checked> 请求零数据保留（ZDR）</label>
        <div class="inline-note span-2">保存前会校验地址与重名；模型目录拉取失败不阻断保存，之后可重试。</div>
      </div>
      <div class="modal-actions">
        <button class="btn ghost" data-add-mode="official">改为官方模板</button>
        <div class="spacer"></div>
        <button class="btn ghost" id="modal-cancel">取消</button>
        <button class="btn primary" id="custom-submit">添加并测试</button>
      </div>`;
  }

  function bindAddModal() {
    $$("[data-template]").forEach((button) => {
      button.addEventListener("click", () => {
        state.addTemplate = button.dataset.template;
        renderAddModal();
      });
    });
    const search = $("#template-search");
    if (search) {
      search.addEventListener("input", () => {
        const query = search.value.trim().toLowerCase();
        $$("#template-grid .template-card").forEach((card) => {
          card.hidden = Boolean(query) && !card.textContent.toLowerCase().includes(query);
        });
      });
      search.focus();
    }
    const next = $("#template-next");
    if (next) {
      next.addEventListener("click", () => {
        if (!state.addTemplate) return;
        state.addStep = 2;
        renderAddModal();
      });
    }
    const back = $("#quick-back");
    if (back) {
      back.addEventListener("click", () => {
        state.addStep = 1;
        renderAddModal();
      });
    }
    const submit = $("#quick-submit");
    if (submit) submit.addEventListener("click", submitQuickAdd);
    const customSubmit = $("#custom-submit");
    if (customSubmit) customSubmit.addEventListener("click", submitCustomAdd);
    $$("[data-add-mode]").forEach((button) => {
      button.addEventListener("click", () => {
        state.addMode = button.dataset.addMode;
        state.addStep = 1;
        state.addTemplate = null;
        renderAddModal();
      });
    });
    const cancel = $("#modal-cancel");
    if (cancel) cancel.addEventListener("click", closeModal);
  }

  async function submitQuickAdd() {
    const template = selectedTemplate();
    if (!template) return;
    const name = $("#quick-name").value.trim();
    const apiKey = $("#quick-key").value;
    if (!name) {
      toast("请填写上游标识", "error");
      return;
    }
    if (template.requiredFields.includes("apiKey") && !apiKey.trim()) {
      toast("请填写 API Key", "error");
      return;
    }
    const button = $("#quick-submit");
    button.disabled = true;
    try {
      const data = await api("/api/access/quick", {
        method: "POST",
        body: JSON.stringify({
          templateId: template.id,
          name,
          apiKey,
          privacy: { zdr: $("#quick-zdr").checked },
        }),
      });
      state.addResult = { access: data.access, probe: null };
      try {
        const probe = await api(`/api/access/${encodeURIComponent(name)}/probe`, { method: "POST" });
        state.addResult.probe = probe;
      } catch (error) {
        state.addResult.probe = { ok: false, message: error.message };
      }
      await refreshAccess();
      renderAddModal();
    } catch (error) {
      toast(error.message, "error");
    } finally {
      button.disabled = false;
    }
  }

  async function submitCustomAdd() {
    const name = $("#custom-name").value.trim();
    const baseUrl = $("#custom-baseurl").value.trim();
    const apiKey = $("#custom-key").value;
    if (!name || !baseUrl || !apiKey.trim()) {
      toast("请填写标识、请求地址与 API Key", "error");
      return;
    }
    const button = $("#custom-submit");
    button.disabled = true;
    try {
      const data = await api("/api/access/custom", {
        method: "POST",
        body: JSON.stringify({
          name,
          vendor: $("#custom-vendor").value.trim(),
          baseUrl,
          protocol: $("#custom-protocol").value,
          apiKey,
          privacy: { zdr: $("#custom-zdr").checked },
        }),
      });
      state.addResult = { access: data.access, probe: null };
      try {
        const probe = await api(`/api/access/${encodeURIComponent(name)}/probe`, { method: "POST" });
        state.addResult.probe = probe;
      } catch (error) {
        state.addResult.probe = { ok: false, message: error.message };
      }
      await refreshAccess();
      renderAddModal();
    } catch (error) {
      toast(error.message, "error");
    } finally {
      button.disabled = false;
    }
  }

  function renderAddResult() {
    const access = state.addResult.access || {};
    const probe = state.addResult.probe;
    return `<div class="callout ${probe && probe.ok ? "" : "warn"}">
        ${probe && probe.ok ? `连接成功 · ${probe.latencyMs}ms` : `已保存，但测试未通过：${esc(probe ? probe.message : "未测试")}`}
      </div>
      <dl class="kv" style="margin-top:14px">
        <dt>接入</dt><dd><b>${esc(access.label || access.id)}</b> <span class="mono tiny">${esc(access.id)}</span></dd>
        <dt>协议</dt><dd>${esc(access.protocol)}</dd>
        <dt>地址</dt><dd class="mono">${esc(access.baseUrl)}</dd>
        <dt>凭据</dt><dd>${esc(authText(access))}</dd>
        <dt>隐私</dt><dd>${esc(privacyText(access.privacy))}</dd>
      </dl>
      <div class="modal-actions">
        <button class="btn ghost" id="add-again">再添加一个</button>
        <button class="btn primary" id="add-done">完成</button>
      </div>`;
  }

  function bindAddResult() {
    const done = $("#add-done");
    if (done) {
      done.addEventListener("click", () => {
        closeModal();
        navigate("access");
      });
    }
    const again = $("#add-again");
    if (again) {
      again.addEventListener("click", () => {
        state.addResult = null;
        state.addStep = 1;
        state.addTemplate = null;
        renderAddModal();
      });
    }
  }

  // ───────────────────────── 凭据池弹窗 ─────────────────────────

  function openPoolModal() {
    const options = state.access
      .map((item) => `<option value="${esc(item.id)}">${esc(item.label || item.id)} · ${esc(item.protocol)}</option>`)
      .join("");
    if (!options) {
      toast("请先添加一个接入", "error");
      return;
    }
    openInfoModal(
      "新建凭据池",
      `<div class="form-grid">
        <label class="field span-2"><span>绑定上游</span><select class="input" id="new-pool-upstream">${options}</select></label>
        <label class="field"><span>策略</span><select class="input" id="new-pool-strategy">
          <option value="priority">优先级（默认）</option><option value="round_robin">轮询</option>
          <option value="weighted">加权</option><option value="least_used">最少使用</option>
        </select></label>
        <label class="field"><span>池 id（可留空）</span><input class="input mono" id="new-pool-id" placeholder="pool_my_gateway"></label>
        <label class="field"><span>冷却秒数</span><input class="input" id="new-pool-cooldown" type="number" value="60"></label>
        <label class="field"><span>失败阈值</span><input class="input" id="new-pool-threshold" type="number" value="3"></label>
      </div>
      <div class="modal-actions"><button class="btn primary" id="create-pool-submit">创建</button></div>`
    );
    const submit = $("#create-pool-submit");
    if (submit) {
      submit.addEventListener("click", async () => {
        try {
          await api("/api/pools", {
            method: "POST",
            body: JSON.stringify({
              upstream: $("#new-pool-upstream").value,
              strategy: $("#new-pool-strategy").value,
              id: $("#new-pool-id").value.trim() || undefined,
              cooldownSeconds: Number($("#new-pool-cooldown").value) || 60,
              failureThreshold: Number($("#new-pool-threshold").value) || 3,
            }),
          });
          closeModal();
          await Promise.all([refreshPools(), refreshAccess()]);
          await refreshPoolDetails();
          renderPools();
          toast("凭据池已创建", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
  }

  function openMemberModal(poolId) {
    openInfoModal(
      `添加成员 · ${poolId}`,
      `<div class="form-grid">
        <label class="field"><span>标签</span><input class="input" id="member-label" placeholder="主账号 / 备用 Key"></label>
        <label class="field"><span>类型</span><select class="input" id="member-kind"><option value="api_key">api_key</option><option value="account">account</option><option value="oauth">oauth</option></select></label>
        <label class="field span-2"><span>密钥（只写）</span><input class="input secret-input" id="member-key" type="password" autocomplete="new-password"></label>
        <label class="field"><span>优先级（小优先）</span><input class="input" id="member-priority" type="number" value="100"></label>
        <label class="field"><span>权重</span><input class="input" id="member-weight" type="number" value="1"></label>
      </div>
      <div class="modal-actions"><button class="btn primary" id="create-member-submit">添加</button></div>`
    );
    const submit = $("#create-member-submit");
    if (submit) {
      submit.addEventListener("click", async () => {
        const apiKey = $("#member-key").value;
        if (!apiKey.trim()) {
          toast("请填写密钥", "error");
          return;
        }
        try {
          await api(`/api/pools/${encodeURIComponent(poolId)}/members`, {
            method: "POST",
            body: JSON.stringify({
              label: $("#member-label").value.trim() || "成员",
              kind: $("#member-kind").value,
              apiKey,
              priority: Number($("#member-priority").value) || 100,
              weight: Number($("#member-weight").value) || 1,
            }),
          });
          closeModal();
          await Promise.all([refreshPools(), refreshAccess()]);
          await refreshPoolDetails();
          renderPools();
          toast("成员已添加", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
  }

  function openPoolPolicyModal(poolId) {
    const pool = state.pools.find((item) => item.id === poolId);
    if (!pool) return;
    openInfoModal(
      `池设置 · ${poolId}`,
      `<div class="form-grid">
        <label class="field"><span>策略</span><select class="input" id="policy-strategy">
          ${Object.entries(STRATEGY_LABEL).map(([value, label]) => `<option value="${value}"${pool.strategy === value ? " selected" : ""}>${label}</option>`).join("")}
        </select></label>
        <label class="field"><span>状态</span><select class="input" id="policy-status">
          <option value="active"${pool.status === "active" ? " selected" : ""}>启用</option>
          <option value="disabled"${pool.status === "disabled" ? " selected" : ""}>停用</option>
        </select></label>
        <label class="field"><span>冷却秒数</span><input class="input" id="policy-cooldown" type="number" value="${Number(pool.cooldownSeconds || 60)}"></label>
        <label class="field"><span>失败阈值</span><input class="input" id="policy-threshold" type="number" value="${Number(pool.failureThreshold || 3)}"></label>
        <label class="check span-2"><input type="checkbox" id="policy-fallback" ${pool.fallbackToSingle ? "checked" : ""}> 池不可用时允许回落单 Key（默认关闭）</label>
      </div>
      <div class="modal-actions"><button class="btn primary" id="policy-submit">保存</button></div>`
    );
    const submit = $("#policy-submit");
    if (submit) {
      submit.addEventListener("click", async () => {
        try {
          await api(`/api/pools/${encodeURIComponent(poolId)}`, {
            method: "PATCH",
            body: JSON.stringify({
              strategy: $("#policy-strategy").value,
              status: $("#policy-status").value,
              cooldownSeconds: Number($("#policy-cooldown").value) || 60,
              failureThreshold: Number($("#policy-threshold").value) || 3,
              fallbackToSingle: $("#policy-fallback").checked,
            }),
          });
          closeModal();
          await Promise.all([refreshPools(), refreshAccess()]);
          await refreshPoolDetails();
          renderPools();
          toast("池设置已保存", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
  }

  // ───────────────────────── 通用模态框 / 插件安装 ─────────────────────────

  function openInfoModal(title, html) {
    $("#modal-title").textContent = title;
    $("#modal-sub").textContent = "";
    $("#modal-body").innerHTML = html;
    $("#modal").hidden = false;
    $("#scrim").hidden = false;
    const cancel = $("#modal-cancel");
    if (cancel) cancel.addEventListener("click", closeModal);
  }

  function openInstallPluginModal() {
    openInfoModal(
      "安装本地插件",
      `<div class="inline-note">只接受 manifest（modelgate.plugin/v1）。安装只登记，不修改配置、不执行 DDL。</div>
      <label class="field"><span>manifest JSON</span>
        <textarea class="input mono" id="plugin-manifest" rows="14" style="width:100%;resize:vertical" placeholder='{"schema":"modelgate.plugin/v1","id":"example.provider","kind":"provider","tier":"declarative","auth":{"mode":"single"},"upstream":{"protocol":"openai","baseUrl":"https://api.example.com/v1"}}'></textarea>
      </label>
      <div class="modal-actions"><button class="btn primary" id="install-plugin-submit">校验并登记</button></div>`
    );
    const submit = $("#install-plugin-submit");
    if (submit) {
      submit.addEventListener("click", async () => {
        let manifest;
        try {
          manifest = JSON.parse($("#plugin-manifest").value);
        } catch {
          toast("manifest 不是合法 JSON", "error");
          return;
        }
        try {
          await api("/api/plugins/install", { method: "POST", body: JSON.stringify({ manifest }) });
          closeModal();
          await refreshPlugins();
          renderPlugins();
          toast("插件已登记；接入请点击「接入」", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
  }

  // ───────────────────────── 事件绑定 ─────────────────────────

  function bindEvents() {
    $("#nav").addEventListener("click", (event) => {
      const button = event.target.closest(".nav-item");
      if (button) navigate(button.dataset.view);
    });
    $$("[data-go]").forEach((button) => {
      if (button.closest("#overview-stats")) return;
      button.addEventListener("click", () => navigate(button.dataset.go));
    });
    $("#theme-toggle").addEventListener("click", toggleTheme);
    $("#config-help-btn").addEventListener("click", () => window.MG_HELP && window.MG_HELP());
    $("#update-btn").addEventListener("click", () => window.MG_UPDATE && window.MG_UPDATE());
    $("#quick-add").addEventListener("click", () => openAddModal("official"));
    $("#access-add").addEventListener("click", () => openAddModal("official"));
    $$("[data-add]").forEach((button) => button.addEventListener("click", () => openAddModal(button.dataset.add)));
    $("#quick-add").addEventListener("click", () => openAddModal("official"));

    $("#overview-refresh").addEventListener("click", async () => {
      await Promise.all([refreshAccess(), refreshPools(), refreshTotals(1)]);
      renderOverview();
    });
    $("#overview-probe").addEventListener("click", async () => {
      const host = $("#health-results");
      host.hidden = false;
      host.innerHTML = `<span class="spin"></span> 正在逐个测试…`;
      const results = await Promise.all(
        state.access.map(async (item) => {
          try {
            return { id: item.id, ...(await api(`/api/access/${encodeURIComponent(item.id)}/probe`, { method: "POST" })) };
          } catch (error) {
            return { id: item.id, ok: false, message: error.message };
          }
        })
      );
      host.innerHTML = results
        .map(
          (result) => `<div class="row"><span class="pill ${result.ok ? "signal" : "danger"}">${result.ok ? "成功" : "失败"}</span>
            <div class="grow"><div class="name">${esc(result.id)}</div><div class="meta">${esc(result.ok ? `${result.latencyMs}ms` : result.message)}</div></div></div>`
        )
        .join("");
    });

    $("#access-refresh").addEventListener("click", async () => {
      await refreshAccess();
      renderAccess();
    });
    $("#access-search").addEventListener("input", renderAccess);
    $("#access-kind").addEventListener("change", renderAccess);
    $("#access-health").addEventListener("change", renderAccess);

    $("#routes-refresh").addEventListener("click", async () => {
      await refreshConfig();
      renderRoutes();
    });

    $("#pools-refresh").addEventListener("click", async () => {
      await Promise.all([refreshPools(), refreshAccess()]);
      await refreshPoolDetails();
      renderPools();
    });
    $("#pool-add").addEventListener("click", openPoolModal);

    $("#plugins-refresh").addEventListener("click", async () => {
      await refreshPlugins();
      renderPlugins();
    });
    $("#plugin-install-open").addEventListener("click", openInstallPluginModal);

    $("#settings-save").addEventListener("click", saveSettings);
    $("#settings-backups").addEventListener("click", showBackups);
    $("#ai-manifest").addEventListener("click", showManifest);
    $("#ai-audit").addEventListener("click", showAudit);

    $("#drawer-close").addEventListener("click", closeDrawer);
    $("#scrim").addEventListener("click", () => {
      closeDrawer();
      closeModal();
    });
    $$(".drawer-tab").forEach((tab) => {
      tab.addEventListener("click", () => {
        state.drawerTab = tab.dataset.dtab;
        renderDrawer();
      });
    });
    $("#drawer-probe").addEventListener("click", async () => {
      if (state.drawerId) await probeAccess(state.drawerId);
    });
    $("#drawer-delete").addEventListener("click", async () => {
      if (!state.drawerId) return;
      const name = state.drawerId;
      if (!confirm(`删除接入「${name}」？默认保留密钥；输入名称无法撤销。`)) return;
      try {
        await api(`/api/access/${encodeURIComponent(name)}`, { method: "DELETE" });
        closeDrawer();
        await refreshAccess();
        renderAccess();
        toast("接入已删除", "ok");
      } catch (error) {
        toast(error.message, "error");
      }
    });
    $("#modal-close").addEventListener("click", closeModal);

    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        if (!$("#modal").hidden) closeModal();
        else if (!$("#drawer").hidden) closeDrawer();
      }
      if (event.key.toLowerCase() === "n" && !event.ctrlKey && !event.metaKey && !isTyping(event.target)) {
        openAddModal("official");
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        navigate("access");
        $("#access-search").focus();
      }
    });
  }

  function isTyping(target) {
    return target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT");
  }

  // 供后续页面模块复用同一套状态、请求、转义和弹窗。
  window.MG = {
    $,
    $$,
    api,
    esc,
    num,
    compactNum,
    fmtTime,
    fmtMs,
    badge,
    statusDot,
    privacyText,
    authText,
    toast,
    state,
    registerView,
    navigate,
    openModal,
    updateModal,
    closeModal,
    setConn,
    refreshAccess,
    refreshTemplates,
    refreshPools,
    refreshPoolDetails,
    refreshPlugins,
    refreshConfig,
    refreshInfo,
    refreshAISettings,
    openAddModal,
    KIND_LABEL,
    STRATEGY_LABEL,
    SUPPORT_LABEL,
    STATUS_LABEL,
  };

  document.addEventListener("DOMContentLoaded", boot);
})();
