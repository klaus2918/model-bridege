# -*- coding: utf-8 -*-
"""配置读写：兼容既有 bridge.config.json，同时把配置落到 SQLite settings。

契约（向后兼容 Node 版 model-bridge）：
- 配置文件字段：port / host / defaultModel / models / maxTokens / timeoutSeconds /
  accessLog / modelsCacheSeconds / aliases / thinking / tools / envFile /
  upstreams{} / upstream{}（旧的单上游形态）
- 上游字段：protocol / baseUrl / apiKey / apiKeyEnv / apiKeyFile / anthropicVersion /
  default / fetchModels / autoModels / models / modelMap / sessionHeader /
  cliVersion / cliEnvironment / modelCatalog / modelCatalogPlan / pauseTurn / cmdZdr
- 密钥解析优先级：apiKeyEnv → apiKeyFile → apiKey（显式空串表示「不发鉴权头」）→ 占位 any-key
- 保存时双写：配置文件为对外契约，SQLite settings 为运行时事实源。
"""
from __future__ import annotations

import copy
import json
import os
import re
import tempfile
from pathlib import Path
from typing import Optional

from .util import access_log_enabled, log, set_access_log

DEFAULT_CONFIG = {
    "port": 8900,
    "host": "127.0.0.1",
    "defaultModel": "deepseek-flash",
    "models": [],
    "maxTokens": 8192,
    "timeoutSeconds": 600,
    "accessLog": True,
    "modelsCacheSeconds": 300,
    "aliases": {},
    "thinking": {"passthrough": True, "cacheSize": 500},
    "tools": {"downgradeForcedChoice": True},
    "upstreams": {},
}

CC_PLAN_RANK = {"go": 1, "goat": 2, "pro": 3, "max": 4}
CC_PLAN_LABEL = {"go": "Go", "goat": "GOAT", "pro": "Pro", "max": "Max"}


def expand_home(p: str) -> str:
    return str(Path(p).expanduser()) if str(p).startswith("~") else str(p)


def default_config_path() -> Path:
    """配置文件位置：开发态在项目根；exe 冻结后必须落在 exe 同目录
    （否则会被写进 PyInstaller 的临时解包目录，退出即丢）。"""
    import sys

    if getattr(sys, "frozen", False):  # pragma: no cover - PyInstaller 场景
        return Path(sys.executable).resolve().parent / "bridge.config.json"
    return Path(__file__).resolve().parent.parent / "bridge.config.json"


def default_db_path() -> Path:
    """数据落在 exe 同目录 data/modelgate.db（与参考工程一致）。"""
    import sys

    if getattr(sys, "frozen", False):  # pragma: no cover - PyInstaller 场景
        base = Path(sys.executable).resolve().parent
    else:
        base = Path.cwd()
    return base / "data" / "modelgate.db"


# 记录「由 env 文件注入」的变量名：重载时只覆盖这些，
# 不会篡改用户在 shell 里显式设置的值。
_ENV_FILE_VARS: set[str] = set()


def env_file_path(config: dict) -> Path | None:
    """解析 envFile 配置，返回实际路径（false / 空则 None）。"""
    spec = config.get("envFile", True)
    if spec is False:
        return None
    raw = spec if isinstance(spec, str) else (Path.home() / ".jcode" / ".env")
    return Path(expand_home(str(raw)))


def load_env_file(config: dict, *, refresh: bool = False) -> None:
    """从 KEY=VALUE 格式文件加载环境变量，供上游 apiKeyEnv 引用。

    - 默认只在变量尚未存在时写入，**不覆盖**已有环境变量。
    - ``refresh=True``：额外覆盖「上次由本函数从 env 文件注入」的变量 ——
      对应「改了 ~/.jcode/.env 里的密钥后点『重新载入』」的场景，免重启。
      仍然不会篡改用户在 shell 里显式设置的值。

    默认读 ~/.jcode/.env，config.envFile 可改路径，设 false 则禁用；
    文件不存在不是错误。
    """
    # 与 Node 版 model-bridge 语义一致：缺省读 ~/.jcode/.env，
    # 只有显式写 "envFile": false 才禁用。早期写成默认 False 导致
    # 所有依赖 apiKeyEnv 的上游（如 go → OPENCODE_API_KEY）拿不到 key。
    target = env_file_path(config)
    if target is None:
        return
    try:
        text = target.read_text(encoding="utf-8")
    except OSError:
        return
    for line in text.splitlines():
        m = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$", line)
        if not m:
            continue
        key, value = m.group(1), m.group(2)
        if value == "":
            continue
        if key in os.environ and not (refresh and key in _ENV_FILE_VARS):
            continue
        os.environ[key] = value
        _ENV_FILE_VARS.add(key)


def write_env_var(config: dict, name: str, value: str) -> Path:
    """把 ``KEY=VALUE`` 写进 env 文件（保留其它行与注释），返回文件路径。

    安全边界：这里写的是**专用密钥文件**（默认 ``~/.jcode/.env``），与
    ``bridge.config.json``（对外契约，可能被分享/提交）严格区分；
    调用方不得把 ``value`` 写进日志或响应。

    存在即替换该键（只替换第一处），不存在则追加。
    """
    target = env_file_path(config)
    if target is None:
        raise ValueError("配置里 envFile=false，未启用 env 文件，无法写入")
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        existing = target.read_text(encoding="utf-8")
    except OSError:
        existing = ""

    lines: list[str] = []
    replaced = False
    for line in existing.splitlines():
        match = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=", line)
        if match and match.group(1) == name:
            if not replaced:
                lines.append(f"{name}={value}")
                replaced = True
            continue  # 重复的同名行一并去掉，只留一处
        lines.append(line)
    if not replaced:
        lines.append(f"{name}={value}")

    target.write_text("\n".join(lines).rstrip("\n") + "\n", encoding="utf-8")
    return target


def read_key_file(file: str) -> str | None:
    """从凭据文件读取 apiKey 字段（CommandCode 的 auth.json 就是这种形态）。

    只读不写：值仅驻内存，不落盘、不进日志；文件缺失或字段为空返回 None。
    """
    try:
        data = json.loads(Path(expand_home(file)).read_text(encoding="utf-8"))
        key = data.get("apiKey")
        return key if isinstance(key, str) and key else None
    except Exception:
        return None


def resolve_api_key(u: dict) -> tuple[str, str]:
    """返回 (key, source)。source ∈ env | file | literal | none。"""
    env_name = u.get("apiKeyEnv")
    if env_name and os.environ.get(env_name):
        return os.environ[env_name], "env"
    if u.get("apiKeyFile"):
        from_file = read_key_file(u["apiKeyFile"])
        if from_file:
            return from_file, "file"
    if u.get("apiKey") is not None:
        return u["apiKey"], "literal"
    return "any-key", "none"


def _base_url_is_local(url: str) -> bool:
    """地址（可能缺协议）是否指向本机/内网：这类补 http://，其余补 https://。

    为什么区分：把 ``127.0.0.1:15721`` 补成 https 会直接把本地桥接打死；
    把 ``api.deepseek.com`` 补成 http 则明文出网。两边都不能一刀切。
    """
    host = url.lstrip("/").split("/")[0]
    if host.startswith("["):            # IPv6 字面量 [::1]:8080
        host = host[1:].split("]")[0]
    else:
        host = host.split(":")[0]
    host = host.lower()
    if host in {"localhost", "::1", "0.0.0.0"}:
        return True
    if host.startswith(("127.", "192.168.", "10.")):
        return True
    return bool(re.match(r"^172\.(1[6-9]|2\d|3[01])\.", host))


def ensure_url_scheme(raw) -> str:
    """缺协议就补，只动协议、不裁剪路径。

    单独抽出来是给「保存前规范化」用的：面板会把 ``api.deepseek.com/v1`` 这类地址
    补全后再提交，直连 API 的调用方也应得到同样结果，否则会退化成
    「配置存进去了、请求全部连不上」这种最难查的故障。
    """
    url = str(raw if raw is not None else "").strip()
    if url and not re.match(r"^[a-z][a-z0-9+.\-]*://", url, re.I):
        url = ("http://" if _base_url_is_local(url) else "https://") + url.lstrip("/")
    return url


def _normalize_base_url(raw) -> str:
    url = ensure_url_scheme(raw if raw is not None else "http://127.0.0.1:15721")
    while url.endswith("/"):
        url = url[:-1]
    if url.endswith("/v1"):
        url = url[: -len("/v1")]
    return url


def infer_protocol(u: dict) -> str:
    base = str(u.get("baseUrl") or "")
    if u.get("protocol"):
        return u["protocol"]
    if "opencode.ai" in base:
        return "openai"
    if "commandcode.ai" in base:
        return "commandcode"
    return "anthropic"


def command_code_roots() -> list[str]:
    home = str(Path.home())
    return [
        str(Path(home) / "AppData" / "Roaming" / "npm" / "node_modules" / "command-code"),
        str(Path(home) / ".npm-global" / "lib" / "node_modules" / "command-code"),
        "/usr/local/lib/node_modules/command-code",
        "/usr/lib/node_modules/command-code",
    ]


def cc_min_plan_rank(text) -> int:
    t = str(text or "").strip().lower()
    if t.startswith("max"):
        return CC_PLAN_RANK["max"]
    if t.startswith("pro"):
        return CC_PLAN_RANK["pro"]
    if t.startswith("goat"):
        return CC_PLAN_RANK["goat"]
    if t.startswith("go"):
        return CC_PLAN_RANK["go"]
    return 0


def command_code_catalog_entries(spec) -> list[dict]:
    """上游权威模型目录（每项带最低套餐档位），来自 CLI 包内 models.md。"""
    if spec is False:
        return []
    if spec and spec != "auto":
        files = [str(spec)]
    else:
        files = [
            str(Path(r) / "dist" / "bundled" / "command-code-knowledge" / "reference" / "models.md")
            for r in command_code_roots()
        ]
    for file in files:
        try:
            text = Path(file).read_text(encoding="utf-8")
        except OSError:
            continue
        entries: list[dict] = []
        seen: set[str] = set()
        for line in text.splitlines():
            m = re.match(r"^\|\s*`([^`]+)`\s*\|(.*)\|\s*$", line)
            if not m:
                continue
            model_id = m.group(1)
            if model_id in seen:
                continue
            seen.add(model_id)
            cols = m.group(2).split("|")  # 名称 | 上下文 | 思考档 | 价格 | Min plan | 适用
            min_plan = cols[4] if len(cols) > 4 else ""
            entries.append({"id": model_id, "minPlanRank": cc_min_plan_rank(min_plan)})
        if entries:
            return entries
    return []


COMPAT_PROFILES = ("openai", "opencode", "anthropic", "commandcode", "deepseek")


_profile_registry_warned = False


def _compat_preset(profile: str, protocol: str, base_url: str) -> dict:
    """按 provider 预设生成兼容能力；任何字段都可由 compat 覆盖。

    数据源为共享注册表 `provider_profiles.json`（与 Rust 端一致）；
    注册表缺失/损坏时退回内置实现，保证服务可用。
    """
    global _profile_registry_warned
    try:
        from .provider_profiles import resolve_compat
        return resolve_compat(profile, protocol, base_url)
    except Exception as exc:  # noqa: BLE001
        if not _profile_registry_warned:
            _profile_registry_warned = True
            log(f"[model-gate] provider 注册表不可用（{exc}），退回内置预设")
        return _compat_preset_builtin(profile, protocol, base_url)


def _compat_preset_builtin(profile: str, protocol: str, base_url: str) -> dict:
    """内置预设实现（兜底副本，行为冻结；注册表以 scripts/provider_profiles_check.py 对照）。"""
    host = str(base_url or "").lower()
    effective = str(profile or "").lower()
    if not effective:
        if protocol == "commandcode" or "commandcode.ai" in host:
            effective = "commandcode"
        elif "api.deepseek.com" in host:
            effective = "deepseek"
        elif "opencode.ai" in host:
            effective = "opencode"
        else:
            effective = protocol
    compat = {
        "profile": effective or "openai",
        "auth": {"scheme": "bearer", "header": "authorization", "prefix": "Bearer "},
        "session": {"header": "", "idStyle": "hex32"},
        "headers": {},
        "endpoint": {"chat": "/v1/chat/completions", "models": "/v1/models"},
        "thinking": {"passthrough": False, "field": "reasoning_content"},
        "toolChoice": {"downgradeForced": False},
        "limits": {},
        "zdr": False,
        "versionHeader": "",
        "environmentHeader": "",
    }
    if effective == "opencode":
        # opencode / zen：缺少会话头会被上游 400 拒绝；空 key 表示不鉴权。
        compat["session"] = {"header": "x-opencode-session", "idStyle": "hex32"}
    elif effective == "commandcode":
        compat["auth"] = {"scheme": "bearer", "header": "authorization", "prefix": "Bearer "}
        compat["session"] = {"header": "x-session-id", "idStyle": "sess16"}
        compat["headers"] = {"user-agent": "cli"}
        compat["endpoint"] = {"chat": "/alpha/generate", "models": "/v1/models"}
        compat["toolChoice"] = {"downgradeForced": True}
        compat["zdr"] = True
        compat["versionHeader"] = "x-command-code-version"
        compat["environmentHeader"] = "x-cli-environment"
        compat["limits"] = {"maxOutputTokens": 64000}
    elif effective == "anthropic":
        compat["auth"] = {"scheme": "both", "header": "authorization", "prefix": "Bearer "}
        compat["endpoint"] = {"chat": "/v1/messages", "models": "/v1/models"}
        compat["thinking"] = {"passthrough": True, "field": "reasoning_content"}
        compat["toolChoice"] = {"downgradeForced": True}
        if "deepseek" in host:
            compat["profile"] = "deepseek"
            compat["limits"] = {"maxOutputTokens": 384 * 1024, "maxContextTokens": 1024 * 1024}
    elif effective == "deepseek":
        compat["thinking"] = {"passthrough": True, "field": "reasoning_content"}
        compat["limits"] = {"maxOutputTokens": 384 * 1024, "maxContextTokens": 1024 * 1024}
        if protocol == "anthropic":
            compat["auth"] = {"scheme": "both", "header": "authorization", "prefix": "Bearer "}
            compat["endpoint"] = {"chat": "/v1/messages", "models": "/v1/models"}
            compat["toolChoice"] = {"downgradeForced": True}
    return compat


def compat_for(protocol: str, base_url: str, u: dict, config: dict | None = None) -> dict:
    """解析上游兼容能力：预设 → 旧平铺字段 → 显式 compat → 全局开关。"""
    raw_compat = u.get("compat") if isinstance(u.get("compat"), dict) else {}
    profile = raw_compat.get("profile") or u.get("profile") or ""
    compat = _compat_preset(str(profile), protocol, base_url)

    # 旧版平铺字段：兼容输入，不覆盖显式 compat。
    if isinstance(u.get("sessionHeader"), bool):
        if u["sessionHeader"]:
            compat["session"]["header"] = compat["session"].get("header") or "x-opencode-session"
        else:
            compat["session"]["header"] = ""
    if isinstance(u.get("cmdZdr"), bool):
        compat["zdr"] = u["cmdZdr"]
    if isinstance(u.get("thinkingPassthrough"), bool):
        compat["thinking"]["passthrough"] = u["thinkingPassthrough"]
    if isinstance(u.get("downgradeForcedChoice"), bool):
        compat["toolChoice"]["downgradeForced"] = u["downgradeForcedChoice"]

    auth = raw_compat.get("auth")
    if isinstance(auth, str):
        compat["auth"]["scheme"] = auth.lower()
    elif isinstance(auth, dict):
        for key, target in (("scheme", "scheme"), ("header", "header"), ("prefix", "prefix")):
            if isinstance(auth.get(key), str):
                compat["auth"][target] = auth[key].lower() if key == "scheme" else auth[key]

    session = raw_compat.get("session")
    if isinstance(session, bool):
        if session:
            compat["session"]["header"] = compat["session"].get("header") or "x-opencode-session"
        else:
            compat["session"]["header"] = ""
    elif isinstance(session, dict):
        enabled = session.get("enabled", True)
        if enabled:
            if isinstance(session.get("header"), str):
                compat["session"]["header"] = session["header"]
            if not compat["session"].get("header"):
                compat["session"]["header"] = "x-opencode-session"
        else:
            compat["session"]["header"] = ""
        if isinstance(session.get("idStyle"), str):
            compat["session"]["idStyle"] = session["idStyle"].lower()

    thinking = raw_compat.get("thinking")
    if isinstance(thinking, bool):
        compat["thinking"]["passthrough"] = thinking
    elif isinstance(thinking, dict):
        if isinstance(thinking.get("passthrough"), bool):
            compat["thinking"]["passthrough"] = thinking["passthrough"]
        if isinstance(thinking.get("field"), str):
            compat["thinking"]["field"] = thinking["field"]

    choice = raw_compat.get("toolChoice")
    if isinstance(choice, bool):
        compat["toolChoice"]["downgradeForced"] = choice
    elif isinstance(choice, dict) and isinstance(choice.get("downgradeForced"), bool):
        compat["toolChoice"]["downgradeForced"] = choice["downgradeForced"]

    headers = raw_compat.get("headers")
    if isinstance(headers, dict):
        for key, value in headers.items():
            if isinstance(value, (str, int, float, bool)):
                compat["headers"][str(key).lower()] = str(value)

    endpoint = raw_compat.get("endpoint")
    if isinstance(endpoint, dict):
        if isinstance(endpoint.get("chat"), str):
            compat["endpoint"]["chat"] = endpoint["chat"]
        if isinstance(endpoint.get("models"), str):
            compat["endpoint"]["models"] = endpoint["models"]

    limits = raw_compat.get("limits")
    if isinstance(limits, dict):
        for key in ("maxOutputTokens", "maxContextTokens"):
            value = _to_int(limits.get(key))
            if value and value > 0:
                compat["limits"][key] = value

    if isinstance(raw_compat.get("zdr"), bool):
        compat["zdr"] = raw_compat["zdr"]
    if "versionHeader" in raw_compat:
        compat["versionHeader"] = str(raw_compat.get("versionHeader") or "")
    if "environmentHeader" in raw_compat:
        compat["environmentHeader"] = str(raw_compat.get("environmentHeader") or "")

    # 能力元数据：注册表 profile → 上游 compat.capabilities（模型级在 modelOverrides 中另行合并）。
    caps = raw_compat.get("capabilities")
    if isinstance(caps, dict):
        try:
            from .provider_profiles import merge_capabilities
            compat["capabilities"] = merge_capabilities(compat.get("profile"), compat.get("capabilities"), caps)
        except Exception:  # noqa: BLE001 注册表不可用时保留已有能力层
            pass

    # 全局开关作为最终约束。
    thinking_cfg = (config or {}).get("thinking")
    if isinstance(thinking_cfg, dict) and thinking_cfg.get("passthrough") is False:
        compat["thinking"]["passthrough"] = False
    tools_cfg = (config or {}).get("tools")
    if isinstance(tools_cfg, dict) and tools_cfg.get("downgradeForcedChoice") is False:
        compat["toolChoice"]["downgradeForced"] = False
    return compat


def model_override_for(up: dict, model_id: str) -> tuple[str | None, dict | None]:
    """命中模型级覆盖：精确 > 最长通配；只支持单个 `*`（多个视为非法不参与匹配）。"""
    overrides = up.get("modelOverrides")
    if not isinstance(overrides, dict) or not model_id:
        return None, None
    best = None
    for order, (pattern, value) in enumerate(overrides.items()):
        if not isinstance(pattern, str) or not isinstance(value, dict):
            continue
        stars = pattern.count("*")
        if stars == 0:
            if pattern != model_id:
                continue
            score = (1 << 30, -order)
        elif stars == 1:
            prefix, _, suffix = pattern.partition("*")
            if not (model_id.startswith(prefix) and model_id.endswith(suffix)
                    and len(model_id) >= len(prefix) + len(suffix)):
                continue
            score = (len(pattern) - 1, -order)
        else:
            continue
        candidate = (score, pattern, value)
        if best is None or candidate[0] > best[0]:
            best = candidate
    if best is None:
        return None, None
    return best[1], best[2]


def resolve_model_limits(up: dict, model_id: str) -> dict:
    """上游 compat.limits 叠加模型级覆盖后的生效 limits。"""
    compat = up.get("compat") or {}
    limits = dict(compat.get("limits") or {})
    _, override = model_override_for(up, model_id)
    if override and isinstance(override.get("limits"), dict):
        for key in ("maxOutputTokens", "maxContextTokens"):
            value = _to_int(override["limits"].get(key))
            if value and value > 0:
                limits[key] = value
    return limits


def resolve_model_capabilities(up: dict, model_id: str) -> dict:
    """模型级能力：上游 compat.capabilities（已含 profile 层）→ 模型覆盖；只影响展示。"""
    compat = up.get("compat") or {}
    caps = dict(compat.get("capabilities") or {})
    _, override = model_override_for(up, model_id)
    if override and isinstance(override.get("capabilities"), dict):
        for key, value in override["capabilities"].items():
            if key in CAPABILITY_KEYS and isinstance(value, bool):
                caps[key] = value
    return caps


def normalize_upstream(name: str, u: dict, config: dict | None = None) -> dict:
    if not isinstance(u, dict):
        u = {}
    protocol = infer_protocol(u)
    base_url = _normalize_base_url(u.get("baseUrl"))
    is_cc = protocol == "commandcode"
    compat = compat_for(protocol, base_url, u, config)
    api_key, api_key_source = resolve_api_key(u)
    declared = [m for m in (u.get("models") or []) if isinstance(m, str)]
    catalog = command_code_catalog_entries(u.get("modelCatalog", "auto" if is_cc else False))
    models_all: list[str] = []
    for mid in list(declared) + [e["id"] for e in catalog]:
        if mid not in models_all:
            models_all.append(mid)
    return {
        "name": name,
        "baseUrl": base_url,
        "apiKey": api_key,
        "apiKeySource": api_key_source,
        "protocol": protocol,
        "anthropicVersion": u.get("anthropicVersion", "2023-06-01"),
        "models": list(models_all),
        "modelsAll": list(models_all),
        "catalogEntries": catalog,
        "default": u.get("default") is True,
        "sessionHeader": u.get("sessionHeader", bool(compat["session"].get("header"))),
        "fetchModels": u.get("fetchModels", not is_cc),
        "autoModels": u.get("autoModels") is True,
        "modelMap": dict(u.get("modelMap") or {}),
        "cliVersion": u.get("cliVersion", "auto"),
        "cliEnvironment": u.get("cliEnvironment", "production"),
        "apiKeyFile": u.get("apiKeyFile"),
        # apiKeyEnv 必须保留：提示与配置回写都要靠它指出「该改哪个环境变量」，
        # 只留 apiKeyFile 会造成不对称（历史缺陷：提示只能泛泛说 apiKeyEnv）。
        "apiKeyEnv": u.get("apiKeyEnv"),
        "modelCatalog": u.get("modelCatalog", "auto" if is_cc else False),
        "modelCatalogPlan": u.get("modelCatalogPlan", "auto" if is_cc else False),
        "modelOverrides": dict(u["modelOverrides"]) if isinstance(u.get("modelOverrides"), dict) else {},
        "pauseTurn": u.get("pauseTurn", is_cc),
        "cmdZdr": u.get("cmdZdr", compat["zdr"]),
        "compat": compat,
        "modelPlan": None,
        "modelPlanError": None,
    }


def load_config(path: str | os.PathLike) -> dict:
    """读取配置文件并归一化；文件不存在时返回默认配置（可后续保存生成）。"""
    p = Path(path)
    try:
        raw = json.loads(p.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return copy.deepcopy(DEFAULT_CONFIG)
    except Exception as exc:
        raise SystemExit(f"[model-gate] 无法读取配置 {p}: {exc}") from exc
    config = copy.deepcopy(DEFAULT_CONFIG)
    config.update({k: v for k, v in raw.items() if v is not None})
    for key in ("thinking", "tools"):
        if isinstance(raw.get(key), dict):
            config[key] = {**DEFAULT_CONFIG[key], **raw[key]}
    return _coerce_loaded(config)


def _coerce_loaded(config: dict) -> dict:
    """加载即净化：手工改坏的配置不让服务起不来（字段降级到可用形态）。"""
    models = config.get("models")
    if not isinstance(models, list):
        config["models"] = [models] if isinstance(models, str) else []
    else:
        config["models"] = [m for m in models if isinstance(m, str)]

    if not isinstance(config.get("aliases"), dict):
        config["aliases"] = {}

    if not isinstance(config.get("upstreams"), dict):
        # 旧的单上游字段或坏结构 → 交给 upstreams_of 走 legacy 分支
        config.pop("upstreams", None)

    if not isinstance(config.get("host"), str) or not config["host"]:
        config["host"] = DEFAULT_CONFIG["host"]

    for key, default in (("port", 8900), ("maxTokens", 8192),
                         ("timeoutSeconds", 600), ("modelsCacheSeconds", 300)):
        value = config.get(key)
        if isinstance(value, bool):
            config[key] = default
            continue
        try:
            number = int(float(value)) if isinstance(value, str) else int(value)
        except (TypeError, ValueError):
            number = default
        if key == "port" and not 0 <= number <= 65535:
            number = default
        if key != "port" and number <= 0:
            number = default
        config[key] = number

    for key in ("accessLog",):
        value = config.get(key)
        if value is not None and not isinstance(value, bool):
            config[key] = bool(value)

    if not isinstance(config.get("defaultModel"), str):
        config["defaultModel"] = str(config.get("defaultModel") or "")
    return config


def save_config(path: str | os.PathLike, config: dict) -> None:
    """原子写回配置文件：临时文件 + 替换，避免半截配置。"""
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(config, ensure_ascii=False, indent=2) + "\n"
    fd, tmp = tempfile.mkstemp(dir=str(p.parent), prefix=p.name, suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(payload)
        os.replace(tmp, p)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def upstreams_of(config: dict) -> list[dict]:
    """归一化全部上游；兼容旧的单上游字段 config.upstream。

    对畸形输入（upstreams 是数组、条目是字符串等）一律降级为可用形态，
    保证「读到坏配置」不会让服务起不来或让保存接口 500。
    """
    declared = (config or {}).get("upstreams") if isinstance(config, dict) else None
    if isinstance(declared, dict) and declared:
        items = [(str(k), v) for k, v in declared.items()]
    else:
        legacy = (config or {}).get("upstream") if isinstance(config, dict) else None
        items = [("default", legacy if isinstance(legacy, dict) else {})]
    return [normalize_upstream(name, u, config) for name, u in items]


TOP_LEVEL_KEYS = {
    "port", "host", "defaultModel", "models", "maxTokens", "timeoutSeconds",
    "accessLog", "modelsCacheSeconds", "aliases", "thinking", "tools", "envFile",
    "upstreams", "upstream", "retentionDays", "pricing",
}

UPSTREAM_KEYS = {
    "protocol", "baseUrl", "apiKey", "apiKeyEnv", "apiKeyFile", "anthropicVersion",
    "default", "fetchModels", "autoModels", "models", "modelMap", "sessionHeader",
    "cliVersion", "cliEnvironment", "modelCatalog", "modelCatalogPlan", "pauseTurn",
    "cmdZdr", "compat", "profile", "name", "thinkingPassthrough",
    "downgradeForcedChoice", "modelOverrides",
}

COMPAT_KEYS = {
    "profile", "auth", "session", "thinking", "toolChoice", "headers", "endpoint",
    "limits", "zdr", "versionHeader", "environmentHeader", "catalog", "capabilities",
}

CAPABILITY_KEYS = ("stream", "tools", "vision", "thinking", "cache", "systemPrompt")

_HEADER_TOKEN_RE = re.compile(r"^[A-Za-z0-9._-]+$")


def _diag(level: str, code: str, path: str, message: str, hint: str = "") -> dict:
    return {"level": level, "code": code, "path": path, "message": message, "hint": hint}


def validate_config(config: dict) -> list[dict]:
    """结构化配置诊断：error 阻断保存，warn/info 只提示。

    与 Rust 内核的 Config::diagnostics 对齐：同一份配置在两种运行时
    得到同一组错误（字段名、严重级别一致）。
    """
    issues: list[dict] = []
    if not isinstance(config, dict):
        return [_diag("error", "invalid_config", "", "配置必须是 JSON 对象")]
    add = lambda *args, **kwargs: issues.append(_diag(*args, **kwargs))

    for key in config:
        if str(key).startswith("_"):
            continue  # `_doc` / `_keys` 等说明字段
        if key not in TOP_LEVEL_KEYS:
            add("warn", "unknown_field", key, f"未知顶层字段「{key}」，不会生效",
                "检查拼写，或改用 compat / upstreams 下的字段")

    host = config.get("host")
    if host is not None and not isinstance(host, str):
        add("error", "invalid_host", "host", "host 必须是字符串")
    elif isinstance(host, str) and not host.strip():
        add("error", "invalid_host", "host", "监听地址不能为空", "本机使用 127.0.0.1")

    port = _to_int(config.get("port"))
    if config.get("port") is not None and (port is None or not 0 <= port <= 65535):
        add("error", "invalid_port", "port", f"端口「{config.get('port')}」需为 0–65535 的整数")

    for key in ("maxTokens", "timeoutSeconds", "modelsCacheSeconds", "retentionDays"):
        value = config.get(key)
        if value is not None and _to_int(value) is None:
            add("error", "invalid_number", key, f"{key} 必须是数字（当前 {value!r}）")
    max_tokens = _to_int(config.get("maxTokens")) or 8192
    if max_tokens <= 0:
        add("error", "invalid_max_tokens", "maxTokens", "maxTokens 必须大于 0")
    timeout = _to_int(config.get("timeoutSeconds"))
    if timeout is not None and timeout < 1:
        add("warn", "invalid_timeout", "timeoutSeconds", "超时小于 1 秒，将按 600 秒运行",
            "建议 60–3600 秒")
    if _to_int(config.get("modelsCacheSeconds")) is not None and _to_int(config.get("modelsCacheSeconds")) < 0:
        add("error", "invalid_number", "modelsCacheSeconds", "缓存秒数不能为负")
    if _to_int(config.get("retentionDays")) is not None and _to_int(config.get("retentionDays")) < 0:
        add("error", "invalid_number", "retentionDays", "保留天数不能为负")

    default_model = config.get("defaultModel")
    if default_model is not None and not isinstance(default_model, str):
        add("error", "invalid_default_model", "defaultModel", "defaultModel 必须是字符串")
    models = config.get("models")
    if models is not None:
        if not isinstance(models, list):
            add("error", "invalid_models", "models", "models 必须是数组")
        elif any(not isinstance(m, str) for m in models):
            add("error", "invalid_models", "models", "models 每一项都必须是字符串")
    aliases = config.get("aliases")
    if aliases is not None and not isinstance(aliases, dict):
        add("error", "invalid_aliases", "aliases", "aliases 必须是对象（别名 = 真实模型）")
    elif isinstance(aliases, dict):
        for key, value in aliases.items():
            if not isinstance(key, str) or not isinstance(value, str):
                add("error", "invalid_aliases", "aliases",
                    "aliases 的键与值都必须是字符串")
                break
    for key in ("thinking", "tools"):
        if config.get(key) is not None and not isinstance(config.get(key), dict):
            add("error", "invalid_object", key, f"{key} 必须是对象")

    declared = config.get("upstreams")
    if declared is not None and not isinstance(declared, dict):
        add("error", "invalid_upstreams", "upstreams", "upstreams 必须是对象（上游名 → 上游配置）")
        return issues
    declared = declared if isinstance(declared, dict) else {}
    if not declared and not isinstance(config.get("upstream"), dict):
        add("error", "no_upstream", "upstreams", "至少要配置一个上游",
            "在 upstreams 下添加一个对象")

    try:
        normalized = upstreams_of(config) if isinstance(config, dict) else []
    except Exception:
        normalized = []

    model_owner: dict[str, str] = {}
    for name, up in declared.items():
        prefix = f"upstreams.{name}"
        if not isinstance(up, dict):
            add("error", "invalid_upstream", prefix, f"上游 {name!r} 的配置必须是对象")
            continue
        for key in up:
            if key not in UPSTREAM_KEYS:
                add("warn", "unknown_field", f"{prefix}.{key}",
                    f"上游「{name}」的字段「{key}」未知，不会生效",
                    "检查拼写；provider 差异请写在 compat 下")
        base = up.get("baseUrl")
        if not isinstance(base, str) or not base.strip():
            add("error", "invalid_base_url", f"{prefix}.baseUrl",
                f"上游「{name}」没有填 Base URL", "例如 https://api.deepseek.com/v1")
        elif not re.match(r"^https?://", ensure_url_scheme(base), re.I):
            add("error", "invalid_base_url", f"{prefix}.baseUrl",
                f"上游「{name}」的 Base URL 无效：{base}", "例如 https://api.deepseek.com/v1")
        protocol = infer_protocol(up)
        if protocol not in ("openai", "anthropic", "commandcode"):
            add("error", "unsupported_protocol", f"{prefix}.protocol",
                f"上游「{name}」的协议「{protocol}」未实现",
                "可用 openai / anthropic / commandcode")
        if up.get("models") is not None and not isinstance(up["models"], list):
            add("error", "invalid_models", f"{prefix}.models", f"上游 {name!r} 的 models 必须是数组")
        if up.get("modelMap") is not None and not isinstance(up["modelMap"], dict):
            add("error", "invalid_model_map", f"{prefix}.modelMap", f"上游 {name!r} 的 modelMap 必须是对象")

        compat = up.get("compat")
        if compat is not None and not isinstance(compat, dict):
            add("error", "invalid_compat", f"{prefix}.compat", "compat 必须是对象",
                '例如 {"auth":{"scheme":"bearer"}}')
            compat = {}
        compat = compat or {}
        for key in compat:
            if key not in COMPAT_KEYS:
                add("warn", "unknown_compat_field", f"{prefix}.compat.{key}",
                    f"compat 字段「{key}」未知，不会生效", "可用字段见 provider-compat.md")
        for sub, allowed in (
            ("auth", {"scheme", "header", "prefix"}),
            ("session", {"enabled", "header", "idStyle"}),
            ("thinking", {"passthrough", "field"}),
            ("toolChoice", {"downgradeForced"}),
            ("headers", None),
            ("endpoint", {"chat", "models"}),
            ("limits", {"maxOutputTokens", "maxContextTokens"}),
            ("catalog", {"source", "path", "plan"}),
            ("capabilities", set(CAPABILITY_KEYS)),
        ):
            value = compat.get(sub)
            if value is None or allowed is None or not isinstance(value, dict):
                continue
            for key in value:
                if key not in allowed:
                    add("warn", "unknown_compat_field", f"{prefix}.compat.{sub}.{key}",
                        f"{sub} 字段「{key}」未知，不会生效")

        profile_hint = str(compat.get("profile") or up.get("profile") or "")
        if profile_hint:
            from .provider_profiles import is_known_profile, profile_ids
            if not is_known_profile(profile_hint):
                add("warn", "unknown_profile", f"{prefix}.compat.profile",
                    f"profile「{profile_hint}」不在注册表，按协议/域名兜底",
                    "可用：" + " / ".join(profile_ids()))

        caps_cfg = compat.get("capabilities")
        if caps_cfg is not None:
            if not isinstance(caps_cfg, dict):
                add("error", "invalid_capabilities", f"{prefix}.compat.capabilities",
                    "capabilities 必须是对象")
            else:
                for key, value in caps_cfg.items():
                    if key in CAPABILITY_KEYS and not isinstance(value, bool):
                        add("error", "invalid_capabilities",
                            f"{prefix}.compat.capabilities.{key}",
                            "能力值必须是布尔（true/false）")

        if protocol != "commandcode":
            compat_cfg = compat if isinstance(compat, dict) else {}
            for key in ("cliVersion", "cliEnvironment", "cmdZdr", "pauseTurn",
                        "modelCatalog", "modelCatalogPlan"):
                if key not in up:
                    continue
                generic = (
                    (key == "cliVersion" and compat_cfg.get("versionHeader"))
                    or (key == "cliEnvironment" and compat_cfg.get("environmentHeader"))
                )
                if not generic:
                    add("warn", "field_not_applicable", f"{prefix}.{key}",
                        f"字段「{key}」只对 commandcode 生效，上游「{name}」不会使用",
                        "如需通用行为，请改用 compat.headers / compat.session")

        # 解析后的能力检查
        parsed = next((u for u in normalized if u["name"] == str(name)), None)
        if parsed is None:
            continue
        c = parsed.get("compat") or {}
        profile = str(c.get("profile") or "")
        profile_ok = (
            (profile == "commandcode" and protocol == "commandcode")
            or (profile == "anthropic" and protocol == "anthropic")
            or (profile == "opencode" and protocol == "openai")
            or (profile == "deepseek" and protocol in ("openai", "anthropic"))
            or (profile == "openai" and protocol == "openai")
        )
        if not profile_ok:
            add("warn", "profile_protocol_mismatch", f"{prefix}.compat.profile",
                f"compat.profile={profile} 与 protocol={protocol} 不匹配，wire 行为仍按 protocol 执行",
                "检查 protocol；profile 只决定鉴权/会话/端点等默认能力")
        auth = c.get("auth") or {}
        scheme = str(auth.get("scheme") or "bearer")
        if scheme not in ("bearer", "x-api-key", "both", "none", "header"):
            add("error", "invalid_auth_scheme", f"{prefix}.compat.auth.scheme",
                f"鉴权方式「{scheme}」无效", "可用 bearer / x-api-key / both / none / header")
        session = c.get("session") or {}
        header = str(session.get("header") or "")
        if header and not _HEADER_TOKEN_RE.match(header):
            add("error", "invalid_header_name", f"{prefix}.compat.session.header",
                f"会话头名「{header}」不是合法的 HTTP 头名",
                "头名只能包含字母、数字和 - _ .")
        endpoint = c.get("endpoint") or {}
        chat = str(endpoint.get("chat") or "")
        if not chat.startswith("/"):
            add("error", "invalid_endpoint", f"{prefix}.compat.endpoint.chat",
                f"对话端点「{chat}」必须以 / 开头", "例如 /v1/chat/completions")
        if not str(endpoint.get("models") or "").startswith("/"):
            add("warn", "invalid_endpoint", f"{prefix}.compat.endpoint.models",
                f"模型端点「{endpoint.get('models')}」必须以 / 开头", "例如 /v1/models")
        limits = c.get("limits") or {}
        max_out = _to_int(limits.get("maxOutputTokens"))
        max_ctx = _to_int(limits.get("maxContextTokens"))
        if max_out and max_ctx and max_out > max_ctx:
            add("error", "invalid_limits", f"{prefix}.compat.limits",
                f"最大输出 {max_out} 大于上下文窗口 {max_ctx}",
                "输出上限应小于等于上下文窗口")
        if max_out and max_tokens > max_out:
            add("warn", "max_tokens_exceeds_upstream",
                f"{prefix}.compat.limits.maxOutputTokens",
                f"全局 maxTokens={max_tokens} 超过上游「{name}」上限 {max_out}，请求会被自动收敛",
                "下调 maxTokens，或确认该上游上限")

        overrides = up.get("modelOverrides")
        if overrides is not None:
            if not isinstance(overrides, dict):
                add("error", "invalid_model_overrides", f"{prefix}.modelOverrides",
                    f"上游「{name}」的 modelOverrides 必须是对象（模型 id → 覆盖项）")
            else:
                known_models = set(parsed.get("models") or [])
                for key, value in overrides.items():
                    path = f"{prefix}.modelOverrides.{key}"
                    if not isinstance(key, str) or not key:
                        add("error", "invalid_model_overrides", path,
                            "模型覆盖键必须是非空字符串")
                        continue
                    if key.count("*") > 1:
                        add("error", "multi_wildcard_override", path,
                            f"模型覆盖键「{key}」包含多个 *，不参与匹配",
                            "只支持一个 * 通配")
                        continue
                    if "*" not in key and known_models and key not in known_models:
                        add("warn", "model_override_unknown_model", path,
                            f"模型覆盖「{key}」不在上游「{name}」的模型清单里",
                            "检查模型 id，或先在上游 models 中声明")
                    if not isinstance(value, dict):
                        add("error", "invalid_model_overrides", path,
                            "模型覆盖值必须是对象（limits / capabilities）")
                        continue
                    if not value:
                        add("error", "invalid_model_overrides", path,
                            "模型覆盖值不能是空对象（至少包含 limits 或 capabilities）")
                        continue
                    for field in value:
                        if field not in ("limits", "capabilities"):
                            add("warn", "unknown_field", f"{path}.{field}",
                                f"模型覆盖字段「{field}」未知，不会生效")
                    limit_cfg = value.get("limits")
                    if limit_cfg is not None:
                        if not isinstance(limit_cfg, dict):
                            add("error", "invalid_model_overrides", f"{path}.limits",
                                "modelOverrides.limits 必须是对象")
                        else:
                            for field in limit_cfg:
                                if field not in ("maxOutputTokens", "maxContextTokens"):
                                    add("warn", "unknown_field", f"{path}.limits.{field}",
                                        f"limits 字段「{field}」未知，不会生效")
                            over_out = (_to_int(limit_cfg.get("maxOutputTokens"))
                                        if "maxOutputTokens" in limit_cfg else None)
                            over_ctx = (_to_int(limit_cfg.get("maxContextTokens"))
                                        if "maxContextTokens" in limit_cfg else None)
                            for field, number in (("maxOutputTokens", over_out),
                                                  ("maxContextTokens", over_ctx)):
                                if field in limit_cfg and (not number or number <= 0):
                                    add("error", "invalid_model_overrides",
                                        f"{path}.limits.{field}",
                                        "limits 值必须是正整数")
                            eff_out = over_out or max_out
                            eff_ctx = over_ctx or max_ctx
                            if eff_out and eff_ctx and eff_out > eff_ctx:
                                add("error", "invalid_limit_relation", f"{path}.limits",
                                    f"最大输出 {eff_out} 大于上下文窗口 {eff_ctx}",
                                    "输出上限应小于等于上下文窗口")
                    override_caps = value.get("capabilities")
                    if override_caps is not None:
                        if not isinstance(override_caps, dict):
                            add("error", "invalid_capabilities", f"{path}.capabilities",
                                "capabilities 必须是对象")
                        else:
                            for ck, cv in override_caps.items():
                                if ck not in CAPABILITY_KEYS:
                                    add("warn", "unknown_field",
                                        f"{path}.capabilities.{ck}",
                                        f"capabilities 字段「{ck}」未知，不会生效")
                                elif not isinstance(cv, bool):
                                    add("error", "invalid_capabilities",
                                        f"{path}.capabilities.{ck}",
                                        "能力值必须是布尔（true/false）")

        if (parsed.get("apiKeySource") == "none" and protocol != "commandcode"
                and scheme != "none" and not parsed.get("apiKeyEnv")
                and not parsed.get("apiKeyFile")):
            add("warn", "missing_credentials", f"{prefix}.apiKey",
                f"上游「{name}」没有配置密钥（将以空鉴权请求）",
                "设置 apiKeyEnv / apiKeyFile，或显式 apiKey 为空表示不鉴权")
        if parsed.get("apiKeyEnv") and "apiKey" in up:
            add("info", "key_precedence", f"{prefix}.apiKey",
                f"上游「{name}」同时配置了 apiKeyEnv 与明文 apiKey，环境变量优先",
                "只保留一种密钥来源更可控")
        if up.get("autoModels") is True and up.get("fetchModels") is False:
            add("warn", "ineffective_auto_models", f"{prefix}.autoModels",
                "autoModels=true 但 fetchModels=false：不会向上游拉取模型",
                "同时开启 fetchModels")
        for model in parsed.get("models") or []:
            if not isinstance(model, str):
                continue
            if model in model_owner and model_owner[model] != str(name):
                add("info", "duplicate_model", f"{prefix}.models",
                    f"模型「{model}」同时出现在「{model_owner[model]}」和「{name}」，按配置顺序取前者",
                    "如需固定路由，使用 /<上游名>/v1 前缀")
            else:
                model_owner.setdefault(model, str(name))

    if isinstance(default_model, str) and default_model.strip():
        known = set(model_list(config, normalized))
        if default_model not in known and default_model not in (aliases or {}):
            add("warn", "default_model_unserved", "defaultModel",
                f"默认模型「{default_model}」不在任何上游清单里",
                "补齐上游 models，或改用已声明的模型名")
    alias_map = aliases if isinstance(aliases, dict) else {}
    for alias, target in alias_map.items():
        if not isinstance(target, str):
            continue
        if target not in set(model_list(config, normalized)):
            add("warn", "alias_target_missing", f"aliases.{alias}",
                f"别名「{alias}」指向的模型「{target}」不在任何上游清单里",
                "补齐上游 models，或修正别名目标")
    return issues


def effective_upstreams(upstreams: list[dict]) -> list[dict]:
    """有效能力视图：面板展示“预设最终解析成了什么”，而不是猜。"""
    rows = []
    for up in upstreams:
        compat = up.get("compat") or {}
        rows.append({
            "name": up.get("name"),
            "protocol": up.get("protocol"),
            "profile": compat.get("profile"),
            "baseUrl": up.get("baseUrl"),
            "chatPath": (compat.get("endpoint") or {}).get("chat"),
            "modelsPath": (compat.get("endpoint") or {}).get("models"),
            "authScheme": (compat.get("auth") or {}).get("scheme"),
            "sessionHeader": (compat.get("session") or {}).get("header"),
            "sessionIdStyle": (compat.get("session") or {}).get("idStyle"),
            "thinkingPassthrough": (compat.get("thinking") or {}).get("passthrough"),
            "thinkingField": (compat.get("thinking") or {}).get("field"),
            "toolChoiceDowngrade": (compat.get("toolChoice") or {}).get("downgradeForced"),
            "maxOutputTokens": (compat.get("limits") or {}).get("maxOutputTokens"),
            "maxContextTokens": (compat.get("limits") or {}).get("maxContextTokens"),
            "zdr": compat.get("zdr"),
            "extraHeaders": compat.get("headers") or {},
            "models": up.get("models") or [],
            "default": bool(up.get("default")),
            "keySource": up.get("apiKeySource"),
            "keyConfigured": bool(up.get("apiKey")) and up.get("apiKeySource") != "none",
            "capabilities": compat.get("capabilities") or {},
            "modelOverrides": up.get("modelOverrides") or {},
        })
    return rows


def compat_headers(up: dict, *, session_id: str = "", cli_version: str = "") -> dict:
    """按 compat 生成出站请求头（探测与诊断复用，避免各协议各写一套）。

    这是“配置可解释”的一部分：面板测活发什么头，必须和真实代理一致。
    """
    compat = up.get("compat") or {}
    headers: dict = {}
    for key, value in (compat.get("headers") or {}).items():
        headers[str(key).lower()] = str(value)

    key = up.get("apiKey") or ""
    auth = compat.get("auth") or {}
    scheme = str(auth.get("scheme") or "").lower()
    if not scheme:
        # 未归一化的旧上游（手工构造/测试）：按协议回退到历史行为。
        scheme = "both" if up.get("protocol") == "anthropic" else "bearer"
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

    session = compat.get("session") or {}
    header = session.get("header")
    if header is None:
        # 旧字段回退：sessionHeader=true 时按 opencode 语义注入。
        header = "x-opencode-session" if up.get("sessionHeader") else ""
    if header and session_id:
        value = session_id
        if str(session.get("idStyle") or "hex32") == "sess16":
            value = f"sess_{session_id[:16]}"
        headers[str(header).lower()] = value

    version_header = str(compat.get("versionHeader") or "")
    if version_header and cli_version and cli_version != "auto":
        headers[version_header] = cli_version
    environment_header = str(compat.get("environmentHeader") or "")
    if environment_header and up.get("cliEnvironment"):
        headers[environment_header] = str(up["cliEnvironment"])
    if compat.get("zdr", up.get("cmdZdr", False)):
        headers["x-cmd-zdr"] = "1"
    if up.get("protocol") == "anthropic":
        headers.setdefault("anthropic-version",
                           str(up.get("anthropicVersion") or "2023-06-01"))
    return headers


def validate_config_shape(config: dict) -> Optional[str]:
    """校验配置结构；合法返回 None，否则返回可读错误（HTTP 400 用）。

    目的：**先校验、后持久化**——避免把坏配置写进 bridge.config.json 后再崩溃，
    导致下次启动直接失败。错误级诊断与 /api/config/validate 完全一致。
    """
    errors = [item for item in validate_config(config) if item["level"] == "error"]
    return errors[0]["message"] if errors else None


def _to_int(value):
    try:
        if isinstance(value, bool):
            return None
        return int(float(value)) if isinstance(value, str) else int(value)
    except (TypeError, ValueError):
        return None


def resolve_model(config: dict, name) -> str:
    """别名 → 真实模型 ID；未命中则原样；空名回落到 defaultModel。"""
    if not name:
        return config.get("defaultModel") or (config.get("models") or [""])[0] or ""
    return (config.get("aliases") or {}).get(name, name)


def pick_upstream(config: dict, upstreams: list[dict], model) -> dict:
    """按模型名选上游：全集精确命中 → 上游级别名 → default 上游 → 第一个。"""
    name = str(model or "")
    target = resolve_model(config, name)
    for u in upstreams:
        if target in (u["modelsAll"] or u["models"]):
            return u
    for u in upstreams:
        if name in u["modelMap"]:
            return u
    for u in upstreams:
        if u["default"]:
            return u
    return upstreams[0]


def all_upstream_models(upstreams: list[dict]) -> list[str]:
    out: list[str] = []
    for u in upstreams:
        for m in u["models"]:
            if isinstance(m, str) and m not in out:
                out.append(m)
    return out


def model_list(config: dict, upstreams: list[dict]) -> list[str]:
    """/v1/models 暴露的清单：顶层 models 是精确白名单，配了就只展示它。"""
    top = config.get("models") or []
    ids: list[str] = [m for m in top if isinstance(m, str)] if top else all_upstream_models(upstreams)
    for alias, target in (config.get("aliases") or {}).items():
        for value in (alias, target):
            if isinstance(value, str) and value not in ids:
                ids.append(value)
    for u in upstreams:
        for alias, target in (u.get("modelMap") or {}).items():
            for value in (alias, target):
                if isinstance(value, str) and value not in ids:
                    ids.append(value)
    return ids


def effective_access_log(config: dict) -> bool:
    return config.get("accessLog") is not False


def apply_runtime_flags(config: dict) -> None:
    set_access_log(effective_access_log(config))



def merge_config_with_snapshot(file_config: dict, snapshot: dict) -> tuple[dict, list[str], list[str]]:
    """文件优先的配置合并。

    返回 ``(合并结果, 被忽略的快照冲突键, 仅快照提供的补充键)``。

    为什么不是「快照整体覆盖文件」：settings 快照是运行时派生产物，一旦里面
    留下测试哨兵或旧值（历史事故：``upstreams.cch.baseUrl`` 变成
    ``http://127.0.0.1:1`` 死端口），就会静默改掉用户手工维护的
    ``bridge.config.json``。用户可见契约是文件，所以文件优先，快照只补文件
    没有定义的键。
    """
    if not isinstance(snapshot, dict) or not snapshot:
        return copy.deepcopy(file_config), [], []

    merged = copy.deepcopy(file_config)
    ignored: list[str] = []
    filled: list[str] = []

    for key, value in snapshot.items():
        if value is None:
            continue
        if key not in merged:
            merged[key] = copy.deepcopy(value)
            filled.append(key)
            continue
        current = merged[key]
        if isinstance(current, dict) and isinstance(value, dict):
            added = _fill_missing_keys(current, value, key)
            filled.extend(added)
            ignored.extend(_conflict_keys(current, value, key))
            continue
        if current != value:
            ignored.append(key)

    return merged, ignored, filled


def _fill_missing_keys(dst: dict, src: dict, prefix: str) -> list[str]:
    """只把 src 有、dst 没有的键补进 dst（递归下钻字典），返回补入的路径。"""
    added: list[str] = []
    for key, value in src.items():
        if value is None:
            continue
        path = f"{prefix}.{key}"
        if key not in dst:
            dst[key] = copy.deepcopy(value)
            added.append(path)
        elif isinstance(dst[key], dict) and isinstance(value, dict):
            added.extend(_fill_missing_keys(dst[key], value, path))
    return added


def _conflict_keys(dst: dict, src: dict, prefix: str) -> list[str]:
    """找出两边都定义但取值不同的键路径（递归字典，列表/标量直接比较）。"""
    conflicts: list[str] = []
    for key, value in src.items():
        if value is None or key not in dst:
            continue
        path = f"{prefix}.{key}"
        current = dst[key]
        if isinstance(current, dict) and isinstance(value, dict):
            conflicts.extend(_conflict_keys(current, value, path))
        elif current != value:
            conflicts.append(path)
    return conflicts


__all__ = [
    "DEFAULT_CONFIG",
    "access_log_enabled",
    "all_upstream_models",
    "apply_runtime_flags",
    "default_config_path",
    "default_db_path",
    "expand_home",
    "infer_protocol",
    "load_config",
    "merge_config_with_snapshot",
    "load_env_file",
    "log",
    "model_list",
    "normalize_upstream",
    "pick_upstream",
    "resolve_api_key",
    "resolve_model",
    "save_config",
    "upstreams_of",
]
