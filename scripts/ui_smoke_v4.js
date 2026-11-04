#!/usr/bin/env node
/*
 * Model-Gate v4 控制台冒烟测试（无 npm 依赖，直接用 Chrome/Edge CDP）。
 *
 * 用法：
 *   node scripts/ui_smoke_v4.js --base http://127.0.0.1:8913
 *   node scripts/ui_smoke_v4.js --base http://127.0.0.1:8913 --screenshot-dir .tmp/v4-shots
 *
 * 前置：base 指向一个已启动的 Model-Gate 实例（/ui/ 即控制台）。
 * 覆盖：页面无 JS 异常、五项一级导航（用量/会话/接入/插件/设置）、用量页
 *       （范围/KPI/统计卡/明细 + localStorage 记忆）、命令面板、会话页三页签、
 *       接入卡片与详情抽屉、次级页签（模型/定价/凭据池/免费来源/模板）、
 *       插件包管理与 ZCode 面板、路由与入口、设置与校验、维护页日志诊断、
 *       g→快捷键、窄屏布局与截图。
 *
 * 说明：冒烟按 4.3 新信息架构断言；干净实例（无请求、promptCapture=false）
 *       与有数据实例都必须通过，空态与数据态同等校验。
 */
"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");

if (typeof WebSocket === "undefined") {
  console.error("需要 Node 22+（内置 WebSocket）");
  process.exit(2);
}

const argv = process.argv.slice(2);
const getArg = (name, fallback = "") => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};
const base = getArg("base", "http://127.0.0.1:8913").replace(/\/+$/, "");
const debugPort = Number(getArg("debug-port", "19233"));
const screenshotDir = getArg("screenshot-dir", path.join(".tmp", "v4-shots"));
const chromeCandidates = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
];
const chromePath = getArg(
  "chrome",
  process.env.CHROME_PATH || chromeCandidates.find((item) => fs.existsSync(item)) || ""
);

const failures = [];
const passes = [];
let cdp = null;
let chrome = null;
let userDataDir = "";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function pass(name, detail = "") {
  passes.push(name);
  console.log(`[ok] ${name}${detail ? ` · ${detail}` : ""}`);
}

function fail(name, error) {
  failures.push(`${name}: ${error && error.message ? error.message : error}`);
  console.error(`[FAIL] ${name} -> ${error && error.message ? error.message : error}`);
}

async function check(name, fn) {
  try {
    pass(name, (await fn()) || "");
  } catch (error) {
    fail(name, error);
  }
}

async function waitFor(fn, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const value = await fn();
      if (value) return value;
    } catch {
      /* 重试 */
    }
    await sleep(100);
  }
  throw new Error(`${label || "等待条件"}超时（${timeoutMs}ms）`);
}

async function fetchJson(url, timeoutMs = 3000) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  return { status: response.status, body: await response.json().catch(() => null) };
}

function connectCdp(wsUrl) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(wsUrl);
    const pending = new Map();
    const listeners = new Map();
    let nextId = 1;
    const client = {
      send(method, params = {}) {
        return new Promise((res, rej) => {
          const id = nextId++;
          pending.set(id, { res, rej });
          socket.send(JSON.stringify({ id, method, params }));
        });
      },
      on(method, fn) {
        if (!listeners.has(method)) listeners.set(method, []);
        listeners.get(method).push(fn);
      },
      once(method, fn) {
        const wrapper = (params) => {
          client.off(method, wrapper);
          fn(params);
        };
        client.on(method, wrapper);
      },
      off(method, fn) {
        const list = listeners.get(method);
        if (!list) return;
        const index = list.indexOf(fn);
        if (index >= 0) list.splice(index, 1);
      },
      close() {
        try {
          socket.close();
        } catch {
          /* ignore */
        }
      },
    };
    socket.addEventListener("open", () => resolve(client));
    socket.addEventListener("error", (event) =>
      reject(new Error(`CDP 连接失败：${event.message || "error"}`))
    );
    socket.addEventListener("message", (event) => {
      let message;
      try {
        message = JSON.parse(String(event.data));
      } catch {
        return;
      }
      if (message.id && pending.has(message.id)) {
        const { res, rej } = pending.get(message.id);
        pending.delete(message.id);
        if (message.error) rej(new Error(`${message.error.message} (${message.error.code})`));
        else res(message.result);
        return;
      }
      if (message.method && listeners.has(message.method)) {
        for (const fn of listeners.get(message.method).slice()) {
          try {
            fn(message.params);
          } catch {
            /* ignore */
          }
        }
      }
    });
  });
}

async function main() {
  const health = await fetchJson(`${base}/health`, 3000).catch(() => ({ status: 0 }));
  if (health.status !== 200) throw new Error(`服务未就绪：${base}/health -> ${health.status}`);

  const candidates = [];
  for (const candidate of [chromePath, ...chromeCandidates]) {
    if (candidate && fs.existsSync(candidate) && !candidates.includes(candidate)) {
      candidates.push(candidate);
    }
  }
  if (!candidates.length) throw new Error(`找不到 Chrome/Edge：${chromePath}`);

  let activeDebugPort = debugPort;
  for (let attempt = 0; attempt < candidates.length; attempt++) {
    const executable = candidates[attempt];
    activeDebugPort = debugPort + attempt;
    userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "mg-v4-smoke-"));
    chrome = spawn(
      executable,
      [
        "--headless=new",
        "--disable-gpu",
        "--disable-extensions",
        "--no-first-run",
        "--no-default-browser-check",
        "--remote-allow-origins=*",
        `--remote-debugging-port=${activeDebugPort}`,
        `--user-data-dir=${userDataDir}`,
        "--window-size=1440,1000",
        "about:blank",
      ],
      { stdio: ["ignore", "ignore", "pipe"], windowsHide: true }
    );
    let chromeErr = "";
    chrome.stderr.on("data", (chunk) => {
      chromeErr += String(chunk);
    });

    try {
      await waitFor(async () => {
        try {
          return (await fetchJson(`http://127.0.0.1:${activeDebugPort}/json/version`, 1000)).status === 200;
        } catch {
          return false;
        }
      }, 30000, "Chrome 调试端口");
      if (chrome.exitCode !== null) throw new Error(`Chrome 提前退出：${chromeErr.slice(-1200)}`);
      break;
    } catch (err) {
      try { chrome.kill(); } catch {}
      chrome = null;
      if (attempt === candidates.length - 1) {
        throw new Error(`Chrome 调试端口未就绪：${executable}: ${err.message}`);
      }
      console.log(`[warn] ${executable} 启动失败，尝试下一个浏览器`);
    }
  }

  const created = await fetch(
    `http://127.0.0.1:${activeDebugPort}/json/new?${encodeURIComponent("about:blank")}`,
    { method: "PUT" }
  );
  const target = await created.json();
  if (!target.webSocketDebuggerUrl) throw new Error("未能创建标签页");
  cdp = await connectCdp(target.webSocketDebuggerUrl);

  const errors = [];
  const record = (text) => {
    if (!text || /favicon\.ico/i.test(text)) return;
    errors.push(text);
  };
  cdp.on("Runtime.exceptionThrown", (params) => {
    const details = params.exceptionDetails || {};
    record(`exception: ${details.exception?.description || details.text || "unknown"}`);
  });
  cdp.on("Runtime.consoleAPICalled", (params) => {
    if (params.type !== "error") return;
    record(`console: ${(params.args || []).map((arg) => arg.value ?? arg.description ?? "").join(" ")}`);
  });
  cdp.on("Log.entryAdded", (params) => {
    const entry = params.entry || {};
    if (entry.level === "error") record(`log: ${entry.text}${entry.url ? ` (${entry.url})` : ""}`);
  });
  cdp.on("Page.javascriptDialogOpening", () => {
    cdp.send("Page.handleJavaScriptDialog", { accept: true }).catch(() => {});
  });

  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await cdp.send("Log.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: 1440,
    height: 1000,
    deviceScaleFactor: 1,
    mobile: false,
  });

  const evaluate = async (expression) => {
    const result = await cdp.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    });
    if (result.exceptionDetails) {
      throw new Error(
        result.exceptionDetails.exception?.description ||
          result.exceptionDetails.text ||
          "页面脚本执行失败"
      );
    }
    return result.result ? result.result.value : undefined;
  };

  const click = async (selector) => {
    const clicked = await evaluate(`(() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return false;
      el.scrollIntoView({ block: "center", inline: "center" });
      el.click();
      return true;
    })()`);
    if (!clicked) throw new Error(`找不到可点击元素：${selector}`);
    await sleep(200);
  };

  const navigate = async (view) => {
    await click(`.nav-item[data-view="${view}"]`);
    await waitFor(
      () => evaluate(`document.querySelector("#view-${view}")?.classList.contains("active")`),
      4000,
      `视图 ${view}`
    );
  };

  const screenshot = async (name) => {
    const dir = path.resolve(screenshotDir);
    fs.mkdirSync(dir, { recursive: true });
    const result = await cdp.send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: false,
    });
    const file = path.join(dir, `${name}.png`);
    fs.writeFileSync(file, Buffer.from(result.data, "base64"));
    return file;
  };

  const loaded = new Promise((resolve) => cdp.once("Page.loadEventFired", resolve));
  await cdp.send("Page.navigate", { url: `${base}/ui/` });
  await Promise.race([loaded, sleep(12000).then(() => { throw new Error("页面加载超时"); })]);
  await waitFor(
    () => evaluate(`document.documentElement.dataset.ui === "v4" && !!document.querySelector("#view-usage.active")`),
    15000,
    "v4 前端初始化"
  );
  await sleep(800);

  await check("页面加载无 JS 异常", async () => {
    if (errors.length) throw new Error(errors.slice(0, 3).join(" | "));
    return "0 个错误";
  });

  await check("一级导航 5 项（用量/会话/接入/插件/设置）", async () => {
    const data = await evaluate(`({
      views: Array.from(document.querySelectorAll(".nav-item")).map((b) => b.dataset.view),
      labels: document.querySelectorAll(".nav-label").length,
    })`);
    const expected = ["usage", "sessions", "access", "plugins", "settings"];
    if (JSON.stringify(data.views) !== JSON.stringify(expected)) {
      throw new Error(`导航异常：${data.views.join("/")}`);
    }
    if (data.labels !== 0) throw new Error(`不应再有侧栏分组标签：${data.labels}`);
    return "5 项 / 顶部单行导航";
  });

  await check("用量页：范围 / KPI / 统计卡 / 明细", async () => {
    await waitFor(() => evaluate(`document.querySelectorAll("#usage-kpis > div").length >= 5`), 8000, "用量 KPI");
    const data = await evaluate(`({
      kpis: document.querySelectorAll("#usage-kpis > div").length,
      ranges: document.querySelectorAll("#usage-range [data-usage-range]").length,
      statModels: (document.querySelector("#usage-stat-models")?.textContent || "").trim().length,
      statUpstreams: (document.querySelector("#usage-stat-upstreams")?.textContent || "").trim().length,
      detailRows: document.querySelectorAll("#usage-detail-table tbody tr").length,
      detailEmpty: !!document.querySelector("#usage-detail-table .empty-line"),
    })`);
    if (data.ranges !== 6) throw new Error(`时间范围档位异常：${data.ranges}`);
    if (!data.statModels || !data.statUpstreams) throw new Error("统计卡为空（既无数据也无空态）");
    if (!data.detailRows && !data.detailEmpty) throw new Error("明细既没有行也没有空态");
    await screenshot("v4-01-usage");
    return `${data.kpis} KPI / ${data.ranges} 档范围 / ${data.detailRows || "空态"} 明细`;
  });

  await check("命令面板（⌘K）可搜索并跳转", async () => {
    await click("#global-search");
    await waitFor(() => evaluate(`!!document.querySelector("#v4-cmdk") && !document.querySelector("#v4-cmdk").hidden`), 3000, "命令面板");
    await evaluate(`(() => {
      const input = document.querySelector("#v4-cmdk-input");
      input.value = "接入";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    await waitFor(() => evaluate(`document.querySelectorAll("#v4-cmdk-list [data-cmdk]").length > 0`), 3000, "命令面板结果");
    const n = await evaluate(`document.querySelectorAll("#v4-cmdk-list [data-cmdk]").length`);
    await screenshot("v4-02-cmdk");
    await evaluate(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }))`);
    await waitFor(() => evaluate(`document.querySelector("#v4-cmdk").hidden`), 3000, "关闭命令面板");
    return `${n} 条结果`;
  });

  await check("会话页：三页签与列表（空态/数据态）", async () => {
    await navigate("sessions");
    await waitFor(() => evaluate(`document.querySelectorAll("#ses-tabs .seg-tab").length === 3`), 6000, "会话页签");
    await waitFor(
      () => evaluate(`document.querySelectorAll("#ses-list .ses-item").length > 0 || !!document.querySelector("#ses-list .empty-line")`),
      8000,
      "会话列表"
    );
    const data = await evaluate(`({
      items: document.querySelectorAll("#ses-list .ses-item").length,
      empty: (document.querySelector("#ses-list .empty-line")?.textContent || "").trim(),
      lanes: document.querySelectorAll("#ses-lanes .ses-lane").length,
      traceVisible: !document.getElementById("ses-pane-trace").hidden,
    })`);
    for (const tab of ["diff", "tools"]) {
      await click(`#ses-tabs [data-ses-tab="${tab}"]`);
      await waitFor(() => evaluate(`!document.querySelector("#ses-pane-${tab}").hidden`), 4000, `页签 ${tab}`);
    }
    await click(`#ses-tabs [data-ses-tab="trace"]`);
    await screenshot("v4-03-sessions");
    return `${data.items || data.empty} / ${data.lanes || 0} 条 lanes / 三页签可切换`;
  });

  await check("接入页：卡片网格与详情抽屉", async () => {
    await navigate("access");
    await waitFor(() => evaluate(`document.querySelectorAll("#access-cards .acc-card").length >= 1`), 8000, "接入卡片");
    const data = await evaluate(`({
      cards: document.querySelectorAll("#access-cards .acc-card").length,
      tabs: document.querySelectorAll("#access-tabs .seg-tab").length,
    })`);
    if (data.tabs !== 5) throw new Error(`次级页签数量异常：${data.tabs}`);
    await evaluate(`document.querySelector("#access-cards .acc-card").click()`);
    await waitFor(() => evaluate(`!document.querySelector("#drawer").hidden`), 4000, "接入详情抽屉");
    const tabs = await evaluate(`Array.from(document.querySelectorAll(".drawer-tab")).map((b) => b.dataset.dtab)`);
    if (tabs.length < 6) throw new Error(`抽屉页签不足：${tabs.join("/")}`);
    await click('.drawer-tab[data-dtab="connection"]');
    await click('.drawer-tab[data-dtab="models"]');
    await click('.drawer-tab[data-dtab="advanced"]');
    // 查询清单成功路径（zen 上游，transport=opencode）：断言勾选网格渲染且
    // 已声明模型（big-pickle）默认勾上。冒烟配置可能只有演示上游（CI），
    // 没有 zen 卡片时跳过这段——该成功路径由带 zen 的本地冒烟覆盖。
    const hasZen = await evaluate(`!!document.querySelector('[data-open="zen"]')`);
    let fetchDetail = "无 zen 上游，跳过查询清单";
    if (hasZen) {
      await evaluate(`document.querySelector('[data-open="zen"]').click()`);
      await waitFor(() => evaluate(`!document.querySelector("#drawer").hidden`), 4000, "zen 抽屉");
      await click('.drawer-tab[data-dtab="models"]');
      await click("#fetch-upstream-models");
      await waitFor(() => evaluate(`document.querySelectorAll("#fetched-model-grid input").length > 0`), 15000, "查询清单勾选网格");
      const fetched = await evaluate(`({
        count: document.querySelectorAll("#fetched-model-grid input").length,
        bigPickleChecked: document.querySelector('#fetched-model-grid input[data-fetched-model="big-pickle"]')?.checked,
        meta: (document.querySelector("#fetch-models-meta")?.textContent || "").trim(),
      })`);
      if (!fetched.bigPickleChecked) throw new Error(`已声明模型未默认勾选：${JSON.stringify(fetched)}`);
      await screenshot("v4-04b-drawer-fetch-models");
      fetchDetail = `查询清单 ${fetched.count} 个 · big-pickle 默认勾选`;
    }
    await evaluate(`document.querySelector("#drawer-close").click()`);
    await waitFor(() => evaluate(`document.querySelector("#drawer").hidden`), 3000, "关闭抽屉");
    return `${data.cards} 张接入卡 / 页签 ${tabs.join("·")} / ${fetchDetail}`;
  });

  await check("用量筛选刷新后保留（localStorage）", async () => {
    await evaluate(`document.querySelector('#usage-range [data-usage-range="30d"]').click()`);
    await waitFor(
      () => evaluate(`document.querySelector('#usage-range [data-usage-range].active')?.dataset.usageRange === "30d"`),
      3000,
      "切换 30 天"
    );
    await sleep(900);
    const reloaded = new Promise((resolve) => cdp.once("Page.loadEventFired", resolve));
    await cdp.send("Page.reload", { ignoreCache: true });
    await Promise.race([reloaded, sleep(12000).then(() => { throw new Error("刷新超时"); })]);
    await waitFor(() => evaluate(`document.documentElement.dataset.ui === "v4"`), 15000, "刷新后初始化");
    await navigate("usage");
    await sleep(500);
    const value = await evaluate(
      `document.querySelector('#usage-range [data-usage-range].active')?.dataset.usageRange`
    );
    if (value !== "30d") throw new Error(`刷新后时间范围未保留：${value}`);
    // 复位到「今天」，避免影响后续断言的可读性。
    await evaluate(`document.querySelector('#usage-range [data-usage-range="today"]').click()`);
    await sleep(600);
    return `刷新后 range=${value}`;
  });

  await check("插件页：包管理列表与内置库", async () => {
    await navigate("plugins");
    // 内置免费插件就 kilo/zen 两条；CI 冷启动首帧可能慢，放宽到 10s。
    await waitFor(() => evaluate(`document.querySelectorAll("#plugin-list .plugin-card").length >= 2`), 10000, "插件卡片");
    const data = await evaluate(`({
      plugins: document.querySelectorAll("#plugin-list .plugin-card").length,
      libraryHidden: document.getElementById("plugin-library-wrap")?.hidden,
      library: document.querySelectorAll("#plugin-library .plugin-card").length,
      zcodeEntry: !!document.querySelector("[data-open-zcode]") || !!document.querySelector("#zcode-panel"),
    })`);
    // 回归守卫：ZCode 账号池已在 4.5.0 整体移除，入口与专属面板不得复活。
    if (data.zcodeEntry) throw new Error("ZCode 账号池入口/面板不应存在");
    await screenshot("v4-06-plugins");
    return `插件 ${data.plugins} / 内置库 ${data.libraryHidden ? "隐藏" : data.library}`;
  });

  await check("插件页：免费插件「模型 · 检测」入口与弹窗", async () => {
    await navigate("plugins");
    await waitFor(() => evaluate(`document.querySelectorAll("#plugin-list .plugin-card").length >= 2`), 10000, "插件卡片");
    // kilo/zen 卡片必须带「模型 · 检测」；ZCode 账号池已移除，不应再有免费模型目录入口。
    const buttons = await evaluate(`({
      kilo: !!document.querySelector('[data-plugin-models="modelgate.free.kilo"]'),
      zen: !!document.querySelector('[data-plugin-models="modelgate.free.opencode"]'),
      zcode: !!document.querySelector('[data-plugin-models="modelgate.account.zcode"]'),
    })`);
    if (!buttons.kilo || !buttons.zen) throw new Error(`Kilo/Zen 缺少「模型 · 检测」按钮：${JSON.stringify(buttons)}`);
    if (buttons.zcode) throw new Error("账号池插件不应提供免费模型目录入口");
    // 像真实用户一样点 zen 卡片按钮打开弹窗（kilo 目录在本机可能不可达，
    // 点按钮走绑定链路：拉取失败会 toast，等待「弹窗或提示」二选一即可）。
    await click('[data-plugin-models="modelgate.free.opencode"]');
    await waitFor(
      () => evaluate(`!!document.querySelector("#pm-table") || (document.querySelector("#toast") && !document.querySelector("#toast").hidden)`),
      12000,
      "模型弹窗或错误提示"
    );
    const modal = await evaluate(`({
      open: !document.querySelector("#modal").hidden && !!document.querySelector("#pm-table"),
      rows: document.querySelectorAll("#pm-table tbody tr").length,
      count: (document.querySelector("#pm-count")?.textContent || "").trim(),
    })`);
    await screenshot("v4-06b-plugin-models");
    await evaluate(`document.querySelector("#modal-close")?.click()`);
    if (!modal.open) return "目录拉取失败（网络/上游），入口与触发链路正常";
    return `zen 弹窗：${modal.count || "0 个候选"} / ${modal.rows} 行`;
  });

  await check("设置：子导航、未保存提示与配置校验", async () => {
    await navigate("settings");
    await waitFor(() => evaluate(`document.querySelectorAll("#settings-subnav [data-settings-pane]").length === 6`), 4000, "设置子导航");
    await waitFor(() => evaluate(`!!document.querySelector("#set-host")?.value`), 6000, "设置表单");
    const host = await evaluate(`document.querySelector("#set-host").value`);
    await evaluate(`(() => {
      const input = document.querySelector("#set-timeout");
      input.value = String(Number(input.value || 600) + 1);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    const dirty = await evaluate(`!document.querySelector("#settings-dirty").hidden`);
    await click(`#settings-subnav [data-settings-pane="update"]`);
    await waitFor(() => evaluate(`!document.querySelector("#spane-update").hidden`), 4000, "更新子页");
    await click("#settings-validate");
    await waitFor(() => evaluate(`(document.querySelector("#settings-issues")?.textContent || "").trim().length > 0`), 8000, "配置校验结果");
    const issues = await evaluate(`document.querySelector("#settings-issues").textContent.trim()`);
    await screenshot("v4-07-settings");
    if (!host) throw new Error("监听地址为空");
    if (!dirty) throw new Error("修改后未出现未保存提示");
    return `${host} / 校验：${issues.slice(0, 24)}`;
  });

  await check("维护页：运行日志 + 诊断分组", async () => {
    await click(`#settings-subnav [data-settings-pane="maintain"]`);
    await waitFor(() => evaluate(`(document.querySelector("#runtime-viewer")?.textContent || "").trim().length > 0`), 8000, "运行日志渲染");
    const meta = await evaluate(`(document.querySelector("#runtime-meta")?.textContent || "").trim()`);
    if (!/\d+ \/ \d+ 行/.test(meta)) throw new Error(`日志元信息格式异常：${meta}`);
    await evaluate(`(() => {
      const viewer = document.querySelector("#runtime-viewer");
      viewer.scrollTop = 0;
      document.querySelector("#runtime-bottom").click();
      return true;
    })()`);
    const atBottom = await evaluate(`(() => {
      const viewer = document.querySelector("#runtime-viewer");
      return viewer.scrollHeight <= viewer.clientHeight || viewer.scrollTop + viewer.clientHeight >= viewer.scrollHeight - 4;
    })()`);
    if (!atBottom) throw new Error("「跳到末尾」未把日志视图滚到底部");
    await click(`#observe-tabs [data-observe-tab="diag"]`);
    await waitFor(() => evaluate(`!document.querySelector("#observe-diag").hidden`), 4000, "诊断视图");
    await waitFor(
      () => evaluate(`document.querySelectorAll("#diag-groups .diag-group .diag-item").length > 0`),
      6000,
      "诊断分组"
    );
    const groups = await evaluate(`document.querySelectorAll("#diag-groups .diag-group").length`);
    await screenshot("v4-08-maintain");
    return `${meta} / ${groups} 组诊断`;
  });

  await check("快捷键 g→u / g→a 跳转", async () => {
    // 先关掉可能残留的浮层（命令面板 / 抽屉 / 模态），否则快捷键按设计会被屏蔽。
    await evaluate(`(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      const modal = document.querySelector("#modal");
      if (modal && !modal.hidden) document.querySelector("#modal-close").click();
      return true;
    })()`);
    await sleep(300);
    const pressG = () => evaluate(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "g", bubbles: true }))`);
    const press = (key) => evaluate(`document.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(key)}, bubbles: true }))`);
    await pressG();
    await press("u");
    await sleep(600);
    const usage = await evaluate(`document.querySelector("#view-usage")?.classList.contains("active")`);
    await pressG();
    await press("a");
    await sleep(600);
    const access = await evaluate(`document.querySelector("#view-access")?.classList.contains("active")`);
    if (!usage || !access) throw new Error(`快捷键跳转失败（usage=${usage}, access=${access}）`);
    return "g→u / g→a 均生效";
  });

  await check("窄屏（390px）布局不横向溢出", async () => {
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: 390,
      height: 844,
      deviceScaleFactor: 1,
      mobile: true,
    });
    await navigate("usage");
    // 大库冷缓存下首次聚合要数秒；本断言测的是布局而非 API 延迟，
    // 显式等 KPI 出现再量溢出，避免把后端速度误判成布局失败。
    await waitFor(
      () => evaluate(`document.querySelectorAll("#usage-kpis > div").length > 0`),
      12000,
      "窄屏用量 KPI"
    );
    const metrics = await evaluate(`({
      inner: window.innerWidth,
      scroll: document.documentElement.scrollWidth,
      kpis: document.querySelectorAll("#usage-kpis > div").length,
      navVisible: document.querySelectorAll(".nav-item").length,
    })`);
    await screenshot("v4-09-mobile");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: 1440,
      height: 1000,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await sleep(300);
    if (metrics.scroll > metrics.inner + 12) {
      throw new Error(`窄屏横向溢出：scrollWidth=${metrics.scroll} > innerWidth=${metrics.inner}`);
    }
    if (!metrics.kpis) throw new Error("窄屏下用量 KPI 未渲染");
    return `${metrics.inner}px 视口 / ${metrics.kpis} KPI / 横向 ${metrics.scroll}px`;
  });

  await check("全流程无 JS 异常", async () => {
    if (errors.length) throw new Error(errors.slice(0, 5).join(" | "));
    return "0 个错误";
  });

  console.log(`\n结果：${passes.length} 通过，${failures.length} 失败`);
  if (failures.length) {
    for (const failure of failures) console.error(` - ${failure}`);
    process.exitCode = 1;
  }
}

main()
  .catch((error) => {
    console.error(`[FATAL] ${error.message}`);
    process.exitCode = 1;
  })
  .finally(async () => {
    if (cdp) cdp.close();
    if (chrome) {
      try {
        chrome.kill();
      } catch {
        /* ignore */
      }
    }
    if (userDataDir) {
      await sleep(300);
      try {
        fs.rmSync(userDataDir, { recursive: true, force: true });
      } catch {
        /* ignore */
      }
    }
  });
