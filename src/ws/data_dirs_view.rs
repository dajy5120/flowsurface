//! 数据目录的界面（docs/47）：挂载点缺失 / 目录不可用时的弹出提醒，与设置窗口里的「数据目录…」卡片。

use iced::widget::{Space, column, row, text_input};
use iced::{Alignment, Element, Length};

use super::data_dirs::{self as dd, DataDirsMsg, Field};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind as Btn};

const ALERT_WIDTH: f32 = 620.0;
const CARD_WIDTH: f32 = 680.0;

/// 要盖在界面最上层的那一层：设置卡片开着就是它，否则有问题时是提醒；都没有 = `None`。
#[must_use]
pub fn overlay<'a>() -> Option<Element<'a, DataDirsMsg>> {
    if let Some(c) = dd::card() {
        return Some(w::modal_layer(card(c), CARD_WIDTH, None));
    }
    dd::alert().map(|rows| w::modal_layer(alert(rows), ALERT_WIDTH, None))
}

fn alert<'a>(rows: Vec<dd::Row>) -> Element<'a, DataDirsMsg> {
    let not_mounted = rows.iter().any(|r| r.problem.as_deref().is_some_and(|p| p.contains("没有挂上")));
    let mut col = column![t::section(if not_mounted { "▲ 数据盘没有挂上" } else { "▲ 数据目录不可用" }).color(pal::bad())].spacing(space(3));
    for r in rows.iter().filter(|r| r.problem.is_some()) {
        col = col.push(
            column![t::label(format!("{}  {}", r.role, r.path.display())), t::body(r.problem.clone().unwrap_or_default())]
                .spacing(space(1)),
        );
    }
    col = col.push(
        t::caption("受影响的：录制器、特征引擎的订单流层析输出、回测与研究读不到大数据；它们会拒绝启动或跳过这一路，不会写到别处。小文件目录正常的话，面板快照、日历、登记表照常。")
            .color(pal::dim()),
    );
    col = col.push(
        row![
            Space::new().width(Length::Fill),
            w::btn("数据目录设置…", Btn::Ghost, Some(DataDirsMsg::Open)),
            w::btn("重新检查", Btn::Standard, Some(DataDirsMsg::Recheck)),
            w::btn("知道了", Btn::Primary, Some(DataDirsMsg::Dismiss)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    col.into()
}

fn field<'a>(label: &str, hint: &str, placeholder: &str, value: &str, f: Field) -> Element<'a, DataDirsMsg> {
    column![
        row![t::label(label.to_string()), t::caption(hint.to_string()).color(pal::dim())].spacing(space(2)).align_y(Alignment::Center),
        text_input(placeholder, value).on_input(move |v| DataDirsMsg::Edit(f, v)).width(Length::Fill),
    ]
    .spacing(space(1))
    .into()
}

fn card<'a>(c: dd::Card) -> Element<'a, DataDirsMsg> {
    let (d, h, m) = dd::effective();
    let mut col = column![
        row![t::section("数据目录"), Space::new().width(Length::Fill), w::btn("关闭", Btn::Ghost, Some(DataDirsMsg::Close))].align_y(Alignment::Center),
        t::caption(format!(
            "现在生效：大数据 {} · 小文件 {} · 挂载点 {}",
            d.display(),
            h.display(),
            m.map_or_else(|| "不要求".to_string(), |m| m.display().to_string())
        ))
        .color(pal::dim()),
        field("大数据目录", "录制的行情、特征、信号、标签、订单流层析……可以放在挂载盘上", "空 = 缺省 ~/ws-data", &c.data, Field::Data),
        field("小文件目录", "SQLite 库、实时断点、面板快照、报告——经常写，放本机盘", "空 = 与大数据目录相同", &c.hot, Field::Hot),
        field("要求挂着的挂载点", "写了就要求它真的挂着，没挂上时拒绝读写并弹出提醒", "例如 /data；空 = 不要求", &c.mount, Field::Mount),
        t::caption("程序不会替你创建数据根目录（免得盘没挂上时写到根分区上）：目录要先建好。路径只在启动时读一次，保存后重启 Cockpit 生效。").color(pal::dim()),
    ]
    .spacing(space(3));
    for (role, path, p) in &c.preview {
        let (mark, msg, tone) = match p {
            None => ("✓", "通过".to_string(), pal::ok()),
            Some(e) => ("✗", e.clone(), pal::bad()),
        };
        col = col.push(t::caption(format!("{mark} {role}  {path}：{msg}")).color(tone));
    }
    if !c.note.is_empty() {
        col = col.push(t::caption(c.note.clone()).color(if c.note.starts_with('✗') { pal::bad() } else { pal::accent() }));
    }
    col = col.push(
        row![
            Space::new().width(Length::Fill),
            w::btn("检查", Btn::Standard, Some(DataDirsMsg::Test)),
            w::btn("保存", Btn::Primary, Some(DataDirsMsg::Save)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    col.into()
}
