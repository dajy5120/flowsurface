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

#[derive(Default, Clone)]
pub struct PmBinanceReadout {
    pub present: bool,
    pub stamp: String,
    pub symbol: String,
    pub round: Option<RoundView>,
    pub book: Option<BookView>,
    pub rec: RecView,
    pub refreshed: String,
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
            loop {
                let st = load(&board_path());
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
        });
    }
    out
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
