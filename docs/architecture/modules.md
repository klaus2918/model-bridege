# 运行期模块开关（Core + Modules）

> 适用版本：4.6.0+。设计动机：在 4 核级别的低功耗机器上把空闲/运行占用压到最低，
> 同时保持「一键只做转发」的最简形态与全功能形态并存。

## 总览

Model-Gate 的架构是**核心转发 + 可选模块**两层：

- **核心转发**（永远在线）：`/health`、`/v1/models`（含 `/unified/...`、`/{upstream}/...` 变体）、
  `/v1/chat/completions`（含变体），以及完整的路由链
  `chat_scoped → build_route_plan → prepare_attempt → stream_response`、
  协议适配（anthropic / commandcode / opencode）、thinking 回传、路由健康冷却。
  这是产品的最小事实，不受任何开关影响。
- **可选模块**（`bridge.config.json` 顶层 `modules` 键逐项控制，或 CLI `--minimal` 一键全关）：
  统计、日志、插件、凭据池、面板、接入编排、AI 配置、自更新。

只做**运行期**门控，不做编译期 features：关闭的模块不挂路由、不启动线程、不产生 IO，
但代码仍在同一二进制里——零编译矩阵维护成本。

## 模块清单

| 键 | 关闭后 |
|---|---|
| `stats` | 不记用量、不算费用；usage-writer / prompt-writer 线程不启动；`/api/usage/*`、`/api/prompts/*` 不挂载；`promptCapture` 一并失效 |
| `logging` | 不装文件日志层（无 `data/modelgate.log`、无轮转）；stdout 仅 warn 以上；`accessLog` 失效 |
| `plugins` | `/api/plugins/*`、`/api/free-sources/*` 不挂载 |
| `pool` | `/api/pools/*` 不挂载；转发凭据选择忽略池绑定，一律单 Key 直连 |
| `panel` | `/ui/*` 与全部管理 API（info/config/upstreams/pricing/logs/catalog）不挂载；托盘「打开面板/检查更新」菜单项不注册；不自动开浏览器 |
| `access` | `/api/access/*` 不挂载 |
| `aiConfig` | `/api/ai/*`、`/llms.txt` 不挂载 |
| `update` | `/api/update/*` 不挂载；启动后不再自动检查更新 |

## 派生规则：数据库按需打开

```
db_needed = stats || plugins || pool || panel || aiConfig || update
```

- `db_needed == true`：正常打开 `data/modelgate.db`（WAL），retention 线程随 `db_needed` 启动。
- `db_needed == false`：用 `Db::open_in_memory()` 顶替（`Arc<Db>` 类型不变，调用点零改动）——
  **不创建 modelgate.db 文件**，没有 WAL / writer / retention 活动。
  端口同时落 `data/state.json`，二次启动的实例提示不依赖库文件。
- `logging` 与 `access` 不依赖持久化表（文件与配置即可），因此它们单独关闭不影响库的打开与否。

## 两种使用方式

### 1. CLI 一键精简

```powershell
Model-Gate.exe --minimal            # 精简形态（托盘保留，可 --no-gui 去托盘）
Model-Gate.exe --minimal --no-gui   # 纯转发：无托盘、无面板、无统计、无文件日志
```

### 2. 配置逐项控制

```json
{
  "modules": {
    "stats": true, "logging": true, "plugins": true, "pool": true,
    "panel": true, "access": true, "aiConfig": true, "update": true
  },
  "runtime": { "workerThreads": 2 }
}
```

缺省键视为 `true`（与 4.5 及之前的行为一致）；`--minimal` 优先级高于配置文件。

## 资源控制（runtime 键）

- `runtime.workerThreads`：Tokio worker 数，默认 **2**。代理是 IO 密集负载，
  少量 worker 即可支撑大量并发 SSE 流；4 核机器上默认开满 4 worker 只会增加
  调度与内存开销。上限 64（防呆）。

## 模式切换与模块的生效时机

**模块开关是启动冻结的**：路由表、后台线程、数据库都是启动时装配的，运行中
热切换会造成「路由没挂载但开始记录」的混合态。因此切换模式的落点是**带新参数重启**，
共三种方式：

1. **托盘一键切换**（4.6.0+）：菜单「以精简模式重启 / 以完整模式重启」——写好
   看护脚本（等本进程优雅退出后带新参数拉起自己），原 `--config/--db/--port`
   等参数原样保留；
2. `Model-Gate.exe --minimal`（或去掉该参数回到全功能）；
3. 编辑 `bridge.config.json` 的 `modules` 后重启。

面板保存与 `/api/config/reload` 会热重载**其他**配置（上游/模型/别名等），
但 `modules` 键以启动时为准，热重载不会改变它。

## 4.6.0 的性能修复（默认形态同样受益）

| 问题 | 修复 |
|---|---|
| 每 10s `today_stats()` SQL 聚合（托盘数字） | 改内存计数器 `today.rs`：启动时用库里「今日」聚合预热一次（跨重启不丢当日累计），之后请求路径增量维护，托盘直读快照，零周期 SQL；`/api/usage/clear` 清理后自动重算对齐 |
| SSE 透传流每个事件全量 JSON parse（只为抓末尾 usage） | `stream_response` 字节级边界扫描 + `"usage"` 子串预判，命中才解析；顺带修复跨 chunk 多字节字符导致的 usage 抓取失真 |
| 每请求 `body.clone()`（promptCapture 关闭时也克隆） | 克隆移到开关判定之后，多模态请求省一次整份拷贝 |
| 面板聚合 10s TTL，30s 自动刷新几乎每次全表扫描 | `AGG_TTL` 10s → 60s，刷新隔次命中缓存 |
| reqwest 空闲连接数无上限 | `pool_idle_timeout(90s)` + `pool_max_idle_per_host(8)` |
| release 用 `opt-level="s"`（体积优先） | 改 `opt-level=3` + `lto="thin"`，每请求 CPU 显著下降 |
| retention 批量删除后 WAL 持续膨胀 | 每轮治理后 `wal_checkpoint(TRUNCATE)` |

## 代码落点

| 关注点 | 位置 |
|---|---|
| `Modules` / `RuntimeConfig` 解析与 `db_needed()` | `rust/model-gate/src/config.rs` |
| 路由条件挂载（core/panel/usage/plugins/pool/access/ai/update） | `rust/model-gate/src/proxy.rs::router` |
| AppState 门控（writer 线程、记录路径、池选择） | `rust/model-gate/src/proxy.rs` |
| 今日计数器 | `rust/model-gate/src/today.rs` |
| 启动编排（--minimal、库派生、日志层、worker_threads、state.json） | `rust/model-gate/src/main.rs` |
| 面板与管理处理器 | `rust/model-gate/src/panel_api.rs`（自 proxy.rs 拆出） |
| 内存库 | `rust/model-gate/src/db.rs::open_in_memory` |
| 托盘菜单门控 | `rust/model-gate/src/gui.rs` |

## 契约测试

- `config.rs::modules_tests`：默认全开、逐项覆盖、workerThreads 防呆、`db_needed` 派生规则。
- `proxy.rs::minimal_modules_keep_core_and_drop_panel`：精简形态核心转发可用 + 七组管理路由 404。
- `proxy.rs::default_modules_record_usage_and_today_counters`：默认形态面板在位、
  用量落库、今日计数器即时累加。
- `today.rs`：计数口径与旧 `today_stats()` 同形。
