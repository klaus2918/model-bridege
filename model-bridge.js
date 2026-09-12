// model-bridge：本地单文件协议桥，把 OpenAI 格式请求转成 Anthropic Messages 送给上游。
//
// 存在意义：Model-Gate 只会说 OpenAI chat（/v1/chat/completions），而 cc-switch 的 CCH 通路
// 只认 Anthropic（/v1/messages）。本工具把这段协议差收进一个文件、一个进程：
//
//   Agent（OpenAI 格式） → model-bridge → cc-switch 本地代理（Anthropic） → CCH → 模型
//
// 特性：零依赖（只用 Node 内置模块）、模型列表 /v1/models、别名、流式 SSE、工具调用、access 日志。
//      thinking 回传（多轮会话必需）：
//        DeepSeek 思考模式要求多轮把上一轮的思考内容原样传回，否则思考链断裂。
//        Agent 通常不会回传该字段，故本工具缓存上游返回的 thinking + signature，
//        在后续轮次的 assistant 消息中自动补回（位于消息首位），客户端若自带 reasoning_content 则以其为准。
//        开关：bridge.config.json 的 thinking.passthrough（默认 true）/ thinking.cacheSize。
//
// 用法：
//   node model-bridge.js                        # 读取同目录 bridge.config.json
//   node model-bridge.js --config <路径>         # 指定配置
//   node model-bridge.js --port 8900            # 覆盖端口
import { createServer } from "node:http";
import { createHash, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** 解析命令行参数（仅支持 --key value 形式） */
function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const val = argv[i + 1] && !argv[i + 1].startsWith("--") ? argv[++i] : "true";
      out[key] = val;
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const configPath = resolve(args.config ?? join(HERE, "bridge.config.json"));

let config;
try {
  config = JSON.parse(readFileSync(configPath, "utf8"));
} catch (e) {
  console.error(`[model-bridge] 无法读取配置 ${configPath}: ${e.message}`);
  process.exit(1);
}

/**
 * 从 KEY=VALUE 格式的文件加载环境变量，供上游 apiKeyEnv 引用。
 * 只在变量尚未存在时写入，不覆盖已有环境变量。
 * 默认读取 ~/.jcode/.env；配置 envFile 可改路径，设 false 则禁用。
 */
function loadEnvFile() {
  const target = config.envFile === false ? null : (config.envFile ?? join(homedir(), ".jcode", ".env"));
  if (!target) return;
  let text;
  try {
    text = readFileSync(target, "utf8");
  } catch {
    return; // 文件不存在不是错误：密钥也可来自真实环境变量
  }
  for (const line of text.split("\n")) {
    const m = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/);
    if (!m) continue;
    const [, key, value] = m;
    if (process.env[key] === undefined && value !== "") process.env[key] = value;
  }
}

loadEnvFile();

const PORT = Number(args.port ?? config.port ?? 8900);
const HOST = config.host ?? "127.0.0.1";
const ACCESS_LOG = config.accessLog !== false;
const TIMEOUT_MS = Number(config.timeoutSeconds ?? 600) * 1000;
const DEFAULT_MAX_TOKENS = Number(config.maxTokens ?? 8192);

// ── 上游定义 ────────────────────────────────────────────────────────────────
// 支持两类上游：
//   1) 本地代理（如 cc-switch / CCH 中继）：Anthropic 协议，走 /v1/messages
//   2) opencode Go：OpenAI 协议，走 /v1/chat/completions，且每个会话必须带
//      x-opencode-session 头（缺失会被 400 MissingSessionID 拒绝）
// 用 config.upstreams 配多个；若只写旧的 config.upstream 则自动视为单上游。
const UPSTREAMS = (() => {
  const list = [];
  if (config.upstreams && Object.keys(config.upstreams).length > 0) {
    for (const [name, u] of Object.entries(config.upstreams)) {
      list.push(normalizeUpstream(name, u));
    }
  } else {
    list.push(normalizeUpstream("default", config.upstream ?? {}));
  }
  return list;
})();

/**
 * 解析上游密钥。
 * - 配置了 apiKeyEnv 且环境变量存在 → 用环境变量（推荐，避免密钥落盘）
 * - 显式配置 apiKey（含空字符串）→ 用配置值；空字符串表示不发送鉴权头
 * - 都没配 → 用 "any-key" 占位（本地代理通常不校验）
 */
function resolveApiKey(u) {
  if (u.apiKeyEnv && process.env[u.apiKeyEnv]) return process.env[u.apiKeyEnv];
  if (u.apiKey !== undefined) return u.apiKey;
  return "any-key";
}

/** 补全单个上游的默认值 */
function normalizeUpstream(name, u) {
  const protocol = u.protocol ?? (u.baseUrl && u.baseUrl.includes("opencode.ai") ? "openai" : "anthropic");
  // 统一去掉尾部的 /v1（若配置里带了），内部拼路径时再补，避免出现 /v1/v1。
  // 这样 baseUrl 写 ".../go" 或 ".../go/v1" 都能正确工作。
  const baseUrl = String(u.baseUrl ?? "http://127.0.0.1:15721")
    .replace(/\/+$/, "")
    .replace(/\/v1$/, "");
  return {
    name,
    baseUrl,
    // apiKey 解析优先级：环境变量 → 配置值 → 默认 "any-key"。
    // 显式配置空字符串（""）表示「不发送鉴权头」，用于 Zen 免费模型这类无需密钥的场景。
    apiKey: resolveApiKey(u),
    protocol, // "anthropic" | "openai"
    anthropicVersion: u.anthropicVersion ?? "2023-06-01",
    models: u.models ?? [],
    default: u.default === true,
    // opencode 专用：是否注入会话头（默认 true）
    sessionHeader: u.sessionHeader !== false,
    // 是否从上游 /v1/models 拉取模型清单（默认 true）
    fetchModels: u.fetchModels !== false,
    // 是否把上游拉取到的「全部」模型也暴露给下游（默认 false）。
    // 关闭时只暴露 config 里显式声明的模型，避免把用不到/无权使用的模型暴露出去。
    autoModels: u.autoModels === true,
  };
}

/** 按模型名选上游：优先精确匹配 models 列表，其次默认上游，最后第一个 */
function pickUpstream(model) {
  const name = String(model ?? "");
  for (const u of UPSTREAMS) {
    if (u.models.includes(name)) return u;
  }
  return UPSTREAMS.find((u) => u.default) ?? UPSTREAMS[0];
}

/** 全部上游模型去重合并（用于 /v1/models） */
function allUpstreamModels() {
  const out = [];
  for (const u of UPSTREAMS) for (const m of u.models) if (!out.includes(m)) out.push(m);
  return out;
}

const OPENCODE_SESSION_HEADER = "x-opencode-session";

/** 会话标识缓存：模型 + 首条用户消息 → 稳定的会话 ID，保证同会话复用（利于上游缓存与路由） */
const sessionByKey = new Map();

/** 由请求内容推导一个稳定的会话 ID（同一对话反复请求得到同一个值） */
function sessionIdFor(body) {
  const firstUser = (body.messages ?? []).find((m) => m?.role === "user");
  const seed = `${body.model ?? ""}|${JSON.stringify(firstUser?.content ?? "")}`;
  const hit = sessionByKey.get(seed);
  if (hit) return hit;
  const id = randomUUID();
  // 简单容量控制：超过 2000 条丢最早的
  if (sessionByKey.size > 2000) sessionByKey.delete(sessionByKey.keys().next().value);
  sessionByKey.set(seed, id);
  return id;
}
// thinking 回传：DeepSeek 思考模式要求多轮会话把上一轮的思考内容原样传回，
// 否则思考链断裂。Agent 通常不会回传该字段，故在此缓存并在后续轮次自动补回。
const THINKING_PASSTHROUGH = config.thinking?.passthrough !== false;
const THINKING_CACHE_MAX = Number(config.thinking?.cacheSize ?? 500);
// 上游处于 thinking 模式时，Anthropic 协议不允许强制工具选择（required / 指定某个工具），
// 强行下发会被上游拒绝（503 Thinking mode does not support this tool_choice）。
// 默认把「强制」降级成 auto（模型仍可自主调用工具），需要真强制时把该开关设为 false。
const DOWNGRADE_FORCED_TOOL_CHOICE = config.tools?.downgradeForcedChoice !== false;

/** tool_call id → 思考内容；assistant 文本指纹 → 思考内容 */
const thinkingByToolId = new Map();
const thinkingByText = new Map();

/** 助手回复文本指纹（规范化空白后取摘要，容忍客户端的轻微改写） */
function fingerprint(text) {
  return createHash("sha1").update(String(text ?? "").trim().replace(/\s+/g, " ")).digest("hex").slice(0, 16);
}

/** 按插入顺序淘汰最旧条目，限制内存占用 */
function evict(map) {
  while (map.size > THINKING_CACHE_MAX) map.delete(map.keys().next().value);
}

/** 记住一轮思考内容，供后续轮次补回 */
function rememberThinking(thinking, signature, text, toolIds) {
  if (!thinking) return;
  const entry = { thinking, signature: signature ?? "" };
  for (const id of toolIds ?? []) if (id) thinkingByToolId.set(id, entry);
  if (text) thinkingByText.set(fingerprint(text), entry);
  evict(thinkingByToolId);
  evict(thinkingByText);
}

/** 取回思考内容：先按 tool_call id 命中，再按文本指纹兜底 */
function recallThinking(text, toolIds) {
  for (const id of toolIds ?? []) {
    const hit = thinkingByToolId.get(id);
    if (hit) return hit;
  }
  if (text) return thinkingByText.get(fingerprint(text)) ?? null;
  return null;
}

/** 构造要回传给上游的 thinking 块；取不到则返回 null */
function buildThinkingBlock(message, text, toolIds) {
  // 客户端显式带了思考内容就以它为准
  const explicit = message?.reasoning_content ?? message?.thinking;
  if (typeof explicit === "string" && explicit.trim()) {
    const block = { type: "thinking", thinking: explicit };
    const sig = message?.reasoning_signature ?? message?.signature;
    if (sig) block.signature = sig;
    return block;
  }
  if (!THINKING_PASSTHROUGH) return null;
  const hit = recallThinking(text, toolIds);
  if (!hit) return null;
  const block = { type: "thinking", thinking: hit.thinking };
  if (hit.signature) block.signature = hit.signature;
  return block;
}

/** 别名 → 真实模型 ID；未命中则原样透传 */
function resolveModel(name) {
  if (!name) return config.defaultModel ?? config.models?.[0] ?? "";
  return config.aliases?.[name] ?? name;
}

/**
 * /v1/models 暴露的模型清单。
 *
 * 顶层 `models` 是**精确控制**：配了就只展示它列出的（加别名），一个不多。
 * 没配才退化为合并各上游声明的模型，保证开箱可用。
 * 绝不去上游拉取——上游的模型名与本工具对外提供的名字未必一致。
 */
function modelList() {
  const top = config.models ?? [];
  const ids = new Set(top.length > 0 ? top : allUpstreamModels());
  for (const [alias, target] of Object.entries(config.aliases ?? {})) {
    ids.add(alias);
    ids.add(target);
  }
  return [...ids];
}

function json(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json; charset=utf-8" });
  res.end(text);
}

/**
 * 带上游标识的 JSON 响应：响应头会写明本次实际用了哪个上游、哪个模型。
 * 便于调用方（以及排障时）确认请求真正落到了哪里。
 */
function jsonWithUpstream(res, status, body, upstreamName, upstreamModel) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "x-bridge-upstream": upstreamName ?? "",
    "x-bridge-upstream-model": upstreamModel ?? "",
  });
  res.end(text);
}

function errorBody(message, type = "upstream_error", code = "upstream_failed") {
  return { error: { message, type, code } };
}

/** 把 OpenAI 的 content（字符串或分片数组）压成纯文本 */
function textOf(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((p) => (typeof p === "string" ? p : p?.type === "text" ? (p.text ?? "") : ""))
    .join("");
}

function safeJson(text) {
  if (!text) return {};
  try {
    return JSON.parse(text);
  } catch {
    return {};
  }
}

/** 按序合并相邻同角色消息，满足 Anthropic 角色交替要求 */
function pushMessage(list, role, content) {
  if (content === "" || content == null) return;
  const last = list[list.length - 1];
  if (last && last.role === role && typeof last.content === "string" && typeof content === "string") {
    last.content += `\n${content}`;
    return;
  }
  list.push({ role, content });
}

/** OpenAI Chat 请求体 → Anthropic Messages 请求体 */
function toAnthropic(body) {
  const systemParts = [];
  const messages = [];
  let injected = 0; // 成功补回 thinking 的助手消息数
  let missed = 0; // 需要 thinking 但缓存未命中的助手消息数

  for (const m of body.messages ?? []) {
    const role = m?.role;
    if (role === "system" || role === "developer") {
      systemParts.push(textOf(m.content));
      continue;
    }
    // 工具结果：OpenAI 用独立 tool 角色，Anthropic 放在 user 消息的 tool_result 块里
    if (role === "tool") {
      messages.push({
        role: "user",
        content: [
          { type: "tool_result", tool_use_id: m.tool_call_id, content: textOf(m.content) },
        ],
      });
      continue;
    }
    // 助手发起的工具调用 → Anthropic 的 tool_use 块
    if (role === "assistant" && Array.isArray(m.tool_calls) && m.tool_calls.length > 0) {
      const blocks = [];
      const text = textOf(m.content);
      const toolIds = m.tool_calls.map((tc) => tc.id);
      // Anthropic 要求 thinking 块位于 assistant 消息首位，且回传时带上原 signature
      const think = buildThinkingBlock(m, text, toolIds);
      if (think) {
        blocks.push(think);
        injected += 1;
      } else if (THINKING_PASSTHROUGH) {
        missed += 1;
      }
      if (text) blocks.push({ type: "text", text });
      for (const tc of m.tool_calls) {
        blocks.push({
          type: "tool_use",
          id: tc.id,
          name: tc.function?.name ?? "unknown",
          input: safeJson(tc.function?.arguments),
        });
      }
      messages.push({ role: "assistant", content: blocks });
      continue;
    }
    // 纯文本的多轮助手消息：同样把思考内容补回
    if (role === "assistant" && THINKING_PASSTHROUGH) {
      const text = textOf(m.content);
      const think = buildThinkingBlock(m, text, []);
      if (think) {
        const blocks = [think];
        if (text) blocks.push({ type: "text", text });
        messages.push({ role: "assistant", content: blocks });
        injected += 1;
        continue;
      }
      missed += 1;
    }
    pushMessage(messages, role === "assistant" ? "assistant" : "user", textOf(m.content));
  }

  const out = {
    model: resolveModel(body.model),
    max_tokens: Number(body.max_tokens ?? body.max_completion_tokens ?? DEFAULT_MAX_TOKENS),
    messages,
  };
  const system = systemParts.filter(Boolean).join("\n\n");
  if (system) out.system = system;
  if (body.temperature !== undefined) out.temperature = body.temperature;
  if (body.top_p !== undefined) out.top_p = body.top_p;
  if (body.stop) out.stop_sequences = Array.isArray(body.stop) ? body.stop : [body.stop];
  if (body.stream) out.stream = true;

  const choice = body.tool_choice;
  // 客户端声明本轮不调用工具时，不下发工具定义（等价于 tool_choice: none，且不踩上游的语义差异）
  const tools =
    choice === "none"
      ? []
      : (body.tools ?? []).filter((t) => t?.type === "function" && t.function?.name);
  let downgraded = 0;
  if (tools.length > 0) {
    out.tools = tools.map((t) => ({
      name: t.function.name,
      description: t.function.description ?? "",
      input_schema: t.function.parameters ?? { type: "object", properties: {} },
    }));
    if (choice === "auto") {
      out.tool_choice = { type: "auto" };
    } else if (choice === "required" || choice === "any" || (choice?.type === "function" && choice.function?.name)) {
      // thinking 模式不支持强制选择：默认降级为 auto（不下发 tool_choice）
      if (DOWNGRADE_FORCED_TOOL_CHOICE) {
        downgraded += 1;
      } else if (choice?.type === "function" && choice.function?.name) {
        out.tool_choice = { type: "tool", name: choice.function.name };
      } else {
        out.tool_choice = { type: "any" };
      }
    }
  }
  return { body: out, injected, missed, downgraded };
}

/** Anthropic 结束原因 → OpenAI finish_reason */
function toFinishReason(stopReason) {
  switch (stopReason) {
    case "max_tokens":
      return "length";
    case "tool_use":
      return "tool_calls";
    default:
      return "stop";
  }
}

/** Anthropic 响应体 → OpenAI 响应体 */
function toOpenAI(data, requestedModel) {
  const blocks = data.content ?? [];
  const text = blocks.filter((b) => b.type === "text").map((b) => b.text ?? "").join("");
  const thinking = blocks.filter((b) => b.type === "thinking").map((b) => b.thinking ?? "").join("");
  const signature = blocks.find((b) => b.type === "thinking")?.signature ?? "";
  const toolUses = blocks.filter((b) => b.type === "tool_use");
  // 缓存本轮的思考内容，供下一轮补回
  rememberThinking(thinking, signature, text, toolUses.map((b) => b.id));

  const message = { role: "assistant", content: text };
  if (thinking) message.reasoning_content = thinking;
  if (toolUses.length > 0) {
    message.tool_calls = toolUses.map((b, i) => ({
      index: i,
      id: b.id,
      type: "function",
      function: { name: b.name, arguments: JSON.stringify(b.input ?? {}) },
    }));
  }

  const prompt = data.usage?.input_tokens ?? 0;
  const completion = data.usage?.output_tokens ?? 0;
  return {
    id: data.id,
    object: "chat.completion",
    created: Math.floor(Date.now() / 1000),
    model: requestedModel,
    choices: [{ index: 0, message, finish_reason: toFinishReason(data.stop_reason) }],
    usage: { prompt_tokens: prompt, completion_tokens: completion, total_tokens: prompt + completion },
  };
}

/** Anthropic SSE 流转成 OpenAI SSE 流（逐事件改写） */
async function pipeStream(upstreamBody, res, requestedModel, onUsage, upName = "", upModel = "") {
  res.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache",
    connection: "keep-alive",
    "x-bridge-upstream": upName,
    "x-bridge-upstream-model": upModel,
  });

  const decoder = new TextDecoder();
  let buffer = "";
  let finished = false;
  let toolIndex = -1;
  let finishReason = "stop";
  let usage = { prompt_tokens: 0, completion_tokens: 0 };
  // 累积本轮思考内容与工具调用 id，流结束后写入缓存供下一轮补回
  let accThinking = "";
  let accSignature = "";
  let accText = "";
  const accToolIds = [];

  const chunk = (delta, extra = {}) =>
    `data: ${JSON.stringify({
      id: "chatcmpl-bridge",
      object: "chat.completion.chunk",
      created: Math.floor(Date.now() / 1000),
      model: requestedModel,
      choices: [{ index: 0, delta, finish_reason: null }],
      ...extra,
    })}\n\n`;

  const emit = (event, data) => {
    if (event === "message_start") {
      usage.prompt_tokens = data?.message?.usage?.input_tokens ?? 0;
      res.write(chunk({ role: "assistant", content: "" }));
      return;
    }
    if (event === "content_block_start") {
      const block = data?.content_block;
      if (block?.type === "tool_use") {
        toolIndex += 1;
        accToolIds.push(block.id);
        res.write(
          chunk({
            tool_calls: [
              { index: toolIndex, id: block.id, type: "function", function: { name: block.name, arguments: "" } },
            ],
          }),
        );
      }
      return;
    }
    if (event === "content_block_delta") {
      const d = data?.delta;
      if (d?.type === "text_delta") {
        accText += d.text ?? "";
        res.write(chunk({ content: d.text ?? "" }));
      } else if (d?.type === "thinking_delta") {
        accThinking += d.thinking ?? "";
        res.write(chunk({ reasoning_content: d.thinking ?? "" }));
      } else if (d?.type === "signature_delta") {
        accSignature = d.signature ?? accSignature;
      }
      else if (d?.type === "input_json_delta" && toolIndex >= 0) {
        res.write(chunk({ tool_calls: [{ index: toolIndex, function: { arguments: d.partial_json ?? "" } }] }));
      }
      return;
    }
    if (event === "message_delta") {
      if (data?.delta?.stop_reason) finishReason = toFinishReason(data.delta.stop_reason);
      if (data?.usage?.output_tokens !== undefined) usage.completion_tokens = data.usage.output_tokens;
      return;
    }
    if (event === "message_stop") {
      finished = true;
      rememberThinking(accThinking, accSignature, accText, accToolIds);
      res.write(
        `data: ${JSON.stringify({
          id: "chatcmpl-bridge",
          object: "chat.completion.chunk",
          created: Math.floor(Date.now() / 1000),
          model: requestedModel,
          choices: [{ index: 0, delta: {}, finish_reason: finishReason }],
          usage: {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.prompt_tokens + usage.completion_tokens,
          },
        })}\n\n`,
      );
      res.write("data: [DONE]\n\n");
    }
  };

  const reader = upstreamBody.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let sep;
    while ((sep = buffer.indexOf("\n\n")) >= 0) {
      const raw = buffer.slice(0, sep);
      buffer = buffer.slice(sep + 2);
      let event = "message";
      const dataLines = [];
      for (const line of raw.split("\n")) {
        if (line.startsWith("event:")) event = line.slice(6).trim();
        else if (line.startsWith("data:")) dataLines.push(line.slice(5).trim());
      }
      if (dataLines.length === 0) continue;
      let parsed;
      try {
        parsed = JSON.parse(dataLines.join("\n"));
      } catch {
        continue;
      }
      emit(event, parsed);
    }
  }
  if (!finished) res.write("data: [DONE]\n\n");
  onUsage(usage);
  res.end();
}

/**
 * 处理 /v1/chat/completions
 * @param forcedUpstream 非空时强制使用该上游（来自 URL 前缀，如 /zen/v1/...）；
 *                       为空时按模型名自动分发。
 */
async function handleChat(req, res, forcedUpstream = null) {
  const started = Date.now();
  let body;
  try {
    const raw = await readBody(req);
    body = JSON.parse(raw);
  } catch {
    return json(res, 400, errorBody("请求体不是合法 JSON", "invalid_request_error", "invalid_json"));
  }
  if (!body?.model && !config.defaultModel) {
    return json(res, 400, errorBody("缺少 model 字段且未配置 defaultModel", "invalid_request_error", "model_required"));
  }

  const requestedModel = body?.model ?? config.defaultModel;
  // 决定打到哪条上游：URL 前缀优先，其次按模型名匹配
  const up = forcedUpstream ?? pickUpstream(resolveModel(requestedModel));
  const isAnthropic = up.protocol === "anthropic";

  // 统一在本地完成模型别名解析：下游看到的别名/模型名原样返回，
  // 送给上游的名字用 resolveModel 的结果。
  const upstreamModel = resolveModel(requestedModel);

  let payload;
  let injected = 0;
  if (isAnthropic) {
    const conv = toAnthropic(body);
    payload = conv.body;
    payload.model = upstreamModel;
    injected = conv.injected;
    // thinking 缺失提示：要求必须回传却没命中缓存时给出告警，便于排查思考链断裂
    if (THINKING_PASSTHROUGH && conv.missed > 0) {
      if (ACCESS_LOG) log(`注意: ${conv.missed} 条助手消息需要 thinking 但缓存未命中（思考链可能断裂）`);
    }
    if (conv.downgraded > 0 && ACCESS_LOG) {
      log("注意: 强制 tool_choice 已降级为 auto（上游 thinking 模式不支持强制工具选择）");
    }
  } else {
    // openai 协议：请求体基本原样透传，只替换模型名
    payload = { ...body, model: upstreamModel };
  }

  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), TIMEOUT_MS);

  // 组装上游请求头：anthropic 用 x-api-key，openai 用 Bearer；
  // opencode 类上游额外注入会话头（缺失会被 400 MissingSessionID 拒绝）。
  const headers = { "content-type": "application/json" };
  if (isAnthropic) {
    headers["anthropic-version"] = up.anthropicVersion;
    if (up.apiKey) {
      headers.authorization = req.headers.authorization ?? `Bearer ${up.apiKey}`;
      headers["x-api-key"] = up.apiKey;
    }
  } else {
    // 空 key 表示该上游无需鉴权（如 Zen 免费模型），不能发空的 Authorization 头
    if (up.apiKey) headers.authorization = `Bearer ${up.apiKey}`;
    if (up.sessionHeader) headers[OPENCODE_SESSION_HEADER] = sessionIdFor(body);
  }

  const url = isAnthropic ? `${up.baseUrl}/v1/messages` : `${up.baseUrl}/v1/chat/completions`;

  let upstream;
  try {
    upstream = await fetch(url, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
      signal: ac.signal,
    });
  } catch (e) {
    clearTimeout(timer);
    if (ACCESS_LOG) log(`${requestedModel} -> ${up.name}:${upstreamModel} 连接失败 ${Date.now() - started}ms ${e.message}`);
    return json(
      res,
      502,
      errorBody(`无法连接上游 ${up.baseUrl}: ${e.message}`, "upstream_error", "upstream_failed"),
    );
  }

  if (!upstream.ok) {
    clearTimeout(timer);
    const text = await upstream.text();
    let message = text;
    try {
      message = JSON.parse(text)?.error?.message ?? text;
    } catch {
      /* 保留原文 */
    }
    if (ACCESS_LOG) log(`${requestedModel} -> ${up.name}:${upstreamModel} ${upstream.status} ${Date.now() - started}ms`);
    return json(
      res,
      upstream.status,
      errorBody(`上游返回 ${upstream.status}: ${message}`, "upstream_error", "upstream_failed"),
    );
  }

  if (body.stream) {
    try {
      // 上游是 openai 协议时，SSE 本身就是 OpenAI 格式，直接转发即可
      if (isAnthropic) {
        await pipeStream(upstream.body, res, requestedModel, (usage) => {
          clearTimeout(timer);
          if (ACCESS_LOG) {
            log(
              `${requestedModel} -> ${up.name}:${upstreamModel} 200 ${Date.now() - started}ms stream tokens=${usage.prompt_tokens}+${usage.completion_tokens} think=${injected}`,
            );
          }
        }, up.name, upstreamModel);
      } else {
        await passThroughStream(upstream.body, res, requestedModel, (usage) => {
          clearTimeout(timer);
          if (ACCESS_LOG) {
            log(
              `${requestedModel} -> ${up.name}:${upstreamModel} 200 ${Date.now() - started}ms stream tokens=${usage.prompt_tokens}+${usage.completion_tokens}`,
            );
          }
        }, up.name, upstreamModel);
      }
    } catch (e) {
      clearTimeout(timer);
      if (ACCESS_LOG) log(`${requestedModel} -> ${up.name}:${upstreamModel} 流式中断 ${Date.now() - started}ms ${e.message}`);
      res.end();
    }
    return;
  }

  clearTimeout(timer);
  const raw = await upstream.text();
  let result;
  if (isAnthropic) {
    let data;
    try {
      data = JSON.parse(raw);
    } catch {
      return json(res, 502, errorBody(`上游响应不是合法 JSON: ${raw.slice(0, 200)}`));
    }
    result = toOpenAI(data, requestedModel);
  } else {
    // openai 协议：响应原样透传，仅把 model 字段改回下游请求的名字
    try {
      result = JSON.parse(raw);
    } catch {
      return json(res, 502, errorBody(`上游响应不是合法 JSON: ${raw.slice(0, 200)}`));
    }
    result.model = requestedModel;
  }
  if (ACCESS_LOG) {
    log(
      `${requestedModel} -> ${up.name}:${upstreamModel} 200 ${Date.now() - started}ms tokens=${result.usage?.prompt_tokens ?? "?"}+${result.usage?.completion_tokens ?? "?"}${isAnthropic ? ` think=${injected}` : ""}`,
    );
  }
  jsonWithUpstream(res, 200, result, up.name, upstreamModel);
}

/**
 * OpenAI 协议上游的 SSE 原样转发。
 * 上游本来就吐 OpenAI 格式的 chunk，只需把 model 字段改回下游请求的名字，
 * 并在流结束时抓取 usage 用于日志。
 */
async function passThroughStream(upstreamBody, res, requestedModel, onUsage, upName = "", upModel = "") {
  res.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache",
    connection: "keep-alive",
    "x-bridge-upstream": upName,
    "x-bridge-upstream-model": upModel,
  });

  const decoder = new TextDecoder();
  let buffer = "";
  const usage = { prompt_tokens: 0, completion_tokens: 0 };

  /** 解析一行 SSE 的 data 载荷；非 JSON 或 [DONE] 返回 null */
  function parseData(payload) {
    const trimmed = payload.trim();
    if (!trimmed || trimmed === "[DONE]") return null;
    try {
      return JSON.parse(trimmed);
    } catch {
      return null;
    }
  }

  /** 改写 chunk 里的 model 字段，保持下游看到的模型名一致 */
  function rewrite(chunk) {
    if (chunk && typeof chunk === "object" && "model" in chunk) chunk.model = requestedModel;
    if (chunk?.usage) {
      usage.prompt_tokens = chunk.usage.prompt_tokens ?? usage.prompt_tokens;
      usage.completion_tokens = chunk.usage.completion_tokens ?? usage.completion_tokens;
    }
    return `data: ${JSON.stringify(chunk)}\n\n`;
  }

  const reader = upstreamBody.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let sep;
    while ((sep = buffer.indexOf("\n\n")) >= 0) {
      const raw = buffer.slice(0, sep);
      buffer = buffer.slice(sep + 2);
      // 一行 SSE 块可能含多行；只处理 data: 行，其余（event:/id:）忽略
      const dataLines = raw
        .split("\n")
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(5));
      if (dataLines.length === 0) {
        // 无 data 的块（如注释/心跳）原样透传，保持连接活性
        if (raw.trim()) res.write(raw + "\n\n");
        continue;
      }
      const joined = dataLines.join("\n");
      if (joined.trim() === "[DONE]") {
        res.write("data: [DONE]\n\n");
        continue;
      }
      const chunk = parseData(joined);
      if (chunk === null) {
        res.write(raw + "\n\n");
        continue;
      }
      res.write(rewrite(chunk));
    }
  }
  onUsage(usage);
  res.end();
}

/**
 * /v1/models：聚合三类来源
 *   1) 配置文件里声明的 models
 *   2) 各上游实时 /v1/models 拉取的列表（带缓存，避免每次请求都打上游）
 *   3) 别名
 * 上游拉取失败时静默降级，仅用已声明的列表，不影响客户端。
 */
// owner: 模型 → 上游名。来自上游拉取的模型由拉取方登记，避免事后反推导致归属错误。
const modelsCache = { at: 0, ids: [], owner: {} };
const MODELS_CACHE_MS = Number(config.modelsCacheSeconds ?? 300) * 1000;

/** 从某个上游拉取模型列表；失败返回空数组 */
async function fetchUpstreamModels(u) {
  if (u.fetchModels === false) return [];
  const headers = {};
  if (u.protocol === "openai") {
    if (u.apiKey) headers.authorization = `Bearer ${u.apiKey}`;
  } else if (u.apiKey) {
    headers["x-api-key"] = u.apiKey;
  }
  try {
    const res = await fetch(`${u.baseUrl}/v1/models`, { headers, signal: AbortSignal.timeout(8000) });
    if (!res.ok) return [];
    const data = await res.json();
    const list = data.data ?? data.models ?? [];
    return list.map((m) => m.id ?? m.slug ?? m.model).filter(Boolean);
  } catch {
    return [];
  }
}

/**
 * 处理 /v1/models
 * @param forcedUpstream 非空时只返回该上游的模型（来自 URL 前缀，如 /go/v1/models）。
 */
async function handleModels(req, res, forcedUpstream = null) {
  // 指定了上游前缀：只列该上游声明的模型，不拉取其它上游
  if (forcedUpstream) {
    const ids = [...forcedUpstream.models];
    if (!ids.length) return json(res, 200, { object: "list", data: [] });
    return json(res, 200, {
      object: "list",
      data: ids.map((id) => ({
        id,
        object: "model",
        created: Math.floor(Date.now() / 1000),
        owned_by: forcedUpstream.name,
      })),
    });
  }

  const now = Date.now();
  // 顶层 models 是精确白名单：配了就严格按它展示，一个不多，也不碰上游
  const whitelist = config.models ?? [];
  if (whitelist.length === 0 && now - modelsCache.at > MODELS_CACHE_MS) {
    // 未配白名单时才退化为拉取（且仅限显式开启 autoModels 的上游）
    const fetched = await Promise.all(
      UPSTREAMS.map(async (u) => ({ u, list: u.autoModels ? await fetchUpstreamModels(u) : [] })),
    );
    const ids = [];
    const owner = {};
    for (const { u, list } of fetched) {
      for (const id of list) {
        if (!ids.includes(id)) ids.push(id);
        if (!owner[id]) owner[id] = u.name;
      }
    }
    modelsCache.at = now;
    modelsCache.ids = ids;
    modelsCache.owner = owner;
  }
  const ids = modelList();
  if (whitelist.length === 0) {
    for (const id of modelsCache.ids) if (!ids.includes(id)) ids.push(id);
  }

  json(res, 200, {
    object: "list",
    data: ids.map((id) => ({
      id,
      object: "model",
      created: Math.floor(now / 1000),
      // 优先用拉取时登记的归属；配置声明的模型按 models 列表匹配
      owned_by: modelsCache.owner[id] ?? pickUpstream(id).name,
    })),
  });
}

/** 读取请求体 */
function readBody(req) {
  return new Promise((resolveBody, rejectBody) => {
    const parts = [];
    req.on("data", (c) => parts.push(c));
    req.on("end", () => resolveBody(Buffer.concat(parts).toString("utf8")));
    req.on("error", rejectBody);
  });
}

function log(line) {
  console.log(`[${new Date().toISOString()}] ${line}`);
}

const server = createServer((req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);

  // 支持用 URL 前缀强制指定上游：/{上游名}/v1/chat/completions
  // 例：/zen/v1/chat/completions 强制走 zen，/go/v1/models 只看 go 的模型。
  // 不带前缀时（/v1/...）保持按模型名自动分发。
  let forcedUpstream = null;
  let pathname = url.pathname;
  const prefixMatch = pathname.match(/^\/([A-Za-z0-9_-]+)(\/v1\/.*)$/);
  if (prefixMatch) {
    const candidate = UPSTREAMS.find((u) => u.name === prefixMatch[1]);
    if (candidate) {
      forcedUpstream = candidate;
      pathname = prefixMatch[2];
    } else if (pathname.includes("/v1/")) {
      // 前缀看着像上游名但不存在：明确报错并列出可用前缀，避免静默走错上游
      return json(
        res,
        404,
        errorBody(
          `未知的上游前缀 "${prefixMatch[1]}"。可用前缀：${UPSTREAMS.map((u) => `/${u.name}`).join("、")}；不带前缀时按模型名自动分发。`,
          "invalid_request_error",
          "unknown_upstream",
        ),
      );
    }
  }

  if (pathname === "/health") {
    const fallback = UPSTREAMS.find((u) => u.default) ?? UPSTREAMS[0];
    return json(res, 200, {
      status: "ok",
      // 兼容字段：默认上游地址（单上游时期的形态，便于既有脚本继续工作）
      upstream: fallback?.baseUrl ?? "",
      upstreams: UPSTREAMS.map((u) => ({
        name: u.name,
        baseUrl: u.baseUrl,
        protocol: u.protocol,
        models: u.models.length,
        default: u.default,
        // 用哪个地址访问这个上游
        endpoint: `http://${req.headers.host ?? `${HOST}:${PORT}`}/${u.name}/v1`,
      })),
    });
  }
  if (pathname === "/v1/models" && req.method === "GET") {
    return handleModels(req, res, forcedUpstream);
  }
  if (pathname === "/v1/chat/completions" && req.method === "POST") {
    handleChat(req, res, forcedUpstream).catch((e) => {
      if (ACCESS_LOG) log(`处理请求异常: ${e.message}`);
      if (!res.headersSent) json(res, 500, errorBody(e.message));
      else res.end();
    });
    return;
  }
  json(res, 404, errorBody(`未实现的端点 ${url.pathname}`, "invalid_request_error", "not_implemented"));
});

server.listen(PORT, HOST, () => {
  console.log(`[model-bridge] 已启动 http://${HOST}:${PORT}/v1`);
  for (const u of UPSTREAMS) {
    console.log(
      `  上游 ${u.name.padEnd(10)} ${u.protocol.padEnd(9)} ${u.baseUrl}${u.default ? "  [默认]" : ""}${u.sessionHeader && u.protocol === "openai" ? "  会话头注入" : ""}`,
    );
  }
  console.log(`[model-bridge] 声明的模型: ${modelList().join(", ") || "(未配置)"}`);
  console.log(
    `[model-bridge] thinking 回传: ${THINKING_PASSTHROUGH ? "开启" : "关闭"}（缓存上限 ${THINKING_CACHE_MAX} 条）`,
  );
});

// 启动失败时给出可操作的提示，而不是抛原始堆栈
server.on("error", (err) => {
  if (err.code === "EADDRINUSE") {
    console.error(`[model-bridge] 端口 ${PORT} 已被占用，无法启动。`);
    console.error(`  处理方式一：停掉占用该端口的进程`);
    console.error(`    netstat -ano | findstr :${PORT}        查看占用进程 PID`);
    console.error(`    taskkill /F /PID <PID>                 结束该进程`);
    console.error(`  处理方式二：换一个端口启动`);
    console.error(`    改 bridge.config.json 的 port，或加参数 --port <新端口>`);
  } else if (err.code === "EACCES") {
    console.error(`[model-bridge] 无权限绑定 ${HOST}:${PORT}（低位端口通常需要管理员权限）。建议换用 1024 以上端口。`);
  } else {
    console.error(`[model-bridge] 启动失败: ${err.message}`);
  }
  process.exit(1);
});
