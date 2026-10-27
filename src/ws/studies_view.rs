//! 检查器「属性」页里 K 线图的「指标」一节（docs/63 I3 + §8.2）：指标浏览器（搜索 / 按用途 / 分类 / 收藏 / 最近用过）、
//! 这个品种的指标对象列表（显示、计算参数、各输出的颜色与显示、主图 / 副图、数据源、顺序、删除）、模板。
//! 主区不加常驻控件（docs/42）。

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Length};

use wealthspring_indicators::meta::ParamKind;

use super::studies::{self as st, AlertOp, StudyMsg, StudySrc};
use crate::ui::{metrics, pal, text as t};

const CATEGORIES: [(&str, &str); 10] = [("trend", "趋势"), ("momentum", "动量"), ("volatility", "波动"), ("volume", "成交量"),
                                       ("vwap", "VWAP"), ("structure", "结构"), ("statistics", "统计"), ("derivatives", "衍生品"), ("breadth", "广度"),
                                       ("custom", "自定义")];

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

/// `symbol`：图的品种键（`Binance Linear:BTCUSDT`）。
pub fn inspector<'a>(symbol: &str) -> Element<'a, StudyMsg> {
    let sym = symbol.to_string();
    let store = st::snapshot();
    let ui = st::ui_state();
    let list = store.by_symbol.get(symbol).cloned().unwrap_or_default();
    let mut b = column![crate::ui::widgets::section("指标")].spacing(metrics::space(2));

    // ── 浏览器 ──
    b = b.push(text_input("搜索名字或用途（趋势强度、动态止损、超买超卖……）", &ui.search).on_input(StudyMsg::Search).size(t::s_small()));
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
    let rank = |id: &str| if store.favorites.iter().any(|x| x == id) { 0 } else if store.recent.iter().any(|x| x == id) { 1 } else { 2 };
    shown.sort_by_key(|d| rank(d.id));
    let mut lib = column![].spacing(1);
    for d in shown.iter().take(if q.is_empty() && ui.category.is_none() { 8 } else { 30 }) {
        let fav = store.favorites.iter().any(|x| x == d.id);
        let pane = if d.pane == "overlay" { "主图" } else { "副图" };
        lib = lib.push(
            row![
                small(if fav { "★" } else { "☆" }, StudyMsg::Favorite(d.id.to_string())),
                text(format!("{}（{}）", d.name, pane)).size(t::s_small()).width(Length::Fill),
                text(d.tags.join("、")).size(t::s_meta()).color(pal::dim()),
                small("＋ 加", StudyMsg::Add { symbol: sym.clone(), id: d.id.to_string() }),
            ]
            .spacing(metrics::space(1))
            .align_y(iced::Alignment::Center),
        );
    }
    b = b.push(lib);
    for (f, e) in st::user_errors() {
        b = b.push(text(format!("自定义指标 {f} 读不了：{e}")).size(t::s_meta()).color(pal::warn()));
    }
    if q.is_empty() && ui.category.is_none() {
        b = b.push(t::metadata("只列了收藏 / 最近用过 / 前几个；搜索或选分类看全部").color(pal::dim()));
    }

    // ── 这个品种的指标 ──
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_string();
    b = b.push(text(format!("{code} 上的指标（同一品种的所有图、所有周期共用）")).size(t::s_meta()).color(pal::dim()));
    if list.is_empty() {
        b = b.push(t::metadata("还没有：在上面点「＋ 加」").color(pal::dim()));
    }
    if let Some(t0) = st::recomputed_from(symbol) {
        let when = chrono::DateTime::from_timestamp_millis(t0 as i64)
            .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        b = b.push(t::metadata(format!("数据修正过：从 {when} 那根起重算（下游指标跟着重算）")).color(pal::dim()));
    }
    for (i, s) in list.iter().enumerate() {
        let Some(d) = s.def() else { continue };
        let uid = s.uid;
        let open = ui.expanded == Some(uid);
        let mut head = row![
            chip(if s.visible { "显示" } else { "隐藏" }, s.visible, StudyMsg::Visible { symbol: sym.clone(), uid, on: !s.visible }),
            button(text(format!("{}{}", if open { "▾ " } else { "▸ " }, s.label())).size(t::s_small()))
                .padding(metrics::pad2(0, 1))
                .style(|th, st_| crate::style::button::modifier(th, st_, false))
                .on_press(StudyMsg::Expand(if open { None } else { Some(uid) })),
        ]
        .spacing(metrics::space(1))
        .align_y(iced::Alignment::Center);
        if let Some(Some(e)) = st::errors_for(symbol).get(i) {
            head = head.push(text(e.clone()).size(t::s_meta()).color(pal::warn()));
        }
        b = b.push(head.wrap());
        if !open {
            continue;
        }
        let mut body = column![].spacing(metrics::space(1)).padding(metrics::pad2(0, 6));
        body = body.push(text(d.description).size(t::s_meta()).color(pal::dim()));
        body = body.push(text(format!("数据：{}；更新：每根 K 线（未收盘暂定、收盘确认）；版本 {}", d.data.join("、"), d.version)).size(t::s_meta()).color(pal::dim()));
        // 位置
        body = body.push(
            row![
                text("位置").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)),
                chip("主图", s.pane == "overlay", StudyMsg::Pane { symbol: sym.clone(), uid, pane: "overlay".into() }),
                chip("副图", s.pane == "sub", StudyMsg::Pane { symbol: sym.clone(), uid, pane: "sub".into() }),
                small("↑", StudyMsg::Move { symbol: sym.clone(), uid, up: true }),
                small("↓", StudyMsg::Move { symbol: sym.clone(), uid, up: false }),
                small("复制", StudyMsg::Duplicate { symbol: sym.clone(), uid }),
                small("删除", StudyMsg::Remove { symbol: sym.clone(), uid }),
            ]
            .spacing(metrics::space(1))
            .align_y(iced::Alignment::Center),
        );
        // 在哪类周期上显示（与画线 docs/46 同口径；不影响计算）
        let mut tfr = row![text("显示于").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0))].spacing(metrics::space(1));
        for (lab, lo, hi) in [("全部周期", 0u64, 0u64), ("日内（≤ 1 小时）", 0, 3_600_000), ("≥ 1 小时", 3_600_000, 0), ("日线及以上", 86_400_000, 0)] {
            tfr = tfr.push(chip(lab, s.min_tf_ms == lo && s.max_tf_ms == hi, StudyMsg::TfRange { symbol: sym.clone(), uid, min: lo, max: hi }));
        }
        body = body.push(tfr.wrap());
        // 在哪个周期上算（I6）：比图大时用已收盘的高周期 K 线算，叠到图上（不前视）
        let mut ctf = row![text("计算周期").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0))].spacing(metrics::space(1));
        for (lab, ms) in [("跟图", 0u64), ("15m", 900_000), ("1h", 3_600_000), ("4h", 14_400_000), ("1d", 86_400_000)] {
            ctf = ctf.push(chip(lab, s.tf_ms == ms, StudyMsg::CalcTf { symbol: sym.clone(), uid, tf: ms }));
        }
        body = body.push(ctf.wrap());
        if s.tf_ms != 0 {
            body = body.push(t::metadata("高周期的值在它收盘之后才出现（不前视）；比图小的周期在这张图上不算").color(pal::dim()));
        }
        // 在哪个品种上算：别的品种用它已收盘的 K 线（后台取），按收盘时刻对齐到这张图上（不前视）
        let draft = ui.symbol_draft.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone());
        let shown_sym = draft.clone().unwrap_or_else(|| s.symbol.rsplit(':').next().unwrap_or_default().to_string());
        let sym2 = sym.clone();
        let typed = draft.unwrap_or_default();
        body = body.push(
            row![
                text("品种").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)),
                chip("本品种", s.symbol.is_empty(), StudyMsg::CalcSymbol { symbol: sym.clone(), uid, text: String::new() }),
                text_input("别的品种，如 ETHUSDT，回车", &shown_sym)
                    .on_input(move |v| StudyMsg::SymbolDraft { uid, text: v })
                    .on_submit(StudyMsg::CalcSymbol { symbol: sym2, uid, text: typed })
                    .size(t::s_small())
                    .width(Length::Fixed(200.0)),
            ]
            .spacing(metrics::space(1))
            .align_y(iced::Alignment::Center),
        );
        // 对照品种（相关 / 价差这类要第二条序列的）
        if st::study_needs(s).contains(&"aux") && s.id != "breadth" {
            let draft = ui.aux_draft.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone());
            let shown = draft.clone().unwrap_or_else(|| if s.aux_symbol.starts_with('@') { String::new() } else { s.aux_symbol.rsplit(':').next().unwrap_or_default().to_string() });
            let typed = draft.unwrap_or_default();
            let sym4 = sym.clone();
            body = body.push(
                row![
                    text("对照品种").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)),
                    text_input("如 ETHUSDT，回车", &shown)
                        .on_input(move |v| StudyMsg::AuxDraft { uid, text: v })
                        .on_submit(StudyMsg::AuxSymbol { symbol: sym4, uid, text: typed })
                        .size(t::s_small())
                        .width(Length::Fixed(200.0)),
                ]
                .spacing(metrics::space(1))
                .align_y(iced::Alignment::Center),
            );
            body = body.push(t::metadata("对照序列 = 那个品种已收盘的收盘价，按收盘时刻对齐（不前视）；只写代码 = 与图同一交易所").color(pal::dim()));
            // 或者：订单流特征 / 图表参数（特征引擎在跑时实时记录，docs/63 §10）
            let q = ui.feat_query.as_ref().filter(|d| d.0 == uid).map(|d| d.1.clone()).unwrap_or_default();
            body = body.push(
                row![
                    text("或用特征").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)),
                    text_input("搜特征 / 图表参数（中文名或键）", &q).on_input(move |v| StudyMsg::FeatQuery { uid, text: v }).size(t::s_small()).width(Length::Fixed(220.0)),
                ]
                .spacing(metrics::space(1))
                .align_y(iced::Alignment::Center),
            );
            let ql = q.trim().to_lowercase();
            if !ql.is_empty() {
                let mut r = row![].spacing(metrics::space(1));
                for (id, label) in super::studies_feat::catalog().into_iter().filter(|(id, l)| id.to_lowercase().contains(&ql) || l.to_lowercase().contains(&ql)).take(8) {
                    let on = s.aux_symbol == format!("@feature:{id}");
                    r = r.push(chip(label, on, StudyMsg::AuxSymbol { symbol: sym.clone(), uid, text: format!("@feature:{id}") }));
                }
                body = body.push(r.wrap());
            }
            if let Some(f) = s.aux_symbol.strip_prefix("@feature:") {
                body = body.push(t::metadata(format!("对照序列 = 特征 {f}（只读，原样搬；Cockpit 开着、引擎在跑的那段才有值）")).color(pal::dim()));
            }
        }
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
        // 计算参数
        for p in &d.inputs {
            let cur = s.params.get(p.key).unwrap_or(&p.default).clone();
            let ctl: Element<'a, StudyMsg> = match p.kind {
                ParamKind::Int | ParamKind::Float => {
                    let (sym2, key) = (sym.clone(), p.key.to_string());
                    text_input("", &st_num(&cur))
                        .on_input(move |v| StudyMsg::Param { symbol: sym2.clone(), uid, key: key.clone(), value: v })
                        .size(t::s_small())
                        .width(Length::Fixed(110.0))
                        .into()
                }
                ParamKind::Choice => {
                    let mut r = row![].spacing(metrics::space(1));
                    for c in &p.choices {
                        r = r.push(chip(*c, cur.as_str() == Some(c), StudyMsg::Param { symbol: sym.clone(), uid, key: p.key.into(), value: (*c).into() }));
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
                        r = r.push(chip(f, on, StudyMsg::Source { symbol: sym.clone(), uid, src: Some(StudySrc::Field(f.into())) }));
                    }
                    for up in &list[..i] {
                        let Some(ud) = up.def() else { continue };
                        for o in &ud.outputs {
                            let src = StudySrc::Study { uid: up.uid, out: o.key.into() };
                            let on = cur_src.as_ref() == Some(&src);
                            r = r.push(chip(format!("{} · {}", up.label(), o.name), on, StudyMsg::Source { symbol: sym.clone(), uid, src: Some(src) }));
                        }
                    }
                    r.wrap().into()
                }
            };
            body = body.push(row![text(p.name).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)), ctl].spacing(metrics::space(1)).align_y(iced::Alignment::Center));
        }
        // 锚点（锚定 VWAP，docs/63 §8.2）：按会话 / 选中画线的起点 / 最近的重要日历事件
        if d.inputs.iter().any(|p| p.key == "anchor_ms") {
            let cur = s.params.get("anchor_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
            let fmt = |t: i64| chrono::DateTime::from_timestamp_millis(t).map(|x| x.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
            let set = |t: i64| StudyMsg::Param { symbol: sym.clone(), uid, key: "anchor_ms".into(), value: t.to_string() };
            let mut r = row![text("锚点").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)), chip("按会话", cur == 0, set(0))].spacing(metrics::space(1));
            if let Some(dw) = super::drawing::selected_drawing(&sym)
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
        // 各输出：显示与颜色（不影响计算）
        let mut outs = row![].spacing(metrics::space(2));
        for (k, o) in d.outputs.iter().enumerate() {
            let sty = s.style(o.key);
            let c = pal::series(sty.color.unwrap_or(k + (uid as usize % 5)));
            outs = outs.push(
                row![
                    button(text("■").size(t::s_small()).color(c)).padding(metrics::pad2(0, 0)).style(|th, st_| crate::style::button::modifier(th, st_, false))
                        .on_press(StudyMsg::OutColor { symbol: sym.clone(), uid, out: o.key.into() }),
                    chip(o.name, sty.visible, StudyMsg::OutVisible { symbol: sym.clone(), uid, out: o.key.into(), on: !sty.visible }),
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
            );
        }
        body = body.push(row![text("输出").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0)), outs.wrap()].spacing(metrics::space(1)));
        body = body.push(alert_rows(&sym, s, &d));
        b = b.push(body);
    }

    // ── 模板 ──
    b = b.push(text("模板：把这个品种上的指标存下来，换品种一键套用").size(t::s_meta()).color(pal::dim()));
    b = b.push(
        row![
            text_input("模板名", &ui.template_name).on_input(StudyMsg::TemplateName).size(t::s_small()).width(Length::Fixed(160.0)),
            small("存为模板", StudyMsg::SaveTemplate { symbol: sym.clone(), name: ui.template_name.clone() }),
        ]
        .spacing(metrics::space(1))
        .align_y(iced::Alignment::Center),
    );
    let mut tl = row![].spacing(metrics::space(2));
    for name in store.templates.keys() {
        tl = tl.push(
            row![
                small(format!("套用「{name}」"), StudyMsg::ApplyTemplate { symbol: sym.clone(), name: name.clone() }),
                small("×", StudyMsg::DeleteTemplate(name.clone())),
            ]
            .spacing(1),
        );
    }
    b = b.push(tl.wrap());
    if !ui.note.is_empty() {
        b = b.push(text(ui.note).size(t::s_meta()).color(pal::warn()));
    }
    b.width(Length::Fill).into()
}

/// 指标条件提醒（I5）：输出 × 条件 × 数值 × 周期 × 盘中 / 收盘确认 × 冷却 × 只响一次 × Telegram。只提醒，不下单。
fn alert_rows<'a>(sym: &str, s: &st::Study, d: &wealthspring_indicators::meta::IndicatorDef) -> Element<'a, StudyMsg> {
    let uid = s.uid;
    let op = |o: AlertOp| StudyMsg::Alert { symbol: sym.to_string(), uid, op: o };
    let lab = |x: &'static str| text(x).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(60.0));
    let Some(a) = s.alert.as_ref() else {
        return row![lab("提醒"), small("＋ 加条件提醒", op(AlertOp::Toggle))].spacing(metrics::space(1)).align_y(iced::Alignment::Center).into();
    };
    let mut c = column![].spacing(metrics::space(1));
    c = c.push(
        row![
            lab("提醒"),
            chip(if a.armed { "开着" } else { "已关" }, a.armed, op(AlertOp::Armed(!a.armed))),
            small("删除提醒", op(AlertOp::Toggle)),
        ]
        .spacing(metrics::space(1))
        .align_y(iced::Alignment::Center),
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
    let (sym2, uid2) = (sym.to_string(), uid);
    r = r.push(text_input("数值", &a.level_text)
        .on_input(move |v| StudyMsg::Alert { symbol: sym2.clone(), uid: uid2, op: AlertOp::Level(v) })
        .size(t::s_small())
        .width(Length::Fixed(100.0)));
    c = c.push(r.align_y(iced::Alignment::Center).wrap());
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
    c = c.push(t::metadata("只提醒，不下单。没开着这个品种这个周期的图时由后台取 K 线判（出口总闸关着不判）；打开图、开始判时不补报历史").color(pal::dim()));
    c.into()
}

fn st_num(v: &serde_json::Value) -> String {
    match v.as_f64() {
        Some(x) if x.fract() == 0.0 => format!("{x:.0}"),
        Some(x) => format!("{x}"),
        None => v.as_str().unwrap_or_default().to_string(),
    }
}
