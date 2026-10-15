//! 图表绘图（docs/46）：TradingView 式画线。这里是引擎与图表之间的那一层——当前工具、选中、
//! 正在拖的预览、按品种的对象集合（懒加载、改了就后台写盘），以及在画布上怎么画。
//!
//! - 模型 [`model`]、坐标与命中测试 [`geometry`]、命令与存盘 [`store`] 都是纯的，可单测；
//! - 图表（`chart.rs` 的 `canvas_interaction`、`kline.rs` / `heatmap.rs` 的 `draw`）只把事件交过来、
//!   在单独一层缓存里调 [`draw_layer`]；
//! - **画线跟着品种走**：键是「交易所 市场:代码」，同一品种的所有面板、所有周期共用一套；
//!   每次改动版本号 +1，各图在下一帧看到版本变了就重画自己那一层。

pub mod geometry;
pub mod model;
pub mod store;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use iced::widget::canvas::{self, Frame, LineDash, Path, Stroke};
use iced::{Point, Size};

use serde::{Deserialize, Serialize};

use geometry::{DrawView, Part, Shape};
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
}

impl Default for Prefs {
    fn default() -> Self {
        Self { continuous: false, magnet: false, favorites: vec![Kind::Trend, Kind::HLine], hide_all: false, lock_all: false }
    }
}

/// 改一个对象的哪个锚点的哪一项（检查器里的精确输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Price,
    Time,
}

/// 工具栏、检查器、图上浮动小条发来的操作（docs/46 G1）。都是全局状态上的操作，与哪个面板无关。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawMsg {
    Tool(Option<Kind>),
    ToggleMenu,
    CloseMenu,
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
    /// 两个锚点的工具：已经放下的第一个（品种, 锚点）
    placing: Option<(String, Anchor)>,
    selected: Option<(String, String)>,
    drag: Option<Drag>,
    /// 最近一条说明（存盘失败、读不了的对象、输入不认得…），显示在工具栏的绘图菜单与检查器
    note: String,
    prefs: Option<Prefs>,
    menu_open: bool,
    /// 检查器输入框里还没回车的文字：键 → 文字
    edits: HashMap<String, String>,
}

static VERSION: AtomicU64 = AtomicU64::new(1);

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

/// 最近一条说明（空 = 没有）。
#[must_use]
pub fn note() -> String {
    with(|st| st.note.clone())
}

#[must_use]
pub fn menu_open() -> bool {
    with(|st| st.menu_open)
}

/// 这个对象现在能不能拖、能不能删（还要看「全部锁定」）。
fn can_edit(st: &mut State, d: &Drawing) -> bool {
    d.editable() && !prefs_of(st).lock_all
}

/// 在这张图上画不画它：没隐藏、没「全部隐藏」、这一类周期上显示。
fn visible(st: &mut State, d: &Drawing, cat: &str) -> bool {
    !d.hidden && !prefs_of(st).hide_all && d.shown_on(cat)
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

/// 这个品种的对象（拖动中的那个换成预览）。
#[must_use]
pub fn items(symbol: &str) -> Vec<Drawing> {
    with(|st| {
        let drag = st.drag.clone().filter(|d| d.symbol == symbol);
        let mut v = book(st, symbol).items.clone();
        if let Some(d) = drag
            && let Some(slot) = v.iter_mut().find(|x| x.id == d.current.id)
        {
            *slot = d.current;
        }
        v
    })
}

/// 在这一类周期的图上看得见的对象（画与命中测试用）。
#[must_use]
pub fn visible_items(symbol: &str, cat: &str) -> Vec<Drawing> {
    let all = items(symbol);
    with(|st| all.into_iter().filter(|d| visible(st, d, cat)).collect())
}

/// 选中的对象（拖动中是预览）。
#[must_use]
pub fn selected_drawing(symbol: &str) -> Option<Drawing> {
    let id = selected(symbol)?;
    items(symbol).into_iter().find(|d| d.id == id)
}

/// 价格轴上要标的价位（G1）：看得见的水平线，以及选中对象的各个锚点（垂直线除外）。(价格, 颜色)
#[must_use]
pub fn axis_marks(symbol: &str, cat: &str) -> Vec<(i64, [u8; 4])> {
    let sel = selected(symbol);
    visible_items(symbol, cat)
        .into_iter()
        .flat_map(|d| {
            let is_sel = sel.as_deref() == Some(d.id.as_str());
            let c = d.style.color;
            let ps: Vec<(i64, [u8; 4])> = match d.kind {
                Kind::HLine => d.anchors.first().map(|a| (a.p, c)).into_iter().collect(),
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
    let items = visible_items(symbol, v.cat);
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
    place(symbol, anchor_for(v, p))
}

/// 选了工具时按下左键：一个锚点的工具直接建好；两个锚点的，第一下记起点、第二下建好。返回建好的对象 id。
pub fn place(symbol: &str, at: Anchor) -> Option<String> {
    let (kind, first) = with(|st| (st.tool, st.placing.clone().filter(|(s, _)| s == symbol).map(|(_, a)| a)));
    let kind = kind?;
    if kind.anchors() == 2 && first.is_none() {
        with(|st| st.placing = Some((symbol.to_string(), at)));
        bump();
        return None;
    }
    let anchors = match first {
        Some(a) => vec![a, at],
        None => vec![at],
    };
    let d = Drawing::new(kind, anchors);
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

/// 两个锚点的工具放下了第一个点（画预览用）。
#[must_use]
pub fn placing(symbol: &str) -> Option<(Kind, Anchor)> {
    with(|st| match (&st.placing, st.tool) {
        (Some((s, a)), Some(k)) if s == symbol => Some((k, *a)),
        _ => None,
    })
}

/// 没选工具时按下左键：点中对象就选中（能拖的开始拖），返回 true；没点中返回 false（图表照常平移），并取消选中。
pub fn press(symbol: &str, v: &DrawView, p: Point) -> bool {
    let all = visible_items(symbol, v.cat);
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
        _ => None,
    }
}

// ── 工具栏 / 检查器 / 浮动小条的操作 ───────────────────────────────────────────

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
        DrawMsg::Tool(k) => {
            set_tool(k);
            with(|st| st.menu_open = false);
        }
        DrawMsg::ToggleMenu => {
            with(|st| st.menu_open = !st.menu_open);
            bump();
        }
        DrawMsg::CloseMenu => with(|st| st.menu_open = false),
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

/// 整组改（每个对象一条「改」）。
fn group_set(symbol: &str, group: &str, f: impl Fn(&mut Drawing)) {
    let ids: Vec<String> = items(symbol).into_iter().filter(|d| d.group.as_deref() == Some(group)).map(|d| d.id).collect();
    for id in ids {
        modify(symbol, &id, false, &f);
    }
}

// ── 画 ──────────────────────────────────────────────────────────────────────

fn stroke(d: &Drawing, emphasis: bool) -> Stroke<'static> {
    let color = crate::ui::chart::drawing_color(d.style.color);
    let width = d.style.width + if emphasis { 0.5 } else { 0.0 };
    let segments: &'static [f32] = match d.style.dash {
        Dash::Solid => &[],
        Dash::Dashed => &[6.0, 4.0],
        Dash::Dotted => &[1.5, 3.0],
    };
    Stroke { width, line_dash: LineDash { segments, offset: 0 }, ..Stroke::default() }.with_color(color)
}

fn draw_one(frame: &mut Frame, v: &DrawView, d: &Drawing, selected: bool) {
    if d.hidden {
        return;
    }
    let Some(shape) = geometry::shape(v, d) else { return };
    match shape {
        Shape::Segment(a, b) => frame.stroke(&Path::line(a, b), stroke(d, selected)),
        Shape::Rect(r) => {
            let fill = d.style.fill.map_or_else(
                || crate::ui::pal::alpha(crate::ui::chart::drawing_color(d.style.color), 0.15),
                crate::ui::chart::drawing_color,
            );
            frame.fill_rectangle(r.position(), r.size(), fill);
            frame.stroke(&Path::rectangle(r.position(), r.size()), stroke(d, selected));
        }
    }
    if selected {
        let edge = crate::ui::chart::drawing_color(d.style.color);
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
    let sel = selected(symbol);
    for d in visible_items(symbol, v.cat) {
        draw_one(frame, v, &d, sel.as_deref() == Some(d.id.as_str()));
    }
}

/// 十字线层：两个锚点的工具放下第一个点之后，跟着光标画预览；选着工具时左下角写一行提示。
pub fn draw_hint(frame: &mut Frame, v: &DrawView, symbol: &str, cursor: Option<Point>) {
    if let (Some((kind, first)), Some(c)) = (placing(symbol), cursor) {
        let mut d = Drawing::new(kind, vec![first, v.anchor_at(c)]);
        d.style.dash = Dash::Dashed;
        draw_one(frame, v, &d, false);
    }
    if let Some(kind) = tool() {
        let step = if kind.anchors() == 2 && placing(symbol).is_none() {
            "点一下放起点"
        } else if kind.anchors() == 2 {
            "再点一下放终点"
        } else {
            "点一下放下"
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
    let v = version();
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
            bars: Vec::new(),
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
        assert!(items(sym)[0].hidden && visible_items(sym, "min").len() == 1);
        handle(DrawMsg::Undo(sym.into()));
        assert!(!items(sym)[0].hidden);
        // 只在小时级显示：分钟图上看不见、点不中
        handle(DrawMsg::ShowOn(sym.into(), id.clone(), "min".into(), false));
        assert_eq!(visible_items(sym, "min").len(), 1);
        assert_eq!(visible_items(sym, "hour").len(), 2);
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
        assert!(visible_items(sym, "hour").is_empty());
        handle(DrawMsg::HideAll(false));
        // 收藏：点一下加、再点一下去
        let before = prefs().favorites.len();
        handle(DrawMsg::Favorite(Kind::Rect));
        handle(DrawMsg::Favorite(Kind::Rect));
        assert_eq!(prefs().favorites.len(), before);
        // 价格轴：水平线的价位都标；选中的趋势线标两端
        assert_eq!(axis_marks(sym, "hour").len(), 2);
    }
}
