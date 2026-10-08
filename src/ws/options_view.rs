//! 期权/0DTE 回测 pane 的视图渲染（docs/18 P2）。
//!
//! **独立新增面板**（`Content::OptionsBoard`）——不改任何既有面板；数据走
//! [`super::options_readout`] 旁路快照。
//!
//! 布局：数据源横幅（合成/真实警示）→ 逐策略表（ui::grid：净 PnL、探针、摩擦分解，
//! docs/35 §16.13 第 5 项）→ 各策略说明（逐条列在表下，不截断）。

use iced::widget::{column, container, text};
use iced::{Color, Element, Length};

use super::options_readout::{OptionsReadout, StrategyRow};
use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};

fn sign_c(v: f64) -> Color {
    if v >= 0.0 {
        crate::ui::pal::up()
    } else {
        crate::ui::pal::down()
    }
}
/// 大分区标题：上方留白 + 2px 实线（几个大功能之间要一眼分得开）。
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    crate::ui::widgets::major(title.to_string())
}
/// 表格单元格：`w` 是比例（原像素宽），整行撑满面板。
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(crate::ui::text::s_small()).color(c)).width(Length::FillPortion(w.round().max(1.0) as u16)).into()
}

thread_local! {
    static GRID: std::cell::RefCell<Option<GridState>> = const { std::cell::RefCell::new(None) };
}

fn cols() -> Vec<Column> {
    vec![
        Column::text("代码", 52.0).key().pinned(),
        Column::text("角色", 90.0).groupable(),
        Column::num("净", None, 90.0),
        Column::text("探针", 84.0).groupable(),
        Column::num("成交", None, 60.0),
        Column::num("权利金", None, 84.0),
        Column::num("费", None, 72.0),
        Column::num("价差", None, 72.0),
        Column::num("对冲", None, 72.0),
        Column::num("结算", None, 84.0),
        Column::num("持仓残差", None, 90.0),
    ]
}

/// 表格交互（经 pane 事件 OptionsGrid 回到这里）。
pub fn grid_update(m: GridMsg) {
    let cols = cols();
    GRID.with(|g| g.borrow_mut().get_or_insert_with(|| GridState::new(&cols)).update(m, &cols, &[]));
}

fn money(v: f64, c: Color) -> Cell {
    Cell::Colored(format!("{v:+.2}"), c)
}

fn strategy_row(r: &StrategyRow) -> Vec<Cell> {
    let down = crate::ui::pal::down();
    vec![
        Cell::Colored(r.code.clone(), crate::ui::pal::warn()),
        Cell::Text(r.role.clone()),
        Cell::Colored(format!("{:+.3}", r.net), sign_c(r.net)),
        Cell::Colored(if r.gate_ok { "✅ 净>0".into() } else { "❌ 净≤0".into() }, if r.gate_ok { crate::ui::pal::up() } else { crate::ui::pal::down() }),
        Cell::num(r.n_fills as f64, r.n_fills.to_string()),
        money(r.premium, crate::ui::pal::txt()),
        money(r.fees, down),
        money(r.spread_cost, down),
        money(r.hedge_cost, down),
        // 结算 / 持仓残差是模拟出来的金额：带 ~，隐藏数值时为 •••（docs/35 §7.2 / §9.3）
        Cell::Colored(crate::ui::fmt::sim(format!("{:+.2}", r.settle_pnl)), sign_c(r.settle_pnl)),
        Cell::Colored(crate::ui::fmt::sim(format!("{:+.2}", r.hedge_mkt_pnl)), sign_c(r.hedge_mkt_pnl)),
    ]
}

pub fn pane_body<'a>() -> Element<'a, GridMsg> {
    let st: OptionsReadout = super::options_readout::snapshot();
    let mut body = column![].spacing(8).padding(crate::ui::metrics::space(3));

    if super::inspector_props::part().intro() {
        body = body.push(sec("期权 / 0DTE 回测·探针（docs/18 · 不下真实单）"));
    }

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无回测快照——运行 `python -m strategies.research.options.run_backtest --strategy all` 生成")
                .size(crate::ui::text::s_small())
                .color(crate::ui::pal::dim()),
        );
        return crate::ui::scroll(body).width(Length::Fill).height(Length::Fill).into();
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

    // 逐策略：一个策略一行（净 PnL、探针、摩擦分解；探针 docs/16 §5 净捕获 vs 摩擦）
    body = body.push(crate::ui::mark::here());
    let cols = cols();
    let rows: Vec<Vec<Cell>> = st.rows.iter().map(strategy_row).collect();
    let n = rows.len();
    let state = GRID.with(|g| {
        let mut g = g.borrow_mut();
        let s = g.get_or_insert_with(|| GridState::new(&cols));
        s.resort(&cols, &rows);
        s.clone()
    });
    // 表高按行数定（外层是滚动容器，网格要固定高度）
    let h = crate::ui::metrics::panel_header() + crate::ui::metrics::row_height() * (n as f32 + 1.0) + 60.0;
    body = body.push(container(grid::view(cols, rows, state, None, |m| m)).height(Length::Fixed(h)));
    // 各策略说明逐条列在表下（不塞进列里截断）
    for r in &st.rows {
        body = body.push(text(format!("{}　{}", r.code, r.desc)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
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

    crate::ui::scroll(body).width(Length::Fill).height(Length::Fill).into()
}
