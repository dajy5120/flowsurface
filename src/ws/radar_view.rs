//! 全市场雷达 pane 的视图渲染（docs/22 §2 ① / §6）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板；数据走
//! [`super::radar_readout`] 旁路快照。
//!
//! 布局：守护控制条 → 数据等级/热身状态横幅 → 树图（面积=24h 成交额，颜色=涨跌速度 z）
//! → 排行表。
//!
//! 配色用**蓝(涨)–橙(跌)发散**而非红绿：树图上有几百个小格，红绿对色盲不可分辨
//! （docs/22 §6）。表格沿用同一对颜色，面板内自洽，并给出图例。

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{button, canvas as canvas_widget, column, container, row, scrollable, text};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use super::radar::{RadarMsg, SortBy};
use super::radar_readout::{RadarReadout, RadarRow, WINDOWS};
use super::treemap::{squarify, Rect};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);

/// 发散色标两端与中性色。蓝=涨、橙=跌（色盲安全，docs/22 §6）。
const C_UP: Color = Color::from_rgb(0.25, 0.62, 0.95);
const C_DOWN: Color = Color::from_rgb(0.93, 0.55, 0.18);
const C_NEUTRAL: Color = Color::from_rgb(0.28, 0.30, 0.34);

/// z → 颜色。`|z| ≥ SATURATE` 到满色；`trusted=false` 的格子向中性去饱和，
/// 让「还没热身/借了横截面基线」在视觉上就弱于可信读数。
const SATURATE: f64 = 3.0;

/// 24h 列没有 z（那需要 30 天非重叠样本），退而用固定标度上色：
/// ±5% 映射到满色。纯显示用，不参与排序，也不冒充 z。
const H24_FULL_SCALE: f64 = 0.05;

fn lerp(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgb(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

pub(crate) fn z_color(z: Option<f64>, trusted: bool) -> Color {
    let c = match z {
        Some(z) if z.is_finite() => {
            let t = (z.abs() / SATURATE).min(1.0) as f32;
            lerp(C_NEUTRAL, if z >= 0.0 { C_UP } else { C_DOWN }, t)
        }
        _ => C_NEUTRAL,
    };
    if trusted {
        c
    } else {
        lerp(c, C_NEUTRAL, 0.6)
    }
}

fn usd(v: f64) -> String {
    if v >= 1e9 {
        format!("${:.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("${:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("${:.0}k", v / 1e3)
    } else {
        format!("${v:.0}")
    }
}

fn opt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:+.2}%", x * 100.0)).unwrap_or_else(|| "—".into())
}

fn opt_z(v: Option<f64>) -> String {
    v.map(|x| format!("{x:+.2}")).unwrap_or_else(|| "—".into())
}

fn cell<'a, M: 'a>(s: String, w: f32, c: Color) -> Element<'a, M> {
    container(text(s).size(11).color(c)).width(Length::Fixed(w)).into()
}

/// 按当前口径排序（返回下标，避免克隆整表）。
pub(crate) fn order(rows: &[RadarRow], sort: SortBy, win: usize) -> Vec<usize> {
    let key = |r: &RadarRow| -> f64 {
        match sort {
            SortBy::Speed => r.z_ret[win].map(f64::abs).unwrap_or(f64::MIN),
            SortBy::Change => r.ret[win].map(f64::abs).unwrap_or(f64::MIN),
            // 量异常按**带符号值**降序，不取绝对值：z_vol 来自滚动 24h 计数器差分，
            // 负尾表示的是「24 小时前那 5 分钟量大」掉出窗口，**不是**「当前量低」
            // （P0 实测 ALICEUSDT z_vol=−31.89 而 5m 涨跌为 0）。只有正尾可解读。
            SortBy::Volume => r.z_vol.unwrap_or(f64::MIN),
        }
    };
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| {
        key(&rows[b])
            .partial_cmp(&key(&rows[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
            // 同分按标的定序，否则每次刷新表格顺序会抖
            .then_with(|| rows[a].symbol.cmp(&rows[b].symbol))
    });
    idx
}

// ───────────────────────────── 树图画布 ─────────────────────────────

struct TreemapCanvas {
    tiles: Vec<(String, f64, Option<f64>, bool)>, // (symbol, weight, z, trusted)
    cache: std::rc::Rc<Cache>,
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
            if self.tiles.is_empty() {
                frame.fill_text(Text {
                    content: "暂无数据——启动守护并等待热身".into(),
                    position: Point::new(8.0, h / 2.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let weights: Vec<f64> = self.tiles.iter().map(|t| t.1).collect();
            for t in squarify(&weights, Rect::new(0.0, 0.0, w, h)) {
                let (sym, _, z, trusted) = &self.tiles[t.idx];
                let (x, y, tw, th) = (t.rect.x, t.rect.y, t.rect.w, t.rect.h);
                frame.fill_rectangle(Point::new(x, y), Size::new(tw, th), z_color(*z, *trusted));
                frame.stroke(
                    &Path::rectangle(Point::new(x, y), Size::new(tw, th)),
                    Stroke::default()
                        .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.35))
                        .with_width(1.0),
                );
                // LOD：格子太小就只留色块，硬画标签会糊成噪声（docs/22 §6）
                if tw >= 46.0 && th >= 26.0 {
                    frame.fill_text(Text {
                        content: sym.clone(),
                        position: Point::new(x + 4.0, y + 3.0),
                        color: C_TXT,
                        size: iced::Pixels(10.0),
                        ..Default::default()
                    });
                    frame.fill_text(Text {
                        content: opt_z(*z),
                        position: Point::new(x + 4.0, y + 14.0),
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.75),
                        size: iced::Pixels(9.0),
                        ..Default::default()
                    });
                }
            }
        });
        vec![geo]
    }
}

// ───────────────────────────── 面板体 ─────────────────────────────

pub fn pane_body<'a>() -> Element<'a, RadarMsg> {
    let st: RadarReadout = super::radar_readout::snapshot();
    let sort = super::radar::sort_by();
    let win = super::radar::window_idx();
    let mut body = column![].spacing(8).padding(10);

    body = body.push(
        text("全市场雷达 · 加密热层（docs/22 P0 · 发现工具·非交易信号）")
            .size(14)
            .color(C_HEAD),
    );

    // ── 守护控制条 ──
    let running = st.svc.active;
    body = body.push(
        row![
            text("守护 ").size(11).color(C_DIM),
            button(text("▶ 启动").size(11))
                .padding([2, 8])
                .on_press_maybe((!running).then_some(RadarMsg::Start)),
            button(text("■ 停止").size(11))
                .padding([2, 8])
                .on_press_maybe(running.then_some(RadarMsg::Stop)),
            text(format!(
                "　{}",
                if running { "运行中" } else { "未运行" }
            ))
            .size(11)
            .color(if running { C_UP } else { C_DIM }),
            text("　").size(11),
            button(text("⟳ 刷新").size(11)).padding([2, 8]).on_press(RadarMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(10).color(C_DIM),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    );
    let am = super::radar::action_message();
    if !am.is_empty() {
        let bad = am.starts_with('✗');
        body = body.push(text(am).size(10).color(if bad { C_BAD } else { C_UP }));
    }

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无快照——点上方「▶ 启动」拉起 ws-radar 守护（首轮约 5s 出数据）")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 窗口 / 排序口径 ──
    let mut wrow = row![text("窗口 ").size(11).color(C_DIM)].spacing(4);
    for (i, w) in WINDOWS.iter().enumerate() {
        wrow = wrow.push(
            button(text(*w).size(11))
                .padding([2, 6])
                .on_press_maybe((i != win).then_some(RadarMsg::SetWindow(i))),
        );
    }
    wrow = wrow.push(text("　排序 ").size(11).color(C_DIM));
    for (s, label) in [
        (SortBy::Speed, "涨跌速度 z"),
        (SortBy::Change, "裸涨跌幅"),
        (SortBy::Volume, "量异常 z↑"),
    ] {
        wrow = wrow.push(
            button(text(label).size(11))
                .padding([2, 6])
                .on_press_maybe((s != sort).then_some(RadarMsg::SetSort(s))),
        );
    }
    body = body.push(wrow.align_y(iced::Alignment::Center));

    if sort == SortBy::Volume {
        body = body.push(
            text("ⓘ 量异常只看正尾：负值来自 24 小时前那段量掉出滚动窗口，不代表当前清淡")
                .size(10)
                .color(C_DIM),
        );
    }
    if sort == SortBy::Change {
        body = body.push(
            text("⚠ 裸涨跌幅口径：榜首会被小市值/低流动标的占满，跨标的不可比——对照用，别据此决策")
                .size(10)
                .color(C_GOLD),
        );
    }

    // ── 数据等级 + 热身状态（docs/22 §0：不标等级 = 自欺）──
    let warm = st.rows.iter().filter(|r| r.trustworthy()).count();
    let prov = st.rows.iter().filter(|r| r.z_provisional).count();
    body = body.push(
        text(format!(
            "数据等级 {} (交易所直连·真实时) · {} 标的 · 单轮 {}ms · z 可信 {}/{}{}",
            if st.tier.is_empty() { "?" } else { &st.tier },
            st.n_symbols,
            st.refreshed_ms,
            warm,
            st.rows.len(),
            if prov > 0 {
                format!(" · {prov} 行借横截面基线(暂且一看)")
            } else {
                String::new()
            }
        ))
        .size(10)
        .color(if warm == 0 { C_GOLD } else { C_DIM }),
    );
    // 回填进度：没有它的话，用户只能对着一片 ⏳ 猜是卡住了还是在干活（docs/22 P0c）
    let bf = &st.backfill;
    if bf.running {
        body = body.push(
            text(format!(
                "⟳ σ 回填中 {}/{}（{:.0}%）{}　—— 完成后各窗口 z 立即可用，不必等热身",
                bf.done,
                bf.total,
                bf.pct() * 100.0,
                if bf.failed > 0 {
                    format!("　失败 {}", bf.failed)
                } else {
                    String::new()
                }
            ))
            .size(10)
            .color(C_HEAD),
        );
    } else if warm == 0 {
        body = body.push(
            text(if bf.finished {
                "⏳ 回填已完成但仍无可信 z——检查 K 线端点是否可达（journalctl --user -u ws-radar）"
            } else {
                "⏳ 全部窗口尚未热身完成——z 值不可信，先别当结论（等待表见 docs/22 §8.1）"
            })
            .size(10)
            .color(C_GOLD),
        );
    }

    // ── 树图 ──
    let idx = order(&st.rows, sort, win);
    let tiles: Vec<(String, f64, Option<f64>, bool)> = idx
        .iter()
        .take(160) // 再多格子就小于可辨识尺寸了，只会拖慢绘制
        .map(|&i| {
            let r = &st.rows[i];
            (
                r.symbol.clone(),
                r.quote_vol_24h,
                r.z_ret[win],
                r.trustworthy(),
            )
        })
        .collect();
    body = body.push(
        text(format!(
            "树图：面积=24h 成交额，颜色=「{}」窗涨跌速度 z　■蓝=涨 ■橙=跌 ■灰=未热身/无信号",
            WINDOWS[win]
        ))
        .size(10)
        .color(C_DIM),
    );
    body = body.push(
        canvas_widget(TreemapCanvas {
            tiles,
            // 每帧新建：数据 2s 一变，Cache 不感知内容变化，复用会把画面冻住
            // （同 tardis_board_view 回放态的处理）
            cache: std::rc::Rc::new(Cache::new()),
        })
        .width(Length::Fill)
        .height(Length::Fixed(320.0)),
    );

    // ── 排行表 ──
    body = body.push(
        row![
            cell("标的".into(), 120.0, C_HEAD),
            cell("venue".into(), 70.0, C_HEAD),
            cell(format!("{} 涨跌", WINDOWS[win]), 78.0, C_HEAD),
            cell("速度 z".into(), 62.0, C_HEAD),
            cell("量异常 z".into(), 70.0, C_HEAD),
            cell("24h".into(), 70.0, C_HEAD),
            cell("24h 额".into(), 72.0, C_HEAD),
            cell("".into(), 40.0, C_HEAD),
        ]
        .spacing(4),
    );
    for &i in idx.iter().take(60) {
        let r = &st.rows[i];
        let trusted = r.trustworthy();
        let zc = z_color(r.z_ret[win], trusted);
        let flag = if !r.sigma_ok && r.z_provisional {
            "≈" // 借了横截面基线
        } else if !r.sigma_ok {
            "⏳" // 还没热身
        } else {
            ""
        };
        body = body.push(
            row![
                cell(r.symbol.clone(), 120.0, if trusted { C_TXT } else { C_DIM }),
                cell(
                    r.venue.trim_start_matches("binance:").to_string(),
                    70.0,
                    C_DIM
                ),
                cell(opt_pct(r.ret[win]), 78.0, zc),
                cell(opt_z(r.z_ret[win]), 62.0, zc),
                cell(opt_z(r.z_vol), 70.0, if trusted { C_TXT } else { C_DIM }),
                cell(
                    opt_pct(r.ret[5]),
                    70.0,
                    z_color(r.ret[5].map(|v| v / H24_FULL_SCALE * SATURATE), true),
                ),
                cell(usd(r.quote_vol_24h), 72.0, C_DIM),
                cell(flag.into(), 40.0, C_GOLD),
            ]
            .spacing(4),
        );
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

    fn row(sym: &str, z5: Option<f64>, ret5: Option<f64>, zvol: Option<f64>) -> RadarRow {
        let mut r = RadarRow {
            symbol: sym.into(),
            sigma_ok: z5.is_some(),
            ..Default::default()
        };
        r.z_ret[1] = z5;
        r.ret[1] = ret5;
        r.z_vol = zvol;
        r
    }

    #[test]
    fn speed_sort_ranks_by_absolute_z() {
        let rows = vec![
            row("A", Some(1.0), Some(0.10), None),
            row("B", Some(-4.0), Some(0.001), None),
            row("C", Some(2.0), Some(0.05), None),
        ];
        let o = order(&rows, SortBy::Speed, 1);
        assert_eq!(
            o.iter().map(|&i| rows[i].symbol.as_str()).collect::<Vec<_>>(),
            vec!["B", "C", "A"],
            "速度口径应按 |z| 排，负号不该把 B 沉下去"
        );
    }

    #[test]
    fn change_sort_differs_from_speed_sort() {
        // 这正是默认口径不用裸涨跌幅的理由：同一批数据两种排法结果完全不同
        let rows = vec![
            row("A", Some(1.0), Some(0.10), None),
            row("B", Some(-4.0), Some(0.001), None),
        ];
        assert_eq!(order(&rows, SortBy::Speed, 1)[0], 1);
        assert_eq!(order(&rows, SortBy::Change, 1)[0], 0);
    }

    #[test]
    fn volume_sort_uses_signed_value_not_abs() {
        // 大负 z_vol 是 24h 前的残影，不该被顶到榜首
        let rows = vec![
            row("A", None, None, Some(-30.0)),
            row("B", None, None, Some(4.0)),
        ];
        let o = order(&rows, SortBy::Volume, 1);
        assert_eq!(rows[o[0]].symbol, "B", "只有正尾可解读，负尾不该排前");
    }

    #[test]
    fn rows_without_data_sink_to_bottom() {
        let rows = vec![
            row("A", None, None, None),
            row("B", Some(0.1), Some(0.0), None),
        ];
        assert_eq!(order(&rows, SortBy::Speed, 1)[0], 1, "有 z 的应排前");
    }

    #[test]
    fn ordering_is_stable_on_ties() {
        let rows = vec![
            row("ZZZ", Some(1.0), None, None),
            row("AAA", Some(1.0), None, None),
        ];
        let o = order(&rows, SortBy::Speed, 1);
        assert_eq!(rows[o[0]].symbol, "AAA", "同分应按标的名定序，避免刷新抖动");
    }

    #[test]
    fn color_is_diverging_and_desaturates_untrusted() {
        let up = z_color(Some(3.0), true);
        let down = z_color(Some(-3.0), true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(down.r > down.b, "跌端应偏橙");

        // 不可信的读数必须在视觉上更弱（更靠近中性）
        let dist = |c: Color| {
            (c.r - C_NEUTRAL.r).abs() + (c.g - C_NEUTRAL.g).abs() + (c.b - C_NEUTRAL.b).abs()
        };
        assert!(dist(z_color(Some(3.0), false)) < dist(up));
    }

    #[test]
    fn color_of_missing_z_is_neutral() {
        let c = z_color(None, true);
        assert!((c.r - C_NEUTRAL.r).abs() < 1e-6 && (c.b - C_NEUTRAL.b).abs() < 1e-6);
    }
}
