//! 绘图的界面（docs/46 G1）：顶部工具栏的「绘图」组与下拉菜单、检查器里选中对象的属性与对象树、
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
pub const fn shortcut(k: Kind) -> &'static str {
    match k {
        Kind::HLine => "Alt+H",
        Kind::VLine => "Alt+V",
        Kind::Trend => "Alt+T",
        Kind::Ray => "Alt+R",
        Kind::Rect => "Alt+Shift+R",
        Kind::FibRetrace => "Alt+F",
        _ => "",
    }
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
    };
    let mut s = format!("{} · {what}", d.kind.label());
    if d.source != Source::Manual {
        s = format!("{}（{}）", s, source_label(d.source));
    }
    s
}

// ── 工具栏 ───────────────────────────────────────────────────────────────────

/// 工具栏「绘图」组：当前工具（点开 / 收起菜单）+ 收藏的工具（再点一下回到光标）。
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
        r = r.push(w::btn(k.label(), if on { Btn::Standard } else { Btn::Ghost }, Some(DrawMsg::Tool((!on).then_some(k)))));
    }
    if p.hide_all {
        r = r.push(w::btn("画线已隐藏", Btn::Subtle, Some(DrawMsg::HideAll(false))));
    }
    r.into()
}

/// 绘图菜单：盖在内容上方的卡片（与「⋯ 页面」同一种做法）。
pub fn menu<'a>() -> Element<'a, DrawMsg> {
    let tool = dw::tool();
    let p = dw::prefs();
    let mut col = column![
        row![
            t::section("绘图工具"),
            Space::new().width(Length::Fill),
            w::btn("↖ 光标", if tool.is_none() { Btn::Standard } else { Btn::Ghost }, Some(DrawMsg::Tool(None))),
            w::btn("收起", Btn::Ghost, Some(DrawMsg::CloseMenu)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center)
    ]
    .spacing(space(2));
    for (group, kinds) in GROUPS {
        let mut r = row![t::metadata(group).color(pal::dim()).width(Length::Fixed(40.0))].spacing(space(2)).align_y(Alignment::Center);
        for &k in kinds {
            let fav = p.favorites.contains(&k);
            r = r.push(
                row![
                    w::btn(
                        if shortcut(k).is_empty() { k.label().to_string() } else { format!("{}  {}", k.label(), shortcut(k)) },
                        if tool == Some(k) { Btn::Standard } else { Btn::Ghost },
                        Some(DrawMsg::Tool(Some(k))),
                    ),
                    w::btn(if fav { "★" } else { "☆" }, Btn::Ghost, Some(DrawMsg::Favorite(k))),
                ]
                .spacing(0)
                .align_y(Alignment::Center),
            );
        }
        col = col.push(r.wrap());
    }
    let tick = |on: bool, label: &'static str, f: fn(bool) -> DrawMsg| row![checkbox(on).on_toggle(f).size(t::s_small()), t::caption(label)].spacing(space(1)).align_y(Alignment::Center);
    col = col.push(
        row![
            tick(p.continuous, "连续绘制", DrawMsg::Continuous),
            tick(p.magnet, "磁吸（吸到开 / 高 / 低 / 收）", DrawMsg::Magnet),
            tick(p.hide_all, "全部隐藏", DrawMsg::HideAll),
            tick(p.lock_all, "全部锁定", DrawMsg::LockAll),
        ]
        .spacing(space(4))
        .wrap(),
    );
    col = col.push(
        t::metadata("光标在图上时：点线选中、拖动；Delete 删除；Ctrl+Z 撤销、Ctrl+Shift+Z 重做；Esc 退出工具；折线回车结束。改颜色、精确价格与时间、文字、斐波那契比例、仓位数量在检查器「属性」页。仓位工具的盈亏只是估算，不会下单").color(pal::dim()),
    );
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
    match dw::selected_drawing(symbol) {
        Some(d) => col = col.push(selected_props(&sym, &d, decimals, multiplier)),
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
        return container(t::metadata(format!("{} · {}（只读）", d.kind.label(), source_label(d.source))).color(pal::dim())).padding(space(1)).into();
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
        .push(w::btn("删除", Btn::Ghost, (!d.locked).then_some(DrawMsg::Delete(s, id))));
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
        let in_menu: Vec<Kind> = GROUPS.iter().flat_map(|(_, k)| k.iter().copied()).collect();
        assert!(Kind::ALL.iter().all(|k| in_menu.contains(k) || *k == Kind::Marker) && !in_menu.contains(&Kind::Marker));
    }
}
