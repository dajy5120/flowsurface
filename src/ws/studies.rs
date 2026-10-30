//! 图上指标（docs/63 I3）：K 线图的叠加指标与副图指标。计算在 Rust 指标核心（`wealthspring-indicators`），
//! 与策略、状态引擎、检验框架是**同一份实现**；这里管：哪些指标挂在哪张图上、按图的 K 线增量计算、画、读数。
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `<小文件目录>/cockpit/studies.json` | 每张图的指标列表（**每张图各自一组**，docs/63 §12.2；键 = 面板设置里的 `chart_id`）、模板、收藏、最近用过、我的默认 |
//!
//! - **四类设置分开**（docs/63 §8.1 #4）：计算参数进核心的缓存键；颜色、线宽、画法、显示与否、在哪类周期上显示只在这里，改了不重算。
//! - **增量**：每张图按（品种, 周期）一份指标集；新收盘的 K 线 `push`，未收盘的最后一根 `provisional`（暂定，不污染已确认值）；
//!   只改了某些指标的参数 → 核心只重算它和依赖它的（§8.1 #8）；指标列表的结构变了或历史 K 线被换掉 → 重建。
//! - 副图画在主图下方的分区里（状态条带之上），每个副图一个纵轴：按输出的单位与坐标轴声明（固定 0–100 / 零轴居中 / 自动）。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use iced::widget::canvas::{self, Path, Stroke};
use iced::{Color, Point, Rectangle, Size};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use wealthspring_indicators::engine::{Field, IndicatorSet, Src};
use wealthspring_indicators::ind::{self, Bar};
use wealthspring_indicators::meta::{Axis, IndicatorDef, OutputKind, Unit};

use crate::ui::pal;

/// 一个输出的显示设置（不影响计算）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutStyle {
    pub visible: bool,
    /// 色板下标（`pal::series(i)`）；None = 按输出顺序自动
    pub color: Option<usize>,
    /// 画法覆盖（line / step / histogram / dots）；None = 元数据的缺省画法
    pub plot: Option<String>,
}

/// 数据源：K 线字段，或同一品种上另一个指标的某个输出（study on study）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudySrc {
    Field(String),
    Study { uid: u64, out: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Study {
    pub uid: u64,
    pub id: String,
    /// 只写改过的计算参数（其余用元数据缺省）
    pub params: BTreeMap<String, Value>,
    pub source: Option<StudySrc>,
    pub visible: bool,
    /// overlay（主图）/ sub（副图）
    pub pane: String,
    /// 按输出 key
    pub styles: BTreeMap<String, OutStyle>,
    /// 只在这些周期范围显示（毫秒；0 = 不限）。与画线的「在哪类周期上显示」同一口径
    pub min_tf_ms: u64,
    pub max_tf_ms: u64,
    /// 在哪个周期上算（毫秒；0 = 跟图）。比图大：用已收盘的高周期 K 线算，叠到图上（不前视，I6）
    pub tf_ms: u64,
    /// 指标条件提醒（I5）：只提醒，不下单
    pub alert: Option<StudyAlert>,
    /// 在哪个品种上算（品种键，`Binance Linear:ETHUSDT`；空 = 这张图的品种）。别的品种只用已收盘的 K 线，
    /// 按收盘时刻对齐到图上（不前视）；K 线由后台取（`studies_bg`）
    pub symbol: String,
    /// 对照品种（docs/63 §10 J2：相关 / 价差这类双输入统计的另一条序列 = 它已收盘的收盘价，按收盘时刻对齐）
    pub aux_symbol: String,
    /// 副图高（屏幕像素；0 = 缺省 64）。拖副图的上边沿改（docs/63 §14）
    pub sub_h: u32,
}

impl Default for Study {
    fn default() -> Self {
        Self { uid: 0, id: String::new(), params: BTreeMap::new(), source: None, visible: true, pane: "overlay".into(),
               styles: BTreeMap::new(), min_tf_ms: 0, max_tf_ms: 0, tf_ms: 0, alert: None, symbol: String::new(), aux_symbol: String::new(), sub_h: 0 }
    }
}

/// 指标条件提醒（docs/63 I5）。在**这个周期**的图收到数据时判（要开着这个品种这个周期的图，与画线提醒同）；
/// 通知中心 + 桌面通知（+ Telegram）走金融日历那一套（免打扰、出口总闸都算）。**只提醒，不下单**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StudyAlert {
    /// 输出 key
    pub out: String,
    /// cross_up / cross_down / above / below
    pub cond: String,
    pub level: f64,
    /// 输入框里的原文（输到一半的「-」「1.」不被改写）
    pub level_text: String,
    /// true = 盘中（未收盘那根一变就判）；false = 收盘确认（新收盘那根才判，不会因为盘中来回而误报）
    pub intrabar: bool,
    /// 在哪个周期判（毫秒）
    pub tf_ms: u64,
    /// 同一条两次提醒至少隔这么久（秒）
    pub cooldown_s: u64,
    /// 响一次就关
    pub once: bool,
    pub telegram: bool,
    pub armed: bool,
    pub last_fired: Option<u64>,
}

impl Default for StudyAlert {
    fn default() -> Self {
        Self { out: String::new(), cond: "cross_up".into(), level: 0.0, level_text: "0".into(), intrabar: false, tf_ms: 3_600_000,
               cooldown_s: 300, once: false, telegram: false, armed: true, last_fired: None }
    }
}

pub const ALERT_CONDS: [(&str, &str); 4] = [("cross_up", "上穿"), ("cross_down", "下穿"), ("above", "高于"), ("below", "低于")];

/// 条件判定：穿越要求上一个值在另一侧（上一个值没有就不算穿越）；高于 / 低于只看当前值（靠冷却防刷屏）。
pub fn alert_hit(cond: &str, prev: f64, cur: f64, level: f64) -> bool {
    if !cur.is_finite() {
        return false;
    }
    match cond {
        "cross_up" => prev.is_finite() && prev < level && cur >= level,
        "cross_down" => prev.is_finite() && prev > level && cur <= level,
        "above" => cur > level,
        "below" => cur < level,
        _ => false,
    }
}

/// 改提醒的哪一项。
#[derive(Debug, Clone)]
pub enum AlertOp {
    /// 有就删、没有就按第一个输出建一个
    Toggle,
    Out(String),
    Cond(String),
    Level(String),
    Intrabar(bool),
    Tf(u64),
    Cooldown(u64),
    Once(bool),
    Telegram(bool),
    Armed(bool),
}

impl Study {
    pub fn def(&self) -> Option<IndicatorDef> {
        ind::def(&self.id)
    }

    pub fn shown_on(&self, tf_ms: u64) -> bool {
        self.visible && (self.min_tf_ms == 0 || tf_ms >= self.min_tf_ms) && (self.max_tf_ms == 0 || tf_ms <= self.max_tf_ms)
    }

    /// 「EMA 20」这样的短名：指标名 + 改过的参数值。
    pub fn label(&self) -> String {
        let name = self.def().map_or_else(|| self.id.clone(), |d| d.name.to_string());
        let mut parts: Vec<String> = Vec::new();
        if let Some(d) = self.def() {
            for p in &d.inputs {
                if p.key == "source" {
                    continue;
                }
                let v = self.params.get(p.key).unwrap_or(&p.default);
                if matches!(p.kind, wealthspring_indicators::meta::ParamKind::Int | wealthspring_indicators::meta::ParamKind::Float) {
                    parts.push(fmt_num(v));
                }
            }
        }
        let mut base = if parts.is_empty() { name } else { format!("{name} {}", parts.join(" ")) };
        if !self.symbol.is_empty() {
            base = format!("{base} · {}", self.symbol.rsplit(':').next().unwrap_or(&self.symbol));
        }
        if let Some(f) = self.aux_symbol.strip_prefix("@feature:") {
            base = format!("{base} · {f}");
        } else if !self.aux_symbol.is_empty() {
            base = format!("{base} vs {}", self.aux_symbol.rsplit(':').next().unwrap_or(&self.aux_symbol));
        }
        if self.tf_ms == 0 { base } else { format!("{base} @{}", tf_label(self.tf_ms)) }
    }

    pub fn style(&self, key: &str) -> OutStyle {
        self.styles.get(key).cloned().unwrap_or(OutStyle { visible: true, color: None, plot: None })
    }
}

fn fmt_num(v: &Value) -> String {
    match v.as_f64() {
        Some(x) if x.fract() == 0.0 => format!("{x:.0}"),
        Some(x) => format!("{x}"),
        None => v.to_string(),
    }
}

/// 一张图上的指标（docs/63 §12.2）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartStudies {
    /// 这张图现在的品种（扫描时更新；后台提醒按它取 K 线）
    pub symbol: String,
    pub list: Vec<Study>,
}

/// 「存为我的默认」（docs/63 §12.1）：以后新加这个指标用这套参数与样式。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StudyDefault {
    pub params: BTreeMap<String, Value>,
    pub styles: BTreeMap<String, OutStyle>,
    pub pane: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// 图 id → 这张图的指标
    pub by_chart: BTreeMap<String, ChartStudies>,
    /// 旧版（2026-10-27 前）按品种存的一组：只读，一张图第一次出现时从这里拷它当前品种的那组（迁移）
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_symbol: BTreeMap<String, Vec<Study>>,
    pub templates: BTreeMap<String, Vec<Study>>,
    pub favorites: Vec<String>,
    pub recent: Vec<String>,
    /// 指标 id → 我的默认
    pub defaults: BTreeMap<String, StudyDefault>,
    pub next_uid: u64,
}

/// 一张图：图 id + 现在的品种（K 线、跨品种数据按品种取，指标列表与计算缓存按图）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ChartRef {
    pub id: String,
    pub symbol: String,
}

impl ChartRef {
    pub fn new(id: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self { id: id.into(), symbol: symbol.into() }
    }
}

fn store_path() -> PathBuf {
    crate::ws::paths::hot_dir().join("cockpit").join("studies.json")
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()))
}

static VERSION: AtomicU64 = AtomicU64::new(1);

/// 指标设置的版本（改了就加一；图据此清主图缓存重画）。
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

fn save(s: &Store) {
    if cfg!(test) {
        VERSION.fetch_add(1, Ordering::Relaxed);
        return;                                   // 测试不碰真的 studies.json
    }
    let p = store_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    VERSION.fetch_add(1, Ordering::Relaxed);
}

// ── 自定义指标（docs/63 I4）：仓库 indicators/user/*.toml，改了两秒内重读 ──────────────────

fn user_state() -> &'static Mutex<(Option<std::time::Instant>, Vec<(String, String)>)> {
    static U: OnceLock<Mutex<(Option<std::time::Instant>, Vec<(String, String)>)>> = OnceLock::new();
    U.get_or_init(|| Mutex::new((None, vec![])))
}

/// 重读自定义指标目录（两秒内最多一次）；有变化就让图重画。
pub fn refresh_user() {
    let mut g = user_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if g.0.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
        return;
    }
    g.0 = Some(std::time::Instant::now());
    let dir = crate::ws::paths::repo_root().join("indicators").join("user");
    let before: Vec<String> = wealthspring_indicators::user::defs().iter().map(|d| format!("{}@{}", d.id, d.version)).collect();
    let (_, errs) = wealthspring_indicators::user::load_dir(&dir);
    let after: Vec<String> = wealthspring_indicators::user::defs().iter().map(|d| format!("{}@{}", d.id, d.version)).collect();
    if before != after || g.1 != errs {
        // 自定义指标变了：清掉计算缓存（同 id 的表达式可能改了），图重画
        caches().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        VERSION.fetch_add(1, Ordering::Relaxed);
    }
    g.1 = errs;
}

/// 自定义指标文件的加载错误 [(文件, 原因)]。
pub fn user_errors() -> Vec<(String, String)> {
    user_state().lock().map(|g| g.1.clone()).unwrap_or_default()
}

/// 内置 + 自定义的全部指标元数据。
pub fn all_defs() -> Vec<IndicatorDef> {
    refresh_user();
    let mut v = ind::defs();
    v.extend(wealthspring_indicators::user::defs());
    v
}

/// 这张图的指标列表。
pub fn studies_of(chart: &str) -> Vec<Study> {
    store().lock().map(|s| s.by_chart.get(chart).map(|c| c.list.clone()).unwrap_or_default()).unwrap_or_default()
}

/// 这张图现在的品种（扫描时记下的；没有为空）。
pub fn chart_symbol(chart: &str) -> String {
    store().lock().map(|s| s.by_chart.get(chart).map(|c| c.symbol.clone()).unwrap_or_default()).unwrap_or_default()
}

/// 一组指标换成新的 uid（组内引用跟着换）。`disarm`：提醒关掉（复制来的提醒不该和原来的一起响）。
fn reuid(next_uid: &mut u64, list: &[Study], disarm: bool) -> Vec<Study> {
    let mut map = HashMap::new();
    let mut out = Vec::new();
    for x in list {
        let mut x = x.clone();
        *next_uid += 1;
        map.insert(x.uid, *next_uid);
        x.uid = *next_uid;
        if let Some(StudySrc::Study { uid, out: o }) = &x.source {
            x.source = map.get(uid).map(|n| StudySrc::Study { uid: *n, out: o.clone() });
        }
        if disarm && let Some(a) = x.alert.as_mut() {
            a.armed = false;
        }
        out.push(x);
    }
    out
}

/// 现在还在某个页面上的图（扫描时登记）：只有它们的提醒才判。
fn live_charts() -> &'static Mutex<std::collections::HashSet<String>> {
    static L: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// 还没迁移的旧图：第一次扫描时（启动、布局刚读进来）就在的图。之后新建的图从空的开始，不拿旧版那组。
fn migrate_pending() -> &'static Mutex<Option<std::collections::HashSet<String>>> {
    static M: OnceLock<Mutex<Option<std::collections::HashSet<String>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// 每帧扫全部页面的 K 线图之后调（`main.rs`）：（图 id, 现在的品种；品种还没就绪为空）。
/// 记下哪些图活着、更新品种；启动时就在、`by_chart` 里还没有的图从旧版按品种的那组迁移过来（拷一份，新 uid），
/// 这些都迁完了就丢掉旧版那份。有变化才存盘。
pub fn sync_charts(charts: &[(String, String)]) {
    {
        let mut live = live_charts().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if live.len() != charts.len() || charts.iter().any(|(id, _)| !live.contains(id)) {
            *live = charts.iter().map(|(id, _)| id.clone()).collect();
        }
    }
    let mut pending = migrate_pending().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let pending = pending.get_or_insert_with(|| charts.iter().map(|(id, _)| id.clone()).collect());
    let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if sync_into(&mut s, pending, charts) {
        save(&s);
    }
}

/// [`sync_charts`] 的本体（不碰全局状态，便于测试）：返回要不要存盘。
fn sync_into(s: &mut Store, pending: &mut std::collections::HashSet<String>, charts: &[(String, String)]) -> bool {
    let mut dirty = false;
    for (id, sym) in charts {
        if sym.is_empty() {
            continue;                                  // 品种还没就绪：等就绪了再迁移（不然拷不到旧那组）
        }
        match s.by_chart.get_mut(id) {
            Some(c) if c.symbol == *sym => {}
            Some(c) => {
                c.symbol = sym.clone();
                dirty = true;
            }
            None => {
                let old = if pending.contains(id) { s.by_symbol.get(sym).cloned().unwrap_or_default() } else { Vec::new() };
                let mut next = s.next_uid;
                let list = reuid(&mut next, &old, false);
                s.next_uid = next;
                // 同一品种多张图都迁过去时，提醒只在第一张开着（不然一条提醒响好几遍）：旧版那份里的提醒随即关掉
                if let Some(src) = s.by_symbol.get_mut(sym) {
                    for x in src.iter_mut() {
                        if let Some(a) = x.alert.as_mut() {
                            a.armed = false;
                        }
                    }
                }
                s.by_chart.insert(id.clone(), ChartStudies { symbol: sym.clone(), list });
                dirty = true;
            }
        }
        pending.remove(id);
    }
    if pending.is_empty() && !s.by_symbol.is_empty() {
        s.by_symbol.clear();                          // 启动时就在的图都迁完了：旧版那份不再用
        dirty = true;
    }
    dirty
}

/// 一张图被复制出来（两个面板撞了同一个 id，后出现的换了新 id）：把原来那张的指标拷给新的（提醒关着）。
pub fn copy_chart(from: &str, to: &str) {
    let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(src) = s.by_chart.get(from).cloned() else { return };
    let mut next = s.next_uid;
    let list = reuid(&mut next, &src.list, true);
    s.next_uid = next;
    s.by_chart.insert(to.to_string(), ChartStudies { symbol: src.symbol, list });
    save(&s);
}

pub fn snapshot() -> Store {
    store().lock().map(|s| s.clone()).unwrap_or_default()
}

/// 改指标设置的入口（检查器「指标」一节发的消息）。返回给界面的一句话（错误或空）。
#[derive(Debug, Clone)]
pub enum StudyMsg {
    Add { chart: String, id: String },
    Remove { chart: String, uid: u64 },
    Visible { chart: String, uid: u64, on: bool },
    Pane { chart: String, uid: u64, pane: String },
    /// 计算参数：文本输入（数字）或选项
    Param { chart: String, uid: u64, key: String, value: String },
    OutVisible { chart: String, uid: u64, out: String, on: bool },
    OutColor { chart: String, uid: u64, out: String },
    /// 直接选色板里的第几个（None = 按输出顺序自动）
    OutColorSet { chart: String, uid: u64, out: String, color: Option<usize> },
    /// 画法（line / step / histogram / dots；None = 元数据的缺省画法）
    OutPlot { chart: String, uid: u64, out: String, plot: Option<String> },
    Source { chart: String, uid: u64, src: Option<StudySrc> },
    Move { chart: String, uid: u64, up: bool },
    /// 复制一个（新的 uid，排在它后面）
    Duplicate { chart: String, uid: u64 },
    /// 只在这些周期显示（毫秒；0 = 不限）
    TfRange { chart: String, uid: u64, min: u64, max: u64 },
    Alert { chart: String, uid: u64, op: AlertOp },
    /// 在哪个周期上算（毫秒；0 = 跟图）
    CalcTf { chart: String, uid: u64, tf: u64 },
    /// 在哪个品种上算：输入框原文（`ETHUSDT` = 与图同一交易所；`Bybit Linear:ETHUSDT` 写全；空 = 图的品种）
    CalcSymbol { chart: String, uid: u64, text: String },
    /// 对照品种（相关 / 价差的另一条序列）：输入框原文，写法同上；空 = 清掉
    AuxSymbol { chart: String, uid: u64, text: String },
    /// 恢复默认：参数与样式回元数据缺省（数据源、位置、周期范围、提醒不动）
    Reset { chart: String, uid: u64 },
    /// 存为我的默认：以后新加这个指标用这套参数、样式与位置
    SaveDefault { chart: String, uid: u64 },
    /// 「取消」：放回打开设置时的样子
    Restore { chart: String, study: Box<Study> },
    Favorite(String),
    SaveTemplate { chart: String, name: String },
    ApplyTemplate { chart: String, name: String },
    DeleteTemplate(String),
    /// 界面状态（不存盘）
    /// 工具栏「指标 ▾」的浏览器卡片：开（哪张图）/ 关
    Menu(Option<String>),
    MenuTab(MenuTab),
    /// 设置弹窗：开（图, 指标）/ 关（= 确定）
    Edit(Option<(String, u64)>),
    EditTab(EditTab),
    /// 取消：放回打开前的样子并关掉
    EditCancel,
    /// 图例折叠（图）
    Legend { chart: String, collapsed: bool },
    /// 拖动排序（docs/63 §12.3 T3）：按住一行的把手 / 拖过别的行（挪到它的位置）/ 松开
    DragStart { chart: String, uid: u64 },
    DragOver { chart: String, uid: u64 },
    DragEnd,
    /// 鼠标进 / 出图例的一行（出只清自己：从一行移到另一行时，进新行与出旧行的先后不定）
    Hover { chart: String, uid: u64, on: bool },
    /// 按住副图的上边沿（开始改副图高）
    ResizeStart { chart: String, uid: u64 },
    /// 拖动中鼠标的位置（窗口坐标；整窗接住鼠标的那层发）/ 松开
    PointerMove { x: f32, y: f32 },
    PointerUp,
    /// 鼠标离开整块图例：收起悬停图标（在块里的行间空隙移动不收，不然移向图标时一滑出行就没了）
    HoverClear(String),
    Search(String),
    Category(Option<String>),
    TemplateName(String),
    Expand(Option<u64>),
    /// 「品种」/「对照品种」输入框里正在打的字（回车才生效）
    SymbolDraft { uid: u64, text: String },
    AuxDraft { uid: u64, text: String },
    /// 「对照序列」里搜特征 / 图表参数的字（不存盘）
    FeatQuery { uid: u64, text: String },
    /// 市场状态（docs/70 C：在指标列表 / 图例里，设置归 `ws::regime`）
    Regime(super::regime::RegimeMsg),
}

/// 正在拖的东西（整窗接住鼠标，见 `main.rs`）。起点等第一次移动时记（按下时拿不到窗口坐标）。
/// 设置弹窗的拖动归公共浮窗（`ui::float_win`，docs/64）。
#[derive(Debug, Clone, PartialEq)]
pub enum Pointer {
    /// 副图高：（图, 指标, 起点 y, 按下时的高）
    Resize { chart: String, uid: u64, start_y: Option<f32>, base_h: f32 },
}

/// 设置弹窗的宽、封顶高（docs/64：外壳是公共浮窗）。
pub const DLG_W: f32 = 560.0;
pub const DLG_H: f32 = 600.0;

/// 浏览器卡片的页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuTab {
    #[default]
    Add,
    Loaded,
}

/// 设置弹窗的页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTab {
    #[default]
    Params,
    Style,
    Visibility,
    Alert,
}

#[derive(Debug, Clone, Default)]
pub struct UiState {
    /// 浏览器卡片开着：哪张图
    pub menu: Option<String>,
    pub menu_tab: MenuTab,
    /// 设置弹窗开着：（图, 指标, 打开时的样子 → 「取消」放回）
    pub editing: Option<(String, u64, Study)>,
    pub edit_tab: EditTab,
    /// 折叠了图例的图
    pub legend_collapsed: std::collections::HashSet<String>,
    pub hover: Option<(String, u64)>,
    /// 正在拖的（图, 指标）
    pub drag: Option<(String, u64)>,
    /// 正在拖副图边沿
    pub pointer: Option<Pointer>,
    /// 这次打开浏览器以来加过的指标 id（加几次记几次）：浏览器里提示「已加」
    pub just_added: Vec<String>,
    /// 打开浏览器那一刻的「最近用过」：开着期间按它排，加了指标列表也不跳（不然鼠标下面换了一个）
    pub recent_at_open: Vec<String>,
    pub search: String,
    pub category: Option<String>,
    pub template_name: String,
    pub expanded: Option<u64>,
    pub note: String,
    /// 「品种」输入框：（哪个指标, 正在打的字）
    pub symbol_draft: Option<(u64, String)>,
    pub aux_draft: Option<(u64, String)>,
    pub feat_query: Option<(u64, String)>,
}

fn ui() -> &'static Mutex<UiState> {
    static U: OnceLock<Mutex<UiState>> = OnceLock::new();
    U.get_or_init(Default::default)
}

pub fn ui_state() -> UiState {
    ui().lock().map(|u| u.clone()).unwrap_or_default()
}

/// 改一张图的列表。空了也留着这一条（不然下次扫描会当成新图、又从旧版迁移一遍）。
fn with_list(s: &mut Store, chart: &str, f: impl FnOnce(&mut Vec<Study>)) {
    f(&mut s.by_chart.entry(chart.to_string()).or_default().list);
}

fn parse_param(def: &IndicatorDef, key: &str, text: &str) -> Result<Value, String> {
    let p = def.inputs.iter().find(|p| p.key == key).ok_or_else(|| format!("没有参数 {key}"))?;
    use wealthspring_indicators::meta::ParamKind::*;
    match p.kind {
        Int => text.trim().parse::<i64>().map(Value::from).map_err(|_| format!("{} 要整数", p.name)),
        Float => text.trim().parse::<f64>().map(Value::from).map_err(|_| format!("{} 要数字", p.name)),
        Choice | Source => Ok(Value::from(text.to_string())),
    }
}

pub fn handle(m: StudyMsg) {
    if let StudyMsg::Regime(r) = m {
        super::regime::handle(r);
        return;
    }
    let mut note = String::new();
    {
        let mut u = ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &m {
            StudyMsg::Search(t) => {
                u.search = t.clone();
                return;
            }
            StudyMsg::Category(c) => {
                u.category = c.clone();
                return;
            }
            StudyMsg::TemplateName(t) => {
                u.template_name = t.clone();
                return;
            }
            StudyMsg::Expand(x) => {
                u.expanded = *x;
                return;
            }
            StudyMsg::Menu(c) => {
                u.drag = None;
                if c.is_some() && u.menu.is_none() {
                    u.just_added.clear();
                    u.recent_at_open = store().lock().map(|s| s.recent.clone()).unwrap_or_default();
                }
                u.menu = c.clone();
                return;
            }
            StudyMsg::MenuTab(t) => {
                u.menu_tab = *t;
                return;
            }
            StudyMsg::Edit(e) => {
                u.editing = e.as_ref().and_then(|(c, uid)| {
                    let st = store().lock().ok()?.by_chart.get(c)?.list.iter().find(|x| x.uid == *uid)?.clone();
                    Some((c.clone(), *uid, st))
                });
                if e.is_some() {
                    u.edit_tab = EditTab::Params;
                }
                return;
            }
            StudyMsg::EditTab(t) => {
                u.edit_tab = *t;
                return;
            }
            StudyMsg::Legend { chart, collapsed } => {
                if *collapsed {
                    u.legend_collapsed.insert(chart.clone());
                } else {
                    u.legend_collapsed.remove(chart);
                }
                return;
            }
            StudyMsg::DragStart { chart, uid } => {
                u.drag = Some((chart.clone(), *uid));
                return;
            }
            StudyMsg::DragEnd => {
                u.drag = None;
                return;
            }
            // 没在拖、拖的不是这张图、拖到自己身上：不动
            StudyMsg::DragOver { chart, uid } if u.drag.as_ref().is_none_or(|d| d.0 != *chart || d.1 == *uid) => return,
            StudyMsg::ResizeStart { chart, uid } => {
                let base_h = store().lock().ok().and_then(|s| s.by_chart.get(chart)?.list.iter().find(|x| x.uid == *uid).map(pane_h)).unwrap_or(SUB_H);
                u.pointer = Some(Pointer::Resize { chart: chart.clone(), uid: *uid, start_y: None, base_h });
                return;
            }
            StudyMsg::PointerMove { y, .. } => {
                match u.pointer.as_mut() {
                    Some(Pointer::Resize { chart, uid, start_y: Some(sy), base_h }) => {
                        // 往上拖 = 变高；拖动中只改内存、让图重画，松开再存盘
                        let h = (*base_h - (y - *sy)).clamp(SUB_H_MIN, SUB_H_MAX).round() as u32;
                        let mut st = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(x) = st.by_chart.get_mut(chart.as_str()).and_then(|c| c.list.iter_mut().find(|x| x.uid == *uid))
                            && x.sub_h != h
                        {
                            x.sub_h = h;
                            VERSION.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Some(Pointer::Resize { start_y, .. }) => *start_y = Some(*y),
                    None => {}
                }
                return;
            }
            StudyMsg::PointerUp => {
                if matches!(u.pointer.take(), Some(Pointer::Resize { .. })) {
                    drop(u);
                    save(&store().lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                }
                return;
            }
            StudyMsg::HoverClear(chart) => {
                if u.hover.as_ref().is_some_and(|h| h.0 == *chart) {
                    u.hover = None;
                }
                return;
            }
            StudyMsg::Hover { chart, uid, on } => {
                let me = (chart.clone(), *uid);
                if *on {
                    u.hover = Some(me);
                } else if u.hover.as_ref() == Some(&me) {
                    u.hover = None;
                }
                return;
            }
            StudyMsg::SymbolDraft { uid, text } => {
                u.symbol_draft = Some((*uid, text.clone()));
                return;
            }
            StudyMsg::AuxDraft { uid, text } => {
                u.aux_draft = Some((*uid, text.clone()));
                return;
            }
            StudyMsg::FeatQuery { uid, text } => {
                u.feat_query = Some((*uid, text.clone()));
                return;
            }
            StudyMsg::CalcSymbol { .. } => u.symbol_draft = None,
            StudyMsg::AuxSymbol { .. } => u.aux_draft = None,
            StudyMsg::Remove { uid, .. } if u.editing.as_ref().is_some_and(|e| e.1 == *uid) => u.editing = None,
            _ => {}
        }
    }
    // 取消：放回打开前的样子（下面照常存盘）
    let m = match m {
        StudyMsg::EditCancel => {
            let Some((chart, _, st)) = ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).editing.take() else { return };
            StudyMsg::Restore { chart, study: Box::new(st) }
        }
        m => m,
    };
    let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match m {
        StudyMsg::Add { chart, id } => {
            if let Some(d) = ind::def(&id) {
                s.next_uid += 1;
                let uid = s.next_uid;
                let pane = d.pane.to_string();
                let pane = if pane == "new" { "sub".to_string() } else { pane };
                // 我的默认（docs/63 §12.1）：参数与样式、位置
                let mine = s.defaults.get(&id).cloned();
                let st = match mine {
                    Some(m) => Study { uid, id: id.clone(), params: m.params, styles: m.styles, pane: if m.pane.is_empty() { pane } else { m.pane }, ..Study::default() },
                    None => Study { uid, id: id.clone(), pane, ..Study::default() },
                };
                with_list(&mut s, &chart, |l| l.push(st));
                s.recent.retain(|x| x != &id);
                s.recent.insert(0, id.clone());
                s.recent.truncate(8);
                let mut u = ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                u.expanded = Some(uid);
                u.just_added.push(id);
            }
        }
        StudyMsg::Reset { chart, uid } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                x.params.clear();
                x.styles.clear();
            }
        }),
        StudyMsg::SaveDefault { chart, uid } => {
            if let Some(x) = s.by_chart.get(&chart).and_then(|c| c.list.iter().find(|x| x.uid == uid)).cloned() {
                s.defaults.insert(x.id.clone(), StudyDefault { params: x.params, styles: x.styles, pane: x.pane });
                note = "✔ 已存为我的默认：以后新加这个指标用这套参数与样式".into();
            }
        }
        StudyMsg::Restore { chart, study } => with_list(&mut s, &chart, |l| {
            if let Some(x) = l.iter_mut().find(|x| x.uid == study.uid) {
                *x = *study;
            }
        }),
        StudyMsg::Remove { chart, uid } => with_list(&mut s, &chart, |l| {
            l.retain(|x| x.uid != uid);
            for x in l.iter_mut() {
                if matches!(&x.source, Some(StudySrc::Study { uid: u, .. }) if *u == uid) {
                    x.source = None;           // 引用了被删的指标：退回缺省数据源
                }
            }
        }),
        StudyMsg::Visible { chart, uid, on } => with_list(&mut s, &chart, |l| l.iter_mut().filter(|x| x.uid == uid).for_each(|x| x.visible = on)),
        StudyMsg::Pane { chart, uid, pane } => with_list(&mut s, &chart, |l| l.iter_mut().filter(|x| x.uid == uid).for_each(|x| x.pane = pane.clone())),
        StudyMsg::Param { chart, uid, key, value } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let Some(d) = x.def() else { continue };
                match parse_param(&d, &key, &value) {
                    Ok(v) => {
                        let mut trial = x.params.clone();
                        trial.insert(key.clone(), v);
                        match ind::resolve(&d, &trial) {
                            Ok(_) => x.params = trial,
                            Err(e) => note = e,
                        }
                    }
                    Err(e) => note = e,
                }
            }
        }),
        StudyMsg::OutVisible { chart, uid, out, on } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.visible = on;
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::OutColor { chart, uid, out } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.color = Some(st.color.map_or(1, |c| (c + 1) % 8));
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::OutColorSet { chart, uid, out, color } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.color = color;
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::OutPlot { chart, uid, out, plot } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.plot = plot.clone();
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::Source { chart, uid, src } => with_list(&mut s, &chart, |l| {
            // 只能引用排在它前面的指标（依赖图无环）
            let pos = l.iter().position(|x| x.uid == uid).unwrap_or(0);
            let ok = match &src {
                Some(StudySrc::Study { uid: up, .. }) => l[..pos].iter().any(|x| x.uid == *up),
                _ => true,
            };
            if ok {
                l[pos].source = src;
            } else {
                note = "只能引用排在它前面的指标".into();
            }
        }),
        StudyMsg::Move { chart, uid, up } => with_list(&mut s, &chart, |l| {
            if let Some(i) = l.iter().position(|x| x.uid == uid) {
                let j = if up { i.checked_sub(1) } else { (i + 1 < l.len()).then_some(i + 1) };
                if let Some(j) = j {
                    let refs = |a: &Study, b: &Study| matches!(&a.source, Some(StudySrc::Study { uid: u, .. }) if *u == b.uid);
                    let (lo, hi) = (i.min(j), i.max(j));
                    if refs(&l[hi], &l[lo]) {
                        note = "它引用了上面那个指标，不能挪到它前面".into();
                    } else {
                        l.swap(i, j);
                    }
                }
            }
        }),
        StudyMsg::DragOver { chart, uid } => {
            let dragged = ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).drag.as_ref().map(|d| d.1);
            if let Some(d) = dragged {
                with_list(&mut s, &chart, |l| {
                    let (Some(from), Some(to)) = (l.iter().position(|x| x.uid == d), l.iter().position(|x| x.uid == uid)) else { return };
                    let mut trial = l.clone();
                    let x = trial.remove(from);
                    trial.insert(to, x);
                    if refs_ordered(&trial) {
                        *l = trial;
                    } else {
                        note = "数据源用了别的指标的，要排在那个指标下面（先断开引用）".into();
                    }
                });
            }
        }
        StudyMsg::Duplicate { chart, uid } => {
            s.next_uid += 1;
            let nu = s.next_uid;
            with_list(&mut s, &chart, |l| {
                if let Some(i) = l.iter().position(|x| x.uid == uid) {
                    let mut c = l[i].clone();
                    c.uid = nu;
                    l.insert(i + 1, c);
                }
            });
        }
        StudyMsg::TfRange { chart, uid, min, max } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                x.min_tf_ms = min;
                x.max_tf_ms = max;
            }
        }),
        StudyMsg::Alert { chart, uid, op } => with_list(&mut s, &chart, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                if matches!(op, AlertOp::Toggle) {
                    x.alert = match x.alert {
                        Some(_) => None,
                        None => Some(StudyAlert { out: x.def().and_then(|d| d.outputs.first().map(|o| o.key.to_string())).unwrap_or_default(),
                                                  ..StudyAlert::default() }),
                    };
                    continue;
                }
                let Some(a) = x.alert.as_mut() else { continue };
                match &op {
                    AlertOp::Toggle => {}
                    AlertOp::Out(o) => a.out = o.clone(),
                    AlertOp::Cond(c) => a.cond = c.clone(),
                    AlertOp::Level(t) => {
                        a.level_text = t.clone();
                        match t.trim().parse::<f64>() {
                            Ok(v) if v.is_finite() => a.level = v,
                            _ => note = "提醒的数值要是数字".into(),
                        }
                    }
                    AlertOp::Intrabar(b) => a.intrabar = *b,
                    AlertOp::Tf(t) => a.tf_ms = *t,
                    AlertOp::Cooldown(c) => a.cooldown_s = *c,
                    AlertOp::Once(b) => a.once = *b,
                    AlertOp::Telegram(b) => a.telegram = *b,
                    AlertOp::Armed(b) => a.armed = *b,
                }
                // 条件变了：重新从下一次数据起判（不拿旧的上一个值去比新条件）
                alert_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|k, _| k.1 != uid);
            }
        }),
        StudyMsg::CalcTf { chart, uid, tf } => with_list(&mut s, &chart, |l| {
            let refs = l.iter().any(|x| matches!(&x.source, Some(StudySrc::Study { uid: u, .. }) if *u == uid));
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                if matches!(x.source, Some(StudySrc::Study { .. })) || refs {
                    note = "引用了别的指标、或被别的指标引用的，跟着对方的周期（先断开引用）".into();
                } else {
                    x.tf_ms = tf;
                }
            }
        }),
        StudyMsg::CalcSymbol { chart, uid, text } => {
            let csym = s.by_chart.get(&chart).map(|c| c.symbol.clone()).unwrap_or_default();
            with_list(&mut s, &chart, |l| {
            let refs = l.iter().any(|x| matches!(&x.source, Some(StudySrc::Study { uid: u, .. }) if *u == uid));
            let other = resolve_symbol(&csym, &text);
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                if matches!(x.source, Some(StudySrc::Study { .. })) || refs {
                    note = "引用了别的指标、或被别的指标引用的，跟着对方的品种（先断开引用）".into();
                } else {
                    x.symbol = other.clone();
                }
            }
        })
        }
        StudyMsg::AuxSymbol { chart, uid, text } => {
            let symbol = s.by_chart.get(&chart).map(|c| c.symbol.clone()).unwrap_or_default();
            with_list(&mut s, &chart, |l| {
            // 对照品种写法同「品种」；和图同一个也照样记（自己对自己的相关没意义，但不替用户改）
            let t = text.trim();
            let key = if t.is_empty() {
                String::new()
            } else if t.starts_with('@') {
                t.to_string()                        // @feature:… 原样
            } else if t.contains(':') {
                resolve_symbol("", t)
            } else {
                format!("{}:{}", symbol.rsplit_once(':').map_or("", |x| x.0), t.to_uppercase())
            };
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                x.aux_symbol = key.clone();
            }
        })
        }
        StudyMsg::Favorite(id) => {
            if s.favorites.contains(&id) {
                s.favorites.retain(|x| x != &id);
            } else {
                s.favorites.push(id);
            }
        }
        StudyMsg::SaveTemplate { chart, name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                note = "模板要有名字".into();
            } else {
                let list = s.by_chart.get(&chart).map(|c| c.list.clone()).unwrap_or_default();
                s.templates.insert(name, list);
            }
        }
        StudyMsg::ApplyTemplate { chart, name } => {
            if let Some(t) = s.templates.get(&name).cloned() {
                // 套模板：换成新的 uid（引用关系跟着换）
                let mut next = s.next_uid;
                let out = reuid(&mut next, &t, false);
                s.next_uid = next;
                with_list(&mut s, &chart, |l| *l = out);
            }
        }
        StudyMsg::DeleteTemplate(name) => {
            s.templates.remove(&name);
        }
        StudyMsg::Search(_) | StudyMsg::Category(_) | StudyMsg::TemplateName(_) | StudyMsg::Expand(_) | StudyMsg::SymbolDraft { .. } | StudyMsg::AuxDraft { .. }
        | StudyMsg::FeatQuery { .. } | StudyMsg::Menu(_) | StudyMsg::MenuTab(_) | StudyMsg::Edit(_) | StudyMsg::EditTab(_) | StudyMsg::EditCancel
        | StudyMsg::Legend { .. } | StudyMsg::Hover { .. } | StudyMsg::HoverClear(_) | StudyMsg::DragStart { .. } | StudyMsg::DragEnd
        | StudyMsg::ResizeStart { .. } | StudyMsg::PointerMove { .. } | StudyMsg::PointerUp | StudyMsg::Regime(_) => {}
    }
    save(&s);
    ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).note = note;
}

/// 每个数据源是别的指标的，都排在那个指标后面（核心按顺序算，依赖图无环）。
fn refs_ordered(l: &[Study]) -> bool {
    l.iter().enumerate().all(|(i, x)| match &x.source {
        Some(StudySrc::Study { uid, .. }) => l[..i].iter().any(|y| y.uid == *uid),
        _ => true,
    })
}

/// 输入框里的品种 → 品种键。`ETHUSDT` 用图的交易所；写了 `交易所:代码` 的照原样；空或与图相同 → 空（图的品种）。
pub fn resolve_symbol(chart_symbol: &str, text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    let key = match t.rsplit_once(':') {
        Some((ex, code)) => format!("{}:{}", ex.trim(), code.trim().to_uppercase()),
        None => {
            let ex = chart_symbol.rsplit_once(':').map_or("", |x| x.0);
            format!("{ex}:{}", t.to_uppercase())
        }
    };
    if key == chart_symbol { String::new() } else { key }
}

/// 一路挂着、开着的提醒：哪张图（现在什么品种）、在哪个周期判、盘中?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRoute {
    pub chart: ChartRef,
    pub tf: u64,
    pub intrabar: bool,
}

/// 挂着、开着的提醒（只算现在还在某个页面上的图）——后台据此决定取哪几路 K 线（`studies_bg`）。
pub fn armed_alerts() -> Vec<AlertRoute> {
    let live = live_charts().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut v: Vec<AlertRoute> = s
        .by_chart
        .iter()
        .filter(|(id, c)| live.contains(*id) && !c.symbol.is_empty())
        .flat_map(|(id, c)| {
            c.list.iter().filter_map(|x| x.alert.as_ref().filter(|a| a.armed)).map(|a| AlertRoute {
                chart: ChartRef::new(id.clone(), c.symbol.clone()),
                tf: a.tf_ms,
                intrabar: a.intrabar,
            })
        })
        .collect();
    v.sort_by(|a, b| (&a.chart.id, a.tf, a.intrabar).cmp(&(&b.chart.id, b.tf, b.intrabar)));
    v.dedup();
    v
}

/// 提醒管理器（docs/65）的一行：一个挂了提醒的指标（开着、暂停的都算）。
#[derive(Debug, Clone)]
pub struct AlertRule {
    pub chart: String,
    pub symbol: String,
    pub uid: u64,
    pub label: String,
    /// 条件的一句话：`RSI 上穿 70 · 收盘确认 · 5m`
    pub cond: String,
    pub alert: StudyAlert,
    /// 这张图还在某个页面上（不在 = 不判）
    pub live: bool,
}

/// 全部挂了提醒的指标。
pub fn alert_rules() -> Vec<AlertRule> {
    let live = live_charts().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut v = Vec::new();
    for (id, c) in &s.by_chart {
        for x in &c.list {
            let Some(a) = x.alert.as_ref() else { continue };
            let oname = x.def().and_then(|d| d.outputs.iter().find(|o| o.key == a.out).map(|o| o.name.to_string())).unwrap_or_else(|| a.out.clone());
            let cname = ALERT_CONDS.iter().find(|k| k.0 == a.cond).map_or("", |k| k.1);
            v.push(AlertRule {
                chart: id.clone(),
                symbol: c.symbol.clone(),
                uid: x.uid,
                label: x.label(),
                cond: format!(
                    "{oname} {cname} {} · {} · {}",
                    a.level_text.trim(),
                    if a.intrabar { "盘中" } else { "收盘确认" },
                    tf_label(a.tf_ms)
                ),
                alert: a.clone(),
                live: live.contains(id),
            });
        }
    }
    v
}

/// 让图重画（跨品种的 K 线到了）。
pub fn touch() {
    VERSION.fetch_add(1, Ordering::Relaxed);
}

// ── 计算（按图增量）──────────────────────────────────────────────────────────

/// 一张图的计算结果：已确认的列 + 未收盘那根的暂定值（追加在末尾，`provisional_ts` 标出）。
#[derive(Debug, Clone, Default)]
pub struct Computed {
    pub ts: Vec<u64>,
    pub provisional_ts: Option<u64>,
    /// 每个指标（与 studies 同序）：None = 算不了（参数 / 数据源错误）；Some(各输出的列，长度 = ts.len())
    pub cols: Vec<Option<Vec<Vec<f64>>>>,
    pub errors: Vec<Option<String>>,
    /// 最近一次数据修正（已收盘的 K 线被改了 / 补了 / 删了）从哪根（开盘时刻）起重算的（docs/63 §8.1 #7）
    pub recomputed_from: Option<u64>,
}

impl Computed {
    pub fn index_of(&self, t: u64) -> Option<usize> {
        self.ts.binary_search(&t).ok()
    }
}

struct Cache {
    studies: Vec<Study>,
    set: IndicatorSet,
    node: Vec<Option<usize>>,
    errors: Vec<Option<String>>,
    first_ts: u64,
    confirmed: Vec<u64>,
    out: Arc<Computed>,
    last_fix: Option<u64>,
    /// 上一次喂进来的已收盘 K 线与 K 线之外的列（实时每跳先整段按位比一遍：全同就跳过逐根比对与重建 Bar）
    last_in: Vec<Ohlcv>,
    last_ex: Extras,
}

/// 两段输入按位相同（f64 比位，NaN 也算相同）。
fn same_input(a: &[Ohlcv], b: &[Ohlcv], ea: &Extras, eb: &Extras) -> bool {
    let fb = |x: f64, y: f64| x.to_bits() == y.to_bits();
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && fb(x.1, y.1) && fb(x.2, y.2) && fb(x.3, y.3) && fb(x.4, y.4) && fb(x.5, y.5))
        && ea.len() == eb.len()
        && ea.iter().zip(eb).all(|((fa, ca), (fb_, cb))| fa == fb_ && ca.len() == cb.len() && ca.iter().zip(cb).all(|(x, y)| fb(*x, *y)))
}

/// 已收盘部分的输入：K 线之外的列只取前 `n` 个（最后那根未收盘的不算）。
fn conf_extras(ex: &Extras, n: usize) -> Extras {
    ex.iter().map(|(f, c)| (*f, c[..n.min(c.len())].to_vec())).collect()
}

fn caches() -> &'static Mutex<HashMap<(String, u64), Cache>> {
    static C: OnceLock<Mutex<HashMap<(String, u64), Cache>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// K 线（开盘时刻毫秒, 开, 高, 低, 收, 量）。
pub type Ohlcv = (u64, f64, f64, f64, f64, f64);

/// K 线之外的列（docs/63 §10）：（字段名, 与 K 线一一对应的值）。字段名见指标核心 `ind::EXTRA_FIELDS`。
type Extras = Vec<(&'static str, Vec<f64>)>;

fn to_bar(k: &Ohlcv, tf: u64, ex: &Extras, i: usize) -> Bar {
    let mut b = Bar::new(k.0 as i64, tf as i64, k.1, k.2, k.3, k.4, k.5);
    for (f, col) in ex {
        b.set_extra(f, col.get(i).copied().unwrap_or(f64::NAN));
    }
    b
}

fn build(studies: &[Study], bars: &[Bar]) -> (IndicatorSet, Vec<Option<usize>>, Vec<Option<String>>) {
    let mut set = IndicatorSet::new();
    let mut node: Vec<Option<usize>> = Vec::new();
    let mut errors = Vec::new();
    let uid_pos: HashMap<u64, usize> = studies.iter().enumerate().map(|(i, s)| (s.uid, i)).collect();
    for b in bars {
        set.push(*b);
    }
    for s in studies {
        let src = match &s.source {
            None => None,
            Some(StudySrc::Field(f)) => Field::parse(f).map(Src::Field),
            Some(StudySrc::Study { uid, out }) => uid_pos.get(uid).and_then(|&p| {
                let up = node.get(p).copied().flatten()?;
                let k = studies[p].def()?.outputs.iter().position(|o| o.key == out)?;
                Some(Src::Node { node: up, out: k })
            }),
        };
        if matches!(s.source, Some(StudySrc::Study { .. })) && src.is_none() {
            node.push(None);
            errors.push(Some("引用的指标不在或算不了".into()));
            continue;
        }
        match set.add(&s.id, &s.params, src) {
            Ok(n) => {
                node.push(Some(n));
                errors.push(None);
            }
            Err(e) => {
                node.push(None);
                errors.push(Some(e));
            }
        }
    }
    (set, node, errors)
}

/// 结构相同（同样的指标、同样的顺序与数据源），只是计算参数可能变了。
fn same_shape(a: &[Study], b: &[Study]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.uid == y.uid && x.id == y.id && x.source == y.source)
}

fn materialize(c: &Cache, prov: Option<&Bar>) -> Computed {
    let mut ts = c.confirmed.clone();
    if let Some(p) = prov {
        ts.push(p.ts as u64);
    }
    let cols = c
        .node
        .iter()
        .zip(&c.studies)
        .map(|(n, s)| {
            let n = (*n)?;
            let nout = s.def()?.outputs.len();
            let pv = c.set.provisional_values(n);
            Some((0..nout).map(|o| {
                let mut col = c.set.column(n, o).to_vec();
                if prov.is_some() {
                    col.push(pv.map_or(f64::NAN, |v| v[o]));
                }
                col
            }).collect())
        })
        .collect();
    Computed { ts, provisional_ts: prov.map(|p| p.ts as u64), cols, errors: c.errors.clone(), recomputed_from: c.last_fix }
}

/// 两根是否相同：0 = 相同；1 = 只有 K 线之外的字段不同（持仓之类晚到了，静默重算）；2 = K 线本身不同（交易所修正，要提示）。
fn bar_diff(a: &Bar, b: &Bar) -> u8 {
    let same = |x: f64, y: f64| x.to_bits() == y.to_bits();
    if a.ts != b.ts || a.tf_ms != b.tf_ms || ![(a.open, b.open), (a.high, b.high), (a.low, b.low), (a.close, b.close), (a.volume, b.volume)].iter().all(|&(x, y)| same(x, y)) {
        return 2;
    }
    let ex = wealthspring_indicators::ind::EXTRA_FIELDS;
    u8::from(!ex.iter().all(|f| same(a.extra(f).unwrap_or(f64::NAN), b.extra(f).unwrap_or(f64::NAN))))
}

/// 每张图（品种, 周期）合并后的结果：十字线读数、错误列表读它。
fn merged() -> &'static Mutex<HashMap<(String, u64), Arc<Computed>>> {
    static M: OnceLock<Mutex<HashMap<(String, u64), Arc<Computed>>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// 把图的 K 线并成高周期 K 线（I6）：**只要已收盘、且整根都在图的数据里的**——开头不完整的那根丢掉，
/// 最后一根要等它最后一根低周期 K 线收盘了才算。`klines` 按时间升序，`now_ms` 判断最后一根是否收盘。
pub fn aggregate_closed(klines: &[Ohlcv], tf: u64, hi: u64, now_ms: u64) -> Vec<Ohlcv> {
    let mut out: Vec<Ohlcv> = Vec::new();
    let mut cur: Option<(Ohlcv, bool)> = None;      // (正在并的那根, 第一根低周期 K 线是不是对齐在开头)
    let flush = |cur: &mut Option<(Ohlcv, bool)>, last_end: u64, out: &mut Vec<Ohlcv>| {
        if let Some((b, head)) = cur.take()
            && head
            && last_end == b.0 + hi
        {
            out.push(b);
        }
    };
    let mut last_end = 0;
    for k in klines {
        if k.0 + tf > now_ms {
            break;                                   // 没收盘的低周期 K 线不进高周期
        }
        let bucket = k.0 / hi * hi;
        if cur.as_ref().is_none_or(|(b, _)| b.0 != bucket) {
            flush(&mut cur, last_end, &mut out);
            cur = Some(((bucket, k.1, k.2, k.3, k.4, k.5), k.0 == bucket));
        } else if let Some((b, _)) = cur.as_mut() {
            b.2 = b.2.max(k.2);
            b.3 = b.3.min(k.3);
            b.4 = k.4;
            b.5 += k.5;
        }
        last_end = k.0 + tf;
    }
    flush(&mut cur, last_end, &mut out);
    out
}

/// 高周期的值对到图的每根 K 线上（I6，不前视）：图上一根 K 线收盘时（开盘 + tf）能看到的，
/// 是**在那之前已经收盘**的最后一根高周期 K 线（开盘 + hi ≤ 图那根的收盘时刻）。规则在指标核心（`engine::align_asof`），
/// 跨品种 / 跨数据类型也用它。
pub fn align_closed(chart_ts: &[u64], tf: u64, hi_ts: &[u64], hi: u64, col: &[f64]) -> Vec<f64> {
    let at: Vec<i64> = chart_ts.iter().map(|t| (t + tf) as i64).collect();
    let avail: Vec<i64> = hi_ts.iter().map(|t| (t + hi) as i64).collect();
    wealthspring_indicators::engine::align_asof(&at, &avail, col, None)
}

/// 按图的 K 线算这个品种的全部指标（增量）。`klines` 按时间升序；`now_ms` 判断最后一根是否收盘。
/// 设了「在哪个周期上算」比图大的，单独并成高周期 K 线算一份，再对齐回来（I6）；
/// 设了别的品种的，用后台取来的那个品种的已收盘 K 线算，同样按收盘时刻对齐回来。
///
/// 图调这个；它同时告诉后台「这个（品种, 周期）有图在管」，提醒由图判（`studies_bg`）。
pub fn compute(chart: &ChartRef, tf: u64, klines: &[Ohlcv], now_ms: u64) -> Arc<Computed> {
    compute_ex(chart, tf, klines, None, now_ms)
}

/// 同 [`compute`]，另给与 `klines` 一一对应的主动买入量（图的 K 线带买卖量时；CVD 之类要它）。
pub fn compute_ex(chart: &ChartRef, tf: u64, klines: &[Ohlcv], buy: Option<&[f64]>, now_ms: u64) -> Arc<Computed> {
    super::studies_bg::mark_chart(&chart.id, tf, now_ms);
    compute_inner(chart, tf, klines, buy, now_ms, now_ms)
}

/// 后台（没有图在算这个品种这个周期时）：同一套计算、同一份提醒状态（`studies_bg`）。
/// `as_now` 只用来判哪根已收盘（`studies_bg::ingest` 给的，可能比真实时刻早或晚）；冷却、响的时刻用 `wall_ms`
pub fn compute_bg(chart: &ChartRef, tf: u64, klines: &[Ohlcv], buy: Option<&[f64]>, as_now: u64, wall_ms: u64) -> Arc<Computed> {
    compute_inner(chart, tf, klines, buy, as_now, wall_ms)
}

/// 一个指标要的、K 线之外的数据（元数据 `data` 里的、加上数据源用到的；docs/63 §10）。
pub fn study_needs(s: &Study) -> Vec<&'static str> {
    use wealthspring_indicators::ind::EXTRA_FIELDS;
    let mut v: Vec<&'static str> = s.def().map(|d| d.data.iter().copied().filter(|x| EXTRA_FIELDS.contains(x)).collect()).unwrap_or_default();
    let src = match &s.source {
        Some(StudySrc::Field(f)) => Some(f.clone()),
        Some(StudySrc::Study { .. }) => None,
        None => s.params.get("source").and_then(|x| x.as_str()).map(str::to_string),
    };
    if let Some(n) = src.as_deref().and_then(Field::parse).and_then(Field::needs) {
        v.push(n);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// 各列按「这根收盘时能知道的最后一个值」对齐（不前视）。持仓、比值类超过这么久没新值算缺（资金费 8 小时一次，不限）。
fn max_age(field: &str, tf: u64) -> Option<i64> {
    match field {
        "funding" => None,
        _ => Some((3 * tf).max(1_800_000) as i64),
    }
}

/// 因子池合成分的对照序列（docs/63 §13）：`pts` = （收到时刻, combo），每秒一个。
/// `mean = false`：每根收盘时能知道的最后一个值（不前视；未收盘那根 = 最新的）；`true`：这根里的均值。没点的根为 NaN。
fn combo_aux(sym: &str, e: u64, ts: &[u64], mean: bool, pts: &[(u64, f64)]) -> Result<Vec<f64>, String> {
    let code = sym.rsplit(':').next().unwrap_or(sym);
    if !code.eq_ignore_ascii_case(super::signals::COMBO_SYMBOL) {
        return Err(format!("因子池合成分只算 {}（ws-signals 跟的品种），这张图是 {code}", super::signals::COMBO_SYMBOL));
    }
    if pts.is_empty() {
        return Err("还没收到 combo（守护 ws-signals 没跑、Redis 连不上，或池子里还没有 alpha 就绪）；收到起每秒记一个，最多留 24 小时".into());
    }
    if mean {
        return Ok(ts
            .iter()
            .map(|&t| {
                let (a, b) = (pts.partition_point(|p| p.0 < t), pts.partition_point(|p| p.0 < t + e));
                if b > a { pts[a..b].iter().map(|p| p.1).sum::<f64>() / (b - a) as f64 } else { f64::NAN }
            })
            .collect());
    }
    let at: Vec<i64> = ts.iter().map(|t| (t + e) as i64).collect();
    let avail: Vec<i64> = pts.iter().map(|p| p.0 as i64).collect();
    let vals: Vec<f64> = pts.iter().map(|p| p.1).collect();
    // 一根里一个点都没有（ws-signals 断了）就算缺，不拿老值顶
    Ok(wealthspring_indicators::engine::align_asof(&at, &avail, &vals, Some(e.max(10_000) as i64)))
}

/// 一组 K 线（`ts` 开盘时刻、周期 `e`）要的 K 线之外的列，与取不到的原因（字段, 说明）。
#[allow(clippy::too_many_arguments)]
fn extras_for(sym: &str, e: u64, ts: &[u64], buy: Option<&[f64]>, needs: &std::collections::BTreeSet<&'static str>, aux: &str, now_ms: u64)
    -> (Extras, Vec<(&'static str, String)>) {
    let at: Vec<i64> = ts.iter().map(|t| (t + e) as i64).collect();
    let nan = || vec![f64::NAN; ts.len()];
    let mut ex: Extras = Vec::new();
    let mut errs: Vec<(&'static str, String)> = Vec::new();
    for &f in needs {
        let col = match f {
            "buy_volume" => match buy.filter(|b| b.len() == ts.len() && b.iter().any(|x| x.is_finite())) {
                Some(b) => b.to_vec(),
                None => {
                    errs.push((f, "这张图的 K 线没有主动买卖量（这家交易所的 K 线不给）".into()));
                    nan()
                }
            },
            "aux" if aux.starts_with("@feature:") => {
                let code = sym.rsplit(':').next().unwrap_or(sym);
                match super::studies_feat::series(aux.trim_start_matches("@feature:"), code, now_ms) {
                    Ok(pts) => {
                        let avail: Vec<i64> = pts.iter().map(|p| p.0 as i64).collect();
                        let vals: Vec<f64> = pts.iter().map(|p| p.1).collect();
                        wealthspring_indicators::engine::align_asof(&at, &avail, &vals, Some((2 * e).max(10_000) as i64))
                    }
                    Err(m) => {
                        errs.push((f, m));
                        nan()
                    }
                }
            }
            "aux" if aux.starts_with("@combo:") => match combo_aux(sym, e, ts, aux.trim_start_matches("@combo:") == "mean", &super::signals::combo_history()) {
                Ok(v) => v,
                Err(m) => {
                    errs.push((f, m));
                    nan()
                }
            },
            "aux" if aux.starts_with("@breadth:") => match breadth_aux(aux, e, &at, now_ms) {
                Ok(v) => v,
                Err(m) => {
                    errs.push((f, m));
                    nan()
                }
            },
            "aux" if aux.is_empty() => {
                errs.push((f, "还没选对照品种（展开指标 → 对照品种）".into()));
                nan()
            }
            "aux" => match super::studies_bg::closed_klines(aux, e, now_ms) {
                Ok((bars, _)) => {
                    let avail: Vec<i64> = bars.iter().map(|k| (k.0 + e) as i64).collect();
                    let close: Vec<f64> = bars.iter().map(|k| k.4).collect();
                    wealthspring_indicators::engine::align_asof(&at, &avail, &close, None)
                }
                Err(m) => {
                    errs.push((f, m));
                    nan()
                }
            },
            _ => match super::studies_bg::series(sym, f, e, now_ms) {
                Ok(pts) => {
                    let avail: Vec<i64> = pts.iter().map(|p| p.0 as i64).collect();
                    let vals: Vec<f64> = pts.iter().map(|p| p.1).collect();
                    wealthspring_indicators::engine::align_asof(&at, &avail, &vals, max_age(f, e))
                }
                Err(m) => {
                    errs.push((f, m));
                    nan()
                }
            },
        };
        ex.push((f, col));
    }
    (ex, errs)
}

/// 市场广度的对照序列（`@breadth:<条件>:<N>`）：篮子里每个品种的已收盘 K 线由后台取，条件与合并都在指标核心（`breadth`）。
fn breadth_aux(key: &str, e: u64, at: &[i64], now_ms: u64) -> Result<Vec<f64>, String> {
    use wealthspring_indicators::breadth;
    let mut it = key.trim_start_matches("@breadth:").split(':');
    let kind = it.next().unwrap_or("above_ema");
    let n: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(50);
    let mut members = Vec::new();
    let mut got = 0;
    for sym in breadth::BASKET {
        if let Ok((bars, _)) = super::studies_bg::closed_klines(&format!("Binance Linear:{sym}"), e, now_ms) {
            got += 1;
            let closes: Vec<f64> = bars.iter().map(|k| k.4).collect();
            members.push((bars.iter().map(|k| (k.0 + e) as i64).collect::<Vec<_>>(), breadth::flags(kind, n, &closes)));
        }
    }
    if got * 2 < breadth::BASKET.len() {
        return Err(if super::egress::streams_enabled() {
            format!("在取篮子的 K 线（{got} / {}）…", breadth::BASKET.len())
        } else {
            "对外连接关着（网络出口 → Cockpit 行情图），取不了篮子的 K 线".into()
        });
    }
    Ok(breadth::combine(at, &members, Some(2 * e as i64)))
}

/// 并成高周期时，主动买量跟着加（有一根没有就算没有）。`agg` 是 [`aggregate_closed`] 的结果。
fn aggregate_buy(klines: &[Ohlcv], buy: Option<&[f64]>, hi: u64, agg: &[Ohlcv]) -> Option<Vec<f64>> {
    let buy = buy.filter(|b| b.len() == klines.len())?;
    let mut sums: HashMap<u64, f64> = HashMap::new();
    for (k, b) in klines.iter().zip(buy) {
        *sums.entry(k.0 / hi * hi).or_insert(0.0) += b;
    }
    Some(agg.iter().map(|a| sums.get(&a.0).copied().unwrap_or(f64::NAN)).collect())
}

fn compute_inner(chart: &ChartRef, tf: u64, klines: &[Ohlcv], buy: Option<&[f64]>, now_ms: u64, wall_ms: u64) -> Arc<Computed> {
    refresh_user();
    let symbol = chart.symbol.as_str();
    // 计算缓存与合并结果按图（每张图各自一组指标，docs/63 §12.2）；K 线与跨品种数据按品种
    let cid = chart.id.as_str();
    let studies = studies_of(cid);
    if studies.is_empty() || klines.is_empty() || tf == 0 {
        return Arc::new(Computed::default());
    }
    // 按（品种, 实际计算周期, 对照品种）分组；本品种比图小、或不是图周期整数倍的算不了（并不出来）。
    // 别的品种不并 K 线，直接取那个周期的，所以不受这条限制
    let eff = |s: &Study| if s.tf_ms == 0 { tf } else { s.tf_ms };
    let other = |s: &Study| if s.symbol.is_empty() || s.symbol == symbol { String::new() } else { s.symbol.clone() };
    // 对照序列：广度指标按自己的条件生成（`@breadth:<条件>:<N>`），其余用用户选的对照品种
    let auxof = |s: &Study| {
        if !study_needs(s).contains(&"aux") {
            String::new()
        } else if s.id == "breadth" {
            let p = s.def().and_then(|d| ind::resolve(&d, &s.params).ok());
            let (k, n) = p.map_or(("above_ema".to_string(), 50), |p| (p.str("kind").to_string(), p.int("n")));
            format!("@breadth:{k}:{n}")
        } else if s.id == "combo" {
            // 因子池合成分（docs/63 §13）：ws-signals 推来的 combo，按 agg 取
            let agg = s.def().and_then(|d| ind::resolve(&d, &s.params).ok()).map_or("last".to_string(), |p| p.str("agg").to_string());
            format!("@combo:{agg}")
        } else {
            s.aux_symbol.clone()
        }
    };
    let mut groups: BTreeMap<(String, u64, String), Vec<usize>> = BTreeMap::new();
    let mut bad: HashMap<usize, String> = HashMap::new();
    for (i, s) in studies.iter().enumerate() {
        let (o, e) = (other(s), eff(s));
        if o.is_empty() && (e < tf || e % tf != 0) {
            bad.insert(i, format!("计算周期 {} 比图的周期 {} 小或不是它的整数倍，这张图上不算", tf_label(e), tf_label(tf)));
        } else {
            groups.entry((o, e, auxof(s))).or_default().push(i);
        }
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut data_errs: HashMap<usize, String> = HashMap::new();
    for ((o, e, a), idx) in groups {
        let sub: Vec<Study> = idx.iter().map(|&i| studies[i].clone()).collect();
        let needs: std::collections::BTreeSet<&'static str> = sub.iter().flat_map(study_needs).collect();
        let tag = if a.is_empty() { String::new() } else { format!("~{a}") };
        let mut errs: Vec<(&'static str, String)> = Vec::new();
        let (c, ch) = if !o.is_empty() {
            match super::studies_bg::closed_klines(&o, e, now_ms) {
                // 缓存键带上图（品种, 周期）：不同的图挂的那组指标不同，共用一份会来回重建
                Ok((bars, bbuy)) => {
                    let ts: Vec<u64> = bars.iter().map(|k| k.0).collect();
                    let (ex, er) = extras_for(&o, e, &ts, Some(&bbuy), &needs, &a, now_ms);
                    errs = er;
                    compute_raw((format!("{o}#{cid}@{tf}{tag}"), e), e, sub, &bars, &ex, u64::MAX)
                }
                Err(msg) => (Arc::new(Computed { cols: vec![None; sub.len()], errors: vec![Some(msg); sub.len()], ..Computed::default() }), Change::None),
            }
        } else if e == tf {
            let ts: Vec<u64> = klines.iter().map(|k| k.0).collect();
            let (ex, er) = extras_for(symbol, tf, &ts, buy, &needs, &a, now_ms);
            errs = er;
            let key = if a.is_empty() { cid.to_string() } else { format!("{cid}{tag}") };
            compute_raw((key, tf), tf, sub, klines, &ex, now_ms)
        } else {
            let agg = aggregate_closed(klines, tf, e, now_ms);
            let abuy = aggregate_buy(klines, buy, e, &agg);
            let ts: Vec<u64> = agg.iter().map(|k| k.0).collect();
            let (ex, er) = extras_for(symbol, e, &ts, abuy.as_deref(), &needs, &a, now_ms);
            errs = er;
            compute_raw((format!("{cid}@{tf}{tag}"), e), e, sub, &agg, &ex, u64::MAX)
        };
        // 取不到的数据：挂到要它的那几个指标上（参数错之类的错误优先）
        for &i in &idx {
            let mine: Vec<&str> = errs.iter().filter(|(f, _)| study_needs(&studies[i]).contains(f)).map(|(_, m)| m.as_str()).collect();
            if !mine.is_empty() {
                data_errs.insert(i, mine.join("；"));
            }
        }
        parts.push((!o.is_empty(), e, idx, c, ch));
    }
    let changed = parts.iter().any(|p| p.4 != Change::None);
    let key = (cid.to_string(), tf);
    // 拿出来而不是拷一份引用：没人再拿着时下面「只改最后一个值」能就地改、不拷整列
    let prev = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
    let native_ts: Vec<u64> = {
        // 图的时间轴：已收盘的 + 未收盘那根
        klines.iter().map(|k| k.0).collect()
    };
    let last_open = klines.last().is_some_and(|k| k.0 + tf > now_ms);
    let provisional_ts = last_open.then(|| klines.last().map(|k| k.0)).flatten();
    let mut errors: Vec<Option<String>> = vec![None; studies.len()];
    for (i, e) in &bad {
        errors[*i] = Some(e.clone());
    }
    for (_, _, idx, c, _) in &parts {
        for (k, &i) in idx.iter().enumerate() {
            errors[i] = c.errors.get(k).cloned().flatten().or_else(|| data_errs.get(&i).cloned());
        }
    }
    // 只有本周期那组的未收盘那根变了（实时每跳）、时间轴与错误都没变：把那几列的最后一个值换掉就行
    let prov_only = |p: &Computed| {
        p.cols.len() == studies.len()
            && p.ts == native_ts
            && p.provisional_ts == provisional_ts
            && p.errors == errors
            && parts.iter().all(|(cross, e, idx, c, ch)| match ch {
                Change::None => true,
                Change::Prov => !cross && *e == tf && c.ts.len() == native_ts.len() && idx.iter().all(|&i| p.cols[i].is_some()),
                Change::Full => false,
            })
    };
    let out = match prev {
        Some(p) if !changed && p.cols.len() == studies.len() && p.ts.len() == native_ts.len() => {
            merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, p.clone());
            p
        }
        Some(mut p) if prov_only(&p) => {
            let m = Arc::make_mut(&mut p);
            for (_, _, idx, c, ch) in &parts {
                if *ch != Change::Prov {
                    continue;
                }
                for (k, &i) in idx.iter().enumerate() {
                    if let (Some(dst), Some(Some(src))) = (m.cols[i].as_mut(), c.cols.get(k)) {
                        for (d, s) in dst.iter_mut().zip(src) {
                            if let (Some(d), Some(s)) = (d.last_mut(), s.last()) {
                                *d = *s;
                            }
                        }
                    }
                }
            }
            merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, p.clone());
            p
        }
        _ => {
            let mut cols: Vec<Option<Vec<Vec<f64>>>> = vec![None; studies.len()];
            for (cross, e, idx, c, _) in &parts {
                for (k, &i) in idx.iter().enumerate() {
                    cols[i] = c.cols.get(k).cloned().flatten().map(|cs| {
                        if *e == tf && !cross {
                            cs
                        } else {
                            cs.iter().map(|col| align_closed(&native_ts, tf, &c.ts, *e, col)).collect()
                        }
                    });
                }
            }
            let recomputed_from = parts.iter().filter(|p| !p.0).filter_map(|p| p.3.recomputed_from).min();
            let m = Arc::new(Computed { ts: native_ts, provisional_ts, cols, errors, recomputed_from });
            merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, m.clone());
            m
        }
    };
    if changed && studies.iter().any(|s| s.alert.as_ref().is_some_and(|a| a.armed && a.tf_ms == tf)) {
        check_alerts(chart, tf, &studies, &out, wall_ms);
    }
    out
}

/// 一组同周期的指标按 K 线增量算。`ckey` 是缓存键（高周期那组带上图的周期，免得和真的高周期图互相顶掉）。
/// `ex`：与 `klines` 一一对应的 K 线之外的列（没有的字段不出现）。
fn compute_raw(ckey: (String, u64), tf: u64, studies: Vec<Study>, klines: &[Ohlcv], ex: &Extras, now_ms: u64) -> (Arc<Computed>, Change) {
    if klines.is_empty() {
        return (Arc::new(Computed { cols: vec![None; studies.len()], errors: vec![None; studies.len()], ..Computed::default() }), Change::Full);
    }
    let last_open = klines.last().is_some_and(|k| k.0 + tf > now_ms);
    let n_conf = if last_open { klines.len() - 1 } else { klines.len() };
    let conf = &klines[..n_conf];
    let mk = |i: usize| to_bar(&klines[i], tf, ex, i);
    let prov = if last_open { Some(mk(klines.len() - 1)) } else { None };
    let mut map = caches().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = ckey;
    // 历史往左延伸（滚回去加载更早的）或指标列表结构变了 → 重建；中间的 K 线变了 → 下面从那根起重算
    let reusable = map.get(&key).is_some_and(|c| c.first_ts == conf.first().map_or(0, |k| k.0) && same_shape(&c.studies, &studies));
    if !reusable {
        let bars: Vec<Bar> = (0..n_conf).map(mk).collect();
        let (set, node, errors) = build(&studies, &bars);
        map.insert(key.clone(), Cache { studies: studies.clone(), set, node, errors, first_ts: conf.first().map_or(0, |k| k.0),
                                        confirmed: conf.iter().map(|k| k.0).collect(), out: Arc::new(Computed::default()), last_fix: None,
                                        last_in: Vec::new(), last_ex: Vec::new() });
    }
    let c = map.get_mut(&key).expect("刚放进去");
    // 只改了计算参数：核心只重算这些实例和依赖它们的（§8.1 #8）
    let mut params_changed = false;
    if c.studies != studies {
        params_changed = c.studies.iter().zip(&studies).any(|(a, b)| a.params != b.params);
        for (i, (old, new)) in c.studies.iter().zip(&studies).enumerate() {
            if old.params != new.params
                && let Some(n) = c.node[i]
            {
                c.errors[i] = c.set.set_params(n, &new.params).err();
            }
        }
        c.studies = studies.clone();
    }
    // 数据修正（§8.1 #7）：已收盘的 K 线被改了、中间补进来、或者末尾被删了 → 核心从第一处不同起重放（检查点恢复），
    // 下游依赖跟着重算。逐根比对的是核心里存着的已确认 K 线，不另存一份。只是 K 线之外的字段变了（持仓之类晚到）
    // 也照样重算，但不算「数据修正」（不提示）
    let mut fixed = false;
    let conf_ex = conf_extras(ex, n_conf);
    // 实时每跳：已收盘部分与上次逐位相同（最常见）→ 不必逐根比
    if !same_input(&c.last_in, conf, &c.last_ex, &conf_ex) {
        let old = c.set.bars();
        let m = old.len().min(n_conf);
        let diffs: Vec<u8> = (0..m).map(|i| bar_diff(&old[i], &mk(i))).collect();
        let d = diffs.iter().position(|&x| x != 0).unwrap_or(m);
        if d < old.len() {
            let bars_changed = old.len() > n_conf || diffs[d..].contains(&2);
            let fresh: Vec<Bar> = (d..n_conf).map(mk).collect();
            c.set.correct(d, fresh);
            c.confirmed.truncate(d);
            c.confirmed.extend(conf[d..].iter().map(|k| k.0));
            if bars_changed {
                c.last_fix = conf.get(d).or(conf.last()).map(|k| k.0);
            }
            fixed = true;
        }
    }
    // 新收盘的根、暂定那根刚收盘（暂定 → 没有）、数据修正、改参数：整份重新生成结果
    let full = n_conf > c.confirmed.len()
        || fixed
        || params_changed
        || c.out.ts.is_empty()
        || c.out.cols.len() != studies.len()
        || c.out.provisional_ts != prov.map(|p| p.ts as u64);
    for i in c.confirmed.len()..n_conf {
        c.set.push(mk(i));
        c.confirmed.push(klines[i].0);
    }
    c.last_in.clear();
    c.last_in.extend_from_slice(conf);
    c.last_ex = conf_ex;
    if let Some(p) = &prov {
        c.set.provisional(*p);
    }
    let change = if full {
        c.out = Arc::new(materialize(c, prov.as_ref()));
        Change::Full
    } else if prov.is_some() {
        // 只有未收盘那根在变（实时每跳）：就地改各列最后一个值。上一跳的结果已没人拿着时不拷贝，否则拷一份再改
        let pv: Vec<Option<Vec<f64>>> = c.node.iter().map(|n| n.and_then(|n| c.set.provisional_values(n).map(<[f64]>::to_vec))).collect();
        let out = Arc::make_mut(&mut c.out);
        for (col, v) in out.cols.iter_mut().zip(&pv) {
            if let Some(col) = col {
                for (o, series) in col.iter_mut().enumerate() {
                    if let Some(last) = series.last_mut() {
                        *last = v.as_ref().map_or(f64::NAN, |v| v[o]);
                    }
                }
            }
        }
        Change::Prov
    } else {
        Change::None
    };
    (c.out.clone(), change)
}

/// 一组指标的计算结果：（别的品种？, 计算周期, 在 studies 里的下标, 结果, 这次变了什么）
type Part = (bool, u64, Vec<usize>, Arc<Computed>, Change);

/// 一次计算相对上一次变了什么：没变 / 只有未收盘那根的值变了（实时每跳）/ 要整份重建。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Change {
    None,
    Prov,
    Full,
}

// ── 指标条件提醒（I5）────────────────────────────────────────────────────────

/// （图, uid）→（上次判过的已收盘那根的开盘时刻, 盘中模式的上一个值）
type AlertState = HashMap<(String, u64), (u64, f64)>;

fn alert_state() -> &'static Mutex<AlertState> {
    static A: OnceLock<Mutex<AlertState>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// 最近响过的指标提醒（新的在后）：界面显示、测试检查。
fn fired() -> &'static Mutex<Vec<(String, String)>> {
    static F: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

pub fn fired_log() -> Vec<(String, String)> {
    fired().lock().map(|f| f.clone()).unwrap_or_default()
}

/// 判这张图（品种, 周期）上挂的提醒。第一次看到某条提醒只记下当前状态、不响——打开图、加载历史不该补报。
fn check_alerts(chart: &ChartRef, tf: u64, studies: &[Study], out: &Computed, now_ms: u64) {
    let symbol = chart.symbol.as_str();
    let n_conf = out.ts.len() - usize::from(out.provisional_ts.is_some());
    let mut hits: Vec<(u64, String, String, bool, super::calendar_alerts::Origin)> = Vec::new();
    {
        let mut stt = alert_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (i, s) in studies.iter().enumerate() {
            let Some(a) = s.alert.as_ref().filter(|a| a.armed && a.tf_ms == tf) else { continue };
            let Some(Some(cols)) = out.cols.get(i) else { continue };
            let Some(k) = s.def().and_then(|d| d.outputs.iter().position(|o| o.key == a.out)) else { continue };
            let col = &cols[k];
            let key = (chart.id.clone(), s.uid);
            let (prev, cur, bar_ts) = if a.intrabar {
                let Some(&cur) = col.last() else { continue };
                let prev = stt.get(&key).map(|x| x.1);
                stt.insert(key.clone(), (0, cur));
                let Some(prev) = prev else { continue };
                (prev, cur, out.ts.last().copied().unwrap_or(0))
            } else {
                if n_conf < 2 {
                    continue;
                }
                let ts = out.ts[n_conf - 1];
                let seen = stt.get(&key).map(|x| x.0);
                stt.insert(key.clone(), (ts, f64::NAN));
                if seen.is_none_or(|t| t >= ts) {
                    continue;
                }
                (col[n_conf - 2], col[n_conf - 1], ts)
            };
            let cool = a.last_fired.is_none_or(|t| now_ms.saturating_sub(t) >= a.cooldown_s * 1000);
            if cool && alert_hit(&a.cond, prev, cur, a.level) {
                let code = symbol.rsplit(':').next().unwrap_or(symbol);
                let oname = s.def().and_then(|d| d.outputs.get(k).map(|o| o.name.to_string())).unwrap_or_default();
                let cname = ALERT_CONDS.iter().find(|c| c.0 == a.cond).map_or("", |c| c.1);
                let title = format!("指标提醒 · {code} · {}", tf_label(tf));
                let body = format!(
                    "{} · {oname} {} → {} {cname} {}（{}，K 线 {}）· 只是提醒，不会下单",
                    s.label(), fmt_val(prev), fmt_val(cur), a.level_text.trim(),
                    if a.intrabar { "盘中" } else { "收盘确认" },
                    chrono::DateTime::from_timestamp_millis(bar_ts as i64).map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default()
                );
                // 进消息中心的出处（docs/65）：哪张图、哪个指标、哪根 K 线、前值 → 现值，连同触发时这条指标的整份设置
                let origin = super::calendar_alerts::Origin {
                    level: 2,
                    occurred_at: Some((if a.intrabar { now_ms } else { bar_ts + tf }) as i64),
                    source: "指标提醒".into(),
                    symbols: vec![code.to_string()],
                    rule_ref: format!("indicator:{}:{}", chart.id, s.uid),
                    refj: serde_json::json!({
                        "chart": chart.id, "symbol": symbol, "tf_ms": tf, "bar_ts": bar_ts, "study": s.label(), "out": a.out,
                        "out_name": oname, "cond": a.cond, "level": a.level, "prev": prev, "cur": cur, "intrabar": a.intrabar,
                        "settings": serde_json::to_value(s).unwrap_or_default(),
                    }),
                };
                hits.push((s.uid, title, body, a.telegram, origin));
            }
        }
    }
    if hits.is_empty() {
        return;
    }
    let mut paused: Vec<String> = Vec::new();
    {
        let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        with_list(&mut s, &chart.id, |l| {
            for x in l.iter_mut() {
                if let Some(a) = x.alert.as_mut().filter(|_| hits.iter().any(|h| h.0 == x.uid)) {
                    a.last_fired = Some(now_ms);
                    if a.once {
                        a.armed = false;
                    }
                    // 总闸（docs/65 §4 第 9 条）：1 分钟里响太多次 = 条件设得不对，自动暂停
                    if burst((chart.id.clone(), x.uid), now_ms) {
                        a.armed = false;
                        paused.push(x.label());
                    }
                }
            }
        });
        save(&s);
    }
    for (_, title, body, tg, origin) in hits {
        deliver(title, body, tg, origin);
    }
    for name in paused {
        super::inbox::post_system(
            2,
            format!("指标提醒已自动暂停 · {}", symbol.rsplit(':').next().unwrap_or(symbol)),
            format!("「{name}」1 分钟里触发超过 {BURST_MAX} 次，已暂停这条提醒；在提醒管理器或指标设置里看条件再恢复"),
        );
    }
}

/// 同一条提醒 1 分钟内最多响几次，超过就自动暂停。
pub const BURST_MAX: usize = 10;

/// 记一次触发；1 分钟内超过 [`BURST_MAX`] 次返回 true。
fn burst(key: (String, u64), now_ms: u64) -> bool {
    type Recent = HashMap<(String, u64), std::collections::VecDeque<u64>>;
    static B: OnceLock<Mutex<Recent>> = OnceLock::new();
    let mut g = B.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let q = g.entry(key).or_default();
    q.push_back(now_ms);
    while q.front().is_some_and(|t| now_ms.saturating_sub(*t) > 60_000) {
        q.pop_front();
    }
    q.len() > BURST_MAX
}

fn tf_label(tf: u64) -> String {
    match tf {
        t if t % 86_400_000 == 0 => format!("{}d", t / 86_400_000),
        t if t % 3_600_000 == 0 => format!("{}h", t / 3_600_000),
        t if t % 60_000 == 0 => format!("{}m", t / 60_000),
        t => format!("{}s", t / 1000),
    }
}

/// 发出去：与画线提醒同一个通道。样张、隔离实例（`WS_ISOLATED`）与测试只记下来。
fn deliver(title: String, body: String, telegram: bool, origin: super::calendar_alerts::Origin) {
    {
        let mut f = fired().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.push((title.clone(), body.clone()));
        if f.len() > 20 {
            f.remove(0);
        }
    }
    if cfg!(test) || super::specimen::enabled() || std::env::var_os("WS_ISOLATED").is_some() {
        return;
    }
    super::spawn_named("ws-indicator-alert", move || {
        let r = super::calendar_alerts::open(&super::calendar_alerts::db_path())
            .and_then(|c| super::calendar_alerts::queue_with(&c, chrono::Utc::now().timestamp_millis(), "indicator", &title, &body, telegram, false, &origin));
        if let Err(e) = r {
            ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).note = format!("指标提醒没发出去：{e}");
        }
    });
}

// ── 画 ──────────────────────────────────────────────────────────────────────

const SUB_H: f32 = 64.0;
const SUB_GAP: f32 = 4.0;
/// 拖副图高时的上下限（像素）。
pub const SUB_H_MIN: f32 = 32.0;
pub const SUB_H_MAX: f32 = 600.0;

/// 这个指标的副图高（像素）。
pub fn pane_h(s: &Study) -> f32 {
    if s.sub_h == 0 { SUB_H } else { (s.sub_h as f32).clamp(SUB_H_MIN, SUB_H_MAX) }
}

/// 这张图在这个周期上显示着的副图指标（与 [`draw`] 的摆法同一份：只数有结果的）的下标。
fn shown_subs(studies: &[Study], tf: u64, comp: &Computed) -> Vec<usize> {
    studies.iter().enumerate().filter(|(i, s)| s.shown_on(tf) && s.pane == "sub" && comp.cols.get(*i).is_some_and(Option::is_some)).map(|(i, _)| i).collect()
}

/// 一条输出的颜色（图上的线、图例、设置弹窗里的色块同一个）。
pub fn color_of(s: &Study, out_idx: usize, key: &str) -> Color {
    let st = s.style(key);
    pal::series(st.color.unwrap_or(out_idx + (s.uid as usize % 5)))
}

/// 主图上：叠加指标的线 / 带 / 阶梯 / 点；主图下方：副图分区。`reserved_bottom` = 状态条带占的高度（副图摞在它上面）。
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut canvas::Frame,
    region: Rectangle,
    interval_to_x: &impl Fn(u64) -> f32,
    price_to_y: &impl Fn(f64) -> f32,
    scaling: f32,
    earliest: u64,
    latest: u64,
    chart: &str,
    tf: u64,
    comp: &Computed,
    reserved_bottom: f32,
    layer: Layer,
) {
    if comp.ts.is_empty() || scaling <= f32::EPSILON {
        return;
    }
    let studies = studies_of(chart);
    let (a, b) = visible_range(comp, tf, earliest, latest);
    if a >= b {
        return;
    }
    // 主图层画到未收盘那根之前；实时层只画最后一段（前一根 → 未收盘那根）与那根的柱 / 点
    let p = comp.provisional_ts.map(|_| comp.ts.len() - 1);
    let live = matches!(layer, Layer::LiveOverlay | Layer::LiveSub);
    let (sa, sb, spts) = match (layer, p) {
        (Layer::All, _) => (a, b, a),
        (Layer::Static, Some(p)) => (a, b.min(p), a),
        (Layer::Static, None) => (a, b, a),
        (_, Some(p)) if p >= a && p < b => (a.max(p.saturating_sub(1)), p + 1, p),
        _ => (b, b, b),
    };
    let w = 1.0 / scaling;
    let subs = shown_subs(&studies, tf, comp);
    for (i, s) in studies.iter().enumerate() {
        if !s.shown_on(tf) || s.pane == "sub" || sa >= sb || layer == Layer::LiveSub {
            continue;
        }
        let (Some(Some(cols)), Some(d)) = (comp.cols.get(i), s.def()) else { continue };
        draw_outputs(frame, &d, s, cols, &comp.ts[..], sa, sb, spts, interval_to_x, &|v| price_to_y(v), w, None, Unit::Price);
    }
    if layer == Layer::LiveOverlay {
        return;
    }
    // 副图：自下而上摞在状态条带上面；各自的高（像素）÷ scaling 换成图坐标
    let slots = sub_slots(&subs.iter().map(|&i| pane_h(&studies[i])).collect::<Vec<_>>(), 0.0);
    for (k, &i) in subs.iter().enumerate() {
        let s = &studies[i];
        let (Some(Some(cols)), Some(d)) = (comp.cols.get(i), s.def()) else { continue };
        let top = region.y + region.height - reserved_bottom + slots[k].0 / scaling;
        let h = (slots[k].1 - slots[k].0) / scaling;
        if !live {
            frame.fill_rectangle(Point::new(region.x, top), Size::new(region.width, h), pal::alpha(pal::card_bg(), 0.97)); // 基本不透明：主图的 K 线与叠加线不透过来
            frame.fill_rectangle(Point::new(region.x, top), Size::new(region.width, w), pal::alpha(pal::dim(), 0.25));
        }
        // 纵轴：第一个可见输出的单位与坐标轴声明；只画同单位的输出
        let Some((unit, axis, lo, hi)) = sub_scale(&d, s, cols, a, b) else { continue };
        let ymap = |v: f64| top + h - ((v - lo) / (hi - lo)) as f32 * h;
        if !live {
            // 参考线（RSI 30 / 70）与零轴
            let mut levels: Vec<f64> = d.outputs.iter().flat_map(|o| o.levels.clone()).collect();
            if matches!(axis, Axis::ZeroCentered) {
                levels.push(0.0);
            }
            for lv in levels {
                let y = ymap(lv);
                frame.stroke(&Path::line(Point::new(region.x, y), Point::new(region.x + region.width, y)),
                             Stroke::default().with_width(w).with_color(pal::alpha(pal::dim(), 0.35)));
            }
        }
        if sa < sb {
            draw_outputs(frame, &d, s, cols, &comp.ts[..], sa, sb, spts, interval_to_x, &ymap, w, Some(ymap(0.0f64.clamp(lo, hi))), unit);
        }
        // 左上角的名字与读数是图例（界面组件，docs/63 §12.1 T2，`studies_view::legend`），不在画布上画
    }
}

/// 图上指标画在哪一层（K 线图的主图层 / 实时层，见 `chart::Caches::live`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// 全画（没拆实时层的图）
    All,
    /// 不随未收盘那根变的：叠加线画到前一根、副图的底板 / 参考线 / 线画到前一根
    Static,
    /// 随未收盘那根变的、主图上的：叠加线的最后一段、那根的柱 / 点（宿主按副图位置裁剪，见 [`sub_panes_px`]）
    LiveOverlay,
    /// 随未收盘那根变的、副图里的：最后一段、那根的柱 / 点
    LiveSub,
}

/// 副图（自上而下）在屏幕上的 [上沿, 下沿)（像素，`bottom_px` = 主图底边减去状态条带后的位置）。
/// 与 [`draw`] 里的摆法同一份：只数有结果的副图。
pub fn sub_panes_px(chart: &str, tf: u64, comp: &Computed, bottom_px: f32) -> Vec<(f32, f32)> {
    let studies = studies_of(chart);
    sub_slots(&shown_subs(&studies, tf, comp).iter().map(|&i| pane_h(&studies[i])).collect::<Vec<_>>(), bottom_px)
}

/// 各副图（自上而下，高 `heights` 像素）的 [上沿, 下沿)（屏幕像素）：自下而上摞，每个下面留一道缝。
pub fn sub_slots(heights: &[f32], bottom_px: f32) -> Vec<(f32, f32)> {
    let mut out = vec![(0.0, 0.0); heights.len()];
    let mut cur = bottom_px;
    for (k, h) in heights.iter().enumerate().rev() {
        let top = cur - SUB_GAP - h;
        out[k] = (top, top + h);
        cur = top;
    }
    out
}

/// 可见的下标区间 [a, b)（左右各多算一根，线能连到边外）。
fn visible_range(comp: &Computed, tf: u64, earliest: u64, latest: u64) -> (usize, usize) {
    let a = comp.ts.partition_point(|t| *t < earliest.saturating_sub(tf));
    let b = comp.ts.partition_point(|t| *t <= latest.saturating_add(tf));
    (a, b)
}

/// 一个副图的纵轴：（单位, 坐标轴声明, 下沿, 上沿），按 [a, b) 里同单位的可见输出（含未收盘那根）。
fn sub_scale(d: &IndicatorDef, s: &Study, cols: &[Vec<f64>], a: usize, b: usize) -> Option<(Unit, Axis, f64, f64)> {
    let first = d.outputs.iter().find(|o| s.style(o.key).visible)?;
    let (axis, unit) = (first.axis, first.unit);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (o, od) in d.outputs.iter().enumerate() {
        if !s.style(od.key).visible || od.unit != unit {
            continue;
        }
        for v in &cols[o][a..b] {
            if v.is_finite() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
    }
    match axis {
        Axis::Fixed { lo: l, hi: hh } => {
            lo = l;
            hi = hh;
        }
        Axis::ZeroCentered => {
            let m = lo.abs().max(hi.abs());
            lo = -m;
            hi = m;
        }
        _ => {}
    }
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return None;
    }
    let pad = (hi - lo) * 0.06;
    Some((unit, axis, lo - pad, hi + pad))
}

/// 各副图当前的纵轴（按位）：实时层拿它与主图层画的时候比，变了（未收盘那根冲出了原来的范围）主图层要重画。
pub fn sub_scales(chart: &str, tf: u64, comp: &Computed, earliest: u64, latest: u64) -> Vec<(u64, u64)> {
    let (a, b) = visible_range(comp, tf, earliest, latest);
    if a >= b {
        return Vec::new();
    }
    studies_of(chart)
        .iter()
        .enumerate()
        .filter(|(_, s)| s.shown_on(tf) && s.pane == "sub")
        .filter_map(|(i, s)| {
            let (Some(Some(cols)), Some(d)) = (comp.cols.get(i), s.def()) else { return None };
            Some(sub_scale(&d, s, cols, a, b).map_or((0, 0), |(_, _, lo, hi)| (lo.to_bits(), hi.to_bits())))
        })
        .collect()
}

/// 副图一共占多高（主图底部要让出来的；与状态条带一起算）。
pub fn sub_height(chart: &str, tf: u64, scaling: f32) -> f32 {
    let px: f32 = studies_of(chart).iter().filter(|s| s.shown_on(tf) && s.pane == "sub").map(|s| pane_h(s) + SUB_GAP).sum();
    px / scaling.max(f32::EPSILON)
}

fn fmt_val(v: f64) -> String {
    let a = v.abs();
    if a >= 1e6 {
        format!("{:.3}M", v / 1e6)
    } else if a >= 1000.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.4}")
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_outputs(
    frame: &mut canvas::Frame,
    d: &IndicatorDef,
    s: &Study,
    cols: &[Vec<f64>],
    ts: &[u64],
    a: usize,
    b: usize,
    pts_from: usize,
    interval_to_x: &impl Fn(u64) -> f32,
    ymap: &dyn Fn(f64) -> f32,
    w: f32,
    zero_y: Option<f32>,
    unit: Unit,
) {
    // 带：成对的 band_upper / band_lower 之间淡淡地填一层
    let upper = d.outputs.iter().position(|o| o.plot == "band_upper");
    let lower = d.outputs.iter().position(|o| o.plot == "band_lower");
    if let (Some(u), Some(l)) = (upper, lower)
        && s.style(d.outputs[u].key).visible
        && s.style(d.outputs[l].key).visible
    {
        let c = color_of(s, u, d.outputs[u].key);
        let mut i = a;
        while i < b {
            while i < b && !(cols[u][i].is_finite() && cols[l][i].is_finite()) {
                i += 1;
            }
            let j0 = i;
            while i < b && cols[u][i].is_finite() && cols[l][i].is_finite() {
                i += 1;
            }
            if i - j0 >= 2 {
                let path = Path::new(|p| {
                    p.move_to(Point::new(interval_to_x(ts[j0]), ymap(cols[u][j0])));
                    for k in j0 + 1..i {
                        p.line_to(Point::new(interval_to_x(ts[k]), ymap(cols[u][k])));
                    }
                    for k in (j0..i).rev() {
                        p.line_to(Point::new(interval_to_x(ts[k]), ymap(cols[l][k])));
                    }
                    p.close();
                });
                frame.fill(&path, pal::alpha(c, 0.07));
            }
        }
    }
    for (o, od) in d.outputs.iter().enumerate() {
        let st = s.style(od.key);
        // 同单位的输出才共用这个纵轴（docs/63 §8.1 #3）；别的单位只在读数里
        if !st.visible || od.unit != unit {
            continue;
        }
        let c = color_of(s, o, od.key);
        let plot = st.plot.as_deref().unwrap_or(od.plot);
        let col = &cols[o];
        match plot {
            "histogram" => {
                let base = zero_y.unwrap_or_else(|| ymap(0.0));
                let bw = ((interval_to_x(ts[a.min(ts.len() - 1)] + 1) - interval_to_x(ts[a.min(ts.len() - 1)])).abs()).max(w);
                let half = if b > a + 1 { ((interval_to_x(ts[a + 1]) - interval_to_x(ts[a])) * 0.35).abs() } else { bw };
                for k in pts_from.max(a)..b {
                    let v = col[k];
                    if !v.is_finite() {
                        continue;
                    }
                    let (x, y) = (interval_to_x(ts[k]), ymap(v));
                    let cc = if v >= 0.0 { pal::alpha(c, 0.7) } else { pal::alpha(pal::down(), 0.7) };
                    frame.fill_rectangle(Point::new(x - half, y.min(base)), Size::new(2.0 * half, (y - base).abs().max(w)), cc);
                }
            }
            "dots" => {
                if od.kind == OutputKind::Events {
                    continue;                          // 方向之类的事件输出不画在价格上（读数里有）
                }
                for k in pts_from.max(a)..b {
                    if col[k].is_finite() {
                        frame.fill(&Path::circle(Point::new(interval_to_x(ts[k]), ymap(col[k])), 1.6 * w), c);
                    }
                }
            }
            _ => {
                let step = plot == "step";
                let mut path = None::<canvas::path::Builder>;
                let mut prev: Option<(f32, f32)> = None;
                let mut paths = Vec::new();
                for k in a..b {
                    let v = col[k];
                    if !v.is_finite() {
                        if let Some(p) = path.take() {
                            paths.push(p.build());
                        }
                        prev = None;
                        continue;
                    }
                    let (x, y) = (interval_to_x(ts[k]), ymap(v));
                    match (&mut path, prev) {
                        (Some(p), Some((_, py))) => {
                            if step {
                                p.line_to(Point::new(x, py));
                            }
                            p.line_to(Point::new(x, y));
                        }
                        _ => {
                            let mut p = canvas::path::Builder::new();
                            p.move_to(Point::new(x, y));
                            path = Some(p);
                        }
                    }
                    prev = Some((x, y));
                }
                if let Some(p) = path.take() {
                    paths.push(p.build());
                }
                for p in paths {
                    frame.stroke(&p, Stroke::default().with_width(1.3 * w).with_color(c));
                }
            }
        }
    }
}

/// 十字线读数：时刻 t（K 线开盘）各可见指标的值 [(文字, 颜色)]。
pub fn describe_at(chart: &str, tf: u64, t: u64) -> Vec<(String, Color)> {
    let studies = studies_of(chart);
    let Some(comp) = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(chart.to_string(), tf)).cloned() else { return vec![] };
    let Some(i) = comp.index_of(t) else { return vec![] };
    let prov = comp.provisional_ts == Some(t);
    let mut out = Vec::new();
    for (k, s) in studies.iter().enumerate() {
        if !s.shown_on(tf) {
            continue;
        }
        let (Some(Some(cols)), Some(d)) = (comp.cols.get(k), s.def()) else { continue };
        let vals: Vec<String> = d.outputs.iter().enumerate()
            .filter(|(_, o)| s.style(o.key).visible && o.kind != OutputKind::Levels)
            .filter_map(|(o, od)| cols[o].get(i).filter(|v| v.is_finite()).map(|v| if d.outputs.len() > 1 { format!("{} {}", od.name, fmt_val(*v)) } else { fmt_val(*v) }))
            .collect();
        if !vals.is_empty() {
            out.push((format!("{} {}{}", s.label(), vals.join(" "), if prov { "（暂定）" } else { "" }), color_of(s, 0, d.outputs[0].key)));
        }
    }
    out
}

// ── 图例（docs/63 §12.1 T2）：图左上角每个指标一行、副图左上角的名字行——界面组件叠在图上，画布只登记位置 ──────

/// 画布登记给图例的：读数块下沿（图例从这里往下摆）、十字线所在那根（None = 光标不在图上，取最后一根）、
/// 副图的底线（副图位置按它算）、图的周期。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LegendInfo {
    pub top: f32,
    pub at: Option<u64>,
    /// 副图摞在哪条线上面（屏幕像素：主图画布的高减去状态条带）
    pub sub_bottom: f32,
    pub tf: u64,
}

fn legend_infos() -> &'static Mutex<HashMap<String, LegendInfo>> {
    static L: OnceLock<Mutex<HashMap<String, LegendInfo>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// 画布（十字线层）每次画完调。
pub fn set_legend_info(chart: &str, info: LegendInfo) {
    let mut m = legend_infos().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if m.get(chart) != Some(&info) {
        m.insert(chart.to_string(), info);
    }
}

pub fn legend_info(chart: &str) -> Option<LegendInfo> {
    legend_infos().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(chart).copied()
}

/// 图例的一行。
#[derive(Debug, Clone)]
pub struct LegendRow {
    pub uid: u64,
    pub label: String,
    /// 名字的颜色（第一条可见输出的颜色）
    pub color: Color,
    /// 各输出的值（文字, 颜色）；隐藏的、这个周期不显示的为空
    pub values: Vec<(String, Color)>,
    /// 显示着（隐藏的、这个周期不显示的也留一行，变淡）
    pub shown: bool,
    pub visible: bool,
    /// 在副图里（显示着的副图指标，名字行摆在它那个副图左上角）
    pub sub: bool,
    /// 未收盘那根的值（暂定）
    pub provisional: bool,
    pub alert: bool,
    pub error: Option<String>,
    /// 副图高（像素）
    pub sub_h: f32,
}

/// 这张图的图例：每个指标一行，与 studies 同序。`at` = 十字线所在那根（None = 最后一根）。
pub fn legend_rows(chart: &str, tf: u64, at: Option<u64>) -> Vec<LegendRow> {
    let studies = studies_of(chart);
    let comp = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(chart.to_string(), tf)).cloned();
    let idx = comp.as_ref().and_then(|c| match at {
        Some(t) => c.index_of(t),
        None => c.ts.len().checked_sub(1),
    });
    studies
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let d = s.def();
            let shown = s.shown_on(tf);
            let color = d.as_ref().and_then(|d| d.outputs.iter().enumerate().find(|(_, o)| s.style(o.key).visible).map(|(o, od)| color_of(s, o, od.key))).unwrap_or(pal::dim());
            let mut values = Vec::new();
            let mut prov = false;
            if shown
                && let (Some(d), Some(c), Some(i)) = (d.as_ref(), comp.as_ref(), idx)
                && let Some(Some(cols)) = c.cols.get(k)
            {
                prov = c.provisional_ts.is_some_and(|p| c.ts.get(i) == Some(&p));
                for (o, od) in d.outputs.iter().enumerate() {
                    if !s.style(od.key).visible || od.kind == OutputKind::Levels {
                        continue;
                    }
                    let v = cols.get(o).and_then(|c| c.get(i)).copied().unwrap_or(f64::NAN);
                    let txt = if v.is_finite() { fmt_val(v) } else { "—".into() };
                    values.push((if d.outputs.len() > 1 { format!("{} {txt}", od.name) } else { txt }, color_of(s, o, od.key)));
                }
            }
            LegendRow {
                uid: s.uid,
                label: s.label(),
                color,
                values,
                shown,
                visible: s.visible,
                sub: shown && s.pane == "sub" && comp.as_ref().is_some_and(|c| c.cols.get(k).is_some_and(Option::is_some)),
                provisional: prov,
                alert: s.alert.as_ref().is_some_and(|a| a.armed),
                error: comp.as_ref().and_then(|c| c.errors.get(k).cloned().flatten()),
                sub_h: pane_h(s),
            }
        })
        .collect()
}

/// 双击命中（docs/63 §12.3 T3）：屏幕上 (t 所在那根, y) 附近 6 像素内最近的那条指标线 → 指标 uid。
/// `price_to_y`：价格 → 屏幕 y（主图）；副图的位置与纵轴与 [`draw`] 同一份（`sub_bottom` 同 [`LegendInfo`]）。
#[allow(clippy::too_many_arguments)]
pub fn hit(chart: &str, tf: u64, t: u64, y: f32, price_to_y: &dyn Fn(f64) -> f32, sub_bottom: f32, earliest: u64, latest: u64) -> Option<u64> {
    const TOL: f32 = 6.0;
    let comp = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(chart.to_string(), tf)).cloned()?;
    let i = comp.ts.partition_point(|x| *x <= t).checked_sub(1)?;
    let studies = studies_of(chart);
    let (a, b) = visible_range(&comp, tf, earliest, latest);
    let subs = shown_subs(&studies, tf, &comp);
    let slots = sub_slots(&subs.iter().map(|&k| pane_h(&studies[k])).collect::<Vec<_>>(), sub_bottom);
    // 落在哪个副图里：只看那一个；都不在：看主图上的叠加指标
    let in_sub = slots.iter().position(|(top, bottom)| y >= *top && y < *bottom);
    type YMap<'f> = Box<dyn Fn(f64) -> f32 + 'f>;
    let cands: Vec<(usize, YMap<'_>)> = match in_sub {
        Some(j) => {
            let k = subs[j];
            let (Some(Some(cols)), Some(d)) = (comp.cols.get(k), studies[k].def()) else { return None };
            if a >= b {
                return None;
            }
            let (_, _, lo, hi) = sub_scale(&d, &studies[k], cols, a, b)?;
            let (top, bottom) = slots[j];
            let h = bottom - top;
            vec![(k, Box::new(move |v: f64| top + h - ((v - lo) / (hi - lo)) as f32 * h))]
        }
        None => studies
            .iter()
            .enumerate()
            .filter(|(_, s)| s.shown_on(tf) && s.pane != "sub")
            .map(|(k, _)| (k, Box::new(|v: f64| price_to_y(v)) as YMap<'_>))
            .collect(),
    };
    let mut best: Option<(f32, u64)> = None;
    for (k, ymap) in cands {
        let (s, Some(Some(cols)), Some(d)) = (&studies[k], comp.cols.get(k), studies[k].def()) else { continue };
        for (o, od) in d.outputs.iter().enumerate() {
            if !s.style(od.key).visible || od.kind == OutputKind::Levels {
                continue;
            }
            let Some(v) = cols.get(o).and_then(|c| c.get(i)).copied().filter(|v| v.is_finite()) else { continue };
            let dist = (ymap(v) - y).abs();
            if dist <= TOL && best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, s.uid));
            }
        }
    }
    best.map(|(_, uid)| uid)
}

/// 这张图各指标的错误（取任一周期的计算；与 studies 同序）。
pub fn errors_for(chart: &str) -> Vec<Option<String>> {
    let map = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.iter().find(|((s, _), _)| s == chart).map(|(_, c)| c.errors.clone()).unwrap_or_default()
}

/// 这张图最近一次因数据修正而重算的起点（任一周期；开盘时刻毫秒）。
pub fn recomputed_from(chart: &str) -> Option<u64> {
    let map = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.iter().filter(|((s, _), _)| s == chart).filter_map(|(_, c)| c.recomputed_from).max()
}

/// 指标设置变了没有（图据此清主图缓存）。`seen` 是这张图上次画的版本。
pub fn changed_since(seen: &AtomicU64) -> bool {
    let v = version();
    seen.swap(v, Ordering::Relaxed) != v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的图：id 与品种同名，并登记为活着（提醒只认活着的图）。
    pub(super) fn cr(sym: &str) -> ChartRef {
        ChartRef::new(sym, sym)
    }

    pub(super) fn put(sym: &str, list: Vec<Study>) {
        store().lock().unwrap().by_chart.insert(sym.into(), ChartStudies { symbol: sym.into(), list });
        live_charts().lock().unwrap().insert(sym.into());
    }

    pub(super) fn drop_chart(sym: &str) {
        store().lock().unwrap().by_chart.remove(sym);
    }

    fn bars(n: usize) -> Vec<Ohlcv> {
        (0..n).map(|i| {
            let c = 100.0 + (i as f64 * 0.3).sin() * 5.0 + i as f64 * 0.01;
            (i as u64 * 60_000, c, c + 0.5, c - 0.5, c, 10.0)
        }).collect()
    }

    #[test]
    fn 增量与暂定() {
        let sym = "测试:STUDYA";
        {
            
            put(sym, vec![
                Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(10))].into_iter().collect(), ..Study::default() },
                Study { uid: 2, id: "sma".into(), params: [("n".to_string(), Value::from(5))].into_iter().collect(),
                        source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), pane: "sub".into(), ..Study::default() },
            ]);
        }
        let b = bars(300);
        // 最后一根没收盘（now 在它的周期里）
        let now = b[299].0 + 30_000;
        let c1 = compute(&cr(sym), 60_000, &b, now);
        assert_eq!(c1.ts.len(), 300);
        assert_eq!(c1.provisional_ts, Some(b[299].0));
        let c2 = compute(&cr(sym), 60_000, &b[..299], b[298].0 + 60_000);
        assert_eq!(c2.provisional_ts, None);
        // 与一次算完的核心结果逐位相同（前 299 根）
        let mut set = IndicatorSet::new();
        let e = set.add("ema", &[("n".to_string(), Value::from(10))].into_iter().collect(), None).unwrap();
        let m = set.add("sma", &[("n".to_string(), Value::from(5))].into_iter().collect(), Some(Src::Node { node: e, out: 0 })).unwrap();
        for k in &b[..299] {
            set.push(to_bar(k, 60_000, &Vec::new(), 0));
        }
        let got = c2.cols[1].as_ref().unwrap()[0].clone();
        assert!(got.iter().zip(set.column(m, 0)).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(!describe_at(sym, 60_000, b[200].0).is_empty());
        drop_chart(sym);
    }

    #[test]
    fn 实时每跳就地改最后一个值_与整段重算逐位相同() {
        let sym = "测试:STUDYLIVE";
        let tf = 60_000;
        let p = |k: &str, v: i64| [(k.to_string(), Value::from(v))].into_iter().collect();
        put(sym, vec![
            Study { uid: 1, id: "ema".into(), params: p("n", 10), ..Study::default() },
            Study { uid: 2, id: "sma".into(), params: p("n", 4), source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), pane: "sub".into(), ..Study::default() },
            Study { uid: 3, id: "macd".into(), pane: "sub".into(), ..Study::default() },
            // 高周期那组（5 分钟）：每跳不变，合并结果里与本周期那组摆在一起
            Study { uid: 4, id: "rsi".into(), params: p("n", 6), tf_ms: 5 * tf, ..Study::default() },
        ]);
        let mut b = bars(400);
        let buy = vec![4.0; 400];
        let now = b[399].0 + 1;
        let _ = compute_ex(&cr(sym), tf, &b, Some(&buy), now);
        let fresh = |b: &[Ohlcv]| {
            caches().lock().unwrap().retain(|k, _| !k.0.contains(sym));
            merged().lock().unwrap().retain(|k, _| k.0 != sym);
            compute_ex(&cr(sym), tf, b, Some(&buy), now)
        };
        for i in 0..30 {
            // 未收盘那根随成交跳：收盘、最高、最低在变
            let c = 100.0 + (i as f64 * 0.7).sin() * 3.0;
            b[399].4 = c;
            b[399].2 = b[399].2.max(c);
            b[399].3 = b[399].3.min(c);
            let live = compute_ex(&cr(sym), tf, &b, Some(&buy), now);
            let want = fresh(&b);
            assert_eq!(live.ts, want.ts);
            assert_eq!(live.provisional_ts, Some(b[399].0));
            for (x, y) in live.cols.iter().zip(&want.cols) {
                let (x, y) = (x.as_ref().unwrap(), y.as_ref().unwrap());
                for (cx, cy) in x.iter().zip(y) {
                    assert!(cx.iter().zip(cy).all(|(u, v)| u.to_bits() == v.to_bits()), "第 {i} 跳");
                }
            }
        }
        drop_chart(sym);
    }

    #[test]
    fn 每张图各自一组_同品种两张图互不影响() {
        let tf = 60_000;
        let p = |k: &str, v: i64| [(k.to_string(), Value::from(v))].into_iter().collect();
        put("测试图甲", vec![Study { uid: 9001, id: "ema".into(), params: p("n", 5), ..Study::default() }]);
        put("测试图乙", vec![Study { uid: 9002, id: "ema".into(), params: p("n", 30), ..Study::default() }]);
        let b = bars(200);
        let now = b[199].0 + 1;
        let a = compute(&ChartRef::new("测试图甲", "测试:同品种"), tf, &b, now);
        let c = compute(&ChartRef::new("测试图乙", "测试:同品种"), tf, &b, now);
        let (x, y) = (&a.cols[0].as_ref().unwrap()[0], &c.cols[0].as_ref().unwrap()[0]);
        assert!(x[10].is_finite() && y[10].is_nan(), "EMA 5 第 10 根有值，EMA 30 还在预热");
        assert_ne!(x[150].to_bits(), y[150].to_bits());
        // 读数也按图
        let (ra, rb) = (describe_at("测试图甲", tf, b[150].0), describe_at("测试图乙", tf, b[150].0));
        assert!(ra[0].0.contains(" 5 ") && rb[0].0.contains(" 30 "), "{ra:?} {rb:?}");
        drop_chart("测试图甲");
        drop_chart("测试图乙");
    }

    #[test]
    fn 迁移_启动时就在的图拿旧版那组_新建的图从空开始() {
        let mut s = Store::default();
        s.by_symbol.insert("B:BTC".into(), vec![
            Study { uid: 1, id: "ema".into(), alert: Some(StudyAlert { armed: true, ..StudyAlert::default() }), ..Study::default() },
            Study { uid: 2, id: "sma".into(), source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), ..Study::default() },
        ]);
        s.next_uid = 2;
        let mut pending: std::collections::HashSet<String> = ["旧图1".to_string(), "旧图2".to_string()].into_iter().collect();
        // 旧图 2 的品种还没就绪：先不迁
        assert!(sync_into(&mut s, &mut pending, &[("旧图1".into(), "B:BTC".into()), ("旧图2".into(), String::new())]));
        let l = &s.by_chart["旧图1"].list;
        assert_eq!(l.len(), 2);
        assert!(l[0].uid > 2 && matches!(&l[1].source, Some(StudySrc::Study { uid, .. }) if *uid == l[0].uid), "新 uid，引用跟着换");
        assert!(!s.by_symbol.is_empty(), "还有旧图没迁");
        // 新建的图（不在启动那批里）：空的
        sync_into(&mut s, &mut pending, &[("新图".into(), "B:BTC".into())]);
        assert!(s.by_chart["新图"].list.is_empty());
        // 旧图 2 就绪了：迁完，旧版那份丢掉
        sync_into(&mut s, &mut pending, &[("旧图2".into(), "B:BTC".into())]);
        assert_eq!(s.by_chart["旧图2"].list.len(), 2);
        assert!(s.by_chart["旧图1"].list[0].alert.as_ref().unwrap().armed, "第一张迁过去的图提醒开着");
        assert!(!s.by_chart["旧图2"].list[0].alert.as_ref().unwrap().armed, "同品种第二张：提醒关着，不重复响");
        assert_ne!(s.by_chart["旧图2"].list[0].uid, s.by_chart["旧图1"].list[0].uid);
        assert!(s.by_symbol.is_empty());
        // 换品种：指标跟着图走，记下新品种
        assert!(sync_into(&mut s, &mut pending, &[("旧图1".into(), "B:ETH".into())]));
        assert_eq!((s.by_chart["旧图1"].symbol.as_str(), s.by_chart["旧图1"].list.len()), ("B:ETH", 2));
        assert!(!sync_into(&mut s, &mut pending, &[("旧图1".into(), "B:ETH".into())]), "没变不存盘");
    }

    #[test]
    fn 复制出来的图_新uid_提醒关着() {
        put("测试图源", vec![
            Study { uid: 9101, id: "ema".into(), alert: Some(StudyAlert { armed: true, ..StudyAlert::default() }), ..Study::default() },
            Study { uid: 9102, id: "sma".into(), source: Some(StudySrc::Study { uid: 9101, out: "ema".into() }), ..Study::default() },
        ]);
        copy_chart("测试图源", "测试图复制");
        let src = studies_of("测试图源");
        let cp = studies_of("测试图复制");
        assert_eq!(cp.len(), 2);
        assert!(cp.iter().all(|x| x.uid != 9101 && x.uid != 9102));
        assert!(matches!(&cp[1].source, Some(StudySrc::Study { uid, .. }) if *uid == cp[0].uid));
        assert!(src[0].alert.as_ref().unwrap().armed && !cp[0].alert.as_ref().unwrap().armed, "复制来的提醒关着，原来的照旧");
        // 只认活着的图：复制出来的这张没登记，不在提醒路由里（原来那张在）
        assert!(armed_alerts().iter().any(|a| a.chart.id == "测试图源"));
        assert!(!armed_alerts().iter().any(|a| a.chart.id == "测试图复制"));
        drop_chart("测试图源");
        drop_chart("测试图复制");
    }

    #[test]
    fn 设置弹窗_取消放回_恢复默认_我的默认() {
        let _g = super::super::studies_bg::test_guard();             // 共用界面状态（提示那一句）：与别的走 handle 的测试串行
        put("测试图设置", vec![Study { uid: 9201, id: "rsi".into(), ..Study::default() }]);
        let c = || "测试图设置".to_string();
        handle(StudyMsg::Edit(Some((c(), 9201))));
        handle(StudyMsg::Param { chart: c(), uid: 9201, key: "n".into(), value: "21".into() });
        handle(StudyMsg::OutColorSet { chart: c(), uid: 9201, out: "rsi".into(), color: Some(3) });
        assert_eq!(studies_of("测试图设置")[0].params.get("n"), Some(&Value::from(21)), "改了立刻生效");
        handle(StudyMsg::EditCancel);
        let x = &studies_of("测试图设置")[0];
        assert!(x.params.is_empty() && x.styles.is_empty(), "取消 = 放回打开前");
        assert!(ui_state().editing.is_none());
        // 存为我的默认 → 再加一个用这套；恢复默认 → 回元数据缺省
        handle(StudyMsg::Param { chart: c(), uid: 9201, key: "n".into(), value: "9".into() });
        handle(StudyMsg::Pane { chart: c(), uid: 9201, pane: "overlay".into() });
        handle(StudyMsg::SaveDefault { chart: c(), uid: 9201 });
        handle(StudyMsg::Add { chart: c(), id: "rsi".into() });
        let l = studies_of("测试图设置");
        assert_eq!((l[1].params.get("n"), l[1].pane.as_str()), (Some(&Value::from(9)), "overlay"));
        handle(StudyMsg::Reset { chart: c(), uid: l[1].uid });
        assert!(studies_of("测试图设置")[1].params.is_empty());
        store().lock().unwrap().defaults.remove("rsi");
        drop_chart("测试图设置");
    }

    #[test]
    fn 拖动排序_引用别人的不能挪到它上面() {
        let _g = super::super::studies_bg::test_guard();             // 共用界面状态（提示那一句）：与别的走 handle 的测试串行
        put("测试图拖动", vec![
            Study { uid: 9301, id: "ema".into(), ..Study::default() },
            Study { uid: 9302, id: "rsi".into(), ..Study::default() },
            Study { uid: 9303, id: "sma".into(), source: Some(StudySrc::Study { uid: 9301, out: "ema".into() }), ..Study::default() },
        ]);
        let c = || "测试图拖动".to_string();
        let order = || studies_of("测试图拖动").iter().map(|x| x.uid).collect::<Vec<_>>();
        // 没在拖：拖过别的行不动
        handle(StudyMsg::DragOver { chart: c(), uid: 9301 });
        assert_eq!(order(), vec![9301, 9302, 9303]);
        // 把 RSI 拖到最上面
        handle(StudyMsg::DragStart { chart: c(), uid: 9302 });
        handle(StudyMsg::DragOver { chart: c(), uid: 9301 });
        assert_eq!(order(), vec![9302, 9301, 9303]);
        handle(StudyMsg::DragEnd);
        // SMA(EMA) 拖到 EMA 上面：不行
        handle(StudyMsg::DragStart { chart: c(), uid: 9303 });
        handle(StudyMsg::DragOver { chart: c(), uid: 9301 });
        assert_eq!(order(), vec![9302, 9301, 9303]);
        assert!(ui_state().note.contains("排在那个指标下面"));
        // 拖到 RSI 那里（EMA 仍在它上面之前）也不行：会排到 EMA 前面
        handle(StudyMsg::DragOver { chart: c(), uid: 9302 });
        assert_eq!(order(), vec![9302, 9301, 9303]);
        handle(StudyMsg::DragEnd);
        assert!(ui_state().drag.is_none());
        drop_chart("测试图拖动");
    }

    #[test]
    fn 双击命中_最近的那条线() {
        let tf = 60_000;
        let p = |k: &str, v: i64| [(k.to_string(), Value::from(v))].into_iter().collect();
        put("测试图命中", vec![
            Study { uid: 9401, id: "ema".into(), params: p("n", 3), ..Study::default() },
            Study { uid: 9402, id: "rsi".into(), params: p("n", 5), pane: "sub".into(), ..Study::default() },
        ]);
        let b = bars(100);
        let now = b[99].0 + 1;
        let comp = compute(&cr("测试图命中"), tf, &b, now);
        let t = b[50].0;
        let ema = comp.cols[0].as_ref().unwrap()[0][50];
        // 价格 → 屏幕 y：1 价位 = 10 像素
        let py = |v: f64| (200.0 - v) as f32 * 10.0;
        let (e, l) = (b[0].0, b[99].0);
        assert_eq!(hit("测试图命中", tf, t + 10_000, py(ema) + 4.0, &py, 900.0, e, l), Some(9401), "离线 4 像素：中");
        assert_eq!(hit("测试图命中", tf, t, py(ema) + 30.0, &py, 900.0, e, l), None, "30 像素：不中");
        // 副图：唯一一个副图在 [900 - 68, 900 - 4)；RSI 的纵轴按可见区间
        let slot = sub_slots(&[SUB_H], 900.0)[0];
        let rsi = comp.cols[1].as_ref().unwrap()[0][50];
        let d = studies_of("测试图命中")[1].def().unwrap();
        let (_, _, lo, hi) = sub_scale(&d, &studies_of("测试图命中")[1], comp.cols[1].as_ref().unwrap(), 0, 100).unwrap();
        let y = slot.0 + SUB_H - ((rsi - lo) / (hi - lo)) as f32 * SUB_H;
        assert_eq!(hit("测试图命中", tf, t, y + 2.0, &py, 900.0, e, l), Some(9402));
        drop_chart("测试图命中");
    }

    #[test]
    fn 因子池合成分_按收盘对齐_均值_只算btc() {
        let m = 60_000u64;
        // 第 0 根 [0, 60s) 里 10s / 50s 各一个点，第 1 根没有点（ws-signals 断了），第 2 根 130s 一个点
        let pts = vec![(10_000, 1.0), (50_000, 3.0), (130_000, -2.0)];
        let ts = vec![0, m, 2 * m];
        let last = combo_aux("Binance Linear:BTCUSDT", m, &ts, false, &pts).unwrap();
        assert_eq!(last[0], 3.0, "收盘时能知道的最后一个");
        assert!(last[1].is_nan(), "这根里一个点都没有：缺，不拿上一根的老值顶");
        assert_eq!(last[2], -2.0);
        let mean = combo_aux("Binance Linear:BTCUSDT", m, &ts, true, &pts).unwrap();
        assert_eq!(mean[0], 2.0);
        assert!(mean[1].is_nan());
        assert!(combo_aux("Binance Linear:ETHUSDT", m, &ts, false, &pts).unwrap_err().contains("只算 BTCUSDT"));
        assert!(combo_aux("Binance Linear:BTCUSDT", m, &ts, false, &[]).is_err());
        // 指标本身：值 = 对照序列，均值 / Z 照外部序列
        let d = ind::def("combo").unwrap();
        assert_eq!(d.pane, "new");
        assert!(d.data.contains(&"aux"));
    }

    #[test]
    fn 副图高_自下而上摞_拖上沿改高_松开存() {
        let _g = super::super::studies_bg::test_guard();             // 共用界面状态：与别的走 handle 的测试串行
        // 两个副图：上面 100 高、下面缺省 64；每个下面一道 4 像素的缝
        // 下面那个：[900 − 4 − 64, 900 − 4)；上面那个再往上让一道缝：[832 − 4 − 100, 832 − 4)
        assert_eq!(sub_slots(&[100.0, SUB_H], 900.0), vec![(728.0, 828.0), (832.0, 896.0)]);
        put("测试图副图高", vec![Study { uid: 9501, id: "rsi".into(), pane: "sub".into(), ..Study::default() }]);
        let c = || "测试图副图高".to_string();
        handle(StudyMsg::ResizeStart { chart: c(), uid: 9501 });
        handle(StudyMsg::PointerMove { x: 0.0, y: 500.0 });             // 第一次移动只记起点
        assert_eq!(studies_of("测试图副图高")[0].sub_h, 0);
        handle(StudyMsg::PointerMove { x: 0.0, y: 440.0 });             // 往上 60：64 → 124
        assert_eq!(studies_of("测试图副图高")[0].sub_h, 124);
        handle(StudyMsg::PointerMove { x: 0.0, y: 2000.0 });            // 往下拖过头：夹在最小高
        assert_eq!(studies_of("测试图副图高")[0].sub_h, SUB_H_MIN as u32);
        handle(StudyMsg::PointerUp);
        assert!(ui_state().pointer.is_none());
        assert_eq!(pane_h(&studies_of("测试图副图高")[0]), SUB_H_MIN);
        drop_chart("测试图副图高");
    }

    #[test]
    fn 条件判定() {
        assert!(alert_hit("cross_up", 69.0, 70.0, 70.0) && !alert_hit("cross_up", 70.0, 71.0, 70.0));
        assert!(alert_hit("cross_down", 31.0, 29.0, 30.0) && !alert_hit("cross_down", f64::NAN, 29.0, 30.0));
        assert!(alert_hit("above", f64::NAN, 1.0, 0.5) && !alert_hit("below", 0.0, f64::NAN, 1.0));
    }

    #[test]
    fn 收盘确认提醒只在新收盘时响_打开图不补报() {
        let sym = "测试:STUDYALERT";
        let tf = 60_000;
        // 一路涨的收盘价：EMA 3 在第 k 根穿过某个价位
        let b: Vec<Ohlcv> = (0..60).map(|i| { let c = 100.0 + i as f64; (i as u64 * tf, c, c, c, c, 1.0) }).collect();
        {
            
            put(sym, vec![Study {
                uid: 7, id: "ema".into(), params: [("n".to_string(), Value::from(3))].into_iter().collect(),
                alert: Some(StudyAlert { out: "ema".into(), cond: "cross_up".into(), level: 150.0, level_text: "150".into(),
                                         tf_ms: tf, cooldown_s: 0, ..StudyAlert::default() }),
                ..Study::default()
            }]);
        }
        let n_fired = || fired_log().iter().filter(|(t, _)| t.contains("STUDYALERT")).count();
        // 打开图：历史里早就穿过 140，但第一次只记状态
        let _ = compute(&cr(sym), tf, &b[..45], 45 * tf);
        assert_eq!(n_fired(), 0);
        // 逐根收盘：EMA 3（SMA 起种、上涨 1/根）≈ 收盘 − 1，收盘 151 那根穿 150
        for k in 46..=60 {
            let _ = compute(&cr(sym), tf, &b[..k], k as u64 * tf);
        }
        assert_eq!(n_fired(), 1, "{:?}", fired_log());
        assert!(fired_log().last().unwrap().1.contains("上穿"));
        drop_chart(sym);
    }

    #[test]
    fn 没开图时后台判提醒_图接手后不重报() {
        let _g = super::super::studies_bg::test_guard();
        super::super::studies_bg::reset();
        let sym = "测试:BGALERT";
        let tf = 60_000;
        let b: Vec<Ohlcv> = (0..60).map(|i| { let c = 100.0 + i as f64; (i as u64 * tf, c, c, c, c, 1.0) }).collect();
        put(sym, vec![Study {
            uid: 8, id: "ema".into(), params: [("n".to_string(), Value::from(3))].into_iter().collect(),
            alert: Some(StudyAlert { out: "ema".into(), cond: "cross_up".into(), level: 150.0, level_text: "150".into(),
                                     tf_ms: tf, cooldown_s: 0, ..StudyAlert::default() }),
            ..Study::default()
        }]);
        let n_fired = || fired_log().iter().filter(|(t, _)| t.contains("BGALERT")).count();
        // 没有图：后台要取这一路
        let now = 45 * tf;
        assert!(armed_alerts().iter().any(|a| a.chart.symbol == sym && a.tf == tf));
        assert!(super::super::studies_bg::due(now, &armed_alerts()).iter().any(|j| j.symbol == sym));
        // 后台逐根算：与开着图时一样只在穿越那根收盘时响一次
        let _ = compute_bg(&cr(sym), tf, &b[..45], None, now, now);
        for k in 46..=52 {
            let _ = compute_bg(&cr(sym), tf, &b[..k], None, k as u64 * tf, k as u64 * tf);
        }
        assert_eq!(n_fired(), 1, "{:?}", fired_log());
        // 图打开（同样的 K 线）：同一份提醒状态，不重报；之后由图判，后台不再取
        let _ = compute(&cr(sym), tf, &b[..52], 52 * tf + 1);
        assert_eq!(n_fired(), 1);
        assert!(!super::super::studies_bg::due(52 * tf + 5_000, &armed_alerts()).iter().any(|j| j.symbol == sym));
        drop_chart(sym);
    }

    #[test]
    fn 跨品种指标只用已收盘的k线_与在那个品种上直接算的逐位相同() {
        let _g = super::super::studies_bg::test_guard();
        super::super::studies_bg::reset();
        let (btc, eth) = ("测试:XBTC", "测试:XETH");
        let tf = 60_000u64;
        let mk = |f: f64| -> Vec<Ohlcv> { (0..80).map(|i| { let c = 100.0 + (i as f64 * f).sin() * 7.0 + i as f64 * 0.1; (i as u64 * tf, c, c + 1.0, c - 1.0, c, 5.0) }).collect() };
        let (b, e) = (mk(0.3), mk(0.17));
        let rsi = |symbol: &str| Study { uid: 1, id: "rsi".into(), symbol: symbol.into(), pane: "sub".into(), ..Study::default() };
        put(btc, vec![rsi(eth)]);
        // 图：BTC 前 60 根已收盘 + 第 61 根未收盘
        let now = 60 * tf + 30_000;
        let c0 = compute(&cr(btc), tf, &b[..61], now);
        assert!(c0.errors[0].as_deref().is_some_and(|x| x.contains("在取")), "{:?}", c0.errors);
        // 后台取到 ETH：连未收盘那根（第 61 根）一起给，算的时候不能用它
        let jobs = super::super::studies_bg::due(now, &[]);
        assert!(jobs.iter().any(|j| j.symbol == eth && j.tf == tf));
        super::super::studies_bg::ingest(eth, tf, Ok(e[..61].iter().map(|k| (*k, f64::NAN)).collect()), now);
        let c = compute(&cr(btc), tf, &b[..61], now);
        assert!(c.errors[0].is_none(), "{:?}", c.errors);
        // 对照：在 ETH 自己的图上直接算（只给已收盘的 60 根）
        put(eth, vec![rsi("")]);
        let d = compute_raw(("测试对照".into(), tf), tf, vec![rsi("")], &e[..60], &Vec::new(), u64::MAX).0;
        let got = &c.cols[0].as_ref().unwrap()[0];
        let want = &d.cols[0].as_ref().unwrap()[0];
        assert_eq!(got.len(), 61);
        for i in 0..60 {
            assert_eq!(got[i].to_bits(), want[i].to_bits(), "第 {i} 根");
        }
        // 未收盘那根：只能是 ETH 上一根已收盘的值，不是 ETH 第 61 根（还没收盘）的
        assert_eq!(got[60].to_bits(), want[59].to_bits());
        drop_chart(btc);
        drop_chart(eth);
    }

    #[test]
    fn 持仓按可得时刻对齐_晚到的数据静默重算() {
        use super::super::studies_bg as bg;
        let _g = bg::test_guard();
        bg::reset();
        let (sym, tf) = ("测试:OIX", 60_000u64);
        let b: Vec<Ohlcv> = (0..30).map(|i| { let c = 100.0 + i as f64; (i as u64 * tf, c, c, c, c, 1.0) }).collect();
        put(sym, vec![Study { uid: 1, id: "oi".into(), pane: "sub".into(),
                                                                         params: [("n".to_string(), Value::from(3))].into_iter().collect(), ..Study::default() }]);
        let now = 30 * tf + 20_000;
        let c0 = compute(&cr(sym), tf, &b, now);
        assert!(c0.errors[0].as_deref().is_some_and(|e| e.contains("持仓")), "{:?}", c0.errors);
        // 后台取到：第 i 根收盘时（(i+1)·tf）能知道持仓 = 1000 + i；只到第 19 根
        assert!(bg::due(now, &[]).iter().any(|j| j.kind == "oi"));
        bg::ingest_series(sym, "oi", tf, Ok((0..20).map(|i| ((i + 1) * tf, 1000.0 + i as f64)).collect()), now);
        let c1 = compute(&cr(sym), tf, &b, now);
        let oi = &c1.cols[0].as_ref().unwrap()[0];
        assert_eq!(oi[5], 1005.0, "收盘那一刻刚好能知道");
        assert_eq!(oi[25], 1019.0, "之后没新值：沿用最近的（30 分钟内）");
        assert!(c1.errors[0].is_none());
        // 晚到的补上：静默重算，与「一开始就有」的结果相同；不算「数据修正」
        bg::ingest_series(sym, "oi", tf, Ok((19..30).map(|i| ((i + 1) * tf, 1000.0 + i as f64)).collect()), now);
        let c2 = compute(&cr(sym), tf, &b, now);
        let oi2 = &c2.cols[0].as_ref().unwrap()[0];
        assert_eq!(oi2[25], 1025.0);
        let chg = &c2.cols[0].as_ref().unwrap()[1];
        assert_eq!(chg[25].to_bits(), (100.0 * (1025.0f64 / 1022.0 - 1.0)).to_bits());
        assert_eq!(c2.recomputed_from, None, "只是 K 线之外的数据晚到，不提示数据修正");
        drop_chart(sym);
    }

    #[test]
    fn cvd用图自带的主动买量() {
        let (sym, tf) = ("测试:CVDX", 60_000u64);
        let b: Vec<Ohlcv> = (0..10).map(|i| (i as u64 * tf, 1.0, 1.0, 1.0, 1.0, 10.0 + i as f64)).collect();
        let buy: Vec<f64> = b.iter().map(|k| 0.7 * k.5).collect();
        put(sym, vec![Study { uid: 1, id: "cvd".into(), pane: "sub".into(), ..Study::default() }]);
        let c = compute_ex(&cr(sym), tf, &b, Some(&buy), 10 * tf);
        let cvd = &c.cols[0].as_ref().unwrap()[1];
        let want: f64 = b.iter().map(|k| 2.0 * 0.7 * k.5 - k.5).sum();
        assert!((cvd[9] - want).abs() < 1e-9, "{} vs {want}", cvd[9]);
        // 没给主动买量（交易所 K 线不带）：说原因
        let c = compute_ex(&cr(sym), tf, &b, None, 10 * tf);
        assert!(c.errors[0].as_deref().is_some_and(|e| e.contains("主动买卖量")), "{:?}", c.errors);
        drop_chart(sym);
    }

    #[test]
    fn 相关的对照品种从后台取_与直接给对照列算的逐位相同() {
        use super::super::studies_bg as bg;
        use wealthspring_indicators::engine::IndicatorSet;
        let _g = bg::test_guard();
        bg::reset();
        let (sym, aux, tf) = ("测试:PAIRA", "测试:PAIRB", 60_000u64);
        let mk = |f: f64| -> Vec<Ohlcv> { (0..60).map(|i| { let c = 100.0 + (i as f64 * f).sin() * 5.0 + i as f64 * 0.1; (i as u64 * tf, c, c + 1.0, c - 1.0, c, 3.0) }).collect() };
        let (a, x) = (mk(0.3), mk(0.21));
        let st = Study { uid: 1, id: "correl".into(), pane: "sub".into(), aux_symbol: aux.into(),
                         params: [("n".to_string(), Value::from(10)), ("on".to_string(), Value::from("levels"))].into_iter().collect(), ..Study::default() };
        put(sym, vec![st]);
        let now = 60 * tf + 20_000;
        assert!(compute(&cr(sym), tf, &a, now).errors[0].as_deref().is_some_and(|e| e.contains("PAIRB")));
        assert!(bg::due(now, &[]).iter().any(|j| j.symbol == aux && j.kind == bg::BARS));
        bg::ingest(aux, tf, Ok(x.iter().map(|k| (*k, f64::NAN)).collect()), now);
        let c = compute(&cr(sym), tf, &a, now);
        assert!(c.errors[0].is_none(), "{:?}", c.errors);
        let got = &c.cols[0].as_ref().unwrap()[0];
        // 对照：直接在核心里给 aux 列
        let mut s = IndicatorSet::new();
        let n = s.add("correl", &[("n".to_string(), Value::from(10)), ("on".to_string(), Value::from("levels"))].into_iter().collect(), None).unwrap();
        for (k, y) in a.iter().zip(&x) {
            let mut bar = Bar::new(k.0 as i64, tf as i64, k.1, k.2, k.3, k.4, k.5);
            bar.aux = y.4;
            s.push(bar);
        }
        assert!(got.iter().zip(s.column(n, 0)).all(|(p, q)| p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan())));
        assert!(Study { id: "correl".into(), aux_symbol: aux.into(), ..Study::default() }.label().contains("vs PAIRB"));
        drop_chart(sym);
    }

    #[test]
    fn 品种输入() {
        assert_eq!(resolve_symbol("Binance Linear:BTCUSDT", " ethusdt "), "Binance Linear:ETHUSDT");
        assert_eq!(resolve_symbol("Binance Linear:BTCUSDT", "Bybit Linear:ethusdt"), "Bybit Linear:ETHUSDT");
        assert_eq!(resolve_symbol("Binance Linear:BTCUSDT", "btcusdt"), "", "和图同一个 = 本品种");
        assert_eq!(resolve_symbol("Binance Linear:BTCUSDT", ""), "");
        let s = Study { id: "rsi".into(), symbol: "Binance Linear:ETHUSDT".into(), ..Study::default() };
        assert!(s.label().contains("· ETHUSDT"), "{}", s.label());
    }

    #[test]
    fn 高周期只用已收盘的k线_不前视() {
        let tf = 300_000u64; // 5m
        let hi = 3_600_000u64; // 1h
        // 从 00:10 开始（第一根 1h 不完整，要丢掉）到 03:00 前，收盘价 = 序号
        let k: Vec<Ohlcv> = (2..36).map(|i| { let c = i as f64; (i * tf, c, c + 0.5, c - 0.5, c, 1.0) }).collect();
        let now = 36 * tf + 1;
        let agg = aggregate_closed(&k, tf, hi, now);
        assert_eq!(agg.iter().map(|b| b.0).collect::<Vec<_>>(), vec![hi, 2 * hi], "第一根不完整、最后一根 02:55 收盘后 03:00 才算");
        assert_eq!((agg[0].1, agg[0].4, agg[0].5), (12.0, 23.0, 12.0));
        // 02:55 那根还没收盘时，02:00 那根 1h 不算
        assert_eq!(aggregate_closed(&k, tf, hi, 35 * tf + 1).len(), 1);
        // 对齐：01:00–02:00 那根在 01:55 那根 5m（02:00 收盘）上才出现
        let ts: Vec<u64> = k.iter().map(|b| b.0).collect();
        let col = align_closed(&ts, tf, &[hi, 2 * hi], hi, &[1.0, 2.0]);
        let at = |t: u64| col[ts.iter().position(|x| *x == t).unwrap()];
        assert!(at(22 * tf).is_nan(), "01:50 那根 01:55 收盘，01:00 的 1h 还没收");
        assert_eq!(at(23 * tf), 1.0, "01:55 那根 02:00 收盘，正好看到 01:00–02:00");
        assert_eq!(at(3 * hi - 2 * tf), 1.0);
        assert_eq!(at(3 * hi - tf), 2.0);
    }

    #[test]
    fn 图上高周期指标与直接在高周期上算的逐位相同() {
        let sym = "测试:STUDYMTF";
        let tf = 60_000u64;
        let hi = 900_000u64;
        let k: Vec<Ohlcv> = (0..600).map(|i| {
            let c = 100.0 + (i as f64 * 0.07).sin() * 3.0;
            (i * tf, c, c + 0.4, c - 0.3, c + 0.1, 2.0)
        }).collect();
        put(sym, vec![
            Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(5))].into_iter().collect(), tf_ms: hi, ..Study::default() },
            Study { uid: 2, id: "sma".into(), ..Study::default() },
        ]);
        let now = 600 * tf - 1; // 最后一根没收盘
        let c = compute(&cr(sym), tf, &k, now);
        let agg = aggregate_closed(&k, tf, hi, now);
        let mut set = IndicatorSet::new();
        let e = set.add("ema", &[("n".to_string(), Value::from(5))].into_iter().collect(), None).unwrap();
        for b in &agg {
            set.push(to_bar(b, hi, &Vec::new(), 0));
        }
        let hts: Vec<u64> = agg.iter().map(|b| b.0).collect();
        let want = align_closed(&c.ts, tf, &hts, hi, set.column(e, 0));
        let got = &c.cols[0].as_ref().unwrap()[0];
        assert!(got.iter().zip(&want).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(got[599].is_finite() && c.cols[1].is_some());
        // 比图小的周期：这张图上不算，给原因
        let c5 = compute(&cr(sym), 1_800_000, &aggregate_closed(&k, tf, 1_800_000, now), now);
        assert!(c5.errors[0].is_some() && c5.cols[0].is_none());
        drop_chart(sym);
    }

    #[test]
    fn 已收盘的k线被修正_从那根起重算_与重新整段算逐位相同() {
        let sym = "测试:STUDYFIX";
        let tf = 60_000u64;
        put(sym, vec![
            Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(8))].into_iter().collect(), ..Study::default() },
            Study { uid: 2, id: "sma".into(), params: [("n".to_string(), Value::from(4))].into_iter().collect(),
                    source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), ..Study::default() },
        ]);
        let mut k = bars(3000);
        let now = 3000 * tf;
        let _ = compute(&cr(sym), tf, &k, now);
        // 第 1200 根被交易所修正，末尾少了 5 根（中间补齐 / 删掉也走同一条路）
        k[1200].4 += 7.5;
        k.truncate(2995);
        let fixed = compute(&cr(sym), tf, &k, now);
        assert_eq!(fixed.recomputed_from, Some(k[1200].0));
        // 只清这个品种的（整张清会把并行跑的别的测试刚算好的结果抹掉）
        caches().lock().unwrap().retain(|k, _| !k.0.contains(sym));
        merged().lock().unwrap().retain(|k, _| k.0 != sym);
        let fresh = compute(&cr(sym), tf, &k, now);
        for i in 0..2 {
            let (a, b) = (&fixed.cols[i].as_ref().unwrap()[0], &fresh.cols[i].as_ref().unwrap()[0]);
            assert_eq!(a.len(), 2995);
            assert!(a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()), "第 {i} 个指标");
        }
        drop_chart(sym);
    }

    #[test]
    fn 标签只写数值参数() {
        let s = Study { uid: 9, id: "bbands".into(), ..Study::default() };
        assert_eq!(s.label(), "布林带 20 2");
    }
}

#[cfg(test)]
mod bench_live {
    use super::*;
    use super::tests::{cr, drop_chart, put};

    /// 手动跑：cargo test --release bench_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn 实时一跳的开销() {
        let sym = "测试:BENCHLIVE";
        let tf = 60_000u64;
        let p = |k: &str, v: i64| [(k.to_string(), Value::from(v))].into_iter().collect();
        put(sym, vec![
            Study { uid: 1, id: "ema".into(), params: p("n", 20), ..Study::default() },
            Study { uid: 2, id: "ema".into(), params: p("n", 50), ..Study::default() },
            Study { uid: 3, id: "bbands".into(), params: p("n", 20), ..Study::default() },
            Study { uid: 4, id: "rsi".into(), params: p("n", 14), pane: "sub".into(), ..Study::default() },
            Study { uid: 5, id: "macd".into(), pane: "sub".into(), ..Study::default() },
            Study { uid: 6, id: "vwap".into(), ..Study::default() },
        ]);
        for n in [1_000usize, 5_000, 20_000] {
            let mut b: Vec<Ohlcv> = (0..n).map(|i| { let c = 100.0 + (i as f64 * 0.3).sin() * 5.0; (i as u64 * tf, c, c + 0.5, c - 0.5, c, 10.0) }).collect();
            let buy = vec![5.0; n];
            let now = b[n - 1].0 + 30_000;
            let _ = compute_ex(&cr(sym), tf, &b, Some(&buy), now);
            let iters = 200;
            let t0 = std::time::Instant::now();
            for i in 0..iters {
                b[n - 1].4 = 100.0 + i as f64 * 0.01;
                let _ = compute_ex(&cr(sym), tf, &b, Some(&buy), now);
            }
            eprintln!("N={n}: compute_ex 每跳 {:?}", t0.elapsed() / iters);
        }
        drop_chart(sym);
    }
}
