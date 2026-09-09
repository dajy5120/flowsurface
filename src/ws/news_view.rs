//! 新闻面板的渲染（docs/25 §5、N2）。
//!
//! 两区：**源健康**在上、**时间线**在下。
//!
//! 健康区放在最上面是有理由的：这一页最容易出的错不是「新闻不好看」，
//! 是**一个源悄悄死了而列表看起来一切正常**（§3.1 的 WSJ，实测陈了
//! 589 天仍返回 200）。把健康藏在下面等于把这件事藏起来。

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Color, Element, Length};

use super::news_readout::{self as ro, ago, NewsRow, SourceRow};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
/// 可点标题的颜色。**整行里最亮的东西**——它是唯一可交互的部分。
const C_LINK: Color = Color::from_rgb(0.55, 0.88, 1.00);
const C_LINK_HOVER: Color = Color::from_rgb(0.82, 0.95, 1.00);

/// 标题链接的样式。
///
/// 「点了没反应」查了半天之后（xdg-open 那个 bug），另一半问题是
/// **看不出哪儿能点**。所以三件事一起给：
///
/// 1. 静止时就用**明显不同于正文的链接色**，不用等鼠标悬上去；
/// 2. 悬停时变亮 **并且给一块背景**——颜色变化对色觉不敏感的人不够，
///    背景块是形状上的变化；
/// 3. 按下时收一点，给一个按到了的回执。
fn link_style(_t: &iced::Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let (text_color, bg) = match status {
        Status::Hovered => (C_LINK_HOVER, Some(Color { a: 0.14, ..C_LINK })),
        Status::Pressed => (C_LINK_HOVER, Some(Color { a: 0.24, ..C_LINK })),
        Status::Disabled => (C_DIM, None),
        Status::Active => (C_LINK, None),
    };
    iced::widget::button::Style {
        text_color,
        background: bg.map(Into::into),
        border: iced::Border { radius: 3.0.into(), ..Default::default() },
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewsMsg {
    Start,
    Stop,
    /// 打开原文。
    Open(String),
    /// 折叠/展开源健康区。
    ToggleHealth,
    /// 筛选框在打字。**只改面板内存**——守护照常收全部，筛选只影响这一屏。
    FilterEdited(String),
    /// 订阅框在打字。
    WatchEdited(String),
    /// 把输入框里的标的加进订阅。
    WatchAdd,
    /// 退订一个标的。
    WatchRemove(String),
}

fn chip<'a>(label: &str, msg: NewsMsg) -> Element<'a, NewsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, NewsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

/// 分级的颜色。一级来源要看得出来——媒体是派生的、更晚的。
///
/// 都比 [`C_LINK`] 暗：**整行里最亮的必须是可点的那部分**，
/// 否则「哪儿能点」这件事又要靠猜。
fn tier_color(tier: &str) -> Color {
    match tier {
        "regulator" => Color { a: 0.85, ..C_OK },
        "exchange" => Color { a: 0.80, ..C_HEAD },
        "corporate" => Color { a: 0.80, ..C_TXT },
        _ => C_DIM,
    }
}

/// 一个源的状态灯：`(灯, 颜色, 说明)`。
///
/// 四个状态要分得开，尤其是**「取得到但是陈的」和「取不到」**——
/// 前者只看状态码永远发现不了，后者一眼就能看见。
pub fn source_lamp(s: &SourceRow) -> (&'static str, Color, String) {
    if s.failing() {
        return ("✕", C_BAD, format!("连续失败 {} 次", s.consecutive_fails));
    }
    if s.is_stale {
        // **这一行是这一页的理由**
        let d = s.stale_secs.unwrap_or(0) / 86400;
        return ("⚠", C_BAD, format!("陈了 {d} 天（一直返回 200）"));
    }
    if s.never_seen() {
        return ("○", C_DIM, "还没抓到".into());
    }
    ("●", C_OK, String::new())
}

pub fn pane_body<'a>() -> Element<'a, NewsMsg> {
    let st = ro::snapshot();
    let mut body = column![].spacing(4).padding(8);

    // ── 顶栏 ──
    let stale_txt = if st.stale_sources > 0 {
        format!("⚠ {} 个源已陈", st.stale_sources)
    } else {
        "源全部新鲜".into()
    };
    body = body.push(
        row![
            text("新闻资讯").size(13).color(C_TXT),
            chip("▶ 启动", NewsMsg::Start),
            chip("■ 停止", NewsMsg::Stop),
            text(if st.svc.active {
                format!("守护运行中 {}s", st.svc.uptime_secs)
            } else {
                "守护未运行".into()
            })
            .size(10)
            .color(if st.svc.active { C_DIM } else { C_GOLD }),
            text(format!("{} 条在窗内", st.total)).size(11).color(C_TXT),
            text(stale_txt).size(11).color(if st.stale_sources > 0 { C_BAD } else { C_DIM }),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    if !st.present || st.sources.is_empty() {
        body = body.push(
            text("还没有快照——守护没起，或者刚起还没抓完第一轮").size(11).color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 源健康 ──
    body = body.push(
        row![
            text("源健康").size(11).color(C_HEAD),
            text("HTTP 200 不等于有新闻——「陈了」这一列才是这一页的理由").size(10).color(C_DIM),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    let mut h = row![].spacing(4);
    for (t, w, n) in [
        ("", 20.0, false),
        ("源", 120.0, false),
        ("分级", 56.0, false),
        ("见过的最新", 84.0, false),
        ("窗内", 44.0, true),
        ("拉/未变/失败", 108.0, false),
        ("状态", 260.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);

    // 有问题的排在前面：一片正常里混着一行红，很容易被翻过去
    let mut rows: Vec<&SourceRow> = st.sources.iter().collect();
    rows.sort_by_key(|s| (!(s.is_stale || s.failing()), s.label.clone()));
    for s in rows {
        let (lamp, lc, why) = source_lamp(s);
        let newest = match s.newest_ms {
            Some(m) => ago(m, st.now_ms),
            // 「从没抓到」和「陈了」是两回事
            None => "—".into(),
        };
        body = body.push(
            row![
                cell(lamp.into(), 20.0, lc, false),
                cell(s.label.clone(), 120.0, C_TXT, false),
                cell(s.tier_label.clone(), 56.0, tier_color(&s.tier), false),
                cell(newest, 84.0, if s.is_stale { C_BAD } else { C_DIM }, false),
                cell(s.in_window.to_string(), 44.0, C_DIM, true),
                cell(
                    format!("{}/{}/{}", s.ok, s.not_modified, s.fails),
                    108.0,
                    if s.fails > 0 { C_GOLD } else { C_DIM },
                    false
                ),
                cell(
                    if why.is_empty() { clip(&s.last_status, 34) } else { why },
                    260.0,
                    lc,
                    false
                ),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        );
    }

    // ── 按标的订阅（N5）──
    let mut wr = row![
        text("订阅").size(11).color(C_HEAD),
        text_input("标的代码，如 AAPL（回车添加）", &ro::watch_input())
            .on_input(NewsMsg::WatchEdited)
            .on_submit(NewsMsg::WatchAdd)
            .size(11)
            .padding([2, 6])
            .width(Length::Fixed(200.0)),
        chip("添加", NewsMsg::WatchAdd),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    for w in &st.watch {
        // 每一个都列，**包括一条申报都没抓到的**——不列的话，
        // 「这只票 SEC 认不出来」会看起来像「它最近没申报」
        let ok = w.fails == 0 && w.ok > 0;
        wr = wr.push(
            row![
                text(format!("{} ({})", w.symbol, w.in_window))
                    .size(11)
                    .color(if ok { C_OK } else { C_GOLD }),
                chip("×", NewsMsg::WatchRemove(w.symbol.clone())),
            ]
            .spacing(2)
            .align_y(iced::Alignment::Center),
        );
    }
    body = body.push(wr.wrap());
    if !st.watch.is_empty() {
        body = body.push(
            text("SEC 一手申报，默认只收重大表格（8-K/10-Q/13D…）——\
                  Form 4 内部人交易占了申报总量的六成，收进来会把 8-K 埋掉")
                .size(10)
                .color(C_DIM),
        );
    }

    // ── 时间线 ──
    let q = ro::filter_text();
    let shown: Vec<&ro::NewsRow> = st.items.iter().filter(|i| ro::matches(&q, i)).collect();
    body = body.push(
        row![
            text("时间线").size(11).color(C_HEAD),
            text_input("筛选：词=都要有 · \"词组\" · -排除 · tier:/kind:/sym:/lang:/src:", &q)
                .on_input(NewsMsg::FilterEdited)
                .size(11)
                .padding([2, 6])
                .width(Length::Fixed(420.0)),
            // **筛掉了多少必须说**。只显示一张短表的话，
            // 「筛选筛没了」和「本来就没有」分不出来
            text(if q.trim().is_empty() {
                format!("{} 条", st.items.len())
            } else {
                format!("{} / {} 条（筛掉 {}）", shown.len(), st.items.len(), st.items.len() - shown.len())
            })
            .size(10)
            .color(if !q.trim().is_empty() && shown.is_empty() { C_GOLD } else { C_DIM }),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text("`~` = 源没给发布时间，这里显示的是我们抓到的时刻").size(10).color(C_DIM),
    );
    for it in shown.iter().take(120) {
        body = body.push(item_row(it, st.now_ms));
    }
    if st.items.is_empty() {
        body = body.push(text("窗内还没有条目").size(11).color(C_DIM));
    } else if shown.is_empty() {
        // 空列表要说清是**筛选**筛没的，不是没新闻
        body = body.push(
            text(format!("这条筛选把 {} 条全筛掉了", st.items.len())).size(11).color(C_GOLD),
        );
    }

    body = body.push(
        text(format!("快照 {} · 读于 {}", st.stamp, st.refreshed)).size(10).color(C_DIM),
    );
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

fn item_row<'a>(it: &NewsRow, now: i64) -> Element<'a, NewsMsg> {
    // 时间是猜的就打个记号。一条三天前的公告显示成刚刚发布，
    // 比不显示时间糟得多
    let when = format!("{}{}", ago(it.sort_ms, now), if it.time_guessed { "~" } else { "" });
    let mut r = row![
        cell(when, 62.0, if it.time_guessed { C_GOLD } else { C_DIM }, false),
        cell(it.source.clone(), 92.0, tier_color(&it.tier), false),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    if !it.symbols.is_empty() {
        r = r.push(cell(it.symbols.join(","), 70.0, C_HEAD, false));
    }
    // **标题本身就是链接**，点了在浏览器里看原文。
    //
    // 不做成末尾一个小「原文」按钮：那个按钮的点击目标只有几十像素，
    // 而标题横跨整行——用户想点的本来就是标题。
    //
    // 标题**完整显示**，长了折行——地址那次的教训（docs/24）：
    // 定宽格子会把区别切掉，而新闻标题的区别常常在后半截
    r = r.push(if it.url.is_empty() {
        // 没有链接的（Deribit 那种没有单条页面的）显示成普通文本，
        // 不给一个点了没反应的假按钮
        Element::from(text(it.title.clone()).size(11).color(C_DIM).width(Length::Fill))
    } else {
        button(text(it.title.clone()).size(11).width(Length::Fill))
            // 上下留 1px、左右 4px：悬停时那块背景才有形状，
            // padding 全 0 的话背景紧贴字，看着像渲染错误
            .padding([1, 4])
            .style(link_style)
            .on_press(NewsMsg::Open(it.url.clone()))
            .width(Length::Fill)
            .into()
    });

    // 「说的那件事什么时候发生」和「什么时候说的」不是一回事
    if let Some(e) = it.effective_ms {
        let gap = (e - it.sort_ms) / 86_400_000;
        if gap.abs() >= 1 {
            r = r.push(
                text(format!("生效 {}", ago(e, now))).size(10).color(C_GOLD),
            );
        }
    }
    if !it.dupes.is_empty() {
        r = r.push(text(format!("+{} 家", it.dupes.len())).size(10).color(C_DIM));
    }
    if it.revised {
        r = r.push(text("改过").size(10).color(C_GOLD));
    }
    r.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(is_stale: bool, fails: i64, newest: Option<i64>) -> SourceRow {
        SourceRow {
            id: "x".into(),
            label: "X".into(),
            is_stale,
            consecutive_fails: fails,
            newest_ms: newest,
            stale_secs: newest.map(|_| 589 * 86400),
            ok: 100,
            ..Default::default()
        }
    }

    #[test]
    fn a_source_that_fetches_fine_but_serves_old_news_is_red() {
        // **N2 的判据。** WSJ 实测陈了 589 天仍返回 200——
        // 「取得到」和「取到的是新的」是两件事
        let (lamp, c, why) = source_lamp(&src(true, 0, Some(1)));
        assert_eq!(lamp, "⚠");
        assert_eq!(c, C_BAD, "必须是红的");
        assert!(why.contains("589") && why.contains("200"), "{why}");
    }

    #[test]
    fn failing_and_stale_are_different_lamps() {
        // 取不到（网络/被拒）和取得到但是陈的，要分得开：
        // 前者查网络，后者查源本身
        let (l1, _, w1) = source_lamp(&src(false, 3, Some(1)));
        assert_eq!(l1, "✕");
        assert!(w1.contains("连续失败"));

        // 从没抓到又是第三种：刚启动而已，不是故障
        let (l2, c2, w2) = source_lamp(&src(false, 0, None));
        assert_eq!(l2, "○");
        assert_ne!(c2, C_BAD, "刚启动不该是红的");
        assert!(w2.contains("还没抓到"));

        // 正常
        assert_eq!(source_lamp(&src(false, 0, Some(1))).0, "●");
    }

    #[test]
    fn a_failure_outranks_staleness_because_it_is_more_actionable() {
        // 同时又失败又陈：先说失败——取不到的话，陈不陈是后话
        assert_eq!(source_lamp(&src(true, 2, Some(1))).0, "✕");
    }

    #[test]
    fn a_clickable_title_looks_clickable_before_the_mouse_gets_there() {
        use iced::widget::button::Status;
        let t = iced::Theme::Dark;
        let active = link_style(&t, Status::Active);
        let hover = link_style(&t, Status::Hovered);

        // 静止时就该和正文不一样——不能等悬停才显形
        assert_ne!(active.text_color, C_TXT);
        assert_ne!(active.text_color, C_DIM);
        assert_eq!(active.text_color, C_LINK);

        // 悬停有两个变化：变亮 + 出现背景块。
        // 只靠颜色变化的话，对色觉不敏感的人等于没有反馈
        assert_ne!(hover.text_color, active.text_color);
        assert!(active.background.is_none());
        assert!(hover.background.is_some(), "悬停要给一块背景，颜色变化不够");

        // 按下要有回执，且比悬停更明显
        let pressed = link_style(&t, Status::Pressed);
        assert!(pressed.background.is_some());
    }

    #[test]
    fn nothing_else_in_a_news_row_outshines_the_link() {
        // 整行里最亮的必须是可点的那部分，否则「哪儿能点」又要靠猜。
        //
        // 比的是**这一行里实际用到的颜色**，不是全局的 C_TXT——
        // 拿近白的正文色比，任何一个还认得出是蓝色的链接色都会输，
        // 而正文色在新闻行里根本不出现（标题非链接时用的是 C_DIM）
        fn lum(c: Color) -> f32 {
            (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) * c.a
        }
        let in_a_row = [
            ("时间", C_DIM),
            ("时间是猜的 / 生效时间", C_GOLD),
            ("分级-监管", tier_color("regulator")),
            ("分级-交易所", tier_color("exchange")),
            ("分级-公司", tier_color("corporate")),
            ("分级-媒体", tier_color("media")),
            ("标的", C_HEAD),
        ];
        for (what, c) in in_a_row {
            assert!(lum(C_LINK) > lum(c), "{what} 比链接还亮");
        }
        // 而且要亮出可感知的差距，不是小数点后第三位
        assert!(lum(C_LINK) - lum(C_GOLD) > 0.02, "和最亮的那个只差一点，等于没差");
    }

    #[test]
    fn primary_sources_are_visually_distinct_from_media() {
        // 一个把 Fed 原文和转述画成一样的界面，没抓住重点
        assert_ne!(tier_color("regulator"), tier_color("media"));
        assert_ne!(tier_color("exchange"), tier_color("media"));
        assert_eq!(tier_color("aggregator"), tier_color("media"), "聚合和媒体同档即可");
    }
}
