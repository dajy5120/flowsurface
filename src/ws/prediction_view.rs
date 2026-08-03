//! 预测市场 pane 的视图渲染（docs/19）。
//!
//! **独立新增面板**（`Content::PredictionBoard`）——不改任何既有面板；数据走
//! [`super::prediction_readout`] 旁路快照。只渲染、不发消息，对消息类型 `M` 泛型。
//!
//! 布局：合规/非信号横幅 → 市场列表（Yes 概率/成交/流动性/关注标签）+ AI 决策支持行（估计 vs 市场·edge·置信度）。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::prediction_readout::{CalibrationView, MarketRow, PredictionReadout};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_GREEN: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_RED: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);

fn sign_c(v: f64) -> Color {
    if v >= 0.0 {
        C_GREEN
    } else {
        C_RED
    }
}
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    text(title.to_string()).size(14).color(C_HEAD).into()
}
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(11).color(c)).width(Length::Fixed(w)).into()
}
fn pct(p: Option<f64>) -> String {
    p.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "—".into())
}
fn usd(v: f64) -> String {
    if v >= 1e6 {
        format!("${:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("${:.0}k", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

fn brier(b: Option<f64>) -> String {
    b.map(|x| format!("{x:.3}")).unwrap_or_else(|| "—".into())
}

/// AI 校准追踪摘要区（docs/19 §5）：AI Brier vs 市场 Brier + 校准曲线（越低越好）。
fn calib_block<'a, M: 'a>(c: &CalibrationView) -> Element<'a, M> {
    let mut col = column![sec("AI 校准追踪 · Brier 越低越准（唯有 AI<市场 才可能有边际）")].spacing(3);

    if c.n_resolved == 0 {
        col = col.push(
            text(format!(
                "已记录 {} 条估计 · 待市场结算积累样本（现 0 已结算）——夜跑 --resolve 自动回填",
                c.n_total
            ))
            .size(11)
            .color(C_DIM),
        );
        return col.into();
    }

    let verdict = if c.ai_beats_market {
        ("✅ AI 胜过市场", C_GREEN)
    } else {
        ("❌ AI 未胜市场（分歧=噪声·勿据以下注）", C_RED)
    };
    col = col.push(
        row![
            cell(format!("已结算 {}/{}", c.n_resolved, c.n_total), 100.0, C_TXT),
            cell(format!("AI Brier {}", brier(c.ai_brier)), 110.0, C_TXT),
            cell(format!("市场 {}", brier(c.market_brier)), 100.0, C_DIM),
            cell(verdict.0.into(), 240.0, verdict.1),
        ]
        .spacing(4),
    );

    // 校准曲线：预测区间 vs 实际发生率（完美=对角线）
    if !c.bins.is_empty() {
        col = col.push(text("校准曲线（区间 · 样本 · 均预测 → 实际）").size(10).color(C_HEAD));
        for b in &c.bins {
            let gap = (b.mean_pred - b.actual).abs();
            let cc = if gap <= 0.1 { C_GREEN } else { C_GOLD };
            col = col.push(
                row![
                    cell(format!("[{:.1},{:.1})", b.lo, b.hi), 80.0, C_DIM),
                    cell(format!("n={}", b.n), 46.0, C_DIM),
                    cell(format!("预{:.0}%", b.mean_pred * 100.0), 60.0, C_DIM),
                    cell(format!("实{:.0}%", b.actual * 100.0), 60.0, cc),
                ]
                .spacing(4),
            );
        }
    }
    col.into()
}

fn market_block<'a, M: 'a>(m: &MarketRow) -> Element<'a, M> {
    let mark = if m.watch { "★" } else { " " };
    let mut col = column![
        row![
            cell(mark.into(), 16.0, C_GOLD),
            cell(format!("Yes {}", pct(m.yes_prob)), 70.0, C_TXT),
            cell(format!("成交 {}", usd(m.volume)), 90.0, C_DIM),
            cell(format!("流动 {}", usd(m.liquidity)), 90.0, C_DIM),
            cell(m.tags.join("·"), 260.0, C_DIM),
        ]
        .spacing(4),
        text(m.question.clone()).size(11).color(C_TXT),
    ]
    .spacing(2);

    // AI 决策支持（若有）：估计 vs 市场·edge·置信度·推理（非投注信号）
    if let Some(ai) = &m.ai {
        col = col.push(
            row![
                cell("AI".into(), 26.0, C_HEAD),
                cell(format!("估计 {:.0}%", ai.ai_prob * 100.0), 78.0, C_TXT),
                cell(format!("市场 {}", pct(ai.market_prob)), 78.0, C_DIM),
                cell(
                    ai.edge
                        .map(|e| format!("Δ {:+.0}%", e * 100.0))
                        .unwrap_or_else(|| "Δ —".into()),
                    70.0,
                    ai.edge.map(sign_c).unwrap_or(C_DIM),
                ),
                cell(format!("置信 {}", ai.confidence), 80.0, C_DIM),
            ]
            .spacing(4),
        );
        if !ai.rationale.is_empty() {
            col = col.push(text(format!("  ↳ {}", ai.rationale)).size(10).color(C_DIM));
        }
    }
    col.into()
}

pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st: PredictionReadout = super::prediction_readout::snapshot();
    let mut body = column![].spacing(8).padding(10);

    body = body.push(sec("预测市场 · Polymarket（docs/19 · 决策支持·非投注信号）"));

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无快照——运行 `python -m factory.prediction.run [--ai]` 生成")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    body = body.push(
        text("⚠ 预测市场校准良好·AI 分歧仅信息性·默认只读不自动下注（合规：Polymarket 加密真金·美国受限）")
            .size(10)
            .color(C_GOLD),
    );
    if st.ai_enabled {
        body = body.push(
            text(format!("AI 决策支持已开 · 已分析 {} 个关注档市场", st.ai_analyzed))
                .size(10)
                .color(C_DIM),
        );
    }

    if let Some(c) = &st.calib {
        body = body.push(calib_block(c));
    }

    let watch_n = st.rows.iter().filter(|r| r.watch).count();
    body = body.push(
        text(format!("关注档 {}/{} · ★=流动+活跃+不确定", watch_n, st.rows.len()))
            .size(11)
            .color(C_HEAD),
    );

    // 「近乎已定」的市场（Yes ≥95% 或 ≤5%）没有分析价值，却因成交额大排在前面，
    // 实测把唯一 ★ 标的可分析市场淹没在 13+ 条已定市场里。
    // **重排而不隐藏**：可分析的提到前面，已定的移到分隔线之后——信息一条不少。
    // 判据用 yes_prob 而非匹配标签字符串（标签文案变了就失效）。
    let decided = |m: &MarketRow| m.yes_prob.is_some_and(|p| !(0.05..=0.95).contains(&p));
    let (settled, open_): (Vec<_>, Vec<_>) = st.rows.iter().partition(|m| decided(m));
    // 可分析的里面 ★ 关注档再提到最前
    let mut open_sorted = open_;
    open_sorted.sort_by_key(|m| !m.watch);
    for m in &open_sorted {
        body = body.push(market_block(m));
    }
    if !settled.is_empty() {
        body = body.push(
            text(format!(
                "── 以下 {} 个近乎已定（Yes ≥95% 或 ≤5%），无分析价值，仅列出 ──",
                settled.len()
            ))
            .size(10)
            .color(C_DIM),
        );
        for m in &settled {
            body = body.push(market_block(m));
        }
    }

    body = body.push(
        text(format!(
            "源 {} · 快照 {}{} · 刷新 {}",
            st.source,
            st.stamp,
            super::staleness::suffix(&st.stamp),
            st.refreshed
        ))
            .size(10)
            .color(C_DIM),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
