//! Alpha Factory pane 的视图渲染（docs/08 F6 — P2）。
//!
//! 把独立 `wealthspring-factory-gui` 的 9 面板仪表盘移植为 FlowSurface 的 `Content::Factory`
//! pane:总览 · Stage-A 排行(F2/F3) · Stage-B(F5) · 现役池(F4) · 组合(F4) · 影子实盘(F6)
//! · 数据底座(F0/F1) · Nightly(F7)。数据走 [`super::factory_readout`] 旁路快照。
//!
//! 本模块只渲染、不发消息,对 pane 的消息类型 `M` 泛型。

use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme, mouse};

use super::factory_readout::{FactoryReadout, HORIZONS, IcDecay};

fn vgap<'a, M: 'a>(h: f32) -> Element<'a, M> {
    container(text("")).height(Length::Fixed(h)).into()
}
fn sec<'a, M: 'a>(title: &str, c: Color) -> Element<'a, M> {
    text(title.to_string()).size(crate::ui::text::s_section()).color(c).into()
}
/// 缺失值一律显示 `—`，**不要退化成 0 或 NaN**。
/// 「没记录」和「值是 0」是两回事：实测同一批影子 run，C4 面板显示 fills 711，
/// 而 Factory 因 `unwrap_or(0)` 显示「成交0」——两个面板对同一事实互相矛盾（docs/20 §23）。
fn opt_f(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{x:+.prec$}"),
        _ => "—".to_string(),
    }
}

fn cell<'a, M: 'a>(s: &str, w: f32) -> Element<'a, M> {
    container(text(s.to_string()).size(crate::ui::text::s_small()))
        .width(Length::Fixed(w))
        .into()
}
fn cellc<'a, M: 'a>(s: &str, w: f32, c: Color) -> Element<'a, M> {
    container(text(s.to_string()).size(crate::ui::text::s_small()).color(c))
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
        "seed" => crate::ui::pal::ok(),
        "gp" => crate::ui::pal::warn(),
        "llm" => crate::ui::pal::series(5),
        "combo" => crate::ui::pal::info(),
        _ => crate::ui::pal::dim(),
    }
}
fn fmt_bp(v: f64) -> String {
    if v.is_finite() {
        format!("{v:+.2}")
    } else {
        "—".into()
    }
}

/// Stage-A 表达式格：有机理假设的（seed/llm）前缀「ⓘ」并悬浮显示假设；gp 子代多无假设。
fn expr_cell<'a, M: 'a>(expr: &str, hypothesis: &str, leak: &str, w: f32) -> Element<'a, M> {
    let has = !hypothesis.trim().is_empty();
    let label = if has {
        format!("ⓘ {}{leak}", trunc(expr, 37))
    } else {
        format!("{}{leak}", trunc(expr, 40))
    };
    let base = container(text(label).size(crate::ui::text::s_small())).width(Length::Fixed(w));
    if has {
        iced::widget::tooltip(
            base,
            container(text(format!("假设：{hypothesis}")).size(crate::ui::text::s_small()))
                .style(crate::style::tooltip)
                .padding(crate::ui::metrics::space(3))
                .max_width(360.0),
            iced::widget::tooltip::Position::Top,
        )
        .into()
    } else {
        base.into()
    }
}


/// IC 衰减曲线：x=视界（500ms→5m 等距），y=ic_mean（含 0 基线），每 alpha 一条线。
struct DecayChart {
    lines: Vec<IcDecay>,
    cache: Cache,
}

impl<M> canvas::Program<M> for DecayChart {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, _c: mouse::Cursor) -> Vec<Geometry> {
        const ML: f32 = 44.0;
        const MB: f32 = 16.0;
        const MT: f32 = 6.0;
        const MR: f32 = 8.0;
        let axis = crate::ui::pal::pend();
        let grid = crate::ui::pal::alpha(crate::ui::pal::dim(), 0.2);
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if self.lines.is_empty() {
                frame.fill_text(Text {
                    content: "数据不足（尚无 Stage-A 评估）".into(),
                    position: Point::new(ML + 4.0, h / 2.0 - 6.0),
                    color: crate::ui::pal::dim(),
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            // y 轴范围：所有点 + 0 基线
            let mut lo = 0.0_f64;
            let mut hi = 0.0_f64;
            for l in &self.lines {
                for (_, v) in &l.pts {
                    lo = lo.min(*v);
                    hi = hi.max(*v);
                }
            }
            if (hi - lo).abs() < 1e-9 {
                hi = lo + 0.01;
            }
            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let nx = HORIZONS.len() as f32;
            let mx = |i: usize| ML + (i as f32) / ((nx - 1.0).max(1.0)) * pw;
            let my = |v: f64| MT + ((hi - v) / (hi - lo)) as f32 * ph;

            // 网格 + y 刻度
            for k in 0..=4 {
                let v = lo + (hi - lo) * (k as f64) / 4.0;
                let y = my(v);
                frame.stroke(
                    &Path::line(Point::new(ML, y), Point::new(ML + pw, y)),
                    Stroke::default().with_width(1.0).with_color(grid),
                );
                frame.fill_text(Text {
                    content: format!("{v:+.2}"),
                    position: Point::new(2.0, y - 5.0),
                    color: axis,
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
            // 0 基线加重
            let zy = my(0.0);
            frame.stroke(
                &Path::line(Point::new(ML, zy), Point::new(ML + pw, zy)),
                Stroke::default().with_width(1.2).with_color(axis),
            );
            // x 轴视界标签
            for (i, hl) in HORIZONS.iter().enumerate() {
                frame.fill_text(Text {
                    content: (*hl).to_string(),
                    position: Point::new((mx(i) - 10.0).max(0.0), h - MB + 2.0),
                    color: axis,
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
            // 每 alpha 一条折线 + 顶点圆点
            for (li, l) in self.lines.iter().enumerate() {
                let col = crate::ui::pal::series(li);
                if l.pts.len() >= 2 {
                    let path = Path::new(|p| {
                        p.move_to(Point::new(mx(l.pts[0].0), my(l.pts[0].1)));
                        for (i, v) in l.pts.iter().skip(1) {
                            p.line_to(Point::new(mx(*i), my(*v)));
                        }
                    });
                    frame.stroke(&path, Stroke::default().with_width(1.6).with_color(col));
                }
                for (i, v) in &l.pts {
                    frame.fill(
                        &Path::circle(Point::new(mx(*i), my(*v)), 2.0),
                        col,
                    );
                }
            }
        });
        vec![geo]
    }
}

/// 渲染 Alpha Factory 仪表盘（9 面板,3 列）。
pub fn pane_body<'a>() -> Element<'a, super::factory::FactoryMsg> {
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
    let status_line: Element<super::factory::FactoryMsg> = if !st.started {
        text("启动 Factory poller…").size(crate::ui::text::s_emph()).color(crate::ui::pal::dim()).into()
    } else if st.db_ok {
        text(format!(
            "alphas 状态  {status_s}      evals {}   全局 trials {}   combos {}      刷新 {}",
            group(st.n_evals),
            group(st.n_trials),
            st.n_combos,
            st.refreshed
        ))
        .size(crate::ui::text::s_emph())
        .color(crate::ui::pal::up())
        .into()
    } else {
        text(format!(
            "✗ 读不到 Registry（{}）——工厂尚未产出或路径不对",
            super::factory_readout::db_path_display()
        ))
        .size(crate::ui::text::s_emph())
        .color(crate::ui::pal::down())
        .into()
    };
    let header = column![
        text("Alpha Factory 驾驶舱").size(crate::ui::text::s_title()).color(crate::ui::pal::head()),
        status_line,
        text(format!("生成来源  {gensrc_s}")).size(crate::ui::text::s_body()).color(crate::ui::pal::dim()),
        text(format!("晋级阈值  {thr_s}")).size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
    ]
    .spacing(5);

    // —— 左列：Stage-A 排行 + Stage-B ——
    let mut sa = column![sec("② Stage-A 排行（按 |IC t|，F2/F3）", crate::ui::pal::up())].spacing(2);
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
                expr_cell(&a.expr, &a.hypothesis, leak, 300.0),
            ]
            .spacing(4),
        );
    }
    // IC 衰减曲线（top6 alpha 的跨视界 IC 廓线）+ 配色图例
    let mut decay_legend = column![].spacing(1);
    for (i, l) in st.ic_decay.iter().enumerate() {
        let col = crate::ui::pal::series(i);
        decay_legend = decay_legend.push(
            text(format!("● [{}] {}", l.gen_src, trunc(&l.expr, 44))).size(crate::ui::text::s_meta()).color(col),
        );
    }
    let decay = column![
        sec("IC 衰减曲线（top6 · 视界 500ms→5m，F2）", crate::ui::pal::up()),
        container(
            Canvas::new(DecayChart { lines: st.ic_decay.clone(), cache: Cache::new() })
                .width(Length::Fill)
                .height(Length::Fixed(150.0))
        ),
        decay_legend,
    ]
    .spacing(3);
    let mut sb = column![sec("⑤ Stage-B 事件回测（Nautilus，F5）", crate::ui::pal::up())].spacing(2);
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
        sb = sb.push(text("（暂无 Stage-B 记录）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
    }
    let left = column![sa, vgap(8.0), decay, vgap(10.0), sb]
        .spacing(4)
        .width(Length::FillPortion(5));

    // —— 中列：现役池 + 组合 ——
    let mut pool = column![sec(
        &format!("③ 现役池（去相关后 {} 条，F4）", st.pool.len()),
        crate::ui::pal::warn()
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
                cell(&opt_f(p.weight, 3), 60.0),
                cell(&p.cluster.to_string(), 36.0),
                cell(&format!("{:.1}", p.ic_t), 50.0),
                cell(&trunc(&p.expr, 38), 280.0),
            ]
            .spacing(4),
        );
    }
    let mut combos = column![sec("④ 组合（ICIR/Ridge 双法，F4）", crate::ui::pal::warn())].spacing(2);
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
                    if c.status == "paper" { crate::ui::pal::up() } else { crate::ui::pal::dim() }
                ),
            ]
            .spacing(4),
        );
    }
    let mid = column![pool, vgap(10.0), combos]
        .spacing(4)
        .width(Length::FillPortion(4));

    // —— 右列：影子实盘 + 数据底座 + Nightly ——
    let mut live = column![sec("⑥ 影子实盘 / realized IC（F6）", crate::ui::pal::series(5))].spacing(2);
    for l in &st.live {
        live = live.push(
            text(format!(
                "{} realized IC@1s {}  PnL {}  成交{}  {}",
                l.symbol,
                opt_f(l.realized_ic_1s, 3),
                opt_f(l.pnl, 2),
                l.n_trades.map_or_else(|| "—".to_string(), |n| n.to_string()),
                l.age
            ))
            .size(crate::ui::text::s_small())
            .color(crate::ui::pal::txt()),
        );
    }
    if st.live.is_empty() {
        live = live.push(text("（暂无影子/实盘记录——跑 shadow_run）").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
    }
    let mut lake = column![sec("① 数据底座（F0/F1）", crate::ui::pal::head())].spacing(2);
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
    let mut nightly = column![sec("⑦ Nightly 流水线（F7）", crate::ui::pal::series(5))].spacing(2);
    // 手动启停（docs/20 §26）。定时器与手动运行相互独立：关了定时器仍可手动跑。
    {
        use super::factory::FactoryMsg;
        let running = st.svc.active;
        // 状态行：nightly 是 oneshot，「重启次数」无意义，看的是上次跑没跑成。
        let (dot, dotc, run) = if running {
            ("●", crate::ui::pal::up(), format!("运行中  已 {}", super::svcctl::fmt_dur(st.svc.uptime_secs)))
        } else if st.svc.ever_ran() {
            let ok = st.svc.last_ok();
            (
                if ok { "○" } else { "✗" },
                if ok { crate::ui::pal::dim() } else { crate::ui::pal::down() },
                format!(
                    "空闲  上次 {} {}",
                    super::svcctl::fmt_stamp(&st.svc.last_finish),
                    if ok { "成功".into() } else { format!("失败（{}）", st.svc.last_result) }
                ),
            )
        } else {
            ("○", crate::ui::pal::dim(), "空闲  未跑过".to_string())
        };
        nightly = nightly.push(
            row![
                text(format!("{dot} ")).size(crate::ui::text::s_emph()).color(dotc),
                text(run).size(crate::ui::text::s_small()).color(crate::ui::pal::txt()),
                text(if st.timer_enabled && !st.timer_next.is_empty() {
                    format!("　下次定时 {}", st.timer_next)
                } else if st.timer_enabled {
                    "　每日定时开".to_string()
                } else {
                    "　每日定时已关".to_string()
                })
                .size(crate::ui::text::s_meta())
                .color(crate::ui::pal::dim()),
            ]
            .align_y(iced::Alignment::Center),
        );
        // 动作按钮常驻、当前状态那个置灰（灰掉的=现在就是这个态），不用切换式按钮
        // ——切换式按钮上写「每日定时 开」时，分不清是「当前开」还是「点了会开」。
        let ctl = row![
            text("运行 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
            button(text("▶ 立即运行").size(crate::ui::text::s_small()))
                .padding(crate::ui::metrics::pad2(0, 3))
                .on_press_maybe((!running).then_some(FactoryMsg::RunNightly)),
            button(text("■ 停止").size(crate::ui::text::s_small()))
                .padding(crate::ui::metrics::pad2(0, 3))
                .on_press_maybe(running.then_some(FactoryMsg::StopNightly)),
            text("　每日定时 ").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()),
            button(text(if st.timer_enabled { "✔ 开" } else { "开" }).size(crate::ui::text::s_small()))
                .padding(crate::ui::metrics::pad2(0, 3))
                .on_press_maybe((!st.timer_enabled).then_some(FactoryMsg::SetTimer(true))),
            button(text(if st.timer_enabled { "关" } else { "✔ 关" }).size(crate::ui::text::s_small()))
                .padding(crate::ui::metrics::pad2(0, 3))
                .on_press_maybe(st.timer_enabled.then_some(FactoryMsg::SetTimer(false))),
            text("　").size(crate::ui::text::s_small()),
            button(text("⟳ 刷新").size(crate::ui::text::s_small())).padding(crate::ui::metrics::pad2(0, 3)).on_press(FactoryMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        nightly = nightly.push(ctl);
        // 操作反馈单独一行：和按钮挤一行会溢出换行、把按钮挤变形
        let am = super::factory::action_message();
        if !am.is_empty() {
            let bad = am.starts_with('✗');
            nightly = nightly.push(text(am).size(crate::ui::text::s_meta()).color(if bad {
                crate::ui::pal::bad()
            } else {
                crate::ui::pal::ok()
            }));
        }
    }
    // 实时进度（Redis `factory:progress`）：run 进行中即可见，先于 md 报告落盘。
    let lv = &st.nightly_live;
    if lv.seen {
        let hc = if lv.running {
            crate::ui::pal::ok()
        } else if lv.header.starts_with('❌') {
            crate::ui::pal::bad()
        } else {
            crate::ui::pal::info()
        };
        nightly = nightly.push(text(format!("{} · {}", lv.date, lv.header)).size(crate::ui::text::s_small()).color(hc));
        let start = lv.steps.len().saturating_sub(10);
        for (step, rc, secs) in &lv.steps[start..] {
            let mark = if *rc == 0 { "✅" } else { "❌" };
            let c = if *rc == 0 { crate::ui::pal::dim() } else { crate::ui::pal::bad() };
            nightly = nightly.push(
                text(format!("{mark} {} · {secs:.0}s", trunc(step, 38))).size(crate::ui::text::s_meta()).color(c),
            );
        }
        nightly = nightly.push(vgap(6.0));
    }
    nightly = nightly.push(
        text(trunc(&st.nightly_title, 60))
            .size(crate::ui::text::s_body())
            .color(crate::ui::pal::txt()),
    );
    for l in st.nightly_lines.iter().take(16) {
        nightly = nightly.push(text(trunc(l, 56)).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
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
        .padding(crate::ui::metrics::space(4))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
