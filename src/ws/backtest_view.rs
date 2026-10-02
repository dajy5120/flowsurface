//! 回测结果 pane 的视图渲染（docs/08 F6-P7）。
//!
//! 仿官方 HTML tearsheet 的版式：**上方** Run Information + Performance Statistics 两张表
//! （全字段、按报告顺序与分类、分节标题分层）；**下方** 图表按报告顺序排列
//! （收益曲线 → 回撤 → 月度热力图 → 收益分布 → 滚动夏普 → 年度收益），带横/纵轴刻度与网格。
//! 数据走 [`super::backtest_readout`] 旁路快照；只渲染、不发消息，对 pane 消息类型 `M` 泛型。

use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Background, Color, Element, Length, Point, Rectangle, Renderer, Theme, mouse};

use super::backtest_readout::BacktestResult;
use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};
// 溯源横幅：高可信（Tardis + 体检过 + 队列位置撮合）绿底，其余琥珀。

const ML: f32 = 46.0; // 左边距（y 轴标签）
const MB: f32 = 16.0; // 下边距（x 轴标签）
const MT: f32 = 14.0; // 上边距：留出纵轴单位那一行
const MR: f32 = 8.0;

fn fmt_num(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

fn fmt_date(ms: i64) -> String {
    use chrono::{DateTime, Utc};
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 画 y 轴网格 + 刻度值（lo..hi 4 等分），返回绘图区映射闭包用的边界。
fn draw_grid(frame: &mut Frame, w: f32, h: f32, lo: f64, hi: f64) {
    draw_grid_labels(frame, w, h, lo, hi, true);
}

/// `labels = false`：只画网格线不写刻度值（资金曲线在隐藏数值模式下，docs/35 §9.3）。
fn draw_grid_labels(frame: &mut Frame, w: f32, h: f32, lo: f64, hi: f64, labels: bool) {
    let pw = (w - ML - MR).max(1.0);
    for i in 0..=4 {
        let v = lo + (hi - lo) * (i as f64) / 4.0;
        let y = MT + ((hi - v) / (hi - lo)) as f32 * (h - MT - MB).max(1.0);
        frame.stroke(
            &Path::line(Point::new(ML, y), Point::new(ML + pw, y)),
            Stroke::default().with_width(1.0).with_color(crate::ui::pal::grid()),
        );
        if !labels {
            continue;
        }
        frame.fill_text(Text {
            content: fmt_num(v),
            position: Point::new(2.0, y - 5.0),
            color: crate::ui::pal::axis(),
            size: iced::Pixels(9.0),
            ..Default::default()
        });
    }
}

fn empty_note(frame: &mut Frame, h: f32) {
    frame.fill_text(Text {
        content: "数据不足".to_string(),
        position: Point::new(ML + 4.0, h / 2.0 - 6.0),
        color: crate::ui::pal::dim(),
        size: iced::Pixels(11.0),
        ..Default::default()
    });
}

// ───────────────────────── 折线图（收益/回撤/滚动夏普）─────────────────────────

struct LineChart {
    pts: Vec<f64>,
    xt: Vec<i64>, // 时间戳（ms），用于 x 轴日期刻度；空则按索引、无 x 标签
    color: Color,
    baseline: Option<f64>,
    fill: bool,
    /// 纵轴是金额（资金曲线）：隐藏数值模式下不写刻度值
    money: bool,
    /// 纵轴单位（图表规范 docs/35 §6.3：坐标轴写单位）
    unit: &'static str,
    cache: Cache,
}

/// 时间序列里的缺口（规则在 [`super::chart_kit::gaps`]，全部自绘图共用一份）。
fn gaps(xt: &[i64]) -> Vec<usize> {
    super::chart_kit::gaps(&xt.iter().map(|t| *t as f64).collect::<Vec<_>>())
}

fn fmt_time(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
}

impl<M> canvas::Program<M> for LineChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if self.pts.len() < 2 {
                empty_note(frame, h);
                return;
            }
            let mut lo = self.pts.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut hi = self.pts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            if let Some(bl) = self.baseline {
                lo = lo.min(bl);
                hi = hi.max(bl);
            }
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            draw_grid_labels(frame, w, h, lo, hi, !(self.money && crate::ui::hide_values()));
            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let n = self.pts.len();
            let mx = |i: usize| ML + (i as f32) / ((n - 1) as f32) * pw;
            let my = |v: f64| MT + ((hi - v) / (hi - lo)) as f32 * ph;

            if let Some(bl) = self.baseline {
                let by = my(bl);
                frame.stroke(
                    &Path::line(Point::new(ML, by), Point::new(ML + pw, by)),
                    Stroke::default().with_width(1.0).with_color(crate::ui::pal::axis()),
                );
            }
            if self.fill {
                let base_y = self.baseline.map(my).unwrap_or(MT + ph);
                let area = Path::new(|p| {
                    p.move_to(Point::new(mx(0), base_y));
                    for (i, v) in self.pts.iter().enumerate() {
                        p.line_to(Point::new(mx(i), my(*v)));
                    }
                    p.line_to(Point::new(mx(n - 1), base_y));
                    p.close();
                });
                frame.fill(&area, Color { a: 0.16, ..self.color });
            }
            // 缺口：断开不连线，并画淡色带 + 「缺口」字样
            let gap_at = if self.xt.len() == n { gaps(&self.xt) } else { Vec::new() };
            for &g in &gap_at {
                let (x0, x1) = (mx(g - 1), mx(g));
                frame.fill_rectangle(Point::new(x0, MT), iced::Size::new((x1 - x0).max(2.0), ph), crate::ui::pal::alpha(crate::ui::pal::warn(), 0.12));
                frame.fill_text(Text {
                    content: "缺口".into(),
                    position: Point::new(x0 + 2.0, MT + 1.0),
                    color: crate::ui::pal::warn(),
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
            let line = Path::new(|p| {
                p.move_to(Point::new(mx(0), my(self.pts[0])));
                for (i, v) in self.pts.iter().enumerate().skip(1) {
                    if gap_at.contains(&i) {
                        p.move_to(Point::new(mx(i), my(*v)));
                    } else {
                        p.line_to(Point::new(mx(i), my(*v)));
                    }
                }
            });
            frame.stroke(&line, Stroke::default().with_width(1.6).with_color(self.color));
            // 纵轴单位写在左上角刻度上方
            if !self.unit.is_empty() {
                frame.fill_text(Text {
                    content: self.unit.to_string(),
                    position: Point::new(2.0, 0.0),
                    color: crate::ui::pal::axis(),
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }

            // x 轴日期刻度（4 个）
            if self.xt.len() == n {
                for k in 0..4 {
                    let i = k * (n - 1) / 3;
                    frame.fill_text(Text {
                        content: fmt_date(self.xt[i]),
                        position: Point::new((mx(i) - 22.0).max(0.0), h - MB + 2.0),
                        color: crate::ui::pal::axis(),
                        size: iced::Pixels(9.0),
                        ..Default::default()
                    });
                }
            }
        });
        // 十字线读数（图表规范 §6.3）：最近的一点的时间与值，等宽数字；不进缓存，跟着光标每帧画
        let mut out = vec![geo];
        if let Some(p) = cursor.position_in(b)
            && self.pts.len() >= 2
        {
            let mut hair = Frame::new(r, b.size());
            let (w, h) = (b.width, b.height);
            let pw = (w - ML - MR).max(1.0);
            let n = self.pts.len();
            let i = (((p.x - ML) / pw) * (n - 1) as f32).round().clamp(0.0, (n - 1) as f32) as usize;
            let x = ML + (i as f32) / ((n - 1) as f32) * pw;
            hair.stroke(
                &Path::line(Point::new(x, MT), Point::new(x, h - MB)),
                Stroke::default().with_width(1.0).with_color(crate::ui::pal::axis()),
            );
            let v = self.pts[i];
            let val = if self.money && crate::ui::hide_values() {
                crate::ui::fmt::Absence::Withheld.glyph().to_string()
            } else if self.money {
                crate::ui::fmt::sim(fmt_num(v))
            } else {
                fmt_num(v)
            };
            let when = self.xt.get(i).map(|t| fmt_time(*t)).unwrap_or_else(|| format!("第 {i} 点"));
            let gap = if self.xt.len() == n && gaps(&self.xt).contains(&i) { " · 缺口后第一点" } else { "" };
            let label = format!("{when}  {val}{}{gap}", if self.unit.is_empty() { String::new() } else { format!(" {}", self.unit) });
            let lx = if x > w / 2.0 { (x - 6.0 - label.chars().count() as f32 * 5.6).max(0.0) } else { x + 6.0 };
            hair.fill_text(Text {
                content: label,
                position: Point::new(lx, MT + 10.0),
                color: crate::ui::pal::txt(),
                size: iced::Pixels(10.0),
                font: crate::style::AZERET_MONO,
                ..Default::default()
            });
            out.push(hair.into_geometry());
        }
        out
    }
}

// ───────────────────────── 柱状图（年度收益 / 收益分布）─────────────────────────

struct BarChart {
    vals: Vec<f64>,
    labels: Vec<String>, // x 轴类别标签（年 / 分箱中心），可空
    color: Option<Color>,
    cache: Cache,
}

impl<M> canvas::Program<M> for BarChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, _c: mouse::Cursor) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if self.vals.is_empty() {
                empty_note(frame, h);
                return;
            }
            let lo = self.vals.iter().cloned().fold(0.0_f64, f64::min);
            let mut hi = self.vals.iter().cloned().fold(0.0_f64, f64::max);
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            draw_grid(frame, w, h, lo, hi);
            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let n = self.vals.len();
            let slot = pw / n as f32;
            let bw = (slot * 0.7).max(1.0);
            let my = |v: f64| MT + ((hi - v) / (hi - lo)) as f32 * ph;
            let zero_y = my(0.0);
            frame.stroke(
                &Path::line(Point::new(ML, zero_y), Point::new(ML + pw, zero_y)),
                Stroke::default().with_width(1.0).with_color(crate::ui::pal::axis()),
            );
            for (i, v) in self.vals.iter().enumerate() {
                let x = ML + i as f32 * slot + (slot - bw) / 2.0;
                let y = my(*v);
                let (top, hh) = if y < zero_y { (y, zero_y - y) } else { (zero_y, y - zero_y) };
                let c = self.color.unwrap_or(if *v >= 0.0 { crate::ui::pal::up() } else { crate::ui::pal::down() });
                frame.fill_rectangle(Point::new(x, top), iced::Size::new(bw, hh.max(1.0)), c);
            }
            // x 轴类别标签：≤12 个时逐个标，否则等距标 4 个
            if !self.labels.is_empty() && self.labels.len() == n {
                let stride = if n <= 12 { 1 } else { (n / 5).max(1) };
                for i in (0..n).step_by(stride) {
                    let x = ML + i as f32 * slot;
                    frame.fill_text(Text {
                        content: self.labels[i].clone(),
                        position: Point::new(x, h - MB + 2.0),
                        color: crate::ui::pal::axis(),
                        size: iced::Pixels(8.5),
                        ..Default::default()
                    });
                }
            }
        });
        vec![geo]
    }
}

// ───────────────────────── 价格 + 成交标记（附加）─────────────────────────

struct PriceChart {
    t: Vec<f64>,
    v: Vec<f64>,
    fills: Vec<[f64; 3]>,
    cache: Cache,
}

impl PriceChart {
    fn plot(&self, w: f32, h: f32) -> Option<super::chart_kit::Plot> {
        if self.v.len() < 2 {
            return None;
        }
        let lo = self.v.iter().cloned().fold(f64::INFINITY, f64::min);
        let mut hi = self.v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        if (hi - lo).abs() < 1e-12 {
            hi = lo + 1.0;
        }
        let (t0, t1) = (self.t[0], *self.t.last()?);
        Some(super::chart_kit::Plot {
            x0: ML,
            y0: MT,
            w: (w - ML - MR).max(1.0),
            h: (h - MT - MB).max(1.0),
            xr: (t0, t0 + (t1 - t0).max(1.0)),
            yr: (lo, hi),
        })
    }
}

impl<M> canvas::Program<M> for PriceChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if self.v.len() < 2 {
                empty_note(frame, h);
                return;
            }
            let (t0, t1) = (self.t[0], *self.t.last().unwrap());
            let tspan = (t1 - t0).max(1.0);
            let lo = self.v.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut hi = self.v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            draw_grid(frame, w, h, lo, hi);
            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let mx = |ts: f64| ML + ((ts - t0) / tspan) as f32 * pw;
            let my = |v: f64| MT + ((hi - v) / (hi - lo)) as f32 * ph;
            let line = Path::new(|p| {
                p.move_to(Point::new(mx(self.t[0]), my(self.v[0])));
                for i in 1..self.v.len() {
                    p.line_to(Point::new(mx(self.t[i]), my(self.v[i])));
                }
            });
            frame.stroke(&line, Stroke::default().with_width(1.3).with_color(crate::ui::pal::info()));
            for f in &self.fills {
                let (ts, side, px) = (f[0], f[1], f[2]);
                let (x, y, s) = (mx(ts), my(px), 3.5_f32);
                let (c, tri) = if side < 1.5 {
                    (crate::ui::pal::up(), Path::new(|p| {
                        p.move_to(Point::new(x, y - s));
                        p.line_to(Point::new(x - s, y + s));
                        p.line_to(Point::new(x + s, y + s));
                        p.close();
                    }))
                } else {
                    (crate::ui::pal::down(), Path::new(|p| {
                        p.move_to(Point::new(x, y + s));
                        p.line_to(Point::new(x - s, y - s));
                        p.line_to(Point::new(x + s, y - s));
                        p.close();
                    }))
                };
                frame.fill(&tri, c);
            }
            for k in 0..4 {
                let i = k * (self.t.len() - 1) / 3;
                frame.fill_text(Text {
                    content: fmt_date(self.t[i] as i64),
                    position: Point::new((mx(self.t[i]) - 22.0).max(0.0), h - MB + 2.0),
                    color: crate::ui::pal::axis(),
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
        });
        // 十字线读数：时间 · 价格；附近有成交时列出成交方向与价
        let mut out = vec![geo];
        if let Some(p) = cursor.position_in(b)
            && let Some(plot) = self.plot(b.width, b.height)
        {
            let mut hair = Frame::new(r, b.size());
            let mut extra = Vec::new();
            if let Some(i) = super::chart_kit::nearest(&self.t, plot.vx(p.x)) {
                extra.push(format!("{}  价 {}", fmt_time(self.t[i] as i64), fmt_num(self.v[i])));
                let near = (plot.xr.1 - plot.xr.0) / f64::from(plot.w.max(1.0)) * 4.0;
                for f in self.fills.iter().filter(|f| (f[0] - self.t[i]).abs() <= near).take(4) {
                    extra.push(format!("{} @ {}", if f[1] < 1.5 { "▲ 买" } else { "▼ 卖" }, fmt_num(f[2])));
                }
            }
            super::chart_kit::crosshair(&mut hair, &plot, p, |v| fmt_time(v as i64), fmt_num, &extra);
            out.push(hair.into_geometry());
        }
        out
    }
}

// ───────────────────────── 组装小部件 ─────────────────────────

fn line_chart<'a, M: 'a>(pts: Vec<f64>, xt: Vec<i64>, color: Color, baseline: Option<f64>, fill: bool, unit: &'static str, h: f32) -> Element<'a, M> {
    chart_line(pts, xt, color, baseline, fill, false, unit, h)
}

/// 同 [`line_chart`]，`money = true` 表示纵轴是金额。
#[allow(clippy::too_many_arguments)]
fn chart_line<'a, M: 'a>(pts: Vec<f64>, xt: Vec<i64>, color: Color, baseline: Option<f64>, fill: bool, money: bool, unit: &'static str, h: f32) -> Element<'a, M> {
    Canvas::new(LineChart { pts, xt, color, baseline, fill, money, unit, cache: Cache::new() })
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .into()
}

fn bar_chart<'a, M: 'a>(vals: Vec<f64>, labels: Vec<String>, color: Option<Color>, h: f32) -> Element<'a, M> {
    Canvas::new(BarChart { vals, labels, color, cache: Cache::new() })
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .into()
}

fn section<'a, M: 'a>(title: &str, c: Color, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    column![
        text(title.to_string()).size(crate::ui::text::s_emph()).color(c),
        container(body).width(Length::Fill).style(crate::style::dashboard_modal),
    ]
    .spacing(3)
    .into()
}

/// 绩效统计里哪些行是金额：带币种的那一节（`PnL Statistics (USDT)`）里，
/// 名称不含 `%` / `Rate` 的行（PnL、盈亏单笔、期望）。比率、胜率、夏普不是金额。
fn is_money_stat(section: &str, label: &str) -> bool {
    section.contains("PnL") && !label.contains('%') && !label.contains("Rate")
}

// ───────────────────────── 统计表（ui::grid，docs/35 §16.15 第 8 项）─────────────────────────

thread_local! {
    /// 两张表各自的网格状态（排序、列宽、折叠的分节）
    static GRIDS: std::cell::RefCell<[Option<GridState>; 2]> = const { std::cell::RefCell::new([None, None]) };
}

/// 列：分节（分组用，隐藏）· 项目 · 值 · 来源。
fn stat_cols() -> Vec<Column> {
    vec![
        Column::text("分节", 160.0).groupable(),
        Column::text("项目", 260.0).key().pinned(),
        Column::num("值", None, 220.0),
        Column::text("来源", 200.0),
    ]
}

fn fresh_state(cols: &[Column]) -> GridState {
    let mut s = GridState::new(cols);
    // 按分节分组，分节列本身不再单独占一列（组头就是分节名）
    s.group_by = Some(0);
    s.hidden.insert(0);
    s
}

/// 表格交互（经 pane 事件 BacktestGrid 回到这里）。`0` = 运行信息，`1` = 绩效统计。
pub fn grid_update(which: u8, m: GridMsg) {
    let cols = stat_cols();
    GRIDS.with(|g| {
        let mut g = g.borrow_mut();
        g[usize::from(which.min(1))].get_or_insert_with(|| fresh_state(&cols)).update(m, &cols, &[]);
    });
}

/// 这次回测的数据成色落成的来源标记（UPDS V8 §72，docs/35 §7.2）：
/// 强制跑（体检判 C 仍照跑）→ 人工覆盖 `^`；体检 C 或没有体检 → 估算 `≈`。
/// 金额另带模拟 `~`（由 `fmt::sim` 加）。**三个标记并列显示**，不按「取最弱」合并——
/// 模拟永远最弱，合并后强制跑与体检等级就看不见了。
struct RunMarks {
    forced: bool,
    estimated: bool,
}

impl RunMarks {
    fn of(r: &BacktestResult) -> Self {
        match &r.data_provenance {
            Some(p) => Self {
                forced: p.quality_flags.iter().any(|f| f.starts_with("forced")),
                estimated: matches!(p.grade.as_deref(), Some("C") | None),
            },
            None => Self { forced: false, estimated: true },
        }
    }

    fn prefix(&self) -> String {
        let mut s = String::new();
        if self.forced {
            s.push_str(crate::ui::fmt::Provenance::Overridden.prefix());
        }
        if self.estimated {
            s.push_str("≈ ");
        }
        s
    }

    fn words(&self, money: bool) -> String {
        let mut w: Vec<&str> = Vec::new();
        if money {
            w.push("~ 模拟");
        }
        if self.forced {
            w.push("^ 强制跑");
        }
        if self.estimated {
            w.push("≈ 体检 C / 未体检");
        }
        w.join(" · ")
    }
}

/// 一行统计 → 网格行。`money`：金额（带 `~`，隐藏数值时为 •••）；`marks`：成色标记（只给结果值，不给运行元数据）。
fn stat_row(section: &str, row: &[String], money: bool, marks: Option<&RunMarks>) -> Vec<Cell> {
    let label = row.first().cloned().unwrap_or_default();
    let raw = row.get(1).cloned().unwrap_or_default();
    // Nautilus 算不出的统计写 nan（没有亏损单时的「最小亏损」等）：是「不适用」，不是一个数，
    // 也不带来源标记（docs/35 §7.1 六种「无」）
    if matches!(raw.trim().to_ascii_lowercase().as_str(), "nan" | "none" | "") {
        return vec![
            Cell::Text(section.to_string()),
            Cell::Text(label),
            Cell::Absent(crate::ui::fmt::Absence::NotApplicable),
            Cell::Colored("算不出（样本不足或没有对应的交易）".into(), crate::ui::pal::dim()),
        ];
    }
    let v = if money { crate::ui::fmt::sim(raw.clone()) } else { raw.clone() };
    let v = match marks {
        Some(m) => format!("{}{v}", m.prefix()),
        None => v,
    };
    let src = marks.map(|m| m.words(money)).unwrap_or_default();
    let num = raw.replace([',', ' ', '%'], "").parse::<f64>().ok();
    vec![
        Cell::Text(section.to_string()),
        Cell::Text(label),
        match num {
            Some(x) => Cell::num(x, v),
            None => Cell::Text(v),
        },
        Cell::Colored(src, crate::ui::pal::dim()),
    ]
}

/// 面板消息：（哪张表, 网格消息）。
pub type BtMsg = (u8, GridMsg);

fn stat_grid<'a>(which: u8, title: &str, rows: Vec<Vec<Cell>>) -> Element<'a, BtMsg> {
    let cols = stat_cols();
    let n = rows.len();
    let state = GRIDS.with(|g| {
        let mut g = g.borrow_mut();
        let s = g[usize::from(which.min(1))].get_or_insert_with(|| fresh_state(&cols));
        s.resort(&cols, &rows);
        s.clone()
    });
    let groups = rows.iter().map(|r| match &r[0] { Cell::Text(s) => s.clone(), _ => String::new() }).collect::<std::collections::BTreeSet<_>>().len();
    // 外层是滚动容器：网格要定高（行 + 组头 + 表头 + 页脚）
    let h = crate::ui::metrics::row_height() * (n + groups + 1) as f32 + crate::ui::metrics::panel_header() + 40.0;
    section(
        title,
        crate::ui::pal::head(),
        container(grid::view(cols, rows, state, None, move |m| (which, m))).height(Length::Fixed(h)),
    )
}

/// 月度收益热力图：年×月 网格，红负绿正、强度按幅值，格内显 %。
fn heatmap<'a, M: 'a>(m: &super::backtest_readout::Monthly) -> Element<'a, M> {
    if m.z.is_empty() || m.months.is_empty() {
        return container(text("数据不足").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())).padding(crate::ui::metrics::space(3)).into();
    }
    let maxabs = m
        .z
        .iter()
        .flatten()
        .filter_map(|o| o.map(|v| v.abs()))
        .fold(1e-9_f64, f64::max);
    let cell_w = Length::Fixed(44.0);
    let mut head = row![container(text("")).width(Length::Fixed(40.0))].spacing(2);
    for mon in &m.months {
        head = head.push(
            container(text(mon.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim())).width(cell_w).align_x(Alignment::Center),
        );
    }
    let mut grid = column![head].spacing(2);
    for (yi, year) in m.years.iter().enumerate() {
        let mut r = row![container(text(year.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()))
            .width(Length::Fixed(40.0))
            .align_y(Alignment::Center)]
        .spacing(2);
        let zrow = m.z.get(yi);
        for mi in 0..m.months.len() {
            let val = zrow.and_then(|rr| rr.get(mi)).and_then(|o| *o);
            let (bg, label) = match val {
                Some(v) => {
                    let inten = (v.abs() / maxabs).clamp(0.0, 1.0) as f32;
                    let c = if v >= 0.0 {
                        crate::ui::pal::alpha(crate::ui::pal::up(), 0.18 + 0.6 * inten)
                    } else {
                        crate::ui::pal::alpha(crate::ui::pal::down(), 0.18 + 0.6 * inten)
                    };
                    (Some(c), format!("{v:.2}"))
                }
                None => (None, String::new()),
            };
            r = r.push(
                container(text(label).size(crate::ui::text::s_meta()))
                    .width(cell_w)
                    .height(Length::Fixed(20.0))
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center)
                    .style(move |_t: &Theme| container::Style {
                        background: bg.map(Background::Color),
                        ..Default::default()
                    }),
            );
        }
        grid = grid.push(r);
    }
    container(scrollable(grid)).padding(crate::ui::metrics::space(2)).into()
}

/// 渲染回测结果：仿官方 tearsheet 版式（表在上、图按序在下）。
/// 进度条：固定宽度的轨道 + 按百分比填充。
///
/// 自绘而不用 iced 的 `ProgressBar`：这个面板整体是自绘风格（tearsheet 的图都是 canvas），
/// 混进一个主题化控件会显得突兀，而且这里只需要一根填充条。
fn progress_bar<'a, M: 'a>(pct: f32) -> Element<'a, M> {
    const W: f32 = 320.0;
    let frac = (pct / 100.0).clamp(0.0, 1.0);
    // **用固定宽度而不是 FillPortion**：只有一个子元素时 FillPortion 会占满整个父容器，
    // 无论比例写多少——那正是「进度条一开始就是 100%」的原因（docs/27 §12）。
    container(
        container(text(""))
            .width(Length::Fixed((W * frac).max(1.0)))
            .height(Length::Fixed(10.0))
            .style(|_: &Theme| container::Style {
                background: Some(Background::Color(crate::ui::pal::up())),
                border: iced::border::rounded(3),
                ..Default::default()
            }),
    )
    .width(Length::Fixed(W))
    .height(Length::Fixed(10.0))
    .style(|_: &Theme| container::Style {
        background: Some(Background::Color(crate::ui::pal::band())),
        border: iced::border::rounded(3),
        ..Default::default()
    })
    .into()
}

/// 溯源横幅（docs/27 S5）——**放在最顶上**。
///
/// 这一段回答的是「这组数字能不能当真」：数据是已购高质量还是自录、窗口体检是几级、
/// 撮合开没开队列位置、费率是多少。没有它，A 级数据 + 真实撮合的结果和覆盖不全的自录数据
/// + 一触价即成交的结果，**长得一模一样**。
///
/// 旧结果没有这一段 → 明说「来源未标注」，而不是留空当作没问题。
fn provenance_banner<'a, M: 'a>(r: &BacktestResult) -> Element<'a, M> {
    let (line1, line2, low) = match &r.data_provenance {
        Some(p) => (p.summary(), p.execution_summary(), p.is_low_confidence()),
        None => (
            "⚠ 来源未标注（此结果早于数据溯源落地）".to_string(),
            "无法判断数据成色与撮合假设——重跑一次即可获得溯源信息".to_string(),
            true,
        ),
    };
    let (bg, fg) = if low {
        (crate::ui::pal::alpha(crate::ui::pal::warn(), 0.20), crate::ui::pal::warn())
    } else {
        (crate::ui::pal::alpha(crate::ui::pal::ok(), 0.18), crate::ui::pal::ok())
    };
    let flags = r
        .data_provenance
        .as_ref()
        .map(|p| p.quality_flags.clone())
        .unwrap_or_default();
    let mut col = column![
        text(format!("{} {line1}", if low { "⚠" } else { "✓" })).size(crate::ui::text::s_body()).color(fg),
        text(line2).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
    ]
    .spacing(2);
    if !flags.is_empty() {
        col = col.push(text(format!("体检标记：{}", flags.join("  "))).size(crate::ui::text::s_meta()).color(fg));
    }
    container(col)
        .padding(crate::ui::metrics::space(3))
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            ..Default::default()
        })
        .into()
}

pub fn pane_body<'a>() -> Element<'a, BtMsg> {
    let r: BacktestResult = super::backtest_readout::snapshot();

    // 正在跑、而手上这份结果属于**别的**运行 → 明说「进行中」，不要把上一次的结论当本次。
    //
    // result.json 是回测跑完才写的，所以运行期间 latest.json 指向的一定是上一次。
    // 照原样渲染的话，那些数字看着完全正常，却与正在跑的这一次毫无关系——
    // 这比显示「暂无数据」危险得多（docs/27 §12）。
    if super::active_run::backtest_running()
        && !super::active_run::is_current_run(&r.meta.run_id)
    {
        let run = super::active_run::current().map(|a| a.run_id).unwrap_or_default();
        let p = super::backtest_readout::progress_snapshot();
        // 进度属于**本次** run 才画——上一次残留的百分比比不画更误导。
        let bar: Element<'a, BtMsg> = if p.run_id == run && !run.is_empty() {
            let clock = chrono::DateTime::from_timestamp((p.clock_ms / 1000) as i64, 0)
                .map(|d| d.format("%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(crate::ui::fmt::unknown);
            column![
                progress_bar::<BtMsg>(p.pct),
                text(format!(
                    "{:.1}%   回测时钟 {clock}   已处理 {} 笔成交",
                    p.pct, p.ticks
                ))
                .size(crate::ui::text::s_small())
                .color(crate::ui::pal::dim()),
            ]
            .spacing(6)
            .align_x(Alignment::Center)
            .into()
        } else {
            text("正在装载数据…（进度在引擎开跑后出现）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()).into()
        };
        return container(iced::widget::center(
            column![
                text("回测进行中…").size(crate::ui::text::s_section()).color(crate::ui::pal::head()),
                text(format!("run {run}")).size(crate::ui::text::s_body()).color(crate::ui::pal::dim()),
                bar,
                text("结果在回测跑完后写入 result.json，这里随即刷新").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
                text("过程中的持仓与收益看「订单」面板，行情看 K 线图").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        ))
        .padding(crate::ui::metrics::space(4))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    if !r.loaded {
        return container(iced::widget::center(
            column![
                text("回测结果").size(crate::ui::text::s_section()).color(crate::ui::pal::head()),
                text("暂无回测结果").size(crate::ui::text::s_emph()),
                text("先跑一次回测：python strategies/quickstart.py").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
                text(format!("读取目录：{}", super::backtest_readout::out_dir_display()))
                    .size(crate::ui::text::s_small())
                    .color(crate::ui::pal::dim()),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        ))
        .padding(crate::ui::metrics::space(4))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let m = &r.meta;
    let header = column![
        text(format!("回测结果 · {} · {}", m.strategy, m.symbol)).size(crate::ui::text::s_section()).color(crate::ui::pal::head()),
        {
            // run id 里编着日期，据此补「距今多久」——只印 run id 时，
            // 6 周前的结果和刚跑完的看起来一模一样（docs/20 §27）
            let stale = super::staleness::is_stale(&m.run);
            text(format!(
                "{} 根K线   ·   run {}{}",
                m.bars,
                m.run,
                super::staleness::suffix(&m.run)
            ))
            .size(crate::ui::text::s_meta())
            .color(if stale { crate::ui::pal::pend() } else { crate::ui::pal::dim() })
        },
    ]
    .spacing(2);
    let header = column![header, provenance_banner::<BtMsg>(&r)].spacing(6);

    // —— 上方两表 —— Run Information（+ Account Summary）/ Performance Statistics（分节）
    // 换成 ui::grid（docs/35 §16.15 第 8 项）：按分节分组、可排序、可复制、可多选求和。
    // 回测里的金额一律是模拟出来的：带 `~`，隐藏数值模式下为 `•••`（docs/35 §7.2 / §9.3）；
    // 结果值另带这次运行的成色标记（强制跑 `^`、体检 C / 未体检 `≈`）
    let marks = RunMarks::of(&r);
    let mut run_rows: Vec<Vec<Cell>> = r.run_info.iter().map(|row| stat_row("Run Information", row, false, None)).collect();
    run_rows.extend(r.account.iter().map(|row| stat_row("Account Summary", row, true, Some(&marks))));
    let stat_rows: Vec<Vec<Cell>> = r
        .stats_sections
        .iter()
        .flat_map(|s| {
            let marks = &marks;
            s.rows.iter().map(move |row| stat_row(&s.name, row, is_money_stat(&s.name, row.first().map_or("", String::as_str)), Some(marks)))
        })
        .collect();
    let run_table = stat_grid(0, "运行信息 / Run Information", run_rows);
    let stats_table = stat_grid(1, "绩效统计 / Performance Statistics", stat_rows);

    let equity_base = r.equity.v.first().copied();
    let yearly_labels: Vec<String> = r.yearly.years.clone();
    let dist_labels: Vec<String> = r.distribution.centers.iter().map(|c| format!("{c:.2}")).collect();
    let n_fills = r.fills.len();

    let charts = column![
        // 报告顺序：equity → drawdown → monthly → distribution → rolling_sharpe → yearly
        section("收益曲线 Equity（资金 · ~ 模拟）", crate::ui::pal::up(), chart_line(r.equity.v.clone(), r.equity.t.clone(), crate::ui::pal::up(), equity_base, false, true, "资金（账户币种）", 200.0)),
        section("回撤 Drawdown %", crate::ui::pal::down(), line_chart(r.drawdown.v.clone(), r.drawdown.t.clone(), crate::ui::pal::down(), Some(0.0), true, "%", 130.0)),
        section("月度收益 Monthly Returns %（年×月）", crate::ui::pal::head(), heatmap(&r.monthly)),
        section("收益分布 Distribution", crate::ui::pal::head(), bar_chart(r.distribution.counts.iter().map(|c| *c as f64).collect(), dist_labels, Some(crate::ui::pal::info()), 120.0)),
        section("滚动夏普 Rolling Sharpe（60 期）", crate::ui::pal::info(), line_chart(r.rolling_sharpe.v.clone(), r.rolling_sharpe.t.clone(), crate::ui::pal::info(), Some(0.0), false, "夏普（无量纲）", 120.0)),
        section("年度收益 Yearly Returns %", crate::ui::pal::up(), bar_chart(r.yearly.v.iter().map(|o| o.unwrap_or(0.0)).collect(), yearly_labels, None, 120.0)),
        // 附加（官方默认报告无此图，cockpit 额外提供）
        section(
            &format!("价格 & 成交 Price & Fills（{n_fills} 笔，附加）"),
            crate::ui::pal::info(),
            Canvas::new(PriceChart {
                t: r.price.t.iter().map(|x| *x as f64).collect(),
                v: r.price.v.clone(),
                fills: r.fills.clone(),
                cache: Cache::new(),
            })
            .width(Length::Fill)
            .height(Length::Fixed(150.0)),
        ),
    ]
    .spacing(10);

    let body = column![header, run_table, stats_table, charts].spacing(12).padding(crate::ui::metrics::space(1));
    container(scrollable(body)).padding(crate::ui::metrics::space(4)).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod gap_tests {
    use super::gaps;

    #[test]
    fn 间隔超过中位数三倍才算缺口() {
        // 1 分钟一根，中间断了 30 分钟
        let t: Vec<i64> = [0, 1, 2, 3, 4, 5, 35, 36, 37, 38, 39].iter().map(|m| m * 60_000).collect();
        assert_eq!(gaps(&t), vec![6]);
        // 均匀的没有缺口；点太少不判
        assert!(gaps(&(0..20).map(|m| m * 60_000).collect::<Vec<i64>>()).is_empty());
        assert!(gaps(&[0, 60_000, 120_000, 900_000]).is_empty());
    }
}

#[cfg(test)]
mod progress_tests {
    /// 进度条已填充宽度（像素）。抽出来是为了能测——上一版用 `FillPortion`，只有一个
    /// 子元素时它会占满整个父容器，于是**进度条从头到尾都是满的**，而百分比文字是对的，
    /// 两者矛盾却没人会当成 bug 报。
    fn filled_px(pct: f32, w: f32) -> f32 {
        ((pct / 100.0).clamp(0.0, 1.0) * w).max(1.0)
    }

    #[test]
    fn 零进度时几乎不填充() {
        assert!(filled_px(0.0, 320.0) <= 1.0, "0% 不该显示成一大截");
    }

    #[test]
    fn 半程填一半() {
        assert!((filled_px(50.0, 320.0) - 160.0).abs() < 0.01);
    }

    #[test]
    fn 满进度填满() {
        assert!((filled_px(100.0, 320.0) - 320.0).abs() < 0.01);
    }

    #[test]
    fn 越界值被夹住() {
        // 上游若因浮点误差给出 100.0001 或 -0.1，不该画出超宽/负宽。
        assert!((filled_px(150.0, 320.0) - 320.0).abs() < 0.01);
        assert!(filled_px(-10.0, 320.0) <= 1.0);
    }
}
