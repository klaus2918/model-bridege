/* v4 提示词列表、详情、会话演进与差异/工具分析。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, $$, esc, num, fmtTime, api, toast, openModal, closeModal } = MG;

  const p = {
    tab: "list",
    range: "7d",
    model: "",
    session: "",
    q: "",
    page: 1,
    pageSize: 50,
    rows: [],
    total: 0,
    facets: { models: [], sessions: [] },
    evolution: [],
    baseId: "",
    targetId: "",
    diff: null,
  };
  let bound = false;

  // 作用域持久化：分页位置与每页条数刷新后保持。
  const SCOPE_KEY = "mg.v4.prompts.scope";

  function saveScope() {
    try {
      localStorage.setItem(
        SCOPE_KEY,
        JSON.stringify({ pageSize: p.pageSize, range: p.range })
      );
    } catch {
      /* 隐私模式下不可写，忽略 */
    }
  }

  function restoreScope() {
    let saved = {};
    try {
      saved = JSON.parse(localStorage.getItem(SCOPE_KEY) || "{}") || {};
    } catch {
      saved = {};
    }
    if (Number(saved.pageSize)) p.pageSize = Number(saved.pageSize);
    if (typeof saved.range === "string" && saved.range) p.range = saved.range;
    const size = $("#prompt-page-size");
    if (size) size.value = String(p.pageSize);
  }

  function pages() {
    return Math.max(1, Math.ceil(p.total / p.pageSize));
  }

  function query(extra = {}) {
    const params = new URLSearchParams({
      range: p.range,
      limit: p.pageSize,
      offset: (p.page - 1) * p.pageSize,
    });
    if (p.model) params.set("model", p.model);
    if (p.session) params.set("session", p.session);
    if (p.q) params.set("q", p.q);
    Object.entries(extra).forEach(([key, value]) => {
      if (value !== "" && value != null) params.set(key, String(value));
    });
    return params;
  }

  function setTab(tab) {
    p.tab = tab;
    $$("#prompt-tabs .seg-tab").forEach((button) => button.classList.toggle("active", button.dataset.promptTab === tab));
    $("#prompt-list-view").hidden = tab !== "list";
    $("#prompt-evolution-view").hidden = tab !== "evolution";
    if (tab === "evolution") loadEvolution().catch((error) => toast(error.message, "error"));
  }

  // 序号守卫：跨页跳转（如请求抽屉「打开提示词」）会与视图加载器并发查询，
  // 只让最后一次查询落地，避免旧筛选的响应晚到覆盖新结果。
  let listSeq = 0;

  async function loadList() {
    const seq = ++listSeq;
    const data = await api(`/api/prompts?${query()}`);
    if (seq !== listSeq) return;
    p.rows = Array.isArray(data.rows) ? data.rows : [];
    p.total = Number(data.total || p.rows.length);
    p.facets = {
      models: Array.isArray(data.facets?.models) ? data.facets.models : p.facets.models,
      sessions: Array.isArray(data.facets?.sessions) ? data.facets.sessions : p.facets.sessions,
    };
    fillFacets();
    $("#prompt-disabled").hidden = Boolean(data.capture);
    $("#prompt-hint").textContent = data.stats
      ? `${data.stats.records || 0} 条 · ${((data.stats.bytes || 0) / 1024 / 1024).toFixed(1)} MB · 脱敏 ${data.stats.redactions || 0} 处`
      : `共 ${p.total} 条`;
    // 翻页越界时回正：总数在别的会话被清理后 offset 可能超过最后一页。
    const totalPages = pages();
    if (p.page > totalPages && totalPages > 0) {
      p.page = totalPages;
      return loadList();
    }
    $("#prompt-page-info").textContent = `第 ${p.page} / ${totalPages} 页 · 共 ${num(p.total)} 条`;
    renderList();
  }

  function fillFacets() {
    const modelHost = $("#prompt-model");
    if (modelHost) {
      modelHost.innerHTML = `<option value="">全部模型</option>${p.facets.models.map((model) => `<option value="${esc(model)}">${esc(model)}</option>`).join("")}`;
      modelHost.value = p.facets.models.includes(p.model) ? p.model : "";
    }
    const sessionHost = $("#prompt-session");
    if (sessionHost) {
      const sessions = p.facets.sessions.slice(0, 300);
      // 从请求抽屉跳入时，会话可能超出当前时间范围的 facets（如范围是 7d、请求更旧）：
      // 服务端查询仍按 p.session 过滤，筛选框要把该会话显示出来，保持所见即所查。
      if (p.session && !sessions.includes(p.session)) sessions.unshift(p.session);
      sessionHost.innerHTML = `<option value="">全部会话</option>${sessions
        .map((session) => `<option value="${esc(session)}">${esc(session)}</option>`)
        .join("")}`;
      sessionHost.value = sessions.includes(p.session) ? p.session : "";
    }
  }

  function renderList() {
    const tbody = $("#prompt-table tbody");
    tbody.innerHTML = p.rows.length
      ? p.rows
          .map(
            (row) => `<tr class="clickable" data-prompt-id="${esc(row.id)}">
              <td>${fmtTime(row.ts)}</td><td><code>${esc(row.session || "—")}</code></td><td>${esc(row.requested_model || "—")}</td>
              <td class="num">${num(row.message_count)}</td><td class="num">${num(row.size_bytes)}</td>
              <td class="preview-cell">${esc((row.preview || "").slice(0, 120))}${row.truncated ? ' <span class="pill amber">截断</span>' : ""}</td>
              <td class="right"><button class="btn ghost sm" data-prompt-download="${esc(row.id)}">下载</button></td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有提示词记录。</div></td></tr>`;
  }

  async function showDetail(id) {
    try {
      const data = await api(`/api/prompts/${encodeURIComponent(id)}`);
      const messages = Array.isArray(data.messages) ? data.messages : [];
      openModal(
        "提示词详情",
        `${data.requested_model || "—"} · ${data.session || "—"} · ${fmtTime(data.ts)}`,
        `<div class="kv">
          <dt>消息数</dt><dd>${num(data.message_count)}</dd>
          <dt>大小</dt><dd>${num(data.size_bytes)} 字符</dd>
          <dt>角色</dt><dd>${esc(JSON.stringify(data.role_counts || {}))}</dd>
          <dt>脱敏</dt><dd>${num(data.redactions)} 处</dd>
        </div>
        <div class="section-title">消息</div>
        <div class="prompt-messages">${messages
          .map(
            (message, index) => `<details class="message-block" ${index === 0 ? "open" : ""}>
              <summary><span class="pill">${esc(message.role || "?")}</span><span class="muted">#${index + 1} · ${num(String(message.content || "").length)} 字符</span></summary>
              <pre>${esc(message.content || "")}</pre>
            </details>`
          )
          .join("") || '<div class="empty-line">没有消息。</div>'}</div>
        <div class="modal-actions"><button class="btn ghost" id="prompt-download">下载 JSONL</button><button class="btn primary" id="prompt-close">关闭</button></div>`
      );
      $("#prompt-close").addEventListener("click", closeModal);
      $("#prompt-download").addEventListener("click", () => window.open(`/api/prompts/${encodeURIComponent(id)}/download`, "_blank"));
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function loadEvolution() {
    const session = $("#evolution-session").value || p.session || p.facets.sessions[0] || "";
    if (!session) {
      p.evolution = [];
      renderTimeline();
      return;
    }
    const data = await api(`/api/prompts/evolution?session=${encodeURIComponent(session)}&limit=100`);
    p.evolution = Array.isArray(data.rows) ? data.rows : [];
    p.session = session;
    if (!p.baseId && p.evolution.length >= 2) {
      p.baseId = p.evolution[p.evolution.length - 2].id;
      p.targetId = p.evolution[p.evolution.length - 1].id;
    }
    renderTimeline();
    renderEvolutionStats();
    if (p.baseId && p.targetId) loadDiff().catch(() => {});
  }

  function renderTimeline() {
    const host = $("#evolution-timeline");
    host.innerHTML = p.evolution.length
      ? p.evolution
          .slice()
          .reverse()
          .map(
            (row, index) => `<div class="timeline-row ${row.id === p.baseId ? "is-base" : ""} ${row.id === p.targetId ? "is-target" : ""}" data-evolution-id="${esc(row.id)}">
              <div class="timeline-time">${fmtTime(row.ts)}</div>
              <div class="timeline-main">
                <div><code>${esc(row.requested_model || "—")}</code> <span class="muted">· ${num(row.message_count)} 条 · ${num(row.size_bytes)} 字符</span></div>
                <div class="timeline-preview">${esc((row.preview || "").slice(0, 100))}</div>
              </div>
              <div class="timeline-actions">
                <button class="btn ghost sm" data-evolution-base="${esc(row.id)}">基准</button>
                <button class="btn ghost sm" data-evolution-target="${esc(row.id)}">对比</button>
              </div>
            </div>`
          )
          .join("")
      : `<div class="empty-line">该会话还没有记录。</div>`;
    $("#evolution-count").textContent = `${p.evolution.length} 轮`;
  }

  function renderEvolutionStats() {
    const target = p.evolution.find((row) => row.id === p.targetId);
    if (!target) {
      $("#evolution-stats").innerHTML = "";
      return;
    }
    const summary = target.summary || {};
    $("#evolution-stats").innerHTML = `
      <div class="stat"><span class="k">消息</span><span class="v">${num(summary.message_count)}</span></div>
      <div class="stat"><span class="k">字符</span><span class="v">${num(summary.char_count)}</span></div>
      <div class="stat"><span class="k">工具调用</span><span class="v">${num(summary.tool_call_count)}</span></div>
      <div class="stat"><span class="k">系统提示词</span><span class="v">${num(summary.system_chars)}</span></div>`;
    $("#evolution-tool-meta").textContent = `${num(summary.tool_call_count)} 次调用 · ${num(summary.tool_result_count)} 条结果`;
    const chain = target.call_chain || {};
    const calls = Array.isArray(chain.tool_calls) ? chain.tool_calls : [];
    const byTool = {};
    calls.forEach((call) => {
      byTool[call.name] = byTool[call.name] || { name: call.name, calls: 0, unmatched: 0, chars: 0, max: 0 };
      byTool[call.name].calls += 1;
    });
    const rows = Object.values(byTool).sort((a, b) => b.calls - a.calls);
    $("#evolution-tool-table tbody").innerHTML = rows.length
      ? rows
          .map(
            (row) => `<tr><td><code>${esc(row.name)}</code></td><td class="num">${num(row.calls)}</td><td class="num">${num(row.unmatched)}</td><td class="num">0</td><td class="num">—</td><td class="num">—</td><td class="num">—</td></tr>`
          )
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">没有工具调用。</div></td></tr>`;
    $("#evolution-tool-report").textContent = `会话：${p.session}\n消息：${summary.message_count || 0}\n工具调用：${summary.tool_call_count || 0}\n工具：${rows.map((row) => `${row.name}×${row.calls}`).join(", ") || "无"}`;
  }

  async function loadDiff() {
    if (!p.baseId || !p.targetId) return;
    const [meta, base, target] = await Promise.all([
      api(`/api/prompts/diff?base=${encodeURIComponent(p.baseId)}&target=${encodeURIComponent(p.targetId)}`),
      api(`/api/prompts/${encodeURIComponent(p.baseId)}`),
      api(`/api/prompts/${encodeURIComponent(p.targetId)}`),
    ]);
    p.diff = meta.diff || {};
    const baseMessages = Array.isArray(base.messages) ? base.messages : [];
    const targetMessages = Array.isArray(target.messages) ? target.messages : [];
    const stats = p.diff.stats || {};
    $("#evolution-compare").innerHTML = `<span class="mono">${esc(p.baseId.slice(0, 8))}</span> → <span class="mono">${esc(p.targetId.slice(0, 8))}</span>
      <span class="hint">${stats.changed ?? "—"} 处变化 · +${num(stats.added)} / -${num(stats.removed)}</span>
      <a class="ghost tiny" href="/api/prompts/diff/download?base=${encodeURIComponent(p.baseId)}&target=${encodeURIComponent(p.targetId)}" download>下载 diff</a>`;
    const role = $("#evolution-role").value;
    const onlyDiff = $("#evolution-only-diff").checked;
    const messages = (p.diff.messages || []).filter((message) => !role || message.role === role).filter((message) => !onlyDiff || message.status !== "same");
    $("#prompt-diff").innerHTML = messages.length
      ? messages
          .map((message) => {
            const index = message.target_index ?? message.base_index ?? 0;
            const before = baseMessages[message.base_index]?.content || "";
            const after = targetMessages[message.target_index]?.content || "";
            return `<details class="diff-block ${message.status}" ${message.status !== "same" ? "open" : ""}>
              <summary><span class="pill ${message.status === "same" ? "" : message.status === "added" ? "signal" : "amber"}">${esc(message.status)}</span>
                <span class="pill">${esc(message.role)}</span><span class="muted">#${index + 1} · ${num(message.base_chars)} → ${num(message.target_chars)} 字符</span></summary>
              <div class="diff-pair">
                <div><div class="section-title">基准</div><pre>${esc(before.slice(0, 4000))}</pre></div>
                <div><div class="section-title">对比</div><pre>${esc(after.slice(0, 4000))}</pre></div>
              </div>
            </details>`;
          })
          .join("")
      : `<div class="empty-line">当前筛选没有差异。</div>`;
  }

  async function clearPrompts() {
    if (!confirm("清空当前筛选范围的提示词记录？不可撤销。")) return;
    try {
      const data = await api("/api/prompts/clear", {
        method: "POST",
        body: JSON.stringify({ range: p.range, model: p.model, session: p.session, q: p.q }),
      });
      toast(`已清空 ${data.removed || 0} 条`, "ok");
      await loadList();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function vacuum() {
    if (!confirm("回收 SQLite 空闲空间？大库可能耗时较久。")) return;
    try {
      const data = await api("/api/prompts/vacuum", { method: "POST", body: "{}" });
      toast(`回收完成${data.before?.bytes && data.after?.bytes ? `：${(data.before.bytes / 1024 / 1024).toFixed(0)} → ${(data.after.bytes / 1024 / 1024).toFixed(0)} MB` : ""}`, "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function bind() {
    if (bound) return;
    bound = true;
    restoreScope();
    $("#prompts-refresh").addEventListener("click", () => {
      if (p.tab === "list") loadList().catch((error) => toast(error.message, "error"));
      else loadEvolution().catch((error) => toast(error.message, "error"));
    });
    $$("#prompt-tabs .seg-tab").forEach((button) => button.addEventListener("click", () => setTab(button.dataset.promptTab)));
    $("#prompt-range").addEventListener("change", () => {
      p.range = $("#prompt-range").value;
      p.page = 1;
      saveScope();
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#prompt-model").addEventListener("change", () => {
      p.model = $("#prompt-model").value;
      p.page = 1;
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#prompt-session").addEventListener("change", () => {
      p.session = $("#prompt-session").value;
      p.page = 1;
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#prompt-q").addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        p.q = $("#prompt-q").value.trim();
        p.page = 1;
        loadList().catch((error) => toast(error.message, "error"));
      }
    });
    $$("[data-prompt-page]").forEach((button) =>
      button.addEventListener("click", () => {
        const totalPages = pages();
        if (button.dataset.promptPage === "first") p.page = 1;
        if (button.dataset.promptPage === "prev") p.page = Math.max(1, p.page - 1);
        if (button.dataset.promptPage === "next") p.page = Math.min(totalPages, p.page + 1);
        if (button.dataset.promptPage === "last") p.page = totalPages;
        loadList().catch((error) => toast(error.message, "error"));
      })
    );
    $("#prompt-page-size").addEventListener("change", () => {
      p.pageSize = Number($("#prompt-page-size").value) || 50;
      p.page = 1;
      saveScope();
      loadList().catch((error) => toast(error.message, "error"));
    });
    $("#prompt-table").addEventListener("click", (event) => {
      const download = event.target.closest("[data-prompt-download]");
      if (download) {
        event.stopPropagation();
        window.open(`/api/prompts/${encodeURIComponent(download.dataset.promptDownload)}/download`, "_blank");
        return;
      }
      const row = event.target.closest("[data-prompt-id]");
      if (row) showDetail(row.dataset.promptId);
    });
    // 导出按筛选范围全量导出，不带分页 offset。
    $("#prompt-export").addEventListener("click", () => window.open(`/api/prompts/export?${query({ limit: 100000, offset: 0 })}`, "_blank"));
    $("#prompt-clear").addEventListener("click", clearPrompts);
    $("#prompt-vacuum").addEventListener("click", vacuum);
    $("#evolution-session").addEventListener("change", () => {
      p.baseId = "";
      p.targetId = "";
      loadEvolution().catch((error) => toast(error.message, "error"));
    });
    $("#evolution-mode").addEventListener("change", renderTimeline);
    $("#evolution-refresh").addEventListener("click", () => loadEvolution().catch((error) => toast(error.message, "error")));
    $("#evolution-timeline").addEventListener("click", (event) => {
      const base = event.target.closest("[data-evolution-base]");
      const target = event.target.closest("[data-evolution-target]");
      if (base) p.baseId = base.dataset.evolutionBase;
      if (target) p.targetId = target.dataset.evolutionTarget;
      if (base || target) {
        renderTimeline();
        renderEvolutionStats();
        loadDiff().catch((error) => toast(error.message, "error"));
      }
    });
    $("#evolution-role").addEventListener("change", () => p.diff && loadDiff().catch(() => {}));
    $("#evolution-only-diff").addEventListener("change", () => p.diff && loadDiff().catch(() => {}));
    $("#copy-tool-report").addEventListener("click", async () => {
      await navigator.clipboard.writeText($("#evolution-tool-report").textContent);
      toast("评估报告已复制", "ok");
    });
  }

  // 请求抽屉「打开提示词」的落点：清掉会稀释关联的模型/搜索筛选，
  // 把列表聚焦到该请求的会话。由 v4.js 在切视图后立即调用；
  // 视图加载器若也在查列表，loadList 的序号守卫保证最终以这里的筛选为准。
  MG.focusPromptSession = (session) => {
    p.session = session || "";
    p.model = "";
    p.q = "";
    p.page = 1;
    if (p.tab !== "list") setTab("list");
    const sessionHost = $("#prompt-session");
    if (sessionHost) sessionHost.value = p.session;
    const modelHost = $("#prompt-model");
    if (modelHost) modelHost.value = "";
    const search = $("#prompt-q");
    if (search) search.value = "";
    loadList().catch((error) => toast(error.message, "error"));
  };

  MG.registerView("prompts", async () => {
    await MG.refreshConfig();
    const capture = stateCaptureEnabled();
    $("#prompt-disabled").hidden = capture;
    if (!capture) {
      $("#prompt-table tbody").innerHTML = `<tr><td colspan="7"><div class="empty-line">提示词记录未开启。</div></td></tr>`;
      return;
    }
    await loadList();
    const evoHost = $("#evolution-session");
    if (evoHost) {
      evoHost.innerHTML = `<option value="">选择会话</option>${p.facets.sessions
        .slice(0, 300)
        .map((session) => `<option value="${esc(session)}" ${session === p.session ? "selected" : ""}>${esc(session)}</option>`)
        .join("")}`;
    }
    if (p.tab === "evolution") await loadEvolution();
  });

  function stateCaptureEnabled() {
    return Boolean(MG.state?.config?.promptCapture);
  }

  document.addEventListener("DOMContentLoaded", bind);
})();
