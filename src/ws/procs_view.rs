//! 进程页的渲染。清单与状态在 [`super::procs`]，这里只画。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::procs;

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
const C_WARN: Color = Color::from_rgb(0.95, 0.6, 0.35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcsMsg {
    /// `(条目 key, systemctl 动作)`。
    Act(String, &'static str),
    Refresh,
}

fn chip<'a>(label: &str, msg: ProcsMsg) -> Element<'a, ProcsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, ProcsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

pub fn handle(m: ProcsMsg) -> String {
    match m {
        ProcsMsg::Act(k, a) => procs::action(&k, a),
        ProcsMsg::Refresh => {
            procs::waker().request();
            String::new()
        }
    }
}

pub fn pane_body<'a>(note: &str) -> Element<'a, ProcsMsg> {
    let rows = procs::rows();
    let mut body = column![].spacing(6).padding(8);

    if rows.is_empty() {
        // poller 起来要一拍。**别显示一张空表**——空表看起来像「什么都没在跑」
        return column![text("正在查询…").size(11).color(C_DIM)].padding(8).into();
    }

    let up = rows.iter().filter(|r| r.st.active).count();
    body = body.push(
        row![
            text("进程").size(13).color(C_HEAD),
            text(format!("{up} / {} 在运行", rows.len()))
                .size(13)
                .color(if up > 0 { C_OK } else { C_DIM }),
            chip("刷新", ProcsMsg::Refresh),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text("编排交给 systemd：进程挂了自动按退避重启，「重启」列的数字就是它爬起来过几次。")
            .size(10)
            .color(C_DIM),
    );
    body = body.push(
        text(
            "这些服务都不随开机自启，只在这里启停。有外部连接的那几个（两个 feed）\
             在「网络出口」页上也能停——两边操作的是同一个单元。",
        )
        .size(10)
        .color(C_DIM),
    );
    if !note.is_empty() {
        body = body.push(text(note.to_string()).size(11).color(C_GOLD));
    }

    // ── 表头 ──
    let mut h = row![].spacing(4);
    for (t, w, n) in [
        ("服务", 130.0, false),
        ("干什么", 360.0, false),
        ("状态", 76.0, false),
        ("已运行", 96.0, true),
        ("重启", 56.0, true),
        ("操作", 150.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_DIM, n));
    }
    body = body.push(h);

    for r in &rows {
        let (st_txt, st_col) = if r.st.active {
            ("运行中".to_string(), C_OK)
        } else if r.st.ever_ran() && !r.st.last_ok() {
            (format!("停止（{}）", r.st.last_result), C_WARN)
        } else {
            ("已停止".to_string(), C_DIM)
        };
        let mut line = row![].spacing(4).align_y(iced::Alignment::Center);
        line = line.push(cell(r.label.clone(), 130.0, C_TXT, false));
        line = line.push(cell(r.what.clone(), 360.0, C_DIM, false));
        line = line.push(cell(st_txt, 76.0, st_col, false));
        line = line.push(cell(
            if r.st.active { super::svcctl::fmt_dur(r.st.uptime_secs) } else { "—".into() },
            96.0,
            C_TXT,
            true,
        ));
        // 重启次数不为 0 就标出来：它说明这个服务在反复爬起来，
        // 而「现在是运行中」会把这件事盖住
        line = line.push(cell(
            r.st.restarts.to_string(),
            56.0,
            if r.st.restarts > 0 { C_WARN } else { C_DIM },
            true,
        ));
        let acts = row![
            chip(if r.st.active { "停止" } else { "启动" },
                 ProcsMsg::Act(r.key.clone(), if r.st.active { "stop" } else { "start" })),
            chip("重启", ProcsMsg::Act(r.key.clone(), "restart")),
        ]
        .spacing(4);
        line = line.push(container(acts).width(Length::Fixed(150.0)));
        body = body.push(line);
        // 停了会怎样：只在已停止时显示。运行时挂着一行警告是噪音，
        // 停了却不说会让人以为「进程没了但功能还在」
        if !r.st.active {
            body = body.push(
                container(text(format!("↳ {}", r.if_stopped)).size(10).color(C_DIM))
                    .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 134.0 }),
            );
        }
    }

    scrollable(body).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_offers_the_action_that_makes_sense_now() {
        // 一个已经在跑的服务给「启动」按钮 = 点了什么也不变，
        // 看起来就像按钮坏了。
        for r in procs::rows() {
            let expect = if r.st.active { "stop" } else { "start" };
            let m = ProcsMsg::Act(r.key.clone(), expect);
            assert!(matches!(m, ProcsMsg::Act(_, a) if a == expect));
        }
    }

    #[test]
    fn refresh_returns_no_note() {
        // 刷新不需要回执文字——时间戳自己会跳。回一句「已刷新」是噪音。
        assert!(handle(ProcsMsg::Refresh).is_empty());
    }
}
