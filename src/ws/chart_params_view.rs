//! 「图表参数」视图的渲染（docs/33 §8）：12 张经典图表卡片，双列网格。
//!
//! 每张卡片：标题（图名 · 对应的 Sierra Study · 这张图回答的问题）→ 参数表（参数 · 值 · 来源角标）
//! → 价梯（价位类参数按价格排在一根竖轴上，现价插在其中）→ 口径行（本卡生效的口径参数）。
//! 只读快照，**不算任何数**。

use iced::widget::{button, column, container, row, text, text_input, tooltip};
use iced::{Alignment, Color, Element, Length};

use super::chart_params::{self as cp, Card, Cell, ChartEditMsg, SlotIndex};
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
const W_VALUE: f32 = 118.0;

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

/// 一张卡片。
fn card_view<'a>(i: usize, c: &Card, m: &Matrix, idx: &SlotIndex<'_>, now: Option<f64>) -> Element<'a, Msg> {
    let mut b = column![
        row![
            text(format!("{} {}", circled(i), c.title)).size(13).color(C_HEAD),
            text(c.sierra.clone()).size(10).color(C_DIM),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(c.question.clone()).size(10).color(C_DIM),
    ]
    .spacing(3);

    let (mut have, mut total) = (0usize, 0usize);
    let mut ladder: Vec<(String, f64)> = Vec::new();
    for r in &c.rows {
        total += 1;
        let cell = cp::cell(r, idx, &m.chart);
        let (val, color) = match &cell {
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
        let (tag, tag_c) = if r.is_chart() { ("图", C_CHART) } else { ("特", C_FEAT) };
        let detail = if r.is_chart() {
            format!(
                "图表参数 {}\n口径：{}\nSierra：{}\n状态：{}{}",
                r.key,
                r.formula,
                r.sierra,
                if r.status == "spec" { "已登记·待实现" } else { &r.status },
                match &cell {
                    Cell::Missing(why) => format!("\n取不到值：{why}"),
                    _ => String::new(),
                }
            )
        } else {
            format!(
                "特征 {}{}\n（直接读特征引擎的值，不重复计算；口径见特征矩阵）{}",
                r.key,
                match &cell {
                    Cell::Value { window_ms: Some(w), .. } => format!(" · 窗口 {}", super::feature_matrix_readout::format_window(*w)),
                    _ => String::new(),
                },
                match &cell {
                    Cell::Missing(why) => format!("\n取不到值：{why}"),
                    Cell::Disabled => "\n本部署没启用这条特征（在特征矩阵的启用集里打开）".into(),
                    _ => String::new(),
                }
            )
        };
        let line = row![
            fixed(text(r.label.clone()).size(11).color(C_TXT), W_LABEL, false),
            fixed(text(val).size(11).color(color), W_VALUE, true),
            text(tag).size(9).color(tag_c),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        b = b.push(tip(line, detail));
    }

    // 价梯：价位类参数 + 现价，按价格从高到低
    if c.ladder && !ladder.is_empty() {
        if let Some(p) = now {
            ladder.push(("现价".into(), p));
        }
        ladder.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut lad = column![text("价梯").size(10).color(C_DIM)].spacing(1);
        for (label, p) in &ladder {
            let is_now = label == "现价";
            let dist = match now {
                Some(n) if !is_now && n > 0.0 => format!("{:+.1} bp", (p - n) / n * 1e4),
                _ => String::new(),
            };
            let c = if is_now { C_NOW } else { C_TXT };
            lad = lad.push(
                row![
                    fixed(text(if is_now { "▶ 现价".to_string() } else { format!("  {label}") }).size(10).color(c), W_LABEL, false),
                    fixed(text(cp::format("price", Some(*p), None, m.tick_size)).size(10).color(c), W_VALUE, true),
                    text(dist).size(9).color(C_DIM),
                ]
                .spacing(6),
            );
        }
        b = b.push(container(lad).padding([4, 0]));
    }

    // 口径行
    let ps: Vec<String> = cp::card_params(&c.id)
        .iter()
        .filter_map(|k| m.chart.params.get(*k).map(|v| format!("{} {}", cp::param_label(k), cp::param_text(k, v))))
        .collect();
    let foot = format!(
        "{have}/{total} 有值{}",
        if ps.is_empty() { String::new() } else { format!(" · 口径：{}", ps.join(" · ")) }
    );
    b = b.push(text(foot).size(9).color(C_DIM));
    for w in cp::mismatches(&c.id, &m.chart.params) {
        b = b.push(text(format!("⚠ {w}")).size(9).color(C_WARN));
    }

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
    let now = idx.slot("mid_price", Some(0)).and_then(|s| s.value);
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

    // 双列网格
    for pair in cards.chunks(2) {
        let mut r = row![].spacing(8);
        for (i, k) in pair {
            r = r.push(container(card_view(*i, k, m, &idx, now)).width(Length::FillPortion(1)));
        }
        if pair.len() == 1 {
            r = r.push(container(text("")).width(Length::FillPortion(1)));
        }
        b = b.push(r);
    }
    b.into()
}
