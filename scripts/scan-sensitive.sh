#!/usr/bin/env bash
# 敏感信息扫描（发布门禁）
#
# 分级原则：一次把所有词都设成阻断级，结果是每条提交都被拦、最后人去放宽规则。
#   阻断级：凭据 / 私钥 / 个人身份 —— 命中即 exit 1，禁止发布
#   提示级：本地绝对路径、本地专有词表 —— 只提示，不拦
#
# 词表与扫描器分离：通用规则在本脚本内（可入库、可分发）；
# 组织专有敏感词放不入库的 .sensitive-terms（每行一个，'#' 开头为注释），本脚本自动读取。
#
# 用法：
#   bash scripts/scan-sensitive.sh                 # 只扫工作区受跟踪文件
#   bash scripts/scan-sensitive.sh --all-history   # 追加扫描全部历史对象（发布前必做）
#   bash scripts/scan-sensitive.sh --terms <文件>  # 指定本地词表

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

ALL_HISTORY=0
TERMS_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --all-history) ALL_HISTORY=1; shift ;;
    --terms)       TERMS_FILE="${2:-}"; shift 2 ;;
    -h|--help)     sed -n '2,20p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)             die "未知参数：$1" ;;
  esac
done

require_cmd git grep
ROOT="$(repo_root)"
cd "$ROOT"
[ -z "$TERMS_FILE" ] && TERMS_FILE="$ROOT/.sensitive-terms"

# ── 阻断级规则（名称|ERE）──
BLOCK_RULES=(
  '私钥文件|-----BEGIN [A-Z ]*PRIVATE KEY-----'
  'AWS Access Key|AKIA[0-9A-Z]{16}'
  'GitHub 令牌|gh[pousr]_[A-Za-z0-9]{36,}'
  'GitHub 细粒度令牌|github_pat_[A-Za-z0-9_]{20,}'
  '通用 sk- 密钥|sk-[A-Za-z0-9_-]{20,}'
  'Bearer 令牌|[Bb]earer[[:space:]]+[A-Za-z0-9._~+/-]{20,}=*'
  'JWT|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.'
  '配置里的非空 apiKey|"apiKey"[[:space:]]*:[[:space:]]*"[^"]{12,}"'
  '密钥环境变量落值|(OPENCODE_API_KEY|OPENAI_API_KEY|ANTHROPIC_API_KEY)[[:space:]]*=[[:space:]]*[^[:space:]"'"'"']{12,}'
)

# ── 提示级规则 ──
HINT_RULES=(
  '本地绝对路径|[A-Za-z]:\\'
)

block_hits=0
hint_hits=0

fail() { block_hits=$((block_hits + 1)); }

report_line() {  # 名称 文件 行号 行内容
  printf '  [%s] %s:%s\n      %s\n' "$1" "$2" "$3" "$(printf '%s' "$4" | cut -c1-160)"
}

# ── 1. 工作区受跟踪文件 ──
mapfile -t FILES < <(git ls-files)
if [ "${#FILES[@]}" -eq 0 ]; then
  die "没有任何受跟踪文件，扫描无意义"
fi
log "扫描工作区受跟踪文件：${#FILES[@]} 个"

for rule in "${BLOCK_RULES[@]}"; do
  name="${rule%%|*}"; ere="${rule#*|}"
  while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    file="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"; text="${rest#*:}"
    report_line "$name" "$file" "$line" "$text"
    fail
  done < <(grep -aEn -e "$ere" -- "${FILES[@]}" 2>/dev/null || true)
done

for rule in "${HINT_RULES[@]}"; do
  name="${rule%%|*}"; ere="${rule#*|}"
  while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    file="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"; text="${rest#*:}"
    report_line "$name" "$file" "$line" "$text"
    hint_hits=$((hint_hits + 1))
  done < <(grep -aEn -e "$ere" -- "${FILES[@]}" 2>/dev/null || true)
done

# ── 2. 个人邮箱（放行 noreply）──
while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  case "$hit" in *"users.noreply.github.com"*) continue ;; esac
  file="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"; text="${rest#*:}"
  report_line '个人邮箱（非 noreply）' "$file" "$line" "$text"
  fail
done < <(grep -aEn -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' -- "${FILES[@]}" 2>/dev/null || true)

# ── 3. 非白名单外部地址 ──
# 放行：本机地址、产品自身上游、代码托管、示例域名；模板串（host 运行时决定）不算命中
ALLOW_SUFFIXES="127.0.0.1 localhost opencode.ai github.com githubusercontent.com example.com"
while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  file="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"; text="${rest#*:}"
  flagged=""
  while IFS= read -r url; do
    [ -z "$url" ] && continue
    case "$url" in *'$'*) continue ;; esac
    host="${url#*://}"; host="${host%%/*}"; host="${host%%:*}"
    [ -z "$host" ] && continue
    allowed=0
    for suf in $ALLOW_SUFFIXES; do
      case "$host" in "$suf"|*."$suf") allowed=1 ;; esac
    done
    [ "$allowed" -eq 0 ] && flagged="$url"
  done < <(printf '%s' "$text" | grep -aoE 'https?://[^"'"'"' )>,;]+' || true)
  [ -z "$flagged" ] && continue
  report_line '非白名单外部地址' "$file" "$line" "$text"
  fail
done < <(grep -aEn -e 'https?://' -- "${FILES[@]}" 2>/dev/null || true)

# ── 4. 本地专有词表（提示级）──
if [ -f "$TERMS_FILE" ]; then
  log "读取本地词表：$TERMS_FILE"
  while IFS= read -r word; do
    case "$word" in ''|'#'*) continue ;; esac
    while IFS= read -r hit; do
      [ -z "$hit" ] && continue
      file="${hit%%:*}"; rest="${hit#*:}"; line="${rest%%:*}"; text="${rest#*:}"
      report_line "本地词表:${word}" "$file" "$line" "$text"
      hint_hits=$((hint_hits + 1))
    done < <(grep -aFEn -e "$word" -- "${FILES[@]}" 2>/dev/null || true)
  done < "$TERMS_FILE"
else
  warn "未找到本地词表 $TERMS_FILE（可选；组织专有敏感词应放这里，不入库）"
fi

# ── 5. 全历史对象 ──
if [ "$ALL_HISTORY" -eq 1 ]; then
  log "扫描全部历史对象（不只 HEAD 树）"
  combined="$(printf '%s\n' "${BLOCK_RULES[@]}" | cut -d'|' -f2- | paste -sd'|' -)"
  count=0
  while read -r oid otype osize; do
    [ "$otype" = "blob" ] || continue
    [ "${osize:-0}" -gt 1048576 ] && continue
    if git cat-file blob "$oid" 2>/dev/null | grep -aqE -e "$combined"; then
      printf '  [阻断级] 历史对象 %s（%s 字节）命中，需用 filter-repo 改写或用新提交覆盖后重扫\n' "$oid" "$osize"
      block_hits=$((block_hits + 1))
    fi
    count=$((count + 1))
  done < <(git cat-file --batch-all-objects --batch-check='%(objectname) %(objecttype) %(objectsize)' 2>/dev/null || true)
  log "历史 blob 扫描完成，共检查 $count 个对象"
fi

# ── 6. 结论 ──
echo
if [ "$block_hits" -gt 0 ]; then
  printf '[FAIL] 阻断级命中 %d 处 —— 禁止发布\n' "$block_hits" >&2
  exit 1
fi
printf '[OK] 阻断级零命中（提示级 %d 处，公开前可酌情清理）\n' "$hint_hits"
exit 0
