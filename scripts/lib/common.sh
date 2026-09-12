#!/usr/bin/env bash
# 公共函数库：由 scripts/ 下各脚本 source。
# 兼容两端：GitHub Actions ubuntu-latest 与本机 Git Bash（Windows）。
# 依赖：git、node 必装；python3 / sha256sum / curl 按脚本需要各自 require。

set -euo pipefail

log()  { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
warn() { printf '[warn] %s\n' "$*" >&2; }
die()  { printf '[error] %s\n' "$*" >&2; exit 1; }

require_cmd() {
  local c
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || die "缺少命令：$c"
  done
}

# 仓库根目录（脚本可被任意 cwd 调用）
repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || die "当前目录不在 git 仓库内"
}

# 版本唯一真源：package.json 的 version
pkg_version() {
  local root="${1:-$(repo_root)}"
  node -e '
    const fs = require("node:fs");
    const j = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (!j.version) { console.error("package.json 缺少 version"); process.exit(2); }
    process.stdout.write(j.version);
  ' "$root/package.json"
}

# 有 timeout 就用，没有（部分 Git Bash）就直接跑
with_timeout() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  else
    "$@"
  fi
}

# GitHub 令牌回退链：GH_TOKEN → GITHUB_TOKEN → git credential fill（带超时，匿名 API 有限流）
resolve_token() {
  local host="${1:-github.com}"
  if [ -n "${GH_TOKEN:-}" ];      then printf '%s' "$GH_TOKEN";      return 0; fi
  if [ -n "${GITHUB_TOKEN:-}" ];  then printf '%s' "$GITHUB_TOKEN";  return 0; fi
  command -v git >/dev/null 2>&1 || return 1
  printf 'protocol=https\nhost=%s\n\n' "$host" \
    | with_timeout 10 git credential fill 2>/dev/null \
    | sed -n 's/^password=//p' | head -1
}

# 取远端 URL（用于从 git 配置推导 owner/repo）
remote_url() {
  git config --get remote.origin.url 2>/dev/null || true
}

# owner/repo（从 origin remote 推导，支持 https 与 ssh 两种写法）
repo_slug() {
  remote_url | sed -E 's#(git@|https://|http://)github\.com[:/]##; s#\.git$##'
}

have_gh() { command -v gh >/dev/null 2>&1; }

# Windows 的 curl 走 schannel 时 CRL 检查可能失败（CRYPT_E_NO_REVOCATION_CHECK），
# MSYS/MinGW 下加 --ssl-no-revoke 规避；Linux 的 curl 没有该选项，故按平台判断。
curl_ssl_opts() {
  case "$(uname -s 2>/dev/null)" in
    MINGW*|MSYS*|CYGWIN*) printf '%s' "--ssl-no-revoke" ;;
  esac
}

# GitHub REST GET：stdout 输出 body
# 返回：0=200  1=404  2=其它（网络/凭据问题，调用方必须按「无法判定」处理）
gh_api_get() {
  local path="$1"
  command -v curl >/dev/null 2>&1 || return 2
  local token; token="$(resolve_token || true)"
  local ssl; ssl="$(curl_ssl_opts)"
  local -a args=(-sS -w '\n%{http_code}' -H 'Accept: application/vnd.github+json')
  [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
  local resp code
  resp="$(curl $ssl "${args[@]}" "https://api.github.com/$path" 2>/dev/null)" || return 2
  code="${resp##*$'\n'}"
  printf '%s' "${resp%$'\n'*}"
  case "$code" in
    200) return 0 ;;
    404) return 1 ;;
    *)   return 2 ;;
  esac
}

# 取 release JSON（含草稿）。
# 注意：/releases/tags/{tag} 对 draft 返回 404（GitHub 行为），必须退化为列表匹配；
# 且列表接口对草稿只在带令牌时可见，无令牌时无法判定 → 返回 2。
# 返回：0=找到（stdout 输出 JSON） 1=不存在 2=无法判定
release_json() {
  local tag="$1" body rc
  # 注意：本脚本开了 set -e，直接写 body="$(...)"; rc=$? 会在命令失败时静默退出，
  # 必须放在 if/|| 里捕获退出码。
  if body="$(gh_api_get "repos/$(repo_slug)/releases/tags/$tag")"; then
    rc=0
  else
    rc=$?
  fi
  if [ "$rc" -eq 0 ]; then
    printf '%s' "$body"
    return 0
  fi
  [ "$rc" -eq 1 ] || return 2
  local token; token="$(resolve_token || true)"
  [ -n "$token" ] || return 2
  body="$(gh_api_get "repos/$(repo_slug)/releases?per_page=100")" || return 2
  printf '%s' "$body" | node -e '
    let s = "";
    process.stdin.on("data", (d) => (s += d)).on("end", () => {
      const tag = process.argv[1];
      let j;
      try { j = JSON.parse(s); } catch { process.exit(2); }
      if (!Array.isArray(j)) process.exit(2);
      const hit = j.find((r) => r.tag_name === tag);
      if (!hit) process.exit(1);
      process.stdout.write(JSON.stringify(hit));
    });
  ' "$tag"
}

# release 是否存在（含草稿）：0=存在 1=不存在 2=无法判定（无法判定时调用方必须拒绝放行）
release_state() {
  if release_json "$1" >/dev/null; then
    return 0
  else
    return $?
  fi
}

# 下载 release 资产（gh 优先，退化到 browser_download_url + curl）
download_release_assets() {
  local tag="$1" dir="$2"
  mkdir -p "$dir"
  if have_gh && [ -n "${GH_TOKEN:-}" ]; then
    gh release download "$tag" --dir "$dir" --clobber >/dev/null 2>&1 && return 0
  fi
  local slug; slug="$(repo_slug)"
  local json
  json="$(gh_api_get "repos/$slug/releases/tags/$tag")" || return 1
  printf '%s' "$json" | node -e '
    let s = "";
    process.stdin.on("data", (d) => (s += d)).on("end", () => {
      const rel = JSON.parse(s);
      for (const a of rel.assets ?? []) console.log(`${a.name}\t${a.browser_download_url}`);
    });
  ' > "$dir/.assets.tsv" || return 1
  local token; token="$(resolve_token || true)"
  local ssl; ssl="$(curl_ssl_opts)"
  local name url
  while IFS=$'\t' read -r name url; do
    [ -z "$name" ] && continue
    if [ -n "$token" ]; then
      curl $ssl -sSL -H "Authorization: Bearer $token" -o "$dir/$name" "$url" || return 1
    else
      curl $ssl -sSL -o "$dir/$name" "$url" || return 1
    fi
  done < "$dir/.assets.tsv"
  rm -f "$dir/.assets.tsv"
  return 0
}
