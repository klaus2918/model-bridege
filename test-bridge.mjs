// model-bridge 冒烟测试：一条命令验证 8900 入口的每一条链路。
//
// 用法：
//   node test-bridge.mjs                                    # 测 http://127.0.0.1:8900
//   node test-bridge.mjs --url http://127.0.0.1:8901 --model deepseek-flash
//
// 覆盖：健康检查、模型列表、非流式对话、流式对话、工具调用、强制 tool_choice 降级、
//      多轮续接（依赖 thinking 回传）、错误路径。
// 注意：上游是 thinking 模型，思考内容同样计入 max_tokens 输出预算，因此测试统一给 2048。
const args = process.argv.slice(2);
const pick = (k, d) => {
  const i = args.indexOf(`--${k}`);
  return i >= 0 ? args[i + 1] : d;
};
const BASE = pick("url", process.env.BRIDGE_URL ?? "http://127.0.0.1:8900");
const MODEL = pick("model", process.env.BRIDGE_MODEL ?? "deepseek-flash");
const KEY = pick("key", "any-key");
const MAX_TOKENS = Number(pick("max-tokens", 2048));

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
function warn(name, detail = "") {
  console.log(`  [注意] ${name}${detail ? ` — ${detail}` : ""}`);
}

function call(path, body) {
  return fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${KEY}` },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(180000),
  });
}

function simpleBody(extra = {}) {
  return {
    model: MODEL,
    max_tokens: MAX_TOKENS,
    messages: [{ role: "user", content: "只回复两个字：pong" }],
    ...extra,
  };
}

const WEATHER_TOOL = {
  type: "function",
  function: {
    name: "get_weather",
    description: "查询指定城市的天气",
    parameters: {
      type: "object",
      properties: { city: { type: "string", description: "城市名" } },
      required: ["city"],
    },
  },
};

// ---- 1. 健康检查 ----
async function t1() {
  console.log("\n[1] GET /health");
  const res = await fetch(`${BASE}/health`, { signal: AbortSignal.timeout(10000) });
  const body = await res.json().catch(() => ({}));
  check("状态码 200", res.status === 200, `实际 ${res.status}`);
  check("含 upstream 字段", typeof body.upstream === "string", JSON.stringify(body).slice(0, 120));
  console.log(`      上游: ${body.upstream}`);
}

// ---- 2. 模型列表 ----
async function t2() {
  console.log("\n[2] GET /v1/models");
  const res = await fetch(`${BASE}/v1/models`, { signal: AbortSignal.timeout(10000) });
  const body = await res.json().catch(() => ({}));
  const ids = (body.data ?? []).map((m) => m.id);
  check("状态码 200", res.status === 200, `实际 ${res.status}`);
  check("返回非空模型列表", ids.length > 0, JSON.stringify(body).slice(0, 120));
  check(`包含 ${MODEL}`, ids.includes(MODEL), `实际 ${ids.join(", ")}`);
  console.log(`      模型: ${ids.join(", ")}`);
}

// ---- 3. 非流式对话 ----
async function t3() {
  console.log("\n[3] POST /v1/chat/completions（非流式）");
  const res = await call("/v1/chat/completions", simpleBody());
  const body = await res.json().catch(() => ({}));
  const choice = body.choices?.[0];
  const content = choice?.message?.content ?? "";
  check("状态码 200", res.status === 200, `实际 ${res.status} ${JSON.stringify(body).slice(0, 200)}`);
  check("usage 有 token 统计", (body.usage?.total_tokens ?? 0) > 0, JSON.stringify(body.usage));
  check("finish_reason 已给出", typeof choice?.finish_reason === "string", `实际 ${choice?.finish_reason}`);
  if (content.length > 0) check("message.content 非空", true);
  else warn("message.content 为空（思考吃满了输出预算）", `finish_reason=${choice?.finish_reason}，可加大 max_tokens`);
  console.log(`      回复: ${JSON.stringify(content)}`);
  console.log(`      思考: ${(choice?.message?.reasoning_content ?? "").slice(0, 60)}...`);
}

// ---- 4. 流式对话 ----
async function t4() {
  console.log("\n[4] POST /v1/chat/completions（流式）");
  const res = await call("/v1/chat/completions", simpleBody({ stream: true }));
  check("状态码 200", res.status === 200, `实际 ${res.status}`);
  check("content-type 为 SSE", (res.headers.get("content-type") ?? "").includes("text/event-stream"));

  let text = "";
  let sawRole = false;
  let finishReason = null;
  let sawDone = false;
  let thinking = "";
  const decoder = new TextDecoder();
  let buffer = "";
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
        sawDone = true;
        continue;
      }
      let evt;
      try {
        evt = JSON.parse(payload);
      } catch {
        continue;
      }
      const delta = evt.choices?.[0]?.delta ?? {};
      if (delta.role === "assistant") sawRole = true;
      if (delta.content) text += delta.content;
      if (delta.reasoning_content) thinking += delta.reasoning_content;
      if (evt.choices?.[0]?.finish_reason) finishReason = evt.choices[0].finish_reason;
    }
  }
  check("首个 chunk 带 role=assistant", sawRole);
  check("累积到正文内容", text.length > 0, `实际 ${JSON.stringify(text)}`);
  check("收到结束信号 [DONE]", sawDone);
  check("finish_reason 已给出", typeof finishReason === "string", `实际 ${finishReason}`);
  console.log(`      流式回复: ${JSON.stringify(text)}（思考 ${thinking.length} 字）`);
}

// ---- 5. 工具调用（auto） ----
async function t5() {
  console.log("\n[5] 工具调用（tool_choice=auto）");
  const res = await call("/v1/chat/completions", {
    model: MODEL,
    max_tokens: MAX_TOKENS,
    tools: [WEATHER_TOOL],
    tool_choice: "auto",
    messages: [{ role: "user", content: "北京天气怎么样？请调用工具查询。" }],
  });
  const body = await res.json().catch(() => ({}));
  const choice = body.choices?.[0];
  const call0 = choice?.message?.tool_calls?.[0];
  check("状态码 200", res.status === 200, `实际 ${res.status} ${JSON.stringify(body).slice(0, 200)}`);
  check("返回 tool_calls", Array.isArray(choice?.message?.tool_calls) && choice.message.tool_calls.length > 0);
  check("工具名为 get_weather", call0?.function?.name === "get_weather", `实际 ${call0?.function?.name}`);
  let parsedArgs = null;
  try {
    parsedArgs = JSON.parse(call0?.function?.arguments ?? "null");
  } catch {
    /* 解析失败即计入失败 */
  }
  check("arguments 是合法 JSON", parsedArgs !== null, call0?.function?.arguments);
  check("arguments 含 city", typeof parsedArgs?.city === "string", JSON.stringify(parsedArgs));
  check("finish_reason 为 tool_calls", choice?.finish_reason === "tool_calls", `实际 ${choice?.finish_reason}`);
  console.log(`      工具调用: ${call0?.function?.name}(${call0?.function?.arguments})`);
  return choice?.message;
}

// ---- 6. 强制 tool_choice 应被降级而不是报 503 ----
async function t6() {
  console.log("\n[6] 强制 tool_choice 的降级处理");
  const res = await call("/v1/chat/completions", {
    model: MODEL,
    max_tokens: MAX_TOKENS,
    tools: [WEATHER_TOOL],
    tool_choice: { type: "function", function: { name: "get_weather" } },
    messages: [{ role: "user", content: "北京天气怎么样？请调用工具查询。" }],
  });
  const body = await res.json().catch(() => ({}));
  check("状态码 200（未因强制选择被上游拒绝）", res.status === 200, `实际 ${res.status} ${JSON.stringify(body).slice(0, 200)}`);
}

// ---- 7. 多轮续接（依赖 thinking 回传） ----
async function t7(assistantMessage) {
  console.log("\n[7] 多轮续接（工具结果回传，依赖 thinking 补回）");
  if (!assistantMessage?.tool_calls) {
    check("上一步拿到 tool_calls 才能续接", false, "上一步未返回 tool_calls");
    return;
  }
  const res = await call("/v1/chat/completions", {
    model: MODEL,
    max_tokens: MAX_TOKENS,
    tools: [WEATHER_TOOL],
    messages: [
      { role: "user", content: "北京天气怎么样？请调用工具查询。" },
      { role: "assistant", content: assistantMessage.content ?? "", tool_calls: assistantMessage.tool_calls },
      { role: "tool", tool_call_id: assistantMessage.tool_calls[0].id, content: "北京：晴，25℃，微风" },
    ],
  });
  const body = await res.json().catch(() => ({}));
  const content = body.choices?.[0]?.message?.content ?? "";
  check("状态码 200", res.status === 200, `实际 ${res.status} ${JSON.stringify(body).slice(0, 300)}`);
  check("返回最终回答", content.length > 0, JSON.stringify(body).slice(0, 200));
  console.log(`      续接回复: ${JSON.stringify(content)}`);
}

// ---- 8. 错误路径 ----
async function t8() {
  console.log("\n[8] 错误路径");
  const notFound = await fetch(`${BASE}/v1/embeddings`, {
    method: "POST",
    body: "{}",
    signal: AbortSignal.timeout(10000),
  });
  check("未实现端点返回 404", notFound.status === 404, `实际 ${notFound.status}`);

  const badJson = await fetch(`${BASE}/v1/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{ 这不是 JSON",
    signal: AbortSignal.timeout(10000),
  });
  check("非法 JSON 返回 400", badJson.status === 400, `实际 ${badJson.status}`);
}

console.log(`model-bridge 冒烟测试 → ${BASE}（模型 ${MODEL}）`);
await t1();
await t2();
await t3();
await t4();
const assistantMessage = await t5();
await t6();
await t7(assistantMessage);
await t8();

console.log(`\n结果：通过 ${passed} 项，失败 ${failed} 项`);
process.exit(failed === 0 ? 0 : 1);
