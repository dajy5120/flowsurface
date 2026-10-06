//! 「七层」面板视图（docs/39）：流水线（七个方块 + 漏斗）· K 线标注（滚轮缩放、拖动平移、十字线）· 事件列表 ·
//! 共用件矩阵。只读 [`super::strategy_layers::view`] 的快照，不做 IO。

use std::sync::Arc;

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{Space, button, column, container, row, scrollable};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::strategy_center::LAYERS;
use super::strategy_layers::{self as sl, LyMsg, LyTab, SymData, View};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

pub fn pane_body<'a>() -> Element<'a, LyMsg> {
    let v = sl::view();
    let title = match &v.target {
        Some(tg) => format!("七层 · {}{}", tg.name, if tg.run_id.is_empty() { String::new() } else { format!(" · {}", tg.run_id) }),
        None => "七层".to_string(),
    };
    let mut col = column![w::panel_header(
        title,
        Some(w::tabs(&[("流水线", LyTab::Pipeline), ("共用件矩阵", LyTab::Matrix)], &v.tab, LyMsg::Tab)),
        vec![w::btn("重新读取", Kind::Subtle, Some(LyMsg::Reload))],
    )]
    .spacing(space(2));
    let body: Element<'a, LyMsg> = match v.tab {
        LyTab::Pipeline => pipeline(&v),
        LyTab::Matrix => matrix(&v),
    };
    col = col.push(body);
    container(col).padding(crate::ui::metrics::pad2(2, 3)).width(Length::Fill).height(Length::Fill).into()
}

fn origin_tone(o: &str) -> Tone {
    match o {
        "原文" => Tone::Success,
        "解读" => Tone::Info,
        "代理" => Tone::Warning,
        _ => Tone::Neutral,
    }
}

fn pipeline<'a>(v: &View) -> Element<'a, LyMsg> {
    let Some(tg) = &v.target else {
        return w::empty("在策略中心选一个策略", "七层面板跟随策略中心当前选中的策略与运行").into();
    };
    if !tg.engine_ok {
        return w::empty("这个策略没有按七层拆分", "七层轨迹只来自大师日线引擎（masters.*）的快速回测；订单流 / 做市等策略走 Nautilus，没有分层").into();
    }
    if tg.dir.is_none() {
        return w::empty("这个策略还没有完成的运行", "在策略中心点「⚡ 快速回测」，跑完这里就会显示七层轨迹").into();
    }
    if v.loading {
        return w::loading("七层轨迹").into();
    }
    if !v.err.is_empty() {
        return w::error("七层轨迹读不出来", v.err.clone(), "在策略中心重跑一次快速回测", None).into();
    }
    let Some(d) = &v.data else {
        return Space::new().into();
    };
    let mut col = column![].spacing(space(2));
    if d.kind == "target" {
        col = col.push(t::metadata("目标仓位型：体制与形态合在「目标仓位」计算里，没有确认层；调仓按目标数量在开盘成交。").color(pal::dim()));
    }
    // 七个方块
    let mut boxes = row![].spacing(space(1));
    for (k, label) in LAYERS {
        let fb = d.funnel.iter().find(|f| f.layer == k);
        let sel = v.layer.as_deref() == Some(k);
        let mut c = column![t::label(label)].spacing(2);
        if let Some(mi) = &tg.master {
            if let Some(o) = mi.rules.get(k) {
                c = c.push(w::badge(o.clone(), origin_tone(o)));
            }
            if let Some(cs) = mi.layers.get(k) {
                for comp in cs {
                    let lbl = v
                        .comps
                        .as_ref()
                        .and_then(|x| x.components.iter().find(|y| &y.key == comp))
                        .map(|y| y.label.clone())
                        .unwrap_or_else(|| comp.clone());
                    c = c.push(t::metadata(lbl).color(pal::accent()));
                }
            }
        }
        if let Some(fb) = fb {
            for (name, n) in &fb.rows {
                c = c.push(row![t::metadata(name.clone()).color(pal::dim()), Space::new().width(Length::Fill), t::numeric(n.to_string())]
                    .spacing(space(1)));
            }
        }
        let msg = if sel { LyMsg::Layer(None) } else { LyMsg::Layer(Some(k.to_string())) };
        boxes = boxes.push(
            button(c)
                .width(Length::FillPortion(1))
                .on_press(msg)
                .style(move |th, st| w::button_style(if sel { Kind::Standard } else { Kind::Subtle }, th, st)),
        );
    }
    col = col.push(boxes);
    col = col.push(t::metadata("点一个方块：K 线上只标这一层的事件、下面列出这一层的事件；再点一次回到「关键事件」（成交 / 止损 / 拒绝）。滚轮缩放、拖动平移。").color(pal::dim()));
    // 品种
    let mut syms = row![].spacing(space(1));
    for s in d.symbols.keys() {
        let on = v.symbol.as_deref() == Some(s.as_str());
        syms = syms.push(w::btn(s.clone(), if on { Kind::Standard } else { Kind::Ghost }, Some(LyMsg::Symbol(s.clone()))));
    }
    col = col.push(syms.wrap());
    col = col.push(
        t::metadata("图例：背景 = 体制（蓝 多空都可 · 绿 只多 · 红 只空 · 灰 都不行；持仓期间不评估）　▲▼ 形态信号　● 入场成交　◆ 加仓　✕ 出场（红 = 止损出场 / 确认拒绝）　━ 止损线　○ 确认通过（红 = 体制挡掉）　□ 数量为 0")
            .color(pal::dim()),
    );
    if let Some(s) = v.symbol.as_ref().and_then(|s| d.symbols.get(s)) {
        let s = Arc::new(s.clone());
        col = col.push(
            iced::widget::Canvas::new(LayerChart { s: s.clone(), layer: v.layer.clone() })
                .width(Length::Fill)
                .height(Length::FillPortion(3)),
        );
        col = col.push(events_list(&s, v.layer.as_deref()));
    }
    col.height(Length::Fill).into()
}

fn events_list<'a>(s: &SymData, layer: Option<&str>) -> Element<'a, LyMsg> {
    let picked: Vec<&(i64, String, String, Option<f64>, String)> = s
        .events
        .iter()
        .filter(|e| match layer {
            Some(l) => e.1 == l,
            None => key_event(&e.2),
        })
        .collect();
    let mut col = column![w::section(format!(
        "事件（{}，共 {} 条，最近 200 条）",
        layer.map(|l| LAYERS.iter().find(|(k, _)| *k == l).map(|(_, x)| *x).unwrap_or("")).unwrap_or("关键事件"),
        picked.len()
    ))]
    .spacing(1);
    for e in picked.iter().rev().take(200) {
        col = col.push(
            row![
                container(t::numeric(fmt_t(e.0))).width(Length::Fixed(140.0)),
                container(t::metadata(sl::kind_label(&e.2))).width(Length::Fixed(80.0)),
                container(t::numeric(e.3.map(|x| format!("{x:.4}")).unwrap_or_default())).width(Length::Fixed(110.0)),
                t::metadata(e.4.clone()).color(pal::dim()),
            ]
            .spacing(space(2)),
        );
    }
    scrollable(col).height(Length::FillPortion(1)).into()
}

/// 不选层时只标这些（信号 / 挂单 / 定仓每天都有，全标会糊成一片）。
fn key_event(k: &str) -> bool {
    k.starts_with("fill") || matches!(k, "fail" | "blocked" | "zero" | "init")
}

fn fmt_t(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

// ── K 线标注图 ─────────────────────────────────────────────────────────

const ML: f32 = 58.0;
const MR: f32 = 8.0;
const MT: f32 = 12.0;
const MB: f32 = 18.0;

pub struct LayerChart {
    pub s: Arc<SymData>,
    pub layer: Option<String>,
}

/// 可见区间（K 线下标，左闭右开）与拖动起点。
#[derive(Default)]
pub struct ChartState {
    lo: f64,
    hi: f64,
    drag: Option<(f32, f64, f64)>,
}

impl ChartState {
    fn range(&self, n: usize) -> (f64, f64) {
        if self.hi > self.lo {
            (self.lo, self.hi)
        } else {
            ((n as f64 - 300.0).max(0.0), n as f64)                // 缺省：最近 300 根
        }
    }
}

fn idx_of(t: &[i64], ms: i64) -> f64 {
    t.partition_point(|x| *x < ms) as f64
}

impl canvas::Program<LyMsg> for LayerChart {
    type State = ChartState;

    fn update(&self, st: &mut ChartState, event: &canvas::Event, b: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<LyMsg>> {
        let n = self.s.t.len();
        if n < 2 {
            return None;
        }
        let (lo, hi) = st.range(n);
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
                let (mut a, mut z) = (at - (at - lo) * k, at + (hi - at) * k);
                if z - a < 20.0 {
                    return Some(canvas::Action::capture());
                }
                a = a.max(0.0);
                z = z.min(n as f64);
                st.lo = a;
                st.hi = z;
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(b)?;
                st.drag = Some((p.x, lo, hi));
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.drag = None;
                None
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some((x0, a, z)), Some(p)) = (st.drag, cursor.position_in(b)) {
                    let dx = f64::from((x0 - p.x) / pw) * (z - a);
                    let shift = dx.clamp(-a, n as f64 - z);
                    st.lo = a + shift;
                    st.hi = z + shift;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                Some(canvas::Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(&self, st: &ChartState, r: &Renderer, _th: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(r, b.size());
        let s = &self.s;
        let n = s.t.len();
        if n < 2 {
            return vec![f.into_geometry()];
        }
        let (lo, hi) = st.range(n);
        let i0 = lo.floor().max(0.0) as usize;
        let i1 = (hi.ceil() as usize).min(n);
        let (pw, ph) = ((b.width - ML - MR).max(1.0), (b.height - MT - MB).max(1.0));
        let sx = |i: f64| ML + ((i - lo) / (hi - lo).max(1e-9)) as f32 * pw;
        let (mut ylo, mut yhi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in i0..i1 {
            if let (Some(l), Some(h)) = (s.l[i], s.h[i]) {
                ylo = ylo.min(l);
                yhi = yhi.max(h);
            }
        }
        if !ylo.is_finite() || yhi <= ylo {
            return vec![f.into_geometry()];
        }
        let pad = (yhi - ylo) * 0.06;
        let (ylo, yhi) = (ylo - pad, yhi + pad);
        let sy = |v: f64| MT + ((yhi - v) / (yhi - ylo)) as f32 * ph;
        let sel = self.layer.as_deref();

        // 体制背景
        let reg_alpha = if sel == Some("regime") { 0.22 } else { 0.08 };
        for (a, z, v) in &s.regime {
            let (xa, xz) = (sx(idx_of(&s.t, *a)), sx(idx_of(&s.t, *z)));
            if xz < ML || xa > ML + pw {
                continue;
            }
            let c = match v {
                2 => pal::accent(),
                1 => pal::up(),
                -1 => pal::down(),
                _ => pal::dim(),
            };
            let (xa, xz) = (xa.max(ML), xz.min(ML + pw));
            f.fill_rectangle(Point::new(xa, MT), Size::new((xz - xa).max(0.5), ph), pal::alpha(c, reg_alpha));
        }
        // 网格与纵轴
        for k in 0..=4 {
            let v = ylo + (yhi - ylo) * f64::from(k) / 4.0;
            let y = sy(v);
            f.stroke(&Path::line(Point::new(ML, y), Point::new(ML + pw, y)), Stroke::default().with_width(1.0).with_color(pal::grid()));
            f.fill_text(Text { content: fmt_v(v), position: Point::new(2.0, y - 6.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
        }
        // K 线
        let bw = (pw / (hi - lo).max(1.0) as f32 * 0.6).clamp(1.0, 9.0);
        for i in i0..i1 {
            let (Some(o), Some(h), Some(l), Some(c)) = (s.o[i], s.h[i], s.l[i], s.c[i]) else { continue };
            let x = sx(i as f64 + 0.5);
            let col = if c >= o { crate::ui::chart::up() } else { crate::ui::chart::down() };
            f.stroke(&Path::line(Point::new(x, sy(h)), Point::new(x, sy(l))), Stroke::default().with_width(1.0).with_color(col));
            if bw > 2.0 {
                let (ya, yb) = (sy(o.max(c)), sy(o.min(c)));
                f.fill_rectangle(Point::new(x - bw / 2.0, ya), Size::new(bw, (yb - ya).max(1.0)), col);
            }
        }
        // 止损线
        let stop_w = if sel == Some("stop") { 2.0 } else { 1.0 };
        for (a, z, v) in &s.stops {
            let Some(v) = v else { continue };
            let (xa, xz) = (sx(idx_of(&s.t, *a) + 0.5).max(ML), sx(idx_of(&s.t, *z) + 0.5).min(ML + pw));
            if xz <= xa || *v < ylo || *v > yhi {
                continue;
            }
            f.stroke(&Path::line(Point::new(xa, sy(*v)), Point::new(xz, sy(*v))), Stroke::default().with_width(stop_w).with_color(pal::warn()));
        }
        // 事件
        for e in &s.events {
            let show = match sel {
                Some(l) => e.1 == l,
                None => key_event(&e.2),
            };
            if !show {
                continue;
            }
            let i = idx_of(&s.t, e.0);
            if i < lo || i >= hi {
                continue;
            }
            let ii = (i as usize).min(n - 1);
            let x = sx(i + 0.5);
            let (hh, ll, cc) = (s.h[ii].unwrap_or(f64::NAN), s.l[ii].unwrap_or(f64::NAN), s.c[ii].unwrap_or(f64::NAN));
            let v = e.3.unwrap_or(f64::NAN);
            let (y, mark, color) = match e.2.as_str() {
                "signal" => {
                    if v >= 0.0 { (sy(ll) + 8.0, Mark::Up, pal::up()) } else { (sy(hh) - 8.0, Mark::Down, pal::down()) }
                }
                "order" => (sy(v.abs()), Mark::Dash, pal::accent()),
                "fill_entry" => (sy(v.abs()), Mark::Dot, if v >= 0.0 { pal::up() } else { pal::down() }),
                "fill_add" => (sy(v.abs()), Mark::Diamond, if v >= 0.0 { pal::up() } else { pal::down() }),
                "fill" => (sy(v.abs()), Mark::Cross, if e.1 == "stop" { pal::bad() } else { pal::txt() }),
                "init" | "move" => (sy(v), Mark::Dash, pal::warn()),
                "pass" => (sy(cc), Mark::Ring, pal::ok()),
                "fail" => (sy(cc), Mark::Cross, pal::bad()),
                "blocked" => (sy(cc), Mark::Ring, pal::bad()),
                "zero" => (sy(cc), Mark::Square, pal::warn()),
                "target" => (sy(cc), if v > 0.0 { Mark::Up } else if v < 0.0 { Mark::Down } else { Mark::Ring }, pal::info()),
                _ => (sy(cc), Mark::Small, pal::info()),
            };
            if y.is_finite() {
                draw_mark(&mut f, mark, Point::new(x, y), color);
            }
        }
        // 横轴
        for (k, i) in [(0usize, i0), (1, i1.saturating_sub(1))] {
            let lbl = fmt_t(s.t[i]);
            let x = if k == 0 { ML } else { ML + pw - lbl.len() as f32 * 5.0 };
            f.fill_text(Text { content: lbl, position: Point::new(x, MT + ph + 3.0), color: pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
        }
        let mut out = vec![f.into_geometry()];
        // 十字线
        if let Some(p) = cursor.position_in(b) {
            let plot = super::chart_kit::Plot { x0: ML, y0: MT, w: pw, h: ph, xr: (lo, hi), yr: (ylo, yhi) };
            let mut hair = Frame::new(r, b.size());
            let i = (plot.vx(p.x).floor().max(0.0) as usize).min(n - 1);
            let mut extra = vec![format!(
                "开 {} 高 {} 低 {} 收 {}",
                s.o[i].map(fmt_v).unwrap_or_default(),
                s.h[i].map(fmt_v).unwrap_or_default(),
                s.l[i].map(fmt_v).unwrap_or_default(),
                s.c[i].map(fmt_v).unwrap_or_default()
            )];
            for e in s.events.iter().filter(|e| e.0 == s.t[i] && sel.is_none_or(|l| e.1 == l)).take(5) {
                extra.push(format!("{} {}", sl::kind_label(&e.2), e.4));
            }
            let ti = s.t[i];
            super::chart_kit::crosshair(&mut hair, &plot, p, move |_| fmt_t(ti), fmt_v, &extra);
            out.push(hair.into_geometry());
        }
        out
    }

    fn mouse_interaction(&self, st: &ChartState, b: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if st.drag.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(b) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

#[derive(Clone, Copy)]
enum Mark {
    Up,
    Down,
    Dot,
    Diamond,
    Cross,
    Dash,
    Ring,
    Square,
    Small,
}

/// 事件标记（几何图形，不依赖字体里有没有这些符号）。
fn draw_mark(f: &mut Frame, m: Mark, p: Point, c: Color) {
    const R: f32 = 4.5;
    let stroke = Stroke::default().with_width(2.0).with_color(c);
    match m {
        Mark::Up | Mark::Down => {
            let d = if matches!(m, Mark::Up) { -1.0 } else { 1.0 };
            let tri = Path::new(|b| {
                b.move_to(Point::new(p.x, p.y + d * R));
                b.line_to(Point::new(p.x - R, p.y - d * R));
                b.line_to(Point::new(p.x + R, p.y - d * R));
                b.close();
            });
            f.fill(&tri, c);
        }
        Mark::Dot => {
            f.fill(&Path::circle(p, R), c);
            f.stroke(&Path::circle(p, R + 1.0), Stroke::default().with_width(1.0).with_color(pal::txt()));
        }
        Mark::Diamond => {
            let dm = Path::new(|b| {
                b.move_to(Point::new(p.x, p.y - R));
                b.line_to(Point::new(p.x + R, p.y));
                b.line_to(Point::new(p.x, p.y + R));
                b.line_to(Point::new(p.x - R, p.y));
                b.close();
            });
            f.fill(&dm, c);
        }
        Mark::Cross => {
            f.stroke(&Path::line(Point::new(p.x - R, p.y - R), Point::new(p.x + R, p.y + R)), stroke);
            f.stroke(&Path::line(Point::new(p.x - R, p.y + R), Point::new(p.x + R, p.y - R)), stroke);
        }
        Mark::Dash => f.stroke(&Path::line(Point::new(p.x - R, p.y), Point::new(p.x + R, p.y)), stroke),
        Mark::Ring => f.stroke(&Path::circle(p, R), stroke),
        Mark::Square => f.stroke(&Path::rectangle(Point::new(p.x - R, p.y - R), Size::new(2.0 * R, 2.0 * R)), stroke),
        Mark::Small => f.fill(&Path::circle(p, 2.0), c),
    }
}

fn fmt_v(v: f64) -> String {
    if v.abs() >= 1000.0 { format!("{v:.0}") } else if v.abs() >= 1.0 { format!("{v:.2}") } else { format!("{v:.5}") }
}

// ── 共用件矩阵（第 3 期）─────────────────────────────────────────────

fn matrix<'a>(v: &View) -> Element<'a, LyMsg> {
    if v.comps_loading {
        return w::loading("共用件目录").into();
    }
    if !v.comps_err.is_empty() {
        return w::error("共用件目录读不出来", v.comps_err.clone(), "确认 ~/ws-venv 可用后点「重新读取」", None).into();
    }
    let Some(c) = &v.comps else {
        return Space::new().into();
    };
    const CW: f32 = 26.0;
    const NW: f32 = 170.0;
    let strategies: Vec<(&String, &String)> = c.strategies.iter().collect();
    let sel_strategy = v.target.as_ref().map(|t| t.strategy.clone());
    // 列头：竖排太难读，用序号 + 悬停看名字；下方给对照表
    let mut head = row![container(t::metadata("共用件 ＼ 策略")).width(Length::Fixed(NW))].spacing(1);
    for (k, (id, name)) in strategies.iter().enumerate() {
        let on = sel_strategy.as_deref() == Some(id.as_str());
        let tip = iced::widget::tooltip(
            container(t::numeric(format!("{}", k + 1)).color(if on { pal::accent() } else { pal::dim() })).width(Length::Fixed(CW)).center_x(Length::Fixed(CW)),
            container(t::caption(format!("{name}（{id}）"))).padding(space(2)).style(crate::style::tooltip),
            iced::widget::tooltip::Position::Top,
        );
        head = head.push(tip);
    }
    let mut grid = column![head].spacing(1);
    for (lk, llabel) in LAYERS {
        let comps: Vec<_> = c.components.iter().filter(|x| x.layer == lk).collect();
        if comps.is_empty() {
            continue;
        }
        grid = grid.push(w::section(llabel));
        for comp in comps {
            let sel = v.component.as_deref() == Some(comp.key.as_str());
            let name = button(row![t::body(comp.label.clone()), t::metadata(format!("({})", comp.used_by.len())).color(pal::dim())].spacing(space(1)))
                .on_press(LyMsg::Component(if sel { None } else { Some(comp.key.clone()) }))
                .width(Length::Fixed(NW))
                .style(move |th, st| w::button_style(if sel { Kind::Standard } else { Kind::Ghost }, th, st));
            let mut r = row![name].spacing(1).align_y(Alignment::Center);
            for (id, _) in &strategies {
                let used = comp.used_by.iter().any(|u| u == *id);
                let on = sel_strategy.as_deref() == Some(id.as_str());
                let bg: Option<Color> = if used { Some(pal::alpha(pal::accent(), if sel || on { 0.75 } else { 0.4 })) } else if on { Some(pal::alpha(pal::accent(), 0.08)) } else { None };
                r = r.push(
                    container(t::metadata(if used { "●" } else { "" }))
                        .width(Length::Fixed(CW))
                        .center_x(Length::Fixed(CW))
                        .style(move |_| iced::widget::container::Style { background: bg.map(iced::Background::Color), ..Default::default() }),
                );
            }
            grid = grid.push(r);
        }
    }
    let mut col = column![
        t::metadata(format!("{} 个共用件 × {} 个策略（策略在 STRATEGY[\"master\"][\"layers\"] 里显式声明）。列号悬停看策略名；高亮列 = 策略中心选中的策略。", c.components.len(), strategies.len())).color(pal::dim()),
        scrollable(grid).direction(iced::widget::scrollable::Direction::Both { vertical: Default::default(), horizontal: Default::default() }).height(Length::FillPortion(3)),
    ]
    .spacing(space(2));
    if let Some(k) = &v.component
        && let Some(comp) = c.components.iter().find(|x| &x.key == k)
    {
        let users: Vec<String> = comp.used_by.iter().map(|u| c.strategies.get(u).cloned().unwrap_or_else(|| u.clone())).collect();
        col = col.push(
            column![
                w::section(format!("{} · {}", comp.label, LAYERS.iter().find(|(x, _)| *x == comp.layer).map(|(_, l)| *l).unwrap_or(""))),
                t::caption(comp.description.clone()),
                t::metadata(format!("实现：{}", comp.implementation)).color(pal::dim()),
                t::metadata(format!("用到它的策略（{}）：{}", users.len(), users.join("、"))),
            ]
            .spacing(space(1)),
        );
    }
    col.height(Length::Fill).into()
}
