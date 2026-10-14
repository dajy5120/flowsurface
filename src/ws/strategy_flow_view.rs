//! 策略画布视图（docs/44 §21，docs/45 C0）：工具条 ∣ 模块库 ∣ 画布；属性在全局检查器里（docs/42）。
//!
//! 只读 [`super::strategy_flow::view`] 的快照，不做 IO。画布每帧重画（运行时的高亮要随时间淡出）。
//!
//! 画布上的操作：
//! - 点左边模块库里的模块 = 加一个节点；拖节点 = 挪位置（选中了几个就一起挪）；
//! - 点节点 = 选中；Ctrl / Shift 点 = 加选 / 减选；Shift 拖空白 = 框选；
//! - 从节点**右侧**的输出端口拖到另一个节点**左侧**的同类型输入端口（或直接拖到那个节点身上）= 连线；
//!   拖到空白处松手 = 弹出「接什么」，只列能接的模块；点一个已经接了线的输入端口 = 拔线；
//! - 拖空白处 / 按住中键拖 = 平移；滚轮 = 缩放；
//! - 键盘（最后一次点的是画布时才生效，免得在输入框里打字误删节点）：
//!   Delete 删除 · Ctrl Z / Ctrl Shift Z 撤销重做 · Ctrl C / X / V 复制剪切粘贴 · Ctrl D 原地再来一份 ·
//!   Ctrl A 全选 · Esc 取消选择 · F 看全。

use std::collections::HashMap;
use std::time::Instant;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{Space, button, checkbox, column, container, opaque, pick_list, row, stack, text_input};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};
use serde_json::Value;

use super::strategy_flow::{self as sf, Control, EXEC, Edge, FlMsg, ModDef, Node, NodeLive, Palette, ParamDef, Quick, View, form_key, value_text};
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

const PORT_TYPES: [&str; 8] = ["signal", "regime", "scale", "stop", "sizing", "intent", "position", "feature"];

fn port_label(p: &str) -> &str {
    // 特征输入端口：显示参数名里有区分度的那一段（n_feature → n）
    if let Some(stem) = p.strip_suffix("_feature") {
        return stem;
    }
    match p {
        "signal" => "信号",
        "regime" => "状态",
        "scale" => "缩放",
        "stop" => "止损",
        "sizing" => "仓位",
        "intent" => "意图",
        "position" => "持仓",
        "feature" => "特征",
        _ => "?",
    }
}

fn port_color(p: &str) -> Color {
    pal::series(PORT_TYPES.iter().position(|x| *x == sf::port_type(p)).unwrap_or(0))
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

    /// 与框选矩形（任意两个对角点）有没有交叠。
    fn touches(&self, a: Point, b: Point) -> bool {
        let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
        self.x <= x1 && self.x + NODE_W >= x0 && self.y <= y1 && self.y + self.h >= y0
    }
}

fn near(a: Point, b: Point, r: f32) -> bool {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) <= r * r
}

// ── 画布 ────────────────────────────────────────────────────────────────

struct FlowCanvas {
    geoms: Vec<Geom>,
    edges: Vec<Edge>,
    selected: Vec<String>,
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
    /// 拖着选中的节点；`last` 是上一次的世界坐标
    Nodes {
        last: Point,
    },
    Wire {
        from: String,
        port: String,
        at: Point,
    },
    Pan {
        last: Point,
    },
    /// 框选（世界坐标）；`add` = 加到现有选择上
    Box {
        start: Point,
        at: Point,
    },
}

#[derive(Default)]
struct CanvasState {
    drag: Drag,
    mods: keyboard::Modifiers,
    /// 最后一次按下鼠标是在画布里：键盘快捷键只在这时生效
    armed: bool,
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
        let selected = self.selected.contains(&g.id);
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

impl FlowCanvas {
    fn key(&self, state: &CanvasState, key: &Key, mods: keyboard::Modifiers, cursor_world: Option<Point>) -> Option<FlMsg> {
        let ctrl = mods.command() || mods.control();
        match key.as_ref() {
            Key::Named(Named::Delete) => Some(FlMsg::DeleteSelected),
            Key::Named(Named::Escape) => Some(FlMsg::Select(None)),
            Key::Character(c) => match (c.to_lowercase().as_str(), ctrl, mods.shift()) {
                ("z", true, false) => Some(FlMsg::Undo),
                ("z", true, true) | ("y", true, _) => Some(FlMsg::Redo),
                ("c", true, _) => Some(FlMsg::Copy),
                ("x", true, _) => Some(FlMsg::Cut),
                ("v", true, _) => Some(FlMsg::Paste(cursor_world.map(|p| (p.x, p.y)))),
                ("d", true, _) => Some(FlMsg::Duplicate),
                ("a", true, _) => Some(FlMsg::SelectAll),
                ("f", false, false) if matches!(state.drag, Drag::None) => Some(FlMsg::ResetView),
                _ => None,
            },
            _ => None,
        }
    }
}

impl canvas::Program<FlMsg> for FlowCanvas {
    type State = CanvasState;

    fn update(&self, state: &mut CanvasState, event: &iced::Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<FlMsg>> {
        use iced::Event::{Keyboard, Mouse};
        match event {
            Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                state.mods = *m;
                None
            }
            Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                if !state.armed {
                    return None;
                }
                let at = cursor.position_in(bounds).map(|p| self.to_world(p));
                self.key(state, key, *modifiers, at).map(|m| canvas::Action::publish(m).and_capture())
            }
            Mouse(mouse::Event::ButtonPressed(btn @ (mouse::Button::Left | mouse::Button::Middle))) => {
                let Some(screen) = cursor.position_in(bounds) else {
                    state.armed = false;
                    return None;
                };
                state.armed = true;
                if *btn == mouse::Button::Middle {
                    state.drag = Drag::Pan { last: screen };
                    return Some(canvas::Action::capture());
                }
                let p = self.to_world(screen);
                if let Some((from, port, at)) = self.out_at(p) {
                    state.drag = Drag::Wire { from, port, at };
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if let Some((to, port)) = self.in_at(p) {
                    // 点一个已经接了线的输入端口 = 拔线
                    if self.edges.iter().any(|e| e.to == to && e.inp == port) {
                        return Some(canvas::Action::publish(FlMsg::Disconnect(to, port)).and_capture());
                    }
                    return Some(canvas::Action::capture());
                }
                let multi = state.mods.shift() || state.mods.control() || state.mods.command();
                if let Some(g) = self.node_at(p) {
                    if multi {
                        return Some(canvas::Action::publish(FlMsg::SelectToggle(g.id.clone())).and_capture());
                    }
                    state.drag = Drag::Nodes { last: p };
                    // 点的是已选中的一群里的一个：保持选择，好一起拖
                    if self.selected.contains(&g.id) {
                        return Some(canvas::Action::capture());
                    }
                    return Some(canvas::Action::publish(FlMsg::Select(Some(g.id.clone()))).and_capture());
                }
                if state.mods.shift() {
                    state.drag = Drag::Box { start: p, at: p };
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                state.drag = Drag::Pan { last: screen };
                Some(canvas::Action::publish(FlMsg::Select(None)).and_capture())
            }
            Mouse(mouse::Event::CursorMoved { .. }) => {
                let screen = cursor.position_in(bounds)?;
                match &mut state.drag {
                    Drag::Nodes { last } => {
                        let p = self.to_world(screen);
                        let (dx, dy) = (p.x - last.x, p.y - last.y);
                        *last = p;
                        Some(canvas::Action::publish(FlMsg::MoveSelected(dx, dy)).and_capture())
                    }
                    Drag::Wire { at, .. } | Drag::Box { at, .. } => {
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
            Mouse(mouse::Event::ButtonReleased(mouse::Button::Left | mouse::Button::Middle)) => {
                match std::mem::take(&mut state.drag) {
                    Drag::Wire { from, port, at } => {
                        // 松手在同类型的输入端口上，或者直接松在一个有这种输入的节点身上
                        let same = |p: &String| sf::port_type(p) == sf::port_type(&port);
                        let target = self.in_at(at).filter(|(_, inp)| same(inp)).or_else(|| self.node_at(at).and_then(|g| g.ins.iter().find(|p| same(p)).map(|p| (g.id.clone(), p.clone()))));
                        match target {
                            Some((to, inp)) if to != from => Some(canvas::Action::publish(FlMsg::Connect(Edge { from, out: port, to, inp })).and_capture()),
                            Some(_) => Some(canvas::Action::request_redraw()),
                            // 松在别的节点上（类型不对）：什么也不做；松在空白处：问「接什么」
                            None if self.node_at(at).is_some() => Some(canvas::Action::request_redraw()),
                            None => Some(canvas::Action::publish(FlMsg::QuickOpen(Quick { from, port, x: at.x, y: at.y - HEAD_H, query: String::new() })).and_capture()),
                        }
                    }
                    Drag::Box { start, at } => {
                        let ids = self.geoms.iter().filter(|g| g.touches(start, at)).map(|g| g.id.clone()).collect();
                        Some(canvas::Action::publish(FlMsg::SelectBox(ids, state.mods.control() || state.mods.command())).and_capture())
                    }
                    _ => None,
                }
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

    fn draw(&self, state: &CanvasState, renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
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
        match &state.drag {
            Drag::Wire { from, port, at } => {
                if let Some(a) = self.geom(from)
                    && let Some(k) = a.outs.iter().position(|p| p == port)
                {
                    self.wire(&mut frame, a.out_pos(k), *at, port_color(port), 2.0);
                }
                // 拖线时把能接的输入端口圈出来
                for g in &self.geoms {
                    for (k, _) in g.ins.iter().enumerate().filter(|(_, p)| sf::port_type(p) == sf::port_type(port)) {
                        frame.stroke(&Path::circle(self.to_screen(g.in_pos(k)), (PORT_R + 3.0) * self.zoom), Stroke::default().with_width(1.5).with_color(port_color(port)));
                    }
                }
            }
            Drag::Box { start, at } => {
                let (a, b) = (self.to_screen(*start), self.to_screen(*at));
                let rect = Path::rectangle(Point::new(a.x.min(b.x), a.y.min(b.y)), Size::new((a.x - b.x).abs(), (a.y - b.y).abs()));
                frame.fill(&rect, pal::alpha(pal::accent(), 0.10));
                frame.stroke(&rect, Stroke::default().with_width(1.0).with_color(pal::accent()));
            }
            _ => {}
        }
        if self.geoms.len() <= 1 {
            frame.fill_text(Text {
                content: "从左边的模块库点模块加节点；从右侧端口拖到另一个节点左侧的同色端口连线，拖到空白处松手可以直接选要接的模块".to_string(),
                position: Point::new(24.0, bounds.height / 2.0),
                color: pal::dim(),
                size: iced::Pixels(t::s_body()),
                ..Default::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, state: &CanvasState, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        let Some(screen) = cursor.position_in(bounds) else { return mouse::Interaction::default() };
        let p = self.to_world(screen);
        match &state.drag {
            Drag::Nodes { .. } | Drag::Pan { .. } => mouse::Interaction::Grabbing,
            Drag::Wire { .. } | Drag::Box { .. } => mouse::Interaction::Crosshair,
            Drag::None if self.out_at(p).is_some() || self.in_at(p).is_some() => mouse::Interaction::Crosshair,
            Drag::None if self.node_at(p).is_some() => mouse::Interaction::Grab,
            Drag::None => mouse::Interaction::default(),
        }
    }
}

// ── 左栏：模块库 ────────────────────────────────────────────────────────

fn palette_col<'a>(v: &View) -> Element<'a, FlMsg> {
    let mut col = column![w::section("模块库（点一下加到画布）"), text_input("搜模块…", &v.lib_query).on_input(FlMsg::LibQuery).size(t::s_small())].spacing(space(1));
    let Some(pal_) = &v.palette else {
        return col.push(if v.palette_loading { w::loading("模块库") } else { w::empty("没读到模块库", "点「刷新」") }).into();
    };
    let q = v.lib_query.trim().to_lowercase();
    let mut last = String::new();
    for m in pal_.modules.iter().filter(|m| q.is_empty() || m.title.to_lowercase().contains(&q) || m.kind_label.contains(&q) || m.summary.to_lowercase().contains(&q)) {
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

// ── 属性（画进全局检查器，docs/42）──────────────────────────────────────
//
// 检查器很窄而且能拖宽：一律「标签在上、控件在下」，并排的东西都 wrap。

fn labelled<'a>(label: String, control: Element<'a, FlMsg>, note: String) -> Element<'a, FlMsg> {
    let mut c = column![t::caption(label), control].spacing(0).width(Length::Fill);
    if !note.is_empty() {
        c = c.push(t::metadata(note).color(pal::dim()));
    }
    c.into()
}

fn field<'a>(label: String, hint: &str, value: String, on: impl Fn(String) -> FlMsg + 'a, note: String) -> Element<'a, FlMsg> {
    labelled(label, text_input(hint, &value).on_input(on).size(t::s_small()).into(), note)
}

/// 一个参数：按它的类型给控件（docs/45 §4.3）。
fn param_field<'a>(v: &View, n: &Node, pd: &ParamDef) -> Element<'a, FlMsg> {
    let (id, name) = (n.id.clone(), pd.name.clone());
    let set = n.params.get(&pd.name);
    let label = format!("{}{}{}", if pd.optimize { "★ " } else { "" }, pd.name, if pd.required { " *" } else { "" });
    let reset = |on: bool| -> Element<'a, FlMsg> {
        if on { w::btn("用缺省", Kind::Ghost, Some(FlMsg::ParamValue(n.id.clone(), pd.name.clone(), None))) } else { Space::new().into() }
    };
    let control: Element<'a, FlMsg> = match pd.control() {
        Control::Switch => {
            let cur = set.or(Some(&pd.default)).and_then(Value::as_bool).unwrap_or(false);
            row![checkbox(cur).on_toggle(move |b| FlMsg::ParamValue(id.clone(), name.clone(), Some(Value::Bool(b)))).size(t::s_body()), t::caption(if cur { "是" } else { "否" }), reset(set.is_some())]
                .spacing(space(2))
                .align_y(Alignment::Center)
                .wrap()
                .into()
        }
        Control::Choice => {
            let choices = pd.choices.clone().unwrap_or_default();
            let names: Vec<String> = choices.iter().map(value_text).collect();
            let cur = set.or(Some(&pd.default)).map(value_text).filter(|s| names.contains(s));
            row![
                pick_list(names, cur, move |s: String| FlMsg::ParamValue(id.clone(), name.clone(), choices.iter().find(|c| value_text(c) == s).cloned())).text_size(t::s_small()),
                reset(set.is_some())
            ]
            .spacing(space(2))
            .align_y(Alignment::Center)
            .wrap()
            .into()
        }
        Control::Table => {
            let rows = v.tables.get(&form_key(&n.id, &pd.name)).cloned().unwrap_or_else(|| sf::table_rows(set));
            let mut c = column![].spacing(space(1));
            for (i, r) in rows.iter().enumerate() {
                let cell = |col: usize, hint: &'static str, width: f32| {
                    let (id, name) = (n.id.clone(), pd.name.clone());
                    text_input(hint, &r[col]).on_input(move |s| FlMsg::TableEdit(id.clone(), name.clone(), i, col, s)).size(t::s_small()).width(Length::Fixed(width))
                };
                c = c.push(
                    row![cell(0, "谁", 76.0), cell(1, "哪一项", 64.0), cell(2, "值", 48.0), w::btn("✕", Kind::Ghost, Some(FlMsg::TableDel(n.id.clone(), pd.name.clone(), i)))]
                        .spacing(space(1))
                        .align_y(Alignment::Center)
                        .wrap(),
                );
            }
            c.push(w::btn("＋ 加一行", Kind::Ghost, Some(FlMsg::TableAdd(id, name)))).into()
        }
        Control::List | Control::Text => {
            let shown = v.form.get(&form_key(&n.id, &pd.name)).cloned().unwrap_or_else(|| set.map(value_text).unwrap_or_default());
            let hint = if pd.required {
                "必填".to_string()
            } else if pd.control() == Control::List && value_text(&pd.default).is_empty() {
                "逗号隔开".to_string()
            } else {
                value_text(&pd.default)
            };
            text_input(&hint, &shown).on_input(move |s| FlMsg::Param(id.clone(), name.clone(), s)).size(t::s_small()).into()
        }
    };
    let note = match pd.control() {
        Control::List if !pd.description.is_empty() => format!("{}（逗号隔开）", pd.description),
        Control::Table if !pd.description.is_empty() => format!("{}（每行：谁 · 哪一项 = 值）", pd.description),
        _ => pd.description.clone(),
    };
    labelled(label, control, note)
}

/// 表单里的一项（运行配置 / 强制风控）：留空 = 用缺省值，缺省值显示成提示。
fn config_field<'a>(v: &View, section: &'static str, root: &Value, pd: &ParamDef) -> Element<'a, FlMsg> {
    let name = pd.name.clone();
    let set = sf::get_path(root, &pd.name);
    if pd.control() == Control::Choice {
        let names: Vec<String> = pd.choices.clone().unwrap_or_default().iter().map(value_text).collect();
        let cur = set.or(Some(&pd.default)).map(value_text).filter(|s| names.contains(s));
        return labelled(pd.name.clone(), pick_list(names, cur, move |s: String| FlMsg::Config(section.to_string(), name.clone(), s)).text_size(t::s_small()).into(), pd.description.clone());
    }
    let shown = v.form.get(&form_key(section, &pd.name)).cloned().unwrap_or_else(|| set.map(value_text).unwrap_or_default());
    field(pd.name.clone(), &value_text(&pd.default), shown, move |s| FlMsg::Config(section.to_string(), name.clone(), s), pd.description.clone())
}

/// 检查器「属性」页：选中一个节点 = 它的参数；选中多个 = 批量操作；没选 = 这张图与问题清单。
pub fn inspector_panel<'a>() -> Element<'a, FlMsg> {
    let v = sf::view();
    let v = &v;
    let mut col = column![].spacing(space(2)).width(Length::Fill);
    if v.selected.len() > 1 {
        col = col.push(w::section(format!("选中了 {} 个节点", v.selected.len())));
        col = col.push(
            row![
                w::btn("复制", Kind::Standard, Some(FlMsg::Copy)),
                w::btn("再来一份", Kind::Standard, Some(FlMsg::Duplicate)),
                w::btn("删除", Kind::Ghost, Some(FlMsg::DeleteSelected)),
            ]
            .spacing(space(2))
            .wrap(),
        );
        col = col.push(t::metadata("拖其中任何一个 = 一起挪。「执行」节点不会被复制或删除。").color(pal::dim()));
        return col.into();
    }
    let Some(n) = v.primary() else {
        // 没选中节点：这张图本身 + 问题清单
        col = col.push(w::section("这份策略"));
        col = col.push(field("id（主题.名字）".into(), "ea.my_strategy", v.graph.id.clone(), FlMsg::MetaId, "决定文件放在哪：strategies/research/<主题>/<名字>".into()));
        col = col.push(field("显示名".into(), "", v.graph.name.clone(), FlMsg::MetaName, String::new()));
        col = col.push(t::metadata(match &v.path {
            Some(p) => format!("文件：{p}"),
            None => "还没保存".to_string(),
        })
        .color(pal::dim()));
        if let Some(p) = &v.palette {
            // 表单（docs/45 §3.5）：运行配置与账户底线不是节点
            col = col.push(w::section("运行配置"));
            for pd in p.backtest_params.iter() {
                col = col.push(config_field(v, "backtest", &v.graph.backtest, pd));
            }
            for pd in p.settings_params.iter() {
                col = col.push(config_field(v, "settings", &v.graph.settings, pd));
            }
            col = col.push(w::section("强制风控（账户底线）"));
            col = col.push(t::metadata("不在图上，图怎么连都绕不过它；只能拒绝或缩量。留空 = 用缺省的底线，不能关掉。").color(pal::dim()));
            for pd in p.envelope_params.iter() {
                col = col.push(config_field(v, "envelope", &v.graph.envelope, pd));
            }
        }
        col = col.push(w::section(format!("问题（{}）", v.issues.len())));
        if v.issues.is_empty() {
            col = col.push(t::metadata(match v.checked {
                Some(true) => "检查通过，可以运行",
                Some(false) => "检查没过",
                None => "改完停手半秒会自动检查",
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
        col = col.push(w::section("快捷键"));
        for line in [
            "Delete 删除 · Esc 取消选择 · F 看全",
            "Ctrl Z 撤销 · Ctrl Shift Z 重做",
            "Ctrl C / X / V 复制 / 剪切 / 粘贴 · Ctrl D 再来一份",
            "Ctrl 点 = 加选 · Shift 拖空白 = 框选 · Ctrl A 全选",
            "拖线到空白处松手 = 选要接的模块",
        ] {
            col = col.push(t::metadata(line).color(pal::dim()));
        }
        return col.into();
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
            col = col.push(labelled("换模块".into(), pick_list(same, Some(n.module.clone()), move |r| FlMsg::Swap(nid.clone(), r)).text_size(t::s_small()).into(), "名字相同的参数会留着".into()));
        }
    }
    if n.name.is_some() || def.is_some_and(|d| d.kind == "regime") {
        let nid = id.clone();
        col = col.push(field("状态名".into(), "trend", n.name.clone().unwrap_or_default(), move |s| FlMsg::RegimeName(nid.clone(), s), "过滤与路由靠它引用".into()));
    }
    if let (Some(lane), Some(p)) = (&n.lane, &v.palette) {
        col = col.push(w::section("这条线"));
        for lp in &p.lane_params {
            let (nid, name) = (id.clone(), lp.name.clone());
            if lp.control() == Control::Choice {
                let names: Vec<String> = lp.choices.clone().unwrap_or_default().iter().map(value_text).collect();
                let cur = lane.get(&lp.name).or(Some(&lp.default)).map(value_text).filter(|s| names.contains(s));
                col = col.push(labelled(lp.name.clone(), pick_list(names, cur, move |s: String| FlMsg::LaneField(nid.clone(), name.clone(), s)).text_size(t::s_small()).into(), lp.description.clone()));
                continue;
            }
            let shown = v.form.get(&form_key(&id, &format!("lane.{}", lp.name))).cloned().unwrap_or_else(|| lane.get(&lp.name).map(value_text).unwrap_or_default());
            col = col.push(field(lp.name.clone(), &value_text(&lp.default), shown, move |s| FlMsg::LaneField(nid.clone(), name.clone(), s), lp.description.clone()));
        }
    }
    if let Some(d) = def
        && !d.params.is_empty()
    {
        col = col.push(w::section("参数（★ = 值得优化；留空 = 用缺省值）"));
        // 由连线给出的参数（过滤 / 路由的 regime）不在这里填：接一根线就是了
        for pd in d.params.iter().filter(|pd| !d.ins.contains(&pd.name)) {
            col = col.push(param_field(v, n, pd));
        }
    }
    if let Some(l) = v.live.get(&n.id) {
        col = col.push(w::section("运行中"));
        col = col.push(t::caption(format!("最近：{} {}　共 {} 次，其中被挡 {} 次", live_label(&l.kind), l.reason, l.n, l.n_blocked)).color(live_color(&l.kind)));
    }
    if n.module != EXEC {
        col = col.push(w::btn("删除这个节点", Kind::Ghost, Some(FlMsg::Delete(id))));
    }
    col.into()
}

/// 检查器里有没有东西可看（画布始终有：没选中节点时是图的属性与问题清单）。
pub fn in_canvas_view(view: Option<&str>) -> bool {
    view != Some(LIBRARY)
}

/// 拖线拖到空白处弹出的「接什么」：只列有同类型输入端口的模块。
fn quick_popup<'a>(v: &View, q: &Quick) -> Element<'a, FlMsg> {
    let empty = Palette::default();
    let cands = q.candidates(v.palette.as_ref().unwrap_or(&empty));
    let first = cands.first().map(|m| m.refer.clone());
    let mut input = text_input("搜能接的模块…", &q.query).on_input(FlMsg::QuickQuery).size(t::s_small());
    if let Some(r) = first {
        input = input.on_submit(FlMsg::QuickPick(r));
    }
    let mut col = column![
        row![t::caption(format!("接在「{}」后面", port_label(&q.port))).color(port_color(&q.port)), Space::new().width(Length::Fill), w::btn("✕", Kind::Ghost, Some(FlMsg::QuickClose))].align_y(Alignment::Center),
        input
    ]
    .spacing(space(1));
    if cands.is_empty() {
        col = col.push(t::metadata("没有能接的模块").color(pal::dim()));
    }
    for m in cands.into_iter().take(9) {
        col = col.push(
            button(row![t::metadata(m.kind_label.clone()).color(pal::dim()), t::label(m.title.clone())].spacing(space(2)))
                .width(Length::Fill)
                .on_press(FlMsg::QuickPick(m.refer.clone()))
                .style(|th, st| w::button_style(Kind::Ghost, th, st)),
        );
    }
    opaque(container(col).width(Length::Fixed(232.0)).padding(space(2)).style(|_| container::Style {
        background: Some(pal::card_bg().into()),
        border: iced::Border { color: pal::accent(), width: 1.0, radius: 4.0.into() },
        ..Default::default()
    }))
}

// ── 整个面板 ────────────────────────────────────────────────────────────

/// 「模块库」页签的视图名（工作区模板里锁定的 `Settings.view`）。
pub const LIBRARY: &str = "模块库";

fn toolbar<'a>(v: &View) -> Element<'a, FlMsg> {
    let state: Element<'a, FlMsg> = if v.running {
        w::badge(format!("运行中 · {} 个决策", v.live_n), Tone::Accent)
    } else {
        match v.checked {
            Some(true) => w::badge("检查通过", Tone::Success),
            Some(false) => w::badge(format!("{} 个问题", v.issues.len()), Tone::Danger),
            None => w::badge(if v.busy { "检查中" } else if v.dirty { "未保存" } else { "未检查" }, Tone::Neutral),
        }
    };
    let idle = !v.busy;
    row![
        w::btn("新建", Kind::Ghost, Some(FlMsg::New)),
        pick_list(v.files.clone(), None::<sf::FileItem>, FlMsg::Open).placeholder("打开 / 导入…").text_size(t::s_small()),
        w::btn("保存", Kind::Standard, idle.then_some(FlMsg::Save)),
        w::btn("↶", Kind::Ghost, v.can_undo.then_some(FlMsg::Undo)),
        w::btn("↷", Kind::Ghost, v.can_redo.then_some(FlMsg::Redo)),
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

/// `view` = 这个面板锁定的视图：「模块库」页签显示模块注册表，其余是画布。
pub fn pane_body<'a>(view: Option<&str>) -> Element<'a, FlMsg> {
    if view == Some(LIBRARY) {
        return super::strategy_center_view::modules_page().map(FlMsg::Center);
    }
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
    let mut layers = stack![canvas::Canvas::new(program).width(Length::Fill).height(Length::Fill)];
    if let Some(q) = &v.quick {
        // 弹在松手的地方（世界坐标 → 画布内的屏幕坐标）；用占位把它推过去
        let (x, y) = ((q.x * v.zoom + v.pan.0).max(0.0), (q.y * v.zoom + v.pan.1).max(0.0));
        layers = layers.push(column![Space::new().height(Length::Fixed(y)), row![Space::new().width(Length::Fixed(x)), quick_popup(&v, q)]]);
    }
    let body = row![
        container(palette_col(&v)).width(Length::Fixed(196.0)).height(Length::Fill),
        container(layers).width(Length::Fill).height(Length::Fill).clip(true).style(|_| container::Style { border: iced::Border { color: pal::line(), width: 1.0, radius: 0.0.into() }, ..Default::default() }),
    ]
    .spacing(space(2))
    .height(Length::Fill);
    let mut col = column![w::panel_header(title, None, vec![]), toolbar(&v)].spacing(space(2));
    if let Some((what, reason, n)) = &v.envelope_hit {
        // 强制风控层出手了：它不是节点，所以不是哪个节点亮，而是这里一条横幅
        let label = match what.as_str() {
            "halt" => "强制风控：已停手",
            "reject" => "强制风控：拒绝了新单",
            _ => "强制风控：缩了量",
        };
        col = col.push(row![w::badge(label, Tone::Danger), t::caption(format!("{reason}（这次运行共 {n} 次）")).color(pal::bad())].spacing(space(2)).align_y(Alignment::Center));
    }
    col = col.push(body);
    if !v.msg.is_empty() {
        col = col.push(t::metadata(v.msg.clone()).color(if v.msg.contains("失败") || v.msg.contains("问题") || v.msg.contains("起不来") { pal::bad() } else { pal::dim() }));
    }
    container(col).width(Length::Fill).height(Length::Fill).into()
}
