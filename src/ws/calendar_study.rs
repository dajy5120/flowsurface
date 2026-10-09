//! 金融日历「研究」页的数据（docs/43 §8 第 2 条、K5c）：历史上事件前后发生过什么——**不是预测**。
//!
//! 计算在 Python（`strategies/research/calendar_events`，传统市场 Databento 日线、加密 Tardis 分钟线），
//! 写到事件库同目录的 `calendar_study.json`；这里只读它（文件修改时间变了才重新解析）。
//! 「重新计算」是人工触发、后台跑一次（不定时、不常驻），约十几秒。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Signed {
    pub mean: Option<f64>,
    pub drift: Option<f64>,
    pub adj: Option<f64>,
    pub t: Option<f64>,
    pub p: Option<f64>,
    pub q: Option<f64>,
    pub median_adj: Option<f64>,
    pub adj_trim: Option<f64>,
}

/// 日线：一个品种 × 一个系列。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DailyRow {
    pub series: String,
    pub n: usize,
    pub first: String,
    pub last: String,
    pub shared_days: usize,
    pub abs_ratio: Option<f64>,
    pub z_ratio: Option<f64>,
    pub z_q: Option<f64>,
    pub vol_ratio: Option<f64>,
    pub vol_q: Option<f64>,
    pub signed: Signed,
    pub extreme_day: String,
    pub few: bool,
    pub base_n: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DailyBlock {
    pub symbol: String,
    pub vendor: String,
    pub start: String,
    pub end: String,
    pub rows: Vec<DailyRow>,
}

/// 日内：一个视界。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Horizon {
    pub min: u32,
    pub ret: Option<f64>,
    pub drift: Option<f64>,
    pub adj: Option<f64>,
    pub abs_ratio: Option<f64>,
    pub abs_pct: Option<f64>,
    pub vol_ratio: Option<f64>,
}

/// 日内：一行是一个事件时刻（`kind = "moment"`）或一个系列的汇总（`"summary"`，`series` 是字符串）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IntraRow {
    pub kind: String,
    pub at: String,
    pub series: serde_json::Value,
    pub base_n: usize,
    pub overlap: Vec<String>,
    pub pre: Option<f64>,
    pub pre_drift: Option<f64>,
    pub h: Vec<Horizon>,
}

impl IntraRow {
    pub fn series_list(&self) -> Vec<String> {
        match &self.series {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IntraBlock {
    pub symbol: String,
    pub vendor: String,
    pub start: String,
    pub end: String,
    pub rows: Vec<IntraRow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Params {
    pub vol_lookback: u32,
    pub horizons_min: Vec<u32>,
    pub pre_min: u32,
    pub n_perm: u32,
    pub min_n: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Coverage {
    pub first: String,
    pub last: String,
    pub n: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Report {
    pub version: u32,
    pub generated_ms: i64,
    pub seed: i64,
    pub params: Params,
    pub series_names: std::collections::BTreeMap<String, String>,
    pub coverage: std::collections::BTreeMap<String, Coverage>,
    pub daily: Vec<DailyBlock>,
    pub intraday: Vec<IntraBlock>,
    pub errors: Vec<String>,
    pub tests: usize,
}

/// 系列在页面上的顺序（P0 在前）；报告里有、这里没列的排在后面。
pub const SERIES_ORDER: [&str; 6] = ["fomc-decision", "ecb-decision", "us-cpi", "us-nfp", "us-gdp", "us-pce"];

impl Report {
    pub fn name<'a>(&'a self, series: &'a str) -> &'a str {
        self.series_names.get(series).map_or(series, String::as_str)
    }

    /// 报告里出现过的系列（日线或日内），按 [`SERIES_ORDER`]。
    pub fn series(&self) -> Vec<String> {
        let mut all: Vec<String> = self.daily.iter().flat_map(|b| b.rows.iter().map(|r| r.series.clone())).collect();
        for b in &self.intraday {
            for r in &b.rows {
                all.extend(r.series_list());
            }
        }
        all.sort_by_key(|s| (SERIES_ORDER.iter().position(|x| x == s).unwrap_or(usize::MAX), s.clone()));
        all.dedup();
        all
    }
}

pub fn report_path() -> PathBuf {
    super::calendar_readout::db_path().with_file_name("calendar_study.json")
}

#[derive(Default)]
struct State {
    cache: Option<(SystemTime, Arc<Report>)>,
    err: String,
    running: Option<Instant>,
    note: String,
}

static STATE: Mutex<State> = Mutex::new(State { cache: None, err: String::new(), running: None, note: String::new() });

/// 读报告：没有文件 = `Ok(None)`；文件坏了 = `Err`。修改时间没变不重新解析。
pub fn load() -> Result<Option<Arc<Report>>, String> {
    let p = report_path();
    let Ok(mtime) = std::fs::metadata(&p).and_then(|m| m.modified()) else {
        return Ok(None);
    };
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, r)) = &g.cache
        && *t == mtime
    {
        return Ok(Some(r.clone()));
    }
    match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
        Ok(r) => {
            let r = Arc::new(r);
            g.cache = Some((mtime, r.clone()));
            g.err.clear();
            Ok(Some(r))
        }
        Err(e) => {
            g.err = format!("{} 读不出来：{e}", p.display());
            Err(g.err.clone())
        }
    }
}

pub fn parse(t: &str) -> Result<Report, String> {
    serde_json::from_str(t).map_err(|e| e.to_string())
}

/// 在跑吗（跑了多少秒）、上次的回执。
pub fn status() -> (Option<u64>, String) {
    let g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    (g.running.map(|t| t.elapsed().as_secs()), g.note.clone())
}

/// 后台重新计算一次。已经在跑就不再起。
pub fn run_now() {
    {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if g.running.is_some() {
            return;
        }
        g.running = Some(Instant::now());
        g.note.clear();
    }
    super::spawn_named("ws-cal-study", || {
        let t0 = Instant::now();
        let out = std::process::Command::new(super::paths::python())
            .current_dir(super::paths::repo_root())
            .args(["-m", "strategies.research.calendar_events.run", "--out"])
            .arg(report_path())
            .env("WS_CALENDAR_DB", super::calendar_readout::db_path())
            .env("PYTHONUNBUFFERED", "1")
            .stdin(std::process::Stdio::null())
            .output();
        let note = match out {
            Ok(o) if o.status.success() => format!("✔ 已重新计算（{} 秒）", t0.elapsed().as_secs()),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                format!("✗ 计算失败：{last}")
            }
            Err(e) => format!("✗ 起不了 Python（{}）：{e}", super::paths::python().display()),
        };
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        g.running = None;
        g.note = note;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_from_the_python_side_parses_and_lists_series_in_order() {
        // 形状与 strategies/research/calendar_events/run.py 写的一致（NaN 已写成 null）
        let t = r#"{"version":1,"generated_ms":1,"seed":0,
          "params":{"vol_lookback":20,"horizons_min":[5,15,60,240],"pre_min":30,"n_perm":20000,"min_n":8},
          "series_names":{"us-cpi":"美国 CPI","fomc-decision":"FOMC 利率决议"},
          "coverage":{"us-cpi":{"first":"2025-01-15","last":"2026-09-11","n":20}},
          "daily":[{"symbol":"ES","vendor":"Databento","bars":"1d","start":"2010-06-07","end":"2026-10-02",
            "rows":[{"series":"us-cpi","n":20,"first":"2025-01-15","last":"2026-09-11","shared_days":0,
              "abs_ratio":1.31,"z_ratio":1.14,"z_p":0.1,"z_q":0.4,"vol_ratio":1.1,"vol_p":0.2,"vol_q":null,
              "signed":{"mean":-0.0003,"drift":0.001,"adj":-0.0013,"t":-0.5,"p":0.6,"q":0.7,"median_adj":0.0,"adj_trim":0.0009,"trimmed":-0.02},
              "extreme_day":"2025-04-10","few":false,"base_n":300}]}],
          "intraday":[{"symbol":"BTCUSDT","vendor":"Tardis","bars":"1m","start":"2026-06-01 00:00:00","end":"2026-06-30 23:59:00",
            "rows":[{"kind":"summary","series":"fomc-decision","n":1,"few":true,"h":[{"min":5,"abs_ratio":13.9}]},
                    {"kind":"moment","at":"2026-06-17T18:00:00","series":["fomc-decision"],"base_n":18,"overlap":[],
                     "pre":0.002,"pre_drift":0.0,"h":[{"min":5,"ret":-0.013,"drift":0.0,"adj":-0.013,"abs_ratio":13.9,"abs_pct":1.0,"vol_ratio":12.1}]}]}],
          "errors":[],"tests":135}"#;
        let r = parse(t).unwrap();
        assert_eq!(r.series(), ["fomc-decision", "us-cpi"]);
        assert_eq!(r.daily[0].rows[0].vol_q, None);
        assert_eq!(r.intraday[0].rows[1].series_list(), ["fomc-decision"]);
        assert_eq!(r.name("us-cpi"), "美国 CPI");
        assert_eq!(r.name("x"), "x");
    }
}
