//! 录制驾驶舱 pane 的视图渲染（docs/08 F6 — P3）。
//!
//! 移植 `wealthspring-recorder-gui` 的界面:服务控制(启停/重启)、保存位置 + 币种/档位编辑、
//! 录制实况、已录总览。可编辑状态来自 [`super::recorder::RecorderPaneState`],只读服务/数据
//! 状态来自 [`super::recorder_readout`]。view 发出 [`RecorderMsg`],pane.rs 包成
//! `Message::PaneEvent(id, Event::RecorderInteraction(..))`。

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Color, Element, Length};

use super::recorder::{ALL, DET_PAGE, GOAL_DAYS, RecorderMsg, RecorderPaneState, TierOpt};

fn fmt_dur(s: i64) -> String {
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}天{h}时{m}分")
    } else if h > 0 {
        format!("{h}时{m}分")
    } else {
        format!("{m}分{}秒", s % 60)
    }
}
fn fmt_size(b: u64) -> String {
    let gb = b as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.2} GB")
    } else {
        format!("{:.1} MB", b as f64 / 1e6)
    }
}
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
fn cell<'a>(s: &str, w: f32) -> Element<'a, RecorderMsg> {
    container(text(s.to_string()).size(12))
        .width(Length::Fixed(w))
        .into()
}
fn vgap<'a>(h: f32) -> Element<'a, RecorderMsg> {
    container(text("")).height(Length::Fixed(h)).into()
}

/// ④ 录制明细：逐（币种 × 数据类型 × 日期）列出已落盘的内容。
///
/// ③ 只到「每币种多少天多少 GB」，回答不了「BTC 的 L2 到底录了哪几天、那天几点到几点、
/// 文件在哪」。这一节补的就是那个粒度。
///
/// 两列值得解释：
/// - **时段**是段起点的首末（UTC），不是覆盖区间——末段本身还覆盖之后约 10 分钟，
///   且中间可能有断流空洞。要判断完整性看 F0 验收，不要拿这两个数相减。
/// - **未封档/孤儿**是读不了但占盘的文件（writer.rs）：前者正在写（当天通常 1 个，正常），
///   后者是上次崩溃遗留、已改名留证（出现即说明崩过）。大小列把它们算进去了——
///   那是真实磁盘占用。
///
/// 返回 `Element<'static>`：表里每个格子都是 `to_string`/`format!` 出来的独立所有权，
/// 没有一处借着 `st`（它是 `pane_body` 的局部快照，借出去会活不过这一帧）。
fn details(
    app: &RecorderPaneState,
    st: &super::recorder_readout::SvcState,
) -> Element<'static, RecorderMsg> {
    let mut opts_sym: Vec<String> = vec![ALL.to_string()];
    opts_sym.extend(st.syms_seen.iter().cloned());
    let opts_stream: Vec<String> = std::iter::once(ALL.to_string())
        .chain(super::recorder_readout::stream_labels().iter().map(|s| s.to_string()))
        .collect();

    let hit: Vec<&super::recorder_readout::LakeRow> = st
        .rows
        .iter()
        .filter(|r| app.det_sym == ALL || r.sym == app.det_sym)
        .filter(|r| app.det_stream == ALL || r.stream == app.det_stream)
        .collect();
    let total_bytes: u64 = hit.iter().map(|r| r.bytes).sum();

    let mut col = column![
        text("④ 录制明细(逐 币种 × 类型 × 日期)").size(16).color(Color::from_rgb(0.85, 0.7, 1.0)),
        row![
            text("币种").size(12),
            pick_list(opts_sym, Some(app.det_sym.clone()), RecorderMsg::DetailSym).text_size(12),
            text("  类型").size(12),
            pick_list(opts_stream, Some(app.det_stream.clone()), RecorderMsg::DetailStream)
                .text_size(12),
            text(format!("   {} 个分区 · 合计 {}", hit.len(), fmt_size(total_bytes)))
                .size(12)
                .color(Color::from_rgb(0.7, 0.75, 0.8)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        text(format!("存储根目录  {}", st.data_dir)).size(11).color(Color::from_rgb(0.55, 0.6, 0.65)),
    ]
    .spacing(6);

    col = col.push(
        row![
            cell("交易所", 150.0),
            cell("币种", 90.0),
            cell("数据类型", 110.0),
            cell("日期", 100.0),
            cell("时段(UTC)", 110.0),
            cell("段数", 60.0),
            cell("大小", 90.0),
            cell("未封档/孤儿", 90.0),
            cell("存储位置(相对根目录)", 300.0),
        ]
        .spacing(6),
    );

    for r in hit.iter().take(app.det_limit) {
        let flags = if r.orphan > 0 {
            (format!("{} / {}", r.inprogress, r.orphan), Color::from_rgb(0.9, 0.5, 0.4))
        } else if r.inprogress > 0 {
            (format!("{} / 0", r.inprogress), Color::from_rgb(0.8, 0.75, 0.45))
        } else {
            ("—".to_string(), Color::from_rgb(0.5, 0.5, 0.5))
        };
        col = col.push(
            row![
                cell(&st.exchange, 150.0),
                cell(&r.sym, 90.0),
                cell(r.stream, 110.0),
                cell(&r.date, 100.0),
                cell(&format!("{} ~ {}", r.first, r.last), 110.0),
                cell(&r.segs.to_string(), 60.0),
                cell(&fmt_size(r.bytes), 90.0),
                container(text(flags.0).size(12).color(flags.1)).width(Length::Fixed(90.0)),
                cell(&r.rel, 300.0),
            ]
            .spacing(6),
        );
    }

    if hit.is_empty() {
        col = col.push(
            text("(没有符合条件的分区)").size(12).color(Color::from_rgb(0.5, 0.5, 0.5)),
        );
    } else if hit.len() > app.det_limit {
        col = col.push(
            row![
                button(text(format!("显示更多(+{DET_PAGE})")).size(12))
                    .on_press(RecorderMsg::DetailMore),
                text(format!("  还有 {} 个分区未显示", hit.len() - app.det_limit))
                    .size(11)
                    .color(Color::from_rgb(0.55, 0.6, 0.65)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    col.into()
}

fn hm(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let (h, m) = ((secs / 3600) % 24, (secs % 3600) / 60);
    format!("{h:02}:{m:02}")
}

fn dur(secs: i64) -> String {
    if secs >= 3600 {
        format!("{:.1}h", secs as f64 / 3600.0)
    } else {
        format!("{}分", secs / 60)
    }
}

/// ⑤ 按日覆盖：**那天到底录到了哪些时间段、洞在哪、能不能跑**。
///
/// ④ 回答「哪天有多少数据」，回答不了「那天能不能跑回测」——实测 2026-09-18 有
/// 6.1 小时数据却被打成 12 段、最长无洞段只有 10.7 分钟；2026-08-14 同样一整天，
/// 最长无洞段是 762 分钟。同样叫「有数据」，一个能跑一个不能。
///
/// **不打 A/B/C 等级**，这是有意的：回测入口的窗口体检已经在用这三个字母
/// （`recorder_health`，判据含延迟/量纲/跨流验证）。同一个字母在两处表示不同的东西，
/// 比不标还糟——那正是 docs/28 §3.3 立「判级口径必须源无关」时要消灭的。
/// 这里给的是**最长可跑窗口**：一个能直接抄进策略文件的数，不需要翻译。
fn coverage(app: &RecorderPaneState, st: &super::recorder_readout::SvcState) -> Element<'static, RecorderMsg> {
    let hit: Vec<&super::recorder_readout::DayCoverage> = st
        .coverage
        .iter()
        .filter(|c| app.det_sym == ALL || c.sym == app.det_sym)
        .collect();

    let mut col = column![
        text("⑤ 按日覆盖（那天真正录到的时间段 · 洞在哪 · 最长可跑窗口）")
            .size(16)
            .color(Color::from_rgb(0.55, 0.9, 0.75)),
        text(
            "区间取自 parquet 页脚统计，只看得见**段间**的洞（段是 600 秒轮转）——             数字是「至少这么碎」。能不能跑最终由回测入口的窗口体检说了算。"
        )
        .size(11)
        .color(Color::from_rgb(0.55, 0.6, 0.65)),
    ]
    .spacing(6);

    col = col.push(
        row![
            cell("币种", 90.0),
            cell("日期", 100.0),
            cell("覆盖时长", 80.0),
            cell("连续段", 60.0),
            cell("缺口", 55.0),
            cell("最长可跑窗口", 150.0),
            cell("录到的时间段 / 缺失的时间段", 420.0),
        ]
        .spacing(6),
    );

    for c in hit.iter().take(app.det_limit) {
        // 最长可跑窗口是这一行里最该被看见的数：不足 60 分钟基本挑不出可用回测窗口。
        let (lw, lc) = if c.longest_s >= 3600 {
            (format!("{} @{}", dur(c.longest_s), hm(c.longest_at)), Color::from_rgb(0.45, 0.85, 0.5))
        } else if c.longest_s >= 600 {
            (format!("{} @{}", dur(c.longest_s), hm(c.longest_at)), Color::from_rgb(0.85, 0.8, 0.4))
        } else {
            (format!("{} @{}", dur(c.longest_s), hm(c.longest_at)), Color::from_rgb(0.9, 0.5, 0.4))
        };
        let runs: String = c
            .runs
            .iter()
            .take(3)
            .map(|&(a, b)| format!("{}~{}", hm(a), hm(b)))
            .collect::<Vec<_>>()
            .join(" ");
        let gaps: String = c
            .gaps
            .iter()
            .take(3)
            .map(|&(a, b)| format!("{}~{}({})", hm(a), hm(b), dur((b - a) / 1_000_000_000)))
            .collect::<Vec<_>>()
            .join(" ");
        let more = |n: usize, k: usize| if n > k { format!(" +{}", n - k) } else { String::new() };
        col = col.push(
            row![
                cell(&c.sym, 90.0),
                cell(&c.date, 100.0),
                cell(&dur(c.covered_s), 80.0),
                cell(&c.runs.len().to_string(), 60.0),
                container(
                    text(c.gaps.len().to_string()).size(12).color(if c.gaps.is_empty() {
                        Color::from_rgb(0.45, 0.85, 0.5)
                    } else {
                        Color::from_rgb(0.85, 0.8, 0.4)
                    })
                )
                .width(Length::Fixed(55.0)),
                container(text(lw).size(12).color(lc)).width(Length::Fixed(150.0)),
                cell(
                    &format!(
                        "✓ {}{}   ✗ {}{}",
                        runs,
                        more(c.runs.len(), 3),
                        if gaps.is_empty() { "无".into() } else { gaps },
                        more(c.gaps.len(), 3)
                    ),
                    420.0
                ),
            ]
            .spacing(6),
        );
    }
    if hit.is_empty() {
        // 首轮要读约 1.2 万个 parquet 的 ts_recv 列（~21 秒），期间 `started` 还是 false。
        // 这时显示「无覆盖数据」会让人以为坏了——它只是还没算完。
        let msg = if st.started {
            "(无覆盖数据)"
        } else {
            "首次扫描中…（要读一遍各段的时间戳列，约 20 秒；之后按 mtime 缓存，几乎零成本）"
        };
        col = col.push(text(msg).size(12).color(Color::from_rgb(0.5, 0.5, 0.5)));
    } else if hit.len() > app.det_limit {
        col = col.push(
            text(format!("（还有 {} 天未显示，用上方「显示更多」）", hit.len() - app.det_limit))
                .size(11)
                .color(Color::from_rgb(0.55, 0.6, 0.65)),
        );
    }
    col.into()
}

/// 渲染录制驾驶舱（控制 + 实况 + 总览 + 明细）。
pub fn pane_body(app: &RecorderPaneState) -> Element<'_, RecorderMsg> {
    let st = super::recorder_readout::snapshot();

    let config = column![
        text("录制驾驶舱 — 24/7 守护录制控制").size(20).color(Color::from_rgb(0.55, 0.8, 1.0)),
        row![
            text("保存位置").size(13),
            text_input("~/ws-data", &app.data_dir)
                .on_input(RecorderMsg::DataDir)
                .width(Length::FillPortion(4)),
            button(text("应用配置并重启").size(13)).on_press(RecorderMsg::ApplyConfig),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        text(app.hint.clone()).size(12).color(if app.hint.starts_with('✗') {
            Color::from_rgb(0.9, 0.4, 0.4)
        } else {
            Color::from_rgb(0.6, 0.7, 0.6)
        }),
    ]
    .spacing(7);

    let mut syms_col = column![text(
        "录制币种与档位(应用后写入 recorder.toml;全 L2=增量盘口+成交+资金费,轻量=顶档快照+成交)"
    )
    .size(12)]
    .spacing(4);
    let mut line = row![].spacing(14);
    for (i, (s, sel)) in app.syms.iter().enumerate() {
        let (s2, s3) = (s.clone(), s.clone());
        line = line.push(
            row![
                checkbox(sel.enabled)
                    .label(s.clone())
                    .on_toggle(move |b| RecorderMsg::ToggleSym(s2.clone(), b)),
                pick_list(TierOpt::ALL.to_vec(), Some(sel.tier), move |t| {
                    RecorderMsg::TierPick(s3.clone(), t)
                })
                .text_size(11),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        );
        if (i + 1).is_multiple_of(5) {
            syms_col = syms_col.push(line);
            line = row![].spacing(14);
        }
    }
    syms_col = syms_col.push(line);
    syms_col = syms_col.push(
        row![
            text_input("自定义币种…", &app.custom)
                .on_input(RecorderMsg::CustomInput)
                .width(Length::Fixed(170.0)),
            button(text("添加").size(12)).on_press(RecorderMsg::AddCustom),
        ]
        .spacing(6),
    );

    // ① 服务控制
    let (dot, dotc, runtxt) = if st.active {
        (
            "●",
            Color::from_rgb(0.4, 0.85, 0.45),
            format!("运行中  已 {}  重启 {} 次", fmt_dur(st.uptime_secs), st.restarts),
        )
    } else {
        ("○", Color::from_rgb(0.6, 0.6, 0.6), "已停止".to_string())
    };
    let svc_ctrl = column![
        row![
            text("① 24/7 守护服务").size(16).color(Color::from_rgb(0.5, 0.8, 1.0)),
            text(format!("  {dot} ")).size(16).color(dotc),
            text(runtxt).size(13),
        ]
        .align_y(Alignment::Center),
        row![
            button(text("▶ 启动").size(14)).on_press(RecorderMsg::Start),
            button(text("■ 停止").size(14)).on_press(RecorderMsg::Stop),
            button(text("↻ 重启").size(14)).on_press(RecorderMsg::Restart),
            button(text("⟳ 刷新").size(14)).on_press(RecorderMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed))
                .size(11)
                .color(Color::from_rgb(0.5, 0.5, 0.5)),
        ]
        .spacing(8),
        // F0 72h 验收：验收对象就是本服务的录制质量，故并入①区。
        // oneshot，「重启次数」无意义，看的是上次结论 PASS/FAIL。
        {
            let (adot, adotc, atxt) = if st.accept.active {
                (
                    "●",
                    Color::from_rgb(0.4, 0.85, 0.45),
                    format!("验收运行中  已 {}", fmt_dur(st.accept.uptime_secs)),
                )
            } else if st.accept_verdict.starts_with("PASS") {
                ("✔", Color::from_rgb(0.4, 0.85, 0.45), format!("上次验收 {} PASS", st.accept_day))
            } else if !st.accept_verdict.is_empty() {
                (
                    "✗",
                    Color::from_rgb(0.9, 0.45, 0.4),
                    format!("上次验收 {} {}", st.accept_day, st.accept_verdict),
                )
            } else {
                ("○", Color::from_rgb(0.6, 0.6, 0.6), "未跑过验收".to_string())
            };
            row![
                button(text("▶ 跑 F0 验收").size(13)).on_press(RecorderMsg::RunAccept),
                text(format!("  {adot} ")).size(14).color(adotc),
                text(atxt).size(12).color(Color::from_rgb(0.75, 0.77, 0.82)),
                text("  (覆盖率≥99%×2整日，报告写数据目录)")
                    .size(10)
                    .color(Color::from_rgb(0.5, 0.5, 0.5)),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
        },
    ]
    .spacing(6);

    // ② 录制实况
    let mut live = column![
        text("② 录制实况(每 symbol 累计计数 · ↑=正在增长 · 来自服务日志)")
            .size(15)
            .color(Color::from_rgb(0.5, 0.9, 0.6))
    ]
    .spacing(2);
    live = live.push(
        row![
            cell("币种", 90.0),
            cell("状态", 76.0),
            cell("L2", 140.0),
            cell("成交", 120.0),
            cell("资金费", 90.0),
            cell("快照", 110.0),
            cell("重同步/错误", 110.0)
        ]
        .spacing(6),
    );
    for (sym, s) in &st.live {
        let (mark, mc) = if s.growing {
            ("↑ 录制中", Color::from_rgb(0.45, 0.85, 0.5))
        } else if st.active {
            ("静默", Color::from_rgb(0.85, 0.8, 0.4))
        } else {
            ("—", Color::from_rgb(0.5, 0.5, 0.5))
        };
        let stream = |n: u64| if n > 0 { group(n) } else { "—".into() };
        live = live.push(
            row![
                cell(sym, 90.0),
                container(text(mark.to_string()).size(12).color(mc)).width(Length::Fixed(76.0)),
                cell(&stream(s.l2), 140.0),
                cell(&stream(s.trades), 120.0),
                cell(&stream(s.mark), 90.0),
                cell(&stream(s.snap20), 110.0),
                cell(&format!("{} / {}", s.resyncs, s.parse_errs), 110.0),
            ]
            .spacing(6),
        );
    }
    if st.live.is_empty() {
        live = live.push(
            text("(服务未运行或暂无日志——点「启动」)").size(12).color(Color::from_rgb(0.5, 0.5, 0.5)),
        );
    }

    // ③ 已录制总览
    let pct = (st.span_days as f64 / GOAL_DAYS as f64).min(1.0);
    let filled = (pct * 24.0) as usize;
    let bar: String = "▓".repeat(filled) + &"░".repeat(24 - filled);
    let mut overview = column![
        text("③ 已录制总览(磁盘落盘,跨全部日期)").size(16).color(Color::from_rgb(0.9, 0.8, 0.4)),
        text(if st.span_days > 0 {
            format!(
                "时间跨度  {} ~ {}({} 天)      总大小  {}",
                st.span_first, st.span_last, st.span_days, fmt_size(st.total_bytes)
            )
        } else {
            "时间跨度  暂无落盘数据".into()
        })
        .size(13),
        text(format!(
            "30 天目标进度  {bar}  {}/{} 天",
            st.span_days, GOAL_DAYS
        ))
        .size(13)
        .color(Color::from_rgb(0.7, 0.8, 0.6)),
    ]
    .spacing(5);
    overview = overview.push(
        row![
            cell("币种", 90.0),
            cell("录制天数", 80.0),
            cell("大小", 120.0),
            cell("今日行数", 120.0)
        ]
        .spacing(6),
    );
    for (sym, l) in &st.lake {
        overview = overview.push(
            row![
                cell(sym, 90.0),
                cell(&l.days.to_string(), 80.0),
                cell(&fmt_size(l.bytes), 120.0),
                cell(&group(l.today_rows), 120.0),
            ]
            .spacing(6),
        );
    }
    if st.lake.is_empty() {
        overview =
            overview.push(text("(该目录暂无落盘)").size(12).color(Color::from_rgb(0.5, 0.5, 0.5)));
    }

    let left = column![svc_ctrl, vgap(12.0), live].spacing(6).width(Length::FillPortion(3));
    let right = scrollable(overview).width(Length::FillPortion(2)).height(Length::Fill);
    // 明细是宽表（九列），塞进左右分栏会被挤到换行，故整幅放在分栏下方。
    let body = row![left, right].spacing(18).height(Length::Shrink);

    container(scrollable(
        column![
            config,
            vgap(8.0),
            syms_col,
            vgap(14.0),
            body,
            vgap(14.0),
            details(app, &st),
            vgap(14.0),
            coverage(app, &st),
        ]
        .spacing(10)
        .padding(14),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
