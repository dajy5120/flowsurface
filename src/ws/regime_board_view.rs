//! 「市场状态」面板（docs/62 F5，§3.2）：状态矩阵、历史时间轴、描述统计、转移矩阵、当前可运行的策略、检验台账。
//!
//! 数据全部来自 K 线叠加层已经读进来的时间线文件（[`super::regime`]）；统计在 [`super::regime_board`]。
//! 时间轴与同页图表双向联动：光标放在 K 线上，矩阵显示那一刻的状态、时间轴画竖线；光标放在时间轴上，K 线图画竖线。
//! **面板只做描述**，状态有没有用只看检验台账（理由见 `regime_board` 模块说明）。

use std::cell::Cell;
use std::sync::Arc;

use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{Canvas, column, container, row, scrollable, text};
use iced::{Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::regime::{self as rg, RegimeFile, RegimeMsg};
use super::regime_board as rb;
use crate::ui::{metrics, pal, text as t, widgets as w};

const DAY: u64 = 86_400_000;
const STRIP: f32 = 12.0;
const GAP: f32 = 2.0;
const LABEL_W: f32 = 92.0;

fn fmt_t(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn fmt_dur(ms: u64) -> String {
    let h = ms as f64 / 3_600_000.0;
    if h < 48.0 { format!("{h:.1} 小时") } else { format!("{:.1} 天", h / 24.0) }
}

/// 面板看的时刻：同页联动的光标落在这份时间线范围里就用它，否则用最新。
fn focus_time(f: &RegimeFile) -> (u64, bool) {
    let (lo, hi) = f.span().unwrap_or((0, 1));
    match crate::chart::linked_cursor::get() {
        Some((_, t)) if t >= lo && t < hi => (t, true),
        _ => (hi.saturating_sub(1), false),
    }
}

pub fn pane_body<'a>() -> Element<'a, RegimeMsg> {
    let files = rg::all_files();
    let Some(f) = rg::board_file() else {
        return w::empty("还没有市场状态时间线", "在终端运行：.venv/bin/python -m factory.regime run BTCUSDT --tf 15m（其他来源见 python -m factory.regime sources）");
    };
    let s = rg::settings();
    let (lo_all, hi) = f.span().unwrap_or((0, 1));
    let lo = if s.board_days == 0 { lo_all } else { hi.saturating_sub(u64::from(s.board_days) * DAY).max(lo_all) };
    let (t_focus, linked) = focus_time(&f);

    let mut pick = row![].spacing(metrics::space(1));
    for x in &files {
        let k = x.key();
        pick = pick.push(super::regime_view::chip(k.clone(), k == f.key(), RegimeMsg::Board(k)));
    }
    let mut days = row![].spacing(metrics::space(1));
    for (d, lab) in [(30u32, "30 天"), (90, "90 天"), (365, "1 年"), (0, "全部")] {
        days = days.push(super::regime_view::chip(lab, s.board_days == d, RegimeMsg::BoardDays(d)));
    }
    let head = column![
        pick.wrap(),
        row![t::metadata("范围").color(pal::dim()), days].spacing(metrics::space(2)).align_y(iced::Alignment::Center),
        text(format!(
            "{} · {} · 看的时刻 {}{}",
            f.key(),
            f.method,
            fmt_t(t_focus),
            if linked { "（跟随同页图表的光标）" } else { "（最新；把光标放到同页 K 线或下面的时间轴上看历史）" }
        ))
        .size(t::s_meta())
        .color(pal::dim()),
    ]
    .spacing(metrics::space(1));

    // 实时（docs/62 F6）：按文件自己的时间判断，不在渲染线程里查守护
    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let close = f.last_ms.unwrap_or(hi);
    let age_close = now.saturating_sub(close);
    let gen_age = now.saturating_sub(f.generated_ms.max(0) as u64);
    let live = gen_age <= 2 * f.tf_ms.max(60_000) + 120_000;
    let (busy, last) = rg::once_status();
    let live_row = row![
        text(format!(
            "{} 最后一根 {} 收盘（{} 前）；文件 {} 前生成",
            if live { "● 实时" } else { "○ 不是实时" },
            fmt_t(close),
            fmt_dur(age_close),
            fmt_dur(gen_age)
        ))
        .size(t::s_meta())
        .color(if live { pal::ok() } else { pal::dim() }),
        w::btn_busy("刷新一次", w::Kind::Ghost, (!busy).then_some(RegimeMsg::RefreshOnce), busy),
        text(last).size(t::s_meta()).color(pal::dim()),
    ]
    .spacing(metrics::space(2))
    .align_y(iced::Alignment::Center);
    let body = column![
        head,
        live_row,
        t::metadata("常驻发布：「网络出口」页启停 ws-regime（每根 K 线收盘后重算、写这里的文件、发 Redis ws:regime:*）；「刷新一次」只跑一轮 factory/regime/live.toml 里的品种").color(pal::dim()),
        w::section("状态矩阵"),
        matrix(&f, t_focus),
        w::section("历史时间轴"),
        timeline(f.clone(), lo, hi, linked.then_some(t_focus)),
        w::section("描述统计（范围内）"),
        stats(&f, lo, hi),
        w::section("转移矩阵"),
        transitions(&f, &s, lo, hi),
        w::section("当前可运行的策略"),
        runnable(&f, t_focus),
        w::section("检验台账"),
        ledger(),
    ]
    .spacing(metrics::space(2))
    .padding(metrics::pad2(2, 3));
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

// ── 状态矩阵 ────────────────────────────────────────────────────────────────

fn matrix<'a>(f: &RegimeFile, t: u64) -> Element<'a, RegimeMsg> {
    let mut col = column![].spacing(2);
    let mut last_layer = String::new();
    for d in &f.dims {
        if d.layer != last_layer {
            last_layer = d.layer.clone();
            let name = f.layer_names.get(&d.layer).cloned().unwrap_or_else(|| d.layer.clone());
            col = col.push(text(name).size(t::s_small()).color(pal::dim()));
        }
        let (state, color, since, why) = match f.at(&d.key, t) {
            Some(seg) => {
                let st = d.states.get(seg.s);
                (
                    st.map(|x| x.name.clone()).unwrap_or_default(),
                    st.and_then(|x| rg::tone_color(&x.tone)),
                    format!("已持续 {}", fmt_dur(t.saturating_sub(seg.t0))),
                    seg.why.clone(),
                )
            }
            None => ("—".into(), None, String::new(), String::new()),
        };
        col = col.push(
            row![
                text(d.name.clone()).size(t::s_small()).width(Length::Fixed(LABEL_W)),
                text(state).size(t::s_small()).color(color.unwrap_or(pal::txt())).width(Length::Fixed(110.0)),
                text(since).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(110.0)),
                text(why).size(t::s_meta()).color(pal::dim()),
            ]
            .spacing(metrics::space(2)),
        );
    }
    col.into()
}

// ── 历史时间轴 ──────────────────────────────────────────────────────────────

struct Timeline {
    f: Arc<RegimeFile>,
    lo: u64,
    hi: u64,
    cursor: Option<u64>,
}

#[derive(Default)]
pub struct TlState {
    cache: Cache,
    key: Cell<(usize, u64, u64, u64)>,
}

impl Timeline {
    fn t_at(&self, x: f32, width: f32) -> u64 {
        let pw = (width - LABEL_W).max(1.0);
        let r = ((x - LABEL_W) / pw).clamp(0.0, 1.0);
        self.lo + (f64::from(r) * (self.hi - self.lo) as f64) as u64
    }
}

impl canvas::Program<RegimeMsg> for Timeline {
    type State = TlState;

    fn update(&self, _st: &mut TlState, event: &canvas::Event, b: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<RegimeMsg>> {
        match event {
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => match cursor.position_in(b) {
                Some(p) if p.x >= LABEL_W => Some(canvas::Action::publish(RegimeMsg::Hover(Some(self.t_at(p.x, b.width))))),
                _ => Some(canvas::Action::publish(RegimeMsg::Hover(None))),
            },
            canvas::Event::Mouse(mouse::Event::CursorLeft) => Some(canvas::Action::publish(RegimeMsg::Hover(None))),
            _ => None,
        }
    }

    fn draw(&self, st: &TlState, r: &Renderer, _th: &Theme, b: Rectangle, _c: mouse::Cursor) -> Vec<Geometry> {
        // 底图按（文件、范围、宽度）缓存：一份 15 分钟线一年两万段，不能每帧重画
        let key = (Arc::as_ptr(&self.f) as usize, self.lo, self.hi, b.width.to_bits() as u64);
        if st.key.get() != key {
            st.cache.clear();
            st.key.set(key);
        }
        let base = st.cache.draw(r, b.size(), |frame: &mut Frame| {
            let pw = (frame.width() - LABEL_W).max(1.0);
            let span = (self.hi - self.lo).max(1) as f32;
            let x_of = |t: u64| LABEL_W + (t.saturating_sub(self.lo) as f32 / span).min(1.0) * pw;
            for (i, d) in self.f.dims.iter().enumerate() {
                let y = i as f32 * (STRIP + GAP);
                frame.fill_text(Text {
                    content: d.name.clone(),
                    position: Point::new(0.0, y - 1.0),
                    size: iced::Pixels(t::s_meta()),
                    color: pal::dim(),
                    ..Text::default()
                });
                // 同一像素里多段时只画最后一段（缩到一年时 15 分钟线一像素几十段，画全了也看不见）
                let mut last_px = -1.0_f32;
                for seg in self.f.overlapping(&d.key, self.lo, self.hi) {
                    let Some(c) = d.states.get(seg.s).and_then(|s| rg::tone_color(&s.tone)) else { continue };
                    let x0 = x_of(seg.t0.max(self.lo));
                    let x1 = x_of(seg.t1.unwrap_or(self.hi).min(self.hi)).max(x0 + 0.5);
                    if x1.floor() <= last_px && x1 - x0 < 1.0 {
                        continue;
                    }
                    last_px = x1.floor();
                    frame.fill_rectangle(Point::new(x0, y), Size::new(x1 - x0, STRIP), pal::alpha(c, 0.85));
                }
            }
            let y_axis = self.f.dims.len() as f32 * (STRIP + GAP) + 2.0;
            for (k, t) in [self.lo, self.lo + (self.hi - self.lo) / 2, self.hi].into_iter().enumerate() {
                let x = x_of(t) - if k == 2 { 96.0 } else if k == 1 { 48.0 } else { 0.0 };
                frame.fill_text(Text {
                    content: fmt_t(t),
                    position: Point::new(x.max(LABEL_W), y_axis),
                    size: iced::Pixels(t::s_meta()),
                    color: pal::dim(),
                    ..Text::default()
                });
            }
        });
        let mut top = Frame::new(r, b.size());
        if let Some(t) = self.cursor
            && t >= self.lo
            && t < self.hi
        {
            let pw = (b.width - LABEL_W).max(1.0);
            let x = LABEL_W + ((t - self.lo) as f32 / (self.hi - self.lo).max(1) as f32) * pw;
            let h = self.f.dims.len() as f32 * (STRIP + GAP);
            top.stroke(&Path::line(Point::new(x, 0.0), Point::new(x, h)), Stroke::default().with_width(1.0).with_color(pal::txt()));
        }
        vec![base, top.into_geometry()]
    }
}

fn timeline<'a>(f: Arc<RegimeFile>, lo: u64, hi: u64, cursor: Option<u64>) -> Element<'a, RegimeMsg> {
    let h = f.dims.len() as f32 * (STRIP + GAP) + 18.0;
    Canvas::new(Timeline { f, lo, hi, cursor }).width(Length::Fill).height(Length::Fixed(h)).into()
}

// ── 统计 ────────────────────────────────────────────────────────────────────

fn stats<'a>(f: &RegimeFile, lo: u64, hi: u64) -> Element<'a, RegimeMsg> {
    let mut col = column![].spacing(2);
    for (d, st) in f.dims.iter().zip(rb::dim_stats(f, lo, hi)) {
        let parts: Vec<String> = st
            .states
            .iter()
            .take(6)
            .map(|(s, share, _, avg_h)| {
                let name = d.states.get(*s).map(|x| x.name.as_str()).unwrap_or("?");
                format!("{name} {:.0}%（均 {})", share * 100.0, fmt_dur((avg_h * 3_600_000.0) as u64))
            })
            .collect();
        col = col.push(
            row![
                text(d.name.clone()).size(t::s_small()).width(Length::Fixed(LABEL_W)),
                text(format!("每天切换 {:.1} 次", st.switches_per_day)).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(110.0)),
                text(parts.join("、")).size(t::s_meta()),
            ]
            .spacing(metrics::space(2)),
        );
    }
    col.into()
}

fn transitions<'a>(f: &RegimeFile, s: &rg::Settings, lo: u64, hi: u64) -> Element<'a, RegimeMsg> {
    let key = s.board_dim.clone().filter(|k| f.dim(k).is_some()).unwrap_or_else(|| s.band.clone());
    let mut chips = row![].spacing(metrics::space(1));
    for d in &f.dims {
        chips = chips.push(super::regime_view::chip(d.name.clone(), d.key == key, RegimeMsg::BoardDim(d.key.clone())));
    }
    let Some(d) = f.dim(&key) else { return chips.wrap().into() };
    let (idx, p, rows) = rb::transitions(f, &key, lo, hi);
    let name = |i: usize| d.states.get(i).map(|x| x.name.clone()).unwrap_or_default();
    let cw = 74.0;
    let mut hdr = row![text("从 \\ 到").size(t::s_meta()).color(pal::dim()).width(Length::Fixed(LABEL_W + 40.0))];
    for &i in &idx {
        hdr = hdr.push(text(name(i)).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(cw)));
    }
    let mut tbl = column![hdr].spacing(2);
    for (r, &i) in idx.iter().enumerate() {
        let mut line = row![text(format!("{}（{} 次）", name(i), rows[r])).size(t::s_small()).width(Length::Fixed(LABEL_W + 40.0))];
        for v in &p[r] {
            let c = if *v > 0.0 { pal::alpha(pal::txt(), 0.35 + 0.65 * (*v as f32)) } else { pal::dim() };
            line = line.push(text(if *v > 0.0 { format!("{:.0}%", v * 100.0) } else { "·".into() }).size(t::s_small()).color(c).width(Length::Fixed(cw)));
        }
        tbl = tbl.push(line);
    }
    // 状态多（结构有 9 种）时表比面板宽：横向滚动，不截掉最后几列
    let tbl = crate::ui::scroll(tbl).direction(iced::widget::scrollable::Direction::Horizontal(
        iced::widget::scrollable::Scrollbar::new().spacing(metrics::SCROLL_GAP),
    ));
    column![chips.wrap(), t::metadata("行 = 从哪个状态切出，格 = 切到各状态的占比（按次数）").color(pal::dim()), tbl]
        .spacing(metrics::space(1))
        .into()
}

// ── 当前可运行的策略 ───────────────────────────────────────────────────────

fn runnable<'a>(f: &RegimeFile, t: u64) -> Element<'a, RegimeMsg> {
    super::strategy_center::ensure_catalog();
    let snap = f.snapshot(t);
    let cat = super::strategy_center::catalog_regimes();
    if cat.is_empty() {
        return t::metadata("策略目录还没读出来（后台在读；也可以打开策略中心）").color(pal::dim()).into();
    }
    let declared: Vec<_> = cat.iter().filter(|(_, _, r)| !r.is_empty()).collect();
    let mut col = column![
        text(format!(
            "{} 个策略里 {} 个写了适用环境（[strategy.regime]）；没写的不限环境，这里不列。按 {} 这一刻判：",
            cat.len(),
            declared.len(),
            fmt_t(t)
        ))
        .size(t::s_meta())
        .color(pal::dim())
    ]
    .spacing(2);
    for (id, name, req) in declared {
        let (ok, miss) = rb::check(req, &snap);
        col = col.push(
            row![
                text(if ok { "✓ 可运行" } else { "✗ 不满足" }).size(t::s_small()).color(if ok { pal::ok() } else { pal::dim() }).width(Length::Fixed(LABEL_W)),
                text(format!("{name}（{id}）")).size(t::s_small()),
                text(miss.join("；")).size(t::s_meta()).color(pal::dim()),
            ]
            .spacing(metrics::space(2)),
        );
    }
    col = col.push(t::metadata("声明只是策略打算在哪些环境里跑，不是证明过的事实；有没有用看下面的检验台账").color(pal::dim()));
    col.into()
}

// ── 检验台账 ────────────────────────────────────────────────────────────────

fn ledger<'a>() -> Element<'a, RegimeMsg> {
    let l = rb::ledger();
    let mut col = column![
        text(format!(
            "累计试验数 {}（框架之前 {} 项 + 正式检验 {} 项；dev 与复现不计）——多重比较看的是这个总数",
            l.cumulative(),
            l.history.iter().map(|h| h.trials).sum::<u32>(),
            l.rows.iter().filter(|r| r.kind == "final").map(|r| r.trials).sum::<u32>()
        ))
        .size(t::s_small())
    ]
    .spacing(2);
    for h in &l.history {
        col = col.push(text(format!("（框架之前）{}：{} 项", h.name, h.trials)).size(t::s_meta()).color(pal::dim()));
    }
    for r in l.rows.iter().rev().take(30) {
        let (v, c) = match r.verdict {
            Some(true) => ("成立", pal::ok()),
            Some(false) => ("不成立", pal::warn()),
            None => ("—", pal::dim()),
        };
        let kind = match r.kind.as_str() {
            "final" => "正式".to_string(),
            "dev" => "开发段".to_string(),
            "replication" => "复现".to_string(),
            k => k.to_string(),
        };
        col = col.push(
            row![
                text(r.time.replace('T', " ")).size(t::s_meta()).color(pal::dim()).width(Length::Fixed(140.0)),
                text(r.name.clone()).size(t::s_small()).width(Length::Fixed(200.0)),
                text(kind).size(t::s_meta()).width(Length::Fixed(50.0)),
                text(format!("{} 项", r.trials)).size(t::s_meta()).width(Length::Fixed(40.0)),
                text(v).size(t::s_small()).color(c),
            ]
            .spacing(metrics::space(2)),
        );
    }
    col = col.push(t::metadata("检验：python -m factory.regime.validate check / dev / final <配置.toml>（结果 Markdown 在 <大数据目录>/regime/validate/<名字>/）").color(pal::dim()));
    container(col).width(Length::Fill).into()
}
