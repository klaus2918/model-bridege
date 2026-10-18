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


def normalize_upstream(name: str, u: dict) -> dict:
    if not isinstance(u, dict):
        u = {}
    protocol = infer_protocol(u)
    base_url = _normalize_base_url(u.get("baseUrl"))
    is_cc = protocol == "commandcode"
    api_key, api_key_source = resolve_api_key(u)
    declared = u.get("models") or []
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
        "sessionHeader": u.get("sessionHeader", not is_cc),
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
        "pauseTurn": u.get("pauseTurn", is_cc),
        "cmdZdr": u.get("cmdZdr", True),
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
    return [normalize_upstream(name, u) for name, u in items]


def validate_config_shape(config: dict) -> Optional[str]:
    """校验配置结构；合法返回 None，否则返回可读错误（HTTP 400 用）。

    目的：**先校验、后持久化**——避免把坏配置写进 bridge.config.json 后再崩溃，
    导致下次启动直接失败。
    """
    if not isinstance(config, dict):
        return "配置必须是 JSON 对象"

    def _is_num(value) -> bool:
        if isinstance(value, bool):
            return False
        return isinstance(value, (int, float)) or (
            isinstance(value, str) and value.strip() != ""
            and _to_int(value) is not None)

    port = config.get("port")
    if port is not None and not _is_num(port):
        return f"port 必须是数字（当前 {port!r}）"
    if _is_num(port) and not 0 <= int(_to_int(port)) <= 65535:
        return "port 必须在 0-65535 之间"

    for key in ("maxTokens", "timeoutSeconds", "modelsCacheSeconds"):
        value = config.get(key)
        if value is not None and not _is_num(value):
            return f"{key} 必须是数字（当前 {value!r}）"

    host = config.get("host")
    if host is not None and not isinstance(host, str):
        return "host 必须是字符串"

    default_model = config.get("defaultModel")
    if default_model is not None and not isinstance(default_model, str):
        return "defaultModel 必须是字符串"

    models = config.get("models")
    if models is not None:
        if not isinstance(models, list):
            return "models 必须是数组"
        if any(not isinstance(m, str) for m in models):
            return "models 每一项都必须是字符串"

    aliases = config.get("aliases")
    if aliases is not None:
        if not isinstance(aliases, dict):
            return "aliases 必须是对象（别名 = 真实模型）"
        if any(not isinstance(k, str) or not isinstance(v, str)
               for k, v in aliases.items()):
            return "aliases 的键与值都必须是字符串"

    upstreams = config.get("upstreams")
    if upstreams is not None:
        if not isinstance(upstreams, dict):
            return "upstreams 必须是对象（上游名 → 上游配置）"
        for name, up in upstreams.items():
            if not isinstance(name, str):
                return f"上游名必须是字符串（当前 {name!r}）"
            if not isinstance(up, dict):
                return f"上游 {name!r} 的配置必须是对象"
            if up.get("models") is not None and not isinstance(up["models"], list):
                return f"上游 {name!r} 的 models 必须是数组"
            if up.get("modelMap") is not None and not isinstance(up["modelMap"], dict):
                return f"上游 {name!r} 的 modelMap 必须是对象"

    thinking = config.get("thinking")
    if thinking is not None and not isinstance(thinking, dict):
        return "thinking 必须是对象"
    tools = config.get("tools")
    if tools is not None and not isinstance(tools, dict):
        return "tools 必须是对象"
    return None


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
            if m not in out:
                out.append(m)
    return out


def model_list(config: dict, upstreams: list[dict]) -> list[str]:
    """/v1/models 暴露的清单：顶层 models 是精确白名单，配了就只展示它。"""
    top = config.get("models") or []
    ids: list[str] = list(top) if top else all_upstream_models(upstreams)
    for alias, target in (config.get("aliases") or {}).items():
        for value in (alias, target):
            if value not in ids:
                ids.append(value)
    for u in upstreams:
        for alias, target in (u.get("modelMap") or {}).items():
            for value in (alias, target):
                if value not in ids:
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
