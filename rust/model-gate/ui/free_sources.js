
/* ── 免费来源（免密上游） ────────────────────────────────────
   两条免密来源：OpenCode Zen 免密池（需客户端指纹）与 Kilo 免密免费池。

   三个关键约定（与后端一致）：
   1. **筛选 ≠ 可用**：目录里出现的免费候选模型不等于此刻能用，必须探测。
   2. **"暂时不可用" ≠ "已下线"**：限流/超时/网络错误归 transient（保留可见、可退避重试）；
      只有上游明确拒绝（404/401/地区 403/免费层拒绝）才归 unavailable。
   3. **免费模型不进入对外全局清单**：只有显式点「加入全局清单」才会写进顶层 models。 */

/* 状态标签 → 徽标样式（tone 只能取 style.css 里已有的 ok / warn / bad） */
const FREE_STATE_STYLE = {
  available: ["可用", "ok"],
  transient: ["暂时不可用", "warn"],
  suspect: ["疑似可用", "warn"],
  region: ["地区受限", "warn"],
  unavailable: ["明确不可用", "bad"],
};

function freeStateBadge(state) {
  const [label, tone] = FREE_STATE_STYLE[state] || [state || "未知", ""];
  return `<span class="badge ${tone}">${esc(label)}</span>`;
}

function freePrivacyBanner(source) {
  const level = (source.privacy && source.privacy.level) || "info";
  const note = (source.privacy && source.privacy.note) || "";
  if (!note) return "";
  const tone = level === "danger" ? "danger" : (level === "warn" ? "warn" : "");
  return `<div class="callout ${tone}">
    <div class="t">隐私提醒 · ${esc(level)}</div>
    <div>${esc(note)}</div>
  </div>`;
}

function renderFreeSources() {
  const box = $("#free-sources");
  if (!box) return;
  const sources = state.free.sources || [];
  if (!sources.length) {
    box.innerHTML = '<div class="empty">没有可用的免费来源</div>';
    return;
  }
  box.innerHTML = sources.map((s) => {
    const level = state.free.level[s.id] || {};
    const ratio = Number(level.ratio || 0);
    const available = Number(level.available || 0);
    const candidates = Number(level.candidates || 0);
    const levelText = candidates
      ? `鱼缸水位 ${ratio}%（${available}/${candidates} 可用）`
      : "尚未探测";
    const levelTone = !candidates ? "" : (ratio >= 60 ? "ok" : (ratio >= 30 ? "warn" : "bad"));
    const configured = s.configured;
    const actions = configured
      ? `<button class="ghost" data-free-act="catalog" data-source="${esc(s.id)}">查看模型</button>`
      : `<button class="primary" data-free-act="attach" data-source="${esc(s.id)}">一键接入</button>`;
    const transportNote = s.transport
      ? `客户端指纹：${esc(s.transport)}`
      : "标准 OpenAI 兼容端点";
    return `<div class="box free-source-card">
      <div class="box-head">
        <h2>${esc(s.label)}</h2>
        <span class="hint">${esc(s.id)} · ${esc(transportNote)}</span>
      </div>
      <div class="grid grid-2">
        <div class="detail-item">
          <span class="detail-key">端点</span>
          <span class="detail-val mono">${esc(s.baseUrl || "")}</span>
        </div>
        <div class="detail-item">
          <span class="detail-key">鉴权</span>
          <span class="detail-val">免密（${esc(s.auth || "none")}）</span>
        </div>
        <div class="detail-item">
          <span class="detail-key">状态</span>
          <span class="detail-val">${configured
            ? `<span class="badge ok">已接入</span> 已声明 ${fmtInt(s.declaredModels || 0)} 个模型`
            : `<span class="badge">未接入</span>`}</span>
        </div>
        <div class="detail-item">
          <span class="detail-key">可用性</span>
          <span class="detail-val">${levelTone
            ? `<span class="badge ${levelTone}">${esc(levelText)}</span>`
            : `<span class="hint">${esc(levelText)}</span>`}</span>
        </div>
      </div>
      ${freePrivacyBanner(s)}
      <div class="actions">
        ${actions}
        <button class="ghost" data-free-act="probe-all" data-source="${esc(s.id)}"
          title="对全部候选模型逐个发一次真实对话，会消耗免费额度">全部探测</button>
        <a class="ghost" href="${esc(s.docsUrl || "#")}" target="_blank" rel="noreferrer">官方文档</a>
      </div>
    </div>`;
  }).join("");
}

function renderFreeModelTable() {
  const tbody = $("#free-model-table tbody");
  if (!tbody) return;
  const models = state.free.models || [];
  const results = state.free.results || {};
  if (!models.length) {
    tbody.innerHTML = '<tr><td colspan="7" class="empty">还没有模型清单</td></tr>';
    return;
  }
  // 分组顺序：可用 → 疑似 → 暂时不可用 → 地区受限 → 明确不可用
  const order = ["available", "suspect", "transient", "region", "unavailable"];
  const rows = models.map((m) => {
    const r = results[m.id] || {};
    return { m, r, state: r.state || "" };
  });
  rows.sort((a, b) => {
    const oa = order.indexOf(a.state);
    const ob = order.indexOf(b.state);
    const sa = oa < 0 ? order.length : oa;
    const sb = ob < 0 ? order.length : ob;
    if (sa !== sb) return sa - sb;
    return String(a.m.id).localeCompare(String(b.m.id));
  });

  tbody.innerHTML = rows.map(({ m, r, state }) => {
    const efforts = (m.reasoningEfforts || []).join(" / ");
    const privacy = m.mayTrainOnYourPrompts === true
      ? `<span class="badge bad" title="上游可能用你的提示词训练">会记录提示词</span>`
      : (m.mayTrainOnYourPromosts === true ? `<span class="badge bad">会记录提示词</span>` : "");
    const reason = r.reasonCode
      ? `<div><b>${esc(r.reasonLabel || r.reasonCode)}</b>：${esc(r.advice || "")}</div>
         <div class="hint">${esc((r.raw || "").slice(0, 160))}</div>`
      : `<span class="hint">${esc(r.probedAt ? "探测于 " + r.probedAt : "未探测")}</span>`;
    const globalBtn = `<button class="ghost tiny" data-free-act="globalize"
      data-model="${esc(m.id)}" title="把这个模型加入对外模型清单（会写进顶层 models）">加入全局清单</button>`;
    const probeBtn = `<button class="ghost tiny" data-free-act="probe-one"
      data-model="${esc(m.id)}" title="对这一个模型发一次真实对话（消耗免费额度）">探测</button>`;
    return `<tr>
      <td><div class="up-name">${esc(m.id)}</div>
          <div class="hint">${esc(m.name || "")} ${privacy}</div></td>
      <td>${freeStateBadge(state)}</td>
      <td class="num">${m.contextLength ? fmtInt(m.contextLength) : "—"}</td>
      <td>${efforts ? esc(efforts) : "—"}</td>
      <td class="num">${r.latencyMs ? fmtInt(r.latencyMs) + "ms" : "—"}</td>
      <td>${reason}</td>
      <td>${probeBtn} ${globalBtn}</td>
    </tr>`;
  }).join("");

  const hint = $("#free-model-hint");
  if (hint) {
    const total = models.length;
    const available = rows.filter((x) => x.state === "available").length;
    const transient = rows.filter((x) => x.state === "transient").length;
    const unavailable = rows.filter((x) => x.state === "unavailable").length;
    hint.textContent = `共 ${fmtInt(total)} 个候选 · 可用 ${available} · 暂时不可用 ${transient} · 明确不可用 ${unavailable}`;
  }
}

async function loadFreeSources() {
  try {
    const data = await api("/api/free-sources");
    state.free.sources = data.sources || [];
    state.free.states = data.states || [];
    renderFreeSources();
  } catch (err) {
    const hint = $("#free-hint");
    if (hint) {
      hint.textContent = `免费来源加载失败：${err.message}`;
      hint.classList.add("err");
    }
  }
}

async function loadFreeCatalog(sourceId) {
  const box = $("#free-models-box");
  const hint = $("#free-probe-hint");
  if (box) box.hidden = false;
  if (hint) { hint.textContent = "拉取模型目录…"; hint.classList.remove("err"); }
  try {
    const data = await postJson("/api/free-sources/catalog", { source: sourceId });
    state.free.source = sourceId;
    state.free.models = data.models || [];
    state.free.results = {};
    state.free.level[sourceId] = data.level || {};
    const title = $("#free-models-title");
    if (title) {
      const src = (state.free.sources || []).find((s) => s.id === sourceId);
      title.textContent = `${src ? src.label : sourceId} · 模型清单`;
    }
    const meta = $("#free-models-meta");
    if (meta) meta.textContent = `${fmtInt(state.free.models.length)} 个免费候选`;
    renderFreeSources();
    renderFreeModelTable();
    if (hint) hint.textContent = "目录已拉取（零生成、零额度消耗）。点「全部探测」看此刻哪些真能用。";
  } catch (err) {
    if (hint) { hint.textContent = `拉取目录失败：${err.message}`; hint.classList.add("err"); }
  }
}

async function probeFreeModel(sourceId, model) {
  const hint = $("#free-probe-hint");
  if (hint) { hint.textContent = `正在探测 ${model} …`; hint.classList.remove("err"); }
  try {
    const data = await postJson("/api/free-sources/probe", { source: sourceId, model });
    const r = data.result || {};
    state.free.results[model] = r;
    renderFreeModelTable();
    if (hint) {
      const label = FREE_STATE_STYLE[r.state] ? FREE_STATE_STYLE[r.state][0] : r.state;
      hint.textContent = `${model}：${label}${r.advice ? " · " + r.advice : ""}`;
      hint.classList.toggle("err", r.state === "unavailable");
    }
    return r;
  } catch (err) {
    if (hint) { hint.textContent = `探测失败：${err.message}`; hint.classList.add("err"); }
    return null;
  }
}

async function probeAllFreeModels(sourceId) {
  const hint = $("#free-probe-hint");
  const btn = $("#free-probe-all");
  if (btn) btn.disabled = true;
  if (hint) { hint.textContent = "正在逐个探测（会消耗免费额度）…"; hint.classList.remove("err"); }
  try {
    const data = await postJson("/api/free-sources/probe-all", { source: sourceId });
    const results = data.results || [];
    state.free.results = {};
    results.forEach((r) => { state.free.results[r.model] = r; });
    state.free.level[sourceId] = data.level || {};
    renderFreeSources();
    renderFreeModelTable();
    if (hint) {
      const skipped = results.filter((r) => r.skipped).length;
      hint.textContent = `探测完成：${fmtInt(results.length)} 个`
        + (skipped ? `（${skipped} 个因退避跳过）` : "");
      hint.classList.remove("err");
    }
  } catch (err) {
    if (hint) { hint.textContent = `批量探测失败：${err.message}`; hint.classList.add("err"); }
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function attachFreeSource(sourceId) {
  const src = (state.free.sources || []).find((s) => s.id === sourceId);
  const ok = await confirmModal({
    title: "一键接入免密来源",
    text: `来源：${src ? src.label : sourceId}\n`
      + `将写入 bridge.config.json 的上游「${src ? src.upstream : ""}」，不填任何 API Key。\n`
      + `免费模型只登记在该来源自己的清单里，不会自动进入对外的 /v1/models；`
      + `需要对外暴露时，在模型卡上点「加入全局清单」。`,
    confirmLabel: "接入",
    danger: false,
  });
  if (!ok) return;
  const hint = $("#free-hint");
  try {
    const data = await postJson("/api/free-sources/attach", { source: sourceId });
    if (hint) hint.textContent = `已接入 ${data.upstream || sourceId}。`;
    (data.notices || []).forEach((n) => toast(n, true));
    await loadFreeSources();
    await loadFreeCatalog(sourceId);
  } catch (err) {
    if (hint) { hint.textContent = `接入失败：${err.message}`; hint.classList.add("err"); }
    toast(`接入失败：${err.message}`, true);
  }
}

/* 「加入全局清单」：显式写进顶层 models（默认不写，见 D6 决策） */
async function globalizeFreeModel(model) {
  const ok = await confirmModal({
    title: "加入对外模型清单",
    text: `模型：${model}\n\n`
      + `这会把「${model}」写进配置的顶层 models（对外模型清单），`
      + `客户端调用 GET /v1/models 时即可看到它。\n`
      + `注意：免费模型的可用性随时可能变化，清单里过期后调用会失败。`,
    confirmLabel: "加入",
    danger: false,
  });
  if (!ok) return;
  try {
    const cfg = await api("/api/config");
    const models = Array.isArray(cfg.models) ? cfg.models.slice() : [];
    if (models.includes(model)) {
      toast("该模型已在对外模型清单中");
      return;
    }
    models.push(model);
    await postJson("/api/config", { config: { ...cfg, models } });
    toast(`已把 ${model} 加入对外模型清单`);
  } catch (err) {
    toast(`加入失败：${err.message}`, true);
  }
}

function initFreeActions() {
  const refresh = $("#free-refresh");
  if (refresh) refresh.addEventListener("click", () => {
    loadFreeSources();
    if (state.free.source) loadFreeCatalog(state.free.source);
  });

  const probeAll = $("#free-probe-all");
  if (probeAll) {
    probeAll.addEventListener("click", () => {
      if (state.free.source) probeAllFreeModels(state.free.source);
    });
  }

  document.addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-free-act]");
    if (!btn) return;
    const act = btn.dataset.freeAct;
    const source = btn.dataset.source || state.free.source;
    if (act === "attach") { attachFreeSource(source); return; }
    if (act === "catalog") { loadFreeCatalog(source); return; }
    if (act === "probe-all") { probeAllFreeModels(source); return; }
    if (act === "probe-one") { probeFreeModel(source, btn.dataset.model); return; }
    if (act === "globalize") { globalizeFreeModel(btn.dataset.model); return; }
  });
}
