//! 全市场雷达 pane 的视图渲染（docs/22 §2 ① / §6）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板；数据走
//! [`super::radar_readout`] 旁路快照，交互状态走 [`super::radar`]。
//!
//! 表达形式对齐 TradingView 的 Heatmap 与 Screener：
//!
//! - **离散分档色阶 + 图例**，不是连续渐变。几百个小格上，连续渐变的相邻档根本分辨不出，
//!   而分档能让人一眼数出「这格比那格深两档」。图例与上色**共用同一份分档定义**——
//!   图例和格子对不上比没有图例更糟。
//! - **Size by / Color by / 分组** 三个选择器（TV 热图顶部那排）。
//! - **字号随格子面积缩放的双行标签**（代号 + 数值），小于可辨识尺寸就只留色块。
//! - 代号显示**基础币**（BTC 而非 BTCUSDT），同 TV 的加密热图。
//! - Screener：**点列头排序**（带 ▲▼，同列再点翻向）+ 列组切换。
//!
//! 配色用蓝(涨)–橙(跌)而非 TV 默认的红绿：树图上格子多且小，红绿对色盲不可分辨。

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{button, canvas as canvas_widget, column, container, row, scrollable, text};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use super::radar::{
    order, ColorBy, ColumnSet, GroupBy, RadarMsg, SizeBy, SortKey, ViewState,
};
use super::radar_readout::{RadarReadout, RadarRow, WINDOWS};
use super::treemap::{squarify_nested, Rect};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);

// ───────────────────────── 离散色阶 ─────────────────────────

const C_NEUTRAL: Color = Color::from_rgb(0.20, 0.21, 0.24);
/// 跌档，由弱到强。
///
/// ⚠ 最弱档**必须仍带明确色相**。第一版把它做得太暗（跟中性灰几乎同色），
/// 而市场大部分时候就落在最弱档——结果整张图看着一片灰蓝，
/// 明明 39% 的标的在跌却一个橙格都看不见。TV 的最弱档也是明确有色、只是降饱和。
const DOWN: [Color; 4] = [
    Color::from_rgb(0.55, 0.36, 0.22),
    Color::from_rgb(0.70, 0.42, 0.20),
    Color::from_rgb(0.84, 0.49, 0.17),
    Color::from_rgb(0.97, 0.58, 0.16),
];
/// 涨档，由弱到强。
const UP: [Color; 4] = [
    Color::from_rgb(0.20, 0.36, 0.50),
    Color::from_rgb(0.18, 0.46, 0.68),
    Color::from_rgb(0.19, 0.56, 0.84),
    Color::from_rgb(0.24, 0.66, 1.00),
];

/// 文字专用色阶：填充档是给大色块设计的，最弱档拿来当深色背景上的文字几乎读不出来。
/// 同一分档、更高亮度，语义一致而可读。
const DOWN_TXT: [Color; 4] = [
    Color::from_rgb(0.72, 0.56, 0.42),
    Color::from_rgb(0.85, 0.61, 0.34),
    Color::from_rgb(0.93, 0.63, 0.28),
    Color::from_rgb(1.00, 0.67, 0.26),
];
const UP_TXT: [Color; 4] = [
    Color::from_rgb(0.56, 0.71, 0.86),
    Color::from_rgb(0.46, 0.74, 0.96),
    Color::from_rgb(0.38, 0.77, 1.00),
    Color::from_rgb(0.35, 0.82, 1.00),
];

/// 分档边界（对称，绝对值递增）。
pub(crate) fn edges(cb: ColorBy) -> [f64; 4] {
    match cb {
        // 实测 z 分布：median≈0.13、p90≈1.5、max≈5。首档取 0.5 会把半个市场压进中性档，
        // 图上就没有信息了；0.3 起分档才让常态波动也看得出方向。
        ColorBy::SpeedZ => [0.3, 0.9, 1.8, 3.0],
        ColorBy::Change => [0.005, 0.01, 0.02, 0.03],
        ColorBy::VolZ => [1.0, 2.0, 3.0, 4.0],
    }
}

/// 落在第几档：0 = 中性，1..=4 由弱到强。
pub(crate) fn bucket(v: f64, e: &[f64; 4]) -> usize {
    let a = v.abs();
    if !a.is_finite() {
        return 0;
    }
    e.iter().filter(|x| a >= **x).count()
}

fn pick(v: Option<f64>, cb: ColorBy, up: &[Color; 4], down: &[Color; 4], zero: Color) -> Color {
    match v {
        Some(v) if v.is_finite() => {
            let b = bucket(v, &edges(cb));
            if b == 0 {
                zero
            } else if v >= 0.0 {
                up[b - 1]
            } else {
                down[b - 1]
            }
        }
        _ => zero,
    }
}

fn fade(c: Color, toward: Color, t: f32) -> Color {
    Color::from_rgb(
        c.r + (toward.r - c.r) * t,
        c.g + (toward.g - c.g) * t,
        c.b + (toward.b - c.b) * t,
    )
}

/// 值 → 树图填充色。`trusted=false`（未热身 / 借横截面基线）向中性去饱和，视觉上就弱一等。
pub(crate) fn scale_color(v: Option<f64>, cb: ColorBy, trusted: bool) -> Color {
    let c = pick(v, cb, &UP, &DOWN, C_NEUTRAL);
    if trusted { c } else { fade(c, C_NEUTRAL, 0.6) }
}

/// 值 → 表格文字色（同一分档，更高亮度）。
pub(crate) fn scale_text(v: Option<f64>, cb: ColorBy, trusted: bool) -> Color {
    let c = pick(v, cb, &UP_TXT, &DOWN_TXT, C_DIM);
    if trusted { c } else { fade(c, C_DIM, 0.55) }
}

fn edge_label(cb: ColorBy, e: f64) -> String {
    match cb {
        ColorBy::Change => format!("{:.1}%", e * 100.0),
        _ => format!("{e:.1}σ"),
    }
}

// ───────────────────────── 取值与格式化 ─────────────────────────

/// 树图上色用的指标。
fn color_metric(r: &RadarRow, v: ViewState) -> Option<f64> {
    match v.color_by {
        ColorBy::SpeedZ => r.z_ret[v.win],
        ColorBy::Change => r.ret[v.win],
        ColorBy::VolZ => r.z_vol,
    }
}

fn area_weight(r: &RadarRow, s: SizeBy) -> f64 {
    match s {
        SizeBy::Turnover => r.quote_vol_24h.max(0.0),
        SizeBy::Equal => 1.0,
    }
}

/// `BTCUSDT` → `BTC`。TV 的加密热图显示基础币，格子里放得下且更好认。
pub(crate) fn base_asset(sym: &str) -> &str {
    for q in ["USDT", "USDC", "FDUSD", "BTC", "ETH", "BNB"] {
        if let Some(b) = sym.strip_suffix(q) {
            if !b.is_empty() {
                return b;
            }
        }
    }
    sym
}

fn usd(v: f64) -> String {
    if v >= 1e9 {
        format!("{:.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0}K", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

fn price(v: f64) -> String {
    if v >= 1000.0 {
        format!("{v:.1}")
    } else if v >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.6}")
    }
}

fn opt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:+.2}%", x * 100.0)).unwrap_or_else(|| "—".into())
}

fn opt_z(v: Option<f64>) -> String {
    v.map(|x| format!("{x:+.2}")).unwrap_or_else(|| "—".into())
}

// ───────────────────────── Screener 列定义 ─────────────────────────

pub(crate) struct Col {
    pub key: SortKey,
    pub title: String,
    pub width: f32,
}

/// 当前列组下要显示的列（对应 TV Screener 顶部的 Overview/Performance/… 标签）。
pub(crate) fn columns(v: ViewState) -> Vec<Col> {
    let c = |key, title: &str, width| Col {
        key,
        title: title.to_string(),
        width,
    };
    let mut out = vec![c(SortKey::Symbol, "标的", 118.0), c(SortKey::Venue, "venue", 62.0)];
    match v.cols {
        ColumnSet::Overview => {
            out.push(c(SortKey::Price, "价格", 88.0));
            out.push(c(SortKey::Ret(v.win), &format!("{} 涨跌", WINDOWS[v.win]), 78.0));
            out.push(c(SortKey::Z(v.win), &format!("{} 速度z", WINDOWS[v.win]), 78.0));
            out.push(c(SortKey::VolZ, "量异常z", 74.0));
            out.push(c(SortKey::Ret(5), "24h", 74.0));
            out.push(c(SortKey::Turnover, "24h 额", 74.0));
        }
        ColumnSet::Performance => {
            for (i, w) in WINDOWS.iter().enumerate() {
                out.push(c(SortKey::Ret(i), w, 74.0));
            }
            out.push(c(SortKey::Turnover, "24h 额", 74.0));
        }
        ColumnSet::Speed => {
            for (i, w) in WINDOWS.iter().enumerate() {
                out.push(c(SortKey::Z(i), w, 68.0));
            }
            out.push(c(SortKey::Turnover, "24h 额", 74.0));
        }
        ColumnSet::Volume => {
            out.push(c(SortKey::VolZ, "量异常z", 80.0));
            out.push(c(SortKey::CntZ, "笔数异常z", 84.0));
            out.push(c(SortKey::Turnover, "24h 额", 80.0));
            out.push(c(SortKey::Ret(v.win), &format!("{} 涨跌", WINDOWS[v.win]), 78.0));
            out.push(c(SortKey::Price, "价格", 88.0));
        }
    }
    out
}

/// 按 24h 成交额取前 n 个（树图选行）。同额按标的定序，避免刷新抖动。
pub(crate) fn top_by_turnover(rows: &[RadarRow], n: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| {
        rows[b]
            .quote_vol_24h
            .partial_cmp(&rows[a].quote_vol_24h)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rows[a].symbol.cmp(&rows[b].symbol))
    });
    idx.truncate(n);
    idx
}

/// 一格的文本与颜色。
pub(crate) fn cell_text(r: &RadarRow, k: SortKey) -> (String, Color) {
    let trusted = r.trustworthy();
    match k {
        SortKey::Symbol => (
            r.symbol.clone(),
            if trusted { C_TXT } else { C_DIM },
        ),
        SortKey::Venue => (r.venue.trim_start_matches("binance:").to_string(), C_DIM),
        SortKey::Price => (price(r.price), C_TXT),
        SortKey::Turnover => (usd(r.quote_vol_24h), C_DIM),
        SortKey::Ret(i) => (opt_pct(r.ret[i]), scale_text(r.ret[i], ColorBy::Change, trusted)),
        SortKey::Z(i) => (opt_z(r.z_ret[i]), scale_text(r.z_ret[i], ColorBy::SpeedZ, trusted)),
        SortKey::VolZ => (opt_z(r.z_vol), scale_text(r.z_vol, ColorBy::VolZ, trusted)),
        SortKey::CntZ => (opt_z(r.z_cnt), scale_text(r.z_cnt, ColorBy::VolZ, trusted)),
    }
}

// ───────────────────────── 树图画布 ─────────────────────────

struct TileData {
    label: String,
    value: String,
    weight: f64,
    color: Color,
}

struct GroupData {
    title: String,
    tiles: Vec<TileData>,
}

struct TreemapCanvas {
    groups: Vec<GroupData>,
    header_h: f32,
    cache: std::rc::Rc<Cache>,
}

const MIN_LABEL_W: f32 = 34.0;
const MIN_LABEL_H: f32 = 18.0;
/// 半角字符的平均字宽 / 字号。canvas 里拿不到实际排版宽度，只能估。
const ADV_NARROW: f32 = 0.62;
/// 全角（CJK 等）字宽 / 字号。Binance 有中文名标的（如「我踏马来了」），
/// 按半角估会严重低估宽度，标签直接压到隔壁格子上。
const ADV_WIDE: f32 = 1.0;

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// 文本宽度，以「字号的倍数」计。
pub(crate) fn text_units(s: &str) -> f32 {
    s.chars()
        .map(|c| if is_wide(c) { ADV_WIDE } else { ADV_NARROW })
        .sum()
}

/// 让 `s` 恰好装进 `avail` 的字号（不超过 `max`）。
pub(crate) fn fit_font(s: &str, avail: f32, max: f32) -> f32 {
    let u = text_units(s);
    if u <= 0.0 {
        return max;
    }
    (avail / u).min(max)
}

/// 把文本裁到 `avail` 像素内。裁到 2 字符以下就整个不画——一两个字母认不出是谁。
pub(crate) fn fit_text(s: &str, font_size: f32, avail: f32) -> Option<String> {
    if avail <= 0.0 || font_size <= 0.0 {
        return None;
    }
    if text_units(s) * font_size <= avail {
        return Some(s.to_string());
    }
    let mut used = 0.0f32;
    let mut out = String::new();
    let mut n = 0usize;
    for c in s.chars() {
        let w = if is_wide(c) { ADV_WIDE } else { ADV_NARROW } * font_size;
        if used + w > avail {
            break;
        }
        used += w;
        out.push(c);
        n += 1;
    }
    (n >= 2).then_some(out)
}

impl<M> canvas::Program<M> for TreemapCanvas {
    type State = ();

    fn draw(
        &self,
        _s: &(),
        r: &Renderer,
        _t: &Theme,
        b: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if w <= 8.0 || h <= 8.0 {
                return;
            }
            if self.groups.iter().all(|g| g.tiles.is_empty()) {
                frame.fill_text(Text {
                    content: "暂无数据——启动守护并等待首轮快照".into(),
                    position: Point::new(8.0, h / 2.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let members: Vec<Vec<f64>> = self
                .groups
                .iter()
                .map(|g| g.tiles.iter().map(|t| t.weight).collect())
                .collect();

            for gl in squarify_nested(&members, Rect::new(0.0, 0.0, w, h), self.header_h) {
                let g = &self.groups[gl.group_idx];
                if self.header_h > 0.0 {
                    frame.fill_rectangle(
                        Point::new(gl.header.x, gl.header.y),
                        Size::new(gl.header.w, gl.header.h),
                        Color::from_rgb(0.13, 0.14, 0.17),
                    );
                    frame.fill_text(Text {
                        content: g.title.clone(),
                        position: Point::new(gl.header.x + 5.0, gl.header.y + 2.0),
                        color: C_HEAD,
                        size: iced::Pixels(10.0),
                        ..Default::default()
                    });
                }
                for t in gl.tiles {
                    let d = &g.tiles[t.idx];
                    let (x, y, tw, th) = (t.rect.x, t.rect.y, t.rect.w, t.rect.h);
                    frame.fill_rectangle(Point::new(x, y), Size::new(tw, th), d.color);
                    frame.stroke(
                        &Path::rectangle(Point::new(x, y), Size::new(tw, th)),
                        Stroke::default()
                            .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
                            .with_width(1.0),
                    );
                    // LOD：字号随格子缩放；放不下就只留色块，硬画会糊成噪声
                    if tw < MIN_LABEL_W || th < MIN_LABEL_H {
                        continue;
                    }
                    let avail = tw - 6.0;
                    // 字号由**标签实际宽度**定，而不只是格子尺寸：只按格子算的话，
                    // 5 个字母的 TRUMP 会被放到 24px 直接压到隔壁 AVAX 上（实测拼成 TRUMPAVAX）。
                    let fs = fit_font(&d.label, avail, th * 0.34).clamp(8.0, 26.0);
                    if let Some(lab) = fit_text(&d.label, fs, avail) {
                        frame.fill_text(Text {
                            content: lab,
                            position: Point::new(x + 3.0, y + 2.0),
                            color: Color::WHITE,
                            size: iced::Pixels(fs),
                            ..Default::default()
                        });
                    }
                    let vfs = (fs * 0.72).max(8.0);
                    if th >= fs * 2.3 {
                        if let Some(val) = fit_text(&d.value, vfs, avail) {
                            frame.fill_text(Text {
                                content: val,
                                position: Point::new(x + 3.0, y + 2.0 + fs * 1.15),
                                color: Color::from_rgba(1.0, 1.0, 1.0, 0.82),
                                size: iced::Pixels(vfs),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        });
        vec![geo]
    }
}

// ───────────────────────── 图例 ─────────────────────────

fn legend<'a>(cb: ColorBy) -> Element<'a, RadarMsg> {
    let e = edges(cb);
    let sw = |c: Color| -> Element<'a, RadarMsg> {
        container(text(" ").size(9))
            .width(Length::Fixed(26.0))
            .height(Length::Fixed(11.0))
            .style(move |_: &Theme| container::Style {
                background: Some(c.into()),
                ..Default::default()
            })
            .into()
    };
    let lbl = |s: String| -> Element<'a, RadarMsg> {
        container(text(s).size(9).color(C_DIM))
            .width(Length::Fixed(26.0))
            .into()
    };
    let mut sw_row = row![].spacing(1);
    for c in DOWN.iter().rev() {
        sw_row = sw_row.push(sw(*c));
    }
    sw_row = sw_row.push(sw(C_NEUTRAL));
    for c in UP.iter() {
        sw_row = sw_row.push(sw(*c));
    }

    // 标签压在档位分界处：4档跌 + 中性 + 4档涨 = 9 格
    let mut lb_row = row![].spacing(1);
    for x in e.iter().rev() {
        lb_row = lb_row.push(lbl(format!("-{}", edge_label(cb, *x))));
    }
    lb_row = lb_row.push(lbl("0".into()));
    for x in e.iter() {
        lb_row = lb_row.push(lbl(format!("+{}", edge_label(cb, *x))));
    }

    row![
        text("跌 ").size(9).color(C_DIM),
        column![sw_row, lb_row].spacing(1),
        text(" 涨").size(9).color(C_DIM),
    ]
    .align_y(iced::Alignment::Center)
    .into()
}

// ───────────────────────── 小部件 ─────────────────────────

fn chip<'a>(label: &str, active: bool, msg: RadarMsg) -> Element<'a, RadarMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .on_press_maybe((!active).then_some(msg))
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color) -> Element<'a, RadarMsg> {
    container(text(s).size(11).color(c)).width(Length::Fixed(w)).into()
}

/// 可点排序的列头（带 ▲▼）。
fn head_cell<'a>(col: &Col, v: ViewState) -> Element<'a, RadarMsg> {
    let active = col.key == v.sort;
    let arrow = if active {
        if v.desc { " ▼" } else { " ▲" }
    } else {
        ""
    };
    container(
        button(
            text(format!("{}{arrow}", col.title))
                .size(11)
                .color(if active { C_HEAD } else { C_TXT }),
        )
        .padding([1, 2])
        .on_press(RadarMsg::SortBy(col.key)),
    )
    .width(Length::Fixed(col.width))
    .into()
}

// ───────────────────────── 面板体 ─────────────────────────

pub fn pane_body<'a>() -> Element<'a, RadarMsg> {
    let st: RadarReadout = super::radar_readout::snapshot();
    let v = super::radar::view();
    let mut body = column![].spacing(6).padding(10);

    body = body.push(
        text("全市场雷达 · 加密热层（docs/22 · 发现工具·非交易信号）")
            .size(14)
            .color(C_HEAD),
    );

    // ── 守护控制条 ──
    let running = st.svc.active;
    body = body.push(
        row![
            text("守护 ").size(11).color(C_DIM),
            chip("▶ 启动", running, RadarMsg::Start),
            chip("■ 停止", !running, RadarMsg::Stop),
            text(format!("　{}", if running { "运行中" } else { "未运行" }))
                .size(11)
                .color(if running { UP[3] } else { C_DIM }),
            text("　").size(11),
            button(text("⟳ 刷新").size(11)).padding([2, 7]).on_press(RadarMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(10).color(C_DIM),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    );
    let am = super::radar::action_message();
    if !am.is_empty() {
        let bad = am.starts_with('✗');
        body = body.push(text(am).size(10).color(if bad { C_BAD } else { UP[3] }));
    }

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无快照——点上方「▶ 启动」拉起 ws-radar 守护（首轮约 5s 出数据）")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 状态行：数据等级 + 热身 + 回填（docs/22 §0：不标等级 = 自欺）──
    let warm = st.rows.iter().filter(|r| r.trustworthy()).count();
    let prov = st.rows.iter().filter(|r| r.z_provisional).count();
    body = body.push(
        text(format!(
            "等级 {} 交易所直连·真实时 · {} 标的 · 单轮 {}ms · z 可信 {}/{}{}",
            if st.tier.is_empty() { "?" } else { &st.tier },
            st.n_symbols,
            st.refreshed_ms,
            warm,
            st.rows.len(),
            if prov > 0 {
                format!(" · {prov} 行借横截面基线")
            } else {
                String::new()
            }
        ))
        .size(10)
        .color(if warm == 0 { C_GOLD } else { C_DIM }),
    );
    let bf = &st.backfill;
    if bf.running {
        body = body.push(
            text(format!(
                "⟳ σ 回填中 {}/{}（{:.0}%）{}",
                bf.done,
                bf.total,
                bf.pct() * 100.0,
                if bf.failed > 0 { format!("　失败 {}", bf.failed) } else { String::new() }
            ))
            .size(10)
            .color(C_HEAD),
        );
    } else if warm == 0 {
        body = body.push(
            text(if bf.finished {
                "⏳ 回填已完成但仍无可信 z——检查 K 线端点（journalctl --user -u ws-radar）"
            } else {
                "⏳ 尚未热身，z 值不可信（等待表见 docs/22 §8.1）"
            })
            .size(10)
            .color(C_GOLD),
        );
    }

    // ── 热图控制条 ──
    let mut r1 = row![text("窗口 ").size(11).color(C_DIM)].spacing(3);
    for (i, w) in WINDOWS.iter().enumerate() {
        r1 = r1.push(chip(w, i == v.win, RadarMsg::SetWindow(i)));
    }
    r1 = r1.push(text("　面积 ").size(11).color(C_DIM));
    for (s, l) in [(SizeBy::Turnover, "24h 成交额"), (SizeBy::Equal, "等权")] {
        r1 = r1.push(chip(l, s == v.size_by, RadarMsg::SetSizeBy(s)));
    }
    r1 = r1.push(text("　分组 ").size(11).color(C_DIM));
    for (g, l) in [(GroupBy::None, "无"), (GroupBy::Venue, "按 venue")] {
        r1 = r1.push(chip(l, g == v.group_by, RadarMsg::SetGroupBy(g)));
    }
    body = body.push(r1.align_y(iced::Alignment::Center));

    let mut r2 = row![text("颜色 ").size(11).color(C_DIM)].spacing(3);
    for (c, l) in [
        (ColorBy::SpeedZ, "涨跌速度 z"),
        (ColorBy::Change, "裸涨跌幅"),
        (ColorBy::VolZ, "量异常 z"),
    ] {
        r2 = r2.push(chip(l, c == v.color_by, RadarMsg::SetColorBy(c)));
    }
    r2 = r2.push(text("　　").size(11));
    r2 = r2.push(legend(v.color_by));
    body = body.push(r2.align_y(iced::Alignment::Center));

    if v.color_by == ColorBy::Change {
        body = body.push(
            text("⚠ 裸涨跌幅跨标的不可比——小市值/低流动标的会占满深色档，对照用")
                .size(10)
                .color(C_GOLD),
        );
    }
    if v.color_by == ColorBy::VolZ {
        body = body.push(
            text("ⓘ 量异常只看正尾：负值来自 24 小时前那段量掉出滚动窗口，不代表当前清淡")
                .size(10)
                .color(C_DIM),
        );
    }

    // ── 树图 ──
    let idx = order(&st.rows, v);
    // 树图选行**按体量**，不按当前排序：热图要回答「整个市场此刻什么样」，
    // 取排序前 N 会让整张图只剩涨幅榜（实测就是满屏一片蓝，看不到任何下跌）。
    // 排序只管下面的 Screener。超过 180 格就小于可辨识尺寸，只会拖慢绘制。
    let picked = top_by_turnover(&st.rows, 180);
    let mk_tile = |i: usize| {
        let r = &st.rows[i];
        let m = color_metric(r, v);
        TileData {
            label: base_asset(&r.symbol).to_string(),
            value: match v.color_by {
                ColorBy::Change => opt_pct(m),
                _ => opt_z(m),
            },
            weight: area_weight(r, v.size_by),
            color: scale_color(m, v.color_by, r.trustworthy()),
        }
    };
    let (groups, header_h) = match v.group_by {
        GroupBy::None => (
            vec![GroupData {
                title: String::new(),
                tiles: picked.iter().map(|&i| mk_tile(i)).collect(),
            }],
            0.0,
        ),
        GroupBy::Venue => {
            let mut names: Vec<String> =
                picked.iter().map(|&i| st.rows[i].venue.clone()).collect();
            names.sort();
            names.dedup();
            let gs = names
                .iter()
                .map(|n| GroupData {
                    title: n.trim_start_matches("binance:").to_string(),
                    tiles: picked
                        .iter()
                        .filter(|&&i| &st.rows[i].venue == n)
                        .map(|&i| mk_tile(i))
                        .collect(),
                })
                .collect();
            (gs, 14.0)
        }
    };
    body = body.push(
        canvas_widget(TreemapCanvas {
            groups,
            header_h,
            // 每帧新建：数据 2s 一变，Cache 不感知内容变化，复用会把画面冻住
            cache: std::rc::Rc::new(Cache::new()),
        })
        .width(Length::Fill)
        .height(Length::Fixed(340.0)),
    );

    // ── Screener ──
    let mut cs = row![text("列组 ").size(11).color(C_DIM)].spacing(3);
    for (c, l) in [
        (ColumnSet::Overview, "概览"),
        (ColumnSet::Performance, "表现"),
        (ColumnSet::Speed, "速度"),
        (ColumnSet::Volume, "量"),
    ] {
        cs = cs.push(chip(l, c == v.cols, RadarMsg::SetColumns(c)));
    }
    let shown = idx.len().min(80);
    cs = cs.push(
        text(format!("　{} / {} 条 · 点列头排序", shown, st.rows.len()))
            .size(10)
            .color(C_DIM),
    );
    body = body.push(cs.align_y(iced::Alignment::Center));

    let cols = columns(v);
    let mut hdr = row![].spacing(3);
    for c in &cols {
        hdr = hdr.push(head_cell(c, v));
    }
    body = body.push(hdr);

    for &i in idx.iter().take(80) {
        let r = &st.rows[i];
        let mut tr = row![].spacing(3);
        for c in &cols {
            let (s, col) = cell_text(r, c.key);
            tr = tr.push(cell(s, c.width, col));
        }
        let flag = if !r.sigma_ok && r.z_provisional {
            "≈"
        } else if !r.sigma_ok {
            "⏳"
        } else {
            ""
        };
        tr = tr.push(cell(flag.into(), 22.0, C_GOLD));
        body = body.push(tr);
    }

    body = body.push(
        text(format!(
            "源 {} · 快照 {}{} · ⏳=未热身 ≈=借横截面基线（均不可当结论）",
            st.source,
            st.stamp,
            super::staleness::suffix(&st.stamp),
        ))
        .size(10)
        .color(C_DIM),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(sym: &str, z5: Option<f64>) -> RadarRow {
        let mut r = RadarRow {
            symbol: sym.into(),
            venue: "binance:spot".into(),
            quote_vol_24h: 1e6,
            sigma_ok: z5.is_some(),
            ..Default::default()
        };
        r.z_ret[1] = z5;
        r
    }

    #[test]
    fn buckets_are_discrete_and_symmetric() {
        // 从 edges() 推探针，别硬编码——调过一次分档边界，硬编码的断言会假失败
        let e = edges(ColorBy::SpeedZ);
        assert_eq!(bucket(0.0, &e), 0);
        assert_eq!(bucket(e[0] - 1e-9, &e), 0);
        for (i, x) in e.iter().enumerate() {
            assert_eq!(bucket(*x, &e), i + 1, "边界值应落进上一档（>=）");
            assert_eq!(bucket(-*x, &e), i + 1, "分档按绝对值，正负对称");
        }
        assert_eq!(bucket(e[3] * 100.0, &e), 4, "超出最强档应封顶，不越界");
        assert_eq!(bucket(f64::NAN, &e), 0);
        assert_eq!(bucket(f64::INFINITY, &e), 0);
    }

    #[test]
    fn legend_and_tiles_share_one_definition() {
        // 图例画 4 档跌 + 中性 + 4 档涨；上色也必须只用这 9 个颜色。
        // 两者若各写一套，图例和格子会对不上——那比没有图例更糟。
        for cb in [ColorBy::SpeedZ, ColorBy::Change, ColorBy::VolZ] {
            let e = edges(cb);
            for probe in [-9.0, -3.0, -1.0, -0.1, 0.0, 0.1, 1.0, 3.0, 9.0] {
                let c = scale_color(Some(probe), cb, true);
                let known = std::iter::once(C_NEUTRAL)
                    .chain(UP.iter().copied())
                    .chain(DOWN.iter().copied())
                    .any(|k| (k.r - c.r).abs() < 1e-6 && (k.g - c.g).abs() < 1e-6);
                assert!(known, "{cb:?} 在 {probe} 处上了色阶之外的颜色");
            }
            assert_eq!(e.len(), 4);
        }
    }

    #[test]
    fn color_is_diverging_and_desaturates_untrusted() {
        let up = scale_color(Some(4.0), ColorBy::SpeedZ, true);
        let down = scale_color(Some(-4.0), ColorBy::SpeedZ, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(down.r > down.b, "跌端应偏橙");
        let dist = |c: Color| (c.r - C_NEUTRAL.r).abs() + (c.b - C_NEUTRAL.b).abs();
        assert!(dist(scale_color(Some(4.0), ColorBy::SpeedZ, false)) < dist(up));
    }

    #[test]
    fn missing_value_is_neutral() {
        let c = scale_color(None, ColorBy::SpeedZ, true);
        assert!((c.r - C_NEUTRAL.r).abs() < 1e-6 && (c.b - C_NEUTRAL.b).abs() < 1e-6);
    }

    #[test]
    fn base_asset_strips_quote_currency() {
        assert_eq!(base_asset("BTCUSDT"), "BTC");
        assert_eq!(base_asset("ETHBTC"), "ETH");
        assert_eq!(base_asset("1000PEPEUSDT"), "1000PEPE");
        // 只剩计价币本身时不该剥成空串
        assert_eq!(base_asset("USDT"), "USDT");
        assert_eq!(base_asset("FOO"), "FOO");
    }

    #[test]
    fn column_sets_differ_and_always_lead_with_symbol() {
        let mut v = ViewState::DEFAULT;
        for cs in [
            ColumnSet::Overview,
            ColumnSet::Performance,
            ColumnSet::Speed,
            ColumnSet::Volume,
        ] {
            v.cols = cs;
            let c = columns(v);
            assert_eq!(c[0].key, SortKey::Symbol, "{cs:?} 首列应是标的");
            assert!(c.len() >= 5, "{cs:?} 列太少");
        }
        v.cols = ColumnSet::Speed;
        let speed = columns(v);
        assert_eq!(
            speed.iter().filter(|c| matches!(c.key, SortKey::Z(_))).count(),
            WINDOWS.len(),
            "速度列组应给出全部窗口的 z"
        );
    }

    #[test]
    fn overview_columns_follow_selected_window() {
        let mut v = ViewState::DEFAULT;
        v.win = 4;
        v.cols = ColumnSet::Overview;
        let c = columns(v);
        assert!(c.iter().any(|x| x.key == SortKey::Z(4)));
        assert!(c.iter().any(|x| x.title.contains("4h")));
    }

    #[test]
    fn cell_text_marks_missing_as_dash_not_zero() {
        let r = row_of("XUSDT", None);
        assert_eq!(cell_text(&r, SortKey::Z(1)).0, "—");
        assert_eq!(cell_text(&r, SortKey::Ret(1)).0, "—");
        assert_eq!(cell_text(&r, SortKey::VolZ).0, "—");
    }

    #[test]
    fn area_weight_equal_mode_ignores_turnover() {
        let mut r = row_of("A", Some(1.0));
        r.quote_vol_24h = 9e9;
        assert!((area_weight(&r, SizeBy::Equal) - 1.0).abs() < 1e-12);
        assert!((area_weight(&r, SizeBy::Turnover) - 9e9).abs() < 1e-3);
    }

    #[test]
    fn treemap_selection_is_by_turnover_not_by_sort() {
        // 按排序选行会让热图只剩涨幅榜（实测满屏全蓝，一个下跌都看不见）。
        let mut big = row_of("BIG", Some(-0.1));
        big.quote_vol_24h = 9e9;
        let mut small = row_of("SMALL", Some(9.0));
        small.quote_vol_24h = 1.0;
        let rows = vec![small, big];
        let top = top_by_turnover(&rows, 1);
        assert_eq!(rows[top[0]].symbol, "BIG", "体量最大的必须进图，哪怕它 z 最低");
    }

    #[test]
    fn treemap_selection_is_stable_and_bounded() {
        let rows: Vec<RadarRow> = (0..50).map(|i| row_of(&format!("S{i}"), Some(1.0))).collect();
        let a = top_by_turnover(&rows, 10);
        assert_eq!(a, top_by_turnover(&rows, 10), "同额时必须定序，否则每轮刷新树图会抖");
        assert_eq!(a.len(), 10);
        assert_eq!(top_by_turnover(&rows, 999).len(), 50, "n 大于行数不该越界");
    }

    #[test]
    fn fit_text_truncates_instead_of_overflowing() {
        assert_eq!(fit_text("BTC", 10.0, 200.0).as_deref(), Some("BTC"));
        // 放不下就截断，绝不返回超长串（溢出会把 PENGU+ONDO 拼成 PENGUONDO）
        let cut = fit_text("1000PEPE", 20.0, 40.0).unwrap();
        assert!(cut.chars().count() < 8, "没截断：{cut}");
        assert!("1000PEPE".starts_with(&cut));
        // 只剩一两个字母认不出是谁，不如不画
        assert!(fit_text("ABCDEF", 20.0, 10.0).is_none());
        assert!(fit_text("ABC", 10.0, 0.0).is_none());
    }

    #[test]
    fn fit_text_never_exceeds_available_width() {
        for s in ["BTC", "1000PEPE", "我踏马来了", "BROCCOLI714", "混合ABC"] {
            for avail in [12.0f32, 30.0, 55.0, 120.0] {
                if let Some(out) = fit_text(s, 14.0, avail) {
                    assert!(
                        text_units(&out) * 14.0 <= avail + 1e-3,
                        "{s:?} 裁成 {out:?} 仍超宽（avail={avail}）"
                    );
                }
            }
        }
    }

    #[test]
    fn wide_chars_count_as_full_width() {
        // Binance 有中文名标的（「我踏马来了」）。按半角估宽会让标签压到隔壁格子上。
        assert!(text_units("我踏马来了") > text_units("ABCDE") * 1.5);
        assert!((text_units("我A") - (ADV_WIDE + ADV_NARROW)).abs() < 1e-6);
    }

    #[test]
    fn fit_font_shrinks_to_make_label_fit() {
        // 只按格子高算字号会让 TRUMP 压到隔壁 AVAX 上（实测拼成 TRUMPAVAX）
        let avail = 40.0;
        let fs = fit_font("TRUMP", avail, 24.0);
        assert!(fs < 24.0, "长标签必须缩字号，得到 {fs}");
        assert!(text_units("TRUMP") * fs <= avail + 1e-3);
        // 短标签不该被无谓缩小
        assert!((fit_font("A", 200.0, 24.0) - 24.0).abs() < 1e-6);
    }

    #[test]
    fn weakest_bucket_still_reads_as_up_or_down() {
        // 最弱档若跟中性灰同色，市场常态波动在图上就完全看不出方向（第一版的实测问题）。
        let sep = |a: Color, b: Color| {
            (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs()
        };
        let up1 = scale_color(Some(0.4), ColorBy::SpeedZ, true);
        let dn1 = scale_color(Some(-0.4), ColorBy::SpeedZ, true);
        let neu = scale_color(Some(0.0), ColorBy::SpeedZ, true);
        assert!(sep(up1, neu) > 0.15, "最弱涨档与中性太接近：{:.3}", sep(up1, neu));
        assert!(sep(dn1, neu) > 0.15, "最弱跌档与中性太接近：{:.3}", sep(dn1, neu));
        assert!(up1.b > up1.r && dn1.r > dn1.b, "最弱档必须仍带方向色相");
    }

    #[test]
    fn speed_edges_do_not_swallow_the_market_in_neutral() {
        // 实测 5m z 的 median≈0.13、p90≈1.5。首档边界若取 0.5，
        // 半个市场会被压进中性档，图上就没有信息了。
        let e = edges(ColorBy::SpeedZ);
        assert!(e[0] <= 0.35, "首档边界 {} 太高", e[0]);
        assert_eq!(bucket(0.5, &e), 1, "常态波动应落进有色档");
        assert_eq!(bucket(1.5, &e), 2);
    }

    #[test]
    fn table_text_is_brighter_than_tile_fill() {
        let lum = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for probe in [0.6, 1.8, -0.6, -3.9] {
            let fill = scale_color(Some(probe), ColorBy::SpeedZ, true);
            let txt = scale_text(Some(probe), ColorBy::SpeedZ, true);
            assert!(
                lum(txt) > lum(fill),
                "z={probe} 文字色不比填充色亮：{:.3} vs {:.3}",
                lum(txt),
                lum(fill)
            );
        }
    }

    #[test]
    fn text_scale_keeps_direction_and_neutral() {
        let up = scale_text(Some(3.0), ColorBy::SpeedZ, true);
        let dn = scale_text(Some(-3.0), ColorBy::SpeedZ, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(dn.r > dn.b, "跌端应偏橙");
        assert!((scale_text(None, ColorBy::SpeedZ, true).r - C_DIM.r).abs() < 1e-6);
    }

    #[test]
    fn area_weight_never_negative() {
        let mut r = row_of("A", Some(1.0));
        r.quote_vol_24h = -5.0; // 脏数据
        assert!(area_weight(&r, SizeBy::Turnover) >= 0.0, "负权重会让树图布局出负尺寸");
    }

    #[test]
    fn color_metric_follows_color_by_selector() {
        let mut r = row_of("A", Some(2.0));
        r.ret[1] = Some(0.03);
        r.z_vol = Some(5.0);
        let mut v = ViewState::DEFAULT;
        v.color_by = ColorBy::SpeedZ;
        assert_eq!(color_metric(&r, v), Some(2.0));
        v.color_by = ColorBy::Change;
        assert_eq!(color_metric(&r, v), Some(0.03));
        v.color_by = ColorBy::VolZ;
        assert_eq!(color_metric(&r, v), Some(5.0));
    }
}
