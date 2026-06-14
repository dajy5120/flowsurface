//! 读 Redis `ws:signals:{symbol}`（由 P0 `ws_signals` 发布器发布，docs/08 F4b–d 精确版）→ cockpit 叠加。
//!
//! 信号来自我们的 `wealthspring-orderbook` 引擎（全档重建 L2 + 成交归因），算出
//! 盘口不平衡 / 最优档吸收 / 撤补（pull）/ 冰山补单 —— cockpit 只读渲染（进程隔离、rebase 友好）。
//! 与 `ws/flow.rs`（cockpit 自算的最优档 proxy）互补：本模块是「引擎精确版」。

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
                    std::thread::spawn(move || {
                        let mut conn = match Client::open(redis_url.as_str())
                            .and_then(|c| c.get_connection())
                        {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        let mut last = String::new();
                        loop {
                            let v: Option<String> = conn.get(&key).ok().flatten();
                            if let Some(s) = v {
                                if s != last {
                                    last = s.clone();
                                    if let Ok(sig) = serde_json::from_str::<Signals>(&s) {
                                        if tx.blocking_send(sig).is_err() {
                                            break;
                                        }
                                    }
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
