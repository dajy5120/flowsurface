//! Alpha Factory pane 的视图渲染（docs/08 F6 — P2）。
//!
//! 把独立 `wealthspring-factory-gui` 的 9 面板仪表盘移植为 FlowSurface 的 `Content::Factory`
//! pane:总览 · Stage-A 排行(F2/F3) · Stage-B(F5) · 现役池(F4) · 组合(F4) · 影子实盘(F6)
//! · 数据底座(F0/F1) · Nightly(F7)。数据走 [`super::factory_readout`] 旁路快照。
//!
//! 本模块只渲染、不发消息,对 pane 的消息类型 `M` 泛型。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::factory_readout::FactoryReadout;

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_GREEN: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_PURPLE: Color = Color::from_rgb(0.8, 0.6, 0.9);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_RED: Color = Color::from_rgb(0.9, 0.45, 0.4);

fn vgap<'a, M: 'a>(h: f32) -> Element<'a, M> {
    container(text("")).height(Length::Fixed(h)).into()
}
fn sec<'a, M: 'a>(title: &str, c: Color) -> Element<'a, M> {
    text(title.to_string()).size(15).color(c).into()
}
fn cell<'a, M: 'a>(s: &str, w: f32) -> Element<'a, M> {
    container(text(s.to_string()).size(11))
        .width(Length::Fixed(w))
        .into()
}
fn cellc<'a, M: 'a>(s: &str, w: f32, c: Color) -> Element<'a, M> {
    container(text(s.to_string()).size(11).color(c))
        .width(Length::Fixed(w))
        .into()
}
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
fn group(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
fn src_color(s: &str) -> Color {
    match s {
        "seed" => Color::from_rgb(0.5, 0.8, 0.6),
        "gp" => Color::from_rgb(0.9, 0.7, 0.4),
        "llm" => Color::from_rgb(0.7, 0.6, 0.95),
        "combo" => Color::from_rgb(0.6, 0.85, 0.95),
        _ => C_DIM,
    }
}
fn fmt_bp(v: f64) -> String {
    if v.is_finite() {
        format!("{v:+.2}")
    } else {
        "—".into()
    }
}

/// 渲染 Alpha Factory 仪表盘（9 面板,3 列）。
pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let st: FactoryReadout = super::factory_readout::snapshot();

    // —— 顶部总览 ——
    let status_s: String = st
        .status_counts
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let gensrc_s: String = st
        .gensrc_counts
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let thr_s: String = st
        .thresholds
        .iter()
        .filter(|(k, _)| k.starts_with("stage_a") || k.starts_with("cost"))
        .map(|(k, v)| format!("{}={v}", k.rsplit('.').next().unwrap_or(k)))
        .collect::<Vec<_>>()
        .join("  ");
    let status_line: Element<M> = if !st.started {
        text("启动 Factory poller…").size(13).color(C_DIM).into()
    } else if st.db_ok {
        text(format!(
            "alphas 状态  {status_s}      evals {}   全局 trials {}   combos {}      刷新 {}",
            group(st.n_evals),
            group(st.n_trials),
            st.n_combos,
            st.refreshed
        ))
        .size(13)
        .color(C_GREEN)
        .into()
    } else {
        text(format!(
            "✗ 读不到 Registry（{}）——工厂尚未产出或路径不对",
            super::factory_readout::db_path_display()
        ))
        .size(13)
        .color(C_RED)
        .into()
    };
    let header = column![
        text("Alpha Factory 驾驶舱").size(22).color(C_HEAD),
        status_line,
        text(format!("生成来源  {gensrc_s}")).size(12).color(C_DIM),
        text(format!("晋级阈值  {thr_s}")).size(11).color(C_DIM),
    ]
    .spacing(5);

    // —— 左列：Stage-A 排行 + Stage-B ——
    let mut sa = column![sec("② Stage-A 排行（按 |IC t|，F2/F3）", C_GREEN)].spacing(2);
    sa = sa.push(
        row![
            cell("源", 46.0),
            cell("视界", 50.0),
            cell("IC", 64.0),
            cell("t", 56.0),
            cell("同号", 44.0),
            cell("净bp", 60.0),
            cell("表达式", 300.0)
        ]
        .spacing(4),
    );
    for a in &st.stage_a {
        let leak = if a.leakage { " 🚨" } else { "" };
        sa = sa.push(
            row![
                cellc(&a.gen_src, 46.0, src_color(&a.gen_src)),
                cell(&a.horizon, 50.0),
                cell(&format!("{:+.3}", a.ic_mean), 64.0),
                cell(&format!("{:+.1}", a.ic_t), 56.0),
                cell(&format!("{}/{}", a.folds_same, a.n_folds), 44.0),
                cell(&fmt_bp(a.net_bp), 60.0),
                cell(&format!("{}{leak}", trunc(&a.expr, 40)), 300.0),
            ]
            .spacing(4),
        );
    }
    let mut sb = column![sec("⑤ Stage-B 事件回测（Nautilus，F5）", C_GREEN)].spacing(2);
    for b in &st.stage_b {
        sb = sb.push(
            row![
                cell(&format!("{}笔", b.n_entries), 50.0),
                cell(&fmt_bp(b.net_bp), 70.0),
                cell(&trunc(&b.expr, 50), 400.0),
            ]
            .spacing(4),
        );
    }
    if st.stage_b.is_empty() {
        sb = sb.push(text("（暂无 Stage-B 记录）").size(11).color(C_DIM));
    }
    let left = column![sa, vgap(10.0), sb]
        .spacing(4)
        .width(Length::FillPortion(5));

    // —— 中列：现役池 + 组合 ——
    let mut pool = column![sec(
        &format!("③ 现役池（去相关后 {} 条，F4）", st.pool.len()),
        C_GOLD
    )]
    .spacing(2);
    pool = pool.push(
        row![
            cell("权重", 60.0),
            cell("簇", 36.0),
            cell("|t|", 50.0),
            cell("表达式", 280.0)
        ]
        .spacing(4),
    );
    for p in &st.pool {
        pool = pool.push(
            row![
                cell(&format!("{:+.3}", p.weight), 60.0),
                cell(&p.cluster.to_string(), 36.0),
                cell(&format!("{:.1}", p.ic_t), 50.0),
                cell(&trunc(&p.expr, 38), 280.0),
            ]
            .spacing(4),
        );
    }
    let mut combos = column![sec("④ 组合（ICIR/Ridge 双法，F4）", C_GOLD)].spacing(2);
    combos = combos.push(
        row![
            cell("方法", 90.0),
            cell("IC", 64.0),
            cell("t", 56.0),
            cell("净bp", 64.0),
            cell("状态", 70.0)
        ]
        .spacing(4),
    );
    for c in &st.combos {
        combos = combos.push(
            row![
                cell(&c.method, 90.0),
                cell(&format!("{:+.3}", c.ic_mean), 64.0),
                cell(&format!("{:+.1}", c.ic_t), 56.0),
                cell(&fmt_bp(c.net_bp), 64.0),
                cellc(
                    &c.status,
                    70.0,
                    if c.status == "paper" { C_GREEN } else { C_DIM }
                ),
            ]
            .spacing(4),
        );
    }
    let mid = column![pool, vgap(10.0), combos]
        .spacing(4)
        .width(Length::FillPortion(4));

    // —— 右列：影子实盘 + 数据底座 + Nightly ——
    let mut live = column![sec("⑥ 影子实盘 / realized IC（F6）", C_PURPLE)].spacing(2);
    for l in &st.live {
        live = live.push(
            text(format!(
                "{} realized IC@1s {:+.3}  PnL {:+.2}  成交{}  {}",
                l.symbol, l.realized_ic_1s, l.pnl, l.n_trades, l.age
            ))
            .size(11)
            .color(Color::from_rgb(0.72, 0.74, 0.8)),
        );
    }
    if st.live.is_empty() {
        live = live.push(text("（暂无影子/实盘记录——跑 shadow_run）").size(11).color(C_DIM));
    }
    let mut lake = column![sec("① 数据底座（F0/F1）", C_HEAD)].spacing(2);
    lake = lake.push(
        row![
            cell("币种", 90.0),
            cell("录制天", 64.0),
            cell("特征帧", 64.0),
            cell("信号帧", 64.0)
        ]
        .spacing(4),
    );
    for r in &st.lake {
        lake = lake.push(
            row![
                cell(&r.symbol, 90.0),
                cell(&r.raw_days.to_string(), 64.0),
                cell(&r.feat_frames.to_string(), 64.0),
                cell(&r.sig_frames.to_string(), 64.0),
            ]
            .spacing(4),
        );
    }
    let mut nightly = column![sec("⑦ Nightly 流水线（F7）", C_PURPLE)].spacing(2);
    // 实时进度（Redis `factory:progress`）：run 进行中即可见，先于 md 报告落盘。
    let lv = &st.nightly_live;
    if lv.seen {
        let hc = if lv.running {
            Color::from_rgb(0.45, 0.85, 0.5)
        } else if lv.header.starts_with('❌') {
            Color::from_rgb(0.9, 0.45, 0.4)
        } else {
            Color::from_rgb(0.6, 0.75, 0.95)
        };
        nightly = nightly.push(text(format!("{} · {}", lv.date, lv.header)).size(11).color(hc));
        let start = lv.steps.len().saturating_sub(10);
        for (step, rc, secs) in &lv.steps[start..] {
            let mark = if *rc == 0 { "✅" } else { "❌" };
            let c = if *rc == 0 { C_DIM } else { Color::from_rgb(0.9, 0.45, 0.4) };
            nightly = nightly.push(
                text(format!("{mark} {} · {secs:.0}s", trunc(step, 38))).size(10).color(c),
            );
        }
        nightly = nightly.push(vgap(6.0));
    }
    nightly = nightly.push(
        text(trunc(&st.nightly_title, 60))
            .size(12)
            .color(Color::from_rgb(0.8, 0.8, 0.85)),
    );
    for l in st.nightly_lines.iter().take(16) {
        nightly = nightly.push(text(trunc(l, 56)).size(10).color(C_DIM));
    }
    let right = column![live, vgap(10.0), lake, vgap(10.0), nightly]
        .spacing(4)
        .width(Length::FillPortion(4));

    let body = row![
        scrollable(left).height(Length::Fill),
        scrollable(mid).height(Length::Fill),
        scrollable(right).height(Length::Fill),
    ]
    .spacing(16)
    .height(Length::Fill);

    container(column![header, vgap(10.0), body].spacing(8))
        .padding(14)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
