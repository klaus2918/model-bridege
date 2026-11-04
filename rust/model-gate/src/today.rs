//! 今日指标内存计数器：替代旧的「每 10 秒 `today_stats()` 查库聚合」。
//!
//! 旧实现为了托盘上的两个数字，每 10 秒对 `usage_records` 做一次日期范围
//! 聚合扫描；库一大这就是常驻 CPU 来源。现在请求路径在 `record_ex` 阶段
//! 顺手累加内存计数，托盘定时器读取时直接产出同形 JSON——零 SQL、零后台任务。
//!
//! 计数器只服务于「今日」展示，是尽力而为的进程内事实：进程重启后从零开始，
//! 跨零点自动重置。持久化事实源仍是 `usage_records` 表，不受影响。

use std::sync::Mutex;

use chrono::{Local, NaiveDate};
use serde_json::{json, Value};

#[derive(Debug, Default)]
struct TodayInner {
    /// 计数归属日期；与当前日期不符即视为跨零点，读到的都是零、写入时重置。
    date: Option<NaiveDate>,
    requests: i64,
    ok: i64,
    prompt_tokens: i64,
    cache_read_tokens: i64,
}

/// 今日（本地日期）请求计数器。线程安全；单把小锁，请求路径开销可忽略。
#[derive(Debug, Default)]
pub struct TodayCounters {
    inner: Mutex<TodayInner>,
}

impl TodayCounters {
    /// 记录一次请求。status < 400 计为成功（与 `Db::today_stats` 口径一致）。
    pub fn record(&self, status: i64, prompt_tokens: i64, cache_read_tokens: i64) {
        let today = Local::now().date_naive();
        let mut inner = self.inner.lock().expect("today counters poisoned");
        if inner.date != Some(today) {
            *inner = TodayInner {
                date: Some(today),
                ..TodayInner::default()
            };
        }
        inner.requests += 1;
        if status < 400 {
            inner.ok += 1;
        }
        inner.prompt_tokens += prompt_tokens;
        inner.cache_read_tokens += cache_read_tokens;
    }

    /// 用库里的「今日」聚合直接设值（进程启动预热 / 用量清理后重算）。
    ///
    /// 计数器是内存态，单靠请求路径累加会有两个盲区：进程重启后从零开始
    /// （而库里有当天全部记录）、清理用量后不会回退。预热把两边都对齐到库口径。
    pub fn seed(&self, requests: i64, ok: i64, prompt_tokens: i64, cache_read_tokens: i64) {
        let today = Local::now().date_naive();
        let mut inner = self.inner.lock().expect("today counters poisoned");
        *inner = TodayInner {
            date: Some(today),
            requests,
            ok,
            prompt_tokens,
            cache_read_tokens,
        };
    }

    /// 归零（保留日期语义），等价于 `seed(0, 0, 0, 0)`。
    pub fn reset(&self) {
        self.seed(0, 0, 0, 0);
    }

    /// 产出与 `Db::today_stats()` 完全同形的 JSON，托盘/面板零适配。
    pub fn snapshot(&self) -> Value {
        let today = Local::now().date_naive();
        let inner = self.inner.lock().expect("today counters poisoned");
        let (requests, ok, prompt, cache_read) = if inner.date == Some(today) {
            (
                inner.requests,
                inner.ok,
                inner.prompt_tokens,
                inner.cache_read_tokens,
            )
        } else {
            (0, 0, 0, 0)
        };
        drop(inner);
        let success_rate = if requests > 0 {
            round4(ok as f64 / requests as f64)
        } else {
            0.0
        };
        json!({
            "requests": requests,
            "ok": ok,
            "success_rate": success_rate,
            "cache_read_tokens": cache_read,
            "prompt_tokens": prompt,
            "cache_hit_rate": hit_rate(cache_read, prompt),
        })
    }
}

// 口径与 db.rs 的同名辅助函数一致：缓存命中率 > 1 属正常（有 cache_write 复用）。
fn hit_rate(cache_read: i64, prompt: i64) -> Option<f64> {
    if prompt <= 0 {
        None
    } else {
        Some(round6(cache_read as f64 / prompt as f64))
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 记录与快照口径对齐_today_stats() {
        let counters = TodayCounters::default();
        counters.record(200, 100, 50);
        counters.record(500, 10, 0);
        let snap = counters.snapshot();
        assert_eq!(snap["requests"], 2);
        assert_eq!(snap["ok"], 1);
        assert_eq!(snap["prompt_tokens"], 110);
        assert_eq!(snap["cache_read_tokens"], 50);
        // success_rate = 1/2，cache_hit_rate = 50/110。
        assert_eq!(snap["success_rate"], 0.5);
        assert!(snap["cache_hit_rate"].as_f64().unwrap() > 0.45);
    }

    #[test]
    fn 空计数器返回零值而非_null() {
        let snap = TodayCounters::default().snapshot();
        assert_eq!(snap["requests"], 0);
        assert_eq!(snap["success_rate"], 0.0);
        assert!(snap["cache_hit_rate"].is_null());
    }

    #[test]
    fn 预热后继续增量且可重置() {
        let counters = TodayCounters::default();
        // 模拟启动预热：库里有 100 条今日记录。
        counters.seed(100, 90, 5000, 800);
        let snap = counters.snapshot();
        assert_eq!(snap["requests"], 100);
        assert_eq!(snap["ok"], 90);
        assert_eq!(snap["prompt_tokens"], 5000);
        // 预热后新请求照常累加。
        counters.record(200, 10, 5);
        let snap = counters.snapshot();
        assert_eq!(snap["requests"], 101);
        assert_eq!(snap["prompt_tokens"], 5010);
        // 用量清空后重置归零。
        counters.reset();
        let snap = counters.snapshot();
        assert_eq!(snap["requests"], 0);
        assert!(snap["cache_hit_rate"].is_null());
    }
}
