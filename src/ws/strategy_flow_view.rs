//! 策略画布视图（docs/44 §21）：工具条 ∣ 模块库 ∣ 画布 ∣ 属性。
//!
//! 只读 [`super::strategy_flow::view`] 的快照，不做 IO。画布每帧重画（运行时的高亮要随时间淡出）。
//!
//! 画布上的操作：
//! - 点左边模块库里的模块 = 加一个节点；拖节点 = 挪位置；点节点 = 选中（右边改参数、换模块、删除）；
//! - 从节点**右侧**的输出端口拖到另一个节点**左侧**的同类型输入端口 = 连线；
//!   点一个已经接了线的输入端口 = 拔掉那根线；
//! - 拖空白处 = 平移；滚轮 = 缩放。

use std::collections::HashMap;
use std::time::Instant;

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{Space, button, column, container, pick_list, row, text_input};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::strategy_flow::{self as sf, EXEC, Edge, FlMsg, ModDef, Node, NodeLive, Palette, View, form_key, value_text};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

// ── 节点的几何（世界坐标；画的时候再乘缩放、加平移）────────────────────

const NODE_W: f32 = 168.0;
const HEAD_H: f32 = 24.0;
const ROW_H: f32 = 17.0;
const FOOT_H: f32 = 18.0;
const PORT_R: f32 = 4.5;
/// 端口的命中半径（比画出来的大，好点）
const PORT_HIT: f32 = 10.0;
/// 高亮从最亮到熄灭的秒数（墙上时间）
const GLOW_SECS: f32 = 2.5;

const PORT_TYPES: [&str; 7] = ["signal", "regime", "scale", "stop", "sizing", "intent", "position"];

fn port_label(p: &str) -> &'static str {
    match p {
        "signal" => "信号",
        "regime" => "状态",
        "scale" => "缩放",
        "stop" => "止损",
        "sizing" => "仓位",
        "intent" => "意图",
        "position" => "持仓",
        _ => "?",
    }
}

fn port_color(p: &str) -> Color {
    pal::series(PORT_TYPES.iter().position(|x| *x == p).unwrap_or(0))
}

/// 运行时一个事件用什么颜色点亮节点。
fn live_color(kind: &str) -> Color {
    match kind {
        "veto" | "risk_reject" | "risk_halt" => pal::bad(),
        "risk_resize" => pal::warn(),
        "fill" | "trade" | "risk_resume" => pal::ok(),
        "regime" | "route" => pal::info(),
        _ => pal::accent(),
    }
}

fn live_label(kind: &str) -> &'static str {
    match kind {
        "regime" => "状态",
        "route" => "路由",
        "intent" => "提出意图",
        "veto" => "否决",
        "risk_reject" => "拒绝",
        "risk_resize" => "缩量",
        "risk_halt" => "停手",
        "risk_resume" => "恢复",
        "fill" => "成交",
        "move_stop" => "挪止损",
        "move_target" => "挪止盈",
        "add" => "加仓",
        "close" => "平仓",
        "trade" => "结束",
        _ => "",
    }
}

#[derive(Clone)]
struct Geom {
    id: String,
    x: f32,
    y: f32,
    h: f32,
    ins: Vec<String>,
    outs: Vec<String>,
    kind_label: String,
    title: String,
    sub: String,
}

impl Geom {
    fn of(n: &Node, def: Option<&ModDef>) -> Geom {
        let (ins, outs) = def.map(|d| (d.ins.clone(), d.outs.clone())).unwrap_or_default();
        let rows = ins.len().max(outs.len()).max(1) as f32;
        let sub = n
            .lane
            .as_ref()
            .and_then(|l| l.get("name").and_then(|v| v.as_str()).map(|s| format!("线 {s}")))
            .or_else(|| n.name.clone())
            .unwrap_or_default();
        Geom {
            id: n.id.clone(),
            x: n.x,
            y: n.y,
            h: HEAD_H + rows * ROW_H + FOOT_H,
            ins,
            outs,
            kind_label: def.map(|d| d.kind_label.clone()).unwrap_or_else(|| "未知模块".into()),
            title: def.map(|d| d.title.clone()).unwrap_or_else(|| n.module.clone()),
            sub,
        }
    }

    fn in_pos(&self, k: usize) -> Point {
        Point::new(self.x, self.y + HEAD_H + ROW_H * (k as f32 + 0.5))
    }

    fn out_pos(&self, k: usize) -> Point {
        Point::new(self.x + NODE_W, self.y + HEAD_H + ROW_H * (k as f32 + 0.5))
    }

    fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + NODE_W && p.y >= self.y && p.y <= self.y + self.h
    }
}

fn near(a: Point, b: Point, r: f32) -> bool {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) <= r * r
}

// ── 画布 ────────────────────────────────────────────────────────────────

struct FlowCanvas {
    geoms: Vec<Geom>,
    edges: Vec<Edge>,
    selected: Option<String>,
    bad: HashMap<String, usize>,
    live: HashMap<String, NodeLive>,
    pan: (f32, f32),
    zoom: f32,
    now: Instant,
}

#[derive(Default)]
enum Drag {
    #[default]
    None,
    Node {
        id: String,
        dx: f32,
        dy: f32,
    },
    Wire {
        from: String,
        port: String,
        at: Point,
    },
    Pan {
        last: Point,
    },
}

impl FlowCanvas {
    fn to_world(&self, p: Point) -> Point {
        Point::new((p.x - self.pan.0) / self.zoom, (p.y - self.pan.1) / self.zoom)
    }

    fn to_screen(&self, p: Point) -> Point {
        Point::new(p.x * self.zoom + self.pan.0, p.y * self.zoom + self.pan.1)
    }

    fn geom(&self, id: &str) -> Option<&Geom> {
        self.geoms.iter().find(|g| g.id == id)
    }

    fn out_at(&self, p: Point) -> Option<(String, String, Point)> {
        for g in self.geoms.iter().rev() {
            for (k, port) in g.outs.iter().enumerate() {
                if near(p, g.out_pos(k), PORT_HIT / self.zoom.max(0.5)) {
                    return Some((g.id.clone(), port.clone(), g.out_pos(k)));
                }
            }
        }
        None
    }

    fn in_at(&self, p: Point) -> Option<(String, String)> {
        for g in self.geoms.iter().rev() {
            for (k, port) in g.ins.iter().enumerate() {
                if near(p, g.in_pos(k), PORT_HIT / self.zoom.max(0.5)) {
                    return Some((g.id.clone(), port.clone()));
                }
            }
        }
        None
    }

    fn node_at(&self, p: Point) -> Option<&Geom> {
        self.geoms.iter().rev().find(|g| g.contains(p))
    }

    /// 这个节点现在有多亮（0..1）以及用什么颜色。
    fn glow(&self, id: &str) -> Option<(f32, Color)> {
        let l = self.live.get(id)?;
        let age = self.now.saturating_duration_since(l.at).as_secs_f32();
        (age < GLOW_SECS).then(|| (1.0 - age / GLOW_SECS, live_color(&l.kind)))
    }

    fn wire(&self, frame: &mut Frame, a: Point, b: Point, color: Color, width: f32) {
        let (a, b) = (self.to_screen(a), self.to_screen(b));
        let bend = ((b.x - a.x).abs() * 0.5).max(30.0 * self.zoom);
        let path = Path::new(|p| {
            p.move_to(a);
            p.bezier_curve_to(Point::new(a.x + bend, a.y), Point::new(b.x - bend, b.y), b);
        });
        frame.stroke(&path, Stroke::default().with_width(width).with_color(color));
    }

    fn label(&self, frame: &mut Frame, s: impl Into<String>, at: Point, color: Color, size: f32) {
        frame.fill_text(Text { content: s.into(), position: self.to_screen(at), color, size: iced::Pixels(size * self.zoom), ..Default::default() });
    }

    fn draw_node(&self, frame: &mut Frame, g: &Geom) {
        let z = self.zoom;
        let tl = self.to_screen(Point::new(g.x, g.y));
        let size = Size::new(NODE_W * z, g.h * z);
        let glow = self.glow(&g.id);
        let body = Path::rounded_rectangle(tl, size, (5.0 * z).into());
        frame.fill(&body, pal::card_bg());
        if let Some((k, c)) = glow {
            frame.fill(&body, pal::alpha(c, 0.28 * k));
        }
        // 标题条
        let head = Path::rounded_rectangle(tl, Size::new(NODE_W * z, HEAD_H * z), (5.0 * z).into());
        frame.fill(&head, pal::band());
        // 边框：出错 > 运行中的高亮 > 选中 > 平常
        let selected = self.selected.as_deref() == Some(g.id.as_str());
        let (color, width) = if self.bad.contains_key(&g.id) {
            (pal::bad(), 2.0)
        } else if let Some((k, c)) = glow {
            (pal::mix(pal::line(), c, k), 1.0 + 2.5 * k)
        } else if selected {
            (pal::accent(), 2.0)
        } else {
            (pal::line(), 1.0)
        };
        frame.stroke(&body, Stroke::default().with_width(width).with_color(color));
        if selected && glow.is_some() {
            frame.stroke(&body, Stroke::default().with_width(1.0).with_color(pal::accent()));
        }
        let small = t::s_meta();
        self.label(frame, g.kind_label.clone(), Point::new(g.x + 8.0, g.y + 5.0), pal::dim(), small);
        self.label(frame, g.title.clone(), Point::new(g.x + 8.0 + 13.0 * g.kind_label.chars().count() as f32, g.y + 5.0), pal::head(), small);
        // 端口
        for (k, port) in g.ins.iter().enumerate() {
            let p = g.in_pos(k);
            frame.fill(&Path::circle(self.to_screen(p), PORT_R * z), port_color(port));
            self.label(frame, port_label(port), Point::new(p.x + 8.0, p.y - 6.5), pal::txt(), small);
        }
        for (k, port) in g.outs.iter().enumerate() {
            let p = g.out_pos(k);
            frame.fill(&Path::circle(self.to_screen(p), PORT_R * z), port_color(port));
            self.label(frame, port_label(port), Point::new(p.x - 8.0 - 12.0 * port_label(port).chars().count() as f32, p.y - 6.5), pal::txt(), small);
        }
        // 底栏：线名 / 状态名；运行时换成最近一次决策与计数
        let foot_y = g.y + g.h - FOOT_H + 3.0;
        match self.live.get(&g.id) {
            Some(l) => {
                let c = glow.map(|(_, c)| c).unwrap_or_else(pal::dim);
                let mut s = format!("{} ×{}", live_label(&l.kind), l.n);
                if l.n_blocked > 0 {
                    s.push_str(&format!("（挡 {}）", l.n_blocked));
                }
                if !l.reason.is_empty() && l.reason.chars().count() <= 8 {
                    s.push_str(&format!(" {}", l.reason));
                }
                self.label(frame, s, Point::new(g.x + 8.0, foot_y), c, small);
            }
            None => {
                let s = match self.bad.get(&g.id) {
                    Some(n) => format!("{n} 个问题"),
                    None => g.sub.clone(),
                };
                self.label(frame, s, Point::new(g.x + 8.0, foot_y), if self.bad.contains_key(&g.id) { pal::bad() } else { pal::dim() }, small);
            }
        }
    }
}

impl canvas::Program<FlMsg> for FlowCanvas {
    type State = Drag;

    fn update(&self, state: &mut Drag, event: &iced::Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<FlMsg>> {
        use iced::Event::Mouse;
        match event {
            Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let screen = cursor.position_in(bounds)?;
                let p = self.to_world(screen);
                if let Some((from, port, at)) = self.out_at(p) {
                    *state = Drag::Wire { from, port, at };
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if let Some((to, port)) = self.in_at(p) {
                    // 点一个已经接了线的输入端口 = 拔线
                    if self.edges.iter().any(|e| e.to == to && e.inp == port) {
                        return Some(canvas::Action::publish(FlMsg::Disconnect(to, port)).and_capture());
                    }
                    return Some(canvas::Action::capture());
                }
                if let Some(g) = self.node_at(p) {
                    *state = Drag::Node { id: g.id.clone(), dx: p.x - g.x, dy: p.y - g.y };
                    return Some(canvas::Action::publish(FlMsg::Select(Some(g.id.clone()))).and_capture());
                }
                *state = Drag::Pan { last: screen };
                Some(canvas::Action::publish(FlMsg::Select(None)).and_capture())
            }
            Mouse(mouse::Event::CursorMoved { .. }) => {
                let screen = cursor.position_in(bounds)?;
                match state {
                    Drag::Node { id, dx, dy } => {
                        let p = self.to_world(screen);
                        Some(canvas::Action::publish(FlMsg::MoveNode(id.clone(), p.x - *dx, p.y - *dy)).and_capture())
                    }
                    Drag::Wire { at, .. } => {
                        *at = self.to_world(screen);
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Drag::Pan { last } => {
                        let (dx, dy) = (screen.x - last.x, screen.y - last.y);
                        *last = screen;
                        Some(canvas::Action::publish(FlMsg::Pan(dx, dy)).and_capture())
                    }
                    Drag::None => None,
                }
            }
            Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let done = std::mem::take(state);
                if let Drag::Wire { from, port, at } = done {
                    // 松手在同类型的输入端口上才算连上
                    if let Some((to, inp)) = self.in_at(at)
                        && inp == port
                        && to != from
                    {
                        return Some(canvas::Action::publish(FlMsg::Connect(Edge { from, out: port, to, inp })).and_capture());
                    }
                    return Some(canvas::Action::request_redraw());
                }
                None
            }
            Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_in(bounds)?;
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                Some(canvas::Action::publish(FlMsg::Zoom(if y > 0.0 { 1.1 } else { 1.0 / 1.1 })).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, state: &Drag, renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // 连线在节点下面
        for e in &self.edges {
            let (Some(a), Some(b)) = (self.geom(&e.from), self.geom(&e.to)) else { continue };
            let (Some(ka), Some(kb)) = (a.outs.iter().position(|p| *p == e.out), b.ins.iter().position(|p| *p == e.inp)) else { continue };
            // 上游节点刚做出决策时，这根线跟着亮一下
            let (color, width) = match self.glow(&e.from) {
                Some((k, c)) => (pal::mix(pal::alpha(port_color(&e.out), 0.75), c, k), 1.5 + 2.0 * k),
                None => (pal::alpha(port_color(&e.out), 0.75), 1.5),
            };
            self.wire(&mut frame, a.out_pos(ka), b.in_pos(kb), color, width);
        }
        for g in &self.geoms {
            self.draw_node(&mut frame, g);
        }
        if let Drag::Wire { from, port, at } = state
            && let Some(a) = self.geom(from)
            && let Some(k) = a.outs.iter().position(|p| p == port)
        {
            self.wire(&mut frame, a.out_pos(k), *at, port_color(port), 2.0);
        }
        if self.geoms.len() <= 1 {
            frame.fill_text(Text {
                content: "从左边的模块库点模块加节点；从右侧端口拖到另一个节点左侧的同色端口连线".to_string(),
                position: Point::new(24.0, bounds.height / 2.0),
                color: pal::dim(),
                size: iced::Pixels(t::s_body()),
                ..Default::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, state: &Drag, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        let Some(screen) = cursor.position_in(bounds) else { return mouse::Interaction::default() };
        let p = self.to_world(screen);
        match state {
            Drag::Node { .. } | Drag::Pan { .. } => mouse::Interaction::Grabbing,
            Drag::Wire { .. } => mouse::Interaction::Crosshair,
            Drag::None if self.out_at(p).is_some() || self.in_at(p).is_some() => mouse::Interaction::Crosshair,
            Drag::None if self.node_at(p).is_some() => mouse::Interaction::Grab,
            Drag::None => mouse::Interaction::default(),
        }
    }
}

// ── 左栏：模块库 ────────────────────────────────────────────────────────

fn palette_col<'a>(v: &View) -> Element<'a, FlMsg> {
    let mut col = column![w::section("模块库（点一下加到画布）")].spacing(space(1));
    let Some(pal_) = &v.palette else {
        return col.push(if v.palette_loading { w::loading("模块库") } else { w::empty("没读到模块库", "点「刷新」") }).into();
    };
    let mut last = String::new();
    for m in &pal_.modules {
        if m.kind_label != last {
            last = m.kind_label.clone();
            col = col.push(t::metadata(last.clone()).color(pal::dim()));
        }
        col = col.push(
            button(column![t::label(m.title.clone()), t::metadata(m.summary.chars().take(22).collect::<String>()).color(pal::dim())].spacing(0))
                .width(Length::Fill)
                .on_press(FlMsg::AddNode(m.refer.clone()))
                .style(|th, st| w::button_style(Kind::Ghost, th, st)),
        );
    }
    crate::ui::scroll(col).height(Length::Fill).into()
}

// ── 右栏：属性 ──────────────────────────────────────────────────────────

fn field<'a>(label: String, hint: &str, value: String, on: impl Fn(String) -> FlMsg + 'a, note: String) -> Element<'a, FlMsg> {
    let mut c = column![row![container(t::caption(label)).width(Length::Fixed(118.0)), text_input(hint, &value).on_input(on).size(t::s_small())].spacing(space(2)).align_y(Alignment::Center)].spacing(0);
    if !note.is_empty() {
        c = c.push(t::metadata(note).color(pal::dim()));
    }
    c.into()
}

fn inspector<'a>(v: &View) -> Element<'a, FlMsg> {
    let mut col = column![].spacing(space(2));
    let node = v.selected.as_ref().and_then(|id| v.graph.node(id));
    let Some(n) = node else {
        // 没选中节点：这张图本身 + 问题清单
        col = col.push(w::section("这份策略"));
        col = col.push(field("id（主题.名字）".into(), "ea.my_strategy", v.graph.id.clone(), FlMsg::MetaId, "决定文件放在哪：strategies/research/<主题>/<名字>".into()));
        col = col.push(field("显示名".into(), "", v.graph.name.clone(), FlMsg::MetaName, String::new()));
        col = col.push(t::metadata(match &v.path {
            Some(p) => format!("文件：{p}"),
            None => "还没保存".to_string(),
        })
        .color(pal::dim()));
        col = col.push(w::section(format!("问题（{}）", v.issues.len())));
        if v.issues.is_empty() {
            col = col.push(t::metadata(match v.checked {
                Some(true) => "检查通过，可以运行",
                Some(false) => "检查没过",
                None => "改过之后还没检查",
            })
            .color(pal::dim()));
        }
        for i in &v.issues {
            let line = t::caption(i.msg.clone()).color(pal::bad());
            col = col.push(if i.node.is_empty() {
                Element::from(line)
            } else {
                button(line).on_press(FlMsg::Select(Some(i.node.clone()))).style(|th, st| w::button_style(Kind::Ghost, th, st)).width(Length::Fill).into()
            });
        }
        return crate::ui::scroll(col).height(Length::Fill).into();
    };
    let def = v.palette.as_ref().and_then(|p| p.get(&n.module));
    let id = n.id.clone();
    col = col.push(w::section(def.map(|d| format!("{} · {}", d.kind_label, d.title)).unwrap_or_else(|| n.module.clone())));
    if let Some(d) = def {
        col = col.push(t::caption(d.summary.clone()));
    }
    for i in v.issues.iter().filter(|i| i.node == n.id) {
        col = col.push(t::caption(i.msg.clone()).color(pal::bad()));
    }
    // 换成同环节的另一个模块
    if let (Some(d), Some(p)) = (def, &v.palette)
        && d.kind != "exec"
    {
        let same: Vec<String> = p.modules.iter().filter(|m| m.kind == d.kind).map(|m| m.refer.clone()).collect();
        if same.len() > 1 {
            let nid = id.clone();
            col = col.push(row![container(t::caption("换模块")).width(Length::Fixed(118.0)), pick_list(same, Some(n.module.clone()), move |r| FlMsg::Swap(nid.clone(), r)).text_size(t::s_small())].spacing(space(2)).align_y(Alignment::Center));
        }
    }
    if n.name.is_some() || def.is_some_and(|d| d.kind == "regime") {
        let nid = id.clone();
        col = col.push(field("状态名".into(), "trend", n.name.clone().unwrap_or_default(), move |s| FlMsg::RegimeName(nid.clone(), s), "过滤与路由靠它引用".into()));
    }
    if let (Some(lane), Some(p)) = (&n.lane, &v.palette) {
        col = col.push(t::metadata("这条线").color(pal::dim()));
        for lp in &p.lane_params {
            let shown = v.form.get(&form_key(&id, &format!("lane.{}", lp.name))).cloned().unwrap_or_else(|| lane.get(&lp.name).map(value_text).unwrap_or_default());
            let (nid, name) = (id.clone(), lp.name.clone());
            col = col.push(field(lp.name.clone(), &value_text(&lp.default), shown, move |s| FlMsg::LaneField(nid.clone(), name.clone(), s), lp.description.clone()));
        }
    }
    if let Some(d) = def
        && !d.params.is_empty()
    {
        col = col.push(t::metadata("参数（★ = 值得优化；留空 = 用缺省值）").color(pal::dim()));
        for pd in &d.params {
            let shown = v.form.get(&form_key(&id, &pd.name)).cloned().unwrap_or_else(|| n.params.get(&pd.name).map(value_text).unwrap_or_default());
            let label = format!("{}{}{}", if pd.optimize { "★ " } else { "" }, pd.name, if pd.required { " *" } else { "" });
            let hint = if pd.required { "必填".to_string() } else { value_text(&pd.default) };
            let (nid, name) = (id.clone(), pd.name.clone());
            col = col.push(field(label, &hint, shown, move |s| FlMsg::Param(nid.clone(), name.clone(), s), pd.description.clone()));
        }
    }
    if let Some(l) = v.live.get(&n.id) {
        col = col.push(w::section("运行中"));
        col = col.push(t::caption(format!("最近：{} {}　共 {} 次，其中被挡 {} 次", live_label(&l.kind), l.reason, l.n, l.n_blocked)).color(live_color(&l.kind)));
    }
    if n.module != EXEC {
        col = col.push(w::btn("删除这个节点", Kind::Ghost, Some(FlMsg::Delete(id))));
    }
    crate::ui::scroll(col).height(Length::Fill).into()
}

// ── 整个面板 ────────────────────────────────────────────────────────────

fn toolbar<'a>(v: &View) -> Element<'a, FlMsg> {
    let state: Element<'a, FlMsg> = if v.running {
        w::badge(format!("运行中 · {} 个决策", v.live_n), Tone::Accent)
    } else {
        match v.checked {
            Some(true) => w::badge("检查通过", Tone::Success),
            Some(false) => w::badge(format!("{} 个问题", v.issues.len()), Tone::Danger),
            None => w::badge(if v.dirty { "未保存" } else { "未检查" }, Tone::Neutral),
        }
    };
    let idle = !v.busy;
    row![
        w::btn("新建", Kind::Ghost, Some(FlMsg::New)),
        pick_list(v.files.clone(), None::<sf::FileItem>, FlMsg::Open).placeholder("打开 / 导入…").text_size(t::s_small()),
        w::btn_busy("检查", Kind::Standard, idle.then_some(FlMsg::Check), v.busy),
        w::btn("保存", Kind::Standard, idle.then_some(FlMsg::Save)),
        Space::new().width(Length::Fixed(space(3))),
        text_input("开始", &v.start).on_input(FlMsg::Start).size(t::s_small()).width(Length::Fixed(96.0)),
        text_input("结束", &v.end).on_input(FlMsg::End).size(t::s_small()).width(Length::Fixed(96.0)),
        t::caption("倍速"),
        text_input("3600", &v.pace).on_input(FlMsg::Pace).size(t::s_small()).width(Length::Fixed(64.0)),
        if v.running { w::btn("■ 停止", Kind::Standard, Some(FlMsg::Stop)) } else { w::btn("▶ 运行", Kind::Primary, idle.then_some(FlMsg::Run)) },
        Space::new().width(Length::Fill),
        state,
        w::btn(format!("{:.0}%", v.zoom * 100.0), Kind::Ghost, Some(FlMsg::ResetView)),
        w::btn("刷新", Kind::Ghost, Some(FlMsg::Refresh)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center)
    .into()
}

pub fn pane_body<'a>() -> Element<'a, FlMsg> {
    let v = sf::view();
    let empty = Palette::default();
    let pal_ = v.palette.as_ref().unwrap_or(&empty);
    let mut bad: HashMap<String, usize> = HashMap::new();
    for i in v.issues.iter().filter(|i| !i.node.is_empty()) {
        *bad.entry(i.node.clone()).or_default() += 1;
    }
    let program = FlowCanvas {
        geoms: v.graph.nodes.iter().map(|n| Geom::of(n, pal_.get(&n.module))).collect(),
        edges: v.graph.edges.clone(),
        selected: v.selected.clone(),
        bad,
        live: v.live.clone(),
        pan: v.pan,
        zoom: v.zoom,
        now: Instant::now(),
    };
    let title = format!("策略画布 · {}{}", v.graph.name, if v.dirty { " *" } else { "" });
    let body = row![
        container(palette_col(&v)).width(Length::Fixed(196.0)).height(Length::Fill),
        container(canvas::Canvas::new(program).width(Length::Fill).height(Length::Fill)).width(Length::Fill).height(Length::Fill).style(|_| container::Style { border: iced::Border { color: pal::line(), width: 1.0, radius: 0.0.into() }, ..Default::default() }),
        container(inspector(&v)).width(Length::Fixed(312.0)).height(Length::Fill),
    ]
    .spacing(space(2))
    .height(Length::Fill);
    let mut col = column![w::panel_header(title, None, vec![]), toolbar(&v), body].spacing(space(2));
    if !v.msg.is_empty() {
        col = col.push(t::metadata(v.msg.clone()).color(if v.msg.contains("失败") || v.msg.contains("问题") || v.msg.contains("起不来") { pal::bad() } else { pal::dim() }));
    }
    container(col).width(Length::Fill).height(Length::Fill).into()
}
