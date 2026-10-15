//! 坐标换算与命中测试（docs/46 §3.2）。纯函数，不碰界面状态，可单测。
//!
//! 屏幕 = 中心 + 缩放 ×（世界 + 平移）——与图表绘制时
//! `frame.translate(center); frame.scale(scaling); frame.translate(translation)` 同一个变换。
//! 世界坐标与 `chart::ViewState::interval_to_x` / `price_to_y` 同一套公式；按逐笔的图横轴是根序号，
//! 所以用各根 K 线的开始时间把锚点时间换成（带小数的）序号。

use iced::{Point, Rectangle, Size, Vector};

use super::model::{Anchor, Drawing, Kind};

/// 横轴怎么算。
#[derive(Debug, Clone, PartialEq)]
pub enum XMode {
    /// 按时间：最新一根 K 线的开始时刻、周期毫秒、格宽。
    Time { latest: u64, interval_ms: u64, cell_w: f32 },
    /// 按逐笔：各根 K 线的开始时间（按时间先后，至少一根）、格宽。
    Tick { times: Vec<u64>, cell_w: f32 },
}

/// 一张图此刻的视图：把（时间, 价格）换成屏幕坐标，反过来也行。
#[derive(Debug, Clone, PartialEq)]
pub struct DrawView {
    pub size: Size,
    pub scaling: f32,
    pub translation: Vector,
    pub x: XMode,
    /// 价格基准（定点 units）、每格的价位（units，合并档位之后的）、格高
    pub base: i64,
    pub tick: i64,
    pub cell_h: f32,
    /// 放锚点时价格取整到的最小变动价位（units）：合约的最小价位，不是合并之后的格子
    pub snap: i64,
    /// 这张图的周期类别（`model::tf_cat`）：对象按它决定显不显示
    pub cat: &'static str,
    /// 这张图的周期（`1m`、`15m`、`50T`…；G3）：新对象记成来源周期；到别的周期看时标出来
    pub tf: String,
    /// 看得见的 K 线：(开始时间, [开, 高, 低, 收])。磁吸用；热力图没有
    pub bars: Vec<(u64, [i64; 4])>,
    /// 价格的显示小数位（图上的标签）
    pub decimals: usize,
    /// 合约乘数（仓位工具的估算；现货 / U 本位按 1）
    pub multiplier: f64,
    /// 成交量分布用的 K 线（`model::VolBar`）。只在这个品种有成交量分布对象时才带（G4）
    pub vol: Vec<super::model::VolBar>,
}

/// 命中了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// 第 i 个锚点（只对选中的对象）
    Anchor(usize),
    /// 对象本身
    Body,
}

/// 命中测试的容差（像素）。
pub const HIT_PX: f32 = 6.0;
/// 锚点手柄的半径（像素）。
pub const HANDLE_PX: f32 = 4.5;

impl DrawView {
    fn center(&self) -> Vector {
        Vector::new(self.size.width / 2.0, self.size.height / 2.0)
    }

    /// 时间 → 世界横坐标。
    #[must_use]
    pub fn world_x(&self, t: u64) -> f32 {
        match &self.x {
            XMode::Time { latest, interval_ms, cell_w } => {
                ((t as f64 - *latest as f64) / (*interval_ms).max(1) as f64 * f64::from(*cell_w)) as f32
            }
            XMode::Tick { times, cell_w } => -(tick_index(times, t) as f32) * cell_w,
        }
    }

    /// 世界横坐标 → 时间，吸到 K 线开始时刻。
    #[must_use]
    pub fn time_at(&self, wx: f32) -> u64 {
        match &self.x {
            XMode::Time { latest, interval_ms, cell_w } => {
                let bars = (wx / cell_w.max(f32::EPSILON)).round() as i64;
                (*latest as i64 + bars * *interval_ms as i64).max(0) as u64
            }
            XMode::Tick { times, cell_w } => {
                let idx = (-wx / cell_w.max(f32::EPSILON)).round() as i64;
                tick_time(times, idx)
            }
        }
    }

    /// 价格 → 世界纵坐标。
    #[must_use]
    pub fn world_y(&self, p: i64) -> f32 {
        (self.base - p) as f32 / self.tick.max(1) as f32 * self.cell_h
    }

    /// 世界纵坐标 → 价格，取整到最小变动价位。
    #[must_use]
    pub fn price_at(&self, wy: f32) -> i64 {
        let tick = self.tick.max(1);
        let snap = self.snap.max(1);
        let raw = self.base as f64 - f64::from(wy) / f64::from(self.cell_h.max(f32::EPSILON)) * tick as f64;
        (raw / snap as f64).round() as i64 * snap
    }

    /// 锚点 → 屏幕点。
    #[must_use]
    pub fn to_screen(&self, a: Anchor) -> Point {
        let c = self.center();
        Point::new(
            c.x + self.scaling * (self.world_x(a.t) + self.translation.x),
            c.y + self.scaling * (self.world_y(a.p) + self.translation.y),
        )
    }

    /// 屏幕点 → 锚点（时间吸到 K 线开始、价格取整）。
    #[must_use]
    pub fn anchor_at(&self, pt: Point) -> Anchor {
        let c = self.center();
        let s = self.scaling.max(f32::EPSILON);
        let wx = (pt.x - c.x) / s - self.translation.x;
        let wy = (pt.y - c.y) / s - self.translation.y;
        Anchor { t: self.time_at(wx), p: self.price_at(wy) }
    }

    /// 磁吸（G1）：锚点的时间落在某根 K 线上时，价格换成那根的开 / 高 / 低 / 收里离光标最近的一个。
    #[must_use]
    pub fn magnet(&self, a: Anchor, cursor: Point) -> Anchor {
        let Some((_, ohlc)) = self.bars.iter().find(|(t, _)| *t == a.t) else { return a };
        let best = ohlc
            .iter()
            .min_by(|x, y| {
                let dx = (self.to_screen(Anchor { t: a.t, p: **x }).y - cursor.y).abs();
                let dy = (self.to_screen(Anchor { t: a.t, p: **y }).y - cursor.y).abs();
                dx.total_cmp(&dy)
            })
            .copied()
            .unwrap_or(a.p);
        Anchor { t: a.t, p: best }
    }

    /// 一个锚点挪动屏幕上的 `delta` 之后（再吸附）。拖整体时每个锚点各挪一份。
    #[must_use]
    pub fn shift(&self, a: Anchor, delta: Vector) -> Anchor {
        self.anchor_at(self.to_screen(a) + delta)
    }
}

/// 按逐笔：时间 → 从最新一根往回数的（带小数）序号。最新一根之后按最后一根的时长外推，最早一根之前按第一根的。
fn tick_index(times: &[u64], t: u64) -> f64 {
    let n = times.len();
    if n == 0 {
        return 0.0;
    }
    let dur = |k: usize| -> f64 {
        if n < 2 {
            return 1.0;
        }
        let k = k.min(n - 2);
        (times[k + 1].saturating_sub(times[k])).max(1) as f64
    };
    let last = n - 1;
    let k = times.partition_point(|&x| x <= t);
    if k == 0 {
        // 比最早一根还早
        return last as f64 + (times[0] - t) as f64 / dur(0);
    }
    let k = k - 1;
    if k == last {
        return -((t - times[last]) as f64 / dur(last.saturating_sub(1)));
    }
    let frac = (t - times[k]) as f64 / (times[k + 1] - times[k]).max(1) as f64;
    (last - k) as f64 - frac
}

/// 按逐笔：序号（0 = 最新一根，负 = 未来）→ 那根的开始时间。
fn tick_time(times: &[u64], idx: i64) -> u64 {
    let n = times.len() as i64;
    if n == 0 {
        return 0;
    }
    let last = *times.last().unwrap_or(&0);
    let step = |a: u64, b: u64| b.saturating_sub(a).max(1) as i64;
    if idx < 0 {
        let d = if n >= 2 { step(times[n as usize - 2], last) } else { 1 };
        return (last as i64 - idx * d) as u64;
    }
    if idx >= n {
        let d = if n >= 2 { step(times[0], times[1]) } else { 1 };
        return (times[0] as i64 - (idx - (n - 1)) * d).max(0) as u64;
    }
    times[(n - 1 - idx) as usize]
}

/// 图元在画的时候扮演什么（按它取颜色与线型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 主线：按对象的样式
    Main,
    /// 辅助线：细虚线（斐波那契的对角线、通道中线、标注的引线）
    Aux,
    /// 填充：线色的淡色
    Fill,
    /// 盈利区 / 亏损区（仓位工具）：涨色 / 跌色的淡填充
    Up,
    Down,
    /// 实心：对象的颜色（标记的三角）
    Solid,
    /// 很淡的填充（成交量分布价值区以外的档）
    Faint,
}

/// 文字相对位置的对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// 图元：一个对象在屏幕上由一组图元组成。**画与命中测试都只认图元**——多锚点工具共用一套编辑逻辑
/// （拖锚点、拖整体、命中），新工具只要写「锚点 → 图元」。
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    Line(Point, Point, Role),
    Area(Rectangle, Role),
    /// 矩形边框（主线样式）
    Frame(Rectangle),
    Poly(Vec<Point>, Role),
    /// 文字：位置（`align` 相对它对齐，竖直居中）、内容、有没有底框
    Label { at: Point, text: String, align: Align, boxed: bool },
}

/// 文字的估计宽度（像素）：ASCII 约 6.5，其余（中文）约 11.5，外加 8 像素边距。命中测试与底框用。
#[must_use]
pub fn label_rect(at: Point, text: &str, align: Align) -> Rectangle {
    let w: f32 = text.chars().map(|c| if c.is_ascii() { 6.5 } else { 11.5 }).sum::<f32>() + 8.0;
    let x = match align {
        Align::Left => at.x,
        Align::Center => at.x - w / 2.0,
        Align::Right => at.x - w,
    };
    Rectangle::new(Point::new(x, at.y - 9.0), Size::new(w, 18.0))
}

fn rect_of(a: Point, b: Point) -> Rectangle {
    Rectangle::new(Point::new(a.x.min(b.x), a.y.min(b.y)), Size::new((a.x - b.x).abs(), (a.y - b.y).abs()))
}

/// 时长（毫秒）的说法：3天4小时、2小时5分钟、12分钟、45秒。
fn duration_text(ms: u64) -> String {
    let s = ms / 1000;
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}秒"),
        (0, 0, m) => format!("{m}分钟"),
        (0, h, 0) => format!("{h}小时"),
        (0, h, m) => format!("{h}小时{m}分钟"),
        (d, 0, _) => format!("{d}天"),
        (d, h, _) => format!("{d}天{h}小时"),
    }
}

/// 金额的小数位：不少于 2 位，也不少于价格的小数位。
fn self_digits(v: &DrawView) -> usize {
    v.decimals.max(2)
}

fn signed(x: f64, digits: usize) -> String {
    if x >= 0.0 { format!("+{x:.digits$}") } else { format!("{x:.digits$}") }
}

impl DrawView {
    /// 价格（定点）的文字。
    fn ptext(&self, p: i64) -> String {
        super::model::price_text(p, self.decimals)
    }

    /// 从 `t` 往后数 `n` 根 K 线的时刻（仓位工具点一下时定右边界）。
    #[must_use]
    pub fn bars_later(&self, t: u64, n: f32) -> u64 {
        let cell = match &self.x {
            XMode::Time { cell_w, .. } | XMode::Tick { cell_w, .. } => *cell_w,
        };
        self.time_at(self.world_x(t) + n * cell)
    }

    /// 两个时刻之间隔了几根 K 线。
    fn bars_between(&self, a: u64, b: u64) -> f32 {
        let cell = match &self.x {
            XMode::Time { cell_w, .. } | XMode::Tick { cell_w, .. } => *cell_w,
        };
        ((self.world_x(b) - self.world_x(a)) / cell.max(f32::EPSILON)).abs()
    }
}

/// 对象 → 屏幕图元。水平 / 垂直线横贯整张图，射线延到图外。
#[must_use]
pub fn prims(v: &DrawView, d: &Drawing) -> Vec<Prim> {
    use Prim::{Area, Frame, Label, Line, Poly};
    let pts: Vec<Point> = d.anchors.iter().map(|a| v.to_screen(*a)).collect();
    let (w, h) = (v.size.width, v.size.height);
    let (Some(&a), b, c) = (pts.first(), pts.get(1).copied(), pts.get(2).copied()) else { return Vec::new() };
    let text = || d.text.clone().unwrap_or_default();
    match (d.kind, b, c) {
        (Kind::HLine, ..) => {
            let mut out = vec![Line(Point::new(0.0, a.y), Point::new(w, a.y), Role::Main)];
            if let Some(t) = d.text.clone().filter(|t| !t.is_empty()) {
                out.push(Label { at: Point::new(4.0, a.y - 10.0), text: t, align: Align::Left, boxed: false });
            }
            out
        }
        (Kind::VLine, ..) => {
            let mut out = vec![Line(Point::new(a.x, 0.0), Point::new(a.x, h), Role::Main)];
            if let Some(t) = d.text.clone().filter(|t| !t.is_empty()) {
                out.push(Label { at: Point::new(a.x + 3.0, 12.0), text: t, align: Align::Left, boxed: false });
            }
            out
        }
        (Kind::Marker, ..) => {
            // ▲ 画在价位下方、尖朝上；▼ 在上方；● 就在价位上
            let (hw, ht, gap) = (4.5, 8.0, 3.0);
            let dir = d.dir.unwrap_or(0);
            let (shape, label_y) = match dir.signum() {
                1 => (vec![Point::new(a.x, a.y + gap), Point::new(a.x - hw, a.y + gap + ht), Point::new(a.x + hw, a.y + gap + ht)], a.y + gap + ht + 8.0),
                -1 => (vec![Point::new(a.x, a.y - gap), Point::new(a.x - hw, a.y - gap - ht), Point::new(a.x + hw, a.y - gap - ht)], a.y - gap - ht - 8.0),
                _ => ((0..8).map(|i| {
                    let t = i as f32 * std::f32::consts::FRAC_PI_4;
                    Point::new(a.x + 3.5 * t.cos(), a.y + 3.5 * t.sin())
                }).collect(), a.y - 12.0),
            };
            let mut out = vec![Poly(shape, Role::Solid)];
            if let Some(t) = d.text.clone().filter(|t| !t.is_empty()) {
                out.push(Label { at: Point::new(a.x, label_y), text: t, align: Align::Center, boxed: false });
            }
            out
        }
        (Kind::VolProfile, Some(b), _) => {
            // 区间里的 K 线按价位分档（均摊）；横条从左边界往右，最长占区间宽度的 60%
            let (t0, t1) = (d.anchors[0].t.min(d.anchors[1].t), d.anchors[0].t.max(d.anchors[1].t));
            let bars: Vec<_> = v.vol.iter().filter(|x| x.0 >= t0 && x.0 <= t1).cloned().collect();
            let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
            let Some(p) = super::model::volume_profile(&bars, d.rows.unwrap_or(24), d.va.unwrap_or(0.7)) else {
                return vec![Frame(rect_of(a, b)), Label { at: Point::new(x0, a.y.min(b.y) - 10.0), text: "区间里没有成交量数据".into(), align: Align::Left, boxed: false }];
            };
            let max = p.bins.iter().map(|b| b.2).fold(0.0_f64, f64::max).max(f64::EPSILON);
            let width = ((x1 - x0) * 0.6).max(24.0);
            let y = |price: i64| v.to_screen(Anchor { t: t0, p: price }).y;
            let mut out = Vec::new();
            for (i, &(lo, hi, vol)) in p.bins.iter().enumerate() {
                let (ya, yb) = (y(hi), y(lo));
                let len = (vol / max) as f32 * width;
                let in_va = lo >= p.val && hi <= p.vah;
                out.push(Area(Rectangle::new(Point::new(x0, ya.min(yb) + 0.5), Size::new(len, ((yb - ya).abs() - 1.0).max(1.0))), if i == p.poc || in_va { Role::Fill } else { Role::Faint }));
            }
            let (poc_mid, top) = ((p.bins[p.poc].0 + p.bins[p.poc].1) / 2, y(p.bins.last().map_or(0, |b| b.1)));
            out.push(Line(Point::new(x0, y(poc_mid)), Point::new(x1, y(poc_mid)), Role::Main));
            out.push(Line(Point::new(x0, y(p.vah)), Point::new(x1, y(p.vah)), Role::Aux));
            out.push(Line(Point::new(x0, y(p.val)), Point::new(x1, y(p.val)), Role::Aux));
            out.push(Frame(Rectangle::new(Point::new(x0, top), Size::new(x1 - x0, (y(p.bins[0].0) - top).max(1.0)))));
            // 标签写在区间右边；右边放不下（区间贴着最新一根）就写在区间里面靠右
            let poc_t = format!("POC {}", v.ptext(poc_mid));
            let outside = x1 + 4.0 + label_rect(Point::ORIGIN, &poc_t, Align::Left).width <= w;
            let (lx, la) = if outside { (x1 + 4.0, Align::Left) } else { (x1 - 4.0, Align::Right) };
            out.push(Label { at: Point::new(lx, y(poc_mid)), text: poc_t, align: la, boxed: !outside });
            out.push(Label { at: Point::new(lx, y(p.vah)), text: format!("VAH {}", v.ptext(p.vah)), align: la, boxed: !outside });
            out.push(Label { at: Point::new(lx, y(p.val)), text: format!("VAL {}", v.ptext(p.val)), align: la, boxed: !outside });
            out
        }
        (Kind::Trend, Some(b), _) => vec![Line(a, b, Role::Main)],
        (Kind::Ray, Some(b), _) => {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = dx.hypot(dy);
            let far = if len < f32::EPSILON { b } else { Point::new(a.x + dx * (w + h) * 4.0 / len, a.y + dy * (w + h) * 4.0 / len) };
            vec![Line(a, far, Role::Main)]
        }
        (Kind::Rect, Some(b), _) => vec![Area(rect_of(a, b), Role::Fill), Frame(rect_of(a, b))],
        (Kind::Polyline, ..) => pts.windows(2).map(|p| Line(p[0], p[1], Role::Main)).collect(),
        (Kind::Channel, Some(b), Some(c)) => {
            // 第三个锚点到 a–b 直线的垂足 → 偏移量；另一条边是 a–b 平移这个偏移
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = (dx * dx + dy * dy).max(f32::EPSILON);
            let t = ((c.x - a.x) * dx + (c.y - a.y) * dy) / l2;
            let off = Vector::new(c.x - (a.x + t * dx), c.y - (a.y + t * dy));
            let half = Vector::new(off.x / 2.0, off.y / 2.0);
            vec![
                Poly(vec![a, b, b + off, a + off], Role::Fill),
                Line(a, b, Role::Main),
                Line(a + off, b + off, Role::Main),
                Line(a + half, b + half, Role::Aux),
            ]
        }
        (Kind::FibRetrace, Some(b), _) => {
            let (pa, pb) = (d.anchors[0].p, d.anchors[1].p);
            let x0 = a.x.min(b.x);
            let x1 = if d.extend { w } else { a.x.max(b.x) };
            let mut out = vec![Line(a, b, Role::Aux)];
            for r in d.fib_levels() {
                let p = pb + ((pa - pb) as f64 * r).round() as i64;
                let y = v.to_screen(Anchor { t: d.anchors[0].t, p }).y;
                out.push(Line(Point::new(x0, y), Point::new(x1, y), Role::Main));
                // 标签写在左边；左边放不下（锚点靠左或在视野外）就写在线上方、从图的左边缘起
                let text = format!("{r} ({})", v.ptext(p));
                if x0 - 4.0 - label_rect(Point::ORIGIN, &text, Align::Left).width >= 0.0 {
                    out.push(Label { at: Point::new(x0 - 4.0, y), text, align: Align::Right, boxed: false });
                } else {
                    out.push(Label { at: Point::new(x0.max(0.0) + 4.0, y - 8.0), text, align: Align::Left, boxed: false });
                }
            }
            out
        }
        (Kind::FibExt, Some(b), Some(c)) => {
            let diff = d.anchors[1].p - d.anchors[0].p;
            let (x0, x1) = (c.x, if d.extend { w } else { c.x + (b.x - a.x).abs().max(60.0) });
            let mut out = vec![Line(a, b, Role::Aux), Line(b, c, Role::Aux)];
            for r in d.fib_levels() {
                let p = d.anchors[2].p + (diff as f64 * r).round() as i64;
                let y = v.to_screen(Anchor { t: d.anchors[2].t, p }).y;
                out.push(Line(Point::new(x0, y), Point::new(x1, y), Role::Main));
                let (lx, la) = if d.extend { (x0 + 4.0, Align::Left) } else { (x1 + 4.0, Align::Left) };
                out.push(Label { at: Point::new(lx, y - if d.extend { 8.0 } else { 0.0 }), text: format!("{r} ({})", v.ptext(p)), align: la, boxed: false });
            }
            out
        }
        (Kind::Text, ..) => vec![Label { at: a, text: text(), align: Align::Left, boxed: false }],
        (Kind::Callout, Some(b), _) => vec![Line(a, b, Role::Aux), Label { at: b, text: text(), align: Align::Left, boxed: true }],
        (Kind::PriceRange, Some(b), _) => {
            let (pa, pb) = (d.anchors[0].p, d.anchors[1].p);
            let delta = pb - pa;
            let pct = if pa == 0 { 0.0 } else { delta as f64 / pa as f64 * 100.0 };
            let ticks = delta / v.snap.max(1);
            let mid = (a.x + b.x) / 2.0;
            let ly = if pb >= pa { b.y.min(a.y) - 12.0 } else { b.y.max(a.y) + 12.0 };
            vec![
                Area(rect_of(a, b), Role::Fill),
                Line(Point::new(mid, a.y), Point::new(mid, b.y), Role::Main),
                Label {
                    at: Point::new(mid, ly),
                    text: format!("{} ({}%) · {ticks} tick · {} bp", v.ptext(delta), signed(pct, 2), signed(pct * 100.0, 0)),
                    align: Align::Center,
                    boxed: true,
                },
            ]
        }
        (Kind::DateRange, Some(b), _) => {
            let (ta, tb) = (d.anchors[0].t, d.anchors[1].t);
            let mid = (a.y + b.y) / 2.0;
            vec![
                Area(rect_of(a, b), Role::Fill),
                Line(Point::new(a.x, mid), Point::new(b.x, mid), Role::Main),
                Label {
                    at: Point::new((a.x + b.x) / 2.0, a.y.max(b.y) + 12.0),
                    text: format!("{} 根 · {}", v.bars_between(ta, tb).round(), duration_text(ta.abs_diff(tb))),
                    align: Align::Center,
                    boxed: true,
                },
            ]
        }
        (Kind::LongPos | Kind::ShortPos, Some(t), Some(s)) => {
            let x1 = t.x.max(a.x + 4.0);
            let mut out = vec![
                Area(rect_of(Point::new(a.x, a.y), Point::new(x1, t.y)), Role::Up),
                Area(rect_of(Point::new(a.x, a.y), Point::new(x1, s.y)), Role::Down),
                Line(Point::new(a.x, a.y), Point::new(x1, a.y), Role::Main),
            ];
            if let Some(e) = super::model::estimate(d, v.multiplier) {
                let cx = (a.x + x1) / 2.0;
                let money = |x: f64| signed(x, self_digits(v)); // 低价品种一个只有几分钱：小数位不少于价格的
                out.push(Label { at: Point::new(cx, t.y), text: format!("目标 {} · {}% · 估 {}", v.ptext(d.anchors[1].p), signed(e.target_pct, 2), money(e.win)), align: Align::Center, boxed: true });
                out.push(Label { at: Point::new(cx, s.y), text: format!("止损 {} · {}% · 估 {}", v.ptext(d.anchors[2].p), signed(e.stop_pct, 2), money(e.loss)), align: Align::Center, boxed: true });
                out.push(Label { at: Point::new(cx, a.y), text: format!("风险收益 1 : {:.2} · 估算，不是委托", e.rr), align: Align::Center, boxed: true });
            }
            out
        }
        // 内置以外的：登记的工具自己画（G5）
        (k, ..) => super::tools::find(k).map(|t| t.prims(v, d, &pts)).unwrap_or_default(),
    }
}

/// 测试用的视图（登记工具的测试也用）：800×400、1 分钟、最新一根 600 000 毫秒、一格 10 价位 4 像素。
#[cfg(test)]
#[must_use]
pub fn tests_view() -> DrawView {
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

/// 对象在屏幕上的外接框（浮动小条按它放：放在整个对象上方，免得盖住对象自己的标签）。裁到视野里。
#[must_use]
pub fn bbox(v: &DrawView, d: &Drawing) -> Option<Rectangle> {
    let mut pts: Vec<Point> = Vec::new();
    for pr in prims(v, d) {
        match pr {
            Prim::Line(a, b, _) => pts.extend([a, b]),
            Prim::Area(r, _) | Prim::Frame(r) => pts.extend([r.position(), Point::new(r.x + r.width, r.y + r.height)]),
            Prim::Poly(p, _) => pts.extend(p),
            Prim::Label { at, text, align, .. } => {
                let r = label_rect(at, &text, align);
                pts.extend([r.position(), Point::new(r.x + r.width, r.y + r.height)]);
            }
        }
    }
    let clamp = |p: &Point| Point::new(p.x.clamp(0.0, v.size.width), p.y.clamp(0.0, v.size.height));
    let pts: Vec<Point> = pts.iter().map(clamp).collect();
    let (x0, y0) = pts.iter().fold((f32::MAX, f32::MAX), |(x, y), p| (x.min(p.x), y.min(p.y)));
    let (x1, y1) = pts.iter().fold((f32::MIN, f32::MIN), |(x, y), p| (x.max(p.x), y.max(p.y)));
    (x0 <= x1).then(|| Rectangle::new(Point::new(x0, y0), Size::new(x1 - x0, y1 - y0)))
}

/// 一排文字（日历竖线顶上的事件名）里挑出不相叠的：按给的顺序，和已经留下的叠了就不写（线照画）。
#[must_use]
pub fn keep_apart(rects: &[Rectangle]) -> Vec<bool> {
    let mut kept: Vec<Rectangle> = Vec::new();
    rects
        .iter()
        .map(|r| {
            let free = kept.iter().all(|k| k.intersection(r).is_none());
            if free {
                kept.push(*r);
            }
            free
        })
        .collect()
}

/// 点到线段的距离。
#[must_use]
pub fn dist_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    if l2 < f32::EPSILON {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0);
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}

fn in_poly(p: Point, poly: &[Point]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

fn grow(r: Rectangle, by: f32) -> Rectangle {
    Rectangle::new(Point::new(r.x - by, r.y - by), Size::new(r.width + 2.0 * by, r.height + 2.0 * by))
}

/// 光标点中了对象的哪里。`with_handles`：选中的对象才有锚点手柄。
#[must_use]
pub fn hit(v: &DrawView, d: &Drawing, p: Point, with_handles: bool) -> Option<Part> {
    if d.hidden {
        return None;
    }
    if with_handles {
        for (i, a) in d.anchors.iter().enumerate() {
            let s = v.to_screen(*a);
            if (s.x - p.x).hypot(s.y - p.y) <= HANDLE_PX + 3.0 {
                return Some(Part::Anchor(i));
            }
        }
    }
    let near = prims(v, d).iter().any(|pr| match pr {
        Prim::Line(a, b, _) => dist_to_segment(p, *a, *b) <= HIT_PX,
        // 矩形、区域：里面或者边框附近都算
        Prim::Area(r, _) | Prim::Frame(r) => grow(*r, HIT_PX).contains(p),
        Prim::Poly(pts, _) => in_poly(p, pts),
        Prim::Label { at, text, align, .. } => label_rect(*at, text, *align).contains(p),
    });
    near.then_some(Part::Body)
}

/// 一组对象里，光标点中的最上面那个（后画的在上面）。`selected`：选中的对象 id（它有手柄）。
#[must_use]
pub fn pick<'a>(v: &DrawView, items: &'a [Drawing], p: Point, selected: Option<&str>) -> Option<(&'a Drawing, Part)> {
    // 选中对象的手柄优先（它可能被别的对象压着）
    if let Some(d) = selected.and_then(|id| items.iter().find(|d| d.id == id))
        && let Some(Part::Anchor(i)) = hit(v, d, p, true)
    {
        return Some((d, Part::Anchor(i)));
    }
    items.iter().rev().find_map(|d| hit(v, d, p, false).map(|part| (d, part)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time_view() -> DrawView {
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

    #[test]
    fn time_axis_round_trips_and_snaps_to_bar_and_tick() {
        let v = time_view();
        let a = Anchor { t: 540_000, p: 9_950 };
        let s = v.to_screen(a);
        assert_eq!((s.x, s.y), (390.0, 220.0)); // 中心 (400, 200)；早一根 = −10 像素；低 5 格 = +20 像素
        assert_eq!(v.anchor_at(s), a);
        // 点在两根之间、两格之间：吸到最近的 K 线开始与价位
        assert_eq!(v.anchor_at(Point::new(393.0, 221.0)), a);
        // 缩放与平移之后照样对得上
        let v2 = DrawView { scaling: 2.0, translation: Vector::new(30.0, -12.0), ..time_view() };
        assert_eq!(v2.anchor_at(v2.to_screen(a)), a);
        assert_eq!(v2.shift(a, Vector::new(20.0, 0.0)), Anchor { t: 600_000, p: 9_950 }, "缩放 2 倍时 20 像素 = 一根");
        // 最小变动价位比格子细：取整到最小价位
        let fine = DrawView { snap: 1, ..time_view() };
        assert_eq!(fine.anchor_at(Point::new(393.0, 221.0)).p, 9_948);
    }

    #[test]
    fn magnet_snaps_to_the_nearest_of_open_high_low_close() {
        let v = DrawView { bars: vec![(540_000, [9_960, 10_000, 9_900, 9_930])], ..time_view() };
        // 光标在 y = 205（价位 9 987.5）附近：最近的是高 10 000（y = 200）
        let a = v.anchor_at(Point::new(390.0, 205.0));
        assert_eq!(v.magnet(a, Point::new(390.0, 205.0)), Anchor { t: 540_000, p: 10_000 });
        // 光标靠下：低 9 900（y = 240）
        assert_eq!(v.magnet(a, Point::new(390.0, 236.0)).p, 9_900);
        // 没有 K 线的时刻不动
        let b = Anchor { t: 600_000, p: 9_950 };
        assert_eq!(v.magnet(b, Point::new(400.0, 200.0)), b);
    }

    #[test]
    fn tick_axis_maps_time_through_bar_start_times() {
        let times = vec![1_000, 1_400, 2_000, 2_100]; // 4 根逐笔 K 线，各自的开始时间
        let v = DrawView { x: XMode::Tick { times: times.clone(), cell_w: 10.0 }, ..time_view() };
        // 最新一根（2100）在世界 x = 0，往回一根 −10
        assert_eq!((v.world_x(2_100), v.world_x(2_000), v.world_x(1_000)), (0.0, -10.0, -30.0));
        assert_eq!(v.world_x(1_700), -15.0, "两根之间按时间插值");
        assert_eq!(v.world_x(2_150), 5.0, "最新一根之后按最后一根的时长（100 ms）外推");
        assert_eq!(v.world_x(800), -35.0, "最早一根之前按第一根的时长（400 ms）外推");
        assert_eq!((v.time_at(-10.0), v.time_at(-14.0), v.time_at(10.0), v.time_at(-40.0)), (2_000, 2_000, 2_200, 600));
        // 锚点存时间：换成更细的逐笔（多了几根）之后，时间还是那个时间，位置跟着 K 线走
        let finer = DrawView { x: XMode::Tick { times: vec![1_000, 1_200, 1_400, 1_700, 2_000, 2_050, 2_100], cell_w: 10.0 }, ..time_view() };
        assert_eq!(finer.world_x(2_000), -20.0);
    }

    #[test]
    fn shapes_and_hit_testing() {
        let v = time_view();
        let a = Anchor { t: 540_000, p: 10_000 };
        let b = Anchor { t: 600_000, p: 9_950 };
        let h = Drawing::new(Kind::HLine, vec![a]);
        assert_eq!(prims(&v, &h), vec![Prim::Line(Point::new(0.0, 200.0), Point::new(800.0, 200.0), Role::Main)]);
        assert_eq!(hit(&v, &h, Point::new(10.0, 204.0), false), Some(Part::Body), "水平线横贯整张图");
        assert_eq!(hit(&v, &h, Point::new(10.0, 210.0), false), None);
        let t = Drawing::new(Kind::Trend, vec![a, b]);
        assert_eq!(hit(&v, &t, Point::new(395.0, 210.0), false), Some(Part::Body));
        assert_eq!(hit(&v, &t, Point::new(420.0, 240.0), false), None, "线段之外不算");
        assert_eq!(hit(&v, &t, Point::new(400.0, 219.0), true), Some(Part::Anchor(1)));
        let r = Drawing::new(Kind::Ray, vec![a, b]);
        assert_eq!(hit(&v, &r, Point::new(420.0, 260.0), false), Some(Part::Body), "射线延出去");
        assert_eq!(hit(&v, &t, Point::new(420.0, 260.0), false), None, "趋势线到终点为止");
        let rect = Drawing::new(Kind::Rect, vec![a, b]);
        assert_eq!(hit(&v, &rect, Point::new(395.0, 210.0), false), Some(Part::Body), "矩形里面算");
        // 后画的在上面；选中对象的手柄优先
        let items = vec![t.clone(), rect.clone()];
        assert_eq!(pick(&v, &items, Point::new(395.0, 210.0), None).map(|(d, _)| d.id.clone()), Some(rect.id.clone()));
        assert_eq!(pick(&v, &items, Point::new(400.0, 220.0), Some(&t.id)).map(|(d, p)| (d.id.clone(), p)), Some((t.id.clone(), Part::Anchor(1))));
        let mut hidden = t;
        hidden.hidden = true;
        assert_eq!(hit(&v, &hidden, Point::new(395.0, 210.0), true), None);
    }

    #[test]
    fn g2_tools_build_their_primitives_from_anchors() {
        let v = time_view();
        let a = Anchor { t: 540_000, p: 10_000 }; // 屏幕 (390, 200)
        let b = Anchor { t: 600_000, p: 10_000 }; // (400, 200)
        let c = Anchor { t: 600_000, p: 9_950 }; //  (400, 220)
        // 平行通道：另一条边过第三个锚点、与 a–b 平行；中线在一半；里面点得中
        let ch = Drawing::new(Kind::Channel, vec![a, b, c]);
        let ps = prims(&v, &ch);
        assert!(ps.contains(&Prim::Line(Point::new(390.0, 220.0), Point::new(400.0, 220.0), Role::Main)));
        assert!(ps.contains(&Prim::Line(Point::new(390.0, 210.0), Point::new(400.0, 210.0), Role::Aux)));
        assert_eq!(hit(&v, &ch, Point::new(395.0, 212.0), false), Some(Part::Body));
        // 斐波那契回撤：0 在第二个锚点、1 在第一个；0.5 在正中
        let fib = Drawing::new(Kind::FibRetrace, vec![a, Anchor { t: 600_000, p: 9_900 }]);
        let lines: Vec<f32> = prims(&v, &fib).iter().filter_map(|p| if let Prim::Line(x, _, Role::Main) = p { Some(x.y) } else { None }).collect();
        assert_eq!(lines, vec![240.0, 230.4, 224.8, 220.0, 215.2, 208.4, 200.0], "各档价位取整到定点单位（这里一个单位 = 0.4 像素）");
        // 延伸：从 C 起量 A→B 的幅度
        let ext = Drawing::new(Kind::FibExt, vec![a, Anchor { t: 560_000, p: 10_100 }, c]);
        let y1 = prims(&v, &ext).iter().filter_map(|p| if let Prim::Label { at, text, .. } = p { text.starts_with("1 ").then_some(at.y) } else { None }).next();
        assert_eq!(y1, Some(180.0), "1 倍：9950 + 100 = 10050");
        // 价格区间：价差、百分比、tick 数、基点
        let pr = Drawing::new(Kind::PriceRange, vec![a, Anchor { t: 600_000, p: 9_900 }]);
        let label = prims(&v, &pr).iter().find_map(|p| if let Prim::Label { text, .. } = p { Some(text.clone()) } else { None }).unwrap();
        assert!(label.contains("(-1.00%)") && label.contains("-10 tick") && label.contains("-100 bp"), "{label}");
        // 日期区间：根数与时长
        let dr = Drawing::new(Kind::DateRange, vec![a, Anchor { t: 780_000, p: 9_900 }]);
        let label = prims(&v, &dr).iter().find_map(|p| if let Prim::Label { text, .. } = p { Some(text.clone()) } else { None }).unwrap();
        assert_eq!(label, "4 根 · 4分钟");
        // 文字：点在字上算点中
        let tx = Drawing::new(Kind::Text, vec![a]);
        assert_eq!(hit(&v, &tx, Point::new(400.0, 202.0), false), Some(Part::Body));
        assert_eq!(hit(&v, &tx, Point::new(380.0, 202.0), false), None);
        // 仓位：盈利区、亏损区、三个标签都写着估算
        let pos = Drawing::new(Kind::LongPos, crate::ws::drawing::model::position_anchors(Kind::LongPos, a, 780_000, 10));
        let ps = prims(&v, &pos);
        assert!(ps.iter().any(|p| matches!(p, Prim::Area(_, Role::Up))) && ps.iter().any(|p| matches!(p, Prim::Area(_, Role::Down))));
        assert!(ps.iter().any(|p| matches!(p, Prim::Label { text, .. } if text.contains("估算，不是委托"))));
        assert_eq!(v.bars_later(540_000, 20.0), 1_740_000);
        // 斐波那契向右延伸：各档线画到图的右边缘
        let mut fx = fib.clone();
        fx.extend = true;
        assert!(prims(&v, &fx).iter().any(|p| matches!(p, Prim::Line(_, b, Role::Main) if b.x == 800.0)));
        // 外接框：仓位工具的框包住盈亏区与标签
        let bb = bbox(&v, &pos).unwrap();
        assert!(bb.y < 200.0 - 20.0 && bb.height > 40.0, "{bb:?}");
        // 顶上的文字挨太近：后一个不写
        let r = |x: f32| label_rect(Point::new(x, 12.0), "FOMC", Align::Left);
        assert_eq!(keep_apart(&[r(10.0), r(20.0), r(200.0)]), vec![true, false, true]);
    }
}
