//! Tardis 历史面板 — 只读数据（docs/20 §9）。
//!
//! 读两份由主仓 Python 生成的 JSON（按 mtime 缓存，改动即重载）：
//!   - catalog：三个数据源各自的 符号 / 日期 / 可用类型（`factory/replay/sources.py --catalog`）
//!   - panel：当前 源×类型×窗口 的图表数据（`factory/replay/panels.py`）
//! **不连任何网络、不订阅任何交易所流。**

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

fn base() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dajy".into());
    PathBuf::from(home).join("ws-data/cockpit")
}

pub fn catalog_path() -> PathBuf {
    std::env::var("WS_TARDIS_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base().join("tardis_catalog.json"))
}

pub fn panel_path() -> PathBuf {
    std::env::var("WS_TARDIS_PANEL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base().join("tardis_panel.json"))
}

// ── catalog ────────────────────────────────────────────────────────────────
#[derive(Clone, Default)]
pub struct SourceEntry {
    pub key: String,
    pub label: String,
    pub available: bool,
    pub symbols: Vec<String>,
    pub types: Vec<String>,
    pub dates: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Default)]
pub struct Catalog {
    pub sources: Vec<SourceEntry>,
    pub type_labels: BTreeMap<String, String>,
    pub error: Option<String>,
}

/// 类型的中文名（catalog 缺失时回退原名）。
pub fn type_label(t: &str) -> String {
    catalog().type_labels.get(t).cloned().unwrap_or_else(|| t.to_string())
}

static CATALOG: OnceLock<Mutex<(Option<SystemTime>, Catalog)>> = OnceLock::new();

pub fn invalidate_catalog() {
    if let Some(m) = CATALOG.get()
        && let Ok(mut g) = m.lock()
    {
        g.0 = None;
    }
}

pub fn catalog() -> Catalog {
    let lock = CATALOG.get_or_init(|| Mutex::new((None, Catalog::default())));
    let Ok(mut g) = lock.lock() else {
        return Catalog::default();
    };
    let p = catalog_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let cat = match std::fs::read_to_string(&p) {
        Ok(t) => parse_catalog(&t),
        Err(e) => Catalog {
            error: Some(format!("读不到数据源清单 {}：{e}（点「刷新清单」生成）", p.display())),
            ..Default::default()
        },
    };
    *g = (mt, cat.clone());
    cat
}

fn parse_catalog(text: &str) -> Catalog {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Catalog { error: Some("清单 JSON 解析失败".into()), ..Default::default() };
    };
    let mut out = Catalog::default();
    if let Some(m) = v.get("type_labels").and_then(|x| x.as_object()) {
        for (k, val) in m {
            if let Some(s) = val.as_str() {
                out.type_labels.insert(k.clone(), s.to_string());
            }
        }
    }
    for s in v.get("sources").and_then(|x| x.as_array()).into_iter().flatten() {
        let str_of = |k: &str| s.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let vec_of = |k: &str| -> Vec<String> {
            s.get(k)
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let mut dates = BTreeMap::new();
        if let Some(m) = s.get("dates").and_then(|x| x.as_object()) {
            for (sym, arr) in m {
                dates.insert(
                    sym.clone(),
                    arr.as_array()
                        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                );
            }
        }
        out.sources.push(SourceEntry {
            key: str_of("key"),
            label: str_of("label"),
            available: s.get("available").and_then(|x| x.as_bool()).unwrap_or(false),
            symbols: vec_of("symbols"),
            types: vec_of("types"),
            dates,
        });
    }
    out
}

// ── panel ──────────────────────────────────────────────────────────────────
/// 一张图的数据。不同 kind 用其中不同字段（见 `panels.py` 文档）。
#[derive(Clone, Default)]
pub struct Chart {
    pub id: String,
    pub title: String,
    pub kind: String, // candle | line | bar | scatter | profile
    pub note: String,
    pub y_label: String,
    pub x_is_time: bool,
    pub x: Vec<f64>,
    pub series: Vec<(String, Vec<f64>)>, // line / bar
    pub o: Vec<f64>,                     // candle
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
    pub v: Vec<f64>,
    pub y: Vec<f64>, // scatter
    pub size: Vec<f64>,
    pub cls: Vec<i64>,
    pub bid_price: Vec<f64>, // profile
    pub bid_amount: Vec<f64>,
    pub ask_price: Vec<f64>,
    pub ask_amount: Vec<f64>,
}

#[derive(Clone, Default)]
pub struct Panel {
    pub loaded: bool,
    pub source: String,
    pub source_label: String,
    pub symbol: String,
    pub date: String,
    pub start: String,
    pub minutes: u32,
    pub dtype: String,
    pub type_label: String,
    pub rows: u64,
    pub charts: Vec<Chart>,
    pub error: Option<String>,
}

static PANEL: OnceLock<Mutex<(Option<SystemTime>, Panel)>> = OnceLock::new();

pub fn invalidate() {
    if let Some(m) = PANEL.get()
        && let Ok(mut g) = m.lock()
    {
        g.0 = None;
    }
}

pub fn panel() -> Panel {
    let lock = PANEL.get_or_init(|| Mutex::new((None, Panel::default())));
    let Ok(mut g) = lock.lock() else {
        return Panel::default();
    };
    let p = panel_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let out = match std::fs::read_to_string(&p) {
        Ok(t) => parse_panel(&t),
        Err(_) => Panel::default(), // 尚未加载过：view 显引导语，不报错
    };
    *g = (mt, out.clone());
    out
}

fn nums(v: Option<&serde_json::Value>) -> Vec<f64> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().map(|t| t.as_f64().unwrap_or(f64::NAN)).collect())
        .unwrap_or_default()
}

fn parse_panel(text: &str) -> Panel {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Panel { loaded: true, error: Some("面板 JSON 解析失败".into()), ..Default::default() };
    };
    let s_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut p = Panel {
        loaded: true,
        source: s_of("source"),
        source_label: s_of("source_label"),
        symbol: s_of("symbol"),
        date: s_of("date"),
        start: s_of("start"),
        minutes: v.get("minutes").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32,
        dtype: s_of("type"),
        type_label: s_of("type_label"),
        rows: v.get("rows").and_then(serde_json::Value::as_u64).unwrap_or(0),
        error: v.get("error").and_then(|x| x.as_str()).map(String::from),
        ..Default::default()
    };
    for c in v.get("charts").and_then(|x| x.as_array()).into_iter().flatten() {
        let cs = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let mut ch = Chart {
            id: cs("id"),
            title: cs("title"),
            kind: cs("kind"),
            note: cs("note"),
            y_label: cs("y_label"),
            x_is_time: c.get("x_is_time").and_then(|x| x.as_bool()).unwrap_or(true),
            x: nums(c.get("x")),
            o: nums(c.get("o")),
            h: nums(c.get("h")),
            l: nums(c.get("l")),
            c: nums(c.get("c")),
            v: nums(c.get("v")),
            y: nums(c.get("y")),
            size: nums(c.get("size")),
            bid_price: nums(c.get("bid_price")),
            bid_amount: nums(c.get("bid_amount")),
            ask_price: nums(c.get("ask_price")),
            ask_amount: nums(c.get("ask_amount")),
            ..Default::default()
        };
        ch.cls = c
            .get("cls")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|t| t.as_i64().unwrap_or(0)).collect())
            .unwrap_or_default();
        for s in c.get("series").and_then(|x| x.as_array()).into_iter().flatten() {
            ch.series.push((
                s.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                nums(s.get("v")),
            ));
        }
        p.charts.push(ch);
    }
    p
}

/// 面板里所有**时间轴**图表的 x 跨度（用于把播放头范围对齐到真实数据范围）。
/// 全是非时间轴图（如深度剖面）时返回 None —— 该类型不支持时间步进。
pub fn panel_time_span(p: &Panel) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in &p.charts {
        if !c.x_is_time {
            continue;
        }
        for v in &c.x {
            if v.is_finite() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
    }
    (lo.is_finite() && hi > lo).then_some((lo, hi))
}

// ── 播放头（时间步进回放，docs/20 §10） ───────────────────────────────────
/// 由 `tardis_cockpit_feed.py --mode panel` 每 ~100ms 发布到 `ws:tardis_replay:status`。
/// 面板据 `data_ts` 只画 x ≤ data_ts 的部分，形成时间步进动画。
#[derive(Clone, Default)]
pub struct Playhead {
    pub active: bool, // 有 panel 模式的回放记录（running/done/stopped 都算）
    pub running: bool,
    pub t0_ms: f64,
    pub t1_ms: f64,
    pub data_ts: f64,
    pub pct: f64,
    pub speed: f64,
    pub state: String,
}

pub const STATUS_KEY: &str = "ws:tardis_replay:status";

static PLAY: OnceLock<Mutex<Playhead>> = OnceLock::new();
static PLAY_POLLER: OnceLock<()> = OnceLock::new();

fn redis_url() -> String {
    std::env::var("WS_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// 读播放头（惰性起 100ms poller——与 feeder 的发布节奏对齐，动画才顺滑）。
pub fn playhead() -> Playhead {
    PLAY_POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            let mut conn = None;
            loop {
                if conn.is_none() {
                    conn = redis::Client::open(redis_url())
                        .ok()
                        .and_then(|c| c.get_connection().ok());
                }
                let mut ph = Playhead::default();
                if let Some(c) = conn.as_mut() {
                    use redis::Commands;
                    match c.get::<_, Option<String>>(STATUS_KEY) {
                        Ok(Some(s)) => parse_playhead(&s, &mut ph),
                        Ok(None) => {}
                        Err(_) => conn = None,
                    }
                }
                if let Ok(mut g) = PLAY.get_or_init(|| Mutex::new(Playhead::default())).lock() {
                    *g = ph;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    });
    PLAY.get().and_then(|m| m.lock().ok().map(|g| g.clone())).unwrap_or_default()
}

fn parse_playhead(s: &str, ph: &mut Playhead) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return;
    };
    // 只认 panel 模式：stream 模式（§8 喂 FS 图表）不该驱动本面板的游标。
    if v.get("mode").and_then(|x| x.as_str()) != Some("panel") {
        return;
    }
    let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    ph.state = v.get("state").and_then(|x| x.as_str()).unwrap_or("").to_string();
    ph.t0_ms = f("t0_ms");
    ph.t1_ms = f("t1_ms");
    ph.data_ts = f("data_ts");
    ph.pct = f("pct");
    ph.speed = f("speed");
    ph.running = ph.state == "running";
    ph.active = ph.t1_ms > ph.t0_ms;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_catalog_with_three_sources() {
        let c = parse_catalog(
            r#"{"type_labels":{"trades":"逐笔成交"},"all_types":["trades"],
                "sources":[
                  {"key":"tardis_duckdb","label":"Tardis · DuckDB 仓","available":true,
                   "symbols":["BTCUSDT"],"types":["trades"],"dates":{"BTCUSDT":["2026-06-01"]}},
                  {"key":"tardis_parquet","label":"P","available":true,
                   "symbols":["BTCUSDT"],"types":["trades"],"dates":{}},
                  {"key":"recorder","label":"R","available":false,
                   "symbols":[],"types":[],"dates":{}}]}"#,
        );
        assert_eq!(c.sources.len(), 3);
        assert_eq!(c.sources[0].key, "tardis_duckdb");
        assert!(c.sources[0].available);
        assert_eq!(c.sources[0].dates["BTCUSDT"], vec!["2026-06-01"]);
        assert!(!c.sources[2].available);
        assert_eq!(c.type_labels["trades"], "逐笔成交");
    }

    #[test]
    fn parses_panel_charts_of_each_kind() {
        let p = parse_panel(
            r#"{"source":"tardis_parquet","source_label":"P","symbol":"BTCUSDT",
                "date":"2026-06-01","start":"09:00","minutes":10,
                "type":"trades","type_label":"逐笔成交","rows":21423,
                "charts":[
                  {"id":"ohlc","title":"K","kind":"candle","x":[1,2],"o":[1,2],"h":[3,4],
                   "l":[0,1],"c":[2,3],"v":[10,20]},
                  {"id":"cvd","title":"CVD","kind":"line","x":[1,2],
                   "series":[{"name":"CVD","v":[0.5,null]}]},
                  {"id":"big","title":"大单","kind":"scatter","x":[1],"y":[9.0],
                   "size":[2.0],"cls":[1]},
                  {"id":"pf","title":"剖面","kind":"profile","bid_price":[1.0],
                   "bid_amount":[5.0],"ask_price":[2.0],"ask_amount":[6.0]}]}"#,
        );
        assert!(p.loaded);
        assert_eq!(p.rows, 21423);
        assert_eq!(p.charts.len(), 4);
        assert_eq!(p.charts[0].kind, "candle");
        assert_eq!(p.charts[0].h, vec![3.0, 4.0]);
        assert_eq!(p.charts[1].series[0].0, "CVD");
        assert!(p.charts[1].series[0].1[1].is_nan(), "null 应转 NaN 而非 0");
        assert_eq!(p.charts[2].cls, vec![1]);
        assert_eq!(p.charts[3].ask_amount, vec![6.0]);
    }

    #[test]
    fn panel_error_is_surfaced() {
        let p = parse_panel(
            r#"{"source":"recorder","source_label":"R","symbol":"BTCUSDT","date":"2026-06-13",
                "start":"09:00","minutes":2,"type":"liquidations","type_label":"强平",
                "rows":0,"charts":[],"error":"数据源「R」不提供类型「强平」"}"#,
        );
        assert_eq!(p.rows, 0);
        assert!(p.charts.is_empty());
        assert!(p.error.unwrap().contains("不提供"));
    }

    #[test]
    fn playhead_reads_panel_mode() {
        let mut ph = Playhead::default();
        parse_playhead(
            r#"{"run_id":"R","symbol":"BTCUSDT","date":"2026-06-01","mode":"panel",
                "t0_ms":1780304400000,"t1_ms":1780305000000,"data_ts":1780304700000,
                "pct":50.0,"speed":120.0,"state":"running"}"#,
            &mut ph,
        );
        assert!(ph.active && ph.running);
        assert_eq!(ph.data_ts, 1780304700000.0);
        assert!((ph.pct - 50.0).abs() < 1e-9);
    }

    /// §8 的 stream 模式状态不得驱动本面板游标（两者共用同一个 status key）。
    #[test]
    fn playhead_ignores_stream_mode() {
        let mut ph = Playhead::default();
        parse_playhead(
            r#"{"run_id":"R","symbol":"BTCUSDT","sent":24000,"total":50909,
                "pct":47.1,"speed":60.0,"state":"running"}"#,
            &mut ph,
        );
        assert!(!ph.active, "stream 模式不该被当成面板播放头");
        assert!(!ph.running);
    }

    #[test]
    fn garbage_does_not_panic() {
        assert!(parse_panel("nope").error.is_some());
        assert!(parse_catalog("nope").error.is_some());
    }
}
