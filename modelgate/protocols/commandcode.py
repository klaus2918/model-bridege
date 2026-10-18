# -*- coding: utf-8 -*-
"""commandcode 协议适配：POST /alpha/generate，NDJSON 事件并集 → OpenAI。

契约依据：.op/archive/2026/09/commandcode-upstream/reference/cc-wire-contract.md
- 上游恒为流式 NDJSON（每行一个 JSON 事件，不是 SSE），事件名多套命名，按并集识别
- pause_turn：用同一份 body 重发续上（与 CLI 的 n=0..Ph 循环一致，上限 5 次）
- x-cmd-zdr: 1 表达零数据留存要求（默认开）
- 非流式下游由本桥本地聚合同一套事件
"""
from __future__ import annotations

import json
import os
import platform
import time
import uuid
from datetime import date
from pathlib import Path
from typing import Callable, Iterable, Optional

from ..config import CC_PLAN_LABEL, CC_PLAN_RANK, command_code_roots, expand_home
from ..util import access_log_enabled, log, safe_json, text_of
from .base import BuildResult, Ctx, ProtocolError, SseWriter, iter_ndjson
from .openai_cc import SessionRegistry

CC_MAX_TOKENS = 64000        # 上游缺省 max_tokens
CC_PAUSE_TURN_MAX = 5        # 续跑上限（与 CLI 的 Ph 一致：最多续 5 次，共 6 次请求）
CC_VERSION_CACHE_MS = 30 * 60 * 1000


class CommandCodeAdapter:
    protocol = "commandcode"

    def __init__(self, sessions: SessionRegistry | None = None,
                 post_lines: Optional[Callable[..., Iterable[str]]] = None) -> None:
        self.sessions = sessions or SessionRegistry()
        # post_lines(url, headers, payload, timeout) → 文本行迭代器（供 pause_turn 续跑）
        self.post_lines = post_lines
        self._thread_ids: dict[str, str] = {}
        self._cc_version_at = 0.0
        self._cc_version_value = ""

    # ── 标识 ─────────────────────────────────────────────────────────────
    def thread_id_for(self, body: dict) -> str:
        """会话级复用的 threadId：必须是合法 UUID，否则上游会丢弃该字段。"""
        key = self.sessions.seed(body)
        hit = self._thread_ids.get(key)
        if hit:
            return hit
        value = str(uuid.uuid4())
        while len(self._thread_ids) > 2000:
            del self._thread_ids[next(iter(self._thread_ids))]
        self._thread_ids[key] = value
        return value

    def cc_session_id(self, body: dict) -> str:
        """CommandCode 的会话标识：sess_ + 稳定 UUID 去横线后前 16 位（与上游 CLI 同构）。"""
        return f"sess_{self.sessions.get(body).replace('-', '')[:16]}"

    # ── 请求转换 ─────────────────────────────────────────────────────────
    @staticmethod
    def to_messages(messages: list[dict]) -> list[dict]:
        """下游 OpenAI 消息 → CommandCode wire 消息（assistant / tool / user 三类）。"""
        out: list[dict] = []
        name_by_id: dict[str, str] = {}
        for m in messages or []:
            role = (m or {}).get("role")
            if role in ("system", "developer"):
                # system 由 build_request 单独归集到 params.system，这里不重复下发
                continue
            if role == "assistant":
                blocks: list[dict] = []
                reasoning = m.get("reasoning_content") or m.get("thinking")
                if isinstance(reasoning, str) and reasoning.strip():
                    blocks.append({"type": "reasoning", "text": reasoning})
                text = text_of(m.get("content"))
                if text:
                    blocks.append({"type": "text", "text": text})
                for tc in m.get("tool_calls") or []:
                    name = (tc.get("function") or {}).get("name") or "unknown"
                    if tc.get("id"):
                        name_by_id[tc["id"]] = name
                    blocks.append({
                        "type": "tool-call",
                        "toolCallId": tc.get("id"),
                        "toolName": name,
                        "input": safe_json((tc.get("function") or {}).get("arguments")),
                    })
                if blocks:
                    out.append({"role": "assistant", "content": blocks})
                continue
            if role == "tool":
                out.append({
                    "role": "tool",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": m.get("tool_call_id"),
                        "toolName": name_by_id.get(m.get("tool_call_id")) or "unknown",
                        "output": {"type": "text", "value": text_of(m.get("content"))},
                    }],
                })
                continue
            # user：文本与图片可同条
            parts: list[dict] = []
            content = m.get("content")
            if isinstance(content, str):
                if content:
                    parts.append({"type": "text", "text": content})
            elif isinstance(content, list):
                for p in content:
                    if isinstance(p, str):
                        if p:
                            parts.append({"type": "text", "text": p})
                    elif isinstance(p, dict) and p.get("type") == "text":
                        if p.get("text"):
                            parts.append({"type": "text", "text": p["text"]})
                    elif isinstance(p, dict) and p.get("type") == "image_url":
                        image = p.get("image_url")
                        url = image if isinstance(image, str) else (image or {}).get("url")
                        mime = ""
                        if isinstance(url, str) and url.startswith("data:"):
                            head = url[5:]
                            cut = max(head.find(","), head.find(";"))
                            mime = head[:cut] if cut > 0 else ""
                        if url and mime:
                            parts.append({"type": "image", "image": url, "mimeType": mime})
            if parts:
                out.append({"role": "user", "content": parts})
        return out

    @staticmethod
    def to_tools(tools) -> list[dict]:
        return [
            {
                "name": t["function"]["name"],
                "description": t["function"].get("description") or "",
                "input_schema": t["function"].get("parameters")
                or {"type": "object", "properties": {}},
            }
            for t in (tools or [])
            if (t or {}).get("type") == "function" and ((t.get("function") or {}).get("name"))
        ]

    @staticmethod
    def to_system(parts: list[str]) -> list[dict]:
        """系统提示 → wire 分段数组（非末段补换行，与上游一致）。"""
        texts = [p for p in parts if isinstance(p, str) and p]
        return [
            {"type": "text", "text": t if i == len(texts) - 1 else f"{t}\n"}
            for i, t in enumerate(texts)
        ]

    @staticmethod
    def wire_config() -> dict:
        """上传的 config 元数据（服务端不校验其内容，按上游形态自造）。"""
        return {
            "workingDir": str(Path.cwd()),
            "date": date.today().isoformat(),
            "environment": platform.system().lower(),
            "structure": [],
            "isGitRepo": False,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        }

    # ── 事件并集解析 ─────────────────────────────────────────────────────
    class _ToolIndex:
        """tool 事件的 id → 下游 tool_calls index 映射（流式与非流式共用）。"""

        def __init__(self) -> None:
            self._next = -1
            self._by_id: dict[str, int] = {}

        def ensure(self, tool_id, name, on_start) -> int:
            if tool_id and tool_id in self._by_id:
                return self._by_id[tool_id]
            self._next += 1
            if tool_id:
                self._by_id[tool_id] = self._next
            if on_start:
                on_start(self._next, tool_id, name)
            return self._next

        def has(self, tool_id) -> bool:
            return bool(tool_id) and tool_id in self._by_id

    @staticmethod
    def stop_reason(ev: dict) -> str:
        raw = str(ev.get("finishReason") or ev.get("rawFinishReason") or "").lower()
        if raw in ("tool_use", "tool-calls", "tool_calls"):
            return "tool_calls"
        if raw in ("length", "max_tokens", "max_output_tokens", "model_context_window_exceeded"):
            return "length"
        return "stop"

    @staticmethod
    def usage_of(ev: dict) -> dict:
        u = ev.get("totalUsage") or ev.get("usage") or {}
        detail = u.get("inputTokenDetails") or {}
        return {
            "prompt_tokens": int(u.get("inputTokens") or 0),
            "completion_tokens": int(u.get("outputTokens") or 0),
            "cacheReadTokens": int(detail.get("cacheReadTokens") or 0),
            "cacheWriteTokens": int(detail.get("cacheWriteTokens") or 0),
            "systemPromptTokens": int(ev.get("systemPromptTokens") or 0),
        }

    @staticmethod
    def error_message(ev: dict) -> str:
        err = ev.get("error")
        if isinstance(err, str) and err:
            return err
        if isinstance(err, dict):
            return err.get("message") or f"上游返回错误（statusCode={err.get('statusCode', '?')}）"
        return "上游返回未知错误"

    def make_sink(self, handlers: dict):
        """把 CommandCode 事件归一成语义回调（流式与非流式共用同一套解析）。"""
        tools = self._ToolIndex()

        def sink(ev: dict) -> None:
            etype = (ev or {}).get("type")
            if etype == "text-delta":
                if handlers.get("text"):
                    handlers["text"](ev.get("text") or "")
                return
            if etype == "reasoning-delta":
                if handlers.get("reasoning"):
                    handlers["reasoning"](ev.get("text") or "")
                return
            if etype == "tool-input-start":
                tools.ensure(ev.get("id") or ev.get("toolCallId"), ev.get("toolName"),
                             handlers.get("toolStart"))
                return
            if etype == "tool-input-delta":
                idx = tools.ensure(ev.get("id") or ev.get("toolCallId"), ev.get("toolName"),
                                   handlers.get("toolStart"))
                if handlers.get("toolInput"):
                    handlers["toolInput"](idx, ev.get("delta") or "")
                return
            if etype == "tool-delta":
                idx = tools.ensure(ev.get("toolCallId") or ev.get("id"), ev.get("toolName"),
                                   handlers.get("toolStart"))
                if handlers.get("toolInput"):
                    handlers["toolInput"](idx, ev.get("delta") or ev.get("partial") or "")
                return
            if etype in ("tool-call", "tool-use"):
                tool_id = ev.get("toolCallId") or ev.get("id")
                known = tools.has(tool_id)
                idx = tools.ensure(tool_id, ev.get("toolName"), handlers.get("toolStart"))
                if handlers.get("toolCall"):
                    handlers["toolCall"](idx, tool_id, ev.get("toolName"),
                                         ev.get("input") or ev.get("args") or {}, known)
                return
            if etype == "finish":
                # 原始终态一并透出：调用方据此决定是否续跑（pause_turn）
                raw = str(ev.get("rawFinishReason") or ev.get("finishReason") or "").lower()
                if handlers.get("finish"):
                    handlers["finish"](self.stop_reason(ev), self.usage_of(ev), raw)
                return
            if etype == "error":
                if handlers.get("error"):
                    handlers["error"](self.error_message(ev))
                return
            # tool-result / provider-metadata / abort 及一切未知事件：对 OpenAI 下游无语义，忽略

        return sink

    @staticmethod
    def consume(lines: Iterable[str], sink) -> bool:
        """逐行读 NDJSON 并喂给 sink；返回是否见到过 finish 事件。"""
        got_finish = False
        for ev in iter_ndjson(lines):
            if ev.get("type") == "finish":
                got_finish = True
            sink(ev)
        return got_finish

    # ── 凭据与版本 ───────────────────────────────────────────────────────
    def cli_version(self, up: dict) -> str:
        """发往上游的 CLI 版本号：配置值 → 本机安装版本 → npm registry（30 分钟缓存）。"""
        if up.get("cliVersion") and up.get("cliVersion") != "auto":
            return up["cliVersion"]
        now = time.time() * 1000
        if self._cc_version_value and now - self._cc_version_at < CC_VERSION_CACHE_MS:
            return self._cc_version_value
        local = local_cli_version()
        if local:
            self._cc_version_at = now
            self._cc_version_value = local
            return local
        try:
            import httpx

            res = httpx.get("https://registry.npmjs.org/command-code/latest", timeout=8.0)
            if res.is_success:
                version = (res.json() or {}).get("version")
                if isinstance(version, str) and version:
                    self._cc_version_at = now
                    self._cc_version_value = version
                    return version
        except Exception:
            pass  # 降级：留空由调用方决定
        return ""

    def detect_plan(self, up: dict) -> Optional[str]:
        """探测当前账号套餐档位；探测失败返回 None（不猜）。"""
        up["modelPlanError"] = None
        if not up.get("apiKey") or up.get("apiKeySource") == "none":
            up["modelPlanError"] = "本地无可用凭据（未配置 apiKeyEnv / apiKeyFile）"
            return None
        try:
            import httpx

            res = httpx.get(
                f"{up['baseUrl']}/alpha/billing/subscriptions",
                headers={
                    "authorization": f"Bearer {up['apiKey']}",
                    "x-command-code-version": self.cli_version(up),
                },
                timeout=8.0,
            )
            if not res.is_success:
                up["modelPlanError"] = f"订阅接口返回 {res.status_code}"
                return None
            plan_id = str(((res.json() or {}).get("data") or {}).get("planId") or "")
            tier = plan_id.split("-")[-1].lower()
            if tier not in CC_PLAN_RANK:
                up["modelPlanError"] = f'未识别的套餐标识 "{plan_id}"'
                return None
            return tier
        except Exception as exc:
            up["modelPlanError"] = str(exc)
            return None

    def refine_models(self, upstreams: list[dict]) -> None:
        """按套餐过滤 commandcode 上游对外的模型清单（启动时触发一次）。"""
        for up in upstreams:
            if up.get("protocol") != "commandcode":
                continue
            up["modelPlan"] = None
            if up.get("modelCatalogPlan") is False or not up.get("catalogEntries"):
                continue
            explicit = str(up.get("modelCatalogPlan") or "").lower()
            tier = explicit if explicit in CC_PLAN_RANK else self.detect_plan(up)
            if not tier:
                if access_log_enabled():
                    log("注意: 未能确定 CommandCode 套餐，模型清单未按套餐过滤"
                        "（可设 modelCatalogPlan: go|goat|pro|max 显式指定，或 false 关闭过滤）")
                continue
            up["modelPlan"] = tier
            rank = CC_PLAN_RANK[tier]
            catalog_ids = {e["id"] for e in up["catalogEntries"]}
            declared = [m for m in up["modelsAll"] if m not in catalog_ids]
            allowed = [e["id"] for e in up["catalogEntries"] if e["minPlanRank"] <= rank]
            merged: list[str] = []
            for m in declared + allowed:
                if m not in merged:
                    merged.append(m)
            up["models"] = merged
            if access_log_enabled():
                log(f"CommandCode 套餐 {CC_PLAN_LABEL[tier]}：清单按套餐过滤为 "
                    f"{len(merged)} 个（目录共 {len(up['catalogEntries'])} 个；"
                    "设 modelCatalogPlan: false 可不过滤）")

    # ── 主入口 ───────────────────────────────────────────────────────────
    def build_request(self, ctx: Ctx, config: dict) -> BuildResult:
        up = ctx.upstream
        # 凭据照搬 CommandCode 自身顺序（环境变量 → auth.json → 配置值）；
        # 都没有时本地直接报 401，不把必然失败的请求打到上游。
        if not up.get("apiKey") or up.get("apiKeySource") == "none":
            return BuildResult(error=ProtocolError(
                message="未配置 CommandCode 凭据：请设置 COMMAND_CODE_API_KEY，"
                        "或先在本机登录 CommandCode（~/.commandcode/auth.json）",
                status=401, type="authentication_error", code="missing_api_key",
            ))

        headers = {"content-type": "application/json", "user-agent": "cli"}
        version = self.cli_version(up)
        if version:
            headers["x-command-code-version"] = version
        elif access_log_enabled():
            log("注意: 未能确定 CommandCode CLI 版本，上游可能以 403 upgrade_required 拒绝")
        if up.get("cliEnvironment"):
            headers["x-cli-environment"] = up["cliEnvironment"]
        headers["x-session-id"] = self.cc_session_id(ctx.body)
        headers["authorization"] = f"Bearer {up['apiKey']}"
        # ZDR（Zero Data Retention）安全头：默认开，可用 cmdZdr: false 关闭
        if up.get("cmdZdr", True):
            headers["x-cmd-zdr"] = "1"

        system_parts: list[str] = []
        rest: list[dict] = []
        for m in ctx.body.get("messages") or []:
            if (m or {}).get("role") in ("system", "developer"):
                system_parts.append(text_of(m.get("content")))
            else:
                rest.append(m)
        params: dict = {
            "model": ctx.upstream_model,
            "messages": self.to_messages(rest),
            "tools": self.to_tools(ctx.body.get("tools")),
            "system": self.to_system(system_parts),
            "max_tokens": int(ctx.body.get("max_tokens") or ctx.body.get("max_completion_tokens")
                              or CC_MAX_TOKENS),
            "stream": True,  # 上游恒为流式，非流式由本桥聚合
        }
        if ctx.body.get("temperature") is not None:
            params["temperature"] = ctx.body["temperature"]
        effort = ctx.body.get("reasoning_effort")
        if isinstance(effort, str) and effort:
            params["reasoning_effort"] = effort
        # 强制工具选择：CommandCode wire 未声明该字段，故不下发（按 auto 处理）
        choice = ctx.body.get("tool_choice")
        if choice and choice not in ("auto", "none") and access_log_enabled():
            log("注意: commandcode 上游不下发 tool_choice，强制选择按 auto 处理（模型仍可自主调用工具）")

        return BuildResult(
            url=f"{up['baseUrl']}/alpha/generate",
            headers=headers,
            payload={
                "config": self.wire_config(),
                "memory": None,
                "taste": None,
                "skills": None,
                "permissionMode": "standard",
                # threadId 必须是合法 UUID，否则上游会静默丢弃该字段
                "threadId": self.thread_id_for(ctx.body),
                "params": params,
            },
            log_suffix="",
        )

    def _resume(self, ctx: Ctx, current) -> Optional[Iterable[str]]:
        """pause_turn 续跑：同一份 body 重发。返回新响应的行迭代器，失败返回 None。"""
        if self.post_lines is None or ctx.request is None:
            return None
        try:
            return self.post_lines(
                ctx.request.url, ctx.request.headers, ctx.request.payload, ctx.timeout
            )
        except Exception as exc:
            raise RuntimeError(f"续跑请求失败：{exc}") from exc

    def parse_stream(self, lines: Iterable[str], writer: SseWriter, ctx: Ctx) -> dict:
        """事件流 → OpenAI SSE（含 pause_turn 续跑）。"""
        finish_reason = "stop"
        usage = {"prompt_tokens": 0, "completion_tokens": 0,
                 "cacheReadTokens": 0, "cacheWriteTokens": 0}
        stream_error = ""
        saw_finish = False
        paused = False

        def on_finish(reason, u, raw):
            nonlocal finish_reason, saw_finish, paused
            finish_reason = reason
            usage["prompt_tokens"] += u["prompt_tokens"]
            usage["completion_tokens"] += u["completion_tokens"]
            usage["cacheReadTokens"] += u["cacheReadTokens"]
            usage["cacheWriteTokens"] += u["cacheWriteTokens"]
            saw_finish = True
            paused = raw == "pause_turn"

        def on_error(message):
            nonlocal stream_error
            stream_error = message

        # sink 只建一次：工具索引必须跨续跑连续，否则下游会看到错位的 tool_calls
        sink = self.make_sink({
            "text": lambda t: writer.write_chunk({"content": t}),
            "reasoning": lambda t: writer.write_chunk({"reasoning_content": t}),
            "toolStart": lambda index, tid, name: writer.write_chunk({
                "tool_calls": [{
                    "index": index, "id": tid, "type": "function",
                    "function": {"name": name, "arguments": ""},
                }]
            }),
            "toolInput": lambda index, delta: writer.write_chunk({
                "tool_calls": [{"index": index, "function": {"arguments": delta}}]
            }),
            "toolCall": lambda index, tid, name, inp, known: (
                None if known else writer.write_chunk({
                    "tool_calls": [{
                        "index": index, "id": tid, "type": "function",
                        "function": {
                            "name": name,
                            "arguments": json.dumps(inp or {}, ensure_ascii=False),
                        },
                    }]
                })
            ),
            "finish": on_finish,
            "error": on_error,
        })

        writer.write_chunk({"role": "assistant", "content": ""})

        source: Optional[Iterable[str]] = lines
        attempts = 0
        while True:
            self.consume(source, sink)
            if not (paused and ctx.pause_turn and ctx.request is not None
                    and attempts < CC_PAUSE_TURN_MAX):
                break
            attempts += 1
            if access_log_enabled():
                log(f"注意: 上游返回 pause_turn，续跑第 {attempts} 次（上限 {CC_PAUSE_TURN_MAX} 次）")
            try:
                source = self._resume(ctx, source)
            except Exception as exc:
                stream_error = str(exc)
                break
            if source is None:
                break
            paused = False

        from .base import usage_payload

        writer.write_chunk({}, finish_reason=finish_reason, usage=usage_payload(usage))
        writer.write_done()  # 上游不发 [DONE]，由本桥补齐
        return {
            **usage,
            "truncated": not saw_finish,
            "paused": paused,
            "error": stream_error,
        }

    def parse_response(self, raw: str, ctx: Ctx) -> dict:
        """NDJSON → 聚合后的 chat.completion（上游恒为流式，本桥本地聚合）。"""
        text_parts: list[str] = []
        reasoning_parts: list[str] = []
        calls: dict[int, dict] = {}
        finish_reason = "stop"
        usage = {"prompt_tokens": 0, "completion_tokens": 0,
                 "cacheReadTokens": 0, "cacheWriteTokens": 0}
        stream_error = ""
        saw_finish = False
        paused = False

        def on_finish(reason, u, raw):
            nonlocal finish_reason, saw_finish, paused
            finish_reason = reason
            usage["prompt_tokens"] += u["prompt_tokens"]
            usage["completion_tokens"] += u["completion_tokens"]
            usage["cacheReadTokens"] += u["cacheReadTokens"]
            usage["cacheWriteTokens"] += u["cacheWriteTokens"]
            saw_finish = True
            paused = raw == "pause_turn"

        def tool_start(index, tid, name):
            calls[index] = {"id": tid, "name": name, "args": ""}

        def tool_input(index, delta):
            if index in calls:
                calls[index]["args"] += delta

        def tool_call(index, tid, name, inp, known):
            if index not in calls:
                calls[index] = {"id": tid, "name": name, "args": ""}
            if not known:
                calls[index]["args"] = json.dumps(inp or {}, ensure_ascii=False)
            calls[index]["id"] = tid or calls[index]["id"]
            calls[index]["name"] = name or calls[index]["name"]

        def on_error(message: str) -> None:
            nonlocal stream_error
            stream_error = message

        sink = self.make_sink({
            "text": text_parts.append,
            "reasoning": reasoning_parts.append,
            "toolStart": tool_start,
            "toolInput": tool_input,
            "toolCall": tool_call,
            "finish": on_finish,
            "error": on_error,
        })

        source: Optional[Iterable[str]] = iter(str(raw or "").splitlines())
        attempts = 0
        while True:
            self.consume(source, sink)
            if not (paused and ctx.pause_turn and ctx.request is not None
                    and attempts < CC_PAUSE_TURN_MAX):
                break
            attempts += 1
            if access_log_enabled():
                log(f"注意: 上游返回 pause_turn，续跑第 {attempts} 次（上限 {CC_PAUSE_TURN_MAX} 次）")
            try:
                source = self._resume(ctx, source)
            except Exception as exc:
                stream_error = str(exc)
                break
            if source is None:
                break
            paused = False

        from .base import usage_payload

        body_text = "".join(text_parts)
        message: dict = {"role": "assistant", "content": body_text or None}
        if reasoning_parts:
            message["reasoning_content"] = "".join(reasoning_parts)
        ordered = [calls[k] for k in sorted(calls)]
        if ordered:
            message["tool_calls"] = [
                {
                    "index": i,
                    "id": c["id"],
                    "type": "function",
                    "function": {"name": c["name"], "arguments": c["args"] or "{}"},
                }
                for i, c in enumerate(ordered)
            ]
            if not body_text:
                message["content"] = None
        result = {
            "id": f"chatcmpl-{int(time.time() * 1000):x}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": ctx.requested_model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
            "usage": usage_payload(usage),
        }
        return {
            "body": result,
            "meta": {
                "truncated": not saw_finish,
                "paused": paused,
                "error": stream_error,
                "usage": usage,
            },
        }


def local_cli_version() -> str:
    """读已安装的 command-code 版本；读不到返回空串。"""
    import json as _json

    for root in command_code_roots():
        try:
            data = _json.loads((Path(root) / "package.json").read_text(encoding="utf-8"))
            version = data.get("version")
            if isinstance(version, str) and version:
                return version
        except Exception:
            continue  # 换下一个候选路径
    return ""
