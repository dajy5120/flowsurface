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
    pub error: Option<String>,
    pub generation: u64,
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
    }
}
