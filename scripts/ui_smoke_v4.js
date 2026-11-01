#!/usr/bin/env node
/*
 * Model-Gate v4 控制台冒烟测试（无 npm 依赖，直接用 Chrome/Edge CDP）。
 *
 * 用法：
 *   node scripts/ui_smoke_v4.js --base http://127.0.0.1:8913
 *   node scripts/ui_smoke_v4.js --base http://127.0.0.1:8913 --screenshot-dir .tmp/v4-shots
 *
 * 前置：base 指向一个已启动的 Model-Gate 实例（v4 为唯一入口，/ui/ 直接是 v4）。
 * 覆盖：页面无 JS 异常、六项一级导航、总览、命令面板、配置中心任务板、
 *       模型目录/单价表、统一入口与路由组、接入列表与详情抽屉、用量分析、
 *       提示词、日志与诊断、设置、凭据池/插件/免费来源/模板/ZCode、快捷键与截图。
 *
 * 说明：v4 把低频配置收进「配置中心」，因此这里的视图校验按 v4 信息架构进行，
 *       不再按 v3 的十二个一级导航逐项点击。
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
    () => evaluate(`document.documentElement.dataset.ui === "v4" && !!document.querySelector("#view-overview.active")`),
    15000,
    "v4 前端初始化"
  );
  await sleep(800);

  await check("页面加载无 JS 异常", async () => {
    if (errors.length) throw new Error(errors.slice(0, 3).join(" | "));
    return "0 个错误";
  });

  await check("一级导航 6 项 / 4 组", async () => {
    const data = await evaluate(`({
      views: Array.from(document.querySelectorAll(".nav-item")).map((b) => b.dataset.view),
      groups: Array.from(document.querySelectorAll(".nav-label")).map((el) => el.textContent.trim()),
    })`);
    const expected = ["overview", "traffic", "prompts", "access", "logs", "config"];
    for (const view of expected) {
      if (!data.views.includes(view)) throw new Error(`缺少一级导航 ${view}`);
    }
    if (data.views.length !== 6) throw new Error(`一级导航数量异常：${data.views.length}`);
    if (data.groups.length !== 4) throw new Error(`导航分组异常：${data.groups.join("/")}`);
    return `${data.views.length} 项 / ${data.groups.length} 组`;
  });

  await check("总览：状态带 + 4 KPI + 健康表 + 最近请求", async () => {
    await waitFor(() => evaluate(`document.querySelectorAll("#ov-kpis .ov-kpi").length === 4`), 8000, "总览 KPI");
    const stats = await evaluate(`({
      kpis: document.querySelectorAll("#ov-kpis .ov-kpi").length,
      health: document.querySelectorAll("#ov-health .health-row").length,
      recent: document.querySelectorAll("#ov-recent .mini-row").length,
      snapshot: (document.querySelector("#ov-snapshot")?.textContent || "").trim().length,
      hero: (document.querySelector("#ov-hero")?.textContent || "").trim(),
    })`);
    if (!stats.health) throw new Error("接入健康表为空");
    if (!stats.snapshot) throw new Error("用量快照为空");
    await screenshot("v4-01-overview");
    return `${stats.kpis} KPI / ${stats.health} 接入 / ${stats.recent} 请求 / 「${stats.hero}」`;
  });

  await check("命令面板（⌘K）可搜索并跳转", async () => {
    await click("#global-search");
    await waitFor(() => evaluate(`!!document.querySelector("#v4-cmdk") && !document.querySelector("#v4-cmdk").hidden`), 3000, "命令面板");
    await evaluate(`(() => {
      const input = document.querySelector("#v4-cmdk-input");
      input.value = "模型";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    await waitFor(() => evaluate(`document.querySelectorAll("#v4-cmdk-list [data-cmdk]").length > 0`), 3000, "命令面板结果");
    const n = await evaluate(`document.querySelectorAll("#v4-cmdk-list [data-cmdk]").length`);
    await screenshot("v4-02-cmdk");
    await evaluate(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }))`);
    await waitFor(() => evaluate(`document.querySelector("#v4-cmdk").hidden`), 3000, "关闭命令面板");
    return `${n} 条结果`;
  });

  await check("配置中心：四张域卡片 + 常用任务", async () => {
    await navigate("config");
    await waitFor(() => evaluate(`document.querySelectorAll("#config-domains [data-domain]").length === 4`), 8000, "配置域卡片");
    const data = await evaluate(`({
      domains: document.querySelectorAll("#config-domains [data-domain]").length,
      tasks: document.querySelectorAll("#config-tasks [data-task]").length,
      changes: document.querySelectorAll("#config-changes .change-row, #config-changes .row, #config-changes li").length,
    })`);
    if (!data.tasks) throw new Error("常用任务为空");
    await screenshot("v4-03-config");
    return `${data.domains} 个域 / ${data.tasks} 个任务 / ${data.changes} 条变更`;
  });

  const openConfig = async (sub, view) => {
    await navigate("config");
    await waitFor(() => evaluate(`!document.querySelector("#config-subbar").hidden`), 4000, "配置中心子导航");
    const link = `#config-subnav [data-config-sub="${sub}"]`;
    const exists = await evaluate(`!!document.querySelector(${JSON.stringify(link)})`);
    if (!exists) throw new Error(`配置中心缺少子页 ${sub}`);
    await click(link);
    await waitFor(
      () => evaluate(`document.querySelector("#view-${view}")?.classList.contains("active")`),
      6000,
      `${sub} 视图`
    );
  };

  await check("模型目录与单价表", async () => {
    await openConfig("models", "models");
    await waitFor(() => evaluate(`!document.querySelector("#model-catalog-panel").hidden`), 5000, "模型目录面板");
    await waitFor(() => evaluate(`document.querySelectorAll("#model-catalog-table tbody tr").length > 0`), 6000, "模型表格");
    const rows = await evaluate(`document.querySelectorAll("#model-catalog-table tbody tr").length`);
    const kpis = await evaluate(`document.querySelectorAll("#model-kpis .stat, #model-kpis .model-kpi").length`);
    await screenshot("v4-04-models");
    await click(`#model-tabs [data-model-tab="pricing"]`);
    await waitFor(() => evaluate(`!document.querySelector("#model-pricing-panel").hidden`), 4000, "单价表面板");
    await screenshot("v4-05-pricing");
    return `${rows} 行模型 / ${kpis} 个 KPI`;
  });

  await check("统一入口与路由组", async () => {
    await openConfig("routes", "routes");
    const matrix = await evaluate(`document.querySelectorAll("#unified-matrix .matrix-row").length`);
    await evaluate(`(() => {
      const input = document.querySelector("#route-preview-input");
      input.value = "group/fast";
      document.querySelector("#route-preview-run").click();
    })()`);
    await sleep(400);
    const preview = await evaluate(`(document.querySelector("#route-preview-result")?.textContent || "").trim().length`);
    if (!preview) throw new Error("生效预览没有输出");
    return `${matrix} 行可见矩阵 / 预览 ${preview} 字符`;
  });

  await check("接入列表与详情抽屉（6 页签）", async () => {
    await navigate("access");
    await waitFor(() => evaluate(`document.querySelectorAll("#access-table tbody tr").length > 0`), 8000, "接入表格");
    const rows = await evaluate(`document.querySelectorAll("#access-table tbody tr").length`);
    await screenshot("v4-06-access");
    await evaluate(`(() => {
      const row = document.querySelector("#access-table tbody tr");
      (row.querySelector("button, td") || row).click();
    })()`);
    await waitFor(() => evaluate(`!document.querySelector("#drawer").hidden`), 4000, "接入详情抽屉");
    const tabs = await evaluate(`Array.from(document.querySelectorAll(".drawer-tab")).map((b) => b.dataset.dtab)`);
    if (tabs.length < 6) throw new Error(`抽屉页签不足：${tabs.join("/")}`);
    await click('.drawer-tab[data-dtab="connection"]');
    await click('.drawer-tab[data-dtab="models"]');
    await click('.drawer-tab[data-dtab="advanced"]');
    await screenshot("v4-07-drawer");
    await evaluate(`document.querySelector("#drawer-close").click()`);
    await waitFor(() => evaluate(`document.querySelector("#drawer").hidden`), 3000, "关闭抽屉");
    return `${rows} 行接入 / 页签 ${tabs.join("·")}`;
  });

  await check("用量分析：KPI / 趋势 / 明细 / 请求抽屉", async () => {
    await navigate("traffic");
    await waitFor(() => evaluate(`document.querySelectorAll("#usage-kpis .usage-kpi").length === 5`), 8000, "用量 KPI");
    const kpis = await evaluate(`document.querySelectorAll("#usage-kpis .usage-kpi").length`);
    const bars = await evaluate(`document.querySelectorAll("#usage-trend .bar-col").length`);
    const pie = await evaluate(`document.querySelectorAll("#usage-pie .pie-row").length`);
    await screenshot("v4-08-usage");
    // 24 小时范围应切换到小时粒度：柱子在 24 根上下，刻度形如 08:00。
    await evaluate(`(() => {
      const select = document.querySelector("#usage-range");
      select.value = "24h";
      select.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
    await waitFor(() => evaluate(`document.querySelector("#usage-range").value === "24h"`), 3000, "切换 24h");
    await sleep(900);
    const hourBars = await evaluate(`document.querySelectorAll("#usage-trend .bar-col").length`);
    const hourLabels = await evaluate(
      `Array.from(document.querySelectorAll("#usage-trend .bar-col > span")).map((el) => el.textContent).filter(Boolean)`
    );
    if (hourBars > 25 || hourBars === 0) throw new Error(`24h 趋势柱子数异常：${hourBars}`);
    if (hourLabels.length && !hourLabels.every((label) => label.includes(":"))) {
      throw new Error(`24h 趋势刻度不是小时格式：${hourLabels.slice(0, 3).join(",")}`);
    }
    await click(`#usage-tabs [data-usage-tab="request"]`);
    await waitFor(() => evaluate(`!document.querySelector("#usage-panel-request").hidden`), 4000, "请求明细面板");
    await waitFor(() => evaluate(`document.querySelectorAll("#usage-detail-table tbody tr").length > 0`), 6000, "请求明细行");
    const rows = await evaluate(`document.querySelectorAll("#usage-detail-table tbody tr").length`);
    await evaluate(`document.querySelector("#usage-detail-table tbody tr").click()`);
    await waitFor(() => evaluate(`!document.querySelector("#request-drawer").hidden`), 4000, "请求详情抽屉");
    await screenshot("v4-09-request-drawer");
    await click("#request-close");
    await waitFor(() => evaluate(`document.querySelector("#request-drawer").hidden`), 3000, "请求抽屉关闭");
    if (!bars) throw new Error("趋势图为空");
    return `${kpis} KPI / ${bars} 趋势柱 / 24h ${hourBars} 柱 / ${pie} 分布行 / ${rows} 明细行`;
  });

  await check("用量筛选刷新后保留（localStorage）", async () => {
    await navigate("traffic");
    await evaluate(`(() => {
      const select = document.querySelector("#usage-range");
      select.value = "30d";
      select.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
    await sleep(900);
    const reloaded = new Promise((resolve) => cdp.once("Page.loadEventFired", resolve));
    await cdp.send("Page.reload", { ignoreCache: true });
    await Promise.race([reloaded, sleep(12000).then(() => { throw new Error("刷新超时"); })]);
    await waitFor(() => evaluate(`document.documentElement.dataset.ui === "v4"`), 15000, "刷新后初始化");
    await navigate("traffic");
    await sleep(500);
    const value = await evaluate(`document.querySelector("#usage-range").value`);
    if (value !== "30d") throw new Error(`刷新后时间范围未保留：${value}`);
    return `刷新后 range=${value}`;
  });

  await check("提示词：列表、详情与会话演进", async () => {
    await navigate("prompts");
    await waitFor(
      () => evaluate(`document.querySelectorAll("#prompt-table tbody tr[data-prompt-id]").length > 0`),
      10000,
      "提示词列表"
    );
    const rows = await evaluate(`document.querySelectorAll("#prompt-table tbody tr[data-prompt-id]").length`);
    await evaluate(`document.querySelector("#prompt-table tbody tr[data-prompt-id]").click()`);
    await waitFor(
      () => evaluate(`document.querySelectorAll("#prompt-side-body .message-block").length > 0`),
      6000,
      "提示词详情侧栏"
    );
    const sideMessages = await evaluate(`document.querySelectorAll("#prompt-side-body .message-block").length`);
    await screenshot("v4-10-prompt-detail");
    await click(`#prompt-tabs [data-prompt-tab="evolution"]`);
    await waitFor(() => evaluate(`!document.querySelector("#prompt-evolution-view").hidden`), 4000, "会话演进视图");
    return `${rows} 行提示词 / 详情 ${sideMessages} 条消息`;
  });

  await check("日志与诊断：尾部日志 + 诊断分组", async () => {
    await navigate("logs");
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
    await screenshot("v4-11-logs");
    await click(`#observe-tabs [data-observe-tab="diag"]`);
    await waitFor(() => evaluate(`!document.querySelector("#observe-diag").hidden`), 4000, "诊断视图");
    await waitFor(
      () => evaluate(`document.querySelectorAll("#diag-groups .diag-card, #diag-groups .diag-row").length > 0`),
      6000,
      "诊断分组"
    );
    const groups = await evaluate(`document.querySelectorAll("#diag-groups .diag-card").length`);
    await screenshot("v4-12-diag");
    return `${meta} / ${groups} 组诊断`;
  });

  await check("设置：表单、未保存提示与校验", async () => {
    await openConfig("settings", "settings");
    await waitFor(() => evaluate(`!!document.querySelector("#set-host")?.value`), 6000, "设置表单");
    const host = await evaluate(`document.querySelector("#set-host").value`);
    await evaluate(`(() => {
      const input = document.querySelector("#set-timeout");
      input.value = String(Number(input.value || 600) + 1);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    const dirty = await evaluate(`!document.querySelector("#settings-dirty").hidden`);
    await click("#settings-validate");
    await waitFor(() => evaluate(`(document.querySelector("#settings-issues")?.textContent || "").trim().length > 0`), 8000, "配置校验结果");
    const issues = await evaluate(`document.querySelector("#settings-issues").textContent.trim()`);
    await screenshot("v4-13-settings");
    if (!host) throw new Error("监听地址为空");
    if (!dirty) throw new Error("修改后未出现未保存提示");
    return `${host} / 校验：${issues.slice(0, 24)}`;
  });

  await check("凭据池 / 插件 / 免费来源 / 模板 / ZCode", async () => {
    await openConfig("pools", "credentials");
    const pools = await evaluate(`document.querySelectorAll("#pool-list .card").length`);
    await openConfig("plugins", "plugins");
    await waitFor(() => evaluate(`document.querySelectorAll("#plugin-list .plugin-card").length >= 3`), 6000, "插件卡片");
    const plugins = await evaluate(`document.querySelectorAll("#plugin-list .plugin-card").length`);
    await openConfig("free", "free");
    await waitFor(() => evaluate(`document.querySelectorAll("#free-sources .card").length >= 1`), 6000, "免费来源卡片");
    const free = await evaluate(`document.querySelectorAll("#free-sources .card").length`);
    await openConfig("templates", "templates");
    await waitFor(() => evaluate(`document.querySelectorAll("#template-table tbody tr").length >= 5`), 6000, "模板行");
    const templates = await evaluate(`document.querySelectorAll("#template-table tbody tr").length`);
    await openConfig("zcode", "zcode");
    await waitFor(() => evaluate(`document.querySelectorAll("#zcode-status .stat").length >= 4`), 6000, "ZCode 状态");
    const zcode = await evaluate(`document.querySelectorAll("#zcode-status .stat").length`);
    await screenshot("v4-14-zcode");
    return `池 ${pools} / 插件 ${plugins} / 免费来源 ${free} / 模板 ${templates} / ZCode ${zcode}`;
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
    const usage = await evaluate(`document.querySelector("#view-traffic")?.classList.contains("active")`);
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
    await navigate("overview");
    await sleep(600);
    const metrics = await evaluate(`({
      inner: window.innerWidth,
      scroll: document.documentElement.scrollWidth,
      kpis: document.querySelectorAll("#ov-kpis .ov-kpi").length,
      navVisible: document.querySelectorAll(".nav-item").length,
    })`);
    await screenshot("v4-15-mobile");
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
    if (!metrics.kpis) throw new Error("窄屏下总览 KPI 未渲染");
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
