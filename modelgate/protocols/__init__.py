# -*- coding: utf-8 -*-
"""协议适配层注册表。

每种上游协议只实现三件事，主流程（proxy.handle_chat）不再出现协议分支：
  build_request(ctx, config)  → BuildResult（含 error 时主流程直接返回）
  parse_response(raw, ctx)    → {body, meta?} | {error}
  parse_stream(lines, w, ctx) → usage dict（流式；自行写响应并结束）
"""
from __future__ import annotations

from .anthropic import AnthropicAdapter
from .base import BuildResult, Ctx, ProtocolError, SseWriter
from .commandcode import CommandCodeAdapter
from .openai_cc import OpenAIAdapter, SessionRegistry

__all__ = [
    "AnthropicAdapter",
    "BuildResult",
    "CommandCodeAdapter",
    "Ctx",
    "OpenAIAdapter",
    "ProtocolError",
    "SessionRegistry",
    "SseWriter",
    "build_registry",
]


def build_registry(*, thinking_cache, downgrade_forced_choice: bool = True,
                   sessions: SessionRegistry | None = None,
                   post_lines=None) -> dict[str, object]:
    """构造 协议名 → 适配器 的注册表；未登记的协议按 openai 处理（与 Node 版一致）。"""
    sessions = sessions or SessionRegistry()
    return {
        "anthropic": AnthropicAdapter(
            thinking_cache, downgrade_forced_choice=downgrade_forced_choice),
        "openai": OpenAIAdapter(sessions),
        "commandcode": CommandCodeAdapter(sessions, post_lines=post_lines),
    }
