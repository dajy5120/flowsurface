//! 「交易核对」视图（docs/56 §3、§6）：顶栏（运行 · 跟随 · 导入 · 进度 · 汇总）｜
//! 左栏（筛选 · 逐笔表 ∣ 成交明细与核对）｜右栏（全景 · 秒级 K 线 · 当天 1 分钟 K 线）。
//! 只读 [`super::trade_review::view`] 的快照，不做 IO。
//!
//! 三张 K 线自绘：滚轮缩放、左键拖动平移、右键复位、十字线读数；开仓 ▲▼、平仓 ● 画在**成交价**上
//! （不是贴在 K 线上下），开平价横线、开平连线（赚涨色、亏跌色）、买一 / 卖一阶梯线。
//! 点标记选中那一笔；全景图上选中的跑出视野时视野跟过去。鼠标在图上时 ←/→ 换笔，1 正确，2 有疑问。

use std::sync::Arc;

use iced::widget::canvas::{self, Frame, Geometry, LineDash, Path, Stroke, Text};
use iced::widget::{Space, column, container, pick_list, row, text_input};
use iced::{Alignment, Background, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, keyboard, mouse};

use super::trade_review::{self as tr, Status, Trade, TrMsg, View};
use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind, Tone};

// ── 选择框的选项 ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
struct RunOpt {
    dir: String,
    label: String,
}

impl std::fmt::Display for RunOpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Opt<V> {
    v: V,
    label: String,
}

impl<V> std::fmt::Display for Opt<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn state_word(m: &tr::RunMeta) -> (&'static str, Tone) {
    match m.state.as_str() {
        "running" if m.stale() => ("运行中？10 分钟没更新", Tone::Warning),
        "running" => ("运行中", Tone::Accent),
        "done" => ("完成", Tone::Success),
        "failed" => ("失败", Tone::Danger),
        "stopped" => ("已停止", Tone::Warning),
        _ => ("—", Tone::Neutral),
    }
}

fn fmt_utc(ns: i64, f: &str) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(ns).format(f).to_string()
}

fn money(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |x| format!("{x:+.2}"))
}

fn px(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |x| format!("{x:.1}"))
}

fn tone_of(v: Option<f64>) -> Color {
    match v {
        Some(x) if x > 0.0 => pal::up(),
        Some(x) if x < 0.0 => pal::down(),
        _ => pal::txt(),
    }
}

// ── 面板 ───────────────────────────────────────────────────────────────

pub fn pane_body<'a>() -> Element<'a, TrMsg> {
    let v = tr::view();
    let mut col = column![top_bar(&v)].spacing(space(2));
    if let Some(m) = &v.meta {
        col = col.push(status_bar(&v, m));
    }
    if !v.note.is_empty() {
        col = col.push(t::metadata(v.note.clone()).color(pal::dim()));
    }
    if v.sel.is_none() {
        col = col.push(w::empty(
            "还没有交易清单",
            "跑一次回测（完整引擎 / fast run / 策略中心）会自动出现在这里；已有的交易文件填路径后点「导入」",
        ));
        return container(col).padding(space(2)).into();
    }
    let left = left_column(&v);
    let right = right_column(&v);
    col = col.push(crate::ui::split::row("trade_review.main", vec![(left, 0.36, 300.0), (right, 0.64, 360.0)], TrMsg::Split));
    container(col.height(Length::Fill)).padding(space(2)).into()
}

fn top_bar<'a>(v: &View) -> Element<'a, TrMsg> {
    let opts: Vec<RunOpt> = v
        .runs
        .iter()
        .map(|r| {
            let (s, _) = state_word(&r.meta);
            let name = if r.meta.name.is_empty() { r.dir.clone() } else { r.meta.name.clone() };
            // 选中的那次用面板已读到的笔数（列表每 3 秒才刷新一次）
            let n = if v.sel.as_deref() == Some(r.dir.as_str()) { v.trades.len().max(r.n) } else { r.n };
            RunOpt { dir: r.dir.clone(), label: format!("{}{name} · {n} 笔 · {s}", if r.meta.running() && !r.meta.stale() { "● " } else { "" }) }
        })
        .collect();
    let cur = v.sel.as_ref().and_then(|d| opts.iter().find(|o| &o.dir == d).cloned());
    row![
        t::label("运行").color(pal::dim()),
        pick_list(opts, cur, |o: RunOpt| TrMsg::SelectRun(o.dir)).text_size(t::s_small()).placeholder("选一次运行"),
        w::btn(if v.follow { "✓ 跟随最新运行" } else { "跟随最新运行" }, if v.follow { Kind::Standard } else { Kind::Ghost }, Some(TrMsg::Follow(!v.follow))),
        Space::new().width(Length::Fixed(space(4))),
        text_input("导入：交易文件或结果目录的路径（trades.csv / result.json / *.json）", &v.import_path)
            .on_input(TrMsg::ImportPath)
            .on_submit(TrMsg::Import)
            .size(t::s_small())
            .width(Length::Fixed(380.0)),
        w::btn_busy("导入", Kind::Subtle, (!v.import_path.trim().is_empty()).then_some(TrMsg::Import), v.import_busy),
        w::btn_why(
            "删除",
            Kind::Ghost,
            v.sel.as_ref().is_some_and(|s| s.starts_with("import-")).then_some(TrMsg::Delete),
            "只能删导入的清单",
        ),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center)
    .wrap()
    .into()
}

fn progress_bar<'a>(frac: f32, c: Color) -> Element<'a, TrMsg> {
    const W: f32 = 200.0;
    let frac = frac.clamp(0.0, 1.0);
    container(
        container(Space::new())
            .width(Length::Fixed((W * frac).max(1.0)))
            .height(Length::Fixed(8.0))
            .style(move |_: &Theme| container::Style { background: Some(Background::Color(c)), border: iced::border::rounded(3), ..Default::default() }),
    )
    .width(Length::Fixed(W))
    .height(Length::Fixed(8.0))
    .style(|_: &Theme| container::Style { background: Some(Background::Color(pal::band())), border: iced::border::rounded(3), ..Default::default() })
    .into()
}

fn status_bar<'a>(v: &View, m: &tr::RunMeta) -> Element<'a, TrMsg> {
    let (s, tone) = state_word(m);
    let mut r = row![w::badge(s, tone)].spacing(space(3)).align_y(Alignment::Center);
    if m.running() || m.state == "stopped" || m.state == "failed" {
        if let Some(p) = m.progress.pct {
            r = r.push(progress_bar(p as f32, tone.color()));
            r = r.push(t::numeric(format!("{:.0}%", p * 100.0)));
        }
        if let (Some(a), Some(b)) = (m.progress.days_done, m.progress.days_total) {
            r = r.push(t::metadata(format!("{a}/{b} 天")).color(pal::dim()));
        }
        if let Some(ns) = m.progress.now_ns {
            r = r.push(t::metadata(format!("模拟时钟 {}", fmt_utc(ns, "%m-%d %H:%M"))).color(pal::dim()));
        }
    }
    let tr_ = &v.trades;
    let gross: f64 = tr_.iter().filter_map(|x| x.gross).sum();
    let fee: f64 = tr_.iter().filter_map(|x| x.fee).sum();
    let has_fee = tr_.iter().any(|x| x.fee.is_some());
    let net: f64 = tr_.iter().map(|x| x.net.or(x.gross).unwrap_or(0.0)).sum();
    let win = tr_.iter().filter(|x| x.gross.unwrap_or(0.0) > 0.0).count();
    r = r.push(t::numeric(format!("{} 笔", tr_.len())));
    r = r.push(row![t::metadata("毛").color(pal::dim()), t::numeric(format!("{gross:+.2}")).color(tone_of(Some(gross)))].spacing(space(1)));
    if has_fee {
        r = r.push(row![t::metadata("手续费").color(pal::dim()), t::numeric(format!("{fee:.2}"))].spacing(space(1)));
        r = r.push(row![t::metadata("净").color(pal::dim()), t::numeric(format!("{net:+.2}")).color(tone_of(Some(net)))].spacing(space(1)));
    }
    r = r.push(t::metadata(format!("毛利为正 {win} 笔")).color(pal::dim()));
    let mut what = vec![];
    if let Some(sym) = &m.symbol {
        what.push(sym.clone());
    }
    if let (Some(a), Some(b)) = (&m.start, &m.end) {
        what.push(if a == b { a.clone() } else { format!("{a}…{b}") });
    }
    if let Some(l) = &m.label {
        what.push(l.clone());
    }
    if let Some(n) = &m.note {
        what.push(n.clone());
    }
    r = r.push(t::metadata(what.join(" · ")).color(pal::dim()));
    r.wrap().into()
}


// ── 左栏：筛选 · 逐笔表 ∣ 成交明细与核对 ───────────────────────────────

fn left_column<'a>(v: &View) -> Element<'a, TrMsg> {
    let mut lanes = vec![Opt { v: None, label: "全部线".to_string() }];
    lanes.extend(v.lanes.iter().map(|l| Opt { v: Some(l.clone()), label: l.clone() }));
    let lane_cur = lanes.iter().find(|o| o.v == v.f_lane).cloned();
    let dirs = vec![
        Opt { v: None, label: "多空".to_string() },
        Opt { v: Some("long".to_string()), label: "多".to_string() },
        Opt { v: Some("short".to_string()), label: "空".to_string() },
    ];
    let dir_cur = dirs.iter().find(|o| o.v == v.f_dir).cloned();
    let mut reasons = vec![Opt { v: None, label: "全部离场原因".to_string() }];
    reasons.extend(v.reasons.iter().map(|l| Opt { v: Some(l.clone()), label: l.clone() }));
    let reason_cur = reasons.iter().find(|o| o.v == v.f_reason).cloned();
    let stats = vec![
        Opt { v: Status::All, label: "全部状态".to_string() },
        Opt { v: Status::None, label: "未核对".to_string() },
        Opt { v: Status::Ok, label: "正确".to_string() },
        Opt { v: Status::Doubt, label: "有疑问".to_string() },
    ];
    let st_cur = stats.iter().find(|o| o.v == v.f_status).cloned();
    let filters = row![
        pick_list(lanes, lane_cur, |o: Opt<Option<String>>| TrMsg::Lane(o.v)).text_size(t::s_small()),
        pick_list(dirs, dir_cur, |o: Opt<Option<String>>| TrMsg::Dir(o.v)).text_size(t::s_small()),
        pick_list(reasons, reason_cur, |o: Opt<Option<String>>| TrMsg::Reason(o.v)).text_size(t::s_small()),
        pick_list(stats, st_cur, |o: Opt<Status>| TrMsg::Status(o.v)).text_size(t::s_small()),
    ]
    .spacing(space(1))
    .wrap();
    let ok = v.trades.iter().filter(|x| v.status_of(x) == Some("ok")).count();
    let doubt = v.trades.iter().filter(|x| v.status_of(x) == Some("doubt")).count();
    let prog = row![
        t::metadata(format!("已核对 {}/{} · 正确 {ok} · 有疑问 {doubt} · 显示 {} 笔", ok + doubt, v.trades.len(), v.shown.len())).color(pal::dim()),
        w::btn("复制核对结果", Kind::Subtle, (ok + doubt > 0 || v.review.values().any(|r| !r.note.is_empty())).then_some(TrMsg::Copy)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center)
    .wrap();

    let table: Element<'a, TrMsg> = if v.trades.is_empty() {
        let running = v.meta.as_ref().is_some_and(|m| m.running());
        w::empty(if running { "还没有平掉的笔" } else { "这份清单是空的" }, if running { "运行中：每平一笔（快速档每跑完一天）这里就多一行" } else { "" })
    } else {
        trade_table(v)
    };
    let list = column![filters, prog, table].spacing(space(2)).height(Length::Fill);
    let detail: Element<'a, TrMsg> = match v.cur.filter(|i| *i < v.trades.len()) {
        Some(i) => {
            let x = &v.trades[i];
            let win = v.charts.get(&x.day).and_then(|c| c.window(&x.key()).cloned());
            detail_card(v, i, x, win.as_ref())
        }
        None => w::empty("没有选中的笔", "在表里或图上点一笔"),
    };
    crate::ui::split::column("trade_review.left", vec![(list.into(), 0.62, 160.0), (detail, 0.38, 140.0)], TrMsg::Split)
}

/// 逐笔表（`crate::ui::grid`：表头排序、拖列宽、按线 / 方向 / 离场原因分组、右键复制）。
/// 行 = 筛选后的笔，行下标 = 在 `v.shown` 里的位置（`trade_review::TABLE` 的约定）。
fn trade_table<'a>(v: &View) -> Element<'a, TrMsg> {
    use crate::ui::fmt::Absence;
    use crate::ui::grid::{Cell, Column, Fit, Named};
    let cols = vec![
        Column::text("核对", 52.0),
        Column::num("#", None, 40.0).key(),
        Column::text("开仓 (UTC)", 112.0),
        Column::text("线", 72.0).groupable(),
        Column::text("向", 34.0).groupable(),
        Column::num("持仓", Some("秒"), 52.0),
        Column::num("开仓价", None, 76.0),
        Column::num("平仓价", None, 76.0),
        Column::num("数量", None, 56.0),
        Column::num("毛", Some("USDT"), 72.0),
        Column::num("手续费", Some("USDT"), 66.0),
        Column::num("净", Some("USDT"), 72.0),
        Column::text("离场原因", 96.0).groupable(),
    ];
    let opt_num = |o: Option<f64>, dp: usize| o.map_or(Cell::Absent(Absence::Missing), |x| Cell::num(x, format!("{x:.dp$}")));
    let opt_money = |o: Option<f64>| o.map_or(Cell::Absent(Absence::Missing), |x| Cell::ColoredNum(x, format!("{x:+.2}"), tone_of(Some(x))));
    let rows: Vec<Vec<Cell>> = v
        .shown
        .iter()
        .map(|&i| {
            let x = &v.trades[i];
            vec![
                match v.status_of(x) {
                    Some("ok") => Cell::Badge("正确".into(), Tone::Success),
                    Some("doubt") => Cell::Badge("疑问".into(), Tone::Warning),
                    _ => Cell::Text("—".into()),
                },
                Cell::num((i + 1) as f64, format!("{}", i + 1)),
                Cell::Text(fmt_utc(x.open_ns, "%m-%d %H:%M:%S")),
                Cell::Text(x.lane.clone()),
                Cell::Colored(if x.long() { "▲ 多".into() } else { "▼ 空".into() }, if x.long() { long_c() } else { short_c() }),
                Cell::num(x.hold_s(), format!("{:.1}", x.hold_s())),
                opt_num(x.entry, 1),
                opt_num(x.exit, 1),
                opt_num(x.qty, 3),
                opt_money(x.gross),
                opt_num(x.fee, 2),
                opt_money(x.net),
                Cell::Text(x.reason_or_dash()),
            ]
        })
        .collect();
    // 表外选中（图上点、键盘换笔）同步进表里的高亮
    crate::ui::grid::named_select(tr::TABLE, v.cur.and_then(|c| v.shown.iter().position(|x| *x == c)));
    crate::ui::grid::named(tr::TABLE, cols, rows, Fit::Fill, None, |m| TrMsg::Table(Named(tr::TABLE.to_string(), m)))
}

fn detail_card<'a>(v: &View, i: usize, x: &Trade, win: Option<&tr::Window>) -> Element<'a, TrMsg> {
    let long = x.long();
    let kv = |k: &str, val: Element<'a, TrMsg>| -> Element<'a, TrMsg> { column![t::metadata(k.to_string()).color(pal::dim()), val].spacing(2).width(Length::Fixed(200.0)).into() };
    let fill = |p: Option<f64>, touch: Option<[f64; 2]>, buy: bool| -> String {
        match touch {
            // 吃单：买在卖一、卖在买一——成交价应正好在这一侧（估算成交另加冲击）
            Some([b, a]) => format!("{}（{} · 买一 {b:.1} 卖一 {a:.1}）", px(p), if buy { "买，对卖一" } else { "卖，对买一" }),
            None => px(p),
        }
    };
    let (ot, ct) = (win.and_then(|w| w.open_touch), win.and_then(|w| w.close_touch));
    let net = x.net;
    let cells = row![
        kv("交易", row![t::body(format!("#{} · {} · ", i + 1, x.lane)), t::body(if long { "多" } else { "空" }).color(if long { long_c() } else { short_c() })].into()),
        kv("开仓 (UTC)", t::numeric(fmt_utc(x.open_ns, "%Y-%m-%d %H:%M:%S%.3f")).into()),
        kv("平仓 (UTC)", t::numeric(format!("{}（{:.1} 秒）", fmt_utc(x.close_ns, "%H:%M:%S%.3f"), x.hold_s())).into()),
        kv("离场原因", t::body(x.reason_or_dash()).into()),
        kv("开仓价", t::numeric(fill(x.entry, ot, long)).into()),
        kv("平仓价", t::numeric(fill(x.exit, ct, !long)).into()),
        kv("数量", t::numeric(x.qty.map_or_else(|| "—".into(), |q| format!("{q}"))).into()),
        kv(
            "毛 / 费 / 净",
            row![
                t::numeric(money(x.gross)).color(tone_of(x.gross)),
                t::numeric(format!(" / {} / ", x.fee.map_or_else(|| "—".into(), |f| format!("{f:.2}")))),
                t::numeric(money(net)).color(tone_of(net)),
            ]
            .into(),
        ),
    ]
    .spacing(space(3))
    .wrap();
    let r = v.review.get(&x.key()).cloned().unwrap_or_default();
    let is = |s: &str| r.s.as_deref() == Some(s);
    let review = row![
        w::btn(if is("ok") { "✓ 正确  1" } else { "正确  1" }, if is("ok") { Kind::Primary } else { Kind::Standard }, Some(TrMsg::Mark("ok"))),
        w::btn(if is("doubt") { "▲ 有疑问  2" } else { "有疑问  2" }, if is("doubt") { Kind::Destructive } else { Kind::Standard }, Some(TrMsg::Mark("doubt"))),
        w::btn("← 上一笔", Kind::Ghost, Some(TrMsg::Prev)),
        w::btn("下一笔 →", Kind::Ghost, Some(TrMsg::Next)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center)
    .wrap();
    let note = text_input("备注（哪里不对）", &r.note).on_input(TrMsg::Note).size(t::s_small()).width(Length::Fill);
    crate::ui::scroll(column![review, note, cells].spacing(space(2)).padding(space(2))).height(Length::Fill).into()
}

// ── 右栏：全景 · 秒级 · 当天 1 分钟 ───────────────────────────────────

fn right_column<'a>(v: &View) -> Element<'a, TrMsg> {
    let cur = v.cur.filter(|i| *i < v.trades.len()).map(|i| v.trades[i].clone());
    let chart = cur.as_ref().and_then(|x| v.charts.get(&x.day).cloned());
    let win = cur.as_ref().and_then(|x| chart.as_ref().and_then(|c| c.window(&x.key()).cloned()));
    let missing = |what: &str, err: Option<String>| -> Element<'a, TrMsg> {
        let msg = if let Some(e) = err {
            format!("{what}：{e}")
        } else if !v.chart_err.is_empty() {
            format!("{what}：{}", v.chart_err)
        } else {
            "生成 K 线数据中…（Tardis 原始逐笔成交 + 买一卖一）".to_string()
        };
        container(t::metadata(msg).color(pal::dim())).center(Length::Fill).into()
    };
    let all: Vec<(usize, &Trade)> = v.trades.iter().enumerate().collect();
    let run_key = v.sel.clone().unwrap_or_default();

    // 全景：整段运行的 K 线 + 全部订单；点标记选中，选中的跑出视野时视野跟过去
    let ov: Element<'a, TrMsg> = match &v.overview {
        Some(o) if !o.bars.t.is_empty() => {
            let g = if o.gran > 0.0 { o.gran } else { 60.0 };
            let full = (*o.bars.t.first().unwrap_or(&0) as f64, *o.bars.t.last().unwrap_or(&0) as f64 + g);
            let focus = cur.as_ref().map(|x| (x.key(), x.open_ns as f64 / 1e9, x.close_ns as f64 / 1e9));
            let c = Candles::new(Arc::new(o.bars.clone()), g, full, format!("ov|{run_key}"), None, marks(&all, cur.as_ref(), g), cur.clone(), None)
                .overview(focus);
            canvas::Canvas::new(c).width(Length::Fill).height(Length::Fill).into()
        }
        Some(o) => missing("全景 K 线", o.error.clone()),
        None => missing("全景 K 线", None),
    };
    let ov_note = v.overview.as_ref().map_or_else(String::new, |o| {
        let g = if o.gran >= 300.0 { "5 分钟" } else { "1 分钟" };
        let miss = if o.missing.is_empty() { String::new() } else { format!(" · 缺 {} 天", o.missing.len()) };
        format!("{} … {} · {g} K 线{miss}", o.start, o.end)
    });

    let Some(x) = cur else {
        let ov_card = column![chart_head("全景 · 整段运行的 K 线与全部订单", ov_note), ov].spacing(space(1));
        return crate::ui::split::column("trade_review.right", vec![(ov_card.into(), 1.0, 120.0)], TrMsg::Split);
    };
    let same_day: Vec<(usize, &Trade)> = all.iter().filter(|(_, t)| t.day == x.day).copied().collect();
    let err = chart.as_ref().and_then(|c| c.error.clone());
    let sec: Element<'a, TrMsg> = match (&chart, &win) {
        (Some(c), Some(wn)) if !wn.bars.t.is_empty() => {
            let o = x.open_ns as f64 / 1e9;
            let cl = x.close_ns as f64 / 1e9;
            let span = (cl - o).max(20.0);
            let init = (o - (span * 2.0).max(180.0), cl + span.max(90.0));
            canvas::Canvas::new(Candles::new(Arc::new(wn.bars.clone()), 1.0, init, x.key(), Some(Arc::new(wn.clone())), marks(&same_day, Some(&x), 1.0), Some(x.clone()), c.quotes.clone()))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        _ => missing("秒级 K 线", err.clone()),
    };
    let day: Element<'a, TrMsg> = match &chart {
        Some(c) if !c.minute.t.is_empty() => {
            let om = (x.open_ns / 1_000_000_000 / 60 * 60) as f64;
            canvas::Canvas::new(Candles::new(Arc::new(c.minute.clone()), 60.0, (om - 3.0 * 3600.0, om + 3.0 * 3600.0), format!("d{}", x.key()), None, marks(&same_day, Some(&x), 60.0), Some(x.clone()), None))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        _ => missing("当天 1 分钟 K 线", err),
    };
    let legend = row![
        legend_item("卖一", pal::down(), true),
        legend_item("买一", pal::up(), true),
        legend_item("▲ 开多", long_c(), false),
        legend_item("▼ 开空", short_c(), false),
        legend_item("● 平仓", exit_c(), false),
    ]
    .spacing(space(3));
    let src = chart.as_ref().and_then(|c| c.quotes.clone()).map_or_else(String::new, |q| format!("，买一卖一来自{q}"));
    let ov_card = column![chart_head("全景 · 整段运行的 K 线与全部订单（点标记定位）", ov_note), ov].spacing(space(1));
    let sec_card = column![
        row![t::label("秒级 K 线 · 开仓前 10 分钟到平仓后 5 分钟").color(pal::head()), Space::new().width(Length::Fill), legend].spacing(space(2)).align_y(Alignment::Center),
        sec,
    ]
    .spacing(space(1));
    let day_card = column![chart_head("当天 1 分钟 K 线 · 当天全部交易", format!("K 线来自 Tardis 原始逐笔成交{src} · 滚轮缩放 · 拖动平移 · 右键复位 · 点标记选中")), day].spacing(space(1));
    crate::ui::split::column("trade_review.right", vec![(ov_card.into(), 0.3, 100.0), (sec_card.into(), 0.42, 160.0), (day_card.into(), 0.28, 100.0)], TrMsg::Split)
}

fn chart_head<'a>(title: &str, note: String) -> Element<'a, TrMsg> {
    row![t::label(title.to_string()).color(pal::head()), Space::new().width(Length::Fill), t::metadata(note).color(pal::dim())].spacing(space(2)).align_y(Alignment::Center).into()
}

fn legend_item<'a>(s: &str, c: Color, line: bool) -> Element<'a, TrMsg> {
    let sw: Element<'a, TrMsg> = if line {
        container(Space::new())
            .width(Length::Fixed(12.0))
            .height(Length::Fixed(2.0))
            .style(move |_: &Theme| container::Style { background: Some(Background::Color(c)), ..Default::default() })
            .into()
    } else {
        Space::new().width(Length::Fixed(0.0)).into()
    };
    row![sw, t::metadata(s.to_string()).color(if line { pal::dim() } else { c })].spacing(space(1)).align_y(Alignment::Center).into()
}

fn long_c() -> Color {
    pal::info()
}
fn short_c() -> Color {
    pal::warn()
}
fn exit_c() -> Color {
    pal::accent()
}

// ── K 线画布 ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct Mark {
    /// 第几笔（trades 下标）：点了选它
    idx: usize,
    t: f64,
    px: f64,
    open: bool,
    long: bool,
    sel: bool,
    label: String,
    /// 悬停时的读数行
    info: String,
}

/// 一组交易 → 图上的标记（时间对齐到 K 线根的起点：秒级 1、分钟级 60）。
fn marks(trades: &[(usize, &Trade)], cur: Option<&Trade>, gran: f64) -> Vec<Mark> {
    let snap = |ns: i64| ((ns as f64 / 1e9) / gran).floor() * gran;
    let cur_key = cur.map(Trade::key);
    let mut out = Vec::new();
    for (idx, x) in trades {
        let sel = cur_key.as_deref() == Some(x.key().as_str());
        let info = format!(
            "#{} {} {} · {} · {:.0} 秒 · 净 {}",
            idx + 1,
            x.lane,
            if x.long() { "多" } else { "空" },
            x.reason_or_dash(),
            x.hold_s(),
            money(x.net.or(x.gross))
        );
        if let Some(e) = x.entry {
            out.push(Mark { idx: *idx, t: snap(x.open_ns), px: e, open: true, long: x.long(), sel, label: format!("{} {e:.1}", if x.long() { "开多" } else { "开空" }), info: info.clone() });
        }
        if let Some(e) = x.exit {
            out.push(Mark { idx: *idx, t: snap(x.close_ns), px: e, open: false, long: x.long(), sel, label: format!("平 {e:.1} {}", money(x.gross)), info });
        }
    }
    out
}

pub struct Candles {
    bars: Arc<tr::Bars>,
    /// 一根 K 线几秒
    gran: f64,
    init: (f64, f64),
    key: String,
    win: Option<Arc<tr::Window>>,
    marks: Vec<Mark>,
    cur: Option<Trade>,
    quotes_src: Option<String>,
    /// 全景图：(选中那笔的键, 开, 平)——选中的跑出视野时视野平移过去（宽度不变）
    focus: Option<(String, f64, f64)>,
    /// 全景图：标记小一点、只给选中那笔写字
    small: bool,
}

impl Candles {
    #[allow(clippy::too_many_arguments)]
    fn new(bars: Arc<tr::Bars>, gran: f64, init: (f64, f64), key: String, win: Option<Arc<tr::Window>>, marks: Vec<Mark>, cur: Option<Trade>, quotes_src: Option<String>) -> Self {
        Self { bars, gran, init, key, win, marks, cur, quotes_src, focus: None, small: false }
    }

    fn overview(mut self, focus: Option<(String, f64, f64)>) -> Self {
        self.focus = focus;
        self.small = true;
        self
    }

    fn full(&self) -> (f64, f64) {
        match (self.bars.t.first(), self.bars.t.last()) {
            (Some(a), Some(b)) => (*a as f64, *b as f64 + self.gran),
            _ => (0.0, 1.0),
        }
    }

    /// 实际画的横轴范围：状态里的（缩放 / 平移过）或初始的；全景图上新选中的笔不在视野里时平移过去。
    fn eff(&self, st: &CState) -> (f64, f64) {
        let (lo, hi) = if st.key == self.key && st.hi > st.lo { (st.lo, st.hi) } else { self.init };
        match &self.focus {
            Some((k, a, z)) if *k != st.focus && (*a < lo || *z > hi) => {
                let w = hi - lo;
                let m = (a + z) / 2.0;
                (m - w / 2.0, m + w / 2.0)
            }
            _ => (lo, hi),
        }
    }
}

#[derive(Default)]
pub struct CState {
    key: String,
    focus: String,
    lo: f64,
    hi: f64,
    /// 按下时的 (x, lo, hi)；松开时没怎么动 = 点击
    drag: Option<(f32, f64, f64)>,
}

const ML: f32 = 6.0;
const MR: f32 = 62.0;
const MT: f32 = 6.0;
const MB: f32 = 16.0;

fn nice_step(span: f64, target: f64) -> f64 {
    let raw = span / target.max(1.0);
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    p * if m < 1.5 { 1.0 } else if m < 3.5 { 2.0 } else if m < 7.5 { 5.0 } else { 10.0 }
}

/// 时间刻度：秒级图 10 秒 … 1 小时，分钟图 5 分钟 … 1 天。
fn time_step(span: f64, w: f32) -> f64 {
    let target = (w / 90.0).max(2.0) as f64;
    let raw = span / target;
    [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0, 172800.0, 432000.0]
        .into_iter()
        .find(|s| *s >= raw)
        .unwrap_or(432000.0)
}

fn mono(content: String, at: Point, c: Color, size: f32) -> Text {
    Text { content, position: at, color: c, size: iced::Pixels(size), font: crate::style::AZERET_MONO, ..Default::default() }
}

/// 一帧的几何：横轴范围、纵轴范围、绘图区。画、点选、悬停共用，保证点到的就是画出来的。
struct Geo {
    lo: f64,
    hi: f64,
    ylo: f64,
    yhi: f64,
    pw: f32,
    ph: f32,
    ph_all: f32,
    i0: usize,
    i1: usize,
}

impl Geo {
    fn sx(&self, t: f64) -> f32 {
        ML + ((t - self.lo) / (self.hi - self.lo).max(1e-9)) as f32 * self.pw
    }
    fn sy(&self, p: f64) -> f32 {
        MT + ((self.yhi - p) / (self.yhi - self.ylo).max(1e-9)) as f32 * self.ph
    }
}

impl Candles {
    fn geo(&self, st: &CState, b: Rectangle) -> Geo {
        let (lo, hi) = self.eff(st);
        let pw = (b.width - ML - MR).max(1.0);
        let ph_all = (b.height - MT - MB).max(1.0);
        let ph = ph_all - ph_all * 0.16 - 4.0;
        let bars = &self.bars;
        let i0 = bars.t.partition_point(|x| (*x as f64) + self.gran < lo);
        let i1 = bars.t.partition_point(|x| (*x as f64) <= hi);
        // 纵轴范围：可见 K 线 + 可见买一卖一 + 可见标记
        let (mut ylo, mut yhi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in i0..i1 {
            ylo = ylo.min(bars.l[i]);
            yhi = yhi.max(bars.h[i]);
        }
        if let Some(wn) = &self.win {
            let q0 = wn.qt.partition_point(|x| (*x as f64) < lo);
            let q1 = wn.qt.partition_point(|x| (*x as f64) <= hi);
            for k in q0..q1 {
                if wn.qb[k].is_finite() && wn.qa[k].is_finite() {
                    ylo = ylo.min(wn.qb[k]);
                    yhi = yhi.max(wn.qa[k]);
                }
            }
        }
        for m in &self.marks {
            if m.t >= lo && m.t <= hi {
                ylo = ylo.min(m.px);
                yhi = yhi.max(m.px);
            }
        }
        if !ylo.is_finite() {
            (ylo, yhi) = (0.0, 1.0);
        }
        let pad = ((yhi - ylo) * 0.06).max(0.5);
        Geo { lo, hi, ylo: ylo - pad, yhi: yhi + pad, pw, ph, ph_all, i0, i1 }
    }

    /// 光标下的标记（9 像素以内最近的那个）。
    fn mark_at(&self, g: &Geo, p: Point) -> Option<&Mark> {
        self.marks
            .iter()
            .filter(|m| m.t >= g.lo - self.gran && m.t <= g.hi)
            .map(|m| (m, (g.sx(m.t + self.gran / 2.0) - p.x).hypot(g.sy(m.px) - p.y)))
            .filter(|(_, d)| *d <= 9.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(m, _)| m)
    }
}

impl canvas::Program<TrMsg> for Candles {
    type State = CState;

    fn update(&self, st: &mut CState, event: &canvas::Event, b: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<TrMsg>> {
        // 先把「换了一笔 / 换了运行 / 全景跟随」落进状态，之后的缩放平移都从实际画的范围出发
        let (lo, hi) = self.eff(st);
        if st.key != self.key {
            st.drag = None;
        }
        st.key = self.key.clone();
        st.focus = self.focus.as_ref().map(|f| f.0.clone()).unwrap_or_default();
        (st.lo, st.hi) = (lo, hi);
        let pw = (b.width - ML - MR).max(1.0);
        match event {
            canvas::Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => {
                cursor.position_in(b)?;
                use keyboard::key::Named;
                let m = match key.as_ref() {
                    keyboard::Key::Named(Named::ArrowLeft | Named::ArrowUp) => TrMsg::Prev,
                    keyboard::Key::Named(Named::ArrowRight | Named::ArrowDown) => TrMsg::Next,
                    keyboard::Key::Character("1") => TrMsg::Mark("ok"),
                    keyboard::Key::Character("2") => TrMsg::Mark("doubt"),
                    _ => return None,
                };
                Some(canvas::Action::publish(m).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = cursor.position_in(b)?;
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let at = lo + f64::from(((p.x - ML) / pw).clamp(0.0, 1.0)) * (hi - lo);
                let k = if y > 0.0 { 0.8 } else { 1.25 };
                let (a, z) = (at - (at - lo) * k, at + (hi - at) * k);
                if z - a < self.gran * 8.0 {
                    return Some(canvas::Action::capture());
                }
                let (fa, fz) = self.full();
                st.lo = a.max(fa - (z - a) * 0.5);
                st.hi = z.min(fz + (z - a) * 0.5);
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                cursor.position_in(b)?;
                (st.lo, st.hi) = self.init;
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(b)?;
                st.drag = Some((p.x, lo, hi));
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let (x0, _, _) = st.drag.take()?;
                let p = cursor.position_in(b)?;
                if (p.x - x0).abs() > 4.0 {
                    return Some(canvas::Action::capture());
                }
                // 点击：点到标记就选那一笔
                let g = self.geo(st, b);
                let m = self.mark_at(&g, p)?;
                Some(canvas::Action::publish(TrMsg::Select(m.idx)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some((x0, a, z)), Some(p)) = (st.drag, cursor.position()) {
                    let dx = f64::from((x0 - (p.x - b.x)) / pw) * (z - a);
                    st.lo = a + dx;
                    st.hi = z + dx;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                cursor.position_in(b).map(|_| canvas::Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(&self, st: &CState, r: &Renderer, _th: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut f = Frame::new(r, b.size());
        let g = self.geo(st, b);
        let (lo, hi, pw, ph, ph_all) = (g.lo, g.hi, g.pw, g.ph, g.ph_all);
        let (i0, i1) = (g.i0, g.i1);
        let (w, h) = (b.width, b.height);
        let vol_h = ph_all * 0.16;
        let vtop = MT + ph + 4.0;
        let bars = &self.bars;
        f.fill_rectangle(Point::new(0.0, 0.0), Size::new(w, h), pal::card_bg());
        if i1 <= i0 {
            f.fill_text(mono("这段时间没有成交".into(), Point::new(ML + 8.0, MT + ph / 2.0), pal::dim(), 11.0));
            return vec![f.into_geometry()];
        }
        // 格栅：横纵同一种颜色，只比底色略深 / 略浅一点（不显眼）；线落在整像素上，不被抗锯齿糊成两像素宽
        let grid = Stroke::default().with_width(1.0).with_color(pal::mix(pal::card_bg(), pal::grid(), 0.1));
        let crisp = |v: f32| v.floor() + 0.5;
        let ystep = nice_step(g.yhi - g.ylo, (ph / 40.0) as f64);
        let mut yv = (g.ylo / ystep).ceil() * ystep;
        while yv <= g.yhi {
            let y = g.sy(yv);
            f.stroke(&Path::line(Point::new(ML, crisp(y)), Point::new(ML + pw, crisp(y))), grid);
            f.fill_text(mono(format!("{yv:.1}"), Point::new(ML + pw + 4.0, y - 6.0), pal::axis(), 10.0));
            yv += ystep;
        }
        let ts = time_step(hi - lo, pw);
        let mut tv = (lo / ts).ceil() * ts;
        let fmt = if ts < 60.0 { "%H:%M:%S" } else if ts < 86400.0 { "%H:%M" } else { "%m-%d" };
        while tv <= hi {
            let x = g.sx(tv);
            f.stroke(&Path::line(Point::new(crisp(x), MT), Point::new(crisp(x), MT + ph_all)), grid);
            let s = fmt_utc((tv * 1e9) as i64, if (tv % 86400.0).abs() < 1e-6 && ts >= 60.0 { "%m-%d" } else { fmt });
            f.fill_text(mono(s, Point::new(x - 22.0, MT + ph_all + 3.0), pal::axis(), 10.0));
            tv += ts;
        }

        // 持仓区间底色（选中那笔）
        if let Some(c) = &self.cur {
            let a = g.sx((c.open_ns as f64 / 1e9 / self.gran).floor() * self.gran);
            let z = g.sx(((c.close_ns as f64 / 1e9 / self.gran).floor() + 1.0) * self.gran);
            if z > ML && a < ML + pw {
                let (a, z) = (a.max(ML), z.min(ML + pw));
                f.fill_rectangle(Point::new(a, MT), Size::new((z - a).max(2.0), ph_all), pal::alpha(pal::accent(), if self.small { 0.25 } else { 0.07 }));
            }
        }

        // K 线 + 成交量。可见根数比像素还多时，按像素合并成粗一点的根（全景图 30 天 4 万根）
        let n = i1 - i0;
        let per = ((n as f32) / (pw / 3.0)).ceil().max(1.0) as usize;
        let slot = pw * (self.gran * per as f64 / (hi - lo).max(1e-9)) as f32;
        let body_w = (slot * 0.7).clamp(1.0, 12.0);
        let mut vols: Vec<(f32, f64, Color)> = Vec::with_capacity(n / per + 1);
        let mut i = i0;
        while i < i1 {
            let j = (i + per).min(i1);
            let o = bars.o[i];
            let c = bars.c[j - 1];
            let (mut hh, mut ll, mut v, mut bv) = (f64::NEG_INFINITY, f64::INFINITY, 0.0, 0.0);
            for k in i..j {
                hh = hh.max(bars.h[k]);
                ll = ll.min(bars.l[k]);
                v += bars.v[k];
                bv += bars.bv.get(k).copied().unwrap_or(0.0);
            }
            let t0 = bars.t[i] as f64;
            let x = g.sx(t0 + self.gran * per as f64 / 2.0);
            i = j;
            if x < ML - slot || x > ML + pw + slot {
                continue;
            }
            let col = if c >= o { pal::up() } else { pal::down() };
            f.stroke(&Path::line(Point::new(x, g.sy(hh)), Point::new(x, g.sy(ll))), Stroke::default().with_width(1.0).with_color(col));
            if body_w >= 2.0 {
                let (yo, yc) = (g.sy(o), g.sy(c));
                f.fill_rectangle(Point::new(x - body_w / 2.0, yo.min(yc)), Size::new(body_w, (yo - yc).abs().max(1.0)), col);
            }
            let vc = if self.gran <= 1.0 { if bv >= v - bv { pal::up() } else { pal::down() } } else { col };
            vols.push((x, v, vc));
        }
        let vmax = vols.iter().map(|x| x.1).fold(0.0, f64::max).max(1e-12);
        for (x, v, c) in vols {
            let vh = (v / vmax) as f32 * vol_h;
            f.fill_rectangle(Point::new(x - body_w / 2.0, vtop + vol_h - vh), Size::new(body_w.max(1.0), vh.max(0.5)), pal::alpha(c, 0.45));
        }

        // 买一 / 卖一阶梯线
        if let Some(wn) = &self.win {
            let dash = Stroke { line_dash: LineDash { segments: &[3.0, 2.0], offset: 0 }, ..Stroke::default().with_width(1.0) };
            for (vals, col) in [(&wn.qa, pal::down()), (&wn.qb, pal::up())] {
                let mut pb = canvas::path::Builder::new();
                let mut started = false;
                let mut last_y = 0.0;
                for (qt, val) in wn.qt.iter().zip(vals.iter()) {
                    let tt = *qt as f64;
                    if tt + 1.0 < lo || tt > hi || !val.is_finite() {
                        continue;
                    }
                    let (x, y) = (g.sx(tt).clamp(ML, ML + pw), g.sy(*val));
                    if !started {
                        pb.move_to(Point::new(x, y));
                        started = true;
                    } else {
                        pb.line_to(Point::new(x, last_y));
                        pb.line_to(Point::new(x, y));
                    }
                    last_y = y;
                }
                if started {
                    f.stroke(&pb.build(), Stroke { style: canvas::Style::Solid(pal::alpha(col, 0.9)), ..dash });
                }
            }
        }

        // 选中那笔：开平价横线 + 连线（全景图只画连线）
        if let Some(c) = &self.cur {
            let pnl_c = if c.gross.unwrap_or(0.0) >= 0.0 { pal::up() } else { pal::down() };
            if !self.small {
                // 右轴上的价签离得太近时平仓那个让开（线仍画在原价位）
                let mut tags: Vec<f32> = Vec::new();
                let mut hl = |p: f64, col: Color| {
                    let y = g.sy(p);
                    f.stroke(&Path::line(Point::new(ML, y), Point::new(ML + pw, y)), Stroke { line_dash: LineDash { segments: &[6.0, 4.0], offset: 0 }, ..Stroke::default().with_width(1.0).with_color(pal::alpha(col, 0.8)) });
                    let mut ty = y;
                    if let Some(prev) = tags.last()
                        && (ty - prev).abs() < 15.0
                    {
                        ty = if ty >= *prev { prev + 15.0 } else { prev - 15.0 };
                    }
                    tags.push(ty);
                    f.fill_rectangle(Point::new(ML + pw + 1.0, ty - 7.0), Size::new(MR - 2.0, 14.0), col);
                    f.fill_text(mono(format!("{p:.1}"), Point::new(ML + pw + 3.0, ty - 6.0), pal::on(col), 10.0));
                };
                if let Some(e) = c.entry {
                    hl(e, if c.long() { long_c() } else { short_c() });
                }
                if let Some(e) = c.exit {
                    hl(e, exit_c());
                }
            }
            if let (Some(e), Some(x)) = (c.entry, c.exit) {
                let snap = |ns: i64| ((ns as f64 / 1e9) / self.gran).floor() * self.gran + self.gran / 2.0;
                f.stroke(&Path::line(Point::new(g.sx(snap(c.open_ns)), g.sy(e)), Point::new(g.sx(snap(c.close_ns)), g.sy(x))), Stroke::default().with_width(2.0).with_color(pnl_c));
            }
        }

        // 标记：画在成交价上（选中的最后画，压在上面）
        let mut ms: Vec<&Mark> = self.marks.iter().filter(|m| m.t >= lo - self.gran && m.t <= hi).collect();
        ms.sort_by_key(|m| m.sel);
        for m in ms {
            let x = g.sx(m.t + self.gran / 2.0);
            let y = g.sy(m.px);
            let s = match (m.sel, self.small) {
                (true, _) => 7.0,
                (false, true) => 3.5,
                (false, false) => 5.0,
            };
            let col = if !m.open { exit_c() } else if m.long { long_c() } else { short_c() };
            let col = if m.sel { col } else { pal::alpha(col, 0.7) };
            if m.open {
                // 开多：尖朝上、顶点在成交价；开空：尖朝下
                let dir = if m.long { 1.0 } else { -1.0 };
                let tri = Path::new(|p| {
                    p.move_to(Point::new(x, y));
                    p.line_to(Point::new(x - s, y + dir * s * 1.6));
                    p.line_to(Point::new(x + s, y + dir * s * 1.6));
                    p.close();
                });
                f.fill(&tri, col);
            } else {
                f.fill(&Path::circle(Point::new(x, y), s * 0.75), col);
                f.stroke(&Path::circle(Point::new(x, y), s * 0.75), Stroke::default().with_width(1.0).with_color(pal::card_bg()));
            }
            if m.sel && (!self.small || m.open) {
                let dy = if m.open == m.long { 12.0 } else { -22.0 };
                let label = if self.small { m.info.clone() } else { m.label.clone() };
                f.fill_text(mono(label, Point::new(x + 6.0, y + dy), col, 10.0));
            }
        }

        // 坐标轴分隔线
        f.stroke(&Path::line(Point::new(ML + pw, MT), Point::new(ML + pw, MT + ph_all)), Stroke::default().with_width(1.0).with_color(pal::line()));
        f.stroke(&Path::line(Point::new(ML, vtop - 2.0), Point::new(ML + pw, vtop - 2.0)), grid);
        if let Some(q) = &self.quotes_src {
            f.fill_text(mono(format!("买一卖一：{q}"), Point::new(ML + 4.0, MT + 2.0), pal::dim(), 10.0));
        }
        let mut out = vec![f.into_geometry()];

        // 十字线读数（光标在标记上时先列这一笔）
        if let Some(p) = cursor.position_in(b)
            && p.x >= ML
            && p.x <= ML + pw
            && p.y >= MT
            && p.y <= MT + ph_all
        {
            let mut hf = Frame::new(r, b.size());
            let line = Stroke::default().with_width(1.0).with_color(pal::alpha(pal::axis(), 0.8));
            hf.stroke(&Path::line(Point::new(p.x, MT), Point::new(p.x, MT + ph_all)), line);
            if p.y <= MT + ph {
                hf.stroke(&Path::line(Point::new(ML, p.y), Point::new(ML + pw, p.y)), line);
                let pv = g.yhi - f64::from((p.y - MT) / ph) * (g.yhi - g.ylo);
                hf.fill_rectangle(Point::new(ML + pw + 1.0, p.y - 7.0), Size::new(MR - 2.0, 14.0), pal::axis());
                hf.fill_text(mono(format!("{pv:.1}"), Point::new(ML + pw + 3.0, p.y - 6.0), pal::card_bg(), 10.0));
            }
            let tt = lo + f64::from((p.x - ML) / pw) * (hi - lo);
            let mut lines = Vec::new();
            if let Some(m) = self.mark_at(&g, p) {
                lines.push(m.info.clone());
                lines.push(format!("{}（点一下选中）", m.label));
            }
            lines.push(fmt_utc((tt * 1e9) as i64, if self.gran < 60.0 { "%m-%d %H:%M:%S" } else { "%m-%d %H:%M" }));
            let k = bars.t.partition_point(|x| (*x as f64) <= tt).saturating_sub(1);
            if k < bars.t.len() && (tt - bars.t[k] as f64) < self.gran {
                lines.push(format!("开 {:.1} 高 {:.1} 低 {:.1} 收 {:.1}", bars.o[k], bars.h[k], bars.l[k], bars.c[k]));
                let bv = bars.bv.get(k).copied().unwrap_or(0.0);
                lines.push(format!("量 {:.3}（主动买 {:.3}）", bars.v[k], bv));
            }
            if let Some(wn) = &self.win {
                let q = wn.qt.partition_point(|x| (*x as f64) <= tt);
                if q > 0 {
                    lines.push(format!("买一 {:.1} / 卖一 {:.1}", wn.qb[q - 1], wn.qa[q - 1]));
                }
            }
            let wmax = lines.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f32 * 6.2 + 10.0;
            let bx = if p.x + 14.0 + wmax > ML + pw { p.x - 14.0 - wmax } else { p.x + 14.0 };
            let by = (p.y + 14.0).min(MT + ph_all - 14.0 * lines.len() as f32 - 4.0).max(MT);
            hf.fill_rectangle(Point::new(bx, by), Size::new(wmax, 14.0 * lines.len() as f32 + 4.0), pal::alpha(pal::card_bg(), 0.94));
            for (j, s) in lines.into_iter().enumerate() {
                hf.fill_text(mono(s, Point::new(bx + 5.0, by + 2.0 + 14.0 * j as f32), pal::txt(), 10.0));
            }
            out.push(hf.into_geometry());
        }
        out
    }

    fn mouse_interaction(&self, st: &CState, b: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if st.drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let Some(p) = cursor.position_in(b) else { return mouse::Interaction::default() };
        if self.mark_at(&self.geo(st, b), p).is_some() { mouse::Interaction::Pointer } else { mouse::Interaction::Crosshair }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 刻度步长() {
        assert_eq!(nice_step(100.0, 5.0), 20.0);
        assert_eq!(nice_step(7.0, 5.0), 1.0);
        assert_eq!(time_step(900.0, 900.0), 120.0);
        assert_eq!(time_step(6.0 * 3600.0, 900.0), 3600.0);
    }

    #[test]
    fn 标记对齐到k线根() {
        let t = Trade { lane: "A".into(), dir: "long".into(), open_ns: 125_500_000_000, close_ns: 190_000_000_000, entry: Some(1.0), exit: Some(2.0), gross: Some(1.0), ..Default::default() };
        let m = marks(&[(4, &t)], Some(&t), 60.0);
        assert_eq!((m[0].t, m[1].t), (120.0, 180.0));
        assert!(m[0].open && !m[1].open && m.iter().all(|x| x.sel && x.idx == 4));
        assert!(m[0].info.starts_with("#5 A 多"));
    }

    fn chart(focus: Option<(String, f64, f64)>) -> Candles {
        let bars = tr::Bars { t: (0..100).map(|i| i * 60).collect(), o: vec![1.0; 100], h: vec![2.0; 100], l: vec![0.5; 100], c: vec![1.5; 100], v: vec![1.0; 100], bv: vec![0.5; 100] };
        let t = Trade { lane: "A".into(), dir: "short".into(), open_ns: 3000 * 1_000_000_000, close_ns: 3060 * 1_000_000_000, entry: Some(1.2), exit: Some(1.1), ..Default::default() };
        Candles::new(Arc::new(bars), 60.0, (0.0, 6000.0), "ov|r".into(), None, marks(&[(0, &t)], Some(&t), 60.0), Some(t), None).overview(focus)
    }

    #[test]
    fn 全景跟随选中的笔() {
        let c = chart(Some(("k1".into(), 5000.0, 5060.0)));
        // 缩放到前一段，选中的笔在视野外：平移过去、宽度不变
        let st = CState { key: "ov|r".into(), focus: "k0".into(), lo: 0.0, hi: 1000.0, drag: None };
        let (lo, hi) = c.eff(&st);
        assert!((hi - lo - 1000.0).abs() < 1e-9 && lo < 5000.0 && hi > 5060.0);
        // 同一笔已经跟过（状态里记着）：不再强拉回去，用户可以自由平移
        let st = CState { focus: "k1".into(), ..st };
        assert_eq!(c.eff(&st), (0.0, 1000.0));
    }

    #[test]
    fn 点到标记() {
        let c = chart(None);
        let st = CState::default();
        let b = Rectangle::new(Point::ORIGIN, Size::new(668.0, 300.0));
        let g = c.geo(&st, b);
        let m = &c.marks[0];
        let p = Point::new(g.sx(m.t + 30.0) + 3.0, g.sy(m.px) - 2.0);
        assert_eq!(c.mark_at(&g, p).map(|m| m.idx), Some(0));
        assert!(c.mark_at(&g, Point::new(p.x + 40.0, p.y)).is_none());
    }
}
