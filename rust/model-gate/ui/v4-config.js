/* v4 设置、统一接入、路由组与高级维护。依赖 window.MG。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, esc, num, fmtTime, api, toast, state, openModal, updateModal, closeModal } = MG;

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
    ["protocol", "string", "openai", "wire 协议：openai / anthropic / commandcode / opencode"],
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

  // ───────────────────────── 检查更新 ─────────────────────────
  //
  // 关键点：`/api/update/status` 只读内存状态——从未检查过时 `latest` 为空，
  // 直接拿它渲染会把「没查过」显示成「已是最新」。所以点击必须走
  // `/api/update/check?force=1` 真正联网查询；`status` 只用于并发检查时轮询兜底。
  //
  // 下载 / 安装链路（v3.1 合并旧面板时曾整块丢失，这里接回）：
  //   下载并安装 = download-install（下载 → SHA256 校验 → 自动静默安装，安装版）
  //   仅下载     = download，ready 后点「安装并重启」；便携版点「启动安装包」
  //   安装       = install，弹窗内二次确认——不能拆成独立 confirm 弹窗，
  //                那会整块重建 #modal-body，把更新弹窗清空。

  const UPDATE_PHASE = {
    idle: ["signal", "已是最新"],
    checking: ["signal", "检查中"],
    available: ["amber", "发现新版本"],
    downloading: ["amber", "正在下载"],
    ready: ["amber", "安装包已就绪"],
    installing: ["amber", "正在安装"],
    error: ["danger", "检查失败"],
  };
  const UPDATE_SUB =
    "发布源：GitHub Releases；安装版一键下载并静默安装，便携版下载后启动安装向导。";

  // 弹窗本地状态：跨轮询与重开保持（最近 status / 渲染缓存 / 确认态 / 说明展开态）。
  const updateUi = {
    status: null,
    html: "",
    timer: 0,
    busy: false,
    bound: false,
    confirmInstall: false,
    notesOpen: false,
  };

  // 顶栏「更新」按钮：3.0 之前旧面板就靠它提示「有新版本 vX」。
  // v4 重写时只留了设置页入口，按钮带着 `hidden` 永远不出现——
  // 用户因此完全看不到新版本，也点不到一键下载安装。这里按旧面板行为恢复。
  function updateButtonLabel(s) {
    if (!s) return "检查更新";
    if (s.phase === "downloading") return "下载中…";
    if (s.phase === "ready") return "安装包已就绪";
    if (s.phase === "installing") return "关闭并安装中…";
    if (s.available && s.latest) return `有新版本 v${s.latest}`;
    return "检查更新";
  }

  function refreshUpdateButton(s) {
    const button = $("#update-btn");
    if (!button) return;
    button.hidden = false;
    button.textContent = updateButtonLabel(s);
    button.classList.toggle(
      "update-available",
      Boolean(s && s.available && s.phase !== "ready" && s.phase !== "installing")
    );
  }

  // 只读当前状态（不联网）：面板启动时用它点亮顶栏按钮。
  async function loadUpdateStatus() {
    try {
      const status = await api("/api/update/status");
      updateUi.status = status;
      refreshUpdateButton(status);
      return status;
    } catch {
      // 状态接口失败也要让按钮可用：否则面板连更新入口都没有。
      refreshUpdateButton(null);
      return null;
    }
  }

  function fmtBytes(value) {
    const n = Number(value || 0);
    if (!Number.isFinite(n) || n <= 0) return "—";
    if (n >= 1073741824) return `${(n / 1073741824).toFixed(2)} GiB`;
    if (n >= 1048576) return `${(n / 1048576).toFixed(1)} MiB`;
    if (n >= 1024) return `${Math.round(n / 1024)} KiB`;
    return `${Math.round(n)} B`;
  }

  function postUpdate(path, body = {}) {
    return api(path, { method: "POST", body: JSON.stringify(body) });
  }

  function updateModalHtml(s) {
    const phase = s.phase || "idle";
    const [tone, phaseLabel] = UPDATE_PHASE[phase] || UPDATE_PHASE.idle;
    const label = phase === "idle" && !s.checked_at ? "未检查" : phaseLabel;
    const latest = s.latest ? `v${esc(s.latest)}` : "—";
    const portable = s.install_supported === false;
    const installDir = s.effective_install_dir || s.install_dir || "";
    const downloadDir = s.installer_dir || s.download_dir || "";
    const portableHint =
      '<p class="hint">当前为便携版：下载完成并通过校验后，点「启动安装包」会拉起安装向导（会先请求关闭本应用）；也可以自己到下载目录运行安装包。</p>';

    const rows = [
      `<dt>当前版本</dt><dd>v${esc(s.current || state.info?.version || "—")}</dd>`,
      `<dt>最新版本</dt><dd>${latest}</dd>`,
      `<dt>状态</dt><dd><span class="pill ${tone}">${label}</span></dd>`,
    ];
    if (s.asset) {
      rows.push(`<dt>安装包</dt><dd>${esc(s.asset.name)} · ${fmtBytes(s.asset.size)}</dd>`);
    }
    if (s.checked_at) rows.push(`<dt>检查时间</dt><dd>${esc(fmtTime(s.checked_at))}</dd>`);
    if (s.release_url) {
      rows.push(
        `<dt>发布说明</dt><dd><a href="${esc(s.release_url)}" target="_blank" rel="noreferrer">${esc(
          s.tag || "打开 Release"
        )}</a></dd>`
      );
    }

    let body = "";
    if (phase === "checking") {
      body = `<p>正在检查 GitHub Releases 上的最新版本…</p>`;
    } else if (phase === "downloading") {
      const progress = s.progress || {};
      const percent = progress.percent == null ? 0 : Math.max(0, Math.min(100, Number(progress.percent)));
      body =
        `<p>正在下载安装包…</p>` +
        `<div class="mixbar-track" style="max-width:420px"><i style="width:${percent}%"></i></div>` +
        `<p class="hint">${fmtBytes(progress.downloaded)}${progress.total ? ` / ${fmtBytes(progress.total)}` : ""} · ${Math.round(percent)}%</p>`;
    } else if (phase === "installing") {
      body =
        `<p>正在关闭应用并安装新版本…</p>` +
        `<p class="hint">安装位置：<code>${esc(installDir || "—")}</code>。应用会先停止服务再退出，安装完成后自动重启；若未自动打开，请手动启动 Model-Gate。</p>`;
    } else if (phase === "error") {
      const message = (s.error && s.error.message) || "更新操作失败";
      body = `<p style="color:var(--danger)">${esc(message)}</p>`;
    } else if (phase === "ready") {
      body =
        `<p>安装包已下载并通过 SHA256 校验。</p>` +
        (portable
          ? portableHint
          : s.auto_install
            ? '<p class="hint">已开启「自动安装」：下载完成后会自动开始安装；也可手动点下面的按钮。</p>'
            : "");
    } else if (phase === "available" && s.available) {
      body =
        `<p>发现新版本 ${latest}。</p>` +
        (s.installable
          ? ""
          : '<p style="color:var(--danger)">该版本缺少可用的安装包或 SHA256SUMS，请前往 Release 页面手动下载。</p>') +
        (portable ? portableHint : "") +
        (s.auto_download
          ? '<p class="hint">已开启「自动下载」：发现新版本后会在后台自动下载并校验。</p>'
          : "");
    } else if (s.checked_at) {
      body = `<p>已是最新版本 ${latest}。</p>`;
    } else {
      body = `<p>尚未检查更新。</p>`;
    }

    // 路径事实：「下到哪」「装到哪」直接可见；「打开」由服务端按枚举推导路径，
    // 面板不传任意路径（防任意文件打开器）。
    const paths = `<div class="kv" style="margin-top:10px">
      <dt>下载位置</dt><dd><code>${esc(downloadDir || "—")}</code> <button class="btn ghost sm" data-update-action="open-download">打开</button></dd>
      <dt>安装位置</dt><dd><code>${esc(installDir || "—")}</code> <button class="btn ghost sm" data-update-action="open-install">打开</button></dd>
    </div>`;

    // 下载目录可指定（DB 设置）；下载 / 安装进行中不接受修改。
    const dirEditor =
      phase === "downloading" || phase === "installing"
        ? ""
        : `<div style="margin-top:10px">
      <label class="hint" for="update-dir-input">下载目录${s.download_dir_custom ? "（自定义）" : "（默认：Windows 下载文件夹）"}</label>
      <div style="display:flex;gap:6px;margin-top:4px">
        <input id="update-dir-input" class="input" type="text" spellcheck="false" style="flex:1" value="${esc(s.download_dir || "")}" placeholder="留空保存 = 恢复默认目录">
        <button class="btn ghost sm" data-update-action="save-download-dir">保存</button>
        <button class="btn ghost sm" data-update-action="reset-download-dir">恢复默认</button>
      </div>
    </div>`;

    // 自动化开关；便携版无法静默自覆盖，自动安装开关置灰。
    const settings = `<div style="margin-top:10px;display:flex;flex-direction:column;gap:4px">
      <label class="check"><input type="checkbox" data-update-setting="autoDownload"${s.auto_download ? " checked" : ""}> 发现新版本后自动在后台下载</label>
      <label class="check"${portable ? ' title="便携版无法静默自覆盖，此开关不生效"' : ""}><input type="checkbox" data-update-setting="autoInstall"${s.auto_install ? " checked" : ""}${portable ? " disabled" : ""}> 下载完成后自动安装</label>
    </div>`;

    // Release 说明展开态存在 updateUi：轮询重渲染后不折起。
    const notes = s.notes
      ? `<details data-update-notes${updateUi.notesOpen ? " open" : ""} style="margin-top:10px"><summary class="hint" style="cursor:pointer">Release 说明（点开查看完整更新内容）</summary><pre style="white-space:pre-wrap;font-size:11px;margin:6px 0 0;color:var(--faint)">${esc(s.notes)}</pre></details>`
      : "";

    // 安装二次确认放在更新弹窗内部：独立 confirm 弹窗会重建 #modal-body，
    // 取消后更新弹窗会被清空。
    const confirmPanel = updateUi.confirmInstall
      ? `<div style="margin-top:12px;padding:10px 12px;border:1px solid var(--line);border-radius:8px">
          <p><strong>${portable ? "确认启动安装包向导？" : "确认现在安装并重启？"}</strong></p>
          <p class="hint">${portable ? "将打开安装向导，按提示完成安装。" : `应用将先停止服务再退出，安装器覆盖 <code>${esc(installDir || "—")}</code> 下的 Model-Gate.exe，完成后自动重启。`}</p>
          <div class="modal-actions"><button class="btn primary" data-update-action="install-confirm">${portable ? "启动安装包" : "确认安装并重启"}</button><button class="btn ghost" data-update-action="install-cancel">取消</button></div>
        </div>`
      : "";

    const actions = [];
    if (phase !== "downloading" && phase !== "installing") {
      actions.push(
        `<button class="btn ghost" data-update-action="check">${s.checked_at ? "重新检查" : "检查更新"}</button>`
      );
    }
    if (phase === "available" && s.installable && !s.installer_ready) {
      // 一键链路：下载 → 校验 → 自动安装（安装版仍会二次确认，不打招呼不重启）。
      if (!portable) {
        actions.push(`<button class="btn primary" data-update-action="download-install">下载并安装</button>`);
      }
      actions.push(`<button class="btn ghost" data-update-action="download">仅下载安装包</button>`);
    }
    if (phase === "ready") {
      actions.push(
        `<button class="btn primary" data-update-action="install">${portable ? "启动安装包" : "安装并重启"}</button>`
      );
    }
    if (s.release_url) {
      actions.push(`<button class="btn ghost" data-update-action="release">打开 Release 页面</button>`);
    }
    actions.push(`<button class="btn ghost" data-update-action="close">关闭</button>`);

    return (
      `<div class="prose"><dl class="kv">${rows.join("")}</dl>${body}` +
      (confirmPanel || `<div class="modal-actions">${actions.join("")}</div>`) +
      `</div>` +
      paths +
      dirEditor +
      settings +
      notes
    );
  }

  // 内容没变就不重建 DOM：轮询每秒重渲染会把展开的说明折起、滚动位置清零。
  function renderUpdate(view, reopen = false) {
    const html = updateModalHtml(view || {});
    if (!reopen && html === updateUi.html) return;
    updateUi.html = html;
    (reopen ? openModal : updateModal)("检查更新", UPDATE_SUB, html);
  }

  // 权威 status（后端返回）：覆盖缓存再渲染。瞬时错误走 renderUpdate，不冲缓存。
  function applyUpdate(status, reopen = false) {
    if (status) updateUi.status = status;
    refreshUpdateButton(updateUi.status);
    renderUpdate(updateUi.status, reopen);
  }

  function stopUpdatePolling() {
    if (updateUi.timer) {
      clearInterval(updateUi.timer);
      updateUi.timer = 0;
    }
  }

  // 下载进行中每秒拉一次内存状态；离开 downloading（ready / installing / error）
  // 就停——installing 的文案已交代「应用即将退出」，进程退出后页面自然失联。
  function startUpdatePolling() {
    stopUpdatePolling();
    updateUi.timer = setInterval(async () => {
      if ($("#modal").hidden) {
        stopUpdatePolling();
        return;
      }
      try {
        const status = await api("/api/update/status");
        applyUpdate(status);
        if (status.phase !== "downloading") stopUpdatePolling();
      } catch {
        stopUpdatePolling();
      }
    }, 1000);
  }

  // 并发检查时 `/check` 不会等待，直接回 `checking`；轮询到终态再渲染。
  async function waitUpdateResult() {
    for (let i = 0; i < 20; i += 1) {
      const data = await api("/api/update/status");
      if (data.phase !== "checking") return data;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    return api("/api/update/status");
  }

  async function runUpdateCheck() {
    if (updateUi.busy) return;
    updateUi.busy = true;
    updateUi.confirmInstall = false;
    renderUpdate({ ...(updateUi.status || {}), phase: "checking", error: null });
    try {
      const first = await api("/api/update/check?force=1");
      applyUpdate(first.phase === "checking" ? await waitUpdateResult() : first);
    } catch (error) {
      // 瞬时错误只改渲染不冲缓存状态：否则一次失败的检查会把
      // 「发现新版本」弄丢，之后「下载并安装」就再也点不了。
      renderUpdate({ ...(updateUi.status || {}), phase: "error", error: { message: error.message } });
      toast(error.message, "error");
    } finally {
      updateUi.busy = false;
    }
  }

  async function handleUpdateSetting(name, checked) {
    try {
      const body = name === "autoDownload" ? { autoDownload: checked } : { autoInstall: checked };
      applyUpdate(await postUpdate("/api/update/settings", body));
      toast(
        name === "autoDownload"
          ? checked
            ? "已开启：发现新版本后自动在后台下载"
            : "已关闭自动下载"
          : checked
            ? "已开启：下载完成后自动安装（仅安装版）"
            : "已关闭自动安装"
      );
    } catch (error) {
      toast(`保存设置失败：${error.message}`, "error");
    }
  }

  async function handleUpdateAction(action) {
    const s = updateUi.status || {};
    try {
      if (action === "check") {
        await runUpdateCheck();
      } else if (action === "download-install") {
        updateUi.confirmInstall = false;
        applyUpdate(await postUpdate("/api/update/download-install"));
        startUpdatePolling();
        toast(s.portable ? "已开始下载；完成后请点「启动安装包」" : "已开始下载，校验通过后自动安装");
      } else if (action === "download") {
        updateUi.confirmInstall = false;
        applyUpdate(await postUpdate("/api/update/download"));
        startUpdatePolling();
      } else if (action === "install") {
        updateUi.confirmInstall = true;
        renderUpdate(updateUi.status);
      } else if (action === "install-cancel") {
        updateUi.confirmInstall = false;
        renderUpdate(updateUi.status);
      } else if (action === "install-confirm") {
        updateUi.confirmInstall = false;
        const status = await postUpdate("/api/update/install");
        applyUpdate(status);
        // 便携版只拉起安装向导（launched），应用继续运行；安装版交给看护脚本后退出。
        toast(status.launched ? "已启动安装向导，按提示完成安装" : "安装程序已启动，应用即将退出…");
      } else if (action === "open-download" || action === "open-install") {
        const result = await postUpdate("/api/update/open-location", {
          target: action === "open-download" ? "download" : "install",
        });
        toast(result.path ? `已打开：${result.path}` : "已打开");
      } else if (action === "save-download-dir" || action === "reset-download-dir") {
        const input = $("#update-dir-input");
        const dir = action === "reset-download-dir" ? "" : input ? input.value.trim() : "";
        applyUpdate(await postUpdate("/api/update/download-dir", { dir }));
        toast(
          action === "reset-download-dir"
            ? "已恢复默认下载目录"
            : `下载目录已保存：${updateUi.status.download_dir || "—"}`
        );
      } else if (action === "release") {
        const url = updateUi.status && updateUi.status.release_url;
        if (url) window.open(url, "_blank", "noopener");
      } else if (action === "close") {
        stopUpdatePolling();
        closeModal();
      }
    } catch (error) {
      renderUpdate({ ...s, phase: "error", error: { message: error.message } });
      toast(error.message, "error");
    }
  }

  // 事件代理挂在 #modal-body 上，绑一次即可：弹窗内容整块替换不影响它。
  function bindUpdateDelegation() {
    if (updateUi.bound) return;
    updateUi.bound = true;
    const host = $("#modal-body");
    host.addEventListener("click", (event) => {
      const button = event.target.closest("[data-update-action]");
      if (button) handleUpdateAction(button.dataset.updateAction);
    });
    host.addEventListener("change", (event) => {
      const box = event.target.closest("[data-update-setting]");
      if (box) handleUpdateSetting(box.dataset.updateSetting, box.checked);
    });
    // toggle 不冒泡，用捕获阶段监听：轮询重渲染后说明保持展开态。
    host.addEventListener(
      "toggle",
      (event) => {
        if (event.target.matches?.("details[data-update-notes]")) {
          updateUi.notesOpen = event.target.open;
        }
      },
      true
    );
  }

  async function update() {
    bindUpdateDelegation();
    updateUi.confirmInstall = false;
    // 先画一版缓存状态（没查过就是「未检查」），force 检查完成后整块替换。
    applyUpdate(updateUi.status || { phase: "idle" }, true);
    await runUpdateCheck();
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
    // 面板启动先读一次状态（不联网）：顶栏按钮据此显示「检查更新 / 有新版本 vX」。
    loadUpdateStatus().then(() => {
      // 托盘「检查更新」以 `?update=1` 打开面板：进来就真实检查一次，
      // 而不是停在默认视图让用户再手动点一次。
      if (new URLSearchParams(location.search).get("update") === "1") {
        update().catch(() => {});
      }
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
