/* v4 模型目录与单价表。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, $$, esc, num, compactNum, api, toast, state, openAddModal, openModal, closeModal } = MG;

  const catalog = {
    page: 1,
    pageSize: 25,
    total: 0,
    rows: [],
    usage: new Map(),
    loaded: false,
  };
  const pricing = {
    entries: [],
    fx: 7.2,
    loaded: false,
    editing: -1,
  };
  let bound = false;

  const SOURCE_LABEL = { explicit: "白名单", fetched: "目录拉取", catalog: "套餐门槛" };

  function currentTab() {
    return $(".seg-tab.active[data-model-tab]")?.dataset.modelTab || "catalog";
  }

  function setTab(tab) {
    $$("#model-tabs .seg-tab").forEach((button) => button.classList.toggle("active", button.dataset.modelTab === tab));
    $("#model-catalog-panel").hidden = tab !== "catalog";
    $("#model-pricing-panel").hidden = tab !== "pricing";
    const title = $("#model-page-title");
    const sub = $("#model-page-sub");
    if (title) title.textContent = tab === "pricing" ? "单价表" : "模型目录";
    if (sub) {
      sub.textContent =
        tab === "pricing"
          ? "上游固定价优先于任意上游；精确匹配优先于通配，保存后可重算历史费用。"
          : "模型清单、别名、上游映射与定价状态；未定价模型可一键聚焦。";
    }
    if (tab === "pricing") loadPricing().catch((error) => toast(error.message, "error"));
  }

  function priceText(quote) {
    if (!quote) return "—";
    if (quote.type === "quota") return `额度制${quote.plan ? ` · ${quote.plan}` : ""}`;
    const currency = quote.currency === "CNY" ? "¥" : "$";
    return `${currency}${quote.input ?? "—"} / ${currency}${quote.output ?? "—"}`;
  }

  function fillUpstreamSelects() {
    const names = Array.from(
      new Set([
        ...state.access.map((item) => item.id),
        ...Object.keys(state.config?.upstreams || {}),
      ])
    ).sort();
    const filter = $("#model-upstream");
    if (filter) {
      const current = filter.value;
      filter.innerHTML = `<option value="">全部上游</option>${names.map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("")}`;
      filter.value = names.includes(current) ? current : "";
    }
    const add = $("#pricing-add-upstream");
    if (add) {
      add.innerHTML = `<option value="">全部上游</option>${names.map((name) => `<option value="${esc(name)}">${esc(name)}</option>`).join("")}`;
    }
  }

  async function loadCatalog() {
    const params = new URLSearchParams({
      page: String(catalog.page),
      page_size: String(catalog.pageSize),
    });
    const search = $("#model-search")?.value.trim();
    const upstream = $("#model-upstream")?.value;
    const price = $("#model-price-filter")?.value;
    const source = $("#model-source-filter")?.value;
    if (search) params.set("q", search);
    if (upstream) params.set("upstream", upstream);
    if (price) params.set("price", price);
    const [data, usageData] = await Promise.all([
      api(`/api/models/catalog?${params}`),
      api("/api/usage/models?range=1&limit=500").catch(() => ({ rows: [] })),
    ]);
    let rows = Array.isArray(data.rows) ? data.rows : [];
    if (source) rows = rows.filter((row) => (row.sources || []).includes(source));
    catalog.rows = rows;
    catalog.total = Number(data.total || rows.length);
    catalog.usage = new Map(
      (Array.isArray(usageData.rows) ? usageData.rows : []).map((row) => [row.model, row])
    );
    catalog.loaded = true;
    renderCatalog(data);
  }

  function renderCatalog(data = {}) {
    const rows = catalog.rows;
    const priced = rows.filter((row) => row.costHint).length;
    const unpriced = rows.length - priced;
    const aliased = rows.reduce((sum, row) => sum + (row.aliases || []).length, 0);
    $("#model-kpis").innerHTML = `
      <button class="model-kpi" data-model-kpi="all">
        <span class="k">模型总数</span><span class="v">${num(catalog.total)}</span><span class="s">当前筛选 · 本页 ${rows.length}</span>
      </button>
      <button class="model-kpi signal" data-model-kpi="priced">
        <span class="k">已定价</span><span class="v">${num(priced)}</span><span class="s">本页命中单价</span>
      </button>
      <button class="model-kpi ${unpriced ? "amber" : ""}" data-model-kpi="missing">
        <span class="k">未定价</span><span class="v">${num(unpriced)}</span><span class="s">点击只看未定价</span>
      </button>
      <button class="model-kpi" data-model-kpi="aliased">
        <span class="k">别名</span><span class="v">${num(aliased)}</span><span class="s">本页别名总数</span>
      </button>`;
    const tbody = $("#model-catalog-table tbody");
    tbody.innerHTML = rows.length
      ? rows
          .map((row) => {
            const upstreams = (row.upstreams || []).map((name) => `<span class="pill">${esc(name)}</span>`).join(" ");
            const sources = (row.sources || []).map((source) => `<span class="pill">${esc(SOURCE_LABEL[source] || source)}</span>`).join(" ");
            const aliases = (row.aliases || []).map((alias) => `<code>${esc(alias)}</code>`).join(" ");
            const usage = catalog.usage.get(row.model) || {};
            const pricedRow = Boolean(row.costHint);
            return `<tr>
              <td><div class="model-name">${esc(row.model)}${row.default ? ' <span class="pill signal">默认</span>' : ""}</div>
                ${row.minPlan ? `<div class="tiny amber">套餐门槛 ${esc(row.minPlan)}</div>` : ""}</td>
              <td><div class="tagline">${upstreams || "—"}</div></td>
              <td>${sources || "—"}</td>
              <td>${aliases || "—"}</td>
              <td class="num">${priceText(row.costHint)}</td>
              <td class="num">${num(usage.requests || 0)}</td>
              <td><span class="pill ${pricedRow ? "signal" : "amber"}">${pricedRow ? "已定价" : "未定价"}</span></td>
              <td class="right"><div class="actions">
                <button class="btn ghost sm" data-model-price="${esc(row.model)}" data-price-upstream="${esc((row.upstreams || [])[0] || "")}">定价</button>
                <button class="btn ghost sm" data-model-default="${esc(row.model)}">设为默认</button>
              </div></td>
            </tr>`;
          })
          .join("")
      : `<tr><td colspan="8"><div class="empty-line">没有匹配的模型。</div></td></tr>`;
    const pages = Math.max(1, Math.ceil(catalog.total / catalog.pageSize));
    $("#model-page-info").textContent = `第 ${catalog.page} / ${pages} 页 · 共 ${num(catalog.total)} 条`;
    $$("#model-kpis [data-model-kpi]").forEach((button) => {
      button.addEventListener("click", () => {
        const kind = button.dataset.modelKpi;
        const filter = $("#model-price-filter");
        if (!filter) return;
        if (kind === "priced") filter.value = "priced";
        else if (kind === "missing") filter.value = "missing";
        else if (kind === "all") filter.value = "";
        else if (kind === "aliased") {
          filter.value = "";
          $("#model-search").value = "";
          toast("别名在表格中查看；可用搜索框筛别名", "ok");
          return;
        }
        catalog.page = 1;
        loadCatalog().catch((error) => toast(error.message, "error"));
      });
    });
  }

  async function setDefaultModel(model) {
    const cfg = state.config || {};
    const upstreams = cfg.upstreams && typeof cfg.upstreams === "object" ? { ...cfg.upstreams } : {};
    let matched = false;
    for (const [name, upstream] of Object.entries(upstreams)) {
      if (upstream && typeof upstream === "object" && (upstream.models || []).includes(model)) {
        upstream.default = true;
        matched = true;
      } else if (upstream && typeof upstream === "object") {
        upstream.default = false;
      }
    }
    if (!matched) return toast("当前页没有该模型的上游声明", "error");
    try {
      await api("/api/config", { method: "POST", body: JSON.stringify({ config: { ...cfg, upstreams }, backup: true }) });
      await MG.refreshConfig();
      await loadCatalog();
      toast(`默认模型已设为 ${model}`, "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function loadPricing() {
    const data = await api("/api/pricing");
    const table = data.pricing && typeof data.pricing === "object" ? data.pricing : {};
    pricing.entries = Array.isArray(table.entries) ? table.entries : [];
    pricing.fx = Number(table.fx?.USD || data.fx?.USD || 7.2);
    pricing.loaded = true;
    $("#pricing-fx").value = pricing.fx;
    $("#pricing-count").textContent = `${pricing.entries.length} 条`;
    $("#pricing-hint").textContent = data.diagnostics?.length ? `${data.diagnostics.length} 条诊断` : "保存后立即生效";
    renderPricing();
  }

  function renderPricing() {
    const tbody = $("#pricing-table tbody");
    tbody.innerHTML = pricing.entries.length
      ? pricing.entries
          .map((entry, index) => `<tr class="${pricing.editing === index ? "editing" : ""}">
            <td>${entry.upstream ? `<span class="pill">${esc(entry.upstream)}</span>` : '<span class="muted">全部上游</span>'}</td>
            <td><code>${esc(entry.model || "")}</code></td>
            <td>${entry.type === "quota" ? '<span class="pill amber">额度制</span>' : '<span class="pill signal">按量</span>'}</td>
            <td>${esc(entry.currency || "USD")}</td>
            <td class="num">${entry.type === "quota" ? "—" : num(entry.input)}</td>
            <td class="num">${entry.type === "quota" ? "—" : num(entry.output)}</td>
            <td class="num">${entry.type === "quota" ? "—" : num(entry.cacheRead)}</td>
            <td>${esc(entry.label || "—")}</td>
            <td class="right"><div class="actions">
              <button class="btn ghost sm" data-price-edit="${index}">编辑</button>
              <button class="btn danger sm" data-price-delete="${index}">删除</button>
            </div></td>
          </tr>`)
          .join("")
      : `<tr><td colspan="9"><div class="empty-line">还没有单价条目。用右侧编辑器新增，或编辑 JSON 批量导入。</div></td></tr>`;
    $("#pricing-empty").hidden = Boolean(pricing.entries.length);
    $("#pricing-count").textContent = `${pricing.entries.length} 条`;
  }

  function entryForm(entry = {}, index = -1) {
    return `<div class="form-grid">
      <label class="field"><span>上游（留空 = 全部）</span><input class="input" id="entry-upstream" value="${esc(entry.upstream || "")}"></label>
      <label class="field"><span>模型（支持 * 通配）</span><input class="input" id="entry-model" value="${esc(entry.model || "")}"></label>
      <label class="field"><span>计费类型</span><select class="input" id="entry-type"><option value="per-token" ${entry.type !== "quota" ? "selected" : ""}>按量计费</option><option value="quota" ${entry.type === "quota" ? "selected" : ""}>额度制</option></select></label>
      <label class="field"><span>币种</span><select class="input" id="entry-currency"><option value="USD" ${entry.currency !== "CNY" ? "selected" : ""}>USD</option><option value="CNY" ${entry.currency === "CNY" ? "selected" : ""}>CNY</option></select></label>
      <label class="field"><span>输入 / 百万 token</span><input class="input" id="entry-input" type="number" step="0.0001" value="${esc(entry.input ?? "")}"></label>
      <label class="field"><span>输出 / 百万 token</span><input class="input" id="entry-output" type="number" step="0.0001" value="${esc(entry.output ?? "")}"></label>
      <label class="field"><span>缓存读取 / 百万 token</span><input class="input" id="entry-cache" type="number" step="0.0001" value="${esc(entry.cacheRead ?? "")}"></label>
      <label class="field"><span>标签</span><input class="input" id="entry-label" value="${esc(entry.label || "")}"></label>
      <label class="field span-2"><span>峰谷 / 套餐高级 JSON（可选）</span><textarea class="input" id="entry-extra" rows="3" spellcheck="false">${esc(entry.plan || entry.peak ? JSON.stringify({ plan: entry.plan, peak: entry.peak }, null, 2) : "")}</textarea></label>
    </div>
    <div class="modal-actions"><button class="btn ghost" id="entry-cancel">取消</button><button class="btn primary" id="entry-save">${index >= 0 ? "保存条目" : "添加条目"}</button></div>`;
  }

  function setEditorEntry(entry = {}, index = -1) {
    pricing.editing = index;
    $("#pricing-add-upstream").value = entry.upstream || "";
    $("#pricing-add-model").value = entry.model || "";
    $("#pricing-add-type").value = entry.type === "quota" ? "quota" : "per-token";
    $("#pricing-add-currency").value = entry.currency === "CNY" ? "CNY" : "USD";
    $("#pricing-add-input").value = entry.input ?? "";
    $("#pricing-add-output").value = entry.output ?? "";
    $("#pricing-add-label").value = entry.label || "";
    $("#pricing-editor-title").textContent = index >= 0 ? "编辑条目" : "新增条目";
    $("#pricing-editor-mode").textContent = index >= 0 ? `编辑 #${index + 1}` : "新增";
    $("#pricing-editor-mode").className = `pill ${index >= 0 ? "amber" : ""}`;
    $("#pricing-add").textContent = index >= 0 ? "保存条目" : "添加到最前";
    $("#pricing-editor-cancel").hidden = index < 0;
  }

  function resetEditor() {
    setEditorEntry({}, -1);
  }

  function openEntry(index = -1) {
    const entry = index >= 0 ? pricing.entries[index] : {};
    setEditorEntry(entry, index);
    $(".pricing-editor")?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }

  async function savePricing() {
    if (!pricing.loaded) await loadPricing();
    const payload = {
      pricing: {
        version: 1,
        fx: { USD: Number($("#pricing-fx").value) || 7.2 },
        entries: pricing.entries,
      },
      backup: $("#pricing-backup").checked,
      recompute: $("#pricing-recompute").checked,
    };
    try {
      const data = await api("/api/pricing", { method: "POST", body: JSON.stringify(payload) });
      toast(`单价已保存${data.recomputed ? `，重算 ${data.recomputed} 条历史` : ""}`, "ok");
      await loadPricing();
      await loadCatalog();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function openPricingJson() {
    openModal(
      "编辑单价 JSON",
      "保存前先应用到编辑器；不会直接落盘。",
      `<textarea class="input code-area" id="pricing-json-text" rows="18" spellcheck="false">${esc(
        JSON.stringify({ version: 1, fx: { USD: pricing.fx }, entries: pricing.entries }, null, 2)
      )}</textarea>
      <div class="modal-actions"><button class="btn ghost" id="pricing-json-cancel">取消</button><button class="btn primary" id="pricing-json-apply">应用到编辑器</button></div>`
    );
    $("#pricing-json-cancel").addEventListener("click", closeModal);
    $("#pricing-json-apply").addEventListener("click", () => {
      try {
        const parsed = JSON.parse($("#pricing-json-text").value);
        pricing.entries = Array.isArray(parsed.entries) ? parsed.entries : [];
        pricing.fx = Number(parsed.fx?.USD || parsed.fx?.usd || pricing.fx);
        $("#pricing-fx").value = pricing.fx;
        $("#pricing-count").textContent = `${pricing.entries.length} 条`;
        renderPricing();
        closeModal();
        toast("已应用到编辑器，记得保存", "ok");
      } catch (error) {
        toast(`JSON 解析失败：${error.message}`, "error");
      }
    });
  }

  function bind() {
    if (bound) return;
    bound = true;
    $("#models-refresh").addEventListener("click", () => {
      if (currentTab() === "pricing") loadPricing();
      else loadCatalog();
    });
    $$("#model-tabs .seg-tab").forEach((button) => button.addEventListener("click", () => setTab(button.dataset.modelTab)));
    ["#model-search", "#model-upstream", "#model-price-filter", "#model-source-filter"].forEach((selector) => {
      $(selector).addEventListener("input", () => {
        catalog.page = 1;
        loadCatalog().catch((error) => toast(error.message, "error"));
      });
      $(selector).addEventListener("change", () => {
        catalog.page = 1;
        loadCatalog().catch((error) => toast(error.message, "error"));
      });
    });
    $$("[data-model-page]").forEach((button) =>
      button.addEventListener("click", () => {
        const pages = Math.max(1, Math.ceil(catalog.total / catalog.pageSize));
        if (button.dataset.modelPage === "first") catalog.page = 1;
        if (button.dataset.modelPage === "prev") catalog.page = Math.max(1, catalog.page - 1);
        if (button.dataset.modelPage === "next") catalog.page = Math.min(pages, catalog.page + 1);
        if (button.dataset.modelPage === "last") catalog.page = pages;
        loadCatalog().catch((error) => toast(error.message, "error"));
      })
    );
    $("#model-page-size").addEventListener("change", () => {
      catalog.pageSize = Number($("#model-page-size").value) || 25;
      catalog.page = 1;
      loadCatalog().catch((error) => toast(error.message, "error"));
    });
    $("#model-catalog-table").addEventListener("click", (event) => {
      const defaultButton = event.target.closest("[data-model-default]");
      if (defaultButton) return setDefaultModel(defaultButton.dataset.modelDefault);
      const priceButton = event.target.closest("[data-model-price]");
      if (priceButton) {
        setTab("pricing");
        $("#pricing-add-model").value = priceButton.dataset.modelPrice;
        $("#pricing-add-upstream").value = priceButton.dataset.priceUpstream || "";
      }
    });
    $("#pricing-add").addEventListener("click", () => {
      const model = $("#pricing-add-model").value.trim();
      if (!model) return toast("模型不能为空", "error");
      const type = $("#pricing-add-type").value;
      const item = {
        upstream: $("#pricing-add-upstream").value,
        model,
        type,
        currency: $("#pricing-add-currency").value,
        input: Number($("#pricing-add-input").value) || 0,
        output: Number($("#pricing-add-output").value) || 0,
        cacheRead: 0,
        label: $("#pricing-add-label").value.trim(),
      };
      if (type === "quota") {
        delete item.input;
        delete item.output;
        delete item.cacheRead;
      }
      if (pricing.editing >= 0) {
        pricing.entries[pricing.editing] = item;
        toast("条目已更新，记得保存单价表", "ok");
      } else {
        pricing.entries.unshift(item);
        toast("条目已添加，记得保存单价表", "ok");
      }
      renderPricing();
      resetEditor();
    });
    $("#pricing-editor-cancel").addEventListener("click", resetEditor);
    $("#pricing-table").addEventListener("click", (event) => {
      const edit = event.target.closest("[data-price-edit]");
      if (edit) return openEntry(Number(edit.dataset.priceEdit));
      const del = event.target.closest("[data-price-delete]");
      if (del) {
        pricing.entries.splice(Number(del.dataset.priceDelete), 1);
        renderPricing();
      }
    });
    $("#pricing-save").addEventListener("click", savePricing);
    $("#pricing-json-open").addEventListener("click", openPricingJson);
  }

  MG.registerView("models", async () => {
    await Promise.all([MG.refreshAccess(), MG.refreshConfig()]);
    fillUpstreamSelects();
    if (currentTab() === "pricing") {
      await loadPricing();
    } else {
      await loadCatalog();
    }
  });

  document.addEventListener("DOMContentLoaded", bind);
})();
