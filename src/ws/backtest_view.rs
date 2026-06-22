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

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_EQUITY: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_DD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_GRID: Color = Color::from_rgba(0.6, 0.6, 0.65, 0.22);
const C_AXIS: Color = Color::from_rgb(0.5, 0.5, 0.55);
const C_POS: Color = Color::from_rgb(0.35, 0.78, 0.45);
const C_NEG: Color = Color::from_rgb(0.88, 0.42, 0.4);
const C_BUY: Color = Color::from_rgb(0.3, 0.85, 0.45);
const C_SELL: Color = Color::from_rgb(0.92, 0.45, 0.42);
const C_BLUE: Color = Color::from_rgb(0.5, 0.7, 0.95);
const C_SECT: Color = Color::from_rgba(0.45, 0.6, 0.9, 0.16);

const ML: f32 = 46.0; // 左边距（y 轴标签）
const MB: f32 = 16.0; // 下边距（x 轴标签）
const MT: f32 = 6.0;
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
    let pw = (w - ML - MR).max(1.0);
    for i in 0..=4 {
        let v = lo + (hi - lo) * (i as f64) / 4.0;
        let y = MT + ((hi - v) / (hi - lo)) as f32 * (h - MT - MB).max(1.0);
        frame.stroke(
            &Path::line(Point::new(ML, y), Point::new(ML + pw, y)),
            Stroke::default().with_width(1.0).with_color(C_GRID),
        );
        frame.fill_text(Text {
            content: fmt_num(v),
            position: Point::new(2.0, y - 5.0),
            color: C_AXIS,
            size: iced::Pixels(9.0),
            ..Default::default()
        });
    }
}

fn empty_note(frame: &mut Frame, h: f32) {
    frame.fill_text(Text {
        content: "数据不足".to_string(),
        position: Point::new(ML + 4.0, h / 2.0 - 6.0),
        color: C_DIM,
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
    cache: Cache,
}

impl<M> canvas::Program<M> for LineChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, _c: mouse::Cursor) -> Vec<Geometry> {
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
            draw_grid(frame, w, h, lo, hi);
            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let n = self.pts.len();
            let mx = |i: usize| ML + (i as f32) / ((n - 1) as f32) * pw;
            let my = |v: f64| MT + ((hi - v) / (hi - lo)) as f32 * ph;

            if let Some(bl) = self.baseline {
                let by = my(bl);
                frame.stroke(
                    &Path::line(Point::new(ML, by), Point::new(ML + pw, by)),
                    Stroke::default().with_width(1.0).with_color(C_AXIS),
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
            let line = Path::new(|p| {
                p.move_to(Point::new(mx(0), my(self.pts[0])));
                for (i, v) in self.pts.iter().enumerate().skip(1) {
                    p.line_to(Point::new(mx(i), my(*v)));
                }
            });
            frame.stroke(&line, Stroke::default().with_width(1.6).with_color(self.color));

            // x 轴日期刻度（4 个）
            if self.xt.len() == n {
                for k in 0..4 {
                    let i = k * (n - 1) / 3;
                    frame.fill_text(Text {
                        content: fmt_date(self.xt[i]),
                        position: Point::new((mx(i) - 22.0).max(0.0), h - MB + 2.0),
                        color: C_AXIS,
                        size: iced::Pixels(9.0),
                        ..Default::default()
                    });
                }
            }
        });
        vec![geo]
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
                Stroke::default().with_width(1.0).with_color(C_AXIS),
            );
            for (i, v) in self.vals.iter().enumerate() {
                let x = ML + i as f32 * slot + (slot - bw) / 2.0;
                let y = my(*v);
                let (top, hh) = if y < zero_y { (y, zero_y - y) } else { (zero_y, y - zero_y) };
                let c = self.color.unwrap_or(if *v >= 0.0 { C_POS } else { C_NEG });
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
                        color: C_AXIS,
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

impl<M> canvas::Program<M> for PriceChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, _c: mouse::Cursor) -> Vec<Geometry> {
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
            frame.stroke(&line, Stroke::default().with_width(1.3).with_color(C_BLUE));
            for f in &self.fills {
                let (ts, side, px) = (f[0], f[1], f[2]);
                let (x, y, s) = (mx(ts), my(px), 3.5_f32);
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
            for k in 0..4 {
                let i = k * (self.t.len() - 1) / 3;
                frame.fill_text(Text {
                    content: fmt_date(self.t[i] as i64),
                    position: Point::new((mx(self.t[i]) - 22.0).max(0.0), h - MB + 2.0),
                    color: C_AXIS,
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
        });
        vec![geo]
    }
}

// ───────────────────────── 组装小部件 ─────────────────────────

fn line_chart<'a, M: 'a>(pts: Vec<f64>, xt: Vec<i64>, color: Color, baseline: Option<f64>, fill: bool, h: f32) -> Element<'a, M> {
    Canvas::new(LineChart { pts, xt, color, baseline, fill, cache: Cache::new() })
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
        text(title.to_string()).size(13).color(c),
        container(body).width(Length::Fill).style(crate::style::dashboard_modal),
    ]
    .spacing(3)
    .into()
}

/// 表里的子节标题（分层：浅蓝底 + 粗体感）。
fn subheader<'a, M: 'a>(s: &str) -> Element<'a, M> {
    container(text(s.to_string()).size(11).color(C_HEAD))
        .width(Length::Fill)
        .padding([2, 6])
        .style(|_t: &Theme| container::Style {
            background: Some(Background::Color(C_SECT)),
            ..Default::default()
        })
        .into()
}

fn kv<'a, M: 'a>(k: &str, v: &str) -> Element<'a, M> {
    row![
        container(text(k.to_string()).size(10).color(C_DIM)).width(Length::FillPortion(5)),
        text(v.to_string()).size(10).font(crate::style::AZERET_MONO),
    ]
    .spacing(8)
    .padding([0, 6])
    .into()
}

/// 一张「表」：标题 + 若干分节（每节子标题 + 行）。
fn table_card<'a, M: 'a>(title: &str, sections: Vec<(String, &Vec<Vec<String>>)>) -> Element<'a, M> {
    let mut col = column![].spacing(2);
    for (name, rows) in sections {
        if rows.is_empty() {
            continue;
        }
        col = col.push(subheader(&name));
        for kvp in rows {
            col = col.push(kv(
                kvp.first().map(String::as_str).unwrap_or(""),
                kvp.get(1).map(String::as_str).unwrap_or(""),
            ));
        }
    }
    section(title, C_HEAD, col)
}

/// 月度收益热力图：年×月 网格，红负绿正、强度按幅值，格内显 %。
fn heatmap<'a, M: 'a>(m: &super::backtest_readout::Monthly) -> Element<'a, M> {
    if m.z.is_empty() || m.months.is_empty() {
        return container(text("数据不足").size(11).color(C_DIM)).padding(8).into();
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
            container(text(mon.clone()).size(9).color(C_DIM)).width(cell_w).align_x(Alignment::Center),
        );
    }
    let mut grid = column![head].spacing(2);
    for (yi, year) in m.years.iter().enumerate() {
        let mut r = row![container(text(year.clone()).size(10).color(C_DIM))
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
                        Color::from_rgba(0.3, 0.75, 0.45, 0.18 + 0.6 * inten)
                    } else {
                        Color::from_rgba(0.85, 0.4, 0.4, 0.18 + 0.6 * inten)
                    };
                    (Some(c), format!("{v:.2}"))
                }
                None => (None, String::new()),
            };
            r = r.push(
                container(text(label).size(9))
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
    container(scrollable(grid)).padding(6).into()
}

/// 渲染回测结果：仿官方 tearsheet 版式（表在上、图按序在下）。
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
        text(format!("{} 根K线   ·   run {}", m.bars, m.run)).size(10).color(C_DIM),
    ]
    .spacing(2);

    // —— 上方两表 —— Run Information（+ Account Summary）/ Performance Statistics（分节）
    let run_table = table_card(
        "运行信息 / Run Information",
        vec![
            ("Run Information".to_string(), &r.run_info),
            ("Account Summary".to_string(), &r.account),
        ],
    );
    let stats_table = table_card(
        "绩效统计 / Performance Statistics",
        r.stats_sections.iter().map(|s| (s.name.clone(), &s.rows)).collect(),
    );

    let equity_base = r.equity.v.first().copied();
    let yearly_labels: Vec<String> = r.yearly.years.clone();
    let dist_labels: Vec<String> = r.distribution.centers.iter().map(|c| format!("{c:.2}")).collect();
    let n_fills = r.fills.len();

    let charts = column![
        // 报告顺序：equity → drawdown → monthly → distribution → rolling_sharpe → yearly
        section("收益曲线 Equity（资金）", C_EQUITY, line_chart(r.equity.v.clone(), r.equity.t.clone(), C_EQUITY, equity_base, false, 200.0)),
        section("回撤 Drawdown %", C_DD, line_chart(r.drawdown.v.clone(), r.drawdown.t.clone(), C_DD, Some(0.0), true, 130.0)),
        section("月度收益 Monthly Returns %（年×月）", C_HEAD, heatmap(&r.monthly)),
        section("收益分布 Distribution", C_HEAD, bar_chart(r.distribution.counts.iter().map(|c| *c as f64).collect(), dist_labels, Some(C_BLUE), 120.0)),
        section("滚动夏普 Rolling Sharpe（60 期）", C_BLUE, line_chart(r.rolling_sharpe.v.clone(), r.rolling_sharpe.t.clone(), C_BLUE, Some(0.0), false, 120.0)),
        section("年度收益 Yearly Returns %", C_POS, bar_chart(r.yearly.v.iter().map(|o| o.unwrap_or(0.0)).collect(), yearly_labels, None, 120.0)),
        // 附加（官方默认报告无此图，cockpit 额外提供）
        section(
            &format!("价格 & 成交 Price & Fills（{n_fills} 笔，附加）"),
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
    ]
    .spacing(10);

    let body = column![header, run_table, stats_table, charts].spacing(12).padding(4);
    container(scrollable(body)).padding(12).width(Length::Fill).height(Length::Fill).into()
}
