//! 预测市场 pane 的视图渲染（docs/19）。
//!
//! **独立新增面板**（`Content::PredictionBoard`）——不改任何既有面板；数据走
//! [`super::prediction_readout`] 旁路快照。
//!
//! 布局：夜跑控制条（启停+定时开关，[`super::prediction::PredictionMsg`]）→ 合规/非信号横幅
//! → 市场列表（Yes 概率/成交/流动性/关注标签）+ AI 决策支持行（估计 vs 市场·edge·置信度）。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::prediction::PredictionMsg;
use super::prediction_readout::{CalibrationView, MarketRow, PredictionReadout};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_GREEN: Color = Color::from_rgb(0.45, 0.85, 0.5);
const C_RED: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);

fn sign_c(v: f64) -> Color {
    if v >= 0.0 {
        C_GREEN
    } else {
        C_RED
    }
}
fn sec<'a, M: 'a>(title: &str) -> Element<'a, M> {
    text(title.to_string()).size(14).color(C_HEAD).into()
}
fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(11).color(c)).width(Length::Fixed(w)).into()
}
fn pct(p: Option<f64>) -> String {
    p.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "—".into())
}
fn usd(v: f64) -> String {
    if v >= 1e6 {
        format!("${:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("${:.0}k", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

fn brier(b: Option<f64>) -> String {
    b.map(|x| format!("{x:.3}")).unwrap_or_else(|| "—".into())
}

/// AI 校准追踪摘要区（docs/19 §5）：AI Brier vs 市场 Brier + 校准曲线（越低越好）。
fn calib_block<'a, M: 'a>(c: &CalibrationView) -> Element<'a, M> {
    let mut col = column![sec("AI 校准追踪 · Brier 越低越准（唯有 AI<市场 才可能有边际）")].spacing(3);

    if c.n_resolved == 0 {
        col = col.push(
            text(format!(
                "已记录 {} 条估计 · 待市场结算积累样本（现 0 已结算）——夜跑 --resolve 自动回填",
                c.n_total
            ))
            .size(11)
            .color(C_DIM),
        );
        return col.into();
    }

    let verdict = if c.ai_beats_market {
        ("✅ AI 胜过市场", C_GREEN)
    } else {
        ("❌ AI 未胜市场（分歧=噪声·勿据以下注）", C_RED)
    };
    col = col.push(
        row![
            cell(format!("已结算 {}/{}", c.n_resolved, c.n_total), 100.0, C_TXT),
            cell(format!("AI Brier {}", brier(c.ai_brier)), 110.0, C_TXT),
            cell(format!("市场 {}", brier(c.market_brier)), 100.0, C_DIM),
            cell(verdict.0.into(), 240.0, verdict.1),
        ]
        .spacing(4),
    );

    // 校准曲线：预测区间 vs 实际发生率（完美=对角线）
    if !c.bins.is_empty() {
        col = col.push(text("校准曲线（区间 · 样本 · 均预测 → 实际）").size(10).color(C_HEAD));
        for b in &c.bins {
            let gap = (b.mean_pred - b.actual).abs();
            let cc = if gap <= 0.1 { C_GREEN } else { C_GOLD };
            col = col.push(
                row![
                    cell(format!("[{:.1},{:.1})", b.lo, b.hi), 80.0, C_DIM),
                    cell(format!("n={}", b.n), 46.0, C_DIM),
                    cell(format!("预{:.0}%", b.mean_pred * 100.0), 60.0, C_DIM),
                    cell(format!("实{:.0}%", b.actual * 100.0), 60.0, cc),
                ]
                .spacing(4),
            );
        }
    }
    col.into()
}

fn market_block<'a, M: 'a>(m: &MarketRow) -> Element<'a, M> {
    let mark = if m.watch { "★" } else { " " };
    let mut col = column![
        row![
            cell(mark.into(), 16.0, C_GOLD),
            cell(format!("Yes {}", pct(m.yes_prob)), 70.0, C_TXT),
            cell(format!("成交 {}", usd(m.volume)), 90.0, C_DIM),
            cell(format!("流动 {}", usd(m.liquidity)), 90.0, C_DIM),
            cell(m.tags.join("·"), 260.0, C_DIM),
        ]
        .spacing(4),
        text(m.question.clone()).size(11).color(C_TXT),
    ]
    .spacing(2);

    // AI 决策支持（若有）：估计 vs 市场·edge·置信度·推理（非投注信号）
    if let Some(ai) = &m.ai {
        col = col.push(
            row![
                cell("AI".into(), 26.0, C_HEAD),
                cell(format!("估计 {:.0}%", ai.ai_prob * 100.0), 78.0, C_TXT),
                cell(format!("市场 {}", pct(ai.market_prob)), 78.0, C_DIM),
                cell(
                    ai.edge
                        .map(|e| format!("Δ {:+.0}%", e * 100.0))
                        .unwrap_or_else(|| "Δ —".into()),
                    70.0,
                    ai.edge.map(sign_c).unwrap_or(C_DIM),
                ),
                cell(format!("置信 {}", ai.confidence), 80.0, C_DIM),
            ]
            .spacing(4),
        );
        if !ai.rationale.is_empty() {
            col = col.push(text(format!("  ↳ {}", ai.rationale)).size(10).color(C_DIM));
        }
    }
    col.into()
}

pub fn pane_body<'a>() -> Element<'a, PredictionMsg> {
    let st: PredictionReadout = super::prediction_readout::snapshot();
    let mut body = column![].spacing(8).padding(10);

    body = body.push(sec("预测市场 · Polymarket（docs/19 · 决策支持·非投注信号）"));

    // ── 夜跑启停（不随开机自启，全由这里控制；状态来自 poller，非每帧查） ──
    // 放在下面「暂无快照」的提前返回之前：没数据时正是最需要点「立即运行」的时候。
    {
        let running = st.svc.active;
        // 状态行：夜跑是 oneshot，「重启次数」无意义，看的是上次跑没跑成。
        let (dot, dotc, run) = if running {
            ("●", C_GREEN, format!("运行中  已 {}", super::svcctl::fmt_dur(st.svc.uptime_secs)))
        } else if st.svc.ever_ran() {
            let ok = st.svc.last_ok();
            (
                if ok { "○" } else { "✗" },
                if ok { C_DIM } else { C_RED },
                format!(
                    "空闲  上次 {} {}",
                    super::svcctl::fmt_stamp(&st.svc.last_finish),
                    if ok { "成功".into() } else { format!("失败（{}）", st.svc.last_result) }
                ),
            )
        } else if !st.stamp.is_empty() {
            // systemd 的 ExecMainExitTimestamp 会被 `systemctl stop` / `reset-failed` 清空，
            // 那时 unit 已无「上次何时跑」的记录。用产物（board 快照）的时间戳兜底，
            // 免得明明跑过却显示「未跑过」。
            ("○", C_DIM, format!("空闲  快照 {}", st.stamp))
        } else {
            ("○", C_DIM, "空闲  未跑过".to_string())
        };
        body = body.push(
            row![
                text(format!("{dot} ")).size(14).color(dotc),
                text(run).size(12).color(C_TXT),
                text(if st.timer_enabled && !st.timer_next.is_empty() {
                    format!("　下次定时 {}", st.timer_next)
                } else if st.timer_enabled {
                    "　每日定时开".to_string()
                } else {
                    "　每日定时已关".to_string()
                })
                .size(11)
                .color(C_DIM),
            ]
            .align_y(iced::Alignment::Center),
        );
        // 动作按钮常驻、当前状态那个置灰（灰掉的=现在就是这个态），不用切换式按钮
        // ——切换式按钮上写「每日定时 开」时，分不清是「当前开」还是「点了会开」。
        let ctl = row![
            text("运行 ").size(11).color(C_DIM),
            button(text("▶ 立即运行").size(11))
                .padding([2, 8])
                .on_press_maybe((!running).then_some(PredictionMsg::RunNightly)),
            button(text("■ 停止").size(11))
                .padding([2, 8])
                .on_press_maybe(running.then_some(PredictionMsg::StopNightly)),
            text("　每日定时 ").size(11).color(C_DIM),
            button(text(if st.timer_enabled { "✔ 开" } else { "开" }).size(11))
                .padding([2, 8])
                .on_press_maybe((!st.timer_enabled).then_some(PredictionMsg::SetTimer(true))),
            button(text(if st.timer_enabled { "关" } else { "✔ 关" }).size(11))
                .padding([2, 8])
                .on_press_maybe(st.timer_enabled.then_some(PredictionMsg::SetTimer(false))),
            text("　").size(11),
            button(text("⟳ 刷新").size(11)).padding([2, 8]).on_press(PredictionMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(10).color(C_DIM),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        body = body.push(ctl);
        // 操作反馈单独一行：和按钮挤一行会溢出换行、把按钮挤变形（同 factory 面板）
        let am = super::prediction::action_message();
        if !am.is_empty() {
            let bad = am.starts_with('✗');
            body = body.push(text(am).size(10).color(if bad { C_RED } else { C_GREEN }));
        }
    }

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无快照——点上方「▶ 立即运行」生成（夜跑含 AI 校准+回填结算）")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    body = body.push(
        text("⚠ 预测市场校准良好·AI 分歧仅信息性·默认只读不自动下注（合规：Polymarket 加密真金·美国受限）")
            .size(10)
            .color(C_GOLD),
    );
    if st.ai_enabled {
        body = body.push(
            text(format!("AI 决策支持已开 · 已分析 {} 个关注档市场", st.ai_analyzed))
                .size(10)
                .color(C_DIM),
        );
    }

    if let Some(c) = &st.calib {
        body = body.push(calib_block(c));
    }

    let watch_n = st.rows.iter().filter(|r| r.watch).count();
    body = body.push(
        text(format!("关注档 {}/{} · ★=流动+活跃+不确定", watch_n, st.rows.len()))
            .size(11)
            .color(C_HEAD),
    );

    // 「近乎已定」的市场（Yes ≥95% 或 ≤5%）没有分析价值，却因成交额大排在前面，
    // 实测把唯一 ★ 标的可分析市场淹没在 13+ 条已定市场里。
    // **重排而不隐藏**：可分析的提到前面，已定的移到分隔线之后——信息一条不少。
    // 判据用 yes_prob 而非匹配标签字符串（标签文案变了就失效）。
    let decided = |m: &MarketRow| m.yes_prob.is_some_and(|p| !(0.05..=0.95).contains(&p));
    let (settled, open_): (Vec<_>, Vec<_>) = st.rows.iter().partition(|m| decided(m));
    // 可分析的里面 ★ 关注档再提到最前
    let mut open_sorted = open_;
    open_sorted.sort_by_key(|m| !m.watch);
    for m in &open_sorted {
        body = body.push(market_block(m));
    }
    if !settled.is_empty() {
        body = body.push(
            text(format!(
                "── 以下 {} 个近乎已定（Yes ≥95% 或 ≤5%），无分析价值，仅列出 ──",
                settled.len()
            ))
            .size(10)
            .color(C_DIM),
        );
        for m in &settled {
            body = body.push(market_block(m));
        }
    }

    body = body.push(
        text(format!(
            "源 {} · 快照 {}{} · 刷新 {}",
            st.source,
            st.stamp,
            super::staleness::suffix(&st.stamp),
            st.refreshed
        ))
            .size(10)
            .color(C_DIM),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
