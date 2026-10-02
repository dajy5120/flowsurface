//! 共用数据选择组件的渲染（状态、扫描与规则在 [`super::data_picker`]）。
//!
//! 自上而下一级一行：管线 → 来源（管线 B）→ 交易所 / 数据商 / 提供方 → 根目录与扫描（本地）→ 市场 → 标的 → 时间 → 数据类型；
//! 最后一行说明当前选到了什么。宿主用 `.map(...)` 把 [`DataPickMsg`] 包进自己的消息。

use iced::widget::{button, column, pick_list, row, text, text_input, tooltip};
use iced::{Color, Element, Length};

use super::data_picker::{
    self as dp, market_label, BSource, DataPick, DataPickMsg, Load, PickOpts, Pipeline, Purpose, TimeMode,
};

type El<'a> = Element<'a, DataPickMsg>;

/// 下拉框的一项：键 + 显示名。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    key: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn label<'a>(t: &str) -> El<'a> {
    iced::widget::container(text(t.to_string()).size(crate::ui::text::s_small()).color(crate::ui::pal::head()))
        .width(Length::Fixed(64.0))
        .into()
}

fn dim<'a>(t: String) -> El<'a> {
    text(t).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()).into()
}

fn chip<'a>(t: String, active: bool, msg: Option<DataPickMsg>) -> El<'a> {
    let b = button(text(t).size(crate::ui::text::s_small()))
        .padding(crate::ui::metrics::pad2(0, 3))
        .style(move |th, st| crate::style::button::modifier(th, st, active));
    match msg {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

fn with_tip<'a>(e: El<'a>, tip: String) -> El<'a> {
    tooltip(
        e,
        iced::widget::container(text(tip).size(crate::ui::text::s_small())).style(crate::style::tooltip).padding(crate::ui::metrics::space(2)),
        tooltip::Position::Bottom,
    )
    .into()
}

fn pick<'a>(
    options: Vec<Choice>,
    selected: Option<&str>,
    placeholder: String,
    width: f32,
    on: impl Fn(String) -> DataPickMsg + 'a,
) -> El<'a> {
    let sel = selected.and_then(|k| options.iter().find(|c| c.key == k).cloned());
    pick_list(options, sel, move |c: Choice| on(c.key))
        .placeholder(placeholder)
        .text_size(11)
        .padding(crate::ui::metrics::pad2(0, 2))
        .width(Length::Fixed(width))
        .into()
}

fn line<'a>(items: Vec<El<'a>>) -> El<'a> {
    let mut r = row![].spacing(6).align_y(iced::Alignment::Center);
    for i in items {
        r = r.push(i);
    }
    r.wrap().into()
}

/// 标的：多于 8 个时带搜索框。
fn symbol_row<'a>(p: &DataPick, all: usize, items: Vec<Choice>) -> El<'a> {
    let mut v = vec![label("标的")];
    if all > 8 {
        v.push(
            text_input("搜索代码", &p.search)
                .on_input(DataPickMsg::Search)
                .size(crate::ui::text::s_small())
                .width(Length::Fixed(90.0))
                .into(),
        );
    }
    let n = items.len();
    v.push(pick(items, p.symbol.as_deref(), format!("选标的（{n}）"), 170.0, DataPickMsg::Symbol));
    line(v)
}

/// 组件本体。
pub fn view<'a>(p: &DataPick, opts: &PickOpts) -> El<'a> {
    let mut b = column![].spacing(5);
    let mut warn: Option<(String, Color)> = None;

    // ① 管线
    let mut v = vec![label("管线")];
    for pl in Pipeline::ALL {
        let blocked = pl == Pipeline::A && (opts.purpose == Purpose::Compute || opts.local_only);
        let c = chip(pl.label().into(), p.pipeline == Some(pl), (!blocked).then_some(DataPickMsg::Pipeline(pl)));
        v.push(if blocked {
            with_tip(
                c,
                if opts.local_only {
                    "本面板只读本地历史文件：选管线 B 的购买数据 / 本地录制".into()
                } else {
                    "管线 A 只喂图表：它的数据不得进入特征、策略、回测与结果（docs/28 §1.2）".into()
                },
            )
        } else {
            c
        });
    }
    b = b.push(line(v));

    match p.pipeline {
        None => {}
        // ── 管线 A：flowsurface 原生交易所 ──
        Some(Pipeline::A) => {
            let venues: Vec<Choice> = dp::NATIVE_VENUES.iter().map(|x| Choice { key: (*x).into(), label: (*x).into() }).collect();
            b = b.push(line(vec![
                label("交易所"),
                pick(venues, p.venue.as_deref(), "选交易所".into(), 140.0, DataPickMsg::Venue),
            ]));
            if let Some(venue) = p.venue.as_deref() {
                b = b.push(native_rows(p, venue, &mut warn));
            }
        }
        // ── 管线 B ──
        Some(Pipeline::B) => {
            let mut v = vec![label("来源")];
            for s in BSource::ALL {
                if opts.sources.as_ref().is_some_and(|only| !only.contains(&s)) {
                    continue;
                }
                v.push(chip(s.label().into(), p.source == Some(s), Some(DataPickMsg::Source(s))));
            }
            b = b.push(line(v));
            match p.source {
                None => {}
                Some(BSource::Live) => {
                    let venues: Vec<Choice> = dp::LIVE_VENUES
                        .iter()
                        .map(|(x, ok)| Choice {
                            key: (*x).into(),
                            label: if *ok { format!("{x}（已接）") } else { format!("{x}（Nautilus 有适配器，本仓未接）") },
                        })
                        .collect();
                    b = b.push(line(vec![
                        label("交易所"),
                        pick(venues, p.venue.as_deref(), "选交易所".into(), 260.0, DataPickMsg::Venue),
                    ]));
                    if let Some(venue) = p.venue.as_deref() {
                        if dp::LIVE_VENUES.iter().any(|(x, ok)| *x == venue && !*ok) {
                            warn = Some((format!("{venue} 的实时还没接进管线 B（Nautilus 有现成适配器，需要配置后才能用）"), crate::ui::pal::warn()));
                        }
                        b = b.push(native_rows(p, venue, &mut warn));
                    }
                }
                Some(BSource::ThirdParty) => {
                    let provs: Vec<Choice> = dp::PROVIDERS
                        .iter()
                        .map(|(k, name, _, note, ok)| Choice {
                            key: (*k).into(),
                            label: format!("{name}{}（{note}）", if *ok { "" } else { " · 未接" }),
                        })
                        .collect();
                    b = b.push(line(vec![
                        label("提供方"),
                        pick(provs, p.vendor.as_deref(), "选第三方 API".into(), 360.0, DataPickMsg::Vendor),
                    ]));
                    if let Some(pv) = dp::PROVIDERS.iter().find(|x| Some(x.0) == p.vendor.as_deref()) {
                        if !pv.4 {
                            warn = Some((format!("{} 还没接", pv.1), crate::ui::pal::warn()));
                        }
                        b = b.push(line(vec![label("市场"), text(market_label(pv.2)).size(crate::ui::text::s_small()).into()]));
                        b = b.push(line(vec![
                            label("标的"),
                            text_input("手填代码 / 市场 slug", p.symbol.as_deref().unwrap_or(""))
                                .on_input(DataPickMsg::Symbol)
                                .size(crate::ui::text::s_small())
                                .width(Length::Fixed(220.0))
                                .into(),
                            dim("第三方 API 的标的由提供方定义，这里手填".into()),
                        ]));
                        b = b.push(line(vec![label("时间"), text("实时").size(crate::ui::text::s_small()).color(crate::ui::pal::ok()).into()]));
                    }
                }
                Some(s) => {
                    // B2 / B3：本地目录
                    if s == BSource::Purchased {
                        let mut v = vec![label("数据商")];
                        for (k, name) in dp::VENDORS {
                            if opts.vendors.as_ref().is_some_and(|only| !only.contains(&k)) {
                                continue;
                            }
                            v.push(chip(name.into(), p.vendor.as_deref() == Some(k), Some(DataPickMsg::Vendor(k.into()))));
                        }
                        v.push(dim("两家各走自己的接口读原始文件，不互相转换格式".into()));
                        b = b.push(line(v));
                    }
                    if p.local_key().is_some() {
                        b = b.push(local_rows(p, opts, &mut warn));
                    }
                }
            }
        }
    }

    // 说明行
    let (t, c) = if let Some(w) = warn {
        w
    } else if !p.note.is_empty() {
        (p.note.clone(), crate::ui::pal::dim())
    } else if let Some(s) = p.selection(opts) {
        (format!("已选：{}", s.describe()), crate::ui::pal::ok())
    } else {
        ("依次选：管线 → 来源 → 市场 → 标的 → 时间".into(), crate::ui::pal::dim())
    };
    b.push(text(t).size(crate::ui::text::s_meta()).color(c)).into()
}

/// 管线 A / B1：交易所的市场与交易对（取 flowsurface 已拉到的列表）。
fn native_rows<'a>(p: &DataPick, venue: &str, warn: &mut Option<(String, Color)>) -> El<'a> {
    let mut b = column![].spacing(5);
    let markets = dp::native_markets(venue);
    if markets.is_empty() {
        *warn = Some((
            format!("{venue} 的交易对列表还没拉到：打开任一行情面板的标的列表（它会向交易所拉取），或直接手填代码"),
            crate::ui::pal::warn(),
        ));
        let mks: Vec<Choice> = ["linear", "spot", "inverse"].iter().map(|m| Choice { key: (*m).into(), label: market_label(m) }).collect();
        b = b.push(line(vec![label("市场"), pick(mks, p.market.as_deref(), "选市场".into(), 130.0, DataPickMsg::Market)]));
        if p.market.is_some() {
            b = b.push(line(vec![
                label("标的"),
                text_input("手填代码，如 BTCUSDT", p.symbol.as_deref().unwrap_or(""))
                    .on_input(DataPickMsg::Symbol)
                    .size(crate::ui::text::s_small())
                    .width(Length::Fixed(170.0))
                    .into(),
            ]));
        }
    } else {
        let mks: Vec<Choice> = markets.iter().map(|m| Choice { key: m.clone(), label: market_label(m) }).collect();
        b = b.push(line(vec![label("市场"), pick(mks, p.market.as_deref(), "选市场".into(), 130.0, DataPickMsg::Market)]));
        if let Some(mk) = p.market.as_deref() {
            let all = dp::native_symbols(venue, mk, "").len();
            let items: Vec<Choice> = dp::native_symbols(venue, mk, &p.search)
                .into_iter()
                .map(|s| Choice { key: s.clone(), label: s })
                .collect();
            b = b.push(symbol_row(p, all, items));
        }
    }
    if p.symbol.is_some() {
        b = b.push(line(vec![label("时间"), text("实时").size(crate::ui::text::s_small()).color(crate::ui::pal::ok()).into()]));
    }
    b.into()
}

/// B2 / B3：根目录、扫描、市场 / 标的 / 日期 / 时段 / 数据类型。
fn local_rows<'a>(p: &DataPick, opts: &PickOpts, warn: &mut Option<(String, Color)>) -> El<'a> {
    let mut b = column![].spacing(5);
    let root = p.root_or_default();
    b = b.push(line(vec![
        label("根目录"),
        text_input("数据根目录", &p.root)
            .on_input(DataPickMsg::Root)
            .size(crate::ui::text::s_small())
            .width(Length::Fixed(380.0))
            .into(),
        chip("选择目录…".into(), false, Some(DataPickMsg::BrowseRoot)),
        chip("↻ 重新扫描".into(), false, Some(DataPickMsg::Rescan)),
    ]));
    let Some(scan) = p.local_scan() else {
        return b.into();
    };
    let sc = match scan {
        Load::Loading => {
            b = b.push(line(vec![label(""), dim(format!("正在扫描 {root} …"))]));
            return b.into();
        }
        Load::Failed(e) => {
            *warn = Some((format!("扫描失败：{e}"), crate::ui::pal::bad()));
            return b.into();
        }
        Load::Ready(s) => s,
    };
    if !sc.exists {
        *warn = Some((format!("目录不存在：{}", sc.root), crate::ui::pal::bad()));
        return b.into();
    }
    let n_sym = {
        let mut v: Vec<&str> = sc.items.iter().map(|i| i.symbol.as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v.len()
    };
    let mut all_types: Vec<&str> = sc.items.iter().flat_map(|i| i.types.iter().map(String::as_str)).collect();
    all_types.sort_unstable();
    all_types.dedup();
    b = b.push(line(vec![
        label(""),
        dim(format!(
            "扫到 {} 个市场、{n_sym} 个标的、{} 个「标的 × 日」；数据类型：{}",
            sc.markets().len(),
            sc.items.len(),
            if all_types.is_empty() { "—".to_string() } else { all_types.join(" · ") }
        )),
    ]));
    if sc.items.is_empty() {
        *warn = Some(("这个目录下没扫到认得的数据（目录结构要与该数据商一致）".into(), crate::ui::pal::warn()));
        return b.into();
    }

    // 市场
    let mks: Vec<Choice> = sc.markets().into_iter().map(|m| Choice { label: market_label(&m), key: m }).collect();
    b = b.push(line(vec![label("市场"), pick(mks, p.market.as_deref(), "选市场".into(), 130.0, DataPickMsg::Market)]));
    let Some(mk) = p.market.as_deref() else {
        return b.into();
    };

    // 标的
    let all = sc.symbols(mk, "").len();
    let items: Vec<Choice> = sc
        .symbols(mk, &p.search)
        .into_iter()
        .map(|(s, n)| Choice { label: if n > 1 { format!("{s} · {n} 天") } else { s.clone() }, key: s })
        .collect();
    b = b.push(symbol_row(p, all, items));
    let Some(sym) = p.symbol.as_deref() else {
        return b.into();
    };

    // 时间：日期（+ 起始时刻 / 时长）
    let dates = sc.dates(sym);
    let latest = dates.last().cloned();
    let n = dates.len();
    let dchoices: Vec<Choice> = dates.into_iter().rev().map(|d| Choice { key: d.clone(), label: d }).collect();
    let mut v = vec![label("时间"), pick(dchoices, p.date.as_deref(), format!("选日期（{n}）"), 130.0, DataPickMsg::Date)];
    if let Some(d) = latest
        && p.date.as_deref() != Some(d.as_str())
    {
        v.push(chip("最新".into(), false, Some(DataPickMsg::Date(d))));
    }
    if opts.time == TimeMode::Window {
        v.push(dim("起始（UTC）".into()));
        v.push(
            text_input("HH:MM", &p.start)
                .on_input(DataPickMsg::Start)
                .size(crate::ui::text::s_small())
                .width(Length::Fixed(60.0))
                .into(),
        );
        v.push(dim("时长".into()));
        let mins: Vec<Choice> = [1u32, 5, 10, 15, 30, 60, 120, 240, 390, 1440]
            .iter()
            .map(|m| Choice {
                key: m.to_string(),
                label: if *m >= 60 { format!("{} 小时", f64::from(*m) / 60.0) } else { format!("{m} 分钟") },
            })
            .collect();
        v.push(pick(mins, Some(&p.minutes.to_string()), "时长".into(), 100.0, |s| {
            DataPickMsg::Minutes(s.parse().unwrap_or(30))
        }));
    }
    b = b.push(line(v));
    // 常用时段 + 换算后的绝对范围（UTC 与北京时间）
    if opts.time == TimeMode::Window {
        b = b.push(crate::ui::widgets::time_range(p.date.as_deref(), &p.start, p.minutes, DataPickMsg::Window));
    }

    // 数据类型：当天有的全部列出，可勾选；没选日期时列出该标的出现过的类型（各几天）
    if opts.hide_types {
        return b.into();
    }
    match p.date.as_deref() {
        Some(d) => {
            let types = sc.types(sym, d);
            let mut v = vec![label("数据类型")];
            for t in &types {
                let on = !p.excluded.contains(t);
                v.push(chip(format!("{} {t}", if on { "☑" } else { "☐" }), on, Some(DataPickMsg::ToggleType(t.clone()))));
            }
            if types.iter().all(|t| p.excluded.contains(t)) {
                *warn = Some(("一类数据都没勾".into(), crate::ui::pal::warn()));
            }
            b = b.push(line(v));
        }
        None => {
            let td = sc.type_days(sym);
            b = b.push(line(vec![
                label("数据类型"),
                dim(td.iter().map(|(t, n)| format!("{t}（{n} 天）")).collect::<Vec<_>>().join(" · ")),
            ]));
        }
    }
    b.into()
}
