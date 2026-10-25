//! 市场状态叠加层的设置（docs/62 R2）：检查器「属性」页里 K 线图的「市场状态」一节。
//!
//! 开关、底色维度、底部条带、切换标记、周期，外加这个品种有没有时间线文件、底色维度的图例。
//! 设置全局一份（[`super::regime::settings`]），改了所有 K 线图一起变。

use iced::widget::{button, column, container, row, text};
use iced::{Background, Element, Length};

use super::regime::{self as rg, RegimeMsg};
use crate::ui::{metrics, pal, text as t};

fn chip<'a>(label: impl Into<String>, active: bool, msg: RegimeMsg) -> Element<'a, RegimeMsg> {
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

/// `symbol`：图的品种键（`Binance Linear:BTCUSDT`）。
pub fn inspector<'a>(symbol: &str) -> Element<'a, RegimeMsg> {
    let s = rg::settings();
    let files = rg::files_for(symbol);
    let mut b = column![crate::ui::widgets::section("市场状态")].spacing(metrics::space(2));
    b = b.push(row![chip("在图上显示", s.on, RegimeMsg::On(!s.on)), chip("切换标记", s.marks, RegimeMsg::Marks(!s.marks))].spacing(metrics::space(1)).wrap());
    let code = symbol.rsplit(':').next().unwrap_or(symbol).to_string();
    let Some(f) = files.first().cloned() else {
        return b
            .push(text(format!("没有 {code} 的状态文件。在终端运行：")).size(t::s_meta()).color(pal::warn()))
            .push(text(format!(".venv/bin/python -m factory.regime run {code} --tf 15m")).size(t::s_meta()).font(crate::style::AZERET_MONO))
            .push(t::metadata("只画时间基的 K 线图；文件改了两秒内重画").color(pal::dim()))
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
    b.width(Length::Fill).into()
}
