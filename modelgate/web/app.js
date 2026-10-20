/* Model-Gate 面板 v2.2
 * 页签：概览 / 模型 / 配置 / 用量 / 日志
 * 数据源：/api/info、/api/proxy/status、/api/upstreams(+health+probe)、
 *          /api/models/catalog、/api/pricing(+recompute)、
 *          /api/usage/{totals,summary,list,clear,export,retention/apply}、/api/logs
 */
"use strict";

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

const state = {
  config: null,
  status: null,
  profiles: [],
  effective: [],
  upstreams: [],
  health: {},
  range: "24h",
  group: "day",
  trendMetric: "total_tokens",
  pieKind: "model",
  page: 0,
  pageSize: 50,
  listTotal: 0,
  filters: { upstream: "", model: "", session: "" },
  // 自定义时间区间（datetime-local 值）；为空时使用 range 预设
  custom: { from: "", to: "" },
  options: { upstreams: [], models: [], sessions: [] },
  pricing: { currency: "CNY", configured: false, pricing: { default: {}, rules: {} } },
  catalog: { page: 1, pageSize: 25, total: 0, q: "", upstream: "", currency: "CNY",
             price: "", source: "", sort: "model", order: "asc" },
  usage: {
    totals: null,
    previous: null,
    models: [],
    sortKey: "total_tokens",
    sortDir: "desc",
    q: "",
    usedOnly: false,
    // 明细深化的查询状态：排序 / 快捷筛选 / 展开行 / 自动刷新
    listSort: "ts",
    listOrder: "desc",
    only: "",
    expanded: "",
    autoTimer: null,
    lastUpdated: null,
    // 上游用量表：数据、排序、展开的上游
    upstreams: [],
    upstreamModels: [],
    upstreamSortKey: "total_tokens",
    upstreamSortDir: "desc",
    upstreamExpanded: "",
  },
  // 概览页的上游表：完整数据、无分页，排序与展开都在前端即可
  overview: { sortKey: "name", sortDir: "asc", expanded: "" },
  // 配置页：有未保存改动时给提示，保存/重载后清除
  configDirty: false,
  // 当前打开的弹窗（同一时间只允许一个）
  modal: null,
  theme: "dark",
  logs: { level: "all", q: "", timer: null, lines: 400, autoSeconds: 4, lastUpdated: null, rows: [] },
};

/* ── 弹窗：维护类操作统一走对话框，不再用原生 confirm ─────────── */
function closeModal() {
  const root = $("#modal-root");
  if (!root) return;
  root.hidden = true;
  root.innerHTML = "";
  state.modal = null;
}

/* openModal({ title, hint, body, actions:[{label,className,onClick}] })
   onClick 返回 true 表示「保持打开」，否则关闭。 */
function openModal(opts) {
  const root = $("#modal-root");
  if (!root) return null;
  root.hidden = false;
  root.innerHTML = `<div class="modal-card" role="dialog" aria-modal="true">
      <div class="modal-head">
        <h3>${esc(opts.title || "")}</h3>
        <button type="button" class="ghost tiny" data-modal-close>关闭</button>
      </div>
      ${opts.hint ? `<div class="hint modal-hint">${esc(opts.hint)}</div>` : ""}
      <div class="modal-body">${opts.body || ""}</div>
      <div class="modal-actions">${
        (opts.actions || []).map((a, i) =>
          `<button type="button" class="${a.className || "ghost"}" data-modal-action="${i}">${
            esc(a.label)}</button>`).join("")
      }</div>
    </div>`;
  state.modal = opts;
  $$("[data-modal-close]", root).forEach((b) => b.addEventListener("click", closeModal));
  $$("[data-modal-action]", root).forEach((btn) => btn.addEventListener("click", () => {
    const action = (opts.actions || [])[Number(btn.dataset.modalAction)];
    if (!action) return;
    const keep = action.onClick ? action.onClick(root) : undefined;
    if (keep !== true) closeModal();
  }));
  root.addEventListener("click", (ev) => {
    if (ev.target === root) closeModal();
  });
  const first = root.querySelector("input, textarea, select, button.primary");
  if (first && first.focus) setTimeout(() => first.focus(), 30);
  return root;
}

/* 确认弹窗：带说明与危险样式，Promise<boolean>；替代 window.confirm */
function confirmModal({ title, text, confirmLabel = "确认", danger = true }) {
  return new Promise((resolve) => {
    const root = openModal({
      title,
      body: `<p class="modal-text">${esc(text || "")}</p>`,
      actions: [
        { label: "取消", onClick: () => resolve(false) },
        {
          label: confirmLabel,
          className: danger ? "primary danger-primary" : "primary",
          onClick: () => resolve(true),
        },
      ],
    });
    if (root) {
      // 关闭按钮/遮罩/Esc 都视为取消
      root.addEventListener("click", (ev) => {
        if (ev.target === root || ev.target.closest("[data-modal-close]")) resolve(false);
      });
    }
  });
}

/* 折叠状态记忆：details[data-fold] 的展开状态写 localStorage，刷新后保持 */
function initFolds() {
  $$("details[data-fold]").forEach((el) => {
    const key = "modelgate.fold." + el.dataset.fold;
    try {
      const saved = localStorage.getItem(key);
      if (saved !== null) el.open = saved === "1";
    } catch (_) {}
    el.addEventListener("toggle", () => {
      try { localStorage.setItem(key, el.open ? "1" : "0"); } catch (_) {}
    });
  });
}

document.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && state.modal) closeModal();
});

/* ── 主题（深/浅）：本地记忆，刷新后保持 ─────────────────── */
function applyTheme(name) {
  state.theme = name === "light" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", state.theme);
  try { localStorage.setItem("modelgate.theme", state.theme); } catch (_) {}
}

function initTheme() {
  let saved = "dark";
  try { saved = localStorage.getItem("modelgate.theme") || "dark"; } catch (_) {}
  applyTheme(saved);
  const btn = $("#theme-toggle");
  if (btn) btn.addEventListener("click", () => {
    applyTheme(state.theme === "dark" ? "light" : "dark");
    toast(`已切换到${state.theme === "dark" ? "深色" : "浅色"}主题`);
  });
}

/* ── 基础工具 ──────────────────────────────────────────── */
async function api(path, options) {
  const res = await fetch(path, options);
  let data = null;
  try { data = await res.json(); } catch (_) { data = null; }
  if (!res.ok) {
    const message = (data && data.error && data.error.message) || `HTTP ${res.status}`;
    throw new Error(message);
  }
  return data;
}

function postJson(path, body) {
  return api(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
}

let toastTimer = null;
function toast(message, isError = false) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.toggle("err", isError);
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, isError ? 6000 : 2800);
}

function esc(value) {
  return String(value == null ? "" : value)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function fmtInt(n) { return Number(n || 0).toLocaleString("zh-CN"); }

/* 中文数量单位：万 / 亿 / 万亿（最多两位小数，去掉无意义的 0） */
function fmtCn(n, digits = 2) {
  const v = Number(n || 0);
  const abs = Math.abs(v);
  const sign = v < 0 ? "-" : "";
  const trim = (x) => x.toFixed(digits).replace(/\.0+$/, "").replace(/(\.\d*?)0+$/, "$1");
  if (abs >= 1e12) return sign + trim(abs / 1e12) + "万亿";
  if (abs >= 1e8) return sign + trim(abs / 1e8) + "亿";
  if (abs >= 1e4) return sign + trim(abs / 1e4) + "万";
  return sign + Math.round(abs).toLocaleString("zh-CN");
}

/* 人类可读数量：30万 / 1.5亿 / 3,000,000 / 384K / 1M 都能读；读不出来返回 null
   为什么要这个：输入框一旦收到“30万”这种带单位的值，后端会判“必须是数字”而 400，表现就是“存不了”。 */
function parseCount(text) {
  if (text === null || text === undefined) return null;
  let s = String(text).trim().replace(/[,\s_　]/g, "");
  if (!s) return null;
  const unit = { "\u4e07": 1e4, "\u4ebf": 1e8, "\u5343": 1e3, "k": 1024, "K": 1024, "m": 1048576, "M": 1048576 };
  const m = s.match(/^(-?\d+(?:\.\d+)?)([\u4e07\u4ebf\u5343kKmM])?$/);
  if (!m) return null;
  const num = Number(m[1]);
  if (!isFinite(num)) return null;
  const scale = m[2] ? unit[m[2]] : 1;
  return Math.round(num * scale);
}

/* 上游地址规范化：缺协议时自动补协议
   为什么：上游列表/弹窗里把地址写成 api.x.com/v1 是「保存不了」的头号原因，
   而原提示只说“有 1 处错误”，用户不知道改哪儿；补全并明确告知比拦住更有用。
   协议选择：本机回环地址补 http://（本地桥接服务多是明文），其余补 https://。 */
function normalizeBaseUrl(raw) {
  const s = String(raw === null || raw === undefined ? "" : raw).trim();
  if (!s) return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(s)) return s;
  const host = s.replace(/^\/+/, "").split("/")[0].split(":")[0].toLowerCase();
  const local = host === "localhost" || host === "::1" || host === "[::1]"
    || host === "0.0.0.0" || /^127\./.test(host) || /^192\.168\./.test(host)
    || /^10\./.test(host) || /^172\.(1[6-9]|2\d|3[01])\./.test(host);
  return (local ? "http://" : "https://") + s.replace(/^\/+/, "");
}

/* 缓存命中率：口径 = 读取缓存的输入 / 总输入（OpenAI 口径，prompt_tokens 已含缓存部分） */
function cacheHitRate(cacheRead, prompt) {
  const c = Number(cacheRead) || 0;
  const p = Number(prompt) || 0;
  if (!p) return null;
  return (c / p) * 100;
}

function fmtRate(cacheRead, prompt, digits = 1) {
  const rate = cacheHitRate(cacheRead, prompt);
  if (rate === null) return "—";
  return rate.toFixed(digits).replace(/\.0$/, "") + "%";
}

/* token 数量：中文单位展示 + 精确值 tooltip；明细与导出仍用精确数字 */
function fmtTok(n) {
  const v = Number(n || 0);
  return `<span class="cn-num" title="${esc(fmtInt(v))} token">${esc(fmtCn(v))}</span>`;
}

function fmtCompact(n) { return fmtCn(n); }
function fmtMs(ms) {
  const v = Number(ms || 0);
  if (v < 1000) return Math.round(v) + "ms";
  return (v / 1000).toFixed(1) + "s";
}
function fmtUptime(seconds) {
  const s = Math.max(0, Number(seconds) || 0);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600),
        m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d} 天 ${h} 小时`;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${Math.floor(s % 60)} 秒`;
  return Math.floor(s) + " 秒";
}
function fmtTime(ts, createdAt) {
  if (createdAt) return String(createdAt).replace("T", " ").slice(0, 19);
  const d = new Date(Number(ts) * 1000);
  const p = (x) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
         `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
function fmtBucket(bucket, group) {
  if (group === "hour") return String(bucket).slice(5, 13).replace("T", " ") + ":00";
  if (group === "day") return String(bucket).slice(5);
  return String(bucket);
}

function kpi(id, label, value, extra, tone) {
  return `<div class="kpi${tone ? " " + tone : ""}"><div class="k">${esc(label)}</div>` +
         `<div class="v">${value}${extra ? `<small>${esc(extra)}</small>` : ""}</div></div>`;
}

/* 费用格式化：未配置单价时显示「—」，避免把「0 元」误读成「免费」 */
function fmtCost(value, pricing) {
  const ready = !!(pricing && pricing.configured);
  if (!ready) return "—";
  const v = Number(value || 0);
  return v.toFixed(v >= 1 ? 2 : 4);
}

function costExtra(pricing) {
  if (!pricing || !pricing.configured) return "未配置单价";
  return `${pricing.currency || "CNY"} · 本地口径估算`;
}

function copyText(text) {
  const done = () => toast(`已复制 ${text}`);
  const fail = () => toast("复制失败，请手动选择复制", true);
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done, fail);
    return;
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
    done();
  } catch (_) {
    fail();
  }
}

/* ── 页签 ──────────────────────────────────────────────── */
function initTabs() {
  $$(".tab-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      $$(".tab-btn").forEach((b) => b.classList.toggle("active", b === btn));
      $$(".panel").forEach((p) => p.classList.toggle(
        "active", p.id === `panel-${btn.dataset.tab}`));
      if (btn.dataset.tab === "overview") loadOverview();
      if (btn.dataset.tab === "model") { loadCatalog(); loadPricing(); }
      if (btn.dataset.tab === "config") loadConfig();
      if (btn.dataset.tab === "usage") loadUsage();
      if (btn.dataset.tab === "logs") loadLogs();
    });
  });
}

/* ── 概览 ──────────────────────────────────────────────── */
async function loadInfo() {
  try {
    const info = await api("/api/info");
    $("#version").textContent = `v${info.version}`;
    $("#conn").textContent = `运行中 · :${info.port}`;
    $("#conn").className = "conn ok";
  } catch (err) {
    $("#conn").textContent = "未连接";
    $("#conn").className = "conn bad";
  }
}

async function loadOverview() {
  await loadInfo();

  let status = null;
  try { status = await api("/api/proxy/status"); state.status = status; }
  catch (err) { state.status = null; }

  try {
    const data = await api("/api/upstreams");
    state.upstreams = data.upstreams || [];
    renderUpstreamTable();
    renderFilterOptions();
  } catch (err) {
    toast(`上游列表加载失败：${err.message}`, true);
  }

  // KPI
  if (status) {
    $("#overview-cards").innerHTML = [
      kpi("", "监听端口", `:${status.port}`, status.host),
      kpi("", "上游数量", fmtInt(status.upstream_count), "个"),
      kpi("", "对外模型", fmtInt(status.model_count), "个"),
      kpi("", "代理端点", `<code class="ep">/v1</code>`, status.endpoint, "accent"),
      kpi("", "运行时长", fmtUptime(status.uptime_seconds), "", "ok"),
      kpi("", "访问日志", status.access_log ? "开启" : "关闭",
          status.thinking.enabled ? "thinking 已开" : "", status.access_log ? "ok" : "warn"),
    ].join("");
  }

  // 代理接入信息（帮助用户把客户端指过来）
  const callouts = $("#endpoint-callouts");
  if (status) {
    callouts.innerHTML = `
    <div class="callout">
      <div class="t">OpenAI 兼容入口（把客户端的 base_url 指到这里）</div>
      <span class="mono-block">${esc(status.endpoint)}</span>
    </div>
    <div class="callout">
      <div class="t">高频用法</div>
      <span class="mono-block">模型清单  GET  ${esc(status.endpoint)}/models
对话补全  POST ${esc(status.endpoint)}/chat/completions
指定上游  POST http://127.0.0.1:${status.port}/&lt;上游名&gt;/v1/chat/completions
健康检查  GET  http://127.0.0.1:${status.port}/health</span>
    </div>
    <div class="callout warn">
      <div class="t">本地数据</div>
      配置：<code>${esc(status.config_path || "")}</code><br>
      用量库：<code>${esc(status.db_path || "")}</code>（SQLite，仅落本机）
    </div>`;
  } else {
    callouts.innerHTML = `<div class="callout danger"><div class="t">无法获取代理状态</div>
      请刷新页面重试；若持续失败，请到「日志」页查看报错。</div>`;
  }
}

async function probeHealth() {
  const btn = $("#probe-health");
  const hint = $("#health-hint");
  const box = $("#health-results");
  if (btn) btn.disabled = true;
  if (hint) { hint.textContent = "探测中…"; hint.classList.remove("err"); }

  try {
    const data = await api("/api/upstreams/health?timeout=8");
    const rows = data.rows || [];
    state.health = {};
    rows.forEach((r) => { state.health[r.name] = r; });
    renderUpstreamTable();
    box.hidden = false;
    box.innerHTML = rows.map((r) => {
      const badge = r.ok ? '<span class="badge ok">可用</span>'
        : `<span class="badge bad">${r.status ? "HTTP " + r.status : "不可达"}</span>`;
      const hintLine = r.hint
        ? `<div class="val" style="color:var(--warn)">建议：${esc(r.hint)}</div>` : "";
      return `<div class="bar-row">
        <div class="bar-head">
          <span class="label">${esc(r.name)} · ${esc(r.protocol)} · ${esc(r.baseUrl)}</span>
          <span class="val">${esc(r.keySource || "")} · ${r.elapsed_ms}ms ${badge}</span>
        </div>
        <div class="val" style="color:var(--text-2);word-break:break-all">${esc((r.message || "").slice(0, 300))}</div>
        ${hintLine}
      </div>`;
    }).join("") || '<div class="empty">没有上游</div>';
    const bad = rows.filter((r) => !r.ok).length;
    if (hint) {
      hint.textContent = bad === 0 ? `${rows.length} 个上游全部可用` : `${bad}/${rows.length} 个上游不可用`;
      hint.classList.toggle("err", bad > 0);
    }
  } catch (err) {
    if (hint) { hint.textContent = `探测失败：${err.message}`; hint.classList.add("err"); }
  } finally {
    if (btn) btn.disabled = false;
  }
}

/* ── 上游表（含内联测试） ──────────────────────────────── */
const UPSTREAM_SORT_LABELS = {
  name: "名称", protocol: "协议", models: "模型数", plan: "套餐", key: "密钥", default: "默认",
};

/* 上游列表是一次性完整数据（没有分页），因此排序在前端完成是正确的；
   反例是分页表格 —— 那里必须由后端在完整结果集上排序。 */
function sortedUpstreams() {
  const key = state.overview.sortKey;
  const dir = state.overview.sortDir === "asc" ? 1 : -1;
  const weight = (u) => {
    if (key === "models") return Number(u.models) || 0;
    if (key === "plan") return (u.plan || (u.planError ? "~" : "zz")).toLowerCase();
    if (key === "key") return u.keyConfigured ? 1 : 0;
    if (key === "default") return u.default ? 1 : 0;
    if (key === "protocol") return String(u.protocol || "").toLowerCase();
    return String(u.name || "").toLowerCase();
  };
  return state.upstreams.slice().sort((a, b) => {
    const wa = weight(a);
    const wb = weight(b);
    if (wa === wb) return String(a.name).localeCompare(String(b.name));
    return (wa > wb ? 1 : -1) * dir;
  });
}

function renderUpstreamSortMarks() {
  const table = $("#upstream-table");
  if (!table) return;
  $$("th[data-up-sort]", table).forEach((th) => {
    const active = th.dataset.upSort === state.overview.sortKey;
    th.classList.toggle("sort-active", active);
    th.setAttribute("aria-sort", active
      ? (state.overview.sortDir === "asc" ? "ascending" : "descending") : "none");
    const base = th.dataset.label || th.textContent.replace(/[▲▼]\s*$/, "").trim();
    th.dataset.label = base;
    th.textContent = active ? `${base} ${state.overview.sortDir === "asc" ? "▲" : "▼"}` : base;
  });
}

/* 上游详情：Base URL、密钥来源、模型清单、套餐限制都在展开行里，表格保持八列 */
function upstreamDetailRow(u) {
  const models = (u.modelsAll || []).slice(0, 24).join(", ");
  const items = [
    ["Base URL", u.baseUrl],
    ["密钥来源", u.keyConfigured ? (u.keySource || "已配置") : "未配置"],
    ["密钥变量", u.apiKeyEnv || u.apiKeyFile],
    ["白名单模型", `${u.models} 个`],
    ["全部模型", `${u.modelsAll ? u.modelsAll.length : 0} 个${models ? "：" + models : ""}`],
    ["套餐", u.plan || (u.planError ? `未知（${u.planError}）` : "无门槛信息")],
    ["会话头", u.sessionHeader ? "已开启" : "未开启"],
  ];
  return `<tr class="detail-row"><td colspan="8"><div class="detail-grid">${
    items.map(([k, v]) => {
      const text = v === null || v === undefined || v === "" ? "—" : String(v);
      return `<div class="detail-item"><span class="detail-key">${esc(k)}</span>` +
        `<span class="detail-val mono">${esc(text)}</span>` +
        `${text !== "—" ? `<button class="ghost tiny" data-copy="${esc(text)}">复制</button>` : ""}</div>`;
    }).join("")
  }</div></td></tr>`;
}

function renderUpstreamTable() {
  const tbody = $("#upstream-table tbody");
  if (!tbody) return;
  if (!state.upstreams.length) {
    tbody.innerHTML = '<tr><td colspan="8" class="empty">还没有配置上游</td></tr>';
    renderUpstreamSortMarks();
    return;
  }
  tbody.innerHTML = sortedUpstreams().map((u) => {
    const plan = u.plan
      ? `<span class="badge brand">${esc(u.plan)}</span>`
      : (u.planError ? `<span class="badge bad" title="${esc(u.planError)}">未知</span>`
                     : `<span class="badge">—</span>`);
    const health = state.health[u.name];
    let healthBadge = `<button type="button" class="ghost btn-test-up" data-name="${esc(u.name)}" title="立即测试该上游">测试</button>`;
    if (health) {
      healthBadge = health.ok
        ? `<span class="badge ok">可用</span>`
        : `<span class="badge bad" title="${esc(health.hint || health.message || "")}">${
            health.status ? "HTTP " + health.status : "不可达"}</span>
           <button type="button" class="ghost btn-test-up" data-name="${esc(u.name)}">重测</button>`;
    }
    const keyHint = !u.keyConfigured
      ? `<span class="badge bad" title="未配置真实密钥，上游会返回 401">未配置</span>`
      : `<span class="badge">${esc(u.keySource)}</span>`;
    const open = state.overview.expanded === u.name;
    const main = `<tr data-up="${esc(u.name)}" class="usage-row${open ? " open" : ""}${
      u.keyConfigured ? "" : " row-bad"}">
      <td><div class="up-name">${esc(u.name)}</div></td>
      <td><span class="badge">${esc(u.protocol)}</span></td>
      <td class="mono">${esc(u.baseUrl)}</td>
      <td class="num">${fmtInt(u.models)}<span style="color:var(--text-3)"> / ${fmtInt(u.modelsAll ? u.modelsAll.length : 0)}</span></td>
      <td>${plan}</td>
      <td>${keyHint}</td>
      <td>${u.default ? '<span class="badge brand">默认</span>' : "—"}</td>
      <td>${healthBadge}</td>
    </tr>`;
    return open ? main + upstreamDetailRow(u) : main;
  }).join("");
  renderUpstreamSortMarks();
  const hint = $("#upstream-table-hint");
  if (hint) {
    const label = UPSTREAM_SORT_LABELS[state.overview.sortKey] || state.overview.sortKey;
    hint.textContent = `共 ${fmtInt(state.upstreams.length)} 个上游 · 按 ${label} ${
      state.overview.sortDir === "asc" ? "升序" : "降序"} · 点任意行展开详情`;
  }
}

async function testUpstream(name) {
  const hint = $("#health-hint");
  if (hint) { hint.textContent = `正在测试 ${name} …`; }
  try {
    const data = await postJson("/api/upstreams/probe", { name, timeout: 8 });
    const row = data.row || {};
    state.health[name] = row;
    renderUpstreamTable();
    if (hint) {
      hint.textContent = row.ok
        ? `${name} 可用（${row.elapsed_ms}ms）`
        : `${name} 不可用：${row.message}${row.hint ? "；建议：" + row.hint : ""}`;
      hint.classList.toggle("err", !row.ok);
    }
    toast(row.ok ? `${name} 连通正常` : `${name}：${row.hint || row.message}`, !row.ok);
  } catch (err) {
    if (hint) { hint.textContent = `测试失败：${err.message}`; hint.classList.add("err"); }
    toast(`测试 ${name} 失败：${err.message}`, true);
  }
}

function initUpstreamActions() {
  const btn = $("#probe-health");
  if (btn) btn.addEventListener("click", probeHealth);

  const copyBtn = $("#copy-endpoint");
  if (copyBtn) copyBtn.addEventListener("click", async () => {
    const url = (state.status && state.status.endpoint) || "http://127.0.0.1:8900/v1";
    try {
      await navigator.clipboard.writeText(url);
      toast(`已复制 ${url}`);
    } catch (_) {
      toast("复制失败，请手动选择", true);
    }
  });

  const table = $("#upstream-table");
  if (table) {
    // 表头排序：上游列表是完整数据（无分页），前端排序即可，且不会与 total 打架
    $$("th[data-up-sort]", table).forEach((th) => {
      th.addEventListener("click", () => {
        const key = th.dataset.upSort;
        if (state.overview.sortKey === key) {
          state.overview.sortDir = state.overview.sortDir === "asc" ? "desc" : "asc";
        } else {
          state.overview.sortKey = key;
          state.overview.sortDir = key === "name" || key === "protocol" || key === "plan" ? "asc" : "desc";
        }
        renderUpstreamTable();
      });
    });
    table.addEventListener("click", (ev) => {
      const testBtn = ev.target.closest(".btn-test-up");
      if (testBtn) { testUpstream(testBtn.dataset.name); return; }
      const copyBtn = ev.target.closest("[data-copy]");
      if (copyBtn) { copyText(copyBtn.dataset.copy || ""); return; }
      const row = ev.target.closest("tr[data-up]");
      if (!row) return;
      const name = row.dataset.up;
      state.overview.expanded = state.overview.expanded === name ? "" : name;
      renderUpstreamTable();
    });
  }

  $("#health-results") && $("#health-results").addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-act]");
    if (btn && btn.dataset.act === "retest") {
      testUpstream(btn.dataset.name);
    }
  });
}

/* ── 配置 ──────────────────────────────────────────────── */
function parseModels(text) {
  return String(text || "").split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
}

function parseAliases(text) {
  const out = {};
  String(text || "").split("\n").forEach((line) => {
    const m = line.match(/^\s*([^=:\s]+)\s*[=:]\s*(.+?)\s*$/);
    if (m) out[m[1]] = m[2];
  });
  return out;
}

const CAPABILITY_LABELS = {
  stream: "流式",
  tools: "工具",
  vision: "视觉",
  thinking: "思考",
  cache: "缓存",
  systemPrompt: "系统提示",
};

const FALLBACK_PROFILE_CHOICES = ["openai", "opencode", "anthropic", "deepseek", "commandcode"];

/* 预设下拉：优先用服务端注册表（/api/config 的 profiles）；缺失时回退旧硬编码。 */
function profileChoices() {
  const list = (state.profiles || []).filter((p) => p && p.id);
  return list.length
    ? list.map((p) => [p.id, p.label || p.id])
    : FALLBACK_PROFILE_CHOICES.map((id) => [id, id]);
}

function capabilityBadges(caps) {
  return Object.entries(CAPABILITY_LABELS)
    .filter(([key]) => caps && caps[key])
    .map(([key, label]) => `<span class="badge ok" title="${esc(key)}">${esc(label)}</span>`)
    .join(" ");
}

function addUpstream() {
  if (!state.config) return;
  state.config.upstreams = state.config.upstreams || {};
  let n = 1, name = `upstream${n}`;
  while (state.config.upstreams[name]) { n += 1; name = `upstream${n}`; }
  state.config.upstreams[name] = {
    protocol: "anthropic",
    baseUrl: "http://127.0.0.1:15721",
    models: [],
    default: Object.keys(state.config.upstreams).length === 0,
  };
  renderUpstreamEditor();
}

async function saveUpstreamSecret(name, secret, input) {
  const ok = await confirmModal({
    title: "写入密钥文件",
    text: `对象：${name}\n写入位置：配置里 apiKeyEnv 指向的变量（默认 ~/.jcode/.env）\n说明：只写密钥文件、不写 bridge.config.json；不重启、不回显。`,
    confirmLabel: "写入",
    danger: false,
  });
  if (!ok) return;
  try {
    const data = await postJson("/api/upstreams/secret", { name, apiKey: secret });
    toast(`已写入 ${data.envName || "密钥文件"} 并热更新`);
    if (input) input.value = "";
    await loadConfig();
    await loadOverview();
  } catch (err) {
    toast(`写入失败：${err.message}`, true);
  }
}

/* 上游「列表 + 详情」：表格负责浏览与操作，隐藏的卡片仍是唯一表单源 */
function upstreamDetailRowCfg(card) {
  const val = (f) => {
    const el = card.querySelector(`[data-field="${f}"]`);
    if (!el) return "";
    return el.type === "checkbox" ? (el.checked ? "开" : "关") : (el.value || "");
  };
  const models = val("models").split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
  const items = [
    ["模型清单", models.slice(0, 12).join(", ") + (models.length > 12 ? ` …共 ${models.length} 个` : "")],
    ["密钥环境变量", val("apiKeyEnv") || "未设置（直接用配置文件里的密钥）"],
    ["密钥状态", card.dataset.keyConfigured === "1" ? "已配置" : "未配置（上游会返回 401）"],
    ["自动拉取模型", val("autoModels")],
    ["鉴权方式", val("compatAuthScheme") || "跟随 provider 预设"],
    ["会话头", val("compatSessionHeader") || "跟随预设 / 不注入"],
    ["对话端点", val("compatChatPath") || "跟随预设"],
    ["最大输出", val("compatMaxOutput") || "跟随预设"],
    ["思考回传", val("compatThinking") === "on" ? "开启" : val("compatThinking") === "off" ? "关闭" : "跟随预设"],
    ["pause_turn 续跑", val("pauseTurn") === "" ? "—" : val("pauseTurn")],
  ];
  return `<tr class="detail-row"><td colspan="7"><div class="detail-grid">${
    items.map(([k, v]) => {
      const text = String(v === "" || v === null || v === undefined ? "—" : v);
      return `<div class="detail-item"><span class="detail-key">${esc(k)}</span>`
        + `<span class="detail-val mono" title="${esc(text)}">${esc(text)}</span></div>`;
    }).join("")
  }</div></td></tr>`;
}

function renderUpstreamCfgTable() {
  const tbody = $("#upstream-cfg-table tbody");
  if (!tbody) return;
  const cards = $$("#upstream-editor .up-card");
  if (!cards.length) {
    tbody.innerHTML = '<tr><td colspan="7" class="empty">还没有上游，点「＋ 新增上游」创建</td></tr>';
    return;
  }
  tbody.innerHTML = cards.map((card, index) => {
    const name = card.dataset.name || "";
    const field = (f) => {
      const el = card.querySelector(`[data-field="${f}"]`);
      if (!el) return "";
      return el.type === "checkbox" ? el.checked : el.value;
    };
    const models = String(field("models") || "").split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
    const isDefault = !!field("default");
    const keyOk = card.dataset.keyConfigured === "1";
    const open = state.cfgUpstreamExpanded === name;
    const main = `<tr class="usage-row${open ? " open" : ""}" data-up-cfg="${esc(name)}">
      <td class="mono">${esc(name)}</td>
      <td><span class="badge">${esc(field("protocol") || "anthropic")}</span></td>
      <td class="mono">${esc(field("baseUrl") || "—")}</td>
      <td class="num mono">${fmtInt(models.length)}</td>
      <td>${keyOk ? '<span class="badge ok">已配置</span>' : '<span class="badge bad">未配置</span>'}</td>
      <td>${isDefault ? '<span class="badge brand">默认</span>' : "—"}</td>
      <td class="num">
        <button type="button" class="ghost tiny" data-cfg-act="edit" data-name="${esc(name)}">编辑</button>
        <button type="button" class="ghost tiny" data-cfg-act="up" data-name="${esc(name)}" ${index === 0 ? "disabled" : ""}>上移</button>
        <button type="button" class="ghost tiny" data-cfg-act="down" data-name="${esc(name)}" ${index === cards.length - 1 ? "disabled" : ""}>下移</button>
        <button type="button" class="ghost tiny danger-ghost" data-cfg-act="del" data-name="${esc(name)}">删除</button>
      </td>
    </tr>`;
    return open ? main + upstreamDetailRowCfg(card) : main;
  }).join("");

  tbody.onclick = (ev) => {
    const btn = ev.target.closest("[data-cfg-act]");
    if (btn) {
      ev.stopPropagation();
      const name = btn.dataset.name;
      const card = $$("#upstream-editor .up-card").find((c) => c.dataset.name === name);
      if (!card) return;
      const act = btn.dataset.cfgAct;
      if (act === "edit") {
        openUpstreamEditor(card);
      } else {
        const proxy = card.querySelector(`[data-act="${act}"]`);
        if (proxy) proxy.click();
      }
      return;
    }
    const row = ev.target.closest("tr[data-up-cfg]");
    if (!row) return;
    const name = row.dataset.upCfg;
    state.cfgUpstreamExpanded = state.cfgUpstreamExpanded === name ? "" : name;
    renderUpstreamCfgTable();
  };

  const hint = $("#upstream-cfg-hint");
  if (hint) {
    const bad = cards.filter((c) => c.dataset.keyConfigured !== "1").length;
    hint.textContent = `共 ${fmtInt(cards.length)} 个上游`
      + (bad ? ` · ${bad} 个未配置密钥（会返回 401）` : "")
      + " · 点击任意行展开详情 ·「编辑」在弹窗里改字段";
  }
}

function renderUpstreamEditor() {
  const host = $("#upstream-editor");
  if (!host) return;
  const ups = (state.config && state.config.upstreams) || {};
  const names = Object.keys(ups);
  host.innerHTML = names.map((name, index) => {
    const u = ups[name];
    const isCC = (u.protocol || "") === "commandcode";
    const isOAI = (u.protocol || "") === "openai";
    const compat = u.compat || {};
    const cProfile = compat.profile || "";
    const cAuth = (compat.auth && compat.auth.scheme) || "";
    const cSession = compat.session && compat.session.enabled === false
      ? "-" : ((compat.session && compat.session.header) || "");
    const cChat = (compat.endpoint && compat.endpoint.chat) || "";
    const cHeaders = compat.headers && Object.keys(compat.headers).length
      ? JSON.stringify(compat.headers, null, 2) : "";
    const cMaxOut = (compat.limits && compat.limits.maxOutputTokens) || "";
    const cThink = compat.thinking && typeof compat.thinking.passthrough === "boolean"
      ? (compat.thinking.passthrough ? "on" : "off") : "";
    const cDowngrade = compat.toolChoice
      && typeof compat.toolChoice.downgradeForced === "boolean"
      ? (compat.toolChoice.downgradeForced ? "on" : "off") : "";
    const cZdr = typeof compat.zdr === "boolean" ? (compat.zdr ? "on" : "off") : "";
    const cVersion = Object.prototype.hasOwnProperty.call(compat, "versionHeader")
      ? (compat.versionHeader || "-") : "";
    const cEnvironment = Object.prototype.hasOwnProperty.call(compat, "environmentHeader")
      ? (compat.environmentHeader || "-") : "";
    const cOverrides = u.modelOverrides && Object.keys(u.modelOverrides).length
      ? JSON.stringify(u.modelOverrides, null, 2) : "";
    const keyConfigured = u._keyConfigured;
    const eff = ((state.effective || []).find((row) => row.name === name)) || null;
    const keyWarn = keyConfigured ? "" :
      `<div class="hint" style="color:var(--warn)">未配置密钥：上游会返回 401，请在下方填写 apiKey（或设置 apiKeyEnv / agent 来源）</div>`;
    return `
    <div class="up-card" data-name="${esc(name)}" data-index="${index}" data-key-configured="${keyConfigured ? 1 : 0}">
      <div class="up-head">
        <span class="name">${esc(name)}</span>
        ${u.default ? '<span class="badge brand">默认上游</span>' : ""}
        <span class="badge">${esc(u.protocol || "anthropic")}${compat.profile ? ` / ${esc(compat.profile)}` : ""}</span>
        <span class="spacer"></span>
        <div class="up-actions">
          <button type="button" class="ghost" data-act="up" ${index === 0 ? "disabled" : ""}>上移</button>
          <button type="button" class="ghost" data-act="down" ${index === names.length - 1 ? "disabled" : ""}>下移</button>
          <button type="button" class="ghost danger-ghost" data-act="del">删除</button>
        </div>
      </div>
      <details class="up-fields" data-up-fields="${esc(name)}">
      <summary>
        <span class="up-preview">${esc(u.protocol || "anthropic")} · ${esc(u.baseUrl || "未填 Base URL")} · ${
          (u.models || []).length} 个模型${u.apiKeyEnv ? ` · 密钥变量 ${esc(u.apiKeyEnv)}` : ""}${
          u.apiKeyAgent ? ` · agent 来源 ${esc(u.apiKeyAgent)}` : ""}</span>
        <button type="button" class="ghost" data-act="edit">编辑</button>
      </summary>
      <div class="grid">
        <label>名称（同时是 URL 前缀 /${esc(name)}）
          <input type="text" data-field="name" value="${esc(name)}">
        </label>
        <label>协议
          <select data-field="protocol">
            ${["anthropic", "openai", "commandcode"].map((p) =>
              `<option value="${p}"${(u.protocol || "anthropic") === p ? " selected" : ""}>${p}</option>`
            ).join("")}
          </select>
        </label>
        <label class="span-2">Base URL
          <input type="text" data-field="baseUrl" value="${esc(u.baseUrl || "")}" placeholder="http://127.0.0.1:15721">
        </label>
        <label>API Key
          <div class="key-row">
            <input type="password" data-field="apiKey" value="" placeholder="${keyConfigured ? "已配置 · 留空保持不变" : "未配置"}">
            <button type="button" class="ghost" data-act="save-env"
              title="把上面填的密钥写入密钥文件（apiKeyEnv 指向的变量）并立即生效，无需重启">写入 .env</button>
          </div>
        </label>
        <label>密钥环境变量（apiKeyEnv）
          <input type="text" data-field="apiKeyEnv" value="${esc(u.apiKeyEnv || "")}" placeholder="COMMAND_CODE_API_KEY">
        </label>
        <label>密钥来源（agent 工具）
          <select data-field="apiKeyAgent">
            ${[["", "不启用"], ["opencode", "opencode (auth.json)"], ["jcode", "jcode (.env)"],
               ["commandcode", "CommandCode (auth.json)"], ["custom", "自定义文件"]]
              .map(([v, label]) => `<option value="${v}"${(u.apiKeyAgent || "") === v ? " selected" : ""}>${label}</option>`).join("")}
          </select>
        </label>
        <label>agent Provider / 变量名
          <input type="text" data-field="apiKeyAgentProvider" value="${esc(u.apiKeyAgentProvider || "")}" placeholder="opencode 填 provider；jcode/自定义填变量名">
        </label>
        <label class="span-2">agent 凭据路径（可选）
          <input type="text" data-field="apiKeyAgentPath" value="${esc(u.apiKeyAgentPath || "")}" placeholder="留空使用默认路径；custom 时必填，如 ~/.codex/auth.json">
        </label>
        <div class="span-2 hint">只读本机客户端凭据；优先级低于 apiKeyEnv / apiKeyFile，高于明文 apiKey。</div>
        <label class="span-2">模型清单（每行一个，也是路由依据）
          <textarea data-field="models" rows="3">${esc((u.models || []).join("\n"))}</textarea>
        </label>
        <label class="check">
          <input type="checkbox" data-field="default" ${u.default ? "checked" : ""}>
          设为默认上游（按模型名分发时的兜底）
        </label>
        <label class="check">
          <input type="checkbox" data-field="autoModels" ${u.autoModels ? "checked" : ""}>
          暴露上游 /v1/models 拉取到的模型
        </label>
        ${isCC ? `
        <label class="check"><input type="checkbox" data-field="pauseTurn" ${u.pauseTurn !== false ? "checked" : ""}>pause_turn 自动续跑</label>
        <label class="check"><input type="checkbox" data-field="cmdZdr" ${u.cmdZdr !== false ? "checked" : ""}>ZDR 安全头 x-cmd-zdr: 1</label>` : ""}
        ${isOAI ? `
        <label class="check"><input type="checkbox" data-field="sessionHeader" ${u.sessionHeader !== false ? "checked" : ""}>注入 x-opencode-session 会话头</label>` : ""}
        <div class="span-2 hint">兼容能力（compat）：留空 = 跟随 provider 预设；这里只覆盖需要差异化的部分，保存时不会丢失。</div>
        <label>Provider 预设
          <select data-field="compatProfile">
            ${[["", "自动识别"]].concat(profileChoices())
              .map(([v, label]) => `<option value="${esc(v)}"${cProfile === v ? " selected" : ""}>${esc(label)}</option>`).join("")}
          </select>
        </label>
        <label>鉴权方式
          <select data-field="compatAuthScheme">
            ${[["", "跟随预设"], ["bearer", "Bearer"], ["x-api-key", "x-api-key"],
               ["both", "Bearer + x-api-key"], ["none", "不鉴权"], ["header", "自定义头"]]
              .map(([v, label]) => `<option value="${v}"${cAuth === v ? " selected" : ""}>${label}</option>`).join("")}
          </select>
        </label>
        <label>会话头名
          <input type="text" data-field="compatSessionHeader" value="${esc(cSession)}" placeholder="留空 = 跟随预设；填 - 表示不注入">
        </label>
        <label class="span-2">对话端点（拼在 Base URL 后）
          <input type="text" data-field="compatChatPath" value="${esc(cChat)}" placeholder="跟随预设，例如 /v1/chat/completions">
        </label>
        <label class="span-2">额外请求头（JSON 对象）
          <textarea data-field="compatHeaders" rows="2" placeholder='{"x-client-name":"model-gate"}'>${esc(cHeaders)}</textarea>
        </label>
        <label>最大输出 token
          <input type="text" data-field="compatMaxOutput" value="${esc(cMaxOut)}" placeholder="跟随预设">
        </label>
        <label>思考回传
          <select data-field="compatThinking">
            ${[["", "跟随预设"], ["on", "开启"], ["off", "关闭"]]
              .map(([v, label]) => `<option value="${v}"${cThink === v ? " selected" : ""}>${label}</option>`).join("")}
          </select>
        </label>
        <label>强制 tool_choice
          <select data-field="compatDowngrade">
            ${[["", "跟随预设"], ["on", "降级为 auto"], ["off", "原样下发"]]
              .map(([v, label]) => `<option value="${v}"${cDowngrade === v ? " selected" : ""}>${label}</option>`).join("")}
          </select>
        </label>
        <label>ZDR 安全头
          <select data-field="compatZdr">
            ${[["", "跟随预设"], ["on", "发送"], ["off", "不发送"]]
              .map(([v, label]) => `<option value="${v}"${cZdr === v ? " selected" : ""}>${label}</option>`).join("")}
          </select>
        </label>
        <label>版本头名
          <input type="text" data-field="compatVersionHeader" value="${esc(cVersion)}" placeholder="跟随预设；填 - 表示不发">
        </label>
        <label>环境头名
          <input type="text" data-field="compatEnvironmentHeader" value="${esc(cEnvironment)}" placeholder="跟随预设；填 - 表示不发">
        </label>
        <label class="span-2">模型级覆盖（JSON 对象，留空 = 无覆盖）
          <textarea data-field="modelOverrides" rows="3" placeholder='{"deepseek-reasoner":{"limits":{"maxOutputTokens":65536}}}'>${esc(cOverrides)}</textarea>
        </label>
        <div class="span-2 hint">键为模型 id，支持单个 * 通配；可设置 limits 与 capabilities，未知字段保存时原样保留。</div>
        <div class="span-2 hint">当前生效：${eff
          ? `${esc(eff.profile || "—")} · 鉴权 ${esc(eff.authScheme || "—")} · 会话 ${esc(eff.sessionHeader || "无")} · 端点 ${esc(eff.chatPath || "—")}${eff.maxOutputTokens ? ` · 输出上限 ${fmtInt(eff.maxOutputTokens)}` : ""}${eff.maxContextTokens ? ` · 上下文上限 ${fmtInt(eff.maxContextTokens)}` : ""}`
          : "保存后显示解析结果"}</div>
        <div class="span-2 hint">能力：${eff
          ? (capabilityBadges(eff.capabilities) || "—")
          : "保存后显示"}${eff ? ` · 模型覆盖 ${fmtInt(Object.keys(eff.modelOverrides || {}).length)} 条` : ""}</div>
      </div>
      </details>
      ${keyWarn}
    </div>`;
  }).join("") || '<div class="empty">还没有上游，点「＋ 新增上游」创建</div>';

  $$("[data-act]", host).forEach((btn) => {
    btn.addEventListener("click", async (ev) => {
      // summary 里的按钮：别让点击顺带把卡片折叠/展开
      if (btn.closest("summary")) ev.stopPropagation();
      const cardEl = btn.closest(".up-card");
      const key = cardEl.dataset.name;
      if (btn.dataset.act === "save-env") {
        const input = cardEl.querySelector('[data-field="apiKey"]');
        const secret = ((input && input.value) || "").trim();
        if (!secret) {
          toast("请先在上面填入新的 API Key", true);
          return;
        }
        saveUpstreamSecret(key, secret, input);
        return;
      }
      if (btn.dataset.act === "edit") {
        openUpstreamEditor(cardEl);
        return;
      }
      const ups = state.config.upstreams;
      const names = Object.keys(ups);
      const idx = names.indexOf(key);
      if (btn.dataset.act === "del") {
        const ok = await confirmModal({
          title: "删除上游",
          text: `将从配置里移除上游「${key}」；保存配置后才真正生效，已产生的用量明细不受影响。`,
          confirmLabel: "删除",
        });
        if (!ok) return;
        delete ups[key];
      } else if (btn.dataset.act === "up" && idx > 0) {
        const items = Object.entries(ups);
        [items[idx - 1], items[idx]] = [items[idx], items[idx - 1]];
        state.config.upstreams = Object.fromEntries(items);
      } else if (btn.dataset.act === "down" && idx < names.length - 1) {
        const items = Object.entries(ups);
        [items[idx + 1], items[idx]] = [items[idx], items[idx + 1]];
        state.config.upstreams = Object.fromEntries(items);
      } else {
        return;
      }
      renderUpstreamEditor();
    });
  });
  renderUpstreamCfgTable();
}

/* 上游编辑弹窗：克隆卡片字段到弹窗里改，点「应用到卡片」写回（读取路径不变） */
function applyUpstreamEdits(targetGrid, sourceGrid) {
  $$("[data-field]", sourceGrid).forEach((src) => {
    const dst = $$("[data-field]", targetGrid).find((el) => el.dataset.field === src.dataset.field);
    if (!dst) return;
    if (src.type === "checkbox") dst.checked = src.checked;
    else dst.value = src.value;
  });
}

function openUpstreamEditor(cardEl) {
  const name = cardEl.dataset.name || "";
  const grid = cardEl.querySelector(".grid");
  if (!grid) return;
  const clone = grid.cloneNode(true);
  // cloneNode 不带「当前输入值」，先按卡片现状同步一遍
  $$("[data-field]", grid).forEach((live) => {
    const twin = $$("[data-field]", clone).find((el) => el.dataset.field === live.dataset.field);
    if (!twin) return;
    if (live.type === "checkbox") twin.checked = live.checked;
    else twin.value = live.value;
  });
  const root = openModal({
    title: `编辑上游「${name}」`,
    hint: "这里只改「配置里这一份」；点「应用到卡片」后再用页面底部「保存配置」写入配置文件与数据库。",
    body: '<div class="modal-grid-host"></div>',
    actions: [
      { label: "取消" },
      {
        label: "应用到卡片",
        className: "primary",
        onClick: () => {
          applyUpstreamEdits(grid, clone);
          setConfigDirty(true);
          toast("已应用到卡片，记得点「保存配置」");
        },
      },
    ],
  });
  const host = root && root.querySelector(".modal-grid-host");
  if (host) host.appendChild(clone);
}

const _readUpstreamEditor = () => {
  const ups = {};
  $$("#upstream-editor .up-card").forEach((cardEl) => {
    const get = (f) => cardEl.querySelector(`[data-field="${f}"]`);
    let name = (get("name").value || "").trim() || cardEl.dataset.name;
    const old = (state.config.upstreams || {})[cardEl.dataset.name] || {};
    const apiKey = get("apiKey").value;
    // 保留所有未在表单里渲染的字段（cliVersion、modelCatalog、thinking…）：
    // 以前这里重建对象，保存一次就会静默丢配置。
    const entry = Object.assign({}, old);
    delete entry._keyConfigured;
    delete entry._keySource;
    entry.protocol = get("protocol").value;
    entry.baseUrl = get("baseUrl").value.trim();
    entry.models = parseModels(get("models").value);
    entry.default = get("default").checked;
    entry.autoModels = get("autoModels").checked;
    if (apiKey) entry.apiKey = apiKey;
    else if (old.apiKey !== undefined) entry.apiKey = old.apiKey;
    const env = get("apiKeyEnv").value.trim();
    if (env) entry.apiKeyEnv = env; else delete entry.apiKeyEnv;
    const agent = get("apiKeyAgent").value;
    if (agent) entry.apiKeyAgent = agent; else delete entry.apiKeyAgent;
    const agentProvider = get("apiKeyAgentProvider").value.trim();
    if (agent && agentProvider) entry.apiKeyAgentProvider = agentProvider;
    else delete entry.apiKeyAgentProvider;
    const agentPath = get("apiKeyAgentPath").value.trim();
    if (agent && agentPath) entry.apiKeyAgentPath = agentPath;
    else delete entry.apiKeyAgentPath;
    const ccBox = (f) => {
      const el = cardEl.querySelector(`[data-field="${f}"]`);
      return el ? el.checked : undefined;
    };
    const pt = ccBox("pauseTurn");
    if (pt !== undefined) entry.pauseTurn = pt;
    const zdr = ccBox("cmdZdr");
    if (zdr !== undefined) entry.cmdZdr = zdr;
    const sh = ccBox("sessionHeader");
    if (sh !== undefined) entry.sessionHeader = sh;

    // ── compat：provider 兼容能力（可留空的字段表示“跟随预设”） ──
    const oldCompat = Object.assign({}, old.compat || {});
    const compat = Object.assign({}, oldCompat);
    state.compatErrors = state.compatErrors || [];
    const text = (f) => {
      const el = get(f);
      return el ? el.value.trim() : null;
    };
    const select = (f) => {
      const el = get(f);
      return el ? el.value : null;
    };

    const profile = select("compatProfile");
    if (profile !== null) {
      if (!profile) delete compat.profile;
      else compat.profile = profile;
    }
    const scheme = select("compatAuthScheme");
    if (scheme !== null) {
      if (!scheme) delete compat.auth;
      else compat.auth = Object.assign({}, oldCompat.auth, { scheme });
    }
    const sessionHeader = text("compatSessionHeader");
    if (sessionHeader !== null) {
      if (!sessionHeader) {
        // 留空 = 跟随 provider 预设（不会误关 opencode/commandcode 的会话头）
        delete compat.session;
      } else if (sessionHeader === "-" || sessionHeader.toLowerCase() === "off") {
        compat.session = Object.assign({}, oldCompat.session, { enabled: false });
      } else {
        compat.session = Object.assign({}, oldCompat.session, { enabled: true, header: sessionHeader });
      }
    }
    const chatPath = text("compatChatPath");
    if (chatPath !== null) {
      const endpoint = Object.assign({}, oldCompat.endpoint);
      if (chatPath) endpoint.chat = chatPath; else delete endpoint.chat;
      if (Object.keys(endpoint).length) compat.endpoint = endpoint; else delete compat.endpoint;
    }
    const headersText = text("compatHeaders");
    if (headersText !== null) {
      if (!headersText) {
        delete compat.headers;
      } else {
        try {
          const parsed = JSON.parse(headersText);
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
            throw new Error("必须是 JSON 对象");
          }
          compat.headers = parsed;
        } catch (err) {
          state.compatErrors.push(
            `上游「${name}」的兼容请求头不是合法 JSON 对象：${err.message}`);
        }
      }
    }
    const maxOutRaw = text("compatMaxOutput");
    if (maxOutRaw !== null) {
      const limits = Object.assign({}, oldCompat.limits);
      if (!maxOutRaw) delete limits.maxOutputTokens;
      else {
        const parsed = parseCount(maxOutRaw);
        if (parsed) limits.maxOutputTokens = parsed;
        else state.compatErrors.push(
          `上游「${name}」的最大输出 token「${maxOutRaw}」无法识别`);
      }
      if (Object.keys(limits).length) compat.limits = limits; else delete compat.limits;
    }
    const thinking = select("compatThinking");
    if (thinking !== null) {
      if (thinking === "on") {
        compat.thinking = Object.assign({}, oldCompat.thinking, { passthrough: true });
      } else if (thinking === "off") {
        compat.thinking = Object.assign({}, oldCompat.thinking, { passthrough: false });
      } else {
        delete compat.thinking;
      }
    }
    const downgrade = select("compatDowngrade");
    if (downgrade !== null) {
      if (downgrade === "on") {
        compat.toolChoice = Object.assign({}, oldCompat.toolChoice, { downgradeForced: true });
      } else if (downgrade === "off") {
        compat.toolChoice = Object.assign({}, oldCompat.toolChoice, { downgradeForced: false });
      } else {
        delete compat.toolChoice;
      }
    }
    const zdrSelect = select("compatZdr");
    if (zdrSelect !== null) {
      if (zdrSelect === "on") compat.zdr = true;
      else if (zdrSelect === "off") compat.zdr = false;
      else delete compat.zdr;
    }
    const versionHeader = text("compatVersionHeader");
    if (versionHeader !== null) {
      if (versionHeader === "-") compat.versionHeader = "";
      else if (versionHeader) compat.versionHeader = versionHeader;
      else delete compat.versionHeader;
    }
    const environmentHeader = text("compatEnvironmentHeader");
    if (environmentHeader !== null) {
      if (environmentHeader === "-") compat.environmentHeader = "";
      else if (environmentHeader) compat.environmentHeader = environmentHeader;
      else delete compat.environmentHeader;
    }

    if (Object.keys(compat).length) entry.compat = compat; else delete entry.compat;

    // ── 模型级覆盖：JSON 编辑；非法结构阻止保存（未编辑时原样保留） ──
    const overridesText = text("modelOverrides");
    if (overridesText !== null) {
      if (!overridesText) {
        delete entry.modelOverrides;
      } else {
        try {
          const parsed = JSON.parse(overridesText);
          if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
            throw new Error("必须是 JSON 对象");
          }
          Object.entries(parsed).forEach(([key, value]) => {
            if (String(key).split("*").length > 2) {
              throw new Error(`键「${key}」包含多个 *，只支持一个通配符`);
            }
            if (!value || typeof value !== "object" || Array.isArray(value)
                || !Object.keys(value).length) {
              throw new Error(`「${key}」必须是非空对象（limits / capabilities）`);
            }
          });
          entry.modelOverrides = parsed;
        } catch (err) {
          state.compatErrors.push(
            `上游「${name}」的模型级覆盖不是合法配置：${err.message}`);
        }
      }
    }
    ups[name] = entry;
  });
  return ups;
};

async function loadConfig() {
  try {
    const data = await api("/api/config");
    state.config = data.config;
    state.effective = data.effective || [];
    state.profiles = data.profiles || [];
    const f = $("#config-form");
    f.host.value = state.config.host || "127.0.0.1";
    f.port.value = state.config.port ?? 8900;
    f.defaultModel.value = state.config.defaultModel || "";
    f.maxTokens.value = state.config.maxTokens ?? 8192;
    f.timeoutSeconds.value = state.config.timeoutSeconds ?? 600;
    f.modelsCacheSeconds.value = state.config.modelsCacheSeconds ?? 300;
    f.retentionDays.value = state.config.retentionDays ?? 0;
    f.models.value = (state.config.models || []).join("\n");
    f.aliases.value = Object.entries(state.config.aliases || {})
      .map(([k, v]) => `${k} = ${v}`).join("\n");
    f.accessLog.checked = state.config.accessLog !== false;
    f.thinkingPassthrough.checked =
      !state.config.thinking || state.config.thinking.passthrough !== false;
    renderUpstreamEditor();
    setConfigDirty(false);
    state.autoFixNotes = [];
    state.serverDiagnostics = data.diagnostics || [];
    renderConfigIssues(serverIssues(state.serverDiagnostics));
    renderServiceFacts();
    $("#config-hint").textContent = "";
    $("#config-hint").classList.remove("err");
  } catch (err) {
    toast(`配置加载失败：${err.message}`, true);
  }
}

function collectConfig() {
  const f = $("#config-form");
  const cfg = Object.assign({}, state.config);
  const notes = [];   // 本次采集里“替用户兜住”的处理；作为提醒回显，不写进配置本体
  cfg.host = f.host.value.trim() || "127.0.0.1";
  const num = (el, fallback, label, min) => {
    const raw = el && el.value;
    const floor = min === undefined ? 1 : min;
    const parsed = parseCount(raw);
    if (parsed === null || parsed < floor) {
      const rawText = String(raw === null || raw === undefined ? "" : raw).trim();
      if (rawText) notes.push(`${label}「${rawText}」无法识别或小于 ${floor}，已按 ${fallback} 处理`);
      return fallback;
    }
    return parsed;
  };
  cfg.port = num(f.port, 8900, "端口");
  cfg.defaultModel = f.defaultModel.value.trim();
  cfg.maxTokens = num(f.maxTokens, 8192, "最大输出 token");
  cfg.timeoutSeconds = num(f.timeoutSeconds, 600, "超时秒数");
  cfg.modelsCacheSeconds = num(f.modelsCacheSeconds, 300, "模型缓存秒数", 0);
  cfg.retentionDays = num(f.retentionDays, 0, "保留天数", 0);
  cfg.models = parseModels(f.models.value);
  cfg.aliases = parseAliases(f.aliases.value);
  cfg.accessLog = f.accessLog.checked;
  cfg.thinking = Object.assign({}, state.config.thinking, {
    passthrough: f.thinkingPassthrough.checked,
  });
  // 上游地址先规范化再落库：缺协议就补 https://，并向用户说明补成了什么
  state.compatErrors = [];
  const ups = _readUpstreamEditor();
  Object.keys(ups).forEach((name) => {
    const before = String(ups[name].baseUrl || "");
    const after = normalizeBaseUrl(before);
    if (after !== before) {
      ups[name].baseUrl = after;
      if (after) notes.push(`上游「${name}」的 Base URL 缺少协议，已补全为 ${after}`);
    }
  });
  cfg.upstreams = ups;
  state.autoFixNotes = notes;
  return cfg;
}

/* ── 配置页：脏值提示 + 字段级校验汇总 ─────────────────── */
/* 服务分区：当前生效值对照 + 面板地址 + 一键恢复默认 */
function serviceUrl() {
  const f = $("#config-form");
  const host = ((f && f.host.value) || "127.0.0.1").trim() || "127.0.0.1";
  const port = Number(f && f.port.value) || 8900;
  const shown = (host === "0.0.0.0" || host === "::") ? "127.0.0.1" : host;
  return `http://${shown}:${port}/ui/`;
}

/* 端口改动提示：保存不会重绑端口，必须重启才生效，所以要在表单里说清楚 */
function updateRestartHint() {
  const hint = $("#svc-restart-hint");
  if (!hint) return;
  const cfg = state.config || {};
  const f = $("#config-form");
  const formPort = Number(f && f.port.value) || 0;
  const livePort = Number(cfg.port) || 0;
  const changed = formPort && livePort && formPort !== livePort;
  hint.textContent = changed
    ? `端口将由 ${livePort} 改为 ${formPort}：保存后需重启进程才生效（当前面板仍监听 ${livePort}）`
    : "";
  hint.classList.toggle("warn-text", changed);
}

function renderServiceFacts() {
  const cfg = state.config || {};
  const set = (sel, value) => {
    const el = $(sel);
    if (el) el.textContent = value;
  };
  set("#svc-cur-port", cfg.port ?? "—");
  set("#svc-cur-model", cfg.defaultModel || "（未设置）");
  const maxEl = $("#svc-cur-max");
  if (maxEl) {
    // v2.17：胶囊显示「可读写法」，悬停给精确值——和输入框里的自填写法对得上
    const raw = cfg.maxTokens;
    maxEl.textContent = raw == null ? "—" : fmtInt(raw);
    const chip = $("#svc-max-chip");
    if (chip) chip.title = `当前默认上限 ${raw == null ? "—" : fmtInt(raw)}；点这里跳到输入框自填`;
  }
  set("#svc-cur-timeout", cfg.timeoutSeconds ?? "—");

  const urlEl = $("#svc-url");
  if (urlEl) urlEl.textContent = serviceUrl();

  const list = $("#svc-model-options");
  if (list) {
    const models = new Set(cfg.models || []);
    Object.values(cfg.upstreams || {}).forEach((up) => {
      (up.models || []).forEach((m) => models.add(m));
    });
    list.innerHTML = Array.from(models).sort()
      .map((m) => `<option value="${esc(m)}"></option>`).join("");
  }
  updateRestartHint();
}

/* 每个上游的输出上限来自 compat.limits.maxOutputTokens；不再写死某一家厂商。 */

function setConfigDirty(dirty) {
  state.configDirty = !!dirty;
  const bar = $("#config-dirty-bar");
  if (bar) bar.hidden = !state.configDirty;
  const text = $("#config-dirty-text");
  if (text) {
    text.textContent = state.configDirty
      ? "有未保存的改动 —— 保存后写入配置文件与数据库"
      : "有未保存的改动";
  }
  const btn = $("#save-config");
  if (btn) btn.disabled = !state.configDirty;
  if (!state.configDirty) {
    const hint = $("#config-hint");
    if (hint) hint.textContent = "";
  }
}

/* 校验：error 拦住保存，warn 只提示；每条带字段名，点击可定位 */
function validateConfig(cfg) {
  const issues = [];
  const add = (level, field, message, fix) => issues.push({ level, field, message, fix });

  // 采集阶段替用户兜住的输入（无法识别的数字、缺失协议的地址）先说明，
  // 这些不再拦保存——拦下来只会让人对着“有 1 处错误”发呆。
  (state.autoFixNotes || []).forEach((m) => add("warn", "", m));
  (state.compatErrors || []).forEach((m) => add("error", "", m));

  const port = Number(cfg.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    add("error", "port", `端口「${cfg.port}」超出范围：需 1–65535 的整数（例如 8900）`);
  }
  if (!cfg.host) add("error", "host", "监听地址不能为空");
  else if (!/^(127\.|localhost$|::1$)/.test(cfg.host)) {
    add("warn", "host", `监听地址 ${cfg.host} 不是本机回环：局域网内可访问，请确认是有意为之`);
  }
  const maxTok = Number(cfg.maxTokens);
  if (!isFinite(maxTok) || maxTok < 1) {
    add("warn", "maxTokens", `最大输出 token「${cfg.maxTokens}」无效，已按 8192 处理（支持 30万 / 3,000,000 / 384K 这种写法）`);
  }
  if (Number(cfg.timeoutSeconds) < 1) {
    add("warn", "timeoutSeconds", `超时「${cfg.timeoutSeconds}」无效，已按 600 秒处理`);
  }
  if (Number(cfg.modelsCacheSeconds) < 0) add("error", "modelsCacheSeconds", "缓存秒数不能为负");
  if (Number(cfg.retentionDays) < 0) add("error", "retentionDays", "保留天数不能为负");

  const ups = cfg.upstreams || {};
  const names = Object.keys(ups);
  if (!names.length) {
    add("error", "upstreams", "至少要配置一个上游：点右侧「＋ 新增上游」即可", "add-upstream");
  }
  names.forEach((name, i) => {
    const up = ups[name] || {};
    const where = `上游「${name || `#${i + 1}`}」`;
    if (!String(name).trim()) add("error", `upstream:${i}`, `第 ${i + 1} 个上游缺少名称`, "focus-upstream");
    const base = String(up.baseUrl || "").trim();
    if (!base) {
      add("error", `upstream:${i}`,
        `${where} 没有填 Base URL：请填入上游地址（例如 https://api.deepseek.com/v1）`, "focus-upstream");
    } else if (!/^https?:\/\//i.test(base)) {
      add("error", `upstream:${i}`,
        `${where} 的 Base URL「${base}」需以 http:// 或 https:// 开头`, "fix-baseurl");
    }
    // 只用警告，不阻断保存：上游可以只靠前缀路由 /<name>/v1/* 使用（proxy.py 的 forced 分支
    // 直接按上游名命中，与模型清单无关），真实配置里 zen 就是这种形态。
    if (!up.autoModels && !(up.models || []).length) {
      add("warn", `upstream:${i}`,
        `${where} 没有模型清单：仍可用前缀路由 /${name}/v1/*（其 /models 会返回空），但不参与按模型名的自动路由`);
    }
    const maxOut = up.compat && up.compat.limits && Number(up.compat.limits.maxOutputTokens);
    if (maxOut && maxTok > maxOut) {
      add("warn", `upstream:${i}`,
        `${where} 的最大输出上限为 ${fmtInt(maxOut)}：全局 maxTokens 会被运行时收敛`);
    }
  });

  const defaultModel = String(cfg.defaultModel || "").trim();
  if (defaultModel) {
    const known = new Set(cfg.models || []);
    names.forEach((n) => ((ups[n] || {}).models || []).forEach((m) => known.add(m)));
    if (known.size && !known.has(defaultModel)) {
      add("warn", "defaultModel", `默认模型 ${defaultModel} 不在任何上游的清单里，运行时可能回退`);
    }
  } else {
    add("warn", "defaultModel", "未设置默认模型：客户端不指定模型时会回退到第一个可用上游");
  }
  return issues;
}

/* 服务端结构化诊断 → 与前端校验同一种展示格式（path 拼进正文，便于定位） */
function serverIssues(diagnostics) {
  return (diagnostics || []).map((d) => ({
    level: d.level === "error" ? "error" : "warn",
    field: "",
    message: `${d.path ? `${d.path}：` : ""}${d.message}${d.hint ? `（${d.hint}）` : ""}`,
  }));
}

function renderConfigIssues(issues) {
  const box = $("#config-issues");
  if (!box) return;
  if (!issues.length) {
    box.hidden = true;
    box.innerHTML = "";
    return;
  }
  box.hidden = false;
  box.innerHTML = issues.map((it) => {
    const tag = it.level === "error" ? "错误" : "提醒";
    const body = `<span class="issue-tag">${tag}</span>${esc(it.message)}`;
    // 没有字段可定位的（例如“已替你补全”这类说明）用纯文本，避免点了没反应
    if (!it.field) return `<div class="issue ${it.level} static">${body}</div>`;
    return `<button type="button" class="issue ${it.level}" data-focus="${esc(it.field)}"`
      + (it.fix ? ` data-fix="${esc(it.fix)}"` : "")
      + `>${body}</button>`;
  }).join("");
  $$("button.issue", box).forEach((btn) => btn.addEventListener("click", () => {
    if (btn.dataset.fix === "add-upstream") {
      const addBtn = $("#add-upstream");
      if (addBtn) addBtn.click();
      return;
    }
    const field = btn.dataset.focus || "";
    const form = $("#config-form");
    const byName = form && form.elements ? form.elements[field] : null;
    if (byName && byName.focus) {
      byName.focus();
      if (byName.scrollIntoView) byName.scrollIntoView({ block: "center" });
      return;
    }
    const m = field.match(/^upstream:(\d+)$/);
    if (!m) return;
    const card = $$("#upstream-editor .up-card")[Number(m[1])];
    if (!card) return;
    if (btn.dataset.fix === "fix-baseurl") {
      const input = card.querySelector('[data-field="baseUrl"]');
      if (input) {
        const fixed = normalizeBaseUrl(input.value);
        if (fixed && fixed !== input.value) {
          input.value = fixed;
          setConfigDirty(true);
          toast(`已补全为 ${fixed}，再点「保存配置」即可`);
        }
      }
    }
    // 优先把光标放到真正有问题的那一格，而不是卡片第一格
    const emptyish = card.querySelector('[data-field="baseUrl"]');
    const first = (emptyish && !String(emptyish.value || "").trim()) ? emptyish
      : card.querySelector("input, textarea, select");
    if (first) first.focus();
    card.scrollIntoView({ block: "center", behavior: "smooth" });
  }));
}

async function saveConfig(ev) {
  ev.preventDefault();
  const hint = $("#config-hint");
  const cfg = collectConfig();
  const issues = validateConfig(cfg);
  renderConfigIssues(issues);
  const errors = issues.filter((i) => i.level === "error");
  if (errors.length) {
    // 点名第一条：只说“有 1 处错误”等于没说，用户不知道该改哪儿
    hint.textContent = `有 ${errors.length} 处需要先修正：${errors[0].message}`;
    hint.classList.add("err");
    toast(`配置有 ${errors.length} 处错误，未提交`, true);
    if (window.console && console.warn) console.warn("[modelgate] 配置校验未通过", issues);
    return;
  }
  try {
    const data = await postJson("/api/config", { config: cfg });
    state.config = cfg;
    setConfigDirty(false);
    const diags = (data && data.diagnostics) || [];
    renderConfigIssues(serverIssues(diags));
    hint.textContent = diags.length
      ? `已保存（配置文件与数据库双写）；有 ${diags.length} 条诊断提醒`
      : "已保存（配置文件与数据库双写）";
    hint.classList.remove("err");
    toast("配置已保存");
    setTimeout(() => loadConfig(), 400);
  } catch (err) {
    hint.textContent = err.message;
    hint.classList.add("err");
    toast(`保存失败：${err.message}`, true);
  }
}

function initConfigActions() {
  const add = $("#add-upstream");
  if (add) add.addEventListener("click", (ev) => {
    // 按钮在 summary 里：阻止冒泡，否则点「新增」会把分组折叠起来
    ev.stopPropagation();
    ev.preventDefault();
    addUpstream();
  });
  const form = $("#config-form");
  if (form) {
    form.addEventListener("submit", saveConfig);
    // 任何输入都标记「已改动」；保存或重新载入后由 setConfigDirty(false) 清掉
    form.addEventListener("input", () => setConfigDirty(true));
    form.addEventListener("change", () => setConfigDirty(true));
  }
  const discard = $("#config-discard");
  if (discard) discard.addEventListener("click", async () => {
    const ok = await confirmModal({
      title: "放弃未保存的改动",
      text: "将丢弃当前表单里的改动，并重新载入正在生效的配置。",
      confirmLabel: "放弃改动",
    });
    if (!ok) return;
    renderConfigIssues([]);
    await loadConfig();
    toast("已放弃未保存的改动");
  });
  // v2.17：这三颗按钮现在住在折叠标题行里，点它们不能顺带把卡片折叠/展开
  const inSummary = (ev) => ev.stopPropagation();
  const copyUrl = $("#svc-copy-url");
  if (copyUrl) copyUrl.addEventListener("click", (ev) => {
    inSummary(ev);
    copyText(serviceUrl());
  });
  const openUrl = $("#svc-open-url");
  if (openUrl) openUrl.addEventListener("click", (ev) => {
    inSummary(ev);
    window.open(serviceUrl(), "_blank", "noopener");
  });
  const maxChip = $("#svc-max-chip");
  if (maxChip) maxChip.addEventListener("click", (ev) => {
    inSummary(ev);
    const input = $("#config-form") && $("#config-form").maxTokens;
    if (!input) return;
    input.focus();
    input.select();
  });
  const reset = $("#svc-reset");
  if (reset) reset.addEventListener("click", (ev) => {
    inSummary(ev);
    const f = $("#config-form");
    if (!f) return;
    f.host.value = "127.0.0.1";
    f.port.value = 8900;
    f.maxTokens.value = 8192;
    f.timeoutSeconds.value = 600;
    f.modelsCacheSeconds.value = 300;
    f.retentionDays.value = 0;
    setConfigDirty(true);
    renderServiceFacts();
    toast("已填入默认值，保存后生效");
  });
  const portInput = form && form.port;
  if (portInput) portInput.addEventListener("input", updateRestartHint);

  const reload = $("#reload-config");
  if (reload) reload.addEventListener("click", async () => {
    try {
      await postJson("/api/config/reload");
      await loadConfig();
      toast("已重新载入配置");
    } catch (err) {
      toast(`重载失败：${err.message}`, true);
    }
  });
  // 有未保存改动时离开页面给一次提醒（不做二次确认弹窗，避免打断）
  window.addEventListener("beforeunload", (ev) => {
    if (!state.configDirty) return;
    ev.preventDefault();
    ev.returnValue = "";
  });
}

/* ── 用量 ──────────────────────────────────────────────── */
function initUsageControls() {
  const bind = (sel, key) => {
    const root = $(sel);
    if (!root) return;
    root.addEventListener("click", (ev) => {
      const btn = ev.target.closest("button[data-" + key + "]");
      if (!btn) return;
      $$("button[data-" + key + "]", root).forEach((b) => b.classList.toggle("active", b === btn));
      if (key === "range") {
        state.range = btn.dataset.range;
        state.custom = { from: "", to: "" };
        const from = $("#scope-from");
        const to = $("#scope-to");
        if (from) from.value = "";
        if (to) to.value = "";
        state.page = 0;
      }
      if (key === "group") { state.group = btn.dataset.group; }
      if (key === "metric") { state.trendMetric = btn.dataset.metric; renderTrend(); }
      if (key === "pie") { state.pieKind = btn.dataset.pie; renderPie(); }
      if (key === "range" || key === "group") loadUsage();
    });
  };
  bind("#range-seg", "range");
  bind("#group-seg", "group");
  bind("#trend-metric-seg", "metric");
  bind("#pie-seg", "pie");

  const refresh = $("#refresh-usage");
  if (refresh) refresh.addEventListener("click", () => loadUsage());

  const clearBtn = $("#clear-usage");
  if (clearBtn) clearBtn.addEventListener("click", async () => {
    const custom = !!(state.custom.from || state.custom.to);
    const desc = custom
      ? `自定义区间 ${state.custom.from || "最早"} ~ ${state.custom.to || "现在"}`
      : `预设区间 ${state.range}`;
    const ok = await confirmModal({
      title: "清空用量明细",
      text: `将删除${desc}内的全部用量明细（含所有上游与模型），此操作不可撤销；代理转发不受影响。`,
      confirmLabel: "清空",
    });
    if (!ok) return;
    try {
      const body = custom
        ? { from: state.custom.from, to: state.custom.to }
        : { range: state.range };
      const data = await postJson("/api/usage/clear", body);
      toast(`已清空 ${data.removed} 条`);
      state.page = 0;
      loadUsage();
    } catch (err) {
      toast(`清空失败：${err.message}`, true);
    }
  });

  const scopeUpstream = $("#scope-upstream");
  if (scopeUpstream) scopeUpstream.addEventListener("change", () => {
    state.filters.upstream = scopeUpstream.value;
    state.filters.model = "";  // 上游变了，原模型未必属于该上游
    state.page = 0;
    loadUsage();
  });

  const scopeModel = $("#scope-model");
  if (scopeModel) scopeModel.addEventListener("change", () => {
    state.filters.model = scopeModel.value;
    state.page = 0;
    loadUsage();
  });

  [["#scope-from", "from"], ["#scope-to", "to"]].forEach(([sel, key]) => {
    const el = $(sel);
    if (!el) return;
    el.addEventListener("change", () => {
      state.custom[key] = el.value || "";
      if (state.custom.from && state.custom.to && state.custom.from >= state.custom.to) {
        toast("起始时间必须早于结束时间", true);
        state.custom[key] = "";
        el.value = "";
        return;
      }
      state.page = 0;
      // 自定义时间优先于预设：取消预设按钮的 active 态
      if (state.custom.from || state.custom.to) {
        $$("#range-seg button").forEach((b) => b.classList.remove("active"));
      }
      loadUsage();
    });
  });

  const clearScope = $("#scope-clear");
  if (clearScope) clearScope.addEventListener("click", () => {
    state.filters = { upstream: "", model: "", session: "" };
    state.custom = { from: "", to: "" };
    const from = $("#scope-from");
    const to = $("#scope-to");
    if (from) from.value = "";
    if (to) to.value = "";
    $$("#range-seg button").forEach((b) => {
      b.classList.toggle("active", b.dataset.range === state.range);
    });
    state.page = 0;
    loadUsage();
  });

  const sessionFilter = $("#filter-session");
  if (sessionFilter) sessionFilter.addEventListener("change", () => {
    state.filters.session = sessionFilter.value;
    state.page = 0;
    loadUsage();
  });

  const exportCsv = $("#export-usage-csv");
  if (exportCsv) exportCsv.addEventListener("click", () => exportUsage("csv"));
  const exportJson = $("#export-usage-json");
  if (exportJson) exportJson.addEventListener("click", () => exportUsage("json"));

  const applyBtn = $("#apply-retention");
  if (applyBtn) applyBtn.addEventListener("click", applyRetention);

  const prev = $("#prev-page");
  if (prev) prev.addEventListener("click", () => {
    if (state.page > 0) { state.page -= 1; loadUsageList(); }
  });
  const next = $("#next-page");
  if (next) next.addEventListener("click", () => {
    state.page += 1; loadUsageList();
  });

  // 首/末页 + 跳页：大区间下翻页从「一次次点」变成一次定位
  const first = $("#first-page");
  if (first) first.addEventListener("click", () => {
    if (state.page > 0) { state.page = 0; loadUsageList(); }
  });
  const pageCount = () => Math.max(1, Math.ceil(state.listTotal / state.pageSize));
  const last = $("#last-page");
  if (last) last.addEventListener("click", () => {
    const target = pageCount() - 1;
    if (state.page !== target) { state.page = target; loadUsageList(); }
  });
  const jump = $("#page-jump");
  if (jump) jump.addEventListener("change", () => {
    const target = Math.min(pageCount(), Math.max(1, Number(jump.value) || 1)) - 1;
    if (target !== state.page) { state.page = target; loadUsageList(); }
    else jump.value = String(state.page + 1);
  });

  // 每页条数：口径可调，不再受固定 50 条限制
  const size = $("#page-size");
  if (size) size.addEventListener("change", () => {
    state.pageSize = Math.max(10, Math.min(200, Number(size.value) || 50));
    state.page = 0;
    loadUsageList();
  });

  // 快捷筛选 chips：只看异常，一到后端就是白名单里的 SQL 片段
  const chips = $("#only-chips");
  if (chips) chips.addEventListener("click", (ev) => {
    const btn = ev.target.closest("button[data-only]");
    if (!btn) return;
    $$("button[data-only]", chips).forEach((b) => b.classList.toggle("active", b === btn));
    state.usage.only = btn.dataset.only || "";
    state.page = 0;
    loadUsageList();
  });

  // 明细排序：点表头切换列与方向（▲▼ 由 CSS 呈现）
  const table = $("#usage-table");
  if (table) {
    $$("th[data-sort]", table).forEach((th) => {
      th.addEventListener("click", () => {
        const key = th.dataset.sort;
        if (state.usage.listSort === key) {
          state.usage.listOrder = state.usage.listOrder === "desc" ? "asc" : "desc";
        } else {
          state.usage.listSort = key;
          state.usage.listOrder = "desc";
        }
        state.page = 0;
        loadUsageList();
      });
    });
    // 行展开：错误原因 / 上游模型 / 完整会话 id 收进详情，表格保持紧凑
    const tbody = $("tbody", table);
    if (tbody) tbody.addEventListener("click", (ev) => {
      const copyBtn = ev.target.closest("[data-copy]");
      if (copyBtn) { copyText(copyBtn.dataset.copy || ""); return; }
      const tr = ev.target.closest("tr[data-id]");
      if (!tr) return;
      const id = tr.dataset.id;
      state.usage.expanded = state.usage.expanded === id ? "" : id;
      renderUsageTable(state.usage.lastRows || []);
    });
  }

  const upstreamTable = $("#upstream-usage-table");
  if (upstreamTable) upstreamTable.addEventListener("click", (ev) => {
    const only = ev.target.closest("[data-only-upstream]");
    if (only) {
      state.filters.upstream = only.dataset.onlyUpstream || "";
      state.filters.model = "";
      state.page = 0;
      loadUsage();
      return;
    }
    const th = ev.target.closest("th.sortable");
    if (th) {
      const key = th.dataset.key;
      if (state.usage.upstreamSortKey === key) {
        state.usage.upstreamSortDir = state.usage.upstreamSortDir === "desc" ? "asc" : "desc";
      } else {
        state.usage.upstreamSortKey = key;
        state.usage.upstreamSortDir = key === "upstream" ? "asc" : "desc";
      }
      renderUpstreamUsage();
      return;
    }
    const tr = ev.target.closest("tr[data-upstream]");
    if (!tr) return;
    const name = tr.dataset.upstream;
    state.usage.upstreamExpanded = state.usage.upstreamExpanded === name ? "" : name;
    renderUpstreamUsage();
  });

  const auto = $("#usage-auto");
  if (auto) {
    auto.addEventListener("change", () => setAutoRefresh(Number(auto.value) || 0));
    setAutoRefresh(Number(auto.value) || 0);
  }
}

/* 页面级作用域 → 查询串：预设/自定义时间 + 上游 + 模型 + 会话。
 * KPI、趋势、分布、上游表、模型表、明细、导出全部走这里，避免同一屏出现两套口径。 */
function usageScopeQuery(extra = {}) {
  const q = new URLSearchParams();
  if (state.custom.from || state.custom.to) {
    if (state.custom.from) q.set("from", state.custom.from);
    if (state.custom.to) q.set("to", state.custom.to);
  } else {
    q.set("range", state.range);
  }
  ["upstream", "model", "session"].forEach((key) => {
    if (state.filters[key]) q.set(key, state.filters[key]);
  });
  Object.entries(extra).forEach(([key, value]) => {
    if (value !== undefined && value !== null && value !== "") q.set(key, value);
  });
  return q;
}

function renderFilterOptions() {
  const optionsHtml = (label, values, current) =>
    `<option value="">全部${label}</option>` +
    values.map((v) => `<option value="${esc(v)}"${v === current ? " selected" : ""}>${esc(v)}</option>`).join("");

  const upstream = $("#scope-upstream");
  if (upstream) {
    const values = state.options.upstreams || [];
    if (state.filters.upstream && !values.includes(state.filters.upstream)) state.filters.upstream = "";
    upstream.innerHTML = optionsHtml("上游", values, state.filters.upstream);
    upstream.value = state.filters.upstream;
  }

  const model = $("#scope-model");
  if (model) {
    const values = state.options.models || [];
    if (state.filters.model && !values.includes(state.filters.model)) state.filters.model = "";
    model.innerHTML = optionsHtml("模型", values, state.filters.model);
    model.value = state.filters.model;
  }

  const session = $("#filter-session");
  if (session) {
    const values = state.options.sessions || [];
    if (state.filters.session && !values.includes(state.filters.session)) state.filters.session = "";
    session.innerHTML = optionsHtml("会话", values, state.filters.session);
    session.value = state.filters.session;
  }
}

function exportUsage(format) {
  const q = usageScopeQuery({ format });
  const link = document.createElement("a");
  link.href = `/api/usage/export?${q.toString()}`;
  link.download = "";
  document.body.appendChild(link);
  link.click();
  link.remove();
  toast(`已开始导出 ${format.toUpperCase()}（当前筛选与区间）`);
}

async function applyRetention() {
  const days = Number(($("#retention-days") || {}).value || 0);
  if (days > 0) {
    const ok = await confirmModal({
      title: "清理超期明细",
      text: `将删除 ${days} 天前的用量明细，此操作不可撤销；代理转发不受影响。`,
      confirmLabel: "清理",
    });
    if (!ok) return;
  }
  try {
    const data = await postJson("/api/usage/retention/apply", { days });
    toast(data.removed ? `已清理 ${data.removed} 条超期明细` : "没有需要清理的超期明细");
    loadUsage();
  } catch (err) {
    toast(`清理失败：${err.message}`, true);
  }
}

async function loadUsage() {
  if (!state.pricingLoaded) await loadPricing();
  try {
    const data = await api(`/api/usage/totals?${usageScopeQuery({ compare: 1 })}`);
    state.usage.previous = data.previous || null;
    renderKpis(data.totals || {});
  } catch (err) {
    $("#usage-kpis").innerHTML = `<div class="empty">${esc(err.message)}</div>`;
  }
  await Promise.all([loadSummary("day"), loadSummary(state.group),
                     loadUsageList(), loadModelUsage(), loadUpstreamUsage()]);
  markFresh();
}

/* 数据新鲜度：告诉用户「这份数字是什么时候的」——自动刷新时尤其重要 */
function markFresh() {
  const at = new Date();
  state.usage.lastUpdated = at;
  const el = $("#usage-fresh");
  if (!el) return;
  const pad = (n) => String(n).padStart(2, "0");
  el.textContent = `更新于 ${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`;
}

/* 自动刷新：只重拉统计，不触碰代理；页面隐藏或不在用量页时跳过 */
function setAutoRefresh(seconds) {
  if (state.usage.autoTimer) {
    clearInterval(state.usage.autoTimer);
    state.usage.autoTimer = null;
  }
  if (!(seconds > 0)) return;
  state.usage.autoTimer = setInterval(() => {
    if (document.hidden) return;
    const panel = $("#panel-usage");
    if (panel && !panel.classList.contains("active")) return;
    loadUsage();
  }, seconds * 1000);
}

/* KPI 卡片：数值 + 说明 + 环比标记（value/delta 为内部 HTML，note 走转义） */
function kpiCard(label, value, note, tone, delta) {
  return `<div class="kpi${tone ? " " + tone : ""}"><div class="k">${esc(label)}</div>` +
    `<div class="v">${value}${delta || ""}</div>` +
    `${note ? `<small>${esc(note)}</small>` : ""}</div>`;
}

/* 环比：与「上一等长区间」比较；缺基线时不显示，避免拿 0 当基准误导 */
function deltaTag(current, previous, lowerIsBetter) {
  const cur = Number(current) || 0;
  const prev = Number(previous);
  if (!isFinite(prev) || prev <= 0) return "";
  const diff = (cur - prev) / prev;
  if (!isFinite(diff) || Math.abs(diff) < 0.005) return '<span class="delta flat">持平</span>';
  const better = lowerIsBetter ? diff < 0 : diff > 0;
  const pct = (Math.abs(diff) * 100).toFixed(Math.abs(diff) < 0.1 ? 1 : 0);
  return `<span class="delta ${better ? "good" : "bad"}">${diff > 0 ? "↑" : "↓"}${pct}%</span>`;
}

function renderKpis(t) {
  state.usage.totals = t;
  const prev = state.usage.previous;
  const requests = Number(t.requests) || 0;
  const errRate = requests ? Math.round((t.errors / requests) * 1000) / 10 : 0;
  const rate = Number(t.success_rate);
  const rateText = requests ? (rate * 100).toFixed(1).replace(/\.0$/, "") + "%" : "—";
  const ttftText = Number(t.ttft_samples) ? fmtMs(t.avg_ttft_ms) : "—";
  const totalTokens = Number(t.total_tokens) || 0;
  const priced = !!(state.pricing && state.pricing.configured);
  const perMillion = priced && totalTokens
    ? fmtCost((Number(t.cost) || 0) / totalTokens * 1e6, state.pricing) : "—";
  const cacheShare = totalTokens && t.cache_read_tokens
    ? `${Math.round((t.cache_read_tokens / totalTokens) * 100)}% 占比` : "";

  const groups = [
    ["核心", [
      kpiCard("缓存命中率", fmtRate(t.cache_read_tokens, t.prompt_tokens),
        `命中 ${fmtInt(t.cache_read_tokens)} / 输入 ${fmtInt(t.prompt_tokens)}`,
        "ok"),
      kpiCard("缓存命中", fmtTok(t.cache_read_tokens), cacheShare, "data"),
      kpiCard("总 token", fmtTok(t.total_tokens), "", "data"),
    ]],
    ["吞吐", [
      kpiCard("请求数", fmtInt(t.requests), "区间内合计", "accent",
        prev ? deltaTag(requests, prev.requests, false) : ""),
      kpiCard("成功率", rateText, requests ? `${fmtInt(t.ok)} 成功` : "该区间无请求",
        requests && Number(t.errors) ? "warn" : "ok"),
      kpiCard("错误", fmtInt(t.errors), errRate ? `${errRate}% 的请求` : "无错误",
        t.errors ? "danger" : "ok",
        prev ? deltaTag(t.errors, prev.errors, true) : ""),
    ]],
    ["Token", [
      kpiCard("输入", fmtTok(t.prompt_tokens), "", "data",
        prev ? deltaTag(t.prompt_tokens, prev.prompt_tokens, true) : ""),
      kpiCard("输出", fmtTok(t.completion_tokens), "", "data"),
    ]],
    ["性能", [
      kpiCard("平均耗时", fmtMs(t.avg_duration_ms), "", "",
        prev ? deltaTag(t.avg_duration_ms, prev.avg_duration_ms, true) : ""),
      kpiCard("P95 耗时", fmtMs(t.p95_duration_ms),
        Number(t.p50_duration_ms) ? `P50 ${fmtMs(t.p50_duration_ms)}` : "", "data"),
      kpiCard("最长", fmtMs(t.max_duration_ms), "", ""),
      kpiCard("首 token", ttftText,
        Number(t.ttft_samples) ? `${fmtInt(t.ttft_samples)} 次采样` : "仅流式可测", "data"),
    ]],
    ["费用", [
      kpiCard("费用估算", fmtCost(t.cost, state.pricing), costExtra(state.pricing),
        priced ? "accent" : "warn"),
      kpiCard("单位成本", perMillion, priced ? "每百万 token" : "未配置单价", "data"),
    ]],
  ];

  // 分组呈现：11 个数字排成一长条很难读，按「吞吐 / Token / 性能 / 费用」成簇
  $("#usage-kpis").innerHTML = groups.map(([title, cards]) =>
    '<div class="kpi-group">' +
    `<div class="kpi-group-head"><span>${esc(title)}</span></div>` +
    `<div class="kpi-row">${cards.join("")}</div></div>`).join("");
  renderTokenMix(t);
  renderLatency(t);
}

/* Token 构成条：输入 / 输出 / 缓存命中 的比例（缓存单列，因为它最省钱） */
function renderTokenMix(t) {
  const box = $("#token-mix");
  if (!box) return;
  const segs = Array.from(box.querySelectorAll("i"));
  const parts = [
    Number(t.prompt_tokens) || 0,
    Number(t.completion_tokens) || 0,
    Number(t.cache_read_tokens) || 0,
  ];
  const total = parts.reduce((a, b) => a + b, 0);
  segs.forEach((el, i) => {
    el.style.width = total ? `${(parts[i] / total) * 100}%` : "0%";
  });
  const set = (sel, value) => {
    const el = $(sel);
    if (!el) return;
    const pct = total ? Math.round((value / total) * 1000) / 10 : 0;
    el.innerHTML = `<b title="${esc(fmtInt(value))} token">${esc(fmtCn(value))}</b> · ${pct}%`;
  };
  set("#mix-in", parts[0]);
  set("#mix-out", parts[1]);
  set("#mix-cache", parts[2]);
  const hitEl = $("#mix-cache-rate");
  if (hitEl) {
    hitEl.textContent = fmtRate(t.cache_read_tokens, t.prompt_tokens);
    hitEl.title = "缓存命中率 = 命中输入 / 总输入（prompt_tokens 含缓存部分）";
  }
  const hint = $("#mix-hint");
  if (hint) hint.textContent = total ? `合计 ${fmtCompact(total)} token` : "该区间暂无数据";
}

/* 耗时分布：均值 / P50 / P95 / 最长 —— 只给均值会掩盖长尾 */
function renderLatency(t) {
  const box = $("#latency-rows");
  if (!box) return;
  const rows = [
    ["平均", t.avg_duration_ms, ""],
    ["P50", t.p50_duration_ms, "一半请求快于此值"],
    ["P95", t.p95_duration_ms, "长尾：5% 请求慢于此值"],
    ["最长", t.max_duration_ms, ""],
  ];
  const max = Math.max(...rows.map((r) => Number(r[1]) || 0), 1);
  box.innerHTML = rows.map(([label, value, note]) => {
    const pct = Math.max(1, Math.round((Number(value || 0) / max) * 100));
    return `<div class="legend-row">
      <div class="legend-head">
        <span class="label">${label}</span>
        <span class="val"><b>${Number(value) ? fmtMs(value) : "—"}</b>${
          note ? ` <span class="hint">${note}</span>` : ""}</span>
      </div>
      <div class="bar-seg" style="margin:0;height:7px">
        <i class="${label === "P95" || label === "最长" ? "out" : "cache"}"
           style="width:${pct}%"></i>
      </div>
    </div>`;
  }).join("");
}

async function loadSummary(group) {
  try {
    const data = await api(`/api/usage/summary?group_by=${group}&${usageScopeQuery()}`);
    window.__summary = window.__summary || {};
    window.__summary[group] = data.rows || [];
    if (group === state.group) renderTrend();
    renderPie();
  } catch (err) {
    if (group === state.group) {
      $("#usage-trend").innerHTML = `<div class="empty">${esc(err.message)}</div>`;
    }
  }
}

/* 趋势图：SVG 柱状 + Y 轴刻度 + 均值线；每根柱子自带原生 tooltip */
const TREND_VIEW = { w: 720, h: 240, left: 66, right: 14, top: 16, bottom: 30 };

function trendMetricInfo(metric) {
  if (metric === "requests") {
    return { field: "requests", unit: " 次", fmt: fmtInt, color: "var(--brand)" };
  }
  if (metric === "errors") {
    return { field: "errors", unit: " 次", fmt: fmtInt, color: "var(--danger)" };
  }
  return { field: "total_tokens", unit: " token", fmt: fmtCn, color: "var(--in)" };
}

function renderTrend() {
  const box = $("#usage-trend");
  if (!box) return;
  const rows = (window.__summary || {})[state.group] || [];
  if (!rows.length) {
    box.innerHTML = '<div class="empty">该区间暂无数据</div>';
    return;
  }
  const info = trendMetricInfo(state.trendMetric);
  const vals = rows.map((r) => Number(r[info.field]) || 0);
  const max = Math.max(...vals, 1);
  const avg = vals.reduce((a, b) => a + b, 0) / vals.length;

  const { w, h, left, right, top, bottom } = TREND_VIEW;
  const pw = w - left - right;
  const ph = h - top - bottom;
  const yOf = (v) => top + ph - (v / max) * ph;
  const step = pw / rows.length;
  const barW = Math.max(3, Math.min(38, step * 0.62));

  // Y 轴：4 段刻度 + 网格线（值用与柱子同一套格式化，避免"刻度看不懂"）
  let grid = "";
  for (let i = 0; i <= 4; i++) {
    const gy = yOf((max / 4) * i);
    grid += `<line x1="${left}" y1="${gy.toFixed(1)}" x2="${w - right}" y2="${gy.toFixed(1)}" class="grid-line"/>`
      + `<text x="${left - 8}" y="${(gy + 4).toFixed(1)}" class="axis-label" text-anchor="end">${
        esc(info.fmt((max / 4) * i))}</text>`;
  }

  const labelEvery = Math.max(1, Math.ceil(rows.length / 8));
  let bars = "";
  rows.forEach((r, i) => {
    const v = vals[i];
    const bx = left + step * i + (step - barW) / 2;
    const by = yOf(v);
    const bh = Math.max(v > 0 ? 2 : 0, top + ph - by);
    const tip = `${fmtBucket(r.bucket, state.group)} · ${info.fmt(v)}${info.unit}`;
    bars += `<g class="bar"><title>${esc(tip)}</title>`
      + `<rect x="${bx.toFixed(1)}" y="${by.toFixed(1)}" width="${barW.toFixed(1)}" `
      + `height="${bh.toFixed(1)}" rx="2" fill="${info.color}"/>`
      + (rows.length <= 14 && bh > 20
        ? `<text x="${(bx + barW / 2).toFixed(1)}" y="${(by - 5).toFixed(1)}" class="bar-value" `
          + `text-anchor="middle">${esc(info.fmt(v))}</text>`
        : "")
      + "</g>";
    if (i % labelEvery === 0) {
      bars += `<text x="${(bx + barW / 2).toFixed(1)}" y="${h - 10}" class="axis-label" `
        + `text-anchor="middle">${esc(fmtBucket(r.bucket, state.group))}</text>`;
    }
  });

  const avgY = yOf(avg);
  const avgLine = `<line x1="${left}" y1="${avgY.toFixed(1)}" x2="${w - right}" y2="${avgY.toFixed(1)}" class="avg-line"/>`
    + `<text x="${w - right}" y="${(avgY - 5).toFixed(1)}" class="axis-label" text-anchor="end">均值 ${
      esc(info.fmt(avg))}</text>`;

  $("#trend-title").textContent = state.group === "day" ? "按天趋势"
    : state.group === "hour" ? "按小时趋势"
    : "按 " + (state.group === "model" ? "模型" : "上游") + " 趋势";

  box.innerHTML = `<svg class="trend-svg" viewBox="0 0 ${w} ${h}" role="img" aria-label="用量趋势">`
    + grid + bars + avgLine + "</svg>";
}

/* 占比分布：甜甜圈 + 对齐的图例表（名称 / 值 / 占比 / 请求数），鼠标悬停看明细 */
const PIE_COLORS = ["var(--in)", "var(--out)", "var(--cache)", "var(--brand)",
                    "var(--data)", "var(--ok)", "var(--warn)", "var(--danger)", "var(--text-3)"];

function renderPie() {
  const box = $("#usage-pie");
  if (!box) return;
  const group = state.pieKind;
  const rows = ((window.__summary || {})[group] || []).slice();
  if (!rows.length) {
    box.innerHTML = '<div class="empty">该区间暂无数据</div>';
    return;
  }
  rows.sort((a, b) => (Number(b.total_tokens) || 0) - (Number(a.total_tokens) || 0));
  const total = rows.reduce((s, r) => s + (Number(r.total_tokens) || 0), 0) || 1;
  const top = rows.slice(0, 8);
  const restVal = rows.slice(8).reduce((s, r) => s + (Number(r.total_tokens) || 0), 0);
  if (restVal > 0) {
    top.push({ bucket: `其他 ${rows.length - 8} 项`, total_tokens: restVal, requests: 0, errors: 0 });
  }

  const R = 54;
  const C = 2 * Math.PI * R;
  let acc = 0;
  const arcs = top.map((r, i) => {
    const v = Number(r.total_tokens) || 0;
    const frac = v / total;
    const dash = frac * C;
    const color = PIE_COLORS[i % PIE_COLORS.length];
    const seg = `<circle r="${R}" cx="70" cy="70" fill="none" stroke="${color}" stroke-width="18" `
      + `stroke-dasharray="${dash.toFixed(2)} ${(C - dash).toFixed(2)}" `
      + `stroke-dashoffset="${(-acc).toFixed(2)}" transform="rotate(-90 70 70)" class="pie-arc">`
      + `<title>${esc(r.bucket)} · ${esc(fmtCn(v))} token · ${(frac * 100).toFixed(1)}%</title></circle>`;
    acc += dash;
    return seg;
  }).join("");

  const legend = top.map((r, i) => {
    const v = Number(r.total_tokens) || 0;
    const share = (v / total) * 100;
    return `<div class="pie-row">
      <i class="dot" style="background:${PIE_COLORS[i % PIE_COLORS.length]}"></i>
      <span class="pie-name" title="${esc(r.bucket)}">${esc(r.bucket)}</span>
      <span class="pie-val">${esc(fmtCn(v))}</span>
      <span class="pie-share">${share.toFixed(1)}%</span>
      <span class="pie-req">${fmtInt(r.requests || 0)} 次${
        Number(r.errors) ? ` · ${r.errors} 错` : ""}</span>
    </div>`;
  }).join("");

  box.innerHTML = `<div class="pie-wrap">
    <svg class="pie-svg" viewBox="0 0 140 140" role="img" aria-label="占比分布">${arcs}
      <text x="70" y="66" class="pie-total" text-anchor="middle">${esc(fmtCn(total))}</text>
      <text x="70" y="84" class="pie-sub" text-anchor="middle">token 合计</text>
    </svg>
    <div class="pie-legend">${legend}</div>
  </div>`;
}

async function loadUsageList() {
  const tbody = $("#usage-table tbody");
  if (tbody) tbody.setAttribute("aria-busy", "true");
  try {
    const q = usageScopeQuery({
      limit: state.pageSize,
      offset: state.page * state.pageSize,
      sort: state.usage.listSort,
      order: state.usage.listOrder,
    });
    if (state.usage.only) q.set("only", state.usage.only);
    // 筛选项本身不能被自身筛选收窄：上游列表只看时间；模型列表跟随上游但忽略已选模型
    const upQ = usageScopeQuery();
    ["upstream", "model", "session"].forEach((k) => upQ.delete(k));
    const modelQ = usageScopeQuery();
    ["model", "session"].forEach((k) => modelQ.delete(k));
    const sessionQ = usageScopeQuery();
    sessionQ.delete("session");
    const [data, byUpstream, byModel, bySession] = await Promise.all([
      api(`/api/usage/list?${q.toString()}`),
      api(`/api/usage/summary?group_by=upstream&${upQ}`),
      api(`/api/usage/summary?group_by=model&${modelQ}`),
      api(`/api/usage/summary?group_by=session&${sessionQ}`),
    ]);
    const rows = data.rows || [];
    state.listTotal = data.total || 0;
    state.usage.lastRows = rows;
    renderUsageTable(rows);
    renderSortMarks();
    // 用全量聚合更新筛选项（会话的「(无会话)」是聚合占位，不能当过滤值用）
    state.options.upstreams = (byUpstream.rows || []).map((r) => r.bucket);
    state.options.models = (byModel.rows || []).map((r) => r.bucket);
    state.options.sessions = (bySession.rows || [])
      .map((r) => r.bucket)
      .filter((b) => b && b !== "(无会话)");
    renderFilterOptions();
    renderPageInfo();
  } catch (err) {
    toast(`明细加载失败：${err.message}`, true);
  } finally {
    if (tbody) tbody.removeAttribute("aria-busy");
  }
}

/* 排序指示：表头加 ▲▼ 与 aria-sort，用户不必回想当前按什么排的 */
function renderSortMarks() {
  const table = $("#usage-table");
  if (!table) return;
  $$("th[data-sort]", table).forEach((th) => {
    const active = th.dataset.sort === state.usage.listSort;
    th.classList.toggle("sort-active", active);
    th.setAttribute("aria-sort", active
      ? (state.usage.listOrder === "asc" ? "ascending" : "descending") : "none");
    const base = th.dataset.label || th.textContent.replace(/[▲▼]\s*$/, "").trim();
    th.dataset.label = base;
    th.textContent = active ? `${base} ${state.usage.listOrder === "asc" ? "▲" : "▼"}` : base;
  });
}

const SLOW_MS = 10000;

/* 展开详情：把错误原因、上游模型、完整会话 id 收进来，表格本身保持紧凑 */
function usageDetailRow(r) {
  const pricingReady = !!(state.pricing && state.pricing.configured);
  const items = [
    ["记录 id", r.id],
    ["上游模型", r.upstream_model],
    ["协议", r.protocol],
    ["完整会话", r.session],
    ["缓存写入", fmtInt(r.cache_write_tokens)],
    ["命中率", fmtRate(r.cache_read_tokens, r.prompt_tokens)],
    ["费用", pricingReady ? Number(r.cost || 0).toFixed(6) : "未配置单价"],
    ["错误", r.error || "无"],
  ];
  return `<tr class="detail-row"><td colspan="10"><div class="detail-grid">${
    items.map(([k, v]) => {
      const text = v === null || v === undefined || v === "" ? "—" : String(v);
      return `<div class="detail-item"><span class="detail-key">${esc(k)}</span>` +
        `<span class="detail-val mono">${esc(text)}</span>` +
        `${text !== "—" ? `<button class="ghost tiny" data-copy="${esc(text)}">复制</button>` : ""}</div>`;
    }).join("")
  }</div></td></tr>`;
}

function renderUsageTable(rows) {
  const tbody = $("#usage-table tbody");
  if (!tbody) return;
  const pricingReady = !!(state.pricing && state.pricing.configured);
  if (!rows.length) {
    const narrowed = state.usage.only || state.filters.upstream
      || state.filters.model || state.filters.session;
    tbody.innerHTML = `<tr><td colspan="10" class="empty">${
      narrowed ? "当前筛选下没有明细：切回「全部」或换个区间试试" : "该区间暂无明细"}</td></tr>`;
    return;
  }
  tbody.innerHTML = rows.map((r) => {
    const ok = Number(r.status) < 400;
    const slow = Number(r.duration_ms) >= SLOW_MS;
    const cost = pricingReady && Number(r.cost) ? Number(r.cost).toFixed(6) : "—";
    const open = state.usage.expanded === r.id;
    const main = `<tr data-id="${esc(r.id)}" class="usage-row${open ? " open" : ""}${ok ? "" : " row-bad"}">
      <td class="mono">${esc(fmtTime(r.ts, r.created_at))}</td>
      <td class="mono">${esc(r.requested_model || "—")}</td>
      <td><span class="badge">${esc(r.upstream || "—")}</span></td>
      <td class="num mono" title="${esc(fmtInt(r.prompt_tokens))} / ${esc(fmtInt(r.completion_tokens))}">${esc(fmtCn(r.prompt_tokens))} / ${esc(fmtCn(r.completion_tokens))}</td>
      <td class="num mono" title="${esc(fmtInt(r.cache_read_tokens))}">${r.cache_read_tokens ? esc(fmtCn(r.cache_read_tokens)) : "—"}</td>
      <td class="num mono">${cost}</td>
      <td class="num mono${slow ? " slow" : ""}">${fmtMs(r.duration_ms)}</td>
      <td class="num mono">${Number(r.ttft_ms) ? fmtMs(r.ttft_ms) : "—"}</td>
      <td>${ok ? `<span class="badge ok">${r.status}</span>`
                  : `<span class="badge bad" title="${esc(r.error || "")}">${r.status}</span>`}${
        r.stream ? ' <span class="badge">流</span>' : ""}${
        r.truncated ? ' <span class="badge bad">截断</span>' : ""}${
        r.paused ? ' <span class="badge bad">未续完</span>' : ""}</td>
      <td class="mono">${r.session ? esc(String(r.session).slice(0, 16)) : "—"}</td>
    </tr>`;
    return open ? main + usageDetailRow(r) : main;
  }).join("");
}

function renderPageInfo() {
  const pages = Math.max(1, Math.ceil(state.listTotal / state.pageSize));
  const el = $("#page-info");
  const from = state.listTotal ? state.page * state.pageSize + 1 : 0;
  const to = Math.min(state.listTotal, (state.page + 1) * state.pageSize);
  if (el) {
    el.textContent = state.listTotal
      ? `第 ${state.page + 1} / ${pages} 页 · 共 ${fmtInt(state.listTotal)} 条（本页 ${from}–${to}）`
      : "共 0 条";
  }
  const jump = $("#page-jump");
  if (jump) { jump.max = String(pages); jump.value = String(state.page + 1); }
  const first = $("#first-page"); if (first) first.disabled = state.page === 0;
  const prev = $("#prev-page"); if (prev) prev.disabled = state.page === 0;
  const last = $("#last-page"); if (last) last.disabled = state.page + 1 >= pages;
  const next = $("#next-page"); if (next) next.disabled = state.page + 1 >= pages;
  const size = $("#page-size");
  if (size) size.value = String(state.pageSize);
  renderTableHint();
}

/* 表头下方的状态行：现在到底在看什么、按什么排的，一眼可读 */
const ONLY_LABELS = {
  error: "仅错误", slow: "仅慢请求 ≥10s", ttft: "有首 token",
  cache: "命中缓存", truncated: "截断", paused: "未续完",
};

function renderTableHint() {
  const hint = $("#usage-table-hint");
  if (!hint) return;
  const parts = [];
  if (state.usage.only) parts.push(ONLY_LABELS[state.usage.only] || state.usage.only);
  if (state.filters.upstream) parts.push(`上游 ${state.filters.upstream}`);
  if (state.filters.model) parts.push(`模型 ${state.filters.model}`);
  if (state.filters.session) parts.push(`会话 ${String(state.filters.session).slice(0, 8)}…`);
  parts.push(`按 ${state.usage.listSort} ${state.usage.listOrder === "asc" ? "升序" : "降序"}`);
  hint.textContent = `${parts.join(" · ")} · 点击任意行展开详情`;
}

/* ── 模型页（清单 / 检索 / 分页 / 单价表） ──────────────── */
const SOURCE_LABELS = { explicit: "白名单", fetched: "目录拉取", catalog: "内置目录" };

function initModelControls() {
  const search = $("#model-search");
  if (search) {
    let timer = null;
    search.addEventListener("input", () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        state.catalog.q = search.value || "";
        state.catalog.page = 1;
        loadCatalog();
      }, 300);
    });
  }

  const upstream = $("#model-upstream");
  if (upstream) upstream.addEventListener("change", () => {
    state.catalog.upstream = upstream.value;
    state.catalog.page = 1;
    loadCatalog();
  });

  const refresh = $("#refresh-catalog");
  if (refresh) refresh.addEventListener("click", () => loadCatalog());

  const prev = $("#model-prev");
  if (prev) prev.addEventListener("click", () => {
    if (state.catalog.page > 1) { state.catalog.page -= 1; loadCatalog(); }
  });
  const next = $("#model-next");
  if (next) next.addEventListener("click", () => { state.catalog.page += 1; loadCatalog(); });

  const save = $("#save-pricing");
  if (save) save.addEventListener("click", savePricing);

  const list = $("#model-table");
  if (list) list.addEventListener("click", (ev) => {
    const copyBtn = ev.target.closest("button[data-copy]");
    if (copyBtn) { copyText(copyBtn.dataset.copy); return; }
    const defaultBtn = ev.target.closest("button[data-default]");
    if (defaultBtn) setDefaultModel(defaultBtn.dataset.default);
  });

  // 单价 / 来源胶囊：与用量页同构的服务端筛选（先筛后分页，total 才可信）
  const chips = $("#model-only-chips");
  if (chips) chips.addEventListener("click", (ev) => {
    const btn = ev.target.closest("button[data-price], button[data-source]");
    if (!btn) return;
    const isPrice = btn.dataset.price !== undefined;
    const attr = isPrice ? "data-price" : "data-source";
    $$(`button[${attr}]`, chips).forEach((b) => b.classList.toggle("active", b === btn));
    if (isPrice) state.catalog.price = btn.dataset.price || "";
    else state.catalog.source = btn.dataset.source || "";
    state.catalog.page = 1;
    loadCatalog();
  });

  // 表头排序：全部由后端在完整结果集上排序，再切片分页
  if (list) {
    $$("th[data-model-sort]", list).forEach((th) => {
      th.addEventListener("click", () => {
        const key = th.dataset.modelSort;
        if (state.catalog.sort === key) {
          state.catalog.order = state.catalog.order === "asc" ? "desc" : "asc";
        } else {
          state.catalog.sort = key;
          state.catalog.order = "asc";
        }
        state.catalog.page = 1;
        loadCatalog();
      });
    });
  }

  const size = $("#model-page-size");
  if (size) size.addEventListener("change", () => {
    state.catalog.pageSize = Math.max(10, Math.min(200, Number(size.value) || 25));
    state.catalog.page = 1;
    loadCatalog();
  });
  const first = $("#model-first");
  if (first) first.addEventListener("click", () => {
    if (state.catalog.page > 1) { state.catalog.page = 1; loadCatalog(); }
  });
  const last = $("#model-last");
  if (last) last.addEventListener("click", () => {
    const pages = Math.max(1, Math.ceil(state.catalog.total / state.catalog.pageSize));
    if (state.catalog.page !== pages) { state.catalog.page = pages; loadCatalog(); }
  });
  const jump = $("#model-jump");
  if (jump) jump.addEventListener("change", () => {
    const pages = Math.max(1, Math.ceil(state.catalog.total / state.catalog.pageSize));
    const target = Math.min(pages, Math.max(1, Number(jump.value) || 1));
    if (target !== state.catalog.page) { state.catalog.page = target; loadCatalog(); }
    else jump.value = String(state.catalog.page);
  });
}

async function loadCatalog() {
  const q = new URLSearchParams({
    page: state.catalog.page,
    page_size: state.catalog.pageSize,
    sort: state.catalog.sort,
    order: state.catalog.order,
  });
  if (state.catalog.q) q.set("q", state.catalog.q);
  if (state.catalog.upstream) q.set("upstream", state.catalog.upstream);
  if (state.catalog.price) q.set("price", state.catalog.price);
  if (state.catalog.source) q.set("source", state.catalog.source);
  try {
    const data = await api(`/api/models/catalog?${q.toString()}`);
    state.catalog.total = data.total || 0;
    state.catalog.currency = data.currency || "CNY";
    state.catalog.upstreams = data.upstreams || [];
    renderCatalog(data.rows || []);
    renderModelOptions();
    renderModelKpis(data);
    renderModelPageInfo();
  } catch (err) {
    toast(`模型清单加载失败：${err.message}`, true);
  }
}

function renderModelOptions() {
  const select = $("#model-upstream");
  if (!select) return;
  const current = select.value;
  select.innerHTML = '<option value="">全部上游</option>' +
    (state.catalog.upstreams || []).map((u) =>
      `<option value="${esc(u.name)}">${esc(u.name)} · ${u.models} 个模型</option>`).join("");
  if (current) select.value = current;
}

function renderModelKpis(data) {
  const box = $("#model-kpis");
  if (!box) return;
  const rows = data.rows || [];
  box.innerHTML = [
    kpi("", "模型总数", fmtInt(data.total), "个", "accent"),
    kpi("", "上游数", fmtInt((data.upstreams || []).length), "个", "accent"),
    kpi("", "当前页", fmtInt(rows.length), `/${fmtInt(data.total)}`),
    kpi("", "单价表", data.pricing_configured ? "已配置" : "未配置",
        data.currency || "CNY", data.pricing_configured ? "ok" : "warn"),
  ].join("");
  const hint = $("#model-hint");
  if (hint) {
    hint.textContent = state.catalog.q
      ? `检索「${state.catalog.q}」命中 ${fmtInt(data.total)} 个`
      : `共 ${fmtInt(data.total)} 个模型`;
  }
}

function renderCatalogRate(row) {
  const rate = row.costHint;
  if (!rate) return '<span class="hint">未配置</span>';
  const parts = [];
  if (rate.input) parts.push(`入${rate.input}`);
  if (rate.output) parts.push(`出${rate.output}`);
  if (rate.cacheRead) parts.push(`读${rate.cacheRead}`);
  if (rate.cacheWrite) parts.push(`写${rate.cacheWrite}`);
  if (!parts.length) return '<span class="hint">未配置</span>';
  return `<span class="mono" title="命中规则 ${esc(row.costRule || "default")}；单位：${
    esc(state.catalog.currency)}/百万 token">${esc(parts.join(" / "))}</span>`;
}

function renderCatalog(rows) {
  const tbody = $("#model-table tbody");
  if (!tbody) return;
  const meta = $("#model-meta");
  if (meta) {
    const pages = Math.max(1, Math.ceil(state.catalog.total / state.catalog.pageSize));
    meta.textContent = `第 ${state.catalog.page} / ${pages} 页`;
  }
  if (!rows.length) {
    tbody.innerHTML = '<tr><td colspan="8" class="empty">没有匹配的模型</td></tr>';
    return;
  }
  tbody.innerHTML = rows.map((r) => {
    const sources = (r.sources || []).map((s) => SOURCE_LABELS[s] || s).join(" + ");
    const aliases = (r.aliases || []).length
      ? r.aliases.map((a) => esc(a)).join(", ") : "—";
    return `<tr>
      <td class="mono">${esc(r.model)}${r.default ? ' <span class="badge ok">默认</span>' : ""}</td>
      <td>${(r.upstreams || []).map((u) => `<span class="badge">${esc(u)}</span>`).join(" ")}</td>
      <td>${(r.protocols || []).map((p) => esc(p)).join(" / ") || "—"}</td>
      <td>${esc(sources || "—")}</td>
      <td class="mono">${aliases}</td>
      <td>${r.minPlan ? `<span class="badge">${esc(r.minPlan)}</span>` : "—"}</td>
      <td>${renderCatalogRate(r)}</td>
      <td class="row-actions">
        <button type="button" class="ghost" data-copy="${esc(r.model)}">复制</button>
        ${r.default ? "" :
          `<button type="button" class="ghost" data-default="${esc(r.model)}">设为默认</button>`}
      </td>
    </tr>`;
  }).join("");
}

function renderModelPageInfo() {
  const pages = Math.max(1, Math.ceil(state.catalog.total / state.catalog.pageSize));
  const info = $("#model-page-info");
  const from = state.catalog.total ? (state.catalog.page - 1) * state.catalog.pageSize + 1 : 0;
  const to = Math.min(state.catalog.total, state.catalog.page * state.catalog.pageSize);
  if (info) {
    info.textContent = state.catalog.total
      ? `第 ${state.catalog.page} / ${pages} 页 · 共 ${fmtInt(state.catalog.total)} 个（本页 ${from}–${to}）`
      : "共 0 个模型";
  }
  const jump = $("#model-jump");
  if (jump) { jump.max = String(pages); jump.value = String(state.catalog.page); }
  const size = $("#model-page-size");
  if (size) size.value = String(state.catalog.pageSize);
  const first = $("#model-first"); if (first) first.disabled = state.catalog.page <= 1;
  const prev = $("#model-prev"); if (prev) prev.disabled = state.catalog.page <= 1;
  const last = $("#model-last"); if (last) last.disabled = state.catalog.page >= pages;
  const next = $("#model-next"); if (next) next.disabled = state.catalog.page >= pages;
  renderModelSortMarks();
  renderModelTableHint();
}

/* 排序指示与状态行：与用量明细同一套视觉语言，两个页面的读法保持一致 */
function renderModelSortMarks() {
  const table = $("#model-table");
  if (!table) return;
  $$("th[data-model-sort]", table).forEach((th) => {
    const active = th.dataset.modelSort === state.catalog.sort;
    th.classList.toggle("sort-active", active);
    th.setAttribute("aria-sort", active
      ? (state.catalog.order === "asc" ? "ascending" : "descending") : "none");
    const base = th.dataset.label || th.textContent.replace(/[▲▼]\s*$/, "").trim();
    th.dataset.label = base;
    th.textContent = active ? `${base} ${state.catalog.order === "asc" ? "▲" : "▼"}` : base;
  });
}

const CATALOG_SORT_LABELS = { model: "名称", upstream: "上游", plan: "套餐门槛", price: "单价" };

function renderModelTableHint() {
  const hint = $("#model-table-hint");
  if (!hint) return;
  const parts = [];
  if (state.catalog.price === "missing") parts.push("未配置单价");
  if (state.catalog.price === "priced") parts.push("已配置单价");
  if (state.catalog.source) parts.push(`来源 ${state.catalog.source}`);
  if (state.catalog.upstream) parts.push(`上游 ${state.catalog.upstream}`);
  if (state.catalog.q) parts.push(`检索「${state.catalog.q}」`);
  const label = CATALOG_SORT_LABELS[state.catalog.sort] || state.catalog.sort;
  parts.push(`按 ${label} ${state.catalog.order === "asc" ? "升序" : "降序"}`);
  hint.textContent = `${parts.join(" · ")} · 点表头切换排序`;
}

async function setDefaultModel(model) {
  try {
    await postJson("/api/config", { config: { defaultModel: model } });
    toast(`默认模型已设为 ${model}`);
    await loadCatalog();
  } catch (err) {
    toast(`设置失败：${err.message}`, true);
  }
}

/* ── 单价表 / 保留策略 ─────────────────────────────────── */
async function loadPricing() {
  try {
    const data = await api("/api/pricing");
    state.pricing = data;
    state.pricingLoaded = true;
    const ta = $("#pricing-json");
    if (ta && document.activeElement !== ta) {
      ta.value = JSON.stringify({
        currency: data.currency,
        default: data.pricing.default,
        rules: data.pricing.rules,
      }, null, 2);
    }
    const currency = $("#pricing-currency");
    if (currency && document.activeElement !== currency) currency.value = data.currency || "CNY";
    const hint = $("#pricing-hint");
    if (hint) {
      hint.textContent = data.configured
        ? `已配置单价（${data.currency}），费用按本地口径估算`
        : "未配置单价：费用列显示「—」，不代表免费";
    }
    const days = $("#retention-days");
    if (days && document.activeElement !== days) days.value = data.retention_days || 0;
    const retentionHint = $("#retention-hint");
    if (retentionHint) {
      retentionHint.textContent = data.retention_days
        ? `超过 ${data.retention_days} 天的明细会被自动清理`
        : "0 = 永久保留";
    }
  } catch (_) {
    /* 单价表读取失败不影响其它面板 */
  }
}

async function savePricing() {
  const ta = $("#pricing-json");
  let parsed;
  try {
    parsed = JSON.parse((ta && ta.value) || "{}");
  } catch (err) {
    toast(`单价表不是合法 JSON：${err.message}`, true);
    return;
  }
  if (!parsed || typeof parsed !== "object") parsed = {};
  const currency = (($("#pricing-currency") || {}).value || "CNY").trim() || "CNY";
  const recompute = !!($("#pricing-recompute") || {}).checked;
  if (recompute) {
    const ok = await confirmModal({
      title: "保存单价表并重算历史费用",
      text: "保存后会按新单价重算全部历史明细的费用字段，耗时取决于明细条数；代理转发不受影响。",
      confirmLabel: "保存并重算",
    });
    if (!ok) return;
  }
  try {
    const data = await postJson("/api/pricing", {
      pricing: Object.assign({}, parsed, { currency }),
      recompute,
    });
    toast(`单价表已保存${data.recomputed ? `，重算 ${data.recomputed} 条历史` : ""}`);
    state.pricing = { currency: data.currency, configured: data.configured, pricing: data.pricing };
    renderKpis((await api(`/api/usage/totals?range=${state.range}`)).totals || {});
    loadCatalog();
  } catch (err) {
    toast(`保存失败：${err.message}`, true);
  }
}

/* ── 模型用量（搜索 / 排序 / 仅看用过的） ─────────────────── */
function initModelUsageControls() {
  const search = $("#model-usage-search");
  if (search) {
    let timer = null;
    search.addEventListener("input", () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        state.usage.q = search.value || "";
        renderModelUsage();
      }, 250);
    });
  }

  const used = $("#model-usage-used");
  if (used) used.addEventListener("change", () => {
    state.usage.usedOnly = used.checked;
    renderModelUsage();
  });

  const sort = $("#model-usage-sort");
  if (sort) sort.addEventListener("change", () => {
    state.usage.sortKey = sort.value;
    state.usage.sortDir = sort.value === "model" ? "asc" : "desc";
    renderModelUsage();
  });

  const table = $("#model-usage-table");
  if (table) table.addEventListener("click", (ev) => {
    const th = ev.target.closest("th.sortable");
    if (!th) return;
    const key = th.dataset.key;
    if (state.usage.sortKey === key) {
      state.usage.sortDir = state.usage.sortDir === "desc" ? "asc" : "desc";
    } else {
      state.usage.sortKey = key;
      state.usage.sortDir = key === "model" ? "asc" : "desc";
    }
    const select = $("#model-usage-sort");
    if (select && Array.from(select.options).some((o) => o.value === key)) {
      select.value = key;
    }
    renderModelUsage();
  });
}

async function loadUpstreamUsage() {
  try {
    const data = await api(`/api/usage/upstreams?${usageScopeQuery({ limit: 200 })}`);
    state.usage.upstreams = data.rows || [];
    state.usage.upstreamModels = data.models || [];
    renderUpstreamUsage();
  } catch (err) {
    const tbody = $("#upstream-usage-table tbody");
    if (tbody) tbody.innerHTML = `<tr><td colspan="10" class="empty">${esc(err.message)}</td></tr>`;
  }
}

function renderUpstreamUsage() {
  const tbody = $("#upstream-usage-table tbody");
  if (!tbody) return;
  const pricingReady = !!(state.pricing && state.pricing.configured);
  const key = state.usage.upstreamSortKey;
  const dir = state.usage.upstreamSortDir === "asc" ? 1 : -1;
  const rows = state.usage.upstreams.slice().sort((a, b) => {
    const va = a[key];
    const vb = b[key];
    if (typeof va === "string" || typeof vb === "string") {
      return String(va || "").localeCompare(String(vb || "")) * dir;
    }
    return ((Number(va) || 0) - (Number(vb) || 0)) * dir;
  });

  $$("#upstream-usage-table th.sortable").forEach((th) => {
    th.classList.remove("asc", "desc");
    if (th.dataset.key === key) th.classList.add(dir === 1 ? "asc" : "desc");
  });

  const hint = $("#upstream-usage-hint");
  if (hint) {
    hint.textContent = rows.length
      ? `共 ${fmtInt(rows.length)} 个上游 · 点击上游行展开其模型`
      : "该作用域没有上游用量";
  }
  if (!rows.length) {
    tbody.innerHTML = '<tr><td colspan="10" class="empty">暂无上游用量</td></tr>';
    return;
  }

  tbody.innerHTML = rows.map((r) => {
    const open = state.usage.upstreamExpanded === r.upstream;
    const models = state.usage.upstreamModels.filter((m) => m.upstream === r.upstream);
    const rate = fmtRate(r.cache_read_tokens, r.prompt_tokens);
    const okRate = Number(r.requests)
      ? (Number(r.success_rate) * 100).toFixed(1).replace(/\.0$/, "") + "%" : "—";
    const main = `<tr data-upstream="${esc(r.upstream)}" class="usage-row${open ? " open" : ""}">
      <td><span class="badge">${esc(r.upstream || "—")}</span></td>
      <td class="num">${fmtInt(r.models_count)}</td>
      <td class="num" title="命中输入 / 总输入">${esc(rate)}</td>
      <td class="num">${fmtInt(r.requests)}</td>
      <td class="num">${fmtTok(r.cache_read_tokens)}</td>
      <td class="num">${fmtTok(r.total_tokens)}</td>
      <td class="num">${okRate}</td>
      <td class="num">${fmtMs(r.avg_duration_ms)}</td>
      <td class="num">${Number(r.p95_duration_ms) ? fmtMs(r.p95_duration_ms) : "—"}</td>
      <td class="num">${pricingReady && Number(r.cost) ? Number(r.cost).toFixed(4) : "—"}
        <button class="ghost tiny" data-only-upstream="${esc(r.upstream)}"
          title="把页面作用域切到该上游">只看此上游</button></td>
    </tr>`;
    if (!open) return main;
    const detail = models.length
      ? models.map((m) => `<div class="detail-item">
          <span class="detail-key">${esc(m.model || "—")}</span>
          <span class="detail-val mono">命中 ${esc(fmtRate(m.cache_read_tokens, m.prompt_tokens))} ·
            ${fmtInt(m.requests)} 次 · ${fmtTok(m.total_tokens)} token ·
            成功率 ${(Number(m.success_rate) * 100).toFixed(1).replace(/\.0$/, "")}% ·
            均耗 ${fmtMs(m.avg_duration_ms)}</span>
        </div>`).join("")
      : '<div class="detail-item"><span class="detail-val">该上游在区间内没有模型明细</span></div>';
    return main + `<tr class="detail-row"><td colspan="10"><div class="detail-grid">${detail}</div></td></tr>`;
  }).join("");
}

async function loadModelUsage() {
  try {
    const data = await api(`/api/usage/models?${usageScopeQuery({ limit: 200 })}`);
    state.usage.models = data.rows || [];
    if (data.pricing_configured !== undefined) {
      state.pricing = Object.assign({}, state.pricing, {
        configured: !!data.pricing_configured,
        currency: data.currency || state.pricing.currency,
      });
    }
    renderModelUsage();
  } catch (err) {
    const tbody = $("#model-usage-table tbody");
    if (tbody) {
      tbody.innerHTML = `<tr><td colspan="11" class="empty">${esc(err.message)}</td></tr>`;
    }
  }
}

function renderModelUsage() {
  const tbody = $("#model-usage-table tbody");
  if (!tbody) return;
  const pricingReady = !!(state.pricing && state.pricing.configured);
  const q = (state.usage.q || "").trim().toLowerCase();
  let rows = state.usage.models.slice();
  if (state.usage.usedOnly) rows = rows.filter((r) => Number(r.requests) > 0);
  if (q) rows = rows.filter((r) => String(r.model || "").toLowerCase().includes(q));
  // 命中率是派生指标：先算出来，才能像其他列一样排序/显示
  rows = rows.map((r) => Object.assign({}, r, {
    cache_hit_rate: cacheHitRate(r.cache_read_tokens, r.prompt_tokens) || 0,
  }));

  const key = state.usage.sortKey;
  const dir = state.usage.sortDir === "asc" ? 1 : -1;
  rows.sort((a, b) => {
    const va = a[key];
    const vb = b[key];
    if (typeof va === "string" || typeof vb === "string") {
      return String(va || "").localeCompare(String(vb || "")) * dir;
    }
    return ((Number(va) || 0) - (Number(vb) || 0)) * dir;
  });

  $$("#model-usage-table th.sortable").forEach((th) => {
    th.classList.remove("asc", "desc");
    if (th.dataset.key === key) th.classList.add(dir === 1 ? "asc" : "desc");
  });

  const hint = $("#model-usage-hint");
  if (hint) {
    hint.textContent = rows.length
      ? `共 ${fmtInt(rows.length)} 个模型${q ? `（关键词「${q}」）` : ""} · 点击表头可排序`
      : "该区间没有模型用量";
  }
  if (!rows.length) {
    tbody.innerHTML = '<tr><td colspan="12" class="empty">暂无模型用量</td></tr>';
    return;
  }
  tbody.innerHTML = rows.map((r) => {
    const okRate = Number(r.requests)
      ? (Number(r.success_rate) * 100).toFixed(1).replace(/\.0$/, "") + "%" : "—";
    return `<tr>
      <td class="mono">${esc(r.model || "—")}</td>
      <td class="num" title="${esc(fmtRate(r.cache_read_tokens, r.prompt_tokens))}">${esc(fmtRate(r.cache_read_tokens, r.prompt_tokens))}</td>
      <td class="num">${fmtInt(r.requests)}</td>
      <td class="num">${fmtTok(r.prompt_tokens)}</td>
      <td class="num">${fmtTok(r.completion_tokens)}</td>
      <td class="num">${fmtTok(r.total_tokens)}</td>
      <td class="num">${fmtTok(r.cache_read_tokens)}</td>
      <td class="num">${pricingReady && Number(r.cost) ? Number(r.cost).toFixed(4) : "—"}</td>
      <td class="num">${Number(r.errors) ? `<span class="badge warn">${okRate}</span>` : okRate}</td>
      <td class="num">${fmtMs(r.avg_duration_ms)}</td>
      <td class="num">${Number(r.p95_duration_ms) ? fmtMs(r.p95_duration_ms) : "—"}</td>
      <td class="num">${Number(r.avg_ttft_ms) ? fmtMs(r.avg_ttft_ms) : "—"}</td>
    </tr>`;
  }).join("");
}

/* ── 日志 ──────────────────────────────────────────────── */
function initLogControls() {
  const root = $("#log-seg");
  if (root) root.addEventListener("click", (ev) => {
    const btn = ev.target.closest("button[data-level]");
    if (!btn) return;
    $$("button[data-level]", root).forEach((b) => b.classList.toggle("active", b === btn));
    state.logs.level = btn.dataset.level;
    loadLogs();
  });

  let timer = null;
  const search = $("#log-search");
  if (search) search.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      state.logs.q = search.value || "";
      loadLogs();
    }, 300);
  });

  const refresh = $("#refresh-logs");
  if (refresh) refresh.addEventListener("click", () => loadLogs());

  const lines = $("#log-lines");
  if (lines) lines.addEventListener("change", () => {
    state.logs.lines = Math.max(50, Math.min(5000, Number(lines.value) || 400));
    loadLogs();
  });

  const auto = $("#log-auto");
  if (auto) {
    auto.addEventListener("change", () => setLogAuto(Number(auto.value) || 0));
    setLogAuto(Number(auto.value) || 0);
  }

  const copyBtn = $("#log-copy");
  if (copyBtn) copyBtn.addEventListener("click", () => {
    const viewer = $("#log-viewer");
    copyText(viewer ? viewer.textContent || "" : "");
  });

  // 单行复制：hover 按钮或点击行空白处；正在框选文本时不打断选择
  const viewer = $("#log-viewer");
  if (viewer) viewer.addEventListener("click", (ev) => {
    const selection = window.getSelection ? String(window.getSelection()) : "";
    if (selection && selection.trim()) return;
    const line = ev.target.closest(".ln[data-line]");
    if (!line) return;
    const row = (state.logs.rows || [])[Number(line.dataset.line)];
    if (!row) return;
    copyText(row.text || "");
  });

  const bottom = $("#log-bottom");
  if (bottom) bottom.addEventListener("click", () => {
    const viewer = $("#log-viewer");
    if (viewer) viewer.scrollTop = viewer.scrollHeight;
  });

  const clearBtn = $("#clear-logs");
  if (clearBtn) clearBtn.addEventListener("click", async () => {
    const ok = await confirmModal({
      title: "清空运行日志",
      text: "将清空本地运行日志文件（不含用量数据库），此操作不可撤销；代理转发不受影响。",
      confirmLabel: "清空",
    });
    if (!ok) return;
    try {
      await postJson("/api/logs/clear", {});
      toast("日志已清空");
      loadLogs();
    } catch (err) {
      toast(`清空失败：${err.message}`, true);
    }
  });
}

/* 自动刷新：只重读日志文件，不触碰代理；页面隐藏或不在日志页时跳过 */
function setLogAuto(seconds) {
  if (state.logs.timer) { clearInterval(state.logs.timer); state.logs.timer = null; }
  state.logs.autoSeconds = seconds;
  if (!(seconds > 0)) return;
  state.logs.timer = setInterval(() => {
    if (document.hidden) return;
    const panel = $("#panel-logs");
    if (panel && !panel.classList.contains("active")) return;
    loadLogs();
  }, seconds * 1000);
}

/* 关键词高亮：在已转义的文本上替换，用户输入不会被当成 HTML */
function highlightLog(text, query) {
  const safe = esc(text);
  if (!query) return safe;
  const needle = esc(query).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  try {
    return safe.replace(new RegExp(needle, "gi"), (m) => `<mark>${m}</mark>`);
  } catch (_) {
    return safe;
  }
}

function classifyLogLine(text) {
  if (/Traceback|Error|错误|失败|异常|500/.test(text)) return "err";
  if (/注意|警告|WARN|截断|未续完|超时/.test(text)) return "warn";
  return "";
}

async function loadLogs() {
  const viewer = $("#log-viewer");
  if (!viewer) return;
  const q = new URLSearchParams({ lines: state.logs.lines, level: state.logs.level });
  if (state.logs.q) q.set("q", state.logs.q);
  try {
    const data = await api(`/api/logs?${q.toString()}`);
    const rows = data.lines || [];
    state.logs.rows = rows;
    // 贴底才自动跟随最新：用户往上翻历史时不要把他拽回底部
    const atBottom = viewer.scrollTop + viewer.clientHeight >= viewer.scrollHeight - 40;
    viewer.innerHTML = rows.length
      ? rows.map((l, i) => {
          const cls = classifyLogLine(l.text || "");
          return `<span class="ln ${cls}" data-line="${i}" title="点击复制本行"><span class="no">${l.no}</span><span class="txt">${
            highlightLog(l.text || "", state.logs.q)}</span><button type="button" class="ln-copy" tabindex="-1">复制</button></span>`;
        }).join("\n")
      : `<span class="ln">（没有匹配的日志）</span>`;
    const meta = $("#log-meta");
    if (meta) {
      meta.textContent = `${fmtInt(data.matched)}/${fmtInt(data.total)} 行` +
        (data.size ? ` · ${fmtInt(data.size)} 字节` : "") +
        (state.logs.q ? ` · 关键字「${state.logs.q}」` : "") +
        (data.note ? ` · ${data.note}` : "");
    }
    const fresh = $("#log-fresh");
    if (fresh) {
      const at = new Date();
      state.logs.lastUpdated = at;
      const pad = (n) => String(n).padStart(2, "0");
      fresh.textContent = `更新于 ${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`;
    }
    if (atBottom) viewer.scrollTop = viewer.scrollHeight;
  } catch (err) {
    viewer.innerHTML = `<span class="ln err">加载失败：${esc(err.message)}</span>`;
  }
}

/* ── 启动 ──────────────────────────────────────────────── */
function boot() {
  initTheme();
  initTabs();
  initUsageControls();
  initModelUsageControls();
  initModelControls();
  initConfigActions();
  initUpstreamActions();
  initLogControls();
  initFolds();
  loadOverview();
  loadPricing();
  setInterval(loadInfo, 15000);
  // 日志自动刷新统一由 setLogAuto（#log-auto 选择器）驱动，这里不再另起一个定时器
}

document.addEventListener("DOMContentLoaded", boot);
