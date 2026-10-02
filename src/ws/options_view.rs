//! 期权/0DTE 回测 pane 的视图渲染（docs/18 P2）。
//!
//! **独立新增面板**（`Content::OptionsBoard`）——不改任何既有面板；数据走
//! [`super::options_readout`] 旁路快照。只渲染、不发消息，对消息类型 `M` 泛型。
//!
//! 布局：数据源横幅（合成/真实警示）→ 逐策略净 PnL + 探针 → 摩擦分解表。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::options_readout::{OptionsReadout, StrategyRow};

fn sign_c(v: f64) -> Color {
    if v >= 0.0 {
        crate::ui::pal::up()
    } else {
        crate::ui::pal::down()
    }
}
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    text(title.to_string()).size(crate::ui::text::s_section()).color(crate::ui::pal::head()).into()
}
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(crate::ui::text::s_small()).color(c)).width(Length::Fixed(w)).into()
}

fn strategy_block<'a, M: 'a>(r: &StrategyRow) -> Element<'a, M> {
    let gate = if r.gate_ok { "✅ 净>0" } else { "❌ 净≤0" };
    column![
        row![
            cell(r.code.clone(), 44.0, crate::ui::pal::warn()),
            cell(r.role.clone(), 80.0, crate::ui::pal::dim()),
            cell(format!("净 {:+.3}", r.net), 90.0, sign_c(r.net)),
            cell(format!("探针 {gate}", ), 90.0, if r.gate_ok { crate::ui::pal::up() } else { crate::ui::pal::down() }),
            cell(format!("成交 {}", r.n_fills), 70.0, crate::ui::pal::txt()),
        ]
        .spacing(4),
        // 摩擦分解（探针·docs/16 §5 净捕获 vs 摩擦）
        row![
            cell(format!("权利金 {:+.2}", r.premium), 100.0, crate::ui::pal::txt()),
            cell(format!("费 {:+.2}", r.fees), 70.0, crate::ui::pal::down()),
            cell(format!("价差 {:+.2}", r.spread_cost), 80.0, crate::ui::pal::down()),
            cell(format!("对冲 {:+.2}", r.hedge_cost), 80.0, crate::ui::pal::down()),
            cell(format!("结算 {}", crate::ui::fmt::sim(format!("{:+.2}", r.settle_pnl))), 80.0, sign_c(r.settle_pnl)),
            cell(format!("持仓残差 {}", crate::ui::fmt::sim(format!("{:+.2}", r.hedge_mkt_pnl))), 110.0, sign_c(r.hedge_mkt_pnl)),
        ]
        .spacing(4),
        text(r.desc.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
    ]
    .spacing(3)
    .into()
}

pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st: OptionsReadout = super::options_readout::snapshot();
    let mut body = column![].spacing(8).padding(crate::ui::metrics::space(3));

    body = body.push(sec("期权 / 0DTE 回测·探针（docs/18 · 不下真实单）"));

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无回测快照——运行 `python -m factory.options.run_backtest --strategy all` 生成")
                .size(crate::ui::text::s_small())
                .color(crate::ui::pal::dim()),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // 数据源横幅：合成数据显式警示（非决策依据）
    let synthetic = st.data_source != "真实";
    body = body.push(
        text(if synthetic {
            format!("⚠ 数据源={} · 数字仅供管道验证·非决策依据（真实回测须切数据商 provider）", st.data_source)
        } else {
            format!("数据源={}", st.data_source)
        })
        .size(crate::ui::text::s_small())
        .color(if synthetic { crate::ui::pal::warn() } else { crate::ui::pal::up() }),
    );

    // 逐策略
    for r in &st.rows {
        body = body.push(strategy_block(r));
    }

    body = body.push(
        text("研究结论：买方排除·卖方不立项·唯一可辩护=vol-order 试点(须先证净捕获>摩擦)")
            .size(crate::ui::text::s_meta())
            .color(crate::ui::pal::dim()),
    );
    body = body.push(
        text(format!(
            "快照 {}{} · 刷新 {}",
            st.stamp,
            super::staleness::suffix(&st.stamp),
            st.refreshed
        ))
        .color(if super::staleness::is_stale(&st.stamp) {
            crate::ui::pal::pend()
        } else {
            crate::ui::pal::dim()
        })
            .size(crate::ui::text::s_meta())
            .color(crate::ui::pal::dim()),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
