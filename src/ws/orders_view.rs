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
//! 沿用 `orders::CHART_FILLS` 的同款旁路）。两张表用 `ui::grid`（docs/35 §16.5 第 2 项）：
//! 可排序、可按标的 / 方向 / 类型分组、可调列宽、虚拟滚动；表格交互发 [`OrdersMsg`]。

use std::cell::RefCell;

use iced::widget::{column, container, row, text};
use iced::{Alignment, Color, Element, Length};

use super::orders::{Trade, WorkingOrder};
use super::readout::Readout;
use crate::ui::fmt::Absence;
use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};

fn money(v: f64) -> Color {
    if v > 0.0 {
        crate::ui::pal::up()
    } else if v < 0.0 {
        crate::ui::pal::down()
    } else {
        crate::ui::pal::dim()
    }
}

/// 秒级 epoch（毫秒输入）→ `MM-DD HH:MM:SS`。
///
/// 回测跨日时只显时分秒会让人对不上是哪一天——整日回测的明细表里这件事很要紧。
fn ts_txt(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    match chrono::DateTime::from_timestamp(secs, 0) {
        Some(dt) => dt.format("%m-%d %H:%M:%S").to_string(),
        // 时间戳超出范围：出错
        None => crate::ui::fmt::invalid_at(&format!("订单时间戳超出范围（{ms} ms）")),
    }
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
        kv("持仓", sim(format!("{} {:.4}", st.pos_side, st.net_qty)), pos_color),
        kv("均价", sim(format!("{:.2}", st.avg_px)), crate::ui::pal::dim()),
        kv("已实现(净)", sim(format!("{:+.4}", st.realized_net)), money(st.realized_net)),
        kv("未实现", sim(format!("{unreal:+.4}")), money(unreal)),
        kv("合计", sim(format!("{total:+.4}")), money(total)),
        kv("手续费", sim(format!("{:.4}", st.fee_total)), crate::ui::pal::down()),
        kv("买/卖", format!("{} / {}", st.n_buy, st.n_sell), crate::ui::pal::dim()),
        kv("挂单", format!("{}", st.working.len()), crate::ui::pal::dim()),
        kv("权益", sim(format!("{:.2}", st.equity)), money(st.return_pct)),
    ]
    .spacing(18)
    .align_y(Alignment::Center)
    .into()
}

// 两张表的网格状态（排序、分组、列宽、选中、滚动）。面板视图每帧从旁路快照现拼数据，
// 状态只能放进程级静态——和数据同款旁路；消息经 [`OrdersMsg`] 回到 [`handle`]。
thread_local! {
    static WORKING: RefCell<Option<GridState>> = const { RefCell::new(None) };
    static TRADES: RefCell<Option<GridState>> = const { RefCell::new(None) };
}

/// 订单面板的交互：两张网格各自的排序 / 分组 / 调宽 / 选中 / 滚动。
#[derive(Debug, Clone)]
pub enum OrdersMsg {
    Working(GridMsg),
    Trades(GridMsg),
}

pub fn handle(m: OrdersMsg) {
    let (cell, cols, msg) = match m {
        OrdersMsg::Working(g) => (&WORKING, working_cols(), g),
        OrdersMsg::Trades(g) => (&TRADES, trade_cols(), g),
    };
    cell.with(|s| {
        // 这里拿不到数据：排序后的顺序在下一帧 view 里按新数据重排
        s.borrow_mut().get_or_insert_with(|| GridState::new(&cols)).update(msg, &cols, &[]);
    });
}

/// 取出状态并按本帧数据重排（几百行，排序是微秒级）。
fn state_for(
    cell: &'static std::thread::LocalKey<RefCell<Option<GridState>>>,
    cols: &[Column],
    rows: &[Vec<Cell>],
) -> GridState {
    cell.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.get_or_insert_with(|| GridState::new(cols));
        st.resort(cols, rows);
        st.clone()
    })
}

use crate::ui::fmt::sim;

/// 数量 / 金额格（docs/35 §7.2）：这里的一切都来自回测或模拟盘，带 `~` 模拟标记；
/// 隐藏数值模式下显示 `•••`（§9.3）。
fn sim_num(v: f64, s: String) -> Cell {
    if crate::ui::hide_values() {
        Cell::Absent(Absence::Withheld)
    } else {
        Cell::Num { v: Some(v), s, prov: crate::ui::fmt::Provenance::Simulated }
    }
}

/// 带涨跌色的模拟金额（毛收益 / 手续费 / 净收益）。
fn sim_colored(s: String, c: Color) -> Cell {
    if crate::ui::hide_values() { Cell::Absent(Absence::Withheld) } else { Cell::Colored(sim(s), c) }
}

type Rows = std::rc::Rc<[Vec<Cell>]>;

thread_local! {
    static WORKING_ROWS: RefCell<Option<(std::time::Instant, bool, Rows)>> = const { RefCell::new(None) };
    static TRADE_ROWS: RefCell<Option<(std::time::Instant, bool, Rows)>> = const { RefCell::new(None) };
}

/// 表格行的节流与冻结（docs/35 §8，UPDS V5 §30）：最多每秒重建 4 次——整日回测时成交一秒几十笔，
/// 逐帧重建既费 CPU 又晃眼；冻结时一直用冻结那一刻的行。隐藏数值开关一变立刻重建（不能等 250ms，
/// 更不能因为冻结就一直露着数）。
fn cached(cell: &'static std::thread::LocalKey<RefCell<Option<(std::time::Instant, bool, Rows)>>>, frozen: bool, build: impl FnOnce() -> Vec<Vec<Cell>>) -> Rows {
    let hide = crate::ui::hide_values();
    cell.with(|c| {
        let mut c = c.borrow_mut();
        let fresh = match &*c {
            Some((t, h, _)) => *h == hide && (frozen || t.elapsed() < std::time::Duration::from_millis(250)),
            None => false,
        };
        if !fresh {
            *c = Some((std::time::Instant::now(), hide, build().into()));
        }
        c.as_ref().map(|(_, _, r)| r.clone()).unwrap_or_else(|| Vec::new().into())
    })
}

fn side_cell(side: u8) -> Cell {
    // 文字本身就是非颜色的区分（UPDS：颜色不能是唯一信号）
    if side == 1 { Cell::Colored("买".into(), crate::ui::pal::up()) } else { Cell::Colored("卖".into(), crate::ui::pal::down()) }
}

fn working_cols() -> Vec<Column> {
    vec![
        Column::text("方向", 52.0).groupable(),
        Column::num("价格", None, 100.0),
        Column::num("数量", None, 90.0),
        Column::text("订单号", 240.0).key(),
    ]
}

/// 活动挂单表。**这张表此前永远是空的**——`ws/orders.rs` 一直认 `OrderAccepted`，
/// 但策略端只发 `OrderFilled`（docs/27 §9.2 已修）。
fn working_table<'a>(working: &[WorkingOrder], frozen: bool) -> Element<'a, OrdersMsg> {
    if working.is_empty() {
        return text("（无活动挂单）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()).into();
    }
    let cols = working_cols();
    let rows = cached(&WORKING_ROWS, frozen, || {
        // 原始顺序 = 价格从高到低（和盘口一样读）；点表头可以改
        let mut ws: Vec<&WorkingOrder> = working.iter().collect();
        ws.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal));
        ws.iter()
            .map(|w| {
                vec![
                    side_cell(w.side),
                    Cell::num(w.price, format!("{:.2}", w.price)),
                    sim_num(w.qty, format!("{:.4}", w.qty)),
                    Cell::Id(w.order_id.clone()),
                ]
            })
            .collect()
    });
    let st = state_for(&WORKING, &cols, &rows);
    container(grid::view(cols, rows, st, None, OrdersMsg::Working)).height(Length::Fixed(170.0)).into()
}

fn trade_cols() -> Vec<Column> {
    vec![
        Column::num("#", None, 48.0).key(),
        Column::text("时间", 124.0),
        Column::text("标的", 110.0).groupable(),
        Column::text("向", 40.0).groupable(),
        Column::text("类型", 70.0).groupable(),
        Column::num("价格", None, 90.0),
        Column::num("数量", None, 80.0),
        Column::num("金额", None, 96.0),
        Column::num("毛收益", None, 88.0),
        Column::num("手续费", None, 80.0),
        Column::num("净收益", None, 88.0),
        Column::num("完成", Some("%"), 60.0),
    ]
}

/// 订单明细表（逐笔，原始顺序新的在上）。网格只渲染可见行，不再需要截断行数。
fn trades_table<'a>(trades: &[Trade], frozen: bool) -> Element<'a, OrdersMsg> {
    if trades.is_empty() {
        return text("（本次运行还没有成交）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()).into();
    }
    let cols = trade_cols();
    let rows = cached(&TRADE_ROWS, frozen, || {
        // 新的在上：回测跑起来后人盯的是「刚刚发生了什么」。
        trades
            .iter()
            .rev()
            .map(|t| {
                vec![
                    Cell::num(t.seq as f64, format!("{}", t.seq)),
                    Cell::Text(ts_txt(t.ts)),
                    if t.instrument.is_empty() { Cell::Absent(Absence::Missing) } else { Cell::Id(t.instrument.clone()) },
                    side_cell(t.side),
                    Cell::Text(t.order_type.clone()),
                    Cell::num(t.price, format!("{:.2}", t.price)),
                    sim_num(t.qty, format!("{:.4}", t.qty)),
                    sim_num(t.amount, format!("{:.2}", t.amount)),
                    sim_colored(format!("{:+.4}", t.gross), money(t.gross)),
                    sim_colored(format!("{:.4}", t.fee), crate::ui::pal::down()),
                    sim_colored(format!("{:+.4}", t.net), money(t.net)),
                    Cell::num(t.filled_pct, format!("{:.0}", t.filled_pct)),
                ]
            })
            .collect()
    });
    // 页脚合计：净收益、手续费（分组折叠后也看得到全表合计）
    let net: f64 = trades.iter().map(|t| t.net).sum();
    let fee: f64 = trades.iter().map(|t| t.fee).sum();
    let foot = format!("合计 净 {} · 手续费 {}", sim(format!("{net:+.4}")), sim(format!("{fee:.4}")));
    let st = state_for(&TRADES, &cols, &rows);
    grid::view(cols, rows, st, Some(foot), OrdersMsg::Trades)
}

/// 面板主体。
///
/// `frozen` = 面板被冻结（docs/35 §8）：两张表停在冻结那一刻的内容。
pub fn pane_body<'a>(frozen: bool) -> Element<'a, OrdersMsg> {
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

    column![
        row![
            text(format!("订单 · run {}", if st.run_id.is_empty() { crate::ui::fmt::UNKNOWN } else { &st.run_id }))
                .size(crate::ui::text::s_emph())
                .color(crate::ui::pal::head()),
            text(format!("本金 {}", sim(format!("{:.0}", st.capital)))).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        summary::<OrdersMsg>(&st),
        text("活动挂单").size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        working_table(&st.working, frozen),
        text(format!("订单明细（{} 笔，新的在上）", st.trades.len())).size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        trades_table(&st.trades, frozen),
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
        assert_eq!(ts_txt(u64::MAX), "!", "非法时间戳是「出错」");
    }

    #[test]
    fn 收益着色分三档() {
        assert_eq!(money(1.0), crate::ui::pal::up());
        assert_eq!(money(-1.0), crate::ui::pal::down());
        assert_eq!(money(0.0), crate::ui::pal::dim(), "零收益不该染成红或绿");
    }
}
