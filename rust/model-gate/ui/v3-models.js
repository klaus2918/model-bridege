/* v3 模型目录与单价表。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, $$, esc, num, compactNum, api, toast, state, openAddModal, openModal, closeModal } = MG;

  const catalog = {
    page: 1,
    pageSize: 25,
    total: 0,
    rows: [],
    loaded: false,
  };
  const pricing = {
    entries: [],
    fx: 7.2,
    loaded: false,
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
    const data = await api(`/api/models/catalog?${params}`);
    let rows = Array.isArray(data.rows) ? data.rows : [];
    if (source) rows = rows.filter((row) => (row.sources || []).includes(source));
    catalog.rows = rows;
    catalog.total = Number(data.total || rows.length);
    catalog.loaded = true;
    renderCatalog(data);
  }

  function renderCatalog(data = {}) {
    const rows = catalog.rows;
    const priced = rows.filter((row) => row.costHint).length;
    const aliased = rows.filter((row) => (row.aliases || []).length).length;
    const defaults = rows.filter((row) => row.default).length;
    $("#model-kpis").innerHTML = `
      <div class="stat"><span class="k">模型总数</span><span class="v">${num(catalog.total)}</span><span class="s">当前筛选</span></div>
      <div class="stat signal"><span class="k">已配置单价</span><span class="v">${num(priced)}</span><span class="s">本页</span></div>
      <div class="stat"><span class="k">有别名</span><span class="v">${num(aliased)}</span><span class="s">本页</span></div>
      <div class="stat amber"><span class="k">默认模型</span><span class="v">${num(defaults)}</span><span class="s">本页</span></div>`;
    const tbody = $("#model-catalog-table tbody");
    tbody.innerHTML = rows.length
      ? rows
          .map((row, index) => {
            const upstreams = (row.upstreams || []).map((name) => `<span class="pill">${esc(name)}</span>`).join(" ");
            const sources = (row.sources || []).map((source) => `<span class="badge">${esc(SOURCE_LABEL[source] || source)}</span>`).join(" ");
            const aliases = (row.aliases || []).map((alias) => `<code>${esc(alias)}</code>`).join(" ");
            return `<tr>
              <td><span class="name-cell">${esc(row.model)}${row.default ? ' <span class="pill signal">默认</span>' : ""}</span></td>
              <td><div class="tagline">${upstreams || "—"}</div></td>
              <td>${sources || "—"}</td>
              <td>${aliases || "—"}</td>
              <td>${row.minPlan ? `<span class="pill amber">${esc(row.minPlan)}</span>` : "—"}</td>
              <td class="num">${priceText(row.costHint)}</td>
              <td class="right"><div class="actions">
                <button class="btn ghost sm" data-model-default="${esc(row.model)}">设为默认</button>
                <button class="btn ghost sm" data-model-price="${esc(row.model)}" data-price-upstream="${esc((row.upstreams || [])[0] || "")}">定价</button>
              </div></td>
            </tr>`;
          })
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有匹配的模型。</div></td></tr>`;
    const pages = Math.max(1, Math.ceil(catalog.total / catalog.pageSize));
    $("#model-page-info").textContent = `第 ${catalog.page} / ${pages} 页 · 共 ${num(catalog.total)} 条`;
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
          .map((entry, index) => `<tr>
            <td>${entry.upstream ? esc(entry.upstream) : '<span class="muted">全部</span>'}</td>
            <td><code>${esc(entry.model || "")}</code></td>
            <td>${entry.type === "quota" ? '<span class="pill amber">额度制</span>' : '<span class="pill">按量</span>'}</td>
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
      : `<tr><td colspan="9"><div class="empty-line">还没有单价条目。</div></td></tr>`;
    $("#pricing-empty").hidden = Boolean(pricing.entries.length);
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

  function openEntry(index = -1) {
    const entry = index >= 0 ? pricing.entries[index] : {};
    openModal(index >= 0 ? "编辑单价条目" : "新增单价条目", "单位：金额 / 百万 token；空值按 0 或未配置处理。", entryForm(entry, index));
    $("#entry-cancel").addEventListener("click", closeModal);
    $("#entry-save").addEventListener("click", () => {
      const type = $("#entry-type").value;
      const extraText = $("#entry-extra").value.trim();
      let extra = {};
      if (extraText) {
        try {
          extra = JSON.parse(extraText);
        } catch {
          toast("高级 JSON 不合法", "error");
          return;
        }
      }
      const item = {
        ...(index >= 0 ? pricing.entries[index] : {}),
        upstream: $("#entry-upstream").value.trim(),
        model: $("#entry-model").value.trim(),
        type,
        currency: $("#entry-currency").value,
        input: Number($("#entry-input").value) || 0,
        output: Number($("#entry-output").value) || 0,
        cacheRead: Number($("#entry-cache").value) || 0,
        label: $("#entry-label").value.trim(),
      };
      if (!item.model) return toast("模型不能为空", "error");
      if (extra.plan) item.plan = extra.plan;
      else delete item.plan;
      if (extra.peak) item.peak = extra.peak;
      else delete item.peak;
      if (type === "quota") {
        delete item.input;
        delete item.output;
        delete item.cacheRead;
      }
      if (index >= 0) pricing.entries[index] = item;
      else pricing.entries.unshift(item);
      closeModal();
      renderPricing();
    });
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
      pricing.entries.unshift({
        upstream: $("#pricing-add-upstream").value,
        model,
        type: $("#pricing-add-type").value,
        currency: $("#pricing-add-currency").value,
        input: Number($("#pricing-add-input").value) || 0,
        output: Number($("#pricing-add-output").value) || 0,
        cacheRead: 0,
        label: $("#pricing-add-label").value.trim(),
      });
      renderPricing();
      $("#pricing-add-model").value = "";
      $("#pricing-add-label").value = "";
    });
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
