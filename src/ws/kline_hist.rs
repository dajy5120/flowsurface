//! K 线图读管线 B 的历史文件（docs/64 §5）。
//!
//! 面板设置里记着选的文件（[`KlineHist`]）；这张图不订阅交易所、不补拉（`pane.rs` / `dashboard.rs` 两处拦），
//! 蜡烛由后台跑 `factory.replay.klines` 从本地文件聚合（各数据商走自己的接口，聚合只有那一份），
//! 读回后整份交给图。换周期 = 重新聚合。渲染线程不起子进程：子进程在命名线程里跑，结果放进这里的表，
//! 每帧 [`take_ready`] 取一次。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use data::layout::pane::KlineHist;

/// 聚合好的一份。
#[derive(Debug, Clone, PartialEq)]
pub struct Bars {
    /// 周期（秒）
    pub tf: u64,
    /// 最小价位（从成交价推出来的）
    pub tick: f32,
    /// [开盘时刻毫秒, 开, 高, 低, 收, 主动买量, 主动卖量]
    pub bars: Vec<[f64; 7]>,
    pub note: String,
}

#[derive(Debug, Clone)]
enum Load {
    Loading,
    Ready(Arc<Bars>),
    Failed(String),
}

struct Job {
    spec: KlineHist,
    tf: u64,
    load: Load,
    /// 已经交给图了
    delivered: bool,
}

static JOBS: Mutex<Option<HashMap<uuid::Uuid, Job>>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut HashMap<uuid::Uuid, Job>) -> T) -> T {
    let mut g = JOBS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(HashMap::new))
}

/// 这张图要（选的文件, 周期）：表里没有或对不上就起一个聚合。
pub fn ensure(pane: uuid::Uuid, spec: &KlineHist, tf: u64) {
    let start = with(|m| match m.get(&pane) {
        Some(j) if j.spec == *spec && j.tf == tf => false,
        _ => {
            m.insert(pane, Job { spec: spec.clone(), tf, load: Load::Loading, delivered: false });
            true
        }
    });
    if start {
        let spec = spec.clone();
        super::spawn_named("ws-klinehist", move || {
            let load = match run(pane, &spec, tf) {
                Ok(b) => Load::Ready(Arc::new(b)),
                Err(e) => Load::Failed(e),
            };
            with(|m| {
                // 期间换了选择 / 周期：这份作废
                if let Some(j) = m.get_mut(&pane)
                    && j.spec == spec
                    && j.tf == tf
                {
                    j.load = load;
                }
            });
        });
    }
}

/// 聚合好、还没交给图的那份（交一次）。
pub fn take_ready(pane: uuid::Uuid) -> Option<Arc<Bars>> {
    with(|m| {
        let j = m.get_mut(&pane)?;
        match (&j.load, j.delivered) {
            (Load::Ready(b), false) => {
                j.delivered = true;
                Some(b.clone())
            }
            _ => None,
        }
    })
}

/// 标题栏上的一句：聚合中 / 失败原因 / 几天有数据。
#[must_use]
pub fn status(pane: uuid::Uuid) -> Option<String> {
    with(|m| {
        m.get(&pane).map(|j| match &j.load {
            Load::Loading => "正在从本地文件聚合 K 线…".to_string(),
            Load::Failed(e) => format!("读不出来：{e}"),
            Load::Ready(b) => format!("{} 根 · {}", b.bars.len(), b.note),
        })
    })
}

/// 这张图不再读历史文件（换回交易所 / 面板关了）。
pub fn forget(pane: uuid::Uuid) {
    with(|m| m.remove(&pane));
}

/// 标题与徽标上的来源名：`B2 · Databento` / `B3 · 本地录制` / `B5 · 本地自有数据`。
#[must_use]
pub fn source_label(spec: &KlineHist) -> String {
    let b = match spec.source.as_str() {
        "recorder" => "B3",
        "own" => "B5",
        _ => "B2",
    };
    format!("{b} · {}", super::data_picker::source_label(&spec.source))
}

/// 日期段的一句：`2026-06-27 → 2026-06-28` 或 `2026-06-28`。
#[must_use]
pub fn span_label(spec: &KlineHist) -> String {
    if spec.end.is_empty() || spec.end == spec.date { spec.date.clone() } else { format!("{} → {}", spec.date, spec.end) }
}

/// 交给图的替身品种：flowsurface 的品种都挂在交易所上，这里借币安 U 本位的名字壳装历史文件的标的
/// （不会被订阅：这张图的流在 `pane.rs` 里拦掉了）。名字过长 / 带 `|` 的截掉。
#[must_use]
pub fn stand_in(spec: &KlineHist, tick: f32) -> exchange::TickerInfo {
    let mut sym: String = spec.symbol.chars().filter(|c| *c != '|').take(28).collect();
    if sym.is_empty() {
        sym = "HIST".into();
    }
    exchange::TickerInfo::new(exchange::Ticker::new(&sym, exchange::adapter::Exchange::BinanceLinear), tick.max(1e-6), 1e-6, None)
}

#[must_use]
pub fn to_klines(b: &Bars, ti: &exchange::TickerInfo) -> Vec<exchange::Kline> {
    b.bars
        .iter()
        .map(|r| {
            exchange::Kline::new(
                r[0] as u64,
                r[1] as f32,
                r[2] as f32,
                r[3] as f32,
                r[4] as f32,
                exchange::Volume::BuySell(exchange::unit::qty::Qty::from_f32(r[5] as f32), exchange::unit::qty::Qty::from_f32(r[6] as f32)),
                ti.min_ticksize,
            )
        })
        .collect()
}

fn run(pane: uuid::Uuid, spec: &KlineHist, tf: u64) -> Result<Bars, String> {
    let out = super::paths::hot_dir().join("cockpit").join("kline_hist").join(format!("{}.json", pane.simple()));
    let end = if spec.end.is_empty() { spec.date.clone() } else { spec.end.clone() };
    let st = std::process::Command::new(super::paths::python())
        .current_dir(super::paths::repo_root())
        .args(["-m", "factory.replay.klines", "--source", &spec.source, "--root", &spec.root, "--symbol", &spec.symbol])
        .args(["--start", &spec.date, "--end", &end, "--tf", &tf.to_string(), "--channel", &spec.channel])
        .arg("--out")
        .arg(&out)
        .output()
        .map_err(|e| format!("起不了 Python：{e}"))?;
    let text = std::fs::read_to_string(&out).map_err(|e| {
        let err = String::from_utf8_lossy(&st.stderr);
        format!("没有结果文件（{e}）{}", err.lines().last().unwrap_or(""))
    })?;
    parse(&text)
}

fn parse(text: &str) -> Result<Bars, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("结果不是 JSON：{e}"))?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        return Err(e.to_string());
    }
    let bars = v
        .get("bars")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let r = r.as_array()?;
                    let mut x = [0.0; 7];
                    for (i, c) in r.iter().take(7).enumerate() {
                        x[i] = c.as_f64()?;
                    }
                    Some(x)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if bars.is_empty() {
        return Err(format!("这段日期里没有成交（{}）", v.get("note").and_then(|n| n.as_str()).unwrap_or("")));
    }
    Ok(Bars {
        tf: v.get("tf").and_then(serde_json::Value::as_u64).unwrap_or(60),
        tick: v.get("tick").and_then(serde_json::Value::as_f64).unwrap_or(0.01) as f32,
        bars,
        note: v.get("note").and_then(|n| n.as_str()).unwrap_or("").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析聚合结果_错误原样带回() {
        let b = parse(r#"{"tf":60,"tick":0.1,"bars":[[1000,1.0,2.0,0.5,1.5,3.0,4.0]],"note":"1 天有数据"}"#).unwrap();
        assert_eq!((b.tf, b.tick, b.bars.len()), (60, 0.1, 1));
        assert_eq!(b.bars[0], [1000.0, 1.0, 2.0, 0.5, 1.5, 3.0, 4.0]);
        assert_eq!(parse(r#"{"error":"超过 200000 根"}"#).unwrap_err(), "超过 200000 根");
        assert!(parse(r#"{"tf":60,"tick":0.1,"bars":[],"note":"0 天有数据"}"#).unwrap_err().contains("没有成交"));
    }

    #[test]
    fn 替身品种与蜡烛() {
        let spec = KlineHist { source: "databento".into(), symbol: "ESZ6".into(), date: "2026-09-01".into(), ..Default::default() };
        let ti = stand_in(&spec, 0.25);
        assert_eq!(ti.ticker.to_full_symbol_and_type().0, "ESZ6");
        let b = Bars { tf: 60, tick: 0.25, bars: vec![[60_000.0, 100.0, 101.0, 99.5, 100.25, 2.0, 1.0]], note: String::new() };
        let k = to_klines(&b, &ti);
        assert_eq!(k[0].time.as_u64(), 60_000);
        assert_eq!(source_label(&spec), "B2 · Databento");
        assert_eq!(span_label(&spec), "2026-09-01");
    }
}
