//! Tardis 历史面板 — 渲染（docs/20 §9）。
//!
//! 层次自上而下：**数据源(3) → 数据类型(8) → 图表（主图 + 该类型的衍生图，纵向堆叠）**。
//! 五种图元全部自绘（candle / line / bar / scatter / profile），**不使用 FS 原生图表、
//! 不声明任何 ticker、零交易所连接**。

use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{
    button, canvas as canvas_widget, column, container, pick_list, row, scrollable, slider, text,
};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::tardis_board::{
    Speed, TARDIS_ACCESS, TardisBoardMsg, TardisBoardState, is_playing, load_message, pick_opts,
    poll_load,
};
use super::tardis_board_readout as ro;

const ML: f32 = 58.0;
const MR: f32 = 10.0;
const MT: f32 = 10.0;
const MB: f32 = 20.0;

fn finite_range(vals: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in vals {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if lo.is_finite() && hi.is_finite() {
        let pad = if (hi - lo).abs() < 1e-12 { hi.abs().max(1.0) * 0.01 } else { (hi - lo) * 0.06 };
        Some((lo - pad, hi + pad))
    } else {
        None
    }
}

/// 按**量程**（而非绝对量级）决定小数位——否则 72900/72950/72990 会全被压成 "72.9k"。
fn fmt_tick(v: f64, span: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let step = (span / 4.0).abs();
    if step == 0.0 {
        return fmt_num(v);
    }
    // 每格至少能分辨出一位有效变化
    let dec = (-(step.log10().floor()) as i32 + 1).clamp(0, 8) as usize;
    if v.abs() >= 1e6 && step >= 1e3 {
        return format!("{:.2}M", v / 1e6);
    }
    format!("{v:.dec$}")
}

fn fmt_num(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else if a >= 1e-4 {
        format!("{v:.5}")
    } else if a == 0.0 {
        "0".into()
    } else {
        format!("{v:.2e}")
    }
}

fn fmt_time(ms: f64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// 帧耗时埋点（docs/20 §20）：累计 `cache.draw()` 的墙钟，每 300 次打一条 INFO。
/// 优化渲染前必须有这个——面板 CPU 常年 100%（上游 unconditional-rendering），
/// 用 CPU% 根本量不出边际收益（§19.1）。
mod probe {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NANOS: AtomicU64 = AtomicU64::new(0);
    static CALLS: AtomicU64 = AtomicU64::new(0);

    pub fn record(ns: u64) {
        let n = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        let tot = NANOS.fetch_add(ns, Ordering::Relaxed) + ns;
        if n.is_multiple_of(3000) {
            log::info!(
                "[tardis-board] 近 3000 次 canvas draw 平均 {:.3} ms（累计 {n} 次）",
                tot as f64 / n as f64 / 1e6
            );
        }
    }
}

/// 跨帧持有的 canvas 几何缓存（docs/20 §20）。
///
/// 原来每帧 `Cache::new()`，等于**完全绕开** iced 的几何缓存——draw 闭包每帧全量重跑
/// （热图约 8,000 次 `fill_rectangle`）。改为按 chart id 跨帧复用。
///
/// ⚠️ 两个必须守住的点：
/// 1. `Cache` **只在尺寸变化或显式 clear 时失效，不看内容**。面板换了内容还复用旧缓存
///    会画出上一份数据，故按 `Panel.generation` 作废整张表。
/// 2. **回放中不能用缓存**——播放头逐帧推进、几何逐帧变化，而 Cache 不感知内容变化，
///    复用会让图冻住不动。故 `playhead.is_some()` 时退回每帧新建。
pub(super) type CacheRef = std::rc::Rc<Cache>;

thread_local! {
    static CACHES: std::cell::RefCell<(u64, std::collections::HashMap<String, CacheRef>)> =
        std::cell::RefCell::new((u64::MAX, std::collections::HashMap::new()));
}

pub(super) fn cache_for(generation: u64, id: &str) -> CacheRef {
    CACHES.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 != generation {
            c.0 = generation;
            c.1.clear();
        }
        c.1.entry(id.to_string())
            .or_insert_with(|| std::rc::Rc::new(Cache::new()))
            .clone()
    })
}

pub(super) struct ChartCanvas {
    pub(super) ch: std::sync::Arc<ro::Chart>,
    pub(super) cache: CacheRef,
    /// 时间步进回放的播放头（ms）；None=不裁剪，显示全窗口。
    pub(super) playhead: Option<f64>,
}

/// 按播放头裁剪出「≤ 游标」的部分（docs/20 §10）。x 轴范围**保持整窗不变**，
/// 这样回放过程中坐标轴不会来回跳动，只是曲线从左往右生长。
pub(super) fn clip_to(ch: &ro::Chart, head: f64) -> ro::Chart {
    if !ch.x_is_time || ch.x.is_empty() {
        return ch.clone();
    }
    let keep = ch.x.iter().take_while(|v| **v <= head).count();
    if keep >= ch.x.len() {
        return ch.clone();
    }
    let cut = |v: &Vec<f64>| -> Vec<f64> {
        if v.len() >= ch.x.len() { v[..keep].to_vec() } else { v.clone() }
    };
    ro::Chart {
        // x 保留全长以锁定坐标轴范围；数据序列截断 → 右侧留白即「尚未播到」。
        x: ch.x.clone(),
        o: cut(&ch.o),
        h: cut(&ch.h),
        l: cut(&ch.l),
        c: cut(&ch.c),
        v: cut(&ch.v),
        y: cut(&ch.y),
        size: cut(&ch.size),
        cls: if ch.cls.len() >= ch.x.len() { ch.cls[..keep].to_vec() } else { ch.cls.clone() },
        series: ch.series.iter().map(|(n, v)| (n.clone(), cut(v))).collect(),
        ..ch.clone()
    }
}

impl ChartCanvas {
    /// 画坐标轴 + 网格，返回绘图区（x0,y0,w,h）与坐标映射闭包所需参数。
    #[allow(clippy::too_many_arguments)]
    fn axes(
        frame: &mut Frame,
        w: f32,
        h: f32,
        xr: (f64, f64),
        yr: (f64, f64),
        x_is_time: bool,
        y_label: &str,
        x_log: bool,
    ) {
        let pw = w - ML - MR;
        let ph = h - MT - MB;
        let axis = Stroke::default().with_color(crate::ui::pal::axis()).with_width(1.0);
        frame.stroke(
            &Path::line(Point::new(ML, MT + ph), Point::new(ML + pw, MT + ph)),
            axis,
        );
        frame.stroke(&Path::line(Point::new(ML, MT), Point::new(ML, MT + ph)), axis);
        // Y 网格 4 格
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            let y = MT + ph * (1.0 - t);
            frame.stroke(
                &Path::line(Point::new(ML, y), Point::new(ML + pw, y)),
                Stroke::default().with_color(crate::ui::pal::grid()).with_width(1.0),
            );
            let val = yr.0 + (yr.1 - yr.0) * t as f64;
            frame.fill_text(Text {
                content: fmt_tick(val, yr.1 - yr.0),
                position: Point::new(2.0, y - 6.0),
                color: crate::ui::pal::axis(),
                size: iced::Pixels(9.0),
                ..Default::default()
            });
        }
        // X 刻度 4 个
        for i in 0..=4 {
            let t = i as f32 / 4.0;
            let x = ML + pw * t;
            // 对数横轴：刻度值要按 exp 反算，否则条形用 log 长度、标签却按线性标，对不上。
            let val = if x_log {
                ((1.0 + xr.1).ln() * t as f64).exp() - 1.0
            } else {
                xr.0 + (xr.1 - xr.0) * t as f64
            };
            let s = if x_is_time {
                fmt_time(val)
            } else if x_log {
                fmt_num(val)
            } else {
                fmt_tick(val, xr.1 - xr.0)
            };
            frame.fill_text(Text {
                content: s,
                position: Point::new(x - 18.0, MT + ph + 5.0),
                color: crate::ui::pal::axis(),
                size: iced::Pixels(9.0),
                ..Default::default()
            });
        }
        if !y_label.is_empty() {
            frame.fill_text(Text {
                content: y_label.to_string(),
                position: Point::new(w - MR - 34.0, 0.0),
                color: crate::ui::pal::axis(),
                size: iced::Pixels(9.0),
                ..Default::default()
            });
        }
    }
}

/// 十字线用的绘图区：与画图时同一套纵轴范围（K 线的价格区不含下方 22% 成交量）。剖面图没有时间轴，不给。
fn plot_of(ch: &ro::Chart, w: f32, h: f32) -> Option<super::chart_kit::Plot> {
    let pw = w - ML - MR;
    let ph = h - MT - MB;
    if pw <= 4.0 || ph <= 4.0 {
        return None;
    }
    let xr = finite_range(ch.x.iter().copied())?;
    let (yr, ph) = match ch.kind.as_str() {
        "profile" => return None,
        "heatmap" => ((ch.y_lo, ch.y_lo + ch.y_step * ch.n_lv as f64), ph),
        "candle" => (finite_range(ch.l.iter().chain(ch.h.iter()).copied())?, ph - ph * 0.22 - 4.0),
        "scatter" => (finite_range(ch.y.iter().copied())?, ph),
        "bar" => (finite_range(ch.series.iter().flat_map(|s| s.1.iter().copied()).chain(std::iter::once(0.0)))?, ph),
        _ => (finite_range(ch.series.iter().flat_map(|s| s.1.iter().copied()))?, ph),
    };
    (xr.1 > xr.0 && yr.1 > yr.0).then_some(super::chart_kit::Plot { x0: ML, y0: MT, w: pw, h: ph, xr, yr })
}

/// 十字线附加读数：最近一个时间桶上这张图的各个值。
fn readout(ch: &ro::Chart, i: usize) -> Vec<String> {
    let g = |v: &Vec<f64>| v.get(i).copied().filter(|x| x.is_finite()).map_or_else(crate::ui::fmt::missing, fmt_num);
    let mut out = vec![if ch.x_is_time { fmt_time(ch.x[i]) } else { fmt_num(ch.x[i]) }];
    match ch.kind.as_str() {
        "candle" => out.push(format!("开 {}  高 {}  低 {}  收 {}  量 {}", g(&ch.o), g(&ch.h), g(&ch.l), g(&ch.c), g(&ch.v))),
        "scatter" => out.push(format!("{} {}", if ch.y_label.is_empty() { "Y" } else { &ch.y_label }, g(&ch.y))),
        "heatmap" => out.push(format!("中价 {}", g(&ch.mid))),
        _ => out.extend(ch.series.iter().map(|(n, v)| format!("{n} {}", g(v)))),
    }
    out
}

impl<M> canvas::Program<M> for ChartCanvas {
    type State = ();

    fn draw(
        &self,
        _s: &(),
        r: &Renderer,
        _t: &Theme,
        b: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let _t0 = std::time::Instant::now();
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            let clipped;
            let ch = match self.playhead {
                Some(head) => {
                    clipped = clip_to(&self.ch, head);
                    &clipped
                }
                None => &self.ch,
            };
            let pw = w - ML - MR;
            let ph = h - MT - MB;
            if pw <= 4.0 || ph <= 4.0 {
                return;
            }
            let empty = |frame: &mut Frame, msg: &str| {
                frame.fill_text(Text {
                    content: msg.to_string(),
                    position: Point::new(ML + 6.0, h / 2.0),
                    color: crate::ui::pal::axis(),
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
            };

            match ch.kind.as_str() {
                "profile" => {
                    // 横向深度剖面：y=价格，x=挂单量（买在左、卖在右各占半幅）。
                    let prices: Vec<f64> =
                        ch.bid_price.iter().chain(ch.ask_price.iter()).copied().collect();
                    let amts: Vec<f64> =
                        ch.bid_amount.iter().chain(ch.ask_amount.iter()).copied().collect();
                    let (Some(yr), Some(ar)) = (
                        finite_range(prices.iter().copied()),
                        finite_range(amts.iter().copied().chain(std::iter::once(0.0))),
                    ) else {
                        empty(frame, "无数据");
                        return;
                    };
                    // 挂单量用**对数**长度：实测末帧 25 档 max/中位 = 3687×，
                    // 线性长度下中位档只有最长条的 1/3687，等于看不见（同 §14 热图的病）。
                    let lmax = (1.0 + ar.1).ln().max(1e-9);
                    Self::axes(frame, w, h, (0.0, ar.1), yr, false, "价格", true);
                    let sy = |v: f64| MT + ph * (1.0 - ((v - yr.0) / (yr.1 - yr.0)) as f32);
                    let sx = |v: f64| ((1.0 + v.max(0.0)).ln() / lmax) as f32 * pw;
                    let bar_h = (ph / (prices.len().max(1) as f32) * 0.8).clamp(1.0, 14.0);
                    for (p, a) in ch.bid_price.iter().zip(ch.bid_amount.iter()) {
                        if !p.is_finite() || !a.is_finite() {
                            continue;
                        }
                        let y = sy(*p);
                        frame.fill_rectangle(
                            Point::new(ML, y - bar_h / 2.0),
                            Size::new(sx(*a).max(1.0), bar_h),
                            crate::ui::pal::up(),
                        );
                    }
                    for (p, a) in ch.ask_price.iter().zip(ch.ask_amount.iter()) {
                        if !p.is_finite() || !a.is_finite() {
                            continue;
                        }
                        let y = sy(*p);
                        frame.fill_rectangle(
                            Point::new(ML, y - bar_h / 2.0),
                            Size::new(sx(*a).max(1.0), bar_h),
                            Color { a: 0.85, ..crate::ui::pal::down() },
                        );
                    }
                    return;
                }
                "heatmap" => {
                    let (nt, nl) = (ch.x.len(), ch.n_lv);
                    if nt < 2 || nl == 0 || ch.z_bid.len() < nt || ch.y_step <= 0.0 {
                        empty(frame, "无盘口快照");
                        return;
                    }
                    let Some(xr) = finite_range(ch.x.iter().copied()) else {
                        empty(frame, "X 轴无有效值");
                        return;
                    };
                    let ylo = ch.y_lo;
                    let yhi = ch.y_lo + ch.y_step * nl as f64;
                    Self::axes(frame, w, h, xr, (ylo, yhi), true, "价格", false);
                    let sx = |v: f64| ML + pw * (((v - xr.0) / (xr.1 - xr.0)) as f32);
                    let sy = |v: f64| MT + ph * (1.0 - ((v - ylo) / (yhi - ylo)) as f32);
                    // 亮度：对数标度 + **双锚归一化**，把 [z_lo, z_hi]（非零格 p05~p99）映满量程。
                    // 单锚不行：SOL 挂单量全 ≫1，ln(1+q)/ln(1+max) 把 4 倍量差压进 0.87~1.00
                    // → 整片同色；BTC 则是单笔巨量把其余压暗。锚点由 panels.py 算好随 JSON 下发。
                    let zmax = ch
                        .z_bid
                        .iter()
                        .chain(ch.z_ask.iter())
                        .flat_map(|r| r.iter().copied())
                        .fold(0.0f64, f64::max);
                    if zmax <= 0.0 {
                        empty(frame, "窗口内无挂单量");
                        return;
                    }
                    // 锚点缺失/退化（旧 JSON、或全簿同量）时回退到「对数到最大值」。
                    let two_anchor = ch.z_lo > 0.0 && ch.z_hi > ch.z_lo;
                    let (lo_ln, span_ln) = if two_anchor {
                        (ch.z_lo.ln(), ch.z_hi.ln() - ch.z_lo.ln())
                    } else {
                        (0.0, (1.0 + zmax).ln())
                    };
                    let cw = (pw / nt as f32).max(0.6);
                    let chh = (ph / nl as f32).max(0.6);
                    // 播放头之后的时间桶不画（时间步进回放，docs/20 §10）
                    let last = match self.playhead {
                        Some(head) => ch.x.iter().take_while(|v| **v <= head).count(),
                        None => nt,
                    };
                    let mut cell = |x: f32, k: usize, q: f64, col: Color| {
                        if q <= 0.0 {
                            return;
                        }
                        let lq = if two_anchor { q.ln() } else { (1.0 + q).ln() };
                        let a = ((lq - lo_ln) / span_ln).clamp(0.06, 1.0) as f32;
                        let y = sy(ylo + ch.y_step * (k as f64 + 1.0));
                        frame.fill_rectangle(
                            Point::new(x, y),
                            Size::new(cw, chh),
                            Color { a: a * 0.92, ..col },
                        );
                    };
                    for ti in 0..last.min(nt) {
                        let x = sx(ch.x[ti]);
                        for (k, q) in ch.z_bid[ti].iter().enumerate().take(nl) {
                            cell(x, k, *q, crate::ui::pal::up()); // 买单堆积
                        }
                        if let Some(row) = ch.z_ask.get(ti) {
                            for (k, q) in row.iter().enumerate().take(nl) {
                                cell(x, k, *q, crate::ui::pal::down()); // 卖单堆积
                            }
                        }
                    }
                    // 中价线
                    let mut prev: Option<Point> = None;
                    for ti in 0..last.min(nt).min(ch.mid.len()) {
                        let m = ch.mid[ti];
                        if !m.is_finite() {
                            prev = None;
                            continue;
                        }
                        let p = Point::new(sx(ch.x[ti]), sy(m));
                        if let Some(q) = prev {
                            frame.stroke(
                                &Path::line(q, p),
                                Stroke::default()
                                    .with_color(crate::ui::pal::alpha(crate::ui::pal::txt(), 0.75))
                                    .with_width(1.0),
                            );
                        }
                        prev = Some(p);
                    }
                    if let Some(head) = self.playhead
                        && head >= xr.0
                        && head <= xr.1
                    {
                        let x = sx(head);
                        frame.stroke(
                            &Path::line(Point::new(x, MT), Point::new(x, MT + ph)),
                            Stroke::default()
                                .with_color(crate::ui::pal::alpha(crate::ui::pal::reference(), 0.85))
                                .with_width(1.2),
                        );
                    }
                    return;
                }
                _ => {}
            }

            let n = ch.x.len();
            if n == 0 {
                empty(frame, "无数据");
                return;
            }
            let Some(xr) = finite_range(ch.x.iter().copied()) else {
                empty(frame, "X 轴无有效值");
                return;
            };
            let sx = |v: f64| ML + pw * (((v - xr.0) / (xr.1 - xr.0)) as f32);
            // 时间轴上的缺口（docs/35 §6.3）：标出来，折线在缺口处断开，不插值
            let gap_at = if ch.x_is_time { super::chart_kit::gaps(&ch.x) } else { Vec::new() };

            match ch.kind.as_str() {
                "candle" => {
                    let Some(yr) = finite_range(ch.l.iter().chain(ch.h.iter()).copied()) else {
                        empty(frame, "无有效价格");
                        return;
                    };
                    // 下方 22% 留给成交量
                    let vh = ph * 0.22;
                    let cph = ph - vh - 4.0;
                    Self::axes(frame, w, h, xr, yr, ch.x_is_time, "价格", false);
                    let sy = |v: f64| MT + cph * (1.0 - ((v - yr.0) / (yr.1 - yr.0)) as f32);
                    let bw = (pw / n as f32 * 0.7).clamp(1.0, 12.0);
                    for i in 0..n {
                        let (o, hh, ll, c) = (
                            *ch.o.get(i).unwrap_or(&f64::NAN),
                            *ch.h.get(i).unwrap_or(&f64::NAN),
                            *ch.l.get(i).unwrap_or(&f64::NAN),
                            *ch.c.get(i).unwrap_or(&f64::NAN),
                        );
                        if !(o.is_finite() && hh.is_finite() && ll.is_finite() && c.is_finite()) {
                            continue;
                        }
                        let x = sx(ch.x[i]);
                        let col = if c >= o { crate::ui::pal::up() } else { crate::ui::pal::down() };
                        frame.stroke(
                            &Path::line(Point::new(x, sy(hh)), Point::new(x, sy(ll))),
                            Stroke::default().with_color(col).with_width(1.0),
                        );
                        let (yt, yb) = (sy(o.max(c)), sy(o.min(c)));
                        frame.fill_rectangle(
                            Point::new(x - bw / 2.0, yt),
                            Size::new(bw, (yb - yt).max(1.0)),
                            col,
                        );
                    }
                    if let Some(vr) = finite_range(ch.v.iter().copied().chain(std::iter::once(0.0)))
                    {
                        let vy0 = MT + ph;
                        for i in 0..n {
                            let v = *ch.v.get(i).unwrap_or(&f64::NAN);
                            if !v.is_finite() {
                                continue;
                            }
                            let hgt = ((v / vr.1.max(1e-12)) as f32 * vh).max(0.5);
                            let up = ch.c.get(i).copied().unwrap_or(0.0)
                                >= ch.o.get(i).copied().unwrap_or(0.0);
                            frame.fill_rectangle(
                                Point::new(sx(ch.x[i]) - bw / 2.0, vy0 - hgt),
                                Size::new(bw, hgt),
                                Color { a: 0.55, ..if up { crate::ui::pal::up() } else { crate::ui::pal::down() } },
                            );
                        }
                    }
                }
                "scatter" => {
                    let Some(yr) = finite_range(ch.y.iter().copied()) else {
                        empty(frame, "无有效 Y");
                        return;
                    };
                    Self::axes(frame, w, h, xr, yr, ch.x_is_time, &ch.y_label, false);
                    let sy = |v: f64| MT + ph * (1.0 - ((v - yr.0) / (yr.1 - yr.0)) as f32);
                    let smax = ch.size.iter().copied().filter(|v| v.is_finite()).fold(0.0, f64::max);
                    for i in 0..n {
                        let y = *ch.y.get(i).unwrap_or(&f64::NAN);
                        if !y.is_finite() {
                            continue;
                        }
                        let sz = ch.size.get(i).copied().unwrap_or(1.0);
                        let rad = if smax > 0.0 {
                            (2.0 + 6.0 * (sz / smax).sqrt() as f32).clamp(1.5, 9.0)
                        } else {
                            3.0
                        };
                        let col = match ch.cls.get(i).copied().unwrap_or(0) {
                            1 => crate::ui::pal::up(),
                            2 => crate::ui::pal::down(),
                            _ => crate::ui::pal::dim(),
                        };
                        frame.fill(
                            &Path::circle(Point::new(sx(ch.x[i]), sy(y)), rad),
                            Color { a: 0.75, ..col },
                        );
                    }
                }
                "bar" => {
                    let Some(yr) = finite_range(
                        ch.series
                            .iter()
                            .flat_map(|s| s.1.iter().copied())
                            .chain(std::iter::once(0.0)),
                    ) else {
                        empty(frame, "无有效数值");
                        return;
                    };
                    Self::axes(frame, w, h, xr, yr, ch.x_is_time, &ch.y_label, false);
                    let sy = |v: f64| MT + ph * (1.0 - ((v - yr.0) / (yr.1 - yr.0)) as f32);
                    let k = ch.series.len().max(1);
                    let bw = (pw / n.max(1) as f32 / k as f32 * 0.8).clamp(0.7, 10.0);
                    let zero = sy(0.0f64.clamp(yr.0, yr.1));
                    for (si, (_, vals)) in ch.series.iter().enumerate() {
                        let col = crate::ui::pal::series(si);
                        for i in 0..n.min(vals.len()) {
                            let v = vals[i];
                            if !v.is_finite() {
                                continue;
                            }
                            let x = sx(ch.x[i]) - (k as f32 * bw) / 2.0 + si as f32 * bw;
                            let y = sy(v);
                            frame.fill_rectangle(
                                Point::new(x, y.min(zero)),
                                Size::new(bw, (y - zero).abs().max(0.8)),
                                Color { a: 0.85, ..col },
                            );
                        }
                    }
                }
                _ => {
                    // line
                    let Some(yr) =
                        finite_range(ch.series.iter().flat_map(|s| s.1.iter().copied()))
                    else {
                        empty(frame, "无有效数值");
                        return;
                    };
                    Self::axes(frame, w, h, xr, yr, ch.x_is_time, &ch.y_label, false);
                    let sy = |v: f64| MT + ph * (1.0 - ((v - yr.0) / (yr.1 - yr.0)) as f32);
                    for (si, (_, vals)) in ch.series.iter().enumerate() {
                        let col = crate::ui::pal::series(si);
                        let mut pending: Option<Point> = None;
                        for i in 0..n.min(vals.len()) {
                            let v = vals[i];
                            if !v.is_finite() {
                                pending = None; // 断点：NaN 处断线，不连虚假直线
                                continue;
                            }
                            if gap_at.contains(&i) {
                                pending = None; // 时间缺口同样断开
                            }
                            let p = Point::new(sx(ch.x[i]), sy(v));
                            if let Some(prev) = pending {
                                frame.stroke(
                                    &Path::line(prev, p),
                                    Stroke::default().with_color(col).with_width(1.2),
                                );
                            }
                            pending = Some(p);
                        }
                    }
                    // 图例
                    for (si, (name, _)) in ch.series.iter().enumerate() {
                        frame.fill_text(Text {
                            content: name.clone(),
                            position: Point::new(ML + 6.0 + si as f32 * 88.0, MT + 1.0),
                            color: crate::ui::pal::series(si),
                            size: iced::Pixels(9.0),
                            ..Default::default()
                        });
                    }
                }
            }

            if let Some(plot) = plot_of(ch, w, h) {
                super::chart_kit::gap_bands(frame, &plot, &ch.x, &gap_at);
            }
            // 播放头竖线：标出「已播到哪」，右侧留白即尚未播放的部分。
            if let Some(head) = self.playhead
                && ch.x_is_time
                && head >= xr.0
                && head <= xr.1
            {
                let x = sx(head);
                frame.stroke(
                    &Path::line(Point::new(x, MT), Point::new(x, MT + ph)),
                    Stroke::default()
                        .with_color(crate::ui::pal::alpha(crate::ui::pal::reference(), 0.85))
                        .with_width(1.2),
                );
            }
        });
        probe::record(_t0.elapsed().as_nanos() as u64);
        // 十字线读数（docs/35 §6.3）：两轴读数 + 最近时间桶上这张图的值；不进缓存，跟着光标每帧画
        let mut out = vec![geo];
        if let Some(p) = cursor.position_in(b)
            && let Some(plot) = plot_of(&self.ch, b.width, b.height)
        {
            let mut hair = Frame::new(r, b.size());
            let extra = super::chart_kit::nearest(&self.ch.x, plot.vx(p.x)).map(|i| readout(&self.ch, i)).unwrap_or_default();
            let (is_time, xspan, yspan) = (self.ch.x_is_time, plot.xr.1 - plot.xr.0, plot.yr.1 - plot.yr.0);
            super::chart_kit::crosshair(
                &mut hair,
                &plot,
                p,
                |v| if is_time { fmt_time(v) } else { fmt_tick(v, xspan) },
                |v| fmt_tick(v, yspan),
                &extra,
            );
            out.push(hair.into_geometry());
        }
        out
    }
}

fn chip<'a>(
    label: String,
    active: bool,
    enabled: bool,
    msg: TardisBoardMsg,
) -> Element<'a, TardisBoardMsg> {
    let t = text(label).size(crate::ui::text::s_body()).color(if !enabled {
        crate::ui::pal::pend()
    } else if active {
        crate::ui::pal::txt()
    } else {
        crate::ui::pal::dim()
    });
    let b = button(t)
        .padding(crate::ui::metrics::pad2(0, 3))
        .style(move |theme, status| crate::style::button::modifier(theme, status, active));
    if enabled { b.on_press(msg).into() } else { b.into() }
}

fn label<'a>(s: &str) -> Element<'a, TardisBoardMsg> {
    text(s.to_string()).size(crate::ui::text::s_body()).into()
}

/// `hosted` = 数据选择正显示在检查器里（见 `ws::inspector_props`）。
/// ① 数据 + 读法 + ② 数据类型（docs/42 第 3 期：检查器托管时整块进「数据」页）。
fn data_block(app: &TardisBoardState, cat: &ro::Catalog, picker: bool, wrap: usize) -> Element<'static, TardisBoardMsg> {
    let mut src_col = column![label("① 数据")].spacing(4);
    if picker {
        src_col = src_col.push(super::data_picker_view::view(&app.pick, &pick_opts()).map(TardisBoardMsg::Data));
    }
    if app.pick.local_key() == Some("tardis") {
        let mut r = row![label("读法")].spacing(6).align_y(Alignment::Center);
        for (k, name) in TARDIS_ACCESS {
            r = r.push(chip(name.into(), app.access == k, true, TardisBoardMsg::Access(k.into())));
        }
        src_col = src_col.push(r.wrap());
    }
    // ② 数据类型（当天没有的类型置灰，不可点——不画空面板）
    let have = app.avail_types();
    let all: Vec<String> = if cat.type_labels.is_empty() {
        have.clone()
    } else {
        let mut v: Vec<String> = cat.type_labels.keys().cloned().collect();
        v.sort_by_key(|t| have.iter().position(|x| x == t).unwrap_or(usize::MAX));
        v
    };
    let mut ty_row1 = row![label("② 数据类型")].spacing(6).align_y(Alignment::Center);
    let mut ty_row2 = row![label("                ")].spacing(6).align_y(Alignment::Center);
    for (i, t) in all.iter().enumerate() {
        let has = have.contains(t);
        let c = chip(
            cat.type_labels.get(t).cloned().unwrap_or_else(|| t.clone()),
            *t == app.dtype,
            has,
            TardisBoardMsg::TypePick(t.clone()),
        );
        if i < wrap {
            ty_row1 = ty_row1.push(c);
        } else {
            ty_row2 = ty_row2.push(c);
        }
    }
    column![src_col, ty_row1.wrap(), ty_row2.wrap()].spacing(8).into()
}

/// 检查器「数据」页：数据选择 + 读法 + 数据类型。
pub fn inspector_data(app: &TardisBoardState) -> Element<'static, TardisBoardMsg> {
    let cat = ro::catalog();
    data_block(app, &cat, true, usize::MAX)
}

pub fn pane_body(app: &TardisBoardState, hosted: bool) -> Element<'_, TardisBoardMsg> {
    // 每帧轮询后台加载（顺带收割已结束的子进程并刷新面板缓存）。
    let loading = poll_load();
    // catalog 每帧只取一次（只用它的类型中文名）：每次调用都要 stat + 深拷贝整份清单。
    let cat = ro::catalog();
    let p = ro::panel();
    let ph = ro::playhead();
    // 播放头只在与**当前已加载面板**的时间范围吻合时生效，避免用上一次回放的游标
    // 去裁当前这张图（换类型/换窗口后范围会变）。
    // 播放头记录是否属于**当前这张已加载面板**（换类型/换窗口后范围会变，旧游标必须失效）。
    let ph_matches = match (ph.active, ro::panel_time_span(&p)) {
        (true, Some((t0, t1))) => (ph.t0_ms - t0).abs() < 1.0 && (ph.t1_ms - t1).abs() < 1.0,
        _ => false,
    };
    let playing_head = (ph_matches && ph.state != "done").then_some(ph.data_ts);
    // 播放头：回放中跟 feeder；静止时若拖过进度条则停在拖到的位置（否则显示整窗）。
    let span = ro::panel_time_span(&p);
    let head = playing_head.or_else(|| match (app.seek_pct, span) {
        (Some(f), Some((t0, t1))) => Some(t0 + (t1 - t0) * (f as f64) / 100.0),
        _ => None,
    });
    // 进度条位置：回放中用 feeder 的 pct，否则用拖动值。
    let bar_pct = if playing_head.is_some() {
        ph.pct as f32
    } else {
        app.seek_pct.unwrap_or(0.0)
    };

    let header = column![
        text("历史数据面板（Tardis / Databento / 本地录制）— 数据 → 数据类型 → 图表")
            .size(crate::ui::text::s_title())
            .color(crate::ui::pal::info()),
        text("零交易所流：全部数据来自本地历史文件，不建立任何实时连接")
            .size(crate::ui::text::s_meta())
            .color(crate::ui::pal::pend()),
    ]
    .spacing(3);

    // ③ 出图（标的 / 日期 / 时段在 ① 里选）
    let picks = row![
        label("③ 出图"),
        // 固定宽度：文字在「单符号」「跨符号对比」间切换时长度不同，不定宽会把整行右推，
        // 「加载」按钮位置随状态漂移（实测点错过多次）。
        container(chip(
            if app.compare { "跨符号对比".into() } else { "单符号".into() },
            app.compare,
            app.n_symbols() > 1,
            TardisBoardMsg::ToggleCompare,
        ))
        .width(Length::Fixed(104.0)),
        {
            let b = button(text(if loading.is_some() { "加载中…" } else { "加载" }).size(crate::ui::text::s_body()))
                .padding(crate::ui::metrics::pad2(0, 4));
            // 加载中不再接受点击（避免叠起多个子进程）
            if loading.is_none() { b.on_press(TardisBoardMsg::Load) } else { b }
        },
        chip(
            if app.auto_load { "自动".into() } else { "手动".into() },
            app.auto_load,
            true,
            TardisBoardMsg::ToggleAuto,
        ),
        // 价格带只影响 L2 热图，故只在该类型下出现——省得对别的类型显示无效控件
        if app.dtype == "incremental_book_L2" {
            row![
                label("热图带宽"),
                pick_list(
                    vec![50u32, 100, 200, 400, 800],
                    Some(app.band_ticks),
                    TardisBoardMsg::BandPick
                )
                .text_size(12),
                label("tick"),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        } else {
            row![]
        },
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    // ④ 时间步进回放（docs/20 §10）：由 §8 的推流器 --mode panel 驱动播放头。
    let playing = is_playing();
    let play_row = row![
        label("④ 回放"),
        pick_list(Speed::ALL.to_vec(), Some(app.speed), TardisBoardMsg::SpeedPick).text_size(12),
        if playing {
            button(text("■ 停止").size(crate::ui::text::s_body())).padding(crate::ui::metrics::pad2(0, 4)).on_press(TardisBoardMsg::StopPlay)
        } else {
            button(text("▶ 回放").size(crate::ui::text::s_body())).padding(crate::ui::metrics::pad2(0, 4)).on_press(TardisBoardMsg::Play)
        },
        // 进度条：拖动即 seek（回放中带新起点重起 feeder，静止时只挪游标）
        slider(0.0..=100.0, bar_pct, TardisBoardMsg::Seek)
            .step(0.5f32)
            .width(Length::FillPortion(3)),
        text(match head {
            Some(h) => format!("{} · {bar_pct:.0}%", fmt_time(h)),
            None if ph_matches && ph.state == "done" => "播完（整窗）".to_string(),
            None => "整窗".to_string(),
        })
        .size(crate::ui::text::s_small())
        .color(if head.is_some() {
            crate::ui::pal::warn()
        } else {
            crate::ui::pal::dim()
        }),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    // ⑤ 导出（docs/20 §17）：CSV 从面板 JSON 生成；存图走窗口截图（所见即所得）。
    let export_row = row![
        label("⑤ 导出"),
        {
            let b = button(text("⬇ CSV").size(crate::ui::text::s_body())).padding(crate::ui::metrics::pad2(0, 3));
            if loading.is_none() && p.loaded { b.on_press(TardisBoardMsg::ExportCsv) } else { b }
        },
        {
            let b = button(text("📷 存图").size(crate::ui::text::s_body())).padding(crate::ui::metrics::pad2(0, 3));
            if loading.is_none() && p.loaded { b.on_press(TardisBoardMsg::ExportPng) } else { b }
        },
        text("→ ~/ws-data/cockpit/export/").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    // 检查器「属性」页（docs/42 第 4 期）：出图（单符号 / 对比、加载、自动、热图带宽）与导出
    if super::inspector_props::part() == super::inspector_props::Part::Props {
        return column![picks.wrap(), export_row.wrap()].spacing(8).into();
    }

    let hint_text = match &loading {
        Some(d) => format!("⏳ 后台加载中 {d}…（界面不冻结，可继续操作）"),
        None => {
            let m = load_message();
            if m.is_empty() { app.hint.clone() } else { m }
        }
    };
    let hint = text(hint_text.clone()).size(crate::ui::text::s_small()).color(if hint_text.starts_with('✗') {
        crate::ui::pal::down()
    } else if loading.is_some() {
        crate::ui::pal::warn()
    } else {
        crate::ui::pal::dim()
    });

    let mut body = column![].spacing(10);
    if let Some(e) = cat.error.clone() {
        body = body.push(text(e).size(crate::ui::text::s_small()).color(crate::ui::pal::down()));
    }
    if !p.loaded {
        body = body.push(
            text("尚未加载 —— 选好上面三层后点「加载」").size(crate::ui::text::s_body()).color(crate::ui::pal::dim()),
        );
    } else if let Some(e) = p.error.clone() {
        body = body.push(text(format!("· {e}")).size(crate::ui::text::s_body()).color(crate::ui::pal::down()));
    } else {
        // 下方图是**已加载**的那一份；若与当前三层选择不符，明示，避免误读成当前选择的结果。
        // 对比模式下 p.symbol 是多符号拼接，不能拿去比；改比「模式是否一致」。
        let symbol_matches = if p.compare || app.compare {
            p.compare == app.compare
        } else {
            p.symbol == app.symbol
        };
        let stale = p.source != app.source
            || p.dtype != app.dtype
            || !symbol_matches
            || p.date != app.date
            || p.start != app.start_hm
            || p.minutes != app.minutes;
        body = body.push(
            text(format!(
                "{} · {} · {} {} {} +{}min · {} 行 · {} 张图{}",
                p.source_label, p.type_label, p.symbol, p.date, p.start, p.minutes, p.rows,
                p.charts.len(),
                if stale { "　⚠ 这是上次加载的结果，点「加载」刷新" } else { "" }
            ))
            .size(crate::ui::text::s_small())
            .color(if stale { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
        );
        for ch in &p.charts {
            let is_main = !ch.title.starts_with("衍生");
            let title = text(ch.title.clone()).size(crate::ui::text::s_emph()).color(if is_main {
                crate::ui::pal::txt()
            } else {
                crate::ui::pal::info()
            });
            let cv: Element<'_, TardisBoardMsg> =
                canvas_widget(ChartCanvas {
                    ch: ch.clone(), // Arc：仅增引用计数
                    // 回放中几何逐帧变化，缓存不感知内容变化，复用会冻住 → 退回每帧新建
                    cache: match head {
                        Some(_) => std::rc::Rc::new(Cache::new()),
                        None => cache_for(p.generation, &ch.id),
                    },
                    playhead: head,
                })
                    .width(Length::Fill)
                    .height(Length::Fixed(match (ch.kind.as_str(), is_main) {
                        ("heatmap", _) => 300.0, // 二维图需要更多纵向空间
                        (_, true) => 210.0,
                        _ => 130.0,
                    }))
                    .into();
            let mut col = column![title, cv].spacing(3);
            if !ch.note.is_empty() {
                col = col.push(text(ch.note.clone()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()));
            }
            body = body.push(container(col).padding(crate::ui::metrics::space(1)));
        }
    }

    // 检查器托管时（docs/42 第 3 期）：标题两行进「说明」，数据 / 读法 / 数据类型进「数据」，主区只留出图、回放、导出与图
    let top = if hosted {
        column![]
    } else {
        column![header, data_block(app, &cat, true, 4)]
    }
    .spacing(8);
    container(
        if hosted {
            // 托管时：提示行 + 回放条（查看器的一部分）+ 图；出图、导出在检查器「属性」页，「加载」也在工具栏
            column![hint, crate::ui::mark::content(column![play_row, scrollable(body).height(Length::Fill)].spacing(8))]
        } else {
            column![top, picks, play_row, export_row, hint, crate::ui::mark::content(scrollable(body).height(Length::Fill))]
        }
            .spacing(8)
            .padding(crate::ui::metrics::space(4)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
