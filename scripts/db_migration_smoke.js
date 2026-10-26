#!/usr/bin/env node
/*
 * 历史数据库兼容性冒烟：
 *   1. 用旧版 schema 创建临时数据库并写入旧提示词/用量；
 *   2. 启动当前 exe，验证自动迁移后旧数据仍可读；
 *   3. 通过代理写入一条新提示词，验证迁移后的新写入正常。
 * 全程使用临时目录与独立端口，不接触正在运行的实例。
 *
 * 用法：node scripts/db_migration_smoke.js --exe dist/Model-Gate-rust.exe
 */

"use strict";

const { spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { DatabaseSync } = require("node:sqlite");

const argv = process.argv.slice(2);
const getArg = (name, fallback = "") => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const exeArg = getArg("exe", "");
const exePath = exeArg ? path.resolve(exeArg) : "";
const port = Number(getArg("port", "18950"));
const keep = argv.includes("--keep");
const base = `http://127.0.0.1:${port}`;
const failures = [];
let server = null;
let tempDir = "";

function ok(name, detail = "") {
  console.log(`[ok] ${name}${detail ? ` · ${detail}` : ""}`);
}
function bad(name, error) {
  failures.push(`${name}: ${error.message || error}`);
  console.error(`[FAIL] ${name} -> ${error.message || error}`);
}
async function check(name, fn) {
  try {
    ok(name, (await fn()) || "");
  } catch (error) {
    bad(name, error);
  }
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(fn, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      const value = await fn();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    await sleep(150);
  }
  throw new Error(`${label}超时${last ? `：${last.message}` : ""}`);
}

async function request(url, options = {}, timeoutMs = 8000) {
  const response = await fetch(url, { ...options, signal: AbortSignal.timeout(timeoutMs) });
  const text = await response.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  return { status: response.status, body };
}

const legacySchema = `
CREATE TABLE usage_records (
    id TEXT PRIMARY KEY, ts INTEGER NOT NULL, created_at TEXT NOT NULL,
    upstream TEXT, protocol TEXT, requested_model TEXT, upstream_model TEXT,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    status INTEGER, stream INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0, paused INTEGER NOT NULL DEFAULT 0,
    error TEXT, cost REAL NOT NULL DEFAULT 0, session TEXT
);
CREATE TABLE prompt_records (
    id TEXT PRIMARY KEY, ts INTEGER NOT NULL, created_at TEXT NOT NULL,
    session TEXT, requested_model TEXT, message_count INTEGER NOT NULL DEFAULT 0,
    size_bytes INTEGER NOT NULL DEFAULT 0, truncated INTEGER NOT NULL DEFAULT 0,
    preview TEXT, messages TEXT NOT NULL
);
INSERT INTO usage_records (
    id, ts, created_at, upstream, requested_model, prompt_tokens,
    completion_tokens, total_tokens, status, session
) VALUES ('old-usage', 1700000000, '2023-11-14T00:00:00.000+08:00',
          'legacy', 'm', 10, 2, 12, 200, 'legacy-session');
INSERT INTO prompt_records (
    id, ts, created_at, session, requested_model, message_count,
    size_bytes, truncated, preview, messages
) VALUES ('old-prompt', 1700000000, '2023-11-14T00:00:00.000+08:00',
          'legacy-session', 'm', 1, 25, 0, 'hello',
          '[{"role":"user","content":"hello"}]');
`;

function prepareLegacyDb() {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "mg-db-migration-"));
  const dataDir = path.join(tempDir, "data");
  fs.mkdirSync(dataDir, { recursive: true });
  const dbPath = path.join(dataDir, "modelgate.db");
  const db = new DatabaseSync(dbPath);
  db.exec(legacySchema);
  db.close();

  const configPath = path.join(tempDir, "bridge.config.json");
  const config = {
    port,
    host: "127.0.0.1",
    defaultModel: "m",
    models: ["m"],
    promptCapture: true,
    promptRetentionDays: 0,
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
  return { configPath, dbPath };
}

async function main() {
  if (!exePath || !fs.existsSync(exePath)) throw new Error(`找不到待测 exe：${exePath}`);
  const { configPath, dbPath } = prepareLegacyDb();

  server = spawn(
    exePath,
    ["--no-gui", "--port", String(port), "--config", configPath, "--db", dbPath, "--force-multi"],
    { cwd: path.dirname(exePath), stdio: ["ignore", "ignore", "pipe"], windowsHide: true },
  );
  let stderr = "";
  server.stderr.on("data", (chunk) => {
    stderr += String(chunk);
  });

  try {
    await waitFor(async () => {
      try {
        return (await request(`${base}/health`, {}, 1000)).status === 200;
      } catch {
        return false;
      }
    }, 15000, "服务就绪");
  } catch (error) {
    throw new Error(`${error.message}\n${stderr.slice(-2000)}`);
  }

  await check("旧提示词记录可列出且自动迁移", async () => {
    const result = await request(`${base}/api/prompts?range=all&limit=10`);
    if (result.status !== 200) throw new Error(`HTTP ${result.status}`);
    const rows = Array.isArray(result.body?.rows) ? result.body.rows : [];
    const old = rows.find((row) => row.id === "old-prompt");
    if (!old) throw new Error("未找到旧记录 old-prompt");
    if (old.redactions !== 0) throw new Error(`redactions 默认值异常：${old.redactions}`);
    if (old.truncation !== null) throw new Error("truncation 应为 null");
    if (old.policy_version !== 0) throw new Error(`policy_version 默认值异常：${old.policy_version}`);
    if (old.preview !== "hello") throw new Error(`旧预览丢失：${old.preview}`);
    return `total=${result.body.total}`;
  });

  await check("旧提示词详情可读", async () => {
    const result = await request(`${base}/api/prompts/old-prompt`);
    if (result.status !== 200) throw new Error(`HTTP ${result.status}`);
    const body = result.body || {};
    if (body.messages?.[0]?.content !== "hello") throw new Error("旧 messages 内容丢失");
    const previews = body.call_chain?.message_previews;
    if (!Array.isArray(previews) || previews[0]?.preview !== "hello") {
      throw new Error("旧记录未生成调用链预览");
    }
    return `messages=${body.messages.length}`;
  });

  await check("旧用量记录可读", async () => {
    const result = await request(`${base}/api/usage/list?range=all&limit=20`);
    if (result.status !== 200) throw new Error(`HTTP ${result.status}`);
    const rows = Array.isArray(result.body?.rows) ? result.body.rows : [];
    const old = rows.find((row) => row.id === "old-usage");
    if (!old) throw new Error("未找到旧用量记录 old-usage");
    if (Number(old.prompt_tokens) !== 10 || Number(old.status) !== 200) {
      throw new Error(`旧用量字段异常：${JSON.stringify(old).slice(0, 200)}`);
    }
    return "old-usage 保留";
  });

  await check("迁移后可写入新提示词", async () => {
    await request(
      `${base}/v1/chat/completions`,
      {
        method: "POST",
        headers: { "content-type": "application/json", "x-session-id": "legacy-session" },
        body: JSON.stringify({
          model: "m",
          stream: false,
          messages: [
            { role: "system", content: "legacy migration smoke" },
            { role: "user", content: "new prompt after migration" },
          ],
        }),
      },
      8000,
    ).catch(() => null);

    const found = await waitFor(async () => {
      const result = await request(`${base}/api/prompts?range=all&limit=20`);
      const rows = Array.isArray(result.body?.rows) ? result.body.rows : [];
      return rows.find((row) => row.id !== "old-prompt");
    }, 10000, "新提示词落库");

    const detail = await request(`${base}/api/prompts/${encodeURIComponent(found.id)}`);
    if (detail.status !== 200) throw new Error(`新记录详情 HTTP ${detail.status}`);
    const previews = detail.body?.call_chain?.message_previews;
    if (!Array.isArray(previews) || previews.length < 2) {
      throw new Error("新记录缺少调用链预览");
    }
    return `新记录 ${found.id.slice(0, 8)}…`;
  });
}

async function cleanup() {
  if (server && server.exitCode === null) {
    await new Promise((resolve) => {
      server.once("exit", resolve);
      try {
        server.kill();
      } catch {
        resolve();
      }
      setTimeout(resolve, 3000);
    });
  }
  if (tempDir) {
    if (keep || failures.length > 0) {
      console.log(`[info] 测试现场保留：${tempDir}`);
    } else {
      try {
        fs.rmSync(tempDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
      } catch (error) {
        console.warn(`[warn] 临时目录清理失败：${error.message}`);
      }
    }
  }
}

main()
  .catch((error) => {
    bad("迁移测试主流程", error);
    console.error(error.stack || error);
    process.exitCode = 1;
  })
  .finally(async () => {
    await sleep(100);
    await cleanup();
    console.log(`\n结果：${failures.length === 0 ? "全部通过" : `${failures.length} 项失败`}`);
    if (failures.length) {
      for (const item of failures) console.error(`- ${item}`);
      process.exitCode = 1;
    }
  });
