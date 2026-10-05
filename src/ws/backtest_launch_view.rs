//! 「发起回测」栏的渲染（状态见 [`super::backtest_launch`]）。

use iced::widget::{button, column, container, pick_list, row, text};
use iced::{Alignment, Element};

use super::backtest_launch::{self as bl, LaunchMsg};

fn btn<'a>(label: &str, msg: Option<LaunchMsg>) -> Element<'a, LaunchMsg> {
    let b = button(text(label.to_string()).size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3));
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
        // strategies/ 之下的相对路径（子目录即分类，如 orderflow/orderflow_confluence.py）
        match self.0.rfind("strategies/") {
            Some(i) => f.write_str(&self.0[i + "strategies/".len()..]),
            None => {
                let p = std::path::Path::new(&self.0);
                f.write_str(&p.file_name().map_or_else(|| self.0.clone(), |n| n.to_string_lossy().into_owned()))
            }
        }
    }
}

/// `hosted` = 数据选择正显示在检查器里（见 `ws::inspector_props`），这里只留一行提示。
pub fn view<'a>(hosted: bool) -> Element<'a, LaunchMsg> {
    let v = bl::view();
    let mut head = row![
        text("发起回测").size(crate::ui::text::s_body()).color(crate::ui::pal::head()),
        btn(if v.open { "▴ 收起" } else { "▾ 选策略与数据" }, Some(LaunchMsg::Toggle)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if v.running {
        head = head.push(btn("■ 停止", Some(LaunchMsg::Stop)));
    }
    let mut b = column![head].spacing(5);
    if !v.status.is_empty() {
        b = b.push(text(v.status.clone()).size(crate::ui::text::s_meta()).color(if v.running { crate::ui::pal::ok() } else { crate::ui::pal::dim() }));
    }
    if v.open {
        let list: Vec<Strat> = bl::strategies().into_iter().map(Strat).collect();
        let cur = v.strategy.clone().map(Strat);
        b = b.push(
            row![
                text("策略").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
                pick_list(list, cur, |s: Strat| LaunchMsg::Strategy(s.0)).text_size(11).placeholder("选 strategies/ 下的策略"),
                btn("选择文件…", Some(LaunchMsg::BrowseStrategy)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
        if let Some(s) = &v.strategy {
            b = b.push(text(s.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
        }
        b = b.push(if hosted {
            super::inspector_props::hint("数据选择")
        } else {
            super::data_picker_view::view(&v.pick, &bl::pick_opts()).map(LaunchMsg::Data)
        });
        b = b.push(
            row![
                // 不能点时说清为什么（docs/35 §6.2：禁用要给原因）
                crate::ui::widgets::btn_why(
                    "▶ 运行回测",
                    crate::ui::widgets::Kind::Primary,
                    (!v.running && v.strategy.is_some() && v.pick.selection(&bl::pick_opts()).is_some())
                        .then_some(LaunchMsg::Run),
                    if v.running {
                        "回测正在跑，跑完再发起下一次"
                    } else if v.strategy.is_none() {
                        "先在上面选一个策略文件"
                    } else {
                        "数据还没选完：管线 → 来源 → 市场 → 标的 → 时间"
                    },
                ),
                text(
                    "这里选的是对策略 BACKTEST 声明的覆盖；费率、撮合等仍按策略与 runner 默认。\
                     跑起来后 K 线 / ▲▼ / 订单面板 / 进度条自动跟随，跑完下面出 tearsheet。",
                )
                .size(crate::ui::text::s_meta())
                .color(crate::ui::pal::dim()),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if !v.note.is_empty() {
        b = b.push(text(v.note.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::warn()));
    }
    container(b).padding(crate::ui::metrics::pad2(2, 3)).into()
}
