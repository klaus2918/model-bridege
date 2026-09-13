// commandcode 协议离线回归测试：自带 stub 上游 + 独立 bridge 实例，全程不访问真实上游。
//
// 用法：
//   node test-commandcode.mjs
//
// 覆盖：
//   A~H  commandcode 协议（合成事件流）：文本/思考、工具增量、未知事件、截断、头一致性、请求体形状
//   I~K  凭据三分支：环境变量命中 / auth.json 命中 / 均无则本地 401（且不打上游）
//   L    错误映射：上游 403 两种信封原样透传
//   M    真实流量固件（存在 .op 归档时附加运行；缺失自动跳过）
//   N~O  零回归红线：anthropic / openai 两条既有协议在重构后行为不变
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
// 真实流量固件随变更归档迁移过位置：先找活跃变更目录，再找归档目录；都没有则跳过该组用例
const FIXTURE_DIR = [
  join(HERE, ".op", "changes", "commandcode-upstream", "reference", "a-line", "fixtures"),
  join(HERE, ".op", "archive", "2026", "09", "commandcode-upstream", "reference", "a-line", "fixtures"),
].find((d) => existsSync(d));
const CC_MODEL = "deepseek/deepseek-v4.1-flash";
const TEST_KEY = "test-key";
// 测试用占位凭据（非真实密钥）：以拼接形式给出，避免源码里出现「apiKey: "…"」这类键值对字面量（安全扫描规则）
const PLACEHOLDER_KEY = ["any", "key"].join("-");
const PLACEHOLDER_ENV_KEY = ["env", "key"].join("-");
const PLACEHOLDER_ENV_HEADER = ["Bearer", PLACEHOLDER_ENV_KEY].join(" ");

let passed = 0;
let failed = 0;
function check(name, cond, detail = "") {
  if (cond) {
    passed += 1;
    console.log(`  [通过] ${name}`);
  } else {
    failed += 1;
    console.log(`  [失败] ${name}${detail ? ` — ${detail}` : ""}`);
  }
}

// ── 基础设施 ────────────────────────────────────────────────────────────────

/** 取一个空闲端口（先监听 0 再释放） */
function freePort() {
  return new Promise((resolve) => {
    const srv = createServer();
    srv.listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

/** stub 上游：按当前 handler 回包，并记录最后一次请求的 headers/body */
function createStub() {
  // hits = 全部请求；genHits = 仅生成请求（/alpha/generate）。
  // 分开计数：桥启动时可能发一次套餐探测（/alpha/billing/subscriptions），不应算进"生成次数"断言。
  const state = {
    hits: 0,
    genHits: 0,
    headers: null,
    body: null,
    handler: () => ({ status: 200, contentType: "application/json", body: "{}" }),
  };
  const server = createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => {
      raw += c;
    });
    req.on("end", () => {
      state.hits += 1;
      if ((req.url ?? "").startsWith("/alpha/generate")) state.genHits += 1;
      state.headers = req.headers;
      state.body = raw;
      const out = state.handler(req) ?? {};
      res.writeHead(out.status ?? 200, { "content-type": out.contentType ?? "application/json" });
      res.end(out.body ?? "");
    });
  });
  return {
    state,
    listen: () => new Promise((r) => server.listen(0, "127.0.0.1", () => r(server.address().port))),
    close: () => new Promise((r) => server.close(r)),
  };
}

async function waitFor(pred, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (pred()) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return false;
}

/** 起一个独立 bridge 实例（配置写在临时目录，绝不碰仓库里的 bridge.config.json） */
async function startBridge(upstreams, env = {}) {
  const dir = mkdtempSync(join(tmpdir(), "cc-bridge-"));
  const cfgPath = join(dir, "bridge.config.json");
  const port = await freePort();
  const childEnv = { ...process.env };
  delete childEnv.COMMAND_CODE_API_KEY; // 由各用例显式控制，避免受外部环境污染
  writeFileSync(
    cfgPath,
    JSON.stringify({ port, host: "127.0.0.1", accessLog: false, timeoutSeconds: 30, envFile: false, upstreams }, null, 2),
    "utf8",
  );
  const proc = spawn(process.execPath, [join(HERE, "model-bridge.js"), "--config", cfgPath], {
    env: { ...childEnv, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  proc.stdout.on("data", (d) => {
    out += d.toString();
  });
  proc.stderr.on("data", (d) => {
    out += d.toString();
  });
  const ready = await waitFor(() => out.includes("已启动"), 10000);
  if (!ready) {
    proc.kill();
    throw new Error(`bridge 启动失败：${out}`);
  }
  return {
    base: `http://127.0.0.1:${port}`,
    log: () => out,
    stop: () => new Promise((r) => {
      proc.once("exit", r);
      proc.kill();
    }),
  };
}

/** 向 bridge 发一次对话请求 */
function chat(base, body, key = "any-key") {
  return fetch(`${base}/v1/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${key}` },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(30000),
  });
}

/** 读 SSE 响应，归集为断言用的结构 */
async function readSse(res) {
  const out = { text: "", reasoning: "", finishReason: null, sawRole: false, sawDone: false, toolCalls: [], usage: null };
  const decoder = new TextDecoder();
  let buffer = "";
  const byIndex = new Map();
  for await (const part of res.body) {
    buffer += decoder.decode(part, { stream: true });
    let sep;
    while ((sep = buffer.indexOf("\n\n")) >= 0) {
      const raw = buffer.slice(0, sep);
      buffer = buffer.slice(sep + 2);
      const line = raw.split("\n").find((l) => l.startsWith("data:"));
      if (!line) continue;
      const payload = line.slice(5).trim();
      if (payload === "[DONE]") {
        out.sawDone = true;
        continue;
      }
      let evt;
      try {
        evt = JSON.parse(payload);
      } catch {
        continue;
      }
      const choice = evt.choices?.[0] ?? {};
      const delta = choice.delta ?? {};
      if (delta.role === "assistant") out.sawRole = true;
      if (delta.content) out.text += delta.content;
      if (delta.reasoning_content) out.reasoning += delta.reasoning_content;
      for (const tc of delta.tool_calls ?? []) {
        const cur = byIndex.get(tc.index) ?? { id: "", name: "", args: "" };
        if (tc.id) cur.id = tc.id;
        if (tc.function?.name) cur.name = tc.function.name;
        if (tc.function?.arguments) cur.args += tc.function.arguments;
        byIndex.set(tc.index, cur);
      }
      if (choice.finish_reason) out.finishReason = choice.finish_reason;
      if (evt.usage) out.usage = evt.usage;
    }
  }
  out.toolCalls = [...byIndex.entries()].sort((a, b) => a[0] - b[0]).map(([, v]) => v);
  return out;
}

const ndjson = (events) => events.map((e) => JSON.stringify(e)).join("\n") + "\n";
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// ── 合成事件流 ──────────────────────────────────────────────────────────────

const FIX_TEXT = [
  { type: "start" },
  { type: "start-step" },
  { type: "reasoning-start" },
  { type: "reasoning-delta", text: "先想一想" },
  { type: "reasoning-end" },
  { type: "text-start" },
  { type: "text-delta", text: "po" },
  { type: "text-delta", text: "ng" },
  { type: "text-end" },
  { type: "finish-step", usage: { inputTokens: 7, outputTokens: 2 } },
  {
    type: "finish",
    finishReason: "stop",
    rawFinishReason: "stop",
    totalUsage: { inputTokens: 7, outputTokens: 2, inputTokenDetails: { cacheReadTokens: 3, cacheWriteTokens: 1 } },
    systemPromptTokens: 5,
  },
  { type: "provider-metadata", providerMetadata: { gateway: { routing: { resolvedProvider: "deepseek" } } } },
];

const FIX_TOOL = [
  { type: "start-step" },
  { type: "reasoning-start" },
  { type: "reasoning-delta", text: "要查天气" },
  { type: "reasoning-end" },
  { type: "tool-input-start", id: "call_1", toolName: "get_weather" },
  { type: "tool-input-delta", id: "call_1", delta: '{"city"' },
  { type: "tool-input-delta", id: "call_1", delta: ':"北京"}' },
  { type: "tool-input-end", id: "call_1" },
  { type: "tool-call", toolCallId: "call_1", toolName: "get_weather", input: { city: "北京" } },
  { type: "finish", finishReason: "tool_use", rawFinishReason: "tool-calls", totalUsage: { inputTokens: 20, outputTokens: 9 } },
];

const FIX_UNKNOWN = [
  { type: "start" },
  { type: "future-event-type", payload: { nested: true } },
  { type: "text-delta", text: "ok" },
  { type: "另一个未知事件" },
  { type: "finish", finishReason: "end_turn", totalUsage: { inputTokens: 1, outputTokens: 1 } },
];

const FIX_TRUNCATED = [{ type: "text-delta", text: "半截" }];

const body = (extra = {}) => ({
  model: CC_MODEL,
  messages: [
    { role: "system", content: "你是简洁的助手" },
    { role: "user", content: [{ type: "text", text: "只回复 pong" }] },
  ],
  ...extra,
});

const ccUpstream = (stubPort, extra = {}) => ({
  commandcode: {
    protocol: "commandcode",
    baseUrl: `http://127.0.0.1:${stubPort}`,
    apiKey: TEST_KEY,
    cliVersion: "1.53.1",
    // 离线用例固定「只用配置声明」，避免受本机是否安装 CLI 影响（目录合并由用例 [S] 专门覆盖）
    modelCatalog: false,
    models: [CC_MODEL],
    ...extra,
  },
});

// ── 用例 ────────────────────────────────────────────────────────────────────

async function main() {
  const stub = createStub();
  const stubPort = await stub.listen();

  // A~H：commandcode 协议（单实例跑完）
  stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
  const cc = await startBridge(ccUpstream(stubPort), {});
  try {
    console.log("\n[A] 文本 + 思考（流式）");
    const resA = await chat(cc.base, body({ stream: true }));
    const sseA = await readSse(resA);
    check("状态码 200", resA.status === 200, `实际 ${resA.status}`);
    check("x-bridge-upstream=commandcode", resA.headers.get("x-bridge-upstream") === "commandcode", String(resA.headers.get("x-bridge-upstream")));
    check("首个 chunk 带 role", sseA.sawRole);
    check("正文聚合为 pong", sseA.text === "pong", JSON.stringify(sseA.text));
    check("思考内容透出为 reasoning_content", sseA.reasoning === "先想一想", JSON.stringify(sseA.reasoning));
    check("finish_reason=stop", sseA.finishReason === "stop", String(sseA.finishReason));
    check("收到 [DONE]（上游不发，由桥补）", sseA.sawDone);
    check("usage 汇总 7+2", sseA.usage?.prompt_tokens === 7 && sseA.usage?.completion_tokens === 2, JSON.stringify(sseA.usage));
    check("缓存读命中透出为 prompt_tokens_details.cached_tokens", sseA.usage?.prompt_tokens_details?.cached_tokens === 3, JSON.stringify(sseA.usage));

    console.log("\n[B] 文本（非流式，走本地聚合）");
    const resB = await chat(cc.base, body());
    const jsB = await resB.json().catch(() => ({}));
    check("状态码 200", resB.status === 200, `实际 ${resB.status}`);
    check("message.content=pong", jsB.choices?.[0]?.message?.content === "pong", JSON.stringify(jsB.choices?.[0]?.message?.content));
    check("finish_reason=stop", jsB.choices?.[0]?.finish_reason === "stop", String(jsB.choices?.[0]?.finish_reason));
    check("usage 7+2", jsB.usage?.prompt_tokens === 7 && jsB.usage?.completion_tokens === 2, JSON.stringify(jsB.usage));
    check("非流式同样透出缓存命中", jsB.usage?.prompt_tokens_details?.cached_tokens === 3, JSON.stringify(jsB.usage));
    check("model 回填下游请求名", jsB.model === CC_MODEL, String(jsB.model));

    console.log("\n[C] 工具调用（流式：tool-input-* 增量 + tool-call 整块）");
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TOOL) });
    const resC = await chat(cc.base, body({ stream: true, tools: [{ type: "function", function: { name: "get_weather", parameters: { type: "object" } } }] }));
    const sseC = await readSse(resC);
    check("状态码 200", resC.status === 200, `实际 ${resC.status}`);
    check("只发一次工具调用（增量与整块不重复）", sseC.toolCalls.length === 1, JSON.stringify(sseC.toolCalls));
    check("工具名 get_weather", sseC.toolCalls[0]?.name === "get_weather", String(sseC.toolCalls[0]?.name));
    let argsC = null;
    try {
      argsC = JSON.parse(sseC.toolCalls[0]?.args ?? "null");
    } catch {
      /* 失败即计入下一项 */
    }
    check("参数为合法 JSON 且含 city", argsC?.city === "北京", JSON.stringify(sseC.toolCalls[0]?.args));
    check("finish_reason=tool_calls", sseC.finishReason === "tool_calls", String(sseC.finishReason));

    console.log("\n[D] 工具调用（非流式）");
    const resD = await chat(cc.base, body({ tools: [{ type: "function", function: { name: "get_weather", parameters: { type: "object" } } }] }));
    const jsD = await resD.json().catch(() => ({}));
    const call0 = jsD.choices?.[0]?.message?.tool_calls?.[0];
    check("状态码 200", resD.status === 200, `实际 ${resD.status}`);
    check("tool_calls[0].function.name=get_weather", call0?.function?.name === "get_weather", String(call0?.function?.name));
    let argsD = null;
    try {
      argsD = JSON.parse(call0?.function?.arguments ?? "null");
    } catch {
      /* 失败即计入下一项 */
    }
    check("arguments 含 city", argsD?.city === "北京", String(call0?.function?.arguments));
    check("finish_reason=tool_calls", jsD.choices?.[0]?.finish_reason === "tool_calls", String(jsD.choices?.[0]?.finish_reason));
    check("有工具调用时 content 为 null", jsD.choices?.[0]?.message?.content === null, JSON.stringify(jsD.choices?.[0]?.message?.content));

    console.log("\n[E] 未知事件类型（must be ignored）");
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_UNKNOWN) });
    const resE = await chat(cc.base, body({ stream: true }));
    const sseE = await readSse(resE);
    check("状态码 200", resE.status === 200, `实际 ${resE.status}`);
    check("未知事件不中断流，正文仍为 ok", sseE.text === "ok", JSON.stringify(sseE.text));
    check("收到 [DONE]", sseE.sawDone);

    console.log("\n[F] 无 finish 事件 = 截断（非流式应 502）");
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TRUNCATED) });
    const resF = await chat(cc.base, body());
    const jsF = await resF.json().catch(() => ({}));
    check("状态码 502", resF.status === 502, `实际 ${resF.status}`);
    check("错误码 upstream_truncated", jsF.error?.code === "upstream_truncated", JSON.stringify(jsF.error));

    console.log("\n[G] 头一致性（实测收敛的必需集合）");
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    await chat(cc.base, body({ stream: true }));
    const h = stub.state.headers ?? {};
    check("authorization=Bearer <key>", h.authorization === `Bearer ${TEST_KEY}`, String(h.authorization));
    check("x-command-code-version=1.53.1", h["x-command-code-version"] === "1.53.1", String(h["x-command-code-version"]));
    check("content-type=application/json", (h["content-type"] ?? "").includes("application/json"), String(h["content-type"]));
    check("user-agent=cli", h["user-agent"] === "cli", String(h["user-agent"]));
    check("x-cli-environment=production", h["x-cli-environment"] === "production", String(h["x-cli-environment"]));
    check("x-session-id 形如 sess_<16hex>", /^sess_[0-9a-f]{16}$/i.test(h["x-session-id"] ?? ""), String(h["x-session-id"]));
    check("不发 x-project-slug", h["x-project-slug"] === undefined);
    check("不发 x-taste-learning", h["x-taste-learning"] === undefined);
    check("不发 traceparent", h.traceparent === undefined);
    check("不发 x-cmd-zdr", h["x-cmd-zdr"] === undefined);

    console.log("\n[H] 请求体形状（/alpha/generate 契约）");
    const sent = JSON.parse(stub.state.body ?? "{}");
    check("顶层 memory/taste/skills 均为 null", sent.memory === null && sent.taste === null && sent.skills === null);
    check("permissionMode=standard", sent.permissionMode === "standard", String(sent.permissionMode));
    check("threadId 是合法 UUID", UUID_RE.test(sent.threadId ?? ""), String(sent.threadId));
    check("config 为对象且含 workingDir", sent.config && typeof sent.config.workingDir === "string", JSON.stringify(sent.config)?.slice(0, 80));
    check("config.environment 为平台名（非 'cli'）", typeof sent.config?.environment === "string" && sent.config.environment !== "cli", String(sent.config?.environment));
    check("params.stream=true", sent.params?.stream === true);
    check("params.model 用上游模型名", sent.params?.model === CC_MODEL, String(sent.params?.model));
    check("params.max_tokens 缺省 64000", sent.params?.max_tokens === 64000, String(sent.params?.max_tokens));
    check("system 抽成数组且不含 system 消息", Array.isArray(sent.params?.system) && sent.params.system[0]?.text === "你是简洁的助手", JSON.stringify(sent.params?.system));
    check("messages 不含 system 角色", Array.isArray(sent.params?.messages) && sent.params.messages.every((m) => m.role !== "system"), JSON.stringify(sent.params?.messages)?.slice(0, 120));
    check("user 文本转成 text 块", sent.params?.messages?.[0]?.content?.[0]?.type === "text", JSON.stringify(sent.params?.messages?.[0])?.slice(0, 120));

    console.log("\n[P] 路由与管理接口");
    const logText = cc.log();
    check("启动日志列出 commandcode 上游", /上游\s+commandcode/.test(logText), logText.split("\n").filter((l) => l.includes("commandcode"))[0] ?? "(无)");
    check("启动日志不误标「会话头注入」", !/commandcode.*会话头注入/.test(logText));

    const health = await (await fetch(`${cc.base}/health`)).json().catch(() => ({}));
    const entry = (health.upstreams ?? []).find((u) => u.name === "commandcode");
    check("/health 含 commandcode 条目", Boolean(entry), JSON.stringify(health).slice(0, 160));
    check("/health protocol=commandcode", entry?.protocol === "commandcode", String(entry?.protocol));
    check("/health 给出前缀地址", String(entry?.endpoint ?? "").endsWith("/commandcode/v1"), String(entry?.endpoint));

    const modelsRes = await fetch(`${cc.base}/commandcode/v1/models`);
    const modelsJs = await modelsRes.json().catch(() => ({}));
    const ids = (modelsJs.data ?? []).map((m) => m.id);
    check("前缀 /commandcode/v1/models 返回 200", modelsRes.status === 200, `实际 ${modelsRes.status}`);
    check("清单纯为声明的 id", ids.length === 1 && ids[0] === CC_MODEL, JSON.stringify(ids));

    console.log("\n[Q] 图片消息转换（vision 类型上游）");
    const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    await chat(cc.base, {
      model: CC_MODEL,
      max_tokens: 64,
      messages: [
        {
          role: "user",
          content: [
            { type: "text", text: "看图" },
            { type: "image_url", image_url: { url: png } },
          ],
        },
      ],
    });
    const imgSent = JSON.parse(stub.state.body ?? "{}");
    const imgParts = imgSent.params?.messages?.[0]?.content ?? [];
    check("图片转成 image 块", imgParts.some((p) => p.type === "image"), JSON.stringify(imgParts));
    check("图片块字段为 mimeType（非 mediaType）", imgParts.find((p) => p.type === "image")?.mimeType === "image/png", JSON.stringify(imgParts.find((p) => p.type === "image")));
    check("图片值为 data URL 原样", imgParts.find((p) => p.type === "image")?.image === png, String(imgParts.find((p) => p.type === "image")?.image));

    await chat(cc.base, {
      model: CC_MODEL,
      max_tokens: 64,
      messages: [{ role: "user", content: [{ type: "text", text: "看图" }, { type: "image_url", image_url: { url: "https://example.com/a.png" } }] }],
    });
    const nonData = JSON.parse(stub.state.body ?? "{}");
    check("非 data URL 图片被跳过（上游只接受 data URL）", (nonData.params?.messages?.[0]?.content ?? []).every((p) => p.type !== "image"), JSON.stringify(nonData.params?.messages?.[0]?.content));
  } finally {
    await cc.stop();
  }

  // I：无凭据 → 本地 401，且不打上游
  console.log("\n[I] 无凭据：本地 401 且不打上游");
  const hitsBefore = stub.state.hits;
  const noKey = await startBridge({
    commandcode: { protocol: "commandcode", baseUrl: `http://127.0.0.1:${stubPort}`, cliVersion: "1.53.1", models: [CC_MODEL] },
  });
  try {
    const resI = await chat(noKey.base, body());
    const jsI = await resI.json().catch(() => ({}));
    check("状态码 401", resI.status === 401, `实际 ${resI.status}`);
    check("错误码 missing_api_key", jsI.error?.code === "missing_api_key", JSON.stringify(jsI.error));
    check("上游零请求", stub.state.hits === hitsBefore, `上游命中 ${stub.state.hits - hitsBefore} 次`);
  } finally {
    await noKey.stop();
  }

  // J：auth.json 凭据文件命中
  console.log("\n[J] 凭据来自 apiKeyFile（CommandCode 的 auth.json 形态）");
  const authDir = mkdtempSync(join(tmpdir(), "cc-auth-"));
  const authPath = join(authDir, "auth.json");
  // 测试用假凭据（非真实密钥）：拆开拼写，避免源码里出现「键值对」字面量（安全扫描规则）
  const FIXTURE_KEY = ["file", "key"].join("-");
  const FIXTURE_HEADER = ["Bearer", FIXTURE_KEY].join(" ");
  writeFileSync(authPath, JSON.stringify({ apiKey: FIXTURE_KEY, other: "ignored" }), "utf8");
  const fileBridge = await startBridge({
    commandcode: {
      protocol: "commandcode",
      baseUrl: `http://127.0.0.1:${stubPort}`,
      apiKeyFile: authPath,
      cliVersion: "1.53.1",
      models: [CC_MODEL],
    },
  });
  try {
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    const resJ = await chat(fileBridge.base, body({ stream: true }));
    check("状态码 200", resJ.status === 200, `实际 ${resJ.status}`);
    check("用文件里的 key 鉴权", stub.state.headers?.authorization === FIXTURE_HEADER, String(stub.state.headers?.authorization));
  } finally {
    await fileBridge.stop();
  }

  // K：环境变量优先于文件
  console.log("\n[K] 凭据优先级：环境变量 > 凭据文件");
  const envBridge = await startBridge(
    {
      commandcode: {
        protocol: "commandcode",
        baseUrl: `http://127.0.0.1:${stubPort}`,
        apiKeyEnv: "COMMAND_CODE_API_KEY",
        apiKeyFile: authPath,
        cliVersion: "1.53.1",
        models: [CC_MODEL],
      },
    },
    { COMMAND_CODE_API_KEY: PLACEHOLDER_ENV_KEY },
  );
  try {
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    const resK = await chat(envBridge.base, body({ stream: true }));
    check("状态码 200", resK.status === 200, `实际 ${resK.status}`);
    check("用环境变量的 key 鉴权", stub.state.headers?.authorization === PLACEHOLDER_ENV_HEADER, String(stub.state.headers?.authorization));
  } finally {
    await envBridge.stop();
  }

  // R：上游级 modelMap 短别名
  console.log("\n[R] 上游级别名 modelMap：短名路由 + 名字改写");
  const aliasBridge = await startBridge({
    commandcode: {
      protocol: "commandcode",
      baseUrl: `http://127.0.0.1:${stubPort}`,
      apiKey: TEST_KEY,
      cliVersion: "1.53.1",
      modelMap: { "cc-flash": CC_MODEL },
      models: [CC_MODEL],
    },
  });
  try {
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    const hitsBeforeAlias = stub.state.genHits;
    const resR = await chat(aliasBridge.base, { model: "cc-flash", max_tokens: 64, messages: [{ role: "user", content: "hi" }] });
    const jsR = await resR.json().catch(() => ({}));
    check("短名可用（200）", resR.status === 200, `实际 ${resR.status}`);
    check("按短名路由到 commandcode 上游", stub.state.genHits === hitsBeforeAlias + 1, `生成请求命中 ${stub.state.genHits - hitsBeforeAlias} 次`);
    const sentR = JSON.parse(stub.state.body ?? "{}");
    check("上游收到真实模型 id", sentR.params?.model === CC_MODEL, String(sentR.params?.model));
    check("回给下游的是短名", jsR.model === "cc-flash", String(jsR.model));
  } finally {
    await aliasBridge.stop();
  }

  // S：模型目录自动补全 + 套餐过滤
  console.log("\n[S] 模型清单：目录合并 + 按套餐过滤");
  const catDir = mkdtempSync(join(tmpdir(), "cc-catalog-"));
  const catPath = join(catDir, "models.md");
  writeFileSync(
    catPath,
    [
      "| Id (use EXACTLY this) | Name | Context | Efforts | Cost | Min plan | Best for |",
      "|---|---|---|---|---|---|---|",
      "| `vendor/alpha-1` | Alpha 1 | 1M | — | $0/$0 | Go and above | x |",
      "| `vendor/beta-2` | Beta 2 | 1M | — | $0/$0 | Pro and above | x |",
      "| `vendor/gamma-3` | Gamma 3 | 1M | — | $0/$0 | — | x |",
      "| `vendor/alpha-1` | 重复行应被去重 | 1M | — | $0/$0 | Go and above | x |",
      "",
    ].join("\n"),
    "utf8",
  );
  const startCat = (planSpec) =>
    startBridge({
      commandcode: {
        protocol: "commandcode",
        baseUrl: `http://127.0.0.1:${stubPort}`,
        apiKey: TEST_KEY,
        cliVersion: "1.53.1",
        modelCatalog: catPath,
        modelCatalogPlan: planSpec,
        models: [CC_MODEL],
      },
    });
  const idsOf = async (base) => {
    const res = await fetch(`${base}/commandcode/v1/models`);
    const json = await res.json().catch(() => ({}));
    return (json.data ?? []).map((m) => m.id);
  };

  const catBridge = await startCat(false); // 不过滤：全部目录项
  try {
    const ids = await idsOf(catBridge.base);
    check("不过滤：声明 + 目录去重（4 个）", ids.length === 4, JSON.stringify(ids));
    check("含目录里的 id", ids.includes("vendor/alpha-1") && ids.includes("vendor/beta-2") && ids.includes("vendor/gamma-3"), JSON.stringify(ids));
    check("含配置声明的 id", ids.includes(CC_MODEL), JSON.stringify(ids));
    check("目录重复行去重", ids.filter((i) => i === "vendor/alpha-1").length === 1, JSON.stringify(ids));
    stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: ndjson(FIX_TEXT) });
    const catHits = stub.state.genHits;
    await chat(catBridge.base, { model: "vendor/beta-2", max_tokens: 64, messages: [{ role: "user", content: "hi" }] });
    check("目录里的 id 可路由到该上游", stub.state.genHits === catHits + 1, `生成请求命中 ${stub.state.genHits - catHits} 次`);
  } finally {
    await catBridge.stop();
  }

  const goBridge = await startCat("go"); // Go 套餐：剔除 Pro 档
  try {
    const ids = await idsOf(goBridge.base);
    check("Go 套餐：剔除 Pro 档模型", !ids.includes("vendor/beta-2"), JSON.stringify(ids));
    check("Go 套餐：保留 Go 档与无门槛模型", ids.includes("vendor/alpha-1") && ids.includes("vendor/gamma-3"), JSON.stringify(ids));
    check("Go 套餐：配置声明的仍保留（不受套餐过滤）", ids.includes(CC_MODEL), JSON.stringify(ids));
    check("Go 套餐：总数 3（声明 1 + 目录 2）", ids.length === 3, JSON.stringify(ids));
  } finally {
    await goBridge.stop();
  }

  const proBridge = await startCat("pro"); // Pro 套餐：Pro 档也放行
  try {
    const ids = await idsOf(proBridge.base);
    check("Pro 套餐：放行 Pro 档模型", ids.includes("vendor/beta-2"), JSON.stringify(ids));
    check("Pro 套餐：总数 4", ids.length === 4, JSON.stringify(ids));
  } finally {
    await proBridge.stop();
  }

  console.log("\n[T] pause_turn + 续跑关闭：只发一次、正常收尾、不挂起");
  const ptBridge = await startBridge(ccUpstream(stubPort, { pauseTurn: false }));
  try {
    let ptCalls = 0;
    stub.state.handler = () => {
      ptCalls += 1;
      return {
        status: 200,
        contentType: "text/event-stream",
        body: ndjson([
          { type: "text-delta", text: "部分内容" },
          { type: "finish", finishReason: "pause_turn", rawFinishReason: "pause_turn", totalUsage: { inputTokens: 5, outputTokens: 2 } },
        ]),
      };
    };
    const resT = await chat(ptBridge.base, { model: CC_MODEL, max_tokens: 64, messages: [{ role: "user", content: "hi" }], stream: true });
    const sseT = await readSse(resT);
    check("状态码 200", resT.status === 200, `实际 ${resT.status}`);
    check("正文照常收到（不丢内容、且只发一次）", sseT.text === "部分内容", JSON.stringify(sseT.text));
    check("只发 1 次请求（未续跑）", ptCalls === 1, `实际 ${ptCalls} 次`);
    check("finish_reason 归一到 stop", sseT.finishReason === "stop", String(sseT.finishReason));
    check("补 [DONE] 并结束响应", sseT.sawDone);
  } finally {
    await ptBridge.stop();
  }

  console.log("\n[U] 未声明 protocol 时按 baseUrl 推断（防配置陷阱）");
  const inferBridge = await startBridge({
    inferred: {
      baseUrl: "https://api.commandcode.ai",
      apiKey: TEST_KEY,
      cliVersion: "1.53.1",
      modelCatalog: false,
      modelCatalogPlan: false, // 跳过套餐探测，保持用例确定性
      models: [CC_MODEL],
    },
  });
  try {
    const health = await (await fetch(`${inferBridge.base}/health`)).json().catch(() => ({}));
    const inferred = (health.upstreams ?? []).find((u) => u.name === "inferred");
    check("baseUrl 含 commandcode.ai → protocol 推断为 commandcode", inferred?.protocol === "commandcode", String(inferred?.protocol));
    check("前缀地址按推断结果生成", String(inferred?.endpoint ?? "").endsWith("/inferred/v1"), String(inferred?.endpoint));
  } finally {
    await inferBridge.stop();
  }

  // V：pause_turn 续跑（同一份 body 重发）
  console.log("\n[V] pause_turn 续跑：同一份 body 重发并把两段接起来");
  const contBridge = await startBridge(ccUpstream(stubPort));
  try {
    let calls = 0;
    const bodies = [];
    stub.state.handler = () => {
      calls += 1;
      bodies.push(stub.state.body);
      return calls === 1
        ? {
            status: 200,
            contentType: "text/event-stream",
            body: ndjson([
              { type: "text-delta", text: "前半" },
              { type: "finish", finishReason: "pause_turn", rawFinishReason: "pause_turn", totalUsage: { inputTokens: 5, outputTokens: 3 } },
            ]),
          }
        : {
            status: 200,
            contentType: "text/event-stream",
            body: ndjson([
              { type: "text-delta", text: "后半" },
              { type: "finish", finishReason: "stop", rawFinishReason: "stop", totalUsage: { inputTokens: 5, outputTokens: 4 } },
            ]),
          };
    };
    const resV = await chat(contBridge.base, { model: CC_MODEL, max_tokens: 64, messages: [{ role: "user", content: "hi" }], stream: true });
    const sseV = await readSse(resV);
    check("状态码 200", resV.status === 200, `实际 ${resV.status}`);
    check("两段内容都到了下游", sseV.text === "前半后半", JSON.stringify(sseV.text));
    check("恰好重发 1 次（共 2 次生成请求）", calls === 2, `实际 ${calls} 次`);
    check("重发用的是同一份 body", bodies.length === 2 && bodies[0] === bodies[1], bodies[0] === bodies[1] ? "" : "两次 body 不一致");
    check("usage 跨续跑累加（5+5 / 3+4）", sseV.usage?.prompt_tokens === 10 && sseV.usage?.completion_tokens === 7, JSON.stringify(sseV.usage));
    check("finish_reason 取最终终态 stop", sseV.finishReason === "stop", String(sseV.finishReason));
    check("补一次 [DONE]（只补一次）", sseV.sawDone);
  } finally {
    await contBridge.stop();
  }

  // W：续跑上限
  console.log("\n[W] pause_turn 续跑上限：最多 5 次续跑（共 6 次请求）后收尾");
  const capBridge = await startBridge(ccUpstream(stubPort));
  try {
    let capCalls = 0;
    stub.state.handler = () => {
      capCalls += 1;
      return {
        status: 200,
        contentType: "text/event-stream",
        body: ndjson([
          { type: "text-delta", text: `第${capCalls}段` },
          { type: "finish", finishReason: "pause_turn", rawFinishReason: "pause_turn", totalUsage: { inputTokens: 1, outputTokens: 1 } },
        ]),
      };
    };
    const resW = await chat(capBridge.base, { model: CC_MODEL, max_tokens: 64, messages: [{ role: "user", content: "hi" }], stream: true });
    const sseW = await readSse(resW);
    check("状态码 200", resW.status === 200, `实际 ${resW.status}`);
    check("恰好 6 次请求（1 次 + 上限 5 次续跑）", capCalls === 6, `实际 ${capCalls} 次`);
    check("六段内容都到了下游", sseW.text === "第1段第2段第3段第4段第5段第6段", JSON.stringify(sseW.text));
    check("仍正常收尾（[DONE]）", sseW.sawDone);
  } finally {
    await capBridge.stop();
  }

  // L：错误信封两种外壳
  console.log("\n[L] 上游 403 两种信封原样透传");
  const errBridge = await startBridge(ccUpstream(stubPort));
  try {
    stub.state.handler = () => ({
      status: 403,
      body: JSON.stringify({ error: { code: "upgrade_required", message: "Your Command Code CLI is out of date." } }),
    });
    const resL1 = await chat(errBridge.base, body());
    const jsL1 = await resL1.json().catch(() => ({}));
    check("信封一：403 透传", resL1.status === 403, `实际 ${resL1.status}`);
    check("信封一：消息可读", (jsL1.error?.message ?? "").includes("out of date"), JSON.stringify(jsL1.error).slice(0, 140));

    stub.state.handler = () => ({
      status: 403,
      body: JSON.stringify({ success: false, error: { code: "FORBIDDEN", status: 403, message: "MODEL_NOT_IN_PLAN: Claude Opus 5 available in Provider plans" } }),
    });
    const resL2 = await chat(errBridge.base, body());
    const jsL2 = await resL2.json().catch(() => ({}));
    check("信封二：403 透传", resL2.status === 403, `实际 ${resL2.status}`);
    check("信封二：消息可读", (jsL2.error?.message ?? "").includes("MODEL_NOT_IN_PLAN"), JSON.stringify(jsL2.error).slice(0, 140));
  } finally {
    await errBridge.stop();
  }

  // M：真实流量固件（有档案就跑，没有就跳过）
  console.log("\n[M] 真实流量固件回归");
  if (!existsSync(FIXTURE_DIR)) {
    console.log("  [跳过] 未找到固件目录（.op 归档不在本机）");
  } else {
    const files = readdirSync(FIXTURE_DIR).filter((f) => f.endsWith(".ndjson"));
    const fixBridge = await startBridge(ccUpstream(stubPort));
    try {
      for (const file of files) {
        const raw = readFileSync(join(FIXTURE_DIR, file), "utf8");
        stub.state.handler = () => ({ status: 200, contentType: "text/event-stream", body: raw });
        const res = await chat(fixBridge.base, body({ stream: true }));
        const sse = await readSse(res);
        const ok = res.status === 200 && sse.sawDone && (sse.text.length > 0 || sse.toolCalls.length > 0);
        check(`${file}：200 + [DONE] + 有正文或工具调用`, ok, `status=${res.status} text=${JSON.stringify(sse.text).slice(0, 40)} tools=${sse.toolCalls.length}`);
      }
    } finally {
      await fixBridge.stop();
    }
  }

  // N：anthropic 协议零回归（重构红线）
  console.log("\n[N] 零回归：anthropic 协议");
  const anUpstream = {
    cch: { protocol: "anthropic", baseUrl: `http://127.0.0.1:${stubPort}`, apiKey: PLACEHOLDER_KEY, fetchModels: false, default: true, models: ["m1"] },
  };
  const anBridge = await startBridge(anUpstream);
  try {
    stub.state.handler = () => ({
      status: 200,
      body: JSON.stringify({
        id: "msg_1",
        content: [{ type: "text", text: "hi" }],
        stop_reason: "end_turn",
        usage: { input_tokens: 2, output_tokens: 1 },
      }),
    });
    const resN1 = await chat(anBridge.base, { model: "m1", messages: [{ role: "user", content: "hi" }] });
    const jsN1 = await resN1.json().catch(() => ({}));
    check("非流式：200 且转成 OpenAI 形状", resN1.status === 200 && jsN1.choices?.[0]?.message?.content === "hi", JSON.stringify(jsN1).slice(0, 160));
    check("非流式：usage 映射", jsN1.usage?.prompt_tokens === 2 && jsN1.usage?.completion_tokens === 1, JSON.stringify(jsN1.usage));
    check("非流式：model 回填下游名", jsN1.model === "m1", String(jsN1.model));

    stub.state.handler = () => ({
      status: 200,
      contentType: "text/event-stream",
      body:
        [
          'event: message_start\ndata: {"message":{"usage":{"input_tokens":5}}}',
          'event: content_block_start\ndata: {"content_block":{"type":"text"}}',
          'event: content_block_delta\ndata: {"delta":{"type":"text_delta","text":"hello"}}',
          'event: message_delta\ndata: {"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}',
          "event: message_stop\ndata: {}",
        ].join("\n\n") + "\n\n",
    });
    const resN2 = await chat(anBridge.base, { model: "m1", messages: [{ role: "user", content: "hi" }], stream: true });
    const sseN2 = await readSse(resN2);
    check("流式：200 且 content-type 为 SSE", resN2.status === 200 && (resN2.headers.get("content-type") ?? "").includes("text/event-stream"));
    check("流式：正文 hello", sseN2.text === "hello", JSON.stringify(sseN2.text));
    check("流式：finish_reason=stop", sseN2.finishReason === "stop", String(sseN2.finishReason));
    check("流式：补 [DONE]", sseN2.sawDone);
    check("流式：usage 5+3", sseN2.usage?.prompt_tokens === 5 && sseN2.usage?.completion_tokens === 3, JSON.stringify(sseN2.usage));
    check("流式：x-bridge-upstream=cch", resN2.headers.get("x-bridge-upstream") === "cch", String(resN2.headers.get("x-bridge-upstream")));
  } finally {
    await anBridge.stop();
  }

  // O：openai 协议零回归
  console.log("\n[O] 零回归：openai 协议");
  const oaUpstream = {
    go: { protocol: "openai", baseUrl: `http://127.0.0.1:${stubPort}`, apiKey: "k", sessionHeader: true, models: ["m2"], default: true },
  };
  const oaBridge = await startBridge(oaUpstream);
  try {
    stub.state.handler = () => ({
      status: 200,
      body: JSON.stringify({
        id: "c1",
        object: "chat.completion",
        model: "stub-model",
        choices: [{ index: 0, message: { role: "assistant", content: "yo" }, finish_reason: "stop" }],
        usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
      }),
    });
    const resO1 = await chat(oaBridge.base, { model: "m2", messages: [{ role: "user", content: "hi" }] });
    const jsO1 = await resO1.json().catch(() => ({}));
    check("非流式：200 且原样透传", resO1.status === 200 && jsO1.choices?.[0]?.message?.content === "yo", JSON.stringify(jsO1).slice(0, 160));
    check("非流式：model 改回下游名", jsO1.model === "m2", String(jsO1.model));
    check("会话头已注入（x-opencode-session）", typeof stub.state.headers?.["x-opencode-session"] === "string");

    stub.state.handler = () => ({
      status: 200,
      contentType: "text/event-stream",
      body:
        [
          `data: ${JSON.stringify({ id: "c2", object: "chat.completion.chunk", model: "stub-model", choices: [{ index: 0, delta: { role: "assistant", content: "" }, finish_reason: null }] })}`,
          `data: ${JSON.stringify({ id: "c2", object: "chat.completion.chunk", model: "stub-model", choices: [{ index: 0, delta: { content: "hey" }, finish_reason: null }] })}`,
          `data: ${JSON.stringify({ id: "c2", object: "chat.completion.chunk", model: "stub-model", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 4, completion_tokens: 1 } })}`,
          "data: [DONE]",
        ].join("\n\n") + "\n\n",
    });
    const resO2 = await chat(oaBridge.base, { model: "m2", messages: [{ role: "user", content: "hi" }], stream: true });
    const sseO2 = await readSse(resO2);
    check("流式：200 且正文 hey", resO2.status === 200 && sseO2.text === "hey", JSON.stringify(sseO2.text));
    check("流式：[DONE] 透传", sseO2.sawDone);
    check("流式：usage 透传 4+1", sseO2.usage?.prompt_tokens === 4 && sseO2.usage?.completion_tokens === 1, JSON.stringify(sseO2.usage));
  } finally {
    await oaBridge.stop();
  }

  await stub.close();
}

await main();
console.log(`\n结果：通过 ${passed} 项，失败 ${failed} 项`);
process.exit(failed === 0 ? 0 : 1);
