//! 策略中心视图（docs/37 P1）：策略库 ∣ 详情 + 参数 + 发起回测 ∣ 运行记录 + 概况。
//!
//! 只读 [`super::strategy_center::view`] 的快照，不做 IO。完整 tearsheet 在同工作区右侧的
//! 「回测结果」面板（点一条运行记录会把它钉在那次运行上）。

use iced::widget::{Space, button, column, container, pick_list, row, scrollable, text_input};
use iced::{Alignment, Element, Length};

use super::strategy_center::{Entry, Param, RunRow, ScMsg, Summary, View, default_text};
use crate::ui::fmt::{Rounding, opt};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

/// 研究结论 → 徽标（这是**研究结论**，不是绩效分）。
fn verdict_badge<'a>(v: &str) -> Element<'a, ScMsg> {
    let (label, tone) = match v {
        "alive" => ("存活", Tone::Success),
        "candidate" => ("候选", Tone::Accent),
        "paper" => ("模拟盘", Tone::Info),
        "live" => ("实盘", Tone::Accent),
        "falsified" => ("已证伪", Tone::Danger),
        "retired" => ("已退役", Tone::Neutral),
        _ => ("未验证", Tone::Warning),
    };
    w::badge(label, tone)
}

fn status_badge<'a>(s: &str) -> Element<'a, ScMsg> {
    let (label, tone) = match s {
        "done" => ("完成", Tone::Success),
        "running" => ("运行中", Tone::Info),
        "queued" => ("排队", Tone::Info),
        "cancelled" => ("已取消", Tone::Neutral),
        _ => ("失败", Tone::Danger),
    };
    w::badge(label, tone)
}

fn signed(v: Option<f64>, dp: usize) -> (String, iced::Color) {
    match v {
        Some(x) => (format!("{x:+.dp$}"), if x >= 0.0 { pal::up() } else { pal::down() }),
        None => (crate::ui::fmt::na(), pal::dim()),
    }
}

/// 回撤 / 收益口径的简称：口径不同的数字不能放在一起比。
fn basis(b: &Option<String>) -> &'static str {
    match b.as_deref() {
        Some("realized") => "已实现",
        Some("mark_to_market_1m") => "盯市·分钟",
        Some("mark_to_market_daily") => "盯市·日",
        Some(_) => "其他口径",
        None => "口径未知",
    }
}

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 运行用了哪些参数（只列改过的；空 = 全缺省）。
fn params_brief(r: &RunRow) -> String {
    match r.params.as_object() {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "),
        _ => "缺省参数".into(),
    }
}

// ── 左栏：策略库 ───────────────────────────────────────────────────────

const VERDICT_FILTERS: [(&str, Option<&str>); 4] =
    [("全部", None), ("存活", Some("alive")), ("未验证", Some("untested")), ("已证伪", Some("falsified"))];

fn library<'a>(v: &View) -> Element<'a, ScMsg> {
    let mut col = column![w::panel_header(
        "策略库",
        Some(t::metadata(format!("{} 个", v.catalog.len())).into()),
        vec![w::btn_busy("刷新", Kind::Subtle, Some(ScMsg::Refresh), v.catalog_loading)],
    )]
    .spacing(space(2));

    col = col.push(
        text_input("搜索名称 / id / 标签", &v.search).on_input(ScMsg::Search).size(t::s_small()),
    );
    let filters = VERDICT_FILTERS.iter().map(|(label, val)| {
        let on = v.verdict.as_deref() == *val;
        w::btn(*label, if on { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::Verdict(val.map(str::to_string))))
    });
    col = col.push(row(filters).spacing(space(1)));

    if !v.catalog_err.is_empty() {
        col = col.push(w::error("策略目录读不出来", v.catalog_err.clone(), "确认 ~/ws-venv 可用后点「刷新」", None));
    } else if v.catalog.is_empty() {
        col = col.push(if v.catalog_loading {
            w::loading("策略目录")
        } else {
            w::empty("还没有接入的策略", "在策略文件里写 STRATEGY（见 strategies/research/README）")
        });
    }

    let q = v.search.to_lowercase();
    let mut items: Vec<&Entry> = v
        .catalog
        .iter()
        .filter(|e| match &e.meta {
            Some(m) => {
                v.verdict.as_ref().is_none_or(|f| &m.verdict == f)
                    && (q.is_empty()
                        || m.name.to_lowercase().contains(&q)
                        || m.id.contains(&q)
                        || m.tags.iter().any(|t| t.contains(&q)))
            }
            None => q.is_empty() && v.verdict.is_none(), // 元数据写错的条目只在不筛选时露出
        })
        .collect();
    items.sort_by(|a, b| {
        let fa = a.meta.as_ref().map(|m| m.family.as_str()).unwrap_or("~");
        let fb = b.meta.as_ref().map(|m| m.family.as_str()).unwrap_or("~");
        fa.cmp(fb).then(a.id.cmp(&b.id))
    });

    let mut list = column![].spacing(space(1));
    let mut family = String::new();
    for e in items {
        let fam = e.meta.as_ref().map(|m| m.family.clone()).unwrap_or_else(|| "（元数据有误）".into());
        if fam != family {
            list = list.push(w::section(fam.clone()));
            family = fam;
        }
        list = list.push(library_item(e, v));
    }
    col = col.push(scrollable(list).height(Length::Fill));
    container(col).width(Length::Fixed(260.0)).height(Length::Fill).into()
}

fn library_item<'a>(e: &Entry, v: &View) -> Element<'a, ScMsg> {
    let selected = v.selected.as_ref().is_some_and(|s| s.id == e.id);
    let mut head = row![].spacing(space(2)).align_y(Alignment::Center);
    let mut sub = column![].spacing(0);
    match &e.meta {
        Some(m) => {
            head = head.push(t::body(m.name.clone())).push(Space::new().width(Length::Fill)).push(verdict_badge(&m.verdict));
            sub = sub.push(t::metadata(m.id.clone()).color(pal::dim()));
            if let Some(r) = v.latest.get(&e.id)
                && let Some(s) = &r.summary
            {
                let (pnl, c) = signed(s.pnl, 2);
                sub = sub.push(
                    row![
                        t::metadata("最近").color(pal::dim()),
                        t::numeric(pnl).color(c),
                        t::metadata(format!(
                            "回撤 {}%（{}）",
                            opt(s.max_dd_pct, 2, Rounding::Measurement),
                            basis(&s.dd_basis)
                        ))
                        .color(pal::dim()),
                    ]
                    .spacing(space(2)),
                );
            }
        }
        None => {
            head = head.push(t::body(e.path.clone())).push(Space::new().width(Length::Fill)).push(w::badge("有误", Tone::Danger));
        }
    }
    if !e.error.is_empty() {
        sub = sub.push(t::metadata(e.error.clone()).color(pal::bad()));
    }
    button(column![head, sub].spacing(space(1)))
        .width(Length::Fill)
        .on_press(ScMsg::Select(e.id.clone()))
        .style(move |th, st| w::button_style(if selected { Kind::Standard } else { Kind::Ghost }, th, st))
        .into()
}

// ── 中栏：详情 + 参数 + 发起 ───────────────────────────────────────────

const LABEL_W: f32 = 128.0;

fn param_row<'a>(p: &Param, v: &View) -> Element<'a, ScMsg> {
    let cur = v.form.get(&p.name).cloned().unwrap_or_default();
    let name = p.name.clone();
    let input: Element<'a, ScMsg> = if let Some(ch) = &p.choices {
        let opts: Vec<String> = ch.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect();
        pick_list(opts, Some(cur.clone()), move |s| ScMsg::Field(name.clone(), s)).text_size(t::s_small()).into()
    } else if p.kind == "boolean" {
        pick_list(vec!["true".to_string(), "false".to_string()], Some(cur.clone()), move |s| ScMsg::Field(name.clone(), s))
            .text_size(t::s_small())
            .into()
    } else {
        text_input(&default_text(p), &cur).on_input(move |s| ScMsg::Field(name.clone(), s)).size(t::s_small()).width(Length::Fixed(140.0)).into()
    };
    let mut hint = vec![];
    if p.minimum.is_some() || p.maximum.is_some() {
        hint.push(format!(
            "[{}, {}]",
            p.minimum.map(|x| x.to_string()).unwrap_or_else(|| "−∞".into()),
            p.maximum.map(|x| x.to_string()).unwrap_or_else(|| "∞".into())
        ));
    }
    if p.nullable {
        hint.push("可留空".into());
    }
    let changed = v.changed.contains(&p.name);
    let label = format!("{}{}{}", if p.optimize { "★ " } else { "" }, p.name, if changed { " ●" } else { "" });
    let mut r = row![
        container(t::caption(label).color(if changed { pal::accent() } else { pal::txt() })).width(Length::Fixed(LABEL_W)),
        input,
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if let Some(err) = v.field_errors.get(&p.name) {
        r = r.push(t::metadata(err.clone()).color(pal::bad()));
    }
    // 第二行：说明 + 范围（放在输入框下面，不和输入框抢宽度——窄栏里挤在右边会折成两行）
    let mut info: Vec<String> = vec![];
    if !p.description.is_empty() {
        info.push(p.description.clone());
    }
    info.extend(hint);
    let mut c = column![r].spacing(0);
    if !info.is_empty() {
        c = c.push(row![Space::new().width(Length::Fixed(LABEL_W + space(2))), t::metadata(info.join(" · ")).color(pal::dim())]);
    }
    c.into()
}

fn detail<'a>(v: &View) -> Element<'a, ScMsg> {
    let Some(e) = &v.selected else {
        return container(w::empty("选一个策略", "左侧策略库里点一个")).width(Length::FillPortion(5)).into();
    };
    let Some(m) = &e.meta else {
        return container(w::error("这个策略的元数据有误", e.error.clone(), "按报错改策略文件里的 STRATEGY，再点「刷新」", None)).width(Length::FillPortion(5)).into();
    };
    let mut col = column![w::panel_header(
        m.name.clone(),
        Some(verdict_badge(&m.verdict)),
        vec![w::btn("打开源码", Kind::Subtle, Some(ScMsg::OpenSource))],
    )]
    .spacing(space(2));

    col = col.push(t::metadata(format!("{} · v{} · {}", m.id, m.version, e.path)).color(pal::dim()));
    if !m.verdict_note.is_empty() {
        col = col.push(t::caption(format!("研究结论：{}", m.verdict_note)));
    }
    let mut chips = row![].spacing(space(1));
    for tag in m.tags.iter().chain(m.data.iter()) {
        chips = chips.push(w::badge(tag.clone(), Tone::Neutral));
    }
    col = col.push(chips);
    if !m.docs.is_empty() {
        col = col.push(t::metadata(format!("文档：{}", m.docs.join("、"))).color(pal::dim()));
    }

    // 参数：★ = 值得优化（x-optimize）；● = 改过（只下发改过的）
    col = col.push(w::section(format!("参数（{} 个，★ 值得优化，● 已改）", e.params.len())));
    let mut prm = column![].spacing(space(2));
    let mut ps: Vec<&Param> = e.params.iter().collect();
    ps.sort_by_key(|p| !p.optimize); // 值得优化的排前面
    for p in ps {
        prm = prm.push(param_row(p, v));
    }
    col = col.push(prm);
    col = col.push(w::btn("参数恢复缺省", Kind::Ghost, Some(ScMsg::ResetParams)));

    // 回测窗口（缺省 = 策略 BACKTEST 的声明）
    col = col.push(w::section("回测窗口"));
    col = col.push(t::metadata(format!(
        "策略声明：{} · {} · {} → {}",
        e.bt("source"),
        e.bt("symbols"),
        e.bt("start"),
        e.bt("end")
    ))
    .color(pal::dim()));
    col = col.push(
        row![
            container(t::caption("开始 → 结束")).width(Length::Fixed(LABEL_W)),
            text_input("YYYY-MM-DD", &v.start).on_input(ScMsg::Start).size(t::s_small()).width(Length::Fixed(104.0)),
            t::caption("→"),
            text_input("YYYY-MM-DD", &v.end).on_input(ScMsg::End).size(t::s_small()).width(Length::Fixed(104.0)),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    col = col.push(
        row![
            container(t::caption("备注")).width(Length::Fixed(LABEL_W)),
            text_input("这次想看什么（记进研究库）", &v.note).on_input(ScMsg::Note).size(t::s_small()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );

    let can_full = m.engines.iter().any(|x| x == "full");
    let ok = v.field_errors.is_empty() && !v.sending && can_full;
    let quick_why = if m.engines.iter().any(|x| x == "quick") { "快速回测（向量化）在 P4 接入" } else { "这个策略没有快速回测引擎" };
    col = col.push(
        row![
            w::btn_why("⚡ 快速回测", Kind::Standard, None::<ScMsg>, quick_why),
            if ok {
                w::btn("▶ 完整回测", Kind::Primary, Some(ScMsg::RunFull))
            } else {
                w::btn_why(
                    "▶ 完整回测",
                    Kind::Primary,
                    None::<ScMsg>,
                    if !can_full { "这个策略没有完整回测引擎" } else if v.sending { "正在发送…" } else { "先改正标红的参数" },
                )
            },
        ]
        .spacing(space(2)),
    );
    if !v.msg.is_empty() {
        col = col.push(t::caption(v.msg.clone()).color(if v.msg.contains("失败") || v.msg.contains("有误") { pal::bad() } else { pal::dim() }));
    }
    col = col.push(t::metadata("完整回测走 ws-control → factory.lab：入口体检、真实撮合、记进研究库。进度看底部进度条与 K 线，结果在右侧「回测结果」。").color(pal::dim()));
    container(scrollable(col)).width(Length::FillPortion(5)).height(Length::Fill).into()
}

// ── 右栏：运行记录 + 概况 ──────────────────────────────────────────────

fn run_item<'a>(r: &RunRow, picked: bool) -> Element<'a, ScMsg> {
    let mut head = row![t::metadata(when(r.created_ts)), status_badge(&r.status)].spacing(space(2)).align_y(Alignment::Center);
    if let Some(s) = &r.summary {
        let (pnl, c) = signed(s.pnl, 2);
        head = head.push(t::numeric(pnl).color(c));
        head = head.push(t::metadata(format!("回撤 {}%", opt(s.max_dd_pct, 2, Rounding::Measurement))).color(pal::dim()));
        head = head.push(t::metadata(format!("成交 {}", opt(s.fills, 0, Rounding::Measurement))).color(pal::dim()));
    }
    if r.dirty {
        head = head.push(w::badge("未提交改动", Tone::Warning));
    }
    head = head.push(Space::new().width(Length::Fill));
    if r.active() {
        head = head.push(w::btn("■ 停止", Kind::Destructive, Some(ScMsg::Stop(r.run_id.clone()))));
    }
    let mut c = column![head, t::metadata(params_brief(r)).color(pal::dim())].spacing(0);
    if !r.note.is_empty() {
        c = c.push(t::metadata(format!("备注：{}", r.note)).color(pal::dim()));
    }
    if !r.error.is_empty() {
        c = c.push(t::metadata(r.error.clone()).color(pal::bad()));
    }
    button(c)
        .width(Length::Fill)
        .on_press(ScMsg::PickRun(r.run_id.clone()))
        .style(move |th, st| w::button_style(if picked { Kind::Standard } else { Kind::Ghost }, th, st))
        .into()
}

fn summary_card<'a>(r: &RunRow, s: &Summary) -> Element<'a, ScMsg> {
    const KW: f32 = 150.0;
    let num = |v: Option<f64>, dp: usize| -> Element<'a, ScMsg> { t::numeric(opt(v, dp, Rounding::Measurement)).into() };
    let (pnl, c) = signed(s.pnl, 2);
    let mut col = column![
        w::section(format!("概况 · {}", r.run_id)),
        w::kv("费后盈亏", row![t::numeric(pnl).color(c), t::metadata(format!("（{}%）", opt(s.pnl_pct, 2, Rounding::Measurement))).color(pal::dim())].spacing(space(1)).into(), KW),
        w::kv("期初 → 期末", t::numeric(format!("{} → {}", opt(s.start_balance, 2, Rounding::Money), opt(s.end_balance, 2, Rounding::Money))).into(), KW),
        w::kv("最大回撤", row![num(s.max_dd_pct, 2), t::metadata(format!("%（{}）", basis(&s.dd_basis))).color(pal::dim())].spacing(space(1)).into(), KW),
        w::kv("夏普 / Sortino", row![num(s.sharpe, 2), t::metadata("/"), num(s.sortino, 2), t::metadata(format!("（{}）", basis(&s.returns_basis))).color(pal::dim())].spacing(space(1)).into(), KW),
        w::kv("胜率 / 盈亏比（按持仓）", row![num(s.win_rate.map(|x| x * 100.0), 1), t::metadata("% /"), num(s.profit_factor, 2)].spacing(space(1)).into(), KW),
        w::kv("成交 / 用时", t::numeric(format!("{} 笔 · {} 秒", opt(s.fills, 0, Rounding::Measurement), opt(s.elapsed_s, 0, Rounding::Measurement))).into(), KW),
    ]
    .spacing(space(1));
    if s.stop_outs.is_some() || s.min_margin_level_pct.is_some() {
        col = col.push(w::kv(
            "强平 / 最低保证金",
            t::numeric(format!("{} 次 · {}%", opt(s.stop_outs, 0, Rounding::Measurement), opt(s.min_margin_level_pct, 1, Rounding::Measurement))).into(),
            KW,
        ));
    }
    if let Some(g) = &s.grade {
        col = col.push(w::kv("数据体检", t::body(g.clone()).into(), KW));
    }
    if !s.quality_flags.is_empty() {
        col = col.push(t::metadata(format!("质量标记：{}", s.quality_flags.join("、"))).color(pal::dim()));
    }
    if s.dd_basis.as_deref() == Some("realized") {
        col = col.push(
            t::metadata("⚠ 回撤与夏普按已实现收益算（Nautilus 口径），不含持仓浮亏——挂单 / 网格 / 库存类策略会显著偏好看。")
                .color(pal::warn()),
        );
    }
    if r.dirty {
        col = col.push(t::metadata("▲ 运行时工作区有未提交改动：这次结果无法从 git 原样复现。").color(pal::warn()));
    }
    col.into()
}

fn runs<'a>(v: &View) -> Element<'a, ScMsg> {
    let pinned = super::backtest_readout::pinned().is_some();
    let mut col = column![w::panel_header(
        "运行记录",
        Some(t::metadata(format!("{} 次", v.runs.len())).into()),
        vec![w::btn("跟随最新结果", if pinned { Kind::Standard } else { Kind::Ghost }, pinned.then_some(ScMsg::FollowLatest))],
    )]
    .spacing(space(2));
    if !v.db_err.is_empty() {
        col = col.push(t::metadata(v.db_err.clone()).color(pal::dim()));
    }
    if v.selected.is_none() {
        return container(col).width(Length::FillPortion(5)).into();
    }
    if v.runs.is_empty() && v.db_err.is_empty() {
        col = col.push(w::empty("这个策略还没跑过", "改好参数点「▶ 完整回测」"));
    }
    let mut list = column![].spacing(space(1));
    for r in &v.runs {
        list = list.push(run_item(r, v.picked_run.as_deref() == Some(r.run_id.as_str())));
    }
    col = col.push(scrollable(list).height(Length::FillPortion(3)));

    // 概况：选中的那次；没选就看最近一次完成的
    let focus = v
        .picked_run
        .as_ref()
        .and_then(|id| v.runs.iter().find(|r| &r.run_id == id))
        .or_else(|| v.runs.iter().find(|r| r.status == "done"));
    if let Some(r) = focus {
        match &r.summary {
            Some(s) => col = col.push(scrollable(summary_card(r, s)).height(Length::FillPortion(2))),
            None if !r.error.is_empty() => col = col.push(w::error("这次运行失败了", r.error.clone(), "日志在 /run/user/<uid>/wealthspring/runs/<运行号>.log", None)),
            None => {}
        }
    }
    container(col).width(Length::FillPortion(5)).height(Length::Fill).into()
}

pub fn pane_body<'a>() -> Element<'a, ScMsg> {
    let v = super::strategy_center::view();
    row![library(&v), detail(&v), runs(&v)]
        .spacing(space(3))
        .padding(crate::ui::metrics::pad2(2, 3))
        .height(Length::Fill)
        .into()
}
