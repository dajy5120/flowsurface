//! 图表绘图（docs/46）：TradingView 式画线。这里是引擎与图表之间的那一层——当前工具、选中、
//! 正在拖的预览、按品种的对象集合（懒加载、改了就后台写盘），以及在画布上怎么画。
//!
//! - 模型 [`model`]、坐标与命中测试 [`geometry`]、命令与存盘 [`store`] 都是纯的，可单测；
//! - 图表（`chart.rs` 的 `canvas_interaction`、`kline.rs` / `heatmap.rs` 的 `draw`）只把事件交过来、
//!   在单独一层缓存里调 [`draw_layer`]；
//! - **画线跟着品种走**：键是「交易所 市场:代码」，同一品种的所有面板、所有周期共用一套；
//!   每次改动版本号 +1，各图在下一帧看到版本变了就重画自己那一层。

pub mod feeds;
pub mod geometry;
pub mod model;
pub mod store;
pub mod tools;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use iced::widget::canvas::{self, Frame, LineDash, Path, Stroke};
use iced::{Point, Size};

use serde::{Deserialize, Serialize};

use geometry::{DrawView, Part, Prim, Role};
use model::{Anchor, Dash, Drawing, Kind};
use store::{Book, Cmd, Db, Effect};

/// 绘图的偏好（G1）：存在库的 `meta` 表（键 `prefs`），所有图共用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// 连续绘制：画完一个不回到光标
    pub continuous: bool,
    /// 磁吸：锚点吸到那根 K 线的开 / 高 / 低 / 收
    pub magnet: bool,
    /// 收藏的工具（工具栏上直接点）
    pub favorites: Vec<Kind>,
    /// 全部隐藏（只是不画，对象不动）
    pub hide_all: bool,
    /// 全部锁定（只能选中看属性，不能拖、不能删）
    pub lock_all: bool,
    /// 样式模板（G3）：名字、哪种工具、样式
    pub templates: Vec<Template>,
    /// 各种工具的缺省样式（G3）：新画的直接用它
    pub defaults: Vec<(Kind, model::Style)>,
    /// 隐藏了哪些系统来源（G4）
    pub hide_sources: Vec<model::Source>,
    /// 浮动绘图条（§13）左上角在内容区里的位置；`None` = 缺省（左上角往里一点）
    pub bar_at: Option<[f32; 2]>,
    /// 浮动绘图条横排（缺省竖排）
    pub bar_horizontal: bool,
}

/// 一个样式模板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub kind: Kind,
    pub style: model::Style,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            continuous: false,
            magnet: false,
            favorites: vec![Kind::Trend, Kind::HLine],
            hide_all: false,
            lock_all: false,
            templates: Vec::new(),
            defaults: Vec::new(),
            hide_sources: Vec::new(),
            bar_at: None,
            bar_horizontal: false,
        }
    }
}

/// 改一个对象的哪个锚点的哪一项（检查器里的精确输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Price,
    Time,
}

/// 对象上可以在检查器里填的参数（G2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    /// 文字 / 标注的内容
    Text,
    /// 斐波那契的比例（逗号分隔）
    Levels,
    /// 仓位工具的数量
    Qty,
    /// 仓位工具的单边手续费（基点）
    Fee,
    /// 成交量分布的档数（G4）
    Rows,
    /// 成交量分布的价值区占比（G4）
    Va,
}

/// 工具栏、检查器、图上浮动小条发来的操作（docs/46 G1）。都是全局状态上的操作，与哪个面板无关。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawMsg {
    Tool(Option<Kind>),
    /// 顶部「绘图」：显示 / 收起浮动绘图条（§13）
    ToggleMenu,
    CloseMenu,
    /// 浮动条上的「⋯」：收藏、系统对象来源、说明
    ToggleMore,
    /// 浮动条横排 / 竖排
    BarHorizontal(bool),
    /// 按下浮动条的拖动柄（两次按下 0.4 秒内 = 复位到缺省位置）
    BarDragStart,
    /// 拖动中光标在窗口里的位置（整窗接住鼠标的那层发）
    BarDragMove(f32, f32),
    BarDragEnd,
    Favorite(Kind),
    Continuous(bool),
    Magnet(bool),
    HideAll(bool),
    LockAll(bool),
    /// (品种, 对象 id)
    Select(String, Option<String>),
    Color(String, String, [u8; 4]),
    Width(String, String, f32),
    DashStyle(String, String, Dash),
    Lock(String, String, bool),
    Hide(String, String, bool),
    /// (品种, id, 周期类别, 显示)
    ShowOn(String, String, String, bool),
    /// 输入框改动：(键, 文字)。键见 [`edit_key`]
    Edit(String, String),
    /// 回车：把输入框里的价格 / 时间写进锚点
    ApplyAnchor(String, String, usize, Field),
    /// 回车：把输入框里的分组名写进对象
    ApplyGroup(String, String),
    /// 回车：把输入框里的参数写进对象（G2）
    ApplyParam(String, String, Param),
    /// 只在来源周期显示（G3）
    OnlyOrigin(String, String, bool),
    /// 把这个对象的样式存成模板（名字取输入框 `template_key`；G3）
    SaveTemplate(String, String),
    /// 第 i 个模板套到这个对象上
    ApplyTemplate(String, String, usize),
    DeleteTemplate(usize),
    /// 这个对象的样式设为这种工具的缺省 / 清掉这种工具的缺省
    SetDefault(String, String),
    ClearDefault(Kind),
    /// 显示 / 隐藏一类系统对象（G4）
    ShowSource(model::Source, bool),
    /// 系统对象复制成手工对象（之后能拖、能改；G4）
    CopyToManual(String, String),
    /// 斐波那契向右延伸
    Extend(String, String, bool),
    /// 打开检查器的「属性」页（浮动小条的「属性…」）：图表不自动开检查器，免得一选中图就变窄、刚画的东西滑出视野
    OpenInspector,
    /// 对象树里展开 / 折叠一个系统分组
    ToggleGroup(String),
    /// 到价提醒（G5）：设条件（`None` = 关）、只提醒一次、也发 Telegram、重新开启
    SetAlert(String, String, Option<model::AlertWhen>),
    AlertOnce(String, String, bool),
    AlertTelegram(String, String, bool),
    AlertRearm(String, String),
    Delete(String, String),
    /// (品种, 分组, 值)
    GroupHide(String, String, bool),
    GroupLock(String, String, bool),
    Undo(String),
    Redo(String),
}

/// 品种键：同一品种的所有图共用一套画线。
#[must_use]
pub fn symbol_key(ticker: &exchange::Ticker) -> String {
    format!("{}:{}", ticker.exchange, ticker)
}

/// 正在拖的对象。
#[derive(Debug, Clone)]
struct Drag {
    symbol: String,
    orig: Drawing,
    part: Part,
    start: Point,
    current: Drawing,
}

#[derive(Default)]
struct State {
    books: HashMap<String, Book>,
    tool: Option<Kind>,
    /// 要点几下的工具：已经放下的那几个（品种, 锚点）
    placing: Option<(String, Vec<Anchor>)>,
    /// 放的那张图的周期（折线回车结束时记成来源周期）
    placing_tf: Option<String>,
    selected: Option<(String, String)>,
    drag: Option<Drag>,
    /// 最近一条说明（存盘失败、读不了的对象、输入不认得…），显示在工具栏的绘图菜单与检查器
    note: String,
    prefs: Option<Prefs>,
    menu_open: bool,
    /// 浮动条的「⋯」面板开着
    more_open: bool,
    /// 拖浮动条：(按下后第一次移动时的光标, 那时条的位置)；`Some(None)` = 按下了、还没动
    bar_drag: Option<Option<([f32; 2], [f32; 2])>>,
    /// 拖动中的位置（松开才存进偏好，免得每一帧写库）
    bar_live: Option<[f32; 2]>,
    bar_last_press: Option<std::time::Instant>,
    /// 检查器输入框里还没回车的文字：键 → 文字
    edits: HashMap<String, String>,
    /// 对象树里展开了的系统分组（系统分组缺省折叠：日历一组就有两百多个）
    expanded: std::collections::HashSet<String>,
    /// 各品种上一次看到的价（到价提醒判穿越用；G5）
    last_px: HashMap<String, i64>,
    /// 最近响过的提醒（标题, 内容）——检查器里看得到；隔离实例与测试里只记这里、不真的发
    fired: Vec<(String, String)>,
}

static VERSION: AtomicU64 = AtomicU64::new(1);
static INSPECT_REQ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 有没有人要打开检查器（主程序每条消息看一次）。
#[must_use]
pub fn take_inspector_request() -> bool {
    INSPECT_REQ.swap(false, Ordering::Relaxed)
}

fn cell() -> &'static Mutex<State> {
    static C: OnceLock<Mutex<State>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut g)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// 画线的版本号：变了 = 有图要重画绘图层。
#[must_use]
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// 测试不写盘。
fn db() -> Option<Db> {
    if cfg!(test) { None } else { Some(Db::at(&Db::default_path())) }
}

fn book<'a>(st: &'a mut State, symbol: &str) -> &'a mut Book {
    if !st.books.contains_key(symbol) {
        let items = match db() {
            Some(d) => {
                let (items, errs) = d.load(symbol);
                if let Some(e) = errs.first() {
                    st.note = e.clone();
                    log::warn!("[drawing] {e}");
                }
                items
            }
            None => Vec::new(),
        };
        st.books.insert(symbol.to_string(), Book::from_items(items));
    }
    st.books.get_mut(symbol).expect("刚插进去")
}

/// 一批变化：后台写盘、版本号 +1。
fn persist(symbol: &str, fx: Vec<Effect>) {
    if fx.is_empty() {
        return;
    }
    bump();
    if let Some(d) = db() {
        let symbol = symbol.to_string();
        super::spawn_named("ws-drawing-save", move || {
            if let Err(e) = d.write(&symbol, &fx) {
                log::warn!("[drawing] 存盘失败：{e}");
                with(|st| st.note = format!("画线没存上：{e}"));
            }
        });
    }
}

// ── 偏好 ─────────────────────────────────────────────────────────────────────

fn prefs_of(st: &mut State) -> &mut Prefs {
    st.prefs.get_or_insert_with(|| {
        db().and_then(|d| d.get_meta("prefs")).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
    })
}

#[must_use]
pub fn prefs() -> Prefs {
    with(|st| prefs_of(st).clone())
}

fn set_prefs(f: impl FnOnce(&mut Prefs)) {
    let p = with(|st| {
        let p = prefs_of(st);
        f(p);
        p.clone()
    });
    bump();
    if let Some(d) = db() {
        super::spawn_named("ws-drawing-prefs", move || {
            if let Err(e) = d.set_meta("prefs", &serde_json::to_string(&p).unwrap_or_default()) {
                with(|st| st.note = format!("绘图设置没存上：{e}"));
            }
        });
    }
}

// ── 到价提醒（G5）──────────────────────────────────────────────────────────

/// 同一条线两次提醒至少隔这么久（来回抖动时不刷屏）。
const ALERT_COOLDOWN_MS: u64 = 60_000;

/// 图表收到新数据时调：价格从上一次的价走到 `price`（定点），碰到了哪条线的提醒就发。返回响了几个。
/// **只提醒，不下单**。`now`：那根 K 线的时间（毫秒；趋势线、射线按它取价位）。
pub fn on_price(symbol: &str, price: i64, now: u64, decimals: usize) -> usize {
    let fired: Vec<(Drawing, i64, i64)> = with(|st| {
        let Some(prev) = st.last_px.insert(symbol.to_string(), price) else { return Vec::new() };
        if prev == price {
            return Vec::new();
        }
        book(st, symbol)
            .items
            .iter()
            .filter_map(|d| {
                let a = d.alert.as_ref().filter(|a| a.armed)?;
                let cool = a.last_fired.is_none_or(|t| now.saturating_sub(t) >= ALERT_COOLDOWN_MS);
                // 线是一个价位；矩形是上下两条边（进出区域都算）
                let level = d.alert_levels(now).into_iter().find(|l| a.when.hit(prev, price, *l))?;
                cool.then(|| (d.clone(), level, prev))
            })
            .collect()
    });
    for (d, level, prev) in &fired {
        let mut after = d.clone();
        if let Some(a) = after.alert.as_mut() {
            a.last_fired = Some(now);
            if a.once {
                a.armed = false;
            }
        }
        let fx = with(|st| book(st, symbol).replace_silently(after));
        persist(symbol, fx);
        let code = symbol.rsplit(':').next().unwrap_or(symbol);
        let when = d.alert.as_ref().map_or("穿越", |a| a.when.label());
        let title = format!("画线提醒 · {code}");
        let mut body = format!(
            "{} → {} {when} {} {}",
            model::price_text(*prev, decimals),
            model::price_text(price, decimals),
            d.kind.label(),
            model::price_text(*level, decimals)
        );
        if let Some(t) = d.text.as_ref().filter(|t| !t.is_empty()) {
            body.push_str(&format!("（{t}）"));
        }
        body.push_str(" · 只是提醒，不会下单");
        deliver(title, body, d.alert.as_ref().is_some_and(|a| a.telegram));
    }
    fired.len()
}

/// 发出去：通知中心 + 桌面通知（+ Telegram），与金融日历同一个通道。样张、隔离实例（`WS_ISOLATED`）与测试只记下来。
fn deliver(title: String, body: String, telegram: bool) {
    with(|st| {
        st.fired.push((title.clone(), body.clone()));
        if st.fired.len() > 20 {
            st.fired.remove(0);
        }
    });
    if cfg!(test) || super::specimen::enabled() || std::env::var_os("WS_ISOLATED").is_some() {
        return;
    }
    super::spawn_named("ws-drawing-alert", move || {
        let r = super::calendar_alerts::open(&super::calendar_alerts::db_path())
            .and_then(|c| super::calendar_alerts::queue_drawing_alert(&c, chrono::Utc::now().timestamp_millis(), &title, &body, telegram, false));
        if let Err(e) = r {
            with(|st| st.note = format!("画线提醒没发出去：{e}"));
        }
    });
}

/// 最近响过的提醒（新的在后）。
#[must_use]
pub fn fired_log() -> Vec<(String, String)> {
    with(|st| st.fired.clone())
}

/// 对象树里这个系统分组展开了没有。
#[must_use]
pub fn group_expanded(name: &str) -> bool {
    with(|st| st.expanded.contains(name))
}

/// 最近一条说明（空 = 没有）。
#[must_use]
pub fn note() -> String {
    with(|st| st.note.clone())
}

#[must_use]
pub fn menu_open() -> bool {
    with(|st| st.menu_open)
}

/// 浮动条的「⋯」面板开着没有。
#[must_use]
pub fn more_open() -> bool {
    with(|st| st.more_open)
}

/// 正在拖浮动条（main 据此整窗接住鼠标）。
#[must_use]
pub fn bar_dragging() -> bool {
    with(|st| st.bar_drag.is_some())
}

/// 浮动条缺省位置：内容区左上角往里一点。
pub const BAR_HOME: [f32; 2] = [8.0, 8.0];

/// 浮动条现在的位置（拖动中用拖动中的，否则偏好里存的，否则缺省）。
#[must_use]
pub fn bar_at() -> [f32; 2] {
    with(|st| st.bar_live.or_else(|| prefs_of(st).bar_at).unwrap_or(BAR_HOME))
}

/// 这个对象现在能不能拖、能不能删（还要看「全部锁定」）。
fn can_edit(st: &mut State, d: &Drawing) -> bool {
    d.editable() && !prefs_of(st).lock_all
}

/// 在这张图上画不画它：没隐藏、没「全部隐藏」、这一类周期上显示。
fn visible(st: &mut State, d: &Drawing, cat: &str, tf: &str) -> bool {
    !d.hidden && !prefs_of(st).hide_all && d.visible_in(cat, tf)
}

// ── 工具与选中 ───────────────────────────────────────────────────────────────

#[must_use]
pub fn tool() -> Option<Kind> {
    with(|st| st.tool)
}

/// 选工具（`None` = 回到光标）。换工具会放弃放了一半的对象。
pub fn set_tool(kind: Option<Kind>) {
    with(|st| {
        st.tool = kind;
        st.placing = None;
        if kind.is_some() {
            st.selected = None;
        }
    });
    bump();
}

/// 选中的对象 id（只看这个品种的）。
#[must_use]
pub fn selected(symbol: &str) -> Option<String> {
    with(|st| st.selected.as_ref().filter(|(s, _)| s == symbol).map(|(_, id)| id.clone()))
}

pub fn select(symbol: &str, id: Option<String>) {
    let changed = with(|st| {
        let new = id.map(|i| (symbol.to_string(), i));
        std::mem::replace(&mut st.selected, new.clone()) != new
    });
    if changed {
        bump();
    }
}

/// Esc：退出工具、放弃放了一半的对象、取消选中。
pub fn escape() {
    with(|st| {
        st.tool = None;
        st.placing = None;
        st.selected = None;
        st.drag = None;
    });
    bump();
}

/// 这个品种的对象（拖动中的那个换成预览）：手工画的在前，系统生成的（订单、日历、策略、外部；G4）在后。
#[must_use]
pub fn items(symbol: &str) -> Vec<Drawing> {
    let (mut v, hidden) = with(|st| {
        let drag = st.drag.clone().filter(|d| d.symbol == symbol);
        let mut v = book(st, symbol).items.clone();
        if let Some(d) = drag
            && let Some(slot) = v.iter_mut().find(|x| x.id == d.current.id)
        {
            *slot = d.current;
        }
        (v, prefs_of(st).hide_sources.clone())
    });
    v.extend(feeds::items(symbol, &hidden));
    v
}

/// 这个品种有没有成交量分布对象（有才让图表把成交量带进 `DrawView`）。
#[must_use]
pub fn needs_volume(symbol: &str) -> bool {
    items(symbol).iter().any(|d| d.kind == Kind::VolProfile)
}

/// 在这一类周期的图上看得见的对象（画与命中测试用）。
#[must_use]
pub fn visible_items(symbol: &str, cat: &str, tf: &str) -> Vec<Drawing> {
    let all = items(symbol);
    with(|st| all.into_iter().filter(|d| visible(st, d, cat, tf)).collect())
}

/// 选中的对象（拖动中是预览）。
#[must_use]
pub fn selected_drawing(symbol: &str) -> Option<Drawing> {
    let id = selected(symbol)?;
    items(symbol).into_iter().find(|d| d.id == id)
}

/// 价格轴上要标的价位（G1）：看得见的手工水平线，以及选中对象的各个锚点（垂直线除外）。(价格, 颜色)
#[must_use]
pub fn axis_marks(symbol: &str, cat: &str, tf: &str) -> Vec<(i64, iced::Color)> {
    let sel = selected(symbol);
    visible_items(symbol, cat, tf)
        .into_iter()
        .flat_map(|d| {
            let is_sel = sel.as_deref() == Some(d.id.as_str());
            let c = base_color(&d);
            let ps: Vec<(i64, iced::Color)> = match d.kind {
                // 系统对象的水平线不标（研究脚本一写几百条，价格轴就被盖满了——G5 实测），选中了才标
                Kind::HLine if d.source == model::Source::Manual || is_sel => d.anchors.first().map(|a| (a.p, c)).into_iter().collect(),
                Kind::HLine => Vec::new(),
                Kind::VLine => Vec::new(),
                _ if is_sel => d.anchors.iter().map(|a| (a.p, c)).collect(),
                _ => Vec::new(),
            };
            ps
        })
        .collect()
}

/// 光标下有没有对象（悬停时换光标）。
#[must_use]
pub fn hovering(symbol: &str, v: &DrawView, p: Point) -> bool {
    let items = visible_items(symbol, v.cat, &v.tf);
    geometry::pick(v, &items, p, selected(symbol).as_deref()).is_some()
}

// ── 图上的操作 ───────────────────────────────────────────────────────────────

/// 放锚点的位置：吸到 K 线开始与最小价位；开着磁吸再吸到开高低收。
fn anchor_for(v: &DrawView, p: Point) -> Anchor {
    let a = v.anchor_at(p);
    if prefs().magnet { v.magnet(a, p) } else { a }
}

/// 选了工具时在图上点了一下（屏幕位置）。
pub fn place_at(symbol: &str, v: &DrawView, p: Point) -> Option<String> {
    if std::env::var_os("WS_DRAWING_BENCH").is_some() {
        log::info!("[drawing-place] size {:?} scaling {} translation {:?} p {:?} → {:?}", v.size, v.scaling, v.translation, p, v.anchor_at(p));
    }
    place_in(symbol, anchor_for(v, p), Some(v))
}

/// 选了工具时按下左键：一个锚点的工具直接建好；两个锚点的，第一下记起点、第二下建好。返回建好的对象 id。
pub fn place(symbol: &str, at: Anchor) -> Option<String> {
    place_in(symbol, at, None)
}

/// 放一个锚点：点够了就建好（多锚点工具共用这一套）。折线点多少下都行，在最后一个点上再点一下（或回车）结束；
/// 仓位工具点一下，目标与止损按缺省比例摆好、右边界 20 根 K 线（`v` 给了才知道一根多宽）。
fn place_in(symbol: &str, at: Anchor, v: Option<&DrawView>) -> Option<String> {
    let (kind, mut pts) = with(|st| (st.tool, st.placing.clone().filter(|(s, _)| s == symbol).map(|(_, a)| a).unwrap_or_default()));
    let kind = kind?;
    let need = kind.clicks();
    if need == 0 && pts.len() >= 2 && pts.last() == Some(&at) {
        return finish(symbol);
    }
    if need == 0 || pts.len() + 1 < need {
        pts.push(at);
        with(|st| {
            st.placing = Some((symbol.to_string(), pts));
            st.placing_tf = v.map(|v| v.tf.clone());
        });
        bump();
        return None;
    }
    pts.push(at);
    let anchors = match kind {
        Kind::LongPos | Kind::ShortPos => {
            let right = v.map_or(at.t + 20 * 60_000, |v| v.bars_later(at.t, 20.0));
            model::position_anchors(kind, at, right, v.map_or(1, |v| v.snap))
        }
        _ => pts,
    };
    create(symbol, kind, anchors, v.map(|v| v.tf.clone()))
}

/// 折线：回车结束（至少两个点）。
pub fn finish(symbol: &str) -> Option<String> {
    let (kind, pts) = with(|st| (st.tool, st.placing.clone().filter(|(s, _)| s == symbol).map(|(_, a)| a).unwrap_or_default()));
    (kind == Some(Kind::Polyline) && pts.len() >= 2).then_some(())?;
    let tf = with(|st| st.placing_tf.clone());
    create(symbol, Kind::Polyline, pts, tf)
}

/// 建好一个对象：用这种工具的缺省样式（有的话），记下画它的周期。
fn create(symbol: &str, kind: Kind, anchors: Vec<Anchor>, origin_tf: Option<String>) -> Option<String> {
    let mut d = Drawing::new(kind, anchors);
    d.origin_tf = origin_tf;
    if let Some((_, st)) = prefs().defaults.into_iter().find(|(k, _)| *k == kind) {
        d.style = st;
    }
    let id = d.id.clone();
    let fx = with(|st| {
        st.placing = None;
        if !prefs_of(st).continuous {
            st.tool = None; // 画完一个回到光标；连续绘制时留着工具接着画
        }
        st.selected = Some((symbol.to_string(), id.clone()));
        book(st, symbol).exec(Cmd::Create(d))
    });
    persist(symbol, fx);
    Some(id)
}

/// 要点几下的工具已经放下的锚点（画预览用）。
#[must_use]
pub fn placing(symbol: &str) -> Option<(Kind, Vec<Anchor>)> {
    with(|st| match (&st.placing, st.tool) {
        (Some((s, a)), Some(k)) if s == symbol => Some((k, a.clone())),
        _ => None,
    })
}

/// 没选工具时按下左键：点中对象就选中（能拖的开始拖），返回 true；没点中返回 false（图表照常平移），并取消选中。
pub fn press(symbol: &str, v: &DrawView, p: Point) -> bool {
    let all = visible_items(symbol, v.cat, &v.tf);
    let sel = selected(symbol);
    match geometry::pick(v, &all, p, sel.as_deref()) {
        Some((d, part)) => {
            let d = d.clone();
            with(|st| {
                if st.selected.as_ref().map(|(_, i)| i) != Some(&d.id) {
                    st.edits.clear();
                }
                st.selected = Some((symbol.to_string(), d.id.clone()));
                st.drag = can_edit(st, &d).then(|| Drag { symbol: symbol.to_string(), orig: d.clone(), part, start: p, current: d.clone() });
            });
            bump();
            true
        }
        None => {
            select(symbol, None);
            false
        }
    }
}

/// 有没有在拖。
#[must_use]
pub fn dragging() -> bool {
    with(|st| st.drag.is_some())
}

/// 拖动中：整体按屏幕位移挪每个锚点，拖锚点就把那个锚点放到光标处。
pub fn drag_to(v: &DrawView, p: Point) {
    let magnet = prefs().magnet;
    let moved = with(|st| {
        let d = st.drag.as_mut()?;
        let mut cur = d.orig.clone();
        match d.part {
            Part::Body => {
                let delta = p - d.start;
                for a in &mut cur.anchors {
                    *a = v.shift(*a, delta);
                }
            }
            Part::Anchor(i) => {
                if let Some(a) = cur.anchors.get_mut(i) {
                    *a = if magnet { v.magnet(v.anchor_at(p), p) } else { v.anchor_at(p) };
                }
                cur.constrain(i);
            }
        }
        (cur != d.current).then(|| d.current = cur)
    });
    if moved.is_some() {
        bump();
    }
}

/// 松开：记一条「改」（没动就不记）。
pub fn drop_drag() {
    let Some(d) = with(|st| st.drag.take()) else { return };
    let fx = with(|st| book(st, &d.symbol).update(d.current.clone()));
    if fx.is_empty() {
        bump(); // 没动：仍要把预览换回去
    }
    persist(&d.symbol, fx);
}

/// 删除选中的对象（锁定的、系统生成的不删）。
pub fn delete_selected(symbol: &str) -> bool {
    let fx = with(|st| {
        let id = st.selected.as_ref().filter(|(s, _)| s == symbol).map(|(_, id)| id.clone())?;
        let d = book(st, symbol).get(&id).cloned()?;
        if !can_edit(st, &d) {
            st.note = "锁定的、系统生成的对象不能删".into();
            return None;
        }
        st.selected = None;
        Some(book(st, symbol).exec(Cmd::Delete(d)))
    });
    match fx {
        Some(fx) => {
            persist(symbol, fx);
            true
        }
        None => false,
    }
}

pub fn undo(symbol: &str) {
    let fx = with(|st| book(st, symbol).undo());
    with(|st| st.selected = None);
    persist(symbol, fx);
}

pub fn redo(symbol: &str) {
    let fx = with(|st| book(st, symbol).redo());
    persist(symbol, fx);
}

/// 快捷键选工具（G0 的临时入口；G1 工具栏）：Alt+H / V / T / R，Alt+Shift+R 矩形。
#[must_use]
pub fn tool_for_key(c: &str, shift: bool) -> Option<Kind> {
    match (c.to_ascii_lowercase().as_str(), shift) {
        ("h", false) => Some(Kind::HLine),
        ("v", false) => Some(Kind::VLine),
        ("t", false) => Some(Kind::Trend),
        ("r", false) => Some(Kind::Ray),
        ("r", true) => Some(Kind::Rect),
        ("f", false) => Some(Kind::FibRetrace),
        (c, shift) => tools::for_shortcut(c, shift),
    }
}

// ── 工具栏 / 检查器 / 浮动小条的操作 ───────────────────────────────────────────

/// 模板名输入框的键。
#[must_use]
pub fn template_key(id: &str) -> String {
    format!("tpl:{id}")
}

/// 参数输入框的键。
#[must_use]
pub fn param_key(id: &str, p: Param) -> String {
    format!("x:{id}:{p:?}")
}

/// 检查器输入框的键：`a:{id}:{锚点}:p|t`（价格 / 时间）、`g:{id}`（分组）。
#[must_use]
pub fn edit_key(id: &str, anchor: Option<(usize, Field)>) -> String {
    match anchor {
        Some((i, Field::Price)) => format!("a:{id}:{i}:p"),
        Some((i, Field::Time)) => format!("a:{id}:{i}:t"),
        None => format!("g:{id}"),
    }
}

/// 输入框里现在的文字（没改过就是 `default`）。
#[must_use]
pub fn edit_text(key: &str, default: String) -> String {
    with(|st| st.edits.get(key).cloned()).unwrap_or(default)
}

/// 改一个对象（命令「改」，可撤销）。`anchors`：要不要动锚点（锁定、全部锁定时不动）。
fn modify(symbol: &str, id: &str, anchors: bool, f: impl FnOnce(&mut Drawing)) {
    let fx = with(|st| {
        let d = book(st, symbol).get(id).cloned()?;
        if d.source != model::Source::Manual || (anchors && !can_edit(st, &d)) {
            st.note = if anchors { "锁定的对象不能改位置".into() } else { "系统生成的对象只读".into() };
            return None;
        }
        let mut after = d;
        f(&mut after);
        Some(book(st, symbol).update(after))
    });
    if let Some(fx) = fx {
        persist(symbol, fx);
    }
}

/// 处理一条绘图消息。
pub fn handle(m: DrawMsg) {
    match m {
        // 浮动条（§13）选了工具不收起：TradingView 那样一直在手边，点顶部「绘图」或条上的 ✕ 才收
        DrawMsg::Tool(k) => set_tool(k),
        DrawMsg::ToggleMenu => {
            with(|st| {
                st.menu_open = !st.menu_open;
                st.more_open = false;
            });
            bump();
        }
        DrawMsg::CloseMenu => with(|st| {
            st.menu_open = false;
            st.more_open = false;
        }),
        DrawMsg::ToggleMore => with(|st| st.more_open = !st.more_open),
        DrawMsg::BarHorizontal(v) => set_prefs(|p| p.bar_horizontal = v),
        DrawMsg::BarDragStart => {
            let reset = with(|st| {
                let now = std::time::Instant::now();
                let double = st.bar_last_press.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(400));
                st.bar_last_press = (!double).then_some(now);
                st.bar_drag = (!double).then_some(None);
                st.bar_live = None;
                double
            });
            if reset {
                set_prefs(|p| p.bar_at = None);
            }
        }
        DrawMsg::BarDragMove(x, y) => with(|st| {
            let cur = st.bar_live.or_else(|| prefs_of(st).bar_at).unwrap_or(BAR_HOME);
            match st.bar_drag {
                Some(None) => st.bar_drag = Some(Some(([x, y], cur))),
                Some(Some((c0, p0))) => st.bar_live = Some([(p0[0] + x - c0[0]).max(0.0), (p0[1] + y - c0[1]).max(0.0)]),
                None => {}
            }
        }),
        DrawMsg::BarDragEnd => {
            let live = with(|st| {
                st.bar_drag = None;
                st.bar_live.take()
            });
            if let Some(at) = live {
                set_prefs(|p| p.bar_at = Some(at));
            }
        }
        DrawMsg::Favorite(k) => set_prefs(|p| {
            if let Some(i) = p.favorites.iter().position(|x| *x == k) {
                p.favorites.remove(i);
            } else {
                p.favorites.push(k);
            }
        }),
        DrawMsg::Continuous(v) => set_prefs(|p| p.continuous = v),
        DrawMsg::Magnet(v) => set_prefs(|p| p.magnet = v),
        DrawMsg::HideAll(v) => set_prefs(|p| p.hide_all = v),
        DrawMsg::LockAll(v) => set_prefs(|p| p.lock_all = v),
        DrawMsg::Select(sym, id) => {
            with(|st| st.edits.clear());
            select(&sym, id);
        }
        DrawMsg::Color(sym, id, c) => modify(&sym, &id, false, |d| d.style.color = c),
        DrawMsg::Width(sym, id, w) => modify(&sym, &id, false, |d| d.style.width = w),
        DrawMsg::DashStyle(sym, id, s) => modify(&sym, &id, false, |d| d.style.dash = s),
        DrawMsg::Lock(sym, id, v) => modify(&sym, &id, false, |d| d.locked = v),
        DrawMsg::Hide(sym, id, v) => modify(&sym, &id, false, |d| d.hidden = v),
        DrawMsg::ShowOn(sym, id, cat, show) => modify(&sym, &id, false, |d| {
            d.hide_on.retain(|c| *c != cat);
            if !show {
                d.hide_on.push(cat);
            }
        }),
        DrawMsg::Edit(key, text) => with(|st| {
            st.edits.insert(key, text);
        }),
        DrawMsg::ApplyAnchor(sym, id, i, field) => {
            let key = edit_key(&id, Some((i, field)));
            let Some(text) = with(|st| st.edits.get(&key).cloned()) else { return };
            let parsed = match field {
                Field::Price => model::parse_price(&text).map(|p| (Some(p), None)),
                Field::Time => model::parse_time(&text).map(|t| (None, Some(t))),
            };
            match parsed {
                Some((p, t)) => {
                    with(|st| {
                        st.edits.remove(&key);
                        st.note.clear();
                    });
                    modify(&sym, &id, true, |d| {
                        if let Some(a) = d.anchors.get_mut(i) {
                            a.p = p.unwrap_or(a.p);
                            a.t = t.unwrap_or(a.t);
                        }
                    });
                }
                None => with(|st| {
                    st.note = match field {
                        Field::Price => format!("价格认不得：{text}"),
                        Field::Time => format!("时间认不得：{text}（写成 2026-10-15 04:30 或 2026-10-15 04:30:00，UTC）"),
                    }
                }),
            }
        }
        DrawMsg::ApplyGroup(sym, id) => {
            let key = edit_key(&id, None);
            let Some(text) = with(|st| st.edits.remove(&key)) else { return };
            let g = text.trim().to_string();
            modify(&sym, &id, false, |d| d.group = (!g.is_empty()).then_some(g));
        }
        DrawMsg::ApplyParam(sym, id, param) => {
            let key = param_key(&id, param);
            let Some(text) = with(|st| st.edits.get(&key).cloned()) else { return };
            let num = |s: &str| s.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x >= 0.0);
            let ok = match param {
                Param::Text => {
                    modify(&sym, &id, false, |d| d.text = Some(text.clone()));
                    true
                }
                Param::Levels => {
                    let lv: Option<Vec<f64>> = text.split([',', '，', ' ']).filter(|x| !x.trim().is_empty()).map(|x| x.trim().parse::<f64>().ok()).collect();
                    match lv {
                        Some(lv) if !lv.is_empty() => {
                            modify(&sym, &id, false, |d| d.levels = lv);
                            true
                        }
                        _ => false,
                    }
                }
                Param::Qty => num(&text).filter(|q| *q > 0.0).map(|q| modify(&sym, &id, false, |d| d.qty = Some(q))).is_some(),
                Param::Fee => num(&text).map(|f| modify(&sym, &id, false, |d| d.fee_bp = Some(f))).is_some(),
                Param::Rows => num(&text)
                    .filter(|r| (4.0..=200.0).contains(r))
                    .map(|r| modify(&sym, &id, false, |d| d.rows = Some(r.round() as u32)))
                    .is_some(),
                Param::Va => num(&text).filter(|v| *v > 0.0 && *v < 1.0).map(|v| modify(&sym, &id, false, |d| d.va = Some(v))).is_some(),
            };
            with(|st| {
                if ok {
                    st.edits.remove(&key);
                    st.note.clear();
                } else {
                    st.note = format!("认不得：{text}");
                }
            });
        }
        DrawMsg::OnlyOrigin(sym, id, v) => modify(&sym, &id, false, |d| d.only_origin = v),
        DrawMsg::SaveTemplate(sym, id) => {
            let key = template_key(&id);
            let name = with(|st| st.edits.remove(&key)).unwrap_or_default().trim().to_string();
            let Some(d) = items(&sym).into_iter().find(|d| d.id == id) else { return };
            let name = if name.is_empty() { format!("{} {}", d.kind.label(), prefs().templates.iter().filter(|t| t.kind == d.kind).count() + 1) } else { name };
            set_prefs(|p| {
                p.templates.retain(|t| !(t.kind == d.kind && t.name == name)); // 同名覆盖
                p.templates.push(Template { name, kind: d.kind, style: d.style.clone() });
            });
        }
        DrawMsg::ApplyTemplate(sym, id, i) => {
            if let Some(t) = prefs().templates.get(i).cloned() {
                modify(&sym, &id, false, |d| d.style = t.style);
            }
        }
        DrawMsg::DeleteTemplate(i) => set_prefs(|p| {
            if i < p.templates.len() {
                p.templates.remove(i);
            }
        }),
        DrawMsg::SetDefault(sym, id) => {
            if let Some(d) = items(&sym).into_iter().find(|d| d.id == id) {
                set_prefs(|p| {
                    p.defaults.retain(|(k, _)| *k != d.kind);
                    p.defaults.push((d.kind, d.style.clone()));
                });
            }
        }
        DrawMsg::ClearDefault(k) => set_prefs(|p| p.defaults.retain(|(x, _)| *x != k)),
        DrawMsg::SetAlert(sym, id, when) => modify(&sym, &id, false, |d| {
            d.alert = when.map(|w| match d.alert.take() {
                Some(a) => model::Alert { when: w, armed: true, ..a },
                None => model::Alert { when: w, once: false, telegram: false, armed: true, last_fired: None },
            });
        }),
        DrawMsg::AlertOnce(sym, id, v) => modify(&sym, &id, false, |d| {
            if let Some(a) = d.alert.as_mut() {
                a.once = v;
            }
        }),
        DrawMsg::AlertTelegram(sym, id, v) => modify(&sym, &id, false, |d| {
            if let Some(a) = d.alert.as_mut() {
                a.telegram = v;
            }
        }),
        DrawMsg::AlertRearm(sym, id) => modify(&sym, &id, false, |d| {
            if let Some(a) = d.alert.as_mut() {
                a.armed = true;
            }
        }),
        DrawMsg::ToggleGroup(g) => with(|st| {
            if !st.expanded.remove(&g) {
                st.expanded.insert(g);
            }
        }),
        DrawMsg::Extend(sym, id, v) => modify(&sym, &id, false, |d| d.extend = v),
        DrawMsg::OpenInspector => INSPECT_REQ.store(true, Ordering::Relaxed),
        DrawMsg::ShowSource(src, show) => set_prefs(|p| {
            p.hide_sources.retain(|s| *s != src);
            if !show {
                p.hide_sources.push(src);
            }
        }),
        DrawMsg::CopyToManual(sym, id) => {
            let Some(orig) = items(&sym).into_iter().find(|d| d.id == id) else { return };
            let mut d = Drawing::new(orig.kind, orig.anchors.clone());
            let new_id = d.id.clone();
            d.style = orig.style.clone();
            d.style.color = color_bytes(base_color(&orig)); // 主题色落成具体颜色：复制出来的就是自己的了
            (d.text, d.levels, d.dir, d.rows, d.va, d.qty, d.fee_bp) = (orig.text, orig.levels, orig.dir, orig.rows, orig.va, orig.qty, orig.fee_bp);
            d.group = Some(format!("复制自{}", orig.source.label()));
            let fx = with(|st| {
                st.selected = Some((sym.clone(), new_id));
                book(st, &sym).exec(Cmd::Create(d))
            });
            persist(&sym, fx);
        }
        DrawMsg::Delete(sym, id) => {
            select(&sym, Some(id));
            delete_selected(&sym);
        }
        DrawMsg::GroupHide(sym, g, v) => group_set(&sym, &g, |d| d.hidden = v),
        DrawMsg::GroupLock(sym, g, v) => group_set(&sym, &g, |d| d.locked = v),
        DrawMsg::Undo(sym) => undo(&sym),
        DrawMsg::Redo(sym) => redo(&sym),
    }
}

fn color_bytes(c: iced::Color) -> [u8; 4] {
    let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(c.r), b(c.g), b(c.b), b(c.a)]
}

/// 整组改（每个对象一条「改」）。
fn group_set(symbol: &str, group: &str, f: impl Fn(&mut Drawing)) {
    let ids: Vec<String> = items(symbol).into_iter().filter(|d| d.group.as_deref() == Some(group)).map(|d| d.id).collect();
    for id in ids {
        modify(symbol, &id, false, &f);
    }
}

// ── 画 ──────────────────────────────────────────────────────────────────────

/// 对象的颜色：设了主题色（系统对象）就用主题色——跟着主题与「红涨绿跌」走；否则用样式里的颜色。
#[must_use]
pub fn base_color(d: &Drawing) -> iced::Color {
    use model::Tone;
    match d.tone {
        Some(Tone::Up) => crate::ui::chart::up_strong(),
        Some(Tone::Down) => crate::ui::chart::down_strong(),
        Some(Tone::Warn) => crate::ui::widgets::Tone::Warning.color(),
        Some(Tone::Danger) => crate::ui::widgets::Tone::Danger.color(),
        Some(Tone::Accent) => crate::ui::pal::accent(),
        Some(Tone::Dim) => crate::ui::pal::dim(),
        None => crate::ui::chart::drawing_color(d.style.color),
    }
}

fn stroke(d: &Drawing, emphasis: bool) -> Stroke<'static> {
    let color = base_color(d);
    let width = d.style.width + if emphasis { 0.5 } else { 0.0 };
    let segments: &'static [f32] = match d.style.dash {
        Dash::Solid => &[],
        Dash::Dashed => &[6.0, 4.0],
        Dash::Dotted => &[1.5, 3.0],
    };
    Stroke { width, line_dash: LineDash { segments, offset: 0 }, ..Stroke::default() }.with_color(color)
}

fn fill(d: &Drawing, role: Role) -> iced::Color {
    use crate::ui::pal::alpha;
    match role {
        Role::Up => alpha(crate::ui::chart::up(), 0.18),
        Role::Down => alpha(crate::ui::chart::down(), 0.18),
        Role::Solid => base_color(d),
        Role::Faint => alpha(base_color(d), 0.07),
        _ if d.tone.is_some() => alpha(base_color(d), 0.15),
        _ => d.style.fill.map_or_else(|| alpha(base_color(d), 0.15), crate::ui::chart::drawing_color),
    }
}

fn draw_one(frame: &mut Frame, v: &DrawView, d: &Drawing, selected: bool) {
    draw_one_with(frame, v, d, selected, false);
}

/// `quiet_labels`：这个对象的文字不写（日历竖线挨得太近时，线照画、字只写前一个）。
fn draw_one_with(frame: &mut Frame, v: &DrawView, d: &Drawing, selected: bool, quiet_labels: bool) {
    if d.hidden {
        return;
    }
    let color = base_color(d);
    let aux = Stroke { width: 1.0, line_dash: LineDash { segments: &[4.0, 3.0], offset: 0 }, ..Stroke::default() }.with_color(crate::ui::pal::alpha(color, 0.8));
    for pr in geometry::prims(v, d) {
        match pr {
            Prim::Line(a, b, Role::Aux) => frame.stroke(&Path::line(a, b), aux),
            Prim::Line(a, b, _) => frame.stroke(&Path::line(a, b), stroke(d, selected)),
            Prim::Area(r, role) => frame.fill_rectangle(r.position(), r.size(), fill(d, role)),
            Prim::Frame(r) => frame.stroke(&Path::rectangle(r.position(), r.size()), stroke(d, selected)),
            Prim::Poly(pts, role) => {
                if let Some((first, rest)) = pts.split_first() {
                    let path = Path::new(|b| {
                        b.move_to(*first);
                        for p in rest {
                            b.line_to(*p);
                        }
                        b.close();
                    });
                    frame.fill(&path, fill(d, role));
                }
            }
            Prim::Label { .. } if quiet_labels => {}
            Prim::Label { at, text, align, boxed } => {
                let r = geometry::label_rect(at, &text, align);
                if boxed {
                    frame.fill_rectangle(r.position(), r.size(), crate::ui::chart::drawing_handle_fill());
                    frame.stroke(&Path::rectangle(r.position(), r.size()), Stroke::default().with_width(1.0).with_color(color));
                }
                frame.fill_text(canvas::Text {
                    content: text,
                    position: Point::new(r.x + 4.0, at.y),
                    size: iced::Pixels(crate::ui::text::s_small()),
                    color: if boxed { crate::ui::pal::txt() } else { color },
                    align_y: iced::alignment::Vertical::Center,
                    ..canvas::Text::default()
                });
            }
        }
    }
    // 到价提醒开着（G5）：第一个锚点旁标「提醒 · 上穿」
    if let (Some(al), Some(a)) = (d.alert.as_ref().filter(|a| a.armed), d.anchors.first()) {
        let at = v.to_screen(*a);
        let x = if d.kind == Kind::HLine { 4.0 } else { at.x + 6.0 };
        frame.fill_text(canvas::Text {
            content: format!("提醒 · {}", al.when.label()),
            position: Point::new(x, at.y + 4.0),
            size: iced::Pixels(crate::ui::text::s_meta()),
            color: crate::ui::widgets::Tone::Warning.color(),
            align_y: iced::alignment::Vertical::Top,
            ..canvas::Text::default()
        });
    }
    // 来源周期（G3）：在别的周期上看时，线头标一个小字（如「15m」）
    // 标在第一个看得见的锚点旁；都在视野外时，水平线标在左边缘（它横贯整张图）
    let inside = |p: &Point| p.x >= 0.0 && p.x <= v.size.width - 30.0 && p.y >= 12.0 && p.y <= v.size.height;
    let spot = d.anchors.iter().map(|a| v.to_screen(*a)).find(inside).or_else(|| {
        (d.kind == Kind::HLine).then(|| d.anchors.first().map(|a| Point::new(2.0, v.to_screen(*a).y))).flatten()
    });
    if let (Some(tag), Some(at)) = (d.origin_tag(&v.tf), spot) {
        frame.fill_text(canvas::Text {
            content: tag.to_string(),
            position: Point::new(at.x + 6.0, at.y - 6.0),
            size: iced::Pixels(crate::ui::text::s_meta()),
            color: crate::ui::pal::alpha(color, 0.75),
            align_y: iced::alignment::Vertical::Bottom,
            ..canvas::Text::default()
        });
    }
    if selected {
        let edge = base_color(d);
        for a in &d.anchors {
            let c = v.to_screen(*a);
            let dot = Path::circle(c, geometry::HANDLE_PX);
            frame.fill(&dot, crate::ui::chart::drawing_handle_fill());
            frame.stroke(&dot, Stroke::default().with_width(1.5).with_color(edge));
        }
    }
}

/// 绘图层：这个品种的全部对象（屏幕坐标，不套图表的平移缩放变换）。
pub fn draw_layer(frame: &mut Frame, v: &DrawView, symbol: &str) {
    let t0 = std::time::Instant::now();
    let sel = selected(symbol);
    let items = visible_items(symbol, v.cat, &v.tf);
    // 裁到画布里：画布不会自己裁，锚点在视野外的文字会越界画到旁边的面板上（G2 实测）
    // 垂直线顶上的文字（日历事件名）挨太近就只写前一个——从左往右挑
    let mut tops: Vec<(f32, usize, iced::Rectangle)> = items
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind == Kind::VLine)
        .filter_map(|(i, d)| {
            geometry::prims(v, d).into_iter().find_map(|p| match p {
                Prim::Label { at, text, align, .. } => Some((at.x, i, geometry::label_rect(at, &text, align))),
                _ => None,
            })
        })
        .collect();
    tops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let keep = geometry::keep_apart(&tops.iter().map(|t| t.2).collect::<Vec<_>>());
    let quiet: std::collections::HashSet<usize> = tops.iter().zip(keep).filter(|(_, k)| !k).map(|(t, _)| t.1).collect();
    frame.with_clip(iced::Rectangle::with_size(v.size), |f| {
        for (i, d) in items.iter().enumerate() {
            draw_one_with(f, v, d, sel.as_deref() == Some(d.id.as_str()), quiet.contains(&i));
        }
    });
    bench_note(items.len(), t0.elapsed());
}

/// 性能自测（G5）：设了 `WS_DRAWING_BENCH` 时，每 60 次重画在日志里记一行「对象数 · 中位 / p95 / 最大毫秒」。
fn bench_note(n: usize, dt: std::time::Duration) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("WS_DRAWING_BENCH").is_some()) {
        return;
    }
    static SAMPLES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
    let Ok(mut g) = SAMPLES.lock() else { return };
    g.push(dt.as_secs_f64() * 1000.0);
    if g.len() >= 60 {
        g.sort_by(f64::total_cmp);
        let q = |f: f64| g[((g.len() - 1) as f64 * f) as usize];
        log::info!("[drawing-bench] 对象 {n} · 绘图层 中位 {:.2} / p95 {:.2} / 最大 {:.2} ms", q(0.5), q(0.95), q(1.0));
        g.clear();
    }
}

/// 十字线层：两个锚点的工具放下第一个点之后，跟着光标画预览；选着工具时左下角写一行提示。
pub fn draw_hint(frame: &mut Frame, v: &DrawView, symbol: &str, cursor: Option<Point>) {
    let placed = placing(symbol);
    if let (Some((kind, pts)), Some(c)) = (&placed, cursor) {
        let mut all = pts.clone();
        all.push(v.anchor_at(c));
        let mut d = Drawing::new(*kind, all);
        d.style.dash = Dash::Dashed;
        frame.with_clip(iced::Rectangle::with_size(v.size), |f| draw_one(f, v, &d, false));
    }
    if let Some(kind) = tool() {
        let n = placed.as_ref().map_or(0, |(_, p)| p.len());
        let step = match kind.clicks() {
            0 if n < 2 => "点下第一个、第二个顶点".to_string(),
            0 => "接着点顶点 · 回车或在最后一点上再点一下结束".to_string(),
            1 => "点一下放下".to_string(),
            k => format!("点第 {} 下（共 {k} 下）", n + 1),
        };
        frame.fill_text(canvas::Text {
            content: format!("画{} · {step} · Esc 退出", kind.label()),
            // 左下角：左上角是 OHLC 读数与日历标记
            position: Point::new(8.0, v.size.height - 8.0),
            align_y: iced::alignment::Vertical::Bottom,
            size: iced::Pixels(crate::ui::text::s_small()),
            color: crate::ui::pal::accent(),
            ..canvas::Text::default()
        });
    }
}

/// 绘图层要不要重画：版本号变了。`seen` 是这张图上次画的版本。
#[must_use]
pub fn changed_since(seen: &AtomicU64) -> bool {
    // 系统对象（订单、日历、策略、外部文件）变了也要重画：指纹并进来
    let v = version() ^ feeds::fingerprint().rotate_left(17);
    seen.swap(v, Ordering::Relaxed) != v
}

/// 画布上没有可用尺寸时不画（热力图 / K 线还没拿到数据）。
#[must_use]
pub fn usable(size: Size) -> bool {
    size.width > 1.0 && size.height > 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use geometry::XMode;
    use iced::Vector;

    fn view() -> DrawView {
        DrawView {
            size: Size::new(800.0, 400.0),
            scaling: 1.0,
            translation: Vector::new(0.0, 0.0),
            x: XMode::Time { latest: 600_000, interval_ms: 60_000, cell_w: 10.0 },
            base: 10_000,
            tick: 10,
            cell_h: 4.0,
            snap: 10,
            cat: "min",
            tf: "1m".into(),
            bars: Vec::new(),
            decimals: 1,
            multiplier: 1.0,
            vol: Vec::new(),
        }
    }

    /// 全局状态：各测试用不同的品种键，互不干扰（工具 / 选中是全局的，这里串在一个测试里走完）。
    #[test]
    fn place_select_drag_delete_undo_on_one_symbol() {
        let sym = "TEST:G0";
        let v = view();
        // 趋势线：两下
        set_tool(Some(Kind::Trend));
        assert_eq!(place(sym, v.anchor_at(Point::new(390.0, 200.0))), None);
        assert_eq!(placing(sym).map(|(k, _)| k), Some(Kind::Trend));
        let id = place(sym, v.anchor_at(Point::new(400.0, 220.0))).expect("第二下建好");
        assert_eq!((tool(), selected(sym).as_deref()), (None, Some(id.as_str())), "画完回到光标、选中新对象");
        let before = items(sym)[0].clone();
        // 拖整体：右移一根、下移 5 格
        assert!(press(sym, &v, Point::new(395.0, 210.0)));
        drag_to(&v, Point::new(405.0, 230.0));
        assert_eq!(items(sym)[0].anchors[0], Anchor { t: 600_000, p: 9_950 }, "拖动中看到预览");
        drop_drag();
        let moved = items(sym)[0].clone();
        assert_eq!(moved.anchors[1], Anchor { t: 660_000, p: 9_900 });
        // 拖锚点
        assert!(press(sym, &v, Point::new(410.0, 240.0)));
        drag_to(&v, Point::new(430.0, 200.0));
        drop_drag();
        assert_eq!(items(sym)[0].anchors[1], Anchor { t: 780_000, p: 10_000 });
        // 撤销两步回到画好时的样子；重做一步
        undo(sym);
        undo(sym);
        assert_eq!(items(sym)[0], before);
        redo(sym);
        assert_eq!(items(sym)[0], moved);
        // 点空白：取消选中，不吃掉事件（图表照常平移）
        assert!(!press(sym, &v, Point::new(100.0, 50.0)));
        assert_eq!(selected(sym), None);
        // 锁定的删不掉
        with(|st| book(st, sym).items[0].locked = true);
        assert!(press(sym, &v, Point::new(405.0, 230.0)));
        assert!(!dragging(), "锁定的能选中、不能拖");
        assert!(!delete_selected(sym));
        with(|st| book(st, sym).items[0].locked = false);
        assert!(delete_selected(sym));
        assert!(items(sym).is_empty());
        undo(sym);
        assert_eq!(items(sym).len(), 1);
        // 水平线一下就建好；Esc 退出工具
        set_tool(Some(Kind::HLine));
        assert!(place(sym, Anchor { t: 1, p: 9_000 }).is_some());
        set_tool(Some(Kind::Rect));
        escape();
        assert_eq!(tool(), None);
        assert_eq!(tool_for_key("R", true), Some(Kind::Rect));
        assert_eq!(tool_for_key("x", false), None);

        // ── G1（偏好是全局的，接在同一个测试里顺序走，免得并行的测试互相干扰）──
        let sym = "TEST:G1";
        // 连续绘制：画完不回到光标
        handle(DrawMsg::Continuous(true));
        handle(DrawMsg::Tool(Some(Kind::HLine)));
        place(sym, Anchor { t: 1, p: 9_000 });
        place(sym, Anchor { t: 1, p: 9_100 });
        assert_eq!((tool(), items(sym).len()), (Some(Kind::HLine), 2));
        handle(DrawMsg::Continuous(false));
        handle(DrawMsg::Tool(None));
        let id = items(sym)[0].id.clone();
        // 精确价格：回车写进锚点；认不得的不改并说明
        handle(DrawMsg::Edit(edit_key(&id, Some((0, Field::Price))), "0.0000009".into()));
        handle(DrawMsg::ApplyAnchor(sym.into(), id.clone(), 0, Field::Price));
        assert_eq!(items(sym)[0].anchors[0].p, 90_000);
        handle(DrawMsg::Edit(edit_key(&id, Some((0, Field::Price))), "九块".into()));
        handle(DrawMsg::ApplyAnchor(sym.into(), id.clone(), 0, Field::Price));
        assert!(note().contains("认不得") && items(sym)[0].anchors[0].p == 90_000);
        // 颜色、分组、整组隐藏；撤销一步
        handle(DrawMsg::Color(sym.into(), id.clone(), [1, 2, 3, 255]));
        handle(DrawMsg::Edit(edit_key(&id, None), " 支撑 ".into()));
        handle(DrawMsg::ApplyGroup(sym.into(), id.clone()));
        assert_eq!((items(sym)[0].style.color, items(sym)[0].group.as_deref()), ([1, 2, 3, 255], Some("支撑")));
        handle(DrawMsg::GroupHide(sym.into(), "支撑".into(), true));
        assert!(items(sym)[0].hidden && visible_items(sym, "min", "1m").len() == 1);
        handle(DrawMsg::Undo(sym.into()));
        assert!(!items(sym)[0].hidden);
        // 只在小时级显示：分钟图上看不见、点不中
        handle(DrawMsg::ShowOn(sym.into(), id.clone(), "min".into(), false));
        assert_eq!(visible_items(sym, "min", "1m").len(), 1);
        assert_eq!(visible_items(sym, "hour", "1h").len(), 2);
        // 全部锁定：能选中，不能拖、不能删、不能改位置；颜色照改
        handle(DrawMsg::LockAll(true));
        let other = items(sym)[1].id.clone();
        select(sym, Some(other.clone()));
        assert!(!delete_selected(sym));
        handle(DrawMsg::Edit(edit_key(&other, Some((0, Field::Price))), "1".into()));
        handle(DrawMsg::ApplyAnchor(sym.into(), other.clone(), 0, Field::Price));
        assert_eq!(items(sym)[1].anchors[0].p, 9_100);
        handle(DrawMsg::Color(sym.into(), other.clone(), [9, 9, 9, 255]));
        assert_eq!(items(sym)[1].style.color, [9, 9, 9, 255]);
        handle(DrawMsg::LockAll(false));
        // 全部隐藏：一个都不画
        handle(DrawMsg::HideAll(true));
        assert!(visible_items(sym, "hour", "1h").is_empty());
        handle(DrawMsg::HideAll(false));
        // 收藏：点一下加、再点一下去
        let before = prefs().favorites.len();
        handle(DrawMsg::Favorite(Kind::Rect));
        handle(DrawMsg::Favorite(Kind::Rect));
        assert_eq!(prefs().favorites.len(), before);
        // 价格轴：水平线的价位都标；选中的趋势线标两端
        assert_eq!(axis_marks(sym, "hour", "1h").len(), 2);

        // ── G2：多锚点工具共用一套放置与编辑 ──
        let sym = "TEST:G2";
        let v = view();
        // 平行通道：点三下
        handle(DrawMsg::Tool(Some(Kind::Channel)));
        assert!(place_at(sym, &v, Point::new(390.0, 200.0)).is_none());
        assert!(place_at(sym, &v, Point::new(400.0, 200.0)).is_none());
        assert_eq!(placing(sym).map(|(_, p)| p.len()), Some(2));
        let ch = place_at(sym, &v, Point::new(400.0, 220.0)).expect("第三下建好");
        assert_eq!(items(sym).iter().find(|d| d.id == ch).map(|d| d.anchors.len()), Some(3));
        // 折线：点几下都行；在最后一个点上再点一下结束
        handle(DrawMsg::Tool(Some(Kind::Polyline)));
        for x in [300.0, 320.0, 340.0] {
            assert!(place_at(sym, &v, Point::new(x, 200.0)).is_none());
        }
        let pl = place_at(sym, &v, Point::new(340.0, 200.0)).expect("在最后一点上再点一下结束");
        assert_eq!(items(sym).iter().find(|d| d.id == pl).map(|d| d.anchors.len()), Some(3));
        // 回车也能结束；只有一个点时回车不建
        handle(DrawMsg::Tool(Some(Kind::Polyline)));
        place_at(sym, &v, Point::new(300.0, 210.0));
        assert!(finish(sym).is_none());
        place_at(sym, &v, Point::new(310.0, 210.0));
        assert!(finish(sym).is_some());
        // 仓位：点一下就放好三个价位，右边界 20 根之后；拖止损，目标的右边界跟着走
        handle(DrawMsg::Tool(Some(Kind::LongPos)));
        let pos = place_at(sym, &v, Point::new(390.0, 200.0)).expect("点一下");
        let d = items(sym).into_iter().find(|d| d.id == pos).unwrap();
        assert_eq!((d.anchors.len(), d.anchors[1].t, d.anchors[1].p > d.anchors[0].p, d.anchors[2].p < d.anchors[0].p), (3, 1_740_000, true, true));
        select(sym, Some(pos.clone()));
        let stop = v.to_screen(d.anchors[2]);
        assert!(press(sym, &v, stop));
        drag_to(&v, Point::new(stop.x + 50.0, stop.y));
        drop_drag();
        let d = items(sym).into_iter().find(|d| d.id == pos).unwrap();
        assert_eq!(d.anchors[1].t, d.anchors[2].t, "目标与止损同一个右边界");
        // 参数：文字、斐波那契比例、仓位数量；认不得的不改
        handle(DrawMsg::Edit(param_key(&pos, Param::Qty), "3".into()));
        handle(DrawMsg::ApplyParam(sym.into(), pos.clone(), Param::Qty));
        handle(DrawMsg::Edit(param_key(&pos, Param::Fee), "负数".into()));
        handle(DrawMsg::ApplyParam(sym.into(), pos.clone(), Param::Fee));
        let d = items(sym).into_iter().find(|d| d.id == pos).unwrap();
        assert_eq!((d.qty, d.fee_bp), (Some(3.0), None));
        handle(DrawMsg::Tool(Some(Kind::FibRetrace)));
        place_at(sym, &v, Point::new(390.0, 200.0));
        let fib = place_at(sym, &v, Point::new(400.0, 240.0)).unwrap();
        handle(DrawMsg::Edit(param_key(&fib, Param::Levels), "0, 0.5，1".into()));
        handle(DrawMsg::ApplyParam(sym.into(), fib.clone(), Param::Levels));
        assert_eq!(items(sym).into_iter().find(|d| d.id == fib).unwrap().levels, vec![0.0, 0.5, 1.0]);
        assert_eq!(tool_for_key("f", false), Some(Kind::FibRetrace));

        // ── G3：来源周期、只在来源周期显示、样式模板与缺省 ──
        let sym = "TEST:G3";
        let v15 = DrawView { tf: "15m".into(), ..view() };
        let v1 = view(); // 1m
        handle(DrawMsg::Tool(Some(Kind::HLine)));
        let id = place_at(sym, &v15, Point::new(390.0, 200.0)).unwrap();
        let d = items(sym)[0].clone();
        assert_eq!((d.origin_tf.as_deref(), d.origin_tag("1m"), d.origin_tag("15m")), (Some("15m"), Some("15m"), None));
        assert_eq!((visible_items(sym, "min", "1m").len(), visible_items(sym, "min", "15m").len()), (1, 1), "缺省处处显示");
        handle(DrawMsg::OnlyOrigin(sym.into(), id.clone(), true));
        assert_eq!((visible_items(sym, "min", "1m").len(), visible_items(sym, "min", "15m").len()), (0, 1));
        assert!(!press(sym, &v1, Point::new(100.0, 200.0)), "1m 上看不见也点不中");
        handle(DrawMsg::OnlyOrigin(sym.into(), id.clone(), false));
        // 模板：存、套用；同名覆盖
        handle(DrawMsg::Color(sym.into(), id.clone(), [10, 20, 30, 255]));
        handle(DrawMsg::Edit(template_key(&id), "支撑蓝".into()));
        handle(DrawMsg::SaveTemplate(sym.into(), id.clone()));
        handle(DrawMsg::Edit(template_key(&id), "支撑蓝".into()));
        handle(DrawMsg::SaveTemplate(sym.into(), id.clone()));
        let tpls: Vec<_> = prefs().templates.into_iter().filter(|t| t.name == "支撑蓝").collect();
        assert_eq!((tpls.len(), tpls[0].kind, tpls[0].style.color), (1, Kind::HLine, [10, 20, 30, 255]));
        handle(DrawMsg::Color(sym.into(), id.clone(), [0, 0, 0, 255]));
        let i = prefs().templates.iter().position(|t| t.name == "支撑蓝").unwrap();
        handle(DrawMsg::ApplyTemplate(sym.into(), id.clone(), i));
        assert_eq!(items(sym)[0].style.color, [10, 20, 30, 255]);
        handle(DrawMsg::DeleteTemplate(i));
        assert!(prefs().templates.iter().all(|t| t.name != "支撑蓝"));
        // 缺省样式：新画的水平线直接用它；清掉之后回到出厂样式
        handle(DrawMsg::SetDefault(sym.into(), id.clone()));
        handle(DrawMsg::Tool(Some(Kind::HLine)));
        let id2 = place_at(sym, &v1, Point::new(390.0, 220.0)).unwrap();
        assert_eq!(items(sym).iter().find(|d| d.id == id2).unwrap().style.color, [10, 20, 30, 255]);
        handle(DrawMsg::ClearDefault(Kind::HLine));
        handle(DrawMsg::Tool(Some(Kind::HLine)));
        let id3 = place_at(sym, &v1, Point::new(390.0, 240.0)).unwrap();
        assert_eq!(items(sym).iter().find(|d| d.id == id3).unwrap().style, model::Style::default());

        // ── G4：系统对象只读、可复制成手工对象、按来源隐藏 ──
        let sym = "TEST:G4";
        let mut sys_obj = Drawing::new(Kind::HLine, vec![Anchor { t: 0, p: 10_000 }]);
        sys_obj.id = "x:test:1".into();
        sys_obj.source = model::Source::Research;
        sys_obj.tone = Some(model::Tone::Up);
        sys_obj.text = Some("研究支撑".into());
        feeds::inject(sym, sys_obj);
        assert_eq!(items(sym).len(), 1);
        assert!(press(sym, &v1, Point::new(100.0, 200.0)), "系统对象点得中");
        assert!(!dragging() && !delete_selected(sym), "但拖不动、删不掉");
        handle(DrawMsg::Color(sym.into(), "x:test:1".into(), [1, 1, 1, 255]));
        assert_eq!(items(sym)[0].style.color, model::Style::default().color, "也改不了");
        handle(DrawMsg::CopyToManual(sym.into(), "x:test:1".into()));
        let copy = items(sym).into_iter().find(|d| d.source == model::Source::Manual).expect("复制出一个手工对象");
        assert!(copy.editable() && copy.text.as_deref() == Some("研究支撑") && copy.group.as_deref() == Some("复制自研究"));
        assert_eq!(copy.style.color, color_bytes(crate::ui::chart::up_strong()), "主题色落成具体颜色");
        assert_eq!(selected(sym).as_deref(), Some(copy.id.as_str()));
        handle(DrawMsg::ShowSource(model::Source::Research, false));
        assert_eq!(items(sym).len(), 1, "隐藏「研究」来源：只剩复制出来的手工对象");
        handle(DrawMsg::ShowSource(model::Source::Research, true));
        assert_eq!(items(sym).len(), 2);
        assert!(!needs_volume(sym));

        // ── G5：到价提醒（只记下来，测试里不真的发）──
        let sym = "TEST:G5";
        handle(DrawMsg::Tool(Some(Kind::HLine)));
        let id = place(sym, Anchor { t: 0, p: 10_000 }).unwrap();
        handle(DrawMsg::SetAlert(sym.into(), id.clone(), Some(model::AlertWhen::Up)));
        handle(DrawMsg::AlertOnce(sym.into(), id.clone(), true));
        let before = fired_log().len();
        assert_eq!(on_price(sym, 9_990, 1_000, 1), 0, "第一次只记价");
        assert_eq!(on_price(sym, 9_995, 2_000, 1), 0, "没穿过");
        assert_eq!(on_price(sym, 10_010, 3_000, 1), 1, "上穿");
        let log = fired_log();
        assert_eq!(log.len(), before + 1);
        assert!(log.last().unwrap().1.contains("上穿 水平线") && log.last().unwrap().1.contains("不会下单"));
        let a = items(sym)[0].alert.clone().unwrap();
        assert_eq!((a.armed, a.last_fired), (false, Some(3_000)), "只提醒一次：响过就停");
        assert_eq!(on_price(sym, 9_990, 4_000, 1) + on_price(sym, 10_020, 5_000, 1), 0, "停了就不再响");
        // 重新开启；一分钟内同一条线不重复
        handle(DrawMsg::AlertRearm(sym.into(), id.clone()));
        handle(DrawMsg::AlertOnce(sym.into(), id.clone(), false));
        on_price(sym, 9_990, 6_000, 1);
        assert_eq!(on_price(sym, 10_010, 7_000, 1), 0, "离上次不到一分钟");
        on_price(sym, 9_990, 70_000, 1);
        assert_eq!(on_price(sym, 10_010, 70_500, 1), 1);
        // 提醒响了改的状态不进撤销栈：撤销一步撤的是用户最后那次「只提醒一次」的改动
        undo(sym);
        assert!(items(sym)[0].alert.as_ref().is_some_and(|a| a.once), "撤回到勾着「只提醒一次」");
        handle(DrawMsg::SetAlert(sym.into(), id.clone(), None));
        assert!(items(sym)[0].alert.is_none());
        // 矩形：价格进出区域（上下两条边）都算；区间外的时刻不看
        let sym = "TEST:G5R";
        handle(DrawMsg::Tool(Some(Kind::Rect)));
        place(sym, Anchor { t: 0, p: 9_900 });
        let rid = place(sym, Anchor { t: 1_000_000, p: 10_100 }).unwrap();
        handle(DrawMsg::SetAlert(sym.into(), rid.clone(), Some(model::AlertWhen::Cross)));
        on_price(sym, 9_800, 1_000, 1);
        assert_eq!(on_price(sym, 9_950, 2_000, 1), 1, "从下面进入区域");
        assert_eq!(on_price(sym, 10_000, 100_000, 1), 0, "在区域里走不算");
        assert_eq!(on_price(sym, 10_200, 200_000, 1), 1, "从上面出去");
        assert!(fired_log().last().unwrap().1.contains("穿越 矩形"));
        on_price(sym, 9_800, 2_000_000, 1);
        assert_eq!(on_price(sym, 10_000, 2_100_000, 1), 0, "矩形的时间区间之后不看");
    }

    /// 性能（G5）：2000 个对象的取对象、图元、命中测试。`cargo test --release perf_ -- --ignored --nocapture`
    #[test]
    #[ignore = "性能自测，手动跑"]
    fn perf_two_thousand_objects() {
        let sym = "TEST:PERF";
        let v = view();
        let kinds = [Kind::HLine, Kind::Trend, Kind::Rect, Kind::FibRetrace, Kind::Channel, Kind::Text, Kind::PriceRange, Kind::Ray];
        with(|st| {
            let b = book(st, sym);
            for i in 0..2000_i64 {
                let k = kinds[i as usize % kinds.len()];
                let a = Anchor { t: 540_000 - (i as u64 % 50) * 60_000, p: 9_000 + (i * 7) % 2_000 };
                let c = Anchor { t: a.t + 120_000, p: a.p + 50 };
                b.exec(Cmd::Create(Drawing::new(k, vec![a, c, Anchor { t: c.t, p: a.p - 30 }])));
            }
        });
        let t = std::time::Instant::now();
        let n = 200;
        let mut prims = 0;
        for _ in 0..n {
            let items = visible_items(sym, v.cat, &v.tf);
            prims += items.iter().map(|d| geometry::prims(&v, d).len()).sum::<usize>();
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        let t = std::time::Instant::now();
        for i in 0..n {
            let items = visible_items(sym, v.cat, &v.tf);
            let _ = geometry::pick(&v, &items, Point::new(100.0 + i as f32, 200.0), None);
        }
        let pick = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
        println!("2000 个对象：取对象 + 图元 {per:.2} ms / 次（{} 个图元）；取对象 + 命中测试 {pick:.2} ms / 次", prims / n as usize);
        assert!(per < 16.0 && pick < 16.0, "一帧之内");
    }
}
