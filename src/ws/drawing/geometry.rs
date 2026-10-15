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
    /// 看得见的 K 线：(开始时间, [开, 高, 低, 收])。磁吸用；热力图没有
    pub bars: Vec<(u64, [i64; 4])>,
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

/// 一个对象在屏幕上的形状（画与命中测试共用）。
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Segment(Point, Point),
    Rect(Rectangle),
}

/// 对象 → 屏幕形状。水平 / 垂直线横贯整张图，射线延到图外。
#[must_use]
pub fn shape(v: &DrawView, d: &Drawing) -> Option<Shape> {
    let pts: Vec<Point> = d.anchors.iter().map(|a| v.to_screen(*a)).collect();
    let (w, h) = (v.size.width, v.size.height);
    Some(match d.kind {
        Kind::HLine => {
            let y = pts.first()?.y;
            Shape::Segment(Point::new(0.0, y), Point::new(w, y))
        }
        Kind::VLine => {
            let x = pts.first()?.x;
            Shape::Segment(Point::new(x, 0.0), Point::new(x, h))
        }
        Kind::Trend => Shape::Segment(*pts.first()?, *pts.get(1)?),
        Kind::Ray => {
            let (a, b) = (*pts.first()?, *pts.get(1)?);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = dx.hypot(dy);
            if len < f32::EPSILON {
                Shape::Segment(a, b)
            } else {
                let far = (w + h) * 4.0 / len; // 足够远：画布自己会裁掉
                Shape::Segment(a, Point::new(a.x + dx * far, a.y + dy * far))
            }
        }
        Kind::Rect => {
            let (a, b) = (*pts.first()?, *pts.get(1)?);
            Shape::Rect(Rectangle::new(Point::new(a.x.min(b.x), a.y.min(b.y)), Size::new((a.x - b.x).abs(), (a.y - b.y).abs())))
        }
    })
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
    let near = match shape(v, d)? {
        Shape::Segment(a, b) => dist_to_segment(p, a, b) <= HIT_PX,
        // 矩形：里面或者边框附近都算
        Shape::Rect(r) => {
            let grown = Rectangle::new(Point::new(r.x - HIT_PX, r.y - HIT_PX), Size::new(r.width + 2.0 * HIT_PX, r.height + 2.0 * HIT_PX));
            grown.contains(p)
        }
    };
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
            bars: Vec::new(),
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
        assert_eq!(shape(&v, &h), Some(Shape::Segment(Point::new(0.0, 200.0), Point::new(800.0, 200.0))));
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
}
