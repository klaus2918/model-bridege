#!/usr/bin/env bash
# 发布后自检：把「已经公开的 release」当被测对象
#
# 为什么不能只看本地日志：校验和只证明上传前文件是什么，上传阶段本身可能损坏；
# 只有反向把已发布资产拉回来重新核对，才能证明远程真实状态。
#
# 五组断言：
#   1. 元数据：release 已公开（非 draft）、非 prerelease
#   2. 资产集合：恰好等于 <产品>-v<版本>.zip + SHA256SUMS
#   3. 校验和：manifest 恰好覆盖该 zip，且逐条哈希匹配
#   4. 产物：对下载回来的 zip 跑 scripts/verify-package.sh（含真实启动 smoke）
#   5. 发行说明：含资产段标记，且点名了两个资产
#
# 令牌回退链：GH_TOKEN → GITHUB_TOKEN → git credential fill；无令牌走匿名 API（每 IP 每小时 60 次）
# 用法：
#   bash scripts/verify-release.sh <tag> [--keep-tmp]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -ge 1 ] || die "用法：bash scripts/verify-release.sh <tag> [--keep-tmp]"
TAG="$1"; shift || true
KEEP_TMP=0
[ "${1:-}" = "--keep-tmp" ] && KEEP_TMP=1

require_cmd curl sha256sum git node

ROOT="$(repo_root)"
VERSION="${TAG#v}"
ZIP_NAME="model-bridge-$TAG.zip"

TMP="$(mktemp -d)"
cleanup() { [ "$KEEP_TMP" -eq 1 ] || rm -rf "$TMP" 2>/dev/null || true; }
trap cleanup EXIT

fail=0
ok()  { printf '  [ok]   %s\n' "$*"; }
bad() { printf '  [fail] %s\n' "$*" >&2; fail=$((fail + 1)); }

# ── 0. 拉 release 元数据（含草稿：by-tag 接口对草稿返回 404，故走 release_json）──
# set -e 下用 if 捕获，避免非零返回导致静默退出
if json="$(release_json "$TAG")"; then
  rc=0
else
  rc=$?
fi
if [ "$rc" -ne 0 ]; then
  bad "查不到 release：$TAG（HTTP 非 200；tag 是否已推、workflow 是否被启用？）"
  echo
  printf '[FAIL] 发布后自检无法进行\n' >&2
  exit 1
fi
printf '%s' "$json" > "$TMP/release.json"
node -e '
  const fs = require("node:fs");
  const r = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  fs.writeFileSync(process.argv[2], r.body ?? "");
  fs.writeFileSync(process.argv[3], ((r.assets ?? []).map((a) => a.name).sort().join("\n") || "") + "\n");
  console.log(JSON.stringify({ isDraft: !!r.isDraft, prerelease: !!r.prerelease }));
' "$TMP/release.json" "$TMP/body.md" "$TMP/assets.raw" > "$TMP/flags.json"
LC_ALL=C sort "$TMP/assets.raw" > "$TMP/assets.txt"

# ── 1. 元数据 ──
if grep -q '"isDraft":false' "$TMP/flags.json"; then
  ok "release 已公开（非 draft）"
else
  bad "release 仍是 draft（半成品未发布）"
fi
if grep -q '"prerelease":false' "$TMP/flags.json"; then
  ok "非 prerelease"
else
  bad "被标记为 prerelease"
fi

# ── 2. 资产集合 ──
# 两侧都用 LC_ALL=C 排序：node 的 sort 是码位序，shell sort 受 locale 影响（本机 en_US 与 CI C 序不同），
# 不统一口径会做出「集合不符」的假失败
printf '%s\n%s\n' "$ZIP_NAME" "SHA256SUMS" | LC_ALL=C sort > "$TMP/expected.txt"
if diff -q "$TMP/expected.txt" "$TMP/assets.txt" >/dev/null 2>&1; then
  ok "资产集合恰好等于 { $ZIP_NAME, SHA256SUMS }"
else
  bad "资产集合不符："
  diff "$TMP/expected.txt" "$TMP/assets.txt" | sed 's/^/      /' || true
fi

# ── 3. 下载资产 ──
if download_release_assets "$TAG" "$TMP/assets" >/dev/null 2>&1; then
  ok "已下载全部资产用于复核"
else
  bad "资产下载失败"
fi

if [ -f "$TMP/assets/SHA256SUMS" ]; then
  if [ "$(wc -l < "$TMP/assets/SHA256SUMS" | tr -d ' ')" = "1" ]; then
    ok "manifest 只覆盖 1 条资产（未混入未发布的平台）"
  else
    bad "manifest 条目数不为 1，可能与发布资产不对齐"
  fi
  if ( cd "$TMP/assets" && sha256sum -c SHA256SUMS >/dev/null 2>&1 ); then
    ok "逐条哈希匹配：sha256sum -c SHA256SUMS 通过"
  else
    bad "校验和不匹配（上传损坏，或 manifest 与资产不对齐）"
  fi
fi

# ── 4. 产物自检 ──
if [ -f "$TMP/assets/$ZIP_NAME" ]; then
  if bash "$SCRIPT_DIR/verify-package.sh" "$TMP/assets/$ZIP_NAME" > "$TMP/pkg.log" 2>&1; then
    ok "下载回来的产物通过 verify-package 自检"
  else
    bad "产物自检失败，详见："
    sed 's/^/      /' "$TMP/pkg.log"
  fi
fi

# ── 5. 发行说明 ──
if grep -q 'model-bridge-assets:start' "$TMP/body.md" 2>/dev/null; then
  ok "发行说明含资产与校验段（幂等标记存在）"
else
  bad "发行说明缺少资产与校验段"
fi
if grep -q "$ZIP_NAME" "$TMP/body.md" 2>/dev/null && grep -q 'SHA256SUMS' "$TMP/body.md" 2>/dev/null; then
  ok "发行说明点名了两个资产（用户知道该下什么）"
else
  bad "发行说明未点名资产文件名"
fi

echo
if [ "$fail" -gt 0 ]; then
  printf '[FAIL] 发布后自检失败 %d 项：%s\n' "$fail" "$TAG" >&2
  exit 1
fi
printf '[OK] 发布后自检通过：%s（版本 %s）\n' "$TAG" "$VERSION"
