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
// CommandCode 相关常量：必须声明在 UPSTREAMS 之前 —— 上游归一化阶段就会用到
// （写在使用点之后会在模块初始化期触发 TDZ：Cannot access 'x' before initialization）
const CC_PLAN_RANK = { go: 1, goat: 2, pro: 3, max: 4 };
const CC_PLAN_LABEL = { go: "Go", goat: "GOAT", pro: "Pro", max: "Max" };
const CC_VERSION_CACHE_MS = 30 * 60 * 1000;
const ccVersion = { at: 0, value: "" };

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
 * 从凭据文件读取 apiKey 字段（CommandCode 的 auth.json 就是这种形态）。
 * 只读不写：值仅驻内存，不落盘、不进日志；文件缺失或字段为空时返回 undefined。
 */
function readKeyFile(file) {
  try {
    const key = JSON.parse(readFileSync(expandHome(file), "utf8"))?.apiKey;
    return typeof key === "string" && key ? key : undefined;
  } catch {
    return undefined;
  }
}

/** 展开路径开头的 ~ 为用户主目录 */
function expandHome(p) {
  return p.startsWith("~") ? join(homedir(), p.slice(1)) : p;
}

/**
 * 解析上游密钥，返回 {key, source}。
 * - 配置了 apiKeyEnv 且环境变量存在 → 用环境变量（推荐，避免密钥落盘）
 * - 配置了 apiKeyFile → 读该文件（只读，见 readKeyFile）
 * - 显式配置 apiKey（含空字符串）→ 用配置值；空字符串表示不发送鉴权头
 * - 都没配 → 用 "any-key" 占位（本地代理通常不校验）；source="none" 供各协议自行判断
 */
function resolveApiKey(u) {
  if (u.apiKeyEnv && process.env[u.apiKeyEnv]) return { key: process.env[u.apiKeyEnv], source: "env" };
  if (u.apiKeyFile) {
    const fromFile = readKeyFile(u.apiKeyFile);
    if (fromFile) return { key: fromFile, source: "file" };
  }
  if (u.apiKey !== undefined) return { key: u.apiKey, source: "literal" };
  return { key: "any-key", source: "none" };
}

/** 补全单个上游的默认值 */
function normalizeUpstream(name, u) {
  // 未显式声明 protocol 时按 baseUrl 推断：opencode 系 → openai；CommandCode → commandcode；其余 → anthropic。
  // 推断错协议会导致用完全不同的形状打上游（例如把 Anthropic 请求发到 /alpha/generate 的域名），故这里给全推断。
  const protocol =
    u.protocol ??
    (u.baseUrl?.includes("opencode.ai") ? "openai" : u.baseUrl?.includes("commandcode.ai") ? "commandcode" : "anthropic");
  // 统一去掉尾部的 /v1（若配置里带了），内部拼路径时再补，避免出现 /v1/v1。
  // 这样 baseUrl 写 ".../go" 或 ".../go/v1" 都能正确工作。
  const baseUrl = String(u.baseUrl ?? "http://127.0.0.1:15721")
    .replace(/\/+$/, "")
    .replace(/\/v1$/, "");
  const isCommandCode = protocol === "commandcode";
  const { key: apiKey, source: apiKeySource } = resolveApiKey(u);
  // 模型清单 = 配置声明的 + CommandCode 权威目录（CLI 自带 models.md）合并去重。
  // 该协议没有 /v1/models 端点，不读目录的话清单就只剩手写的几个。
  // modelsAll = 全集（供路由）；models = 对外可见，启动后按套餐过滤收窄。
  const declaredModels = u.models ?? [];
  const catalogEntries = isCommandCode ? commandCodeCatalogEntries(u.modelCatalog ?? "auto") : [];
  const modelsAll = [...new Set([...declaredModels, ...catalogEntries.map((e) => e.id)])];
  return {
    name,
    baseUrl,
    // apiKey 解析优先级：环境变量 → 凭据文件 → 配置值 → 默认 "any-key"。
    // 显式配置空字符串（""）表示「不发送鉴权头」，用于 Zen 免费模型这类无需密钥的场景。
    apiKey,
    apiKeySource,
    protocol, // "anthropic" | "openai" | "commandcode"
    anthropicVersion: u.anthropicVersion ?? "2023-06-01",
    models: modelsAll,
    modelsAll,
    catalogEntries,
    default: u.default === true,
    // opencode 专用：是否注入会话头（commandcode 协议不适用，默认关闭）
    sessionHeader: u.sessionHeader ?? !isCommandCode,
    // 是否从上游 /v1/models 拉取模型清单（commandcode 无该端点，默认关闭）
    fetchModels: u.fetchModels ?? !isCommandCode,
    // 是否把上游拉取到的「全部」模型也暴露给下游（默认 false）。
    // 关闭时只暴露 config 里显式声明的模型，避免把用不到/无权使用的模型暴露出去。
    autoModels: u.autoModels === true,
    // 上游级别名（短名 → 真实模型 id），不污染全局 config.aliases
    modelMap: u.modelMap ?? {},
    // commandcode 专用：发往上游的 CLI 版本标识与环境名
    cliVersion: u.cliVersion ?? "auto",
    cliEnvironment: u.cliEnvironment ?? "production",
    apiKeyFile: u.apiKeyFile ?? null,
    // commandcode 专用：模型目录来源（"auto"=读本机 CLI | false=只用配置声明 | 具体文件路径）
    modelCatalog: u.modelCatalog ?? (isCommandCode ? "auto" : false),
    // commandcode 专用：按套餐过滤对外清单（"auto"=探测账号套餐 | go/goat/pro/max=显式指定 | false=不过滤）
    modelCatalogPlan: u.modelCatalogPlan ?? (isCommandCode ? "auto" : false),
    // commandcode 专用：pause_turn 续跑（默认开，与上游 CLI 行为一致；关掉则收到 pause_turn 即收尾）
    pauseTurn: u.pauseTurn ?? isCommandCode,
    // commandcode 专用：ZDR 安全头（默认开，向上游发送 x-cmd-zdr: 1 表达零数据留存要求）
    cmdZdr: u.cmdZdr ?? true,
  };
}

/**
 * 按模型名选上游，优先级：
 *   1) 全局别名解析后的名字精确命中某上游的 models 列表
 *   2) 命中某上游的上游级别名（modelMap 的键）
 *   3) 配置 default 的上游，最后退回第一个
 */
function pickUpstream(model) {
  const name = String(model ?? "");
  const target = resolveModel(name);
  for (const u of UPSTREAMS) {
    // 路由用全集：即使该模型因套餐过滤未对外暴露，显式点名时仍应落到正确的上游
    if ((u.modelsAll ?? u.models).includes(target)) return u;
  }
  for (const u of UPSTREAMS) {
    if (Object.hasOwn(u.modelMap ?? {}, name)) return u;
  }
  return UPSTREAMS.find((u) => u.default) ?? UPSTREAMS[0];
}

/** 送给上游的真实模型名：上游级别名 → 全局别名 → 原样 */
function upstreamModelFor(up, model) {
  const name = String(model ?? "");
  return up.modelMap?.[name] ?? resolveModel(name);
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
  // 上游级别名（modelMap）同样对外可见，便于下游直接用短名
  for (const u of UPSTREAMS) {
    for (const [alias, target] of Object.entries(u.modelMap ?? {})) {
      ids.add(alias);
      ids.add(target);
    }
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

// ── 协议适配层 ──────────────────────────────────────────────────────────────
// 每种上游协议只实现三件事，主流程（handleChat）不再出现协议分支：
//   buildRequest(ctx)           → { url, headers, payload, meta }
//   parseResponse(raw, ctx)     → { body } | { error }（非流式）
//   parseStream(streamBody, res, ctx) → Promise（流式；自行写响应头并结束响应）
// ctx 字段：body / requestedModel / upstreamModel / upstream / clientAuth / onUsage。
// meta.logSuffix 供访问日志追加协议特有信息（如 anthropic 的 think=N）。

/** anthropic 协议：OpenAI 请求 → /v1/messages，SSE 逐事件改写成 OpenAI 流 */
const anthropicProtocol = {
  buildRequest(ctx) {
    const conv = toAnthropic(ctx.body);
    const headers = { "content-type": "application/json" };
    headers["anthropic-version"] = ctx.upstream.anthropicVersion;
    if (ctx.upstream.apiKey) {
      headers.authorization = ctx.clientAuth ?? `Bearer ${ctx.upstream.apiKey}`;
      headers["x-api-key"] = ctx.upstream.apiKey;
    }
    // thinking 缺失提示：要求必须回传却没命中缓存时给出告警，便于排查思考链断裂
    if (THINKING_PASSTHROUGH && conv.missed > 0 && ACCESS_LOG) {
      log(`注意: ${conv.missed} 条助手消息需要 thinking 但缓存未命中（思考链可能断裂）`);
    }
    if (conv.downgraded > 0 && ACCESS_LOG) {
      log("注意: 强制 tool_choice 已降级为 auto（上游 thinking 模式不支持强制工具选择）");
    }
    return {
      url: `${ctx.upstream.baseUrl}/v1/messages`,
      headers,
      payload: { ...conv.body, model: ctx.upstreamModel },
      meta: { logSuffix: ` think=${conv.injected}` },
    };
  },
  parseResponse(raw, ctx) {
    let data;
    try {
      data = JSON.parse(raw);
    } catch {
      return { error: `上游响应不是合法 JSON: ${raw.slice(0, 200)}` };
    }
    return { body: toOpenAI(data, ctx.requestedModel) };
  },
  parseStream(streamBody, res, ctx) {
    return pipeStream(streamBody, res, ctx.requestedModel, ctx.onUsage, ctx.upstream.name, ctx.upstreamModel);
  },
};

/** openai 协议：请求体原样透传，只替换模型名；SSE 直接转发并改写 model 字段 */
const openaiProtocol = {
  buildRequest(ctx) {
    const headers = { "content-type": "application/json" };
    // 空 key 表示该上游无需鉴权（如 Zen 免费模型），不能发空的 Authorization 头
    if (ctx.upstream.apiKey) headers.authorization = `Bearer ${ctx.upstream.apiKey}`;
    // opencode 类上游必须带会话头（缺失会被 400 MissingSessionID 拒绝）
    if (ctx.upstream.sessionHeader) headers[OPENCODE_SESSION_HEADER] = sessionIdFor(ctx.body);
    return {
      url: `${ctx.upstream.baseUrl}/v1/chat/completions`,
      headers,
      payload: { ...ctx.body, model: ctx.upstreamModel },
      meta: { logSuffix: "" },
    };
  },
  parseResponse(raw, ctx) {
    let result;
    try {
      result = JSON.parse(raw);
    } catch {
      return { error: `上游响应不是合法 JSON: ${raw.slice(0, 200)}` };
    }
    result.model = ctx.requestedModel;
    return { body: result };
  },
  parseStream(streamBody, res, ctx) {
    return passThroughStream(streamBody, res, ctx.requestedModel, ctx.onUsage, ctx.upstream.name, ctx.upstreamModel);
  },
};

// ── commandcode 协议 ────────────────────────────────────────────────────────
// CommandCode 是私有接口：POST /alpha/generate，请求体是「config 元数据 + params」，
// 响应是 NDJSON（每行一个 JSON 事件，不是 SSE）。事件名存在多套命名，故按并集识别、
// 未知 type 一律静默忽略。契约依据见 .op/changes/commandcode-upstream/reference/cc-wire-contract.md
const CC_MAX_TOKENS = 64000; // 上游缺省 max_tokens
const CC_PAUSE_TURN_MAX = 5; // pause_turn 续跑上限（与 CLI 的 Ph 一致：最多续 5 次，共 6 次请求）

/** 事件并集：只有这些 type 有语义，其余（start/start-step/text-start/text-end/finish-step…）忽略 */
function ccIndexByTool() {
  let next = -1;
  const byId = new Map();
  return {
    ensure(id, name, onStart) {
      if (id && byId.has(id)) return byId.get(id);
      next += 1;
      if (id) byId.set(id, next);
      onStart?.(next, id, name);
      return next;
    },
    has(id) {
      return Boolean(id) && byId.has(id);
    },
  };
}

/**
 * 把 CommandCode 事件归一成语义回调（流式与非流式共用同一套解析）。
 * handlers：text / reasoning / toolStart / toolInput / toolCall / finish / error
 */
function makeCommandCodeSink(handlers) {
  const tools = ccIndexByTool();
  return (ev) => {
    const type = ev?.type;
    if (type === "text-delta") return handlers.text?.(ev.text ?? "");
    if (type === "reasoning-delta") return handlers.reasoning?.(ev.text ?? "");
    if (type === "tool-input-start") {
      tools.ensure(ev.id ?? ev.toolCallId, ev.toolName, handlers.toolStart);
      return undefined;
    }
    if (type === "tool-input-delta") {
      const i = tools.ensure(ev.id ?? ev.toolCallId, ev.toolName, handlers.toolStart);
      return handlers.toolInput?.(i, ev.delta ?? "");
    }
    if (type === "tool-delta") {
      const i = tools.ensure(ev.toolCallId ?? ev.id, ev.toolName, handlers.toolStart);
      return handlers.toolInput?.(i, ev.delta ?? ev.partial ?? "");
    }
    if (type === "tool-call" || type === "tool-use") {
      const id = ev.toolCallId ?? ev.id;
      // 参数可能已由 tool-input-* 增量下发（known），此时不重复发整块
      const known = tools.has(id);
      const i = tools.ensure(id, ev.toolName, handlers.toolStart);
      return handlers.toolCall?.(i, id, ev.toolName, ev.input ?? ev.args ?? {}, known);
    }
    if (type === "finish") {
      // 原始终态一并透出：调用方据此决定是否续跑（pause_turn）
      const raw = String(ev.rawFinishReason ?? ev.finishReason ?? "").toLowerCase();
      return handlers.finish?.(ccStopReason(ev), ccUsage(ev), raw);
    }
    if (type === "error") return handlers.error?.(ccErrorMessage(ev));
    // tool-result / provider-metadata / abort 及一切未知事件：对 OpenAI 下游无语义，忽略
    return undefined;
  };
}

/**
 * 逐行读取 NDJSON 并喂给 sink；返回是否见到过 finish 事件。
 * source 可以是文本（非流式的首个响应）或 ReadableStream（流式 / 续跑响应）。
 */
async function consumeCommandCode(source, sink) {
  let gotFinish = false;
  const feed = (line) => {
    const text = line.trim();
    if (!text) return;
    let ev;
    try {
      ev = JSON.parse(text);
    } catch {
      return; // 非 JSON 行静默跳过（上游语义）
    }
    if (ev?.type === "finish") gotFinish = true;
    sink(ev);
  };
  if (typeof source === "string") {
    for (const line of source.split("\n")) feed(line);
    return { gotFinish };
  }
  const decoder = new TextDecoder();
  let buffer = "";
  const reader = source.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let sep;
    while ((sep = buffer.indexOf("\n")) >= 0) {
      feed(buffer.slice(0, sep));
      buffer = buffer.slice(sep + 1);
    }
  }
  feed(buffer); // 末行可能没有换行符
  return { gotFinish };
}

/** finish 事件 → OpenAI finish_reason（两套映射取并集） */
function ccStopReason(ev) {
  const raw = String(ev?.finishReason ?? ev?.rawFinishReason ?? "").toLowerCase();
  if (raw === "tool_use" || raw === "tool-calls" || raw === "tool_calls") return "tool_calls";
  if (raw === "length" || raw === "max_tokens" || raw === "max_output_tokens" || raw === "model_context_window_exceeded") return "length";
  return "stop";
}

/** finish 事件 → OpenAI usage */
function ccUsage(ev) {
  const u = ev?.totalUsage ?? ev?.usage ?? {};
  const detail = u.inputTokenDetails ?? {};
  return {
    prompt_tokens: Number(u.inputTokens ?? 0),
    completion_tokens: Number(u.outputTokens ?? 0),
    cacheReadTokens: Number(detail.cacheReadTokens ?? 0),
    cacheWriteTokens: Number(detail.cacheWriteTokens ?? 0),
    systemPromptTokens: Number(ev?.systemPromptTokens ?? 0),
  };
}

/** error 事件 → 可读信息（error 为字符串或 {message,statusCode,isRetryable}） */
function ccErrorMessage(ev) {
  const e = ev?.error;
  if (typeof e === "string" && e) return e;
  if (e && typeof e === "object") return e.message ?? `上游返回错误（statusCode=${e.statusCode ?? "?"}）`;
  return "上游返回未知错误";
}

/** 下游 OpenAI 消息 → CommandCode wire 消息（assistant / tool / user 三类） */
function toCommandCodeMessages(messages) {
  const out = [];
  const nameById = new Map();
  for (const m of messages ?? []) {
    const role = m?.role;
    if (role === "assistant") {
      const blocks = [];
      const reasoning = m.reasoning_content ?? m.thinking;
      if (typeof reasoning === "string" && reasoning.trim()) blocks.push({ type: "reasoning", text: reasoning });
      const text = textOf(m.content);
      if (text) blocks.push({ type: "text", text });
      for (const tc of m.tool_calls ?? []) {
        const name = tc?.function?.name ?? "unknown";
        if (tc?.id) nameById.set(tc.id, name);
        blocks.push({
          type: "tool-call",
          toolCallId: tc?.id,
          toolName: name,
          input: safeJson(tc?.function?.arguments),
        });
      }
      if (blocks.length > 0) out.push({ role: "assistant", content: blocks });
      continue;
    }
    if (role === "tool") {
      out.push({
        role: "tool",
        content: [
          {
            type: "tool-result",
            toolCallId: m.tool_call_id,
            toolName: nameById.get(m.tool_call_id) ?? "unknown",
            output: { type: "text", value: textOf(m.content) },
          },
        ],
      });
      continue;
    }
    // user：文本与图片可同条
    const parts = [];
    if (typeof m?.content === "string") {
      if (m.content) parts.push({ type: "text", text: m.content });
    } else if (Array.isArray(m?.content)) {
      for (const p of m.content) {
        if (typeof p === "string") {
          if (p) parts.push({ type: "text", text: p });
        } else if (p?.type === "text") {
          if (p.text) parts.push({ type: "text", text: p.text });
        } else if (p?.type === "image_url") {
          const url = typeof p.image_url === "string" ? p.image_url : p.image_url?.url;
          const mime = /^data:([^;,]+)[;,]/.exec(String(url ?? ""))?.[1];
          if (url && mime) parts.push({ type: "image", image: url, mimeType: mime });
        }
      }
    }
    if (parts.length > 0) out.push({ role: "user", content: parts });
  }
  return out;
}

/** 工具定义 → CommandCode wire 工具 */
function toCommandCodeTools(tools) {
  return (tools ?? [])
    .filter((t) => t?.type === "function" && t.function?.name)
    .map((t) => ({
      name: t.function.name,
      description: t.function.description ?? "",
      input_schema: t.function.parameters ?? { type: "object", properties: {} },
    }));
}

/** 系统提示 → wire 分段数组（非末段补换行，与上游一致） */
function toCommandCodeSystem(parts) {
  const list = (parts ?? []).filter((p) => typeof p === "string" && p.length > 0);
  return list.map((text, i) => ({ type: "text", text: i < list.length - 1 ? `${text}\n` : text }));
}

/** 上传的 config 元数据（服务端不校验其内容，按上游形态自造） */
function commandCodeConfig() {
  return {
    workingDir: process.cwd(),
    date: new Date().toISOString().slice(0, 10),
    environment: process.platform,
    structure: [],
    isGitRepo: false,
    currentBranch: "",
    mainBranch: "",
    gitStatus: "",
    recentCommits: [],
  };
}

/** 会话级复用的 threadId：必须是合法 UUID，否则上游会丢弃该字段 */
const threadIdByKey = new Map();
function threadIdFor(body) {
  const first = (body.messages ?? []).find((m) => m?.role === "user");
  const seed = `${body.model ?? ""}|${JSON.stringify(first?.content ?? "")}`;
  const hit = threadIdByKey.get(seed);
  if (hit) return hit;
  const id = randomUUID();
  if (threadIdByKey.size > 2000) threadIdByKey.delete(threadIdByKey.keys().next().value);
  threadIdByKey.set(seed, id);
  return id;
}

/** CommandCode 的会话标识：sess_ + 稳定 UUID 去横线后的前 16 位（与上游 CLI 同构） */
function ccSessionId(body) {
  return `sess_${sessionIdFor(body).replace(/-/g, "").slice(0, 16)}`;
}

/** 本机已安装的 command-code 包根目录候选 */
function commandCodeRoots() {
  return [
    join(homedir(), "AppData", "Roaming", "npm", "node_modules", "command-code"),
    join(homedir(), ".npm-global", "lib", "node_modules", "command-code"),
    "/usr/local/lib/node_modules/command-code",
    "/usr/lib/node_modules/command-code",
  ];
}

/** 读已安装的 command-code 版本；读不到返回空串 */
function localCliVersion() {
  for (const root of commandCodeRoots()) {
    try {
      const v = JSON.parse(readFileSync(join(root, "package.json"), "utf8"))?.version;
      if (typeof v === "string" && v) return v;
    } catch {
      /* 换下一个候选路径 */
    }
  }
  return "";
}

/** 套餐档位标签见文件顶部的 CC_PLAN_RANK / CC_PLAN_LABEL（须在其后使用） */

/** 「Min plan」文案 → 档位数值（0 = 无门槛） */
function ccMinPlanRank(text) {
  const t = String(text ?? "").trim().toLowerCase();
  if (t.startsWith("max")) return CC_PLAN_RANK.max;
  if (t.startsWith("pro")) return CC_PLAN_RANK.pro;
  if (t.startsWith("goat")) return CC_PLAN_RANK.goat;
  if (t.startsWith("go")) return CC_PLAN_RANK.go;
  return 0;
}

/**
 * 上游权威模型目录（每项带最低套餐档位）。
 * CommandCode 没有 /v1/models 端点，清单来自 CLI 包内自带的 models.md：
 * 每行首列是精确模型 id，第 6 列是「Min plan」。
 * @param spec "auto"（读本机 CLI）| false（关闭）| 具体文件路径
 */
function commandCodeCatalogEntries(spec) {
  if (spec === false) return [];
  const files =
    spec && spec !== "auto"
      ? [String(spec)]
      : commandCodeRoots().map((r) => join(r, "dist", "bundled", "command-code-knowledge", "reference", "models.md"));
  for (const file of files) {
    // 只把「文件不存在」当作可跳过；解析阶段不上兜底 catch，避免把程序错误（如顺序/引用问题）静默吞掉
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    const entries = [];
    const seen = new Set();
    for (const line of text.split("\n")) {
      const m = /^\|\s*`([^`]+)`\s*\|(.*)\|\s*$/.exec(line);
      if (!m) continue;
      const id = m[1];
      if (seen.has(id)) continue;
      seen.add(id);
      const cols = m[2].split("|"); // 名称 | 上下文 | 思考档 | 价格 | Min plan | 适用
      entries.push({ id, minPlanRank: ccMinPlanRank(cols[4]) });
    }
    if (entries.length > 0) return entries;
  }
  return [];
}

/**
 * 探测当前账号的 CommandCode 套餐档位。
 * `/alpha/billing/subscriptions` 的 data.planId 形如 "individual-goat" / "teams-pro"，
 * 取最后一段即档位名。探测失败返回 null（不猜，交由调用方决定不过滤）。
 */
async function detectCommandCodePlan(up) {
  // 失败原因挂在上游对象上，由 /health 暴露（避免"探测失败但看不出为什么"）
  up.modelPlanError = null;
  if (!up.apiKey || up.apiKeySource === "none") {
    up.modelPlanError = "本地无可用凭据（未配置 apiKeyEnv / apiKeyFile）";
    return null;
  }
  try {
    const res = await fetch(`${up.baseUrl}/alpha/billing/subscriptions`, {
      headers: { authorization: `Bearer ${up.apiKey}`, "x-command-code-version": await commandCodeVersion(up) },
      signal: AbortSignal.timeout(8000),
    });
    if (!res.ok) {
      up.modelPlanError = `订阅接口返回 ${res.status}`;
      return null;
    }
    const planId = String((await res.json())?.data?.planId ?? "");
    const tier = planId.split("-").pop().toLowerCase();
    if (!Object.hasOwn(CC_PLAN_RANK, tier)) {
      up.modelPlanError = `未识别的套餐标识 "${planId}"`;
      return null;
    }
    return tier;
  } catch (e) {
    up.modelPlanError = e?.message ?? String(e);
    return null;
  }
}

/**
 * 按套餐过滤 commandcode 上游对外的模型清单（异步补齐，启动时触发一次）。
 * - 配置里显式 models 声明的不受套餐过滤（那是使用者的明确选择）
 * - 目录里的模型按「Min plan ≤ 当前套餐」保留
 * - 探测不到套餐则不过滤，并给出一条可操作的提示
 */
async function refineCommandCodeModels() {
  for (const u of UPSTREAMS) {
    if (u.protocol !== "commandcode") continue;
    u.modelPlan = null;
    if (u.modelCatalogPlan === false || u.catalogEntries.length === 0) continue;
    const explicit = String(u.modelCatalogPlan ?? "").toLowerCase();
    const tier = Object.hasOwn(CC_PLAN_RANK, explicit) ? explicit : await detectCommandCodePlan(u);
    if (!tier) {
      if (ACCESS_LOG) {
        log("注意: 未能确定 CommandCode 套餐，模型清单未按套餐过滤（可设 modelCatalogPlan: go|goat|pro|max 显式指定，或 false 关闭过滤）");
      }
      continue;
    }
    u.modelPlan = tier;
    const rank = CC_PLAN_RANK[tier];
    const catalogIds = new Set(u.catalogEntries.map((e) => e.id));
    const declared = u.modelsAll.filter((id) => !catalogIds.has(id)); // 配置声明的，不受过滤
    const allowed = u.catalogEntries.filter((e) => e.minPlanRank <= rank).map((e) => e.id);
    u.models = [...new Set([...declared, ...allowed])];
    if (ACCESS_LOG) {
      log(
        `CommandCode 套餐 ${CC_PLAN_LABEL[tier]}：清单按套餐过滤为 ${u.models.length} 个（目录共 ${u.catalogEntries.length} 个；设 modelCatalogPlan: false 可不过滤）`,
      );
    }
  }
}

/** 发往上游的 CLI 版本号：配置值 → 本机安装版本 → npm registry（30 分钟缓存） */
async function commandCodeVersion(up) {
  if (up.cliVersion && up.cliVersion !== "auto") return up.cliVersion;
  if (ccVersion.value && Date.now() - ccVersion.at < CC_VERSION_CACHE_MS) return ccVersion.value;
  const local = localCliVersion();
  if (local) {
    ccVersion.at = Date.now();
    ccVersion.value = local;
    return local;
  }
  try {
    const res = await fetch("https://registry.npmjs.org/command-code/latest", { signal: AbortSignal.timeout(8000) });
    const v = res.ok ? (await res.json())?.version : "";
    if (typeof v === "string" && v) {
      ccVersion.at = Date.now();
      ccVersion.value = v;
      return v;
    }
  } catch {
    /* 降级：留空由调用方决定 */
  }
  return "";
}

/** 事件流 → OpenAI SSE（含 pause_turn 续跑） */
async function pipeCommandCodeStream(upstreamBody, res, ctx) {
  res.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache",
    connection: "keep-alive",
    "x-bridge-upstream": ctx.upstream.name,
    "x-bridge-upstream-model": ctx.upstreamModel,
  });

  let finishReason = "stop";
  let usage = { prompt_tokens: 0, completion_tokens: 0 };
  let streamError = "";
  let sawFinish = false;
  let paused = false;
  const chunk = (delta, extra = {}) =>
    `data: ${JSON.stringify({
      id: "chatcmpl-bridge",
      object: "chat.completion.chunk",
      created: Math.floor(Date.now() / 1000),
      model: ctx.requestedModel,
      choices: [{ index: 0, delta, finish_reason: null }],
      ...extra,
    })}\n\n`;

  res.write(chunk({ role: "assistant", content: "" }));

  // sink 只建一次：工具索引必须跨续跑连续，否则下游会看到错位的 tool_calls
  const sink = makeCommandCodeSink({
    text: (t) => res.write(chunk({ content: t })),
    reasoning: (t) => res.write(chunk({ reasoning_content: t })),
    toolStart: (index, id, name) =>
      res.write(chunk({ tool_calls: [{ index, id, type: "function", function: { name, arguments: "" } }] })),
    toolInput: (index, delta) => res.write(chunk({ tool_calls: [{ index, function: { arguments: delta } }] })),
    toolCall: (index, id, name, input, known) => {
      if (known) return; // 参数已由增量下发
      res.write(
        chunk({
          tool_calls: [{ index, id, type: "function", function: { name, arguments: JSON.stringify(input ?? {}) } }],
        }),
      );
    },
    finish: (reason, u, raw) => {
      finishReason = reason;
      usage = {
        prompt_tokens: usage.prompt_tokens + u.prompt_tokens,
        completion_tokens: usage.completion_tokens + u.completion_tokens,
        cacheReadTokens: (usage.cacheReadTokens ?? 0) + u.cacheReadTokens,
        cacheWriteTokens: (usage.cacheWriteTokens ?? 0) + u.cacheWriteTokens,
      };
      sawFinish = true;
      paused = raw === "pause_turn";
    },
    error: (message) => {
      streamError = message;
    },
  });

  // pause_turn 续跑：上游用同一份 body 重发即可续上（与 CLI 的 n=0..Ph 循环一致）
  let source = upstreamBody;
  let attempts = 0;
  for (;;) {
    await consumeCommandCode(source, sink);
    if (!paused || !ctx.pauseTurn || !ctx.request || attempts >= CC_PAUSE_TURN_MAX) break;
    attempts += 1;
    if (ACCESS_LOG) log(`注意: 上游返回 pause_turn，续跑第 ${attempts} 次（上限 ${CC_PAUSE_TURN_MAX} 次）`);
    let next;
    try {
      next = await fetch(ctx.request.url, {
        method: "POST",
        headers: ctx.request.headers,
        body: JSON.stringify(ctx.request.payload),
        signal: ctx.signal,
      });
    } catch (e) {
      streamError = `续跑请求失败：${e.message}`;
      break;
    }
    if (!next.ok || !next.body) {
      streamError = `续跑请求失败：上游返回 ${next.status}`;
      break;
    }
    source = next.body;
  }

  res.write(
    `data: ${JSON.stringify({
      id: "chatcmpl-bridge",
      object: "chat.completion.chunk",
      created: Math.floor(Date.now() / 1000),
      model: ctx.requestedModel,
      choices: [{ index: 0, delta: {}, finish_reason: finishReason }],
      usage: {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.prompt_tokens + usage.completion_tokens,
        // 把上游的缓存读命中透给下游（OpenAI 的形状），便于调用方核对缓存效果
        ...(usage.cacheReadTokens > 0 ? { prompt_tokens_details: { cached_tokens: usage.cacheReadTokens } } : {}),
      },
    })}\n\n`,
  );
  res.write("data: [DONE]\n\n"); // 上游不发 [DONE]，由本桥补齐
  res.end();
  ctx.onUsage?.({ ...usage, truncated: !sawFinish, paused, error: streamError });
}

/** NDJSON → 聚合后的 chat.completion（非流式下游；上游恒为流式，本桥本地聚合；含 pause_turn 续跑） */
async function aggregateCommandCode(rawText, ctx) {
  const text = [];
  const reasoning = [];
  const calls = [];
  let finishReason = "stop";
  let usage = { prompt_tokens: 0, completion_tokens: 0 };
  let streamError = "";
  let sawFinish = false;
  let paused = false;
  const sink = makeCommandCodeSink({
    text: (t) => text.push(t),
    reasoning: (t) => reasoning.push(t),
    toolStart: (index, id, name) => {
      calls[index] = { id, name, args: "" };
    },
    toolInput: (index, delta) => {
      if (calls[index]) calls[index].args += delta;
    },
    toolCall: (index, id, name, input, known) => {
      if (!calls[index]) calls[index] = { id, name, args: "" };
      if (!known) calls[index].args = JSON.stringify(input ?? {});
      calls[index].id = id ?? calls[index].id;
      calls[index].name = name ?? calls[index].name;
    },
    finish: (reason, u, raw) => {
      finishReason = reason;
      usage = {
        prompt_tokens: usage.prompt_tokens + u.prompt_tokens,
        completion_tokens: usage.completion_tokens + u.completion_tokens,
        cacheReadTokens: (usage.cacheReadTokens ?? 0) + u.cacheReadTokens,
        cacheWriteTokens: (usage.cacheWriteTokens ?? 0) + u.cacheWriteTokens,
      };
      sawFinish = true;
      paused = raw === "pause_turn";
    },
    error: (message) => {
      streamError = message;
    },
  });

  // pause_turn 续跑：同一份 body 重发，首个响应是文本、后续响应是流
  let source = String(rawText ?? "");
  let attempts = 0;
  for (;;) {
    await consumeCommandCode(source, sink);
    if (!paused || !ctx.pauseTurn || !ctx.request || attempts >= CC_PAUSE_TURN_MAX) break;
    attempts += 1;
    if (ACCESS_LOG) log(`注意: 上游返回 pause_turn，续跑第 ${attempts} 次（上限 ${CC_PAUSE_TURN_MAX} 次）`);
    let next;
    try {
      next = await fetch(ctx.request.url, {
        method: "POST",
        headers: ctx.request.headers,
        body: JSON.stringify(ctx.request.payload),
        signal: ctx.signal,
      });
    } catch (e) {
      streamError = `续跑请求失败：${e.message}`;
      break;
    }
    if (!next.ok || !next.body) {
      streamError = `续跑请求失败：上游返回 ${next.status}`;
      break;
    }
    source = next.body;
  }

  const body = text.join("");
  const message = { role: "assistant", content: body || null };
  if (reasoning.length > 0) message.reasoning_content = reasoning.join("");
  const list = calls.filter(Boolean);
  if (list.length > 0) {
    message.tool_calls = list.map((c, i) => ({
      index: i,
      id: c.id,
      type: "function",
      function: { name: c.name, arguments: c.args || "{}" },
    }));
    if (!body) message.content = null;
  }
  return {
    result: {
      id: `chatcmpl-${Date.now().toString(36)}`,
      object: "chat.completion",
      created: Math.floor(Date.now() / 1000),
      model: ctx.requestedModel,
      choices: [{ index: 0, message, finish_reason: finishReason }],
      usage: {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.prompt_tokens + usage.completion_tokens,
        // 把上游的缓存读命中透给下游（OpenAI 的形状），便于调用方核对缓存效果
        ...(usage.cacheReadTokens > 0 ? { prompt_tokens_details: { cached_tokens: usage.cacheReadTokens } } : {}),
      },
    },
    meta: { truncated: !sawFinish, paused, error: streamError, usage },
  };
}

/** commandcode 协议：OpenAI 请求 → /alpha/generate，NDJSON 事件流 → OpenAI */
const commandcodeProtocol = {
  async buildRequest(ctx) {
    const up = ctx.upstream;
    // 凭据：照搬 CommandCode 自身顺序（环境变量 → auth.json → 配置值）；
    // 都没有时本地直接报 401，不把必然失败的请求打到上游。
    if (!up.apiKey || up.apiKeySource === "none") {
      return {
        error: {
          status: 401,
          message: "未配置 CommandCode 凭据：请设置 COMMAND_CODE_API_KEY，或先在本机登录 CommandCode（~/.commandcode/auth.json）",
          type: "authentication_error",
          code: "missing_api_key",
        },
      };
    }

    const headers = { "content-type": "application/json", "user-agent": "cli" };
    const version = await commandCodeVersion(up);
    if (version) headers["x-command-code-version"] = version;
    else if (ACCESS_LOG) log("注意: 未能确定 CommandCode CLI 版本，上游可能以 403 upgrade_required 拒绝");
    if (up.cliEnvironment) headers["x-cli-environment"] = up.cliEnvironment;
    headers["x-session-id"] = ccSessionId(ctx.body);
    headers.authorization = `Bearer ${up.apiKey}`;
    // ZDR（Zero Data Retention）安全头：默认开启，告知上游执行零数据留存策略（可通过 cmdZdr: false 关闭）
    if (up.cmdZdr) headers["x-cmd-zdr"] = "1";

    const systemParts = [];
    const rest = [];
    for (const m of ctx.body.messages ?? []) {
      if (m?.role === "system" || m?.role === "developer") systemParts.push(textOf(m.content));
      else rest.push(m);
    }
    const params = {
      model: ctx.upstreamModel,
      messages: toCommandCodeMessages(rest),
      tools: toCommandCodeTools(ctx.body.tools),
      system: toCommandCodeSystem(systemParts),
      max_tokens: Number(ctx.body.max_tokens ?? ctx.body.max_completion_tokens ?? CC_MAX_TOKENS),
      stream: true, // 上游恒为流式，非流式由本桥聚合
    };
    if (ctx.body.temperature !== undefined) params.temperature = ctx.body.temperature;
    const effort = ctx.body.reasoning_effort;
    if (typeof effort === "string" && effort) params.reasoning_effort = effort;
    // 强制工具选择：CommandCode 的 wire 里本桥未声明该字段，故不下发（上游按默认 auto 处理）。
    // 与 anthropic 分支的降级处理保持一致——明确记录，避免"以为强制了、其实没有"。
    const choice = ctx.body.tool_choice;
    if (choice && choice !== "auto" && choice !== "none" && ACCESS_LOG) {
      log("注意: commandcode 上游不下发 tool_choice，强制选择按 auto 处理（模型仍可自主调用工具）");
    }
    return {
      url: `${up.baseUrl}/alpha/generate`,
      headers,
      payload: {
        config: commandCodeConfig(),
        memory: null,
        taste: null,
        skills: null,
        permissionMode: "standard",
        // threadId 必须是合法 UUID，否则上游会静默丢弃该字段
        threadId: threadIdFor(ctx.body),
        params,
      },
      meta: { logSuffix: "" },
    };
  },
  async parseResponse(raw, ctx) {
    // 上游只吐事件流：非流式由本地聚合同一套事件（含 pause_turn 续跑）
    const { result, meta } = await aggregateCommandCode(raw, ctx);
    return { body: result, meta };
  },
  async parseStream(upstreamBody, res, ctx) {
    await pipeCommandCodeStream(upstreamBody, res, ctx);
  },
};

/** 协议名 → 适配器；未登记的协议按 openai 处理（与重构前行为一致） */
const PROTOCOLS = { anthropic: anthropicProtocol, openai: openaiProtocol, commandcode: commandcodeProtocol };

function protocolFor(up) {
  return PROTOCOLS[up.protocol] ?? PROTOCOLS.openai;
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
  // 决定打到哪条上游：URL 前缀优先，其次按模型名匹配（含上游级别名）
  const up = forcedUpstream ?? pickUpstream(requestedModel);

  // 统一在本地完成模型别名解析：下游看到的别名/模型名原样返回，
  // 送给上游的名字按「上游级别名 → 全局别名 → 原样」解析。
  const upstreamModel = upstreamModelFor(up, requestedModel);

  const adapter = protocolFor(up);
  const build = await adapter.buildRequest({
    body,
    requestedModel,
    upstreamModel,
    upstream: up,
    clientAuth: req.headers.authorization,
  });
  // 协议在本地即可判定的失败（如 commandcode 缺凭据）：直接返回，不把必然失败的请求打到上游
  if (build.error) {
    const e = build.error;
    return json(res, e.status ?? 500, errorBody(e.message, e.type, e.code));
  }
  const logSuffix = build.meta?.logSuffix ?? "";

  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), TIMEOUT_MS);

  let upstream;
  try {
    upstream = await fetch(build.url, {
      method: "POST",
      headers: build.headers,
      body: JSON.stringify(build.payload),
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
      await adapter.parseStream(upstream.body, res, {
        requestedModel,
        upstreamModel,
        upstream: up,
        logSuffix,
        signal: ac.signal,
        request: build, // 供 pause_turn 续跑用同一份 body 重发
        pauseTurn: up.pauseTurn,
        onUsage: (usage) => {
          clearTimeout(timer);
          if (ACCESS_LOG) {
            // commandcode 协议在此额外上报截断/续跑/流内错误（其它协议不带这些字段）
            const extra = `${usage.truncated ? " 截断(无 finish)" : ""}${usage.paused ? " pause_turn 未续完" : ""}${usage.cacheReadTokens ? ` cache=${usage.cacheReadTokens}+${usage.cacheWriteTokens ?? 0}` : ""}${usage.error ? ` 上游错误: ${usage.error}` : ""}`;
            log(
              `${requestedModel} -> ${up.name}:${upstreamModel} 200 ${Date.now() - started}ms stream tokens=${usage.prompt_tokens}+${usage.completion_tokens}${logSuffix}${extra}`,
            );
          }
        },
      });
    } catch (e) {
      clearTimeout(timer);
      if (ACCESS_LOG) log(`${requestedModel} -> ${up.name}:${upstreamModel} 流式中断 ${Date.now() - started}ms ${e.message}`);
      res.end();
    }
    return;
  }

  const raw = await upstream.text();
  // 注意：不在这里 clearTimeout —— 协议可能有后续请求（如 commandcode 的 pause_turn 续跑），
  // 超时应当覆盖整段处理；同步返回的协议不受影响。
  const parsed = await adapter.parseResponse(raw, {
    requestedModel,
    upstreamModel,
    upstream: up,
    signal: ac.signal,
    request: build,
    pauseTurn: up.pauseTurn,
  });
  clearTimeout(timer);
  if (parsed.error) return json(res, 502, errorBody(parsed.error));
  const result = parsed.body;
  const metaSuffix = `${parsed.meta?.truncated ? " 截断(无 finish)" : ""}${parsed.meta?.paused ? " pause_turn 未续完" : ""}${parsed.meta?.usage?.cacheReadTokens ? ` cache=${parsed.meta.usage.cacheReadTokens}+${parsed.meta.usage.cacheWriteTokens ?? 0}` : ""}${parsed.meta?.error ? ` 上游错误: ${parsed.meta.error}` : ""}`;
  if (ACCESS_LOG) {
    log(
      `${requestedModel} -> ${up.name}:${upstreamModel} 200 ${Date.now() - started}ms tokens=${result.usage?.prompt_tokens ?? "?"}+${result.usage?.completion_tokens ?? "?"}${logSuffix}${metaSuffix}`,
    );
  }
  // 上游语义：流结束仍未收到 finish 事件即视为截断（可重试），与 CLI 一致
  if (parsed.meta?.truncated) {
    return json(res, 502, errorBody("上游响应被截断（未收到 finish 事件）", "upstream_error", "upstream_truncated"));
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
        // models = 对外可见（可能已按套餐过滤）；modelsAll = 声明 + 目录全集
        models: u.models.length,
        modelsAll: (u.modelsAll ?? u.models).length,
        plan: u.modelPlan ?? null,
        planError: u.modelPlanError ?? null,
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

  // 服务已监听后再做套餐探测与清单收窄：此刻模块级常量全部初始化完毕（避免 TDZ 类错误）
  refineCommandCodeModels().catch((e) => {
    if (ACCESS_LOG) log(`注意: CommandCode 套餐探测/过滤失败：${e?.message ?? e}`);
  });
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
