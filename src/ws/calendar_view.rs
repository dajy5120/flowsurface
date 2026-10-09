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
use iced::widget::{button, column, container, pick_list, row, text, text_input};
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
    // ── 提醒（docs/43 K4）──
    /// 一键启用预设模板（序号）。
    AlertPreset(usize),
    AlertToggle(i64, bool),
    AlertDelete(i64),
    /// 为选中的事件单独设提醒（检查器详情里的按钮）。
    AlertForEvent(String),
    DraftScope(super::calendar_alerts::Scope),
    DraftTarget(String),
    DraftImportance(u8),
    DraftOffset(i64),
    DraftChannel(super::calendar_alerts::Channel),
    DraftOnChange(bool),
    DraftOnRelease(bool),
    DraftAdd,
    /// 通知中心里「错过」的标已读。
    AckMissed,
    QuietFrom(String),
    QuietTo(String),
    QuietP0(bool),
    QuietSave,
    /// 发一条 Telegram 测试消息（交给 ws-telegram 发）。
    TelegramTest,
    TgToken(String),
    TgChat(String),
    TgImp(u8),
    TgSave,
    /// 保存，再让 ws-telegram 测试连通（getMe + getChat，不发消息）。
    TgSaveProbe,
    /// 让 ws-telegram 从最近消息里读会话 ID（getUpdates）。
    TgDetect,
    TgEnable(bool),
    RulesTable(crate::ui::grid::GridMsg),
    NoticesTable(crate::ui::grid::GridMsg),
    /// 事件流表（ui::grid）。
    Table(crate::ui::grid::GridMsg),
}

thread_local! {
    /// 事件流表每一行的事件 id（与表的数据行同序，操作格按行号找回）。
    static STREAM_ROWS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// 规则表每一行的（规则 id, 启用着吗）。
    static RULE_ROWS: std::cell::RefCell<Vec<(i64, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

const RULE_COL_TOGGLE: usize = 7;
const RULE_COL_DELETE: usize = 8;

/// 规则表的操作格 → 动作。
pub(crate) fn rule_action(row: usize, col: usize) -> Option<CalMsg> {
    let (id, on) = RULE_ROWS.with(|r| r.borrow().get(row).copied())?;
    match col {
        RULE_COL_TOGGLE => Some(CalMsg::AlertToggle(id, !on)),
        RULE_COL_DELETE => Some(CalMsg::AlertDelete(id)),
        _ => None,
    }
}

const STREAM_COL_DETAIL: usize = 12;

/// 事件流表的操作格 → 动作。
pub(crate) fn stream_action(row: usize, col: usize) -> Option<CalMsg> {
    if col != STREAM_COL_DETAIL {
        return None;
    }
    STREAM_ROWS.with(|r| r.borrow().get(row).cloned()).map(CalMsg::PickEvent)
}


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

/// 可多选的小按钮。选中的**前面打 ✓**、强调色淡底——只靠底色深浅，选没选几乎看不出来（K4 截图里实测），
/// 而且颜色不能单独承载意义。
fn chip_on<'a>(label: impl Into<String>, active: bool, msg: CalMsg) -> Element<'a, CalMsg> {
    let label: String = label.into();
    let shown = if active { format!("✓ {label}") } else { label };
    button(text(shown).size(crate::ui::text::s_small()))
        .padding(crate::ui::metrics::pad2(0, 2))
        .style(move |t, st| {
            let mut s = crate::style::button::modifier(t, st, active);
            if active {
                s.background = Some(Background::Color(crate::ui::color(crate::ui::core().accent_soft)));
            }
            s
        })
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
        s.push_str(match e.status.as_str() {
            "unlisted" => "⊘ ",
            "released" => "✓ ",
            _ => "⚠ ",
        });
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
        // 滚动源（国债一周、Nasdaq 两周…）覆盖期短是常态，由守护在快照里标（docs/43 §13.3 第 4 条）
        .filter(|s| !s.rolling)
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
            &[("月历", View::Month), ("全年", View::Year), ("事件流", View::Stream), ("提醒", View::Alerts)],
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
        View::Alerts => alerts_view(&snap),
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
    let vals = e.headline().map(ro::value_text).filter(|t| !t.is_empty());
    let mut title_col = column![small(title, event_color(e))];
    if let Some(v) = vals {
        title_col = title_col.push(meta(v, crate::ui::pal::txt()));
    }
    let r = row![
        container(small(ro::display_time(e, tz), crate::ui::pal::txt())).width(Length::Fixed(96.0)),
        container(small(format!("{g} {}", ro::importance_label(e.importance)), ic)).width(Length::Fixed(40.0)),
        container(small(ro::country_label(&e.country).to_string(), crate::ui::pal::dim())).width(Length::Fixed(40.0)),
        container(title_col).width(Length::Fill),
        meta(m, if matches!(e.status.as_str(), "scheduled" | "released") { crate::ui::pal::dim() } else { crate::ui::pal::warn() }),
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
        Column::text("实际", 80.0),
        Column::text("预期", 80.0),
        Column::text("前值", 80.0),
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
            Cell::Text(e.headline().and_then(|v| v.actual.clone()).unwrap_or_default()),
            Cell::Text(e.headline().and_then(|v| v.consensus.clone()).unwrap_or_default()),
            Cell::Text(e.headline().and_then(|v| v.previous.clone()).unwrap_or_default()),
            Cell::Colored(
                prec,
                // 只有「需要留意」的才用警示色：预计、按惯例、撤下、不符。仅日期是财报 / 休市的常态
                if matches!(e.precision.as_str(), "exact" | "date_only") && matches!(e.status.as_str(), "scheduled" | "released") {
                    crate::ui::pal::dim()
                } else {
                    crate::ui::pal::warn()
                },
            ),
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
    if !e.values.is_empty() {
        col = col.push(crate::ui::widgets::section("数值"));
        for v in &e.values {
            let label = if v.label.is_empty() { "—".to_string() } else { v.label.clone() };
            col = col.push(kv(&label, ro::value_text(v), if v.actual.is_some() { crate::ui::pal::txt() } else { crate::ui::pal::dim() }));
        }
        col = col.push(meta("同一名字出现两次时（CPI 环比与同比都叫 CPI），第二行记为「·2」：来源没有字段区分口径", crate::ui::pal::dim()));
    }
    if let Some(u) = &e.url {
        col = col.push(crate::ui::widgets::kv(
            "原文",
            crate::ui::widgets::btn(clip(u, 48), crate::ui::widgets::Kind::Ghost, Some(CalMsg::Open(u.clone()))),
            72.0,
        ));
    }
    col = col.push(crate::ui::widgets::kv(
        "提醒",
        crate::ui::widgets::btn("为这个事件设提醒", crate::ui::widgets::Kind::Standard, Some(CalMsg::AlertForEvent(e.id.clone()))),
        72.0,
    ));
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
        let rolling = s.rolling;
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


// ── 提醒（docs/43 K4）──────────────────────────────────────────────

/// 下拉里的一项（规则的目标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetOpt {
    pub key: String,
    pub label: String,
}

impl std::fmt::Display for TargetOpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// 某个范围下可选的目标（系列 / 类别 / 地区），从当前快照里的事件取。
fn targets(snap: &CalReadout, scope: super::calendar_alerts::Scope) -> Vec<TargetOpt> {
    use super::calendar_alerts::Scope;
    let mut v: Vec<TargetOpt> = match scope {
        Scope::Series => {
            let mut m: std::collections::BTreeMap<String, String> = Default::default();
            for e in &snap.events {
                if let Some(sr) = &e.series {
                    m.entry(sr.clone()).or_insert_with(|| e.series_name.clone().unwrap_or_else(|| sr.clone()));
                }
            }
            m.into_iter().map(|(key, label)| TargetOpt { key, label }).collect()
        }
        Scope::Kind => ro::KINDS.iter().map(|k| TargetOpt { key: (*k).into(), label: ro::kind_label(k).into() }).collect(),
        Scope::Country => {
            let mut cs: Vec<&str> = snap.events.iter().map(|e| e.country.as_str()).collect();
            cs.sort_unstable();
            cs.dedup();
            cs.into_iter().map(|c| TargetOpt { key: c.into(), label: ro::country_label(c).into() }).collect()
        }
        Scope::All | Scope::Event => Vec::new(),
    };
    v.sort_by(|a, b| a.label.cmp(&b.label));
    v
}

/// 规则的目标写成人话。
fn target_label(snap: &CalReadout, r: &super::calendar_alerts::Rule) -> String {
    use super::calendar_alerts::Scope;
    match r.scope {
        Scope::All => format!("重要性 ≤ {}", ro::importance_label(r.max_importance)),
        Scope::Series => targets(snap, Scope::Series).into_iter().find(|t| t.key == r.target).map_or_else(|| r.target.clone(), |t| t.label),
        Scope::Event => snap.events.iter().find(|e| e.id == r.target).map_or_else(|| r.target.clone(), ro::display_title),
        Scope::Kind => format!("{} · ≤ {}", ro::kind_label(&r.target), ro::importance_label(r.max_importance)),
        Scope::Country => format!("{} · ≤ {}", ro::country_label(&r.target), ro::importance_label(r.max_importance)),
    }
}

fn notice_state(s: &str) -> (&'static str, Color) {
    match s {
        "fired" => ("已发", crate::ui::pal::ok()),
        "missed" => ("错过（Cockpit 关着）", crate::ui::pal::warn()),
        "missed_ack" => ("错过 · 已读", crate::ui::pal::dim()),
        "cancelled" => ("已撤", crate::ui::pal::dim()),
        "suppressed" => ("免打扰挡掉", crate::ui::pal::dim()),
        "pending" => ("待发", crate::ui::pal::info()),
        _ => ("?", crate::ui::pal::dim()),
    }
}

fn delivery_text(d: &[(String, String, Option<String>)]) -> String {
    d.iter()
        .map(|(ch, st, err)| {
            let name = super::calendar_alerts::Channel::from_key(ch).map_or(ch.as_str(), |c| c.label());
            let mark = match st.as_str() {
                "sent" => "✓".to_string(),
                "pending" => "待送".to_string(),
                "uncertain" => format!("不确定（已发出、没收到回复）{}", err.clone().map(|e| format!("：{e}")).unwrap_or_default()),
                "expired" => "过期未送".to_string(),
                "failed" => format!("✗ {}", err.clone().unwrap_or_default()),
                "disabled" => err.clone().unwrap_or_else(|| "关".into()),
                other => other.to_string(),
            };
            format!("{name} {mark}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn alerts_view<'a>(snap: &CalReadout) -> Element<'a, CalMsg> {
    use super::calendar_alerts::{self as al, Channel, Scope};
    use crate::ui::grid::{Cell, Column, Fit};
    let a = al::snapshot();
    let st = al::status();
    let kv = |k: &str, v: Element<'a, CalMsg>| crate::ui::widgets::kv(k.to_string(), v, 72.0);

    // ── 顶部：预设 + 状态 ──
    let mut head = row![meta("一键启用：", crate::ui::pal::dim())].spacing(crate::ui::metrics::space(2)).align_y(Alignment::Center);
    for (i, (name, _)) in al::presets().iter().enumerate() {
        head = head.push(crate::ui::widgets::btn(*name, crate::ui::widgets::Kind::Subtle, Some(CalMsg::AlertPreset(i))));
    }
    let state_txt = if !st.running {
        if st.last_error.is_empty() { "提醒线程没在跑".to_string() } else { st.last_error.clone() }
    } else {
        let next = st
            .next_due
            .and_then(|t| chrono::TimeZone::timestamp_millis_opt(&chrono::Local, t).single())
            .map(|t| format!(" · 下一条 {}", t.format("%m-%d %H:%M")))
            .unwrap_or_default();
        format!("待发 {}{next} · 错过 {}", st.pending, st.missed)
    };
    head = head.push(meta(state_txt, if st.missed > 0 { crate::ui::pal::warn() } else { crate::ui::pal::dim() }));
    if st.missed > 0 {
        head = head.push(crate::ui::widgets::btn("知道了", crate::ui::widgets::Kind::Ghost, Some(CalMsg::AckMissed)));
    }
    let note = ro::alert_note();
    if !note.is_empty() {
        head = head.push(meta(note.clone(), if note.starts_with('✗') { crate::ui::pal::bad() } else { crate::ui::pal::ok() }));
    }
    let mut col = column![head.wrap(), crate::ui::mark::here()].spacing(crate::ui::metrics::space(3));

    // ── 规则 ──
    col = col.push(crate::ui::widgets::section(format!("规则（{}）", a.rules.len())));
    if a.rules.is_empty() {
        col = col.push(meta("还没有规则：点上面的预设一键启用，或在下面新建。提醒只在 Cockpit 开着时发；关着时到点的，下次打开记为「错过」，不补弹。", crate::ui::pal::dim()));
    } else {
        let cols = vec![
            Column::text("名称", 160.0),
            Column::text("范围", 70.0),
            Column::text("目标", 200.0),
            Column::text("提前", 200.0),
            Column::text("渠道", 150.0),
            Column::text("改期 / 撤下", 80.0),
            Column::text("公布", 50.0),
            Column::text("", 60.0),
            Column::text("", 60.0),
        ];
        let mut data = Vec::new();
        let mut ids = Vec::new();
        for r in &a.rules {
            let c = if r.enabled { crate::ui::pal::txt() } else { crate::ui::pal::dim() };
            data.push(vec![
                Cell::Colored(r.name.clone(), c),
                Cell::Text(r.scope.label().into()),
                Cell::Text(target_label(snap, r)),
                Cell::Text(r.offsets.iter().map(|m| al::offset_label(*m)).collect::<Vec<_>>().join("、")),
                Cell::Text(r.channels.iter().map(|c| c.label()).collect::<Vec<_>>().join(" · ")),
                Cell::Text(if r.on_change { "通知".into() } else { String::new() }),
                Cell::Text(if r.on_release { "通知".into() } else { String::new() }),
                Cell::Action(if r.enabled { "停用".into() } else { "启用".into() }, Tone::Neutral),
                Cell::Action("删除".into(), Tone::Danger),
            ]);
            ids.push((r.id, r.enabled));
        }
        RULE_ROWS.with(|x| *x.borrow_mut() = ids);
        col = col.push(crate::ui::grid::named("calendar.rules", cols, data, Fit::Rows(8), None, CalMsg::RulesTable));
    }

    // ── 新建规则 ──
    let d = ro::draft();
    col = col.push(crate::ui::widgets::section("新建规则"));
    col = col.push(kv(
        "范围",
        crate::ui::widgets::segmented(
            &[("系列", Scope::Series), ("全部事件", Scope::All), ("类别", Scope::Kind), ("地区", Scope::Country)],
            &d.scope,
            CalMsg::DraftScope,
        ),
    ));
    if matches!(d.scope, Scope::Series | Scope::Kind | Scope::Country) {
        let opts = targets(snap, d.scope);
        let sel = opts.iter().find(|o| o.key == d.target).cloned();
        col = col.push(kv(
            "目标",
            pick_list(opts, sel, |o: TargetOpt| CalMsg::DraftTarget(o.key))
                .placeholder("选一个")
                .text_size(crate::ui::text::s_small())
                .padding(crate::ui::metrics::pad2(0, 2))
                .into(),
        ));
    }
    if matches!(d.scope, Scope::All | Scope::Kind | Scope::Country) {
        col = col.push(kv(
            "重要性",
            crate::ui::widgets::segmented(&[("P0", 0_u8), ("≤P1", 1), ("≤P2", 2), ("全部", 3)], &d.max_importance, CalMsg::DraftImportance),
        ));
    }
    let mut offs = row![].spacing(crate::ui::metrics::space(1));
    for m in al::OFFSET_CHOICES {
        offs = offs.push(chip_on(al::offset_label(m), d.offsets.contains(&m), CalMsg::DraftOffset(m)));
    }
    col = col.push(kv("提前", offs.wrap().into()));
    let tg = al::telegram();
    let mut chs = row![].spacing(crate::ui::metrics::space(1));
    for c in Channel::ALL {
        let label = if c == Channel::Telegram && !tg.configured() { "Telegram（未配置）".to_string() } else { c.label().to_string() };
        chs = chs.push(chip_on(label, d.channels.contains(&c), CalMsg::DraftChannel(c)));
    }
    col = col.push(kv("渠道", chs.wrap().into()));
    col = col.push(kv(
        "另外",
        row![
            chip_on("改期 / 撤下时通知", d.on_change, CalMsg::DraftOnChange(!d.on_change)),
            chip_on("实际值公布时通知（只报数）", d.on_release, CalMsg::DraftOnRelease(!d.on_release)),
        ]
        .spacing(crate::ui::metrics::space(1))
        .wrap()
        .into(),
    ));
    col = col.push(row![
        crate::ui::widgets::btn("添加规则", crate::ui::widgets::Kind::Primary, Some(CalMsg::DraftAdd)),
        meta("没有时刻的事件（仅日期 / 预计）只按「提前 N 天」提醒，在当天早上 09:00；「提前 30 分钟」这类不建——不编时刻", crate::ui::pal::dim()),
    ]
    .spacing(crate::ui::metrics::space(3))
    .align_y(Alignment::Center)
    .wrap());

    // ── 通知中心 ──
    col = col.push(crate::ui::widgets::section(format!("通知中心（最近 {}）", a.notices.len())));
    if a.notices.is_empty() {
        col = col.push(meta("还没有通知", crate::ui::pal::dim()));
    } else {
        let cols = vec![
            Column::text("时间", 100.0),
            Column::text("事件", 280.0),
            Column::text("内容", 300.0),
            Column::text("状态", 140.0).groupable(),
            Column::text("渠道", 320.0),
        ];
        let data: Vec<Vec<Cell>> = a
            .notices
            .iter()
            .map(|n| {
                let (stl, stc) = notice_state(&n.state);
                let t = n.fired_at.unwrap_or(n.fire_at);
                let when = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, t).single().map(|x| x.format("%m-%d %H:%M").to_string()).unwrap_or_default();
                let stl = match &n.note {
                    Some(x) if n.state == "pending" || n.state == "fired" => format!("{stl} · {x}"),
                    _ => stl.to_string(),
                };
                vec![Cell::Text(when), Cell::Text(n.title.clone()), Cell::Text(n.body.clone()), Cell::Colored(stl, stc), Cell::Text(delivery_text(&n.deliveries))]
            })
            .collect();
        col = col.push(crate::ui::grid::named("calendar.notices", cols, data, Fit::Rows(12), None, CalMsg::NoticesTable));
    }

    // ── 免打扰与渠道 ──
    col = col.push(crate::ui::widgets::section("免打扰与渠道"));
    let (qf, qt, qp) = ro::quiet_edit(&a.quiet);
    col = col.push(kv(
        "免打扰",
        row![
            text_input("23:00", &qf).on_input(CalMsg::QuietFrom).size(crate::ui::text::s_small()).padding(crate::ui::metrics::pad2(0, 2)).width(Length::Fixed(64.0)),
            meta("至", crate::ui::pal::dim()),
            text_input("07:00", &qt).on_input(CalMsg::QuietTo).size(crate::ui::text::s_small()).padding(crate::ui::metrics::pad2(0, 2)).width(Length::Fixed(64.0)),
            chip_on("P0 例外", qp, CalMsg::QuietP0(!qp)),
            crate::ui::widgets::btn("保存", crate::ui::widgets::Kind::Standard, Some(CalMsg::QuietSave)),
            meta(
                if a.quiet.on() { format!("现在：{}–{}{}", a.quiet.from, a.quiet.to, if a.quiet.p0_exempt { "，P0 照常提醒" } else { "" }) } else { "现在：关（开始与结束相同 = 关）".into() },
                crate::ui::pal::dim()
            ),
        ]
        .spacing(crate::ui::metrics::space(2))
        .align_y(Alignment::Center)
        .wrap()
        .into(),
    ));
    col = col.push(telegram_section(&tg));
    crate::ui::scroll(col).width(Length::Fill).height(Length::Fill).into()
}


/// Telegram 配置与连通测试（docs/43 §18）：在界面里填令牌和会话、测试，不用再手改文件。
///
/// 界面里没有网络代码：「测试连通」「从最近消息读取」都写请求文件交给 `ws-telegram`，结果从它写回的状态文件读。
/// **令牌只进不出**：输入框是密码框，保存后只显示遮挡写法（`123456:AA…k9`），草稿清空。
fn telegram_section<'a>(tg: &super::calendar_alerts::TelegramConfig) -> Element<'a, CalMsg> {
    use super::calendar_alerts as al;
    let kv = |k: &str, v: Element<'a, CalMsg>| crate::ui::widgets::kv(k.to_string(), v, 72.0);
    let (token, chat, imp) = ro::tg_form(tg);
    let st = al::tg_status();
    let private = al::telegram_file_private();
    let mut col = column![crate::ui::widgets::section("Telegram")].spacing(crate::ui::metrics::space(2));

    // 现状一行
    let (state, c) = if !tg.configured() {
        ("未配置".to_string(), crate::ui::pal::dim())
    } else if tg.enabled {
        (format!("已启用 · 只发 ≤ {}", ro::importance_label(tg.min_importance)), crate::ui::pal::ok())
    } else {
        ("已配置、没启用".to_string(), crate::ui::pal::warn())
    };
    let mut head = row![meta(state, c)].spacing(crate::ui::metrics::space(3)).align_y(Alignment::Center);
    if tg.configured() {
        head = head.push(chip_on(if tg.enabled { "启用中（点一下停用）" } else { "启用 Telegram" }, tg.enabled, CalMsg::TgEnable(!tg.enabled)));
    }
    if private == Some(false) {
        head = head.push(meta("⚠ 配置文件别人也能读（令牌在里面）——点「保存」会改回 600", crate::ui::pal::warn()));
    }
    col = col.push(kv("状态", head.wrap().into()));

    // 令牌（密码框）
    let ph = if tg.bot_token.trim().is_empty() {
        "粘贴 @BotFather 给的令牌（数字:一串字母）".to_string()
    } else {
        format!("已保存 {}——要换就粘贴新的", al::masked_token(&tg.bot_token))
    };
    col = col.push(kv(
        "机器人令牌",
        text_input(&ph, &token)
            .on_input(CalMsg::TgToken)
            .secure(true)
            .size(crate::ui::text::s_small())
            .padding(crate::ui::metrics::pad2(0, 2))
            .width(Length::Fixed(420.0))
            .into(),
    ));

    // 会话 ID + 从最近消息读取
    let mut chat_row = row![
        text_input("数字，如 123456789（群组 / 频道是 -100 开头）", &chat)
            .on_input(CalMsg::TgChat)
            .size(crate::ui::text::s_small())
            .padding(crate::ui::metrics::pad2(0, 2))
            .width(Length::Fixed(260.0)),
        crate::ui::widgets::btn("从最近消息读取", crate::ui::widgets::Kind::Subtle, Some(CalMsg::TgDetect)),
    ]
    .spacing(crate::ui::metrics::space(2))
    .align_y(Alignment::Center);
    if st.action == "detect" && !st.chats.is_empty() {
        for (id, kind, name) in &st.chats {
            let k = match kind.as_str() {
                "private" => "私聊",
                "channel" => "频道",
                "group" | "supergroup" => "群组",
                _ => "",
            };
            chat_row = chat_row.push(chip_on(format!("{name}（{k} {id}）"), *id == chat, CalMsg::TgChat(id.clone())));
        }
    }
    col = col.push(kv("会话 ID", chat_row.wrap().into()));
    col = col.push(kv(
        "只发",
        crate::ui::widgets::segmented(&[("P0", 0_u8), ("≤P1", 1), ("≤P2", 2), ("全部", 3)], &imp, CalMsg::TgImp),
    ));

    // 动作
    col = col.push(
        row![
            crate::ui::widgets::btn("保存", crate::ui::widgets::Kind::Standard, Some(CalMsg::TgSave)),
            crate::ui::widgets::btn("保存并测试连通", crate::ui::widgets::Kind::Primary, Some(CalMsg::TgSaveProbe)),
            crate::ui::widgets::btn("发一条测试消息", crate::ui::widgets::Kind::Standard, (tg.configured() && tg.enabled).then_some(CalMsg::TelegramTest)),
        ]
        .spacing(crate::ui::metrics::space(2))
        .align_y(Alignment::Center)
        .wrap(),
    );

    // 结果：在等的请求还没回来 → 「测试中」；回来了 → 它的结论
    let waiting = ro::tg_wait().filter(|(n, _)| *n != st.nonce);
    let result: Option<(String, Color)> = match waiting {
        Some((_, secs)) if secs >= 20 => Some(("✗ 20 秒没有回应：发送进程 ws-telegram 没起来？在「资源｜进程」里看看".into(), crate::ui::pal::bad())),
        Some(_) => Some(("测试中…（ws-telegram 正在问 api.telegram.org）".into(), crate::ui::pal::info())),
        None if !st.message.is_empty() => Some((
            format!("{} · {}", st.at, st.message),
            if st.ok { crate::ui::pal::ok() } else { crate::ui::pal::warn() },
        )),
        None => None,
    };
    if let Some((t, c)) = result {
        col = col.push(meta(t, c));
    }
    col = col.push(meta(
        "怎么配：① 在 Telegram 找 @BotFather 发 /newbot，复制它给的令牌贴到上面；② 打开你的新机器人、随便发一条消息；\
         ③ 点「从最近消息读取」选你的会话；④「保存并测试连通」（只检查、不发消息）；⑤「启用 Telegram」，再「发一条测试消息」确认能收到。",
        crate::ui::pal::dim(),
    ));
    col = col.push(meta(
        "令牌只存本机 ~/.config/wealthspring/telegram.json（权限 600），界面不回显。发送由独立进程 ws-telegram 负责（不抓新闻），开着时随 Cockpit 起停；\
         同一轮的普通提醒合并成一条、P0 单独发、每秒最多一条；发出去没收到回复记「不确定」，只有 P0 重试一次；事件过了未送出即作废。只出不进：不收消息、不做机器人命令。",
        crate::ui::pal::dim(),
    ));
    col.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: &str, to: &str) -> ro::CalSource {
        ro::CalSource { id: id.into(), label: id.into(), coverage_to: Some(to.into()), rolling: id != "bls-schedule", ..Default::default() }
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
