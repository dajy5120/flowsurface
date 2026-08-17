//! C4 活体影子 pane 的视图渲染（docs/14 §2「live 指标接 P2 面板」）。
//!
//! **独立新增面板**（`Content::C4Shadow`）——不改任何既有面板；数据走
//! [`super::c4_readout`] 旁路快照。
//!
//! 布局：守护控制条（启停，[`super::c4::C4Msg`]）→ 今日实时（checkpoint）
//! → 影子日表（UTC 日切落账）→ 活体vs重放对照
//! → C4 进度（合格日 n/7，判定规则 docs/preregister-c4-live.md）。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::c4::C4Msg;
use super::c4_readout::{C4Readout, QUALIFY_TARGET, QUALIFY_UPTIME_SECS};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_GREEN: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_RED: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);

fn sign_c(v: f64) -> Color {
    if v >= 0.0 { C_GREEN } else { C_RED }
}
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    text(title.to_string()).size(14).color(C_HEAD).into()
}
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(11).color(c)).width(Length::Fixed(w)).into()
}
fn wr_s(w: Option<f64>) -> String {
    w.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "—".into())
}
fn bp_s(b: Option<f64>) -> String {
    b.map(|x| format!("{x:+.2}")).unwrap_or_else(|| "—".into())
}

pub fn pane_body<'a>() -> Element<'a, C4Msg> {
    let st: C4Readout = super::c4_readout::snapshot();
    let mut body = column![].spacing(6).padding(10);

    // ── 守护启停（不随开机自启，全由这里控制；状态来自 poller，非每帧查） ──
    body = body.push(sec("maker 影子守护（SOLUSDT · 不下真实单）"));
    {
        // 状态行：● 运行中 已X 重启N次 / ○ 已停止（同录制驾驶舱口径）
        let (dot, dotc, run) = if st.svc.active {
            (
                "●",
                C_GREEN,
                format!(
                    "运行中  已 {}  重启 {} 次",
                    super::svcctl::fmt_dur(st.svc.uptime_secs),
                    st.svc.restarts
                ),
            )
        } else {
            ("○", C_DIM, "已停止".to_string())
        };
        body = body.push(
            row![text(format!("{dot} ")).size(14).color(dotc), text(run).size(12).color(C_TXT)]
                .align_y(iced::Alignment::Center),
        );
        let ctl = row![
            button(text("▶ 启动").size(11)).padding([2, 8]).on_press(C4Msg::StartShadow),
            button(text("■ 停止").size(11)).padding([2, 8]).on_press(C4Msg::StopShadow),
            button(text("↻ 重启").size(11)).padding([2, 8]).on_press(C4Msg::RestartShadow),
            button(text("⟳ 刷新").size(11)).padding([2, 8]).on_press(C4Msg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(10).color(C_DIM),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        body = body.push(ctl);
        // 操作反馈单独一行：和按钮挤一行会溢出换行、把按钮挤变形（同 factory 面板）
        let am = super::c4::action_message();
        if !am.is_empty() {
            let bad = am.starts_with('✗');
            body = body.push(text(am).size(10).color(if bad { C_RED } else { C_GREEN }));
        }
    }

    // ── 今日实时（守护 checkpoint，5min 刷新） ──
    body = body.push(sec("今日实时（maker 影子守护 · SOLUSDT · 不下真实单）"));
    match &st.today {
        Some(t) => {
            let stale = t.age_secs > 400;
            body = body.push(
                row![
                    cell(format!("UTC {}", t.utc_day), 110.0, C_TXT),
                    cell(format!("在线 {:.1}h", t.uptime_h), 80.0, C_TXT),
                    cell(
                        format!("ckpt {}s前", t.age_secs.max(0)),
                        90.0,
                        if stale { C_GOLD } else { C_DIM },
                    ),
                    cell(format!("重连 {}", t.reconnects), 70.0, C_DIM),
                ]
                .spacing(4),
            );
            body = body.push(
                row![
                    cell(format!("fills {}", t.n_fills), 80.0, C_TXT),
                    cell(format!("库存 {:+.2}", t.inv), 90.0, C_TXT),
                    cell(format!("日净值 {:+.3}U", t.day_pnl), 120.0, sign_c(t.day_pnl)),
                    cell(format!("胜率 {}", wr_s(t.win_rate)), 90.0, C_TXT),
                ]
                .spacing(4),
            );
            if stale {
                body = body.push(
                    text("⚠ checkpoint 未更新——守护可能卡住，可点上方「↻ 重启」")
                        .size(11)
                        .color(C_GOLD),
                );
            }
        }
        None => {
            body = body.push(
                text("（无 checkpoint——守护未运行，点上方「▶ 启动」开始）")
                    .size(11)
                    .color(C_GOLD),
            );
        }
    }

    // ── 影子日表（Registry maker_shadow_day，UTC 日切） ──
    body = body.push(sec("影子日（UTC 日切落账）"));
    if st.days.is_empty() {
        body = body.push(text("（暂无——首个整日于 UTC 午夜自动落账）").size(11).color(C_DIM));
    } else {
        body = body.push(
            row![
                cell("日".into(), 50.0, C_DIM),
                cell("胜率".into(), 50.0, C_DIM),
                cell("费后U".into(), 70.0, C_DIM),
                cell("bp/回合".into(), 60.0, C_DIM),
                cell("在线".into(), 50.0, C_DIM),
                cell("重连".into(), 40.0, C_DIM),
                cell("".into(), 30.0, C_DIM),
            ]
            .spacing(4),
        );
        for d in &st.days {
            let c = if d.partial { C_DIM } else { C_TXT };
            let day_md = if d.day.len() >= 10 { d.day[5..].to_string() } else { d.day.clone() };
            body = body.push(
                row![
                    cell(day_md, 50.0, c),
                    cell(wr_s(d.win_rate), 50.0, c),
                    cell(format!("{:+.2}", d.pnl), 70.0, sign_c(d.pnl)),
                    cell(bp_s(d.bp), 60.0, d.bp.map(sign_c).unwrap_or(C_DIM)),
                    cell(format!("{:.0}%", d.uptime_secs / 864.0), 50.0, c),
                    cell(format!("{}", d.reconnects), 40.0, C_DIM),
                    cell(if d.partial { "残".into() } else { "".into() }, 30.0, C_DIM),
                ]
                .spacing(4),
            );
        }
    }

    // ── 活体 vs 重放（同日对照，证伪监测） ──
    if !st.vs.is_empty() {
        body = body.push(sec("活体 vs 重放（Δ=活体−重放 bp/回合）"));
        for v in &st.vs {
            let day_md = if v.day.len() >= 10 { v.day[5..].to_string() } else { v.day.clone() };
            let dlt = v.live_bp.map(|l| l - v.replay_bp);
            body = body.push(
                row![
                    cell(day_md, 50.0, C_TXT),
                    cell(format!("活 {}", bp_s(v.live_bp)), 80.0, C_TXT),
                    cell(format!("放 {:+.2}", v.replay_bp), 80.0, C_TXT),
                    cell(
                        dlt.map(|d| format!("Δ {d:+.2}")).unwrap_or_else(|| "Δ —".into()),
                        70.0,
                        dlt.map(sign_c).unwrap_or(C_DIM),
                    ),
                    cell(
                        if v.falsify { "⚠ 证伪旗".into() } else { "".into() },
                        70.0,
                        C_RED,
                    ),
                ]
                .spacing(4),
            );
        }
    }

    // ── C4 进度（preregister-c4-live：7 合格日 · 非残段 · 在线≥80%） ──
    let q = st.qualified();
    body = body.push(sec("C4 判定进度"));
    body = body.push(
        row![
            cell(
                format!("合格影子日 {q}/{QUALIFY_TARGET}"),
                130.0,
                if q >= QUALIFY_TARGET { C_GREEN } else { C_TXT },
            ),
            cell(
                format!("（非残段·在线≥{:.0}%·docs/preregister-c4-live）", QUALIFY_UPTIME_SECS / 864.0),
                300.0,
                C_DIM,
            ),
            cell(
                if st.any_falsify() { "⚠ 存在证伪旗".into() } else { "".into() },
                100.0,
                C_RED,
            ),
        ]
        .spacing(4),
    );
    body = body.push(text(format!("刷新 {}", st.refreshed)).size(10).color(C_DIM));

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
