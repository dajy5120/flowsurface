//! 金融日历面板的渲染（docs/43 §5、K2）。
//!
//! 三页（docs/41 页签锁定视图）：
//! - **月历**：左边月份网格（每格最多 3 个、重要的在前、「+N」），右边选中那天的完整列表。
//!   日历负责概览、列表负责密度。
//! - **全年**：未来 12 个月的小热图，深浅 = 当天重要事件；点一天看那天的列表。
//! - **事件流**：未来 7 / 30 / 90 / 365 天的高密度表（`ui::grid`）。
//!
//! 主区始终减负（docs/42）：筛选、显示时区、选中事件的详情在检查器「属性」页；刷新、数据新旧、
//! 各源覆盖到哪天在「数据」页。**确定性要看得出来**：预计的、按惯例补时刻的、来源已撤下的，
//! 都和已确认的画得不一样（docs/43 §5）。

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::calendar_readout::{self as ro, CalReadout, DispTz, Ev, Filter, View, YearMode};
use crate::ui::widgets::Tone;

#[derive(Debug, Clone)]
pub enum CalMsg {
    SetView(View),
    /// 选中一天。
    Select(NaiveDate),
    PrevMonth,
    NextMonth,
    Today,
    /// 选中一个事件（检查器显示详情）。
    PickEvent(String),
    /// 打开原文。
    Open(String),
    /// 立即刷新：起一次 `ws-news --once calendar`。
    Refresh,
    MaxImportance(u8),
    ToggleKind(String),
    ToggleCountry(String),
    ShowUnlisted(bool),
    Query(String),
    Tz(DispTz),
    StreamDays(i64),
    /// 全年页：滚动 12 个月 / 自然年。
    YearMode(YearMode),
    PrevYear,
    NextYear,
    /// 事件流表（ui::grid）。
    Table(crate::ui::grid::GridMsg),
}

thread_local! {
    /// 事件流表每一行的事件 id（与表的数据行同序，操作格按行号找回）。
    static STREAM_ROWS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

const STREAM_COL_DETAIL: usize = 9;

/// 事件流表的操作格 → 动作。
pub(crate) fn stream_action(row: usize, col: usize) -> Option<CalMsg> {
    if col != STREAM_COL_DETAIL {
        return None;
    }
    STREAM_ROWS.with(|r| r.borrow().get(row).cloned()).map(CalMsg::PickEvent)
}

/// 「整年公布」之外的滚动源：覆盖期天然很短，不能按覆盖期标红（docs/43 §13.3 第 4 条）。
const ROLLING_SOURCES: [&str; 3] = ["treasury-auctions", "binance-exchangeinfo", "calendar-rules"];

// ── 小件 ───────────────────────────────────────────────────────────

/// 重要性：颜色 + 符号（**颜色从不单独承载意义**）。用状态色，不用涨跌色。
pub fn importance_style(i: u8) -> (&'static str, Color) {
    match i {
        0 => ("■", Tone::Danger.color()),
        1 => ("▲", Tone::Warning.color()),
        2 => ("●", Tone::Info.color()),
        _ => ("·", crate::ui::pal::dim()),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

fn chip_on<'a>(label: impl Into<String>, active: bool, msg: CalMsg) -> Element<'a, CalMsg> {
    button(text(label.into()).size(crate::ui::text::s_small()))
        .padding(crate::ui::metrics::pad2(0, 2))
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

fn small<'a>(s: impl Into<String>, c: Color) -> iced::widget::Text<'a> {
    text(s.into()).size(crate::ui::text::s_small()).color(c)
}

fn meta<'a>(s: impl Into<String>, c: Color) -> iced::widget::Text<'a> {
    text(s.into()).size(crate::ui::text::s_meta()).color(c)
}

fn weekday_cn(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "周一",
        Weekday::Tue => "周二",
        Weekday::Wed => "周三",
        Weekday::Thu => "周四",
        Weekday::Fri => "周五",
        Weekday::Sat => "周六",
        Weekday::Sun => "周日",
    }
}

/// 事件标题前后的确定性 / 状态记号。
fn marks(e: &Ev) -> String {
    let mut s = String::new();
    if let Some(st) = ro::status_label(&e.status) {
        s.push_str(if e.status == "unlisted" { "⊘ " } else { "⚠ " });
        s.push_str(st);
        s.push(' ');
    }
    let (m, _) = ro::precision_mark(&e.precision);
    if !m.is_empty() {
        s.push_str(m);
    }
    s
}

/// 事件文字的颜色：撤下的灰掉；预计的比确认的暗一档。
fn event_color(e: &Ev) -> Color {
    if e.status == "unlisted" {
        return crate::ui::pal::dim();
    }
    let (_, c) = importance_style(e.importance);
    if e.precision == "estimated" { crate::ui::pal::alpha(c, 0.7) } else { c }
}

/// 某天之后、哪些「整年公布」的源还没公布（BLS 只到年底：明年的 CPI 不是没有，是还没排）。
fn not_published_yet(snap: &CalReadout, d: NaiveDate) -> Vec<&ro::CalSource> {
    snap.sources
        .iter()
        .filter(|s| !ROLLING_SOURCES.contains(&s.id.as_str()))
        .filter(|s| {
            s.coverage_to
                .as_deref()
                .and_then(|c| NaiveDate::parse_from_str(c, "%Y-%m-%d").ok())
                .is_some_and(|c| d > c)
        })
        .collect()
}

// ── 入口 ───────────────────────────────────────────────────────────

pub fn pane_body<'a>(lock: Option<&str>) -> Element<'a, CalMsg> {
    if let Some(x) = lock.and_then(|l| View::ALL.into_iter().find(|v| v.label() == l))
        && ro::view() != x
    {
        ro::set_view(x);
    }
    let snap = ro::snapshot();
    let f = ro::filter();
    let part = super::inspector_props::part();
    let mut body = column![].spacing(crate::ui::metrics::space(2)).padding(if part.side() { 0.0 } else { crate::ui::metrics::space(3) });

    if part.props() {
        body = body.push(props_view(&snap, &f));
    }
    if part.data() {
        body = body.push(data_view(&snap));
    }
    if part.side() {
        return body.into();
    }

    let v = ro::view();
    if lock.is_none() {
        body = body.push(crate::ui::widgets::segmented(
            &[("月历", View::Month), ("全年", View::Year), ("事件流", View::Stream)],
            &v,
            CalMsg::SetView,
        ));
    }
    if !snap.present {
        body = body.push(crate::ui::mark::here()).push(crate::ui::widgets::empty(
            "还没有日历数据",
            "打开这一页时会自动抓一次（ws-news --once calendar，约 10 秒）；也可以在检查器「数据」页点「立即刷新」。",
        ));
        return body.width(Length::Fill).height(Length::Fill).into();
    }
    let content: Element<'a, CalMsg> = match v {
        View::Month => month_view(&snap, &f),
        View::Year => year_view(&snap, &f),
        View::Stream => stream_view(&snap, &f),
    };
    body.push(content).width(Length::Fill).height(Length::Fill).into()
}

// ── 月历 ───────────────────────────────────────────────────────────

fn filter_summary(f: &Filter) -> String {
    let imp = match f.max_importance {
        0 => "只看 P0".to_string(),
        3 => "全部重要性".to_string(),
        i => format!("重要性 ≤ {}", ro::importance_label(i)),
    };
    let mut s = format!("{} · {}", f.tz.label(), imp);
    let n = f.changed().len();
    if n > 0 {
        s.push_str(&format!(" · 已改 {n} 项筛选（检查器「属性」）"));
    }
    s
}

fn month_view<'a>(snap: &CalReadout, f: &Filter) -> Element<'a, CalMsg> {
    let (y, m) = ro::cursor(snap);
    let first = NaiveDate::from_ymd_opt(y, m, 1).unwrap_or_else(|| ro::today(snap));
    let next = if m == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { NaiveDate::from_ymd_opt(y, m + 1, 1) }.unwrap_or(first);
    let last = next - Duration::days(1);
    let nav = row![
        crate::ui::widgets::btn("‹", crate::ui::widgets::Kind::Subtle, Some(CalMsg::PrevMonth)),
        text(format!("{y} 年 {m} 月")).size(crate::ui::text::s_section()).color(crate::ui::pal::txt()),
        crate::ui::widgets::btn("›", crate::ui::widgets::Kind::Subtle, Some(CalMsg::NextMonth)),
        crate::ui::widgets::btn("今天", crate::ui::widgets::Kind::Subtle, Some(CalMsg::Today)),
        meta(filter_summary(f), crate::ui::pal::dim()),
    ]
    .spacing(crate::ui::metrics::space(2))
    .align_y(Alignment::Center)
    .wrap();
    // 内容从翻月这一行之后开始（非内容高度闸门，docs/42 第 4 期）
    let mut col = column![nav, crate::ui::mark::here()].spacing(crate::ui::metrics::space(2));
    // 这个月里哪些整年公布的源已经到头了：后面的日子不是没事，是还没公布
    let gone = not_published_yet(snap, last);
    if !gone.is_empty() {
        let names: Vec<String> =
            gone.iter().map(|s| format!("{}（到 {}）", s.label, s.coverage_to.clone().unwrap_or_default())).collect();
        col = col.push(meta(format!("⚠ 尚未公布：{} 之后的日程还没排出来，不是没有事件", names.join("、")), crate::ui::pal::warn()));
    }

    // 网格：周一开头，6 周
    let today = ro::today(snap);
    let sel = ro::selected(snap);
    let start = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    let mut head = row![].spacing(crate::ui::metrics::space(1));
    for w in ["一", "二", "三", "四", "五", "六", "日"] {
        head = head.push(container(meta(w, crate::ui::pal::dim())).width(Length::Fill).align_x(Alignment::Center));
    }
    let mut grid = column![head].spacing(crate::ui::metrics::space(1)).height(Length::Fill);
    for wk in 0..6 {
        let mut r = row![].spacing(crate::ui::metrics::space(1)).height(Length::Fill);
        for dd in 0..7 {
            let d = start + Duration::days(wk * 7 + dd);
            r = r.push(month_cell(snap, f, d, d.month() == m, d == today, d == sel));
        }
        grid = grid.push(r);
    }
    let left = container(grid).width(Length::FillPortion(3)).height(Length::Fill);
    let right = container(crate::ui::scroll(day_list(snap, f, sel))).width(Length::FillPortion(2)).height(Length::Fill);
    col.push(row![left, right].spacing(crate::ui::metrics::space(3)).height(Length::Fill)).height(Length::Fill).into()
}

fn month_cell<'a>(snap: &CalReadout, f: &Filter, d: NaiveDate, in_month: bool, is_today: bool, is_sel: bool) -> Element<'a, CalMsg> {
    let evs = ro::events_on(snap, d, f);
    let top = ro::top_for_cell(&evs);
    let day_c = if is_today { crate::ui::pal::accent() } else if in_month { crate::ui::pal::txt() } else { crate::ui::pal::dim() };
    let mut c = column![row![
        text(d.day().to_string()).size(crate::ui::text::s_body()).color(day_c),
        meta(if is_today { "今天" } else { "" }, crate::ui::pal::accent()),
    ]
    .spacing(crate::ui::metrics::space(1))]
    .spacing(0);
    for e in top.iter().take(3) {
        let (g, _) = importance_style(e.importance);
        let t = if e.scheduled_at.is_some() { ro::display_time(e, f.tz) } else { String::new() };
        let line = format!("{g} {}{} {}", marks(e), t, clip(&ro::display_title(e), 16));
        // 非本月的日子：事件也淡一档，和本月的分得开
        c = c.push(meta(line, if in_month { event_color(e) } else { crate::ui::pal::alpha(event_color(e), 0.55) }));
    }
    if top.len() > 3 {
        c = c.push(meta(format!("+{} 个", top.len() - 3), crate::ui::pal::dim()));
    }
    let bg_sel = is_sel;
    button(container(c).width(Length::Fill).height(Length::Fill).clip(true))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(crate::ui::metrics::pad2(1, 1))
        .on_press(CalMsg::Select(d))
        .style(move |th, st| {
            let mut s = crate::ui::widgets::button_style(crate::ui::widgets::Kind::Ghost, th, st);
            let core = crate::ui::core();
            s.border = if is_today || bg_sel {
                Border { width: 1.0, color: crate::ui::color(core.accent_primary), radius: crate::ui::metrics::radius::SM.into() }
            } else {
                crate::ui::metrics::hairline_subtle(crate::ui::metrics::radius::SM)
            };
            if bg_sel {
                s.background = Some(Background::Color(crate::ui::color(core.accent_soft)));
            }
            s
        })
        .into()
}

/// 选中那天的完整列表。
fn day_list<'a>(snap: &CalReadout, f: &Filter, d: NaiveDate) -> Element<'a, CalMsg> {
    let evs = ro::events_on(snap, d, f);
    let all_n = snap.events.iter().filter(|e| ro::display_date(e, f.tz) == Some(d)).count();
    let mut col = column![row![
        text(format!("{} 月 {} 日 {}", d.month(), d.day(), weekday_cn(d.weekday()))).size(crate::ui::text::s_section()).color(crate::ui::pal::txt()),
        meta(
            if all_n > evs.len() { format!("{} 个事件（筛掉 {}）", evs.len(), all_n - evs.len()) } else { format!("{} 个事件", evs.len()) },
            crate::ui::pal::dim()
        ),
    ]
    .spacing(crate::ui::metrics::space(2))
    .align_y(Alignment::Center)]
    .spacing(crate::ui::metrics::space(1));
    let gone = not_published_yet(snap, d);
    if !gone.is_empty() {
        for s in gone {
            col = col.push(meta(
                format!("尚未公布：{}（{}）只公布到 {}", s.label, s.org, s.coverage_to.clone().unwrap_or_default()),
                crate::ui::pal::warn(),
            ));
        }
    }
    if evs.is_empty() {
        col = col.push(meta(
            if all_n > 0 { "这一天的事件都被筛选挡住了（检查器「属性」页）" } else { "这一天没有已知事件" },
            crate::ui::pal::dim(),
        ));
    }
    let picked = ro::selected_event();
    for e in evs {
        col = col.push(event_row(e, f.tz, picked.as_deref() == Some(e.id.as_str())));
    }
    col.into()
}

fn event_row<'a>(e: &Ev, tz: DispTz, picked: bool) -> Element<'a, CalMsg> {
    let (g, ic) = importance_style(e.importance);
    let mut title = ro::display_title(e);
    if let Some(sp) = e.span_start
        && Some(sp) != e.date_local
    {
        title.push_str(&format!("（会期 {} 起）", sp.format("%m-%d")));
    }
    let m = marks(e);
    let r = row![
        container(small(ro::display_time(e, tz), crate::ui::pal::txt())).width(Length::Fixed(96.0)),
        container(small(format!("{g} {}", ro::importance_label(e.importance)), ic)).width(Length::Fixed(40.0)),
        container(small(ro::country_label(&e.country).to_string(), crate::ui::pal::dim())).width(Length::Fixed(40.0)),
        container(small(title, event_color(e))).width(Length::Fill),
        meta(m, if e.status == "scheduled" { crate::ui::pal::dim() } else { crate::ui::pal::warn() }),
    ]
    .spacing(crate::ui::metrics::space(2))
    .align_y(Alignment::Center);
    button(r)
        .width(Length::Fill)
        .padding(crate::ui::metrics::pad2(0, 1))
        .on_press(CalMsg::PickEvent(e.id.clone()))
        .style(move |th, st| {
            let mut s = crate::ui::widgets::button_style(crate::ui::widgets::Kind::Ghost, th, st);
            if picked {
                s.background = Some(Background::Color(crate::ui::color(crate::ui::core().accent_soft)));
            }
            s
        })
        .into()
}

// ── 全年 ───────────────────────────────────────────────────────────

fn year_view<'a>(snap: &CalReadout, f: &Filter) -> Element<'a, CalMsg> {
    let today = ro::today(snap);
    let sel = ro::selected(snap);
    let mode = ro::year_mode();
    // 12 个（年, 月）：滚动 = 本月起往后 12 个月；自然年 = 那一年的 1–12 月
    let (first_idx, title) = match mode {
        YearMode::Rolling => (today.year() * 12 + today.month0() as i32, format!("{} 起 12 个月", today.format("%Y-%m"))),
        YearMode::Natural => {
            let y = ro::natural_year(snap);
            (y * 12, format!("{y} 年"))
        }
    };
    let mut head = row![crate::ui::widgets::segmented(
        &[("滚动 12 个月", YearMode::Rolling), ("自然年", YearMode::Natural)],
        &mode,
        CalMsg::YearMode,
    )]
    .spacing(crate::ui::metrics::space(3))
    .align_y(Alignment::Center);
    if mode == YearMode::Natural {
        head = head
            .push(crate::ui::widgets::btn("‹", crate::ui::widgets::Kind::Subtle, Some(CalMsg::PrevYear)))
            .push(text(title).size(crate::ui::text::s_section()).color(crate::ui::pal::txt()))
            .push(crate::ui::widgets::btn("›", crate::ui::widgets::Kind::Subtle, Some(CalMsg::NextYear)));
    } else {
        head = head.push(text(title).size(crate::ui::text::s_section()).color(crate::ui::pal::txt()));
    }
    head = head
        .push(meta("■ 有 P0", Tone::Danger.color()))
        .push(meta("▲ 有 P1", Tone::Warning.color()))
        .push(meta("● 其他事件（越多越深）", Tone::Info.color()))
        .push(meta(filter_summary(f), crate::ui::pal::dim()));
    let mut body = column![head.wrap(), crate::ui::mark::here()].spacing(crate::ui::metrics::space(3));
    // 翻到快照没带的日子：说清楚是「快照里没有」，不是「那几个月没事」
    if let Some((from, to)) = ro::window(snap) {
        let lo = NaiveDate::from_ymd_opt(first_idx.div_euclid(12), first_idx.rem_euclid(12) as u32 + 1, 1);
        let hi_idx = first_idx + 11;
        let hi = NaiveDate::from_ymd_opt(hi_idx.div_euclid(12), hi_idx.rem_euclid(12) as u32 + 1, 28);
        if lo.is_some_and(|d| d < from) || hi.is_some_and(|d| d > to) {
            body = body.push(meta(
                format!("⚠ 日历数据只含 {from} 至 {to}（今年年初到明年年底），范围外的日子空着不代表没有事件"),
                crate::ui::pal::warn(),
            ));
        }
    }
    // 布局（2026-10-09 用户）：日历区约 80% 高、固定 3 行 × 4 个月、随窗口放大；当天事件约 20%、在自己的区域里滚
    let mut grid = column![].spacing(crate::ui::metrics::space(3)).height(Length::FillPortion(4));
    for row_i in 0..3 {
        let mut r = row![].spacing(crate::ui::metrics::space(4)).height(Length::Fill);
        for col_i in 0..4 {
            let idx = first_idx + row_i * 4 + col_i;
            let (y, m) = (idx.div_euclid(12), idx.rem_euclid(12) as u32 + 1);
            r = r.push(mini_month(snap, f, y, m, today, sel));
        }
        grid = grid.push(r);
    }
    let events = container(crate::ui::scroll(day_list(snap, f, sel)).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::FillPortion(1));
    body.push(grid).push(events).width(Length::Fill).height(Length::Fill).into()
}

fn mini_month<'a>(snap: &CalReadout, f: &Filter, y: i32, m: u32, today: NaiveDate, sel: NaiveDate) -> Element<'a, CalMsg> {
    let Some(first) = NaiveDate::from_ymd_opt(y, m, 1) else { return column![].into() };
    // 格子随区域伸缩（宽高都 Fill）：窗口越大、月历越大
    let mut col = column![small(format!("{y} 年 {m} 月"), crate::ui::pal::head())].spacing(crate::ui::metrics::space(1)).width(Length::Fill).height(Length::Fill);
    let mut head = row![].spacing(1);
    for w in ["一", "二", "三", "四", "五", "六", "日"] {
        head = head.push(container(meta(w, crate::ui::pal::dim())).width(Length::Fill).align_x(Alignment::Center));
    }
    col = col.push(head);
    let start = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    for wk in 0..6 {
        let mut r = row![].spacing(1).height(Length::Fill);
        for dd in 0..7 {
            let d = start + Duration::days(wk * 7 + dd);
            if d.month() != m {
                r = r.push(container(text("")).width(Length::Fill).height(Length::Fill));
                continue;
            }
            let evs = ro::events_on(snap, d, f);
            let best = evs.iter().map(|e| e.importance).min();
            let fill = match best {
                Some(0) => Some(crate::ui::pal::alpha(Tone::Danger.color(), 0.55)),
                Some(1) => Some(crate::ui::pal::alpha(Tone::Warning.color(), 0.45)),
                Some(_) => Some(crate::ui::pal::alpha(Tone::Info.color(), 0.12 * evs.len().min(4) as f32)),
                None => None,
            };
            let mark = match best {
                Some(0) => "■",
                Some(1) => "▲",
                _ => "",
            };
            let (is_today, is_sel) = (d == today, d == sel);
            let label = if mark.is_empty() { d.day().to_string() } else { format!("{}", d.day()) };
            r = r.push(
                button(container(small(label, crate::ui::pal::txt())).center_x(Length::Fill).center_y(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(Padding::ZERO)
                    .on_press(CalMsg::Select(d))
                    .style(move |th, st| {
                        let mut s = crate::ui::widgets::button_style(crate::ui::widgets::Kind::Ghost, th, st);
                        if let Some(c) = fill {
                            s.background = Some(Background::Color(c));
                        }
                        if is_today || is_sel {
                            s.border = Border {
                                width: if is_sel { 2.0 } else { 1.0 },
                                color: crate::ui::color(crate::ui::core().accent_primary),
                                radius: crate::ui::metrics::radius::SM.into(),
                            };
                        }
                        s
                    }),
            );
        }
        col = col.push(r);
    }
    container(col).width(Length::Fill).height(Length::Fill).padding(crate::ui::metrics::pad2(1, 1)).into()
}

// ── 事件流 ─────────────────────────────────────────────────────────

fn countdown(e: &Ev, now_ms: i64) -> String {
    let Some(t) = e.scheduled_at else { return crate::ui::fmt::missing() };
    let dt = t - now_ms;
    if dt < 0 {
        return "已过".into();
    }
    let mins = dt / 60_000;
    let (dd, hh, mm) = (mins / 1440, mins % 1440 / 60, mins % 60);
    if dd > 0 {
        format!("{dd} 天 {hh} 时")
    } else if hh > 0 {
        format!("{hh} 时 {mm} 分")
    } else {
        format!("{mm} 分")
    }
}

fn stream_view<'a>(snap: &CalReadout, f: &Filter) -> Element<'a, CalMsg> {
    use crate::ui::grid::{Cell, Column, Fit};
    let today = ro::today(snap);
    let end = today + Duration::days(f.stream_days);
    let now = chrono::Local::now().timestamp_millis();
    let mut evs: Vec<&Ev> = snap
        .events
        .iter()
        .filter(|e| ro::display_date(e, f.tz).is_some_and(|d| d >= today && d <= end) && f.pass(e))
        .collect();
    evs.sort_by_key(|e| (ro::display_date(e, f.tz), e.scheduled_at.unwrap_or(i64::MIN), e.importance));
    let head = row![
        crate::ui::widgets::segmented(&[("7 天", 7_i64), ("30 天", 30), ("90 天", 90), ("一年", 365)], &f.stream_days, CalMsg::StreamDays),
        meta(format!("{} 个事件 · {}", evs.len(), filter_summary(f)), crate::ui::pal::dim()),
    ]
    .spacing(crate::ui::metrics::space(3))
    .align_y(Alignment::Center)
    .wrap();
    let cols = vec![
        Column::text("日期", 96.0).groupable(),
        Column::text("时间", 104.0),
        Column::text("倒计时", 84.0),
        Column::text("地区", 56.0).groupable(),
        Column::text("重要性", 60.0).groupable(),
        Column::text("事件", 320.0),
        Column::text("类别", 84.0).groupable(),
        Column::text("确定性", 90.0),
        Column::text("来源", 200.0),
        Column::text("", 60.0),
    ];
    let mut data = Vec::with_capacity(evs.len());
    let mut ids = Vec::with_capacity(evs.len());
    for e in &evs {
        let (g, ic) = importance_style(e.importance);
        let d = ro::display_date(e, f.tz).unwrap_or(today);
        let prec = ro::precision_short(&e.precision);
        let prec = match ro::status_label(&e.status) {
            Some(s) => format!("{s} · {prec}"),
            None => prec.to_string(),
        };
        // 来源写中文名（源管理里的名字），不写 id
        let src: Vec<String> =
            e.sources.iter().map(|id| snap.sources.iter().find(|s| &s.id == id).map_or_else(|| id.clone(), |s| s.label.clone())).collect();
        data.push(vec![
            Cell::Text(format!("{} {}", d.format("%m-%d"), weekday_cn(d.weekday()))),
            Cell::Text(ro::display_time(e, f.tz)),
            Cell::Text(countdown(e, now)),
            Cell::Text(ro::country_label(&e.country).to_string()),
            Cell::Colored(format!("{g} {}", ro::importance_label(e.importance)), ic),
            Cell::Colored(ro::display_title(e), event_color(e)),
            Cell::Text(ro::kind_label(&e.kind).to_string()),
            Cell::Colored(prec, if e.precision == "exact" && e.status == "scheduled" { crate::ui::pal::dim() } else { crate::ui::pal::warn() }),
            Cell::Text(src.join(" · ")),
            Cell::Action("详情".into(), Tone::Neutral),
        ]);
        ids.push(e.id.clone());
    }
    STREAM_ROWS.with(|r| *r.borrow_mut() = ids);
    column![head, crate::ui::mark::here(), crate::ui::grid::named("calendar.stream", cols, data, Fit::Fill, None, CalMsg::Table)]
        .spacing(crate::ui::metrics::space(2))
        .height(Length::Fill)
        .into()
}

// ── 检查器「属性」：筛选 + 选中事件详情 ───────────────────────────────

fn props_view<'a>(snap: &CalReadout, f: &Filter) -> Element<'a, CalMsg> {
    let mut col = column![crate::ui::widgets::section("显示")].spacing(crate::ui::metrics::space(2));
    col = col.push(crate::ui::widgets::kv(
        "时区",
        crate::ui::widgets::segmented(&[("本地", DispTz::Local), ("UTC", DispTz::Utc), ("事件当地", DispTz::Event)], &f.tz, CalMsg::Tz),
        72.0,
    ));
    col = col.push(crate::ui::widgets::kv(
        "重要性",
        crate::ui::widgets::segmented(&[("P0", 0_u8), ("≤P1", 1), ("≤P2", 2), ("全部", 3)], &f.max_importance, CalMsg::MaxImportance),
        72.0,
    ));
    let mut kinds = row![].spacing(crate::ui::metrics::space(1));
    for k in ro::KINDS {
        let on = !f.hidden_kinds.iter().any(|x| x == k);
        kinds = kinds.push(chip_on(ro::kind_label(k), on, CalMsg::ToggleKind(k.to_string())));
    }
    col = col.push(crate::ui::widgets::kv("类别", kinds.wrap().into(), 72.0));
    let mut countries: Vec<&str> = snap.events.iter().map(|e| e.country.as_str()).collect();
    countries.sort_unstable();
    countries.dedup();
    let mut cr = row![].spacing(crate::ui::metrics::space(1));
    for c in countries {
        let on = !f.hidden_countries.iter().any(|x| x == c);
        cr = cr.push(chip_on(ro::country_label(c).to_string(), on, CalMsg::ToggleCountry(c.to_string())));
    }
    col = col.push(crate::ui::widgets::kv("地区", cr.wrap().into(), 72.0));
    col = col.push(crate::ui::widgets::kv(
        "已撤下",
        chip_on(if f.show_unlisted { "显示（灰色、带 ⊘）" } else { "隐藏" }, f.show_unlisted, CalMsg::ShowUnlisted(!f.show_unlisted)),
        72.0,
    ));
    col = col.push(crate::ui::widgets::kv(
        "搜索",
        text_input("标题 / 系列 / 标的，空格 = 都要有", &f.query)
            .on_input(CalMsg::Query)
            .size(crate::ui::text::s_small())
            .padding(crate::ui::metrics::pad2(0, 2))
            .into(),
        72.0,
    ));
    col = col.push(meta(
        "确定性：≈ = 时刻按惯例（来源只给日期）；预计 = 规则推算；⊘ = 列过它的来源都撤下了；⚠ = 规则与官方不符",
        crate::ui::pal::dim(),
    ));

    // 选中的事件
    let Some(id) = ro::selected_event() else {
        return col.push(meta("点列表里的事件看详情", crate::ui::pal::dim())).into();
    };
    let Some(e) = snap.events.iter().find(|e| e.id == id) else {
        return col.push(meta("选中的事件不在当前快照的窗口里", crate::ui::pal::dim())).into();
    };
    let kv = |k: &str, v: String, c: Color| crate::ui::widgets::kv(k.to_string(), small(v, c).into(), 72.0);
    let (g, ic) = importance_style(e.importance);
    let (_, prec) = ro::precision_mark(&e.precision);
    col = col
        .push(crate::ui::widgets::section("选中的事件"))
        .push(text(ro::display_title(e)).size(crate::ui::text::s_emph()).color(crate::ui::pal::txt()))
        .push(kv("时间", ro::all_times(e), crate::ui::pal::txt()))
        .push(kv("确定性", prec.to_string(), if e.precision == "exact" { crate::ui::pal::txt() } else { crate::ui::pal::warn() }));
    if let Some(sp) = e.span_start {
        col = col.push(kv("会期", format!("{} 至 {}", sp, e.date_local.map(|d| d.to_string()).unwrap_or_default()), crate::ui::pal::txt()));
    }
    if let Some(st) = ro::status_label(&e.status) {
        col = col.push(kv("状态", st.to_string(), crate::ui::pal::warn()));
    }
    col = col
        .push(kv("重要性", format!("{g} {}", ro::importance_label(e.importance)), ic))
        .push(kv("类别", format!("{} · {}", ro::kind_label(&e.kind), ro::country_label(&e.country)), crate::ui::pal::txt()));
    if ro::display_title(e) != e.title {
        col = col.push(kv("原标题", e.title.clone(), crate::ui::pal::dim()));
    }
    if let Some(s) = &e.series {
        col = col.push(kv("系列", s.clone(), crate::ui::pal::dim()));
    }
    if !e.assets.is_empty() {
        col = col.push(kv("关联标的", e.assets.join(" · "), crate::ui::pal::txt()));
    }
    let src: Vec<String> = e
        .sources
        .iter()
        .map(|id| match snap.sources.iter().find(|s| &s.id == id) {
            Some(s) => format!("{}（{}）", s.label, s.org),
            None => id.clone(),
        })
        .collect();
    col = col.push(kv("来源", src.join("、"), crate::ui::pal::txt()));
    if let Some(n) = &e.note {
        col = col.push(kv("备注", n.clone(), crate::ui::pal::dim()));
    }
    if let Some(u) = &e.url {
        col = col.push(crate::ui::widgets::kv(
            "原文",
            crate::ui::widgets::btn(clip(u, 48), crate::ui::widgets::Kind::Ghost, Some(CalMsg::Open(u.clone()))),
            72.0,
        ));
    }
    let revs = ro::selected_revisions();
    col = col.push(crate::ui::widgets::section(format!("修订历史（{}）", e.revisions)));
    if revs.is_empty() {
        col = col.push(meta("没有改过", crate::ui::pal::dim()));
    }
    for (field, old, new, source, at) in revs {
        let when = chrono::DateTime::from_timestamp_millis(at)
            .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let field = match field.as_str() {
            "date_local" => "日期",
            "time_local" => "时刻",
            "title" => "标题",
            "precision" => "确定性",
            "status" => "状态",
            "span_start" => "会期",
            other => other,
        };
        col = col.push(meta(
            format!("{when} · {field}：{} → {}（{source}）", old.unwrap_or_else(|| "—".into()), new.unwrap_or_else(|| "—".into())),
            crate::ui::pal::txt(),
        ));
    }
    col.into()
}

// ── 检查器「数据」：刷新 + 各源覆盖期 ─────────────────────────────────

fn data_view<'a>(snap: &CalReadout) -> Element<'a, CalMsg> {
    let now = chrono::Local::now().timestamp_millis();
    let age = if snap.present {
        let h = (now - snap.generated_ms) / 3_600_000;
        let m = (now - snap.generated_ms) / 60_000 % 60;
        format!("日历数据生成于 {h} 小时 {m} 分前")
    } else {
        "还没有日历数据".into()
    };
    let stale = ro::needs_refresh(snap.present, snap.generated_ms, now);
    let mut col = column![
        row![
            text("金融日历").size(crate::ui::text::s_emph()).color(crate::ui::pal::txt()),
            crate::ui::widgets::btn_busy("⇩ 立即刷新", crate::ui::widgets::Kind::Standard, Some(CalMsg::Refresh), super::once::CALENDAR.running()),
        ]
        .spacing(crate::ui::metrics::space(3))
        .align_y(Alignment::Center),
        meta(age, if stale { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
    ]
    .spacing(crate::ui::metrics::space(2));
    let st = super::once::CALENDAR.status();
    if !st.is_empty() {
        col = col.push(meta(st, crate::ui::pal::info()));
    }
    let auto = ro::auto_note();
    if !auto.is_empty() {
        col = col.push(meta(auto, crate::ui::pal::dim()));
    }
    col = col.push(meta(
        "抓取在资讯守护 ws-news 里（与新闻同一个守护、同一张源表）。打开日历时数据超过 12 小时会自动刷新一次；不常驻、不定时。",
        crate::ui::pal::dim(),
    ));
    col = col.push(crate::ui::widgets::section("各源覆盖到哪天"));
    for s in &snap.sources {
        let rolling = ROLLING_SOURCES.contains(&s.id.as_str());
        let (txt, c) = match (s.coverage_to.as_deref(), s.coverage_days_left) {
            (Some(to), Some(l)) if !rolling && l < 30 => (format!("到 {to}（只剩 {l} 天）"), crate::ui::pal::warn()),
            (Some(to), Some(l)) => (format!("到 {to}（{l} 天）{}", if rolling { " · 滚动发布" } else { "" }), crate::ui::pal::dim()),
            _ => ("还没抓到".into(), crate::ui::pal::warn()),
        };
        col = col.push(
            column![
                row![small(format!("{} · {}", s.org, s.label), crate::ui::pal::txt()), meta(txt, c)]
                    .spacing(crate::ui::metrics::space(2))
                    .wrap(),
                meta(s.last_report.clone(), crate::ui::pal::dim()),
            ]
            .spacing(0),
        );
    }
    col = col.push(meta("源的启停、测试、加 ICS 订阅在「新闻资讯｜源管理」（按机构分组，用途列可筛「日历」）。", crate::ui::pal::dim()));
    col.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: &str, to: &str) -> ro::CalSource {
        ro::CalSource { id: id.into(), label: id.into(), coverage_to: Some(to.into()), ..Default::default() }
    }

    #[test]
    fn days_after_a_yearly_source_runs_out_say_not_published_yet() {
        // docs/43 K2 判据：BLS 只公布到 2026-12-30——明年的日子要说「尚未公布」，不是空着
        let snap = CalReadout {
            present: true,
            sources: vec![src("bls-schedule", "2026-12-30"), src("treasury-auctions", "2026-10-13"), src("calendar-rules", "2027-10-29")],
            ..Default::default()
        };
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let ids = |x: NaiveDate| not_published_yet(&snap, x).iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert!(ids(d("2026-12-30")).is_empty());
        assert_eq!(ids(d("2027-01-13")), ["bls-schedule"]);
        // 滚动发布的源（国债只列一周）覆盖期短是常态，不算「尚未公布」
        assert!(!ids(d("2026-11-01")).contains(&"treasury-auctions".to_string()));
    }
}
