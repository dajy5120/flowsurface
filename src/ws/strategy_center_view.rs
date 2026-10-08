//! 策略中心视图（docs/37 P1）：策略库 ∣ 详情 + 参数 + 发起回测 ∣ 运行记录 + 概况。
//!
//! 只读 [`super::strategy_center::view`] 的快照，不做 IO。完整 tearsheet 在同工作区右侧的
//! 「回测结果」面板（点一条运行记录会把它钉在那次运行上）。

use iced::widget::{Space, button, column, container, pick_list, row, text_input};
use iced::{Alignment, Element, Length};

use super::strategy_center::{Entry, Param, RunRow, ScMsg, Summary, Tab, View, default_text};
use super::strategy_center_cmp::{self as sc, Cmp};
use super::strategy_center_opt::{self as so, Gate, Heat, StudyRow};
use super::strategy_center_val::{self as sv, ExpRow, ValRow};
use crate::ui::fmt::{Rounding, opt};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

/// 研究结论 → 徽标（这是**研究结论**，不是绩效分）。
pub fn verdict_label(v: &str) -> &'static str {
    match v {
        "alive" => "存活",
        "candidate" => "候选",
        "paper" => "模拟盘",
        "live" => "实盘",
        "falsified" => "已证伪",
        "retired" => "已退役",
        _ => "未验证",
    }
}

fn verdict_badge<'a>(v: &str) -> Element<'a, ScMsg> {
    let tone = match v {
        "alive" => Tone::Success,
        "candidate" | "live" => Tone::Accent,
        "paper" => Tone::Info,
        "falsified" => Tone::Danger,
        "retired" => Tone::Neutral,
        _ => Tone::Warning,
    };
    w::badge(verdict_label(v), tone)
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
        Some("realized_balance") => "已实现余额",
        Some("realized_daily") => "已实现·日",
        Some("position_returns") => "⚠ 按持仓收益",
        Some("trade_bp_cumulative") => "逐笔 bp 累计",
        Some("quick_bp_daily") => "快速·日 bp",
        Some("mark_to_market_1m") => "盯市·分钟",
        Some("mark_to_market_daily") => "盯市·日",
        Some("mark_to_market") => "盯市",
        Some("quick_bp") => "快速 bp",
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
    use super::strategy_center_lib as lb;
    let f = &v.lib;
    let mut col = column![w::panel_header(
        "策略库",
        Some(t::metadata(format!("{} 个", v.catalog.len())).into()),
        vec![w::btn_busy("刷新", Kind::Subtle, Some(ScMsg::Refresh), v.catalog_loading)],
    )]
    .spacing(space(2));

    col = col.push(text_input("搜索名称 / id / 标签 / 大师", &v.search).on_input(ScMsg::Search).size(t::s_small()));
    // 分组方式 + 结论
    col = col.push(w::segmented(&lb::GROUPS, &f.group_by, ScMsg::LibGroup));
    let filters = VERDICT_FILTERS.iter().map(|(label, val)| {
        let on = v.verdict.as_deref() == *val;
        w::btn(*label, if on { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::Verdict(val.map(str::to_string))))
    });
    col = col.push(row(filters).spacing(space(1)));
    // 策略类型（多选，取「或」）
    let mut chips = row![w::btn("全部类型", if f.styles.is_empty() { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::LibStylesClear))]
        .spacing(space(1));
    for (k, label) in lb::STYLES {
        let on = f.styles.contains(k);
        chips = chips.push(w::btn(label, if on { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::LibStyle(k.to_string()))));
    }
    col = col.push(chips.wrap());

    // 目录树
    let q = v.search.clone();
    let verdict = v.verdict.as_deref();
    let mut head = row![
        w::btn(if f.tree_hidden { "▸ 目录" } else { "▾ 目录" }, Kind::Ghost, Some(ScMsg::LibTreeHidden)),
        w::btn("全部", if f.dir.is_none() { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::LibDir(None))),
    ]
    .spacing(space(1))
    .align_y(Alignment::Center);
    if let Some(d) = &f.dir {
        for (label, path) in lb::breadcrumb(d) {
            head = head.push(t::metadata("›").color(pal::dim()));
            head = head.push(w::btn(label, if f.dir.as_deref() == Some(path.as_str()) { Kind::Standard } else { Kind::Ghost },
                                    Some(ScMsg::LibDir(Some(path)))));
        }
    }
    col = col.push(head.wrap());
    // 目录树与下面的策略列表之间是可拖拽的分隔条（上下拖改目录区高度，双击复位）
    let mut tree_area: Option<Element<'a, ScMsg>> = None;
    if !f.tree_hidden {
        let mut tree = column![].spacing(0);
        for n in lb::tree(&v.catalog, f, verdict, &q) {
            let toggle: Element<'a, ScMsg> = if n.has_children {
                w::btn(if n.open { "▾" } else { "▸" }, Kind::Ghost, Some(ScMsg::LibOpen(n.path.clone())))
            } else {
                container(t::metadata("·").color(pal::dim())).width(Length::Fixed(space(6))).center_x(Length::Fixed(space(6))).into()
            };
            let sel = f.dir.as_deref() == Some(n.path.as_str());
            let label = button(row![t::label(n.label.clone()), t::metadata(format!("({})", n.count)).color(pal::dim())].spacing(space(1)))
                .on_press(ScMsg::LibDir(Some(n.path.clone())))
                .style(move |th, st| w::button_style(if sel { Kind::Standard } else { Kind::Ghost }, th, st));
            tree = tree.push(
                row![Space::new().width(Length::Fixed(space(4) * n.depth as f32)), toggle, label]
                    .spacing(space(1))
                    .align_y(Alignment::Center),
            );
        }
        tree_area = Some(crate::ui::scroll(tree.width(Length::Fill)).width(Length::Fill).height(Length::Fill).into());
    }

    let mut lower = column![].spacing(space(2));
    if !v.catalog_err.is_empty() {
        lower = lower.push(w::error("策略目录读不出来", v.catalog_err.clone(), "确认 ~/ws-venv 可用后点「刷新」", None));
    } else if v.catalog.is_empty() {
        lower = lower.push(if v.catalog_loading {
            w::loading("策略目录")
        } else {
            w::empty("还没有接入的策略", "在策略文件里写 STRATEGY（见 strategies/research/README）")
        });
    }

    let mut items = lb::visible(&v.catalog, f, verdict, &q);
    let key = |e: &Entry| lb::group_of(e, f.group_by);
    items.sort_by(|a, b| key(a).0.cmp(&key(b).0).then(a.id.cmp(&b.id)));
    if items.is_empty() && !v.catalog.is_empty() {
        lower = lower.push(w::empty("没有符合条件的策略", "放宽类型 / 目录 / 结论过滤"));
    }
    let mut list = column![].spacing(space(1));
    let mut family = String::new();
    for e in items {
        let (_, fam) = key(e);
        if fam != family {
            list = list.push(w::section(fam.clone()));
            family = fam;
        }
        list = list.push(library_item(e, v));
    }
    lower = lower.push(crate::ui::scroll(list.width(Length::Fill)).height(Length::Fill));
    let lower: Element<'a, ScMsg> = lower.height(Length::Fill).into();
    col = col.push(match tree_area {
        Some(tree) => crate::ui::split::column(
            "strategy_center.library",
            vec![(tree, 0.3, 60.0), (lower, 0.7, 120.0)],
            ScMsg::Split,
        ),
        None => lower,
    });
    // 宽度由外层分栏定（左右拖）
    container(col).width(Length::Fill).height(Length::Fill).into()
}

fn library_item<'a>(e: &Entry, v: &View) -> Element<'a, ScMsg> {
    let selected = v.selected.as_ref().is_some_and(|s| s.id == e.id);
    let mut head = row![].spacing(space(2)).align_y(Alignment::Center);
    let mut sub = column![].spacing(0);
    match &e.meta {
        Some(m) => {
            head = head.push(t::body(m.name.clone())).push(Space::new().width(Length::Fill)).push(verdict_badge(&m.verdict));
            sub = sub.push(t::metadata(m.id.clone()).color(pal::dim()));
            if !m.styles.is_empty() {
                let st: Vec<&str> = m.styles.iter().map(|s| super::strategy_center_lib::style_label(s)).collect();
                sub = sub.push(t::metadata(st.join(" · ")).color(pal::accent()));
            }
            if let Some(mi) = &m.master {
                sub = sub.push(t::metadata(format!("{} · {} · {}", mi.name, stars(mi.openness), delivery_label(&mi.delivery))).color(pal::dim()));
            }
            if let Some(r) = v.latest.get(&e.id)
                && let Some(s) = &r.summary
                && s.is_bp()
            {
                let (per, c) = signed(s.net_bp_mean, 2);
                sub = sub.push(row![t::metadata("最近 ⚡").color(pal::dim()), t::numeric(format!("{per} bp/笔")).color(c)].spacing(space(2)));
            } else if let Some(r) = v.latest.get(&e.id)
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
    if let Some(mi) = &m.master {
        col = col.push(master_card(mi));
    }
    // 百科卡片：不可程序化，只有卡片与规则文档
    if m.is_card() {
        col = col.push(t::caption("百科卡片：这位大师的方法主要靠判断 / 专有信息 / 单次事件，不可程序化或缺数据，没有回测入口。").color(pal::dim()));
        col = col.push(doc_section(e));
        return container(crate::ui::scroll(col)).width(Length::FillPortion(5)).height(Length::Fill).into();
    }
    // 专用引擎：没有统一回测入口，只给结论与运行方式
    if !m.engines.is_empty() && m.engines.iter().all(|x| x == "harness") {
        col = col.push(w::section("运行方式（专用引擎）"));
        col = col.push(t::caption(if m.description.is_empty() { "见策略文件说明".to_string() } else { m.description.clone() }));
        col = col.push(
            t::metadata("这类研究没有统一的回测入口（数据形态 / 回测方式与订单流策略不同），策略中心只列结论；在 Studio 终端里按上面的命令运行。")
                .color(pal::dim()),
        );
        return container(crate::ui::scroll(col)).width(Length::FillPortion(5)).height(Length::Fill).into();
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
    let can_quick = m.engines.iter().any(|x| x == "quick");
    let quick_ok = can_quick && v.field_errors.is_empty() && !v.sending;
    col = col.push(
        row![
            if quick_ok {
                w::btn("⚡ 快速回测", Kind::Standard, Some(ScMsg::RunQuick))
            } else {
                w::btn_why("⚡ 快速回测", Kind::Standard, None::<ScMsg>,
                           if !can_quick { "这个策略没有快速回测引擎" } else if v.sending { "正在发送…" } else { "先改正标红的参数" })
            },
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
    if can_quick {
        col = col.push(t::metadata(if m.quick.as_deref() == Some("bars") {
            "⚡ 快速回测 = 大师日线七层引擎：组合级、逐根推进、下一根成交、真实合约乘数与费用、逐日盯市；日线看不到盘中先后，当根进出多的策略会自动并列最坏情况。"
        } else {
            "⚡ 快速回测 = 100ms 特征帧向量回测，秒级、按「每笔 1 倍名义」的 bp 计，不建模排队与延迟——只用来筛，结论以完整回测为准。"
        }).color(pal::dim()));
    }
    if can_full || can_quick {
        col = col.push(opt_section(v, e));
    }
    if m.master.is_some() {
        col = col.push(doc_section(e));
    }
    container(crate::ui::scroll(col)).width(Length::FillPortion(5)).height(Length::Fill).into()
}

// ── 右栏：运行记录 + 概况 ──────────────────────────────────────────────

fn run_item<'a>(r: &RunRow, picked: bool, in_basket: bool) -> Element<'a, ScMsg> {
    let mut head = row![t::metadata(when(r.created_ts)), status_badge(&r.status)].spacing(space(2)).align_y(Alignment::Center);
    if let Some(s) = &r.summary {
        if s.is_bp() {
            let (per, c) = signed(s.net_bp_mean, 2);
            head = head.push(w::badge("⚡ 快速", Tone::Neutral));
            head = head.push(t::numeric(format!("{per} bp/笔")).color(c));
        } else {
            let (pnl, c) = signed(s.pnl, 2);
            head = head.push(t::numeric(pnl).color(c));
            head = head.push(t::metadata(format!("回撤 {}%", opt(s.max_dd_pct, 2, Rounding::Measurement))).color(pal::dim()));
        }
        head = head.push(t::metadata(format!("成交 {}", opt(s.fills, 0, Rounding::Measurement))).color(pal::dim()));
    }
    if r.dirty {
        head = head.push(w::badge("未提交改动", Tone::Warning));
    }
    head = head.push(Space::new().width(Length::Fill));
    if r.active() {
        head = head.push(w::btn("■ 停止", Kind::Destructive, Some(ScMsg::Stop(r.run_id.clone()))));
    }
    if r.status == "done" && r.result_dir.is_some() {
        head = head.push(w::btn(if in_basket { "✓ 对比" } else { "＋对比" }, if in_basket { Kind::Standard } else { Kind::Ghost },
                                Some(ScMsg::CmpToggle(r.run_id.clone()))));
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
    if s.is_bp() {
        let (tot, c) = signed(s.pnl, 1);
        let (per, c2) = signed(s.net_bp_mean, 3);
        return column![
            w::section(format!("概况 · {} · ⚡ 快速回测", r.run_id)),
            w::kv("每笔净值", row![t::numeric(per).color(c2), t::metadata("bp").color(pal::dim())].spacing(space(1)).into(), KW),
            w::kv("毛 / 成本", t::numeric(format!("{} / {} bp", opt(s.gross_bp_mean, 3, Rounding::Measurement), opt(s.cost_bp_mean, 3, Rounding::Measurement))).into(), KW),
            w::kv("累计 / 回撤", row![t::numeric(tot).color(c), t::metadata(format!("bp · 回撤 {} bp", opt(s.max_dd_bp, 1, Rounding::Measurement))).color(pal::dim())].spacing(space(1)).into(), KW),
            w::kv("笔数 / 胜率", t::numeric(format!("{} 笔 · {}%", opt(s.fills.map(|x| x / 2.0), 0, Rounding::Measurement), opt(s.win_rate.map(|x| x * 100.0), 1, Rounding::Measurement))).into(), KW),
            w::kv("天数 / 用时", t::numeric(format!("{} 天 · {} 秒", opt(s.days, 0, Rounding::Measurement), opt(s.elapsed_s, 0, Rounding::Measurement))).into(), KW),
            t::metadata("按「每笔 1 倍名义仓位」计 bp，不是账户金额；不建模排队、逆选择与延迟。只用来筛——结论以完整回测为准。").color(pal::warn()),
        ]
        .spacing(space(1))
        .into();
    }
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
    if let Some(n) = &s.note {
        col = col.push(t::metadata(format!("⚠ {n}")).color(pal::warn()));
    }
    if s.returns_basis.as_deref() == Some("position_returns") {
        col = col.push(
            t::metadata("⚠ 这次回测不到两个自然日，Nautilus 的夏普 / Sortino 退回按「每笔持仓收益」算，与账户无关，别拿来比较。")
                .color(pal::warn()),
        );
    }
    if s.dd_basis.as_deref().is_some_and(|b| b.starts_with("realized")) {
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
    let n_active = v.studies.iter().filter(|s| s.active()).count();
    let study_tab = if n_active > 0 { "优化 ●" } else { "优化" };
    let check_tab = if v.validations.iter().any(ValRow::active) { "验证 ●" } else { "验证" };
    // 标签页文字要 'static：篮子里几个就显示几（最多 MAX_BASKET）
    const CMP_TABS: [&str; 9] = ["对比", "对比 1", "对比 2", "对比 3", "对比 4", "对比 5", "对比 6", "对比 7", "对比 8"];
    let cmp_tab = CMP_TABS[v.basket.len().min(CMP_TABS.len() - 1)];
    let mut col = column![
        w::panel_header(
            "",
            Some(w::tabs(&[("运行记录", Tab::Runs), (study_tab, Tab::Studies), (check_tab, Tab::Checks), (cmp_tab, Tab::Compare)], &v.tab, ScMsg::Tab)),
            vec![w::btn("跟随最新结果", if pinned { Kind::Standard } else { Kind::Ghost }, pinned.then_some(ScMsg::FollowLatest))],
        )
    ]
    .spacing(space(2));
    if v.tab == Tab::Studies {
        return container(col.push(studies(v))).width(Length::FillPortion(5)).height(Length::Fill).into();
    }
    if v.tab == Tab::Compare {
        return container(col.push(crate::ui::scroll(compare(v)).height(Length::Fill))).width(Length::FillPortion(5)).height(Length::Fill).into();
    }
    if v.tab == Tab::Checks {
        return container(col.push(crate::ui::scroll(checks(v)).height(Length::Fill))).width(Length::FillPortion(5)).height(Length::Fill).into();
    }
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
        let in_basket = v.basket.iter().any(|b| b.run_id == r.run_id);
        list = list.push(run_item(r, v.picked_run.as_deref() == Some(r.run_id.as_str()), in_basket));
    }
    col = col.push(crate::ui::scroll(list).height(Length::FillPortion(3)));

    // 概况：选中的那次；没选就看最近一次完成的
    let focus = v
        .picked_run
        .as_ref()
        .and_then(|id| v.runs.iter().find(|r| &r.run_id == id))
        .or_else(|| v.runs.iter().find(|r| r.status == "done"));
    if let Some(r) = focus {
        match &r.summary {
            Some(s) => col = col.push(crate::ui::scroll(summary_card(r, s)).height(Length::FillPortion(2))),
            None if !r.error.is_empty() => col = col.push(w::error("这次运行失败了", r.error.clone(), "日志在 /run/user/<uid>/wealthspring/runs/<运行号>.log", None)),
            None => {}
        }
    }
    container(col).width(Length::FillPortion(5)).height(Length::Fill).into()
}

pub fn pane_body<'a>() -> Element<'a, ScMsg> {
    let v = super::strategy_center::view();
    use crate::ui::split;
    // 三栏之间是可拖拽的分隔条（按比例：拖动在相邻两栏之间挪宽度，每栏有最小宽度；双击复位）
    container(split::row(
        "strategy_center.cols",
        vec![
            (library(&v), 0.26, 220.0),
            (container(detail(&v)).padding(crate::ui::metrics::pad2(0, 2)).into(), 0.37, 260.0),
            (runs(&v), 0.37, 300.0),
        ],
        ScMsg::Split,
    ))
    .padding(crate::ui::metrics::pad2(2, 3))
    .height(Length::Fill)
    .into()
}

// ── 中栏：参数优化表单 ─────────────────────────────────────────────────

/// AI 研究助理：一句话 → 草稿（只起草；填进下面的表单后仍由人点「开始优化」）。
fn ai_box<'a>(v: &View) -> Element<'a, ScMsg> {
    let mut col = column![
        w::section("🤖 AI 研究助理（只起草，不发起）"),
        row![
            text_input("一句话说想怎么调，例如：用快速回测在 6 月前 20 天扫入场阈值和最长持仓", &v.ai_text)
                .on_input(ScMsg::AiText)
                .on_submit(ScMsg::AiDraft)
                .size(t::s_small()),
            w::btn_busy("起草", Kind::Standard, (!v.ai_text.trim().is_empty()).then_some(ScMsg::AiDraft), v.ai_loading),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    ]
    .spacing(space(2));
    if v.ai_loading {
        col = col.push(t::metadata("AI 在看策略的参数表、历史优化与试验数…（半分钟左右）").color(pal::dim()));
    }
    if !v.ai_err.is_empty() {
        col = col.push(t::caption(v.ai_err.clone()).color(pal::bad()));
    }
    if let Some(a) = &v.ai {
        col = col.push(t::body(if a.reply.is_empty() { "（AI 没有回答）".to_string() } else { a.reply.clone() }));
        for c in &a.cautions {
            col = col.push(t::metadata(format!("⚠ {c}")).color(pal::warn()));
        }
        for e in &a.errors {
            col = col.push(t::metadata(format!("✗ {e}")).color(pal::bad()));
        }
        if let Some(b) = a.brief() {
            col = col.push(t::metadata(format!("草稿：{b}")).color(pal::dim()));
        }
        let mut foot = row![t::metadata(format!(
            "{} · AI 调用 {} 次{} · 本策略已有 {} 次运行，全局试验 {}——据此发起的每次试验都会再计入",
            a.assist_id,
            a.n_calls,
            a.cost_usd.map(|c| format!("（${c:.3}）")).unwrap_or_default(),
            a.trials.strategy_runs,
            a.trials.global.map(|n| n.to_string()).unwrap_or_else(|| "未知".into()),
        ))
        .color(pal::dim())]
        .spacing(space(2))
        .align_y(Alignment::Center);
        if a.spec.is_some() {
            foot = foot.push(Space::new().width(Length::Fill));
            foot = foot.push(if a.ready {
                w::btn("填入表单", Kind::Standard, Some(ScMsg::AiApply))
            } else {
                w::btn_why("填入表单", Kind::Standard, None::<ScMsg>, "草稿没过校验（见上面的 ✗），改一下说法再起草")
            });
        }
        col = col.push(foot);
    }
    if v.msg.contains("草稿") {
        col = col.push(t::caption(v.msg.clone()).color(if v.msg.contains("填不进") { pal::warn() } else { pal::ok() }));
    }
    col.into()
}

fn opt_section<'a>(v: &View, e: &Entry) -> Element<'a, ScMsg> {
    let f = &v.opt;
    let mut col = column![
        ai_box(v),
        w::section("参数优化（Optuna）"),
        t::metadata("☑ = 参与搜索；数值参数填 下限 ~ 上限 · 步长（网格必须有步长）").color(pal::dim()),
    ]
    .spacing(space(2));
    let mut ps: Vec<&Param> = e.params.iter().filter(|p| so::searchable(p)).collect();
    ps.sort_by_key(|p| !p.optimize);
    for p in ps {
        let Some(of) = f.fields.get(&p.name) else { continue };
        let name = p.name.clone();
        let mut r = row![
            container(w::btn(
                format!("{} {}", if of.on { "☑" } else { "☐" }, p.name),
                if of.on { Kind::Standard } else { Kind::Ghost },
                Some(ScMsg::OptToggle(name.clone())),
            ))
            .width(Length::Fixed(112.0)),
        ]
        .spacing(space(1))
        .align_y(Alignment::Center);
        if !of.on && p.optimize && p.choices.is_none() && p.kind != "boolean" && (p.minimum.is_none() || p.maximum.is_none()) {
            r = r.push(t::metadata("★ 注解没给完整范围，勾上后要自己填").color(pal::dim()));
        }
        if of.on {
            if let Some(ch) = &p.choices {
                r = r.push(t::metadata(format!("全部 {} 个取值", ch.len())).color(pal::dim()));
            } else if p.kind == "boolean" {
                r = r.push(t::metadata("true / false").color(pal::dim()));
            } else {
                let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
                r = r
                    .push(text_input("下限", &of.low).on_input(move |s| ScMsg::OptLow(n1.clone(), s)).size(t::s_small()).width(Length::Fixed(58.0)))
                    .push(t::caption("~"))
                    .push(text_input("上限", &of.high).on_input(move |s| ScMsg::OptHigh(n2.clone(), s)).size(t::s_small()).width(Length::Fixed(58.0)))
                    .push(text_input("步长", &of.step).on_input(move |s| ScMsg::OptStep(n3.clone(), s)).size(t::s_small()).width(Length::Fixed(50.0)));
            }
        }
        col = col.push(r);
    }
    if e.meta.as_ref().is_some_and(|m| m.engines.iter().any(|x| x == "quick")) {
        let engines = [("full", "完整回测"), ("quick", "快速回测")].iter().map(|(k, l)| {
            w::btn(*l, if f.engine == *k { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::OptEngine((*k).to_string())))
        });
        col = col.push(
            row![container(t::caption("每次试验用")).width(Length::Fixed(LABEL_W)), row(engines).spacing(space(1))]
                .spacing(space(2))
                .align_y(Alignment::Center),
        );
    }
    let samplers = so::SAMPLERS.iter().map(|(k, label)| {
        w::btn(*label, if f.sampler == *k { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::OptSampler((*k).to_string())))
    });
    col = col.push(
        row![container(t::caption("搜索方式")).width(Length::Fixed(LABEL_W)), row(samplers).spacing(space(1))]
            .spacing(space(2))
            .align_y(Alignment::Center),
    );
    let objs: Vec<String> = so::OBJECTIVES.iter().map(|(k, _)| (*k).to_string()).collect();
    let obj_desc = so::OBJECTIVES.iter().find(|(k, _)| *k == f.objective).map(|(_, d)| *d).unwrap_or("");
    col = col.push(
        row![
            container(t::caption("目标（求最大）")).width(Length::Fixed(LABEL_W)),
            pick_list(objs, Some(f.objective.clone()), ScMsg::OptObjective).text_size(t::s_small()),
            t::metadata(obj_desc).color(pal::dim()),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    let mut sizes = row![
        container(t::caption("尝试次数 / 并发")).width(Length::Fixed(LABEL_W)),
        text_input("20", &f.trials).on_input(ScMsg::OptTrials).size(t::s_small()).width(Length::Fixed(64.0)),
        t::caption("/"),
        text_input("2", &f.workers).on_input(ScMsg::OptWorkers).size(t::s_small()).width(Length::Fixed(44.0)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if f.sampler == "grid" {
        sizes = sizes.push(
            t::metadata(match v.grid_estimate {
                Some(n) if n > so::MAX_GRID => format!("网格 {n} 组，超过上限 {}", so::MAX_GRID),
                Some(n) => format!("网格共 {n} 组"),
                None => "网格要每个数值参数都填步长".into(),
            })
            .color(if v.grid_estimate.is_some_and(|n| n <= so::MAX_GRID) { pal::dim() } else { pal::warn() }),
        );
    }
    col = col.push(sizes);
    // 能不能发：与真正发送走同一个校验（study_from_form）；不能发就把原因写在按钮正下方
    let why = if v.sending { Some("正在发送…".to_string()) } else { v.opt_error.clone() };
    col = col.push(match &why {
        None => w::btn("🔬 开始优化", Kind::Standard, Some(ScMsg::StartStudy)),
        Some(r) => w::btn_why("🔬 开始优化", Kind::Standard, None::<ScMsg>, r.clone()),
    });
    if let Some(r) = &v.opt_error {
        col = col.push(t::caption(format!("还不能开始：{r}")).color(pal::warn()));
    }
    if let Some(a) = &f.assist_id {
        col = col.push(t::metadata(format!("表单来自 AI 草稿 {a}：发起后研究库会记下这次优化出自它")).color(pal::dim()));
    }
    // 发起 / 停止优化的结果写在这里（中栏顶部那行离这儿太远，滚到下面看不见）
    if v.msg.contains("优化") && !v.msg.contains("草稿") {
        col = col.push(t::caption(v.msg.clone()).color(if v.msg.contains("失败") { pal::bad() } else { pal::ok() }));
    }
    col = col.push(
        t::metadata("每次尝试都是一次完整回测，独立进程、不打扰图表；全部计入多重检验——试得越多，DSR 打折越狠。固定参数取上面表单里改过的值。")
            .color(pal::dim()),
    );
    col.into()
}

// ── 右栏「优化」页 ─────────────────────────────────────────────────────

fn gate_badge<'a>(name: &str, g: &Option<Gate>, val: Option<String>) -> Element<'a, ScMsg> {
    let (tone, mark) = match g.as_ref().and_then(|g| g.passed) {
        Some(true) => (Tone::Success, "过"),
        Some(false) => (Tone::Danger, "不过"),
        None => (Tone::Neutral, "算不了"),
    };
    w::badge(format!("{name} {mark}{}", val.map(|x| format!(" {x}")).unwrap_or_default()), tone)
}

fn study_item<'a>(s: &StudyRow, picked: bool) -> Element<'a, ScMsg> {
    let (label, tone) = match s.status.as_str() {
        "running" => ("运行中", Tone::Info),
        "done" => ("完成", Tone::Success),
        "cancelled" => ("已取消", Tone::Neutral),
        _ => ("失败", Tone::Danger),
    };
    let sampler = so::SAMPLERS.iter().find(|(k, _)| *k == s.sampler).map(|(_, l)| *l).unwrap_or("?");
    let mut head = row![
        t::metadata(when(s.created_ts)),
        w::badge(label, tone),
        t::metadata(format!("{sampler} · {} · {}/{}（失败 {}）", s.objective, s.n_done, s.n_trials, s.n_failed)).color(pal::dim()),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if let Some(b) = s.best_value {
        head = head.push(t::numeric(format!("最优 {b:.4}")));
    }
    head = head.push(Space::new().width(Length::Fill));
    if s.active() {
        head = head.push(w::btn("■ 停止", Kind::Destructive, Some(ScMsg::StopStudy(s.study_id.clone()))));
    }
    let mut c = column![head].spacing(0);
    if !s.note.is_empty() {
        c = c.push(t::metadata(format!("备注：{}", s.note)).color(pal::dim()));
    }
    button(c)
        .width(Length::Fill)
        .on_press(ScMsg::PickStudy(s.study_id.clone()))
        .style(move |th, st| w::button_style(if picked { Kind::Standard } else { Kind::Ghost }, th, st))
        .into()
}

fn heat_view<'a>(h: &Heat) -> Element<'a, ScMsg> {
    const CW: f32 = 58.0;
    const CH: f32 = 22.0;
    let vals: Vec<f64> = h.z.iter().flatten().flatten().copied().collect();
    let hi = vals.iter().copied().fold(f64::MIN, f64::max).max(1e-12);
    let lo = vals.iter().copied().fold(f64::MAX, f64::min).min(-1e-12);
    let lbl = |v: &serde_json::Value| -> String {
        match v {
            serde_json::Value::Number(n) => n.as_f64().map(|x| if x.fract() == 0.0 { format!("{x:.0}") } else { format!("{x:.3}") }).unwrap_or_default(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    };
    let mut grid = column![
        row(std::iter::once(container(t::metadata(format!("{} ↓  {} →", h.y, h.x)).color(pal::dim())).width(Length::Fixed(CW * 1.6)).into())
            .chain(h.xs.iter().map(|x| container(t::metadata(lbl(x))).width(Length::Fixed(CW)).into())))
    ]
    .spacing(1);
    for (i, y) in h.ys.iter().enumerate() {
        let mut r = row![container(t::metadata(lbl(y))).width(Length::Fixed(CW * 1.6))].spacing(1);
        for z in h.z.get(i).cloned().unwrap_or_default() {
            let cell: Element<'a, ScMsg> = match z {
                Some(v) => {
                    // 正值按 up 色、负值按 down 色，深浅 = 相对最大 / 最小（颜色不单独承载意义：格子里有数）
                    let (base, k) = if v >= 0.0 { (pal::up(), v / hi) } else { (pal::down(), v / lo) };
                    let bg = iced::Color { a: (0.12 + 0.55 * k.clamp(0.0, 1.0)) as f32, ..base };
                    container(t::metadata(format!("{v:.2}")))
                        .width(Length::Fixed(CW))
                        .height(Length::Fixed(CH))
                        .center_x(Length::Fixed(CW))
                        .center_y(Length::Fixed(CH))
                        .style(move |_| iced::widget::container::Style { background: Some(iced::Background::Color(bg)), ..Default::default() })
                        .into()
                }
                None => container(t::metadata("·").color(pal::dim())).width(Length::Fixed(CW)).height(Length::Fixed(CH)).center_x(Length::Fixed(CW)).into(),
            };
            r = r.push(cell);
        }
        grid = grid.push(r);
    }
    crate::ui::scroll(grid).direction(iced::widget::scrollable::Direction::Horizontal(Default::default())).into()
}

fn study_detail<'a>(s: &StudyRow, v: &View) -> Element<'a, ScMsg> {
    let mut col = column![w::section(format!("优化 · {}", s.study_id))].spacing(space(2));
    if !s.error.is_empty() {
        col = col.push(t::metadata(s.error.clone()).color(pal::bad()));
    }
    let Some(a) = &s.analysis else {
        col = col.push(t::caption(if s.active() {
            format!("进行中：已完成 {}/{}，失败 {}。全部跑完后出 DSR / PBO / 高原分析。", s.n_done, s.n_trials, s.n_failed)
        } else {
            "没有分析结果".into()
        }));
        if let (Some(b), Some(rid)) = (s.best_value, &s.best_run_id) {
            col = col.push(row![t::caption(format!("目前最优 {b:.4} · {}", s.best_params)), w::btn("查看", Kind::Ghost, Some(ScMsg::PickRun(rid.clone())))].spacing(space(2)));
        }
        return col.into();
    };
    if !a.why.is_empty() {
        col = col.push(t::caption(a.why.clone()));
    }
    let dsr_v = a.dsr.as_ref().and_then(|g| g.dsr).map(|x| format!("{x:.2}"));
    let pbo_v = a.pbo.as_ref().and_then(|g| g.pbo).map(|x| format!("{x:.2}"));
    let pl_v = a.plateau.as_ref().and_then(|g| g.mean_ratio).map(|x| format!("{:.0}%", x * 100.0));
    col = col.push(row![gate_badge("DSR", &a.dsr, dsr_v), gate_badge("PBO", &a.pbo, pbo_v), gate_badge("高原", &a.plateau, pl_v)].spacing(space(2)));
    for (name, g) in [("DSR", &a.dsr), ("PBO", &a.pbo), ("高原", &a.plateau)] {
        if let Some(g) = g
            && !g.why.is_empty()
        {
            col = col.push(t::metadata(format!("{name}：{}", g.why)).color(if g.passed == Some(false) { pal::warn() } else { pal::dim() }));
        }
    }
    if let Some(d) = &a.dsr
        && let (Some(n), Some(sr)) = (d.n_trials, d.sr_annual)
    {
        col = col.push(t::metadata(format!("DSR 按 {n:.0} 次尝试打折（这个策略全部完成的回测都算）；最优年化夏普 {sr:.2}；收益口径 {}", a.returns_basis.join("、"))).color(pal::dim()));
    }
    if let (Some(b), Some(rid)) = (s.best_value, &s.best_run_id) {
        col = col.push(
            row![t::caption(format!("最优 {b:.4} · {}", s.best_params)), w::btn("查看这次", Kind::Ghost, Some(ScMsg::PickRun(rid.clone())))]
                .spacing(space(2))
                .align_y(Alignment::Center),
        );
    }
    if !a.importance.is_empty() {
        col = col.push(w::section("参数重要性（fANOVA）"));
        let mut imp: Vec<(&String, &f64)> = a.importance.iter().collect();
        imp.sort_by(|x, y| y.1.total_cmp(x.1));
        for (k, val) in imp {
            col = col.push(row![
                container(t::metadata(k.clone())).width(Length::Fixed(LABEL_W)),
                t::metadata("█".repeat(((val * 20.0).round() as usize).max(1))).color(pal::accent()),
                t::metadata(format!("{val:.2}")).color(pal::dim()),
            ].spacing(space(2)));
        }
    }
    if let Some(h) = &a.heatmap {
        col = col.push(w::section("热力图（每格 = 该组合能达到的最好目标值；找成片的高值，不找孤立的一格）"));
        col = col.push(heat_view(h));
    }
    col = col.push(w::section(format!("试验（共 {}，按目标值排序，点一条看结果）", a.trials.len())));
    for tr in a.trials.iter().take(15) {
        let picked = tr.run_id.is_some() && tr.run_id == v.picked_run;
        let label = format!("#{:<3} {:>10}  {}", tr.number, tr.value.map(|x| format!("{x:.4}")).unwrap_or_default(), tr.params);
        let b = button(t::metadata(label)).width(Length::Fill).style(move |th, st| w::button_style(if picked { Kind::Standard } else { Kind::Ghost }, th, st));
        col = col.push(match &tr.run_id {
            Some(rid) => b.on_press(ScMsg::PickRun(rid.clone())),
            None => b,
        });
    }
    col.into()
}

fn studies<'a>(v: &View) -> Element<'a, ScMsg> {
    if v.selected.is_none() {
        return Space::new().into();
    }
    let mut col = column![].spacing(space(2));
    if v.msg.contains("优化") && !v.msg.contains("草稿") {
        col = col.push(t::caption(v.msg.clone()).color(if v.msg.contains("失败") { pal::bad() } else { pal::ok() }));
    }
    if v.studies.is_empty() && !v.msg.contains("发起优化") {
        return w::empty("这个策略还没做过优化", "在中栏「参数优化」里勾参数、点「🔬 开始优化」");
    }
    let mut list = column![].spacing(space(1));
    for s in &v.studies {
        list = list.push(study_item(s, v.picked_study.as_deref() == Some(s.study_id.as_str())));
    }
    col = col.push(crate::ui::scroll(list).height(Length::FillPortion(2)));
    let focus = v.picked_study.as_ref().and_then(|id| v.studies.iter().find(|s| &s.study_id == id)).or(v.studies.first());
    if let Some(s) = focus {
        col = col.push(crate::ui::scroll(study_detail(s, v)).height(Length::FillPortion(5)));
    }
    col.into()
}

// ── 右栏「验证」页：闸门 · 验证任务 · 实验 ─────────────────────────────

fn gate_row<'a>(g: &sv::Gate) -> Element<'a, ScMsg> {
    let (tone, mark) = match g.status.as_str() {
        "pass" => (Tone::Success, "过"),
        "fail" => (Tone::Danger, "不过"),
        _ => (Tone::Neutral, "算不了"),
    };
    let val = match &g.value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Number(n) => n.as_f64().map(|x| format!("{x:.3}")).unwrap_or_default(),
        other => other.as_str().map(str::to_string).unwrap_or_else(|| other.to_string()),
    };
    let mut c = column![
        row![
            container(w::badge(format!("{} {}", g.id, mark), tone)).width(Length::Fixed(92.0)),
            container(t::caption(g.name.clone())).width(Length::Fixed(80.0)),
            t::numeric(val),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    ]
    .spacing(0);
    let mut sub = g.why.clone();
    if let Some(src) = &g.source {
        sub = format!("{sub}  [{src}]");
    }
    if !sub.trim().is_empty() {
        c = c.push(row![Space::new().width(Length::Fixed(92.0 + space(2))), t::metadata(sub).color(pal::dim())]);
    }
    c.into()
}

fn val_item<'a>(r: &ValRow) -> Element<'a, ScMsg> {
    let (label, tone) = match (r.status.as_str(), r.passed) {
        ("running", _) => ("运行中", Tone::Info),
        ("done", Some(true)) => ("过", Tone::Success),
        ("done", Some(false)) => ("不过", Tone::Danger),
        ("done", None) => ("算不了", Tone::Neutral),
        ("cancelled", _) => ("已取消", Tone::Neutral),
        _ => ("失败", Tone::Danger),
    };
    let mut head = row![t::metadata(when(r.created_ts)), w::badge(label, tone), t::caption(sv::kind_label(&r.kind))]
        .spacing(space(2))
        .align_y(Alignment::Center);
    head = head.push(Space::new().width(Length::Fill));
    if r.active() {
        head = head.push(w::btn("■ 停止", Kind::Destructive, Some(ScMsg::StopValidation(r.val_id.clone()))));
    }
    let mut c = column![head].spacing(0);
    let why = r.why();
    if !why.is_empty() {
        c = c.push(t::metadata(why).color(if r.status == "failed" { pal::bad() } else { pal::dim() }));
    }
    // 成本压力 / 参数扰动：逐行列出；WFO：逐窗列出
    if let Some(rows) = r.result.get("rows").and_then(|x| x.as_array()) {
        for x in rows {
            let pnl = x.get("pnl").and_then(|v| v.as_f64());
            let (txt, col) = signed(pnl, 2);
            c = c.push(row![
                container(t::metadata(x.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string())).width(Length::Fixed(160.0)),
                t::numeric(txt).color(col),
                t::metadata(format!("回撤 {}%", opt(x.get("max_dd_pct").and_then(|v| v.as_f64()), 2, Rounding::Measurement))).color(pal::dim()),
            ].spacing(space(2)));
        }
    }
    if let Some(ws) = r.result.get("windows").and_then(|x| x.as_array()) {
        for (i, x) in ws.iter().enumerate() {
            let te = x.get("test").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|d| d.as_str()).collect::<Vec<_>>().join("…")).unwrap_or_default();
            let (txt, col) = signed(x.get("oos_pnl").and_then(|v| v.as_f64()), 2);
            let mut rr = row![
                container(t::metadata(format!("#{i} 测试 {te}"))).width(Length::Fixed(200.0)),
                t::numeric(txt).color(col),
                t::metadata(format!("参数 {}", x.get("params").cloned().unwrap_or_default())).color(pal::dim()),
            ].spacing(space(2));
            if let Some(e) = x.get("error").and_then(|v| v.as_str()) {
                rr = rr.push(t::metadata(e.to_string()).color(pal::bad()));
            }
            c = c.push(rr);
        }
    }
    container(c).width(Length::Fill).into()
}

fn experiment_block<'a>(v: &View) -> Element<'a, ScMsg> {
    const KW: f32 = 110.0;
    let f = &v.eform;
    let mut col = column![w::section("实验（预注册：假设与规则先于数据锁死，留出数据只许跑一次）")].spacing(space(2));
    let current: Option<&ExpRow> = v.experiments.iter().find(|x| x.locked());
    match current {
        Some(x) => {
            col = col.push(w::kv("实验", t::body(x.exp_id.clone()).into(), KW));
            col = col.push(w::kv("假设", t::caption(x.hypothesis.clone()).into(), KW));
            col = col.push(w::kv("回撤上限", t::numeric(x.dd_limit.map(|d| format!("{d}%")).unwrap_or_else(|| "未声明（G7 算不了）".into())).into(), KW));
            col = col.push(w::kv(
                "留出窗口",
                t::caption(match &x.holdout {
                    Some((a, b)) => format!("{a} → {b}{}", x.holdout_run_id.as_ref().map(|r| format!("（已跑：{r}）")).unwrap_or_else(|| "（未跑）".into())),
                    None => "无".into(),
                })
                .into(),
                KW,
            ));
            col = col.push(t::metadata("这个实验下发起的优化 / 验证会自动挂上实验号；它们的数据窗口碰到留出窗口会被拒。").color(pal::dim()));
            if x.holdout.is_some() && x.holdout_run_id.is_none() {
                col = col.push(w::btn(
                    if f.holdout_armed { "⚠ 再点一次：确认跑留出数据" } else { "跑留出数据（只此一次）" },
                    if f.holdout_armed { Kind::Destructive } else { Kind::Standard },
                    Some(ScMsg::HoldoutRun),
                ));
                col = col.push(t::metadata("用的参数 = 上面详情表单里当前的值。跑完就不能再改参数重跑。").color(pal::dim()));
            }
            let verdicts = [("alive", "存活"), ("falsified", "证伪"), ("inconclusive", "不确定")].iter().map(|(k, l)| {
                w::btn(*l, if f.verdict == *k { Kind::Standard } else { Kind::Ghost }, Some(ScMsg::ConcludeVerdict((*k).to_string())))
            });
            col = col.push(row![container(t::caption("结论")).width(Length::Fixed(KW)), row(verdicts).spacing(space(1))].spacing(space(2)).align_y(Alignment::Center));
            col = col.push(row![
                container(t::caption("说明")).width(Length::Fixed(KW)),
                text_input("依据哪些闸门 / 留出结果", &f.conclusion).on_input(ScMsg::ConcludeText).size(t::s_small()),
            ].spacing(space(2)).align_y(Alignment::Center));
            col = col.push(w::btn("下结论并关闭实验", Kind::Standard, (!v.sending).then_some(ScMsg::Conclude)));
        }
        None => {
            col = col.push(t::metadata("当前没有进行中的实验。先写下假设与回撤上限、留出一段数据，再开始研究。").color(pal::dim()));
            col = col.push(row![container(t::caption("假设")).width(Length::Fixed(KW)),
                text_input("如：风险等级 1 下费后为正且回撤 < 40%", &f.hypothesis).on_input(ScMsg::ExpHypothesis).size(t::s_small())]
                .spacing(space(2)).align_y(Alignment::Center));
            col = col.push(row![container(t::caption("回撤上限 %")).width(Length::Fixed(KW)),
                text_input("40", &f.dd_limit).on_input(ScMsg::ExpDdLimit).size(t::s_small()).width(Length::Fixed(80.0))]
                .spacing(space(2)).align_y(Alignment::Center));
            col = col.push(row![container(t::caption("留出窗口")).width(Length::Fixed(KW)),
                text_input("YYYY-MM-DD", &f.holdout_start).on_input(ScMsg::ExpHoldoutStart).size(t::s_small()).width(Length::Fixed(104.0)),
                t::caption("→"),
                text_input("YYYY-MM-DD", &f.holdout_end).on_input(ScMsg::ExpHoldoutEnd).size(t::s_small()).width(Length::Fixed(104.0))]
                .spacing(space(2)).align_y(Alignment::Center));
            col = col.push(w::btn("锁定实验", Kind::Standard, (!v.sending).then_some(ScMsg::ExpCreate)));
        }
    }
    for x in v.experiments.iter().filter(|x| !x.locked()).take(5) {
        col = col.push(t::metadata(format!(
            "{} · {} · {}：{}",
            when(x.created_ts),
            x.exp_id,
            match x.verdict.as_deref() { Some("alive") => "存活", Some("falsified") => "证伪", _ => "不确定" },
            if x.conclusion.is_empty() { x.hypothesis.clone() } else { x.conclusion.clone() }
        )).color(pal::dim()));
    }
    col.into()
}

fn checks<'a>(v: &View) -> Element<'a, ScMsg> {
    if v.selected.is_none() {
        return Space::new().into();
    }
    let mut col = column![].spacing(space(2));
    if v.msg.contains("验证") || v.msg.contains("实验") || v.msg.contains("留出") || v.msg.contains("结论") {
        col = col.push(t::caption(v.msg.clone()).color(if v.msg.contains("失败") || v.msg.contains("没") { pal::bad() } else { pal::ok() }));
    }
    // 闸门
    col = col.push(row![w::section("闸门清单（不打总分：每道过 / 不过 / 算不了）"), Space::new().width(Length::Fill),
        w::btn_busy("重算", Kind::Ghost, Some(ScMsg::GatesRefresh), v.gates_loading)].align_y(Alignment::Center));
    if !v.gates_err.is_empty() {
        col = col.push(t::metadata(v.gates_err.clone()).color(pal::bad()));
    }
    match &v.gates {
        Some(g) => {
            for x in &g.gates {
                col = col.push(gate_row(x));
            }
            if let Some(h) = &g.holdout {
                col = col.push(gate_row(h));
            }
            col = col.push(t::caption(g.verdict.clone()).color(if g.verdict.contains("道全过——") { pal::ok() } else { pal::warn() }));
        }
        None => col = col.push(if v.gates_loading { w::loading("闸门") } else { t::metadata("（切到本页时计算）").color(pal::dim()).into() }),
    }
    // 发起验证
    col = col.push(w::section("发起验证（参数取详情表单当前值；WFO 的搜索空间取「参数优化」表单）"));
    let kinds = sv::KINDS.iter().map(|(k, l)| w::btn(*l, Kind::Standard, (!v.sending).then(|| ScMsg::StartValidation((*k).to_string()))));
    col = col.push(row(kinds).spacing(space(2)));
    col = col.push(row![
        t::caption("WFO 训练 / 测试天数"),
        text_input("60", &v.vform.train_days).on_input(ScMsg::WfoTrain).size(t::s_small()).width(Length::Fixed(60.0)),
        t::caption("/"),
        text_input("30", &v.vform.test_days).on_input(ScMsg::WfoTest).size(t::s_small()).width(Length::Fixed(60.0)),
        t::metadata("整体区间 = 详情里的回测窗口").color(pal::dim()),
    ].spacing(space(2)).align_y(Alignment::Center));
    // 验证任务
    col = col.push(w::section(format!("验证记录（{}）", v.validations.len())));
    for r in v.validations.iter().take(12) {
        col = col.push(val_item(r));
    }
    col = col.push(experiment_block(v));
    col.into()
}

// ── 右栏：对比与组合（docs/37 P5）────────────────────────────────────

/// 一格定宽文字（对比表用）。
fn cellw<'a>(e: impl Into<Element<'a, ScMsg>>, wd: f32) -> Element<'a, ScMsg> {
    container(e).width(Length::Fixed(wd)).into()
}

fn short_id(id: &str) -> String {
    id.rsplit('-').next().unwrap_or(id).to_string()
}

fn compare<'a>(v: &View) -> Element<'a, ScMsg> {
    let mut col = column![w::section(format!("对比篮（可跨策略，最多 {} 个；切策略不清空）", sc::MAX_BASKET))].spacing(space(2));
    if v.basket.is_empty() {
        col = col.push(w::empty("对比篮是空的", "在「运行记录」里点运行旁边的「＋对比」——换个策略再点，就能跨策略比"));
        return col.into();
    }
    for (i, b) in v.basket.iter().enumerate() {
        col = col.push(
            row![
                t::label("■").color(pal::series(i)),
                t::numeric(format!("[{i}]")),
                t::body(b.strategy.clone()),
                t::metadata(format!("{} · {}", when(b.created_ts), if b.brief.is_empty() { "缺省参数".into() } else { b.brief.clone() })).color(pal::dim()),
                Space::new().width(Length::Fill),
                w::btn("✕", Kind::Ghost, Some(ScMsg::CmpToggle(b.run_id.clone()))),
            ]
            .spacing(space(2))
            .align_y(Alignment::Center),
        );
    }
    let freq = v.cmp_freq.clone();
    col = col.push(
        row![
            w::segmented(&sc::FREQS.iter().map(|(k, l)| (*l, k.to_string())).collect::<Vec<_>>(), &freq, ScMsg::CmpFreq),
            Space::new().width(Length::Fill),
            w::btn("清空", Kind::Ghost, Some(ScMsg::CmpClear)),
            w::btn_busy("重新计算", Kind::Primary, (v.basket.len() >= 2).then_some(ScMsg::CmpRun), v.cmp_loading),
        ]
        .spacing(space(2))
        .align_y(Alignment::Center),
    );
    if v.basket.len() < 2 {
        col = col.push(t::metadata("至少放 2 次运行才能比").color(pal::dim()));
        return col.into();
    }
    if !v.cmp_err.is_empty() {
        col = col.push(w::error("对比没算出来", v.cmp_err.clone(), "常见原因：几次运行的时间段没有交集", None));
    }
    match &v.cmp {
        Some(c) => col = col.push(compare_result(c)),
        None if v.cmp_loading => col = col.push(w::loading("对齐收益序列")),
        None => {}
    }
    col.into()
}

fn compare_result<'a>(c: &Cmp) -> Element<'a, ScMsg> {
    let mut col = column![].spacing(space(2));
    for wn in &c.warnings {
        col = col.push(t::metadata(format!("⚠ {wn}")).color(pal::warn()));
    }
    // 统计并排
    col = col.push(w::section(format!("统计并排（{}；累计 / 回撤 / 夏普按分桶算，「运行回撤」是运行自己的）", c.freq_label)));
    const C0: f32 = 34.0;
    const C1: f32 = 170.0;
    const CN: f32 = 84.0;
    let head = |s: &'static str, wd: f32| cellw(t::metadata(s).color(pal::dim()), wd);
    col = col.push(row![head("", C0), head("策略 · 时段", C1), head("累计", CN), head("分桶回撤", CN), head("运行回撤", CN),
                        head("年化波动", CN), head("夏普", CN), head("口径", CN)].spacing(space(1)));
    for (i, r) in c.runs.iter().enumerate() {
        let u = r.unit_label();
        let (tot, tc) = signed(r.total, 2);
        col = col.push(
            row![
                cellw(t::numeric(format!("[{i}]")).color(pal::series(i)), C0),
                cellw(column![t::body(r.strategy.clone()), t::metadata(format!("{} → {}", &r.start[..r.start.len().min(10)], &r.end[..r.end.len().min(10)])).color(pal::dim())], C1),
                cellw(t::numeric(format!("{tot} {u}")).color(tc), CN),
                cellw(t::numeric(format!("{} {u}", opt(r.max_dd, 2, Rounding::Measurement))), CN),
                cellw(t::numeric(match r.run_dd_pct() { Some(x) => format!("{x:.2} %"), None => crate::ui::fmt::na() }), CN),
                cellw(t::numeric(format!("{} {u}", opt(r.vol_ann, 1, Rounding::Measurement))), CN),
                cellw(t::numeric(opt(r.sharpe, 2, Rounding::Measurement)), CN),
                cellw(t::metadata(basis(&Some(r.basis.clone()))).color(pal::dim()), CN),
            ]
            .spacing(space(1))
            .align_y(Alignment::Center),
        );
    }
    // 收益曲线叠加
    let units: std::collections::BTreeSet<&str> = c.runs.iter().map(|r| r.unit.as_str()).collect();
    let unit = if units.len() > 1 { "累计（% / bp 混合）" } else if units.contains("bp") { "累计 bp" } else { "累计 %" };
    col = col.push(w::section("收益曲线（各自从 0 起算，窗口外不画）"));
    let series = c.curves.series.iter().enumerate().map(|(i, s)| (s.clone(), pal::series(i), format!("[{i}]"))).collect();
    col = col.push(iced::widget::Canvas::new(sc::MultiLine { t: c.curves.t.clone(), series, unit: unit.into(), cache: Default::default() })
        .width(Length::Fill).height(Length::Fixed(200.0)));
    // 相关矩阵
    col = col.push(w::section("收益相关矩阵（两两只用重叠期；格子下方小字 = 重叠期数）"));
    col = col.push(corr_view(c));
    // 组合
    let p = &c.portfolio;
    col = col.push(w::section(format!("组合（共同区间 {} 期{}）", p.periods,
        match (&p.start, &p.end) { (Some(a), Some(b)) => format!("，{} → {}", &a[..a.len().min(16)], &b[..b.len().min(16)]), _ => String::new() })));
    if !p.why.is_empty() {
        col = col.push(t::metadata(p.why.clone()).color(pal::dim()));
    }
    if !p.methods.is_empty() {
        let pu = if p.methods[0].unit == "bp" { "bp" } else { "%" };
        col = col.push(row![head("配权", 90.0), head("权重", 200.0), head("累计", CN), head("回撤", CN), head("夏普", CN), head("分散比", CN)].spacing(space(1)));
        for (k, m) in p.methods.iter().enumerate() {
            let (tot, tc) = signed(m.total, 2);
            let ws = m.weights.iter().enumerate().map(|(i, x)| format!("[{i}]{x:.2}")).collect::<Vec<_>>().join(" ");
            col = col.push(
                row![
                    cellw(t::body(m.label.clone()).color(pal::series(c.runs.len() + k)), 90.0),
                    cellw(t::metadata(ws), 200.0),
                    cellw(t::numeric(format!("{tot} {pu}")).color(tc), CN),
                    cellw(t::numeric(format!("{} {pu}", opt(m.max_dd, 2, Rounding::Measurement))), CN),
                    cellw(t::numeric(opt(m.sharpe, 2, Rounding::Measurement)), CN),
                    cellw(t::numeric(opt(m.diversification, 2, Rounding::Measurement)), CN),
                ]
                .spacing(space(1))
                .align_y(Alignment::Center),
            );
        }
        let series = p.methods.iter().enumerate()
            .map(|(k, m)| (m.curve.iter().map(|x| Some(*x)).collect(), pal::series(c.runs.len() + k), m.label.clone()))
            .collect();
        col = col.push(iced::widget::Canvas::new(sc::MultiLine { t: p.t.clone(), series, unit: format!("组合累计 {pu}"), cache: Default::default() })
            .width(Length::Fill).height(Length::Fixed(180.0)));
    }
    col = col.push(
        t::metadata("权重按整段共同区间事后估计（等权 / 逆波动 / 等风险贡献，复用 classic.risk_parity），读作「这几条放一起的风险结构」，\
                     不是可交易的配权回测；分散比 = 加权单体波动之和 ÷ 组合波动（1 = 没分散）。对比只读，不进闸门。")
            .color(pal::dim()),
    );
    col.into()
}

fn corr_view<'a>(c: &Cmp) -> Element<'a, ScMsg> {
    const CW: f32 = 64.0;
    const CH: f32 = 34.0;
    let n = c.runs.len();
    let mut grid = column![row(std::iter::once(cellw(t::metadata(""), CW))
        .chain((0..n).map(|j| cellw(t::numeric(format!("[{j}]")).color(pal::series(j)), CW))))]
    .spacing(1);
    for i in 0..n {
        let mut r = row![cellw(t::numeric(format!("[{i}] {}", short_id(&c.runs[i].run_id))).color(pal::series(i)), CW)].spacing(1);
        for j in 0..n {
            let rho = c.corr.matrix.get(i).and_then(|x| x.get(j)).copied().flatten();
            let ov = c.corr.overlap.get(i).and_then(|x| x.get(j)).copied().unwrap_or(0);
            // 相关不是涨跌：用强调色，深浅 = |ρ|（格子里有数，颜色不单独承载意义）
            let bg = rho.map(|x| pal::alpha(pal::accent(), (0.08 + 0.6 * x.abs()) as f32));
            let body = column![
                t::numeric(rho.map(|x| format!("{x:+.2}")).unwrap_or_else(|| "—".into())),
                t::metadata(format!("{ov}")).color(pal::dim()),
            ]
            .align_x(Alignment::Center);
            r = r.push(
                container(body)
                    .width(Length::Fixed(CW))
                    .height(Length::Fixed(CH))
                    .center_x(Length::Fixed(CW))
                    .center_y(Length::Fixed(CH))
                    .style(move |_| iced::widget::container::Style { background: bg.map(iced::Background::Color), ..Default::default() }),
            );
        }
        grid = grid.push(r);
    }
    crate::ui::scroll(grid).direction(iced::widget::scrollable::Direction::Horizontal(Default::default())).into()
}

// ── 交易大师百科卡（docs/38）─────────────────────────────────────────

fn stars(n: u8) -> String {
    let n = n.min(5) as usize;
    format!("{}{}", "★".repeat(n), "☆".repeat(5 - n))
}

fn delivery_label(d: &str) -> &'static str {
    match d {
        "code" => "完整代码",
        "proxy" => "代理",
        "card" => "卡片",
        _ => "—",
    }
}

fn origin_tone(o: &str) -> Tone {
    match o {
        "原文" => Tone::Success,
        "解读" => Tone::Info,
        "代理" => Tone::Warning,
        _ => Tone::Neutral,
    }
}

fn master_card<'a>(mi: &super::strategy_center::MasterInfo) -> Element<'a, ScMsg> {
    const KW: f32 = 96.0;
    let mut col = column![w::section(format!("百科卡 · {}", super::strategy_center::school_label(&mi.school)))].spacing(space(1));
    col = col.push(w::kv("大师", t::body(if mi.era.is_empty() { mi.name.clone() } else { format!("{}（{}）", mi.name, mi.era) }).into(), KW));
    col = col.push(w::kv(
        "公开度 / 交付",
        row![t::body(stars(mi.openness)), w::badge(delivery_label(&mi.delivery), if mi.delivery == "code" { Tone::Success } else { Tone::Warning })]
            .spacing(space(2))
            .align_y(Alignment::Center)
            .into(),
        KW,
    ));
    if !mi.verification.is_empty() {
        col = col.push(w::kv("调研核验", t::body(mi.verification.clone()).into(), KW));
    }
    if !mi.markets.is_empty() || !mi.timeframe.is_empty() {
        col = col.push(w::kv("市场 / 周期", t::body(format!("{} · {}", mi.markets, mi.timeframe)).into(), KW));
    }
    let mut layers = row![].spacing(space(1));
    for (k, label) in super::strategy_center::LAYERS {
        if let Some(o) = mi.rules.get(k) {
            layers = layers.push(w::badge(format!("{label} {o}"), origin_tone(o)));
        }
    }
    col = col.push(w::kv("七层规则", layers.into(), KW));
    for (i, s) in mi.sources.iter().enumerate() {
        col = col.push(w::kv(if i == 0 { "出处" } else { "" }, t::caption(s.clone()).into(), KW));
    }
    if !mi.decay.is_empty() {
        col = col.push(w::kv("衰减 / 反证", t::caption(mi.decay.clone()).color(pal::warn()).into(), KW));
    }
    if let Some(of) = mi.orderflow_value {
        col = col.push(w::kv("订单流价值", t::body(stars(of)).into(), KW));
    }
    col = col.push(t::metadata("七层规则：原文 = 出自大师本人著作 / 论文；解读 = 原文有思想、数值是我们定的；代理 = 原方法不可得，用可证伪的近似；不可得 = 专有。").color(pal::dim()));
    col.into()
}

fn doc_section<'a>(e: &Entry) -> Element<'a, ScMsg> {
    if e.doc.is_empty() {
        return Space::new().into();
    }
    column![w::section("规则与出处（策略模块文档）"), t::code(e.doc.clone()).size(t::s_small())].spacing(space(1)).into()
}

