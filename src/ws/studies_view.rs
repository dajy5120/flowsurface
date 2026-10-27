//! 图上指标的界面（docs/63 §12，TradingView 式）：
//!
//! - 顶部工具栏「指标 · N ▾」（紧跟「绘图」）→ [`menu_card`] 指标浏览器卡片：「添加」（搜索 / 分类 / 收藏 / 最近用过，点一行就加、卡片不关）
//!   与「已加载」（👁 开关、名字点开设置、⚙、⧉ 复制、↑↓ 排序、✕ 删除；模板）；
//! - [`editor`] 单个指标的设置弹窗：参数 / 样式 / 可见性 / 提醒，改了立刻生效；确定 / 取消（放回打开前）/ 恢复默认 / 存为我的默认；
//! - 检查器「属性」页的「指标」一节 [`inspector`] 复用同一份「已加载」列表。
//!
//! 每张图各自一组（`chart` = 面板设置里的 `chart_id`）；`symbol` = 图现在的品种键（只用来解析「别的品种」与锚点）。

use iced::widget::{button, column, container, row, scrollable, text, text_input, tooltip, Space};
use iced::{Alignment, Element, Length};

use wealthspring_indicators::meta::ParamKind;

use super::studies::{self as st, AlertOp, EditTab, MenuTab, StudyMsg, StudySrc};
use crate::ui::widgets::{self as w, Kind};
use crate::ui::{metrics, pal, text as t};

const CATEGORIES: [(&str, &str); 10] = [("trend", "趋势"), ("momentum", "动量"), ("volatility", "波动"), ("volume", "成交量"),
                                       ("vwap", "VWAP"), ("structure", "结构"), ("statistics", "统计"), ("derivatives", "衍生品"), ("breadth", "广度"),
                                       ("custom", "自定义")];

/// 画法（样式页）：（值, 名字）。
const PLOTS: [(&str, &str); 4] = [("line", "线"), ("step", "阶梯"), ("histogram", "柱"), ("dots", "点")];

fn chip<'a>(label: impl Into<String>, active: bool, msg: StudyMsg) -> Element<'a, StudyMsg> {
    let label: String = label.into();
    button(text(if active { format!("✓ {label}") } else { label }).size(t::s_small()))
        .padding(metrics::pad2(0, 2))
        .style(move |th, s| crate::style::button::modifier(th, s, active))
        .on_press(msg)
        .into()
}

fn small<'a>(label: impl Into<String>, msg: StudyMsg) -> Element<'a, StudyMsg> {
    button(text(label.into()).size(t::s_meta())).padding(metrics::pad2(0, 1)).style(|th, s| crate::style::button::modifier(th, s, false)).on_press(msg).into()
}

/// 小图标按钮 + 悬停说明。
fn icon<'a>(glyph: &'static str, tip: &'static str, msg: StudyMsg) -> Element<'a, StudyMsg> {
    tooltip(
        button(text(glyph).size(t::s_small())).padding(metrics::pad2(0, 1)).style(|th, s| crate::style::button::modifier(th, s, false)).on_press(msg),
        container(t::caption(tip)).padding(metrics::space(2)).style(crate::style::tooltip),
        tooltip::Position::Top,
    )
    .into()
}

fn lab<'a>(s: &'static str) -> iced::widget::Text<'a> {
    text(s).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(64.0))
}

// ── 工具栏入口 ─────────────────────────────────────────────────────────────

/// 工具栏「指标 · N ▾」：点开 / 收起这张图的指标浏览器。
pub fn toolbar<'a>(chart: &str) -> Element<'a, StudyMsg> {
    let n = st::studies_of(chart).len();
    let open = st::ui_state().menu.as_deref() == Some(chart);
    let mut v = if n == 0 { "添加".to_string() } else { format!("{n} 个") };
    if open {
        v.push_str(" ▴");
    }
    crate::ui::shell::tool_pick("指标", v, Some(StudyMsg::Menu((!open).then(|| chart.to_string()))), open)
}

// ── 指标浏览器卡片 ─────────────────────────────────────────────────────────

/// 盖在内容上方的指标浏览器（工具栏「指标 ▾」点开）。
pub fn menu_card<'a>(chart: &str, symbol: &str) -> Element<'a, StudyMsg> {
    let ui = st::ui_state();
    let n = st::studies_of(chart).len();
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_string();
    let head = row![
        t::section("指标"),
        t::metadata(format!("{code} · 这张图")).color(pal::dim()),
        Space::new().width(Length::Fixed(metrics::space(4))),
        w::tabs(&[("添加", MenuTab::Add), ("已加载", MenuTab::Loaded)], &ui.menu_tab, StudyMsg::MenuTab),
        t::metadata(format!("{n} 个")).color(pal::dim()),
        Space::new().width(Length::Fill),
        w::btn("关闭", Kind::Ghost, Some(StudyMsg::Menu(None))),
    ]
    .spacing(metrics::space(2))
    .align_y(Alignment::Center);
    let body: Element<'a, StudyMsg> = match ui.menu_tab {
        MenuTab::Add => add_tab(chart, &ui),
        MenuTab::Loaded => loaded_list(chart, true),
    };
    let mut col = column![head, body].spacing(metrics::space(2));
    if !ui.note.is_empty() {
        col = col.push(text(ui.note).size(t::s_meta()).color(pal::warn()));
    }
    container(col).padding(metrics::space(3)).width(Length::Fill).into()
}

fn add_tab<'a>(chart: &str, ui: &st::UiState) -> Element<'a, StudyMsg> {
    let store = st::snapshot();
    let mut b = column![].spacing(metrics::space(2));
    b = b.push(
        text_input("搜索名字或用途（趋势强度、动态止损、超买超卖……）", &ui.search)
            .on_input(StudyMsg::Search)
            .size(t::s_small()),
    );
    let mut cats = row![chip("全部", ui.category.is_none(), StudyMsg::Category(None))].spacing(metrics::space(1));
    for (k, n) in CATEGORIES {
        cats = cats.push(chip(n, ui.category.as_deref() == Some(k), StudyMsg::Category(Some(k.to_string()))));
    }
    b = b.push(cats.wrap());
    let q = ui.search.trim().to_lowercase();
    let defs = st::all_defs();
    let mut shown: Vec<_> = defs
        .iter()
        .filter(|d| ui.category.as_deref().is_none_or(|c| d.category == c || (c == "custom" && d.id.starts_with("user."))))
        .filter(|d| q.is_empty() || d.name.to_lowercase().contains(&q) || d.id.contains(&q) || d.tags.iter().any(|x| x.contains(&q)) || d.description.contains(&q))
        .collect();
    // 收藏在前，再最近用过，其余按原顺序
    let rank = |id: &str| if store.favorites.iter().any(|x| x == id) { 0 } else if ui.recent_at_open.iter().any(|x| x == id) { 1 } else { 2 };
    shown.sort_by_key(|d| rank(d.id));
    let mut lib = column![].spacing(1);
    for d in &shown {
        let fav = store.favorites.iter().any(|x| x == d.id);
        let pane = if d.pane == "overlay" { "主图" } else { "副图" };
        let added = ui.just_added.iter().filter(|x| *x == d.id).count();
        let name = row![
            text(d.name).size(t::s_small()),
            text(pane).size(t::s_meta()).color(pal::dim()),
            text(d.tags.join("、")).size(t::s_meta()).color(pal::dim()),
        ]
        .spacing(metrics::space(2))
        .align_y(Alignment::Center);
        // 整行是按钮：点一下就加到这张图（卡片不关，可连着加）
        let add = tooltip(
            button(name).width(Length::Fill).padding(metrics::pad2(1, 2)).style(|th, s| crate::style::button::modifier(th, s, false))
                .on_press(StudyMsg::Add { chart: chart.to_string(), id: d.id.to_string() }),
            container(t::caption(d.description)).padding(metrics::space(2)).max_width(420.0).style(crate::style::tooltip),
            tooltip::Position::Bottom,
        );
        let mut r = row![small(if fav { "★" } else { "☆" }, StudyMsg::Favorite(d.id.to_string())), add].spacing(metrics::space(1)).align_y(Alignment::Center);
        if added > 0 {
            r = r.push(text(if added == 1 { "已加".to_string() } else { format!("已加 ×{added}") }).size(t::s_meta()).color(pal::ok()));
        }
        lib = lib.push(r);
    }
    if shown.is_empty() {
        lib = lib.push(t::metadata("没有匹配的指标").color(pal::dim()));
    }
    b = b.push(scrollable(lib).height(Length::Fixed(320.0)));
    for (f, e) in st::user_errors() {
        b = b.push(text(format!("自定义指标 {f} 读不了：{e}")).size(t::s_meta()).color(pal::warn()));
    }
    b = b.push(t::metadata("点一行加到这张图（卡片不关，可连着加）；★ 收藏的排在前面；悬停看说明").color(pal::dim()));
    b.into()
}

/// 这张图已加载的指标：一行一个（👁、名字 → 设置、⚙、⧉、↑↓、✕）+ 模板。浏览器卡片与检查器共用。
fn loaded_list<'a>(chart: &str, templates: bool) -> Element<'a, StudyMsg> {
    let store = st::snapshot();
    let ui = st::ui_state();
    let list = st::studies_of(chart);
    let c = chart.to_string();
    let mut b = column![].spacing(metrics::space(1));
    if list.is_empty() {
        b = b.push(t::metadata("这张图还没有指标：在「添加」里点一个").color(pal::dim()));
    }
    if let Some(t0) = st::recomputed_from(chart) {
        let when = chrono::DateTime::from_timestamp_millis(t0 as i64)
            .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        b = b.push(t::metadata(format!("数据修正过：从 {when} 那根起重算（下游指标跟着重算）")).color(pal::dim()));
    }
    let errors = st::errors_for(chart);
    for (i, s) in list.iter().enumerate() {
        let uid = s.uid;
        let color = s.def().and_then(|d| d.outputs.first().map(|o| st::color_of(s, 0, o.key))).unwrap_or(pal::dim());
        let name_c = if s.visible { color } else { pal::dim() };
        let dragging = ui.drag.as_ref().is_some_and(|d| d.0 == chart && d.1 == uid);
        // 拖动排序（T3）：按住把手，拖过别的行就挪到那里，松开结束
        let grip = tooltip(
            iced::widget::mouse_area(text("⠿").size(t::s_small()).color(pal::dim()))
                .on_press(StudyMsg::DragStart { chart: c.clone(), uid })
                .interaction(iced::mouse::Interaction::Grab),
            container(t::caption("按住拖动排序")).padding(metrics::space(2)).style(crate::style::tooltip),
            tooltip::Position::Top,
        );
        let mut r = row![
            grip,
            icon(if s.visible { "👁" } else { "◌" }, if s.visible { "隐藏" } else { "显示" }, StudyMsg::Visible { chart: c.clone(), uid, on: !s.visible }),
            button(text(s.label()).size(t::s_small()).color(name_c))
                .padding(metrics::pad2(0, 1))
                .style(|th, st_| crate::style::button::modifier(th, st_, false))
                .on_press(StudyMsg::Edit(Some((c.clone(), uid)))),
            text(if s.pane == "sub" { "副图" } else { "主图" }).size(t::s_meta()).color(pal::dim()),
        ]
        .spacing(metrics::space(1))
        .align_y(Alignment::Center);
        if s.alert.as_ref().is_some_and(|a| a.armed) {
            r = r.push(text("🔔").size(t::s_meta()));
        }
        r = r.push(Space::new().width(Length::Fill));
        r = r.push(icon("⚙", "设置", StudyMsg::Edit(Some((c.clone(), uid)))));
        r = r.push(icon("⧉", "复制一个", StudyMsg::Duplicate { chart: c.clone(), uid }));
        r = r.push(icon("↑", "上移", StudyMsg::Move { chart: c.clone(), uid, up: true }));
        r = r.push(icon("↓", "下移", StudyMsg::Move { chart: c.clone(), uid, up: false }));
        r = r.push(icon("✕", "删除", StudyMsg::Remove { chart: c.clone(), uid }));
        let r = container(r).style(move |_| container::Style {
            background: dragging.then(|| iced::Background::Color(pal::alpha(pal::accent(), 0.18))),
            ..Default::default()
        });
        b = b.push(
            iced::widget::mouse_area(r)
                .on_enter(StudyMsg::DragOver { chart: c.clone(), uid })
                .on_release(StudyMsg::DragEnd)
                .interaction(if ui.drag.is_some() { iced::mouse::Interaction::Grabbing } else { iced::mouse::Interaction::default() }),
        );
        if let Some(Some(e)) = errors.get(i) {
            b = b.push(text(e.clone()).size(t::s_meta()).color(pal::warn()));
        }
    }
    // 拖出列表、在列表里任何地方松开：结束拖动
    let mut b = column![iced::widget::mouse_area(b).on_exit(StudyMsg::DragEnd).on_release(StudyMsg::DragEnd)];
    if templates {
        b = b.push(Space::new().height(Length::Fixed(metrics::space(2))));
        b = b.push(t::metadata("模板：把这张图的指标存下来，别的图一键套用（套用 = 换成模板里那组）").color(pal::dim()));
        b = b.push(
            row![
                text_input("模板名", &ui.template_name).on_input(StudyMsg::TemplateName).size(t::s_small()).width(Length::Fixed(160.0)),
                small("存为模板", StudyMsg::SaveTemplate { chart: c.clone(), name: ui.template_name.clone() }),
            ]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center),
        );
        let mut tl = row![].spacing(metrics::space(2));
        for name in store.templates.keys() {
            tl = tl.push(
                row![
                    small(format!("套用「{name}」"), StudyMsg::ApplyTemplate { chart: c.clone(), name: name.clone() }),
                    small("×", StudyMsg::DeleteTemplate(name.clone())),
                ]
                .spacing(1),
            );
        }
        b = b.push(tl.wrap());
    }
    b.into()
}

// ── 检查器 ─────────────────────────────────────────────────────────────────

/// 检查器「属性」页的「指标」一节：与浏览器卡片「已加载」同一份列表；加指标、改设置走卡片与弹窗。
pub fn inspector<'a>(chart: &str) -> Element<'a, StudyMsg> {
    let ui = st::ui_state();
    let mut b = column![
        row![
            w::section("指标"),
            Space::new().width(Length::Fill),
            w::btn("＋ 添加指标", Kind::Standard, Some(StudyMsg::Menu(Some(chart.to_string())))),
        ]
        .align_y(Alignment::Center),
        t::metadata("这张图自己的一组（每张图各自一组）；点名字或 ⚙ 打开设置").color(pal::dim()),
        loaded_list(chart, true),
    ]
    .spacing(metrics::space(2));
    if !ui.note.is_empty() {
        b = b.push(text(ui.note).size(t::s_meta()).color(pal::warn()));
    }
    b.width(Length::Fill).into()
}

// ── 设置弹窗 ───────────────────────────────────────────────────────────────

/// 单个指标的设置弹窗（开着时；居中盖在内容上方，图照常看得到——改了立刻生效）。
pub fn editor<'a>() -> Option<Element<'a, StudyMsg>> {
    let ui = st::ui_state();
    let (chart, uid, _) = ui.editing.clone()?;
    let list = st::studies_of(&chart);
    let i = list.iter().position(|x| x.uid == uid)?;
    let s = &list[i];
    let d = s.def()?;
    let symbol = st::chart_symbol(&chart);
    let head = row![
        t::section(format!("{} · 设置", s.label())),
        Space::new().width(Length::Fill),
        w::btn("✕", Kind::Ghost, Some(StudyMsg::EditCancel)),
    ]
    .align_y(Alignment::Center);
    let tabs = w::tabs(
        &[("参数", EditTab::Params), ("样式", EditTab::Style), ("可见性", EditTab::Visibility), ("提醒", EditTab::Alert)],
        &ui.edit_tab,
        StudyMsg::EditTab,
    );
    let body: Element<'a, StudyMsg> = match ui.edit_tab {
        EditTab::Params => params_tab(&chart, &symbol, &list, i, &d, &ui),
        EditTab::Style => style_tab(&chart, s, &d),
        EditTab::Visibility => visibility_tab(&chart, s),
        EditTab::Alert => alert_rows(&chart, s, &d),
    };
    let mut col = column![
        head,
        text(d.description).size(t::s_meta()).color(pal::dim()),
        tabs,
        scrollable(container(body).padding(metrics::pad2(2, 0))).height(Length::Fixed(360.0)),
    ]
    .spacing(metrics::space(2));
    if !ui.note.is_empty() {
        col = col.push(text(ui.note.clone()).size(t::s_meta()).color(pal::warn()));
    }
    col = col.push(
        row![
            w::btn("恢复默认", Kind::Ghost, Some(StudyMsg::Reset { chart: chart.clone(), uid })),
            w::btn("存为我的默认", Kind::Ghost, Some(StudyMsg::SaveDefault { chart: chart.clone(), uid })),
            Space::new().width(Length::Fill),
            w::btn("取消", Kind::Standard, Some(StudyMsg::EditCancel)),
            w::btn("确定", Kind::Primary, Some(StudyMsg::Edit(None))),
        ]
        .spacing(metrics::space(2))
        .align_y(Alignment::Center),
    );
    let card = container(iced::widget::opaque(container(col).padding(metrics::space(5)).width(Length::Fixed(560.0)).style(w::float_card)))
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding(iced::Padding { top: 72.0, ..iced::Padding::ZERO });
    Some(card.into())
}

/// 参数页：计算参数、数据源、计算周期、品种、对照品种、锚点。
fn params_tab<'a>(chart: &str, symbol: &str, list: &[st::Study], i: usize, d: &wealthspring_indicators::meta::IndicatorDef, ui: &st::UiState) -> Element<'a, StudyMsg> {
    let s = &list[i];
    let uid = s.uid;
    let c = chart.to_string();
    let mut body = column![].spacing(metrics::space(2));
    // 计算参数
    for p in &d.inputs {
        let cur = s.params.get(p.key).unwrap_or(&p.default).clone();
        let ctl: Element<'a, StudyMsg> = match p.kind {
            ParamKind::Int | ParamKind::Float => {
                let (c2, key) = (c.clone(), p.key.to_string());
                text_input("", &st_num(&cur))
                    .on_input(move |v| StudyMsg::Param { chart: c2.clone(), uid, key: key.clone(), value: v })
                    .size(t::s_small())
                    .width(Length::Fixed(110.0))
                    .into()
            }
            ParamKind::Choice => {
                let mut r = row![].spacing(metrics::space(1));
                for ch in &p.choices {
                    r = r.push(chip(*ch, cur.as_str() == Some(ch), StudyMsg::Param { chart: c.clone(), uid, key: p.key.into(), value: (*ch).into() }));
                }
                r.wrap().into()
            }
            ParamKind::Source => {
                // 数据源：K 线字段，或排在前面的指标的输出（study on study）
                let mut r = row![].spacing(metrics::space(1));
                let cur_src = s.source.clone();
                // K 线字段，再是 K 线之外的（主动买卖、衍生品；没有的品种 / 交易所给 NaN，指标那一行会说原因）
                for f in ["close", "open", "high", "low", "hl2", "hlc3", "ohlc4", "volume", "delta", "buy_volume", "oi", "funding", "premium", "ls_ratio", "taker_ratio"] {
                    let on = match &cur_src {
                        None => cur.as_str() == Some(f),
                        Some(StudySrc::Field(x)) => x == f,
                        _ => false,
                    };
                    r = r.push(chip(f, on, StudyMsg::Source { chart: c.clone(), uid, src: Some(StudySrc::Field(f.into())) }));
                }
                for up in &list[..i] {
                    let Some(ud) = up.def() else { continue };
                    for o in &ud.outputs {
                        let src = StudySrc::Study { uid: up.uid, out: o.key.into() };
                        let on = cur_src.as_ref() == Some(&src);
                        r = r.push(chip(format!("{} · {}", up.label(), o.name), on, StudyMsg::Source { chart: c.clone(), uid, src: Some(src) }));
                    }
                }
                r.wrap().into()
            }
        };
        body = body.push(row![text(p.name).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(64.0)), ctl].spacing(metrics::space(1)).align_y(Alignment::Center));
    }
    // 锚点（锚定 VWAP，docs/63 §8.2）：按会话 / 选中画线的起点 / 最近的重要日历事件
    if d.inputs.iter().any(|p| p.key == "anchor_ms") {
        let cur = s.params.get("anchor_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
        let fmt = |t: i64| chrono::DateTime::from_timestamp_millis(t).map(|x| x.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
        let set = |t: i64| StudyMsg::Param { chart: c.clone(), uid, key: "anchor_ms".into(), value: t.to_string() };
        let mut r = row![lab("锚点"), chip("按会话", cur == 0, set(0))].spacing(metrics::space(1));
        if let Some(dw) = super::drawing::selected_drawing(symbol)
            && let Some(a) = dw.anchors.iter().map(|a| a.t).min()
        {
            r = r.push(chip(format!("选中画线的起点 {}", fmt(a as i64)), cur == a as i64, set(a as i64)));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let cal = super::calendar_readout::snapshot();
        let mut evs: Vec<_> = cal.events.iter().filter(|e| e.importance <= 1 && e.scheduled_at.is_some_and(|t| t <= now)).collect();
        evs.sort_by_key(|e| std::cmp::Reverse(e.scheduled_at));
        for e in evs.into_iter().take(6) {
            let t0 = e.scheduled_at.unwrap_or_default();
            r = r.push(chip(format!("{} {}", super::calendar_readout::display_title(e), fmt(t0)), cur == t0, set(t0)));
        }
        body = body.push(r.wrap());
        if cur > 0 {
            body = body.push(t::metadata(format!("从 {} 起累计（锚定，不按会话重置）", fmt(cur))).color(pal::dim()));
        }
    }
    // 在哪个周期上算（I6）：比图大时用已收盘的高周期 K 线算，叠到图上（不前视）
    let mut ctf = row![lab("计算周期")].spacing(metrics::space(1));
    for (l, ms) in [("跟图", 0u64), ("15m", 900_000), ("1h", 3_600_000), ("4h", 14_400_000), ("1d", 86_400_000)] {
        ctf = ctf.push(chip(l, s.tf_ms == ms, StudyMsg::CalcTf { chart: c.clone(), uid, tf: ms }));
    }
    body = body.push(ctf.wrap());
    if s.tf_ms != 0 {
        body = body.push(t::metadata("高周期的值在它收盘之后才出现（不前视）；比图小的周期在这张图上不算").color(pal::dim()));
    }
    // 在哪个品种上算：别的品种用它已收盘的 K 线（后台取），按收盘时刻对齐到这张图上（不前视）
    let draft = ui.symbol_draft.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone());
    let shown_sym = draft.clone().unwrap_or_else(|| s.symbol.rsplit(':').next().unwrap_or_default().to_string());
    let typed = draft.unwrap_or_default();
    body = body.push(
        row![
            lab("品种"),
            chip("本品种", s.symbol.is_empty(), StudyMsg::CalcSymbol { chart: c.clone(), uid, text: String::new() }),
            text_input("别的品种，如 ETHUSDT，回车", &shown_sym)
                .on_input(move |v| StudyMsg::SymbolDraft { uid, text: v })
                .on_submit(StudyMsg::CalcSymbol { chart: c.clone(), uid, text: typed })
                .size(t::s_small())
                .width(Length::Fixed(200.0)),
        ]
        .spacing(metrics::space(1))
        .align_y(Alignment::Center),
    );
    if !s.symbol.is_empty() {
        body = body.push(
            t::metadata(format!(
                "在 {} 上算：只用它已收盘的 K 线，那根收盘后才出现在这张图上（不前视）；K 线由后台每根收盘后取一次。\
                 只写代码 = 与这张图同一交易所；别的交易所写全，如 Bybit Linear:ETHUSDT",
                s.symbol
            ))
            .color(pal::dim()),
        );
    }
    // 对照品种（相关 / 价差这类要第二条序列的）
    if st::study_needs(s).contains(&"aux") && s.id != "breadth" {
        let draft = ui.aux_draft.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone());
        let shown = draft.clone().unwrap_or_else(|| if s.aux_symbol.starts_with('@') { String::new() } else { s.aux_symbol.rsplit(':').next().unwrap_or_default().to_string() });
        let typed = draft.unwrap_or_default();
        body = body.push(
            row![
                lab("对照品种"),
                text_input("如 ETHUSDT，回车", &shown)
                    .on_input(move |v| StudyMsg::AuxDraft { uid, text: v })
                    .on_submit(StudyMsg::AuxSymbol { chart: c.clone(), uid, text: typed })
                    .size(t::s_small())
                    .width(Length::Fixed(200.0)),
            ]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center),
        );
        body = body.push(t::metadata("对照序列 = 那个品种已收盘的收盘价，按收盘时刻对齐（不前视）；只写代码 = 与图同一交易所").color(pal::dim()));
        // 或者：订单流特征 / 图表参数（特征引擎在跑时实时记录，docs/63 §10）
        let q = ui.feat_query.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone()).unwrap_or_default();
        body = body.push(
            row![
                lab("或用特征"),
                text_input("搜特征 / 图表参数（中文名或键）", &q).on_input(move |v| StudyMsg::FeatQuery { uid, text: v }).size(t::s_small()).width(Length::Fixed(220.0)),
            ]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center),
        );
        let ql = q.trim().to_lowercase();
        if !ql.is_empty() {
            let mut r = row![].spacing(metrics::space(1));
            for (id, label) in super::studies_feat::catalog().into_iter().filter(|(id, l)| id.to_lowercase().contains(&ql) || l.to_lowercase().contains(&ql)).take(8) {
                let on = s.aux_symbol == format!("@feature:{id}");
                r = r.push(chip(label, on, StudyMsg::AuxSymbol { chart: c.clone(), uid, text: format!("@feature:{id}") }));
            }
            body = body.push(r.wrap());
        }
        if let Some(f) = s.aux_symbol.strip_prefix("@feature:") {
            body = body.push(t::metadata(format!("对照序列 = 特征 {f}（只读，原样搬；Cockpit 开着、引擎在跑的那段才有值）")).color(pal::dim()));
        }
    }
    body = body.push(t::metadata(format!("数据：{}；更新：每根 K 线（未收盘暂定、收盘确认）；版本 {}", d.data.join("、"), d.version)).color(pal::dim()));
    body.into()
}

/// 样式页：位置（主图 / 副图）+ 每条输出：显示、颜色、画法（不影响计算）。
fn style_tab<'a>(chart: &str, s: &st::Study, d: &wealthspring_indicators::meta::IndicatorDef) -> Element<'a, StudyMsg> {
    let uid = s.uid;
    let c = chart.to_string();
    let mut body = column![
        row![
            lab("位置"),
            chip("主图", s.pane == "overlay", StudyMsg::Pane { chart: c.clone(), uid, pane: "overlay".into() }),
            chip("副图", s.pane == "sub", StudyMsg::Pane { chart: c.clone(), uid, pane: "sub".into() }),
        ]
        .spacing(metrics::space(1))
        .align_y(Alignment::Center)
    ]
    .spacing(metrics::space(2));
    for (k, o) in d.outputs.iter().enumerate() {
        let sty = s.style(o.key);
        let cur_c = st::color_of(s, k, o.key);
        let mut r = row![
            text("■").size(t::s_body()).color(cur_c),
            chip(o.name, sty.visible, StudyMsg::OutVisible { chart: c.clone(), uid, out: o.key.into(), on: !sty.visible }),
        ]
        .spacing(metrics::space(1))
        .align_y(Alignment::Center);
        // 色板（8 个）+ 自动
        r = r.push(chip("自动", sty.color.is_none(), StudyMsg::OutColorSet { chart: c.clone(), uid, out: o.key.into(), color: None }));
        for ci in 0..8 {
            let on = sty.color == Some(ci);
            r = r.push(
                button(text(if on { "◼" } else { "■" }).size(t::s_body()).color(pal::series(ci)))
                    .padding(metrics::pad2(0, 0))
                    .style(move |th, st_| crate::style::button::modifier(th, st_, on))
                    .on_press(StudyMsg::OutColorSet { chart: c.clone(), uid, out: o.key.into(), color: Some(ci) }),
            );
        }
        body = body.push(r.wrap());
        let mut pr = row![Space::new().width(Length::Fixed(64.0)), chip("缺省", sty.plot.is_none(), StudyMsg::OutPlot { chart: c.clone(), uid, out: o.key.into(), plot: None })]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center);
        for (v, n) in PLOTS {
            pr = pr.push(chip(n, sty.plot.as_deref() == Some(v), StudyMsg::OutPlot { chart: c.clone(), uid, out: o.key.into(), plot: Some(v.into()) }));
        }
        body = body.push(pr.wrap());
    }
    body.into()
}

/// 可见性页：显示与否、在哪类周期上显示（与画线 docs/46 同口径；不影响计算）。
fn visibility_tab<'a>(chart: &str, s: &st::Study) -> Element<'a, StudyMsg> {
    let uid = s.uid;
    let c = chart.to_string();
    let mut tfr = row![lab("显示于")].spacing(metrics::space(1));
    for (l, lo, hi) in [("全部周期", 0u64, 0u64), ("日内（≤ 1 小时）", 0, 3_600_000), ("≥ 1 小时", 3_600_000, 0), ("日线及以上", 86_400_000, 0)] {
        tfr = tfr.push(chip(l, s.min_tf_ms == lo && s.max_tf_ms == hi, StudyMsg::TfRange { chart: c.clone(), uid, min: lo, max: hi }));
    }
    column![
        row![lab("显示"), chip(if s.visible { "显示中" } else { "已隐藏" }, s.visible, StudyMsg::Visible { chart: c.clone(), uid, on: !s.visible })]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center),
        tfr.wrap(),
        t::metadata("只管显示；隐藏了照样计算（提醒照判）").color(pal::dim()),
    ]
    .spacing(metrics::space(2))
    .into()
}

/// 指标条件提醒（I5）：输出 × 条件 × 数值 × 周期 × 盘中 / 收盘确认 × 冷却 × 只响一次 × Telegram。只提醒，不下单。
fn alert_rows<'a>(chart: &str, s: &st::Study, d: &wealthspring_indicators::meta::IndicatorDef) -> Element<'a, StudyMsg> {
    let uid = s.uid;
    let op = |o: AlertOp| StudyMsg::Alert { chart: chart.to_string(), uid, op: o };
    let Some(a) = s.alert.as_ref() else {
        return column![
            row![lab("提醒"), small("＋ 加条件提醒", op(AlertOp::Toggle))].spacing(metrics::space(1)).align_y(Alignment::Center),
            t::metadata("只提醒，不下单").color(pal::dim()),
        ]
        .spacing(metrics::space(2))
        .into();
    };
    let mut c = column![].spacing(metrics::space(1));
    c = c.push(
        row![lab("提醒"), chip(if a.armed { "开着" } else { "已关" }, a.armed, op(AlertOp::Armed(!a.armed))), small("删除提醒", op(AlertOp::Toggle))]
            .spacing(metrics::space(1))
            .align_y(Alignment::Center),
    );
    let mut r = row![lab("当")].spacing(metrics::space(1));
    for o in &d.outputs {
        r = r.push(chip(o.name, a.out == o.key, op(AlertOp::Out(o.key.into()))));
    }
    c = c.push(r.wrap());
    let mut r = row![lab("")].spacing(metrics::space(1));
    for (k, n) in st::ALERT_CONDS {
        r = r.push(chip(n, a.cond == k, op(AlertOp::Cond(k.into()))));
    }
    let (c2, uid2) = (chart.to_string(), uid);
    r = r.push(text_input("数值", &a.level_text)
        .on_input(move |v| StudyMsg::Alert { chart: c2.clone(), uid: uid2, op: AlertOp::Level(v) })
        .size(t::s_small())
        .width(Length::Fixed(100.0)));
    c = c.push(r.align_y(Alignment::Center).wrap());
    let mut r = row![lab("周期")].spacing(metrics::space(1));
    for (n, ms) in [("1m", 60_000u64), ("5m", 300_000), ("15m", 900_000), ("1h", 3_600_000), ("4h", 14_400_000), ("1d", 86_400_000)] {
        r = r.push(chip(n, a.tf_ms == ms, op(AlertOp::Tf(ms))));
    }
    r = r.push(chip("收盘确认", !a.intrabar, op(AlertOp::Intrabar(false))));
    r = r.push(chip("盘中", a.intrabar, op(AlertOp::Intrabar(true))));
    c = c.push(r.wrap());
    let mut r = row![lab("冷却")].spacing(metrics::space(1));
    for (n, sec) in [("1 分钟", 60u64), ("5 分钟", 300), ("30 分钟", 1800), ("4 小时", 14_400)] {
        r = r.push(chip(n, a.cooldown_s == sec, op(AlertOp::Cooldown(sec))));
    }
    r = r.push(chip("只响一次", a.once, op(AlertOp::Once(!a.once))));
    r = r.push(chip("也发 Telegram", a.telegram, op(AlertOp::Telegram(!a.telegram))));
    c = c.push(r.wrap());
    c = c.push(
        t::metadata("只提醒，不下单。这张图没显示、或显示的是别的周期时，由后台按这张图现在的品种取 K 线判（出口总闸关着不判）；打开图、开始判时不补报历史")
            .color(pal::dim()),
    );
    c.into()
}

fn st_num(v: &serde_json::Value) -> String {
    match v.as_f64() {
        Some(x) if x.fract() == 0.0 => format!("{x:.0}"),
        Some(x) => format!("{x}"),
        None => v.as_str().unwrap_or_default().to_string(),
    }
}

// ── 图例（T2）──────────────────────────────────────────────────────────────

/// 图例（docs/63 §12.1 T2）：叠在 K 线图上的界面组件。主图左上角每个指标一行（名字 + 参数 + 值，颜色同线；
/// 隐藏的也留一行、变淡）；**鼠标停在那一行才出现** 👁 / ⚙ / ✕；点名字 = 设置；▴ 折成一行。
/// 副图指标的名字行摆在各自副图的左上角，同样可操作。位置与十字线所在那根由画布登记（`studies::set_legend_info`）。
pub fn legend<'a>(chart: &str) -> Option<Element<'a, StudyMsg>> {
    let info = st::legend_info(chart)?;
    let rows = st::legend_rows(chart, info.tf, info.at);
    if rows.is_empty() {
        return None;
    }
    let ui = st::ui_state();
    let hover = ui.hover.as_ref().filter(|h| h.0 == chart).map(|h| h.1);
    let collapsed = ui.legend_collapsed.contains(chart);
    let c = chart.to_string();
    let mut main = column![].spacing(1);
    if collapsed {
        main = main.push(pill(
            button(text(format!("▸ {} 个指标", rows.len())).size(t::s_small()).color(pal::dim()))
                .padding(metrics::pad2(0, 1))
                .style(|th, s| crate::style::button::modifier(th, s, false))
                .on_press(StudyMsg::Legend { chart: c.clone(), collapsed: false })
                .into(),
        ));
    } else {
        for r in rows.iter().filter(|r| !r.sub) {
            main = main.push(legend_row(chart, r, hover == Some(r.uid)));
        }
        main = main.push(
            button(text("▴").size(t::s_meta()).color(pal::dim()))
                .padding(metrics::pad2(0, 1))
                .style(|th, s| crate::style::button::modifier(th, s, false))
                .on_press(StudyMsg::Legend { chart: c.clone(), collapsed: true }),
        );
    }
    let mut layers: Vec<Element<'a, StudyMsg>> =
        // 各层铺满（Stack 按第一层定大小）；空白处不接鼠标，图照常拖、照常十字线
        vec![container(main).padding(iced::Padding { top: info.top, left: 6.0, ..iced::Padding::ZERO }).width(Length::Fill).height(Length::Fill).into()];
    // 副图的名字行：各自副图的左上角
    let subs: Vec<&st::LegendRow> = rows.iter().filter(|r| r.sub).collect();
    for (r, (top, _)) in subs.iter().zip(st::sub_slots(subs.len(), info.sub_bottom)) {
        layers.push(
            container(legend_row(chart, r, hover == Some(r.uid)))
                .padding(iced::Padding { top: top + 1.0, left: 4.0, ..iced::Padding::ZERO })
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        );
    }
    Some(iced::widget::Stack::with_children(layers).into())
}

/// 半透明底板（读数压在 K 线上也看得清）。
fn pill<'a>(e: Element<'a, StudyMsg>) -> Element<'a, StudyMsg> {
    container(e)
        .padding(metrics::pad2(0, 1))
        .style(|_| container::Style {
            background: Some(iced::Background::Color(pal::alpha(pal::card_bg(), 0.72))),
            border: iced::Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn legend_row<'a>(chart: &str, r: &st::LegendRow, hovered: bool) -> Element<'a, StudyMsg> {
    let c = chart.to_string();
    let uid = r.uid;
    let name_c = if r.shown { r.color } else { pal::alpha(pal::dim(), 0.7) };
    let mut line = row![
        button(text(r.label.clone()).size(t::s_small()).color(name_c))
            .padding(0)
            .style(|th, s| crate::style::button::modifier(th, s, false))
            .on_press(StudyMsg::Edit(Some((c.clone(), uid)))),
    ]
    .spacing(metrics::space(2))
    .align_y(Alignment::Center);
    for (v, col) in &r.values {
        line = line.push(text(v.clone()).size(t::s_small()).color(*col).font(crate::ui::text::mono_font()));
    }
    if !r.visible {
        line = line.push(text("已隐藏").size(t::s_meta()).color(pal::dim()));
    } else if !r.shown {
        line = line.push(text("这个周期不显示").size(t::s_meta()).color(pal::dim()));
    }
    if r.alert {
        line = line.push(text("🔔").size(t::s_meta()));
    }
    if let Some(e) = &r.error {
        line = line.push(tooltip(
            text("⚠").size(t::s_small()).color(pal::warn()),
            container(t::caption(e.clone())).padding(metrics::space(2)).max_width(360.0).style(crate::style::tooltip),
            tooltip::Position::Bottom,
        ));
    }
    // 悬停才出现的三个图标（平时只有文字，主区减负）
    if hovered {
        line = line.push(icon(if r.visible { "👁" } else { "◌" }, if r.visible { "隐藏" } else { "显示" }, StudyMsg::Visible { chart: c.clone(), uid, on: !r.visible }));
        line = line.push(icon("⚙", "设置", StudyMsg::Edit(Some((c.clone(), uid)))));
        line = line.push(icon("✕", "删除", StudyMsg::Remove { chart: c.clone(), uid }));
    }
    iced::widget::mouse_area(pill(line.into()))
        .on_enter(StudyMsg::Hover { chart: c.clone(), uid, on: true })
        .on_exit(StudyMsg::Hover { chart: c, uid, on: false })
        .into()
}
