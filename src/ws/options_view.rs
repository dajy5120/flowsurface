//! 期权/0DTE 回测 pane 的视图渲染（docs/18 P2）。
//!
//! **独立新增面板**（`Content::OptionsBoard`）——不改任何既有面板；数据走
//! [`super::options_readout`] 旁路快照。只渲染、不发消息，对消息类型 `M` 泛型。
//!
//! 布局：数据源横幅（合成/真实警示）→ 逐策略净 PnL + 探针 → 摩擦分解表。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::options_readout::{OptionsReadout, StrategyRow};

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

fn strategy_block<'a, M: 'a>(r: &StrategyRow) -> Element<'a, M> {
    let gate = if r.gate_ok { "✅ 净>0" } else { "❌ 净≤0" };
    column![
        row![
            cell(r.code.clone(), 44.0, C_GOLD),
            cell(r.role.clone(), 80.0, C_DIM),
            cell(format!("净 {:+.3}", r.net), 90.0, sign_c(r.net)),
            cell(format!("探针 {gate}", ), 90.0, if r.gate_ok { C_GREEN } else { C_RED }),
            cell(format!("成交 {}", r.n_fills), 70.0, C_TXT),
        ]
        .spacing(4),
        // 摩擦分解（探针·docs/16 §5 净捕获 vs 摩擦）
        row![
            cell(format!("权利金 {:+.2}", r.premium), 100.0, C_TXT),
            cell(format!("费 {:+.2}", r.fees), 70.0, C_RED),
            cell(format!("价差 {:+.2}", r.spread_cost), 80.0, C_RED),
            cell(format!("对冲 {:+.2}", r.hedge_cost), 80.0, C_RED),
            cell(format!("结算 {:+.2}", r.settle_pnl), 80.0, sign_c(r.settle_pnl)),
            cell(format!("持仓残差 {:+.2}", r.hedge_mkt_pnl), 110.0, sign_c(r.hedge_mkt_pnl)),
        ]
        .spacing(4),
        text(r.desc.clone()).size(10).color(C_DIM),
    ]
    .spacing(3)
    .into()
}

pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st: OptionsReadout = super::options_readout::snapshot();
    let mut body = column![].spacing(8).padding(10);

    body = body.push(sec("期权 / 0DTE 回测·探针（docs/18 · 不下真实单）"));

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无回测快照——运行 `python -m factory.options.run_backtest --strategy all` 生成")
                .size(11)
                .color(C_DIM),
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
        .size(11)
        .color(if synthetic { C_GOLD } else { C_GREEN }),
    );

    // 逐策略
    for r in &st.rows {
        body = body.push(strategy_block(r));
    }

    body = body.push(
        text("研究结论：买方排除·卖方不立项·唯一可辩护=vol-order 试点(须先证净捕获>摩擦)")
            .size(10)
            .color(C_DIM),
    );
    body = body.push(
        text(format!("快照 {} · 刷新 {}", st.stamp, st.refreshed))
            .size(10)
            .color(C_DIM),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
