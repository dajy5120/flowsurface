//! 「图表参数」视图的渲染（docs/33 §8）：12 张经典图表卡片。按宽度自动排 1–6 列，卡片固定高度、内容在卡片里滚动。
//!
//! 每张卡片：标题（图名 · 对应的 Sierra Study · 这张图回答的问题）→ 参数表（参数 · 值 · 来源角标）
//! → 价梯（价位类参数按价格排在一根竖轴上，现价插在其中）→ 口径行（本卡生效的口径参数）。
//! 只读快照，**不算任何数**。

use iced::widget::{button, column, container, row, text, text_input, tooltip};
use iced::{Alignment, Color, Element, Length};

use super::chart_params::{self as cp, Card, Cell, ChartCol, ChartEditMsg, ChartUiMsg, SlotIndex};
use super::feature_matrix::FeatureMatrixMsg as Msg;
use super::feature_matrix_readout::Matrix;

const C_HEAD: Color = Color::from_rgb(0.70, 0.80, 0.95);
const C_TXT: Color = Color::from_rgb(0.86, 0.89, 0.94);
const C_DIM: Color = Color::from_rgb(0.50, 0.54, 0.60);
const C_PEND: Color = Color::from_rgb(0.42, 0.44, 0.50);
const C_WARN: Color = Color::from_rgb(0.90, 0.72, 0.32);
const C_FEAT: Color = Color::from_rgb(0.45, 0.65, 0.95);
const C_CHART: Color = Color::from_rgb(0.75, 0.55, 0.95);
const C_NOW: Color = Color::from_rgb(0.95, 0.85, 0.35);
const C_BORDER: Color = Color::from_rgba(0.72, 0.78, 0.88, 0.28);
const C_CARD_BG: Color = Color::from_rgba(0.55, 0.62, 0.75, 0.05);

const W_LABEL: f32 = 150.0;

fn tip<'a>(e: impl Into<Element<'a, Msg>>, t: String) -> Element<'a, Msg> {
    tooltip(e, container(text(t).size(11)).style(crate::style::tooltip).padding(6), tooltip::Position::Top).into()
}

fn fixed<'a>(e: impl Into<Element<'a, Msg>>, w: f32, right: bool) -> Element<'a, Msg> {
    container(e)
        .width(Length::Fixed(w))
        .align_x(if right { Alignment::End } else { Alignment::Start })
        .clip(true)
        .into()
}

/// 卡片序号 ①–⑫。
fn circled(i: usize) -> &'static str {
    const N: [&str; 12] = ["①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩", "⑪", "⑫"];
    N.get(i).copied().unwrap_or("·")
}

/// 表格线（与特征矩阵同一组颜色）。
const C_LINE: Color = Color::from_rgba(0.72, 0.78, 0.88, 0.16);
const C_HEAD_LINE: Color = Color::from_rgba(0.78, 0.84, 0.95, 0.62);
const C_BAND: Color = Color::from_rgba(0.45, 0.60, 0.90, 0.18);
const C_UP: Color = Color::from_rgb(0.30, 0.82, 0.45);
const C_DOWN: Color = Color::from_rgb(0.93, 0.36, 0.34);
/// 卡片最小宽度：窄于它就减少列数。
const CARD_MIN_W: f32 = 260.0;
const GAP: f32 = 8.0;
/// 分隔线（含可拖的把手）的宽度。
const SEP: f32 = 5.0;

/// 竖线：高度随所在行（行高随文字折行自动撑开）。
fn vline<'a>(c: Color) -> Element<'a, Msg> {
    container(iced::widget::rule::vertical(1.0).style(move |_t: &iced::Theme| iced::widget::rule::Style {
        color: c,
        radius: iced::border::Radius::default(),
        fill_mode: iced::widget::rule::FillMode::Full,
        snap: true,
    }))
    .width(Length::Fixed(SEP))
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .into()
}

/// 表头里的分隔把手：拖动调宽度，双击回到默认宽度。`grow` = 往右拖是加宽这一列（否则是变窄——
/// 「参数」与「值」之间那条线往右拖，参数列变宽、值列变窄）。
fn grip<'a>(col: ChartCol, grow: bool) -> Element<'a, Msg> {
    iced::widget::mouse_area(vline(C_HEAD_LINE))
        .on_press(Msg::ChartUi(ChartUiMsg::DragStart(col, cp::col_w(col), grow)))
        .on_double_click(Msg::ChartUi(ChartUiMsg::Auto(col)))
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .into()
}

/// 滚动条单独占一条、不压在内容上。
fn vscroll<'a>(e: impl Into<Element<'a, Msg>>) -> iced::widget::Scrollable<'a, Msg> {
    iced::widget::scrollable(e).direction(iced::widget::scrollable::Direction::Vertical(
        iced::widget::scrollable::Scrollbar::new().width(6).scroller_width(6).spacing(3),
    ))
}

fn hline<'a>(c: Color) -> Element<'a, Msg> {
    iced::widget::rule::horizontal(1.0)
        .style(move |_t: &iced::Theme| iced::widget::rule::Style {
            color: c,
            radius: iced::border::Radius::default(),
            fill_mode: iced::widget::rule::FillMode::Full,
            snap: true,
        })
        .into()
}

/// 定宽 / 填满的格子：行高随内容（文字折成两行时整行撑高），垂直居中。
fn cell<'a>(e: impl Into<Element<'a, Msg>>, w: Length, right: bool) -> Element<'a, Msg> {
    container(e)
        .width(w)
        .padding([2, 4])
        .align_y(Alignment::Center)
        .align_x(if right { Alignment::End } else { Alignment::Start })
        .clip(true)
        .into()
}

/// 涨跌箭头的上下文：与特征矩阵同一组设置（比较时长、阈值、绿涨红跌）。
pub struct Trend {
    /// 特征：N 秒前那份快照的每格 (z, 值)。
    past: Option<Box<[(f32, f32)]>>,
    /// 图表参数：N 秒前后的变化折成近 10 分钟 σ 的倍数。
    chart: std::sync::Arc<std::collections::HashMap<String, f64>>,
    ths: Vec<f64>,
    label: String,
}

impl Trend {
    fn new(m: &Matrix) -> Self {
        let v = super::feature_matrix::state();
        let ms = u64::from(v.trend_ms);
        Self {
            past: super::feature_matrix_readout::past(m.as_of, ms),
            chart: super::feature_matrix_readout::chart_dz(m.as_of, ms),
            ths: v.ths.clone(),
            label: format!("近 {}", super::feature_matrix_readout::format_window(v.trend_ms)),
        }
    }

    /// (档位, 变化量, 口径说明)。
    fn of(&self, r: &cp::Row, idx: &SlotIndex<'_>, cell: &Cell) -> Option<(i8, f64, &'static str)> {
        if r.is_chart() {
            let d = *self.chart.get(&r.key)?;
            return Some((super::feature_matrix::trend_level(d, &self.ths), d, "σ（按该参数近 10 分钟的波动折算）"));
        }
        let Cell::Value { window_ms, .. } = cell else { return None };
        let s = idx.slot(&r.key, *window_ms)?;
        let (z0, _) = *self.past.as_ref()?.get(s.idx)?;
        let z = s.z?;
        if z0.is_nan() {
            return None;
        }
        let d = z - f64::from(z0);
        Some((super::feature_matrix::trend_level(d, &self.ths), d, "z"))
    }
}

fn arrow(level: i8) -> (String, Color) {
    let n = usize::from(level.unsigned_abs());
    match level.signum() {
        1 => ("▲".repeat(n), C_UP),
        -1 => ("▼".repeat(n), C_DOWN),
        _ => (String::new(), C_DIM),
    }
}

/// 一张卡片：色带标题 → 表头 → 表体（参数 · 值 · 涨跌 · 来源；卡片内滚动）→ 价梯 → 口径行与提示。
#[allow(clippy::too_many_arguments)]
fn card_view<'a>(
    i: usize,
    c: &Card,
    m: &Matrix,
    idx: &SlotIndex<'_>,
    now: Option<f64>,
    tr: &Trend,
    w: f32,
    h: f32,
) -> Element<'a, Msg> {
    let mismatches = cp::mismatches(&c.id, &m.chart.params);
    let mut body = column![].spacing(0);
    let (mut have, mut total) = (0usize, 0usize);
    let mut ladder: Vec<(String, f64)> = Vec::new();
    for (ri, r) in c.rows.iter().enumerate() {
        total += 1;
        let cl = cp::cell(r, idx, &m.chart);
        let (val, color) = match &cl {
            Cell::Value { v, text: t, quality, .. } => {
                have += 1;
                if c.ladder && r.fmt == "price"
                    && let Some(x) = v
                {
                    ladder.push((r.label.clone(), *x));
                }
                (cp::format(&r.fmt, *v, t.as_deref(), m.tick_size), if quality == "GOOD" { C_TXT } else { C_WARN })
            }
            Cell::Pending => ("待实现".to_string(), C_PEND),
            Cell::Disabled => ("未启用".to_string(), C_PEND),
            Cell::Missing(_) => ("—".to_string(), C_DIM),
        };
        let trend = tr.of(r, idx, &cl);
        let (arr, arr_c) = arrow(trend.map_or(0, |t| t.0));
        let (tag, tag_c) = if r.is_chart() { ("图", C_CHART) } else { ("特", C_FEAT) };
        let trend_line = match trend {
            Some((_, d, unit)) => format!("\n涨跌：{} 变化 {d:+.2} {unit}", tr.label),
            None => format!("\n涨跌：{} 的历史还不够，或该参数没有变化幅度", tr.label),
        };
        let detail = if r.is_chart() {
            format!(
                "图表参数 {}\n口径：{}\nSierra：{}\n状态：{}{}{}",
                r.key,
                r.formula,
                r.sierra,
                if r.status == "spec" { "已登记·待实现" } else { &r.status },
                match &cl {
                    Cell::Missing(why) => format!("\n取不到值：{why}"),
                    _ => String::new(),
                },
                trend_line
            )
        } else {
            format!(
                "特征 {}{}\n（直接读特征引擎的值，不重复计算；口径见特征矩阵）{}{}",
                r.key,
                match &cl {
                    Cell::Value { window_ms: Some(w), .. } => format!(" · 窗口 {}", super::feature_matrix_readout::format_window(*w)),
                    _ => String::new(),
                },
                match &cl {
                    Cell::Missing(why) => format!("\n取不到值：{why}"),
                    Cell::Disabled => "\n本部署没启用这条特征（在特征矩阵的启用集里打开）".into(),
                    _ => String::new(),
                },
                trend_line
            )
        };
        let line = container(
            row![
                cell(text(r.label.clone()).size(11).color(C_TXT), Length::Fill, false),
                vline(C_LINE),
                cell(text(val).size(11).color(color), Length::Fixed(cp::col_w(ChartCol::Value)), true),
                vline(C_LINE),
                cell(text(arr).size(10).color(arr_c), Length::Fixed(cp::col_w(ChartCol::Arrow)), false),
                vline(C_LINE),
                cell(text(tag).size(9).color(tag_c), Length::Fixed(cp::col_w(ChartCol::Tag)), false),
                vline(C_LINE),
            ]
            .align_y(Alignment::Center),
        )
        .style(move |_t: &iced::Theme| container::Style {
            // 行间浅色底纹交替，长表横向扫读不串行
            background: (ri % 2 == 1).then_some(iced::Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.025))),
            ..Default::default()
        });
        body = body.push(tip(line, detail));
        body = body.push(hline(C_LINE));
    }

    // 价梯：价位类参数 + 现价，按价格从高到低（同一张表的样式）
    if c.ladder && !ladder.is_empty() {
        if let Some(p) = now {
            ladder.push(("现价".into(), p));
        }
        ladder.sort_by(|a, b| b.1.total_cmp(&a.1));
        body = body.push(container(text("价梯（按价格从高到低，右列为距现价）").size(10).color(C_HEAD)).padding([6, 4]));
        body = body.push(hline(C_HEAD_LINE));
        for (label, p) in &ladder {
            let is_now = label == "现价";
            let dist = match now {
                Some(n) if !is_now && n > 0.0 => format!("{:+.1} bp", (p - n) / n * 1e4),
                _ => String::new(),
            };
            let col = if is_now { C_NOW } else { C_TXT };
            body = body.push(
                row![
                    cell(text(if is_now { "▶ 现价".to_string() } else { label.clone() }).size(10).color(col), Length::Fill, false),
                    vline(C_LINE),
                    cell(text(cp::format("price", Some(*p), None, m.tick_size)).size(10).color(col), Length::Fixed(cp::col_w(ChartCol::Value)), true),
                    vline(C_LINE),
                    cell(text(dist).size(9).color(C_DIM), Length::Fixed(cp::col_w(ChartCol::Arrow) + cp::col_w(ChartCol::Tag) + SEP), true),
                    vline(C_LINE),
                ]
                .align_y(Alignment::Center),
            );
            body = body.push(hline(C_LINE));
        }
    }

    // 口径行与图上设置不一致的提示（文字全部保留在卡片底部）
    let ps: Vec<String> = cp::card_params(&c.id)
        .iter()
        .filter_map(|k| m.chart.params.get(*k).map(|v| format!("{} {}", cp::param_label(k), cp::param_text(k, v))))
        .collect();
    let mut foot = column![text(if ps.is_empty() { String::new() } else { format!("口径：{}", ps.join(" · ")) }).size(9).color(C_DIM)]
        .spacing(2);
    for w in &mismatches {
        foot = foot.push(text(format!("⚠ {w}")).size(9).color(C_WARN));
    }
    body = body.push(container(foot).padding([6, 4]));

    // 色带标题（图名 · 有值数 · 不一致标记）+ 第二行（Sierra Study · 这张图回答的问题）
    let band = container(
        column![
            row![
                text(format!("{} {}", circled(i), c.title)).size(13).color(C_HEAD),
                iced::widget::space::horizontal(),
                text(format!("{have}/{total} 有值")).size(10).color(C_DIM),
                text(if mismatches.is_empty() { String::new() } else { " ⚠".to_string() }).size(11).color(C_WARN),
            ]
            .align_y(Alignment::Center),
            text(format!("{} · {}", c.sierra, c.question)).size(10).color(C_DIM),
        ]
        .spacing(2),
    )
    .padding([4, 6])
    .width(Length::Fill)
    .style(|_t: &iced::Theme| container::Style { background: Some(iced::Background::Color(C_BAND)), ..Default::default() });
    // 表头：分隔线可拖（所有卡片共用一组列宽），双击回到默认
    let head = iced::widget::mouse_area(
        row![
            cell(text("参数").size(10).color(C_HEAD), Length::Fill, false),
            grip(ChartCol::Value, false),
            cell(text("值").size(10).color(C_HEAD), Length::Fixed(cp::col_w(ChartCol::Value)), true),
            grip(ChartCol::Value, true),
            cell(text("涨跌").size(10).color(C_HEAD), Length::Fixed(cp::col_w(ChartCol::Arrow)), false),
            grip(ChartCol::Arrow, true),
            cell(text("源").size(10).color(C_HEAD), Length::Fixed(cp::col_w(ChartCol::Tag)), false),
            grip(ChartCol::Tag, true),
        ]
        .height(Length::Fixed(22.0))
        .align_y(Alignment::Center),
    )
    .on_move(|p| Msg::ChartUi(ChartUiMsg::Move(p.x)))
    .on_release(Msg::ChartUi(ChartUiMsg::DragEnd))
    .on_exit(Msg::ChartUi(ChartUiMsg::DragEnd));

    container(
        column![
            band,
            head,
            hline(C_HEAD_LINE),
            vscroll(body).height(Length::Fill),
        ]
        .spacing(0),
    )
    .width(Length::Fixed(w))
    .height(Length::Fixed(h))
    .style(|_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(C_CARD_BG)),
        border: iced::Border { color: C_BORDER, width: 1.0, radius: 4.0.into() },
        ..Default::default()
    })
    .into()
}

fn btn<'a>(label: &str, msg: Msg, active: bool) -> Element<'a, Msg> {
    button(text(label.to_string()).size(11))
        .padding([2, 8])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

fn edit_msg(m: ChartEditMsg) -> Msg {
    Msg::ChartEdit(m)
}

/// 口径设置（docs/33 批 4）：可编辑项用输入框（枢轴公式用选项），只读项列出原因。
fn editor<'a>(m: &Matrix, d: &cp::Draft) -> Element<'a, Msg> {
    let c = &m.chart;
    let mut b = column![text("口径设置（写配置文件 chart 段，重启特征引擎生效；重启后当日累计从头开始，上一交易日存档保留）").size(11).color(C_HEAD)]
        .spacing(4);
    for e in &c.editable {
        let cur = d.text.get(&e.key).cloned().unwrap_or_default();
        let label = cp::param_label(&e.key);
        let hint = match (e.key.as_str(), e.n) {
            ("bar_period_ms", _) => format!("秒，{}–{}", e.lo / 1_000.0, e.hi / 1_000.0),
            (_, 0) => String::new(),
            (_, 1) => format!("{}–{}", e.lo, e.hi),
            (_, n) => format!("{n} 个数，{}–{}，从小到大，逗号分隔", e.lo, e.hi),
        };
        let input: Element<'a, Msg> = if e.n == 0 {
            let mut r = row![].spacing(4);
            for o in &e.options {
                let key = e.key.clone();
                r = r.push(btn(&cp::param_text(&e.key, &serde_json::json!(o)), edit_msg(ChartEditMsg::Input(key, o.clone())), cur == *o));
            }
            r.into()
        } else {
            let key = e.key.clone();
            text_input("", &cur)
                .on_input(move |t| edit_msg(ChartEditMsg::Input(key.clone(), t)))
                .size(11)
                .width(Length::Fixed(140.0))
                .into()
        };
        b = b.push(
            row![fixed(text(label).size(11).color(C_TXT), W_LABEL, false), input, text(hint).size(10).color(C_DIM)]
                .spacing(8)
                .align_y(Alignment::Center),
        );
    }
    if !c.read_only.is_empty() {
        b = b.push(text("只读").size(10).color(C_DIM));
        for (k, why) in &c.read_only {
            let v = c.params.get(k).map_or_else(String::new, |v| cp::param_text(k, v));
            let label = if cp::param_label(k).is_empty() { k.as_str() } else { cp::param_label(k) };
            b = b.push(
                row![
                    fixed(text(label.to_string()).size(10).color(C_DIM), W_LABEL, false),
                    fixed(text(v).size(10).color(C_DIM), 220.0, false),
                    text(why.clone()).size(10).color(C_PEND),
                ]
                .spacing(8),
            );
        }
    }
    if !d.error.is_empty() {
        b = b.push(text(d.error.clone()).size(11).color(C_WARN));
    }
    b = b.push(
        row![
            btn("应用（写配置并重启引擎）", edit_msg(ChartEditMsg::Apply), false),
            btn("恢复默认", edit_msg(ChartEditMsg::Defaults), false),
            btn("取消", edit_msg(ChartEditMsg::Close), false),
        ]
        .spacing(8),
    );
    container(b)
        .padding(8)
        .width(Length::Fill)
        .style(|_t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(C_CARD_BG)),
            border: iced::Border { color: C_BORDER, width: 1.0, radius: 4.0.into() },
            ..Default::default()
        })
        .into()
}

/// 视图本体（放在特征面板的 scrollable 里）。
pub fn view<'a>(m: &Matrix) -> Element<'a, Msg> {
    let c = &m.chart;
    if !c.present {
        return text("这份快照没有图表参数段：常驻引擎 / 回放程序还是旧版本，重新编译并重启 ws-features（或重新开始回放）")
            .size(11)
            .color(C_WARN)
            .into();
    }
    let idx = SlotIndex::new(m);
    let market = m.market.as_deref();
    let cards: Vec<(usize, &Card)> =
        c.cards.iter().enumerate().filter(|(_, k)| cp::card_visible(k, market)).collect();

    // 总览：特征引用多少有值、图表参数实现了多少
    let (mut f_all, mut f_have, mut c_all, mut c_impl) = (0, 0, 0, 0);
    for (_, k) in &cards {
        for r in &k.rows {
            let v = matches!(cp::cell(r, &idx, c), Cell::Value { .. });
            if r.is_chart() {
                c_all += 1;
                c_impl += usize::from(r.status != "spec");
            } else {
                f_all += 1;
                f_have += usize::from(v);
            }
        }
    }
    let hidden = c.cards.len() - cards.len();
    let mut b = column![
        text(format!(
            "{} 张卡片{} · 引用特征 {f_have}/{f_all} 有值 · 图表参数已实现 {c_impl}/{c_all}{}",
            cards.len(),
            if hidden > 0 { format!("（{hidden} 张不适用于当前市场，已隐藏）") } else { String::new() },
            if c.enabled { "" } else { " · 图表参数层未启用" }
        ))
        .size(11)
        .color(C_DIM),
        row![
            text("角标").size(10).color(C_DIM),
            text("特 = 直接读特征（不重复计算）").size(10).color(C_FEAT),
            text("图 = 图表参数（docs/33）").size(10).color(C_CHART),
            text("悬停一行看口径与对应的 Sierra 输出").size(10).color(C_DIM),
        ]
        .spacing(10),
    ]
    .spacing(6);
    match cp::draft() {
        Some(d) if !c.editable.is_empty() => b = b.push(editor(m, &d)),
        _ if !c.editable.is_empty() => b = b.push(btn("✎ 口径设置", edit_msg(ChartEditMsg::Open), false)),
        _ => {}
    }

    // 卡片网格：按宽度自动定列数（最多 6 列），卡片固定高度、内容在卡片里滚动（docs/33 排版 A 方案）
    let owned: Vec<(usize, Card)> = cards.iter().map(|(i, k)| (*i, (*k).clone())).collect();
    let mm = m.clone();
    let grid = iced::widget::responsive(move |size| {
        let idx = SlotIndex::new(&mm);
        let now = idx.slot("mid_price", Some(0)).and_then(|s| s.value);
        let tr = Trend::new(&mm);
        let n = owned.len().max(1);
        let cols = (((size.width + GAP) / (CARD_MIN_W + GAP)).floor() as usize).clamp(1, 6).min(n);
        let rows = n.div_ceil(cols);
        let w = ((size.width - GAP * (cols as f32 - 1.0)) / cols as f32).floor().max(CARD_MIN_W.min(size.width));
        // 两行排满一屏；超过两行时每张至少 320 高，整体可滚动
        let half = ((size.height - GAP) / 2.0).floor();
        let h = if rows <= 2 { half.max(200.0) } else { half.max(320.0) };
        let mut g = column![].spacing(GAP);
        for chunk in owned.chunks(cols) {
            let mut r = row![].spacing(GAP);
            for (i, k) in chunk {
                r = r.push(card_view(*i, k, &mm, &idx, now, &tr, w, h));
            }
            g = g.push(r);
        }
        if rows <= 2 {
            g.into()
        } else {
            vscroll(g).height(Length::Fill).into()
        }
    });
    b = b.push(container(grid).width(Length::Fill).height(Length::Fill));
    let b = b.height(Length::Fill);
    b.into()
}
