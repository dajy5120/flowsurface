//! 回测结果 pane 的视图渲染（docs/08 F6-P7）。
//!
//! 把回测脚本导出的 result.json 在 cockpit 原生展示：收益曲线 + 回撤（iced 画布折线）、
//! 各维度统计（表格）。数据走 [`super::backtest_readout`] 旁路快照；只渲染、不发消息，
//! 对 pane 消息类型 `M` 泛型。

use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Theme, mouse};

use super::backtest_readout::BacktestResult;

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_EQUITY: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_DD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_GRID: Color = Color::from_rgba(0.6, 0.6, 0.65, 0.25);

/// 折线图画布程序：y 值序列（x=索引等距），可选基线 + 区域填充。
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
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let w = frame.width();
            let h = frame.height();
            let pad = 8.0_f32;

            if self.pts.len() < 2 {
                frame.fill_text(Text {
                    content: "暂无曲线数据".to_string(),
                    position: Point::new(8.0, h / 2.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }

            // y 值域（含基线），避免退化。
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
            let map_x = |i: usize| (i as f32) / ((n - 1) as f32) * w;
            let map_y = |v: f64| pad + ((hi - v) / (hi - lo)) as f32 * plot_h;

            // 基线（零/起始资金）：浅色横线。
            if let Some(b) = self.baseline {
                let by = map_y(b);
                frame.stroke(
                    &Path::line(Point::new(0.0, by), Point::new(w, by)),
                    Stroke::default().with_width(1.0).with_color(C_GRID),
                );
            }

            // 区域填充（回撤用）：折线 + 落到基线/底部 围成闭合区域。
            if self.fill {
                let base_y = self.baseline.map(map_y).unwrap_or(h - pad);
                let area = Path::new(|p| {
                    p.move_to(Point::new(map_x(0), base_y));
                    for (i, v) in self.pts.iter().enumerate() {
                        p.line_to(Point::new(map_x(i), map_y(*v)));
                    }
                    p.line_to(Point::new(map_x(n - 1), base_y));
                    p.close();
                });
                frame.fill(
                    &area,
                    Color { a: 0.16, ..self.color },
                );
            }

            // 折线本体。
            let line = Path::new(|p| {
                p.move_to(Point::new(map_x(0), map_y(self.pts[0])));
                for (i, v) in self.pts.iter().enumerate().skip(1) {
                    p.line_to(Point::new(map_x(i), map_y(*v)));
                }
            });
            frame.stroke(
                &line,
                Stroke::default().with_width(1.6).with_color(self.color),
            );

            // 角标：最高/最低值。
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

fn fmt_num(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

fn chart<'a, M: 'a>(pts: Vec<f64>, color: Color, baseline: Option<f64>, fill: bool, h: f32) -> Element<'a, M> {
    Canvas::new(LineChart {
        pts,
        color,
        baseline,
        fill,
        cache: Cache::new(),
    })
    .width(Length::Fill)
    .height(Length::Fixed(h))
    .into()
}

fn section_title<'a, M: 'a>(s: &str, c: Color) -> Element<'a, M> {
    text(s.to_string()).size(14).color(c).into()
}

/// 渲染回测结果（收益曲线 / 回撤 / 各维度统计）。
pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let r: BacktestResult = super::backtest_readout::snapshot();

    if !r.loaded {
        return container(
            iced::widget::center(
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
            ),
        )
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let m = &r.meta;
    let header = column![
        text(format!("回测结果 · {} · {}", m.strategy, m.symbol))
            .size(16)
            .color(C_HEAD),
        text(format!(
            "{} 根K线   ·   完成于 {}   ·   run {}",
            m.bars, m.finished_at, m.run
        ))
        .size(11)
        .color(C_DIM),
    ]
    .spacing(3);

    // 起始资金作收益曲线基线。
    let equity_base = r.equity.v.first().copied();
    let equity_section = column![
        section_title("收益曲线（资金）", C_EQUITY),
        container(chart(r.equity.v.clone(), C_EQUITY, equity_base, false, 170.0))
            .width(Length::Fill)
            .style(crate::style::dashboard_modal),
    ]
    .spacing(4);

    let dd_section = column![
        section_title("回撤 %", C_DD),
        container(chart(r.drawdown.v.clone(), C_DD, Some(0.0), true, 130.0))
            .width(Length::Fill)
            .style(crate::style::dashboard_modal),
    ]
    .spacing(4);

    // 各维度统计：两列 [标签 | 值]。
    let mut stats_col = column![section_title("各维度统计", C_HEAD)].spacing(2);
    for kv in &r.stats {
        let k = kv.first().cloned().unwrap_or_default();
        let v = kv.get(1).cloned().unwrap_or_default();
        stats_col = stats_col.push(
            row![
                container(text(k).size(11).color(C_DIM)).width(Length::Fixed(220.0)),
                text(v).size(11).font(crate::style::AZERET_MONO),
            ]
            .spacing(8),
        );
    }

    let body = column![header, equity_section, dd_section, stats_col]
        .spacing(12)
        .padding(4);

    container(scrollable(body))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
