//! Tardis 回放只读进度快照（docs/20 Phase 5）。
//!
//! 惰性起后台 poller（1s）读 Redis `ws:tardis_replay:status`——由
//! `factory/replay/tardis_cockpit_feed.py` 每批推送后写入。**新增只读键，不动既有契约**
//! （`ws:active_run` / `ws:bt:*` 仍由既有 [`super::active_run`] / [`super::replay`] 消费）。
//! 惰性起：打开过 Tardis 工作区才轮询。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const STATUS_KEY: &str = "ws:tardis_replay:status";

#[derive(Default, Clone)]
pub struct ReplayStatus {
    pub started: bool, // poller 是否已起（区分「无数据」与「还没轮询」）
    pub run_id: String,
    pub symbol: String,
    pub date: String,
    pub sent: u64,
    pub total: u64,
    pub pct: f64,
    pub speed: f64,
    pub state: String, // running | done | stopped
    pub elapsed: f64,
    pub refreshed: String,
}

static STATE: OnceLock<Mutex<ReplayStatus>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

/// pane 渲染时读快照（惰性起 poller）。
pub fn snapshot() -> ReplayStatus {
    ensure_poller();
    STATE.get().and_then(|m| m.lock().ok().map(|g| g.clone())).unwrap_or_default()
}

fn redis_url() -> String {
    std::env::var("WS_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            let mut conn = None;
            loop {
                if conn.is_none() {
                    conn = redis::Client::open(redis_url())
                        .ok()
                        .and_then(|c| c.get_connection().ok());
                }
                let mut st = ReplayStatus {
                    started: true,
                    refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
                    ..Default::default()
                };
                if let Some(c) = conn.as_mut() {
                    use redis::Commands;
                    match c.get::<_, Option<String>>(STATUS_KEY) {
                        Ok(Some(s)) => parse_into(&s, &mut st),
                        Ok(None) => {}
                        Err(_) => conn = None, // 断了下轮重连
                    }
                }
                let lock = STATE.get_or_init(|| Mutex::new(ReplayStatus::default()));
                if let Ok(mut g) = lock.lock() {
                    *g = st;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    });
}

fn parse_into(s: &str, st: &mut ReplayStatus) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return;
    };
    let s_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let f_of = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let u_of = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    st.run_id = s_of("run_id");
    st.symbol = s_of("symbol");
    st.date = s_of("date");
    st.state = s_of("state");
    st.sent = u_of("sent");
    st.total = u_of("total");
    st.pct = f_of("pct");
    st.speed = f_of("speed");
    st.elapsed = f_of("elapsed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_feeder_status() {
        let mut st = ReplayStatus::default();
        parse_into(
            r#"{"run_id":"TARDIS-BTCUSDT-2026-06-15-0900","symbol":"BTCUSDT","date":"2026-06-15",
                "sent":1923,"total":1923,"pct":100.0,"speed":60.0,"state":"done","elapsed":2.0}"#,
            &mut st,
        );
        assert_eq!(st.run_id, "TARDIS-BTCUSDT-2026-06-15-0900");
        assert_eq!(st.symbol, "BTCUSDT");
        assert_eq!(st.sent, 1923);
        assert_eq!(st.total, 1923);
        assert_eq!(st.state, "done");
        assert!((st.pct - 100.0).abs() < 1e-9);
    }

    /// 真机集成（需活的 redis + 先跑一次 feeder）：poller 能读到 feeder 写的进度。
    /// 默认忽略；手动：
    ///   ~/ws-venv/bin/python -m factory.replay.tardis_cockpit_feed BTCUSDT 2026-06-15 \
    ///       --from 09:00 --minutes 2 --speed 0
    ///   cargo test -- --ignored reads_live_status
    #[test]
    #[ignore]
    fn reads_live_status() {
        let st = snapshot();
        std::thread::sleep(Duration::from_millis(1500)); // 等 poller 首轮
        let st2 = snapshot();
        assert!(st.started || st2.started, "poller 未起");
        assert!(st2.total > 0, "未读到 feeder 进度（先跑一次 feeder）");
        assert!(!st2.symbol.is_empty());
    }

    /// 坏 JSON / 缺字段不 panic，落回默认值（poller 里跑，必须稳）。
    #[test]
    fn tolerates_garbage() {
        let mut st = ReplayStatus::default();
        parse_into("not json", &mut st);
        assert_eq!(st.sent, 0);
        parse_into(r#"{"symbol":"ETHUSDT"}"#, &mut st);
        assert_eq!(st.symbol, "ETHUSDT");
        assert_eq!(st.total, 0);
    }
}
