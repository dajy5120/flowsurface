//! 市场状态在 K 线上（docs/62 R2）：读状态引擎写的时间线文件，画底色带、底部状态条、切换标记，十字线读数里列出各维度。
//!
//! | 来源 | 从哪来 | 画成什么 |
//! |---|---|---|
//! | 时间线文件 | `<小文件目录>/regime/<代码>_<周期>.json`（`$WS_REGIME_DIR` 覆盖；Python：`python -m factory.regime run`） | 底色带（选一个维度）、底部条带（若干维度）、切换处的竖短线 + 新状态名 |
//!
//! - **只用当时已知的状态**：段的开始时刻是确认那根 K 线的**收盘**（= 下一根开始），所以底色从确认后的下一根开始。
//! - 颜色：多 / 空跟涨跌色（随涨跌约定与色弱开关），其余用中性色与状态色（docs/35：状态色 ≠ 涨跌色）。
//! - 设置是全局的（所有 K 线图一份），存在 `<小文件目录>/cockpit/regime_overlay.json`；改在检查器「属性」页。
//! - 每两秒看一次目录，文件改了就重读并让各图重画（[`changed_since`]）。只画时间基的图。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Size};
use serde::{Deserialize, Serialize};

use crate::ui::pal;

// ── 文件 ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct StateDef {
    pub key: String,
    pub name: String,
    pub tone: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DimDef {
    pub key: String,
    #[serde(default)]
    pub layer: String,
    pub name: String,
    pub states: Vec<StateDef>,
}

#[derive(Debug, Clone)]
pub struct Seg {
    pub t0: u64,
    /// 空 = 持续到最新
    pub t1: Option<u64>,
    pub s: usize,
    pub why: String,
}

#[derive(Debug, Clone)]
pub struct RegimeFile {
    pub symbol: String,
    pub timeframe: String,
    pub tf_ms: u64,
    pub method: String,
    pub generated_ms: i64,
    pub dims: Vec<DimDef>,
    pub segs: HashMap<String, Vec<Seg>>,
}

/// 文件里的一段：`[开始, 结束|null, 状态下标, 原因]`。
type RawSeg = (i64, Option<i64>, usize, String);

/// 十字线读数的一项：（维度名, 状态名, 颜色）。
pub type StateItem = (String, String, Option<Color>);

#[derive(Deserialize)]
struct Raw {
    format: u32,
    symbol: String,
    timeframe: String,
    tf_ms: u64,
    #[serde(default)]
    method: String,
    #[serde(default)]
    generated_ms: i64,
    dimensions: Vec<DimDef>,
    segments: HashMap<String, Vec<RawSeg>>,
}

/// 解析一个时间线文件。坏文件返回说明。
pub fn parse(text: &str) -> Result<RegimeFile, String> {
    let r: Raw = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if r.format != 1 {
        return Err(format!("不认识的格式 {}", r.format));
    }
    let mut segs = HashMap::new();
    for d in &r.dimensions {
        let n = d.states.len();
        let v: Vec<Seg> = r
            .segments
            .get(&d.key)
            .map(|v| {
                v.iter()
                    .filter(|(t0, _, s, _)| *t0 >= 0 && *s < n)
                    .map(|(t0, t1, s, why)| Seg { t0: *t0 as u64, t1: t1.map(|x| x.max(0) as u64), s: *s, why: why.clone() })
                    .collect()
            })
            .unwrap_or_default();
        segs.insert(d.key.clone(), v);
    }
    Ok(RegimeFile {
        symbol: r.symbol,
        timeframe: r.timeframe,
        tf_ms: r.tf_ms,
        method: r.method,
        generated_ms: r.generated_ms,
        dims: r.dimensions,
        segs,
    })
}

impl RegimeFile {
    pub fn dim(&self, key: &str) -> Option<&DimDef> {
        self.dims.iter().find(|d| d.key == key)
    }

    /// 时刻 t 所在的段（段按开始时刻升序、首尾相接）。
    pub fn at(&self, key: &str, t: u64) -> Option<&Seg> {
        let v = self.segs.get(key)?;
        let k = v.partition_point(|s| s.t0 <= t);
        let s = v.get(k.checked_sub(1)?)?;
        s.t1.is_none_or(|e| t < e).then_some(s)
    }

    /// 和 [lo, hi] 有重叠的段。
    pub fn overlapping(&self, key: &str, lo: u64, hi: u64) -> &[Seg] {
        let Some(v) = self.segs.get(key) else { return &[] };
        let a = v.partition_point(|s| s.t1.is_some_and(|e| e <= lo));
        let b = v.partition_point(|s| s.t0 <= hi);
        &v[a..b.max(a)]
    }
}

/// 时间线文件目录：`$WS_REGIME_DIR`，否则 `<小文件目录>/regime`（与 `factory/regime/__main__.py` 同一处）。
#[must_use]
pub fn files_dir() -> PathBuf {
    std::env::var_os("WS_REGIME_DIR").map_or_else(|| crate::ws::paths::hot_dir().join("regime"), PathBuf::from)
}

#[derive(Default)]
struct Cache {
    checked: Option<Instant>,
    files: HashMap<PathBuf, (SystemTime, Option<Arc<RegimeFile>>)>,
    note: String,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

static FILES_VER: AtomicU64 = AtomicU64::new(1);
static SETTINGS_VER: AtomicU64 = AtomicU64::new(1);

fn refresh() {
    let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
        return;
    }
    c.checked = Some(Instant::now());
    let mut seen = Vec::new();
    let mut changed = false;
    for e in std::fs::read_dir(files_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else { continue };
        seen.push(p.clone());
        if c.files.get(&p).is_some_and(|(m, _)| *m == mtime) {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
            Ok(f) => {
                c.files.insert(p, (mtime, Some(Arc::new(f))));
            }
            Err(e) => {
                c.note = format!("{name} 读不了：{e}");
                c.files.insert(p, (mtime, None));
            }
        }
        changed = true;
    }
    let before = c.files.len();
    c.files.retain(|p, _| seen.contains(p));
    if changed || c.files.len() != before {
        FILES_VER.fetch_add(1, Ordering::Relaxed);
    }
}

/// 这个品种（图的品种键或代码）有的全部时间线，按周期从小到大。
#[must_use]
pub fn files_for(symbol: &str) -> Vec<Arc<RegimeFile>> {
    refresh();
    let c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut v: Vec<Arc<RegimeFile>> = c
        .files
        .values()
        .filter_map(|(_, f)| f.clone())
        .filter(|f| crate::ws::drawing::feeds::symbol_matches(symbol, &f.symbol))
        .collect();
    v.sort_by_key(|f| f.tf_ms);
    v
}

/// 最近一个读不了的文件的说明（检查器里显示）。
#[must_use]
pub fn note() -> String {
    cache().lock().map(|c| c.note.clone()).unwrap_or_default()
}

/// 给一张周期为 `chart_iv` 的图挑时间线：设置里指定了周期就用它；否则周期最接近的（一样近取不小于图周期的）。
#[must_use]
pub fn pick(files: &[Arc<RegimeFile>], chart_iv: u64, want: Option<&str>) -> Option<Arc<RegimeFile>> {
    if let Some(w) = want
        && let Some(f) = files.iter().find(|f| f.timeframe == w)
    {
        return Some(f.clone());
    }
    let iv = chart_iv.max(1) as f64;
    files
        .iter()
        .min_by(|a, b| {
            let da = (a.tf_ms.max(1) as f64 / iv).ln().abs();
            let db = (b.tf_ms.max(1) as f64 / iv).ln().abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal).then((a.tf_ms < chart_iv).cmp(&(b.tf_ms < chart_iv)))
        })
        .cloned()
}

// ── 设置 ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub on: bool,
    /// 底色用哪个维度
    pub band: String,
    /// 底部条带（从上到下）
    pub strips: Vec<String>,
    /// 底色维度切换处画竖短线 + 新状态名
    pub marks: bool,
    /// 指定周期；空 = 跟图的周期自动挑
    pub tf: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            on: true,
            band: "composite".into(),
            strips: vec!["direction".into(), "trend_quality".into(), "vol_level".into(), "structure".into()],
            marks: true,
            tf: None,
        }
    }
}

fn settings_path() -> PathBuf {
    crate::ws::paths::hot_dir().join("cockpit").join("regime_overlay.json")
}

fn settings_cell() -> &'static Mutex<Settings> {
    static S: OnceLock<Mutex<Settings>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(std::fs::read_to_string(settings_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
    })
}

#[must_use]
pub fn settings() -> Settings {
    settings_cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

#[derive(Debug, Clone)]
pub enum RegimeMsg {
    On(bool),
    Band(String),
    Strip(String, bool),
    Marks(bool),
    Tf(Option<String>),
}

/// 改设置：存盘 + 让各图重画。
pub fn handle(m: RegimeMsg) {
    let mut s = settings_cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    apply(&mut s, m);
    let p = settings_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(t) = serde_json::to_string_pretty(&*s) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    SETTINGS_VER.fetch_add(1, Ordering::Relaxed);
}

fn apply(s: &mut Settings, m: RegimeMsg) {
    match m {
        RegimeMsg::On(v) => s.on = v,
        RegimeMsg::Band(k) => s.band = k,
        RegimeMsg::Strip(k, on) => {
            s.strips.retain(|x| *x != k);
            if on {
                s.strips.push(k);
            }
        }
        RegimeMsg::Marks(v) => s.marks = v,
        RegimeMsg::Tf(t) => s.tf = t,
    }
}

/// 状态叠加层要不要重画：文件或设置变了。`seen` 是这张图上次画的版本。
#[must_use]
pub fn changed_since(seen: &AtomicU64) -> bool {
    refresh();
    let v = FILES_VER.load(Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ SETTINGS_VER.load(Ordering::Relaxed);
    seen.swap(v, Ordering::Relaxed) != v
}

// ── 颜色 ─────────────────────────────────────────────────────────────────────

/// 色调 → 颜色（不透明）。`none` 不着色。
#[must_use]
pub fn tone_color(tone: &str) -> Option<Color> {
    Some(match tone {
        "bull" | "bull_event" => pal::up(),
        "bear" | "bear_event" => pal::down(),
        "bull_soft" => pal::mix(pal::up(), pal::dim(), 0.5),
        "bear_soft" => pal::mix(pal::down(), pal::dim(), 0.5),
        "range" => pal::info(),
        "warn" => pal::warn(),
        "danger" => pal::bad(),
        "calm" => pal::series(4),
        "strong" => pal::accent(),
        "moderate" => pal::alpha(pal::accent(), 0.6),
        "weak" => pal::alpha(pal::accent(), 0.3),
        _ => return None,
    })
}

fn state_color(d: &DimDef, s: usize) -> Option<Color> {
    d.states.get(s).and_then(|st| tone_color(&st.tone))
}

/// 事件类状态（突破等）底色加深一点。
fn tone_strength(d: &DimDef, s: usize) -> f32 {
    d.states.get(s).map_or(1.0, |st| if st.tone.ends_with("_event") { 1.6 } else { 1.0 })
}

// ── 画 ───────────────────────────────────────────────────────────────────────

/// wgpu 在线性空间混合：深色底上 8% 看起来像 25%，所以底色给得很淡（真图上目测定的）
const BAND_ALPHA: f32 = 0.035;
const STRIP_H: f32 = 10.0;
const STRIP_GAP: f32 = 1.0;

/// 在 K 线主图上画状态叠加层（时间基）。`interval_to_x`：K 线开始时刻 → 这根中心的 x；`chart_iv`：图的周期（毫秒）。
#[allow(clippy::too_many_arguments)]
pub fn draw_overlay(
    frame: &mut canvas::Frame,
    region: Rectangle,
    interval_to_x: &impl Fn(u64) -> f32,
    scaling: f32,
    earliest: u64,
    latest: u64,
    symbol: &str,
    chart_iv: u64,
) {
    let s = settings();
    if !s.on || scaling <= f32::EPSILON || chart_iv == 0 {
        return;
    }
    let Some(f) = pick(&files_for(symbol), chart_iv, s.tf.as_deref()) else { return };
    let half = (interval_to_x(earliest + chart_iv) - interval_to_x(earliest)) / 2.0;
    let (rx0, rx1) = (region.x, region.x + region.width);
    let hi = latest.saturating_add(chart_iv);
    let span = |seg: &Seg| -> Option<(f32, f32)> {
        let x0 = (interval_to_x(seg.t0) - half).clamp(rx0, rx1);
        let x1 = (interval_to_x(seg.t1.unwrap_or(hi).min(hi)) - half).clamp(rx0, rx1);
        (x1 > x0).then_some((x0, x1))
    };

    // 底色带
    if let Some(d) = f.dim(&s.band) {
        for seg in f.overlapping(&s.band, earliest, hi) {
            if let (Some(c), Some((x0, x1))) = (state_color(d, seg.s), span(seg)) {
                frame.fill_rectangle(
                    Point::new(x0, region.y),
                    Size::new(x1 - x0, region.height),
                    pal::alpha(c, BAND_ALPHA * tone_strength(d, seg.s)),
                );
            }
        }
        // 切换标记：竖短线 + 新状态名（太挤的省掉名字）。放在底部条带正上方、不放图顶：
        // 画布的文字总画在所有底板之上，放图顶会透过十字线读数的底板
        if s.marks {
            let base = region.y + region.height - s.strips.len() as f32 * (STRIP_H + STRIP_GAP) / scaling;
            let mut last_label = f32::NEG_INFINITY;
            for seg in f.overlapping(&s.band, earliest, hi) {
                let x = interval_to_x(seg.t0) - half;
                if x < rx0 || x > rx1 {
                    continue;
                }
                let c = state_color(d, seg.s).unwrap_or_else(pal::dim);
                frame.fill_rectangle(Point::new(x, base - 10.0 / scaling), Size::new(1.0 / scaling, 10.0 / scaling), c);
                // 「不明」之类不着色的状态只画竖线、不写名字（太多了会糊成一片）
                let named = d.states.get(seg.s).is_some_and(|st| tone_color(&st.tone).is_some());
                if named && (x - last_label) * scaling >= 70.0 {
                    frame.fill_text(canvas::Text {
                        content: d.states.get(seg.s).map(|st| st.name.clone()).unwrap_or_default(),
                        position: Point::new(x + 2.0 / scaling, base - 13.0 / scaling),
                        size: iced::Pixels(9.0 / scaling),
                        color: c,
                        font: crate::ui::text::ui_font(),
                        ..canvas::Text::default()
                    });
                    last_label = x;
                }
            }
        }
    }

    // 底部条带（从上到下 = 设置里的顺序），左端写维度名
    let n = s.strips.len() as f32;
    for (k, key) in s.strips.iter().enumerate() {
        let Some(d) = f.dim(key) else { continue };
        let y = region.y + region.height - (n - k as f32) * (STRIP_H + STRIP_GAP) / scaling;
        frame.fill_rectangle(
            Point::new(rx0, y),
            Size::new(region.width, STRIP_H / scaling),
            pal::alpha(pal::dim(), 0.08),
        );
        for seg in f.overlapping(key, earliest, hi) {
            if let (Some(c), Some((x0, x1))) = (state_color(d, seg.s), span(seg)) {
                frame.fill_rectangle(Point::new(x0, y), Size::new(x1 - x0, STRIP_H / scaling), pal::alpha(c, 0.6));
            }
        }
        frame.fill_text(canvas::Text {
            content: d.name.clone(),
            position: Point::new(rx0 + 3.0 / scaling, y),
            size: iced::Pixels(9.0 / scaling),
            color: pal::txt(),
            font: crate::ui::text::ui_font(),
            ..canvas::Text::default()
        });
    }
}

/// 十字线读数：时刻 `t`（这根 K 线的开始）各维度的状态，按层分行 [(层, [(维度名, 状态名, 颜色)])]。
/// 层按文件里第一次出现的先后（L2 在前、L1 在后），底色维度排在它那一行的第一个。
#[must_use]
pub fn describe_at(symbol: &str, chart_iv: u64, t: u64) -> Option<(String, Vec<(String, Vec<StateItem>)>)> {
    let s = settings();
    if !s.on {
        return None;
    }
    let f = pick(&files_for(symbol), chart_iv, s.tf.as_deref())?;
    let mut keys: Vec<&DimDef> = f.dim(&s.band).into_iter().collect();
    keys.extend(f.dims.iter().filter(|d| d.key != s.band));
    let mut rows: Vec<(String, Vec<StateItem>)> = Vec::new();
    for layer in f.dims.iter().map(|d| d.layer.as_str()) {
        if rows.iter().any(|(l, _)| l == layer) {
            continue;
        }
        let items: Vec<StateItem> = keys
            .iter()
            .filter(|d| d.layer == layer)
            .filter_map(|d| {
                let seg = f.at(&d.key, t)?;
                let st = d.states.get(seg.s)?;
                Some((d.name.clone(), st.name.clone(), tone_color(&st.tone)))
            })
            .collect();
        if !items.is_empty() {
            rows.push((layer.to_string(), items));
        }
    }
    (!rows.is_empty()).then(|| (f.timeframe.clone(), rows))
}

/// 时刻 `t` 底色维度那一段的切换原因。
#[must_use]
pub fn reason_at(symbol: &str, chart_iv: u64, t: u64) -> Option<String> {
    let s = settings();
    let f = pick(&files_for(symbol), chart_iv, s.tf.as_deref())?;
    f.at(&s.band, t).map(|seg| seg.why.clone()).filter(|w| !w.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const J: &str = r#"{"format": 1, "symbol": "BTCUSDT", "timeframe": "15m", "tf_ms": 900000, "method": "rules_v1",
        "generated_ms": 1, "dimensions": [
          {"key": "composite", "layer": "L2", "name": "综合", "states": [
             {"key": "bull_trend", "name": "上涨趋势", "tone": "bull"}, {"key": "unknown", "name": "不明", "tone": "none"}]}],
        "segments": {"composite": [[1000, 2000, 1, ""], [2000, 5000, 0, "票 +3"], [5000, null, 1, "x"], [6000, 7000, 9, "坏下标丢掉"]]}}"#;

    #[test]
    fn 解析与按时刻查段() {
        let f = parse(J).unwrap();
        assert_eq!((f.symbol.as_str(), f.tf_ms), ("BTCUSDT", 900_000));
        assert_eq!(f.segs["composite"].len(), 3, "状态下标越界的段丢掉");
        assert!(f.at("composite", 999).is_none());
        assert_eq!(f.at("composite", 2000).map(|s| s.s), Some(0));
        assert_eq!(f.at("composite", 4999).map(|s| s.why.as_str()), Some("票 +3"));
        assert_eq!(f.at("composite", 9_999_999).map(|s| s.s), Some(1), "最后一段持续到最新");
        let o: Vec<u64> = f.overlapping("composite", 2500, 5500).iter().map(|s| s.t0).collect();
        assert_eq!(o, vec![2000, 5000]);
        assert!(f.overlapping("composite", 0, 500).is_empty());
        assert!(parse(&J.replace("\"format\": 1", "\"format\": 2")).is_err());
    }

    #[test]
    fn 按图的周期挑时间线() {
        let mk = |tf: &str, ms: u64| {
            let mut f = parse(J).unwrap();
            f.timeframe = tf.into();
            f.tf_ms = ms;
            Arc::new(f)
        };
        let v = vec![mk("15m", 900_000), mk("1h", 3_600_000), mk("4h", 14_400_000)];
        let tf = |iv: u64, want: Option<&str>| pick(&v, iv, want).map(|f| f.timeframe.clone()).unwrap();
        assert_eq!(tf(60_000, None), "15m");
        assert_eq!(tf(3_600_000, None), "1h");
        assert_eq!(tf(1_800_000, None), "1h", "一样近取不小于图周期的");
        assert_eq!(tf(86_400_000, None), "4h");
        assert_eq!(tf(60_000, Some("4h")), "4h");
        assert_eq!(tf(60_000, Some("2h")), "15m", "指定的周期没有文件就退回自动");
    }

    #[test]
    fn 设置改动() {
        let mut s = Settings::default();
        apply(&mut s, RegimeMsg::Strip("direction".into(), false));
        assert!(!s.strips.contains(&"direction".to_string()));
        apply(&mut s, RegimeMsg::Strip("htf_direction".into(), true));
        assert_eq!(s.strips.last().map(String::as_str), Some("htf_direction"));
        apply(&mut s, RegimeMsg::Band("structure".into()));
        apply(&mut s, RegimeMsg::Tf(Some("1h".into())));
        assert_eq!((s.band.as_str(), s.tf.as_deref()), ("structure", Some("1h")));
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let partial: Settings = serde_json::from_str(r#"{"on": false}"#).unwrap();
        assert_eq!((partial.on, partial.band.as_str()), (false, "composite"), "缺的字段用缺省");
    }

    #[test]
    fn 色调() {
        assert!(tone_color("none").is_none());
        assert!(tone_color("bull").is_some() && tone_color("range").is_some() && tone_color("danger").is_some());
    }
}
