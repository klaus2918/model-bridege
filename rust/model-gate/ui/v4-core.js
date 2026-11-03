/* Model-Gate v4 控制台内核（原生 JS，无构建步骤）。
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
    view: "usage",
    access: [],
    templates: [],
    pools: [],
    poolDetails: {},
    plugins: [],
    pluginLibrary: [],
    accessTab: "list",
    config: null,
    info: null,
    aiSettings: { enabled: true, allowApply: false, allowPoolStats: true },
    drawerId: null,
    drawerTab: "overview",
    addMode: "official",
    addStep: 1,
    addTemplate: null,
    addResult: null,
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
    const k = health === "ok" ? "ok" : health === "error" ? "error" : health === "disabled" ? "idle" : health === "idle" ? "idle" : "warn";
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
    state.pluginLibrary = Array.isArray(data.library) ? data.library : [];
  }

  async function refreshConfig() {
    const data = await api("/api/config");
    state.config = data.config && typeof data.config === "object" ? data.config : data;
  }

  async function refreshInfo() {
    state.info = await api("/api/info");
    $("#version").textContent = `v${state.info.version || "—"}`;
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

  async function boot() {
    initTheme();
    renderThemeUI();
    bindEvents();
    try {
      await Promise.all([
        refreshInfo(),
        refreshAccess(),
        refreshPools(),
        refreshPlugins(),
        refreshConfig(),
        refreshAISettings(),
      ]);
      setConn(true, "运行中");
    } catch (error) {
      setConn(false, "连接失败");
      toast(`连接失败：${error.message}`, "error");
    }
    navigate(initialView());
  }

  // ───────────────────────── 导航 / 主题 ─────────────────────────

  // 旧地址兼容：v4.1 及以前的视图名 → 新 IA（用量/会话/接入/插件/设置）。
  const LEGACY_VIEWS = {
    overview: "usage",
    traffic: "usage",
    prompts: "sessions",
    models: "access",
    credentials: "access",
    free: "access",
    templates: "access",
    routes: "settings",
    logs: "settings",
    config: "settings",
  };

  function initialView() {
    const allowed = [
      "usage",
      "sessions",
      "access",
      "plugins",
      "settings",
      ...externalViews.keys(),
    ];
    const value = new URLSearchParams(location.search).get("view");
    if (allowed.includes(value)) return value;
    if (LEGACY_VIEWS[value]) return LEGACY_VIEWS[value];
    return "usage";
  }

  const viewLoaders = {
    access: async () => {
      await refreshAccess();
      renderAccess();
      runAccessPane();
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
    settings: async () => {
      await Promise.all([refreshConfig(), refreshTemplates(), refreshAISettings(), refreshAccess()]);
      renderSettings();
      renderRoutes();
      renderThemeUI();
    },
  };

  // 接入页次级页签的懒加载：切到哪个 pane 就刷新哪个模块的数据。
  function runAccessPane() {
    const map = { pools: "credentials", models: "models", free: "free", templates: "templates" };
    const name = map[state.accessTab || "list"];
    if (!name) return;
    const loader = externalViews.get(name) || viewLoaders[name];
    if (loader) Promise.resolve(loader()).catch((error) => toast(error.message, "error"));
  }

  function runView(name) {
    const loader = externalViews.get(name) || viewLoaders[name];
    if (loader) Promise.resolve(loader()).catch((error) => toast(error.message, "error"));
  }

  function navigate(view) {
    state.view = view;
    const url = new URL(location.href);
    if (view === "usage") url.searchParams.delete("view");
    else url.searchParams.set("view", view);
    history.replaceState(null, "", url);
    $$(".nav-item").forEach((button) => {
      button.classList.toggle("active", button.dataset.view === view);
    });
    $$(".view").forEach((section) => {
      section.classList.toggle("active", section.id === `view-${view}`);
    });
    const loader = externalViews.get(view) || viewLoaders[view];
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

  // ───────────────────────── 主题配色 ─────────────────────────
  // 与 v4-base.css 的 html[data-theme] 块一一对应；swatch 取 [bg, surface, signal] 做预览。

  const THEMES = [
    { id: "dark", label: "石墨琥珀", mode: "深色", swatch: ["#0b0d10", "#12151a", "#f0a020"] },
    { id: "light", label: "冷灰白", mode: "浅色", swatch: ["#f2f3f5", "#ffffff", "#b26a00"] },
    { id: "ocean", label: "海洋蓝", mode: "深色", swatch: ["#0a0e14", "#101820", "#3ba7e8"] },
    { id: "emerald", label: "翡翠绿", mode: "深色", swatch: ["#0a0f0c", "#111813", "#43c878"] },
    { id: "dusk", label: "暮紫", mode: "深色", swatch: ["#0e0b15", "#151022", "#a78bfa"] },
    { id: "paper", label: "暖纸", mode: "浅色", swatch: ["#f6f2ea", "#fffdf9", "#b26a00"] },
  ];

  function currentTheme() {
    const id = document.documentElement.dataset.theme;
    return THEMES.some((theme) => theme.id === id) ? id : "dark";
  }

  function initTheme() {
    const saved = localStorage.getItem("mg-v4-theme") || localStorage.getItem("mg-v3-theme");
    document.documentElement.dataset.theme = THEMES.some((theme) => theme.id === saved) ? saved : "dark";
  }

  function setTheme(id) {
    if (!THEMES.some((theme) => theme.id === id) || id === currentTheme()) return;
    document.documentElement.dataset.theme = id;
    localStorage.setItem("mg-v4-theme", id);
    renderThemeUI();
    toast(`主题已切换：${THEMES.find((theme) => theme.id === id).label}`, "ok");
  }

  function themeItemHtml(theme, current) {
    const active = theme.id === current;
    return `<button class="theme-item${active ? " active" : ""}" data-theme-pick="${theme.id}" role="option" aria-selected="${active}">
      <span class="theme-swatches">${theme.swatch.map((color) => `<i style="background:${color}"></i>`).join("")}</span>
      <span>${esc(theme.label)}</span>
      <span class="tiny muted">${esc(theme.mode)}</span>
      ${active ? '<span class="theme-check">✓</span>' : ""}
    </button>`;
  }

  function renderThemeUI() {
    const current = currentTheme();
    const menu = $("#theme-menu");
    if (menu) menu.innerHTML = THEMES.map((theme) => themeItemHtml(theme, current)).join("");
    const cards = $("#theme-cards");
    if (cards) {
      cards.innerHTML = THEMES.map(
        (theme) => `<button class="theme-card${theme.id === current ? " active" : ""}" data-theme-pick="${theme.id}">
          <span class="theme-preview" style="background:${theme.swatch[0]}"><i class="soft" style="background:${theme.swatch[1]}"></i><i style="background:${theme.swatch[2]}"></i></span>
          <span class="theme-name">${esc(theme.label)}${theme.id === current ? '<span class="theme-check">✓</span>' : ""}</span>
          <span class="theme-mode">${esc(theme.mode)}</span>
        </button>`
      ).join("");
    }
  }

  function toggleThemeMenu() {
    const menu = $("#theme-menu");
    if (!menu) return;
    if (menu.hidden) {
      renderThemeUI();
      menu.hidden = false;
    } else {
      menu.hidden = true;
    }
  }

  function closeThemeMenu(event) {
    const menu = $("#theme-menu");
    if (!menu || menu.hidden) return;
    if (event && (menu.contains(event.target) || event.target.closest("#theme-toggle"))) return;
    menu.hidden = true;
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
    const host = $("#access-cards");
    if (!host) return;
    const rows = filteredAccess();
    // 异常置顶：待处理 → 已停用 → 正常。
    const weight = (item) => (item.health.state === "ok" ? 2 : item.health.state === "disabled" ? 1 : 0);
    const sorted = [...rows].sort((a, b) => weight(a) - weight(b));
    host.innerHTML = sorted
      .map((item) => {
        const pool = item.pool ? `池 ${item.pool.available}/${item.pool.members}` : "";
        const credential = item.auth.mode === "pool" ? pool || "池" : item.auth.configured ? "单 Key" : item.auth.mode === "none" ? "无需" : "待补充";
        const disabled = item.enabled === false;
        const healthLabel = item.health.state === "ok" ? "正常" : item.health.state === "disabled" ? "已停用" : "待处理";
        const healthClass = item.health.state === "ok" ? "ok" : item.health.state === "disabled" ? "" : "warn";
        return `<article class="acc-card${disabled ? " off" : ""}${item.health.state !== "ok" ? " attn" : ""}" data-open="${esc(item.id)}">
          <div class="acc-r1">
            <b>${esc(item.label || item.id)}</b>
            ${badge(item.kind)}
            <span class="status-line ${healthClass}">${statusDot(item.health.state)}${esc(healthLabel)}</span>
            ${disabled ? '<span class="pill">已停用</span>' : ""}
            <span class="grow"></span>
            <span class="acc-today">${num(item.usage && item.usage.todayRequests)} <i>今日</i></span>
          </div>
          <div class="acc-r2 mono" title="${esc(item.baseUrl || "")}">${esc(item.protocol || "—")} · ${esc(item.baseUrl || "—")}</div>
          <div class="acc-r3">
            <span class="pill ${item.auth.configured ? "signal" : "amber"}">${esc(credential)}</span>
            <span class="pill">${item.models ? item.models.declared : 0} 模型</span>
            ${item.models && item.models.aliases ? `<span class="pill">${item.models.aliases} 别名</span>` : ""}
            ${item.source && item.source.pluginId ? `<span class="pill">插件 ${esc(item.source.pluginId)}</span>` : ""}
            <span class="tiny muted mono">${esc(item.id)}</span>
          </div>
          <div class="acc-r4">
            <button class="btn ghost sm" data-probe="${esc(item.id)}">测试</button>
            <button class="btn ghost sm" data-toggle-enabled="${esc(item.id)}">${disabled ? "启用" : "停用"}</button>
            <button class="btn ghost sm" data-open="${esc(item.id)}">详情</button>
          </div>
        </article>`;
      })
      .join("");
    $("#access-empty").hidden = rows.length > 0;
    bindRowOpeners(host);
    $$("[data-probe]", host).forEach((button) => {
      button.addEventListener("click", async (event) => {
        event.stopPropagation();
        await probeAccess(button.dataset.probe);
      });
    });
    $$("[data-toggle-enabled]", host).forEach((button) => {
      button.addEventListener("click", async (event) => {
        event.stopPropagation();
        const id = button.dataset.toggleEnabled;
        const item = state.access.find((row) => row.id === id);
        const next = !(item && item.enabled !== false);
        button.disabled = true;
        try {
          await api(`/api/access/${encodeURIComponent(id)}`, {
            method: "PATCH",
            body: JSON.stringify({ enabled: next }),
          });
          toast(next ? `已启用 ${id}` : `已停用 ${id}`, "ok");
          await Promise.all([refreshAccess(), refreshConfig().catch(() => {})]);
          renderDrawer();
          renderAccess();
        } catch (error) {
          toast(error.message, "error");
          button.disabled = false;
        }
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
      item.enabled === false
        ? `<span class="pill">已停用</span>`
        : item.health.state === "ok"
          ? `<span class="pill signal">正常</span>`
          : `<span class="pill amber">待处理</span>`,
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
    return `${item.enabled === false
      ? `<div class="callout warn">该接入已停用：不参与路由、模型清单、健康检查，总览也不再统计它的请求。</div>`
      : ""}
    <dl class="kv">
      <dt>状态</dt><dd>${item.enabled === false ? "已停用" : "启用中"}</dd>
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
    <div class="modal-actions"><button class="btn ghost sm" id="toggle-enabled">${item.enabled === false ? "启用该接入" : "停用该接入"}</button></div>
    ${item.privacy && item.privacy.notice ? `<div class="callout warn">${esc(item.privacy.notice)}</div>` : ""}
    ${item.pool ? `<div class="section-title">凭据池</div>${poolSummaryHtml(item.pool)}` : ""}`;
  }

  function drawerConnection(item) {
    return `<div class="form-grid">
      <label class="field span-2"><span>展示名</span><input class="input" id="edit-label" value="${esc(item.label || item.id)}"></label>
      <label class="field"><span>协议</span>
        <select class="input" id="edit-protocol">
          ${["openai", "anthropic", "commandcode", "opencode"]
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
      <div class="modal-actions" style="margin:0 0 8px">
        <button class="btn ghost sm" id="fetch-upstream-models">⇣ 查询清单</button>
        <span class="hint" id="fetch-models-meta"></span>
      </div>
      <div id="upstream-models-result" hidden style="margin-bottom:12px"></div>
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

  // ───────────────────────── 抽屉：查询上游模型清单 ─────────────────────────
  // 后端 /api/upstreams/{name}/models 是 transport 感知的（opencode 指纹头、
  // commandcode 本地 models.md、普通 OpenAI 目录各走各的）；多选结果只写入
  // 表单（替换/追加），点「保存模型与目录」才落盘。

  function renderFetchedModels(item) {
    const host = $("#upstream-models-result");
    const meta = $("#fetch-models-meta");
    const fetched = state.fetchedModels;
    if (!host || !meta || !fetched || fetched.id !== item.id) return;
    const declared = $("#edit-models")
      .value.split(/[\n,，]+/)
      .map((model) => model.trim())
      .filter(Boolean);
    if (!fetched.models.length) {
      host.hidden = true;
      meta.textContent = "上游返回了空清单：检查目录端点或改用手动声明。";
      return;
    }
    host.hidden = false;
    meta.textContent = `拉取到 ${fetched.models.length} 个可用模型 · 已生效的默认勾选`;
    host.innerHTML = `
      <div class="toolbar">
        <input class="input search" id="fetched-model-search" placeholder="检索模型 id">
        <button class="btn ghost sm" id="fetched-select-all">全选</button>
        <button class="btn ghost sm" id="fetched-clear">取消选择</button>
      </div>
      <div class="check-grid" id="fetched-model-grid">
        ${fetched.models
          .map(
            (model) =>
              `<label class="check"><input type="checkbox" data-fetched-model="${esc(model)}" ${declared.includes(model) ? "checked" : ""}> <span class="mono tiny">${esc(model)}</span></label>`
          )
          .join("")}
      </div>
      <div class="modal-actions">
        <button class="btn primary sm" id="replace-fetched-models">替换清单</button>
        <button class="btn ghost sm" id="append-fetched-models">追加到清单</button>
        <span class="hint">只改下方表单；点「保存模型与目录」才写入配置。</span>
      </div>`;
    const pickedModels = () =>
      $$("#fetched-model-grid input:checked").map((input) => input.dataset.fetchedModel);
    const currentModels = () =>
      $("#edit-models")
        .value.split(/[\n,，]+/)
        .map((model) => model.trim())
        .filter(Boolean);
    $("#fetched-model-search").addEventListener("input", (event) => {
      const query = event.target.value.trim().toLowerCase();
      $$("#fetched-model-grid label").forEach((label) => {
        const id = (label.querySelector("input")?.dataset.fetchedModel || "").toLowerCase();
        label.style.display = !query || id.includes(query) ? "" : "none";
      });
    });
    $("#fetched-select-all").addEventListener("click", () => {
      $$("#fetched-model-grid input").forEach((input) => {
        input.checked = true;
      });
    });
    $("#fetched-clear").addEventListener("click", () => {
      $$("#fetched-model-grid input").forEach((input) => {
        input.checked = false;
      });
    });
    $("#replace-fetched-models").addEventListener("click", () => {
      const picked = pickedModels();
      if (!picked.length) {
        toast("先勾选要写入的模型", "error");
        return;
      }
      $("#edit-models").value = picked.join("\n");
      toast(`已用 ${picked.length} 个模型替换清单，记得点「保存模型与目录」`, "ok");
    });
    $("#append-fetched-models").addEventListener("click", () => {
      const picked = pickedModels();
      if (!picked.length) {
        toast("先勾选要追加的模型", "error");
        return;
      }
      const current = currentModels();
      const added = picked.filter((model) => !current.includes(model));
      $("#edit-models").value = current.concat(added).join("\n");
      toast(added.length ? `已追加 ${added.length} 个模型，记得点「保存模型与目录」` : "勾选的模型都已在清单里", added.length ? "ok" : "");
    });
  }

  function bindDrawerActions(item) {
    // 模型页：从上游拉取可用清单（GET /api/upstreams/{name}/models，transport 感知）。
    if (state.fetchedModels && state.fetchedModels.id === item.id && $("#upstream-models-result")) {
      renderFetchedModels(item);
    }
    const fetchUpstreamModels = $("#fetch-upstream-models");
    if (fetchUpstreamModels) {
      fetchUpstreamModels.addEventListener("click", async () => {
        const meta = $("#fetch-models-meta");
        const host = $("#upstream-models-result");
        if (!meta || !host) return;
        fetchUpstreamModels.disabled = true;
        host.hidden = true;
        meta.textContent = "正在向上游查询清单…";
        try {
          const data = await api(`/api/upstreams/${encodeURIComponent(item.id)}/models`);
          state.fetchedModels = { id: item.id, models: Array.isArray(data.models) ? data.models : [] };
          renderFetchedModels(item);
        } catch (error) {
          state.fetchedModels = null;
          meta.textContent = `查询失败：${error.message}`;
        } finally {
          fetchUpstreamModels.disabled = false;
        }
      });
    }
    const toggleEnabled = $("#toggle-enabled");
    if (toggleEnabled) {
      toggleEnabled.addEventListener("click", async () => {
        const next = item.enabled === false;
        toggleEnabled.disabled = true;
        try {
          await api(`/api/access/${encodeURIComponent(item.id)}`, {
            method: "PATCH",
            body: JSON.stringify({ enabled: next }),
          });
          toast(next ? `已启用 ${item.id}` : `已停用 ${item.id}`, "ok");
          await Promise.all([refreshAccess(), refreshConfig().catch(() => {})]);
          renderDrawer();
          renderAccess();
        } catch (error) {
          toast(error.message, "error");
          toggleEnabled.disabled = false;
        }
      });
    }
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

  function upstreamNameList() {
    const config = state.config || {};
    const names = new Set(state.access.map((item) => item.id));
    if (config.upstreams && typeof config.upstreams === "object") {
      Object.keys(config.upstreams).forEach((name) => names.add(name));
    }
    return [...names].sort();
  }

  function routeGroupPairs() {
    const groups = state.config?.groups;
    if (Array.isArray(groups)) {
      return groups.map((group) => ({
        id: String(group.id || group.name || ""),
        members: Array.isArray(group.members) ? group.members.map(String) : [],
      })).filter((group) => group.id);
    }
    if (groups && typeof groups === "object") {
      return Object.entries(groups).map(([id, members]) => ({
        id,
        members: Array.isArray(members) ? members.map(String) : [],
      }));
    }
    return [];
  }

  function routeModelCount(name) {
    const item = state.access.find((row) => row.id === name);
    return Number(item?.models?.declared || 0);
  }

  function cleanRouteConfig(value) {
    if (Array.isArray(value)) return value.map(cleanRouteConfig);
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value)
          .filter(([key]) => !key.startsWith("_") && value[key] !== undefined)
          .map(([key, item]) => [key, cleanRouteConfig(item)])
      );
    }
    return value;
  }

  async function saveRouteDraft(patch, message) {
    const config = cleanRouteConfig({ ...(state.config || {}) });
    if (patch.unified) config.unified = { ...(config.unified || {}), ...patch.unified };
    if (patch.groups) config.groups = patch.groups;
    try {
      await api("/api/config", { method: "POST", body: JSON.stringify({ config, backup: true }) });
      await refreshConfig();
      renderRoutes();
      toast(message || "路由配置已保存", "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function renderRoutes() {
    const config = state.config || {};
    const unified = config.unified || {};
    const storedVisible = Array.isArray(unified.visibleUpstreams) ? unified.visibleUpstreams.filter(Boolean) : [];
    const names = upstreamNameList();
    const visible = new Set(storedVisible.length ? storedVisible : names);
    const routeGroups = routeGroupPairs();
    const groups = Array.isArray(unified.groups) ? unified.groups : [];
    let dirty = false;

    const summary = $("#route-summary");
    if (summary) {
      summary.innerHTML = `
        <div class="route-stat"><span class="k">上游</span><b>${num(names.length)}</b><span class="s">接入与配置去重</span></div>
        <div class="route-stat ${storedVisible.length ? "amber" : "signal"}"><span class="k">统一入口可见</span><b>${storedVisible.length || names.length}</b><span class="s">${storedVisible.length ? "白名单生效" : "未设白名单 · 全部可见"}</span></div>
        <div class="route-stat"><span class="k">命名分组</span><b>${num(groups.length)}</b><span class="s">/unified/{组}/v1</span></div>
        <div class="route-stat"><span class="k">路由组</span><b>${num(routeGroups.length)}</b><span class="s">失败按成员顺序切换</span></div>
        <div class="route-stat-actions">
          <span class="pill" id="route-dirty" hidden>未保存</span>
          <button class="btn ghost sm" id="route-visible-all">全部可见</button>
          <button class="btn primary sm" id="route-save" disabled>保存路由</button>
        </div>`;
    }
    $("#unified-count").textContent = `${visible.size} 个可见`;
    $("#group-count").textContent = `${routeGroups.length} 组`;

    const markDirty = () => {
      dirty = true;
      const pill = $("#route-dirty");
      if (pill) pill.hidden = false;
      const save = $("#route-save");
      if (save) save.disabled = false;
    };

    const matrix = $("#unified-matrix");
    if (matrix) {
      matrix.innerHTML = names.length
        ? names
            .map(
              (name) => `<label class="matrix-row">
                <input type="checkbox" data-visible-upstream="${esc(name)}" ${visible.has(name) ? "checked" : ""}>
                <span class="grow"><span class="nm">${esc(name)}</span><span class="sub">${routeModelCount(name)} 个模型 · /${esc(name)}/v1</span></span>
                <span class="pill">${state.access.some((item) => item.id === name) ? "已接入" : "仅配置"}</span>
              </label>`
            )
            .join("")
        : `<div class="empty-line">还没有上游。先添加接入。</div>`;
      $$("#unified-matrix [data-visible-upstream]").forEach((input) => {
        input.addEventListener("change", () => {
          const next = new Set(visible);
          if (input.checked) next.add(input.dataset.visibleUpstream);
          else next.delete(input.dataset.visibleUpstream);
          visible.clear();
          next.forEach((name) => visible.add(name));
          markDirty();
          $("#unified-count").textContent = `${visible.size} 个可见`;
        });
      });
    }

    const groupHost = $("#route-group-cards");
    if (groupHost) {
      groupHost.innerHTML = routeGroups.length
        ? routeGroups
            .map(
              (group) => `<article class="route-group-card" data-group="${esc(group.id)}">
                <div class="route-group-head">
                  <div><div class="route-group-name">group/${esc(group.id)}</div><div class="hint">${group.members.length} 个成员 · 按顺序尝试</div></div>
                  <span class="pill">${esc(group.id)}</span>
                </div>
                <ol class="route-members" data-group-members="${esc(group.id)}">
                  ${group.members.length
                    ? group.members
                        .map(
                          (member, index) => `<li draggable="true" data-member="${esc(member)}" data-index="${index}">
                            <span class="drag">⋮⋮</span>
                            <span class="grow"><b>${esc(member)}</b><small>${index + 1} · ${routeModelCount(member)} 个模型</small></span>
                            <button class="icon-btn" data-member-up="${esc(member)}" title="上移" ${index === 0 ? "disabled" : ""}>↑</button>
                            <button class="icon-btn" data-member-down="${esc(member)}" title="下移" ${index === group.members.length - 1 ? "disabled" : ""}>↓</button>
                            <button class="icon-btn danger" data-member-remove="${esc(member)}" title="移除">×</button>
                          </li>`
                        )
                        .join("")
                    : `<li class="empty-line">还没有成员。从下方选择上游加入。</li>`}
                </ol>
                <div class="route-group-foot">
                  <select class="input select" data-group-add-select="${esc(group.id)}">
                    <option value="">添加上游…</option>
                    ${names.filter((name) => !group.members.includes(name)).map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("")}
                  </select>
                  <button class="btn ghost sm" data-group-add="${esc(group.id)}">加入成员</button>
                  <button class="btn danger sm" data-group-delete="${esc(group.id)}">删除组</button>
                </div>
              </article>`
            )
            .join("")
        : `<div class="empty-line">还没有路由组。到「设置 → 路由组」新增，或从下方新建。</div>`;
      groupHost.insertAdjacentHTML(
        "beforeend",
        `<div class="route-group-add"><input class="input" id="route-group-new" placeholder="新组标识，例如 fast"><button class="btn ghost sm" id="route-group-create">+ 新建路由组</button></div>`
      );
      bindRouteGroupActions(routeGroups, markDirty);
    }

    const save = $("#route-save");
    if (save) {
      save.addEventListener("click", () => {
        const nextGroups = {};
        routeGroups.forEach((group) => {
          nextGroups[group.id] = group.members;
        });
        saveRouteDraft({ unified: { visibleUpstreams: visible.size === names.length ? [] : [...visible] }, groups: nextGroups }, "统一入口与路由组已保存");
      });
    }
    $("#route-visible-all")?.addEventListener("click", () => {
      visible.clear();
      names.forEach((name) => visible.add(name));
      $$("#unified-matrix [data-visible-upstream]").forEach((input) => {
        input.checked = true;
      });
      $("#unified-count").textContent = `${names.length} 个可见`;
      markDirty();
    });

    const previewInput = $("#route-preview-input");
    const previewRun = $("#route-preview-run");
    if (previewRun && previewInput) {
      previewRun.addEventListener("click", () => renderRoutePreview(previewInput.value.trim()));
      previewInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") renderRoutePreview(previewInput.value.trim());
      });
    }

    // 让「在设置中编辑」真正跳到设置页的路由分区。
    $$('#view-routes [data-config-sub="groups"]').forEach((button) => {
      button.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        navigate("settings");
        setTimeout(() => {
          if (window.MG_APPLY_SETTINGS_SECTION) window.MG_APPLY_SETTINGS_SECTION("routes");
          document.querySelector("#view-settings")?.scrollIntoView({ block: "start" });
        }, 80);
      });
    });
    void dirty;
  }

  function bindRouteGroupActions(routeGroups, markDirty) {
    const findGroup = (id) => routeGroups.find((group) => group.id === id);
    const mutate = (groupId, fn) => {
      const group = findGroup(groupId);
      if (!group) return;
      fn(group.members);
      markDirty();
      renderGroupLists(routeGroups, markDirty);
    };

    $$("#route-group-cards [data-group-members]").forEach((list) => {
      const groupId = list.dataset.groupMembers;
      list.querySelectorAll("li[data-member]").forEach((item) => {
        item.addEventListener("dragstart", (event) => {
          event.dataTransfer.setData("text/plain", `${groupId}|${item.dataset.index}`);
          event.dataTransfer.effectAllowed = "move";
        });
        item.addEventListener("dragover", (event) => event.preventDefault());
        item.addEventListener("drop", (event) => {
          event.preventDefault();
          const [fromGroup, fromIndex] = String(event.dataTransfer.getData("text/plain") || "").split("|");
          if (fromGroup !== groupId) return;
          const toIndex = Number(item.dataset.index);
          mutate(groupId, (members) => {
            const [moved] = members.splice(Number(fromIndex), 1);
            if (moved) members.splice(toIndex, 0, moved);
          });
        });
      });
      list.querySelectorAll("[data-member-up]").forEach((button) => {
        button.addEventListener("click", () => mutate(groupId, (members) => {
          const index = members.indexOf(button.dataset.memberUp);
          if (index > 0) [members[index - 1], members[index]] = [members[index], members[index - 1]];
        }));
      });
      list.querySelectorAll("[data-member-down]").forEach((button) => {
        button.addEventListener("click", () => mutate(groupId, (members) => {
          const index = members.indexOf(button.dataset.memberDown);
          if (index >= 0 && index < members.length - 1) [members[index + 1], members[index]] = [members[index], members[index + 1]];
        }));
      });
      list.querySelectorAll("[data-member-remove]").forEach((button) => {
        button.addEventListener("click", () => mutate(groupId, (members) => {
          const index = members.indexOf(button.dataset.memberRemove);
          if (index >= 0) members.splice(index, 1);
        }));
      });
    });

    $$("#route-group-cards [data-group-add]").forEach((button) => {
      button.addEventListener("click", () => {
        const groupId = button.dataset.groupAdd;
        const select = document.querySelector(`[data-group-add-select="${groupId}"]`);
        const name = select?.value;
        if (!name) return;
        mutate(groupId, (members) => {
          if (!members.includes(name)) members.push(name);
        });
      });
    });

    $$("#route-group-cards [data-group-delete]").forEach((button) => {
      button.addEventListener("click", () => {
        const group = findGroup(button.dataset.groupDelete);
        if (!group) return;
        if (!confirm(`删除路由组 group/${group.id}？`)) return;
        routeGroups.splice(routeGroups.indexOf(group), 1);
        markDirty();
        renderGroupLists(routeGroups, markDirty);
      });
    });

    $("#route-group-create")?.addEventListener("click", () => {
      const input = $("#route-group-new");
      const id = String(input?.value || "").trim();
      if (!/^[A-Za-z0-9._-]+$/.test(id)) {
        toast("组标识只允许字母、数字、- _ .", "error");
        return;
      }
      if (findGroup(id)) {
        toast("组标识已存在", "error");
        return;
      }
      routeGroups.push({ id, members: [] });
      if (input) input.value = "";
      markDirty();
      renderGroupLists(routeGroups, markDirty);
    });
  }

  function renderGroupLists(routeGroups, markDirty) {
    const host = $("#route-group-cards");
    if (!host) return;
    // 重新渲染成员列表与选择器，但保留已输入的新组名。
    const draftName = $("#route-group-new")?.value || "";
    const names = upstreamNameList();
    host.innerHTML = routeGroups.length
      ? routeGroups
          .map(
            (group) => `<article class="route-group-card" data-group="${esc(group.id)}">
              <div class="route-group-head"><div><div class="route-group-name">group/${esc(group.id)}</div><div class="hint">${group.members.length} 个成员 · 按顺序尝试</div></div><span class="pill">${esc(group.id)}</span></div>
              <ol class="route-members" data-group-members="${esc(group.id)}">
                ${group.members.length
                  ? group.members
                      .map(
                        (member, index) => `<li draggable="true" data-member="${esc(member)}" data-index="${index}">
                          <span class="drag">⋮⋮</span>
                          <span class="grow"><b>${esc(member)}</b><small>${index + 1} · ${routeModelCount(member)} 个模型</small></span>
                          <button class="icon-btn" data-member-up="${esc(member)}" ${index === 0 ? "disabled" : ""}>↑</button>
                          <button class="icon-btn" data-member-down="${esc(member)}" ${index === group.members.length - 1 ? "disabled" : ""}>↓</button>
                          <button class="icon-btn danger" data-member-remove="${esc(member)}">×</button>
                        </li>`
                      )
                      .join("")
                  : `<li class="empty-line">还没有成员。从下方选择上游加入。</li>`}
              </ol>
              <div class="route-group-foot">
                <select class="input select" data-group-add-select="${esc(group.id)}"><option value="">添加上游…</option>${names.filter((name) => !group.members.includes(name)).map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("")}</select>
                <button class="btn ghost sm" data-group-add="${esc(group.id)}">加入成员</button>
                <button class="btn danger sm" data-group-delete="${esc(group.id)}">删除组</button>
              </div>
            </article>`
          )
          .join("")
      : `<div class="empty-line">还没有路由组。到「设置 → 路由组」新增，或从下方新建。</div>`;
    host.insertAdjacentHTML("beforeend", `<div class="route-group-add"><input class="input" id="route-group-new" placeholder="新组标识，例如 fast" value="${esc(draftName)}"><button class="btn ghost sm" id="route-group-create">+ 新建路由组</button></div>`);
    bindRouteGroupActions(routeGroups, markDirty);
    markDirty();
  }

  function renderRoutePreview(query) {
    const host = $("#route-preview-result");
    if (!host) return;
    if (!query) {
      host.innerHTML = `<div class="empty-line">输入后显示尝试顺序与不切换规则。</div>`;
      return;
    }
    const groups = routeGroupPairs();
    let order = [];
    let mode = "";
    if (query.startsWith("group/")) {
      const id = query.slice(6);
      const group = groups.find((item) => item.id === id);
      if (!group) {
        host.innerHTML = `<div class="inline-note danger">找不到路由组 group/${esc(id)}。请求会返回错误，不会静默落到默认上游。</div>`;
        return;
      }
      order = group.members;
      mode = `路由组 group/${esc(id)}`;
    } else {
      order = state.access
        .filter((item) => JSON.stringify(item.models || {}).toLowerCase().includes(query.toLowerCase()))
        .map((item) => item.id);
      if (!order.length) order = state.access.map((item) => item.id);
      mode = `模型 ${esc(query)}`;
    }
    host.innerHTML = `<div class="preview-mode">${mode}</div>
      <ol class="preview-steps">${order.map((name, index) => `<li><span class="n">${index + 1}</span><span class="grow">${esc(name)}</span><span class="pill">${state.access.some((item) => item.id === name) ? "已接入" : "仅配置"}</span></li>`).join("") || '<li class="empty-line">没有可尝试的上游。</li>'}</ol>
      <div class="inline-note" style="margin-top:10px">401 / 403 / 404 / 普通 400 不切换；只有网络错误、429、5xx 才继续下一个成员。</div>`;
  }

  // ───────────────────────── 凭据池 ─────────────────────────

  function renderPools() {
    const host = $("#pool-list");
    const summary = $("#pool-summary");
    const pools = state.pools || [];
    const totals = pools.reduce(
      (acc, pool) => {
        acc.members += Number(pool.members || 0);
        acc.available += Number(pool.available || 0);
        acc.cooling += Number(pool.cooling || 0);
        acc.exhausted += Number(pool.exhausted || 0);
        acc.disabled += Number(pool.disabled || 0);
        acc.needsSecret += Number(pool.needs_secret || 0);
        return acc;
      },
      { members: 0, available: 0, cooling: 0, exhausted: 0, disabled: 0, needsSecret: 0 }
    );
    if (summary) {
      summary.innerHTML = `
        <div class="pool-stat"><span class="k">池</span><b>${num(pools.length)}</b><span class="s">绑定上游 ${num(new Set(pools.map((pool) => pool.upstream)).size)} 个</span></div>
        <div class="pool-stat signal"><span class="k">可用成员</span><b>${num(totals.available)}</b><span class="s">共 ${num(totals.members)} 个成员</span></div>
        <div class="pool-stat ${totals.cooling ? "amber" : ""}"><span class="k">冷却中</span><b>${num(totals.cooling)}</b><span class="s">按失败阈值自动恢复</span></div>
        <div class="pool-stat ${totals.exhausted || totals.needsSecret ? "danger" : ""}"><span class="k">需要补充</span><b>${num(totals.exhausted + totals.needsSecret)}</b><span class="s">配额耗尽 ${num(totals.exhausted)} · 待补密钥 ${num(totals.needsSecret)}</span></div>`;
    }

    if (!pools.length) {
      host.innerHTML = `<div class="card pool-empty"><div class="empty">
        <div class="empty-mark">⚿</div><h3>还没有凭据池</h3>
        <p>多 Key 或账号型接入可创建池；池成员密钥只写本机加密库，界面只显示掩码与状态。</p>
        <button class="btn primary" id="pool-add-empty">+ 新建凭据池</button>
      </div></div>`;
      const button = $("#pool-add-empty");
      if (button) button.addEventListener("click", openPoolModal);
      return;
    }
    host.innerHTML = pools
      .map((pool) => {
        const detail = state.poolDetails[pool.id];
        const members = detail && Array.isArray(detail.memberList) ? detail.memberList : [];
        const total = Number(pool.members || members.length || 0);
        const available = Number(pool.available || 0);
        const cooling = Number(pool.cooling || 0);
        const exhausted = Number(pool.exhausted || 0);
        const disabled = Number(pool.disabled || 0);
        return `<article class="pool-card" data-pool="${esc(pool.id)}">
          <div class="pool-card-head">
            <div class="grow"><div class="pool-name">${esc(pool.id)}</div><div class="pool-sub">绑定上游 <b>${esc(pool.upstream || "—")}</b> · ${esc(STRATEGY_LABEL[pool.strategy] || pool.strategy || "priority")}</div></div>
            <span class="pill ${pool.status === "active" || !pool.status ? "signal" : "amber"}">${esc(pool.status || "active")}</span>
          </div>
          <div class="pool-meter" title="可用 ${available} / 共 ${total}">
            ${Array.from({ length: Math.max(total, 1) }, (_, index) => {
              const kind = index < available ? "ok" : index < available + cooling ? "cool" : index < available + cooling + exhausted ? "bad" : "off";
              return `<i class="${kind}"></i>`;
            }).join("")}
          </div>
          <div class="pool-tagline">
            <span class="pill signal">可用 ${available}/${total}</span>
            ${cooling ? `<span class="pill amber">冷却 ${cooling}</span>` : ""}
            ${exhausted ? `<span class="pill danger">耗尽 ${exhausted}</span>` : ""}
            ${disabled ? `<span class="pill">停用 ${disabled}</span>` : ""}
            ${pool.quota_limit ? `<span class="pill">配额 ${compactNum(pool.quota_used)}/${compactNum(pool.quota_limit)}</span>` : ""}
          </div>
          ${members.length
            ? `<div class="table-wrap pool-members"><table class="table">
                <thead><tr><th>成员</th><th>状态</th><th class="num">优先级</th><th class="num">配额</th><th>冷却至 / 最近错误</th><th class="right">操作</th></tr></thead>
                <tbody>${members
                  .map(
                    (member) => `<tr>
                      <td><div class="name">${esc(member.label || member.id)}</div><div class="tiny muted mono">${esc(member.kind || "key")} · ${esc(String(member.id).slice(0, 10))}</div></td>
                      <td><span class="pill ${
                        member.status === "active" ? "signal" : member.cooling ? "amber" : member.status === "exhausted" ? "danger" : ""
                      }">${esc(STATUS_LABEL[member.status] || member.status || "—")}</span></td>
                      <td class="num">${num(member.priority)}</td>
                      <td class="num tiny">${member.quota_limit ? `${compactNum(member.quota_used)}/${compactNum(member.quota_limit)}` : "—"}</td>
                      <td class="tiny muted">${member.coolingUntil ? `冷却至 ${fmtTime(member.coolingUntil)}` : esc(member.lastError || "—")}</td>
                      <td class="right"><div class="actions">
                        <button class="btn ghost sm" data-member-toggle="${esc(pool.id)}|${esc(member.id)}|${member.status === "disabled" ? "active" : "disabled"}">${member.status === "disabled" ? "启用" : "停用"}</button>
                        <button class="btn danger sm" data-member-delete="${esc(pool.id)}|${esc(member.id)}">删除</button>
                      </div></td>
                    </tr>`
                  )
                  .join("")}</tbody>
              </table></div>`
            : `<div class="empty-line">池里还没有成员。添加成员时只填标签与密钥。</div>`}
          <div class="pool-card-foot">
            <button class="btn primary sm" data-member-add="${esc(pool.id)}">+ 添加成员</button>
            <button class="btn ghost sm" data-pool-probe="${esc(pool.id)}">探测全部</button>
            <button class="btn ghost sm" data-pool-policy="${esc(pool.id)}">池设置</button>
          </div>
        </article>`;
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

  function pluginCard(plugin, inLibrary) {
    const attached = Array.isArray(plugin.attached) ? plugin.attached : [];
    const name = plugin.name || plugin.id;
    const initials = String(name).slice(0, 2).toUpperCase();
    const kindLabel = { free: "免费来源", "free-source": "免费来源", account: "账号池", provider: "Provider", aggregator: "聚合", tool: "工具" }[plugin.kind] || plugin.kind || "插件";
    const sourceLabel = inLibrary ? "内置包" : plugin.builtin ? "内置包" : plugin.source === "import" ? "导入包" : "本地包";
    // 免费来源插件（kilo/zen）自带模型获取与检测：目录端点、免费判定与探测
    // 参数由插件 freeSource 指向的注册表条目承载，每个插件各用自己的一套。
    const modelsBtn = plugin.freeSource && !inLibrary
      ? `<button class="btn ghost sm" data-plugin-models="${esc(plugin.id)}">模型 · 检测</button>`
      : "";
    return `<article class="plugin-card ${plugin.enabled && !inLibrary ? "on" : "off"}${inLibrary ? " removed" : ""}" data-plugin="${esc(plugin.id)}">
      <div class="plugin-head">
        <span class="plugin-avatar">${esc(initials)}</span>
        <div class="grow"><div class="plugin-name">${esc(name)}</div><div class="plugin-sub mono">${esc(plugin.id)} · v${esc(plugin.version)}</div></div>
        ${inLibrary ? '<span class="pill">已移除</span>' : `<span class="pill ${plugin.enabled ? "signal" : ""}">${plugin.enabled ? "启用" : "停用"}</span>`}
      </div>
      <div class="plugin-chips">
        <span class="chip">${esc(kindLabel)}</span>
        ${plugin.tier ? `<span class="chip">${esc(plugin.tier)}</span>` : ""}
        <span class="chip">${esc(sourceLabel)}</span>
        ${plugin.builtin ? `<span class="chip on">内置</span>` : `<span class="chip">已安装</span>`}
      </div>
      <p class="plugin-desc">${esc(plugin.description || "未提供描述。")}</p>
      <div class="plugin-meta">${attached.length ? `已接入 ${esc(attached.join("、"))}` : inLibrary ? "已移除：入口隐藏，重装即恢复" : "未接入上游 · 安装只登记，接入才创建上游"}</div>
      <div class="plugin-foot">
        ${inLibrary
          ? `<button class="btn primary sm" data-plugin-restore="${esc(plugin.id)}">重装</button>`
          : `<button class="btn primary sm" data-plugin-attach="${esc(plugin.id)}">${plugin.enabled ? "接入" : "先启用"}</button>
            ${attached.length ? `<button class="btn ghost sm" data-plugin-detach="${esc(plugin.id)}" data-name="${esc(attached[0])}">解绑</button>` : ""}
            ${modelsBtn}
            <button class="btn ghost sm" data-plugin-toggle="${esc(plugin.id)}" data-enabled="${plugin.enabled ? "0" : "1"}">${plugin.enabled ? "停用" : "启用"}</button>
            <button class="btn danger sm" data-plugin-remove="${esc(plugin.id)}" data-builtin="${plugin.builtin ? "1" : "0"}">移除</button>`}
      </div>
    </article>`;
  }

  function renderPlugins() {
    const host = $("#plugin-list");
    if (host) host.innerHTML = state.plugins.map((plugin) => pluginCard(plugin, false)).join("") || '<div class="empty-line">没有插件</div>';
    const lib = $("#plugin-library");
    if (lib) {
      lib.innerHTML = state.pluginLibrary.map((plugin) => pluginCard(plugin, true)).join("");
      const wrap = $("#plugin-library-wrap");
      if (wrap) wrap.hidden = state.pluginLibrary.length === 0;
    }
    const root = $("#view-plugins") || document;
    $$("[data-plugin-attach]", root).forEach((button) =>
      button.addEventListener("click", () => attachPlugin(button.dataset.pluginAttach))
    );
    $$("[data-plugin-detach]", root).forEach((button) =>
      button.addEventListener("click", () => detachPlugin(button.dataset.pluginDetach, button.dataset.name))
    );
    $$("[data-plugin-toggle]", root).forEach((button) =>
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
    $$("[data-plugin-remove]", root).forEach((button) =>
      button.addEventListener("click", async () => {
        const builtin = button.dataset.builtin === "1";
        const message = builtin
          ? "移除内置插件？入口将隐藏，已接入的上游需先解绑；之后可在「内置库」一键重装。"
          : "卸载插件？已创建的上游与凭据会保留。";
        if (!confirm(message)) return;
        try {
          await api(`/api/plugins/${encodeURIComponent(button.dataset.pluginRemove)}`, { method: "DELETE" });
          await refreshPlugins();
          renderPlugins();
          toast(builtin ? "已移除；可在「内置库」重装" : "插件已卸载", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
    $$("[data-plugin-restore]", root).forEach((button) =>
      button.addEventListener("click", async () => {
        try {
          await api(`/api/plugins/${encodeURIComponent(button.dataset.pluginRestore)}/restore`, { method: "POST" });
          await refreshPlugins();
          renderPlugins();
          toast("已从内置库重装并启用", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      })
    );
    $$("[data-plugin-models]", root).forEach((button) =>
      button.addEventListener("click", () => {
        // 处理器在 v4-resources.js（免费来源域逻辑所在），加载顺序晚于本文件，
        // 因此点击时才解引用，缺失时提示而不是抛错。
        const opener = MG.openPluginModels;
        if (opener) opener(button.dataset.pluginModels).catch((error) => toast(error.message, "error"));
        else toast("模型检测模块未加载", "error");
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
      "导入插件包",
      `<div class="inline-note">三种方式任选其一；导入即登记并与内置包同权（可停用 / 移除 / 重装）。安装只登记，不修改配置、不执行 DDL。</div>
      <label class="field"><span>方式一：选择 manifest.json 文件</span>
        <input class="input" id="plugin-import-file" type="file" accept=".json,application/json">
      </label>
      <label class="field"><span>方式二：插件目录路径（内含 manifest.json）</span>
        <div class="row"><input class="input mono grow" id="plugin-import-path" placeholder="D:\\plugins\\my-provider">
        <button class="btn primary sm" id="plugin-import-submit">导入</button></div>
      </label>
      <label class="field"><span>方式三：粘贴 manifest JSON</span>
        <textarea class="input mono" id="plugin-manifest" rows="10" style="width:100%;resize:vertical" placeholder='{"schema":"modelgate.plugin/v1","id":"example.provider","kind":"provider","tier":"declarative","auth":{"mode":"single"},"upstream":{"protocol":"openai","baseUrl":"https://api.example.com/v1"}}'></textarea>
      </label>
      <div class="modal-actions"><button class="btn primary" id="install-plugin-submit">校验并登记</button></div>`
    );
    const submitJson = async (manifest, note) => {
      try {
        await api("/api/plugins/install", { method: "POST", body: JSON.stringify({ manifest }) });
        closeModal();
        await refreshPlugins();
        renderPlugins();
        toast(note || "插件已登记；接入请点击「接入」", "ok");
      } catch (error) {
        toast(error.message, "error");
      }
    };
    const file = $("#plugin-import-file");
    if (file) {
      file.addEventListener("change", () => {
        const selected = file.files && file.files[0];
        if (!selected) return;
        const reader = new FileReader();
        reader.onload = async () => {
          let manifest;
          try {
            manifest = JSON.parse(String(reader.result));
          } catch {
            toast("manifest 不是合法 JSON", "error");
            return;
          }
          await submitJson(manifest, "已从文件导入；接入请点击「接入」");
        };
        reader.readAsText(selected);
      });
    }
    const pathSubmit = $("#plugin-import-submit");
    if (pathSubmit) {
      pathSubmit.addEventListener("click", async () => {
        const path = ($("#plugin-import-path").value || "").trim();
        if (!path) {
          toast("请填写插件目录路径", "error");
          return;
        }
        try {
          await api("/api/plugins/import", { method: "POST", body: JSON.stringify({ path }) });
          closeModal();
          await refreshPlugins();
          renderPlugins();
          toast("已从目录导入；接入请点击「接入」", "ok");
        } catch (error) {
          toast(error.message, "error");
        }
      });
    }
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
        await submitJson(manifest);
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
      button.addEventListener("click", () => navigate(button.dataset.go));
    });
    $("#theme-toggle").addEventListener("click", (event) => {
      event.stopPropagation();
      toggleThemeMenu();
    });
    document.addEventListener("click", closeThemeMenu);
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") closeThemeMenu();
    });
    document.addEventListener("click", (event) => {
      const pick = event.target.closest("[data-theme-pick]");
      if (pick) {
        setTheme(pick.dataset.themePick);
        closeThemeMenu();
      }
    });
    $("#config-help-btn").addEventListener("click", () => window.MG_HELP && window.MG_HELP());
    $("#update-btn").addEventListener("click", () => window.MG_UPDATE && window.MG_UPDATE());
    $("#access-add").addEventListener("click", () => openAddModal("official"));
    $$("[data-add]").forEach((button) => button.addEventListener("click", () => openAddModal(button.dataset.add)));

    $("#access-probe-all").addEventListener("click", async () => {
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
    $("#plugin-import-open").addEventListener("click", openInstallPluginModal);

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
    runView,
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
