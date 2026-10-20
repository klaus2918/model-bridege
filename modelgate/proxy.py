# -*- coding: utf-8 -*-
"""本地 HTTP 代理服务：路由、流式转发、访问日志与用量落库。

对外契约与 Node 版 model-bridge.js 完全一致：
  GET  /health                     上游与状态一览
  GET  /v1/models                  模型清单（顶层 models 为精确白名单）
  POST /v1/chat/completions        OpenAI 形状的对话补全（支持 stream）
  POST /{upstream}/v1/...          URL 前缀强制指定上游；未知前缀 404 并列出可用前缀
"""
from __future__ import annotations

import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Optional

import httpx

from .config import model_list, pick_upstream, resolve_model, upstreams_of
from .db import Database
from .protocols import Ctx, SseWriter, build_registry
from .protocols.thinking import ThinkingCache
from .util import access_log_enabled, log

MODELS_CACHE_DEFAULT_MS = 300_000


class InvalidRequestBody(ValueError):
    """请求体结构不合法（应返回 400，而不是 500）。"""


def _num(value):
    """把客户端发来的数字字段安全转成 int/float；转不动返回 None。"""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        text = value.strip()
        if not text:
            return None
        try:
            return float(text) if any(c in text for c in ".eE") else int(text)
        except ValueError:
            return None
    return None


def _key_source_desc(upstream: dict) -> str:
    """把「这把密钥从哪来」翻译成用户能直接去改的位置。"""
    source = upstream.get("apiKeySource")
    if source == "env" and upstream.get("apiKeyEnv"):
        return f"环境变量 {upstream['apiKeyEnv']}（默认写在 ~/.jcode/.env）"
    if source == "file" and upstream.get("apiKeyFile"):
        return f"密钥文件 {upstream['apiKeyFile']} 的 apiKey 字段"
    if source == "literal":
        return "配置页填写 / 配置文件里的 apiKey"
    return "该上游的 apiKey / apiKeyEnv / apiKeyFile"


def upstream_error_hint(upstream: dict, status: int, message: str) -> str:
    """把上游错误翻译成用户能照做的提示（中英混排，附在原文后）。

    目的：代理出问题时，用户看到的不该只是一句上游英文原文，而应知道
    「下一步改哪里」——密钥、余额、baseUrl 还是版本。
    """
    text = (message or "").lower()
    key_source = upstream.get("apiKeySource")
    placeholder = (not upstream.get("apiKey")) or key_source == "none" \
        or upstream.get("apiKey") == "any-key"
    base = str(upstream.get("baseUrl") or "")
    local = base.startswith("http://127.0.0.1") or base.startswith("http://localhost")

    if status in (401, 403):
        if "insufficient" in text or "credit" in text or "balance" in text:
            return "上游账户余额不足，请先充值或更换上游"
        if "upgrade" in text or "minversion" in text or "out of date" in text:
            return "上游要求更高客户端版本（x-command-code-version），请升级对应 CLI"
        # 密钥过期是最容易被误判成「工具坏了」的一类：上游明确说 expired 时，
        # 必须直接告诉用户「去新建 key」，而不是泛泛地说「密钥无效」。
        if "expired" in text or "api_key_expired" in text or "has expired" in text:
            return (f"该 API Key 已在上游侧过期（不是本工具的问题）："
                    f"请到上游控制台新建 API Key，替换 {_key_source_desc(upstream)}，"
                    f"然后在面板点「重新载入」即可生效（无需重启）。"
                    f"注意：重新登录不会续期这把 key。")
        if placeholder:
            env = upstream.get("apiKeyEnv")
            hint = "未配置真实密钥（当前是占位值）。请在配置页填写 apiKey"
            if env:
                hint += f"，或设置环境变量 {env}"
            else:
                hint += "，或设置 apiKeyEnv"
            return hint
        if "invalid" in text or "incorrect" in text or "not valid" in text:
            return (f"上游认为该密钥无效：请核对 {_key_source_desc(upstream)} "
                    f"是否完整（注意多余空格、换行或复制时被截断）")
        return f"密钥无效或已过期，请检查 {_key_source_desc(upstream)}"
    if status == 404 and local:
        return f"本地上游未提供该端点，请确认 baseUrl={base} 指向的是正确的代理服务"
    if status == 400 and ("model" in text and "not supported" in text):
        return "该上游不支持此模型，请在配置页把模型清单调整为该上游支持的模型，或改用其它上游"
    if status == 400 and ("insufficient" in text or "credit" in text):
        return "上游账户余额不足，请先充值或更换上游"
    if local:
        return f"本地上游无响应（{base}），请确认对应服务已启动，或修改 baseUrl"
    return ""


def read_response_text(response) -> str:
    """安全读取响应体文本。

    关键点：客户端要求 stream 时，我们用 send(stream=True) 拿到的是**流式响应**；
    若上游此刻返回的是错误状态码，直接取 `.text` 会抛 httpx.ResponseNotRead
    （真实用户场景：客户端请求流式 → 上游 4xx → 旧代码 500 且不落库）。
    因此必须先 read() 再取文本；读不到时给出占位说明而不是抛异常。
    """
    try:
        response.read()
    except Exception:
        pass
    try:
        return response.text
    except Exception as exc:  # noqa: BLE001
        return f"<无法读取上游响应体: {type(exc).__name__}: {exc}>"


def sanitize_chat_body(body: dict) -> tuple[dict, list[str]]:
    """在入口把畸形字段归一，避免下游协议适配层抛异常变成 500。

    - messages 必须是数组，非对象条目丢弃；tool_calls 里的非对象项丢弃
    - tools/stop 等数组字段只保留合法类型
    - max_tokens / temperature 等数字字段做安全转换，转不动就移除
    返回 (净化后的 body, 告警列表)。结构性错误抛 InvalidRequestBody（→ 400）。
    """
    if not isinstance(body, dict):
        raise InvalidRequestBody("请求体必须是 JSON 对象")

    out = dict(body)
    warnings: list[str] = []

    # messages：结构性要求，缺失按「空列表」处理（由调用方决定是否 400）
    if "messages" in out and out["messages"] is not None \
            and not isinstance(out["messages"], list):
        raise InvalidRequestBody("messages 字段必须是数组")

    messages: list[dict] = []
    dropped = 0
    for item in out.get("messages") or []:
        if not isinstance(item, dict):
            dropped += 1
            continue
        entry = dict(item)
        tool_calls = entry.get("tool_calls")
        if isinstance(tool_calls, list):
            kept = [tc for tc in tool_calls if isinstance(tc, dict)]
            if len(kept) != len(tool_calls):
                dropped += len(tool_calls) - len(kept)
            entry["tool_calls"] = kept
        elif tool_calls is not None:
            entry.pop("tool_calls", None)
            dropped += 1
        # content 只接受 字符串 / 分片数组 / 数字 / null；其余转成字符串兜底
        content = entry.get("content")
        if content is not None and not isinstance(content, (str, list, int, float)):
            entry["content"] = str(content)
            warnings.append("content 类型非法，已转成字符串")
        messages.append(entry)
    if dropped:
        warnings.append(f"已忽略 {dropped} 条非法消息/工具调用项")
    if "messages" in out or "messages" in body:
        out["messages"] = messages

    # tools：只保留带 function.name 的对象
    tools = out.get("tools")
    if isinstance(tools, list):
        kept_tools = [t for t in tools
                      if isinstance(t, dict)
                      and t.get("type") == "function"
                      and isinstance(t.get("function"), dict)
                      and t["function"].get("name")]
        if len(kept_tools) != len(tools):
            warnings.append(f"已忽略 {len(tools) - len(kept_tools)} 条非法工具定义")
        out["tools"] = kept_tools

    # stop：字符串或字符串数组
    if "stop" in out and out["stop"] is not None:
        stop = out["stop"]
        if isinstance(stop, str):
            out["stop"] = [stop] if stop else []
        elif isinstance(stop, list):
            out["stop"] = [s for s in stop if isinstance(s, str)]
        else:
            out.pop("stop", None)
            warnings.append("stop 类型非法，已忽略")

    # 数字字段：转不动就移除，交给默认值
    for key in ("max_tokens", "max_completion_tokens", "n", "top_k",
                "presence_penalty", "frequency_penalty", "seed"):
        if key in out and out[key] is not None:
            num = _num(out[key])
            if num is None:
                out.pop(key)
                warnings.append(f"{key} 不是数字，已忽略")
            else:
                out[key] = int(num)
    for key in ("temperature", "top_p"):
        if key in out and out[key] is not None:
            num = _num(out[key])
            if num is None:
                out.pop(key)
                warnings.append(f"{key} 不是数字，已忽略")
            else:
                out[key] = float(num)

    # model 必须是字符串（数字/对象等转字符串）
    if "model" in out and out["model"] is not None \
            and not isinstance(out["model"], str):
        out["model"] = str(out["model"])
        warnings.append("model 已转成字符串")

    return out, warnings


class Bridge:
    """代理运行态：配置 + 上游 + 协议注册表 + 数据库 + 各类缓存。"""

    def __init__(self, config: dict, db: Database, *, config_path: Optional[Path] = None) -> None:
        self.config = config
        self.db = db
        self.started_at = time.time()
        self.config_path = Path(config_path) if config_path else None
        self.upstreams: list[dict] = upstreams_of(config)
        self.thinking_cache = ThinkingCache(
            enabled=config.get("thinking", {}).get("passthrough") is not False,
            max_size=int(config.get("thinking", {}).get("cacheSize") or 500),
        )
        self.downgrade_forced = config.get("tools", {}).get("downgradeForcedChoice") is not False
        self.timeout = float(config.get("timeoutSeconds") or 600)
        self.access_log = config.get("accessLog") is not False
        self.models_cache_ms = int(config.get("modelsCacheSeconds") or 300) * 1000
        self.client = httpx.Client(
            timeout=httpx.Timeout(self.timeout, connect=10.0),
            follow_redirects=False,
        )
        self.adapters = build_registry(
            thinking_cache=self.thinking_cache,
            downgrade_forced_choice=self.downgrade_forced,
            post_lines=self.post_lines,
        )
        self._models_cache: dict = {"at": 0.0, "ids": [], "owner": {}}
        self._cache_lock = threading.Lock()
        self._retention_started = False

    # ── 生命周期 ─────────────────────────────────────────────────────────
    def start_background_tasks(self) -> None:
        """服务监听后再做套餐探测与清单收窄（与 Node 版一致，避免启动期阻塞）。"""
        def runner() -> None:
            try:
                adapter = self.adapters.get("commandcode")
                if adapter is not None:
                    adapter.refine_models(self.upstreams)
            except Exception as exc:  # 探测失败不影响代理可用
                if self.access_log:
                    log(f"注意: CommandCode 套餐探测/过滤失败：{exc}")

        threading.Thread(target=runner, daemon=True, name="cc-plan-refine").start()

        # 保留策略：启动即跑一次，之后每 24h 一次。
        # 用标志位防止重复启动线程（端口降级/配置热更新都会再次调用本方法）。
        with self._cache_lock:
            if not self._retention_started:
                self._retention_started = True
                threading.Thread(target=self._retention_loop, daemon=True,
                                 name="retention").start()
            else:
                threading.Thread(target=self.apply_retention, daemon=True,
                                 name="retention-now").start()

    def apply_retention(self) -> int:
        """按 ``retentionDays`` 清理超期明细（0 = 永久保留）。返回删除条数。"""
        try:
            days = int(self.config.get("retentionDays") or 0)
        except (TypeError, ValueError):
            days = 0
        if days <= 0:
            return 0
        try:
            removed = self.db.delete_older_than(days=days)
            if removed:
                self.db.vacuum()
                if self.access_log:
                    log(f"保留策略：已清理 {removed} 条早于 {days} 天的明细")
            return removed
        except Exception:
            return 0

    def _retention_loop(self) -> None:
        """每 24h 执行一次保留策略（首轮延时启动，不抢启动期资源）。"""
        while True:
            time.sleep(RETENTION_INTERVAL_SECONDS)
            self.apply_retention()

    def close(self) -> None:
        try:
            self.client.close()
        except Exception:
            pass

    # ── 请求工具 ─────────────────────────────────────────────────────────
    @property
    def pricing(self) -> dict:
        """当前单价表（规整后）。费用估算只读它，改配置即时生效。"""
        from .pricing import normalize_pricing

        return normalize_pricing(self.config.get("pricing"))

    def note_active_port(self, port: int) -> None:
        """记录实际监听端口：供二次启动时定位并激活已有实例的面板地址。

        端口可能因占用而降级，所以必须以「实际绑定到的那一个」为准，
        而不是配置里声明的那个。
        """
        self.last_port = int(port)
        try:
            self.db.set_setting("lastPort", str(int(port)))
        except Exception:
            pass

    def protocol_for(self, upstream: dict):
        """协议名 → 适配器；未登记的协议按 openai 处理（与 Node 版一致）。"""
        return self.adapters.get(upstream.get("protocol")) or self.adapters["openai"]

    def post_lines(self, url: str, headers: dict, payload: dict, timeout: float):
        """发一次 POST 并返回文本行迭代器（供 commandcode 的 pause_turn 续跑）。"""
        request = self.client.build_request(
            "POST", url, headers=headers, json=payload, timeout=timeout
        )
        response = self.client.send(request, stream=True)

        def generator():
            try:
                yield from response.iter_lines()
            finally:
                response.close()

        return generator()

    def fetch_upstream_models(self, upstream: dict) -> list[str]:
        """从某个上游拉取模型列表；失败返回空数组。"""
        if upstream.get("fetchModels") is False:
            return []
        compat = upstream.get("compat") or {}
        auth = compat.get("auth") or {}
        scheme = str(auth.get("scheme") or "bearer").lower()
        headers: dict = {str(k).lower(): str(v)
                         for k, v in (compat.get("headers") or {}).items()}
        key = upstream.get("apiKey")
        if key and scheme != "none":
            if scheme == "x-api-key":
                headers["x-api-key"] = key
            elif scheme == "both":
                headers["authorization"] = f"Bearer {key}"
                headers["x-api-key"] = key
            elif scheme == "header":
                headers[str(auth.get("header") or "authorization").lower()] = (
                    f"{auth.get('prefix') or ''}{key}")
            else:
                headers["authorization"] = f"{auth.get('prefix') or 'Bearer '}{key}"
        path = str((compat.get("endpoint") or {}).get("models") or "/v1/models")
        try:
            res = self.client.get(f"{upstream['baseUrl']}{path}",
                                  headers=headers, timeout=8.0)
            if not res.is_success:
                return []
            data = res.json() or {}
            items = data.get("data") or data.get("models") or []
            out = []
            for m in items:
                if isinstance(m, dict):
                    ident = m.get("id") or m.get("slug") or m.get("model")
                    if ident:
                        out.append(ident)
            return out
        except Exception:
            return []

    def refresh_models_cache(self, *, force: bool = False) -> dict:
        """未配白名单时退化为拉取（仅限显式开启 autoModels 的上游），带缓存。

        ``force=True`` 跳过缓存（托盘「刷新模型目录」用），失败时保留旧缓存。
        """
        now = time.time() * 1000
        with self._cache_lock:
            if not force and now - self._models_cache["at"] <= self.models_cache_ms:
                return self._models_cache
            previous = self._models_cache
        ids: list[str] = []
        owner: dict[str, str] = {}
        for up in self.upstreams:
            fetched = self.fetch_upstream_models(up) if up.get("autoModels") else []
            for model_id in fetched:
                if model_id not in ids:
                    ids.append(model_id)
                owner.setdefault(model_id, up["name"])
        if force and not ids and previous.get("ids"):
            # 强制刷新失败（上游不可达）时不要清空已有清单：宁可旧，不要空
            return previous
        with self._cache_lock:
            self._models_cache = {"at": now, "ids": ids, "owner": owner}
            return self._models_cache


# ── HTTP 处理器 ────────────────────────────────────────────────────────────
class BridgeHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "ModelGate/2.0"
    bridge: Bridge  # 由 create_server 注入

    # BaseHTTPRequestHandler 的日志钩子：统一走本工具的访问日志开关
    def log_message(self, fmt: str, *args) -> None:  # noqa: D102
        return

    # ── 响应工具 ─────────────────────────────────────────────────────────
    def _send(self, status: int, body: bytes, content_type: str,
              extra_headers: Optional[dict] = None) -> None:
        """写出完整响应。客户端中途断开只收尾，不向上抛（否则会触发二次响应）。"""
        self._responded = True
        try:
            self.send_response(status)
            self.send_header("content-type", content_type)
            self.send_header("content-length", str(len(body)))
            self.send_header("connection", "close")
            for key, value in (extra_headers or {}).items():
                if value is not None:
                    self.send_header(key, str(value))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError,
                OSError) as exc:
            # 客户端已放弃读取（如超时断开）：这是常态，不是服务端错误
            if access_log_enabled():
                log(f"响应写入中断（客户端已断开）: {type(exc).__name__}")
        finally:
            self.close_connection = True

    def send_json(self, status: int, payload, extra_headers: Optional[dict] = None) -> None:
        text = json.dumps(payload, ensure_ascii=False)
        self._send(status, text.encode("utf-8"),
                   "application/json; charset=utf-8", extra_headers)

    def send_error_json(self, message: str, type_: str = "upstream_error",
                        code: str = "upstream_failed", status: int = 500) -> None:
        self.send_json(status, {"error": {"message": message, "type": type_, "code": code}})

    def start_sse(self, extra_headers: Optional[dict] = None) -> SseWriter:
        self._responded = True
        try:
            self.send_response(200)
            self.send_header("content-type", "text/event-stream; charset=utf-8")
            self.send_header("cache-control", "no-cache")
            self.send_header("connection", "close")
            for key, value in (extra_headers or {}).items():
                if value is not None:
                    self.send_header(key, str(value))
            self.end_headers()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError,
                OSError) as exc:
            if access_log_enabled():
                log(f"SSE 响应头写入中断: {type(exc).__name__}")
        finally:
            self.close_connection = True
        return SseWriter(self.wfile, getattr(self, "_requested_model", ""))

    def read_body(self) -> bytes:
        length = int(self.headers.get("content-length") or 0)
        return self.rfile.read(length) if length else b""

    # ── 路由 ─────────────────────────────────────────────────────────────
    def do_GET(self) -> None:  # noqa: N802
        self._route("GET")

    def do_POST(self) -> None:  # noqa: N802
        self._route("POST")

    def _route(self, method: str) -> None:
        """路由分发；任何未捕获异常都转成 500 响应，绝不静默断开连接。

        traceback 同时写 stderr 与文件日志：exe 是无控制台程序，stderr 用户看不到。
        """
        self._responded = False
        try:
            self._route_inner(method)
        except Exception:
            import traceback as _tb

            detail = _tb.format_exc()
            try:
                import sys as _sys

                _sys.stderr.write(detail)
                _sys.stderr.flush()
            except Exception:
                pass
            try:
                from .util import log as _log

                first = detail.strip().splitlines()[-1] if detail.strip() else "unknown"
                _log(f"处理请求异常 [{method} {self.path}]: {first}")
                _log(detail.rstrip())
            except Exception:
                pass
            try:
                if not getattr(self, "_responded", False):
                    self.send_error_json("内部错误：详见服务端控制台",
                                         type_="server_error",
                                         code="internal_error", status=500)
                else:
                    self.close_connection = True
            except Exception:
                # 响应通道本身已坏（客户端断开），只能收尾，不能二次写响应
                self.close_connection = True

    def _route_inner(self, method: str) -> None:
        path = self.path.split("?", 1)[0]
        forced = None
        # 前缀路由：/{上游名}/v1/...（仅当第二段确实是 v1 时才按前缀识别，
        # 避免把 /v1/models 本身误判成上游前缀）
        segments = path.split("/")
        if len(segments) >= 3 and segments[1] and segments[2] == "v1":
            candidate = segments[1]
            match = next((u for u in self.bridge.upstreams if u["name"] == candidate), None)
            if match is not None:
                forced = match
                path = "/" + "/".join(segments[2:])
            elif "/v1/" in path:
                available = "、".join(f"/{u['name']}" for u in self.bridge.upstreams)
                self.send_error_json(
                    f'未知的上游前缀 "{candidate}"。可用前缀：{available}；'
                    "不带前缀时按模型名自动分发。",
                    type_="invalid_request_error", code="unknown_upstream", status=404,
                )
                return

        if path == "/health" and method == "GET":
            self.handle_health()
        elif path == "/v1/models" and method == "GET":
            self.handle_models(forced)
        elif path == "/v1/chat/completions" and method == "POST":
            self.handle_chat(forced)
        # /api/* 需要保留查询串（range / group_by / limit 等），传原始 self.path
        elif path.startswith("/api/") and method == "GET":
            self.handle_api_get(self.path)
        elif path.startswith("/api/") and method == "POST":
            self.handle_api_post(self.path)
        elif path.startswith("/ui") and method == "GET":
            self.handle_ui(path)
        else:
            self.send_error_json(
                f"未实现的端点 {self.path}",
                type_="invalid_request_error", code="not_implemented", status=404,
            )

    # ── 端点实现 ─────────────────────────────────────────────────────────
    def handle_health(self) -> None:
        fallback = next((u for u in self.bridge.upstreams if u["default"]),
                        self.bridge.upstreams[0] if self.bridge.upstreams else None)
        host = self.headers.get("host") or (
            f"{self.bridge.config.get('host')}:{self.bridge.config.get('port')}"
        )
        self.send_json(200, {
            "status": "ok",
            # 兼容字段：默认上游地址（单上游时期的形态，便于既有脚本继续工作）
            "upstream": (fallback or {}).get("baseUrl", ""),
            "upstreams": [
                {
                    "name": u["name"],
                    "baseUrl": u["baseUrl"],
                    "protocol": u["protocol"],
                    # models = 对外可见（可能已按套餐过滤）；modelsAll = 声明 + 目录全集
                    "models": len(u["models"]),
                    "modelsAll": len(u["modelsAll"]),
                    "plan": u.get("modelPlan"),
                    "planError": u.get("modelPlanError"),
                    "default": u["default"],
                    "endpoint": f"http://{host}/{u['name']}/v1",
                }
                for u in self.bridge.upstreams
            ],
        })

    def handle_models(self, forced) -> None:
        now = int(time.time())
        # 指定了上游前缀：只列该上游声明的模型，不拉取其它上游
        if forced is not None:
            ids = list(forced["models"])
            if not ids:
                self.send_json(200, {"object": "list", "data": []})
                return
            self.send_json(200, {
                "object": "list",
                "data": [
                    {"id": i, "object": "model", "created": now, "owned_by": forced["name"]}
                    for i in ids
                ],
            })
            return

        whitelist = self.bridge.config.get("models") or []
        if not whitelist:
            cache = self.bridge.refresh_models_cache()
            cached_owner = dict(cache["owner"])
            cached_ids = list(cache["ids"])
        else:
            cached_owner, cached_ids = {}, []

        ids = model_list(self.bridge.config, self.bridge.upstreams)
        if not whitelist:
            for model_id in cached_ids:
                if model_id not in ids:
                    ids.append(model_id)

        self.send_json(200, {
            "object": "list",
            "data": [
                {
                    "id": i,
                    "object": "model",
                    "created": now,
                    # 优先用拉取时登记的归属；配置声明的模型按上游命中推断
                    "owned_by": cached_owner.get(i)
                    or pick_upstream(self.bridge.config, self.bridge.upstreams, i)["name"],
                }
                for i in ids
            ],
        })

    def handle_chat(self, forced) -> None:
        started = time.time()
        try:
            raw = self.read_body()
            body = json.loads(raw.decode("utf-8") or "{}")
        except Exception:
            self.send_error_json("请求体不是合法 JSON",
                                 type_="invalid_request_error", code="invalid_json",
                                 status=400)
            return
        if not isinstance(body, dict):
            body = {}

        # 入口净化：畸形字段在协议适配层之前处理掉，否则会变成 500
        try:
            body, sanitize_warnings = sanitize_chat_body(body)
        except InvalidRequestBody as exc:
            self.send_error_json(str(exc), "invalid_request_error",
                                 "invalid_request", status=400)
            return
        if sanitize_warnings and self.bridge.access_log:
            log(f"请求体已净化 [{self.path}]: {'；'.join(sanitize_warnings)}")

        default_model = self.bridge.config.get("defaultModel")
        if not body.get("model") and not default_model:
            self.send_error_json("缺少 model 字段且未配置 defaultModel",
                                 type_="invalid_request_error", code="model_required",
                                 status=400)
            return

        requested_model = body.get("model") or default_model
        primary = forced or pick_upstream(
            self.bridge.config, self.bridge.upstreams, requested_model)
        # 候补上游：只在「客户端还没收到任何字节」的区间内使用（见 _failover_candidates）。
        candidates = self._failover_candidates(requested_model, primary, forced=forced)

        upstream = primary
        upstream_model = ""
        adapter = None
        ctx = None
        log_suffix = ""
        upstream_response = None
        failover_note = ""

        for index, candidate in enumerate(candidates):
            upstream = candidate
            upstream_model = upstream["modelMap"].get(
                requested_model, resolve_model(self.bridge.config, requested_model))
            adapter = self.bridge.protocol_for(upstream)
            self._requested_model = requested_model
            self._session = self._resolve_session(adapter, body)

            ctx = Ctx(
                body=body,
                requested_model=requested_model,
                upstream_model=upstream_model,
                upstream=upstream,
                client_auth=self.headers.get("authorization"),
                timeout=self.bridge.timeout,
                pause_turn=bool(upstream.get("pauseTurn")),
                stream=bool(body.get("stream")),
            )

            build = adapter.build_request(ctx, self.bridge.config)
            if build.error is not None:
                self.send_json(build.error.status, build.error.body())
                return
            ctx.request = build
            log_suffix = build.log_suffix or ""

            try:
                request = self.bridge.client.build_request(
                    "POST", build.url, headers=build.headers, json=build.payload
                )
                # 流式：保持连接由适配器逐行消费；非流式：直接读完，便于后续取 .text
                upstream_response = self.bridge.client.send(
                    request, stream=bool(ctx.stream))
            except Exception as exc:
                elapsed = int((time.time() - started) * 1000)
                if self._has_next_candidate(index, candidates):
                    failover_note = f"{upstream['name']} 连接失败({exc})"
                    if self.bridge.access_log:
                        log(f"{requested_model} -> {upstream['name']}:{upstream_model} "
                            f"连接失败 {elapsed}ms {exc}；"
                            f"自动切换上游 {candidates[index + 1]['name']}")
                    continue
                if self.bridge.access_log:
                    log(f"{requested_model} -> {upstream['name']}:{upstream_model} "
                        f"连接失败 {elapsed}ms {exc}")
                self._record(usage={}, upstream=upstream, upstream_model=upstream_model,
                             duration_ms=elapsed, status=502, stream=ctx.stream,
                             error=str(exc) + (
                                 f"（前序已失败：{failover_note}）" if failover_note else ""))
                hint = upstream_error_hint(upstream, 0, str(exc))
                suffix = f"；建议：{hint}" if hint else ""
                self.send_error_json(
                    f"无法连接上游 {upstream['baseUrl']}: {exc}{suffix}", status=502)
                return

            # 5xx / 429 属于「尚未写出任何响应字节」的可切换区间：
            # 客户端还没收到东西，此刻换上游对用户是无感的 —— 这正是「代理不中断」。
            status = upstream_response.status_code
            if (not upstream_response.is_success
                    and self._retryable_upstream_status(status)
                    and self._has_next_candidate(index, candidates)):
                preview = read_response_text(upstream_response)[:200]
                failover_note = f"{upstream['name']} 返回 {status}"
                if self.bridge.access_log:
                    log(f"{requested_model} -> {upstream['name']}:{upstream_model} "
                        f"{status}；自动切换上游 {candidates[index + 1]['name']}"
                        f"（{preview[:120]}）")
                continue
            break
        else:  # pragma: no cover - 循环分支均以 return/continue/break 收尾
            self.send_error_json("全部候选上游均不可用", status=502)
            return

        try:
            if not upstream_response.is_success:
                # 上游可能是流式响应（客户端要求 stream）：必须先 read() 才能取文本
                text = read_response_text(upstream_response)
                message = text
                try:
                    message = (json.loads(text) or {}).get("error", {}).get("message") or text
                except Exception:
                    pass  # 保留原文
                elapsed = int((time.time() - started) * 1000)
                status = upstream_response.status_code
                if self.bridge.access_log:
                    log(f"{requested_model} -> {upstream['name']}:{upstream_model} "
                        f"{status} {elapsed}ms")
                self._record(usage={}, upstream=upstream, upstream_model=upstream_model,
                             duration_ms=elapsed, status=status, stream=ctx.stream,
                             error=message[:500])
                hint = upstream_error_hint(upstream, status, str(message))
                suffix = f"；建议：{hint}" if hint else ""
                self.send_error_json(f"上游返回 {status}: {message}{suffix}",
                                     status=status)
                return

            if ctx.stream:
                self._handle_stream(adapter, ctx, upstream_response, started,
                                    upstream, upstream_model, log_suffix)
                return

            raw_text = read_response_text(upstream_response)
            # 不在这里清超时：协议可能有后续请求（commandcode pause_turn 续跑），
            # 超时应当覆盖整段处理。
            parsed = adapter.parse_response(raw_text, ctx)
        finally:
            # httpx.Response 不支持 with，必须显式关闭，否则连接泄漏
            try:
                upstream_response.close()
            except Exception:
                pass

        elapsed = int((time.time() - started) * 1000)
        if parsed.get("error"):
            if self.bridge.access_log:
                log(f"{requested_model} -> {upstream['name']}:{upstream_model} "
                    f"{elapsed}ms 解析失败: {parsed['error']}")
            self._record(usage={}, upstream=upstream, upstream_model=upstream_model,
                         duration_ms=elapsed, status=502, stream=False,
                         error=str(parsed["error"])[:500])
            self.send_error_json(parsed["error"], status=502)
            return

        result = parsed["body"]
        meta = parsed.get("meta") or {}
        usage = meta.get("usage") or result.get("usage") or {}
        extra = self._meta_suffix(meta)
        if self.bridge.access_log:
            log(f"{requested_model} -> {upstream['name']}:{upstream_model} 200 {elapsed}ms "
                f"tokens={usage.get('prompt_tokens', '?')}+{usage.get('completion_tokens', '?')}"
                f"{log_suffix}{extra}")
        # 上游语义：流结束仍未收到 finish 事件即视为截断（可重试），与 CLI 一致
        if meta.get("truncated"):
            self._record(usage=usage, upstream=upstream, upstream_model=upstream_model,
                         duration_ms=elapsed, status=502, stream=False, truncated=True,
                         paused=bool(meta.get("paused")), error=meta.get("error"))
            self.send_error_json("上游响应被截断（未收到 finish 事件）",
                                 status=502, code="upstream_truncated")
            return
        self._record(usage=usage, upstream=upstream, upstream_model=upstream_model,
                     duration_ms=elapsed, status=200, stream=False,
                     truncated=False, paused=bool(meta.get("paused")),
                     error=meta.get("error"))
        self.send_json(200, result, {
            "x-bridge-upstream": upstream["name"],
            "x-bridge-upstream-model": upstream_model,
        })

    def _handle_stream(self, adapter, ctx, upstream_response, started,
                       upstream, upstream_model, log_suffix) -> None:
        """流式响应：写 SSE 头，逐行喂给协议适配器，收尾时落库。"""
        writer = self.start_sse({
            "x-bridge-upstream": upstream["name"],
            "x-bridge-upstream-model": upstream_model,
        })
        usage: dict = {}
        error: Optional[str] = None
        timed_lines = _TimedLines(upstream_response.iter_lines())
        try:
            usage = adapter.parse_stream(timed_lines, writer, ctx) or {}
        except Exception as exc:
            error = str(exc)
            if self.bridge.access_log:
                log(f"{ctx.requested_model} -> {upstream['name']}:{upstream_model} "
                    f"流式中断 {int((time.time() - started) * 1000)}ms {exc}")
            try:
                writer.write_done()
            except Exception:
                pass

        elapsed = int((time.time() - started) * 1000)
        # TTFT = 上游首行到达 - 请求发出：流式才有增量信号，非流式恒为 0。
        ttft = int((timed_lines.first_at - started) * 1000) if timed_lines.first_at else 0
        ttft = max(0, min(ttft, elapsed))
        if self.bridge.access_log and error is None:
            extra = self._meta_suffix(usage)
            log(f"{ctx.requested_model} -> {upstream['name']}:{upstream_model} 200 {elapsed}ms "
                f"ttft={ttft}ms stream tokens={usage.get('prompt_tokens', 0)}+"
                f"{usage.get('completion_tokens', 0)}{log_suffix}{extra}")
        self._record(usage=usage, upstream=upstream, upstream_model=upstream_model,
                     duration_ms=elapsed, ttft_ms=ttft,
                     status=200 if error is None else 502,
                     stream=True, truncated=bool(usage.get("truncated")),
                     paused=bool(usage.get("paused")),
                     error=error or usage.get("error"))

    @staticmethod
    def _meta_suffix(meta: dict) -> str:
        if not isinstance(meta, dict):
            return ""
        parts = []
        if meta.get("truncated"):
            parts.append(" 截断(无 finish)")
        if meta.get("paused"):
            parts.append(" pause_turn 未续完")
        cache_read = (meta.get("usage") or {}).get("cacheReadTokens") if isinstance(
            meta.get("usage"), dict) else meta.get("cacheReadTokens")
        if cache_read:
            cache_write = (meta.get("usage") or {}).get("cacheWriteTokens", 0) if isinstance(
                meta.get("usage"), dict) else meta.get("cacheWriteTokens", 0)
            parts.append(f" cache={cache_read}+{cache_write or 0}")
        if meta.get("error"):
            parts.append(f" 上游错误: {meta['error']}")
        return "".join(parts)

    def _failover_candidates(self, requested_model: str, primary: dict, *,
                             forced=None, limit: int = 2) -> list[dict]:
        """候补上游：同样声明了该模型、且不是主选的那些（总数上限 1+limit）。

        设计取舍（服务「代理不中断」，但不做危险的重放）：
        - **只在客户端还没收到任何字节时使用**：连接失败或 5xx/429 之后才切换；
          一旦开始向客户端写响应，绝不重放（否则用户会收到重复内容）。
        - **上限 2 个候补**：上游集体故障时，别把一次请求放大成多次调用。
        - **前缀路由（forced）不切换**：用户显式指定了上游，就忠于他的选择。
        """
        if forced is not None:
            return [primary]
        model = str(requested_model or "")
        out = [primary]
        max_total = max(1, int(limit) + 1)
        for upstream in self.bridge.upstreams:
            if upstream["name"] == primary["name"]:
                continue
            served = set(upstream.get("models") or []) | set(upstream.get("modelMap") or {})
            if model and model not in served:
                continue
            out.append(upstream)
            if len(out) >= max_total:
                break
        return out

    @staticmethod
    def _has_next_candidate(index: int, candidates: list) -> bool:
        return index + 1 < len(candidates)

    @staticmethod
    def _retryable_upstream_status(status: int) -> bool:
        """可切换上游的状态码：上游自身故障（5xx）与限流（429）。

        4xx（401/400/404…）不切换：那是请求体或凭据的问题，
        换一个上游只会把真实问题掩盖掉，还会浪费额度。
        """
        return int(status) >= 500 or int(status) == 429

    def _resolve_session(self, adapter, body: dict) -> str:
        """定位本次请求的会话标识：显式头优先，其次用适配器推导的会话。"""
        session = client_session(self.headers, body)
        if not session:
            getter = getattr(adapter, "session_id", None)
            if callable(getter):
                try:
                    session = str(getter(body) or "")
                except Exception:
                    session = ""
        return session[:64]

    def _record(self, *, usage: dict, upstream: dict, upstream_model: str,
                duration_ms: int, status: int, stream: bool,
                truncated: bool = False, paused: bool = False,
                error: Optional[str] = None, ttft_ms: int = 0) -> None:
        """用量落库（含费用估算、会话标识与 TTFT）：任何异常都不能影响响应。"""
        try:
            from .pricing import cost_of

            requested_model = getattr(self, "_requested_model", "")
            prompt_tokens = int((usage or {}).get("prompt_tokens") or 0)
            completion_tokens = int((usage or {}).get("completion_tokens") or 0)
            cache_read = int((usage or {}).get("cacheReadTokens") or 0)
            cache_write = int((usage or {}).get("cacheWriteTokens") or 0)
            cost = cost_of(
                self.bridge.pricing,
                upstream=upstream.get("name", ""),
                model=requested_model,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                cache_read_tokens=cache_read,
                cache_write_tokens=cache_write,
            )
            self.bridge.db.record_usage(
                upstream=upstream.get("name", ""),
                protocol=upstream.get("protocol", ""),
                requested_model=requested_model,
                upstream_model=upstream_model,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                cache_read_tokens=cache_read,
                cache_write_tokens=cache_write,
                duration_ms=duration_ms,
                ttft_ms=ttft_ms,
                status=status,
                stream=stream,
                truncated=truncated,
                paused=paused,
                error=str(error)[:1000] if error else None,
                cost=cost,
                session=getattr(self, "_session", ""),
            )
        except Exception:
            pass

    # ── 静态页与本地 API（桌面 GUI 用） ──────────────────────────────────
    def handle_ui(self, path: str) -> None:
        web_root = Path(__file__).resolve().parent / "web"
        # 允许 ?v=xxx 版本参数：浏览器换缓存键，服务端仍按文件名定位
        rel = path[len("/ui"):].split("?", 1)[0].lstrip("/") or "index.html"
        target = (web_root / rel).resolve()
        if not str(target).startswith(str(web_root.resolve())) or not target.is_file():
            self.send_error_json("页面不存在", status=404)
            return
        content_type = {
            ".html": "text/html; charset=utf-8",
            ".js": "application/javascript; charset=utf-8",
            ".css": "text/css; charset=utf-8",
            ".svg": "image/svg+xml",
        }.get(target.suffix, "application/octet-stream")
        body = target.read_bytes()
        if target.suffix == ".html":
            # 资源版本戳：即使浏览器按启发式规则复用了旧 HTML，
            # 里面的 app.js/style.css 也会指向新版本，避免「代码改了、界面没变」。
            from . import __version__

            stamp = f"?v={__version__}"
            text = body.decode("utf-8")
            text = text.replace('href="style.css"', f'href="style.css{stamp}"')
            text = text.replace('src="app.js"', f'src="app.js{stamp}"')
            body = text.encode("utf-8")
        # 面板是本地资产：改完必须立刻可见。缺缓存头时浏览器会按启发式规则
        # 继续用旧的 app.js/style.css，导致「代码改了、界面没变」的假象。
        self._send(200, body, content_type, {
            "cache-control": "no-store, no-cache, must-revalidate, max-age=0",
            "pragma": "no-cache",
            "expires": "0",
        })

    def handle_api_get(self, path: str) -> None:
        from .api import handle_get

        handle_get(self, path)

    def handle_api_post(self, path: str) -> None:
        from .api import handle_post

        handle_post(self, path)


SESSION_HEADERS = ("x-opencode-session", "x-session-id", "x-claude-code-session-id")

# 保留策略的后台间隔：24 小时（启动时另有一次立即执行）
RETENTION_INTERVAL_SECONDS = 24 * 3600


def client_session(headers, body) -> str:
    """提取会话标识用于**本地统计分组**（不参与上游协议）。

    优先级：显式会话头 → 请求体 metadata.session_id → body.user。
    都没有时返回空串（统计里归入「(无会话)」），不伪造 ID。
    """
    for key in SESSION_HEADERS:
        try:
            value = headers.get(key) if headers is not None else None
        except Exception:
            value = None
        if value:
            return str(value).strip()[:64]
    if isinstance(body, dict):
        meta = body.get("metadata")
        if isinstance(meta, dict):
            for key in ("session_id", "sessionId"):
                if meta.get(key):
                    return str(meta[key]).strip()[:64]
        if body.get("user"):
            return str(body["user"]).strip()[:64]
    return ""


class _TimedLines:
    """包装上游行迭代器，记录**首行到达时刻**（用于 TTFT）。

    为什么用迭代器包装而不是读第一行的特殊分支：适配器（anthropic / openai /
    commandcode）都按行消费，包装后对所有协议一律生效，且不改变它们的逻辑。
    """

    def __init__(self, iterator) -> None:
        self._iterator = iterator
        self.first_at: float | None = None

    def __iter__(self):
        for line in self._iterator:
            if self.first_at is None:
                self.first_at = time.time()
            yield line


def resolve_port(config: dict) -> int:
    """取监听端口：port=0 表示让系统分配随机端口（测试/无冲突场景）。"""
    raw = config.get("port")
    return int(raw) if raw is not None else 8900


def create_server(bridge: Bridge) -> ThreadingHTTPServer:
    """构造并返回 ThreadingHTTPServer（不启动，便于测试注入随机端口）。"""
    handler = type("BoundBridgeHandler", (BridgeHandler,), {"bridge": bridge})
    host = bridge.config.get("host") or "127.0.0.1"
    port = resolve_port(bridge.config)
    return ThreadingHTTPServer((host, port), handler)


def port_has_listener(host: str, port: int) -> bool:
    """探测某地址端口是否已有监听者（用于给出准确的启动失败原因）。"""
    import socket

    if not port:
        return False
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.6)
            return sock.connect_ex((host, port)) == 0
    except Exception:
        return False


def port_error_message(host: str, port: int, exc: OSError) -> str:
    """把绑定失败翻译成人话：先确认是不是真被占用，再谈权限。

    Windows 上端口被占用可能表现为 10048（ADDRINUSE）或 10013（EACCES，
    与独占/动态端口段有关），因此不能只看 errno，必须实际探测一次。
    """
    errno = exc.errno
    winerror = getattr(exc, "winerror", None)
    if port_has_listener(host, port) or errno in (98, 48, 10048) or winerror == 10048:
        return (
            f"端口 {port} 已被占用，Model-Gate 无法启动。\n"
            f"  查看占用进程: netstat -ano | findstr :{port}\n"
            f"  结束占用进程: taskkill /F /PID <PID>\n"
            f"  或换端口: 改 bridge.config.json 的 port，或加参数 --port <新端口>"
        )
    if errno in (13, 10013) or winerror == 10013:
        return (f"无权限绑定 {host}:{port}（Windows 可能将该端口列入保留区间）。"
                f"请改用 1024 以上的空闲端口，或以管理员身份重试。")
    return f"代理启动失败: {exc}"


def _self_check(host: str, port: int) -> bool:
    """保留为薄封装，供 run_forever 调用（内部统一走 _port_free）。"""
    return _port_free(host, port)
    """bind 之后自检：端口是否只属于本进程。

    Windows 的 SO_REUSEADDR 可能让已被占用的端口也 bind 成功，
    仅靠 OSError 判断会漏掉「起得来但连不上」的半死状态。
    这里用「再 bind 一次同一端口」判定：能再 bind 说明之前没人占，
    不能则说明已被别的进程持有。
    """
    import socket as _socket

    probe = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    try:
        probe.bind((host, port))
        return True
    except OSError:
        return False
    finally:
        probe.close()


def _port_free(host: str, port: int) -> bool:
    """端口是否未被占用（只 bind，不 listen）。

    自连接探测不可用：探针自己 listen 之后 connect 必然成功，判定恒真。
    只 bind 才是可靠判据——端口若已被他人独占监听，bind 直接失败。
    """
    import socket as _socket

    probe = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    try:
        probe.bind((host, port))
        return True
    except OSError:
        return False
    finally:
        probe.close()


def _bind_available(config: dict, original: int, exc: OSError, *,
                    attempts: int = 20) -> int:
    """配置端口不可用时向后寻找可用端口；返回新端口，全部失败抛原异常。

    GUI 与 --no-gui 共用：桌面/命令行版都以「服务能起来」优先，
    客户端可经 /health 获知真实端口。
    """
    for candidate in range(original + 1, original + 1 + attempts):
        if candidate <= 1024:
            continue
        if _port_free("127.0.0.1", candidate):
            config["port"] = candidate
            print(f"[model-gate] 端口 {original} 不可用（{type(exc).__name__}），"
                  f"已自动改用 {candidate}")
            return candidate
    raise exc


def run_forever(bridge: Bridge, *, allow_port_fallback: bool = True) -> None:
    """启动代理并阻塞运行；端口占用/无权限时给出可操作提示（对齐 Node 版）。"""
    host = bridge.config.get("host") or "127.0.0.1"
    port = resolve_port(bridge.config)
    server = None
    try:
        # 先判可用性、再绑定。顺序不能反：本进程一旦已在监听，探测用的 bind
        # 必然失败，于是「空闲端口」也会被判成被占用，白白漂移一个端口 ——
        # 用户按配置里的端口接入就会连不上。
        if allow_port_fallback and not _port_free(host, port):
            raise OSError(10048, f"port {port} already in use")
        server = create_server(bridge)
    except OSError as exc:
        if not allow_port_fallback:
            for line in port_error_message(host, port, exc).splitlines():
                print(f"[model-gate] {line}")
            raise SystemExit(1) from exc
        try:
            if server is not None:
                server.server_close()
        except Exception:
            pass
        try:
            _bind_available(bridge.config, port, exc)
            port = resolve_port(bridge.config)
            server = create_server(bridge)
        except OSError as exc2:
            for line in port_error_message(host, port, exc2).splitlines():
                print(f"[model-gate] {line}")
            raise SystemExit(1) from exc2

    bridge.start_background_tasks()
    bridge.note_active_port(port)
    # 横幅走 log()：exe 是无控制台程序，只有文件日志能让用户确认「启动成功」
    log(f"已启动 http://{host}:{port}/v1")
    for up in bridge.upstreams:
        suffix = "  [默认]" if up["default"] else ""
        session = "  会话头注入" if up["sessionHeader"] and up["protocol"] == "openai" else ""
        log(f"  上游 {up['name']:<10} {up['protocol']:<9} {up['baseUrl']}{suffix}{session}")
    log(f"声明的模型: {', '.join(model_list(bridge.config, bridge.upstreams)) or '(未配置)'}")
    log(f"thinking 回传: {'开启' if bridge.thinking_cache.enabled else '关闭'}"
        f"（缓存上限 {bridge.thinking_cache.max_size} 条）")
    log(f"用量已落库: {bridge.db.path}")

    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        print("\n[model-gate] 已停止")
    finally:
        server.server_close()
        bridge.close()
