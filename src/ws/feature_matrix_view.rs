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
    FeatureMatrixMsg as Msg, QualityFilter, StatusFilter, View, ViewState,
};
use super::feature_matrix_readout::{self as ro, Matrix, Slot};

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
    container(r).width(Length::Fixed(130.0)).into()
}

/// 一行 slot。
fn slot_row<'a>(s: &Slot) -> Element<'a, Msg> {
    let pending = s.not_implemented();
    let name_c = if pending { C_PEND } else { C_TXT };
    // 待实现的行：状态列写明「已登记·待实现」，而不是留空或显示质量。
    // 留空的话它看起来就像一个「碰巧没值」的已实现特征。
    let (stat, stat_c) = if pending {
        ("已登记·待实现".to_string(), C_PEND)
    } else if s.reason.is_empty() {
        (s.quality.clone(), qcolor(&s.quality))
    } else {
        (format!("{} · {}", s.quality, s.reason), qcolor(&s.quality))
    };
    row![
        cell(s.name_cn.clone(), 120.0, name_c),
        cell(s.key.clone(), 165.0, C_DIM),
        cell(s.window_label(), 45.0, C_DIM),
        numc(num(s.value), 95.0, if pending { C_PEND } else { C_TXT }),
        numc(num(s.z), 62.0, C_DIM),
        numc(
            s.percentile.map_or_else(|| "—".into(), |p| format!("{:.0}%", p * 100.0)),
            48.0,
            C_DIM
        ),
        // 72 而不是 50：最长的单位是 `quote_ccy`，50px 装不下，
        // 溢出的字会压到右边的数据层徽标上（实机截图里 `quote_ccy Trades` 叠在一起）。
        cell(s.unit.clone(), 72.0, C_DIM),
        layer_badges(s),
        cell(stat, 210.0, stat_c),
    ]
    .spacing(4)
    .into()
}

fn header_row<'a>() -> Element<'a, Msg> {
    row![
        cell("特征".into(), 120.0, C_DIM),
        cell("键".into(), 165.0, C_DIM),
        cell("窗口".into(), 45.0, C_DIM),
        numc("值".into(), 95.0, C_DIM),
        numc("z".into(), 62.0, C_DIM),
        numc("分位".into(), 48.0, C_DIM),
        cell("单位".into(), 72.0, C_DIM),
        cell("数据层".into(), 130.0, C_DIM),
        cell("状态".into(), 210.0, C_DIM),
    ]
    .spacing(4)
    .into()
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

/// ① 特征矩阵：七阶段纵向分节。
fn matrix_view<'a>(m: &Matrix, v: &ViewState) -> Element<'a, Msg> {
    let mut b = column![].spacing(6);
    let mut shown = 0usize;
    for (key, label) in Matrix::STAGES {
        let rows: Vec<&Slot> = m.by_stage(key).into_iter().filter(|s| v.passes(s)).collect();
        if rows.is_empty() {
            continue;
        }
        let good = rows.iter().filter(|s| s.quality == "GOOD").count();
        b = b.push(sec(format!(
            "{label} · {} 条（可用 {good}）",
            rows.len()
        )));
        b = b.push(header_row());
        for s in &rows {
            shown += 1;
            b = b.push(slot_row(s));
        }
    }
    if shown == 0 {
        b = b.push(text("当前筛选下没有任何 slot——点「✕ 清筛选」").size(11).color(C_WARN));
    } else if v.any_filter() {
        // 筛过之后忘了筛，会把「矩阵里只有 3 条」当成引擎的问题。
        b = b.push(dim(format!(
            "当前筛选显示 {shown} 条，共 {} 条（已筛掉 {}）",
            m.slots.len(),
            m.slots.len().saturating_sub(shown)
        )));
    }
    b.into()
}

/// ② 实时向量：按阶段折叠，异常高亮。
///
/// 与 ① 的区别不是排版而是**用途**：① 是「这个引擎有哪些特征、各自什么状态」，
/// ② 是「此刻这个向量长什么样」。所以 ② 默认收起正常的阶段，只把异常摊开。
fn vector_view<'a>(m: &Matrix, v: &ViewState) -> Element<'a, Msg> {
    let mut b = column![].spacing(4);
    b = b.push(dim(format!(
        "FeatureVector @ {}（事件时钟）——点阶段名折叠/展开。异常质量高亮。",
        m.as_of
    )));
    for (key, label) in Matrix::STAGES {
        let rows: Vec<&Slot> = m.by_stage(key).into_iter().filter(|s| v.passes(s)).collect();
        if rows.is_empty() {
            continue;
        }
        let bad = rows.iter().filter(|s| s.abnormal() && !s.not_implemented()).count();
        let pend = rows.iter().filter(|s| s.not_implemented()).count();
        let collapsed = v.is_collapsed(key);
        let head = format!(
            "{} {label} · {} 条{}{}",
            if collapsed { "▸" } else { "▾" },
            rows.len(),
            if bad > 0 { format!("　异常 {bad}") } else { String::new() },
            if pend > 0 { format!("　待实现 {pend}") } else { String::new() },
        );
        b = b.push(
            button(text(head).size(12).color(if bad > 0 { C_WARN } else { C_HEAD }))
                .padding([1, 4])
                .style(|t, st| crate::style::button::modifier(t, st, false))
                .on_press(Msg::ToggleStage(key.to_string())),
        );
        if collapsed {
            continue;
        }
        for s in &rows {
            let c = if s.not_implemented() {
                C_PEND
            } else if s.abnormal() {
                qcolor(&s.quality)
            } else {
                C_TXT
            };
            b = b.push(row![
                cell(format!("　{}", s.name_cn), 130.0, c),
                cell(s.window_label(), 45.0, C_DIM),
                numc(num(s.value), 95.0, c),
                numc(num(s.z), 62.0, C_DIM),
                cell(
                    if s.not_implemented() {
                        "已登记·待实现".into()
                    } else if s.reason.is_empty() {
                        s.quality.clone()
                    } else {
                        format!("{} · {}", s.quality, s.reason)
                    },
                    220.0,
                    c
                ),
            ].spacing(4));
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

pub fn pane_body<'a>() -> Element<'a, Msg> {
    let m: Matrix = ro::snapshot();
    let v = super::feature_matrix::state();
    let mut b = column![sec(
        "订单流与市场微观结构 · 特征矩阵（docs/31 · 感知层·非交易信号）".into()
    )]
    .spacing(6)
    .padding(10);

    // 视图切换。
    let mut vr = row![].spacing(4);
    for view in View::ALL {
        vr = vr.push(chip(view.label().into(), v.view == view, Msg::SetView(view)));
    }
    b = b.push(vr.align_y(iced::Alignment::Center));

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

    b = b.push(top_bar(&m));
    if v.view != View::Engine {
        b = b.push(filter_bar(&m, &v));
    }
    b = b.push(match v.view {
        View::Matrix => matrix_view(&m, &v),
        View::Vector => vector_view(&m, &v),
        View::Engine => engine_view(&m),
    });

    container(scrollable(b)).width(Length::Fill).height(Length::Fill).into()
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
