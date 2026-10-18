# -*- coding: utf-8 -*-
"""thinking 回传缓存（Anthropic 协议专用）。

DeepSeek 思考模式要求多轮把上一轮的思考内容原样传回，否则思考链断裂。
Agent 通常不会回传该字段，故本模块缓存上游返回的 thinking + signature，
在后续轮次的 assistant 消息中自动补回（位于消息首位）；
客户端若自带 reasoning_content 则以其为准。
开关：bridge.config.json 的 thinking.passthrough（默认 true）/ thinking.cacheSize。
"""
from __future__ import annotations

import hashlib
import threading


def _fingerprint(text: str) -> str:
    """助手回复文本指纹（规范化空白后取摘要，容忍客户端的轻微改写）。"""
    normalized = " ".join(str(text or "").split())
    return hashlib.sha1(normalized.encode("utf-8")).hexdigest()[:16]


class ThinkingCache:
    """tool_call id → 思考内容；assistant 文本指纹 → 思考内容。线程安全。"""

    def __init__(self, enabled: bool = True, max_size: int = 500) -> None:
        self.enabled = enabled
        self.max_size = int(max_size or 500)
        self._by_tool_id: dict[str, dict] = {}
        self._by_text: dict[str, dict] = {}
        self._lock = threading.Lock()

    def _evict(self, mapping: dict) -> None:
        while len(mapping) > self.max_size:
            first = next(iter(mapping))
            del mapping[first]

    def remember(self, thinking: str, signature: str, text: str, tool_ids) -> None:
        """记住一轮思考内容，供后续轮次补回。"""
        if not thinking:
            return
        entry = {"thinking": thinking, "signature": signature or ""}
        with self._lock:
            for tid in tool_ids or []:
                if tid:
                    self._by_tool_id[tid] = entry
            if text:
                self._by_text[_fingerprint(text)] = entry
            self._evict(self._by_tool_id)
            self._evict(self._by_text)

    def recall(self, text: str, tool_ids) -> dict | None:
        """取回思考内容：先按 tool_call id 命中，再按文本指纹兜底。"""
        with self._lock:
            for tid in tool_ids or []:
                hit = self._by_tool_id.get(tid)
                if hit:
                    return hit
            if text:
                return self._by_text.get(_fingerprint(text))
        return None

    def build_block(self, message: dict, text: str, tool_ids) -> dict | None:
        """构造要回传给上游的 thinking 块；取不到则返回 None。

        客户端显式带了思考内容时以它为准（不受 passthrough 开关影响）。
        """
        explicit = message.get("reasoning_content") or message.get("thinking")
        if isinstance(explicit, str) and explicit.strip():
            block = {"type": "thinking", "thinking": explicit}
            sig = message.get("reasoning_signature") or message.get("signature")
            if sig:
                block["signature"] = sig
            return block
        if not self.enabled:
            return None
        hit = self.recall(text, tool_ids)
        if not hit:
            return None
        block = {"type": "thinking", "thinking": hit["thinking"]}
        if hit.get("signature"):
            block["signature"] = hit["signature"]
        return block

    def clear(self) -> None:
        with self._lock:
            self._by_tool_id.clear()
            self._by_text.clear()

    def stats(self) -> dict:
        with self._lock:
            return {
                "enabled": self.enabled,
                "max_size": self.max_size,
                "by_tool_id": len(self._by_tool_id),
                "by_text": len(self._by_text),
            }
