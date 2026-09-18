//! 币安钱包预测市场的实时读数（预测市场面板的 Binance 视图）。
//!
//! 读 `~/ws-data/live/pm_binance.json`——由 `factory.prediction.pm_recorder` 兼职写出。
//!
//! **为什么由录制器写而不是面板自己连**：录制器本来就握着 WS 实时流，面板再连一条
//! 等于对同一份行情建两个真相源（还多占一份 WS 配额）。面板只读快照，零交易所流——
//! 与 radar/news/prediction 几个面板同一模式。
//!
//! 快照里只有「现在是什么样」。历史在 `raw/pm_book` 与 `raw/pm_round` 的 parquet 里，
//! 要看历史该去读那个，不该让这份每秒重写的小 JSON 背上历史。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 当前这一轮。
#[derive(Default, Clone)]
pub struct RoundView {
    pub market_id: String,
    pub question: String,
    /// 距结算还有多少秒；`-1` = 未知。
    pub secs_left: i64,
    pub start_price: f64,
    pub fee_bps: f64,
    pub participants: i64,
    pub volume: f64,
    pub liquidity: f64,
}

/// 一本簿的完整档位（面板画梯形图用）。
#[derive(Default, Clone)]
pub struct Ladder {
    /// `(价, 量)`，价降序（最优买价在前）。
    pub bids: Vec<(f64, f64)>,
    /// `(价, 量)`，价升序（最优卖价在前）。
    pub asks: Vec<(f64, f64)>,
    /// 全簿总档数——快照只给前若干档，这个说明被截掉了多少。
    pub n_bids: usize,
    pub n_asks: usize,
}

impl Ladder {
    /// 各档**金额**的最大值，用来定梯形图条的满格宽度。
    ///
    /// 用金额不用份数：便宜档能堆出很大的份数，按份数定标会让 0.02 那档撑满整行，
    /// 把真正有钱的档压成看不见的一条。这和失衡度用金额是同一个理由。
    pub fn max_notional(&self) -> f64 {
        self.bids
            .iter()
            .chain(self.asks.iter())
            .map(|(p, z)| p * z)
            .fold(0.0_f64, f64::max)
    }
}

/// 一轮已结束（或进行中）的结果。
#[derive(Default, Clone)]
pub struct RecentRound {
    pub market_id: String,
    pub start_ms: i64,
    /// `"UP"` / `"DOWN"` / 空 = 还没判出来。**空不等于平局**，是没有结论。
    pub resolved: String,
    pub start_price: f64,
    pub participants: i64,
    pub volume: f64,
}

/// 两本簿的当前状态。
#[derive(Default, Clone)]
pub struct BookView {
    /// 按**金额**的失衡度（默认口径）。
    pub imbalance: Option<f64>,
    /// 按**份数**的失衡度——**仅供对照**。实测它会指反：临近结算时便宜一侧
    /// 用很少的钱就能堆出大量份额，那是彩票式挂单不是共识。
    pub imbalance_shares: Option<f64>,
    pub up_bid: Option<f64>,
    pub up_ask: Option<f64>,
    pub down_bid: Option<f64>,
    pub down_ask: Option<f64>,
    pub up_notional: f64,
    pub down_notional: f64,
    pub up_levels: (i64, i64),
    pub down_levels: (i64, i64),
    /// 押涨那本的完整档位。
    pub up: Ladder,
    /// 押跌那本的完整档位。**是独立的一本簿，不是 Up 的另一侧**——
    /// 两者只是部分镜像，差额正是「只挂在这一侧的原生单」。
    pub down: Ladder,
}

/// 录制状态。**放在面板上是有意的**：这份数据没有第三方历史源，
/// 录制断了就是永久缺口，必须一眼能看见它还在不在录。
#[derive(Default, Clone)]
pub struct RecView {
    pub book_rows: i64,
    pub rounds: i64,
    pub ws_msgs: i64,
    pub resubs: i64,
    pub up_stale_ms: i64,
    pub down_stale_ms: i64,
    pub last_error: String,
}

/// 预测市场流「超过多久算一个洞」。
///
/// **比行情流的 1 秒宽**：Down 那本走 REST，本来就是 1 秒一拍，网络抖动让某一拍
/// 变成 1.1 秒是常态。用 1 秒阈值会把正常采样间隔全判成洞，算出来的「最长可跑窗口」
/// 会碎到毫无意义。3 秒 = 漏掉三拍才算断。
pub const PM_GAP_NS: i64 = 3_000_000_000;

/// 某一天录到了什么。
#[derive(Default, Clone)]
pub struct PmDay {
    pub date: String,
    /// 封好的段数。
    pub segs: usize,
    pub bytes: u64,
    /// 覆盖秒数、最长无洞段、缺口数——口径与「数据录制」页的按日覆盖一致。
    pub covered_s: i64,
    pub longest_s: i64,
    pub longest_at: i64,
    pub gaps: usize,
    /// 录到的连续段，`(起, 止)` 纳秒。
    pub runs: Vec<(i64, i64)>,
    /// 那天的轮次数与其中已判出结果的轮次数。
    pub rounds: usize,
    pub rounds_resolved: usize,
}

#[derive(Default, Clone)]
pub struct PmBinanceReadout {
    pub present: bool,
    pub stamp: String,
    pub symbol: String,
    pub round: Option<RoundView>,
    pub book: Option<BookView>,
    pub rec: RecView,
    pub refreshed: String,
    /// 最近几轮的结果（从已落盘的轮次表读，重启后仍在）。
    pub recent: Vec<RecentRound>,
    /// 按日落盘明细（「数据录制」页用）。扫盘得来，比快照慢得多，故单独节流刷新。
    pub days: Vec<PmDay>,
    /// 这份按日明细是什么时候扫的。**扫盘要几百毫秒，不可能每秒一次**，
    /// 所以要让人看见它可能是旧的。
    pub days_scanned: String,
}

static READOUT: OnceLock<Mutex<PmBinanceReadout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn board_path() -> PathBuf {
    std::env::var("WS_PM_BINANCE_BOARD").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("ws-data/live/pm_binance.json")
    })
}

pub fn snapshot() -> PmBinanceReadout {
    ensure_poller();
    READOUT.get().and_then(|m| m.lock().ok().map(|g| g.clone())).unwrap_or_default()
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            // 按日明细要读每个段的 ts 列，成本是快照的几百倍。每 SCAN_EVERY 拍扫一次，
            // 中间沿用上一次的结果——面板上标了扫描时刻，不会让人当成实时值。
            const SCAN_EVERY: u32 = 30;
            let mut tick = 0u32;
            let mut days: Vec<PmDay> = Vec::new();
            let mut scanned = String::new();
            loop {
                let mut st = load(&board_path());
                if tick % SCAN_EVERY == 0 {
                    days = scan_days(&data_root(), &st.symbol);
                    scanned = chrono::Local::now().format("%H:%M:%S").to_string();
                }
                tick = tick.wrapping_add(1);
                st.days = days.clone();
                st.days_scanned = scanned.clone();
                let lock = READOUT.get_or_init(|| Mutex::new(PmBinanceReadout::default()));
                if let Ok(mut g) = lock.lock() {
                    *g = st;
                }
                // 1 秒：录制器也是每秒写一次，再快只是空读。
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    });
}

/// 纯解析（可单测）：JSON → 读数。文件缺失/坏掉一律返回 `present=false`，
/// **不返回半份**——半份会让面板显示一个看起来正常的空市场。
pub fn load(path: &std::path::Path) -> PmBinanceReadout {
    let mut out = PmBinanceReadout {
        refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
        ..Default::default()
    };
    let Ok(txt) = std::fs::read_to_string(path) else { return out };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return out };
    out.present = true;
    out.stamp = v["stamp"].as_str().unwrap_or_default().to_string();
    out.symbol = v["symbol"].as_str().unwrap_or_default().to_string();

    let r = &v["recording"];
    out.rec = RecView {
        book_rows: r["book_rows"].as_i64().unwrap_or(0),
        rounds: r["rounds"].as_i64().unwrap_or(0),
        ws_msgs: r["ws_msgs"].as_i64().unwrap_or(0),
        resubs: r["resubs"].as_i64().unwrap_or(0),
        up_stale_ms: r["up_stale_ms"].as_i64().unwrap_or(-1),
        down_stale_ms: r["down_stale_ms"].as_i64().unwrap_or(-1),
        last_error: r["last_error"].as_str().unwrap_or_default().to_string(),
    };

    if let Some(rd) = v.get("round").filter(|x| !x.is_null()) {
        out.round = Some(RoundView {
            market_id: rd["market_id"].as_str().unwrap_or_default().to_string(),
            question: rd["question"].as_str().unwrap_or_default().to_string(),
            secs_left: rd["secs_left"].as_i64().unwrap_or(-1),
            start_price: rd["start_price"].as_f64().unwrap_or(0.0),
            fee_bps: rd["fee_bps"].as_f64().unwrap_or(0.0),
            participants: rd["participants"].as_i64().unwrap_or(0),
            volume: rd["volume"].as_f64().unwrap_or(0.0),
            liquidity: rd["liquidity"].as_f64().unwrap_or(0.0),
        });
    }
    if let Some(b) = v.get("book").filter(|x| !x.is_null()) {
        let lv = |k: &str| {
            let a = b[k].as_array();
            a.map(|x| {
                (
                    x.first().and_then(|y| y.as_i64()).unwrap_or(0),
                    x.get(1).and_then(|y| y.as_i64()).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0))
        };
        let lad = |k: &str| {
            let o = &b[k];
            let rows = |kk: &str| {
                o[kk]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| {
                                let r = r.as_array()?;
                                Some((r.first()?.as_f64()?, r.get(1)?.as_f64()?))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Ladder {
                bids: rows("bids"),
                asks: rows("asks"),
                n_bids: o["n_bids"].as_u64().unwrap_or(0) as usize,
                n_asks: o["n_asks"].as_u64().unwrap_or(0) as usize,
            }
        };
        out.book = Some(BookView {
            imbalance: b["imbalance"].as_f64(),
            imbalance_shares: b["imbalance_shares"].as_f64(),
            up_bid: b["up_bid"].as_f64(),
            up_ask: b["up_ask"].as_f64(),
            down_bid: b["down_bid"].as_f64(),
            down_ask: b["down_ask"].as_f64(),
            up_notional: b["up_notional"].as_f64().unwrap_or(0.0),
            down_notional: b["down_notional"].as_f64().unwrap_or(0.0),
            up_levels: lv("up_levels"),
            down_levels: lv("down_levels"),
            up: lad("up"),
            down: lad("down"),
        });
    }
    if let Some(rs) = v.get("recent").and_then(|x| x.as_array()) {
        out.recent = rs
            .iter()
            .map(|r| RecentRound {
                market_id: r["market_id"].as_str().unwrap_or_default().to_string(),
                start_ms: r["start_ms"].as_i64().unwrap_or(0),
                resolved: r["resolved"].as_str().unwrap_or_default().to_string(),
                start_price: r["start_price"].as_f64().unwrap_or(0.0),
                participants: r["participants"].as_i64().unwrap_or(0),
                volume: r["volume"].as_f64().unwrap_or(0.0),
            })
            .collect();
    }
    out
}

fn data_root() -> PathBuf {
    std::env::var("WS_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("ws-data")
    })
}

/// 扫 `raw/pm_book/<symbol>/<date>/*.parquet`，按天算覆盖；顺带数那天的轮次。
///
/// 覆盖读的是每段的 `ts_recv` 列，不是页脚 min/max——「数据录制」页那边实测过，
/// 页脚法把最长无洞段高估 1.27~4.61 倍，而段内的洞是真实存在的（守护还在跑、
/// 流却停了）。同样的坑这边一样会踩，所以从一开始就读列。
///
/// `.inprogress` **不计入**：它是正在写的检查点，随时会变长，
/// 算进覆盖里会让同一天的数字每扫一次都不一样。
pub fn scan_days(root: &std::path::Path, symbol: &str) -> Vec<PmDay> {
    use super::recorder_readout::{coverage_from_spans, parquet_ts_runs};

    let sym = if symbol.is_empty() { "BTCUSDT-5M" } else { symbol };
    let book = root.join("raw").join("pm_book").join(sym);
    let mut out: Vec<PmDay> = Vec::new();
    let Ok(days) = std::fs::read_dir(&book) else { return out };
    for d in days.flatten() {
        let date = d.file_name().to_string_lossy().to_string();
        let Ok(files) = std::fs::read_dir(d.path()) else { continue };
        let mut spans: Vec<(i64, i64)> = Vec::new();
        let (mut segs, mut bytes) = (0usize, 0u64);
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !name.ends_with(".parquet") {
                continue;
            }
            bytes += f.metadata().map(|m| m.len()).unwrap_or(0);
            segs += 1;
            if let Some(v) = parquet_ts_runs(&f.path(), PM_GAP_NS) {
                spans.extend(v);
            }
        }
        if segs == 0 {
            continue;
        }
        let c = coverage_from_spans(spans, PM_GAP_NS);
        let (rounds, resolved) = count_rounds(root, sym, &date);
        out.push(PmDay {
            date,
            segs,
            bytes,
            covered_s: c.covered_s,
            longest_s: c.longest_s,
            longest_at: c.longest_at,
            gaps: c.gaps.len(),
            runs: c.runs,
            rounds,
            rounds_resolved: resolved,
        });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// 那天的轮次总数与已判出结果的数量。
///
/// 读的是 `resolved` 列的非空计数。**空不是平局，是没判出来**——把它算成一个结果
/// 会让「已判」虚高，而那个数正是判断样本够不够的依据。
fn count_rounds(root: &std::path::Path, symbol: &str, date: &str) -> (usize, usize) {
    use parquet::file::reader::{FileReader, SerializedFileReader};

    let p = root
        .join("raw")
        .join("pm_round")
        .join(symbol)
        .join(format!("{date}.parquet"));
    let Ok(f) = std::fs::File::open(&p) else { return (0, 0) };
    let Ok(r) = SerializedFileReader::new(f) else { return (0, 0) };
    let Ok(it) = r.get_row_iter(None) else { return (0, 0) };
    let (mut total, mut res) = (0usize, 0usize);
    for row in it.flatten() {
        total += 1;
        // 按**字段名**找，不按列序号：schema 的列序和投影后的字段序在嵌套 schema 下
        // 不是一回事，而加一列就会让写死的序号悄悄指向别的列。
        if row
            .get_column_iter()
            .any(|(n, v)| n == "resolved" && !v.to_string().trim_matches('"').is_empty())
        {
            res += 1;
        }
    }
    (total, res)
}

/// 录制是否还活着。判据是**两条流都没有陈旧太久**——只看其中一条会漏掉
/// 「WS 在推但 REST 挂了」这种半死状态，而那时失衡度算出来仍然像模像样。
pub fn recording_alive(st: &PmBinanceReadout) -> bool {
    st.present
        && st.rec.up_stale_ms >= 0
        && st.rec.up_stale_ms < 15_000
        && st.rec.down_stale_ms >= 0
        && st.rec.down_stale_ms < 15_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("pm_binance.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn 文件缺失时不present() {
        let st = load(std::path::Path::new("/nonexistent/pm_binance.json"));
        assert!(!st.present);
        assert!(st.round.is_none() && st.book.is_none());
    }

    #[test]
    fn 坏json不present而不是半份() {
        let d = std::env::temp_dir().join(format!("pmb-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = write(&d, "{不是合法 json");
        assert!(!load(&p).present, "坏文件应判不存在，而不是给一份空壳让面板看着正常");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 解析完整快照() {
        let d = std::env::temp_dir().join(format!("pmb2-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = write(
            &d,
            r#"{"stamp":"2026-09-18T12:47:15Z","symbol":"BTCUSDT-5M",
                "recording":{"book_rows":144,"rounds":1,"ws_msgs":132,"resubs":1,
                             "up_stale_ms":716,"down_stale_ms":1167,"last_error":""},
                "round":{"market_id":"11599392","question":"Bitcoin Up or Down",
                         "secs_left":164,"start_price":78120.005,"fee_bps":200.0,
                         "participants":361,"volume":731.4,"liquidity":17511.97},
                "book":{"imbalance":-0.3718,"imbalance_shares":-0.3018,
                        "up_bid":0.46,"up_ask":0.47,"down_bid":0.53,"down_ask":0.54,
                        "up_notional":566.15,"down_notional":1236.2,
                        "up_levels":[46,53],"down_levels":[53,46]}}"#,
        );
        let st = load(&p);
        assert!(st.present);
        let r = st.round.as_ref().unwrap();
        assert_eq!(r.market_id, "11599392");
        assert_eq!(r.secs_left, 164);
        assert_eq!(r.fee_bps, 200.0);
        let b = st.book.as_ref().unwrap();
        assert!((b.imbalance.unwrap() + 0.3718).abs() < 1e-6);
        assert_eq!(b.up_levels, (46, 53));
        assert!(recording_alive(&st), "两条流都新鲜时应判在录");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **半死状态必须判成没在录。**
    ///
    /// WS 还在推、REST 挂了的话，`latest()` 仍会返回带旧 Down 簿的 `Pair`，
    /// 失衡度算出来像模像样——只看一条流的话，这种状态会被当成正常。
    #[test]
    fn 一条流陈旧就算没在录() {
        let mut st = PmBinanceReadout { present: true, ..Default::default() };
        st.rec = RecView { up_stale_ms: 500, down_stale_ms: 60_000, ..Default::default() };
        assert!(!recording_alive(&st), "Down 陈旧 60s 应判没在录");
        st.rec.down_stale_ms = 900;
        assert!(recording_alive(&st));
        st.rec.up_stale_ms = -1; // 从未收到
        assert!(!recording_alive(&st));
    }
}

#[cfg(test)]
mod live_scan {
    //! 对着真实数据目录跑一次扫描（`--ignored` 才跑）。
    //! 单测用不了真 parquet，而扫盘的坑（列名、空值、.inprogress）只有真数据能暴露。
    use super::*;

    #[test]
    #[ignore = "需要本机 ~/ws-data 有预测市场录制"]
    fn 扫真实目录() {
        let days = scan_days(&data_root(), "BTCUSDT-5M");
        assert!(!days.is_empty(), "没扫到任何一天——录制器跑过吗？");
        for d in &days {
            println!(
                "{}  段 {}  {:.1}MB  覆盖 {}s  最长 {}s  洞 {}  轮次 {}/{} 已判",
                d.date,
                d.segs,
                d.bytes as f64 / 1e6,
                d.covered_s,
                d.longest_s,
                d.gaps,
                d.rounds_resolved,
                d.rounds
            );
            assert!(d.covered_s >= d.longest_s, "最长段不可能超过总覆盖");
            assert!(d.rounds_resolved <= d.rounds, "已判轮次不可能多于总轮次");
        }
    }
}
