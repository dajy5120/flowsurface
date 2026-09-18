//! 预测市场回放 — 只读数据。
//!
//! 读两份由主仓 `factory/replay/pm_replay.py` 生成的 JSON（按 mtime 缓存）：
//!   - catalog：`<data>/cockpit/pm_replay_catalog.json` —— 符号 / 日期 / 轮次清单
//!   - panel：`<data>/cockpit/pm_replay_panel.json` —— 当前那一轮的图表
//!
//! **零交易所连接**：数据全部来自 `ws-pm-recorder` 已落盘的 parquet。
//!
//! 图表结构直接复用 [`super::tardis_board_readout::Chart`]——两边产出的是同一份
//! JSON 契约，绘图与播放头裁剪（`clip_to`）也是同一套代码。再定义一份自己的
//! Chart 只会让两边悄悄漂移。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use super::tardis_board_readout::Chart;

fn base() -> PathBuf {
    super::paths::data_dir().join("cockpit")
}

pub fn catalog_path() -> PathBuf {
    std::env::var("WS_PM_REPLAY_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base().join("pm_replay_catalog.json"))
}

pub fn panel_path() -> PathBuf {
    std::env::var("WS_PM_REPLAY_PANEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base().join("pm_replay_panel.json"))
}

/// 一轮。
#[derive(Clone, Default, PartialEq)]
pub struct RoundEntry {
    pub market_id: String,
    /// 只有时段，如 `8:50AM-8:55AM ET`——一天几十上百轮，有区分度的就是它。
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// `"UP"` / `"DOWN"` / 空 = 还没判出来。
    ///
    /// **未结算的轮次回放价值低**：看不到结局，就没法判断当时的失衡指对没指对。
    /// 所以要在清单上标出来，而不是让人点进去才发现。
    pub resolved: String,
}

#[derive(Clone, Default)]
pub struct DateEntry {
    pub date: String,
    pub rounds: Vec<RoundEntry>,
}

#[derive(Clone, Default)]
pub struct SymbolEntry {
    pub symbol: String,
    pub dates: Vec<DateEntry>,
}

#[derive(Clone, Default)]
pub struct PmCatalog {
    pub symbols: Vec<SymbolEntry>,
    pub error: Option<String>,
}

impl PmCatalog {
    pub fn symbol(&self, s: &str) -> Option<&SymbolEntry> {
        self.symbols.iter().find(|x| x.symbol == s)
    }
    pub fn date(&self, s: &str, d: &str) -> Option<&DateEntry> {
        self.symbol(s)?.dates.iter().find(|x| x.date == d)
    }
}

/// 一帧：某一刻的两本簿 + 当时的失衡度。
///
/// **和实时面板显示的是同一组东西**——回放时那一刻的盘口要和当时实时看到的一样，
/// 否则「回放」就名不副实。所以这里直接用 [`super::pm_binance_readout::BookView`]，
/// 渲染也走实时面板那套函数，两边不会悄悄漂移。
#[derive(Clone, Default)]
pub struct Frame {
    pub ts: f64,
    pub book: super::pm_binance_readout::BookView,
}

/// 轮次元信息（与实时面板显示的同一组字段）。
#[derive(Clone, Default)]
pub struct RoundMeta {
    pub start_price: f64,
    pub fee_bps: f64,
    pub participants: i64,
    pub volume: f64,
    pub liquidity: f64,
    pub final_up_px: f64,
    pub final_down_px: f64,
}

/// 一轮的回放面板。
#[derive(Clone, Default)]
pub struct PmPanel {
    pub loaded: bool,
    pub symbol: String,
    pub date: String,
    pub market_id: String,
    pub question: String,
    pub resolved: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// 原始簿更新条数与两本对齐后的采样点数。
    ///
    /// 两个都显示是有意的：差值就是「只有一本簿到过」的那些时刻，
    /// 它们被丢掉了——半份簿算出的失衡度会把另一侧当成 0。
    pub rows: u64,
    pub samples: u64,
    pub charts: Vec<Arc<Chart>>,
    /// 逐帧盘口，按 `ts` 升序。用 `Arc` 装：`panel()` 每帧被 view 调用并整份 clone，
    /// 一轮上千帧、每帧两本簿各十几档，整份拷贝会在回放时逐帧发生。
    pub frames: Arc<Vec<Frame>>,
    pub round: RoundMeta,
    pub error: Option<String>,
    pub generation: u64,
}

impl PmPanel {
    /// 播放头所在那一帧。取**不晚于**播放头的最后一帧——盘口是状态，
    /// 取后面那帧等于让人看到还没发生的事。
    pub fn frame_at(&self, head_ms: f64) -> Option<&Frame> {
        let i = self.frames.partition_point(|f| f.ts <= head_ms);
        if i == 0 { None } else { self.frames.get(i - 1) }
    }
}

static CATALOG: OnceLock<Mutex<(Option<SystemTime>, PmCatalog)>> = OnceLock::new();
static PANEL: OnceLock<Mutex<(Option<SystemTime>, PmPanel)>> = OnceLock::new();
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 作废两份缓存，下次读时重新解析。生成/刷新之后调用。
pub fn invalidate() {
    if let Some(m) = CATALOG.get()
        && let Ok(mut g) = m.lock()
    {
        g.0 = None;
    }
    if let Some(m) = PANEL.get()
        && let Ok(mut g) = m.lock()
    {
        g.0 = None;
    }
}

pub fn catalog() -> PmCatalog {
    let lock = CATALOG.get_or_init(|| Mutex::new((None, PmCatalog::default())));
    let Ok(mut g) = lock.lock() else { return PmCatalog::default() };
    let p = catalog_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let c = match std::fs::read_to_string(&p) {
        Ok(t) => parse_catalog(&t),
        Err(e) => PmCatalog {
            error: Some(format!("读不到轮次清单 {}：{e}（点「刷新清单」生成）", p.display())),
            ..Default::default()
        },
    };
    *g = (mt, c.clone());
    c
}

pub fn parse_catalog(text: &str) -> PmCatalog {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return PmCatalog { error: Some("轮次清单 JSON 解析失败".into()), ..Default::default() };
    };
    let mut out = PmCatalog::default();
    for s in v.get("symbols").and_then(|x| x.as_array()).into_iter().flatten() {
        let mut se = SymbolEntry {
            symbol: s["symbol"].as_str().unwrap_or_default().to_string(),
            dates: vec![],
        };
        for d in s.get("dates").and_then(|x| x.as_array()).into_iter().flatten() {
            let mut de =
                DateEntry { date: d["date"].as_str().unwrap_or_default().to_string(), rounds: vec![] };
            for r in d.get("rounds").and_then(|x| x.as_array()).into_iter().flatten() {
                de.rounds.push(RoundEntry {
                    market_id: r["market_id"].as_str().unwrap_or_default().to_string(),
                    label: r["label"].as_str().unwrap_or_default().to_string(),
                    start_ms: r["start_ms"].as_i64().unwrap_or(0),
                    end_ms: r["end_ms"].as_i64().unwrap_or(0),
                    resolved: r["resolved"].as_str().unwrap_or_default().to_string(),
                });
            }
            se.dates.push(de);
        }
        out.symbols.push(se);
    }
    out
}

pub fn panel() -> PmPanel {
    let lock = PANEL.get_or_init(|| Mutex::new((None, PmPanel::default())));
    let Ok(mut g) = lock.lock() else { return PmPanel::default() };
    let p = panel_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let mut pl = match std::fs::read_to_string(&p) {
        Ok(t) => parse_panel(&t),
        Err(_) => PmPanel::default(),
    };
    pl.generation = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    *g = (mt, pl.clone());
    pl
}

pub fn parse_panel(text: &str) -> PmPanel {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return PmPanel { error: Some("面板 JSON 解析失败".into()), ..Default::default() };
    };
    if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
        return PmPanel { error: Some(e.to_string()), ..Default::default() };
    }
    let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let mut out = PmPanel {
        loaded: v["loaded"].as_bool().unwrap_or(false),
        symbol: s("symbol"),
        date: s("date"),
        market_id: s("market_id"),
        question: s("question"),
        resolved: s("resolved"),
        start_ms: f("start_ms"),
        end_ms: f("end_ms"),
        rows: v["rows"].as_u64().unwrap_or(0),
        samples: v["samples"].as_u64().unwrap_or(0),
        ..Default::default()
    };
    if let Some(r) = v.get("round") {
        let g = |k: &str| r[k].as_f64().unwrap_or(0.0);
        out.round = RoundMeta {
            start_price: g("start_price"),
            fee_bps: g("fee_bps"),
            participants: r["participants"].as_i64().unwrap_or(0),
            volume: g("volume"),
            liquidity: g("liquidity"),
            final_up_px: g("final_up_px"),
            final_down_px: g("final_down_px"),
        };
    }
    let mut frames: Vec<Frame> = Vec::new();
    for f in v.get("frames").and_then(|x| x.as_array()).into_iter().flatten() {
        let lad = |k: &str| -> super::pm_binance_readout::Ladder {
            let rows = |kk: &str| -> Vec<(f64, f64)> {
                f[k][kk]
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
            let (bids, asks) = (rows("bids"), rows("asks"));
            super::pm_binance_readout::Ladder {
                n_bids: bids.len(),
                n_asks: asks.len(),
                bids,
                asks,
            }
        };
        let up = lad("up");
        let down = lad("down");
        frames.push(Frame {
            ts: f["ts"].as_f64().unwrap_or(0.0),
            book: super::pm_binance_readout::BookView {
                imbalance: f["imb_money"].as_f64(),
                imbalance_shares: f["imb_shares"].as_f64(),
                up_bid: up.bids.first().map(|x| x.0),
                up_ask: up.asks.first().map(|x| x.0),
                down_bid: down.bids.first().map(|x| x.0),
                down_ask: down.asks.first().map(|x| x.0),
                up_notional: f["up_notional"].as_f64().unwrap_or(0.0),
                down_notional: f["dn_notional"].as_f64().unwrap_or(0.0),
                up_levels: (up.bids.len() as i64, up.asks.len() as i64),
                down_levels: (down.bids.len() as i64, down.asks.len() as i64),
                up,
                down,
            },
        });
    }
    out.frames = Arc::new(frames);
    for c in v.get("charts").and_then(|x| x.as_array()).into_iter().flatten() {
        let arr = |k: &str| -> Vec<f64> {
            c.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().map(|t| t.as_f64().unwrap_or(f64::NAN)).collect())
                .unwrap_or_default()
        };
        let mut ch = Chart {
            id: c["id"].as_str().unwrap_or_default().to_string(),
            title: c["title"].as_str().unwrap_or_default().to_string(),
            kind: c["kind"].as_str().unwrap_or("line").to_string(),
            note: c["note"].as_str().unwrap_or_default().to_string(),
            y_label: c["y_label"].as_str().unwrap_or_default().to_string(),
            x_is_time: c["x_is_time"].as_bool().unwrap_or(true),
            x: arr("x"),
            ..Default::default()
        };
        for s in c.get("series").and_then(|x| x.as_array()).into_iter().flatten() {
            ch.series.push((
                s["name"].as_str().unwrap_or_default().to_string(),
                s.get("v")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().map(|t| t.as_f64().unwrap_or(f64::NAN)).collect())
                    .unwrap_or_default(),
            ));
        }
        out.charts.push(Arc::new(ch));
    }
    out
}

/// 面板的时间跨度（ms）。回放的播放头就在这个区间里推进。
pub fn time_span(p: &PmPanel) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for c in &p.charts {
        if !c.x_is_time {
            continue;
        }
        if let (Some(a), Some(b)) = (c.x.first(), c.x.last()) {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
    }
    (lo.is_finite() && hi > lo).then_some((lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 坏json不当成已加载() {
        let p = parse_panel("{不是 json");
        assert!(!p.loaded && p.error.is_some());
        // 关键：不能返回一个 loaded=true 的空壳，那会让面板显示一张空图而不是报错。
        assert!(p.charts.is_empty());
    }

    #[test]
    fn python侧的error字段要透出来() {
        let p = parse_panel(r#"{"error":"轮次 123 在 2026-09-18 的簿数据里没有记录"}"#);
        assert!(!p.loaded);
        assert!(p.error.as_deref().unwrap().contains("没有记录"));
    }

    #[test]
    fn 解析清单与轮次结算态() {
        let c = parse_catalog(
            r#"{"symbols":[{"symbol":"BTCUSDT-5M","dates":[{"date":"2026-09-18","rounds":[
                {"market_id":"1","label":"9AM-9:05AM ET","start_ms":1,"end_ms":2,"resolved":"DOWN"},
                {"market_id":"2","label":"9:05AM-9:10AM ET","start_ms":2,"end_ms":3,"resolved":""}
            ]}]}]}"#,
        );
        let d = c.date("BTCUSDT-5M", "2026-09-18").unwrap();
        assert_eq!(d.rounds.len(), 2);
        assert_eq!(d.rounds[0].resolved, "DOWN");
        // 空串是「没判出来」，不是平局——面板要把它和 UP/DOWN 区别显示。
        assert!(d.rounds[1].resolved.is_empty());
        assert!(c.date("BTCUSDT-5M", "2099-01-01").is_none());
    }

    #[test]
    fn 时间跨度取全部时间轴图的并集() {
        let p = parse_panel(
            r#"{"loaded":true,"charts":[
                {"id":"a","kind":"line","x_is_time":true,"x":[100,200],"series":[]},
                {"id":"b","kind":"line","x_is_time":true,"x":[150,300],"series":[]}
            ]}"#,
        );
        assert_eq!(time_span(&p), Some((100.0, 300.0)));
    }

    #[test]
    fn 无时间轴时没有跨度可播() {
        let p = parse_panel(r#"{"loaded":true,"charts":[{"id":"a","kind":"line","x_is_time":false,"x":[1,2],"series":[]}]}"#);
        assert_eq!(time_span(&p), None, "没有时间轴就不该给出播放区间");
    }
}

#[cfg(test)]
mod live_files {
    //! 对着真实生成的 JSON 跑一遍（`--ignored` 才跑）。
    //! 单测里手写的 JSON 只能证明解析器不崩，证明不了它和 Python 那边对得上。
    use super::*;

    #[test]
    #[ignore = "需要先跑 factory.replay.pm_replay 生成 JSON"]
    fn 解析真实产物() {
        let c = parse_catalog(&std::fs::read_to_string(catalog_path()).expect("先生成清单"));
        assert!(c.error.is_none() && !c.symbols.is_empty());
        let d = &c.symbols[0].dates[0];
        println!("{} {} — {} 轮", c.symbols[0].symbol, d.date, d.rounds.len());
        assert!(d.rounds.iter().any(|r| !r.resolved.is_empty()), "该有已结算的轮次");

        let p = parse_panel(&std::fs::read_to_string(panel_path()).expect("先生成面板"));
        assert!(p.loaded && p.error.is_none(), "{:?}", p.error);
        let (t0, t1) = time_span(&p).expect("回放要有时间轴");
        println!(
            "{} {} 结算={} {} 图 跨度 {:.0}s 采样 {}/{}",
            p.date, p.market_id, p.resolved, p.charts.len(), (t1 - t0) / 1000.0, p.samples, p.rows
        );
        // 一轮是 5 分钟；跨度明显超出说明轮次过滤没生效（混进了别的市场）。
        assert!((t1 - t0) <= 400_000.0, "一轮不该超过 ~5 分钟，跨度 {:.0}s", (t1 - t0) / 1000.0);
        for ch in &p.charts {
            for (name, v) in &ch.series {
                assert_eq!(v.len(), ch.x.len(), "{} 的序列 {name} 与 x 轴长度不一致", ch.id);
            }
        }
        // 逐帧盘口：回放时显示「那一刻的两本簿」。
        assert!(!p.frames.is_empty(), "真实产物该带逐帧盘口");
        println!("帧 {} 个，轮次 {} 人 / 费率 {:.2}%", p.frames.len(), p.round.participants, p.round.fee_bps / 100.0);
        let mid = &p.frames[p.frames.len() / 2];
        assert!(!mid.book.up.bids.is_empty() && !mid.book.down.bids.is_empty(),
                "轮次中段两本簿都该有买盘");
        // 帧必须严格递增，否则 frame_at 的二分会取错。
        for w in p.frames.windows(2) {
            assert!(w[1].ts > w[0].ts, "帧时间轴必须严格递增");
        }
        let (t0m, t1m) = (p.frames[0].ts, p.frames[p.frames.len() - 1].ts);
        assert!(t0m >= t0 - 1.0 && t1m <= t1 + 1.0, "帧不该超出图的时间范围");
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    fn panel_with(ts: &[f64]) -> PmPanel {
        let frames: Vec<String> = ts
            .iter()
            .map(|t| {
                format!(
                    r#"{{"ts":{t},"imb_money":0.1,"imb_shares":-0.2,
                        "up_notional":10,"dn_notional":5,
                        "up":{{"bids":[[0.6,10]],"asks":[[0.61,5]]}},
                        "down":{{"bids":[[0.39,7]],"asks":[[0.40,3]]}}}}"#
                )
            })
            .collect();
        parse_panel(&format!(
            r#"{{"loaded":true,"start_ms":0,"end_ms":1000,"charts":[],"frames":[{}]}}"#,
            frames.join(",")
        ))
    }

    #[test]
    fn 取不晚于播放头的最后一帧() {
        let p = panel_with(&[0.0, 250.0, 500.0, 750.0]);
        assert_eq!(p.frames.len(), 4);
        assert_eq!(p.frame_at(600.0).unwrap().ts, 500.0, "盘口是状态，不能显示还没发生的那帧");
        assert_eq!(p.frame_at(500.0).unwrap().ts, 500.0, "正好落在帧上取该帧");
        assert_eq!(p.frame_at(1e9).unwrap().ts, 750.0);
    }

    #[test]
    fn 播放头在第一帧之前没有盘口可显示() {
        let p = panel_with(&[100.0, 200.0]);
        assert!(p.frame_at(50.0).is_none(), "两本簿还没凑齐时不该拿后面的帧充数");
    }

    #[test]
    fn 帧解析出两本独立的簿与两个口径() {
        let p = panel_with(&[0.0]);
        let f = &p.frames[0];
        assert_eq!(f.book.up.bids, vec![(0.6, 10.0)]);
        assert_eq!(f.book.down.bids, vec![(0.39, 7.0)]);
        assert_eq!(f.book.up_bid, Some(0.6));
        assert_eq!(f.book.down_bid, Some(0.39));
        // 两个口径都要在，且这里刻意给了反号的值——面板要能显示出这件事。
        assert_eq!(f.book.imbalance, Some(0.1));
        assert_eq!(f.book.imbalance_shares, Some(-0.2));
    }

    #[test]
    fn 旧版面板没有frames时不崩() {
        let p = parse_panel(r#"{"loaded":true,"charts":[]}"#);
        assert!(p.frames.is_empty());
        assert!(p.frame_at(100.0).is_none());
    }
}
