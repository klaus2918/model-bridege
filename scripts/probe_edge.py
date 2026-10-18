# -*- coding: utf-8 -*-
"""边界请求探针：用畸形/极端请求体逼出返回 500 的代码路径。

用法：py -3.11 scripts\\probe_edge.py http://127.0.0.1:8912
"""
from __future__ import annotations

import json
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8912"
FIVES: list[str] = []


def post(path: str, payload, as_text: bool = False) -> tuple[int, str]:
    # as_text 表示「直接用原始载荷」；但仍要保证最终是 bytes
    if as_text and not isinstance(payload, (str, bytes, bytearray)):
        raw = json.dumps(payload, ensure_ascii=False)
    elif as_text:
        raw = payload
    else:
        raw = json.dumps(payload, ensure_ascii=False)
    if isinstance(raw, str):
        raw = raw.encode("utf-8")
    req = urllib.request.Request(
        BASE + path, data=raw, method="POST",
        headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=25) as res:
            return res.status, res.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except Exception as exc:  # noqa: BLE001
        return -1, f"{type(exc).__name__}: {exc}"


def label(path: str, tag: str, payload, as_text: bool = False) -> None:
    status, body = post(path, payload, as_text)
    # 只有 500 / internal_error 算缺陷；502/503 是上游不可达的正确语义
    internal = status == 500 or "internal_error" in body
    if internal:
        FIVES.append(f"{tag} -> {status} {body[:400]}")
    mark = "500 " if internal else "OK  "
    if status == -1:
        mark = "WARN"
    print(f"[{mark}{status}] {tag}  {body.replace(chr(10), ' ')[:180]}")


CHAT = "/v1/chat/completions"
PREFIX = "/cch/v1/chat/completions"   # 强制 anthropic 上游


def main() -> int:
    print(f"probing {BASE}\n")
    base = {"model": "deepseek-flash"}

    cases = [
        ("messages 是字符串数组", {**base, "messages": ["hi"]}),
        ("messages 含 null", {**base, "messages": [None]}),
        ("messages 含空对象", {**base, "messages": [{}]}),
        ("assistant 无 content", {**base, "messages": [{"role": "assistant"}]}),
        ("assistant tool_calls 含 null",
         {**base, "messages": [{"role": "assistant", "tool_calls": [None]}]}),
        ("tool 消息缺 tool_call_id", {**base, "messages": [
            {"role": "tool", "content": "x"}]}),
        ("tools 含 null", {**base, "messages": [{"role": "user", "content": "hi"}],
                           "tools": [None]}),
        ("tool_choice 指名但无 tools",
         {**base, "messages": [{"role": "user", "content": "hi"}],
          "tool_choice": {"type": "function", "function": {"name": "f"}}}),
        ("model 是数字", {"model": 123, "messages": [{"role": "user", "content": "hi"}]}),
        ("stream 非布尔", {**base, "stream": "maybe",
                           "messages": [{"role": "user", "content": "hi"}]}),
        ("temperature 是字符串",
         {**base, "temperature": "hot", "messages": [{"role": "user", "content": "hi"}]}),
        ("无 messages 字段", {**base}),
        ("messages 为空数组", {**base, "messages": []}),
        ("content 是数字", {**base, "messages": [{"role": "user", "content": 42}]}),
        ("content 是对象数组含 image_url 空",
         {**base, "messages": [{"role": "user", "content": [
             {"type": "image_url", "image_url": None}]}]}),
        ("max_tokens 是字符串",
         {**base, "max_tokens": "8k", "messages": [{"role": "user", "content": "hi"}]}),
        ("stop 是空数组", {**base, "stop": [], "messages": [
            {"role": "user", "content": "hi"}]}),
        ("别名模型", {"model": "alias-not-exist",
                      "messages": [{"role": "user", "content": "hi"}]}),
    ]
    for tag, payload in cases:
        label(PREFIX, tag, payload)

    # 非对象请求体
    for tag, payload in [
        ("顶层是数组", [{"model": "x", "messages": []}]),
        ("顶层是字符串", '"just a string"'),
        ("顶层是 null", "null"),
        ("空对象", {}),
        ("非法 JSON", "{not json"),
        ("截断 JSON", '{"model": "x", "messages": ['),
        ("无效 UTF-8 字节", b"\xff\xfe\x00\x01"),
    ]:
        label(PREFIX, tag, payload, as_text=True)

    # 其它协议前缀同样跑一遍最易崩的两条
    for prefix in ("/go", "/zen", "/commandcode"):
        for tag, payload in [("非 dict 消息", {**base, "messages": ["hi"]}),
                             ("缺 messages", {**base})]:
            label(f"{prefix}/v1/chat/completions", f"{prefix} {tag}", payload)

    print()
    if FIVES:
        print(f"FOUND_5XX={len(FIVES)}")
        for line in FIVES:
            print("  " + line)
        return 1
    print("NO_5XX")
    return 0


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    raise SystemExit(main())
