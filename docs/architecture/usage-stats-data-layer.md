# 用量统计数据层升级

> 目标：在面板、API、配置文件、SQLite 表结构全部不变的前提下，降低统计端点的
> 内存峰值与耗时，并让「写入用量」不再被看板查询阻塞。

## 一、升级前后

| 维度 | 升级前 | 升级后 |
|---|---|---|
| P50/P95 计算 | 把过滤后的**全部** `duration_ms` 取回 Python，排序、分组后再线性插值 | SQLite 窗口函数只定位 `floor(r*(n-1))` 与 `ceil(...)` 两个边界样本，Python 只做插值 |
| 10 万行单次统计的 Python 峰值内存 | 13.9 – 20.5 MB | 4 – 31 KB |
| 10 万行单次统计耗时 | 0.59 – 2.01 s | 0.24 – 1.12 s |
| 连接模型 | 单连接 + 全局 `RLock`，读写互斥 | 1 条写连接 + 固定 2 条只读连接（WAL），读不阻塞写 |
| 外部行为 | — | 完全不变（字段、排序、数值、四舍五入口径一致） |

基准可复现：

```bat
py scripts/bench_stats_memory.py --rows 100000
```

## 二、为什么这样改

1. **内存峰值来自「把明细搬进 Python」**，不是 SQLite 本身。用量页每次刷新会并发请求
   totals / summary / models / upstreams 四个端点；旧实现每个端点都物化一份全量耗时
   列表，行数越大峰值越高，且会随并发叠加。
2. **百分位只需要两个样本**。`percentile()` 的线性插值只依赖第 `floor(r*(n-1))` 与
   下一个样本；这两个值可以在 SQLite 里用 `ROW_NUMBER()` / `COUNT(*) OVER ()` 直接定位。
3. **读写分离是 WAL 的自然用法**。看板聚合查询可能持续数百毫秒，旧实现会持有全局锁，
   使同一时刻完成的对话请求在「落库」这一步排队。改为固定大小的只读连接池后：
   - 连接数固定，不随 HTTP 线程增长（内存可预期）；
   - 读连接只执行 SELECT，写连接负责 INSERT/UPDATE/DELETE；
   - 通过 `PRAGMA busy_timeout=5000` 处理极少见的写-写竞争。

## 三、兼容性约束

- `Database._conn` 仍是写连接，测试与既有脚本对它的直接读写保持有效。
- `:memory:` 数据库无法多连接共享，自动退回「单连接 + 锁」。
- `percentile()` 函数保留（API、测试与外部脚本可能引用），新代码使用
  `percentile_from_edges()`，两者对同一组样本返回完全相同的值。
- 表结构、索引、迁移逻辑未改动。

## 四、验证

- `tests/test_stats_percentile_sql.py`：空库、单样本、随机 400 行，逐组比对
  totals / summary(day|model|upstream|session) / model_usage / upstream_usage 的
  P50、P95、Max 与 `percentile()`；并验证读连接能立即看到已提交的写入。
- 既有回归：`test_usage_query`、`test_export_retention`、`test_latency_failover`、
  `test_proxy_e2e`、`test_ui_and_api` 等全部通过。

## 五、没有采用的方案

| 方案 | 不采用的原因 |
|---|---|
| 全量改用 asyncio / aiohttp | 适配层是同步迭代器协议（SSE、NDJSON、pause_turn 续跑），重写面覆盖 proxy + 3 个协议适配器；收益集中在本已很轻的 I/O 等待上，回归风险远大于收益 |
| 用 Go/Rust 重写代理内核 | 运行时内存的大头是 WebView2 面板进程，换语言救不回；同时会丢失现有 Python 测试的直接覆盖，属于「功能与设计不变」的高风险路径。（2026-10 复评：该结论基于保留内嵌 WebView2 的前提。Rust 桌面壳已改为托盘 + 系统浏览器，实测常驻 19.3MB、进程树无 `msedgewebview2`，重写收益成立且已落地；保留此条作为历史决策记录，见 [`tech-stack-selection.md`](tech-stack-selection.md)。） |
| 统计结果缓存 | 每条对话结束都会写库，缓存会被持续失效；在真实流量下收益不稳定，反而增加一致性问题 |
