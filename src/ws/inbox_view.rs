//! 消息中心与提醒管理器的界面（docs/65）。只读快照（`inbox::snapshot` / `alert_rules::view_state`），不查库。

use iced::widget::{button, checkbox, column, container, row, text_input, Space};
use iced::{Alignment, Element, Length};
use wealthspring_inbox as ib;

use super::alert_rules::{RuleKind, RulesMsg};
use super::inbox::{InboxMsg, Range};
use crate::ui::{metrics, pal, text as t, widgets as w};

/// 宽于这个就左列表右详情，窄了点一条在下面展开。
const WIDE: f32 = 860.0;
const DETAIL_W: f32 = 380.0;
const CATS_W: f32 = 96.0;

fn space(i: usize) -> f32 {
    metrics::space(i)
}

fn tone(level: u8) -> w::Tone {
    match level {
        0 => w::Tone::Danger,
        1 => w::Tone::Warning,
        2 => w::Tone::Info,
        _ => w::Tone::Neutral,
    }
}

/// `13:25:07`（今天）/ `10-27 13:25`（更早）/ `2025-12-31 13:25`（往年）；0 = 时间未知。
fn when(ms: i64) -> String {
    use chrono::{Datelike, TimeZone};
    if ms <= 0 {
        return "时间未知".into();
    }
    let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let now = chrono::Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M:%S").to_string()
    } else if t.year() == now.year() {
        t.format("%m-%d %H:%M").to_string()
    } else {
        t.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn full_time(ms: i64) -> String {
    use chrono::TimeZone;
    if ms <= 0 {
        return "未知（源没给时间）".into();
    }
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

fn lag_text(ms: i64) -> String {
    let s = ms / 1000;
    match s {
        s if s < 60 => format!("{s} 秒"),
        s if s < 3600 => format!("{} 分 {} 秒", s / 60, s % 60),
        s if s < 86_400 => format!("{} 小时 {} 分", s / 3600, (s % 3600) / 60),
        s => format!("{} 天", s / 86_400),
    }
}

// ── 顶栏的铃铛 ────────────────────────────────────────────────────────

/// 渠道异常的一句（铃铛旁「发送失败」图标的悬停说明）；没有异常为空。
#[must_use]
pub fn chan_note(b: &ib::Badge) -> String {
    if b.chan_failed > 0 { format!("Telegram 最近 24 小时有 {} 条没发出去", b.chan_failed) } else { String::new() }
}

// ── 消息中心 ──────────────────────────────────────────────────────────

/// 浮窗里的高（浮窗本身按窗口夹边，矮窗口里整张卡会出滚动条）。
const BODY_H: f32 = 600.0;

pub fn body<'a>() -> Element<'a, InboxMsg> {
    // responsive 在浮窗里拿不到有界的高：给定高，宽度照样跟着浮窗
    container(iced::widget::responsive(|size| content(size.width >= WIDE))).height(Length::Fixed(BODY_H)).into()
}

fn content<'a>(wide: bool) -> Element<'a, InboxMsg> {
    let u = super::inbox::ui();
    let s = super::inbox::snapshot();

    // 第一行：搜索、品种、时间段、全部已读、导出、分类开关
    let top = row![
        w::ibtn_tip(
            crate::style::Icon::UiSidebar,
            if u.cats_hidden { "分类" } else { "" },
            w::Kind::Ghost,
            Some(InboxMsg::ToggleCats),
            if u.cats_hidden { "显示左侧分类" } else { "收起左侧分类" }
        ),
        text_input("搜索标题 / 正文 / 来源", &u.text).on_input(InboxMsg::Text).size(t::s_small()).padding(metrics::pad2(1, 2)).width(Length::Fixed(220.0)),
        text_input("品种", &u.symbol).on_input(InboxMsg::Symbol).size(t::s_small()).padding(metrics::pad2(1, 2)).width(Length::Fixed(110.0)),
        w::segmented(&Range::ALL.map(|r| (r.label(), r)), &u.range, InboxMsg::Range),
        Space::new().width(Length::Fill),
        w::ibtn(
            crate::style::Icon::UiVolume,
            if u.sound_page { "回到消息" } else { "声音" },
            if u.sound_page { w::Kind::Primary } else { w::Kind::Standard },
            Some(InboxMsg::SoundPage)
        ),
        w::btn("提醒管理器", w::Kind::Standard, Some(InboxMsg::Rules)),
        w::btn("全部已读", w::Kind::Standard, (s.badge.unread > 0).then_some(InboxMsg::AllRead)),
        w::btn("导出 CSV", w::Kind::Standard, Some(InboxMsg::Export)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    let top: Element<'a, InboxMsg> = if wide { top.into() } else { top.wrap().into() };
    let mut head = column![top].spacing(space(2));

    // 只看一条规则 / 批量操作
    if let Some(r) = u.rule_ref.as_ref() {
        head = head.push(
            row![
                w::badge(format!("只看规则：{r}"), w::Tone::Accent),
                w::btn("✕ 去掉", w::Kind::Ghost, Some(InboxMsg::ClearRule)),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
        );
    }
    if !u.checked.is_empty() {
        let archived = u.cat == ib::Cat::Archived;
        head = head.push(
            row![
                t::label(format!("已勾 {} 条", u.checked.len())),
                w::btn("标已读", w::Kind::Standard, Some(InboxMsg::Read)),
                w::btn("标未读", w::Kind::Standard, Some(InboxMsg::Unread)),
                w::btn("知道了", w::Kind::Standard, Some(InboxMsg::Ack)),
                if archived { w::btn("取消归档", w::Kind::Standard, Some(InboxMsg::Unarchive)) } else { w::btn("归档", w::Kind::Standard, Some(InboxMsg::Archive)) },
                w::btn("清除勾选", w::Kind::Ghost, Some(InboxMsg::CheckNone)),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center)
            .wrap(),
        );
    } else if !s.list.is_empty() && !u.sound_page {
        head = head.push(row![w::btn("勾选本页全部", w::Kind::Ghost, Some(InboxMsg::CheckShown))].spacing(space(2)));
    }
    if !u.note.is_empty() {
        head = head.push(t::caption(u.note.clone()).color(if u.note.starts_with('✗') { pal::bad() } else { pal::dim() }));
    }
    if !s.err.is_empty() {
        head = head.push(t::caption(s.err.clone()).color(pal::bad()));
    }
    if !s.live {
        head = head.push(t::caption("消息库没有在读（样张 / 测试模式）").color(pal::dim()));
    }

    if u.sound_page {
        return column![head, crate::ui::scroll(sound_page()).width(Length::Fill).height(Length::Fill)].spacing(space(3)).height(Length::Fill).into();
    }

    // 左：分类
    let mut cats = column![].spacing(space(1)).width(Length::Fixed(CATS_W));
    for c in ib::Cat::ALL {
        let label = match c {
            ib::Cat::Unread if s.badge.unread > 0 => format!("{} {}", c.label(), s.badge.unread),
            ib::Cat::Urgent if s.badge.urgent > 0 => format!("{} {}", c.label(), s.badge.urgent),
            _ => c.label().to_string(),
        };
        let on = u.cat == c;
        cats = cats.push(
            button(t::label(label).color(if on { pal::txt() } else { pal::dim() }))
                .width(Length::Fill)
                .padding(metrics::pad2(1, 2))
                .on_press(InboxMsg::Cat(c))
                .style(move |th, st| crate::style::button::transparent(th, st, on)),
        );
    }

    // 中：列表
    let detail = s.detail.as_ref().filter(|(m, _)| Some(m.id) == u.sel);
    let mut list = column![].spacing(1);
    if s.list.is_empty() {
        list = list.push(w::empty(
            "没有消息",
            if u.rule_ref.is_some() || !u.text.is_empty() || !u.symbol.is_empty() {
                "换个筛选条件，或把时间段放宽到「全部」"
            } else {
                "指标 / 画线 / 日历提醒、新闻守护收到的新闻、弹出过的提示都会记在这里，关了程序也查得到"
            },
        ));
    }
    for g in &s.groups {
        let head_m = &s.list[g.members[0]];
        let expanded = u.expanded.contains(&head_m.id);
        list = list.push(line(head_m, &u, g.members.len(), g.by_source, expanded));
        if !wide && let Some((m, d)) = detail.filter(|(m, _)| m.id == head_m.id) {
            list = list.push(container(detail_view(m, d)).padding(metrics::pad2(1, 6)));
        }
        if expanded {
            for &i in &g.members[1..] {
                let m = &s.list[i];
                list = list.push(container(line(m, &u, 1, false, false)).padding(metrics::pad2(0, 6)));
                if !wide && let Some((dm, d)) = detail.filter(|(dm, _)| dm.id == m.id) {
                    list = list.push(container(detail_view(dm, d)).padding(metrics::pad2(1, 8)));
                }
            }
        }
    }
    let list = crate::ui::scroll(list.width(Length::Fill)).width(Length::Fill).height(Length::Fill);

    let mut main = row![].spacing(space(3)).height(Length::Fill);
    if !u.cats_hidden {
        main = main.push(cats).push(w::vrule());
    }
    main = main.push(list);
    if wide {
        main = main.push(w::vrule());
        let right: Element<'a, InboxMsg> = match detail {
            Some((m, d)) => crate::ui::scroll(detail_view(m, d)).width(Length::Fixed(DETAIL_W)).height(Length::Fill).into(),
            None => container(w::empty("点左边一条看详情", "详情里有出处、触发时的设置、两个时刻（发生 / 收到）与各渠道的投递情况"))
                .width(Length::Fixed(DETAIL_W))
                .into(),
        };
        main = main.push(right);
    }
    column![head, main].spacing(space(3)).height(Length::Fill).into()
}

// ── 提醒声音（docs/68）────────────────────────────────────────────────

const LEVEL_WHAT: [&str; 4] = [
    "日历最重要的事件等；没点「知道了」就一直响",
    "画线到价、日历次重要事件",
    "指标提醒、关注品种的新闻与关键词告警、错误提示",
    "普通新闻、一般系统消息",
];

fn sound_page<'a>() -> Element<'a, InboxMsg> {
    use super::alert_sound::{self as snd, SoundMsg};
    use iced::widget::{pick_list, slider};
    let c = snd::cfg();
    let st = snd::status();
    let files = snd::files();
    let dir = snd::dir();
    let now = chrono::Utc::now().timestamp_millis();
    let m = InboxMsg::Sound;

    let mut col = column![w::section("声音文件夹")].spacing(space(3)).padding(metrics::pad2(1, 2));
    col = col.push(
        row![
            t::label(dir.display().to_string()),
            w::btn("打开文件夹", w::Kind::Standard, Some(m(SoundMsg::OpenDir))),
            w::btn("⟳ 重新扫描", w::Kind::Standard, Some(m(SoundMsg::Rescan))),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
        .wrap(),
    );
    col = col.push(t::caption(format!(
        "把自己的音乐 / 音效（wav、mp3、flac、ogg、m4a）放进这个文件夹（可以再分一层子文件夹），点「重新扫描」后在下面选。现在有 {} 个。",
        files.len()
    )));

    col = col.push(w::section("各级别的声音"));
    for l in 0..4u8 {
        let lc = &c.levels[usize::from(l)];
        let sel = (!lc.file.is_empty()).then(|| lc.file.clone());
        let vol = lc.volume;
        let line = row![
            container(w::badge(format!("P{l}"), tone(l))).width(Length::Fixed(44.0)),
            checkbox(lc.on).label("响").on_toggle(move |v| InboxMsg::Sound(SoundMsg::On(l, v))).size(t::s_small()),
            pick_list(files.clone(), sel, move |f| InboxMsg::Sound(SoundMsg::File(l, f)))
                .placeholder("选一个声音")
                .text_size(t::s_small())
                .padding(metrics::pad2(0, 2))
                .width(Length::Fixed(260.0)),
            t::caption("音量"),
            slider(0.0..=100.0, f32::from(vol), move |v| InboxMsg::Sound(SoundMsg::Volume(l, v.round() as u8))).width(Length::Fixed(120.0)),
            container(t::caption(vol.to_string())).width(Length::Fixed(28.0)),
            w::ibtn(crate::style::Icon::UiPlay, "试听", w::Kind::Ghost, Some(m(SoundMsg::Test(l)))),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
        .wrap();
        let mut block = column![line, t::caption(LEVEL_WHAT[usize::from(l)]).color(pal::dim())].spacing(space(1));
        if !lc.file.is_empty() && !files.contains(&lc.file) && !files.is_empty() {
            block = block.push(t::caption(format!("找不到「{}」——文件夹里没有了，这一级不会响；重新选一个", lc.file)).color(pal::bad()));
        }
        if l == 0 {
            let reps = [("10 秒", 10u32), ("15 秒", 15), ("30 秒", 30), ("1 分钟", 60)];
            block = block.push(
                row![t::caption("没确认时每隔"), w::segmented(&reps, &c.p0_repeat_secs, move |s| InboxMsg::Sound(SoundMsg::Repeat(s))), t::caption("再响一次")]
                    .spacing(space(2))
                    .align_y(Alignment::Center),
            );
        }
        col = col.push(block);
    }
    col = col.push(
        row![
            t::caption("每一声最长放"),
            w::segmented(&[("3 秒", 3u32), ("5 秒", 5), ("8 秒", 8), ("15 秒", 15), ("30 秒", 30)], &c.max_secs, move |s| InboxMsg::Sound(SoundMsg::MaxSecs(s))),
            t::caption("（选的是整首歌时只放开头这一段）"),
            w::btn("停止试听", w::Kind::Ghost, Some(m(SoundMsg::StopTest))),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
        .wrap(),
    );

    col = col.push(w::section("静音与免打扰"));
    let mt = snd::mute_text(&c, now);
    let mute_row = row![
        t::label(if mt.is_empty() { "声音开着".to_string() } else { mt.clone() }).color(if mt.is_empty() { pal::txt() } else { pal::warn() }),
        if mt.is_empty() {
            w::btn("静音 1 小时", w::Kind::Standard, Some(m(SoundMsg::MuteHour)))
        } else {
            w::btn("恢复声音", w::Kind::Standard, Some(m(SoundMsg::Unmute)))
        },
        checkbox(c.mute_forever).label("一直静音").on_toggle(|v| InboxMsg::Sound(SoundMsg::MuteForever(v))).size(t::s_small()),
    ]
    .spacing(space(3))
    .align_y(Alignment::Center);
    col = col.push(mute_row);
    col = col.push(t::caption("顶栏铃铛旁的喇叭也能点：静音 1 小时 / 恢复。静音时没确认的 P0，静音结束后接着响。"));
    col = col.push(t::caption(if st.quiet.is_empty() {
        "免打扰：没设（与日历提醒共用，在金融日历面板「提醒」里改）".to_string()
    } else {
        format!("免打扰 {}：时段里不响（与日历提醒共用，在金融日历面板「提醒」里改）", st.quiet)
    }));
    col = col.push(t::caption("只对 Cockpit 开着时新进来的消息响；启动前的积压、入库即已读的（历史、免打扰挡掉的）不响。Cockpit 放声音的级别，桌面通知不再自带提示音。").color(pal::dim()));

    if !st.device_err.is_empty() {
        col = col.push(t::caption(st.device_err.clone()).color(pal::bad()));
    }
    if !st.note.is_empty() {
        col = col.push(t::caption(st.note.clone()).color(if st.note.starts_with('✗') { pal::bad() } else { pal::dim() }));
    }
    col.into()
}

/// 列表里的一行。`n`：折起来的一组有几条；`by_source`：是多个来源（而不是同一规则连续触发）。
fn line<'a>(m: &ib::Msg, u: &super::inbox::Ui, n: usize, by_source: bool, expanded: bool) -> Element<'a, InboxMsg> {
    let id = m.id;
    let sel = u.sel == Some(id);
    let unread = m.unread();
    let title_c = if unread { pal::head() } else { pal::dim() };
    let mut r = row![
        checkbox(u.checked.contains(&id)).on_toggle(move |v| InboxMsg::Check(id, v)).size(t::s_small()),
        // 源没给时间的新闻按抓取时刻排，标「≈」
        container(t::code(format!("{}{}", if m.refj["time_guessed"].as_bool() == Some(true) { "≈" } else { "" }, when(m.occurred_at))).color(pal::dim()))
            .width(Length::Fixed(104.0)),
        w::badge(ib::level_label(m.level), tone(m.level)),
        container(t::caption(ib::kind_label(&m.kind)).color(pal::dim())).width(Length::Fixed(56.0)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if !m.symbols.is_empty() {
        r = r.push(t::label(m.symbols.join(" ")).color(pal::accent()));
    }
    let mut title = t::label(format!("{}{}", if m.needs_ack() { "⚠ " } else if unread { "● " } else { "" }, m.title)).color(title_c);
    if unread {
        title = title.font(iced::Font { weight: iced::font::Weight::Bold, ..t::ui_font() });
    }
    // 标题一行、正文摘要一行（整段在详情里）
    let mut text_col = column![title].spacing(0).width(Length::Fill);
    if !m.body.is_empty() {
        let mut b: String = m.body.chars().take(90).collect();
        if m.body.chars().count() > 90 {
            b.push('…');
        }
        text_col = text_col.push(t::caption(b).color(pal::dim()).wrapping(iced::widget::text::Wrapping::None));
    }
    r = r.push(container(text_col).width(Length::Fill).clip(true));
    let row_btn = button(r.width(Length::Fill))
        .width(Length::Fill)
        .padding(metrics::pad2(1, 1))
        .on_press(InboxMsg::Select(id))
        .style(move |th, st| crate::style::button::transparent(th, st, sel));
    let mut out = row![row_btn].spacing(space(1)).align_y(Alignment::Center);
    if n > 1 {
        let label = if by_source { format!("{n} 个来源 {}", if expanded { "▴" } else { "▾" }) } else { format!("×{n} {}", if expanded { "▴" } else { "▾" }) };
        out = out.push(w::btn(label, w::Kind::Ghost, Some(InboxMsg::Expand(id))));
    }
    out = out.push(w::ibtn_tip(
        if m.starred { crate::style::Icon::StarFilled } else { crate::style::Icon::Star },
        "",
        w::Kind::Ghost,
        Some(InboxMsg::Star(id, !m.starred)),
        if m.starred { "去掉星标" } else { "加星标（永不过期）" },
    ));
    out.into()
}

fn ref_key(k: &str) -> &str {
    match k {
        "chart" => "图",
        "symbol" => "品种",
        "tf_ms" => "周期（毫秒）",
        "bar_ts" => "K 线时刻",
        "study" => "指标",
        "out" | "out_name" => "输出",
        "cond" => "条件",
        "level" => "阈值",
        "prev" => "前值",
        "cur" => "现值",
        "price" => "现价",
        "intrabar" => "盘中判",
        "drawing" => "线",
        "kind" => "种类",
        "when" => "怎么算触发",
        "event_id" => "事件",
        "what" => "提醒类型",
        "offset_min" => "提前（分钟）",
        "state" => "状态",
        "note" => "备注",
        "scheduled_at" => "事件时刻",
        "published_at" => "源给的发布时间",
        "effective_at" => "生效时间",
        "time_guessed" => "源没给时间（按抓取时刻）",
        "lang" => "语言",
        "suppressed" => "免打扰",
        "news_key" => "新闻键",
        "tier" => "来源分级",
        "dupes" => "也发了的来源",
        "rule" => "规则",
        "query" => "查询",
        "imported" => "从旧记录导入",
        x => x,
    }
}

fn ref_val(k: &str, v: &serde_json::Value) -> String {
    match (k, v) {
        ("bar_ts" | "scheduled_at" | "published_at" | "effective_at", serde_json::Value::Number(n)) => n.as_i64().map(full_time).unwrap_or_default(),
        (_, serde_json::Value::String(s)) => s.clone(),
        (_, serde_json::Value::Bool(b)) => if *b { "是".into() } else { "否".into() },
        (_, serde_json::Value::Null) => "—".into(),
        (_, x) => x.to_string(),
    }
}

/// 详情：标题、两个时刻、来源、出处、触发时的设置、投递、动作。
fn detail_view<'a>(m: &ib::Msg, deliveries: &[(String, String, Option<String>)]) -> Element<'a, InboxMsg> {
    let kv = |k: &str, v: String| w::kv(k.to_string(), t::caption(v).into(), 72.0);
    let mut col = column![
        row![w::badge(ib::level_label(m.level), tone(m.level)), t::caption(ib::kind_label(&m.kind)).color(pal::dim())]
            .spacing(space(2))
            .align_y(Alignment::Center),
        t::label(m.title.clone()).color(pal::head()),
    ]
    .spacing(space(2));
    if !m.body.is_empty() {
        col = col.push(t::body(m.body.clone()));
    }
    col = col.push(kv("发生", full_time(m.occurred_at)));
    let lag = m.lag_ms();
    col = col.push(kv(
        "收到",
        if lag > 5_000 && m.occurred_at > 0 { format!("{}（晚 {}）", full_time(m.received_at), lag_text(lag)) } else { full_time(m.received_at) },
    ));
    if !m.source.is_empty() {
        col = col.push(kv("来源", m.source.clone()));
    }
    if !m.symbols.is_empty() {
        col = col.push(kv("品种", m.symbols.join(" ")));
    }
    if let Some(o) = m.refj.as_object() {
        let empty = |v: &serde_json::Value| v.is_null() || v.as_array().is_some_and(Vec::is_empty) || v.as_str() == Some("");
        let shown: Vec<_> = o.iter().filter(|(k, v)| *k != "settings" && !empty(v)).collect();
        if !shown.is_empty() {
            col = col.push(w::section("出处"));
            for (k, v) in shown {
                col = col.push(kv(ref_key(k), ref_val(k, v)));
            }
        }
        if let Some(st) = o.get("settings").filter(|v| !v.is_null()) {
            col = col.push(w::section("触发时的设置"));
            let txt: String = st.to_string().chars().take(600).collect();
            col = col.push(t::code(txt).color(pal::dim()));
        }
    }
    if !deliveries.is_empty() {
        col = col.push(w::section("投递"));
        for (ch, state, err) in deliveries {
            let mut s = ib::delivery_text(ch, state);
            if let Some(e) = err.as_ref().filter(|e| !e.is_empty()) {
                s.push_str(&format!("：{e}"));
            }
            col = col.push(t::caption(s).color(if state == "failed" || state == "expired" { pal::bad() } else { pal::dim() }));
        }
    }
    // 动作
    let mut acts = row![].spacing(space(2)).align_y(Alignment::Center);
    let goto = match m.kind.as_str() {
        ib::CALENDAR => m.refj.get("scheduled_at").and_then(serde_json::Value::as_i64).or(Some(m.occurred_at)),
        ib::SYSTEM => None,
        _ => Some(m.occurred_at),
    };
    if let Some(ms) = goto {
        acts = acts.push(w::btn("在图上看", w::Kind::Standard, Some(InboxMsg::Goto(ms))));
    }
    if ib::safe_url(&m.url).is_some() {
        acts = acts.push(w::btn("打开原文", w::Kind::Standard, Some(InboxMsg::OpenUrl(m.url.clone()))));
    }
    if m.needs_ack() {
        acts = acts.push(w::btn("知道了", w::Kind::Primary, Some(InboxMsg::Ack)));
    }
    if !m.rule_ref.is_empty() {
        acts = acts.push(w::btn("这条规则的全部触发", w::Kind::Ghost, Some(InboxMsg::RuleHistory(m.rule_ref.clone()))));
    }
    acts = acts.push(if m.archived_at.is_some() {
        w::btn("取消归档", w::Kind::Ghost, Some(InboxMsg::Unarchive))
    } else {
        w::btn("归档", w::Kind::Ghost, Some(InboxMsg::Archive))
    });
    col = col.push(acts.wrap());
    if m.kind == ib::NEWS || m.kind == ib::NEWS_ALERT {
        col = col.push(t::caption("外部新闻原文只当数据看：不会交给 AI 起草或策略当指令").color(pal::dim()));
    }
    container(col).padding(metrics::pad2(1, 2)).width(Length::Fill).into()
}

// ── 提醒管理器 ────────────────────────────────────────────────────────

pub fn rules_body<'a>() -> Element<'a, RulesMsg> {
    let (kind, rows, loading, note) = super::alert_rules::view_state();
    let stats = super::inbox::snapshot().stats.clone();
    let mut kinds: Vec<(&'static str, Option<RuleKind>)> = vec![("全部", None)];
    kinds.extend(RuleKind::ALL.map(|k| (k.label(), Some(k))));
    let mut head = row![w::segmented(&kinds, &kind, RulesMsg::Kind)].spacing(space(3)).align_y(Alignment::Center);
    if loading {
        head = head.push(t::caption("读取中…").color(pal::dim()));
    }
    if !note.is_empty() {
        head = head.push(t::caption(note).color(pal::dim()));
    }
    let shown: Vec<_> = rows.iter().filter(|r| kind.is_none_or(|k| r.kind == k)).collect();
    let mut list = column![].spacing(space(1));
    if shown.is_empty() && !loading {
        list = list.push(w::empty(
            "没有提醒规则",
            "指标提醒在 K 线图的指标设置里加；画线提醒在线的属性里开；日历规则在「金融日历」的提醒页；新闻关键词在 news.toml",
        ));
    }
    for r in shown {
        let (n, last) = stats.get(&r.rule_ref).copied().unwrap_or((0, 0));
        let mut info = column![
            row![
                w::badge(r.kind.label(), w::Tone::Neutral),
                // 日历规则的级别是「提醒到哪一档重要性」
                w::badge(
                    if r.kind == RuleKind::Calendar { format!("≤{}", ib::level_label(r.level)) } else { ib::level_label(r.level).to_string() },
                    tone(r.level)
                ),
                t::label(r.symbol.clone()).color(pal::accent()),
                t::label(r.name.clone()).color(if r.on { pal::head() } else { pal::dim() }),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center)
            .wrap(),
            t::caption(r.cond.clone()).color(pal::dim()),
            t::caption(format!(
                "{} · 累计触发 {n} 次{}",
                r.channels,
                if last > 0 { format!(" · 最近 {}", when(last)) } else { String::new() }
            ))
            .color(pal::dim()),
        ]
        .spacing(space(1))
        .width(Length::Fill);
        if !r.note.is_empty() {
            info = info.push(t::caption(r.note.clone()).color(pal::warn()));
        }
        let mut acts = row![].spacing(space(2)).align_y(Alignment::Center);
        acts = acts.push(t::caption(if r.on { "● 开着" } else { "○ 暂停" }).color(if r.on { pal::ok() } else { pal::dim() }));
        if let Some(a) = r.act.clone() {
            acts = acts.push(if r.on {
                w::btn("暂停", w::Kind::Standard, Some(RulesMsg::Set(a, false)))
            } else {
                w::btn("恢复", w::Kind::Standard, Some(RulesMsg::Set(a, true)))
            });
        }
        acts = acts.push(w::btn("触发历史", w::Kind::Ghost, (n > 0).then(|| RulesMsg::History(r.rule_ref.clone()))));
        list = list.push(container(row![info, acts].spacing(space(3)).align_y(Alignment::Center)).padding(metrics::pad2(1, 2)));
        list = list.push(container(Space::new().height(Length::Fixed(1.0))).width(Length::Fill).style(|_| container::Style {
            background: Some(iced::Background::Color(pal::line())),
            ..Default::default()
        }));
    }
    column![
        head,
        t::caption("规则还存在原来的地方：这里只汇总、暂停 / 恢复、看触发历史；改条件回原处改。只提醒，不下单。").color(pal::dim()),
        crate::ui::scroll(list.width(Length::Fill)).height(Length::Fill),
    ]
    .spacing(space(2))
    .height(Length::Fill)
    .into()
}
