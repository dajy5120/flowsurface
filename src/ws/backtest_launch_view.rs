//! 「发起回测」栏的渲染（状态见 [`super::backtest_launch`]）。

use iced::widget::{button, column, container, pick_list, row, text};
use iced::{Alignment, Color, Element};

use super::backtest_launch::{self as bl, LaunchMsg};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.58, 0.64);
const C_OK: Color = Color::from_rgb(0.40, 0.82, 0.50);
const C_WARN: Color = Color::from_rgb(0.92, 0.72, 0.32);

fn btn<'a>(label: &str, msg: Option<LaunchMsg>) -> Element<'a, LaunchMsg> {
    let b = button(text(label.to_string()).size(11)).padding([2, 8]);
    match msg {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// 策略下拉框的一项（显示文件名，值是完整路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Strat(String);

impl std::fmt::Display for Strat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = std::path::Path::new(&self.0);
        f.write_str(&p.file_name().map_or_else(|| self.0.clone(), |n| n.to_string_lossy().into_owned()))
    }
}

pub fn view<'a>() -> Element<'a, LaunchMsg> {
    let v = bl::view();
    let mut head = row![
        text("发起回测").size(12).color(C_HEAD),
        btn(if v.open { "▴ 收起" } else { "▾ 选策略与数据" }, Some(LaunchMsg::Toggle)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if v.running {
        head = head.push(btn("■ 停止", Some(LaunchMsg::Stop)));
    }
    let mut b = column![head].spacing(5);
    if !v.status.is_empty() {
        b = b.push(text(v.status.clone()).size(10).color(if v.running { C_OK } else { C_DIM }));
    }
    if v.open {
        let list: Vec<Strat> = bl::strategies().into_iter().map(Strat).collect();
        let cur = v.strategy.clone().map(Strat);
        b = b.push(
            row![
                text("策略").size(11).color(C_DIM),
                pick_list(list, cur, |s: Strat| LaunchMsg::Strategy(s.0)).text_size(11).placeholder("选 strategies/ 下的策略"),
                btn("选择文件…", Some(LaunchMsg::BrowseStrategy)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
        if let Some(s) = &v.strategy {
            b = b.push(text(s.clone()).size(10).color(C_DIM));
        }
        b = b.push(super::data_picker_view::view(&v.pick, &bl::pick_opts()).map(LaunchMsg::Data));
        b = b.push(
            row![
                btn(
                    "▶ 运行回测",
                    (!v.running && v.strategy.is_some() && v.pick.selection(&bl::pick_opts()).is_some())
                        .then_some(LaunchMsg::Run),
                ),
                text(
                    "这里选的是对策略 BACKTEST 声明的覆盖；费率、撮合等仍按策略与 runner 默认。\
                     跑起来后 K 线 / ▲▼ / 订单面板 / 进度条自动跟随，跑完下面出 tearsheet。",
                )
                .size(10)
                .color(C_DIM),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if !v.note.is_empty() {
        b = b.push(text(v.note.clone()).size(10).color(C_WARN));
    }
    container(b).padding([6, 8]).into()
}
