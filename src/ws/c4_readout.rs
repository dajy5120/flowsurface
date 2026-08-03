//! C4 活体影子只读快照（docs/14 §2「live 指标接 P2 面板」）。
//!
//! **独立新增面板**的数据层——不改任何既有面板/readout。数据源（全只读）：
//! ① 守护 checkpoint `~/ws-data/live/maker_shadow-SOLUSDT.json`（maker_shadow --daemon
//!    每 5min + SIGTERM 原子写）→ 今日实时行（fills/库存/日净值/在线/FIFO 回合胜率）；
//! ② Registry `live_metrics`：kind=maker_shadow_day（UTC 日切落账）+ live_vs_replay
//!    （同日活体 vs 重放对照，preregister-c4 证伪旗）。
//!
//! 沿用 F6 旁路模式（同 `factory_readout`）：惰性起 10s poller，pane 只读快照。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// preregister-c4-live 合格日口径：非残段 且 在线 ≥80%（86400×0.8=69120s）。
pub const QUALIFY_UPTIME_SECS: f64 = 69120.0;
/// C4 判定所需合格影子日数（preregister-c4-live）。
pub const QUALIFY_TARGET: usize = 7;

/// 今日实时（checkpoint）。
#[derive(Default, Clone)]
pub struct TodayRow {
    pub utc_day: String,
    pub n_fills: usize,
    pub inv: f64,
    pub day_pnl: f64, // 费后（按 checkpoint params.maker_fee_bp 换算）
    pub win_rate: Option<f64>,
    pub uptime_h: f64,
    pub reconnects: i64,
    pub age_secs: i64, // checkpoint 距今；>400s ≈ 守护可能没在跑（5min 周期+余量）
}

/// 已落账影子日（Registry maker_shadow_day，新→旧）。
#[derive(Default, Clone)]
pub struct DayRow {
    pub day: String,
    pub win_rate: Option<f64>,
    pub pnl: f64,
    pub bp: Option<f64>,
    pub uptime_secs: f64,
    pub reconnects: i64,
    pub partial: bool,
}

/// 活体 vs 重放对照行（Registry live_vs_replay，新→旧）。
#[derive(Default, Clone)]
pub struct VsRow {
    pub day: String,
    pub live_bp: Option<f64>,
    pub replay_bp: f64,
    pub falsify: bool,
}

#[derive(Default, Clone)]
pub struct C4Readout {
    pub today: Option<TodayRow>,
    pub days: Vec<DayRow>,
    pub vs: Vec<VsRow>,
    pub refreshed: String,
}

impl C4Readout {
    /// 合格影子日数（非残段 · 在线≥80%）。
    pub fn qualified(&self) -> usize {
        self.days
            .iter()
            .filter(|d| !d.partial && d.uptime_secs >= QUALIFY_UPTIME_SECS)
            .count()
    }
    pub fn any_falsify(&self) -> bool {
        self.vs.iter().any(|v| v.falsify)
    }
}

static READOUT: OnceLock<Mutex<C4Readout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn db_path() -> PathBuf {
    std::env::var("WS_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/registry.sqlite"))
}
fn ckpt_path() -> PathBuf {
    std::env::var("WS_C4_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/maker_shadow-SOLUSDT.json"))
}

/// pane 渲染时读快照（惰性起 poller：首次调用才开后台线程）。
pub fn snapshot() -> C4Readout {
    ensure_poller();
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let snap = poll_once();
            let lock = READOUT.get_or_init(|| Mutex::new(C4Readout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            std::thread::sleep(Duration::from_secs(10));
        });
    });
}

fn poll_once() -> C4Readout {
    let mut st = C4Readout {
        refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
        ..Default::default()
    };
    st.today = read_ckpt();
    read_registry(&mut st);
    st
}

/// FIFO 配对回合胜率（与 factory.live.maker_shadow.round_stats 同口径）。
/// fills: (side 1买/2卖, price, qty)；fee = 单腿费率。
fn fifo_win_rate(fills: &[(u8, f64, f64)], fee: f64) -> Option<f64> {
    let mut buys: Vec<(f64, f64)> =
        fills.iter().filter(|f| f.0 == 1).map(|f| (f.1, f.2)).collect();
    let mut sells: Vec<(f64, f64)> =
        fills.iter().filter(|f| f.0 == 2).map(|f| (f.1, f.2)).collect();
    let (mut bi, mut si, mut wins, mut total) = (0usize, 0usize, 0u32, 0u32);
    while bi < buys.len() && si < sells.len() {
        let m = buys[bi].1.min(sells[si].1);
        let pnl = (sells[si].0 - buys[bi].0) * m - (buys[bi].0 + sells[si].0) * m * fee;
        total += 1;
        if pnl > 0.0 {
            wins += 1;
        }
        buys[bi].1 -= m;
        sells[si].1 -= m;
        if buys[bi].1 <= 1e-12 {
            bi += 1;
        }
        if sells[si].1 <= 1e-12 {
            si += 1;
        }
    }
    (total > 0).then(|| f64::from(wins) / f64::from(total))
}

fn read_ckpt() -> Option<TodayRow> {
    let txt = std::fs::read_to_string(ckpt_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some(parse_ckpt(&v, chrono::Utc::now().timestamp() as f64))
}

/// 纯解析（可单测）：checkpoint JSON → 今日行。
fn parse_ckpt(v: &serde_json::Value, now: f64) -> TodayRow {
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let fee_bp = v
        .pointer("/params/maker_fee_bp")
        .and_then(|x| x.as_f64())
        .unwrap_or(2.0);
    let fee = fee_bp * 1e-4;
    // fees 字段按 QueueSim 内置 2bp 记账 → 换算到 CLI 费率（同 maker_shadow 收尾口径）
    let fees_cli = f("fees") / 2e-4 * fee;
    let fills: Vec<(u8, f64, f64)> = v
        .get("day_fills")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f4| {
                    let a = f4.as_array()?;
                    Some((
                        a.get(1)?.as_u64()? as u8,
                        a.get(2)?.as_f64()?,
                        a.get(3)?.as_f64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    TodayRow {
        utc_day: v.get("utc_day").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
        n_fills: fills.len(),
        inv: f("inv"),
        day_pnl: (f("cash") + f("inv") * f("mid") - fees_cli) - f("day_open_equity"),
        win_rate: fifo_win_rate(&fills, fee),
        uptime_h: f("day_uptime") / 3600.0,
        // 必须减当日基线：`n_reconnects` 是**进程累计**（守护可连跑多日），
        // 而本行其余字段都是当日口径（day_pnl 减 day_open_equity、uptime 用 day_uptime、
        // 成交用 day_fills）。漏减会让「今日重连」显示成开机以来的总数——
        // 实测 2045 累计 vs 今日实际 6 次，差 340 倍，被误读成网络故障（docs/20 §24）。
        reconnects: (f("n_reconnects") - f("base_reconnects")).max(0.0) as i64,
        age_secs: (now - f("ts")) as i64,
    }
}

fn read_registry(st: &mut C4Readout) {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return;
    };
    // 表可能尚未建（守护首日未落账）——prepare 失败即静默空表。
    if let Ok(mut stmt) = conn.prepare(
        "SELECT json_extract(metrics_json,'$.utc_day'),
                json_extract(metrics_json,'$.win_rate'),
                json_extract(metrics_json,'$.pnl_usdt'),
                json_extract(metrics_json,'$.bp_per_round'),
                json_extract(metrics_json,'$.uptime_secs'),
                json_extract(metrics_json,'$.n_reconnects'),
                json_extract(metrics_json,'$.partial')
         FROM live_metrics WHERE kind='maker_shadow_day' ORDER BY id DESC LIMIT 9",
    ) {
        st.days = stmt
            .query_map([], |r| {
                Ok(DayRow {
                    day: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    win_rate: r.get::<_, Option<f64>>(1)?,
                    pnl: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    bp: r.get::<_, Option<f64>>(3)?,
                    uptime_secs: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    reconnects: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    partial: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }
    if let Ok(mut stmt) = conn.prepare(
        "SELECT json_extract(metrics_json,'$.utc_day'),
                json_extract(metrics_json,'$.live_bp'),
                json_extract(metrics_json,'$.replay_bp'),
                json_extract(metrics_json,'$.falsify_flag')
         FROM live_metrics WHERE kind='live_vs_replay' ORDER BY id DESC LIMIT 4",
    ) {
        st.vs = stmt
            .query_map([], |r| {
                Ok(VsRow {
                    day: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    live_bp: r.get::<_, Option<f64>>(1)?,
                    replay_bp: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    falsify: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    /// 「今日重连」必须是当日增量，不是进程累计。
    /// 守护连跑多日时 `n_reconnects` 会一直涨，漏减 `base_reconnects` 会把开机以来的
    /// 总数当成今日值——实测 2045 vs 实际 6，差 340 倍，被误读成网络故障（docs/20 §24）。
    #[test]
    fn today_reconnects_subtracts_daily_base() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"utc_day":"2026-08-03","cash":0.0,"inv":0.0,"mid":0.0,
                "day_open_equity":0.0,"day_uptime":35362.0,
                "n_reconnects":2045,"base_reconnects":2039,"fees":0.0,"ts":0.0}"#,
        )
        .unwrap();
        let row = parse_ckpt(&v, 0.0);
        assert_eq!(row.reconnects, 6, "应为当日增量 6，而非累计 2045");
    }

    use super::*;

    #[test]
    fn fifo_pairs_profit_loss_and_split() {
        // 盈利对：毛 0.2 > 双腿费 ~0.04
        let win = [(1u8, 100.0, 1.0), (2u8, 100.2, 1.0)];
        assert_eq!(fifo_win_rate(&win, 2e-4), Some(1.0));
        // 亏损对：毛 0.01 < 费
        let lose = [(1u8, 100.0, 1.0), (2u8, 100.01, 1.0)];
        assert_eq!(fifo_win_rate(&lose, 2e-4), Some(0.0));
        // 量拆分：1 买配 2 半卖 → 一胜一负
        let split = [(1u8, 100.0, 1.0), (2u8, 100.5, 0.5), (2u8, 99.0, 0.5)];
        assert_eq!(fifo_win_rate(&split, 2e-4), Some(0.5));
        // 无配对
        assert_eq!(fifo_win_rate(&[(1u8, 100.0, 1.0)], 2e-4), None);
    }

    #[test]
    fn ckpt_parse_computes_day_pnl_and_age() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"utc_day":"2026-07-04","ts":1000.0,"mid":80.0,
                "cash":-160.0,"inv":2.0,"fees":0.064,
                "params":{"maker_fee_bp":2.0},
                "day_open_equity":-0.5,"day_uptime":7200.0,"n_reconnects":3,
                "day_fills":[[1,1,80.0,1.0],[2,1,80.0,1.0]]}"#,
        )
        .unwrap();
        let t = parse_ckpt(&v, 1300.0);
        assert_eq!(t.utc_day, "2026-07-04");
        assert_eq!(t.n_fills, 2);
        assert_eq!(t.reconnects, 3);
        assert_eq!(t.age_secs, 300);
        assert!((t.uptime_h - 2.0).abs() < 1e-9);
        // pnl = (cash + inv*mid − fees) − open = (-160+160-0.064) − (-0.5) = 0.436
        assert!((t.day_pnl - 0.436).abs() < 1e-9);
    }

    #[test]
    fn qualified_counts_full_days_only() {
        let st = C4Readout {
            days: vec![
                DayRow { uptime_secs: 80000.0, partial: false, ..Default::default() },
                DayRow { uptime_secs: 80000.0, partial: true, ..Default::default() },
                DayRow { uptime_secs: 3600.0, partial: false, ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(st.qualified(), 1);
    }
}
