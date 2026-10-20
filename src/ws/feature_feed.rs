//! 订单流特征引擎的图表事件流（docs/31 §8.2）。
//!
//! 读 `<小文件目录>/cockpit/feature_chart.jsonl`（`WS_FEATURE_CHART` 可覆盖）——
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

/// 缺省的 K 线桶（毫秒）：流的周期不是时间周期时用它。与 `replay.rs` 同：逐笔聚合成 K 线发 `KlineReceived`，
/// 让时间基的蜡烛/足迹能建桶、图上标记能对齐 x 轴。时间周期的 K 线按各自的周期聚（[`kline_tfs`]）。
const TF_MS: u64 = 60_000;

/// 一次读多少行就交给渲染端。太小则来回切换开销大，太大则首屏迟迟不出。
const CHUNK: usize = 20_000;

/// 图表流文件。**必须与主仓 `chartfeed::feed_path()` 算出同一个路径。**
///
/// 数据源选了本地回放时读回放写的那份（[`super::feature_source::chart_override`]）——路径变了，
/// 订阅（按路径建）随之重建、从头读。
pub fn feed_path() -> PathBuf {
    if let Some(p) = super::feature_source::chart_override() {
        return p;
    }
    default_feed_path()
}

/// 常驻引擎写的那一份（不管数据源选了什么）。
pub fn default_feed_path() -> PathBuf {
    std::env::var("WS_FEATURE_CHART")
        .map(PathBuf::from)
        .unwrap_or_else(|_| super::paths::hot_dir().join("cockpit").join("feature_chart.jsonl"))
}

/// 订阅最近一次把数据交给图的时刻：`(图表流路径, 那批数据里最新的事件时间 ns, 墙钟)`。
/// 链路徽标据此判断「图表流在长、图却没收到」（上一次 Waiting for data 的那类问题）。
static LAST_RX: std::sync::Mutex<Option<(PathBuf, u64, std::time::Instant)>> = std::sync::Mutex::new(None);

fn mark_received(path: &str, ts_ns: u64) {
    if let Ok(mut g) = LAST_RX.lock() {
        *g = Some((PathBuf::from(path), ts_ns, std::time::Instant::now()));
    }
}

/// `path` 这份图表流最近一次交给图的 `(事件时间 ns, 墙钟)`；换过文件 / 还没收到 = `None`。
#[must_use]
pub fn last_received(path: &std::path::Path) -> Option<(u64, std::time::Instant)> {
    let g = LAST_RX.lock().ok()?;
    g.as_ref().filter(|(p, _, _)| p == path).map(|(_, ts, at)| (*ts, *at))
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
    /// 把攒着的成交发出去（时间线订阅在每个 100ms 格边界插一个，见 [`timeline_subscription`]；JSONL 里没有）。
    Mark { ts_ns: u64 },
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
        // 价或量 ≤ 0 的不是成交（币安 @trade 流的占位消息 `{"p":"0","q":"0"}`，录制器忠实录下了）：
        // 画进图里是一根砸到 0 的尖刺，价格轴被拉到 0 ~ 8 万。写入端已经滤掉，这里再挡一道。
        "trade" => Some(Row::Trade {
            ts_ns: ts(&v),
            px: v["px"].as_f64().filter(|x| *x > 0.0)?,
            qty: v["qty"].as_f64().filter(|x| *x > 0.0)?,
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
#[derive(Clone)]
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

/// K 线流的周期（毫秒）；周期不是时间的（逐笔）按 1 分钟聚。
fn kline_tf_ms(tf: exchange::Timeframe) -> u64 {
    match tf.to_milliseconds() {
        0 => TF_MS,
        ms => ms,
    }
}

/// 这组流里出现的 K 线周期（去重、升序）。
fn kline_tfs(kinds: &[StreamKind]) -> Vec<u64> {
    let mut v: Vec<u64> = kinds
        .iter()
        .filter_map(|k| match k {
            StreamKind::Kline { timeframe, .. } => Some(kline_tf_ms(*timeframe)),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 把一批行交给图（JSONL 与时间线两种订阅共用）：K 线聚合、成交批、深度、检出标记。
///
/// 状态（进行中的 K 线桶、检出）跨批保留；时间线订阅换新实例时从 [`FEED`] 接着用（切走工作区再回来不重灌）。
#[derive(Clone, Default)]
struct Deliver {
    /// 进行中的 K 线桶：按周期（毫秒）各一个——K 线图改成 5 分钟、Footprint 还是 1 分钟，各聚各的
    agg: std::collections::BTreeMap<u64, KlineAgg>,
    dets: Vec<(u64, String, f64, f64, bool)>,
    /// 时间线订阅：这一代的 `(目录, epoch)`——往回跳了就不再投递（JSONL 订阅为 `None`）
    owner: Option<(PathBuf, u64)>,
}

type Out = iced::futures::channel::mpsc::Sender<Event>;

impl Deliver {
    /// 还该不该投递（每发一个事件前看一眼）。
    fn alive(&self) -> bool {
        self.owner.as_ref().is_none_or(|(d, e)| super::replay_clock::feed_alive(d, *e))
    }

    /// 一个周期的 K 线桶（`tf = None`：全部周期）→ 这个周期的流。
    fn klines_of(&self, ev: &mut Vec<Event>, kinds: &[StreamKind], tf: Option<u64>) {
        for k in kinds {
            if let StreamKind::Kline { timeframe, .. } = k {
                let ms = kline_tf_ms(*timeframe);
                if tf.is_some_and(|t| t != ms) {
                    continue;
                }
                if let Some(a) = self.agg.get(&ms) {
                    ev.push(Event::KlineReceived(*k, a.to_kline()));
                }
            }
        }
    }

    /// 攒着的成交发出去（先发进行中的 K 线桶）：按时间聚合的 Footprint 只把成交记进**已经存在**的桶
    /// （`insert_trades_existing_buckets`），桶的 K 线后到的话这批成交在足迹里直接丢掉（最快回放时几乎每一格都丢）。
    fn flush_trades(&self, ev: &mut Vec<Event>, kinds: &[StreamKind], trades: &mut Vec<Trade>, at_ns: u64) {
        if trades.is_empty() {
            return;
        }
        self.klines_of(ev, kinds, None);
        let batch: Box<[Trade]> = std::mem::take(trades).into();
        let at = batch.last().map_or(at_ns, |t| t.time.as_u64() * 1_000_000);
        for k in kinds {
            if let StreamKind::Trades { .. } = k {
                ev.push(Event::TradesReceived(*k, UnixMs(at / 1_000_000), batch.clone()));
            }
        }
    }

    /// 一批行 → 交给图的事件（K 线聚合、成交批、深度；检出攒在 `dets`）。纯计算，状态在 `self`：
    /// 订阅一个个发出去，往回跳之后的一口气重灌在主循环里直接交给图（[`bulk_fill`]），两边是同一串事件。
    fn events(&mut self, kinds: &[StreamKind], rows: Vec<Row>) -> Vec<Event> {
        let mut ev = Vec::with_capacity(rows.len() / 4 + 8);
        let mut trades: Vec<Trade> = Vec::new();
        let tfs = kline_tfs(kinds);
        for row in rows {
            match row {
                Row::Meta(_) => {}
                Row::Trade { ts_ns, px, qty, sell } => {
                    let t = to_trade(ts_ns, px, qty, sell);
                    // K 线聚合（让时间基的图能建桶）：每个 K 线周期各聚一份
                    let p = t.price.to_f32();
                    let q = f32::from(t.qty);
                    let (buy, sl) = if t.is_sell { (0.0, q) } else { (q, 0.0) };
                    for &tf in &tfs {
                        let b = (t.time.as_u64() / tf) * tf;
                        match self.agg.get_mut(&tf) {
                            Some(a) if a.bucket == b => {
                                a.high = a.high.max(p);
                                a.low = a.low.min(p);
                                a.close = p;
                                a.buy += buy;
                                a.sell += sl;
                            }
                            _ => {
                                self.klines_of(&mut ev, kinds, Some(tf));
                                self.agg.insert(tf, KlineAgg { bucket: b, open: p, high: p, low: p, close: p, buy, sell: sl });
                            }
                        }
                    }
                    trades.push(t);
                }
                Row::Depth { ts_ns, bids, asks } => {
                    // 先把攒着的成交发出去，**再发深度**：
                    // 顺序反了的话足迹里这一格的成交会落在下一个簿状态上。
                    self.flush_trades(&mut ev, kinds, &mut trades, ts_ns);
                    let depth = std::sync::Arc::new(to_depth(&bids, &asks));
                    for k in kinds {
                        // 时间用**事件时钟**：热图的时间轴靠它，
                        // 传到达时刻会把 30 分钟压成灌数据的那两秒。
                        if let StreamKind::Depth { .. } = k {
                            ev.push(Event::DepthReceived(*k, UnixMs(ts_ns / 1_000_000), depth.clone()));
                        }
                    }
                }
                Row::Mark { ts_ns } => self.flush_trades(&mut ev, kinds, &mut trades, ts_ns),
                Row::Det { ts_ns, kind, px, qty, sell } => {
                    self.dets.push((ts_ns, kind, px, qty, sell));
                    // 常驻会一直收到检出：不设上限的话这个 Vec 会无界增长，
                    // 且每帧 clone 的成本跟着涨。留最新的 MAX_MARKS 个。
                    if self.dets.len() > MAX_MARKS * 2 {
                        self.dets.drain(..self.dets.len() - MAX_MARKS);
                    }
                }
            }
        }
        self.flush_trades(&mut ev, kinds, &mut trades, 0);
        // 进行中的 K 线桶也发出（latest）。
        self.klines_of(&mut ev, kinds, None);
        ev
    }

    /// 图上的检出标记（最新的 `MAX_MARKS` 个）。
    fn publish_marks(&self) {
        if !self.dets.is_empty() {
            let from = self.dets.len().saturating_sub(MAX_MARKS);
            orders::publish_chart_fills(&detections_to_fills(&self.dets[from..]));
        }
    }

    /// 一批行交给图。返回 `false` = 对端没了（订阅被丢弃）或这一代已经过期。
    async fn rows(&mut self, out: &mut Out, kinds: &[StreamKind], rows: Vec<Row>, rx_path: &str) -> bool {
        // 这批里最新的事件时间：交完之后记下（链路徽标的「图表」一跳）
        let newest = rows
            .iter()
            .filter_map(|r| match r {
                Row::Trade { ts_ns, .. } | Row::Depth { ts_ns, .. } => Some(*ts_ns),
                _ => None,
            })
            .max();
        for e in self.events(kinds, rows) {
            if !self.alive() || out.send(e).await.is_err() {
                return false;
            }
        }
        if let Some(ts) = newest {
            mark_received(rx_path, ts);
        }
        if !self.alive() {
            return false;
        }
        self.publish_marks();
        true
    }
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
                let rx_path = path.clone();

                // 阻塞读盘在独立线程（与 replay.rs 同形状）。
                super::spawn_named("ws-featurefeed", move || {
                    // 文件可能**还不存在**：回放进程要先把数据读进来、推断完刻度才建文件
                    // （本地录制数据要读整段 parquet，要好几秒）。原来打不开就直接退出，而订阅的身份
                    // （路径 + 流）没变、iced 不会再建一次 → 四张图永远 Waiting for data，不报任何错。
                    // 所以等它出现；订阅被丢弃（换了路径 / 关了工作区）就不等了。
                    let mut src = loop {
                        if tx.is_closed() {
                            return;
                        }
                        match JsonlSource::open(PathBuf::from(&path)) {
                            Ok(s) => break s,
                            Err(_) => std::thread::sleep(Duration::from_millis(200)),
                        }
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

                let mut d = Deliver::default();
                while let Some(rows) = rx.recv().await {
                    if !d.rows(&mut output, &kinds, rows, &rx_path).await {
                        return;
                    }
                }
            },
        )
    })
}

// ── 回放时间线（docs/51 P2）──────────────────────────────────────────────────────
//
// 统一播放头（[`super::replay_clock`]）决定灌到哪一刻：读盘线程每 30ms 看一眼播放头，把 `[已灌到, 播放头)` 的
// 成交、深度、检出按 `ti` 排好交给图。往回走由时钟请求清图、换一个 epoch，这条订阅随之被换掉、从头灌。
//
// **批的切法与播放节奏无关**：只灌到 100ms 格上（播放头本来就对齐在格上），并且每过一个 100ms 格边界插一个
// [`Row::Mark`] 把攒着的成交发出去——一路播过来与后退之后一口气重灌，图收到的成交批完全相同，画出来逐位一样。

/// 四张图读哪一种：常驻引擎 / 旧回放的 JSONL 图表流，或回放时间线（目录 + epoch）。
#[derive(Debug, Clone, PartialEq)]
pub enum FeedSpec {
    Jsonl(String),
    Timeline(PathBuf, u64),
}

/// 时间线订阅身份：目录、epoch、流组。
#[derive(Clone)]
pub struct TimelineId {
    pub dir: PathBuf,
    pub epoch: u64,
    pub kinds: Vec<StreamKind>,
}

impl std::hash::Hash for TimelineId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "ws-timeline-feed".hash(state);
        self.dir.hash(state);
        self.epoch.hash(state);
        for k in &self.kinds {
            k.hash(state);
        }
    }
}

/// 上一条时间线订阅灌到哪了：`(目录, epoch, 已灌到 ti, 投递状态)`。换了订阅实例（切走工作区再回来）同一 epoch 接着灌。
static FEED: std::sync::Mutex<Option<(PathBuf, u64, i64, Deliver)>> = std::sync::Mutex::new(None);

const GRID_NS: i64 = 100_000_000;

/// 往回跳之后重灌时，深度快照只灌播放头之前这么久（见 [`bulk_fill`]）。
const BULK_DEPTH_KEEP_NS: i64 = 15 * 60 * 1_000_000_000;

/// `[lo, hi)` 的行（成交、深度、检出按 `ti` 排；同一刻深度在前——深度 `d` 是「`ti < d` 的事件之后」的簿），
/// 每个 100ms 格边界插一个 [`Row::Mark`]。成交至多 `CHUNK` 条，多了就截在某个格边界，返回实际灌到哪。
fn timeline_rows(tl: &super::timeline::Timeline, lo: i64, hi: i64) -> (Vec<Row>, i64) {
    let mut trades = tl.trades(lo, hi, CHUNK);
    let mut hi = hi;
    if trades.len() >= CHUNK {
        // 截在最后一笔所在格的起点（那一格下次整格再灌）；一格里就超过 CHUNK 笔时整格照灌
        let cut = (trades[trades.len() - 1].ti / GRID_NS) * GRID_NS;
        if cut > lo {
            hi = cut;
            trades.retain(|t| t.ti < hi);
        } else {
            hi = (cut + GRID_NS).min(hi);
            trades = tl.trades(lo, hi, usize::MAX);
        }
    }
    let mut rows: Vec<(i64, u8, Row)> = Vec::with_capacity(trades.len() + 16);
    for d in tl.depths(lo, hi) {
        rows.push((d.ti, 0, Row::Depth { ts_ns: d.ti as u64, bids: d.bids, asks: d.asks }));
    }
    for t in trades {
        let sell = match t.side {
            1 => Some(false),
            -1 => Some(true),
            _ => None,
        };
        rows.push((t.ti, 2, Row::Trade { ts_ns: t.ti as u64, px: t.px, qty: t.qty, sell }));
    }
    for e in tl.events(lo, hi, &["det:sweep", "det:iceberg"]) {
        let kind = e.kind.trim_start_matches("det:").to_string();
        rows.push((e.ti, 3, Row::Det { ts_ns: e.ti as u64, kind, px: e.px.unwrap_or(0.0), qty: e.size.unwrap_or(0.0), sell: e.dir < 0 }));
    }
    let mut g = (lo / GRID_NS + 1) * GRID_NS;
    while g <= hi {
        rows.push((g, 1, Row::Mark { ts_ns: g as u64 }));
        g += GRID_NS;
    }
    rows.sort_by_key(|(t, k, _)| (*t, *k));
    (rows.into_iter().map(|(_, _, r)| r).collect(), hi)
}

/// 往回跳之后的一口气重灌（docs/51 P3）：在主循环里把 `[起点, 播放头)` 直接变成交给图的事件，不走订阅消息——
/// 逐条走消息管道时每条都要过一遍 update 与订阅重算，14 分钟的数据实测 8.5 秒。
/// 灌完把「灌到哪」与投递状态记进 [`FEED`]，随后建起来的这一代订阅从那里接着往前灌。
#[must_use]
pub fn bulk_fill(dir: &std::path::Path, epoch: u64, head: i64, kinds: &[StreamKind]) -> Vec<Event> {
    let Ok(tl) = super::timeline::Timeline::open(dir) else { return Vec::new() };
    let hi = (head / GRID_NS) * GRID_NS;
    let mut d = Deliver { owner: Some((dir.to_path_buf(), epoch)), ..Deliver::default() };
    let mut fed = tl.meta.t0;
    let mut out = Vec::new();
    // 比播放头早 BULK_DEPTH_KEEP 以上的深度快照不灌：热图自己只留约 13.6 分钟（8192 个 100ms 桶）的深度，
    // Ladder 只看最后一张——灌进去也会被丢掉，却是重灌里最贵的一项
    let depth_from = hi - BULK_DEPTH_KEEP_NS;
    while fed < hi {
        let (mut rows, upto) = timeline_rows(&tl, fed, hi);
        if upto <= fed {
            break;
        }
        if fed < depth_from {
            rows.retain(|r| !matches!(r, Row::Depth { ts_ns, .. } if (*ts_ns as i64) < depth_from));
        }
        out.extend(d.events(kinds, rows));
        fed = upto;
    }
    d.publish_marks();
    if let Ok(mut g) = FEED.lock() {
        *g = Some((dir.to_path_buf(), epoch, fed, d));
    }
    super::replay_clock::request_rebuild((fed.max(0) / 1_000_000) as u64);
    out
}

/// 建一条时间线订阅。
pub fn timeline_subscription(dir: PathBuf, epoch: u64, kinds: Vec<StreamKind>) -> Subscription<Event> {
    Subscription::run_with(TimelineId { dir, epoch, kinds }, |id: &TimelineId| {
        let (dir, epoch, kinds) = (id.dir.clone(), id.epoch, id.kinds.clone());
        iced::stream::channel(256, move |mut output: Out| async move {
            // (行, 灌到哪, 这一批之后追上了播放头)
            let (tx, mut rx) = tokio::sync::mpsc::channel::<(Vec<Row>, i64, bool)>(4);
            // 同一 epoch 接着灌；否则从头
            let resume = FEED.lock().ok().and_then(|g| g.clone()).filter(|(d, e, _, _)| *d == dir && *e == epoch);
            let (from, mut d) = resume.map_or((i64::MIN, Deliver::default()), |(_, _, f, d)| (f, d));
            d.owner = Some((dir.clone(), epoch));
            let rx_path = dir.join("tl_trades.bin").to_string_lossy().into_owned();
            let tdir = dir.clone();
            super::spawn_named("ws-timelinefeed", move || {
                let tl = loop {
                    if tx.is_closed() {
                        return;
                    }
                    match super::timeline::Timeline::open(&tdir) {
                        Ok(t) => break t,
                        Err(_) => std::thread::sleep(Duration::from_millis(200)),
                    }
                };
                let mut fed = if from == i64::MIN { tl.meta.t0 } else { from };
                loop {
                    if tx.is_closed() {
                        break;
                    }
                    // 时钟换了目录 / epoch：这条订阅作废（上层马上换新的）
                    if !super::replay_clock::feed_alive(&tdir, epoch) {
                        break;
                    }
                    let Some((_, head, _)) = super::replay_clock::target() else { break };
                    let hi = (head / GRID_NS) * GRID_NS;
                    if hi > fed {
                        let (rows, upto) = timeline_rows(&tl, fed, hi);
                        if tx.blocking_send((rows, upto, upto >= hi)).is_err() {
                            break;
                        }
                        fed = upto;
                    } else {
                        std::thread::sleep(Duration::from_millis(30));
                    }
                }
            });
            // 从头灌的（开回放 / 往回跳之后）：第一次追上播放头时请热图从历史整体重画
            let mut want_rebuild = from == i64::MIN;
            while let Some((rows, upto, caught_up)) = rx.recv().await {
                if !d.rows(&mut output, &kinds, rows, &rx_path).await {
                    return;
                }
                if want_rebuild && caught_up {
                    want_rebuild = false;
                    super::replay_clock::request_rebuild((upto.max(0) / 1_000_000) as u64);
                }
                if let Ok(mut g) = FEED.lock() {
                    *g = Some((dir.clone(), epoch, upto, d.clone()));
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::Timeframe;
    use std::hash::{Hash, Hasher};

    /// 录制数据里的币安占位消息（价 / 量为 0）不是成交，不进图。
    #[test]
    fn 零价零量的占位成交不进图() {
        assert!(parse_row(r#"{"t":"trade","ts":1,"px":0,"qty":0,"sell":null}"#).is_none());
        assert!(parse_row(r#"{"t":"trade","ts":1,"px":83000.5,"qty":0,"sell":true}"#).is_none());
        assert!(parse_row(r#"{"t":"trade","ts":1,"px":83000.5,"qty":0.01,"sell":true}"#).is_some());
    }
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
