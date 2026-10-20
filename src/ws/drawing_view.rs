//! 绘图的界面（docs/46 G1、§13）：顶部工具栏的「绘图」组与浮动绘图条、检查器里选中对象的属性与对象树、
//! 图上选中对象旁的浮动小条。状态全在 [`super::drawing`]，这里只渲染、发 [`DrawMsg`]。
//!
//! 入口不在图里常驻（docs/46 §2 第 4 条、docs/42「主区始终减负」）：选工具在工具栏，改属性在检查器，
//! 图上只有选中对象时才浮出一条小操作条。

use iced::widget::{Space, button, checkbox, column, container, row, text_input};
use iced::{Alignment, Element, Length};

use super::drawing::model::{Dash, Drawing, Kind, Source, TF_CATS, price_text, time_text};
use super::drawing::{self as dw, DrawMsg, Field, Param};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind as Btn};

/// 预设颜色（RGBA）：蓝、红、绿、橙、紫、青、灰、白。
pub const COLORS: [[u8; 4]; 8] = [
    [41, 98, 255, 255],
    [242, 54, 69, 255],
    [8, 153, 129, 255],
    [255, 152, 0, 255],
    [156, 39, 176, 255],
    [0, 188, 212, 255],
    [120, 123, 134, 255],
    [255, 255, 255, 255],
];
/// 线宽档位。
pub const WIDTHS: [f32; 4] = [1.0, 1.5, 2.0, 3.0];
/// 工具分类（菜单里一组一行）。
pub const GROUPS: [(&str, &[Kind]); 4] = [
    ("线条", &[Kind::HLine, Kind::VLine, Kind::Trend, Kind::Ray, Kind::Polyline]),
    ("通道", &[Kind::Channel, Kind::FibRetrace, Kind::FibExt]),
    ("标注", &[Kind::Rect, Kind::Text, Kind::Callout]),
    ("测量", &[Kind::PriceRange, Kind::DateRange, Kind::LongPos, Kind::ShortPos, Kind::VolProfile]),
];

/// 工具的快捷键（菜单里写在名字后面）。
#[must_use]
pub fn shortcut(k: Kind) -> &'static str {
    match k {
        Kind::HLine => "Alt+H",
        Kind::VLine => "Alt+V",
        Kind::Trend => "Alt+T",
        Kind::Ray => "Alt+R",
        Kind::Rect => "Alt+Shift+R",
        Kind::FibRetrace => "Alt+F",
        k => super::drawing::tools::find(k).map_or("", |t| t.shortcut()),
    }
}

/// 菜单分组：内置的四组，登记的工具（G5）按它声明的组并进去（新组排在后面）。
#[must_use]
pub fn menu_groups() -> Vec<(&'static str, Vec<Kind>)> {
    let mut out: Vec<(&'static str, Vec<Kind>)> = GROUPS.iter().map(|(g, k)| (*g, k.to_vec())).collect();
    for t in super::drawing::tools::all() {
        match out.iter_mut().find(|(g, _)| *g == t.group()) {
            Some((_, ks)) => ks.push(t.kind()),
            None => out.push((t.group(), vec![t.kind()])),
        }
    }
    out
}

fn dash_label(d: Dash) -> &'static str {
    match d {
        Dash::Solid => "实线",
        Dash::Dashed => "虚线",
        Dash::Dotted => "点线",
    }
}

fn source_label(s: Source) -> &'static str {
    s.label()
}

/// 一个颜色块按钮。
fn swatch<'a>(c: [u8; 4], on: bool, msg: Option<DrawMsg>) -> Element<'a, DrawMsg> {
    let fill = crate::ui::chart::drawing_color(c);
    let edge = if on { pal::accent() } else { pal::line() };
    let b = button(Space::new().width(Length::Fixed(14.0)).height(Length::Fixed(14.0)))
        .padding(iced::Padding::ZERO)
        .style(move |_, _| button::Style {
            background: Some(iced::Background::Color(fill)),
            border: iced::Border { color: edge, width: if on { 2.0 } else { 1.0 }, radius: 3.0.into() },
            ..button::Style::default()
        });
    match msg {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// 对象在列表里的一行名字：种类 + 主要价位 / 时刻。
#[must_use]
pub fn object_label(d: &Drawing, decimals: usize) -> String {
    let p = |i: usize| d.anchors.get(i).map(|a| price_text(a.p, decimals)).unwrap_or_default();
    let tm = |i: usize| d.anchors.get(i).map(|a| time_text(a.t).get(5..16).unwrap_or_default().to_string()).unwrap_or_default();
    let what = match d.kind {
        Kind::HLine => p(0),
        Kind::VLine => tm(0),
        Kind::Trend | Kind::Ray => format!("{} → {}", p(0), p(1)),
        Kind::Rect | Kind::Channel | Kind::FibRetrace | Kind::PriceRange => format!("{} – {}", p(0), p(1)),
        Kind::FibExt => format!("{} → {} → {}", p(0), p(1), p(2)),
        Kind::Polyline => format!("{} 个点", d.anchors.len()),
        Kind::Text | Kind::Callout => d.text.clone().unwrap_or_default().chars().take(16).collect(),
        Kind::DateRange => format!("{} – {}", tm(0), tm(1)),
        Kind::LongPos | Kind::ShortPos => format!("入 {} · 目标 {} · 止损 {}", p(0), p(1), p(2)),
        Kind::Marker => format!("{} {}", tm(0), d.text.clone().unwrap_or_else(|| p(0))),
        Kind::VolProfile => format!("{} – {}", tm(0), tm(1)),
        k => super::drawing::tools::find(k).map(|t| t.summary(d, decimals)).unwrap_or_default(),
    };
    let mut s = format!("{} · {what}", d.kind.label());
    if d.source != Source::Manual {
        s = format!("{}（{}）", s, source_label(d.source));
    }
    s
}

// ── 工具栏 ───────────────────────────────────────────────────────────────────

/// 工具栏「绘图」组：当前工具（点一下显示 / 收起浮动绘图条）+ 收藏的工具图标（再点一下回到光标）。
pub fn toolbar<'a>() -> Element<'a, DrawMsg> {
    let tool = dw::tool();
    let p = dw::prefs();
    let mut value = tool.map_or_else(|| "光标".to_string(), |k| k.label().to_string());
    if p.continuous && tool.is_some() {
        value.push_str(" · 连续");
    }
    if dw::menu_open() {
        value.push_str(" ▴");
    }
    let mut r = row![crate::ui::shell::tool_pick("绘图", value, Some(DrawMsg::ToggleMenu), tool.is_some() || dw::menu_open())]
        .spacing(space(1))
        .align_y(Alignment::Center);
    for k in p.favorites {
        let on = tool == Some(k);
        r = r.push(w::icon_tool_inline(glyph(k), tip_of(k), on, Some(DrawMsg::Tool((!on).then_some(k))), iced::widget::tooltip::Position::Bottom));
    }
    if p.hide_all {
        r = r.push(w::btn("画线已隐藏", Btn::Subtle, Some(DrawMsg::HideAll(false))));
    }
    r.into()
}

/// 工具栏上的「分析：<当前分析> ▾」（docs/52）。
pub fn analysis_toolbar<'a>(symbol: &str) -> Element<'a, DrawMsg> {
    let (cur, overlay) = dw::ana_summary(symbol);
    let open = dw::ana_menu().as_deref() == Some(symbol);
    let mut v = cur;
    if overlay > 0 {
        v.push_str(&format!(" · 叠 {overlay}"));
    }
    v.push_str(if open { " ▴" } else { " ▾" });
    crate::ui::shell::tool_pick("分析", v, Some(DrawMsg::AnaMenu(symbol.to_string())), open || overlay > 0)
}

/// 「分析 ▾」卡片（盖在内容上方）：这个品种的每份分析一行——设为当前、叠加显示、改名、对象数、修改时间、删除；
/// 下面新建 / 另存为。
pub fn analysis_menu<'a>(symbol: &str, note: Option<(&str, &'a iced::widget::text_editor::Content)>) -> Element<'a, DrawMsg> {
    let sym = symbol.to_string();
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_string();
    let mut col = column![
        row![
            t::label(format!("分析 · {code}")),
            t::metadata("● 当前：新画的线归它、能改；☑ 叠加：一起显示（淡色、只读）。画线随改随存；回放时自动记下数据段、播放头、筛选、条件与视野，设为当前就回到那里").color(pal::dim()),
            Space::new().width(Length::Fill),
            w::btn("收起", Btn::Ghost, Some(DrawMsg::AnaClose)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
    ]
    .spacing(space(1));
    for r in dw::ana_rows(symbol) {
        let id = r.id.clone();
        let name: Element<'a, DrawMsg> = match &id {
            None => t::label(r.name.clone()).into(),
            Some(aid) => {
                let key = dw::ana_name_key(aid);
                let (k, s2, a2) = (key.clone(), sym.clone(), aid.clone());
                text_input("名字", &dw::edit_text(&key, r.name.clone()))
                    .size(t::s_small())
                    .width(Length::Fixed(260.0))
                    .on_input(move |v| DrawMsg::Edit(k.clone(), v))
                    .on_submit(DrawMsg::AnaRename(s2, a2))
                    .into()
            }
        };
        let when = if r.updated_ms > 0 {
            chrono::DateTime::from_timestamp_millis(r.updated_ms)
                .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut line = row![
            w::btn(if r.active { "● 当前" } else { "○ 设为当前" }, if r.active { Btn::Standard } else { Btn::Ghost }, (!r.active).then(|| DrawMsg::AnaOpen(sym.clone(), id.clone()))),
            checkbox(r.shown || r.active)
                .label("叠加")
                .size(t::s_small())
                .on_toggle_maybe((!r.active).then(|| {
                    let (s2, i2) = (sym.clone(), id.clone());
                    move |v| DrawMsg::AnaShow(s2.clone(), i2.clone(), v)
                })),
            name,
            t::metadata(format!("{} 个对象", r.objects)).color(pal::dim()),
            t::metadata(when).color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center);
        // 记下的数据段与播放头（docs/52 A2）；当前那份给「记下现在的状态」
        match super::analysis_state::describe(r.state.as_ref()) {
            Some((d, here)) => {
                line = line.push(t::metadata(if here || !r.active { d } else { format!("{d}（现在看的不是这一段：不自动记）") }).color(if here { pal::ok() } else { pal::dim() }));
            }
            None if id.is_some() => line = line.push(t::metadata("还没记下数据段（回放这个品种时自动记）").color(pal::dim())),
            None => {}
        }
        if r.active && id.is_some() {
            line = line.push(w::btn("记下现在的状态", Btn::Ghost, Some(DrawMsg::AnaRecord(sym.clone()))));
            line = line.push(w::btn("更新缩略图", Btn::Ghost, Some(DrawMsg::AnaThumb(sym.clone()))));
        }
        if !r.active && r.objects > 0 {
            line = line.push(w::btn("复制画线到当前", Btn::Ghost, Some(DrawMsg::AnaCopyFrom(sym.clone(), id.clone()))));
        }
        if let Some(aid) = &id {
            line = line.push(w::btn("导出", Btn::Ghost, Some(DrawMsg::AnaExport(sym.clone(), aid.clone()))));
        }
        if let Some(aid) = &id {
            line = line.push(w::btn(
                if r.confirm_delete { "确认删除（连同画线）" } else { "删除" },
                if r.confirm_delete { Btn::Destructive } else { Btn::Ghost },
                Some(DrawMsg::AnaDelete(sym.clone(), aid.clone())),
            ));
        }
        col = col.push(line);
        // 当前分析：结论笔记（多行，随改随存）与缩略图（docs/52 A3）
        if r.active
            && let (Some(aid), Some((nid, content))) = (&id, note)
            && aid == nid
        {
            let mut detail = row![].spacing(space(3));
            if let Some(h) = thumb_handle(aid, r.updated_ms, dw::analysis(symbol, aid).and_then(|a| a.extra.get("thumb").cloned()).as_ref()) {
                detail = detail.push(iced::widget::image(h).width(Length::Fixed(dw_thumb_w())));
            }
            detail = detail.push(
                column![
                    t::metadata("结论笔记").color(pal::dim()),
                    iced::widget::text_editor(content)
                        .placeholder("这份分析看到了什么、结论、下一步……")
                        .on_action(DrawMsg::AnaNoteEdit)
                        .size(t::s_small())
                        .height(Length::Fixed(dw_note_h())),
                ]
                .spacing(space(1))
                .width(Length::Fill),
            );
            col = col.push(detail);
        }
        if r.active {
            col = col.push(versions(symbol));
        }
    }
    let key = dw::ANA_NEW_KEY.to_string();
    col = col.push(
        row![
            text_input("新分析的名字（例如 BTC 6/1 早盘吸收）", &dw::edit_text(&key, String::new()))
                .size(t::s_small())
                .width(Length::Fixed(320.0))
                .on_input(move |v| DrawMsg::Edit(key.clone(), v))
                .on_submit(DrawMsg::AnaNew(sym.clone())),
            w::btn("＋ 新建（空白）", Btn::Standard, Some(DrawMsg::AnaNew(sym.clone()))),
            w::btn("另存为（复制当前的画线）", Btn::Ghost, Some(DrawMsg::AnaSaveAs(sym.clone()))),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    let n_all = dw::all_analyses().len();
    let open = dw::ana_list_open();
    col = col.push(
        row![
            w::btn(format!("全部分析（{n_all}）{}", if open { " ▴" } else { " ▾" }), if open { Btn::Standard } else { Btn::Ghost }, Some(DrawMsg::AnaList)),
            t::metadata(format!("导出目录：{}", dw::export_dir().display())).color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    if open {
        col = col.push(all_list());
    }
    let note_text = dw::note();
    if !note_text.is_empty() {
        col = col.push(t::caption(note_text).color(pal::warn()));
    }
    container(col).padding(space(3)).into()
}

/// 当前分析的版本（docs/52 A4）：存一个版本（填说明）；历史（新的在前）每个可回退 / 删除（都点两下确认）。
fn versions<'a>(symbol: &str) -> Element<'a, DrawMsg> {
    let sym = symbol.to_string();
    let key = dw::ANA_VER_KEY.to_string();
    let rows = dw::ver_rows(symbol);
    let mut col = column![
        row![
            t::metadata("版本").color(pal::dim()),
            text_input("这个版本的说明（例如 收盘前定稿）", &dw::edit_text(&key, String::new()))
                .size(t::s_small())
                .width(Length::Fixed(280.0))
                .on_input(move |v| DrawMsg::Edit(key.clone(), v))
                .on_submit(DrawMsg::AnaVerSave(sym.clone())),
            w::btn("存一个版本", Btn::Standard, Some(DrawMsg::AnaVerSave(sym.clone()))),
            t::metadata(if rows.is_empty() { "还没存过版本：存的是这份分析现在的全部画线与结论".to_string() } else { format!("{} 个版本；回退前与现在不同会先自动存一个", rows.len()) }).color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
        .wrap()
    ]
    .spacing(space(1));
    const SHOW: usize = 12;
    for v in rows.iter().take(SHOW) {
        let when = chrono::DateTime::from_timestamp_millis(v.created_ms)
            .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let mut line = row![
            t::metadata(when).color(pal::dim()),
            t::label(v.label.clone()),
            t::metadata(format!("{} 个对象", v.objects)).color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center);
        if v.same_as_now {
            line = line.push(t::metadata("= 现在").color(pal::ok()));
        }
        line = line
            .push(w::btn(
                if v.confirm_revert { "确认回退" } else { "回退到这个版本" },
                if v.confirm_revert { Btn::Destructive } else { Btn::Ghost },
                (!v.same_as_now).then(|| DrawMsg::AnaVerRevert(sym.clone(), v.id.clone())),
            ))
            .push(w::btn(
                if v.confirm_delete { "确认删除" } else { "删除" },
                if v.confirm_delete { Btn::Destructive } else { Btn::Ghost },
                Some(DrawMsg::AnaVerDelete(sym.clone(), v.id.clone())),
            ));
        col = col.push(line);
    }
    if rows.len() > SHOW {
        col = col.push(t::metadata(format!("还有 {} 个更早的版本", rows.len() - SHOW)).color(pal::dim()));
    }
    container(col).padding(iced::Padding::ZERO.left(space(4))).into()
}

/// 缩略图显示宽、笔记框高（逻辑像素）。
fn dw_thumb_w() -> f32 {
    space(3) * 30.0
}

fn dw_note_h() -> f32 {
    space(3) * 12.0
}

/// 缩略图的显示句柄（解一次 PNG 就缓存，分析改了才重解）。
fn thumb_handle(id: &str, ver: i64, thumb: Option<&serde_json::Value>) -> Option<iced::widget::image::Handle> {
    static CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, (i64, iced::widget::image::Handle)>>> = std::sync::Mutex::new(None);
    let thumb = thumb?;
    let mut g = CACHE.lock().ok()?;
    let m = g.get_or_insert_with(Default::default);
    if let Some((v, h)) = m.get(id)
        && *v == ver
    {
        return Some(h.clone());
    }
    let (w, h, px) = super::analysis_state::decode_thumb(thumb)?;
    let handle = iced::widget::image::Handle::from_rgba(w, h, px);
    m.insert(id.to_string(), (ver, handle.clone()));
    Some(handle)
}

/// 「全部分析」：全部品种的分析（新改的在前），搜索名字 / 结论 / 品种 / 日期；每份一张卡片（缩略图、名字、品种、数据段、
/// 修改时间、结论第一行；打开、导出）。下面是导入：导出目录里的文件，或输入路径。
fn all_list<'a>() -> Element<'a, DrawMsg> {
    let key = dw::ANA_SEARCH_KEY.to_string();
    let q = dw::edit_text(&key, String::new());
    let ql = q.to_lowercase();
    let mut col = column![
        text_input("搜索名字、结论、品种、日期…", &q).size(t::s_small()).width(Length::Fixed(dw_thumb_w() * 1.5)).on_input(move |v| DrawMsg::Edit(key.clone(), v))
    ]
    .spacing(space(2));
    let mut cards = row![].spacing(space(3));
    let mut shown = 0;
    for a in dw::all_analyses() {
        let seg = super::analysis_state::describe(a.extra.get("state"));
        let hay = format!("{} {} {} {}", a.name, a.note, a.symbol, seg.as_ref().map(|s| s.0.as_str()).unwrap_or_default()).to_lowercase();
        if !ql.is_empty() && !hay.contains(&ql) {
            continue;
        }
        shown += 1;
        let code = a.symbol.rsplit(':').next().unwrap_or(&a.symbol).to_string();
        let when = chrono::DateTime::from_timestamp_millis(a.updated_ms)
            .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let mut card = column![].spacing(space(1)).width(Length::Fixed(dw_thumb_w()));
        match thumb_handle(&a.id, a.updated_ms, a.extra.get("thumb")) {
            Some(h) => card = card.push(iced::widget::image(h).width(Length::Fixed(dw_thumb_w()))),
            None => card = card.push(container(t::metadata("（没有缩略图）").color(pal::dim())).height(Length::Fixed(dw_thumb_w() * 0.5)).center_y(Length::Fixed(dw_thumb_w() * 0.5))),
        }
        card = card
            .push(t::label(a.name.clone()))
            .push(t::metadata(format!("{code} · {when}")).color(pal::dim()))
            .push(t::metadata(seg.map_or_else(|| "没有记下数据段".to_string(), |s| s.0)).color(pal::dim()));
        if let Some(first) = a.note.lines().find(|l| !l.trim().is_empty()) {
            card = card.push(t::caption(first.to_string()));
        }
        card = card.push(
            row![
                w::btn("打开", Btn::Standard, Some(DrawMsg::AnaOpenFromList(a.symbol.clone(), a.id.clone()))),
                w::btn("导出", Btn::Ghost, Some(DrawMsg::AnaExport(a.symbol.clone(), a.id.clone()))),
            ]
            .spacing(space(1)),
        );
        cards = cards.push(container(card).padding(space(2)).style(|_| iced::widget::container::Style { border: crate::ui::metrics::hairline(crate::ui::metrics::radius::SM), ..Default::default() }));
    }
    if shown == 0 {
        col = col.push(t::metadata(if q.is_empty() { "还没有分析" } else { "没有匹配的" }).color(pal::dim()));
    }
    col = col.push(cards.wrap());
    // 导入
    let ikey = dw::ANA_IMPORT_KEY.to_string();
    col = col.push(
        row![
            t::label("导入"),
            text_input("导出文件的路径（.json）", &dw::edit_text(&ikey, String::new()))
                .size(t::s_small())
                .width(Length::Fixed(dw_thumb_w() * 2.0))
                .on_input(move |v| DrawMsg::Edit(ikey.clone(), v))
                .on_submit(DrawMsg::AnaImport(String::new())),
            w::btn("导入", Btn::Ghost, Some(DrawMsg::AnaImport(String::new()))),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    for f in dw::export_files().into_iter().take(8) {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        col = col.push(
            row![t::metadata(name).color(pal::dim()), w::btn("导入这个", Btn::Ghost, Some(DrawMsg::AnaImport(f.to_string_lossy().into_owned())))]
                .spacing(space(2))
                .align_y(Alignment::Center),
        );
    }
    col.into()
}

/// 工具的图标（§13）：内置的在这里配，登记的工具用它自己声明的（缺省铅笔）。
#[must_use]
pub fn glyph(k: Kind) -> char {
    use crate::style::Icon as I;
    char::from(match k {
        Kind::HLine => I::DrHLine,
        Kind::VLine => I::DrVLine,
        Kind::Trend => I::DrTrend,
        Kind::Ray => I::DrRay,
        Kind::Polyline => I::DrPolyline,
        Kind::Channel => I::DrChannel,
        Kind::FibRetrace => I::DrFibRetrace,
        Kind::FibExt => I::DrFibExt,
        Kind::Rect => I::DrRect,
        Kind::Text => I::DrText,
        Kind::Callout => I::DrCallout,
        Kind::PriceRange => I::DrPriceRange,
        Kind::DateRange => I::DrDateRange,
        Kind::LongPos => I::DrLong,
        Kind::ShortPos => I::DrShort,
        Kind::VolProfile => I::DrVolProfile,
        Kind::Marker => I::DrMarker,
        k => return super::drawing::tools::find(k).map_or(char::from(I::DrPencil), |t| t.icon()),
    })
}

fn tip_of(k: Kind) -> String {
    if shortcut(k).is_empty() { k.label().to_string() } else { format!("{}  {}", k.label(), shortcut(k)) }
}

/// 浮动条上的一格：按钮（主轴长 = 一格）、分隔线、拖动柄。
enum Cell<'a> {
    Item(Element<'a, DrawMsg>),
    Sep,
    Grip,
}

impl Cell<'_> {
    fn len(&self, spacing: f32) -> f32 {
        match self {
            Cell::Item(_) => w::TOOL_CELL + spacing,
            Cell::Sep => 1.0 + spacing,
            Cell::Grip => 12.0 + spacing,
        }
    }
}

/// 浮动绘图条（docs/46 §13）：点顶部「绘图」后浮在内容上方，每个工具一个图标，悬停看名字与快捷键。
/// 按住顶上的拖动柄拖到哪儿就停在哪儿（位置存偏好），双击拖动柄回到左上角。
/// 内容区放不下一排时自动折成两排（三排…）。返回铺满内容区的一层，只有条和「⋯」面板本身接鼠标。
pub fn overlay<'a>(area: iced::Size) -> Element<'a, DrawMsg> {
    use iced::widget::tooltip::Position;
    let tool = dw::tool();
    let p = dw::prefs();
    let vertical = !p.bar_horizontal;
    let at = if vertical { Position::Right } else { Position::Bottom };
    let item = |g: char, tip: String, on: bool, m: DrawMsg| Cell::Item(w::icon_tool(g, tip, on, Some(m), at));
    let mut cells = vec![Cell::Grip, item(char::from(crate::style::Icon::DrCursor), "光标  Esc".into(), tool.is_none(), DrawMsg::Tool(None)), Cell::Sep];
    for (_, kinds) in menu_groups() {
        for k in kinds {
            let on = tool == Some(k);
            cells.push(item(glyph(k), tip_of(k), on, DrawMsg::Tool((!on).then_some(k))));
        }
        cells.push(Cell::Sep);
    }
    use crate::style::Icon as I;
    cells.push(item(char::from(I::DrMagnet), format!("磁吸：吸到开 / 高 / 低 / 收（{}）", if p.magnet { "开" } else { "关" }), p.magnet, DrawMsg::Magnet(!p.magnet)));
    cells.push(item(
        char::from(I::DrContinuous),
        format!("连续绘制：画完一个不回到光标（{}）", if p.continuous { "开" } else { "关" }),
        p.continuous,
        DrawMsg::Continuous(!p.continuous),
    ));
    cells.push(item(
        char::from(if p.lock_all { I::Locked } else { I::Unlocked }),
        if p.lock_all { "全部锁定中：点一下解锁".into() } else { "全部锁定（只能选中看属性，不能拖、不能删）".into() },
        p.lock_all,
        DrawMsg::LockAll(!p.lock_all),
    ));
    cells.push(item(
        char::from(if p.hide_all { I::DrEyeOff } else { I::DrEye }),
        if p.hide_all { "画线已全部隐藏：点一下显示".into() } else { "全部隐藏（只是不画，对象不动）".into() },
        p.hide_all,
        DrawMsg::HideAll(!p.hide_all),
    ));
    cells.push(Cell::Sep);
    let more = dw::more_open();
    cells.push(item(char::from(I::DrMore), "更多：收藏、系统对象、横排 / 竖排、说明".into(), more, DrawMsg::ToggleMore));
    cells.push(item(char::from(I::Close), "收起绘图条（顶部「绘图」再打开）".into(), false, DrawMsg::CloseMenu));

    // 折行：主轴放不下就另起一排
    let gap = space(1);
    let pad = space(1);
    let main_room = (if vertical { area.height } else { area.width } - 2.0 * pad - 8.0).max(w::TOOL_CELL * 4.0);
    let mut lines: Vec<Vec<Cell<'a>>> = vec![Vec::new()];
    let mut used = 0.0;
    for c in cells {
        let l = c.len(gap);
        if used + l > main_room && !lines.last().is_none_or(Vec::is_empty) {
            lines.push(Vec::new());
            used = 0.0;
        }
        used += l;
        lines.last_mut().expect("至少一排").push(c);
    }
    let main_len = lines.iter().map(|ln| ln.iter().map(|c| c.len(gap)).sum::<f32>()).fold(0.0, f32::max);
    let n = lines.len() as f32;
    let cross_len = n * w::TOOL_CELL + (n - 1.0) * gap;
    let (bw, bh) = if vertical { (cross_len + 2.0 * pad + 2.0, main_len + 2.0 * pad + 2.0) } else { (main_len + 2.0 * pad + 2.0, cross_len + 2.0 * pad + 2.0) };
    let line_el = |ln: Vec<Cell<'a>>| -> Element<'a, DrawMsg> {
        let els = ln.into_iter().map(|c| match c {
            Cell::Item(e) => e,
            Cell::Sep => w::tool_sep(vertical),
            Cell::Grip => w::tool_grip(vertical, DrawMsg::BarDragStart),
        });
        if vertical {
            iced::widget::Column::with_children(els).spacing(gap).align_x(Alignment::Center).into()
        } else {
            iced::widget::Row::with_children(els).spacing(gap).align_y(Alignment::Center).into()
        }
    };
    let lines: Vec<Element<'a, DrawMsg>> = lines.into_iter().map(line_el).collect();
    let body: Element<'a, DrawMsg> = if vertical {
        iced::widget::Row::with_children(lines).spacing(gap).into()
    } else {
        iced::widget::Column::with_children(lines).spacing(gap).into()
    };
    let bar = iced::widget::opaque(container(body).padding(pad).style(w::float_card));

    // 位置：放不下就往回收，始终整条可见
    let [x0, y0] = dw::bar_at();
    let x = x0.min(area.width - bw).max(0.0);
    let y = y0.min(area.height - bh).max(0.0);
    let place = |e: Element<'a, DrawMsg>, x: f32, y: f32| -> Element<'a, DrawMsg> {
        container(e).padding(iced::Padding { top: y, left: x, right: 0.0, bottom: 0.0 }).width(Length::Fill).height(Length::Fill).into()
    };
    let mut layer = iced::widget::Stack::new().width(Length::Fill).height(Length::Fill).push(place(bar.into(), x, y));
    if more {
        // 「⋯」面板：竖排时贴在条右边（右边放不下就放左边），横排时贴在条下面
        let pw = MORE_WIDTH.min(area.width);
        let (mx, my) = if vertical {
            let right = x + bw + gap;
            (if right + pw <= area.width { right } else { (x - gap - pw).max(0.0) }, y)
        } else {
            (x.min(area.width - pw).max(0.0), y + bh + gap)
        };
        let room = (area.height - my - pad).max(w::TOOL_CELL * 4.0);
        let card = container(crate::ui::scroll(more_panel()))
            .padding(space(3))
            .width(Length::Fixed(pw))
            .max_height(room)
            .style(w::float_card);
        layer = layer.push(place(iced::widget::opaque(card).into(), mx, my));
    }
    layer.into()
}

const MORE_WIDTH: f32 = 380.0;

/// 浮动条「⋯」面板：收藏（顶部工具栏上直接点的那几个）、横排 / 竖排、系统对象来源、操作说明。
fn more_panel<'a>() -> Element<'a, DrawMsg> {
    let p = dw::prefs();
    let mut col = column![
        row![
            t::section("绘图"),
            Space::new().width(Length::Fill),
            w::btn(if p.bar_horizontal { "改竖排" } else { "改横排" }, Btn::Ghost, Some(DrawMsg::BarHorizontal(!p.bar_horizontal))),
            w::btn("关闭", Btn::Ghost, Some(DrawMsg::ToggleMore)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
        t::caption("点图标收藏到顶部工具栏（高亮 = 已收藏）").color(pal::dim()),
    ]
    .spacing(space(2));
    for (group, kinds) in menu_groups() {
        let mut r = row![t::metadata(group).color(pal::dim()).width(Length::Fixed(40.0))].spacing(space(1)).align_y(Alignment::Center);
        for k in kinds {
            let fav = p.favorites.contains(&k);
            r = r.push(w::icon_tool(
                glyph(k),
                format!("{}：{}", k.label(), if fav { "已收藏，点一下取消" } else { "点一下收藏到顶部工具栏" }),
                fav,
                Some(DrawMsg::Favorite(k)),
                iced::widget::tooltip::Position::Top,
            ));
        }
        col = col.push(r.wrap());
    }
    // 系统对象（G4）：按来源显示 / 隐藏
    let mut srcs = row![t::caption("显示系统对象：")].spacing(space(3)).align_y(Alignment::Center);
    for src in Source::SYSTEM {
        let on = !p.hide_sources.contains(&src);
        srcs = srcs.push(
            row![checkbox(on).on_toggle(move |v| DrawMsg::ShowSource(src, v)).size(t::s_small()), t::caption(src.label())].spacing(space(1)).align_y(Alignment::Center),
        );
    }
    col = col.push(srcs.wrap());
    col = col.push(
        t::metadata(format!(
            "订单 = 成交 ▲▼、持仓线、挂单；日历 = 重要事件竖线；策略 = 策略画布这次运行的成交；回测 / 订单流 / 研究 = 脚本写进 {} 的对象。系统对象只读，可以复制成手工对象",
            super::drawing::feeds::external_dir().display()
        ))
        .color(pal::dim()),
    );
    col = col.push(
        t::metadata("光标在图上时：点线选中、拖动；Delete 删除；Ctrl+Z 撤销、Ctrl+Shift+Z 重做；Esc 退出工具；折线回车结束。改颜色、精确价格与时间、文字、斐波那契比例、仓位数量在检查器「属性」页。仓位工具的盈亏只是估算，不会下单。拖动柄按住拖动绘图条，双击回到左上角").color(pal::dim()),
    );
    let note = dw::note();
    if !note.is_empty() {
        col = col.push(t::caption(note).color(pal::warn()));
    }
    let ext = super::drawing::feeds::external_note();
    if !ext.is_empty() {
        col = col.push(t::caption(ext).color(pal::warn()));
    }
    col.into()
}

// ── 检查器 ───────────────────────────────────────────────────────────────────

fn kv<'a>(k: &str, v: Element<'a, DrawMsg>) -> Element<'a, DrawMsg> {
    row![t::metadata(k.to_string()).color(pal::dim()).width(Length::Fixed(56.0)), v].spacing(space(2)).align_y(Alignment::Center).into()
}

/// 检查器「属性」页的「画线」一节：选中对象的属性 + 这个品种的对象树。`decimals`：价格的小数位。
pub fn inspector<'a>(symbol: &str, decimals: usize, multiplier: f64) -> Element<'a, DrawMsg> {
    let sym = symbol.to_string();
    let mut col = column![w::section("画线")].spacing(space(2));
    let note = dw::note();
    if !note.is_empty() {
        col = col.push(t::caption(note).color(pal::warn()));
    }
    // 最近响过的到价提醒（只列这个品种的）
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_string();
    for (title, body) in dw::fired_log().into_iter().rev().filter(|(t, _)| t.ends_with(&code)).take(3) {
        col = col.push(t::caption(format!("◆ {title}：{body}")).color(pal::warn()));
    }
    match dw::selected_drawing(symbol) {
        Some(d) => {
            // 叠加显示的别的分析的线：只读，写明是哪份、给个「设为当前」（docs/52）
            let (ana, active) = dw::ana_of(symbol, &d);
            if !active {
                col = col.push(
                    row![
                        t::caption(format!("属于分析「{ana}」（叠加显示，只读）")).color(pal::warn()),
                        w::btn("设为当前", Btn::Ghost, Some(DrawMsg::AnaOpen(sym.clone(), d.sheet.clone()))),
                    ]
                    .spacing(space(2))
                    .align_y(Alignment::Center),
                );
            }
            col = col.push(selected_props(&sym, &d, decimals, multiplier));
        }
        None => {
            col = col.push(t::metadata("点图上的线选中它，这里改颜色、线型、精确价格与时间。画线：顶部工具栏「绘图」，或光标在图上按 Alt+T / H / V / R、Alt+Shift+R").color(pal::dim()));
        }
    }
    col = col.push(tree(&sym, decimals));
    col.width(Length::Fill).into()
}

/// 一个参数输入框（回车生效）。
fn param_input<'a>(sym: &str, d: &Drawing, p: Param, placeholder: &str, current: String, width: f32, ro: bool) -> Element<'a, DrawMsg> {
    let key = dw::param_key(&d.id, p);
    let (s, id, k) = (sym.to_string(), d.id.clone(), key.clone());
    let mut inp = text_input(placeholder, &dw::edit_text(&key, current)).size(t::s_small()).width(Length::Fixed(width));
    if !ro {
        inp = inp.on_input(move |v| DrawMsg::Edit(k.clone(), v)).on_submit(DrawMsg::ApplyParam(s, id, p));
    }
    inp.into()
}

fn selected_props<'a>(sym: &str, d: &Drawing, decimals: usize, multiplier: f64) -> Element<'a, DrawMsg> {
    let id = d.id.clone();
    let s = sym.to_string();
    let ro = d.source != Source::Manual;
    let mut col = column![row![t::label(d.kind.label()), t::metadata(source_label(d.source)).color(pal::dim())].spacing(space(2)).align_y(Alignment::Center)]
        .spacing(space(2));
    if ro {
        col = col.push(
            row![
                t::metadata("系统生成的对象只读").color(pal::dim()),
                w::btn("复制为手工对象", Btn::Standard, Some(DrawMsg::CopyToManual(s.clone(), id.clone()))),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
        );
        if let Some(t) = d.text.clone().filter(|t| !t.is_empty()) {
            col = col.push(kv("内容", t::caption(t).into()));
        }
    }
    // 颜色、线宽、线型
    let mut colors = row![].spacing(space(1)).align_y(Alignment::Center);
    for c in COLORS {
        colors = colors.push(swatch(c, d.style.color == c, (!ro).then(|| DrawMsg::Color(s.clone(), id.clone(), c))));
    }
    col = col.push(kv("颜色", colors.wrap().into()));
    let mut widths = row![].spacing(space(1));
    for wd in WIDTHS {
        let on = (d.style.width - wd).abs() < 0.01;
        widths = widths.push(w::btn(format!("{wd}"), if on { Btn::Standard } else { Btn::Ghost }, (!ro).then(|| DrawMsg::Width(s.clone(), id.clone(), wd))));
    }
    col = col.push(kv("线宽", widths.into()));
    let mut dashes = row![].spacing(space(1));
    for ds in [Dash::Solid, Dash::Dashed, Dash::Dotted] {
        dashes = dashes.push(w::btn(dash_label(ds), if d.style.dash == ds { Btn::Standard } else { Btn::Ghost }, (!ro).then(|| DrawMsg::DashStyle(s.clone(), id.clone(), ds))));
    }
    col = col.push(kv("线型", dashes.into()));
    // 各工具自己的参数（G2）
    match d.kind {
        Kind::Text | Kind::Callout => {
            col = col.push(kv("文字", param_input(sym, d, Param::Text, "文字", d.text.clone().unwrap_or_default(), 200.0, ro)));
        }
        Kind::FibRetrace | Kind::FibExt => {
            let cur = d.fib_levels().iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            col = col.push(kv("比例", param_input(sym, d, Param::Levels, "0, 0.382, 0.5, 0.618, 1", cur, 220.0, ro)));
            let (s2, id2) = (s.clone(), id.clone());
            let mut cb = checkbox(d.extend).size(t::s_small());
            if !ro {
                cb = cb.on_toggle(move |v| DrawMsg::Extend(s2.clone(), id2.clone(), v));
            }
            col = col.push(kv("", row![cb, t::caption("各档线向右延伸到图边")].spacing(space(1)).align_y(Alignment::Center).into()));
        }
        Kind::VolProfile => {
            col = col.push(kv(
                "分档",
                row![
                    param_input(sym, d, Param::Rows, "24", format!("{}", d.rows.unwrap_or(24)), 56.0, ro),
                    t::metadata("价值区占比").color(pal::dim()),
                    param_input(sym, d, Param::Va, "0.7", format!("{}", d.va.unwrap_or(0.7)), 56.0, ro),
                ]
                .spacing(space(2))
                .align_y(Alignment::Center)
                .into(),
            ));
            col = col.push(t::metadata("每根 K 线的成交量均摊到它的高低之间（K 线没有逐价位成交时的近似）；改区间、数据更新时自动重算").color(pal::dim()));
        }
        Kind::LongPos | Kind::ShortPos => {
            col = col.push(kv(
                "数量",
                row![
                    param_input(sym, d, Param::Qty, "1", format!("{}", d.qty.unwrap_or(1.0)), 72.0, ro),
                    t::metadata("手续费（基点 / 边）").color(pal::dim()),
                    param_input(sym, d, Param::Fee, "4.5", format!("{}", d.fee_bp.unwrap_or(4.5)), 56.0, ro),
                ]
                .spacing(space(2))
                .align_y(Alignment::Center)
                .into(),
            ));
            if let Some(e) = super::drawing::model::estimate(d, multiplier) {
                col = col.push(
                    t::caption(format!(
                        "风险收益 1 : {:.2} · 到目标 {:+.2}% 估 {:+.m$} · 到止损 {:+.2}% 估 {:+.m$}（合约乘数 {multiplier}，扣进出两边手续费）",
                        e.rr,
                        e.target_pct,
                        e.win,
                        e.stop_pct,
                        e.loss,
                        m = decimals.max(2)
                    ))
                    .color(pal::txt()),
                );
            }
            col = col.push(t::metadata("只是估算，不是委托：不连交易所、不下单").color(pal::warn()));
        }
        _ => {}
    }
    // 锚点：精确价格与时间（回车生效）
    let locked = d.locked || dw::prefs().lock_all;
    for (i, a) in d.anchors.iter().enumerate() {
        let name = if d.anchors.len() == 1 { "位置".to_string() } else { format!("锚点 {}", i + 1) };
        let mut r = row![].spacing(space(1)).align_y(Alignment::Center);
        if d.kind != Kind::VLine {
            let key = dw::edit_key(&id, Some((i, Field::Price)));
            let (s2, id2, k2) = (s.clone(), id.clone(), key.clone());
            let mut inp = text_input("价格", &dw::edit_text(&key, price_text(a.p, decimals))).size(t::s_small()).width(Length::Fixed(96.0));
            if !(ro || locked) {
                inp = inp.on_input(move |v| DrawMsg::Edit(k2.clone(), v)).on_submit(DrawMsg::ApplyAnchor(s2, id2, i, Field::Price));
            }
            r = r.push(inp);
        }
        if d.kind != Kind::HLine {
            let key = dw::edit_key(&id, Some((i, Field::Time)));
            let (s2, id2, k2) = (s.clone(), id.clone(), key.clone());
            let mut inp = text_input("时间（UTC）", &dw::edit_text(&key, time_text(a.t))).size(t::s_small()).width(Length::Fixed(140.0));
            if !(ro || locked) {
                inp = inp.on_input(move |v| DrawMsg::Edit(k2.clone(), v)).on_submit(DrawMsg::ApplyAnchor(s2, id2, i, Field::Time));
            }
            r = r.push(inp);
        }
        col = col.push(kv(&name, r.wrap().into()));
    }
    col = col.push(t::metadata(if locked { "锁定中：位置不能改" } else { "改完按回车；时间是 UTC" }).color(pal::dim()));
    // 显示在哪几类周期上
    let mut cats = row![].spacing(space(3)).align_y(Alignment::Center);
    for (cat, label) in TF_CATS {
        let on = d.shown_on(cat);
        let (s2, id2) = (s.clone(), id.clone());
        let mut cb = checkbox(on).size(t::s_small());
        if !ro {
            cb = cb.on_toggle(move |v| DrawMsg::ShowOn(s2.clone(), id2.clone(), cat.to_string(), v));
        }
        cats = cats.push(row![cb, t::caption(label)].spacing(space(1)).align_y(Alignment::Center));
    }
    col = col.push(kv("显示在", cats.wrap().into()));
    // 到价提醒（G5）：水平线、趋势线、射线；只提醒，不下单
    if !ro && d.kind.alertable() {
        use super::drawing::model::AlertWhen;
        let cur = d.alert.as_ref().map(|a| a.when);
        let mut r = row![w::btn("关", if cur.is_none() { Btn::Standard } else { Btn::Ghost }, Some(DrawMsg::SetAlert(s.clone(), id.clone(), None)))].spacing(space(1));
        for wh in AlertWhen::ALL {
            r = r.push(w::btn(wh.label(), if cur == Some(wh) { Btn::Standard } else { Btn::Ghost }, Some(DrawMsg::SetAlert(s.clone(), id.clone(), Some(wh)))));
        }
        col = col.push(kv("到价提醒", r.wrap().into()));
        if let Some(a) = &d.alert {
            let (s2, id2, s3, id3) = (s.clone(), id.clone(), s.clone(), id.clone());
            col = col.push(kv(
                "",
                row![
                    checkbox(a.once).on_toggle(move |v| DrawMsg::AlertOnce(s2.clone(), id2.clone(), v)).size(t::s_small()),
                    t::caption("只提醒一次"),
                    checkbox(a.telegram).on_toggle(move |v| DrawMsg::AlertTelegram(s3.clone(), id3.clone(), v)).size(t::s_small()),
                    t::caption("也发 Telegram"),
                ]
                .spacing(space(1))
                .align_y(Alignment::Center)
                .wrap()
                .into(),
            ));
            let fired = a.last_fired.map(|t| time_text(t).get(5..16).unwrap_or_default().to_string());
            let status = match (a.armed, fired) {
                (true, None) => "等价格穿过这条线（图表开着、有新数据时检查）".to_string(),
                (true, Some(f)) => format!("上次响在 {f}（UTC）；同一条线一分钟内不重复"),
                (false, f) => format!("响过了（{}），只提醒一次所以停了", f.unwrap_or_default()),
            };
            col = col.push(row![t::metadata(status).color(pal::dim())].spacing(space(2)).align_y(Alignment::Center));
            if !a.armed {
                col = col.push(row![w::btn("重新开启", Btn::Ghost, Some(DrawMsg::AlertRearm(s.clone(), id.clone())))]);
            }
            col = col.push(t::metadata("与金融日历同一个通道：通知中心、桌面通知、（勾了且配好的）Telegram；免打扰时段照样生效。只提醒，不会下单").color(pal::dim()));
        }
    }
    // 来源周期（G3）：画在哪个周期；可以只在那个周期显示
    if let Some(o) = d.origin_tf.clone() {
        let (s2, id2) = (s.clone(), id.clone());
        let mut cb = checkbox(d.only_origin).size(t::s_small());
        if !ro {
            cb = cb.on_toggle(move |v| DrawMsg::OnlyOrigin(s2.clone(), id2.clone(), v));
        }
        col = col.push(kv(
            "来源周期",
            row![t::caption(format!("画于 {o}")), cb, t::caption(format!("只在 {o} 上显示"))].spacing(space(2)).align_y(Alignment::Center).wrap().into(),
        ));
    }
    // 样式模板（G3）：套用 / 删除这种工具的模板；把当前样式存成模板、设为缺省
    if !ro {
        let p = dw::prefs();
        let mut tpls = row![].spacing(space(1)).align_y(Alignment::Center);
        let mut any = false;
        for (i, tp) in p.templates.iter().enumerate().filter(|(_, tp)| tp.kind == d.kind) {
            any = true;
            tpls = tpls.push(
                row![
                    swatch(tp.style.color, false, None),
                    w::btn(tp.name.clone(), Btn::Ghost, Some(DrawMsg::ApplyTemplate(s.clone(), id.clone(), i))),
                    w::btn("✕", Btn::Ghost, Some(DrawMsg::DeleteTemplate(i))),
                ]
                .spacing(0)
                .align_y(Alignment::Center),
            );
        }
        if !any {
            tpls = tpls.push(t::metadata(format!("还没有{}的模板", d.kind.label())).color(pal::dim()));
        }
        col = col.push(kv("模板", tpls.wrap().into()));
        let tkey = dw::template_key(&id);
        let (s2, id2, k2) = (s.clone(), id.clone(), tkey.clone());
        let has_default = p.defaults.iter().any(|(k, _)| *k == d.kind);
        col = col.push(kv(
            "",
            row![
                text_input("模板名", &dw::edit_text(&tkey, String::new()))
                    .size(t::s_small())
                    .width(Length::Fixed(110.0))
                    .on_input(move |v| DrawMsg::Edit(k2.clone(), v))
                    .on_submit(DrawMsg::SaveTemplate(s2, id2)),
                w::btn("存为模板", Btn::Ghost, Some(DrawMsg::SaveTemplate(s.clone(), id.clone()))),
                w::btn(format!("设为{}缺省", d.kind.label()), Btn::Ghost, Some(DrawMsg::SetDefault(s.clone(), id.clone()))),
                if has_default { w::btn("清除缺省", Btn::Ghost, Some(DrawMsg::ClearDefault(d.kind))) } else { Space::new().width(Length::Shrink).into() },
            ]
            .spacing(space(1))
            .align_y(Alignment::Center)
            .wrap()
            .into(),
        ));
    }
    // 分组
    let gkey = dw::edit_key(&id, None);
    let (s2, id2, g2) = (s.clone(), id.clone(), gkey.clone());
    let mut ginp = text_input("不分组", &dw::edit_text(&gkey, d.group.clone().unwrap_or_default())).size(t::s_small()).width(Length::Fixed(140.0));
    if !ro {
        ginp = ginp.on_input(move |v| DrawMsg::Edit(g2.clone(), v)).on_submit(DrawMsg::ApplyGroup(s2, id2));
    }
    col = col.push(kv("分组", row![ginp, t::metadata("回车生效").color(pal::dim())].spacing(space(2)).align_y(Alignment::Center).into()));
    // 锁定 / 隐藏 / 删除
    let (s3, id3, s4, id4) = (s.clone(), id.clone(), s.clone(), id.clone());
    let mut lock = checkbox(d.locked).size(t::s_small());
    let mut hide = checkbox(d.hidden).size(t::s_small());
    if !ro {
        lock = lock.on_toggle(move |v| DrawMsg::Lock(s3.clone(), id3.clone(), v));
        hide = hide.on_toggle(move |v| DrawMsg::Hide(s4.clone(), id4.clone(), v));
    }
    col = col.push(
        row![
            lock,
            t::caption("锁定"),
            hide,
            t::caption("隐藏"),
            Space::new().width(Length::Fill),
            w::btn("删除", Btn::Destructive, (!ro && !locked).then(|| DrawMsg::Delete(s.clone(), id.clone()))),
        ]
        .spacing(space(1))
        .align_y(Alignment::Center),
    );
    container(col).padding(space(2)).style(|_| container::Style { border: iced::Border { color: pal::line(), width: 1.0, radius: 4.0.into() }, ..Default::default() }).into()
}

/// 对象树：按分组列出这个品种的全部对象（含隐藏的）；点名字选中，每行可隐藏 / 锁定 / 删除；分组整组隐藏 / 锁定。
fn tree<'a>(sym: &str, decimals: usize) -> Element<'a, DrawMsg> {
    let all = dw::items(sym);
    let sel = dw::selected(sym);
    let s = sym.to_string();
    let mut col = column![row![
        t::label(format!("对象（{}）", all.len())),
        Space::new().width(Length::Fill),
        w::btn("撤销", Btn::Ghost, Some(DrawMsg::Undo(s.clone()))),
        w::btn("重做", Btn::Ghost, Some(DrawMsg::Redo(s.clone()))),
    ]
    .spacing(space(1))
    .align_y(Alignment::Center)]
    .spacing(space(1));
    if all.is_empty() {
        return col.push(t::metadata("这个品种还没有画线").color(pal::dim())).into();
    }
    // 分组：没分组的在前，组按名字
    // 系统对象按来源归类（「系统 · 订单」…），不跟手工的分组混在一起
    let key = |d: &Drawing| -> Option<String> {
        if d.source == Source::Manual { d.group.clone() } else { Some(format!("系统 · {}{}", d.source.label(), d.group.as_ref().map(|g| format!(" · {g}")).unwrap_or_default())) }
    };
    let mut groups: Vec<Option<String>> = all.iter().map(key).collect();
    groups.sort();
    groups.dedup();
    for g in groups {
        let members: Vec<&Drawing> = all.iter().filter(|d| key(d) == g).collect();
        let system = members.first().is_some_and(|d| d.source != Source::Manual);
        if system {
            // 系统分组缺省折叠（日历一组两百多个）：点标题展开；展开了也最多列 200 个
            let name = g.clone().unwrap_or_default();
            let open = dw::group_expanded(&name);
            col = col.push(
                button(t::caption(format!("{} {name}（{}）", if open { "▾" } else { "▸" }, members.len())).color(pal::head()))
                    .padding(iced::Padding::ZERO)
                    .style(|th, st| w::button_style(Btn::Ghost, th, st))
                    .on_press(DrawMsg::ToggleGroup(name.clone())),
            );
            if !open {
                continue;
            }
        } else if let Some(name) = &g {
            let all_hidden = members.iter().all(|d| d.hidden);
            let all_locked = members.iter().all(|d| d.locked);
            col = col.push(
                row![
                    t::caption(format!("▸ {name}（{}）", members.len())).color(pal::head()),
                    Space::new().width(Length::Fill),
                    w::btn(if all_hidden { "显示" } else { "隐藏" }, Btn::Ghost, Some(DrawMsg::GroupHide(s.clone(), name.clone(), !all_hidden))),
                    w::btn(if all_locked { "解锁" } else { "锁定" }, Btn::Ghost, Some(DrawMsg::GroupLock(s.clone(), name.clone(), !all_locked))),
                ]
                .spacing(space(1))
                .align_y(Alignment::Center),
            );
        }
        let shown = members.len().min(200);
        let more = members.len() - shown;
        for d in members.into_iter().take(shown) {
            let on = sel.as_deref() == Some(d.id.as_str());
            let label = match &d.origin_tf {
                Some(o) => format!("{} · {o}", object_label(d, decimals)),
                None => object_label(d, decimals),
            };
            let name = button(t::caption(label).color(if d.hidden { pal::dim() } else { pal::txt() }))
                .padding(iced::Padding::ZERO)
                .style(move |th, st| w::button_style(if on { Btn::Standard } else { Btn::Ghost }, th, st))
                .on_press(DrawMsg::Select(s.clone(), Some(d.id.clone())))
                .width(Length::Fill);
            let editable = d.source == Source::Manual;
            col = col.push(
                row![
                    swatch(d.style.color, false, None),
                    name,
                    w::btn(if d.hidden { "显示" } else { "隐藏" }, Btn::Ghost, editable.then(|| DrawMsg::Hide(s.clone(), d.id.clone(), !d.hidden))),
                    w::btn(if d.locked { "解锁" } else { "锁定" }, Btn::Ghost, editable.then(|| DrawMsg::Lock(s.clone(), d.id.clone(), !d.locked))),
                    w::btn("✕", Btn::Ghost, (editable && !d.locked).then(|| DrawMsg::Delete(s.clone(), d.id.clone()))),
                ]
                .spacing(space(1))
                .align_y(Alignment::Center),
            );
        }
        if more > 0 {
            col = col.push(t::metadata(format!("…还有 {more} 个（在图上点选，或用绘图菜单按来源隐藏）")).color(pal::dim()));
        }
    }
    col.into()
}

// ── 图上的浮动小条 ────────────────────────────────────────────────────────────

/// 选中对象旁的小操作条：四个常用颜色、线宽、线型、锁定、删除。
pub fn floating<'a>(sym: &str, d: &Drawing) -> Element<'a, DrawMsg> {
    let (s, id) = (sym.to_string(), d.id.clone());
    if d.source != Source::Manual {
        return container(
            row![t::metadata(format!("{} · {}（只读）", d.kind.label(), source_label(d.source))).color(pal::dim()), w::btn("属性…", Btn::Ghost, Some(DrawMsg::OpenInspector))]
                .spacing(space(1))
                .align_y(Alignment::Center),
        )
        .padding(space(1))
        .into();
    }
    let mut r = row![].spacing(space(1)).align_y(Alignment::Center);
    for c in &COLORS[..4] {
        r = r.push(swatch(*c, d.style.color == *c, Some(DrawMsg::Color(s.clone(), id.clone(), *c))));
    }
    let next_w = WIDTHS.iter().copied().find(|w| *w > d.style.width + 0.01).unwrap_or(WIDTHS[0]);
    let next_d = match d.style.dash {
        Dash::Solid => Dash::Dashed,
        Dash::Dashed => Dash::Dotted,
        Dash::Dotted => Dash::Solid,
    };
    r = r
        .push(w::btn(format!("线宽 {}", d.style.width), Btn::Ghost, Some(DrawMsg::Width(s.clone(), id.clone(), next_w))))
        .push(w::btn(dash_label(d.style.dash), Btn::Ghost, Some(DrawMsg::DashStyle(s.clone(), id.clone(), next_d))))
        .push(w::btn(if d.locked { "解锁" } else { "锁定" }, Btn::Ghost, Some(DrawMsg::Lock(s.clone(), id.clone(), !d.locked))))
        .push(w::btn("删除", Btn::Ghost, (!d.locked).then_some(DrawMsg::Delete(s, id))))
        .push(w::btn("属性…", Btn::Ghost, Some(DrawMsg::OpenInspector)));
    container(r)
        .padding(space(1))
        .style(|_| container::Style {
            background: Some(iced::Background::Color(crate::ui::chart::drawing_handle_fill())),
            border: iced::Border { color: pal::accent(), width: 1.0, radius: 4.0.into() },
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::drawing::model::Anchor;

    #[test]
    fn object_labels_read_like_the_chart() {
        let a = Anchor { t: 1_792_038_600_000, p: 36_400_000_000 };
        let b = Anchor { t: 1_792_042_200_000, p: 37_020_000_000 };
        assert_eq!(object_label(&Drawing::new(Kind::HLine, vec![a]), 4), "水平线 · 0.3640");
        assert_eq!(object_label(&Drawing::new(Kind::VLine, vec![a]), 4), "垂直线 · 10-15 04:30");
        assert_eq!(object_label(&Drawing::new(Kind::Trend, vec![a, b]), 4), "趋势线 · 0.3640 → 0.3702");
        let mut sys = Drawing::new(Kind::HLine, vec![a]);
        sys.source = Source::Order;
        assert_eq!(object_label(&sys, 2), "水平线 · 0.36（订单）");
        // 菜单里每种工具都有，「标记」除外（它是系统生成的：成交、订单流事件、策略决策）
        let in_menu: Vec<Kind> = menu_groups().into_iter().flat_map(|(_, k)| k).collect();
        assert!(Kind::all().iter().all(|k| in_menu.contains(k) || *k == Kind::Marker) && !in_menu.contains(&Kind::Marker));
        assert!(menu_groups().iter().any(|(g, k)| *g == "线条" && k.contains(&Kind("arrow"))), "登记的工具并进它声明的组");
    }
}
