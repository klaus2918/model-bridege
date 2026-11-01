/* v4 免费来源与 ZCode 账号池。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, esc, num, compactNum, fmtTime, fmtMs, api, toast, openModal, closeModal } = MG;

  const free = { sources: [], states: [], current: "", models: [], results: {}, level: {}, loaded: false };
  const zc = { config: null, status: null, accounts: [], claims: [], oauthTimer: null };
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
            return `<tr>
              <td><div class="name-cell"><code>${esc(model.id)}</code>${model.mayTrainOnYourPrompts ? '<span class="pill danger">可能训练</span>' : ""}</div><div class="hint">${esc(model.name || "")}</div></td>
              <td><span class="pill ${meta.tone === "ok" ? "signal" : meta.tone === "bad" ? "danger" : meta.tone === "warn" ? "amber" : "info"}">${esc(meta.label)}</span></td>
              <td class="num">${model.contextLength ? compactNum(model.contextLength) : "—"}</td>
              <td>${(model.reasoningEfforts || []).join(" / ") || "—"}</td>
              <td class="num">${result.latencyMs ? fmtMs(result.latencyMs) : "—"}</td>
              <td>${esc(result.reason || result.suggestion || result.message || "—")}</td>
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

  async function loadZcode() {
    const [status, config, accounts] = await Promise.all([
      api("/api/zcode/status"),
      api("/api/zcode/config"),
      api("/api/zcode/accounts"),
    ]);
    zc.status = status || {};
    zc.config = config.config || {};
    zc.accounts = Array.isArray(accounts.accounts) ? accounts.accounts : [];
    zc.claims = Array.isArray(accounts.claims) ? accounts.claims : [];
    renderZcode();
  }

  function renderZcode() {
    const status = zc.status || {};
    const pool = status.pool || {};
    $("#zcode-status").innerHTML = `
      <div class="stat ${status.enabled ? "signal" : ""}"><span class="k">插件</span><span class="v">${status.enabled ? "启用" : "停用"}</span><span class="s">backend ${esc(status.backend || "—")}</span></div>
      <div class="stat"><span class="k">账号</span><span class="v">${num(pool.total)}</span><span class="s">${num(pool.active)} 活跃 · ${num(pool.cooling)} 冷却</span></div>
      <div class="stat amber"><span class="k">异常</span><span class="v">${num(pool.invalid)}</span><span class="s">${num(pool.exhausted)} 配额耗尽</span></div>
      <div class="stat"><span class="k">验证码</span><span class="v">${num(status.captcha?.available)}</span><span class="s">失效 ${num(status.captcha?.invalidations)}</span></div>`;
    const cfg = zc.config || {};
    $("#zcode-enabled").checked = Boolean(cfg.enabled);
    $("#zcode-cli-path").value = cfg.cliPath || "";
    $("#zcode-auto-claim").checked = Boolean(cfg.claimDailyTime);
    $("#zcode-claim-time").value = cfg.claimDailyTime || "";
    $("#zcode-concurrency").value = cfg.accountConcurrency || 2;
    $("#zcode-backend").value = cfg.backend || "appserver";
    $("#zcode-node-path").value = cfg.nodePath || "";
    $("#zcode-data-dir").value = cfg.dataDir || "";
    $("#zcode-browser-path").value = cfg.browserPath || "";
    $("#zcode-captcha-scene").value = cfg.captchaScene || "";
    $("#zcode-pool-hint").textContent = `${status.accounts?.total ?? zc.accounts.length} 个账号 · CLI ${status.cliExists ? "存在" : "未找到"} · 浏览器 ${status.browserExists ? "存在" : "未找到"}`;
    $("#zcode-account-table tbody").innerHTML = zc.accounts.length
      ? zc.accounts
          .map(
            (account) => `<tr>
              <td><div class="name">${esc(account.name)}</div><div class="meta">${esc(account.secretMasked || "")}</div></td>
              <td>${esc(account.mode || "—")}</td>
              <td><span class="pill ${account.enabled ? "signal" : "amber"}">${esc(account.status || (account.enabled ? "active" : "disabled"))}</span></td>
              <td>${esc(account.plan?.name || account.plan || "—")}</td>
              <td class="num">${num(account.useCount)} / ${num(account.failCount)}</td>
              <td>${esc(account.lastError || "—")}</td>
              <td class="right"><div class="actions">
                <button class="btn ghost sm" data-zc-action="refresh" data-id="${esc(account.id)}">刷新</button>
                <button class="btn ghost sm" data-zc-action="toggle" data-id="${esc(account.id)}">${account.enabled ? "停用" : "启用"}</button>
                <button class="btn danger sm" data-zc-action="delete" data-id="${esc(account.id)}">删除</button>
              </div></td>
            </tr>`
          )
          .join("")
      : `<tr><td colspan="7"><div class="empty-line">还没有账号。</div></td></tr>`;
    $("#zcode-claim-table tbody").innerHTML = zc.claims.length
      ? zc.claims
          .map(
            (claim) => `<tr><td>${fmtTime(claim.checkedAt || claim.ts)}</td><td>${esc(claim.accountName || claim.accountId || "—")}</td><td>${esc(claim.plan || "—")}</td><td>${esc(claim.ok ? "成功" : claim.result || claim.message || "—")}</td><td>${fmtTime(claim.nextCheckAt)}</td></tr>`
          )
          .join("")
      : `<tr><td colspan="5"><div class="empty-line">没有领取记录。</div></td></tr>`;
  }

  async function saveZcodeConfig() {
    const next = {
      ...(zc.config || {}),
      enabled: $("#zcode-enabled").checked,
      backend: $("#zcode-backend").value,
      cliPath: $("#zcode-cli-path").value.trim(),
      nodePath: $("#zcode-node-path").value.trim(),
      dataDir: $("#zcode-data-dir").value.trim(),
      browserPath: $("#zcode-browser-path").value.trim(),
      captchaScene: $("#zcode-captcha-scene").value.trim(),
      claimDailyTime: $("#zcode-auto-claim").checked ? $("#zcode-claim-time").value : "",
      accountConcurrency: Number($("#zcode-concurrency").value) || 2,
    };
    try {
      const data = await api("/api/zcode/config", { method: "PUT", body: JSON.stringify(next) });
      zc.config = data.config || next;
      toast("ZCode 设置已保存", "ok");
      await loadZcode();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function importLocal() {
    try {
      const info = await api("/api/zcode/accounts/import/local", { method: "POST", body: "{}" });
      if (!info.found) return toast(`未在 ${info.dataDir} 找到本机凭据`, "error");
      if (!confirm(`找到 ${info.keys?.length || 1} 组本机凭据（provider ${info.activeProvider || "—"}），导入？`)) return;
      const data = await api("/api/zcode/accounts/import", { method: "POST", body: "{}" });
      toast(data.duplicate ? "账号已在池中" : "账号已导入", "ok");
      await loadZcode();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function manualAdd() {
    openModal(
      "批量添加 ZCode 账号",
      "每行一个；可写「名称=JWT」。密钥只保存密文。",
      `<textarea class="input code-area" id="zcode-tokens" rows="8" spellcheck="false" placeholder="账号A=eyJhbGciOi..."></textarea>
       <div class="modal-actions"><button class="btn ghost" id="zc-cancel">取消</button><button class="btn primary" id="zc-add">添加</button></div>`
    );
    $("#zc-cancel").addEventListener("click", closeModal);
    $("#zc-add").addEventListener("click", async () => {
      const lines = $("#zcode-tokens").value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
      let added = 0;
      let duplicate = 0;
      let failed = 0;
      for (const [index, line] of lines.entries()) {
        const [name, token] = line.includes("=") ? line.split("=", 2) : [`zcode-${index + 1}`, line];
        try {
          const result = await api("/api/zcode/accounts", { method: "POST", body: JSON.stringify({ name: name.trim(), token: token.trim() }) });
          if (result.duplicate) duplicate += 1;
          else added += 1;
        } catch {
          failed += 1;
        }
      }
      closeModal();
      toast(`新增 ${added}，重复 ${duplicate}，失败 ${failed}`, added ? "ok" : "");
      await loadZcode();
    });
  }

  async function oauthStart() {
    try {
      const start = await api("/api/zcode/oauth/start", { method: "POST", body: "{}" });
      if (start.authorizeUrl) window.open(start.authorizeUrl, "_blank", "noopener");
      $("#zcode-action-hint").textContent = `等待 OAuth 授权（${String(start.flowId || "").slice(0, 8)}…）`;
      clearInterval(zc.oauthTimer);
      let polls = 0;
      zc.oauthTimer = setInterval(async () => {
        polls += 1;
        if (polls > 80) {
          clearInterval(zc.oauthTimer);
          $("#zcode-action-hint").textContent = "OAuth 等待超时，请重试。";
          return;
        }
        try {
          const data = await api(`/api/zcode/oauth/poll/${encodeURIComponent(start.flowId)}`);
          if (data.status === "complete" || data.ok) {
            clearInterval(zc.oauthTimer);
            toast("OAuth 登录完成", "ok");
            $("#zcode-action-hint").textContent = "OAuth 登录完成";
            await loadZcode();
          }
        } catch {
          // 等待中不刷错误。
        }
      }, 2000);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function claimPreview() {
    try {
      const data = await api("/api/zcode/claim/preview");
      const rows = (data.accounts || [])
        .map((account) => `<tr><td>${esc(account.accountName)}</td><td>${esc(account.status)}</td><td>${esc((account.plans || []).map((plan) => plan.name || plan.planId).join("、") || "无")}</td><td>${esc(account.error || "—")}</td></tr>`)
        .join("");
      openModal(
        "可领取账号",
        "预览不会执行领取。",
        `<div class="table-wrap"><table class="table"><thead><tr><th>账号</th><th>状态</th><th>套餐</th><th>错误</th></tr></thead><tbody>${rows || '<tr><td colspan="4">没有可领取账号</td></tr>'}</tbody></table></div>
         <div class="modal-actions"><button class="btn primary" id="zc-close">关闭</button></div>`
      );
      $("#zc-close").addEventListener("click", closeModal);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function claimNow() {
    if (!confirm("立即领取 Start Plan？会启动本机浏览器并消耗领取机会。")) return;
    try {
      const data = await api("/api/zcode/claim", { method: "POST", body: "{}" });
      const rows = (data.reports || [])
        .map((report) => `<tr><td>${esc(report.accountName || "—")}</td><td>${esc(report.plan || "—")}</td><td>${esc(report.ok ? "成功" : report.message || "失败")}</td><td>${esc(report.error || "—")}</td></tr>`)
        .join("");
      openModal(
        `领取结果（成功 ${data.claimed ?? 0}）`,
        "领取记录已写入账号页。",
        `<div class="table-wrap"><table class="table"><thead><tr><th>账号</th><th>套餐</th><th>结果</th><th>说明</th></tr></thead><tbody>${rows || '<tr><td colspan="4">没有结果</td></tr>'}</tbody></table></div>
         <div class="modal-actions"><button class="btn primary" id="zc-close">关闭</button></div>`
      );
      $("#zc-close").addEventListener("click", closeModal);
      await loadZcode();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function accountAction(action, id) {
    const account = zc.accounts.find((item) => item.id === id);
    try {
      if (action === "refresh") {
        await api(`/api/zcode/accounts/${encodeURIComponent(id)}/refresh`, { method: "POST", body: "{}" });
        toast("额度已刷新", "ok");
      }
      if (action === "toggle") {
        await api(`/api/zcode/accounts/${encodeURIComponent(id)}`, {
          method: "PUT",
          body: JSON.stringify({ enabled: !account?.enabled }),
        });
        toast(account?.enabled ? "账号已停用" : "账号已启用", "ok");
      }
      if (action === "delete") {
        if (!confirm(`删除账号「${account?.name || id}」？领取记录会保留。`)) return;
        await api("/api/zcode/accounts", { method: "DELETE", body: JSON.stringify({ ids: [id] }) });
        toast("账号已删除", "ok");
      }
      await loadZcode();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function showZcodeHelp() {
    openModal(
      "ZCode 配置说明",
      "ZCode 是可选账号型上游；不启用只影响聊天，不启动 CLI。",
      `<div class="prose">
        <p><b>启用</b>：只影响 <code>/zcode/v1</code> 是否调用本机 ZCode CLI。</p>
        <p><b>每日领取</b>：只依赖账号 JWT + 无头浏览器，不需要安装 ZCode；留空时间即关闭。</p>
        <p><b>换号上限</b>：单次对话在池中最多尝试的账号数，不是并发数。</p>
        <p><b>验证码</b>：场景 ID、区域与前缀属于高级项；自检会启动浏览器求解一次。</p>
        <p><b>安全</b>：页面只显示掩码，账号密文保存在本机数据库。</p>
      </div>
      <div class="modal-actions"><button class="btn primary" id="zc-close">知道了</button></div>`
    );
    $("#zc-close").addEventListener("click", closeModal);
  }

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

    $("#zcode-refresh").addEventListener("click", () => loadZcode().catch((error) => toast(error.message, "error")));
    $("#zcode-help").addEventListener("click", showZcodeHelp);
    $("#zcode-save").addEventListener("click", saveZcodeConfig);
    $("#zcode-import-local").addEventListener("click", importLocal);
    $("#zcode-add-manual").addEventListener("click", manualAdd);
    $("#zcode-oauth-start").addEventListener("click", oauthStart);
    $("#zcode-claim-preview").addEventListener("click", claimPreview);
    $("#zcode-claim-now").addEventListener("click", claimNow);
    $("#zcode-captcha-test").addEventListener("click", async () => {
      try {
        const data = await api("/api/zcode/captcha/test", { method: "POST", body: "{}" });
        $("#zcode-captcha-hint").textContent = `成功：${data.solvedMs} ms · 参数 ${data.paramLength} · 区域 ${data.region}`;
      } catch (error) {
        $("#zcode-captcha-hint").textContent = `失败：${error.message}`;
      }
    });
    $("#zcode-attach").addEventListener("click", async () => {
      try {
        const data = await api("/api/zcode/attach", { method: "POST", body: JSON.stringify({ models: ["GLM-5.3-Flash", "GLM-5.3"] }) });
        toast(`已接入 ${data.upstream || "zcode"}（${data.modelCount ?? 0} 个模型）`, "ok");
        await loadZcode();
        await MG.refreshAccess();
      } catch (error) {
        toast(error.message, "error");
      }
    });
    $("#zcode-account-table").addEventListener("click", (event) => {
      const button = event.target.closest("[data-zc-action]");
      if (button) accountAction(button.dataset.zcAction, button.dataset.id);
    });
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
  MG.registerView("zcode", loadZcode);
  document.addEventListener("DOMContentLoaded", () => {
    bind();
    bindTemplates();
  });
})();
