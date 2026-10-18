# -*- coding: utf-8 -*-
"""冷启动验收：对已打包的 dist/Model-Gate.exe 打真实请求（由 build/验收流程调用）。

用法：
    dist\\Model-Gate.exe --no-gui --port 8911     # 先拉起
    py -3.11 scripts\\coldstart_check.py           # 再跑本脚本
"""
from __future__ import annotations

import json
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8911"
FAILURES: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    mark = "PASS" if ok else "FAIL"
    print(f"[{mark}] {name}" + (f" — {detail}" if detail else ""))
    if not ok:
        FAILURES.append(name)


def get(path: str) -> tuple[int, str]:
    req = urllib.request.Request(BASE + path)
    try:
        with urllib.request.urlopen(req, timeout=20) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")


def post_json(path: str, payload: dict) -> tuple[int, str]:
    data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(BASE + path, data=data, method="POST",
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")


def main() -> int:
    # 1. 健康检查
    status, body = get("/health")
    check("/health 200", status == 200, f"HTTP {status}")
    try:
        health = json.loads(body)
        names = [u["name"] for u in health.get("upstreams", [])]
        check("/health 含 4 个上游", names == ["cch", "go", "zen", "commandcode"],
              ",".join(names))
    except Exception as exc:
        check("/health JSON 可解析", False, str(exc))
        names = []

    # 2. 模型清单
    status, body = get("/v1/models")
    check("/v1/models 200", status == 200, f"HTTP {status}")
    try:
        models = json.loads(body)
        ids = [m["id"] for m in models.get("data", [])]
        check("/v1/models 返回模型", bool(ids), f"{len(ids)} 个")
        check("/v1/models 含默认模型", "deepseek-flash" in ids, ",".join(ids[:5]))
    except Exception as exc:
        check("/v1/models JSON 可解析", False, str(exc))

    # 3. 面板静态资源（打包时必须随包带上）
    status, body = get("/ui/")
    check("/ui/ 返回 index.html", status == 200 and "Model-Gate" in body,
          f"HTTP {status}")
    status, body = get("/ui/app.js")
    check("/ui/app.js 返回 JS", status == 200 and "/api/usage/summary" in body,
          f"HTTP {status}")
    status, body = get("/ui/style.css")
    check("/ui/style.css 返回 CSS", status == 200 and "--brand" in body,
          f"HTTP {status}")

    # 4. 控制面 API
    status, body = get("/api/info")
    check("/api/info 200", status == 200, f"HTTP {status}")
    try:
        info = json.loads(body)
        check("/api/info 版本号", bool(info.get("version")), info.get("version", ""))
        check("/api/info 数据库路径在 data/", "modelgate.db" in (info.get("db_path") or ""),
              info.get("db_path", ""))
    except Exception as exc:
        check("/api/info JSON", False, str(exc))

    status, body = get("/api/config")
    check("/api/config 200", status == 200, f"HTTP {status}")
    check("/api/config 不泄漏密钥", status == 200 and "test-key" not in body
          and '"apiKey"' not in body, "明文 key 已脱敏")

    status, body = get("/api/usage/totals?range=24h")
    check("/api/usage/totals 200", status == 200, f"HTTP {status}")
    status, body = get("/api/usage/summary?group_by=model")
    check("/api/usage/summary 200", status == 200, f"HTTP {status}")
    status, body = get("/api/usage/list?limit=5")
    check("/api/usage/list 200", status == 200, f"HTTP {status}")

    # 5. 未知前缀 404（契约）
    status, body = get("/nope/v1/models")
    check("未知上游前缀 404", status == 404, f"HTTP {status}")
    try:
        err = json.loads(body).get("error", {})
        check("404 列出可用前缀", "/cch" in err.get("message", ""),
              err.get("code", ""))
    except Exception as exc:
        check("404 错误体可解析", False, str(exc))

    # 6. 未实现端点 404
    status, body = get("/v1/embeddings")
    check("未实现端点 404", status == 404, f"HTTP {status}")

    print()
    if FAILURES:
        print(f"COLDSTART FAILED: {len(FAILURES)} 项未通过")
        for name in FAILURES:
            print(f"  - {name}")
        return 1
    print("COLDSTART OK: 全部通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
