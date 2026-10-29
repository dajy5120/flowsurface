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
    let b = button(crate::ui::widgets::sym_label(&t, crate::ui::text::s_small(), None))
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
                            // 宿主要的数据这家根本给不了：置灰并说明（docs/45 §21 D2）
                            let lacks = opts.require.as_ref().map(|r| r.vendor_lacks(k)).unwrap_or_default();
                            let c = chip(name.into(), p.vendor.as_deref() == Some(k), lacks.is_empty().then(|| DataPickMsg::Vendor(k.into())));
                            v.push(if lacks.is_empty() { c } else { with_tip(c, format!("{name} 没有{}", lacks.join("、"))) });
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
            if all_types.is_empty() { crate::ui::fmt::missing() } else { all_types.join(" · ") }
        )),
    ]));
    if sc.items.is_empty() {
        *warn = Some(("这个目录下没扫到认得的数据（目录结构要与该数据商一致）".into(), crate::ui::pal::warn()));
        return b.into();
    }

    // 市场
    let mks: Vec<Choice> = sc.markets().into_iter().map(|m| Choice { label: market_label(&m), key: m }).collect();
    // 宿主预置了标的而没给市场（按已存的选择打开）：按扫描补上
    let guessed = p.market.clone().or_else(|| p.symbol.as_deref().and_then(|s| sc.market_of(s)));
    b = b.push(line(vec![label("市场"), pick(mks, guessed.as_deref(), "选市场".into(), 130.0, DataPickMsg::Market)]));
    let Some(mk) = guessed.as_deref() else {
        return b.into();
    };

    // 标的
    let all = sc.symbols(mk, "").len();
    let items: Vec<Choice> = sc
        .symbols(mk, &p.search)
        .into_iter()
        .map(|(s, n)| {
            // B5 本地自有数据：标出渠道（同名标的可能来自不同渠道）
            let ch = sc.channel_of(&s);
            let ch = if ch.is_empty() { String::new() } else { format!(" · {ch}") };
            Choice { label: if n > 1 { format!("{s}{ch} · {n} 天") } else { format!("{s}{ch}") }, key: s }
        })
        .collect();
    b = b.push(symbol_row(p, all, items));
    let Some(sym) = p.symbol.as_deref() else {
        return b.into();
    };

    // 时间：日期（+ 起始时刻 / 时长）
    let dates = sc.dates(sym);
    let latest = dates.last().cloned();
    let n = dates.len();
    // 缺要的数据的日子在下拉里标出来（还能选，选了会提示，运行前拦下）
    let req = opts.require.as_ref();
    let vendor = p.local_key().unwrap_or("");
    let dchoices: Vec<Choice> = dates
        .into_iter()
        .rev()
        .map(|d| {
            let lack = req.map(|r| r.day_lacks(vendor, &sc.types(sym, &d))).unwrap_or_default();
            Choice { label: if lack.is_empty() { d.clone() } else { format!("{d} · 缺{}", lack.join("、")) }, key: d }
        })
        .collect();
    let mut v = vec![label("时间"), pick(dchoices.clone(), p.date.as_deref(), format!("选日期（{n}）"), 130.0, DataPickMsg::Date)];
    if opts.range {
        // 一段日期：结束日只能选在起始日之后；不选 = 只跑那一天
        v.push(dim("到".into()));
        let later: Vec<Choice> = dchoices.into_iter().filter(|c| p.date.as_deref().is_none_or(|d| c.key.as_str() >= d)).collect();
        v.push(pick(later, p.end.as_deref().or(p.date.as_deref()), "结束日".into(), 130.0, DataPickMsg::End));
        if let (Some(d), Some(last)) = (p.date.as_deref(), latest.as_deref()) {
            let days = sc.dates_in(sym, d, p.end.as_deref().unwrap_or(d)).len();
            v.push(dim(format!("{days} 天有数据")));
            if p.end.as_deref() != Some(last) && last > d {
                v.push(chip("到最新".into(), false, Some(DataPickMsg::End(last.to_string()))));
            }
        }
    } else if let Some(d) = latest
        && p.date.as_deref() != Some(d.as_str())
    {
        v.push(chip("最新".into(), false, Some(DataPickMsg::Date(d))));
    }
    // B5 本地自有数据按整日跑（回测脚本不切时段）
    let window = opts.time == TimeMode::Window && p.source != Some(BSource::Own);
    if opts.time == TimeMode::Window && !window {
        v.push(dim("按整日（UTC）".into()));
    }
    if window {
        v.push(dim("起始（UTC）".into()));
        // 步进 ±15 分钟或手填；填错显示错误态（docs/35 §6.1 时间输入）
        v.push(crate::ui::widgets::time_input(&p.start, 15, DataPickMsg::Start));
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
    // 回放只读所选那一天的文件（docs/67 T0）：过了 UTC 24:00 的那段没有数据，说清楚
    if window && let Some(over) = super::data_picker::past_midnight(&p.start, p.minutes) {
        *warn = Some((format!("过了 UTC 24:00 {over} 分钟：只取所选那一天的数据（不跨天），后面那段是空的——改起始或时长"), crate::ui::pal::bad()));
    }
    if let Some(cov) = p.coverage(opts) {
        if cov.days == 0 {
            *warn = Some(("所选日期里本地没有这个标的的数据".into(), crate::ui::pal::bad()));
        } else if !cov.missing.is_empty() {
            let mut lack: Vec<&str> = cov.missing.iter().flat_map(|(_, l)| l.iter().map(String::as_str)).collect();
            lack.sort_unstable();
            lack.dedup();
            let days: Vec<&str> = cov.missing.iter().take(4).map(|(d, _)| d.as_str()).collect();
            *warn = Some((
                format!("{} 天里有 {} 天缺{}（{}{}）：运行前会被拦下", cov.days, cov.missing.len(), lack.join("、"), days.join("、"), if cov.missing.len() > 4 { " …" } else { "" }),
                crate::ui::pal::bad(),
            ));
        }
    }
    // 常用时段 + 换算后的绝对范围（UTC 与北京时间）
    if window {
        b = b.push(crate::ui::widgets::time_range(p.date.as_deref(), &p.start, p.minutes, DataPickMsg::Window));
    }

    // 数据类型：当天有的全部列出，可勾选；没选日期时列出该标的出现过的类型（各几天）
    if opts.hide_types {
        return b.into();
    }
    match p.date.as_deref() {
        Some(d) => {
            let types = if opts.range { sc.types_in(sym, d, p.end.as_deref().unwrap_or(d)) } else { sc.types(sym, d) };
            let mut v = vec![label("数据类型")];
            for t in &types {
                // 必需的类型（满足宿主要的数据）锁住，不能取消
                if req.is_some_and(|r| r.needs_type(vendor, t)) {
                    v.push(with_tip(chip(format!("☑ {t} · 必需"), true, None), "这张图要用它，取消不了".into()));
                    continue;
                }
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

/// 工具栏「数据 ▾」按钮上的字（docs/64 §4）：选完了是一行说明（去掉类型数），没选完说选到了哪一级。
#[must_use]
pub fn short(p: &DataPick, opts: &PickOpts) -> String {
    if let Some(s) = p.selection(opts) {
        let d = s.describe();
        return match d.rsplit_once(" · ") {
            Some((head, tail)) if tail.ends_with("类数据") => head.to_string(),
            _ => d,
        };
    }
    let mut parts: Vec<String> = Vec::new();
    match p.pipeline {
        Some(Pipeline::A) => parts.push("A".into()),
        Some(Pipeline::B) => parts.push(p.source.map_or_else(|| "B".to_string(), |s| s.label().split(' ').next().unwrap_or("B").to_string())),
        None => {}
    }
    if let Some(k) = p.local_key() {
        parts.push(dp::source_label(k));
    } else if let Some(v) = p.venue.as_ref().or(p.vendor.as_ref()) {
        parts.push(v.clone());
    }
    parts.extend(p.symbol.clone());
    if parts.is_empty() { "未选".into() } else { format!("{} · 未选完", parts.join(" · ")) }
}

/// 面板里原来摆选择器的地方：一行说明 + 去哪改（docs/64 §4：选择器只在工具栏「数据 ▾」浮窗里）。
pub fn summary<'a, M: 'a>(p: &DataPick, opts: &PickOpts) -> Element<'a, M> {
    text(format!("数据：{}　（在顶部工具栏「数据 ▾」里改）", short(p, opts)))
        .size(crate::ui::text::s_meta())
        .color(crate::ui::pal::dim())
        .into()
}
