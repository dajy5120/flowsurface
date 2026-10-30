//! 市场状态的设置界面（docs/62 R2、docs/69、docs/70 C）：显示（开关、底色、条带、标记、周期）、计算（用哪份、自动算、现在算）、
//! 参数（按市场 / 品种 / 周期）。三块在两个地方用：检查器「属性」页的「市场状态」一节，与设置弹窗（图例齿轮 / 工具栏「状态」徽标）。
//!
//! 显示设置按图（docs/70 C，[`super::regime::view`]）；没单独设过的图用全局那份。参数按品种共享（影响计算）。

use iced::widget::{button, column, container, row, text};
use iced::{Background, Element, Length};

use super::regime::{self as rg, RegimeMsg, ViewOp};
use crate::ui::{metrics, pal, text as t};

pub(crate) fn chip<'a>(label: impl Into<String>, active: bool, msg: RegimeMsg) -> Element<'a, RegimeMsg> {
    let label: String = label.into();
    let shown = if active { format!("✓ {label}") } else { label };
    button(text(shown).size(t::s_small()))
        .padding(metrics::pad2(0, 2))
        .style(move |th, st| {
            let mut s = crate::style::button::modifier(th, st, active);
            if active {
                s.background = Some(Background::Color(crate::ui::color(crate::ui::core().accent_soft)));
            }
            s
        })
        .on_press(msg)
        .into()
}

fn swatch<'a>(c: Option<iced::Color>, name: String) -> Element<'a, RegimeMsg> {
    let sw = container(text(""))
        .width(Length::Fixed(10.0))
        .height(Length::Fixed(10.0))
        .style(move |_| container::Style { background: c.map(Background::Color), border: metrics::hairline(2.0), ..Default::default() });
    row![sw, text(name).size(t::s_meta()).color(pal::dim())].spacing(metrics::space(1)).align_y(iced::Alignment::Center).into()
}

fn code_of(symbol: &str) -> String {
    symbol.rsplit(':').next().unwrap_or(symbol).to_uppercase()
}

/// 这份时间线是什么数据算的。
fn source_text(f: &rg::RegimeFile) -> String {
    match f.source.as_str() {
        "chart" => {
            // 与图上数据的日期同口径（UTC 日）
            let utc = |ms: u64| chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.format("%m-%d").to_string()).unwrap_or_default();
            let span = f.span().map(|(a, b)| format!("{}–{}", utc(a), utc(b.saturating_sub(1)))).unwrap_or_default();
            format!("图上的 K 线 {span}")
        }
        "binance" => "币安 U 本位永续 K 线".into(),
        o => o.to_string(),
    }
}

fn day(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.with_timezone(&chrono::Local).format("%m-%d").to_string()).unwrap_or_default()
}

/// 这张图的计算状态一行（docs/69 B、docs/70 B）：（文字, 颜色）。
fn auto_line(chart: &str, symbol: &str, iv: u64, code: &str, auto: bool) -> (String, iced::Color) {
    use super::regime_auto::{self as ra, Job};
    let Some((tf, _)) = ra::target(chart, iv) else {
        return ("这张图不是按时间周期的（按笔 / 秒级），状态算不了".into(), pal::dim());
    };
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let hist = ra::is_hist(rg::range_of(chart), iv, now_ms);
    let what = if hist { "图上这段" } else { "实时" };
    match ra::job(chart, symbol, iv) {
        Some(Job::Running { since, why }) => return (format!("◌ 正在算 {code} {tf}（{what}，{why}，{} 秒）……", since.elapsed().as_secs()), pal::dim()),
        Some(Job::Failed { msg, .. }) => return (format!("✗ {code} {tf} 算不了：{msg}（10 分钟后再自动试，或点「现在算」）"), pal::bad()),
        _ => {}
    }
    let done = match ra::job(chart, symbol, iv) {
        Some(Job::Done { secs, .. }) => format!("（上次算了 {secs:.1} 秒）"),
        _ => String::new(),
    };
    match ra::exact_file(chart, symbol, iv) {
        Some(f) => {
            let (stale, pchg) = ra::file_state(&f, now_ms);
            let mut s = if f.source == "chart" {
                format!("● {tf} 状态：按图上的 K 线算（{}）{done}", source_text(&f))
            } else {
                format!("● {tf} 状态算到 {} 收盘{done}", f.last_ms.map(rg::local_hm).unwrap_or_else(|| "?".into()))
            };
            if stale {
                s.push_str(if auto { " · 等新 K 线（自动算）" } else { " · 已过期（自动算关着，点「现在算」）" });
            }
            if pchg {
                s.push_str(" · 参数改了、还没重算");
            }
            (s, if (stale && !auto) || pchg { pal::warn() } else { pal::ok() })
        }
        None if hist => (
            if auto { format!("{code} 图上这段还没有 {tf} 的状态，马上按图上的 K 线算（不联网）") } else { format!("{code} 图上这段还没有 {tf} 的状态；点「现在算」") },
            pal::warn(),
        ),
        None => (
            if auto { format!("{code} 还没有 {tf} 的状态，马上自动算（数据：币安 U 本位永续 K 线）") } else { format!("{code} 还没有 {tf} 的状态；点「现在算」") },
            pal::warn(),
        ),
    }
}

/// 显示：开关、标记、周期、底色（带图例）、底部条带。选项取这个品种所有时间线维度的并集（docs/70 A）。
fn display<'a>(chart: &str, symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    let v = rg::view(chart);
    let c = chart.to_string();
    let vm = move |op: ViewOp| RegimeMsg::View(c.clone(), op);
    let files = rg::files_for(symbol);
    let f = rg::file_for(chart, symbol, iv);
    let mut b = column![row![chip("在图上显示", v.on, vm(ViewOp::On(!v.on))), chip("切换标记", v.marks, vm(ViewOp::Marks(!v.marks)))].spacing(metrics::space(1)).wrap()]
        .spacing(metrics::space(2));

    // 周期：已有时间线的周期 + 图自己的周期
    let mut tfs: Vec<(u64, String)> = files.iter().map(|x| (x.tf_ms, x.timeframe.clone())).collect();
    if let Some(t) = super::regime_auto::tf_str(iv) {
        tfs.push((iv, t.to_string()));
    }
    tfs.sort();
    tfs.dedup_by(|a, b| a.1 == b.1);
    let pin = rg::pinned_tf(chart, iv);
    let mut tr = row![chip("跟图", pin.is_none(), vm(ViewOp::Tf(None, iv)))].spacing(metrics::space(1));
    for (_, x) in &tfs {
        let on = pin.as_deref() == Some(x.as_str());
        tr = tr.push(chip(x.clone(), on, vm(ViewOp::Tf(Some(x.clone()), iv))));
    }
    b = b.push(t::metadata("周期（跟图 = 图换周期 / 换数据，状态跟着重算；指定的只在现在这个图周期下有效）").color(pal::dim())).push(tr.wrap());

    let dims = rg::dims_union(&files, f.as_deref());
    if dims.is_empty() {
        return b.push(text("这个品种还没有状态文件（打开「跟随图表自动算」或点「现在算」）").size(t::s_meta()).color(pal::warn())).into();
    }
    let has = |k: &str| f.as_ref().is_some_and(|f| f.dim(k).is_some());
    let label = |d: &rg::DimDef| if has(&d.key) { d.name.clone() } else { format!("{} *", d.name) };
    let mut band = row![].spacing(metrics::space(1));
    for d in &dims {
        band = band.push(chip(label(d), v.band == d.key, vm(ViewOp::Band(d.key.clone()))));
    }
    b = b.push(t::metadata("底色").color(pal::dim())).push(band.wrap());
    if let Some(d) = dims.iter().find(|d| d.key == v.band) {
        let mut lg = row![].spacing(metrics::space(3));
        for st in &d.states {
            lg = lg.push(swatch(rg::tone_color(&st.tone), st.name.clone()));
        }
        b = b.push(lg.wrap());
    }
    let mut strips = row![].spacing(metrics::space(1));
    for d in &dims {
        let on = v.strips.contains(&d.key);
        strips = strips.push(chip(label(d), on, vm(ViewOp::Strip(d.key.clone(), !on))));
    }
    b = b.push(t::metadata("底部条带（按点选的先后从上到下）").color(pal::dim())).push(strips.wrap());
    if dims.iter().any(|d| !has(&d.key)) {
        b = b.push(text("* = 图现在用的这份没有这一维（别的周期 / 数据有）；选上了照样保留，换到有的那份就画").size(t::s_meta()).color(pal::dim()));
    }
    if dims.iter().any(|d| d.layer == "L1") {
        b = b.push(
            text("拥挤度 / 持仓流向 / 资金费来自资金费 + 持仓（L1）：只作看盘描述，检验过没有择时价值（docs/62 §11–§13）")
                .size(t::s_meta())
                .color(pal::dim()),
        );
    }
    b.into()
}

/// 计算：自动算开关、这张图的状态行 + 现在算、用的是哪份。
fn calc<'a>(chart: &str, symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    let s = rg::settings();
    let code = code_of(symbol);
    let mut b = column![row![chip("跟随图表自动算", s.auto, RegimeMsg::Auto(!s.auto))].spacing(metrics::space(1))].spacing(metrics::space(2));
    let (line, c) = auto_line(chart, symbol, iv, &code, s.auto);
    let mut st = row![text(line).size(t::s_meta()).color(c)].spacing(metrics::space(2)).align_y(iced::Alignment::Center);
    if super::regime_auto::target(chart, iv).is_some() {
        st = st.push(chip("现在算", false, RegimeMsg::ComputeNow(chart.to_string(), symbol.to_string(), iv)));
    }
    b = b.push(st.wrap());
    let files = rg::files_for(symbol);
    if let Some(f) = rg::file_for(chart, symbol, iv) {
        let cov = rg::coverage(&f, rg::range_of(chart));
        let made = chrono::DateTime::from_timestamp_millis(f.generated_ms)
            .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default();
        b = b.push(
            text(format!(
                "图上用的：{} {}（{}）· 盖住图上这段 {:.0}% · {} · 生成于 {made}。状态在确认那根收盘后才知道，所以底色从下一根开始",
                code,
                f.timeframe,
                source_text(&f),
                cov * 100.0,
                f.method
            ))
            .size(t::s_meta())
            .color(if cov < 0.9 { pal::warn() } else { pal::dim() }),
        );
    }
    let have: Vec<String> = files.iter().map(|x| format!("{}（{}）", x.timeframe, source_text(x))).collect();
    if !have.is_empty() {
        // 不用 metadata 样式：它会把字母转大写，命令就没法照抄了
        b = b.push(
            text(format!("{code} 已有：{}。命令行：python -m factory.regime run {code} --tf <周期>", have.join("、")))
                .size(t::s_meta())
                .color(pal::dim()),
        );
    }
    let note = rg::note();
    if !note.is_empty() {
        b = b.push(t::metadata(note).color(pal::warn()));
    }
    b.into()
}

/// 检查器「属性」页的「市场状态」一节（docs/62 R2 起就在这里，保留）：显示 + 计算 + 参数，外加「在弹窗里设置」。
/// `chart`：图 id；`symbol`：图的品种键（`Binance Linear:BTCUSDT`）；`iv`：图的周期（毫秒，按笔的图为 0）。
pub fn inspector<'a>(chart: &str, symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    let mut b = column![
        row![
            crate::ui::widgets::section("市场状态"),
            chip("在弹窗里设置", false, RegimeMsg::Dialog(Some((chart.to_string(), symbol.to_string(), iv)))),
        ]
        .spacing(metrics::space(2))
        .align_y(iced::Alignment::Center)
        .wrap(),
        display(chart, symbol, iv),
        calc(chart, symbol, iv),
    ]
    .spacing(metrics::space(2));
    if let Some(f) = rg::file_for(chart, symbol, iv) {
        b = b.push(params(chart, &code_of(symbol), iv, &f));
    }
    b.width(Length::Fill).into()
}

/// 设置弹窗（docs/70 C，公共浮窗）：三页——显示 / 计算 / 参数。
pub fn dialog<'a>(chart: &str, symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    let (_, tab) = rg::dialog_state();
    let tabs = crate::ui::widgets::tabs(&[("显示", 0u8), ("计算", 1), ("参数", 2)], &tab, RegimeMsg::DialogTab);
    let body: Element<'a, RegimeMsg> = match tab {
        0 => display(chart, symbol, iv),
        1 => calc(chart, symbol, iv),
        _ => match rg::file_for(chart, symbol, iv) {
            Some(f) => params(chart, &code_of(symbol), iv, &f),
            None => text("还没有状态文件：先到「计算」页点「现在算」").size(t::s_meta()).color(pal::warn()).into(),
        },
    };
    column![
        text(format!("{} · 这张图（显示设置只管这张图；参数按品种共享）", code_of(symbol))).size(t::s_meta()).color(pal::dim()),
        tabs,
        crate::ui::scroll(column![body].padding(metrics::pad2(1, 2))).height(Length::Fill),
    ]
    .spacing(metrics::space(2))
    .width(Length::Fill)
    .into()
}

/// 工具栏「状态」徽标（docs/70 C）：`● 15m 16:00` / `停在 06-30` / `✗` / `◌`；点开设置弹窗。
pub fn toolbar<'a>(chart: &str, symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    use super::regime_auto::{self as ra, Job};
    let v = rg::view(chart);
    let open = rg::dialog_state().0.as_ref().is_some_and(|(c, _, _)| c == chart);
    let value = if !v.on {
        "关".to_string()
    } else {
        match (ra::job(chart, symbol, iv), rg::file_for(chart, symbol, iv)) {
            (Some(Job::Running { .. }), _) => "◌ 算".to_string(),
            (Some(Job::Failed { .. }), _) => "✗".to_string(),
            (_, Some(f)) => {
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                let range = rg::range_of(chart);
                let end = range.map(|r| r.1).unwrap_or(now_ms);
                let hist = super::regime_auto::is_hist(range, iv, now_ms);
                match f.valid_until() {
                    Some(u) if u < end => format!("停在 {}", day(u.saturating_sub(1))),
                    _ if f.source == "chart" => format!("● {} 图上", f.timeframe),
                    // 历史图：写到哪一刻没意义（文件比图长），只写周期
                    _ if hist => format!("● {}", f.timeframe),
                    _ => format!("● {} {}", f.timeframe, f.last_ms.map(|l| rg::local_hm(l).split(' ').next_back().unwrap_or("").to_string()).unwrap_or_default()),
                }
            }
            (_, None) => "无".to_string(),
        }
    };
    crate::ui::shell::tool_pick("状态", value, Some(RegimeMsg::Dialog((!open).then(|| (chart.to_string(), symbol.to_string(), iv)))), open)
}

/// 图例里市场状态那一行：（名字, 值 [(文字, 颜色)], 显示着吗）。
pub type LegendLine = (String, Vec<(String, iced::Color)>, bool);

/// 图例第一行（docs/70 C）：（名字, 值 [(文字, 颜色)], 显示着吗）；这张图从指标列表去掉了 / 不是时间周期的图为 None。
#[must_use]
pub fn legend_line(chart: &str, symbol: &str, iv: u64, at: Option<u64>) -> Option<LegendLine> {
    let v = rg::view(chart);
    if !v.listed || iv == 0 {
        return None;
    }
    let f = rg::file_for(chart, symbol, iv);
    let name = match &f {
        Some(f) => format!("市场状态 {}", f.timeframe),
        None => "市场状态".to_string(),
    };
    let mut vals = Vec::new();
    match &f {
        Some(f) => {
            let t = at.or_else(|| rg::range_of(chart).map(|r| r.1.saturating_sub(iv))).unwrap_or(0);
            match f.dim(&v.band).and_then(|d| f.at(&d.key, t).and_then(|s| d.states.get(s.s)).map(|st| (d.name.clone(), st.clone()))) {
                Some((dn, st)) => vals.push((format!("{dn} {}", st.name), rg::tone_color(&st.tone).unwrap_or_else(pal::dim))),
                None => vals.push(("未算".to_string(), pal::warn())),
            }
        }
        None => vals.push(("还没有状态（自动算中 / 点齿轮）".to_string(), pal::dim())),
    }
    Some((name, vals, v.on))
}

/// 参数区（docs/69 C）：改哪一层、预设、8 个参数。
fn params<'a>(chart: &str, code: &str, iv: u64, f: &rg::RegimeFile) -> Element<'a, RegimeMsg> {
    use super::regime_params::{self as rp, Scope};
    let pf = rp::load();
    let market = rp::market_of(&f.source);
    let tf = super::regime_auto::target(chart, iv).map_or_else(|| f.timeframe.clone(), |(t, _)| t.to_string());
    let scopes = [Scope::All, Scope::Market(market.to_string()), Scope::Symbol(code.to_string()), Scope::SymbolTf(code.to_string(), tf.clone())];
    let (k, note) = rg::param_ui_state();
    let scope = scopes[usize::from(k.min(3))].clone();
    let mut b = column![
        crate::ui::widgets::section("参数（按市场 / 品种 / 周期）"),
        text("只改看盘描述：检验框架不读这些参数，docs/62「状态没有择时价值」的结论不变").size(t::s_meta()).color(pal::dim()),
    ]
    .spacing(metrics::space(2));
    let mut sr = row![t::metadata("改哪一层").color(pal::dim())].spacing(metrics::space(1)).align_y(iced::Alignment::Center);
    for (i, sc) in scopes.iter().enumerate() {
        sr = sr.push(chip(sc.label(), i == usize::from(k.min(3)), RegimeMsg::ParamScope(i as u8)));
    }
    b = b.push(sr.wrap());
    let mut pr = row![t::metadata("预设").color(pal::dim())].spacing(metrics::space(1)).align_y(iced::Alignment::Center);
    for (i, (name, _, _)) in rp::PRESETS.iter().enumerate() {
        pr = pr.push(chip(*name, false, RegimeMsg::ParamPreset(scope.clone(), i)));
    }
    b = b.push(pr.wrap());
    b = b.push(text(format!("预设写进「{}」这一层：反应快 = 切换早、更碎；稳一点 = 切换晚、更少来回；缺省 = 清掉这一层", scope.label())).size(t::s_meta()).color(pal::dim()));

    let eff = rp::resolve(&pf, market, code, &tf);
    let fmt = |v: f64, step: f64| if step >= 1.0 { format!("{v:.0}") } else { format!("{v:.2}") };
    for (key, name, desc, step, lo, hi) in rp::KEYS {
        let cur = eff.get(key).copied().or_else(|| f.params.get(key).and_then(serde_json::Value::as_f64));
        let from = rp::origin(&pf, market, code, &tf, key).unwrap_or_else(|| "缺省".into());
        let mine = rp::has(&pf, &scope, key);
        let val = cur.map_or_else(|| "—".to_string(), |v| fmt(v, step));
        let label = iced::widget::tooltip(
            container(text(name).size(t::s_small())).width(Length::Fixed(68.0)),
            container(text(desc).size(t::s_meta())).padding(metrics::space(2)).style(crate::style::tooltip),
            iced::widget::tooltip::Position::Top,
        );
        let mut r = row![label].spacing(metrics::space(1)).align_y(iced::Alignment::Center);
        let step_to = |d: f64| cur.map(|v| ((v + d).clamp(lo, hi) * 1000.0).round() / 1000.0);
        r = r.push(chip("−", false, RegimeMsg::ParamSet(scope.clone(), key.to_string(), step_to(-step))));
        r = r.push(container(text(val).size(t::s_small()).color(if from == "缺省" { pal::txt() } else { pal::accent() })).width(Length::Fixed(40.0)));
        r = r.push(chip("+", false, RegimeMsg::ParamSet(scope.clone(), key.to_string(), step_to(step))));
        // 值就是这一层写的：给「恢复」；来自别的层：写是哪层（缺省不写）
        if mine {
            r = r.push(chip("恢复", false, RegimeMsg::ParamSet(scope.clone(), key.to_string(), None)));
        } else if from != "缺省" {
            r = r.push(text(format!("（{from}）")).size(t::s_meta()).color(pal::dim()));
        }
        b = b.push(r.wrap());
    }
    let err = rp::load_err();
    if !err.is_empty() {
        b = b.push(text(err).size(t::s_meta()).color(pal::warn()));
    }
    if !note.is_empty() {
        b = b.push(text(note.clone()).size(t::s_meta()).color(if note.starts_with('✗') { pal::bad() } else { pal::dim() }));
    }
    b = b.push(text(format!("存在 {}；Python 命令行 run / live 也照它算", rp::path().display())).size(t::s_meta()).color(pal::dim()));
    b.into()
}
