//! Tardis 历史回放 pane 的视图渲染（docs/20 Phase 5）。
//!
//! 可编辑状态来自 [`super::tardis_replay::TardisReplayState`]，只读进度来自
//! [`super::tardis_replay_readout`]。view 发出 [`TardisReplayMsg`]，pane.rs 包成
//! `Message::PaneEvent(id, Event::TardisReplayInteraction(..))`。

use iced::widget::{button, column, container, pick_list, progress_bar, row, text};
use iced::{Alignment, Color, Element, Length};

use super::tardis_replay::{
    MINUTES, Speed, TardisReplayMsg, TardisReplayState, available_dates, available_symbols, hours,
    is_running,
};

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn label<'a>(s: &str) -> Element<'a, TardisReplayMsg> {
    text(s.to_string()).size(13).into()
}

/// 渲染回放控制 + 进度。
pub fn pane_body(app: &TardisReplayState) -> Element<'_, TardisReplayMsg> {
    let st = super::tardis_replay_readout::snapshot();
    let running = is_running();

    let syms = available_symbols();
    let dates = available_dates(&app.symbol);
    let n_days = dates.len();

    let header = column![
        text("Tardis 历史回放 — 已购 30 天逐笔进 Cockpit")
            .size(20)
            .color(Color::from_rgb(0.55, 0.8, 1.0)),
        text(format!(
            "数据源：{}｜{} 个符号 × {n_days} 天",
            super::tardis_replay::tardis_root().display(),
            syms.len()
        ))
        .size(11)
        .color(Color::from_rgb(0.55, 0.6, 0.65)),
    ]
    .spacing(4);

    // 拆两行：5 个选择器挤一行会在窄 pane（右侧栏典型宽度）里溢出被裁。
    let picks = column![
        row![
            label("符号"),
            pick_list(syms, Some(app.symbol.clone()), TardisReplayMsg::SymbolPick).text_size(13),
            label("日期"),
            pick_list(dates, Some(app.date.clone()), TardisReplayMsg::DatePick).text_size(13),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            label("起始(UTC)"),
            pick_list(hours(), Some(app.start_hm.clone()), TardisReplayMsg::StartPick)
                .text_size(13),
            label("时长(分)"),
            pick_list(MINUTES.to_vec(), Some(app.minutes), TardisReplayMsg::MinutesPick)
                .text_size(13),
            label("倍速"),
            pick_list(Speed::ALL.to_vec(), Some(app.speed), TardisReplayMsg::SpeedPick)
                .text_size(13),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(8);

    // 起停按钮：跑着就只给「停止」，避免重复起进程。
    let actions = row![
        if running {
            button(text("■ 停止回放").size(13)).on_press(TardisReplayMsg::Stop)
        } else {
            button(text("▶ 开始回放").size(13)).on_press(TardisReplayMsg::Start)
        },
        text(if running { "回放进行中" } else { "空闲" })
            .size(12)
            .color(if running {
                Color::from_rgb(0.5, 0.85, 0.5)
            } else {
                Color::from_rgb(0.55, 0.6, 0.65)
            }),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let hint = text(app.hint.clone()).size(12).color(if app.hint.starts_with('✗') {
        Color::from_rgb(0.9, 0.4, 0.4)
    } else {
        Color::from_rgb(0.6, 0.7, 0.6)
    });

    // 进度区：poller 起来前/无回放记录时给占位，不显 0% 误导。
    let progress: Element<'_, TardisReplayMsg> = if st.total == 0 {
        text(if st.started {
            "尚无回放记录（点「开始回放」后此处显示进度）"
        } else {
            "连接中…"
        })
        .size(12)
        .color(Color::from_rgb(0.55, 0.6, 0.65))
        .into()
    } else {
        // 取 owned String：`st` 是本地快照，借它的 &str 活不过本函数返回值。
        let state_zh: String = match st.state.as_str() {
            "running" => "回放中".into(),
            "done" => "已完成".into(),
            "stopped" => "已停止".into(),
            other => other.to_string(),
        };
        let speed_zh =
            if st.speed == 0.0 { "最快".to_string() } else { format!("×{:.0}", st.speed) };
        column![
            row![
                text(format!("{} {}", st.symbol, st.date)).size(14),
                text(state_zh).size(12).color(match st.state.as_str() {
                    "running" => Color::from_rgb(0.5, 0.85, 0.5),
                    "stopped" => Color::from_rgb(0.9, 0.7, 0.4),
                    _ => Color::from_rgb(0.55, 0.6, 0.65),
                }),
                text(format!("速度 {speed_zh}")).size(12),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            progress_bar(0.0..=100.0, st.pct as f32).girth(Length::Fixed(10.0)),
            text(format!(
                "{} / {} 笔（{:.1}%）｜墙钟 {:.0}s｜run {}",
                group(st.sent),
                group(st.total),
                st.pct,
                st.elapsed,
                st.run_id
            ))
            .size(11)
            .color(Color::from_rgb(0.6, 0.65, 0.7)),
        ]
        .spacing(6)
        .into()
    };

    let note = text(
        "行情经 Redis ws:bt:{run}:trades 进左侧图表（复用既有回测入图链路）；\
         回放期间三态自动切到「回测」，结束落回 stopped。",
    )
    .size(11)
    .color(Color::from_rgb(0.5, 0.55, 0.6));

    container(
        column![header, picks, actions, hint, progress, note]
            .spacing(12)
            .padding(14),
    )
    .width(Length::Fill)
    .into()
}
