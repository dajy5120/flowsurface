//! 订单面板（docs/27 §10）——回测/实盘过程中「持仓与收益怎么变、订单都发生了什么」一页看全。
//!
//! 与既有 `ws/view.rs` 的 WealthSpring 读数面板的分工：那个是**多合一的概览**（订单摘要 +
//! 订单流 + 引擎信号 + 现役池），这个**只做订单**，但做全：
//!
//! - 顶部读数条：持仓方向/数量/均价、已实现（毛/净）、未实现、手续费累计、买卖笔数
//! - 活动挂单表：`OrderAccepted` 进、成交或撤单出（此前生产端不发这些事件，表永远空的，
//!   见 docs/27 §9.2）
//! - 订单明细表：逐笔，时间/序号/方向/类型/价格/数量/金额/毛收益/手续费/净收益/完成度
//!
//! 数据全部来自进程级旁路快照 [`super::orders`]（pane 视图深嵌在 dashboard 里拿不到 `&App`，
//! 沿用 `orders::CHART_FILLS` 的同款旁路）。**本模块只渲染、不发消息**，故对 pane 的消息
//! 类型 `M` 完全泛型。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length};

use super::orders::{Trade, WorkingOrder};
use super::readout::Readout;

/// 明细表最多渲染多少行。`OrderState` 那边已按 FILL_CAP 裁过，这里再兜一层：
/// 整日回测可能上千笔，一次性铺开会让 iced 每帧都重建几千个 widget。
const ROW_CAP: usize = 300;

fn money(v: f64) -> Color {
    if v > 0.0 {
        crate::ui::pal::up()
    } else if v < 0.0 {
        crate::ui::pal::down()
    } else {
        crate::ui::pal::dim()
    }
}

fn side_txt<'a, M: 'a>(side: u8) -> Element<'a, M> {
    let (s, c) = if side == 1 { ("买", crate::ui::pal::up()) } else { ("卖", crate::ui::pal::down()) };
    text(s).size(crate::ui::text::s_small()).color(c).into()
}

/// 秒级 epoch（毫秒输入）→ `MM-DD HH:MM:SS`。
///
/// 回测跨日时只显时分秒会让人对不上是哪一天——整日回测的明细表里这件事很要紧。
fn ts_txt(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    match chrono::DateTime::from_timestamp(secs, 0) {
        Some(dt) => dt.format("%m-%d %H:%M:%S").to_string(),
        None => "—".to_string(),
    }
}

fn cell<'a, M: 'a>(s: impl Into<String>, w: f32, c: Color) -> Element<'a, M> {
    container(text(s.into()).size(crate::ui::text::s_small()).color(c)).width(Length::Fixed(w)).into()
}

/// 顶部读数条：持仓 + 收益 + 手续费 + 笔数。
fn summary<'a, M: 'a>(st: &Readout) -> Element<'a, M> {
    let pos_color = match st.pos_side.as_str() {
        "LONG" => crate::ui::pal::up(),
        "SHORT" => crate::ui::pal::down(),
        _ => crate::ui::pal::dim(),
    };
    let unreal = st.unrealized.unwrap_or(0.0);
    let total = st.realized_net + unreal;
    let kv = |k: &'static str, v: String, c: Color| -> Element<'a, M> {
        column![text(k).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()), text(v).size(crate::ui::text::s_emph()).color(c)].spacing(1).into()
    };
    row![
        kv("持仓", format!("{} {:.4}", st.pos_side, st.net_qty), pos_color),
        kv("均价", format!("{:.2}", st.avg_px), crate::ui::pal::dim()),
        kv("已实现(净)", format!("{:+.4}", st.realized_net), money(st.realized_net)),
        kv("未实现", format!("{unreal:+.4}"), money(unreal)),
        kv("合计", format!("{total:+.4}"), money(total)),
        kv("手续费", format!("{:.4}", st.fee_total), crate::ui::pal::down()),
        kv("买/卖", format!("{} / {}", st.n_buy, st.n_sell), crate::ui::pal::dim()),
        kv("挂单", format!("{}", st.working.len()), crate::ui::pal::dim()),
        kv("权益", format!("{:.2}", st.equity), money(st.return_pct)),
    ]
    .spacing(18)
    .align_y(Alignment::Center)
    .into()
}

/// 活动挂单表。**这张表此前永远是空的**——`ws/orders.rs` 一直认 `OrderAccepted`，
/// 但策略端只发 `OrderFilled`（docs/27 §9.2 已修）。
fn working_table<'a, M: 'a>(working: &[&WorkingOrder]) -> Element<'a, M> {
    if working.is_empty() {
        return text("（无活动挂单）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()).into();
    }
    let head = row![
        cell("方向", 40.0, crate::ui::pal::head()),
        cell("价格", 90.0, crate::ui::pal::head()),
        cell("数量", 80.0, crate::ui::pal::head()),
        cell("订单号", 220.0, crate::ui::pal::head()),
    ]
    .spacing(6);
    let rows = working.iter().fold(column![head].spacing(2), |col, w| {
        col.push(
            row![
                container(side_txt::<M>(w.side)).width(Length::Fixed(40.0)),
                cell(format!("{:.2}", w.price), 90.0, crate::ui::pal::dim()),
                cell(format!("{:.4}", w.qty), 80.0, crate::ui::pal::dim()),
                cell(w.order_id.clone(), 220.0, crate::ui::pal::dim()),
            ]
            .spacing(6),
        )
    });
    scrollable(rows).height(Length::Fixed(90.0)).into()
}

/// 订单明细表（逐笔，新的在上）。
fn trades_table<'a, M: 'a>(trades: &[Trade]) -> Element<'a, M> {
    if trades.is_empty() {
        return text("（本次运行还没有成交）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()).into();
    }
    let head = row![
        cell("#", 34.0, crate::ui::pal::head()),
        cell("时间", 108.0, crate::ui::pal::head()),
        cell("向", 28.0, crate::ui::pal::head()),
        cell("类型", 58.0, crate::ui::pal::head()),
        cell("价格", 84.0, crate::ui::pal::head()),
        cell("数量", 68.0, crate::ui::pal::head()),
        cell("金额", 84.0, crate::ui::pal::head()),
        cell("毛收益", 76.0, crate::ui::pal::head()),
        cell("手续费", 70.0, crate::ui::pal::head()),
        cell("净收益", 76.0, crate::ui::pal::head()),
        cell("完成", 48.0, crate::ui::pal::head()),
    ]
    .spacing(6);
    // 新的在上：回测跑起来后人盯的是「刚刚发生了什么」。
    let rows = trades.iter().rev().take(ROW_CAP).fold(column![head].spacing(2), |col, t| {
        col.push(
            row![
                cell(format!("{}", t.seq), 34.0, crate::ui::pal::dim()),
                cell(ts_txt(t.ts), 108.0, crate::ui::pal::dim()),
                container(side_txt::<M>(t.side)).width(Length::Fixed(28.0)),
                cell(t.order_type.clone(), 58.0, crate::ui::pal::dim()),
                cell(format!("{:.2}", t.price), 84.0, crate::ui::pal::dim()),
                cell(format!("{:.4}", t.qty), 68.0, crate::ui::pal::dim()),
                cell(format!("{:.2}", t.amount), 84.0, crate::ui::pal::dim()),
                cell(format!("{:+.4}", t.gross), 76.0, money(t.gross)),
                cell(format!("{:.4}", t.fee), 70.0, crate::ui::pal::down()),
                cell(format!("{:+.4}", t.net), 76.0, money(t.net)),
                cell(format!("{:.0}%", t.filled_pct), 48.0, crate::ui::pal::dim()),
            ]
            .spacing(6),
        )
    });
    scrollable(rows).height(Length::Fill).into()
}

/// 面板主体。
pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st = super::readout::snapshot();

    if st.run_id.is_empty() && st.trades.is_empty() {
        return container(iced::widget::center(
            column![
                text("订单").size(crate::ui::text::s_section()).color(crate::ui::pal::head()),
                text("等待运行——跑一次回测或实盘后这里会逐笔填充").size(crate::ui::text::s_body()).color(crate::ui::pal::dim()),
                text("（数据来自通道① trader-{run}:stream:events.*）").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
            ]
            .spacing(6)
            .align_x(Alignment::Center),
        ))
        .padding(crate::ui::metrics::space(4))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let mut working: Vec<&WorkingOrder> = st.working.iter().collect();
    working.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));

    column![
        row![
            text(format!("订单 · run {}", if st.run_id.is_empty() { "—" } else { &st.run_id }))
                .size(crate::ui::text::s_emph())
                .color(crate::ui::pal::head()),
            text(format!("本金 {:.0}", st.capital)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        summary::<M>(&st),
        text("活动挂单").size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        working_table::<M>(&working),
        text(format!("订单明细（{} 笔，新的在上）", st.trades.len())).size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        trades_table::<M>(&st.trades),
    ]
    .spacing(8)
    .padding(crate::ui::metrics::space(3))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 订单面板在内容选择器里露面() {
        // 漏进 ALL 的话，面板代码全写好了但用户在「添加面板」里根本找不到它——
        // 编译通过、测试通过、界面上没有。
        use data::layout::pane::ContentKind;
        assert!(ContentKind::ALL.contains(&ContentKind::Orders));
        assert_eq!(ContentKind::Orders.to_string(), "订单");
    }

    #[test]
    fn 时间戳按毫秒解并带日期() {
        // 跨日回测的明细表里只显时分秒会让人对不上是哪一天。
        let s = ts_txt(1_780_704_602_927);
        assert!(s.starts_with("06-06"), "{s}");
        assert_eq!(s.len(), "06-06 00:10:02".len(), "{s}");
    }

    #[test]
    fn 非法时间戳不panic() {
        assert_eq!(ts_txt(u64::MAX), "—");
    }

    #[test]
    fn 收益着色分三档() {
        assert_eq!(money(1.0), crate::ui::pal::up());
        assert_eq!(money(-1.0), crate::ui::pal::down());
        assert_eq!(money(0.0), crate::ui::pal::dim(), "零收益不该染成红或绿");
    }
}
