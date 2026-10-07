//! 「订单流层析」视图（docs/40 §5）：数据选择 · 层析时间轴（泳道共用一条时间轴，滚轮缩放、拖动平移、十字线、点选事件）·
//! 因果链卡片 · 本窗口同类事件的前瞻 · 特征字典（按层）。只读 [`super::ofms_lab::view`] 的快照，不做 IO。

use std::sync::Arc;

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use std::collections::BTreeMap;

use super::ofms_lab::{self as ol, Dict, OfData, OfMsg, OfTab, View};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

/// 泳道：(层号, 名称, 相对高度)。L6–L10 合在「策略」一条（P4 接入后显示）。
const LANES: [(u8, &str, f32); 7] = [
    (3, "价格 · L2 深度 · L3 结构", 5.0),
    (0, "L0 数据完整性", 0.6),
    (1, "L1 微观体制", 1.0),
    (2, "L2 流动性", 1.5),
    (4, "L4 订单流", 2.0),
    (5, "L5 价格响应", 1.5),
    (6, "L6–L10 策略", 0.8),
];

pub fn pane_body<'a>(lock: Option<&str>) -> Element<'a, OfMsg> {
    if let Some(l) = lock {
        ol::lock_tab(l);
    }
    let v = ol::view();
    // 页面锁定了视图：切换在顶部页签上（docs/41），面板里不再画第二层页签
    let tabs = lock.is_none().then(|| w::tabs(&ol::TABS.map(|(t, n)| (n, t)), &v.tab, OfMsg::Tab));
    let mut col = column![w::panel_header("订单流层析", tabs, vec![])].spacing(space(2));
    let body: Element<'a, OfMsg> = match v.tab {
        OfTab::Timeline => timeline_tab(&v),
        OfTab::Response => response_tab(&v),
        OfTab::Setups => setups_tab(&v),
        OfTab::Dictionary => dictionary_tab(&v),
    };
    col = col.push(body);
    container(col).padding(crate::ui::metrics::pad2(2, 3)).width(Length::Fill).height(Length::Fill).into()
}

// ── 层析时间轴页 ───────────────────────────────────────────────────────

fn timeline_tab<'a>(v: &View) -> Element<'a, OfMsg> {
    let picker = super::data_picker_view::view(&v.pick, &ol::opts()).map(OfMsg::Pick);
    let mut top = column![picker].spacing(space(1));
    let mut actions = row![
        w::btn_busy("生成 / 读取", Kind::Primary, Some(OfMsg::Run), v.running),
        w::btn(if v.live { "● 实时（点击停止）" } else { "实时" }, if v.live { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::Live(!v.live))),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if v.live {
        for m in [15u32, 30, 60, 120] {
            actions = actions.push(w::btn(format!("{m} 分"), if v.live_minutes == m { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::LiveMinutes(m))));
        }
    }
    for (l, name, _) in LANES {
        let on = !v.hidden.contains(&l);
        actions = actions.push(w::btn(name, if on { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::ToggleLane(l))));
    }
    top = top.push(actions.wrap());
    if !v.note.is_empty() {
        top = top.push(t::metadata(v.note.clone()).color(pal::dim()));
    }
    let Some(d) = &v.data else {
        return column![top, w::empty("还没有数据", "选一个窗口点「生成 / 读取」：Rust 特征引擎回放 + OFMS 检测器，结果按窗口缓存")]
            .spacing(space(2))
            .into();
    };
    let chart = iced::widget::Canvas::new(Tomo { d: d.clone(), hidden: v.hidden.clone(), selected: v.selected, overlay: v.overlay.clone() })
        .width(Length::FillPortion(3))
        .height(Length::Fill);
    let side = scrollable(side_panel(d, v.dict.as_deref(), v.selected)).width(Length::FillPortion(1)).height(Length::Fill);
    column![
        top,
        t::metadata(format!(
            "{} · 滚轮缩放、拖动平移、点事件看因果链{}",
            d.title,
            if v.live { "（缩放 / 平移后不再跟随最新；右键回到全程跟随）" } else { "" }
        ))
        .color(pal::dim()),
        row![chart, side].spacing(space(2)).height(Length::Fill),
    ]
    .spacing(space(2))
    .height(Length::Fill)
    .into()
}

fn side_panel<'a>(d: &Arc<OfData>, dict: Option<&Dict>, sel: Option<u64>) -> Element<'a, OfMsg> {
    let mut col = column![].spacing(space(1));
    // 事件计数（按层）
    col = col.push(w::section("本窗口事件"));
    let mut counts: std::collections::BTreeMap<(u8, String), usize> = std::collections::BTreeMap::new();
    for e in &d.events {
        *counts.entry((e.layer, e.key.clone())).or_default() += 1;
    }
    for ((l, k), n) in &counts {
        let name = dict.and_then(|x| x.event(k)).map(|x| x.name.clone()).unwrap_or_else(|| k.clone());
        col = col.push(row![t::metadata(format!("L{l}")).color(pal::dim()), t::body(name), Space::new().width(Length::Fill), t::numeric(n.to_string())]
            .spacing(space(2)));
    }
    let Some(e) = sel.and_then(|id| d.by_id.get(&id)).map(|i| &d.events[*i]) else {
        col = col.push(t::metadata("点时间轴上的事件标记，这里显示它的因果链与同类事件之后价格怎么走。").color(pal::dim()));
        return col.into();
    };
    let meta = dict.and_then(|x| x.event(&e.key));
    col = col.push(w::section(format!("{} · L{}", meta.map(|m| m.name.as_str()).unwrap_or(&e.key), e.layer)));
    if let Some(m) = meta {
        col = col.push(t::caption(m.definition.clone()));
        col = col.push(w::badge(
            match m.nature.as_str() {
                "observed" => "观测",
                "inferred" => "推断（L2 上撤 / 吃靠成交归因；MBO 校验见 docs/40 §11）",
                "derived" => "规则组合",
                _ => "策略",
            },
            if m.nature == "inferred" { Tone::Warning } else { Tone::Neutral },
        ));
    }
    col = col.push(t::metadata(format!(
        "{} · 方向 {} · 价 {} · 量 {} · 强度 {} · {}",
        fmt_t(e.t),
        match e.dir { 1 => "↑", -1 => "↓", _ => "—" },
        fmt_v(e.price),
        fmt_v(e.size),
        fmt_v(e.strength),
        e.detail
    )));
    // 因果链
    let chain = ol::chain_of(d, e.chain);
    col = col.push(w::section(format!("因果链（{} 环）", chain.len())));
    let mut prev: Option<i64> = None;
    for x in chain.iter().take(40) {
        let name = dict.and_then(|dd| dd.event(&x.key)).map(|m| m.name.clone()).unwrap_or_else(|| x.key.clone());
        let gap = prev.map(|p| format!("+{:.1}s", (x.t - p) as f64 / 1000.0)).unwrap_or_default();
        prev = Some(x.t);
        let me = x.id == e.id;
        col = col.push(
            button(
                row![
                    container(t::metadata(gap).color(pal::dim())).width(Length::Fixed(52.0)),
                    t::metadata(format!("L{}", x.layer)).color(pal::dim()),
                    t::body(name).color(if me { pal::accent() } else { pal::txt() }),
                    t::metadata(match x.dir { 1 => "↑", -1 => "↓", _ => "" }),
                    Space::new().width(Length::Fill),
                    t::numeric(fmt_v(x.price)).color(pal::dim()),
                ]
                .spacing(space(1))
                .align_y(Alignment::Center),
            )
            .on_press(OfMsg::Select(Some(x.id)))
            .style(move |th, st| w::button_style(if me { Kind::Standard } else { Kind::Ghost }, th, st)),
        );
    }
    // 同类事件之后
    let hs = [5i64, 30, 60, 300];
    let (n, means) = ol::same_kind_stats(d, &e.key, &hs);
    col = col.push(w::section(format!("本窗口同类事件之后（{n} 次，按事件方向对齐，bp）")));
    let mut r = row![].spacing(space(3));
    for (h, m) in hs.iter().zip(means) {
        let (s, c) = match m {
            Some(x) => (format!("{x:+.2}"), if x >= 0.0 { pal::up() } else { pal::down() }),
            None => ("—".into(), pal::dim()),
        };
        r = r.push(column![t::metadata(format!("{h}s")).color(pal::dim()), t::numeric(s).color(c)]);
    }
    col = col.push(r);
    col = col.push(
        t::metadata("只是本窗口的描述，不是结论：没有扣成本、没有去重叠、没有多重检验。跨窗口的正式统计在「响应表」（P3）。")
            .color(pal::warn()),
    );
    col.into()
}

// ── 层析画布 ──────────────────────────────────────────────────────────

const ML: f32 = 64.0;
const MR: f32 = 8.0;
const LANE_GAP: f32 = 6.0;
const AXIS_H: f32 = 16.0;

pub struct Tomo {
    pub d: Arc<OfData>,
    pub hidden: std::collections::BTreeSet<u8>,
    pub selected: Option<u64>,
    /// 描述语言策略在本窗口上的逐笔（入场形态矩阵里点「叠加到时间轴」）
    pub overlay: Option<Arc<ol::Overlay>>,
}

#[derive(Default)]
pub struct TomoState {
    lo: f64,
    hi: f64,
    drag: Option<(f32, f64, f64, f32)>,
}

impl TomoState {
    fn range(&self, d: &OfData) -> (f64, f64) {
        if self.hi > self.lo {
            return (self.lo, self.hi);
        }
        match (d.states.t.first(), d.states.t.last()) {
            (Some(a), Some(b)) if b > a => (*a as f64, *b as f64),
            _ => (0.0, 1.0),
        }
    }
}

struct Lane {
    id: u8,
    name: &'static str,
    y: f32,
    h: f32,
}

impl Tomo {
    fn lanes(&self, b: Rectangle) -> Vec<Lane> {
        let vis: Vec<_> = LANES.iter().filter(|(l, _, _)| !self.hidden.contains(l)).collect();
        let total: f32 = vis.iter().map(|x| x.2).sum::<f32>().max(1e-3);
        let avail = (b.height - AXIS_H - LANE_GAP * vis.len() as f32).max(10.0);
        let mut y = 0.0;
        vis.iter()
            .map(|(id, name, wgt)| {
                let h = avail * wgt / total;
                let l = Lane { id: *id, name, y, h };
                y += h + LANE_GAP;
                l
            })
            .collect()
    }

    fn sx(&self, b: Rectangle, lo: f64, hi: f64, t: f64) -> f32 {
        ML + ((t - lo) / (hi - lo).max(1.0)) as f32 * (b.width - ML - MR).max(1.0)
    }

    /// 事件画在哪条泳道（价格泳道放 L2 / L3 / L4 有价位的事件）。
    /// 价格泳道只放结构事件（L3）与「墙被吃」——补单 / 墙出现 / 撤墙 / 真空一小时上千个，放价格上会把价格线埋掉。
    fn lane_of(e: &ol::OfEvent) -> u8 {
        match (e.layer, e.key.as_str()) {
            (_, "WALL_CONSUMED") | (3, _) => 3,
            (0, _) => 0,
            (1, _) => 1,
            (2, _) => 2,
            (4, _) => 4,
            (5, _) => 5,
            _ => 6,
        }
    }

    /// 可见范围内事件太密（平均每 3 像素不止一个）时画成细竖线而不是符号。
    fn dense(&self, lane: u8, lo: f64, hi: f64, pw: f32) -> bool {
        let n = self.d.events.iter().filter(|e| Self::lane_of(e) == lane && (e.t as f64) >= lo && (e.t as f64) <= hi).count();
        n as f32 > pw / 3.0
    }
}

fn fmt_t(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.format("%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

fn fmt_v(v: f64) -> String {
    if !v.is_finite() {
        "—".into()
    } else if v.abs() >= 1000.0 {
        format!("{v:.1}")
    } else if v.abs() >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.4}")
    }
}

/// 事件颜色：方向（涨跌色）或层色。
fn ev_color(e: &ol::OfEvent) -> Color {
    match e.key.as_str() {
        "WALL_PULLED" | "BOOK_DESYNC" | "DATA_GAP" | "CROSSED_BOOK" | "LEVEL_FAILURE" => pal::warn(),
        "RESP_ABSORPTION" | "EXHAUSTION" | "RESP_WEAKNESS" => pal::accent(),
        "RESP_VACUUM" | "VACUUM" => pal::info(),
        _ => match e.dir {
            1 => crate::ui::chart::up(),
            -1 => crate::ui::chart::down(),
            _ => pal::dim(),
        },
    }
}

/// `r`：符号半径；`ring`：选中链上的事件加一圈。
fn mark(f: &mut Frame, p: Point, e: &ol::OfEvent, r: f32, ring: bool) {
    let c = ev_color(e);
    match e.key.as_str() {
        "AGGRESSION" | "SWEEP" | "LEVEL_BREAK" | "FLOW_REVERSAL" => {
            let d = if e.dir >= 0 { -1.0 } else { 1.0 };
            let tri = Path::new(|b| {
                b.move_to(Point::new(p.x, p.y + d * r));
                b.line_to(Point::new(p.x - r, p.y - d * r));
                b.line_to(Point::new(p.x + r, p.y - d * r));
                b.close();
            });
            f.fill(&tri, c);
        }
        "WALL_APPEAR" => f.fill_rectangle(Point::new(p.x - r, p.y - 1.5), Size::new(2.0 * r, 3.0), c),
        "WALL_CONSUMED" | "WALL_PULLED" | "LEVEL_FAILURE" | "DATA_GAP" | "BOOK_DESYNC" => {
            let s = Stroke::default().with_width(2.0).with_color(c);
            f.stroke(&Path::line(Point::new(p.x - r, p.y - r), Point::new(p.x + r, p.y + r)), s);
            f.stroke(&Path::line(Point::new(p.x - r, p.y + r), Point::new(p.x + r, p.y - r)), s);
        }
        _ => {
            f.fill(&Path::circle(p, r), c);
        }
    }
    if ring {
        f.stroke(&Path::circle(p, r + 3.0), Stroke::default().with_width(1.5).with_color(pal::txt()));
    }
}

/// 序列在 [lo, hi] 时间内的极值（忽略 NaN）。
fn extent(t: &[i64], v: &[f64], lo: f64, hi: f64) -> (f64, f64) {
    let mut a = (f64::INFINITY, f64::NEG_INFINITY);
    for (x, y) in t.iter().zip(v) {
        let x = *x as f64;
        if x >= lo && x <= hi && y.is_finite() {
            a = (a.0.min(*y), a.1.max(*y));
        }
    }
    if !a.0.is_finite() {
        (0.0, 1.0)
    } else if (a.1 - a.0).abs() < 1e-12 {
        (a.0 - 0.5, a.1 + 0.5)
    } else {
        a
    }
}

fn line(f: &mut Frame, pts: impl Iterator<Item = (f32, f32)>, c: Color, wdt: f32) {
    let mut started = false;
    let path = Path::new(|b| {
        for (x, y) in pts {
            if !y.is_finite() {
                started = false;
                continue;
            }
            if started {
                b.line_to(Point::new(x, y));
            } else {
                b.move_to(Point::new(x, y));
                started = true;
            }
        }
    });
    f.stroke(&path, Stroke::default().with_width(wdt).with_color(c));
}

fn regime_color(s: &str) -> Color {
    let h = s.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    pal::series((h % 8) as usize)
}

fn response_color(s: &str) -> Option<Color> {
    Some(match s {
        "RESP_CONTINUATION" => crate::ui::chart::up(),
        "RESP_EXPANSION" => pal::ok(),
        "RESP_ABSORPTION" => pal::accent(),
        "RESP_VACUUM" => pal::info(),
        "RESP_WEAKNESS" => pal::bad(),
        _ => return None,
    })
}

impl canvas::Program<OfMsg> for Tomo {
    type State = TomoState;

    fn update(&self, st: &mut TomoState, event: &canvas::Event, b: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<OfMsg>> {
        let (lo, hi) = st.range(&self.d);
        let pw = (b.width - ML - MR).max(1.0);
        match event {
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = cursor.position_in(b)?;
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let at = lo + f64::from(((p.x - ML) / pw).clamp(0.0, 1.0)) * (hi - lo);
                let k = if y > 0.0 { 0.8 } else { 1.25 };
                let (a, z) = (at - (at - lo) * k, at + (hi - at) * k);
                if z - a < 5_000.0 {
                    return Some(canvas::Action::capture());
                }
                let (full_lo, full_hi) = TomoState::default().range(&self.d);
                st.lo = a.max(full_lo);
                st.hi = z.min(full_hi);
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                cursor.position_in(b)?;
                st.lo = 0.0;
                st.hi = 0.0;
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(b)?;
                st.drag = Some((p.x, lo, hi, p.y));
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let (x0, _, _, y0) = st.drag.take()?;
                let p = cursor.position_in(b)?;
                if (p.x - x0).abs() > 4.0 {
                    return None;
                }
                // 点击：找最近的事件标记
                let lanes = self.lanes(b);
                let lane = lanes.iter().find(|l| p.y >= l.y && p.y <= l.y + l.h)?;
                let best = self
                    .d
                    .events
                    .iter()
                    .filter(|e| Self::lane_of(e) == lane.id)
                    .map(|e| (e, (self.sx(b, lo, hi, e.t as f64) - p.x).abs()))
                    .filter(|(_, dx)| *dx <= 6.0)
                    .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                let _ = y0;
                Some(canvas::Action::publish(OfMsg::Select(best.map(|(e, _)| e.id))))
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some((x0, a, z, _)), Some(p)) = (st.drag, cursor.position_in(b)) {
                    let (full_lo, full_hi) = TomoState::default().range(&self.d);
                    let dx = f64::from((x0 - p.x) / pw) * (z - a);
                    let shift = dx.clamp(full_lo - a, full_hi - z);
                    st.lo = a + shift;
                    st.hi = z + shift;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                Some(canvas::Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(&self, st: &TomoState, r: &Renderer, _th: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(r, b.size());
        let d = &self.d;
        let s = &d.states;
        let (lo, hi) = st.range(d);
        let pw = (b.width - ML - MR).max(1.0);
        let x_of = |t: f64| self.sx(b, lo, hi, t);
        let i0 = s.t.partition_point(|x| (*x as f64) < lo);
        let i1 = s.t.partition_point(|x| (*x as f64) <= hi);
        let lanes = self.lanes(b);
        let sel_chain = self.selected.and_then(|id| d.by_id.get(&id)).map(|i| d.events[*i].chain);

        for lane in &lanes {
            let (y, h) = (lane.y, lane.h);
            f.fill_rectangle(Point::new(ML, y), Size::new(pw, h), pal::alpha(pal::card_bg(), 0.35));
            f.fill_text(Text { content: lane.name.into(), position: Point::new(2.0, y + 1.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
            let ts = &s.t[i0..i1];
            match lane.id {
                3 => {
                    // 价格泳道：深度热图（买 = 涨色、卖 = 跌色，深浅 = 挂量）+ 中间价 + VWAP + 结构位
                    let (mut plo, mut phi) = extent(ts, &s.mid[i0..i1], lo, hi);
                    let pad = (phi - plo) * 0.08;
                    plo -= pad;
                    phi += pad;
                    let py = |v: f64| y + ((phi - v) / (phi - plo)) as f32 * h;
                    if let Some(dep) = &d.depth {
                        let colw = (pw / ((hi - lo) / 1000.0).max(1.0) as f32).max(1.0);
                        let rowh = (h / 120.0).max(1.0);
                        for (tm, ask, px, q) in &dep.rows {
                            let tm = *tm as f64;
                            if tm < lo || tm > hi || *px < plo || *px > phi || dep.max_qty <= 0.0 {
                                continue;
                            }
                            let a = ((q / dep.max_qty).sqrt() * 0.85) as f32;
                            let c = if *ask { crate::ui::chart::down() } else { crate::ui::chart::up() };
                            f.fill_rectangle(Point::new(x_of(tm), py(*px) - rowh / 2.0), Size::new(colw, rowh), pal::alpha(c, a));
                        }
                    }
                    line(&mut f, ts.iter().zip(&s.level_hi[i0..i1]).map(|(t, v)| (x_of(*t as f64), py(*v))), pal::alpha(pal::dim(), 0.8), 1.0);
                    line(&mut f, ts.iter().zip(&s.level_lo[i0..i1]).map(|(t, v)| (x_of(*t as f64), py(*v))), pal::alpha(pal::dim(), 0.8), 1.0);
                    line(&mut f, ts.iter().zip(&s.vwap[i0..i1]).map(|(t, v)| (x_of(*t as f64), py(*v))), pal::accent(), 1.0);
                    line(&mut f, ts.iter().zip(&s.mid[i0..i1]).map(|(t, v)| (x_of(*t as f64), py(*v))), pal::txt(), 1.5);
                    for k in 0..=3 {
                        let v = plo + (phi - plo) * f64::from(k) / 3.0;
                        f.fill_text(Text { content: fmt_v(v), position: Point::new(2.0, py(v) - 5.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
                    }
                    let dense3 = self.dense(3, lo, hi, pw);
                    for e in d.events.iter().filter(|e| Self::lane_of(e) == 3) {
                        let tm = e.t as f64;
                        if tm < lo || tm > hi || !e.price.is_finite() {
                            continue;
                        }
                        let big = sel_chain == Some(e.chain);
                        mark(&mut f, Point::new(x_of(tm), py(e.price)), e, if big { 5.0 } else if dense3 { 2.0 } else { 4.0 }, big);
                    }
                }
                0 => {
                    if s.synced[i0..i1].iter().all(|x| *x) && !d.events.iter().any(|e| e.layer == 0) {
                        f.fill_text(Text { content: "全程同步、无断档".into(), position: Point::new(ML + 4.0, y), color: pal::dim(), size: iced::Pixels(9.0), ..Default::default() });
                    }
                    for (k, tm) in ts.iter().enumerate() {
                        if !s.synced[i0 + k] {
                            f.fill_rectangle(Point::new(x_of(*tm as f64), y), Size::new(2.0, h), pal::alpha(pal::bad(), 0.6));
                        }
                    }
                }
                1 => {
                    // 体制色带 + 短 / 长波动比
                    let mut start: Option<(f64, &str)> = None;
                    for k in i0..i1 {
                        let lab = s.regime[k].as_str();
                        let tm = s.t[k] as f64;
                        match start {
                            Some((t0, l)) if l == lab && k + 1 < i1 => {
                                let _ = t0;
                            }
                            Some((t0, l)) => {
                                f.fill_rectangle(Point::new(x_of(t0), y), Size::new((x_of(tm) - x_of(t0)).max(1.0), h * 0.5), pal::alpha(regime_color(l), 0.55));
                                start = Some((tm, lab));
                            }
                            None => start = Some((tm, lab)),
                        }
                    }
                    let ry = |v: f64| y + h - (v.clamp(0.0, 3.0) / 3.0) as f32 * h * 0.45;
                    line(&mut f, ts.iter().zip(&s.rv_ratio[i0..i1]).map(|(t, v)| (x_of(*t as f64), ry(*v))), pal::warn(), 1.0);
                }
                2 => {
                    let (_, dmax) = extent(ts, &s.depth_bid[i0..i1], lo, hi);
                    let (_, amax) = extent(ts, &s.depth_ask[i0..i1], lo, hi);
                    let m = dmax.max(amax).max(1e-9);
                    let mid_y = y + h / 2.0;
                    line(&mut f, ts.iter().zip(&s.depth_ask[i0..i1]).map(|(t, v)| (x_of(*t as f64), mid_y - (v / m) as f32 * h / 2.0)), crate::ui::chart::down(), 1.0);
                    line(&mut f, ts.iter().zip(&s.depth_bid[i0..i1]).map(|(t, v)| (x_of(*t as f64), mid_y + (v / m) as f32 * h / 2.0)), crate::ui::chart::up(), 1.0);
                    f.fill_text(Text { content: "卖深 ↑ / 买深 ↓（±10bp）".into(), position: Point::new(ML + 2.0, y + 1.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
                }
                4 => {
                    // 主动买（上）/ 卖（下）柱 + CVD
                    let (_, bmax) = extent(ts, &s.buy[i0..i1], lo, hi);
                    let (_, smax) = extent(ts, &s.sell[i0..i1], lo, hi);
                    let m = bmax.max(smax).max(1e-9);
                    let mid_y = y + h / 2.0;
                    let bw = (pw / (i1 - i0).max(1) as f32).max(1.0);
                    for k in i0..i1 {
                        let x = x_of(s.t[k] as f64);
                        let hb = (s.buy[k] / m) as f32 * h / 2.0;
                        let hs = (s.sell[k] / m) as f32 * h / 2.0;
                        if hb > 0.0 {
                            f.fill_rectangle(Point::new(x, mid_y - hb), Size::new(bw, hb), pal::alpha(crate::ui::chart::up(), 0.8));
                        }
                        if hs > 0.0 {
                            f.fill_rectangle(Point::new(x, mid_y), Size::new(bw, hs), pal::alpha(crate::ui::chart::down(), 0.8));
                        }
                    }
                    let (clo, chi) = extent(ts, &s.cvd[i0..i1], lo, hi);
                    let cy = |v: f64| y + ((chi - v) / (chi - clo).max(1e-9)) as f32 * h;
                    line(&mut f, ts.iter().zip(&s.cvd[i0..i1]).map(|(t, v)| (x_of(*t as f64), cy(*v))), pal::accent(), 1.2);
                }
                5 => {
                    // 响应状态色带 + 残差 z
                    for k in i0..i1 {
                        if let Some(c) = response_color(&s.response[k]) {
                            f.fill_rectangle(Point::new(x_of(s.t[k] as f64), y), Size::new(2.0, h), pal::alpha(c, 0.35));
                        }
                    }
                    let zy = |v: f64| y + h / 2.0 - (v.clamp(-4.0, 4.0) / 4.0) as f32 * h / 2.0;
                    f.stroke(&Path::line(Point::new(ML, zy(0.0)), Point::new(ML + pw, zy(0.0))), Stroke::default().with_width(1.0).with_color(pal::grid()));
                    line(&mut f, ts.iter().zip(&s.residual_z[i0..i1]).map(|(t, v)| (x_of(*t as f64), zy(*v))), pal::txt(), 1.0);
                }
                _ => match &self.overlay {
                    None => {
                        f.fill_text(Text {
                            content: "策略泳道：在「入场形态矩阵」里选一个事件轨描述文件「叠加到时间轴」".into(),
                            position: Point::new(ML + 4.0, y + h / 2.0 - 5.0),
                            color: pal::dim(),
                            size: iced::Pixels(10.0),
                            ..Default::default()
                        });
                    }
                    Some(o) => {
                        // 进场（L6/L8）▲▼、持有段、出场（L9）✕；颜色 = 这一笔扣成本后的盈亏
                        let mid_y = y + h / 2.0;
                        for tr in &o.trades {
                            let (a, z) = ((tr.entry_ts / 1_000_000) as f64, (tr.exit_ts / 1_000_000) as f64);
                            if z < lo || a > hi {
                                continue;
                            }
                            let c = if tr.net_bp >= 0.0 { pal::ok() } else { pal::bad() };
                            let (xa, xz) = (x_of(a.max(lo)), x_of(z.min(hi)));
                            f.fill_rectangle(Point::new(xa, mid_y - 2.0), Size::new((xz - xa).max(1.0), 4.0), pal::alpha(c, 0.45));
                            let up = tr.side > 0;
                            let dirc = if up { crate::ui::chart::up() } else { crate::ui::chart::down() };
                            let r = 5.0;
                            let d = if up { -1.0 } else { 1.0 };
                            let tri = Path::new(|b| {
                                b.move_to(Point::new(xa, mid_y + d * r));
                                b.line_to(Point::new(xa - r, mid_y - d * r));
                                b.line_to(Point::new(xa + r, mid_y - d * r));
                                b.close();
                            });
                            f.fill(&tri, dirc);
                            let sx = Stroke::default().with_width(2.0).with_color(c);
                            f.stroke(&Path::line(Point::new(xz - 4.0, mid_y - 4.0), Point::new(xz + 4.0, mid_y + 4.0)), sx);
                            f.stroke(&Path::line(Point::new(xz - 4.0, mid_y + 4.0), Point::new(xz + 4.0, mid_y - 4.0)), sx);
                        }
                        let net: f64 = o.trades.iter().map(|t| t.net_bp).sum();
                        f.fill_text(Text {
                            content: format!("{} · {} 笔 · 净 {net:+.1} bp（快速档，逐秒；往返成本 {:.1} bp）", o.spec, o.trades.len(), o.fee_rt_bp),
                            position: Point::new(ML + 4.0, y),
                            color: pal::dim(),
                            size: iced::Pixels(9.0),
                            ..Default::default()
                        });
                    }
                },
            }
            // 非价格泳道的事件标记画在泳道中线
            if lane.id != 3 {
                let dense = self.dense(lane.id, lo, hi, pw);
                for e in d.events.iter().filter(|e| Self::lane_of(e) == lane.id) {
                    let tm = e.t as f64;
                    if tm < lo || tm > hi {
                        continue;
                    }
                    let on = sel_chain == Some(e.chain);
                    if dense && !on {
                        // 密：底部一条细刻度带，颜色照事件
                        f.fill_rectangle(Point::new(x_of(tm), y + h - 6.0), Size::new(1.0, 6.0), pal::alpha(ev_color(e), 0.7));
                    } else {
                        mark(&mut f, Point::new(x_of(tm), y + h / 2.0), e, if on { 5.0 } else { 3.5 }, on);
                    }
                }
            }
        }
        // 选中链：竖线贯穿全部泳道
        if let Some(c) = sel_chain {
            for e in d.events.iter().filter(|e| e.chain == c) {
                let x = x_of(e.t as f64);
                if x >= ML && x <= ML + pw {
                    f.stroke(&Path::line(Point::new(x, 0.0), Point::new(x, b.height - AXIS_H)),
                             Stroke::default().with_width(1.0).with_color(pal::alpha(pal::accent(), 0.5)));
                }
            }
        }
        // 时间轴
        for k in 0..=4 {
            let tm = lo + (hi - lo) * f64::from(k) / 4.0;
            let x = x_of(tm);
            let lbl = fmt_t(tm as i64);
            f.fill_text(Text { content: lbl.clone(), position: Point::new((x - lbl.len() as f32 * 2.5).clamp(ML, ML + pw - 70.0), b.height - AXIS_H + 3.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
        }
        let mut out = vec![f.into_geometry()];
        // 十字线：竖线贯穿 + 这一秒的读数
        if let Some(p) = cursor.position_in(b) {
            if p.x >= ML && p.x <= ML + pw {
                let mut hair = Frame::new(r, b.size());
                hair.stroke(&Path::line(Point::new(p.x, 0.0), Point::new(p.x, b.height - AXIS_H)), Stroke::default().with_width(1.0).with_color(pal::axis()));
                let tm = lo + f64::from((p.x - ML) / pw) * (hi - lo);
                let k = s.t.partition_point(|x| (*x as f64) < tm).min(s.t.len().saturating_sub(1));
                if !s.t.is_empty() {
                    let lines = vec![
                        fmt_t(s.t[k]),
                        format!("中间价 {}  VWAP {}", fmt_v(s.mid[k]), fmt_v(s.vwap[k])),
                        format!("主动买 {}  卖 {}  CVD {}", fmt_v(s.buy[k]), fmt_v(s.sell[k]), fmt_v(s.cvd[k])),
                        format!("体制 {}  波动比 {}", s.regime[k], fmt_v(s.rv_ratio[k])),
                        format!("响应 {}  残差 z {}", if s.response[k].is_empty() { "正常" } else { &s.response[k] }, fmt_v(s.residual_z[k])),
                    ];
                    super::chart_kit::readout_box(&mut hair, &lines, p, Rectangle::new(Point::ORIGIN, b.size()));
                }
                out.push(hair.into_geometry());
            }
        }
        out
    }

    fn mouse_interaction(&self, st: &TomoState, b: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if st.drag.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(b) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

// ── 特征字典页 ─────────────────────────────────────────────────────────

fn dictionary_tab<'a>(v: &View) -> Element<'a, OfMsg> {
    let Some(d) = &v.dict else {
        return if v.dict_err.is_empty() { w::loading("特征字典").into() } else { w::error("字典读不出来", v.dict_err.clone(), "", None).into() };
    };
    let mut head = row![w::btn("全部层", if v.dict_layer.is_none() { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::DictLayer(None)))].spacing(space(1));
    for l in &d.ofms.layers {
        let n = d.features.iter().filter(|f| f.ofms_layer == l.id).count();
        let on = v.dict_layer == Some(l.id);
        head = head.push(w::btn(format!("{} {}（{n}）", l.key, l.name), if on { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::DictLayer(Some(l.id)))));
    }
    let mut col = column![head.wrap(), text_input("搜索 key / 名称 / 公式", &v.dict_search).on_input(OfMsg::DictSearch).size(t::s_small())].spacing(space(2));
    if let Some(l) = v.dict_layer.and_then(|id| d.ofms.layers.iter().find(|x| x.id == id)) {
        col = col.push(t::caption(format!("{} · {}：{}", l.key, l.name, l.question)));
        let evs: Vec<_> = d.ofms.events.iter().filter(|e| e.layer == l.id).collect();
        if !evs.is_empty() {
            col = col.push(w::section(format!("这一层的事件（{}）", evs.len())));
            for e in evs {
                let params = e.params.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" · ");
                col = col.push(column![
                    row![t::body(format!("{} {}", e.key, e.name)), w::badge(e.nature.clone(), if e.nature == "inferred" { Tone::Warning } else { Tone::Neutral })].spacing(space(2)),
                    t::metadata(e.definition.clone()).color(pal::dim()),
                    t::metadata(params).color(pal::dim()),
                ]);
            }
        }
    }
    let q = v.dict_search.to_lowercase();
    let feats: Vec<_> = d
        .features
        .iter()
        .filter(|f| v.dict_layer.is_none_or(|l| f.ofms_layer == l))
        .filter(|f| q.is_empty() || f.key.contains(&q) || f.name_cn.to_lowercase().contains(&q) || f.formula.to_lowercase().contains(&q))
        .collect();
    col = col.push(w::section(format!("特征（{} 条）", feats.len())));
    let mut list = column![].spacing(space(1));
    for f in feats.iter().take(300) {
        let prior = match f.prior.as_str() {
            "gross" => ("毛来源", Tone::Success),
            "falsified" => ("已证伪", Tone::Danger),
            _ => ("未检验", Tone::Neutral),
        };
        list = list.push(column![
            row![
                t::metadata(format!("L{}", f.ofms_layer)).color(pal::dim()),
                t::body(f.name_cn.clone()),
                t::code(f.key.clone()).size(t::s_small()),
                w::badge(prior.0, prior.1),
                Space::new().width(Length::Fill),
                t::metadata(format!("{} · {}", f.stage, f.family)).color(pal::dim()),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
            t::metadata(f.formula.clone()),
            t::metadata(format!("假设：{}", f.hypothesis)).color(pal::dim()),
        ]);
    }
    col = col.push(scrollable(list).height(Length::Fill));
    col.height(Length::Fill).into()
}

// ── 响应表页（P3）─────────────────────────────────────────────────────

const HS: [u32; 5] = [1, 5, 30, 60, 300];

fn verdict_color(v: &str) -> Option<Color> {
    match v {
        "越过成本" => Some(pal::ok()),
        "有信息未越过成本" => Some(pal::warn()),
        _ => None,
    }
}

fn cell_label(dict: Option<&Dict>, kind: &str, cell: &str) -> String {
    if kind == "event" {
        return dict.and_then(|d| d.event(cell)).map(|e| format!("L{} {}", e.layer, e.name)).unwrap_or_else(|| cell.to_string());
    }
    // 状态格：体制|订单流|响应
    let mut it = cell.split('|');
    let (a, b, c) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    let c = if c == "NORMAL" { "正常".to_string() } else { dict.and_then(|d| d.event(c)).map(|e| e.name.clone()).unwrap_or_else(|| c.to_string()) };
    format!("{a} · {b} · {c}")
}

fn response_tab<'a>(v: &View) -> Element<'a, OfMsg> {
    let Some(r) = &v.resp else {
        return if v.resp_err.is_empty() {
            w::loading("响应表").into()
        } else {
            column![w::error("还没有响应表", v.resp_err.clone(), "生成后点「重新读取」", None), w::btn("重新读取", Kind::Ghost, Some(OfMsg::RespReload))]
                .spacing(space(2))
                .into()
        };
    };
    let dict = v.dict.as_deref();
    let k = v.resp_kind.as_str();
    let mut head = row![].spacing(space(1)).align_y(Alignment::Center);
    for (key, name) in [("event", "事件格"), ("state", "状态格（L1 × L4 × L5）"), ("trans", "响应状态机")] {
        head = head.push(w::btn(name, if k == key { Kind::Standard } else { Kind::Ghost }, Some(OfMsg::RespKind(key.into()))));
    }
    head = head.push(Space::new().width(space(3)));
    head = head.push(w::btn(
        if v.resp_informative { "只看有信息 ✓" } else { "只看有信息" },
        if v.resp_informative { Kind::Standard } else { Kind::Ghost },
        Some(OfMsg::RespInformative(!v.resp_informative)),
    ));
    head = head.push(Space::new().width(Length::Fill));
    head = head.push(w::btn("重新读取", Kind::Ghost, Some(OfMsg::RespReload)));
    let sm = &r.summary;
    let verdicts = sm["verdicts"]
        .as_object()
        .map(|o| o.iter().map(|(a, b)| format!("{a} {b}")).collect::<Vec<_>>().join(" · "))
        .unwrap_or_default();
    let mut col = column![
        head.wrap(),
        t::caption(format!(
            "{} 个窗口（{}）· {} – {} · {} · 试验数 {} · Bonferroni z {} · taker 往返手续费 {} bp + 点差",
            sm["windows"],
            sm["sample"].as_str().unwrap_or(""),
            sm["from"].as_str().unwrap_or(""),
            sm["to"].as_str().unwrap_or(""),
            sm["symbols"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("/")).unwrap_or_default(),
            sm["n_trials"],
            sm["z_bonferroni"],
            sm["fee_rt_bp"],
        )),
        t::metadata(verdicts).color(pal::dim()),
    ]
    .spacing(space(2));
    if k == "trans" {
        col = col.push(t::metadata("L5 响应状态之间的转移：次数，与转移后 30 秒中间价（按当秒订单流方向对齐、扣漂移，bp）。只描述路径，不单独判可交易。").color(pal::dim()));
        let mut list = column![row![
            container(t::metadata("从")).width(Length::Fixed(160.0)),
            container(t::metadata("到")).width(Length::Fixed(160.0)),
            container(t::metadata("次数")).width(Length::Fixed(80.0)),
            t::metadata("30s 均值 ± 标准误"),
        ]
        .spacing(space(2))]
        .spacing(space(1));
        for x in r.transitions.iter().take(80) {
            let name = |s: &str| cell_label(dict, "state", &format!("||{s}")).trim_start_matches(" ·  · ").to_string();
            list = list.push(
                row![
                    container(t::body(name(&x.from))).width(Length::Fixed(160.0)),
                    container(t::body(name(&x.to))).width(Length::Fixed(160.0)),
                    container(t::numeric(x.n.to_string())).width(Length::Fixed(80.0)),
                    t::numeric(format!("{:+.2} ± {:.2}", x.mean30_bp, x.se30_bp.unwrap_or(f64::NAN)))
                        .color(if x.mean30_bp >= 0.0 { pal::up() } else { pal::down() }),
                ]
                .spacing(space(2)),
            );
        }
        return col.push(scrollable(list).height(Length::Fill)).height(Length::Fill).into();
    }
    // 格子 × 视界
    let mut cells: BTreeMap<&str, Vec<&ol::RespRow>> = BTreeMap::new();
    for x in r.rows.iter().filter(|x| x.kind == k) {
        cells.entry(x.cell.as_str()).or_default().push(x);
    }
    let informative = |xs: &Vec<&ol::RespRow>| xs.iter().any(|x| verdict_color(&x.verdict).is_some());
    let mut order: Vec<_> = cells.iter().filter(|(_, xs)| !v.resp_informative || informative(xs)).collect();
    // 按 30s 的 |t| 降序（最有信息的在上）
    order.sort_by(|a, b| {
        let tt = |xs: &Vec<&ol::RespRow>| xs.iter().find(|x| x.h == 30).and_then(|x| x.t_mid).map(f64::abs).unwrap_or(0.0);
        tt(b.1).partial_cmp(&tt(a.1)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let cw = 120.0;
    let mut hdr = row![container(t::metadata("格子")).width(Length::Fixed(300.0)), container(t::metadata("样本（30s）")).width(Length::Fixed(90.0))]
        .spacing(space(1));
    for h in HS {
        hdr = hdr.push(container(t::metadata(format!("{h}s 均值 / 净"))).width(Length::Fixed(cw)));
    }
    if k == "event" {
        hdr = hdr.push(t::metadata("按周 30s 均值（最近一周反号或不到全样本 1/4 = 衰减）"));
    }
    let mut list = column![hdr].spacing(2.0);
    for (cell, xs) in order.iter().take(200) {
        let n30 = xs.iter().find(|x| x.h == 30).map(|x| x.n).unwrap_or(0);
        let mut rr = row![
            container(t::body(cell_label(dict, k, cell))).width(Length::Fixed(300.0)),
            container(t::numeric(n30.to_string()).color(pal::dim())).width(Length::Fixed(90.0)),
        ]
        .spacing(space(1))
        .align_y(Alignment::Center);
        for h in HS {
            let x = xs.iter().find(|x| x.h == h);
            let (label, bg) = match x {
                Some(x) if x.verdict != "样本不足" => (
                    format!("{:+.2} / {:+.1}{}", x.mean_bp.unwrap_or(f64::NAN), x.net_bp.unwrap_or(f64::NAN),
                            if x.side.as_deref() == Some("逆") && verdict_color(&x.verdict).is_some() { " 逆" } else { "" }),
                    verdict_color(&x.verdict).map(|c| pal::alpha(c, 0.28)),
                ),
                Some(_) => ("样本不足".into(), None),
                None => ("—".into(), None),
            };
            rr = rr.push(
                container(text(label).size(t::s_meta()))
                    .width(Length::Fixed(cw))
                    .height(Length::Fixed(20.0))
                    .align_y(Alignment::Center)
                    .style(move |_t: &Theme| container::Style { background: bg.map(iced::Background::Color), ..Default::default() }),
            );
        }
        if k == "event" {
            if let Some(ro) = r.rolling.iter().find(|x| x.cell == **cell) {
                let wk = ro.weeks.iter().map(|x| format!("{} {:+.2}", &x.week[5..], x.mean_bp)).collect::<Vec<_>>().join("  ");
                rr = rr.push(
                    t::metadata(format!("{wk}{}", if ro.decayed { " · 衰减" } else { "" }))
                        .color(if ro.decayed { pal::warn() } else { pal::dim() }),
                );
            }
        }
        list = list.push(rr);
    }
    col = col.push(
        row![
            w::badge("越过成本", Tone::Success),
            w::badge("有信息未越过成本", Tone::Warning),
            t::metadata("均值 = 扣漂移的中间价 markout（bp，按事件 / 订单流方向）；净 = |均值| − 点差 − 手续费（顺 / 逆取好的一边）。").color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    col.push(scrollable(list).height(Length::Fill)).height(Length::Fill).into()
}

// ── 入场形态矩阵页（P4）───────────────────────────────────────────────

fn setups_tab<'a>(v: &View) -> Element<'a, OfMsg> {
    let Some(ss) = &v.setups else {
        return if v.setups_err.is_empty() {
            w::loading("描述文件").into()
        } else {
            column![w::error("描述文件读不出来", v.setups_err.clone(), "改好后点「重新读取」", None), w::btn("重新读取", Kind::Ghost, Some(OfMsg::RespReload))]
                .spacing(space(2))
                .into()
        };
    };
    let dict = v.dict.as_deref();
    let mut col = column![
        row![
            t::caption("strategies/research/ofms/specs/*.toml：每个描述文件按 OFMS 层写条件，同一份编译到快速档与 Nautilus 完整档。"),
            Space::new().width(Length::Fill),
            w::btn("重新读取", Kind::Ghost, Some(OfMsg::RespReload)),
        ]
        .align_y(Alignment::Center),
        t::metadata("证据列：触发事件在响应表（P3）里 30 秒视界的结论；特征轨（z 表达式）不在响应表里，看策略中心的回测。").color(pal::dim()),
    ]
    .spacing(space(2));
    if let Some(o) = &v.overlay {
        col = col.push(
            row![
                t::body(format!("时间轴上叠加着 {}（{} 笔）", o.spec, o.trades.len())),
                w::btn("取消叠加", Kind::Ghost, Some(OfMsg::ClearOverlay)),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
        );
    }
    const LS: [&str; 9] = ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9"];
    for s in ss.iter() {
        if let Some(e) = &s.error {
            col = col.push(w::error(s.path.clone(), e.clone(), "按报错改描述文件", None));
            continue;
        }
        let verdict = match s.verdict.as_str() {
            "falsified" => ("已证伪", Tone::Danger),
            "alive" | "candidate" => ("存活 / 候选", Tone::Success),
            _ => ("未验证", Tone::Neutral),
        };
        let mut head = row![
            t::body(s.name.clone()),
            t::code(s.id.clone()).size(t::s_small()),
            w::badge(verdict.0, verdict.1),
            w::badge(if s.signal == "events" { "事件轨" } else { "特征轨" }, Tone::Neutral),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center);
        if let Some(r) = &s.replaces {
            head = head.push(t::metadata(format!("重写自 {r}（逐笔对拍）")).color(pal::dim()));
        }
        head = head.push(Space::new().width(Length::Fill));
        if s.signal == "events" {
            head = head.push(w::btn_busy("叠加到时间轴", Kind::Standard, Some(OfMsg::Overlay(s.id.clone())), v.overlay_running));
        }
        let mut grid = column![].spacing(1.0);
        for l in LS {
            let Some(txt) = s.layers.get(l) else { continue };
            let lname = dict.map(|d| d.layer_name(l[1..].parse().unwrap_or(0))).unwrap_or_default();
            grid = grid.push(
                row![
                    container(t::metadata(format!("{l} {lname}")).color(pal::dim())).width(Length::Fixed(150.0)),
                    t::body(txt.clone()),
                ]
                .spacing(space(2)),
            );
        }
        // 证据：触发事件在响应表里 30s 的结论
        if let Some(r) = &v.resp {
            for k in &s.triggers {
                let name = dict.and_then(|d| d.event(k)).map(|e| e.name.clone()).unwrap_or_else(|| k.clone());
                let x = r.rows.iter().find(|x| x.kind == "event" && &x.cell == k && x.h == 30);
                let (txt, c) = match x {
                    Some(x) => (
                        format!("证据 · {name}：30s 均值 {:+.2} bp，净 {:+.1} bp → {}（n={}）", x.mean_bp.unwrap_or(f64::NAN), x.net_bp.unwrap_or(f64::NAN), x.verdict, x.n),
                        verdict_color(&x.verdict).unwrap_or(pal::dim()),
                    ),
                    None => (format!("证据 · {name}：响应表里没有这一格"), pal::dim()),
                };
                grid = grid.push(row![container(t::metadata("证据").color(pal::dim())).width(Length::Fixed(150.0)), t::body(txt).color(c)].spacing(space(2)));
            }
        }
        col = col.push(
            container(column![head, grid].spacing(space(1)))
                .padding(space(2))
                .width(Length::Fill)
                .style(|_t: &Theme| container::Style { background: Some(iced::Background::Color(pal::card_bg())), ..Default::default() }),
        );
    }
    scrollable(col).height(Length::Fill).into()
}
