#!/usr/bin/env node
// 渲染发行说明。
//
// 规则（与知识库《GitHub Actions 流水线打包发布》一致）：
//   - 优先用 changelog/v<version>.json：写给用户看效果，不写实现；纯内部改动（重构/CI/测试）不列
//   - 文件缺失时属于「发布前置条件不满足」→ 默认直接失败；显式加 --allow-fallback 才退化为提交清单
//   - 末尾始终附 compare/<prev>...<tag> 链接；首发没有上一个 tag 时退化为 commits/<tag>
//
// 用法：
//   node scripts/render-notes.mjs <version> [--repo owner/name] [--prev vX.Y.Z] [--allow-fallback]
// 例：
//   node scripts/render-notes.mjs 1.0.0 --repo klaus2918/model-bridege > NOTES.md

import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

const argv = process.argv.slice(2);
const version = argv.find((a) => !a.startsWith("--"));
if (!version) {
  console.error("用法：node scripts/render-notes.mjs <version> [--repo owner/name] [--prev vX.Y.Z] [--allow-fallback]");
  process.exit(1);
}

const flagValue = (name) => {
  const i = argv.indexOf(name);
  if (i < 0) return null;
  const next = argv[i + 1];
  return next && !next.startsWith("--") ? next : true;
};

const repo = flagValue("--repo");
const prev = flagValue("--prev");
const allowFallback = argv.includes("--allow-fallback");

const changelogPath = join(process.cwd(), "changelog", `v${version}.json`);
const out = [];

function commitList(range) {
  try {
    const text = execFileSync("git", ["log", "--no-merges", "--pretty=- %s (%h)", range], { encoding: "utf8" });
    return text.split("\n").map((s) => s.trim()).filter(Boolean);
  } catch {
    return [];
  }
}

if (existsSync(changelogPath)) {
  const c = JSON.parse(readFileSync(changelogPath, "utf8"));
  out.push(`## ${c.title ?? `v${version}`}`, "");
  out.push(`**版本**：v${c.version ?? version}　**日期**：${c.date ?? "-"}`, "");
  const sections = [["highlights", "亮点"], ["improvements", "改进"], ["fixes", "修复"]];
  for (const [key, label] of sections) {
    const items = c[key] ?? [];
    if (items.length === 0) continue;
    out.push(`### ${label}`, ...items.map((x) => `- ${x}`), "");
  }
} else if (allowFallback) {
  console.error(`[warn] 缺少 changelog/v${version}.json，退化为提交清单（不应用于正式发布）`);
  out.push(`## v${version}`, "", "### 变更（自动生成，未整理）", "");
  const items = commitList(prev ? `${prev}..v${version}` : "v" + version);
  out.push(...(items.length ? items : ["- （无法从 git 历史生成清单）"]), "");
} else {
  console.error(`[error] 缺少 changelog/v${version}.json —— 发布前置条件不满足，先补再发`);
  process.exit(1);
}

if (repo && repo !== true) {
  const tag = `v${version}`;
  const link = prev && prev !== true
    ? `https://github.com/${repo}/compare/${prev}...${tag}`
    : `https://github.com/${repo}/commits/${tag}`;
  out.push("---", "", `**完整变更**：${link}`, "");
}

process.stdout.write(out.join("\n"));
