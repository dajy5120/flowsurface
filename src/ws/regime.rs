//! 市场状态在 K 线上（docs/62 R2）：读状态引擎写的时间线文件，画底色带、底部状态条、切换标记，十字线读数里列出各维度。
//!
//! | 来源 | 从哪来 | 画成什么 |
//! |---|---|---|
//! | 时间线文件 | `<小文件目录>/regime/<代码>_<周期>.json`（`$WS_REGIME_DIR` 覆盖；Python：`python -m factory.regime run`） | 底色带（选一个维度）、底部条带（若干维度）、切换处的竖短线 + 新状态名 |
//!
//! - **只用当时已知的状态**：段的开始时刻是确认那根 K 线的**收盘**（= 下一根开始），所以底色从确认后的下一根开始。
//! - 颜色：多 / 空跟涨跌色（随涨跌约定与色弱开关），其余用中性色与状态色（docs/35：状态色 ≠ 涨跌色）。
//! - 显示设置按图（docs/70 C，`Settings.charts`；没单独设过的图用全局那份），存在 `<小文件目录>/cockpit/regime_overlay.json`；
//!   改在检查器「属性」页、图例齿轮或工具栏「状态」打开的弹窗里。挑哪份时间线看图上加载的时间段（docs/70 A）。
//! - 每两秒看一次目录，文件改了就重读并让各图重画（[`changed_since`]）。只画时间基的图。

use std::collections::{BTreeMap, HashMap};
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
    /// 层号 → 读数里那一行的名字（例如 `L1` → 「资金 · 持仓」或「宏观 · 跨资产」）；没给就用层号
    pub layer_names: HashMap<String, String>,
    /// 最后一根 K 线的收盘时刻（毫秒）；老文件没有，用各维度最后一段推
    pub last_ms: Option<u64>,
    /// 数据来源（`binance` / `databento_futures` ……）；老文件没有 = 币安
    pub source: String,
    /// 算这份时套用的用户参数（docs/69，`params.json` 逐层覆盖的结果）
    pub user_params: std::collections::BTreeMap<String, f64>,
    /// 实际用的全部引擎参数（检查器显示「现在用的值」）
    pub params: serde_json::Map<String, serde_json::Value>,
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
    #[serde(default)]
    layer_names: HashMap<String, String>,
    /// 最后一根 K 线的收盘时刻（毫秒）；老文件没有
    #[serde(default)]
    last_ms: Option<u64>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    user_params: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    params: serde_json::Map<String, serde_json::Value>,
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
        layer_names: r.layer_names,
        last_ms: r.last_ms,
        source: if r.source.is_empty() { "binance".into() } else { r.source },
        user_params: r.user_params.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()?))).collect(),
        params: r.params,
    })
}

impl RegimeFile {
    pub fn dim(&self, key: &str) -> Option<&DimDef> {
        self.dims.iter().find(|d| d.key == key)
    }

    /// 状态管到哪（docs/69 A）：最后确认的状态管到下一根收盘 = `last_ms + 周期`。老文件（没有 `last_ms`）不截。
    #[must_use]
    pub fn valid_until(&self) -> Option<u64> {
        self.last_ms.map(|l| l + self.tf_ms)
    }

    /// 时刻 t 所在的段（段按开始时刻升序、首尾相接）。超过 [`Self::valid_until`] 的没有（还没算，不拿旧状态顶）。
    pub fn at(&self, key: &str, t: u64) -> Option<&Seg> {
        if self.valid_until().is_some_and(|v| t >= v) {
            return None;
        }
        let v = self.segs.get(key)?;
        let k = v.partition_point(|s| s.t0 <= t);
        let s = v.get(k.checked_sub(1)?)?;
        s.t1.is_none_or(|e| t < e).then_some(s)
    }

    /// 「代码 周期」——面板里认这份文件用（同名不同来源的 symbol 里带来源前缀，不会撞）。
    #[must_use]
    pub fn key(&self) -> String {
        format!("{} {}", self.symbol, self.timeframe)
    }

    /// 全部维度最早的段开始与最晚的段开始（最后一段持续到最新，以它的开始 + 一根为右端）。
    #[must_use]
    pub fn span(&self) -> Option<(u64, u64)> {
        let lo = self.segs.values().filter_map(|v| v.first()).map(|s| s.t0).min()?;
        let hi = self.segs.values().filter_map(|v| v.last()).map(|s| s.t1.unwrap_or(s.t0 + self.tf_ms)).max()?;
        // 文件给了最后一根的收盘就用它（右端 = 它 + 1 毫秒，「看的时刻」= 最后一根收盘）。
        // 没给的老文件：「各维度最后一段开始 + 一根」——最后一段持续很久时会低估
        let hi = self.last_ms.map_or(hi, |l| l + 1);
        Some((lo, hi.max(lo + 1)))
    }

    /// 时刻 t 各维度的状态快照 {维度: 状态键}（缺的维度不出现）。
    #[must_use]
    pub fn snapshot(&self, t: u64) -> BTreeMap<String, String> {
        self.dims
            .iter()
            .filter_map(|d| self.at(&d.key, t).and_then(|s| d.states.get(s.s)).map(|st| (d.key.clone(), st.key.clone())))
            .collect()
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
        // 用户参数（docs/69 C）与时间线放同一目录，不是时间线
        if p.extension().is_none_or(|x| x != "json") || p.file_name().is_some_and(|n| n == "params.json") {
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

// ── 刷新一次（docs/62 F6）──────────────────────────────────────────────────

/// (在跑, 上一次的结果)
static ONCE: Mutex<(bool, String)> = Mutex::new((false, String::new()));

/// 子进程跑一轮 `factory.regime live --once`（live.toml 里的品种 × 周期：重算、写时间线、发 Redis）。
/// 子进程留在 Cockpit 的 cgroup 里，关窗即停；输出写运行目录下的 `regime-once.log`。
pub fn refresh_once() {
    {
        let mut g = ONCE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.0 {
            return;
        }
        *g = (true, "正在算……".into());
    }
    super::spawn_named("ws-regime-once", || {
        let log = super::paths::runtime_dir().join("regime-once.log");
        let t0 = Instant::now();
        let st = std::fs::File::create(&log).and_then(|f| {
            let e = f.try_clone()?;
            std::process::Command::new(super::paths::python())
                .args(["-m", "factory.regime", "live", "--once"])
                .current_dir(super::paths::repo_root())
                .stdout(f)
                .stderr(e)
                .status()
        });
        let at = chrono::Local::now().format("%H:%M").to_string();
        let msg = match st {
            Ok(s) if s.success() => format!("{at} 刷新完成（{:.0} 秒）", t0.elapsed().as_secs_f32()),
            Ok(s) => format!("{at} 刷新失败（退出码 {:?}），见 {}", s.code(), log.display()),
            Err(e) => format!("{at} 起不来 Python：{e}"),
        };
        *ONCE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (false, msg);
    });
}

#[must_use]
pub fn once_status() -> (bool, String) {
    ONCE.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 毫秒 → 本地 `MM-DD HH:MM`。
#[must_use]
pub fn local_hm(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// 全部时间线文件，按 (品种, 周期长短) 排。
#[must_use]
pub fn all_files() -> Vec<Arc<RegimeFile>> {
    refresh();
    let c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut v: Vec<Arc<RegimeFile>> = c.files.values().filter_map(|(_, f)| f.clone()).collect();
    v.sort_by(|a, b| a.symbol.cmp(&b.symbol).then(a.tf_ms.cmp(&b.tf_ms)));
    v
}

/// 面板选中的时间线：设置里的那份，没有就第一份。
#[must_use]
pub fn board_file() -> Option<Arc<RegimeFile>> {
    let s = settings();
    let all = all_files();
    s.board.as_ref().and_then(|k| all.iter().find(|f| &f.key() == k).cloned()).or_else(|| all.first().cloned())
}

/// 面板选中那份时间线**最新**一刻的全部维度状态（策略中心「只看当前状态可运行」用）。
#[must_use]
pub fn latest_snapshot() -> Option<(String, BTreeMap<String, String>)> {
    let f = board_file()?;
    let (_, hi) = f.span()?;
    Some((f.key(), f.snapshot(hi.saturating_sub(1))))
}

/// 文件盖住图上加载那段的比例（0–1）；不知道图的时间段 = 1。
#[must_use]
pub fn coverage(f: &RegimeFile, range: Option<(u64, u64)>) -> f64 {
    let Some((a, b)) = range.filter(|(a, b)| b > a) else { return 1.0 };
    let Some((lo, hi)) = f.span() else { return 0.0 };
    let hi = f.valid_until().map_or(hi, |v| v.max(hi));
    let ov = hi.min(b).saturating_sub(lo.max(a));
    ov as f64 / (b - a) as f64
}

/// 给一张周期为 `chart_iv`、加载了 `range` 这段的图挑时间线（docs/70 A）：
/// 先看盖不盖得住图上那段（≥ 90%；都不到就取盖得最多的），盖得住的里挑周期最接近的（一样近取不小于图周期的）。
/// 设置里指定了周期：那份盖得住一点就用它，一点都盖不住就退回自动。
#[must_use]
pub fn pick(files: &[Arc<RegimeFile>], chart_iv: u64, want: Option<&str>) -> Option<Arc<RegimeFile>> {
    pick_in(files, chart_iv, want, None)
}

#[must_use]
pub fn pick_in(files: &[Arc<RegimeFile>], chart_iv: u64, want: Option<&str>, range: Option<(u64, u64)>) -> Option<Arc<RegimeFile>> {
    if let Some(w) = want
        && let Some(f) = files.iter().find(|f| f.timeframe == w && coverage(f, range) > 0.0)
    {
        return Some(f.clone());
    }
    let best = files.iter().map(|f| coverage(f, range)).fold(0.0_f64, f64::max);
    let bar = best.min(0.9) - 1e-9;
    let iv = chart_iv.max(1) as f64;
    files
        .iter()
        .filter(|f| coverage(f, range) >= bar)
        .min_by(|a, b| {
            let da = (a.tf_ms.max(1) as f64 / iv).ln().abs();
            let db = (b.tf_ms.max(1) as f64 / iv).ln().abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal).then((a.tf_ms < chart_iv).cmp(&(b.tf_ms < chart_iv)))
        })
        .cloned()
}

// ── 每张图（docs/70）：加载的时间段 ────────────────────────────────────────────

fn ranges() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    static R: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// K 线图每帧登记：图 id → 加载的 K 线时间段（第一根开盘, 最后一根收盘）。
pub fn note_range(chart: &str, range: Option<(u64, u64)>) {
    if let (Some(r), Ok(mut g)) = (range, ranges().lock())
        && g.get(chart) != Some(&r)
    {
        g.insert(chart.to_string(), r);
    }
}

#[must_use]
pub fn range_of(chart: &str) -> Option<(u64, u64)> {
    ranges().lock().ok()?.get(chart).copied()
}

/// 这张图用哪份时间线（按图的显示设置、加载的时间段）。
#[must_use]
pub fn file_for(chart: &str, symbol: &str, chart_iv: u64) -> Option<Arc<RegimeFile>> {
    let v = view(chart);
    pick_in(&files_for(symbol), chart_iv, v.tf.as_deref(), range_of(chart))
}

/// 这个品种全部时间线的维度并集（docs/70 A：选项不因为当前那份少几个就消失）。`first` 的维度排在前面。
#[must_use]
pub fn dims_union(files: &[Arc<RegimeFile>], first: Option<&RegimeFile>) -> Vec<DimDef> {
    let mut out: Vec<DimDef> = Vec::new();
    for f in first.into_iter().chain(files.iter().map(AsRef::as_ref)) {
        for d in &f.dims {
            if !out.iter().any(|x| x.key == d.key) {
                out.push(d.clone());
            }
        }
    }
    out
}

// ── 设置 ─────────────────────────────────────────────────────────────────────

/// 一张图的显示设置（docs/70 C）：没单独设过的图用全局那份（`Settings` 里的同名字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    pub on: bool,
    /// 在这张图的指标列表 / 图例里（✕ 去掉；「指标 ▾」里再加回来）
    pub listed: bool,
    pub band: String,
    pub strips: Vec<String>,
    pub marks: bool,
    pub tf: Option<String>,
}

impl Default for View {
    fn default() -> Self {
        let s = Settings::default();
        Self { on: s.on, listed: true, band: s.band, strips: s.strips, marks: s.marks, tf: s.tf }
    }
}

impl Settings {
    fn base_view(&self) -> View {
        View { on: self.on, listed: true, band: self.band.clone(), strips: self.strips.clone(), marks: self.marks, tf: self.tf.clone() }
    }
}

/// 这张图的显示设置。
#[must_use]
pub fn view(chart: &str) -> View {
    let s = settings_cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    s.charts.get(chart).cloned().unwrap_or_else(|| s.base_view())
}

/// 改一张图的显示设置的哪一项。
#[derive(Debug, Clone)]
pub enum ViewOp {
    On(bool),
    /// 加到 / 从指标列表去掉（去掉 = 同时不显示）
    Listed(bool),
    Band(String),
    Strip(String, bool),
    Marks(bool),
    Tf(Option<String>),
}

fn apply_view(v: &mut View, op: ViewOp) {
    match op {
        ViewOp::On(x) => v.on = x,
        ViewOp::Listed(x) => {
            v.listed = x;
            v.on = x;
        }
        ViewOp::Band(k) => v.band = k,
        ViewOp::Strip(k, on) => {
            v.strips.retain(|x| *x != k);
            if on {
                v.strips.push(k);
            }
        }
        ViewOp::Marks(x) => v.marks = x,
        ViewOp::Tf(t) => v.tf = t,
    }
}

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
    /// 「市场状态」面板看哪份时间线（`RegimeFile::key`）；空 = 第一份
    pub board: Option<String>,
    /// 面板统计与时间轴的范围（天）；0 = 全部
    pub board_days: u32,
    /// 面板转移矩阵看哪个维度；空 = 底色维度
    pub board_dim: Option<String>,
    /// 跟随图表自动算（docs/69 B）：打开着的 K 线图按它的品种与周期，有新 K 线收盘 / 参数改了就重算
    pub auto: bool,
    /// 每张图自己的显示设置（docs/70 C，图 id → 设置）；没有的图用上面的全局字段。在某张图上改，全局字段也跟着变（= 以后新图的缺省）
    pub charts: BTreeMap<String, View>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            on: true,
            band: "composite".into(),
            strips: vec!["direction".into(), "trend_quality".into(), "vol_level".into(), "structure".into()],
            marks: true,
            tf: None,
            board: None,
            board_days: 90,
            board_dim: None,
            auto: true,
            charts: BTreeMap::new(),
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
    /// 面板（docs/62 F5）：选时间线 / 范围 / 转移矩阵的维度
    Board(String),
    BoardDays(u32),
    BoardDim(String),
    /// 面板时间轴上的光标（毫秒；None = 离开）：发到同页时间联动，K 线图跟着画竖线
    Hover(Option<u64>),
    /// 「刷新一次」（docs/62 F6）：子进程跑 `python -m factory.regime live --once`
    RefreshOnce,
    /// 跟随图表自动算（docs/69 B）
    Auto(bool),
    /// 「现在算」：（图 id, 图的品种键, 图的周期毫秒）
    ComputeNow(String, String, u64),
    /// 参数（docs/69 C）：改哪一层（检查器里的选择，不存盘）
    ParamScope(u8),
    /// 改一个参数（`None` = 去掉这一层的覆盖）
    ParamSet(super::regime_params::Scope, String, Option<f64>),
    /// 套预设（`regime_params::PRESETS` 的下标）
    ParamPreset(super::regime_params::Scope, usize),
    /// 改一张图的显示设置（docs/70 C）
    View(String, ViewOp),
    /// 设置弹窗：开（图 id, 品种键, 图的周期）/ 关
    Dialog(Option<DialogTarget>),
    DialogTab(u8),
}

/// 设置弹窗开给哪张图：（图 id, 品种键, 图的周期毫秒）。
pub type DialogTarget = (String, String, u64);

/// 设置弹窗（docs/70 C）：（开着的图, 页签 0 显示 / 1 计算 / 2 参数）。
fn dialog_cell() -> &'static Mutex<(Option<DialogTarget>, u8)> {
    static D: OnceLock<Mutex<(Option<DialogTarget>, u8)>> = OnceLock::new();
    D.get_or_init(|| Mutex::new((None, 0)))
}

#[must_use]
pub fn dialog_state() -> (Option<DialogTarget>, u8) {
    dialog_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

/// 检查器参数区的界面状态：（改哪一层 0 全部 / 1 市场 / 2 品种 / 3 品种 + 周期, 最近一句说明）。
fn param_ui() -> &'static Mutex<(u8, String)> {
    static P: OnceLock<Mutex<(u8, String)>> = OnceLock::new();
    P.get_or_init(|| Mutex::new((3, String::new())))
}

#[must_use]
pub fn param_ui_state() -> (u8, String) {
    param_ui().lock().map(|g| g.clone()).unwrap_or_default()
}

fn note_param(r: Result<(), String>) {
    if let Ok(mut g) = param_ui().lock() {
        g.1 = match r {
            Ok(()) => if settings().auto { "✔ 已存；开着的图会自动重算".into() } else { "✔ 已存；点「现在算」重算".into() },
            Err(e) => format!("✗ 参数没存上：{e}"),
        };
    }
}

/// 面板时间轴的光标只发联动，不改设置、不存盘。
const BOARD_CURSOR_SRC: u64 = 0x5245_4749_4d45;          // 「REGIME」：联动来源 id，与图表的指针地址不会撞

/// 改设置：存盘 + 让各图重画。
pub fn handle(m: RegimeMsg) {
    if let RegimeMsg::Hover(t) = m {
        crate::chart::linked_cursor::publish(BOARD_CURSOR_SRC, t);
        return;
    }
    match m {
        RegimeMsg::RefreshOnce => {
            refresh_once();
            return;
        }
        RegimeMsg::ComputeNow(chart, sym, iv) => {
            super::regime_auto::force(&chart, &sym, iv);
            return;
        }
        RegimeMsg::ParamScope(k) => {
            if let Ok(mut g) = param_ui().lock() {
                g.0 = k;
            }
            return;
        }
        RegimeMsg::ParamSet(sc, k, v) => {
            note_param(super::regime_params::edit(|f| super::regime_params::set(f, &sc, &k, v)));
            return;
        }
        RegimeMsg::ParamPreset(sc, i) => {
            note_param(super::regime_params::edit(|f| super::regime_params::apply_preset(f, &sc, i)));
            return;
        }
        RegimeMsg::Dialog(d) => {
            if let Ok(mut g) = dialog_cell().lock() {
                g.0 = d;
            }
            return;
        }
        RegimeMsg::DialogTab(k) => {
            if let Ok(mut g) = dialog_cell().lock() {
                g.1 = k;
            }
            return;
        }
        _ => {}
    }
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
        RegimeMsg::Board(k) => s.board = Some(k),
        RegimeMsg::BoardDays(d) => s.board_days = d,
        RegimeMsg::BoardDim(k) => s.board_dim = Some(k),
        RegimeMsg::Auto(v) => s.auto = v,
        RegimeMsg::View(chart, op) => {
            let mut v = s.charts.get(&chart).cloned().unwrap_or_else(|| s.base_view());
            apply_view(&mut v, op);
            // 也成为以后新图的缺省（「从列表去掉」只管这张图）
            s.on = if v.listed { v.on } else { s.on };
            s.band = v.band.clone();
            s.strips = v.strips.clone();
            s.marks = v.marks;
            s.tf = v.tf.clone();
            s.charts.insert(chart, v);
        }
        RegimeMsg::Hover(_)
        | RegimeMsg::RefreshOnce
        | RegimeMsg::ComputeNow(..)
        | RegimeMsg::ParamScope(_)
        | RegimeMsg::ParamSet(..)
        | RegimeMsg::ParamPreset(..)
        | RegimeMsg::Dialog(_)
        | RegimeMsg::DialogTab(_) => {}
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
/// 这张图上有没有市场状态叠加层（底色、条带、切换标记）。
#[must_use]
pub fn active(chart: &str, symbol: &str, chart_iv: u64) -> bool {
    view(chart).on && chart_iv > 0 && file_for(chart, symbol, chart_iv).is_some()
}

/// 底部状态条带占的高度（图上指标的副图摞在它上面，docs/63 I3）。
#[must_use]
pub fn bottom_reserved(chart: &str, symbol: &str, chart_iv: u64, scaling: f32) -> f32 {
    let s = view(chart);
    if !s.on || scaling <= f32::EPSILON || file_for(chart, symbol, chart_iv).is_none() {
        return 0.0;
    }
    s.strips.len() as f32 * (STRIP_H + STRIP_GAP) / scaling
}

/// `extra_bottom`：条带上面还被图上指标的副图占了多高（切换标记画在它们上面）。
#[allow(clippy::too_many_arguments)]
pub fn draw_overlay(
    frame: &mut canvas::Frame,
    region: Rectangle,
    interval_to_x: &impl Fn(u64) -> f32,
    scaling: f32,
    earliest: u64,
    latest: u64,
    chart: &str,
    symbol: &str,
    chart_iv: u64,
    extra_bottom: f32,
) {
    let s = view(chart);
    if !s.on || scaling <= f32::EPSILON || chart_iv == 0 {
        return;
    }
    let Some(f) = file_for(chart, symbol, chart_iv) else { return };
    let half = (interval_to_x(earliest + chart_iv) - interval_to_x(earliest)) / 2.0;
    let (rx0, rx1) = (region.x, region.x + region.width);
    let hi = latest.saturating_add(chart_iv);
    // 状态只画到算过的地方（docs/69 A）：最后一段是开放的，不截就会把旧状态一直涂到最新
    let upto = f.valid_until().map_or(hi, |v| v.min(hi));
    let span = |seg: &Seg| -> Option<(f32, f32)> {
        let x0 = (interval_to_x(seg.t0) - half).clamp(rx0, rx1);
        let x1 = (interval_to_x(seg.t1.unwrap_or(upto).min(upto)) - half).clamp(rx0, rx1);
        (x1 > x0).then_some((x0, x1))
    };
    // 没算到的那段：淡灰底 + 「状态停在 …」
    if upto < hi {
        let x0 = (interval_to_x(upto) - half).clamp(rx0, rx1);
        let x1 = (interval_to_x(hi) - half).clamp(rx0, rx1);
        if x0 < x1 {
            frame.fill_rectangle(Point::new(x0, region.y), Size::new(x1 - x0, region.height), pal::alpha(pal::dim(), 0.05));
            // 字写在灰底左边一点往左摆不下时贴着右端（灰底很窄时也看得全）
            let tx = (x0 + 4.0 / scaling).min(rx1 - 110.0 / scaling).max(rx0);
            frame.fill_text(canvas::Text {
                content: format!("状态停在 {}", local_hm(upto)),
                position: Point::new(tx, region.y + 22.0 / scaling),
                size: iced::Pixels(10.0 / scaling),
                color: pal::warn(),
                font: crate::ui::text::ui_font(),
                ..canvas::Text::default()
            });
        }
    }

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
            let base = region.y + region.height - s.strips.len() as f32 * (STRIP_H + STRIP_GAP) / scaling - extra_bottom;
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

/// 十字线读数：时刻 `t`（这根 K 线的开始）各维度的状态，按层分行 [(行名, [(维度名, 状态名, 颜色)])]。
/// 行名取文件的 `layer_names`，没有就是层号。
/// 层按文件里第一次出现的先后（L2 在前、L1 在后），底色维度排在它那一行的第一个。
#[must_use]
pub fn describe_at(chart: &str, symbol: &str, chart_iv: u64, t: u64) -> Option<(String, Vec<(String, Vec<StateItem>)>)> {
    let s = view(chart);
    if !s.on {
        return None;
    }
    let f = file_for(chart, symbol, chart_iv)?;
    // 超过算过的地方：一行「未算」，不显示旧状态（docs/69 A）
    if let Some(v) = f.valid_until()
        && t >= v
    {
        return Some((f.timeframe.clone(), vec![(String::new(), vec![("未算".into(), format!("（停在 {}）", local_hm(v)), Some(pal::warn()))])]));
    }
    let rows = layer_rows(&f, &s.band, t);
    (!rows.is_empty()).then(|| (f.timeframe.clone(), rows))
}

/// 按层分行（纯函数，便于测试）：层按文件里第一次出现的先后，底色维度排在它那一行第一个。
fn layer_rows(f: &RegimeFile, band: &str, t: u64) -> Vec<(String, Vec<StateItem>)> {
    let mut keys: Vec<&DimDef> = f.dim(band).into_iter().collect();
    keys.extend(f.dims.iter().filter(|d| d.key != band));
    let mut rows: Vec<(String, Vec<StateItem>)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for layer in f.dims.iter().map(|d| d.layer.as_str()) {
        if seen.contains(&layer) {
            continue;
        }
        seen.push(layer);
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
            rows.push((f.layer_names.get(layer).cloned().unwrap_or_else(|| layer.to_string()), items));
        }
    }
    rows
}

/// 时刻 `t` 底色维度那一段的切换原因。
#[must_use]
pub fn reason_at(chart: &str, symbol: &str, chart_iv: u64, t: u64) -> Option<String> {
    let s = view(chart);
    let f = file_for(chart, symbol, chart_iv)?;
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
    fn 过了最后一根收盘就不算_不拿旧状态顶() {
        let mut f = parse(J).unwrap();
        assert_eq!((f.source.as_str(), f.valid_until()), ("binance", None), "老文件没有 last_ms：不截");
        f.last_ms = Some(9_000);
        // 最后确认的状态管到下一根收盘（+ 周期 900_000）
        assert_eq!(f.valid_until(), Some(909_000));
        assert_eq!(f.at("composite", 908_999).map(|s| s.s), Some(1));
        assert!(f.at("composite", 909_000).is_none());
        assert!(f.at("composite", 9_999_999).is_none(), "三天前的状态不再画到今天");
        let g = parse(&J.replace("\"format\": 1,", "\"format\": 1, \"source\": \"databento_futures\", \"user_params\": {\"swing_k\": 1.2},")).unwrap();
        assert_eq!(g.source, "databento_futures");
        assert_eq!(g.user_params.get("swing_k"), Some(&1.2));
    }

    #[test]
    fn 按时间段挑_盖不住的同周期不用() {
        // 15m 盖 2025-10 – 2026-10；1m 只盖最近两周；图是 6 月的 1 分钟
        let mk = |tf: &str, ms: u64, t0: u64, last: u64| {
            let mut f = parse(J).unwrap();
            f.timeframe = tf.into();
            f.tf_ms = ms;
            f.segs.insert("composite".into(), vec![Seg { t0, t1: None, s: 0, why: String::new() }]);
            f.last_ms = Some(last);
            Arc::new(f)
        };
        let d = 86_400_000;
        let v = vec![mk("1m", 60_000, 300 * d, 314 * d), mk("15m", 900_000, 0, 360 * d)];
        let june = Some((150 * d, 180 * d));
        assert_eq!(pick_in(&v, 60_000, None, june).unwrap().timeframe, "15m", "1m 那份盖不住 6 月：用盖得住的 15m");
        assert_eq!(pick_in(&v, 60_000, None, Some((305 * d, 313 * d))).unwrap().timeframe, "1m", "都盖得住：周期最接近的");
        assert_eq!(pick_in(&v, 60_000, Some("1m"), june).unwrap().timeframe, "15m", "指定的周期一点都盖不住：退回自动");
        assert_eq!(pick_in(&v, 60_000, Some("15m"), Some((305 * d, 313 * d))).unwrap().timeframe, "15m");
        assert!((coverage(&v[0], june) - 0.0).abs() < 1e-9);
        assert!((coverage(&v[1], june) - 1.0).abs() < 1e-9);
        let union = dims_union(&v, None);
        assert_eq!(union.len(), v[0].dims.len());
    }

    #[test]
    fn 按图的显示设置_沿用全局_改了成为新图缺省() {
        let mut s = Settings::default();
        assert_eq!(s.base_view().band, "composite");
        apply(&mut s, RegimeMsg::View("图1".into(), ViewOp::Band("structure".into())));
        apply(&mut s, RegimeMsg::View("图1".into(), ViewOp::Strip("funding_level".into(), true)));
        assert_eq!(s.charts["图1"].band, "structure");
        assert!(s.charts["图1"].strips.contains(&"funding_level".to_string()));
        assert_eq!(s.band, "structure", "也成为以后新图的缺省");
        apply(&mut s, RegimeMsg::View("图2".into(), ViewOp::Listed(false)));
        assert!(!s.charts["图2"].listed && !s.charts["图2"].on);
        assert!(s.on, "从一张图去掉不影响别的图的缺省");
        // 老设置文件（没有 charts）照样读
        let old: Settings = serde_json::from_str(r#"{"on": true, "band": "direction", "strips": ["vol_level"], "marks": false, "tf": "15m"}"#).unwrap();
        assert_eq!((old.base_view().band.as_str(), old.base_view().tf.as_deref(), old.auto), ("direction", Some("15m"), true));
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
    fn 按层分行不重复() {
        const J: &str = r#"{"format": 1, "symbol": "BTCUSDT", "timeframe": "1h", "tf_ms": 3600000,
          "dimensions": [
            {"key": "composite", "layer": "L2", "name": "综合", "states": [{"key": "a", "name": "甲", "tone": "bull"}]},
            {"key": "crowding", "layer": "L1", "name": "拥挤度", "states": [{"key": "a", "name": "甲", "tone": "none"}]},
            {"key": "oi_flow", "layer": "L1", "name": "持仓流向", "states": [{"key": "a", "name": "甲", "tone": "none"}]},
            {"key": "transition", "layer": "L2", "name": "转换", "states": [{"key": "a", "name": "甲", "tone": "warn"}]}],
          "segments": {"composite": [[0, null, 0, ""]], "crowding": [[0, null, 0, ""]], "oi_flow": [[0, null, 0, ""]],
                       "transition": [[0, null, 0, ""]]},
          "layer_names": {"L1": "资金 · 持仓"}}"#;
        let f = parse(J).unwrap();
        let rows = layer_rows(&f, "crowding", 10);
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["L2", "资金 · 持仓"], "同一层只出一行；行名取 layer_names");
        assert_eq!(rows[0].1.len(), 2, "后面才出现的 L2 维度也并进 L2 那一行");
        assert_eq!(rows[1].1[0].0, "拥挤度", "底色维度排在它那一行第一个");
    }

    #[test]
    fn 色调() {
        assert!(tone_color("none").is_none());
        assert!(tone_color("bull").is_some() && tone_color("range").is_some() && tone_color("danger").is_some());
    }
}
