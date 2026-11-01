/* v4 设置、统一接入、路由组与高级维护。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, esc, num, api, toast, state, openModal, closeModal } = MG;

  let unifiedGroups = [];
  let routeGroups = [];
  let bound = false;
  let snapshot = "";
  let overwrite = false;

  const asArray = (value) => (Array.isArray(value) ? value : []);
  const lines = (value) => (Array.isArray(value) ? value.join("\n") : String(value || ""));

  function parseLines(value) {
    return String(value || "")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
  }

  function parseAliases(value) {
    const aliases = {};
    String(value || "")
      .split(/\r?\n/)
      .forEach((line) => {
        const index = line.indexOf("=");
        if (index <= 0) return;
        const key = line.slice(0, index).trim();
        const target = line.slice(index + 1).trim();
        if (key && target) aliases[key] = target;
      });
    return aliases;
  }

  function aliasesText(value) {
    if (!value || typeof value !== "object") return "";
    return Object.entries(value)
      .map(([key, target]) => `${key} = ${target}`)
      .join("\n");
  }

  function config() {
    return state.config && typeof state.config === "object" ? state.config : {};
  }

  // /api/config 会附带 `_keyConfigured`、`_keySource` 等运行时派生字段。
  // 这些字段不是配置 schema 的一部分，校验和保存前必须剔除，避免被当成未知项或写回磁盘。
  function cleanConfig(value) {
    if (Array.isArray(value)) return value.map(cleanConfig);
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value)
          .filter(([key]) => !key.startsWith("_") && value[key] !== undefined)
          .map(([key, item]) => [key, cleanConfig(item)])
      );
    }
    return value;
  }

  function fingerprint(value) {
    try {
      return JSON.stringify(cleanConfig(value || {}));
    } catch {
      return "";
    }
  }

  function upstreamNames() {
    const names = new Set();
    asArray(state.access).forEach((item) => item.id && names.add(item.id));
    const upstreams = config().upstreams;
    if (upstreams && typeof upstreams === "object") {
      Object.keys(upstreams).forEach((name) => names.add(name));
    }
    return Array.from(names).sort();
  }

  function setValue(id, value) {
    const element = $(id);
    if (element) element.value = value == null ? "" : value;
  }

  function setChecked(id, value) {
    const element = $(id);
    if (element) element.checked = Boolean(value);
  }

  function renderGroups() {
    const names = upstreamNames();
    const unifiedHost = $("#unified-group-list");
    if (unifiedHost) {
      unifiedHost.innerHTML = unifiedGroups.length
        ? unifiedGroups
            .map(
              (group, index) => `<div class="row" data-unified-group="${index}">
                <input class="input" data-ug-id value="${esc(group.id || "")}" placeholder="分组标识">
                <input class="input" data-ug-label value="${esc(group.label || "")}" placeholder="显示名称">
                <input class="input grow" data-ug-upstreams value="${esc(asArray(group.upstreams).join(", "))}" placeholder="上游，逗号分隔">
                <button class="icon-btn" data-remove-unified="${index}" title="删除">×</button>
              </div>`
            )
            .join("")
        : `<div class="empty-line">还没有统一入口分组。</div>`;
    }

    const routeHost = $("#route-group-settings");
    if (routeHost) {
      routeHost.innerHTML = routeGroups.length
        ? routeGroups
            .map(
              (group, index) => `<div class="row" data-route-group="${index}">
                <input class="input" data-rg-id value="${esc(group.id || "")}" placeholder="路由组标识">
                <input class="input grow" data-rg-members value="${esc(asArray(group.members).join(", "))}" placeholder="成员，逗号分隔：上游/模型">
                <button class="icon-btn" data-remove-route="${index}" title="删除">×</button>
              </div>`
            )
            .join("")
        : `<div class="empty-line">还没有路由组。</div>`;
    }

    const visibleHost = $("#unified-visible-list");
    if (visibleHost) {
      const selected = new Set(asArray(config().unified && config().unified.visibleUpstreams));
      visibleHost.innerHTML = names.length
        ? names
            .map(
              (name) => `<label class="check"><input type="checkbox" data-visible-upstream="${esc(name)}" ${selected.has(name) ? "checked" : ""}> ${esc(name)}</label>`
            )
            .join("")
        : `<div class="empty-line">暂无上游。</div>`;
    }
  }

  function readGroups() {
    unifiedGroups = MG.$$("[data-unified-group]").map((row) => ({
      id: row.querySelector("[data-ug-id]").value.trim(),
      label: row.querySelector("[data-ug-label]").value.trim(),
      upstreams: row
        .querySelector("[data-ug-upstreams]")
        .value.split(/[,，\s]+/)
        .map((item) => item.trim())
        .filter(Boolean),
    }));
    routeGroups = MG.$$("[data-route-group]").map((row) => ({
      id: row.querySelector("[data-rg-id]").value.trim(),
      members: row
        .querySelector("[data-rg-members]")
        .value.split(/[,，\s]+/)
        .map((item) => item.trim())
        .filter(Boolean),
    }));
  }

  function render() {
    const cfg = config();
    snapshot = fingerprint(cfg);
    overwrite = false;
    const revision = $("#settings-revision");
    if (revision) revision.hidden = true;
    setValue("#set-host", cfg.host || "127.0.0.1");
    setValue("#set-port", cfg.port || 8900);
    setValue("#set-default-model", cfg.defaultModel || "");
    setValue("#set-max-tokens", cfg.maxTokens || 8192);
    setValue("#set-timeout", cfg.timeoutSeconds || 600);
    setValue("#set-models-cache", cfg.modelsCacheSeconds || 300);
    setValue("#set-retention", cfg.retentionDays || 0);
    setValue("#set-models", lines(cfg.models));
    setValue("#set-aliases", aliasesText(cfg.aliases));
    setChecked("#set-access-log", cfg.accessLog !== false);
    setChecked("#set-thinking", cfg.thinkingPassthrough !== false);
    setChecked("#set-free-panel", cfg.freeSourcesPanel !== false);
    setChecked("#set-zcode-panel", cfg.zcodePanel === true);
    setChecked("#set-prompt-capture", Boolean(cfg.promptCapture));
    setValue("#set-prompt-retention", cfg.promptRetentionDays == null ? 7 : cfg.promptRetentionDays);
    setValue("#set-prompt-max-records", cfg.promptMaxRecords || 0);
    setValue("#set-prompt-max-chars", cfg.promptMaxChars || 1000000);
    setChecked("#set-ai-enabled", state.aiSettings.enabled !== false);
    setChecked("#set-ai-apply", Boolean(state.aiSettings.allowApply));
    setChecked("#set-ai-pool", state.aiSettings.allowPoolStats !== false);

    unifiedGroups = asArray(cfg.unified && cfg.unified.groups).map((group) => ({
      id: group.id || "",
      label: group.label || "",
      upstreams: asArray(group.upstreams),
    }));
    routeGroups = Object.entries(cfg.groups && typeof cfg.groups === "object" ? cfg.groups : {}).map(([id, members]) => ({
      id,
      members: asArray(members),
    }));
    renderGroups();

    const count = asArray(state.templates).length;
    const templateCount = $("#template-count");
    if (templateCount) templateCount.textContent = String(count);
    const templateHost = $("#template-list");
    if (templateHost) {
      templateHost.innerHTML = count
        ? state.templates
            .slice(0, 12)
            .map(
              (template) => `<div class="row">
                <div class="grow"><div class="name">${esc(template.label || template.id)}</div><div class="meta">${esc(template.id)} · ${esc(template.kind || "")}</div></div>
                <span class="pill ${template.kind === "official" ? "signal" : ""}">${esc(template.kind || "template")}</span>
              </div>`
            )
            .join("")
        : `<div class="empty-line">模板接口不可用。</div>`;
    }

    const service = $("#settings-service-hint");
    if (service) service.textContent = `${state.info?.version ? `v${state.info.version}` : "—"} · ${cfg.host || "127.0.0.1"}:${cfg.port || 8900}`;
    const storage = $("#settings-storage-hint");
    if (storage) {
      const configPath = state.info?.configPath || state.info?.config_path || "bridge.config.json";
      const dbPath = state.info?.db_path || "data/modelgate.db";
      storage.textContent = `配置：${configPath} · 数据库：${dbPath}`;
    }
    const dirty = $("#settings-dirty");
    if (dirty) dirty.hidden = true;
  }

  function renderIssues(rows) {
    const host = $("#settings-issues");
    if (!host) return;
    const list = Array.isArray(rows) ? rows : [];
    host.innerHTML = list.length
      ? list
          .map(
            (issue) => `<div class="row">
              <span class="pill ${issue.level === "error" ? "danger" : issue.level === "warn" ? "amber" : ""}">${esc(issue.level || "info")}</span>
              <div class="grow"><div class="name">${esc(issue.path || issue.code || "")}</div><div class="meta">${esc(issue.message || "")}</div>${issue.hint ? `<div class="tiny muted">${esc(issue.hint)}</div>` : ""}</div>
            </div>`
          )
          .join("")
      : `<div class="empty-line">校验通过，没有发现问题。</div>`;
  }

  async function validateCurrent(candidate, quiet = false) {
    const data = await api("/api/config/validate", {
      method: "POST",
      body: JSON.stringify({ config: cleanConfig(candidate) }),
    });
    renderIssues(data.diagnostics);
    if (data.errors) {
      if (!quiet) {
        openModal(
          "配置校验未通过",
          `发现 ${data.errors} 个错误、${data.warnings || 0} 个提示；修正后再保存。`,
          `<div class="list compact">${(data.diagnostics || [])
            .filter((issue) => issue.level === "error")
            .map(
              (issue) => `<div class="row"><span class="pill danger">error</span><div class="grow"><div class="name">${esc(issue.path || issue.code || "")}</div><div class="meta">${esc(issue.message || "")}</div></div></div>`
            )
            .join("")}</div><div class="modal-actions"><button class="btn primary" id="validate-close">返回修改</button></div>`
        );
        $("#validate-close").addEventListener("click", closeModal);
      }
      return false;
    }
    if (!quiet) toast(`校验通过：${data.warnings || 0} 个提示`, "ok");
    return true;
  }

  async function save() {
    const cfg = cleanConfig(config());
    readGroups();
    const port = Number($("#set-port").value);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      toast("端口必须是 1–65535 的整数", "error");
      return;
    }
    const next = cleanConfig({
      ...cfg,
      host: $("#set-host").value.trim() || "127.0.0.1",
      port,
      defaultModel: $("#set-default-model").value.trim(),
      maxTokens: $("#set-max-tokens").value.trim() || 8192,
      timeoutSeconds: Number($("#set-timeout").value) || 600,
      modelsCacheSeconds: Number($("#set-models-cache").value) || 0,
      retentionDays: Number($("#set-retention").value) || 0,
      models: parseLines($("#set-models").value),
      aliases: parseAliases($("#set-aliases").value),
      accessLog: $("#set-access-log").checked,
      thinkingPassthrough: $("#set-thinking").checked,
      freeSourcesPanel: $("#set-free-panel").checked,
      zcodePanel: $("#set-zcode-panel").checked,
      promptCapture: $("#set-prompt-capture").checked,
      promptRetentionDays: Number($("#set-prompt-retention").value) || 0,
      promptMaxRecords: Number($("#set-prompt-max-records").value) || 0,
      promptMaxChars: Math.max(1000, Number($("#set-prompt-max-chars").value) || 1000),
      unified: {
        ...(cfg.unified && typeof cfg.unified === "object" ? cfg.unified : {}),
        visibleUpstreams: MG.$$("[data-visible-upstream]")
          .filter((input) => input.checked)
          .map((input) => input.dataset.visibleUpstream),
        groups: unifiedGroups.filter((group) => group.id),
      },
      groups: Object.fromEntries(routeGroups.filter((group) => group.id).map((group) => [group.id, group.members])),
    });
    try {
      if (!(await validateCurrent(next, false))) {
        return;
      }
      if (!overwrite) {
        const live = await api("/api/config");
        const liveCfg = live.config && typeof live.config === "object" ? live.config : live;
        if (snapshot && fingerprint(liveCfg) !== snapshot) {
          const notice = $("#settings-revision");
          if (notice) {
            notice.hidden = false;
            notice.scrollIntoView({ block: "nearest", behavior: "smooth" });
          }
          toast("配置已在其他窗口或磁盘上被修改，请先重载或确认覆盖", "error");
          return;
        }
      }
      const backup = $("#settings-backup") ? $("#settings-backup").checked : true;
      await api("/api/config", { method: "POST", body: JSON.stringify({ config: next, backup }) });
      state.config = next;
      overwrite = false;
      snapshot = fingerprint(next);
      const revision = $("#settings-revision");
      if (revision) revision.hidden = true;
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
      await MG.refreshAISettings();
      await MG.refreshConfig();
      render();
      toast("设置已保存并热重载", "ok");
      try {
        await MG.refreshInfo();
      } catch {
        // 端口变化会断开旧连接，属预期。
      }
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function mask(value, depth = 0) {
    if (depth > 12) return "[depth]";
    if (Array.isArray(value)) return value.map((item) => mask(item, depth + 1));
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value).map(([key, item]) => {
          if (/key|secret|token|authorization|password|cookie/i.test(key)) return [key, item ? "[redacted]" : ""];
          return [key, mask(item, depth + 1)];
        })
      );
    }
    return value;
  }

  function showRawConfig() {
    const cfg = mask(config());
    openModal(
      "配置 JSON（已脱敏）",
      "只看不改；密钥、token、secret 已替换为 [redacted]。修改请用表单或 /api/ai/plan。",
      `<pre class="code-block">${esc(JSON.stringify(cfg, null, 2))}</pre>
       <div class="modal-actions"><button class="btn ghost" id="raw-copy">复制</button><button class="btn primary" id="raw-close">关闭</button></div>`
    );
    $("#raw-close").addEventListener("click", closeModal);
    $("#raw-copy").addEventListener("click", async () => {
      await navigator.clipboard.writeText(JSON.stringify(cfg, null, 2));
      toast("已复制脱敏 JSON", "ok");
    });
  }

  async function restoreBackup() {
    if (!confirm("用上一次备份覆盖当前配置？恢复前会自动另存现场。")) return;
    try {
      const data = await api("/api/config/backups/restore", { method: "POST", body: "{}" });
      await MG.refreshConfig();
      render();
      toast(`已恢复：${data.restored_from || "备份"}`, "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function reloadConfig() {
    if (!confirm("从磁盘重新载入 bridge.config.json？未保存的面板改动会丢失。")) return;
    try {
      await api("/api/config/reload", { method: "POST", body: "{}" });
      await MG.refreshConfig();
      render();
      toast("已从文件重载", "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function exportDiagnostics() {
    try {
      const [info, configData, access, pools] = await Promise.all([
        api("/api/info"),
        api("/api/config"),
        api("/api/access"),
        api("/api/pools").catch(() => ({ items: [] })),
      ]);
      const payload = mask({
        exportedAt: new Date().toISOString(),
        info,
        config: configData.config || configData,
        access: access.items || [],
        pools: pools.items || [],
      });
      const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
      const link = document.createElement("a");
      link.href = URL.createObjectURL(blob);
      link.download = `model-gate-diagnostics-${new Date().toISOString().slice(0, 10)}.json`;
      link.click();
      URL.revokeObjectURL(link.href);
      toast("诊断已导出（脱敏）", "ok");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  const HELP_TOP_FIELDS = [
    ["host", "string", "127.0.0.1", "监听地址；只在本机用不要改，局域网访问填 0.0.0.0"],
    ["port", "number", "8900", "监听端口；被占用会自动顺延，实际端口看控制台顶部"],
    ["defaultModel", "string", "—", "默认模型；客户端未指定模型时使用"],
    ["models", "string[]", "上游合并", "对外模型白名单；填了就只暴露这些模型"],
    ["maxTokens", "number", "8192", "单次请求默认输出上限"],
    ["timeoutSeconds", "number", "600", "转发上游的超时秒数"],
    ["maxBodyBytes", "number", "67108864", "chat 单次请求体上限（字节，默认 64 MiB）；超限返回 413 request_too_large"],
    ["modelsCacheSeconds", "number", "300", "模型目录缓存秒数"],
    ["accessLog", "boolean", "true", "是否写访问日志"],
    ["retentionDays", "number", "30", "用量记录保留天数，0 = 永久"],
    ["promptCapture", "boolean", "false", "是否记录入站提示词，默认关闭"],
    ["aliases", "object", "{}", "模型别名：对外名 → 真实模型名"],
    ["upstreams", "object", "—", "接入清单：键为上游标识，值见下表"],
  ];

  const HELP_UPSTREAM_FIELDS = [
    ["protocol", "string", "openai", "wire 协议：openai / anthropic / commandcode / opencode / zcode"],
    ["baseUrl", "string", "—", "上游请求地址，一般带 /v1 后缀"],
    ["apiKeyStore", "string", "—", "本机加密库引用；密钥由 Windows DPAPI 保存，配置只留引用"],
    ["apiKeyEnv", "string", "—", "旧字段：密钥环境变量名；v4 默认使用加密库"],
    ["default", "boolean", "false", "是否兜底接入；都未标记时取第一条"],
    ["models", "string[]", "—", "该接入提供的模型清单"],
    ["modelMap", "object", "{}", "模型映射：对外名 → 上游真实名"],
    ["modelOverrides", "object", "{}", "模型级覆盖，键支持单个 * 通配"],
    ["fetchModels", "boolean", "true", "是否向上游拉取模型目录"],
    ["autoModels", "boolean", "false", "是否把拉取结果并入模型清单"],
    ["compat", "object", "模板预设", "兼容参数：profile、headers、隐私适配器等"],
    ["credentialPool", "string", "—", "绑定的凭据池 id；设置后按池策略选 Key"],
  ];

  function helpTable(rows) {
    return `<div class="table-wrap"><table class="table">
      <thead><tr><th>字段</th><th>类型</th><th>默认</th><th>用途</th></tr></thead>
      <tbody>${rows
        .map((row) => `<tr><td><code>${esc(row[0])}</code></td><td>${esc(row[1])}</td><td>${esc(row[2])}</td><td>${esc(row[3])}</td></tr>`)
        .join("")}</tbody>
    </table></div>`;
  }

  function help() {
    openModal(
      "配置说明",
      "v4 只暴露安全字段；密钥写入独立密钥库，永不回显。",
      `<div class="prose">
        <h3>最短配置</h3>
        <p>官方厂商：选模板 → 填 Key。自定义厂商：标识、厂商、请求地址、协议、Key 五项。其余字段均有模板默认值，可在接入详情中修改。</p>
        <h3>配置文件</h3>
        <p>唯一事实源是 <code>bridge.config.json</code>；设置页「高级维护」显示真实路径。用量与提示词记录在 <code>data/modelgate.db</code>，与配置独立。</p>
        <h3>常用字段（顶层）</h3>
        ${helpTable(HELP_TOP_FIELDS)}
        <h3>常用字段（upstreams 中的一条）</h3>
        ${helpTable(HELP_UPSTREAM_FIELDS)}
        <h3>密钥与凭据池</h3>
        <p>配置文件只写引用，不写明文。密钥在「接入详情 → 凭据」写入本机加密库；多 Key 接入在「凭据池」创建成员并按 priority / round_robin / weighted / least_used 选择，成员失败会自动切换。</p>
        <h3>隐私</h3>
        <p>ZDR 默认勾选，但按厂商能力显示“请求级 / 账户级 / 不支持”；不支持时只记录意图，不会假装已保护。训练策略可选跟随模板、禁止或允许。</p>
        <h3>AI 配置接口</h3>
        <p><code>GET /api/ai/manifest</code> 与 <code>GET /llms.txt</code> 提供脱敏说明；<code>POST /api/ai/plan</code> 只生成草案，应用需要一次性确认令牌，密钥和账号永不下发。</p>
        <h3>兼容</h3>
        <p>数据库只追加表；旧接口 <code>/v1</code>、<code>/unified/v1</code>、<code>/{上游}/v1</code> 保持。旧配置字段无需迁移。</p>
        <h3>常见问题</h3>
        <ul>
          <li>改了没生效：设置页点「从文件重载」；仍不行看「运行日志」。</li>
          <li>端口被占用：服务会自动顺延，实际地址以顶部为准。</li>
          <li>密钥读不到：DPAPI 密文绑定当前 Windows 用户与机器，换机后需要重新写入。</li>
        </ul>
      </div>
      <div class="modal-actions"><button class="btn primary" id="help-close">知道了</button></div>`
    );
    $("#help-close").addEventListener("click", closeModal);
  }

  async function update() {
    try {
      const data = await api("/api/update/status");
      const latest = data.latest || data.latestVersion || data.version || "—";
      const current = data.current || state.info?.version || "—";
      const available = data.available ?? data.updateAvailable;
      openModal(
        "检查更新",
        "发布源：GitHub Releases；只检查，不自动替换本机文件。",
        `<div class="kv">
          <dt>当前版本</dt><dd>v${esc(current)}</dd>
          <dt>最新版本</dt><dd>v${esc(latest)}</dd>
          <dt>状态</dt><dd>${available ? '<span class="pill amber">有新版本</span>' : '<span class="pill signal">已是最新</span>'}</dd>
        </div>
        <div class="modal-actions"><button class="btn primary" id="update-close">关闭</button></div>`
      );
      $("#update-close").addEventListener("click", closeModal);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function bind() {
    if (bound) return;
    bound = true;
    $("#settings-reload").addEventListener("click", reloadConfig);
    $("#settings-restore").addEventListener("click", restoreBackup);
    $("#settings-overwrite")?.addEventListener("click", () => {
      overwrite = true;
      save().catch((error) => toast(error.message, "error"));
    });
    $("#settings-json").addEventListener("click", showRawConfig);
    $("#settings-diagnostics").addEventListener("click", exportDiagnostics);
    $("#settings-update").addEventListener("click", update);
    $("#settings-validate").addEventListener("click", () => {
      validateCurrent(config(), false).catch((error) => toast(error.message, "error"));
    });
    $("#unified-group-add").addEventListener("click", () => {
      readGroups();
      unifiedGroups.push({ id: "", label: "", upstreams: [] });
      renderGroups();
    });
    $("#route-group-add").addEventListener("click", () => {
      readGroups();
      routeGroups.push({ id: "", members: [] });
      renderGroups();
    });
    $("#unified-group-list").addEventListener("click", (event) => {
      const button = event.target.closest("[data-remove-unified]");
      if (!button) return;
      readGroups();
      unifiedGroups.splice(Number(button.dataset.removeUnified), 1);
      renderGroups();
    });
    $("#route-group-settings").addEventListener("click", (event) => {
      const button = event.target.closest("[data-remove-route]");
      if (!button) return;
      readGroups();
      routeGroups.splice(Number(button.dataset.removeRoute), 1);
      renderGroups();
    });
    const view = $("#view-settings");
    if (view) {
      view.addEventListener("input", () => {
        const dirty = $("#settings-dirty");
        if (dirty) dirty.hidden = false;
      });
      view.addEventListener("change", () => {
        const dirty = $("#settings-dirty");
        if (dirty) dirty.hidden = false;
      });
    }
  }

  window.MG_SETTINGS = { render, save };
  window.MG_HELP = help;
  window.MG_UPDATE = update;
  document.addEventListener("DOMContentLoaded", bind);
})();
