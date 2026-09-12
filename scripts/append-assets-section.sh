#!/usr/bin/env bash
# 幂等追加「发布资产与校验方式」段到 release 说明（重跑不堆叠）
#
# 为什么需要幂等：重跑发布是常态，用标记做替换而不是无脑追加，说明才不会越跑越长。
#
# 用法：
#   bash scripts/append-assets-section.sh <tag> <asset> [asset ...]
# 环境：GH_TOKEN 或 GITHUB_TOKEN（CI 里由 ${{ github.token }} 提供）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -ge 2 ] || die "用法：bash scripts/append-assets-section.sh <tag> <asset> [asset ...]"
TAG="$1"; shift
ASSETS=("$@")

if ! have_gh; then
  die "缺少命令：gh（本步骤通常只在 CI 运行；本地需先安装 GitHub CLI：winget install GitHub.cli）"
fi
TOKEN="$(resolve_token)" || true
if [ -z "$TOKEN" ]; then
  die "未取到 GitHub 令牌（GH_TOKEN / GITHUB_TOKEN / git credential fill 均失败）"
fi
export GH_TOKEN="$TOKEN"

START='<!-- model-bridge-assets:start -->'
END='<!-- model-bridge-assets:end -->'

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP" 2>/dev/null || true' EXIT

gh release view "$TAG" --json body --jq '.body // ""' > "$TMP/body.md"
# 先删掉旧段（存在才删），保证幂等
sed -e "/$START/,/$END/d" "$TMP/body.md" > "$TMP/body.clean.md"

{
  cat "$TMP/body.clean.md"
  printf '\n%s\n' "$START"
  printf '## 发布资产与校验方式\n\n'
  for a in "${ASSETS[@]}"; do
    printf -- '- `%s`\n' "$a"
  done
  printf '\n校验下载到的文件（与本文件同目录）：\n\n'
  printf '```bash\nsha256sum -c SHA256SUMS        # Linux/macOS\n'
  printf 'Get-FileHash <文件> -Algorithm SHA256   # Windows，与 SHA256SUMS 逐条对照\n```\n'
  printf '\n本包为跨平台 Node 单文件程序：解压后 `node model-bridge.js` 即可运行（需 Node 18+）。\n'
  printf '%s\n' "$END"
} > "$TMP/body.new.md"

gh release edit "$TAG" --notes-file "$TMP/body.new.md" >/dev/null
log "已更新 $TAG 的发行说明（资产段幂等替换）"
