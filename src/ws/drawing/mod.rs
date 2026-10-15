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

use geometry::{DrawView, Part, Shape};
use model::{Anchor, Dash, Drawing, Kind};
use store::{Book, Cmd, Db, Effect};

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
    /// 最近一条说明（存盘失败、读不了的对象…），G1 显示在工具栏
    note: String,
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

// ── 图上的操作 ───────────────────────────────────────────────────────────────

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
        st.tool = None; // G0：画完一个回到光标（连续绘制在 G1）
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
    let all = items(symbol);
    let sel = selected(symbol);
    match geometry::pick(v, &all, p, sel.as_deref()) {
        Some((d, part)) => {
            let d = d.clone();
            with(|st| {
                st.selected = Some((symbol.to_string(), d.id.clone()));
                st.drag = d.editable().then(|| Drag { symbol: symbol.to_string(), orig: d.clone(), part, start: p, current: d.clone() });
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
                    *a = v.anchor_at(p);
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
        if !d.editable() {
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
    for d in items(symbol) {
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
    }
}
