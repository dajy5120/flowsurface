//! 预测市场回放 — 视图（「Tardis 历史回放」工作区的独立视图）。
//!
//! 选 日期 → 轮次 → 加载 → 播放。图与播放头裁剪复用 [`super::tardis_board_view`]
//! 的 `ChartCanvas`，所以这里只管选择器与播放条。

use iced::widget::canvas::Cache;
use iced::widget::{button, canvas as canvas_widget, column, container, row, scrollable, slider, text};
use iced::{Alignment, Color, Element, Length};

use super::pm_replay::{PmReplayMsg, PmReplayState, Speed};
use super::pm_replay_readout as ro;
use super::tardis_board_view::{ChartCanvas, cache_for, clip_to};

const C_HEAD: Color = Color::from_rgb(0.85, 0.72, 0.95);
const C_DIM: Color = Color::from_rgb(0.50, 0.54, 0.60);
const C_TXT: Color = Color::from_rgb(0.84, 0.87, 0.92);
const C_UP: Color = Color::from_rgb(0.30, 0.80, 0.48);
const C_DOWN: Color = Color::from_rgb(0.90, 0.38, 0.38);
const C_WARN: Color = Color::from_rgb(0.88, 0.70, 0.34);

fn chip<'a>(label: String, active: bool, c: Color, msg: PmReplayMsg) -> Element<'a, PmReplayMsg> {
    button(text(label).size(12).color(if active { Color::from_rgb(0.98, 0.99, 1.0) } else { c }))
        .padding([3, 8])
        .style(move |t, s| crate::style::button::modifier(t, s, active))
        .on_press(msg)
        .into()
}

/// 轮次结算态 → 颜色与字。**空不是平局，是没判出来**，所以给第三种显示，
/// 不能让它看起来像一个结果。
fn verdict(resolved: &str) -> (&'static str, Color) {
    match resolved {
        "UP" => ("涨", C_UP),
        "DOWN" => ("跌", C_DOWN),
        _ => ("未判", C_DIM),
    }
}

fn hms(ms: f64) -> String {
    let s = (ms / 1000.0) as i64;
    format!("{:02}:{:02}:{:02}", s / 3600 % 24, s / 60 % 60, s % 60)
}

pub fn pane_body(app: &PmReplayState) -> Element<'_, PmReplayMsg> {
    // 每帧轮询后台任务：顺带收割已结束的子进程并作废缓存。
    let busy = super::pm_replay::poll();
    let cat = ro::catalog();
    let p = ro::panel();
    let span = ro::time_span(&p);
    let playing = super::pm_replay::is_playing();
    // 播放中跟播放头；静止时若拖过进度条则停在那里，否则显示整窗。
    let head = match (playing, span) {
        (true, Some((_, t1))) => super::pm_replay::head(t1),
        (false, Some((t0, t1))) => app.seek_pct.map(|s| t0 + (t1 - t0) * s as f64),
        _ => None,
    };

    let mut body = column![
        text("预测市场回放 · 币安钱包 BTC 5 分钟涨跌（自录数据，零交易所连接）")
            .size(15)
            .color(C_HEAD),
        text(
            "回放单位是**一轮**：5 分钟市场每轮换一个市场与一套盘口，跨轮把曲线接起来\
             只会画出一次假的暴跌。"
        )
        .size(10)
        .color(C_DIM),
    ]
    .spacing(6)
    .padding(10);

    // ── 选择器 ──
    if let Some(e) = &cat.error {
        body = body.push(text(e.clone()).size(11).color(C_WARN));
    }
    let mut syms = row![text("符号").size(12).color(C_DIM)].spacing(6).align_y(Alignment::Center);
    for s in &cat.symbols {
        syms = syms.push(chip(
            s.symbol.clone(),
            s.symbol == app.symbol,
            C_TXT,
            PmReplayMsg::PickSymbol(s.symbol.clone()),
        ));
    }
    syms = syms.push(chip("⟳ 刷新清单".into(), false, C_DIM, PmReplayMsg::RefreshCatalog));
    body = body.push(syms);

    if let Some(sym) = cat.symbol(&app.symbol) {
        let mut dates = row![text("日期").size(12).color(C_DIM)].spacing(6).align_y(Alignment::Center);
        for d in sym.dates.iter().take(10) {
            let n = d.rounds.len();
            dates = dates.push(chip(
                format!("{} ({n})", d.date),
                d.date == app.date,
                C_TXT,
                PmReplayMsg::PickDate(d.date.clone()),
            ));
        }
        body = body.push(dates);
    }

    // ── 加载 + 播放 ──
    // 点轮次已自动加载，这颗按钮是给「录了更多数据后重取当前这一轮」用的。
    let mut ctl = row![chip("↻ 重新加载".into(), false, C_DIM, PmReplayMsg::Load)]
        .spacing(6)
        .align_y(Alignment::Center);
    ctl = ctl.push(if playing {
        chip("⏸ 暂停".into(), true, C_TXT, PmReplayMsg::Pause)
    } else {
        chip("▶ 回放".into(), false, C_TXT, PmReplayMsg::Play)
    });
    ctl = ctl.push(chip("⏮ 回到开头".into(), false, C_DIM, PmReplayMsg::Rewind));
    for s in Speed::ALL {
        ctl = ctl.push(chip(s.to_string(), s == app.speed, C_TXT, PmReplayMsg::PickSpeed(s)));
    }
    if let Some(d) = &busy {
        ctl = ctl.push(text(format!("  正在生成{d}…")).size(11).color(C_WARN));
    }
    body = body.push(ctl);

    // 进度条：位置是「数据时间」，不是墙钟——倍速变了它也仍然对得上图。
    if let Some((t0, t1)) = span {
        let pct = head.map(|h| ((h - t0) / (t1 - t0) * 100.0) as f32).unwrap_or(0.0);
        body = body.push(
            row![
                slider(0.0..=100.0, pct.clamp(0.0, 100.0), |v| PmReplayMsg::Seek(v / 100.0))
                    .width(Length::FillPortion(4)),
                text(format!(
                    "{} / {}",
                    hms(head.unwrap_or(t0) - t0),
                    hms(t1 - t0)
                ))
                .size(11)
                .color(C_TXT),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }

    let rounds = app.rounds();
    if rounds.is_empty() {
        body = body.push(
            text("这一天没有轮次记录。录制器跑过吗？（「进程」页可启停，清单要点「刷新清单」重扫）")
                .size(11)
                .color(C_WARN),
        );
    } else {
        // **换行排，不用横向滚动条。**
        //
        // 第一版是 `scrollable(row).direction(Horizontal)`，套在整页的竖向
        // scrollable 里 → 高度被压成一条缝，还被下面的控制行盖住。嵌套两层方向
        // 相反的滚动本来就难摆，而这里根本不需要：换行之后由外层竖向滚动统一管。
        let n_all = app.all_rounds().len();
        let per_row = 6; // 一行 6 个：标签形如「8:55AM-9AM ET 跌」，再多就换行了
        let mut grid = column![].spacing(4);
        let mut line = row![].spacing(4).align_y(Alignment::Center);
        for (i, r) in rounds.iter().enumerate() {
            let (v, c) = verdict(&r.resolved);
            line = line.push(chip(
                format!("{} {}", r.label, v),
                r.market_id == app.market_id,
                c,
                PmReplayMsg::PickRound(r.market_id.clone()),
            ));
            if (i + 1) % per_row == 0 {
                grid = grid.push(line);
                line = row![].spacing(4).align_y(Alignment::Center);
            }
        }
        grid = grid.push(line);

        let filtered = n_all - rounds.len();
        body = body.push(
            column![
                row![
                    text(format!("轮次 {} / {n_all}　绿=涨 红=跌 灰=未判", rounds.len()))
                        .size(12)
                        .color(C_DIM),
                    chip("◀ 上一轮".into(), false, C_TXT, PmReplayMsg::Step(-1)),
                    chip("下一轮 ▶".into(), false, C_TXT, PmReplayMsg::Step(1)),
                    chip(
                        if app.only_resolved { "只看已结算 ✓".into() } else { "只看已结算".to_string() },
                        app.only_resolved,
                        C_TXT,
                        PmReplayMsg::ToggleOnlyResolved(!app.only_resolved),
                    ),
                    text(if filtered > 0 {
                        format!("（隐藏 {filtered} 个未判出的）")
                    } else {
                        String::new()
                    })
                    .size(10)
                    .color(C_DIM),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                grid,
            ]
            .spacing(4),
        );
    }

    let msg = super::pm_replay::message();
    let hint = if app.hint.is_empty() { msg } else { app.hint.clone() };
    if !hint.is_empty() {
        body = body.push(text(hint.clone()).size(11).color(if hint.starts_with('✗') {
            C_DOWN
        } else {
            C_DIM
        }));
    }

    // ── 图 ──
    if let Some(e) = &p.error {
        body = body.push(text(e.clone()).size(12).color(C_DOWN));
    } else if !p.loaded {
        body = body.push(
            text("在下面点一个轮次即可加载。").size(12).color(C_DIM),
        );
    } else {
        // 已加载的是不是当前选的那一轮——不是就说出来，别让人以为在看当前选择。
        let stale = p.market_id != app.market_id || p.date != app.date;
        let (v, vc) = verdict(&p.resolved);
        body = body.push(
            row![
                text(format!("{} · {}", p.date, p.question)).size(12).color(C_TXT),
                text(format!("  结算 {v}")).size(13).color(vc),
                text(format!(
                    "  {} 条簿更新 → {} 个两本齐全的采样点",
                    p.rows, p.samples
                ))
                .size(11)
                .color(C_DIM),
            ]
            .spacing(2),
        );
        if p.rows > p.samples {
            // 差值 = 只有一本簿到过的时刻，被丢掉了。半份簿算出的失衡度会把另一侧当成 0。
            body = body.push(
                text(format!(
                    "丢掉 {} 条「只有一本簿」的更新——半份簿算不出失衡度（会把另一侧当成 0）",
                    p.rows - p.samples
                ))
                .size(10)
                .color(C_DIM),
            );
        }
        if stale {
            body = body.push(
                text("⚠ 图上是上次加载的那一轮，点「↻ 重新加载」换成当前选择").size(11).color(C_WARN),
            );
        }
        // ── 那一刻的盘口 ──
        //
        // 渲染走实时面板那套函数（`pm_binance_view::{imbalance_block, ladder_block}`），
        // 不另写一份：回放看到的盘口要和当时实时看到的一模一样，否则「回放」名不副实。
        // 两边各写一份的话，改了一边忘了另一边，差异会悄悄存在很久。
        let at = head.unwrap_or(p.end_ms);
        match p.frame_at(at) {
            None if p.frames.is_empty() => {
                body = body.push(
                    text("（这份面板没有逐帧盘口——用旧版 pm_replay.py 生成的，重新「加载」一次）")
                        .size(11)
                        .color(C_WARN),
                );
            }
            None => {
                // 播放头在第一帧之前：还没有任何一刻两本簿都到齐。
                body = body.push(text("两本簿还没凑齐").size(11).color(C_DIM));
            }
            Some(f) => {
                let lag = (at - f.ts) / 1000.0;
                body = body.push(
                    row![
                        text("盘口 @ ").size(12).color(C_DIM),
                        text(hms(f.ts - p.start_ms)).size(13).color(C_TXT),
                        // 帧是降采样过的，播放头落在两帧之间很正常。说出滞后多少，
                        // 好过让人以为看到的是精确那一刻。
                        text(format!("  (帧滞后 {lag:.2}s)")).size(10).color(C_DIM),
                        text(format!(
                            "　费率 {:.2}%　{} 人　量 {}　流动性 {}",
                            p.round.fee_bps / 100.0,
                            p.round.participants,
                            super::pm_binance_view::usd(p.round.volume),
                            super::pm_binance_view::usd(p.round.liquidity),
                        ))
                        .size(10)
                        .color(C_DIM),
                    ]
                    .align_y(Alignment::Center),
                );
                body = body.push(super::pm_binance_view::imbalance_block(&f.book));
                body = body.push(
                    row![
                        container(super::pm_binance_view::ladder_block(
                            "押涨 Up",
                            "买这本 = 押 BTC 涨",
                            &f.book.up,
                            C_UP,
                        ))
                        .width(Length::FillPortion(1)),
                        container(super::pm_binance_view::ladder_block(
                            "押跌 Down",
                            "独立的一本簿，不是 Up 的另一侧",
                            &f.book.down,
                            C_DOWN,
                        ))
                        .width(Length::FillPortion(1)),
                    ]
                    .spacing(14),
                );
            }
        }

        for ch in &p.charts {
            let cv: Element<'_, PmReplayMsg> = canvas_widget(ChartCanvas {
                ch: ch.clone(),
                // 回放中几何逐帧变化，而 Cache 不感知内容变化——复用会让图冻住。
                cache: match head {
                    Some(_) => std::rc::Rc::new(Cache::new()),
                    None => cache_for(p.generation, &ch.id),
                },
                playhead: head,
            })
            .width(Length::Fill)
            .height(Length::Fixed(200.0))
            .into();
            let mut col =
                column![text(ch.title.clone()).size(13).color(Color::from_rgb(0.85, 0.88, 0.92)), cv]
                    .spacing(3);
            if !ch.note.is_empty() {
                col = col.push(text(ch.note.clone()).size(10).color(C_DIM));
            }
            body = body.push(container(col).padding(4));
        }
        let _ = clip_to; // 裁剪发生在 ChartCanvas 内部；这里只是把播放头传进去
    }

    container(scrollable(body)).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 未判出的轮次与涨跌区分显示() {
        assert_eq!(verdict("UP").0, "涨");
        assert_eq!(verdict("DOWN").0, "跌");
        // 空串不能显示成任何一个方向——它是「没有结论」。
        assert_eq!(verdict("").0, "未判");
        assert_ne!(verdict("").1, verdict("UP").1);
        assert_ne!(verdict("").1, verdict("DOWN").1);
    }

    #[test]
    fn 进度显示的是数据时间偏移() {
        assert_eq!(hms(0.0), "00:00:00");
        assert_eq!(hms(300_000.0), "00:05:00"); // 一轮正好 5 分钟
        assert_eq!(hms(65_000.0), "00:01:05");
    }
}
