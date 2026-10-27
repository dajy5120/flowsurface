//! 读 Redis `ws:signals:{symbol}`（由 P0 `ws_signals` 发布器发布，docs/08 F4b–d 精确版）→ cockpit 叠加。
//!
//! 信号来自我们的 `wealthspring-orderbook` 引擎（全档重建 L2 + 成交归因），算出
//! 盘口不平衡 / 最优档吸收 / 撤补（pull）/ 冰山补单 —— cockpit 只读渲染（进程隔离、rebase 友好）。
//! 与 `ws/flow.rs`（cockpit 自算的最优档 proxy）互补：本模块是「引擎精确版」。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use redis::{Client, Commands};
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Signals {
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub best_bid: f64,
    #[serde(default)]
    pub best_ask: f64,
    #[serde(default)]
    pub spread: f64,
    #[serde(default)]
    pub imb5: f64,
    #[serde(default)]
    pub absorb_bid: f64,
    #[serde(default)]
    pub absorb_ask: f64,
    #[serde(default)]
    pub traded_bid: f64,
    #[serde(default)]
    pub traded_ask: f64,
    #[serde(default)]
    pub pulled_bid: f64,
    #[serde(default)]
    pub pulled_ask: f64,
    #[serde(default)]
    pub sess_traded_bid: f64,
    #[serde(default)]
    pub sess_traded_ask: f64,
    #[serde(default)]
    pub sess_pulled_bid: f64,
    #[serde(default)]
    pub sess_pulled_ask: f64,
    #[serde(default)]
    pub iceberg_bid: f64,
    #[serde(default)]
    pub iceberg_ask: f64,
    #[serde(default)]
    pub depth_bid: u64,
    #[serde(default)]
    pub depth_ask: u64,
    // F4c combo：现役池实时加权值 Σ wᵢ·alphaᵢ + 覆盖数（已就绪/池总数）。
    #[serde(default)]
    pub combo: f64,
    #[serde(default)]
    pub n_combo: u64,
    #[serde(default)]
    pub n_pool: u64,
}

// ── F4c combo：进程级旁路（main.rs 的 WsSignals 写入）→「因子池合成分」指标（docs/63 §13）──
/// combo 跟的品种（`ws-signals` 发布器只算这一个，见下面 `subscription` 的调用处）。
pub const COMBO_SYMBOL: &str = "BTCUSDT";

/// 「因子池合成分」指标（docs/63 §13）用的历史：每秒一个点（这一秒最后收到的值），留 24 小时。
const COMBO_KEEP_MS: u64 = 24 * 3_600_000;
static COMBO_HIST: OnceLock<Mutex<VecDeque<(u64, f64)>>> = OnceLock::new();

/// 追加一笔 combo 采样 `(ts_ms, combo)`（接收时本机时戳，与实时主图时间轴对齐）。
pub fn push_combo(ts_ms: u64, combo: f64) {
    if combo.is_finite()
        && let Ok(mut h) = COMBO_HIST.get_or_init(Default::default).lock()
    {
        // 同一秒只留最后一个
        if h.back().is_some_and(|(t, _)| *t / 1_000 >= ts_ms / 1_000) {
            h.pop_back();
        }
        h.push_back((ts_ms, combo));
        while h.front().is_some_and(|(t, _)| t + COMBO_KEEP_MS < ts_ms) {
            h.pop_front();
        }
    }
}

/// 「因子池合成分」指标的历史（收到时刻毫秒, 值），每秒一个、最多 24 小时。
pub fn combo_history() -> Vec<(u64, f64)> {
    COMBO_HIST.get().and_then(|m| m.lock().ok().map(|g| g.iter().copied().collect())).unwrap_or_default()
}

/// 轮询 `ws:signals:{symbol}`（250ms）→ 变化时发出引擎信号快照。
pub fn subscription(redis_url: String, symbol: String) -> iced::Subscription<Signals> {
    use iced::futures::SinkExt;
    iced::Subscription::run_with(
        ("ws-signals", redis_url, symbol),
        |(_, redis_url, symbol): &(&str, String, String)| {
            let redis_url = redis_url.clone();
            let key = format!("ws:signals:{symbol}");
            iced::stream::channel(
                8,
                move |mut output: iced::futures::channel::mpsc::Sender<Signals>| async move {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<Signals>(8);
                    super::spawn_named("ws-signals", move || {
                        let mut conn = match Client::open(redis_url.as_str())
                            .and_then(|c| c.get_connection())
                        {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        let mut last = String::new();
                        loop {
                            // 对端存活探测：**每轮无条件做一次**。
                            // 原先唯一的检测点是 `tx.blocking_send(..).is_err()`，而它嵌在
                            // 「值发生变化」的分支里——值冻住时（恰恰是上游守护挂掉的表现）
                            // 这个线程永远发现不了订阅已被销毁，以固定间隔永久空转。
                            if tx.is_closed() {
                                break;
                            }
                            let v: Option<String> = conn.get(&key).ok().flatten();
                            if let Some(s) = v
                                && s != last
                            {
                                last = s.clone();
                                if let Ok(sig) = serde_json::from_str::<Signals>(&s)
                                    && tx.blocking_send(sig).is_err()
                                {
                                    break;
                                }
                            }
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                    });
                    while let Some(sig) = rx.recv().await {
                        if output.send(sig).await.is_err() {
                            break;
                        }
                    }
                },
            )
        },
    )
}
