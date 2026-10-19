#!/usr/bin/env bash
# 打包发布资产（CI 与本地共用）
#
# 规则：
#   - 只打「显式列出的产品文件」（必达集合），且每个都必须是 git 受跟踪文件
#   - 绝不包含 .git、dist、node_modules 或任何未跟踪文件（本地手工打包曾把 .git 打进去）
#   - 条目名扁平（用 basename），不含目录层级
#   - 时间戳固定，产物可复现（同一份源码两次打包得到同一 SHA256）
#   - 输出名自解释：<产品>-v<版本>.zip
#
# 用法：
#   bash scripts/build-package.sh [--out-dir dist]
#   bash scripts/build-package.sh --list     # 只打印将入包的文件清单

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

PRODUCT="model-bridge"

# 必达集合：用户拿到 zip 就能跑起来的最小集合（Legacy Node 版，冻结维护）
# 打包时统一扁平化到 zip 根目录；模板改名为 bridge.config.json，避免把真实配置带入产物。
PRODUCT_FILES=(
  "legacy/node-bridge/model-bridge.js"
  "bridge.config.example.json"
  "legacy/node-bridge/start-bridge.cmd"
  "legacy/node-bridge/restart-bridge.cmd"
  "legacy/node-bridge/test-bridge.mjs"
  "legacy/node-bridge/probe-upstream.mjs"
  "README.md"
  ".gitignore"
)

# 源路径 -> zip 内条目名
zip_name() {
  case "$1" in
    bridge.config.example.json) printf 'bridge.config.json\n' ;;
    *) printf '%s\n' "${1##*/}" ;;
  esac
}

OUT_DIR="dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --out-dir) OUT_DIR="${2:-dist}"; shift 2 ;;
    --list)    for f in "${PRODUCT_FILES[@]}"; do zip_name "$f"; done; exit 0 ;;
    -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)         die "未知参数：$1" ;;
  esac
done

ROOT="$(repo_root)"
cd "$ROOT"

VERSION="$(pkg_version "$ROOT")"
TAG="v$VERSION"
ZIP="$OUT_DIR/$PRODUCT-$TAG.zip"

log "版本（legacy/node-bridge/package.json）：$VERSION"

# 必达文件必须存在且受跟踪
for f in "${PRODUCT_FILES[@]}"; do
  [ -f "$f" ] || die "缺少产品文件：$f"
  git ls-files --error-unmatch -- "$f" >/dev/null 2>&1 || die "产品文件未被 git 跟踪：$f"
done

mkdir -p "$OUT_DIR"
rm -f "$ZIP"

if command -v python3 >/dev/null 2>&1; then
  python3 - "$ZIP" "${PRODUCT_FILES[@]}" <<'PY'
import os
import sys
import zipfile

out = sys.argv[1]
files = sorted(sys.argv[2:])
FIXED_TIME = (1980, 1, 1, 0, 0, 0)

with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for path in files:
        if not os.path.isfile(path):
            sys.exit(f"缺少文件：{path}")
        name = os.path.basename(path)
        if name == "bridge.config.example.json":
            name = "bridge.config.json"
        info = zipfile.ZipInfo(filename=name, date_time=FIXED_TIME)
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        with open(path, "rb") as fh:
            z.writestr(info, fh.read())
PY
elif command -v zip >/dev/null 2>&1; then
  die "需要 python3 完成模板改名打包（zip 命令无法把 example 改名为 bridge.config.json）"
else
  die "需要 python3 来完成打包"
fi

[ -f "$ZIP" ] || die "打包失败：$ZIP 未生成"

log "已生成 $ZIP（$(wc -c < "$ZIP" | tr -d ' ') 字节）"
log "包内条目："
if command -v unzip >/dev/null 2>&1; then
  unzip -Z1 "$ZIP" | sed 's/^/  /'
elif command -v python3 >/dev/null 2>&1; then
  python3 -c 'import sys,zipfile;print("\n".join("  "+n for n in zipfile.ZipFile(sys.argv[1]).namelist()))' "$ZIP"
fi
