//! 图上指标（docs/63 I3）：K 线图的叠加指标与副图指标。计算在 Rust 指标核心（`wealthspring-indicators`），
//! 与策略、状态引擎、检验框架是**同一份实现**；这里管：哪些指标挂在哪个品种上、按图的 K 线增量计算、画、读数。
//!
//! | 文件 | 内容 |
//! |---|---|
//! | `<小文件目录>/cockpit/studies.json` | 每个品种的指标列表（同一品种的所有图、所有周期共用，与画线 docs/46 同）、模板、收藏、最近用过 |
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
}

impl Default for Study {
    fn default() -> Self {
        Self { uid: 0, id: String::new(), params: BTreeMap::new(), source: None, visible: true, pane: "overlay".into(),
               styles: BTreeMap::new(), min_tf_ms: 0, max_tf_ms: 0, tf_ms: 0, alert: None }
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
        let base = if parts.is_empty() { name } else { format!("{name} {}", parts.join(" ")) };
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub by_symbol: BTreeMap<String, Vec<Study>>,
    pub templates: BTreeMap<String, Vec<Study>>,
    pub favorites: Vec<String>,
    pub recent: Vec<String>,
    pub next_uid: u64,
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

pub fn studies_for(symbol: &str) -> Vec<Study> {
    store().lock().map(|s| s.by_symbol.get(symbol).cloned().unwrap_or_default()).unwrap_or_default()
}

pub fn snapshot() -> Store {
    store().lock().map(|s| s.clone()).unwrap_or_default()
}

/// 改指标设置的入口（检查器「指标」一节发的消息）。返回给界面的一句话（错误或空）。
#[derive(Debug, Clone)]
pub enum StudyMsg {
    Add { symbol: String, id: String },
    Remove { symbol: String, uid: u64 },
    Visible { symbol: String, uid: u64, on: bool },
    Pane { symbol: String, uid: u64, pane: String },
    /// 计算参数：文本输入（数字）或选项
    Param { symbol: String, uid: u64, key: String, value: String },
    OutVisible { symbol: String, uid: u64, out: String, on: bool },
    OutColor { symbol: String, uid: u64, out: String },
    Source { symbol: String, uid: u64, src: Option<StudySrc> },
    Move { symbol: String, uid: u64, up: bool },
    /// 复制一个（新的 uid，排在它后面）
    Duplicate { symbol: String, uid: u64 },
    /// 只在这些周期显示（毫秒；0 = 不限）
    TfRange { symbol: String, uid: u64, min: u64, max: u64 },
    Alert { symbol: String, uid: u64, op: AlertOp },
    /// 在哪个周期上算（毫秒；0 = 跟图）
    CalcTf { symbol: String, uid: u64, tf: u64 },
    Favorite(String),
    SaveTemplate { symbol: String, name: String },
    ApplyTemplate { symbol: String, name: String },
    DeleteTemplate(String),
    /// 界面状态（不存盘）
    Search(String),
    Category(Option<String>),
    TemplateName(String),
    Expand(Option<u64>),
}

#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub search: String,
    pub category: Option<String>,
    pub template_name: String,
    pub expanded: Option<u64>,
    pub note: String,
}

fn ui() -> &'static Mutex<UiState> {
    static U: OnceLock<Mutex<UiState>> = OnceLock::new();
    U.get_or_init(Default::default)
}

pub fn ui_state() -> UiState {
    ui().lock().map(|u| u.clone()).unwrap_or_default()
}

fn with_list(s: &mut Store, symbol: &str, f: impl FnOnce(&mut Vec<Study>)) {
    let list = s.by_symbol.entry(symbol.to_string()).or_default();
    f(list);
    if list.is_empty() {
        s.by_symbol.remove(symbol);
    }
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
            _ => {}
        }
    }
    let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match m {
        StudyMsg::Add { symbol, id } => {
            if let Some(d) = ind::def(&id) {
                s.next_uid += 1;
                let uid = s.next_uid;
                let pane = d.pane.to_string();
                let pane = if pane == "new" { "sub".to_string() } else { pane };
                with_list(&mut s, &symbol, |l| l.push(Study { uid, id: id.clone(), pane, ..Study::default() }));
                s.recent.retain(|x| x != &id);
                s.recent.insert(0, id);
                s.recent.truncate(8);
                ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).expanded = Some(uid);
            }
        }
        StudyMsg::Remove { symbol, uid } => with_list(&mut s, &symbol, |l| {
            l.retain(|x| x.uid != uid);
            for x in l.iter_mut() {
                if matches!(&x.source, Some(StudySrc::Study { uid: u, .. }) if *u == uid) {
                    x.source = None;           // 引用了被删的指标：退回缺省数据源
                }
            }
        }),
        StudyMsg::Visible { symbol, uid, on } => with_list(&mut s, &symbol, |l| l.iter_mut().filter(|x| x.uid == uid).for_each(|x| x.visible = on)),
        StudyMsg::Pane { symbol, uid, pane } => with_list(&mut s, &symbol, |l| l.iter_mut().filter(|x| x.uid == uid).for_each(|x| x.pane = pane.clone())),
        StudyMsg::Param { symbol, uid, key, value } => with_list(&mut s, &symbol, |l| {
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
        StudyMsg::OutVisible { symbol, uid, out, on } => with_list(&mut s, &symbol, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.visible = on;
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::OutColor { symbol, uid, out } => with_list(&mut s, &symbol, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                let mut st = x.style(&out);
                st.color = Some(st.color.map_or(1, |c| (c + 1) % 8));
                x.styles.insert(out.clone(), st);
            }
        }),
        StudyMsg::Source { symbol, uid, src } => with_list(&mut s, &symbol, |l| {
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
        StudyMsg::Move { symbol, uid, up } => with_list(&mut s, &symbol, |l| {
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
        StudyMsg::Duplicate { symbol, uid } => {
            s.next_uid += 1;
            let nu = s.next_uid;
            with_list(&mut s, &symbol, |l| {
                if let Some(i) = l.iter().position(|x| x.uid == uid) {
                    let mut c = l[i].clone();
                    c.uid = nu;
                    l.insert(i + 1, c);
                }
            });
        }
        StudyMsg::TfRange { symbol, uid, min, max } => with_list(&mut s, &symbol, |l| {
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                x.min_tf_ms = min;
                x.max_tf_ms = max;
            }
        }),
        StudyMsg::Alert { symbol, uid, op } => with_list(&mut s, &symbol, |l| {
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
        StudyMsg::CalcTf { symbol, uid, tf } => with_list(&mut s, &symbol, |l| {
            let refs = l.iter().any(|x| matches!(&x.source, Some(StudySrc::Study { uid: u, .. }) if *u == uid));
            for x in l.iter_mut().filter(|x| x.uid == uid) {
                if matches!(x.source, Some(StudySrc::Study { .. })) || refs {
                    note = "引用了别的指标、或被别的指标引用的，跟着对方的周期（先断开引用）".into();
                } else {
                    x.tf_ms = tf;
                }
            }
        }),
        StudyMsg::Favorite(id) => {
            if s.favorites.contains(&id) {
                s.favorites.retain(|x| x != &id);
            } else {
                s.favorites.push(id);
            }
        }
        StudyMsg::SaveTemplate { symbol, name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                note = "模板要有名字".into();
            } else {
                let list = s.by_symbol.get(&symbol).cloned().unwrap_or_default();
                s.templates.insert(name, list);
            }
        }
        StudyMsg::ApplyTemplate { symbol, name } => {
            if let Some(t) = s.templates.get(&name).cloned() {
                // 套模板：换成新的 uid（引用关系跟着换）
                let mut map = HashMap::new();
                let mut out = Vec::new();
                for mut x in t {
                    s.next_uid += 1;
                    map.insert(x.uid, s.next_uid);
                    x.uid = s.next_uid;
                    if let Some(StudySrc::Study { uid, out: o }) = &x.source {
                        x.source = map.get(uid).map(|n| StudySrc::Study { uid: *n, out: o.clone() });
                    }
                    out.push(x);
                }
                s.by_symbol.insert(symbol, out);
            }
        }
        StudyMsg::DeleteTemplate(name) => {
            s.templates.remove(&name);
        }
        StudyMsg::Search(_) | StudyMsg::Category(_) | StudyMsg::TemplateName(_) | StudyMsg::Expand(_) => {}
    }
    save(&s);
    ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).note = note;
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
}

fn caches() -> &'static Mutex<HashMap<(String, u64), Cache>> {
    static C: OnceLock<Mutex<HashMap<(String, u64), Cache>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// K 线（开盘时刻毫秒, 开, 高, 低, 收, 量）。
pub type Ohlcv = (u64, f64, f64, f64, f64, f64);

fn to_bar(k: &Ohlcv, tf: u64) -> Bar {
    Bar::new(k.0 as i64, tf as i64, k.1, k.2, k.3, k.4, k.5)
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

fn same_bar(b: &Bar, k: &Ohlcv, tf: u64) -> bool {
    b.ts == k.0 as i64 && b.tf_ms == tf as i64 && [b.open, b.high, b.low, b.close, b.volume].iter().zip([k.1, k.2, k.3, k.4, k.5]).all(|(a, x)| a.to_bits() == x.to_bits())
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
/// 设了「在哪个周期上算」比图大的，单独并成高周期 K 线算一份，再对齐回来（I6）。
pub fn compute(symbol: &str, tf: u64, klines: &[Ohlcv], now_ms: u64) -> Arc<Computed> {
    refresh_user();
    let studies = studies_for(symbol);
    if studies.is_empty() || klines.is_empty() || tf == 0 {
        return Arc::new(Computed::default());
    }
    // 按实际计算周期分组；比图小、或不是图周期整数倍的算不了
    let eff = |s: &Study| if s.tf_ms == 0 { tf } else { s.tf_ms };
    let mut groups: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut bad: HashMap<usize, String> = HashMap::new();
    for (i, s) in studies.iter().enumerate() {
        let e = eff(s);
        if e < tf || e % tf != 0 {
            bad.insert(i, format!("计算周期 {} 比图的周期 {} 小或不是它的整数倍，这张图上不算", tf_label(e), tf_label(tf)));
        } else {
            groups.entry(e).or_default().push(i);
        }
    }
    let mut parts: Vec<(u64, Vec<usize>, Arc<Computed>)> = Vec::new();
    let mut changed = false;
    for (e, idx) in groups {
        let sub: Vec<Study> = idx.iter().map(|&i| studies[i].clone()).collect();
        let (c, ch) = if e == tf {
            compute_raw((symbol.to_string(), tf), tf, sub, klines, now_ms)
        } else {
            let agg = aggregate_closed(klines, tf, e, now_ms);
            compute_raw((format!("{symbol}@{tf}"), e), e, sub, &agg, u64::MAX)
        };
        changed |= ch;
        parts.push((e, idx, c));
    }
    let key = (symbol.to_string(), tf);
    let prev = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key).cloned();
    let native_ts: Vec<u64> = {
        // 图的时间轴：已收盘的 + 未收盘那根
        klines.iter().map(|k| k.0).collect()
    };
    let last_open = klines.last().is_some_and(|k| k.0 + tf > now_ms);
    let out = match prev {
        Some(p) if !changed && p.cols.len() == studies.len() && p.ts.len() == native_ts.len() => p,
        _ => {
            let mut cols: Vec<Option<Vec<Vec<f64>>>> = vec![None; studies.len()];
            let mut errors: Vec<Option<String>> = vec![None; studies.len()];
            for (i, e) in &bad {
                errors[*i] = Some(e.clone());
            }
            for (e, idx, c) in &parts {
                for (k, &i) in idx.iter().enumerate() {
                    errors[i] = c.errors.get(k).cloned().flatten();
                    cols[i] = c.cols.get(k).cloned().flatten().map(|cs| {
                        if *e == tf {
                            cs
                        } else {
                            cs.iter().map(|col| align_closed(&native_ts, tf, &c.ts, *e, col)).collect()
                        }
                    });
                }
            }
            let recomputed_from = parts.iter().filter_map(|p| p.2.recomputed_from).min();
            let m = Arc::new(Computed { ts: native_ts, provisional_ts: last_open.then(|| klines.last().map(|k| k.0)).flatten(), cols, errors,
                                        recomputed_from });
            merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, m.clone());
            m
        }
    };
    if changed && studies.iter().any(|s| s.alert.as_ref().is_some_and(|a| a.armed && a.tf_ms == tf)) {
        check_alerts(symbol, tf, &studies, &out, now_ms);
    }
    out
}

/// 一组同周期的指标按 K 线增量算。`ckey` 是缓存键（高周期那组带上图的周期，免得和真的高周期图互相顶掉）。
fn compute_raw(ckey: (String, u64), tf: u64, studies: Vec<Study>, klines: &[Ohlcv], now_ms: u64) -> (Arc<Computed>, bool) {
    if klines.is_empty() {
        return (Arc::new(Computed { cols: vec![None; studies.len()], errors: vec![None; studies.len()], ..Computed::default() }), true);
    }
    let last_open = klines.last().is_some_and(|k| k.0 + tf > now_ms);
    let n_conf = if last_open { klines.len() - 1 } else { klines.len() };
    let conf = &klines[..n_conf];
    let prov = if last_open { klines.last().map(|k| to_bar(k, tf)) } else { None };
    let mut map = caches().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = ckey;
    // 历史往左延伸（滚回去加载更早的）或指标列表结构变了 → 重建；中间的 K 线变了 → 下面从那根起重算
    let reusable = map.get(&key).is_some_and(|c| c.first_ts == conf.first().map_or(0, |k| k.0) && same_shape(&c.studies, &studies));
    if !reusable {
        let bars: Vec<Bar> = conf.iter().map(|k| to_bar(k, tf)).collect();
        let (set, node, errors) = build(&studies, &bars);
        map.insert(key.clone(), Cache { studies: studies.clone(), set, node, errors, first_ts: conf.first().map_or(0, |k| k.0),
                                        confirmed: conf.iter().map(|k| k.0).collect(), out: Arc::new(Computed::default()), last_fix: None });
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
    // 下游依赖跟着重算。逐根比对的是核心里存着的已确认 K 线，不另存一份
    let mut fixed = false;
    {
        let old = c.set.bars();
        let m = old.len().min(conf.len());
        let d = (0..m).find(|&i| !same_bar(&old[i], &conf[i], tf)).unwrap_or(m);
        if d < old.len() {
            let fresh: Vec<Bar> = conf[d..].iter().map(|k| to_bar(k, tf)).collect();
            c.set.correct(d, fresh);
            c.confirmed.truncate(d);
            c.confirmed.extend(conf[d..].iter().map(|k| k.0));
            c.last_fix = conf.get(d).or(conf.last()).map(|k| k.0);
            fixed = true;
        }
    }
    // 新收盘的根、未收盘那根在变、或者暂定那根刚收盘（暂定 → 没有）都要重新生成结果
    let changed = conf.len() > c.confirmed.len()
        || fixed
        || params_changed
        || prov.is_some()
        || c.out.ts.is_empty()
        || c.out.provisional_ts != prov.map(|p| p.ts as u64);
    for k in &conf[c.confirmed.len()..] {
        c.set.push(to_bar(k, tf));
        c.confirmed.push(k.0);
    }
    if let Some(p) = &prov {
        c.set.provisional(*p);
    }
    if changed || c.out.cols.len() != studies.len() {
        c.out = Arc::new(materialize(c, prov.as_ref()));
    }
    (c.out.clone(), changed)
}

// ── 指标条件提醒（I5）────────────────────────────────────────────────────────

/// （品种, uid）→（上次判过的已收盘那根的开盘时刻, 盘中模式的上一个值）
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
fn check_alerts(symbol: &str, tf: u64, studies: &[Study], out: &Computed, now_ms: u64) {
    let n_conf = out.ts.len() - usize::from(out.provisional_ts.is_some());
    let mut hits: Vec<(u64, String, String, bool)> = Vec::new();
    {
        let mut stt = alert_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (i, s) in studies.iter().enumerate() {
            let Some(a) = s.alert.as_ref().filter(|a| a.armed && a.tf_ms == tf) else { continue };
            let Some(Some(cols)) = out.cols.get(i) else { continue };
            let Some(k) = s.def().and_then(|d| d.outputs.iter().position(|o| o.key == a.out)) else { continue };
            let col = &cols[k];
            let key = (symbol.to_string(), s.uid);
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
                hits.push((s.uid, title, body, a.telegram));
            }
        }
    }
    if hits.is_empty() {
        return;
    }
    {
        let mut s = store().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        with_list(&mut s, symbol, |l| {
            for x in l.iter_mut() {
                if let Some(a) = x.alert.as_mut().filter(|_| hits.iter().any(|h| h.0 == x.uid)) {
                    a.last_fired = Some(now_ms);
                    if a.once {
                        a.armed = false;
                    }
                }
            }
        });
        save(&s);
    }
    for (_, title, body, tg) in hits {
        deliver(title, body, tg);
    }
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
fn deliver(title: String, body: String, telegram: bool) {
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
            .and_then(|c| super::calendar_alerts::queue_now(&c, chrono::Utc::now().timestamp_millis(), "indicator", &title, &body, telegram, false));
        if let Err(e) = r {
            ui().lock().unwrap_or_else(std::sync::PoisonError::into_inner).note = format!("指标提醒没发出去：{e}");
        }
    });
}

// ── 画 ──────────────────────────────────────────────────────────────────────

const SUB_H: f32 = 64.0;
const SUB_GAP: f32 = 4.0;

fn color_of(s: &Study, out_idx: usize, key: &str) -> Color {
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
    symbol: &str,
    tf: u64,
    comp: &Computed,
    reserved_bottom: f32,
) {
    if comp.ts.is_empty() || scaling <= f32::EPSILON {
        return;
    }
    let studies = studies_for(symbol);
    let a = comp.ts.partition_point(|t| *t < earliest.saturating_sub(tf));
    let b = comp.ts.partition_point(|t| *t <= latest.saturating_add(tf));
    if a >= b {
        return;
    }
    let w = 1.0 / scaling;
    let subs: Vec<usize> = studies.iter().enumerate().filter(|(i, s)| s.shown_on(tf) && s.pane == "sub" && comp.cols.get(*i).is_some_and(Option::is_some)).map(|(i, _)| i).collect();
    for (i, s) in studies.iter().enumerate() {
        if !s.shown_on(tf) || s.pane == "sub" {
            continue;
        }
        let (Some(Some(cols)), Some(d)) = (comp.cols.get(i), s.def()) else { continue };
        draw_outputs(frame, &d, s, cols, &comp.ts[..], a, b, interval_to_x, &|v| price_to_y(v), w, None, Unit::Price);
    }
    // 副图：自下而上摞在状态条带上面
    let n = subs.len() as f32;
    for (k, &i) in subs.iter().enumerate() {
        let s = &studies[i];
        let (Some(Some(cols)), Some(d)) = (comp.cols.get(i), s.def()) else { continue };
        let top = region.y + region.height - reserved_bottom - (n - k as f32) * (SUB_H + SUB_GAP) / scaling;
        let h = SUB_H / scaling;
        frame.fill_rectangle(Point::new(region.x, top), Size::new(region.width, h), pal::alpha(pal::card_bg(), 0.97)); // 基本不透明：主图的 K 线与叠加线不透过来
        frame.fill_rectangle(Point::new(region.x, top), Size::new(region.width, w), pal::alpha(pal::dim(), 0.25));
        // 纵轴：第一个可见输出的单位与坐标轴声明；只画同单位的输出
        let Some(first) = d.outputs.iter().find(|o| s.style(o.key).visible) else { continue };
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
            continue;
        }
        let pad = (hi - lo) * 0.06;
        let (lo, hi) = (lo - pad, hi + pad);
        let ymap = |v: f64| top + h - ((v - lo) / (hi - lo)) as f32 * h;
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
        draw_outputs(frame, &d, s, cols, &comp.ts[..], a, b, interval_to_x, &ymap, w, Some(ymap(0.0f64.clamp(lo, hi))), unit);
        // 左上角：名字 + 最后一个可见值
        let last = cols.first().and_then(|c| c[..b].iter().rev().find(|v| v.is_finite())).copied();
        frame.fill_text(canvas::Text {
            content: format!("{}  {}", s.label(), last.map_or("—".into(), |v| fmt_val(v))),
            position: Point::new(region.x + 4.0 * w, top + 2.0 * w),
            size: iced::Pixels(10.0 * w),
            color: pal::dim(),
            font: crate::ui::text::ui_font(),
            ..canvas::Text::default()
        });
    }
}

/// 副图一共占多高（主图底部要让出来的；与状态条带一起算）。
pub fn sub_height(symbol: &str, tf: u64, scaling: f32) -> f32 {
    let n = studies_for(symbol).iter().filter(|s| s.shown_on(tf) && s.pane == "sub").count() as f32;
    n * (SUB_H + SUB_GAP) / scaling.max(f32::EPSILON)
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
                for k in a..b {
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
                for k in a..b {
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
pub fn describe_at(symbol: &str, tf: u64, t: u64) -> Vec<(String, Color)> {
    let studies = studies_for(symbol);
    let Some(comp) = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(symbol.to_string(), tf)).cloned() else { return vec![] };
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

/// 这个品种各指标的错误（取任一周期的计算；与 studies 同序）。
pub fn errors_for(symbol: &str) -> Vec<Option<String>> {
    let map = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.iter().find(|((s, _), _)| s == symbol).map(|(_, c)| c.errors.clone()).unwrap_or_default()
}

/// 这个品种最近一次因数据修正而重算的起点（任一周期的图；开盘时刻毫秒）。
pub fn recomputed_from(symbol: &str) -> Option<u64> {
    let map = merged().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.iter().filter(|((s, _), _)| s == symbol).filter_map(|(_, c)| c.recomputed_from).max()
}

/// 指标设置变了没有（图据此清主图缓存）。`seen` 是这张图上次画的版本。
pub fn changed_since(seen: &AtomicU64) -> bool {
    let v = version();
    seen.swap(v, Ordering::Relaxed) != v
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut s = store().lock().unwrap();
            s.by_symbol.insert(sym.into(), vec![
                Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(10))].into_iter().collect(), ..Study::default() },
                Study { uid: 2, id: "sma".into(), params: [("n".to_string(), Value::from(5))].into_iter().collect(),
                        source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), pane: "sub".into(), ..Study::default() },
            ]);
        }
        let b = bars(300);
        // 最后一根没收盘（now 在它的周期里）
        let now = b[299].0 + 30_000;
        let c1 = compute(sym, 60_000, &b, now);
        assert_eq!(c1.ts.len(), 300);
        assert_eq!(c1.provisional_ts, Some(b[299].0));
        let c2 = compute(sym, 60_000, &b[..299], b[298].0 + 60_000);
        assert_eq!(c2.provisional_ts, None);
        // 与一次算完的核心结果逐位相同（前 299 根）
        let mut set = IndicatorSet::new();
        let e = set.add("ema", &[("n".to_string(), Value::from(10))].into_iter().collect(), None).unwrap();
        let m = set.add("sma", &[("n".to_string(), Value::from(5))].into_iter().collect(), Some(Src::Node { node: e, out: 0 })).unwrap();
        for k in &b[..299] {
            set.push(to_bar(k, 60_000));
        }
        let got = c2.cols[1].as_ref().unwrap()[0].clone();
        assert!(got.iter().zip(set.column(m, 0)).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(!describe_at(sym, 60_000, b[200].0).is_empty());
        store().lock().unwrap().by_symbol.remove(sym);
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
            let mut s = store().lock().unwrap();
            s.by_symbol.insert(sym.into(), vec![Study {
                uid: 7, id: "ema".into(), params: [("n".to_string(), Value::from(3))].into_iter().collect(),
                alert: Some(StudyAlert { out: "ema".into(), cond: "cross_up".into(), level: 150.0, level_text: "150".into(),
                                         tf_ms: tf, cooldown_s: 0, ..StudyAlert::default() }),
                ..Study::default()
            }]);
        }
        let n_fired = || fired_log().iter().filter(|(t, _)| t.contains("STUDYALERT")).count();
        // 打开图：历史里早就穿过 140，但第一次只记状态
        let _ = compute(sym, tf, &b[..45], 45 * tf);
        assert_eq!(n_fired(), 0);
        // 逐根收盘：EMA 3（SMA 起种、上涨 1/根）≈ 收盘 − 1，收盘 151 那根穿 150
        for k in 46..=60 {
            let _ = compute(sym, tf, &b[..k], k as u64 * tf);
        }
        assert_eq!(n_fired(), 1, "{:?}", fired_log());
        assert!(fired_log().last().unwrap().1.contains("上穿"));
        store().lock().unwrap().by_symbol.remove(sym);
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
        store().lock().unwrap().by_symbol.insert(sym.into(), vec![
            Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(5))].into_iter().collect(), tf_ms: hi, ..Study::default() },
            Study { uid: 2, id: "sma".into(), ..Study::default() },
        ]);
        let now = 600 * tf - 1; // 最后一根没收盘
        let c = compute(sym, tf, &k, now);
        let agg = aggregate_closed(&k, tf, hi, now);
        let mut set = IndicatorSet::new();
        let e = set.add("ema", &[("n".to_string(), Value::from(5))].into_iter().collect(), None).unwrap();
        for b in &agg {
            set.push(to_bar(b, hi));
        }
        let hts: Vec<u64> = agg.iter().map(|b| b.0).collect();
        let want = align_closed(&c.ts, tf, &hts, hi, set.column(e, 0));
        let got = &c.cols[0].as_ref().unwrap()[0];
        assert!(got.iter().zip(&want).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(got[599].is_finite() && c.cols[1].is_some());
        // 比图小的周期：这张图上不算，给原因
        let c5 = compute(sym, 1_800_000, &aggregate_closed(&k, tf, 1_800_000, now), now);
        assert!(c5.errors[0].is_some() && c5.cols[0].is_none());
        store().lock().unwrap().by_symbol.remove(sym);
    }

    #[test]
    fn 已收盘的k线被修正_从那根起重算_与重新整段算逐位相同() {
        let sym = "测试:STUDYFIX";
        let tf = 60_000u64;
        store().lock().unwrap().by_symbol.insert(sym.into(), vec![
            Study { uid: 1, id: "ema".into(), params: [("n".to_string(), Value::from(8))].into_iter().collect(), ..Study::default() },
            Study { uid: 2, id: "sma".into(), params: [("n".to_string(), Value::from(4))].into_iter().collect(),
                    source: Some(StudySrc::Study { uid: 1, out: "ema".into() }), ..Study::default() },
        ]);
        let mut k = bars(3000);
        let now = 3000 * tf;
        let _ = compute(sym, tf, &k, now);
        // 第 1200 根被交易所修正，末尾少了 5 根（中间补齐 / 删掉也走同一条路）
        k[1200].4 += 7.5;
        k.truncate(2995);
        let fixed = compute(sym, tf, &k, now);
        assert_eq!(fixed.recomputed_from, Some(k[1200].0));
        caches().lock().unwrap().clear();
        merged().lock().unwrap().clear();
        let fresh = compute(sym, tf, &k, now);
        for i in 0..2 {
            let (a, b) = (&fixed.cols[i].as_ref().unwrap()[0], &fresh.cols[i].as_ref().unwrap()[0]);
            assert_eq!(a.len(), 2995);
            assert!(a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()), "第 {i} 个指标");
        }
        store().lock().unwrap().by_symbol.remove(sym);
    }

    #[test]
    fn 标签只写数值参数() {
        let s = Study { uid: 9, id: "bbands".into(), ..Study::default() };
        assert_eq!(s.label(), "布林带 20 2");
    }
}
