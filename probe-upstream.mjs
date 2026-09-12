// 上游（cc-switch 本地代理）探测脚本：确认它支持哪些协议、哪些模型名、哪些 tool_choice。
// 用法：node probe-upstream.mjs [上游地址，默认 http://127.0.0.1:15721]
const UP = (process.argv[2] ?? "http://127.0.0.1:15721").replace(/\/+$/, "");
const KEY = "any-key";
const ANTHROPIC_HEADERS = { "anthropic-version": "2023-06-01", "x-api-key": KEY };

async function req(label, path, { method = "POST", body, headers = {} } = {}) {
  try {
    const r = await fetch(`${UP}${path}`, {
      method,
      headers: { "content-type": "application/json", ...headers },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(60000),
    });
    const text = await r.text();
    let summary = text.slice(0, 160);
    try {
      const j = JSON.parse(text);
      if (j.model !== undefined || j.stop_reason !== undefined) {
        summary = `model=${j.model} stop=${j.stop_reason} text=${JSON.stringify(
          (j.content ?? []).filter((b) => b.type === "text").map((b) => b.text).join(""),
        ).slice(0, 60)} tools=${(j.content ?? []).filter((b) => b.type === "tool_use").length}`;
      } else if (j.error) {
        summary = `${j.error.type ?? ""}: ${j.error.message ?? ""}`;
      } else if (j.models) {
        summary = `slug=${j.models.map((m) => m.slug).join(", ")}`;
      }
    } catch {
      /* 保留原文 */
    }
    console.log(`[${r.status}] ${label} → ${summary}`);
  } catch (e) {
    console.log(`[ERR] ${label} → ${e.name}: ${e.message}`);
  }
}

console.log(`上游探测：${UP}\n`);

await req("模型目录 GET /v1/models", "/v1/models", { method: "GET" });

for (const model of ["deepseek-flash", "deepseek-v4-flash", "deepseek-v4-pro"]) {
  await req(`Anthropic /v1/messages（model=${model}）`, "/v1/messages", {
    headers: ANTHROPIC_HEADERS,
    body: { model, max_tokens: 256, messages: [{ role: "user", content: "只回复两个字：pong" }] },
  });
}

await req("OpenAI /v1/chat/completions（Codex 端点）", "/v1/chat/completions", {
  headers: { authorization: `Bearer ${KEY}` },
  body: { model: "deepseek-v4-flash", max_tokens: 64, messages: [{ role: "user", content: "hi" }] },
});

const TOOL = {
  name: "get_weather",
  description: "查询指定城市的天气",
  input_schema: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
};

await req("tools + tool_choice=auto", "/v1/messages", {
  headers: ANTHROPIC_HEADERS,
  body: {
    model: "deepseek-flash",
    max_tokens: 512,
    tools: [TOOL],
    tool_choice: { type: "auto" },
    messages: [{ role: "user", content: "北京天气怎么样？请调用工具查询。" }],
  },
});

await req("tools + tool_choice={type:tool}（强制）", "/v1/messages", {
  headers: ANTHROPIC_HEADERS,
  body: {
    model: "deepseek-flash",
    max_tokens: 512,
    tools: [TOOL],
    tool_choice: { type: "tool", name: "get_weather" },
    messages: [{ role: "user", content: "北京天气怎么样？" }],
  },
});
