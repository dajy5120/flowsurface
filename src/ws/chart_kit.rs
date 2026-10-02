//! 自绘图表共用的图表规范件（docs/35 §6.3 / §16.15 第 9 项）：缺口标记、十字线读数。
//!
//! 上游图表（K 线、热图、足迹…）的规范在 `src/chart/` 里做；本项目自绘的图（Tardis 历史面板、
//! 回测结果、自有数据图）各自画坐标轴，但缺口与十字线的**规则只有一份**，都调这里：
//!
//! - **缺口不插值**：时间轴上相邻两点间隔同时超过中位间隔 10 倍与 60 秒，断开不连线，画淡色带并写「缺口 时长」。
//!   两个门槛缺一不可：逐笔聚合的秒级 K 线在安静时段本来就没有点（那是没成交，不是数据洞），
//!   只用倍数会满屏都是缺口（2026-10-02 Tardis 面板 SPY 2s K 线实测）。
//! - **十字线读数**：光标处画竖线 + 横线，横轴、纵轴各给一个读数（等宽数字），
//!   另可附几行（各序列在最近一点的值）。读数不进几何缓存，跟着光标每帧画。

use iced::widget::canvas::{Frame, Path, Stroke, Text};
use iced::{Point, Size};

/// 点太少时「正常间隔」本身不可靠（4 个点的资金曲线曾把最后一段误判成缺口）。
const MIN_POINTS: usize = 10;

/// 缺口的最短时长（毫秒）。
const MIN_GAP_MS: f64 = 60_000.0;

/// 缺口位置：返回缺口**后**那一点的下标。`x` 是毫秒时间戳、按时间升序。
pub fn gaps(x: &[f64]) -> Vec<usize> {
    if x.len() < MIN_POINTS {
        return Vec::new();
    }
    let mut d: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).filter(|v| v.is_finite() && *v > 0.0).collect();
    if d.is_empty() {
        return Vec::new();
    }
    d.sort_by(f64::total_cmp);
    let limit = (d[d.len() / 2] * 10.0).max(MIN_GAP_MS);
    x.windows(2)
        .enumerate()
        .filter(|(_, w)| (w[1] - w[0]).is_finite() && w[1] - w[0] > limit)
        .map(|(i, _)| i + 1)
        .collect()
}

/// 绘图区：左上角、宽高（像素）与两轴的数据范围。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plot {
    pub x0: f32,
    pub y0: f32,
    pub w: f32,
    pub h: f32,
    pub xr: (f64, f64),
    pub yr: (f64, f64),
}

impl Plot {
    pub fn sx(&self, v: f64) -> f32 {
        self.x0 + ((v - self.xr.0) / (self.xr.1 - self.xr.0).max(f64::MIN_POSITIVE)) as f32 * self.w
    }

    /// 像素 → 横轴数据值。
    pub fn vx(&self, px: f32) -> f64 {
        self.xr.0 + f64::from((px - self.x0) / self.w.max(1.0)) * (self.xr.1 - self.xr.0)
    }

    /// 像素 → 纵轴数据值。
    pub fn vy(&self, py: f32) -> f64 {
        self.yr.1 - f64::from((py - self.y0) / self.h.max(1.0)) * (self.yr.1 - self.yr.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x0 + self.w && p.y >= self.y0 && p.y <= self.y0 + self.h
    }
}

/// 画缺口色带（`idx` 来自 [`gaps`]）。
pub fn gap_bands(frame: &mut Frame, plot: &Plot, x: &[f64], idx: &[usize]) {
    for &g in idx {
        let (Some(a), Some(b)) = (x.get(g.wrapping_sub(1)), x.get(g)) else { continue };
        let (x0, x1) = (plot.sx(*a), plot.sx(*b));
        frame.fill_rectangle(
            Point::new(x0, plot.y0),
            Size::new((x1 - x0).max(2.0), plot.h),
            crate::ui::pal::alpha(crate::ui::pal::warn(), 0.12),
        );
        let secs = ((b - a) / 1000.0).round() as i64;
        let dur = if secs >= 86_400 {
            format!("{} 天", secs / 86_400)
        } else if secs >= 3600 {
            format!("{} 时", secs / 3600)
        } else {
            format!("{} 分", secs / 60)
        };
        frame.fill_text(Text {
            content: format!("缺口 {dur}"),
            position: Point::new(x0 + 2.0, plot.y0 + 1.0),
            color: crate::ui::pal::warn(),
            size: iced::Pixels(9.0),
            ..Default::default()
        });
    }
}

/// 最近一点的下标（`x` 升序）。
pub fn nearest(x: &[f64], v: f64) -> Option<usize> {
    if x.is_empty() {
        return None;
    }
    let i = x.partition_point(|a| *a < v);
    match (i.checked_sub(1), x.get(i)) {
        (Some(j), Some(b)) => Some(if (v - x[j]).abs() <= (b - v).abs() { j } else { i }),
        (Some(j), None) => Some(j),
        (None, _) => Some(0),
    }
}

/// 读数框：几行等宽文字，贴在 `near` 旁边、不出 `area`（树图悬停、十字线附加读数共用）。
pub fn readout_box(frame: &mut Frame, lines: &[String], near: Point, area: iced::Rectangle) {
    if lines.is_empty() {
        return;
    }
    let wmax = lines.iter().map(|s| s.chars().map(|c| if c.is_ascii() { 1.0 } else { 1.8 }).sum::<f32>()).fold(0.0, f32::max) * 6.0 + 8.0;
    let hh = 13.0 * lines.len() as f32 + 2.0;
    let bx = if near.x + 12.0 + wmax > area.x + area.width { near.x - 12.0 - wmax } else { near.x + 12.0 };
    let by = (near.y + 12.0).min(area.y + area.height - hh).max(area.y);
    frame.fill_rectangle(Point::new(bx, by), Size::new(wmax, hh), crate::ui::pal::alpha(crate::ui::pal::card_bg(), 0.94));
    for (k, s) in lines.iter().enumerate() {
        frame.fill_text(Text {
            content: s.clone(),
            position: Point::new(bx + 4.0, by + 1.0 + 13.0 * k as f32),
            color: crate::ui::pal::txt(),
            size: iced::Pixels(10.0),
            font: crate::style::AZERET_MONO,
            ..Default::default()
        });
    }
}

/// 十字线：光标在绘图区内时画竖线 + 横线、两轴读数，以及附加的读数行（各序列的值）。
pub fn crosshair(
    frame: &mut Frame,
    plot: &Plot,
    p: Point,
    xfmt: impl Fn(f64) -> String,
    yfmt: impl Fn(f64) -> String,
    extra: &[String],
) {
    if !plot.contains(p) {
        return;
    }
    let line = Stroke::default().with_width(1.0).with_color(crate::ui::pal::alpha(crate::ui::pal::axis(), 0.8));
    frame.stroke(&Path::line(Point::new(p.x, plot.y0), Point::new(p.x, plot.y0 + plot.h)), line);
    frame.stroke(&Path::line(Point::new(plot.x0, p.y), Point::new(plot.x0 + plot.w, p.y)), line);
    let tag = |frame: &mut Frame, s: String, at: Point| {
        let w = s.chars().count() as f32 * 6.0 + 6.0;
        frame.fill_rectangle(at, Size::new(w, 13.0), crate::ui::pal::alpha(crate::ui::pal::card_bg(), 0.92));
        frame.fill_text(Text {
            content: s,
            position: Point::new(at.x + 3.0, at.y + 1.0),
            color: crate::ui::pal::txt(),
            size: iced::Pixels(10.0),
            font: crate::style::AZERET_MONO,
            ..Default::default()
        });
    };
    // 横轴读数贴在绘图区底边，纵轴读数贴在左边
    let xs = xfmt(plot.vx(p.x));
    let xw = xs.chars().count() as f32 * 6.0 + 6.0;
    tag(frame, xs, Point::new((p.x - xw / 2.0).clamp(plot.x0, plot.x0 + plot.w - xw), plot.y0 + plot.h - 13.0));
    tag(frame, yfmt(plot.vy(p.y)), Point::new(plot.x0, (p.y - 6.5).clamp(plot.y0, plot.y0 + plot.h - 13.0)));
    // 附加读数：靠光标、避开右边界
    if !extra.is_empty() {
        let wmax = extra.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f32 * 6.0 + 8.0;
        let bx = if p.x + 12.0 + wmax > plot.x0 + plot.w { p.x - 12.0 - wmax } else { p.x + 12.0 };
        let by = (p.y + 12.0).min(plot.y0 + plot.h - 13.0 * extra.len() as f32 - 14.0).max(plot.y0);
        for (k, s) in extra.iter().enumerate() {
            tag(frame, s.clone(), Point::new(bx, by + 13.0 * k as f32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Plot, gaps, nearest};

    #[test]
    fn 缺口与最近点() {
        // 1 分钟一根，中间断了 30 分钟
        let t: Vec<f64> = [0., 1., 2., 3., 4., 5., 35., 36., 37., 38., 39.].iter().map(|m| m * 60_000.0).collect();
        assert_eq!(gaps(&t), vec![6]);
        // 2 秒 K 线安静了 40 秒：没成交，不是缺口（不到 60 秒）
        let q: Vec<f64> = [0., 2., 4., 6., 8., 10., 50., 52., 54., 56., 58.].iter().map(|s| s * 1000.0).collect();
        assert!(gaps(&q).is_empty());
        assert!(gaps(&(0..20).map(|m| f64::from(m) * 60_000.0).collect::<Vec<_>>()).is_empty());
        assert!(gaps(&[0.0, 60_000.0, 900_000.0]).is_empty(), "点太少不判");
        assert_eq!(nearest(&[0.0, 10.0, 20.0], 14.0), Some(1));
        assert_eq!(nearest(&[0.0, 10.0, 20.0], 16.0), Some(2));
        assert_eq!(nearest(&[0.0, 10.0, 20.0], 99.0), Some(2));
        assert_eq!(nearest(&[], 1.0), None);
    }

    #[test]
    fn 像素与数据值互换() {
        let p = Plot { x0: 10.0, y0: 0.0, w: 100.0, h: 50.0, xr: (0.0, 1000.0), yr: (0.0, 5.0) };
        assert!((p.vx(60.0) - 500.0).abs() < 1e-6);
        assert!((p.vy(0.0) - 5.0).abs() < 1e-6);
        assert!((p.sx(500.0) - 60.0).abs() < 1e-4);
    }
}
