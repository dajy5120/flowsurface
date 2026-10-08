//! C4 活体影子 pane 的视图渲染（docs/14 §2「live 指标接 P2 面板」）。
//!
//! **独立新增面板**（`Content::C4Shadow`）——不改任何既有面板；数据走
//! [`super::c4_readout`] 旁路快照。
//!
//! 布局：守护控制条（启停，[`super::c4::C4Msg`]）→ 今日实时（checkpoint）
//! → 影子日表（UTC 日切落账）→ 活体vs重放对照
//! → C4 进度（合格日 n/7，判定规则 docs/preregister-c4-live.md）。

use iced::widget::{button, column, container, row, scrollable, text};
use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};
use iced::{Color, Element, Length};

use super::c4::C4Msg;
use super::c4_readout::{C4Readout, QUALIFY_TARGET, QUALIFY_UPTIME_SECS};

fn sign_c(v: f64) -> Color {
    if v >= 0.0 { crate::ui::pal::up() } else { crate::ui::pal::down() }
}
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    text(title.to_string()).size(crate::ui::text::s_section()).color(crate::ui::pal::head()).into()
}
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(crate::ui::text::s_small()).color(c)).width(Length::Fixed(w)).into()
}
fn wr_s(w: Option<f64>) -> String {
    w.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| crate::ui::fmt::missing())
}
fn bp_s(b: Option<f64>) -> String {
    b.map(|x| format!("{x:+.2}")).unwrap_or_else(|| crate::ui::fmt::missing())
}

thread_local! {
    static DAYS: std::cell::RefCell<Option<GridState>> = const { std::cell::RefCell::new(None) };
    static VS: std::cell::RefCell<Option<GridState>> = const { std::cell::RefCell::new(None) };
}

fn day_cols() -> Vec<Column> {
    vec![
        Column::text("日", 60.0).key(),
        Column::num("胜率", None, 60.0),
        Column::num("费后", Some("U"), 80.0),
        Column::num("bp/回合", None, 70.0),
        Column::num("在线", Some("%"), 60.0),
        Column::num("重连", None, 50.0),
        Column::text("残段", 44.0).groupable(),
    ]
}

fn vs_cols() -> Vec<Column> {
    vec![
        Column::text("日", 60.0).key(),
        Column::num("活体", Some("bp/回合"), 100.0),
        Column::num("重放", Some("bp/回合"), 100.0),
        Column::num("Δ 活−放", None, 80.0),
        Column::text("证伪", 80.0).groupable(),
    ]
}

/// 两张表的网格交互（经 C4Msg::DaysGrid / VsGrid 回到这里）。
pub fn grid_update(vs: bool, m: GridMsg) {
    let (cell, cols) = if vs { (&VS, vs_cols()) } else { (&DAYS, day_cols()) };
    cell.with(|g| g.borrow_mut().get_or_insert_with(|| GridState::new(&cols)).update(m, &cols, &[]));
}

/// 一张固定高度的网格（外层是滚动容器）。
fn table<'a>(vs: bool, cols: Vec<Column>, rows: Vec<Vec<Cell>>, wrap: fn(GridMsg) -> C4Msg) -> Element<'a, C4Msg> {
    let cell = if vs { &VS } else { &DAYS };
    let n = rows.len();
    let state = cell.with(|g| {
        let mut g = g.borrow_mut();
        let s = g.get_or_insert_with(|| GridState::new(&cols));
        s.resort(&cols, &rows);
        s.clone()
    });
    let h = crate::ui::metrics::panel_header() + crate::ui::metrics::row_height() * (n.min(14) as f32 + 1.0) + 60.0;
    container(grid::view(cols, rows, state, None, wrap)).height(Length::Fixed(h)).into()
}

pub fn pane_body<'a>() -> Element<'a, C4Msg> {
    let st: C4Readout = super::c4_readout::snapshot();
    // 部位（docs/42 第 4 期）：守护启停与状态进检查器「数据」页，主区从「今日实时」起
    let part = super::inspector_props::part();
    let mut body = column![].spacing(6).padding(if part.side() { 0.0 } else { crate::ui::metrics::space(3) });

    // ── 守护启停（不随开机自启，全由这里控制；状态来自 poller，非每帧查） ──
    if part.data() {
    body = body.push(sec("maker 影子守护（SOLUSDT · 不下真实单）"));
    {
        // 状态行：● 运行中 已X 重启N次 / ○ 已停止（同录制驾驶舱口径）
        let (dot, dotc, run) = if st.svc.active {
            (
                "●",
                crate::ui::pal::up(),
                format!(
                    "运行中  已 {}  重启 {} 次",
                    super::svcctl::fmt_dur(st.svc.uptime_secs),
                    st.svc.restarts
                ),
            )
        } else {
            ("○", crate::ui::pal::dim(), "已停止".to_string())
        };
        body = body.push(
            row![text(format!("{dot} ")).size(crate::ui::text::s_section()).color(dotc), text(run).size(crate::ui::text::s_body()).color(crate::ui::pal::txt())]
                .align_y(iced::Alignment::Center),
        );
        let ctl = row![
            button(text("▶ 启动").size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3)).on_press(C4Msg::StartShadow),
            button(text("■ 停止").size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3)).on_press(C4Msg::StopShadow),
            button(text("↻ 重启").size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3)).on_press(C4Msg::RestartShadow),
            button(text("⟳ 刷新").size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3)).on_press(C4Msg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        body = body.push(ctl);
        // 操作反馈单独一行：和按钮挤一行会溢出换行、把按钮挤变形（同 factory 面板）
        let am = super::c4::action_message();
        if !am.is_empty() {
            let bad = am.starts_with('✗');
            body = body.push(text(am).size(crate::ui::text::s_meta()).color(if bad { crate::ui::pal::down() } else { crate::ui::pal::up() }));
        }
    }
    }
    if part.side() {
        return body.into();
    }

    // ── 今日实时（守护 checkpoint，5min 刷新） ──
    body = body.push(crate::ui::mark::here());
    body = body.push(sec("今日实时（maker 影子守护 · SOLUSDT · 不下真实单）"));
    match &st.today {
        Some(t) => {
            let stale = t.age_secs > 400;
            body = body.push(
                row![
                    cell(format!("UTC {}", t.utc_day), 110.0, crate::ui::pal::txt()),
                    cell(format!("在线 {:.1}h", t.uptime_h), 80.0, crate::ui::pal::txt()),
                    cell(
                        format!("ckpt {}s前", t.age_secs.max(0)),
                        90.0,
                        if stale { crate::ui::pal::warn() } else { crate::ui::pal::dim() },
                    ),
                    cell(format!("重连 {}", t.reconnects), 70.0, crate::ui::pal::dim()),
                ]
                .spacing(4),
            );
            body = body.push(
                row![
                    cell(format!("fills {}", t.n_fills), 80.0, crate::ui::pal::txt()),
                    cell(format!("库存 {:+.2}", t.inv), 90.0, crate::ui::pal::txt()),
                    cell(format!("日净值 {}", crate::ui::fmt::sim(format!("{:+.3}U", t.day_pnl))), 120.0, sign_c(t.day_pnl)),
                    cell(format!("胜率 {}", wr_s(t.win_rate)), 90.0, crate::ui::pal::txt()),
                ]
                .spacing(4),
            );
            if stale {
                body = body.push(
                    text("⚠ checkpoint 未更新——守护可能卡住，可在检查器「数据」页点「↻ 重启」")
                        .size(crate::ui::text::s_small())
                        .color(crate::ui::pal::warn()),
                );
            }
        }
        None => {
            body = body.push(
                text("（无 checkpoint——守护未运行，到检查器「数据」页（工具栏「⚙ 设置」）点「▶ 启动」）")
                    .size(crate::ui::text::s_small())
                    .color(crate::ui::pal::warn()),
            );
        }
    }

    // ── 影子日表（Registry maker_shadow_day，UTC 日切） ──
    body = body.push(sec("影子日（UTC 日切落账）"));
    if st.days.is_empty() {
        body = body.push(text("（暂无——首个整日于 UTC 午夜自动落账）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
    } else {
        // 换成 ui::grid（docs/35 §16.13 第 5 项）：可排序、调宽、多选合计、右键复制
        let cols = day_cols();
        let rows: Vec<Vec<Cell>> = st
            .days
            .iter()
            .map(|d| {
                let day_md = if d.day.len() >= 10 { d.day[5..].to_string() } else { d.day.clone() };
                let c = if d.partial { crate::ui::pal::dim() } else { crate::ui::pal::txt() };
                vec![
                    Cell::Colored(day_md, c),
                    Cell::Colored(wr_s(d.win_rate), c),
                    Cell::Colored(crate::ui::fmt::sim(format!("{:+.2}", d.pnl)), sign_c(d.pnl)),
                    Cell::Colored(bp_s(d.bp), d.bp.map(sign_c).unwrap_or(crate::ui::pal::dim())),
                    Cell::Colored(format!("{:.0}%", d.uptime_secs / 864.0), c),
                    Cell::num(d.reconnects as f64, d.reconnects.to_string()),
                    Cell::Colored(if d.partial { "残".into() } else { String::new() }, crate::ui::pal::dim()),
                ]
            })
            .collect();
        body = body.push(table(false, cols, rows, C4Msg::DaysGrid));
    }

    // ── 活体 vs 重放（同日对照，证伪监测） ──
    if !st.vs.is_empty() {
        body = body.push(sec("活体 vs 重放（Δ=活体−重放 bp/回合）"));
        let cols = vs_cols();
        let rows: Vec<Vec<Cell>> = st
            .vs
            .iter()
            .map(|v| {
                let day_md = if v.day.len() >= 10 { v.day[5..].to_string() } else { v.day.clone() };
                let dlt = v.live_bp.map(|l| l - v.replay_bp);
                vec![
                    Cell::Text(day_md),
                    Cell::Text(bp_s(v.live_bp)),
                    Cell::Text(format!("{:+.2}", v.replay_bp)),
                    Cell::Colored(dlt.map(|d| format!("{d:+.2}")).unwrap_or_else(|| crate::ui::fmt::missing()), dlt.map(sign_c).unwrap_or(crate::ui::pal::dim())),
                    Cell::Colored(if v.falsify { "⚠ 证伪旗".into() } else { String::new() }, crate::ui::pal::down()),
                ]
            })
            .collect();
        body = body.push(table(true, cols, rows, C4Msg::VsGrid));
    }

    // ── C4 进度（preregister-c4-live：7 合格日 · 非残段 · 在线≥80%） ──
    let q = st.qualified();
    body = body.push(sec("C4 判定进度"));
    body = body.push(
        row![
            cell(
                format!("合格影子日 {q}/{QUALIFY_TARGET}"),
                130.0,
                if q >= QUALIFY_TARGET { crate::ui::pal::up() } else { crate::ui::pal::txt() },
            ),
            cell(
                format!("（非残段·在线≥{:.0}%·docs/preregister-c4-live）", QUALIFY_UPTIME_SECS / 864.0),
                300.0,
                crate::ui::pal::dim(),
            ),
            cell(
                if st.any_falsify() { "⚠ 存在证伪旗".into() } else { "".into() },
                100.0,
                crate::ui::pal::down(),
            ),
        ]
        .spacing(4),
    );
    body = body.push(text(format!("刷新 {}", st.refreshed)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
