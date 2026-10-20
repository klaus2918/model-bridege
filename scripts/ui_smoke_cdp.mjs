// 面板 5 页真实渲染冒烟（CDP，任意 Chromium 内核目标）。
// 说明：Rust 版默认打开系统浏览器（无内嵌 WebView2），本脚本适用于：
//   1) Python 回退版 pywebview（EdgeChromium）：
//        set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333
//        dist\Model-Gate-python.exe ...
//   2) 带调试端口的系统浏览器（如 Chrome --remote-debugging-port=9333 打开 /ui/）。
// 运行：node scripts/ui_smoke_cdp.mjs（端口可用 MG_CDP_PORT 覆盖）
const PORT = process.env.MG_CDP_PORT || 9333;
const BASE = `http://127.0.0.1:${PORT}`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function findTarget() {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await (await fetch(`${BASE}/json/list`)).json();
      const page = list.find((t) => t.type === "page" && t.url.includes("/ui/"));
      if (page) return page;
    } catch {}
    await sleep(250);
  }
  throw new Error("未找到 /ui/ 页面目标（CDP 是否开启？）");
}

function connect(url) {
  const ws = new WebSocket(url);
  let nextId = 1;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    }
  });
  const ready = new Promise((resolve, reject) => {
    ws.addEventListener("open", resolve);
    ws.addEventListener("error", reject);
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  return { ws, ready, send };
}

async function main() {
  const target = await findTarget();
  const { ws, ready, send } = connect(target.webSocketDebuggerUrl);
  await ready;
  await send("Runtime.enable");

  const evaluate = async (expression) => {
    const res = await send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (res.exceptionDetails) {
      throw new Error(res.exceptionDetails.exception?.description || "JS 异常");
    }
    return res.result.value;
  };

  const results = {};
  results.title = await evaluate("document.title");
  results.tabs = await evaluate(
    "Array.from(document.querySelectorAll('.tab-btn')).map(b=>b.dataset.tab)"
  );

  const checks = [
    ["overview", "#panel-overview", "#overview-cards .kpi, #overview-cards > *"],
    ["model", "#panel-model", "#model-table tbody tr, #model-table .empty"],
    ["usage", "#panel-usage", "#usage-kpis > *, #usage-kpis .empty"],
    ["config", "#panel-config", "#config-form input, #config-form select"],
    ["logs", "#panel-logs", "#log-viewer *, #log-viewer .empty"],
  ];

  for (const [tab, panel, probe] of checks) {
    await evaluate(`document.querySelector('[data-tab="${tab}"]').click()`);
    await sleep(900);
    const state = await evaluate(`(() => {
      const p = document.querySelector('${panel}');
      const active = document.querySelector('.panel.active')?.id || '';
      const nodes = document.querySelectorAll('${probe}').length;
      return { active, visible: !!p && !p.hidden, nodes, text: (p?.innerText || '').slice(0, 80) };
    })()`);
    results[tab] = state;
    if (state.active !== panel.slice(1)) {
      throw new Error(`标签 ${tab} 未激活对应面板：${state.active}`);
    }
    if (!state.visible) throw new Error(`面板 ${panel} 不可见`);
    if (state.nodes === 0) throw new Error(`面板 ${panel} 没有渲染任何内容`);
  }

  results.consoleErrors = await evaluate(
    "window.__mgErrors ? window.__mgErrors.length : 0"
  );
  console.log(JSON.stringify(results, null, 2));
  ws.close();
}

main().catch((err) => {
  console.error("[ui-smoke] FAIL:", err.message);
  process.exit(1);
});
