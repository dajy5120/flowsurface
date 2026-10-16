//! 系统生成的绘图对象（docs/46 G4）：不存库、每次现算，**只读**（不能拖、不能删，可以复制成手工对象）。
//!
//! | 来源 | 从哪来 | 画成什么 |
//! |---|---|---|
//! | 订单 | `ws::orders` 的旁路快照（回测 / 观察模式的成交、持仓、挂单） | 成交 ▲▼ + 序号；持仓线（方向、数量、均价、费后盈亏）；挂单虚线 |
//! | 日历 | `ws::calendar_alerts` 的图上标记（P0 / P1 事件） | 虚线竖线 + 短名 |
//! | 策略 | 策略画布这次运行的决策轨迹（`ws::strategy_flow::chart_events`） | 成交处的 ● + 「线 原因」 |
//! | 外部 | `<小文件目录>/chart_drawings/*.json`（Python：`strategies/chart_drawings.py`） | 文件里写的任何对象，来源按文件定（研究 / 订单流 / 回测 / 策略） |
//!
//! 以前前三样是 K 线图里各画各的叠加层（不分品种、按逐笔的图上位置也不对）；现在都是同一套引擎画的对象，
//! 在对象树里看得见、能按来源隐藏、点了能看属性。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use super::model::{Anchor, Dash, Drawing, Kind, PRICE_SCALE, Source, Style, Tone};

fn units(price: f64) -> i64 {
    (price * PRICE_SCALE as f64).round() as i64
}

fn sys(id: String, kind: Kind, anchors: Vec<Anchor>, source: Source) -> Drawing {
    let mut d = Drawing::new(kind, anchors);
    d.id = id;
    d.source = source;
    d.text = None;
    d
}

// ── 订单 ─────────────────────────────────────────────────────────────────────

fn orders() -> Vec<Drawing> {
    use crate::ws::orders;
    let mut out = Vec::new();
    for f in orders::chart_fills_snapshot() {
        if f.ts == 0 || f.px <= 0.0 {
            continue;
        }
        let mut d = sys(format!("o:fill:{}:{}", f.seq, f.ts), Kind::Marker, vec![Anchor { t: f.ts, p: units(f.px) }], Source::Order);
        let buy = f.side == 1;
        d.dir = Some(if buy { 1 } else { -1 });
        d.tone = Some(if buy { Tone::Up } else { Tone::Down });
        d.text = (f.seq > 0).then(|| f.seq.to_string());
        out.push(d);
    }
    let pos = orders::chart_position_snapshot();
    if pos.side != "FLAT" && pos.net_qty.abs() > 1e-12 && pos.avg_px > 0.0 {
        let mut d = sys("o:pos".into(), Kind::HLine, vec![Anchor { t: 0, p: units(pos.avg_px) }], Source::Order);
        let long = pos.net_qty > 0.0;
        d.tone = Some(if long { Tone::Up } else { Tone::Down });
        d.style.width = 1.2;
        let u = pos.unrealized.map_or_else(|| "u—".to_string(), |v| format!("u{v:+.2}"));
        d.text = Some(format!("{} {:.4} @ {:.1} · {u} r{:+.2}", pos.side, pos.net_qty.abs(), pos.avg_px, pos.realized));
        out.push(d);
    }
    for (i, o) in orders::chart_working_snapshot().into_iter().enumerate() {
        if o.price <= 0.0 {
            continue;
        }
        let buy = o.side == 1;
        let mut d = sys(format!("o:wo:{}:{i}", o.order_id), Kind::HLine, vec![Anchor { t: 0, p: units(o.price) }], Source::Order);
        d.tone = Some(if buy { Tone::Up } else { Tone::Down });
        d.style = Style { width: 1.0, dash: Dash::Dashed, ..Style::default() };
        d.text = Some(format!("LMT {} {:.4} @ {:.1}", if buy { "BUY" } else { "SELL" }, o.qty, o.price));
        out.push(d);
    }
    out
}

// ── 日历 ─────────────────────────────────────────────────────────────────────

/// 日历取前后一年的事件：快照里是好几年的全部重要事件（上千个），全做成对象太重（G4 实测 1230 个）。
const CALENDAR_SPAN_MS: i64 = 366 * 86_400_000;

fn calendar() -> Vec<Drawing> {
    let snap = crate::ws::calendar_alerts::snapshot();
    if !snap.chart_lines {
        return Vec::new();
    }
    let now = chrono::Utc::now().timestamp_millis();
    snap.marks
        .iter()
        .filter(|m| m.at_ms >= 0 && (m.at_ms - now).abs() <= CALENDAR_SPAN_MS)
        .map(|m| {
            let mut d = sys(format!("c:{}:{}", m.at_ms, m.label), Kind::VLine, vec![Anchor { t: m.at_ms as u64, p: 0 }], Source::Calendar);
            d.tone = Some(if m.importance == 0 { Tone::Danger } else { Tone::Warn });
            d.style = Style { width: 1.0, dash: Dash::Dashed, ..Style::default() };
            d.text = Some(m.label.clone());
            d
        })
        .collect()
}

// ── 策略画布 ─────────────────────────────────────────────────────────────────

/// 这张图的品种键能不能对上一个代码（`ESU6`、`BTCUSDT`…）：键是「交易所 市场:代码」。
#[must_use]
pub fn symbol_matches(key: &str, sym: &str) -> bool {
    key == sym || key.rsplit(':').next().is_some_and(|code| code.eq_ignore_ascii_case(sym))
}

fn strategy(symbol: &str) -> Vec<Drawing> {
    let Some((run_sym, events)) = crate::ws::strategy_flow::chart_events() else { return Vec::new() };
    if !symbol_matches(symbol, &run_sym) {
        return Vec::new();
    }
    events
        .iter()
        .filter_map(|e| {
            let ts = e.get("ts")?.as_i64()?;
            let price = e.get("why")?.get("price")?.as_f64()?;
            let s = |k: &str| e.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
            let mut d = sys(format!("s:{ts}:{}", s("lane")), Kind::Marker, vec![Anchor { t: (ts / 1_000_000).max(0) as u64, p: units(price) }], Source::Strategy);
            d.dir = Some(0);
            d.tone = Some(Tone::Accent);
            d.text = Some(format!("{} {}", s("lane"), s("reason")).trim().to_string());
            Some(d)
        })
        .collect()
}

// ── 外部（Python 接口）────────────────────────────────────────────────────────

/// 外部文件的目录：`$WS_CHART_DRAWINGS`，否则 `<小文件目录>/chart_drawings`（与 `strategies/chart_drawings.py` 同一处）。
#[must_use]
pub fn external_dir() -> PathBuf {
    std::env::var_os("WS_CHART_DRAWINGS").map_or_else(|| crate::ws::paths::hot_dir().join("chart_drawings"), PathBuf::from)
}

/// 外部文件格式（`format = 1`）。价格写成数，时间写毫秒。
#[derive(Debug, Deserialize)]
struct ExtFile {
    #[serde(default)]
    name: String,
    /// 品种：完整键（`Binance Linear:BTCUSDT`）或只写代码（`BTCUSDT`，匹配所有交易所的这个代码）
    symbol: String,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    objects: Vec<ExtObj>,
}

#[derive(Debug, Deserialize)]
struct ExtAnchor {
    t: u64,
    #[serde(default)]
    price: f64,
}

#[derive(Debug, Deserialize)]
struct ExtObj {
    #[serde(default)]
    id: Option<String>,
    kind: Kind,
    anchors: Vec<ExtAnchor>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tone: Option<Tone>,
    #[serde(default)]
    color: Option<[u8; 4]>,
    #[serde(default)]
    width: Option<f32>,
    #[serde(default)]
    dash: Option<Dash>,
    #[serde(default)]
    dir: Option<i8>,
    #[serde(default)]
    rows: Option<u32>,
    #[serde(default)]
    va: Option<f64>,
    #[serde(default)]
    levels: Vec<f64>,
}

/// 读到的外部对象：(文件名, 品种, 对象)；带版本号（文件变了 +1）。
#[derive(Default)]
struct ExtCache {
    files: HashMap<PathBuf, (SystemTime, String, Vec<Drawing>)>,
    version: u64,
    note: String,
    checked: Option<Instant>,
}

fn ext_cache() -> &'static Mutex<ExtCache> {
    static C: OnceLock<Mutex<ExtCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// 一个外部文件 → 对象（品种、对象）。坏文件返回说明。
fn parse_ext(name: &str, text: &str) -> Result<(String, Vec<Drawing>), String> {
    let f: ExtFile = serde_json::from_str(text).map_err(|e| format!("{name}：{e}"))?;
    let source = f.source.filter(|s| *s != Source::Manual).unwrap_or(Source::Research);
    let label = if f.name.is_empty() { name.to_string() } else { f.name.clone() };
    let objs = f
        .objects
        .into_iter()
        .enumerate()
        .filter(|(_, o)| !o.anchors.is_empty())
        .map(|(i, o)| {
            let anchors = o.anchors.iter().map(|a| Anchor { t: a.t, p: units(a.price) }).collect();
            let mut d = sys(format!("x:{name}:{}", o.id.unwrap_or_else(|| i.to_string())), o.kind, anchors, source);
            d.text = o.text;
            d.tone = o.tone;
            d.dir = o.dir;
            d.rows = o.rows;
            d.va = o.va;
            d.levels = o.levels;
            d.group = Some(label.clone());
            if let Some(c) = o.color {
                d.style.color = c;
            }
            if let Some(w) = o.width {
                d.style.width = w;
            }
            if let Some(ds) = o.dash {
                d.style.dash = ds;
            }
            d
        })
        .collect();
    Ok((f.symbol, objs))
}

/// 重扫外部目录（最多每 2 秒一次；只重读改过的文件）。
fn refresh_external() {
    let mut c = ext_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
        return;
    }
    c.checked = Some(Instant::now());
    let dir = external_dir();
    let mut seen = Vec::new();
    let mut changed = false;
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else { continue };
        seen.push(p.clone());
        if c.files.get(&p).is_some_and(|(m, ..)| *m == mtime) {
            continue;
        }
        let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| parse_ext(&name, &t)) {
            Ok((sym, objs)) => {
                c.files.insert(p, (mtime, sym, objs));
            }
            Err(e) => {
                c.note = format!("外部画线读不了：{e}");
                c.files.insert(p, (mtime, String::new(), Vec::new()));
            }
        }
        changed = true;
    }
    let before = c.files.len();
    c.files.retain(|p, _| seen.contains(p));
    if changed || c.files.len() != before {
        c.version += 1;
    }
}

fn external(symbol: &str) -> Vec<Drawing> {
    refresh_external();
    let c = ext_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    c.files.values().filter(|(_, sym, _)| !sym.is_empty() && symbol_matches(symbol, sym)).flat_map(|(_, _, o)| o.iter().cloned()).collect()
}

/// 外部画线最近一条说明（读不了的文件）。
#[must_use]
pub fn external_note() -> String {
    ext_cache().lock().map(|c| c.note.clone()).unwrap_or_default()
}

// ── 汇总 ─────────────────────────────────────────────────────────────────────

/// 这个品种的全部系统对象（`hidden`：隐藏了哪些来源）。订单与日历不分品种（回测 / 观察模式只有一个品种在跑；
/// 日历事件对哪个品种都一样）。
#[must_use]
pub fn items(symbol: &str, hidden: &[Source]) -> Vec<Drawing> {
    // 每次鼠标移动都要几次（画、命中、悬停、价格轴）：250 毫秒内同一品种、同一组隐藏来源直接复用
    type Cached = Option<(Instant, String, Vec<Source>, Vec<Drawing>)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    if !cfg!(test)
        && let Ok(g) = CACHE.lock()
        && let Some((t, sym, hid, v)) = g.as_ref()
        && t.elapsed() < Duration::from_millis(250)
        && sym == symbol
        && hid.as_slice() == hidden
    {
        return v.clone();
    }
    let v = compute(symbol, hidden);
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), symbol.to_string(), hidden.to_vec(), v.clone()));
    }
    v
}

fn compute(symbol: &str, hidden: &[Source]) -> Vec<Drawing> {
    let on = |s: Source| !hidden.contains(&s);
    let mut out = Vec::new();
    if on(Source::Order) {
        out.extend(orders());
    }
    if on(Source::Calendar) {
        out.extend(calendar());
    }
    if on(Source::Strategy) {
        out.extend(strategy(symbol));
    }
    out.extend(external(symbol).into_iter().filter(|d| on(d.source)));
    #[cfg(test)]
    out.extend(test_injected(symbol).into_iter().filter(|d| on(d.source)));
    out
}

/// 测试用：给某个品种塞几个系统对象（不碰全局快照与外部目录）。
#[cfg(test)]
static INJECTED: Mutex<Vec<(String, Drawing)>> = Mutex::new(Vec::new());

#[cfg(test)]
pub fn inject(symbol: &str, d: Drawing) {
    INJECTED.lock().unwrap().push((symbol.to_string(), d));
}

#[cfg(test)]
fn test_injected(symbol: &str) -> Vec<Drawing> {
    INJECTED.lock().unwrap().iter().filter(|(s, _)| s == symbol).map(|(_, d)| d.clone()).collect()
}

/// 系统对象的指纹：变了各图要重画绘图层。算一次要拷几份快照，所以最多每 250 毫秒算一次。
#[must_use]
pub fn fingerprint() -> u64 {
    static LAST: Mutex<Option<(Instant, u64)>> = Mutex::new(None);
    let mut g = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((t, v)) = *g
        && t.elapsed() < Duration::from_millis(250)
    {
        return v;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let fills = crate::ws::orders::chart_fills_snapshot();
    (fills.len(), fills.last().map(|f| (f.ts, f.seq))).hash(&mut h);
    let pos = crate::ws::orders::chart_position_snapshot();
    (pos.side, pos.net_qty.to_bits(), pos.avg_px.to_bits(), pos.realized.to_bits(), pos.unrealized.map(f64::to_bits)).hash(&mut h);
    for o in crate::ws::orders::chart_working_snapshot() {
        (o.order_id, o.price.to_bits(), o.qty.to_bits()).hash(&mut h);
    }
    let cal = crate::ws::calendar_alerts::snapshot();
    (cal.chart_lines, cal.marks.len(), cal.marks.first().map(|m| m.at_ms)).hash(&mut h);
    crate::ws::strategy_flow::chart_events().map(|(s, e)| (s, e.len())).hash(&mut h);
    refresh_external();
    ext_cache().lock().map(|c| c.version).unwrap_or(0).hash(&mut h);
    let v = h.finish();
    *g = Some((Instant::now(), v));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_file_format_and_symbol_matching() {
        let j = r#"{"format": 1, "name": "OFMS 2026-06-01 00:00", "symbol": "BTCUSDT", "source": "order_flow",
            "objects": [
              {"kind": "marker", "anchors": [{"t": 1780272006180, "price": 73643.7}], "dir": -1, "tone": "down", "text": "吸收"},
              {"kind": "h_line", "id": "poc", "anchors": [{"t": 0, "price": 73600}], "text": "POC", "dash": "dashed"},
              {"kind": "vol_profile", "anchors": [{"t": 1, "price": 0}, {"t": 2, "price": 0}], "rows": 30}
            ]}"#;
        let (sym, objs) = parse_ext("ofms", j).unwrap();
        assert_eq!((sym.as_str(), objs.len()), ("BTCUSDT", 3));
        assert_eq!((objs[0].kind, objs[0].dir, objs[0].tone, objs[0].source), (Kind::Marker, Some(-1), Some(Tone::Down), Source::OrderFlow));
        assert_eq!(objs[0].anchors[0].p, 7_364_370_000_000_000, "价格按 10⁻¹¹ 定点");
        assert_eq!((objs[1].id.as_str(), objs[1].style.dash, objs[1].group.as_deref()), ("x:ofms:poc", Dash::Dashed, Some("OFMS 2026-06-01 00:00")));
        assert!(!objs[0].editable(), "外部对象只读");
        assert_eq!(objs[2].rows, Some(30));
        assert!(parse_ext("bad", "{").is_err());
        assert!(symbol_matches("Binance Linear:BTCUSDT", "BTCUSDT") && symbol_matches("Binance Linear:BTCUSDT", "Binance Linear:BTCUSDT"));
        assert!(!symbol_matches("Binance Linear:BTCUSDT", "ETHUSDT") && !symbol_matches("Binance Linear:BTCUSDTX", "BTCUSDT"));
    }
}
