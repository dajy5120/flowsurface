//! 自有数据回测入图（docs/08）：读最新 result.json（`backtest_readout` 快照）的 price 收盘价
//! → 聚合 M1 K 线喂图（▶运行 脚本自带数据的回测，如 quickstart/backtest_low_level）；fills →
//! 图上成交标记 `CHART_FILLS`（▲▼）。仅「自有数据回测」工作区激活时订阅（main.rs 门控）。
//!
//! 与 replay.rs 对称：replay 源自 Redis `ws:bt:{run}:trades`（录制数据），本模块源自 result.json
//! 的 price/fills（自有数据）。result.json 变化（dir 变）才重推。

use std::time::Duration;

use exchange::adapter::{Event, StreamKind};
use exchange::unit::{Price, Qty, UnixMs};
use exchange::{Kline, TickerInfo, Timeframe, Volume};
use iced::Subscription;
use iced::futures::SinkExt;

use super::backtest_readout;
use super::orders::{self, Fill};

const TF_MS: u64 = 60_000;

/// 订阅身份（手动 Hash：TickerInfo 含 f32 不可 Hash）。
#[derive(Clone)]
pub struct SelfdataId {
    pub ticker: TickerInfo,
}

impl std::hash::Hash for SelfdataId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "ws-selfdata".hash(state);
        self.ticker.ticker.hash(state);
    }
}

fn emit_kline(bucket: u64, o: f32, h: f32, l: f32, c: f32) -> Kline {
    Kline {
        time: UnixMs(bucket),
        open: Price::from_f32(o),
        high: Price::from_f32(h),
        low: Price::from_f32(l),
        close: Price::from_f32(c),
        volume: Volume::BuySell(Qty::from_f32(0.0), Qty::from_f32(0.0)),
    }
}

/// 为某 ticker 建一条自有数据 replay 订阅（builder 非捕获，参数经 id 传入）。
pub fn subscription(ticker_info: TickerInfo) -> Subscription<Event> {
    Subscription::run_with(SelfdataId { ticker: ticker_info }, |id: &SelfdataId| {
        let ticker_info = id.ticker;
        iced::stream::channel(
            256,
            move |mut output: iced::futures::channel::mpsc::Sender<Event>| async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<backtest_readout::BacktestResult>(4);

                // 轮询 result.json 快照：dir 变化（新结果）且有 price 才推。
                std::thread::spawn(move || {
                    let mut last_dir = String::new();
                    loop {
                        let snap = backtest_readout::snapshot();
                        if snap.loaded && snap.dir != last_dir && !snap.price.v.is_empty() {
                            last_dir = snap.dir.clone();
                            if tx.blocking_send(snap).is_err() {
                                break; // 订阅已 drop
                            }
                        }
                        std::thread::sleep(Duration::from_millis(800));
                    }
                });

                while let Some(snap) = rx.recv().await {
                    // ① price（收盘价线）→ M1 K 线（open=high=low=close）。
                    let mut agg: Option<(u64, f32, f32, f32, f32)> = None; // (bucket,o,h,l,c)
                    let n = snap.price.t.len().min(snap.price.v.len());
                    for i in 0..n {
                        let ts = snap.price.t[i].max(0) as u64;
                        let px = snap.price.v[i] as f32;
                        let b = (ts / TF_MS) * TF_MS;
                        match agg.as_mut() {
                            Some((bk, _o, h, l, c)) if *bk == b => {
                                *h = h.max(px);
                                *l = l.min(px);
                                *c = px;
                            }
                            _ => {
                                if let Some((bk, o, h, l, c)) = agg {
                                    let kl =
                                        StreamKind::Kline { ticker_info, timeframe: Timeframe::M1 };
                                    if output
                                        .send(Event::KlineReceived(kl, emit_kline(bk, o, h, l, c)))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                agg = Some((b, px, px, px, px));
                            }
                        }
                    }
                    if let Some((bk, o, h, l, c)) = agg {
                        let kl = StreamKind::Kline { ticker_info, timeframe: Timeframe::M1 };
                        if output
                            .send(Event::KlineReceived(kl, emit_kline(bk, o, h, l, c)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    // ② fills（[ts_ms, side, px]）→ 图上 ▲▼。
                    let fills: Vec<Fill> = snap
                        .fills
                        .iter()
                        .enumerate()
                        .map(|(i, f)| Fill {
                            ts: f[0].max(0.0) as u64,
                            side: f[1] as u8,
                            px: f[2],
                            qty: 0.0,
                            seq: (i + 1) as u64,
                        })
                        .collect();
                    orders::publish_chart_fills(&fills);
                }
            },
        )
    })
}
