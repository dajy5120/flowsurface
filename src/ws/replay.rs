//! 回测行情入图 replay 订阅（docs/08 F1.2）。
//!
//! 回测态（`ws:active_run` mode=backtest）下跟随当前 run，读 `ws:bt:{run}:trades`，把每批成交转成
//! `exchange::Event::TradesReceived` —— 直接喂 FS 现有 `ingest_trades` 路径，**无需改图表层**。
//! 桥接：阻塞 redis 轮询在独立线程（`tokio::sync::mpsc::blocking_send`）→ async 订阅转发为 Event。
//! 非回测态空闲（不产数据）；run 变更则重连。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use exchange::adapter::{Event, StreamKind};
use exchange::unit::{Price, Qty, UnixMs};
use exchange::{Kline, TickerInfo, Timeframe, Trade, Volume};
use iced::Subscription;
use iced::futures::SinkExt;

use super::active_run::ActiveRunWatcher;
use super::bt_trades::BtTradeConsumer;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// M1 桶大小（毫秒）——回测逐笔聚合成「时间轴蜡烛图」用（docs/08 F3b）。
const TF_MS: u64 = 60_000;

/// 跨批次维护的 M1 K 线聚合：回测只发逐笔（无 K 线源），时间基蜡烛/足迹无法建桶。
/// 这里把逐笔聚合成 M1 K 线并发 `KlineReceived`，使回测数据进入标准蜡烛图，
/// 同时让时间基的图上成交标记 ▲▼（F3b）能正确对齐 x 轴。
struct KlineAgg {
    bucket: u64,
    open: f32,
    high: f32,
    low: f32,
    close: f32,
    buy: f32,
    sell: f32,
}

impl KlineAgg {
    fn to_kline(&self) -> Kline {
        Kline {
            time: UnixMs(self.bucket),
            open: Price::from_f32(self.open),
            high: Price::from_f32(self.high),
            low: Price::from_f32(self.low),
            close: Price::from_f32(self.close),
            volume: Volume::BuySell(Qty::from_f32(self.buy), Qty::from_f32(self.sell)),
        }
    }
}

/// 订阅身份 + 参数载体（`run_with` 的 builder 必须是非捕获 fn，故所有状态经此传入）。
/// 手动 Hash：TickerInfo 含 f32 不可 Hash，按 (url + ticker 符号) 标识。
#[derive(Clone)]
pub struct ReplayId {
    pub redis_url: String,
    pub ticker: TickerInfo,
}

impl std::hash::Hash for ReplayId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "ws-bt-replay".hash(state);
        self.redis_url.hash(state);
        self.ticker.ticker.hash(state);
    }
}

/// 为某 ticker 建一条回测 replay 订阅。builder 必须非捕获（fn ptr）：参数经 ReplayId 传入，
/// 闭包从 id 取（不捕获环境）；返回的流不借用 id（内部 clone/copy）。
pub fn subscription(redis_url: String, ticker_info: TickerInfo) -> Subscription<Event> {
    Subscription::run_with(ReplayId { redis_url, ticker: ticker_info }, |id: &ReplayId| {
        let redis_url = id.redis_url.clone();
        let ticker_info = id.ticker;
        iced::stream::channel(256, move |mut output: iced::futures::channel::mpsc::Sender<Event>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Box<[Trade]>>(64);

            // 阻塞线程：跟随回测 run 轮询 ws:bt:{run}:trades，转 Trade 批量回传。
            std::thread::spawn(move || {
                let mut watcher = ActiveRunWatcher::connect(&redis_url).ok();
                let mut active: Option<String> = None;
                let mut consumer: Option<BtTradeConsumer> = None;
                loop {
                    let want = watcher
                        .as_mut()
                        .and_then(|w| w.poll().ok().flatten())
                        .filter(|ar| ar.mode == "backtest")
                        .map(|ar| ar.run_id);
                    if want != active {
                        active = want.clone();
                        consumer = active
                            .as_ref()
                            .and_then(|r| BtTradeConsumer::connect(&redis_url, r).ok());
                    }
                    let Some(c) = consumer.as_mut() else {
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    };
                    match c.poll() {
                        Ok(b) if !b.is_empty() => {
                            let trades: Box<[Trade]> = b
                                .iter()
                                .map(|t| Trade {
                                    time: UnixMs(t.ts),
                                    is_sell: t.side == 2,
                                    price: Price::from_f32(t.px as f32),
                                    qty: Qty::from_f32(t.qty as f32),
                                })
                                .collect();
                            if tx.blocking_send(trades).is_err() {
                                break; // 订阅已 drop
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            consumer = None;
                            active = None;
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    }
                }
            });

            let mut agg: Option<KlineAgg> = None;
            while let Some(trades) = rx.recv().await {
                // ① 聚合 M1 K 线并发 KlineReceived（回测入标准蜡烛图 + 让 ▲▼ 对齐 x 轴）。
                for t in trades.iter() {
                    let b = (t.time.as_u64() / TF_MS) * TF_MS;
                    let px = t.price.to_f32();
                    let q = f32::from(t.qty);
                    let (buy, sell) = if t.is_sell { (0.0, q) } else { (q, 0.0) };
                    match agg.as_mut() {
                        Some(a) if a.bucket == b => {
                            a.high = a.high.max(px);
                            a.low = a.low.min(px);
                            a.close = px;
                            a.buy += buy;
                            a.sell += sell;
                        }
                        _ => {
                            // 新桶：先把上一桶定型发出，再开新桶。
                            if let Some(a) = agg.as_ref() {
                                let kl = StreamKind::Kline { ticker_info, timeframe: Timeframe::M1 };
                                if output.send(Event::KlineReceived(kl, a.to_kline())).await.is_err()
                                {
                                    return;
                                }
                            }
                            agg = Some(KlineAgg {
                                bucket: b,
                                open: px,
                                high: px,
                                low: px,
                                close: px,
                                buy,
                                sell,
                            });
                        }
                    }
                }
                // 当前进行中的桶也发出（latest）。
                if let Some(a) = agg.as_ref() {
                    let kl = StreamKind::Kline { ticker_info, timeframe: Timeframe::M1 };
                    if output.send(Event::KlineReceived(kl, a.to_kline())).await.is_err() {
                        return;
                    }
                }
                // ② 逐笔照发（喂 CVD/flow tap 与足迹簇）。
                let stream = StreamKind::Trades { ticker_info };
                if output.send(Event::TradesReceived(stream, UnixMs(now_ms()), trades)).await.is_err()
                {
                    break;
                }
            }
        })
    })
}
