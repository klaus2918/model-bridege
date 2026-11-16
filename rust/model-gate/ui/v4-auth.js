/* v4 接入认证面板：验证开关 / IP 白名单 / 认证码（生成与吊销）。依赖 window.MG。
   认证码明文只在生成弹窗里出现一次；之后任何界面都不再显示。 */
(() => {
  "use strict";
  const MG = window.MG;
  const { $, esc, num, api, toast, openModal } = MG;

  const auth = { enabled: true, ipAllowlist: [], keys: [], keysMax: 32, loaded: false };

  async function loadAuth() {
    const data = await api("/api/auth");
    applyAuth(data.auth || {});
    renderAuth();
  }

  function applyAuth(view) {
    auth.enabled = view.enabled !== false;
    auth.ipAllowlist = Array.isArray(view.ipAllowlist) ? view.ipAllowlist.slice() : [];
    auth.keys = Array.isArray(view.keys) ? view.keys.slice() : [];
    if (view.keysMax) auth.keysMax = view.keysMax;
  }

  function renderAuth() {
    $("#auth-state-pill").textContent = auth.enabled ? "已开启" : "已关闭";
    $("#auth-state-pill").className = `pill ${auth.enabled ? "signal" : "amber"}`;
    const checkbox = $("#auth-enabled");
    if (checkbox) checkbox.checked = auth.enabled;

    $("#auth-allowlist").innerHTML = auth.ipAllowlist.length
      ? auth.ipAllowlist
          .map(
            (ip) =>
              `<span class="chip">${esc(ip)} <button class="icon-btn danger" data-auth-ip="${esc(ip)}" title="移除">×</button></span>`,
          )
          .join(" ")
      : `<span class="hint">白名单为空：拒绝一切直连来源，只认认证码</span>`;

    $("#auth-key-count").textContent = `${num(auth.keys.length)} 条${auth.keys.length >= auth.keysMax ? "（已达上限）" : ""}`;
    const tbody = $("#auth-key-table tbody");
    tbody.innerHTML = auth.keys
      .map(
        (key) => `<tr>
          <td>${key.name ? esc(key.name) : '<span class="hint">未命名</span>'}</td>
          <td class="mono">${esc(key.prefix)}…</td>
          <td class="mono">${esc((key.createdAt || "").replace("T", " ").slice(0, 19))}</td>
          <td class="right"><button class="btn ghost sm danger" data-auth-revoke="${esc(key.id)}">吊销</button></td>
        </tr>`,
      )
      .join("");
    $("#auth-key-empty").hidden = auth.keys.length > 0;
    const generate = $("#auth-key-generate");
    if (generate) generate.disabled = auth.keys.length >= auth.keysMax;
  }

  function showKeyModal(plain, record) {
    openModal(
      "认证码已生成",
      "明文仅此一次展示，请立即复制保存",
      `<div class="form-grid">
        <div class="field"><span>名称</span><div class="mono">${esc(record?.name || "未命名")}</div></div>
        <div class="field"><span>认证码</span><textarea class="input mono" id="auth-plain" rows="3" readonly>${esc(plain)}</textarea></div>
        <div class="card-actions wrap span-2">
          <button class="btn primary sm" id="auth-plain-copy">复制认证码</button>
          <span class="hint">客户端用法：Authorization: Bearer &lt;认证码&gt; 或 x-api-key 头</span>
        </div>
      </div>`,
    );
    $("#auth-plain-copy")?.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText($("#auth-plain").value);
        toast("已复制到剪贴板", "ok");
      } catch {
        $("#auth-plain").select();
        toast("复制失败，请手动选择复制", "error");
      }
    });
  }

  async function saveEnabled() {
    const enabled = $("#auth-enabled").checked;
    const data = await api("/api/auth", { method: "PUT", body: JSON.stringify({ enabled }) });
    applyAuth(data.auth || {});
    renderAuth();
    toast(enabled ? "认证已开启" : "认证已关闭，所有来源放行", "ok");
  }

  async function saveAllowlist() {
    const data = await api("/api/auth", {
      method: "PUT",
      body: JSON.stringify({ ipAllowlist: auth.ipAllowlist }),
    });
    applyAuth(data.auth || {});
    renderAuth();
    toast("白名单已保存并生效", "ok");
  }

  async function addIp() {
    const input = $("#auth-ip-input");
    const text = (input.value || "").trim();
    if (!text) return;
    const data = await api("/api/auth", {
      method: "PUT",
      body: JSON.stringify({ ipAllowlist: [...auth.ipAllowlist, text] }),
    });
    input.value = "";
    applyAuth(data.auth || {});
    renderAuth();
    toast("已加入白名单", "ok");
  }

  async function removeIp(ip) {
    const data = await api("/api/auth", {
      method: "PUT",
      body: JSON.stringify({ ipAllowlist: auth.ipAllowlist.filter((item) => item !== ip) }),
    });
    applyAuth(data.auth || {});
    renderAuth();
    toast("已从白名单移除", "ok");
  }

  async function restoreDefault() {
    const data = await api("/api/auth", {
      method: "PUT",
      body: JSON.stringify({ ipAllowlist: ["127.0.0.1", "::1"] }),
    });
    applyAuth(data.auth || {});
    renderAuth();
    toast("已恢复默认白名单（仅本机）", "ok");
  }

  async function generateKey() {
    const name = window.prompt("认证码名称（备注用，可留空）", "") || "";
    const data = await api("/api/auth/keys", { method: "POST", body: JSON.stringify({ name }) });
    await loadAuth();
    showKeyModal(data.key || "", data.auth);
  }

  async function revokeKey(id) {
    const record = auth.keys.find((key) => key.id === id);
    const label = record?.name || record?.prefix || id;
    const lastKeyRisk =
      auth.keys.length === 1 && auth.ipAllowlist.length === 0
        ? "这是最后一条认证码且白名单为空：吊销后任何来源（包括本机面板）都无法访问，只能手改 bridge.config.json 恢复。"
        : `使用它的客户端会立刻失去访问权。`;
    if (!window.confirm(`吊销认证码「${label}」？${lastKeyRisk}`)) return;
    await api(`/api/auth/keys/${encodeURIComponent(id)}`, { method: "DELETE" });
    await loadAuth();
    toast("已吊销", "ok");
  }

  function bind() {
    $("#auth-refresh")?.addEventListener("click", () => loadAuth().catch((error) => toast(error.message, "error")));
    $("#auth-save")?.addEventListener("click", () => saveEnabled().catch((error) => toast(error.message, "error")));
    $("#auth-ip-add")?.addEventListener("click", () => addIp().catch((error) => toast(error.message, "error")));
    $("#auth-ip-input")?.addEventListener("keydown", (event) => {
      if (event.key === "Enter") addIp().catch((error) => toast(error.message, "error"));
    });
    $("#auth-allowlist-save")?.addEventListener("click", () => saveAllowlist().catch((error) => toast(error.message, "error")));
    $("#auth-allowlist-default")?.addEventListener("click", () => restoreDefault().catch((error) => toast(error.message, "error")));
    $("#auth-key-generate")?.addEventListener("click", () => generateKey().catch((error) => toast(error.message, "error")));
    $("#auth-key-table")?.addEventListener("click", (event) => {
      const button = event.target.closest("[data-auth-revoke]");
      if (button) revokeKey(button.dataset.authRevoke).catch((error) => toast(error.message, "error"));
    });
    $("#auth-allowlist")?.addEventListener("click", (event) => {
      const tag = event.target.closest("[data-auth-ip]");
      if (tag) removeIp(tag.dataset.authIp).catch((error) => toast(error.message, "error"));
    });
  }

  MG.registerView("auth", loadAuth);
  document.addEventListener("DOMContentLoaded", bind);
})();
