//! 市场状态叠加层的设置（docs/62 R2）：检查器「属性」页里 K 线图的「市场状态」一节。
//!
//! 开关、底色维度、底部条带、切换标记、周期，外加这个品种有没有时间线文件、底色维度的图例。
//! 设置全局一份（[`super::regime::settings`]），改了所有 K 线图一起变。

use iced::widget::{button, column, container, row, text};
use iced::{Background, Element, Length};

use super::regime::{self as rg, RegimeMsg};
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

/// 这张图的自动算状态一行（docs/69 B）：（文字, 颜色）。
fn auto_line(symbol: &str, iv: u64, code: &str, auto: bool) -> (String, iced::Color) {
    use super::regime_auto::{self as ra, Job};
    let Some((tf, _)) = ra::target(iv) else {
        return ("这张图不是按时间周期的（按笔 / 秒级），状态算不了".into(), pal::dim());
    };
    match ra::job(symbol, iv) {
        Some(Job::Running { since, why }) => return (format!("◌ 正在算 {code} {tf}（{why}，{} 秒）……", since.elapsed().as_secs()), pal::dim()),
        Some(Job::Failed { msg, .. }) => return (format!("✗ {code} {tf} 算不了：{msg}（10 分钟后再自动试，或点「现在算」）"), pal::bad()),
        _ => {}
    }
    let done = match ra::job(symbol, iv) {
        Some(Job::Done { secs, .. }) => format!("（上次算了 {secs:.1} 秒）"),
        _ => String::new(),
    };
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    match ra::exact_file(symbol, iv) {
        Some(f) => {
            let (stale, pchg) = ra::file_state(&f, now_ms);
            let upto = f.last_ms.map(rg::local_hm).unwrap_or_else(|| "?".into());
            let mut s = format!("● {tf} 状态算到 {upto} 收盘{done}");
            if stale {
                s.push_str(if auto { " · 等新 K 线（自动算）" } else { " · 已过期（自动算关着，点「现在算」）" });
            }
            if pchg {
                s.push_str(" · 参数改了、还没重算");
            }
            (s, if (stale && !auto) || pchg { pal::warn() } else { pal::ok() })
        }
        None => (
            if auto { format!("{code} 还没有 {tf} 的状态，马上自动算（数据：币安 U 本位永续 K 线）") } else { format!("{code} 还没有 {tf} 的状态；点「现在算」") },
            pal::warn(),
        ),
    }
}

/// `symbol`：图的品种键（`Binance Linear:BTCUSDT`）；`iv`：图的周期（毫秒，按笔的图为 0）。
pub fn inspector<'a>(symbol: &str, iv: u64) -> Element<'a, RegimeMsg> {
    let s = rg::settings();
    let files = rg::files_for(symbol);
    let mut b = column![crate::ui::widgets::section("市场状态")].spacing(metrics::space(2));
    b = b.push(
        row![
            chip("在图上显示", s.on, RegimeMsg::On(!s.on)),
            chip("切换标记", s.marks, RegimeMsg::Marks(!s.marks)),
            chip("跟随图表自动算", s.auto, RegimeMsg::Auto(!s.auto)),
        ]
        .spacing(metrics::space(1))
        .wrap(),
    );
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_uppercase();
    let (line, c) = auto_line(symbol, iv, &code, s.auto);
    let mut st = row![text(line).size(t::s_meta()).color(c)].spacing(metrics::space(2)).align_y(iced::Alignment::Center);
    if super::regime_auto::target(iv).is_some() {
        st = st.push(chip("现在算", false, RegimeMsg::ComputeNow(symbol.to_string(), iv)));
    }
    b = b.push(st.wrap());
    let Some(f) = rg::pick(&files, iv, s.tf.as_deref()) else {
        return b
            .push(t::metadata("只画时间基的 K 线图；算完两秒内上图").color(pal::dim()))
            .width(Length::Fill)
            .into();
    };

    // 周期
    let mut tfs = row![chip("自动", s.tf.is_none(), RegimeMsg::Tf(None))].spacing(metrics::space(1));
    for x in &files {
        let on = s.tf.as_deref() == Some(x.timeframe.as_str());
        tfs = tfs.push(chip(x.timeframe.clone(), on, RegimeMsg::Tf(Some(x.timeframe.clone()))));
    }
    b = b.push(t::metadata("周期（自动 = 跟图的周期挑最接近的）").color(pal::dim())).push(tfs.wrap());

    // 底色
    let mut band = row![].spacing(metrics::space(1));
    for d in &f.dims {
        band = band.push(chip(d.name.clone(), s.band == d.key, RegimeMsg::Band(d.key.clone())));
    }
    b = b.push(t::metadata("底色").color(pal::dim())).push(band.wrap());
    if let Some(d) = f.dim(&s.band) {
        let mut lg = row![].spacing(metrics::space(3));
        for st in &d.states {
            lg = lg.push(swatch(rg::tone_color(&st.tone), st.name.clone()));
        }
        b = b.push(lg.wrap());
    }

    // 底部条带
    let mut strips = row![].spacing(metrics::space(1));
    for d in &f.dims {
        let on = s.strips.contains(&d.key);
        strips = strips.push(chip(d.name.clone(), on, RegimeMsg::Strip(d.key.clone(), !on)));
    }
    b = b.push(t::metadata("底部条带（按点选的先后从上到下）").color(pal::dim())).push(strips.wrap());
    if f.dims.iter().any(|d| d.layer == "L1") {
        b = b.push(
            text("拥挤度 / 持仓流向 / 资金费来自资金费 + 持仓（L1）：只作看盘描述，检验过没有择时价值（docs/62 §11–§13）")
                .size(t::s_meta())
                .color(pal::dim()),
        );
    }

    let made = chrono::DateTime::from_timestamp_millis(f.generated_ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let have: Vec<String> = files.iter().map(|x| x.timeframe.clone()).collect();
    // 不用 metadata 样式：它会把字母转大写，命令就没法照抄了
    b = b.push(
        text(format!(
            "{code} 有 {} 的时间线 · {} · {} 那份生成于 {made}。状态在确认那根收盘后才知道，所以底色从下一根开始；重新计算：python -m factory.regime run {code} --tf <周期>",
            have.join(" / "),
            f.method,
            f.timeframe
        ))
        .size(t::s_meta())
        .color(pal::dim()),
    );
    let note = rg::note();
    if !note.is_empty() {
        b = b.push(t::metadata(note).color(pal::warn()));
    }
    b = b.push(params(&code, iv, &f));
    b.width(Length::Fill).into()
}

/// 参数区（docs/69 C）：改哪一层、预设、8 个参数。
fn params<'a>(code: &str, iv: u64, f: &rg::RegimeFile) -> Element<'a, RegimeMsg> {
    use super::regime_params::{self as rp, Scope};
    let pf = rp::load();
    let market = rp::market_of(&f.source);
    let tf = super::regime_auto::target(iv).map_or_else(|| f.timeframe.clone(), |(t, _)| t.to_string());
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
