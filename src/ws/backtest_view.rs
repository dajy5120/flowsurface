//! 回测结果 pane 的视图渲染（docs/08 F6-P7）。
//!
//! 把回测脚本导出的 result.json 在 cockpit 原生展示——**与官方 HTML tearsheet 同内容、无遗漏**：
//! 收益曲线 · 回撤 · 价格&成交 · 月度收益热力图 · 年度收益 · 滚动夏普 · 收益分布 · 各维度统计。
//! 数据走 [`super::backtest_readout`] 旁路快照；只渲染、不发消息，对 pane 消息类型 `M` 泛型。

use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Background, Color, Element, Length, Point, Rectangle, Renderer, Theme, mouse};

use super::backtest_readout::BacktestResult;

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_EQUITY: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_DD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_GRID: Color = Color::from_rgba(0.6, 0.6, 0.65, 0.25);
const C_POS: Color = Color::from_rgb(0.35, 0.78, 0.45);
const C_NEG: Color = Color::from_rgb(0.88, 0.42, 0.4);
const C_BUY: Color = Color::from_rgb(0.3, 0.85, 0.45);
const C_SELL: Color = Color::from_rgb(0.92, 0.45, 0.42);
const C_BLUE: Color = Color::from_rgb(0.5, 0.7, 0.95);

fn fmt_num(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

// ───────────────────────── 折线图（收益/回撤/滚动夏普）─────────────────────────

struct LineChart {
    pts: Vec<f64>,
    color: Color,
    baseline: Option<f64>,
    fill: bool,
    cache: Cache,
}

impl<M> canvas::Program<M> for LineChart {
    type State = ();
    fn draw(
        &self,
        _s: &(),
        renderer: &Renderer,
        _t: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            let pad = 8.0_f32;
            if self.pts.len() < 2 {
                frame.fill_text(Text {
                    content: "数据不足".to_string(),
                    position: Point::new(8.0, h / 2.0 - 6.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let mut lo = self.pts.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut hi = self.pts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            if let Some(b) = self.baseline {
                lo = lo.min(b);
                hi = hi.max(b);
            }
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            let plot_h = (h - 2.0 * pad).max(1.0);
            let n = self.pts.len();
            let mx = |i: usize| (i as f32) / ((n - 1) as f32) * w;
            let my = |v: f64| pad + ((hi - v) / (hi - lo)) as f32 * plot_h;

            if let Some(b) = self.baseline {
                let by = my(b);
                frame.stroke(
                    &Path::line(Point::new(0.0, by), Point::new(w, by)),
                    Stroke::default().with_width(1.0).with_color(C_GRID),
                );
            }
            if self.fill {
                let base_y = self.baseline.map(my).unwrap_or(h - pad);
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
            let line = Path::new(|p| {
                p.move_to(Point::new(mx(0), my(self.pts[0])));
                for (i, v) in self.pts.iter().enumerate().skip(1) {
                    p.line_to(Point::new(mx(i), my(*v)));
                }
            });
            frame.stroke(&line, Stroke::default().with_width(1.6).with_color(self.color));
            frame.fill_text(Text {
                content: fmt_num(hi),
                position: Point::new(4.0, 2.0),
                color: C_DIM,
                size: iced::Pixels(10.0),
                ..Default::default()
            });
            frame.fill_text(Text {
                content: fmt_num(lo),
                position: Point::new(4.0, h - 12.0),
                color: C_DIM,
                size: iced::Pixels(10.0),
                ..Default::default()
            });
        });
        vec![geo]
    }
}

// ───────────────────────── 柱状图（年度收益 / 收益分布）─────────────────────────

struct BarChart {
    vals: Vec<f64>,
    /// 单色（分布）；None 时按正负用绿/红（年度收益）。
    color: Option<Color>,
    cache: Cache,
}

impl<M> canvas::Program<M> for BarChart {
    type State = ();
    fn draw(
        &self,
        _s: &(),
        renderer: &Renderer,
        _t: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            let pad = 8.0_f32;
            if self.vals.is_empty() {
                frame.fill_text(Text {
                    content: "数据不足".to_string(),
                    position: Point::new(8.0, h / 2.0 - 6.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let lo = self.vals.iter().cloned().fold(0.0_f64, f64::min);
            let mut hi = self.vals.iter().cloned().fold(0.0_f64, f64::max);
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            let plot_h = (h - 2.0 * pad).max(1.0);
            let n = self.vals.len();
            let slot = w / n as f32;
            let bw = (slot * 0.7).max(1.0);
            let my = |v: f64| pad + ((hi - v) / (hi - lo)) as f32 * plot_h;
            let zero_y = my(0.0);
            frame.stroke(
                &Path::line(Point::new(0.0, zero_y), Point::new(w, zero_y)),
                Stroke::default().with_width(1.0).with_color(C_GRID),
            );
            for (i, v) in self.vals.iter().enumerate() {
                let x = i as f32 * slot + (slot - bw) / 2.0;
                let y = my(*v);
                let (top, height) = if y < zero_y { (y, zero_y - y) } else { (zero_y, y - zero_y) };
                let c = self.color.unwrap_or(if *v >= 0.0 { C_POS } else { C_NEG });
                frame.fill_rectangle(Point::new(x, top), iced::Size::new(bw, height.max(1.0)), c);
            }
            frame.fill_text(Text {
                content: fmt_num(hi),
                position: Point::new(4.0, 2.0),
                color: C_DIM,
                size: iced::Pixels(10.0),
                ..Default::default()
            });
            if lo < 0.0 {
                frame.fill_text(Text {
                    content: fmt_num(lo),
                    position: Point::new(4.0, h - 12.0),
                    color: C_DIM,
                    size: iced::Pixels(10.0),
                    ..Default::default()
                });
            }
        });
        vec![geo]
    }
}

// ───────────────────────── 价格 + 成交标记（bars_with_fills）─────────────────────────

struct PriceChart {
    t: Vec<f64>,
    v: Vec<f64>,
    fills: Vec<[f64; 3]>, // [ts, side(1买/2卖), px]
    cache: Cache,
}

impl<M> canvas::Program<M> for PriceChart {
    type State = ();
    fn draw(
        &self,
        _s: &(),
        renderer: &Renderer,
        _t: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            let pad = 8.0_f32;
            if self.v.len() < 2 {
                frame.fill_text(Text {
                    content: "数据不足".to_string(),
                    position: Point::new(8.0, h / 2.0 - 6.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let (t0, t1) = (self.t[0], *self.t.last().unwrap());
            let tspan = (t1 - t0).max(1.0);
            let lo = self.v.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut hi = self.v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            if (hi - lo).abs() < 1e-12 {
                hi = lo + 1.0;
            }
            let plot_h = (h - 2.0 * pad).max(1.0);
            let mx = |ts: f64| ((ts - t0) / tspan) as f32 * w;
            let my = |v: f64| pad + ((hi - v) / (hi - lo)) as f32 * plot_h;

            let line = Path::new(|p| {
                p.move_to(Point::new(mx(self.t[0]), my(self.v[0])));
                for i in 1..self.v.len() {
                    p.line_to(Point::new(mx(self.t[i]), my(self.v[i])));
                }
            });
            frame.stroke(&line, Stroke::default().with_width(1.3).with_color(C_BLUE));

            // 成交标记：▲ 买（绿）/ ▼ 卖（红）。
            for f in &self.fills {
                let (ts, side, px) = (f[0], f[1], f[2]);
                let x = mx(ts);
                let y = my(px);
                let s = 3.5_f32;
                let (c, tri) = if side < 1.5 {
                    (C_BUY, Path::new(|p| {
                        p.move_to(Point::new(x, y - s));
                        p.line_to(Point::new(x - s, y + s));
                        p.line_to(Point::new(x + s, y + s));
                        p.close();
                    }))
                } else {
                    (C_SELL, Path::new(|p| {
                        p.move_to(Point::new(x, y + s));
                        p.line_to(Point::new(x - s, y - s));
                        p.line_to(Point::new(x + s, y - s));
                        p.close();
                    }))
                };
                frame.fill(&tri, c);
            }
            frame.fill_text(Text {
                content: fmt_num(hi),
                position: Point::new(4.0, 2.0),
                color: C_DIM,
                size: iced::Pixels(10.0),
                ..Default::default()
            });
            frame.fill_text(Text {
                content: fmt_num(lo),
                position: Point::new(4.0, h - 12.0),
                color: C_DIM,
                size: iced::Pixels(10.0),
                ..Default::default()
            });
        });
        vec![geo]
    }
}

// ───────────────────────── 组装小部件 ─────────────────────────

fn line_chart<'a, M: 'a>(pts: Vec<f64>, color: Color, baseline: Option<f64>, fill: bool, h: f32) -> Element<'a, M> {
    Canvas::new(LineChart { pts, color, baseline, fill, cache: Cache::new() })
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .into()
}

fn bar_chart<'a, M: 'a>(vals: Vec<f64>, color: Option<Color>, h: f32) -> Element<'a, M> {
    Canvas::new(BarChart { vals, color, cache: Cache::new() })
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .into()
}

fn section<'a, M: 'a>(title: &str, c: Color, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    column![
        text(title.to_string()).size(14).color(c),
        container(body).width(Length::Fill).style(crate::style::dashboard_modal),
    ]
    .spacing(4)
    .into()
}

/// 月度收益热力图：年×月 网格，颜色按收益正负/强度（红负绿正），格内显 %。
fn heatmap<'a, M: 'a>(m: &super::backtest_readout::Monthly) -> Element<'a, M> {
    if m.z.is_empty() || m.months.is_empty() {
        return text("数据不足").size(11).color(C_DIM).into();
    }
    let maxabs = m
        .z
        .iter()
        .flatten()
        .filter_map(|o| o.map(|v| v.abs()))
        .fold(1e-9_f64, f64::max);

    let cell_w = Length::Fixed(46.0);
    let mut head = row![container(text("")).width(Length::Fixed(44.0))].spacing(2);
    for mon in &m.months {
        head = head.push(
            container(text(mon.clone()).size(9).color(C_DIM))
                .width(cell_w)
                .align_x(Alignment::Center),
        );
    }
    let mut grid = column![head].spacing(2);

    for (yi, year) in m.years.iter().enumerate() {
        let mut r = row![container(text(year.clone()).size(10).color(C_DIM))
            .width(Length::Fixed(44.0))
            .align_y(Alignment::Center)]
        .spacing(2);
        let zrow = m.z.get(yi);
        for mi in 0..m.months.len() {
            let val = zrow.and_then(|rr| rr.get(mi)).and_then(|o| *o);
            let (bg, label) = match val {
                Some(v) => {
                    let inten = (v.abs() / maxabs).clamp(0.0, 1.0) as f32;
                    let c = if v >= 0.0 {
                        Color::from_rgba(0.3, 0.75, 0.45, 0.18 + 0.6 * inten)
                    } else {
                        Color::from_rgba(0.85, 0.4, 0.4, 0.18 + 0.6 * inten)
                    };
                    (Some(c), format!("{v:.2}"))
                }
                None => (None, String::new()),
            };
            let cell = container(text(label).size(9))
                .width(cell_w)
                .height(Length::Fixed(20.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(move |_theme: &Theme| container::Style {
                    background: bg.map(Background::Color),
                    ..Default::default()
                });
            r = r.push(cell);
        }
        grid = grid.push(r);
    }
    scrollable(grid).width(Length::Fill).into()
}

/// 渲染回测结果：与官方 tearsheet 同内容（无遗漏）。
pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let r: BacktestResult = super::backtest_readout::snapshot();

    if !r.loaded {
        return container(iced::widget::center(
            column![
                text("回测结果").size(16).color(C_HEAD),
                text("暂无回测结果").size(13),
                text("先跑一次回测：python strategies/quickstart.py").size(11).color(C_DIM),
                text(format!("读取目录：{}", super::backtest_readout::out_dir_display()))
                    .size(11)
                    .color(C_DIM),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        ))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let m = &r.meta;
    let header = column![
        text(format!("回测结果 · {} · {}", m.strategy, m.symbol)).size(16).color(C_HEAD),
        text(format!("{} 根K线   ·   完成于 {}   ·   run {}", m.bars, m.finished_at, m.run))
            .size(11)
            .color(C_DIM),
    ]
    .spacing(3);

    let equity_base = r.equity.v.first().copied();
    let n_fills = r.fills.len();

    let body = column![
        header,
        section("收益曲线（资金）", C_EQUITY, line_chart(r.equity.v.clone(), C_EQUITY, equity_base, false, 160.0)),
        section("回撤 %", C_DD, line_chart(r.drawdown.v.clone(), C_DD, Some(0.0), true, 120.0)),
        section(
            &format!("价格 & 成交（{n_fills} 笔）", ),
            C_BLUE,
            Canvas::new(PriceChart {
                t: r.price.t.iter().map(|x| *x as f64).collect(),
                v: r.price.v.clone(),
                fills: r.fills.clone(),
                cache: Cache::new(),
            })
            .width(Length::Fill)
            .height(Length::Fixed(150.0)),
        ),
        section("月度收益 %（年×月）", C_HEAD, heatmap(&r.monthly)),
        section("年度收益 %", C_POS, bar_chart(r.yearly.v.iter().map(|o| o.unwrap_or(0.0)).collect(), None, 110.0)),
        section("滚动夏普（60 期）", C_BLUE, line_chart(r.rolling_sharpe.v.clone(), C_BLUE, Some(0.0), false, 110.0)),
        section("收益分布", C_HEAD, bar_chart(r.distribution.counts.iter().map(|c| *c as f64).collect(), Some(C_BLUE), 110.0)),
        stats_table(&r.stats),
    ]
    .spacing(12)
    .padding(4);

    container(scrollable(body)).padding(12).width(Length::Fill).height(Length::Fill).into()
}

fn stats_table<'a, M: 'a>(stats: &[Vec<String>]) -> Element<'a, M> {
    let mut col = column![text("各维度统计").size(14).color(C_HEAD)].spacing(2);
    for kv in stats {
        let k = kv.first().cloned().unwrap_or_default();
        let v = kv.get(1).cloned().unwrap_or_default();
        col = col.push(
            row![
                container(text(k).size(11).color(C_DIM)).width(Length::Fixed(220.0)),
                text(v).size(11).font(crate::style::AZERET_MONO),
            ]
            .spacing(8),
        );
    }
    col.into()
}
