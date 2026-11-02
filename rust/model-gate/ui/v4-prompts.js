// 会话页（M2）：会话列表（含单会话费用）→ 中栏三页签（轨迹流 / 提示词演进 / 工具评估）→ 右节点详情。
// 提示词即轨迹：user / 思考 / 工具调用都是轨迹节点；费用与 token 由 usage_records 按 id join 进来。
(function () {
  const MG = window.MG;
  const { $, $$, api, esc, num, compactNum, fmtTime, fmtMs, toast } = MG;

  const s = {
    range: "7d",
    q: "",
    sessions: [],
    titles: {},
    active: null,
    rows: [], // evolution 行（升序）
    usageById: new Map(),
    rounds: [],
    roundIndex: 0,
    tab: "trace",
    detailTab: "overview",
    detailId: null,
    detail: null,
    diff: { base: "", target: "", role: "", only: true, data: null },
    captureOff: false,
  };

  const PICK_KEY = "mg.v4.sessions.pick";

  function fmtCost(value) {
    const n = Number(value || 0);
    if (!n) return "—";
    if (Math.abs(n) < 0.01) return `¥${n.toFixed(4)}`;
    return `¥${n.toFixed(2)}`;
  }

  function sessionLabel(key) {
    return s.titles[key] || (key && key.length > 22 ? `${key.slice(0, 21)}…` : key) || "(无会话)";
  }

  // ───────────────────────── 列表 ─────────────────────────

  async function load() {
    const listParams = new URLSearchParams({ range: s.range, limit: "200" });
    if (s.q) listParams.set("q", s.q);
    const [sessionsData, promptsData] = await Promise.all([
      api(`/api/usage/sessions?${new URLSearchParams({ range: s.range, limit: "200", sort: "last_ts", order: "desc" })}`),
      api(`/api/prompts?${listParams}`),
    ]);
    s.sessions = Array.isArray(sessionsData.rows) ? sessionsData.rows : [];
    s.captureOff = Boolean(promptsData.capture && promptsData.capture.enabled === false);
    // 会话标题：取该会话最新一条提示词的 preview（服务端只存哈希键时的可读名）。
    s.titles = {};
    const promptRows = Array.isArray(promptsData.rows) ? promptsData.rows.slice().sort((a, b) => Number(b.ts || 0) - Number(a.ts || 0)) : [];
    for (const row of promptRows) {
      const key = row.session || "";
      if (key && !s.titles[key]) s.titles[key] = row.preview || "";
    }
    const captureNote = $("#ses-capture-off");
    if (captureNote) captureNote.hidden = !s.captureOff;
    const meta = $("#ses-meta");
    if (meta) meta.textContent = `${s.sessions.length} 个会话 · ${promptRows.length} 条提示词记录`;
    renderList();
    // 选中：外部跳转指定 > 本地记忆 > 第一个。
    const pending = MG.pendingSession;
    if (pending != null && pending !== "") {
      MG.pendingSession = null;
      try { localStorage.setItem(PICK_KEY, pending); } catch { /* 忽略 */ }
      await openSession(pending);
      return;
    }
    if (!s.active || !s.sessions.some((row) => row.session === s.active)) {
      let pick = null;
      try { pick = localStorage.getItem(PICK_KEY); } catch { /* 忽略 */ }
      pick = pick && s.sessions.some((row) => row.session === pick) ? pick : (s.sessions[0] && s.sessions[0].session) || null;
      await openSession(pick);
    } else {
      renderList();
      await refreshActive();
    }
  }

  function renderList() {
    const host = $("#ses-list");
    if (!host) return;
    host.innerHTML =
      s.sessions
        .map((row) => {
          const key = row.session || "";
          const active = key === s.active;
          const fail = Number(row.errors || 0);
          return `<button class="ses-item${active ? " sel" : ""}" data-ses-key="${esc(key)}">
            <span class="t1">${esc(sessionLabel(key))}</span>
            <span class="t2">${num(row.requests)} 请求 · ${fmtCost(row.cost)} · ${compactNum(row.total_tokens)} tok · ${esc(fmtTime(row.last_ts))}${fail ? ` <span class="pill danger">${fail} 失败</span>` : ""}</span>
          </button>`;
        })
        .join("") || '<div class="empty-line">区间内没有会话</div>';
    const count = $("#ses-count");
    if (count) count.textContent = `${s.sessions.length} 个`;
  }

  // ───────────────────────── 会话明细 ─────────────────────────

  async function openSession(key) {
    s.active = key || null;
    try { localStorage.setItem(PICK_KEY, key || ""); } catch { /* 忽略 */ }
    renderList();
    if (!key) {
      s.rows = [];
      s.rounds = [];
      renderRounds();
      renderTrace();
      return;
    }
    const [evo, usage] = await Promise.all([
      api(`/api/prompts/evolution?session=${encodeURIComponent(key)}&limit=50`),
      api(`/api/usage/list?${new URLSearchParams({ range: s.range, session: key, limit: "500" })}`),
    ]);
    s.rows = (Array.isArray(evo.rows) ? evo.rows : []).slice().sort((a, b) => Number(a.ts || 0) - Number(b.ts || 0));
    s.usageById = new Map((Array.isArray(usage.rows) ? usage.rows : []).map((row) => [String(row.id), row]));
    buildRounds();
    s.roundIndex = Math.max(0, s.rounds.length - 1);
    fillDiffSelects();
    renderLanes();
    renderRounds();
    renderTools();
    renderTrace();
    if (!s.diff.data) setTab(s.tab);
    // 打开会话即选中最新一条记录，右侧详情立刻有内容。
    const newest = s.rows[s.rows.length - 1];
    if (newest && String(s.detailId || "") !== String(newest.id)) {
      openNode(newest.id).catch(() => {});
    }
  }

  async function refreshActive() {
    if (s.active) await openSession(s.active);
  }

  function usageOf(row) {
    return s.usageById.get(String(row.id)) || {};
  }

  // 轮次：call_chain.user_turns 累积值增长 = 新一轮用户输入进入上下文。
  function buildRounds() {
    const rounds = [];
    let prevTurns = 0;
    for (const row of s.rows) {
      const turns = Number(row.call_chain?.user_turns ?? 0);
      if (!rounds.length || turns > prevTurns) {
        rounds.push({ index: rounds.length + 1, rows: [], startId: row.id, endId: row.id, user: row.call_chain?.first_user || row.preview || "" });
      }
      const current = rounds[rounds.length - 1];
      current.rows.push(row);
      current.endId = row.id;
      if (turns > prevTurns) current.user = row.call_chain?.first_user || row.preview || current.user;
      prevTurns = Math.max(prevTurns, turns);
    }
    s.rounds = rounds;
  }

  function renderLanes() {
    const host = $("#ses-lanes");
    if (!host) return;
    if (!s.rows.length) {
      host.innerHTML = "";
      return;
    }
    const maxIn = Math.max(...s.rows.map((row) => Number(usageOf(row).prompt_tokens || 0)), 1);
    const maxOut = Math.max(...s.rows.map((row) => Number(usageOf(row).completion_tokens || 0)), 1);
    const maxTool = Math.max(...s.rows.map((row) => Number(row.call_chain?.tool_call_count || 0)), 1);
    const lane = (name, cellsHtml) =>
      `<div class="ses-lane"><span class="ses-lane-name">${esc(name)}</span><span class="ses-lane-dots">${cellsHtml}</span></div>`;
    const cells = (values) =>
      values
        .slice(-60)
        .map((value) => `<i class="ses-dot${value.bad ? " bad" : ""}" style="width:${value.width}px" title="${esc(value.title)}"></i>`)
        .join("");
    const inputDots = s.rows.map((row) => {
      const tokens = Number(usageOf(row).prompt_tokens || 0);
      return { width: 4 + Math.round((tokens / maxIn) * 9), title: `${fmtTime(row.ts)} · 入 ${compactNum(tokens)} tok` };
    });
    const modelDots = s.rows.map((row) => {
      const usage = usageOf(row);
      const out = Number(usage.completion_tokens || 0);
      const bad = Number(usage.status || 200) >= 400;
      return { width: 4 + Math.round((out / maxOut) * 9), bad, title: `${fmtTime(row.ts)} · 出 ${compactNum(out)} tok · ${usage.status || "—"}` };
    });
    const toolDots = s.rows.map((row) => {
      const calls = Number(row.call_chain?.tool_call_count || 0);
      return { width: calls ? 6 + Math.round((calls / maxTool) * 8) : 3, title: `${fmtTime(row.ts)} · 工具 ${calls} 次` };
    });
    host.innerHTML = [lane("输入", cells(inputDots)), lane("模型", cells(modelDots)), lane("工具", cells(toolDots))].join("");
  }

  function currentRound() {
    return s.rounds[s.roundIndex] || null;
  }

  function roundCost(round) {
    return round.rows.reduce((sum, row) => sum + Number(usageOf(row).cost || 0), 0);
  }

  function renderRounds() {
    const round = currentRound();
    const head = $("#ses-round");
    const meta = $("#ses-round-meta");
    const cost = $("#ses-round-cost");
    if (!round) {
      if (head) head.textContent = s.active ? "该会话没有提示词记录" : "选择会话";
      if (meta) meta.textContent = "";
      if (cost) cost.textContent = "";
      return;
    }
    if (head) head.textContent = `第 ${round.index} 轮 · ${round.rows.length} 请求`;
    if (meta) {
      const nav = s.rounds.length > 1
        ? `<button class="linklike" data-ses-round="prev" ${s.roundIndex === 0 ? "disabled" : ""}>‹ 上一轮</button> <button class="linklike" data-ses-round="next" ${s.roundIndex === s.rounds.length - 1 ? "disabled" : ""}>下一轮 ›</button>`
        : "";
      meta.innerHTML = `${esc(fmtTime(round.rows[0].ts))} 起 · ${nav}`;
    }
    if (cost) cost.textContent = `本轮 ${fmtCost(roundCost(round))} · 会话累计 ${fmtCost(s.rounds.reduce((sum, item) => sum + roundCost(item), 0))}`;
  }

  function nodeHtml(id, icon, title, sub, bad, tab) {
    return `<button class="ses-node${bad ? " bad" : ""}" data-ses-node="${esc(id)}" data-ses-dtab="${esc(tab || "overview")}">
      <span class="ses-ico ${esc(icon)}">${icon === "user" ? "U" : icon === "think" ? "✦" : "⚒"}</span>
      <span class="grow"><span class="n1">${esc(title)}</span>${sub ? `<span class="n2">${esc(sub)}</span>` : ""}</span>
    </button>`;
  }

  function renderTrace() {
    const host = $("#ses-nodes");
    if (!host) return;
    const round = currentRound();
    if (!round) {
      host.innerHTML = '<div class="empty-line">左侧选择一个会话</div>';
      return;
    }
    const parts = [];
    if (round.user) parts.push(nodeHtml(round.rows[0]?.id || "", "user", `user ·「${round.user}」`, "", false, "overview"));
    for (const row of round.rows) {
      const usage = usageOf(row);
      const chain = row.call_chain || {};
      const bad = Number(usage.status || 200) >= 400;
      const thinkText = chain.last_assistant_preview || row.preview || "(无文本输出)";
      const tools = Number(chain.tool_call_count || 0);
      parts.push(
        nodeHtml(
          row.id,
          "think",
          `${fmtTime(row.ts)} · ${thinkText}`,
          `入 ${compactNum(usage.prompt_tokens)} / 出 ${compactNum(usage.completion_tokens)} · ${fmtCost(usage.cost)} · ${fmtMs(usage.duration_ms)}${tools ? ` · 工具 ${tools}` : ""}${bad ? ` · ${usage.status}` : ""}`,
          bad,
          "overview"
        )
      );
      for (const pair of Array.isArray(chain.pairs) ? chain.pairs : []) {
        const ok = Boolean(pair.has_result);
        const result = pair.result_preview || (ok ? "" : "未匹配到工具结果");
        parts.push(nodeHtml(row.id, "tool", `${pair.name} ${(pair.arguments_preview || "").slice(0, 90)}`, `→ ${result.slice(0, 140)}`, !ok, "preview"));
      }
    }
    host.innerHTML = parts.join("") || '<div class="empty-line">该轮没有记录</div>';
  }

  // ───────────────────────── 节点详情 ─────────────────────────

  async function openNode(id, tab) {
    s.detailId = id;
    if (tab) setDetailTab(tab, true);
    try {
      s.detail = await api(`/api/prompts/${encodeURIComponent(id)}`);
    } catch (error) {
      toast(error.message, "error");
      return;
    }
    renderDetail();
  }

  function setDetailTab(tab, silent) {
    s.detailTab = tab;
    $$("#ses-detail-tabs [data-ses-dtab]").forEach((button) => button.classList.toggle("active", button.dataset.sesDtab === tab));
    if (!silent) renderDetail();
  }

  function detailUsage() {
    return s.detail ? s.usageById.get(String(s.detail.id)) || {} : {};
  }

  function renderDetail() {
    const body = $("#ses-detail-body");
    const title = $("#ses-detail-title");
    const sub = $("#ses-detail-sub");
    const foot = $("#ses-detail-meta");
    if (!body) return;
    if (!s.detail) {
      body.innerHTML = '<div class="empty-line">点击轨迹节点查看详情</div>';
      return;
    }
    const detail = s.detail;
    const usage = detailUsage();
    const badges = [];
    if (detail.truncated) badges.push('<span class="pill amber">已截断</span>');
    if (Number(detail.redactions || 0) > 0) badges.push(`<span class="pill amber">脱敏 ${detail.redactions} 处</span>`);
    if (title) title.textContent = "节点详情";
    if (sub) sub.innerHTML = `${esc(detail.requested_model || "—")} ${badges.join(" ")}`;

    if (s.detailTab === "overview") {
      const fields = [
        ["时间", fmtTime(detail.ts)],
        ["上游", detail.upstream],
        ["协议", detail.protocol],
        ["状态", usage.status || detail.status],
        ["输入 / 输出", `${compactNum(usage.prompt_tokens)} / ${compactNum(usage.completion_tokens)} tok`],
        ["缓存读取", usage.cache_read_tokens ? compactNum(usage.cache_read_tokens) : "—"],
        ["费用", fmtCost(usage.cost)],
        ["耗时 / 首 Token", `${fmtMs(usage.duration_ms)} / ${usage.ttft_ms ? fmtMs(usage.ttft_ms) : "—"}`],
        ["消息数 / 字符", `${detail.message_count} / ${num(detail.size_bytes)}`],
        ["会话", detail.session || "—"],
      ];
      const chain = detail.call_chain || {};
      body.innerHTML = `<div class="kv ses-kv">${fields
        .map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v == null || v === "" ? "—" : v)}</dd>`)
        .join("")}</div>
        ${chain.last_user_preview ? `<div class="ses-quote"><span class="bt">最新 user</span>${esc(chain.last_user_preview)}</div>` : ""}
        ${chain.last_assistant_preview ? `<div class="ses-quote a"><span class="bt">最新 assistant</span>${esc(chain.last_assistant_preview)}</div>` : ""}
        ${detail.neighbors && (detail.neighbors.prev || detail.neighbors.next)
          ? `<div class="card-actions wrap" style="margin-top:10px">
              ${detail.neighbors.prev ? `<button class="btn ghost sm" data-ses-neighbor="${esc(detail.neighbors.prev.id)}">‹ 上一请求</button>` : ""}
              ${detail.neighbors.next ? `<button class="btn ghost sm" data-ses-neighbor="${esc(detail.neighbors.next.id)}">下一请求 ›</button>` : ""}
            </div>`
          : ""}`;
    } else if (s.detailTab === "preview") {
      const chain = detail.call_chain || {};
      const previews = Array.isArray(chain.message_previews) ? chain.message_previews : [];
      body.innerHTML = previews.length
        ? previews
            .map(
              (item) =>
                `<div class="ses-quote ${esc(item.role)}"><span class="bt">#${item.index} ${esc(item.role)} · ${item.chars} 字符</span>${esc(item.preview || "")}</div>`
            )
            .join("") + (chain._gap ? '<div class="hint" style="padding:4px 0">…中间消息已省略…</div>' : "")
        : '<div class="empty-line">没有消息预览</div>';
    } else if (s.detailTab === "raw") {
      const messages = Array.isArray(detail.messages) ? detail.messages : [];
      body.innerHTML = `<pre class="code-block ses-raw">${esc(JSON.stringify(messages, null, 2))}</pre>`;
    } else {
      const chain = detail.call_chain || {};
      const think = [chain.last_assistant_preview, detail.preview].filter(Boolean).join("\n\n");
      body.innerHTML = think
        ? `<div class="ses-quote a"><span class="bt">assistant 输出 / 最近用户消息（脱敏视图）</span>${esc(think)}</div>`
        : '<div class="empty-line">该节点没有可展示的思考文本</div>';
    }
    if (foot) {
      foot.innerHTML = `该请求 ${compactNum(usage.prompt_tokens)} 入 / ${compactNum(usage.completion_tokens)} 出 · ${fmtCost(usage.cost)} · ${fmtMs(usage.duration_ms)} · <button class="linklike" data-ses-in-usage="1">在用量页查看 ›</button>`;
    }
  }

  // ───────────────────────── 演进对比 ─────────────────────────

  function fillDiffSelects() {
    const base = $("#ses-diff-base");
    const target = $("#ses-diff-target");
    if (!base || !target) return;
    const options = s.rounds
      .map((round) => `<option value="${esc(round.endId)}">第 ${round.index} 轮 · ${round.rows.length} 请求 · ${esc(fmtTime(round.rows[round.rows.length - 1].ts))}</option>`)
      .join("");
    base.innerHTML = options || '<option value="">（会话不足两轮）</option>';
    target.innerHTML = options || '<option value="">（会话不足两轮）</option>';
    if (s.rounds.length >= 2) {
      if (!s.diff.base || !s.rounds.some((round) => round.endId === s.diff.base)) s.diff.base = s.rounds[s.rounds.length - 2].endId;
      if (!s.diff.target || !s.rounds.some((round) => round.endId === s.diff.target)) s.diff.target = s.rounds[s.rounds.length - 1].endId;
    }
    base.value = s.diff.base;
    target.value = s.diff.target;
  }

  async function runDiff() {
    if (!s.diff.base || !s.diff.target) {
      toast("会话至少需要两轮才能对比", "error");
      return;
    }
    const params = new URLSearchParams({ base: s.diff.base, target: s.diff.target, context: "3" });
    if (s.diff.role) params.set("role", s.diff.role);
    const meta = $("#ses-diff-meta");
    if (meta) meta.textContent = "对比中…";
    try {
      s.diff.data = await api(`/api/prompts/diff?${params}`);
      renderDiff();
    } catch (error) {
      toast(error.message, "error");
      if (meta) meta.textContent = "对比失败";
    }
  }

  function renderDiff() {
    const host = $("#ses-diff");
    const stats = $("#ses-diff-stats");
    const meta = $("#ses-diff-meta");
    const data = s.diff.data;
    if (!host) return;
    if (!data || !data.diff) {
      host.innerHTML = '<div class="empty-line">选择基准与对比轮次后点「对比」</div>';
      return;
    }
    const stat = data.diff.stats || {};
    if (stats) {
      stats.innerHTML = [
        ["+ 新增", stat.added],
        ["− 删除", stat.removed],
        ["~ 修改", stat.modified],
        ["= 相同", stat.same],
        ["变化", `${stat.delta_percent ?? 0}%`],
      ]
        .map(([label, value]) => `<span class="pill">${esc(label)} ${num(value)}</span>`)
        .join("") + (stat.system_changed ? '<span class="pill amber">system 有变更</span>' : "");
    }
    if (meta) meta.textContent = `${data.base?.ts ? fmtTime(data.base.ts) : ""} → ${data.target?.ts ? fmtTime(data.target.ts) : ""}`;
    const rows = Array.isArray(data.diff.messages) ? data.diff.messages : [];
    const visible = rows.filter((row) => (s.diff.only ? row.status !== "same" : true));
    host.innerHTML = visible.length
      ? visible
          .map((row) => {
            const tone = row.status === "added" ? "add" : row.status === "removed" ? "del" : row.status === "modified" ? "mod" : "ctx";
            const mark = { added: "+", removed: "−", modified: "~", same: "=" }[row.status] || " ";
            const content = row.status === "modified" && Array.isArray(row.diff)
              ? row.diff.map((op) => `${op.op === "add" ? "+" : op.op === "del" ? "−" : " "} ${op.text || ""}`).join("\n")
              : row.content || "";
            return `<div class="ses-diff-row ${tone}"><span class="mark">${mark}</span><span class="role">${esc(row.role)}${row.target_index != null ? ` #${row.target_index}` : ""}</span><pre class="grow">${esc(String(content)).slice(0, 2000)}</pre></div>`;
          })
          .join("")
      : '<div class="empty-line">所选范围内没有差异</div>';
  }

  // ───────────────────────── 工具评估 ─────────────────────────

  function renderTools() {
    const tbody = $("#ses-tools-table tbody");
    const meta = $("#ses-tools-meta");
    if (!tbody) return;
    const tools = new Map();
    const seen = new Set();
    for (const row of s.rows) {
      const chain = row.call_chain || {};
      for (const pair of Array.isArray(chain.pairs) ? chain.pairs : []) {
        const tool = tools.get(pair.name) || { calls: 0, unmatched: 0, dup: 0, results: 0, chars: 0, max: 0 };
        tool.calls += 1;
        if (pair.has_result) {
          tool.results += 1;
          const text = pair.result_preview || "";
          tool.chars += text.length;
          tool.max = Math.max(tool.max, text.length);
        } else {
          tool.unmatched += 1;
        }
        const key = `${pair.name}::${pair.arguments_preview || ""}`;
        if (seen.has(key)) tool.dup += 1;
        seen.add(key);
        tools.set(pair.name, tool);
      }
    }
    const rows = [...tools.entries()].sort((a, b) => b[1].calls - a[1].calls);
    tbody.innerHTML = rows
      .map(
        ([name, tool]) => `<tr>
          <td class="mono">${esc(name)}</td>
          <td class="num">${tool.calls}</td>
          <td class="num${tool.unmatched ? " bad" : ""}">${tool.unmatched}</td>
          <td class="num">${tool.dup}</td>
          <td class="num">${tool.results}</td>
          <td class="num">${compactNum(tool.chars)}</td>
          <td class="num">${compactNum(tool.max)}</td>
        </tr>`
      )
      .join("") || '<tr><td colspan="7"><div class="empty-line">该会话没有工具调用</div></td></tr>';
    if (meta) meta.textContent = `基于 ${s.rows.length} 条提示词记录（结果字符合计为预览长度，供横向比较）`;
    const report = $("#ses-tools-report");
    if (report) {
      report.textContent = ["| 工具 | 调用 | 未匹配 | 重复 | 结果 | 字符合计 | 单次最大 |", "| --- | --- | --- | --- | --- | --- | --- |"]
        .concat(rows.map(([name, t]) => `| ${name} | ${t.calls} | ${t.unmatched} | ${t.dup} | ${t.results} | ${t.chars} | ${t.max} |`))
        .join("\n");
    }
  }

  // ───────────────────────── 页签 ─────────────────────────

  function setTab(tab) {
    s.tab = tab;
    $$("#ses-tabs [data-ses-tab]").forEach((button) => button.classList.toggle("active", button.dataset.sesTab === tab));
    const trace = $("#ses-pane-trace");
    const diff = $("#ses-pane-diff");
    const tools = $("#ses-pane-tools");
    if (trace) trace.hidden = tab !== "trace";
    if (diff) diff.hidden = tab !== "diff";
    if (tools) tools.hidden = tab !== "tools";
  }

  // ───────────────────────── 绑定 ─────────────────────────

  let bound = false;

  function bind() {
    if (bound) return;
    bound = true;
    $("#ses-range").addEventListener("change", () => {
      s.range = $("#ses-range").value;
      load().catch((error) => toast(error.message, "error"));
    });
    let searchTimer = null;
    $("#ses-q").addEventListener("input", () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(() => {
        s.q = $("#ses-q").value.trim();
        load().catch(() => {});
      }, 250);
    });
    $("#ses-refresh").addEventListener("click", () => load().catch((error) => toast(error.message, "error")));
    $("#ses-export").addEventListener("click", () => {
      const params = new URLSearchParams({ range: s.range });
      if (s.q) params.set("q", s.q);
      window.open(`/api/prompts/export?${params}`);
    });

    $("#ses-list").addEventListener("click", (event) => {
      const item = event.target.closest("[data-ses-key]");
      if (item) openSession(item.dataset.sesKey).catch((error) => toast(error.message, "error"));
    });
    $("#ses-tabs").addEventListener("click", (event) => {
      const button = event.target.closest("[data-ses-tab]");
      if (button) setTab(button.dataset.sesTab);
    });
    $("#ses-detail-tabs").addEventListener("click", (event) => {
      const button = event.target.closest("[data-ses-dtab]");
      if (button) setDetailTab(button.dataset.sesDtab);
    });
    $("#ses-nodes").addEventListener("click", (event) => {
      const node = event.target.closest("[data-ses-node]");
      if (node) openNode(node.dataset.sesNode, node.dataset.sesDtab).catch((error) => toast(error.message, "error"));
    });
    $("#ses-detail-body").addEventListener("click", (event) => {
      const neighbor = event.target.closest("[data-ses-neighbor]");
      if (neighbor) openNode(neighbor.dataset.sesNeighbor).catch((error) => toast(error.message, "error"));
      if (event.target.closest("[data-ses-in-usage]")) MG.navigate("usage");
    });
    $("#ses-round-meta").addEventListener("click", (event) => {
      const button = event.target.closest("[data-ses-round]");
      if (!button || button.disabled) return;
      if (button.dataset.sesRound === "prev") s.roundIndex = Math.max(0, s.roundIndex - 1);
      else s.roundIndex = Math.min(s.rounds.length - 1, s.roundIndex + 1);
      renderRounds();
      renderTrace();
    });

    $("#ses-diff-base").addEventListener("change", () => {
      s.diff.base = $("#ses-diff-base").value;
    });
    $("#ses-diff-target").addEventListener("change", () => {
      s.diff.target = $("#ses-diff-target").value;
    });
    $("#ses-diff-role").addEventListener("change", () => {
      s.diff.role = $("#ses-diff-role").value;
      if (s.diff.data) renderDiff();
    });
    $("#ses-diff-only").addEventListener("change", () => {
      s.diff.only = $("#ses-diff-only").checked;
      if (s.diff.data) renderDiff();
    });
    $("#ses-diff-run").addEventListener("click", () => runDiff());
    $("#ses-tools-copy").addEventListener("click", async () => {
      await navigator.clipboard.writeText($("#ses-tools-report").textContent || "");
      toast("评估报告已复制", "ok");
    });

    // 记录维护按钮位于「设置 → 维护」。
    $("#prompt-clear").addEventListener("click", async () => {
      if (!confirm("清空全部提示词记录？不可恢复。")) return;
      try {
        await api("/api/prompts/clear", { method: "POST", body: "{}" });
        toast("提示词记录已清空", "ok");
        await load();
      } catch (error) {
        toast(error.message, "error");
      }
    });
    $("#prompt-vacuum").addEventListener("click", async () => {
      try {
        await api("/api/prompts/vacuum", { method: "POST", body: "{}" });
        toast("已回收数据库空间", "ok");
      } catch (error) {
        toast(error.message, "error");
      }
    });

    // 供请求抽屉 / 用量页跳转：定位指定会话。
    MG.focusPromptSession = (key) => {
      MG.pendingSession = key || "";
      MG.navigate("sessions");
    };
  }

  MG.registerView("sessions", async () => {
    if (!bound) bind();
    await load();
  });

  document.addEventListener("DOMContentLoaded", bind);
})();
