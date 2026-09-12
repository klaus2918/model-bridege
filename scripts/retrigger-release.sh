#!/usr/bin/env bash
# 重触发发布：删除并同名重建 tag（用于「tag 推了但事件被丢弃」或「发布链路刚改，要按新提交重跑」）
#
# 为什么 --at 是必需概念而不是可选项：tag 指向旧提交时，重跑的还是旧 workflow。
# 每条拒绝条件都对应一类真实事故，宁可停下来让人确认。
#
# 用法：
#   bash scripts/retrigger-release.sh --dry-run v1.0.0        # 只查前提，不动任何东西
#   bash scripts/retrigger-release.sh --at HEAD v1.0.0        # 把 tag 指到 HEAD 后重推
#   bash scripts/retrigger-release.sh --at <sha> v1.0.0
#   bash scripts/retrigger-release.sh --at HEAD --assume-no-release v1.0.0
#
# 环境：GH_TOKEN 或 GITHUB_TOKEN（用于查 release 是否存在）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

AT="HEAD"
DRY=0
ASSUME_NO_RELEASE=0
TAG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --at) AT="${2:-HEAD}"; shift 2 ;;
    --assume-no-release) ASSUME_NO_RELEASE=1; shift ;;
    -h|--help) sed -n '2,16p' "${BASH_SOURCE[0]}"; exit 0 ;;
    v*) TAG="$1"; shift ;;
    *) die "未知参数：$1" ;;
  esac
done
[ -n "$TAG" ] || die "用法：bash scripts/retrigger-release.sh [--dry-run] [--at <ref>] <tag>"
case "$TAG" in v*) ;; *) die "tag 必须形如 vX.Y.Z，收到：$TAG" ;; esac

require_cmd git node curl
ROOT="$(repo_root)"
cd "$ROOT"
VERSION="${TAG#v}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || die "tag 版本段不合法：$VERSION"

refuse() { die "拒绝执行：$*"; }

# ── 1. 工作区必须干净且与远端同步 ──
[ -z "$(git status --porcelain)" ] || refuse "工作区有未提交改动（产物与源码不一致）"
log "拉取 origin 最新状态"
git fetch --quiet origin master || refuse "无法访问 origin（先确认网络与凭据）"

# ── 2. 目标提交必须在 origin/master 上 ──
TARGET="$(git rev-parse --verify "$AT^{commit}" 2>/dev/null)" || refuse "解析不到目标提交：$AT"
git merge-base --is-ancestor "$TARGET" origin/master || refuse "目标提交 $TARGET 不在 origin/master 上（不允许发布未合并提交）"
log "目标提交：$TARGET（$(git log -1 --pretty=%s "$TARGET")）"

# ── 3. 版本同源：目标提交的 package.json 版本必须等于 tag ──
PKG_VERSION="$(git show "$TARGET:package.json" | node -e '
let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
  try { process.stdout.write(JSON.parse(s).version ?? ""); } catch { process.stdout.write(""); }
});')"
[ "$PKG_VERSION" = "$VERSION" ] || refuse "tag $TAG 与目标提交的 package.json 版本（${PKG_VERSION:-无}）不一致"

# ── 4. 发布前置文件必须在目标提交里 ──
git cat-file -e "$TARGET:changelog/v$VERSION.json" 2>/dev/null \
  || refuse "目标提交缺少 changelog/v$VERSION.json（发行说明会退化成提交清单）"
git cat-file -e "$TARGET:.github/workflows/release.yml" 2>/dev/null \
  || refuse "目标提交缺少 .github/workflows/release.yml（推了也不会有构建）"

# ── 5. 该 tag 不能已有 release（含草稿）──
if [ "$ASSUME_NO_RELEASE" -eq 1 ]; then
  warn "按 --assume-no-release 跳过 release 存在性检查"
else
  release_state "$TAG"
  case "$?" in
    0) refuse "该 tag 已有 release（含草稿）：覆写已发布内容不可接受。改发新的 patch 版本，或先手动清理草稿" ;;
    1) log "已确认 $TAG 尚无 release" ;;
    *) refuse "无法判定 $TAG 是否已有 release（网络或凭据受限）。确认没有发布后，可显式加 --assume-no-release" ;;
  esac
fi

# ── 6. 本地 tag 与远端 tag 若同时存在且指向不同提交 → 版本双真相 ──
LOCAL_REF="$(git rev-parse --verify "refs/tags/$TAG" 2>/dev/null || true)"
REMOTE_REF="$(git ls-remote --tags origin "refs/tags/$TAG" 2>/dev/null | awk '{print $1}' | head -1 || true)"
if [ -n "$LOCAL_REF" ] && [ -n "$REMOTE_REF" ] && [ "$LOCAL_REF" != "$REMOTE_REF" ]; then
  refuse "本地 tag 与远端 tag 指向不同提交（$LOCAL_REF vs $REMOTE_REF）"
fi

if [ "$DRY" -eq 1 ]; then
  log "dry-run：前提全部满足，将执行——"
  printf '  git tag -d %s（若存在）\n  git tag -a %s %s -m "release: %s"\n  git push origin :refs/tags/%s\n  git push origin %s\n' \
    "$TAG" "$TAG" "$TARGET" "$TAG" "$TAG" "$TAG"
  exit 0
fi

# ── 7. 执行：删除同名 tag → 指到目标提交 → 推送 ──
[ -n "$LOCAL_REF" ] && git tag -d "$TAG" >/dev/null
git tag -a "$TAG" "$TARGET" -m "release: $TAG"
if [ -n "$REMOTE_REF" ]; then
  git push origin ":refs/tags/$TAG" || refuse "删除远端旧 tag 失败"
fi
git push origin "$TAG"
log "已重推 $TAG → $TARGET，等待流水线：gh run list --workflow=release.yml"
