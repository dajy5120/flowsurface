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

use super::strategy_flow::{self as sf, Control, DebugTab, EXEC, Edge, FlMsg, ModDef, Needs, Node, NodeLive, Palette, ParamDef, Quick, View, form_key, value_text};
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

const PORT_TYPES: [&str; 10] = ["signal", "regime", "scale", "stop", "sizing", "intent", "position", "feature", "series", "flag"];

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
        "out" => "",
        "long" => "做多",
        "short" => "做空",
        "price" => "价位",
        "allow" => "放行",
        "when" => "平仓",
        _ => p,
    }
}

/// 按端口**类型**上色。
/// 端口类型的说法（弹窗标题用）。
fn type_label(ty: &str) -> &str {
    match ty {
        "series" => "数",
        "flag" => "条件",
        other => port_label(other),
    }
}

fn port_color(ty: &str) -> Color {
    pal::series(PORT_TYPES.iter().position(|x| *x == ty).unwrap_or(0))
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

/// 画布上的一个方块：一个节点，或者一个折叠起来的子图。
#[derive(Clone)]
struct Geom {
    /// 节点 id；折叠的子图是 `g:<子图 id>`
    id: String,
    /// 折叠的子图：它的 id
    group: Option<String>,
    /// 它代表哪些节点（节点 = 自己一个）
    members: Vec<String>,
    x: f32,
    y: f32,
    h: f32,
    ins: Vec<String>,
    outs: Vec<String>,
    in_types: Vec<String>,
    out_types: Vec<String>,
    /// 端口实际是哪个节点的哪个端口（折叠的子图把成员对外的端口露出来）
    in_ref: Vec<(String, String)>,
    out_ref: Vec<(String, String)>,
    kind_label: String,
    title: String,
    sub: String,
    /// 无线引用的发布者
    wireless: bool,
    /// 旁路了（画成淡的）；标记：「旁路」「比较 ×3」「静音」（docs/45 C7，不能忘了取消就拿去比较）
    bypass: bool,
    tag: String,
    bad: usize,
    live: Option<NodeLive>,
}

impl Geom {
    fn of(n: &Node, def: Option<&ModDef>) -> Geom {
        let (mut ins, outs) = def.map(|d| (d.ins.clone(), d.outs.clone())).unwrap_or_default();
        // 由连线驱动的参数：各是一个「数」输入端口（docs/45 C8）
        let extra: Vec<String> = n.driven.iter().filter(|p| !ins.contains(p)).cloned().collect();
        ins.extend(extra);
        let in_types = ins.iter().map(|p| if n.driven.contains(p) { "series" } else { def.map_or(sf::port_type(p), |d| d.in_type(p)) }.to_string()).collect();
        let out_types = outs.iter().map(|p| def.map_or(sf::port_type(p), |d| d.out_type(p)).to_string()).collect();
        // 数值节点把最要紧的参数直接写在底栏上（N、常数的值、特征名、表达式）
        let brief = ["n", "value", "name", "tf", "expr"].iter().filter_map(|k| n.params.get(*k).map(value_text)).collect::<Vec<_>>().join(" · ");
        let rows = ins.len().max(outs.len()).max(1) as f32;
        let sub = n
            .lane
            .as_ref()
            .and_then(|l| l.get("name").and_then(|v| v.as_str()).map(|s| format!("线 {s}")))
            .or_else(|| n.name.clone())
            .unwrap_or_else(|| if def.is_some_and(|d| d.kind == "value") { brief.chars().take(22).collect() } else { String::new() });
        Geom {
            id: n.id.clone(),
            group: None,
            members: vec![n.id.clone()],
            x: n.x,
            y: n.y,
            h: HEAD_H + rows * ROW_H + FOOT_H,
            in_ref: ins.iter().map(|p| (n.id.clone(), p.clone())).collect(),
            out_ref: outs.iter().map(|p| (n.id.clone(), p.clone())).collect(),
            ins,
            outs,
            in_types,
            out_types,
            kind_label: def.map(|d| d.kind_label.clone()).unwrap_or_else(|| "未知模块".into()),
            title: def.map(|d| d.title.clone()).unwrap_or_else(|| n.module.clone()),
            sub,
            wireless: n.wireless,
            bypass: n.bypass,
            tag: node_tag(n),
            bad: 0,
            live: None,
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

/// 节点标题条右侧的标记：旁路 / 静音 / 待比较几个候选。
fn node_tag(n: &Node) -> String {
    let mut out = Vec::new();
    if n.bypass {
        out.push("旁路".to_string());
    }
    if n.lane.as_ref().and_then(|l| l.get("mute")).and_then(Value::as_bool) == Some(true) {
        out.push("静音".to_string());
    }
    let k: usize = n.vary.values().filter_map(Value::as_array).map(Vec::len).product();
    if !n.vary.is_empty() {
        out.push(format!("比较×{k}"));
    }
    out.join(" ")
}

fn near(a: Point, b: Point, r: f32) -> bool {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) <= r * r
}

/// 展开着的子图画的框。
#[derive(Clone)]
struct GroupFrame {
    id: String,
    title: String,
    members: Vec<String>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl GroupFrame {
    /// 标题条（点它 = 选中整个子图，拖它 = 整个挪）。
    fn title_hit(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + FRAME_HEAD
    }
}

/// 画出来的一根线：两头是方块的第几个端口。
#[derive(Clone)]
struct Link {
    a: usize,
    ka: usize,
    b: usize,
    kb: usize,
    ty: String,
}

/// 端口旁的小标签：无线引用（↯ 名字），或者进了子图之后通到外面的线。
#[derive(Clone)]
struct Stub {
    geom: usize,
    port: usize,
    input: bool,
    label: String,
    ty: String,
    /// 另一头的节点（选中它时这个标签高亮）
    other: String,
}

/// 这一帧画什么：由图 + 折叠状态 + 正在看的子图算出来（纯函数，好测）。
#[derive(Clone, Default)]
struct Scene {
    geoms: Vec<Geom>,
    frames: Vec<GroupFrame>,
    links: Vec<Link>,
    stubs: Vec<Stub>,
}

const FRAME_HEAD: f32 = 22.0;
const FRAME_PAD: f32 = 10.0;

fn build_scene(v: &View, pal_: &Palette) -> Scene {
    let g = &v.graph;
    let mut bad: HashMap<&str, usize> = HashMap::new();
    for i in v.issues.iter().filter(|i| !i.node.is_empty()) {
        *bad.entry(i.node.as_str()).or_default() += 1;
    }
    // 按 id 查节点 / 子图 / 模块：大图（上千节点）里逐个线性找是平方级的（docs/45 C8）
    let node_of: HashMap<&str, &Node> = g.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let folded: std::collections::HashSet<&str> = g.groups.iter().filter(|x| x.collapsed).map(|x| x.id.as_str()).collect();
    let defs: HashMap<&str, &ModDef> = pal_.modules.iter().map(|m| (m.refer.as_str(), m)).collect();
    let title_of = |n: &Node| n.name.clone().or_else(|| defs.get(n.module.as_str()).map(|d| d.title.clone())).unwrap_or_else(|| n.module.clone());
    let collapsed = |n: &Node| v.focus.is_none() && n.group.as_deref().is_some_and(|gid| folded.contains(gid));
    let visible = |n: &Node| v.focus.is_none() || n.group == v.focus;
    let mut sc = Scene::default();
    // 1. 方块：看得见的节点各一个；折叠的子图合成一个
    let mut where_: HashMap<&str, usize> = HashMap::new(); // 节点 → 它在哪个方块里
    for n in g.nodes.iter().filter(|n| visible(n) && !collapsed(n)) {
        let mut ge = Geom::of(n, defs.get(n.module.as_str()).copied());
        ge.bad = bad.get(n.id.as_str()).copied().unwrap_or(0);
        ge.live = v.live.get(&n.id).cloned();
        where_.insert(n.id.as_str(), sc.geoms.len());
        sc.geoms.push(ge);
    }
    let mut by_group: HashMap<&str, Vec<&Node>> = HashMap::new();
    for n in &g.nodes {
        if let Some(gid) = n.group.as_deref() {
            by_group.entry(gid).or_default().push(n);
        }
    }
    if v.focus.is_none() {
        for gr in g.groups.iter().filter(|x| x.collapsed) {
            let members: Vec<&Node> = by_group.get(gr.id.as_str()).cloned().unwrap_or_default();
            if members.is_empty() {
                continue;
            }
            let member_ids: std::collections::HashSet<&str> = members.iter().map(|m| m.id.as_str()).collect();
            let inside = |id: &str| member_ids.contains(id);
            // 对外的端口：有线通到子图外面的那些，按成员的上下位置排
            let mut ins: Vec<(f32, String, String, String)> = Vec::new();
            let mut outs: Vec<(f32, String, String, String)> = Vec::new();
            for e in &g.edges {
                if inside(&e.to) && !inside(&e.from) && !ins.iter().any(|x| x.1 == e.to && x.2 == e.inp) {
                    let m = members.iter().find(|m| m.id == e.to).expect("成员");
                    let ty = if m.driven.contains(&e.inp) { "series" } else { defs.get(m.module.as_str()).map_or(sf::port_type(&e.inp), |d| d.in_type(&e.inp)) }.to_string();
                    ins.push((m.y, e.to.clone(), e.inp.clone(), ty));
                }
                if inside(&e.from) && !inside(&e.to) && !outs.iter().any(|x| x.1 == e.from && x.2 == e.out) {
                    let m = members.iter().find(|m| m.id == e.from).expect("成员");
                    let ty = defs.get(m.module.as_str()).map_or(sf::port_type(&e.out), |d| d.out_type(&e.out)).to_string();
                    outs.push((m.y, e.from.clone(), e.out.clone(), ty));
                }
            }
            ins.sort_by(|a, b| a.0.total_cmp(&b.0));
            outs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let rows = ins.len().max(outs.len()).max(1) as f32;
            let live = members.iter().filter_map(|m| v.live.get(&m.id)).fold(None::<NodeLive>, |acc, l| match acc {
                None => Some(l.clone()),
                Some(mut a) => {
                    let (n, nb) = (a.n + l.n, a.n_blocked + l.n_blocked);
                    if l.at > a.at {
                        a = l.clone();
                    }
                    a.n = n;
                    a.n_blocked = nb;
                    Some(a)
                }
            });
            let idx = sc.geoms.len();
            for m in &members {
                where_.insert(m.id.as_str(), idx);
            }
            sc.geoms.push(Geom {
                id: format!("g:{}", gr.id),
                group: Some(gr.id.clone()),
                members: members.iter().map(|m| m.id.clone()).collect(),
                x: members.iter().map(|m| m.x).fold(f32::MAX, f32::min),
                y: members.iter().map(|m| m.y).fold(f32::MAX, f32::min),
                h: HEAD_H + rows * ROW_H + FOOT_H,
                ins: ins.iter().map(|x| x.2.clone()).collect(),
                outs: outs.iter().map(|x| x.2.clone()).collect(),
                in_types: ins.iter().map(|x| x.3.clone()).collect(),
                out_types: outs.iter().map(|x| x.3.clone()).collect(),
                in_ref: ins.into_iter().map(|x| (x.1, x.2)).collect(),
                out_ref: outs.into_iter().map(|x| (x.1, x.2)).collect(),
                kind_label: "子图".into(),
                title: gr.title.clone(),
                sub: format!("{} 个节点 · 双击进去看", members.len()),
                wireless: false,
                bypass: false,
                tag: String::new(),
                bad: members.iter().map(|m| bad.get(m.id.as_str()).copied().unwrap_or(0)).sum(),
                live,
            });
        }
        // 2. 展开着的子图：一个框
        let mut geoms_of: HashMap<&str, Vec<&Geom>> = HashMap::new();
        for ge in sc.geoms.iter().filter(|ge| ge.group.is_none()) {
            if let Some(gid) = node_of.get(ge.id.as_str()).and_then(|n| n.group.as_deref()) {
                geoms_of.entry(gid).or_default().push(ge);
            }
        }
        for gr in g.groups.iter().filter(|x| !x.collapsed) {
            let ms: Vec<&Geom> = geoms_of.get(gr.id.as_str()).cloned().unwrap_or_default();
            if ms.is_empty() {
                continue;
            }
            let (x0, y0) = (ms.iter().map(|m| m.x).fold(f32::MAX, f32::min), ms.iter().map(|m| m.y).fold(f32::MAX, f32::min));
            let (x1, y1) = (ms.iter().map(|m| m.x + NODE_W).fold(f32::MIN, f32::max), ms.iter().map(|m| m.y + m.h).fold(f32::MIN, f32::max));
            sc.frames.push(GroupFrame {
                id: gr.id.clone(),
                title: gr.title.clone(),
                members: ms.iter().map(|m| m.id.clone()).collect(),
                x: x0 - FRAME_PAD,
                y: y0 - FRAME_PAD - FRAME_HEAD,
                w: x1 - x0 + 2.0 * FRAME_PAD,
                h: y1 - y0 + 2.0 * FRAME_PAD + FRAME_HEAD,
            });
        }
    }
    // 3. 线：两头都看得见就画线（无线引用画标签）；有一头在外面（进了子图时）画标签
    for e in &g.edges {
        let (a, b) = (where_.get(e.from.as_str()).copied(), where_.get(e.to.as_str()).copied());
        let (from_n, to_n) = (node_of.get(e.from.as_str()).copied(), node_of.get(e.to.as_str()).copied());
        match (a, b) {
            (Some(a), Some(b)) if a == b => {} // 折叠的子图内部的线
            (Some(a), Some(b)) => {
                let (Some(ka), Some(kb)) = (sc.geoms[a].out_ref.iter().position(|r| r.0 == e.from && r.1 == e.out), sc.geoms[b].in_ref.iter().position(|r| r.0 == e.to && r.1 == e.inp)) else { continue };
                let ty = sc.geoms[a].out_types[ka].clone();
                if from_n.is_some_and(|n| n.wireless) {
                    let label = format!("↯ {}", from_n.map(title_of).unwrap_or_default());
                    sc.stubs.push(Stub { geom: b, port: kb, input: true, label, ty, other: e.from.clone() });
                } else {
                    sc.links.push(Link { a, ka, b, kb, ty });
                }
            }
            (Some(a), None) => {
                if let Some(ka) = sc.geoms[a].out_ref.iter().position(|r| r.0 == e.from && r.1 == e.out) {
                    let label = format!("→ {}", to_n.map(title_of).unwrap_or_default());
                    sc.stubs.push(Stub { geom: a, port: ka, input: false, label, ty: sc.geoms[a].out_types[ka].clone(), other: e.to.clone() });
                }
            }
            (None, Some(b)) => {
                if let Some(kb) = sc.geoms[b].in_ref.iter().position(|r| r.0 == e.to && r.1 == e.inp) {
                    let mark = if from_n.is_some_and(|n| n.wireless) { "↯" } else { "←" };
                    let label = format!("{mark} {}", from_n.map(title_of).unwrap_or_default());
                    sc.stubs.push(Stub { geom: b, port: kb, input: true, label, ty: sc.geoms[b].in_types[kb].clone(), other: e.from.clone() });
                }
            }
            (None, None) => {}
        }
    }
    sc
}

// ── 画布 ────────────────────────────────────────────────────────────────

struct FlowCanvas {
    scene: Scene,
    /// 叠显示的耗时（开着「耗时」时才有）：节点 → (毫秒, 次数)
    profile: HashMap<String, (f64, u64)>,
    edges: Vec<Edge>,
    selected: Vec<String>,
    /// 选中的那笔成交经过的节点（按先后）
    path: Vec<String>,
    breakpoints: Vec<String>,
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
        /// 端口类型（决定能接到哪）
        ty: String,
        /// 从方块的哪个位置画出去
        origin: Point,
        at: Point,
    },
    Pan {
        last: Point,
    },
    /// 框选（世界坐标）
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
    /// 上一次点的是什么、什么时候（认双击用）
    last_click: Option<(String, Instant)>,
    size: Size,
}

/// 小地图（右下角）的大小。
const MAP: Size = Size::new(168.0, 104.0);
const MAP_PAD: f32 = 8.0;
/// 缩到多小就不画端口文字与底栏 / 只画标题。
const LOD_PLAIN: f32 = 0.5;
const LOD_TINY: f32 = 0.3;

impl FlowCanvas {
    fn to_world(&self, p: Point) -> Point {
        Point::new((p.x - self.pan.0) / self.zoom, (p.y - self.pan.1) / self.zoom)
    }

    fn to_screen(&self, p: Point) -> Point {
        Point::new(p.x * self.zoom + self.pan.0, p.y * self.zoom + self.pan.1)
    }

    fn out_at(&self, p: Point) -> Option<(String, String, String, Point)> {
        for g in self.scene.geoms.iter().rev() {
            for k in 0..g.outs.len() {
                if near(p, g.out_pos(k), PORT_HIT / self.zoom.max(0.5)) {
                    return Some((g.out_ref[k].0.clone(), g.out_ref[k].1.clone(), g.out_types[k].clone(), g.out_pos(k)));
                }
            }
        }
        None
    }

    fn in_at(&self, p: Point) -> Option<(String, String, String)> {
        for g in self.scene.geoms.iter().rev() {
            for k in 0..g.ins.len() {
                if near(p, g.in_pos(k), PORT_HIT / self.zoom.max(0.5)) {
                    return Some((g.in_ref[k].0.clone(), g.in_ref[k].1.clone(), g.in_types[k].clone()));
                }
            }
        }
        None
    }

    fn node_at(&self, p: Point) -> Option<&Geom> {
        self.scene.geoms.iter().rev().find(|g| g.contains(p))
    }

    fn is_selected(&self, g: &Geom) -> bool {
        !g.members.is_empty() && g.members.iter().all(|m| self.selected.contains(m))
    }

    /// 这个方块现在有多亮（0..1）以及用什么颜色。
    fn glow(&self, g: &Geom) -> Option<(f32, Color)> {
        let l = g.live.as_ref()?;
        let age = self.now.saturating_duration_since(l.at).as_secs_f32();
        (age < GLOW_SECS).then(|| (1.0 - age / GLOW_SECS, live_color(&l.kind)))
    }

    /// 一个端口的记号：条件（flag）画菱形，其余画圆——线型与端口形状区分语义（docs/45 §3.1）。
    fn port_mark(&self, frame: &mut Frame, at: Point, ty: &str) {
        let (c, r) = (self.to_screen(at), PORT_R * self.zoom);
        if ty == "flag" {
            let r = r * 1.25;
            let d = Path::new(|p| {
                p.move_to(Point::new(c.x, c.y - r));
                p.line_to(Point::new(c.x + r, c.y));
                p.line_to(Point::new(c.x, c.y + r));
                p.line_to(Point::new(c.x - r, c.y));
                p.close();
            });
            frame.fill(&d, port_color(ty));
        } else {
            frame.fill(&Path::circle(c, r), port_color(ty));
        }
    }

    /// 数值线细、条件线虚、决策层的线照旧。
    fn wire_of(&self, frame: &mut Frame, a: Point, b: Point, color: Color, width: f32, ty: &str) {
        let (a, b) = (self.to_screen(a), self.to_screen(b));
        let bend = ((b.x - a.x).abs() * 0.5).max(30.0 * self.zoom);
        let path = Path::new(|p| {
            p.move_to(a);
            p.bezier_curve_to(Point::new(a.x + bend, a.y), Point::new(b.x - bend, b.y), b);
        });
        let stroke = Stroke::default().with_color(color);
        match ty {
            "flag" => frame.stroke(&path, Stroke { line_dash: canvas::LineDash { segments: &[6.0, 4.0], offset: 0 }, ..stroke.with_width(width) }),
            "series" => frame.stroke(&path, stroke.with_width((width - 0.5).max(1.0))),
            _ => frame.stroke(&path, stroke.with_width(width)),
        }
    }

    fn label(&self, frame: &mut Frame, s: impl Into<String>, at: Point, color: Color, size: f32) {
        frame.fill_text(Text { content: s.into(), position: self.to_screen(at), color, size: iced::Pixels(size * self.zoom), ..Default::default() });
    }

    fn draw_node(&self, frame: &mut Frame, g: &Geom) {
        let z = self.zoom;
        let tl = self.to_screen(Point::new(g.x, g.y));
        let size = Size::new(NODE_W * z, g.h * z);
        let glow = self.glow(g);
        let body = Path::rounded_rectangle(tl, size, (5.0 * z).into());
        frame.fill(&body, if g.bypass { pal::alpha(pal::card_bg(), 0.35) } else { pal::card_bg() });
        if let Some((k, c)) = glow {
            frame.fill(&body, pal::alpha(c, 0.28 * k));
        }
        // 标题条（折叠的子图用强调色，一眼看得出它不是一个模块）
        let head = Path::rounded_rectangle(tl, Size::new(NODE_W * z, HEAD_H * z), (5.0 * z).into());
        frame.fill(&head, if g.group.is_some() { pal::alpha(pal::accent(), 0.35) } else { pal::band() });
        // 边框：出错 > 运行中的高亮 > 选中 > 平常
        let selected = self.is_selected(g);
        let (color, width) = if g.bad > 0 {
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
        // 选中的那笔成交经过这里：粗框 + 第几步
        if let Some(k) = self.path.iter().position(|n| g.members.contains(n)) {
            frame.stroke(&body, Stroke::default().with_width(3.0).with_color(pal::warn()));
            let c = Point::new(tl.x + NODE_W * z - 2.0 * z, tl.y - 2.0 * z);
            frame.fill(&Path::circle(c, 9.0 * z), pal::warn());
            frame.fill_text(Text { content: (k + 1).to_string(), position: Point::new(c.x - 4.0 * z, c.y - 7.0 * z), color: pal::card_bg(), size: iced::Pixels(t::s_meta() * z), ..Default::default() });
        }
        // 断点：左上角一个红点
        if g.members.iter().any(|m| self.breakpoints.contains(m)) {
            frame.fill(&Path::circle(Point::new(tl.x, tl.y), 5.0 * z), pal::bad());
        }
        if g.group.is_some() {
            // 叠一层边，看着像一摞
            let back = Path::rounded_rectangle(Point::new(tl.x + 4.0 * z, tl.y + 4.0 * z), size, (5.0 * z).into());
            frame.stroke(&back, Stroke::default().with_width(1.0).with_color(pal::alpha(pal::line(), 0.7)));
        }
        let small = t::s_meta();
        if z < LOD_TINY {
            return; // 太小了：只剩色块
        }
        if z < LOD_PLAIN {
            // 缩小了：只写一个大一点的标题，不画端口文字与底栏
            self.label(frame, g.title.clone(), Point::new(g.x + 8.0, g.y + 4.0), pal::head(), small * 1.6);
            for k in 0..g.ins.len() {
                self.port_mark(frame, g.in_pos(k), &g.in_types[k]);
            }
            for k in 0..g.outs.len() {
                self.port_mark(frame, g.out_pos(k), &g.out_types[k]);
            }
            return;
        }
        self.label(frame, g.kind_label.clone(), Point::new(g.x + 8.0, g.y + 5.0), pal::dim(), small);
        self.label(frame, g.title.clone(), Point::new(g.x + 8.0 + 13.0 * g.kind_label.chars().count() as f32, g.y + 5.0), pal::head(), small);
        if g.wireless {
            self.label(frame, "↯", Point::new(g.x + NODE_W - 16.0, g.y + 5.0), pal::accent(), small);
        }
        if !g.tag.is_empty() {
            // 旁路 / 静音 / 待比较：标在节点上方，缩小了也看得见
            self.label(frame, g.tag.clone(), Point::new(g.x + 4.0, g.y - 14.0), pal::warn(), small);
        }
        // 端口
        for (k, port) in g.ins.iter().enumerate() {
            let p = g.in_pos(k);
            self.port_mark(frame, p, &g.in_types[k]);
            self.label(frame, port_label(port), Point::new(p.x + 8.0, p.y - 6.5), pal::txt(), small);
        }
        for (k, port) in g.outs.iter().enumerate() {
            let p = g.out_pos(k);
            self.port_mark(frame, p, &g.out_types[k]);
            self.label(frame, port_label(port), Point::new(p.x - 8.0 - 12.0 * port_label(port).chars().count() as f32, p.y - 6.5), pal::txt(), small);
        }
        // 底栏：线名 / 状态名；运行时换成最近一次决策与计数；开着「耗时」时换成耗时
        let foot_y = g.y + g.h - FOOT_H + 3.0;
        let spent = g.members.iter().filter_map(|m| self.profile.get(m)).fold(None::<(f64, u64)>, |a, x| Some(a.map_or(*x, |a| (a.0 + x.0, a.1 + x.1))));
        if let Some((ms, calls)) = spent {
            let worst = self.profile.values().map(|x| x.0).fold(0.0, f64::max).max(1e-9);
            let k = (ms / worst) as f32;
            frame.fill(&Path::rectangle(self.to_screen(Point::new(g.x, g.y + g.h - 3.0)), Size::new(NODE_W * k * self.zoom, 3.0 * self.zoom)), pal::warn());
            self.label(frame, format!("{ms:.1} ms · {calls} 次"), Point::new(g.x + 8.0, foot_y), if k > 0.5 { pal::warn() } else { pal::dim() }, small);
            return;
        }
        match &g.live {
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
                let s = if g.bad > 0 { format!("{} 个问题", g.bad) } else { g.sub.clone() };
                self.label(frame, s, Point::new(g.x + 8.0, foot_y), if g.bad > 0 { pal::bad() } else { pal::dim() }, small);
            }
        }
    }

    /// 小地图的换算：世界坐标的包围盒 → 小地图里的位置与比例。
    fn map_geometry(&self, bounds: Size) -> Option<(Point, f32, Point)> {
        let gs = &self.scene.geoms;
        if gs.len() < 6 {
            return None; // 几个节点一眼看得全，不用小地图
        }
        let (x0, y0) = (gs.iter().map(|g| g.x).fold(f32::MAX, f32::min), gs.iter().map(|g| g.y).fold(f32::MAX, f32::min));
        let (x1, y1) = (gs.iter().map(|g| g.x + NODE_W).fold(f32::MIN, f32::max), gs.iter().map(|g| g.y + g.h).fold(f32::MIN, f32::max));
        let k = ((MAP.width - 8.0) / (x1 - x0).max(1.0)).min((MAP.height - 8.0) / (y1 - y0).max(1.0));
        let origin = Point::new(bounds.width - MAP.width - MAP_PAD, bounds.height - MAP.height - MAP_PAD);
        Some((origin, k, Point::new(x0, y0)))
    }

    /// 点在小地图上 → 把那个位置挪到画布正中要用的平移量。
    fn map_click(&self, screen: Point, bounds: Size) -> Option<FlMsg> {
        let (origin, k, min) = self.map_geometry(bounds)?;
        if screen.x < origin.x || screen.y < origin.y || screen.x > origin.x + MAP.width || screen.y > origin.y + MAP.height {
            return None;
        }
        let world = Point::new(min.x + (screen.x - origin.x - 4.0) / k, min.y + (screen.y - origin.y - 4.0) / k);
        Some(FlMsg::SetPan(bounds.width / 2.0 - world.x * self.zoom, bounds.height / 2.0 - world.y * self.zoom))
    }

    fn draw_map(&self, frame: &mut Frame, bounds: Size) {
        let Some((origin, k, min)) = self.map_geometry(bounds) else { return };
        let bg = Path::rectangle(origin, MAP);
        frame.fill(&bg, pal::alpha(pal::card_bg(), 0.92));
        frame.stroke(&bg, Stroke::default().with_width(1.0).with_color(pal::line()));
        let at = |x: f32, y: f32| Point::new(origin.x + 4.0 + (x - min.x) * k, origin.y + 4.0 + (y - min.y) * k);
        for g in &self.scene.geoms {
            let c = if g.bad > 0 {
                pal::bad()
            } else if self.is_selected(g) {
                pal::accent()
            } else if g.group.is_some() {
                pal::alpha(pal::accent(), 0.55)
            } else {
                pal::dim()
            };
            frame.fill(&Path::rectangle(at(g.x, g.y), Size::new((NODE_W * k).max(2.0), (g.h * k).max(2.0))), c);
        }
        // 当前视野
        let (a, b) = (self.to_world(Point::ORIGIN), self.to_world(Point::new(bounds.width, bounds.height)));
        let (p, q) = (at(a.x, a.y), at(b.x, b.y));
        let (x0, y0) = (p.x.clamp(origin.x, origin.x + MAP.width), p.y.clamp(origin.y, origin.y + MAP.height));
        let (x1, y1) = (q.x.clamp(origin.x, origin.x + MAP.width), q.y.clamp(origin.y, origin.y + MAP.height));
        frame.stroke(&Path::rectangle(Point::new(x0, y0), Size::new((x1 - x0).max(1.0), (y1 - y0).max(1.0))), Stroke::default().with_width(1.0).with_color(pal::head()));
    }

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
                ("g", true, false) => Some(FlMsg::GroupSelected),
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
        if state.size != bounds.size() {
            // 画布大小变了：告诉状态那边（搜索跳转、看全要用）
            state.size = bounds.size();
            return Some(canvas::Action::publish(FlMsg::Viewport(bounds.width, bounds.height)));
        }
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
                if let Some(m) = self.map_click(screen, bounds.size()) {
                    return Some(canvas::Action::publish(m).and_capture());
                }
                let p = self.to_world(screen);
                if let Some((from, port, ty, at)) = self.out_at(p) {
                    state.drag = Drag::Wire { from, port, ty, origin: at, at };
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if let Some((to, port, _)) = self.in_at(p) {
                    // 点一个已经接了线的输入端口 = 拔线
                    if self.edges.iter().any(|e| e.to == to && e.inp == port) {
                        return Some(canvas::Action::publish(FlMsg::Disconnect(to, port)).and_capture());
                    }
                    return Some(canvas::Action::capture());
                }
                let multi = state.mods.shift() || state.mods.control() || state.mods.command();
                // 双击：折叠的子图 = 进去看；展开的子图的标题 = 折叠起来
                let double = |state: &mut CanvasState, what: &str| {
                    let hit = state.last_click.as_ref().is_some_and(|(id, at)| id == what && at.elapsed().as_millis() < 400);
                    state.last_click = if hit { None } else { Some((what.to_string(), Instant::now())) };
                    hit
                };
                if let Some(g) = self.node_at(p) {
                    if multi {
                        return Some(canvas::Action::publish(FlMsg::SelectToggleMany(g.members.clone())).and_capture());
                    }
                    if let Some(gid) = &g.group
                        && double(state, &g.id)
                    {
                        return Some(canvas::Action::publish(FlMsg::Focus(Some(gid.clone()))).and_capture());
                    }
                    state.drag = Drag::Nodes { last: p };
                    // 点的是已选中的一群里的一个：保持选择，好一起拖
                    if self.selected.iter().any(|s| g.members.contains(s)) && g.members.iter().all(|m| self.selected.contains(m)) {
                        return Some(canvas::Action::capture());
                    }
                    return Some(canvas::Action::publish(FlMsg::SelectBox(g.members.clone(), false)).and_capture());
                }
                if let Some(f) = self.scene.frames.iter().rev().find(|f| f.title_hit(p)) {
                    if double(state, &f.id) {
                        return Some(canvas::Action::publish(FlMsg::ToggleCollapse(f.id.clone())).and_capture());
                    }
                    state.drag = Drag::Nodes { last: p };
                    return Some(canvas::Action::publish(FlMsg::SelectBox(f.members.clone(), multi)).and_capture());
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
                    Drag::Wire { from, port, ty, at, .. } => {
                        // 松手在同类型的输入端口上，或者直接松在一个有这种输入的方块身上
                        let target = self
                            .in_at(at)
                            .filter(|(_, _, t)| *t == ty)
                            .map(|(id, inp, _)| (id, inp))
                            .or_else(|| self.node_at(at).and_then(|g| g.in_types.iter().position(|t| *t == ty).map(|k| g.in_ref[k].clone())));
                        match target {
                            Some((to, inp)) if to != from => Some(canvas::Action::publish(FlMsg::Connect(Edge { from, out: port, to, inp })).and_capture()),
                            Some(_) => Some(canvas::Action::request_redraw()),
                            // 松在别的方块上（类型不对）：什么也不做；松在空白处：问「接什么」
                            None if self.node_at(at).is_some() => Some(canvas::Action::request_redraw()),
                            None => Some(canvas::Action::publish(FlMsg::QuickOpen(Quick { from, port, ty, x: at.x, y: at.y - HEAD_H, query: String::new() })).and_capture()),
                        }
                    }
                    Drag::Box { start, at } => {
                        let ids = self.scene.geoms.iter().filter(|g| g.touches(start, at)).flat_map(|g| g.members.clone()).collect();
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

    fn draw(&self, state: &CanvasState, renderer: &Renderer, theme: &Theme, bounds: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let t0 = Instant::now();
        let out = self.draw_all(state, renderer, theme, bounds, cursor);
        sf::bench_note(None, Some(t0.elapsed().as_secs_f32() * 1000.0));
        out
    }

    fn mouse_interaction(&self, state: &CanvasState, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        self.interaction(state, bounds, cursor)
    }
}

impl FlowCanvas {
    fn draw_all(&self, state: &CanvasState, renderer: &Renderer, _theme: &Theme, bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let sc = &self.scene;
        let small = t::s_meta();
        // 视野裁剪（docs/45 C8）：世界坐标里看得见的那块，外扩一点（标签、端口圈会伸出方块）
        let m = 40.0 / self.zoom;
        let (v0, v1) = (self.to_world(Point::ORIGIN), self.to_world(Point::new(bounds.width, bounds.height)));
        let (vx0, vy0, vx1, vy1) = (v0.x - m, v0.y - m, v1.x + m, v1.y + m);
        let seen = |x0: f32, y0: f32, x1: f32, y1: f32| x1 >= vx0 && x0 <= vx1 && y1 >= vy0 && y0 <= vy1;
        // 展开着的子图的框在最下面
        for f in sc.frames.iter().filter(|f| seen(f.x, f.y, f.x + f.w, f.y + f.h)) {
            let tl = self.to_screen(Point::new(f.x, f.y));
            let rect = Path::rounded_rectangle(tl, Size::new(f.w * self.zoom, f.h * self.zoom), (6.0 * self.zoom).into());
            let picked = f.members.iter().all(|m| self.selected.contains(m));
            frame.fill(&rect, pal::alpha(pal::accent(), 0.05));
            frame.stroke(&rect, Stroke::default().with_width(if picked { 2.0 } else { 1.0 }).with_color(pal::alpha(pal::accent(), if picked { 0.9 } else { 0.45 })));
            if self.zoom >= LOD_TINY {
                self.label(&mut frame, f.title.clone(), Point::new(f.x + 8.0, f.y + 4.0), pal::head(), small * if self.zoom < LOD_PLAIN { 1.6 } else { 1.0 });
            }
        }
        // 连线在方块下面
        for l in &sc.links {
            let (a, b) = (&sc.geoms[l.a], &sc.geoms[l.b]);
            let (pa, pb) = (a.out_pos(l.ka), b.in_pos(l.kb));
            // 曲线不会越出两端的包围盒（控制点在两端之间的水平方向上），外扩一点就够
            let bend = (pb.x - pa.x).abs().max(60.0) * 0.5;
            if !seen(pa.x.min(pb.x) - bend, pa.y.min(pb.y), pa.x.max(pb.x) + bend, pa.y.max(pb.y)) {
                continue;
            }
            // 上游刚做出决策时，这根线跟着亮一下
            let (color, width) = match self.glow(a) {
                Some((k, c)) => (pal::mix(pal::alpha(port_color(&l.ty), 0.75), c, k), 1.5 + 2.0 * k),
                None => (pal::alpha(port_color(&l.ty), 0.75), 1.5),
            };
            self.wire_of(&mut frame, a.out_pos(l.ka), b.in_pos(l.kb), color, width, &l.ty);
        }
        for g in sc.geoms.iter().filter(|g| seen(g.x, g.y - 16.0, g.x + NODE_W, g.y + g.h)) {
            self.draw_node(&mut frame, g);
        }
        // 端口旁的标签：无线引用、通到子图外面的线。选中另一头时高亮
        if self.zoom >= LOD_PLAIN {
            for s in &sc.stubs {
                let g = &sc.geoms[s.geom];
                if !seen(g.x - 140.0, g.y, g.x + NODE_W + 140.0, g.y + g.h) {
                    continue;
                }
                let lit = self.selected.contains(&s.other);
                let color = if lit { pal::accent() } else { pal::alpha(port_color(&s.ty), 0.9) };
                let text: String = s.label.chars().take(14).collect();
                let w = 7.0 * text.chars().count() as f32 + 6.0;
                let p = if s.input { g.in_pos(s.port) } else { g.out_pos(s.port) };
                let at = if s.input { Point::new(p.x - w - 8.0, p.y - 6.5) } else { Point::new(p.x + 10.0, p.y - 6.5) };
                let (a, b) = if s.input { (Point::new(p.x - 8.0, p.y), p) } else { (p, Point::new(p.x + 8.0, p.y)) };
                frame.stroke(&Path::line(self.to_screen(a), self.to_screen(b)), Stroke::default().with_width(1.5).with_color(color));
                self.label(&mut frame, text, at, color, small);
            }
        }
        match &state.drag {
            Drag::Wire { ty, origin, at, .. } => {
                self.wire_of(&mut frame, *origin, *at, port_color(ty), 2.0, "");
                // 拖线时把能接的输入端口圈出来
                for g in &sc.geoms {
                    for (k, _) in g.in_types.iter().enumerate().filter(|(_, t)| *t == ty) {
                        frame.stroke(&Path::circle(self.to_screen(g.in_pos(k)), (PORT_R + 3.0) * self.zoom), Stroke::default().with_width(1.5).with_color(port_color(ty)));
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
        if sc.geoms.len() <= 1 {
            frame.fill_text(Text {
                content: "从左边的模块库点模块加节点；从右侧端口拖到另一个节点左侧的同色端口连线，拖到空白处松手可以直接选要接的模块".to_string(),
                position: Point::new(24.0, bounds.height / 2.0),
                color: pal::dim(),
                size: iced::Pixels(t::s_body()),
                ..Default::default()
            });
        }
        self.draw_map(&mut frame, bounds.size());
        vec![frame.into_geometry()]
    }

    fn interaction(&self, state: &CanvasState, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        let Some(screen) = cursor.position_in(bounds) else { return mouse::Interaction::default() };
        let p = self.to_world(screen);
        match &state.drag {
            Drag::Nodes { .. } | Drag::Pan { .. } => mouse::Interaction::Grabbing,
            Drag::Wire { .. } | Drag::Box { .. } => mouse::Interaction::Crosshair,
            Drag::None if self.map_click(screen, bounds.size()).is_some() => mouse::Interaction::Pointer,
            Drag::None if self.out_at(p).is_some() || self.in_at(p).is_some() => mouse::Interaction::Crosshair,
            Drag::None if self.node_at(p).is_some() || self.scene.frames.iter().any(|f| f.title_hit(p)) => mouse::Interaction::Grab,
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
    // 分类一行一个，点开才列出里面的模块（八十多项全摊开找不到东西）；搜索时全部摊开
    let mut labels: Vec<&str> = Vec::new();
    for m in &pal_.modules {
        if !labels.contains(&m.kind_label.as_str()) {
            labels.push(&m.kind_label);
        }
    }
    for label in labels {
        let hits: Vec<&ModDef> = pal_
            .modules
            .iter()
            .filter(|m| m.kind_label == label)
            .filter(|m| q.is_empty() || m.title.to_lowercase().contains(&q) || m.kind_label.contains(&q) || m.summary.to_lowercase().contains(&q))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let open = !q.is_empty() || v.lib_open.iter().any(|x| x == label);
        col = col.push(
            button(row![t::label(format!("{} {label}", if open { "▾" } else { "▸" })), Space::new().width(Length::Fill), t::metadata(hits.len().to_string()).color(pal::dim())].align_y(Alignment::Center))
                .width(Length::Fill)
                .on_press(FlMsg::LibToggle(label.to_string()))
                .style(|th, st| w::button_style(Kind::Ghost, th, st)),
        );
        if !open {
            continue;
        }
        for m in hits {
            col = col.push(
                button(column![t::label(m.title.clone()), t::metadata(m.summary.chars().take(22).collect::<String>()).color(pal::dim())].spacing(0))
                    .width(Length::Fill)
                    .on_press(FlMsg::AddNode(m.refer.clone()))
                    .style(|th, st| w::button_style(Kind::Ghost, th, st)),
            );
        }
    }
    col = col.push(code_node_section(v));
    crate::ui::scroll(col).height(Length::Fill).into()
}

/// 「新建代码节点」（docs/45 §3.4）：不在画布上写代码，而是生成一个带标准接口的模块文件，在 Studio 里改。
fn code_node_section<'a>(v: &View) -> Element<'a, FlMsg> {
    let kinds: Vec<String> = sf::CODE_KINDS.iter().map(|k| k.to_string()).collect();
    let name_ok = !v.code_name.trim().is_empty();
    let mut c = column![
        w::section("新建代码节点"),
        t::metadata("生成一个模块文件（strategies/modules/<环节>/<名字>.py）并在 Studio 里打开；改完点「契约测试」，过了就出现在上面的模块库里。").color(pal::dim()),
        row![
            pick_list(kinds, Some(v.code_kind.clone()), FlMsg::CodeKind).text_size(t::s_small()),
            text_input("名字，如 my_signal", &v.code_name).on_input(FlMsg::CodeName).size(t::s_small()),
        ]
        .spacing(space(1))
        .wrap(),
        w::btn("生成并打开", Kind::Standard, name_ok.then_some(FlMsg::CodeNew)),
    ]
    .spacing(space(1));
    if let Some((refer, path)) = &v.code_made {
        c = c.push(t::caption(format!("{refer}  {path}")));
        c = c.push(w::btn("契约测试", Kind::Standard, Some(FlMsg::CodeTest)));
        match &v.code_problems {
            Some(p) if p.is_empty() => c = c.push(t::caption("契约测试通过").color(pal::ok())),
            Some(p) => {
                for x in p {
                    c = c.push(t::caption(x.clone()).color(pal::bad()));
                }
            }
            None => {}
        }
    }
    c.into()
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
    // 写成引用（=节点.参数）的参数一律按文字显示，免得开关 / 下拉把引用盖掉
    let ctl = if set.is_some_and(|x| sf::param_ref(x).is_some()) { Control::Text } else { pd.control() };
    let control: Element<'a, FlMsg> = match ctl {
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
    let note = match ctl {
        Control::List if !pd.description.is_empty() => format!("{}（逗号隔开）", pd.description),
        Control::Table if !pd.description.is_empty() => format!("{}（每行：谁 · 哪一项 = 值）", pd.description),
        _ if set.and_then(sf::param_ref).is_some() => format!("引用：与 {} 相同（改那边这里跟着变）", set.and_then(sf::param_ref).unwrap_or_default()),
        _ => pd.description.clone(),
    };
    // 数值参数可以改为由连线驱动（docs/45 C8）：节点上多一个「数」输入端口，接上表达式 / 指标
    let drivable = matches!(pd.ty.as_str(), "number" | "integer") && pd.choices.is_none() && v.palette.as_ref().and_then(|p| p.get(&n.module)).is_some_and(|d| DRIVABLE.contains(&d.kind.as_str()));
    let driven = n.driven.contains(&pd.name);
    let control: Element<'a, FlMsg> = if drivable {
        let (nid, pname) = (n.id.clone(), pd.name.clone());
        let flag = row![checkbox(driven).on_toggle(move |b| FlMsg::Drive(nid.clone(), pname.clone(), b)).size(t::s_small()), t::metadata(if driven { "由连线驱动：接一根「数」线到节点上的这个端口；这里的值只是开头用的" } else { "由连线驱动" }).color(pal::dim())]
            .spacing(space(1))
            .align_y(Alignment::Center);
        column![control, flag].spacing(0).into()
    } else {
        control
    };
    // 在分组里的节点：参数可以勾「暴露」，存成子图资产时成为子图节点上的参数
    if n.group.is_some() && ctl != Control::Table {
        let (nid, pname, on) = (n.id.clone(), pd.name.clone(), n.expose.contains_key(&pd.name));
        let control = column![control, row![checkbox(on).on_toggle(move |b| FlMsg::Expose(nid.clone(), pname.clone(), b)).size(t::s_small()), t::metadata("暴露成子图参数").color(pal::dim())].spacing(space(1)).align_y(Alignment::Center)].spacing(0);
        return labelled(label, control.into(), note);
    }
    labelled(label, control, note)
}

/// 「数据」一节（docs/45 §21）：共用数据选择组件，选完写回 `[backtest]`。Dukascopy 黄金报价在 D3 并入
/// 「B5 本地自有数据」之前走表单里的起止日。
fn data_section<'a>(v: &View, p: &Palette) -> Element<'a, FlMsg> {
    let bt = &v.graph.backtest;
    let src = bt.get("source").and_then(Value::as_str).unwrap_or("dukascopy");
    let mut col = column![w::section("数据"), t::caption(sf::data_label(bt))].spacing(space(1));
    if src == "dukascopy" {
        col = col.push(t::metadata("Dukascopy 黄金报价（整日）。D3 把它并入来源「B5 本地自有数据」之前，起止日在这里填：").color(pal::dim()));
        for pd in p.backtest_params.iter().filter(|pd| pd.name == "start" || pd.name == "end") {
            col = col.push(config_field(v, "backtest", bt, pd));
        }
        col = col.push(t::metadata("或者在下面选已购数据（Databento / Tardis）：").color(pal::dim()));
    } else {
        col = col.push(row![w::btn("↩ 用 Dukascopy 报价", Kind::Ghost, Some(FlMsg::UseDukascopy))]);
    }
    col = col.push(super::data_picker_view::view(&v.pick, &sf::pick_opts()).map(FlMsg::Data));
    col.into()
}

/// 画布左上角的数据牌：这张图拿什么数据跑。点一下 = 取消选中，检查器回到「这份策略」（数据选择器在那里）。
fn data_badge<'a>(v: &View) -> Element<'a, FlMsg> {
    let label = t::caption(format!("数据 · {}", sf::data_label(&v.graph.backtest)));
    container(opaque(button(label).on_press(FlMsg::Select(None)).padding(crate::ui::metrics::pad2(1, 2)).style(|th, st| w::button_style(Kind::Standard, th, st))))
        .padding(space(2))
        .into()
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
    if let Some(gr) = v.selected_group() {
        // 选中的是一整个子图
        let gid = gr.id.clone();
        col = col.push(w::section(format!("子图（{} 个节点）", v.selected.len())));
        let g1 = gid.clone();
        col = col.push(field("名字".into(), "", gr.title.clone(), move |s| FlMsg::GroupTitle(g1.clone(), s), String::new()));
        col = col.push(
            row![
                w::btn(if gr.collapsed { "展开" } else { "折叠" }, Kind::Standard, Some(FlMsg::ToggleCollapse(gid.clone()))),
                w::btn("进去看", Kind::Standard, Some(FlMsg::Focus(Some(gid.clone())))),
                w::btn("解散", Kind::Ghost, Some(FlMsg::Ungroup(gid))),
            ]
            .spacing(space(2))
            .wrap(),
        );
        col = col.push(t::metadata("子图只影响怎么画：折叠、解散都不会改变策略。折叠后顶层只占一个节点，对外的线照常画；双击它进去看。解散只是不再分组，节点都留着。").color(pal::dim()));
        col = col.push(row![w::btn("复制", Kind::Ghost, Some(FlMsg::Copy)), w::btn("删除里面的节点", Kind::Ghost, Some(FlMsg::DeleteSelected))].spacing(space(2)).wrap());
        // 存成子图资产（docs/45 C6）：别的图可以引用它，改一处处处生效
        let from = gr.asset.clone();
        col = col.push(w::section(if from.is_some() { "存回子图资产" } else { "存成子图资产" }));
        let hint = from.as_deref().and_then(|a| a.strip_prefix("sub.")).and_then(|a| a.split_once('@')).map(|(id, _)| id.to_string()).unwrap_or_else(|| "trail_breakeven".into());
        col = col.push(field("资产 id（英文、数字、下划线）".into(), &hint, v.asset_id.clone(), FlMsg::AssetId, if from.is_some() { format!("从 {} 展开来的；留空 = 存回它", from.clone().unwrap_or_default()) } else { String::new() }));
        col = col.push(row![checkbox(v.asset_major).on_toggle(FlMsg::AssetMajor).size(t::s_body()), t::caption("升大版本（接口变了要勾）")].spacing(space(2)).align_y(Alignment::Center));
        col = col.push(w::btn(if from.is_some() { "存回资产" } else { "存成资产" }, Kind::Primary, (!v.busy).then(|| FlMsg::SaveAsset(gr.id.clone()))));
        col = col.push(
            t::metadata("接口 = 从外面接进来、接出去的线，加上节点参数里勾了「暴露」的参数。存完这组节点变成一个引用资产的节点；同一个 id 再存：接口没变就升小版本，所有引用它的图一起变；接口变了要升大版本，老版本留着给已经在用的图。")
                .color(pal::dim()),
        );
        return col.into();
    }
    if v.selected.len() > 1 {
        col = col.push(w::section(format!("选中了 {} 个节点", v.selected.len())));
        col = col.push(w::btn("编成子图（Ctrl G）", Kind::Standard, Some(FlMsg::GroupSelected)));
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
            col = col.push(data_section(v, p));
            col = col.push(w::section("运行配置"));
            for pd in p.settings_params.iter() {
                col = col.push(config_field(v, "settings", &v.graph.settings, pd));
            }
            // 这张图要什么数据（docs/45 §21）：数据源喂不了的，下面「提醒」里有一条「数据：…」
            if !v.needs.kinds.is_empty() {
                let kinds: Vec<&str> = v.needs.kinds.iter().map(|k| Needs::label(k)).collect();
                col = col.push(t::caption(format!("要的数据：{}", kinds.join("、"))));
                if let Some(why) = v.needs.why.get("book") {
                    col = col.push(t::metadata(format!("盘口与成交：{why}")).color(pal::dim()));
                }
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
        for wmsg in &v.warnings {
            col = col.push(t::caption(format!("提醒：{wmsg}")).color(pal::warn()));
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
            "Ctrl G 把选中的编成子图 · 双击折叠的子图 = 进去看 · 双击框的标题 = 折叠",
            "右下角小地图：点哪里跳到哪里；工具条的「整理」自动排版",
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
        if d.kind == "sub" {
            // 子图实例（docs/45 C6）：引用的是资产，不是拷贝
            col = col.push(t::metadata(format!("子图资产 {} · 版本 {} · {}", d.refer, d.version, d.path)).color(pal::dim()));
            col = col.push(t::metadata(format!("里面：{}", d.inner.join(" → "))).color(pal::dim()));
            col = col.push(t::metadata(if d.validations.is_empty() { "验证记录：没有（「能复用」要靠这张表里有那一行）".to_string() } else { format!("验证记录 {} 条", d.validations.len()) }).color(pal::dim()));
            col = col.push(w::btn("解除关联（展开成分组）", Kind::Standard, (!v.busy).then(|| FlMsg::Unlink(id.clone()))));
            col = col.push(t::metadata("资产改了，所有引用它的图一起变；想各改各的就先解除关联。").color(pal::dim()));
        }
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
        let (nid, muted) = (id.clone(), lane.get("mute").and_then(Value::as_bool) == Some(true));
        col = col.push(labelled(
            "静音".into(),
            row![checkbox(muted).on_toggle(move |b| FlMsg::LaneMute(nid.clone(), b)).size(t::s_body()), t::caption(if muted { "不开新仓，持仓照常管理" } else { "照常" })].spacing(space(2)).align_y(Alignment::Center).into(),
            "静音与旁路会标在节点上、写进运行记录：别忘了取消就拿去比较".into(),
        ));
    }
    if let Some(d) = def
        && !d.params.is_empty()
    {
        col = col.push(w::section("参数（★ = 值得优化；留空 = 用缺省值）"));
        col = col.push(t::metadata("填 =节点.参数（如 =n5.sl）= 与那个参数相同").color(pal::dim()));
        // 由连线给出的参数（过滤 / 路由的 regime）不在这里填：接一根线就是了
        for pd in d.params.iter().filter(|pd| !d.ins.contains(&pd.name)) {
            col = col.push(param_field(v, n, pd));
        }
    }
    if let Some(d) = def
        && BYPASSABLE.contains(&d.kind.as_str())
    {
        let (nid, on) = (id.clone(), n.bypass);
        col = col.push(labelled(
            "旁路".into(),
            row![checkbox(on).on_toggle(move |b| FlMsg::Bypass(nid.clone(), b)).size(t::s_body()), t::caption(if on { "当它不存在" } else { "生效" })].spacing(space(2)).align_y(Alignment::Center).into(),
            "过滤 → 全部放行；出场 / 止损 / 缩放 → 不调整；风控 → 不审".into(),
        ));
    }
    if let (Some(d), Some(p)) = (def, &v.palette)
        && !matches!(d.kind.as_str(), "exec" | "value" | "sub")
    {
        col = col.push(vary_section(v, n, d, p));
    }
    if def.is_some_and(|d| !d.outs.is_empty() && d.kind != "value") {
        // 无线引用（docs/45 §4.2）：一个输出给很多个用时不画线，用它的节点在端口旁标名字
        let nid = id.clone();
        col = col.push(labelled(
            "无线输出".into(),
            row![checkbox(n.wireless).on_toggle(move |b| FlMsg::Wireless(nid.clone(), b)).size(t::s_body()), t::caption(if n.wireless { "不画线" } else { "画线" })].spacing(space(2)).align_y(Alignment::Center).into(),
            "给很多节点用的输出（市场状态、特征）开着它，图上少一把线。只影响怎么画".into(),
        ));
    }
    if n.module != EXEC && def.is_some_and(|d| d.kind != "value") {
        let (nid, on) = (id.clone(), v.breakpoints.contains(&n.id));
        col = col.push(labelled(
            "断点".into(),
            row![checkbox(on).on_toggle(move |b| FlMsg::Breakpoint(nid.clone(), b)).size(t::s_body()), t::caption(if on { "它下一次做决策时停" } else { "不停" })].spacing(space(2)).align_y(Alignment::Center).into(),
            "运行时在这里停下，可以一步一步往下走。只在画布上运行时有效".into(),
        ));
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

/// 参数能由连线驱动的环节（与 graph.py 的 DRIVABLE 一致）。
const DRIVABLE: [&str; 7] = ["setup", "confirm", "entry", "stop", "sizing", "scale", "exit"];

/// 能旁路的环节（与 graph.py 的 BYPASSABLE 一致）。
const BYPASSABLE: [&str; 5] = ["confirm", "exit", "stop", "scale", "risk"];

/// 「待比较」（docs/45 C7）：换模块的候选，或几个参数取值。「排进队列」时取所有标记的笛卡尔积，每组一次运行。
fn vary_section<'a>(v: &View, n: &Node, d: &ModDef, p: &Palette) -> Element<'a, FlMsg> {
    let mut c = column![w::section("待比较（排进队列时每个候选跑一次）")].spacing(space(1));
    let picked: Vec<String> = n.vary.get("use").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let others: Vec<&ModDef> = p.modules.iter().filter(|m| m.kind == d.kind && m.refer != n.module).collect();
    if !others.is_empty() {
        c = c.push(t::metadata("换模块：勾上的与现在这个各跑一次").color(pal::dim()));
        for m in others {
            let (nid, r, on) = (n.id.clone(), m.refer.clone(), picked.contains(&m.refer));
            c = c.push(row![checkbox(on).on_toggle(move |b| FlMsg::VaryUse(nid.clone(), r.clone(), b)).size(t::s_small()), t::caption(m.title.clone())].spacing(space(1)).align_y(Alignment::Center));
        }
    }
    let scalar = |pd: &&ParamDef| matches!(pd.ty.as_str(), "integer" | "number" | "boolean" | "string") && pd.choices.is_none();
    let shown: Vec<&ParamDef> = d.params.iter().filter(scalar).filter(|pd| pd.optimize || n.vary.contains_key(&pd.name)).collect();
    if !shown.is_empty() {
        c = c.push(t::metadata("参数：填几个候选值，逗号隔开（只列 ★ 参数）").color(pal::dim()));
    }
    for pd in shown {
        let key = form_key(&n.id, &format!("vary.{}", pd.name));
        let text = v.form.get(&key).cloned().unwrap_or_else(|| n.vary.get(&pd.name).map(value_text).unwrap_or_default());
        let (nid, name) = (n.id.clone(), pd.name.clone());
        c = c.push(field(pd.name.clone(), "如 20, 30, 40", text, move |s| FlMsg::VaryParam(nid.clone(), name.clone(), s), String::new()));
    }
    c.push(t::metadata("换模块与改参数不能标在同一个节点上。每次运行都经策略中心、计入全局试验数。").color(pal::dim())).into()
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
        row![t::caption(format!("接在「{}」后面", type_label(&q.ty))).color(port_color(&q.ty)), Space::new().width(Length::Fill), w::btn("✕", Kind::Ghost, Some(FlMsg::QuickClose))].align_y(Alignment::Center),
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

/// 运行中：暂停 / 继续 / 走到下一个决策（docs/45 C5）。只在运行时出现。
fn run_controls<'a>(v: &View) -> Element<'a, FlMsg> {
    if !v.running {
        return Space::new().into();
    }
    let mut r = row![].spacing(space(1)).align_y(Alignment::Center);
    if v.paused.is_some() {
        r = r.push(w::btn("▶ 继续", Kind::Primary, Some(FlMsg::Ctl("run".into()))));
        r = r.push(w::btn("⏭ 下一个决策", Kind::Standard, Some(FlMsg::Ctl("step".into()))));
    } else {
        r = r.push(w::btn("⏸ 暂停", Kind::Standard, Some(FlMsg::Ctl("pause".into()))));
    }
    r.into()
}

/// 调试面板（画布下方，docs/45 C5）：成交（点一笔亮出来龙去脉）｜为什么没做｜断点。
fn debug_panel<'a>(v: &View) -> Element<'a, FlMsg> {
    let tabs = row![
        w::tabs(&[("成交", DebugTab::Trades), ("为什么没做", DebugTab::WhyNot), ("断点", DebugTab::Breaks), ("队列与对比", DebugTab::Queue)], &v.debug_tab, FlMsg::DebugTab),
        t::metadata(format!("成交 {} 笔 · 断点 {} 个", v.trades.len(), v.breakpoints.len())).color(pal::dim()),
    ]
    .spacing(space(3))
    .align_y(Alignment::Center);
    let body: Element<'a, FlMsg> = match v.debug_tab {
        DebugTab::Trades => {
            let mut list = column![].spacing(0);
            if v.trades.is_empty() {
                list = list.push(t::metadata(if v.running { "还没有平掉的仓位" } else { "运行一次之后，这里列出每一笔（平仓才算一笔）" }).color(pal::dim()));
            }
            for (i, e) in v.trades.iter().rev().take(300) {
                let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let gross = e.get("why").and_then(|w| w.get("gross")).and_then(Value::as_f64);
                let line = format!("{}  线 {}  {}  {}", sf::ts_text(e.get("ts").and_then(Value::as_i64).unwrap_or(0)), s("lane"), s("reason"), gross.map(|g| format!("{g:+.2}")).unwrap_or_default());
                let picked = v.picked_trade == Some(*i);
                list = list.push(
                    button(t::caption(line).color(match gross {
                        Some(g) if g > 0.0 => pal::ok(),
                        Some(g) if g < 0.0 => pal::bad(),
                        _ => pal::txt(),
                    }))
                    .width(Length::Fill)
                    .on_press(FlMsg::PickTrade(*i))
                    .style(move |th, st| w::button_style(if picked { Kind::Standard } else { Kind::Ghost }, th, st)),
                );
            }
            let mut steps = column![t::metadata(if v.trace_path.is_empty() { "点左边一笔，这里列出它从信号到平仓的每一步，画布上亮出经过的节点（黄框上的数字是第几步）" } else { "这一笔的来龙去脉" }).color(pal::dim())].spacing(space(1));
            for st in &v.trace_path {
                let label = if st.kind == "origin" { "依据".to_string() } else { live_label(&st.kind).to_string() };
                steps = steps.push(
                    column![
                        row![t::metadata(sf::ts_text(st.ts)).color(pal::dim()), t::caption(format!("{label} · {}", if st.module.is_empty() { "内核" } else { st.module.as_str() })), t::caption(st.reason.clone()).color(pal::dim())].spacing(space(2)).wrap(),
                        t::caption(st.why.clone()).color(pal::dim()), // 依据要读得清：不用全大写的小字
                    ]
                    .spacing(0),
                );
            }
            row![
                container(crate::ui::scroll(list).height(Length::Fill)).width(Length::FillPortion(2)),
                container(crate::ui::scroll(steps).height(Length::Fill)).width(Length::FillPortion(3)),
            ]
            .spacing(space(3))
            .into()
        }
        DebugTab::WhyNot => {
            let mut c = column![t::metadata("每条线的每一次进场检查停在了哪一关（每小时更新一次，计数从开跑算起）。「检查了」是过了所有过滤之后真正看信号的次数；看了信号但没下单 = 没有信号或已有持仓。").color(pal::dim())].spacing(space(1));
            if v.why_not.is_empty() {
                c = c.push(t::metadata(if v.running { "跑满一小时（回测时间）后出第一份" } else { "运行一次之后才有" }).color(pal::dim()));
            }
            for (lane, m) in &v.why_not {
                let mut items: Vec<(String, i64)> = m.iter().map(|(k, x)| (k.clone(), x.as_i64().unwrap_or(0))).collect();
                items.sort_by(|a, b| b.1.cmp(&a.1));
                let total: i64 = items.iter().filter(|(k, _)| !k.starts_with("信号被否决") && k != "风控没放行" && k != "下了单").map(|x| x.1).sum();
                let text = items.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ");
                c = c.push(row![container(t::caption(format!("线 {lane}"))).width(Length::Fixed(80.0)), t::metadata(format!("进场检查 {total} 次：{text}"))].spacing(space(2)).wrap());
            }
            crate::ui::scroll(c).height(Length::Fill).into()
        }
        DebugTab::Queue => queue_view(v),
        DebugTab::Breaks => {
            let mut c = column![t::metadata("断点：那个模块下一次做出决策（提出意图、否决、缩量、平仓……）时回测停下，停在哪会显示在工具条下方；「⏭ 下一个决策」再走一步。在节点的检查器里加。").color(pal::dim())].spacing(space(1));
            for id in &v.breakpoints {
                let name = v.graph.node(id).map(|n| n.module.clone()).unwrap_or_else(|| id.clone());
                c = c.push(row![t::caption(name), w::btn("取消", Kind::Ghost, Some(FlMsg::Breakpoint(id.clone(), false)))].spacing(space(2)).align_y(Alignment::Center));
            }
            crate::ui::scroll(c).height(Length::Fill).into()
        }
    };
    container(column![tabs, body].spacing(space(1))).height(Length::Fixed(220.0)).width(Length::Fill).into()
}

/// 运行队列（左）与选中那一项的对比表 + 收益曲线叠加（右）。
fn queue_view<'a>(v: &View) -> Element<'a, FlMsg> {
    let mut list = column![t::metadata("「排进队列」= 存图后按工具条上的日期跑一组变体（标了待比较的取全部组合，没标就一次）。依次执行，每次都经策略中心、计入全局试验数。").color(pal::dim())].spacing(0);
    for (i, j) in v.queue.iter().enumerate() {
        let state = match j.status.as_str() {
            "waiting" => "排队".to_string(),
            "running" => format!("跑着 {}/{}", j.n_finished(), j.labels.len().max(1)),
            "done" => format!("完成 {} 组", j.rows.len()),
            "cancelled" => "取消了".to_string(),
            _ => "失败".to_string(),
        };
        let picked = v.queue_pick == Some(i);
        let mut r = row![button(column![t::caption(j.title.clone()), t::metadata(state).color(pal::dim())].spacing(0)).width(Length::Fill).on_press(FlMsg::QueuePick(i)).style(move |th, st| w::button_style(if picked { Kind::Standard } else { Kind::Ghost }, th, st))]
            .spacing(space(1))
            .align_y(Alignment::Center);
        if matches!(j.status.as_str(), "waiting" | "running") {
            r = r.push(w::btn("取消", Kind::Ghost, Some(FlMsg::QueueCancel(i))));
        }
        list = list.push(r);
    }
    let right: Element<'a, FlMsg> = match v.queue_pick.and_then(|i| v.queue.get(i)) {
        None => t::metadata("选左边一项看对比").color(pal::dim()).into(),
        Some(j) => compare_view(j),
    };
    row![
        container(crate::ui::scroll(list).height(Length::Fill)).width(Length::FillPortion(2)),
        container(crate::ui::scroll(right).height(Length::Fill)).width(Length::FillPortion(5)),
    ]
    .spacing(space(3))
    .into()
}

/// 一组变体的对比表（相对第一组的差）与收益曲线叠加。
fn compare_view<'a>(j: &sf::Job) -> Element<'a, FlMsg> {
    let mut c = column![].spacing(space(1));
    if !j.error.is_empty() {
        c = c.push(t::caption(j.error.clone()).color(pal::bad()));
    }
    if !j.marks.is_empty() {
        c = c.push(t::caption(format!("这次跑的时候：{}", j.marks.join("；"))).color(pal::warn()));
    }
    let mut info = format!("批次 {}", if j.study_id.is_empty() { "—" } else { j.study_id.as_str() });
    if let Some(n) = j.global_trials {
        info.push_str(&format!(" · 全局试验数 {n}（每组都计入；DSR 按它打折）"));
    }
    c = c.push(t::metadata(info).color(pal::dim()));
    let cols: [(&str, &str); 7] = [("trades", "笔数"), ("pnl", "盈亏"), ("pnl_pct", "收益 %"), ("sharpe", "夏普"), ("max_dd_pct", "最大回撤 %"), ("profit_factor", "盈亏比"), ("win_rate", "胜率")];
    let cell = |s: String, wd: f32| container(t::caption(s)).width(Length::Fixed(wd));
    let mut head = row![cell("变体".into(), 180.0)].spacing(space(1));
    for (_, label) in cols {
        head = head.push(cell(label.into(), 84.0));
    }
    c = c.push(head);
    let mut rows: Vec<&Value> = j.rows.iter().collect();
    rows.sort_by_key(|r| r.get("i").and_then(Value::as_i64).unwrap_or(0));
    let base = rows.iter().find(|r| r.get("metrics").is_some()).and_then(|r| r.get("metrics")).cloned();
    let best = rows.iter().filter_map(|r| r.get("metrics").and_then(|m| m.get("pnl")).and_then(Value::as_f64)).fold(f64::NEG_INFINITY, f64::max);
    for (k, r) in rows.iter().enumerate() {
        let label = format!("[{k}] {}", r.get("label").and_then(Value::as_str).unwrap_or(""));
        let Some(m) = r.get("metrics") else {
            c = c.push(row![cell(label, 180.0), t::caption(r.get("error").and_then(Value::as_str).unwrap_or("失败").chars().take(80).collect::<String>()).color(pal::bad())].spacing(space(1)));
            continue;
        };
        let top = m.get("pnl").and_then(Value::as_f64) == Some(best) && rows.len() > 1;
        let mut line = row![container(t::caption(label).color(if top { pal::ok() } else { pal::txt() })).width(Length::Fixed(180.0))].spacing(space(1));
        for (key, _) in cols {
            let x = m.get(key).and_then(Value::as_f64);
            let b = base.as_ref().and_then(|b| b.get(key)).and_then(Value::as_f64);
            let s = match (x, b) {
                (Some(x), Some(b)) if k > 0 && (x - b).abs() > 1e-9 => format!("{} ({:+.2})", fmt_num(x), x - b),
                (Some(x), _) => fmt_num(x),
                _ => "—".into(),
            };
            line = line.push(cell(s, 84.0));
        }
        c = c.push(line);
    }
    if rows.len() > 1 {
        c = c.push(t::metadata("括号里是与 [0] 的差；绿色 = 盈亏最高。几组之中挑最好的那组是多重比较：结论要看策略中心按全局试验数打折后的 DSR，或者拿留出段再验。").color(pal::dim()));
    }
    if let Some(cv) = &j.curves {
        if let Some(e) = cv.get("error").and_then(Value::as_str) {
            c = c.push(t::metadata(format!("收益曲线没画：{e}")).color(pal::dim()));
        } else {
            let ts: Vec<i64> = cv.get("t").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
            let series = cv
                .get("series")
                .and_then(Value::as_array)
                .map(|a| a.iter().enumerate().map(|(i, s)| (s.as_array().map(|x| x.iter().map(Value::as_f64).collect()).unwrap_or_default(), pal::series(i), format!("[{i}]"))).collect())
                .unwrap_or_default();
            c = c.push(t::metadata(format!("收益曲线叠加（{}，各自从 0 起算）", cv.get("freq").and_then(Value::as_str).unwrap_or(""))).color(pal::dim()));
            c = c.push(iced::widget::Canvas::new(super::strategy_center_cmp::MultiLine { t: ts, series, unit: "累计 %".into(), cache: Default::default() }).width(Length::Fill).height(Length::Fixed(150.0)));
        }
    }
    c.into()
}

fn fmt_num(x: f64) -> String {
    if x.abs() >= 1000.0 { format!("{x:.0}") } else if x.abs() >= 10.0 { format!("{x:.1}") } else { format!("{x:.2}") }
}

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
        pick_list(v.palette.as_ref().map(|p| p.templates.clone()).unwrap_or_default(), None::<sf::Template>, FlMsg::NewFrom).placeholder("从模板…").text_size(t::s_small()),
        pick_list(v.files.clone(), None::<sf::FileItem>, FlMsg::Open).placeholder("打开 / 导入…").text_size(t::s_small()),
        w::btn("保存", Kind::Standard, idle.then_some(FlMsg::Save)),
        w::btn("↶", Kind::Ghost, v.can_undo.then_some(FlMsg::Undo)),
        w::btn("↷", Kind::Ghost, v.can_redo.then_some(FlMsg::Redo)),
        Space::new().width(Length::Fixed(space(3))),
        t::caption("倍速"),
        text_input("3600", &v.pace).on_input(FlMsg::Pace).size(t::s_small()).width(Length::Fixed(64.0)),
        if v.running { w::btn("■ 停止", Kind::Standard, Some(FlMsg::Stop)) } else { w::btn("▶ 运行", Kind::Primary, idle.then_some(FlMsg::Run)) },
        w::btn("排进队列", Kind::Standard, idle.then_some(FlMsg::Enqueue)),
        run_controls(v),
        w::btn(if v.debug_open { "调试 ▾" } else { "调试 ▸" }, if v.debug_open { Kind::Standard } else { Kind::Ghost }, Some(FlMsg::DebugToggle)),
        Space::new().width(Length::Fill),
        text_input("搜节点，回车跳过去", &v.find).on_input(FlMsg::FindQuery).on_submit(FlMsg::FindNext).size(t::s_small()).width(Length::Fixed(150.0)),
        t::metadata(if v.find.trim().is_empty() { String::new() } else { format!("{}/{}", v.find_hits.1, v.find_hits.0) }).color(pal::dim()),
        w::btn("整理", Kind::Ghost, idle.then_some(FlMsg::Tidy)),
        w::btn("耗时", if v.profile_on { Kind::Standard } else { Kind::Ghost }, Some(FlMsg::ProfileToggle)),
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
    let t0 = Instant::now();
    let v = sf::view();
    let empty = Palette::default();
    let pal_ = v.palette.as_ref().unwrap_or(&empty);
    let program = FlowCanvas {
        profile: v.profile.clone(),
        scene: build_scene(&v, pal_),
        edges: v.graph.edges.clone(),
        selected: v.selected.clone(),
        path: v.path_nodes.clone(),
        breakpoints: v.breakpoints.clone(),
        pan: v.pan,
        zoom: v.zoom,
        now: Instant::now(),
    };
    sf::bench_note(Some(t0.elapsed().as_secs_f32() * 1000.0), None);
    let title = format!("策略画布 · {}{}", v.graph.name, if v.dirty { " *" } else { "" });
    let mut layers = stack![canvas::Canvas::new(program).width(Length::Fill).height(Length::Fill), data_badge(&v)];
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
    if let Some(p) = &v.paused {
        // 调试器停住了：停在哪条决策上
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let what = if s("kind") == "pause" { "手动暂停".to_string() } else { format!("{} · {} · {}", live_label(&s("kind")), if s("module").is_empty() { "内核".to_string() } else { s("module") }, s("reason")) };
        col = col.push(row![w::badge("已暂停", Tone::Warning), t::caption(format!("线 {} · {what}", s("lane"))), t::metadata(sf::ts_text(p.get("ts").and_then(Value::as_i64).unwrap_or(0))).color(pal::dim())].spacing(space(2)).align_y(Alignment::Center));
    }
    if let Some(gr) = v.focus.as_ref().and_then(|f| v.graph.group(f)) {
        // 进了子图：面包屑，点「顶层」回去
        col = col.push(
            row![
                w::btn("← 顶层", Kind::Standard, Some(FlMsg::Focus(None))),
                t::caption(format!("› {}（{} 个节点）", gr.title, v.graph.members(&gr.id).len())),
                t::metadata("通到外面的线画成端口旁的标签").color(pal::dim()),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
        );
    }
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
    if v.debug_open {
        col = col.push(debug_panel(&v));
    }
    if !v.msg.is_empty() {
        col = col.push(t::metadata(v.msg.clone()).color(if v.msg.contains("失败") || v.msg.contains("问题") || v.msg.contains("起不来") { pal::bad() } else { pal::dim() }));
    }
    container(col).width(Length::Fill).height(Length::Fill).into()
}
