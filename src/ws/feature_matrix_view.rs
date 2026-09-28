//! 特征矩阵面板 — 视图（docs/31 §8.1）。
//!
//! 三个视图：① 特征矩阵（七阶段纵向分节）② 实时向量（按阶段折叠）③ 引擎健康。
//! 「研究纪律」在**另一个面板**（特征库 / docs/30）——§8.1 要求它与 ①② 分开。
//!
//! ## 这一页有两条刻意的排版规则
//!
//! **1. 顶栏先说「多少个 slot 现在可下单」，再说别的。**
//! 109 个 slot 里 93 个 GOOD 和 37 个 GOOD 是完全不同的两种处境，
//! 而这件事从逐行的表里看不出来——人会盯住第一行。
//!
//! **2. 待实现的行照样显示，标成「已登记·待实现」。**
//! 藏起来会让人以为矩阵里的就是全部（docs/31 §8.1 明确要求显示）。
//! 但它们**不进「仅异常」筛选**：待实现不是故障。
//!
//! ## 面板不算任何值
//!
//! 值、z、分位、质量全部来自引擎的旁路 JSON。面板连「值缺了就填 0」都不做——
//! 缺就显示「—」。这是 W7 验收（面板与引擎对同一时刻的值一致）唯一能立住的做法：
//! 只要面板里有第二条计算路径，两边就会漂。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::feature_matrix::{
    FeatureMatrixMsg as Msg, Metric, QualityFilter, StatusFilter, TableMode, View, ViewState,
};
use super::feature_matrix_readout::{self as ro, FeatureRow, Matrix, Slot};

const C_HEAD: Color = Color::from_rgb(0.70, 0.80, 0.95);
const C_DIM: Color = Color::from_rgb(0.50, 0.54, 0.60);
const C_TXT: Color = Color::from_rgb(0.84, 0.87, 0.92);
const C_OK: Color = Color::from_rgb(0.30, 0.80, 0.48);
const C_BAD: Color = Color::from_rgb(0.90, 0.38, 0.38);
const C_WARN: Color = Color::from_rgb(0.90, 0.72, 0.32);
/// 待实现：比 `C_DIM` 更暗。它既不是好也不是坏，是「还没做」。
const C_PEND: Color = Color::from_rgb(0.42, 0.44, 0.50);

fn cell<'a>(t: String, w: f32, c: Color) -> Element<'a, Msg> {
    container(text(t).size(11).color(c)).width(Length::Fixed(w)).into()
}

/// 数值单元右对齐——右对齐后小数点纵向成列，一眼能比大小。
fn numc<'a>(t: String, w: f32, c: Color) -> Element<'a, Msg> {
    container(text(t).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(iced::Alignment::End)
        .into()
}

fn dim<'a>(t: String) -> Element<'a, Msg> {
    text(t).size(10).color(C_DIM).into()
}

fn sec<'a>(t: String) -> Element<'a, Msg> {
    text(t).size(13).color(C_HEAD).into()
}

fn chip<'a>(label: String, active: bool, msg: Msg) -> Element<'a, Msg> {
    button(text(label).size(11))
        .padding([2, 7])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

/// 数值格式化。
///
/// **`None` 必须显示成「—」而不是 0。** 这是整个面板最容易出错的一处：
/// 一个「不可用」的 slot 显示成 `0.0000` 看起来完全正常，而它其实什么都不知道。
fn num(v: Option<f64>) -> String {
    match v {
        None => "—".into(),
        // `x == 0.0` 而不是 `x.abs() < eps`：0 与 1e-30 是两个不同的数，
        // 后者要走科学计数法那一支。
        Some(0.0) => "0".into(),
        Some(x) if x.is_infinite() => "∞".into(),
        Some(x) if x.abs() >= 1e6 || x.abs() < 1e-4 => format!("{x:.3e}"),
        Some(x) if x.abs() >= 1e3 => format!("{x:.2}"),
        Some(x) => format!("{x:.4}"),
    }
}

/// 质量的颜色。五态各一色——`DEGRADED` 与 `UNAVAILABLE` 撞色就失去意义：
/// 前者是「等一会儿就有」，后者是「这个市场永远没有」。
fn qcolor(q: &str) -> Color {
    match q {
        "GOOD" => C_OK,
        "DEGRADED" => C_WARN,
        "STALE" => Color::from_rgb(0.80, 0.55, 0.30),
        "INVALID" => C_BAD,
        "UNAVAILABLE" => C_PEND,
        _ => C_TXT,
    }
}

/// 数据层徽标的颜色。
///
/// 串必须与引擎的 `DataLayer::as_str()` **逐字一致**。第一版我按直觉写成了
/// `trades` / `derivative` / `own_orders`（小写蛇形），而引擎写的是
/// `Trades` / `Derivative` / `OwnOrders`——后果是真实快照里 57 个 slot 的徽标
/// 全落到兜底的 `C_DIM` 上，面板看起来完全正常，只是那一列不再区分数据层。
/// 主仓 `tests/panel_contract.rs` 把这份列表与引擎词表对拍，
/// 本文件的 `数据层徽标覆盖引擎的全部词表` 从另一侧守同一件事。
fn layer_color(l: &str) -> Color {
    match l {
        "BBO" => Color::from_rgb(0.55, 0.75, 0.95),
        "Trades" => Color::from_rgb(0.40, 0.85, 0.60),
        "L2" => Color::from_rgb(0.85, 0.72, 0.95),
        "L3" => Color::from_rgb(0.95, 0.60, 0.70),
        "Derivative" => Color::from_rgb(0.95, 0.70, 0.45),
        "OwnOrders" => Color::from_rgb(0.60, 0.90, 0.90),
        "Derived" => Color::from_rgb(0.72, 0.74, 0.80),
        _ => C_DIM,
    }
}

/// 引擎 `DataLayer` 词表的全部取值（`registry.rs` 的 `as_str_impl!(DataLayer, …)`）。
const ENGINE_LAYERS: [&str; 7] = [
    "BBO",
    "Trades",
    "L2",
    "L3",
    "Derivative",
    "OwnOrders",
    "Derived",
];

fn layer_badges<'a>(s: &Slot) -> Element<'a, Msg> {
    let mut r = row![].spacing(3);
    for l in &s.inputs {
        r = r.push(text(l.clone()).size(9).color(layer_color(l)));
    }
    r.into()
}

// ── 表格 ─────────────────────────────────────────────────────────────────
//
// 一行一个特征、各时间窗口横向成组。让它可读的三件事：
// 1. 窗口组交替底色 + 组间竖线：一眼分清「这几列是 1s 的、那几列是 5m 的」；
// 2. 行斑马纹 + 悬停整行高亮：长行不看串；
// 3. 表头在滚动区之外（固定在顶上）。为此所有阶段共用同一套窗口列（可见行的窗口并集）——
//    各阶段各自出列的话，固定的表头就对不上下面的行。
//
// 质量不占文字列：每个窗口组左边一条色条（绿良好 / 黄降级 / 橙过期 / 红无效 / 灰不可用），原因在悬停提示里。
// 「透视」模式每个窗口只一列、只显示一个指标，z 与分位按偏离着色成热力图。

const P_NAME: u16 = 5;
const P_KEY: u16 = 4;
const P_VAL: u16 = 3;
const P_Z: u16 = 2;
const P_PCT: u16 = 2;
/// 透视模式每个窗口一格。
const P_CELL: u16 = 3;
const P_UNIT: u16 = 2;
const P_LAYER: u16 = 3;
const P_STAT: u16 = 4;
/// 质量色条宽度。
const BAR: f32 = 3.0;
/// 行高（固定：各格子的底色要铺满整行）。
const ROW_H: f32 = 22.0;

fn pc<'a>(e: impl Into<Element<'a, Msg>>, portion: u16) -> Element<'a, Msg> {
    container(e).width(Length::FillPortion(portion)).into()
}

/// 右对齐的比例列（数值列：小数点纵向成列）。
fn pr<'a>(e: impl Into<Element<'a, Msg>>, portion: u16) -> Element<'a, Msg> {
    container(e)
        .width(Length::FillPortion(portion))
        .align_x(iced::Alignment::End)
        .into()
}

/// 带底色的容器（`None` = 透明）。
fn tinted<'a>(e: impl Into<Element<'a, Msg>>, c: Option<Color>) -> container::Container<'a, Msg> {
    container(e).style(move |_t: &iced::Theme| container::Style {
        background: c.map(iced::Background::Color),
        ..Default::default()
    })
}

/// 窗口组之间的竖线。
fn vline<'a>() -> Element<'a, Msg> {
    iced::widget::rule::vertical(1.0).style(crate::style::split_ruler).into()
}

/// 窗口组的交替底色。
fn group_bg(i: usize) -> Color {
    if i.is_multiple_of(2) {
        Color::from_rgba(0.55, 0.65, 0.85, 0.05)
    } else {
        Color::from_rgba(0.55, 0.65, 0.85, 0.12)
    }
}

/// 窗口的列标题。`0` 是瞬时量，不是「零秒窗口」。
fn window_label(w: u32) -> String {
    ro::format_window(w)
}

/// 原因的简短中文（悬停提示与透视「质量」格里用，原文附在提示里）。
fn reason_cn(r: &str) -> &str {
    match r {
        "disabled" => "未启用",
        "capability_missing" => "缺数据",
        "insufficient_samples" => "样本不足",
        "not_applicable" => "不适用",
        "undefined" => "无定义",
        "one_sided_book" => "单边簿",
        "offline_only" => "仅离线",
        "not_implemented" => "待实现",
        "book_desync" => "簿失同步",
        "crossed_book" => "交叉簿",
        "silent_too_long" => "静默过久",
        "window_capacity_exceeded" => "窗口溢出",
        "conflated_l2" => "合并推送",
        "l2_proxy" => "L2 近似",
        "session_unknown" => "时段未知",
        "no_continuous_matching" => "非连续撮合",
        "non_monotonic_clock" => "时钟回退",
        other => other,
    }
}

fn pct_text(p: Option<f64>) -> String {
    p.map_or_else(|| "—".into(), |p| format!("{:.0}%", p * 100.0))
}

/// 一个 slot 的完整说明（悬停提示）。
fn slot_tip(s: &Slot) -> String {
    format!(
        "{} · {}\n质量 {}{}\n值 {}　z {}　分位 {}",
        s.key,
        s.window_label(),
        s.quality,
        if s.reason.is_empty() {
            String::new()
        } else {
            format!(" · {}（{}）", reason_cn(&s.reason), s.reason)
        },
        num(s.value),
        num(s.z),
        pct_text(s.percentile),
    )
}

fn with_tip<'a>(e: impl Into<Element<'a, Msg>>, tip: String) -> Element<'a, Msg> {
    iced::widget::tooltip(
        e,
        container(text(tip).size(11)).style(crate::style::tooltip).padding(6),
        iced::widget::tooltip::Position::Top,
    )
    .into()
}

/// 偏离着色：`x` ∈ [−1, 1]，负蓝正红，越深偏离越大；接近 0 不着色。
fn heat(x: f64) -> Option<Color> {
    let x = x.clamp(-1.0, 1.0);
    if x.abs() < 0.05 {
        return None;
    }
    let a = (0.10 + 0.50 * x.abs()) as f32;
    Some(if x > 0.0 {
        Color::from_rgba(0.92, 0.32, 0.30, a)
    } else {
        Color::from_rgba(0.30, 0.55, 0.95, a)
    })
}

/// 质量色条的颜色（没启用 / 待实现的用最暗的灰）。
fn bar_color(s: &Slot) -> Color {
    if s.not_implemented() || s.disabled() {
        C_PEND.scale_alpha(0.5)
    } else {
        qcolor(&s.quality)
    }
}

/// 值的颜色：良好用正文色，其余按质量；没启用 / 待实现的压暗。
fn value_color(s: &Slot) -> Color {
    if s.not_implemented() || s.disabled() {
        C_PEND
    } else if s.quality == "GOOD" {
        C_TXT
    } else {
        qcolor(&s.quality)
    }
}

const fn group_portion(mode: TableMode) -> u16 {
    match mode {
        TableMode::Full => P_VAL + P_Z + P_PCT,
        TableMode::Pivot => P_CELL,
    }
}

/// 一个窗口组。这个特征没有该窗口时留空（「—」表示有这个窗口但没有值）。
fn window_group<'a>(s: Option<&Slot>, mode: TableMode, metric: Metric, gi: usize) -> Element<'a, Msg> {
    let portion = group_portion(mode);
    let Some(s) = s else {
        return tinted(text(""), Some(group_bg(gi)))
            .width(Length::FillPortion(portion))
            .height(Length::Fill)
            .into();
    };
    let tip = slot_tip(s);
    let body: Element<'a, Msg> = match mode {
        TableMode::Full => {
            let bar = tinted(text(""), Some(bar_color(s)))
                .width(Length::Fixed(BAR))
                .height(Length::Fill);
            tinted(
                row![
                    bar,
                    pr(text(num(s.value)).size(11).color(value_color(s)), P_VAL),
                    pr(text(num(s.z)).size(11).color(C_DIM), P_Z),
                    pr(text(pct_text(s.percentile)).size(11).color(C_DIM), P_PCT),
                ]
                .spacing(4)
                .padding([0, 4])
                .align_y(iced::Alignment::Center)
                .height(Length::Fill),
                Some(group_bg(gi)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
        TableMode::Pivot => {
            let (t, bg, c) = match metric {
                Metric::Z => (
                    s.z.map_or_else(|| "—".into(), |z| format!("{z:+.2}")),
                    s.z.and_then(|z| heat(z / 3.0)),
                    C_TXT,
                ),
                Metric::Pct => (
                    pct_text(s.percentile),
                    s.percentile.and_then(|p| heat((p - 0.5) * 2.0)),
                    C_TXT,
                ),
                Metric::Value => (num(s.value), None, value_color(s)),
                Metric::Quality => (
                    if s.not_implemented() {
                        "待实现".to_string()
                    } else if s.reason.is_empty() {
                        "良好".to_string()
                    } else {
                        reason_cn(&s.reason).to_string()
                    },
                    Some(bar_color(s).scale_alpha(0.28)),
                    C_TXT,
                ),
            };
            let muted = s.not_implemented() || s.disabled();
            tinted(
                container(text(t).size(11).color(if muted { C_PEND } else { c }))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(iced::Alignment::End)
                    .align_y(iced::Alignment::Center)
                    .padding([0, 6]),
                Some(bg.unwrap_or_else(|| group_bg(gi))),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    };
    container(with_tip(body, tip))
        .width(Length::FillPortion(portion))
        .height(Length::Fill)
        .into()
}

/// 一条特征各窗口的质量汇总（状态列）：全部良好只写 GOOD，否则写最要紧的那个问题。
fn row_status(r: &FeatureRow<'_>) -> (String, Color) {
    let h = r.head();
    if h.not_implemented() {
        return ("已登记·待实现".into(), C_PEND);
    }
    if !r.enabled() {
        return ("未启用".into(), C_PEND);
    }
    let n = r.slots.len();
    let good = r.slots.iter().filter(|s| s.quality == "GOOD").count();
    if good == n {
        return ("GOOD".into(), C_OK);
    }
    // 最要紧的：INVALID > STALE > DEGRADED > UNAVAILABLE
    let rank = |q: &str| match q {
        "INVALID" => 0,
        "STALE" => 1,
        "DEGRADED" => 2,
        "UNAVAILABLE" => 3,
        _ => 4,
    };
    let worst = r
        .slots
        .iter()
        .filter(|s| s.quality != "GOOD")
        .min_by_key(|s| rank(&s.quality))
        .expect("good < n");
    let what = if worst.reason.is_empty() {
        worst.quality.clone()
    } else {
        format!("{} · {}", worst.quality, reason_cn(&worst.reason))
    };
    let t = if good > 0 { format!("{good}/{n} 窗口良好　{what}") } else { what };
    (t, qcolor(&worst.quality))
}

/// 固定表头。完整模式两层（上层窗口标签横跨一组，下层 值 / z / 分位）；透视模式一层。
fn table_header<'a>(wins: &[u32], mode: TableMode, metric: Metric) -> Element<'a, Msg> {
    let h = |t: &str| text(t.to_string()).size(10).color(C_DIM);
    let portion = group_portion(mode);
    let mut top = row![pc(h("特征"), P_NAME), pc(h("键"), P_KEY)]
        .spacing(0)
        .height(Length::Fixed(18.0));
    for (gi, w) in wins.iter().enumerate() {
        let label = match mode {
            TableMode::Full => format!("窗口 {}", window_label(*w)),
            TableMode::Pivot => format!("{} · {}", window_label(*w), metric.label()),
        };
        top = top.push(vline()).push(
            tinted(
                container(text(label).size(10).color(C_HEAD))
                    .width(Length::Fill)
                    .align_x(iced::Alignment::Center),
                Some(group_bg(gi)),
            )
            .width(Length::FillPortion(portion))
            .height(Length::Fill),
        );
    }
    top = top
        .push(vline())
        .push(pc(h("单位"), P_UNIT))
        .push(pc(h("数据层"), P_LAYER))
        .push(pc(h("状态"), P_STAT));
    let mut b = column![top];
    if mode == TableMode::Full {
        let mut bottom = row![pc(text(""), P_NAME), pc(text(""), P_KEY)]
            .spacing(0)
            .height(Length::Fixed(16.0));
        for (gi, _) in wins.iter().enumerate() {
            bottom = bottom.push(vline()).push(
                tinted(
                    row![
                        container(text("")).width(Length::Fixed(BAR)),
                        pr(h("值"), P_VAL),
                        pr(h("z"), P_Z),
                        pr(h("分位"), P_PCT),
                    ]
                    .spacing(4)
                    .padding([0, 4]),
                    Some(group_bg(gi)),
                )
                .width(Length::FillPortion(portion))
                .height(Length::Fill),
            );
        }
        bottom = bottom
            .push(vline())
            .push(pc(text(""), P_UNIT))
            .push(pc(text(""), P_LAYER))
            .push(pc(text(""), P_STAT));
        b = b.push(bottom);
    }
    tinted(b.padding([2, 6]), Some(Color::from_rgba(0.55, 0.65, 0.85, 0.10)))
        .width(Length::Fill)
        .into()
}

/// 一行。斑马纹 + 悬停整行高亮；鼠标进出发消息（面板据此记下高亮的是哪一行）。
fn feature_line<'a>(
    r: &FeatureRow<'_>,
    wins: &[u32],
    mode: TableMode,
    metric: Metric,
    idx: usize,
    hovered: bool,
) -> Element<'a, Msg> {
    let h = r.head();
    let muted = h.not_implemented() || !r.enabled();
    let mut line = row![
        container(text(h.name_cn.clone()).size(11).color(if muted { C_PEND } else { C_TXT }))
            .width(Length::FillPortion(P_NAME))
            .align_y(iced::Alignment::Center)
            .height(Length::Fill),
        container(text(h.key.clone()).size(10).color(C_DIM))
            .width(Length::FillPortion(P_KEY))
            .align_y(iced::Alignment::Center)
            .height(Length::Fill),
    ]
    .spacing(0)
    .height(Length::Fixed(ROW_H));
    for (gi, w) in wins.iter().enumerate() {
        line = line.push(vline()).push(window_group(r.window(*w), mode, metric, gi));
    }
    let (st, sc) = row_status(r);
    line = line
        .push(vline())
        .push(
            container(text(h.unit.clone()).size(10).color(C_DIM))
                .width(Length::FillPortion(P_UNIT))
                .padding([0, 4])
                .align_y(iced::Alignment::Center)
                .height(Length::Fill),
        )
        .push(
            container(layer_badges(h))
                .width(Length::FillPortion(P_LAYER))
                .align_y(iced::Alignment::Center)
                .height(Length::Fill),
        )
        .push(
            container(text(st).size(10).color(sc))
                .width(Length::FillPortion(P_STAT))
                .align_y(iced::Alignment::Center)
                .height(Length::Fill),
        );
    let bg = if hovered {
        Some(Color::from_rgba(0.55, 0.75, 1.0, 0.16))
    } else if !idx.is_multiple_of(2) {
        Some(Color::from_rgba(1.0, 1.0, 1.0, 0.035))
    } else {
        None
    };
    let key = h.key.clone();
    iced::widget::mouse_area(tinted(line.padding([0, 6]), bg).width(Length::Fill))
        .on_enter(Msg::HoverIn(key.clone()))
        .on_exit(Msg::HoverOut(key))
        .into()
}

/// 阶段分节条（横贯整行）。
fn stage_band<'a>(t: String, c: Color) -> container::Container<'a, Msg> {
    tinted(text(t).size(12).color(c), Some(Color::from_rgba(0.55, 0.65, 0.85, 0.16)))
        .width(Length::Fill)
        .padding([3, 6])
}

/// 表格的窗口列：可见行（全部阶段）的窗口并集，升序。固定表头与所有行共用这一套。
fn table_windows(m: &Matrix, v: &ViewState) -> Vec<u32> {
    let mut w: Vec<u32> = Matrix::STAGES
        .iter()
        .flat_map(|(k, _)| visible(m, v, k))
        .flat_map(|r| r.slots.iter().map(|s| s.window_ms).collect::<Vec<_>>())
        .collect();
    w.sort_unstable();
    w.dedup();
    w
}

/// 通过当前筛选的特征：任一窗口通过筛选即显示整行；未启用的按开关藏起来。
fn visible<'m>(m: &'m Matrix, v: &ViewState, stage: &str) -> Vec<FeatureRow<'m>> {
    m.features_in(stage)
        .into_iter()
        .filter(|r| (v.show_disabled || r.enabled()) && r.slots.iter().any(|s| v.passes(s)))
        .collect()
}

/// 表格上方一行：完整 / 透视、透视的指标、图例。
fn table_controls<'a>(v: &ViewState) -> Element<'a, Msg> {
    let mut r = row![text("展示 ").size(11).color(C_DIM)]
        .spacing(4)
        .align_y(iced::Alignment::Center);
    for t in TableMode::ALL {
        r = r.push(chip(t.label().into(), v.table == t, Msg::SetTable(t)));
    }
    if v.table == TableMode::Pivot {
        r = r.push(text("　指标 ").size(11).color(C_DIM));
        for mt in Metric::ALL {
            r = r.push(chip(mt.label().into(), v.metric == mt, Msg::SetMetric(mt)));
        }
    }
    r = r.push(text("　").size(11));
    let legend: Element<'a, Msg> = match (v.table, v.metric) {
        (TableMode::Full, _) | (TableMode::Pivot, Metric::Value) => {
            let mut l = row![text("色条 = 质量：").size(10).color(C_DIM)].spacing(4);
            for (q, name) in [
                ("GOOD", "良好"),
                ("DEGRADED", "降级"),
                ("STALE", "过期"),
                ("INVALID", "无效"),
                ("UNAVAILABLE", "不可用"),
            ] {
                l = l.push(text(format!("■{name}")).size(10).color(qcolor(q)));
            }
            l.push(text("　悬停看原因").size(10).color(C_DIM)).into()
        }
        (TableMode::Pivot, Metric::Z) => dim("蓝 = 低于常态，红 = 高于常态，颜色越深偏离越大（|z| ≥ 3 封顶）".into()),
        (TableMode::Pivot, Metric::Pct) => dim("蓝 = 处在历史低位，红 = 处在历史高位，50% 附近不着色".into()),
        (TableMode::Pivot, Metric::Quality) => dim("格子按质量着色，写的是原因；悬停看原文".into()),
    };
    r.push(legend).into()
}

/// 筛选条。四个维度 + 清除（docs/31 §8.1：可按阶段/类别/状态/市场筛）。
fn filter_bar<'a>(m: &Matrix, v: &ViewState) -> Element<'a, Msg> {
    let mut b = column![].spacing(3);

    let mut r1 = row![text("质量 ").size(11).color(C_DIM)].spacing(4);
    for q in QualityFilter::ALL {
        r1 = r1.push(chip(q.label().into(), v.quality == q, Msg::SetQuality(q)));
    }
    r1 = r1.push(text("　进度 ").size(11).color(C_DIM));
    for s in StatusFilter::ALL {
        r1 = r1.push(chip(s.label().into(), v.status == s, Msg::SetStatus(s)));
    }
    let hidden = m.features().iter().filter(|r| !r.enabled()).count();
    if hidden > 0 {
        r1 = r1.push(text("　").size(11));
        r1 = r1.push(chip(
            format!("显示未启用（{hidden}）"),
            v.show_disabled,
            Msg::ToggleShowDisabled,
        ));
    }
    if v.any_filter() {
        r1 = r1.push(text("　").size(11));
        r1 = r1.push(chip("✕ 清筛选".into(), false, Msg::ClearFilters));
    }
    b = b.push(r1.align_y(iced::Alignment::Center));

    let mut r2 = row![text("阶段 ").size(11).color(C_DIM)].spacing(4);
    r2 = r2.push(chip("全部".into(), v.stage.is_none(), Msg::SetStage(None)));
    for (k, label) in Matrix::STAGES {
        r2 = r2.push(chip(
            label.into(),
            v.stage == Some(k),
            Msg::SetStage(Some(k)),
        ));
    }
    b = b.push(r2.align_y(iced::Alignment::Center));

    // 市场筛选的取值**来自快照**而不是写死：加一个市场 profile 就该自动出现。
    let mut r3 = row![text("市场 ").size(11).color(C_DIM)].spacing(4);
    r3 = r3.push(chip("全部".into(), v.market.is_none(), Msg::SetMarket(None)));
    for mk in m.markets() {
        r3 = r3.push(chip(
            mk.clone(),
            v.market.as_deref() == Some(mk.as_str()),
            Msg::SetMarket(Some(mk)),
        ));
    }
    b = b.push(r3.align_y(iced::Alignment::Center));

    // 族（类别）有三十来个，横排会溢出——所以只在选了阶段之后列该阶段的族。
    // 一次给三十个按钮等于没给筛选。
    if let Some(st) = v.stage {
        let mut fams: Vec<String> = m
            .by_stage(st)
            .iter()
            .map(|s| s.family.clone())
            .collect();
        fams.sort();
        fams.dedup();
        let mut r4 = row![text("类别 ").size(11).color(C_DIM)].spacing(4);
        r4 = r4.push(chip("全部".into(), v.family.is_none(), Msg::SetFamily(None)));
        for f in fams {
            r4 = r4.push(chip(
                f.clone(),
                v.family.as_deref() == Some(f.as_str()),
                Msg::SetFamily(Some(f)),
            ));
        }
        b = b.push(r4.align_y(iced::Alignment::Center));
    } else {
        b = b.push(dim("类别筛选在选定阶段后出现——三十多个族横排等于没有筛选".into()));
    }
    b.into()
}

/// 顶栏：先说「现在有多少个 slot 能下单」。
fn top_bar<'a>(m: &Matrix) -> Element<'a, Msg> {
    let usable = m.usable_slots;
    let total = m.total_slots.max(1);
    let frac = usable as f64 / total as f64;
    let c = if frac >= 0.8 {
        C_OK
    } else if frac >= 0.5 {
        C_WARN
    } else {
        C_BAD
    };
    let mut b = column![].spacing(3);
    b = b.push(row![
        text(if frac >= 0.8 { "● " } else { "○ " }).size(15).color(c),
        text(format!(
            "{usable}/{} 个 slot 处于可下单质量（{:.0}%）",
            m.total_slots,
            frac * 100.0
        ))
        .size(12)
        .color(c),
        text(format!(
            "　已登记·待实现 {}　市场状态 {}",
            m.not_implemented(),
            m.regime
        ))
        .size(11)
        .color(C_DIM),
    ]);
    // 质量分布：一行里写清「降级的是多数还是少数」。
    let mut qr = row![text("质量分布 ").size(10).color(C_DIM)].spacing(6);
    for (q, n) in m.quality_counts() {
        qr = qr.push(text(format!("{q}×{n}")).size(10).color(qcolor(&q)));
    }
    b = b.push(qr);
    b = b.push(dim(format!(
        "标的 {}　事件 {}　时段 {}{}　簿 {} {:?}　快照时刻 {}　读于 {}",
        m.symbol_id,
        m.event_count,
        m.session_kind,
        m.session_bucket.map_or(String::new(), |x| format!("·桶{x}")),
        m.book_sync,
        m.book_depth,
        m.as_of,
        m.refreshed
    )));
    b.into()
}

/// ① 特征矩阵（表体）：七阶段纵向分节，一行一个特征，窗口横向成组。表头在滚动区外。
fn matrix_view<'a>(m: &Matrix, v: &ViewState, wins: &[u32]) -> Element<'a, Msg> {
    let mut b = column![].spacing(0).width(Length::Fill);
    let mut shown = 0usize;
    for (key, label) in Matrix::STAGES {
        let rows = visible(m, v, key);
        if rows.is_empty() {
            continue;
        }
        let good = rows
            .iter()
            .filter(|r| r.slots.iter().all(|s| s.quality == "GOOD"))
            .count();
        b = b.push(stage_band(
            format!("{label} · {} 条特征（全部窗口良好 {good}）", rows.len()),
            C_HEAD,
        ));
        for (i, r) in rows.iter().enumerate() {
            shown += 1;
            let hov = v.hover.as_deref() == Some(r.head().key.as_str());
            b = b.push(feature_line(r, wins, v.table, v.metric, i, hov));
        }
    }
    let total = m.features().len();
    if shown == 0 {
        b = b.push(
            text("当前筛选下没有任何特征——点「✕ 清筛选」或「显示未启用」")
                .size(11)
                .color(C_WARN),
        );
    } else if shown < total {
        // 筛过之后忘了筛，会把「矩阵里只有 3 条」当成引擎的问题。
        b = b.push(container(dim(format!(
            "当前显示 {shown} 条特征，共 {total} 条（其余被筛选或未启用）"
        ))).padding([6, 6]));
    }
    b.into()
}

/// ② 实时向量（表体）：按阶段折叠，异常高亮。
///
/// 与 ① 的区别不是排版而是**用途**：① 是「这个引擎有哪些特征、各自什么状态」，
/// ② 是「此刻这个向量长什么样」。列与 ① 相同（参数一个不少），阶段可折叠。
fn vector_view<'a>(m: &Matrix, v: &ViewState, wins: &[u32]) -> Element<'a, Msg> {
    let mut b = column![].spacing(0).width(Length::Fill);
    for (key, label) in Matrix::STAGES {
        let rows = visible(m, v, key);
        if rows.is_empty() {
            continue;
        }
        let bad = rows
            .iter()
            .filter(|r| r.enabled() && r.slots.iter().any(|s| s.abnormal() && !s.not_implemented()))
            .count();
        let collapsed = v.is_collapsed(key);
        let head = format!(
            "{} {label} · {} 条特征{}",
            if collapsed { "▸" } else { "▾" },
            rows.len(),
            if bad > 0 { format!("　有异常窗口 {bad}") } else { String::new() },
        );
        b = b.push(
            button(stage_band(head, if bad > 0 { C_WARN } else { C_HEAD }))
                .padding(0)
                .width(Length::Fill)
                .style(|t, st| crate::style::button::modifier(t, st, false))
                .on_press(Msg::ToggleStage(key.to_string())),
        );
        if collapsed {
            continue;
        }
        for (i, r) in rows.iter().enumerate() {
            let hov = v.hover.as_deref() == Some(r.head().key.as_str());
            b = b.push(feature_line(r, wins, v.table, v.metric, i, hov));
        }
    }
    b.into()
}

/// ③ 引擎健康：簿、检出、缓冲容量、刷新率。
fn engine_view<'a>(m: &Matrix) -> Element<'a, Msg> {
    let mut b = column![].spacing(4);

    b = b.push(sec("簿重建".into()));
    b = b.push(dim(format!(
        "同步 {}　档数 {:?}　失同步 {} 次　交叉消除 {} 档　深度裁剪 {} 档",
        m.book_sync,
        m.book_depth,
        m.book_desync_count,
        m.book_uncrossed_levels,
        m.book_truncated_levels
    )));
    b = b.push(dim(
        "交叉消除与深度裁剪「看发生率不看有没有」：任何真实的增量簿都会有少量交叉，\
         归零反而说明规范化层没在工作。"
            .into(),
    ));

    b = b.push(sec("检出与状态".into()));
    b = b.push(dim(format!(
        "扫单组 {}　冰山循环 {}　市场状态 {}　VPIN 桶 {}　markout 待结算 {}　占位成交 {}",
        m.sweep_groups,
        m.iceberg_refills,
        m.regime,
        m.vpin_buckets,
        m.markout_pending,
        m.placeholder_count
    )));

    b = b.push(sec("共享窗口缓冲".into()));
    b = b.push(dim(format!(
        "{} 条缓冲承载 {} 个 slot；每事件平均刷 {:.1} 个 slot",
        m.pool_windows, m.total_slots, m.refreshes_per_event
    )));
    let dc = if m.saturated_windows > 0 { C_WARN } else { C_DIM };
    b = b.push(
        text(format!(
            "累计拒收样本 {}（{:.2}/事件）　当前饱和 {} 条",
            m.window_drops,
            if m.event_count > 0 {
                m.window_drops as f64 / m.event_count as f64
            } else {
                0.0
            },
            m.saturated_windows
        ))
        .size(11)
        .color(dc),
    );
    b = b.push(dim(
        "「当前饱和」是「此刻」窗口里缺样本的条数，不是「曾经缺过」。\
         累计拒收看增长率：长窗口被容量上限截断是 16 MB 预算内的自觉取舍，\
         短窗口持续拒收才是容量定错了。"
            .into(),
    ));
    let mut rows: Vec<&ro::PoolWindow> = m.pool_detail.iter().filter(|w| w.drops > 0).collect();
    rows.sort_by_key(|w| std::cmp::Reverse(w.drops));
    if rows.is_empty() {
        b = b.push(text("没有任何缓冲拒收过样本").size(11).color(C_OK));
    } else {
        b = b.push(row![
            cell("可观测量".into(), 150.0, C_DIM),
            cell("窗口".into(), 60.0, C_DIM),
            numc("占用".into(), 130.0, C_DIM),
            numc("累计拒收".into(), 100.0, C_DIM),
            cell("".into(), 70.0, C_DIM),
        ].spacing(4));
        for w in rows.iter().take(12) {
            b = b.push(row![
                cell(w.observable.clone(), 150.0, C_TXT),
                cell(
                    if w.window_ms == 0 { "瞬时".into() } else { format!("{}s", w.window_ms / 1000) },
                    60.0,
                    C_DIM
                ),
                numc(format!("{}/{}", w.len, w.capacity), 130.0, C_DIM),
                numc(w.drops.to_string(), 100.0, if w.saturated { C_WARN } else { C_DIM }),
                cell(
                    if w.saturated { "⚠ 当前饱和".into() } else { String::new() },
                    70.0,
                    C_WARN
                ),
            ].spacing(4));
        }
    }

    b = b.push(sec("研究纪律".into()));
    b = b.push(dim(
        "样本量、多重比较与 FDR 不在这个面板上——它们在「特征库」面板（docs/30）。\
         分开是刻意的：这一页回答「引擎此刻算出了什么」，那一页回答\
         「这些值里有没有一个真的预测得动」。把两者放在一起，\
         人会把「93 个 slot 质量良好」读成「93 个有效信号」。"
            .into(),
    ));
    b.into()
}

/// 时间窗口：全局一组（替换每条可自定义特征的默认窗口）+ 选择页里的单条覆盖。
fn window_bar<'a>(m: &Matrix, v: &ViewState) -> Element<'a, Msg> {
    use super::feature_matrix_readout::format_window;
    let cw = super::feature_matrix::config_windows();
    let fixed = if m.present {
        m.features().iter().filter(|r| !r.head().win_custom).count()
    } else {
        0
    };
    let mut b = column![].spacing(4);
    let mut r = row![text("时间窗口 ").size(12).color(C_HEAD)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    match &v.win_edit {
        None => {
            let g = cw.global.as_deref().map_or_else(
                || "默认（各特征用字典窗口）".to_string(),
                super::feature_matrix_readout::format_windows,
            );
            r = r.push(text(format!("全局：{g}")).size(11).color(C_TXT));
            if !cw.overrides.is_empty() {
                r = r.push(text(format!("单条覆盖 {} 条", cw.overrides.len())).size(10).color(C_DIM));
            }
            r = r.push(chip("修改…".into(), false, Msg::WinOpen));
            if fixed > 0 {
                r = r.push(
                    text(format!("瞬时量与窗口固定的 {fixed} 条不受影响；单条覆盖在「自定义…」里逐条设"))
                        .size(10)
                        .color(C_DIM),
                );
            }
            b = b.push(r);
        }
        Some(w) => {
            r = r.push(text("全局：").size(11).color(C_DIM));
            if w.list.is_empty() {
                r = r.push(text("默认（各特征用字典窗口）").size(11).color(C_TXT));
            }
            for x in &w.list {
                r = r.push(chip(format!("{} ✕", format_window(*x)), true, Msg::WinRemove(*x)));
            }
            r = r
                .push(
                    iced::widget::text_input("加窗口，如 10s 或 1m, 15m", &w.input)
                        .on_input(Msg::WinInput)
                        .on_submit(Msg::WinAdd)
                        .size(11)
                        .width(Length::Fixed(200.0)),
                )
                .push(chip("添加".into(), false, Msg::WinAdd));
            b = b.push(r.wrap());
            let mut r2 = row![text("常用 ").size(10).color(C_DIM)]
                .spacing(6)
                .align_y(iced::Alignment::Center);
            for (i, (name, ws)) in super::feature_matrix::WINDOW_PRESETS.iter().enumerate() {
                r2 = r2.push(chip(
                    format!("{name}（{}）", super::feature_matrix_readout::format_windows(ws)),
                    false,
                    Msg::WinQuick(i),
                ));
            }
            r2 = r2
                .push(chip("恢复默认".into(), false, Msg::WinDefault))
                .push(text("　").size(10))
                .push(chip("✔ 应用并重启引擎".into(), true, Msg::WinApply))
                .push(chip("取消".into(), false, Msg::WinClose));
            b = b.push(r2.wrap());
            if !w.err.is_empty() {
                b = b.push(text(w.err.clone()).size(10).color(C_BAD));
            }
            b = b.push(dim(format!(
                "范围 100ms ~ 1d，至多 8 个。替换每条可自定义特征的默认窗口（字典窗口里带「瞬时」的保留瞬时）；                 瞬时量与窗口固定的 {fixed} 条不受影响。窗口越多、越长，延迟与内存越高；                 2 秒这类很短的窗口里，多数统计量会因样本不足而没有值。"
            )));
        }
    }
    b.into()
}

/// 启用集：默认 / 全开 / 自定义。点下去写配置并重启引擎（窗口从头累计）。
fn mode_bar<'a>(m: &Matrix) -> Element<'a, Msg> {
    let (mode, keys) = super::feature_matrix::read_config();
    let mut r = row![text("启用集 ").size(12).color(C_HEAD)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    r = r.push(chip("默认".into(), mode == "default", Msg::SetMode("default")));
    r = r.push(chip("全开".into(), mode == "all", Msg::SetMode("all")));
    let active_preset = super::feature_matrix::config_preset().filter(|_| mode == "custom");
    r = r.push(chip(
        match (&active_preset, mode.as_str()) {
            (Some(n), _) => format!("自定义：{n}（{}）…", keys.len()),
            (None, "custom") => format!("自定义（{}）…", keys.len()),
            _ => "自定义…".into(),
        },
        mode == "custom",
        Msg::OpenPicker,
    ));
    // 已存的选择集：选中即应用并重启引擎
    let names: Vec<String> = super::feature_presets::list().into_iter().map(|p| p.name).collect();
    if !names.is_empty() {
        r = r.push(text("　选择集").size(11).color(C_DIM));
        r = r.push(
            iced::widget::pick_list(names, active_preset.clone(), Msg::PresetApply)
                .placeholder("选一个直接应用…")
                .text_size(11)
                .padding([2, 6]),
        );
    }
    if m.present {
        let f = m.features();
        let on = f.iter().filter(|r| r.enabled()).count();
        r = r.push(text(format!("引擎当前启用 {on}/{} 条特征", f.len())).size(10).color(C_DIM));
    }
    if mode == "all" {
        r = r.push(text("全开超出常驻延迟 / 内存预算，适合临时观察").size(10).color(C_WARN));
    }
    r.into()
}

fn prior_tag(p: &str) -> (&'static str, Color) {
    match p {
        "gross" => ("毛", C_OK),
        "falsified" => ("证伪", C_BAD),
        "untested" => ("未检验", C_DIM),
        _ => ("", C_DIM),
    }
}

/// 选择集栏：已存的选择集（点名字载入编辑）+ 名称 / 备注 + 保存 / 更新 / 删除 / 导出 / 导入。
fn preset_bar<'a>(p: &super::feature_matrix::Picker) -> Element<'a, Msg> {
    let presets = super::feature_presets::list();
    let mut b = column![].spacing(5);

    let mut lr = row![text("选择集 ").size(12).color(C_HEAD)]
        .spacing(5)
        .align_y(iced::Alignment::Center);
    if presets.is_empty() {
        lr = lr.push(text("还没有保存过选择集——勾选好后在下面起个名字「另存为新选择集」").size(10).color(C_DIM));
    }
    for pr in &presets {
        let editing = p.editing.as_deref() == Some(pr.name.as_str());
        let label = format!("{}（{}）", pr.name, pr.keys.len());
        lr = lr.push(chip(label, editing, Msg::PresetLoad(pr.name.clone())));
    }
    lr = lr.push(text("　").size(11));
    lr = lr.push(chip("＋ 新建".into(), p.editing.is_none(), Msg::PresetNew));
    lr = lr.push(chip("导入…".into(), false, Msg::PresetImport));
    // 选择集多了会超出一行：换行排，不横向溢出
    b = b.push(lr.wrap());

    let status = match &p.editing {
        Some(n) if p.dirty() => (format!("正在编辑「{n}」· 有未保存的修改"), C_WARN),
        Some(n) => (format!("正在编辑「{n}」· 已保存"), C_OK),
        None => ("新选择集（未保存）".to_string(), C_DIM),
    };
    let mut er = row![
        text(status.0).size(11).color(status.1),
        iced::widget::text_input("名称", &p.name)
            .on_input(Msg::PresetName)
            .size(11)
            .width(Length::Fixed(180.0)),
        iced::widget::text_input("备注（可选）", &p.note)
            .on_input(Msg::PresetNote)
            .size(11)
            .width(Length::Fixed(260.0)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    if let Some(n) = &p.editing {
        let renaming = p.name.trim() != n;
        er = er.push(chip(
            if renaming { format!("更新并改名为「{}」", p.name.trim()) } else { "更新".into() },
            p.dirty(),
            Msg::PresetUpdate,
        ));
    }
    er = er.push(chip("另存为新选择集".into(), false, Msg::PresetSaveNew));
    if let Some(n) = &p.editing {
        er = er.push(chip("导出…".into(), false, Msg::PresetExport(n.clone())));
        let confirming = p.confirm_delete.as_deref() == Some(n.as_str());
        er = er.push(chip(
            if confirming { "确认删除".into() } else { "删除".into() },
            confirming,
            Msg::PresetDelete(n.clone()),
        ));
    }
    b = b.push(er);
    let note = super::feature_matrix::preset_note();
    if !note.is_empty() {
        let c = if note.starts_with('✗') { C_BAD } else { C_DIM };
        b = b.push(text(note).size(10).color(c));
    }
    b.into()
}

/// 自定义启用集的选择页：七阶段、一行一个特征、勾选框。
fn picker_view<'a>(m: &Matrix, p: &super::feature_matrix::Picker) -> Element<'a, Msg> {
    use super::feature_matrix::Bulk;
    let feats = m.features();
    let total = feats.len();
    let mut b = column![].spacing(6).width(Length::Fill);

    let extra = feats
        .iter()
        .filter(|r| p.selected.contains(&r.head().key) && !r.head().default_on)
        .count();
    b = b.push(
        row![
            text(format!("自定义启用集：已选 {}/{total} 条特征", p.selected.len())).size(13).color(C_HEAD),
            text(format!("（其中 {extra} 条不在默认集）")).size(10).color(C_DIM),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    b = b.push(preset_bar(p));
    b = b.push(
        row![
            text("时间窗口").size(12).color(C_HEAD),
            text("全局").size(11).color(C_DIM),
            iced::widget::text_input("空 = 字典默认；如 1m 5m 15m", &p.global_text)
                .on_input(Msg::PickGlobalWindows)
                .size(11)
                .width(Length::Fixed(220.0)),
            text("每条特征的「窗口」列可单独覆盖；两者都随选择集保存，应用时一起写入").size(10).color(C_DIM),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    if let Err(e) = p.windows() {
        b = b.push(text(format!("✗ {e}")).size(10).color(C_BAD));
    }
    b = b.push(
        row![
            iced::widget::text_input("搜索键名或中文名…", &p.search)
                .on_input(Msg::PickSearch)
                .size(11)
                .width(Length::Fixed(260.0)),
            chip("全选".into(), false, Msg::PickBulk(None, Bulk::All)),
            chip("全不选".into(), false, Msg::PickBulk(None, Bulk::None)),
            chip("恢复默认".into(), false, Msg::PickBulk(None, Bulk::Default)),
            text("　").size(11),
            chip("✔ 应用并重启引擎".into(), true, Msg::ApplyPicker),
            chip("取消".into(), false, Msg::ClosePicker),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    b = b.push(dim(
        "批量按钮只作用于当前搜索结果。应用后引擎重启，所有窗口从头累计；\
         选得越多延迟与内存越高（全开约为默认集的 1.4 倍内存）。先验：毛 = 批 0 确认的毛来源一族，\
         证伪 = docs/20 判定无可交易 alpha 的几类，未检验 = 其余。"
            .into(),
    ));
    b = b.push(
        row![
            pc(text("").size(10), 1),
            pc(text("特征").size(10).color(C_DIM), P_NAME),
            pc(text("先验").size(10).color(C_DIM), 2),
            pc(text("级别").size(10).color(C_DIM), 2),
            pc(text("数据层").size(10).color(C_DIM), P_LAYER),
            pc(text("默认窗口").size(10).color(C_DIM), 4),
            pc(text("窗口覆盖").size(10).color(C_DIM), 4),
            pc(text("默认集").size(10).color(C_DIM), 2),
        ]
        .spacing(6),
    );

    for (key, label) in Matrix::STAGES {
        let rows: Vec<&FeatureRow<'_>> = feats.iter().filter(|r| r.head().stage == key && p.matches(r.head())).collect();
        if rows.is_empty() {
            continue;
        }
        let sel = rows.iter().filter(|r| p.selected.contains(&r.head().key)).count();
        b = b.push(
            row![
                sec(format!("{label} · 已选 {sel}/{}", rows.len())),
                chip("全选".into(), false, Msg::PickBulk(Some(key), Bulk::All)),
                chip("全不选".into(), false, Msg::PickBulk(Some(key), Bulk::None)),
                chip("默认".into(), false, Msg::PickBulk(Some(key), Bulk::Default)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        );
        for r in rows {
            let h = r.head();
            let on = p.selected.contains(&h.key);
            let k = h.key.clone();
            let (pt, pcol) = prior_tag(&h.prior);
            let wins = super::feature_matrix_readout::format_windows(&h.win_default);
            let over: Element<'a, Msg> = if h.win_custom {
                let k2 = h.key.clone();
                iced::widget::text_input("—", p.win_text.get(&h.key).map_or("", String::as_str))
                    .on_input(move |t| Msg::PickWindows(k2.clone(), t))
                    .size(10)
                    .into()
            } else {
                text("固定").size(10).color(C_PEND).into()
            };
            let lat = match h.latency.as_str() {
                "hot" => "逐事件".to_string(),
                "warm" => "250ms 节拍".to_string(),
                "offline_only" => "仅离线".to_string(),
                x => x.to_string(),
            };
            b = b.push(
                row![
                    pc(iced::widget::checkbox(on).on_toggle(move |x| Msg::PickToggle(k.clone(), x)).size(14), 1),
                    pc(
                        column![
                            text(h.name_cn.clone()).size(11).color(if h.not_implemented() { C_PEND } else { C_TXT }),
                            text(h.key.clone()).size(9).color(C_DIM),
                        ],
                        P_NAME
                    ),
                    pc(text(pt).size(10).color(pcol), 2),
                    pc(text(lat).size(10).color(C_DIM), 2),
                    pc(layer_badges(h), P_LAYER),
                    pc(text(wins).size(10).color(C_DIM), 4),
                    pc(over, 4),
                    pc(text(if h.default_on { "✓" } else { "" }).size(10).color(C_OK), 2),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }
    }
    b.into()
}

/// 特征引擎总开关：状态 + 启动 / 停止。
///
/// 与进程页「特征引擎」那一行是同一个单元、同一个动作——放在这里是因为
/// 看特征的人要在看特征的地方启停，而不是切到进程页去找。
/// 停掉**没有数据缺口**（录制器照常落盘，事后可回放重算），所以不需要二次确认。
fn engine_bar<'a>() -> Element<'a, Msg> {
    let note = super::feature_matrix::engine_note();
    let mut r = row![text("特征引擎 ").size(12).color(C_HEAD)]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    match super::feature_matrix::engine_state() {
        // 轮询还没出第一轮：别猜状态，也别给按钮——猜错了按钮就是反的
        None => r = r.push(text("查询中…").size(11).color(C_DIM)),
        Some(st) if st.active => {
            r = r
                .push(text("● 运行中").size(11).color(C_OK))
                .push(
                    text(format!("已运行 {}", super::svcctl::fmt_dur(st.uptime_secs)))
                        .size(10)
                        .color(C_DIM),
                )
                .push(chip("■ 停止".into(), false, Msg::Engine("stop")));
        }
        Some(_) => {
            r = r
                .push(text("○ 已停止").size(11).color(C_WARN))
                .push(chip("▶ 启动".into(), false, Msg::Engine("start")));
        }
    }
    if !note.is_empty() {
        r = r.push(text(note).size(10).color(C_DIM));
    }
    r.into()
}

pub fn pane_body<'a>() -> Element<'a, Msg> {
    let m: Matrix = ro::snapshot();
    let v = super::feature_matrix::state();
    let mut b = column![sec(
        "订单流与市场微观结构 · 特征矩阵（docs/31 · 感知层·非交易信号）".into()
    )]
    .spacing(6)
    .padding(10);

    // 视图切换 + 收起控件
    let mut vr = row![].spacing(4);
    for view in View::ALL {
        vr = vr.push(chip(view.label().into(), v.view == view, Msg::SetView(view)));
    }
    if v.view != View::Engine && v.picker.is_none() {
        vr = vr.push(text("　").size(11)).push(chip(
            if v.fold_controls { "▾ 展开控件".into() } else { "▴ 收起控件".into() },
            v.fold_controls,
            Msg::ToggleFoldControls,
        ));
    }
    b = b.push(vr.align_y(iced::Alignment::Center));
    let fold = v.fold_controls && v.view != View::Engine && v.picker.is_none();
    if !fold {
        b = b.push(engine_bar());
        b = b.push(mode_bar(&m));
        b = b.push(window_bar(&m, &v));
    }

    if let Some(p) = &v.picker {
        if !m.present {
            return b
                .push(text("还没有快照：先启动一次引擎，面板才拿得到完整的特征清单").size(11).color(C_WARN))
                .push(chip("取消".into(), false, Msg::ClosePicker))
                .into();
        }
        return container(scrollable(b.push(picker_view(&m, p)).width(Length::Fill)))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    if !m.present {
        return b
            .push(
                text(format!(
                    "暂无快照（{}）。生成：\n  \
                     cargo run --release -p wealthspring-features --example replay_events_csv -- <目录>\n\
                     引擎常驻时由 sidecar::SidecarWriter 每 500ms 写一次。面板只读，不连交易所。",
                    ro::board_path().display()
                ))
                .size(11)
                .color(C_DIM),
            )
            .into();
    }

    if super::feature_matrix::engine_state().is_some_and(|st| !st.active) {
        // 停了之后旁路文件还在：不说一声，人会把最后一张快照当成现在
        b = b.push(
            text("引擎已停止：下面是停止前的最后一次快照，不再更新")
                .size(11)
                .color(C_WARN),
        );
    }
    if v.view == View::Engine {
        b = b.push(top_bar(&m)).push(engine_view(&m));
        return container(scrollable(b.width(Length::Fill)))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }
    if !fold {
        b = b.push(top_bar(&m));
        b = b.push(filter_bar(&m, &v));
    }
    b = b.push(table_controls(&v));

    // 表头固定在滚动区之外；表体单独滚动。两者用同一套窗口列，所以上下对齐。
    let wins = table_windows(&m, &v);
    let body = match v.view {
        View::Vector => vector_view(&m, &v, &wins),
        _ => matrix_view(&m, &v, &wins),
    };
    column![
        b,
        container(table_header(&wins, v.table, v.metric)).padding([0, 10]),
        container(scrollable(body).height(Length::Fill))
            .padding(iced::Padding { top: 0.0, right: 10.0, bottom: 10.0, left: 10.0 })
            .height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缺值显示成破折号而不是零() {
        // 面板上最贵的一个错：`UNAVAILABLE` 的 slot 显示 `0.0000` 看起来完全正常。
        assert_eq!(num(None), "—");
        assert_eq!(num(Some(0.0)), "0");
        assert_ne!(num(None), num(Some(0.0)));
    }

    #[test]
    fn 小量级不被格式化成一串零() {
        // 波动率类特征量级在 1e-5，格式成 "0.0000" 会让整列看起来全是零。
        assert!(num(Some(1.2e-5)).contains('e'));
        assert_eq!(num(Some(0.1234)), "0.1234");
        // 价格量级（8 万）不该被写成科学计数法——那一列是要用来核对的。
        assert_eq!(num(Some(81138.35)), "81138.35");
    }

    #[test]
    fn 非有限值不显示成数字() {
        // NaN 格式化成 "NaN" 还算诚实，但 inf 会被 `{:.4}` 写成 "inf"；
        // 统一成 ∞，免得和一个真的很大的数混起来。
        assert_eq!(num(Some(f64::INFINITY)), "∞");
        assert_eq!(num(Some(f64::NEG_INFINITY)), "∞");
    }

    #[test]
    fn 五个质量态颜色互不相同() {
        // DEGRADED（等一会儿就有）与 UNAVAILABLE（这个市场永远没有）撞色就失去意义。
        let qs = ["GOOD", "DEGRADED", "STALE", "INVALID", "UNAVAILABLE"];
        for (i, a) in qs.iter().enumerate() {
            for bb in &qs[i + 1..] {
                let (x, y) = (qcolor(a), qcolor(bb));
                assert!(
                    (x.r - y.r).abs() + (x.g - y.g).abs() + (x.b - y.b).abs() > 0.15,
                    "{a} 与 {bb} 的颜色太接近"
                );
            }
        }
    }

    #[test]
    fn 数据层徽标覆盖引擎的全部词表() {
        // 这一条守的是那个安静的漏：配色表里的串与引擎的词表差一个大小写，
        // 徽标就全变成兜底色。用**兜底色本身**当判据——落到 C_DIM 即视为未覆盖。
        for l in ENGINE_LAYERS {
            let c = layer_color(l);
            assert!(
                (c.r - C_DIM.r).abs() + (c.g - C_DIM.g).abs() + (c.b - C_DIM.b).abs() > 0.05,
                "数据层 `{l}` 落到了兜底色——配色表里的串与引擎的 DataLayer::as_str() 不一致"
            );
        }
        // 没登记过的串仍然要走兜底而不是 panic。
        assert_eq!(layer_color("不存在的层").r, C_DIM.r);
    }

    #[test]
    fn 数据层徽标颜色互不相同() {
        let ls = ENGINE_LAYERS;
        for (i, a) in ls.iter().enumerate() {
            for bb in &ls[i + 1..] {
                let (x, y) = (layer_color(a), layer_color(bb));
                assert!(
                    (x.r - y.r).abs() + (x.g - y.g).abs() + (x.b - y.b).abs() > 0.15,
                    "{a} 与 {bb} 的颜色太接近"
                );
            }
        }
    }

    #[test]
    fn 显示的串里不能留markdown记号() {
        // iced 的 `text` 不解析 Markdown。文档注释里写 `**强调**` 是对的，
        // 但同样的写法漏进面板串里就会被原样画出来——实机截图里
        // 「交叉消除与深度裁剪**看发生率不看有没有**」就是这么露出来的。
        // 只扫非测试部分：本测试模块自己带着这些记号。
        let whole = include_str!("feature_matrix_view.rs");
        let body = whole.split("#[cfg(test)]").next().unwrap();
        for (i, line) in body.lines().enumerate() {
            let t = line.trim_start();
            // 注释行随便写——那是给读代码的人看的。
            if t.starts_with("//") {
                continue;
            }
            assert!(
                !t.contains("**"),
                "第 {} 行的显示串里有 Markdown 记号：{t}",
                i + 1
            );
        }
    }

    #[test]
    fn 七个阶段都要有分节() {
        // 漏一个阶段的话，那一族特征在面板上根本不出现——而引擎在算它。
        assert_eq!(Matrix::STAGES.len(), 7);
        let keys: Vec<&str> = Matrix::STAGES.iter().map(|(k, _)| *k).collect();
        for k in ["S1_price", "S2_trade", "S3_book", "S4_liquidity",
                  "S5_order_behavior", "S6_regime", "S7_execution"] {
            assert!(keys.contains(&k), "{k} 没有分节");
        }
    }
}
