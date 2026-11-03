/* v4 免费来源面板。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, esc, num, compactNum, fmtTime, fmtMs, api, toast, openModal, closeModal } = MG;

  const free = { sources: [], states: [], current: "", models: [], results: {}, level: {}, loaded: false };
  let bound = false;

  function stateMeta(id) {
    return free.states.find((state) => state.id === id) || { id, label: id || "未探测", tone: "info" };
  }

  function freeResult(model) {
    return free.results[model] || {};
  }

  function privacyBadge(privacy) {
    const level = privacy?.level || "info";
    return `<span class="pill ${level === "danger" ? "danger" : level === "warn" ? "amber" : level === "ok" ? "signal" : "info"}">${esc(level)}</span>`;
  }

  async function loadFree() {
    const data = await api("/api/free-sources");
    free.sources = Array.isArray(data.sources) ? data.sources : [];
    free.states = Array.isArray(data.states) ? data.states : [];
    free.loaded = true;
    renderFree();
    if (free.current) await loadFreeCatalog(free.current);
  }

  function renderFree() {
    const configured = free.sources.filter((source) => source.configured).length;
    const usable = free.sources.filter((source) => source.transportActive).length;
    $("#free-summary").innerHTML = `
      <div class="stat"><span class="k">来源</span><span class="v">${num(free.sources.length)}</span><span class="s">${num(configured)} 已接入</span></div>
      <div class="stat signal"><span class="k">传输可用</span><span class="v">${num(usable)}</span><span class="s">按接入配置</span></div>
      <div class="stat amber"><span class="k">当前模型</span><span class="v">${num(free.models.length)}</span><span class="s">${free.current || "未选择"}</span></div>`;
    const host = $("#free-sources");
    host.innerHTML = free.sources.length
      ? free.sources
          .map(
            (source) => `<div class="card ${free.current === source.id ? "accent" : ""}">
              <div class="card-head"><h2>${esc(source.label || source.id)}</h2>${privacyBadge(source.privacy)}</div>
              <div class="meta mono">${esc(source.id)} · ${esc(source.protocol)} · ${esc(source.transport || "直接")}</div>
              <p class="hint">${esc(source.privacy?.note || "未声明隐私风险。")}</p>
              <div class="tagline">
                <span class="pill ${source.configured ? "signal" : ""}">${source.configured ? "已接入" : "未接入"}</span>
                <span class="pill">${num(source.declaredModels)} 个声明</span>
                <span class="pill">${esc(source.auth || "none")}</span>
              </div>
              <div class="card-actions wrap" style="margin-top:12px">
                ${source.configured ? `<button class="btn ghost sm" data-free-action="catalog" data-source="${esc(source.id)}">查看模型</button>` : `<button class="btn primary sm" data-free-action="attach" data-source="${esc(source.id)}">一键接入</button>`}
                <button class="btn ghost sm" data-free-action="probe-all" data-source="${esc(source.id)}">全部探测</button>
                ${source.docsUrl ? `<a class="btn ghost sm" href="${esc(source.docsUrl)}" target="_blank" rel="noreferrer">文档</a>` : ""}
              </div>
            </div>`
          )
          .join("")
      : `<div class="card"><div class="empty-line">没有可用的免费来源模板。</div></div>`;
    renderFreeModels();
  }

  async function loadFreeCatalog(sourceId) {
    const data = await api("/api/free-sources/catalog", { method: "POST", body: JSON.stringify({ source: sourceId }) });
    free.current = sourceId;
    free.models = Array.isArray(data.models) ? data.models : [];
    free.results = {};
    free.level = data.level || {};
    $("#free-models-card").hidden = false;
    $("#free-models-title").textContent = `${free.sources.find((source) => source.id === sourceId)?.label || sourceId} · 模型`;
    $("#free-models-meta").textContent = `${free.models.length} 个候选 · ${data.level?.available ?? 0} 个可用`;
    renderFree();
  }

  function renderFreeModels() {
    if (!free.current) return;
    const filter = $("#free-model-state").value;
    const rows = free.models.filter((model) => !filter || freeResult(model.id).state === filter);
    $("#free-model-table tbody").innerHTML = rows.length
      ? rows
          .map((model) => {
            const result = freeResult(model.id);
            const meta = stateMeta(result.state);
            // 探测载荷的拒因字段是 reasonLabel/advice（与插件弹窗同款）；raw 兜底。
            const reason = [result.reasonLabel, result.advice].filter(Boolean).join("：") || result.raw || "—";
            return `<tr>
              <td><div class="name-cell"><code>${esc(model.id)}</code>${model.mayTrainOnYourPrompts ? '<span class="pill danger">可能训练</span>' : ""}</div><div class="hint">${esc(model.name || "")}</div></td>
              <td><span class="pill ${meta.tone === "ok" ? "signal" : meta.tone === "bad" ? "danger" : meta.tone === "warn" ? "amber" : "info"}">${esc(meta.label)}</span></td>
              <td class="num">${model.contextLength ? compactNum(model.contextLength) : "—"}</td>
              <td>${(model.reasoningEfforts || []).join(" / ") || "—"}</td>
              <td class="num">${result.latencyMs ? fmtMs(result.latencyMs) : "—"}</td>
              <td>${esc(reason)}</td>
              <td class="right"><div class="actions">
                <button class="btn ghost sm" data-free-action="probe-one" data-source="${esc(free.current)}" data-model="${esc(model.id)}">探测</button>
                <button class="btn ghost sm" data-free-action="globalize" data-model="${esc(model.id)}">加入全局</button>
              </div></td>
            </tr>`;
          })
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有匹配的模型。</div></td></tr>`;
  }

  async function probeFree(source, model) {
    const data = await api("/api/free-sources/probe", { method: "POST", body: JSON.stringify({ source, model }) });
    free.results[model] = data.result || {};
    renderFreeModels();
    toast(`${model}：${stateMeta(free.results[model].state).label}`, free.results[model].state === "available" ? "ok" : "");
  }

  async function probeAllFree(source) {
    const button = $(`[data-free-action="probe-all"][data-source="${CSS.escape(source)}"]`);
    if (button) button.disabled = true;
    $("#free-probe-hint").textContent = "正在逐个探测，会消耗免费额度…";
    try {
      const data = await api("/api/free-sources/probe-all", { method: "POST", body: JSON.stringify({ source }) });
      free.results = {};
      (data.results || []).forEach((result) => {
        free.results[result.model] = result;
      });
      free.level = data.level || free.level;
      renderFreeModels();
      $("#free-probe-hint").textContent = `探测完成：${(data.results || []).filter((result) => result.state === "available").length} 个可用`;
    } finally {
      if (button) button.disabled = false;
    }
  }

  async function attachFree(sourceId) {
    const source = free.sources.find((item) => item.id === sourceId);
    if (!confirm(`一键接入「${source?.label || sourceId}」？将写入上游「${source?.upstream || ""}」，不填 API Key。`)) return;
    try {
      const data = await api("/api/free-sources/attach", { method: "POST", body: JSON.stringify({ source: sourceId }) });
      toast(`已接入 ${data.upstream || sourceId}`, "ok");
      await loadFree();
      await loadFreeCatalog(sourceId);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function globalizeModel(model) {
    const cfg = MG.state.config || {};
    const models = Array.isArray(cfg.models) ? cfg.models.slice() : [];
    if (models.includes(model)) return toast("模型已在全局清单", "");
    models.push(model);
    try {
      await api("/api/config", { method: "POST", body: JSON.stringify({ config: { ...cfg, models }, backup: true }) });
      await MG.refreshConfig();
      toast(`已把 ${model} 加入全局清单`, "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  // ───────────────────────── 插件页「模型 · 检测」 ─────────────────────────
  // 插件卡片 → 各自的免费来源（manifest.freeSource → 注册表条目）。
  // 目录端点/免费判定/探测参数都在后端注册表；这里只渲染与触发探测。
  // 状态（pm）独立于免费来源子页的 free 对象，互不干扰。

  const pm = { plugin: "", source: "", models: [], states: [], results: {}, filter: "" };

  function pmStateMeta(id) {
    return pm.states.find((state) => state.id === id) || { id, label: id || "未探测", tone: "info" };
  }

  function pmPill(state) {
    const meta = pmStateMeta(state);
    return `<span class="pill ${meta.tone === "ok" ? "signal" : meta.tone === "bad" ? "danger" : meta.tone === "warn" ? "amber" : "info"}">${esc(meta.label)}</span>`;
  }

  function renderPmTable() {
    const body = $("#pm-table tbody");
    if (!body) return;
    const rows = pm.models.filter((model) => !pm.filter || (pm.results[model.id] || {}).state === pm.filter);
    body.innerHTML = rows.length
      ? rows
          .map((model) => {
            const result = pm.results[model.id] || {};
            const reason = [result.reasonLabel, result.advice].filter(Boolean).join("：") || result.raw || "—";
            return `<tr>
              <td><div class="name-cell"><code>${esc(model.id)}</code>${model.mayTrainOnYourPrompts ? '<span class="pill danger">可能训练</span>' : ""}</div><div class="hint">${esc(model.name || "")}</div></td>
              <td>${pmPill(result.state)}</td>
              <td class="num">${model.contextLength ? compactNum(model.contextLength) : "—"}</td>
              <td>${(model.reasoningEfforts || []).join(" / ") || "—"}</td>
              <td class="num">${result.latencyMs ? fmtMs(result.latencyMs) : "—"}</td>
              <td>${esc(reason)}</td>
              <td class="right"><div class="actions">
                <button class="btn ghost sm" data-pm-action="probe" data-model="${esc(model.id)}">探测</button>
              </div></td>
            </tr>`;
          })
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有匹配的模型。</div></td></tr>`;
    const count = $("#pm-count");
    if (count) {
      const available = pm.models.filter((model) => (pm.results[model.id] || {}).state === "available").length;
      count.textContent = `${pm.models.length} 个候选 · 可用 ${available}`;
    }
  }

  function pmToolbar() {
    const options = [{ id: "", label: "全部状态" }]
      .concat(pm.states.map((state) => ({ id: state.id, label: state.label })))
      .map((state) => `<option value="${esc(state.id)}">${esc(state.label)}</option>`)
      .join("");
    return `<div class="toolbar">
      <select class="input select" id="pm-state">${options}</select>
      <button class="btn ghost sm" id="pm-probe-all">全部探测</button>
      <span class="hint" id="pm-count">—</span>
      <span class="hint" id="pm-hint">探测会真实调用上游，消耗免费额度。</span>
    </div>`;
  }

  async function probePmOne(model) {
    const data = await api(`/api/plugins/${encodeURIComponent(pm.plugin)}/probe`, {
      method: "POST",
      body: JSON.stringify({ model }),
    });
    pm.results[model] = data.result || {};
    renderPmTable();
    toast(`${model}：${pmStateMeta(pm.results[model].state).label}`, pm.results[model].state === "available" ? "ok" : "");
  }

  async function probePmAll() {
    const button = $("#pm-probe-all");
    if (button) button.disabled = true;
    const hint = $("#pm-hint");
    if (hint) hint.textContent = "正在逐个探测，会消耗免费额度…";
    try {
      const data = await api(`/api/plugins/${encodeURIComponent(pm.plugin)}/probe-all`, { method: "POST", body: "{}" });
      pm.results = {};
      (data.results || []).forEach((result) => {
        pm.results[result.model] = result;
      });
      renderPmTable();
      const available = (data.results || []).filter((result) => result.state === "available").length;
      if (hint) hint.textContent = `探测完成：${available} 个可用`;
    } finally {
      if (button) button.disabled = false;
    }
  }

  async function openPluginModels(pluginId) {
    const plugin = (MG.state.plugins || []).find((item) => item.id === pluginId);
    const data = await api(`/api/plugins/${encodeURIComponent(pluginId)}/models`, { method: "POST", body: "{}" });
    pm.plugin = pluginId;
    pm.source = data.source || "";
    pm.models = Array.isArray(data.models) ? data.models : [];
    pm.states = Array.isArray(data.states) ? data.states : [];
    pm.results = {};
    pm.filter = "";
    openModal(
      `${plugin?.name || pluginId} · 模型`,
      `来源 ${pm.source} · ${pm.models.length} 个免费候选 · 状态由真实探测判定`,
      `${pmToolbar()}
       <div class="table-wrap"><table class="table" id="pm-table">
         <thead><tr><th>模型</th><th>状态</th><th class="num">上下文</th><th>思考档位</th><th class="num">延迟</th><th>拒因与建议</th><th class="right">操作</th></tr></thead>
         <tbody></tbody>
       </table></div>`
    );
    $("#pm-state").addEventListener("change", (event) => {
      pm.filter = event.target.value;
      renderPmTable();
    });
    $("#pm-probe-all").addEventListener("click", () => {
      probePmAll().catch((error) => {
        const hint = $("#pm-hint");
        if (hint) hint.textContent = "探测失败";
        toast(error.message, "error");
      });
    });
    $("#pm-table").addEventListener("click", (event) => {
      const button = event.target.closest("[data-pm-action]");
      if (button && button.dataset.pmAction === "probe") {
        probePmOne(button.dataset.model).catch((error) => toast(error.message, "error"));
      }
    });
    renderPmTable();
  }

  MG.openPluginModels = openPluginModels;


  function bind() {
    if (bound) return;
    bound = true;
    $("#free-refresh").addEventListener("click", () => loadFree().catch((error) => toast(error.message, "error")));
    $("#free-sources").addEventListener("click", (event) => {
      const button = event.target.closest("[data-free-action]");
      if (!button) return;
      const action = button.dataset.freeAction;
      const source = button.dataset.source;
      if (action === "catalog") loadFreeCatalog(source).catch((error) => toast(error.message, "error"));
      if (action === "attach") attachFree(source);
      if (action === "probe-all") probeAllFree(source).catch((error) => toast(error.message, "error"));
    });
    $("#free-model-table").addEventListener("click", (event) => {
      const button = event.target.closest("[data-free-action]");
      if (!button) return;
      const action = button.dataset.freeAction;
      if (action === "probe-one") probeFree(button.dataset.source, button.dataset.model).catch((error) => toast(error.message, "error"));
      if (action === "globalize") globalizeModel(button.dataset.model);
    });
    $("#free-model-state").addEventListener("change", renderFreeModels);
    $("#free-probe-all").addEventListener("click", () => free.current && probeAllFree(free.current).catch((error) => toast(error.message, "error")));

  }

  // ───────────────────────── 模板 ─────────────────────────

  const tpl = { items: [], loading: false, bound: false };

  const SUPPORT_TEXT = {
    request: "请求级",
    account: "账户级",
    none: "不支持请求级",
  };

  function templateModels(template) {
    const defaults = template.defaults || {};
    const list = Array.isArray(defaults.defaultModels) ? defaults.defaultModels : [];
    if (list.length) return list.length;
    const models = template.models;
    if (Array.isArray(models)) return models.length;
    return defaults.autoModels || defaults.fetchModels ? 1 : 0;
  }

  function renderTemplatesPage() {
    const items = tpl.items || [];
    const summary = $("#template-summary");
    if (summary) {
      const official = items.filter((item) => item.kind === "official").length;
      const protocols = new Set(items.map((item) => item.defaults?.protocol).filter(Boolean));
      const requestLevel = items.filter((item) => item.privacy?.support === "request").length;
      summary.innerHTML = `
        <div class="template-stat"><span class="k">模板</span><b>${num(items.length)}</b><span class="s">官方 ${num(official)} · 自定义 ${num(Math.max(0, items.length - official))}</span></div>
        <div class="template-stat"><span class="k">协议</span><b>${num(protocols.size)}</b><span class="s">${esc([...protocols].join(" / ") || "—")}</span></div>
        <div class="template-stat signal"><span class="k">请求级隐私</span><b>${num(requestLevel)}</b><span class="s">可选 ZDR 的模板</span></div>
        <div class="template-stat"><span class="k">默认模型</span><b>${num(items.reduce((sum, item) => sum + templateModels(item), 0))}</b><span class="s">来自模板默认值</span></div>`;
    }

    const body = $("#template-table tbody");
    if (!body) return;
    if (!items.length) {
      body.innerHTML = `<tr><td colspan="7"><div class="empty-line">模板目录为空。检查内置模板或重新加载配置。</div></td></tr>`;
      return;
    }
    body.innerHTML = items
      .map((template) => {
        const privacy = template.privacy || {};
        const support = privacy.support || "none";
        const tone = support === "request" ? "signal" : support === "account" ? "info" : "amber";
        return `<tr data-template-row="${esc(template.id)}">
          <td><div class="name-cell"><b>${esc(template.label || template.id)}</b></div><div class="tiny muted mono">${esc(template.id)} · v${esc(template.version || "—")}</div></td>
          <td><span class="pill">${esc(template.kind || "official")}</span></td>
          <td class="mono tiny">${esc(template.defaults?.protocol || "—")}</td>
          <td><span class="addr" title="${esc(template.defaults?.baseUrl || "")}">${esc(template.defaults?.baseUrl || "自定义地址")}</span></td>
          <td class="num">${num(templateModels(template))}</td>
          <td><span class="pill ${tone}">${esc(SUPPORT_TEXT[support] || support)}</span></td>
          <td class="right"><span class="actions">
            <button class="btn ghost sm" data-template-view="${esc(template.id)}">查看</button>
            <button class="btn primary sm" data-template-use="${esc(template.id)}">用于接入</button>
          </span></td>
        </tr>`;
      })
      .join("");
  }

  function showTemplate(template) {
    const defaults = template.defaults || {};
    const privacy = template.privacy || {};
    openModal(
      template.label || template.id,
      `${template.id} · v${template.version || "—"} · ${template.kind || "official"}`,
      `<div class="kv">
        <span class="k">协议</span><span class="v mono">${esc(defaults.protocol || "—")}</span>
        <span class="k">默认地址</span><span class="v mono">${esc(defaults.baseUrl || "自定义")}</span>
        <span class="k">默认模型</span><span class="v">${esc((defaults.defaultModels || []).join("、") || "—")}</span>
        <span class="k">必填字段</span><span class="v">${esc((template.requiredFields || []).join("、") || "—")}</span>
        <span class="k">隐私能力</span><span class="v">${esc(SUPPORT_TEXT[privacy.support] || privacy.support || "—")}${privacy.zdrDefault ? " · 默认 ZDR" : ""}</span>
        <span class="k">文档</span><span class="v">${template.docsUrl ? `<a href="${esc(template.docsUrl)}" target="_blank" rel="noreferrer">打开</a>` : "—"}</span>
      </div>
      <p class="hint" style="margin-top:12px">${esc(template.description || "未提供描述。")}</p>`
    );
  }

  async function loadTemplates(force = true) {
    if (tpl.loading) return;
    tpl.loading = true;
    try {
      if (force) MG.state.templates = [];
      await MG.refreshTemplates();
      tpl.items = Array.isArray(MG.state.templates) ? MG.state.templates : [];
      renderTemplatesPage();
    } finally {
      tpl.loading = false;
    }
  }

  function bindTemplates() {
    $("#templates-refresh")?.addEventListener("click", () => loadTemplates(true).catch((error) => toast(error.message, "error")));
    $("#template-table")?.addEventListener("click", (event) => {
      const view = event.target.closest("[data-template-view]");
      if (view) {
        const template = tpl.items.find((item) => item.id === view.dataset.templateView);
        if (template) showTemplate(template);
        return;
      }
      const use = event.target.closest("[data-template-use]");
      if (use) {
        const id = use.dataset.templateUse;
        MG.openAddModal("official");
        requestAnimationFrame(() => {
          const card = document.querySelector(`#modal-body [data-template="${id}"]`);
          if (card) card.click();
        });
      }
    });
  }

  MG.registerView("templates", () => loadTemplates(true));
  MG.registerView("free", loadFree);
  document.addEventListener("DOMContentLoaded", () => {
    bind();
    bindTemplates();
  });
})();
