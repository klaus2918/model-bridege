/* Model-Gate · ZCode 账号池面板
 * 数据源：/api/zcode/{status,accounts,config,claim,attach,captcha}
 * 约束：不显示 JWT/API Key 原文；所有写操作都走显式按钮。
 */
"use strict";

const ZC = {
  status: null,
  config: null,
  accounts: [],
  claims: [],
  loading: false,
  oauthTimer: null,
};

function zcTime(ms) {
  if (!ms) return "—";
  try {
    return new Date(Number(ms)).toLocaleString("zh-CN", { hour12: false });
  } catch (_) {
    return "—";
  }
}

function zcChip(label, tone) {
  const cls = tone ? ` ${tone}` : "";
  return `<span class="badge${cls}">${esc(label)}</span>`;
}

function zcStatusTone(status) {
  if (status === "active") return "ok";
  if (status === "invalid" || status === "disabled") return "bad";
  if (status === "cooling" || status === "exhausted") return "warn";
  return "";
}

function zcAccountName(id) {
  const found = ZC.accounts.find((a) => a.id === id);
  return found ? found.name : id;
}

function zcConfigValue(id, value) {
  const el = $(id);
  if (el) el.value = value == null ? "" : value;
}

function zcFillConfig(cfg) {
  ZC.config = cfg || {};
  const enabled = $("#zcode-enabled");
  if (enabled) enabled.checked = !!cfg.enabled;
  zcConfigValue("#zcode-backend", cfg.backend || "appserver");
  zcConfigValue("#zcode-claim-interval", cfg.claimIntervalSeconds);
  zcConfigValue("#zcode-concurrency", cfg.accountConcurrency);
  zcConfigValue("#zcode-browser-path", cfg.browserPath);
  zcConfigValue("#zcode-cli-path", cfg.cliPath);
  zcConfigValue("#zcode-node-path", cfg.nodePath);
  zcConfigValue("#zcode-data-dir", cfg.dataDir);
  zcConfigValue("#zcode-captcha-scene", cfg.captchaScene);
}

function zcRenderStatus(status) {
  const host = $("#zcode-status");
  if (!host) return;
  const pool = status.pool || {};
  const captcha = status.captcha || {};
  const last = status.lastClaim;
  const chips = [
    zcChip(status.enabled ? "插件已启用" : "插件未启用", status.enabled ? "ok" : "bad"),
    zcChip(`backend ${status.backend || "—"}`, status.backend === "appserver" ? "ok" : "warn"),
    zcChip(`账号 ${status.accounts?.total ?? 0} / 活跃 ${status.accounts?.active ?? 0}`, "ok"),
    zcChip(`池 active ${pool.active ?? 0} · cooling ${pool.cooling ?? 0} · invalid ${pool.invalid ?? 0}`, pool.invalid ? "warn" : ""),
    zcChip(`浏览器 ${status.browserExists ? "可用" : "缺失"}`, status.browserExists ? "ok" : "bad"),
    zcChip(`CLI ${status.cliExists ? "可用" : "缺失"}`, status.cliExists ? "ok" : "bad"),
    zcChip(`验证码池 ${captcha.available ?? 0}`, captcha.available ? "ok" : "warn"),
  ];
  if (last) {
    chips.push(zcChip(
      `最近领取 ${last.ok ? "成功" : "失败"}${last.code != null ? ` (${last.code})` : ""} · ${zcTime(last.createdAt)}`,
      last.ok ? "ok" : "warn",
    ));
  }
  host.innerHTML = chips.join("");
  const poolHint = $("#zcode-pool-hint");
  if (poolHint) {
    poolHint.textContent = `池 ${pool.total ?? 0} 个账号：active ${pool.active ?? 0} / cooling ${pool.cooling ?? 0} / exhausted ${pool.exhausted ?? 0} / invalid ${pool.invalid ?? 0} / disabled ${pool.disabled ?? 0}`;
  }
  const glmHint = $("#zcode-glm-hint");
  if (glmHint) {
    glmHint.textContent = status.enabled
      ? "接入后写入 upstreams.zcode；请求由账号池轮询，backend=appserver。"
      : "插件未启用：先在上方启用并保存设置，再接入 GLM。";
  }
}

function zcPlanText(account) {
  const plan = account.plan;
  const plans = account.quota && account.quota.plans;
  const parts = [];
  if (plan && plan.name) parts.push(plan.name);
  if (Array.isArray(plans)) parts.push(`${plans.length} 个可领`);
  if (plan && Array.isArray(plan.grants) && plan.grants.length) {
    parts.push(plan.grants.map((g) => `${g.name || "额度"}:${g.units ?? "?"}`).join(" / "));
  }
  return parts.length ? parts.join(" · ") : "—";
}

function zcRenderAccounts() {
  const tbody = $("#zcode-account-table");
  if (!tbody) return;
  if (!ZC.accounts.length) {
    tbody.innerHTML = `<tr><td colspan="7" class="empty">还没有账号：用「从本机 ZCode 导入」或「OAuth 登录」添加。</td></tr>`;
  } else {
    tbody.innerHTML = ZC.accounts.map((a) => {
      const status = zcStatusTone(a.status);
      const cooling = a.coolingUntil && a.coolingUntil > Date.now()
        ? `<div class="hint">冷却至 ${zcTime(a.coolingUntil)}</div>` : "";
      return `<tr data-zc-id="${esc(a.id)}">
        <td>${esc(a.name)}<div class="hint">${esc(a.secretMasked || "")}</div></td>
        <td>${esc(a.mode)}</td>
        <td><span class="badge${status ? ` ${status}` : ""}">${esc(a.status)}</span>${cooling}</td>
        <td>${esc(zcPlanText(a))}</td>
        <td class="num">${a.useCount ?? 0} / ${a.failCount ?? 0}</td>
        <td title="${esc(a.lastError || "")}">${esc((a.lastError || "").slice(0, 80))}</td>
        <td>
          <button type="button" class="ghost tiny" data-zc-action="refresh">刷新额度</button>
          <button type="button" class="ghost tiny" data-zc-action="toggle">${a.enabled ? "停用" : "启用"}</button>
          <button type="button" class="ghost tiny danger-ghost" data-zc-action="delete">删除</button>
        </td>
      </tr>`;
    }).join("");
  }
  const hint = $("#zcode-account-hint");
  if (hint) {
    hint.textContent = ZC.accounts.length
      ? "账号 JWT 只存本机加密库；面板只显示掩码。点「刷新额度」会调用 billing/preview（只读，不领取）。"
      : "导入后账号进入 SQLite 账号池；自动领取按设置里的间隔执行。";
  }
}

function zcRenderClaims() {
  const tbody = $("#zcode-claim-table");
  if (!tbody) return;
  if (!ZC.claims.length) {
    tbody.innerHTML = `<tr><td colspan="5" class="empty">还没有领取记录。</td></tr>`;
  } else {
    tbody.innerHTML = ZC.claims.map((c) => {
      const tone = c.ok ? "ok" : (c.code === 1005 ? "warn" : "bad");
      const result = c.ok
        ? "成功"
        : `失败${c.code != null ? ` (${c.code})` : ""}${c.message ? ` · ${c.message}` : ""}`;
      return `<tr>
        <td>${esc(zcTime(c.createdAt))}</td>
        <td>${esc(zcAccountName(c.accountId))}</td>
        <td>${esc(c.planName || c.planId || "—")}</td>
        <td><span class="badge ${tone}">${esc(result)}</span></td>
        <td>${esc(c.nextAt ? zcTime(c.nextAt) : "—")}</td>
      </tr>`;
    }).join("");
  }
  const hint = $("#zcode-claims-hint");
  if (hint) hint.textContent = `最近 ${ZC.claims.length} 条；1005=今日名额用完，3007=验证码失败。`;
}

async function loadZcode() {
  if (ZC.loading) return;
  ZC.loading = true;
  try {
    const [status, accounts, config] = await Promise.all([
      api("/api/zcode/status"),
      api("/api/zcode/accounts"),
      api("/api/zcode/config"),
    ]);
    ZC.status = status;
    ZC.accounts = accounts.accounts || [];
    ZC.claims = accounts.claims || [];
    zcRenderStatus(status);
    zcRenderAccounts();
    zcRenderClaims();
    zcFillConfig(config.config || {});
    const hint = $("#zcode-action-hint");
    if (hint) hint.textContent = "";
  } catch (err) {
    toast(`加载 ZCode 状态失败：${err.message}`, true);
  } finally {
    ZC.loading = false;
  }
}

async function zcSaveConfig() {
  const previous = ZC.config || {};
  const next = {
    ...previous,
    enabled: !!$("#zcode-enabled")?.checked,
    backend: $("#zcode-backend")?.value || "appserver",
    cliPath: $("#zcode-cli-path")?.value || "",
    nodePath: $("#zcode-node-path")?.value || "",
    dataDir: $("#zcode-data-dir")?.value || "",
    browserPath: $("#zcode-browser-path")?.value || "",
    captchaScene: $("#zcode-captcha-scene")?.value || previous.captchaScene || "",
    claimIntervalSeconds: Number($("#zcode-claim-interval")?.value || 0),
    accountConcurrency: Number($("#zcode-concurrency")?.value || 1),
  };
  try {
    const saved = await api("/api/zcode/config", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(next),
    });
    ZC.config = saved.config || next;
    toast("ZCode 设置已保存");
    await loadZcode();
  } catch (err) {
    toast(`保存失败：${err.message}`, true);
  }
}

async function zcImportLocalInfo() {
  try {
    const info = await postJson("/api/zcode/accounts/import/local", {});
    if (!info.found) {
      toast("本机没有可导入的 zcodejwttoken：请先登录 ZCode 客户端，或改用 OAuth/粘贴。", true);
      return;
    }
    openModal({
      title: "从本机 ZCode 导入",
      hint: `找到 zcodejwttoken（${info.tokenLength} 字符）。JWT 不会显示，也不会写入配置文件。`,
      body: `<div class="hint">数据目录：${esc(info.dataDir || "")}<br>字段：${esc((info.keys || []).join("、"))}</div>`,
      actions: [
        { label: "取消", className: "ghost" },
        {
          label: "确认导入",
          className: "primary",
          onClick: async () => {
            try {
              const result = await postJson("/api/zcode/accounts/import", {});
              toast(result.duplicate ? "该账号已在池中" : "账号已导入");
              await loadZcode();
              return false;
            } catch (err) {
              toast(`导入失败：${err.message}`, true);
              return true;
            }
          },
        },
      ],
    });
  } catch (err) {
    toast(`读取本机凭据失败：${err.message}`, true);
  }
}

function zcAddManual() {
  openModal({
    title: "粘贴 JWT / API Key",
    hint: "只提交一次；后端加密入库，面板不回显。",
    body: `<div class="grid">
      <label>名称<input type="text" data-zc-field="name" placeholder="zcode-account"></label>
      <label>JWT / API Key<textarea rows="4" data-zc-field="token" spellcheck="false"></textarea></label>
    </div>`,
    actions: [
      { label: "取消", className: "ghost" },
      {
        label: "加入账号池",
        className: "primary",
        onClick: async (root) => {
          const name = (root.querySelector('[data-zc-field="name"]')?.value || "").trim();
          const token = (root.querySelector('[data-zc-field="token"]')?.value || "").trim();
          if (!token) {
            toast("请粘贴 JWT 或 API Key", true);
            return true;
          }
          try {
            const result = await postJson("/api/zcode/accounts", { name: name || "zcode-account", token });
            toast(result.duplicate ? "该账号已在池中" : "账号已加入");
            await loadZcode();
            return false;
          } catch (err) {
            toast(`加入失败：${err.message}`, true);
            return true;
          }
        },
      },
    ],
  });
}

async function zcOAuthStart() {
  const hint = $("#zcode-action-hint");
  try {
    if (ZC.oauthTimer) clearInterval(ZC.oauthTimer);
    const start = await postJson("/api/zcode/oauth/start", {});
    if (hint) hint.textContent = `已打开授权链接；等待授权（flow ${start.flowId.slice(0, 8)}…）`;
    if (start.authorizeUrl) window.open(start.authorizeUrl, "_blank", "noopener");
    let polls = 0;
    ZC.oauthTimer = setInterval(async () => {
      polls += 1;
      if (polls > 80) {
        clearInterval(ZC.oauthTimer);
        ZC.oauthTimer = null;
        if (hint) hint.textContent = "OAuth 等待超时，请重试。";
        return;
      }
      try {
        const data = await api(`/api/zcode/oauth/poll/${encodeURIComponent(start.flowId)}`);
        if (data.status === "ready") {
          clearInterval(ZC.oauthTimer);
          ZC.oauthTimer = null;
          toast(data.duplicate ? "该账号已在池中" : "OAuth 账号已入池");
          if (hint) hint.textContent = "";
          await loadZcode();
        } else if (data.status === "expired" || data.status === "failed") {
          clearInterval(ZC.oauthTimer);
          ZC.oauthTimer = null;
          if (hint) hint.textContent = `OAuth ${data.status}`;
        }
      } catch (err) {
        clearInterval(ZC.oauthTimer);
        ZC.oauthTimer = null;
        if (hint) hint.textContent = `OAuth 轮询失败：${err.message}`;
      }
    }, 2500);
  } catch (err) {
    if (hint) hint.textContent = `OAuth 启动失败：${err.message}`;
    toast(`OAuth 启动失败：${err.message}`, true);
  }
}

function zcShowReports(title, reports) {
  const rows = (reports || []).map((r) => `<tr>
    <td>${esc(r.accountName || r.accountId || "—")}</td>
    <td>${esc(r.planName || r.planId || "—")}</td>
    <td><span class="badge ${r.ok ? "ok" : (r.code === 1005 ? "warn" : "bad")}">${r.ok ? "成功" : `失败${r.code != null ? ` (${r.code})` : ""}`}</span></td>
    <td>${esc(r.message || (r.ok ? "已领取" : "—"))}</td>
    <td>${esc(r.nextAt ? zcTime(r.nextAt) : "—")}</td>
  </tr>`).join("");
  openModal({
    title,
    wide: true,
    body: `<div class="table-wrap"><table><thead><tr><th>账号</th><th>套餐</th><th>结果</th><th>说明</th><th>下次</th></tr></thead><tbody>${rows || `<tr><td colspan="5" class="empty">没有可领取的账号</td></tr>`}</tbody></table></div>`,
    actions: [{ label: "关闭", className: "ghost" }],
  });
}

async function zcClaimPreview() {
  const hint = $("#zcode-action-hint");
  if (hint) hint.textContent = "正在预览可领套餐…";
  try {
    const data = await api("/api/zcode/claim/preview");
    if (hint) hint.textContent = "";
    const rows = (data.accounts || []).map((a) => {
      const plans = (a.plans || []).map((p) => `${p.name || p.planId}${p.priority != null ? ` (p${p.priority})` : ""}`).join("、") || "无";
      return `<tr><td>${esc(a.accountName)}</td><td>${esc(a.status)}</td><td>${esc(plans)}</td><td>${esc(a.error || "—")}</td></tr>`;
    }).join("");
    openModal({
      title: "可领套餐预览",
      wide: true,
      hint: "preview 只读，不消耗领取次数。",
      body: `<div class="table-wrap"><table><thead><tr><th>账号</th><th>状态</th><th>套餐</th><th>错误</th></tr></thead><tbody>${rows || `<tr><td colspan="4" class="empty">没有可用账号</td></tr>`}</tbody></table></div>`,
      actions: [{ label: "关闭", className: "ghost" }],
    });
  } catch (err) {
    if (hint) hint.textContent = "";
    toast(`预览失败：${err.message}`, true);
  }
}

async function zcClaimNow() {
  const hint = $("#zcode-action-hint");
  if (hint) hint.textContent = "正在领取：可能需要启动浏览器求解验证码…";
  try {
    const data = await postJson("/api/zcode/claim", {});
    if (hint) hint.textContent = "";
    zcShowReports(`领取结果（成功 ${data.claimed ?? 0} / ${(data.reports || []).length}）`, data.reports || []);
    await loadZcode();
  } catch (err) {
    if (hint) hint.textContent = "";
    toast(`领取失败：${err.message}`, true);
  }
}

async function zcCaptchaTest() {
  const hint = $("#zcode-captcha-hint");
  if (hint) hint.textContent = "求解中（首次会启动无头浏览器）…";
  try {
    const data = await postJson("/api/zcode/captcha/test", {});
    if (hint) hint.textContent = `成功：${data.solvedMs} ms，参数长度 ${data.paramLength}，区域 ${data.region}`;
  } catch (err) {
    if (hint) hint.textContent = `失败：${err.message}`;
  }
}

async function zcAttach() {
  try {
    const data = await postJson("/api/zcode/attach", {
      models: ["GLM-5.3-Flash", "GLM-5.3"],
    });
    toast(`已接入 ${data.upstream || "zcode"}（${data.modelCount ?? 0} 个模型）`);
    await loadZcode();
  } catch (err) {
    toast(`接入失败：${err.message}`, true);
  }
}

async function zcAccountAction(action, id) {
  const account = ZC.accounts.find((a) => a.id === id);
  if (!account) return;
  try {
    if (action === "refresh") {
      await postJson(`/api/zcode/accounts/${encodeURIComponent(id)}/refresh`, {});
      toast("额度已刷新");
    } else if (action === "toggle") {
      await api(`/api/zcode/accounts/${encodeURIComponent(id)}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ enabled: !account.enabled }),
      });
      toast(account.enabled ? "账号已停用" : "账号已启用");
    } else if (action === "delete") {
      openModal({
        title: "删除 ZCode 账号",
        hint: `确认删除「${account.name}」？该账号的领取记录会保留。`,
        actions: [
          { label: "取消", className: "ghost" },
          {
            label: "删除",
            className: "primary",
            onClick: async () => {
              await api("/api/zcode/accounts", {
                method: "DELETE",
                headers: { "content-type": "application/json" },
                body: JSON.stringify({ ids: [id] }),
              });
              toast("账号已删除");
              await loadZcode();
              return false;
            },
          },
        ],
      });
      return;
    }
    await loadZcode();
  } catch (err) {
    toast(`操作失败：${err.message}`, true);
  }
}

function zcBind() {
  $("#zcode-refresh")?.addEventListener("click", loadZcode);
  $("#zcode-save")?.addEventListener("click", zcSaveConfig);
  $("#zcode-captcha-test")?.addEventListener("click", zcCaptchaTest);
  $("#zcode-import-local")?.addEventListener("click", zcImportLocalInfo);
  $("#zcode-add-manual")?.addEventListener("click", zcAddManual);
  $("#zcode-oauth-start")?.addEventListener("click", zcOAuthStart);
  $("#zcode-claim-preview")?.addEventListener("click", zcClaimPreview);
  $("#zcode-claim-now")?.addEventListener("click", zcClaimNow);
  $("#zcode-attach")?.addEventListener("click", zcAttach);
  $("#zcode-account-table")?.addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-zc-action]");
    if (!btn) return;
    const row = btn.closest("[data-zc-id]");
    if (!row) return;
    zcAccountAction(btn.dataset.zcAction, row.dataset.zcId);
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", zcBind);
} else {
  zcBind();
}
