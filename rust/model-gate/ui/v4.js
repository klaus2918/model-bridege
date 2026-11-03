/* v4 壳层：接入页签 / 设置子导航 / 请求抽屉 / 运行日志与诊断 / ⌘K。
   路由唯一入口是 MG.navigate（v4-core.js），本文件不再持有第二套路由。 */
(() => {
  "use strict";
  // v4.css 的选择器全部带 html[data-ui="v4"] 前缀；必须在样式判定前打上标记。
  document.documentElement.dataset.ui = "v4";
  const MG = window.MG;
  const { $, $$, api, esc, num, fmtTime, fmtMs, toast, state } = MG;
  const v4 = {
    observe: "logs",
    drawerRow: null,
    cmdkItems: [],
    cmdkIndex: 0,
    logsTimer: null,
    logSearchTimer: null,
    lastLogText: null,
    lastDiag: null,
  };

  function money(value) {
    const n = Number(value || 0);
    if (!n) return "—";
    if (Math.abs(n) < 0.01) return `¥${n.toFixed(4)}`;
    return `¥${n.toFixed(2)}`;
  }

  function setHTML(selector, html) {
    const el = $(selector);
    if (el) el.innerHTML = html;
  }

  function show(el, visible) {
    if (el) el.hidden = !visible;
  }

  function retry(fn, timeout = 1200) {
    const started = Date.now();
    (function tick() {
      if (fn()) return;
      if (Date.now() - started > timeout) return;
      setTimeout(tick, 60);
    })();
  }

  // ───────────────────────── 接入页次级页签 ─────────────────────────

  const ACCESS_PANES = {
    list: "access-pane-list",
    pools: "access-pane-pools",
    models: "access-pane-models",
    free: "access-pane-free",
    templates: "access-pane-templates",
  };
  const PANE_VIEW = { pools: "credentials", models: "models", free: "free", templates: "templates" };

  function setAccessTab(tab) {
    if (!ACCESS_PANES[tab]) tab = "list";
    state.accessTab = tab;
    $$("#access-tabs [data-access-tab]").forEach((button) => {
      button.classList.toggle("active", button.dataset.accessTab === tab);
    });
    for (const [key, id] of Object.entries(ACCESS_PANES)) {
      const el = document.getElementById(id);
      if (el) el.hidden = key !== tab;
    }
    if (tab !== "list") MG.runView(PANE_VIEW[tab]);
  }

  // ───────────────────────── 设置子导航 ─────────────────────────

  function setSettingsPane(pane) {
    $$("#settings-subnav [data-settings-pane]").forEach((button) => {
      button.classList.toggle("active", button.dataset.settingsPane === pane);
    });
    $$("#view-settings .settings-panes > div").forEach((el) => {
      el.hidden = el.id !== `spane-${pane}`;
    });
    // 进入维护页立即拉一次日志，避免空面板。
    if (pane === "maintain" && v4.observe === "logs") loadLogs().catch(() => {});
    if (pane === "maintain" && v4.observe === "diag") loadDiagnostics().catch(() => {});
  }

  function setObserveTab(tab) {
    v4.observe = tab === "diag" ? "diag" : "logs";
    $$("#observe-tabs [data-observe-tab]").forEach((button) => {
      button.classList.toggle("active", button.dataset.observeTab === v4.observe);
    });
    show($("#observe-logs"), v4.observe === "logs");
    show($("#observe-diag"), v4.observe === "diag");
    if (v4.observe === "diag") loadDiagnostics().catch(() => {});
    else loadLogs().catch(() => {});
  }

  // ───────────────────────── 请求抽屉（保留自 v4.1） ─────────────────────────

  function closeRequestDrawer() {
    const drawer = $("#request-drawer");
    if (drawer) drawer.hidden = true;
    const scrim = $("#scrim");
    if (scrim && $("#modal").hidden) scrim.hidden = true;
  }

  function openRequestDrawer(row) {
    if (!row) return;
    const drawer = $("#request-drawer");
    if (!drawer) return;
    v4.drawerRow = row;
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
    const hasSession = Boolean(row.session);
    setHTML(
      "#request-body",
      `<div class="kv request-kv">${fields
        .map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v == null || v === "" ? "—" : v)}</dd>`)
        .join("")}</div>
       <div class="inline-note" style="margin-top:14px">脱敏视图：请求头与提示词正文不在此处展示。</div>
       <div class="card-actions wrap" style="margin-top:12px">
         ${hasSession ? '<button class="btn ghost sm" data-v4-drawer-go="sessions">在会话中查看轨迹</button>' : ""}
         <button class="btn ghost sm" data-v4-drawer-go="logs">打开日志</button>
       </div>`
    );
    drawer.hidden = false;
    const scrim = $("#scrim");
    if (scrim) scrim.hidden = false;
  }

  MG.openRequestDrawer = openRequestDrawer;

  function openSessionsForRequest(row) {
    closeRequestDrawer();
    if (!row) return;
    if (row.session) MG.focusPromptSession?.(row.session);
    else toast("该请求没有会话键", "error");
  }

  // 「打开日志」：预填请求 ID（access 日志行尾带 req=<id>），落到 设置→维护→运行日志。
  function openLogsForRequest(row) {
    closeRequestDrawer();
    MG.filterRuntimeLogs?.(row ? row.id : "");
    MG.navigate("settings");
    setSettingsPane("maintain");
    setObserveTab("logs");
  }

  function runDrawerGo(target) {
    const row = v4.drawerRow;
    if (target === "sessions") openSessionsForRequest(row);
    else if (target === "logs") openLogsForRequest(row);
  }

  // ───────────────────────── 运行日志（自 v4-usage.js 移入） ─────────────────────────

  async function loadLogs() {
    const lines = $("#runtime-lines")?.value || "400";
    const level = $("#runtime-level")?.value || "all";
    const q = $("#runtime-search")?.value.trim() || "";
    const data = await api(`/api/logs?lines=${encodeURIComponent(lines)}&level=${encodeURIComponent(level)}&q=${encodeURIComponent(q)}`);
    const viewer = $("#runtime-viewer");
    if (!viewer) return;
    const text = (data.lines || []).map((line) => line.text).join("\n") || "（无匹配日志）";
    // 自动刷新时只替换变化过的内容：内容没变就不动 DOM，避免滚动位置与选中文本被打断。
    if (text !== v4.lastLogText) {
      const atBottom = viewer.scrollHeight - viewer.scrollTop - viewer.clientHeight < 24;
      viewer.textContent = text;
      v4.lastLogText = text;
      if (atBottom) viewer.scrollTop = viewer.scrollHeight;
    }
    const size = data.size ? `${(data.size / 1024 / 1024).toFixed(1)} MB` : "—";
    const tail = data.tail ? ` · 尾部 ${Math.round(Number(data.windowBytes || 0) / 1024)} KB` : "";
    const meta = $("#runtime-meta");
    if (meta) meta.textContent = `${data.matched ?? data.lines?.length ?? 0} / ${data.total ?? 0} 行 · ${size}${tail}`;
  }

  function logViewActive() {
    const view = $("#view-settings");
    const pane = $("#spane-maintain");
    return Boolean(view && view.classList.contains("active") && pane && !pane.hidden) && document.visibilityState !== "hidden";
  }

  function applyLogAutoRefresh() {
    clearInterval(v4.logsTimer);
    const seconds = Number($("#runtime-auto")?.value) || 0;
    v4.logsTimer = seconds > 0
      ? setInterval(() => {
          if (logViewActive()) loadLogs().catch(() => {});
        }, seconds * 1000)
      : null;
  }

  // 请求抽屉「打开日志」的落点：只预填搜索框，实际查询交给随后的加载器。
  MG.filterRuntimeLogs = (q) => {
    const input = $("#runtime-search");
    if (!input) return;
    input.value = q || "";
    clearTimeout(v4.logSearchTimer);
  };

  function bindLogs() {
    $("#runtime-refresh")?.addEventListener("click", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-level")?.addEventListener("change", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-lines")?.addEventListener("change", () => loadLogs().catch((error) => toast(error.message, "error")));
    $("#runtime-search")?.addEventListener("input", () => {
      clearTimeout(v4.logSearchTimer);
      v4.logSearchTimer = setTimeout(() => loadLogs().catch(() => {}), 250);
    });
    $("#runtime-auto")?.addEventListener("change", applyLogAutoRefresh);
    $("#runtime-copy")?.addEventListener("click", async () => {
      await navigator.clipboard.writeText($("#runtime-viewer").textContent);
      toast("已复制当前日志", "ok");
    });
    $("#runtime-bottom")?.addEventListener("click", () => {
      const viewer = $("#runtime-viewer");
      if (viewer) viewer.scrollTop = viewer.scrollHeight;
    });
    $("#runtime-clear")?.addEventListener("click", async () => {
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

  // ───────────────────────── 诊断（保留自 v4.1，目标改为新 IA） ─────────────────────────

  async function loadDiagnostics() {
    setHTML("#diag-meta", "检查中…");
    const pricing = await api("/api/pricing").catch(() => ({}));
    v4.lastDiag = pricing || {};
    renderDiagnostics(v4.lastDiag);
    const meta = $("#diag-meta");
    if (meta) meta.textContent = `检查时间 ${fmtTime(Math.floor(Date.now() / 1000))}`;
  }

  function diagnosticsFrom(pricing) {
    const groups = new Map();
    const push = (group, level, title, detail, target) => {
      if (!groups.has(group)) groups.set(group, []);
      groups.get(group).push({ level, title, detail, target });
    };
    const access = state.access || [];
    const pools = state.pools || [];
    const config = state.config || {};
    const upstreams = config.upstreams && typeof config.upstreams === "object" ? Object.keys(config.upstreams) : [];

    if (!config || !Object.keys(config).length) {
      push("配置", "error", "配置文件未读取", "无法读取 bridge.config.json，请检查文件权限或格式。", { view: "settings", pane: "maintain" });
    } else {
      push("配置", "ok", "配置文件可读", `${upstreams.length} 个上游、${Array.isArray(config.models) ? config.models.length : 0} 个对外模型；保存前默认备份。`, { view: "settings", pane: "maintain" });
    }

    if (!access.length) {
      push("接入", "warn", "还没有接入", "添加第一个官方模板或自定义上游后，网关才会转发请求。", { view: "access" });
    }
    for (const item of access) {
      const stateName = item.health?.state || "warn";
      if (stateName !== "ok") {
        push("接入", stateName === "error" ? "error" : "warn", `${item.label || item.id} 健康异常`, item.health?.message || "打开接入详情查看诊断页签。", { view: "access" });
      }
      if (item.auth?.mode !== "none" && !item.auth?.configured) {
        push("接入", "warn", `${item.label || item.id} 缺少凭据`, "密钥只写不回显；请写入或绑定凭据池。", { view: "access" });
      }
    }

    for (const pool of pools) {
      if (Number(pool.cooling || 0) > 0) {
        push("凭据", "warn", `池 ${pool.id} 有 ${pool.cooling} 个成员冷却`, `可用 ${pool.available}/${pool.members}；可在成员表重置冷却或补充成员。`, { view: "access", pane: "pools" });
      }
      if (Number(pool.exhausted || 0) > 0) {
        push("凭据", "error", `池 ${pool.id} 配额耗尽`, `${pool.exhausted} 个成员已耗尽，请调整配额或替换成员。`, { view: "access", pane: "pools" });
      }
      if (Number(pool.needs_secret || 0) > 0) {
        push("凭据", "warn", `池 ${pool.id} 有成员待补密钥`, "成员只保存 secret 引用；请重新写入密钥。", { view: "access", pane: "pools" });
      }
    }

    const entries = Number(pricing.entries || 0);
    if (!entries) {
      push("价格", "warn", "单价表为空", "费用统计会显示 0；至少添加一个默认条目或按上游固定价。", { view: "access", pane: "models" });
    } else {
      push("价格", "ok", `单价表 ${entries} 条`, `汇率 USD→CNY ${Number(pricing.fx?.USD || 7.2).toFixed(2)}；上游固定价优先于任意上游。`, { view: "access", pane: "models" });
    }
    for (const diag of Array.isArray(pricing.diagnostics) ? pricing.diagnostics : []) {
      push("价格", diag.level === "error" ? "error" : "warn", diag.code || "定价诊断", diag.message || "", { view: "access", pane: "models" });
    }

    for (const plugin of state.plugins || []) {
      if (!plugin.enabled) {
        push("插件", "warn", `插件 ${plugin.name || plugin.id} 已停用`, "停用后不会参与接入与请求路由。", { view: "plugins" });
      } else if (!Array.isArray(plugin.attached) || plugin.attached.length === 0) {
        push("插件", "info", `插件 ${plugin.name || plugin.id} 未接入`, "安装只登记；点击「接入」才创建上游。", { view: "plugins" });
      }
    }

    if (!state.templates?.length) {
      push("模板", "warn", "模板目录为空", "官方模板不可用；请检查内置模板或重新加载配置。", { view: "access", pane: "templates" });
    }

    return groups;
  }

  function renderDiagnostics(pricing) {
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
          return (rank[a.level] ?? 4) - (rank[b.level] ?? 4);
        });
        return `<div class="diag-group"><div class="diag-group-head">${esc(group)}</div>${items
          .map(
            (item) => `<div class="diag-item ${esc(item.level)}" ${item.target ? `data-diag-view="${esc(item.target.view)}" data-diag-pane="${esc(item.target.pane || "")}" role="button"` : ""}>
              <span class="diag-dot"></span><div class="grow"><div class="name">${esc(item.title)}</div><div class="meta">${esc(item.detail)}</div></div>
              ${item.target ? '<span class="hint">前往 ›</span>' : ""}
            </div>`
          )
          .join("")}</div>`;
      })
      .join("");
  }

  async function exportDiagnostics() {
    const pricing = v4.lastDiag || (await api("/api/pricing").catch(() => ({})));
    const groups = diagnosticsFrom(pricing || {});
    const lines = ["# Model-Gate 诊断", ""];
    for (const [group, items] of groups) {
      lines.push(`## ${group}`);
      for (const item of items) lines.push(`- [${item.level}] ${item.title} — ${item.detail}`);
      lines.push("");
    }
    await navigator.clipboard.writeText(lines.join("\n"));
    toast("诊断报告已复制到剪贴板", "ok");
  }

  // ───────────────────────── ⌘K（命令面板，按新 IA 重排） ─────────────────────────

  function ensureCmdk() {
    if ($("#v4-cmdk")) return;
    const host = document.createElement("div");
    host.className = "cmdk";
    host.id = "v4-cmdk";
    host.hidden = true;
    host.innerHTML = `<div class="cmdk-panel" role="dialog" aria-modal="true" aria-label="全局搜索">
      <div class="cmdk-input"><span class="ico">⌕</span><input id="v4-cmdk-input" placeholder="搜索页面、接入、任务…" autocomplete="off"><kbd>ESC</kbd></div>
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
    const pages = [["usage", "用量"], ["sessions", "会话"], ["access", "接入"], ["plugins", "插件"], ["settings", "设置"]];
    for (const [view, label] of pages) {
      items.push({ group: "页面", icon: "◈", label, sub: `打开${label}`, run: () => MG.navigate(view) });
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
    items.push({ group: "任务", icon: "＋", label: "添加接入", sub: "打开添加向导", run: () => MG.openAddModal("official") });
    items.push({
      group: "任务",
      icon: "⚿",
      label: "新建凭据池",
      sub: "接入 · 凭据池",
      run: () => {
        MG.navigate("access");
        setAccessTab("pools");
        retry(() => {
          const el = $("#pool-add");
          if (!el) return false;
          el.click();
          return true;
        });
      },
    });
    items.push({
      group: "任务",
      icon: "▤",
      label: "补齐未定价模型",
      sub: "接入 · 单价表",
      run: () => {
        MG.navigate("access");
        setAccessTab("models");
        retry(() => {
          const el = $('#model-tabs [data-model-tab="pricing"]');
          if (!el) return false;
          el.click();
          return true;
        });
      },
    });
    items.push({
      group: "任务",
      icon: "◎",
      label: "开启提示词记录",
      sub: "设置 · 记录与脱敏",
      run: () => {
        MG.navigate("settings");
        setSettingsPane("record");
      },
    });
    items.push({ group: "任务", icon: "⑃", label: "导出诊断", sub: "复制诊断报告", run: () => exportDiagnostics() });
    return items;
  }

  function openAccessDrawer(id) {
    MG.navigate("access");
    setAccessTab("list");
    retry(() => {
      const row = document.querySelector(`#access-cards [data-open="${id.replaceAll('"', '\\"')}"]`);
      if (!row) return false;
      row.click();
      return true;
    }, 1600);
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

  // ───────────────────────── 事件绑定 ─────────────────────────

  function bindEvents() {
    $("#global-search").addEventListener("click", openCmdk);
    let gPending = false;
    document.addEventListener("keydown", (event) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        openCmdk();
      }
      if (event.key === "Escape") {
        closeCmdk();
        closeRequestDrawer();
      }
      // g→键位跳转：g u/s/a/p/e = 用量/会话/接入/插件/设置。
      const target = event.target;
      const typing = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
      if (typing || event.ctrlKey || event.metaKey || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key === "g" && !gPending) {
        gPending = true;
        setTimeout(() => { gPending = false; }, 1200);
        return;
      }
      if (gPending) {
        gPending = false;
        const map = { u: "usage", s: "sessions", a: "access", p: "plugins", e: "settings" };
        if (map[key]) {
          event.preventDefault();
          MG.navigate(map[key]);
        }
      }
    });

    $("#access-tabs").addEventListener("click", (event) => {
      const button = event.target.closest("[data-access-tab]");
      if (button) setAccessTab(button.dataset.accessTab);
    });
    $("#settings-subnav").addEventListener("click", (event) => {
      const button = event.target.closest("[data-settings-pane]");
      if (button) setSettingsPane(button.dataset.settingsPane);
    });
    $("#observe-tabs").addEventListener("click", (event) => {
      const button = event.target.closest("[data-observe-tab]");
      if (button) setObserveTab(button.dataset.observeTab);
    });

    document.addEventListener("click", (event) => {
      const diag = event.target.closest("[data-diag-view]");
      if (diag) {
        MG.navigate(diag.dataset.diagView);
        if (diag.dataset.diagPane) setAccessTab(diag.dataset.diagPane);
      }
    });

    $("#request-close").addEventListener("click", closeRequestDrawer);
    $("#request-drawer").addEventListener("click", (event) => {
      const button = event.target.closest("[data-v4-drawer-go]");
      if (button) runDrawerGo(button.dataset.v4DrawerGo);
    });

    bindLogs();
  }

  document.addEventListener("DOMContentLoaded", bindEvents);
})();
