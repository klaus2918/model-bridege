#!/usr/bin/env node
/*
 * Model-Gate v3 控制台冒烟测试（无 npm 依赖，直接用 Chrome/Edge CDP）。
 *
 * 用法：
 *   node scripts/ui_smoke_v3.js --base http://127.0.0.1:8912
 *   node scripts/ui_smoke_v3.js --base http://127.0.0.1:8912 --screenshot-dir docs/design/refactor-2026/screenshots-v3
 *
 * 前置：base 指向一个已启动、带演示数据的 Model-Gate 实例。
 * 覆盖：页面加载无 JS 异常、九个视图切换、接入详情抽屉、快速添加向导、
 *       凭据池、插件、设置、截图。
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
const base = getArg("base", "http://127.0.0.1:8912").replace(/\/+$/, "");
const debugPort = Number(getArg("debug-port", "19233"));
const screenshotDir = getArg(
  "screenshot-dir",
  path.join("docs", "design", "refactor-2026", "screenshots-v3"),
);
const chromeCandidates = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
];
const chromePath = getArg(
  "chrome",
  process.env.CHROME_PATH || chromeCandidates.find((item) => fs.existsSync(item)) || "",
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
      reject(new Error(`CDP 连接失败：${event.message || "error"}`)),
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
  if (!fs.existsSync(chromePath)) throw new Error(`找不到 Chrome/Edge：${chromePath}`);

  userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "mg-v3-smoke-"));
  chrome = spawn(
    chromePath,
    [
      "--headless=new",
      "--disable-gpu",
      "--disable-extensions",
      "--no-first-run",
      "--no-default-browser-check",
      "--remote-allow-origins=*",
      `--remote-debugging-port=${debugPort}`,
      `--user-data-dir=${userDataDir}`,
      "--window-size=1440,1000",
      "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"], windowsHide: true },
  );
  let chromeErr = "";
  chrome.stderr.on("data", (chunk) => {
    chromeErr += String(chunk);
  });

  await waitFor(async () => {
    try {
      return (await fetchJson(`http://127.0.0.1:${debugPort}/json/version`, 1000)).status === 200;
    } catch {
      return false;
    }
  }, 12000, "Chrome 调试端口");
  if (chrome.exitCode !== null) throw new Error(`Chrome 提前退出：${chromeErr.slice(-1200)}`);

  const created = await fetch(
    `http://127.0.0.1:${debugPort}/json/new?${encodeURIComponent("about:blank")}`,
    { method: "PUT" },
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
    record(
      `console: ${(params.args || []).map((arg) => arg.value ?? arg.description ?? "").join(" ")}`,
    );
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
          "页面脚本执行失败",
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
    await sleep(180);
  };

  const navigate = async (view) => {
    await click(`.nav-item[data-view="${view}"]`);
    await waitFor(
      () => evaluate(`document.querySelector("#view-${view}")?.classList.contains("active")`),
      4000,
      `视图 ${view}`,
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
    () => evaluate(`document.querySelectorAll(".nav-item").length >= 9 && !!document.querySelector("#view-overview.active")`),
    12000,
    "v3 前端初始化",
  );
  await sleep(600);

  await check("页面加载无 JS 异常", async () => {
    if (errors.length) throw new Error(errors.slice(0, 3).join(" | "));
    return "0 个错误";
  });

  await check("侧栏 9 个子视图与分组", async () => {
    const data = await evaluate(`({
      views: Array.from(document.querySelectorAll(".nav-item")).map((b) => b.dataset.view),
      groups: Array.from(document.querySelectorAll(".nav-label")).map((el) => el.textContent.trim()),
    })`);
    const expected = ["overview", "access", "models", "routes", "credentials", "plugins", "traffic", "observe", "settings"];
    for (const view of expected) {
      if (!data.views.includes(view)) throw new Error(`缺少视图 ${view}`);
    }
    if (data.groups.length < 5) throw new Error(`分组不足：${data.groups.join("/")}`);
    return `${data.views.length} 视图 / ${data.groups.length} 分组`;
  });

  await check("总览展示接入健康与统计", async () => {
    const count = await evaluate(`document.querySelectorAll("#overview-access .row").length`);
    const stats = await evaluate(`document.querySelectorAll("#overview-stats .stat").length`);
    if (count < 1) throw new Error("总览没有接入行");
    if (stats < 4) throw new Error(`KPI 不足：${stats}`);
    await screenshot("v3-01-overview");
    return `${count} 个接入 / ${stats} 个 KPI`;
  });

  await check("全部接入列表与筛选", async () => {
    await navigate("access");
    await waitFor(() => evaluate(`document.querySelectorAll("#access-table tbody tr").length > 0`), 5000, "接入表格");
    const rows = await evaluate(`document.querySelectorAll("#access-table tbody tr").length`);
    await evaluate(`(() => {
      const input = document.querySelector("#access-search");
      input.value = "___no_such_access___";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    const empty = await evaluate(`!document.querySelector("#access-empty").hidden`);
    await evaluate(`(() => {
      const input = document.querySelector("#access-search");
      input.value = "";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    if (!empty) throw new Error("搜索空态未出现");
    await screenshot("v3-02-access");
    return `${rows} 行，空态正常`;
  });

  await check("接入详情抽屉与子页签", async () => {
    await click("#access-table tbody tr[data-open]");
    await waitFor(() => evaluate(`!document.querySelector("#drawer").hidden`), 3000, "详情抽屉");
    const tabs = await evaluate(`Array.from(document.querySelectorAll(".drawer-tab")).map((b) => b.dataset.dtab)`);
    if (tabs.length < 5) throw new Error(`抽屉页签不足：${tabs.join("/")}`);
    await click('.drawer-tab[data-dtab="connection"]');
    await waitFor(() => evaluate(`!!document.querySelector("#edit-baseurl")`), 2000, "连接页");
    await screenshot("v3-03-drawer");
    await click("#drawer-close");
    return tabs.join(" / ");
  });

  await check("快速添加：模板 → 凭据两步", async () => {
    await click("#quick-add");
    await waitFor(() => evaluate(`!document.querySelector("#modal").hidden`), 3000, "添加弹窗");
    const templates = await evaluate(`document.querySelectorAll("#template-grid .template-card").length`);
    if (templates < 5) throw new Error(`官方模板不足：${templates}`);
    await click('#template-grid .template-card[data-template="deepseek"]');
    await click("#template-next");
    await waitFor(() => evaluate(`!!document.querySelector("#quick-name")`), 2000, "凭据步骤");
    const name = await evaluate(`document.querySelector("#quick-name").value`);
    const zdr = await evaluate(`document.querySelector("#quick-zdr").checked`);
    if (name !== "deepseek") throw new Error(`默认标识异常：${name}`);
    if (!zdr) throw new Error("隐私默认未勾选");
    await screenshot("v3-04-quick-add");
    await click("#modal-close");
    return `deepseek，ZDR 默认开，共 ${templates} 个官方模板`;
  });

  await check("凭据池卡片与成员", async () => {
    await navigate("credentials");
    await waitFor(
      () =>
        evaluate(
          `document.querySelectorAll("#pool-list .card").length > 0 || !!document.querySelector("#pool-add-empty")`,
        ),
      6000,
      "凭据池",
    );
    const pools = await evaluate(`document.querySelectorAll("#pool-list .card").length`);
    await screenshot("v3-05-credentials");
    return pools > 0 ? `${pools} 个池` : "0 个池（空态）";
  });

  await check("插件中心：内置与接入状态", async () => {
    await navigate("plugins");
    await waitFor(() => evaluate(`document.querySelectorAll("#plugin-list .card").length >= 3`), 6000, "插件卡片");
    const cards = await evaluate(`document.querySelectorAll("#plugin-list .card").length`);
    await screenshot("v3-06-plugins");
    return `${cards} 个插件`;
  });

  await check("流量与观测视图可渲染", async () => {
    await navigate("traffic");
    await waitFor(() => evaluate(`document.querySelectorAll("#traffic-stats .stat").length >= 4`), 6000, "流量统计");
    await screenshot("v3-07-traffic");
    await navigate("observe");
    await waitFor(() => evaluate(`!!document.querySelector("#diagnostic-list")`), 4000, "诊断列表");
    return "流量 / 观测已渲染";
  });

  await check("设置读取配置与 AI 开关", async () => {
    await navigate("settings");
    await waitFor(() => evaluate(`!!document.querySelector("#set-host").value`), 5000, "设置表单");
    const host = await evaluate(`document.querySelector("#set-host").value`);
    const templates = await evaluate(`document.querySelectorAll("#template-list .row").length`);
    await screenshot("v3-08-settings");
    if (!host) throw new Error("监听地址为空");
    return `${host} / ${templates} 个模板`;
  });

  await check("经典面板仍可打开", async () => {
    const response = await fetch(`${base}/ui/classic`);
    const text = await response.text();
    if (response.status !== 200 || !text.includes("data-tab=\"overview\"")) {
      throw new Error("经典面板未保留");
    }
    return "HTTP 200";
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
