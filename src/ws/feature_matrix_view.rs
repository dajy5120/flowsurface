//! 特征矩阵面板 — 视图（docs/31 §8.1）。
//!
//! 两个视图：特征矩阵（七阶段纵向分节，阶段可折叠、异常标黄）与引擎健康。
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
    Col, FeatureMatrixMsg as Msg, Metric, QualityFilter, StatusFilter, TableMode, View, ViewState,
};
use super::feature_matrix_readout::{self as ro, FeatureRow, Matrix, Slot};

fn cell<'a>(t: String, w: f32, c: Color) -> Element<'a, Msg> {
    container(text(t).size(crate::ui::text::s_small()).color(c)).width(Length::Fixed(w)).into()
}

/// 数值单元右对齐——右对齐后小数点纵向成列，一眼能比大小。
fn numc<'a>(t: String, w: f32, c: Color) -> Element<'a, Msg> {
    container(text(t).size(crate::ui::text::s_small()).color(c))
        .width(Length::Fixed(w))
        .align_x(iced::Alignment::End)
        .into()
}

fn dim<'a>(t: String) -> Element<'a, Msg> {
    text(t).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()).into()
}

fn sec<'a>(t: String) -> Element<'a, Msg> {
    text(t).size(crate::ui::text::s_emph()).color(crate::ui::pal::head()).into()
}

fn chip<'a>(label: String, active: bool, msg: Msg) -> Element<'a, Msg> {
    button(text(label).size(crate::ui::text::s_small()))
        .padding(crate::ui::metrics::pad2(0, 2))
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

/// 数值格式化。
///
/// **`None` 必须显示成「—」而不是 0。** 这是整个面板最容易出错的一处：
/// 一个「不可用」的 slot 显示成 `0.0000` 看起来完全正常，而它其实什么都不知道。
/// 一个 slot 的值文字，带 UPDS 来源标记（docs/35 §7.2）：质量 DEGRADED 的是「估算」→ 前缀 `≈`；
/// INVALID 是「错误」→ 前缀 `!`（V8 六种「无」里的错误符号）。原先只靠颜色区分，灰度 / 色弱下看不出。
fn vtext(s: &Slot) -> String {
    let v = num(s.value);
    if s.value.is_none() {
        return v;
    }
    match s.quality.as_str() {
        "DEGRADED" => format!("≈ {v}"),
        "INVALID" => format!("{} {v}", crate::ui::fmt::Absence::Invalid.glyph()),
        _ => v,
    }
}

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
        "GOOD" => crate::ui::pal::ok(),
        "DEGRADED" => crate::ui::pal::warn(),
        "STALE" => crate::ui::pal::stale(),
        "INVALID" => crate::ui::pal::bad(),
        "UNAVAILABLE" => crate::ui::pal::pend(),
        _ => crate::ui::pal::txt(),
    }
}

/// 数据层徽标的颜色。
///
/// 串必须与引擎的 `DataLayer::as_str()` **逐字一致**。第一版我按直觉写成了
/// `trades` / `derivative` / `own_orders`（小写蛇形），而引擎写的是
/// `Trades` / `Derivative` / `OwnOrders`——后果是真实快照里 57 个 slot 的徽标
/// 全落到兜底的 `crate::ui::pal::dim()` 上，面板看起来完全正常，只是那一列不再区分数据层。
/// 主仓 `tests/panel_contract.rs` 把这份列表与引擎词表对拍，
/// 本文件的 `数据层徽标覆盖引擎的全部词表` 从另一侧守同一件事。
fn layer_color(l: &str) -> Color {
    match l {
        // 七个数据层 = 领域包的图表系列色（感知均匀、色弱友好，docs/35 §4.2）
        "BBO" => crate::ui::pal::series(0),
        "Trades" => crate::ui::pal::series(6),
        "L2" => crate::ui::pal::series(5),
        "L3" => crate::ui::pal::series(7),
        "Derivative" => crate::ui::pal::series(4),
        "OwnOrders" => crate::ui::pal::series(3),
        "Derived" => crate::ui::pal::series(1),
        _ => crate::ui::pal::dim(),
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
        r = r.push(text(l.clone()).size(crate::ui::text::s_meta()).color(layer_color(l)));
    }
    r.into()
}

// ── 表格 ─────────────────────────────────────────────────────────────────
//
// 一行一个特征、各时间窗口横向成组；组与组之间一条竖线。
//
// **列宽是固定像素**（横向滚动的前提）。第一次按内容自适应一次（估算每列最长的内容）后就**固定**，
// 数据怎么变都不跟着跳；之后只由用户在表头拖分隔线调整。双击分隔线 = 这一列按当前内容重新自适应一次；
// 宽度存在 `feature_matrix_ui.json`。
// 文字一律不换行、超出裁掉——换行会撑高固定行高，压到下一行上。
//
// 表头在滚动区之外固定在顶上；表体双向滚动，横向滚动时把表头滚到同一位置（`Effect::ScrollX`），
// 所以横向滚动条在表格区域底部，表头始终与下面的列对齐。所有阶段共用同一套窗口列（可见行的窗口并集）。
//
// 质量不占文字列：每个窗口组左边一条色条，原因在悬停提示里。「透视」模式每个窗口一列、只显示一个指标，
// z 与分位按偏离蓝—红着色成热力图。

/// 每列右侧的分隔带宽度（表头里是拖拽把手）。
const SEP: f32 = 7.0;
/// 质量色条宽度。
const BAR: f32 = 3.0;
/// 行高（固定：色条与热力底色要铺满整行）。
const ROW_H: f32 = 22.0;

/// 比例列（选择页等非表格处仍在用）。
const P_NAME: u16 = 5;
const P_LAYER: u16 = 3;

fn pc<'a>(e: impl Into<Element<'a, Msg>>, portion: u16) -> Element<'a, Msg> {
    container(e).width(Length::FillPortion(portion)).into()
}

/// 带底色的容器（`None` = 透明）。
fn tinted<'a>(e: impl Into<Element<'a, Msg>>, c: Option<Color>) -> container::Container<'a, Msg> {
    container(e).style(move |_t: &iced::Theme| container::Style {
        background: c.map(iced::Background::Color),
        ..Default::default()
    })
}

/// 不换行、超出裁掉的文字。
fn nowrap<'a>(t: String, size: f32, c: Color) -> iced::widget::Text<'a> {
    text(t).size(size).color(c).wrapping(iced::widget::text::Wrapping::None)
}

/// 定宽格子：垂直居中、裁掉溢出。`right` = 右对齐（数值列，小数点纵向成列）。
fn fcell<'a>(e: impl Into<Element<'a, Msg>>, w: f32, right: bool) -> Element<'a, Msg> {
    container(e)
        .width(Length::Fixed(w))
        .height(Length::Fill)
        .align_y(iced::Alignment::Center)
        .align_x(if right { iced::Alignment::End } else { iced::Alignment::Start })
        .clip(true)
        .into()
}

/// 竖线样式：固定颜色（不跟主题取色——深色主题下调色板的「强背景色」太暗，线几乎看不见）。
fn line_style(c: Color) -> iced::widget::rule::Style {
    iced::widget::rule::Style {
        color: c,
        radius: iced::border::Radius::default(),
        fill_mode: iced::widget::rule::FillMode::Full,
        snap: true,
    }
}

/// 表体里的分隔带：每列之间一条浅色竖线；`group` = 窗口组边界（稍深）。
fn sep<'a>(group: bool) -> Element<'a, Msg> {
    let c = if group { crate::ui::pal::group_line() } else { crate::ui::pal::line() };
    container(iced::widget::rule::vertical(1.0).style(move |_t: &iced::Theme| line_style(c)))
        .width(Length::Fixed(SEP))
        .height(Length::Fill)
        .align_x(iced::Alignment::Center)
        .into()
}

/// 表头分隔线的样式：比表体的线明显得多；`strong` = 窗口组边界（更亮）。
fn head_rule(_t: &iced::Theme, strong: bool) -> iced::widget::rule::Style {
    line_style(if strong { crate::ui::pal::group_line() } else { crate::ui::pal::head_line() })
}

/// 表头里的竖线（不可拖，用在上层的窗口标题行）。
fn head_sep<'a>(strong: bool) -> Element<'a, Msg> {
    container(iced::widget::rule::vertical(if strong { 2.0 } else { 1.0 }).style(move |t: &iced::Theme| head_rule(t, strong)))
        .width(Length::Fixed(SEP))
        .height(Length::Fill)
        .align_x(iced::Alignment::Center)
        .into()
}

/// 表头里的横线。
fn head_hline<'a>(total: f32) -> Element<'a, Msg> {
    container(iced::widget::rule::horizontal(1.0).style(|t: &iced::Theme| head_rule(t, false)))
        .width(Length::Fixed(total))
        .into()
}

/// 表头里的分隔带：画出分隔线，可拖拽调 `col` 的宽度，双击按当前内容重新自适应这一列。
/// `line` = 窗口组边界（线更粗更亮）。
fn grip<'a>(line: bool, col: Col, w: f32) -> Element<'a, Msg> {
    iced::widget::mouse_area(
        container(iced::widget::rule::vertical(if line { 2.0 } else { 1.0 }).style(move |t: &iced::Theme| head_rule(t, line)))
        .width(Length::Fixed(SEP))
        .height(Length::Fill)
        .align_x(iced::Alignment::Center),
    )
    .on_press(Msg::DragStart(col, w))
    .on_double_click(Msg::AutoCol(col))
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .into()
}

/// 估算文字宽度（像素）：中日韩字符按一个字号，其余按 0.62 个字号。
fn est(t: &str, size: f32) -> f32 {
    t.chars()
        .map(|c| if (c as u32) >= 0x2E80 { size } else { size * 0.62 })
        .sum()
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

fn z_text(z: Option<f64>) -> String {
    z.map_or_else(|| "—".into(), |z| format!("{z:+.2}"))
}

/// 透视「质量」格里的字。
fn quality_word(s: &Slot) -> String {
    if s.not_implemented() {
        "待实现".into()
    } else if s.reason.is_empty() {
        "良好".into()
    } else {
        reason_cn(&s.reason).to_string()
    }
}

/// 涨（绿）/ 跌（红）。与质量色条的绿红是两件事：箭头只说「这个数比 N 秒前变大 / 变小」，不说好坏。
/// 完整模式里箭头那一小列的宽度：够画「启用的档数」个箭头。
fn arrow_width(levels: usize) -> f32 {
    10.0 + 7.0 * levels.max(1) as f32
}

/// 涨跌比较的上下文：N 秒前那份快照（每格 z、值）与阈值。
struct Trend {
    past: Option<Box<[(f32, f32)]>>,
    /// 各档阈值（第 N 档画 N 个箭头）。
    ths: Vec<f64>,
    /// 「近 5s」这样的说法。
    label: String,
}

impl Trend {
    fn new(m: &Matrix, v: &ViewState) -> Self {
        Self {
            past: ro::past(m.as_of, u64::from(v.trend_ms)),
            ths: v.ths.clone(),
            label: format!("近 {}", ro::format_window(v.trend_ms)),
        }
    }

    /// 这一格与 N 秒前相比 z 的变化。没有 z（归一化为 none 的特征）或历史还不够长时 `None`。
    fn dz(&self, s: &Slot) -> Option<f64> {
        let (z0, _) = *self.past.as_ref()?.get(s.idx)?;
        let z = s.z?;
        (!z0.is_nan()).then(|| z - f64::from(z0))
    }

    fn level(&self, s: &Slot) -> i8 {
        self.dz(s)
            .map_or(0, |d| super::feature_matrix::trend_level(d, &self.ths))
    }
}

/// 箭头与颜色：第 N 档画 N 个，▲ 绿（变大）/ ▼ 红（变小），0 不画。
fn arrow(level: i8) -> (String, Color) {
    let n = usize::from(level.unsigned_abs());
    match level.signum() {
        1 => ("▲".repeat(n), crate::ui::pal::up()),
        -1 => ("▼".repeat(n), crate::ui::pal::down()),
        _ => (String::new(), crate::ui::pal::dim()),
    }
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
        container(text(tip).size(crate::ui::text::s_small())).style(crate::style::tooltip).padding(crate::ui::metrics::space(2)),
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
        crate::ui::pal::alpha(crate::ui::pal::bad(), a)
    } else {
        crate::ui::pal::alpha(crate::ui::pal::accent(), a)
    })
}

/// 质量色条的颜色（没启用 / 待实现的用最暗的灰）。
fn bar_color(s: &Slot) -> Color {
    if s.not_implemented() || s.disabled() {
        crate::ui::pal::pend().scale_alpha(0.5)
    } else {
        qcolor(&s.quality)
    }
}

/// 值的颜色：良好用正文色，其余按质量；没启用 / 待实现的压暗。
fn value_color(s: &Slot) -> Color {
    if s.not_implemented() || s.disabled() {
        crate::ui::pal::pend()
    } else if s.quality == "GOOD" {
        crate::ui::pal::txt()
    } else {
        qcolor(&s.quality)
    }
}

/// 一条特征各窗口的质量汇总（状态列）：全部良好只写 GOOD，否则写最要紧的那个问题。
fn row_status(r: &FeatureRow<'_>) -> (String, Color) {
    let h = r.head();
    if h.not_implemented() {
        return ("已登记·待实现".into(), crate::ui::pal::pend());
    }
    if !r.enabled() {
        return ("未启用".into(), crate::ui::pal::pend());
    }
    let n = r.slots.len();
    let good = r.slots.iter().filter(|s| s.quality == "GOOD").count();
    if good == n {
        return ("GOOD".into(), crate::ui::pal::ok());
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

/// 各类列的实际宽度（像素）：自适应，再叠用户拖过的。
#[derive(Debug, Clone, Copy)]
struct Widths {
    name: f32,
    key: f32,
    val: f32,
    z: f32,
    pct: f32,
    cell: f32,
    unit: f32,
    layer: f32,
    status: f32,
    /// 箭头小列（随启用的档数定，不可拖）。
    arrow: f32,
}

impl Widths {
    fn get(&self, c: Col) -> f32 {
        match c {
            Col::Name => self.name,
            Col::Key => self.key,
            Col::Val => self.val,
            Col::Z => self.z,
            Col::Pct => self.pct,
            Col::Cell => self.cell,
            Col::Unit => self.unit,
            Col::Layer => self.layer,
            Col::Status => self.status,
        }
    }

    /// 一个窗口组的宽度（不含组后的分隔带）。
    fn group(&self, mode: TableMode) -> f32 {
        match mode {
            TableMode::Full => BAR + self.val + self.arrow + SEP + self.z + SEP + self.pct,
            TableMode::Pivot => self.cell,
        }
    }

    /// 整表宽度。
    fn total(&self, mode: TableMode, nwin: usize) -> f32 {
        self.name
            + SEP
            + self.key
            + SEP
            + nwin as f32 * (self.group(mode) + SEP)
            + self.unit
            + SEP
            + self.layer
            + SEP
            + self.status
            + SEP
    }
}

/// 自适应列宽：按可见内容估算每类列最长的那一格，再叠用户拖过的宽度。
fn widths(rows: &[FeatureRow<'_>], wins: &[u32], v: &ViewState) -> Widths {
    let pad = 12.0;
    let mx = |it: &mut dyn Iterator<Item = f32>, lo: f32, hi: f32| it.fold(lo, f32::max).min(hi);
    let slots = || rows.iter().flat_map(|r| r.slots.iter().copied());
    let name = mx(&mut rows.iter().map(|r| est(&r.head().name_cn, 11.0) + pad), 60.0, 280.0);
    let key = mx(&mut rows.iter().map(|r| est(&r.head().key, 10.0) + pad), 60.0, 300.0);
    let mut val = mx(&mut slots().map(|s| est(&vtext(s), 11.0) + pad), 36.0, 170.0);
    let z = mx(&mut slots().map(|s| est(&num(s.z), 11.0) + pad), 34.0, 110.0);
    let pct = est("100%", 11.0) + pad;
    // 窗口组要装得下它的标题
    let label = wins.iter().map(|w| est(&format!("窗口 {}", window_label(*w)), 10.0) + pad).fold(0.0, f32::max);
    let arrow = arrow_width(v.ths.len());
    let full = BAR + val + arrow + SEP + z + SEP + pct;
    if full < label {
        val += label - full;
    }
    let cell = match v.metric {
        Metric::Z => est("+12.34", 11.0) + pad,
        Metric::Pct => est("100%", 11.0) + pad,
        Metric::Value => val,
        Metric::Quality => mx(&mut slots().map(|s| est(&quality_word(s), 11.0) + pad), 40.0, 140.0),
        Metric::Change => est(&format!("{} +12.34", "▼".repeat(v.ths.len())), 11.0) + pad,
    }
    .max(wins.iter().map(|w| est(&format!("{} · {}", window_label(*w), v.metric.label()), 10.0) + pad).fold(0.0, f32::max));
    let unit = mx(&mut rows.iter().map(|r| est(&r.head().unit, 10.0) + pad), 36.0, 130.0);
    let layer = mx(
        &mut rows.iter().map(|r| r.head().inputs.iter().map(|l| est(l, 9.0) + 3.0).sum::<f32>() + pad),
        44.0,
        220.0,
    );
    let status = mx(&mut rows.iter().map(|r| est(&row_status(r).0, 10.0) + pad), 60.0, 340.0);
    let mut w = Widths { name, key, val, z, pct, cell, unit, layer, status, arrow };
    // 自适应只算一次：还没有宽度的列（第一次打开 / 双击恢复之后）把这次算出的宽度固定下来，
    // 已有的一概不动——之后数据刷新不会让列宽跟着跳。空表（还没数据）不固定。
    if !rows.is_empty() {
        super::feature_matrix::freeze_missing(&Col::ALL.map(|c| (c, w.get(c))));
    }
    for (c, x) in &v.col_w {
        let x = *x;
        match c {
            Col::Name => w.name = x,
            Col::Key => w.key = x,
            Col::Val => w.val = x,
            Col::Z => w.z = x,
            Col::Pct => w.pct = x,
            Col::Cell => w.cell = x,
            Col::Unit => w.unit = x,
            Col::Layer => w.layer = x,
            Col::Status => w.status = x,
        }
    }
    w
}

/// 一个窗口组（这个特征没有该窗口时留空；「—」表示有这个窗口但没有值）。
fn window_group<'a>(s: Option<&Slot>, mode: TableMode, metric: Metric, w: &Widths, tr: &Trend) -> Element<'a, Msg> {
    let gw = w.group(mode);
    let Some(s) = s else {
        return fcell(text(""), gw, false);
    };
    let dz = tr.dz(s);
    let level = tr.level(s);
    let tip = format!(
        "{}\n{} Δz {}",
        slot_tip(s),
        tr.label,
        dz.map_or_else(
            || if tr.past.is_none() { "—（历史还不够长）".to_string() } else { "—（这条特征没有 z）".to_string() },
            |d| format!("{d:+.2}")
        )
    );
    let body: Element<'a, Msg> = match mode {
        TableMode::Full => row![
            tinted(text(""), Some(bar_color(s)))
                .width(Length::Fixed(BAR))
                .height(Length::Fill),
            fcell(nowrap(vtext(s), 11.0, value_color(s)), w.val, true),
            {
                let (a, c) = arrow(level);
                fcell(nowrap(a, 10.0, c), w.arrow, false)
            },
            sep(false),
            fcell(nowrap(num(s.z), 11.0, crate::ui::pal::dim()), w.z, true),
            sep(false),
            fcell(nowrap(pct_text(s.percentile), 11.0, crate::ui::pal::dim()), w.pct, true),
        ]
        .height(Length::Fill)
        .into(),
        TableMode::Pivot => {
            let (t, bg, c) = match metric {
                Metric::Z => (z_text(s.z), s.z.and_then(|z| heat(z / 3.0)), crate::ui::pal::txt()),
                Metric::Pct => (
                    pct_text(s.percentile),
                    s.percentile.and_then(|p| heat((p - 0.5) * 2.0)),
                    crate::ui::pal::txt(),
                ),
                Metric::Value => (vtext(s), None, value_color(s)),
                Metric::Quality => (quality_word(s), Some(bar_color(s).scale_alpha(0.28)), crate::ui::pal::txt()),
                Metric::Change => {
                    let (a, c) = arrow(level);
                    // 档位越高底色越深
                    let depth = f32::from(level.unsigned_abs()) / tr.ths.len().max(1) as f32;
                    let bg = match level.signum() {
                        1 => Some(crate::ui::pal::up().scale_alpha(0.08 + 0.32 * depth)),
                        -1 => Some(crate::ui::pal::down().scale_alpha(0.08 + 0.32 * depth)),
                        _ => None,
                    };
                    (
                        dz.map_or_else(|| "—".into(), |d| format!("{a} {d:+.2}").trim().to_string()),
                        bg,
                        if level == 0 { crate::ui::pal::dim() } else { c },
                    )
                }
            };
            let muted = s.not_implemented() || s.disabled();
            tinted(
                container(nowrap(t, 11.0, if muted { crate::ui::pal::pend() } else { c }))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(iced::Alignment::End)
                    .align_y(iced::Alignment::Center)
                    .padding(crate::ui::metrics::pad2(0, 2))
                    .clip(true),
                bg,
            )
            .width(Length::Fixed(gw))
            .height(Length::Fill)
            .into()
        }
    };
    container(with_tip(body, tip))
        .width(Length::Fixed(gw))
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// 固定表头（可拖分隔线调列宽）。完整模式两层：上层窗口标签横跨一组，下层 值 / z / 分位。
fn table_header<'a>(wins: &[u32], mode: TableMode, metric: Metric, w: &Widths) -> Element<'a, Msg> {
    let h = |t: &str| nowrap(t.to_string(), 10.0, crate::ui::pal::dim());
    let gw = w.group(mode);
    let mut b = column![];
    if mode == TableMode::Full {
        let mut top = row![
            fcell(text(""), w.name, false),
            head_sep(false),
            fcell(text(""), w.key, false),
            head_sep(true),
        ]
        .height(Length::Fixed(18.0));
        for win in wins {
            top = top
                .push(
                    container(nowrap(format!("窗口 {}", window_label(*win)), 10.0, crate::ui::pal::head()))
                        .width(Length::Fixed(gw))
                        .height(Length::Fill)
                        .align_x(iced::Alignment::Center)
                        .align_y(iced::Alignment::Center)
                        .clip(true),
                )
                .push(head_sep(true));
        }
        b = b.push(top).push(head_hline(w.total(mode, wins.len())));
    }
    let mut r = row![
        fcell(h("特征"), w.name, false),
        grip(false, Col::Name, w.name),
        fcell(h("键"), w.key, false),
        grip(true, Col::Key, w.key),
    ]
    .height(Length::Fixed(18.0));
    for win in wins {
        match mode {
            TableMode::Full => {
                r = r
                    .push(container(text("")).width(Length::Fixed(BAR)))
                    .push(fcell(h("值"), w.val, true))
                    .push(fcell(h(" Δ"), w.arrow, false))
                    .push(grip(false, Col::Val, w.val))
                    .push(fcell(h("z"), w.z, true))
                    .push(grip(false, Col::Z, w.z))
                    .push(fcell(h("分位"), w.pct, true))
                    .push(grip(true, Col::Pct, w.pct));
            }
            TableMode::Pivot => {
                r = r
                    .push(
                        container(nowrap(format!("{} · {}", window_label(*win), metric.label()), 10.0, crate::ui::pal::head()))
                            .width(Length::Fixed(gw))
                            .height(Length::Fill)
                            .align_x(iced::Alignment::End)
                            .align_y(iced::Alignment::Center)
                            .padding(crate::ui::metrics::pad2(0, 2))
                            .clip(true),
                    )
                    .push(grip(true, Col::Cell, w.cell));
            }
        }
    }
    r = r
        .push(fcell(h("单位"), w.unit, false))
        .push(grip(false, Col::Unit, w.unit))
        .push(fcell(h("数据层"), w.layer, false))
        .push(grip(false, Col::Layer, w.layer))
        .push(fcell(h("状态"), w.status, false))
        .push(grip(false, Col::Status, w.status));
    let total = w.total(mode, wins.len());
    b = b.push(r).push(head_hline(total));
    iced::widget::mouse_area(
        tinted(b, Some(crate::ui::pal::card_bg())).width(Length::Fixed(total)),
    )
    .on_move(|p| Msg::HeaderMove(p.x))
    .on_release(Msg::DragEnd)
    .on_exit(Msg::DragEnd)
    .into()
}

/// 一行。斑马纹 + 悬停整行高亮；鼠标进出发消息（面板据此记下高亮的是哪一行）。
fn feature_line<'a>(
    r: &FeatureRow<'_>,
    wins: &[u32],
    mode: TableMode,
    metric: Metric,
    w: &Widths,
    tr: &Trend,
    idx: usize,
    hovered: bool,
) -> Element<'a, Msg> {
    let h = r.head();
    let muted = h.not_implemented() || !r.enabled();
    let mut line = row![
        fcell(nowrap(h.name_cn.clone(), 11.0, if muted { crate::ui::pal::pend() } else { crate::ui::pal::txt() }), w.name, false),
        sep(false),
        fcell(nowrap(h.key.clone(), 10.0, crate::ui::pal::dim()), w.key, false),
        sep(true),
    ]
    .height(Length::Fixed(ROW_H));
    for win in wins {
        line = line.push(window_group(r.window(*win), mode, metric, w, tr)).push(sep(true));
    }
    let (st, sc) = row_status(r);
    line = line
        .push(fcell(nowrap(h.unit.clone(), 10.0, crate::ui::pal::dim()), w.unit, false))
        .push(sep(false))
        .push(fcell(layer_badges(h), w.layer, false))
        .push(sep(false))
        .push(fcell(nowrap(st, 10.0, sc), w.status, false))
        .push(sep(false));
    // 悬停整行高亮用分节色（比网格的选中色轻）；斑马纹与 ui::grid 同一处定义
    let bg = if hovered { Some(crate::ui::pal::band()) } else { crate::ui::grid::row_tint(idx, false) };
    let key = h.key.clone();
    iced::widget::mouse_area(tinted(line, bg).width(Length::Fixed(w.total(mode, wins.len()))))
        .on_enter(Msg::HoverIn(key.clone()))
        .on_exit(Msg::HoverOut(key))
        .into()
}

/// 阶段分节条（横贯整表）：标题 + 本阶段「变大 / 变小」的格子数（一眼看这一段整体往哪边走）。
fn stage_band<'a>(t: String, c: Color, width: f32, rows: &[FeatureRow<'_>], tr: &Trend) -> container::Container<'a, Msg> {
    let mut r = row![nowrap(t, 12.0, c)].spacing(10).align_y(iced::Alignment::Center);
    if tr.past.is_some() {
        let (mut up, mut down) = (0usize, 0usize);
        for s in rows.iter().flat_map(|r| r.slots.iter()) {
            match tr.level(s) {
                l if l > 0 => up += 1,
                l if l < 0 => down += 1,
                _ => {}
            }
        }
        r = r
            .push(nowrap(format!("{}：", tr.label), 10.0, crate::ui::pal::dim()))
            .push(nowrap(format!("▲ {up}"), 11.0, crate::ui::pal::up()))
            .push(nowrap(format!("▼ {down}"), 11.0, crate::ui::pal::down()));
    }
    // 与 ui::grid 的组头同一套底色 / 高度 / 内边距（docs/35 §16.5 第 2 项）
    crate::ui::grid::group_band(r, width)
}

/// 通过当前筛选的特征：任一窗口通过筛选即显示整行；未启用的按开关藏起来。
fn visible<'m>(m: &'m Matrix, v: &ViewState, stage: &str) -> Vec<FeatureRow<'m>> {
    m.features_in(stage)
        .into_iter()
        .filter(|r| (v.show_disabled || r.enabled()) && r.slots.iter().any(|s| v.passes(s)))
        .collect()
}

/// 表格的窗口列：可见行（全部阶段）的窗口并集，升序。固定表头与所有行共用这一套。
fn table_windows(rows: &[FeatureRow<'_>]) -> Vec<u32> {
    let mut w: Vec<u32> = rows.iter().flat_map(|r| r.slots.iter().map(|s| s.window_ms)).collect();
    w.sort_unstable();
    w.dedup();
    w
}

/// 表格上方一行：完整 / 透视、透视的指标、图例、列宽。
fn table_controls<'a>(v: &ViewState) -> Element<'a, Msg> {
    let mut r = row![text("展示 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())]
        .spacing(4)
        .align_y(iced::Alignment::Center);
    for t in TableMode::ALL {
        r = r.push(chip(t.label().into(), v.table == t, Msg::SetTable(t)));
    }
    if v.table == TableMode::Pivot {
        r = r.push(text("　指标 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
        for mt in Metric::ALL {
            r = r.push(chip(mt.label().into(), v.metric == mt, Msg::SetMetric(mt)));
        }
    }
    r = r.push(text("　").size(crate::ui::text::s_small()));
    let legend: Element<'a, Msg> = match (v.table, v.metric) {
        (TableMode::Full, _) | (TableMode::Pivot, Metric::Value) => {
            let mut l = row![text("色条 = 质量：").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim())].spacing(4);
            for (q, name) in [
                ("GOOD", "良好"),
                ("DEGRADED", "降级"),
                ("STALE", "过期"),
                ("INVALID", "无效"),
                ("UNAVAILABLE", "不可用"),
            ] {
                l = l.push(text(format!("■{name}")).size(crate::ui::text::s_meta()).color(qcolor(q)));
            }
            l.push(text("　值前 ≈ = 降级（估算）、! = 无效　悬停看原因").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim())).into()
        }
        (TableMode::Pivot, Metric::Z) => dim("蓝 = 低于常态，红 = 高于常态，颜色越深偏离越大（|z| ≥ 3 封顶）".into()),
        (TableMode::Pivot, Metric::Pct) => dim("蓝 = 处在历史低位，红 = 处在历史高位，50% 附近不着色".into()),
        (TableMode::Pivot, Metric::Quality) => dim("格子按质量着色，写的是原因；悬停看原文".into()),
        (TableMode::Pivot, Metric::Change) => dim("与 N 秒前相比 z 的变化：绿 ▲ 变大，红 ▼ 变小；箭头个数 = 达到的档位，底色越深档位越高".into()),
    };
    r = r.push(legend);
    r = r.push(text("　列宽：拖表头分隔线调整，双击某条分隔线 = 这一列按当前内容重新自适应").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
    r = r.push(chip("全部重新自适应".into(), false, Msg::ResetWidths));
    let trend = row![
        text("涨跌 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
        text("与").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        iced::widget::text_input("5s", &v.trend_text)
            .on_input(Msg::TrendLookback)
            .size(crate::ui::text::s_small())
            .width(Length::Fixed(56.0)),
        text("前相比 z 的变化，|Δz| 达到").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);
    // 五档阈值：第 N 档达到就画 N 个箭头；空着 = 不用这一档（要从第 1 档起按顺序填）
    let mut levels = row![].spacing(4).align_y(iced::Alignment::Center);
    for i in 0..super::feature_matrix::TREND_LEVELS {
        let on = i < v.ths.len();
        levels = levels
            .push(nowrap("▲".repeat(i + 1), 10.0, if on { crate::ui::pal::up() } else { crate::ui::pal::pend() }))
            .push(
                iced::widget::text_input("不用", &v.th_text[i])
                    .on_input(move |t| Msg::TrendTh(i, t))
                    .size(crate::ui::text::s_small())
                    .width(Length::Fixed(46.0)),
            );
    }
    let trend = trend.push(levels).push(
        text("（第 N 档达到画 N 个箭头，空着的档不用；绿涨红跌，只说变大变小、不说好坏；没有 z 的特征不画）")
            .size(crate::ui::text::s_meta())
            .color(crate::ui::pal::dim()),
    );
    let mut col = column![r.wrap(), trend.wrap()].spacing(4);
    if !v.trend_err.is_empty() {
        col = col.push(text(v.trend_err.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::bad()));
    }
    col.into()
}

/// 筛选条。四个维度 + 清除（docs/31 §8.1：可按阶段/类别/状态/市场筛）。
fn filter_bar<'a>(m: &Matrix, v: &ViewState) -> Element<'a, Msg> {
    let mut b = column![].spacing(3);

    let mut r1 = row![text("质量 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].spacing(4);
    for q in QualityFilter::ALL {
        r1 = r1.push(chip(q.label().into(), v.quality == q, Msg::SetQuality(q)));
    }
    r1 = r1.push(text("　进度 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
    for s in StatusFilter::ALL {
        r1 = r1.push(chip(s.label().into(), v.status == s, Msg::SetStatus(s)));
    }
    let hidden = m.features().iter().filter(|r| !r.enabled()).count();
    if hidden > 0 {
        r1 = r1.push(text("　").size(crate::ui::text::s_small()));
        r1 = r1.push(chip(
            format!("显示未启用（{hidden}）"),
            v.show_disabled,
            Msg::ToggleShowDisabled,
        ));
    }
    if v.any_filter() {
        r1 = r1.push(text("　").size(crate::ui::text::s_small()));
        r1 = r1.push(chip("✕ 清筛选".into(), false, Msg::ClearFilters));
    }
    b = b.push(r1.align_y(iced::Alignment::Center));

    let mut r2 = row![text("阶段 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].spacing(4);
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
    let mut r3 = row![text("市场 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].spacing(4);
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
        let mut r4 = row![text("类别 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].spacing(4);
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
        crate::ui::pal::ok()
    } else if frac >= 0.5 {
        crate::ui::pal::warn()
    } else {
        crate::ui::pal::bad()
    };
    let mut b = column![].spacing(3);
    b = b.push(row![
        text(if frac >= 0.8 { "● " } else { "○ " }).size(crate::ui::text::s_section()).color(c),
        text(format!(
            "{usable}/{} 个 slot 处于可下单质量（{:.0}%）",
            m.total_slots,
            frac * 100.0
        ))
        .size(crate::ui::text::s_body())
        .color(c),
        text(format!(
            "　已登记·待实现 {}　市场状态 {}",
            m.not_implemented(),
            m.regime
        ))
        .size(crate::ui::text::s_small())
        .color(crate::ui::pal::dim()),
    ]);
    // 质量分布：一行里写清「降级的是多数还是少数」。
    let mut qr = row![text("质量分布 ").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim())].spacing(6);
    for (q, n) in m.quality_counts() {
        qr = qr.push(text(format!("{q}×{n}")).size(crate::ui::text::s_meta()).color(qcolor(&q)));
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

/// 特征矩阵（表体）：七阶段纵向分节。
///
/// 阶段标题可以点击折叠 / 展开；阶段里有异常窗口（质量不是 GOOD，且已实现、已启用）时标题变黄、
/// 写出个数——原「实时向量」视图的两个功能，2026-09-29 合并进来（两个视图列、行、筛选都一样）。
fn matrix_view<'a>(m: &Matrix, v: &ViewState, wins: &[u32], w: &Widths, tr: &Trend) -> Element<'a, Msg> {
    let total = w.total(v.table, wins.len());
    let mut b = column![].spacing(0).width(Length::Fixed(total));
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
        let bad = rows
            .iter()
            .filter(|r| r.enabled() && r.slots.iter().any(|s| s.abnormal() && !s.not_implemented()))
            .count();
        let collapsed = v.is_collapsed(key);
        let head = format!(
            "{} {label} · {} 条特征（全部窗口良好 {good}）{}",
            if collapsed { "▸" } else { "▾" },
            rows.len(),
            if bad > 0 { format!("　有异常窗口 {bad}") } else { String::new() },
        );
        b = b.push(
            button(stage_band(head, if bad > 0 { crate::ui::pal::warn() } else { crate::ui::pal::head() }, total, &rows, tr))
                .padding(iced::Padding::ZERO)
                .style(|t, st| crate::style::button::modifier(t, st, false))
                .on_press(Msg::ToggleStage(key.to_string())),
        );
        if collapsed {
            shown += rows.len();
            continue;
        }
        for (i, r) in rows.iter().enumerate() {
            shown += 1;
            let hov = v.hover.as_deref() == Some(r.head().key.as_str());
            b = b.push(feature_line(r, wins, v.table, v.metric, w, tr, i, hov));
        }
    }
    let n = m.features().len();
    if shown == 0 {
        b = b.push(
            container(nowrap(
                "当前筛选下没有任何特征——点「✕ 清筛选」或「显示未启用」".into(),
                11.0,
                crate::ui::pal::warn(),
            ))
            .padding(crate::ui::metrics::pad2(2, 2)),
        );
    } else if shown < n {
        // 筛过之后忘了筛，会把「矩阵里只有 3 条」当成引擎的问题。
        b = b.push(
            container(nowrap(
                format!("当前显示 {shown} 条特征，共 {n} 条（其余被筛选或未启用）"),
                10.0,
                crate::ui::pal::dim(),
            ))
            .padding(crate::ui::metrics::pad2(2, 2)),
        );
    }
    b.into()
}

/// 引擎健康：簿、检出、缓冲容量、刷新率。
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
    let dc = if m.saturated_windows > 0 { crate::ui::pal::warn() } else { crate::ui::pal::dim() };
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
        .size(crate::ui::text::s_small())
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
        b = b.push(text("没有任何缓冲拒收过样本").size(crate::ui::text::s_small()).color(crate::ui::pal::ok()));
    } else {
        b = b.push(row![
            cell("可观测量".into(), 150.0, crate::ui::pal::dim()),
            cell("窗口".into(), 60.0, crate::ui::pal::dim()),
            numc("占用".into(), 130.0, crate::ui::pal::dim()),
            numc("累计拒收".into(), 100.0, crate::ui::pal::dim()),
            cell("".into(), 70.0, crate::ui::pal::dim()),
        ].spacing(4));
        for w in rows.iter().take(12) {
            b = b.push(row![
                cell(w.observable.clone(), 150.0, crate::ui::pal::txt()),
                cell(
                    if w.window_ms == 0 { "瞬时".into() } else { format!("{}s", w.window_ms / 1000) },
                    60.0,
                    crate::ui::pal::dim()
                ),
                numc(format!("{}/{}", w.len, w.capacity), 130.0, crate::ui::pal::dim()),
                numc(w.drops.to_string(), 100.0, if w.saturated { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
                cell(
                    if w.saturated { "⚠ 当前饱和".into() } else { String::new() },
                    70.0,
                    crate::ui::pal::warn()
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
    let mut r = row![text("时间窗口 ").size(crate::ui::text::s_body()).color(crate::ui::pal::head())]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    match &v.win_edit {
        None => {
            let g = cw.global.as_deref().map_or_else(
                || "默认（各特征用字典窗口）".to_string(),
                super::feature_matrix_readout::format_windows,
            );
            r = r.push(text(format!("全局：{g}")).size(crate::ui::text::s_small()).color(crate::ui::pal::txt()));
            if !cw.overrides.is_empty() {
                r = r.push(text(format!("单条覆盖 {} 条", cw.overrides.len())).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
            }
            r = r.push(chip("修改…".into(), false, Msg::WinOpen));
            if fixed > 0 {
                r = r.push(
                    text(format!("瞬时量与窗口固定的 {fixed} 条不受影响；单条覆盖在「自定义…」里逐条设"))
                        .size(crate::ui::text::s_meta())
                        .color(crate::ui::pal::dim()),
                );
            }
            b = b.push(r);
        }
        Some(w) => {
            r = r.push(text("全局：").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
            if w.list.is_empty() {
                r = r.push(text("默认（各特征用字典窗口）").size(crate::ui::text::s_small()).color(crate::ui::pal::txt()));
            }
            for x in &w.list {
                r = r.push(chip(format!("{} ✕", format_window(*x)), true, Msg::WinRemove(*x)));
            }
            r = r
                .push(
                    iced::widget::text_input("加窗口，如 10s 或 1m, 15m", &w.input)
                        .on_input(Msg::WinInput)
                        .on_submit(Msg::WinAdd)
                        .size(crate::ui::text::s_small())
                        .width(Length::Fixed(200.0)),
                )
                .push(chip("添加".into(), false, Msg::WinAdd));
            b = b.push(r.wrap());
            let mut r2 = row![text("常用 ").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim())]
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
                .push(text("　").size(crate::ui::text::s_meta()))
                .push(chip("✔ 应用并重启引擎".into(), true, Msg::WinApply))
                .push(chip("取消".into(), false, Msg::WinClose));
            b = b.push(r2.wrap());
            if !w.err.is_empty() {
                b = b.push(text(w.err.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::bad()));
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
    let mut r = row![text("启用集 ").size(crate::ui::text::s_body()).color(crate::ui::pal::head())]
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
        r = r.push(text("　选择集").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
        r = r.push(
            iced::widget::pick_list(names, active_preset.clone(), Msg::PresetApply)
                .placeholder("选一个直接应用…")
                .text_size(11)
                .padding(crate::ui::metrics::pad2(0, 2)),
        );
    }
    if m.present {
        let f = m.features();
        let on = f.iter().filter(|r| r.enabled()).count();
        r = r.push(text(format!("引擎当前启用 {on}/{} 条特征", f.len())).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
    }
    if mode == "all" {
        r = r.push(text("全开超出常驻延迟 / 内存预算，适合临时观察").size(crate::ui::text::s_meta()).color(crate::ui::pal::warn()));
    }
    r.into()
}

fn prior_tag(p: &str) -> (&'static str, Color) {
    match p {
        "gross" => ("毛", crate::ui::pal::ok()),
        "falsified" => ("证伪", crate::ui::pal::bad()),
        "untested" => ("未检验", crate::ui::pal::dim()),
        _ => ("", crate::ui::pal::dim()),
    }
}

/// 选择集栏：已存的选择集（点名字载入编辑）+ 名称 / 备注 + 保存 / 更新 / 删除 / 导出 / 导入。
fn preset_bar<'a>(p: &super::feature_matrix::Picker) -> Element<'a, Msg> {
    let presets = super::feature_presets::list();
    let mut b = column![].spacing(5);

    let mut lr = row![text("选择集 ").size(crate::ui::text::s_body()).color(crate::ui::pal::head())]
        .spacing(5)
        .align_y(iced::Alignment::Center);
    if presets.is_empty() {
        lr = lr.push(text("还没有保存过选择集——勾选好后在下面起个名字「另存为新选择集」").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
    }
    for pr in &presets {
        let editing = p.editing.as_deref() == Some(pr.name.as_str());
        let label = format!("{}（{}）", pr.name, pr.keys.len());
        lr = lr.push(chip(label, editing, Msg::PresetLoad(pr.name.clone())));
    }
    lr = lr.push(text("　").size(crate::ui::text::s_small()));
    lr = lr.push(chip("＋ 新建".into(), p.editing.is_none(), Msg::PresetNew));
    lr = lr.push(chip("导入…".into(), false, Msg::PresetImport));
    // 选择集多了会超出一行：换行排，不横向溢出
    b = b.push(lr.wrap());

    let status = match &p.editing {
        Some(n) if p.dirty() => (format!("正在编辑「{n}」· 有未保存的修改"), crate::ui::pal::warn()),
        Some(n) => (format!("正在编辑「{n}」· 已保存"), crate::ui::pal::ok()),
        None => ("新选择集（未保存）".to_string(), crate::ui::pal::dim()),
    };
    let mut er = row![
        text(status.0).size(crate::ui::text::s_small()).color(status.1),
        iced::widget::text_input("名称", &p.name)
            .on_input(Msg::PresetName)
            .size(crate::ui::text::s_small())
            .width(Length::Fixed(180.0)),
        iced::widget::text_input("备注（可选）", &p.note)
            .on_input(Msg::PresetNote)
            .size(crate::ui::text::s_small())
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
        let c = if note.starts_with('✗') { crate::ui::pal::bad() } else { crate::ui::pal::dim() };
        b = b.push(text(note).size(crate::ui::text::s_meta()).color(c));
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
            text(format!("自定义启用集：已选 {}/{total} 条特征", p.selected.len())).size(crate::ui::text::s_emph()).color(crate::ui::pal::head()),
            text(format!("（其中 {extra} 条不在默认集）")).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    b = b.push(preset_bar(p));
    b = b.push(
        row![
            text("时间窗口").size(crate::ui::text::s_body()).color(crate::ui::pal::head()),
            text("全局").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
            iced::widget::text_input("空 = 字典默认；如 1m 5m 15m", &p.global_text)
                .on_input(Msg::PickGlobalWindows)
                .size(crate::ui::text::s_small())
                .width(Length::Fixed(220.0)),
            text("每条特征的「窗口」列可单独覆盖；两者都随选择集保存，应用时一起写入").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    if let Err(e) = p.windows() {
        b = b.push(text(format!("✗ {e}")).size(crate::ui::text::s_meta()).color(crate::ui::pal::bad()));
    }
    b = b.push(
        row![
            iced::widget::text_input("搜索键名或中文名…", &p.search)
                .on_input(Msg::PickSearch)
                .size(crate::ui::text::s_small())
                .width(Length::Fixed(260.0)),
            chip("全选".into(), false, Msg::PickBulk(None, Bulk::All)),
            chip("全不选".into(), false, Msg::PickBulk(None, Bulk::None)),
            chip("恢复默认".into(), false, Msg::PickBulk(None, Bulk::Default)),
            text("　").size(crate::ui::text::s_small()),
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
            pc(text("").size(crate::ui::text::s_meta()), 1),
            pc(text("特征").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), P_NAME),
            pc(text("先验").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 2),
            pc(text("级别").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 2),
            pc(text("数据层").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), P_LAYER),
            pc(text("默认窗口").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 4),
            pc(text("窗口覆盖").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 4),
            pc(text("默认集").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 2),
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
                    .size(crate::ui::text::s_meta())
                    .into()
            } else {
                text("固定").size(crate::ui::text::s_meta()).color(crate::ui::pal::pend()).into()
            };
            let lat = match h.latency.as_str() {
                "hot" => "逐事件".to_string(),
                "warm" => "250ms 节拍".to_string(),
                "offline_only" => "仅离线".to_string(),
                x => x.to_string(),
            };
            b = b.push(
                row![
                    pc(iced::widget::checkbox(on).on_toggle(move |x| Msg::PickToggle(k.clone(), x)).size(crate::ui::text::s_section()), 1),
                    pc(
                        column![
                            text(h.name_cn.clone()).size(crate::ui::text::s_small()).color(if h.not_implemented() { crate::ui::pal::pend() } else { crate::ui::pal::txt() }),
                            text(h.key.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
                        ],
                        P_NAME
                    ),
                    pc(text(pt).size(crate::ui::text::s_meta()).color(pcol), 2),
                    pc(text(lat).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 2),
                    pc(layer_badges(h), P_LAYER),
                    pc(text(wins).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), 4),
                    pc(over, 4),
                    pc(text(if h.default_on { "✓" } else { "" }).size(crate::ui::text::s_meta()).color(crate::ui::pal::ok()), 2),
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
/// 数据源栏：收起时一行（现在读的是什么 + 回放状态），展开是共用数据选择组件 + 回放控制。
fn source_bar<'a>(s: &super::feature_source::View, hosted: bool) -> Element<'a, Msg> {
    use super::feature_source::{Pace, SourceMsg};
    let mut head = row![
        text("数据源").size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        text(s.reading.clone()).size(crate::ui::text::s_small()).color(if s.is_replay { crate::ui::pal::warn() } else { crate::ui::pal::txt() }),
        chip(if s.open { "▴ 收起".into() } else { "▾ 选择数据".into() }, s.open, Msg::Source(SourceMsg::Toggle)),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if s.running {
        head = head
            .push(chip(
                if s.paused { "▶ 继续".into() } else { "⏸ 暂停".into() },
                s.paused,
                Msg::Source(SourceMsg::Pause),
            ))
            .push(chip("■ 停止回放".into(), false, Msg::Source(SourceMsg::Stop)));
    }
    let mut b = column![head].spacing(5);
    if !s.status.is_empty() {
        b = b.push(text(s.status.clone()).size(crate::ui::text::s_meta()).color(if s.paused {
            crate::ui::pal::warn()
        } else if s.running {
            crate::ui::pal::ok()
        } else {
            crate::ui::pal::dim()
        }));
    }
    if s.open {
        let opts = super::feature_source::pick_opts();
        b = b.push(if hosted {
            super::inspector_props::hint("数据选择")
        } else {
            super::data_picker_view::view(&s.pick, &opts).map(|m| Msg::Source(SourceMsg::Data(m)))
        });
        if s.pick.local_key().is_some() {
            let mut pr = row![text("回放速度").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].spacing(4).align_y(iced::Alignment::Center);
            for p in Pace::ALL {
                pr = pr.push(chip(p.label().into(), s.pace == p, Msg::Source(SourceMsg::Pace(p))));
            }
            pr = pr.push(text("　").size(crate::ui::text::s_small())).push(chip(
                if s.running { "↻ 重新开始".into() } else { "▶ 开始回放".into() },
                false,
                Msg::Source(SourceMsg::Start),
            ));
            // 暂停 / 继续：只在有回放在跑时可点
            pr = pr.push(if s.running {
                chip(
                    if s.paused { "▶ 继续回放".into() } else { "⏸ 暂停回放".into() },
                    s.paused,
                    Msg::Source(SourceMsg::Pause),
                )
            } else {
                text("⏸ 暂停回放").size(crate::ui::text::s_small()).color(crate::ui::pal::pend()).into()
            });
            b = b.push(pr).push(
                text(
                    "回放由特征引擎直接读数据商接口（不转格式），启用集与时间窗口用本面板的配置；\
                     写自己的一组文件，常驻引擎照常运行。选回 B1 即切回实时。",
                )
                .size(crate::ui::text::s_meta())
                .color(crate::ui::pal::dim()),
            );
        }
    }
    if !s.note.is_empty() {
        b = b.push(text(s.note.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::warn()));
    }
    container(b).padding(crate::ui::metrics::pad2(1, 2)).into()
}

fn engine_bar<'a>() -> Element<'a, Msg> {
    let note = super::feature_matrix::engine_note();
    let mut r = row![text("特征引擎 ").size(crate::ui::text::s_body()).color(crate::ui::pal::head())]
        .spacing(6)
        .align_y(iced::Alignment::Center);
    match super::feature_matrix::engine_state() {
        // 轮询还没出第一轮：别猜状态，也别给按钮——猜错了按钮就是反的
        None => r = r.push(text("查询中…").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())),
        Some(st) if st.active => {
            r = r
                .push(text("● 运行中").size(crate::ui::text::s_small()).color(crate::ui::pal::ok()))
                .push(
                    text(format!("已运行 {}", super::svcctl::fmt_dur(st.uptime_secs)))
                        .size(crate::ui::text::s_meta())
                        .color(crate::ui::pal::dim()),
                )
                .push(chip("■ 停止".into(), false, Msg::Engine("stop")));
        }
        Some(_) => {
            r = r
                .push(text("○ 已停止").size(crate::ui::text::s_small()).color(crate::ui::pal::warn()))
                .push(chip("▶ 启动".into(), false, Msg::Engine("start")));
        }
    }
    if !note.is_empty() {
        r = r.push(text(note).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
    }
    r.into()
}

///
/// `hosted` = 这个面板的可编辑属性正显示在检查器里（见 `ws::inspector_props`）。
pub fn pane_body<'a>(hosted: bool) -> Element<'a, Msg> {
    let m = ro::snapshot();
    let v = super::feature_matrix::state();
    let mut b = column![sec(
        "订单流与市场微观结构 · 特征矩阵（docs/31 · 感知层·非交易信号）".into()
    )]
    .spacing(6)
    .padding(crate::ui::metrics::space(3));

    // 视图切换 + 收起控件
    let mut vr = row![].spacing(4);
    for view in View::ALL {
        vr = vr.push(chip(view.label().into(), v.view == view, Msg::SetView(view)));
    }
    if v.view != View::Engine && v.picker.is_none() {
        vr = vr.push(text("　").size(crate::ui::text::s_small())).push(chip(
            if v.fold_controls { "▾ 展开控件".into() } else { "▴ 收起控件".into() },
            v.fold_controls,
            Msg::ToggleFoldControls,
        ));
    }
    b = b.push(vr.align_y(iced::Alignment::Center));
    let src = super::feature_source::view();
    b = b.push(source_bar(&src, hosted));
    let fold = v.fold_controls && v.view != View::Engine && v.picker.is_none();
    if !fold {
        b = b.push(engine_bar());
        b = b.push(mode_bar(&m));
        b = b.push(window_bar(&m, &v));
    }

    if let Some(p) = &v.picker {
        if !m.present {
            return b
                .push(text("还没有快照：先启动一次引擎，面板才拿得到完整的特征清单").size(crate::ui::text::s_small()).color(crate::ui::pal::warn()))
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
                     引擎常驻时由 sidecar::SidecarWriter 每 500ms 写一次。面板只读，不连交易所。\n\
                     选了本地数据：点上面的「▶ 开始回放」，回放跑起来后这里就有快照。",
                    ro::board_path().display()
                ))
                .size(crate::ui::text::s_small())
                .color(crate::ui::pal::dim()),
            )
            .into();
    }

    if !src.is_replay && super::feature_matrix::engine_state().is_some_and(|st| !st.active) {
        // 停了之后旁路文件还在：不说一声，人会把最后一张快照当成现在
        b = b.push(
            text("引擎已停止：下面是停止前的最后一次快照，不再更新")
                .size(crate::ui::text::s_small())
                .color(crate::ui::pal::warn()),
        );
    }
    if v.view == View::Chart {
        // 不套外层滚动：卡片网格要拿到面板的真实高度（两行排满一屏、卡片内各自滚动）
        b = b.push(top_bar(&m)).push(super::chart_params_view::view(&m, hosted));
        return container(b.width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
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

    // 表头固定在滚动区之外（横向跟着表体滚：表体滚动时回传偏移，上层把表头滚到同一位置）；
    // 表体双向滚动，横向滚动条在表格区域底部。表头与所有行共用同一套窗口列与列宽。
    let all_rows: Vec<FeatureRow<'_>> = Matrix::STAGES.iter().flat_map(|(k, _)| visible(&m, &v, k)).collect();
    let wins = table_windows(&all_rows);
    let w = widths(&all_rows, &wins, &v);
    let tr = Trend::new(&m, &v);
    let body = matrix_view(&m, &v, &wins, &w, &tr);
    let head = scrollable(table_header(&wins, v.table, v.metric, &w))
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .id(super::feature_matrix::HEAD_ID);
    let body = scrollable(body)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new(),
            horizontal: scrollable::Scrollbar::new(),
        })
        .id(super::feature_matrix::BODY_ID)
        .on_scroll(|vp| Msg::TableScrolled(vp.absolute_offset().x))
        .width(Length::Fill)
        .height(Length::Fill);
    column![
        b,
        container(head).padding(crate::ui::metrics::pad2(0, 3)),
        container(body)
            .padding(iced::Padding { top: 0.0, right: 10.0, bottom: 4.0, left: 10.0 })
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
        // 徽标就全变成兜底色。用**兜底色本身**当判据——落到 crate::ui::pal::dim() 即视为未覆盖。
        for l in ENGINE_LAYERS {
            let c = layer_color(l);
            assert!(
                (c.r - crate::ui::pal::dim().r).abs() + (c.g - crate::ui::pal::dim().g).abs() + (c.b - crate::ui::pal::dim().b).abs() > 0.05,
                "数据层 `{l}` 落到了兜底色——配色表里的串与引擎的 DataLayer::as_str() 不一致"
            );
        }
        // 没登记过的串仍然要走兜底而不是 panic。
        assert_eq!(layer_color("不存在的层").r, crate::ui::pal::dim().r);
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
