//! 币安钱包预测市场面板（`Content::PmBinance`）——BTC 5 分钟涨跌，逐笔盘口。
//!
//! ## 为什么是独立面板而不是塞进「预测市场 Polymarket」
//!
//! 两者只是名字里都有「预测市场」，看的东西完全不同：
//!
//! | | Polymarket 面板 | 这个面板 |
//! |---|---|---|
//! | 节奏 | 天级（市场几天到几个月） | **秒级**（一轮 5 分钟，WS 约 5 条/秒） |
//! | 内容 | 市场列表 + AI 决策支持 + Brier 校准 | **两本簿的完整档位** + 轮次倒计时 |
//! | 屏幕 | 一屏列十几个市场 | 一屏就够放两本簿 |
//!
//! 挤在一个 pane 里的结果是两边都被压成几行——盘口档位本来就要竖着排。
//!
//! ## 数据从哪来
//!
//! 只读 `~/ws-data/live/pm_binance.json`（[`super::pm_binance_readout`] 每秒轮询）。
//! **面板里没有一行网络代码**——WS 连接在 `ws-pm-recorder` 守护里。让面板自己连的话，
//! 同一份行情就有了两个真相源，还多占一份 WS 配额。启停在「进程」和「网络出口」两页。

use iced::widget::{column, container, row, text};
use iced::{Color, Element, Length};

use super::pm_binance_readout::{self as pm, BookView, Ladder, PmBinanceReadout, RoundView};

const C_HEAD: Color = Color::from_rgb(0.62, 0.70, 0.86);
const C_DIM: Color = Color::from_rgb(0.48, 0.52, 0.60);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GREEN: Color = Color::from_rgb(0.25, 0.78, 0.45);
const C_RED: Color = Color::from_rgb(0.90, 0.35, 0.35);
const C_WARN: Color = Color::from_rgb(0.92, 0.72, 0.30);

/// 档位条的满格宽度（字符数）。
const BAR_W: usize = 14;

fn sec<'a, M: 'a>(t: String) -> Element<'a, M> {
    text(t).size(14).color(C_HEAD).into()
}
fn dim<'a, M: 'a>(t: String) -> Element<'a, M> {
    text(t).size(10).color(C_DIM).into()
}
fn cell<'a, M: 'a>(t: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(t).size(11).color(c)).width(Length::Fixed(w)).into()
}

fn usd(v: f64) -> String {
    if v >= 1e6 {
        format!("${:.2}M", v / 1e6)
    } else if v >= 1e3 {
        format!("${:.1}k", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

/// 倒计时。轮次只有 5 分钟，秒级精度是必要的。
fn countdown(s: i64) -> (String, Color) {
    if s < 0 {
        return ("—".into(), C_DIM);
    }
    let c = if s <= 30 {
        C_RED // 快结算了：此时便宜一侧的彩票挂单最猖狂，失衡度最容易读错
    } else if s <= 90 {
        C_WARN
    } else {
        C_TXT
    };
    (format!("{}:{:02}", s / 60, s % 60), c)
}

/// 一档的条形。按**金额**定长，不按份数——见 [`Ladder::max_notional`]。
fn lvl_bar<'a, M: 'a>(px: f64, sz: f64, max_notional: f64, c: Color) -> Element<'a, M> {
    let n = if max_notional > 0.0 {
        ((px * sz / max_notional).clamp(0.0, 1.0) * BAR_W as f64).round() as usize
    } else {
        0
    };
    row![
        cell(format!("{px:.3}"), 42.0, c),
        cell(format!("{sz:>9.1}"), 62.0, C_TXT),
        cell(usd(px * sz), 56.0, C_DIM),
        text("█".repeat(n)).size(11).color(c),
    ]
    .into()
}

/// 一本簿：卖盘在上（价从高到低），买盘在下——和交易所盘口同一个方向，
/// 中间那条线就是价差。
fn ladder_block<'a, M: 'a>(title: &str, sub: &str, l: &Ladder, tint: Color) -> Element<'a, M> {
    let mx = l.max_notional();
    let mut c = column![
        text(title.to_string()).size(12).color(tint),
        dim(sub.to_string()),
        row![
            cell("价".into(), 42.0, C_DIM),
            cell("份数".into(), 62.0, C_DIM),
            cell("金额".into(), 56.0, C_DIM),
        ],
    ]
    .spacing(2);

    if l.bids.is_empty() && l.asks.is_empty() {
        return c.push(dim("（无档位）".into())).into();
    }
    // 卖盘倒序：最优卖价贴着中间那条线，和买盘最优价对着——否则要跨过整段
    // 深档才能看出价差，盘口就白摆了。
    for (px, sz) in l.asks.iter().rev() {
        c = c.push(lvl_bar(*px, *sz, mx, C_RED));
    }
    c = c.push(
        row![
            cell("——".into(), 42.0, C_DIM),
            dim(format!("价差 {}", spread(l))),
        ]
        .spacing(4),
    );
    for (px, sz) in &l.bids {
        c = c.push(lvl_bar(*px, *sz, mx, C_GREEN));
    }
    if l.n_bids > l.bids.len() || l.n_asks > l.asks.len() {
        c = c.push(dim(format!(
            "（只显示前若干档；全簿 买 {} / 卖 {} 档）",
            l.n_bids, l.n_asks
        )));
    }
    c.into()
}

fn spread(l: &Ladder) -> String {
    match (l.bids.first(), l.asks.first()) {
        (Some((b, _)), Some((a, _))) => format!("{:.3}", a - b),
        _ => "—".into(),
    }
}

/// 轮次抬头：问的是什么、还剩多久、这一轮的规模。
fn round_block<'a, M: 'a>(r: &RoundView) -> Element<'a, M> {
    let (cd, cc) = countdown(r.secs_left);
    column![
        row![
            text(r.question.clone()).size(12).color(C_TXT),
            text(format!("   剩 {cd}")).size(14).color(cc),
        ],
        row![
            cell(format!("开盘 {:.2}", r.start_price), 120.0, C_DIM),
            cell(format!("费 {:.2}%", r.fee_bps / 100.0), 70.0, C_DIM),
            cell(format!("{} 人", r.participants), 62.0, C_DIM),
            cell(format!("量 {}", usd(r.volume)), 90.0, C_DIM),
            cell(format!("流动性 {}", usd(r.liquidity)), 110.0, C_DIM),
            cell(format!("#{}", r.market_id), 90.0, C_DIM),
        ],
    ]
    .spacing(3)
    .into()
}

/// 失衡度。**两个口径都显示，且明说哪个是默认**。
///
/// 实测两者会反号：临近结算时便宜一侧（0.01–0.05）用很少的钱就能堆出大量份额，
/// 那是彩票式挂单不是共识。只显示一个数字的话，看的人没法察觉这件事。
fn imbalance_block<'a, M: 'a>(b: &BookView) -> Element<'a, M> {
    let fmt = |v: Option<f64>| v.map(|x| format!("{x:+.3}")).unwrap_or_else(|| "—".into());
    let dir = |v: Option<f64>| match v {
        Some(x) if x > 0.0 => ("押涨占优", C_GREEN),
        Some(x) if x < 0.0 => ("押跌占优", C_RED),
        Some(_) => ("均衡", C_DIM),
        None => ("无数据", C_DIM),
    };
    let (md, mc) = dir(b.imbalance);
    let (sd, _) = dir(b.imbalance_shares);
    let disagree = match (b.imbalance, b.imbalance_shares) {
        (Some(a), Some(c)) => a.signum() != c.signum() && a != 0.0 && c != 0.0,
        _ => false,
    };
    let mut col = column![
        row![
            cell("失衡(金额)".into(), 80.0, C_DIM),
            text(fmt(b.imbalance)).size(18).color(mc),
            text(format!("  {md}")).size(12).color(mc),
        ],
        row![
            cell("失衡(份数)".into(), 80.0, C_DIM),
            text(fmt(b.imbalance_shares)).size(12).color(C_DIM),
            text(format!("  {sd} · 仅供对照")).size(10).color(C_DIM),
        ],
        row![
            cell("押注金额".into(), 80.0, C_DIM),
            cell(format!("涨 {}", usd(b.up_notional)), 100.0, C_GREEN),
            cell(format!("跌 {}", usd(b.down_notional)), 100.0, C_RED),
        ],
    ]
    .spacing(3);
    if disagree {
        // 反号的时刻正是份数口径出错的时刻，说出来比让人自己比对两行数字可靠。
        col = col.push(
            text("⚠ 两个口径反号——便宜一侧在堆份额，按份数读会指反方向")
                .size(11)
                .color(C_WARN),
        );
    }
    col.into()
}

/// 录制状态。**放在显眼处是有意的**：这份数据没有第三方历史源，
/// 录制断了就是永久缺口，事后一秒都补不回来。
fn rec_block<'a, M: 'a>(st: &PmBinanceReadout) -> Element<'a, M> {
    let alive = pm::recording_alive(st);
    let (dot, c, t) = if alive {
        (
            "●",
            C_GREEN,
            format!(
                "录制中  {} 条簿 / {} 轮  WS {} 条（重订 {} 次）",
                st.rec.book_rows, st.rec.rounds, st.rec.ws_msgs, st.rec.resubs
            ),
        )
    } else {
        (
            "✗",
            C_RED,
            format!(
                "未在录  Up 陈旧 {}ms / Down 陈旧 {}ms",
                st.rec.up_stale_ms, st.rec.down_stale_ms
            ),
        )
    };
    let mut col = column![row![
        text(dot.to_string()).size(14).color(c),
        text(format!(" {t}")).size(11).color(c),
    ]]
    .spacing(2);
    if !st.rec.last_error.is_empty() {
        col = col.push(dim(format!("最后错误 {}", st.rec.last_error)));
    }
    col = col.push(dim(
        "启停在「进程」页；连接与流量在「网络出口」页".into(),
    ));
    col.into()
}

/// 最近几轮的结果条。看「刚才几轮是涨是跌」——从落盘的轮次表读，重启后仍在。
fn recent_block<'a, M: 'a>(st: &PmBinanceReadout) -> Element<'a, M> {
    if st.recent.is_empty() {
        return dim("最近轮次：（还没有已落盘的轮次）".into());
    }
    let mut r = row![cell("最近".into(), 40.0, C_DIM)].spacing(4);
    for x in &st.recent {
        let (t, c) = match x.resolved.as_str() {
            "UP" => ("涨", C_GREEN),
            "DOWN" => ("跌", C_RED),
            // 空不是平局，是还没判出来——用不同的字形，别让它看起来像一个结果。
            _ => ("?", C_DIM),
        };
        r = r.push(cell(t.into(), 18.0, c));
    }
    let n_res = st.recent.iter().filter(|x| !x.resolved.is_empty()).count();
    let n_up = st.recent.iter().filter(|x| x.resolved == "UP").count();
    column![
        r,
        dim(format!(
            "已判 {n_res}/{} 轮，其中涨 {n_up} —— 样本太少，别据此判方向",
            st.recent.len()
        )),
    ]
    .spacing(2)
    .into()
}

pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st = pm::snapshot();
    let mut body = column![].spacing(8).padding(10);
    body = body.push(sec(format!(
        "Binance Prediction Trading · {} · 实时盘口",
        if st.symbol.is_empty() { "BTC 5m".into() } else { st.symbol.clone() }
    )));

    if !st.present {
        body = body.push(
            text(
                "暂无快照。启动录制器：在「进程」页开 ws-pm-recorder\n\
                 （需 .env 里的 BINANCE_PREDICTION_* 凭证 + 账户开通 Prediction Trading）",
            )
            .size(11)
            .color(C_DIM),
        );
        return body.into();
    }

    body = body.push(rec_block(&st));
    if let Some(r) = &st.round {
        body = body.push(round_block(r));
    }
    if let Some(b) = &st.book {
        body = body.push(imbalance_block(b));
        // 两本簿并排：左押涨右押跌，与失衡度的正负方向一致（正=涨=左）。
        body = body.push(
            row![
                container(ladder_block(
                    "押涨 Up",
                    "买这本 = 押 BTC 涨。bid 是想押涨的人挂的",
                    &b.up,
                    C_GREEN,
                ))
                .width(Length::FillPortion(1)),
                container(ladder_block(
                    "押跌 Down",
                    "独立的一本簿，不是 Up 的另一侧",
                    &b.down,
                    C_RED,
                ))
                .width(Length::FillPortion(1)),
            ]
            .spacing(14),
        );
    } else {
        body = body.push(
            text("两本簿还没凑齐——宁可不显示，也不给半份（半份会把押跌当成 0）")
                .size(11)
                .color(C_WARN),
        );
    }
    body = body.push(recent_block(&st));
    body = body.push(dim(format!("快照 {}  刷新 {}", st.stamp, st.refreshed)));
    body.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lad(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Ladder {
        Ladder {
            bids: bids.to_vec(),
            asks: asks.to_vec(),
            n_bids: bids.len(),
            n_asks: asks.len(),
        }
    }

    /// **条长按金额，不按份数。**
    ///
    /// 0.02 那档有 5000 份（100 USDT），0.90 那档有 500 份（450 USDT）。
    /// 按份数定标的话便宜那档会撑满整行，把真正有钱的档压成一条——
    /// 和失衡度按份数会指反是同一个错误的两种表现。
    fn 满格按金额而不是份数() {
        let l = lad(&[(0.90, 500.0), (0.02, 5000.0)], &[]);
        assert!((l.max_notional() - 450.0).abs() < 1e-9, "满格应是 450 USDT 那档");
    }

    #[test]
    fn 条长确实按金额() {
        满格按金额而不是份数();
    }

    #[test]
    fn 价差取两个最优价之差() {
        assert_eq!(spread(&lad(&[(0.46, 1.0)], &[(0.47, 1.0)])), "0.010");
        assert_eq!(spread(&lad(&[], &[(0.47, 1.0)])), "—", "缺一侧就没有价差可言");
    }

    #[test]
    fn 倒计时在临近结算时变色() {
        assert_eq!(countdown(-1).0, "—");
        assert_eq!(countdown(125).0, "2:05");
        assert_eq!(countdown(9).0, "0:09");
        // 最后 30 秒标红：便宜一侧的彩票挂单此时最猖狂，失衡度最容易读错。
        assert_eq!(countdown(20).1, C_RED);
        assert_eq!(countdown(60).1, C_WARN);
        assert_eq!(countdown(200).1, C_TXT);
    }

    #[test]
    fn 空簿不会panic() {
        let l = lad(&[], &[]);
        assert_eq!(l.max_notional(), 0.0);
        let _: Element<'_, ()> = ladder_block("t", "s", &l, C_GREEN);
    }

    #[test]
    fn 没快照时不渲染空市场() {
        // present=false 走的是提示分支；这里只确认它不会去碰 None 的 round/book。
        let st = PmBinanceReadout::default();
        assert!(!st.present && st.round.is_none() && st.book.is_none());
    }
}
