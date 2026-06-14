//! 回测行情入图 replay 订阅（docs/08 F1.2）。
//!
//! 回测态（`ws:active_run` mode=backtest）下跟随当前 run，读 `ws:bt:{run}:trades`，把每批成交转成
//! `exchange::Event::TradesReceived` —— 直接喂 FS 现有 `ingest_trades` 路径，**无需改图表层**。
//! 桥接：阻塞 redis 轮询在独立线程（`tokio::sync::mpsc::blocking_send`）→ async 订阅转发为 Event。
//! 非回测态空闲（不产数据）；run 变更则重连。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use exchange::adapter::{Event, StreamKind};
use exchange::unit::{Price, Qty, UnixMs};
use exchange::{TickerInfo, Trade};
use iced::Subscription;
use iced::futures::SinkExt;

use super::active_run::ActiveRunWatcher;
use super::bt_trades::BtTradeConsumer;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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

            while let Some(trades) = rx.recv().await {
                let stream = StreamKind::Trades { ticker_info };
                if output.send(Event::TradesReceived(stream, UnixMs(now_ms()), trades)).await.is_err()
                {
                    break;
                }
            }
        })
    })
}
