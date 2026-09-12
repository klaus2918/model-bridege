#!/usr/bin/env bash
# 生成 SHA256SUMS（CI 与本地共用同一实现 —— 口径只有一处，不会出现「CI 生成的与本地生成的不一样」）
#
# 三条口径：
#   1. 覆盖范围 = 本次实际发布的资产，而不是构建目录里的全量文件
#   2. manifest 自身不列入
#   3. 格式固定 <sha256>␠␠<filename>（两个空格，sha256sum -c 可直接吃），按文件名排序
#
# 用法：
#   bash scripts/generate_checksums.sh <资产目录> <输出文件> [扩展名 ...]
# 例：
#   bash scripts/generate_checksums.sh dist SHA256SUMS .zip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -ge 2 ] || die "用法：bash scripts/generate_checksums.sh <资产目录> <输出文件> [扩展名 ...]"
DIR="$1"; OUT="$2"; shift 2
EXTS=("$@")
[ "${#EXTS[@]}" -eq 0 ] && EXTS=(".zip" ".tar.gz" ".exe")

require_cmd sha256sum
[ -d "$DIR" ] || die "资产目录不存在：$DIR"

# 收集候选（按文件名排序），按扩展名过滤，去重
MATCHED=()
while IFS= read -r f; do
  for e in "${EXTS[@]}"; do
    case "$f" in
      *"$e")
        dup=0
        for m in "${MATCHED[@]:-}"; do [ "$m" = "$f" ] && dup=1; done
        [ "$dup" -eq 0 ] && MATCHED+=("$f")
        ;;
    esac
  done
done < <(cd "$DIR" && ls -1 | LC_ALL=C sort)

[ "${#MATCHED[@]}" -gt 0 ] || die "目录 $DIR 下没有匹配的发布资产（扩展名：${EXTS[*]}）"

# 固定两空格格式（sha256sum -c 可直接吃）；Git Bash 下 sha256sum 默认二进制模式会输出 " *"
( cd "$DIR" && { sha256sum -t "${MATCHED[@]}" 2>/dev/null || sha256sum "${MATCHED[@]}"; } ) \
  | sed 's/ \*/  /' > "$OUT"

log "已生成 $OUT（${#MATCHED[@]} 条）："
cat "$OUT"
