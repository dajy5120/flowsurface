//! WealthSpring 原生 dockable pane 的视图渲染（docs/08）。
//!
//! 把原先 App 级「悬浮读数框」（F3a 订单/PnL + F4a 订单流 + F4b–d 引擎信号 + F4c combo
//! + Factory 现役池）改造为 FlowSurface 的一个正式停靠面板内容（`Content::WealthSpring`）。
//!
//! 数据走进程级旁路快照 [`super::readout`]（pane 视图深嵌在 dashboard 内、拿不到 `&App`，
//! 沿用 `orders::CHART_FILLS` 的旁路模式）。本模块只渲染、不发消息，故对 pane 的消息类型
//! `M` 完全泛型，可直接塞进 FS 的 `compose_stack_view`。

use iced::widget::{center, column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length};

use data::layout::pane::WsPaneMode;

use super::readout::Readout;
use crate::style;

/// 渲染 WealthSpring 读数面板（订单/PnL · 订单流 · 引擎信号 · 现役池）。
///
/// `mode`（docs/08 F6 方案 2）：`Live`/`Backtest` pane 仅在 `ws:active_run` 对应态渲染读数，
/// 否则显占位——让「实盘」「回测」工作区各自只显属于自己的数据。
pub fn pane_body<'a, M: 'a>(mode: WsPaneMode) -> Element<'a, M> {
    let r = super::readout::snapshot();

    // 三态过滤：pane 限定态 ≠ 当前活动态 → 显占位，不串数据。
    let want = match mode {
        WsPaneMode::Any => None,
        WsPaneMode::Live => Some("live"),
        WsPaneMode::Backtest => Some("backtest"),
    };
    if let Some(want) = want
        && r.mode != want
    {
        let label = if want == "live" { "实盘" } else { "回测" };
        let cur = if r.mode.is_empty() { "实时看盘".to_string() } else { r.mode.clone() };
        return container(center(
            column![
                text(format!("WealthSpring · {label}工作区"))
                    .size(style::text_size::SECTION)
                    .font(style::AZERET_MONO),
                text(format!("当前非{label}态（active_run = {cur}）"))
                    .size(style::text_size::BODY),
                text(format!("切到{label}时此面板自动显示对应读数")).size(style::text_size::SMALL),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        ))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let header = text(format!(
        "WealthSpring · {} {}",
        if r.mode.is_empty() { "watch" } else { &r.mode },
        if r.run_id.is_empty() {
            String::new()
        } else {
            format!("[{}]", r.run_id)
        }
    ))
    .size(style::text_size::SECTION)
    .font(style::AZERET_MONO);

    let mut body = column![header].spacing(10).padding(4);

    // ── 持仓 / PnL（F3a，含本金/权益/收益率）──
    {
        let mut col = column![
            kv("方向", format!("{} {:.3} @ {:.2}", r.pos_side, r.net_qty, r.avg_px)),
            kv("本金", format!("{:.2} USDT", r.capital)),
            kv("权益", format!("{:.2} USDT", r.equity)),
            kv("收益率", format!("{:+.3} %", r.return_pct)),
            kv("累计收益", format!("毛 {:+.2}   净 {:+.2}", r.realized, r.realized_net)),
            kv("累计手续费", format!("{:.4}", r.fee_total)),
            kv("成交", format!("{} 笔  买 {} / 卖 {}", r.n_fills, r.n_buy, r.n_sell)),
        ]
        .spacing(3);
        if let Some(u) = r.unrealized {
            col = col.push(kv("浮动盈亏", format!("{:+.2}", u)));
        }
        body = body.push(section("持仓 / PnL", col));
    }

    // ── 订单明细（每笔：时间/方向/类型/金额/收益/手续费/净收益）──
    if !r.trades.is_empty() {
        let head = |s: &'static str, w: f32| cell(text(s).size(style::text_size::TINY), Length::Fixed(w));
        let mut col = column![row![
            head("时间", 62.0),
            head("方向", 34.0),
            head("类型", 46.0),
            head("金额", 78.0),
            head("收益", 64.0),
            head("手续费", 56.0),
            head("净收益", 64.0),
        ]
        .spacing(6)]
        .spacing(2);
        // 最新在上，取最近 18 笔。
        for t in r.trades.iter().rev().take(18) {
            col = col.push(trade_row(t));
        }
        body = body.push(section("订单明细", col));
    }

    // ── 订单流（F4a：cockpit 侧从逐笔/盘口算）──
    {
        let div = match r.divergence {
            1 => "↑ 看涨背离",
            -1 => "↓ 看跌背离",
            _ => "—",
        };
        body = body.push(section(
            "订单流",
            column![
                kv("CVD", format!("{:+.3}", r.cvd)),
                kv("成交不平衡", format!("{:+.2}", r.imbalance)),
                kv("背离", div.to_string()),
                kv("盘口不平衡", format!("{:+.2}", r.book_imb)),
                kv("spread", format!("{:.1}", r.spread)),
                kv(
                    "吸收 b/a",
                    format!("{:.2} / {:.2}", r.absorbed_bid, r.absorbed_ask),
                ),
                kv(
                    "撤补 b/a",
                    format!("{:.2} / {:.2}", r.pulled_bid, r.pulled_ask),
                ),
            ]
            .spacing(3),
        ));
    }

    // ── 引擎信号（F4b–d 精确版：ws_signals 全档 L2 重建 + 成交归因）──
    if r.has_signals {
        let mut col = column![
            kv(
                "本场吸收 b/a",
                format!("{:.2} / {:.2}", r.sess_traded_bid, r.sess_traded_ask),
            ),
            kv(
                "本场撤补 b/a",
                format!("{:.2} / {:.2}", r.sess_pulled_bid, r.sess_pulled_ask),
            ),
            kv(
                "冰山 b/a",
                format!("{:.2} / {:.2}", r.iceberg_bid, r.iceberg_ask),
            ),
            kv("档位 b/a", format!("{} / {}", r.depth_bid, r.depth_ask)),
        ]
        .spacing(3);
        // F4c：现役池 combo 实时加权值 + 覆盖数。
        if r.sig_n_pool > 0 {
            col = col.push(kv(
                "combo",
                format!("{:+.4}   覆盖 {}/{}", r.sig_combo, r.sig_n_combo, r.sig_n_pool),
            ));
        }
        body = body.push(section("引擎信号", col));
    }

    // ── Factory 现役池（F4c：读 ws:factory:pool）──
    {
        let mut col = column![kv(
            "规模",
            format!(
                "池 {} / alphas {} / evals {}",
                r.fac_n_pool, r.fac_alphas, r.fac_evals
            ),
        )]
        .spacing(3);

        if !r.pool.is_empty() {
            // 表头
            col = col.push(
                row![
                    cell(text("alpha 表达式").size(style::text_size::SMALL), Length::Fill),
                    cell(text("权重").size(style::text_size::SMALL), Length::Fixed(70.0)),
                    cell(text("IC·t").size(style::text_size::SMALL), Length::Fixed(60.0)),
                ]
                .spacing(6),
            );
            for m in r.pool.iter().take(14) {
                col = col.push(
                    row![
                        cell(
                            text(truncate(&m.expr, 40))
                                .size(style::text_size::SMALL)
                                .font(style::AZERET_MONO),
                            Length::Fill,
                        ),
                        cell(
                            text(format!("{:+.3}", m.weight))
                                .size(style::text_size::SMALL)
                                .font(style::AZERET_MONO),
                            Length::Fixed(70.0),
                        ),
                        cell(
                            text(format!("{:.1}", m.ic_t))
                                .size(style::text_size::SMALL)
                                .font(style::AZERET_MONO),
                            Length::Fixed(60.0),
                        ),
                    ]
                    .spacing(6),
                );
            }
        }
        body = body.push(section("Factory 现役池", col));
    }

    if !has_any(&r) {
        body = body.push(
            text("等待 WealthSpring 数据…（需 run_all 起 ws_signals / factory_bridge / live_paper）")
                .size(style::text_size::SMALL),
        );
    }

    container(scrollable(body))
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 一个带标题的小节卡片。
fn section<'a, M: 'a>(title: &'a str, body: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(
        column![
            text(title)
                .size(style::text_size::BODY)
                .font(style::AZERET_MONO),
            body.into(),
        ]
        .spacing(6),
    )
    .padding(10)
    .width(Length::Fill)
    .style(style::dashboard_modal)
    .into()
}

/// 键值行（左标签 + 右值，等宽数字对齐）。
fn kv<'a, M: 'a>(key: &'a str, value: String) -> Element<'a, M> {
    row![
        text(key).size(style::text_size::SMALL).width(Length::Fixed(110.0)),
        text(value)
            .size(style::text_size::SMALL)
            .font(style::AZERET_MONO),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// 表格单元格（固定/自适应宽）。
fn cell<'a, M: 'a>(content: impl Into<Element<'a, M>>, width: Length) -> Element<'a, M> {
    container(content).width(width).into()
}

/// 盈亏配色：正绿、负红、零灰。
fn pnl_color(v: f64) -> Color {
    if v > 0.0 {
        Color::from_rgb(0.30, 0.72, 0.47)
    } else if v < 0.0 {
        Color::from_rgb(0.86, 0.32, 0.34)
    } else {
        Color::from_rgb(0.6, 0.6, 0.6)
    }
}

/// ms 时间戳 → 本地 HH:MM:SS。
fn fmt_ts(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// 「订单明细」一行：时间/方向/类型/金额/收益/手续费/净收益。开仓行收益显「—」。
fn trade_row<'a, M: 'a>(t: &super::orders::Trade) -> Element<'a, M> {
    let sz = style::text_size::SMALL;
    let mono = |s: String| text(s).size(sz).font(style::AZERET_MONO);
    let (dir, dir_c) =
        if t.side == 1 { ("买", pnl_color(1.0)) } else { ("卖", pnl_color(-1.0)) };
    // 开仓（买）本笔无毛收益 → 显「—」；平仓显带符号毛收益。
    let gross = if t.side == 1 {
        text("—".to_string()).size(sz).font(style::AZERET_MONO)
    } else {
        text(format!("{:+.2}", t.gross)).size(sz).font(style::AZERET_MONO).color(pnl_color(t.gross))
    };
    row![
        cell(mono(fmt_ts(t.ts)), Length::Fixed(62.0)),
        cell(text(dir.to_string()).size(sz).color(dir_c), Length::Fixed(34.0)),
        cell(mono(t.order_type.clone()), Length::Fixed(46.0)),
        cell(mono(format!("{:.2}", t.amount)), Length::Fixed(78.0)),
        cell(gross, Length::Fixed(64.0)),
        cell(mono(format!("{:.4}", t.fee)), Length::Fixed(56.0)),
        cell(
            text(format!("{:+.2}", t.net)).size(sz).font(style::AZERET_MONO).color(pnl_color(t.net)),
            Length::Fixed(64.0),
        ),
    ]
    .spacing(6)
    .into()
}

fn has_any(r: &Readout) -> bool {
    r.has_orders
        || r.n_fills > 0
        || r.cvd != 0.0
        || r.spread > 0.0
        || !r.pool.is_empty()
        || r.has_signals
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}
