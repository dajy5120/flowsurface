//! 性能预算的测量（docs/35 §8 / §13.2，UPDS V5 §39）：冷启动到第一帧 < 1.5 s、
//! 切换工作区（面板切换）< 100 ms、命令面板首批结果 < 50 ms。
//!
//! 只做计时、不改行为：`main` 记起点，`view` 每次画完报一次（第一次 = 冷启动；
//! 之前刚切过工作区 = 这次切换的耗时），命令面板每次过滤报一次。
//! 样张模式退出时写 `perf.json`，`scripts/ui_perf_gate.py` 按预算判定。
//!
//! 「画完」指界面树构建完成（update + view），不含 GPU 提交——那部分由显卡驱动决定，
//! 测不准也不归界面代码管。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();
static FIRST_FRAME: OnceLock<Duration> = OnceLock::new();
/// 正在切的工作区：（名字，开始时刻，第一帧到达的耗时，此后最慢一帧）。
static SWITCH: Mutex<Option<(String, Instant, Option<Duration>, Duration)>> = Mutex::new(None);
/// 已完成的切换：（名字，到第一帧，切换后 1 秒内最慢一帧）。
static SWITCHES: Mutex<Vec<(String, Duration, Duration)>> = Mutex::new(Vec::new());
/// 切换后观察多久：面板内容常常晚一两帧才画（先出空壳再填数据），
/// 只看第一帧会把那一下真正的卡顿漏掉——实测「资源」页首帧 0.4ms、第五帧 217ms。
const SETTLE: Duration = Duration::from_secs(1);
static PALETTE: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

/// 进程起点（`main` 第一行调）。
pub fn mark_start() {
    let _ = START.set(Instant::now());
}

/// 开始切换工作区。启动时那次加载算在冷启动里，不另记。
pub fn switch_started(name: &str) {
    if FIRST_FRAME.get().is_none() {
        return;
    }
    flush_switch();
    if let Ok(mut g) = SWITCH.lock() {
        *g = Some((name.to_string(), Instant::now(), None, Duration::ZERO));
    }
}

fn flush_switch() {
    if let Ok(mut g) = SWITCH.lock()
        && let Some((name, _, Some(first), worst)) = g.take()
        && let Ok(mut v) = SWITCHES.lock()
    {
        v.push((name, first, worst));
    }
}

/// 一帧的界面树构建完了；`took` 是这一帧 view 本身的耗时。
pub fn view_done(took: Duration) {
    if FIRST_FRAME.get().is_none()
        && let Some(t0) = START.get()
    {
        let _ = FIRST_FRAME.set(t0.elapsed());
    }
    let mut done = false;
    if let Ok(mut g) = SWITCH.lock()
        && let Some((_, t0, first, worst)) = g.as_mut()
    {
        if first.is_none() {
            *first = Some(t0.elapsed());
        } else {
            *worst = (*worst).max(took);
        }
        done = t0.elapsed() >= SETTLE;
    }
    if done {
        flush_switch();
    }
}

/// 命令面板过滤一次的耗时。
pub fn palette_filtered(d: Duration) {
    if let Ok(mut v) = PALETTE.lock()
        && v.len() < 1000
    {
        v.push(d);
    }
}

/// 汇总成 JSON（毫秒）。
pub fn report() -> serde_json::Value {
    let ms = |d: &Duration| d.as_secs_f64() * 1e3;
    flush_switch();
    let switches = SWITCHES.lock().map(|v| v.clone()).unwrap_or_default();
    let palette = PALETTE.lock().map(|v| v.clone()).unwrap_or_default();
    serde_json::json!({
        "cold_start_ms": FIRST_FRAME.get().map(ms),
        "switch_ms": switches.iter().map(|(n, d, w)| serde_json::json!({"workspace": n, "ms": ms(d), "worst_frame_ms": ms(w)})).collect::<Vec<_>>(),
        "switch_max_ms": switches.iter().map(|(_, d, w)| ms(d).max(ms(w))).fold(0.0, f64::max),
        "palette_max_ms": palette.iter().map(ms).fold(0.0, f64::max),
        "palette_n": palette.len(),
        "budget": {"cold_start_ms": 1500, "switch_ms": 100, "palette_ms": 50},
    })
}

/// 样张模式退出时写到样张目录。
pub fn write(dir: &std::path::Path) {
    let p = dir.join("perf.json");
    if let Err(e) = std::fs::write(&p, serde_json::to_string_pretty(&report()).unwrap_or_default()) {
        log::warn!("[perf] 写 {} 失败：{e}", p.display());
    } else {
        log::info!("[perf] {}", report());
    }
}
