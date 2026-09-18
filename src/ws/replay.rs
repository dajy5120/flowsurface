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
    /// 当前活动 run。**必须进订阅身份**（docs/27 §12）：run 一变 iced 就重建订阅，
    /// 新消费端从 `"0"` 重读整条流——于是「清空图表」与「重放数据」有了确定的先后
    /// （清空在 `update()` 里发生，订阅重建在其之后），不再相互竞争。
    ///
    /// 不带它的话：清空与 replay 各由一条独立线程触发，清空若晚于首批数据到达，
    /// 那批数据被抹掉且 `last_id` 已前移，**永远不会重发**——图上就此空着。
    pub run: String,
}

impl std::hash::Hash for ReplayId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "ws-bt-replay".hash(state);
        self.redis_url.hash(state);
        self.ticker.ticker.hash(state);
        self.run.hash(state);
    }
}

/// 为某 ticker 建一条回测 replay 订阅。builder 必须非捕获（fn ptr）：参数经 ReplayId 传入，
/// 闭包从 id 取（不捕获环境）；返回的流不借用 id（内部 clone/copy）。
pub fn subscription(redis_url: String, ticker_info: TickerInfo, run: String) -> Subscription<Event> {
    Subscription::run_with(ReplayId { redis_url, ticker: ticker_info, run }, |id: &ReplayId| {
        let redis_url = id.redis_url.clone();
        let ticker_info = id.ticker;
        let run = id.run.clone();
        iced::stream::channel(256, move |mut output: iced::futures::channel::mpsc::Sender<Event>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Box<[Trade]>>(64);

            // 阻塞线程：只读**本订阅身份对应的那一个 run**，转 Trade 批量回传。
            //
            // 这里以前还自己轮询 `ws:active_run` 并在 run 变化时换 consumer——
            // 与 `ReplayId.run` 进订阅身份是**同一件事的两套机制**，而且互相打架：
            // 线程可以悄悄切去服务另一个 run，于是这条订阅发出的帧与它的身份不符，
            // 上一次运行的成交会落进本次的图里（docs/28 §4.4）。
            //
            // 现在只有一套：run 变 → App 重发 → `ReplayId` 变 → iced 丢掉旧订阅重建新的。
            // 「丢弃来源不符的帧」因此是**结构性保证**，不靠运行时比对。
            std::thread::spawn(move || {
                if run.is_empty() {
                    return; // 没有活动回测：本订阅无事可做，直接退出而不是空转轮询
                }
                let mut consumer = BtTradeConsumer::connect(&redis_url, &run).ok();
                loop {
                    // 对端存活探测：**每轮无条件做一次**。
                    // 原先唯一的检测点是 `tx.blocking_send(..).is_err()`，而它嵌在
                    // 「值发生变化」的分支里——值冻住时（恰恰是上游守护挂掉的表现）
                    // 这个线程永远发现不了订阅已被销毁，以固定间隔永久空转。
                    if tx.is_closed() {
                        break;
                    }
                    if consumer.is_none() {
                        consumer = BtTradeConsumer::connect(&redis_url, &run).ok();
                    }
                    let Some(c) = consumer.as_mut() else {
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    };
                    match c.poll() {
                        Ok(b) if !b.is_empty() => {
                            let trades: Box<[Trade]> = b
                                .iter()
                                // 定点直传：不经 f32/f64（docs/28 §5.2）。
                                .map(|t| Trade {
                                    time: UnixMs(t.ts),
                                    is_sell: t.side == 2,
                                    price: t.price(),
                                    qty: t.qty(),
                                })
                                .collect();
                            if tx.blocking_send(trades).is_err() {
                                break; // 订阅已 drop
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            consumer = None; // 下一轮重连同一个 run，不改服务对象
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

    use crate::ws::bt_trades::bt_trades_key;

    fn hash_of(id: &ReplayId) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        h.finish()
    }

    fn id(run: &str) -> ReplayId {
        use exchange::adapter::Exchange;
        use exchange::Ticker;
        ReplayId {
            redis_url: "redis://127.0.0.1:6379".into(),
            ticker: TickerInfo::new(Ticker::new("BTCUSDT", Exchange::BinanceLinear), 0.1, 0.001, None),
            run: run.into(),
        }
    }

    /// run 必须进订阅身份——这是「丢弃来源不符的帧」的**结构性保证**（docs/28 §4.4）。
    ///
    /// 身份里带了 run，iced 才会在 run 变化时丢掉旧订阅重建新的；旧订阅连同它那条
    /// 到 `ws:bt:{旧run}:trades` 的连接一起消失，上一次的成交不可能再落进本次的图。
    /// 不带的话就得在运行时逐帧比对，而帧本身不携带 run——比无可比。
    #[test]
    fn run_进订阅身份() {
        assert_ne!(hash_of(&id("BT-1")), hash_of(&id("BT-2")), "run 不同必须换身份");
        assert_eq!(hash_of(&id("BT-1")), hash_of(&id("BT-1")), "同 run 必须同身份");
        assert_ne!(hash_of(&id("")), hash_of(&id("BT-1")), "空 run（无活动回测）也是一种身份");
    }

    #[test]
    fn 流键按_run_分开() {
        assert_eq!(bt_trades_key("BT-1"), "ws:bt:BT-1:trades");
        assert_ne!(bt_trades_key("BT-1"), bt_trades_key("BT-2"));
    }
}
