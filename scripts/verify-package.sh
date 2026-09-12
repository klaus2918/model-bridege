#!/usr/bin/env bash
# 产物自检：把一个发布 zip 当被测对象（静态清单 + 动态启动）
#
# 比只跑单元测试更能挡住「包本身有问题」的事故：用真实产物跑一遍用户路径。
#
# 断言：
#   1. 条目集合恰好等于必达集合（多一个少一个都失败），条目扁平、不含 .git
#   2. 解包后每个 .js/.mjs 通过 node --check 语法校验
#   3. 包内 bridge.config.json 与仓库当前内容一致（证明产物来自本次源码）
#   4. 用包内真实代码启动一次服务，/health 与 /<上游>/v1/models 返回 200
#
# 用法：
#   bash scripts/verify-package.sh <zip> [--keep-tmp]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -ge 1 ] || die "用法：bash scripts/verify-package.sh <zip> [--keep-tmp]"
ZIP="$1"; shift || true
KEEP_TMP=0
[ "${1:-}" = "--keep-tmp" ] && KEEP_TMP=1

require_cmd node git
[ -f "$ZIP" ] || die "找不到产物：$ZIP"

ROOT="$(repo_root)"
TMP="$(mktemp -d)"
cleanup() { [ "$KEEP_TMP" -eq 1 ] || rm -rf "$TMP" 2>/dev/null || true; }
trap cleanup EXIT

fail=0
ok()   { printf '  [ok]   %s\n' "$*"; }
bad()  { printf '  [fail] %s\n' "$*" >&2; fail=$((fail + 1)); }

# ── 1. 条目集合 ──
mapfile -t EXPECTED < <(bash "$SCRIPT_DIR/build-package.sh" --list | sort)
mapfile -t ACTUAL < <(python3 -c '
import sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as z:
    for n in z.namelist():
        sys.stdout.write(n + "\n")
' "$ZIP" | tr -d '\r' | sort)

log "包内条目 ${#ACTUAL[@]} 个，期望 ${#EXPECTED[@]} 个"
diff_out="$(diff <(printf '%s\n' "${EXPECTED[@]}") <(printf '%s\n' "${ACTUAL[@]}") || true)"
if [ -n "$diff_out" ]; then
  bad "条目集合与必达集合不一致："
  printf '%s\n' "$diff_out" | sed 's/^/      /'
else
  ok "条目集合恰好等于必达集合"
fi

for n in "${ACTUAL[@]}"; do
  case "$n" in
    */*)      bad "条目含目录层级：$n" ;;
    .git|.git/*) bad "条目命中 .git 目录：$n" ;;
  esac
done

# ── 2. 解包 ──
mkdir -p "$TMP/pkg"
python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$ZIP" "$TMP/pkg"

mapfile -t JSF < <(cd "$TMP/pkg" && ls -1 | grep -E '\.(js|mjs)$' || true)
for f in "${JSF[@]}"; do
  if (cd "$TMP/pkg" && node --check "$f" >/dev/null 2>&1); then
    ok "语法校验通过：$f"
  else
    bad "语法校验失败：$f"
  fi
done

# ── 3. 配置与仓库一致 ──
if cmp -s "$TMP/pkg/bridge.config.json" "$ROOT/bridge.config.json"; then
  ok "bridge.config.json 与仓库一致"
else
  bad "bridge.config.json 与仓库不一致（产物不是本次源码打出来的？）"
fi

# ── 4. 用包内真实代码启动 ──
require_cmd curl
PORT=$(( 20000 + RANDOM % 20000 ))
cat > "$TMP/stub.config.json" <<JSON
{
  "port": $PORT,
  "host": "127.0.0.1",
  "accessLog": false,
  "envFile": false,
  "upstreams": {
    "stub": {
      "protocol": "openai",
      "baseUrl": "http://127.0.0.1:9/v1",
      "apiKey": "",
      "fetchModels": false,
      "autoModels": false,
      "models": ["stub-model"]
    }
  }
}
JSON

( cd "$TMP/pkg" && node model-bridge.js --config "$TMP/stub.config.json" > "$TMP/server.log" 2>&1 ) &
SRV=$!
stop_server() { kill "$SRV" 2>/dev/null || true; wait "$SRV" 2>/dev/null || true; sleep 1; }
trap 'stop_server; cleanup' EXIT

code=""
for _ in $(seq 1 40); do
  code="$(curl -s -o "$TMP/health.json" -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null || true)"
  [ "$code" = "200" ] && break
  sleep 0.5
done

if [ "$code" = "200" ]; then
  ok "启动成功：GET /health -> 200"
else
  bad "启动失败：GET /health -> ${code:-无响应}"
  sed 's/^/      /' "$TMP/server.log" || true
fi

if grep -q '"stub"' "$TMP/health.json" 2>/dev/null; then
  ok "/health 列出了 stub 上游"
else
  bad "/health 未列出 stub 上游"
fi

code2="$(curl -s -o "$TMP/models.json" -w '%{http_code}' "http://127.0.0.1:$PORT/stub/v1/models" 2>/dev/null || true)"
if [ "$code2" = "200" ] && grep -q 'stub-model' "$TMP/models.json"; then
  ok "前缀分发可用：GET /stub/v1/models -> 200 且声明了 stub-model"
else
  bad "前缀分发异常：GET /stub/v1/models -> ${code2:-无响应}"
fi

stop_server

echo
if [ "$fail" -gt 0 ]; then
  printf '[FAIL] 产物自检失败 %d 项\n' "$fail" >&2
  exit 1
fi
printf '[OK] 产物自检通过：%s\n' "$ZIP"
