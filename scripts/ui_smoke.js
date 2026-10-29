#!/usr/bin/env node
/*
 * Model-Gate 面板无头浏览器冒烟测试。
 *
 * 不依赖 npm 包：直接通过 Chrome DevTools Protocol 驱动本机 Chrome/Edge。
 * 覆盖：页面加载无 JS 异常、全部页签可切换、主题切换、配置说明弹窗、
 *       提示词列表/详情/全部展开折叠/会话演进/差异渲染、常用页面基础元素。
 *
 * 用法：
 *   node scripts/ui_smoke.js --exe dist/Model-Gate-rust.exe
 *   node scripts/ui_smoke.js --base http://127.0.0.1:18942 --seed
 * 指定 --exe 时脚本会自建临时配置/数据库并启动服务，结束后自动清理；
 * --keep 保留现场目录，--no-seed 跳过提示词注入用例。
 */

"use strict";

const { spawn, execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

if (typeof WebSocket === "undefined") {
  console.error("需要 Node 22+（内置 WebSocket）");
  process.exit(2);
}

const argv = process.argv.slice(2);
const getArg = (name, fallback = "") => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const baseArg = getArg("base", "").replace(/\/+$/, "");
const exeArg = getArg("exe", "");
const exePath = exeArg ? path.resolve(exeArg) : "";
const serverPort = Number(getArg("port", "18942"));
const base = baseArg || `http://127.0.0.1:${serverPort}`;
const keepArtifacts = argv.includes("--keep");
let serverProcess = null;
let serverTempDir = "";
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
const debugPort = Number(getArg("debug-port", "19222"));
const doSeed = exePath ? !argv.includes("--no-seed") : argv.includes("--seed");
const screenshotPath = getArg(
  "screenshot",
  path.join(os.tmpdir(), "model-gate-ui-smoke.png"),
);
const pageUrl = `${base}/ui/`;

const failures = [];
const passes = [];
let sessionId = "";

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
    const detail = await fn();
    pass(name, detail || "");
  } catch (error) {
    fail(name, error);
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(fn, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const value = await fn();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(100);
  }
  throw new Error(`${label || "等待条件"}超时（${timeoutMs}ms）${lastError ? `：${lastError.message}` : ""}`);
}

async function fetchJson(url, options = {}, timeoutMs = 8000) {
  const response = await fetch(url, {
    ...options,
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  return { status: response.status, body };
}

function chromeLaunchArgs() {
  return [
    "--headless=new",
    "--disable-gpu",
    "--disable-extensions",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--remote-allow-origins=*",
    `--remote-debugging-port=${debugPort}`,
    `--user-data-dir=${userDataDir}`,
    "--window-size=1440,1000",
    "about:blank",
  ];
}

let userDataDir = "";
let chrome = null;
let cdp = null;
let targetId = "";

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
    socket.addEventListener("error", (event) => {
      reject(new Error(`CDP WebSocket 连接失败：${event.message || "error"}`));
    });
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
        if (message.error) {
          rej(new Error(`${message.error.message} (${message.error.code})`));
        } else {
          res(message.result);
        }
        return;
      }
      if (message.method && listeners.has(message.method)) {
        for (const fn of listeners.get(message.method).slice()) {
          try {
            fn(message.params);
          } catch {
            /* 监听器异常不影响测试主流程 */
          }
        }
      }
    });
  });
}

async function startServer() {
  if (!exePath) return;
  if (!fs.existsSync(exePath)) throw new Error(`找不到待测 exe：${exePath}`);
  serverTempDir = fs.mkdtempSync(path.join(os.tmpdir(), "mg-ui-smoke-server-"));
  const configPath = path.join(serverTempDir, "bridge.config.json");
  const dbPath = path.join(serverTempDir, "data", "modelgate.db");
  const config = {
    port: serverPort,
    host: "127.0.0.1",
    defaultModel: "m",
    models: ["m"],
    promptCapture: true,
    promptRetentionDays: 7,
    promptMaxRecords: 100,
    promptMaxChars: 4000,
    upstreams: {
      fake: {
        protocol: "openai",
        baseUrl: "http://127.0.0.1:1",
        apiKey: "k",
        fetchModels: false,
        autoModels: false,
        models: ["m"],
        default: true,
      },
    },
  };
  fs.writeFileSync(configPath, JSON.stringify(config, null, 2), "utf8");
  let stderr = "";
  serverProcess = spawn(
    exePath,
    [
      "--no-gui",
      "--port",
      String(serverPort),
      "--config",
      configPath,
      "--db",
      dbPath,
      "--force-multi",
    ],
    {
      cwd: path.dirname(exePath),
      stdio: ["ignore", "ignore", "pipe"],
      windowsHide: true,
    },
  );
  serverProcess.stderr.on("data", (chunk) => {
    stderr += String(chunk);
  });
  try {
    await waitFor(async () => {
      try {
        const health = await fetchJson(`${base}/health`, {}, 1000);
        return health.status === 200;
      } catch {
        return false;
      }
    }, 15000, "测试服务就绪");
  } catch (error) {
    throw new Error(`${error.message}\n${stderr.slice(-2000)}`);
  }
  if (serverProcess.exitCode !== null) {
    throw new Error(`测试服务提前退出：\n${stderr.slice(-2000)}`);
  }
}

async function main() {
  await startServer();
  if (!fs.existsSync(chromePath)) {
    throw new Error(`找不到 Chrome/Edge：${chromePath}`);
  }
  const health = await fetchJson(`${base}/health`, {}, 3000);
  if (health.status !== 200) throw new Error(`服务未就绪：${base}/health -> ${health.status}`);

  if (doSeed) {
    sessionId = await seedPrompts();
    console.log(`[seed] 会话 ${sessionId}`);
  }

  userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "mg-ui-smoke-"));
  chrome = spawn(chromePath, chromeLaunchArgs(), {
    stdio: ["ignore", "ignore", "pipe"],
    windowsHide: true,
  });
  let chromeStderr = "";
  chrome.stderr.on("data", (chunk) => {
    chromeStderr += String(chunk);
  });

  await waitFor(async () => {
    try {
      const version = await fetchJson(`http://127.0.0.1:${debugPort}/json/version`, {}, 1000);
      return version.status === 200;
    } catch {
      return false;
    }
  }, 12000, "Chrome 调试端口");
  if (chrome.exitCode !== null) {
    throw new Error(`Chrome 提前退出：\n${chromeStderr.slice(-2000)}`);
  }

  const created = await fetch(
    `http://127.0.0.1:${debugPort}/json/new?${encodeURIComponent("about:blank")}`,
    { method: "PUT" },
  );
  const target = await created.json();
  targetId = target.id;
  if (!target.webSocketDebuggerUrl) throw new Error("未能创建浏览器标签页");

  cdp = await connectCdp(target.webSocketDebuggerUrl);
  const pageErrors = [];
  let loaded = false;

  const recordError = (text) => {
    if (!text) return;
    if (/favicon\.ico/i.test(text)) return;
    pageErrors.push(text);
  };

  cdp.on("Runtime.exceptionThrown", (params) => {
    const details = params.exceptionDetails || {};
    recordError(
      `exception: ${details.exception?.description || details.text || "unknown exception"}`,
    );
  });
  cdp.on("Log.entryAdded", (params) => {
    const entry = params.entry || {};
    if (entry.level === "error") recordError(`log: ${entry.text}${entry.url ? ` (${entry.url})` : ""}`);
  });
  cdp.on("Runtime.consoleAPICalled", (params) => {
    if (params.type !== "error") return;
    recordError(
      `console: ${(params.args || [])
        .map((arg) => arg.value ?? arg.description ?? "")
        .join(" ")}`,
    );
  });

  await cdp.send("Runtime.enable");
  await cdp.send("Page.enable");
  await cdp.send("Log.enable");

  const loadEvent = new Promise((resolve) => cdp.once("Page.loadEventFired", resolve));
  if (!loaded) {
    await cdp.send("Page.navigate", { url: pageUrl });
    await Promise.race([
      loadEvent,
      sleep(12000).then(() => {
        throw new Error("页面加载超时");
      }),
    ]);
    loaded = true;
  }

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
    await sleep(120);
  };

  const isVisible = (selector) =>
    evaluate(`(() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return false;
      const style = getComputedStyle(el);
      return !el.hidden && style.display !== "none" && style.visibility !== "hidden" && el.getClientRects().length > 0;
    })()`);

  const activePanels = () =>
    evaluate(
      `Array.from(document.querySelectorAll("main > section.panel")).filter((p) => p.classList.contains("active") && !p.hidden && getComputedStyle(p).display !== "none").map((p) => p.id)`,
    );

  await waitFor(
    () => evaluate(`document.readyState === "complete" && document.querySelectorAll("[data-tab]").length >= 7 && !!document.querySelector("#panel-overview")`),
    12000,
    "前端脚本初始化",
  );

  await check("页面加载无 JS 异常", async () => {
    await sleep(500);
    if (pageErrors.length) throw new Error(pageErrors.slice(0, 3).join(" | "));
    return `已收集 ${pageErrors.length} 个错误`;
  });

  const tabs = await evaluate(`Array.from(document.querySelectorAll("[data-tab]")).map((b) => b.dataset.tab)`);
  await check("页签数量", async () => {
    if (!Array.isArray(tabs) || tabs.length < 7) throw new Error(`页签数量异常：${JSON.stringify(tabs)}`);
    return tabs.join(" / ");
  });

  for (const tab of tabs) {
    // 免费来源等面板由配置控制可见性（默认隐藏）；隐藏页签不参与点击遍历，
    // 其「配置开启后可见」的契约在下面的专项用例中验证。
    const hiddenByDefault = await evaluate(
      `document.querySelector('[data-tab="${tab}"]')?.hidden === true`,
    );
    if (hiddenByDefault) {
      await check(`页签默认隐藏（配置开启后显示）：${tab}`, async () => "hidden");
      continue;
    }
    await check(`页签可切换：${tab}`, async () => {
      await click(`[data-tab="${tab}"]`);
      await sleep(220);
      const panels = await activePanels();
      const expected = `panel-${tab}`;
      if (!panels.includes(expected)) throw new Error(`未激活 ${expected}，当前 ${panels.join(",") || "无"}`);
      if (panels.length !== 1) throw new Error(`同时激活多个面板：${panels.join(",")}`);
      if (!(await isVisible(`#${expected}`))) throw new Error(`${expected} 不可见`);
      return expected;
    });
  }

  await check("主题切换可点击", async () => {
    await click("#theme-toggle");
    await click("#theme-toggle");
    return "点击两次无异常";
  });

  await check("配置说明弹窗可开可关", async () => {
    await click('[data-tab="config"]');
    await click("#config-help-btn");
    await waitFor(() => evaluate(`!document.querySelector("#modal-root").hidden`), 3000, "配置说明弹窗打开");
    const title = await evaluate(`document.querySelector("#modal-root h3")?.textContent || ""`);
    if (!title.trim()) throw new Error("弹窗标题为空");
    await click("[data-modal-close]");
    await waitFor(() => evaluate(`document.querySelector("#modal-root").hidden`), 3000, "配置说明弹窗关闭");
    return title.trim();
  });

  if (doSeed) {
    await check("提示词列表加载", async () => {
      await click('[data-tab="prompts"]');
      await waitFor(
        () => evaluate(`document.querySelectorAll("#prompt-table tbody tr[data-prompt-id]").length > 0`),
        8000,
        "提示词列表行",
      );
      const total = await evaluate(`document.querySelector("#prompt-total")?.textContent || ""`);
      if (!/共\s*\d+\s*条/.test(total)) throw new Error(`总数展示异常：${total}`);
      return total.trim();
    });

    await check("提示词详情：调用链与用量面板", async () => {
      const targetId = await evaluate(`(() => {
        const rows = Array.from(document.querySelectorAll("#prompt-table tbody tr[data-prompt-id]"));
        const row = rows.find((r) => !r.textContent.includes("截断")) || rows[rows.length - 1];
        return row ? row.dataset.promptId : "";
      })()`);
      if (!targetId) throw new Error("没有可用的提示词记录");
      await click(`[data-prompt-view="${targetId}"]`);
      try {
        await waitFor(
          () => evaluate(`!document.querySelector("#modal-root").hidden && !!document.querySelector(".prompt-call-chain")`),
          5000,
          "提示词详情弹窗",
        );
      } catch (error) {
        const toastText = await evaluate(`document.querySelector("#toast")?.textContent || ""`);
        throw new Error(`${error.message}${toastText ? `；toast=${toastText}` : ""}`);
      }
      const result = await evaluate(`(() => ({
        messages: document.querySelectorAll(".prompt-msg").length,
        sequence: document.querySelectorAll(".prompt-message-sequence .badge").length,
        chain: !!document.querySelector(".prompt-call-chain"),
        toolTable: !!document.querySelector(".prompt-tool-table"),
        usage: !!document.querySelector(".prompt-usage-panel"),
      }))()`);
      if (!result.messages) throw new Error("详情没有消息");
      if (!result.sequence) throw new Error("详情缺少消息序列");
      if (!result.chain || !result.usage) throw new Error(`缺少审查面板：${JSON.stringify(result)}`);
      const chainText = await evaluate(`document.querySelector(".prompt-call-chain").textContent`);
      if (!chainText.includes("read_file")) throw new Error("调用链未显示工具名 read_file");
      const formatted = await evaluate(`(() => {
        const messagePres = Array.from(document.querySelectorAll(".prompt-messages pre")).map((p) => p.textContent);
        const toolPres = Array.from(document.querySelectorAll(".prompt-tool-pre")).map((p) => p.textContent);
        const pretty = (text) => /\\n\\s{2}"path": "README\\.md"/.test(text);
        return {
          messageJson: messagePres.some(pretty),
          toolArgsJson: toolPres.some(pretty),
          badges: document.querySelectorAll(".prompt-msg .badge.info").length,
        };
      })()`);
      if (!formatted.messageJson) throw new Error("消息 JSON 未自动格式化");
      if (!formatted.toolArgsJson) throw new Error("工具参数 JSON 未自动格式化");
      if (formatted.badges < 1) throw new Error("未显示 JSON 格式化标记");
      return `messages=${result.messages}, sequence=${result.sequence}, toolTable=${result.toolTable}, json=${formatted.badges}`;
    });

    await check("提示词详情：全部展开/全部折叠", async () => {
      await click("[data-prompt-expand-all]");
      const expanded = await evaluate(`(() => {
        const all = document.querySelectorAll(".prompt-msg").length;
        const open = document.querySelectorAll(".prompt-msg[open]").length;
        return { all, open };
      })()`);
      if (expanded.open !== expanded.all) throw new Error(`展开不完整：${expanded.open}/${expanded.all}`);
      await click("[data-prompt-collapse-all]");
      const collapsed = await evaluate(`document.querySelectorAll(".prompt-msg[open]").length`);
      if (collapsed !== 0) throw new Error(`折叠后仍有 ${collapsed} 条展开`);
      return `${expanded.all} 条可展开折叠`;
    });

    if (await evaluate(`!document.querySelector("#modal-root").hidden`)) {
      await click("[data-modal-close]");
      await waitFor(() => evaluate(`document.querySelector("#modal-root").hidden`), 3000, "详情弹窗关闭");
    }

    await check("提示词行点击可打开详情", async () => {
      await click("#prompt-table tbody tr[data-prompt-id] .prompt-preview");
      await waitFor(
        () => evaluate(`!document.querySelector("#modal-root").hidden && !!document.querySelector(".prompt-messages")`),
        5000,
        "行点击详情弹窗",
      );
      if (await evaluate(`!document.querySelector("#modal-root").hidden`)) {
        await click("[data-modal-close]");
        await waitFor(() => evaluate(`document.querySelector("#modal-root").hidden`), 3000, "详情弹窗关闭");
      }
      return "行点击生效";
    });

    await check("提示词详情：头尾截断提示", async () => {
      const truncatedId = await evaluate(`(() => {
        const rows = Array.from(document.querySelectorAll("#prompt-table tbody tr[data-prompt-id]"));
        const row = rows.find((r) => r.textContent.includes("截断"));
        return row ? row.dataset.promptId : "";
      })()`);
      if (!truncatedId) throw new Error("没有截断记录可验证");
      await click(`[data-prompt-view="${truncatedId}"]`);
      try {
        await waitFor(
          () => evaluate(`!document.querySelector("#modal-root").hidden && !!document.querySelector(".prompt-messages")`),
          5000,
          "截断记录详情",
        );
      } catch (error) {
        const toastText = await evaluate(`document.querySelector("#toast")?.textContent || ""`);
        throw new Error(`${error.message}${toastText ? `；toast=${toastText}` : ""}`);
      }
      const divider = await evaluate(`document.querySelector(".prompt-truncation")?.textContent || ""`);
      if (!divider.includes("中间省略") && !divider.includes("已按预算截断")) {
        throw new Error("未显示头尾保留或预算截断提示");
      }
      return divider.trim();
    });
    if (await evaluate(`!document.querySelector("#modal-root").hidden`)) {
      await click("[data-modal-close]");
      await waitFor(() => evaluate(`document.querySelector("#modal-root").hidden`), 3000, "详情弹窗关闭");
    }

    await check("会话演进与差异渲染", async () => {
      await click('#prompt-view-seg [data-view="evolution"]');
      await waitFor(
        () => evaluate(`Array.from(document.querySelectorAll("#evolution-session option")).some((o) => o.value === ${JSON.stringify(sessionId)})`),
        5000,
        "会话下拉选项",
      );
      const selected = await evaluate(`(() => {
        const select = document.querySelector("#evolution-session");
        const option = Array.from(select.options).find((o) => o.value === ${JSON.stringify(sessionId)});
        if (!option) return false;
        select.value = option.value;
        select.dispatchEvent(new Event("change", { bubbles: true }));
        return true;
      })()`);
      if (!selected) throw new Error("无法选择测试会话");
      await waitFor(
        () => evaluate(`document.querySelectorAll("#evolution-timeline .prompt-timeline-row").length >= 2`),
        8000,
        "演进时间轴",
      );
      // 默认已选最近两轮为基准/对比，差异明细应自动加载
      await waitFor(
        () => evaluate(`document.querySelector("#prompt-diff").textContent.trim().length > 0`),
        8000,
        "差异明细（默认对比最近两轮）",
      );
      const barText = await evaluate(`document.querySelector("#evolution-compare-bar").textContent`);
      if (!barText.includes("基准 #") || !barText.includes("对比 #")) {
        throw new Error(`对比条缺少基准/对比标记：${barText.slice(0, 80)}`);
      }
      const timelineText = await evaluate(`document.querySelector("#evolution-timeline").textContent`);
      for (const label of ["首条 user：", "字符", "基准", "对比", "详情"]) {
        if (!timelineText.includes(label)) throw new Error(`时间轴缺少关键信息：${label}`);
      }
      // 点行展开消息序列（原「会话流程」面板已合并进行内展开）
      const ids = await evaluate(
        `Array.from(document.querySelectorAll("#evolution-timeline [data-evolution-id]")).map((e) => e.dataset.evolutionId)`,
      );
      await click(`[data-evolution-id="${ids[0]}"]`);
      await waitFor(
        () => evaluate(`document.querySelectorAll("#evolution-timeline .expanded .prompt-transcript-msg").length >= 5`),
        5000,
        "行内消息序列",
      );
      const expanded = await evaluate(
        `document.querySelectorAll("#evolution-timeline .expanded .prompt-transcript-msg").length`,
      );
      // 槽位切换：把第一轮设为对比对象，差异应按新基准/对比刷新
      await click(`[data-evolution-id="${ids[0]}"] [data-evolution-slot="target"]`);
      await waitFor(
        () => evaluate(`document.querySelector("#evolution-pair").textContent.includes("对比 #1")`),
        8000,
        "槽位切换后差异刷新",
      );
      // 工具执行评估：会话概览 chips + 可复制的 Markdown 报告
      const toolChips = await evaluate(
        `document.querySelectorAll("#evolution-session-chips .ev-stat-chip").length`,
      );
      if (toolChips < 5) throw new Error(`会话概览 chips 不足：${toolChips}`);
      const reportLen = await evaluate(`document.querySelector("#evolution-tool-report").textContent.length`);
      if (reportLen < 200) throw new Error(`评估报告过短：${reportLen}`);
      const toolRows = await evaluate(
        `document.querySelectorAll("#evolution-tool-table tbody tr").length`,
      );
      if (toolRows < 1) throw new Error("工具画像表为空");
      return `timeline=${ids.length}, expandedMsgs=${expanded}, report=${reportLen}`;
    });

    await check("会话演进可查看完整提示词", async () => {
      const selector = "#evolution-timeline [data-evolution-detail]";
      await click(selector);
      try {
        await waitFor(
          () => evaluate(`!document.querySelector("#modal-root").hidden && !!document.querySelector(".prompt-messages")`),
          5000,
          "演进详情弹窗",
        );
      } catch (error) {
        const toastText = await evaluate(`document.querySelector("#toast")?.textContent || ""`);
        throw new Error(`${error.message}${toastText ? `；toast=${toastText}` : ""}`);
      }
      if (await evaluate(`!document.querySelector("#modal-root").hidden`)) {
        await click("[data-modal-close]");
        await waitFor(() => evaluate(`document.querySelector("#modal-root").hidden`), 3000, "详情弹窗关闭");
      }
      return "时间轴提供详情入口";
    });
  }

  await check("日志页基础元素", async () => {
    await click('[data-tab="logs"]');
    await waitFor(() => isVisible("#log-viewer"), 3000, "日志查看器");
    await click("#log-bottom");
    return "log-viewer 可见";
  });

  await check("概览页 KPI 渲染", async () => {
    await click('[data-tab="overview"]');
    await waitFor(
      () => evaluate(`document.querySelectorAll("#overview-cards .kpi, #overview-cards > *").length > 0`),
      5000,
      "概览 KPI",
    );
    return "KPI 已渲染";
  });

  await check("模型页基础元素", async () => {
    await click('[data-tab="model"]');
    await waitFor(() => isVisible("#model-table"), 3000, "模型表格");
    return "model-table 可见";
  });

  await check("用量页基础元素", async () => {
    await click('[data-tab="usage"]');
    await waitFor(() => isVisible("#usage-table"), 3000, "用量表格");
    return "usage-table 可见";
  });

  await check("免费来源页默认隐藏，配置开启后可见", async () => {
    if (!(await evaluate(`document.querySelector('[data-tab="free"]')?.hidden === true`))) {
      throw new Error("免费来源页签默认应为隐藏（freeSourcesPanel 未开启）");
    }
    // 走配置接口开启面板开关（与面板「显示『免费来源』页签」同一字段），再刷新页面
    await evaluate(`(async () => {
      const res = await fetch("/api/config");
      const data = await res.json();
      const config = { ...(data.config || {}), freeSourcesPanel: true };
      await fetch("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ config }),
      });
    })()`);
    await cdp.send("Page.navigate", { url: pageUrl });
    await sleep(1200);
    await waitFor(
      () => evaluate(`document.querySelector('[data-tab="free"]')?.hidden === false`),
      5000,
      "免费来源页签出现",
    );
    await click('[data-tab="free"]');
    await waitFor(() => isVisible("#free-sources"), 3000, "免费来源列表");
    return "开启后 free-sources 可见";
  });

  await check("提示词页最终无 JS 异常", async () => {
    await sleep(400);
    if (pageErrors.length) throw new Error(pageErrors.slice(0, 5).join(" | "));
    return `共 ${pageErrors.length} 个错误`;
  });

  try {
    const shot = await cdp.send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: false,
    });
    fs.writeFileSync(screenshotPath, Buffer.from(shot.data, "base64"));
    console.log(`[info] 截图：${screenshotPath}`);
  } catch (error) {
    console.warn(`[warn] 截图失败：${error.message}`);
  }

  if (pageErrors.length) {
    for (const error of pageErrors.slice(0, 10)) console.error(`[page-error] ${error}`);
  }

  console.log(`\n结果：${passes.length} 通过，${failures.length} 失败`);
  if (failures.length) {
    for (const item of failures) console.error(`- ${item}`);
    process.exitCode = 1;
  }
}

async function seedPrompts() {
  const session = `ui-smoke-${Date.now()}`;
  const payloads = [
    {
      messages: [
        { role: "system", content: "You are a UI smoke assistant." },
        { role: "user", content: "请检查仓库中的 README.md，并汇报结构。" },
        {
          role: "assistant",
          content: "",
          tool_calls: [
            {
              id: "call_ui_smoke_1",
              type: "function",
              function: { name: "read_file", arguments: '{"path":"README.md"}' },
            },
          ],
        },
        {
          role: "tool",
          tool_call_id: "call_ui_smoke_1",
          name: "read_file",
          content: JSON.stringify({ ok: true, path: "README.md", note: "UI smoke fixture" }),
        },
        { role: "user", content: "总结上面的改动。" },
      ],
    },
    {
      messages: [
        { role: "system", content: "You are a long-context smoke assistant." },
        {
          role: "user",
          content: `LONG-CONTEXT-START ${"上下文片段 ".repeat(700)} LONG-CONTEXT-END`,
        },
      ],
    },
  ];

  for (const payload of payloads) {
    try {
      await fetchJson(
        `${base}/v1/chat/completions`,
        {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "x-session-id": session,
          },
          body: JSON.stringify({ model: "m", stream: false, messages: payload.messages }),
        },
        8000,
      );
    } catch {
      /* 上游不可达是预期情况；提示词在转发前已入队 */
    }
  }

  await waitFor(
    async () => {
      const result = await fetchJson(
        `${base}/api/prompts?session=${encodeURIComponent(session)}&limit=10`,
        {},
        3000,
      );
      if (result.status !== 200 || !result.body) return false;
      const rows = Array.isArray(result.body.rows) ? result.body.rows : [];
      return rows.length >= 2 && rows.some((row) => row.truncated === true);
    },
    10000,
    "提示词落库（含截断记录）",
  );

  return session;
}

async function cleanup() {
  if (cdp) cdp.close();
  if (targetId) {
    fetch(`http://127.0.0.1:${debugPort}/json/close/${targetId}`).catch(() => {});
  }
  const waitExit = (proc) =>
    new Promise((resolve) => {
      if (!proc || proc.exitCode !== null) return resolve();
      const timer = setTimeout(resolve, 3000);
      proc.once("exit", () => {
        clearTimeout(timer);
        resolve();
      });
      try {
        proc.kill();
      } catch {
        clearTimeout(timer);
        resolve();
      }
    });
  if (process.platform === "win32" && chrome && chrome.pid && chrome.exitCode === null) {
    try {
      execFileSync("taskkill", ["/PID", String(chrome.pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
      /* 进程可能已退出 */
    }
  }
  await waitExit(chrome);
  await waitExit(serverProcess);
  await sleep(500);
  if (serverTempDir) {
    if (keepArtifacts || failures.length > 0) {
      console.log(`[info] 测试现场保留：${serverTempDir}`);
    } else {
      try {
        fs.rmSync(serverTempDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
      } catch (error) {
        console.warn(`[warn] 临时目录清理失败：${error.message}`);
      }
    }
  }
  if (userDataDir) {
    try {
      fs.rmSync(userDataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch {
      /* ignore */
    }
  }
}

main()
  .catch((error) => {
    fail("测试主流程", error);
    console.error(error && error.stack ? error.stack : error);
    process.exitCode = 1;
  })
  .finally(async () => {
    await sleep(100);
    await cleanup();
  });
