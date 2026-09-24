//! 订单流特征引擎的图表事件流（docs/31 §8.2）。
//!
//! 读 `~/ws-data/cockpit/feature_chart.jsonl`（`WS_FEATURE_CHART` 可覆盖）——
//! 由主仓 `wealthspring-features` 的 `ChartFeedWriter` 写出，内容是**引擎看到的那条流**：
//! 逐笔成交 + 按 250ms 节流的深度快照（取自引擎重建出来的簿）+ 扫单/冰山检出。
//! 转成 `exchange::Event` 喂现有的 Footprint / 热图 / Ladder / Tape，**不改图表层一行**。
//!
//! ## 为什么不让图自己去读 events.csv
//!
//! 那会在面板侧再造一条 L2 重建路径，于是图上画的簿与 `obi_l5` 这些特征读的簿
//! 不是同一本。§8.2 要的恰恰是「图上看到的那一笔，就是引擎算出那个值的那一笔」——
//! 与 W7 面板不重算特征值是同一条纪律（docs/31 §9.8）。
//!
//! ## 时间戳用事件时钟，不用到达时刻
//!
//! `DepthReceived` 的 `UnixMs` 是热图时间轴上的采样时刻。这里传**文件里的事件时间**，
//! 于是灌得多快都不影响图：30 分钟的数据在两秒内喂完，画出来仍是 30 分钟。
//! 传 `now_ms()` 的话整段历史会被压缩成「刚刚这两秒」。
//!
//! ## 源无关
//!
//! [`ChartSource`] 是回放与常驻引擎的共同接口，而 [`JsonlSource`] **两种都覆盖**：
//! 回放路径由 `examples/replay_events_csv.rs` 写完整一份，常驻路径由
//! `ws_features` 守护持续追加——面板侧读的是同一个文件、同一个格式，
//! 一行都不用分情况。文件增长就继续读，被轮转（换了一份新的、更短）就从头再读。
//!
//! 也就是说「换数据源」这件事在面板这一侧是**不存在的**：
//! 起不起那个守护，决定的是文件由谁在写，不是面板怎么读。

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use exchange::adapter::{Event, StreamKind};
use exchange::depth::Depth;
use exchange::unit::{Price, Qty, UnixMs};
use exchange::{Kline, Trade, Volume};
use iced::Subscription;
use iced::futures::SinkExt;

use super::orders::{self, Fill};

/// M1 桶大小（毫秒）。与 `replay.rs` 同：逐笔聚合成 M1 K 线发 `KlineReceived`，
/// 让时间基的蜡烛/足迹能建桶、图上标记能对齐 x 轴。
const TF_MS: u64 = 60_000;

/// 一次读多少行就交给渲染端。太小则来回切换开销大，太大则首屏迟迟不出。
const CHUNK: usize = 20_000;

/// 图表流文件。**必须与主仓 `chartfeed::feed_path()` 算出同一个路径。**
pub fn feed_path() -> PathBuf {
    std::env::var("WS_FEATURE_CHART")
        .map(PathBuf::from)
        .unwrap_or_else(|_| super::paths::data_dir().join("cockpit").join("feature_chart.jsonl"))
}

/// 文件头。图表靠 `tick_size` / `min_qty` 定价格轴刻度与聚合桶。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub symbol: String,
    pub tick_size: f64,
    pub min_qty: f64,
}

/// 流里的一行。
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Meta(Meta),
    Trade {
        ts_ns: u64,
        px: f64,
        qty: f64,
        /// `None` = 成交没带方向。**不能当成卖**——那会在 Footprint 上
        /// 制造一边倒的假象（主仓侧同一条纪律）。
        sell: Option<bool>,
    },
    Depth {
        ts_ns: u64,
        bids: Vec<(f64, f64)>,
        asks: Vec<(f64, f64)>,
    },
    Det {
        ts_ns: u64,
        kind: String,
        px: f64,
        qty: f64,
        sell: bool,
    },
}

/// 解析一行 JSONL。认不出的行返回 `None` 而不是 panic——
/// 流文件可能正被引擎追加，最后一行有可能只写了一半。
pub fn parse_row(line: &str) -> Option<Row> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let ts = |v: &serde_json::Value| v["ts"].as_u64().unwrap_or(0);
    match v["t"].as_str()? {
        "meta" => Some(Row::Meta(Meta {
            symbol: v["symbol"].as_str().unwrap_or_default().to_string(),
            tick_size: v["tick_size"].as_f64().unwrap_or(0.0),
            min_qty: v["min_qty"].as_f64().unwrap_or(0.0),
        })),
        "trade" => Some(Row::Trade {
            ts_ns: ts(&v),
            px: v["px"].as_f64()?,
            qty: v["qty"].as_f64()?,
            // `as_bool()` 对 JSON `null` 返回 None——正是要的语义。
            sell: v["sell"].as_bool(),
        }),
        "depth" => {
            let lv = |k: &str| -> Vec<(f64, f64)> {
                v[k].as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| Some((x[0].as_f64()?, x[1].as_f64()?)))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Some(Row::Depth {
                ts_ns: ts(&v),
                bids: lv("bids"),
                asks: lv("asks"),
            })
        }
        "det" => Some(Row::Det {
            ts_ns: ts(&v),
            kind: v["kind"].as_str().unwrap_or_default().to_string(),
            px: v["px"].as_f64().unwrap_or(0.0),
            qty: v["qty"].as_f64().unwrap_or(0.0),
            sell: v["sell"].as_bool().unwrap_or(false),
        }),
        _ => None,
    }
}

/// 回放与常驻引擎的共同接口（§8.2 的源无关要求）。
pub trait ChartSource {
    /// 取下一批行。`None` = 源已结束且不会再有数据。
    fn poll(&mut self) -> Option<Vec<Row>>;
}

/// 读 JSONL 文件。文件增长就继续读，所以它同时是「回放读完整一份」与
/// 「tail 一个常驻引擎正在写的文件」两种用法。
pub struct JsonlSource {
    reader: BufReader<std::fs::File>,
    path: PathBuf,
    /// 已消费到的字节位置。文件**变短**说明被重写了（重跑了一次回放），
    /// 此时必须从头再读——否则会从旧的偏移继续，读到的是新文件中间的半行。
    pos: u64,
    done: bool,
}

impl JsonlSource {
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let f = std::fs::File::open(&path)?;
        Ok(Self {
            reader: BufReader::with_capacity(1 << 20, f),
            path,
            pos: 0,
            done: false,
        })
    }

    fn reopen_if_truncated(&mut self) -> std::io::Result<()> {
        let len = std::fs::metadata(&self.path)?.len();
        if len < self.pos {
            let f = std::fs::File::open(&self.path)?;
            self.reader = BufReader::with_capacity(1 << 20, f);
            self.pos = 0;
        }
        Ok(())
    }
}

impl ChartSource for JsonlSource {
    fn poll(&mut self) -> Option<Vec<Row>> {
        if self.done {
            return None;
        }
        if self.reopen_if_truncated().is_err() {
            self.done = true;
            return None;
        }
        let mut out = Vec::with_capacity(CHUNK.min(4096));
        let mut line = String::new();
        while out.len() < CHUNK {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    // 半行：引擎正在写，还没写完这一行。**回退**到行首下次再读，
                    // 否则这一行会被当成坏行永久丢掉。
                    if !line.ends_with('\n') {
                        let _ = self.reader.seek(SeekFrom::Start(self.pos));
                        break;
                    }
                    self.pos += n as u64;
                    if let Some(r) = parse_row(line.trim_end()) {
                        out.push(r);
                    }
                }
                Err(_) => break,
            }
        }
        Some(out)
    }
}

/// 订阅身份。路径与它服务的那组流都要进——
/// 换了文件或换了图，iced 必须丢掉旧订阅重建（`replay.rs` 那条教训：
/// 「丢弃来源不符的帧」要靠**结构性保证**，不靠运行时比对）。
#[derive(Clone)]
pub struct FeedId {
    pub path: String,
    pub kinds: Vec<StreamKind>,
}

impl std::hash::Hash for FeedId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "ws-feature-feed".hash(state);
        self.path.hash(state);
        for k in &self.kinds {
            k.hash(state);
        }
    }
}

fn to_trade(ts_ns: u64, px: f64, qty: f64, sell: Option<bool>) -> Trade {
    Trade {
        time: UnixMs(ts_ns / 1_000_000),
        // 缺方向的成交画成买：Footprint 的单元格只有买/卖两栏，没有第三栏。
        // 选买是因为 `Aggressor::None` 在录制里只占 0.4%（381/92881），
        // 且主仓侧已经把它标成 null——**这里是显示上的取舍，不是把缺失当成买**。
        is_sell: sell.unwrap_or(false),
        price: Price::from_f32(px as f32),
        qty: Qty::from_f32(qty as f32),
    }
}

fn to_depth(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Depth {
    let mut d = Depth::default();
    for (p, q) in bids {
        d.bids.insert(Price::from_f32(*p as f32), Qty::from_f32(*q as f32));
    }
    for (p, q) in asks {
        d.asks.insert(Price::from_f32(*p as f32), Qty::from_f32(*q as f32));
    }
    d
}

/// 逐笔聚合成 M1 K 线（与 `replay.rs` 同一形状）。
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

/// 图上最多画多少个检出标记。
///
/// `orders::chart_fills_snapshot` 每帧 clone 一次，其文档写明「≤500 条」。
/// 常驻场景下检出会一直来（30 分钟录制里就有 2840 个），不设上限的话
/// 这个 Vec 会一直长，而且每帧 clone 的成本也一直涨。
///
/// 超出时**留最新的**——人看的是图的右边。
pub const MAX_MARKS: usize = 500;

/// 把检出转成图上的 ▲▼ 标记。
///
/// 复用 docs/08 F3b 的 `CHART_FILLS` 通道——它本来画的是回测成交，
/// 这里画的是扫单/冰山。两者不会同时出现：特征可视化工作区里没有回测在跑。
pub fn detections_to_fills(dets: &[(u64, String, f64, f64, bool)]) -> Vec<Fill> {
    dets.iter()
        .enumerate()
        .map(|(i, (ts, _kind, px, qty, sell))| Fill {
            side: if *sell { 2 } else { 1 },
            px: *px,
            qty: *qty,
            ts: ts / 1_000_000,
            seq: i as u64 + 1,
        })
        .collect()
}

/// 建一条特征图表流订阅。
pub fn subscription(path: String, kinds: Vec<StreamKind>) -> Subscription<Event> {
    Subscription::run_with(FeedId { path, kinds }, |id: &FeedId| {
        let path = id.path.clone();
        let kinds = id.kinds.clone();
        iced::stream::channel(
            256,
            move |mut output: iced::futures::channel::mpsc::Sender<Event>| async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<Row>>(8);

                // 阻塞读盘在独立线程（与 replay.rs 同形状）。
                std::thread::spawn(move || {
                    let Ok(mut src) = JsonlSource::open(PathBuf::from(&path)) else {
                        return;
                    };
                    loop {
                        // 对端存活探测每轮无条件做一次：文件读完后这个线程会长期
                        // 空转等待追加，订阅被 drop 时必须自己发现（replay.rs 那条教训）。
                        if tx.is_closed() {
                            break;
                        }
                        match src.poll() {
                            None => break,
                            Some(rows) if rows.is_empty() => {
                                // 读到文件尾：等引擎追加。回放场景下就一直等着，
                                // 不再产生任何事件（也不空转 CPU）。
                                std::thread::sleep(Duration::from_millis(400));
                            }
                            Some(rows) => {
                                if tx.blocking_send(rows).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });

                let mut agg: Option<KlineAgg> = None;
                let mut dets: Vec<(u64, String, f64, f64, bool)> = Vec::new();
                while let Some(rows) = rx.recv().await {
                    let mut trades: Vec<Trade> = Vec::new();
                    for row in rows {
                        match row {
                            Row::Meta(_) => {}
                            Row::Trade { ts_ns, px, qty, sell } => {
                                let t = to_trade(ts_ns, px, qty, sell);
                                // K 线聚合（让时间基的图能建桶）。
                                let b = (t.time.as_u64() / TF_MS) * TF_MS;
                                let p = t.price.to_f32();
                                let q = f32::from(t.qty);
                                let (buy, sl) = if t.is_sell { (0.0, q) } else { (q, 0.0) };
                                match agg.as_mut() {
                                    Some(a) if a.bucket == b => {
                                        a.high = a.high.max(p);
                                        a.low = a.low.min(p);
                                        a.close = p;
                                        a.buy += buy;
                                        a.sell += sl;
                                    }
                                    _ => {
                                        if let Some(a) = agg.as_ref() {
                                            for k in &kinds {
                                                if let StreamKind::Kline { .. } = k
                                                    && output
                                                        .send(Event::KlineReceived(*k, a.to_kline()))
                                                        .await
                                                        .is_err()
                                                {
                                                    return;
                                                }
                                            }
                                        }
                                        agg = Some(KlineAgg {
                                            bucket: b,
                                            open: p,
                                            high: p,
                                            low: p,
                                            close: p,
                                            buy,
                                            sell: sl,
                                        });
                                    }
                                }
                                trades.push(t);
                            }
                            Row::Depth { ts_ns, bids, asks } => {
                                // 先把攒着的成交发出去，**再发深度**：
                                // 顺序反了的话足迹里这一格的成交会落在下一个簿状态上。
                                if !trades.is_empty() {
                                    let batch: Box<[Trade]> = std::mem::take(&mut trades).into();
                                    let at = batch.last().map_or(ts_ns, |t| t.time.as_u64() * 1_000_000);
                                    for k in &kinds {
                                        if let StreamKind::Trades { .. } = k
                                            && output
                                                .send(Event::TradesReceived(
                                                    *k,
                                                    UnixMs(at / 1_000_000),
                                                    batch.clone(),
                                                ))
                                                .await
                                                .is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                                let depth = std::sync::Arc::new(to_depth(&bids, &asks));
                                for k in &kinds {
                                    // 时间用**事件时钟**：热图的时间轴靠它，
                                    // 传到达时刻会把 30 分钟压成灌数据的那两秒。
                                    if let StreamKind::Depth { .. } = k
                                        && output
                                            .send(Event::DepthReceived(
                                                *k,
                                                UnixMs(ts_ns / 1_000_000),
                                                depth.clone(),
                                            ))
                                            .await
                                            .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                            Row::Det { ts_ns, kind, px, qty, sell } => {
                                dets.push((ts_ns, kind, px, qty, sell));
                                // 常驻会一直收到检出：不设上限的话这个 Vec 会无界增长，
                                // 且每帧 clone 的成本跟着涨。留最新的 MAX_MARKS 个。
                                if dets.len() > MAX_MARKS * 2 {
                                    dets.drain(..dets.len() - MAX_MARKS);
                                }
                            }
                        }
                    }
                    if !trades.is_empty() {
                        let batch: Box<[Trade]> = trades.into();
                        let at = batch.last().map_or(0, |t| t.time.as_u64());
                        for k in &kinds {
                            if let StreamKind::Trades { .. } = k
                                && output
                                    .send(Event::TradesReceived(*k, UnixMs(at), batch.clone()))
                                    .await
                                    .is_err()
                            {
                                return;
                            }
                        }
                    }
                    // 进行中的 K 线桶也发出（latest）。
                    if let Some(a) = agg.as_ref() {
                        for k in &kinds {
                            if let StreamKind::Kline { .. } = k
                                && output.send(Event::KlineReceived(*k, a.to_kline())).await.is_err()
                            {
                                return;
                            }
                        }
                    }
                    if !dets.is_empty() {
                        let from = dets.len().saturating_sub(MAX_MARKS);
                        orders::publish_chart_fills(&detections_to_fills(&dets[from..]));
                    }
                }
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::Timeframe;
    use std::hash::{Hash, Hasher};
    use std::io::Write;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-feed-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("feed.jsonl")
    }

    const SAMPLE: &str = concat!(
        r#"{"t":"meta","symbol":"BTCUSDT","tick_size":0.1,"min_qty":0.001,"depth_levels":50}"#,
        "\n",
        r#"{"t":"trade","ts":1789778999455000000,"px":81262,"qty":0.001,"sell":false}"#,
        "\n",
        r#"{"t":"trade","ts":1789778999456000000,"px":81261,"qty":0.5,"sell":true}"#,
        "\n",
        r#"{"t":"trade","ts":1789778999457000000,"px":81261,"qty":0.2,"sell":null}"#,
        "\n",
        r#"{"t":"depth","ts":1789778999792000000,"bids":[[81261.9,3.5],[81260.0,1.0]],"asks":[[81262.0,2.0]]}"#,
        "\n",
        r#"{"t":"det","ts":1789779000000000000,"kind":"sweep","px":81263.0,"qty":1.5,"sell":false}"#,
        "\n",
    );

    #[test]
    fn 解析四种行() {
        let rows: Vec<Row> = SAMPLE.lines().filter_map(parse_row).collect();
        // meta + 三笔成交 + 一份深度 + 一个检出。
        assert_eq!(rows.len(), 6);
        assert!(matches!(&rows[0], Row::Meta(m) if m.symbol == "BTCUSDT" && m.tick_size == 0.1));
        assert!(matches!(rows[5], Row::Det { .. }));
        let Row::Depth { bids, asks, .. } = &rows[4] else {
            panic!("第五行该是 depth")
        };
        assert_eq!(bids.len(), 2);
        assert_eq!(asks.len(), 1);
    }

    #[test]
    fn 缺方向的成交不能被读成卖() {
        // 主仓侧特意把它写成 null；这里读成 `true` 的话，Footprint 上
        // 那一格会凭空多出卖量——一张看起来完全正常的图。
        let rows: Vec<Row> = SAMPLE.lines().filter_map(parse_row).collect();
        let sells: Vec<Option<bool>> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Trade { sell, .. } => Some(*sell),
                _ => None,
            })
            .collect();
        assert_eq!(sells, vec![Some(false), Some(true), None]);
    }

    #[test]
    fn 坏行被跳过而不是让整条流停下() {
        // 引擎正在追加时最后一行可能只写了一半。
        assert!(parse_row("{半行").is_none());
        assert!(parse_row("").is_none());
        assert!(parse_row(r#"{"t":"没见过的类型"}"#).is_none());
        // 好行照常解析。
        assert!(parse_row(r#"{"t":"trade","ts":1,"px":1.0,"qty":2.0,"sell":false}"#).is_some());
    }

    #[test]
    fn 深度转换保持买高卖低与档位() {
        let d = to_depth(&[(81261.9, 3.5), (81260.0, 1.0)], &[(81262.0, 2.0)]);
        assert_eq!(d.bids.len(), 2);
        assert_eq!(d.asks.len(), 1);
        // BTreeMap 按价格升序——最优买是最大的那个 key，最优卖是最小的。
        let best_bid = d.bids.keys().next_back().unwrap().to_f32();
        let best_ask = d.asks.keys().next().unwrap().to_f32();
        assert!(best_bid < best_ask, "转换之后买卖反了：{best_bid} / {best_ask}");
    }

    #[test]
    fn 时间戳从纳秒转成毫秒() {
        // 直接把纳秒当毫秒会让 K 线落到公元 58000 年，图上一片空白。
        let t = to_trade(1_789_778_999_455_000_000, 81262.0, 0.001, Some(false));
        assert_eq!(t.time.as_u64(), 1_789_778_999_455);
        // 2026 年前后的毫秒时间戳量级（13 位）。
        assert_eq!(t.time.as_u64().to_string().len(), 13);
    }

    #[test]
    fn 读文件与半行回退() {
        let p = tmp("partial");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            // 最后一行故意不带换行——模拟引擎正写到一半。
            write!(f, "{}", &SAMPLE[..SAMPLE.len() - 1]).unwrap();
        }
        let mut src = JsonlSource::open(p.clone()).unwrap();
        let rows = src.poll().unwrap();
        // 半行不该被消费；其余五行正常。
        assert_eq!(rows.len(), 5, "半行被当成完整行读了");
        // 补上换行之后，那一行必须**还在**——不能被永久丢掉。
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            writeln!(f).unwrap();
        }
        let rows2 = src.poll().unwrap();
        assert_eq!(rows2.len(), 1, "回退后没把那一行补读回来");
        assert!(matches!(rows2[0], Row::Det { .. }));
    }

    #[test]
    fn 文件被重写后从头再读() {
        // 重跑一次回放会把文件截短。从旧偏移继续读会落在新文件的半行中间。
        let p = tmp("truncate");
        std::fs::write(&p, SAMPLE).unwrap();
        let mut src = JsonlSource::open(p.clone()).unwrap();
        assert_eq!(src.poll().unwrap().len(), 6);
        std::fs::write(&p, &SAMPLE[..120]).unwrap();
        let rows = src.poll().unwrap();
        assert!(!rows.is_empty(), "文件被重写后一行都没读到");
        assert!(matches!(&rows[0], Row::Meta(_)), "没有从头读");
    }

    #[test]
    fn 检出标记有上限且留最新的() {
        // 常驻会一直收到检出。`chart_fills_snapshot` 每帧 clone，其文档写明 ≤500，
        // 不封顶的话这个 Vec 会无界增长、每帧成本也一直涨。
        let dets: Vec<(u64, String, f64, f64, bool)> = (0..1_500u64)
            .map(|i| (i * 1_000_000, "sweep".to_string(), 100.0 + i as f64, 1.0, false))
            .collect();
        let from = dets.len().saturating_sub(MAX_MARKS);
        let fills = detections_to_fills(&dets[from..]);
        assert_eq!(fills.len(), MAX_MARKS);
        // 留的是最新的那一段——人看的是图的右边。
        assert_eq!(fills[fills.len() - 1].ts, 1_499);
        assert_eq!(fills[0].ts, 1_000);
    }

    #[test]
    fn 检出转成图上标记的方向与序号() {
        let dets = vec![
            (1_000_000_000u64, "sweep".to_string(), 100.0, 1.0, false),
            (2_000_000_000u64, "iceberg".to_string(), 101.0, 0.0, true),
        ];
        let fills = detections_to_fills(&dets);
        assert_eq!(fills[0].side, 1, "买方扫单该是 ▲");
        assert_eq!(fills[1].side, 2, "卖方该是 ▼");
        assert_eq!(fills[0].ts, 1_000, "标记时间也要转成毫秒");
        assert_eq!(fills[0].seq, 1);
        assert_eq!(fills[1].seq, 2);
    }

    fn hash_of(id: &FeedId) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        h.finish()
    }

    #[test]
    fn 路径与流组都进订阅身份() {
        // replay.rs 那条教训：「丢弃来源不符的帧」要靠结构性保证。
        // 换了文件或换了图，iced 必须丢掉旧订阅重建。
        use exchange::Ticker;
        use exchange::adapter::Exchange;
        let ti = exchange::TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        );
        let a = FeedId {
            path: "/a.jsonl".into(),
            kinds: vec![StreamKind::Trades { ticker_info: ti }],
        };
        let b = FeedId {
            path: "/b.jsonl".into(),
            kinds: a.kinds.clone(),
        };
        let c = FeedId {
            path: "/a.jsonl".into(),
            kinds: vec![StreamKind::Kline {
                ticker_info: ti,
                timeframe: Timeframe::M1,
            }],
        };
        assert_ne!(hash_of(&a), hash_of(&b), "换文件必须换身份");
        assert_ne!(hash_of(&a), hash_of(&c), "换流组必须换身份");
        assert_eq!(hash_of(&a), hash_of(&a.clone()));
    }
}
